//! Runtime CLI parity acceptance fixture Worker.
//!
//! This is test support, not a product Worker Package. It composes the shared
//! runtime mechanism surface from package declarations and operator-selected
//! configuration only: generic file/process/web tools, subagents, progressive
//! skills, a compiled Worker hook, and one protected effect tool. It adds no
//! domain prompt, coding workflow, TUI, Gateway routing, or host lifecycle.

use adk_rust::{AdkError, ErrorCategory, ErrorComponent, Tool, ToolContext};
use async_trait::async_trait;
use runtime::native::WebPolicy;
use runtime::native::{EffectPolicy, EffectReconciler, EffectStatus, ProcessPolicy, SandboxPolicy};
use runtime::policy::PermissionMode;
use runtime::subagents::{SubagentConfig, SubagentDefinition};
use runtime::worker::{
    BuiltinTool, HookDecision, TaskHookContext, ToolHookContext, ToolHookOutcome, Worker,
    WorkerHooks,
};
use runtime::OperationRecord;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

const EFFECT_TOOL: &str = "ledger_write";

fn workspace() -> PathBuf {
    std::env::var_os("ACCEPTANCE_SANDBOX_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("goldenbee-acceptance/workspace"))
}

fn effect_file() -> PathBuf {
    std::env::var_os("ACCEPTANCE_EFFECT_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("goldenbee-acceptance/effects.log"))
}

fn hook_log() -> Option<PathBuf> {
    std::env::var_os("ACCEPTANCE_HOOK_LOG").map(PathBuf::from)
}

struct LedgerTool {
    effect: PathBuf,
}

#[async_trait]
impl Tool for LedgerTool {
    fn name(&self) -> &str {
        EFFECT_TOOL
    }

    fn description(&self) -> &str {
        "Append one value to the fixture's protected external ledger"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {"entry": {"type": "string"}},
            "required": ["entry"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let entry = args
            .get("entry")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let key = args
            .get("operation_key")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(parent) = self.effect.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    "acceptance.storage",
                    format!("effect directory unavailable: {error}"),
                )
            })?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.effect)
            .map_err(|error| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    "acceptance.storage",
                    format!("effect ledger unavailable: {error}"),
                )
            })?;
        writeln!(file, "{key}\t{entry}")
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Internal,
                    "acceptance.storage",
                    format!("effect ledger unwritable: {error}"),
                )
            })?;
        Ok(json!({
            "external_ref": format!("ledger-{}", entry),
            "receipt": "accepted"
        }))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

struct LedgerStatus {
    effect: PathBuf,
}

#[async_trait]
impl EffectReconciler for LedgerStatus {
    async fn status(&self, operation: &OperationRecord) -> EffectStatus {
        let entry = std::fs::read_to_string(&self.effect).unwrap_or_default();
        if entry.lines().any(|line| {
            line.split_once('\t')
                .is_some_and(|(key, _)| key == operation.idempotency_key)
        }) {
            return EffectStatus::Committed {
                external_ref: Some(format!("ledger-{}", operation.idempotency_key)),
                receipt: Some("accepted".into()),
            };
        }
        EffectStatus::Absent
    }
}

/// Compiled Worker hook that records stable runtime events.
struct FileHooks {
    log: PathBuf,
}

impl FileHooks {
    fn append(&self, line: &str) {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .and_then(|mut file| writeln!(file, "{line}"));
    }
}

#[async_trait]
impl WorkerHooks for FileHooks {
    fn label(&self) -> &str {
        "acceptance-file-hooks"
    }

    async fn session_start(
        &self,
        ctx: &runtime::worker::SessionHookContext,
    ) -> runtime::worker::WorkerResult<()> {
        self.append(&format!("session_start|{}", ctx.session_id));
        Ok(())
    }

    async fn prompt_submit(
        &self,
        _ctx: &TaskHookContext,
    ) -> runtime::worker::WorkerResult<HookDecision> {
        self.append("prompt_submit|");
        Ok(HookDecision::Continue)
    }

    async fn pre_tool(&self, ctx: &ToolHookContext) -> runtime::worker::WorkerResult<HookDecision> {
        self.append(&format!("pre_tool|{}", ctx.tool));
        Ok(HookDecision::Continue)
    }

    async fn post_tool(
        &self,
        ctx: &ToolHookContext,
        _outcome: &ToolHookOutcome,
    ) -> runtime::worker::WorkerResult<()> {
        self.append(&format!("post_tool|{}", ctx.tool));
        Ok(())
    }

    async fn stop(
        &self,
        ctx: &runtime::worker::StopHookContext,
    ) -> runtime::worker::WorkerResult<()> {
        self.append(&format!("stop|{}|{}", ctx.task.session_id, ctx.state));
        Ok(())
    }
}

fn subagents() -> SubagentConfig {
    SubagentConfig::new()
        .max_depth(1)
        .max_fanout(2)
        .max_tasks(4)
        .permission_mode(PermissionMode::AcceptEdits)
        .definition(
            "helper",
            SubagentDefinition::new("echo")
                .description("Short deterministic helper that repeats its input.")
                .inherit_tools(),
        )
        .definition(
            "researcher",
            SubagentDefinition::new("general")
                .description("General specialist that inherits the parent tool set.")
                .inherit_tools(),
        )
}

fn worker() -> Worker {
    let effect = effect_file();
    let hooks = hook_log().map(|log| Arc::new(FileHooks { log }) as Arc<dyn WorkerHooks>);
    let policy = ProcessPolicy::new(SandboxPolicy::new(workspace()));
    let mut worker = Worker::new("acceptance-fixture", "0.0.1")
        .required_permissions(["task:submit", "session:write", "subagent:manage"])
        .file_tools()
        .process_tool(BuiltinTool::Shell, policy)
        .web_tools(
            WebPolicy::new()
                .allow_domain("127.0.0.1")
                .allow_http(true)
                .allow_private_network(true)
                .max_bytes(64 * 1024),
        )
        .builtin_tools([BuiltinTool::WriteTodos])
        .progressive_skills()
        .subagents(subagents())
        .effect_tool(
            EFFECT_TOOL,
            Arc::new(LedgerTool {
                effect: effect.clone(),
            }),
            Arc::new(LedgerStatus { effect }),
            EffectPolicy::default(),
        );
    // The fixture never asks for an effect approval; the exact tool is linked
    // through the shared adapter so the acceptance scenario can assert typed
    // operation evidence.
    if let Some(hooks) = hooks {
        worker = worker.hooks(hooks);
    }
    worker
}

fn main() -> ExitCode {
    let _ = dotenvy::dotenv();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("acceptance-fixture: runtime could not start: {error}");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(worker().run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("acceptance-fixture: {}: {}", error.code, error.message);
            ExitCode::from(if error.code.starts_with("cli.") { 2 } else { 1 })
        }
    }
}
