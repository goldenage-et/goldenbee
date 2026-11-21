//! Worker-defined command assets serialized as Gemini-style TOML commands.
//!
//! One file is one command:
//!
//! ```toml
//! description = "Summarize the requested topic"
//! prompt = "Summarize {{args}} for an operator audience."
//! allowed_tools = ["read_file"]
//! model = "fast"
//!
//! [[arguments]]
//! name = "audience"
//! default = "operator"
//! ```
//!
//! The file stem becomes the command name; a file under a subdirectory uses
//! Gemini-style path namespacing (`review/security.toml` -> `review:security`).
//! Prompt shell interpolation (`!{...}`) is rejected: a command asset cannot
//! run a host command outside the sandbox and approval path.

use common::{ApiError, CommandArgument, CommandDescriptor};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::worker::assets::{canonical_dir, read_dir, safe_file};

/// Maximum accepted command prompt bytes.
const MAX_PROMPT_BYTES: usize = 65_536;

/// One TOML command asset before cross-asset validation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandFile {
    /// Explicit command name. Omitted names derive from the file path.
    #[serde(default)]
    pub name: Option<String>,
    /// Operator-facing description.
    #[serde(default)]
    pub description: String,
    /// Prompt template with `{{args}}` or declared placeholders.
    pub prompt: String,
    /// Tools the command narrows the target agent to.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Declared model reference.
    #[serde(default)]
    pub model: Option<String>,
    /// Agent the command targets. Omitted uses the Worker default agent.
    #[serde(default)]
    pub agent: Option<String>,
    /// Declared arguments.
    #[serde(default)]
    pub arguments: Vec<ArgumentFile>,
}

/// One declared command argument.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArgumentFile {
    /// Stable argument name.
    pub name: String,
    /// Operator-facing description.
    #[serde(default)]
    pub description: Option<String>,
    /// Whether the caller must supply the argument.
    #[serde(default)]
    pub required: bool,
    /// Value applied when the caller omits the argument.
    #[serde(default)]
    pub default: Option<String>,
}

/// One loaded, structurally validated command asset.
#[derive(Debug, Clone)]
pub(crate) struct CommandAsset {
    /// Canonical command name.
    pub name: String,
    /// Operator-facing description.
    pub description: String,
    /// Prompt template.
    pub prompt: String,
    /// Declared tool narrowing, empty for inherit.
    pub allowed_tools: Vec<String>,
    /// Declared model reference.
    pub model: Option<String>,
    /// Target agent id before default resolution.
    pub agent: Option<String>,
    /// Declared arguments in prompt order.
    pub arguments: Vec<CommandArgument>,
}

impl CommandAsset {
    /// Wire descriptor for discovery.
    pub(crate) fn descriptor(&self) -> CommandDescriptor {
        CommandDescriptor {
            name: self.name.clone(),
            description: self.description.clone(),
            kind: common::command_kinds::WORKER.into(),
            source: "assets".into(),
            arguments: self.arguments.clone(),
            allowed_tools: self.allowed_tools.clone(),
            model: self.model.clone(),
        }
    }

    /// Placeholder names this prompt may reference.
    pub(crate) fn placeholders(prompt: &str) -> Result<BTreeSet<String>, ApiError> {
        let mut found = BTreeSet::new();
        let mut rest = prompt;
        while let Some(start) = rest.find("{{") {
            let after = &rest[start + 2..];
            let end = after.find("}}").ok_or_else(|| {
                ApiError::new(
                    "asset.command_placeholder_invalid",
                    "command prompt has an unterminated '{{' placeholder",
                )
            })?;
            let name = after[..end].trim();
            if name.is_empty() || name.contains('{') || name.contains('}') {
                return Err(ApiError::new(
                    "asset.command_placeholder_invalid",
                    "command prompt has an empty or nested placeholder",
                ));
            }
            found.insert(name.to_string());
            rest = &after[end + 2..];
        }
        Ok(found)
    }
}

/// Load every command asset under the configured Worker and plugin directories.
pub(crate) fn load(dirs: &[PathBuf]) -> Result<Vec<CommandAsset>, ApiError> {
    let mut assets: Vec<CommandAsset> = Vec::new();
    for dir in dirs {
        load_into(dir, &mut assets)?;
    }
    assets.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(assets)
}

