//! Registry document persistence, token hashing, and file permissions.

use std::collections::BTreeMap;
use std::path::Path;

use common::ApiError;

use super::model::Document;

pub(super) fn persistence_error(error: std::io::Error) -> ApiError {
    ApiError::retryable(
        "storage.unavailable",
        format!("registry state could not be persisted: {error}"),
    )
}

#[cfg(unix)]
pub(super) fn sync_parent(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
pub(super) fn sync_parent(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
pub(super) fn build_control(document: &Document) -> Result<BTreeMap<String, String>, ApiError> {
    let mut control = BTreeMap::new();
    for approval in document.approvals.values() {
        if approval.control_token_hash.is_empty() {
            continue;
        }
        if let Some(existing) =
            control.insert(approval.control_token_hash.clone(), approval.id.clone())
        {
            return Err(ApiError::new(
                "config.invalid",
                format!(
                    "control token is bound to both '{existing}' and '{}'",
                    approval.id
                ),
            ));
        }
    }
    Ok(control)
}
pub(super) fn read_document(path: &Path) -> Result<Document, ApiError> {
    let bytes = std::fs::read(path).map_err(|_| {
        ApiError::new(
            "registry.unreadable",
            "registry state file could not be read",
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        ApiError::new("registry.invalid", "registry state is not valid JSON")
            .with("detail", error.to_string())
    })
}

pub(super) fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

pub(super) fn hash_token(token: &str) -> String {
    common::bytes_digest(token.as_bytes())
}
