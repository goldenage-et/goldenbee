//! Caller-authenticated Worker diagnostic export.

use crate::http::{failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, Query, State as AxumState};
use axum::Extension;
use axum::Json;
use common::DiagnosticBundle;
use gateway_core::Caller;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct DiagnosticQuery {
    #[serde(default)]
    task: Option<String>,
}

pub(super) async fn diagnostics(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(query): Query<DiagnosticQuery>,
) -> JsonResult<DiagnosticBundle> {
    state
        .core
        .diagnostics(&caller, &id, query.task.as_deref())
        .await
        .map(Json)
        .map_err(failure)
}
