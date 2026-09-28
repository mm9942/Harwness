//! `harw-session-ws`: WebSocket transport of the session control plane
//! (W00 §2.2, §3, §4).
//!
//! Transport only. This crate never decides who a caller is or what they may
//! do: a listener authenticates the peer (kernel credentials on a Unix
//! socket, the node transport's mutual handshake remotely), asks the host
//! for a [`harw_protocol::SessionPort`] bound to that identity, and hands the
//! upgraded socket plus the port to [`conn::serve_connection`].
//!
//! - [`upgrade`]: HTTP/1 upgrade validation and responses (exact subprotocol
//!   `harw.session.v1`, `Origin` refused in v1).
//! - [`codec`]: one UTF-8 JSON wire message per text frame; binary refused.
//! - [`limits`]: message sizes, hello timeout, in-flight and queue bounds.
//! - [`dispatch`]: closed method table → typed `SessionPort` calls.
//! - [`conn`]: the connection driver: hello gate, request correlation and a
//!   fair multiplexer that keeps responses and control frames ahead of
//!   delta floods.

#![forbid(unsafe_code)]

pub mod codec;
pub mod conn;
pub mod dispatch;
pub mod limits;
pub mod upgrade;

pub use conn::{ConnectionEnd, serve_connection};
pub use limits::WsLimits;
