//! Runtime-owned neutral session, context, and permission commands.
//!
//! These commands read the same durable Runtime and immutable asset revision
//! that tasks use. They render identically through the CLI and HTTP/SDK
//! surfaces, and they never mutate policy, sessions, or capabilities.

use crate::execution::runner::native_user_id;
use crate::Runtime;
use adk_rust::session::SessionService;
use common::{command_kinds, scopes, ApiError, CommandDescriptor, CommandRequest, Principal};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use super::CommandRegistry;

const MAX_SESSIONS: usize = 50;

/// Every neutral command in stable discovery order.
pub(crate) const NEUTRAL: &[NeutralCommand] = &[
    NeutralCommand {
        name: "session.info",
        description: "Show the current session identity and bounded history counts.",
    },
    NeutralCommand {
        name: "session.list",
        description: "List sessions visible to the authenticated principal.",
    },
    NeutralCommand {
        name: "context.info",
        description: "Show the resolved context inputs for the active asset revision.",
    },
    NeutralCommand {
        name: "permissions.info",
        description: "Show the effective package capabilities and instance grants.",
    },
    NeutralCommand {
        name: "skills.list",
        description: "List discovered skills with progressive-disclosure metadata.",
    },
    NeutralCommand {
        name: "commands.list",
        description: "List every runtime and Worker-defined command.",
    },
];

/// One neutral runtime command descriptor.
pub(crate) struct NeutralCommand {
    /// Stable command name.
    pub name: &'static str,
    /// Operator-facing description.
    pub description: &'static str,
}

impl NeutralCommand {
    pub(crate) fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: self.name.into(),
            description: self.description.into(),
            kind: command_kinds::NEUTRAL.into(),
            source: "runtime".into(),
            arguments: Vec::new(),
            allowed_tools: Vec::new(),
            model: None,
        }
    }
}

/// Whether a name is a runtime-owned neutral command.
pub(crate) fn is_neutral(name: &str) -> bool {
    NEUTRAL.iter().any(|command| command.name == name)
}

/// Execute one neutral command against the owning Runtime.
pub(crate) async fn execute(
    name: &str,
    registry: &CommandRegistry,
    runtime: &Runtime,
    principal: &Principal,
    request: &CommandRequest,
) -> Result<Value, ApiError> {
    principal.require(scopes::TASK_READ)?;
    match name {
        "session.info" => session_info(runtime, principal, request).await,
        "session.list" => session_list(runtime, principal).await,
        "context.info" => Ok(context_info(registry, runtime)),
        "permissions.info" => Ok(permissions_info(registry, runtime, principal)),
        "skills.list" => Ok(json!({"skills": registry.skills()})),
        "commands.list" => Ok(json!({"commands": registry.descriptors()})),
        other => Err(ApiError::new(
            "command.not_found",
            format!("command '{other}' is not a neutral runtime command"),
        )),
    }
}

async fn session_info(
    runtime: &Runtime,
    principal: &Principal,
    request: &CommandRequest,
) -> Result<Value, ApiError> {
    let sessions = sessions(runtime).await?;
    let app = runtime.config().package.clone();
    let user = native_user_id(&principal.namespace, &principal.id)?;
    let session = sessions
        .get(adk_rust::session::GetRequest {
            app_name: app.clone(),
            user_id: user.clone(),
            session_id: request.session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await
        .map_err(|error| {
            ApiError::new(
                "session.not_found",
                format!("session '{}' is not visible: {error}", request.session_id),
            )
        })?;
    let mut state_keys: Vec<String> = session.state().all().keys().cloned().collect();
    state_keys.sort();
    Ok(json!({
        "session_id": session.id(),
        "app": app,
        "user": user,
        "event_count": session.events().len(),
        "state_keys": state_keys,
        "updated_at": session.last_update_time().to_rfc3339(),
    }))
}

async fn session_list(runtime: &Runtime, principal: &Principal) -> Result<Value, ApiError> {
    let sessions = sessions(runtime).await?;
    let app = runtime.config().package.clone();
    let user = native_user_id(&principal.namespace, &principal.id)?;
    let listed = sessions
        .list(adk_rust::session::ListRequest {
            app_name: app.clone(),
            user_id: user,
            limit: Some(MAX_SESSIONS),
            offset: None,
        })
        .await
        .map_err(|error| {
            ApiError::new(
                "session.list_failed",
                format!("sessions are unavailable: {error}"),
            )
        })?;
    let items: Vec<Value> = listed
        .into_iter()
        .map(|session| {
            json!({
                "session_id": session.id(),
                "event_count": session.events().len(),
                "updated_at": session.last_update_time().to_rfc3339(),
            })
        })
        .collect();
    Ok(json!({"app": app, "sessions": items}))
}

fn context_info(registry: &CommandRegistry, runtime: &Runtime) -> Value {
    json!({
        "instance": runtime.config().instance_id,
        "package": runtime.config().package,
        "package_version": runtime.config().package_version,
        "config_revision": runtime.config().config_revision,
        "default_agent": registry.default_agent(),
        "agents": registry.catalog(),
        "skills": registry.skills(),
        "prompts": registry.prompts(),
        "commands": registry
            .descriptors()
            .iter()
            .map(|command| command.name.clone())
            .collect::<Vec<_>>(),
        "capabilities": runtime.config().capabilities,
        "disclosure": registry.disclosure(),
    })
}

fn permissions_info(registry: &CommandRegistry, runtime: &Runtime, principal: &Principal) -> Value {
    let mut grants = runtime
        .config()
        .instance_grants
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    grants.sort();
    let capabilities: Vec<Value> = runtime
        .config()
        .capabilities
        .iter()
        .map(|capability| {
            json!({
                "id": capability.id,
                "kind": capability.kind,
                "permissions": capability.permissions,
            })
        })
        .collect();
    json!({
        "principal": principal.id,
        "namespace": principal.namespace,
        "scopes": principal.scopes,
        "instance_grants": grants,
        "mode": "package",
        "decision_source": "package-declared capabilities only",
        "required_permissions": registry.required_permissions(),
        "capabilities": capabilities,
    })
}

async fn sessions(
    runtime: &Runtime,
) -> Result<std::sync::Arc<crate::sessions::FileSessionService>, ApiError> {
    runtime.sessions().await.ok_or_else(|| {
        ApiError::new(
            "session.unavailable",
            "this Worker has no durable session service",
        )
    })
}

/// Argument map supplied by a neutral command. Neutral commands currently take
/// no arguments; accepting an unknown key would imply policy they do not have.
pub(crate) fn reject_arguments(arguments: &BTreeMap<String, String>) -> Result<(), ApiError> {
    if let Some(name) = arguments.keys().next() {
        return Err(ApiError::new(
            "command.arguments_unexpected",
            format!("neutral commands take no arguments, found '{name}'"),
        ));
    }
    Ok(())
}
