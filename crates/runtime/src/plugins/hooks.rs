//! External process hooks using a JSON stdin/stdout protocol.
//!
//! The protocol is modeled on Gemini CLI hooks: one JSON object on stdin, one
//! optional JSON object on stdout, exit code `0` for success, `2` for a block,
//! and any other code for a handler failure. Goldenbee keeps its own schema and
//! still applies its own timeout, output ceiling, and permission boundary.
//!
//! An external hook can only continue or restrict. It cannot grant a capability
//! the runtime already denied, change a sandbox decision, or rewrite a
//! committed effect.

use crate::plugins::manifest::PluginHook;
use crate::worker::{
    CompactHookContext, CompactHookOutcome, HookDecision, SessionHookContext, StopHookContext,
    SubagentStopHookContext, TaskHookContext, ToolHookContext, ToolHookOutcome, WorkerHooks,
    WorkerResult,
};
use crate::ExecutionOutcome;
use async_trait::async_trait;
use common::ApiError;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Ordered external hooks belonging to one plugin bundle.
pub(super) struct ExternalHooks {
    plugin: String,
    label: String,
    root: PathBuf,
    hooks: Vec<ResolvedHook>,
    max_output_bytes: usize,
}

struct ResolvedHook {
    declaration: PluginHook,
    command: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookOutput {
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

impl ExternalHooks {
    /// Resolve and validate every declared hook script inside the plugin root.
    pub(super) fn new(
        plugin: &str,
        root: &Path,
        hooks: &[PluginHook],
        max_output_bytes: usize,
    ) -> Result<Option<Self>, ApiError> {
        if hooks.is_empty() {
            return Ok(None);
        }
        let mut resolved = Vec::with_capacity(hooks.len());
        for declaration in hooks {
            let command = root.join(&declaration.command);
            let command = command.canonicalize().map_err(|error| {
                ApiError::new(
                    "plugin.hook_unreadable",
                    format!(
                        "plugin '{plugin}' hook '{}' command '{}' is not readable: {error}",
                        declaration.name, declaration.command
                    ),
                )
            })?;
            if !command.starts_with(root) {
                return Err(ApiError::new(
                    "plugin.path_denied",
                    format!(
                        "plugin '{plugin}' hook '{}' command resolves outside the plugin directory",
                        declaration.name
                    ),
                ));
            }
            let metadata = std::fs::metadata(&command).map_err(|error| {
                ApiError::new(
                    "plugin.hook_unreadable",
                    format!(
                        "plugin '{plugin}' hook '{}' command is not readable: {error}",
                        declaration.name
                    ),
                )
            })?;
            if !metadata.is_file() {
                return Err(ApiError::new(
                    "plugin.hook_invalid",
                    format!(
                        "plugin '{plugin}' hook '{}' command is not a file",
                        declaration.name
                    ),
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    return Err(ApiError::new(
                        "plugin.hook_invalid",
                        format!(
                            "plugin '{plugin}' hook '{}' command is not executable",
                            declaration.name
                        ),
                    ));
                }
            }
            resolved.push(ResolvedHook {
                declaration: declaration.clone(),
                command,
            });
        }
        Ok(Some(Self {
            plugin: plugin.to_string(),
            label: format!("plugin:{plugin}"),
            root: root.to_path_buf(),
            hooks: resolved,
            max_output_bytes: max_output_bytes.max(1),
        }))
    }

    fn event_hooks<'a>(&'a self, event: &'a str) -> impl Iterator<Item = &'a ResolvedHook> {
        self.hooks
            .iter()
            .filter(move |hook| hook.declaration.event == event)
    }

    fn matches(hook: &ResolvedHook, tool: &str) -> bool {
        match hook.declaration.matcher.as_deref() {
            None => true,
            Some("*") => true,
            Some(matcher) => match matcher.strip_suffix('*') {
                Some(prefix) => tool.starts_with(prefix),
                None => matcher == tool,
            },
        }
    }

    fn envelope(
        &self,
        hook: &PluginHook,
        event: &str,
        task_id: Option<&str>,
        payload: Value,
    ) -> Value {
        json!({
            "schema": 1,
            "event": event,
            "plugin": self.plugin,
            "hook": hook.name,
            "task_id": task_id,
            "payload": payload,
        })
    }

    async fn run(&self, hook: &ResolvedHook, envelope: Value) -> WorkerResult<HookDecision> {
        let mut command = tokio::process::Command::new(&hook.command);
        command
            .args(&hook.declaration.args)
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            ApiError::new(
                "plugin.hook_spawn",
                format!(
                    "plugin '{}' hook '{}' could not start: {error}",
                    self.plugin, hook.declaration.name
                ),
            )
        })?;
        let input = serde_json::to_vec(&envelope).map_err(|error| {
            ApiError::new(
                "plugin.hook_protocol",
                format!(
                    "plugin '{}' hook '{}' payload could not be serialized: {error}",
                    self.plugin, hook.declaration.name
                ),
            )
        })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&input).await.map_err(|error| {
                ApiError::new(
                    "plugin.hook_io",
                    format!(
                        "plugin '{}' hook '{}' stdin failed: {error}",
                        self.plugin, hook.declaration.name
                    ),
                )
            })?;
            drop(stdin);
        }
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let maximum = self.max_output_bytes as u64;
        let stdout_reader = tokio::spawn(async move {
            let mut bytes = Vec::new();
            if let Some(stream) = stdout.as_mut() {
                let _ = stream.take(maximum).read_to_end(&mut bytes).await;
            }
            bytes
        });
        let stderr_reader = tokio::spawn(async move {
            let mut bytes = Vec::new();
            if let Some(stream) = stderr.as_mut() {
                let _ = stream.take(maximum).read_to_end(&mut bytes).await;
            }
            bytes
        });
        let deadline = Duration::from_millis(hook.declaration.timeout_ms.max(1));
        let status = match tokio::time::timeout(deadline, child.wait()).await {
            Ok(result) => result.map_err(|error| {
                ApiError::new(
                    "plugin.hook_io",
                    format!(
                        "plugin '{}' hook '{}' failed while waiting: {error}",
                        self.plugin, hook.declaration.name
                    ),
                )
            })?,
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                let _ = stdout_reader.await;
                let _ = stderr_reader.await;
                return Err(ApiError::new(
                    "hook.timeout",
                    format!(
                        "plugin '{}' hook '{}' exceeded {} ms",
                        self.plugin,
                        hook.declaration.name,
                        deadline.as_millis()
                    ),
                ));
            }
        };
        let stdout = stdout_reader.await.unwrap_or_default();
        let stderr = stderr_reader.await.unwrap_or_default();
        let code = status.code().unwrap_or(-1);
        if code == 2 {
            let reason = output_reason(&stdout, &stderr)
                .unwrap_or_else(|| format!("hook '{}' denied the action", hook.declaration.name));
            return Ok(HookDecision::Deny {
                reason: common::redact(&reason),
            });
        }
        if !status.success() {
            return Err(ApiError::new(
                "plugin.hook_failed",
                format!(
                    "plugin '{}' hook '{}' exited with status {code}: {}",
                    self.plugin,
                    hook.declaration.name,
                    common::redact(&String::from_utf8_lossy(&stderr))
                ),
            ));
        }
        let decision = parse_output(&stdout).map_err(|message| {
            ApiError::new(
                "plugin.hook_protocol",
                format!(
                    "plugin '{}' hook '{}' returned invalid JSON: {message}",
                    self.plugin, hook.declaration.name
                ),
            )
        })?;
        match decision.decision.as_deref() {
            None | Some("continue") | Some("allow") => Ok(HookDecision::Continue),
            Some("deny") | Some("block") => Ok(HookDecision::Deny {
                reason: common::redact(
                    decision
                        .reason
                        .unwrap_or_else(|| {
                            format!("hook '{}' denied the action", hook.declaration.name)
                        })
                        .as_str(),
                ),
            }),
            Some(other) => Err(ApiError::new(
                "plugin.hook_protocol",
                format!(
                    "plugin '{}' hook '{}' returned unknown decision '{other}'",
                    self.plugin, hook.declaration.name
                ),
            )),
        }
    }

    async fn decide(
        &self,
        event: &'static str,
        task_id: Option<&str>,
        tool: Option<&str>,
        payload: Value,
    ) -> WorkerResult<HookDecision> {
        for hook in self.event_hooks(event) {
            if let Some(tool) = tool {
                if !Self::matches(hook, tool) {
                    continue;
                }
            }
            let envelope = self.envelope(&hook.declaration, event, task_id, payload.clone());
            match self.run(hook, envelope).await? {
                HookDecision::Continue => {}
                denied => return Ok(denied),
            }
        }
        Ok(HookDecision::Continue)
    }

    async fn observe(
        &self,
        event: &'static str,
        task_id: Option<&str>,
        tool: Option<&str>,
        payload: Value,
    ) -> WorkerResult<()> {
        for hook in self.event_hooks(event) {
            if let Some(tool) = tool {
                if !Self::matches(hook, tool) {
                    continue;
                }
            }
            let envelope = self.envelope(&hook.declaration, event, task_id, payload.clone());
            let _ = self.run(hook, envelope).await?;
        }
        Ok(())
    }
}

