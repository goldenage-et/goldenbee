//! Operator configuration for instance-owned MCP connections.
//!
//! One connection table describes both local stdio children (ticket 25) and
//! authorized remote Streamable HTTP endpoints (ticket 26). A definition with
//! `endpoint` is remote; a definition with `command` is local. The schema
//! contains references and policy only. Raw credential values are always
//! resolved from an external source at call time and are never stored in this
//! structure, a profile revision, or an exported configuration.

use common::ApiError;
use reqwest13::Url;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// MCP configuration for one Worker Instance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// Server definitions, keyed by stable connection id. Local and remote
    /// connections share the table; `endpoint` selects the remote transport.
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerConfig>,
    /// Protected credential references and their required audience/scope.
    #[serde(default)]
    pub credentials: Vec<McpCredentialConfig>,
}

/// One MCP connection: local stdio or authorized remote Streamable HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    /// Transport. Local connections default to `stdio`; remote connections
    /// select Streamable HTTP by defining `endpoint`.
    #[serde(default = "default_transport")]
    pub transport: String,
    /// Absolute local executable path. Shell interpretation is never used.
    #[serde(default)]
    pub command: Option<String>,
    /// Exact arguments passed to the local executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Explicit local child environment. Values beginning with `env:` resolve
    /// from the parent process; raw secret values are rejected at load time.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Remote Streamable HTTP endpoint. Presence selects the remote transport.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Whether the connection is available to profile revisions.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Installed tool ceiling. Discovery may return only these names.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Allowed local tools that additionally require a per-tool profile grant.
    #[serde(default)]
    pub protected_tools: Vec<String>,
    /// Deadline for local process start and MCP initialization.
    #[serde(default = "default_startup_timeout_ms")]
    pub startup_timeout_ms: u64,
    /// Deadline for one local tool invocation.
    #[serde(default = "default_call_timeout_ms")]
    pub call_timeout_ms: u64,
    /// Largest serialized local tool result returned to the model.
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Credential reference owned by this instance. Never a secret value.
    #[serde(default)]
    pub credential: Option<String>,
    /// Audience the remote credential must carry. Defaults to the endpoint.
    #[serde(default)]
    pub audience: Option<String>,
    /// Scopes the remote credential must carry for this connection.
    #[serde(default)]
    pub required_scopes: Vec<String>,
    /// Authority required in both instance and profile grants.
    #[serde(default)]
    pub grant: Option<String>,
    /// Exact DNS names or IP literals permitted for this remote endpoint.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Permit loopback remote destinations (development and deterministic tests).
    #[serde(default)]
    pub allow_loopback: bool,
    /// Permit private, link-local, or otherwise non-public remote destinations.
    #[serde(default)]
    pub allow_private_network: bool,
    /// Permit plain HTTP to an explicitly allowed loopback remote endpoint.
    #[serde(default)]
    pub allow_insecure_http: bool,
    /// Per-request and handshake timeout for remote connections.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Largest accepted remote tool schema, in bytes.
    #[serde(default = "default_schema_bytes")]
    pub max_schema_bytes: usize,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            transport: default_transport(),
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            endpoint: None,
            enabled: true,
            allowed_tools: Vec::new(),
            protected_tools: Vec::new(),
            startup_timeout_ms: default_startup_timeout_ms(),
            call_timeout_ms: default_call_timeout_ms(),
            max_output_bytes: default_max_output_bytes(),
            credential: None,
            audience: None,
            required_scopes: Vec::new(),
            grant: None,
            allowed_hosts: Vec::new(),
            allow_loopback: false,
            allow_private_network: false,
            allow_insecure_http: false,
            timeout_ms: default_timeout_ms(),
            max_schema_bytes: default_schema_bytes(),
        }
    }
}

impl McpServerConfig {
    /// Whether this definition selects the remote Streamable HTTP transport.
    pub fn is_remote(&self) -> bool {
        self.endpoint.is_some()
    }

    /// Local executable path, empty when this is a remote definition.
    pub fn command(&self) -> &str {
        self.command.as_deref().unwrap_or_default()
    }

