//! Authorized Runtime Profile management through the owning Worker.
//!
//! Gateway forwards typed revision operations to an approved Worker it can
//! reach and that the caller may see. Profiles remain Worker-owned: validation,
//! immutable revisions, optimistic concurrency, and file-loaded asset resolution
//! all happen in the Worker runtime. The Gateway stores no profile state.

use super::worker_client::issue_control_delegation;
use super::Gateway;
use crate::caller::Caller;
use common::{
    scopes, ApiError, CreateProfileRequest, DeletedProfile, Profile, ProfileVersionRequest,
    ReviseProfileRequest, WorkerSummary,
};

fn encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

impl Gateway {
    fn require_profile_read(&self, caller: &Caller) -> Result<(), ApiError> {
        caller
            .principal
            .require_any(&[scopes::PROFILE_READ, scopes::ADMIN])
    }

    fn require_profile_write(&self, caller: &Caller) -> Result<(), ApiError> {
        caller
            .principal
            .require_any(&[scopes::PROFILE_WRITE, scopes::ADMIN])
    }

    fn profile_target(&self, caller: &Caller, worker: &str) -> Result<WorkerSummary, ApiError> {
        if !caller.allows(worker) {
            return Err(ApiError::new(
                "scope.worker_denied",
                format!("Worker '{worker}' is not visible to this caller"),
            ));
        }
        let worker = self.worker(worker)?;
        if matches!(worker.status.as_str(), "disabled" | "decommissioned") {
            return Err(ApiError::new(
                "profile.worker_unavailable",
                format!("Worker '{}' is not available for management", worker.id),
            )
            .with("worker", worker.id));
        }
        if !worker.reachable {
            return Err(ApiError::retryable(
                "route.worker_unavailable",
                format!("Worker '{}' is not reachable", worker.id),
            )
            .with("worker", worker.id));
        }
        Ok(worker)
    }

    fn profile_token(
        &self,
        caller: &Caller,
        worker: &WorkerSummary,
        scope: &str,
    ) -> Result<String, ApiError> {
        issue_control_delegation(&self.state.secret, worker, &caller.principal, scope)
    }

    fn profile_audit(&self, caller: &Caller, worker: &str, action: &str, outcome: &str) {
        self.state.registry.audit(
            self.state.registry.now(),
            action,
            worker,
            &caller.principal.id,
            outcome,
            "profile revision operation",
        );
    }

    fn denied_profile(&self, caller: &Caller, worker: &str, error: ApiError) -> ApiError {
        self.profile_audit(caller, worker, "profile.denied", "denied");
        error
    }

    /// List profiles in the caller's namespace on one visible Worker.
    pub async fn profiles(&self, caller: &Caller, worker: &str) -> Result<Vec<Profile>, ApiError> {
        if let Err(error) = self.require_profile_read(caller) {
            return Err(self.denied_profile(caller, worker, error));
        }
        let target = self.profile_target(caller, worker)?;
        let token = self.profile_token(caller, &target, scopes::PROFILE_READ)?;
        match self
            .state
            .worker_get::<Vec<Profile>>(&target, "/v1/profiles", &token)
            .await
        {
            Ok(profiles) => Ok(profiles),
            Err(error) => Err(self.denied_profile(caller, worker, error)),
        }
    }

    /// Read one profile owned by the Worker.
    pub async fn profile(
        &self,
        caller: &Caller,
        worker: &str,
        name: &str,
    ) -> Result<Profile, ApiError> {
        if let Err(error) = self.require_profile_read(caller) {
            return Err(self.denied_profile(caller, worker, error));
        }
        let target = self.profile_target(caller, worker)?;
        let token = self.profile_token(caller, &target, scopes::PROFILE_READ)?;
        match self
            .state
            .worker_get::<Profile>(&target, &format!("/v1/profiles/{}", encode(name)), &token)
            .await
        {
            Ok(profile) => Ok(profile),
            Err(error) => Err(self.denied_profile(caller, worker, error)),
        }
    }

    /// Create an inactive Worker-owned profile.
    pub async fn create_profile(
        &self,
        caller: &Caller,
        worker: &str,
        request: &CreateProfileRequest,
    ) -> Result<Profile, ApiError> {
        if let Err(error) = self.require_profile_write(caller) {
            return Err(self.denied_profile(caller, worker, error));
        }
        let target = self.profile_target(caller, worker)?;
        let token = self.profile_token(caller, &target, scopes::PROFILE_WRITE)?;
        let profile = self
            .state
            .worker_post::<Profile, _>(&target, "/v1/profiles", &token, request)
            .await
            .map_err(|error| self.denied_profile(caller, worker, error))?;
        self.profile_audit(caller, &target.id, "profile.create", "accepted");
        Ok(profile)
    }

