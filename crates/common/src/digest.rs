//! Canonical contract digests.

use serde::Serialize;

/// Canonical JSON digest scoped to the contract representation.
pub fn canonical_digest<T: Serialize>(value: &T) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let hash = Sha256::digest(bytes);
    format!("sha256:{}", hex(&hash))
}

/// SHA-256 of bytes as `sha256:<hex>`.
pub fn bytes_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
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
