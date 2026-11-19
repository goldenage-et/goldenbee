//! Caller-authenticated team roster routes.

mod handlers;

use crate::state::SharedState;
use axum::routing::{get, post};
use axum::Router;

pub(crate) fn routes() -> Router<SharedState> {
    Router::new()
        .route("/v1/teams", get(handlers::teams).post(handlers::create))
        .route("/v1/teams/{id}", get(handlers::team).put(handlers::update))
        .route("/v1/teams/{id}/validate", post(handlers::validate))
        .route("/v1/teams/{id}/activate", post(handlers::activate))
        .route("/v1/teams/{id}/deactivate", post(handlers::deactivate))
}
