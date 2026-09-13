//! Exponential retry-backoff policy with a pure `next_delay` calculation
//! (knowledge-surfaces §6.2 "retry/backoff policy"). Execution of the schedule
//! (sleeping/awaiting) is left to the caller / a future `backon` integration.

use jiff::SignedDuration;
use serde::{de, Deserialize, Serialize};

use crate::error::{JobRuntimeError, JobRuntimeResult};

/// Parameters of a capped exponential backoff schedule.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RetryPolicy {
    /// Total attempts permitted before the policy is exhausted.
    pub max_attempts: u32,
    /// Delay used for attempt 0 (the base of the exponential curve).
    pub base_delay: SignedDuration,
    /// Multiplier applied per attempt (e.g. `2.0` doubles each time).
    pub factor: f64,
    /// Upper bound on any single computed delay.
    pub max_delay: SignedDuration,
}

impl<'de> Deserialize<'de> for RetryPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawRetryPolicy {
            max_attempts: u32,
            base_delay: SignedDuration,
            factor: f64,
            max_delay: SignedDuration,
        }

        let raw = RawRetryPolicy::deserialize(deserializer)?;
        Self::try_new(raw.max_attempts, raw.base_delay, raw.factor, raw.max_delay)
            .map_err(|error| de::Error::custom(error.to_string()))
    }
}

impl RetryPolicy {
    /// Construct a retry policy after checking all values that affect delay
    /// calculation. Direct struct literals remain supported for compatibility;
    /// deserialization uses this constructor as its validation boundary.
    pub fn try_new(
        max_attempts: u32,
        base_delay: SignedDuration,
        factor: f64,
        max_delay: SignedDuration,
    ) -> JobRuntimeResult<Self> {
        let policy = Self {
            max_attempts,
            base_delay,
            factor,
            max_delay,
        };
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> JobRuntimeResult<()> {
        if !(self.factor.is_finite() && self.factor > 0.0)
            || self.base_delay.is_negative()
            || self.max_delay.is_negative()
        {
            return Err(JobRuntimeError::RetryExhausted { attempts: 0 });
        }
        Ok(())
    }

    /// Delay for an attempt count; attempt 1 is the first retry and uses the base delay.
    pub fn next_delay(&self, attempt: u32) -> JobRuntimeResult<SignedDuration> {
        if self.base_delay.is_negative() || self.max_delay.is_negative() {
            return Err(JobRuntimeError::RetryExhausted { attempts: attempt });
        }
        if !(self.factor.is_finite() && self.factor > 0.0) {
            return Err(JobRuntimeError::RetryExhausted { attempts: attempt });
        }
        if attempt >= self.max_attempts {
            return Err(JobRuntimeError::RetryExhausted { attempts: attempt });
        }
        let base_ms = self.base_delay.as_millis().max(0) as f64;
        let max_ms = self.max_delay.as_millis().max(0);
        // `Job::record_failure` increments attempts before asking for the
        // delay, so attempt 1 is the first retry and must still use base_delay.
        let exponent = attempt.saturating_sub(1) as f64;
        let scaled = base_ms * self.factor.powf(exponent);
        let chosen = if max_ms > 0 {
            scaled.min(max_ms as f64)
        } else {
            scaled
        };
        let ms = if chosen.is_finite() && chosen >= 0.0 {
            chosen.min(i64::MAX as f64) as i64
        } else {
            clamp_i128(max_ms)
        };
        Ok(SignedDuration::from_millis(ms))
    }
}

/// Clamp an `i128` millisecond count into the `i64` range accepted by `SignedDuration`.
fn clamp_i128(v: i128) -> i64 {
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < 0 {
        0
    } else {
        v as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(factor: f64) -> RetryPolicy {
        RetryPolicy {
            max_attempts: 4,
            base_delay: SignedDuration::from_secs(1),
            factor,
            max_delay: SignedDuration::from_secs(30),
        }
    }

    #[test]
    fn first_retry_uses_the_base_delay() {
        let retry = policy(2.0);
        assert_eq!(retry.next_delay(0).unwrap(), SignedDuration::from_secs(1));
        assert_eq!(retry.next_delay(1).unwrap(), SignedDuration::from_secs(1));
        assert_eq!(retry.next_delay(2).unwrap(), SignedDuration::from_secs(2));
        assert_eq!(retry.next_delay(3).unwrap(), SignedDuration::from_secs(4));
    }

    #[test]
    fn retry_rejects_invalid_factors() {
        for factor in [f64::NAN, f64::NEG_INFINITY, -1.0, 0.0, f64::INFINITY] {
            match policy(factor).next_delay(0) {
                Err(JobRuntimeError::RetryExhausted { attempts: 0 }) => {}
                result => panic!("invalid factor returned unexpected result: {result:?}"),
            }
        }
    }

    #[test]
    fn construction_rejects_invalid_factor_and_negative_delays() {
        for factor in [f64::NAN, f64::NEG_INFINITY, -1.0, 0.0, f64::INFINITY] {
            assert!(RetryPolicy::try_new(
                4,
                SignedDuration::from_secs(1),
                factor,
                SignedDuration::from_secs(30),
            )
            .is_err());
        }

        for (base_delay, max_delay) in [
            (
                SignedDuration::from_millis(-1),
                SignedDuration::from_secs(30),
            ),
            (
                SignedDuration::from_secs(1),
                SignedDuration::from_millis(-1),
            ),
        ] {
            assert!(RetryPolicy::try_new(4, base_delay, 2.0, max_delay).is_err());
        }
    }

    #[test]
    fn next_delay_rejects_negative_delays_from_legacy_literals() {
        for (base_delay, max_delay) in [
            (
                SignedDuration::from_millis(-1),
                SignedDuration::from_secs(30),
            ),
            (
                SignedDuration::from_secs(1),
                SignedDuration::from_millis(-1),
            ),
        ] {
            let retry = RetryPolicy {
                max_attempts: 4,
                base_delay,
                factor: 2.0,
                max_delay,
            };
            assert!(matches!(
                retry.next_delay(1),
                Err(JobRuntimeError::RetryExhausted { attempts: 1 })
            ));
        }
    }

    #[test]
    fn deserialization_rejects_invalid_factor_and_negative_delays() {
        let valid = serde_json::json!({
            "max_attempts": 4,
            "base_delay": "1s",
            "factor": 2.0,
            "max_delay": "30s"
        });

        for invalid in [
            serde_json::json!({ "factor": 0.0 }),
            serde_json::json!({ "factor": -1.0 }),
            serde_json::json!({ "base_delay": "-1ms" }),
            serde_json::json!({ "max_delay": "-1ms" }),
        ] {
            let mut value = valid.clone();
            for (key, replacement) in invalid.as_object().expect("test object") {
                value[key] = replacement.clone();
            }
            assert!(serde_json::from_value::<RetryPolicy>(value).is_err());
        }
    }
}
