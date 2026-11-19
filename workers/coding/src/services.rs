//! Instance-scoped repository, worktree, and build services.
//!
//! One process owns one `CodingServices`. It provisions a worktree, enforces
//! exclusive write ownership, and exposes the shared sandbox command tool.
//! Nothing here is a generic operation registry; the service is package-owned
//! state used by package hooks and native tools.

use crate::build::{parse_checks, BuildLimits, BuildService};
use crate::repo::{self, CodingResult, Worktree};
use common::ApiError;
use runtime::native::{SandboxCommandTool, SandboxPolicy};
use runtime::worker::WorkerSettings;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Resolved package settings for one instance.
#[derive(Debug, Clone)]
pub struct CodingConfig {
    /// Authorized workspace root.
    pub workspace_root: PathBuf,
    /// Repository below the workspace root.
    pub repo: PathBuf,
    /// Ref checked out detached.
    pub base_ref: String,
    /// Configured independent checks.
    pub checks: Vec<String>,
    /// Model reference for the writable developer agent.
    pub developer_model: String,
    /// Model reference for the read-only reviewer agent.
    pub reviewer_model: String,
    /// Build and command limits.
    pub limits: BuildLimits,
    /// Instance identity.
    pub instance: String,
    /// Digest of the effective settings for reload detection.
    pub digest: String,
}

impl CodingConfig {
    /// Parse and validate the operator settings.
    pub fn load(settings: &WorkerSettings) -> CodingResult<Self> {
        let values = &settings.file.settings;
        let root_value = values.get("workspace_root").ok_or_else(|| {
            ApiError::new(
                "coding.config_missing",
                "settings.workspace_root is required for the Coding package",
            )
        })?;
        let root = resolve_root(&settings.config_dir, root_value)?;
        let repo_value = values.get("repo").ok_or_else(|| {
            ApiError::new(
                "coding.config_missing",
                "settings.repo is required for the Coding package",
            )
        })?;
        let repo_path = repo::resolve_below(&root, repo_value)?;
        let repo = repo::canonical_repo(&root, &repo_path)?;
        let base_ref = values
            .get("base_ref")
            .cloned()
            .unwrap_or_else(|| "HEAD".into());
        if base_ref.trim().is_empty() || base_ref.starts_with('-') {
            return Err(ApiError::new(
                "coding.config_invalid",
                "settings.base_ref must be a non-empty git ref",
            ));
        }
        let checks = parse_checks(values.get("checks"));
        let parallelism = parse_number(values, "build_parallelism", 1_usize)?.clamp(1, 64);
        let seconds = parse_number(values, "max_build_seconds", 60_u64)?.clamp(1, 3_600);
        let output = parse_number(values, "max_output_bytes", 256 * 1024_usize)?
            .clamp(1_024, 16 * 1024 * 1024);
        let memory = parse_number(values, "max_memory_bytes", 1024_u64 * 1024 * 1024)?
            .clamp(64 * 1024 * 1024, 16 * 1024 * 1024 * 1024);
        let max_processes = parse_number(values, "max_processes", 8_192_u64)?.clamp(128, 32_768);
        let limits = BuildLimits {
            parallelism,
            timeout: Duration::from_secs(seconds),
            max_output_bytes: output,
            memory_bytes: memory,
            cpu_seconds: seconds,
            max_processes,
        };
        let developer_model = values
            .get("developer_model")
            .cloned()
            .unwrap_or_else(|| "deterministic".into());
        let reviewer_model = values
            .get("reviewer_model")
            .cloned()
            .unwrap_or_else(|| "deterministic".into());
        let instance = settings.file.instance.clone();
        let digest = common::bytes_digest(
            format!(
                "{}|{}|{}|{}|{}|{}|{}|{}",
                root.display(),
                repo.display(),
                base_ref,
                instance,
                developer_model,
                reviewer_model,
                checks.join("\u{1e}"),
                parallelism
            )
            .as_bytes(),
        );
        Ok(Self {
            workspace_root: root,
            repo,
            base_ref,
            checks,
            developer_model,
            reviewer_model,
            limits,
            instance,
            digest,
        })
    }
}

