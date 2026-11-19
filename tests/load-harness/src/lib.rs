//! Measurement primitives for ticket 49.
//!
//! This crate is test tooling only. It records environment provenance, latency
//! distributions, and bounded `/proc` samples so separate acceptance suites can
//! publish raw measurements without adding a product telemetry or SLO surface.

use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Current wall-clock milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Elapsed nanoseconds since `started`.
pub fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos() as u64
}

/// Workspace root inferred from this crate's source location.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

/// Directory where raw JSON reports are written.
///
/// `LOAD_RESULTS_DIR` may name an absolute path or a path relative to the
/// workspace root. The default keeps generated evidence under `target/` so it
/// is not committed accidentally.
pub fn results_dir() -> PathBuf {
    match std::env::var("LOAD_RESULTS_DIR") {
        Ok(value) if !value.trim().is_empty() => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                workspace_root().join(path)
            }
        }
        _ => workspace_root().join("target/load-evidence"),
    }
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unavailable".into())
}

fn first_cpu_model() -> String {
    let text = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    text.lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.trim() == "model name")
        })
        .map(|(_, value)| value.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn memory_kb(key: &str) -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    text.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name != key {
            return None;
        }
        rest.split_whitespace().next()?.parse().ok()
    })
}

/// Static and runtime environment recorded with every report.
#[derive(Debug, Clone, Serialize)]
pub struct Environment {
    /// Capture time in milliseconds since the Unix epoch.
    pub captured_at_ms: u64,
    /// UTC capture time.
    pub captured_at: String,
    /// Operating system family.
    pub os: String,
    /// CPU architecture.
    pub arch: String,
    /// Kernel identity.
    pub kernel: String,
    /// Reported CPU model.
    pub cpu_model: String,
    /// Logical CPU count.
    pub logical_cpus: usize,
    /// Total host memory in KiB.
    pub memory_total_kb: Option<u64>,
    /// Available host memory at capture time in KiB.
    pub memory_available_kb: Option<u64>,
    /// Compiler version used to build this harness.
    pub rustc: String,
    /// Cargo version.
    pub cargo: String,
    /// Workspace root used for the run.
    pub workspace: String,
    /// Git commit checked out when the measurement ran.
    pub git_head: String,
    /// Uncommitted status summary, empty when the tree was clean.
    pub git_status: String,
    /// Debug or release profile reported by Cargo's build script state.
    pub profile: String,
    /// Protocol pinned by the workspace.
    pub protocol: String,
    /// Native ADK version pinned by the workspace.
    pub adk: String,
    /// Workspace manifest feature/limit posture relevant to this run.
    pub features: Vec<String>,
    /// Resolution assumption for `/proc` CPU accounting.
    pub proc_clock_ticks_per_second: u64,
}

impl Environment {
    /// Capture the current host and workspace provenance.
    pub fn capture() -> Self {
        let root = workspace_root();
        let status = command_output(
            "git",
            &[
                "-C",
                root.to_string_lossy().as_ref(),
                "status",
                "--porcelain",
            ],
        );
        let captured_at_ms = now_ms();
        Self {
            captured_at_ms,
            captured_at: chrono::DateTime::from_timestamp_millis(captured_at_ms as i64)
                .map(|value| value.to_rfc3339())
                .unwrap_or_else(|| "unknown".into()),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            kernel: command_output("uname", &["-srm"]),
            cpu_model: first_cpu_model(),
            logical_cpus: thread::available_parallelism()
                .map(|value| value.get())
                .unwrap_or(1),
            memory_total_kb: memory_kb("MemTotal"),
            memory_available_kb: memory_kb("MemAvailable"),
            rustc: command_output("rustc", &["--version"]),
            cargo: command_output("cargo", &["--version"]),
            workspace: root.display().to_string(),
            git_head: command_output(
                "git",
                &["-C", root.to_string_lossy().as_ref(), "rev-parse", "HEAD"],
            ),
            git_status: status,
            profile: if cfg!(debug_assertions) {
                "debug/test".into()
            } else {
                "release".into()
            },
            protocol: common::PROTOCOL_VERSION.into(),
            adk: "adk-rust 2.3.0 (pinned)".into(),
            features: vec![
                "workspace default test profile".into(),
                "runtime graph feature not enabled unless the measured package enables it".into(),
                "adk-rust facade features: agents, runner, sessions, server, plugin".into(),
            ],
            proc_clock_ticks_per_second: 100,
        }
    }
}

