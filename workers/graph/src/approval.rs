//! Protected external effect used by the graph approval scenario.
//!
//! The tool writes one line to a file-backed external ledger and reports a
//! definite state through the same status interface the shared runtime uses
//! for reconciliation. It is a test scenario inside the graph package, not a
//! second execution engine.

use adk_rust::{AdkError, ErrorCategory, ErrorComponent, Tool, ToolContext};
use async_trait::async_trait;
use runtime::native::{EffectApprovalPolicy, EffectPolicy, EffectReconciler, EffectStatus};
use runtime::OperationRecord;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Native tool name used by the approval graph scenario.
pub const TOOL: &str = "ledger_write";

fn data_dir() -> PathBuf {
    std::env::var_os("GOLDENBEE_GRAPH_EFFECT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("goldenbee-graph-effect"))
}

fn applied_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("applied").join(key)
}

/// One real external effect with a stable Worker-injected operation key.
pub struct LedgerTool {
    dir: PathBuf,
}

impl Default for LedgerTool {
    fn default() -> Self {
        Self { dir: data_dir() }
    }
}

#[async_trait]
impl Tool for LedgerTool {
    fn name(&self) -> &str {
        TOOL
    }

    fn description(&self) -> &str {
        "Append one entry to the external approval ledger"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "entry": {"type": "string"}
            },
            "required": ["entry"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let Some(key) = args.get("operation_key").and_then(Value::as_str) else {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "tool.operation_key_missing",
                "the effect adapter did not pass an operation key",
            ));
        };
        let entry = args
            .get("entry")
            .and_then(Value::as_str)
            .unwrap_or_default();
        std::fs::create_dir_all(self.dir.join("applied")).map_err(storage)?;
        let external_ref = format!("graph-ref-{key}");
        let receipt = "graph ledger accepted".to_string();
        std::fs::write(
            applied_path(&self.dir, key),
            json!({
                "external_ref": external_ref,
                "receipt": receipt,
                "entry": entry,
            })
            .to_string(),
        )
        .map_err(storage)?;
        std::fs::create_dir_all(&self.dir).map_err(storage)?;
        let mut effects = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("effects.log"))
            .map_err(storage)?;
        writeln!(effects, "{key} {entry}")
            .and_then(|_| effects.sync_all())
            .map_err(storage)?;
        Ok(json!({
            "external_ref": external_ref,
            "receipt": receipt,
            "entry": entry,
        }))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

fn storage(error: std::io::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Internal,
        "tool.storage",
        format!("graph effect store unavailable: {error}"),
    )
}

/// Status interface for the external ledger.
pub struct LedgerStatus {
    dir: PathBuf,
}

impl Default for LedgerStatus {
    fn default() -> Self {
        Self { dir: data_dir() }
    }
}

#[async_trait]
impl EffectReconciler for LedgerStatus {
    async fn status(&self, operation: &OperationRecord) -> EffectStatus {
        match std::fs::read_to_string(applied_path(&self.dir, &operation.idempotency_key)) {
            Ok(value) => match serde_json::from_str::<Value>(&value) {
                Ok(value) => EffectStatus::Committed {
                    external_ref: value
                        .get("external_ref")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    receipt: value
                        .get("receipt")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                },
                Err(error) => EffectStatus::Unknown {
                    detail: format!("the graph effect record is unreadable: {error}"),
                },
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => EffectStatus::Absent,
            Err(error) => EffectStatus::Unknown {
                detail: format!("the graph effect store could not be read: {error}"),
            },
        }
    }
}

/// Approval policy for the protected graph scenario.
pub fn policy() -> EffectPolicy {
    let ttl = std::env::var("GOLDENBEE_GRAPH_APPROVAL_TTL_MS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(60_000);
    EffectPolicy::default().requiring_approval(
        EffectApprovalPolicy::new("external-write", ttl).target_argument("entry"),
    )
}
