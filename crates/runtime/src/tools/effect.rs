//! Protected native tool adapter for external side effects.
//!
//! A Worker Package registers a normal native `Tool` with
//! [`Worker::effect_tool`](crate::worker::Worker::effect_tool). The adapter
//! persists an operation identity before dispatch and records a receipt only
//! after a confirmed response. Ambiguous outcomes are handed to the registered
//! status or idempotency interface and are never automatically repeated when
//! that interface cannot prove absence or definite failure.

use crate::execution::operations::{
    BeginOutcome, EffectPolicy, EffectReconciler, EffectStatus, STATUS_ABSENT, STATUS_FAILED,
    STATUS_UNKNOWN,
};
use crate::execution::runner::decode_native_user_id;
use crate::{OperationRecord, Runtime};
use adk_rust::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Native tool plus the Worker Instance's durable effect policy.
pub struct EffectTool {
    inner: Arc<dyn Tool>,
    reconciler: Arc<dyn EffectReconciler>,
    policy: EffectPolicy,
    runtime: Arc<OnceLock<Runtime>>,
}

enum KnownAction {
    Retry(OperationRecord),
    Done(Value),
}

impl EffectTool {
    /// Wrap one registered native tool.
    ///
    /// The runtime cell is bound by Worker startup after durable state opens.
    /// Before that point every invocation fails closed instead of dispatching
    /// an untracked effect.
    pub fn new(
        inner: Arc<dyn Tool>,
        reconciler: Arc<dyn EffectReconciler>,
        policy: EffectPolicy,
        runtime: Arc<OnceLock<Runtime>>,
    ) -> Self {
        Self {
            inner,
            reconciler,
            policy,
            runtime,
        }
    }

    fn bound(&self) -> std::result::Result<Runtime, Value> {
        self.runtime.get().cloned().ok_or_else(|| {
            json!({
                "error": "effect.unbound",
                "message": "the Worker runtime is not available for effect dispatch",
            })
        })
    }

    fn active_task<'a>(
        &self,
        runtime: &'a Runtime,
        ctx: &dyn ToolContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<common::Task, Value>> + Send + 'a>,
    > {
        let session = ctx.session_id().to_string();
        let user = ctx.user_id().to_string();
        Box::pin(async move {
            let (namespace, caller) = decode_native_user_id(&user).map_err(|error| {
                json!({
                    "error": "effect.task_unbound",
                    "message": common::redact(&error.message),
                })
            })?;
            runtime
                .effect_task(&session, &namespace, &caller)
                .await
                .ok_or_else(|| {
                    json!({
                        "error": "effect.task_unbound",
                        "message": "a protected effect must execute inside an admitted running task",
                    })
                })
        })
    }

    /// Ensure the exact operation has a live approval before dispatch.
    ///
    /// The native confirmation handler normally performs this wait before the
    /// tool is called. This check is the fail-closed tool-boundary guard: a
    /// custom agent that bypasses native confirmation still pauses here, and a
    /// rejected, revoked, expired, or changed operation can never reach the
    /// external effect.
    async fn ensure_approval(
        &self,
        runtime: &Runtime,
        task: &common::Task,
        canonical: &Value,
        digest: &str,
    ) -> std::result::Result<Option<common::Approval>, Value> {
        let Some(policy) = self.policy.approval.as_ref() else {
            return Ok(None);
        };
        let target = policy.target(canonical);
        loop {
            match runtime
                .approval_for_operation(&task.id, self.inner.name(), digest)
                .await
            {
                Some(approval) if approval.status == "approved" => {
                    if common::now_ms() < approval.expires_at {
                        return Ok(Some(approval));
                    }
                    return Err(json!({
                        "error": "effect.approval_expired",
                        "approval": approval.id,
                        "operation": self.inner.name(),
                        "message": "the approval expired before the effect was dispatched",
                    }));
                }
                Some(approval) if approval.status == "pending" => {
                    let remaining = (approval.expires_at - common::now_ms()).max(1) as u64;
                    match runtime
                        .wait_for_approval(&approval.id, Duration::from_millis(remaining))
                        .await
                    {
                        Ok(approval) if approval.status == "approved" => {
                            if common::now_ms() < approval.expires_at {
                                return Ok(Some(approval));
                            }
                            return Err(json!({
                                "error": "effect.approval_expired",
                                "approval": approval.id,
                                "operation": self.inner.name(),
                                "message": "the approval expired before the effect was dispatched",
                            }));
                        }
                        _ => {
                            return Err(json!({
                                "error": "effect.approval_denied",
                                "approval": approval.id,
                                "operation": self.inner.name(),
                                "message": "the protected operation was not approved",
                            }))
                        }
                    }
                }
                Some(approval) => {
                    return Err(json!({
                        "error": "effect.approval_denied",
                        "approval": approval.id,
                        "status": approval.status,
                        "operation": self.inner.name(),
                        "message": "the recorded approval does not authorize this operation",
                    }));
                }
                None => {
                    let approval = runtime
                        .request_approval(
                            &task.id,
                            self.inner.name(),
                            &target,
                            digest,
                            &policy.risk,
                            policy.ttl_ms,
                        )
                        .await
                        .map_err(|error| api_error_value(&error))?;
                    if !matches!(approval.status.as_str(), "pending" | "approved") {
                        return Err(json!({
                            "error": "effect.approval_denied",
                            "approval": approval.id,
                            "status": approval.status,
                            "operation": self.inner.name(),
                            "message": "the recorded approval does not authorize this operation",
                        }));
                    }
                }
            }
        }
    }

