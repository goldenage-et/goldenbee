//! Generic configured memory-file loading for runtime context assembly.
//!
//! The loader is intentionally independent of any product filename. Operators
//! declare exact paths and globs, hierarchy order, precedence, and whether
//! `@path` imports are followed. Loading is deterministic: source precedence,
//! declared path order, and sorted glob matches decide the assembled order, and
//! each file is represented by a content digest.
//!
//! Missing, unreadable, oversized, duplicate, and cyclic sources are never
//! silently dropped. A required source fails startup with a typed error; an
//! optional source records a typed diagnostic that is carried into the context
//! manifest.

use crate::context::config::{ContextConfig, MemorySource};
use crate::worker::WorkerResult;
use common::ApiError;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// One assembled memory file.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryFile {
    /// Configured source identity.
    pub source: String,
    /// Source precedence.
    pub precedence: i32,
    /// Display path relative to the configuration directory when possible.
    pub path: String,
    /// Content digest.
    pub digest: String,
    /// Byte length.
    pub bytes: usize,
    /// Whether this file was reached through an `@path` import.
    pub imported: bool,
}

/// One typed diagnostic for an optional source that could not be assembled.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryDiagnostic {
    /// Stable error code.
    pub code: String,
    /// Configured source identity.
    pub source: String,
    /// Display path relative to the configuration directory when possible.
    pub path: String,
    /// Redacted explanation.
    pub message: String,
}

/// Immutable assembled context inputs for one asset revision.
#[derive(Debug, Clone, Default)]
pub struct ContextBundle {
    /// Ordered files in assembly order.
    pub files: Vec<MemoryFile>,
    /// Typed diagnostics for optional sources.
    pub diagnostics: Vec<MemoryDiagnostic>,
    /// Rendered instruction suffix, empty when no memory files are configured.
    pub text: String,
    /// Digest of the ordered file list and configured limits.
    pub digest: String,
    /// Digest of the operator memory configuration.
    pub config_digest: String,
}

impl ContextBundle {
    /// Assemble every configured memory source under `base`.
    pub fn load(config: &ContextConfig, base: &Path) -> WorkerResult<Self> {
        config.validate()?;
        let config_digest = common::canonical_digest(config);
        let mut sources = config.memory.clone();
        sources.sort_by(|left, right| {
            right
                .precedence
                .cmp(&left.precedence)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut loader = Loader {
            config,
            base,
            files: Vec::new(),
            diagnostics: Vec::new(),
            seen: BTreeMap::new(),
            stack: Vec::new(),
            text: String::new(),
            used_bytes: 0,
        };
        for source in &sources {
            loader.load_source(source)?;
        }
        let digest = common::canonical_digest(&BundleDigest {
            config: &config_digest,
            files: &loader.files,
            diagnostics: &loader.diagnostics,
        });
        Ok(Self {
            files: loader.files,
            diagnostics: loader.diagnostics,
            text: loader.text,
            digest,
            config_digest,
        })
    }

    /// Append assembled memory to a base instruction.
    pub fn apply_to_instruction(&self, base: &str) -> String {
        if self.text.is_empty() {
            base.to_string()
        } else if base.trim().is_empty() {
            self.text.clone()
        } else {
            format!("{base}\n\n{}", self.text)
        }
    }
}

#[derive(Serialize)]
struct BundleDigest<'a> {
    config: &'a str,
    files: &'a [MemoryFile],
    diagnostics: &'a [MemoryDiagnostic],
}

struct Loader<'a> {
    config: &'a ContextConfig,
    base: &'a Path,
    files: Vec<MemoryFile>,
    diagnostics: Vec<MemoryDiagnostic>,
    seen: BTreeMap<PathBuf, String>,
    stack: Vec<PathBuf>,
    text: String,
    used_bytes: usize,
}

