//! Backoff and resume-cursor tests (S07). Panic-free style: tests return
//! `Result`, no unwrap/expect/indexing.

use std::error::Error;
use std::time::Duration;

use harw_protocol::Cursor;
use harw_session_remote::{BackoffPolicy, ResumeCursors};
use harw_types::SessionId;

type TestResult = Result<(), Box<dyn Error>>;

/// Deterministic xorshift for property-style sweeps.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn policy(initial_ms: u64, max_ms: u64, jitter: u16) -> BackoffPolicy {
    BackoffPolicy {
        initial: Duration::from_millis(initial_ms),
        max: Duration::from_millis(max_ms),
        jitter_permille: jitter,
    }
}

#[test]
fn doubles_then_caps_without_jitter() -> TestResult {
    let p = policy(100, 1000, 0);
    let got: Result<Vec<_>, _> = (0..6).map(|a| p.delay(a, u64::MAX)).collect();
    let ms: Vec<u128> = got?.iter().map(Duration::as_millis).collect();
    assert_eq!(ms, vec![100, 200, 400, 800, 1000, 1000]);
    Ok(())
}

#[test]
fn deterministic_for_same_inputs() -> TestResult {
    let p = BackoffPolicy::default();
    assert_eq!(p.delay(3, 12345)?, p.delay(3, 12345)?);
    Ok(())
}

#[test]
fn jitter_only_shortens_and_spans_range() -> TestResult {
    let p = policy(1000, 1_000_000, 200);
    assert_eq!(p.delay(0, 0)?, Duration::from_millis(1000));
    let low = p.delay(0, u64::MAX)?;
    assert!(low < Duration::from_millis(1000));
    assert!(low >= Duration::from_millis(800));
    // Half entropy lands near the middle of the jitter window.
    let mid = p.delay(0, u64::MAX / 2)?;
    assert!(mid >= Duration::from_millis(899) && mid <= Duration::from_millis(901));
    Ok(())
}

#[test]
fn full_jitter_can_reach_near_zero_but_not_below() -> TestResult {
    let p = policy(1000, 1000, 1000);
    assert_eq!(p.delay(0, 0)?, Duration::from_secs(1));
    assert!(p.delay(0, u64::MAX)? <= Duration::from_nanos(1));
    Ok(())
}

#[test]
fn jitter_above_1000_is_clamped() -> TestResult {
    let wild = policy(1000, 1000, u16::MAX);
    let full = policy(1000, 1000, 1000);
    for e in [0, 1, u64::MAX / 3, u64::MAX] {
        assert_eq!(wild.delay(0, e)?, full.delay(0, e)?);
    }
    Ok(())
}

#[test]
fn degenerate_policies_are_safe() -> TestResult {
    // initial above max is clamped to max.
    assert_eq!(policy(5000, 1000, 0).delay(0, 0)?, Duration::from_secs(1));
    // zero initial and zero max stay zero.
    assert_eq!(policy(0, 1000, 500).delay(40, 9)?, Duration::ZERO);
    assert_eq!(policy(10, 0, 500).delay(2, 9)?, Duration::ZERO);
    Ok(())
}

#[test]
fn extreme_values_do_not_overflow() -> TestResult {
    let huge = BackoffPolicy {
        initial: Duration::MAX,
        max: Duration::MAX,
        jitter_permille: 1000,
    };
    for attempt in [0, 1, 63, 64, 99, 100, 101, 1000, u32::MAX] {
        for entropy in [0, 1, u64::MAX / 2, u64::MAX] {
            let d = huge.delay(attempt, entropy)?;
            assert!(d <= Duration::MAX);
        }
    }
    assert_eq!(huge.delay(u32::MAX, 0)?, Duration::MAX);
    let default = BackoffPolicy::default();
    assert_eq!(default.delay(u32::MAX, 0)?, default.max);
    Ok(())
}

