//! [`AuthHubClient`]: typed client of the Auth/Crypto Hub (`secure.sock`).
//!
//! Key routes are the CryptGuard reference KMS table
//! (`crypt_guard_hyper::route`):
//!
//! ```text
//! POST /v1/keys                                  generate_key
//! GET  /v1/keys/{namespace}/{id}[@version]         describe
//! GET  /v1/keys/{namespace}/{id}[@version]/public  public_key
//! POST /v1/keys/{namespace}/{id}[@version]:rotate  rotate
//! POST /v1/keys/{namespace}/{id}[@version]:wrap    wrap_key
//! POST /v1/keys/{namespace}/{id}[@version]:unwrap  unwrap_key
//! POST /v1/keys/{namespace}/{id}[@version]:rewrap  rewrap_key
//! POST /v1/keys/{namespace}/{id}[@version]:sign    sign
//! ```
//!
//! plus the common `/v1/health`, `/v1/version`, `/v1/capabilities`
//! ([`crate::info`]). The hub has no list route, so there is no `list`.

use core::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crypt_guard_hyper::codec::FrameWriter;
use http::Method;
use zeroize::Zeroizing;

use crate::codec::{self, FRAME_CONTENT_TYPE};
use crate::error::InfraClientError;
use crate::info::{self, Capabilities, Health, VersionInfo};
use crate::key::{
    CreatedKey, KeyContext, KeyDescription, KeyProfile, KeyRef, PublicKeyBytes, WrappedKey,
};
use crate::token::BearerToken;
use crate::transport::{ClientOptions, RequestBody, UdsTransport};

/// Default AuthHub socket (drift report D1: `/run/harw/infra/`).
pub const DEFAULT_AUTH_SOCKET: &str = "/run/harw/infra/secure.sock";

const OCTET_STREAM: &str = "application/octet-stream";

/// Maximum length of a `sign` message this client will send.
///
/// Mirrors `harw_dod_encrypt::MAX_SIGNABLE_TRANSCRIPT_LEN` (60_896), but this
/// crate does not depend on `harw-dod-encrypt` (see `auth.rs` module scope
/// notes / task report), so the value is duplicated here as a local
/// constant. Keep it in sync if the upstream constant changes.
const MAX_SIGNABLE_MESSAGE_LEN: usize = 60_896;

/// Client of the Auth/Crypto Hub.
///
/// `Clone` is a cheap network-handle clone: every clone shares one
/// immutable, `Arc`-held set of connection parameters (socket path, token,
/// limits). There is no pool, cache, lock or other mutable state; each call
/// opens its own connection. Secret-bearing requests are moved in
/// (`wrap_key` takes the material by value), never cloned (masterplan §10,
/// §24).
#[derive(Clone)]
pub struct AuthHubClient {
    transport: Arc<UdsTransport>,
}

impl AuthHubClient {
    /// A client for the hub at `socket`. Nothing is connected until the
    /// first call.
    pub fn new(
        socket: impl Into<PathBuf>,
        token: Option<BearerToken>,
        options: ClientOptions,
    ) -> Self {
        Self {
            transport: Arc::new(UdsTransport::new(socket.into(), token, options)),
        }
    }

    /// The configured socket path.
    pub fn socket_path(&self) -> &Path {
        self.transport.socket()
    }

    /// The configured call limits.
    pub fn options(&self) -> ClientOptions {
        self.transport.options()
    }

    /// `GET /v1/health`.
    pub async fn health(&self) -> Result<Health, InfraClientError> {
        info::health(&self.transport).await
    }

    /// `GET /v1/version`.
    pub async fn version(&self) -> Result<VersionInfo, InfraClientError> {
        info::version(&self.transport).await
    }

    /// `GET /v1/capabilities` (descriptive, not authority).
    pub async fn capabilities(&self) -> Result<Capabilities, InfraClientError> {
        info::capabilities(&self.transport).await
    }

    /// Generate a new key `key.namespace()/key.id()` with `profile`.
    ///
    /// Only namespace and id are sent (validated by building the
    /// [`KeyRef`]); a pinned version on `key` is ignored, the hub assigns the
    /// first version and returns it in [`CreatedKey::key`].
    ///
    /// A mutation: on [`InfraClientError::Timeout`] the outcome is unknown;
    /// check with [`Self::describe`] before retrying.
    pub async fn generate_key(
        &self,
        key: &KeyRef,
        profile: KeyProfile,
    ) -> Result<CreatedKey, InfraClientError> {
        let body = codec::generate_body(key, profile)?;
        let response = self
            .transport
            .call(
                Method::POST,
                "/v1/keys",
                RequestBody::Public(body),
                Some(FRAME_CONTENT_TYPE),
            )
            .await?;
        response.expect_content_type(FRAME_CONTENT_TYPE)?;
        codec::parse_key_created(&response.body)
    }

