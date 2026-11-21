//! One-shot headless run route.
//!
//! This is the app-server projection of the same durable task admission used by
//! `/v1/tasks` and the local CLI. It adds no executor, store, or event bus, and
//! a delegation keeps its capability and limit narrowing.

use crate::core::crypto::DelegationClaims;
use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::State;
use axum::response::Json;
use axum::Extension;
use common::{Principal, RunRequest, RunResult};

pub(super) async fn run(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    claims: Option<Extension<DelegationClaims>>,
    Json(mut request): Json<RunRequest>,
) -> JsonResult<RunResult> {
    if request.agent.is_none() {
        request.agent = state.default_agent.clone();
    }
    let result = state
        .runtime
        .run_headless_with_claims(
            &principal,
            request,
            claims.as_ref().map(|claims| &claims.0),
            &mut |_| Ok(()),
        )
        .await
        .map_err(failure)?;
    Ok(Json(result))
}
