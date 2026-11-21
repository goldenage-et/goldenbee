//! Provider-neutral model selection vocabulary.
//!
//! These values are catalog and task-evidence shapes. They never identify a
//! concrete endpoint, credential, or native adapter type.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Provider-neutral reasoning effort requested for one invocation.
///
/// The runtime applies a level only through a native adapter that exposes the
/// corresponding request field. A catalog entry declares which levels it
/// supports; a request outside that declaration is rejected before admission
/// rather than silently downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ReasoningLevel {
    /// Disable explicit reasoning effort.
    #[serde(rename = "none")]
    None,
    /// Minimal effort where the provider retains that setting.
    #[serde(rename = "minimal")]
    Minimal,
    /// Low effort.
    #[serde(rename = "low")]
    Low,
    /// Balanced effort.
    #[serde(rename = "medium")]
    Medium,
    /// High effort.
    #[serde(rename = "high")]
    High,
    /// Extra-high effort.
    #[serde(rename = "xhigh")]
    XHigh,
    /// Maximum effort where the provider supports it.
    #[serde(rename = "max")]
    Max,
}

impl ReasoningLevel {
    /// Stable wire and evidence spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Every level in ascending effort order.
    pub fn all() -> &'static [ReasoningLevel] {
        &[
            Self::None,
            Self::Minimal,
            Self::Low,
            Self::Medium,
            Self::High,
            Self::XHigh,
            Self::Max,
        ]
    }

    /// Parse a stable level name.
    pub fn parse(value: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|level| level.as_str() == value)
    }
}

impl fmt::Display for ReasoningLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ReasoningLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value).ok_or_else(|| format!("'{value}' is not a supported reasoning level"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_names_round_trip() {
        for level in ReasoningLevel::all() {
            assert_eq!(level.as_str().parse::<ReasoningLevel>(), Ok(*level));
            let wire = serde_json::to_string(level).expect("serializes");
            assert_eq!(
                serde_json::from_str::<ReasoningLevel>(&wire).expect("deserializes"),
                *level
            );
        }
    }

    #[test]
    fn effort_order_is_monotonic() {
        let levels = ReasoningLevel::all();
        for pair in levels.windows(2) {
            assert!(pair[0] < pair[1], "{pair:?}");
        }
    }
}
