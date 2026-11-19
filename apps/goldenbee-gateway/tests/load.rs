//! Ticket 49 measurements across Worker, Gateway, SDK, and remote fan-out
//! seams. Every product workload uses the real packaged processes; raw HTTP is
//! used only to separate transport cost from SDK cost. A local deterministic
//! endpoint replaces the external model, so these numbers describe harness
//! behavior and not provider latency or a universal SLO.

mod support;

use common::{GatewaySubmitRequest, Limits, Task, TaskState};
use load_harness::{elapsed_ns, now_ms, Distribution, Report, Sampler, WorkloadReport};
use sdk::Client as SdkClient;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::{Stack, ADMIN_TOKEN, GATEWAY_TOKEN, WORKER_A_TOKEN};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const TASKS: usize = 16;
const CONCURRENCY: usize = 4;
const DELAY_MS: u64 = 40;

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone)]
struct Outcome {
    accepted: bool,
    replayed: bool,
    state: Option<TaskState>,
    submit_ns: u64,
    end_to_end_ns: u64,
    artifacts: usize,
    terminal_event: bool,
    error: Option<String>,
}

fn terminal(state: TaskState) -> bool {
    state.is_terminal()
}

fn event_is_terminal(kind: &str) -> bool {
    matches!(
        kind,
        "task.succeeded" | "task.failed" | "task.cancelled" | "task.unknown"
    )
}

fn state_from(value: &Value, gateway: bool) -> Option<TaskState> {
    let text = if gateway {
        value.get("task")?.get("state")?.as_str()?
    } else {
        value.get("state")?.as_str()?
    };
    serde_json::from_value(Value::String(text.to_string())).ok()
}

fn task_from(value: Value, gateway: bool) -> Option<Task> {
    if gateway {
        serde_json::from_value(value.get("task")?.clone()).ok()
    } else {
        serde_json::from_value(value).ok()
    }
}

fn summarize(
    mut report: WorkloadReport,
    outcomes: &[Outcome],
    expect_artifacts: bool,
) -> WorkloadReport {
    let accepted: Vec<&Outcome> = outcomes.iter().filter(|outcome| outcome.accepted).collect();
    let terminals: Vec<&Outcome> = accepted
        .iter()
        .copied()
        .filter(|outcome| outcome.state.is_some_and(terminal))
        .collect();
    report.acceptance_latency = Distribution::from_nanos(
        &accepted
            .iter()
            .map(|outcome| outcome.submit_ns)
            .collect::<Vec<_>>(),
    );
    report.completion_latency = Distribution::from_nanos(
        &terminals
            .iter()
            .map(|outcome| outcome.end_to_end_ns)
            .collect::<Vec<_>>(),
    );
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
        "errors".into(),
        json!(outcomes
            .iter()
            .filter_map(|outcome| outcome.error.clone())
            .collect::<Vec<_>>()),
    );
    report
}

