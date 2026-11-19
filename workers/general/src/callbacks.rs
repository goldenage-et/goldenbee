//! Compiled native ADK callbacks for the General package.
//!
//! Marker strings keep normal inputs unchanged; tests use them to observe that
//! callbacks really run through the pinned Runner.

use adk_rust::prelude::{Content, Part};
use adk_rust::{BeforeModelResult, FunctionResponseData};
use runtime::worker::AgentCallbacks;

/// Compiled per-agent native callbacks. Markers keep normal inputs unchanged.
pub fn native_callbacks() -> AgentCallbacks {
    AgentCallbacks {
        before_agent: Some(Box::new(|ctx| {
            Box::pin(async move {
                if text_of(ctx.user_content()).contains("cb-short") {
                    return Ok(Some(Content::new("model").with_text("agent-short-circuit")));
                }
                Ok(None)
            })
        })),
        after_agent: None,
        before_model: Some(Box::new(|_ctx, mut request| {
            Box::pin(async move {
                if let Some(last) = request
                    .contents
                    .iter_mut()
                    .rev()
                    .find(|content| content.role == "user")
                {
                    if text_of(last).contains("cb:") {
                        append_text(last, " [agent-before]");
                    }
                }
                Ok(BeforeModelResult::Continue(request))
            })
        })),
        after_model: Some(Box::new(|_ctx, mut response| {
            Box::pin(async move {
                if let Some(content) = response.content.as_mut() {
                    if text_of(content).contains("[agent-before]")
                        && !text_of(content).contains("[agent-after]")
                    {
                        append_text(content, " [agent-after]");
                        return Ok(Some(response));
                    }
                }
                Ok(None)
            })
        })),
        before_tool: Some(Box::new(|ctx| {
            Box::pin(async move {
                let command = ctx
                    .tool_input()
                    .and_then(|input| input.get("command"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if ctx.tool_name() == Some("exec_command") && command.contains("cb-skip-tool") {
                    let name = ctx.tool_name().unwrap_or("exec_command").to_string();
                    let mut content = Content::new("function");
                    content.parts.push(Part::FunctionResponse {
                        function_response: FunctionResponseData::new(
                            name,
                            serde_json::json!({"skipped": "agent-before-tool"}),
                        ),
                        id: None,
                        annotations: None,
                    });
                    return Ok(Some(content));
                }
                Ok(None)
            })
        })),
        after_tool: None,
    }
}

pub(crate) fn text_of(content: &Content) -> String {
    content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

pub(crate) fn append_text(content: &mut Content, suffix: &str) {
    if let Some(Part::Text { text }) = content
        .parts
        .iter_mut()
        .find(|part| matches!(part, Part::Text { .. }))
    {
        text.push_str(suffix);
    } else {
        content.parts.push(Part::Text {
            text: suffix.to_string(),
        });
    }
}
