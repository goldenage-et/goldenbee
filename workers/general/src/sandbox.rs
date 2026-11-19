//! Sandboxed command tool supplied by the General package.
//!
//! The shared runtime owns the OS boundary. The package only selects the
//! workspace and optional mechanism paths.

use runtime::native::{SandboxCommandTool, SandboxPolicy};
use std::path::PathBuf;

/// Default writable workspace for the package's contained command tool.
///
/// Operators may place it anywhere permitted by host policy. The ticket 32
/// sandbox boundary validates and creates it.
pub fn default_sandbox_workspace() -> PathBuf {
    if let Some(path) = std::env::var_os("GOLDENBEE_SANDBOX_DIR") {
        return PathBuf::from(path);
    }
    std::env::var_os("GOLDENBEE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("goldenbee-general"))
        .join("workspace")
}

/// Explicit command-tool policy. Mechanism paths may be overridden by the
/// deployment so an unavailable boundary fails closed instead of guessing.
pub fn sandbox_policy(workspace: impl Into<PathBuf>) -> SandboxPolicy {
    let mut policy = SandboxPolicy::new(workspace);
    if let Some(path) = std::env::var_os("GOLDENBEE_SANDBOX_BWRAP") {
        policy = policy.with_bubblewrap(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("GOLDENBEE_SANDBOX_PRLIMIT") {
        policy = policy.with_prlimit(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("GOLDENBEE_SANDBOX_KILL") {
        policy = policy.with_kill(PathBuf::from(path));
    }
    policy
}

/// Real command tool bound to the package.
pub fn sandbox_tool(workspace: impl Into<PathBuf>) -> SandboxCommandTool {
    SandboxCommandTool::new(sandbox_policy(workspace))
}
