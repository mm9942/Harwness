//! Tailscale support for harw: reach the control plane from your own
//! tailnet (for example a phone) without a public port.
//!
//! # Pieces
//! - [`addr`]: which addresses belong to a tailnet (`100.64.0.0/10`,
//!   `fd7a:115c:a1e0::/48`).
//! - [`localapi`]: a minimal client for `tailscaled`'s LocalAPI over its Unix
//!   socket (`status`, `whois`). No embedded Tailscale implementation: harw
//!   talks to the `tailscaled` that is already running.
//! - [`gate`]: admission of one TCP peer. The remote address must be a
//!   tailnet address, must not be this node, and `whois` must name a login;
//!   anything else is refused (fail closed).
//! - [`proxy`]: listens on the node's tailnet address and forwards admitted
//!   connections byte for byte to a dedicated Unix socket of the control
//!   plane.
//!
//! # Trust model
//! The control plane authenticates Unix-socket peers by `SO_PEERCRED`. The
//! proxy connects to its dedicated socket as harw's own user, so the web
//! server must give that socket a fixed, lower tier (Operator) instead of the
//! owner tier of the main socket. A local process of the same user that
//! reaches the dedicated socket can therefore only lose rights, never gain
//! them.

pub mod addr;
pub mod gate;
pub mod localapi;
pub mod proxy;

pub use addr::is_tailnet_ip;
pub use gate::{Admission, AdmissionFuture, GateError, TailnetGate, TailnetPeer};
pub use localapi::{LocalApi, LocalApiError, Status, WhoIs, find_socket};
pub use proxy::{DEFAULT_TAILNET_PORT, MAX_CONNECTIONS, serve_tailnet};
