//! Optional Gateway registration and lease heartbeat.
//!
//! Registration and renewal run in the background with bounded backoff. A
//! missing or unreachable Gateway never blocks local Worker execution. An
//! authentication or authorization rejection stops the heartbeat and records
//! an actionable status error instead of crashing the process.

use crate::worker::{GatewayConfig, WorkerInstance};
use crate::Runtime;
use common::{ApiError, Capacity, WorkerLeaseRequest, WorkerRegistrationRequest};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tokio::task::JoinHandle;

const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize)]
struct LeaseAck {
    worker: String,
}

enum StepError {
    /// Authentication or authorization failed. Retrying cannot help.
    Auth(String),
    /// The approval disabled this identity. Keep retrying so an explicit
    /// enable recovers automatically.
    Disabled(String),
    /// The approval disappeared. Retry registration from the beginning.
    NotFound(String),
    /// A terminal identity or configuration conflict.
    Terminal(String),
    /// Transient network or Gateway failure.
    Retry(String),
}

/// Start the optional heartbeat. Returns `None` when no Gateway is configured.
pub(super) async fn spawn(
    instance: WorkerInstance,
    advertise_url: String,
) -> Option<JoinHandle<()>> {
    let config = instance.gateway_config().await?;
    let runtime = instance.runtime();
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    Some(tokio::spawn(async move {
        heartbeat(client, config, runtime, advertise_url).await;
    }))
}

async fn heartbeat(client: Client, config: GatewayConfig, runtime: Runtime, advertise_url: String) {
    let base = config.url.trim_end_matches('/').to_string();
    let advertise_url = config.advertise_url.clone().unwrap_or(advertise_url);
    let interval = Duration::from_millis(config.renew_interval_ms.clamp(100, 3_600_000));
    let mut worker_id: Option<String> = None;
    let mut backoff = INITIAL_BACKOFF;
    let mut last_status = String::new();
    loop {
        let health = runtime.health().await;
        if !health.ready {
            eprintln!(
                "gateway: heartbeat stopped: Worker is not ready (phase={})",
                health.phase
            );
            return;
        }
        let capacity: Capacity = health.capacity;
        let incarnation = runtime.config().incarnation.clone();
        let result = match worker_id.as_deref() {
            Some(id) => renew(&client, &base, &config, id, &incarnation, &capacity).await,
            None => {
                register(
                    &client,
                    &base,
                    &config,
                    &advertise_url,
                    &incarnation,
                    &capacity,
                )
                .await
            }
        };
        match result {
            Ok(id) => {
                if last_status != "registered" {
                    eprintln!("gateway: registered '{id}' and started lease renewal");
                    last_status = "registered".into();
                }
                worker_id = Some(id);
                backoff = INITIAL_BACKOFF;
                tokio::time::sleep(interval).await;
            }
            Err(StepError::Auth(message)) => {
                eprintln!("gateway: heartbeat stopped: {message}");
                return;
            }
            Err(StepError::Terminal(message)) => {
                eprintln!("gateway: heartbeat stopped: {message}");
                return;
            }
            Err(StepError::Disabled(message)) => {
                if last_status != "disabled" {
                    eprintln!(
                        "gateway: Worker is disabled; heartbeat will retry until an operator enables it: {message}"
                    );
                    last_status = "disabled".into();
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
            Err(StepError::NotFound(message)) => {
                if last_status != "missing" {
                    eprintln!("gateway: approval is not available yet: {message}");
                    last_status = "missing".into();
                }
                worker_id = None;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
            Err(StepError::Retry(message)) => {
                if last_status != message {
                    eprintln!(
                        "gateway: registration heartbeat is retrying; local execution is unaffected: {message}"
                    );
                    last_status = message.clone();
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

async fn register(
    client: &Client,
    base: &str,
    config: &GatewayConfig,
    advertise_url: &str,
    incarnation: &str,
    capacity: &Capacity,
) -> Result<String, StepError> {
    let body = WorkerRegistrationRequest {
        endpoint: advertise_url.to_string(),
        incarnation: incarnation.to_string(),
        capacity: capacity.clone(),
    };
    let response = client
        .post(format!("{base}/v1/workers/register"))
        .bearer_auth(&config.control_token)
        .json(&body)
        .send()
        .await
        .map_err(|error| StepError::Retry(format!("Gateway is unreachable: {error}")))?;
    lease_worker(response).await
}

async fn renew(
    client: &Client,
    base: &str,
    config: &GatewayConfig,
    id: &str,
    incarnation: &str,
    capacity: &Capacity,
) -> Result<String, StepError> {
    let body = WorkerLeaseRequest {
        incarnation: incarnation.to_string(),
        capacity: capacity.clone(),
    };
    let response = client
        .post(format!("{base}/v1/workers/{id}/lease"))
        .bearer_auth(&config.control_token)
        .json(&body)
        .send()
        .await
        .map_err(|error| StepError::Retry(format!("Gateway is unreachable: {error}")))?;
    lease_worker(response).await
}

async fn lease_worker(response: reqwest::Response) -> Result<String, StepError> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| StepError::Retry(format!("response body was lost: {error}")))?;
    if status.is_success() {
        return serde_json::from_slice::<LeaseAck>(&bytes)
            .map(|ack| ack.worker)
            .map_err(|_| StepError::Retry("lease response did not match protocol 1.0".into()));
    }
    let error = serde_json::from_slice::<ApiError>(&bytes).unwrap_or_else(|_| {
        ApiError::new(
            "worker.control_rejected",
            format!("Gateway returned {status} without a typed error"),
        )
    });
    let message = format!("{}: {}", error.code, common::redact(&error.message));
    match status.as_u16() {
        401 | 403 => Err(StepError::Auth(message)),
        404 => Err(StepError::NotFound(message)),
        409 if error.code == "worker.disabled" => Err(StepError::Disabled(message)),
        409 if error.code == "worker.decommissioned" => Err(StepError::Terminal(message)),
        409 => Err(StepError::Terminal(message)),
        429 | 500..=599 => Err(StepError::Retry(message)),
        _ => Err(StepError::Terminal(message)),
    }
}
