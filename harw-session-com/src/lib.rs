//! `harw-session-com`: the communication layer of the gateway.
//!
//! # Responsibility
//! One service that turns an accepted stream plus a **listener-built**
//! [`harw_session_host::ClientIdentity`] into a served `harw.session.v1`
//! WebSocket, over any ingress. It authenticates nothing itself: kernel peer
//! credentials (Unix socket) and the node transport's authenticated peer
//! (self-cloud) stay with their listeners. It decides nothing about
//! authority: caps live in the identity and are checked by the host.
//!
//! ```text
//!  listener (UDS peer-cred | node transport | duplex in tests)
//!        | accepted stream + ClientIdentity
//!        v
//!  ComServer::serve_io        connection permit, identity validity
//!        v
//!  hyper http1 (upgrades, header timeout, bounded headers; refusals close)
//!        v   TowerToHyperService
//!  tower: PeerLayer -> trace -> [added layers] -> UpgradeService
//!        v
//!  validate upgrade -> ComBinder::bind (HTTP status on refusal) -> 101
//!        v
//!  tokio-tungstenite session (harw-session-ws): session.*, tool.*, gateway.*
//! ```
//!
//! # What this adds over the pieces below it
//! - **Refusals are HTTP statuses.** The identity is bound before the 101, so
//!   a revoked, denied or draining caller gets 403/503, not a socket that
//!   opens and closes.
//! - **`tool.*` and `gateway.*` become reachable.** [`PortOffer::All`] serves
//!   all method tables; a listener only has to build the identity with the
//!   right caps ([`local::local_identity`] adds the tier's gateway caps).
//! - **Ingress policy is a Tower layer** ([`ComServer::layer`]): refuse agent
//!   callers, demand a device, rate-limit reconnects, without touching the
//!   upgrade or the host.
//! - **Two ingresses, one stack.** Local: [`local::serve_unix`] reads
//!   `SO_PEERCRED` and calls [`ComServer::serve_io`]. Self-cloud:
//!   [`ComServer::service_for`] plus a [`RemoteLayer`] maps the node
//!   transport's authenticated peer to an enrolled device ([`remote`]).
//! - **One bounded server for every ingress**: a connection limit that covers
//!   the whole WebSocket session, bounded headers, a header timeout, refusals
//!   that close the connection, graceful [`ComServer::drain`].
//!
//! # Pitfall found while building this
//! `hyper`'s `keep_alive(false)` looks like the way to make a one-purpose
//! endpoint, but in hyper 1.11 it rewrites the 101 response's
//! `Connection: Upgrade` to `Connection: close`, so strict WebSocket clients
//! refuse the handshake. Refusals close through an explicit header instead.
//!
//! # Dependencies, on purpose
//! `hyper`, `hyper-util`, `tokio`, `tokio-tungstenite` (through
//! `harw-session-ws`) and `tower` are used. `hyper-tungstenite` is not: the
//! upgrade in `harw_session_ws::upgrade` already does the handshake and is
//! stricter (exact subprotocol, `Origin` refused, validated key and path).
//!
//! # Not covered here
//! Binding sockets and stale-socket handling (see `harw-session-daemon`),
//! the node transport itself (its `serve` takes [`ComServer::service_for`]),
//! and policy beyond what the host enforces.
//!
//! # Concurrency
//! [`ComServer`] is `Send + Sync`; each connection runs on its own task.

#![forbid(unsafe_code)]

pub mod binder;
pub mod config;
pub mod error;
pub mod layer;
pub mod refusal;
pub mod remote;
pub mod server;
pub mod service;

#[cfg(unix)]
pub mod local;

#[cfg(test)]
mod test_support;

pub use binder::{ComBinder, HostBinder, PortOffer};
pub use config::ComConfig;
pub use error::ComError;
pub use layer::{PeerLayer, PeerService, TrustedPeer};
pub use refusal::ComRefusal;
pub use remote::{DeviceRecord, DeviceRegistry, EnrollError, RemoteLayer, RemoteService};
pub use server::{BoxedService, ComServer, DEFAULT_GRACE};
pub use service::UpgradeService;
