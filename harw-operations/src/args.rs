//! Reusable parsing helpers for [`crate::FromRawArgs`] implementations.
//!
//! Many operations parse a single trailing sub-command token or a joined tail;
//! this module centralizes those patterns so the `FromRawArgs` body of most ops
//! collapses to a one-liner like:
//!
//! ```rust
//! use harw_operations::args::first_optional;
//! use harw_operations::FromRawArgs;
//!
//! #[derive(Default)]
//! pub struct MyArgs { pub cmd: Option<String> }
//! impl FromRawArgs for MyArgs {
//!     fn from_raw_args(tokens: &[String]) -> Result<Self, harw_operations::OpError> {
//!         Ok(Self { cmd: first_optional(tokens) })
//!     }
//! }
//! ```
//!
//! # Responsibility scope
//! Owns all generic token-slicing utilities. Does not contain op-specific logic.
//!
//! # Key types / functions
//! - [`first_optional`] — first token or `None`
//! - [`require_first`] — first token or [`crate::OpError::InvalidArguments`]
//! - [`join_all_optional`] — space-joined tail or `None` for empty input
//! - [`join_from`] — space-joined suffix starting at index
//! - [`nth_optional`] — nth token or `None`
//! - [`require_empty`] — asserts no tokens; errors otherwise
//! - [`split_subcommand`] — splits `(head, rest)` for subcommand dispatch
//!
//! # Concurrency
//! All functions are pure and stateless; fully `Send + Sync`.
//!
//! # Error types
//! [`crate::OpError::InvalidArguments`] — only variant produced by this module.

use crate::error::OpError;

/// Returns the first token (if any) as an owned `Option<String>`.
///
/// # Description
/// Clones the first element of `tokens`. Returns `None` when the slice is empty.
/// This is the go-to helper for ops whose sole optional argument is a trailing token.
///
/// # Arguments
/// - `tokens` (`&[String]`): raw argument tokens from the command surface.
///
/// # Returns
/// `Some(first_token.clone())` or `None` when the slice is empty.
///
/// # Examples
/// ```rust
/// use harw_operations::args::first_optional;
/// let tokens = vec!["hello".to_string()];
/// assert_eq!(first_optional(&tokens), Some("hello".to_string()));
/// assert_eq!(first_optional(&[]), None);
/// ```
#[must_use]
pub fn first_optional(tokens: &[String]) -> Option<String> {
    tokens.first().cloned()
}

/// Requires a first token; returns [`OpError::InvalidArguments`] if absent.
///
/// # Description
/// Like [`first_optional`] but makes the token mandatory. Use for ops that must
/// have at least one argument (e.g. an ID, name, or keyword).
///
/// # Arguments
/// - `tokens` (`&[String]`): raw argument tokens.
/// - `what` (`&str`): English noun-phrase describing the required value;
///   used verbatim in the error message as `"{what} required"`.
///
/// # Returns
/// `Ok(first_token.clone())` when `tokens` is non-empty.
///
/// # Errors
/// [`OpError::InvalidArguments`] when `tokens` is empty.
///
/// # Examples
/// ```rust
/// use harw_operations::args::require_first;
/// use harw_operations::OpError;
/// let ok = require_first(&["id-123".to_string()], "session id").unwrap();
/// assert_eq!(ok, "id-123");
/// let err = require_first(&[], "session id").unwrap_err();
/// assert!(matches!(err, OpError::InvalidArguments(_)));
/// ```
pub fn require_first(tokens: &[String], what: &str) -> Result<String, OpError> {
    tokens
        .first()
        .cloned()
        .ok_or_else(|| OpError::InvalidArguments(format!("{what} required")))
}