/// One instance-scoped coding service.
pub struct CodingServices {
    config: CodingConfig,
    worktree: Mutex<Worktree>,
    build: BuildService,
    repo_fingerprint: String,
    base_commit: String,
    write_owner: Mutex<Option<String>>,
    baseline: Mutex<Option<(String, String)>>,
}

impl CodingServices {
    /// Resolve settings, claim the instance worktree, and build the sandbox.
    pub fn open(settings: &WorkerSettings) -> CodingResult<Self> {
        let config = CodingConfig::load(settings)?;
        let mut worktree = Worktree::claim(
            &config.workspace_root,
            &config.repo,
            &config.instance,
            &config.base_ref,
        )?;
        let base_commit = worktree.git(&["rev-parse", &config.base_ref])?;
        let base_commit = base_commit.trim().to_string();
        if base_commit.is_empty() {
            worktree.release();
            return Err(ApiError::new(
                "coding.base_ref_invalid",
                "configured base ref does not resolve to a commit",
            ));
        }
        let build = match BuildService::new(worktree.path(), config.limits.clone()) {
            Ok(build) => build,
            Err(error) => {
                worktree.release();
                return Err(error);
            }
        };
        let repo_fingerprint = match repo::fingerprint(&config.repo) {
            Ok(value) => value,
            Err(error) => {
                worktree.release();
                return Err(error);
            }
        };
        Ok(Self {
            config,
            worktree: Mutex::new(worktree),
            build,
            repo_fingerprint,
            base_commit,
            write_owner: Mutex::new(None),
            baseline: Mutex::new(None),
        })
    }

    /// Effective settings.
    pub fn config(&self) -> &CodingConfig {
        &self.config
    }

    /// Owned worktree path.
    pub fn worktree_path(&self) -> PathBuf {
        self.worktree
            .lock()
            .expect("worktree lock is not poisoned")
            .path()
            .to_path_buf()
    }

