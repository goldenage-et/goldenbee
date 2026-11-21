//! Evidence redaction applied before task events, audit, traces, effect
//! receipts, and diagnostics are persisted or projected.
//!
//! Three classes are removed by field:
//!
//! - secret values: marker-shaped credentials (`sk-`, `Bearer `, `token=` and
//!   neighbours) and secret-bearing object keys;
//! - credential values: the same marker pass catches raw bearer tokens, while
//!   credential *references* such as `env:NAME` remain visible evidence;
//! - disallowed host paths: every absolute path outside the configured roots
//!   becomes `[REDACTED_PATH]`; a path under an allowed root keeps its
//!   workspace-relative shape behind `[root]`.
//!
//! The rules are deterministic so two processes with the same configuration
//! redact the same way. They never depend on a Client presentation layer.

use serde_json::{Map, Value};
use std::path::PathBuf;

/// Marker that replaces an absolute path outside the configured roots.
pub(crate) const REDACTED_PATH: &str = "[REDACTED_PATH]";
/// Marker that replaces an allowed root prefix.
pub(crate) const ROOT_MARKER: &str = "[root]";

/// Secret-bearing object keys. Matching is case-insensitive and substring
/// based so `x-api-key` and `authorization` are both covered.
const SECRET_KEYS: &[&str] = &[
    "authorization",
    "api_key",
    "apikey",
    "private_key",
    "client_secret",
    "passphrase",
    "password",
    "secret",
    "token",
];

/// Accounting fields that look secret-bearing by substring but are evidence.
const ACCOUNTING_KEYS: &[&str] = &[
    "prompt_tokens",
    "candidate_tokens",
    "estimated_tokens",
    "max_tokens",
    "tokens_used",
    "token_count",
];

/// Deterministic redactor for one Runtime configuration revision.
#[derive(Debug, Clone, Default)]
pub(crate) struct Redactor {
    roots: Vec<String>,
}

impl Redactor {
    /// Build a redactor that treats the supplied roots as allowed.
    pub(crate) fn new(roots: &[PathBuf]) -> Self {
        let mut roots: Vec<String> = roots
            .iter()
            .map(|root| {
                let text = root.to_string_lossy().to_string();
                let text = text.trim_end_matches('/').to_string();
                if text.is_empty() {
                    "/".to_string()
                } else {
                    text
                }
            })
            .collect();
        // Longest first so a nested root is recognized before its parent.
        roots.sort_by_key(|root| std::cmp::Reverse(root.len()));
        roots.dedup();
        Self { roots }
    }

    /// Redact one free-form string.
    pub(crate) fn text(&self, text: &str) -> String {
        let redacted = common::redact(text);
        self.redact_paths(&redacted)
    }

    /// Redact a JSON payload recursively.
    pub(crate) fn json(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.text(&text)),
            Value::Array(values) => {
                Value::Array(values.into_iter().map(|value| self.json(value)).collect())
            }
            Value::Object(map) => Value::Object(self.object(map)),
            other => other,
        }
    }

    fn object(&self, map: Map<String, Value>) -> Map<String, Value> {
        map.into_iter()
            .map(|(key, value)| {
                let lower = key.to_ascii_lowercase();
                let accounting = ACCOUNTING_KEYS.iter().any(|candidate| lower == *candidate);
                if !accounting
                    && SECRET_KEYS
                        .iter()
                        .any(|candidate| lower.contains(candidate))
                {
                    (key, Value::String("[REDACTED]".into()))
                } else {
                    (key, self.json(value))
                }
            })
            .collect()
    }

    fn redact_paths(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut copied = 0usize;
        let mut index = 0usize;
        while index < text.len() {
            let Some(offset) = text[index..].find('/') else {
                break;
            };
            let start = index + offset;
            if !starts_path(text, start) {
                index = start + 1;
                continue;
            }
            let end = path_end(text, start);
            let token = &text[start..end];
            if token.starts_with("//") {
                index = end.max(start + 1);
                continue;
            }
            out.push_str(&text[copied..start]);
            out.push_str(&self.path(token));
            copied = end;
            index = end.max(start + 1);
        }
        out.push_str(&text[copied..]);
        out
    }

    fn path(&self, token: &str) -> String {
        let token = token.trim_end_matches(['.', ',', ';', ':']);
        if token.len() < 2 {
            return REDACTED_PATH.to_string();
        }
        for root in &self.roots {
            if token == root {
                return ROOT_MARKER.to_string();
            }
            if let Some(rest) = token.strip_prefix(root) {
                if rest.starts_with('/') {
                    return format!("{ROOT_MARKER}{rest}");
                }
            }
        }
        REDACTED_PATH.to_string()
    }
}

/// Whether a `/` at `index` begins a filesystem path rather than a URL.
fn starts_path(text: &str, index: usize) -> bool {
    match text[..index].chars().next_back() {
        None => true,
        Some(previous) => {
            previous.is_whitespace()
                || matches!(
                    previous,
                    '"' | '\'' | '=' | '(' | '[' | '{' | ',' | ';' | '<' | '>'
                )
        }
    }
}

/// Whether `ch` ends a filesystem path token.
fn ends_path(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\'' | '<' | '>' | '|' | ',' | ';' | ':' | ')' | ']' | '}'
        )
}

fn path_end(text: &str, start: usize) -> usize {
    for (offset, ch) in text[start..].char_indices() {
        if offset > 0 && ends_path(ch) {
            return start + offset;
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_disallowed_paths_are_redacted() {
        let redactor = Redactor::new(&[PathBuf::from("/tmp/worker/data")]);
        let text = "read /home/other/tenant/key.pem with token=sk-live-secret and Bearer abc";
        let redacted = redactor.text(text);
        assert!(!redacted.contains("/home/other/tenant"));
        assert!(!redacted.contains("sk-live-secret"));
        assert!(!redacted.contains("Bearer abc"));
        assert!(redacted.contains(REDACTED_PATH));
    }

    #[test]
    fn allowed_roots_keep_relative_evidence() {
        let redactor = Redactor::new(&[PathBuf::from("/tmp/worker/data")]);
        assert_eq!(
            redactor.text("/tmp/worker/data/workspace/a.txt"),
            "[root]/workspace/a.txt"
        );
    }

    #[test]
    fn urls_are_not_treated_as_host_paths() {
        let redactor = Redactor::new(&[]);
        assert_eq!(
            redactor.text("see https://example.com/a/b for details"),
            "see https://example.com/a/b for details"
        );
    }

    #[test]
    fn secret_keys_are_redacted_but_accounting_survives() {
        let redactor = Redactor::new(&[]);
        let value = redactor.json(serde_json::json!({
            "api_key": "value",
            "authorization": "value",
            "prompt_tokens": 7,
            "nested": {"password": "value", "ok": true}
        }));
        assert_eq!(value["api_key"], "[REDACTED]");
        assert_eq!(value["authorization"], "[REDACTED]");
        assert_eq!(value["prompt_tokens"], 7);
        assert_eq!(value["nested"]["password"], "[REDACTED]");
        assert_eq!(value["nested"]["ok"], true);
    }
}