#[test]
fn property_bounds_and_monotonic_ceiling() -> TestResult {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..5000 {
        let initial = rng.next() % 5_000;
        let max = rng.next() % 100_000;
        let jitter = u16::try_from(rng.next() % 1200)?;
        let p = policy(initial, max, jitter);
        let attempt = u32::try_from(rng.next() % 200)?;
        let entropy = rng.next();
        let d = p.delay(attempt, entropy)?;
        assert!(d <= p.max, "exceeds cap");
        // Zero entropy gives the un-jittered ceiling, which never shrinks
        // with more attempts and is never below the jittered value.
        let ceil = p.delay(attempt, 0)?;
        assert!(d <= ceil);
        assert!(p.delay(attempt.saturating_add(1), 0)? >= ceil);
        // Jitter never removes more than the permille share (+1ns rounding).
        let permille = u128::from(jitter.min(1000));
        let floor = ceil.as_nanos() - ceil.as_nanos() * permille / 1000;
        assert!(d.as_nanos() + 1 >= floor);
    }
    Ok(())
}

fn sid(s: &str) -> Result<SessionId, Box<dyn Error>> {
    Ok(SessionId::try_from_str(s)?)
}

fn cur(generation: u32, durable: u64, live: u32) -> Cursor {
    Cursor {
        generation,
        durable,
        live,
    }
}

#[test]
fn cursor_absent_then_recorded() -> TestResult {
    let mut c = ResumeCursors::new();
    let s = sid("s1")?;
    assert_eq!(c.cursor(&s)?, None);
    c.record(&s, cur(1, 5, 2))?;
    assert_eq!(c.cursor(&s)?, Some(cur(1, 5, 2)));
    assert_eq!(c.cursor(&sid("other")?)?, None);
    Ok(())
}

#[test]
fn record_is_monotonic_within_generation() -> TestResult {
    let mut c = ResumeCursors::new();
    let s = sid("s1")?;
    c.record(&s, cur(1, 10, 3))?;
    c.record(&s, cur(1, 9, 99))?; // older durable: ignored even with higher live
    assert_eq!(c.cursor(&s)?, Some(cur(1, 10, 3)));
    c.record(&s, cur(1, 10, 2))?; // same durable, lower live: ignored
    assert_eq!(c.cursor(&s)?, Some(cur(1, 10, 3)));
    c.record(&s, cur(1, 10, 4))?;
    assert_eq!(c.cursor(&s)?, Some(cur(1, 10, 4)));
    c.record(&s, cur(1, 11, 0))?; // durable advance resets live
    assert_eq!(c.cursor(&s)?, Some(cur(1, 11, 0)));
    Ok(())
}

#[test]
fn generation_change_resets_even_when_durable_lower() -> TestResult {
    let mut c = ResumeCursors::new();
    let s = sid("s1")?;
    c.record(&s, cur(1, 500, 7))?;
    c.record(&s, cur(2, 3, 0))?;
    assert_eq!(c.cursor(&s)?, Some(cur(2, 3, 0)));
    // A stale older generation never moves it back.
    c.record(&s, cur(1, 900, 9))?;
    assert_eq!(c.cursor(&s)?, Some(cur(2, 3, 0)));
    Ok(())
}

#[test]
fn sessions_are_independent_and_clones_diverge() -> TestResult {
    let mut c = ResumeCursors::new();
    let (a, b) = (sid("a")?, sid("b")?);
    c.record(&a, cur(1, 1, 0))?;
    c.record(&b, cur(3, 9, 9))?;
    let snapshot = c.clone();
    c.record(&a, cur(1, 2, 0))?;
    assert_eq!(snapshot.cursor(&a)?, Some(cur(1, 1, 0)));
    assert_eq!(c.cursor(&a)?, Some(cur(1, 2, 0)));
    assert_eq!(c.cursor(&b)?, Some(cur(3, 9, 9)));
    Ok(())
}

#[test]
fn boundary_values_and_property_never_regress() -> TestResult {
    let mut c = ResumeCursors::new();
    let s = sid("s")?;
    c.record(&s, cur(u32::MAX, u64::MAX, u32::MAX))?;
    c.record(&s, cur(0, 0, 0))?;
    assert_eq!(c.cursor(&s)?, Some(cur(u32::MAX, u64::MAX, u32::MAX)));

    let mut c = ResumeCursors::new();
    let mut rng = Rng(42);
    let mut best = Cursor::default();
    for _ in 0..2000 {
        let next = cur(
            u32::try_from(rng.next() % 4)?,
            rng.next() % 6,
            u32::try_from(rng.next() % 4)?,
        );
        c.record(&s, next)?;
        best = best.max(next);
        assert_eq!(c.cursor(&s)?, Some(best));
    }
    Ok(())
}
