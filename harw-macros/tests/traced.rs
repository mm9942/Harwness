//! Positive integration tests for the `#[traced]` attribute macro.
//!
//! Requires `tracing` and `harw-observe` (for `Redact`/`Redacted`, via
//! `#[derive(harw_macros::Redact)]`) as dev-dependencies; see
//! `harw-macros/Cargo.toml`. No `tracing` subscriber is installed — `tracing`
//! degrades gracefully to no-op spans without one, so these tests only need
//! to prove the annotated functions still behave correctly, not that any
//! particular span/field text was recorded.
//!
//! # Design-doc reference
//! AW1-02-Brief (`#[traced]`).

use harw_macros::{Redact, traced};

mod common;
use common::{TestError, TestResult};

/// Argument type carrying a `Redact` implementation via the sibling
/// `#[derive(Redact)]` macro (AW0-02), so `#[traced(fields(...))]` has a
/// legitimate `Redact`-implementing type to call.
#[derive(Redact, Clone, Copy)]
struct TracedNum {
    #[redact(show)]
    value: i32,
}

// ── `fn` form ────────────────────────────────────────────────────────────────

#[traced(level = "info", fields(n))]
fn traced_sync_double(n: TracedNum) -> i32 {
    n.value * 2
}

#[test]
fn traced_sync_fn_returns_expected_value() {
    let result = traced_sync_double(TracedNum { value: 21 });
    assert_eq!(result, 42);
}

// ── `async fn` form ────────────────────────────────────────────────────────

#[traced(level = "debug", fields(a, b))]
async fn traced_async_add(a: TracedNum, b: TracedNum) -> i32 {
    a.value + b.value
}

#[test]
fn traced_async_fn_executes_and_returns_value() -> TestResult {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    let fut = pin!(traced_async_add(
        TracedNum { value: 2 },
        TracedNum { value: 3 }
    ));
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    match fut.poll(&mut cx) {
        Poll::Ready(v) => assert_eq!(v, 5),
        Poll::Pending => {
            return Err(TestError::Unexpected(
                "async body has no internal .await; must resolve on first poll".to_owned(),
            ));
        }
    }
    Ok(())
}

// ── bare `#[traced]` (no attribute arguments) — defaults must compile ───────

#[traced]
fn traced_bare_defaults_to_info_and_no_fields() -> &'static str {
    "ok"
}

#[test]
fn traced_bare_attribute_compiles_and_runs() {
    assert_eq!(traced_bare_defaults_to_info_and_no_fields(), "ok");
}
