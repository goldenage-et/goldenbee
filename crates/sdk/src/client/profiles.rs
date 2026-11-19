//! Runtime Profile revision management through the owning Worker.

use super::transport::{delete, get, post, put};
use super::Client;
use common::{
    ApiError, CreateProfileRequest, DeletedProfile, Profile, ProfileVersionRequest,
    ReviseProfileRequest,
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

fn base(worker: &str) -> String {
    format!("/v1/workers/{}/profiles", encode(worker))
}

impl Client {
    /// List Runtime Profiles on one visible Worker.
    pub async fn profiles(&self, worker: &str) -> Result<Vec<Profile>, ApiError> {
        get(self, &base(worker)).await
    }

    /// Read one Worker-owned profile.
    pub async fn profile(&self, worker: &str, name: &str) -> Result<Profile, ApiError> {
        get(self, &format!("{}/{}", base(worker), encode(name))).await
    }

    /// Create an inactive profile revision on its owning Worker.
    pub async fn create_profile(
        &self,
        worker: &str,
        request: &CreateProfileRequest,
    ) -> Result<Profile, ApiError> {
        post(self, &base(worker), request).await
    }

    /// Append a revision with the observed revision count.
    pub async fn revise_profile(
        &self,
        worker: &str,
        name: &str,
        request: &ReviseProfileRequest,
    ) -> Result<Profile, ApiError> {
        put(self, &format!("{}/{}", base(worker), encode(name)), request).await
    }

    /// Validate an immutable revision without activating it.
    pub async fn validate_profile(
        &self,
        worker: &str,
        name: &str,
        request: &ProfileVersionRequest,
    ) -> Result<Profile, ApiError> {
        post(
            self,
            &format!("{}/{}/validate", base(worker), encode(name)),
            request,
        )
        .await
    }

    /// Activate one exact validated revision.
    pub async fn activate_profile(
        &self,
        worker: &str,
        name: &str,
        request: &ProfileVersionRequest,
    ) -> Result<Profile, ApiError> {
        post(
            self,
            &format!("{}/{}/activate", base(worker), encode(name)),
            request,
        )
        .await
    }

    /// Stop new admissions without rewriting active work.
    pub async fn deactivate_profile(&self, worker: &str, name: &str) -> Result<Profile, ApiError> {
        post(
            self,
            &format!("{}/{}/deactivate", base(worker), encode(name)),
            &ProfileVersionRequest::default(),
        )
        .await
    }

    /// Remove an inactive profile, leaving the installed Worker intact.
    pub async fn delete_profile(
        &self,
        worker: &str,
        name: &str,
    ) -> Result<DeletedProfile, ApiError> {
        delete(self, &format!("{}/{}", base(worker), encode(name))).await
    }
}
