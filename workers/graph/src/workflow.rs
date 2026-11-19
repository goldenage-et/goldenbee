//! Deterministic native graph definitions used by the graph Worker Package.
//!
//! `branching` is the default workflow. Operator package settings select the
//! deterministic `crash`, `interrupt`, and `cycle` scenarios used to prove
//! restart, checkpoint boundaries, and native recursion bounds.

use adk_rust::graph::{AgentNode, GraphAgent, GraphError, NodeOutput, END, START};
use adk_rust::prelude::LlmAgentBuilder;
use runtime::native::{model_for, GraphServices};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const CRASH_EXIT_CODE: i32 = 75;

/// Build the graph definition selected by package settings.
pub fn build(services: &GraphServices) -> Result<GraphAgent, GraphError> {
    let scenario = services
        .settings
        .get("scenario")
        .map(String::as_str)
        .unwrap_or("branching");
    match scenario {
        "cycle" => cycle(services),
        "interrupt" => branching(services, true, false),
        "crash" => branching(services, false, true),
        "approval" => approval(services),
        _ => branching(services, false, false),
    }
}

/// A committed first super-step followed by one native agent node whose
/// protected tool pauses for an exact approval. A kill during the approval wait
/// leaves a native checkpoint whose frontier still names the agent node.
fn approval(services: &GraphServices) -> Result<GraphAgent, GraphError> {
    let tool = services
        .tools
        .get(crate::approval::TOOL)
        .cloned()
        .ok_or_else(|| GraphError::Other("the approval effect tool is not registered".into()))?;
    let model = model_for("deterministic")
        .map_err(|error| GraphError::Other(format!("deterministic model failed: {error}")))?;
    let agent = LlmAgentBuilder::new("approval-node")
        .description("Runs one approval-protected native tool call")
        .instruction("Call the protected ledger tool exactly as requested.")
        .model(model)
        .tool(tool)
        .require_tool_confirmation(crate::approval::TOOL)
        .build()
        .map_err(|error| GraphError::Other(format!("approval agent failed: {error}")))?;
    GraphAgent::builder("branching")
        .description("Approval-gated native graph workflow")
        .channels(&["input", "messages", "output"])
        .checkpointer_arc(services.checkpointer.clone())
        .node_fn("prepare", |_ctx| async move {
            Ok(NodeOutput::new().with_update("output", json!("prepared")))
        })
        .node(AgentNode::new(Arc::new(agent)))
        .edge(START, "prepare")
        .edge("prepare", "approval-node")
        .edge("approval-node", END)
        .build()
}

fn effects_path(services: &GraphServices) -> Arc<PathBuf> {
    Arc::new(services.data_dir.join("graph-effects.log"))
}

fn append_effect(path: &Path, node: &str) -> Result<(), GraphError> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| GraphError::Other(format!("effect log unavailable: {error}")))?;
    writeln!(file, "{node}")
        .map_err(|error| GraphError::Other(format!("effect log write failed: {error}")))
}

fn input_text(ctx: &adk_rust::graph::NodeContext) -> String {
    ctx.state
        .get("input")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn branching(
    services: &GraphServices,
    interrupt_before_b: bool,
    crash_in_b: bool,
) -> Result<GraphAgent, GraphError> {
    let effects = effects_path(services);
    let crash_marker = Arc::new(services.data_dir.join("crash-b.once"));

    let a_effects = effects.clone();
    let b_effects = effects.clone();
    let c_effects = effects.clone();
    let b_marker = crash_marker.clone();

    let mut builder = GraphAgent::builder("branching")
        .description("Branching native ADK workflow with a persistent checkpoint")
        .channels(&["input", "messages", "output", "branch"])
        .checkpointer_arc(services.checkpointer.clone())
        .node_fn("a", move |ctx| {
            let effects = a_effects.clone();
            async move {
                append_effect(&effects, "a")?;
                let branch = if input_text(&ctx).contains("right") {
                    "right"
                } else {
                    "left"
                };
                Ok(NodeOutput::new()
                    .with_update("branch", json!(branch))
                    .with_update("output", json!(format!("a:{branch}"))))
            }
        })
        .node_fn("b", move |_ctx| {
            let effects = b_effects.clone();
            let marker = b_marker.clone();
            async move {
                append_effect(&effects, "b")?;
                if crash_in_b && marker.exists() {
                    let _ = std::fs::remove_file(marker.as_path());
                    std::process::exit(CRASH_EXIT_CODE);
                }
                Ok(NodeOutput::new().with_update("output", json!("b:left")))
            }
        })
        .node_fn("c", move |ctx| {
            let effects = c_effects.clone();
            async move {
                append_effect(&effects, "c")?;
                let branch = ctx
                    .state
                    .get("branch")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("left");
                Ok(NodeOutput::new().with_update("output", json!(format!("{branch}:complete"))))
            }
        })
        .edge(START, "a")
        .conditional_edge(
            "a",
            |state| {
                state
                    .get("branch")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("left")
                    .to_string()
            },
            [("left", "b"), ("right", "c")],
        )
        .edge("b", "c")
        .edge("c", END);
    if interrupt_before_b {
        builder = builder.interrupt_before(&["b"]);
    }
    builder.build()
}

fn cycle(services: &GraphServices) -> Result<GraphAgent, GraphError> {
    let effects = effects_path(services);
    GraphAgent::builder("branching")
        .description("Cyclic native ADK workflow bounded by the native recursion limit")
        .channels(&["input", "messages", "output", "loops"])
        .checkpointer_arc(services.checkpointer.clone())
        .node_fn("loop", move |ctx| {
            let effects = effects.clone();
            async move {
                append_effect(&effects, "loop")?;
                let loops = ctx
                    .state
                    .get("loops")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
                    + 1;
                Ok(NodeOutput::new()
                    .with_update("loops", json!(loops))
                    .with_update("output", json!(format!("loop:{loops}"))))
            }
        })
        .edge(START, "loop")
        .edge("loop", "loop")
        .build()
}
