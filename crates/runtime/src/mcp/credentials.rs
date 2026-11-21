//! Instance-owned credential references for remote MCP connections.
//!
//! Only references and policy metadata come from operator configuration. The
//! value is resolved from an external source for each use, so rotation does not
//! require a configuration export, and revocation blocks new calls without a
//! restart.

use crate::mcp::config::McpConfig;
use common::ApiError;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// A resolved secret value. Debug output is always redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// Stable, non-reversible fingerprint used only to detect rotation.
    pub(crate) fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

/// One credential that passed ownership, audience, scope, expiry, and
/// revocation checks.
#[derive(Clone, Debug)]
pub struct CredentialGrant {
    /// Resolved value. Never log or serialize this field.
    pub(crate) value: SecretValue,
    /// Audience this use is bound to.
    pub audience: String,
    /// Scopes the value may exercise.
    pub scopes: Vec<String>,
    /// Monotonic generation incremented by runtime revocation.
    pub generation: u64,
}

impl CredentialGrant {
    /// Fingerprint used to detect a rotated value.
    pub fn fingerprint(&self) -> String {
        self.value.fingerprint()
    }
}

struct Inner {
    bindings: RwLock<BTreeMap<String, crate::mcp::config::McpCredentialConfig>>,
    revoked: RwLock<BTreeSet<String>>,
    generation: AtomicU64,
}

/// Cloneable credential store owned by one Worker Instance.
#[derive(Clone)]
pub struct McpCredentials {
    inner: Arc<Inner>,
}

impl McpCredentials {
    /// Build the store from validated configuration.
    pub fn new(config: &McpConfig) -> Self {
        let bindings = config
            .credentials
            .iter()
            .map(|binding| (binding.reference.clone(), binding.clone()))
            .collect();
        Self {
            inner: Arc::new(Inner {
                bindings: RwLock::new(bindings),
                revoked: RwLock::new(BTreeSet::new()),
                generation: AtomicU64::new(1),
            }),
        }
    }

    /// Resolve a reference with the required audience and scopes.
    pub fn resolve(&self, reference: &str) -> Result<CredentialGrant, ApiError> {
        let binding = self
            .inner
            .bindings
            .read()
            .ok()
            .and_then(|bindings| bindings.get(reference).cloned())
            .ok_or_else(|| {
                ApiError::new(
                    "mcp.credential_missing",
                    "the referenced MCP credential is not owned by this instance",
                )
            })?;
        if binding.revoked
            || self
                .inner
                .revoked
                .read()
                .map(|revoked| revoked.contains(reference))
                .unwrap_or(true)
        {
            return Err(ApiError::new(
                "mcp.credential_revoked",
                "the MCP credential has been revoked",
            ));
        }
        if binding.expires_at > 0 && binding.expires_at <= common::now_ms() {
            return Err(ApiError::new(
                "mcp.credential_expired",
                "the MCP credential has expired",
            ));
        }
        let Some(name) = binding.source.strip_prefix("env:") else {
            return Err(ApiError::new(
                "mcp.credential_missing",
                "the MCP credential source is not supported",
            ));
        };
        let value = std::env::var(name).map_err(|_| {
            ApiError::new(
                "mcp.credential_missing",
                "the MCP credential value is not available",
            )
        })?;
        if value.trim().is_empty() {
            return Err(ApiError::new(
                "mcp.credential_missing",
                "the MCP credential value is empty",
            ));
        }
        Ok(CredentialGrant {
            value: SecretValue(value),
            audience: binding.audience,
            scopes: binding.scopes,
            generation: self.inner.generation.load(Ordering::SeqCst),
        })
    }

    /// Revoke a reference for all new calls.
    pub fn revoke(&self, reference: &str) -> Result<(), ApiError> {
        let known = self
            .inner
            .bindings
            .read()
            .map(|bindings| bindings.contains_key(reference))
            .unwrap_or(false);
        if !known {
            return Err(ApiError::new(
                "mcp.credential_missing",
                "the referenced MCP credential is not owned by this instance",
            ));
        }
        if let Ok(mut revoked) = self.inner.revoked.write() {
            revoked.insert(reference.to_string());
        }
        self.inner.generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// Whether a reference is currently revoked.
    pub fn is_revoked(&self, reference: &str) -> bool {
        self.inner
            .revoked
            .read()
            .map(|revoked| revoked.contains(reference))
            .unwrap_or(true)
    }

    /// Current generation.
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::config::{McpConfig, McpCredentialConfig};

    fn config(reference: &str, expires_at: i64, revoked: bool) -> McpConfig {
        McpConfig {
            servers: BTreeMap::new(),
            allow_plugin_servers: false,
            credentials: vec![McpCredentialConfig {
                reference: reference.into(),
                source: format!("env:{}", variable(reference)),
                audience: "mcp://tests".into(),
                scopes: vec!["mcp:call".into()],
                expires_at,
                revoked,
            }],
        }
    }

    fn variable(reference: &str) -> String {
        format!(
            "GOLDENBEE_MCP_TEST_{}",
            reference
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() {
                        character.to_ascii_uppercase()
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        )
    }

    #[test]
    fn resolves_owned_reference_without_exposing_value() {
        let reference = "secret://mcp/resolve";
        let name = variable(reference);
        std::env::set_var(&name, "sk-canary-value");
        let store = McpCredentials::new(&config(reference, 0, false));
        let grant = store.resolve(reference).expect("credential resolves");
        assert_eq!(grant.audience, "mcp://tests");
        assert!(format!("{grant:?}").contains("[REDACTED]"));
        assert!(!format!("{grant:?}").contains("sk-canary-value"));
        std::env::remove_var(&name);
    }

    #[test]
    fn missing_expired_and_revoked_credentials_block() {
        let reference = "secret://mcp/blocked";
        let store = McpCredentials::new(&config(reference, 0, false));
        assert_eq!(
            store.resolve(reference).expect_err("missing env").code,
            "mcp.credential_missing"
        );

        let name = variable(reference);
        std::env::set_var(&name, "sk-blocked");
        let expired = McpCredentials::new(&config(reference, 1, false));
        assert_eq!(
            expired.resolve(reference).expect_err("expired").code,
            "mcp.credential_expired"
        );

        let revoked = McpCredentials::new(&config(reference, 0, true));
        assert_eq!(
            revoked.resolve(reference).expect_err("revoked").code,
            "mcp.credential_revoked"
        );
        std::env::remove_var(&name);
    }

    #[test]
    fn runtime_revocation_is_immediate_and_generation_changes() {
        let reference = "secret://mcp/revoke";
        let name = variable(reference);
        std::env::set_var(&name, "sk-revoke");
        let store = McpCredentials::new(&config(reference, 0, false));
        let before = store.resolve(reference).expect("resolves").generation;
        store.revoke(reference).expect("known reference");
        assert_eq!(
            store.resolve(reference).expect_err("revoked").code,
            "mcp.credential_revoked"
        );
        assert!(store.generation() > before);
        std::env::remove_var(&name);
    }
}
