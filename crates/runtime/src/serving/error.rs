//! Stable redacted HTTP error responses.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use common::ApiError;

/// A redacted JSON error response.
pub fn failure(error: ApiError) -> Response {
    let status = StatusCode::from_u16(error.status()).unwrap_or(StatusCode::BAD_REQUEST);
    (status, Json(error)).into_response()
}

pub(super) type JsonResult<T> = std::result::Result<Json<T>, Response>;
