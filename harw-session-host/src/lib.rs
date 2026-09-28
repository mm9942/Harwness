//! `harw-session-host`: the persistent session control plane host (W00 §2.4,
//! PL-65 §2).
//!
//! The host is the **single writer** of hosted sessions. Clients (local TUI,
//! phone, laptop) attach through a transport and send intentions; the host
//! admits every call against the caller's tenant and capabilities, runs at
//! most one turn per session, and fans frames out to every attachment
//! without ever blocking the turn loop.
//!
//! Transport independent: [`SessionHost::connect`] returns a
//! [`harw_protocol::SessionPort`] bound to one authenticated
//! [`ClientIdentity`]. The WebSocket layer (`harw-session-ws`) drives that
//! port; tests drive it in process.
//!
//! Module map:
//! - [`identity`]: who is calling and what they may do (never from payloads).
//! - [`record`]: durable `HostedSessionRecord` files.
//! - [`replay`]: transcript records → durable frames.
//! - [`live_ring`]: bounded live buffer of the running turn.
//! - [`fanout`]: per-attachment bounded queues with coalescing and `Lagged`.
//! - [`arbiter`]: per-session FIFO, compare-and-swap on the head, idempotency.
//! - [`approvals`]: first-writer-wins approval resolution.
//! - [`driver`]: the port to the agent runtime that actually runs turns.
//! - [`host`]: the composition of all of the above.

#![forbid(unsafe_code)]

pub mod approvals;
pub mod arbiter;
pub mod driver;
pub mod error;
pub mod fanout;
pub mod host;
pub mod identity;
pub mod live_ring;
pub mod record;
pub mod replay;

pub use error::HostError;
pub use host::{HostConfig, HostConnection, SessionHost};
pub use identity::{ClientIdentity, ConnectionId, caps_for_tier};