fn load_into(dir: &Path, assets: &mut Vec<CommandAsset>) -> Result<(), ApiError> {
    let root = canonical_dir(dir, "command")?;
    let mut pending = vec![(root.clone(), Vec::<String>::new())];
    while let Some((directory, namespace)) = pending.pop() {
        for path in read_dir(&directory, "command")? {
            if path.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        ApiError::new(
                            "asset.command_invalid",
                            format!("command directory '{}' has no valid name", path.display()),
                        )
                    })?;
                validate_component(name, &path)?;
                if namespace.len() >= 4 {
                    return Err(ApiError::new(
                        "asset.command_invalid",
                        format!(
                            "command directory '{}' is nested deeper than four levels",
                            path.display()
                        ),
                    ));
                }
                let mut next = namespace.clone();
                next.push(name.to_string());
                pending.push((path, next));
                continue;
            }
            if !path.is_file() {
                continue;
            }
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if extension != "toml" {
                continue;
            }
            let safe = safe_file(&path, &root, "command")?;
            let content = std::fs::read_to_string(&safe).map_err(|error| {
                ApiError::new(
                    "asset.command_unreadable",
                    format!("cannot read command '{}': {error}", path.display()),
                )
            })?;
            let file: CommandFile = toml::from_str(&content).map_err(|error| {
                ApiError::new(
                    "asset.command_malformed",
                    format!("command '{}' is invalid TOML: {error}", path.display()),
                )
            })?;
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| {
                    ApiError::new(
                        "asset.command_invalid",
                        format!("command file '{}' has no valid name", path.display()),
                    )
                })?;
            let derived = namespace
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(stem))
                .collect::<Vec<_>>()
                .join(":");
            let name = file.name.clone().unwrap_or(derived);
            validate_name(&name, &path)?;
            if assets.iter().any(|asset| asset.name == name) {
                return Err(ApiError::new(
                    "asset.command_duplicate",
                    format!("command '{name}' is defined more than once"),
                ));
            }
            assets.push(resolve(file, name, safe)?);
        }
    }
    Ok(())
}

fn resolve(file: CommandFile, name: String, _path: PathBuf) -> Result<CommandAsset, ApiError> {
    if file.prompt.trim().is_empty() {
        return Err(ApiError::new(
            "asset.command_prompt_required",
            format!("command '{name}' has an empty prompt"),
        ));
    }
    if file.prompt.len() > MAX_PROMPT_BYTES {
        return Err(ApiError::new(
            "asset.command_prompt_too_large",
            format!("command '{name}' prompt exceeds {MAX_PROMPT_BYTES} bytes"),
        ));
    }
    // Prompt shell interpolation is a host-execution mechanism. Reject it here
    // so a command asset can never execute a script outside the sandbox tool.
    if file.prompt.contains("!{") {
        return Err(ApiError::new(
            "asset.command_shell_rejected",
            format!(
                "command '{name}' uses '!{{...}}' shell interpolation; run scripts through a sandboxed tool instead"
            ),
        ));
    }
    let mut arguments = Vec::with_capacity(file.arguments.len());
    let mut names = BTreeSet::new();
    for argument in file.arguments {
        if !is_argument_name(&argument.name) {
            return Err(ApiError::new(
                "asset.command_argument_invalid",
                format!(
                    "command '{name}' argument '{}' must be 1-64 characters of [A-Za-z0-9_-]",
                    argument.name
                ),
            ));
        }
        if !names.insert(argument.name.clone()) {
            return Err(ApiError::new(
                "asset.command_argument_invalid",
                format!(
                    "command '{name}' declares argument '{}' more than once",
                    argument.name
                ),
            ));
        }
        if argument.required && argument.default.is_some() {
            return Err(ApiError::new(
                "asset.command_argument_invalid",
                format!(
                    "command '{name}' argument '{}' is required and also has a default",
                    argument.name
                ),
            ));
        }
        arguments.push(CommandArgument {
            name: argument.name,
            description: argument.description,
            required: argument.required,
            default: argument.default,
        });
    }
    for placeholder in CommandAsset::placeholders(&file.prompt)? {
        if placeholder != "args" && !names.contains(&placeholder) {
            return Err(ApiError::new(
                "asset.command_placeholder_unknown",
                format!(
                    "command '{name}' prompt references undeclared placeholder '{{{placeholder}}}'"
                ),
            ));
        }
    }
    let mut allowed = BTreeSet::new();
    for tool in &file.allowed_tools {
        if !is_tool_name(tool) {
            return Err(ApiError::new(
                "asset.command_tool_invalid",
                format!("command '{name}' has an invalid allowed tool '{tool}'"),
            ));
        }
        if !allowed.insert(tool.clone()) {
            return Err(ApiError::new(
                "asset.command_tool_invalid",
                format!("command '{name}' lists tool '{tool}' more than once"),
            ));
        }
    }
    if let Some(model) = &file.model {
        if model.trim().is_empty() || model.len() > 128 {
            return Err(ApiError::new(
                "asset.command_model_invalid",
                format!("command '{name}' has an invalid model reference"),
            ));
        }
    }
    Ok(CommandAsset {
        name,
        description: file.description,
        prompt: file.prompt,
        allowed_tools: file.allowed_tools,
        model: file.model,
        agent: file.agent,
        arguments,
    })
}

