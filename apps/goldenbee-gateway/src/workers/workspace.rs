//! Caller-authenticated Worker workspace inspection routes.

use crate::http::{failure, JsonResult};
use crate::state::SharedState;
use axum::body::Body;
use axum::extract::{Path, Query, State as AxumState};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Json, Response};
use axum::Extension;
use common::{WorkspaceFile, WorkspaceTreePage};
use futures::StreamExt;
use gateway_core::Caller;
use serde::Deserialize;

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
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(query): Query<TreeQuery>,
) -> JsonResult<WorkspaceTreePage> {
    state
        .core
        .workspace_tree(
            &caller,
            &id,
            &query.path,
            query.limit.unwrap_or(500),
            query.cursor.as_deref(),
        )
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn file(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
) -> JsonResult<WorkspaceFile> {
    state
        .core
        .workspace_file(&caller, &id, &query.path)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn content(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Response, Response> {
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let content = state
        .core
        .workspace_content(&caller, &id, &query.path, range.as_deref())
        .await
        .map_err(failure)?;
    let body = Body::from_stream(
        content
            .stream
            .map(|item| item.map_err(|error| std::io::Error::other(error.message))),
    );
    let mut response = Response::new(body);
    *response.status_mut() =
        StatusCode::from_u16(content.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let output = response.headers_mut();
    output.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    if let Some(length) = content.content_length {
        if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
            output.insert(header::CONTENT_LENGTH, value);
        }
    }
    if let Some(range) = &content.content_range {
        if let Ok(value) = HeaderValue::from_str(range) {
            output.insert(header::CONTENT_RANGE, value);
        }
    }
    output.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    output.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    output.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(digest) = content.digest {
        if let Ok(value) = HeaderValue::from_str(&digest) {
            output.insert("x-goldenbee-workspace-digest", value);
        }
    }
    Ok(response)
}

pub(super) async fn events(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let stream = state
        .core
        .workspace_events(&caller, &id)
        .await
        .map_err(failure)?;
    let body = Body::from_stream(
        stream.map(|item| item.map_err(|error| std::io::Error::other(error.message))),
    );
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}
