//! Budget ceilings (tokens / wall-time / tool-calls) and running usage, with
//! fully-implemented charge/check arithmetic (knowledge-surfaces §4.1, §4.3).

use std::fmt;

use jiff::SignedDuration;
use serde::{Deserialize, Serialize};

use crate::error::{JobRuntimeError, JobRuntimeResult};

/// Which ceiling a [`JobRuntimeError::BudgetExceeded`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetKind {
    /// Model token consumption.
    Tokens,
    /// Elapsed wall-clock time.
    WallTime,
    /// Number of tool invocations.
    ToolCalls,
}

impl fmt::Display for BudgetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tokens => "tokens",
            Self::WallTime => "wall-time",
            Self::ToolCalls => "tool-calls",
        })
    }
}

/// Optional ceilings a governed job may not exceed; `None` means unbounded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    /// Maximum model tokens, or `None` for unbounded.
    pub max_tokens: Option<u64>,
    /// Maximum wall-clock duration, or `None` for unbounded.
    pub max_wall: Option<SignedDuration>,
    /// Maximum tool invocations, or `None` for unbounded.
    pub max_tool_calls: Option<u32>,
}

/// Running total charged against a [`Budget`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetUsage {
    /// Tokens consumed so far.
    pub tokens: u64,
    /// Wall-clock time consumed so far.
    pub wall: SignedDuration,
    /// Tool invocations made so far.
    pub tool_calls: u32,
}

impl Default for BudgetUsage {
    fn default() -> Self {
        Self {
            tokens: 0,
            wall: SignedDuration::ZERO,
            tool_calls: 0,
        }
    }
}

impl Budget {
    /// Construct an unbounded budget (every ceiling `None`).
    #[must_use]
    pub fn unbounded() -> Self {
        Self {
            max_tokens: None,
            max_wall: None,
            max_tool_calls: None,
        }
    }

    /// Charge `amount` tokens onto `usage`, failing if the token ceiling is passed.
    pub fn charge_tokens(&self, usage: &mut BudgetUsage, amount: u64) -> JobRuntimeResult<()> {
        let new = usage.tokens.saturating_add(amount);
        if let Some(limit) = self.max_tokens {
            if new > limit {
                return Err(JobRuntimeError::BudgetExceeded {
                    kind: BudgetKind::Tokens,
                    used: new,
                    limit,
                });
            }
        }
        usage.tokens = new;
        Ok(())
    }

    /// Charge one tool call onto `usage`, failing if the tool-call ceiling is passed.
    pub fn charge_tool_call(&self, usage: &mut BudgetUsage) -> JobRuntimeResult<()> {
        let new = usage.tool_calls.saturating_add(1);
        if let Some(limit) = self.max_tool_calls {
            if new > limit {
                return Err(JobRuntimeError::BudgetExceeded {
                    kind: BudgetKind::ToolCalls,
                    used: u64::from(new),
                    limit: u64::from(limit),
                });
            }
        }
        usage.tool_calls = new;
        Ok(())
    }

    /// Charge `elapsed` wall-time onto `usage`, failing if the wall-time ceiling is passed.
    pub fn charge_wall(
        &self,
        usage: &mut BudgetUsage,
        elapsed: SignedDuration,
    ) -> JobRuntimeResult<()> {
        if elapsed.is_negative() {
            return Err(JobRuntimeError::NegativeWallCharge { duration: elapsed });
        }

        let new = usage
            .wall
            .checked_add(elapsed)
            .ok_or(JobRuntimeError::BudgetExceeded {
                kind: BudgetKind::WallTime,
                used: u64::MAX,
                limit: self.max_wall.map_or(0, secs_u64),
            })?;
        if let Some(limit) = self.max_wall {
            if new > limit {
                return Err(JobRuntimeError::BudgetExceeded {
                    kind: BudgetKind::WallTime,
                    used: secs_u64(new),
                    limit: secs_u64(limit),
                });
            }
        }
        usage.wall = new;
        Ok(())
    }

    /// Verify current `usage` sits within every ceiling without mutating it.
    pub fn check(&self, usage: &BudgetUsage) -> JobRuntimeResult<()> {
        if let Some(limit) = self.max_tokens {
            if usage.tokens > limit {
                return Err(JobRuntimeError::BudgetExceeded {
                    kind: BudgetKind::Tokens,
                    used: usage.tokens,
                    limit,
                });
            }
        }
        if let Some(limit) = self.max_tool_calls {
            if usage.tool_calls > limit {
                return Err(JobRuntimeError::BudgetExceeded {
                    kind: BudgetKind::ToolCalls,
                    used: u64::from(usage.tool_calls),
                    limit: u64::from(limit),
                });
            }
        }
        if let Some(limit) = self.max_wall {
            if usage.wall > limit {
                return Err(JobRuntimeError::BudgetExceeded {
                    kind: BudgetKind::WallTime,
                    used: secs_u64(usage.wall),
                    limit: secs_u64(limit),
                });
            }
        }
        Ok(())
    }
}

/// Whole seconds of a duration, clamped at zero (for human-readable errors).
fn secs_u64(d: SignedDuration) -> u64 {
    let s = d.as_secs();
    if s < 0 { 0 } else { s as u64 }
}

#[cfg(test)]
mod tests {
    use super::{Budget, BudgetUsage};
    use crate::error::JobRuntimeError;
    use crate::test_support::{TestError, TestResult, ctx};
    use jiff::SignedDuration;

    #[test]
    fn charge_wall_rejects_negative_full_second_without_mutating_usage() -> TestResult {
        let budget = Budget::unbounded();
        let mut usage = BudgetUsage {
            tokens: 12,
            wall: SignedDuration::from_secs(3),
            tool_calls: 2,
        };
        let original_usage = usage.clone();
        let duration = SignedDuration::from_secs(-1);

        let Err(error) = budget.charge_wall(&mut usage, duration) else {
            return Err(TestError::Unexpected(
                "negative wall charges must be rejected".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            JobRuntimeError::NegativeWallCharge {
                duration: captured,
            } if captured == duration
        ));
        assert_eq!(usage, original_usage);
        Ok(())
    }

    #[test]
    fn charge_wall_rejects_negative_sub_second_without_mutating_usage() -> TestResult {
        let budget = Budget::unbounded();
        let mut usage = BudgetUsage {
            tokens: 12,
            wall: SignedDuration::from_secs(3),
            tool_calls: 2,
        };
        let original_usage = usage.clone();
        let duration = SignedDuration::from_millis(-1);

        let Err(error) = budget.charge_wall(&mut usage, duration) else {
            return Err(TestError::Unexpected(
                "negative wall charges must be rejected".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            JobRuntimeError::NegativeWallCharge {
                duration: captured,
            } if captured == duration
        ));
        assert_eq!(usage, original_usage);
        Ok(())
    }

    #[test]
    fn charge_wall_accepts_zero_and_positive_durations() -> TestResult {
        let budget = Budget::unbounded();
        let mut usage = BudgetUsage::default();

        budget
            .charge_wall(&mut usage, SignedDuration::ZERO)
            .map_err(ctx("zero wall charge must be accepted"))?;
        budget
            .charge_wall(&mut usage, SignedDuration::from_millis(1500))
            .map_err(ctx("positive wall charge must be accepted"))?;

        assert_eq!(usage.wall, SignedDuration::from_millis(1500));
        Ok(())
    }
}
