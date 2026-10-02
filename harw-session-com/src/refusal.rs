//! Why a connection was refused, and the HTTP answer it gets.
//!
//! A refusal happens **before** the 101 response, so the client sees a status
//! code instead of a silently closed socket. Reasons are fixed strings; the
//! host's own error text stays in the logs.

use std::fmt;

use harw_session_host::HostError;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::{Response, StatusCode};

/// A refusal decided before the WebSocket upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComRefusal {
    /// The identity may not connect (capability, tenant or agent admission).
    Denied,
    /// The identity's authority was revoked.
    Revoked,
    /// The host is draining and accepts no new connection.
    Draining,
    /// The host failed; details are logged, not sent.
    Internal,
}

impl ComRefusal {
    /// HTTP status of the refusal.
    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self {
            Self::Denied | Self::Revoked => StatusCode::FORBIDDEN,
            Self::Draining => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Fixed, non-sensitive reason (also the response body).
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Denied => "connection refused",
            Self::Revoked => "authority revoked",
            Self::Draining => "host is draining, retry later",
            Self::Internal => "host error",
        }
    }

    /// Maps a host error. Detail is logged here and never returned.
    #[must_use]
    pub fn from_host(error: &HostError) -> Self {
        match error {
            HostError::Revoked => Self::Revoked,
            HostError::Denied(_) | HostError::NotFound | HostError::HelloRequired => Self::Denied,
            // Every other variant, including ones a later host adds, is a
            // host-side failure: detail is logged, the client sees `Internal`.
            #[allow(unreachable_patterns)]
            _ => {
                tracing::warn!(%error, "session host refused a connection");
                Self::Internal
            }
        }
    }

    /// The HTTP response for this refusal.
    #[must_use]
    pub fn response(self) -> Response<Full<Bytes>> {
        plain(self.status(), self.reason())
    }
}

impl fmt::Display for ComRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.reason(), self.status())
    }
}

impl std::error::Error for ComRefusal {}

/// A plain-text response.
pub(crate) fn plain(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_stable() {
        assert_eq!(ComRefusal::Denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(ComRefusal::Revoked.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            ComRefusal::Draining.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ComRefusal::Internal.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn host_errors_map_without_leaking_detail() {
        assert_eq!(
            ComRefusal::from_host(&HostError::Revoked),
            ComRefusal::Revoked
        );
        assert_eq!(
            ComRefusal::from_host(&HostError::Denied("secret tenant name".into())),
            ComRefusal::Denied
        );
        let internal = ComRefusal::from_host(&HostError::Storage("/var/lib/path".into()));
        assert_eq!(internal, ComRefusal::Internal);
        assert!(!internal.reason().contains("/var"));
    }
}
