//! # wait
//!
//! ## Responsibility
//! Blocking-until-condition primitives: the closed set of conditions a caller
//! may wait for ([`WaitCondition`]), the bounded timeout ([`WaitTimeout`]) and
//! the outcome envelope ([`WaitOutcome`]). Execution belongs to the backend
//! ([`crate::host::BrowserRuntime::wait`]).
//!
//! ## Security notes (remediation C-BROWSER, F-008)
//! - There is **no custom-script condition**: arbitrary JavaScript predicates
//!   are not representable. JSON naming `CustomScript`/`script` is rejected by
//!   deserialization (unknown variant). `ScriptMessage` only *listens* for a
//!   message on a page-bridge channel; it executes nothing.
//! - Unknown fields are rejected everywhere.
//! - [`WaitTimeout`] serializes as integer milliseconds and deserializes only
//!   within `1..=HARD_MAX_WAIT_MS`; callers must additionally run
//!   [`WaitTimeout::validate`] and [`WaitCondition::validate`] with the
//!   session's [`crate::policy::BrowserLimits`].
//!
//! ## Concurrency
//! Plain data and pure functions, `Send + Sync`.
//!
//! ## Errors
//! - [`crate::error::Error::InvalidArgument`]: a limit is violated.
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::policy::BrowserLimits;
//! use harw_browser::wait::{WaitCondition, WaitTimeout};
//!
//! let limits = BrowserLimits::default();
//! assert!(WaitCondition::NavigationComplete.validate(&limits).is_ok());
//! assert!(WaitTimeout::from_millis(0).validate(&limits).is_err());
//! ```

use crate::action::validate_target;
use crate::error::{Error, Result};
use crate::policy::{BrowserLimits, HARD_MAX_WAIT_MS};
use crate::selector::Target;

/// Maximum byte length of a `ScriptMessage` channel name.
pub const MAX_SCRIPT_CHANNEL_BYTES: usize = 128;

/// The closed set of conditions a caller may block on.
///
/// # Description
/// Element conditions carry a [`Target`]; pattern conditions carry a string
/// bounded by [`BrowserLimits::max_text_bytes`]. No variant executes
/// caller-supplied code.
///
/// # Concurrency
/// Plain data, `Send + Sync`.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::wait::WaitCondition;
///
/// let condition = WaitCondition::UrlMatches("https://erp.example.com/done".to_owned());
/// assert!(matches!(condition, WaitCondition::UrlMatches(_)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum WaitCondition {
    ElementPresent(Target),
    ElementVisible(Target),
    ElementClickable(Target),
    ElementGone(Target),
    UrlMatches(String),
    TitleMatches(String),
    NavigationComplete,
    NetworkQuiescence { idle_ms: u64 },
    RequestObserved { path_contains: String },
    LogMatches(String),
    ScriptMessage { channel: String },
    DownloadComplete,
}

impl WaitCondition {
    /// Validates the condition's payload against `limits`.
    ///
    /// # Description
    /// Targets via [`validate_target`]; `UrlMatches`, `TitleMatches`,
    /// `LogMatches` and `RequestObserved::path_contains` at most
    /// [`BrowserLimits::max_text_bytes`]; `NetworkQuiescence::idle_ms` within
    /// `1..=max_wait_ms`; `ScriptMessage::channel` non-empty, at most
    /// [`MAX_SCRIPT_CHANNEL_BYTES`] of `[A-Za-z0-9._:-]`.
    ///
    /// # Arguments
    /// - `limits` (`&BrowserLimits`): borrowed.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: any limit is violated.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::BrowserLimits;
    /// use harw_browser::wait::WaitCondition;
    ///
    /// let condition = WaitCondition::NetworkQuiescence { idle_ms: 0 };
    /// assert!(condition.validate(&BrowserLimits::default()).is_err());
    /// ```
    pub fn validate(&self, limits: &BrowserLimits) -> Result<()> {
        match self {
            Self::ElementPresent(target)
            | Self::ElementVisible(target)
            | Self::ElementClickable(target)
            | Self::ElementGone(target) => validate_target(target, limits),
            Self::UrlMatches(pattern) | Self::TitleMatches(pattern) | Self::LogMatches(pattern) => {
                check_pattern_len(pattern, limits)
            }
            Self::RequestObserved { path_contains } => check_pattern_len(path_contains, limits),
            Self::NetworkQuiescence { idle_ms } => {
                if *idle_ms == 0 || *idle_ms > limits.max_wait_ms() {
                    return Err(Error::InvalidArgument {
                        detail: format!(
                            "network quiescence idle_ms must be 1..={}",
                            limits.max_wait_ms()
                        ),
                    });
                }
                Ok(())
            }
            Self::ScriptMessage { channel } => {
                let well_formed = !channel.is_empty()
                    && channel.len() <= MAX_SCRIPT_CHANNEL_BYTES
                    && channel.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-')
                    });
                if !well_formed {
                    return Err(Error::InvalidArgument {
                        detail: format!(
                            "script message channel must be 1..={MAX_SCRIPT_CHANNEL_BYTES} bytes of [A-Za-z0-9._:-]"
                        ),
                    });
                }
                Ok(())
            }
            Self::NavigationComplete | Self::DownloadComplete => Ok(()),
        }
    }
}

