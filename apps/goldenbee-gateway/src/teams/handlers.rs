//! Team list, lifecycle, and optimistic update handlers.

use crate::http::{failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, State as AxumState};
use axum::http::HeaderMap;
use axum::{Extension, Json};
use common::ApiError;
use common::{CreateTeamRequest, Team, TeamVersionRequest, UpdateTeamRequest};
use gateway_core::Caller;

pub(super) async fn teams(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
) -> JsonResult<Vec<Team>> {
    state.core.teams(&caller).map(Json).map_err(failure)
}

pub(super) async fn create(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Json(request): Json<CreateTeamRequest>,
) -> JsonResult<Team> {
    state
        .core
        .create_team(&caller, &request.name, request.team)
        .map(Json)
        .map_err(failure)
}

pub(super) async fn team(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<Team> {
    state.core.team(&caller, &id).map(Json).map_err(failure)
}

pub(super) async fn update(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<UpdateTeamRequest>,
) -> JsonResult<Team> {
    let expected_version = match (request.expected_version, if_match(&headers)) {
        (0, Some(version)) => version,
        (0, None) => {
            return Err(failure(ApiError::new(
                "team.version_required",
                "expected_version or an If-Match header is required",
            )))
        }
        (version, _) => version,
    };
    state
        .core
        .update_team(&caller, &id, expected_version, request.team)
        .map(Json)
        .map_err(failure)
}

fn if_match(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get(axum::http::header::IF_MATCH)?.to_str().ok()?;
    value
        .trim()
        .trim_start_matches("W/")
        .trim_matches('"')
        .parse()
        .ok()
}

pub(super) async fn validate(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    body: Option<Json<TeamVersionRequest>>,
) -> JsonResult<Team> {
    let request = body.map(|Json(body)| body).unwrap_or_default();
    state
        .core
        .validate_team(&caller, &id, &request)
        .map(Json)
        .map_err(failure)
}

pub(super) async fn activate(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    body: Option<Json<TeamVersionRequest>>,
) -> JsonResult<Team> {
    let request = body.map(|Json(body)| body).unwrap_or_default();
    state
        .core
        .activate_team(&caller, &id, &request)
        .map(Json)
        .map_err(failure)
}

pub(super) async fn deactivate(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    body: Option<Json<TeamVersionRequest>>,
) -> JsonResult<Team> {
    let request = body.map(|Json(body)| body).unwrap_or_default();
    state
        .core
        .deactivate_team(&caller, &id, &request)
        .map(Json)
        .map_err(failure)
}
