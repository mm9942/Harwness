//! `harw-session-daemon`: the local session daemon (Cloud Hub wiring W2).
//!
//! Composition only. It binds a Unix socket, takes the peer's **kernel
//! credentials** as the identity (never anything from the wire), upgrades the
//! HTTP/1 connection to `harw.session.v1` and serves it against one
//! [`harw_session_host::SessionHost`]. Protocol rules stay in
//! `harw-session-ws`, session and policy rules in `harw-session-host`.
//!
//! # Safety properties
//! - The socket lives in a private directory (no group/other access) and is
//!   created with owner-only permissions; a stale socket is replaced only
//!   after a connect attempt proves nobody is listening, a live one is never
//!   taken over and a non-socket file is never touched.
//! - Only peers whose uid equals the configured uid are served; everyone else
//!   is dropped before any HTTP is read.
//! - Connections are bounded; header reads are time-limited.
//! - Shutdown stops accepting, drains the host and closes live sessions with
//!   the `Draining` reason, then removes the socket file.

#![forbid(unsafe_code)]

mod compose;
mod server;

pub use compose::{Daemon, DaemonServices};
pub use server::{DaemonError, UdsConfig, UdsServer};