fn check_pattern_len(pattern: &str, limits: &BrowserLimits) -> Result<()> {
    if pattern.len() > limits.max_text_bytes() {
        return Err(Error::InvalidArgument {
            detail: format!(
                "wait pattern is {} bytes, maximum is {}",
                pattern.len(),
                limits.max_text_bytes()
            ),
        });
    }
    Ok(())
}

/// Maximum time to block on a [`WaitCondition`], in milliseconds.
///
/// # Description
/// Never exceeds [`HARD_MAX_WAIT_MS`]: [`WaitTimeout::from_millis`] clamps,
/// [`WaitTimeout::try_from_millis`] and deserialization reject. A zero timeout
/// is representable via `from_millis(0)` but fails [`WaitTimeout::validate`]
/// and deserialization. Serialized as a plain integer of milliseconds.
///
/// # Concurrency
/// Plain data, `Send + Sync`, `Copy`.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::policy::HARD_MAX_WAIT_MS;
/// use harw_browser::wait::WaitTimeout;
///
/// assert_eq!(WaitTimeout::from_millis(u64::MAX).millis(), HARD_MAX_WAIT_MS);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct WaitTimeout(u64);

impl WaitTimeout {
    /// Builds a timeout, clamping to at most [`HARD_MAX_WAIT_MS`].
    ///
    /// # Arguments
    /// - `millis` (`u64`): requested timeout in milliseconds.
    pub fn from_millis(millis: u64) -> Self {
        Self(millis.min(HARD_MAX_WAIT_MS))
    }

    /// Builds a timeout strictly within `1..=HARD_MAX_WAIT_MS`.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: `millis` is zero or above the hard ceiling.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::wait::WaitTimeout;
    ///
    /// assert!(WaitTimeout::try_from_millis(0).is_err());
    /// ```
    pub fn try_from_millis(millis: u64) -> Result<Self> {
        if millis == 0 || millis > HARD_MAX_WAIT_MS {
            return Err(Error::InvalidArgument {
                detail: format!("wait timeout {millis} ms outside 1..={HARD_MAX_WAIT_MS}"),
            });
        }
        Ok(Self(millis))
    }

    /// Returns the timeout in milliseconds.
    pub fn millis(&self) -> u64 {
        self.0
    }

    /// Returns the timeout as a [`std::time::Duration`].
    pub fn duration(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.0)
    }

    /// Checks the timeout against the session's wait ceiling.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`]: zero or above [`BrowserLimits::max_wait_ms`].
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::policy::BrowserLimits;
    /// use harw_browser::wait::WaitTimeout;
    ///
    /// let limits = BrowserLimits::default().with_max_wait_ms(1_000);
    /// assert!(WaitTimeout::from_millis(1_001).validate(&limits).is_err());
    /// ```
    pub fn validate(&self, limits: &BrowserLimits) -> Result<()> {
        if self.0 == 0 || self.0 > limits.max_wait_ms() {
            return Err(Error::InvalidArgument {
                detail: format!(
                    "wait timeout {} ms outside 1..={}",
                    self.0,
                    limits.max_wait_ms()
                ),
            });
        }
        Ok(())
    }
}

impl TryFrom<u64> for WaitTimeout {
    type Error = Error;

    fn try_from(millis: u64) -> Result<Self> {
        Self::try_from_millis(millis)
    }
}

impl From<WaitTimeout> for u64 {
    fn from(timeout: WaitTimeout) -> Self {
        timeout.0
    }
}

/// Result of waiting for a [`WaitCondition`].
///
/// # Concurrency
/// Plain data, `Send + Sync`.
///
/// # Examples
/// ```rust,no_run
/// use harw_browser::wait::{WaitCondition, WaitOutcome};
///
/// let outcome = WaitOutcome::new(true, 12, WaitCondition::NavigationComplete);
/// assert!(outcome.satisfied);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitOutcome {
    pub satisfied: bool,
    pub elapsed_ms: u64,
    pub condition: WaitCondition,
}

