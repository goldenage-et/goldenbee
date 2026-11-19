//! Authenticated registry client used by publishers and installers.
//!
//! This crate owns the wire client only. The registry service, bearer-token
//! configuration, and storage implementation are a separate deployment.

use crate::types::{
    PublicQuery, PublicRelease, PublishReceipt, RegistryError, ReleaseBundle, VersionEntry,
};
use serde_json::Value;

/// One authenticated registry connection.
#[derive(Debug, Clone)]
pub struct RegistryClient {
    base: String,
    token: Option<String>,
    client: reqwest::Client,
}

impl RegistryClient {
    /// Create an authenticated client for one registry URL and bearer token.
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Result<Self, RegistryError> {
        Self::with_token(base, Some(token.into()))
    }

    /// Create an anonymous client for explicitly public discovery and download.
    /// Installation still passes the full local installer trust path.
    pub fn public(base: impl Into<String>) -> Result<Self, RegistryError> {
        Self::with_token(base, None)
    }

    fn with_token(base: impl Into<String>, token: Option<String>) -> Result<Self, RegistryError> {
        let base = base.into().trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(RegistryError::new(
                "registry.invalid",
                "registry URL must use http or https",
            ));
        }
        Ok(Self {
            base,
            token,
            client: reqwest::Client::new(),
        })
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    /// Whether this client carries a bearer token.
    pub fn is_authenticated(&self) -> bool {
        self.token.is_some()
    }

