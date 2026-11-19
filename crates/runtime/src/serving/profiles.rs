//! Worker-owned Runtime Profile revision management.
//!
//! These routes expose the owning Worker's existing immutable revision store.
//! They do not load package files, revalidate assets in a second loader, or
//! activate a revision from a Client. Gateway delegation scopes are checked by
//! the runtime before any read or mutation.

use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::Extension;
use common::{
    CreateProfileRequest, DeletedProfile, Principal, Profile, ProfileVersionRequest,
    ReviseProfileRequest,
};

pub(super) async fn list(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<Vec<Profile>> {
    state
        .runtime
        .profiles(&principal, &principal.namespace)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn profile(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> JsonResult<Profile> {
    state
        .runtime
        .profile(&principal, &principal.namespace, &name)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn create(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<CreateProfileRequest>,
) -> JsonResult<Profile> {
    state
        .runtime
        .create_profile(
            &principal,
            &principal.namespace,
            &request.name,
            request.revision,
        )
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn revise(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    Json(request): Json<ReviseProfileRequest>,
) -> JsonResult<Profile> {
    state
        .runtime
        .revise_profile(
            &principal,
            &principal.namespace,
            &name,
            request.expected_revision,
            request.revision,
        )
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn validate(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    body: Option<Json<ProfileVersionRequest>>,
) -> JsonResult<Profile> {
    let request = body.map(|Json(body)| body).unwrap_or_default();
    state
        .runtime
        .validate_profile(&principal, &principal.namespace, &name, request.revision)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn activate(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    Json(request): Json<ProfileVersionRequest>,
) -> JsonResult<Profile> {
    let Some(revision) = request.revision else {
        return Err(failure(common::ApiError::new(
            "profile.revision_required",
            "profile activation requires an exact revision",
        )));
    };
    state
        .runtime
        .activate_profile(&principal, &principal.namespace, &name, revision)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn deactivate(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> JsonResult<Profile> {
    state
        .runtime
        .deactivate_profile(&principal, &principal.namespace, &name)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn delete(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> JsonResult<DeletedProfile> {
    state
        .runtime
        .delete_profile(&principal, &principal.namespace, &name)
        .await
        .map(|()| Json(DeletedProfile { deleted: name }))
        .map_err(failure)
}
