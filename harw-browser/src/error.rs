//! # error
//!
//! ## Responsibility
//! This module owns the **crate-wide error type** ([`Error`]) and its
//! `Result` alias ([`Result`]). Every fallible operation defined anywhere in
//! `harw-browser` — and every conforming backend implementation of
//! [`crate::host::BrowserHost`] / [`crate::host::BrowserRuntime`] — returns
//! this type. It does not own recoverable, non-fatal evidence (see
//! [`crate::diagnostic::BrowserDiagnostic`] for that); [`Error`] is reserved
//! for operations that cannot proceed.
//!
//! ## Key types exported
//! - [`Error`] — the crate-wide error enum; each variant carries all context
//!   needed to understand the failure without reading source code.
//! - [`Result`] — `std::result::Result<T, Error>`, the alias used by every
//!   fallible function in this crate.
//!
//! ## Concurrency
//! `Error` is plain data (`Send + Sync` follows automatically), so it can be
//! constructed, matched, and propagated across threads with no locking.
//!
//! ## Errors
//! - [`Error::SessionNotFound`] — the requested
//!   [`crate::ids::BrowserSessionId`] has no corresponding open session.
//! - [`Error::StaleRevision`] — an [`crate::action::ActionRequest`] carried an
//!   `expected_revision` that no longer matches the context's current
//!   revision.
//! - [`Error::SelectorNotFound`] — no element matched a
//!   [`crate::selector::Target`]'s candidates.
//! - [`Error::OriginNotAllowed`] — a navigation target's origin is rejected
//!   by [`crate::policy::OriginPolicy`].
//! - [`Error::Timeout`] — a [`crate::wait::WaitCondition`] was not satisfied
//!   within its [`crate::wait::WaitTimeout`].
//! - [`Error::CapabilityUnavailable`] — the operation requires a browser
//!   capability the backend reports as unavailable (see
//!   [`crate::capability::CapabilityStatus::Unavailable`]).
//! - [`Error::InvalidArgument`] — a caller-supplied argument fails validation
//!   before any browser interaction is attempted.
//! - [`Error::UrlParse`] — a `url::Url` failed to parse; wraps
//!   `url::ParseError` and is produced via `?` through the `From` impl.
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::error::{Error, Result};
//! use harw_browser::ids::BrowserSessionId;
//!
//! fn find_session(id: BrowserSessionId) -> Result<()> {
//!     Err(Error::SessionNotFound { session_id: id })
//! }
//! ```

use std::fmt;

use crate::ids::{BrowserObservationRevision, BrowserSessionId};

/// The crate-wide `Result` alias: `std::result::Result<T, Error>`.
///
/// Every fallible function in `harw-browser` and every conforming backend
/// implementation returns this alias so callers only ever need to match on
/// one error type.
pub type Result<T> = std::result::Result<T, Error>;

/// The crate-wide error type returned by every fallible operation defined in
/// `harw-browser`.
///
/// # Description
/// Each variant is a struct-like variant (or, for [`Error::UrlParse`], a
/// tuple variant wrapping a foreign error type) carrying all the context
/// needed to understand and report the failure without inspecting source
/// code. [`Error`] implements [`std::error::Error`] (with `source()` linking
/// to the wrapped cause for [`Error::UrlParse`]), [`std::fmt::Display`]
/// (human-readable, no internal jargon), and [`std::fmt::Debug`] (delegates
/// to `Display` so there is exactly one message format).
///
/// # Concurrency
/// Plain data, `Send + Sync`, no interior mutability.
pub enum Error {
    SessionNotFound {
        session_id: BrowserSessionId,
    },
    StaleRevision {
        expected: BrowserObservationRevision,
        current: BrowserObservationRevision,
    },
    SelectorNotFound {
        detail: String,
    },
    OriginNotAllowed {
        origin: String,
    },
    Timeout {
        detail: String,
    },
    CapabilityUnavailable {
        detail: String,
    },
    InvalidArgument {
        detail: String,
    },
    UrlParse(url::ParseError),
}

// ── Display ──────────────────────────────────────────────────────────────
/// Formats a human-readable, jargon-free description of the error, including
/// the context each variant carries (session id, revisions, detail strings,
/// etc.).
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionNotFound { session_id } => {
                write!(f, "browser session '{session_id}' was not found")
            }
            Self::StaleRevision { expected, current } => write!(
                f,
                "observation revision is stale: expected revision {}, but current revision is {}",
                expected.value(),
                current.value(),
            ),
            Self::SelectorNotFound { detail } => {
                write!(f, "no element matched the selector: {detail}")
            }
            Self::OriginNotAllowed { origin } => {
                write!(
                    f,
                    "origin '{origin}' is not permitted by the current policy"
                )
            }
            Self::Timeout { detail } => write!(f, "operation timed out: {detail}"),
            Self::CapabilityUnavailable { detail } => {
                write!(f, "required browser capability is unavailable: {detail}")
            }
            Self::InvalidArgument { detail } => write!(f, "invalid argument: {detail}"),
            Self::UrlParse(source) => write!(f, "failed to parse URL: {source}"),
        }
    }
}

