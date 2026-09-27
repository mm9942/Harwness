//! `harw-auth-hub` — the local Auth/Crypto Hub (Masterplan v2 §6.3, H3).
//!
//! A small daemon that owns key material and serves the CryptGuard 3.1 KMS
//! surface over Hyper HTTP/1 on an `AF_UNIX` stream socket
//! (default [`config::DEFAULT_SOCKET_PATH`]). Callers are identified by the
//! kernel (`SO_PEERCRED`), mapped to CryptGuard principals by a strict TOML
//! config, and authorized per namespace by `NamespacePolicy`. Private key
//! material never leaves the process: the reference route table has no
//! export operation.
//!
//! # Modules
//!
//! - [`config`]: strict TOML config (peers by uid, grants, bearer tokens).
//! - [`auth`]: [`PeerCred`] and [`PeerCredAuthenticator`].
//! - [`audit`]: [`AuditProvider`] and per-request HTTP audit events.
//! - [`meta`]: `/v1/health`, `/v1/version`, `/v1/capabilities`.
//! - [`service`]: [`HubService`] / [`ConnectionService`] assembly.
//! - [`server`]: [`HubListener`] (path bind or systemd activation) and
//!   [`serve`] (accept loop, graceful shutdown).
//! - [`systemd`]: `LISTEN_PID`/`LISTEN_FDS` handling without `unsafe`.
//!
//! # Persistence
//!
//! Keys live only in CryptGuard's `InMemoryProvider`: **all keys are lost
//! when the process exits or restarts.** A sealed export/import of the key
//! store is future work (MIG-rest); until then, do not use the hub for keys
//! whose loss is not acceptable.
//!
//! # Wire format
//!
//! `/v1/keys…` requests and responses use CryptGuard's `CGK1` frames
//! (`crypt_guard_hyper::codec`); see the crate README for the route table.

#![forbid(unsafe_code)]

pub mod audit;
pub mod auth;
pub mod config;
pub mod error;
pub mod meta;
pub mod server;
pub mod service;
pub mod systemd;

#[cfg(test)]
mod test_support;

pub use audit::{AUDIT_TARGET, AuditProvider};
pub use auth::{PeerCred, PeerCredAuthenticator};
pub use config::{DEFAULT_CONFIG_PATH, DEFAULT_SOCKET_PATH, HubConfig};
pub use error::HubError;
pub use server::{HubListener, serve, shutdown_signal};
pub use service::{ConnectionService, HubService};
