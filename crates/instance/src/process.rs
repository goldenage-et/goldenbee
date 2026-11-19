//! Process start, readiness gating, and scoped stop.
//!
//! The tool starts only the recorded verified executable with the recorded
//! configuration. Readiness is written to `state.json` only after the Worker's
//! own liveness endpoint answers. Stop sends SIGTERM only to a pid whose command
//! line still names this instance's configuration.

use crate::layout::{
    instance_paths, load_record, load_state, save_state, token_for, InstanceError, InstanceRecord,
    InstanceState,
};
use crate::trust::enforce;
use serde_json::Value;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// Successful start result.
#[derive(Debug, Clone)]
pub struct StartOutcome {
    /// Persistent record.
    pub record: InstanceRecord,
    /// Persisted lifecycle state.
    pub state: InstanceState,
    /// Listener address.
    pub addr: String,
}

/// Successful stop result.
#[derive(Debug, Clone)]
pub struct StopOutcome {
    /// Persistent record.
    pub record: InstanceRecord,
    /// Persisted lifecycle state.
    pub state: InstanceState,
}

/// Start one instance and wait for its real readiness gate.
pub fn start(root: &Path, id: &str, addr: Option<&str>) -> Result<StartOutcome, InstanceError> {
    start_with_env(root, id, addr, &[])
}

/// Start one instance with additional process environment entries.
///
/// The host supplies only narrow control environment such as the shared
/// delegation secret or provider credentials by reference; the instance's own
/// configuration still selects its agents, assets, storage, and grants.
pub fn start_with_env(
    root: &Path,
    id: &str,
    addr: Option<&str>,
    environment: &[(String, String)],
) -> Result<StartOutcome, InstanceError> {
    let paths = instance_paths(root, id)?;
    let record = load_record(&paths)?;
    let mut state = load_state(&paths)?;
    enforce(&state)?;
    if let Some(pid) = state.pid {
        if !process_matches(pid, &record.config_path) {
            state.last_error =
                Some("instance.pid_reused: the recorded pid no longer names this instance".into());
        }
        if process_matches(pid, &record.config_path) {
            return Err(InstanceError::new(
                "instance.already_running",
                format!("instance '{id}' is already running as pid {pid}"),
            ));
        }
    }
    if !record.binary.is_file() {
        return Err(InstanceError::new(
            "instance.release_invalid",
            "recorded executable is missing from the verified release",
        ));
    }
    if !record.config_path.is_file() {
        return Err(InstanceError::new(
            "instance.config_invalid",
            "recorded configuration file is missing",
        ));
    }
    std::fs::create_dir_all(&record.data_dir).map_err(storage)?;
    std::fs::create_dir_all(&record.workspace_root).map_err(storage)?;
    let temp = paths.dir.join("tmp");
    std::fs::create_dir_all(&temp).map_err(storage)?;
    let addr = match addr {
        Some(addr) => addr.to_string(),
        None => free_addr()?,
    };
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log)
        .map_err(storage)?;
    let stderr = log_file.try_clone().map_err(storage)?;
    let mut child = Command::new(&record.binary)
        .arg("serve")
        .arg("--config")
        .arg(&record.config_path)
        .arg("--addr")
        .arg(&addr)
        .current_dir(&paths.dir)
        .env_clear()
        .env("HOME", &paths.dir)
        .env("TMPDIR", &temp)
        .env("GOLDENBEE_INSTANCE_ID", id)
        .env("GOLDENBEE_DATA_DIR", &record.data_dir)
        .env("GOLDENBEE_SANDBOX_DIR", &record.workspace_root)
        .envs(
            environment
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| {
            InstanceError::new(
                "instance.startup_failed",
                format!("recorded executable could not start: {error}"),
            )
        })?;

    let token = token_for(&paths, &record);
    let base = format!("http://{addr}");
    let deadline = Instant::now() + START_TIMEOUT;
    let mut failure = None;
    let mut incarnation = None;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            failure = Some(format!("process exited with {status} before readiness"));
            break;
        }
        match get_json(&format!("{base}/v1/health"), None) {
            Ok(health) => {
                incarnation = health
                    .get("incarnation")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(token) = &token {
                    match get_json(&format!("{base}/v1/status"), Some(token)) {
                        Ok(status)
                            if status.get("phase").and_then(Value::as_str) == Some("ready") =>
                        {
                            break;
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                } else {
                    break;
                }
            }
            Err(_) => std::thread::sleep(POLL),
        }
    }
    if Instant::now() >= deadline {
        failure = Some("readiness was not reported within the startup deadline".into());
    }
    if let Some(reason) = failure {
        let _ = child.kill();
        let _ = child.wait();
        state.lifecycle = "faulted".into();
        state.pid = None;
        state.addr = None;
        state.incarnation = None;
        state.last_error = Some(format!("instance.startup_failed: {reason}"));
        save_state(&paths, &state)?;
        return Err(InstanceError::new(
            "instance.startup_failed",
            format!("instance '{id}' did not become ready: {reason}"),
        ));
    }

    state.lifecycle = "running".into();
    state.pid = Some(child.id());
    state.incarnation = incarnation;
    state.addr = Some(addr.clone());
    state.started_at = Some(now_ms());
    state.stopped_at = None;
    state.last_error = None;
    save_state(&paths, &state)?;
    Ok(StartOutcome {
        record,
        state,
        addr,
    })
}

