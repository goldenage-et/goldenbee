//! Authenticated Worker serving for Gateway and authorized native drivers.
//!
//! Native ADK serving remains upstream. This module adds only the harness
//! management routes required for durable tasks, events, and truthful status.

// Axum responses are large by design; handlers return them directly rather than
// boxing every error path.
#![allow(clippy::result_large_err)]

pub mod a2a;
mod approvals;
mod artifacts;
mod auth;
mod checkpoints;
mod commands;
mod control;
mod diagnostics;
mod error;
mod health;
mod lifecycle;
mod mcp;
mod peers;
mod plans;
mod profiles;
mod retention;
mod runs;
mod sessions;
mod tasks;

pub use a2a::A2aServing;
pub use error::failure;

use crate::agents::PackageManifest;
use crate::browser::BrowserController;
use crate::Runtime;
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

/// Shared state for the Worker management routes and their authentication
/// middleware.
#[derive(Clone)]
pub struct WorkerState {
    /// Durable Worker runtime.
    pub runtime: Runtime,
    /// Installed package manifest.
    pub manifest: PackageManifest,
    /// Whether the per-instance durable A2A task store is open.
    pub a2a_durable: bool,
    /// Optional package-declared browser capability.
    pub browser: Option<Arc<BrowserController>>,
    /// Active command registry for this asset revision.
    pub(crate) commands: Option<Arc<crate::commands::CommandRegistry>>,
    /// Configured default agent for a run that omits one.
    pub default_agent: Option<String>,
    /// Trusted plugin bundles active for this revision.
    pub plugins: Vec<crate::plugins::PluginView>,
    /// Instance-owned local MCP connection registry.
    pub mcp: Option<Arc<crate::mcp::McpRegistry>>,
    /// Instance-owned remote MCP connection registry.
    pub remote_mcp: Option<Arc<crate::mcp::RemoteMcpRegistry>>,
}

impl WorkerState {
    /// Build serving state.
    pub fn new(runtime: Runtime, manifest: PackageManifest) -> Self {
        Self {
            runtime,
            manifest,
            a2a_durable: false,
            browser: None,
            commands: None,
            default_agent: None,
            plugins: Vec::new(),
            mcp: None,
            remote_mcp: None,
        }
    }

    /// Record that the durable A2A store opened successfully.
    pub fn with_durable_a2a(mut self, durable: bool) -> Self {
        self.a2a_durable = durable;
        self
    }

    /// Attach the instance-owned browser capability when the package declares it.
    pub fn with_browser(mut self, browser: Option<Arc<BrowserController>>) -> Self {
        self.browser = browser;
        self
    }

    /// Attach the active command registry for this asset revision.
    pub(crate) fn with_commands(mut self, commands: Arc<crate::commands::CommandRegistry>) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Attach the configured default agent for agent-omitted headless runs.
    pub fn with_default_agent(mut self, agent: impl Into<String>) -> Self {
        self.default_agent = Some(agent.into());
        self
    }

    /// Attach the trusted plugin inventory for this asset revision.
    pub fn with_plugins(mut self, plugins: Vec<crate::plugins::PluginView>) -> Self {
        self.plugins = plugins;
        self
    }

    /// Attach the instance-owned MCP connection registries.
    pub fn with_mcp(
        mut self,
        local: Option<Arc<crate::mcp::McpRegistry>>,
        remote: Option<Arc<crate::mcp::RemoteMcpRegistry>>,
    ) -> Self {
        self.mcp = local;
        self.remote_mcp = remote;
        self
    }
}

