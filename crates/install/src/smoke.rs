//! Constrained post-verification smoke acceptance.
//!
//! A valid signature never substitutes for isolation. The candidate binary
//! runs only after signature, digest, compatibility, and permission checks, and
//! only inside the existing Linux sandbox boundary. The smoke report is
//! compared with signed metadata; a mismatch rejects the release.

use crate::error::InstallError;
use common::ReleaseMetadata;
use runtime::native::{Sandbox, SandboxLimits, SandboxPolicy};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

/// Address-space ceiling for post-verification smoke execution.
///
/// This deliberately equals the runtime sandbox default: installing a release
/// must not weaken the containment boundary. It is stated here so a regression
/// fails instead of silently relaxing the ceiling.
const SMOKE_MEMORY_BYTES: u64 = 512 * 1024 * 1024;

/// Allocator arena ceiling for the constrained child.
///
/// glibc reserves a per-thread malloc arena, which is 64 MiB of *virtual*
/// address space on x86_64. A short-lived probe or launcher that starts a
/// multi-threaded Tokio runtime can therefore exhaust `RLIMIT_AS` while barely
/// touching resident memory, failing thread creation with `EAGAIN`. One arena
/// is sufficient for these bounded commands and keeps the fixed address-space
/// ceiling as the boundary instead of raising it.
const MALLOC_ARENA_MAX: &str = "1";

/// Machine-readable capability report emitted by a Worker package.
#[derive(Debug, Clone, Deserialize)]
pub struct ProbeReport {
    /// Package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// Management protocol.
    pub protocol: String,
    /// Pinned ADK version.
    pub adk: String,
    /// Capabilities actually registered by the binary.
    #[serde(default)]
    pub registered: Vec<String>,
    /// Compiled hooks actually attached.
    #[serde(default)]
    pub hooks: Vec<String>,
    /// Native launcher modes composed by the binary.
    #[serde(default)]
    pub launcher_modes: Vec<String>,
    /// Whether the native server composition can be constructed.
    #[serde(default)]
    pub serve_ready: bool,
}

/// Observable smoke result recorded with the installed release.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SmokeEvidence {
    /// Capabilities reported by the binary.
    pub registered: Vec<String>,
    /// Launcher modes actually exercised.
    pub modes: Vec<String>,
}

