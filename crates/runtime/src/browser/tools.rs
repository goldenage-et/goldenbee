//! Native browser tools wrapped with harness policy and resource bounds.
//!
//! Tool discovery must not start a browser. The dynamic toolset builds the
//! pinned native tools from an unstarted session template, then each call
//! applies destination policy before resolving the authenticated session and
//! executing the real native tool.

use crate::browser::session::{BrowserController, SessionKey};
use crate::browser::BrowserPolicy;
use adk_rust::{ReadonlyContext, Result, Tool, ToolContext, Toolset};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// Model-selected uploads read arbitrary host files and send them to a site.
/// The browser capability is not an authorized file exfiltration channel.
const DENIED_TOOLS: &[&str] = &["browser_file_upload"];

/// Dynamic toolset resolving the authenticated session's isolated browser.
pub struct BrowserCapabilityToolset {
    controller: Arc<BrowserController>,
}

impl BrowserCapabilityToolset {
    /// Wrap one instance-owned controller as a native toolset.
    pub fn new(controller: Arc<BrowserController>) -> Self {
        Self { controller }
    }
}

#[async_trait]
impl Toolset for BrowserCapabilityToolset {
    fn name(&self) -> &str {
        self.controller.id()
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        if !self.controller.enabled().await {
            return Ok(Vec::new());
        }
        let settings = self.controller.settings().await;
        let key = SessionKey {
            user: ctx.user_id().to_string(),
            session: ctx.session_id().to_string(),
        };
        let template = Arc::new(adk_browser::BrowserSession::new(
            adk_browser::BrowserConfig::new()
                .webdriver_url("http://127.0.0.1:1")
                .browser(settings.browser.native())
                .headless(true)
                .require_explicit_start(true),
        ));
        let tools = adk_browser::BrowserToolset::with_profile(template, settings.profile.native())
            .all_tools()
            .into_iter()
            .filter(|tool| !DENIED_TOOLS.contains(&tool.name()))
            .map(|tool| {
                Arc::new(GuardedTool {
                    name: tool.name().to_string(),
                    template: tool,
                    controller: self.controller.clone(),
                    key: key.clone(),
                    policy: settings.policy.clone(),
                    timeout: TOOL_TIMEOUT,
                    max_output_bytes: settings.max_output_bytes,
                }) as Arc<dyn Tool>
            })
            .collect();
        Ok(tools)
    }
}

struct GuardedTool {
    name: String,
    template: Arc<dyn Tool>,
    controller: Arc<BrowserController>,
    key: SessionKey,
    policy: BrowserPolicy,
    timeout: Duration,
    max_output_bytes: usize,
}

