//! Durable audit records and trace samples.

use crate::core::store::{AuditRecord, State};
use crate::{Result, Runtime};
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

    /// Add a trace sample.
    pub async fn trace(
        &self,
        trace_id: &str,
        task_id: &str,
        span: &str,
        duration_ms: u64,
        attributes: BTreeMap<String, String>,
    ) -> Result<()> {
        self.store()
            .write(|state| {
                state.traces.push(common::TraceSample {
                    trace_id: trace_id.to_string(),
                    task_id: task_id.to_string(),
                    span: span.to_string(),
                    duration_ms,
                    attributes: attributes
                        .into_iter()
                        .map(|(key, value)| (key, common::redact(&value)))
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
        principal: principal.to_string(),
        action: action.to_string(),
        target: target.to_string(),
        outcome: outcome.to_string(),
        detail: common::redact(detail),
        source: source.to_string(),
        policy_revision,
        correlation,
    });
}
