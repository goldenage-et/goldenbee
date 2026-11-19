//! Provider secret encryption primitives shared by Gateway and Worker.
//!
//! Two independent layers protect a provider API key:
//!
//! 1. At rest, the Gateway encrypts each credential version with a master key
//!    (`GOLDENBEE_VAULT_MASTER_KEY`) using XChaCha20-Poly1305. The associated
//!    data binds the provider id and credential version.
//! 2. In transit, an assigned credential is encrypted to a Worker's X25519
//!    public key with an ephemeral key, HKDF-SHA256, and XChaCha20-Poly1305.
//!    The associated data binds provider, version, Worker, instance, and
//!    envelope expiry so a response cannot be replayed under another identity.
//!
//! No function in this crate logs, formats, or persists a plaintext credential.
//! Callers own transport encoding and storage.

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::fmt;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

/// HKDF context string for a per-Worker provider envelope.
pub const ENVELOPE_INFO: &[u8] = b"goldenbee-provider-key-v1";
/// Associated data for the one-time master-key check.
pub const MASTER_CHECK_AAD: &[u8] = b"goldenbee-vault-master-check-v1";

const MASTER_KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;
const DERIVED_KEY_BYTES: usize = 32;

/// Stable, secret-free crypto failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// A key was not 32 bytes or was not valid base64.
    InvalidKey,
    /// The master key does not match the existing vault document.
    MasterKeyMismatch,
    /// Encryption failed.
    Encrypt,
    /// Decryption failed, including associated-data mismatch and tampering.
    Decrypt,
    /// The derived shared secret was not contributory.
    InvalidContributoryKey,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidKey => "key material is not a valid 32-byte value",
            Self::MasterKeyMismatch => "vault master key does not match the stored document",
            Self::Encrypt => "secret encryption failed",
            Self::Decrypt => "secret decryption failed",
            Self::InvalidContributoryKey => "Worker public key is not usable for an envelope",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CryptoError {}

/// A parsed 32-byte at-rest master key.
///
/// The value is zeroized on drop and never exposed through `Debug`.
pub struct MasterKey(Zeroizing<[u8; MASTER_KEY_BYTES]>);

impl MasterKey {
    /// Parse a base64-encoded 32-byte key.
    pub fn parse(value: &str) -> Result<Self, CryptoError> {
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(value.trim())
            .map_err(|_| CryptoError::InvalidKey)?;
        if decoded.len() != MASTER_KEY_BYTES {
            return Err(CryptoError::InvalidKey);
        }
        let mut key = Zeroizing::new([0u8; MASTER_KEY_BYTES]);
        key.copy_from_slice(&decoded);
        Ok(Self(key))
    }

    /// Generate a new random master key. Intended for development provisioning.
    pub fn generate() -> Self {
        let mut key = Zeroizing::new([0u8; MASTER_KEY_BYTES]);
        OsRng.fill_bytes(key.as_mut());
        Self(key)
    }

    /// Base64 representation. Callers must write it only to a protected file or
    /// process environment, never to Gateway JSON or logs.
    pub fn encode(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(self.0.as_ref())
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MasterKey([REDACTED])")
    }
}

/// One sealed value: a random XChaCha20-Poly1305 nonce and ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSecret {
    /// 24-byte XChaCha20 nonce.
    pub nonce: [u8; NONCE_BYTES],
    /// Ciphertext including the Poly1305 tag.
    pub ciphertext: Vec<u8>,
}

/// One sealed per-Worker envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedEnvelope {
    /// Ephemeral X25519 public key.
    pub ephemeral_public_key: [u8; 32],
    /// 24-byte XChaCha20 nonce.
    pub nonce: [u8; NONCE_BYTES],
    /// Ciphertext including the Poly1305 tag.
    pub ciphertext: Vec<u8>,
}

/// SHA-256 fingerprint of a credential, prefixed with the digest algorithm.
pub fn fingerprint(secret: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(secret)))
}

/// Associated data for an at-rest credential version.
pub fn at_rest_aad(provider: &str, version: u64) -> Vec<u8> {
    format!("goldenbee:provider:v1:{provider}:{version}").into_bytes()
}

