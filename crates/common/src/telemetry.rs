//! Bounded operational metrics and trace correlation primitives.
//!
//! Metrics deliberately carry a small, fixed vocabulary. Raw task, session,
//! principal, and secret values are never metric labels; they belong to audit
//! and trace evidence where access is already scoped.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How one metric value was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricQuality {
    /// Directly observed by the owning process.
    Observed,
    /// Reported by the model provider.
    ProviderConfirmed,
    /// Derived locally from observed text and declared pricing.
    Estimated,
    /// Value is not available. It must not be serialized as zero.
    Unknown,
}

/// Bounded operational metric sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    /// Stable metric name.
    pub name: String,
    /// Value. `None` means explicitly unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<u64>,
    /// Value quality.
    pub quality: MetricQuality,
    /// Bounded labels only.
    pub labels: BTreeMap<String, String>,
}

impl MetricSample {
    /// Construct a metric with a fixed label map.
    pub fn new(
        name: &str,
        value: Option<u64>,
        quality: MetricQuality,
        labels: BTreeMap<String, String>,
    ) -> Self {
        Self {
            name: name.to_string(),
            value,
            quality,
            labels,
        }
    }
}

/// Label names that are part of the bounded metric vocabulary.
pub const BOUNDED_LABEL_KEYS: &[&str] = &[
    "instance",
    "package",
    "phase",
    "admission",
    "state",
    "capability",
    "outcome",
    "quality",
];

/// Whether a label key is part of the bounded vocabulary.
pub fn is_bounded_label(key: &str) -> bool {
    BOUNDED_LABEL_KEYS.contains(&key)
}

/// Normalize a W3C trace id or generate a fresh one.
///
/// A valid id is 32 lowercase hexadecimal characters and is not all zeroes.
/// Anything else is replaced rather than propagated, so a caller cannot inject
/// an unbounded or secret-bearing value into correlation fields.
pub fn trace_id(candidate: Option<&str>) -> String {
    match candidate {
        Some(value)
            if value.len() == 32
                && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                && value.bytes().any(|byte| byte != b'0') =>
        {
            value.to_ascii_lowercase()
        }
        _ => uuid::Uuid::new_v4().simple().to_string(),
    }
}

/// Extract a trace id from a W3C `traceparent` value.
pub fn traceparent_id(value: &str) -> Option<String> {
    let mut parts = value.trim().split('-');
    let version = parts.next()?;
    let trace = parts.next()?;
    let _span = parts.next()?;
    let _flags = parts.next()?;
    if version.len() != 2 || !version.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(trace_id(Some(trace)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_ids_are_bounded_and_invalid_input_is_replaced() {
        let valid = "0123456789abcdef0123456789abcdef";
        assert_eq!(trace_id(Some(valid)), valid);
        assert_eq!(
            traceparent_id("00-0123456789abcdef0123456789abcdef-0123456789abcdef-01").as_deref(),
            Some(valid)
        );
        for invalid in ["", "sk-secret-canary", &"0".repeat(32)] {
            let generated = trace_id(Some(invalid));
            assert_eq!(generated.len(), 32);
            assert_ne!(generated, invalid);
        }
    }

    #[test]
    fn metric_label_vocabulary_is_bounded() {
        for key in ["instance", "package", "phase", "state", "quality"] {
            assert!(is_bounded_label(key), "{key}");
        }
        for key in ["task_id", "session_id", "principal", "secret", "token"] {
            assert!(!is_bounded_label(key), "{key}");
        }
    }
}