/// Run the constrained smoke checks for one staged release.
pub async fn run(
    release_dir: &Path,
    metadata: &ReleaseMetadata,
    workspace: &Path,
) -> Result<SmokeEvidence, InstallError> {
    std::fs::create_dir_all(workspace).map_err(|error| {
        InstallError::new(
            "install.smoke_failed",
            format!("smoke workspace could not be created: {error}"),
        )
    })?;
    let workspace = std::fs::canonicalize(workspace).map_err(|error| {
        InstallError::new(
            "install.smoke_failed",
            format!("smoke workspace could not be resolved: {error}"),
        )
    })?;
    let limits = SandboxLimits {
        timeout: Duration::from_secs(20),
        max_output_bytes: 256 * 1024,
        memory_bytes: SMOKE_MEMORY_BYTES,
        // The smoke probe creates a few runtime threads. Keep the ceiling
        // bounded while leaving room when the host already runs many tasks
        // under this uid; RLIMIT_NPROC applies to the caller's uid.
        max_processes: 8_192,
        ..SandboxLimits::default()
    };
    let policy = SandboxPolicy::new(&workspace)
        .with_limits(limits)
        .with_environment("GOLDENBEE_DATA_DIR", "/workspace/data")
        .with_environment("MALLOC_ARENA_MAX", MALLOC_ARENA_MAX)
        .with_read_only_mount(release_dir);
    let sandbox = Sandbox::new(policy).map_err(|error| {
        InstallError::new(
            "install.smoke_unavailable",
            format!("required sandbox boundary is unavailable: {error}"),
        )
    })?;
    let binary = release_dir.join("binary");
    let quoted = shell_quote(&binary);
    let probe = sandbox
        .exec(&format!("{quoted} probe --json"), None)
        .await
        .map_err(smoke_error)?;
    if probe.exit_code != 0 {
        return Err(InstallError::new(
            "install.smoke_failed",
            format!(
                "capability probe exited with {}: {}",
                probe.exit_code,
                probe.stderr.trim()
            ),
        ));
    }
    let report: ProbeReport = serde_json::from_str(probe.stdout.trim()).map_err(|_| {
        InstallError::new(
            "install.smoke_failed",
            "capability probe did not emit a machine-readable report",
        )
    })?;
    compare(metadata, &report)?;

    let mut modes = Vec::new();
    for mode in &metadata.package.launcher_modes {
        let (command, expected) = match mode.as_str() {
            "dev" | "console" => (
                format!("{quoted} dev --probe-only"),
                "launcher composition ready",
            ),
            "serve" => (
                format!("{quoted} serve --probe-only --addr 127.0.0.1:0"),
                "serve composition ready",
            ),
            _ => {
                return Err(InstallError::new(
                    "install.metadata_invalid",
                    format!("launcher mode '{mode}' is not supported"),
                ));
            }
        };
        let output = sandbox.exec(&command, None).await.map_err(smoke_error)?;
        if output.exit_code != 0 {
            return Err(InstallError::new(
                "install.smoke_failed",
                format!(
                    "launcher mode '{mode}' exited with {}: {}",
                    output.exit_code,
                    output.stderr.trim()
                ),
            ));
        }
        if !output.stdout.contains(expected) && mode == "serve" {
            return Err(InstallError::new(
                "install.smoke_failed",
                "serve composition did not report readiness",
            ));
        }
        modes.push(mode.clone());
    }

    Ok(SmokeEvidence {
        registered: report.registered,
        modes,
    })
}

fn compare(metadata: &ReleaseMetadata, report: &ProbeReport) -> Result<(), InstallError> {
    let package = &metadata.package;
    if report.package != package.name || report.version != package.version {
        return Err(InstallError::new(
            "install.smoke_mismatch",
            "smoke package identity does not match signed metadata",
        ));
    }
    if report.protocol != package.protocol || report.adk != package.adk {
        return Err(InstallError::new(
            "install.smoke_mismatch",
            "smoke compatibility declarations do not match signed metadata",
        ));
    }
    let declared: BTreeSet<&str> = package
        .capabilities
        .iter()
        .map(|capability| capability.id.as_str())
        .collect();
    let registered: BTreeSet<&str> = report.registered.iter().map(String::as_str).collect();
    if declared != registered {
        return Err(InstallError::new(
            "install.smoke_mismatch",
            "registered capabilities do not match signed metadata; compare the signed capability list with the binary's registrations",
        ));
    }
    let declared_hooks: BTreeSet<&str> = package.hooks.iter().map(String::as_str).collect();
    let report_hooks: BTreeSet<&str> = report.hooks.iter().map(String::as_str).collect();
    if declared_hooks != report_hooks {
        return Err(InstallError::new(
            "install.smoke_mismatch",
            "attached hooks do not match signed metadata",
        ));
    }
    let declared_modes: BTreeSet<&str> =
        package.launcher_modes.iter().map(String::as_str).collect();
    let report_modes: BTreeSet<&str> = report.launcher_modes.iter().map(String::as_str).collect();
    if declared_modes != report_modes || (declared_modes.contains("serve") && !report.serve_ready) {
        return Err(InstallError::new(
            "install.smoke_mismatch",
            "launcher modes do not match signed metadata",
        ));
    }
    Ok(())
}

fn smoke_error(error: runtime::native::SandboxError) -> InstallError {
    if error.code == "sandbox.unavailable" {
        InstallError::new("install.smoke_unavailable", error.message)
    } else {
        InstallError::new("install.smoke_failed", error.message)
    }
}

fn shell_quote(path: &Path) -> String {
    let value = path.display().to_string();
    format!("'{}'", value.replace('\'', "'\\''"))
}