fn validate_name(name: &str, path: &Path) -> Result<(), ApiError> {
    if name.len() > 128 {
        return Err(ApiError::new(
            "asset.command_invalid",
            format!(
                "command '{}' name is longer than 128 characters",
                path.display()
            ),
        ));
    }
    for component in name.split(':') {
        validate_component(component, path)?;
    }
    Ok(())
}

fn validate_component(component: &str, path: &Path) -> Result<(), ApiError> {
    if component.is_empty()
        || component.len() > 64
        || !component.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
    {
        return Err(ApiError::new(
            "asset.command_invalid",
            format!(
                "command path '{}' must use names of 1-64 characters of [A-Za-z0-9_-]",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn is_argument_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

fn is_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "*"
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character == '_'
                || character == '-'
                || character == '.'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("command directory");
        }
        std::fs::write(&path, content).expect("command asset");
        path
    }

    #[test]
    fn loads_path_namespaced_commands_and_declared_arguments() {
        let temp = tempfile::tempdir().expect("tempdir");
        write(
            temp.path(),
            "review/security.toml",
            "description = \"Security review\"\nprompt = \"Review {{args}} as {{audience}}\"\n\n[[arguments]]\nname = \"audience\"\ndefault = \"operator\"\n",
        );
        let assets = load(&[temp.path().to_path_buf()]).expect("commands load");
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].name, "review:security");
        assert_eq!(assets[0].arguments[0].name, "audience");
        assert_eq!(assets[0].arguments[0].default.as_deref(), Some("operator"));
    }

    #[test]
    fn duplicate_names_and_unknown_placeholders_fail_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        write(
            temp.path(),
            "one.toml",
            "name = \"same\"\nprompt = \"{{unknown}}\"",
        );
        let error = load(&[temp.path().to_path_buf()]).expect_err("unknown placeholder");
        assert_eq!(error.code, "asset.command_placeholder_unknown");

        write(temp.path(), "one.toml", "name = \"same\"\nprompt = \"ok\"");
        write(temp.path(), "two.toml", "name = \"same\"\nprompt = \"ok\"");
        let error = load(&[temp.path().to_path_buf()]).expect_err("duplicate name");
        assert_eq!(error.code, "asset.command_duplicate");
    }

    #[test]
    fn shell_interpolation_is_rejected_before_execution() {
        let temp = tempfile::tempdir().expect("tempdir");
        write(
            temp.path(),
            "run.toml",
            "prompt = \"!{touch canary}\\n{{args}}\"",
        );
        let error = load(&[temp.path().to_path_buf()]).expect_err("shell interpolation");
        assert_eq!(error.code, "asset.command_shell_rejected");
    }

    #[test]
    fn unknown_fields_and_invalid_arguments_are_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        write(temp.path(), "bad.toml", "prompt = \"ok\"\nunknown = true");
        let error = load(&[temp.path().to_path_buf()]).expect_err("unknown field");
        assert_eq!(error.code, "asset.command_malformed");

        write(
            temp.path(),
            "bad.toml",
            "prompt = \"{{style}}\"\n\n[[arguments]]\nname = \"style\"\nrequired = true\ndefault = \"brief\"",
        );
        let error = load(&[temp.path().to_path_buf()]).expect_err("required default");
        assert_eq!(error.code, "asset.command_argument_invalid");
    }

    #[cfg(unix)]
    #[test]
    fn a_command_symlink_cannot_escape_the_configured_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        let target = outside.path().join("escape.toml");
        std::fs::write(&target, "prompt = \"escaped\"").expect("outside command");
        std::os::unix::fs::symlink(&target, temp.path().join("escape.toml"))
            .expect("command symlink");
        let error = load(&[temp.path().to_path_buf()]).expect_err("escaping command");
        assert_eq!(error.code, "asset.path_denied");
    }
}
