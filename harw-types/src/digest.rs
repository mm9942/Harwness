//! Content-addressed digests for evidence and manifest files.
//!
//! # Responsibility
//!
//! This module owns [`ContentDigest`], the crate's single content-addressing
//! primitive. It does not know about evidence records, chunk stores, or index
//! manifests — those live in downstream crates (`harw-dod-signals`,
//! `harw-lens-types`, and others) that embed a `ContentDigest` as a field.
//! This module's only job is: given bytes, produce a stable 32-byte digest,
//! and move that digest safely across process, file, and wire boundaries.
//!
//! # Why a dedicated type instead of `[u8; 32]`
//!
//! A digest travels through evidence references, chunk indexes, and
//! manifests. As a bare `[u8; 32]` it would be indistinguishable from any
//! other 32-byte value (a key, a nonce, a hash of something else entirely).
//! [`ContentDigest`] gives the value a name and a single validated
//! construction path.
//!
//! # Hash function
//!
//! [`ContentDigest::of`] uses [`blake3`], a cryptographic hash function. The
//! digest is 32 bytes (256 bits) regardless of input length.
//!
//! # Wire format
//!
//! [`ContentDigest`] serializes and deserializes as a 64-character lowercase
//! hexadecimal string, never as a byte array. A `ContentDigest` is embedded in
//! `EvidenceRef` and in `IndexManifest`, both of which are file formats a
//! human must be able to read and diff; a JSON array of 32 numbers is not
//! that, a hex string is.
//!
//! # Concurrency
//!
//! [`ContentDigest`] is `Copy`, contains no interior mutability, and is
//! `Send + Sync`. Hashing via [`ContentDigest::of`] touches no shared state
//! and is safe to call concurrently from any number of threads.
//!
//! # Errors
//!
//! Parsing a digest from an untrusted string (`FromStr`, `Deserialize`) can
//! fail with [`InvalidDigest`] when the input is not exactly 64 hexadecimal
//! characters.
//!
//! # Examples
//!
//! ```rust,no_run
//! # use harw_types::ContentDigest;
//! let digest = ContentDigest::of(b"hello world");
//! let text = digest.to_string();
//! assert_eq!(text.len(), 64);
//!
//! let parsed: ContentDigest = text.parse().expect("round-trips");
//! assert_eq!(parsed, digest);
//! ```

