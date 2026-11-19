//! Gateway connection state shared by the typed operation modules.

mod admin;
mod approvals;
mod artifacts;
mod control;
mod diagnostics;
mod fleet;
mod profiles;
mod providers;
mod retry;
mod tasks;
mod teams;
mod transport;
pub(crate) mod watch;
mod workers;

#[cfg(test)]
mod tests;

use common::{ApiError, GatewayHealth};

pub use artifacts::{ArtifactDownload, MAX_ARTIFACT_BYTES};
pub use retry::RetryPolicy;
pub use watch::{Watch, MAX_STREAM_BUFFER};

/// Reusable Gateway client.
#[derive(Clone)]
pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
    stream_http: reqwest::Client,
    retry: RetryPolicy,
}

impl Client {
    /// Build a client for one Gateway endpoint and caller token.
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        Self::with_retry_policy(base, token, RetryPolicy::default())
    }

    /// Build a client with explicit bounded retry settings.
    pub fn with_retry_policy(
        base: impl Into<String>,
        token: impl Into<String>,
        retry: RetryPolicy,
    ) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: transport::http_client(),
            stream_http: transport::stream_client(),
            retry,
        }
    }

    /// Current bounded retry settings.
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry
    }

    /// Gateway liveness.
    pub async fn health(&self) -> Result<GatewayHealth, ApiError> {
        transport::get(self, "/v1/health").await
    }
}
