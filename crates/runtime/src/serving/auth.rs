//! Bearer authentication and Gateway delegation narrowing.

use crate::core::crypto::{verify_delegation, DelegationClaims};
use crate::serving::error::failure;
use crate::serving::WorkerState;
use crate::Result;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap};
use axum::middleware::Next;
use axum::response::Response;
use common::{ApiError, Principal};
use std::net::SocketAddr;

impl WorkerState {
    /// Resolve a local operator token or a Gateway delegation token.
    pub(super) async fn principal(
        &self,
        token: &str,
        native: bool,
    ) -> Result<(Principal, Option<DelegationClaims>)> {
        if let Ok(principal) = self.runtime.authenticate(token).await {
            return Ok((principal, None));
        }
        // Native development routes are operator-only. Product traffic uses
        // the versioned harness routes.
        if native {
            return Err(ApiError::new(
                "auth.unauthenticated",
                "native development routes require a local operator token",
            ));
        }
        let claims = verify_delegation(
            &self.runtime.config().delegation_secret,
            token,
            &self.runtime.config().instance_id,
            common::now_ms(),
        )?;
        let scopes: Vec<&str> = claims.scopes.iter().map(String::as_str).collect();
        Ok((
            Principal::new(claims.principal.clone(), claims.namespace.clone(), &scopes),
            Some(claims),
        ))
    }
}

fn is_public(path: &str) -> bool {
    matches!(
        path,
        "/" | "/v1/health"
            | "/api/health"
            | "/.well-known/agent.json"
            | "/.well-known/agent-card.json"
    ) || path.starts_with("/ui/")
        // A scoped grant is itself the bearer credential for one artifact and
        // expiry window. The handler validates it; no other route is public.
        || path.starts_with("/v1/artifact-grants/")
}

pub(super) async fn authorize(
    State(state): State<WorkerState>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    if is_public(&path) {
        return next.run(request).await;
    }
    // Explicit local development mode for the native ADK web UI, which cannot
    // attach a bearer token. Only loopback peers qualify, and harness `/v1`
    // and A2A routes remain authenticated: an ownerless A2A task would be
    // visible to every caller.
    if !path.starts_with("/v1/") && !path.starts_with("/a2a") && state.runtime.config().dev_ui {
        let loopback = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .is_some_and(|ConnectInfo(peer)| peer.ip().is_loopback());
        if loopback {
            return next.run(request).await;
        }
    }
    let Some(token) = bearer(request.headers()) else {
        return failure(ApiError::new(
            "auth.unauthenticated",
            "a bearer token is required",
        ));
    };
    // A2A is an authorized peer/Gateway protocol, not the native development
    // surface. It accepts an operator token or a Gateway delegation token
    // scoped to this instance; product Clients still reach it only through the
    // Gateway. Native widget/debug/UI routes remain operator-only.
    let native = !path.starts_with("/v1/") && !path.starts_with("/a2a");
    match state.principal(&token, native).await {
        Ok((principal, claims)) => {
            request.extensions_mut().insert(principal);
            if let Some(claims) = claims {
                request.extensions_mut().insert(claims);
            }
            next.run(request).await
        }
        Err(error) => failure(error),
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|token| token.to_string())
}
