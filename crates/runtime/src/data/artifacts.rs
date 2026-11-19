//! Immutable native artifacts with scoped grants and digest verification.
//!
//! Content and versioning are owned by the pinned native ADK
//! [`adk_artifact::ArtifactService`]. This module adds harness-only metadata:
//! ownership, task-to-invocation provenance, digest, grants, and bounded
//! transfer. Artifact references are opaque ids; no shared filesystem path,
//! remote URL, or local-file scheme is addressable from a request.

use crate::{new_id, now, ApiError, ArtifactRecord, ArtifactScope, Result, Runtime};
use adk_artifact::{
    ArtifactService, DeleteRequest, FileArtifactService, ListRequest, ListResponse, LoadRequest,
    LoadResponse, SaveRequest, SaveResponse, VersionsRequest, VersionsResponse,
};
use adk_rust::Part;
use async_trait::async_trait;
use base64::Engine;
use common::{scopes, Artifact, ArtifactGrant, Principal, Task};
use std::path::PathBuf;

/// One authorized artifact version plus its original bytes.
pub struct ArtifactContent {
    /// Immutable metadata.
    pub artifact: Artifact,
    /// Original content, digest-verified before this returns.
    pub bytes: Vec<u8>,
}

impl Runtime {
    /// Finalize metadata for an artifact version produced by a native
    /// invocation. The native event's `artifact_delta` supplies the immutable
    /// version, so task-to-invocation provenance is recorded at creation.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_native_artifact(
        &self,
        task: &Task,
        native_invocation_id: &str,
        app_name: &str,
        user_id: &str,
        file_name: &str,
        version: i64,
    ) -> Result<Artifact> {
        validate_name(file_name)?;
        validate_app_name(app_name)?;
        let scope = ArtifactScope {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: task.session_id.clone(),
            file_name: file_name.to_string(),
        };
        let payload = self
            .artifact_store()
            .payload(
                &scope.app_name,
                &scope.user_id,
                &scope.session_id,
                &scope.file_name,
                version,
            )
            .await?;
        let digest = common::bytes_digest(&payload.bytes);
        let existing = self
            .store()
            .read(|state| {
                state
                    .artifacts
                    .values()
                    .find(|record| {
                        record.scope == scope
                            && record.artifact.version == version
                            && record.artifact.task_id == task.id
                    })
                    .cloned()
            })
            .await;
        if let Some(record) = existing {
            if record.artifact.digest != digest {
                return Err(ApiError::new(
                    "artifact.immutable_conflict",
                    "the finalized native version no longer matches its recorded digest",
                ));
            }
            return Ok(record.artifact);
        }
        let artifact = Artifact {
            id: new_id("art"),
            task_id: task.id.clone(),
            native_invocation_id: native_invocation_id.to_string(),
            attempt: task.attempt,
            trace_id: task.trace_id.clone(),
            version,
            namespace: task.namespace.clone(),
            owner: task.caller.clone(),
            name: file_name.to_string(),
            content_type: payload.content_type,
            size: payload.bytes.len() as u64,
            digest: digest.clone(),
            finalized: true,
            created_at: now(),
        };
        let event_artifact = artifact.clone();
        self.store()
            .write(|state| {
                state.artifacts.insert(
                    artifact.id.clone(),
                    ArtifactRecord {
                        artifact: artifact.clone(),
                        scope,
                    },
                );
                Ok(())
            })
            .await?;
        self.append_event(
            &task.id,
            "artifact.finalized",
            serde_json::json!({
                "artifact_id": event_artifact.id,
                "task_id": event_artifact.task_id,
                "native_invocation_id": event_artifact.native_invocation_id,
                "attempt": event_artifact.attempt,
                "version": event_artifact.version,
                "name": event_artifact.name,
                "content_type": event_artifact.content_type,
                "digest": event_artifact.digest,
                "size": event_artifact.size,
            }),
            true,
        )
        .await?;
        Ok(artifact)
    }

    /// Read metadata for a visible artifact.
    pub async fn artifact(&self, principal: &Principal, artifact_id: &str) -> Result<Artifact> {
        principal.require(scopes::ARTIFACT_READ)?;
        let record = self
            .store()
            .read(|state| state.artifacts.get(artifact_id).cloned())
            .await
            .ok_or_else(|| ApiError::new("artifact.not_found", "artifact is not visible"))?;
        principal.require_namespace(&record.artifact.namespace)?;
        if record.artifact.owner != principal.id && !principal.has(scopes::ADMIN) {
            return Err(ApiError::new(
                "scope.artifact_owner",
                "artifact belongs to another caller",
            ));
        }
        Ok(record.artifact)
    }

    /// List artifacts for a task.
    pub async fn artifacts(&self, principal: &Principal, task_id: &str) -> Result<Vec<Artifact>> {
        let task = self.task(principal, task_id).await?;
        principal.require(scopes::ARTIFACT_READ)?;
        Ok(self
            .store()
            .read(|state| {
                state
                    .artifacts
                    .values()
                    .filter(|record| record.artifact.task_id == task.id)
                    .map(|record| record.artifact.clone())
                    .collect()
            })
            .await)
    }

    /// Read artifact metadata and digest-verified bytes with authorization.
    pub async fn read_artifact(
        &self,
        principal: &Principal,
        artifact_id: &str,
    ) -> Result<ArtifactContent> {
        let artifact = self.artifact(principal, artifact_id).await?;
        let bytes = self.read_verified(&artifact).await?;
        Ok(ArtifactContent { artifact, bytes })
    }

    /// Issue a short-lived scoped grant for one immutable artifact version.
    pub async fn grant_artifact(
        &self,
        principal: &Principal,
        artifact_id: &str,
        ttl_ms: i64,
    ) -> Result<ArtifactGrant> {
        let artifact = self.artifact(principal, artifact_id).await?;
        let grant = format!(
            "grant_{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(uuid::Uuid::new_v4().as_bytes())
        );
        let record = ArtifactGrant {
            artifact_id: artifact.id,
            grant: grant.clone(),
            expires_at: now() + ttl_ms.max(1),
        };
        self.store()
            .write(|state| {
                state.grants.insert(grant.clone(), record.clone());
                Ok(())
            })
            .await?;
        Ok(record)
    }

    /// Exchange a scoped grant for digest-verified bytes. The grant binds one
    /// artifact and expiry; it carries no filesystem path or URL.
    pub async fn read_grant(&self, grant: &str) -> Result<ArtifactContent> {
        let record = self
            .store()
            .read(|state| state.grants.get(grant).cloned())
            .await
            .ok_or_else(|| {
                ApiError::new("artifact.grant_invalid", "artifact grant is not valid")
            })?;
        if record.expires_at <= now() {
            return Err(ApiError::new(
                "artifact.grant_expired",
                "artifact grant has expired",
            ));
        }
        let artifact = self
            .store()
            .read(|state| {
                state
                    .artifacts
                    .get(&record.artifact_id)
                    .map(|record| record.artifact.clone())
            })
            .await
            .ok_or_else(|| ApiError::new("artifact.not_found", "artifact is not visible"))?;
        let bytes = self.read_verified(&artifact).await?;
        Ok(ArtifactContent { artifact, bytes })
    }

    /// Delete one native version when retention or erasure removes metadata.
    pub async fn delete_native_artifact(&self, scope: &ArtifactScope, version: i64) -> Result<()> {
        self.artifact_store()
            .delete_version(
                &scope.app_name,
                &scope.user_id,
                &scope.session_id,
                &scope.file_name,
                version,
            )
            .await
    }

    async fn read_verified(&self, artifact: &Artifact) -> Result<Vec<u8>> {
        let scope = self
            .store()
            .read(|state| {
                state
                    .artifacts
                    .get(&artifact.id)
                    .map(|record| record.scope.clone())
            })
            .await
            .ok_or_else(|| ApiError::new("artifact.not_found", "artifact metadata disappeared"))?;
        let payload = self
            .artifact_store()
            .payload(
                &scope.app_name,
                &scope.user_id,
                &scope.session_id,
                &scope.file_name,
                artifact.version,
            )
            .await?;
        if payload.bytes.len() as u64 != artifact.size
            || common::bytes_digest(&payload.bytes) != artifact.digest
        {
            return Err(ApiError::new(
                "artifact.digest_mismatch",
                "artifact content does not match its finalized digest",
            ));
        }
        Ok(payload.bytes)
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name.len() > 200
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(ApiError::new(
            "validation.artifact_name",
            "artifact name must be a 1-200 character identifier",
        ));
    }
    Ok(())
}

