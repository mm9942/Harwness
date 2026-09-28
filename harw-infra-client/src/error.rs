//! Error types of the infrastructure clients.
//!
//! [`InfraClientError`] is what every network call returns. It is
//! deliberately payload-free (apart from static reasons and HTTP status
//! codes): no response body, no key material, no token and no backend detail
//! ever travels inside an error value.
//!
//! [`InfraConfigError`] covers building clients from configuration (socket
//! paths, the bearer-token file). It never touches the network.

use core::fmt;
use std::path::PathBuf;

use http::StatusCode;

/// Failure of one call to an infrastructure daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InfraClientError {
    /// The daemon is not reachable (socket missing, connection refused,
    /// connection dropped before a response) or answered `503`.
    Unavailable,
    /// The daemon rejected the caller's credentials (`401`).
    Unauthenticated,
    /// The caller is authenticated but not permitted (`403`). Note that the
    /// CryptGuard adapter hides `Forbidden` as `404` by default, so a denied
    /// key usually surfaces as [`Self::NotFound`].
    Forbidden,
    /// The addressed key or route does not exist (`404`).
    NotFound,
    /// The exchange violated the wire contract: malformed HTTP, wrong
    /// content type, malformed `CGK1` frame or JSON, response too large.
    Protocol(&'static str),
    /// The call did not complete within the configured timeout. For a
    /// mutation (generate, rotate) the outcome is **unknown**: the daemon may
    /// or may not have committed it (masterplan §9).
    Timeout,
    /// Any other non-success status reported by the daemon.
    Remote(RemoteErrorKind),
}

impl InfraClientError {
    /// `true` for transient conditions ([`Self::Unavailable`],
    /// [`Self::Timeout`], [`RemoteErrorKind::Overloaded`]).
    ///
    /// Retrying is only safe for idempotent calls (health, describe, public
    /// key, unwrap); never auto-retry a mutation on the strength of this.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::Timeout | Self::Remote(RemoteErrorKind::Overloaded)
        )
    }
}

impl fmt::Display for InfraClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("infrastructure daemon unavailable"),
            Self::Unauthenticated => f.write_str("infrastructure daemon rejected the credentials"),
            Self::Forbidden => f.write_str("infrastructure daemon denied the operation"),
            Self::NotFound => f.write_str("not found"),
            Self::Protocol(reason) => write!(f, "infrastructure protocol error: {reason}"),
            Self::Timeout => f.write_str("infrastructure call timed out"),
            Self::Remote(kind) => write!(f, "infrastructure daemon error: {kind}"),
        }
    }
}

impl std::error::Error for InfraClientError {}

/// Classification of a non-success status that has no dedicated
/// [`InfraClientError`] variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoteErrorKind {
    /// `400`: the daemon could not decode the request.
    BadRequest,
    /// `405`.
    MethodNotAllowed,
    /// `409`: state conflict (e.g. key already exists, key disabled).
    Conflict,
    /// `413`: request body above the daemon's limit.
    PayloadTooLarge,
    /// `422`: cryptographic authentication failed (tampered wrapped key,
    /// wrong `info`/`aad`). Deliberately indistinguishable causes.
    AuthenticationFailed,
    /// `429`: the daemon sheds load.
    Overloaded,
    /// `500`.
    Internal,
    /// `501`: operation not supported for this key / profile.
    Unsupported,
    /// Any other status code.
    Status(u16),
}

impl fmt::Display for RemoteErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadRequest => f.write_str("bad request"),
            Self::MethodNotAllowed => f.write_str("method not allowed"),
            Self::Conflict => f.write_str("conflict"),
            Self::PayloadTooLarge => f.write_str("payload too large"),
            Self::AuthenticationFailed => f.write_str("authentication failed"),
            Self::Overloaded => f.write_str("overloaded"),
            Self::Internal => f.write_str("internal error"),
            Self::Unsupported => f.write_str("unsupported"),
            Self::Status(code) => write!(f, "status {code}"),
        }
    }
}

