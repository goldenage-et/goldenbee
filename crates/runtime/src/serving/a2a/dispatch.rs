//! JSON-RPC dispatch and the thin durable-send/cancel wrapper.
//!
//! Native `V1Executor` and `TaskStore` own task state and transitions. This
//! module only adds what the pinned `RequestHandler` cannot inject: a durable
//! message-id idempotency record and a per-task cancellation token.

use super::A2aState;
use crate::core::crypto::DelegationClaims;
use a2a_protocol_types::events::StreamResponse;
use a2a_protocol_types::jsonrpc::{
    JsonRpcError, JsonRpcErrorResponse, JsonRpcId, JsonRpcRequest, JsonRpcRequestId,
    JsonRpcSuccessResponse, JsonRpcVersion,
};
use a2a_protocol_types::params::{
    CancelTaskParams, MessageSendParams, TaskIdParams, TaskQueryParams,
};
use a2a_protocol_types::{Artifact, ArtifactId, Message, Task, TaskState, TaskStatus};
use adk_rust::runner::Runner;
use adk_rust::server::a2a::v1::convert::{internal_task_to_wire, wire_part_to_adk};
use adk_rust::server::a2a::v1::error::A2aError;
use adk_rust::server::a2a::v1::task_store::{
    ListTasksParams, TaskStore, TaskStoreEntry, OWNER_METADATA_KEY,
};
use adk_rust::session::{CreateRequest, GetRequest};
use adk_rust::{Agent, Content, Part, SessionId, UserId};
use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use common::Principal;
use futures::StreamExt;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// JSON-RPC and SSE entry point for `/a2a`.
pub(super) async fn jsonrpc(
    State(state): State<Arc<A2aState>>,
    caller: Option<Extension<Principal>>,
    claims: Option<Extension<DelegationClaims>>,
    body: Bytes,
) -> Response {
    let request: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return rpc_error(
                None,
                &A2aError::InvalidParams {
                    message: format!("invalid JSON-RPC request: {error}"),
                },
            )
        }
    };
    let Some(Extension(principal)) = caller else {
        let body = serde_json::to_vec(&JsonRpcErrorResponse::new(
            request.id.to_response_id(),
            JsonRpcError::with_data(
                -32000,
                "A2A requires an authenticated caller",
                serde_json::Value::Null,
            ),
        ))
        .unwrap_or_default();
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::CONTENT_TYPE, "application/a2a+json")],
            body,
        )
            .into_response();
    };
    let id = request.id.clone();
    match request.method.as_str() {
        "SendMessage" => {
            let params: MessageSendParams = match parse(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(id.to_response_id(), &error),
            };
            match send(
                &state,
                &principal,
                claims.as_ref().map(|c| &c.0),
                params.message,
            )
            .await
            {
                Ok(task) => rpc_success(id, &task),
                Err(error) => rpc_error(id.to_response_id(), &error),
            }
        }
        "SendStreamingMessage" => {
            let params: MessageSendParams = match parse(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(id.to_response_id(), &error),
            };
            match stream_message(
                &state,
                &principal,
                claims.as_ref().map(|c| &c.0),
                params.message,
            )
            .await
            {
                Ok(stream) => {
                    let response_id = id.to_response_id();
                    let sse = stream.map(move |result| -> Result<Event, Infallible> {
                        let value = match result {
                            Ok(response) => serde_json::to_value(JsonRpcSuccessResponse {
                                jsonrpc: JsonRpcVersion,
                                id: response_id.clone(),
                                result: &response,
                            })
                            .unwrap_or_default(),
                            Err(error) => serde_json::to_value(JsonRpcErrorResponse::new(
                                response_id.clone(),
                                JsonRpcError::with_data(
                                    error.json_rpc_code(),
                                    error.to_string(),
                                    error.to_error_info(),
                                ),
                            ))
                            .unwrap_or_default(),
                        };
                        Ok(Event::default().data(value.to_string()))
                    });
                    Sse::new(sse)
                        .keep_alive(KeepAlive::default())
                        .into_response()
                }
                Err(error) => rpc_error(id.to_response_id(), &error),
            }
        }
        "GetTask" => {
            let params: TaskQueryParams = match parse(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(id.to_response_id(), &error),
            };
            let owner = match state.owner(&principal) {
                Ok(owner) => owner,
                Err(error) => return rpc_error(id.to_response_id(), &error),
            };
            let scope = state
                .requests
                .for_caller(Some(state.caller(&owner, &principal)));
            match scope.tasks_get(&params.id, params.history_length).await {
                Ok(task) => rpc_success(id, &task),
                Err(error) => rpc_error(id.to_response_id(), &error),
            }
        }
        "CancelTask" => {
            let params: CancelTaskParams = match parse(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(id.to_response_id(), &error),
            };
            match cancel(&state, &principal, &params.id).await {
                Ok(task) => rpc_success(id, &task),
                Err(error) => rpc_error(id.to_response_id(), &error),
            }
        }
        "ListTasks" => {
            let params: a2a_protocol_types::params::ListTasksParams = match parse(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(id.to_response_id(), &error),
            };
            let owner = match state.owner(&principal) {
                Ok(owner) => owner,
                Err(error) => return rpc_error(id.to_response_id(), &error),
            };
            let scope = state
                .requests
                .for_caller(Some(state.caller(&owner, &principal)));
            let params = ListTasksParams {
                context_id: params.context_id,
                state: params.status,
                page_size: params.page_size,
                page_token: params.page_token,
                history_length: params.history_length,
                status_timestamp_after: params.status_timestamp_after,
                include_artifacts: params.include_artifacts,
            };
            match scope.tasks_list(params).await {
                Ok(tasks) => rpc_success(id, &tasks),
                Err(error) => rpc_error(id.to_response_id(), &error),
            }
        }
        "SubscribeToTask" => {
            let params: TaskIdParams = match parse(request.params) {
                Ok(params) => params,
                Err(error) => return rpc_error(id.to_response_id(), &error),
            };
            let owner = match state.owner(&principal) {
                Ok(owner) => owner,
                Err(error) => return rpc_error(id.to_response_id(), &error),
            };
            let scope = state
                .requests
                .for_caller(Some(state.caller(&owner, &principal)));
            match scope.tasks_subscribe(&params.id).await {
                Ok(stream) => {
                    let response_id = id.to_response_id();
                    let sse = stream.map(move |result| -> Result<Event, Infallible> {
                        let value = match result {
                            Ok(response) => serde_json::to_value(JsonRpcSuccessResponse {
                                jsonrpc: JsonRpcVersion,
                                id: response_id.clone(),
                                result: &response,
                            })
                            .unwrap_or_default(),
                            Err(error) => serde_json::to_value(JsonRpcErrorResponse::new(
                                response_id.clone(),
                                JsonRpcError::with_data(
                                    error.json_rpc_code(),
                                    error.to_string(),
                                    error.to_error_info(),
                                ),
                            ))
                            .unwrap_or_default(),
                        };
                        Ok(Event::default().data(value.to_string()))
                    });
                    Sse::new(sse)
                        .keep_alive(KeepAlive::default())
                        .into_response()
                }
                Err(error) => rpc_error(id.to_response_id(), &error),
            }
        }
        "CreateTaskPushNotificationConfig"
        | "GetTaskPushNotificationConfig"
        | "ListTaskPushNotificationConfigs"
        | "DeleteTaskPushNotificationConfig" => {
            rpc_error(id.to_response_id(), &A2aError::PushNotificationNotSupported)
        }
        "GetExtendedAgentCard" => rpc_error(
            id.to_response_id(),
            &A2aError::ExtendedAgentCardNotConfigured,
        ),
        other => rpc_error(
            id.to_response_id(),
            &A2aError::MethodNotFound {
                method: other.to_string(),
            },
        ),
    }
}

