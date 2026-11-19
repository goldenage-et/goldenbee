//! Authenticated caller identity and optional Worker allow-list.

use common::Principal;
use std::collections::BTreeSet;

/// An already-authenticated caller.
///
/// A business backend using the `gateway-core` library resolves its own
/// identity, scopes, limits, and authorization policy before constructing this
/// value. The control plane enforces the scopes and allow-list it contains but
/// does not authenticate the caller or know where the identity came from.
#[derive(Debug, Clone)]
pub struct Caller {
    /// Authenticated principal, namespace, and management scopes.
    pub principal: Principal,
    /// Optional explicit Worker allow-list. `None` permits every approved
    /// Worker this control plane knows about.
    pub worker_allowlist: Option<BTreeSet<String>>,
}

impl Caller {
    /// Construct a caller without a Worker allow-list restriction.
    pub fn new(principal: Principal) -> Self {
        Self {
            principal,
            worker_allowlist: None,
        }
    }

    /// Restrict this caller to the named Worker identities.
    pub fn with_worker_allowlist(
        mut self,
        workers: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.worker_allowlist = Some(workers.into_iter().map(Into::into).collect());
        self
    }

    pub(crate) fn allows(&self, worker: &str) -> bool {
        self.worker_allowlist
            .as_ref()
            .is_none_or(|workers| workers.contains(worker))
    }
}
