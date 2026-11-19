//! Compiled native ADK callbacks and plugin for the Knowledge package.
//!
//! The before-tool callback re-checks source authorization before the toolset
//! executes. A denied URI therefore remains denied through hooks as well as
//! through the adapter and tool. The plugin demonstrates a run-scope native
//! boundary without replacing the Runner or agent loop.

use crate::service::KnowledgeCell;
use adk_rust::plugin::{Plugin, PluginConfig};
use adk_rust::prelude::{Content, Part};
use adk_rust::FunctionResponseData;
use runtime::worker::AgentCallbacks;

/// Compiled per-agent native callbacks for automatically loaded agents.
pub fn native_callbacks(cell: KnowledgeCell) -> AgentCallbacks {
    AgentCallbacks {
        before_tool: Some(Box::new(move |ctx| {
            let cell = cell.clone();
            Box::pin(async move {
                let name = ctx.tool_name().unwrap_or_default();
                if !matches!(name, "knowledge_ingest" | "knowledge_remove") {
                    return Ok(None);
                }
                let uri = ctx
                    .tool_input()
                    .and_then(|input| input.get("uri"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let denial = if name == "knowledge_remove" {
                    cell.removal_denial(uri).await
                } else {
                    cell.denial(uri).await
                };
                let Some(denial) = denial else {
                    return Ok(None);
                };
                let mut content = Content::new("function");
                content.parts.push(Part::FunctionResponse {
                    function_response: FunctionResponseData::new(name, denial.to_json()),
                    id: None,
                    annotations: None,
                });
                Ok(Some(content))
            })
        })),
        ..Default::default()
    }
}

/// Native run-scope plugin proving package policy attaches to loaded agents.
pub fn knowledge_plugin() -> Plugin {
    Plugin::new(PluginConfig {
        name: "knowledge-policy".into(),
        before_run: Some(Box::new(|ctx| {
            Box::pin(async move {
                if text_of(ctx.user_content()).contains("knowledge-plugin-deny") {
                    return Ok(Some(
                        Content::new("model").with_text("knowledge.access_denied: plugin policy"),
                    ));
                }
                Ok(None)
            })
        })),
        ..Default::default()
    })
}

fn text_of(content: &Content) -> String {
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