use crate::error::InvalidDigest;
use serde::{de::Deserializer, ser::Serializer, Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A blake3 digest over some content.
///
/// # Description
///
/// [`ContentDigest`] wraps the raw 32-byte blake3 output. It carries no
/// information about what was hashed; callers key evidence, chunks, and
/// manifest entries by the digest value itself.
///
/// # Examples
///
/// ```rust,no_run
/// # use harw_types::ContentDigest;
/// let a = ContentDigest::of(b"same input");
/// let b = ContentDigest::of(b"same input");
/// assert_eq!(a, b);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    /// Computes the blake3 digest of `bytes`.
    ///
    /// # Description
    ///
    /// Hashes the entire input in one call. There is no incremental/streaming
    /// variant on this type; callers that need to hash data incrementally
    /// should drive a [`blake3::Hasher`] themselves and build a
    /// `ContentDigest` from its finished output bytes.
    ///
    /// # Arguments
    ///
    /// - `bytes` (`&[u8]`): the content to hash. Borrowed; not retained.
    ///
    /// # Returns
    ///
    /// A new `ContentDigest` deterministic in `bytes`: identical input always
    /// produces an identical digest.
    ///
    /// # Concurrency
    ///
    /// Pure function, safe to call from any thread.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use harw_types::ContentDigest;
    /// let digest = ContentDigest::of(b"payload");
    /// assert_eq!(digest, ContentDigest::of(b"payload"));
    /// ```
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Borrows the raw 32 digest bytes.
    ///
    /// # Description
    ///
    /// Returns the underlying blake3 output with no encoding applied. Use
    /// the [`Display`](fmt::Display) impl (or `.to_string()`) for the hex
    /// form used on the wire.
    ///
    /// # Returns
    ///
    /// `&[u8; 32]` — a borrowed view of the digest bytes, valid as long as
    /// `self` is live.
    ///
    /// # Concurrency
    ///
    /// Read-only; safe to call from any thread.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use harw_types::ContentDigest;
    /// let digest = ContentDigest::of(b"payload");
    /// assert_eq!(digest.as_bytes().len(), 32);
    /// ```
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Converts one ASCII hex digit (`0`-`9`, `a`-`f`, `A`-`F`) to its 4-bit
/// value.
///
/// Private helper for [`FromStr for ContentDigest`](FromStr). Returns `None`
/// for any byte that is not an ASCII hex digit.
fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Formats the digest as 64 lowercase hexadecimal characters.
///
/// # Concurrency
///
/// Read-only; safe to call from any thread.
///
/// # Examples
///
/// ```rust,no_run
/// # use harw_types::ContentDigest;
/// let digest = ContentDigest::of(b"payload");
/// assert_eq!(digest.to_string().len(), 64);
/// ```
impl fmt::Display for ContentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Parses a digest from exactly 64 hexadecimal characters.
///
/// # Description
///
/// Accepts both lowercase and uppercase hex digits so digests copied from
/// other tools round-trip regardless of case; [`Display`](fmt::Display)
/// itself always emits lowercase. Any input that is not exactly 64 hex
/// digits (too short, too long, or containing a non-hex character) is
/// rejected.
///
/// # Errors
///
/// - [`InvalidDigest`]: `s` is not exactly 64 hexadecimal characters.
///
/// # Examples
///
/// ```rust,no_run
/// # use harw_types::ContentDigest;
/// # use std::str::FromStr;
/// let digest = ContentDigest::of(b"payload");
/// let text = digest.to_string();
/// assert_eq!(ContentDigest::from_str(&text).unwrap(), digest);
/// assert!("too-short".parse::<ContentDigest>().is_err());
/// ```
impl FromStr for ContentDigest {
    type Err = InvalidDigest;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let char_count = s.chars().count();
        if char_count != 64 {
            return Err(InvalidDigest::new(char_count));
        }

        let bytes = s.as_bytes();
        let mut out = [0u8; 32];
        for (i, slot) in out.iter_mut().enumerate() {
            let (Some(hi), Some(lo)) = (hex_nibble(bytes[2 * i]), hex_nibble(bytes[2 * i + 1]))
            else {
                return Err(InvalidDigest::new(char_count));
            };
            *slot = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

/// Serializes the digest as a 64-character lowercase hex string.
///
/// # Description
///
/// Never serializes as a byte array. `ContentDigest` values are embedded in
/// `EvidenceRef` and `IndexManifest`, both human-readable file formats; a hex
/// string stays diffable and greppable where a JSON array of 32 numbers
/// would not.
///
/// # Errors
///
/// Returns the serializer's error type if the underlying `serialize_str`
/// call fails; this type itself introduces no additional failure mode.
///
/// # Concurrency
///
/// Read-only; safe to call from any thread.
///
/// # Examples
///
/// ```rust,no_run
/// # use harw_types::ContentDigest;
/// let digest = ContentDigest::of(b"payload");
/// let json = serde_json::to_string(&digest).unwrap();
/// assert!(json.starts_with('"'));
/// ```
impl Serialize for ContentDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Deserializes the digest from a 64-character hex string.
///
/// # Description
///
/// Delegates to [`FromStr for ContentDigest`](FromStr), so the same
/// validation applies: the input must be exactly 64 hexadecimal characters.
///
/// # Errors
///
/// Returns the deserializer's error type, built from [`InvalidDigest`] via
/// `serde::de::Error::custom`, when the input is not a valid digest string.
///
/// # Concurrency
///
/// Safe to call from any thread.
///
/// # Examples
///
/// ```rust,no_run
/// # use harw_types::ContentDigest;
/// let digest = ContentDigest::of(b"payload");
/// let json = serde_json::to_string(&digest).unwrap();
/// let round_tripped: ContentDigest = serde_json::from_str(&json).unwrap();
/// assert_eq!(round_tripped, digest);
/// ```
impl<'de> Deserialize<'de> for ContentDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<Self>().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::ContentDigest;
    use std::str::FromStr;

    #[test]
    fn test_of_is_deterministic() {
        let a = ContentDigest::of(b"same input");
        let b = ContentDigest::of(b"same input");
        assert_eq!(a, b);
    }

    #[test]
    fn test_of_differs_for_different_input() {
        let a = ContentDigest::of(b"input one");
        let b = ContentDigest::of(b"input two");
        assert_ne!(a, b);
    }

    #[test]
    fn test_display_from_str_roundtrip() {
        let digest = ContentDigest::of(b"round trip me");
        let text = digest.to_string();
        assert_eq!(text.len(), 64);
        let parsed = ContentDigest::from_str(&text).expect("valid hex round-trips");
        assert_eq!(parsed, digest);
    }

    #[test]
    fn test_from_str_rejects_63_hex_characters() {
        let short = "a".repeat(63);
        assert!(ContentDigest::from_str(&short).is_err());
    }

    #[test]
    fn test_from_str_rejects_65_hex_characters() {
        let long = "a".repeat(65);
        assert!(ContentDigest::from_str(&long).is_err());
    }

    #[test]
    fn test_from_str_rejects_non_hex_characters() {
        let non_hex = "z".repeat(64);
        assert!(ContentDigest::from_str(&non_hex).is_err());
    }

    #[test]
    fn test_from_str_accepts_uppercase_hex() {
        let digest = ContentDigest::of(b"case insensitive");
        let upper = digest.to_string().to_uppercase();
        let parsed = ContentDigest::from_str(&upper).expect("uppercase hex is valid");
        assert_eq!(parsed, digest);
    }

    #[test]
    fn test_serde_roundtrip_uses_string_not_array() {
        let digest = ContentDigest::of(b"serde payload");
        let json = serde_json::to_string(&digest).expect("serializes");

        // Explicitly assert the JSON form is a string, not a byte array.
        assert!(json.starts_with('"') && json.ends_with('"'));
        assert!(!json.starts_with('['));

        let round_tripped: ContentDigest =
            serde_json::from_str(&json).expect("deserializes back");
        assert_eq!(round_tripped, digest);
    }

    #[test]
    fn test_deserialize_rejects_invalid_hex_string() {
        let json = "\"not-a-valid-digest\"";
        assert!(serde_json::from_str::<ContentDigest>(json).is_err());
    }
}
