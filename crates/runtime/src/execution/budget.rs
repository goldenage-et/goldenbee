//! Execution budget accounting at the shared Worker runtime boundary.
//!
//! Budgets are consumed by one task identity. Provider fallback, retries, and
//! native delegation within that task do not reset the counter or widen the
//! allowance. Call and time limits are enforced before the next unit of work is
//! dispatched. Provider-reported token and operator-priced cost limits are
//! enforced after usage is observed, so the policy event discloses that a
//! provider call already in flight may have completed.
//!
//! These helpers do not aggregate across tasks and do not replace the Gateway
//! projection. They never turn a missing provider value into zero consumption.

use common::{ApiError, Limits, ResolvedModel, Usage};
use serde_json::{json, Value};

/// Durable policy event recorded when a configured budget stops work.
pub const POLICY_EVENT: &str = "budget.exhausted";
/// Durable disclosure when a supported usage budget cannot be enforced exactly.
pub const USAGE_UNAVAILABLE_EVENT: &str = "budget.usage_unavailable";

/// The budget dimension that stopped or constrained execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetKind {
    /// Wall-clock deadline.
    Deadline,
    /// Model calls.
    ModelCalls,
    /// Tool calls.
    ToolCalls,
    /// Estimated or reported tokens.
    Tokens,
    /// Operator-priced estimated cost.
    Cost,
}

impl BudgetKind {
    /// Stable label used in errors and durable events.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deadline => "deadline",
            Self::ModelCalls => "model_calls",
            Self::ToolCalls => "tool_calls",
            Self::Tokens => "tokens",
            Self::Cost => "cost",
        }
    }

    /// The stable task error code for this stop.
    pub fn error_code(self) -> &'static str {
        match self {
            Self::Deadline => "budget.deadline_exceeded",
            Self::ModelCalls => "budget.model_calls_exceeded",
            Self::ToolCalls => "budget.tool_calls_exceeded",
            Self::Tokens => "budget.tokens_exceeded",
            Self::Cost => "budget.cost_exceeded",
        }
    }

    /// Recognize a budget error code emitted by this runtime.
    pub fn from_error_code(code: &str) -> Option<Self> {
        match code {
            "budget.deadline_exceeded" => Some(Self::Deadline),
            "budget.model_calls_exceeded" => Some(Self::ModelCalls),
            "budget.tool_calls_exceeded" => Some(Self::ToolCalls),
            "budget.tokens_exceeded" => Some(Self::Tokens),
            "budget.cost_exceeded" => Some(Self::Cost),
            _ => None,
        }
    }

    /// Configured limit for this dimension. Zero means unset.
    pub fn limit(self, limits: &Limits) -> u64 {
        match self {
            Self::Deadline => limits.max_duration_ms,
            Self::ModelCalls => u64::from(limits.max_model_calls),
            Self::ToolCalls => u64::from(limits.max_tool_calls),
            Self::Tokens => limits.max_tokens,
            Self::Cost => limits.max_cost_micros,
        }
    }

    /// Observed consumption for this dimension.
    pub fn used(self, usage: &Usage) -> u64 {
        match self {
            Self::Deadline => 0,
            Self::ModelCalls => u64::from(usage.model_calls),
            Self::ToolCalls => u64::from(usage.tool_calls),
            Self::Tokens => total_tokens(usage),
            Self::Cost => usage.estimated_cost_micros.unwrap_or(0),
        }
    }
}

/// A budget stop selected before the next unit of work or after usage was
/// reported.
#[derive(Debug, Clone)]
pub struct BudgetStop {
    /// Dimension that stopped the task.
    pub kind: BudgetKind,
    /// Configured limit.
    pub limit: u64,
    /// Observed consumption when the stop was selected.
    pub used: u64,
    /// Whether the stop happened before the next provider or tool call.
    pub exact_preemptive: bool,
    /// Enforced ordering and any unavoidable uncertainty.
    pub uncertainty: String,
}

