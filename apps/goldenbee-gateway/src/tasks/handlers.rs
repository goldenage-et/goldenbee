//! Task submission, projection, retained events, and cancellation.

use crate::http::{failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, RawQuery, State as AxumState};
use axum::{Extension, Json};
use common::{
    GatewaySubmitRequest, GatewaySubmitResponse, GatewayTask, StreamPage, MAX_EVENT_PAGE,
};
use gateway_core::Caller;

pub(super) async fn submit(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    headers: axum::http::HeaderMap,
    Json(request): Json<GatewaySubmitRequest>,
) -> JsonResult<GatewaySubmitResponse> {
    let trace = headers
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .and_then(common::traceparent_id)
        .or_else(|| {
            headers
                .get("x-goldenbee-trace-id")
                .and_then(|value| value.to_str().ok())
                .map(|value| common::trace_id(Some(value)))
        });
    state
        .core
        .submit_with_trace(&caller, request, trace.as_deref())
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn task(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<GatewayTask> {
    state
        .core
        .task(&caller, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn events(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> JsonResult<StreamPage> {
    let after = common::parse_cursor(raw.as_deref()).map_err(failure)?;
    let limit = common::parse_limit(raw.as_deref())
        .map_err(failure)?
        .unwrap_or(500)
        .clamp(1, MAX_EVENT_PAGE);
    state
        .core
        .events(&caller, &id, after, limit)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn cancel(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<GatewayTask> {
    state
        .core
        .cancel(&caller, &id)
        .await
        .map(Json)
        .map_err(failure)
}
