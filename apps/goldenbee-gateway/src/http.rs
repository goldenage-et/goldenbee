//! HTTP serialization helpers shared by the route modules.

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use common::ApiError;

/// Result type for JSON handlers: failures already serialized as Gateway errors.
pub(crate) type JsonResult<T> = std::result::Result<Json<T>, Response>;

/// Read a caller or Worker bearer token from the request headers.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|token| token.to_string())
}

/// Serialize a control-plane error with its stable status and error code.
pub(crate) fn failure(error: ApiError) -> Response {
    let status = StatusCode::from_u16(error.status()).unwrap_or(StatusCode::BAD_REQUEST);
    (status, Json(error)).into_response()
}