/// Harness management routes. Merge these with the native ADK router, then
/// apply [`authenticate`].
pub fn api_router(state: WorkerState) -> Router {
    Router::new()
        .route("/v1/health", get(health::liveness))
        .route("/v1/status", get(health::status))
        .route("/v1/diagnostics", get(diagnostics::diagnostics))
        .route("/v1/capabilities", get(health::capabilities))
        .route("/v1/commands", get(commands::list))
        .route("/v1/mcp/servers", get(mcp::servers))
        .route("/v1/mcp/servers/{server}/restart", post(mcp::restart))
        .route("/v1/mcp/servers/{server}/resources", get(mcp::resources))
        .route(
            "/v1/mcp/servers/{server}/resources/read",
            post(mcp::read_resource),
        )
        .route("/v1/mcp/servers/{server}/prompts", get(mcp::prompts))
        .route(
            "/v1/mcp/servers/{server}/prompts/{prompt}",
            post(mcp::prompt),
        )
        .route(
            "/v1/commands/{name}",
            get(commands::get).post(commands::invoke),
        )
        .route("/v1/drain", post(lifecycle::drain))
        .route("/v1/tasks", get(tasks::list).post(tasks::submit))
        .route("/v1/runs", post(runs::run))
        .route("/v1/tasks/{id}", get(tasks::task))
        .route("/v1/tasks/{id}/operations", get(tasks::operations))
        .route("/v1/tasks/{id}/audit", get(tasks::audit))
        .route("/v1/tasks/{id}/events", get(tasks::events))
        .route("/v1/tasks/{id}/events/stream", get(tasks::stream))
        .route("/v1/tasks/{id}/artifacts", get(artifacts::task_artifacts))
        .route("/v1/tasks/{id}/cancel", post(tasks::cancel))
        .route("/v1/tasks/{id}/resume", post(tasks::resume))
        .route(
            "/v1/tasks/{id}/checkpoints",
            get(checkpoints::task_checkpoints),
        )
        .route("/v1/checkpoints/{checkpoint}", get(checkpoints::checkpoint))
        .route("/v1/checkpoints/{checkpoint}/diff", get(checkpoints::diff))
        .route(
            "/v1/checkpoints/{checkpoint}/restore",
            post(checkpoints::restore),
        )
        .route("/v1/approvals", get(approvals::list))
        .route("/v1/approvals/{id}", get(approvals::get))
        .route("/v1/approvals/{id}/decision", post(approvals::decide))
        .route("/v1/approvals/{id}/revoke", post(approvals::revoke))
        .route("/v1/profiles", get(profiles::list).post(profiles::create))
        .route(
            "/v1/profiles/{name}",
            get(profiles::profile)
                .put(profiles::revise)
                .delete(profiles::delete),
        )
        .route("/v1/profiles/{name}/validate", post(profiles::validate))
        .route("/v1/profiles/{name}/activate", post(profiles::activate))
        .route("/v1/profiles/{name}/deactivate", post(profiles::deactivate))
        .route("/v1/peers", get(peers::peers))
        .route("/v1/peers/{peer}", get(peers::peer))
        .route("/v1/peers/{peer}/resolve", post(peers::resolve))
        .route("/v1/peers/{peer}/delegate", post(peers::delegate))
        .route(
            "/v1/teams/{team}/members/{member}/delegate",
            post(peers::delegate_team_member),
        )
        .route("/v1/delegations", get(peers::delegations))
        .route("/v1/delegations/{id}", get(peers::delegation))
        .route("/v1/delegations/{id}/recover", post(peers::recover))
        .route("/v1/delegations/{id}/cancel", post(peers::cancel))
        .route("/v1/plans", get(plans::list).post(plans::create))
        .route("/v1/plans/{id}", get(plans::plan))
        .route("/v1/plans/{id}/dispatch", post(plans::dispatch))
        .route("/v1/plans/{id}/recover", post(plans::recover))
        .route("/v1/plans/{id}/evaluate", post(plans::evaluate))
        .route("/v1/plans/{id}/cancel", post(plans::cancel))
        .route("/v1/plans/{id}/escalate", post(plans::escalate))
        .route(
            "/v1/sessions/{session}/control",
            get(control::status).post(control::acquire),
        )
        .route("/v1/sessions", get(sessions::list))
        .route("/v1/sessions/latest", get(sessions::latest))
        .route("/v1/sessions/{session}", get(sessions::session))
        .route("/v1/sessions/{session}/fork", post(sessions::fork))
        .route("/v1/sessions/{session}/rename", post(sessions::rename))
        .route("/v1/sessions/{session}/compact", post(sessions::compact))
        .route(
            "/v1/sessions/{session}/transcript",
            get(sessions::transcript).post(sessions::export),
        )
        .route(
            "/v1/sessions/{session}/control/commands",
            post(control::command),
        )
        .route(
            "/v1/sessions/{session}/control/handback",
            post(control::hand_back),
        )
        .route("/v1/sessions/{session}/control/audit", get(control::audit))
        .route("/v1/artifacts/{id}", get(artifacts::artifact_metadata))
        .route(
            "/v1/artifacts/{id}/content",
            get(artifacts::artifact_content),
        )
        .route("/v1/artifacts/{id}/grant", post(artifacts::artifact_grant))
        .route("/v1/artifact-grants/{grant}", get(artifacts::grant_content))
        .route("/v1/retention", post(retention::apply))
        .route("/v1/principals/{principal}/erase", post(retention::erase))
        .with_state(state)
}

/// Wrap a merged router with bearer authentication and stable redacted errors.
pub fn authenticate(app: Router, state: WorkerState) -> Router {
    app.layer(axum::middleware::from_fn_with_state(state, auth::authorize))
}