async fn send(
    state: &Arc<A2aState>,
    principal: &Principal,
    claims: Option<&DelegationClaims>,
    message: Message,
) -> Result<Task, A2aError> {
    validate_message(&message)?;
    let (agent, capability) = delegate_authority(state, claims, &message)?;
    claim_delegation(state, claims, &message).await?;
    let owner = state.owner(principal)?;
    let digest = message_digest(&message)?;
    let task_id = task_id(&state.instance_id, &owner, &message.id.0);

    if let Some(existing) = state.store.find_idempotent(&owner, &message.id.0).await? {
        if existing.1 != digest {
            return Err(conflict());
        }
        let entry = state.store.get_task(&existing.0).await?;
        return internal_task_to_wire(&entry);
    }

    let context_id = message
        .context_id
        .as_ref()
        .map(|context| context.0.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let now = chrono::Utc::now();
    let mut metadata = HashMap::new();
    copy_correlation(&mut metadata, &message);
    metadata.insert(
        OWNER_METADATA_KEY.to_string(),
        serde_json::Value::String(owner.clone()),
    );
    metadata.insert(
        "goldenbee.invocations".into(),
        serde_json::Value::Number(1.into()),
    );
    if let Some(capability) = &capability {
        metadata.insert(
            "goldenbee.capability".into(),
            serde_json::Value::String(capability.clone()),
        );
    }
    let entry = TaskStoreEntry {
        id: task_id.clone(),
        context_id: context_id.clone(),
        status: TaskStatus::with_timestamp(TaskState::Submitted),
        artifacts: Vec::new(),
        history: Vec::new(),
        metadata,
        push_configs: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    if !state
        .store
        .insert_new(&entry, &owner, &message.id.0, &digest)
        .await?
    {
        // Another request claimed this message id between the lookup and the
        // insert. Return its task, or conflict when the payload differs.
        let existing = state
            .store
            .find_idempotent(&owner, &message.id.0)
            .await?
            .ok_or_else(|| A2aError::Internal {
                message: "A2A idempotency record disappeared".into(),
            })?;
        if existing.1 != digest {
            return Err(conflict());
        }
        let entry = state.store.get_task(&existing.0).await?;
        return internal_task_to_wire(&entry);
    }
    state
        .store
        .add_history_message(&task_id, message.clone())
        .await?;

    let token = CancellationToken::new();
    state
        .tokens
        .lock()
        .await
        .insert(task_id.clone(), token.clone());
    // The native child owns its own execution. Spawning and awaiting the handle
    // keeps the task running when a parent connection is lost or the parent
    // process is killed; dropping only the join handle does not cancel it.
    let runner = state.clone();
    let runner_task = task_id.clone();
    let runner_context = context_id.clone();
    let runner_owner = owner.clone();
    let runner_scopes = principal.scopes.clone();
    let runner_message = message.clone();
    let handle = tokio::spawn(async move {
        run_task(
            &runner,
            &runner_task,
            &runner_context,
            &runner_owner,
            &runner_scopes,
            &runner_message,
            agent,
            token,
            None,
        )
        .await;
    });
    let _ = handle.await;
    let entry = state.store.get_task(&task_id).await?;
    internal_task_to_wire(&entry)
}

async fn stream_message(
    state: &Arc<A2aState>,
    principal: &Principal,
    claims: Option<&DelegationClaims>,
    message: Message,
) -> Result<futures::stream::BoxStream<'static, Result<StreamResponse, A2aError>>, A2aError> {
    validate_message(&message)?;
    let (agent, capability) = delegate_authority(state, claims, &message)?;
    claim_delegation(state, claims, &message).await?;
    let owner = state.owner(principal)?;
    let digest = message_digest(&message)?;
    let task_id = task_id(&state.instance_id, &owner, &message.id.0);

    if let Some(existing) = state.store.find_idempotent(&owner, &message.id.0).await? {
        if existing.1 != digest {
            return Err(conflict());
        }
        let entry = state.store.get_task(&existing.0).await?;
        let task = internal_task_to_wire(&entry)?;
        return Ok(futures::stream::once(async move { Ok(StreamResponse::Task(task)) }).boxed());
    }

    let context_id = message
        .context_id
        .as_ref()
        .map(|context| context.0.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let now = chrono::Utc::now();
    let mut metadata = HashMap::new();
    copy_correlation(&mut metadata, &message);
    metadata.insert(
        OWNER_METADATA_KEY.to_string(),
        serde_json::Value::String(owner.clone()),
    );
    metadata.insert(
        "goldenbee.invocations".into(),
        serde_json::Value::Number(1.into()),
    );
    if let Some(capability) = &capability {
        metadata.insert(
            "goldenbee.capability".into(),
            serde_json::Value::String(capability.clone()),
        );
    }
    let entry = TaskStoreEntry {
        id: task_id.clone(),
        context_id: context_id.clone(),
        status: TaskStatus::with_timestamp(TaskState::Submitted),
        artifacts: Vec::new(),
        history: Vec::new(),
        metadata,
        push_configs: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    if !state
        .store
        .insert_new(&entry, &owner, &message.id.0, &digest)
        .await?
    {
        let existing = state
            .store
            .find_idempotent(&owner, &message.id.0)
            .await?
            .ok_or_else(|| A2aError::Internal {
                message: "A2A idempotency record disappeared".into(),
            })?;
        if existing.1 != digest {
            return Err(conflict());
        }
        let entry = state.store.get_task(&existing.0).await?;
        let task = internal_task_to_wire(&entry)?;
        return Ok(futures::stream::once(async move { Ok(StreamResponse::Task(task)) }).boxed());
    }
    state
        .store
        .add_history_message(&task_id, message.clone())
        .await?;

    let first = internal_task_to_wire(&state.store.get_task(&task_id).await?)?;
    let (sink, receiver) = mpsc::channel(64);
    let _ = sink.send(StreamResponse::Task(first)).await;
    let token = CancellationToken::new();
    state
        .tokens
        .lock()
        .await
        .insert(task_id.clone(), token.clone());
    let state = state.clone();
    let scopes = principal.scopes.clone();
    let message = message.clone();
    let stream_task = task_id.clone();
    let stream_context = context_id.clone();
    tokio::spawn(async move {
        run_task(
            &state,
            &stream_task,
            &stream_context,
            &owner,
            &scopes,
            &message,
            agent,
            token,
            Some(sink),
        )
        .await;
    });

    let stream = futures::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (Ok(item), receiver))
    });
    Ok(stream.boxed())
}