/// Associated data for a per-Worker envelope.
pub fn envelope_aad(
    provider: &str,
    version: u64,
    worker: &str,
    instance: &str,
    expires_at: i64,
) -> Vec<u8> {
    format!("goldenbee:envelope:v1:{provider}:{version}:{worker}:{instance}:{expires_at}")
        .into_bytes()
}

/// Encrypt one credential version with the vault master key.
pub fn seal_at_rest(
    master: &MasterKey,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<SealedSecret, CryptoError> {
    let cipher = XChaCha20Poly1305::new_from_slice(master.0.as_ref())
        .map_err(|_| CryptoError::InvalidKey)?;
    let nonce = random_nonce();
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Encrypt)?;
    Ok(SealedSecret { nonce, ciphertext })
}

/// Decrypt one at-rest credential version.
pub fn open_at_rest(
    master: &MasterKey,
    aad: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if nonce.len() != NONCE_BYTES {
        return Err(CryptoError::Decrypt);
    }
    let cipher = XChaCha20Poly1305::new_from_slice(master.0.as_ref())
        .map_err(|_| CryptoError::InvalidKey)?;
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)?;
    Ok(Zeroizing::new(plaintext))
}

/// Store and later verify the master-key check value.
pub fn seal_master_check(master: &MasterKey) -> Result<SealedSecret, CryptoError> {
    seal_at_rest(master, MASTER_CHECK_AAD, b"goldenbee-vault")
}

/// Verify that a master key opens the stored check value.
pub fn verify_master_check(
    master: &MasterKey,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<(), CryptoError> {
    let plaintext = open_at_rest(master, MASTER_CHECK_AAD, nonce, ciphertext)
        .map_err(|_| CryptoError::MasterKeyMismatch)?;
    if plaintext.as_slice() != b"goldenbee-vault" {
        return Err(CryptoError::MasterKeyMismatch);
    }
    Ok(())
}

/// Generate an X25519 private key as raw bytes.
pub fn generate_private_key() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

/// Derive the X25519 public key for a raw private key.
pub fn public_key(private_key: &[u8; 32]) -> [u8; 32] {
    PublicKey::from(&StaticSecret::from(*private_key)).to_bytes()
}

/// Encrypt a credential to a Worker's X25519 public key.
///
/// The ephemeral private key exists only for this call and is zeroized when the
/// function returns.
pub fn seal_envelope(
    recipient_public_key: &[u8; 32],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<SealedEnvelope, CryptoError> {
    let ephemeral = StaticSecret::from(generate_private_key());
    let recipient = PublicKey::from(*recipient_public_key);
    let shared = ephemeral.diffie_hellman(&recipient);
    let key = derive_envelope_key(shared.as_bytes())?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| CryptoError::InvalidKey)?;
    let nonce = random_nonce();
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Encrypt)?;
    Ok(SealedEnvelope {
        ephemeral_public_key: PublicKey::from(&ephemeral).to_bytes(),
        nonce,
        ciphertext,
    })
}

/// Decrypt a per-Worker envelope with the Worker's private key.
pub fn open_envelope(
    private_key: &[u8; 32],
    ephemeral_public_key: &[u8; 32],
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if nonce.len() != NONCE_BYTES {
        return Err(CryptoError::Decrypt);
    }
    let secret = StaticSecret::from(*private_key);
    let ephemeral = PublicKey::from(*ephemeral_public_key);
    let shared = secret.diffie_hellman(&ephemeral);
    let key = derive_envelope_key(shared.as_bytes())?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| CryptoError::InvalidKey)?;
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)?;
    Ok(Zeroizing::new(plaintext))
}

fn derive_envelope_key(
    shared: &[u8; 32],
) -> Result<Zeroizing<[u8; DERIVED_KEY_BYTES]>, CryptoError> {
    if shared.iter().all(|byte| *byte == 0) {
        return Err(CryptoError::InvalidContributoryKey);
    }
    let hkdf = Hkdf::<Sha256>::new(None, shared);
    let mut key = Zeroizing::new([0u8; DERIVED_KEY_BYTES]);
    hkdf.expand(ENVELOPE_INFO, key.as_mut())
        .map_err(|_| CryptoError::InvalidKey)?;
    Ok(key)
}