    /// Remote endpoint, empty when this is a local definition.
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_deref().unwrap_or_default()
    }

    /// Immutable local policy projection used by revision validation and
    /// execution.
    pub fn policy(&self) -> McpServerPolicy {
        McpServerPolicy {
            enabled: self.enabled,
            credential: self.credential.clone(),
            protected_tools: self.protected_tools.clone(),
        }
    }

    /// Effective remote audience binding.
    pub fn audience(&self) -> &str {
        self.audience.as_deref().unwrap_or_else(|| self.endpoint())
    }

    /// Effective authority required by profiles that enable a remote server.
    pub fn grant_for(&self, id: &str) -> String {
        self.grant
            .clone()
            .unwrap_or_else(|| format!("mcp:remote:{id}"))
    }

    /// Stable identity for reload comparison. Any change restarts the child.
    pub fn signature(&self) -> String {
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            self.transport,
            self.command,
            self.args,
            self.env,
            self.endpoint,
            self.enabled,
            self.allowed_tools,
            self.protected_tools,
            self.startup_timeout_ms,
            self.call_timeout_ms,
            self.max_output_bytes,
            self.credential,
            self.audience,
            self.required_scopes,
            self.grant,
            self.allowed_hosts,
            self.allow_loopback,
            self.allow_private_network,
            self.allow_insecure_http,
            self.timeout_ms,
            self.max_schema_bytes,
        )
    }
}

/// Policy visible to profile validation without exposing command details.
#[derive(Debug, Clone)]
pub struct McpServerPolicy {
    /// Whether the operator allows this connection for future invocations.
    pub enabled: bool,
    /// Credential reference that a profile must also name.
    pub credential: Option<String>,
    /// Tools that require a per-tool profile grant.
    pub protected_tools: Vec<String>,
}

/// One owned credential binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCredentialConfig {
    /// Opaque reference profiles and servers use. Never a value.
    pub reference: String,
    /// External source, currently `env:NAME`.
    pub source: String,
    /// Required audience.
    pub audience: String,
    /// Scopes the resolved credential is allowed to use.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Absolute expiry in Unix milliseconds. Zero means no configured expiry.
    #[serde(default)]
    pub expires_at: i64,
    /// Operator revocation. Runtime revocation is tracked separately.
    #[serde(default)]
    pub revoked: bool,
}