    /// Monotonic base commit captured at provisioning.
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }

    /// Repository fingerprint captured before any task ran.
    pub fn repo_fingerprint(&self) -> &str {
        &self.repo_fingerprint
    }

    /// Sandbox command tool bound to the instance worktree.
    pub fn command_tool(&self) -> SandboxCommandTool {
        self.build.tool()
    }

    /// Configured check commands.
    pub fn checks(&self) -> &[String] {
        &self.config.checks
    }

    /// Build service used by evidence collection.
    pub fn build(&self) -> &BuildService {
        &self.build
    }

    /// Binary diff from the captured base commit to the current worktree.
    pub fn diff(&self) -> CodingResult<String> {
        self.worktree
            .lock()
            .expect("worktree lock is not poisoned")
            .git(&["diff", "--binary", &self.base_commit])
    }

    /// Porcelain status of the owned worktree.
    pub fn status(&self) -> CodingResult<String> {
        self.worktree
            .lock()
            .expect("worktree lock is not poisoned")
            .git(&["status", "--porcelain=v1"])
    }

    /// Capture the worktree state before this task writes.
    pub fn begin_task(&self) -> CodingResult<()> {
        let diff = self.diff()?;
        let status = self.status()?;
        *self.baseline.lock().expect("baseline lock is not poisoned") = Some((
            common::bytes_digest(diff.as_bytes()),
            common::bytes_digest(status.as_bytes()),
        ));
        Ok(())
    }

    /// Whether this task changed the checkout relative to its baseline.
    pub fn task_changed(&self) -> CodingResult<bool> {
        let baseline = self
            .baseline
            .lock()
            .expect("baseline lock is not poisoned")
            .clone()
            .ok_or_else(|| {
                ApiError::new(
                    "coding.baseline_missing",
                    "task baseline was not captured before execution",
                )
            })?;
        let diff = self.diff()?;
        let status = self.status()?;
        Ok(baseline
            != (
                common::bytes_digest(diff.as_bytes()),
                common::bytes_digest(status.as_bytes()),
            ))
    }

    /// Clear the task baseline after evidence collection.
    pub fn clear_baseline(&self) {
        *self.baseline.lock().expect("baseline lock is not poisoned") = None;
    }

    /// Recreate a checkout removed by cancellation cleanup.
    pub fn ensure_worktree(&self) -> CodingResult<()> {
        self.worktree
            .lock()
            .expect("worktree lock is not poisoned")
            .ensure(&self.config.base_ref)
    }

    /// Remove the checkout while keeping instance ownership. The next task
    /// recreates it at the same path.
    pub fn remove_checkout(&self) -> CodingResult<()> {
        self.worktree
            .lock()
            .expect("worktree lock is not poisoned")
            .remove_checkout()
    }

    /// Take exclusive write ownership for one task.
    pub fn acquire_write(&self, task_id: &str) -> CodingResult<()> {
        let mut owner = self
            .write_owner
            .lock()
            .expect("write ownership lock is not poisoned");
        if let Some(current) = owner.as_deref() {
            return Err(ApiError::new(
                "coding.workspace_busy",
                format!("task '{current}' already owns the writable worktree"),
            ));
        }
        *owner = Some(task_id.to_string());
        Ok(())
    }

    /// Release write ownership held by this task.
    pub fn release_write(&self, task_id: &str) {
        let mut owner = self
            .write_owner
            .lock()
            .expect("write ownership lock is not poisoned");
        if owner.as_deref() == Some(task_id) {
            *owner = None;
        }
    }

    /// Release the worktree and ownership file.
    pub fn shutdown(&self) {
        self.worktree
            .lock()
            .expect("worktree lock is not poisoned")
            .release();
    }

    /// Build a sandbox policy for ad-hoc checks in tests.
    pub fn sandbox_policy(&self) -> SandboxPolicy {
        SandboxPolicy::new(self.worktree_path())
            .with_timeout(self.config.limits.timeout)
            .with_output_limit(self.config.limits.max_output_bytes)
            .with_environment("CARGO_TARGET_DIR", "/workspace/target")
            .with_environment("CARGO_NET_OFFLINE", "true")
    }

    /// Settings map exposed to hooks, without secrets.
    pub fn public_settings(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("instance".into(), self.config.instance.clone()),
            ("base_ref".into(), self.config.base_ref.clone()),
            ("checks".into(), self.config.checks.join("\n")),
            (
                "worktree".into(),
                self.worktree_path().display().to_string(),
            ),
        ])
    }
}

impl Drop for CodingServices {
    fn drop(&mut self) {
        if let Ok(worktree) = self.worktree.lock() {
            let mut worktree = worktree;
            worktree.release();
        }
    }
}

fn resolve_root(config_dir: &Path, value: &str) -> CodingResult<PathBuf> {
    let requested = PathBuf::from(value);
    let joined = if requested.is_absolute() {
        requested
    } else {
        config_dir.join(requested)
    };
    let root = std::fs::canonicalize(&joined).map_err(|error| {
        ApiError::new(
            "coding.workspace_unavailable",
            format!("workspace_root is not readable: {error}"),
        )
    })?;
    if !root.is_dir() {
        return Err(ApiError::new(
            "coding.workspace_unavailable",
            "workspace_root must be a directory",
        ));
    }
    if root == Path::new("/") {
        return Err(ApiError::new(
            "coding.workspace_invalid",
            "workspace_root cannot be the host root",
        ));
    }
    Ok(root)
}

fn parse_number<T>(values: &BTreeMap<String, String>, key: &str, default: T) -> CodingResult<T>
where
    T: std::str::FromStr,
{
    match values.get(key) {
        None => Ok(default),
        Some(value) => value.parse::<T>().map_err(|_| {
            ApiError::new(
                "coding.config_invalid",
                format!("settings.{key} must be a positive integer"),
            )
        }),
    }
}