async fn raw_submit(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    body: Value,
) -> (u64, Result<Value, String>) {
    let started = Instant::now();
    let response = client
        .post(format!("{base}/v1/tasks"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await;
    let submit_ns = elapsed_ns(started);
    match response {
        Ok(response) if response.status().is_success() => match response.json::<Value>().await {
            Ok(value) => (submit_ns, Ok(value)),
            Err(error) => (submit_ns, Err(format!("response: {error}"))),
        },
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            (submit_ns, Err(format!("HTTP {status}: {body}")))
        }
        Err(error) => (submit_ns, Err(format!("transport: {error}"))),
    }
}

async fn raw_task(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    id: &str,
    gateway: bool,
) -> Option<(Task, bool)> {
    let response = client
        .get(format!("{base}/v1/tasks/{id}"))
        .bearer_auth(token)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value = response.json::<Value>().await.ok()?;
    task_from(value, gateway).map(|task| (task, true))
}

async fn raw_wait_terminal(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    id: &str,
    gateway: bool,
) -> Option<Task> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((task, _)) = raw_task(client, base, token, id, gateway).await {
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

async fn raw_artifacts(client: &reqwest::Client, base: &str, token: &str, id: &str) -> usize {
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

async fn raw_terminal_event(client: &reqwest::Client, base: &str, token: &str, id: &str) -> bool {
    let Ok(response) = client
        .get(format!("{base}/v1/tasks/{id}/events"))
        .bearer_auth(token)
        .send()
        .await
    else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    let Ok(value) = response.json::<Value>().await else {
        return false;
    };
    value["events"].as_array().is_some_and(|events| {
        events
            .iter()
            .any(|event| event["kind"].as_str().is_some_and(event_is_terminal))
    })
}

#[allow(clippy::too_many_arguments)]
async fn raw_batch(
    base: &str,
    token: &str,
    worker: Option<&str>,
    label: &str,
    count: usize,
    concurrency: usize,
    input: impl Fn(usize) -> String + Send + Sync + 'static,
    expect_artifacts: bool,
) -> Vec<Outcome> {
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
        let worker = worker.map(str::to_string);
        let label = label.to_string();
        let input = input.clone();
        set.spawn(async move {
            let _permit = permit;
            let started = Instant::now();
            let body = if let Some(worker) = &worker {
                json!({
                    "worker": worker,
                    "agent": "general",
                    "session_id": format!("gateway-{label}-{index}"),
                    "input": input(index),
                    "idempotency_key": format!("gateway-{label}-{index}"),
                })
            } else {
                json!({
                    "namespace": "local",
                    "agent": "general",
                    "session_id": format!("worker-{label}-{index}"),
                    "input": input(index),
                    "idempotency_key": format!("worker-{label}-{index}"),
                })
            };
            let (submit_ns, submitted) = raw_submit(&client, &base, &token, body).await;
            match submitted {
                Ok(value) => {
                    let id = value["task_id"].as_str().unwrap_or_default().to_string();
                    let replayed = value["replayed"].as_bool().unwrap_or(false);
                    let gateway = worker.is_some();
                    let task = raw_wait_terminal(&client, &base, &token, &id, gateway).await;
                    let state = task.as_ref().map(|task| task.state);
                    let artifacts = if expect_artifacts && state == Some(TaskState::Succeeded) {
                        raw_artifacts(&client, &base, &token, &id).await
                    } else {
                        0
                    };
                    Outcome {
                        accepted: true,
                        replayed,
                        state,
                        submit_ns,
                        end_to_end_ns: elapsed_ns(started),
                        artifacts,
                        terminal_event: raw_terminal_event(&client, &base, &token, &id).await,
                        error: task.and_then(|task| task.error.map(|error| error.code)),
                    }
                }
                Err(error) => Outcome {
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
        outcomes.push(result.expect("raw HTTP workload"));
    }
    outcomes
}

async fn sdk_batch(
    client: SdkClient,
    label: &str,
    count: usize,
    concurrency: usize,
    input: impl Fn(usize) -> String + Send + Sync + 'static,
    expect_artifacts: bool,
) -> Vec<Outcome> {
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
        let label = label.to_string();
        let input = input.clone();
        set.spawn(async move {
            let _permit = permit;
            let started = Instant::now();
            let request = GatewaySubmitRequest {
                worker: Some("worker-a".into()),
                agent: "general".into(),
                session_id: format!("sdk-{label}-{index}"),
                input: input(index),
                idempotency_key: format!("sdk-{label}-{index}"),
                profile: None,
                team: None,
                team_version: None,
                limits: Limits::default(),
            };
            let submitted = client.submit(&request).await;
            let submit_ns = elapsed_ns(started);
            match submitted {
                Ok(submitted) => {
                    let id = submitted.task_id;
                    let deadline = Instant::now() + Duration::from_secs(30);
                    let task = loop {
                        match client.task(&id).await {
                            Ok(task) if terminal(task.task.state) => break Some(task),
                            Ok(_) => {}
                            Err(error) => {
                                if Instant::now() >= deadline {
                                    break None;
                                }
                                let _ = error;
                            }
                        }
                        if Instant::now() >= deadline {
                            break None;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    };
                    let state = task.as_ref().map(|task| task.task.state);
                    let artifacts = if expect_artifacts && state == Some(TaskState::Succeeded) {
                        client
                            .artifacts(&id)
                            .await
                            .map(|values| values.len())
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    let terminal_event = client
                        .events(&id, 0, 1_000)
                        .await
                        .map(|page| {
                            page.events
                                .iter()
                                .any(|event| event_is_terminal(&event.kind))
                        })
                        .unwrap_or(false);
                    Outcome {
                        accepted: true,
                        replayed: submitted.replayed,
                        state,
                        submit_ns,
                        end_to_end_ns: elapsed_ns(started),
                        artifacts,
                        terminal_event,
                        error: task.and_then(|task| task.task.error.map(|error| error.code)),
                    }
                }
                Err(error) => Outcome {
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
        outcomes.push(result.expect("SDK workload"));
    }
    outcomes
}

fn identities(stack: &Stack, gateway: bool) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    map.insert("package".into(), "general".into());
    map.insert("worker".into(), "worker-a".into());
    map.insert("worker_instance".into(), stack.worker_a.instance.clone());
    map.insert("worker_pid".into(), stack.worker_a.pid().to_string());
    if gateway {
        map.insert("gateway".into(), stack.gateway_base.clone());
        map.insert("gateway_pid".into(), stack.gateway_pid().to_string());
    }
    map
}

fn paths(stack: &Stack) -> Vec<PathBuf> {
    vec![
        stack.gateway_data_dir(),
        stack.worker_a.data_dir().join("state.json"),
        stack.worker_b.data_dir().join("state.json"),
    ]
}

#[test]
fn worker_gateway_sdk_fixed_workloads_separate_overhead() {
    let _guard = lock();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let stack = Stack::start_with_worker_env(&[
        ("GOLDENBEE_MAX_ACTIVE", "8".into()),
        ("GOLDENBEE_MAX_QUEUE", "64".into()),
        ("GOLDENBEE_PER_PRINCIPAL", "64".into()),
    ]);

    let sampler = Sampler::start(
        vec![stack.worker_a.pid()],
        vec![stack.worker_a.data_dir().join("state.json")],
        Duration::from_millis(25),
    );
    let mut worker_outcomes = Vec::new();
    for run in 0..2 {
        let label = format!("direct-{run}");
        worker_outcomes.extend(runtime.block_on(raw_batch(
            &stack.worker_a.base,
            WORKER_A_TOKEN,
            None,
            &label,
            TASKS,
            CONCURRENCY,
            |index| format!("delay:{DELAY_MS} artifact: worker direct {index}"),
            true,
        )));
    }
    let mut worker_report = WorkloadReport::new(
        "worker-service-fixed-gateway-fixture",
        "worker-service",
        "packaged general Worker and deterministic model",
        "delay:40 artifact: worker direct N".len(),
        CONCURRENCY,
        2,
    );
    worker_report.identities = identities(&stack, false);
    worker_report.limits.insert("max_active".into(), "8".into());
    worker_report
        .notes
        .push("Direct Worker HTTP level; no Gateway hop.".into());
    let worker_report = summarize(worker_report, &worker_outcomes, true);
    let mut worker_report = worker_report;
    worker_report.limiting_resource =
        "native execution plus Worker HTTP serving; bounded by max_active=8".into();
    worker_report.process = Some(sampler.finish());
    assert_eq!(
        worker_report.completed,
        (TASKS * 2) as u64,
        "{worker_report:?}"
    );
    assert_eq!(worker_report.effect_failures, 0, "{worker_report:?}");
    assert_eq!(
        worker_report.missing_terminal_events, 0,
        "{worker_report:?}"
    );

    let sampler = Sampler::start(
        vec![stack.gateway_pid(), stack.worker_a.pid()],
        paths(&stack),
        Duration::from_millis(25),
    );
    let mut gateway_outcomes = Vec::new();
    for run in 0..2 {
        let label = format!("raw-{run}");
        gateway_outcomes.extend(runtime.block_on(raw_batch(
            &stack.gateway_base,
            GATEWAY_TOKEN,
            Some("worker-a"),
            &label,
            TASKS,
            CONCURRENCY,
            |index| format!("delay:{DELAY_MS} artifact: gateway raw {index}"),
            true,
        )));
    }
    let mut gateway_report = WorkloadReport::new(
        "gateway-route-relay-fixed",
        "gateway-route",
        "real gateway process over packaged general Worker",
        "delay:40 artifact: gateway raw N".len(),
        CONCURRENCY,
        2,
    );
    gateway_report.identities = identities(&stack, true);
    gateway_report
        .limits
        .insert("max_active".into(), "8".into());
    gateway_report
        .notes
        .push("Raw HTTP Client-to-Gateway-to-Worker path; no SDK client.".into());
    let gateway_report = summarize(gateway_report, &gateway_outcomes, true);
    let mut gateway_report = gateway_report;
    gateway_report.limiting_resource =
        "Gateway routing and relay over the same Worker max_active=8".into();
    gateway_report.process = Some(sampler.finish());
    assert_eq!(
        gateway_report.completed,
        (TASKS * 2) as u64,
        "{gateway_report:?}"
    );
    assert_eq!(gateway_report.effect_failures, 0, "{gateway_report:?}");
    assert_eq!(
        gateway_report.missing_terminal_events, 0,
        "{gateway_report:?}"
    );

    let sampler = Sampler::start(
        vec![stack.gateway_pid(), stack.worker_a.pid()],
        paths(&stack),
        Duration::from_millis(25),
    );
    let mut sdk_outcomes = Vec::new();
    for run in 0..2 {
        let label = format!("sdk-{run}");
        let sdk_client = SdkClient::new(&stack.gateway_base, GATEWAY_TOKEN);
        sdk_outcomes.extend(runtime.block_on(sdk_batch(
            sdk_client,
            &label,
            TASKS,
            CONCURRENCY,
            |index| format!("delay:{DELAY_MS} artifact: sdk journey {index}"),
            true,
        )));
    }
    let mut sdk_report = WorkloadReport::new(
        "sdk-gateway-fixed",
        "sdk-gateway",
        "real sdk::Client over the same Gateway and Worker",
        "delay:40 artifact: sdk journey N".len(),
        CONCURRENCY,
        2,
    );
    sdk_report.identities = identities(&stack, true);
    sdk_report.limits.insert("max_active".into(), "8".into());
    sdk_report.notes.push(
        "SDK path includes typed request, bounded retry wrapper, and SSE/event helpers.".into(),
    );
    let sdk_report = summarize(sdk_report, &sdk_outcomes, true);
    let mut sdk_report = sdk_report;
    sdk_report.limiting_resource =
        "SDK client plus Gateway relay over the same Worker max_active=8".into();
    sdk_report.process = Some(sampler.finish());
    assert_eq!(sdk_report.completed, (TASKS * 2) as u64, "{sdk_report:?}");
    assert_eq!(sdk_report.effect_failures, 0, "{sdk_report:?}");
    assert_eq!(sdk_report.missing_terminal_events, 0, "{sdk_report:?}");

    let mut gateway_report = gateway_report;
    let mut sdk_report = sdk_report;
    gateway_report.extra.insert(
        "sdk_minus_gateway_acceptance_p50_ms".into(),
        json!(sdk_report.acceptance_latency.p50_ms - gateway_report.acceptance_latency.p50_ms),
    );
    gateway_report.extra.insert(
        "gateway_minus_worker_completion_p50_ms".into(),
        json!(gateway_report.completion_latency.p50_ms - worker_report.completion_latency.p50_ms),
    );
    sdk_report.extra.insert(
        "sdk_minus_gateway_acceptance_p50_ms".into(),
        json!(sdk_report.acceptance_latency.p50_ms - gateway_report.acceptance_latency.p50_ms),
    );
    sdk_report.extra.insert(
        "sdk_minus_gateway_completion_p50_ms".into(),
        json!(sdk_report.completion_latency.p50_ms - gateway_report.completion_latency.p50_ms),
    );
    for report in [&mut gateway_report, &mut sdk_report] {
        report.notes.push(
            "Overhead differences are arithmetic on this host and workload, not a universal SLO."
                .into(),
        );
    }

    let failures = runtime.block_on(raw_batch(
        &stack.gateway_base,
        GATEWAY_TOKEN,
        Some("worker-a"),
        "fail",
        2,
        1,
        |index| format!("provider-fail gateway {index}"),
        false,
    ));
    let mut terminal = runtime.block_on(raw_batch(
        &stack.gateway_base,
        GATEWAY_TOKEN,
        Some("worker-a"),
        "after",
        1,
        1,
        |index| format!("artifact: gateway terminal {index}"),
        true,
    ));
    let mut outcomes = failures;
    outcomes.append(&mut terminal);
    let mut failure_report = WorkloadReport::new(
        "gateway-failure-terminal",
        "gateway-route",
        "deterministic provider failure and artifact success",
        32,
        1,
        1,
    );
    failure_report.identities = identities(&stack, true);
    failure_report.limiting_resource =
        "state transitions and durable terminal events; no saturation bound in this workload"
            .into();
    failure_report.notes.push(
        "A failed provider call stays failed; a later success carries its terminal event.".into(),
    );
    let failure_report = summarize(failure_report, &outcomes, true);
    assert_eq!(failure_report.failed, 2, "{failure_report:?}");
    assert_eq!(
        failure_report.missing_terminal_events, 0,
        "{failure_report:?}"
    );

    let report = Report::new(vec![
        worker_report,
        gateway_report,
        sdk_report,
        failure_report,
    ]);
    let path = report.persist("49-worker-gateway-sdk");
    assert!(path.is_file());
    println!("measurement timestamp {}", now_ms());
}

fn post_json(base: &str, token: &str, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
    let response = Stack::client()
        .post(format!("{base}{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .expect("POST responds");
    let status = response.status();
    let text = response.text().unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text})),
    )
}

fn get_json(base: &str, token: &str, path: &str) -> Value {
    Stack::client()
        .get(format!("{base}{path}"))
        .bearer_auth(token)
        .send()
        .expect("GET responds")
        .json()
        .expect("JSON response")
}

fn member(worker: &str, capability: &str) -> Value {
    json!({
        "worker": worker,
        "role": if worker == "worker-a" { "master" } else { "specialist" },
        "capabilities": [capability],
        "max_depth": 2
    })
}

fn remote_child(id: &str, member: &str, capability: &str, objective: String) -> Value {
    json!({
        "id": id,
        "member": member,
        "capability": capability,
        "objective": objective
    })
}

fn plan_criteria(children: &[(&str, &str)]) -> Vec<Value> {
    children
        .iter()
        .flat_map(|(id, text)| {
            [
                json!({
                    "id": format!("{id}-output"),
                    "child": id,
                    "evidence": {"kind": "output_contains", "text": text}
                }),
                json!({
                    "id": format!("{id}-artifact"),
                    "child": id,
                    "evidence": {"kind": "artifact_digest"}
                }),
            ]
        })
        .collect()
}

#[test]
fn remote_fanout_fixed_workloads_identify_child_bounds() {
    let _guard = lock();
    let stack = Stack::start_with_options(support::Options {
        with_echo_worker: true,
        worker_a_env: vec![
            ("GOLDENBEE_MAX_ACTIVE".into(), "4".into()),
            ("GOLDENBEE_PER_PRINCIPAL".into(), "64".into()),
        ],
        ..support::Options::default()
    });
    let worker_c = stack.worker_c.as_ref().expect("independent echo Worker");
    assert_eq!(stack.worker_b.status()["capacity"]["limit"], 4);

    let created = post_json(
        &stack.gateway_base,
        ADMIN_TOKEN,
        "/v1/teams",
        json!({
            "name": "load-fanout",
            "master": "worker-a",
            "members": [
                member("worker-a", "general"),
                member("worker-b", "general"),
                member("worker-c", "echo")
            ],
            "limits": {"max_depth": 2, "max_fanout": 16}
        }),
    );
    assert_eq!(created.0, reqwest::StatusCode::OK, "{}", created.1);
    let activated = post_json(
        &stack.gateway_base,
        ADMIN_TOKEN,
        "/v1/teams/load-fanout/activate",
        json!({"version": 1}),
    );
    assert_eq!(activated.0, reqwest::StatusCode::OK, "{}", activated.1);

    let sampler = Sampler::start(
        vec![
            stack.worker_a.pid(),
            stack.worker_b.pid(),
            worker_c.pid(),
            stack.gateway_pid(),
        ],
        vec![
            stack.worker_a.data_dir().join("state.json"),
            stack.worker_b.data_dir().join("state.json"),
            worker_c.data_dir().join("state.json"),
            stack.gateway_data_dir(),
        ],
        Duration::from_millis(20),
    );

    let mut dispatch_latencies = Vec::new();
    let mut outcomes = Vec::new();
    let mut max_children_active = 0u64;
    let mut rounds = 0;
    for round in 0..2 {
        let accepted = post_json(
            &stack.gateway_base,
            ADMIN_TOKEN,
            "/v1/tasks",
            json!({
                "team": "load-fanout",
                "team_version": 1,
                "agent": "general",
                "session_id": format!("fanout-session-{round}"),
                "input": format!("coordinate fanout {round}"),
                "idempotency_key": format!("fanout-parent-{round}")
            }),
        );
        assert_eq!(accepted.0, reqwest::StatusCode::OK, "{}", accepted.1);
        let execution = accepted.1["task_id"].as_str().expect("execution id");
        let projection = stack.wait_task(ADMIN_TOKEN, execution);
        assert_eq!(projection["task"]["state"], "succeeded", "{projection}");
        let master_task = projection["task"]["id"]
            .as_str()
            .expect("master task id")
            .to_string();

        let mut children = Vec::new();
        let mut criteria_children = Vec::new();
        for index in 0..6 {
            let text = format!("fanout b {round} {index}");
            children.push(remote_child(
                &format!("b-{index}"),
                "worker-b",
                "general",
                format!("delay:100 artifact: {text}"),
            ));
            criteria_children.push((format!("b-{index}"), text));
        }
        let text = format!("fanout echo {round}");
        children.push(remote_child("c-0", "worker-c", "echo", text.clone()));
        let criteria = {
            let mut values = plan_criteria(
                &criteria_children
                    .iter()
                    .map(|(id, text)| (id.as_str(), text.as_str()))
                    .collect::<Vec<_>>(),
            );
            values.push(json!({
                "id": "c-0-output",
                "child": "c-0",
                "evidence": {"kind": "output_contains", "text": text}
            }));
            values
        };
        let created = post_json(
            &stack.worker_a.base,
            &stack.worker_a.token,
            "/v1/plans",
            json!({
                "parent_task_id": master_task,
                "objective": format!("fanout round {round}"),
                "criteria": criteria,
                "children": children,
                "idempotency_key": format!("fanout-plan-{round}")
            }),
        );
        assert_eq!(created.0, reqwest::StatusCode::OK, "{}", created.1);
        let plan_id = created.1["id"].as_str().expect("plan id").to_string();

        let started = Instant::now();
        let dispatched = post_json(
            &stack.worker_a.base,
            &stack.worker_a.token,
            &format!("/v1/plans/{plan_id}/dispatch"),
            json!({"wait_ms": 15000}),
        );
        assert_eq!(dispatched.0, reqwest::StatusCode::OK, "{}", dispatched.1);
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut plan = dispatched.1;
        loop {
            let state = plan["state"].as_str().unwrap_or_default();
            if state == "verified"
                || matches!(
                    state,
                    "partial" | "conflicting" | "unverified" | "canceled" | "escalated"
                )
            {
                break;
            }
            assert!(Instant::now() < deadline, "plan did not converge: {plan}");
            let active_children = plan["children"]
                .as_array()
                .map(|children| {
                    children
                        .iter()
                        .filter(|child| {
                            !matches!(
                                child["state"].as_str(),
                                Some(
                                    "completed"
                                        | "failed"
                                        | "canceled"
                                        | "rejected"
                                        | "unreachable"
                                        | "saturated"
                                        | "unknown"
                                )
                            )
                        })
                        .count() as u64
                })
                .unwrap_or(0);
            max_children_active = max_children_active.max(active_children);
            std::thread::sleep(Duration::from_millis(10));
            plan = get_json(
                &stack.worker_a.base,
                &stack.worker_a.token,
                &format!("/v1/plans/{plan_id}"),
            );
        }
        let dispatch_ns = elapsed_ns(started);
        dispatch_latencies.push(dispatch_ns);
        assert_eq!(plan["state"], "verified", "{}", plan);
        let plan_children = plan["children"].as_array().expect("plan children");
        assert!(plan_children
            .iter()
            .all(|child| child["state"] == "completed"));
        let b_completed = plan_children
            .iter()
            .filter(|child| child["id"].as_str().is_some_and(|id| id.starts_with("b-")))
            .count();
        let b_artifacts: usize = plan_children
            .iter()
            .filter(|child| child["id"].as_str().is_some_and(|id| id.starts_with("b-")))
            .map(|child| child["artifacts"].as_array().map(Vec::len).unwrap_or(0))
            .sum();
        let c_output = plan_children
            .iter()
            .find(|child| child["id"] == "c-0")
            .and_then(|child| child["output"].as_str())
            .unwrap_or_default();
        assert!(
            c_output.contains(&format!("fanout echo {round}")),
            "{c_output}"
        );
        outcomes.push(Outcome {
            accepted: true,
            replayed: false,
            state: Some(TaskState::Succeeded),
            submit_ns: dispatch_ns,
            end_to_end_ns: dispatch_ns,
            artifacts: b_artifacts,
            terminal_event: plan_children.len() == 7,
            error: None,
        });
        assert_eq!(b_completed, 6);
        rounds += 1;
    }

    let mut report = WorkloadReport::new(
        "remote-fanout-fixed",
        "remote-fanout",
        "Master Worker plan over native A2A to worker-b and independent echo worker-c",
        "6 general children with delay:100 + 1 echo child per round".len(),
        7,
        rounds,
    );
    report
        .identities
        .insert("master".into(), stack.worker_a.instance.clone());
    report
        .identities
        .insert("child-general".into(), stack.worker_b.instance.clone());
    report
        .identities
        .insert("child-echo".into(), worker_c.instance.clone());
    report
        .identities
        .insert("gateway".into(), stack.gateway_base.clone());
    report.limits.insert("plan.children".into(), "7".into());
    report
        .limits
        .insert("worker-b.max_active".into(), "4".into());
    report.notes.push(
        "Native A2A child execution is not counted by the harness max_active semaphore; six general children and one echo child run concurrently in their child processes."
            .into(),
    );
    let report = summarize(report, &outcomes, false);
    let mut report = report;
    report.limiting_resource =
        "native A2A concurrency in worker-b and worker-c; the harness max_active=4 is not applied to native A2A child tasks"
            .into();
    report.completion_latency = Distribution::from_nanos(&dispatch_latencies);
    report
        .extra
        .insert("max_children_active".into(), json!(max_children_active));
    report.process = Some(sampler.finish());
    let path = Report::new(vec![report]).persist("49-remote-fanout");
    assert!(path.is_file());
}

#[test]
fn overload_and_gateway_restart_are_bounded_with_terminal_events() {
    let _guard = lock();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let mut stack = Stack::start_with_worker_env(&[
        ("GOLDENBEE_MAX_ACTIVE", "1".into()),
        ("GOLDENBEE_MAX_QUEUE", "2".into()),
        ("GOLDENBEE_PER_PRINCIPAL", "16".into()),
    ]);
    assert_eq!(stack.worker_a.status()["capacity"]["limit"], 1);

    let sampler = Sampler::start(
        vec![stack.gateway_pid(), stack.worker_a.pid()],
        paths(&stack),
        Duration::from_millis(20),
    );
    let mut outcomes = Vec::new();
    let mut accepted_ids: Vec<(String, Instant)> = Vec::new();
    for index in 0..6 {
        let started = Instant::now();
        let (submit_ns, submitted) = runtime.block_on(raw_submit(
            &reqwest::Client::new(),
            &stack.gateway_base,
            GATEWAY_TOKEN,
            json!({
                "worker": "worker-a",
                "agent": "general",
                "session_id": format!("overload-gateway-{index}"),
                "input": format!("delay:400 artifact: gateway overload {index}"),
                "idempotency_key": format!("gateway-overload-{index}")
            }),
        ));
        match submitted {
            Ok(value) => {
                let id = value["task_id"].as_str().expect("task id").to_string();
                accepted_ids.push((id, started));
                outcomes.push(Outcome {
                    accepted: true,
                    replayed: false,
                    state: None,
                    submit_ns,
                    end_to_end_ns: 0,
                    artifacts: 0,
                    terminal_event: false,
                    error: None,
                });
            }
            Err(error) => outcomes.push(Outcome {
                accepted: false,
                replayed: false,
                state: None,
                submit_ns: submit_ns.max(elapsed_ns(started)),
                end_to_end_ns: elapsed_ns(started),
                artifacts: 0,
                terminal_event: false,
                error: Some(error),
            }),
        }
    }
    assert_eq!(accepted_ids.len(), 3, "{outcomes:?}");
    let mut max_active = 0u64;
    let mut max_queued = 0u64;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let health = stack.worker_a.status();
        max_active = max_active.max(health["capacity"]["active"].as_u64().unwrap_or(0));
        max_queued = max_queued.max(health["capacity"]["queued"].as_u64().unwrap_or(0));
        if accepted_ids.iter().all(|(id, _)| {
            matches!(
                stack.task(GATEWAY_TOKEN, id)["task"]["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled" | "unknown")
            )
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "overload did not converge");
        std::thread::sleep(Duration::from_millis(10));
    }
    for (index, (id, started)) in accepted_ids.iter().enumerate() {
        let projection = stack.task(GATEWAY_TOKEN, id);
        outcomes[index].state = state_from(&projection, true);
        outcomes[index].end_to_end_ns = elapsed_ns(*started);
        outcomes[index].artifacts = projection["task"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("gateway overload"))
            as usize;
        outcomes[index].terminal_event = stack.events(GATEWAY_TOKEN, id)["events"]
            .as_array()
            .is_some_and(|events| {
                events
                    .iter()
                    .any(|event| event["kind"].as_str().is_some_and(event_is_terminal))
            });
    }
    let mut overload = WorkloadReport::new(
        "gateway-overload-queue",
        "gateway-route",
        "Gateway to one Worker with max_active=1 and max_queue=2",
        "delay:400 artifact: gateway overload N".len(),
        1,
        1,
    );
    overload.identities = identities(&stack, true);
    overload.limits.insert("max_active".into(), "1".into());
    overload.limits.insert("max_queue".into(), "2".into());
    overload.limiting_resource =
        "Worker max_active=1 and max_queue=2; Gateway forwards the same bound".into();
    let mut overload = summarize(overload, &outcomes, false);
    overload
        .extra
        .insert("max_active_observed".into(), json!(max_active));
    overload
        .extra
        .insert("max_queued_observed".into(), json!(max_queued));
    overload.process = Some(sampler.finish());
    assert_eq!(overload.overload_rejections, 3, "{overload:?}");
    assert_eq!(overload.completed, 3, "{overload:?}");
    assert_eq!(overload.missing_terminal_events, 0, "{overload:?}");

    let before_tasks = stack.worker_a.tasks().len();
    let recovery_started = Instant::now();
    let (recovery_submit_ns, submitted) = runtime.block_on(raw_submit(
        &reqwest::Client::new(),
        &stack.gateway_base,
        GATEWAY_TOKEN,
        json!({
            "worker": "worker-a",
            "agent": "general",
            "session_id": "gateway-restart-session",
            "input": "delay:2000 artifact: gateway restart recovery",
            "idempotency_key": "gateway-restart-recovery"
        }),
    ));
    let id = submitted.expect("recovery task accepted")["task_id"]
        .as_str()
        .expect("task id")
        .to_string();
    stack.restart_gateway();
    let sampler = Sampler::start(
        vec![stack.gateway_pid(), stack.worker_a.pid()],
        paths(&stack),
        Duration::from_millis(20),
    );
    let projection = stack.wait_task(GATEWAY_TOKEN, &id);
    assert_eq!(projection["task"]["state"], "succeeded", "{projection}");
    let events = stack.events(GATEWAY_TOKEN, &id);
    let terminal_count = events["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter(|event| event["kind"].as_str().is_some_and(event_is_terminal))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(terminal_count, 1, "{events}");
    let artifacts = get_json(
        &stack.gateway_base,
        GATEWAY_TOKEN,
        &format!("/v1/tasks/{id}/artifacts"),
    );
    let artifact_count = artifacts.as_array().map(Vec::len).unwrap_or(0);
    assert_eq!(artifact_count, 1, "{artifacts}");
    let after_tasks = stack.worker_a.tasks().len();
    assert_eq!(
        after_tasks,
        before_tasks + 1,
        "recovery created a duplicate task"
    );

    let mut recovery = WorkloadReport::new(
        "gateway-restart-recovery",
        "gateway-route",
        "Gateway restart against the same durable dispatch mapping and Worker",
        "delay:2000 artifact: gateway restart recovery".len(),
        1,
        1,
    );
    recovery.identities = identities(&stack, true);
    recovery.accepted = 1;
    recovery.completed = 1;
    recovery.effects_verified = artifact_count as u64;
    recovery.acceptance_latency = Distribution::from_nanos(&[recovery_submit_ns]);
    recovery.completion_latency = Distribution::from_nanos(&[elapsed_ns(recovery_started)]);
    recovery.terminal_events = terminal_count as u64;
    recovery.missing_terminal_events = 0;
    recovery.limiting_resource =
        "durable Gateway dispatch mapping; no task re-submission during restart".into();
    recovery.notes.push(
        "The restarted Gateway projected the original task and its durable terminal event.".into(),
    );
    recovery.extra.insert(
        "worker_task_delta".into(),
        json!(after_tasks - before_tasks),
    );
    recovery.process = Some(sampler.finish());
    let path = Report::new(vec![overload, recovery]).persist("49-overload-recovery");
    assert!(path.is_file());
}
