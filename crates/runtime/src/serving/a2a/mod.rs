//! Native A2A v1 serving for a Worker Instance.
//!
//! The pinned release supplies the protocol types, `RequestHandler`,
//! `V1Executor`, `TaskStore` state machine, agent card, and version
//! negotiation. This module adds only the per-instance durable store, a thin
//! dispatch wrapper for durable idempotency and per-task cancellation, and the
//! route/card projection. It does not implement another A2A state machine.

mod dispatch;
mod store;

pub use store::{DelegationClaim, DurableTaskStore, A2A_STORE_FILE};

use crate::agents::PackageManifest;
use crate::data::artifacts::NativeArtifactStore;
use crate::data::memory::NativeMemory;
use crate::execution::runner::native_user_id;
use crate::Runtime;
use a2a_protocol_types::events::{TaskArtifactUpdateEvent, TaskStatusUpdateEvent};
use a2a_protocol_types::{AgentCapabilities, AgentSkill};
use adk_rust::plugin::PluginManager;
use adk_rust::server::a2a::v1::card::{build_v1_agent_card, CachedAgentCard};
use adk_rust::server::a2a::v1::error::A2aError;
use adk_rust::server::a2a::v1::executor::V1Executor;
use adk_rust::server::a2a::v1::push::PushNotificationSender;
use adk_rust::server::a2a::v1::request_handler::RequestHandler;
use adk_rust::server::a2a::v1::task_store::TaskStore;
use adk_rust::Agent;
use async_trait::async_trait;
use axum::extract::State;
use axum::http::header::{CONTENT_TYPE, ETAG};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use common::Principal;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// One running native A2A surface.
pub struct A2aServing {
    router: Router,
}

impl A2aServing {
    /// Open the per-instance durable task store and build the A2A v1 router.
    ///
    /// Storage is opened and checked before this returns, so serving never
    /// advertises durability before the backend exists.
    pub async fn open(
        runtime: Runtime,
        manifest: &PackageManifest,
        agents: BTreeMap<String, Arc<dyn Agent>>,
        default_agent: String,
        sessions: Arc<dyn adk_rust::session::SessionService>,
        plugins: Option<Arc<PluginManager>>,
        base_url: &str,
    ) -> crate::Result<Self> {
        if agents.is_empty() {
            return Err(crate::ApiError::new(
                "a2a.agent_missing",
                "A2A serving requires at least one resolved native agent",
            ));
        }
        if !agents.contains_key(&default_agent) {
            return Err(crate::ApiError::new(
                "a2a.agent_missing",
                format!("default A2A agent '{default_agent}' was not resolved"),
            ));
        }
        let store = DurableTaskStore::open(&runtime.config().data_dir).await?;
        let repaired = store.mark_interrupted().await?;
        if repaired > 0 {
            eprintln!("a2a: retained {repaired} interrupted non-terminal tasks");
        }
        let executor = Arc::new(V1Executor::new(store.clone()));
        let card = Arc::new(RwLock::new(CachedAgentCard::new(build_card(
            manifest, &agents, base_url,
        ))));
        let push: Arc<dyn PushNotificationSender> = Arc::new(RejectingPushSender);
        let requests = Arc::new(RequestHandler::new(
            executor.clone(),
            store.clone(),
            push,
            card.clone(),
        ));
        let state = Arc::new(A2aState {
            instance_id: runtime.config().instance_id.clone(),
            app_name: manifest.name.clone(),
            agents,
            default_agent,
            sessions,
            artifacts: Some(runtime.artifact_store()),
            memory: runtime.memory().await,
            plugins,
            store,
            executor,
            requests,
            card,
            tokens: Mutex::new(HashMap::new()),
        });
        let router = Router::new()
            .route("/a2a", post(dispatch::jsonrpc))
            .route("/.well-known/agent.json", get(agent_card))
            .route("/.well-known/agent-card.json", get(agent_card))
            .layer(axum::middleware::from_fn(
                adk_rust::server::a2a::v1::version::version_negotiation,
            ))
            .with_state(state.clone());
        Ok(Self { router })
    }

    /// Router to merge with the harness and native development routes.
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// Whether A2A is backed by the durable per-instance store.
    pub fn durable(&self) -> bool {
        true
    }
}

struct A2aState {
    instance_id: String,
    app_name: String,
    agents: BTreeMap<String, Arc<dyn Agent>>,
    default_agent: String,
    sessions: Arc<dyn adk_rust::session::SessionService>,
    artifacts: Option<Arc<NativeArtifactStore>>,
    memory: Option<Arc<NativeMemory>>,
    plugins: Option<Arc<PluginManager>>,
    store: Arc<DurableTaskStore>,
    executor: Arc<V1Executor>,
    requests: Arc<RequestHandler>,
    card: Arc<RwLock<CachedAgentCard>>,
    /// Per-task cooperative cancellation. Process-local by design: a restart
    /// has no live run to cancel, and the durable task state is retained.
    tokens: Mutex<HashMap<String, CancellationToken>>,
}

