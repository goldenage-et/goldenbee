//! Worker-native static peer management and delegation routes.
//!
//! These routes are a native operator/delegation surface, not the product
//! Client path. They drive the pinned A2A protocol through the runtime peer
//! adapter and never create a second task engine.

use crate::core::crypto::DelegationClaims;
use crate::peer::{DelegateRequest, PeerDelegation, PeerRecord};
use crate::serving::error::{failure, JsonResult};
use crate::serving::WorkerState;
use axum::extract::{Path, State};
use axum::response::Json;
use axum::Extension;
use common::Principal;

pub(super) async fn peers(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<Vec<PeerRecord>> {
    state
        .runtime
        .peer_records(&principal)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn peer(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(peer): Path<String>,
) -> JsonResult<PeerRecord> {
    state
        .runtime
        .peer_record(&principal, &peer)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn resolve(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(peer): Path<String>,
) -> JsonResult<PeerRecord> {
    state
        .runtime
        .resolve_peer(&principal, &peer)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn delegate(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    claims: Option<Extension<DelegationClaims>>,
    Path(peer): Path<String>,
    Json(request): Json<DelegateRequest>,
) -> JsonResult<PeerDelegation> {
    state
        .runtime
        .delegate_to_peer(
            &principal,
            claims.as_ref().map(|claims| &claims.0),
            &peer,
            request,
        )
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn delegate_team_member(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    claims: Option<Extension<DelegationClaims>>,
    Path((team, member)): Path<(String, String)>,
    Json(request): Json<DelegateRequest>,
) -> JsonResult<PeerDelegation> {
    state
        .runtime
        .delegate_to_team_member(
            &principal,
            claims.as_ref().map(|claims| &claims.0),
            &team,
            &member,
            request,
        )
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn delegations(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
) -> JsonResult<Vec<PeerDelegation>> {
    state
        .runtime
        .peer_delegations(&principal)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn delegation(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<PeerDelegation> {
    state
        .runtime
        .peer_delegation(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn recover(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<PeerDelegation> {
    state
        .runtime
        .recover_peer_delegation(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}

pub(super) async fn cancel(
    State(state): State<WorkerState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> JsonResult<PeerDelegation> {
    state
        .runtime
        .cancel_peer_delegation(&principal, &id)
        .await
        .map(Json)
        .map_err(failure)
}
