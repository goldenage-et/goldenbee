//! Standalone operator configuration: listener, caller tokens, and core config.

use common::ApiError;
use gateway_core::{Caller, CoreConfig};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

fn default_listen() -> String {
    "127.0.0.1:8080".to_string()
}

/// Bounded duplex socket settings for the standalone adapter.
#[derive(Debug, Clone, Deserialize)]
pub struct SocketConfig {
    /// Inbound frame ceiling in bytes.
    #[serde(default = "default_socket_max_frame")]
    pub max_frame_bytes: usize,
    /// Bounded outgoing subscriber queue per connection.
    #[serde(default = "default_socket_queue")]
    pub subscriber_queue: usize,
    /// Maximum concurrent task subscriptions on one connection.
    #[serde(default = "default_socket_subscriptions")]
    pub max_subscriptions: usize,
    /// Idle timeout before a silent connection is closed.
    #[serde(default = "default_socket_idle")]
    pub idle_timeout_ms: u64,
    /// Server heartbeat interval.
    #[serde(default = "default_socket_heartbeat")]
    pub heartbeat_ms: u64,
    /// Bound on one frame send before the connection is closed.
    #[serde(default = "default_socket_send_timeout")]
    pub send_timeout_ms: u64,
    /// Short-lived one-use ticket lifetime.
    #[serde(default = "default_socket_ticket_ttl")]
    pub ticket_ttl_ms: u64,
}

fn default_socket_max_frame() -> usize {
    common::SOCKET_MAX_FRAME_BYTES
}

fn default_socket_queue() -> usize {
    common::SOCKET_SUBSCRIBER_QUEUE
}

fn default_socket_subscriptions() -> usize {
    common::SOCKET_MAX_SUBSCRIPTIONS
}

fn default_socket_idle() -> u64 {
    common::SOCKET_IDLE_TIMEOUT_MS
}

fn default_socket_heartbeat() -> u64 {
    common::SOCKET_HEARTBEAT_MS
}

fn default_socket_send_timeout() -> u64 {
    common::SOCKET_SEND_TIMEOUT_MS
}

fn default_socket_ticket_ttl() -> u64 {
    common::SOCKET_TICKET_TTL_MS
}

impl Default for SocketConfig {
    fn default() -> Self {
        Self {
            max_frame_bytes: default_socket_max_frame(),
            subscriber_queue: default_socket_queue(),
            max_subscriptions: default_socket_subscriptions(),
            idle_timeout_ms: default_socket_idle(),
            heartbeat_ms: default_socket_heartbeat(),
            send_timeout_ms: default_socket_send_timeout(),
            ticket_ttl_ms: default_socket_ticket_ttl(),
        }
    }
}

impl SocketConfig {
    fn validate(&self) -> Result<(), ApiError> {
        if !(1024..=1024 * 1024).contains(&self.max_frame_bytes) {
            return Err(ApiError::new(
                "config.invalid",
                "socket.max_frame_bytes must be between 1024 and 1048576",
            ));
        }
        if !(1..=4096).contains(&self.subscriber_queue) {
            return Err(ApiError::new(
                "config.invalid",
                "socket.subscriber_queue must be between 1 and 4096",
            ));
        }
        if !(1..=256).contains(&self.max_subscriptions) {
            return Err(ApiError::new(
                "config.invalid",
                "socket.max_subscriptions must be between 1 and 256",
            ));
        }
        if !(100..=600_000).contains(&self.idle_timeout_ms) {
            return Err(ApiError::new(
                "config.invalid",
                "socket.idle_timeout_ms must be between 100 and 600000",
            ));
        }
        if !(100..=600_000).contains(&self.heartbeat_ms) {
            return Err(ApiError::new(
                "config.invalid",
                "socket.heartbeat_ms must be between 100 and 600000",
            ));
        }
        if self.heartbeat_ms >= self.idle_timeout_ms {
            return Err(ApiError::new(
                "config.invalid",
                "socket.heartbeat_ms must be shorter than socket.idle_timeout_ms",
            ));
        }
        if !(10..=60_000).contains(&self.send_timeout_ms) {
            return Err(ApiError::new(
                "config.invalid",
                "socket.send_timeout_ms must be between 10 and 60000",
            ));
        }
        if !(100..=300_000).contains(&self.ticket_ttl_ms) {
            return Err(ApiError::new(
                "config.invalid",
                "socket.ticket_ttl_ms must be between 100 and 300000",
            ));
        }
        Ok(())
    }
}

/// `gb-gateway serve` operator configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Listener address. Loopback by default; the CLI may override it.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Caller token bindings. Tokens are secrets and are never logged.
    #[serde(default)]
    pub callers: Vec<CallerConfig>,
    /// Bounded authenticated duplex socket settings.
    #[serde(default)]
    pub socket: SocketConfig,
    /// Reusable control-plane configuration.
    #[serde(flatten)]
    pub core: CoreConfig,
}

/// One authenticated caller. The token is the bearer credential; the principal
/// carries the namespace and scopes the Gateway enforces.
#[derive(Debug, Clone, Deserialize)]
pub struct CallerConfig {
    /// Bearer token presented by the caller.
    pub token: String,
    /// Stable principal identifier.
    pub id: String,
    /// Namespace the caller may act in.
    pub namespace: String,
    /// Granted management scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Optional explicit Worker allow-list. Omitted means every approved
    /// Worker this Gateway knows about.
    #[serde(default)]
    pub workers: Option<Vec<String>>,
}

impl CallerConfig {
    /// Resolve an already-authenticated caller for the control plane.
    pub fn caller(&self) -> Caller {
        let principal = common::Principal::new(
            self.id.clone(),
            self.namespace.clone(),
            &self.scopes.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let caller = Caller::new(principal);
        match &self.workers {
            Some(workers) => caller.with_worker_allowlist(workers.clone()),
            None => caller,
        }
    }
}

impl Config {
    /// Read and validate a JSON operator configuration file.
    pub fn load(path: impl AsRef<Path>) -> Result<Config, ApiError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|_| {
            ApiError::new(
                "config.unreadable",
                "operator configuration file could not be read",
            )
            .with("path", path.display().to_string())
        })?;
        let mut config: Config = serde_json::from_slice(&bytes).map_err(|error| {
            ApiError::new(
                "config.invalid",
                "operator configuration is not valid Gateway JSON",
            )
            .with("detail", error.to_string())
            .with("path", path.display().to_string())
        })?;
        config.core.base_dir = path.parent().map(|parent| parent.to_path_buf());
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ApiError> {
        if self.callers.is_empty() {
            return Err(ApiError::new(
                "config.invalid",
                "configuration must bind at least one caller token",
            ));
        }
        let mut tokens = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for caller in &self.callers {
            if caller.token.trim().is_empty() {
                return Err(ApiError::new(
                    "config.invalid",
                    "caller token bindings must not be empty",
                ));
            }
            if caller.id.trim().is_empty() || caller.namespace.trim().is_empty() {
                return Err(ApiError::new(
                    "config.invalid",
                    "caller id and namespace must not be empty",
                ));
            }
            if !tokens.insert(caller.token.as_str()) {
                return Err(ApiError::new(
                    "config.invalid",
                    "each caller token may be bound only once",
                ));
            }
            if !ids.insert(caller.id.as_str()) {
                return Err(ApiError::new(
                    "config.invalid",
                    "caller ids must be unique so authorization is unambiguous",
                ));
            }
        }
        self.socket.validate()?;
        self.core.validate()
    }
}
