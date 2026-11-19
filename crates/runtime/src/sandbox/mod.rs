//! Linux OS containment for native command execution.
//!
//! This adapter is package-agnostic: a Worker Package supplies a
//! [`SandboxPolicy`] with explicit mounts, environment, limits, and workspace.
//! Commands run through `bubblewrap` with all namespaces unshared, network
//! disabled, capabilities dropped, a non-root uid/gid, and a cleared
//! environment. `prlimit` applies resource ceilings before the sandbox starts.
//!
//! The ADK tool surface mirrors the pinned native `exec_command` semantics:
//! a shell command plus an optional workspace-relative working directory,
//! returning `stdout`, `stderr`, `exit_code`, and `timed_out`. Missing required
//! mechanisms fail closed; there is no host fallback.

mod process;

use adk_rust::{Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

/// Resource and duration ceilings applied to one sandboxed command.
#[derive(Debug, Clone)]
pub struct SandboxLimits {
    /// Wall-clock deadline for the command.
    pub timeout: Duration,
    /// Maximum bytes captured from each of stdout and stderr.
    pub max_output_bytes: usize,
    /// Address-space ceiling in bytes (`RLIMIT_AS`).
    pub memory_bytes: u64,
    /// CPU-time ceiling in seconds (`RLIMIT_CPU`).
    pub cpu_seconds: u64,
    /// Process ceiling (`RLIMIT_NPROC`).
    pub max_processes: u64,
    /// Maximum written file size in bytes (`RLIMIT_FSIZE`).
    pub max_file_bytes: u64,
    /// Open descriptor ceiling (`RLIMIT_NOFILE`).
    pub max_open_files: u64,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_output_bytes: 1024 * 1024,
            memory_bytes: 512 * 1024 * 1024,
            cpu_seconds: 5,
            max_processes: 4096,
            max_file_bytes: 16 * 1024 * 1024,
            max_open_files: 256,
        }
    }
}

/// Explicit host policy for one contained command environment.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    workspace: PathBuf,
    read_only_mounts: Vec<PathBuf>,
    environment: BTreeMap<String, String>,
    uid: u32,
    gid: u32,
    bubblewrap: PathBuf,
    prlimit: PathBuf,
    kill: PathBuf,
    limits: SandboxLimits,
}

impl SandboxPolicy {
    /// Policy with only the runtime directories mounted read-only and the
    /// workspace writable at `/workspace`. Network access is always disabled.
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let read_only_mounts = ["/usr", "/bin", "/lib", "/lib64", "/sbin"]
            .into_iter()
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .collect();
        let mut environment = BTreeMap::new();
        environment.insert("PATH".into(), "/usr/bin:/bin".into());
        environment.insert("HOME".into(), "/workspace".into());
        environment.insert("LANG".into(), "C.UTF-8".into());
        Self {
            workspace: workspace.into(),
            read_only_mounts,
            environment,
            uid: 65534,
            gid: 65534,
            bubblewrap: PathBuf::from("/usr/bin/bwrap"),
            prlimit: PathBuf::from("/usr/bin/prlimit"),
            kill: PathBuf::from("/usr/bin/kill"),
            limits: SandboxLimits::default(),
        }
    }

    /// Set the wall-clock deadline.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.limits.timeout = timeout;
        self
    }

    /// Set the captured output byte ceiling for stdout and stderr separately.
    pub fn with_output_limit(mut self, max_output_bytes: usize) -> Self {
        self.limits.max_output_bytes = max_output_bytes;
        self
    }

    /// Replace all resource ceilings.
    pub fn with_limits(mut self, limits: SandboxLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Add one explicitly passed environment variable.
    pub fn with_environment(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }

    /// Mount one additional host path read-only inside the boundary. This is
    /// how immutable verified releases are exposed to a constrained smoke run.
    pub fn with_read_only_mount(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_only_mounts.push(path.into());
        self
    }

    /// Override the bubblewrap executable path.
    pub fn with_bubblewrap(mut self, path: impl Into<PathBuf>) -> Self {
        self.bubblewrap = path.into();
        self
    }

    /// Override the prlimit executable path.
    pub fn with_prlimit(mut self, path: impl Into<PathBuf>) -> Self {
        self.prlimit = path.into();
        self
    }

    /// Override the kill executable path.
    pub fn with_kill(mut self, path: impl Into<PathBuf>) -> Self {
        self.kill = path.into();
        self
    }

    /// Override the contained security identity. Zero is rejected so commands
    /// never run as root inside the user namespace.
    pub fn with_identity(mut self, uid: u32, gid: u32) -> Self {
        self.uid = uid;
        self.gid = gid;
        self
    }

    /// Host workspace mounted writable at `/workspace`.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Resource ceilings applied to each command.
    pub fn limits(&self) -> &SandboxLimits {
        &self.limits
    }
}