    async fn finish_known(
        &self,
        runtime: &Runtime,
        task_id: &str,
        operation: &OperationRecord,
        status: &str,
        detail: &str,
    ) -> Result<KnownAction> {
        if self.policy.permits(status) && operation.attempt < self.policy.max_attempts.max(1) {
            // Persist the known outcome first; only a persisted known-absent
            // or definite-failed operation may advance to another attempt.
            if runtime
                .resolve_operation(&operation.id, status, None, detail)
                .await
                .is_ok()
            {
                if let Ok(retried) = runtime
                    .retry_reserved_operation(&operation.id, &self.policy)
                    .await
                {
                    let _ = runtime
                        .append_event(
                            task_id,
                            "operation.retry",
                            json!({
                                "operation": retried.id,
                                "tool": retried.tool,
                                "attempt": retried.attempt,
                                "previous_status": status,
                            }),
                            true,
                        )
                        .await;
                    return Ok(KnownAction::Retry(retried));
                }
            }
        }
        let resolved = runtime
            .resolve_operation(&operation.id, status, None, detail)
            .await
            .map_err(|error| {
                adk_rust::AdkError::new(
                    adk_rust::ErrorComponent::Tool,
                    adk_rust::ErrorCategory::Internal,
                    "effect.state_unavailable",
                    common::redact(&error.message),
                )
            })?;
        let _ = runtime
            .append_event(
                task_id,
                "operation.failed",
                json!({
                    "operation": resolved.id,
                    "tool": resolved.tool,
                    "attempt": resolved.attempt,
                    "status": resolved.status,
                }),
                true,
            )
            .await;
        Ok(KnownAction::Done(json!({
            "error": if status == STATUS_ABSENT { "effect.absent" } else { "effect.failed" },
            "operation": resolved.id,
            "message": common::redact(detail),
        })))
    }
}

#[async_trait]
impl Tool for EffectTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn declaration(&self) -> Value {
        self.inner.declaration()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    /// A protected effect is never read-only and never concurrency-safe.
    fn is_read_only(&self) -> bool {
        false
    }

    fn is_concurrency_safe(&self) -> bool {
        false
    }

