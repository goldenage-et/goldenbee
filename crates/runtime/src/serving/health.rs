//! Liveness, status, and capability projections.

use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use common::Principal;
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
    Ok(Json(health))
}

/// Browser capability projection. Absent when the package did not declare it.
#[derive(Debug, Serialize)]
pub(super) struct BrowserCapability {
    toolset: String,
    enabled: bool,
    ready: String,
    profile: String,
    max_sessions: usize,
    active_sessions: usize,
    evaluate_js: bool,
    streaming: bool,
    takeover: bool,
    control_protocol: &'static str,
    control_commands: [&'static str; 4],
    terminal_control: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct Capabilities {
    package: String,
    version: String,
    protocol: String,
    runtime: String,
    adk: String,
    instance: String,
    durable_tasks: bool,
    a2a_task_storage: &'static str,
    a2a_durable: bool,
    artifact_storage: &'static str,
    artifact_transfer: &'static str,
    artifact_max_bytes: usize,
    /// Configured agent ids this instance can route.
    capabilities: Vec<common::CapabilityDescriptor>,
    /// Installed executable functionality declared by the package manifest.
    installed_capabilities: Vec<common::CapabilityDescriptor>,
    /// Discoverable runtime and Worker-defined commands.
    commands: Vec<common::CommandDescriptor>,
    /// Package-declared browser capability, when installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    browser: Option<BrowserCapability>,
}

pub(super) async fn capabilities(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<Capabilities> {
    principal
        .require(common::scopes::TASK_READ)
        .map_err(failure)?;
    let browser = match &state.browser {
        Some(controller) => {
            let settings = controller.settings().await;
            if settings.enabled {
                Some(BrowserCapability {
                    toolset: controller.id().to_string(),
                    enabled: true,
                    ready: controller.readiness().await.to_string(),
                    profile: format!("{:?}", settings.profile).to_ascii_lowercase(),
                    max_sessions: settings.max_sessions,
                    active_sessions: controller.active_count().await,
                    evaluate_js: false,
                    streaming: false,
                    takeover: true,
                    control_protocol: "fenced-lease-v1",
                    control_commands: ["navigate", "click", "type", "extract_text"],
                    terminal_control: false,
                })
            } else {
                None
            }
        }
        None => None,
    };
    Ok(Json(Capabilities {
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
        },
        a2a_durable: state.a2a_durable,
        artifact_storage: "native_adk",
        artifact_transfer: "authenticated_or_scoped_grant",
        artifact_max_bytes: crate::data::artifacts::MAX_ARTIFACT_BYTES,
        capabilities: state.runtime.config().capabilities.clone(),
        installed_capabilities: state.manifest.capabilities.clone(),
        commands: match &state.commands {
            Some(commands) => commands.descriptors().to_vec(),
            None => Vec::new(),
        },
        browser,
    }))
}