impl BudgetStop {
    /// Build the stable task error carrying the policy evidence.
    pub fn error(&self) -> ApiError {
        let message = match self.kind {
            BudgetKind::Deadline => "native execution exceeded its wall-clock budget".to_string(),
            BudgetKind::ModelCalls => {
                format!("model-call budget of {} was exhausted", self.limit)
            }
            BudgetKind::ToolCalls => {
                format!("tool-call budget of {} was exhausted", self.limit)
            }
            BudgetKind::Tokens => {
                format!("token budget of {} was exhausted", self.limit)
            }
            BudgetKind::Cost => {
                format!(
                    "estimated cost budget of {} micros was exhausted",
                    self.limit
                )
            }
        };
        ApiError::new(self.kind.error_code(), message)
            .with("budget_kind", self.kind.as_str())
            .with("limit", self.limit.to_string())
            .with("used", self.used.to_string())
            .with("uncertainty", self.uncertainty.clone())
            .with(
                "exact_preemptive",
                if self.exact_preemptive {
                    "true"
                } else {
                    "false"
                },
            )
    }

    /// Durable policy payload. It deliberately excludes task input, prompt
    /// text, credentials, and provider responses.
    pub fn event_payload(&self, limits: &Limits, usage: &Usage, partial: bool) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "limit": self.limit,
            "used": self.used,
            "usage": usage,
            "limits": limits,
            "exact_preemptive": self.exact_preemptive,
            "enforcement": if self.exact_preemptive {
                "before_next_unit"
            } else {
                "after_reported_usage"
            },
            "uncertainty": self.uncertainty,
            "partial_output": partial,
            "durable_partial_events": true,
            "recovery": self.error().recovery,
        })
    }
}

/// Pre-admission budget validation for the provider that will run.
///
/// A configured monetary ceiling is only enforceable when the operator declared
/// the pricing basis for that provider. Rejecting the task before admission is
/// better than running work and later reporting usage the runtime cannot price.
pub fn validate_admission(
    limits: &Limits,
    provider: &crate::ProviderProfile,
    id: &str,
) -> std::result::Result<(), ApiError> {
    if limits.max_cost_micros == 0 {
        return Ok(());
    }
    if provider.input_cost_micros_per_million.is_none()
        || provider.candidate_cost_micros_per_million.is_none()
    {
        return Err(ApiError::new(
            "budget.cost_unverifiable",
            format!(
                "provider '{id}' has a configured cost budget but no operator-declared input and candidate pricing"
            ),
        )
        .with("provider", id)
        .with("budget_kind", BudgetKind::Cost.as_str())
        .with("limit", limits.max_cost_micros.to_string()));
    }
    Ok(())
}