    fn is_agent_delegation(&self) -> bool {
        self.inner.is_agent_delegation()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let runtime = match self.bound() {
            Ok(runtime) => runtime,
            Err(value) => return Ok(value),
        };
        let task = match self.active_task(&runtime, ctx.as_ref()).await {
            Ok(task) => task,
            Err(value) => return Ok(value),
        };
        let canonical = canonical_arguments(&args);
        let digest = effect_digest(self.inner.name(), &canonical);
        // A committed operation is replayed from its receipt and never needs a
        // fresh approval, even after the original approval window closes.
        let already_committed = runtime
            .operations_for(&task.id)
            .await
            .iter()
            .any(|operation| {
                operation.tool == self.inner.name()
                    && operation.digest == digest
                    && operation.status == "committed"
            });
        let approval = if already_committed {
            None
        } else {
            match self
                .ensure_approval(&runtime, &task, &canonical, &digest)
                .await
            {
                Ok(approval) => approval,
                Err(value) => {
                    // The wait released the execution slot; the native turn
                    // continues after this denial or expiry result.
                    runtime.set_pause(&task.id, crate::PAUSE_RUNNING).await;
                    return Ok(value);
                }
            }
        };
        let invocation = ctx.invocation_id().to_string();
        let begin = match runtime
            .begin_operation(
                &task.id,
                self.inner.name(),
                &digest,
                Some(&invocation),
                &self.policy,
            )
            .await
        {
            Ok(begin) => begin,
            Err(error) => return Ok(api_error_value(&error)),
        };

        match begin {
            BeginOutcome::Committed(operation) => {
                let _ = runtime
                    .append_event(
                        &task.id,
                        "operation.replayed",
                        json!({
                            "operation": operation.id,
                            "tool": operation.tool,
                            "attempt": operation.attempt,
                        }),
                        true,
                    )
                    .await;
                Ok(receipt_value(&operation))
            }
            BeginOutcome::Blocked { operation, reason } => {
                if operation.status == STATUS_UNKNOWN {
                    runtime.block_task_for_effect(&task.id).await;
                }
                Ok(json!({
                    "error": "effect.blocked",
                    "status": operation.status,
                    "operation": operation.id,
                    "message": common::redact(&reason),
                }))
            }
            BeginOutcome::Dispatch(mut operation) => {
                let _ = runtime
                    .append_event(
                        &task.id,
                        "operation.reserved",
                        json!({
                            "operation": operation.id,
                            "tool": operation.tool,
                            "attempt": operation.attempt,
                            "invocation": operation.invocation_id,
                        }),
                        true,
                    )
                    .await;
                crate::core::fault::exit_if(crate::core::fault::Point::AfterEffectReservation);
                loop {
                    let mut dispatch_args = args.clone();
                    if let Some(object) = dispatch_args.as_object_mut() {
                        object.insert(
                            "operation_key".into(),
                            Value::String(operation.idempotency_key.clone()),
                        );
                    }
                    let result = self.inner.execute(ctx.clone(), dispatch_args).await;
                    match result {
                        Ok(value) => {
                            crate::core::fault::exit_if(
                                crate::core::fault::Point::AfterEffectDispatch,
                            );
                            let (external_ref, receipt) = confirmed_details(&value);
                            let committed = match runtime
                                .complete_operation(
                                    &operation.id,
                                    external_ref.as_deref(),
                                    &receipt,
                                )
                                .await
                            {
                                Ok(committed) => committed,
                                Err(error) => return Ok(api_error_value(&error)),
                            };
                            crate::core::fault::exit_if(
                                crate::core::fault::Point::AfterEffectReceipt,
                            );
                            if let Some(approval) = &approval {
                                let _ = runtime
                                    .record_approval_receipt(
                                        &approval.id,
                                        committed
                                            .receipt
                                            .as_deref()
                                            .or(committed.external_ref.as_deref())
                                            .unwrap_or("protected effect committed"),
                                    )
                                    .await;
                            }
                            let _ = runtime
                                .append_event(
                                    &task.id,
                                    "operation.committed",
                                    json!({
                                        "operation": committed.id,
                                        "tool": committed.tool,
                                        "attempt": committed.attempt,
                                        "external_ref": committed.external_ref,
                                    }),
                                    true,
                                )
                                .await;
                            return Ok(value);
                        }
                        Err(error) => {
                            let status = match classify_error(&error) {
                                Some(status) => status,
                                None => self.reconciler.status(&operation).await,
                            };
                            match status {
                                EffectStatus::Committed {
                                    external_ref,
                                    receipt,
                                } => {
                                    let detail = receipt.unwrap_or_else(|| {
                                        "external status confirmed commitment".into()
                                    });
                                    let committed = match runtime
                                        .complete_operation(
                                            &operation.id,
                                            external_ref.as_deref(),
                                            &detail,
                                        )
                                        .await
                                    {
                                        Ok(committed) => committed,
                                        Err(error) => return Ok(api_error_value(&error)),
                                    };
                                    crate::core::fault::exit_if(
                                        crate::core::fault::Point::AfterEffectReceipt,
                                    );
                                    if let Some(approval) = &approval {
                                        let _ = runtime
                                            .record_approval_receipt(
                                                &approval.id,
                                                committed
                                                    .receipt
                                                    .as_deref()
                                                    .or(committed.external_ref.as_deref())
                                                    .unwrap_or("protected effect committed"),
                                            )
                                            .await;
                                    }
                                    let _ = runtime
                                        .append_event(
                                            &task.id,
                                            "operation.committed",
                                            json!({
                                                "operation": committed.id,
                                                "tool": committed.tool,
                                                "attempt": committed.attempt,
                                                "external_ref": committed.external_ref,
                                                "reconciled": true,
                                            }),
                                            true,
                                        )
                                        .await;
                                    return Ok(receipt_value(&committed));
                                }
                                EffectStatus::Absent => {
                                    let detail = "external status confirms no effect exists";
                                    match self
                                        .finish_known(
                                            &runtime,
                                            &task.id,
                                            &operation,
                                            STATUS_ABSENT,
                                            detail,
                                        )
                                        .await
                                    {
                                        Ok(KnownAction::Retry(retried)) => operation = retried,
                                        Ok(KnownAction::Done(value)) => return Ok(value),
                                        Err(error) => return Err(error),
                                    }
                                }
                                EffectStatus::Failed { detail } => {
                                    match self
                                        .finish_known(
                                            &runtime,
                                            &task.id,
                                            &operation,
                                            STATUS_FAILED,
                                            &detail,
                                        )
                                        .await
                                    {
                                        Ok(KnownAction::Retry(retried)) => operation = retried,
                                        Ok(KnownAction::Done(value)) => return Ok(value),
                                        Err(error) => return Err(error),
                                    }
                                }
                                EffectStatus::Unknown { detail } => {
                                    let _ = runtime
                                        .mark_operation_unknown(&operation.id, &detail)
                                        .await;
                                    runtime.block_task_for_effect(&task.id).await;
                                    let _ = runtime
                                        .append_event(
                                            &task.id,
                                            "operation.unknown",
                                            json!({
                                                "operation": operation.id,
                                                "tool": operation.tool,
                                                "attempt": operation.attempt,
                                                "reason": detail,
                                            }),
                                            true,
                                        )
                                        .await;
                                    return Ok(json!({
                                        "error": "effect.unknown",
                                        "operation": operation.id,
                                        "message": common::redact(&detail),
                                    }));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Reserved argument carrying the stable external idempotency key.
///
/// It is injected after the operation digest is computed and always overwrites
/// a model-supplied value, so a model cannot choose the key.
pub const OPERATION_KEY_ARGUMENT: &str = "operation_key";

/// Canonical digest for one native tool call. The reserved operation key is
/// excluded because the Worker injects it after approval.
pub(crate) fn effect_digest(tool: &str, canonical_arguments: &Value) -> String {
    common::canonical_digest(&json!({
        "tool": tool,
        "arguments": canonical_arguments,
    }))
}

/// Canonical argument form used by the digest and approval target.
pub(crate) fn canonical_arguments(args: &Value) -> Value {
    match args {
        Value::Object(map) => {
            let mut sorted = std::collections::BTreeMap::new();
            for (key, value) in map {
                if key == OPERATION_KEY_ARGUMENT {
                    continue;
                }
                sorted.insert(key.clone(), canonical_arguments(value));
            }
            Value::Object(sorted.into_iter().collect::<Map<_, _>>())
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_arguments).collect()),
        value => value.clone(),
    }
}

fn confirmed_details(value: &Value) -> (Option<String>, String) {
    let external_ref = value
        .get("external_ref")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let receipt = value
        .get("receipt")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    (external_ref, receipt)
}

fn classify_error(error: &adk_rust::AdkError) -> Option<EffectStatus> {
    match error.code {
        "effect.absent" => Some(EffectStatus::Absent),
        "effect.failed" => Some(EffectStatus::Failed {
            detail: common::redact(&error.message),
        }),
        "effect.unknown" => Some(EffectStatus::Unknown {
            detail: common::redact(&error.message),
        }),
        _ => None,
    }
}

fn receipt_value(operation: &OperationRecord) -> Value {
    json!({
        "operation": operation.id,
        "status": operation.status,
        "external_ref": operation.external_ref,
        "receipt": operation.receipt,
        "replayed": true,
    })
}

fn api_error_value(error: &common::ApiError) -> Value {
    json!({
        "error": error.code,
        "message": common::redact(&error.message),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_ignores_model_supplied_operation_key_and_object_order() {
        let first = json!({"b": 2, "a": {"d": 4, "c": 3}});
        let second = json!({"a": {"c": 3, "d": 4}, "b": 2, "operation_key": "forged"});
        assert_eq!(
            common::canonical_digest(&json!({
                "tool": "ledger_write",
                "arguments": canonical_arguments(&first),
            })),
            common::canonical_digest(&json!({
                "tool": "ledger_write",
                "arguments": canonical_arguments(&second),
            }))
        );
    }
}