async fn cancel(
    state: &Arc<A2aState>,
    principal: &Principal,
    task_id: &str,
) -> Result<Task, A2aError> {
    let owner = state.owner(principal)?;
    let scope = state
        .requests
        .for_caller(Some(state.caller(&owner, principal)));
    // Native ownership and state-machine validation run first. Only a task
    // this caller may cancel reaches the cooperative token.
    let task = scope.tasks_cancel(task_id).await?;
    if let Some(token) = state.tokens.lock().await.get(task_id) {
        token.cancel();
    }
    Ok(task)
}

#[allow(clippy::too_many_arguments)]
async fn run_task(
    state: &Arc<A2aState>,
    task_id: &str,
    context_id: &str,
    owner: &str,
    scopes: &[String],
    message: &Message,
    agent: Arc<dyn Agent>,
    token: CancellationToken,
    sink: Option<mpsc::Sender<StreamResponse>>,
) {
    if state
        .executor
        .transition_state(task_id, context_id, TaskState::Working, None)
        .await
        .is_ok()
    {
        state.emit_status(task_id, context_id, &sink).await;
    } else if state
        .store
        .get_task(task_id)
        .await
        .is_ok_and(|entry| super::store::is_terminal(entry.status.state))
    {
        // A cancel or another terminal decision won the race before the run
        // started. Do not invoke the objective.
        state.emit_status(task_id, context_id, &sink).await;
        state.tokens.lock().await.remove(task_id);
        return;
    }

    let result = invoke(state, agent, context_id, owner, scopes, message, &token).await;
    if token.is_cancelled() {
        if let Ok(entry) = state.store.get_task(task_id).await {
            if !super::store::is_terminal(entry.status.state) {
                let _ = state
                    .executor
                    .transition_state(task_id, context_id, TaskState::Canceled, None)
                    .await;
            }
        }
    } else {
        match result {
            Ok(text) => {
                if !token.is_cancelled() && !text.is_empty() {
                    let artifact = Artifact::new(
                        ArtifactId::new(uuid::Uuid::new_v4().to_string()),
                        vec![a2a_protocol_types::Part::text(&text)],
                    );
                    if let Err(error) = state
                        .executor
                        .record_artifact(task_id, context_id, artifact.clone())
                        .await
                    {
                        let _ = state
                            .executor
                            .fail_task(task_id, context_id, &error.to_string())
                            .await;
                    } else {
                        state
                            .emit_artifact(task_id, context_id, artifact, &sink)
                            .await;
                    }
                }
                if let Ok(entry) = state.store.get_task(task_id).await {
                    if !super::store::is_terminal(entry.status.state) {
                        let _ = state
                            .executor
                            .transition_state(task_id, context_id, TaskState::Completed, None)
                            .await;
                    }
                }
            }
            Err(error) => {
                if let Ok(entry) = state.store.get_task(task_id).await {
                    if !super::store::is_terminal(entry.status.state) {
                        let _ = state
                            .executor
                            .fail_task(task_id, context_id, &error.to_string())
                            .await;
                    }
                }
            }
        }
    }
    state.emit_status(task_id, context_id, &sink).await;
    state.tokens.lock().await.remove(task_id);
}

