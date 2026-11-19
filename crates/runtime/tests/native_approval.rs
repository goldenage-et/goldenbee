//! Pinned native confirmation proof before Worker glue.
//!
//! These tests use the exact `adk-agent 2.3.0` `LlmAgent` confirmation policy.
//! Without a handler the agent emits an interrupted event carrying the exact
//! `ToolConfirmationRequest`; with a handler the native loop asks before
//! dispatch and only then executes the tool.

use adk_rust::prelude::*;
use adk_rust::runner::Runner;
use adk_rust::session::{CreateRequest, InMemorySessionService, SessionService};
use adk_rust::{
    Agent, Content, SessionId, Tool, ToolConfirmationDecision, ToolConfirmationHandler,
    ToolConfirmationRequest, UserId,
};
use async_trait::async_trait;
use runtime::native::model_for;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug)]
struct CountingTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "effect_tool"
    }

    fn description(&self) -> &str {
        "Count one native dispatch"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({"type": "object", "properties": {"value": {"type": "string"}}}))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_rust::Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"applied": args}))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

#[derive(Debug)]
struct FixedDecision(ToolConfirmationDecision);

#[async_trait]
impl ToolConfirmationHandler for FixedDecision {
    async fn decide(
        &self,
        request: &ToolConfirmationRequest,
    ) -> adk_rust::Result<ToolConfirmationDecision> {
        assert_eq!(request.tool_name, "effect_tool");
        assert_eq!(request.args["value"], "exact");
        Ok(self.0)
    }
}

fn agent(calls: Arc<AtomicUsize>) -> Arc<dyn Agent> {
    let model = model_for("deterministic").expect("deterministic model");
    Arc::new(
        LlmAgentBuilder::new("approval-proof")
            .instruction("Call the protected tool when asked.")
            .model(model)
            .tool(Arc::new(CountingTool { calls }))
            .require_tool_confirmation("effect_tool")
            .build()
            .expect("native agent"),
    )
}

async fn run(
    agent: Arc<dyn Agent>,
    session: &str,
    run_config: Option<adk_rust::RunConfig>,
) -> Vec<adk_rust::Event> {
    let sessions = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name("approval-proof")
        .agent(agent)
        .session_service(sessions.clone())
        .build()
        .expect("native runner");
    let user = UserId::new("user").expect("user id");
    let session = SessionId::new(session).expect("session id");
    sessions
        .create(CreateRequest {
            app_name: "approval-proof".into(),
            user_id: "user".into(),
            session_id: Some(session.to_string()),
            state: Default::default(),
        })
        .await
        .expect("native session");
    let mut stream = runner
        .run_with_config(
            user,
            session,
            Content::new("user").with_text("call:effect_tool {\"value\":\"exact\"}"),
            run_config,
        )
        .await
        .expect("native run");
    let mut events = Vec::new();
    while let Some(event) = futures::StreamExt::next(&mut stream).await {
        events.push(event.expect("native event"));
    }
    events
}

#[tokio::test]
async fn pinned_native_confirmation_interrupts_before_dispatch_and_carries_exact_arguments() {
    let calls = Arc::new(AtomicUsize::new(0));
    let events = run(agent(calls.clone()), "native-interrupt", None).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the tool must not execute without a decision"
    );
    let interrupted = events
        .iter()
        .find(|event| event.llm_response.interrupted)
        .expect("native interruption event");
    let request = interrupted
        .actions
        .tool_confirmation
        .as_ref()
        .expect("native ToolConfirmationRequest");
    assert_eq!(request.tool_name, "effect_tool");
    assert_eq!(request.args, json!({"value": "exact"}));
    assert!(
        request
            .function_call_id
            .as_deref()
            .is_some_and(|id| !id.is_empty()),
        "a native decision is bound to the exact function call"
    );
}

#[tokio::test]
async fn pinned_native_confirmation_handler_decides_before_the_tool_runs() {
    let calls = Arc::new(AtomicUsize::new(0));
    let config = adk_rust::RunConfig::builder()
        .tool_confirmation_handler(Arc::new(FixedDecision(ToolConfirmationDecision::Approve)))
        .build();
    let events = run(agent(calls.clone()), "native-approve", Some(config)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        events
            .iter()
            .any(|event| event.actions.tool_confirmation_decision
                == Some(ToolConfirmationDecision::Approve)),
        "the native decision is recorded on the event"
    );

    let denied_calls = Arc::new(AtomicUsize::new(0));
    let denied_config = adk_rust::RunConfig::builder()
        .tool_confirmation_handler(Arc::new(FixedDecision(ToolConfirmationDecision::Deny)))
        .build();
    run(
        agent(denied_calls.clone()),
        "native-deny",
        Some(denied_config),
    )
    .await;
    assert_eq!(
        denied_calls.load(Ordering::SeqCst),
        0,
        "native denial blocks the tool"
    );
}