    /// Public key of `key`.
    pub async fn public_key(&self, key: &KeyRef) -> Result<PublicKeyBytes, InfraClientError> {
        let path = format!("{}/public", key.path());
        let response = self
            .transport
            .call(Method::GET, &path, RequestBody::Empty, None)
            .await?;
        response.expect_content_type(OCTET_STREAM)?;
        Ok(PublicKeyBytes::new(response.into_public_body()))
    }

    /// Metadata of `key` (never key material).
    pub async fn describe(&self, key: &KeyRef) -> Result<KeyDescription, InfraClientError> {
        let path = key.path();
        let response = self
            .transport
            .call(Method::GET, &path, RequestBody::Empty, None)
            .await?;
        response.expect_content_type(FRAME_CONTENT_TYPE)?;
        codec::parse_metadata(&response.body)
    }

    /// Rotate `key`: create its next version. A mutation (see
    /// [`Self::generate_key`] on timeouts).
    ///
    /// Rotation is a compare-and-set on the hub: the request always carries
    /// the version the caller expects to be the current primary
    /// (`POST …/{id}@{expected}:rotate`), and the hub creates
    /// `expected + 1` only if `expected` is still the primary.
    ///
    /// - `key` pinned to a version: that version is the expectation. This is
    ///   the retry-safe form: a repeated call after a lost response cannot
    ///   create a second new version.
    /// - `key` unpinned: the current primary is read with [`Self::describe`]
    ///   first and used as the expectation. This only guards against a
    ///   concurrent rotation between the two calls; do not use it to retry.
    ///
    /// # Errors
    /// - [`InfraClientError::Remote`] with
    ///   [`RemoteErrorKind::Conflict`](crate::RemoteErrorKind::Conflict)
    ///   (`409`): `expected` is not the current primary. Either another
    ///   rotation committed first, or an earlier attempt of *this* rotation
    ///   already committed (response lost). The outcome needs a
    ///   [`Self::describe`]; never blindly retry.
    /// - [`InfraClientError::Timeout`]: outcome unknown, as for every
    ///   mutation; describe before retrying.
    /// - [`InfraClientError::Protocol`]: the hub described or created a key
    ///   other than the one asked for (wrong name, no version, or a created
    ///   version other than `expected + 1`).
    pub async fn rotate(&self, key: &KeyRef) -> Result<CreatedKey, InfraClientError> {
        let expected = match key.version() {
            Some(_) => key.clone(),
            None => {
                let current = self.describe(key).await?.key;
                let same_key = current.namespace() == key.namespace() && current.id() == key.id();
                if !same_key || current.version().is_none() {
                    return Err(InfraClientError::Protocol(
                        "describe returned no current version of the key to rotate",
                    ));
                }
                current
            }
        };
        let path = format!("{}:rotate", expected.path());
        let response = self
            .transport
            .call(Method::POST, &path, RequestBody::Empty, None)
            .await?;
        response.expect_content_type(FRAME_CONTENT_TYPE)?;
        let created = codec::parse_key_created(&response.body)?;
        let next = expected.version().and_then(|v| v.checked_add(1));
        let as_expected = created.key.namespace() == expected.namespace()
            && created.key.id() == expected.id()
            && created.key.version().is_some()
            && created.key.version() == next;
        if !as_expected {
            return Err(InfraClientError::Protocol(
                "rotate created a version other than expected + 1",
            ));
        }
        Ok(created)
    }

    /// Wrap (encrypt) key `material` under `key`, bound to `context`.
    ///
    /// `material` is moved in and zeroized once the request is encoded into
    /// its (zeroizing) request body.
    pub async fn wrap_key(
        &self,
        key: &KeyRef,
        context: &KeyContext,
        material: Zeroizing<Vec<u8>>,
    ) -> Result<WrappedKey, InfraClientError> {
        let body = codec::wrap_body(context, &material)?;
        drop(material);
        let path = format!("{}:wrap", key.path());
        let response = self
            .transport
            .call(
                Method::POST,
                &path,
                RequestBody::Secret(body),
                Some(FRAME_CONTENT_TYPE),
            )
            .await?;
        response.expect_content_type(OCTET_STREAM)?;
        Ok(WrappedKey::new(response.into_public_body()))
    }

    /// Unwrap (decrypt) `wrapped` under `key`, bound to `context`.
    ///
    /// The plaintext is returned in the zeroizing buffer it was received
    /// into (no further copy). A wrong key, context or a tampered blob is
    /// `Remote(AuthenticationFailed)`.
    pub async fn unwrap_key(
        &self,
        key: &KeyRef,
        context: &KeyContext,
        wrapped: &WrappedKey,
    ) -> Result<Zeroizing<Vec<u8>>, InfraClientError> {
        let body = codec::unwrap_body(context, wrapped)?;
        let path = format!("{}:unwrap", key.path());
        let response = self
            .transport
            .call(
                Method::POST,
                &path,
                RequestBody::Public(body),
                Some(FRAME_CONTENT_TYPE),
            )
            .await?;
        response.expect_content_type(OCTET_STREAM)?;
        Ok(response.body)
    }

