//! Release asset download and checksum verification.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One resolved download source.
#[derive(Debug, Clone)]
pub struct Remote {
    /// GitHub repository, `owner/name`.
    pub repo: String,
    /// Release tag, or `latest`.
    pub tag: String,
    /// Optional release-base override.
    pub base: Option<String>,
    /// Optional GitHub token for a private release.
    pub token: Option<String>,
}

impl Remote {
    fn asset_url(&self, asset: &str) -> String {
        match &self.base {
            Some(base) => format!("{}/{}", base.trim_end_matches('/'), asset),
            None if self.tag == "latest" => format!(
                "https://github.com/{}/releases/latest/download/{asset}",
                self.repo
            ),
            None => format!(
                "https://github.com/{}/releases/download/{}/{}",
                self.repo, self.tag, asset
            ),
        }
    }

    fn sums_url(&self) -> String {
        self.asset_url("SHA256SUMS")
    }
}

/// Download, verify, and install one release asset.
pub fn install_asset(remote: &Remote, asset: &str, destination: &Path) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| "gb: install destination has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|_| format!("gb: cannot create {}", parent.display()))?;
    let temporary = temp_path(destination, asset);
    let _ = fs::remove_file(&temporary);
    download(
        &remote.asset_url(asset),
        &temporary,
        remote.token.as_deref(),
    )?;
    let sums_path = temp_path(destination, "SHA256SUMS");
    let _ = fs::remove_file(&sums_path);
    if let Err(error) = download(&remote.sums_url(), &sums_path, remote.token.as_deref()) {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(&sums_path);
        return Err(error);
    }
    let sums = fs::read_to_string(&sums_path)
        .map_err(|_| "gb: release checksum file could not be read".to_string());
    let sums = match sums {
        Ok(sums) => sums,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            let _ = fs::remove_file(&sums_path);
            return Err(error);
        }
    };
    let expected = checksum_for(&sums, asset)
        .ok_or_else(|| format!("gb: SHA256SUMS does not contain {asset}"));
    let expected = match expected {
        Ok(expected) => expected,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            let _ = fs::remove_file(&sums_path);
            return Err(error);
        }
    };
    let actual = sha256_file(&temporary)?;
    let _ = fs::remove_file(&sums_path);
    if !actual.eq_ignore_ascii_case(&expected) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "gb: checksum mismatch for {asset}: expected {expected}, got {actual}"
        ));
    }
    set_mode(&temporary, 0o755)?;
    fs::rename(&temporary, destination)
        .map_err(|_| format!("gb: cannot install {}", destination.display()))?;
    Ok(())
}

fn download(url: &str, destination: &Path, token: Option<&str>) -> Result<(), String> {
    let mut command = Command::new("curl");
    command.args([
        "--fail",
        "--location",
        "--silent",
        "--show-error",
        "--retry",
        "3",
        "--output",
    ]);
    command.arg(destination);
    if let Some(token) = token {
        command.arg("--header");
        command.arg(format!("Authorization: Bearer {token}"));
    }
    command.arg(url);
    let status = command.status().map_err(|_| {
        "gb: curl is required to install from GitHub; install curl or use --from <dir>".to_string()
    })?;
    if !status.success() {
        return Err(format!(
            "gb: download failed for {}; check the release tag and network",
            url
        ));
    }
    Ok(())
}

fn checksum_for(text: &str, asset: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == asset).then(|| hash.to_string())
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|_| format!("gb: cannot read downloaded {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| format!("gb: cannot read downloaded {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn temp_path(destination: &Path, asset: &str) -> PathBuf {
    destination.with_file_name(format!(".{asset}.download.{}", std::process::id()))
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
    fn asset_urls_use_latest_or_pinned_tag() {
        let latest = Remote {
            repo: "owner/repo".into(),
            tag: "latest".into(),
            base: None,
            token: None,
        };
        assert_eq!(
            latest.asset_url("gb"),
            "https://github.com/owner/repo/releases/latest/download/gb"
        );
        let pinned = Remote {
            tag: "v0.1.0".into(),
            ..latest
        };
        assert_eq!(
            pinned.asset_url("gb"),
            "https://github.com/owner/repo/releases/download/v0.1.0/gb"
        );
    }

    #[test]
    fn checksums_accept_gnu_and_binary_names() {
        let text = "abc  gb\nfed  *gb-gateway\n";
        assert_eq!(checksum_for(text, "gb").as_deref(), Some("abc"));
        assert_eq!(checksum_for(text, "gb-gateway").as_deref(), Some("fed"));
        assert!(checksum_for(text, "missing").is_none());
    }
}