// ── Debug (delegates to Display for clean output) ──────────────────────────
/// Delegates to [`fmt::Display`] so there is exactly one message format for
/// `{:?}` and `{}` output — no duplicated formatting logic to keep in sync.
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// ── std::error::Error ───────────────────────────────────────────────────────
impl std::error::Error for Error {
    /// Returns the wrapped cause for [`Error::UrlParse`], or `None` for every
    /// other variant, since only `UrlParse` wraps a foreign error.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UrlParse(source) => Some(source),
            _ => None,
        }
    }
}

// ── From impls ──────────────────────────────────────────────────────────────
/// Converts a `url::ParseError` into [`Error::UrlParse`] so `?` works
/// wherever a `url::Url` is parsed inside a function returning [`Result`].
impl From<url::ParseError> for Error {
    fn from(e: url::ParseError) -> Self {
        Self::UrlParse(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn sample_url_parse_error() -> TestResult<url::ParseError> {
        // "not a url" has no scheme separator, guaranteed to fail parsing.
        match "not a url".parse::<url::Url>() {
            Ok(_) => Err(TestError::Unexpected(
                "test fixture string must fail to parse as a URL".into(),
            )),
            Err(e) => Ok(e),
        }
    }

    #[test]
    fn test_display_session_not_found_contains_session_id() {
        let session_id = BrowserSessionId::new();
        let err = Error::SessionNotFound { session_id };
        let message = err.to_string();
        assert!(!message.is_empty());
        assert!(message.contains(&session_id.to_string()));
    }

    #[test]
    fn test_display_stale_revision_contains_expected_and_current() {
        let expected = BrowserObservationRevision::initial();
        let current = expected.next();
        let err = Error::StaleRevision { expected, current };
        let message = err.to_string();
        assert!(message.contains(&expected.value().to_string()));
        assert!(message.contains(&current.value().to_string()));
    }

    #[test]
    fn test_display_selector_not_found_contains_detail() {
        let err = Error::SelectorNotFound {
            detail: "css `.missing`".to_owned(),
        };
        let message = err.to_string();
        assert!(!message.is_empty());
        assert!(message.contains("css `.missing`"));
    }

    #[test]
    fn test_display_origin_not_allowed_contains_origin() {
        let err = Error::OriginNotAllowed {
            origin: "https://evil.example".to_owned(),
        };
        let message = err.to_string();
        assert!(!message.is_empty());
        assert!(message.contains("https://evil.example"));
    }

    #[test]
    fn test_display_timeout_contains_detail() {
        let err = Error::Timeout {
            detail: "waited 5000ms for navigation".to_owned(),
        };
        let message = err.to_string();
        assert!(!message.is_empty());
        assert!(message.contains("waited 5000ms for navigation"));
    }

    #[test]
    fn test_display_capability_unavailable_contains_detail() {
        let err = Error::CapabilityUnavailable {
            detail: "BiDi connection not established".to_owned(),
        };
        let message = err.to_string();
        assert!(!message.is_empty());
        assert!(message.contains("BiDi connection not established"));
    }

    #[test]
    fn test_display_invalid_argument_contains_detail() {
        let err = Error::InvalidArgument {
            detail: "viewport width must be positive".to_owned(),
        };
        let message = err.to_string();
        assert!(!message.is_empty());
        assert!(message.contains("viewport width must be positive"));
    }

    #[test]
    fn test_display_url_parse_is_non_empty() -> TestResult {
        let err = Error::UrlParse(sample_url_parse_error()?);
        let message = err.to_string();
        assert!(!message.is_empty());
        assert!(message.contains("failed to parse URL"));
        Ok(())
    }

    #[test]
    fn test_debug_delegates_to_display() -> TestResult {
        let errors: Vec<Error> = vec![
            Error::SessionNotFound {
                session_id: BrowserSessionId::new(),
            },
            Error::SelectorNotFound {
                detail: "detail".to_owned(),
            },
            Error::UrlParse(sample_url_parse_error()?),
        ];
        for err in errors {
            assert_eq!(format!("{err:?}"), format!("{err}"));
        }
        Ok(())
    }

    #[test]
    fn test_from_url_parse_error_round_trips() -> TestResult {
        let parse_err = sample_url_parse_error()?;
        let expected_message = parse_err.to_string();
        let err: Error = parse_err.into();
        match err {
            Error::UrlParse(inner) => assert_eq!(inner.to_string(), expected_message),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Error::UrlParse, got {other}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_source_returns_some_for_url_parse() -> TestResult {
        use std::error::Error as _;

        let err = Error::UrlParse(sample_url_parse_error()?);
        assert!(err.source().is_some());
        Ok(())
    }

    #[test]
    fn test_source_returns_none_for_non_wrapping_variants() {
        use std::error::Error as _;

        let errors: Vec<Error> = vec![
            Error::SessionNotFound {
                session_id: BrowserSessionId::new(),
            },
            Error::StaleRevision {
                expected: BrowserObservationRevision::initial(),
                current: BrowserObservationRevision::initial().next(),
            },
            Error::SelectorNotFound {
                detail: "detail".to_owned(),
            },
            Error::OriginNotAllowed {
                origin: "https://example.com".to_owned(),
            },
            Error::Timeout {
                detail: "detail".to_owned(),
            },
            Error::CapabilityUnavailable {
                detail: "detail".to_owned(),
            },
            Error::InvalidArgument {
                detail: "detail".to_owned(),
            },
        ];
        for err in errors {
            assert!(err.source().is_none());
        }
    }
}
