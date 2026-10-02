//! Image references and the alias catalog.
//!
//! The model never supplies an image reference. It supplies an alias such as
//! `rust`, and the trusted configuration maps the alias to a digest-pinned
//! reference. A tag is refused (fail closed): a tag can move, a digest cannot.

use std::collections::BTreeMap;

use crate::error::ContainerPolicyError;
use crate::validate::check_name;

/// Longest accepted repository name in bytes.
const MAX_NAME_BYTES: usize = 255;
/// Length of a hex SHA-256 digest.
const SHA256_HEX_LEN: usize = 64;

/// A digest-pinned image reference: `name@sha256:<64 lowercase hex>`.
///
/// The name cannot start with `-`, so the value can never be read as an
/// engine option, and it has no tag.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageRef {
    name: String,
    hex: String,
}

impl ImageRef {
    /// Parses `name@sha256:<hex>`.
    ///
    /// The name is `[a-z0-9._/-]` segments, optionally prefixed by a registry
    /// host with a numeric port (`localhost:5000/team/img`). A colon anywhere
    /// else means a tag and is refused.
    ///
    /// # Errors
    /// [`ContainerPolicyError::InvalidImage`] with the reason.
    pub fn parse(raw: &str) -> Result<Self, ContainerPolicyError> {
        let err = ContainerPolicyError::InvalidImage;
        let (name, digest) = raw.split_once('@').ok_or(err("digest is required"))?;
        if digest.contains('@') {
            return Err(err("more than one '@'"));
        }
        let hex = digest
            .strip_prefix("sha256:")
            .ok_or(err("only sha256 digests are accepted"))?;
        if hex.len() != SHA256_HEX_LEN
            || !hex.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        {
            return Err(err("digest must be 64 lowercase hex characters"));
        }
        Self::check_name(name)?;
        Ok(Self {
            name: name.to_owned(),
            hex: hex.to_owned(),
        })
    }

    fn check_name(name: &str) -> Result<(), ContainerPolicyError> {
        let err = ContainerPolicyError::InvalidImage;
        if name.is_empty() {
            return Err(err("empty name"));
        }
        if name.len() > MAX_NAME_BYTES {
            return Err(err("name too long"));
        }
        if !name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return Err(err("name must start with a lowercase letter or digit"));
        }
        if name.contains("//") || name.contains("..") || name.ends_with('/') {
            return Err(err("malformed path"));
        }
        let path = match name.split_once('/') {
            Some((host, rest)) if host.contains(':') => {
                let (host_name, port) = host.split_once(':').ok_or(err("malformed host"))?;
                if host_name.is_empty()
                    || port.is_empty()
                    || port.len() > 5
                    || !port.chars().all(|c| c.is_ascii_digit())
                {
                    return Err(err("a tag is not allowed, pin by digest"));
                }
                if !host_name.chars().all(Self::is_name_char) {
                    return Err(err("invalid character in registry host"));
                }
                rest
            }
            _ => name,
        };
        if path.contains(':') {
            return Err(err("a tag is not allowed, pin by digest"));
        }
        if !path.chars().all(|c| Self::is_name_char(c) || c == '/') {
            return Err(err("invalid character in name"));
        }
        Ok(())
    }

    fn is_name_char(c: char) -> bool {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')
    }

    /// Repository name without the digest.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The 64 lowercase hex characters of the digest.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// The value passed to the engine: `name@sha256:<hex>`.
    #[must_use]
    pub fn to_arg(&self) -> String {
        format!("{}@sha256:{}", self.name, self.hex)
    }
}

/// Maps short aliases to digest-pinned images. Built from trusted config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageCatalog {
    entries: BTreeMap<String, ImageRef>,
}

impl ImageCatalog {
    /// Builds a catalog from `(alias, reference)` pairs.
    ///
    /// # Errors
    /// An invalid alias, an invalid reference, or a duplicate alias.
    pub fn new<'a>(
        entries: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Self, ContainerPolicyError> {
        let mut map = BTreeMap::new();
        for (alias, reference) in entries {
            check_name(alias, 32, "image alias")
                .map_err(|_| ContainerPolicyError::InvalidAlias("expected [a-z0-9][a-z0-9._-]{0,31}"))?;
            let image = ImageRef::parse(reference)?;
            if map.insert(alias.to_owned(), image).is_some() {
                return Err(ContainerPolicyError::DuplicateAlias);
            }
        }
        Ok(Self { entries: map })
    }

