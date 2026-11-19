//! Minimal release-signing primitives.
//!
//! Delegation claims and their HMAC token format live in [`common`] because
//! both the Gateway and the Worker need them. This module re-exports those
//! symbols for existing Worker callers and keeps release signature verification
//! with the Worker-side runtime.

pub use common::delegation::{issue_delegation, verify_delegation, DelegationClaims};

use base64::Engine;
use common::ApiError;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Verify a release manifest signature against a trusted signer key.
pub fn verify_release_signature(
    public_key_b64: &str,
    manifest_digest: &str,
    signature_b64: &str,
) -> Result<(), ApiError> {
    let key_bytes = B64
        .decode(public_key_b64)
        .map_err(|_| ApiError::new("release.signer_invalid", "trusted key is not base64"))?;
    let key_bytes: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| ApiError::new("release.signer_invalid", "trusted key has wrong length"))?;
    let key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| ApiError::new("release.signer_invalid", "trusted key is invalid"))?;
    let signature_bytes = B64
        .decode(signature_b64)
        .map_err(|_| ApiError::new("release.signature_invalid", "signature is not base64"))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| ApiError::new("release.signature_invalid", "signature has wrong length"))?;
    key.verify(manifest_digest.as_bytes(), &signature)
        .map_err(|_| ApiError::new("release.signature_invalid", "signature does not verify"))
}
