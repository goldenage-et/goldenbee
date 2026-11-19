//! Runtime Profile list, revision, and activation handlers.

use crate::http::{failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, State as AxumState};
use axum::{Extension, Json};
use common::{
    CreateProfileRequest, DeletedProfile, Profile, ProfileVersionRequest, ReviseProfileRequest,
};
use gateway_core::Caller;

pub(super) async fn profiles(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(worker): Path<String>,
) -> JsonResult<Vec<Profile>> {
    state
        .core
        .profiles(&caller, &worker)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn profile(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, name)): Path<(String, String)>,
) -> JsonResult<Profile> {
    state
        .core
        .profile(&caller, &worker, &name)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn create(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(worker): Path<String>,
    Json(request): Json<CreateProfileRequest>,
) -> JsonResult<Profile> {
    state
        .core
        .create_profile(&caller, &worker, &request)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn revise(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, name)): Path<(String, String)>,
    Json(request): Json<ReviseProfileRequest>,
) -> JsonResult<Profile> {
    state
        .core
        .revise_profile(&caller, &worker, &name, &request)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn validate(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, name)): Path<(String, String)>,
    body: Option<Json<ProfileVersionRequest>>,
) -> JsonResult<Profile> {
    let request = body.map(|Json(body)| body).unwrap_or_default();
    state
        .core
        .validate_profile(&caller, &worker, &name, &request)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn activate(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, name)): Path<(String, String)>,
    body: Json<ProfileVersionRequest>,
) -> JsonResult<Profile> {
    state
        .core
        .activate_profile(&caller, &worker, &name, &body)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn deactivate(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, name)): Path<(String, String)>,
) -> JsonResult<Profile> {
    state
        .core
        .deactivate_profile(&caller, &worker, &name)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn delete(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((worker, name)): Path<(String, String)>,
) -> JsonResult<DeletedProfile> {
    state
        .core
        .delete_profile(&caller, &worker, &name)
        .await
        .map(Json)
        .map_err(failure)
}
