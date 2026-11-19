//! Component installation and local policy generation.

use crate::download::{self, Remote};
use crate::paths::{release_dir, Paths};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

/// One installable operator component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    /// Gateway HTTP adapter.
    Gateway,
    /// Host-local lifecycle composer.
    Composer,
    /// Independent Client CLI.
    Client,
    /// Gateway and composer.
    All,
}

/// Install controls shared by every component.
#[derive(Debug, Default)]
pub struct InstallOptions {
    /// Source release directory.
    pub from: Option<PathBuf>,
    /// Binary destination override.
    pub bin_dir: Option<PathBuf>,
    /// Configuration destination override.
    pub config_dir: Option<PathBuf>,
    /// State destination override.
    pub data_dir: Option<PathBuf>,
    /// Release tag or version for remote installs.
    pub release: Option<String>,
    /// GitHub repository, `owner/name`.
    pub repo: Option<String>,
    /// Release base URL override.
    pub release_base: Option<String>,
    /// Replace existing binaries and generated config.
    pub force: bool,
    /// Install binaries without generating local configuration.
    pub no_config: bool,
}

/// Install one component and optionally generate local configuration.
pub fn install(component: Component, options: &InstallOptions) -> Result<(), String> {
    let paths = Paths::resolve(
        options.bin_dir.clone(),
        options.config_dir.clone(),
        options.data_dir.clone(),
    )?;
    fs::create_dir_all(&paths.bin)
        .map_err(|_| format!("gb: cannot create {}", paths.bin.display()))?;
    match component {
        Component::Gateway => install_gateway(&paths, options),
        Component::Composer => install_composer(&paths, options),
        Component::Client => install_client(&paths, options),
        Component::All => {
            install_gateway(&paths, options)?;
            install_composer(&paths, options)
        }
    }
}

fn install_gateway(paths: &Paths, options: &InstallOptions) -> Result<(), String> {
    copy_component(paths, options, "gb-gateway")?;
    if !options.no_config {
        install_gateway_config(paths, options.force)?;
    }
    Ok(())
}

fn install_composer(paths: &Paths, options: &InstallOptions) -> Result<(), String> {
    copy_component(paths, options, "gb-composer")?;
    if !options.no_config {
        install_composer_config(paths, options.force)?;
    }
    Ok(())
}

fn install_client(paths: &Paths, options: &InstallOptions) -> Result<(), String> {
    copy_component(paths, options, "gb-client")
}

fn copy_component(paths: &Paths, options: &InstallOptions, name: &str) -> Result<(), String> {
    let destination = paths.bin.join(name);
    if destination.exists() && !options.force {
        println!("gb: kept existing {}", destination.display());
        return Ok(());
    }
    if let Some(source) = local_source(name, options)? {
        if source == destination {
            return Ok(());
        }
        let temporary = destination.with_extension(format!("tmp.{}", std::process::id()));
        fs::copy(&source, &temporary).map_err(|error| {
            format!(
                "gb: cannot copy {} to {}: {error}",
                source.display(),
                temporary.display()
            )
        })?;
        set_mode(&temporary, 0o755)?;
        fs::rename(&temporary, &destination)
            .map_err(|error| format!("gb: cannot install {}: {error}", destination.display()))?;
    } else {
        let remote = Remote {
            repo: options
                .repo
                .clone()
                .or_else(|| std::env::var("GOLDENBEE_REPO").ok())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "goldenage-et/goldenbee".to_string()),
            tag: release_tag(options.release.as_deref()),
            base: options
                .release_base
                .clone()
                .or_else(|| std::env::var("GOLDENBEE_RELEASE_BASE").ok())
                .filter(|value| !value.is_empty()),
            token: std::env::var("GOLDENBEE_GITHUB_TOKEN")
                .ok()
                .filter(|value| !value.is_empty()),
        };
        if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            return Err(
                "gb: remote installs currently support only x86_64-unknown-linux-gnu".to_string(),
            );
        }
        download::install_asset(&remote, &asset_name(name), &destination)?;
    }
    println!("gb: installed {}", destination.display());
    Ok(())
}

fn local_source(name: &str, options: &InstallOptions) -> Result<Option<PathBuf>, String> {
    if let Some(directory) = &options.from {
        return find_in(directory, name).map(Some).ok_or_else(|| {
            format!(
                "gb: {name} or {} was not found under {}",
                asset_name(name),
                directory.display()
            )
        });
    }
    if options.release_base.is_some() || std::env::var_os("GOLDENBEE_RELEASE_BASE").is_some() {
        return Ok(None);
    }
    let mut candidates = Vec::new();
    if let Some(directory) = release_dir() {
        candidates.push(directory);
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            candidates.push(parent.to_path_buf());
        }
    }
    if let Ok(current) = std::env::current_dir() {
        candidates.push(current.join("target/debug"));
        candidates.push(current.join("target/release"));
    }
    Ok(candidates
        .into_iter()
        .find_map(|directory| find_in(&directory, name)))
}

fn find_in(directory: &Path, name: &str) -> Option<PathBuf> {
    [name.to_string(), asset_name(name)]
        .into_iter()
        .map(|candidate| directory.join(candidate))
        .find(|candidate| candidate.is_file())
}

fn asset_name(name: &str) -> String {
    format!("{name}-x86_64-unknown-linux-gnu")
}

