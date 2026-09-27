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
//! ```
//!
//! plus the common `/v1/health`, `/v1/version`, `/v1/capabilities`
//! ([`crate::info`]). The hub has no list route, so there is no `list`.

use core::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    pub async fn rotate(&self, key: &KeyRef) -> Result<CreatedKey, InfraClientError> {
        let path = format!("{}:rotate", key.path());
        let response = self
            .transport
            .call(Method::POST, &path, RequestBody::Empty, None)
            .await?;
        response.expect_content_type(FRAME_CONTENT_TYPE)?;
        codec::parse_key_created(&response.body)
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
