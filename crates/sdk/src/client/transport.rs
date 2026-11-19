//! HTTP verbs, bounded retries, response decoding, and transport errors.

use super::Client;
use crate::PROTOCOL_VERSION;
use common::ApiError;
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Bounded client for request/response calls.
pub(super) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

/// Client for long-lived streams. Only the connect attempt is bounded.
pub(super) fn stream_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}

pub(super) async fn get<T: DeserializeOwned>(client: &Client, path: &str) -> Result<T, ApiError> {
    request(client, transient_retry, || {
        client
            .http
            .get(format!("{}{path}", client.base))
            .bearer_auth(&client.token)
    })
    .await
}

pub(super) async fn get_query<T: DeserializeOwned>(
    client: &Client,
    path: &str,
    query: &[(&str, &str)],
) -> Result<T, ApiError> {
    request(client, transient_retry, || {
        client
            .http
            .get(format!("{}{path}", client.base))
            .query(query)
            .bearer_auth(&client.token)
    })
    .await
}

pub(super) async fn post<T: DeserializeOwned, B: serde::Serialize>(
    client: &Client,
    path: &str,
    body: &B,
) -> Result<T, ApiError> {
    request(client, transient_retry, || {
        client
            .http
            .post(format!("{}{path}", client.base))
            .bearer_auth(&client.token)
            .json(body)
    })
    .await
}

/// Submit with reconciliation retries that reuse the caller's request body,
/// idempotency key, and canonical identity. A lost acknowledgment therefore
/// converges on the original task instead of creating a second one.
pub(super) async fn post_reconciling<T: DeserializeOwned, B: serde::Serialize>(
    client: &Client,
    path: &str,
    body: &B,
) -> Result<T, ApiError> {
    post_reconciling_with_trace(client, path, body, None).await
}

pub(super) async fn post_reconciling_with_trace<T: DeserializeOwned, B: serde::Serialize>(
    client: &Client,
    path: &str,
    body: &B,
    trace: Option<&str>,
) -> Result<T, ApiError> {
    let mut completed = 0;
    // Keep the first truthful uncertainty. A later retry that cannot reach the
    // Worker must not replace "outcome unknown" with a narrower transient
    // error, because the original request may still have been accepted.
    let mut uncertain: Option<ApiError> = None;
    loop {
        completed += 1;
        let mut request = client
            .http
            .post(format!("{}{path}", client.base))
            .bearer_auth(&client.token);
        if let Some(trace) = trace {
            request = request.header("traceparent", format!("00-{trace}-0000000000000001-01"));
        }
        let result = match request.json(body).send().await {
            Ok(response) => decode(response).await,
            Err(error) => Err(transport(error)),
        };
        match result {
            Ok(value) => return Ok(value),
            Err(error) => {
                if error.code == "task.unknown_submission" {
                    uncertain = Some(error.clone());
                }
                if completed < client.retry.max_attempts() && submission_retry(&error) {
                    tokio::time::sleep(client.retry.delay_after(completed)).await;
                    continue;
                }
                return Err(uncertain.unwrap_or(error));
            }
        }
    }
}

pub(super) async fn post_empty<T: DeserializeOwned>(
    client: &Client,
    path: &str,
) -> Result<T, ApiError> {
    request(client, transient_retry, || {
        client
            .http
            .post(format!("{}{path}", client.base))
            .bearer_auth(&client.token)
    })
    .await
}

pub(super) async fn put<T: DeserializeOwned, B: serde::Serialize>(
    client: &Client,
    path: &str,
    body: &B,
) -> Result<T, ApiError> {
    let response = client
        .http
        .put(format!("{}{path}", client.base))
        .bearer_auth(&client.token)
        .json(body)
        .send()
        .await
        .map_err(transport)?;
    decode(response).await
}

pub(super) async fn patch<T: DeserializeOwned, B: serde::Serialize>(
    client: &Client,
    path: &str,
    body: &B,
) -> Result<T, ApiError> {
    request(client, transient_retry, || {
        client
            .http
            .patch(format!("{}{path}", client.base))
            .bearer_auth(&client.token)
            .json(body)
    })
    .await
}

pub(super) async fn delete<T: DeserializeOwned>(
    client: &Client,
    path: &str,
) -> Result<T, ApiError> {
    request(client, transient_retry, || {
        client
            .http
            .delete(format!("{}{path}", client.base))
            .bearer_auth(&client.token)
    })
    .await
}

async fn request<T, M>(
    client: &Client,
    retry: fn(&ApiError) -> bool,
    mut make: M,
) -> Result<T, ApiError>
where
    T: DeserializeOwned,
    M: FnMut() -> reqwest::RequestBuilder,
{
    let mut completed = 0;
    loop {
        completed += 1;
        let result = match make().send().await {
            Ok(response) => decode(response).await,
            Err(error) => Err(transport(error)),
        };
        match result {
            Ok(value) => return Ok(value),
            Err(error) if completed < client.retry.max_attempts() && retry(&error) => {
                tokio::time::sleep(client.retry.delay_after(completed)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Retry a connection attempt only. HTTP responses, including typed failures,
/// are returned to the operation so rate limits and authorization stay visible.
pub(super) async fn send<M>(client: &Client, mut make: M) -> Result<reqwest::Response, ApiError>
where
    M: FnMut() -> reqwest::RequestBuilder,
{
    let mut completed = 0;
    loop {
        completed += 1;
        match make().send().await {
            Ok(response) => return Ok(response),
            Err(error) => {
                let error = transport(error);
                if completed < client.retry.max_attempts() && transient_retry(&error) {
                    tokio::time::sleep(client.retry.delay_after(completed)).await;
                    continue;
                }
                return Err(error);
            }
        }
    }
}

pub(super) fn transport(error: reqwest::Error) -> ApiError {
    ApiError::retryable("sdk.transport", format!("Gateway request failed: {error}"))
}

fn transient_retry(error: &ApiError) -> bool {
    matches!(
        error.code.as_str(),
        "sdk.transport" | "storage.unavailable" | "route.worker_unavailable"
    )
}

fn submission_retry(error: &ApiError) -> bool {
    transient_retry(error) || error.code == "task.unknown_submission"
}

async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, ApiError> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| ApiError::retryable("sdk.transport", error.to_string()))?;
    decode_bytes(status, &bytes)
}

pub(super) fn decode_bytes<T: DeserializeOwned>(
    status: reqwest::StatusCode,
    bytes: &[u8],
) -> Result<T, ApiError> {
    if !status.is_success() {
        if let Ok(error) = serde_json::from_slice::<ApiError>(bytes) {
            return Err(error);
        }
        return Err(ApiError::new(
            "sdk.invalid_response",
            format!("Gateway returned {status} without a typed error"),
        ));
    }
    // `task.unknown_submission` is a truthful success-family status with a
    // typed error body. The SDK must expose it as uncertainty, not as a
    // malformed success payload.
    if let Ok(error) = serde_json::from_slice::<ApiError>(bytes) {
        return Err(error);
    }
    serde_json::from_slice(bytes).map_err(|error| {
        ApiError::new(
            "sdk.invalid_response",
            format!("Gateway response did not match protocol {PROTOCOL_VERSION}: {error}"),
        )
    })
}
