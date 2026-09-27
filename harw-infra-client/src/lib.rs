//! Typed local clients for the Harwness infrastructure daemons (layer I).
//!
//! | Client | Daemon | Default socket |
//! |---|---|---|
//! | [`AuthHubClient`] | Auth/Crypto Hub | [`DEFAULT_AUTH_SOCKET`] |
//! | [`NetworkControlClient`] (skeleton) | NetSec control API | [`DEFAULT_NETWORK_SOCKET`] |
//! | [`SecurityHubClient`] (skeleton) | SecurityHub | [`DEFAULT_SECURITY_SOCKET`] |
//!
//! [`AuthHubDekWrapper`] adapts [`AuthHubClient`] to the synchronous
//! `harw_secrets::DekWrapper` trait for V3 (KMS-wrapped) secret records.
//!
//! Masterplan v2 §11 / §24 / §34 H4: this crate is the **only** place that
//! opens these sockets. Runtime code receives the clients through
//! [`InfrastructureAvailability`] at the composition root and never dials a
//! socket itself.
//!
//! # Transport
//! AF_UNIX stream → HTTP/1 (hyper client connection) → one request per
//! connection, under one per-call timeout ([`ClientOptions`]). Key routes and
//! bodies follow the CryptGuard reference KMS surface (`crypt_guard_hyper`
//! route table and `CGK1` codec, content type
//! `application/vnd.cryptguard.kms.v1`); the common
//! health/version/capabilities documents are JSON ([`info`] module docs).
//!
//! # Clone boundary (§10)
//! Every client is `Clone` as a network handle only: an `Arc` around
//! immutable connection parameters. No pool, lock, cache or crypto state is
//! shared. Secret-bearing inputs are moved, never cloned.
//!
//! # Secrets
//! - Plaintext key material in and out is `zeroize::Zeroizing<Vec<u8>>`.
//! - The bearer token is zeroized, not `Clone`, and redacted in `Debug`.
//! - Errors carry no payload bytes.
//! - hyper's internal I/O buffers are outside this crate's control and are
//!   not zeroized (see the crate README, "Zeroization boundary").
//!
//! # Public API contains no CryptGuard types
//! [`KeyRef`], [`WrappedKey`], [`PublicKeyBytes`], [`KeyProfile`], … are
//! this crate's own. `crypt_guard_hyper` is used only for its wire codec.

#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![warn(missing_docs)]

mod auth;
mod codec;
mod config;
mod dek;
mod error;
pub mod info;
mod key;
mod network;
mod security;
mod token;
mod transport;

#[cfg(test)]
mod test_server;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use auth::{AuthHubClient, DEFAULT_AUTH_SOCKET};
pub use config::{InfraClientConfig, InfrastructureAvailability};
pub use dek::{AUTHHUB_DEK_PROFILE_ID, AUTHHUB_DEK_WRAP_INFO, AuthHubDekWrapper};
pub use error::{InfraClientError, InfraConfigError, RemoteErrorKind};
pub use info::{Capabilities, Health, HealthState, VersionInfo};
pub use key::{
    CreatedKey, KeyContext, KeyDescription, KeyProfile, KeyRef, KeyRefError, KeyState,
    MAX_KEY_NAME_LEN, PublicKeyBytes, WrappedKey,
};
pub use network::{DEFAULT_NETWORK_SOCKET, NetworkControlClient};
pub use security::{
    ContextVerification, DEFAULT_SECURITY_SOCKET, MAX_CONTEXT_ID_LEN, SecurityHubClient,
};
pub use token::{BearerToken, MAX_TOKEN_LEN};
pub use transport::{ClientOptions, DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_TIMEOUT};
