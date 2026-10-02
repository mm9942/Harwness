//! Shared character and path rules.
//!
//! The rules are deliberately stricter than the engines need. A value that
//! passes here cannot add an option to an engine flag (`,` splits `--mount`
//! fields, `:` splits `--volume`, `=` splits key/value), cannot start an
//! option (leading `-`), and cannot smuggle a control character.

use crate::error::ContainerPolicyError;

/// Longest accepted path in bytes.
pub(crate) const MAX_PATH_BYTES: usize = 4096;

/// `true` for a character that is allowed in an identifier-like name.
fn is_name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')
}

/// Checks a short identifier: `[a-z0-9][a-z0-9._-]*`, at most `max` bytes.
pub(crate) fn check_name(
    value: &str,
    max: usize,
    field: &'static str,
) -> Result<(), ContainerPolicyError> {
    let err = |reason| ContainerPolicyError::InvalidName { field, reason };
    if value.is_empty() {
        return Err(err("empty"));
    }
    if value.len() > max {
        return Err(err("too long"));
    }
    if !value
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(err("must start with a lowercase letter or digit"));
    }
    if !value.chars().all(is_name_char) {
        return Err(err("only lowercase letters, digits, '.', '_' and '-' are allowed"));
    }
    Ok(())
}

/// Checks a label value: `[A-Za-z0-9._:-]`, 1..=64 bytes.
pub(crate) fn check_label_value(
    value: &str,
    field: &'static str,
) -> Result<(), ContainerPolicyError> {
    let err = |reason| ContainerPolicyError::InvalidName { field, reason };
    if value.is_empty() {
        return Err(err("empty"));
    }
    if value.len() > 64 {
        return Err(err("too long"));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    {
        return Err(err("only letters, digits, '.', '_', ':' and '-' are allowed"));
    }
    Ok(())
}

/// Checks an absolute, clean path that cannot alter an engine flag.
///
/// Rejects relative paths, `.`/`..`/empty components, a trailing `/` (except
/// the root), control characters, and the separators `,` `:` `=` `"` `'` `\`.
/// A symlink cannot be detected here; the caller canonicalizes first.
pub(crate) fn check_abs_path(value: &str) -> Result<(), ContainerPolicyError> {
    let err = ContainerPolicyError::InvalidPath;
    if value.is_empty() {
        return Err(err("empty"));
    }
    if value.len() > MAX_PATH_BYTES {
        return Err(err("too long"));
    }
    if !value.starts_with('/') {
        return Err(err("must be absolute"));
    }
    if value
        .chars()
        .any(|c| c.is_control() || matches!(c, ',' | ':' | '=' | '"' | '\'' | '\\'))
    {
        return Err(err("contains a control character or a separator"));
    }
    if value == "/" {
        return Ok(());
    }
    if value.ends_with('/') {
        return Err(err("trailing slash"));
    }
    for component in value[1..].split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(err("empty, '.' or '..' component"));
        }
    }
    Ok(())
}

/// Checks a relative path below a root: no leading `/`, no `.`/`..`/empty
/// component, and the same forbidden characters as [`check_abs_path`].
pub(crate) fn check_rel_path(value: &str) -> Result<(), ContainerPolicyError> {
    let err = ContainerPolicyError::InvalidWorkdir;
    if value.is_empty() {
        return Err(err("empty"));
    }
    if value.len() > MAX_PATH_BYTES {
        return Err(err("too long"));
    }
    if value.starts_with('/') {
        return Err(err("must be relative"));
    }
    if value
        .chars()
        .any(|c| c.is_control() || matches!(c, ',' | ':' | '=' | '"' | '\'' | '\\'))
    {
        return Err(err("contains a control character or a separator"));
    }
    for component in value.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(err("empty, '.' or '..' component"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn names_follow_the_identifier_rule() -> TestResult {
        ensure(check_name("harw-1.a_b", 32, "x").is_ok(), "valid")?;
        ensure(check_name("", 32, "x").is_err(), "empty")?;
        ensure(check_name("-a", 32, "x").is_err(), "leading dash")?;
        ensure(check_name("A", 32, "x").is_err(), "uppercase")?;
        ensure(check_name("a b", 32, "x").is_err(), "space")?;
        ensure(check_name("aaaa", 3, "x").is_err(), "too long")
    }

    #[test]
    fn abs_paths_cannot_alter_a_flag() -> TestResult {
        ensure(check_abs_path("/srv/ws").is_ok(), "plain")?;
        ensure(check_abs_path("/").is_ok(), "root")?;
        for bad in [
            "srv", "/a/", "/a//b", "/a/./b", "/a/../b", "/a,b", "/a:b", "/a=b", "/a\"b", "/a'b",
            "/a\\b", "/a\nb", "",
        ] {
            ensure(check_abs_path(bad).is_err(), bad)?;
        }
        Ok(())
    }

    #[test]
    fn rel_paths_stay_below_the_root() -> TestResult {
        ensure(check_rel_path("crates/x").is_ok(), "plain")?;
        for bad in ["", "/a", "a/..", "../a", "a//b", "a,b", "a:b"] {
            ensure(check_rel_path(bad).is_err(), bad)?;
        }
        Ok(())
    }

    #[test]
    fn label_values_are_restricted() -> TestResult {
        ensure(check_label_value("Run:1.a_b-c", "x").is_ok(), "valid")?;
        ensure(check_label_value("a b", "x").is_err(), "space")?;
        ensure(check_label_value("a=b", "x").is_err(), "equals")?;
        ensure(check_label_value(&"a".repeat(65), "x").is_err(), "long")
    }
}