fn validate_app_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 200 || name.chars().any(char::is_control) {
        return Err(ApiError::new(
            "validation.artifact_scope",
            "artifact application identity is not valid",
        ));
    }
    Ok(())
}

/// Directory below the Worker Instance data directory.
pub const ARTIFACT_STORE_DIR: &str = "native-artifacts";

/// Maximum accepted artifact size. Transfer buffers are bounded by this value.
pub const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

/// Neutral content view of one stored native artifact version.
#[derive(Debug, Clone)]
pub struct ArtifactPayload {
    /// Content type reported by the native `Part`.
    pub content_type: String,
    /// Original bytes, never the serialized native envelope.
    pub bytes: Vec<u8>,
}

/// File-backed native artifact service with the harness storage policy.
pub struct NativeArtifactStore {
    inner: FileArtifactService,
}

impl NativeArtifactStore {
    /// Open the native store below a Worker Instance data directory.
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let base = data_dir.into().join(ARTIFACT_STORE_DIR);
        let inner = FileArtifactService::new(base).map_err(|error| {
            storage_error(format!("cannot open native artifact store: {error}"))
        })?;
        Ok(Self { inner })
    }

    /// Load one immutable version as neutral content.
    pub async fn payload(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
        version: i64,
    ) -> Result<ArtifactPayload> {
        let response = self
            .inner
            .load(LoadRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                session_id: session_id.to_string(),
                file_name: file_name.to_string(),
                version: Some(version),
            })
            .await
            .map_err(native_error)?;
        part_payload(&response.part)
    }

    /// Delete one native version. Missing content is not an error so metadata
    /// retention remains idempotent.
    pub async fn delete_version(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
        version: i64,
    ) -> Result<()> {
        self.inner
            .delete(DeleteRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                session_id: session_id.to_string(),
                file_name: file_name.to_string(),
                version: Some(version),
            })
            .await
            .map_err(native_error)
    }
}