/// Stop one instance by the recorded pid only.
pub fn stop(root: &Path, id: &str) -> Result<StopOutcome, InstanceError> {
    let paths = instance_paths(root, id)?;
    let record = load_record(&paths)?;
    let mut state = load_state(&paths)?;
    if let Some(pid) = state.pid {
        if !process_matches(pid, &record.config_path) {
            state.last_error =
                Some("instance.pid_reused: the recorded pid no longer names this instance".into());
        }
        if process_matches(pid, &record.config_path) {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let deadline = Instant::now() + STOP_TIMEOUT;
            while process_matches(pid, &record.config_path) {
                if Instant::now() >= deadline {
                    state.last_error = Some("instance.stop_timeout: process did not exit".into());
                    save_state(&paths, &state)?;
                    return Err(InstanceError::new(
                        "instance.stop_timeout",
                        format!("instance '{id}' did not stop within the drain deadline"),
                    ));
                }
                std::thread::sleep(POLL);
            }
        }
    }
    state.lifecycle = "stopped".into();
    state.pid = None;
    state.addr = None;
    state.stopped_at = Some(now_ms());
    save_state(&paths, &state)?;
    Ok(StopOutcome { record, state })
}

/// True only while `pid` is live and its command line names this config.
pub(crate) fn process_matches(pid: u32, config: &Path) -> bool {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
    if let Some(stat) = &stat {
        if let Some(state) = stat
            .rsplit(')')
            .next()
            .and_then(|rest| rest.split_whitespace().next())
        {
            if state == "Z" || state == "X" {
                return false;
            }
        }
    } else {
        return false;
    }
    let Ok(command) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let expected = config.as_os_str().as_encoded_bytes();
    command
        .split(|byte| *byte == 0)
        .any(|argument| argument == expected)
}

/// Fetch one small JSON document with a bounded timeout.
pub(crate) fn get_json(url: &str, token: Option<&str>) -> Result<Value, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(900))
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client.get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    response.json().map_err(|error| error.to_string())
}

fn free_addr() -> Result<String, InstanceError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(storage)?;
    let addr = listener.local_addr().map_err(storage)?;
    drop(listener);
    Ok(addr.to_string())
}

fn storage(error: std::io::Error) -> InstanceError {
    InstanceError::new(
        "instance.storage_unavailable",
        format!("instance process operation failed: {error}"),
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