fn random_nonce() -> [u8; NONCE_BYTES] {
    let mut nonce = [0u8; NONCE_BYTES];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master() -> MasterKey {
        MasterKey::parse("MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=")
            .expect("32-byte base64 fixture")
    }

    #[test]
    fn at_rest_roundtrip_and_aad_binding() {
        let master = master();
        let aad = at_rest_aad("openrouter", 1);
        let sealed = seal_at_rest(&master, &aad, b"sk-test-value").expect("seals");
        let opened = open_at_rest(&master, &aad, &sealed.nonce, &sealed.ciphertext).expect("opens");
        assert_eq!(opened.as_slice(), b"sk-test-value");
        let wrong = at_rest_aad("openrouter", 2);
        assert!(open_at_rest(&master, &wrong, &sealed.nonce, &sealed.ciphertext).is_err());
    }

    #[test]
    fn at_rest_rejects_tamper_and_wrong_master() {
        let master = master();
        let aad = at_rest_aad("openai", 3);
        let mut sealed = seal_at_rest(&master, &aad, b"secret").expect("seals");
        sealed.ciphertext[0] ^= 1;
        assert!(open_at_rest(&master, &aad, &sealed.nonce, &sealed.ciphertext).is_err());

        let good = seal_at_rest(&master, &aad, b"secret").expect("seals");
        let other = MasterKey::generate();
        assert!(open_at_rest(&other, &aad, &good.nonce, &good.ciphertext).is_err());
    }

    #[test]
    fn master_check_fails_for_another_key() {
        let master = master();
        let check = seal_master_check(&master).expect("seals");
        verify_master_check(&master, &check.nonce, &check.ciphertext).expect("matches");
        assert_eq!(
            verify_master_check(&MasterKey::generate(), &check.nonce, &check.ciphertext),
            Err(CryptoError::MasterKeyMismatch)
        );
    }

    #[test]
    fn envelope_opens_only_with_the_intended_worker_key() {
        let worker = generate_private_key();
        let public = public_key(&worker);
        let aad = envelope_aad("openrouter", 4, "general-a", "general-a", 1_000);
        let sealed = seal_envelope(&public, &aad, b"sk-envelope").expect("seals");
        let opened = open_envelope(
            &worker,
            &sealed.ephemeral_public_key,
            &sealed.nonce,
            &sealed.ciphertext,
            &aad,
        )
        .expect("opens");
        assert_eq!(opened.as_slice(), b"sk-envelope");

        let other = generate_private_key();
        assert!(open_envelope(
            &other,
            &sealed.ephemeral_public_key,
            &sealed.nonce,
            &sealed.ciphertext,
            &aad,
        )
        .is_err());
    }

    #[test]
    fn envelope_rejects_wrong_aad_and_tamper() {
        let worker = generate_private_key();
        let public = public_key(&worker);
        let aad = envelope_aad("openrouter", 4, "general-a", "general-a", 1_000);
        let sealed = seal_envelope(&public, &aad, b"sk-envelope").expect("seals");
        let wrong = envelope_aad("openrouter", 4, "general-a", "general-a", 2_000);
        assert!(open_envelope(
            &worker,
            &sealed.ephemeral_public_key,
            &sealed.nonce,
            &sealed.ciphertext,
            &wrong,
        )
        .is_err());

        let mut tampered = sealed.clone();
        tampered.ciphertext[0] ^= 2;
        assert!(open_envelope(
            &worker,
            &tampered.ephemeral_public_key,
            &tampered.nonce,
            &tampered.ciphertext,
            &aad,
        )
        .is_err());
    }

    #[test]
    fn master_key_debug_and_fingerprint_are_secret_safe() {
        let master = master();
        assert!(!format!("{master:?}").contains(&master.encode()));
        let fingerprint = fingerprint(b"sk-test-value");
        assert!(fingerprint.starts_with("sha256:"));
        assert!(!fingerprint.contains("sk-test-value"));
    }
}
