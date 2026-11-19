//! Caller-authenticated Runtime Profile proxy routes.
//!
//! Every route names the owning Worker. Gateway checks caller scope and Worker
//! visibility, then forwards the typed revision operation through a scoped
//! delegation token. It does not store profile state.

mod handlers;

use crate::state::SharedState;
use axum::routing::{get, post};
use axum::Router;

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route(
            "/v1/workers/{worker}/profiles",
            get(handlers::profiles).post(handlers::create),
        )
        .route(
            "/v1/workers/{worker}/profiles/{name}",
            get(handlers::profile)
                .put(handlers::revise)
                .delete(handlers::delete),
        )
        .route(
            "/v1/workers/{worker}/profiles/{name}/validate",
            post(handlers::validate),
        )
        .route(
            "/v1/workers/{worker}/profiles/{name}/activate",
            post(handlers::activate),
        )
        .route(
            "/v1/workers/{worker}/profiles/{name}/deactivate",
            post(handlers::deactivate),
        )
}
