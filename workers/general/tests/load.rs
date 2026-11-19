//! Ticket 49 measurements at direct native execution and Worker serving seams.
//!
//! The fixed workload uses the real General Worker composition and the pinned
//! deterministic model. Each task runs the native ADK Runner through the shared
//! runtime. The Worker-service level adds the packaged HTTP process. Remote
//! fanout and Gateway/SDK levels are measured in the sibling acceptance suites.
//!
//! This is measurement tooling, not a product SLO. Every report records the
//! host, toolchain, fixture, input, concurrency, identities, process samples,
//! terminal events, and the resource that actually bounded the run.

mod fixture;

use common::{Limits, Principal, SubmitTaskRequest, Task, TaskState};
use load_harness::{
    elapsed_ns, now_ms, Distribution, ProcessSummary, Report, Sampler, WorkloadReport,
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const WORKLOAD_TASKS: usize = 16;
const WORKLOAD_CONCURRENCY: usize = 4;
const WORKLOAD_DELAY_MS: u64 = 40;
const WORKER_TOKEN: &str = "load-worker-token";

#[derive(Debug, Clone)]
struct BatchOutcome {
    accepted: bool,
    replayed: bool,
    state: Option<TaskState>,
    submit_ns: u64,
    end_to_end_ns: u64,
    artifacts: usize,
    terminal_event: bool,
    error: Option<String>,
}

fn request(session: String, input: String, key: String) -> SubmitTaskRequest {
    SubmitTaskRequest {
        namespace: Some("local".into()),
        agent: "general".into(),
        session_id: session,
        input,
        idempotency_key: key,
        profile: None,
        team: None,
        worker: None,
        limits: Limits::default(),
    }
}

fn worker_fixture(
    data: &Path,
    instance: &str,
    max_active: usize,
    max_queue: usize,
    per_principal: usize,
) -> fixture::Fixture {
    let fixture = fixture::write(
        data,
        fixture::Options {
            instance: instance.into(),
            token: Some(WORKER_TOKEN.into()),
            model: "deterministic".into(),
            principals: Vec::new(),
            settings: Vec::new(),
            token_scopes: vec!["*".into()],
        },
    );
    fixture::set_top_level(
        &fixture,
        &format!(
            "max_active = {max_active}\nmax_queue = {max_queue}\nper_principal = {per_principal}\nevent_retention = 5000\nmax_streams = 32\nstream_buffer = 256\n"
        ),
    );
    fixture
}

fn terminal(state: TaskState) -> bool {
    state.is_terminal()
}

fn terminal_kind(state: Option<TaskState>) -> &'static str {
    match state {
        Some(TaskState::Succeeded) => "succeeded",
        Some(TaskState::Failed) => "failed",
        Some(TaskState::Cancelled) => "cancelled",
        Some(TaskState::Unknown) => "unknown",
        _ => "pending",
    }
}

fn has_terminal_event(events: &[common::StreamEvent], state: Option<TaskState>) -> bool {
    let wanted = match state {
        Some(TaskState::Succeeded) => "task.succeeded",
        Some(TaskState::Failed) => "task.failed",
        Some(TaskState::Cancelled) => "task.cancelled",
        Some(TaskState::Unknown) => "task.unknown",
        _ => return false,
    };
    events.iter().any(|event| event.kind == wanted)
}