    /// Sign `message` with `key`.
    ///
    /// Refused client-side, before any I/O: a `message` longer than
    /// [`MAX_SIGNABLE_MESSAGE_LEN`] is rejected with
    /// [`InfraClientError::Protocol`] without a round trip to the hub. This
    /// is only a client-side guard; the client never builds or checks
    /// transcripts itself — the hub's authorizer is what actually enforces
    /// the signing purpose.
    pub async fn sign(&self, key: &KeyRef, message: &[u8]) -> Result<Vec<u8>, InfraClientError> {
        if message.len() > MAX_SIGNABLE_MESSAGE_LEN {
            return Err(InfraClientError::Protocol(
                "sign message exceeds the maximum signable length",
            ));
        }
        let mut writer = FrameWriter::new();
        writer
            .field(message)
            .map_err(|_| InfraClientError::Protocol("malformed CGK1 frame"))?;
        let body = writer.finish();
        let path = format!("{}:sign", key.path());
        let response = self
            .transport
            .call(
                Method::POST,
                &path,
                RequestBody::Public(body),
                Some(FRAME_CONTENT_TYPE),
            )
            .await?;
        response.expect_content_type(OCTET_STREAM)?;
        Ok(response.into_public_body())
    }

    /// Re-wrap `wrapped` from `from` to `to` inside the hub, without the
    /// plaintext ever leaving it. `to` without a version means its latest.
    pub async fn rewrap_key(
        &self,
        from: &KeyRef,
        from_context: &KeyContext,
        to: &KeyRef,
        to_context: &KeyContext,
        wrapped: &WrappedKey,
    ) -> Result<WrappedKey, InfraClientError> {
        let body = codec::rewrap_body(from_context, to, to_context, wrapped)?;
        let path = format!("{}:rewrap", from.path());
        let response = self
            .transport
            .call(
                Method::POST,
                &path,
                RequestBody::Public(body),
                Some(FRAME_CONTENT_TYPE),
            )
            .await?;
        response.expect_content_type(OCTET_STREAM)?;
        Ok(WrappedKey::new(response.into_public_body()))
    }

    /// Whether two handles share the same connection parameters.
    #[cfg(test)]
    pub(crate) fn shares_transport_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.transport, &other.transport)
    }
}

impl fmt::Debug for AuthHubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthHubClient")
            .field("socket", &self.transport.socket())
            .field(
                "token",
                &if self.transport.has_token() {
                    "[REDACTED]"
                } else {
                    "none"
                },
            )
            .field("options", &self.transport.options())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::AuthHubClient;
    use crate::test_server::MockHub;
    use crate::test_support::{TestError, TestResult};
    use crate::{ClientOptions, InfraClientError, KeyRef, RemoteErrorKind};

    fn client(hub: &MockHub) -> AuthHubClient {
        AuthHubClient::new(&hub.socket, None, ClientOptions::default())
    }

    #[tokio::test]
    async fn test_rotate_with_the_current_version_creates_the_next() -> TestResult {
        let hub = MockHub::start().await?;
        // `rotated/*`: primary v2.
        let created = client(&hub)
            .rotate(&KeyRef::versioned("rotated", "kek", 2)?)
            .await?;
        assert_eq!(created.key, KeyRef::versioned("rotated", "kek", 3)?);
        Ok(())
    }

    #[tokio::test]
    async fn test_rotate_with_a_stale_version_is_a_conflict() -> TestResult {
        let hub = MockHub::start().await?;
        // Expecting v1 while v2 is primary: another rotation (or a lost
        // earlier attempt of this one) committed. The caller must describe.
        match client(&hub)
            .rotate(&KeyRef::versioned("rotated", "kek", 1)?)
            .await
        {
            Err(InfraClientError::Remote(RemoteErrorKind::Conflict)) => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "expected Remote(Conflict), got {other:?}"
            ))),
        }
    }

    #[tokio::test]
    async fn test_unpinned_rotate_sends_the_described_primary() -> TestResult {
        let hub = MockHub::start().await?;
        let auth = client(&hub);
        // The mock would answer `409` for `rotated/kek@1:rotate` and create
        // v2 for a bare `:rotate`; v3 proves `@2:rotate` was sent.
        let created = auth.rotate(&KeyRef::latest("rotated", "kek")?).await?;
        assert_eq!(created.key, KeyRef::versioned("rotated", "kek", 3)?);

        let created = auth.rotate(&KeyRef::latest("app", "k1")?).await?;
        assert_eq!(created.key, KeyRef::versioned("app", "k1", 2)?);
        Ok(())
    }

    #[tokio::test]
    async fn test_rotate_errors_pass_through() -> TestResult {
        let hub = MockHub::start().await?;
        let auth = client(&hub);
        match auth.rotate(&KeyRef::latest("status", "404")?).await {
            Err(InfraClientError::NotFound) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotFound, got {other:?}"
                )));
            }
        }
        match auth.rotate(&KeyRef::versioned("garbage", "k", 1)?).await {
            Err(InfraClientError::Protocol(_)) => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "expected Protocol, got {other:?}"
            ))),
        }
    }
}