/// Failure from the OS containment adapter. Nothing is reported as executed
/// unless the boundary was actually established.
#[derive(Debug, Clone)]
pub struct SandboxError {
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Redacted human-readable detail.
    pub message: String,
}

impl SandboxError {
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: "sandbox.unavailable",
            message: message.into(),
        }
    }

    fn invalid_policy(message: impl Into<String>) -> Self {
        Self {
            code: "sandbox.invalid_policy",
            message: message.into(),
        }
    }

    fn spawn(message: impl Into<String>) -> Self {
        Self {
            code: "sandbox.spawn",
            message: message.into(),
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            code: "sandbox.io",
            message: message.into(),
        }
    }

    fn output_limit(limit: usize) -> Self {
        Self {
            code: "sandbox.output_limit",
            message: format!("captured output exceeded the {limit} byte limit"),
        }
    }

    fn cleanup(message: impl Into<String>) -> Self {
        Self {
            code: "sandbox.cleanup",
            message: message.into(),
        }
    }

    /// Tool-facing error code, matching the native `exec_command` JSON shape.
    pub fn tool_code(&self) -> &'static str {
        match self.code {
            "sandbox.unavailable" => "sandbox_unavailable",
            "sandbox.output_limit" => "output_limit",
            _ => "execution_error",
        }
    }
}

impl fmt::Display for SandboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SandboxError {}

/// Observable outcome of one contained command.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Process exit code, or -1 when no normal status exists.
    pub exit_code: i32,
    /// Elapsed wall-clock duration.
    pub duration: Duration,
    /// Whether the deadline terminated the command.
    pub timed_out: bool,
    /// Whether caller cancellation terminated the command.
    pub cancelled: bool,
}

/// One validated Linux containment boundary.
#[derive(Debug, Clone)]
pub struct Sandbox {
    policy: SandboxPolicy,
}

impl Sandbox {
    /// Validate the mechanism and policy. A missing executable, root identity,
    /// or malformed mount fails here rather than degrading to host execution.
    pub fn new(mut policy: SandboxPolicy) -> Result<Self, SandboxError> {
        if policy.uid == 0 || policy.gid == 0 {
            return Err(SandboxError::invalid_policy(
                "the sandbox identity must not be root",
            ));
        }
        if policy.limits.timeout.is_zero() {
            return Err(SandboxError::invalid_policy(
                "the command timeout must be greater than zero",
            ));
        }
        if policy.limits.max_output_bytes == 0 {
            return Err(SandboxError::invalid_policy(
                "the output limit must be greater than zero",
            ));
        }
        for (name, value) in [
            ("memory", policy.limits.memory_bytes),
            ("cpu", policy.limits.cpu_seconds),
            ("process", policy.limits.max_processes),
            ("file", policy.limits.max_file_bytes),
            ("descriptor", policy.limits.max_open_files),
        ] {
            if value == 0 {
                return Err(SandboxError::invalid_policy(format!(
                    "the {name} limit must be greater than zero"
                )));
            }
        }
        for (key, value) in &policy.environment {
            if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
                return Err(SandboxError::invalid_policy(
                    "environment names and values must be well formed",
                ));
            }
        }
        if !policy.workspace.is_absolute() {
            return Err(SandboxError::invalid_policy(
                "the workspace path must be absolute",
            ));
        }
        fs::create_dir_all(&policy.workspace).map_err(|error| {
            SandboxError::invalid_policy(format!("workspace could not be created: {error}"))
        })?;
        let workspace = fs::canonicalize(&policy.workspace).map_err(|error| {
            SandboxError::invalid_policy(format!("workspace could not be resolved: {error}"))
        })?;
        if !workspace.is_dir() {
            return Err(SandboxError::invalid_policy(
                "the workspace must be a directory",
            ));
        }
        if workspace == Path::new("/") {
            return Err(SandboxError::invalid_policy(
                "the host root cannot be the writable workspace",
            ));
        }
        policy.workspace = workspace;
        for mount in &policy.read_only_mounts {
            if !mount.is_absolute() || !mount.exists() {
                return Err(SandboxError::invalid_policy(format!(
                    "read-only mount is not an existing absolute path: {}",
                    mount.display()
                )));
            }
            if mount == Path::new("/") || mount == Path::new("/workspace") {
                return Err(SandboxError::invalid_policy(
                    "read-only mounts cannot cover the root or workspace",
                ));
            }
        }
        require_executable(&policy.bubblewrap)?;
        require_executable(&policy.prlimit)?;
        require_executable(&policy.kill)?;
        let probe = std::process::Command::new(&policy.bubblewrap)
            .arg("--version")
            .env_clear()
            .stdin(Stdio::null())
            .output()
            .map_err(|error| {
                SandboxError::unavailable(format!("bubblewrap could not start: {error}"))
            })?;
        if !probe.status.success() {
            return Err(SandboxError::unavailable(
                "bubblewrap failed its version probe",
            ));
        }
        let probe = std::process::Command::new(&policy.prlimit)
            .arg("--version")
            .env_clear()
            .stdin(Stdio::null())
            .output()
            .map_err(|error| {
                SandboxError::unavailable(format!("prlimit could not start: {error}"))
            })?;
        if !probe.status.success() {
            return Err(SandboxError::unavailable(
                "prlimit failed its version probe",
            ));
        }
        Ok(Self { policy })
    }

    /// Policy in force for this boundary.
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }
}