impl WaitOutcome {
    /// Builds an outcome from its parts.
    pub fn new(satisfied: bool, elapsed_ms: u64, condition: WaitCondition) -> Self {
        Self {
            satisfied,
            elapsed_ms,
            condition,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selector::Selector;
    use std::time::Duration;

    #[test]
    fn test_wait_timeout_from_millis_duration_matches() {
        assert_eq!(
            WaitTimeout::from_millis(500).duration(),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn test_wait_timeout_from_millis_clamps_to_hard_ceiling() {
        assert_eq!(WaitTimeout::from_millis(u64::MAX).millis(), HARD_MAX_WAIT_MS);
    }

    #[test]
    fn test_wait_timeout_try_from_millis_bounds() {
        assert!(WaitTimeout::try_from_millis(0).is_err());
        assert!(WaitTimeout::try_from_millis(HARD_MAX_WAIT_MS + 1).is_err());
        assert_eq!(
            WaitTimeout::try_from_millis(HARD_MAX_WAIT_MS).map(|t| t.millis()).ok(),
            Some(HARD_MAX_WAIT_MS)
        );
    }

    #[test]
    fn test_wait_timeout_validate_against_limits() {
        let limits = BrowserLimits::default().with_max_wait_ms(1_000);
        assert!(WaitTimeout::from_millis(1_000).validate(&limits).is_ok());
        assert!(WaitTimeout::from_millis(1_001).validate(&limits).is_err());
        assert!(WaitTimeout::from_millis(0).validate(&limits).is_err());
    }

    #[test]
    fn test_wait_timeout_serde_is_integer_millis_and_bounded() {
        let timeout = WaitTimeout::from_millis(2500);
        let json = serde_json::to_value(timeout).expect("timeout serializes");
        assert_eq!(json, serde_json::json!(2500));
        let decoded: WaitTimeout = serde_json::from_value(json).expect("timeout deserializes");
        assert_eq!(decoded, timeout);
        assert!(serde_json::from_value::<WaitTimeout>(serde_json::json!(0)).is_err());
        assert!(
            serde_json::from_value::<WaitTimeout>(serde_json::json!(HARD_MAX_WAIT_MS + 1)).is_err()
        );
        assert!(
            serde_json::from_value::<WaitTimeout>(serde_json::json!({"secs": 1, "nanos": 0}))
                .is_err()
        );
    }

    #[test]
    fn test_wait_condition_deserialize_rejects_custom_script() {
        for tag in ["CustomScript", "custom_script", "Script", "script", "EvaluateScript"] {
            let json = serde_json::json!({ tag: { "predicate": "fetch('https://evil')" } });
            assert!(
                serde_json::from_value::<WaitCondition>(json).is_err(),
                "variant {tag} must not be representable"
            );
        }
    }

    #[test]
    fn test_wait_condition_deserialize_rejects_unknown_fields() {
        let json = serde_json::json!({ "NetworkQuiescence": { "idle_ms": 5, "script": "1" } });
        assert!(serde_json::from_value::<WaitCondition>(json).is_err());
        let ok = serde_json::json!({ "NetworkQuiescence": { "idle_ms": 5 } });
        assert!(serde_json::from_value::<WaitCondition>(ok).is_ok());
    }

    #[test]
    fn test_wait_condition_validate_limits() {
        let limits = BrowserLimits::default()
            .with_max_text_bytes(4)
            .with_max_wait_ms(1_000);
        assert!(
            WaitCondition::UrlMatches("12345".to_owned())
                .validate(&limits)
                .is_err()
        );
        assert!(
            WaitCondition::RequestObserved {
                path_contains: "12345".to_owned()
            }
            .validate(&limits)
            .is_err()
        );
        assert!(
            WaitCondition::NetworkQuiescence { idle_ms: 1_001 }
                .validate(&limits)
                .is_err()
        );
        assert!(
            WaitCondition::ElementPresent(Target::new(Selector::Css(String::new())))
                .validate(&limits)
                .is_err()
        );
        assert!(
            WaitCondition::ScriptMessage {
                channel: "bad channel".to_owned()
            }
            .validate(&limits)
            .is_err()
        );
        assert!(
            WaitCondition::ScriptMessage {
                channel: "harw:reply".to_owned()
            }
            .validate(&limits)
            .is_ok()
        );
        assert!(WaitCondition::DownloadComplete.validate(&limits).is_ok());
    }

    #[test]
    fn test_wait_condition_variants_equality() {
        assert_eq!(
            WaitCondition::NavigationComplete,
            WaitCondition::NavigationComplete
        );
        assert_ne!(
            WaitCondition::LogMatches("error".to_owned()),
            WaitCondition::LogMatches("warning".to_owned())
        );
    }

    #[test]
    fn test_wait_outcome_field_access() {
        let outcome = WaitOutcome::new(true, 120, WaitCondition::DownloadComplete);
        assert!(outcome.satisfied);
        assert_eq!(outcome.elapsed_ms, 120);
        assert_eq!(outcome.condition, WaitCondition::DownloadComplete);
    }

    #[test]
    fn test_wait_outcome_serde_json_round_trip_with_element_target() {
        let outcome = WaitOutcome::new(
            false,
            4000,
            WaitCondition::ElementClickable(Target::new(Selector::TestId("submit".to_owned()))),
        );
        let json = serde_json::to_string(&outcome).expect("outcome serializes");
        let decoded: WaitOutcome = serde_json::from_str(&json).expect("outcome deserializes");
        assert_eq!(decoded, outcome);
    }
}
