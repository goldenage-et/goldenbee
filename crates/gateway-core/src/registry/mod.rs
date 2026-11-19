//! Gateway-owned Worker registry: approvals, leases, discovery, and audit.
//!
//! Registry state belongs to the Gateway. Worker execution, sessions, and task
//! outcomes stay authoritative in the Worker. Lease expiry changes only the
//! connectivity projection in this module.
//!
//! Operator configuration seeds approvals on first startup. Afterwards the
//! registry document is authoritative: runtime edits are never silently
//! overwritten by a later configuration seed. Control tokens are stored only as
//! SHA-256 hashes.

mod approvals;
mod audit;
mod capabilities;
mod discovery;
mod model;
mod persistence;
mod registration;
mod teams;
#[cfg(test)]
mod tests;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use common::ApiError;

use self::model::Inner;
pub(crate) use self::model::{
    Approval, ApprovalPatch, ApprovalRecord, Document, NewApproval, Verified, REGISTRY_SCHEMA,
};
use self::persistence::{
    build_control, hash_token, persistence_error, read_document, restrict_permissions, sync_parent,
};

/// Gateway-side time source. Tests inject a controlled clock.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Persistent Gateway registry.
pub struct Registry {
    clock: Clock,
    lease_ttl_ms: i64,
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl Registry {
    /// Load registry state, seed config-owned approvals, and expire stale leases.
    pub fn open(
        path: Option<PathBuf>,
        clock: Clock,
        lease_ttl_ms: i64,
        approvals: Vec<Approval>,
    ) -> Result<Self, ApiError> {
        let now = (clock)();
        let mut document = match &path {
            Some(path) if path.exists() => read_document(path)?,
            _ => Document::default(),
        };
        if document.version > REGISTRY_SCHEMA {
            return Err(ApiError::new(
                "registry.schema_unsupported",
                format!(
                    "registry schema {} is newer than supported {}",
                    document.version, REGISTRY_SCHEMA
                ),
            ));
        }
        document.version = REGISTRY_SCHEMA;
        self::approvals::seed_approvals(&mut document, approvals, now)?;
        let control = build_control(&document)?;
        self::registration::expire_document(&mut document, now);
        let registry = Self {
            clock,
            lease_ttl_ms: lease_ttl_ms.max(1),
            path,
            inner: Mutex::new(Inner { document, control }),
        };
        let snapshot = registry.lock().document.clone();
        registry.write(&snapshot)?;
        Ok(registry)
    }

    /// Current Gateway-side time.
    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    /// Rebuild a registry over an existing document without touching a file.
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    /// Shared adapters use this per transaction; it never becomes authoritative
    /// process state.
    pub(crate) fn from_document(
        document: Document,
        clock: Clock,
        lease_ttl_ms: i64,
    ) -> Result<Self, ApiError> {
        let mut document = document;
        document.version = REGISTRY_SCHEMA;
        let control = build_control(&document)?;
        Ok(Self {
            clock,
            lease_ttl_ms: lease_ttl_ms.max(1),
            path: None,
            inner: Mutex::new(Inner { document, control }),
        })
    }

    /// Clone the current document for a durable adapter.
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    pub(crate) fn snapshot(&self) -> Document {
        self.lock().document.clone()
    }

    /// Seed config-owned approvals after a shared adapter acquired the row.
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    pub(crate) fn seed_config(&self, approvals: Vec<Approval>, now: i64) -> Result<(), ApiError> {
        self.mutate(|document| self::approvals::seed_approvals(document, approvals, now))
    }

    /// Resolve a Worker control token to its approved identity without
    /// retaining the plain token.
    pub fn control_worker(&self, token: &str) -> Option<String> {
        self.lock().control.get(&hash_token(token)).cloned()
    }

    /// Clone the current approval for an identity.
    pub fn approval_record(&self, id: &str) -> Option<ApprovalRecord> {
        self.lock().document.approvals.get(id).cloned()
    }
    fn approval_or_not_found(&self, id: &str) -> Result<ApprovalRecord, ApiError> {
        self.lock()
            .document
            .approvals
            .get(id)
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    "worker.not_found",
                    format!("Worker '{id}' is not approved by this Gateway"),
                )
            })
    }

    fn mutate<T>(
        &self,
        change: impl FnOnce(&mut Document) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let mut inner = self.lock();
        let value = change(&mut inner.document)?;
        self::audit::trim_audit(&mut inner.document.audit);
        inner.control = build_control(&inner.document)?;
        self.write(&inner.document)?;
        Ok(value)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn write(&self, document: &Document) -> Result<(), ApiError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(persistence_error)?;
        let bytes = serde_json::to_vec(document).map_err(|error| {
            ApiError::new(
                "storage.unavailable",
                format!("registry state could not be serialized: {error}"),
            )
        })?;
        let temporary = path.with_extension("tmp");
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            restrict_permissions(&temporary)?;
            std::fs::rename(&temporary, path)?;
            restrict_permissions(path)?;
            sync_parent(path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(persistence_error)
    }
}