    /// Resolves an alias.
    ///
    /// # Errors
    /// [`ContainerPolicyError::UnknownAlias`] (the alias is not echoed).
    pub fn resolve(&self, alias: &str) -> Result<&ImageRef, ContainerPolicyError> {
        self.entries.get(alias).ok_or(ContainerPolicyError::UnknownAlias)
    }

    /// Aliases in sorted order, for `container.engine` output.
    pub fn aliases(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Number of aliases.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no alias is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn digest() -> String {
        format!("sha256:{HEX}")
    }

    #[test]
    fn accepts_a_digest_pinned_reference() -> TestResult {
        let r = ImageRef::parse(&format!("docker.io/library/rust@{}", digest()))?;
        ensure(r.name() == "docker.io/library/rust", "name")?;
        ensure(r.hex() == HEX, "hex")?;
        ensure(r.to_arg() == format!("docker.io/library/rust@sha256:{HEX}"), "arg")
    }

    #[test]
    fn accepts_a_registry_with_a_port() -> TestResult {
        ensure(
            ImageRef::parse(&format!("localhost:5000/team/img@{}", digest())).is_ok(),
            "port",
        )
    }

    #[test]
    fn refuses_tags_and_missing_digests() -> TestResult {
        ensure(ImageRef::parse("rust:latest").is_err(), "tag only")?;
        ensure(ImageRef::parse("rust").is_err(), "bare")?;
        ensure(
            ImageRef::parse(&format!("rust:1.98@{}", digest())).is_err(),
            "tag plus digest",
        )?;
        ensure(
            ImageRef::parse(&format!("localhost:5000/img:tag@{}", digest())).is_err(),
            "tag after a registry port",
        )?;
        ensure(
            ImageRef::parse(&format!("localhost:x/img@{}", digest())).is_err(),
            "non numeric port",
        )
    }

    #[test]
    fn refuses_option_like_and_malformed_names() -> TestResult {
        for name in [
            "-rust", "--privileged", "Rust", "a b", "a//b", "a/../b", "a/", "/a", "a\nb", "",
        ] {
            ensure(
                ImageRef::parse(&format!("{name}@{}", digest())).is_err(),
                name,
            )?;
        }
        Ok(())
    }

    #[test]
    fn refuses_bad_digests() -> TestResult {
        for d in [
            "sha256:abc",
            &format!("sha512:{HEX}"),
            &format!("sha256:{}", HEX.to_uppercase()),
            &format!("sha256:{HEX}0"),
            "",
        ] {
            ensure(ImageRef::parse(&format!("rust@{d}")).is_err(), d)?;
        }
        ensure(
            ImageRef::parse(&format!("rust@{}@{}", digest(), digest())).is_err(),
            "two digests",
        )
    }

    #[test]
    fn catalog_resolves_aliases_only() -> TestResult {
        let cat = ImageCatalog::new([("rust", format!("rust@{}", digest()).as_str())])?;
        ensure(cat.resolve("rust").is_ok(), "known")?;
        ensure(cat.resolve("python") == Err(ContainerPolicyError::UnknownAlias), "unknown")?;
        ensure(cat.aliases().collect::<Vec<_>>() == ["rust"], "aliases")?;
        ensure(cat.len() == 1 && !cat.is_empty(), "len")
    }

    #[test]
    fn catalog_rejects_bad_entries() -> TestResult {
        let good = format!("rust@{}", digest());
        ensure(ImageCatalog::new([("Rust", good.as_str())]).is_err(), "alias case")?;
        ensure(ImageCatalog::new([("rust", "rust:latest")]).is_err(), "tag")?;
        ensure(
            ImageCatalog::new([("rust", good.as_str()), ("rust", good.as_str())])
                == Err(ContainerPolicyError::DuplicateAlias),
            "duplicate",
        )
    }
}
