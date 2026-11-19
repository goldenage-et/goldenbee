//! Retained registry audit decisions.

use common::RegistryAuditEntry;

use super::model::Document;
use super::Registry;

impl Registry {
    /// Retained audit decisions, newest last.
    pub fn audit_entries(&self, worker: Option<&str>) -> Vec<RegistryAuditEntry> {
        self.lock()
            .document
            .audit
            .iter()
            .filter(|entry| worker.is_none_or(|worker| entry.worker == worker))
            .cloned()
            .collect()
    }

    /// Audit a rejected control or operator decision.
    pub fn audit(
        &self,
        now: i64,
        action: &str,
        worker: &str,
        principal: &str,
        outcome: &str,
        detail: &str,
    ) {
        let mut document = self.lock();
        let policy_revision = document.document.version as u64;
        document.document.audit.push(RegistryAuditEntry {
            at: now,
            action: action.into(),
            worker: worker.into(),
            principal: principal.into(),
            outcome: outcome.into(),
            detail: detail.into(),
            source: "gateway".into(),
            policy_revision,
            correlation: String::new(),
        });
        trim_audit(&mut document.document.audit);
        let _ = self.write(&document.document);
    }
}

pub(super) fn trim_audit(audit: &mut Vec<RegistryAuditEntry>) {
    const LIMIT: usize = 2_000;
    if audit.len() > LIMIT {
        let excess = audit.len() - LIMIT;
        audit.drain(0..excess);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_audit(
    document: &mut Document,
    at: i64,
    action: &str,
    worker: &str,
    principal: &str,
    outcome: &str,
    detail: &str,
) {
    let policy_revision = document.version as u64;
    document.audit.push(RegistryAuditEntry {
        at,
        action: action.into(),
        worker: worker.into(),
        principal: principal.into(),
        outcome: outcome.into(),
        detail: detail.into(),
        source: "gateway".into(),
        policy_revision,
        correlation: String::new(),
    });
    trim_audit(&mut document.audit);
}