fn require_executable(path: &Path) -> Result<(), SandboxError> {
    let metadata = fs::metadata(path).map_err(|_| {
        SandboxError::unavailable(format!("required mechanism is missing: {}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(SandboxError::unavailable(format!(
            "required mechanism is not a file: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(SandboxError::unavailable(format!(
                "required mechanism is not executable: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Native ADK command tool backed by the OS boundary.
///
/// `name`, parameters, and result fields match the pinned ADK
/// `exec_command` tool. When the boundary is unavailable the tool returns a
/// structured error and never executes the command on the host.
#[derive(Debug, Clone)]
pub struct SandboxCommandTool {
    sandbox: Result<Sandbox, SandboxError>,
}

impl SandboxCommandTool {
    /// Build a tool from an explicit policy. Mechanism validation happens here
    /// so the failure is reported on every invocation.
    pub fn new(policy: SandboxPolicy) -> Self {
        Self {
            sandbox: Sandbox::new(policy),
        }
    }

    /// Construct an explicitly unavailable tool, used by packages that detect a
    /// missing deployment prerequisite before a policy can be validated.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            sandbox: Err(SandboxError::unavailable(reason)),
        }
    }

    /// The validated boundary, when available.
    pub fn sandbox(&self) -> Option<&Sandbox> {
        self.sandbox.as_ref().ok()
    }
}

#[async_trait]
impl Tool for SandboxCommandTool {
    fn name(&self) -> &str {
        "exec_command"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the sandbox workspace"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to execute"
                },
                "working_dir": {
                    "type": "string",
                    "description": "Optional working directory relative to workspace root"
                }
            },
            "required": ["command"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return Ok(json!({
                "error": "invalid_arguments",
                "message": "Missing required parameter 'command'"
            }));
        };
        let working_dir = args.get("working_dir").and_then(Value::as_str);
        let sandbox = match &self.sandbox {
            Ok(sandbox) => sandbox,
            Err(error) => {
                return Ok(json!({
                    "error": error.tool_code(),
                    "message": error.message
                }));
            }
        };
        match sandbox
            .exec_with_cancel(command, working_dir, until_cancelled(ctx))
            .await
        {
            Ok(output) if output.cancelled => Ok(json!({
                "error": "cancelled",
                "message": "command cancellation terminated the process group"
            })),
            Ok(output) => Ok(json!({
                "stdout": output.stdout,
                "stderr": output.stderr,
                "exit_code": output.exit_code,
                "timed_out": output.timed_out
            })),
            Err(error) => Ok(json!({
                "error": error.tool_code(),
                "message": error.message
            })),
        }
    }
}

async fn until_cancelled(ctx: Arc<dyn ToolContext>) {
    loop {
        if ctx.is_cancelled() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
