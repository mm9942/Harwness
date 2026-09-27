//! Error surface of the job runtime (`JobRuntimeError`), built with the
//! `harw-macros::HarwError` derive per the shared error contract. The derive
//! emits `Display`, `std::error::Error`, the `#[from]` conversions and the
//! `JobRuntimeResult<T>` alias (enum name ends in `Error`).

use harw_macros::HarwError;
use harw_types::WorkId;

use crate::budget::BudgetKind;
use crate::job::JobState;

/// Central error type of the job governance primitives.
///
/// The `HarwError` derive additionally emits `pub type JobRuntimeResult<T>`.
#[derive(Debug, HarwError)]
pub enum JobRuntimeError {
    /// A budget ceiling (tokens / wall-time / tool-calls) was exceeded.
    #[msg("budget exceeded on {kind}: used {used} exceeds ceiling {limit}")]
    BudgetExceeded {
        kind: BudgetKind,
        used: u64,
        limit: u64,
    },

    /// A wall-clock charge cannot move budget usage backwards.
    #[msg("wall-clock charge duration cannot be negative: {duration}")]
    NegativeWallCharge { duration: jiff::SignedDuration },

    /// A lease was used past its expiry instant.
    #[msg("lease on work {work_id} expired at {expired_at}")]
    LeaseExpired {
        work_id: WorkId,
        expired_at: jiff::Timestamp,
    },

    /// A claim was attempted on work already held by another holder.
    #[msg("lease on work {work_id} is already held by '{holder}'")]
    LeaseContended { work_id: WorkId, holder: String },

    /// The retry policy has no attempts left.
    #[msg("retry policy exhausted after {attempts} attempt(s)")]
    RetryExhausted { attempts: u32 },

    /// A lifecycle operation was attempted from a state that does not permit it.
    #[msg("work {work_id} is {actual:?}; operation requires {expected:?}")]
    InvalidState {
        work_id: WorkId,
        expected: JobState,
        actual: JobState,
    },

    /// Timestamp arithmetic overflowed the representable `jiff` range.
    #[from]
    Time(jiff::Error),
}

#[cfg(test)]
mod tests {
    use super::JobRuntimeError;

    #[test]
    fn negative_wall_charge_displays_signed_duration() {
        let duration = jiff::SignedDuration::from_millis(-1500);
        let error = JobRuntimeError::NegativeWallCharge { duration };

        assert_eq!(
            error.to_string(),
            format!("wall-clock charge duration cannot be negative: {duration}")
        );
    }
}
