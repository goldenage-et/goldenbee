//! Direct Worker and Gateway execution equivalence; Worker independence.

mod support;

use serde_json::Value;
use support::{Stack, GATEWAY_TOKEN};

#[test]
fn direct_and_gateway_execution_are_equivalent_with_distinct_routing_metadata() {
    let mut stack = Stack::start();
    let input = "equivalent deterministic execution";

    let accepted: Value = stack
        .submit(
            GATEWAY_TOKEN,
            "worker-a",
            "general",
            input,
            "session-gateway",
            "equivalence-gateway",
        )
        .json()
        .unwrap();
    let routed_id = accepted["task_id"].as_str().unwrap().to_string();
    let routed = stack.wait_task(GATEWAY_TOKEN, &routed_id);
    let worker_task_id = routed["task"]["id"].as_str().expect("worker task id");
    let routed_worker_task = stack.worker_a.task(worker_task_id);

    let direct: Value =
        stack
            .worker_a
            .submit("general", input, "session-direct", "equivalence-direct");
    let direct_id = direct["task_id"].as_str().unwrap().to_string();
    let direct_task = stack.worker_a.wait_direct(&direct_id);

    assert_eq!(direct_task.state, routed_worker_task.state);
    assert_eq!(direct_task.agent, routed_worker_task.agent);
    assert_eq!(direct_task.output, routed_worker_task.output);
    assert_eq!(
        routed_worker_task.session_id, "session-gateway",
        "Gateway preserves the caller's explicit session identity"
    );
    assert_eq!(direct_task.session_id, "session-direct");
    assert_eq!(
        routed_worker_task.caller, "caller-1",
        "the Worker sees the Gateway-delegated caller identity"
    );
    assert_eq!(direct_task.caller, "local");

    assert_eq!(routed["worker"], "worker-a");
    assert_eq!(routed["worker_instance"], stack.worker_a.instance);
    assert_eq!(routed["task_id"], routed_id);
    assert_eq!(
        routed["task"]["output"].as_str(),
        direct_task.output.as_deref(),
        "Gateway must not alter the real Worker result"
    );
    let direct_json = serde_json::to_value(&direct_task).unwrap();
    assert!(
        direct_json.get("worker").is_none(),
        "routing metadata belongs to Gateway, not the Worker projection"
    );

    stack.kill_gateway();
    let after = stack.worker_a.submit(
        "general",
        "worker survives gateway shutdown",
        "session-standalone",
        "standalone",
    );
    let after_id = after["task_id"].as_str().unwrap().to_string();
    let standalone = stack.worker_a.wait_direct(&after_id);
    assert_eq!(standalone.state, common::TaskState::Succeeded);
    assert!(standalone
        .output
        .unwrap_or_default()
        .contains("worker survives gateway shutdown"));
}
