//! Stable installer failure. Messages are redacted before they reach audit or
//! operator output.

use std::fmt;

/// A rejected inspection, installation, or smoke check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallError {
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Redacted human-readable detail.
    pub message: String,
}

impl InstallError {
    /// Build an error with a stable code.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: common::redact(&message.into()),
        }
    }
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for InstallError {}
