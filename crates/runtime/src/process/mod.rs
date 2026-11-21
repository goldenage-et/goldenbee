//! Task-owned process and PTY supervision.
//!
//! One supervisor owns every background command started by the runtime process
//! tools. The model is the Codex `unified_exec` contract: a start call returns a
//! stable session id while the process runs, later calls wait for or terminate
//! the same id, and output is drained incrementally from a bounded ring buffer.
//!
//! Containment does not change with the mode. Every pipe and PTY child executes
//! the exact `prlimit`/`bubblewrap` vector produced by [`crate::sandbox`]. A
//! task cancellation token kills the owned process group, and Worker shutdown
//! kills anything still running.

use crate::sandbox::{Sandbox, SandboxError, SandboxPolicy};
use crate::Runtime;
use common::Task;
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify};

/// Default upper bound for concurrently running background processes.
const DEFAULT_MAX_PROCESSES: usize = 8;
/// Default per-stream retained output bytes.
const DEFAULT_MAX_BUFFER_BYTES: usize = 1024 * 1024;
/// Default lifetime of one background process.
const DEFAULT_MAX_LIFETIME: Duration = Duration::from_secs(300);
/// Default time an exited process remains readable by id.
const DEFAULT_RETENTION: Duration = Duration::from_secs(300);
/// Default number of `tool.output` events emitted per process.
const DEFAULT_MAX_OUTPUT_EVENTS: usize = 256;
/// How long a terminate request waits for an observed exit.
const KILL_CONFIRM: Duration = Duration::from_secs(5);

/// Process and PTY policy declared by a Worker Package.
#[derive(Debug, Clone)]
pub struct ProcessPolicy {
    sandbox: SandboxPolicy,
    max_processes: usize,
    max_buffer_bytes: usize,
    max_lifetime: Duration,
    retention: Duration,
    start_yield: Duration,
    max_output_events: usize,
}

impl ProcessPolicy {
    /// Build a policy around one explicit sandbox boundary.
    pub fn new(sandbox: SandboxPolicy) -> Self {
        Self {
            sandbox,
            max_processes: DEFAULT_MAX_PROCESSES,
            max_buffer_bytes: DEFAULT_MAX_BUFFER_BYTES,
            max_lifetime: DEFAULT_MAX_LIFETIME,
            retention: DEFAULT_RETENTION,
            start_yield: Duration::from_millis(250),
            max_output_events: DEFAULT_MAX_OUTPUT_EVENTS,
        }
    }

    /// Maximum concurrently running processes owned by this Worker.
    pub fn max_processes(mut self, max_processes: usize) -> Self {
        self.max_processes = max_processes.max(1);
        self
    }

    /// Retained output ceiling per stream.
    pub fn max_buffer_bytes(mut self, max_buffer_bytes: usize) -> Self {
        self.max_buffer_bytes = max_buffer_bytes.max(1024);
        self
    }

    /// Maximum wall-clock lifetime of one background process.
    pub fn max_lifetime(mut self, max_lifetime: Duration) -> Self {
        self.max_lifetime = max_lifetime;
        self
    }

    /// How long an exited process remains readable by id.
    pub fn retention(mut self, retention: Duration) -> Self {
        self.retention = retention;
        self
    }

    /// How long a start call yields before returning a running id.
    pub fn start_yield(mut self, start_yield: Duration) -> Self {
        self.start_yield = start_yield;
        self
    }

    /// Sandbox boundary used for every child.
    pub fn sandbox(&self) -> &SandboxPolicy {
        &self.sandbox
    }

    /// Foreground command deadline from the sandbox limits.
    pub fn command_timeout(&self) -> Duration {
        self.sandbox.limits().timeout
    }

    /// How long a start call yields by default.
    pub fn default_yield(&self) -> Duration {
        self.start_yield
    }
}

/// Process execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessMode {
    /// Piped stdout and stderr.
    Pipe,
    /// One merged terminal stream.
    Pty,
}

