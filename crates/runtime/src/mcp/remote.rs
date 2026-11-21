//! Native remote MCP Streamable HTTP connections with Worker-owned policy.
//!
//! ADK supplies the MCP client, toolset, schema conversion, and Runner tool
//! dispatch. This module supplies the deployment-specific pieces the pinned
//! `McpHttpClientBuilder` does not expose: destination/TLS policy, pinned DNS,
//! redirect refusal, credential audience/scope checks, revocation, a fixed
//! discovery ceiling, and truthful ambiguous-outcome reporting.

use crate::mcp::config::{McpConfig, McpServerConfig};
use crate::mcp::credentials::{CredentialGrant, McpCredentials};
use crate::mcp::health::{self, McpServerHealth, McpState};
use crate::mcp::names;
use crate::mcp::policy::{DestinationPolicy, VettedDestination};
use crate::mcp::surface;
use adk_rust::{
    AdkError, ErrorCategory, ErrorComponent, ReadonlyContext, Result as AdkResult, Toolset,
};
use adk_tool::mcp::rmcp::service::RunningServiceCancellationToken;
use adk_tool::mcp::rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use adk_tool::mcp::rmcp::ServiceExt;
use adk_tool::mcp::{
    AdkClientHandler, AutoDeclineElicitationHandler, ClientLifecycleMode, ClientServiceExt,
    McpSchemaLimits, McpToolset,
};
use async_trait::async_trait;
use common::{
    ApiError, McpPromptContent, McpPromptDescriptor, McpResourceContent, McpResourceDescriptor,
    ProfileRevision,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;
use tokio::time::timeout;

/// Maximum tool description retained in diagnostics and evidence.
const MAX_ERROR_CHARS: usize = 512;

/// Instance-owned registry of configured remote MCP connections.
pub struct RemoteMcpRegistry {
    servers: BTreeMap<String, Arc<RemoteServer>>,
    uncertainty: Mutex<BTreeMap<String, String>>,
    grants: RwLock<BTreeSet<String>>,
    kernel: Option<crate::tools::ToolKernel>,
}

impl RemoteMcpRegistry {
    /// Create the registry after configuration validation.
    pub fn new(
        config: McpConfig,
        credentials: McpCredentials,
        grants: &[String],
        kernel: Option<crate::tools::ToolKernel>,
    ) -> Arc<Self> {
        let servers = config
            .servers
            .iter()
            .filter(|(_, server)| server.is_remote())
            .map(|(id, server)| {
                (
                    id.clone(),
                    Arc::new(RemoteServer::new(
                        id.clone(),
                        server.clone(),
                        credentials.clone(),
                    )),
                )
            })
            .collect();
        Arc::new(Self {
            servers,
            uncertainty: Mutex::new(BTreeMap::new()),
            grants: RwLock::new(grants.iter().cloned().collect()),
            kernel,
        })
    }

    /// Whether the operator configured this connection id.
    pub fn known(&self, id: &str) -> bool {
        self.servers.contains_key(id)
    }

    /// Record the instance grants in effect after a validated reload.
    pub fn set_grants(&self, grants: &[String]) {
        if let Ok(mut current) = self.grants.write() {
            *current = grants.iter().cloned().collect();
        }
    }

    fn granted(&self, id: &str) -> bool {
        let grant = self
            .servers
            .get(id)
            .map(|server| server.grant())
            .unwrap_or_else(|| names::connection_grant(id));
        self.grants
            .read()
            .map(|grants| names::granted(&grants, &grant))
            .unwrap_or(false)
    }

    /// Observable state of every configured remote connection.
    pub async fn health(&self) -> Vec<McpServerHealth> {
        let mut health = Vec::new();
        for (id, server) in &self.servers {
            health.push(server.health(id, self.granted(id)).await);
        }
        health
    }

    /// Inventory projection for `/v1/capabilities` and `/v1/mcp/servers`.
    pub async fn inventory(&self) -> Vec<common::McpServerInventory> {
        let mut inventory = Vec::new();
        for (id, server) in &self.servers {
            let record = server.health(id, self.granted(id)).await;
            inventory.push(record.inventory(&server.config));
        }
        inventory
    }

    /// Restart one remote connection with the current credential generation.
    pub async fn restart(&self, id: &str) -> Result<(), ApiError> {
        let Some(server) = self.servers.get(id) else {
            return Err(ApiError::new(
                "mcp.connection_unknown",
                format!("MCP connection '{id}' is not configured"),
            ));
        };
        if !self.granted(id) {
            return Err(ApiError::new(
                "mcp.grant_denied",
                "connection is outside instance authority",
            ));
        }
        server.restart().await
    }

    /// Discover allowlisted resources for one remote connection.
    pub async fn resources(&self, id: &str) -> Result<Vec<McpResourceDescriptor>, ApiError> {
        let (toolset, config) = self.client(id).await?;
        let grants = self.grant_snapshot();
        let policy = surface::policy(&config, id, &grants);
        surface::list_resources(&toolset, &policy).await
    }

    /// Read one allowlisted resource with a bounded result.
    pub async fn read_resource(&self, id: &str, uri: &str) -> Result<McpResourceContent, ApiError> {
        let (toolset, config) = self.client(id).await?;
        let grants = self.grant_snapshot();
        let policy = surface::policy(&config, id, &grants);
        surface::read_resource(&toolset, &policy, uri).await
    }

    /// Discover allowlisted prompts for one remote connection.
    pub async fn prompts(&self, id: &str) -> Result<Vec<McpPromptDescriptor>, ApiError> {
        let (toolset, config) = self.client(id).await?;
        let grants = self.grant_snapshot();
        let policy = surface::policy(&config, id, &grants);
        surface::list_prompts(&toolset, &policy).await
    }

    /// Retrieve one allowlisted prompt with a bounded result.
    pub async fn prompt(
        &self,
        id: &str,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<McpPromptContent, ApiError> {
        let (toolset, config) = self.client(id).await?;
        let grants = self.grant_snapshot();
        let policy = surface::policy(&config, id, &grants);
        surface::get_prompt(&toolset, &policy, name, arguments).await
    }

    async fn client(
        &self,
        id: &str,
    ) -> Result<(Arc<McpToolset<AdkClientHandler>>, McpServerConfig), ApiError> {
        let server = self.servers.get(id).ok_or_else(|| {
            ApiError::new(
                "mcp.connection_unknown",
                format!("MCP connection '{id}' is not configured"),
            )
        })?;
        if !self.granted(id) {
            return Err(ApiError::new(
                "mcp.grant_denied",
                "connection is outside instance authority",
            ));
        }
        server.live().await
    }

    fn grant_snapshot(&self) -> BTreeSet<String> {
        self.grants
            .read()
            .map(|grants| grants.clone())
            .unwrap_or_default()
    }

    /// Validate profile-scoped connection ids, grants, credentials, and
    /// destinations before a native Runner is started.
    pub async fn preflight(&self, revision: &ProfileRevision) -> Result<(), ApiError> {
        for id in &revision.mcp {
            // Local stdio connections are validated and served by the local
            // registry; this registry only preflights remote definitions.
            let Some(server) = self.servers.get(id).cloned() else {
                continue;
            };
            if !server.config.enabled {
                return Err(ApiError::new(
                    "mcp.connection_disabled",
                    format!("MCP connection '{id}' is disabled for this instance"),
                ));
            }
            let grant = server.grant();
            if !revision
                .grants
                .iter()
                .any(|value| value == "*" || value == &grant)
            {
                return Err(ApiError::new(
                    "profile.mcp_grant_missing",
                    format!("profile does not grant MCP connection '{id}'"),
                ));
            }
            if revision.restrictions.iter().any(|value| value == &grant) {
                return Err(ApiError::new(
                    "profile.mcp_grant_denied",
                    format!("profile restricts MCP connection '{id}'"),
                ));
            }
            server.preflight().await?;
        }
        Ok(())
    }

    /// Build the per-invocation toolset for a frozen profile revision.
    pub fn toolset(
        self: &Arc<Self>,
        revision: &ProfileRevision,
    ) -> Option<Arc<dyn adk_rust::Toolset>> {
        if revision.mcp.is_empty() {
            return None;
        }
        let servers = revision
            .mcp
            .iter()
            .filter_map(|id| self.servers.get(id).cloned())
            .collect::<Vec<_>>();
        if servers.is_empty() {
            return None;
        }
        Some(Arc::new(RemoteMcpToolset {
            registry: Arc::clone(self),
            servers,
            name: "remote_mcp".into(),
        }))
    }

    /// Record that a possibly committed write has an unknown outcome.
    pub async fn mark_uncertain(&self, session_id: &str, reason: &str) {
        self.uncertainty
            .lock()
            .await
            .entry(session_id.to_string())
            .or_insert_with(|| reason.to_string());
    }

    /// Take any uncertain outcome produced during a session invocation.
    pub async fn take_uncertainty(&self, session_id: &str) -> Option<String> {
        self.uncertainty.lock().await.remove(session_id)
    }

    /// Whether an earlier call in this invocation already has an unknown effect.
    pub async fn has_uncertainty(&self, session_id: &str) -> bool {
        self.uncertainty.lock().await.contains_key(session_id)
    }

    /// Close every live connection and cancel pending handshakes.
    pub async fn shutdown(&self) {
        for server in self.servers.values() {
            server.shutdown().await;
        }
    }
}

/// The toolset injected through the native Runner's per-invocation
/// `runtime_toolsets`, so file-loaded, profile-built, and manually registered
/// agents see exactly the same resolved connection context.
struct RemoteMcpToolset {
    registry: Arc<RemoteMcpRegistry>,
    servers: Vec<Arc<RemoteServer>>,
    name: String,
}

#[async_trait]
impl adk_rust::Toolset for RemoteMcpToolset {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(
        &self,
        ctx: Arc<dyn ReadonlyContext>,
    ) -> AdkResult<Vec<Arc<dyn adk_rust::Tool>>> {
        let mut tools: Vec<Arc<dyn adk_rust::Tool>> = Vec::new();
        for server in &self.servers {
            let snapshots = server.discover(ctx.clone()).await.map_err(|error| {
                adk_error(category_for_code(&error.code), &error.code, &error.message)
            })?;
            for snapshot in snapshots {
                let exposed = names::tool_id(&server.id, &snapshot.name);
                if tools.iter().any(|existing| existing.name() == exposed) {
                    return Err(AdkError::new(
                        ErrorComponent::Tool,
                        ErrorCategory::Internal,
                        "mcp.tool_conflict",
                        format!("MCP toolsets expose duplicate tool name '{exposed}'"),
                    ));
                }
                let tool = Arc::new(PinnedMcpTool {
                    registry: Arc::clone(&self.registry),
                    server: server.clone(),
                    original: snapshot.name,
                    name: exposed,
                    description: snapshot.description,
                    parameters: snapshot.parameters,
                    response: snapshot.response,
                    read_only: snapshot.read_only,
                    long_running: snapshot.long_running,
                }) as Arc<dyn adk_rust::Tool>;
                tools.push(match &self.registry.kernel {
                    Some(kernel) => kernel.wrap(tool, crate::mcp::toolset::descriptor(&server.id)),
                    None => tool,
                });
            }
        }
        Ok(tools)
    }
}

/// A discovered MCP tool pinned to the configured name ceiling.
struct PinnedMcpTool {
    registry: Arc<RemoteMcpRegistry>,
    server: Arc<RemoteServer>,
    original: String,
    name: String,
    description: String,
    parameters: Option<Value>,
    response: Option<Value>,
    read_only: bool,
    long_running: bool,
}

#[async_trait]
impl adk_rust::Tool for PinnedMcpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.parameters.clone()
    }

    fn response_schema(&self) -> Option<Value> {
        self.response.clone()
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn is_long_running(&self) -> bool {
        self.long_running
    }

    async fn execute(&self, ctx: Arc<dyn adk_rust::ToolContext>, args: Value) -> AdkResult<Value> {
        self.server
            .call(&self.registry, &self.original, ctx, args, self.read_only)
            .await
    }
}

#[derive(Clone)]
struct ToolSnapshot {
    name: String,
    description: String,
    parameters: Option<Value>,
    response: Option<Value>,
    read_only: bool,
    long_running: bool,
}

struct LiveConnection {
    toolset: Arc<McpToolset<AdkClientHandler>>,
    cancel: RunningServiceCancellationToken,
    fingerprint: String,
    generation: u64,
    connected_at: i64,
    tools: BTreeMap<String, (ToolSnapshot, Arc<dyn adk_rust::Tool>)>,
}

struct RemoteServer {
    id: String,
    config: McpServerConfig,
    credentials: McpCredentials,
    policy: DestinationPolicy,
    state: Mutex<Option<LiveConnection>>,
    lifecycle: Mutex<()>,
    failure: Mutex<Option<ApiError>>,
    restarts: AtomicU32,
}

impl RemoteServer {
    fn new(id: String, config: McpServerConfig, credentials: McpCredentials) -> Self {
        let policy = DestinationPolicy::new(&config);
        Self {
            id,
            config,
            credentials,
            policy,
            state: Mutex::new(None),
            lifecycle: Mutex::new(()),
            failure: Mutex::new(None),
            restarts: AtomicU32::new(0),
        }
    }

    fn grant(&self) -> String {
        self.config.grant_for(&self.id)
    }

    fn transport(&self) -> &'static str {
        if self.config.uses_sse() {
            "sse"
        } else {
            "http"
        }
    }

    async fn health(&self, id: &str, granted: bool) -> McpServerHealth {
        let live = {
            let state = self.state.lock().await;
            state
                .as_ref()
                .map(|connection| (connection.toolset.clone(), connection.connected_at))
        };
        let state = if !self.config.enabled {
            McpState::Disabled
        } else if let Some((toolset, connected_at)) = live {
            let deadline = std::time::Duration::from_millis(self.config.timeout_ms.clamp(1, 1_000));
            let alive = !toolset.is_closed().await
                && matches!(timeout(deadline, toolset.list_prompts()).await, Ok(Ok(_)));
            if alive {
                let mut health = health::record(
                    id,
                    &self.config,
                    McpState::Connected,
                    self.restarts.load(Ordering::SeqCst),
                    Some(connected_at),
                    None,
                );
                health.transport = self.transport().into();
                return health;
            }
            {
                let mut current = self.state.lock().await;
                if let Some(dead) = current.take() {
                    dead.cancel.cancel();
                }
            }
            *self.failure.lock().await = Some(ApiError::new(
                "mcp.connection_closed",
                "MCP server is no longer responding",
            ));
            McpState::Unavailable
        } else {
            McpState::Unavailable
        };
        let failure = self
            .failure
            .lock()
            .await
            .as_ref()
            .map(|error| common::McpFailureView {
                code: error.code.clone(),
                message: common::redact(&error.message),
            });
        let mut health = health::record(
            id,
            &self.config,
            if granted {
                state
            } else {
                McpState::Unavailable
            },
            self.restarts.load(Ordering::SeqCst),
            None,
            failure,
        );
        health.transport = self.transport().into();
        health
    }

    async fn preflight(&self) -> Result<(), ApiError> {
        let _grant = self.authorize()?;
        self.policy.vet(self.config.endpoint()).await?;
        Ok(())
    }

    fn authorize(&self) -> Result<CredentialGrant, ApiError> {
        if !self.config.enabled {
            return Err(ApiError::new(
                "mcp.connection_disabled",
                "MCP connection is disabled for this instance",
            ));
        }
        let reference = self.config.credential.as_deref().ok_or_else(|| {
            ApiError::new(
                "mcp.credential_missing",
                "MCP connection has no owned credential reference",
            )
        })?;
        let grant = self.credentials.resolve(reference)?;
        if grant.audience != self.config.audience() {
            return Err(ApiError::new(
                "mcp.credential_audience_denied",
                "MCP credential audience does not match this connection",
            ));
        }
        for scope in &self.config.required_scopes {
            if !grant.scopes.iter().any(|bound| bound == scope) {
                return Err(ApiError::new(
                    "mcp.credential_scope_denied",
                    "MCP credential does not carry a required scope",
                ));
            }
        }
        Ok(grant)
    }

    /// Ensure a live client and return it with the configuration snapshot.
    async fn live(&self) -> Result<(Arc<McpToolset<AdkClientHandler>>, McpServerConfig), ApiError> {
        let grant = self.authorize()?;
        let mut state = self.state.lock().await;
        self.ensure_connected(&mut state, &grant).await?;
        let connection = state.as_ref().ok_or_else(|| {
            ApiError::new("mcp.transport_unavailable", "MCP connection is not live")
        })?;
        Ok((connection.toolset.clone(), self.config.clone()))
    }

    /// Restart the connection with the current credential generation.
    async fn restart(&self) -> Result<(), ApiError> {
        let _lifecycle = self.lifecycle.lock().await;
        if !self.config.enabled {
            return Err(ApiError::new(
                "mcp.connection_disabled",
                "MCP connection is disabled for this instance",
            ));
        }
        let grant = self.authorize()?;
        {
            let mut state = self.state.lock().await;
            if let Some(old) = state.take() {
                old.cancel.cancel();
            }
        }
        let destination = self.policy.vet(self.config.endpoint()).await?;
        match self.connect(destination, &grant).await {
            Ok(connection) => {
                let mut state = self.state.lock().await;
                *state = Some(connection);
                self.restarts.fetch_add(1, Ordering::SeqCst);
                *self.failure.lock().await = None;
                Ok(())
            }
            Err(error) => {
                *self.failure.lock().await = Some(error.clone());
                Err(error)
            }
        }
    }

    async fn discover(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<ToolSnapshot>, ApiError> {
        let grant = self.authorize()?;
        let mut state = self.state.lock().await;
        self.ensure_connected(&mut state, &grant).await?;
        let first = {
            let connection = state.as_ref().ok_or_else(|| {
                ApiError::new("mcp.transport_unavailable", "MCP connection is not live")
            })?;
            connection.toolset.tools(ctx.clone()).await
        };
        let tools = match first {
            Ok(tools) => tools,
            Err(error) if is_reconnectable(&error.to_string()) => {
                // Discovery is not an effect. Reconnect once and re-pin the
                // destination; a tool call is never replayed here.
                if let Some(old) = state.take() {
                    old.cancel.cancel();
                }
                self.ensure_connected(&mut state, &grant).await?;
                let connection = state.as_ref().ok_or_else(|| {
                    ApiError::new("mcp.transport_unavailable", "MCP connection is not live")
                })?;
                connection
                    .toolset
                    .tools(ctx)
                    .await
                    .map_err(|error| classify_transport_error(&error.to_string(), false))?
            }
            Err(error) => return Err(classify_transport_error(&error.to_string(), false)),
        };
        let mut snapshots = Vec::new();
        let allowed: BTreeSet<&str> = self
            .config
            .allowed_tools
            .iter()
            .map(String::as_str)
            .collect();
        let mut pinned = BTreeMap::new();
        for tool in tools {
            let name = tool.name().to_string();
            if !allowed.contains(name.as_str()) {
                continue;
            }
            let snapshot = ToolSnapshot {
                name: name.clone(),
                description: tool.description().to_string(),
                parameters: tool.parameters_schema(),
                response: tool.response_schema(),
                read_only: tool.is_read_only(),
                long_running: tool.is_long_running(),
            };
            pinned.insert(name, (snapshot.clone(), tool));
            snapshots.push(snapshot);
        }
        if let Some(connection) = state.as_mut() {
            connection.tools = pinned;
        }
        Ok(snapshots)
    }

    async fn call(
        &self,
        registry: &RemoteMcpRegistry,
        name: &str,
        ctx: Arc<dyn adk_rust::ToolContext>,
        args: Value,
        read_only: bool,
    ) -> AdkResult<Value> {
        let session = ctx.session_id().to_string();
        if registry.has_uncertainty(&session).await {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Unavailable,
                "mcp.outcome_unknown",
                "mcp.outcome_unknown: an earlier remote MCP write has an unknown outcome; reconcile it before another call",
            ));
        }
        let grant = match self.authorize() {
            Ok(grant) => grant,
            Err(error) => {
                let code = error.code.clone();
                return Err(adk_error(category_for_code(&code), &code, &error.message));
            }
        };
        let tool = {
            let mut state = self.state.lock().await;
            if let Err(error) = self.ensure_connected(&mut state, &grant).await {
                return Err(adk_error(
                    category_for_code(&error.code),
                    &error.code,
                    &error.message,
                ));
            }
            let Some(connection) = state.as_ref() else {
                return Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unavailable,
                    "mcp.transport_unavailable",
                    "MCP connection is not live",
                ));
            };
            let Some((_, tool)) = connection.tools.get(name) else {
                return Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Forbidden,
                    "mcp.tool_denied",
                    format!("tool '{name}' is outside the pinned MCP exposure"),
                ));
            };
            tool.clone()
        };
        let deadline = std::time::Duration::from_millis(self.config.call_timeout_ms.max(1));
        enum Outcome {
            Done(AdkResult<Value>),
            TimedOut,
            Cancelled,
        }
        let outcome = tokio::select! {
            biased;
            _ = wait_for_cancel(ctx.clone()) => Outcome::Cancelled,
            result = timeout(deadline, tool.execute(ctx, args)) => match result {
                Err(_) => Outcome::TimedOut,
                Ok(result) => Outcome::Done(result),
            },
        };
        let code = match outcome {
            Outcome::Done(Ok(value)) => {
                *self.failure.lock().await = None;
                return Ok(surface::bound_value(
                    value,
                    self.config.max_output_bytes.max(1),
                ));
            }
            Outcome::Done(Err(error)) => {
                let lower = error.to_string().to_ascii_lowercase();
                if is_ambiguous_transport(&lower) {
                    "mcp.outcome_unknown"
                } else {
                    classify_error_code(&error.to_string())
                }
            }
            Outcome::TimedOut if read_only => "mcp.timeout",
            Outcome::TimedOut => "mcp.outcome_unknown",
            Outcome::Cancelled if read_only => "mcp.cancelled",
            Outcome::Cancelled => "mcp.outcome_unknown",
        };
        if code == "mcp.outcome_unknown" {
            // The call may have committed remotely. Do not retry and do not
            // report a definite failure.
            registry
                .mark_uncertain(
                    &session,
                    "remote MCP write outcome is unknown after transport loss or cancellation",
                )
                .await;
        }
        if code != "mcp.cancelled" && code != "mcp.timeout" {
            *self.failure.lock().await = Some(ApiError::new(code, safe_message(code)));
        }
        Err(AdkError::new(
            ErrorComponent::Tool,
            category_for_code(code),
            static_code(code),
            format!("{code}: {}", safe_message(code)),
        ))
    }

    async fn ensure_connected(
        &self,
        state: &mut Option<LiveConnection>,
        grant: &CredentialGrant,
    ) -> Result<(), ApiError> {
        let fingerprint = grant.fingerprint();
        let stale = state.as_ref().is_none_or(|connection| {
            connection.fingerprint != fingerprint || connection.generation != grant.generation
        });
        if stale {
            if let Some(old) = state.take() {
                old.cancel.cancel();
            }
            let destination = self.policy.vet(self.config.endpoint()).await?;
            match self.connect(destination, grant).await {
                Ok(connection) => {
                    *state = Some(connection);
                    *self.failure.lock().await = None;
                }
                Err(error) => {
                    *self.failure.lock().await = Some(error.clone());
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    async fn connect(
        &self,
        destination: VettedDestination,
        grant: &CredentialGrant,
    ) -> Result<LiveConnection, ApiError> {
        let client = self.policy.client(&destination)?;
        let handler = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler));
        let service = if self.config.uses_sse() {
            let (transport, _) = crate::mcp::sse::connect(
                self.config.endpoint(),
                &destination,
                client,
                Some(grant.value.expose().to_string()),
                self.policy_timeout(),
                self.config.max_output_bytes.max(1024),
            )
            .await?;
            timeout(
                self.policy_timeout(),
                handler.serve_with_lifecycle(transport, ClientLifecycleMode::Initialize),
            )
            .await
            .map_err(|_| {
                ApiError::new(
                    "mcp.transport_unavailable",
                    "legacy MCP SSE handshake exceeded its deadline",
                )
            })?
            .map_err(|error| classify_transport_error(&error.to_string(), true))?
        } else {
            let config = StreamableHttpClientTransportConfig::with_uri(self.config.endpoint())
                // A session re-init retries the in-flight request. Refuse it so
                // an interrupted write cannot be repeated by the transport.
                .reinit_on_expired_session(false)
                .auth_header(grant.value.expose().to_string());
            let transport = StreamableHttpClientTransport::with_client(client, config);
            timeout(self.policy_timeout(), handler.serve(transport))
                .await
                .map_err(|_| {
                    ApiError::new(
                        "mcp.transport_unavailable",
                        "MCP server handshake exceeded its deadline",
                    )
                })?
                .map_err(|error| classify_transport_error(&error.to_string(), true))?
        };
        let toolset = Arc::new(
            McpToolset::new(service)
                .with_name(self.id.clone())
                .with_schema_limits(
                    McpSchemaLimits::default().with_max_bytes(self.config.max_schema_bytes),
                ),
        );
        let cancel = toolset.cancellation_token().await;
        Ok(LiveConnection {
            toolset,
            cancel,
            fingerprint: grant.fingerprint(),
            generation: grant.generation,
            connected_at: common::now_ms(),
            tools: BTreeMap::new(),
        })
    }

    fn policy_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.config.timeout_ms.max(1))
    }

    async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        if let Some(connection) = state.take() {
            connection.cancel.cancel();
        }
    }
}

