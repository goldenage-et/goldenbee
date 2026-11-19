//! Ticket 45: Gateway projections honor the owning Worker's deletion outcome.
//!
//! These are public protocol tests. They use the same caller-facing routes as a
//! Client and never inspect Gateway storage internals.

mod support;

use sdk::ErrorKind;
use support::{Options, Stack, GATEWAY_TOKEN, WORKER_A_TOKEN};

#[test]
fn erased_worker_content_is_absent_from_projections_before_and_after_reconnect() {
    let mut stack = Stack::start_with_options(Options::default());
    let submitted = stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "retention",
        "s45",
        "s45",
    );
    assert!(submitted.status().is_success());
    let body: serde_json::Value = submitted.json().expect("submit response");
    let id = body["task_id"].as_str().expect("task id").to_string();
    let projection = stack.wait_task(GATEWAY_TOKEN, &id);
    let caller = projection["task"]["caller"]
        .as_str()
        .expect("caller identity")
        .to_string();
    assert_eq!(projection["task"]["state"], "succeeded");

    // Authorized local maintenance erases the owning principal at the Worker.
    let erased = support::Stack::client()
        .post(format!(
            "{}/v1/principals/{caller}/erase",
            stack.worker_a.base
        ))
        .bearer_auth(WORKER_A_TOKEN)
        .send()
        .expect("worker erasure responds");
    assert!(
        erased.status().is_success(),
        "worker erasure failed: {}",
        erased.status()
    );
    let report: serde_json::Value = erased.json().expect("erasure report");
    assert_eq!(report["completed"], true, "{report}");
    assert!(
        report["removed"]["task_metadata"].as_u64().unwrap_or(0) >= 1,
        "{report}"
    );

    // The Gateway must not serve the previously cached projection.
    let task = support::Stack::client()
        .get(format!("{}/v1/tasks/{id}", stack.gateway_base))
        .bearer_auth(GATEWAY_TOKEN)
        .send()
        .expect("gateway task responds");
    assert_eq!(task.status(), reqwest::StatusCode::NOT_FOUND);
    let error: serde_json::Value = task.json().expect("typed error");
    assert_eq!(error["code"], "task.not_found");

    let events = support::Stack::client()
        .get(format!("{}/v1/tasks/{id}/events", stack.gateway_base))
        .bearer_auth(GATEWAY_TOKEN)
        .send()
        .expect("gateway events respond");
    assert_eq!(events.status(), reqwest::StatusCode::NOT_FOUND);

    // Restarting the Gateway does not resurrect the cleared projection.
    stack.restart_gateway();
    let after_restart = support::Stack::client()
        .get(format!("{}/v1/tasks/{id}", stack.gateway_base))
        .bearer_auth(GATEWAY_TOKEN)
        .send()
        .expect("gateway task responds after restart");
    assert_eq!(after_restart.status(), reqwest::StatusCode::NOT_FOUND);
}

#[test]
fn sdk_observes_the_owners_not_found_outcome_instead_of_a_cached_projection() {
    let stack = Stack::start_with_options(Options::default());
    let submitted = stack.submit(
        GATEWAY_TOKEN,
        "worker-a",
        "general",
        "sdk",
        "s45-sdk",
        "s45-sdk",
    );
    assert!(submitted.status().is_success());
    let id: serde_json::Value = submitted.json().expect("submit response");
    let id = id["task_id"].as_str().expect("task id").to_string();
    let projection = stack.wait_task(GATEWAY_TOKEN, &id);
    let caller = projection["task"]["caller"]
        .as_str()
        .expect("caller identity")
        .to_string();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let client = sdk::Client::new(&stack.gateway_base, GATEWAY_TOKEN);
    let before = runtime
        .block_on(client.task(&id))
        .expect("projection is readable before erasure");
    assert_eq!(before.task_id, id);

    let erased = support::Stack::client()
        .post(format!(
            "{}/v1/principals/{caller}/erase",
            stack.worker_a.base
        ))
        .bearer_auth(WORKER_A_TOKEN)
        .send()
        .expect("worker erasure responds");
    assert!(erased.status().is_success());

    let error = runtime
        .block_on(client.task(&id))
        .expect_err("the SDK must observe deletion, not a cached task");
    assert_eq!(error.code, "task.not_found");
    assert_eq!(sdk::classify(&error), ErrorKind::NotFound);

    let watch_error = match runtime.block_on(client.watch(&id, 0)) {
        Ok(_) => panic!("reconnect after erasure must fail"),
        Err(error) => error,
    };
    assert_eq!(watch_error.code, "task.not_found");
}