impl ProcessMode {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pipe => "pipe",
            Self::Pty => "pty",
        }
    }
}

/// Truthful process state at the moment of observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Still running.
    Running,
    /// Exited on its own or after a requested signal.
    Exited,
    /// A terminate request was sent but the outcome could not be confirmed.
    Unknown,
}

impl ProcessState {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Unknown => "unknown",
        }
    }
}

/// One observation of a supervised process.
#[derive(Debug, Clone)]
pub struct ProcessView {
    /// Stable runtime process id.
    pub id: String,
    /// Mode selected at start.
    pub mode: ProcessMode,
    /// Current state.
    pub state: ProcessState,
    /// Why termination was requested, when the runtime requested it.
    pub reason: Option<&'static str>,
    /// Observed exit code, when one exists.
    pub exit_code: Option<i32>,
    /// Host process id used for process-group termination.
    pub pid: Option<u32>,
    /// Output captured since the previous observation.
    pub stdout: String,
    /// Standard error captured since the previous observation.
    pub stderr: String,
    /// Bytes dropped from the bounded buffer before this observation.
    pub dropped_bytes: usize,
    /// The retained buffer is incomplete.
    pub truncated: bool,
    /// Elapsed time since start.
    pub duration_ms: u64,
    /// The wait deadline expired while the process was still running.
    pub wait_timed_out: bool,
    /// A runtime terminate request was sent for this process.
    pub kill_requested: bool,
}

impl ProcessView {
    /// Stable JSON result for a tool call.
    pub fn into_value(self) -> serde_json::Value {
        json!({
            "process_id": self.id,
            "mode": self.mode.as_str(),
            "state": self.state.as_str(),
            "reason": self.reason,
            "exit_code": self.exit_code,
            "pid": self.pid,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "dropped_bytes": self.dropped_bytes,
            "truncated": self.truncated,
            "duration_ms": self.duration_ms,
            "wait_timed_out": self.wait_timed_out,
            "kill_requested": self.kill_requested,
        })
    }
}

/// Failure from the process supervisor.
#[derive(Debug, Clone)]
pub struct ProcessError {
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Redacted detail.
    pub message: String,
}

impl ProcessError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Stable JSON error for a tool result.
    pub fn into_value(self) -> serde_json::Value {
        json!({
            "error": self.code,
            "message": common::redact(&self.message),
        })
    }
}

/// One supervisor per Worker runtime plus the validated boundary its children
/// enter. Tools share the same validated handle, so a later policy change cannot
/// silently weaken a running session.
#[derive(Clone)]
pub struct ProcessRuntime {
    policy: ProcessPolicy,
    sandbox: std::result::Result<Arc<Sandbox>, SandboxError>,
}

impl ProcessRuntime {
    /// Validate the sandbox once at Worker startup.
    pub fn new(policy: ProcessPolicy) -> Self {
        let sandbox = Sandbox::new(policy.sandbox().clone()).map(Arc::new);
        Self { policy, sandbox }
    }

    /// Effective process policy.
    pub fn policy(&self) -> &ProcessPolicy {
        &self.policy
    }

    /// Validated sandbox boundary, or the fail-closed reason it is unavailable.
    pub fn sandbox(&self) -> std::result::Result<&Arc<Sandbox>, ProcessError> {
        self.sandbox.as_ref().map_err(|error| ProcessError {
            code: error.tool_code(),
            message: error.message.clone(),
        })
    }
}

/// One process supervisor per Worker runtime.
#[derive(Clone)]
pub struct ProcessSupervisor {
    inner: Arc<SupervisorInner>,
}

struct SupervisorInner {
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    seq: AtomicU64,
    /// Starts that passed the capacity check but are not yet in `sessions`.
    reserved: AtomicUsize,
}

