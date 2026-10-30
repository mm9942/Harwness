//! Conversion of `bpf_ktime_get_ns()` values to wall clock with an explicit
//! confidence indicator.  A kernel monotonic timestamp is never decoded as a
//! Unix epoch timestamp.

use jiff::Timestamp;

/// Whether a wall-clock mapping is still based on the same sampled relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeConfidence {
    /// The event is close to the mapping sample and no discontinuity is known.
    Measured,
    /// A clock step or suspend/resume makes the displayed wall time only an
    /// estimate; the original monotonic time remains authoritative.
    Uncertain,
}

/// One measured relation between `CLOCK_MONOTONIC` and realtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelTimeMapper {
    sampled_ktime_ns: u64,
    sampled_realtime: Timestamp,
    confidence: TimeConfidence,
}

impl KernelTimeMapper {
    /// Sample the two clocks that define the conversion.  The caller should
    /// resample around a detected realtime step or suspend/resume and then
    /// retain the previous mapping with [`TimeConfidence::Uncertain`] for
    /// affected queued records.
    pub fn sample() -> Option<Self> {
        let monotonic = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let realtime = rustix::time::clock_gettime(rustix::time::ClockId::Realtime);
        Some(Self::measured(
            timespec_nanoseconds(monotonic)?,
            Timestamp::from_nanosecond(i128::from(timespec_nanoseconds(realtime)?)).ok()?,
        ))
    }
    #[must_use]
    pub fn measured(sampled_ktime_ns: u64, sampled_realtime: Timestamp) -> Self {
        Self {
            sampled_ktime_ns,
            sampled_realtime,
            confidence: TimeConfidence::Measured,
        }
    }

    #[must_use]
    pub fn after_discontinuity(sampled_ktime_ns: u64, sampled_realtime: Timestamp) -> Self {
        Self {
            sampled_ktime_ns,
            sampled_realtime,
            confidence: TimeConfidence::Uncertain,
        }
    }

    #[must_use]
    pub fn confidence(&self) -> TimeConfidence {
        self.confidence
    }

    pub fn map(&self, ktime_ns: u64) -> Option<Timestamp> {
        let delta = i128::from(ktime_ns) - i128::from(self.sampled_ktime_ns);
        Timestamp::from_nanosecond(self.sampled_realtime.as_nanosecond().checked_add(delta)?).ok()
    }
}

fn timespec_nanoseconds(timespec: rustix::time::Timespec) -> Option<u64> {
    let seconds = u64::try_from(timespec.tv_sec).ok()?;
    let nanoseconds = u64::try_from(timespec.tv_nsec).ok()?;
    seconds.checked_mul(1_000_000_000)?.checked_add(nanoseconds)
}

#[cfg(test)]
mod tests {
    use super::{KernelTimeMapper, TimeConfidence};
    use crate::test_support::{TestError, TestResult, ctx};
    use jiff::Timestamp;

    #[test]
    fn maps_monotonic_delta_without_treating_ktime_as_epoch() -> TestResult {
        let mapper = KernelTimeMapper::measured(
            900,
            Timestamp::new(1_700_000_000, 0).map_err(ctx("gültiger Zeitstempel"))?,
        );
        let mapped = mapper
            .map(1_900)
            .ok_or(TestError::Missing("gemapptes Timestamp"))?;
        assert_eq!(
            mapped,
            Timestamp::new(1_700_000_000, 1_000).map_err(ctx("gültiger Zeitstempel"))?
        );
        Ok(())
    }

    #[test]
    fn discontinuity_is_visible_to_consumers() {
        let mapper = KernelTimeMapper::after_discontinuity(10, Timestamp::UNIX_EPOCH);
        assert_eq!(mapper.confidence(), TimeConfidence::Uncertain);
    }

    #[test]
    fn sample_maps_its_own_monotonic_instant_to_realtime() -> TestResult {
        let mapper = KernelTimeMapper::sample().ok_or(TestError::Missing(
            "Linux realtime and monotonic clocks fit the wire range",
        ))?;
        // The test deliberately asserts only a usable mapping, not an exact
        // wall-clock value, because the two clock syscalls are distinct reads.
        assert!(mapper.map(0).is_some());
        assert_eq!(mapper.confidence(), TimeConfidence::Measured);
        Ok(())
    }
}
