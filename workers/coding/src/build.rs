//! Bounded command execution for repository checks and builds.
//!
//! Every command runs through the shared runtime sandbox. The package sets a
//! per-worktree `CARGO_TARGET_DIR`, a bounded parallelism permit, an output
//! cap, a wall-clock deadline, and memory/CPU limits. Repository instructions
//! never select commands; the operator configuration and compiled checks do.

use crate::repo::CodingResult;
use common::ApiError;
use runtime::native::{Sandbox, SandboxCommandTool, SandboxLimits, SandboxPolicy};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Build and command limits resolved from operator settings.
#[derive(Debug, Clone)]
pub struct BuildLimits {
    /// Maximum simultaneous commands in this instance.
    pub parallelism: usize,
    /// Wall-clock deadline for one command.
    pub timeout: Duration,
    /// Captured output ceiling per stream.
    pub max_output_bytes: usize,
    /// Address-space ceiling in bytes.
    pub memory_bytes: u64,
    /// CPU-time ceiling in seconds.
    pub cpu_seconds: u64,
    /// Process/thread ceiling (`RLIMIT_NPROC`).
    pub max_processes: u64,
}

impl Default for BuildLimits {
    fn default() -> Self {
        Self {
            parallelism: 1,
            timeout: Duration::from_secs(60),
            max_output_bytes: 256 * 1024,
            memory_bytes: 1024 * 1024 * 1024,
            cpu_seconds: 60,
            max_processes: 8_192,
        }
    }
}

/// One contained command executor for a worktree.
#[derive(Clone)]
pub struct BuildService {
    workspace: PathBuf,
    target_dir: PathBuf,
    sandbox: Sandbox,
    tool: SandboxCommandTool,
    permits: Arc<Semaphore>,
    limits: BuildLimits,
}

impl BuildService {
    /// Build a policy with the worktree mounted at `/workspace`.
    pub fn new(worktree: &Path, limits: BuildLimits) -> CodingResult<Self> {
        let target_dir = worktree.join("target");
        let policy = SandboxPolicy::new(worktree)
            .with_limits(SandboxLimits {
                timeout: limits.timeout,
                max_output_bytes: limits.max_output_bytes,
                memory_bytes: limits.memory_bytes,
                cpu_seconds: limits.cpu_seconds,
                max_processes: limits.max_processes,
                max_file_bytes: 64 * 1024 * 1024,
                max_open_files: 512,
            })
            .with_environment("CARGO_TARGET_DIR", "/workspace/target")
            .with_environment("CARGO_NET_OFFLINE", "true")
            .with_environment("TMPDIR", "/tmp");
        let sandbox = Sandbox::new(policy.clone()).map_err(|error| {
            ApiError::new(
                "coding.sandbox_unavailable",
                format!("OS containment is unavailable: {error}"),
            )
        })?;
        let tool = SandboxCommandTool::new(policy);
        Ok(Self {
            workspace: worktree.to_path_buf(),
            target_dir,
            sandbox,
            tool,
            permits: Arc::new(Semaphore::new(limits.parallelism.max(1))),
            limits,
        })
    }

    /// Per-worktree build output directory. Never a shared workspace target.
    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    /// Worktree mounted inside the boundary.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Native ADK command tool bound to the same boundary.
    pub fn tool(&self) -> SandboxCommandTool {
        self.tool.clone()
    }

    /// Execute one configured check and return inspectable evidence.
    pub async fn check(&self, command: &str, cancel: &CancellationToken) -> Value {
        let permit = match self.permits.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                return json!({
                    "command": command,
                    "status": "failed",
                    "exit_code": -1,
                    "error": "coding.build_unavailable",
                    "message": "build concurrency guard is closed",
                    "unverified": "check did not run",
                })
            }
        };
        let started = Instant::now();
        let outcome = self
            .sandbox
            .exec_with_cancel(command, None, cancel.cancelled())
            .await;
        drop(permit);
        let duration_ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(output) if output.cancelled => json!({
                "command": command,
                "status": "cancelled",
                "exit_code": output.exit_code,
                "duration_ms": duration_ms,
                "stdout_digest": common::bytes_digest(output.stdout.as_bytes()),
                "stderr_digest": common::bytes_digest(output.stderr.as_bytes()),
                "unverified": "check cancelled with its process group",
            }),
            Ok(output) if output.timed_out => json!({
                "command": command,
                "status": "timed_out",
                "exit_code": output.exit_code,
                "duration_ms": duration_ms,
                "stdout_digest": common::bytes_digest(output.stdout.as_bytes()),
                "stderr_digest": common::bytes_digest(output.stderr.as_bytes()),
                "unverified": "check exceeded its wall-clock limit",
            }),
            Ok(output) => json!({
                "command": command,
                "status": if output.exit_code == 0 { "passed" } else { "failed" },
                "exit_code": output.exit_code,
                "duration_ms": duration_ms,
                "stdout": bounded(&output.stdout, 4096),
                "stderr": bounded(&output.stderr, 4096),
                "stdout_digest": common::bytes_digest(output.stdout.as_bytes()),
                "stderr_digest": common::bytes_digest(output.stderr.as_bytes()),
                "target_dir": "target",
            }),
            Err(error) => json!({
                "command": command,
                "status": "failed",
                "exit_code": -1,
                "duration_ms": duration_ms,
                "error": error.tool_code(),
                "message": error.message,
                "unverified": "check did not complete inside the OS boundary",
            }),
        }
    }

    /// Limits in force for this instance.
    pub fn limits(&self) -> &BuildLimits {
        &self.limits
    }
}

/// Parse a comma or newline separated check list from operator settings.
pub fn parse_checks(value: Option<&String>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split([',', '\n'])
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn bounded(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}[truncated]", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_accept_comma_and_newline_separators() {
        let value = "sh tests/first.sh\nsh tests/second.sh, sh tests/third.sh".to_string();
        assert_eq!(
            parse_checks(Some(&value)),
            vec![
                "sh tests/first.sh",
                "sh tests/second.sh",
                "sh tests/third.sh"
            ]
        );
        assert!(parse_checks(None).is_empty());
    }

    #[test]
    fn target_directory_is_inside_the_worktree() {
        let root = tempfile::tempdir().unwrap();
        let service = BuildService::new(root.path(), BuildLimits::default()).unwrap();
        assert_eq!(service.target_dir(), root.path().join("target"));
        assert!(service.target_dir().starts_with(root.path()));
    }
}
