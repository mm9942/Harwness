//! `harw-session-ws`: WebSocket transport of the session control plane
//! (W00 §2.2, §3, §4).
//!
//! Transport only. This crate never decides who a caller is or what they may
//! do: a listener authenticates the peer (kernel credentials on a Unix
//! socket, the node transport's mutual handshake remotely), asks the host
//! for a [`harw_protocol::SessionPort`] bound to that identity — plus, for
//! R18, the [`harw_protocol::ToolPort`] and [`harw_protocol::GatewayPort`]
//! when the host offers them to this connection ([`dispatch::Ports`]) — and
//! hands the upgraded socket plus the ports to [`conn::serve_connection`]
//! (session methods only) or [`conn::serve_connection_with`].
//!
//! - [`upgrade`]: HTTP/1 upgrade validation and responses (exact subprotocol
//!   `harw.session.v1`, `Origin` refused in v1).
//! - [`codec`]: one UTF-8 JSON wire message per text frame; binary refused.
//! - [`limits`]: message sizes, hello timeout, in-flight and queue bounds.
//! - [`dispatch`]: closed method tables → typed `SessionPort`, `ToolPort`
//!   and `GatewayPort` calls.
//! - [`conn`]: the connection driver: hello gate, request correlation and a
//!   fair multiplexer that keeps responses and control frames ahead of
//!   delta floods.

#![forbid(unsafe_code)]

pub mod codec;
pub mod conn;
pub mod dispatch;
pub mod limits;
pub mod upgrade;

pub use conn::{ConnectionEnd, serve_connection, serve_connection_with};
pub use dispatch::Ports;
pub use limits::WsLimits;
// Skeleton (W00 D10): client crates wrap an upgraded stream without taking a
// direct tungstenite dependency; this crate stays the only owner.
pub use tokio_tungstenite::WebSocketStream;
pub use tokio_tungstenite::tungstenite::protocol::Role;