async fn invoke(
    state: &A2aState,
    agent: Arc<dyn Agent>,
    context_id: &str,
    owner: &str,
    scopes: &[String],
    message: &Message,
    token: &CancellationToken,
) -> Result<String, A2aError> {
    ensure_session(state, owner, context_id).await?;

    let mut parts = Vec::new();
    for part in &message.parts {
        parts.push(
            wire_part_to_adk(part).map_err(|error| A2aError::InvalidParams {
                message: error.to_string(),
            })?,
        );
    }
    let content = Content {
        role: "user".to_string(),
        parts,
    };

    let mut builder = Runner::builder()
        .app_name(state.app_name.clone())
        .agent(agent)
        .session_service(state.sessions.clone())
        .request_context(adk_rust::RequestContext {
            user_id: owner.to_string(),
            scopes: scopes.to_vec(),
            metadata: HashMap::new(),
        })
        .cancellation_token(token.clone());
    if let Some(artifacts) = &state.artifacts {
        builder = builder.artifact_service(artifacts.clone());
    }
    if let Some(memory) = &state.memory {
        let service = adk_memory::MemoryServiceAdapter::new(
            memory.clone(),
            state.app_name.clone(),
            owner.to_string(),
        );
        builder = builder.memory_service(Arc::new(service) as Arc<dyn adk_rust::Memory>);
    }
    if let Some(plugins) = &state.plugins {
        builder = builder.plugin_manager(plugins.clone());
    }
    let runner = builder.build().map_err(|error| A2aError::Internal {
        message: format!("native Runner could not be constructed: {error}"),
    })?;

    let user_id = UserId::new(owner).map_err(|error| A2aError::Internal {
        message: format!("native A2A user identity is invalid: {error}"),
    })?;
    let session_id = SessionId::new(context_id).map_err(|error| A2aError::Internal {
        message: format!("native A2A context identity is invalid: {error}"),
    })?;
    let mut stream = tokio::select! {
        _ = token.cancelled() => return Ok(String::new()),
        result = runner.run(user_id, session_id, content) => {
            result.map_err(|error| A2aError::Internal { message: format!("native invocation was not admitted: {error}") })?
        }
    };
    let mut output = String::new();
    loop {
        let next = tokio::select! {
            _ = token.cancelled() => return Ok(output),
            next = stream.next() => next,
        };
        let Some(event) = next else { break };
        let event = event.map_err(|error| A2aError::Internal {
            message: format!("native invocation failed: {error}"),
        })?;
        if event.llm_response.partial {
            continue;
        }
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    output.push_str(text);
                }
            }
        }
    }
    Ok(output)
}