/// A latency distribution in milliseconds.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Distribution {
    /// Sample count.
    pub count: usize,
    /// Smallest sample.
    pub min_ms: f64,
    /// Arithmetic mean.
    pub mean_ms: f64,
    /// Median.
    pub p50_ms: f64,
    /// 90th percentile.
    pub p90_ms: f64,
    /// 95th percentile.
    pub p95_ms: f64,
    /// 99th percentile.
    pub p99_ms: f64,
    /// Largest sample.
    pub max_ms: f64,
    /// Sum of all samples.
    pub total_ms: f64,
}

impl Distribution {
    /// Build a distribution from nanosecond durations.
    pub fn from_nanos(samples: &[u64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut values: Vec<f64> = samples
            .iter()
            .map(|value| *value as f64 / 1_000_000.0)
            .collect();
        values.sort_by(|left, right| left.total_cmp(right));
        let total: f64 = values.iter().sum();
        let percentile = |percent: f64| {
            let rank = ((percent / 100.0) * values.len() as f64).ceil() as usize;
            values[rank.saturating_sub(1).min(values.len() - 1)]
        };
        Self {
            count: values.len(),
            min_ms: round(values[0]),
            mean_ms: round(total / values.len() as f64),
            p50_ms: round(percentile(50.0)),
            p90_ms: round(percentile(90.0)),
            p95_ms: round(percentile(95.0)),
            p99_ms: round(percentile(99.0)),
            max_ms: round(values[values.len() - 1]),
            total_ms: round(total),
        }
    }
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[derive(Debug, Clone, Default)]
struct ProcSample {
    at_ms: u64,
    rss_kb: u64,
    threads: u64,
    fds: u64,
    sockets: u64,
    subprocesses: u64,
    cpu_ticks: u64,
    read_bytes: u64,
    write_bytes: u64,
    state_bytes: u64,
}

fn sample_proc(pid: u32, paths: &[PathBuf]) -> Option<ProcSample> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    let cpu_ticks = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kb = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let threads = status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(1);
    let mut fds = 0u64;
    let mut sockets = 0u64;
    if let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) {
        for entry in entries.flatten() {
            fds += 1;
            if entry
                .path()
                .read_link()
                .ok()
                .and_then(|target| target.to_str().map(str::to_string))
                .is_some_and(|target| target.starts_with("socket:"))
            {
                sockets += 1;
            }
        }
    }
    let mut subprocesses = 0u64;
    if let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) {
        for task in tasks.flatten() {
            if let Ok(children) = fs::read_to_string(task.path().join("children")) {
                subprocesses += children.split_whitespace().count() as u64;
            }
        }
    }
    let io = fs::read_to_string(format!("/proc/{pid}/io")).unwrap_or_default();
    let field = |name: &str| {
        io.lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(0)
    };
    let state_bytes = paths
        .iter()
        .filter_map(|path| fs::metadata(path).ok().map(|metadata| metadata.len()))
        .sum();
    Some(ProcSample {
        at_ms: now_ms(),
        rss_kb,
        threads,
        fds,
        sockets,
        subprocesses,
        cpu_ticks,
        read_bytes: field("read_bytes:"),
        write_bytes: field("write_bytes:"),
        state_bytes,
    })
}

/// Bounded process and state-file sampling summary.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProcessSummary {
    /// Sampled process identities.
    pub pids: Vec<u32>,
    /// Watched state files or directories.
    pub watched_paths: Vec<String>,
    /// Number of samples.
    pub samples: usize,
    /// Sampling interval.
    pub interval_ms: u64,
    /// Peak resident set in KiB.
    pub max_rss_kb: u64,
    /// Mean resident set in KiB.
    pub mean_rss_kb: u64,
    /// Peak thread count.
    pub max_threads: u64,
    /// Peak file descriptor count.
    pub max_fds: u64,
    /// Peak socket descriptor count.
    pub max_sockets: u64,
    /// Peak direct child process count.
    pub max_subprocesses: u64,
    /// Peak aggregate CPU percentage (may exceed 100 for multiple threads).
    pub max_cpu_percent: f64,
    /// Mean aggregate CPU percentage.
    pub mean_cpu_percent: f64,
    /// Bytes read from block devices during the sample window.
    pub read_bytes_delta: u64,
    /// Bytes written to block devices during the sample window.
    pub write_bytes_delta: u64,
    /// Peak total size of watched state paths in bytes.
    pub state_bytes_max: u64,
}

