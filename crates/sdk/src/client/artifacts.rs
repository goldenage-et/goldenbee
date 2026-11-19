//! Authorized artifact metadata and digest-verified downloads.

use super::transport::{decode_bytes, transport};
use super::Client;
use common::{ApiError, Artifact};

/// Maximum bytes the SDK will buffer for one artifact transfer.
///
/// This matches the Worker protocol's explicit artifact ceiling. The transfer
/// is read incrementally so a peer cannot force unbounded allocation.
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

/// One immutable artifact and its digest-verified original bytes.
#[derive(Debug, Clone)]
pub struct ArtifactDownload {
    /// Immutable metadata reported by the owning Worker.
    pub artifact: Artifact,
    /// Original bytes verified against `artifact.size` and `artifact.digest`.
    pub bytes: Vec<u8>,
}

impl Client {
    /// List immutable artifacts belonging to one routed task.
    pub async fn artifacts(&self, task_id: &str) -> Result<Vec<Artifact>, ApiError> {
        super::transport::get(self, &format!("/v1/tasks/{task_id}/artifacts")).await
    }

    /// Read metadata for one routed task artifact.
    pub async fn artifact(&self, task_id: &str, artifact_id: &str) -> Result<Artifact, ApiError> {
        super::transport::get(
            self,
            &format!("/v1/tasks/{task_id}/artifacts/{artifact_id}"),
        )
        .await
    }

    /// Fetch one artifact and verify its recorded size and SHA-256 digest.
    ///
    /// The download is bounded by [`MAX_ARTIFACT_BYTES`]. Connection loss and a
    /// lost body may be retried within the client policy because artifact
    /// versions are immutable; a digest mismatch is never retried.
    pub async fn download_artifact(
        &self,
        task_id: &str,
        artifact_id: &str,
    ) -> Result<ArtifactDownload, ApiError> {
        let artifact = self.artifact(task_id, artifact_id).await?;
        if artifact.size > MAX_ARTIFACT_BYTES as u64 {
            return Err(ApiError::new(
                "artifact.too_large",
                format!(
                    "artifact reports {} bytes, above the {MAX_ARTIFACT_BYTES} byte client limit",
                    artifact.size
                ),
            ));
        }
        let path = format!("/v1/tasks/{task_id}/artifacts/{artifact_id}/content");
        let mut completed = 0;
        let bytes = loop {
            completed += 1;
            match self
                .http
                .get(format!("{}{path}", self.base))
                .bearer_auth(&self.token)
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    if !status.is_success() {
                        let bytes = response.bytes().await.map_err(transport)?;
                        return Err(decode_bytes::<serde_json::Value>(status, &bytes).unwrap_err());
                    }
                    match read_bounded(response, MAX_ARTIFACT_BYTES).await {
                        Ok(bytes) => break bytes,
                        Err(error)
                            if completed < self.retry.max_attempts()
                                && error.code == "sdk.transport" =>
                        {
                            tokio::time::sleep(self.retry.delay_after(completed)).await;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => {
                    let error = transport(error);
                    if completed < self.retry.max_attempts() {
                        tokio::time::sleep(self.retry.delay_after(completed)).await;
                        continue;
                    }
                    return Err(error);
                }
            }
        };
        if bytes.len() as u64 != artifact.size {
            return Err(ApiError::new(
                "artifact.digest_mismatch",
                "artifact content length does not match its finalized metadata",
            ));
        }
        if common::bytes_digest(&bytes) != artifact.digest {
            return Err(ApiError::new(
                "artifact.digest_mismatch",
                "artifact content does not match its finalized digest",
            ));
        }
        Ok(ArtifactDownload { artifact, bytes })
    }
}

async fn read_bounded(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, ApiError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ApiError::retryable("sdk.transport", error.to_string()))?
    {
        let next = bytes.len().saturating_add(chunk.len());
        if next > limit {
            return Err(ApiError::new(
                "artifact.too_large",
                format!("artifact transfer exceeded the {limit} byte client limit"),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