/// Check whether a model event leaves no allowance for the requested tools or
/// reports consumption beyond a configured limit.
///
/// `pending_tool_calls` is the number of calls the just-observed model response
/// requests. Those calls have not been dispatched yet, so a stop selected here
/// prevents them from running.
pub fn check_after_model(
    limits: &Limits,
    usage: &Usage,
    pending_tool_calls: u32,
) -> Option<BudgetStop> {
    // A model budget exhausted on a response that still requests tools leaves
    // no model call to process those results. Stop before dispatch.
    if limits.max_model_calls > 0 {
        if usage.model_calls > limits.max_model_calls {
            return Some(BudgetStop {
                kind: BudgetKind::ModelCalls,
                limit: u64::from(limits.max_model_calls),
                used: u64::from(usage.model_calls),
                exact_preemptive: false,
                uncertainty:
                    "a model call completed before the observed call count crossed the limit".into(),
            });
        }
        if pending_tool_calls > 0 && usage.model_calls == limits.max_model_calls {
            return Some(BudgetStop {
                kind: BudgetKind::ModelCalls,
                limit: u64::from(limits.max_model_calls),
                used: u64::from(usage.model_calls),
                exact_preemptive: true,
                uncertainty:
                    "no model-call allowance remained to process the requested tool results".into(),
            });
        }
    }

    if limits.max_tool_calls > 0 {
        let requested = u64::from(usage.tool_calls) + u64::from(pending_tool_calls);
        if requested > u64::from(limits.max_tool_calls) {
            return Some(BudgetStop {
                kind: BudgetKind::ToolCalls,
                limit: u64::from(limits.max_tool_calls),
                used: u64::from(usage.tool_calls),
                exact_preemptive: true,
                uncertainty: "requested tool calls were not dispatched after the limit".into(),
            });
        }
    }

    let reported = total_tokens(usage);
    if limits.max_tokens > 0 {
        let exhausted = reported > limits.max_tokens
            || (pending_tool_calls > 0 && reported >= limits.max_tokens);
        if exhausted {
            return Some(BudgetStop {
                kind: BudgetKind::Tokens,
                limit: limits.max_tokens,
                used: reported,
                exact_preemptive: false,
                uncertainty: "enforced after provider-reported usage was observed; a provider call already in flight may complete and token counts may be incomplete"
                    .into(),
            });
        }
    }

    if limits.max_cost_micros > 0 {
        if let Some(cost) = usage.estimated_cost_micros {
            let exhausted = cost > limits.max_cost_micros
                || (pending_tool_calls > 0 && cost >= limits.max_cost_micros);
            if exhausted {
                return Some(BudgetStop {
                    kind: BudgetKind::Cost,
                    limit: limits.max_cost_micros,
                    used: cost,
                    exact_preemptive: false,
                    uncertainty: "cost is estimated from operator-declared pricing, not provider billing, and is enforced after reported usage"
                        .into(),
                });
            }
        }
    }

    None
}

/// Reconstruct a policy stop from a committed task error and its limits.
pub fn stop_from_error(error: &ApiError, limits: &Limits, usage: &Usage) -> Option<BudgetStop> {
    let kind = BudgetKind::from_error_code(&error.code)?;
    let detail = |key: &str| error.details.get(key).map(String::as_str);
    let limit = detail("limit")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| kind.limit(limits));
    let used = detail("used")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| kind.used(usage));
    let exact_preemptive = detail("exact_preemptive")
        .map(|value| value == "true")
        .unwrap_or(matches!(kind, BudgetKind::Deadline | BudgetKind::ToolCalls));
    let uncertainty = detail("uncertainty")
        .map(str::to_string)
        .unwrap_or_else(|| match kind {
            BudgetKind::Deadline => {
                "wall-clock enforcement stops the attempt at the deadline".to_string()
            }
            BudgetKind::ModelCalls => {
                "model calls already made are accounted; the native iteration bound may have stopped further calls"
                    .to_string()
            }
            BudgetKind::ToolCalls => "further tool dispatch was blocked".to_string(),
            BudgetKind::Tokens => {
                "token usage is enforced after provider-reported usage is observed; a provider call already in flight may have completed"
                    .to_string()
            }
            BudgetKind::Cost => {
                "cost is estimated from operator-declared pricing, not provider billing, and is enforced after reported usage"
                    .to_string()
            }
        });
    Some(BudgetStop {
        kind,
        limit,
        used,
        exact_preemptive,
        uncertainty,
    })
}

/// Whether a token or cost budget was configured but had no enforceable basis.
pub fn usage_disclosure(limits: &Limits, usage: &Usage) -> Option<Value> {
    if limits.max_tokens == 0 && limits.max_cost_micros == 0 {
        return None;
    }
    if limits.max_tokens > 0 && usage.prompt_tokens.is_none() && usage.candidate_tokens.is_none() {
        return Some(json!({
            "kind": "tokens",
            "limits": limits,
            "usage": usage,
            "exact_preemptive": false,
            "uncertainty": "the provider reported no token counts; the harness estimate cannot reproduce provider billing, so the supported token budget could not be enforced exactly and missing usage is unavailable, not zero",
        }));
    }
    if limits.max_cost_micros > 0 && usage.estimated_cost_micros.is_none() {
        return Some(json!({
            "kind": "cost",
            "limits": limits,
            "usage": usage,
            "exact_preemptive": false,
            "uncertainty": "no operator-declared provider pricing was available; missing cost is unavailable, not zero",
        }));
    }
    None
}

