//! Command-line grammar for the Goldenbee operator bootstrap.

use crate::dispatch;
use crate::install::{self, Component, InstallOptions};
use std::path::PathBuf;
use std::process::ExitCode;

/// Run one parsed invocation.
pub fn run(args: Vec<String>) -> ExitCode {
    match parse(args) {
        Ok(Invocation::Help) => {
            help();
            ExitCode::SUCCESS
        }
        Ok(Invocation::Version) => {
            println!("gb {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Invocation::Install(component, options)) => {
            match install::install(component, &options) {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => {
                    eprintln!("{message}");
                    ExitCode::from(1)
                }
            }
        }
        Ok(Invocation::Gateway(args)) => finish(dispatch::gateway(args)),
        Ok(Invocation::Composer(args)) => finish(dispatch::composer(args)),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

enum Invocation {
    Help,
    Version,
    Install(Component, InstallOptions),
    Gateway(Vec<String>),
    Composer(Vec<String>),
}

fn parse(mut args: Vec<String>) -> Result<Invocation, String> {
    let Some(command) = args.first().cloned() else {
        return Ok(Invocation::Help);
    };
    args.remove(0);
    match command.as_str() {
        "help" | "--help" | "-h" => Ok(Invocation::Help),
        "--version" | "-V" => Ok(Invocation::Version),
        "i" | "install" => {
            if args.iter().any(|arg| arg == "--help" || arg == "-h") {
                return Ok(Invocation::Help);
            }
            let (component, options) = parse_install(args)?;
            Ok(Invocation::Install(component, options))
        }
        "gw" | "gateway" => Ok(Invocation::Gateway(args)),
        "cmp" | "composer" => Ok(Invocation::Composer(args)),
        other => Err(format!("gb: unknown command '{other}'; run `gb --help`")),
    }
}

fn parse_install(args: Vec<String>) -> Result<(Component, InstallOptions), String> {
    let mut args = args.into_iter();
    let Some(target) = args.next() else {
        return Err("gb: install requires gateway, composer, client-cli, or all".to_string());
    };
    let component = match target.as_str() {
        "gateway" | "gw" => Component::Gateway,
        "composer" | "cmp" => Component::Composer,
        "client-cli" | "client" => Component::Client,
        "all" => Component::All,
        other => {
            return Err(format!(
                "gb: unknown install target '{other}'; use gateway, composer, client-cli, or all"
            ))
        }
    };
    let mut options = InstallOptions::default();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--from" => options.from = Some(PathBuf::from(value(&mut args, "--from")?)),
            "--bin-dir" => options.bin_dir = Some(PathBuf::from(value(&mut args, "--bin-dir")?)),
            "--config-dir" => {
                options.config_dir = Some(PathBuf::from(value(&mut args, "--config-dir")?))
            }
            "--data-dir" => options.data_dir = Some(PathBuf::from(value(&mut args, "--data-dir")?)),
            "--release" => options.release = Some(value(&mut args, "--release")?),
            "--repo" => options.repo = Some(value(&mut args, "--repo")?),
            "--release-base" => options.release_base = Some(value(&mut args, "--release-base")?),
            "--force" => options.force = true,
            "--no-config" => options.no_config = true,
            other => return Err(format!("gb: unrecognized install option '{other}'")),
        }
    }
    Ok((component, options))
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("gb: {flag} requires a value"))
}

fn finish(result: Result<i32, String>) -> ExitCode {
    match result {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}

fn help() {
    println!(
        "Usage: gb <command> [options]\n\n\
         Install components:\n  \
         i|install gateway [--from <dir>] [--release <tag>] [--repo <owner/name>] [--release-base <url>] [--bin-dir <dir>] [--config-dir <dir>] [--data-dir <dir>] [--force] [--no-config]\n  \
         i|install composer [same options]\n  \
         i|install client-cli [same options]\n  \
         i|install all [same options]      install Gateway and composer\n\n\
         Dispatch:\n  \
         gw|gateway <gateway args...>      run gb-gateway\n  \
         cmp|composer <composer args...>   run gb-composer\n\n\
         With no `--config`, lifecycle commands use the installed default config.\n\
         Without --from, installs download from GitHub release assets and verify\n\
         SHA256SUMS. The default tag matches this gb version. --from keeps local\n\
         sibling or release-directory installation.\n\n\
         Defaults:\n  \
         binaries  $GOLDENBEE_BIN_DIR or ~/.local/bin\n  \
         config    $GOLDENBEE_CONFIG_DIR or ~/.config/goldenbee\n  \
         state     $GOLDENBEE_DATA_DIR or ~/.local/share/goldenbee\n\n\
         --version"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_targets_parse() {
        assert!(matches!(
            parse(vec!["i".into(), "gateway".into()]),
            Ok(Invocation::Install(Component::Gateway, _))
        ));
        assert!(matches!(
            parse(vec!["install".into(), "composer".into()]),
            Ok(Invocation::Install(Component::Composer, _))
        ));
        assert!(matches!(
            parse(vec!["i".into(), "client-cli".into()]),
            Ok(Invocation::Install(Component::Client, _))
        ));
    }

    #[test]
    fn dispatch_aliases_parse() {
        assert!(matches!(
            parse(vec!["gw".into(), "up".into()]),
            Ok(Invocation::Gateway(args)) if args == ["up"]
        ));
        assert!(matches!(
            parse(vec!["composer".into(), "serve".into()]),
            Ok(Invocation::Composer(args)) if args == ["serve"]
        ));
    }

    #[test]
    fn install_options_parse() {
        let Ok(Invocation::Install(Component::Composer, options)) = parse(vec![
            "i".into(),
            "composer".into(),
            "--from".into(),
            "/release".into(),
            "--release".into(),
            "v0.2.0".into(),
            "--force".into(),
        ]) else {
            panic!("expected composer install");
        };
        assert_eq!(
            options.from.as_deref(),
            Some(std::path::Path::new("/release"))
        );
        assert_eq!(options.release.as_deref(), Some("v0.2.0"));
        assert!(options.force);
    }
}