/// Joins all tokens with a single space.
///
/// # Description
/// Returns `None` when the vector is empty (rather than `Some("")`) so callers
/// can pattern-match on absence cleanly. Useful for ops that accept free-form
/// text spread across multiple tokens.
///
/// # Arguments
/// - `tokens` (`&[String]`): raw argument tokens.
///
/// # Returns
/// `Some(tokens.join(" "))` or `None` when `tokens` is empty.
///
/// # Examples
/// ```rust
/// use harw_operations::args::join_all_optional;
/// assert_eq!(join_all_optional(&[]), None);
/// assert_eq!(
///     join_all_optional(&["hello".to_string(), "world".to_string()]),
///     Some("hello world".to_string()),
/// );
/// ```
#[must_use]
pub fn join_all_optional(tokens: &[String]) -> Option<String> {
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

/// Joins tokens starting from `start_idx` with a single space.
///
/// # Description
/// Returns `None` when `start_idx` is out of range (including when `tokens` is
/// shorter than `start_idx`). Handy for ops whose first token is a subcommand
/// and the remainder is free-form text: `join_from(tokens, 1)`.
///
/// # Arguments
/// - `tokens` (`&[String]`): raw argument tokens.
/// - `start_idx` (`usize`): zero-based index of the first token to include.
///
/// # Returns
/// `Some(tokens[start_idx..].join(" "))` or `None` when out of range.
///
/// # Examples
/// ```rust
/// use harw_operations::args::join_from;
/// let toks = vec!["set".to_string(), "key".to_string(), "value".to_string()];
/// assert_eq!(join_from(&toks, 1), Some("key value".to_string()));
/// assert_eq!(join_from(&toks, 3), None);
/// assert_eq!(join_from(&[], 0), None);
/// ```
#[must_use]
pub fn join_from(tokens: &[String], start_idx: usize) -> Option<String> {
    if start_idx >= tokens.len() {
        None
    } else {
        Some(tokens[start_idx..].join(" "))
    }
}

/// Returns the nth token as an owned `Option<String>`.
///
/// # Description
/// Zero-based index lookup with graceful out-of-bounds handling. Use when an op
/// expects positional arguments (e.g. `tokens[0]` = subcommand, `tokens[1]` = id).
///
/// # Arguments
/// - `tokens` (`&[String]`): raw argument tokens.
/// - `n` (`usize`): zero-based index.
///
/// # Returns
/// `Some(tokens[n].clone())` or `None` when out of range.
///
/// # Examples
/// ```rust
/// use harw_operations::args::nth_optional;
/// let toks = vec!["a".to_string(), "b".to_string()];
/// assert_eq!(nth_optional(&toks, 0), Some("a".to_string()));
/// assert_eq!(nth_optional(&toks, 1), Some("b".to_string()));
/// assert_eq!(nth_optional(&toks, 2), None);
/// ```
#[must_use]
pub fn nth_optional(tokens: &[String], n: usize) -> Option<String> {
    tokens.get(n).cloned()
}

/// Asserts that `tokens` is exactly empty; errors if any tokens are present.
///
/// # Description
/// Convenient for ops that take no arguments and want to reject stray input
/// rather than silently ignoring it.
///
/// # Arguments
/// - `tokens` (`&[String]`): raw argument tokens; must be empty.
///
/// # Returns
/// `Ok(())` when `tokens` is empty.
///
/// # Errors
/// [`OpError::InvalidArguments`] when `tokens` is non-empty, with a message
/// reporting how many unexpected arguments were received.
///
/// # Examples
/// ```rust
/// use harw_operations::args::require_empty;
/// use harw_operations::OpError;
/// assert!(require_empty(&[]).is_ok());
/// let err = require_empty(&["stray".to_string()]).unwrap_err();
/// assert!(matches!(err, OpError::InvalidArguments(_)));
/// ```
pub fn require_empty(tokens: &[String]) -> Result<(), OpError> {
    if tokens.is_empty() {
        Ok(())
    } else {
        Err(OpError::InvalidArguments(format!(
            "this command takes no arguments; got {}",
            tokens.len()
        )))
    }
}

/// Splits the first token into `(subcommand, rest)`.
///
/// # Description
/// Handy for ops with a dispatch shape like `switch <id>` / `list` / `show`.
/// The caller matches on the returned subcommand string and processes `rest`
/// with the appropriate sub-handler.
///
/// Returns `None` for empty input so the caller can emit an appropriate "usage"
/// error.
///
/// # Arguments
/// - `tokens` (`&[String]`): raw argument tokens.
///
/// # Returns
/// `Some((first.clone(), &tokens[1..]))` or `None` when `tokens` is empty.
///
/// # Examples
/// ```rust
/// use harw_operations::args::split_subcommand;
/// let toks = vec!["switch".to_string(), "abc".to_string()];
/// assert_eq!(split_subcommand(&toks), Some(("switch".to_string(), &toks[1..])));
/// assert_eq!(split_subcommand(&[]), None);
/// ```
#[must_use]
pub fn split_subcommand(tokens: &[String]) -> Option<(String, &[String])> {
    let (first, rest) = tokens.split_first()?;
    Some((first.clone(), rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // ── first_optional ────────────────────────────────────────────────────────

    #[test]
    fn test_first_optional_empty_returns_none() {
        assert_eq!(first_optional(&[]), None);
    }

    #[test]
    fn test_first_optional_returns_first_of_multi() {
        let tokens = strs(&["alpha", "beta", "gamma"]);
        assert_eq!(first_optional(&tokens), Some("alpha".to_string()));
    }

    // ── require_first ─────────────────────────────────────────────────────────

    #[test]
    fn test_require_first_error_on_empty() {
        let err = require_first(&[], "session id").unwrap_err();
        assert!(
            matches!(err, OpError::InvalidArguments(ref msg) if msg.contains("session id")),
            "expected message to contain 'session id', got: {err}"
        );
    }

    #[test]
    fn test_require_first_returns_first() {
        let tokens = strs(&["id-99", "extra"]);
        assert_eq!(require_first(&tokens, "id").unwrap(), "id-99");
    }

    // ── join_all_optional ─────────────────────────────────────────────────────

    #[test]
    fn test_join_all_optional_empty_returns_none() {
        assert_eq!(join_all_optional(&[]), None);
    }

    #[test]
    fn test_join_all_optional_joins_with_space() {
        let tokens = strs(&["hello", "world"]);
        assert_eq!(join_all_optional(&tokens), Some("hello world".to_string()));
    }

    // ── join_from ─────────────────────────────────────────────────────────────

    #[test]
    fn test_join_from_index_out_of_range_returns_none() {
        let tokens = strs(&["a", "b"]);
        assert_eq!(join_from(&tokens, 5), None);
        assert_eq!(join_from(&[], 0), None);
    }

    #[test]
    fn test_join_from_index_zero_equals_join_all() {
        let tokens = strs(&["x", "y", "z"]);
        assert_eq!(join_from(&tokens, 0), join_all_optional(&tokens));
    }

    #[test]
    fn test_join_from_mid_index_skips_prefix() {
        let tokens = strs(&["set", "key", "val"]);
        assert_eq!(join_from(&tokens, 1), Some("key val".to_string()));
    }

    // ── nth_optional ──────────────────────────────────────────────────────────

    #[test]
    fn test_nth_optional_returns_correct_index() {
        let tokens = strs(&["a", "b", "c"]);
        assert_eq!(nth_optional(&tokens, 0), Some("a".to_string()));
        assert_eq!(nth_optional(&tokens, 2), Some("c".to_string()));
    }

    #[test]
    fn test_nth_optional_out_of_range_returns_none() {
        let tokens = strs(&["only"]);
        assert_eq!(nth_optional(&tokens, 1), None);
        assert_eq!(nth_optional(&[], 0), None);
    }

    // ── require_empty ─────────────────────────────────────────────────────────

    #[test]
    fn test_require_empty_ok_on_empty() {
        assert!(require_empty(&[]).is_ok());
    }

    #[test]
    fn test_require_empty_err_on_nonempty() {
        let tokens = strs(&["stray"]);
        let err = require_empty(&tokens).unwrap_err();
        assert!(
            matches!(err, OpError::InvalidArguments(_)),
            "expected InvalidArguments, got: {err}"
        );
    }

    #[test]
    fn test_require_empty_err_message_contains_count() {
        let tokens = strs(&["a", "b", "c"]);
        let err = require_empty(&tokens).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains('3'),
            "error message should mention count 3, was: {msg}"
        );
    }

    // ── split_subcommand ──────────────────────────────────────────────────────

    #[test]
    fn test_split_subcommand_empty_returns_none() {
        assert_eq!(split_subcommand(&[]), None);
    }

    #[test]
    fn test_split_subcommand_two_tokens_splits_head_and_tail() {
        let tokens = strs(&["switch", "abc"]);
        let (head, tail) = split_subcommand(&tokens).unwrap();
        assert_eq!(head, "switch");
        assert_eq!(tail, &tokens[1..]);
    }

    #[test]
    fn test_split_subcommand_single_token_tail_is_empty() {
        let tokens = strs(&["list"]);
        let (head, tail) = split_subcommand(&tokens).unwrap();
        assert_eq!(head, "list");
        assert!(tail.is_empty());
    }
}
