//! Worker-owned fenced control lease for one interactive browser session.
//!
//! The pinned `adk-browser` 2.3.0 crate has no ownership, lease, fencing,
//! takeover, expiry, or audit facility. It exposes one `BrowserSession` whose
//! public methods are callable by any holder of the `Arc`. That is sufficient
//! for one agent and insufficient for human takeover. This module is the
//! narrowly justified adapter: the Worker allocates fencing generations and
//! serializes agent commands with human commands on one per-session gate.
//!
//! The lease is process-local by design. A process restart allocates a new
//! epoch, and every command from the previous process is rejected even if its
//! numeric generation happens to repeat.

use crate::browser::SessionKey;
use common::{now_ms, ApiError, ControlAuditEntry, ControlLease, ControlOwner, ControlStatus};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

const DEFAULT_TTL_MS: i64 = 120_000;
const MAX_TTL_MS: i64 = 3_600_000;
const MAX_AUDIT_ENTRIES: usize = 1_024;

#[derive(Debug, Clone)]
struct LeaseState {
    generation: u64,
    owner: ControlOwner,
    principal: Option<String>,
    expires_at_ms: i64,
}

impl Default for LeaseState {
    fn default() -> Self {
        Self {
            generation: 0,
            owner: ControlOwner::Agent,
            principal: None,
            expires_at_ms: 0,
        }
    }
}

/// Per-instance authority for interactive-control ownership.
pub(crate) struct ControlRegistry {
    epoch: String,
    leases: Mutex<HashMap<SessionKey, LeaseState>>,
    gates: Mutex<HashMap<SessionKey, Arc<AsyncMutex<()>>>>,
    audit: Mutex<VecDeque<(SessionKey, ControlAuditEntry)>>,
}

impl ControlRegistry {
    pub(crate) fn new() -> Self {
        Self {
            epoch: format!("ctl-{}", uuid::Uuid::new_v4().simple()),
            leases: Mutex::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
            audit: Mutex::new(VecDeque::new()),
        }
    }

    pub(crate) fn epoch(&self) -> &str {
        &self.epoch
    }

