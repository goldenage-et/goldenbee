//! Versioned team roster operations.

use super::transport::{get, post, put};
use super::Client;
use common::{ApiError, CreateTeamRequest, Team, TeamVersionRequest, UpdateTeamRequest};

impl Client {
    /// List teams visible to this caller.
    pub async fn teams(&self) -> Result<Vec<Team>, ApiError> {
        get(self, "/v1/teams").await
    }

    /// Fetch one team roster.
    pub async fn team(&self, name: &str) -> Result<Team, ApiError> {
        get(self, &format!("/v1/teams/{name}")).await
    }

    /// Create an inactive versioned roster.
    pub async fn create_team(&self, request: &CreateTeamRequest) -> Result<Team, ApiError> {
        post(self, "/v1/teams", request).await
    }

    /// Update a roster with optimistic concurrency.
    pub async fn update_team(
        &self,
        name: &str,
        request: &UpdateTeamRequest,
    ) -> Result<Team, ApiError> {
        put(self, &format!("/v1/teams/{name}"), request).await
    }

    /// Revalidate a roster against current Worker leases.
    pub async fn validate_team(
        &self,
        name: &str,
        request: &TeamVersionRequest,
    ) -> Result<Team, ApiError> {
        post(self, &format!("/v1/teams/{name}/validate"), request).await
    }

    /// Activate a revalidated roster.
    pub async fn activate_team(
        &self,
        name: &str,
        request: &TeamVersionRequest,
    ) -> Result<Team, ApiError> {
        post(self, &format!("/v1/teams/{name}/activate"), request).await
    }

    /// Deactivate future team admission.
    pub async fn deactivate_team(
        &self,
        name: &str,
        request: &TeamVersionRequest,
    ) -> Result<Team, ApiError> {
        post(self, &format!("/v1/teams/{name}/deactivate"), request).await
    }
}
