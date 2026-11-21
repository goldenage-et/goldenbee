//! bubblewrap, prlimit, and process-group execution.

use super::{ExecOutput, Sandbox, SandboxError, SandboxProfile};
use std::future::Future;
use std::path::{Component, Path};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

impl Sandbox {
    /// Execute a shell command inside the boundary.
    pub async fn exec(
        &self,
        command: &str,
        working_dir: Option<&str>,
    ) -> Result<ExecOutput, SandboxError> {
        self.exec_with_cancel(command, working_dir, std::future::pending())
            .await
    }

    /// Execute a shell command and terminate its process group and descendants
    /// when `cancel` completes.
    pub async fn exec_with_cancel<F>(
        &self,
        command: &str,
        working_dir: Option<&str>,
        cancel: F,
    ) -> Result<ExecOutput, SandboxError>
    where
        F: Future<Output = ()>,
    {
        let guest_dir = self.guest_working_dir(working_dir)?;
        let argv = self.argv_for(command, &guest_dir, true)?;
        let mut process = Command::new(argv.program);
        process
            .args(argv.args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let started = Instant::now();
        let mut child = process.spawn().map_err(|error| {
            SandboxError::spawn(format!("sandboxed command could not start: {error}"))
        })?;
        let pid = child.id().ok_or_else(|| {
            SandboxError::spawn("the sandbox process did not expose an owned process id")
        })?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| SandboxError::io("sandbox stdout was not captured"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| SandboxError::io("sandbox stderr was not captured"))?;

        let read = read_both(
            &mut stdout,
            &mut stderr,
            self.policy.limits().max_output_bytes,
        );
        tokio::pin!(read);
        tokio::pin!(cancel);
        let deadline = tokio::time::sleep(self.policy.limits().timeout);
        tokio::pin!(deadline);

        enum Selected {
            Finished(((Vec<u8>, bool), (Vec<u8>, bool))),
            TimedOut,
            Cancelled,
        }
        let selected = tokio::select! {
            result = &mut read => Selected::Finished(result),
            _ = &mut deadline => Selected::TimedOut,
            _ = &mut cancel => Selected::Cancelled,
        };

        match selected {
            Selected::Finished(((stdout, stdout_truncated), (stderr, stderr_truncated))) => {
                if stdout_truncated || stderr_truncated {
                    self.terminate(&mut child, pid).await?;
                    return Err(SandboxError::output_limit(
                        self.policy.limits().max_output_bytes,
                    ));
                }
                let status = tokio::time::timeout(Duration::from_secs(2), child.wait())
                    .await
                    .map_err(|_| {
                        SandboxError::cleanup(
                            "the sandbox supervisor did not exit after output closed",
                        )
                    })?
                    .map_err(|error| {
                        SandboxError::io(format!("sandbox status could not be read: {error}"))
                    })?;
                Ok(ExecOutput {
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    exit_code: status.code().unwrap_or(-1),
                    duration: started.elapsed(),
                    timed_out: false,
                    cancelled: false,
                })
            }
            Selected::TimedOut | Selected::Cancelled => {
                self.terminate(&mut child, pid).await?;
                let timed_out = matches!(selected, Selected::TimedOut);
                let cancelled = matches!(selected, Selected::Cancelled);
                let ((stdout, _), (stderr, _)) = tokio::time::timeout(Duration::from_secs(2), read)
                    .await
                    .unwrap_or(((Vec::new(), false), (Vec::new(), false)));
                Ok(ExecOutput {
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    exit_code: -1,
                    duration: started.elapsed(),
                    timed_out,
                    cancelled,
                })
            }
        }
    }

    fn guest_working_dir(&self, working_dir: Option<&str>) -> Result<String, SandboxError> {
        let Some(relative) = working_dir.filter(|value| !value.is_empty()) else {
            return Ok("/workspace".into());
        };
        let path = Path::new(relative);
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(SandboxError::invalid_policy(
                "working_dir must stay inside the workspace",
            ));
        }
        Ok(format!("/workspace/{relative}"))
    }

    /// Program and arguments for one contained shell command.
    ///
    /// The process supervisor and PTY adapter reuse this exact vector, so a
    /// background or interactive command cannot choose a weaker boundary than
    /// the foreground executor.
    pub(crate) fn argv(
        &self,
        command: &str,
        working_dir: Option<&str>,
    ) -> Result<super::SandboxArgv, SandboxError> {
        let guest_dir = self.guest_working_dir(working_dir)?;
        self.argv_for(command, &guest_dir, true)
    }

    /// Program and arguments for one interactive PTY command.
    ///
    /// The PTY layer already creates the session and controlling terminal, so
    /// `--new-session` is omitted. Calling `setsid` twice fails, and the PTY is
    /// a per-session terminal with no host tty to inject into.
    pub(crate) fn pty_argv(
        &self,
        command: &str,
        working_dir: Option<&str>,
    ) -> Result<super::SandboxArgv, SandboxError> {
        let guest_dir = self.guest_working_dir(working_dir)?;
        self.argv_for(command, &guest_dir, false)
    }

