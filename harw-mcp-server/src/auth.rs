use std::fmt;

/// Result of authenticating one HTTP request. `principal_key` is opaque and
/// safe to retain in a transport session; raw bearer bytes never are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    pub principal_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpAuthError {
    MissingBearer,
    InvalidBearer,
}
impl fmt::Display for McpAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBearer => f.write_str("missing MCP bearer credential"),
            Self::InvalidBearer => f.write_str("invalid MCP bearer credential"),
        }
    }
}
impl std::error::Error for McpAuthError {}

pub trait McpAuthenticator: Send + Sync {
    fn authenticate(
        &self,
        authorization: Option<&str>,
    ) -> Result<AuthenticatedPrincipal, McpAuthError>;
}

pub struct StaticBearerAuthenticator {
    entries: Vec<(String, Vec<u8>)>,
}
impl StaticBearerAuthenticator {
    #[must_use]
    pub fn new(entries: Vec<(String, Vec<u8>)>) -> Self {
        Self { entries }
    }
}
impl McpAuthenticator for StaticBearerAuthenticator {
    fn authenticate(
        &self,
        authorization: Option<&str>,
    ) -> Result<AuthenticatedPrincipal, McpAuthError> {
        let bearer = authorization
            .and_then(parse_bearer_header)
            .ok_or(McpAuthError::MissingBearer)?;
        let mut matched = None;
        let mut duplicate = false;
        for (key, token) in &self.entries {
            if constant_time_equal(token, bearer.as_bytes()) {
                if matched.is_some() {
                    duplicate = true;
                } else {
                    matched = Some(key.clone());
                }
            }
        }
        matched
            .filter(|_| !duplicate)
            .map(|principal_key| AuthenticatedPrincipal { principal_key })
            .ok_or(McpAuthError::InvalidBearer)
    }
}

fn parse_bearer_header(authorization: &str) -> Option<&str> {
    let bytes = authorization.as_bytes();
    if bytes.len() <= b"Bearer ".len()
        || !bytes[..b"Bearer".len()].eq_ignore_ascii_case(b"Bearer")
        || bytes[b"Bearer".len()] != b' '
    {
        return None;
    }

    let bearer = &authorization[b"Bearer ".len()..];
    let (credential, padding) = bearer.split_once('=').unwrap_or((bearer, ""));
    if !credential.is_empty()
        && credential.bytes().all(is_token68_byte)
        && padding.bytes().all(|byte| byte == b'=')
    {
        Some(bearer)
    } else {
        None
    }
}

fn is_token68_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/')
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::{
        AuthenticatedPrincipal, McpAuthError, McpAuthenticator, StaticBearerAuthenticator,
    };

    fn authenticator(entries: &[(&str, &str)]) -> StaticBearerAuthenticator {
        StaticBearerAuthenticator::new(
            entries
                .iter()
                .map(|(key, token)| ((*key).to_owned(), token.as_bytes().to_vec()))
                .collect(),
        )
    }

    #[test]
    fn accepts_bearer_scheme_case_insensitively() {
        let auth = authenticator(&[("alice", "secret")]);

        assert_eq!(
            auth.authenticate(Some("bEaReR secret")),
            Ok(AuthenticatedPrincipal {
                principal_key: "alice".to_owned(),
            })
        );
    }

    #[test]
    fn accepts_token68_padding() {
        let auth = authenticator(&[("alice", "secret==")]);

        assert_eq!(
            auth.authenticate(Some("BEARER secret==")),
            Ok(AuthenticatedPrincipal {
                principal_key: "alice".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_malformed_bearer_headers() {
        let auth = authenticator(&[("alice", "secret")]);

        for header in [
            "Bearer",
            "Bearer ",
            "Bearer  secret",
            "Bearer secret ",
            "Bearer secret\textra",
            "Basic secret",
            " Bearer secret",
        ] {
            assert_eq!(
                auth.authenticate(Some(header)),
                Err(McpAuthError::MissingBearer)
            );
        }
    }

    #[test]
    fn rejects_duplicate_configured_credentials() {
        let auth = authenticator(&[("alice", "shared"), ("bob", "shared")]);

        assert_eq!(
            auth.authenticate(Some("Bearer shared")),
            Err(McpAuthError::InvalidBearer)
        );
    }

    #[test]
    fn still_accepts_unique_credentials_with_other_entries_configured() {
        let auth = authenticator(&[("alice", "first"), ("bob", "second")]);

        assert_eq!(
            auth.authenticate(Some("Bearer second")),
            Ok(AuthenticatedPrincipal {
                principal_key: "bob".to_owned(),
            })
        );
    }
}