impl Loader<'_> {
    fn load_source(&mut self, source: &MemorySource) -> WorkerResult<()> {
        for declared in &source.paths {
            let path = self.resolve(declared);
            match self.expand(&path, source, false, 1) {
                Ok(_) => {}
                Err(error) => self.source_failure(source, declared, error)?,
            }
        }
        for pattern in &source.globs {
            let matches = self.glob(pattern)?;
            if matches.is_empty() {
                let error = ApiError::new(
                    "context.memory_missing",
                    format!("memory glob '{pattern}' matched no files"),
                )
                .with("source", source.id.clone())
                .with("path", pattern.clone());
                self.source_failure(source, pattern, error)?;
                continue;
            }
            for path in matches {
                match self.expand(&path, source, false, 1) {
                    Ok(_) => {}
                    Err(error) => self.source_failure(source, &self.display(&path), error)?,
                }
            }
        }
        Ok(())
    }

    fn glob(&self, pattern: &str) -> WorkerResult<Vec<PathBuf>> {
        let resolved = if Path::new(pattern).is_absolute() {
            PathBuf::from(pattern)
        } else {
            self.base.join(pattern)
        };
        let text = resolved.to_string_lossy().to_string();
        let entries = glob::glob(&text).map_err(|error| {
            ApiError::new(
                "context.memory_glob_invalid",
                format!("memory glob '{pattern}' is invalid: {error}"),
            )
        })?;
        let mut matches = Vec::new();
        for entry in entries {
            match entry {
                Ok(path) if path.is_file() => matches.push(path),
                Ok(_) => {}
                Err(error) => {
                    return Err(ApiError::new(
                        "context.memory_unreadable",
                        format!("memory glob '{pattern}' could not read a match: {error}"),
                    ))
                }
            }
        }
        matches.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
        Ok(matches)
    }

    fn expand(
        &mut self,
        path: &Path,
        source: &MemorySource,
        imported: bool,
        depth: usize,
    ) -> WorkerResult<String> {
        if depth > self.config.max_import_depth {
            return Err(ApiError::new(
                "context.memory_import_depth",
                format!(
                    "memory import depth exceeded {} while loading '{}'",
                    self.config.max_import_depth,
                    path.display()
                ),
            )
            .with("source", source.id.clone()));
        }
        let canonical = path.canonicalize().map_err(|error| {
            ApiError::new(
                "context.memory_missing",
                format!("memory file '{}' is not readable: {error}", path.display()),
            )
            .with("source", source.id.clone())
            .with("path", self.display(path))
        })?;
        if self.stack.contains(&canonical) {
            return Err(ApiError::new(
                "context.memory_import_cycle",
                format!(
                    "memory import cycle detected at '{}'",
                    self.display(&canonical)
                ),
            )
            .with("source", source.id.clone())
            .with("path", self.display(&canonical)));
        }
        if let Some(existing) = self.seen.get(&canonical) {
            self.diagnostics.push(MemoryDiagnostic {
                code: "context.memory_duplicate".into(),
                source: source.id.clone(),
                path: self.display(&canonical),
                message: format!(
                    "file is already assembled by source '{existing}'; the first occurrence wins"
                ),
            });
            return Ok(String::new());
        }
        if self.files.len() >= self.config.max_memory_files {
            return Err(ApiError::new(
                "context.memory_file_limit",
                format!(
                    "context memory exceeded the {} file limit",
                    self.config.max_memory_files
                ),
            )
            .with("source", source.id.clone()));
        }
        let limit = source.max_bytes.unwrap_or(self.config.max_memory_bytes);
        let bytes = read_bounded(&canonical, limit).map_err(|error| match error {
            ReadError::Missing(message) => ApiError::new("context.memory_missing", message)
                .with("source", source.id.clone())
                .with("path", self.display(&canonical)),
            ReadError::Unreadable(message) => ApiError::new("context.memory_unreadable", message)
                .with("source", source.id.clone())
                .with("path", self.display(&canonical)),
            ReadError::Oversized { bytes, limit } => ApiError::new(
                "context.memory_oversized",
                format!(
                    "memory file '{}' is {bytes} bytes, above the {limit}-byte ceiling",
                    self.display(&canonical)
                ),
            )
            .with("source", source.id.clone())
            .with("path", self.display(&canonical))
            .with("bytes", bytes.to_string())
            .with("limit", limit.to_string()),
        })?;
        self.seen.insert(canonical.clone(), source.id.clone());
        self.stack.push(canonical.clone());
        let digest = common::bytes_digest(&bytes);
        self.files.push(MemoryFile {
            source: source.id.clone(),
            precedence: source.precedence,
            path: self.display(&canonical),
            digest: digest.clone(),
            bytes: bytes.len(),
            imported,
        });
        self.used_bytes = self.used_bytes.saturating_add(bytes.len());
        if self.used_bytes > self.config.max_context_bytes {
            return Err(ApiError::new(
                "context.memory_context_oversized",
                format!(
                    "assembled context memory exceeded the {}-byte ceiling",
                    self.config.max_context_bytes
                ),
            )
            .with("source", source.id.clone()));
        }
        let text = String::from_utf8(bytes).map_err(|error| {
            ApiError::new(
                "context.memory_unreadable",
                format!(
                    "memory file '{}' is not valid UTF-8: {error}",
                    self.display(&canonical)
                ),
            )
            .with("source", source.id.clone())
            .with("path", self.display(&canonical))
        })?;
        let expanded = if source.imports {
            self.expand_imports(&text, &canonical, source, depth)?
        } else {
            text
        };
        self.stack.pop();
        let rendered = format!(
            "<context_memory_file source=\"{}\" path=\"{}\" digest=\"{}\" precedence=\"{}\" imported=\"{}\">\n{}\n</context_memory_file>\n",
            attribute(&source.id),
            attribute(&self.display(&canonical)),
            attribute(&digest),
            source.precedence,
            imported,
            expanded.trim_end_matches('\n')
        );
        if !imported {
            self.text.push_str(&rendered);
        }
        Ok(rendered)
    }

    fn expand_imports(
        &mut self,
        text: &str,
        current: &Path,
        source: &MemorySource,
        depth: usize,
    ) -> WorkerResult<String> {
        let mut output = String::new();
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim();
            let Some(reference) = import_reference(trimmed) else {
                output.push_str(line);
                continue;
            };
            let target = if Path::new(&reference).is_absolute() {
                PathBuf::from(&reference)
            } else {
                current
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(&reference)
            };
            match self.expand(&target, source, true, depth + 1) {
                Ok(rendered) => output.push_str(&rendered),
                Err(error) => {
                    if source.optional {
                        self.diagnostics.push(MemoryDiagnostic {
                            code: error.code.clone(),
                            source: source.id.clone(),
                            path: self.display(&target),
                            message: common::redact(&error.message),
                        });
                        output.push_str(line);
                    } else {
                        return Err(error);
                    }
                }
            }
            if !line.ends_with('\n') {
                output.push('\n');
            }
        }
        Ok(output)
    }

    fn source_failure(
        &mut self,
        source: &MemorySource,
        declared: &str,
        error: ApiError,
    ) -> WorkerResult<()> {
        if !source.optional {
            return Err(error);
        }
        self.diagnostics.push(MemoryDiagnostic {
            code: error.code.clone(),
            source: source.id.clone(),
            path: if declared.is_empty() {
                String::new()
            } else {
                declared.to_string()
            },
            message: common::redact(&error.message),
        });
        Ok(())
    }

    fn resolve(&self, declared: &str) -> PathBuf {
        let path = Path::new(declared);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.base.join(path)
        }
    }

    fn display(&self, path: &Path) -> String {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let base = self
            .base
            .canonicalize()
            .unwrap_or_else(|_| self.base.to_path_buf());
        if let Ok(relative) = canonical.strip_prefix(&base) {
            relative.to_string_lossy().replace('\\', "/")
        } else {
            canonical
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| canonical.to_string_lossy().to_string())
        }
    }
}

