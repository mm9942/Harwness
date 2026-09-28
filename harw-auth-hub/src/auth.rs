//! Caller authentication by kernel-reported peer credentials.
//!
//! The accept loop reads `SO_PEERCRED` of every accepted `AF_UNIX`
//! connection (`tokio::net::UnixStream::peer_cred`, no `unsafe`) and inserts
//! it as a [`PeerCred`] request extension on every request of that
//! connection (see [`crate::service::ConnectionService`]). A client cannot
//! forge an extension: extensions are process-local, never parsed from the
//! wire.
//!
//! [`PeerCredAuthenticator`] maps the uid to a CryptGuard principal:
//!
//! 1. uid listed in the config → that principal;
//! 2. otherwise, if bearer tokens are configured and the request carries an
//!    `Authorization` header → CryptGuard's constant-time [`BearerTokens`]
//!    decides (a missing or wrong token is `Unauthenticated`);
//! 3. otherwise → `Unauthenticated` (HTTP 401).
//!
//! There is no anonymous path: the authenticator never returns `Ok(None)`.

use std::collections::HashMap;

use crypt_guard_hyper::{Authenticator, BearerTokens};
use crypt_guard_service::{CryptoServiceError, Principal as CgPrincipal};
use http::request::Parts;

/// Kernel-reported credentials of the connected peer (`SO_PEERCRED`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCred {
    /// Effective uid of the peer at `connect(2)` time.
    pub uid: u32,
    /// Effective gid of the peer at `connect(2)` time.
    pub gid: u32,
    /// Pid of the peer, when the kernel reports one.
    pub pid: Option<i32>,
}

/// `SO_PEERCRED` uid → principal, with optional bearer-token fallback.
pub struct PeerCredAuthenticator {
    peers: HashMap<u32, CgPrincipal>,
    bearer: Option<BearerTokens>,
}

impl PeerCredAuthenticator {
    /// Authenticator over a uid → principal map and optional bearer tokens.
    #[must_use]
    pub fn new(peers: HashMap<u32, CgPrincipal>, bearer: Option<BearerTokens>) -> Self {
        Self { peers, bearer }
    }

    /// Whether a bearer-token fallback is configured.
    #[must_use]
    pub fn has_bearer_fallback(&self) -> bool {
        self.bearer.is_some()
    }
}

impl core::fmt::Debug for PeerCredAuthenticator {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PeerCredAuthenticator")
            .field("peers", &self.peers.len())
            .field("bearer", &self.bearer)
            .finish()
    }
}

impl Authenticator for PeerCredAuthenticator {
    fn authenticate(&self, parts: &Parts) -> Result<Option<CgPrincipal>, CryptoServiceError> {
        let peer = parts.extensions.get::<PeerCred>();
        if let Some(principal) = peer.and_then(|peer| self.peers.get(&peer.uid)) {
            return Ok(Some(principal.clone()));
        }
        let has_authorization = parts.headers.contains_key(http::header::AUTHORIZATION);
        match &self.bearer {
            Some(tokens) if has_authorization => match tokens.authenticate(parts)? {
                Some(principal) => Ok(Some(principal)),
                None => Err(CryptoServiceError::Unauthenticated),
            },
            _ => Err(CryptoServiceError::Unauthenticated),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crypt_guard_hyper::{Authenticator, BearerTokens};
    use crypt_guard_service::{CryptoServiceError, Principal as CgPrincipal, SecretBytes};
    use http::request::Parts;

    use super::{PeerCred, PeerCredAuthenticator};
    use crate::test_support::{TestResult, ctx};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn parts(peer: Option<PeerCred>, bearer: Option<&str>) -> TestResult<Parts> {
        let mut builder = http::Request::builder().uri("/v1/keys");
        if let Some(token) = bearer {
            builder = builder.header(http::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let mut request = builder.body(()).map_err(ctx("build request"))?;
        if let Some(peer) = peer {
            request.extensions_mut().insert(peer);
        }
        Ok(request.into_parts().0)
    }

    fn peer(uid: u32) -> PeerCred {
        PeerCred {
            uid,
            gid: uid,
            pid: Some(4242),
        }
    }

    fn authenticator(with_bearer: bool) -> PeerCredAuthenticator {
        let mut peers = HashMap::new();
        peers.insert(1000, CgPrincipal::new("harw-web"));
        let bearer = with_bearer.then(|| {
            let mut tokens = BearerTokens::new();
            tokens.insert(
                SecretBytes::copy_from_slice(TOKEN.as_bytes()),
                CgPrincipal::new("ops"),
            );
            tokens
        });
        PeerCredAuthenticator::new(peers, bearer)
    }

    #[test]
    fn known_uid_maps_to_principal() -> TestResult {
        let auth = authenticator(false);
        let result = auth.authenticate(&parts(Some(peer(1000)), None)?);
        assert_eq!(result, Ok(Some(CgPrincipal::new("harw-web"))));
        Ok(())
    }

    #[test]
    fn unknown_uid_is_unauthenticated() -> TestResult {
        let auth = authenticator(false);
        let result = auth.authenticate(&parts(Some(peer(1001)), None)?);
        assert_eq!(result, Err(CryptoServiceError::Unauthenticated));
        Ok(())
    }

    #[test]
    fn missing_peer_cred_is_unauthenticated() -> TestResult {
        let auth = authenticator(false);
        let result = auth.authenticate(&parts(None, None)?);
        assert_eq!(result, Err(CryptoServiceError::Unauthenticated));
        Ok(())
    }

    #[test]
    fn bearer_ignored_without_fallback() -> TestResult {
        let auth = authenticator(false);
        let result = auth.authenticate(&parts(Some(peer(1001)), Some(TOKEN))?);
        assert_eq!(result, Err(CryptoServiceError::Unauthenticated));
        Ok(())
    }

    #[test]
    fn bearer_fallback_for_unknown_uid() -> TestResult {
        let auth = authenticator(true);
        let result = auth.authenticate(&parts(Some(peer(1001)), Some(TOKEN))?);
        assert_eq!(result, Ok(Some(CgPrincipal::new("ops"))));
        let wrong = auth.authenticate(&parts(Some(peer(1001)), Some("wrong-token-wrong-token"))?);
        assert_eq!(wrong, Err(CryptoServiceError::Unauthenticated));
        let none = auth.authenticate(&parts(Some(peer(1001)), None)?);
        assert_eq!(none, Err(CryptoServiceError::Unauthenticated));
        Ok(())
    }

    #[test]
    fn debug_is_redacted() {
        let rendered = format!("{:?}", authenticator(true));
        assert!(!rendered.contains(TOKEN));
    }
}
