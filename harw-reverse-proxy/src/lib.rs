//! `harw-reverse-proxy`: the policy core of the reverse proxy (PL-89, P0).
//!
//! Pure functions and types, no sockets, no HTTP stack and no Pingora. An
//! adapter over Pingora or hyper (a later step, after the dependency decision
//! in `docs/planning/89-reverse-proxy/README.md`) calls [`RouteTable::decide`]
//! and [`sanitize_headers`] and performs the I/O; it makes no policy decisions.
//!
//! - [`RouteTable`] is closed: a request matches exactly one route or is
//!   refused. There is no default upstream.
//! - Upstreams are literal socket addresses classified with
//!   [`harw_egress::classify`]; cloud metadata, link-local, multicast and other
//!   reserved ranges are never allowed, private and public ranges only through
//!   an explicit [`UpstreamScope`].
//! - `Host` and the request path are normalized once; matching and the upstream
//!   request use the normalized form.
//! - [`WsPolicy`] classifies a session WebSocket upgrade (subprotocol pinned,
//!   Origin refused unless allowlisted, identity headers stripped, limits).
//! - [`sanitize_headers`] drops hop-by-hop and spoofable headers and sets the
//!   forwarding headers itself.

#![forbid(unsafe_code)]

mod group;
mod headers;
mod normalize;
mod route;
mod ws;

#[cfg(test)]
mod test_support;

pub use group::{Backend, GroupError, UpstreamGroup, may_retry};
pub use headers::{ClientInfo, HeaderPolicy, Proto, sanitize_headers};
pub use normalize::{
    NormalizedHost, format_authority, normalize_config_host, normalize_host, normalize_path,
};
pub use route::{
    ConfigError, Decision, Forward, Refusal, RequestHead, Route, RouteTable, UpstreamScope,
};
pub use ws::{
    MAX_UPGRADE_HEADERS, SESSION_WS_PATH, SESSION_WS_SUBPROTOCOL, WsConfigError, WsDecision,
    WsDeny, WsLimits, WsPolicy, WsRequest, WsRoute,
};
