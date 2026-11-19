//! Repository and worktree provisioning with exclusive instance ownership.
//!
//! Every writable assignment runs in its own git worktree below the configured
//! workspace root. An instance-level ownership file prevents two processes from
//! claiming the same checkout. Git is invoked with fixed argv; no shell, no
//! model-controlled flags, and repository instructions never select commands.

use common::ApiError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Result type used by package services.
pub type CodingResult<T> = Result<T, ApiError>;

/// One owned worktree and the instance lock that protects it.
#[derive(Debug)]
pub struct Worktree {
    repo: PathBuf,
    path: PathBuf,
    owner: PathBuf,
    released: bool,
}

impl Worktree {
    /// Claim `<root>/worktrees/<instance>` for this process and check out
    /// `base_ref` detached.
    pub fn claim(root: &Path, repo: &Path, instance: &str, base_ref: &str) -> CodingResult<Self> {
        validate_instance(instance)?;
        let repo = canonical_repo(root, repo)?;
        let worktrees = root.join("worktrees");
        fs::create_dir_all(&worktrees).map_err(|error| {
            ApiError::new(
                "coding.workspace_unavailable",
                format!("cannot create worktree root: {error}"),
            )
        })?;
        let owner = worktrees.join(format!("{instance}.owner"));
        claim_owner(&owner, instance)?;
        let path = worktrees.join(instance);
        if path.exists() {
            // A missing owner means the previous process did not shut down
            // cleanly. Remove only this instance's path.
            let _ = git(&repo, &["worktree", "remove", "--force", path_arg(&path)?]);
            fs::remove_dir_all(&path).map_err(|error| {
                ApiError::new(
                    "coding.workspace_conflict",
                    format!("cannot clear stale worktree: {error}"),
                )
            })?;
        }
        let _ = git(&repo, &["worktree", "prune"]);
        let target = path_arg(&path)?;
        if let Err(error) = git(&repo, &["worktree", "add", "--detach", target, base_ref]) {
            let _ = fs::remove_file(&owner);
            return Err(error);
        }
        Ok(Self {
            repo,
            path,
            owner,
            released: false,
        })
    }

    /// Checked-out worktree path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Recreate the checkout after a cancellation cleanup. The agent workspace
    /// root is the same absolute path, so re-adding is transparent.
    pub fn ensure(&mut self, base_ref: &str) -> CodingResult<()> {
        if self.path.join(".git").exists() {
            return Ok(());
        }
        let _ = git(&self.repo, &["worktree", "prune"]);
        let target = path_arg(&self.path)?;
        let _ = git(
            &self.repo,
            &["worktree", "add", "--detach", target, base_ref],
        )?;
        Ok(())
    }

    /// Remove the checkout while retaining instance ownership.
    pub fn remove_checkout(&mut self) -> CodingResult<()> {
        if self.released {
            return Ok(());
        }
        let target = path_arg(&self.path)?;
        let _ = git(&self.repo, &["worktree", "remove", "--force", target]);
        if self.path.exists() {
            fs::remove_dir_all(&self.path).map_err(|error| {
                ApiError::new(
                    "coding.cleanup_failed",
                    format!("cannot remove worktree: {error}"),
                )
            })?;
        }
        let _ = git(&self.repo, &["worktree", "prune"]);
        Ok(())
    }

    /// Remove the checkout, release ownership, and leave no instance paths.
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        let _ = self.remove_checkout();
        let _ = fs::remove_file(&self.owner);
        self.released = true;
    }

    /// Run a fixed-argument git command rooted at the worktree.
    pub fn git(&self, args: &[&str]) -> CodingResult<String> {
        git(&self.path, args)
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        self.release();
    }
}