    pub(crate) fn gate(&self, key: &SessionKey) -> Arc<AsyncMutex<()>> {
        let mut gates = self.gates.lock().expect("control gate lock");
        gates
            .entry(key.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// Atomically allocate a new human fencing generation.
    ///
    /// The caller holds the session gate, so no agent command is in flight.
    pub(crate) fn acquire(
        &self,
        key: &SessionKey,
        principal: &str,
        ttl_ms: Option<u64>,
    ) -> Result<ControlLease, ApiError> {
        let mut leases = self.leases.lock().expect("control lease lock");
        let state = leases.entry(key.clone()).or_default();
        self.expire_if_needed(key, state);
        if state.owner == ControlOwner::Human && state.principal.as_deref() != Some(principal) {
            return Err(ApiError::new(
                "control.lease_held",
                "another human controller currently owns this session",
            )
            .with("generation", state.generation.to_string()));
        }
        let ttl = ttl_ms
            .map(|ttl| ttl as i64)
            .unwrap_or(DEFAULT_TTL_MS)
            .clamp(1, MAX_TTL_MS);
        state.generation = state.generation.saturating_add(1);
        state.owner = ControlOwner::Human;
        state.principal = Some(principal.to_string());
        state.expires_at_ms = now_ms().saturating_add(ttl);
        let lease = lease_of(key, state, &self.epoch);
        drop(leases);
        self.record(
            key,
            principal,
            "acquire",
            lease.generation,
            "accepted",
            "takeover",
        );
        Ok(lease)
    }

    /// Explicitly return ownership to the Worker agent with a new generation.
    pub(crate) fn hand_back(
        &self,
        key: &SessionKey,
        principal: &str,
        epoch: &str,
        generation: u64,
    ) -> Result<ControlLease, ApiError> {
        self.verify_without_mutation(key, principal, epoch, generation)?;
        let mut leases = self.leases.lock().expect("control lease lock");
        let state = leases.entry(key.clone()).or_default();
        state.generation = state.generation.saturating_add(1);
        state.owner = ControlOwner::Agent;
        state.principal = None;
        state.expires_at_ms = 0;
        let lease = lease_of(key, state, &self.epoch);
        drop(leases);
        self.record(
            key,
            principal,
            "hand_back",
            lease.generation,
            "accepted",
            "agent",
        );
        Ok(lease)
    }

    /// Validate a human command without mutating a live lease.
    pub(crate) fn verify(
        &self,
        key: &SessionKey,
        principal: &str,
        epoch: &str,
        generation: u64,
    ) -> Result<(), ApiError> {
        self.verify_inner(key, principal, epoch, generation, true)
    }

    fn verify_without_mutation(
        &self,
        key: &SessionKey,
        principal: &str,
        epoch: &str,
        generation: u64,
    ) -> Result<(), ApiError> {
        self.verify_inner(key, principal, epoch, generation, false)
    }

    fn verify_inner(
        &self,
        key: &SessionKey,
        principal: &str,
        epoch: &str,
        generation: u64,
        expire: bool,
    ) -> Result<(), ApiError> {
        if epoch != self.epoch {
            return Err(ApiError::new(
                "control.epoch_mismatch",
                "this control request belongs to a previous Worker process",
            ));
        }
        let mut leases = self.leases.lock().expect("control lease lock");
        let Some(state) = leases.get_mut(key) else {
            return Err(ApiError::new(
                "control.session_not_found",
                "no control lease exists for this exact session",
            ));
        };
        if state.owner != ControlOwner::Human {
            return Err(ApiError::new(
                "control.fenced",
                "a newer ownership generation superseded this controller",
            )
            .with("generation", state.generation.to_string()));
        }
        let expired = state.expires_at_ms <= now_ms();
        if expired {
            if expire {
                self.expire_if_needed(key, state);
            }
            return Err(ApiError::new(
                "control.lease_expired",
                "the human control lease expired and the agent owner resumed",
            ));
        }
        if state.principal.as_deref() != Some(principal) {
            return Err(ApiError::new(
                "control.principal_denied",
                "only the principal that took control may operate this session",
            ));
        }
        if state.generation != generation {
            return Err(ApiError::new(
                "control.fenced",
                "a newer ownership generation superseded this controller",
            )
            .with("generation", state.generation.to_string()));
        }
        Ok(())
    }

    /// Current lease validation before an agent browser command may mutate a page.
    pub(crate) fn begin_agent(&self, key: &SessionKey) -> Result<u64, ApiError> {
        let mut leases = self.leases.lock().expect("control lease lock");
        let state = leases.entry(key.clone()).or_default();
        if state.owner == ControlOwner::Human {
            if state.expires_at_ms > now_ms() {
                return Err(ApiError::new(
                    "control.fenced",
                    "a human controller currently owns this browser session",
                )
                .with("generation", state.generation.to_string()));
            }
            self.expire_if_needed(key, state);
        }
        Ok(state.generation)
    }

    pub(crate) fn status(&self, key: &SessionKey) -> Result<ControlStatus, ApiError> {
        let mut leases = self.leases.lock().expect("control lease lock");
        let Some(state) = leases.get_mut(key) else {
            return Err(ApiError::new(
                "control.session_not_found",
                "no control lease exists for this exact session",
            ));
        };
        self.expire_if_needed(key, state);
        Ok(ControlStatus {
            session_id: key.session.clone(),
            generation: state.generation,
            owner: state.owner,
            principal: state.principal.clone(),
            epoch: self.epoch.clone(),
            expires_at_ms: state.expires_at_ms,
        })
    }

    pub(crate) fn audit(&self, key: &SessionKey) -> Vec<ControlAuditEntry> {
        self.audit
            .lock()
            .expect("control audit lock")
            .iter()
            .filter(|(entry_key, _)| entry_key == key)
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    pub(crate) fn record(
        &self,
        key: &SessionKey,
        principal: &str,
        action: &str,
        generation: u64,
        outcome: &str,
        detail: &str,
    ) {
        let mut audit = self.audit.lock().expect("control audit lock");
        if audit.len() >= MAX_AUDIT_ENTRIES {
            audit.pop_front();
        }
        audit.push_back((
            key.clone(),
            ControlAuditEntry {
                at: now_ms(),
                action: action.to_string(),
                session_id: key.session.clone(),
                principal: principal.to_string(),
                generation,
                outcome: outcome.to_string(),
                detail: detail.to_string(),
            },
        ));
    }

    pub(crate) fn record_reject(&self, key: &SessionKey, principal: &str, error: &ApiError) {
        let generation = self
            .leases
            .lock()
            .expect("control lease lock")
            .get(key)
            .map(|state| state.generation)
            .unwrap_or(0);
        self.record(
            key,
            principal,
            "reject",
            generation,
            "rejected",
            &error.code,
        );
    }

    /// Drop lease and gate state after the session itself is released.
    pub(crate) fn forget(&self, key: &SessionKey) {
        self.leases.lock().expect("control lease lock").remove(key);
        self.gates.lock().expect("control gate lock").remove(key);
    }

    fn expire_if_needed(&self, key: &SessionKey, state: &mut LeaseState) {
        if state.owner == ControlOwner::Human && state.expires_at_ms <= now_ms() {
            state.generation = state.generation.saturating_add(1);
            state.owner = ControlOwner::Agent;
            state.principal = None;
            state.expires_at_ms = 0;
            let principal = "worker".to_string();
            self.record(
                key,
                &principal,
                "expire",
                state.generation,
                "accepted",
                "lease_expired",
            );
        }
    }
}

fn lease_of(key: &SessionKey, state: &LeaseState, epoch: &str) -> ControlLease {
    ControlLease {
        session_id: key.session.clone(),
        generation: state.generation,
        owner: state.owner,
        principal: state.principal.clone(),
        epoch: epoch.to_string(),
        expires_at_ms: state.expires_at_ms,
        schema_version: common::CONTROL_SCHEMA_VERSION.to_string(),
    }
}
