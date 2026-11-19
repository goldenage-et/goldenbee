//! Execution limits, runtime capacity, and token usage.

use serde::{Deserialize, Serialize};

/// Exact execution limits. Zero means unset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Wall-clock deadline in milliseconds.
    #[serde(default)]
    pub max_duration_ms: u64,
    /// Maximum model calls.
    #[serde(default)]
    pub max_model_calls: u32,
    /// Maximum tool calls.
    #[serde(default)]
    pub max_tool_calls: u32,
    /// Maximum estimated total tokens.
    #[serde(default)]
    pub max_tokens: u64,
    /// Maximum estimated cost in millionths of a currency unit.
    ///
    /// The estimate is derived from operator-declared provider pricing, not
    /// provider billing. Zero means unset.
    #[serde(default)]
    pub max_cost_micros: u64,
    /// Maximum child fanout.
    #[serde(default)]
    pub max_fanout: u32,
    /// Maximum delegation depth.
    #[serde(default)]
    pub max_depth: u32,
}

impl Limits {
    /// Return limits that never widen the parent ceiling.
    pub fn narrower(&self, other: &Limits) -> Limits {
        fn min_nonzero(a: u64, b: u64) -> u64 {
            match (a, b) {
                (0, b) => b,
                (a, 0) => a,
                (a, b) => a.min(b),
            }
        }
        fn min_nonzero_u32(a: u32, b: u32) -> u32 {
            match (a, b) {
                (0, b) => b,
                (a, 0) => a,
                (a, b) => a.min(b),
            }
        }
        Limits {
            max_duration_ms: min_nonzero(self.max_duration_ms, other.max_duration_ms),
            max_model_calls: min_nonzero_u32(self.max_model_calls, other.max_model_calls),
            max_tool_calls: min_nonzero_u32(self.max_tool_calls, other.max_tool_calls),
            max_tokens: min_nonzero(self.max_tokens, other.max_tokens),
            max_cost_micros: min_nonzero(self.max_cost_micros, other.max_cost_micros),
            max_fanout: min_nonzero_u32(self.max_fanout, other.max_fanout),
            max_depth: min_nonzero_u32(self.max_depth, other.max_depth),
        }
    }
}

/// Runtime capacity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capacity {
    /// Active native executions.
    pub active: u32,
    /// Configured active limit.
    pub limit: u32,
    /// Queued tasks.
    pub queued: u32,
    /// Configured queue limit.
    pub queue_limit: u32,
}

/// Token usage and cost keeping provider-confirmed values, local estimates, and
/// unavailable values distinct.
///
/// Absent fields mean the value could not be established. They are never
/// serialized as zero consumption. `estimated_tokens` and
/// `estimated_cost_micros` are harness-side estimates derived from observed
/// text and operator-declared provider pricing; they are not provider billing.
/// This record belongs to one task. It is not a second authoritative aggregate
/// store for the Gateway or any Client.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Provider-confirmed prompt tokens, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    /// Provider-confirmed candidate tokens, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_tokens: Option<u64>,
    /// Locally estimated tokens for calls whose provider counts were absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens: Option<u64>,
    /// Locally estimated monetary cost in millionths of a currency unit.
    ///
    /// Derived from operator-declared input and candidate pricing. Absent when
    /// no pricing was declared, or when neither reported nor estimated token
    /// counts are available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_micros: Option<u64>,
    /// Model calls observed.
    #[serde(default)]
    pub model_calls: u32,
    /// Tool calls that produced a result.
    #[serde(default)]
    pub tool_calls: u32,
}