/// Finalize truthful usage for one task.
///
/// Provider counts stay exactly as reported. Absent provider counts gain a
/// separate local estimate. Cost is derived only from operator-declared pricing
/// over provider-reported token counts; a call whose provider reported no usage
/// gains no cost at all.
pub fn finalize_usage(
    usage: &mut Usage,
    input: &str,
    output: Option<&str>,
    model: Option<&ResolvedModel>,
) {
    if usage.model_calls > 0 && (usage.prompt_tokens.is_none() || usage.candidate_tokens.is_none())
    {
        let mut missing = 0u64;
        if usage.prompt_tokens.is_none() {
            missing = missing.saturating_add(estimate_tokens(input));
        }
        if usage.candidate_tokens.is_none() {
            missing = missing.saturating_add(estimate_tokens(output.unwrap_or_default()));
        }
        if missing > 0 {
            usage.estimated_tokens =
                Some(usage.estimated_tokens.unwrap_or(0).saturating_add(missing));
        }
    }
    if usage.estimated_cost_micros.is_none() {
        usage.estimated_cost_micros = model.and_then(|model| estimated_cost_micros(usage, model));
    }
}

/// Accumulate one native response's provider-reported usage.
///
/// Every optional field stays absent when the provider omitted it. Thinking
/// tokens are recorded as evidence but never added to candidate tokens, so a
/// provider that already counts them does not get priced twice.
pub fn record_model_usage(usage: &mut Usage, metadata: &adk_rust::UsageMetadata) {
    usage.prompt_tokens = Some(
        usage
            .prompt_tokens
            .unwrap_or(0)
            .saturating_add(metadata.prompt_token_count.max(0) as u64),
    );
    usage.candidate_tokens = Some(
        usage
            .candidate_tokens
            .unwrap_or(0)
            .saturating_add(metadata.candidates_token_count.max(0) as u64),
    );
    usage.cache_read_tokens = add_optional(
        usage.cache_read_tokens,
        metadata.cache_read_input_token_count,
    );
    usage.cache_write_tokens = add_optional(
        usage.cache_write_tokens,
        metadata.cache_creation_input_token_count,
    );
    usage.thinking_tokens = add_optional(usage.thinking_tokens, metadata.thinking_token_count);
}

fn add_optional(current: Option<u64>, value: Option<i32>) -> Option<u64> {
    value.map(|value| current.unwrap_or(0).saturating_add(value.max(0) as u64))
}

/// Approximate tokens from UTF-8 text. This is an explicit local estimate.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() / 4).max(1) as u64
}

/// Total token basis used for budget checks: provider values plus a separate
/// estimate for calls whose provider counts were absent.
pub fn total_tokens(usage: &Usage) -> u64 {
    usage
        .prompt_tokens
        .unwrap_or(0)
        .saturating_add(usage.candidate_tokens.unwrap_or(0))
        .saturating_add(usage.estimated_tokens.unwrap_or(0))
}

