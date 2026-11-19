//! Caller bearer authentication for the caller-authenticated route class.
//!
//! Worker control routes are mounted outside this middleware. Their handlers
//! authenticate the Worker's own control token, so the Gateway never accepts
//! caller identity for control traffic.

use crate::http::{bearer, failure};
use crate::state::SharedState;
use axum::extract::{Request, State as AxumState};
use axum::middleware::Next;
use axum::response::Response;
use common::ApiError;

pub(crate) async fn authorize(
    AxumState(state): AxumState<SharedState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = bearer(request.headers()) else {
        return failure(ApiError::new(
            "auth.unauthenticated",
            "a bearer token is required",
        ));
    };
    match state.callers.get(&token) {
        Some(caller) => {
            request.extensions_mut().insert(caller.clone());
            next.run(request).await
        }
        None => failure(ApiError::new(
            "auth.unauthenticated",
            "token is not recognized by this Gateway",
        )),
    }
}