#[async_trait]
impl ArtifactService for NativeArtifactStore {
    async fn save(&self, req: SaveRequest) -> adk_rust::Result<SaveResponse> {
        // Validate before delegating so an agent cannot store an arbitrary URL,
        // local-file reference, or unbounded payload through this instance.
        part_payload(&req.part).map_err(|error| {
            adk_rust::AdkError::artifact(format!("{}: {}", error.code, error.message))
        })?;
        self.inner.save(req).await
    }

    async fn load(&self, req: LoadRequest) -> adk_rust::Result<LoadResponse> {
        self.inner.load(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> adk_rust::Result<()> {
        self.inner.delete(req).await
    }

    async fn list(&self, req: ListRequest) -> adk_rust::Result<ListResponse> {
        self.inner.list(req).await
    }

    async fn versions(&self, req: VersionsRequest) -> adk_rust::Result<VersionsResponse> {
        self.inner.versions(req).await
    }

    async fn health_check(&self) -> adk_rust::Result<()> {
        self.inner.health_check().await
    }
}

/// Convert a native `Part` to original bytes without retaining the serialized
/// envelope. Non-content parts are refused rather than fetched.
pub fn part_payload(part: &Part) -> Result<ArtifactPayload> {
    let (content_type, bytes) = match part {
        Part::Text { text } => (
            "text/plain; charset=utf-8".to_string(),
            text.as_bytes().to_vec(),
        ),
        Part::InlineData {
            mime_type, data, ..
        } => {
            let content_type = if mime_type.trim().is_empty() {
                "application/octet-stream".to_string()
            } else {
                mime_type.clone()
            };
            (content_type, data.clone())
        }
        Part::FileData { .. } => {
            return Err(ApiError::new(
                "artifact.uri_denied",
                "artifact content cannot be a remote URL or local-file reference",
            ));
        }
        _ => {
            return Err(ApiError::new(
                "artifact.unsupported_part",
                "artifact content must be text or inline binary data",
            ));
        }
    };
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(ApiError::new(
            "artifact.too_large",
            format!("artifact exceeds the {MAX_ARTIFACT_BYTES} byte limit"),
        ));
    }
    Ok(ArtifactPayload {
        content_type,
        bytes,
    })
}

fn native_error(error: adk_rust::AdkError) -> ApiError {
    if error.message.to_ascii_lowercase().contains("not found") {
        ApiError::new("artifact.not_found", "artifact content is not available")
    } else {
        storage_error(format!("native artifact operation failed: {error}"))
    }
}

fn storage_error(message: impl Into<String>) -> ApiError {
    ApiError::new("artifact.storage_unavailable", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_and_local_file_references_are_denied() {
        for uri in ["https://example.com/evidence", "file:///etc/passwd"] {
            let error = part_payload(&Part::FileData {
                mime_type: "text/plain".into(),
                file_uri: uri.into(),
                annotations: None,
            })
            .expect_err("uri references are denied");
            assert_eq!(error.code, "artifact.uri_denied");
        }
    }

    #[test]
    fn text_and_inline_payloads_preserve_content_metadata() {
        let text = part_payload(&Part::Text {
            text: "evidence".into(),
        })
        .expect("text payload");
        assert_eq!(text.content_type, "text/plain; charset=utf-8");
        assert_eq!(text.bytes, b"evidence");

        let inline = part_payload(&Part::InlineData {
            mime_type: "application/octet-stream".into(),
            data: vec![0, 1, 2],
            uri: None,
            annotations: None,
        })
        .expect("inline payload");
        assert_eq!(inline.content_type, "application/octet-stream");
        assert_eq!(inline.bytes, vec![0, 1, 2]);
    }
}
