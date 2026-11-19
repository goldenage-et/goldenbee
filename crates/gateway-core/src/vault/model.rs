//! Persisted provider-vault document.
//!
//! Every field here is safe to write to the vault file. Credential plaintext is
//! never part of this shape; only ciphertext, nonce, fingerprint, and version
//! metadata are stored.

use common::ProviderMetadata;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(super) const VAULT_SCHEMA: u64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Document {
    #[serde(default)]
    pub(super) version: u64,
    /// One master-key check value. It proves a supplied key opens this document
    /// before any credential is touched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) master_check: Option<StoredSecret>,
    #[serde(default)]
    pub(super) providers: BTreeMap<String, ProviderRecord>,
    #[serde(default)]
    pub(super) workers: BTreeMap<String, WorkerRecord>,
    #[serde(default)]
    pub(super) pair_codes: BTreeMap<String, PairCodeRecord>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: VAULT_SCHEMA,
            master_check: None,
            providers: BTreeMap::new(),
            workers: BTreeMap::new(),
            pair_codes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ProviderRecord {
    pub(super) metadata: ProviderMetadata,
    #[serde(default)]
    pub(super) secrets: BTreeMap<u64, SecretVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SecretVersion {
    pub(super) nonce: String,
    pub(super) ciphertext: String,
    pub(super) fingerprint: String,
    pub(super) created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) revoked_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredSecret {
    pub(super) nonce: String,
    pub(super) ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct WorkerRecord {
    pub(super) worker_id: String,
    pub(super) instance: String,
    /// Base64 X25519 public key.
    pub(super) public_key: String,
    pub(super) enrolled_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PairCodeRecord {
    pub(super) worker_id: String,
    pub(super) instance: String,
    pub(super) expires_at: i64,
    pub(super) created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) used_at: Option<i64>,
}

impl ProviderRecord {
    pub(super) fn active_secret(&self) -> Option<(u64, &SecretVersion)> {
        self.secrets
            .iter()
            .rev()
            .find(|(_, secret)| secret.revoked_at.is_none())
            .map(|(version, secret)| (*version, secret))
    }
}
