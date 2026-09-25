//! [`ArtifactDigest`]: the identity of one encoded artifact.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The artifact hash: BLAKE3 over every byte of an encoded artifact before
/// its 32-byte trailer, i.e. the trailer itself.
///
/// It is the identity of a build and the value an executable footer copies,
/// so the digest printed for an artifact file and for a built binary is the
/// same. Displays and serializes as 64 lowercase hex characters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactDigest([u8; 32]);

impl ArtifactDigest {
    /// Hashes `bytes` with BLAKE3.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Wraps raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw 32 digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex form (same as `Display`).
    #[must_use]
    pub fn to_hex(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for ArtifactDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ArtifactDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ArtifactDigest({self})")
    }
}

/// A string that is not 64 hex characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidArtifactDigest;

impl fmt::Display for InvalidArtifactDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("artifact digest must be exactly 64 hexadecimal characters")
    }
}

impl std::error::Error for InvalidArtifactDigest {}

impl FromStr for ArtifactDigest {
    type Err = InvalidArtifactDigest;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        if bytes.len() != 64 {
            return Err(InvalidArtifactDigest);
        }
        let mut out = [0u8; 32];
        for (slot, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
            let high = hex_value(pair[0]).ok_or(InvalidArtifactDigest)?;
            let low = hex_value(pair[1]).ok_or(InvalidArtifactDigest)?;
            *slot = (high << 4) | low;
        }
        Ok(Self(out))
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl Serialize for ArtifactDigest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ArtifactDigest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::ArtifactDigest;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_digest_hex_roundtrip() -> TestResult {
        let digest = ArtifactDigest::of(b"artifact");
        let hex = digest.to_hex();
        assert_eq!(hex.len(), 64);
        assert!(
            hex.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        let parsed: ArtifactDigest = hex.parse().map_err(ctx("hex parses"))?;
        assert_eq!(parsed, digest);
        Ok(())
    }

    #[test]
    fn test_digest_rejects_bad_hex() {
        assert!("zz".repeat(32).parse::<ArtifactDigest>().is_err());
        assert!("ab".repeat(31).parse::<ArtifactDigest>().is_err());
    }

    #[test]
    fn test_digest_serde_is_hex_string() -> TestResult {
        let digest = ArtifactDigest::of(b"x");
        let json = serde_json::to_string(&digest).map_err(ctx("serializes"))?;
        assert_eq!(json, format!("\"{digest}\""));
        let back: ArtifactDigest = serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(back, digest);
        Ok(())
    }
}
