//! Provider catalog, credential, pairing, and Worker envelope routes.
//!
//! Admin routes authenticate a caller and the control plane requires `admin` or
//! `provider:admin`. Worker routes authenticate the Worker control token inside
//! the handler and are mounted outside caller authentication.

use crate::http::{bearer, failure, JsonResult};
use crate::state::SharedState;
use axum::extract::{Path, State as AxumState};
use axum::http::HeaderMap;
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use common::{
    CreatePairCodeRequest, CreateProviderRequest, PairCodeResponse, ProviderBundle,
    ProviderCredentialSummary, ProviderMetadata, SetProviderCredentialRequest,
    UpdateProviderRequest, WorkerEnrollRequest, WorkerEnrollResponse,
};
use gateway_core::Caller;

/// Worker-control-authenticated provider routes.
pub(crate) fn control_routes() -> Router<SharedState> {
    Router::new()
        .route("/v1/model-providers/enroll", post(enroll))
        .route("/v1/model-providers", get(bundle))
}

/// Caller-authenticated provider administration.
pub(crate) fn admin_routes() -> Router<SharedState> {
    Router::new()
        .route("/v1/admin/model-providers", get(list).post(create))
        .route("/v1/admin/model-providers/pair-code", post(pair_code))
        .route("/v1/admin/model-providers/{id}", get(show).patch(update))
        .route(
            "/v1/admin/model-providers/{id}/credential",
            put(set_credential).delete(revoke_credential),
        )
        .route("/v1/admin/model-providers/{id}/discover", post(discover))
}

async fn list(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
) -> JsonResult<Vec<ProviderMetadata>> {
    state.core.provider_list(&caller).map(Json).map_err(failure)
}

async fn show(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<ProviderMetadata> {
    state
        .core
        .provider_show(&caller, &id)
        .map(Json)
        .map_err(failure)
}

async fn create(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Json(request): Json<CreateProviderRequest>,
) -> JsonResult<ProviderMetadata> {
    state
        .core
        .provider_create(&caller, request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn update(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(request): Json<UpdateProviderRequest>,
) -> JsonResult<ProviderMetadata> {
    state
        .core
        .provider_update(&caller, &id, request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn set_credential(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(request): Json<SetProviderCredentialRequest>,
) -> JsonResult<ProviderCredentialSummary> {
    state
        .core
        .provider_set_credential(&caller, &id, &request)
        .await
        .map(Json)
        .map_err(failure)
}

async fn discover(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<ProviderMetadata> {
    state
        .core
        .provider_discover(&caller, &id)
        .await
        .map(Json)
        .map_err(failure)
}

async fn revoke_credential(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> JsonResult<ProviderCredentialSummary> {
    state
        .core
        .provider_revoke_credential(&caller, &id)
        .map(Json)
        .map_err(failure)
}

async fn pair_code(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
    Json(request): Json<CreatePairCodeRequest>,
) -> JsonResult<PairCodeResponse> {
    state
        .core
        .provider_create_pair_code(&caller, request)
        .map(Json)
        .map_err(failure)
}

async fn enroll(
    AxumState(state): AxumState<SharedState>,
    headers: HeaderMap,
    Json(request): Json<WorkerEnrollRequest>,
) -> JsonResult<WorkerEnrollResponse> {
    state
        .core
        .provider_enroll_worker(bearer(&headers).as_deref(), &request)
        .map(Json)
        .map_err(failure)
}

async fn bundle(
    AxumState(state): AxumState<SharedState>,
    headers: HeaderMap,
) -> JsonResult<ProviderBundle> {
    state
        .core
        .provider_bundle(bearer(&headers).as_deref())
        .map(Json)
        .map_err(failure)
}