/// Stop waiting when the owning invocation is cancelled.
async fn wait_for_cancel(ctx: Arc<dyn adk_rust::ToolContext>) {
    loop {
        if ctx.is_cancelled() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

fn static_code(code: &str) -> &'static str {
    match code {
        "mcp.credential_missing" => "mcp.credential_missing",
        "mcp.credential_expired" => "mcp.credential_expired",
        "mcp.credential_revoked" => "mcp.credential_revoked",
        "mcp.credential_audience_denied" => "mcp.credential_audience_denied",
        "mcp.credential_scope_denied" => "mcp.credential_scope_denied",
        "mcp.endpoint_denied" => "mcp.endpoint_denied",
        "mcp.tls_required" => "mcp.tls_required",
        "mcp.redirect_denied" => "mcp.redirect_denied",
        "mcp.dns_unresolved" => "mcp.dns_unresolved",
        "mcp.transport_unavailable" => "mcp.transport_unavailable",
        "mcp.tool_denied" => "mcp.tool_denied",
        "mcp.tool_failed" => "mcp.tool_failed",
        "mcp.outcome_unknown" => "mcp.outcome_unknown",
        "mcp.cancelled" => "mcp.cancelled",
        "mcp.timeout" => "mcp.timeout",
        _ => "mcp.error",
    }
}

fn category_for_code(code: &str) -> ErrorCategory {
    if code.contains("credential") || code.contains("revoked") {
        ErrorCategory::Unauthorized
    } else if code.contains("denied") || code.contains("grant") {
        ErrorCategory::Forbidden
    } else if code.contains("unavailable") || code.contains("transport") || code.contains("timeout")
    {
        ErrorCategory::Unavailable
    } else {
        ErrorCategory::Internal
    }
}

fn adk_error(category: ErrorCategory, code: &str, message: &str) -> AdkError {
    // `AdkError` takes a static code. Keep the stable Worker code in the message
    // as well so task evidence can recover it without guessing.
    AdkError::new(
        ErrorComponent::Tool,
        category,
        static_code(code),
        format!("{code}: {}", safe_detail(message)),
    )
}

/// A fixed, secret-free diagnostic for a stable MCP code.
fn safe_message(code: &str) -> &'static str {
    match code {
        "mcp.redirect_denied" => "remote MCP server attempted a redirect",
        "mcp.outcome_unknown" => "remote MCP write outcome is unknown after transport loss",
        "mcp.credential_expired" => "the MCP credential has expired",
        "mcp.credential_revoked" => "the MCP credential has been revoked",
        "mcp.credential_missing" => "the MCP credential value is not available",
        "mcp.credential_audience_denied" => {
            "MCP credential audience does not match this connection"
        }
        "mcp.credential_scope_denied" => "MCP credential does not carry a required scope",
        "mcp.tool_denied" => "the tool is outside the pinned MCP exposure",
        "mcp.cancelled" => "the MCP tool call was cancelled",
        "mcp.timeout" => "the MCP tool call exceeded its deadline",
        _ => "remote MCP tool call failed",
    }
}