/// Estimate monetary cost from operator-declared pricing and provider-reported
/// token counts.
///
/// Returns `None` when pricing is absent, the provider reported no prompt or
/// candidate count, or only a local token estimate exists. Cache-read and
/// cache-write tokens are priced separately and subtracted from the uncached
/// input basis so a cached prompt is not billed twice. This never claims
/// provider billing.
pub fn estimated_cost_micros(usage: &Usage, model: &ResolvedModel) -> Option<u64> {
    let input_price = model.input_cost_micros_per_million?;
    let candidate_price = model.candidate_cost_micros_per_million?;
    let prompt_tokens = usage.prompt_tokens?;
    let candidate_tokens = usage.candidate_tokens?;
    let cache_read = usage.cache_read_tokens.unwrap_or(0).min(prompt_tokens);
    let cache_write = usage
        .cache_write_tokens
        .unwrap_or(0)
        .min(prompt_tokens.saturating_sub(cache_read));
    let uncached_input = prompt_tokens
        .saturating_sub(cache_read)
        .saturating_sub(cache_write);
    let input = scale_micros(uncached_input, input_price);
    let candidate = scale_micros(candidate_tokens, candidate_price);
    let cache_read = scale_micros(
        cache_read,
        model
            .cache_read_cost_micros_per_million
            .unwrap_or(input_price),
    );
    let cache_write = scale_micros(
        cache_write,
        model
            .cache_write_cost_micros_per_million
            .unwrap_or(input_price),
    );
    Some(
        input
            .saturating_add(candidate)
            .saturating_add(cache_read)
            .saturating_add(cache_write),
    )
}

