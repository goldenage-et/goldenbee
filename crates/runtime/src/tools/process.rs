//! Opt-in shell, PTY, and background process tools.
//!
//! Every tool delegates execution to [`ProcessSupervisor`](crate::process) so
//! the sandbox boundary, stable ids, bounded buffers, cancellation cascade, and
//! wait/kill semantics are identical in every mode. The kernel remains the only
//! authorization, timeout, and evidence boundary.

use super::kernel::{ToolDescriptor, ToolEffect, ToolKernel, ToolRisk};
use crate::process::{ProcessError, ProcessRuntime, ProcessState, ProcessSupervisor};
use crate::Runtime;
use adk_rust::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Foreground contained command.
pub struct ShellTool {
    kernel: ToolKernel,
    runtime: Arc<OnceLock<Runtime>>,
    process: Arc<ProcessRuntime>,
}

impl ShellTool {
    /// Build the foreground command tool.
    pub fn new(
        kernel: ToolKernel,
        runtime: Arc<OnceLock<Runtime>>,
        process: Arc<ProcessRuntime>,
    ) -> Self {
        Self {
            kernel,
            runtime,
            process,
        }
    }

    /// Runtime tool descriptor.
    pub fn descriptor(process: &ProcessRuntime) -> ToolDescriptor {
        ToolDescriptor::new("process:execute")
            .risk(ToolRisk::High)
            .effect(ToolEffect::None)
            .builtin()
            .typed_errors()
            .audit()
            .writes()
            .timeout(process.policy().command_timeout() + Duration::from_secs(7))
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Run one command to completion inside the Worker sandbox and return its \
         bounded stdout, stderr, and exit code. The command cannot reach the host \
         network or write outside the configured workspace."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional workspace-relative working directory."
                }
            },
            "required": ["command"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(task) = self.kernel.task_for(ctx.as_ref()).await else {
            return Ok(bound_error(
                "process.task_unbound",
                "a shell command must execute inside an admitted running task",
            ));
        };
        let Some(runtime) = self.runtime.get() else {
            return Ok(bound_error(
                "process.runtime_unbound",
                "the Worker runtime is not available",
            ));
        };
        let command = match required_command(&args) {
            Ok(command) => command,
            Err(error) => return Ok(error.into_value()),
        };
        let working_dir = args.get("working_dir").and_then(Value::as_str);
        let sandbox = match self.process.sandbox() {
            Ok(sandbox) => sandbox.clone(),
            Err(error) => return Ok(error.into_value()),
        };
        let supervisor = runtime.processes();
        let started = supervisor
            .start_pipe(
                runtime,
                &task,
                &sandbox,
                self.process.policy(),
                "shell",
                &command,
                working_dir,
                0,
            )
            .await;
        let view = match started {
            Ok(view) => view,
            Err(error) => return Ok(error.into_value()),
        };
        let view = match supervisor
            .wait(&view.id, self.process.policy().command_timeout())
            .await
        {
            Ok(view) if view.state == ProcessState::Running => {
                match supervisor.kill(&view.id, "timeout").await {
                    Ok(killed) => {
                        let state = killed.state;
                        let mut value = killed.into_value();
                        value["wait_timed_out"] = Value::Bool(true);
                        insert_error(
                            &mut value,
                            "process.timeout",
                            "the command exceeded its deadline",
                        );
                        let _ = runtime
                            .append_event(
                                &task.id,
                                "process.terminated",
                                json!({
                                    "process_id": view.id,
                                    "tool": "shell",
                                    "reason": "timeout",
                                    "state": state.as_str(),
                                }),
                                true,
                            )
                            .await;
                        supervisor.discard(&view.id).await;
                        return Ok(value);
                    }
                    Err(error) => {
                        supervisor.discard(&view.id).await;
                        return Ok(error.into_value());
                    }
                }
            }
            Ok(view) => view,
            Err(error) => return Ok(error.into_value()),
        };
        let id = view.id.clone();
        let value = view.into_value();
        supervisor.discard(&id).await;
        Ok(value)
    }
}