    fn argv_for(
        &self,
        command: &str,
        guest_dir: &str,
        new_session: bool,
    ) -> Result<super::SandboxArgv, SandboxError> {
        let rules = self.policy.rules();
        rules.check_command(command).map_err(SandboxError::policy)?;
        let limits = self.policy.limits();
        let mut args: Vec<std::ffi::OsString> = Vec::new();
        let mut push = |arg: &str| args.push(std::ffi::OsString::from(arg));
        push(&format!("--cpu={}", limits.cpu_seconds));
        push(&format!("--as={}", limits.memory_bytes));
        push(&format!("--nproc={}", limits.max_processes));
        push(&format!("--fsize={}", limits.max_file_bytes));
        push(&format!("--nofile={}", limits.max_open_files));
        push("--");
        push(&self.policy.bubblewrap.to_string_lossy());
        push("--unshare-all");
        push("--unshare-user");
        if rules.network.enabled {
            // `--unshare-all` removes the network namespace; re-share it only
            // for an explicitly network-enabled policy.
            push("--share-net");
        }
        push("--uid");
        push(&self.policy.uid.to_string());
        push("--gid");
        push(&self.policy.gid.to_string());
        push("--die-with-parent");
        if new_session {
            push("--new-session");
        }
        push("--disable-userns");
        push("--cap-drop");
        push("ALL");
        push("--clearenv");
        for (key, value) in &self.policy.environment {
            push("--setenv");
            push(key);
            push(value);
        }
        if rules.network.enabled {
            if let Some(proxy) = &rules.network.proxy {
                for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
                    if !self.policy.environment.contains_key(key) {
                        push("--setenv");
                        push(key);
                        push(proxy);
                    }
                }
                if !rules.network.proxy_bypass.is_empty()
                    && !self.policy.environment.contains_key("NO_PROXY")
                {
                    push("--setenv");
                    push("NO_PROXY");
                    push(&rules.network.proxy_bypass.join(","));
                }
            }
        }
        push("--proc");
        push("/proc");
        push("--dev");
        push("/dev");
        push("--tmpfs");
        push("/tmp");
        for mount in &self.policy.read_only_mounts {
            push("--ro-bind");
            push(&mount.to_string_lossy());
            push(&mount.to_string_lossy());
        }
        if rules.network.enabled {
            // Resolver metadata is the only host `/etc` content needed for a
            // network-enabled command. Never mount the whole directory.
            for path in ["/etc/resolv.conf", "/etc/hosts"] {
                let path = Path::new(path);
                if path.exists() {
                    push("--ro-bind");
                    push(&path.to_string_lossy());
                    push(&path.to_string_lossy());
                }
            }
        }
        // The workspace mount expresses the filesystem profile. `read-only`
        // keeps every declared path visible but unwritable; `workspace-write`
        // exposes the whole workspace or only the authorized roots.
        match rules.profile {
            SandboxProfile::ReadOnly => {
                push("--ro-bind");
                push(&self.policy.workspace.to_string_lossy());
                push("/workspace");
            }
            SandboxProfile::WorkspaceWrite => {
                let roots = self.writable_roots();
                if roots.is_empty() {
                    push("--bind");
                    push(&self.policy.workspace.to_string_lossy());
                    push("/workspace");
                } else {
                    push("--ro-bind");
                    push(&self.policy.workspace.to_string_lossy());
                    push("/workspace");
                    for root in roots {
                        push("--bind");
                        push(&root.host.to_string_lossy());
                        push(&root.guest.to_string_lossy());
                    }
                }
                for protected in self.protected_paths() {
                    push("--ro-bind");
                    push(&protected.host.to_string_lossy());
                    push(&protected.guest.to_string_lossy());
                }
            }
            SandboxProfile::FullAccess => {
                push("--bind");
                push(&self.policy.workspace.to_string_lossy());
                push("/workspace");
            }
        }
        push("--chdir");
        push(guest_dir);
        push("--");
        push("/bin/sh");
        push("-c");
        push(command);
        Ok(super::SandboxArgv {
            program: self.policy.prlimit.clone(),
            args,
        })
    }

    /// Terminate one process group with the configured `kill` mechanism.
    ///
    /// The group signal is the same mechanism the foreground executor uses, so
    /// a backed-out background process cannot survive its own tool contract.
    pub(crate) fn kill_group(&self, pid: u32) {
        let _ = std::process::Command::new(&self.policy.kill)
            .arg("-KILL")
            .arg("--")
            .arg(format!("-{pid}"))
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    async fn terminate(&self, child: &mut Child, pid: u32) -> Result<(), SandboxError> {
        // Kill the process group first so cooperative descendants receive the
        // signal together, then kill the supervisor. `--die-with-parent` and
        // the PID namespace guarantee the remaining descendants die with it.
        self.kill_group(pid);
        let _ = child.start_kill();
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(SandboxError::cleanup(format!(
                "the sandbox supervisor could not be reaped: {error}"
            ))),
            Err(_) => Err(SandboxError::cleanup(
                "the sandbox supervisor did not terminate after SIGKILL",
            )),
        }
    }
}

async fn read_capped<R>(reader: &mut R, limit: usize) -> (Vec<u8>, bool)
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => return (captured, false),
            Ok(read) => {
                let remaining = limit.saturating_sub(captured.len());
                if read > remaining {
                    captured.extend_from_slice(&buffer[..remaining]);
                    return (captured, true);
                }
                captured.extend_from_slice(&buffer[..read]);
            }
            Err(_) => return (captured, false),
        }
    }
}

async fn read_both<R1, R2>(
    stdout: &mut R1,
    stderr: &mut R2,
    limit: usize,
) -> ((Vec<u8>, bool), (Vec<u8>, bool))
where
    R1: AsyncRead + Unpin,
    R2: AsyncRead + Unpin,
{
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut out_done = false;
    let mut err_done = false;
    let out_future = read_capped(stdout, limit);
    let err_future = read_capped(stderr, limit);
    tokio::pin!(out_future, err_future);
    while !(out_done && err_done) {
        tokio::select! {
            (data, truncated) = &mut out_future, if !out_done => {
                out_done = true;
                out = data;
                if truncated {
                    return ((out, true), (err, false));
                }
            }
            (data, truncated) = &mut err_future, if !err_done => {
                err_done = true;
                err = data;
                if truncated {
                    return ((out, false), (err, true));
                }
            }
        }
    }
    ((out, false), (err, false))
}