    /// Append a revision with optimistic concurrency enforced by the Worker.
    pub async fn revise_profile(
        &self,
        caller: &Caller,
        worker: &str,
        name: &str,
        request: &ReviseProfileRequest,
    ) -> Result<Profile, ApiError> {
        if let Err(error) = self.require_profile_write(caller) {
            return Err(self.denied_profile(caller, worker, error));
        }
        let target = self.profile_target(caller, worker)?;
        let token = self.profile_token(caller, &target, scopes::PROFILE_WRITE)?;
        let profile = self
            .state
            .worker_put::<Profile, _>(
                &target,
                &format!("/v1/profiles/{}", encode(name)),
                &token,
                request,
            )
            .await
            .map_err(|error| self.denied_profile(caller, worker, error))?;
        self.profile_audit(caller, &target.id, "profile.revise", "accepted");
        Ok(profile)
    }

    /// Validate an immutable revision without activating it.
    pub async fn validate_profile(
        &self,
        caller: &Caller,
        worker: &str,
        name: &str,
        request: &ProfileVersionRequest,
    ) -> Result<Profile, ApiError> {
        if let Err(error) = self.require_profile_read(caller) {
            return Err(self.denied_profile(caller, worker, error));
        }
        let target = self.profile_target(caller, worker)?;
        let token = self.profile_token(caller, &target, scopes::PROFILE_READ)?;
        let profile = self
            .state
            .worker_post::<Profile, _>(
                &target,
                &format!("/v1/profiles/{}/validate", encode(name)),
                &token,
                request,
            )
            .await
            .map_err(|error| self.denied_profile(caller, worker, error))?;
        self.profile_audit(caller, &target.id, "profile.validate", "accepted");
        Ok(profile)
    }

    /// Activate an exact validated revision on the owning Worker.
    pub async fn activate_profile(
        &self,
        caller: &Caller,
        worker: &str,
        name: &str,
        request: &ProfileVersionRequest,
    ) -> Result<Profile, ApiError> {
        if let Err(error) = self.require_profile_write(caller) {
            return Err(self.denied_profile(caller, worker, error));
        }
        let target = self.profile_target(caller, worker)?;
        let token = self.profile_token(caller, &target, scopes::PROFILE_WRITE)?;
        let profile = self
            .state
            .worker_post::<Profile, _>(
                &target,
                &format!("/v1/profiles/{}/activate", encode(name)),
                &token,
                request,
            )
            .await
            .map_err(|error| self.denied_profile(caller, worker, error))?;
        self.profile_audit(caller, &target.id, "profile.activate", "accepted");
        Ok(profile)
    }

    /// Deactivate future admission while leaving active work untouched.
    pub async fn deactivate_profile(
        &self,
        caller: &Caller,
        worker: &str,
        name: &str,
    ) -> Result<Profile, ApiError> {
        if let Err(error) = self.require_profile_write(caller) {
            return Err(self.denied_profile(caller, worker, error));
        }
        let target = self.profile_target(caller, worker)?;
        let token = self.profile_token(caller, &target, scopes::PROFILE_WRITE)?;
        let profile = self
            .state
            .worker_post::<Profile, _>(
                &target,
                &format!("/v1/profiles/{}/deactivate", encode(name)),
                &token,
                &ProfileVersionRequest::default(),
            )
            .await
            .map_err(|error| self.denied_profile(caller, worker, error))?;
        self.profile_audit(caller, &target.id, "profile.deactivate", "accepted");
        Ok(profile)
    }

    /// Remove an inactive profile from the owning Worker.
    pub async fn delete_profile(
        &self,
        caller: &Caller,
        worker: &str,
        name: &str,
    ) -> Result<DeletedProfile, ApiError> {
        if let Err(error) = self.require_profile_write(caller) {
            return Err(self.denied_profile(caller, worker, error));
        }
        let target = self.profile_target(caller, worker)?;
        let token = self.profile_token(caller, &target, scopes::PROFILE_WRITE)?;
        let deleted = self
            .state
            .worker_delete::<DeletedProfile>(
                &target,
                &format!("/v1/profiles/{}", encode(name)),
                &token,
            )
            .await
            .map_err(|error| self.denied_profile(caller, worker, error))?;
        self.profile_audit(caller, &target.id, "profile.delete", "accepted");
        Ok(deleted)
    }
}
