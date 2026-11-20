//! Package installation into the Gateway-owned verified cache.

use super::{install_error, HostManager};
use common::{
    ApiError, PackageInstallOutcome, PackageInstallRequest, PackageList, PackageSource,
    PackageSummary,
};
use install::{release, Installer, TrustPolicy};
use std::path::{Path, PathBuf};

struct PreparedSource {
    root: PathBuf,
    _temporary: Option<tempfile::TempDir>,
}

pub(super) async fn install(
    host: &HostManager,
    request: PackageInstallRequest,
) -> Result<PackageInstallOutcome, ApiError> {
    let policy = host.trust_policy()?;
    let prepared = prepare_source(host, &request.source).await?;
    let artifact = resolve_artifact(&prepared.root, request.package.as_deref(), &policy)?;
    let installer = Installer::new(host.packages_root(), policy);
    let inspection = installer.inspect(&artifact).map_err(install_error)?;
    if let Some(name) = &request.package {
        if &inspection.metadata.package.name != name {
            return Err(ApiError::new(
                "package.not_found",
                format!(
                    "source contains package '{}' instead of '{name}'",
                    inspection.metadata.package.name
                ),
            ));
        }
    }
    if let Some(expected) = &request.expected_digest {
        if !install::is_digest(expected) {
            return Err(ApiError::new(
                "package.digest_invalid",
                "expected_digest must be a sha256 digest",
            ));
        }
        if &inspection.artifact_digest != expected {
            return Err(ApiError::new(
                "package.digest_mismatch",
                "the candidate package digest does not match expected_digest",
            )
            .with("expected", expected.clone())
            .with("actual", inspection.artifact_digest.clone()));
        }
    }
    let outcome = installer.install(&artifact).await.map_err(install_error)?;
    let installed = outcome.release();
    Ok(PackageInstallOutcome {
        package: PackageSummary {
            package: installed.package.clone(),
            version: installed.version.clone(),
            artifact_digest: installed.artifact_digest.clone(),
            signer: installed.signer.clone(),
            target: installed.target.clone(),
        },
        installed_now: outcome.installed_now(),
    })
}

pub(super) fn list(host: &HostManager) -> Result<PackageList, ApiError> {
    let installed = Installer::new(host.packages_root(), TrustPolicy::default())
        .installed()
        .map_err(install_error)?;
    Ok(PackageList {
        packages: installed
            .into_iter()
            .map(|installed| PackageSummary {
                package: installed.package,
                version: installed.version,
                artifact_digest: installed.artifact_digest,
                signer: installed.signer,
                target: installed.target,
            })
            .collect(),
    })
}

async fn prepare_source(
    host: &HostManager,
    source: &PackageSource,
) -> Result<PreparedSource, ApiError> {
    match source {
        PackageSource::Folder { path } => Ok(PreparedSource {
            root: host.package_source(path)?,
            _temporary: None,
        }),
        PackageSource::Url { url } => {
            if !url.starts_with("https://") && !url.starts_with("http://") {
                return Err(ApiError::new(
                    "host.package_source_invalid",
                    "package URLs must use HTTP or HTTPS",
                ));
            }
            let response = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .map_err(|_| {
                    ApiError::new(
                        "host.package_download_failed",
                        "the package download client could not start",
                    )
                })?
                .get(url)
                .send()
                .await
                .map_err(|_| {
                    ApiError::new(
                        "host.package_download_failed",
                        "the package archive could not be downloaded",
                    )
                })?;
            if !response.status().is_success() {
                return Err(ApiError::new(
                    "host.package_download_failed",
                    "the package archive URL returned an error",
                )
                .with("status", response.status().to_string()));
            }
            let bytes = response.bytes().await.map_err(|_| {
                ApiError::new(
                    "host.package_download_failed",
                    "the package archive body ended early",
                )
            })?;
            let temporary = tempfile::tempdir().map_err(|_| {
                ApiError::new(
                    "host.storage_unavailable",
                    "a temporary package directory could not be created",
                )
            })?;
            extract_archive(&bytes, temporary.path())?;
            let root = unwrap_single_directory(temporary.path());
            Ok(PreparedSource {
                root,
                _temporary: Some(temporary),
            })
        }
    }
}

fn resolve_artifact(
    root: &Path,
    package: Option<&str>,
    policy: &TrustPolicy,
) -> Result<PathBuf, ApiError> {
    if root.join("release-manifest.json").is_file() {
        let bytes = std::fs::read(root.join("release-manifest.json")).map_err(|_| {
            ApiError::new(
                "release.manifest_unavailable",
                "release manifest is unreadable",
            )
        })?;
        let manifest: release::ReleaseManifest = serde_json::from_slice(&bytes).map_err(|_| {
            ApiError::new("release.manifest_invalid", "release manifest is malformed")
        })?;
        let trusted = policy
            .trusted_signers
            .get(&manifest.signer)
            .ok_or_else(|| {
                ApiError::new(
                    "install.signer_untrusted",
                    format!("signer '{}' is not trusted", manifest.signer),
                )
            })?;
        if trusted.trim() != manifest.signer_public_key.trim() {
            return Err(ApiError::new(
                "release.signature_invalid",
                "release signer key differs from the trusted policy key",
            ));
        }
        release::verify(root, trusted).map_err(install_error)?;
        let selected = match package {
            Some(name) => manifest
                .packages
                .iter()
                .find(|candidate| candidate.name == name)
                .ok_or_else(|| {
                    ApiError::new(
                        "package.not_found",
                        format!("release does not contain package '{name}'"),
                    )
                })?,
            None if manifest.packages.len() == 1 => &manifest.packages[0],
            None => {
                return Err(ApiError::new(
                    "package.selection_required",
                    "release contains multiple packages; select one with --package",
                ))
            }
        };
        return Ok(root.join(&selected.directory));
    }
    if root.join("index.json").is_file() && root.join("oci-layout").is_file() {
        return Ok(root.to_path_buf());
    }
    Err(ApiError::new(
        "host.package_source_invalid",
        "package source is neither a release root nor a package artifact",
    ))
}

fn extract_archive(bytes: &[u8], destination: &Path) -> Result<(), ApiError> {
    if bytes.len() < 2 || bytes[0] != 0x1f || bytes[1] != 0x8b {
        return Err(ApiError::new(
            "host.package_source_unsupported",
            "package URL must return a gzip-compressed tar archive",
        ));
    }
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().map_err(|_| {
        ApiError::new(
            "host.package_source_invalid",
            "package archive could not be read",
        )
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|_| {
            ApiError::new(
                "host.package_source_invalid",
                "package archive entry could not be read",
            )
        })?;
        let path = entry.path().map_err(|_| {
            ApiError::new(
                "host.package_source_invalid",
                "package archive contains an invalid path",
            )
        })?;
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(ApiError::new(
                "host.package_source_invalid",
                "package archive contains a path traversal",
            ));
        }
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            return Err(ApiError::new(
                "host.package_source_invalid",
                "package archive contains an unsupported link",
            ));
        }
        entry.unpack_in(destination).map_err(|_| {
            ApiError::new(
                "host.package_source_invalid",
                "package archive entry could not be extracted",
            )
        })?;
    }
    Ok(())
}

fn unwrap_single_directory(root: &Path) -> PathBuf {
    let Ok(entries) = std::fs::read_dir(root) else {
        return root.to_path_buf();
    };
    let mut directories = Vec::new();
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            directories.push(entry.path());
        } else {
            return root.to_path_buf();
        }
    }
    if directories.len() == 1 {
        directories.remove(0)
    } else {
        root.to_path_buf()
    }
}
