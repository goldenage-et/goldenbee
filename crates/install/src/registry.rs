//! Installer-side registry integration.
//!
//! The HTTP client itself lives in the public `registry` crate. This module
//! keeps the installer-specific operations: building an upload bundle from a
//! verified local layout, and turning a downloaded immutable release into the
//! existing verification, policy, and smoke path.

use crate::artifact::{is_digest, read_layout};
use crate::{InstallError, InstallOutcome, Installer};
use ::registry::{
    PublicQuery, PublicRelease, PublishReceipt, RegistryClient as CoreClient, RegistryError,
    VersionEntry,
};
use serde_json::Value;
use std::path::Path;

pub use ::registry::ReleaseBundle;

/// Read and verify one local OCI layout into an upload bundle.
pub fn bundle_from_layout(root: &Path) -> Result<ReleaseBundle, InstallError> {
    let layout = read_layout(root)?;
    let index_bytes = std::fs::read(root.join("index.json")).map_err(|_| {
        InstallError::new("install.layout_invalid", "artifact has no index descriptor")
    })?;
    let index: Value = serde_json::from_slice(&index_bytes)
        .map_err(|_| InstallError::new("install.layout_invalid", "artifact index is malformed"))?;
    let descriptor_digest = index["manifests"][0]["digest"].as_str().ok_or_else(|| {
        InstallError::new("install.layout_invalid", "artifact index has no digest")
    })?;
    if descriptor_digest != layout.artifact_digest {
        return Err(InstallError::new(
            "install.layout_invalid",
            "index descriptor does not match the verified manifest digest",
        ));
    }
    let mut blobs = Vec::new();
    let mut pending = vec![descriptor_digest.to_string()];
    let manifest: Value = serde_json::from_slice(&layout.manifest_bytes)
        .map_err(|_| InstallError::new("install.layout_invalid", "manifest is malformed"))?;
    if let Some(config) = manifest["config"]["digest"].as_str() {
        pending.push(config.to_string());
    }
    if let Some(layers) = manifest["layers"].as_array() {
        for layer in layers {
            if let Some(digest) = layer["digest"].as_str() {
                pending.push(digest.to_string());
            }
        }
    }
    for digest in pending {
        let bytes = read_blob(root, &digest)?;
        blobs.push((digest, bytes));
    }
    Ok(ReleaseBundle {
        metadata: layout.metadata,
        index_json: index_bytes,
        blobs,
    })
}

/// One authenticated registry connection with installer integration.
#[derive(Debug, Clone)]
pub struct RegistryClient {
    inner: CoreClient,
}

impl RegistryClient {
    /// Create an authenticated client for one registry URL and bearer token.
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Result<Self, InstallError> {
        Ok(Self {
            inner: CoreClient::new(base, token)?,
        })
    }

    /// Create an anonymous client for explicitly public discovery and download.
    /// Installation still passes the full local installer trust path.
    pub fn public(base: impl Into<String>) -> Result<Self, InstallError> {
        Ok(Self {
            inner: CoreClient::public(base)?,
        })
    }

    /// Anonymous discovery over explicitly public Worker Package releases.
    pub async fn discover(&self, query: &PublicQuery) -> Result<Vec<PublicRelease>, InstallError> {
        self.inner.discover(query).await.map_err(Into::into)
    }

    /// Change an authenticated package's public visibility. This is separate
    /// from the immutable release and never changes installer trust.
    pub async fn set_visibility(
        &self,
        publisher: &str,
        package: &str,
        public: bool,
    ) -> Result<(), InstallError> {
        self.inner
            .set_visibility(publisher, package, public)
            .await
            .map_err(Into::into)
    }

    /// Upload one signed layout. Identical retries converge; a conflicting
    /// version is rejected by the registry.
    pub async fn publish(
        &self,
        publisher: &str,
        bundle: &ReleaseBundle,
    ) -> Result<PublishReceipt, InstallError> {
        self.inner
            .publish(publisher, bundle)
            .await
            .map_err(Into::into)
    }

    /// List immutable versions and compatibility metadata for one package.
    pub async fn list(
        &self,
        publisher: &str,
        package: &str,
    ) -> Result<Vec<VersionEntry>, InstallError> {
        self.inner
            .list(publisher, package)
            .await
            .map_err(Into::into)
    }

    /// Fetch one published version.
    pub async fn show(
        &self,
        publisher: &str,
        package: &str,
        version: &str,
    ) -> Result<VersionEntry, InstallError> {
        self.inner
            .show(publisher, package, version)
            .await
            .map_err(Into::into)
    }