#[async_trait]
impl Tool for GuardedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        self.template.description()
    }

    fn enhanced_description(&self) -> String {
        self.template.enhanced_description()
    }

    fn declaration(&self) -> Value {
        self.template.declaration()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.template.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.template.response_schema()
    }

    fn required_scopes(&self) -> &[&str] {
        self.template.required_scopes()
    }

    fn is_read_only(&self) -> bool {
        self.template.is_read_only()
    }

    fn is_concurrency_safe(&self) -> bool {
        self.template.is_concurrency_safe()
    }

    fn is_long_running(&self) -> bool {
        self.template.is_long_running()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if ctx.is_cancelled() {
            return Err(adk_rust::AdkError::tool(
                "browser.cancelled: the task was cancelled before the browser call",
            ));
        }
        // Serialize with human takeover and validate ownership after acquiring
        // the gate. A command queued before takeover therefore cannot execute
        // after the new controller has been announced.
        let gate = self.controller.control_gate(&self.key);
        let _gate = gate.lock().await;
        if ctx.is_cancelled() {
            return Err(adk_rust::AdkError::tool(
                "browser.cancelled: the task was cancelled while waiting for the browser",
            ));
        }
        self.controller
            .begin_agent_command(&self.key)
            .map_err(|error| {
                adk_rust::AdkError::tool(format!("{}: {}", error.code, error.message))
            })?;
        let navigation = is_navigation(&self.name);
        if navigation {
            if let Some(url) = args.get("url").and_then(Value::as_str) {
                check_navigation(&self.policy, url).await?;
            }
        }
        let entry = self
            .controller
            .get_or_create_key(self.key.clone())
            .await
            .map_err(|error| {
                adk_rust::AdkError::tool(format!("{}: {}", error.code, error.message))
            })?;
        let settings = self.controller.settings().await;
        let tool = adk_browser::BrowserToolset::with_profile(
            entry.session.clone(),
            settings.profile.native(),
        )
        .all_tools()
        .into_iter()
        .find(|tool| tool.name() == self.name)
        .ok_or_else(|| {
            adk_rust::AdkError::tool(format!(
                "browser.tool_missing: {} is not exposed by the configured profile",
                self.name
            ))
        })?;
        let result = tokio::time::timeout(self.timeout, tool.execute(ctx, args))
            .await
            .map_err(|_| {
                adk_rust::AdkError::tool(format!(
                    "browser.timeout: {} exceeded {:?}",
                    self.name, self.timeout
                ))
            })??;
        if navigation {
            let current = entry.session.current_url().await.unwrap_or_default();
            if let Err(error) = self.policy.check_url(&current) {
                let _ = entry.session.navigate("about:blank").await;
                return Err(adk_rust::AdkError::tool(format!(
                    "browser.redirect_denied: navigation ended at a disallowed destination ({}: {})",
                    error.code, error.message
                )));
            }
        }
        Ok(cap_value(result, self.max_output_bytes))
    }
}

fn is_navigation(name: &str) -> bool {
    matches!(
        name,
        "browser_navigate"
            | "browser_new_tab"
            | "browser_new_window"
            | "browser_back"
            | "browser_forward"
            | "browser_refresh"
    )
}

pub(crate) async fn check_navigation(policy: &BrowserPolicy, value: &str) -> Result<()> {
    let url = policy
        .check_url(value)
        .map_err(|error| adk_rust::AdkError::tool(format!("{}: {}", error.code, error.message)))?;
    let host = url.host_str().unwrap_or_default().to_string();
    let port = url.port_or_known_default().unwrap_or(0);
    let addresses = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .map_err(|_| {
        adk_rust::AdkError::tool("browser.destination_denied: destination lookup timed out")
    })?
    .map_err(|error| {
        adk_rust::AdkError::tool(format!(
            "browser.destination_denied: destination lookup failed: {error}"
        ))
    })?;
    for address in addresses {
        policy
            .check_resolved(&host, port, url.scheme(), &address.ip().to_string())
            .map_err(|error| {
                adk_rust::AdkError::tool(format!("{}: {}", error.code, error.message))
            })?;
    }
    Ok(())
}

/// Bound a tool result before it reaches the model or an event stream.
fn cap_value(value: Value, max_bytes: usize) -> Value {
    let mut used = 0_usize;
    let mut truncated = false;
    cap_inner(value, max_bytes, &mut used, &mut truncated)
}

fn cap_inner(value: Value, max: usize, used: &mut usize, truncated: &mut bool) -> Value {
    match value {
        Value::String(text) => {
            let remaining = max.saturating_sub(*used);
            if text.len() <= remaining {
                *used += text.len();
                Value::String(text)
            } else {
                *truncated = true;
                let end = floor_char_boundary(&text, remaining.saturating_sub(32));
                *used = max;
                Value::String(format!("{}…[truncated]", &text[..end]))
            }
        }
        Value::Array(values) => {
            let mut capped = Vec::new();
            for value in values {
                if *used >= max {
                    *truncated = true;
                    break;
                }
                capped.push(cap_inner(value, max, used, truncated));
            }
            Value::Array(capped)
        }
        Value::Object(map) => {
            let mut capped = serde_json::Map::new();
            for (key, value) in map {
                if *used >= max {
                    *truncated = true;
                    break;
                }
                capped.insert(key, cap_inner(value, max, used, truncated));
            }
            if *truncated {
                capped.insert("truncated".into(), Value::Bool(true));
            }
            Value::Object(capped)
        }
        other => other,
    }
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut index = index;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}
