//! `harw-netsec`: the NetSec local network-control daemon.
//!
//! # Responsibility (crypto masterplan v2 §6.2, §18, §34 H7)
//! NetSec owns **topology and network control**: which nodes exist, where
//! they can be reached, which zone and route lead there, and whether a node
//! is being drained. Its local control API is `network.sock`
//! (`/run/harw/infra/network.sock`, drift report D1): Hyper HTTP/1 over
//! `AF_UNIX`/`SOCK_STREAM`, authorised by `SO_PEERCRED`.
//!
//! NetSec is **not** an identity authority. The Auth/Crypto Hub answers "is
//! this cryptographically node X?"; NetSec answers "where is node X, through
//! which zone/route, should it be drained?". A node record carries only a
//! *reference* to its identity key, and no address is ever evidence of
//! identity ("10.0.0.7 therefore trusted node harw-07" is exactly what this
//! crate must never say). `network.sock` is a local control API, not the
//! inter-node transport (§19, H11).
//!
//! # Modules
//! - [`state_machine`] — the explicit node lifecycle table.
//! - [`model`] / [`ids`] — node, zone and route records and identifiers.
//! - [`store`] — durable JSON store (temp + fsync + rename + dir fsync).
//! - [`config`] — `/etc/harw-netsec/config.toml`.
//! - [`api`] — endpoints and wire bodies.
//! - [`server`] — Hyper service, peer-credential check, socket binding.
//! - [`systemd`] — socket activation.
//! - [`daemon`] / [`cli`] — the binary's composition and command line.

pub mod api;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod error;
pub mod ids;
pub mod model;
pub mod server;
pub mod state_machine;
pub mod store;
pub mod systemd;

#[cfg(test)]
pub(crate) mod test_support;

pub use config::NetsecConfig;
pub use error::{NetsecError, NetsecResult};
pub use ids::{RouteId, ZoneId};
pub use model::{NodeAddress, NodeRecord, NodeRegistration, Route, TransitionRecord, Zone};
pub use server::{NetsecService, PeerIdentity, ServerSettings, bind_socket, serve};
pub use state_machine::{NodeEvent, NodeState, TRANSITIONS, next_state};
pub use store::{NetsecStore, NetworkState};