/// Git command with fixed argv, cleared git redirection, and redacted errors.
pub fn git(cwd: &Path, args: &[&str]) -> CodingResult<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("advice.detachedHead=false")
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .map_err(|error| {
            ApiError::new(
                "coding.git_unavailable",
                format!("git could not start: {error}"),
            )
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(ApiError::new(
            "coding.git_failed",
            format!(
                "git {} failed: {}",
                args.first().copied().unwrap_or("command"),
                common::redact(&bounded(&detail, 2048))
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Resolve the repository against the authorized workspace root.
pub fn canonical_repo(root: &Path, repo: &Path) -> CodingResult<PathBuf> {
    let canonical = fs::canonicalize(repo).map_err(|error| {
        ApiError::new(
            "coding.repo_unavailable",
            format!("repository path is not readable: {error}"),
        )
    })?;
    if !canonical.starts_with(root) || canonical == root {
        return Err(ApiError::new(
            "coding.repo_outside_root",
            "repository must be a real directory below the configured workspace root",
        ));
    }
    if !canonical.join(".git").exists() {
        return Err(ApiError::new(
            "coding.repo_invalid",
            "repository root does not contain git metadata",
        ));
    }
    Ok(canonical)
}

/// Repository fingerprint used to detect changes outside the owned worktree.
pub fn fingerprint(repo: &Path) -> CodingResult<String> {
    let head = git(repo, &["rev-parse", "HEAD"])?;
    let status = git(repo, &["status", "--porcelain=v1"])?;
    Ok(format!("{head}\n{status}"))
}

fn claim_owner(owner: &Path, instance: &str) -> CodingResult<()> {
    match OpenOptions::new().write(true).create_new(true).open(owner) {
        Ok(mut file) => {
            let _ = writeln!(file, "instance={instance}\npid={}", std::process::id());
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let previous = fs::read_to_string(owner).unwrap_or_default();
            let live = previous
                .lines()
                .find_map(|line| line.strip_prefix("pid="))
                .and_then(|value| value.trim().parse::<u32>().ok())
                .is_some_and(pid_alive);
            if live {
                return Err(ApiError::new(
                    "coding.workspace_busy",
                    "another live instance owns this worktree",
                ));
            }
            fs::remove_file(owner).map_err(|error| {
                ApiError::new(
                    "coding.workspace_conflict",
                    format!("cannot clear stale instance ownership: {error}"),
                )
            })?;
            claim_owner(owner, instance)
        }
        Err(error) => Err(ApiError::new(
            "coding.workspace_unavailable",
            format!("cannot claim instance ownership: {error}"),
        )),
    }
}

fn pid_alive(pid: u32) -> bool {
    pid > 0 && Path::new("/proc").join(pid.to_string()).exists()
}

fn validate_instance(instance: &str) -> CodingResult<()> {
    if instance.is_empty()
        || instance.len() > 100
        || !instance.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
        || instance == "."
        || instance == ".."
    {
        return Err(ApiError::new(
            "coding.instance_invalid",
            "instance identity must be a 1-100 character path-safe identifier",
        ));
    }
    Ok(())
}

fn path_arg(path: &Path) -> CodingResult<&str> {
    path.to_str()
        .ok_or_else(|| ApiError::new("coding.path_invalid", "worktree path is not valid UTF-8"))
}

fn bounded(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

/// Normalize an operator-supplied path and keep it below the workspace root.
pub fn resolve_below(root: &Path, value: &str) -> CodingResult<PathBuf> {
    let requested = Path::new(value);
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let normalized = normalize(&joined);
    if !normalized.starts_with(root) {
        return Err(ApiError::new(
            "coding.path_outside_root",
            "configured repository path must stay below the workspace root",
        ));
    }
    Ok(normalized)
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_identity_is_path_safe() {
        assert!(validate_instance("coding-a_1.2").is_ok());
        for invalid in ["", "..", "../a", "a/b", "a b"] {
            assert_eq!(
                validate_instance(invalid).unwrap_err().code,
                "coding.instance_invalid"
            );
        }
    }

    #[test]
    fn repository_must_stay_below_the_workspace_root() {
        let root = tempfile::tempdir().unwrap();
        let path = resolve_below(root.path(), "../escape").unwrap_err();
        assert_eq!(path.code, "coding.path_outside_root");
        let path = resolve_below(root.path(), "repo").unwrap();
        assert_eq!(path, root.path().join("repo"));
    }
}