impl A2aState {
    fn agent_for(&self, capability: Option<&str>) -> Result<Arc<dyn Agent>, A2aError> {
        match capability {
            Some(capability) => {
                self.agents
                    .get(capability)
                    .cloned()
                    .ok_or_else(|| A2aError::InvalidParams {
                        message: format!("capability '{capability}' is not served by this Worker"),
                    })
            }
            None => self
                .agents
                .get(&self.default_agent)
                .cloned()
                .ok_or_else(|| A2aError::Internal {
                    message: format!(
                        "default A2A agent '{}' is no longer resolved",
                        self.default_agent
                    ),
                }),
        }
    }

    fn owner(&self, principal: &Principal) -> Result<String, A2aError> {
        native_user_id(&principal.namespace, &principal.id).map_err(|error| A2aError::Internal {
            message: error.message,
        })
    }

    fn caller(&self, owner: &str, principal: &Principal) -> adk_rust::RequestContext {
        adk_rust::RequestContext {
            user_id: owner.to_string(),
            scopes: principal.scopes.clone(),
            metadata: HashMap::new(),
        }
    }

    async fn emit_status(
        &self,
        task_id: &str,
        context_id: &str,
        sink: &Option<tokio::sync::mpsc::Sender<a2a_protocol_types::events::StreamResponse>>,
    ) {
        let Some(sink) = sink else { return };
        let Ok(entry) = self.store.get_task(task_id).await else {
            return;
        };
        let _ = sink
            .send(a2a_protocol_types::events::StreamResponse::StatusUpdate(
                TaskStatusUpdateEvent {
                    task_id: a2a_protocol_types::TaskId::new(task_id),
                    context_id: a2a_protocol_types::ContextId::new(context_id),
                    status: entry.status,
                    metadata: None,
                },
            ))
            .await;
    }

    async fn emit_artifact(
        &self,
        task_id: &str,
        context_id: &str,
        artifact: a2a_protocol_types::Artifact,
        sink: &Option<tokio::sync::mpsc::Sender<a2a_protocol_types::events::StreamResponse>>,
    ) {
        let Some(sink) = sink else { return };
        let _ = sink
            .send(a2a_protocol_types::events::StreamResponse::ArtifactUpdate(
                TaskArtifactUpdateEvent {
                    task_id: a2a_protocol_types::TaskId::new(task_id),
                    context_id: a2a_protocol_types::ContextId::new(context_id),
                    artifact,
                    append: Some(false),
                    last_chunk: Some(true),
                    metadata: None,
                },
            ))
            .await;
    }
}

fn build_card(
    manifest: &PackageManifest,
    agents: &BTreeMap<String, Arc<dyn Agent>>,
    base_url: &str,
) -> a2a_protocol_types::AgentCard {
    let url = format!("{}/a2a", base_url.trim_end_matches('/'));
    // Advertise only agents this instance actually routes. The compiled
    // manifest also lists tools and toolsets; those are not A2A skills.
    let skills = agents
        .iter()
        .map(|(id, agent)| {
            let description = manifest
                .capabilities
                .iter()
                .find(|capability| &capability.id == id)
                .map(|capability| capability.description.clone())
                .filter(|description| !description.is_empty())
                .unwrap_or_else(|| agent.description().to_string());
            AgentSkill {
                id: id.clone(),
                name: id.clone(),
                description,
                tags: vec!["agent".into()],
                examples: None,
                input_modes: None,
                output_modes: None,
                security_requirements: None,
            }
        })
        .collect();
    // Only behavior this module actually serves is advertised. Push delivery
    // and an extended card are rejected by dispatch, so both are false.
    let capabilities = AgentCapabilities::none()
        .with_streaming(true)
        .with_push_notifications(false)
        .with_extended_agent_card(false);
    build_v1_agent_card(
        &manifest.name,
        &format!("{} Worker A2A v1 endpoint", manifest.name),
        &url,
        &manifest.version,
        skills,
        capabilities,
    )
}

/// Push notification delivery is not implemented. This sender exists so the
/// native request handler can be built; dispatch rejects every push operation
/// before it reaches a handler that would advertise or silently accept one.
struct RejectingPushSender;

#[async_trait]
impl PushNotificationSender for RejectingPushSender {
    async fn send_status_update(
        &self,
        _url: &str,
        _event: &TaskStatusUpdateEvent,
        _config: &a2a_protocol_types::TaskPushNotificationConfig,
    ) -> Result<(), A2aError> {
        Err(A2aError::PushNotificationNotSupported)
    }

    async fn send_artifact_update(
        &self,
        _url: &str,
        _event: &TaskArtifactUpdateEvent,
        _config: &a2a_protocol_types::TaskPushNotificationConfig,
    ) -> Result<(), A2aError> {
        Err(A2aError::PushNotificationNotSupported)
    }
}

async fn agent_card(State(state): State<Arc<A2aState>>, headers: HeaderMap) -> Response {
    let card = state.card.read().await;
    if let Some(value) = headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
    {
        if card.matches_etag(value) {
            return StatusCode::NOT_MODIFIED.into_response();
        }
    }
    (
        [
            (CONTENT_TYPE, "application/json".to_string()),
            (ETAG, format!("\"{}\"", card.etag)),
        ],
        card.card_json.clone(),
    )
        .into_response()
}
