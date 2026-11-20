//! Shared adapter state: the control plane, caller bindings, and socket tickets.

use crate::host::HostManager;
use crate::socket::SocketLimits;
use common::{ApiError, SocketTicket};
use gateway_core::{Caller, Gateway as CoreGateway};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Shared handle to one prepared Gateway process.
pub(crate) type SharedState = Arc<AppState>;

/// Control plane and caller authentication data shared by every route.
pub(crate) struct AppState {
    /// Control-plane owner. Every Worker, task, and event decision comes from
    /// here; the adapter holds no second policy layer.
    pub(crate) core: CoreGateway,
    /// Gateway-owned local host lifecycle. `None` leaves this Gateway
    /// control-plane only.
    pub(crate) local_host: Option<Arc<HostManager>>,
    /// Stable local host identity, when local management is enabled.
    pub(crate) local_host_id: Option<String>,
    /// Caller token bindings. Tokens are secrets and are never logged.
    pub(crate) callers: BTreeMap<String, Caller>,
    /// Bounded short-lived one-use socket ticket set.
    pub(crate) tickets: Mutex<TicketStore>,
    /// Bounded duplex socket settings.
    pub(crate) socket: SocketLimits,
}

struct TicketRecord {
    caller: Caller,
    expires_at_ms: i64,
}

/// Bounded one-use ticket set. A ticket is removed at redemption, so a repeat
/// can never authenticate a second connection.
pub(crate) struct TicketStore {
    records: BTreeMap<String, TicketRecord>,
    max: usize,
}

impl TicketStore {
    /// Create an empty ticket set with an explicit capacity.
    pub(crate) fn new(max: usize) -> Self {
        Self {
            records: BTreeMap::new(),
            max: max.max(1),
        }
    }

    /// Mint one ticket bound to the authenticated caller.
    pub(crate) fn mint(&mut self, caller: Caller, ttl_ms: u64) -> Result<SocketTicket, ApiError> {
        let now = common::now_ms();
        self.records.retain(|_, record| record.expires_at_ms > now);
        if self.records.len() >= self.max {
            return Err(ApiError::retryable(
                "capacity.socket_tickets",
                "this Gateway has reached its bounded number of pending socket tickets",
            ));
        }
        let ticket = format!("st-{}", uuid::Uuid::new_v4().simple());
        let expires_at_ms = now.saturating_add(ttl_ms.min(i64::MAX as u64) as i64);
        self.records.insert(
            ticket.clone(),
            TicketRecord {
                caller,
                expires_at_ms,
            },
        );
        Ok(SocketTicket {
            ticket,
            expires_at_ms,
            path: "/v1/socket".into(),
        })
    }

    /// Redeem a ticket exactly once.
    pub(crate) fn redeem(&mut self, ticket: &str) -> Result<Caller, ApiError> {
        let Some(record) = self.records.remove(ticket) else {
            return Err(ApiError::new(
                "auth.socket_ticket",
                "the socket ticket is unknown, expired, or already used",
            ));
        };
        if record.expires_at_ms <= common::now_ms() {
            return Err(ApiError::new(
                "auth.socket_ticket",
                "the socket ticket has expired; request a new one",
            ));
        }
        Ok(record.caller)
    }
}
