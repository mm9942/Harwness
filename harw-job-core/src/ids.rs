//! Generic, validated identifiers of the job model (Job-Runtime-Doc §6).
//!
//! None of these names a Harwness concept: a [`JobScopeId`] is an opaque
//! authority scope, not a tenant or workspace (Eco-Doc §57). Adapters map
//! their own security context into it.
//!
//! String identifiers are validated on construction *and* on
//! deserialization (`serde(try_from = "String")`), so an invalid value can
//! never enter the model through a persisted record.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Maximum byte length of a structural identifier ([`AttemptId`],
/// [`RunnerId`], [`JobScopeId`]).
pub const MAX_ID_LEN: usize = 128;

/// Maximum byte length of an [`IdempotencyKey`].
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 255;

/// Why an identifier was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    /// The identifier was empty.
    Empty {
        /// Name of the identifier type.
        kind: &'static str,
    },
    /// The identifier exceeded its maximum byte length.
    TooLong {
        /// Name of the identifier type.
        kind: &'static str,
        /// Actual byte length.
        len: usize,
        /// Permitted maximum.
        max: usize,
    },
    /// The identifier contained a character outside its alphabet.
    InvalidChar {
        /// Name of the identifier type.
        kind: &'static str,
        /// The first offending character.
        ch: char,
    },
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { kind } => write!(f, "{kind} must not be empty"),
            Self::TooLong { kind, len, max } => {
                write!(f, "{kind} is {len} bytes long; at most {max} are allowed")
            }
            Self::InvalidChar { kind, ch } => {
                write!(f, "{kind} contains the invalid character {ch:?}")
            }
        }
    }
}

impl std::error::Error for IdError {}

/// Alphabet of structural identifiers: ASCII letters, digits, `-`, `_`, `.`,
/// `:`.
fn is_structural_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':')
}

/// Alphabet of idempotency keys: every visible ASCII character (no space, no
/// control characters).
fn is_visible_ascii(ch: char) -> bool {
    ch.is_ascii_graphic()
}

fn validate(
    kind: &'static str,
    value: &str,
    max: usize,
    allowed: fn(char) -> bool,
) -> Result<(), IdError> {
    if value.is_empty() {
        return Err(IdError::Empty { kind });
    }
    if value.len() > max {
        return Err(IdError::TooLong {
            kind,
            len: value.len(),
            max,
        });
    }
    match value.chars().find(|ch| !allowed(*ch)) {
        Some(ch) => Err(IdError::InvalidChar { kind, ch }),
        None => Ok(()),
    }
}

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident, $max:expr, $allowed:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validates and wraps `value`.
            ///
            /// # Errors
            /// [`IdError`] if the value is empty, too long, or contains a
            /// character outside the identifier's alphabet.
            pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                validate(stringify!($name), &value, $max, $allowed)?;
                Ok(Self(value))
            }

            /// The validated identifier text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

string_id!(
    /// Identity of one execution attempt of a job. A retried job gets a new
    /// attempt id; outcomes and leases are recorded per attempt.
    AttemptId,
    MAX_ID_LEN,
    is_structural_char
);

string_id!(
    /// Identity of the runner (worker process, host agent) that claims and
    /// executes attempts.
    RunnerId,
    MAX_ID_LEN,
    is_structural_char
);

string_id!(
    /// Generic authority scope of a job. Opaque to the core: an adapter maps
    /// its own security context (tenant/workspace, DoD context, ...) into it
    /// (Eco-Doc §57).
    JobScopeId,
    MAX_ID_LEN,
    is_structural_char
);

string_id!(
    /// Caller-chosen deduplication key: two submissions with the same key in
    /// the same scope denote the same job.
    IdempotencyKey,
    MAX_IDEMPOTENCY_KEY_LEN,
    is_visible_ascii
);

/// Monotonic fencing token issued by the durable store with every lease.
///
/// A write carrying a token older than the current one comes from a stale
/// (zombie) holder and must be rejected. Tokens only ever grow; there is no
/// way to decrement one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FencingToken(u64);

impl FencingToken {
    /// The token before any lease was issued.
    pub const INITIAL: Self = Self(0);

    /// Wraps a persisted token value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw token value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next token, or `None` if the token space is exhausted. Exhaustion
    /// fails closed instead of wrapping to an older value.
    #[must_use]
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    /// Whether a write fenced with `self` may proceed while `current` is the
    /// newest issued token: only the current holder may write.
    #[must_use]
    pub fn admits(self, current: Self) -> bool {
        self == current
    }

    /// Whether `self` was issued after `other`.
    #[must_use]
    pub fn supersedes(self, other: Self) -> bool {
        self > other
    }
}