impl Default for ProcessSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSupervisor {
    /// Create an empty supervisor.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                sessions: Mutex::new(BTreeMap::new()),
                seq: AtomicU64::new(1),
                reserved: AtomicUsize::new(0),
            }),
        }
    }

    /// Start a piped command and return its observation after the yield window.
    #[allow(clippy::too_many_arguments)]
    pub async fn start_pipe(
        &self,
        runtime: &Runtime,
        task: &Task,
        sandbox: &Arc<Sandbox>,
        policy: &ProcessPolicy,
        tool: &'static str,
        command: &str,
        working_dir: Option<&str>,
        yield_ms: u64,
    ) -> Result<ProcessView, ProcessError> {
        let argv = sandbox.argv(command, working_dir).map_err(sandbox_error)?;
        let reservation = self.ensure_capacity(policy).await?;
        let mut child = tokio::process::Command::new(&argv.program)
            .args(&argv.args)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false)
            .process_group(0)
            .spawn()
            .map_err(|error| {
                ProcessError::new("process.spawn", format!("cannot start: {error}"))
            })?;
        let pid = child
            .id()
            .ok_or_else(|| ProcessError::new("process.spawn", "the child exposed no pid"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProcessError::new("process.spawn", "stdout was not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ProcessError::new("process.spawn", "stderr was not captured"))?;
        let session = self
            .insert(task, tool, ProcessMode::Pipe, Some(pid), sandbox.clone())
            .await;
        drop(reservation);
        let id = session.id().to_string();
        if let Err(error) = self.announce_start(runtime, &session).await {
            let _ = self.kill(&id, "evidence_failed").await;
            return Err(error);
        }
        let max_buffer = policy.max_buffer_bytes / 2;
        tokio::spawn(pump(
            session.clone(),
            runtime.clone(),
            Stream::Stdout,
            stdout,
            max_buffer,
            policy.max_output_events,
        ));
        tokio::spawn(pump(
            session.clone(),
            runtime.clone(),
            Stream::Stderr,
            stderr,
            max_buffer,
            policy.max_output_events,
        ));
        // The reaper owns the child. Killing by process group needs no child
        // handle, so a wait never blocks termination.
        let reaper_session = session.clone();
        let reaper_runtime = runtime.clone();
        let lifetime = policy.max_lifetime;
        tokio::spawn(async move {
            let outcome = tokio::select! {
                status = child.wait() => status,
                _ = tokio::time::sleep(lifetime) => {
                    reaper_session.kill_group().await;
                    let _ = reaper_runtime
                        .append_event(
                            &reaper_session.task_id,
                            "process.terminated",
                            json!({
                                "process_id": reaper_session.id,
                                "tool": reaper_session.tool,
                                "reason": "lifetime",
                                "state": "unknown",
                            }),
                            true,
                        )
                        .await;
                    child.wait().await
                }
            };
            let code = outcome.ok().and_then(|status| status.code());
            if reaper_session.finish_natural(code).await {
                announce_exit(&reaper_runtime, &reaper_session, code).await;
            }
        });
        watch_cancellation(runtime, &session).await;
        schedule_retention(self.clone(), session.clone(), policy.retention);
        self.observe(&id, Duration::from_millis(yield_ms)).await
    }

    /// Start an interactive PTY command.
    #[allow(clippy::too_many_arguments)]
    pub async fn start_pty(
        &self,
        runtime: &Runtime,
        task: &Task,
        sandbox: &Arc<Sandbox>,
        policy: &ProcessPolicy,
        tool: &'static str,
        command: &str,
        working_dir: Option<&str>,
        yield_ms: u64,
    ) -> Result<ProcessView, ProcessError> {
        let argv = sandbox
            .pty_argv(command, working_dir)
            .map_err(sandbox_error)?;
        let reservation = self.ensure_capacity(policy).await?;
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| {
                ProcessError::new("process.spawn", format!("cannot open a PTY: {error}"))
            })?;
        let mut builder = portable_pty::CommandBuilder::new(&argv.program);
        builder.args(&argv.args);
        builder.env_clear();
        let child = pair.slave.spawn_command(builder).map_err(|error| {
            ProcessError::new(
                "process.spawn",
                format!("cannot start the PTY command: {error}"),
            )
        })?;
        drop(pair.slave);
        let pid = child.process_id();
        let pgid = pair.master.process_group_leader();
        let reader = pair.master.try_clone_reader().map_err(|error| {
            ProcessError::new("process.spawn", format!("PTY reader unavailable: {error}"))
        })?;
        let writer = pair.master.take_writer().map_err(|error| {
            ProcessError::new("process.spawn", format!("PTY writer unavailable: {error}"))
        })?;
        let killer = child.clone_killer();
        let session = self
            .insert(task, tool, ProcessMode::Pty, pid, sandbox.clone())
            .await;
        drop(reservation);
        let id = session.id().to_string();
        {
            let mut handle = session.handle.lock().await;
            if let Handle::Pty(ref mut pty) = *handle {
                pty.master = Some(pair.master);
                pty.writer = Some(writer);
                pty.killer = Some(killer);
                pty.pgid = pgid;
            }
        }
        if let Err(error) = self.announce_start(runtime, &session).await {
            let _ = self.kill(&id, "evidence_failed").await;
            return Err(error);
        }
        let (chunks_tx, mut chunks_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        if let Err(error) = std::thread::Builder::new()
            .name(format!("pty-reader-{}", session.id()))
            .spawn(move || {
                let mut reader = reader;
                let mut buffer = [0_u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            if chunks_tx.send(buffer[..read].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
        {
            let _ = self.kill(&id, "spawn_failed").await;
            return Err(ProcessError::new(
                "process.spawn",
                format!("PTY reader thread failed: {error}"),
            ));
        }
        let pump_session = session.clone();
        let pump_runtime = runtime.clone();
        let max_buffer = policy.max_buffer_bytes;
        let max_events = policy.max_output_events;
        tokio::spawn(async move {
            while let Some(chunk) = chunks_rx.recv().await {
                push_output(
                    &pump_session,
                    &pump_runtime,
                    Stream::Stdout,
                    &chunk,
                    max_buffer,
                    max_events,
                )
                .await;
            }
            pump_session.readers.fetch_sub(1, Ordering::SeqCst);
        });
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel::<Option<i32>>();
        let mut wait_child = child;
        if let Err(error) = std::thread::Builder::new()
            .name(format!("pty-wait-{}", session.id()))
            .spawn(move || {
                let code = wait_child
                    .wait()
                    .ok()
                    .map(|status| status.exit_code() as i32);
                let _ = exit_tx.send(code);
            })
        {
            let _ = self.kill(&id, "spawn_failed").await;
            return Err(ProcessError::new(
                "process.spawn",
                format!("PTY wait thread failed: {error}"),
            ));
        }
        let wait_session = session.clone();
        let wait_runtime = runtime.clone();
        tokio::spawn(async move {
            if let Some(code) = exit_rx.recv().await {
                if wait_session.finish_natural(code).await {
                    announce_exit(&wait_runtime, &wait_session, code).await;
                }
            }
        });
        watch_cancellation(runtime, &session).await;
        watch_lifetime(runtime, &session, policy.max_lifetime);
        schedule_retention(self.clone(), session.clone(), policy.retention);
        self.observe(&id, Duration::from_millis(yield_ms)).await
    }

    /// Write input to an interactive PTY.
    pub async fn write_stdin(&self, id: &str, input: &str) -> Result<(), ProcessError> {
        let session = self.session(id).await?;
        session
            .write(input)
            .await
            .map_err(|error| ProcessError::new("process.write", error))
    }

    /// Resize one PTY.
    pub async fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<(), ProcessError> {
        let session = self.session(id).await?;
        session
            .resize(rows.max(1), cols.max(1))
            .await
            .map_err(|error| ProcessError::new("process.resize", error))
    }

    /// Wait for a process to exit, or return its running state at the deadline.
    pub async fn wait(&self, id: &str, timeout: Duration) -> Result<ProcessView, ProcessError> {
        self.observe(id, timeout).await
    }

    /// Terminate one process and report what was actually observed.
    pub async fn kill(&self, id: &str, reason: &'static str) -> Result<ProcessView, ProcessError> {
        let session = self.session(id).await?;
        session.request_kill();
        // Signal only a process that is still running. Once reaped, the host pid
        // may be reused, and a group signal could reach an unrelated process.
        if session.is_running().await {
            session.kill_group().await;
        }
        let deadline = Instant::now() + KILL_CONFIRM;
        loop {
            {
                let status = session.status.lock().await;
                if !matches!(*status, Status::Running { .. }) {
                    drop(status);
                    session.await_readers(Duration::from_secs(1)).await;
                    let mut view = session.view().await;
                    view.reason = Some(reason);
                    return Ok(view);
                }
            }
            if Instant::now() >= deadline {
                let mut status = session.status.lock().await;
                if matches!(*status, Status::Running { .. }) {
                    *status = Status::Unknown {
                        reason,
                        at: Instant::now(),
                    };
                    session.notify.notify_waiters();
                }
                drop(status);
                let mut view = session.view().await;
                view.reason = Some(reason);
                return Ok(view);
            }
            let _ =
                tokio::time::timeout(Duration::from_millis(25), session.notify.notified()).await;
        }
    }

    /// Remove one finished session. Running processes are kept.
    pub async fn discard(&self, id: &str) {
        let mut sessions = self.inner.sessions.lock().await;
        let finished = match sessions.get(id) {
            Some(session) => !matches!(*session.status.lock().await, Status::Running { .. }),
            None => false,
        };
        if finished {
            sessions.remove(id);
        }
    }

    /// Terminate every process group and clear the table.
    pub async fn shutdown(&self) {
        let sessions: Vec<Arc<Session>> = {
            let sessions = self.inner.sessions.lock().await;
            sessions.values().cloned().collect()
        };
        for session in &sessions {
            session.request_kill();
            if session.is_running().await {
                session.kill_group().await;
            }
        }
        // Give the reapers a bounded chance to observe the exit.
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let running = {
                let sessions = self.inner.sessions.lock().await;
                let mut running = false;
                for session in sessions.values() {
                    if matches!(*session.status.lock().await, Status::Running { .. }) {
                        running = true;
                        break;
                    }
                }
                running
            };
            if !running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.inner.sessions.lock().await.clear();
    }

    /// Number of currently running owned processes.
    pub async fn running(&self) -> usize {
        let sessions = self.inner.sessions.lock().await;
        let mut count = 0;
        for session in sessions.values() {
            if matches!(*session.status.lock().await, Status::Running { .. }) {
                count += 1;
            }
        }
        count
    }

    /// Whether a process id belongs to this task.
    pub async fn owns(&self, id: &str, task_id: &str) -> bool {
        self.inner
            .sessions
            .lock()
            .await
            .get(id)
            .is_some_and(|session| session.task_id == task_id)
    }

    async fn ensure_capacity(&self, policy: &ProcessPolicy) -> Result<Reservation, ProcessError> {
        loop {
            let reserved = self.inner.reserved.load(Ordering::SeqCst);
            if self.running().await + reserved >= policy.max_processes {
                return Err(ProcessError::new(
                    "process.limit",
                    format!(
                        "the Worker already owns {} running processes",
                        policy.max_processes
                    ),
                ));
            }
            if self
                .inner
                .reserved
                .compare_exchange(reserved, reserved + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Ok(Reservation {
                    inner: self.inner.clone(),
                });
            }
        }
    }

    async fn insert(
        &self,
        task: &Task,
        tool: &'static str,
        mode: ProcessMode,
        pid: Option<u32>,
        sandbox: Arc<Sandbox>,
    ) -> Arc<Session> {
        let mut sessions = self.inner.sessions.lock().await;
        let id = format!("proc_{}", self.inner.seq.fetch_add(1, Ordering::SeqCst));
        let session = Arc::new(Session {
            id: id.clone(),
            task_id: task.id.clone(),
            tool,
            mode,
            pid,
            started: Instant::now(),
            sandbox,
            status: Mutex::new(Status::Running {
                started: Instant::now(),
            }),
            notify: Notify::new(),
            stdout: Mutex::new(Buffer::default()),
            stderr: Mutex::new(Buffer::default()),
            events: AtomicUsize::new(0),
            kill_requested: AtomicBool::new(false),
            readers: AtomicUsize::new(if mode == ProcessMode::Pty { 1 } else { 2 }),
            handle: Mutex::new(if mode == ProcessMode::Pty {
                Handle::Pty(PtyHandle::default())
            } else {
                Handle::Pipe
            }),
        });
        sessions.insert(id, session.clone());
        session
    }

    async fn session(&self, id: &str) -> Result<Arc<Session>, ProcessError> {
        self.inner
            .sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| {
                ProcessError::new(
                    "process.not_found",
                    "no process with this id is owned by the Worker",
                )
            })
    }

    async fn announce_start(
        &self,
        runtime: &Runtime,
        session: &Session,
    ) -> Result<(), ProcessError> {
        let payload = json!({
            "process_id": session.id,
            "task_id": session.task_id,
            "tool": session.tool,
            "mode": session.mode.as_str(),
            "pid": session.pid,
        });
        runtime
            .append_event(&session.task_id, "process.started", payload, true)
            .await
            .map(|_| ())
            .map_err(|error| {
                ProcessError::new(
                    "process.evidence_failed",
                    format!("the process start could not be recorded: {}", error.code),
                )
            })
    }

    async fn observe(&self, id: &str, timeout: Duration) -> Result<ProcessView, ProcessError> {
        let session = self.session(id).await?;
        let deadline = Instant::now() + timeout;
        loop {
            {
                let status = session.status.lock().await;
                if !matches!(*status, Status::Running { .. }) {
                    drop(status);
                    session.await_readers(Duration::from_secs(1)).await;
                    return Ok(session.view().await);
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let mut view = session.view().await;
                view.wait_timed_out = true;
                return Ok(view);
            }
            let _ = tokio::time::timeout(remaining, session.notify.notified()).await;
        }
    }
}

/// One capacity slot held from the start check until the session is visible.
struct Reservation {
    inner: Arc<SupervisorInner>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.inner.reserved.fetch_sub(1, Ordering::SeqCst);
    }
}

fn schedule_retention(supervisor: ProcessSupervisor, session: Arc<Session>, retention: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(retention).await;
        let mut sessions = supervisor.inner.sessions.lock().await;
        if sessions
            .get(&session.id)
            .is_some_and(|current| Arc::ptr_eq(current, &session))
        {
            let finished = !matches!(*session.status.lock().await, Status::Running { .. });
            if finished {
                sessions.remove(&session.id);
            }
        }
    });
}

async fn watch_cancellation(runtime: &Runtime, session: &Arc<Session>) {
    let Some(token) = runtime.cancel_token(&session.task_id).await else {
        return;
    };
    let runtime = runtime.clone();
    let session = session.clone();
    tokio::spawn(async move {
        token.cancelled().await;
        session.request_kill();
        if session.is_running().await {
            session.kill_group().await;
        }
        let deadline = Instant::now() + KILL_CONFIRM;
        while Instant::now() < deadline {
            if !matches!(*session.status.lock().await, Status::Running { .. }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let state = {
            let status = session.status.lock().await;
            if matches!(*status, Status::Running { .. }) {
                ProcessState::Unknown
            } else {
                ProcessState::Exited
            }
        };
        let _ = runtime
            .append_event(
                &session.task_id,
                "process.terminated",
                json!({
                    "process_id": session.id,
                    "tool": session.tool,
                    "reason": "cancelled",
                    "state": state.as_str(),
                }),
                true,
            )
            .await;
    });
}

fn watch_lifetime(runtime: &Runtime, session: &Arc<Session>, lifetime: Duration) {
    if lifetime.is_zero() {
        return;
    }
    let runtime = runtime.clone();
    let session = session.clone();
    tokio::spawn(async move {
        tokio::time::sleep(lifetime).await;
        if session.is_running().await {
            session.request_kill();
            session.kill_group().await;
            let _ = runtime
                .append_event(
                    &session.task_id,
                    "process.terminated",
                    json!({
                        "process_id": session.id,
                        "tool": session.tool,
                        "reason": "lifetime",
                        "state": "unknown",
                    }),
                    true,
                )
                .await;
        }
    });
}

async fn announce_exit(runtime: &Runtime, session: &Session, code: Option<i32>) {
    let _ = runtime
        .append_event(
            &session.task_id,
            "process.exited",
            json!({
                "process_id": session.id,
                "tool": session.tool,
                "exit_code": code,
                "duration_ms": session.started.elapsed().as_millis() as u64,
            }),
            true,
        )
        .await;
}

fn sandbox_error(error: SandboxError) -> ProcessError {
    ProcessError::new(error.tool_code(), error.message)
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

async fn pump<R>(
    session: Arc<Session>,
    runtime: Runtime,
    stream: Stream,
    mut reader: R,
    max_buffer: usize,
    max_events: usize,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                push_output(
                    &session,
                    &runtime,
                    stream,
                    &buffer[..read],
                    max_buffer,
                    max_events,
                )
                .await;
            }
        }
    }
    session.readers.fetch_sub(1, Ordering::SeqCst);
}

async fn push_output(
    session: &Session,
    runtime: &Runtime,
    stream: Stream,
    bytes: &[u8],
    max_buffer: usize,
    max_events: usize,
) {
    let (chunk, dropped) = {
        let mut buffer = match stream {
            Stream::Stdout => session.stdout.lock().await,
            Stream::Stderr => session.stderr.lock().await,
        };
        let chunk = buffer.push(bytes, max_buffer);
        (chunk, buffer.dropped)
    };
    let emitted = session.events.fetch_add(1, Ordering::SeqCst);
    if emitted >= max_events {
        return;
    }
    let text = String::from_utf8_lossy(&chunk);
    let redacted = common::redact(&text);
    let _ = runtime
        .append_event(
            &session.task_id,
            "tool.output",
            json!({
                "process_id": session.id,
                "tool": session.tool,
                "stream": stream.as_str(),
                "chunk": redacted,
                "dropped_bytes": dropped,
                "seq": emitted + 1,
            }),
            true,
        )
        .await;
}

enum Status {
    Running { started: Instant },
    Exited { code: Option<i32>, at: Instant },
    Unknown { reason: &'static str, at: Instant },
}

struct Session {
    id: String,
    task_id: String,
    tool: &'static str,
    mode: ProcessMode,
    pid: Option<u32>,
    started: Instant,
    sandbox: Arc<Sandbox>,
    status: Mutex<Status>,
    notify: Notify,
    stdout: Mutex<Buffer>,
    stderr: Mutex<Buffer>,
    events: AtomicUsize,
    kill_requested: AtomicBool,
    readers: AtomicUsize,
    handle: Mutex<Handle>,
}

impl Session {
    fn id(&self) -> &str {
        &self.id
    }

    async fn finish_natural(&self, code: Option<i32>) -> bool {
        let mut status = self.status.lock().await;
        if matches!(*status, Status::Running { .. }) {
            *status = Status::Exited {
                code,
                at: Instant::now(),
            };
            self.notify.notify_waiters();
            true
        } else {
            false
        }
    }

    /// Wait until every output reader has observed EOF.
    async fn await_readers(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while self.readers.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn request_kill(&self) {
        self.kill_requested.store(true, Ordering::SeqCst);
    }

    async fn is_running(&self) -> bool {
        matches!(*self.status.lock().await, Status::Running { .. })
    }

    async fn kill_group(&self) {
        if let Some(pid) = self.pid {
            self.sandbox.kill_group(pid);
        }
        let mut handle = self.handle.lock().await;
        if let Handle::Pty(pty) = &mut *handle {
            if let Some(killer) = pty.killer.as_mut() {
                let _ = killer.kill();
            }
            if let Some(pgid) = pty.pgid {
                if pgid > 0 {
                    self.sandbox.kill_group(pgid as u32);
                }
            }
        }
    }

    async fn write(&self, input: &str) -> Result<(), String> {
        let mut handle = self.handle.lock().await;
        let Handle::Pty(pty) = &mut *handle else {
            return Err("stdin is only available for PTY sessions".into());
        };
        let writer = pty
            .writer
            .as_mut()
            .ok_or_else(|| "the PTY writer is closed".to_string())?;
        writer
            .write_all(input.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|error| format!("PTY write failed: {error}"))
    }

    async fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        let mut handle = self.handle.lock().await;
        let Handle::Pty(pty) = &mut *handle else {
            return Err("resize is only available for PTY sessions".into());
        };
        let master = pty
            .master
            .as_mut()
            .ok_or_else(|| "the PTY is closed".to_string())?;
        master
            .resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("PTY resize failed: {error}"))
    }

    async fn view(&self) -> ProcessView {
        let (state, reason, exit_code, duration) = {
            let status = self.status.lock().await;
            match &*status {
                Status::Running { started } => {
                    (ProcessState::Running, None, None, started.elapsed())
                }
                Status::Exited { code, at } => (
                    ProcessState::Exited,
                    None,
                    *code,
                    at.saturating_duration_since(self.started),
                ),
                Status::Unknown { reason, at } => (
                    ProcessState::Unknown,
                    Some(*reason),
                    None,
                    at.saturating_duration_since(self.started),
                ),
            }
        };
        let (stdout, stdout_dropped, stdout_truncated) = self.stdout.lock().await.drain();
        let (stderr, stderr_dropped, stderr_truncated) = self.stderr.lock().await.drain();
        ProcessView {
            id: self.id.clone(),
            mode: self.mode,
            state,
            reason,
            exit_code,
            pid: self.pid,
            stdout,
            stderr,
            dropped_bytes: stdout_dropped + stderr_dropped,
            truncated: stdout_truncated || stderr_truncated,
            duration_ms: duration.as_millis() as u64,
            wait_timed_out: false,
            kill_requested: self.kill_requested.load(Ordering::SeqCst),
        }
    }
}

enum Handle {
    Pipe,
    Pty(PtyHandle),
}

#[derive(Default)]
struct PtyHandle {
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    killer: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    pgid: Option<i32>,
}

#[derive(Default)]
struct Buffer {
    data: VecDeque<u8>,
    dropped: usize,
    truncated: bool,
}

impl Buffer {
    fn push(&mut self, bytes: &[u8], max: usize) -> Vec<u8> {
        for byte in bytes {
            if self.data.len() >= max {
                self.data.pop_front();
                self.dropped += 1;
                self.truncated = true;
            }
            self.data.push_back(*byte);
        }
        let keep = bytes.len().min(max);
        bytes[bytes.len().saturating_sub(keep)..].to_vec()
    }

    fn drain(&mut self) -> (String, usize, bool) {
        let bytes: Vec<u8> = self.data.drain(..).collect();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let dropped = self.dropped;
        let truncated = self.truncated;
        self.dropped = 0;
        self.truncated = false;
        (text, dropped, truncated)
    }
}
