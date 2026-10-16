//! Digest-pinned image references.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Why an image reference was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestError {
    /// No `@sha256:` part (a plain tag, or nothing).
    NotPinned,
    /// The repository name is empty, too long or has forbidden characters.
    InvalidName,
    /// The digest is not 64 lowercase hex characters.
    InvalidDigest,
}

impl fmt::Display for DigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotPinned => "image must be pinned by digest (name@sha256:<64 hex>)",
            Self::InvalidName => "invalid image repository name",
            Self::InvalidDigest => "digest must be 64 lowercase hex characters",
        })
    }
}

impl std::error::Error for DigestError {}

/// `name@sha256:<64 hex>`. A tag is never accepted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ImageDigest {
    name: String,
    hex: String,
}

impl ImageDigest {
    /// Parses `name@sha256:<hex>`.
    ///
    /// # Errors
    /// [`DigestError`] for a tag, a bad name or a bad digest.
    pub fn parse(raw: &str) -> Result<Self, DigestError> {
        let (name, digest) = raw.split_once('@').ok_or(DigestError::NotPinned)?;
        let hex = digest
            .strip_prefix("sha256:")
            .ok_or(DigestError::NotPinned)?;
        if name.is_empty()
            || name.len() > 255
            || name.contains(':')
                && !name
                    .rsplit('/')
                    .next()
                    .is_some_and(|last| !last.contains(':'))
            || !name.bytes().all(|b| {
                b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'.' | b'_' | b'-' | b'/' | b':')
            })
            || name.split('/').any(|seg| seg.is_empty() || seg == ".")
            || name.starts_with(['/', '.', '-'])
            || name.contains("..")
        {
            return Err(DigestError::InvalidName);
        }
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(DigestError::InvalidDigest);
        }
        Ok(Self {
            name: name.to_owned(),
            hex: hex.to_owned(),
        })
    }

    /// Repository name (without digest).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The 64 hex digits.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// `name@sha256:<hex>`.
    #[must_use]
    pub fn reference(&self) -> String {
        format!("{}@sha256:{}", self.name, self.hex)
    }

    /// `sha256:<hex>` — the manifest digest as engines list it in `RepoDigests`
    /// (not the config digest an engine reports as the image id).
    #[must_use]
    pub fn digest(&self) -> String {
        format!("sha256:{}", self.hex)
    }
}

impl fmt::Display for ImageDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@sha256:{}", self.name, self.hex)
    }
}

impl TryFrom<String> for ImageDigest {
    type Error = DigestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ImageDigest> for String {
    fn from(value: ImageDigest) -> Self {
        value.reference()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_pinned_reference_round_trips_through_serde() {
        let raw = format!("docker.io/library/rust@sha256:{HEX}");
        let digest = ImageDigest::parse(&raw);
        assert_eq!(
            digest.as_ref().map(ImageDigest::reference).ok(),
            Some(raw.clone())
        );
        let json = serde_json::to_string(&digest.ok()).unwrap_or_default();
        assert_eq!(json, format!("\"{raw}\""));
        let back: Result<ImageDigest, _> = serde_json::from_str(&format!("\"{raw}\""));
        assert!(back.is_ok());
    }

    #[test]
    fn tags_and_malformed_digests_are_refused() {
        for raw in [
            "docker.io/library/rust:latest",
            "docker.io/library/rust",
            "rust@sha256:abc",
            "rust@sha512:aa",
            &format!("rust@sha256:{}", HEX.to_uppercase()),
            &format!("@sha256:{HEX}"),
            &format!("../x@sha256:{HEX}"),
            &format!("/abs@sha256:{HEX}"),
            &format!("a b@sha256:{HEX}"),
        ] {
            assert!(ImageDigest::parse(raw).is_err(), "{raw}");
        }
        assert!(serde_json::from_str::<ImageDigest>("\"rust:1.80\"").is_err());
    }

    #[test]
    fn registry_ports_are_not_tags() {
        let with_port = format!("registry.example:5000/team/app@sha256:{HEX}");
        assert!(ImageDigest::parse(&with_port).is_ok());
    }
}