async fn ensure_session(state: &A2aState, owner: &str, context_id: &str) -> Result<(), A2aError> {
    let existing = state
        .sessions
        .get(GetRequest {
            app_name: state.app_name.clone(),
            user_id: owner.to_string(),
            session_id: context_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await;
    match existing {
        Ok(_) => Ok(()),
        Err(error) if error.category == adk_rust::ErrorCategory::NotFound => state
            .sessions
            .create(CreateRequest {
                app_name: state.app_name.clone(),
                user_id: owner.to_string(),
                session_id: Some(context_id.to_string()),
                state: HashMap::new(),
            })
            .await
            .map(|_| ())
            .map_err(|error| A2aError::Internal {
                message: format!("native A2A session could not be created: {error}"),
            }),
        Err(error) => Err(A2aError::Internal {
            message: format!("native A2A session could not be opened: {error}"),
        }),
    }
}

fn delegate_authority(
    state: &A2aState,
    claims: Option<&DelegationClaims>,
    message: &Message,
) -> Result<(Arc<dyn Agent>, Option<String>), A2aError> {
    let mut capability = message_capability(message)?;
    if let Some(claims) = claims {
        if capability.is_none() {
            // A Gateway delegation that does not name a capability still
            // selects the instance's configured default agent, but only when
            // that agent is inside the delegated capability set.
            if !claims
                .capabilities
                .iter()
                .any(|granted| granted == &state.default_agent)
            {
                return Err(A2aError::InvalidParams {
                    message:
                        "delegated A2A messages must name a capability granted by the delegation"
                            .into(),
                });
            }
            capability = Some(state.default_agent.clone());
        }
        let requested = capability.as_ref().expect("capability is present");
        if !claims
            .capabilities
            .iter()
            .any(|granted| granted == requested)
        {
            return Err(A2aError::InvalidParams {
                message: format!("delegation does not grant capability '{requested}'"),
            });
        }
    }
    let agent = state.agent_for(capability.as_deref())?;
    Ok((agent, capability))
}

async fn claim_delegation(
    state: &A2aState,
    claims: Option<&DelegationClaims>,
    message: &Message,
) -> Result<(), A2aError> {
    let Some(claims) = claims else {
        return Ok(());
    };
    match state
        .store
        .claim_delegation(&claims.token_id, &message.id.0)
        .await?
    {
        super::store::DelegationClaim::New | super::store::DelegationClaim::Same => Ok(()),
        super::store::DelegationClaim::Replayed => Err(A2aError::InvalidParams {
            message: "delegation.replayed: token was already used for another objective".into(),
        }),
    }
}

fn message_capability(message: &Message) -> Result<Option<String>, A2aError> {
    let Some(metadata) = &message.metadata else {
        return Ok(None);
    };
    let Some(value) = metadata.get("goldenbee.capability") else {
        return Ok(None);
    };
    let Some(capability) = value.as_str() else {
        return Err(A2aError::InvalidParams {
            message: "goldenbee.capability must be a string".into(),
        });
    };
    if capability.trim().is_empty() {
        return Err(A2aError::InvalidParams {
            message: "goldenbee.capability must not be empty".into(),
        });
    }
    Ok(Some(capability.to_string()))
}

fn copy_correlation(metadata: &mut HashMap<String, serde_json::Value>, message: &Message) {
    let Some(source) = &message.metadata else {
        return;
    };
    let Some(object) = source.as_object() else {
        return;
    };
    for (key, value) in object {
        if key.starts_with("goldenbee.") {
            metadata.insert(key.clone(), value.clone());
        }
    }
}

fn validate_message(message: &Message) -> Result<(), A2aError> {
    if message.parts.is_empty() {
        return Err(A2aError::InvalidParams {
            message: "message must contain at least one part".into(),
        });
    }
    let id = message.id.0.trim();
    if id.is_empty() {
        return Err(A2aError::InvalidParams {
            message: "messageId must not be empty or whitespace-only".into(),
        });
    }
    if message.id.0.len() > 256 {
        return Err(A2aError::InvalidParams {
            message: "messageId exceeds 256 character limit".into(),
        });
    }
    if let Some(metadata) = &message.metadata {
        let size = serde_json::to_vec(metadata)
            .map(|value| value.len())
            .unwrap_or(0);
        if size > 65_536 {
            return Err(A2aError::InvalidParams {
                message: format!("metadata exceeds 64 KB limit ({size} bytes)"),
            });
        }
    }
    Ok(())
}

fn message_digest(message: &Message) -> Result<String, A2aError> {
    let encoded = serde_json::to_vec(message).map_err(|error| A2aError::InvalidParams {
        message: format!("message cannot be canonicalized: {error}"),
    })?;
    let digest = Sha256::digest(encoded);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn task_id(instance: &str, owner: &str, message_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(instance.as_bytes());
    hasher.update(b"\0");
    hasher.update(owner.as_bytes());
    hasher.update(b"\0");
    hasher.update(message_id.as_bytes());
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("a2a-{digest}")
}

fn conflict() -> A2aError {
    A2aError::InvalidParams {
        message: "messageId was already used with a different payload".into(),
    }
}

fn parse<T: DeserializeOwned>(params: Option<serde_json::Value>) -> Result<T, A2aError> {
    serde_json::from_value(params.unwrap_or(serde_json::Value::Null)).map_err(|error| {
        A2aError::InvalidParams {
            message: error.to_string(),
        }
    })
}

fn rpc_success<T: serde::Serialize>(id: JsonRpcRequestId, result: &T) -> Response {
    rpc_json(JsonRpcSuccessResponse {
        jsonrpc: JsonRpcVersion,
        id: id.to_response_id(),
        result,
    })
}

fn rpc_error(id: JsonRpcId, error: &A2aError) -> Response {
    rpc_json(JsonRpcErrorResponse::new(
        id,
        JsonRpcError::with_data(
            error.json_rpc_code(),
            error.to_string(),
            error.to_error_info(),
        ),
    ))
}

fn rpc_json<T: serde::Serialize>(value: T) -> Response {
    let body = serde_json::to_vec(&value).unwrap_or_default();
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/a2a+json".to_string(),
        )],
        body,
    )
        .into_response()
}
