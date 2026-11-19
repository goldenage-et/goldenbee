//! Artifact metadata, grants, and bounded transfers.

use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use crate::ArtifactContent;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Json, Response};
use axum::Extension;
use common::{Artifact, ArtifactGrant, Principal};
use serde::Deserialize;

pub(super) async fn task_artifacts(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<Vec<Artifact>> {
    state
        .runtime
        .artifacts(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn artifact_metadata(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<Artifact> {
    state
        .runtime
        .artifact(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn artifact_content(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> std::result::Result<Response, Response> {
    state
        .runtime
        .read_artifact(&principal, &id)
        .await
        .map(transfer)
        .map_err(failure)
}

#[derive(Debug, Deserialize)]
pub(super) struct GrantBody {
    #[serde(default)]
    ttl_ms: Option<i64>,
}

pub(super) async fn artifact_grant(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Option<Json<GrantBody>>,
) -> JsonResult<ArtifactGrant> {
    let ttl_ms = body
        .and_then(|Json(body)| body.ttl_ms)
        .unwrap_or(60_000)
        .clamp(1, 300_000);
    state
        .runtime
        .grant_artifact(&principal, &id, ttl_ms)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn grant_content(
    State(state): State<WorkerState>,
    Path(grant): Path<String>,
) -> std::result::Result<Response, Response> {
    state
        .runtime
        .read_grant(&grant)
        .await
        .map(transfer)
        .map_err(failure)
}

/// Stream original bytes in bounded chunks. Bytes never enter the event stream,
/// and dropping the response body stops further transfer work.
fn transfer(content: ArtifactContent) -> Response {
    let ArtifactContent { artifact, bytes } = content;
    let length = bytes.len();
    let content_type = HeaderValue::from_str(&artifact.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let digest = HeaderValue::from_str(&artifact.digest)
        .unwrap_or_else(|_| HeaderValue::from_static("sha256:unavailable"));
    let chunks = futures::stream::unfold((bytes, 0usize), |(bytes, offset)| async move {
        const CHUNK: usize = 64 * 1024;
        if offset >= bytes.len() {
            return None;
        }
        let end = (offset + CHUNK).min(bytes.len());
        let chunk = Bytes::copy_from_slice(&bytes[offset..end]);
        Some((Ok::<Bytes, std::io::Error>(chunk), (bytes, end)))
    });
    let mut response = Response::new(Body::from_stream(chunks));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, content_type);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("x-goldenbee-artifact-digest", digest);
    if let Ok(id) = HeaderValue::from_str(&artifact.id) {
        headers.insert("x-goldenbee-artifact-id", id);
    }
    response
}
