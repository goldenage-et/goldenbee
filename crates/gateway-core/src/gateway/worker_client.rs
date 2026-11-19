//! Worker HTTP exchange, liveness probe, and delegation issuance.

use crate::registry::Verified;
use common::delegation::{issue_delegation, DelegationClaims};
use common::{
    scopes, ApiError, CapabilityDescriptor, Limits, Principal, SubmitTaskRequest,
    SubmitTaskResponse, TeamRoster, WorkerSummary,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::submit::DispatchReply;
use super::State;

const DELEGATION_TTL_MS: i64 = 30_000;

pub(super) fn issue_worker_delegation(
    secret: &[u8],
    worker: &WorkerSummary,
    principal: &Principal,
    capability: &str,
    scope: &str,
    limits: &Limits,
) -> Result<String, ApiError> {
    issue_worker_delegation_scopes(secret, worker, principal, capability, &[scope], limits)
}

pub(super) fn issue_worker_delegation_scopes(
    secret: &[u8],
    worker: &WorkerSummary,
    principal: &Principal,
    capability: &str,
    scopes: &[&str],
    limits: &Limits,
) -> Result<String, ApiError> {
    let claims = DelegationClaims {
        issuer: "goldenbee-gateway".into(),
        audience: worker.instance.clone(),
        principal: principal.id.clone(),
        namespace: principal.namespace.clone(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        capabilities: vec![capability.to_string()],
        expires_at: common::now_ms() + DELEGATION_TTL_MS,
        depth: 0,
        max_depth: 2,
        limits: limits.clone(),
        token_id: uuid::Uuid::new_v4().to_string(),
        team: None,
        path: Vec::new(),
    };
    issue_delegation(secret, &claims)
}

/// Issue a delegation for a non-task control exchange.
///
/// The Worker remains the lease authority; this token only proves the caller's
/// authenticated identity and the single control scope the Gateway checked.
pub(super) fn issue_control_delegation(
    secret: &[u8],
    worker: &WorkerSummary,
    principal: &Principal,
    scope: &str,
) -> Result<String, ApiError> {
    let claims = DelegationClaims {
        issuer: "goldenbee-gateway".into(),
        audience: worker.instance.clone(),
        principal: principal.id.clone(),
        namespace: principal.namespace.clone(),
        scopes: vec![scope.to_string()],
        capabilities: Vec::new(),
        expires_at: common::now_ms() + DELEGATION_TTL_MS,
        depth: 0,
        max_depth: 2,
        limits: Limits::default(),
        token_id: uuid::Uuid::new_v4().to_string(),
        team: None,
        path: Vec::new(),
    };
    issue_delegation(secret, &claims)
}

/// Issue a Master delegation carrying the immutable team roster snapshot.
///
/// The token's depth ceiling is translated for the shared verifier, which
/// rejects a token when `depth >= max_depth`: a team policy depth of one hop
/// therefore issues `max_depth = 2` for the initial Master token. The roster's
/// own policy stays authoritative in the snapshot and is rechecked on every
/// child delegation.
pub(super) fn issue_team_delegation(
    secret: &[u8],
    worker: &WorkerSummary,
    principal: &Principal,
    capability: &str,
    scope: &str,
    limits: &Limits,
    roster: &TeamRoster,
) -> Result<String, ApiError> {
    let master = roster.master_member().ok_or_else(|| {
        ApiError::new(
            "team.master_invalid",
            "the designated Master is not a roster member",
        )
    })?;
    if !master
        .capabilities
        .iter()
        .any(|granted| granted == capability)
    {
        return Err(ApiError::new(
            "scope.capability_denied",
            format!("the Master member does not authorize capability '{capability}'"),
        ));
    }
    let mut limits = limits.narrower(&roster.limits);
    limits.max_depth = roster.limits.max_depth;
    limits.max_fanout = roster.limits.max_fanout;
    let token_max_depth = roster.limits.max_depth.saturating_add(1).min(16);
    let claims = DelegationClaims {
        issuer: "goldenbee-gateway".into(),
        audience: worker.instance.clone(),
        principal: principal.id.clone(),
        namespace: principal.namespace.clone(),
        scopes: vec![scope.to_string()],
        capabilities: master.capabilities.clone(),
        expires_at: common::now_ms() + DELEGATION_TTL_MS,
        depth: 0,
        max_depth: token_max_depth,
        limits,
        token_id: uuid::Uuid::new_v4().to_string(),
        team: Some(roster.clone()),
        path: vec![master.instance.clone()],
    };
    issue_delegation(secret, &claims)
}

#[derive(Debug, Deserialize)]
struct WorkerLiveness {
    instance: String,
    incarnation: String,
}

#[derive(Debug, Deserialize)]
struct WorkerCapabilities {
    package: String,
    version: String,
    protocol: String,
    instance: String,
    capabilities: Vec<CapabilityDescriptor>,
}

pub(super) async fn probe(
    client: &reqwest::Client,
    secret: &[u8],
    endpoint: &str,
) -> Result<Verified, ApiError> {
    let response = client
        .get(format!("{endpoint}/v1/health"))
        .send()
        .await
        .map_err(|_| {
            ApiError::retryable(
                "worker.endpoint_unavailable",
                "Worker health endpoint did not respond",
            )
        })?;
    if !response.status().is_success() {
        return Err(ApiError::retryable(
            "worker.endpoint_unavailable",
            "Worker health endpoint did not report liveness",
        ));
    }
    let liveness: WorkerLiveness = response.json().await.map_err(|_| {
        ApiError::new(
            "worker.endpoint_unverified",
            "Worker health payload does not match the management protocol",
        )
    })?;
    if liveness.instance.trim().is_empty() || liveness.incarnation.trim().is_empty() {
        return Err(ApiError::new(
            "worker.endpoint_unverified",
            "Worker did not report instance and incarnation identity",
        ));
    }
    let claims = DelegationClaims {
        issuer: "goldenbee-gateway".into(),
        audience: liveness.instance.clone(),
        principal: "gateway-control".into(),
        namespace: "gateway".into(),
        scopes: vec![scopes::TASK_READ.into()],
        capabilities: Vec::new(),
        expires_at: common::now_ms() + DELEGATION_TTL_MS,
        depth: 0,
        max_depth: 2,
        limits: Limits::default(),
        token_id: uuid::Uuid::new_v4().to_string(),
        team: None,
        path: Vec::new(),
    };
    let token = issue_delegation(secret, &claims)
        .map_err(|_| ApiError::new("gateway.delegation", "delegation could not be issued"))?;
    let response = client
        .get(format!("{endpoint}/v1/capabilities"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| {
            ApiError::retryable(
                "worker.endpoint_unavailable",
                "Worker capability endpoint did not respond",
            )
        })?;
    if !response.status().is_success() {
        return Err(ApiError::new(
            "worker.endpoint_unverified",
            "Worker did not accept authenticated capability verification",
        ));
    }
    let capabilities: WorkerCapabilities = response.json().await.map_err(|_| {
        ApiError::new(
            "worker.endpoint_unverified",
            "Worker capability payload does not match the management protocol",
        )
    })?;
    if capabilities.instance != liveness.instance {
        return Err(ApiError::new(
            "worker.endpoint_unverified",
            "Worker reported inconsistent instance identity",
        ));
    }
    Ok(Verified {
        instance: liveness.instance,
        incarnation: liveness.incarnation,
        package: capabilities.package,
        version: capabilities.version,
        protocol: capabilities.protocol,
        capabilities: capabilities.capabilities,
    })
}

impl State {
    pub(super) async fn worker_get<T: DeserializeOwned>(
        &self,
        worker: &WorkerSummary,
        path: &str,
        token: &str,
    ) -> Result<T, ApiError> {
        let response = self
            .client
            .get(format!("{}{path}", worker.endpoint))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| unavailable(worker))?;
        decode(response, worker).await
    }

    pub(super) async fn worker_post<T: DeserializeOwned, B: Serialize>(
        &self,
        worker: &WorkerSummary,
        path: &str,
        token: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let response = self
            .client
            .post(format!("{}{path}", worker.endpoint))
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .map_err(|_| unavailable(worker))?;
        decode(response, worker).await
    }

    pub(super) async fn worker_put<T: DeserializeOwned, B: Serialize>(
        &self,
        worker: &WorkerSummary,
        path: &str,
        token: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let response = self
            .client
            .put(format!("{}{path}", worker.endpoint))
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .map_err(|_| unavailable(worker))?;
        decode(response, worker).await
    }

    pub(super) async fn worker_delete<T: DeserializeOwned>(
        &self,
        worker: &WorkerSummary,
        path: &str,
        token: &str,
    ) -> Result<T, ApiError> {
        let response = self
            .client
            .delete(format!("{}{path}", worker.endpoint))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| unavailable(worker))?;
        decode(response, worker).await
    }

    /// Submit with enough transport distinction to keep confirmed acceptance,
    /// definite rejection, unreachable, and ambiguous outcomes separate.
    pub(super) async fn dispatch_submit(
        &self,
        worker: &WorkerSummary,
        token: &str,
        body: &SubmitTaskRequest,
        trace: Option<&str>,
    ) -> DispatchReply {
        let mut request = self
            .client
            .post(format!("{}/v1/tasks", worker.endpoint))
            .bearer_auth(token);
        if let Some(trace) = trace {
            request = request.header("traceparent", format!("00-{trace}-0000000000000001-01"));
        }
        let response = match request.json(body).send().await {
            Ok(response) => response,
            Err(error) => {
                return if error.is_connect() {
                    DispatchReply::Unreachable(unavailable(worker))
                } else {
                    DispatchReply::Ambiguous(
                        ApiError::retryable(
                            "route.worker_unavailable",
                            "the remote request ended before an acknowledgment",
                        )
                        .with("worker", worker.id.clone()),
                    )
                };
            }
        };
        let status = response.status();
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => {
                return DispatchReply::Ambiguous(
                    ApiError::retryable(
                        "route.worker_unavailable",
                        "the remote acknowledgment body was lost",
                    )
                    .with("worker", worker.id.clone()),
                );
            }
        };
        if !status.is_success() {
            if let Ok(error) = serde_json::from_slice::<ApiError>(&bytes) {
                return DispatchReply::Rejected(error);
            }
            return DispatchReply::Rejected(ApiError::new(
                "route.invalid_worker_response",
                format!(
                    "Worker '{}' returned {status} without a typed error",
                    worker.id
                ),
            ));
        }
        match serde_json::from_slice::<SubmitTaskResponse>(&bytes) {
            Ok(response) => DispatchReply::Accepted(response),
            Err(_) => DispatchReply::Ambiguous(ApiError::retryable(
                "route.invalid_worker_response",
                "the remote acceptance payload could not be decoded",
            )),
        }
    }
}

pub(super) fn unavailable(worker: &WorkerSummary) -> ApiError {
    ApiError::retryable(
        "route.worker_unavailable",
        format!("Worker '{}' did not respond", worker.id),
    )
    .with("worker", worker.id.clone())
}

pub(super) async fn decode<T: DeserializeOwned>(
    response: reqwest::Response,
    worker: &WorkerSummary,
) -> Result<T, ApiError> {
    let status = response.status();
    let bytes = response.bytes().await.map_err(|_| unavailable(worker))?;
    if !status.is_success() {
        if let Ok(error) = serde_json::from_slice::<ApiError>(&bytes) {
            return Err(error);
        }
        return Err(ApiError::new(
            "route.invalid_worker_response",
            format!(
                "Worker '{}' returned {status} without a typed error",
                worker.id
            ),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::new(
            "route.invalid_worker_response",
            format!(
                "Worker '{}' returned a payload that does not match protocol {}",
                worker.id,
                common::PROTOCOL_VERSION
            ),
        )
    })
}
