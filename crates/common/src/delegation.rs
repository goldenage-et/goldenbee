//! Gateway-to-Worker delegation signing primitives.
//!
//! The wire claim shape and its HMAC token format are shared so Gateway can
//! issue a token and a Worker can verify it without either process linking the
//! other's implementation. Signing secrets are supplied by the deployment and
//! are never carried in the token.

use crate::error::ApiError;
use crate::limits::Limits;
use crate::team::TeamRoster;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Claims carried by a Gateway-to-Worker delegation token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationClaims {
    /// Issuer.
    pub issuer: String,
    /// Audience Worker identity.
    pub audience: String,
    /// Caller principal.
    pub principal: String,
    /// Namespace.
    pub namespace: String,
    /// Scopes delegated.
    pub scopes: Vec<String>,
    /// Capability subset.
    pub capabilities: Vec<String>,
    /// Expiry in milliseconds.
    pub expires_at: i64,
    /// Current delegation depth.
    pub depth: u32,
    /// Maximum depth.
    pub max_depth: u32,
    /// Narrowed limits.
    pub limits: Limits,
    /// Unique token identity for replay rejection.
    pub token_id: String,
    /// Immutable process-level team roster, when this token targets a team Master.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<TeamRoster>,
    /// Delegation path of Worker instance identities, including the current hop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
}

/// Issue a signed delegation token.
pub fn issue_delegation(secret: &[u8], claims: &DelegationClaims) -> Result<String, ApiError> {
    let body = serde_json::to_vec(claims)
        .map_err(|error| ApiError::new("delegation.invalid", error.to_string()))?;
    let body = B64.encode(body);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret)
        .map_err(|_| ApiError::new("delegation.invalid", "invalid signing secret"))?;
    mac.update(body.as_bytes());
    let signature = B64.encode(mac.finalize().into_bytes());
    Ok(format!("{body}.{signature}"))
}

/// Verify and decode a delegation token.
pub fn verify_delegation(
    secret: &[u8],
    token: &str,
    audience: &str,
    now: i64,
) -> Result<DelegationClaims, ApiError> {
    let (body, signature) = token
        .split_once('.')
        .ok_or_else(|| ApiError::new("delegation.invalid", "malformed delegation token"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret)
        .map_err(|_| ApiError::new("delegation.invalid", "invalid signing secret"))?;
    mac.update(body.as_bytes());
    let expected = B64.encode(mac.finalize().into_bytes());
    if expected != signature {
        return Err(ApiError::new(
            "delegation.invalid",
            "delegation signature does not verify",
        ));
    }
    let bytes = B64
        .decode(body)
        .map_err(|_| ApiError::new("delegation.invalid", "malformed delegation body"))?;
    let claims: DelegationClaims = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::new("delegation.invalid", "malformed delegation claims"))?;
    if claims.expires_at <= now {
        return Err(ApiError::new(
            "delegation.expired",
            "delegation token has expired",
        ));
    }
    if claims.audience != audience {
        return Err(ApiError::new(
            "delegation.wrong_audience",
            "delegation token targets another Worker",
        ));
    }
    if claims.depth >= claims.max_depth {
        return Err(ApiError::new(
            "delegation.depth_exceeded",
            "delegation depth limit reached",
        ));
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims() -> DelegationClaims {
        DelegationClaims {
            issuer: "gateway".into(),
            audience: "worker-a".into(),
            principal: "u1".into(),
            namespace: "app".into(),
            scopes: vec!["task:submit".into()],
            capabilities: vec!["echo".into()],
            expires_at: 10_000,
            depth: 0,
            max_depth: 2,
            limits: Limits::default(),
            token_id: "t1".into(),
            team: None,
            path: Vec::new(),
        }
    }

    #[test]
    fn delegation_roundtrip_and_audience_check() {
        let token = issue_delegation(b"secret", &claims()).unwrap();
        assert!(verify_delegation(b"secret", &token, "worker-a", 1).is_ok());
        assert_eq!(
            verify_delegation(b"secret", &token, "worker-b", 1)
                .unwrap_err()
                .code,
            "delegation.wrong_audience"
        );
    }

    #[test]
    fn tampering_is_rejected() {
        let token = issue_delegation(b"secret", &claims()).unwrap();
        let mut tampered = token.clone();
        tampered.push('x');
        assert!(verify_delegation(b"secret", &tampered, "worker-a", 1).is_err());
    }
}