fn safe_detail(message: &str) -> String {
    common::redact(message)
        .chars()
        .take(MAX_ERROR_CHARS)
        .collect()
}

/// Classify connection/handshake failures without exposing response bodies.
fn classify_transport_error(message: &str, connecting: bool) -> ApiError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("mcp.redirect_denied")
        || lower.contains("following redirect")
        || lower.contains("redirect")
    {
        return ApiError::new(
            "mcp.redirect_denied",
            "remote MCP server attempted a redirect",
        );
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return ApiError::new(
            "mcp.transport_unavailable",
            "remote MCP server did not respond in time",
        );
    }
    if connecting {
        ApiError::new(
            "mcp.transport_unavailable",
            "remote MCP server connection failed",
        )
    } else {
        ApiError::new(
            "mcp.transport_unavailable",
            "remote MCP server connection was lost",
        )
    }
}

fn classify_error_code(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("mcp.redirect_denied")
        || lower.contains("following redirect")
        || lower.contains("redirect")
    {
        return "mcp.redirect_denied";
    }
    if lower.contains("mcp.credential_expired") {
        return "mcp.credential_expired";
    }
    if lower.contains("mcp.credential_revoked") {
        return "mcp.credential_revoked";
    }
    if lower.contains("mcp.tool_denied") {
        return "mcp.tool_denied";
    }
    if is_ambiguous_transport(&lower) {
        return "mcp.outcome_unknown";
    }
    "mcp.tool_failed"
}

/// Ambiguity markers intentional: these can occur after request transmission.
fn is_ambiguous_transport(lower: &str) -> bool {
    [
        "eof",
        "connection closed",
        "broken pipe",
        "connection reset",
        "transport error",
        "error decoding response body",
        "error reading a body",
        "incomplete message",
        "unexpected end",
        "session expired",
        "stream ended",
        "timed out",
        "timeout",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Transport symptoms that make a fresh discovery safe to retry.
fn is_reconnectable(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "eof",
        "connection closed",
        "broken pipe",
        "connection reset",
        "transport error",
        "session expired",
        "stream ended",
        "timed out",
        "timeout",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Extract a stable `mcp.*` code from a native tool-response error string.
pub fn error_code(message: &str) -> Option<String> {
    let start = message.find("mcp.")?;
    let rest = &message[start..];
    let end = rest
        .char_indices()
        .find(|(_, character)| {
            !(character.is_ascii_alphanumeric() || *character == '_' || *character == '.')
        })
        .map(|(index, _)| index)
        .unwrap_or(rest.len());
    let code = &rest[..end];
    if code.len() > "mcp.".len() {
        Some(code.to_string())
    } else {
        None
    }
}
