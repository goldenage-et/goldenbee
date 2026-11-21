//! MCP server health, resource, and prompt operations.
//!
//! These routes expose the same registry the model-facing toolsets use. A
//! caller with `operate` sees configured state and can read only the resources
//! and prompts the operator allowlisted; a profile or plugin never widens that
//! ceiling.

use crate::mcp::{McpRegistry, RemoteMcpRegistry};
use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::Extension;
use common::{
    scopes, ApiError, McpCapabilityInventory, McpPromptContent, McpPromptDescriptor,
    McpPromptRequest, McpResourceContent, McpResourceDescriptor, McpResourceReadRequest,
    McpServerInventory, Principal,
};
use std::sync::Arc;

enum Target<'a> {
    Local(&'a Arc<McpRegistry>),
    Remote(&'a Arc<RemoteMcpRegistry>),
}

async fn target<'a>(state: &'a WorkerState, server: &str) -> Result<Target<'a>, ApiError> {
    if let Some(local) = &state.mcp {
        if local.known(server).await {
            return Ok(Target::Local(local));
        }
    }
    if let Some(remote) = &state.remote_mcp {
        if remote.known(server) {
            return Ok(Target::Remote(remote));
        }
    }
    Err(ApiError::new(
        "mcp.connection_unknown",
        format!("MCP connection '{server}' is not configured"),
    ))
}

async fn inventory(state: &WorkerState) -> Vec<McpServerInventory> {
    let mut servers: Vec<McpServerInventory> = Vec::new();
    if let Some(local) = &state.mcp {
        servers.extend(local.inventory().await);
    }
    if let Some(remote) = &state.remote_mcp {
        servers.extend(remote.inventory().await);
    }
    servers.sort_by(|left, right| left.id.cmp(&right.id));
    servers
}

pub(super) async fn servers(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<McpCapabilityInventory> {
    principal.require(scopes::OPERATE).map_err(failure)?;
    Ok(Json(McpCapabilityInventory {
        servers: inventory(&state).await,
    }))
}

pub(super) async fn restart(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(server): Path<String>,
) -> JsonResult<McpServerInventory> {
    principal.require(scopes::OPERATE).map_err(failure)?;
    let outcome = match target(&state, &server).await.map_err(failure)? {
        Target::Local(local) => local.restart(&server).await,
        Target::Remote(remote) => remote.restart(&server).await,
    };
    if let Err(error) = &outcome {
        let _ = state
            .runtime
            .audit(
                &principal.id,
                "mcp.restart",
                &server,
                "failed",
                &format!("code={}", error.code),
            )
            .await;
        return Err(failure(outcome.expect_err("checked failure")));
    }
    let _ = state
        .runtime
        .audit(
            &principal.id,
            "mcp.restart",
            &server,
            "restarted",
            "operator-requested",
        )
        .await;
    inventory(&state)
        .await
        .into_iter()
        .find(|entry| entry.id == server)
        .map(Json)
        .ok_or_else(|| {
            failure(ApiError::new(
                "mcp.connection_unknown",
                format!("MCP connection '{server}' is not configured"),
            ))
        })
}

pub(super) async fn resources(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(server): Path<String>,
) -> JsonResult<Vec<McpResourceDescriptor>> {
    principal.require(scopes::OPERATE).map_err(failure)?;
    let resources = match target(&state, &server).await.map_err(failure)? {
        Target::Local(local) => local.resources(&server).await,
        Target::Remote(remote) => remote.resources(&server).await,
    };
    resources.map(Json).map_err(failure)
}

pub(super) async fn read_resource(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(server): Path<String>,
    Json(request): Json<McpResourceReadRequest>,
) -> JsonResult<McpResourceContent> {
    principal.require(scopes::OPERATE).map_err(failure)?;
    let content = match target(&state, &server).await.map_err(failure)? {
        Target::Local(local) => local.read_resource(&server, &request.uri).await,
        Target::Remote(remote) => remote.read_resource(&server, &request.uri).await,
    };
    content.map(Json).map_err(failure)
}

pub(super) async fn prompts(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(server): Path<String>,
) -> JsonResult<Vec<McpPromptDescriptor>> {
    principal.require(scopes::OPERATE).map_err(failure)?;
    let prompts = match target(&state, &server).await.map_err(failure)? {
        Target::Local(local) => local.prompts(&server).await,
        Target::Remote(remote) => remote.prompts(&server).await,
    };
    prompts.map(Json).map_err(failure)
}

pub(super) async fn prompt(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path((server, name)): Path<(String, String)>,
    Json(request): Json<McpPromptRequest>,
) -> JsonResult<McpPromptContent> {
    principal.require(scopes::OPERATE).map_err(failure)?;
    let arguments = request
        .arguments
        .into_iter()
        .map(|(key, value)| (key, serde_json::Value::String(value)))
        .collect();
    let content = match target(&state, &server).await.map_err(failure)? {
        Target::Local(local) => local.prompt(&server, &name, arguments).await,
        Target::Remote(remote) => remote.prompt(&server, &name, arguments).await,
    };
    content.map(Json).map_err(failure)
}