fn release_tag(explicit: Option<&str>) -> String {
    let value = explicit
        .map(str::to_string)
        .or_else(|| std::env::var("GOLDENBEE_VERSION").ok())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    if value == "latest" || value.starts_with('v') {
        value
    } else {
        format!("v{value}")
    }
}

fn install_gateway_config(paths: &Paths, force: bool) -> Result<(), String> {
    let token = shared_token(paths)?;
    fs::create_dir_all(paths.config.join("workers"))
        .map_err(|_| format!("gb: cannot create {}", paths.config.display()))?;
    fs::create_dir_all(paths.data.join("gateway"))
        .map_err(|_| format!("gb: cannot create {}", paths.data.display()))?;
    let path = paths.config.join("gateway.json");
    if path.exists() && !force {
        println!("gb: kept existing {}", path.display());
        return Ok(());
    }
    let document = json!({
        "listen": "127.0.0.1:8080",
        "data_dir": paths.data.join("gateway"),
        "registry": paths.data.join("gateway-registry.json"),
        "lease_ttl_ms": 30000,
        "callers": [{
            "token": token,
            "id": "local",
            "namespace": "local",
            "scopes": ["*"]
        }],
        "hosts": [{
            "id": "local",
            "endpoint": "http://127.0.0.1:9090",
            "token": token
        }]
    });
    write_json(&path, &document)?;
    println!("gb: wrote {}", path.display());
    Ok(())
}

fn install_composer_config(paths: &Paths, force: bool) -> Result<(), String> {
    let token = shared_token(paths)?;
    for directory in [
        paths.data.join("instances"),
        paths.data.join("packages"),
        paths.data.join("workspaces"),
        paths.config.join("workers"),
    ] {
        fs::create_dir_all(&directory)
            .map_err(|_| format!("gb: cannot create {}", directory.display()))?;
    }
    let path = paths.config.join("composer.json");
    if path.exists() && !force {
        println!("gb: kept existing {}", path.display());
        return Ok(());
    }
    let document = json!({
        "id": "local",
        "instances_root": paths.data.join("instances"),
        "packages_root": paths.data.join("packages"),
        "operations": ["provision", "inspect", "upgrade", "rollback"],
        "allowed_grants": [
            "task:submit",
            "task:read",
            "task:cancel",
            "session:write",
            "profile:read",
            "profile:write"
        ],
        "allowed_profile_grants": ["task:submit", "task:read"],
        "allowed_config_roots": [paths.config.join("workers")],
        "allowed_workspace_roots": [paths.data.join("workspaces")],
        "allowed_secret_prefixes": ["secret://"],
        "allow_remote_listener": false,
        "callers": [{
            "token": token,
            "gateway": "local",
            "scopes": ["instance:admin"]
        }]
    });
    write_json(&path, &document)?;
    println!("gb: wrote {}", path.display());
    Ok(())
}

fn shared_token(paths: &Paths) -> Result<String, String> {
    fs::create_dir_all(&paths.config)
        .map_err(|_| format!("gb: cannot create {}", paths.config.display()))?;
    let path = paths.config.join("local-token");
    if let Ok(token) = fs::read_to_string(&path) {
        let token = token.trim();
        if !token.is_empty() {
            return Ok(token.to_string());
        }
    }
    if let Some(token) = existing_gateway_token(&paths.config.join("gateway.json")) {
        write_secret(&path, &token)?;
        return Ok(token);
    }
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    write_secret(&path, &token)?;
    Ok(token)
}

fn existing_gateway_token(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("callers")?
        .as_array()?
        .first()?
        .get("token")?
        .as_str()
        .map(str::to_string)
}

fn write_secret(path: &Path, value: &str) -> Result<(), String> {
    fs::write(path, format!("{value}\n"))
        .map_err(|_| format!("gb: cannot write {}", path.display()))?;
    set_mode(path, 0o600)
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| "gb: configuration could not be serialized".to_string())?;
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&temporary, bytes)
        .map_err(|_| format!("gb: cannot write {}", temporary.display()))?;
    set_mode(&temporary, 0o600)?;
    fs::rename(&temporary, path).map_err(|_| format!("gb: cannot install {}", path.display()))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|_| format!("gb: cannot set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_explicit_component_names_the_directory() {
        let directory = tempfile::tempdir().expect("tempdir");
        let options = InstallOptions {
            from: Some(directory.path().to_path_buf()),
            ..InstallOptions::default()
        };
        let error = local_source("gb-gateway", &options).unwrap_err();
        assert!(error.contains("gb-gateway"));
        assert!(error.contains(&directory.path().display().to_string()));
    }

    #[test]
    fn local_source_accepts_target_suffixed_assets() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("gb-gateway-x86_64-unknown-linux-gnu");
        fs::write(&path, "binary").expect("asset");
        let options = InstallOptions {
            from: Some(directory.path().to_path_buf()),
            ..InstallOptions::default()
        };
        assert_eq!(local_source("gb-gateway", &options).unwrap(), Some(path));
    }

    #[test]
    fn release_tags_are_normalized() {
        assert_eq!(release_tag(Some("0.1.0")), "v0.1.0");
        assert_eq!(release_tag(Some("v0.1.0")), "v0.1.0");
        assert_eq!(release_tag(Some("latest")), "latest");
    }

    #[test]
    fn existing_gateway_token_is_reused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gateway.json");
        fs::write(&path, r#"{"callers":[{"token":"existing-token"}]}"#).expect("write");
        assert_eq!(
            existing_gateway_token(&path).as_deref(),
            Some("existing-token")
        );
    }
}