/// Background `/proc` sampler.
pub struct Sampler {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    samples: Arc<Mutex<Vec<ProcSample>>>,
    pids: Vec<u32>,
    paths: Vec<PathBuf>,
    interval: Duration,
}

impl Sampler {
    /// Start sampling the supplied processes and state paths.
    pub fn start(pids: Vec<u32>, paths: Vec<PathBuf>, interval: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = stop.clone();
        let worker_samples = samples.clone();
        let worker_pids = pids.clone();
        let worker_paths = paths.clone();
        let handle = thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                let mut aggregate = ProcSample {
                    at_ms: now_ms(),
                    ..ProcSample::default()
                };
                for pid in &worker_pids {
                    if let Some(sample) = sample_proc(*pid, &worker_paths) {
                        aggregate.rss_kb += sample.rss_kb;
                        aggregate.threads += sample.threads;
                        aggregate.fds += sample.fds;
                        aggregate.sockets += sample.sockets;
                        aggregate.subprocesses += sample.subprocesses;
                        aggregate.cpu_ticks += sample.cpu_ticks;
                        aggregate.read_bytes += sample.read_bytes;
                        aggregate.write_bytes += sample.write_bytes;
                        aggregate.state_bytes = aggregate.state_bytes.max(sample.state_bytes);
                    }
                }
                if aggregate.state_bytes == 0 {
                    aggregate.state_bytes = worker_paths
                        .iter()
                        .filter_map(|path| fs::metadata(path).ok().map(|metadata| metadata.len()))
                        .sum();
                }
                worker_samples
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(aggregate);
                thread::sleep(interval);
            }
        });
        Self {
            stop,
            handle: Some(handle),
            samples,
            pids,
            paths,
            interval,
        }
    }

    /// Stop sampling and summarize the bounded observations.
    pub fn finish(mut self) -> ProcessSummary {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let samples = self
            .samples
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if samples.is_empty() {
            return ProcessSummary {
                pids: self.pids,
                watched_paths: self
                    .paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
                interval_ms: self.interval.as_millis() as u64,
                ..ProcessSummary::default()
            };
        }
        let mut cpu_percentages = Vec::new();
        for pair in samples.windows(2) {
            let ticks = pair[1].cpu_ticks.saturating_sub(pair[0].cpu_ticks);
            let elapsed = pair[1].at_ms.saturating_sub(pair[0].at_ms).max(1) as f64 / 1000.0;
            cpu_percentages.push(ticks as f64 / 100.0 / elapsed * 100.0);
        }
        let mean_cpu = if cpu_percentages.is_empty() {
            0.0
        } else {
            cpu_percentages.iter().sum::<f64>() / cpu_percentages.len() as f64
        };
        let first = samples.first().expect("sample exists");
        let last = samples.last().expect("sample exists");
        ProcessSummary {
            pids: self.pids,
            watched_paths: self
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            samples: samples.len(),
            interval_ms: self.interval.as_millis() as u64,
            max_rss_kb: samples
                .iter()
                .map(|sample| sample.rss_kb)
                .max()
                .unwrap_or(0),
            mean_rss_kb: samples.iter().map(|sample| sample.rss_kb).sum::<u64>()
                / samples.len() as u64,
            max_threads: samples
                .iter()
                .map(|sample| sample.threads)
                .max()
                .unwrap_or(0),
            max_fds: samples.iter().map(|sample| sample.fds).max().unwrap_or(0),
            max_sockets: samples
                .iter()
                .map(|sample| sample.sockets)
                .max()
                .unwrap_or(0),
            max_subprocesses: samples
                .iter()
                .map(|sample| sample.subprocesses)
                .max()
                .unwrap_or(0),
            max_cpu_percent: round(cpu_percentages.iter().copied().fold(0.0_f64, f64::max)),
            mean_cpu_percent: round(mean_cpu),
            read_bytes_delta: last.read_bytes.saturating_sub(first.read_bytes),
            write_bytes_delta: last.write_bytes.saturating_sub(first.write_bytes),
            state_bytes_max: samples
                .iter()
                .map(|sample| sample.state_bytes)
                .max()
                .unwrap_or(0),
        }
    }
}

