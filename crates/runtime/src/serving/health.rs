//! Liveness, status, and capability projections.

use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use common::{BrowserCapabilityInventory, CapabilityInventory, Principal};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct Liveness {
    status: &'static str,
    instance: String,
    incarnation: String,
}

pub(super) async fn liveness(State(state): State<WorkerState>) -> Json<Liveness> {
    Json(Liveness {
        status: "live",
        instance: state.runtime.config().instance_id.clone(),
        incarnation: state.runtime.incarnation().to_string(),
    })
}

pub(super) async fn status(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<common::Health> {
    principal
        .require(common::scopes::TASK_READ)
        .map_err(failure)?;
    let mut health = state.runtime.health().await;
    if let Some(browser) = &state.browser {
        health
            .subsystems
            .insert("browser".into(), browser.readiness().await.to_string());
    }
    let mut mcp_servers = Vec::new();
    if let Some(local) = &state.mcp {
        mcp_servers.extend(local.inventory().await);
    }
    if let Some(remote) = &state.remote_mcp {
        mcp_servers.extend(remote.inventory().await);
    }
    if !mcp_servers.is_empty() {
        let connected = mcp_servers
            .iter()
            .filter(|server| server.state == "connected")
            .count();
        health.subsystems.insert(
            "mcp".into(),
            format!("{connected}/{} connected", mcp_servers.len()),
        );
    }
    Ok(Json(health))
}

pub(super) async fn capabilities(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<CapabilityInventory> {
    principal
        .require(common::scopes::TASK_READ)
        .map_err(failure)?;
    let browser = match &state.browser {
        Some(controller) => {
            let settings = controller.settings().await;
            if settings.enabled {
                Some(BrowserCapabilityInventory {
                    toolset: controller.id().to_string(),
                    enabled: true,
                    ready: controller.readiness().await.to_string(),
                    profile: format!("{:?}", settings.profile).to_ascii_lowercase(),
                    max_sessions: settings.max_sessions,
                    active_sessions: controller.active_count().await,
                    evaluate_js: false,
                    streaming: false,
                    takeover: true,
                    control_protocol: "fenced-lease-v1".into(),
                    control_commands: vec![
                        "navigate".into(),
                        "click".into(),
                        "type".into(),
                        "extract_text".into(),
                    ],
                    terminal_control: false,
                })
            } else {
                None
            }
        }
        None => None,
    };
    let mut mcp_servers = Vec::new();
    if let Some(local) = &state.mcp {
        mcp_servers.extend(local.inventory().await);
    }
    if let Some(remote) = &state.remote_mcp {
        mcp_servers.extend(remote.inventory().await);
    }
    mcp_servers.sort_by(|left, right| left.id.cmp(&right.id));
    let mcp = (!mcp_servers.is_empty()).then_some(common::McpCapabilityInventory {
        servers: mcp_servers,
    });
    Ok(Json(CapabilityInventory {
        package: state.manifest.name.clone(),
        version: state.manifest.version.clone(),
        protocol: state.manifest.protocol.clone(),
        runtime: state.manifest.runtime.clone(),
        adk: state.manifest.adk.clone(),
        instance: state.runtime.config().instance_id.clone(),
        durable_tasks: true,
        a2a_task_storage: if state.a2a_durable {
            "sqlite"
        } else {
            "in_memory"
        }
        .into(),
        a2a_durable: state.a2a_durable,
        artifact_storage: "native_adk".into(),
        artifact_transfer: "authenticated_or_scoped_grant".into(),
        artifact_max_bytes: crate::data::artifacts::MAX_ARTIFACT_BYTES,
        capabilities: state.runtime.config().capabilities.clone(),
        installed_capabilities: state.manifest.capabilities.clone(),
        commands: match &state.commands {
            Some(commands) => commands.descriptors().to_vec(),
            None => Vec::new(),
        },
        plugins: state.plugins.iter().cloned().map(Into::into).collect(),
        browser,
        mcp,
    }))
}
