//! Worker route registration by authentication class.
//!
//! Control traffic authenticates the Worker's own token inside the handler and
//! is mounted outside caller authentication. Discovery, audit, and admin
//! routes require an authenticated caller.

mod admin;
mod diagnostics;
mod handlers;
mod types;
mod workspace;

use crate::state::SharedState;
use axum::routing::{delete, get, post};
use axum::Router;

/// Worker control routes. The handler authenticates the bound control token.
pub(crate) fn control_routes() -> Router<SharedState> {
    Router::new()
        .route("/v1/workers/register", post(handlers::register_worker))
        .route("/v1/workers/{id}/lease", post(handlers::renew_worker))
}

/// Caller-authenticated Worker discovery, audit, and administration.
pub(crate) fn caller_routes() -> Router<SharedState> {
    Router::new()
        .route("/v1/workers", get(handlers::workers))
        .route("/v1/workers/audit", get(handlers::audit))
        .route("/v1/workers/{id}", delete(handlers::decommission))
        .route(
            "/v1/workers/{id}/diagnostics",
            get(diagnostics::diagnostics),
        )
        .route("/v1/workers/{id}/workspace/tree", get(workspace::tree))
        .route("/v1/workers/{id}/workspace/file", get(workspace::file))
        .route(
            "/v1/workers/{id}/workspace/events/stream",
            get(workspace::events),
        )
        .merge(admin::routes())
}