    /// Download a chosen immutable digest and run the existing installer.
    ///
    /// No downloaded binary is executed for inspection. It is executed only by
    /// the installer's constrained smoke step after signature, digest,
    /// compatibility, permission, and policy checks.
    pub async fn install(
        &self,
        publisher: &str,
        package: &str,
        digest: &str,
        installer: &Installer,
    ) -> Result<InstallOutcome, InstallError> {
        if !is_digest(digest) {
            return Err(InstallError::new(
                "install.registry_invalid",
                "install requires an immutable sha256 digest",
            ));
        }
        let versions = self.list(publisher, package).await?;
        let entry = versions
            .into_iter()
            .find(|entry| entry.artifact_digest == digest)
            .ok_or_else(|| {
                InstallError::new(
                    "install.registry_not_found",
                    "chosen digest is not published for this package",
                )
            })?;
        if entry.package != package || entry.publisher != publisher {
            return Err(InstallError::new(
                "install.registry_digest_mismatch",
                "registry metadata identity does not match the requested package",
            ));
        }
        let index_bytes = self
            .inner
            .fetch_bytes(&format!(
                "/v1{}/packages/{}/{}/releases/{}/index",
                self.inner_prefix(),
                encode_path(publisher),
                encode_path(package),
                encode_path(digest)
            ))
            .await?;
        let index: Value = serde_json::from_slice(&index_bytes).map_err(|_| {
            InstallError::new(
                "install.registry_invalid",
                "release descriptor is malformed",
            )
        })?;
        let manifest_digest = index["manifests"][0]["digest"].as_str().ok_or_else(|| {
            InstallError::new(
                "install.registry_invalid",
                "release descriptor has no manifest digest",
            )
        })?;
        if manifest_digest != digest {
            return Err(InstallError::new(
                "install.registry_digest_mismatch",
                "release descriptor does not match the chosen digest",
            ));
        }
        let temporary = tempfile::tempdir().map_err(|error| {
            InstallError::new(
                "install.storage_unavailable",
                format!("registry staging directory could not be created: {error}"),
            )
        })?;
        write_layout_envelope(temporary.path(), &index_bytes)?;
        let manifest_bytes = self
            .inner
            .download_blob(
                &self.inner.blob_path(publisher, package, manifest_digest),
                manifest_digest,
            )
            .await?;
        write_blob(temporary.path(), manifest_digest, &manifest_bytes)?;
        let manifest: Value = serde_json::from_slice(&manifest_bytes).map_err(|_| {
            InstallError::new("install.registry_invalid", "release manifest is malformed")
        })?;
        let mut descriptors = Vec::new();
        if let Some(digest) = manifest["config"]["digest"].as_str() {
            descriptors.push(digest.to_string());
        }
        if let Some(layers) = manifest["layers"].as_array() {
            for layer in layers {
                if let Some(digest) = layer["digest"].as_str() {
                    descriptors.push(digest.to_string());
                }
            }
        }
        for descriptor in descriptors {
            let bytes = self
                .inner
                .download_blob(
                    &self.inner.blob_path(publisher, package, &descriptor),
                    &descriptor,
                )
                .await?;
            write_blob(temporary.path(), &descriptor, &bytes)?;
        }
        installer.install(temporary.path()).await
    }

    fn inner_prefix(&self) -> &'static str {
        // Public clients use the anonymous route prefix. The core client keeps
        // this decision private, so mirror it from the authentication mode.
        if self.inner.is_authenticated() {
            ""
        } else {
            "/public"
        }
    }
}

impl From<RegistryError> for InstallError {
    fn from(error: RegistryError) -> Self {
        let code = match error.code {
            "registry.unauthorized" => "install.registry_unauthorized",
            "registry.forbidden" => "install.registry_forbidden",
            "registry.not_found" => "install.registry_not_found",
            "registry.conflict" => "install.registry_conflict",
            "registry.limit" => "install.registry_limit",
            "registry.digest_mismatch" => "install.registry_digest_mismatch",
            "registry.unavailable" => "install.registry_unavailable",
            "registry.invalid" => "install.registry_invalid",
            _ => "install.registry_invalid",
        };
        Self::new(code, error.message)
    }
}

fn read_blob(root: &Path, digest: &str) -> Result<Vec<u8>, InstallError> {
    let hex = digest.strip_prefix("sha256:").ok_or_else(|| {
        InstallError::new("install.layout_invalid", "artifact digest is not sha256")
    })?;
    let bytes = std::fs::read(root.join("blobs").join("sha256").join(hex)).map_err(|_| {
        InstallError::new(
            "install.layout_invalid",
            "artifact references a missing blob",
        )
    })?;
    if common::bytes_digest(&bytes) != digest {
        return Err(InstallError::new(
            "install.layer_digest_mismatch",
            "artifact blob fails digest verification",
        ));
    }
    Ok(bytes)
}

fn write_layout_envelope(root: &Path, index: &[u8]) -> Result<(), InstallError> {
    std::fs::create_dir_all(root.join("blobs").join("sha256")).map_err(storage)?;
    std::fs::write(root.join("index.json"), index).map_err(storage)?;
    std::fs::write(
        root.join("oci-layout"),
        br#"{"imageLayoutVersion":"1.0.0"}"#,
    )
    .map_err(storage)
}

fn write_blob(root: &Path, digest: &str, bytes: &[u8]) -> Result<(), InstallError> {
    let hex = digest.strip_prefix("sha256:").ok_or_else(|| {
        InstallError::new("install.layout_invalid", "artifact digest is not sha256")
    })?;
    std::fs::write(root.join("blobs").join("sha256").join(hex), bytes).map_err(storage)
}

fn storage(error: std::io::Error) -> InstallError {
    InstallError::new(
        "install.storage_unavailable",
        format!("registry staging layout could not be written: {error}"),
    )
}

fn encode_path(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':') {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}