/// Start one command in the background.
pub struct BackgroundProcessTool {
    kernel: ToolKernel,
    runtime: Arc<OnceLock<Runtime>>,
    process: Arc<ProcessRuntime>,
}

impl BackgroundProcessTool {
    /// Build the background process tool.
    pub fn new(
        kernel: ToolKernel,
        runtime: Arc<OnceLock<Runtime>>,
        process: Arc<ProcessRuntime>,
    ) -> Self {
        Self {
            kernel,
            runtime,
            process,
        }
    }

    /// Runtime tool descriptor.
    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new("process:execute")
            .risk(ToolRisk::High)
            .effect(ToolEffect::None)
            .builtin()
            .typed_errors()
            .audit()
            .writes()
            .no_timeout()
    }
}

#[async_trait]
impl Tool for BackgroundProcessTool {
    fn name(&self) -> &str {
        "background_process"
    }

    fn description(&self) -> &str {
        "Start one command in the background and return a stable process id. \
         Use process_wait to read buffered output and process_kill to stop it. \
         The command runs inside the Worker sandbox and is killed when its task \
         is cancelled or the Worker stops."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional workspace-relative working directory."
                },
                "yield_time_ms": {
                    "type": "integer",
                    "description": "How long to wait for initial output (default 250)."
                }
            },
            "required": ["command"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(task) = self.kernel.task_for(ctx.as_ref()).await else {
            return Ok(bound_error(
                "process.task_unbound",
                "a background process must execute inside an admitted running task",
            ));
        };
        let Some(runtime) = self.runtime.get() else {
            return Ok(bound_error(
                "process.runtime_unbound",
                "the Worker runtime is not available",
            ));
        };
        let command = match required_command(&args) {
            Ok(command) => command,
            Err(error) => return Ok(error.into_value()),
        };
        let working_dir = args.get("working_dir").and_then(Value::as_str);
        let yield_ms = yield_ms(&args, self.process.policy().default_yield());
        let sandbox = match self.process.sandbox() {
            Ok(sandbox) => sandbox.clone(),
            Err(error) => return Ok(error.into_value()),
        };
        match runtime
            .processes()
            .start_pipe(
                runtime,
                &task,
                &sandbox,
                self.process.policy(),
                "background_process",
                &command,
                working_dir,
                yield_ms,
            )
            .await
        {
            Ok(view) => Ok(view.into_value()),
            Err(error) => Ok(error.into_value()),
        }
    }
}

/// Start or continue an interactive PTY session.
pub struct PtyTool {
    kernel: ToolKernel,
    runtime: Arc<OnceLock<Runtime>>,
    process: Arc<ProcessRuntime>,
}

impl PtyTool {
    /// Build the PTY tool.
    pub fn new(
        kernel: ToolKernel,
        runtime: Arc<OnceLock<Runtime>>,
        process: Arc<ProcessRuntime>,
    ) -> Self {
        Self {
            kernel,
            runtime,
            process,
        }
    }

    /// Runtime tool descriptor.
    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new("process:execute")
            .risk(ToolRisk::High)
            .effect(ToolEffect::None)
            .builtin()
            .typed_errors()
            .audit()
            .writes()
            .no_timeout()
    }
}

#[async_trait]
impl Tool for PtyTool {
    fn name(&self) -> &str {
        "pty"
    }