fn import_reference(line: &str) -> Option<String> {
    let rest = line.strip_prefix('@')?.trim();
    if rest.is_empty() {
        return None;
    }
    let unquoted = if let Some(value) = rest
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        value
    } else if let Some(value) = rest
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    {
        value
    } else {
        rest
    };
    if unquoted.is_empty() || unquoted.chars().any(char::is_whitespace) {
        return None;
    }
    Some(unquoted.to_string())
}

fn attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

enum ReadError {
    Missing(String),
    Unreadable(String),
    Oversized { bytes: usize, limit: usize },
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, ReadError> {
    let file = std::fs::File::open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ReadError::Missing(format!(
                "memory file '{}' was configured but does not exist",
                path.display()
            ))
        } else {
            ReadError::Unreadable(format!(
                "memory file '{}' is not readable: {error}",
                path.display()
            ))
        }
    })?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            ReadError::Unreadable(format!(
                "memory file '{}' is not readable: {error}",
                path.display()
            ))
        })?;
    if bytes.len() > limit {
        let actual = std::fs::metadata(path)
            .map(|metadata| metadata.len() as usize)
            .unwrap_or(bytes.len());
        return Err(ReadError::Oversized {
            bytes: actual,
            limit,
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(sources: Vec<MemorySource>) -> ContextConfig {
        ContextConfig {
            memory: sources,
            ..ContextConfig::default()
        }
    }

    fn source(id: &str, paths: &[&str], globs: &[&str], precedence: i32) -> MemorySource {
        MemorySource {
            id: id.into(),
            paths: paths.iter().map(|value| (*value).into()).collect(),
            globs: globs.iter().map(|value| (*value).into()).collect(),
            precedence,
            imports: true,
            optional: false,
            max_bytes: None,
        }
    }

    #[test]
    fn precedence_and_declared_order_are_deterministic() {
        let dir = tempfile::tempdir().expect("temp");
        std::fs::write(dir.path().join("a.md"), "alpha").unwrap();
        std::fs::write(dir.path().join("b.md"), "beta").unwrap();
        let config = config(vec![
            source("low", &["a.md"], &[], 1),
            source("high", &["b.md"], &[], 10),
        ]);
        let first = ContextBundle::load(&config, dir.path()).expect("load");
        let second = ContextBundle::load(&config, dir.path()).expect("load");
        assert_eq!(first.digest, second.digest);
        assert_eq!(first.files[0].source, "high");
        assert_eq!(first.files[1].source, "low");
        assert!(first.text.find("beta").unwrap() < first.text.find("alpha").unwrap());
    }

    #[test]
    fn imports_expand_and_cycles_are_typed() {
        let dir = tempfile::tempdir().expect("temp");
        std::fs::write(dir.path().join("root.md"), "root\n@child.md\nend\n").unwrap();
        std::fs::write(dir.path().join("child.md"), "child").unwrap();
        let bundle =
            ContextBundle::load(&config(vec![source("s", &["root.md"], &[], 0)]), dir.path())
                .expect("load");
        assert!(bundle.text.contains("root"));
        assert!(bundle.text.contains("child"));
        assert!(bundle.text.contains("end"));
        assert_eq!(bundle.files.len(), 2);
        assert!(bundle.files[1].imported);

        std::fs::write(dir.path().join("cycle.md"), "@cycle.md").unwrap();
        let error = ContextBundle::load(
            &config(vec![source("s", &["cycle.md"], &[], 0)]),
            dir.path(),
        )
        .expect_err("cycle rejected");
        assert_eq!(error.code, "context.memory_import_cycle");
    }

    #[test]
    fn required_missing_file_fails_and_optional_records_a_diagnostic() {
        let dir = tempfile::tempdir().expect("temp");
        let error = ContextBundle::load(
            &config(vec![source("s", &["missing.md"], &[], 0)]),
            dir.path(),
        )
        .expect_err("required file missing");
        assert_eq!(error.code, "context.memory_missing");

        let mut optional = source("s", &["missing.md"], &[], 0);
        optional.optional = true;
        let bundle = ContextBundle::load(&config(vec![optional]), dir.path()).expect("optional");
        assert!(bundle.files.is_empty());
        assert_eq!(bundle.diagnostics.len(), 1);
        assert_eq!(bundle.diagnostics[0].code, "context.memory_missing");
    }

    #[test]
    fn globs_expand_in_sorted_order_and_oversized_files_are_typed() {
        let dir = tempfile::tempdir().expect("temp");
        std::fs::write(dir.path().join("b.md"), "beta").unwrap();
        std::fs::write(dir.path().join("a.md"), "alpha").unwrap();
        let bundle = ContextBundle::load(&config(vec![source("s", &[], &["*.md"], 0)]), dir.path())
            .expect("glob");
        assert_eq!(bundle.files.len(), 2);
        assert!(bundle.files[0].path.ends_with("a.md"));

        let mut limited = source("s", &["a.md"], &[], 0);
        limited.max_bytes = Some(1);
        let error = ContextBundle::load(&config(vec![limited]), dir.path())
            .expect_err("oversized rejected");
        assert_eq!(error.code, "context.memory_oversized");
    }
}
