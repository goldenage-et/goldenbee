//! Native ADK plugin for coding execution observation.
//!
//! The pinned Runner dispatches `before_run`, `on_user_message`, `on_event`, and
//! `after_run`. This plugin uses `on_event` to observe the real model/tool event
//! stream. It never replaces, retries, or widens a tool: isolation remains owned
//! by the native toolset, workspace containment, and the shared sandbox.

use crate::package::ToolCounters;
use adk_rust::plugin::{Plugin, PluginConfig};
use adk_rust::Part;
use std::sync::Arc;

/// Run-scope audit plugin. Tool names and counts are attached to evidence.
pub fn audit_plugin(counters: Arc<ToolCounters>) -> Plugin {
    Plugin::new(PluginConfig {
        name: "coding-tool-audit".into(),
        on_event: Some(Box::new(move |_ctx, event| {
            let counters = counters.clone();
            Box::pin(async move {
                if let Some(content) = event.llm_response.content.as_ref() {
                    for part in &content.parts {
                        if let Part::FunctionCall { name, .. } = part {
                            counters.record(name);
                        }
                    }
                }
                // Never short-circuit or replace the native event.
                Ok(None)
            })
        })),
        ..Default::default()
    })
}
