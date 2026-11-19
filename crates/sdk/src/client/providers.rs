//! Gateway-managed model-provider administration.
//!
//! Credentials are write-only. The SDK accepts a key only from an owned
//! request and never returns, formats, or logs the value.

use super::transport::{delete, get, patch, post, post_empty, put};
use super::Client;
use common::{
    ApiError, CreatePairCodeRequest, CreateProviderRequest, PairCodeResponse,
    ProviderCredentialSummary, ProviderMetadata, UpdateProviderRequest,
};

fn encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

impl Client {
    /// Non-secret provider catalog.
    pub async fn providers(&self) -> Result<Vec<ProviderMetadata>, ApiError> {
        get(self, "/v1/admin/model-providers").await
    }

    /// One provider catalog entry.
    pub async fn provider(&self, id: &str) -> Result<ProviderMetadata, ApiError> {
        get(self, &format!("/v1/admin/model-providers/{}", encode(id))).await
    }

    /// Create a provider and run discovery when the adapter supports it.
    pub async fn create_provider(
        &self,
        request: &CreateProviderRequest,
    ) -> Result<ProviderMetadata, ApiError> {
        post(self, "/v1/admin/model-providers", request).await
    }

    /// Update metadata using the revision the caller observed.
    pub async fn update_provider(
        &self,
        id: &str,
        request: &UpdateProviderRequest,
    ) -> Result<ProviderMetadata, ApiError> {
        patch(
            self,
            &format!("/v1/admin/model-providers/{}", encode(id)),
            request,
        )
        .await
    }

    /// Refresh discovered models.
    pub async fn discover_provider(&self, id: &str) -> Result<ProviderMetadata, ApiError> {
        post_empty(
            self,
            &format!("/v1/admin/model-providers/{}/discover", encode(id)),
        )
        .await
    }

    /// Write one provider credential. The key travels only in the JSON body.
    pub async fn set_provider_credential(
        &self,
        id: &str,
        key: &str,
    ) -> Result<ProviderCredentialSummary, ApiError> {
        #[derive(serde::Serialize)]
        struct Credential<'a> {
            key: &'a str,
        }
        put(
            self,
            &format!("/v1/admin/model-providers/{}/credential", encode(id)),
            &Credential { key },
        )
        .await
    }

    /// Revoke the active provider credential.
    pub async fn revoke_provider_credential(
        &self,
        id: &str,
    ) -> Result<ProviderCredentialSummary, ApiError> {
        delete(
            self,
            &format!("/v1/admin/model-providers/{}/credential", encode(id)),
        )
        .await
    }

    /// Mint a one-time Worker enrollment code for the provider vault.
    pub async fn provider_pair_code(
        &self,
        worker: &str,
        instance: &str,
    ) -> Result<PairCodeResponse, ApiError> {
        post(
            self,
            "/v1/admin/model-providers/pair-code",
            &CreatePairCodeRequest {
                worker_id: worker.into(),
                instance: instance.into(),
            },
        )
        .await
    }
}