fn scale_micros(tokens: u64, micros_per_million: u64) -> u64 {
    let value = u128::from(tokens) * u128::from(micros_per_million);
    value.div_ceil(1_000_000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_provider_counts_stay_absent_and_gain_a_separate_estimate() {
        let mut usage = Usage {
            model_calls: 1,
            ..Usage::default()
        };
        finalize_usage(&mut usage, "hello world", Some("a useful answer"), None);
        assert_eq!(usage.prompt_tokens, None);
        assert_eq!(usage.candidate_tokens, None);
        assert!(usage.estimated_tokens.is_some_and(|value| value > 0));
        assert_eq!(usage.estimated_cost_micros, None);
    }

    #[test]
    fn reported_usage_is_never_turned_into_an_estimate() {
        let mut usage = Usage {
            prompt_tokens: Some(3),
            candidate_tokens: Some(2),
            model_calls: 1,
            ..Usage::default()
        };
        finalize_usage(&mut usage, "hello", Some("answer"), None);
        assert_eq!(usage.estimated_tokens, None);
    }

    #[test]
    fn pending_tools_are_stopped_before_dispatch_when_the_call_budget_is_spent() {
        let limits = Limits {
            max_model_calls: 1,
            ..Limits::default()
        };
        let usage = Usage {
            model_calls: 1,
            ..Usage::default()
        };
        let stop = check_after_model(&limits, &usage, 1).expect("stop");
        assert_eq!(stop.kind, BudgetKind::ModelCalls);
        assert!(stop.exact_preemptive);
    }

    #[test]
    fn token_stop_discloses_post_report_enforcement() {
        let limits = Limits {
            max_tokens: 1,
            ..Limits::default()
        };
        let usage = Usage {
            prompt_tokens: Some(2),
            candidate_tokens: Some(1),
            model_calls: 1,
            ..Usage::default()
        };
        let stop = check_after_model(&limits, &usage, 1).expect("stop");
        assert_eq!(stop.kind, BudgetKind::Tokens);
        assert!(!stop.exact_preemptive);
        assert!(stop.uncertainty.contains("reported"));
    }

    #[test]
    fn a_cost_budget_requires_operator_declared_pricing() {
        let mut profile = crate::RuntimeConfig::local(std::path::PathBuf::from("/tmp"))
            .providers
            .get("deterministic")
            .cloned()
            .expect("built-in provider");
        let limits = Limits {
            max_cost_micros: 1,
            ..Limits::default()
        };
        let error = validate_admission(&limits, &profile, "deterministic")
            .expect_err("unpriceable cost budget must be rejected");
        assert_eq!(error.code, "budget.cost_unverifiable");
        profile.input_cost_micros_per_million = Some(1);
        profile.candidate_cost_micros_per_million = Some(1);
        assert!(validate_admission(&limits, &profile, "deterministic").is_ok());
    }

    #[test]
    fn cost_uses_only_operator_declared_pricing() {
        let model = ResolvedModel {
            provider: "p".into(),
            adapter: "deterministic".into(),
            model: "m".into(),
            local: true,
            residency: "local".into(),
            endpoint: None,
            credential: None,
            input_cost_micros_per_million: Some(1_000_000),
            candidate_cost_micros_per_million: Some(2_000_000),
            cache_read_cost_micros_per_million: None,
            cache_write_cost_micros_per_million: None,
            max_output_tokens: None,
            reasoning: None,
            profile_revision: 1,
        };
        let mut usage = Usage {
            prompt_tokens: Some(2),
            candidate_tokens: Some(1),
            model_calls: 1,
            ..Usage::default()
        };
        finalize_usage(&mut usage, "", None, Some(&model));
        assert_eq!(usage.estimated_cost_micros, Some(4));
    }

    #[test]
    fn cost_is_unavailable_without_provider_reported_usage() {
        let model = price_model(None, None);
        let mut usage = Usage {
            model_calls: 1,
            estimated_tokens: Some(100),
            ..Usage::default()
        };
        finalize_usage(&mut usage, "hello", Some("answer"), Some(&model));
        assert_eq!(
            usage.estimated_cost_micros, None,
            "a local token estimate must not become a cost claim"
        );
    }

    #[test]
    fn cost_prices_cache_read_and_write_separately_without_double_billing() {
        let model = price_model(Some(250_000), Some(1_250_000));
        let usage = Usage {
            prompt_tokens: Some(1_000),
            candidate_tokens: Some(200),
            cache_read_tokens: Some(400),
            cache_write_tokens: Some(100),
            model_calls: 1,
            ..Usage::default()
        };
        // 500 uncached * 1 + 400 read * 0.25 + 100 write * 1.25 + 200 * 2
        assert_eq!(estimated_cost_micros(&usage, &model), Some(1_125));
    }

    #[test]
    fn cache_tokens_without_a_cache_price_fall_back_to_the_input_price() {
        let model = price_model(None, None);
        let usage = Usage {
            prompt_tokens: Some(10),
            candidate_tokens: Some(0),
            cache_read_tokens: Some(4),
            cache_write_tokens: Some(1),
            model_calls: 1,
            ..Usage::default()
        };
        // 5 uncached + 4 read + 1 write, all at the input price.
        assert_eq!(estimated_cost_micros(&usage, &model), Some(10));
    }

    fn price_model(cache_read: Option<u64>, cache_write: Option<u64>) -> ResolvedModel {
        ResolvedModel {
            provider: "p".into(),
            adapter: "openai-compatible".into(),
            model: "m".into(),
            local: false,
            residency: "remote".into(),
            endpoint: None,
            credential: None,
            input_cost_micros_per_million: Some(1_000_000),
            candidate_cost_micros_per_million: Some(2_000_000),
            cache_read_cost_micros_per_million: cache_read,
            cache_write_cost_micros_per_million: cache_write,
            max_output_tokens: None,
            reasoning: None,
            profile_revision: 1,
        }
    }

    #[test]
    fn reported_cache_and_thinking_tokens_are_recorded_without_double_counting() {
        let metadata = adk_rust::UsageMetadata {
            prompt_token_count: 100,
            candidates_token_count: 20,
            total_token_count: 120,
            cache_read_input_token_count: Some(40),
            cache_creation_input_token_count: Some(10),
            thinking_token_count: Some(7),
            ..adk_rust::UsageMetadata::default()
        };
        let mut usage = Usage::default();
        record_model_usage(&mut usage, &metadata);
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.candidate_tokens, Some(20));
        assert_eq!(usage.cache_read_tokens, Some(40));
        assert_eq!(usage.cache_write_tokens, Some(10));
        assert_eq!(usage.thinking_tokens, Some(7));
    }
}
