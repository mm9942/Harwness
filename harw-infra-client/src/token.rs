//! The AuthHub bearer token.
//!
//! The token is a credential: it lives in zeroizing memory, is never printed
//! (`Debug` shows `[REDACTED]`, there is no `Display`), and is read from a
//! file that must not be accessible to "other". The per-request
//! `Authorization` header is a copy owned by `http`/`hyper`; it is marked
//! sensitive but, like every header value, it is not zeroized.

use core::fmt;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use http::HeaderValue;
use zeroize::Zeroizing;

use crate::error::{InfraClientError, InfraConfigError};

/// Maximum accepted token length in bytes (after trimming whitespace).
pub const MAX_TOKEN_LEN: usize = 4096;

const BEARER_PREFIX: &[u8] = b"Bearer ";

/// A bearer token for `Authorization: Bearer <token>`.
///
/// Not `Clone`: a client shares one token through its `Arc`-held transport.
pub struct BearerToken {
    value: Zeroizing<Vec<u8>>,
}

impl BearerToken {
    /// Take ownership of token bytes. Leading and trailing ASCII whitespace
    /// (e.g. the newline of a token file) is removed; the rest must be
    /// non-empty visible ASCII (`0x21..=0x7e`) and at most
    /// [`MAX_TOKEN_LEN`] bytes.
    pub fn from_secret(bytes: Zeroizing<Vec<u8>>) -> Result<Self, InfraConfigError> {
        let start = bytes
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(bytes.len());
        let end = bytes
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(start, |last| last + 1);
        let trimmed = bytes.get(start..end).unwrap_or_default();
        if trimmed.is_empty() {
            return Err(InfraConfigError::InvalidToken);
        }
        if trimmed.len() > MAX_TOKEN_LEN {
            return Err(InfraConfigError::TokenTooLarge);
        }
        if !trimmed.iter().all(|b| (0x21..=0x7e).contains(b)) {
            return Err(InfraConfigError::InvalidToken);
        }
        if trimmed.len() == bytes.len() {
            return Ok(Self { value: bytes });
        }
        let mut value = Zeroizing::new(Vec::with_capacity(trimmed.len()));
        value.extend_from_slice(trimmed);
        // `bytes` (with the untrimmed copy) is zeroized when it drops here.
        Ok(Self { value })
    }

    /// Read a token file.
    ///
    /// The file must be a regular file without any "other" permission bits
    /// (`mode & 0o007 == 0`); group access is allowed so a service group can
    /// share it. At most [`MAX_TOKEN_LEN`] + 1 bytes are read into a
    /// pre-sized zeroizing buffer (no reallocation, no intermediate copy).
    pub fn read_from_file(path: &Path) -> Result<Self, InfraConfigError> {
        let unreadable = || InfraConfigError::TokenFileUnreadable {
            path: path.to_path_buf(),
        };
        let mut file = File::open(path).map_err(|_| unreadable())?;
        let metadata = file.metadata().map_err(|_| unreadable())?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o007 != 0 {
            return Err(InfraConfigError::TokenFileInsecure {
                path: path.to_path_buf(),
            });
        }

        // Whitespace around the token is tolerated, so allow a little slack
        // beyond `MAX_TOKEN_LEN` before declaring the file too large.
        let limit = MAX_TOKEN_LEN + 64;
        let mut buf = Zeroizing::new(vec![0u8; limit + 1]);
        let mut filled = 0;
        while let Some(rest) = buf.get_mut(filled..) {
            if rest.is_empty() {
                break;
            }
            let read = file.read(rest).map_err(|_| unreadable())?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        if filled > limit {
            return Err(InfraConfigError::TokenTooLarge);
        }
        buf.truncate(filled);
        Self::from_secret(buf)
    }

    /// The `Authorization` header value, marked sensitive.
    pub(crate) fn header_value(&self) -> Result<HeaderValue, InfraClientError> {
        let mut header = Zeroizing::new(Vec::with_capacity(BEARER_PREFIX.len() + self.value.len()));
        header.extend_from_slice(BEARER_PREFIX);
        header.extend_from_slice(&self.value);
        let mut value = HeaderValue::from_bytes(&header)
            .map_err(|_| InfraClientError::Protocol("bearer token is not a valid header value"))?;
        value.set_sensitive(true);
        Ok(value)
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BearerToken([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn secret(bytes: &[u8]) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(bytes.to_vec())
    }

    fn write_token(
        dir: &Path,
        name: &str,
        contents: &[u8],
        mode: u32,
    ) -> TestResult<std::path::PathBuf> {
        let path = dir.join(name);
        let mut file = File::create(&path)?;
        file.write_all(contents)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        Ok(path)
    }

    #[test]
    fn test_token_is_trimmed_and_validated() -> TestResult {
        let token = BearerToken::from_secret(secret(b"  abc.DEF-123\n"))?;
        assert_eq!(token.value.as_slice(), b"abc.DEF-123");
        assert!(matches!(
            BearerToken::from_secret(secret(b" \n")),
            Err(InfraConfigError::InvalidToken)
        ));
        assert!(matches!(
            BearerToken::from_secret(secret(b"has space")),
            Err(InfraConfigError::InvalidToken)
        ));
        assert!(matches!(
            BearerToken::from_secret(secret(&[b'a'; MAX_TOKEN_LEN + 1])),
            Err(InfraConfigError::TokenTooLarge)
        ));
        Ok(())
    }

    #[test]
    fn test_token_debug_is_redacted() -> TestResult {
        let token = BearerToken::from_secret(secret(b"super-secret-token"))?;
        let debug = format!("{token:?}");
        assert!(!debug.contains("super-secret-token"));
        assert!(debug.contains("REDACTED"));
        Ok(())
    }

    #[test]
    fn test_header_value_is_bearer_and_sensitive() -> TestResult {
        let token = BearerToken::from_secret(secret(b"tok"))?;
        let value = token
            .header_value()
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert_eq!(value.as_bytes(), b"Bearer tok");
        assert!(value.is_sensitive());
        Ok(())
    }

    #[test]
    fn test_read_from_file_accepts_owner_and_group_only() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = write_token(dir.path(), "token", b"file-token\n", 0o640)?;
        let token = BearerToken::read_from_file(&path)?;
        assert_eq!(token.value.as_slice(), b"file-token");

        let world = write_token(dir.path(), "world", b"file-token\n", 0o644)?;
        assert!(matches!(
            BearerToken::read_from_file(&world),
            Err(InfraConfigError::TokenFileInsecure { .. })
        ));

        assert!(matches!(
            BearerToken::read_from_file(&dir.path().join("missing")),
            Err(InfraConfigError::TokenFileUnreadable { .. })
        ));

        assert!(matches!(
            BearerToken::read_from_file(dir.path()),
            Err(InfraConfigError::TokenFileInsecure { .. }
                | InfraConfigError::TokenFileUnreadable { .. })
        ));

        let large = write_token(dir.path(), "large", &[b'a'; MAX_TOKEN_LEN + 200], 0o600)?;
        assert!(matches!(
            BearerToken::read_from_file(&large),
            Err(InfraConfigError::TokenTooLarge)
        ));
        Ok(())
    }
}