/// Map a non-success HTTP status to the client error.
pub(crate) fn map_status(status: StatusCode) -> InfraClientError {
    match status.as_u16() {
        401 => InfraClientError::Unauthenticated,
        403 => InfraClientError::Forbidden,
        404 => InfraClientError::NotFound,
        503 => InfraClientError::Unavailable,
        400 => InfraClientError::Remote(RemoteErrorKind::BadRequest),
        405 => InfraClientError::Remote(RemoteErrorKind::MethodNotAllowed),
        409 => InfraClientError::Remote(RemoteErrorKind::Conflict),
        413 => InfraClientError::Remote(RemoteErrorKind::PayloadTooLarge),
        422 => InfraClientError::Remote(RemoteErrorKind::AuthenticationFailed),
        429 => InfraClientError::Remote(RemoteErrorKind::Overloaded),
        500 => InfraClientError::Remote(RemoteErrorKind::Internal),
        501 => InfraClientError::Remote(RemoteErrorKind::Unsupported),
        other => InfraClientError::Remote(RemoteErrorKind::Status(other)),
    }
}

/// Failure while building clients from configuration.
#[derive(Debug)]
#[non_exhaustive]
pub enum InfraConfigError {
    /// A socket path is not absolute. There is no fallback to a working or
    /// home directory (masterplan §39).
    RelativeSocketPath {
        /// The configuration field (`auth_socket`, …).
        field: &'static str,
    },
    /// `token_file` is set but `auth_socket` is not: the token would be
    /// silently unused, so the configuration is rejected.
    TokenWithoutAuthSocket,
    /// The token file could not be opened or read.
    TokenFileUnreadable {
        /// The configured token file.
        path: PathBuf,
    },
    /// The token file is not a regular file or is accessible to "other"
    /// (mode `o+rwx` bits set).
    TokenFileInsecure {
        /// The configured token file.
        path: PathBuf,
    },
    /// The token is longer than [`crate::MAX_TOKEN_LEN`] bytes.
    TokenTooLarge,
    /// The token is empty or contains bytes outside visible ASCII.
    InvalidToken,
}

impl fmt::Display for InfraConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativeSocketPath { field } => {
                write!(f, "infrastructure socket path `{field}` must be absolute")
            }
            Self::TokenWithoutAuthSocket => {
                f.write_str("`token_file` is configured but `auth_socket` is not")
            }
            Self::TokenFileUnreadable { path } => {
                write!(f, "token file '{}' could not be read", path.display())
            }
            Self::TokenFileInsecure { path } => write!(
                f,
                "token file '{}' is not a regular file or is accessible to other users",
                path.display()
            ),
            Self::TokenTooLarge => f.write_str("bearer token is too large"),
            Self::InvalidToken => f.write_str("bearer token is empty or not visible ASCII"),
        }
    }
}

impl std::error::Error for InfraConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_status_covers_the_dedicated_variants() {
        let table = [
            (401, InfraClientError::Unauthenticated),
            (403, InfraClientError::Forbidden),
            (404, InfraClientError::NotFound),
            (503, InfraClientError::Unavailable),
            (400, InfraClientError::Remote(RemoteErrorKind::BadRequest)),
            (
                405,
                InfraClientError::Remote(RemoteErrorKind::MethodNotAllowed),
            ),
            (409, InfraClientError::Remote(RemoteErrorKind::Conflict)),
            (
                413,
                InfraClientError::Remote(RemoteErrorKind::PayloadTooLarge),
            ),
            (
                422,
                InfraClientError::Remote(RemoteErrorKind::AuthenticationFailed),
            ),
            (429, InfraClientError::Remote(RemoteErrorKind::Overloaded)),
            (500, InfraClientError::Remote(RemoteErrorKind::Internal)),
            (501, InfraClientError::Remote(RemoteErrorKind::Unsupported)),
            (418, InfraClientError::Remote(RemoteErrorKind::Status(418))),
        ];
        for (code, expected) in table {
            let status = StatusCode::from_u16(code).ok();
            assert_eq!(status.map(map_status), Some(expected), "status {code}");
        }
    }

    #[test]
    fn test_transient_classification() {
        assert!(InfraClientError::Unavailable.is_transient());
        assert!(InfraClientError::Timeout.is_transient());
        assert!(InfraClientError::Remote(RemoteErrorKind::Overloaded).is_transient());
        assert!(!InfraClientError::NotFound.is_transient());
        assert!(!InfraClientError::Protocol("x").is_transient());
    }
}
