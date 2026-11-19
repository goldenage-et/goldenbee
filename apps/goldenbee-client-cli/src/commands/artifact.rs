//! Task artifact inspection and verified download.

use crate::cli::output::{api_error, json};
use sdk::Client;
use std::process::ExitCode;

/// Artifact commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactCommand {
    /// List task artifacts.
    List {
        /// Task id.
        task: String,
    },
    /// Show one artifact.
    Show {
        /// Task id.
        task: String,
        /// Artifact id.
        artifact: String,
    },
    /// Download one artifact.
    Download {
        /// Task id.
        task: String,
        /// Artifact id.
        artifact: String,
        /// Output path.
        output: String,
        /// Replace an existing path.
        force: bool,
    },
}

pub(crate) fn artifact_view(artifact: &common::Artifact) {
    println!(
        "{} name={} type={} size={} digest={} finalized={}",
        artifact.id,
        artifact.name,
        artifact.content_type,
        artifact.size,
        artifact.digest,
        artifact.finalized
    );
}

pub(crate) async fn artifact_command(
    command: ArtifactCommand,
    json_output: bool,
    client: &Client,
) -> ExitCode {
    match command {
        ArtifactCommand::List { task } => match client.artifacts(&task).await {
            Ok(artifacts) => {
                if json_output {
                    return match json(&artifacts) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                if artifacts.is_empty() {
                    println!("no artifacts");
                }
                for artifact in &artifacts {
                    artifact_view(artifact);
                }
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        ArtifactCommand::Show { task, artifact } => match client.artifact(&task, &artifact).await {
            Ok(artifact) => {
                if json_output {
                    return match json(&artifact) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(code) => code,
                    };
                }
                artifact_view(&artifact);
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
        ArtifactCommand::Download {
            task,
            artifact,
            output,
            force,
        } => match client.download_artifact(&task, &artifact).await {
            Ok(download) => {
                if let Err(message) = write_artifact(&output, &download.bytes, force) {
                    eprintln!("gb-client: artifact.output: {message}");
                    return ExitCode::from(1);
                }
                println!(
                    "wrote {} bytes to {} ({})",
                    download.bytes.len(),
                    output,
                    download.artifact.digest
                );
                ExitCode::SUCCESS
            }
            Err(error) => api_error(error),
        },
    }
}

pub(crate) fn write_artifact(path: &str, bytes: &[u8], force: bool) -> Result<(), String> {
    use std::io::Write;
    let path = std::path::Path::new(path);
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err("refusing to write through a symbolic link".into());
        }
        if !force {
            return Err("path already exists; pass --force to replace it".into());
        }
    }
    let directory = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "output path has no file name".to_string())?;
    let temporary = directory.join(format!(".{name}.tmp-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("cannot write {}: {error}", temporary.display()));
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("cannot replace {}: {error}", path.display()));
    }
    Ok(())
}