/// One measured workload at one architectural seam.
#[derive(Debug, Clone, Serialize)]
pub struct WorkloadReport {
    /// Stable workload name.
    pub name: String,
    /// Seam being measured, for example `native-direct` or `sdk-gateway`.
    pub level: String,
    /// Model or deterministic fixture used.
    pub fixture: String,
    /// Input bytes per invocation.
    pub input_bytes: usize,
    /// Requested client concurrency.
    pub concurrency: usize,
    /// Independent repeated runs.
    pub runs: usize,
    /// Package, instance, and process identities used by this workload.
    pub identities: BTreeMap<String, String>,
    /// Effective bounds and limits.
    pub limits: BTreeMap<String, String>,
    /// Submission/acceptance latency.
    pub acceptance_latency: Distribution,
    /// Submission-to-terminal completion latency.
    pub completion_latency: Distribution,
    /// Durable-event receive lag where measured.
    pub event_lag: Distribution,
    /// Accepted invocations.
    pub accepted: u64,
    /// Completed invocations observed terminal.
    pub completed: u64,
    /// Failed invocations.
    pub failed: u64,
    /// Cancelled invocations.
    pub cancelled: u64,
    /// Idempotent replays or safe retries observed.
    pub retried_or_replayed: u64,
    /// Effects verified independently of task state.
    pub effects_verified: u64,
    /// Expected effects that were missing or conflicting.
    pub effect_failures: u64,
    /// Backpressure or overload rejections.
    pub overload_rejections: u64,
    /// Durable terminal events observed by a subscriber or event page.
    pub terminal_events: u64,
    /// Terminal events that should have existed but were not observed.
    pub missing_terminal_events: u64,
    /// Resource that actually bounded the workload.
    pub limiting_resource: String,
    /// Process, socket, memory, and state-file samples.
    pub process: Option<ProcessSummary>,
    /// Interpretation bounds and other raw values.
    pub notes: Vec<String>,
    /// Additional workload-specific observations.
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl WorkloadReport {
    /// Start a report with the measured input and concurrency.
    pub fn new(
        name: impl Into<String>,
        level: impl Into<String>,
        fixture: impl Into<String>,
        input_bytes: usize,
        concurrency: usize,
        runs: usize,
    ) -> Self {
        Self {
            name: name.into(),
            level: level.into(),
            fixture: fixture.into(),
            input_bytes,
            concurrency,
            runs,
            identities: BTreeMap::new(),
            limits: BTreeMap::new(),
            acceptance_latency: Distribution::default(),
            completion_latency: Distribution::default(),
            event_lag: Distribution::default(),
            accepted: 0,
            completed: 0,
            failed: 0,
            cancelled: 0,
            retried_or_replayed: 0,
            effects_verified: 0,
            effect_failures: 0,
            overload_rejections: 0,
            terminal_events: 0,
            missing_terminal_events: 0,
            limiting_resource: String::new(),
            process: None,
            notes: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    /// Insert a string observation.
    pub fn note(&mut self, value: impl Into<String>) {
        self.notes.push(value.into());
    }
}

/// Complete report persisted as raw JSON.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Report schema version.
    pub schema: u32,
    /// Environment provenance.
    pub environment: Environment,
    /// Measured workloads.
    pub workloads: Vec<WorkloadReport>,
}

impl Report {
    /// Capture the environment and bundle measured workloads.
    pub fn new(workloads: Vec<WorkloadReport>) -> Self {
        Self {
            schema: 1,
            environment: Environment::capture(),
            workloads,
        }
    }

    /// Persist pretty JSON under the results directory and return its path.
    pub fn write(&self, slug: &str) -> io::Result<PathBuf> {
        let directory = results_dir();
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{slug}.json"));
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(&path, bytes)?;
        Ok(path)
    }

    /// Persist the report, print a short provenance line, and return the path.
    pub fn persist(&self, slug: &str) -> PathBuf {
        let path = self.write(slug).expect("load report is written");
        println!(
            "LOAD_REPORT {} schema={} workloads={} git={} host={} cpus={}",
            path.display(),
            self.schema,
            self.workloads.len(),
            self.environment.git_head,
            self.environment.os,
            self.environment.logical_cpus
        );
        path
    }
}
