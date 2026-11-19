//! Operator diagnostic exporter. It composes existing evidence only.

use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Query, State};
use axum::response::Json;
use axum::Extension;
use common::{DiagnosticBundle, Principal};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(super) struct DiagnosticQuery {
    #[serde(default)]
    task: Option<String>,
}

/// Export one redacted diagnostic bundle for this Worker Instance.
pub(super) async fn diagnostics(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<DiagnosticQuery>,
) -> JsonResult<DiagnosticBundle> {
    state
        .runtime
        .diagnostics(&principal, query.task.as_deref())
        .await
        .map(Json)
        .map_err(failure)
}
