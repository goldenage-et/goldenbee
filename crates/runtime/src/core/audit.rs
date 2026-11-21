//! Durable audit records and trace samples.

use crate::core::redaction::Redactor;
use crate::core::store::{AuditRecord, State};
use crate::{Result, Runtime};
use common::AuditEntry;
use std::collections::BTreeMap;

impl Runtime {
    /// Record an audit action.
    pub async fn audit(
        &self,
        principal: &str,
        action: &str,
        target: &str,
        outcome: &str,
        detail: &str,
    ) -> Result<()> {
        self.audit_with_trace(principal, action, target, outcome, detail, "")
            .await
    }

    /// Record an audit action with an explicit correlation.
    pub async fn audit_with_trace(
        &self,
        principal: &str,
        action: &str,
        target: &str,
        outcome: &str,
        detail: &str,
        correlation: &str,
    ) -> Result<()> {
        let source = self.config().instance_id.clone();
        let policy_revision = self.config_revision();
        let redactor = self.redactor();
        self.store()
            .write(|state| {
                push_audit(
                    state,
                    principal,
                    action,
                    target,
                    outcome,
                    detail,
                    &source,
                    policy_revision,
                    correlation,
                    &redactor,
                );
                Ok(())
            })
            .await
    }

    /// Read audit records, filtered by target prefix.
    pub async fn audit_records(&self, target: &str) -> Vec<AuditRecord> {
        self.store()
            .read(|state| {
                state
                    .audit
                    .iter()
                    .filter(|record| record.target.starts_with(target))
                    .cloned()
                    .collect()
            })
            .await
    }

    /// Read audit evidence for one task as a typed, redacted projection.
    ///
    /// Records match when they target the task directly or when their trace
    /// correlation belongs to the task. The projection names the principal,
    /// action, decision, reason, and affected resource explicitly.
    pub async fn audit_entries(
        &self,
        principal: &common::Principal,
        task_id: &str,
        limit: usize,
    ) -> Result<Vec<AuditEntry>> {
        use common::scopes;
        principal.require(scopes::TASK_READ)?;
        let task = self.task(principal, task_id).await?;
        let redactor = self.redactor();
        let limit = limit.clamp(1, 1_000);
        let entries = self
            .store()
            .read(|state| {
                state
                    .audit
                    .iter()
                    .rev()
                    .filter(|record| {
                        record.target == task.id
                            || (!task.trace_id.is_empty() && record.correlation == task.trace_id)
                    })
                    .take(limit)
                    .map(|record| audit_entry(record, &redactor))
                    .collect::<Vec<_>>()
            })
            .await;
        let mut entries = entries;
        entries.sort_by_key(|entry| entry.seq);
        Ok(entries)
    }

    /// Add a trace sample.
    pub async fn trace(
        &self,
        trace_id: &str,
        task_id: &str,
        span: &str,
        duration_ms: u64,
        attributes: BTreeMap<String, String>,
    ) -> Result<()> {
        let redactor = self.redactor();
        self.store()
            .write(|state| {
                state.traces.push(common::TraceSample {
                    trace_id: trace_id.to_string(),
                    task_id: task_id.to_string(),
                    span: span.to_string(),
                    duration_ms,
                    attributes: attributes
                        .into_iter()
                        .map(|(key, value)| (key, redactor.text(&value)))
                        .collect(),
                    at: common::now_ms(),
                });
                if state.traces.len() > 5_000 {
                    state.traces.remove(0);
                }
                Ok(())
            })
            .await
    }

    /// Read traces for a task.
    pub async fn traces_for(&self, task_id: &str) -> Vec<common::TraceSample> {
        self.store()
            .read(|state| {
                state
                    .traces
                    .iter()
                    .filter(|sample| sample.task_id == task_id || task_id.is_empty())
                    .cloned()
                    .collect()
            })
            .await
    }
}

/// Append an audit record inside an existing metadata transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_audit(
    state: &mut State,
    principal: &str,
    action: &str,
    target: &str,
    outcome: &str,
    detail: &str,
    source: &str,
    policy_revision: u64,
    correlation: &str,
    redactor: &Redactor,
) {
    state.counters.audit += 1;
    let seq = state.counters.audit;
    let correlation = if correlation.is_empty() {
        state
            .tasks
            .get(target)
            .map(|task| task.trace_id.clone())
            .unwrap_or_default()
    } else {
        common::trace_id(Some(correlation))
    };
    state.audit.push(AuditRecord {
        seq,
        at: common::now_ms(),
        principal: redactor.text(principal),
        action: action.to_string(),
        target: redactor.text(target),
        outcome: outcome.to_string(),
        detail: redactor.text(detail),
        source: source.to_string(),
        policy_revision,
        correlation,
    });
}

fn audit_entry(record: &AuditRecord, redactor: &Redactor) -> AuditEntry {
    AuditEntry {
        seq: record.seq,
        at: record.at,
        principal: redactor.text(&record.principal),
        action: record.action.clone(),
        decision: record.outcome.clone(),
        reason: redactor.text(&record.detail),
        resource: redactor.text(&record.target),
        source: record.source.clone(),
        policy_revision: record.policy_revision,
        trace_id: Some(record.correlation.clone()).filter(|trace| !trace.is_empty()),
    }
}
