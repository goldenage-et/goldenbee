//! Caller discovery, Worker summaries, admin inventory, and liveness counts.

use common::{AdminWorkerSummary, WorkerDiscoveryFilter, WorkerSummary};

use super::capabilities::admin_summary;
use super::Registry;

impl Registry {
    /// Current state for one Worker.
    pub fn worker(&self, id: &str) -> Option<WorkerSummary> {
        let now = self.now();
        self.lock()
            .document
            .workers
            .get(id)
            .map(|record| record.summary(now))
    }

    /// Admin inventory. Decommissioned identities remain visible here.
    pub fn admin_workers(&self) -> Vec<AdminWorkerSummary> {
        let now = self.now();
        let document = self.lock();
        document
            .document
            .approvals
            .values()
            .map(|approval| {
                admin_summary(approval, document.document.workers.get(&approval.id), now)
            })
            .collect()
    }

    /// Admin view of one approved identity.
    pub fn admin_worker(&self, id: &str) -> Option<AdminWorkerSummary> {
        let now = self.now();
        let document = self.lock();
        document
            .document
            .approvals
            .get(id)
            .map(|approval| admin_summary(approval, document.document.workers.get(id), now))
    }

    /// Authorized, filtered caller discovery. Disabled and decommissioned
    /// identities are hidden.
    pub fn discover(
        &self,
        allowed: impl Fn(&str) -> bool,
        filter: &WorkerDiscoveryFilter,
    ) -> Vec<WorkerSummary> {
        let now = self.now();
        let document = self.lock();
        document
            .document
            .workers
            .values()
            .filter(|record| {
                document
                    .document
                    .approvals
                    .get(&record.id)
                    .is_some_and(|approval| approval.enabled && !approval.decommissioned)
            })
            .filter(|record| !matches!(record.status.as_str(), "decommissioned" | "disabled"))
            .filter(|record| allowed(&record.id))
            .filter(|record| {
                filter.capability.as_ref().is_none_or(|capability| {
                    record
                        .capabilities
                        .iter()
                        .any(|declared| &declared.id == capability)
                })
            })
            .filter(|record| {
                filter
                    .protocol
                    .as_ref()
                    .is_none_or(|protocol| &record.protocol == protocol)
            })
            .filter(|record| {
                filter
                    .locality
                    .as_ref()
                    .is_none_or(|locality| &record.locality == locality)
            })
            .filter(|record| match filter.health.as_deref() {
                Some("healthy") => record.reachable(now),
                Some("unreachable") => !record.reachable(now),
                _ => true,
            })
            .map(|record| record.summary(now))
            .collect()
    }

    /// Registry counts for liveness.
    pub fn health(&self) -> (u32, u32) {
        let now = self.now();
        let document = self.lock();
        let approved = document
            .document
            .approvals
            .values()
            .filter(|approval| approval.enabled && !approval.decommissioned)
            .count() as u32;
        let reachable = document
            .document
            .workers
            .values()
            .filter(|record| record.reachable(now))
            .count() as u32;
        (approved, reachable)
    }
}
