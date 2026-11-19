//! Cross-process fault-injection fixture for protected external effects.
//!
//! This is test support, not a product Worker Package. It registers one real
//! native tool through the shared runtime's effect adapter and gives the
//! external system a file-backed status/idempotency interface. Every applied
//! effect appends one line to `effects.log`, so tests assert real effect
//! counts rather than reported status.

use adk_rust::{AdkError, ErrorCategory, ErrorComponent, Tool, ToolContext};
use async_trait::async_trait;
use runtime::native::{EffectApprovalPolicy, EffectPolicy, EffectReconciler, EffectStatus};
use runtime::worker::Worker;
use runtime::OperationRecord;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

const TOOL: &str = "ledger_write";

fn external_dir() -> PathBuf {
    std::env::var_os("GOLDENBEE_EFFECT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("goldenbee-effect-fixture"))
}

fn ensure(directory: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)
}

fn applied_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("applied").join(key)
}

fn failed_path(dir: &Path, key: &str) -> PathBuf {
    dir.join("failed").join(key)
}

struct LedgerTool {
    dir: PathBuf,
    behavior: String,
}

#[async_trait]
impl Tool for LedgerTool {
    fn name(&self) -> &str {
        TOOL
    }

    fn description(&self) -> &str {
        "Append one entry to the external ledger"
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
        let attempts_dir = self.dir.join("attempts");
        ensure(&attempts_dir).map_err(|error| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "tool.storage",
                format!("attempt store unavailable: {error}"),
            )
        })?;
        let attempts_path = attempts_dir.join(key);
        let attempts = std::fs::read_to_string(&attempts_path)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(0)
            + 1;
        std::fs::write(&attempts_path, attempts.to_string()).map_err(|error| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "tool.storage",
                format!("attempt store unavailable: {error}"),
            )
        })?;
        if self.behavior == "fail-once" && attempts == 1 {
            ensure(&self.dir.join("failed")).map_err(|error| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    "tool.storage",
                    format!("failure store unavailable: {error}"),
                )
            })?;
            std::fs::write(failed_path(&self.dir, key), "injected definite failure").map_err(
                |error| {
                    AdkError::new(
                        ErrorComponent::Tool,
                        ErrorCategory::Internal,
                        "tool.storage",
                        format!("failure store unavailable: {error}"),
                    )
                },
            )?;
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "tool.transport",
                "injected transport failure after a definite external failure",
            ));
        }

        ensure(&self.dir.join("applied")).map_err(|error| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "tool.storage",
                format!("effect store unavailable: {error}"),
            )
        })?;
        let external_ref = format!("ref-{key}");
        let receipt = if self.behavior == "secret" {
            "token=sk-canary-123456 accepted".to_string()
        } else {
            "accepted".to_string()
        };
        let applied = json!({
            "external_ref": external_ref,
            "receipt": receipt,
            "entry": entry,
        });
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(applied_path(&self.dir, key))
            .map_err(|error| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    "tool.storage",
                    format!("effect store unavailable: {error}"),
                )
            })?;
        file.write_all(applied.to_string().as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    "tool.storage",
                    format!("effect store unavailable: {error}"),
                )
            })?;
        let mut effects = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("effects.log"))
            .map_err(|error| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    "tool.storage",
                    format!("effect log unavailable: {error}"),
                )
            })?;
        writeln!(effects, "{key} {entry}")
            .and_then(|_| effects.sync_all())
            .map_err(|error| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    "tool.storage",
                    format!("effect log unavailable: {error}"),
                )
            })?;
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

struct LedgerStatus {
    dir: PathBuf,
}

#[async_trait]
impl EffectReconciler for LedgerStatus {
    async fn status(&self, operation: &OperationRecord) -> EffectStatus {
        if std::env::var("GOLDENBEE_EFFECT_STATUS").as_deref() == Ok("unavailable") {
            return EffectStatus::Unknown {
                detail: "the external status interface is temporarily unavailable".into(),
            };
        }
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
                    detail: format!("the external status record is unreadable: {error}"),
                },
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if failed_path(&self.dir, &operation.idempotency_key).exists() {
                    EffectStatus::Failed {
                        detail: "the external system reported a definite failure".into(),
                    }
                } else {
                    EffectStatus::Absent
                }
            }
            Err(error) => EffectStatus::Unknown {
                detail: format!("the external status interface could not be read: {error}"),
            },
        }
    }
}

fn main() -> ExitCode {
    let _ = dotenvy::dotenv();
    let dir = external_dir();
    let behavior = std::env::var("GOLDENBEE_EFFECT_BEHAVIOR").unwrap_or_default();
    let mut policy = if std::env::var("GOLDENBEE_EFFECT_RETRY").as_deref() == Ok("1") {
        EffectPolicy::reconcile(2)
    } else {
        EffectPolicy::default()
    };
    if std::env::var("GOLDENBEE_EFFECT_APPROVAL").as_deref() == Ok("1") {
        let ttl = std::env::var("GOLDENBEE_EFFECT_APPROVAL_TTL_MS")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(60_000);
        policy = policy.requiring_approval(
            EffectApprovalPolicy::new("external-write", ttl).target_argument("entry"),
        );
    }
    let worker = Worker::new("effect-fixture", "0.0.1")
        .required_permissions(["task:submit", "session:write"])
        .effect_tool(
            TOOL,
            Arc::new(LedgerTool {
                dir: dir.clone(),
                behavior,
            }),
            Arc::new(LedgerStatus { dir }),
            policy,
        );
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("effect-fixture: runtime could not start: {error}");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(worker.run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("effect-fixture: {}: {}", error.code, error.message);
            ExitCode::from(if error.code.starts_with("cli.") { 2 } else { 1 })
        }
    }
}