    fn description(&self) -> &str {
        "Start an interactive terminal session in the Worker sandbox, or write \
         input to an existing one. The first call takes `command` and returns a \
         process id; later calls take `process_id` with optional `input`, \
         `rows`, and `cols`. Output is bounded and also streamed as tool events."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Command to start when no process_id is supplied."
                },
                "process_id": {
                    "type": "string",
                    "description": "Existing PTY session to continue."
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional workspace-relative working directory."
                },
                "input": {
                    "type": "string",
                    "description": "Input to write to an existing session."
                },
                "rows": {
                    "type": "integer",
                    "description": "Terminal rows for resize."
                },
                "cols": {
                    "type": "integer",
                    "description": "Terminal columns for resize."
                },
                "yield_time_ms": {
                    "type": "integer",
                    "description": "How long to wait for output (default 250)."
                }
            }
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(task) = self.kernel.task_for(ctx.as_ref()).await else {
            return Ok(bound_error(
                "process.task_unbound",
                "a PTY session must execute inside an admitted running task",
            ));
        };
        let Some(runtime) = self.runtime.get() else {
            return Ok(bound_error(
                "process.runtime_unbound",
                "the Worker runtime is not available",
            ));
        };
        let supervisor = runtime.processes();
        let yield_ms = yield_ms(&args, self.process.policy().default_yield());
        if let Some(id) = args.get("process_id").and_then(Value::as_str) {
            if !supervisor.owns(id, &task.id).await {
                return Ok(bound_error(
                    "process.denied",
                    "the process id is not owned by this task",
                ));
            }
            if let Some(input) = args.get("input").and_then(Value::as_str) {
                if let Err(error) = supervisor.write_stdin(id, input).await {
                    return Ok(error.into_value());
                }
            }
            if let (Some(rows), Some(cols)) = (
                args.get("rows").and_then(Value::as_u64),
                args.get("cols").and_then(Value::as_u64),
            ) {
                if let Err(error) = supervisor
                    .resize(
                        id,
                        rows.min(u16::MAX as u64) as u16,
                        cols.min(u16::MAX as u64) as u16,
                    )
                    .await
                {
                    return Ok(error.into_value());
                }
            }
            return match supervisor.wait(id, Duration::from_millis(yield_ms)).await {
                Ok(view) => Ok(view.into_value()),
                Err(error) => Ok(error.into_value()),
            };
        }
        let command = match required_command(&args) {
            Ok(command) => command,
            Err(error) => return Ok(error.into_value()),
        };
        let working_dir = args.get("working_dir").and_then(Value::as_str);
        let sandbox = match self.process.sandbox() {
            Ok(sandbox) => sandbox.clone(),
            Err(error) => return Ok(error.into_value()),
        };
        match supervisor
            .start_pty(
                runtime,
                &task,
                &sandbox,
                self.process.policy(),
                "pty",
                &command,
                working_dir,
                yield_ms,
            )
            .await
        {
            Ok(view) => Ok(view.into_value()),
            Err(error) => Ok(error.into_value()),
        }
    }
}

/// Observe one owned background process.
pub struct ProcessWaitTool {
    kernel: ToolKernel,
    runtime: Arc<OnceLock<Runtime>>,
}

impl ProcessWaitTool {
    /// Build the wait tool.
    pub fn new(kernel: ToolKernel, runtime: Arc<OnceLock<Runtime>>) -> Self {
        Self { kernel, runtime }
    }

    /// Runtime tool descriptor.
    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new("process:read")
            .risk(ToolRisk::Low)
            .effect(ToolEffect::None)
            .builtin()
            .typed_errors()
            .no_timeout()
    }
}

#[async_trait]
impl Tool for ProcessWaitTool {
    fn name(&self) -> &str {
        "process_wait"
    }

    fn description(&self) -> &str {
        "Wait up to `timeout_ms` for a process owned by this task and return its \
         state plus output captured since the last read. A running process is \
         reported as `running` with `wait_timed_out`, not as an error."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "process_id": {
                    "type": "string",
                    "description": "Process id returned by a start tool."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "How long to wait (default 1000, max 600000)."
                }
            },
            "required": ["process_id"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(task) = self.kernel.task_for(ctx.as_ref()).await else {
            return Ok(bound_error(
                "process.task_unbound",
                "a process wait must execute inside an admitted running task",
            ));
        };
        let Some(runtime) = self.runtime.get() else {
            return Ok(bound_error(
                "process.runtime_unbound",
                "the Worker runtime is not available",
            ));
        };
        let Some(id) = args.get("process_id").and_then(Value::as_str) else {
            return Ok(bound_error(
                "process.invalid_arguments",
                "missing required string argument 'process_id'",
            ));
        };
        let supervisor: &ProcessSupervisor = runtime.processes();
        if !supervisor.owns(id, &task.id).await {
            return Ok(bound_error(
                "process.denied",
                "the process id is not owned by this task",
            ));
        }
        let timeout = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(1000)
            .min(600_000);
        match supervisor.wait(id, Duration::from_millis(timeout)).await {
            Ok(view) => Ok(view.into_value()),
            Err(error) => Ok(error.into_value()),
        }
    }
}