impl McpConfig {
    /// Validate credentials and every remote server definition. Local server
    /// definitions are validated by the Worker configuration loader, which
    /// also checks executable paths and child environment values.
    pub fn validate(&self) -> Result<(), ApiError> {
        let mut references = BTreeSet::new();
        for binding in &self.credentials {
            validate_reference(&binding.reference)?;
            if !references.insert(binding.reference.clone()) {
                return Err(ApiError::new(
                    "config.invalid",
                    format!(
                        "MCP credential reference '{}' is declared more than once",
                        binding.reference
                    ),
                ));
            }
            let Some(name) = binding.source.strip_prefix("env:") else {
                return Err(ApiError::new(
                    "config.invalid",
                    "MCP credential sources must use env:NAME references",
                ));
            };
            if name.trim().is_empty()
                || name
                    .chars()
                    .any(|character| !(character.is_ascii_alphanumeric() || character == '_'))
            {
                return Err(ApiError::new(
                    "config.invalid",
                    "MCP credential env source has an invalid variable name",
                ));
            }
            if binding.audience.trim().is_empty() {
                return Err(ApiError::new(
                    "config.invalid",
                    "MCP credential audience must not be empty",
                ));
            }
        }

        for (id, server) in &self.servers {
            validate_id(id)?;
            if !server.is_remote() {
                continue;
            }
            if server.command.is_some() {
                return Err(ApiError::new(
                    "config.invalid",
                    format!("MCP server '{id}' cannot define both command and endpoint"),
                ));
            }
            validate_endpoint(server.endpoint())?;
            if server.allowed_tools.is_empty() {
                return Err(ApiError::new(
                    "config.invalid",
                    format!("MCP server '{id}' must declare at least one allowed tool"),
                ));
            }
            let mut tools = BTreeSet::new();
            for tool in &server.allowed_tools {
                if tool.trim().is_empty() || tool.len() > 128 {
                    return Err(ApiError::new(
                        "config.invalid",
                        format!("MCP server '{id}' has an invalid allowed tool name"),
                    ));
                }
                if !tools.insert(tool) {
                    return Err(ApiError::new(
                        "config.invalid",
                        format!("MCP server '{id}' declares tool '{tool}' more than once"),
                    ));
                }
            }
            if server.allowed_hosts.is_empty() {
                return Err(ApiError::new(
                    "config.invalid",
                    format!("MCP server '{id}' must declare an allowed_hosts entry"),
                ));
            }
            let host = endpoint_host(server.endpoint())?;
            if let Some(grant) = &server.grant {
                if grant.trim().is_empty() || grant == "*" || grant.len() > 128 {
                    return Err(ApiError::new(
                        "config.invalid",
                        format!("MCP server '{id}' has an invalid grant"),
                    ));
                }
            }
            if !server
                .allowed_hosts
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(&host))
            {
                return Err(ApiError::new(
                    "config.invalid",
                    format!("MCP server '{id}' endpoint host is not in allowed_hosts"),
                ));
            }
            if server.timeout_ms == 0 || server.timeout_ms > 300_000 {
                return Err(ApiError::new(
                    "config.invalid",
                    format!("MCP server '{id}' timeout_ms must be 1..=300000"),
                ));
            }
            if server.max_schema_bytes < 1024 {
                return Err(ApiError::new(
                    "config.invalid",
                    format!("MCP server '{id}' max_schema_bytes is too small"),
                ));
            }
            if let Some(reference) = &server.credential {
                validate_reference(reference)?;
                let binding = self
                    .credentials
                    .iter()
                    .find(|binding| &binding.reference == reference)
                    .ok_or_else(|| {
                        ApiError::new(
                            "config.invalid",
                            format!("MCP server '{id}' references an undeclared credential"),
                        )
                    })?;
                if binding.audience != server.audience() {
                    return Err(ApiError::new(
                        "config.invalid",
                        format!("MCP server '{id}' audience does not match its credential binding"),
                    ));
                }
                for scope in &server.required_scopes {
                    if !binding.scopes.iter().any(|bound| bound == scope) {
                        return Err(ApiError::new(
                            "config.invalid",
                            format!(
                                "MCP server '{id}' requires a scope its credential does not bind"
                            ),
                        ));
                    }
                }
            } else {
                return Err(ApiError::new(
                    "config.invalid",
                    format!("MCP server '{id}' must declare an owned credential reference"),
                ));
            }
        }
        Ok(())
    }

    /// Configuration projection for remote connections only. Local stdio
    /// definitions can change on reload; remote endpoints and credentials
    /// require a restart.
    pub fn remote_only(&self) -> Self {
        Self {
            servers: self
                .servers
                .iter()
                .filter(|(_, server)| server.is_remote())
                .map(|(id, server)| (id.clone(), server.clone()))
                .collect(),
            credentials: self.credentials.clone(),
        }
    }

    /// Look up a credential binding.
    pub fn credential(&self, reference: &str) -> Option<&McpCredentialConfig> {
        self.credentials
            .iter()
            .find(|binding| binding.reference == reference)
    }
}