    fn api_prefix(&self) -> &'static str {
        if self.is_authenticated() {
            ""
        } else {
            "/public"
        }
    }

    fn require_token(&self) -> Result<&str, RegistryError> {
        self.token.as_deref().ok_or_else(|| {
            RegistryError::new(
                "registry.unauthorized",
                "this registry operation requires an authenticated token",
            )
        })
    }

    /// Anonymous discovery over explicitly public Worker Package releases.
    pub async fn discover(&self, query: &PublicQuery) -> Result<Vec<PublicRelease>, RegistryError> {
        let response = self
            .authorize(self.client.get(format!(
                "{}/v1/public/packages?{}",
                self.base,
                query_string(query)
            )))
            .send()
            .await
            .map_err(transport)?;
        #[derive(serde::Deserialize)]
        struct Packages {
            packages: Vec<PublicRelease>,
        }
        let packages: Packages = self.decode(response).await?;
        Ok(packages.packages)
    }

    /// Change an authenticated package's public visibility. This is separate
    /// from the immutable release and never changes installer trust.
    pub async fn set_visibility(
        &self,
        publisher: &str,
        package: &str,
        public: bool,
    ) -> Result<(), RegistryError> {
        self.require_token()?;
        let response = self
            .authorize(self.client.put(format!(
                "{}/v1/packages/{}/{}/visibility",
                self.base,
                encode_path(publisher),
                encode_path(package)
            )))
            .json(&serde_json::json!({ "public": public }))
            .send()
            .await
            .map_err(transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.rejection(response).await)
        }
    }

    /// Upload one signed layout. Identical retries converge; a conflicting
    /// version is rejected by the registry.
    pub async fn publish(
        &self,
        publisher: &str,
        bundle: &ReleaseBundle,
    ) -> Result<PublishReceipt, RegistryError> {
        let package = &bundle.metadata.package.name;
        self.require_token()?;
        let upload = self.start_upload(publisher, package).await?;
        for (digest, bytes) in &bundle.blobs {
            self.upload_blob(&upload, digest, bytes).await?;
        }
        let response = self
            .authorize(self.client.post(format!(
                "{}/v1/uploads/{}/publish",
                self.base,
                encode_path(&upload)
            )))
            .body(bundle.index_json.clone())
            .send()
            .await
            .map_err(transport)?;
        let receipt: PublishReceipt = self.decode(response).await?;
        if receipt.entry.artifact_digest != artifact_digest(&bundle.index_json)? {
            return Err(RegistryError::new(
                "registry.digest_mismatch",
                "registry committed a different immutable digest than was uploaded",
            ));
        }
        if receipt.entry.package != bundle.metadata.package.name
            || receipt.entry.version != bundle.metadata.package.version
        {
            return Err(RegistryError::new(
                "registry.digest_mismatch",
                "registry committed different package identity than was uploaded",
            ));
        }
        Ok(receipt)
    }

    /// List immutable versions and compatibility metadata for one package.
    pub async fn list(
        &self,
        publisher: &str,
        package: &str,
    ) -> Result<Vec<VersionEntry>, RegistryError> {
        let response = self
            .authorize(self.client.get(format!(
                "{}/v1{}/packages/{}/{}/versions",
                self.base,
                self.api_prefix(),
                encode_path(publisher),
                encode_path(package)
            )))
            .send()
            .await
            .map_err(transport)?;
        #[derive(serde::Deserialize)]
        struct Versions {
            versions: Vec<VersionEntry>,
        }
        let versions: Versions = self.decode(response).await?;
        Ok(versions.versions)
    }

    /// Fetch one published version.
    pub async fn show(
        &self,
        publisher: &str,
        package: &str,
        version: &str,
    ) -> Result<VersionEntry, RegistryError> {
        let response = self
            .authorize(self.client.get(format!(
                "{}/v1{}/packages/{}/{}/versions/{}",
                self.base,
                self.api_prefix(),
                encode_path(publisher),
                encode_path(package),
                encode_path(version)
            )))
            .send()
            .await
            .map_err(transport)?;
        self.decode(response).await
    }

    /// Fetch raw bytes from one registry-relative path.
    pub async fn fetch_bytes(&self, path: &str) -> Result<Vec<u8>, RegistryError> {
        let response = self
            .authorize(self.client.get(format!("{}{}", self.base, path)))
            .send()
            .await
            .map_err(transport)?;
        if !response.status().is_success() {
            return Err(self.rejection(response).await);
        }
        Ok(response.bytes().await.map_err(transport)?.to_vec())
    }

    /// Download one blob and verify its content digest.
    pub async fn download_blob(&self, path: &str, digest: &str) -> Result<Vec<u8>, RegistryError> {
        let bytes = self.fetch_bytes(path).await?;
        if common::bytes_digest(&bytes) != digest {
            return Err(RegistryError::new(
                "registry.digest_mismatch",
                "downloaded content fails digest verification",
            ));
        }
        Ok(bytes)
    }

    /// Build the registry-relative path for one content-addressed blob.
    pub fn blob_path(&self, publisher: &str, package: &str, digest: &str) -> String {
        format!(
            "/v1{}/packages/{}/{}/blobs/{}",
            self.api_prefix(),
            encode_path(publisher),
            encode_path(package),
            encode_path(digest)
        )
    }

    async fn start_upload(&self, publisher: &str, package: &str) -> Result<String, RegistryError> {
        let response = self
            .authorize(self.client.post(format!(
                "{}/v1/packages/{}/{}/uploads",
                self.base,
                encode_path(publisher),
                encode_path(package)
            )))
            .send()
            .await
            .map_err(transport)?;
        #[derive(serde::Deserialize)]
        struct Started {
            upload_id: String,
        }
        let started: Started = self.decode(response).await?;
        Ok(started.upload_id)
    }

    async fn upload_blob(
        &self,
        upload: &str,
        digest: &str,
        bytes: &[u8],
    ) -> Result<(), RegistryError> {
        let response = self
            .authorize(self.client.put(format!(
                "{}/v1/uploads/{}/blobs/{}",
                self.base,
                encode_path(upload),
                encode_path(digest)
            )))
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(self.rejection(response).await)
        }
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, RegistryError> {
        if !response.status().is_success() {
            return Err(self.rejection(response).await);
        }
        response.json().await.map_err(|_| {
            RegistryError::new("registry.invalid", "registry response is not valid JSON")
        })
    }

    async fn rejection(&self, response: reqwest::Response) -> RegistryError {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let code = value["code"].as_str().unwrap_or("");
        let message = value["message"]
            .as_str()
            .unwrap_or("registry rejected the request");
        let stable = match status.as_u16() {
            401 => "registry.unauthorized",
            403 => "registry.forbidden",
            404 => "registry.not_found",
            409 => "registry.conflict",
            413 => "registry.limit",
            422 => "registry.digest_mismatch",
            _ => "registry.unavailable",
        };
        RegistryError::new(stable, format!("{code}: {message}"))
    }
}

fn transport(error: reqwest::Error) -> RegistryError {
    RegistryError::new(
        "registry.unavailable",
        format!("registry server is not available: {}", error.without_url()),
    )
}

fn artifact_digest(index: &[u8]) -> Result<String, RegistryError> {
    let index: Value = serde_json::from_slice(index)
        .map_err(|_| RegistryError::new("registry.invalid", "artifact index is malformed"))?;
    index["manifests"][0]["digest"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| RegistryError::new("registry.invalid", "artifact index has no digest"))
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

fn query_string(query: &PublicQuery) -> String {
    let values = [
        ("publisher", query.publisher.as_deref()),
        ("package", query.package.as_deref()),
        ("capability", query.capability.as_deref()),
        ("target", query.target.as_deref()),
        ("runtime", query.runtime.as_deref()),
        ("adk", query.adk.as_deref()),
        ("protocol", query.protocol.as_deref()),
    ];
    let mut parts = Vec::new();
    for (key, value) in values {
        if let Some(value) = value {
            parts.push(format!("{key}={}", encode_path(value)));
        }
    }
    if let Some(limit) = query.limit {
        parts.push(format!("limit={limit}"));
    }
    parts.join("&")
}
