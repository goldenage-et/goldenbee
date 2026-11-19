//! Root route composition by authentication class.
//!
//! Caller bearer authentication is applied only to the caller-authenticated
//! router. Public health stays open, and Worker control handlers authenticate
//! their own control token.

use crate::state::SharedState;
use crate::{auth, control, fleet, health, profiles, providers, socket, tasks, teams, workers};
use axum::middleware;
use axum::routing::get;
use axum::Router;

pub(crate) fn router(state: SharedState) -> Router {
    let public = Router::new()
        .route("/v1/health", get(health::health))
        .merge(socket::upgrade_route());
    let caller = workers::caller_routes()
        .merge(tasks::routes())
        .merge(teams::routes())
        .merge(profiles::routes())
        .merge(control::routes())
        .merge(fleet::routes())
        .merge(providers::admin_routes())
        .merge(socket::caller_routes())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authorize,
        ));
    public
        .merge(workers::control_routes())
        .merge(providers::control_routes())
        .merge(caller)
        .with_state(state)
}