/// Terminate one owned background process.
pub struct ProcessKillTool {
    kernel: ToolKernel,
    runtime: Arc<OnceLock<Runtime>>,
}

impl ProcessKillTool {
    /// Build the kill tool.
    pub fn new(kernel: ToolKernel, runtime: Arc<OnceLock<Runtime>>) -> Self {
        Self { kernel, runtime }
    }

    /// Runtime tool descriptor.
    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new("process:execute")
            .risk(ToolRisk::High)
            .effect(ToolEffect::None)
            .builtin()
            .typed_errors()
            .audit()
            .timeout(Duration::from_secs(10))
    }
}

#[async_trait]
impl Tool for ProcessKillTool {
    fn name(&self) -> &str {
        "process_kill"
    }

    fn description(&self) -> &str {
        "Terminate a process owned by this task and report the observed final \
         state. An unconfirmed termination is reported as `unknown`, never as a \
         claimed kill."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "process_id": {
                    "type": "string",
                    "description": "Process id returned by a start tool."
                }
            },
            "required": ["process_id"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let Some(task) = self.kernel.task_for(ctx.as_ref()).await else {
            return Ok(bound_error(
                "process.task_unbound",
                "a process kill must execute inside an admitted running task",
            ));
        };
        let Some(runtime) = self.runtime.get() else {
            return Ok(bound_error(
                "process.runtime_unbound",
                "the Worker runtime is not available",
            ));
        };
        let Some(id) = args.get("process_id").and_then(Value::as_str) else {
            return Ok(bound_error(
                "process.invalid_arguments",
                "missing required string argument 'process_id'",
            ));
        };
        let supervisor = runtime.processes();
        if !supervisor.owns(id, &task.id).await {
            return Ok(bound_error(
                "process.denied",
                "the process id is not owned by this task",
            ));
        }
        match supervisor.kill(id, "killed").await {
            Ok(view) => {
                let state = view.state;
                let exit_code = view.exit_code;
                let mut value = view.into_value();
                if state == ProcessState::Unknown {
                    insert_error(
                        &mut value,
                        "process.unknown",
                        "the process did not confirm termination",
                    );
                }
                let _ = runtime
                    .append_event(
                        &task.id,
                        "process.terminated",
                        json!({
                            "process_id": id,
                            "tool": "process_kill",
                            "reason": "killed",
                            "state": state.as_str(),
                            "exit_code": exit_code,
                        }),
                        true,
                    )
                    .await;
                Ok(value)
            }
            Err(error) => Ok(error.into_value()),
        }
    }
}

fn required_command(args: &Value) -> std::result::Result<String, ProcessError> {
    args.get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ProcessError {
            code: "process.invalid_arguments",
            message: "missing required string argument 'command'".into(),
        })
}

fn yield_ms(args: &Value, default: Duration) -> u64 {
    args.get("yield_time_ms")
        .and_then(Value::as_u64)
        .unwrap_or(default.as_millis() as u64)
        .min(5_000)
}

fn bound_error(code: &str, message: impl Into<String>) -> Value {
    let message: String = message.into();
    json!({
        "error": code,
        "message": common::redact(&message),
    })
}

fn insert_error(value: &mut Value, code: &str, message: &str) {
    if let Some(object) = value.as_object_mut() {
        object.insert("error".into(), Value::String(code.into()));
        object.insert("message".into(), Value::String(common::redact(message)));
    }
}
