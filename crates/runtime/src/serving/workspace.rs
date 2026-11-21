//! Read-only Worker workspace tree and bounded file preview routes.

use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Json, Response};
use axum::Extension;
use common::{scopes, ApiError, Principal, WorkspaceFile, WorkspaceTreePage};
use futures::stream::{self, Stream};
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub(super) struct TreeQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct FileQuery {
    path: String,
}

pub(super) async fn tree(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<TreeQuery>,
) -> JsonResult<WorkspaceTreePage> {
    principal.require(scopes::WORKSPACE_READ).map_err(failure)?;
    let service = state.workspace.ok_or_else(workspace_disabled)?;
    let page = tokio::task::spawn_blocking(move || {
        service.tree(
            &query.path,
            query.limit.unwrap_or(500),
            query.cursor.as_deref(),
        )
    })
    .await
    .map_err(|_| {
        failure(ApiError::new(
            "workspace.unavailable",
            "the workspace listing task did not complete",
        ))
    })?
    .map_err(failure)?;
    Ok(Json(page))
}

pub(super) async fn file(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<FileQuery>,
) -> JsonResult<WorkspaceFile> {
    principal.require(scopes::WORKSPACE_READ).map_err(failure)?;
    let service = state.workspace.ok_or_else(workspace_disabled)?;
    let file = tokio::task::spawn_blocking(move || service.file(&query.path))
        .await
        .map_err(|_| {
            failure(ApiError::new(
                "workspace.unavailable",
                "the workspace read task did not complete",
            ))
        })?
        .map_err(failure)?;
    Ok(Json(file))
}

pub(super) async fn events(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    principal.require(scopes::WORKSPACE_READ).map_err(failure)?;
    let service = state.workspace.ok_or_else(workspace_disabled)?;
    let watch = service.watch().map_err(failure)?;
    let source = "worker".to_string();
    let incarnation = state.runtime.config().incarnation.clone();
    let stream_id = "workspace".to_string();
    // The watcher is best-effort and not replayable. The browser debounces
    // batches and refetches the affected directories; a reconnect refetches the
    // whole visible tree.
    let stream = stream::unfold((watch, 1_u64), move |(mut watch, seq)| {
        let source = source.clone();
        let incarnation = incarnation.clone();
        let stream_id = stream_id.clone();
        async move {
            let paths = watch.recv().await?;
            let payload = serde_json::json!({
                "paths": paths,
                "at": common::now_ms(),
            });
            let event = serde_json::json!({
                "event_id": format!("workspace-{seq}"),
                "source": source,
                "incarnation": incarnation,
                "stream_id": stream_id,
                "seq": seq,
                "schema_version": 1,
                "kind": "workspace.changed",
                "durable": false,
                "payload": payload,
                "at": common::now_ms(),
            });
            let event = Event::default()
                .event("workspace.changed")
                .data(event.to_string());
            Some((Ok(event), (watch, seq + 1)))
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

fn workspace_disabled() -> axum::response::Response {
    failure(ApiError::new(
        "workspace.disabled",
        "workspace browsing is disabled for this Worker",
    ))
}
