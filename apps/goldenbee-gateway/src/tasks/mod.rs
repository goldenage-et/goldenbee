//! Caller-authenticated task route registration.

mod approvals;
mod artifacts;
mod handlers;
mod stream;

use crate::state::SharedState;
use axum::routing::{get, post};
use axum::Router;

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/v1/tasks", post(handlers::submit))
        .route("/v1/tasks/{id}", get(handlers::task))
        .route("/v1/tasks/{id}/events", get(handlers::events))
        .route("/v1/tasks/{id}/events/stream", get(stream::stream_events))
        .route("/v1/tasks/{id}/approvals", get(approvals::list))
        .route("/v1/tasks/{id}/approvals/{approval}", get(approvals::get))
        .route(
            "/v1/tasks/{id}/approvals/{approval}/decision",
            post(approvals::decide),
        )
        .route(
            "/v1/tasks/{id}/approvals/{approval}/revoke",
            post(approvals::revoke),
        )
        .route("/v1/tasks/{id}/artifacts", get(artifacts::list))
        .route(
            "/v1/tasks/{id}/artifacts/{artifact}",
            get(artifacts::metadata),
        )
        .route(
            "/v1/tasks/{id}/artifacts/{artifact}/content",
            get(artifacts::content),
        )
        .route("/v1/tasks/{id}/cancel", post(handlers::cancel))
}
