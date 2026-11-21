//! Plugin content digest and trust verification.
//!
//! Goldenbee's trust source is explicit: an operator-configured Ed25519 signer,
//! or an operator allowlist pinning the plugin id to a content digest. A plugin
//! that has neither fails closed. The content digest binds every bundled file,
//! so a changed skill, command, hook script, or MCP declaration is detected.

use crate::plugins::manifest::{PluginManifest, PLUGIN_MANIFEST_FILE};
use common::ApiError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Operator-selected plugin policy.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginsConfig {
    /// Directory containing one subdirectory per plugin bundle.
    #[serde(default)]
    pub directory: Option<String>,
    /// Operator allowlist: plugin id to `*` or an exact content digest.
    #[serde(default)]
    pub allow: BTreeMap<String, String>,
    /// Trusted signer identities and base64 Ed25519 public keys.
    #[serde(default)]
    pub trusted_signers: BTreeMap<String, String>,
}

/// Recorded trust decision for one plugin bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustRecord {
    /// `signature` or `allowlist`.
    pub source: String,
    /// Signer identity, or `operator` for an allowlisted bundle.
    pub signer: String,
    /// SHA-256 content digest.
    pub digest: String,
    /// Canonical manifest digest the signature covered.
    pub manifest_digest: String,
}

/// Verify one bundle and return its recorded trust.
pub fn verify(
    manifest: &PluginManifest,
    root: &Path,
    config: &PluginsConfig,
) -> Result<TrustRecord, ApiError> {
    let digest = content_digest(root)?;
    let manifest_digest = manifest.signing_digest();
    if let Some(trust) = &manifest.trust {
        let signer = trust.signer.as_deref().ok_or_else(|| {
            ApiError::new(
                "plugin.trust_incomplete",
                format!(
                    "plugin '{}' declares trust metadata without a signer; add [trust].signer or remove the section",
                    manifest.name
                ),
            )
        })?;
        let signature = trust.signature.as_deref().ok_or_else(|| {
            ApiError::new(
                "plugin.trust_incomplete",
                format!(
                    "plugin '{}' declares signer '{signer}' without a signature",
                    manifest.name
                ),
            )
        })?;
        let key = config.trusted_signers.get(signer).ok_or_else(|| {
            ApiError::new(
                "plugin.untrusted",
                format!(
                    "plugin '{}' signer '{signer}' is not in plugins.trusted_signers",
                    manifest.name
                ),
            )
        })?;
        let expected = trust.digest.as_deref().ok_or_else(|| {
            ApiError::new(
                "plugin.trust_incomplete",
                format!(
                    "signed plugin '{}' must declare [trust].digest so bundled files are bound",
                    manifest.name
                ),
            )
        })?;
        if expected != digest {
            return Err(ApiError::new(
                "plugin.tampered",
                format!(
                    "plugin '{}' content digest does not match [trust].digest; reinstall the bundle or update the operator pin",
                    manifest.name
                ),
            ));
        }
        crate::crypto::verify_release_signature(key, &manifest_digest, signature).map_err(
            |_| {
                ApiError::new(
                    "plugin.signature_invalid",
                    format!(
                        "plugin '{}' signature does not verify against signer '{signer}'",
                        manifest.name
                    ),
                )
            },
        )?;
        return Ok(TrustRecord {
            source: "signature".into(),
            signer: signer.to_string(),
            digest,
            manifest_digest,
        });
    }
    let allowed = config.allow.get(&manifest.name).ok_or_else(|| {
        ApiError::new(
            "plugin.untrusted",
            format!(
                "plugin '{}' has no signature and is not in plugins.allow; refusing to activate it",
                manifest.name
            ),
        )
    })?;
    if allowed != "*" && allowed != &digest {
        return Err(ApiError::new(
            "plugin.tampered",
            format!(
                "plugin '{}' content digest does not match the operator allowlist pin",
                manifest.name
            ),
        ));
    }
    Ok(TrustRecord {
        source: "allowlist".into(),
        signer: "operator".into(),
        digest,
        manifest_digest,
    })
}

/// SHA-256 digest of every bundled file except `plugin.toml`.
///
/// Relative paths and sorted traversal make the digest deterministic. A
/// symlink that resolves outside the plugin root is rejected.
pub fn content_digest(root: &Path) -> Result<String, ApiError> {
    let root = root.canonicalize().map_err(|error| {
        ApiError::new(
            "plugin.directory_invalid",
            format!(
                "plugin directory '{}' is not readable: {error}",
                root.display()
            ),
        )
    })?;
    let mut files = Vec::new();
    collect_files(&root, &root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for (relative, path) in files {
        if relative == Path::new(PLUGIN_MANIFEST_FILE) {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            ApiError::new(
                "plugin.asset_unreadable",
                format!("cannot read plugin asset '{}': {error}", path.display()),
            )
        })?;
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(format!("sha256:{}", hex(&hasher.finalize())))
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), ApiError> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        ApiError::new(
            "plugin.asset_unreadable",
            format!(
                "cannot read plugin directory '{}': {error}",
                directory.display()
            ),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            ApiError::new(
                "plugin.asset_unreadable",
                format!(
                    "cannot read plugin directory '{}': {error}",
                    directory.display()
                ),
            )
        })?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            ApiError::new(
                "plugin.asset_unreadable",
                format!("cannot inspect plugin asset '{}': {error}", path.display()),
            )
        })?;
        if metadata.file_type().is_symlink() {
            let resolved = path.canonicalize().map_err(|error| {
                ApiError::new(
                    "plugin.asset_unreadable",
                    format!(
                        "cannot resolve plugin symlink '{}': {error}",
                        path.display()
                    ),
                )
            })?;
            if !resolved.starts_with(root) {
                return Err(ApiError::new(
                    "plugin.path_denied",
                    format!(
                        "plugin asset '{}' resolves outside the plugin directory",
                        path.display()
                    ),
                ));
            }
            if resolved.is_file() {
                add_file(root, &path, &resolved, files)?;
            } else if resolved.is_dir() {
                return Err(ApiError::new(
                    "plugin.path_denied",
                    format!(
                        "plugin asset '{}' is a symlinked directory; bundle directories must live inside the plugin root",
                        path.display()
                    ),
                ));
            }
            continue;
        }
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            add_file(root, &path, &path, files)?;
        }
    }
    Ok(())
}

fn add_file(
    root: &Path,
    observed: &Path,
    resolved: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), ApiError> {
    let relative = observed.strip_prefix(root).map_err(|_| {
        ApiError::new(
            "plugin.path_denied",
            format!(
                "plugin asset '{}' is outside the plugin directory",
                observed.display()
            ),
        )
    })?;
    files.push((relative.to_path_buf(), resolved.to_path_buf()));
    Ok(())
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
