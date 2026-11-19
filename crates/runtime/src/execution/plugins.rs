//! Native callback and plugin wiring for automatically loaded agents.

use adk_rust::plugin::{Plugin, PluginManager};
use adk_rust::prelude::LlmAgentBuilder;
use adk_rust::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, BeforeAgentCallback,
    BeforeModelCallback, BeforeModelResult, BeforeToolCallback,
};
use std::sync::Arc;

/// Native per-agent callbacks applied to every automatically loaded agent.
///
/// Callbacks are constructed fresh for each agent so the same registration is
/// never shared between two native agent instances.
#[derive(Default)]
pub struct AgentCallbacks {
    /// Before the agent runs. Returning content short-circuits the agent.
    pub before_agent: Option<BeforeAgentCallback>,
    /// After the agent runs. Returning content replaces the result.
    pub after_agent: Option<AfterAgentCallback>,
    /// Before a model call. Can modify the request or skip the call.
    pub before_model: Option<BeforeModelCallback>,
    /// After a model call. Can modify the response.
    pub after_model: Option<AfterModelCallback>,
    /// Before a tool call. Returning content skips the tool.
    pub before_tool: Option<BeforeToolCallback>,
    /// After a tool call. Returning content replaces the result.
    pub after_tool: Option<AfterToolCallback>,
}

impl AgentCallbacks {
    /// Compose one native before-model callback ahead of the package callbacks.
    ///
    /// The additional callback runs first so memory recall can inject context;
    /// the package callback still observes and may modify or skip the request.
    pub fn with_before_model(mut self, additional: BeforeModelCallback) -> Self {
        let existing = self.before_model.take().map(Arc::new);
        self.before_model = Some(Box::new(move |ctx, request| {
            let recall = additional(ctx.clone(), request);
            let existing = existing.clone();
            Box::pin(async move {
                match recall.await? {
                    BeforeModelResult::Continue(request) => match existing {
                        Some(existing) => existing(ctx, request).await,
                        None => Ok(BeforeModelResult::Continue(request)),
                    },
                    BeforeModelResult::Skip(response) => Ok(BeforeModelResult::Skip(response)),
                }
            })
        }));
        self
    }

    /// Apply these callbacks to a native agent builder.
    pub fn apply(self, mut builder: LlmAgentBuilder) -> LlmAgentBuilder {
        if let Some(callback) = self.before_agent {
            builder = builder.before_callback(callback);
        }
        if let Some(callback) = self.after_agent {
            builder = builder.after_callback(callback);
        }
        if let Some(callback) = self.before_model {
            builder = builder.before_model_callback(callback);
        }
        if let Some(callback) = self.after_model {
            builder = builder.after_model_callback(callback);
        }
        if let Some(callback) = self.before_tool {
            builder = builder.before_tool_callback(callback);
        }
        if let Some(callback) = self.after_tool {
            builder = builder.after_tool_callback(callback);
        }
        builder
    }
}

pub(crate) fn build_plugins(plugins: Vec<Plugin>) -> Option<Arc<PluginManager>> {
    if plugins.is_empty() {
        None
    } else {
        Some(Arc::new(PluginManager::new(plugins)))
    }
}