impl fmt::Display for FencingToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttemptId, FencingToken, IdError, IdempotencyKey, JobScopeId, MAX_ID_LEN,
        MAX_IDEMPOTENCY_KEY_LEN, RunnerId,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn structural_ids_accept_their_alphabet() -> TestResult {
        let value = "attempt-01_a.b:c";
        assert_eq!(
            AttemptId::new(value).map_err(ctx("attempt"))?.as_str(),
            value
        );
        assert_eq!(
            RunnerId::new(value).map_err(ctx("runner"))?.to_string(),
            value
        );
        let scope = JobScopeId::new(value).map_err(ctx("scope"))?;
        assert_eq!(AsRef::<str>::as_ref(&scope), value);
        let parsed: RunnerId = value.parse().map_err(ctx("parse"))?;
        assert_eq!(String::from(parsed), value);
        Ok(())
    }

    #[test]
    fn structural_ids_reject_empty_too_long_and_foreign_chars() {
        assert_eq!(
            AttemptId::new(""),
            Err(IdError::Empty { kind: "AttemptId" })
        );
        let long = "a".repeat(MAX_ID_LEN + 1);
        assert_eq!(
            RunnerId::new(long),
            Err(IdError::TooLong {
                kind: "RunnerId",
                len: MAX_ID_LEN + 1,
                max: MAX_ID_LEN
            })
        );
        assert!(RunnerId::new("a".repeat(MAX_ID_LEN)).is_ok());
        for (bad, ch) in [
            ("a b", ' '),
            ("a/b", '/'),
            ("../x", '/'),
            ("a\0b", '\0'),
            ("ä", 'ä'),
            ("a\nb", '\n'),
        ] {
            assert_eq!(
                JobScopeId::new(bad),
                Err(IdError::InvalidChar {
                    kind: "JobScopeId",
                    ch
                })
            );
        }
    }

    #[test]
    fn idempotency_key_accepts_visible_ascii_only() -> TestResult {
        let key = IdempotencyKey::new("build/main#42?x=1").map_err(ctx("key"))?;
        assert_eq!(key.as_str(), "build/main#42?x=1");
        assert!(IdempotencyKey::new("a".repeat(MAX_IDEMPOTENCY_KEY_LEN)).is_ok());
        assert!(matches!(
            IdempotencyKey::new("a".repeat(MAX_IDEMPOTENCY_KEY_LEN + 1)),
            Err(IdError::TooLong { .. })
        ));
        assert!(matches!(
            IdempotencyKey::new("with space"),
            Err(IdError::InvalidChar { ch: ' ', .. })
        ));
        assert!(matches!(
            IdempotencyKey::new("tab\t"),
            Err(IdError::InvalidChar { ch: '\t', .. })
        ));
        assert!(matches!(
            IdempotencyKey::new(""),
            Err(IdError::Empty { .. })
        ));
        Ok(())
    }

    #[test]
    fn ids_round_trip_through_serde_and_reject_invalid_input() -> TestResult {
        let id = RunnerId::new("runner-1").map_err(ctx("runner"))?;
        let json = serde_json::to_string(&id).map_err(ctx("serialize"))?;
        assert_eq!(json, "\"runner-1\"");
        let back: RunnerId = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, id);

        assert!(serde_json::from_str::<RunnerId>("\"\"").is_err());
        assert!(serde_json::from_str::<AttemptId>("\"a b\"").is_err());
        assert!(serde_json::from_str::<IdempotencyKey>("\"a\\u0000\"").is_err());
        Ok(())
    }

    #[test]
    fn id_error_display_names_the_type() {
        assert_eq!(
            IdError::Empty { kind: "RunnerId" }.to_string(),
            "RunnerId must not be empty"
        );
        assert_eq!(
            IdError::TooLong {
                kind: "AttemptId",
                len: 3,
                max: 2
            }
            .to_string(),
            "AttemptId is 3 bytes long; at most 2 are allowed"
        );
        assert_eq!(
            IdError::InvalidChar {
                kind: "JobScopeId",
                ch: '/'
            }
            .to_string(),
            "JobScopeId contains the invalid character '/'"
        );
    }

    #[test]
    fn fencing_tokens_are_monotonic_and_fail_closed_on_exhaustion() -> TestResult {
        let first = FencingToken::INITIAL
            .next()
            .ok_or(TestError::Missing("next token"))?;
        assert_eq!(first.get(), 1);
        assert!(first.supersedes(FencingToken::INITIAL));
        assert!(!FencingToken::INITIAL.supersedes(first));
        assert!(!first.supersedes(first));

        let second = first.next().ok_or(TestError::Missing("second token"))?;
        assert!(second.admits(second));
        assert!(!first.admits(second), "a stale holder must be fenced off");

        assert_eq!(FencingToken::new(u64::MAX).next(), None);
        Ok(())
    }

    #[test]
    fn fencing_token_serializes_as_plain_number() -> TestResult {
        let token = FencingToken::new(7);
        let json = serde_json::to_string(&token).map_err(ctx("serialize"))?;
        assert_eq!(json, "7");
        let back: FencingToken = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, token);
        assert_eq!(token.to_string(), "7");
        Ok(())
    }
}
