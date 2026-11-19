//! Authenticated duplex socket ticket issuance and upgrade handshake.
//!
//! Ticket minting uses the caller-authenticated HTTP surface. The upgrade
//! query carries only the opaque one-use ticket, never the caller bearer.

mod connection;
mod sse;

pub(crate) use connection::SocketLimits;

use crate::http::failure;
use crate::state::SharedState;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{RawQuery, State as AxumState};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use common::{ApiError, SocketTicket};
use gateway_core::Caller;

/// Authenticated route that mints one short-lived one-use upgrade ticket.
pub(crate) fn caller_routes() -> Router<SharedState> {
    Router::new().route("/v1/socket/tickets", post(ticket))
}

/// Public upgrade route. It authenticates by redeeming the ticket, so the
/// caller bearer middleware does not apply.
pub(crate) fn upgrade_route() -> Router<SharedState> {
    Router::new().route("/v1/socket", get(upgrade))
}

async fn ticket(
    AxumState(state): AxumState<SharedState>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<SocketTicket>, Response> {
    state
        .tickets
        .lock()
        .map_err(|_| {
            failure(ApiError::new(
                "storage.unavailable",
                "the socket ticket store is unavailable",
            ))
        })?
        .mint(caller, state.socket.ticket_ttl_ms.max(100))
        .map(Json)
        .map_err(failure)
}

async fn upgrade(
    AxumState(state): AxumState<SharedState>,
    RawQuery(raw): RawQuery,
    websocket: WebSocketUpgrade,
) -> Response {
    let ticket = match ticket_from_query(raw.as_deref()) {
        Ok(ticket) => ticket,
        Err(error) => return failure(error),
    };
    let caller = {
        let mut store = match state.tickets.lock() {
            Ok(store) => store,
            Err(_) => {
                return failure(ApiError::new(
                    "storage.unavailable",
                    "the socket ticket store is unavailable",
                ))
            }
        };
        match store.redeem(&ticket) {
            Ok(caller) => caller,
            Err(error) => return failure(error),
        }
    };
    let limits = state.socket.clone();
    let message_bound = limits.max_frame_bytes.saturating_mul(2);
    websocket
        .max_frame_size(message_bound)
        .max_message_size(message_bound)
        .max_write_buffer_size(limits.max_frame_bytes)
        .write_buffer_size(limits.max_frame_bytes.min(8 * 1024))
        .on_upgrade(move |socket| connection::run(socket, state, caller, limits))
}

fn ticket_from_query(query: Option<&str>) -> Result<String, ApiError> {
    let Some(query) = query else {
        return Err(ApiError::new(
            "auth.socket_ticket",
            "a one-use socket ticket is required",
        ));
    };
    let mut found = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key != "ticket" {
            continue;
        }
        if found.is_some() {
            return Err(ApiError::new(
                "auth.socket_ticket",
                "the socket ticket must appear exactly once",
            ));
        }
        found = Some(value.to_string());
    }
    match found {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(ApiError::new(
            "auth.socket_ticket",
            "a one-use socket ticket is required",
        )),
    }
}
