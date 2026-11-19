//! Stable, redacted package errors shared by adapters, tools, and hooks.

use common::ApiError;
use serde_json::json;

/// One package failure with a stable code and a safe message.
#[derive(Debug, Clone)]
pub struct KnowledgeError {
    /// Stable machine-readable code, for example `knowledge.unsupported_format`.
    pub code: String,
    /// Redacted, actionable message.
    pub message: String,
    /// Optional structured facts such as a byte limit or invalid offset.
    pub details: Vec<(String, String)>,
}

impl KnowledgeError {
    /// Construct an error with a stable code.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Vec::new(),
        }
    }

    /// Attach one structured detail.
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.push((key.into(), value.into()));
        self
    }

    /// Tool-facing JSON. It never contains host roots that were not already
    /// part of the caller-selected source URI.
    pub fn to_json(&self) -> serde_json::Value {
        let mut value = json!({
            "error": self.code,
            "message": common::redact(&self.message),
        });
        for (key, detail) in &self.details {
            value[key] = json!(common::redact(detail));
        }
        value
    }

    /// Convert to the runtime's management error type.
    pub fn to_api_error(&self) -> ApiError {
        let mut error = ApiError::new(self.code.clone(), common::redact(&self.message));
        for (key, value) in &self.details {
            error = error.with(key.clone(), common::redact(value));
        }
        error
    }
}

/// Package result type.
pub type KnowledgeResult<T> = Result<T, KnowledgeError>;
