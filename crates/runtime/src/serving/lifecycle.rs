//! Authorized process lifecycle operations.

use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use common::{scopes, Principal};
use serde::Serialize;

/// Result of one authorized drain request.
#[derive(Debug, Serialize)]
pub(super) struct DrainResponse {
    /// Stable lifecycle phase.
    phase: String,
    /// Whether this process currently accepts new work.
    ready: bool,
    /// `open` or `closed`.
    admission: String,
    /// A drain was already active before this request.
    already_draining: bool,
    /// The bounded completion window expired.
    timed_out: bool,
    /// Remaining execution tokens were cancelled at the deadline.
    forced_cancelled: bool,
    /// Non-terminal work still visible after the bound.
    unfinished: usize,
    /// Current active execution count.
    active: u32,
    /// Current queued execution count.
    queued: u32,
}

pub(super) async fn drain(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<DrainResponse> {
    principal.require(scopes::INSTANCE_ADMIN).map_err(failure)?;
    let started = state.runtime.begin_drain().await;
    let outcome = state.runtime.await_drain().await;
    let health = state.runtime.health().await;
    Ok(Json(DrainResponse {
        phase: health.phase,
        ready: health.ready,
        admission: health.admission,
        already_draining: started.already_draining,
        timed_out: outcome.timed_out,
        forced_cancelled: outcome.forced,
        unfinished: outcome.unfinished,
        active: health.capacity.active,
        queued: health.capacity.queued,
    }))
}
