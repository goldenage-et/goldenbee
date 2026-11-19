//! Dispatch to installed Gateway and composer executables.

use crate::paths::Paths;
use std::path::{Path, PathBuf};
use std::process::Command;

const GATEWAY_CONFIG_COMMANDS: &[&str] = &[
    "serve",
    "up",
    "status",
    "down",
    "pair",
    "backup",
    "backup-verify",
    "restore",
];

/// Execute `gb-gateway` with the supplied arguments.
pub fn gateway(args: Vec<String>) -> Result<i32, String> {
    let paths = Paths::resolve(None, None, None)?;
    let binary = resolve("gb-gateway", "GOLDENBEE_GATEWAY_BIN", &paths.bin)?;
    let config = paths.config.join("gateway.json");
    let args = inject_config(args, &config, GATEWAY_CONFIG_COMMANDS);
    exec(&binary, &args)
}

/// Execute `gb-composer` with the supplied arguments.
pub fn composer(args: Vec<String>) -> Result<i32, String> {
    let paths = Paths::resolve(None, None, None)?;
    let binary = resolve("gb-composer", "GOLDENBEE_COMPOSER_BIN", &paths.bin)?;
    let config = paths.config.join("composer.json");
    let args = inject_config(args, &config, &["serve"]);
    exec(&binary, &args)
}

fn resolve(name: &str, env_name: &str, bin_dir: &Path) -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os(env_name) {
        if !value.is_empty() {
            return Ok(PathBuf::from(value));
        }
    }
    let installed = bin_dir.join(name);
    if installed.is_file() {
        return Ok(installed);
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            let sibling = parent.join(name);
            if sibling.is_file() {
                return Ok(sibling);
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(format!(
        "gb: {name} is not installed; run `gb i {}`",
        if name == "gb-gateway" {
            "gateway"
        } else {
            "composer"
        }
    ))
}

fn inject_config(mut args: Vec<String>, config: &Path, commands: &[&str]) -> Vec<String> {
    if args
        .iter()
        .any(|arg| arg == "--config" || arg.starts_with("--config="))
    {
        return args;
    }
    let Some(command) = args.first().map(String::as_str) else {
        return args;
    };
    if commands.contains(&command) {
        args.push("--config".to_string());
        args.push(config.display().to_string());
    }
    args
}

#[cfg(unix)]
fn exec(binary: &Path, args: &[String]) -> Result<i32, String> {
    use std::os::unix::process::CommandExt;
    let error = Command::new(binary).args(args).exec();
    Err(format!("gb: cannot execute {}: {error}", binary.display()))
}

#[cfg(not(unix))]
fn exec(binary: &Path, args: &[String]) -> Result<i32, String> {
    let status = Command::new(binary)
        .args(args)
        .status()
        .map_err(|error| format!("gb: cannot execute {}: {error}", binary.display()))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_config_is_injected_for_lifecycle_commands() {
        let args = inject_config(
            vec!["up".to_string()],
            Path::new("/tmp/gateway.json"),
            GATEWAY_CONFIG_COMMANDS,
        );
        assert_eq!(args, ["up", "--config", "/tmp/gateway.json"]);
    }

    #[test]
    fn explicit_config_is_not_duplicated() {
        let args = inject_config(
            vec![
                "up".to_string(),
                "--config".to_string(),
                "/custom.json".to_string(),
            ],
            Path::new("/tmp/gateway.json"),
            GATEWAY_CONFIG_COMMANDS,
        );
        assert_eq!(args, ["up", "--config", "/custom.json"]);
    }

    #[test]
    fn provider_commands_do_not_get_a_config() {
        let args = inject_config(
            vec!["provider".to_string(), "list".to_string()],
            Path::new("/tmp/gateway.json"),
            GATEWAY_CONFIG_COMMANDS,
        );
        assert_eq!(args, ["provider", "list"]);
    }
}