fn validate_reference(reference: &str) -> Result<(), ApiError> {
    let looks_like_value = reference.starts_with("sk-")
        || reference.starts_with("Bearer ")
        || reference.starts_with("AKIA")
        || reference.len() > 256;
    if looks_like_value
        || !(reference.starts_with("secret://")
            || reference.starts_with("env:")
            || reference.starts_with("vault:"))
    {
        return Err(ApiError::new(
            "config.invalid",
            "MCP credentials must use secret://, env:, or vault: references",
        ));
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), ApiError> {
    if id.trim().is_empty()
        || id.len() > 64
        || id.chars().any(|character| {
            !(character.is_ascii_alphanumeric() || character == '-' || character == '_')
        })
    {
        return Err(ApiError::new(
            "config.invalid",
            format!("MCP server id '{id}' must be a short identifier"),
        ));
    }
    Ok(())
}

fn validate_endpoint(endpoint: &str) -> Result<(), ApiError> {
    let url = Url::parse(endpoint)
        .map_err(|_| ApiError::new("config.invalid", "MCP endpoint is not a valid URL"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(ApiError::new(
            "config.invalid",
            "MCP endpoint must use http or https",
        ));
    }
    if url.host_str().is_none() {
        return Err(ApiError::new("config.invalid", "MCP endpoint has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::new(
            "config.invalid",
            "MCP endpoint must not embed credentials",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ApiError::new(
            "config.invalid",
            "MCP endpoint must not carry a query or fragment",
        ));
    }
    Ok(())
}

fn endpoint_host(endpoint: &str) -> Result<String, ApiError> {
    Url::parse(endpoint)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .ok_or_else(|| ApiError::new("config.invalid", "MCP endpoint has no host"))
}

fn default_transport() -> String {
    "stdio".into()
}

fn default_true() -> bool {
    true
}

fn default_startup_timeout_ms() -> u64 {
    10_000
}

fn default_call_timeout_ms() -> u64 {
    30_000
}

fn default_max_output_bytes() -> usize {
    256 * 1024
}

fn default_timeout_ms() -> u64 {
    10_000
}

fn default_schema_bytes() -> usize {
    64 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential_binding() -> McpCredentialConfig {
        McpCredentialConfig {
            reference: "secret://mcp/tools".into(),
            source: "env:GOLDENBEE_TEST_MCP_TOKEN".into(),
            audience: "https://mcp.example.test/mcp".into(),
            scopes: vec!["mcp:call".into()],
            expires_at: 0,
            revoked: false,
        }
    }

    fn server_config() -> McpServerConfig {
        McpServerConfig {
            endpoint: Some("https://mcp.example.test/mcp".into()),
            enabled: true,
            allowed_tools: vec!["read_record".into()],
            credential: Some("secret://mcp/tools".into()),
            required_scopes: vec!["mcp:call".into()],
            allowed_hosts: vec!["mcp.example.test".into()],
            timeout_ms: 1_000,
            max_schema_bytes: 2_048,
            ..McpServerConfig::default()
        }
    }

    #[test]
    fn configuration_binds_credential_audience_and_scope() {
        let mut config = McpConfig::default();
        config.credentials.push(credential_binding());
        config.servers.insert("tools".into(), server_config());
        config.validate().expect("valid configuration");
        assert_eq!(
            config.servers["tools"].grant_for("tools"),
            "mcp:remote:tools"
        );
    }

    #[test]
    fn missing_credential_is_rejected() {
        let mut config = McpConfig::default();
        config.credentials.push(credential_binding());
        let mut server = server_config();
        server.credential = None;
        config.servers.insert("tools".into(), server);
        let error = config.validate().expect_err("credential is required");
        assert_eq!(error.code, "config.invalid");
    }

    #[test]
    fn audience_or_scope_mismatch_is_rejected() {
        let mut config = McpConfig::default();
        let mut credential = credential_binding();
        credential.audience = "https://other.example.test".into();
        config.credentials.push(credential);
        config.servers.insert("tools".into(), server_config());
        assert_eq!(
            config.validate().expect_err("audience mismatch").code,
            "config.invalid"
        );

        let mut config = McpConfig::default();
        let mut credential = credential_binding();
        credential.scopes.clear();
        config.credentials.push(credential);
        config.servers.insert("tools".into(), server_config());
        assert_eq!(
            config.validate().expect_err("scope mismatch").code,
            "config.invalid"
        );
    }

    #[test]
    fn endpoint_must_be_in_allowed_hosts_and_not_embed_credentials() {
        let mut config = McpConfig::default();
        config.credentials.push(credential_binding());
        let mut server = server_config();
        server.allowed_hosts = vec!["other.example.test".into()];
        config.servers.insert("tools".into(), server);
        assert_eq!(
            config.validate().expect_err("host allowlist mismatch").code,
            "config.invalid"
        );

        let mut config = McpConfig::default();
        let mut credential = credential_binding();
        credential.audience = "https://user:pass@mcp.example.test/mcp".into();
        config.credentials.clear();
        config.credentials.push(credential);
        let mut server = server_config();
        server.endpoint = Some("https://user:pass@mcp.example.test/mcp".into());
        config.servers.insert("tools".into(), server);
        assert_eq!(
            config.validate().expect_err("embedded credential").code,
            "config.invalid"
        );
    }
}
