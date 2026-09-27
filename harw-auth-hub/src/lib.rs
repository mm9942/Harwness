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
//! # Stack
//!
//! ```text
//! AF_UNIX accept ── SO_PEERCRED ──┐
//!                                 ▼
//! ConnectionService (per connection, carries PeerCred)
//!   ├─ GET /v1/health | /v1/version | /v1/capabilities
//!   │     → meta::respond_with(meta, store identity of this hub)
//!   └─ everything else, with PeerCred as request extension →
//!        TowerToHyperService<CryptoHttpService<CryptoWorkerHandle, PeerCredAuthenticator>>
//!          │   (bounded bodies; sign/verify limit = SIGN_BODY_LIMIT)
//!          └─ CryptoWorkerHandle ── bounded sync_channel ──▶ thread harw-kms-crypto-0
//!               └─ AuditProvider
//!                    └─ PolicyProvider<_, HarwUsageAuthorizer<NamespacePolicy>>
//!                         └─ KmsGuardProvider
//!                              └─ InMemoryProvider | SealedProvider   (KeyStore)
//! ```
//!
//! Only the crypto worker handle (a channel sender and an `Arc`) is cloned
//! per connection; all key state is owned by the single crypto worker
//! thread, which executes requests strictly one at a time, as
//! [`guard::KmsGuardProvider`] requires. No crypto runs on a Tokio executor
//! thread.
//!
//! # Modules
//!
//! - [`config`]: strict TOML config (peers by uid, grants, bearer tokens,
//!   `[key_store]`, `[crypto_worker]`).
//! - [`auth`]: [`PeerCred`] and [`PeerCredAuthenticator`].
//! - [`audit`]: [`AuditProvider`] and per-request HTTP audit events.
//! - [`guard`]: [`guard::KmsGuardProvider`], the version-lifecycle guard in
//!   front of the key store.
//! - [`crypto_worker`]: [`crypto_worker::CryptoWorkerHandle`], the dedicated
//!   worker thread that runs the provider stack off the Tokio executor.
//! - [`sealed`]: [`sealed::SealedProvider`], the persistent, sealed-file key
//!   store, and [`sealed::Kek`] / [`sealed::SealedStoreError`].
//! - [`meta`]: `/v1/health`, `/v1/version`, `/v1/capabilities`.
//! - [`service`]: [`HubService`] / [`ConnectionService`] assembly, and
//!   [`service::KeyStore`] (which key store a hub runs over).
//! - [`server`]: [`HubListener`] (path bind or systemd activation) and
//!   [`serve`] (accept loop, graceful shutdown).
//! - [`systemd`]: `LISTEN_PID`/`LISTEN_FDS` handling without `unsafe`.
//!
//! # Persistence
//!
//! The hub runs over one of two key stores, chosen by
//! [`config::KeyStoreConfig`] (`[key_store]`, default `memory`) and passed
//! to [`HubService::build`] as [`service::KeyStore`]:
//!
//! - **In-memory** (`KeyStoreConfig::InMemory`): CryptGuard's
//!   `InMemoryProvider`. **All keys are lost when the process exits or
//!   restarts.**
//! - **Sealed file** (`KeyStoreConfig::Sealed`): [`sealed::SealedProvider`],
//!   a from-scratch, on-disk-persistent reimplementation of the same
//!   `CryptoProvider` contract. Every key version's provenance seed is
//!   encrypted and authenticated in a single store file with a
//!   key-encryption key ([`sealed::Kek`]) that is never stored next to it.
//!   The KEK source ([`config::KekSource`]) is a `0600` key file or a
//!   systemd credential; see [`config::HubConfig::load_kek`].
//!
//! Each store has a stable **epoch**, persisted next to a sealed store as
//! `<path>.epoch` and freshly generated for an in-memory store on every
//! start; it is reported on `/v1/version` and `/v1/capabilities` so a client
//! can detect a hub that lost its keys (in-memory restart) or was pointed
//! at a different store file.
//!
//! ## Limits
//!
//! - **No rollback detection**: an attacker with write access to the store
//!   directory can replace the store file with an older, still-authentic
//!   version (for example re-enabling a destroyed key version). The store's
//!   internal generation counter is persisted for diagnostics only; there
//!   is no external monotonic counter to detect this.
//! - **Destroy cannot scrub old filesystem blocks**: destroying a key
//!   removes its seed from the current store file and from memory
//!   (zeroized), but earlier file generations may still survive in freed
//!   filesystem blocks. Guaranteed crypto-shredding requires rotating the
//!   KEK (reseal under a new KEK, then destroy the old one).
//!
//! # No signing oracle
//!
//! Every `harw.*` key is bound to one semantic purpose
//! (`harw_dod_encrypt::purpose`), and [`HarwUsageAuthorizer`] enforces that
//! binding on every operation before the version guard or key store see it:
//! a key may only sign transcripts of its own purpose, so a key never
//! becomes an arbitrary document-signing oracle. Sign requests are also
//! bounded by `MAX_SIGNABLE_TRANSCRIPT_LEN` and the crypto HTTP adapter's
//! sign/verify body limit.
//!
//! [`HarwUsageAuthorizer`]: harw_dod_encrypt::cg::HarwUsageAuthorizer
//!
//! # Wire format
//!
//! `/v1/keys…` requests and responses use CryptGuard's `CGK1` frames
//! (`crypt_guard_hyper::codec`); see the crate README for the route table.

#![forbid(unsafe_code)]

pub mod audit;
pub mod auth;
pub mod config;
pub mod crypto_worker;
pub mod error;
pub mod guard;
pub mod meta;
pub mod sealed;
pub mod server;
pub mod service;
pub mod systemd;

#[cfg(test)]
mod test_support;

pub use audit::{AUDIT_TARGET, AuditProvider};
pub use auth::{PeerCred, PeerCredAuthenticator};
pub use config::{DEFAULT_CONFIG_PATH, DEFAULT_SOCKET_PATH, HubConfig, KekSource, KeyStoreConfig};
pub use error::HubError;
pub use sealed::{Kek, SealedProvider, SealedStoreError};
pub use server::{HubListener, serve, shutdown_signal};
pub use service::{ConnectionService, HubService, KeyStore};
