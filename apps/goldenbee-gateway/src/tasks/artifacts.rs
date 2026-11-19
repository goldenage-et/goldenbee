//! Artifact metadata list and bounded content relay through the Gateway.

use crate::http::failure;
use crate::state::SharedState;
use axum::body::Body;
use axum::extract::{Path, State as AxumState};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Json, Response};
use axum::Extension;
use futures::StreamExt;
use gateway_core::{ArtifactContent, Caller};

pub(super) async fn list(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<Vec<common::Artifact>>, Response> {
    state
        .core
        .artifacts(&caller, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn metadata(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((id, artifact)): Path<(String, String)>,
) -> Result<Json<common::Artifact>, Response> {
    state
        .core
        .artifact(&caller, &id, &artifact)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn content(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path((id, artifact)): Path<(String, String)>,
) -> Result<Response, Response> {
    state
        .core
        .artifact_content(&caller, &id, &artifact)
        .await
        .map(transfer)
        .map_err(failure)
}

/// Forward Worker chunks unchanged. Dropping the response closes the upstream
/// transfer and releases the Gateway permit; it never cancels the task.
fn transfer(content: ArtifactContent) -> Response {
    let ArtifactContent { artifact, content } = content;
    let body = Body::from_stream(
        content.map(|item| item.map_err(|error| std::io::Error::other(error.message))),
    );
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    let content_type = HeaderValue::from_str(&artifact.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CONTENT_TYPE, content_type);
    if let Ok(length) = HeaderValue::from_str(&artifact.size.to_string()) {
        headers.insert(header::CONTENT_LENGTH, length);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(digest) = HeaderValue::from_str(&artifact.digest) {
        headers.insert("x-goldenbee-artifact-digest", digest);
    }
    if let Ok(id) = HeaderValue::from_str(&artifact.id) {
        headers.insert("x-goldenbee-artifact-id", id);
    }
    response
}
