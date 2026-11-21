//! Neutral and Worker-defined command discovery and invocation.

use crate::commands;
use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::Extension;
use common::{scopes, ApiError, CommandDescriptor, CommandRequest, CommandResponse, Principal};

fn registry(state: &WorkerState) -> Result<&commands::CommandRegistry, ApiError> {
    state.commands.as_deref().ok_or_else(|| {
        ApiError::new(
            "command.unavailable",
            "this Worker instance exposes no command registry",
        )
    })
}

pub(super) async fn list(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<Vec<CommandDescriptor>> {
    principal.require(scopes::TASK_READ).map_err(failure)?;
    let registry = registry(&state).map_err(failure)?;
    Ok(Json(commands::descriptor_list(registry).await))
}

pub(super) async fn get(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> JsonResult<CommandDescriptor> {
    principal.require(scopes::TASK_READ).map_err(failure)?;
    let registry = registry(&state).map_err(failure)?;
    registry
        .descriptor(&name)
        .cloned()
        .map(Json)
        .ok_or_else(|| {
            failure(ApiError::new(
                "command.not_found",
                format!("command '{name}' is not defined"),
            ))
        })
}

pub(super) async fn invoke(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    Json(request): Json<CommandRequest>,
) -> JsonResult<CommandResponse> {
    principal.require(scopes::TASK_SUBMIT).map_err(failure)?;
    let registry = registry(&state).map_err(failure)?;
    commands::invoke(registry, &state.runtime, &principal, &name, request)
        .await
        .map(Json)
        .map_err(failure)
}
