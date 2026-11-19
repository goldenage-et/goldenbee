//! HTTP listener over the host composer. Authentication is per-host bearer
//! credential; handlers authenticate before any privileged host action.

use crate::composer::Composer;
use crate::config::HostCaller;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::{
    ApiError, HostInspection, HostOperationOutcome, HostProvisionRequest, HostRollbackRequest,
    HostUpgradeRequest,
};
use std::sync::Arc;

type Shared = Arc<Composer>;

pub(crate) fn router(composer: Arc<Composer>) -> Router {
    Router::new()
        .route("/v1/host", get(descriptor))
        .route("/v1/instances", post(provision))
        .route("/v1/instances/{instance}", get(inspect))
        .route("/v1/instances/{instance}/upgrade", post(upgrade))
        .route("/v1/instances/{instance}/rollback", post(rollback))
        .route("/v1/operations/{operation}", get(operation))
        .with_state(composer)
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(str::to_string)
}

fn failure(error: ApiError) -> Response {
    let status = StatusCode::from_u16(error.status()).unwrap_or(StatusCode::BAD_REQUEST);
    (status, Json(error)).into_response()
}

fn authenticate(composer: &Composer, headers: &HeaderMap) -> Result<HostCaller, Response> {
    composer
        .authenticate(bearer(headers).as_deref())
        .map_err(failure)
}

async fn descriptor(State(composer): State<Shared>, headers: HeaderMap) -> Response {
    if let Err(response) = authenticate(&composer, &headers) {
        return response;
    }
    Json(composer.descriptor()).into_response()
}

async fn provision(
    State(composer): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<HostProvisionRequest>,
) -> Response {
    let caller = match authenticate(&composer, &headers) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match tokio::task::spawn_blocking(move || composer.provision(&caller, &request)).await {
        Ok(Ok(outcome)) => Json(outcome).into_response(),
        Ok(Err(error)) => failure(error),
        Err(_) => failure(ApiError::new(
            "host.operation_failed",
            "the host operation task did not complete",
        )),
    }
}

async fn inspect(
    State(composer): State<Shared>,
    headers: HeaderMap,
    Path(instance): Path<String>,
) -> Response {
    let caller = match authenticate(&composer, &headers) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match tokio::task::spawn_blocking(move || composer.inspect(&caller, &instance)).await {
        Ok(Ok(view)) => {
            let worker = None;
            Json(HostInspection {
                host: view.host.clone(),
                view,
                worker,
            })
            .into_response()
        }
        Ok(Err(error)) => failure(error),
        Err(_) => failure(ApiError::new(
            "host.operation_failed",
            "the host inspection task did not complete",
        )),
    }
}

async fn upgrade(
    State(composer): State<Shared>,
    headers: HeaderMap,
    Path(instance): Path<String>,
    Json(request): Json<HostUpgradeRequest>,
) -> Response {
    let caller = match authenticate(&composer, &headers) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match tokio::task::spawn_blocking(move || composer.upgrade(&caller, &instance, &request)).await
    {
        Ok(Ok(outcome)) => Json(outcome).into_response(),
        Ok(Err(error)) => failure(error),
        Err(_) => failure(ApiError::new(
            "host.operation_failed",
            "the host upgrade task did not complete",
        )),
    }
}

async fn rollback(
    State(composer): State<Shared>,
    headers: HeaderMap,
    Path(instance): Path<String>,
    Json(request): Json<HostRollbackRequest>,
) -> Response {
    let caller = match authenticate(&composer, &headers) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match tokio::task::spawn_blocking(move || composer.rollback(&caller, &instance, &request)).await
    {
        Ok(Ok(outcome)) => Json(outcome).into_response(),
        Ok(Err(error)) => failure(error),
        Err(_) => failure(ApiError::new(
            "host.operation_failed",
            "the host rollback task did not complete",
        )),
    }
}

async fn operation(
    State(composer): State<Shared>,
    headers: HeaderMap,
    Path(operation): Path<String>,
) -> Response {
    let caller = match authenticate(&composer, &headers) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match tokio::task::spawn_blocking(move || composer.operation(&caller, &operation)).await {
        Ok(Ok(outcome)) => Json::<HostOperationOutcome>(outcome).into_response(),
        Ok(Err(error)) => failure(error),
        Err(_) => failure(ApiError::new(
            "host.operation_failed",
            "the host operation lookup did not complete",
        )),
    }
}