fn parse_output(bytes: &[u8]) -> Result<HookOutput, String> {
    let text = String::from_utf8_lossy(bytes);
    if text.trim().is_empty() {
        return Ok(HookOutput {
            decision: None,
            reason: None,
        });
    }
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

fn output_reason(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    if let Ok(output) = parse_output(stdout) {
        if let Some(reason) = output.reason {
            return Some(reason);
        }
    }
    let text = String::from_utf8_lossy(stderr).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn task_payload(ctx: &TaskHookContext) -> Value {
    json!({
        "namespace": ctx.namespace,
        "caller": ctx.caller,
        "agent": ctx.agent,
        "session_id": ctx.session_id,
        "profile": ctx.profile,
        "profile_revision": ctx.profile_revision,
        "asset_revision": ctx.asset_revision,
        "input": common::redact(&ctx.input),
    })
}

fn tool_payload(ctx: &ToolHookContext) -> Value {
    json!({
        "tool": ctx.tool,
        "capability": ctx.capability,
        "risk": ctx.risk,
        "effect": ctx.effect,
        "arguments_digest": ctx.arguments_digest,
        "paths": ctx.paths,
        "command": ctx.command,
        "domain": ctx.domain,
        "sandbox": ctx.sandbox,
        "permission": ctx.permission,
        "call_id": ctx.call_id,
        "invocation": ctx.invocation,
        "profile": ctx.profile,
        "profile_revision": ctx.profile_revision,
    })
}

#[async_trait]
impl WorkerHooks for ExternalHooks {
    fn label(&self) -> &str {
        &self.label
    }

    async fn prompt_submit(&self, ctx: &TaskHookContext) -> WorkerResult<HookDecision> {
        self.decide("prompt_submit", Some(&ctx.task_id), None, task_payload(ctx))
            .await
    }

    async fn pre_tool(&self, ctx: &ToolHookContext) -> WorkerResult<HookDecision> {
        self.decide(
            "pre_tool",
            ctx.task_id.as_deref(),
            Some(&ctx.tool),
            tool_payload(ctx),
        )
        .await
    }

    async fn post_tool(
        &self,
        ctx: &ToolHookContext,
        outcome: &ToolHookOutcome,
    ) -> WorkerResult<()> {
        let mut payload = tool_payload(ctx);
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "outcome".into(),
                json!({
                    "ok": outcome.ok,
                    "error_code": outcome.error_code,
                    "duration_ms": outcome.duration_ms,
                    "result_digest": outcome.result_digest,
                    "output_bytes": outcome.output_bytes,
                    "truncated": outcome.truncated,
                }),
            );
        }
        self.observe(
            "post_tool",
            ctx.task_id.as_deref(),
            Some(&ctx.tool),
            payload,
        )
        .await
    }

    async fn stop(&self, ctx: &StopHookContext) -> WorkerResult<()> {
        self.observe(
            "stop",
            Some(&ctx.task.task_id),
            None,
            json!({
                "state": ctx.state,
                "error_code": ctx.error_code,
                "uncertainty": ctx.uncertainty,
            }),
        )
        .await
    }

    async fn subagent_stop(&self, ctx: &SubagentStopHookContext) -> WorkerResult<()> {
        self.observe(
            "subagent_stop",
            Some(&ctx.parent_task_id),
            None,
            json!({
                "child_task_id": ctx.child_task_id,
                "agent": ctx.agent,
                "session_id": ctx.session_id,
                "state": ctx.state,
            }),
        )
        .await
    }

    async fn pre_compact(&self, ctx: &CompactHookContext) -> WorkerResult<()> {
        self.observe(
            "pre_compact",
            Some(&ctx.task.task_id),
            None,
            compact_payload(ctx, None),
        )
        .await
    }

    async fn post_compact(
        &self,
        ctx: &CompactHookContext,
        outcome: &CompactHookOutcome,
    ) -> WorkerResult<()> {
        self.observe(
            "post_compact",
            Some(&ctx.task.task_id),
            None,
            compact_payload(ctx, Some(outcome.events_after)),
        )
        .await
    }

    async fn session_start(&self, ctx: &SessionHookContext) -> WorkerResult<()> {
        self.observe(
            "session_start",
            ctx.task_id.as_deref(),
            None,
            session_payload(ctx),
        )
        .await
    }

    async fn session_end(&self, ctx: &SessionHookContext) -> WorkerResult<()> {
        self.observe(
            "session_end",
            ctx.task_id.as_deref(),
            None,
            session_payload(ctx),
        )
        .await
    }

    async fn after_task(
        &self,
        _ctx: &TaskHookContext,
        _outcome: &ExecutionOutcome,
    ) -> WorkerResult<()> {
        // `stop` is the plugin-facing terminal event. The compiled `after_task`
        // hook remains package code only.
        Ok(())
    }
}

fn compact_payload(ctx: &CompactHookContext, events_after: Option<usize>) -> Value {
    json!({
        "strategy": ctx.strategy,
        "context_budget": ctx.context_budget,
        "preserve_recent": ctx.preserve_recent,
        "attempt": ctx.attempt,
        "events_before": ctx.events_before,
        "events_after": events_after,
    })
}

fn session_payload(ctx: &SessionHookContext) -> Value {
    json!({
        "namespace": ctx.namespace,
        "caller": ctx.caller,
        "agent": ctx.agent,
        "session_id": ctx.session_id,
        "profile": ctx.profile,
        "profile_revision": ctx.profile_revision,
        "asset_revision": ctx.asset_revision,
    })
}
