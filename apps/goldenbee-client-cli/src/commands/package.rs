//! Package cache installation and inspection.

use crate::cli::output::{api_error, json};
use common::{PackageInstallRequest, PackageSource};
use sdk::Client;
use std::process::ExitCode;

/// Package commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageCommand {
    /// Install one package from a folder or URL.
    Install {
        /// Folder path or HTTP(S) archive URL.
        source: String,
        /// Package name when the source contains more than one.
        package: Option<String>,
        /// Expected immutable digest.
        digest: Option<String>,
    },
    /// List verified packages.
    List,
}

pub(crate) async fn package_command(
    command: PackageCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        PackageCommand::Install {
            source,
            package,
            digest,
        } => {
            let request = PackageInstallRequest {
                source: if source.starts_with("http://") || source.starts_with("https://") {
                    PackageSource::Url { url: source }
                } else {
                    PackageSource::Folder { path: source }
                },
                package,
                expected_digest: digest,
            };
            match client.install_package(&request).await {
                Ok(outcome) => {
                    if json_output {
                        return match json(&outcome) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(code) => code,
                        };
                    }
                    println!(
                        "{} {} digest={} signer={} target={} installed_now={}",
                        outcome.package.package,
                        outcome.package.version,
                        outcome.package.artifact_digest,
                        outcome.package.signer,
                        outcome.package.target,
                        outcome.installed_now
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => api_error(error),
            }
        }
        PackageCommand::List => match client.packages().await {
            Ok(list) => {
                if json_output {
                    return match json(&list) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                for package in list.packages {
                    println!(
                        "{} {} digest={} signer={} target={}",
                        package.package,
                        package.version,
                        package.artifact_digest,
                        package.signer,
                        package.target
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
    }
}