async fn wait_terminal_event(
    runtime: &runtime::Runtime,
    task_id: &str,
    state: Option<TaskState>,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let events = runtime
            .events_since(task_id, 0, 1_000)
            .await
            .map(|page| page.events)
            .unwrap_or_default();
        if has_terminal_event(&events, state) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_runtime_terminal(
    runtime: &runtime::Runtime,
    principal: &Principal,
    task_id: &str,
) -> Option<Task> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match runtime.task(principal, task_id).await {
            Ok(task) if terminal(task.state) => return Some(task),
            Ok(_) => {}
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn native_batch(
    runtime: &runtime::Runtime,
    principal: &Principal,
    label: &str,
    count: usize,
    concurrency: usize,
    input: impl Fn(usize) -> String + Send + Sync + 'static,
    expect_artifacts: bool,
) -> Vec<BatchOutcome> {
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut set = JoinSet::new();
    let input = Arc::new(input);
    for index in 0..count {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("concurrency permit");
        let runtime = runtime.clone();
        let principal = principal.clone();
        let input = input.clone();
        let label = label.to_string();
        set.spawn(async move {
            let _permit = permit;
            let started = Instant::now();
            let response = runtime
                .submit_task(
                    &principal,
                    request(
                        format!("native-{label}-{index}"),
                        input(index),
                        format!("native-{label}-{index}"),
                    ),
                )
                .await;
            let submit_ns = elapsed_ns(started);
            match response {
                Ok(response) => {
                    let task = wait_runtime_terminal(&runtime, &principal, &response.task_id).await;
                    let state = task.as_ref().map(|task| task.state);
                    let artifacts = if expect_artifacts && state == Some(TaskState::Succeeded) {
                        runtime
                            .artifacts(&principal, &response.task_id)
                            .await
                            .map(|values| values.len())
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    BatchOutcome {
                        accepted: true,
                        replayed: response.replayed,
                        state,
                        submit_ns,
                        end_to_end_ns: elapsed_ns(started),
                        artifacts,
                        terminal_event: wait_terminal_event(&runtime, &response.task_id, state)
                            .await,
                        error: task.and_then(|task| task.error.map(|error| error.code)),
                    }
                }
                Err(error) => BatchOutcome {
                    accepted: false,
                    replayed: false,
                    state: None,
                    submit_ns,
                    end_to_end_ns: elapsed_ns(started),
                    artifacts: 0,
                    terminal_event: false,
                    error: Some(error.code),
                },
            }
        });
    }
    let mut outcomes = Vec::new();
    while let Some(result) = set.join_next().await {
        outcomes.push(result.expect("native workload task"));
    }
    outcomes
}

fn summarize(
    mut report: WorkloadReport,
    outcomes: &[BatchOutcome],
    expect_artifacts: bool,
) -> WorkloadReport {
    let accepted: Vec<&BatchOutcome> = outcomes.iter().filter(|outcome| outcome.accepted).collect();
    let terminals: Vec<&BatchOutcome> = accepted
        .iter()
        .copied()
        .filter(|outcome| outcome.state.is_some_and(terminal))
        .collect();
    let acceptance: Vec<u64> = accepted.iter().map(|outcome| outcome.submit_ns).collect();
    let completion: Vec<u64> = terminals
        .iter()
        .map(|outcome| outcome.end_to_end_ns)
        .collect();
    report.acceptance_latency = Distribution::from_nanos(&acceptance);
    report.completion_latency = Distribution::from_nanos(&completion);
    report.accepted = accepted.len() as u64;
    report.completed = terminals.len() as u64;
    report.failed = terminals
        .iter()
        .filter(|outcome| outcome.state == Some(TaskState::Failed))
        .count() as u64;
    report.cancelled = terminals
        .iter()
        .filter(|outcome| outcome.state == Some(TaskState::Cancelled))
        .count() as u64;
    report.retried_or_replayed = outcomes.iter().filter(|outcome| outcome.replayed).count() as u64;
    report.overload_rejections = outcomes.iter().filter(|outcome| !outcome.accepted).count() as u64;
    report.terminal_events = terminals
        .iter()
        .filter(|outcome| outcome.terminal_event)
        .count() as u64;
    report.missing_terminal_events = terminals.len() as u64 - report.terminal_events;
    report.effects_verified = terminals
        .iter()
        .filter(|outcome| outcome.state == Some(TaskState::Succeeded))
        .map(|outcome| outcome.artifacts as u64)
        .sum();
    if expect_artifacts {
        report.effect_failures = terminals
            .iter()
            .filter(|outcome| outcome.state == Some(TaskState::Succeeded) && outcome.artifacts == 0)
            .count() as u64;
    }
    report.extra.insert(
        "succeeded".into(),
        json!(terminals
            .iter()
            .filter(|outcome| outcome.state == Some(TaskState::Succeeded))
            .count()),
    );
    report.extra.insert(
        "states".into(),
        json!(terminals
            .iter()
            .map(|outcome| terminal_kind(outcome.state))
            .collect::<Vec<_>>()),
    );
    report.extra.insert(
        "errors".into(),
        json!(outcomes
            .iter()
            .filter_map(|outcome| outcome.error.clone())
            .collect::<Vec<_>>()),
    );
    report
}

async fn native_direct_workload() -> (WorkloadReport, ProcessSummary) {
    let data = tempfile::tempdir().expect("native data");
    let fixture = worker_fixture(data.path(), "load-native", 4, 64, 64);
    let instance = general::worker()
        .expect("general composition")
        .start(&fixture.config)
        .await
        .expect("native Worker starts");
    let runtime = instance.runtime();
    let principal = instance
        .operator_principal()
        .await
        .expect("operator principal");
    let sampler = Sampler::start(
        vec![std::process::id()],
        vec![data.path().join("state.json")],
        Duration::from_millis(25),
    );
    let mut outcomes = Vec::new();
    for run in 0..2 {
        let label = format!("direct-{run}");
        outcomes.extend(
            native_batch(
                &runtime,
                &principal,
                &label,
                WORKLOAD_TASKS,
                WORKLOAD_CONCURRENCY,
                |index| format!("delay:{WORKLOAD_DELAY_MS} artifact: native direct {index}"),
                true,
            )
            .await,
        );
    }
    let mut report = WorkloadReport::new(
        "native-direct-fixed",
        "native-direct",
        "deterministic model + native ADK Runner in process",
        "delay:40 artifact: native direct N".len(),
        WORKLOAD_CONCURRENCY,
        2,
    );
    report.identities.insert("package".into(), "general".into());
    report
        .identities
        .insert("instance".into(), runtime.config().instance_id.clone());
    report.identities.insert(
        "process".into(),
        format!("in-process:{}", std::process::id()),
    );
    report.limits.insert("max_active".into(), "4".into());
    report.limits.insert("max_queue".into(), "64".into());
    report.limits.insert("per_principal".into(), "64".into());
    report
        .notes
        .push("No HTTP serving or Gateway hop is present at this level.".into());
    let mut report = summarize(report, &outcomes, true);
    report.limiting_resource =
        "native execution through the pinned ADK Runner; bounded by runtime max_active=4".into();
    let process = sampler.finish();
    report.process = Some(process.clone());
    instance.shutdown().await;
    (report, process)
}

async fn native_concurrency_sweep() -> Vec<WorkloadReport> {
    let data = tempfile::tempdir().expect("concurrency data");
    let fixture = worker_fixture(data.path(), "load-concurrency", 4, 64, 64);
    let instance = general::worker()
        .expect("general composition")
        .start(&fixture.config)
        .await
        .expect("concurrency Worker starts");
    let runtime = instance.runtime();
    let principal = instance
        .operator_principal()
        .await
        .expect("operator principal");
    let mut reports = Vec::new();
    for concurrency in [1usize, 4] {
        let label = format!("concurrency-{concurrency}");
        let outcomes = native_batch(
            &runtime,
            &principal,
            &label,
            8,
            concurrency,
            |index| format!("delay:60 artifact: concurrency {index}"),
            true,
        )
        .await;
        let mut report = WorkloadReport::new(
            format!("native-concurrency-{concurrency}"),
            "native-direct",
            "deterministic model + native ADK Runner in process",
            "delay:60 artifact: concurrency N".len(),
            concurrency,
            1,
        );
        report.identities.insert("package".into(), "general".into());
        report
            .identities
            .insert("instance".into(), runtime.config().instance_id.clone());
        report.limits.insert("max_active".into(), "4".into());
        report.limits.insert("max_queue".into(), "64".into());
        report.limiting_resource = if concurrency < 4 {
            format!("client concurrency {concurrency}; runtime max_active=4 was not reached")
        } else {
            "runtime max_active=4; four native executions may run while eight tasks are requested"
                .into()
        };
        report
            .notes
            .push("Same fixed workload and task delay at two client concurrency values.".into());
        reports.push(summarize(report, &outcomes, true));
    }
    let serial_p50 = reports[0].completion_latency.p50_ms;
    let parallel_p50 = reports[1].completion_latency.p50_ms;
    reports[1].extra.insert(
        "speedup_p50_vs_concurrency_1".into(),
        json!((serial_p50 / parallel_p50 * 100.0).round() / 100.0),
    );
    if parallel_p50 >= serial_p50 {
        reports[1].limiting_resource =
            "shared-host CPU contention dominated; runtime max_active=4 did not produce a measured p50 speedup"
                .into();
        reports[1].notes.push(
            "Shared-host contention dominated this repeat; the concurrency comparison did not show a p50 speedup."
                .into(),
        );
    }
    instance.shutdown().await;
    reports
}

async fn native_failure_cancel_workload() -> WorkloadReport {
    let data = tempfile::tempdir().expect("failure data");
    let fixture = worker_fixture(data.path(), "load-native-failure", 2, 16, 32);
    let instance = general::worker()
        .expect("general composition")
        .start(&fixture.config)
        .await
        .expect("failure Worker starts");
    let runtime = instance.runtime();
    let principal = instance
        .operator_principal()
        .await
        .expect("operator principal");

    let failures = native_batch(
        &runtime,
        &principal,
        "fail",
        2,
        1,
        |index| format!("provider-fail native {index}"),
        false,
    )
    .await;

    let mut cancelled = Vec::new();
    for index in 0..2 {
        let response = runtime
            .submit_task(
                &principal,
                request(
                    format!("native-cancel-{index}"),
                    format!("delay:5000 native cancel {index}"),
                    format!("native-cancel-{index}"),
                ),
            )
            .await
            .expect("cancel task admitted");
        let started = Instant::now();
        let _ = runtime.cancel_task(&principal, &response.task_id).await;
        let task = wait_runtime_terminal(&runtime, &principal, &response.task_id).await;
        let state = task.as_ref().map(|task| task.state);
        cancelled.push(BatchOutcome {
            accepted: true,
            replayed: false,
            state,
            submit_ns: 0,
            end_to_end_ns: elapsed_ns(started),
            artifacts: 0,
            terminal_event: wait_terminal_event(&runtime, &response.task_id, state).await,
            error: None,
        });
    }

    let replay = runtime
        .submit_task(
            &principal,
            request(
                "native-replay".into(),
                "artifact: native replay".into(),
                "native-replay-key".into(),
            ),
        )
        .await
        .expect("first replay task");
    let replay_again = runtime
        .submit_task(
            &principal,
            request(
                "native-replay".into(),
                "artifact: native replay".into(),
                "native-replay-key".into(),
            ),
        )
        .await
        .expect("duplicate replay task");
    let replay_task = wait_runtime_terminal(&runtime, &principal, &replay.task_id).await;
    let replay_state = replay_task.as_ref().map(|task| task.state);
    let replay_terminal = wait_terminal_event(&runtime, &replay.task_id, replay_state).await;

    let mut outcomes = failures;
    outcomes.extend(cancelled);
    outcomes.push(BatchOutcome {
        accepted: true,
        replayed: replay_again.replayed,
        state: replay_task.as_ref().map(|task| task.state),
        submit_ns: 0,
        end_to_end_ns: 0,
        artifacts: if replay_task
            .as_ref()
            .is_some_and(|task| task.state == TaskState::Succeeded)
        {
            runtime
                .artifacts(&principal, &replay.task_id)
                .await
                .map(|values| values.len())
                .unwrap_or(0)
        } else {
            0
        },
        terminal_event: replay_terminal,
        error: None,
    });

    let mut report = WorkloadReport::new(
        "native-failure-cancel-replay",
        "native-direct",
        "deterministic failure and cancellation sentinels",
        32,
        1,
        1,
    );
    report.identities.insert("package".into(), "general".into());
    report
        .identities
        .insert("instance".into(), runtime.config().instance_id.clone());
    report.limits.insert("max_active".into(), "2".into());
    report.limiting_resource =
        "state transitions and durable terminal events; no saturation bound in this workload"
            .into();
    report
        .notes
        .push("Failures stay failed; cancellation is terminal; duplicate keys replay the original handle.".into());
    let report = summarize(report, &outcomes, false);
    instance.shutdown().await;
    report
}

async fn overload_workload() -> WorkloadReport {
    let data = tempfile::tempdir().expect("overload data");
    let fixture = worker_fixture(data.path(), "load-overload", 1, 2, 16);
    let instance = general::worker()
        .expect("general composition")
        .start(&fixture.config)
        .await
        .expect("overload Worker starts");
    let runtime = instance.runtime();
    let principal = instance
        .operator_principal()
        .await
        .expect("operator principal");

    let mut accepted_tasks: Vec<(String, Instant)> = Vec::new();
    let mut outcomes = Vec::new();
    for index in 0..6 {
        let started = Instant::now();
        let response = runtime
            .submit_task(
                &principal,
                request(
                    format!("overload-{index}"),
                    format!("delay:500 artifact: overload {index}"),
                    format!("overload-{index}"),
                ),
            )
            .await;
        match response {
            Ok(response) => {
                accepted_tasks.push((response.task_id, started));
                outcomes.push(BatchOutcome {
                    accepted: true,
                    replayed: response.replayed,
                    state: None,
                    submit_ns: elapsed_ns(started),
                    end_to_end_ns: 0,
                    artifacts: 0,
                    terminal_event: false,
                    error: None,
                });
            }
            Err(error) => outcomes.push(BatchOutcome {
                accepted: false,
                replayed: false,
                state: None,
                submit_ns: elapsed_ns(started),
                end_to_end_ns: 0,
                artifacts: 0,
                terminal_event: false,
                error: Some(error.code),
            }),
        }
    }
    let mut max_active = 0;
    let mut max_queued = 0;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let health = runtime.health().await;
        max_active = max_active.max(health.capacity.active);
        max_queued = max_queued.max(health.capacity.queued);
        let mut all_terminal = true;
        for (task_id, _) in &accepted_tasks {
            match runtime.task(&principal, task_id).await {
                Ok(task) if terminal(task.state) => {}
                _ => {
                    all_terminal = false;
                    break;
                }
            }
        }
        if all_terminal {
            break;
        }
        assert!(Instant::now() < deadline, "overload tasks did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for (index, (task_id, started)) in accepted_tasks.iter().enumerate() {
        let task = wait_runtime_terminal(&runtime, &principal, task_id).await;
        let state = task.as_ref().map(|task| task.state);
        let terminal_event = wait_terminal_event(&runtime, task_id, state).await;
        let artifacts = if task
            .as_ref()
            .is_some_and(|task| task.state == TaskState::Succeeded)
        {
            runtime
                .artifacts(&principal, task_id)
                .await
                .map(|values| values.len())
                .unwrap_or(0)
        } else {
            0
        };
        outcomes[index].state = state;
        outcomes[index].end_to_end_ns = elapsed_ns(*started);
        outcomes[index].artifacts = artifacts;
        outcomes[index].terminal_event = terminal_event;
    }
    let mut report = WorkloadReport::new(
        "native-overload-queue",
        "native-direct",
        "deterministic delay and artifact sentinels",
        "delay:500 artifact: overload N".len(),
        1,
        1,
    );
    report.identities.insert("package".into(), "general".into());
    report
        .identities
        .insert("instance".into(), runtime.config().instance_id.clone());
    report.limits.insert("max_active".into(), "1".into());
    report.limits.insert("max_queue".into(), "2".into());
    report.limits.insert("per_principal".into(), "16".into());
    report.limiting_resource =
        "runtime max_active=1 and max_queue=2; excess admissions receive capacity.queue_full"
            .into();
    report
        .notes
        .push("One active and two queued slots are the measured saturation point.".into());
    let mut report = summarize(report, &outcomes, true);
    report
        .extra
        .insert("max_active_observed".into(), json!(max_active));
    report
        .extra
        .insert("max_queued_observed".into(), json!(max_queued));
    instance.shutdown().await;
    report
}

fn blocking_health(base: String, token: String) -> Option<Value> {
    // Reqwest's blocking client owns a private Tokio runtime. Construct and
    // drop it on a plain thread so an async test never drops that runtime from
    // an async context.
    std::thread::spawn(move || {
        reqwest::blocking::Client::new()
            .get(format!("{base}/v1/status"))
            .bearer_auth(token)
            .send()
            .ok()
            .filter(|response| response.status().is_success())
            .and_then(|response| response.json::<Value>().ok())
    })
    .join()
    .ok()
    .flatten()
}

struct Worker {
    child: Child,
    base: String,
    token: String,
    config: PathBuf,
    data: PathBuf,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Worker {
    fn spawn(config: PathBuf, data: PathBuf, token: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
            .args([
                "serve",
                "--config",
                config.to_str().expect("config path"),
                "--addr",
                "127.0.0.1:0",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("Worker process starts");
        let stdout = child.stdout.take().expect("Worker stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("Worker listener announcement");
        let base = line
            .split("listening on ")
            .nth(1)
            .unwrap_or_else(|| panic!("Worker address announcement: {line}"))
            .trim()
            .to_string();
        let worker = Self {
            child,
            base,
            token: token.into(),
            config,
            data,
        };
        worker.wait_ready();
        worker
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn base(&self) -> &str {
        &self.base
    }

    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if blocking_health(self.base.clone(), self.token.clone())
                .is_some_and(|health| health["ready"] == true)
            {
                return;
            }
            assert!(Instant::now() < deadline, "Worker did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn restart(&mut self) -> f64 {
        let started = Instant::now();
        let _ = self.child.kill();
        let _ = self.child.wait();
        *self = Worker::spawn(self.config.clone(), self.data.clone(), &self.token);
        started.elapsed().as_secs_f64() * 1000.0
    }
}

async fn http_submit(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    session: String,
    input: String,
    key: String,
) -> (u64, Result<String, String>, bool) {
    let started = Instant::now();
    let response = client
        .post(format!("{base}/v1/tasks"))
        .bearer_auth(token)
        .json(&json!({
            "namespace": "local",
            "agent": "general",
            "session_id": session,
            "input": input,
            "idempotency_key": key,
        }))
        .send()
        .await;
    let submit_ns = elapsed_ns(started);
    match response {
        Ok(response) if response.status().is_success() => match response.json::<Value>().await {
            Ok(body) => match body["task_id"].as_str() {
                Some(id) => (
                    submit_ns,
                    Ok(id.to_string()),
                    body["replayed"].as_bool().unwrap_or(false),
                ),
                None => (
                    submit_ns,
                    Err("worker response omitted task_id".into()),
                    false,
                ),
            },
            Err(error) => (submit_ns, Err(format!("worker response: {error}")), false),
        },
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            (submit_ns, Err(format!("HTTP {status}: {body}")), false)
        }
        Err(error) => (submit_ns, Err(format!("transport: {error}")), false),
    }
}

async fn http_task(client: &reqwest::Client, base: &str, token: &str, id: &str) -> Option<Task> {
    let response = client
        .get(format!("{base}/v1/tasks/{id}"))
        .bearer_auth(token)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Task>().await.ok()
}

async fn http_wait_terminal(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    id: &str,
) -> Option<Task> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(task) = http_task(client, base, token, id).await {
            if terminal(task.state) {
                return Some(task);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn http_artifacts(client: &reqwest::Client, base: &str, token: &str, id: &str) -> usize {
    let Ok(response) = client
        .get(format!("{base}/v1/tasks/{id}/artifacts"))
        .bearer_auth(token)
        .send()
        .await
    else {
        return 0;
    };
    if !response.status().is_success() {
        return 0;
    }
    match response.json::<Vec<Value>>().await {
        Ok(values) => values.len(),
        Err(_) => 0,
    }
}

async fn http_terminal_event(client: &reqwest::Client, base: &str, token: &str, id: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let found = match client
            .get(format!("{base}/v1/tasks/{id}/events"))
            .bearer_auth(token)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                match response.json::<Value>().await {
                    Ok(page) => page["events"].as_array().is_some_and(|events| {
                        events.iter().any(|event| {
                            matches!(
                                event["kind"].as_str(),
                                Some(
                                    "task.succeeded"
                                        | "task.failed"
                                        | "task.cancelled"
                                        | "task.unknown"
                                )
                            )
                        })
                    }),
                    Err(_) => false,
                }
            }
            _ => false,
        };
        if found || Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn http_batch(
    base: &str,
    token: &str,
    label: &str,
    count: usize,
    concurrency: usize,
    input: impl Fn(usize) -> String + Send + Sync + 'static,
    expect_artifacts: bool,
) -> Vec<BatchOutcome> {
    let client = reqwest::Client::new();
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let input = Arc::new(input);
    let mut set = JoinSet::new();
    for index in 0..count {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("concurrency permit");
        let client = client.clone();
        let base = base.to_string();
        let token = token.to_string();
        let input = input.clone();
        let label = label.to_string();
        set.spawn(async move {
            let _permit = permit;
            let started = Instant::now();
            let (submit_ns, submitted, replayed) = http_submit(
                &client,
                &base,
                &token,
                format!("http-{label}-{index}"),
                input(index),
                format!("http-{label}-{index}"),
            )
            .await;
            match submitted {
                Ok(id) => {
                    let task = http_wait_terminal(&client, &base, &token, &id).await;
                    let state = task.as_ref().map(|task| task.state);
                    let artifacts = if expect_artifacts && state == Some(TaskState::Succeeded) {
                        http_artifacts(&client, &base, &token, &id).await
                    } else {
                        0
                    };
                    BatchOutcome {
                        accepted: true,
                        replayed,
                        state,
                        submit_ns,
                        end_to_end_ns: elapsed_ns(started),
                        artifacts,
                        terminal_event: http_terminal_event(&client, &base, &token, &id).await,
                        error: task.and_then(|task| task.error.map(|error| error.code)),
                    }
                }
                Err(error) => BatchOutcome {
                    accepted: false,
                    replayed: false,
                    state: None,
                    submit_ns,
                    end_to_end_ns: elapsed_ns(started),
                    artifacts: 0,
                    terminal_event: false,
                    error: Some(error),
                },
            }
        });
    }
    let mut outcomes = Vec::new();
    while let Some(result) = set.join_next().await {
        outcomes.push(result.expect("HTTP workload task"));
    }
    outcomes
}

async fn worker_service_workload() -> (WorkloadReport, ProcessSummary) {
    let data = tempfile::tempdir().expect("worker data");
    let fixture = worker_fixture(data.path(), "load-worker", 4, 64, 64);
    let worker = Worker::spawn(
        fixture.config.clone(),
        data.path().to_path_buf(),
        WORKER_TOKEN,
    );
    let sampler = Sampler::start(
        vec![worker.pid()],
        vec![data.path().join("state.json")],
        Duration::from_millis(25),
    );
    let mut outcomes = Vec::new();
    for run in 0..2 {
        let label = format!("worker-{run}");
        outcomes.extend(
            http_batch(
                worker.base(),
                WORKER_TOKEN,
                &label,
                WORKLOAD_TASKS,
                WORKLOAD_CONCURRENCY,
                |index| format!("delay:{WORKLOAD_DELAY_MS} artifact: worker service {index}"),
                true,
            )
            .await,
        );
    }
    let mut report = WorkloadReport::new(
        "worker-service-fixed",
        "worker-service",
        "packaged general serve + deterministic model",
        "delay:40 artifact: worker service N".len(),
        WORKLOAD_CONCURRENCY,
        2,
    );
    report.identities.insert("package".into(), "general".into());
    report
        .identities
        .insert("instance".into(), format!("load-worker:{}", worker.base()));
    report
        .identities
        .insert("pid".into(), worker.pid().to_string());
    report.limits.insert("max_active".into(), "4".into());
    report.limits.insert("max_queue".into(), "64".into());
    report
        .notes
        .push("Adds packaged HTTP serving over the native-direct level.".into());
    let mut report = summarize(report, &outcomes, true);
    report.limiting_resource =
        "Worker HTTP service over the pinned ADK Runner; bounded by max_active=4".into();
    let process = sampler.finish();
    report.process = Some(process.clone());
    drop(worker);
    (report, process)
}

async fn worker_failure_cancel_replay() -> WorkloadReport {
    let data = tempfile::tempdir().expect("worker failure data");
    let fixture = worker_fixture(data.path(), "load-worker-failure", 2, 16, 32);
    let worker = Worker::spawn(
        fixture.config.clone(),
        data.path().to_path_buf(),
        WORKER_TOKEN,
    );
    let client = reqwest::Client::new();
    let failures = http_batch(
        worker.base(),
        WORKER_TOKEN,
        "fail",
        2,
        1,
        |index| format!("provider-fail worker {index}"),
        false,
    )
    .await;

    let mut cancellations = Vec::new();
    for index in 0..2 {
        let (_, submitted, _) = http_submit(
            &client,
            worker.base(),
            WORKER_TOKEN,
            format!("http-cancel-{index}"),
            format!("delay:5000 worker cancel {index}"),
            format!("http-cancel-{index}"),
        )
        .await;
        let Ok(id) = submitted else {
            panic!("cancel task was not accepted");
        };
        let started = Instant::now();
        let _ = client
            .post(format!("{}/v1/tasks/{id}/cancel", worker.base()))
            .bearer_auth(WORKER_TOKEN)
            .send()
            .await;
        let task = http_wait_terminal(&client, worker.base(), WORKER_TOKEN, &id).await;
        cancellations.push(BatchOutcome {
            accepted: true,
            replayed: false,
            state: task.as_ref().map(|task| task.state),
            submit_ns: 0,
            end_to_end_ns: elapsed_ns(started),
            artifacts: 0,
            terminal_event: http_terminal_event(&client, worker.base(), WORKER_TOKEN, &id).await,
            error: None,
        });
    }

    let (_, first, _) = http_submit(
        &client,
        worker.base(),
        WORKER_TOKEN,
        "http-replay".into(),
        "artifact: worker replay".into(),
        "http-replay-key".into(),
    )
    .await;
    let first = first.expect("replay first task");
    let (_, second, replayed) = http_submit(
        &client,
        worker.base(),
        WORKER_TOKEN,
        "http-replay".into(),
        "artifact: worker replay".into(),
        "http-replay-key".into(),
    )
    .await;
    assert_eq!(first, second.expect("replay second task"));
    let task = http_wait_terminal(&client, worker.base(), WORKER_TOKEN, &first).await;
    let mut outcomes = failures;
    outcomes.extend(cancellations);
    outcomes.push(BatchOutcome {
        accepted: true,
        replayed,
        state: task.as_ref().map(|task| task.state),
        submit_ns: 0,
        end_to_end_ns: 0,
        artifacts: if task
            .as_ref()
            .is_some_and(|task| task.state == TaskState::Succeeded)
        {
            http_artifacts(&client, worker.base(), WORKER_TOKEN, &first).await
        } else {
            0
        },
        terminal_event: http_terminal_event(&client, worker.base(), WORKER_TOKEN, &first).await,
        error: None,
    });
    let mut report = WorkloadReport::new(
        "worker-failure-cancel-replay",
        "worker-service",
        "deterministic failure and cancellation sentinels",
        32,
        1,
        1,
    );
    report.identities.insert("package".into(), "general".into());
    report.identities.insert(
        "instance".into(),
        format!("load-worker-failure:{}", worker.base()),
    );
    report.limits.insert("max_active".into(), "2".into());
    report.limiting_resource =
        "state transitions and durable terminal events; no saturation bound in this workload"
            .into();
    report
        .notes
        .push("HTTP acceptance is distinguished from terminal effects and terminal events.".into());
    let report = summarize(report, &outcomes, false);
    drop(worker);
    report
}

async fn churn_workload() -> WorkloadReport {
    let data = tempfile::tempdir().expect("churn data");
    let fixture = worker_fixture(data.path(), "load-churn", 2, 8, 16);
    let mut worker = Worker::spawn(
        fixture.config.clone(),
        data.path().to_path_buf(),
        WORKER_TOKEN,
    );
    let first_incarnation = blocking_health(worker.base().to_string(), WORKER_TOKEN.into())
        .expect("health")["instance"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut startups = Vec::new();
    let client = reqwest::Client::new();
    let mut outcomes = Vec::new();
    for index in 0..3 {
        let startup_ms = worker.restart();
        startups.push((startup_ms * 1_000_000.0) as u64);
        let started = Instant::now();
        let (_, submitted, _) = http_submit(
            &client,
            worker.base(),
            WORKER_TOKEN,
            format!("churn-{index}"),
            format!("artifact: churn {index}"),
            format!("churn-{index}"),
        )
        .await;
        let id = submitted.expect("churn task admitted");
        let task = http_wait_terminal(&client, worker.base(), WORKER_TOKEN, &id).await;
        let state = task.as_ref().map(|task| task.state);
        outcomes.push(BatchOutcome {
            accepted: true,
            replayed: false,
            state,
            submit_ns: 0,
            end_to_end_ns: elapsed_ns(started),
            artifacts: if state == Some(TaskState::Succeeded) {
                http_artifacts(&client, worker.base(), WORKER_TOKEN, &id).await
            } else {
                0
            },
            terminal_event: http_terminal_event(&client, worker.base(), WORKER_TOKEN, &id).await,
            error: None,
        });
    }
    let instance = blocking_health(worker.base().to_string(), WORKER_TOKEN.into()).expect("health")
        ["instance"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut report = WorkloadReport::new(
        "worker-restart-churn",
        "worker-service",
        "packaged general serve restart against one data directory",
        "artifact: churn N".len(),
        1,
        3,
    );
    report.identities.insert("package".into(), "general".into());
    report.identities.insert(
        "instance".into(),
        format!("{instance} (start={first_incarnation})"),
    );
    report.limits.insert("max_active".into(), "2".into());
    report.limiting_resource =
        "process startup and durable-state reopen; no task was left non-terminal".into();
    let mut report = summarize(report, &outcomes, true);
    report.extra.insert(
        "startup_ms".into(),
        serde_json::to_value(Distribution::from_nanos(&startups)).unwrap(),
    );
    report
        .notes
        .push("Instance identity stayed stable while process incarnation changed.".into());
    drop(worker);
    report
}

fn launcher_workload() -> WorkloadReport {
    let data = tempfile::tempdir().expect("launcher data");
    let fixture = worker_fixture(data.path(), "load-launcher", 1, 1, 1);
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_general"))
        .args([
            "dev",
            "--config",
            fixture.config.to_str().expect("config path"),
        ])
        .env("GOLDENBEE_DATA_DIR", data.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("native Launcher starts");
    child
        .stdin
        .as_mut()
        .expect("Launcher stdin")
        .write_all(b"prompt: launcher development measurement\nexit\n")
        .expect("Launcher input");
    let output = child.wait_with_output().expect("Launcher exits");
    let elapsed = elapsed_ns(started);
    let mut report = WorkloadReport::new(
        "native-launcher-dev-start",
        "native-launcher-development-mode",
        "pinned adk-runner console Launcher with deterministic model",
        "prompt: launcher development measurement".len(),
        1,
        1,
    );
    report.identities.insert("package".into(), "general".into());
    report
        .identities
        .insert("instance".into(), "load-launcher".into());
    report.limits.insert("mode".into(), "dev".into());
    report.completed = u64::from(output.status.success());
    report.acceptance_latency = Distribution::from_nanos(&[elapsed]);
    report.completion_latency = Distribution::from_nanos(&[elapsed]);
    report.limiting_resource =
        "interactive native Launcher startup and one development prompt".into();
    report.notes.push(
        "Development-mode result only. It is not evidence of product CLI, Gateway, or future web performance."
            .into(),
    );
    report
        .extra
        .insert("exit_success".into(), json!(output.status.success()));
    report
        .extra
        .insert("stderr_bytes".into(), json!(output.stderr.len()));
    report
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fixed_native_worker_and_launcher_workloads_record_actual_limits() {
    let (native, native_process) = native_direct_workload().await;
    assert!(native.process.is_some() || native_process.samples > 0);

    let failure_cancel = native_failure_cancel_workload().await;
    assert_eq!(failure_cancel.failed, 2, "{failure_cancel:?}");
    assert_eq!(failure_cancel.cancelled, 2, "{failure_cancel:?}");
    assert_eq!(
        failure_cancel.missing_terminal_events, 0,
        "{failure_cancel:?}"
    );
    assert_eq!(failure_cancel.retried_or_replayed, 1, "{failure_cancel:?}");

    let overload = overload_workload().await;
    assert_eq!(overload.overload_rejections, 3, "{overload:?}");
    assert!(overload.extra["max_active_observed"].as_u64().unwrap_or(0) <= 1);

    let (worker, worker_process) = worker_service_workload().await;
    assert_eq!(worker.completed, (WORKLOAD_TASKS * 2) as u64);
    assert_eq!(worker.effect_failures, 0, "{worker:?}");
    assert_eq!(worker.missing_terminal_events, 0, "{worker:?}");
    assert!(worker_process.samples > 0);

    let worker_failure = worker_failure_cancel_replay().await;
    assert_eq!(worker_failure.failed, 2, "{worker_failure:?}");
    assert_eq!(worker_failure.cancelled, 2, "{worker_failure:?}");
    assert_eq!(worker_failure.retried_or_replayed, 1, "{worker_failure:?}");
    assert_eq!(
        worker_failure.missing_terminal_events, 0,
        "{worker_failure:?}"
    );

    let mut sweep = native_concurrency_sweep().await;
    assert_eq!(sweep.len(), 2);
    for report in &sweep {
        assert_eq!(report.completed, 8, "{report:?}");
        assert_eq!(report.effect_failures, 0, "{report:?}");
    }

    let churn = churn_workload().await;
    assert_eq!(churn.completed, 3, "{churn:?}");
    assert_eq!(churn.effect_failures, 0, "{churn:?}");
    assert_eq!(churn.missing_terminal_events, 0, "{churn:?}");

    let launcher = launcher_workload();
    assert_eq!(launcher.completed, 1, "{launcher:?}");

    let mut workloads = vec![native, failure_cancel, overload, worker, worker_failure];
    workloads.append(&mut sweep);
    workloads.extend([churn, launcher]);
    let report = Report::new(workloads);
    let path = report.persist("49-general-native-worker");
    assert!(path.is_file());
    println!("measurement timestamp {}", now_ms());
}
