//! Bounded transport retry policy shared by every SDK operation.

use std::time::Duration;

/// Maximum attempts one SDK operation may make.
pub const MAX_ATTEMPTS: u32 = 8;

/// Maximum delay before a retry attempt.
pub const MAX_DELAY: Duration = Duration::from_secs(5);

/// Bounded retry settings for safe transport failures.
///
/// The SDK retries only failures where repeating the same request identity is
/// safe: connection loss, a lost acknowledgment body, and explicitly retryable
/// Gateway availability errors. Rate-limit, authorization, validation, and
/// protocol errors are returned immediately as typed [`common::ApiError`]
/// values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u32,
    initial_delay: Duration,
    max_delay: Duration,
}

impl RetryPolicy {
    /// Build a policy limited to 1-8 attempts and a delay ceiling of 5 seconds.
    pub fn new(max_attempts: u32, initial_delay: Duration, max_delay: Duration) -> Self {
        let max_delay = max_delay.min(MAX_DELAY);
        let initial_delay = initial_delay.min(max_delay);
        Self {
            max_attempts: max_attempts.clamp(1, MAX_ATTEMPTS),
            initial_delay,
            max_delay,
        }
    }

    /// Maximum attempts, including the first one.
    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    /// First retry delay.
    pub fn initial_delay(&self) -> Duration {
        self.initial_delay
    }

    /// Retry delay ceiling.
    pub fn max_delay(&self) -> Duration {
        self.max_delay
    }

    /// Exponential delay before attempt `completed + 1`, capped by the policy.
    pub(crate) fn delay_after(&self, completed: u32) -> Duration {
        let exponent = completed.saturating_sub(1).min(16);
        let factor = 2u32.saturating_pow(exponent);
        self.initial_delay
            .saturating_mul(factor)
            .min(self.max_delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(3, Duration::from_millis(100), Duration::from_secs(1))
    }
}
