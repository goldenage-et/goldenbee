//! Public liveness route.

use crate::state::SharedState;
use axum::extract::State as AxumState;
use axum::Json;
use common::GatewayHealth;

pub(crate) async fn health(AxumState(state): AxumState<SharedState>) -> Json<GatewayHealth> {
    Json(state.core.health())
}
