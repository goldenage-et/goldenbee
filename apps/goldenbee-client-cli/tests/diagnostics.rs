//! Public CLI diagnostic export through the SDK and Gateway.

#[path = "../../goldenbee-gateway/tests/support/mod.rs"]
mod support;

use serde_json::Value;
use support::{Stack, ADMIN_TOKEN};

fn stdout_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn diagnose_exports_bounded_evidence_and_task_trace() {
    let stack = Stack::start();
    let output = stack.cli_as(ADMIN_TOKEN, &["diagnose", "--worker", "worker-a", "--json"]);
    assert!(
        output.status.success(),
        "diagnose failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle = stdout_json(&output);
    assert_eq!(bundle["health"]["instance"], stack.worker_a.instance);
    assert!(bundle["health"]["config_revision"].as_u64().unwrap_or(0) >= 1);
    assert!(bundle["health"]["asset_revision"].as_u64().unwrap_or(0) >= 1);
    let metrics = bundle["metrics"].as_array().expect("metrics");
    assert!(!metrics.is_empty());
    for metric in metrics {
        for key in metric["labels"].as_object().expect("labels").keys() {
            assert!(
                common::is_bounded_label(key),
                "unbounded metric label {key}"
            );
        }
    }

    // A product submission is explicitly distinguishable and carries one
    // trace id through the Gateway projection.
    let accepted = stack
        .submit(
            ADMIN_TOKEN,
            "worker-a",
            "general",
            "cli diagnostic evidence",
            "cli-diagnostic-session",
            "cli-diagnostic-key",
        )
        .json::<Value>()
        .expect("submit JSON");
    let execution = accepted["task_id"].as_str().expect("execution id");
    let projection = stack.wait_task(ADMIN_TOKEN, execution);
    let trace = projection["task"]["trace_id"].as_str().expect("trace");
    assert_eq!(trace.len(), 32);
    assert_eq!(projection["task"]["traffic"], "product");

    let output = stack.cli_as(ADMIN_TOKEN, &["task", execution, "--json"]);
    assert!(output.status.success());
    let task = stdout_json(&output);
    assert_eq!(task["task"]["trace_id"], trace);

    let output = stack.cli_as(
        ADMIN_TOKEN,
        &["diagnose", execution, "--worker", "worker-a", "--json"],
    );
    assert!(
        output.status.success(),
        "diagnose failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bundle = stdout_json(&output);
    assert_eq!(bundle["correlation"]["trace_id"], trace);
    assert_eq!(bundle["task"]["traffic"], "product");
    let text = serde_json::to_string(&bundle).unwrap();
    assert!(!text.contains("cli-diagnostic-session"));
}
