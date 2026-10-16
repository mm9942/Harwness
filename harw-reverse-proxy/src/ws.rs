//! WebSocket route policy for the session control plane (PL-89 S10, W00 §3.1).
//!
//! Pure and I/O-free: [`WsPolicy::decide`] classifies an upgrade request and
//! returns a typed [`WsDecision`]. The adapter performs the I/O and makes no
//! policy decisions. Fails closed: anything not positively recognized is denied.
//!
//! Rules (mirroring `harw-session-ws::upgrade::validate_upgrade`, plus the edge
//! rules of the masterplan section 14):
//!
//! - method `GET`, exact configured host and path, no query;
//! - `Connection` names `upgrade`, exactly one `Upgrade: websocket`, exactly
//!   one `Sec-WebSocket-Version: 13`, exactly one well-formed key;
//! - the subprotocol `harw.session.v1` must be offered; the forwarded request
//!   offers only that token, so the upstream cannot select another;
//! - `Origin` is refused unless it is in the (default empty) allowlist;
//! - `x-harw-*` and identity-bearing headers are stripped, never forwarded;
//!   `Sec-WebSocket-Extensions` is stripped (no compression: message limits are
//!   enforced on the real payload);
//! - ambiguous requests (repeated singleton headers, a body) are denied.
//!
//! `harw-session-ws` is ring A and must not become a dependency of this crate,
//! so the limits are copied as constants (see the mapping test).

use std::fmt;
use std::time::Duration;

use crate::headers::{ClientInfo, HeaderPolicy, sanitize_headers};
use crate::normalize::{format_authority, normalize_config_host, normalize_host, normalize_path};

/// Exact subprotocol token (`harw_protocol::session_wire::SESSION_WS_SUBPROTOCOL`).
pub const SESSION_WS_SUBPROTOCOL: &str = "harw.session.v1";
/// Endpoint path (`harw_protocol::session_wire::SESSION_WS_PATH`).
pub const SESSION_WS_PATH: &str = "/v1/session-ws";

/// Most request headers accepted on an upgrade.
pub const MAX_UPGRADE_HEADERS: usize = 128;

/// Per-route WebSocket limits. Field for field the same as
/// `harw_session_ws::WsLimits`; see [`WsLimits::SESSION_V1`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsLimits {
    /// Largest inbound message after reassembly, bytes.
    pub max_message_bytes: usize,
    /// Largest single inbound frame payload, bytes.
    pub max_frame_bytes: usize,
    /// Time to complete `session.hello` after the upgrade.
    pub hello_timeout: Duration,
    /// Concurrent requests per connection.
    pub max_in_flight: usize,
    /// Frames buffered per attachment.
    pub attachment_buffer: usize,
    /// Responses and control messages buffered ahead of the writer.
    pub response_buffer: usize,
    /// Attached sessions per connection.
    pub max_attachments: usize,
    /// Interval between server pings.
    pub ping_interval: Duration,
    /// Time without any inbound message (pongs included) before close.
    pub idle_timeout: Duration,
}

const MIB: usize = 1024 * 1024;

impl WsLimits {
    /// Copy of `harw_session_ws::WsLimits::default()` (harw-session-ws
    /// `src/limits.rs`). A documented duplicate, not a link: if that default
    /// changes, update this copy and the mapping test together.
    pub const SESSION_V1: Self = Self {
        max_message_bytes: MIB,
        max_frame_bytes: MIB,
        hello_timeout: Duration::from_secs(10),
        max_in_flight: 32,
        attachment_buffer: 16,
        response_buffer: 64,
        max_attachments: 32,
        ping_interval: Duration::from_secs(15),
        idle_timeout: Duration::from_secs(60),
    };

    /// Hard ceilings: a configuration above any of these is rejected.
    pub const CEILING: Self = Self {
        max_message_bytes: 16 * MIB,
        max_frame_bytes: 16 * MIB,
        hello_timeout: Duration::from_secs(60),
        max_in_flight: 1024,
        attachment_buffer: 1024,
        response_buffer: 4096,
        max_attachments: 1024,
        ping_interval: Duration::from_secs(120),
        idle_timeout: Duration::from_secs(600),
    };

    /// `None` when the limits are acceptable, else the offending field name.
    fn violation(&self) -> Option<&'static str> {
        let c = Self::CEILING;
        let checks: [(&'static str, bool); 10] = [
            (
                "max_message_bytes",
                self.max_message_bytes == 0 || self.max_message_bytes > c.max_message_bytes,
            ),
            (
                "max_frame_bytes",
                self.max_frame_bytes == 0 || self.max_frame_bytes > c.max_frame_bytes,
            ),
            (
                "max_frame_bytes>max_message_bytes",
                self.max_frame_bytes > self.max_message_bytes,
            ),
            (
                "hello_timeout",
                self.hello_timeout.is_zero() || self.hello_timeout > c.hello_timeout,
            ),
            (
                "max_in_flight",
                self.max_in_flight == 0 || self.max_in_flight > c.max_in_flight,
            ),
            (
                "attachment_buffer",
                self.attachment_buffer == 0 || self.attachment_buffer > c.attachment_buffer,
            ),
            (
                "response_buffer",
                self.response_buffer == 0 || self.response_buffer > c.response_buffer,
            ),
            (
                "max_attachments",
                self.max_attachments == 0 || self.max_attachments > c.max_attachments,
            ),
            (
                "ping_interval",
                self.ping_interval.is_zero() || self.ping_interval > c.ping_interval,
            ),
            (
                "idle_timeout",
                self.idle_timeout <= self.ping_interval || self.idle_timeout > c.idle_timeout,
            ),
        ];
        checks.iter().find(|(_, bad)| *bad).map(|(n, _)| *n)
    }
}

impl Default for WsLimits {
    fn default() -> Self {
        Self::SESSION_V1
    }
}

/// Why a [`WsPolicy`] was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsConfigError {
    /// Empty route id.
    BadId,
    /// Host is not a valid normalized host without port.
    BadHost,
    /// Path is not a normalized absolute path.
    BadPath,
    /// A limit is zero, above its ceiling or inconsistent (field name).
    BadLimits(&'static str),
    /// An origin allowlist entry is not `http(s)://host[:port]` (entry).
    BadOrigin(String),
}

impl fmt::Display for WsConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadId => f.write_str("websocket route: empty id"),
            Self::BadHost => f.write_str("websocket route: invalid host"),
            Self::BadPath => f.write_str("websocket route: invalid path"),
            Self::BadLimits(n) => write!(f, "websocket route: invalid limit {n}"),
            Self::BadOrigin(o) => write!(f, "websocket route: invalid origin entry {o:?}"),
        }
    }
}

impl std::error::Error for WsConfigError {}

/// Why an upgrade is denied, with the status an adapter should answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsDeny {
    /// Malformed or ambiguous `Host` (400).
    BadHost,
    /// Host not served by this route (421).
    UnknownHost,
    /// Malformed target or traversal attempt (400).
    BadPath,
    /// Path (after normalization) is not the route path, or a query is present (404).
    NoRoute,
    /// Method is not `GET` (405).
    WrongMethod,
    /// Malformed header, too many headers or a body (400).
    BadHeader,
    /// Not a (single, unambiguous) websocket upgrade (426).
    NotAnUpgrade,
    /// `Sec-WebSocket-Version` missing, repeated or not `13` (426).
    WrongVersion,
    /// `Sec-WebSocket-Key` missing, repeated or malformed (400).
    BadKey,
    /// `harw.session.v1` not offered, or a malformed offer list (400).
    MissingSubprotocol,
    /// `Origin` present and not allowlisted (403).
    OriginRefused,
}

impl WsDeny {
    /// HTTP status for the denial.
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            Self::BadHost
            | Self::BadPath
            | Self::BadHeader
            | Self::BadKey
            | Self::MissingSubprotocol => 400,
            Self::UnknownHost => 421,
            Self::NoRoute => 404,
            Self::WrongMethod => 405,
            Self::NotAnUpgrade | Self::WrongVersion => 426,
            Self::OriginRefused => 403,
        }
    }
}

impl fmt::Display for WsDeny {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadHost => "malformed or ambiguous Host header",
            Self::UnknownHost => "no websocket route for this host",
            Self::BadPath => "malformed request target",
            Self::NoRoute => "no websocket route for this path",
            Self::WrongMethod => "session websocket requires GET",
            Self::BadHeader => "malformed or ambiguous headers",
            Self::NotAnUpgrade => "websocket upgrade required",
            Self::WrongVersion => "websocket version 13 required",
            Self::BadKey => "missing or malformed Sec-WebSocket-Key",
            Self::MissingSubprotocol => "subprotocol harw.session.v1 required",
            Self::OriginRefused => "Origin header refused",
        })
    }
}

impl std::error::Error for WsDeny {}

/// The parts of an upgrade request the decision needs.
#[derive(Debug, Clone, Copy)]
pub struct WsRequest<'a> {
    /// Method as sent.
    pub method: &'a str,
    /// The single `Host` header value as sent.
    pub host: &'a str,
    /// Request target (`path[?query]`) as sent.
    pub target: &'a str,
    /// All request headers as sent, in order, repeats included. `Host` may be
    /// included; it is dropped from the forwarded set.
    pub headers: &'a [(String, String)],
    /// The client as the proxy saw it.
    pub client: ClientInfo,
}

/// An admitted upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsRoute {
    /// Route id.
    pub route_id: String,
    /// Normalized path to request upstream.
    pub upstream_target: String,
    /// Normalized original host, for `X-Forwarded-Host`.
    pub original_host: String,
    /// The one subprotocol the upstream may select.
    pub subprotocol: &'static str,
    /// Headers to send upstream: hygiene applied, `x-harw-*` and identity
    /// headers removed, the subprotocol offer reduced to the pinned token.
    pub forward_headers: Vec<(String, String)>,
}

/// The outcome of [`WsPolicy::decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsDecision {
    /// Forward the upgrade under these limits.
    Allow {
        /// The admitted route (boxed: it is much larger than `Deny`).
        route: Box<WsRoute>,
        /// Limits to enforce on the connection.
        limits: WsLimits,
    },
    /// Refuse it.
    Deny {
        /// Why.
        reason: WsDeny,
    },
}

/// Headers that carry identity (or credentials for it). The control plane
/// establishes identity in the transport, so none of these cross the proxy.
/// `x-harw-*` is dropped separately by [`sanitize_headers`].
const STRIPPED_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "remote-user",
    "x-user",
    "x-user-id",
    "x-remote-user",
    "x-forwarded-user",
    "x-forwarded-email",
    "x-forwarded-groups",
    "x-authenticated-user",
    "x-auth-request-user",
    "x-auth-request-email",
    "x-principal",
    "x-tenant",
    "x-tenant-id",
    "sec-websocket-extensions",
];

/// Policy of one WebSocket route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsPolicy {
    route_id: String,
    host: String,
    path: String,
    limits: WsLimits,
    origins: Vec<String>,
}

fn normalize_origin(raw: &str) -> Option<String> {
    let (scheme, rest) = raw.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let h = normalize_host(rest)?;
    Some(format!("{scheme}://{}", format_authority(&h.host, h.port)))
}

/// Every value of the named header (ASCII case-insensitive), in order.
fn values<'a>(headers: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .collect()
}

/// Base64 of exactly 16 bytes: 22 alphabet characters (the last with zero low
/// bits) and `==`.
fn valid_key(k: &str) -> bool {
    let b = k.as_bytes();
    b.len() == 24
        && b.ends_with(b"==")
        && b[..22]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/')
        && matches!(b[21], b'A' | b'Q' | b'g' | b'w')
}

fn is_token(t: &str) -> bool {
    !t.is_empty()
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

impl WsPolicy {
    /// Build a validated policy. `path` is normally [`SESSION_WS_PATH`],
    /// `limits` [`WsLimits::SESSION_V1`], `origins` empty (no browser origin).
    pub fn new(
        route_id: &str,
        host: &str,
        path: &str,
        limits: WsLimits,
        origins: &[&str],
    ) -> Result<Self, WsConfigError> {
        if route_id.is_empty() {
            return Err(WsConfigError::BadId);
        }
        let host = normalize_config_host(host).ok_or(WsConfigError::BadHost)?;
        match normalize_path(path) {
            Some((p, None)) if p == path && p.starts_with('/') => {}
            _ => return Err(WsConfigError::BadPath),
        }
        if let Some(field) = limits.violation() {
            return Err(WsConfigError::BadLimits(field));
        }
        let mut allowed = Vec::with_capacity(origins.len());
        for o in origins {
            match normalize_origin(o) {
                Some(n) => allowed.push(n),
                None => return Err(WsConfigError::BadOrigin((*o).to_owned())),
            }
        }
        Ok(Self {
            route_id: route_id.to_owned(),
            host,
            path: path.to_owned(),
            limits,
            origins: allowed,
        })
    }

    /// Decide an upgrade request. Pure and deterministic.
    #[must_use]
    pub fn decide(&self, req: &WsRequest<'_>) -> WsDecision {
        match self.check(req) {
            Ok((route, limits)) => WsDecision::Allow {
                route: Box::new(route),
                limits,
            },
            Err(reason) => WsDecision::Deny { reason },
        }
    }

    fn check(&self, req: &WsRequest<'_>) -> Result<(WsRoute, WsLimits), WsDeny> {
        // Normalize once, match on the normalized form.
        let host = normalize_host(req.host).ok_or(WsDeny::BadHost)?;
        let (path, query) = normalize_path(req.target).ok_or(WsDeny::BadPath)?;
        if host.host != self.host {
            return Err(WsDeny::UnknownHost);
        }
        if path != self.path || query.is_some() {
            return Err(WsDeny::NoRoute);
        }
        if req.method != "GET" {
            return Err(WsDeny::WrongMethod);
        }
        let h = req.headers;
        if h.len() > MAX_UPGRADE_HEADERS {
            return Err(WsDeny::BadHeader);
        }
        // A handshake has no body.
        if !values(h, "transfer-encoding").is_empty()
            || values(h, "content-length").iter().any(|v| v.trim() != "0")
        {
            return Err(WsDeny::BadHeader);
        }
        // Several `Host` headers are ambiguous even if `req.host` was one.
        if values(h, "host").len() > 1 {
            return Err(WsDeny::BadHost);
        }
        let connection_upgrade = values(h, "connection")
            .iter()
            .flat_map(|v| v.split(','))
            .any(|t| t.trim().eq_ignore_ascii_case("upgrade"));
        let upgrade = values(h, "upgrade");
        if !connection_upgrade
            || upgrade.len() != 1
            || !upgrade
                .first()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("websocket"))
        {
            return Err(WsDeny::NotAnUpgrade);
        }
        let version = values(h, "sec-websocket-version");
        if version.len() != 1 || version.first().is_none_or(|v| v.trim() != "13") {
            return Err(WsDeny::WrongVersion);
        }
        let key = values(h, "sec-websocket-key");
        if key.len() != 1 || key.first().is_none_or(|k| !valid_key(k)) {
            return Err(WsDeny::BadKey);
        }
        // Offers: comma lists across any number of headers. An empty element
        // or a non-token is malformed; the pinned token must be offered
        // verbatim (tokens are case-sensitive).
        let mut offered = false;
        for v in values(h, "sec-websocket-protocol") {
            for t in v.split(',') {
                let t = t.trim();
                if !is_token(t) {
                    return Err(WsDeny::MissingSubprotocol);
                }
                offered |= t == SESSION_WS_SUBPROTOCOL;
            }
        }
        if !offered {
            return Err(WsDeny::MissingSubprotocol);
        }
        // Origin: v1 refuses browsers. Only an exact allowlisted origin passes.
        let origin = values(h, "origin");
        if !origin.is_empty() {
            let ok = origin.len() == 1
                && origin
                    .first()
                    .and_then(|o| normalize_origin(o))
                    .is_some_and(|o| self.origins.contains(&o));
            if !ok {
                return Err(WsDeny::OriginRefused);
            }
        }
        let policy = HeaderPolicy {
            strip: STRIPPED_HEADERS.iter().map(|s| (*s).to_owned()).collect(),
            set: vec![(
                "sec-websocket-protocol".to_owned(),
                SESSION_WS_SUBPROTOCOL.to_owned(),
            )],
            allow_upgrade: true,
        };
        let original_host = format_authority(&host.host, host.port);
        let forward_headers = sanitize_headers(h, &original_host, req.client, &policy)
            .map_err(|_| WsDeny::BadHeader)?;
        Ok((
            WsRoute {
                route_id: self.route_id.clone(),
                upstream_target: path,
                original_host,
                subprotocol: SESSION_WS_SUBPROTOCOL,
                forward_headers,
            },
            self.limits,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headers::Proto;
    use crate::test_support::{TestError, TestResult, ensure};

    const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

    type Headers = Vec<(String, String)>;

    fn h(pairs: &[(&str, &str)]) -> Headers {
        pairs
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn good() -> Headers {
        h(&[
            ("Host", "hub.example.org"),
            ("Connection", "Upgrade"),
            ("Upgrade", "websocket"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", KEY),
            ("Sec-WebSocket-Protocol", "harw.session.v1"),
        ])
    }

    fn with(extra: &[(&str, &str)]) -> Headers {
        let mut v = good();
        v.extend(h(extra));
        v
    }

    /// `good()` with every header of this name removed and `replacement` added.
    fn replace(name: &str, replacement: &[(&str, &str)]) -> Headers {
        let mut v: Headers = good()
            .into_iter()
            .filter(|(n, _)| !n.eq_ignore_ascii_case(name))
            .collect();
        v.extend(h(replacement));
        v
    }

    fn policy(origins: &[&str]) -> Result<WsPolicy, WsConfigError> {
        WsPolicy::new(
            "session-ws",
            "hub.example.org",
            SESSION_WS_PATH,
            WsLimits::SESSION_V1,
            origins,
        )
    }

    fn client() -> ClientInfo {
        ClientInfo {
            addr: [203, 0, 113, 9].into(),
            proto: Proto::Https,
        }
    }

    fn run(p: &WsPolicy, method: &str, target: &str, headers: &[(String, String)]) -> WsDecision {
        p.decide(&WsRequest {
            method,
            host: "hub.example.org",
            target,
            headers,
            client: client(),
        })
    }

    fn on_path(p: &WsPolicy, headers: &[(String, String)]) -> WsDecision {
        run(p, "GET", SESSION_WS_PATH, headers)
    }

    fn denied(d: &WsDecision, want: WsDeny) -> bool {
        matches!(d, WsDecision::Deny { reason } if *reason == want)
    }

    fn allowed(d: WsDecision) -> Result<(WsRoute, WsLimits), TestError> {
        match d {
            WsDecision::Allow { route, limits } => Ok((*route, limits)),
            WsDecision::Deny { reason } => Err(TestError(format!("expected allow, got {reason}"))),
        }
    }

    fn names(r: &WsRoute) -> Vec<String> {
        r.forward_headers
            .iter()
            .map(|(n, _)| n.to_ascii_lowercase())
            .collect()
    }

    #[test]
    fn a_well_formed_upgrade_is_allowed_with_the_session_limits() -> TestResult {
        let p = policy(&[])?;
        let (route, limits) = allowed(on_path(&p, &good()))?;
        ensure(limits == WsLimits::SESSION_V1, "limits")?;
        ensure(route.subprotocol == "harw.session.v1", "pinned")?;
        ensure(route.upstream_target == SESSION_WS_PATH, "target")?;
        ensure(!names(&route).contains(&"host".to_owned()), "host dropped")
    }

    #[test]
    fn limits_mirror_harw_session_ws_defaults() -> TestResult {
        // Documented mapping to `WsLimits::default()` in
        // harw-session-ws/src/limits.rs. This crate must not depend on
        // harw-session-ws (ring A), so the values are copied; update both
        // together.
        let l = WsLimits::SESSION_V1;
        ensure(
            l.max_message_bytes == 1024 * 1024,
            "max_message_bytes = 1 MiB",
        )?;
        ensure(l.max_frame_bytes == 1024 * 1024, "max_frame_bytes = 1 MiB")?;
        ensure(l.hello_timeout == Duration::from_secs(10), "hello_timeout")?;
        ensure(l.max_in_flight == 32, "max_in_flight = 32")?;
        ensure(l.attachment_buffer == 16, "attachment_buffer = 16")?;
        ensure(l.response_buffer == 64, "response_buffer = 64")?;
        ensure(l.max_attachments == 32, "max_attachments = 32")?;
        ensure(l.ping_interval == Duration::from_secs(15), "ping_interval")?;
        ensure(l.idle_timeout == Duration::from_secs(60), "idle_timeout")?;
        ensure(l.violation().is_none(), "the defaults satisfy the ceilings")?;
        ensure(
            WsLimits::CEILING.violation().is_none(),
            "the ceiling itself is consistent",
        )?;
        ensure(WsLimits::default() == l, "Default is the session profile")
    }

    #[test]
    fn wrong_or_missing_subprotocol_is_denied() -> TestResult {
        let p = policy(&[])?;
        for bad in [
            "harw.session.v2",
            "HARW.SESSION.V1",
            "harw.session.v1x",
            "chat",
            "harw.session.v1,,chat",
        ] {
            let hs = replace("sec-websocket-protocol", &[("Sec-WebSocket-Protocol", bad)]);
            ensure(denied(&on_path(&p, &hs), WsDeny::MissingSubprotocol), bad)?;
        }
        ensure(
            denied(
                &on_path(&p, &replace("sec-websocket-protocol", &[])),
                WsDeny::MissingSubprotocol,
            ),
            "absent",
        )
    }

    #[test]
    fn the_forwarded_offer_is_reduced_to_the_pinned_token() -> TestResult {
        let p = policy(&[])?;
        let hs = replace(
            "sec-websocket-protocol",
            &[
                ("sec-websocket-protocol", "chat, harw.session.v1"),
                ("Sec-WebSocket-Protocol", "other"),
            ],
        );
        let (route, _) = allowed(on_path(&p, &hs))?;
        let offers: Vec<&str> = route
            .forward_headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("sec-websocket-protocol"))
            .map(|(_, v)| v.as_str())
            .collect();
        ensure(
            offers == ["harw.session.v1"],
            "only the pinned token goes upstream",
        )
    }

    #[test]
    fn missing_or_wrong_upgrade_headers_are_denied() -> TestResult {
        let p = policy(&[])?;
        let d = |hs: Headers| on_path(&p, &hs);
        ensure(
            denied(&d(replace("connection", &[])), WsDeny::NotAnUpgrade),
            "no connection",
        )?;
        ensure(
            denied(&d(replace("upgrade", &[])), WsDeny::NotAnUpgrade),
            "no upgrade",
        )?;
        ensure(
            denied(
                &d(replace("upgrade", &[("Upgrade", "h2c")])),
                WsDeny::NotAnUpgrade,
            ),
            "h2c",
        )?;
        ensure(
            denied(
                &d(replace("connection", &[("Connection", "keep-alive")])),
                WsDeny::NotAnUpgrade,
            ),
            "keep-alive only",
        )?;
        ensure(
            denied(
                &d(replace("sec-websocket-version", &[])),
                WsDeny::WrongVersion,
            ),
            "no version",
        )?;
        ensure(
            denied(
                &d(replace(
                    "sec-websocket-version",
                    &[("Sec-WebSocket-Version", "8")],
                )),
                WsDeny::WrongVersion,
            ),
            "version 8",
        )?;
        ensure(
            denied(&d(replace("sec-websocket-key", &[])), WsDeny::BadKey),
            "no key",
        )?;
        for bad in [
            "short",
            "dGhlIHNhbXBsZSBub25jZR==",
            "dGhlIHNhbXBsZSBub25jZQ=A",
            "dGhlIHNhbXBsZSBub25jZQ!!",
        ] {
            ensure(
                denied(
                    &d(replace("sec-websocket-key", &[("Sec-WebSocket-Key", bad)])),
                    WsDeny::BadKey,
                ),
                bad,
            )?;
        }
        ensure(
            denied(
                &run(&p, "POST", SESSION_WS_PATH, &good()),
                WsDeny::WrongMethod,
            ),
            "POST",
        )?;
        ensure(
            denied(
                &run(&p, "get", SESSION_WS_PATH, &good()),
                WsDeny::WrongMethod,
            ),
            "lowercase method",
        )
    }

    #[test]
    fn case_variants_of_header_names_and_tokens_are_accepted() -> TestResult {
        let p = policy(&[])?;
        let hs = h(&[
            ("hOsT", "hub.example.org"),
            ("CONNECTION", "keep-alive, UPGRADE"),
            ("uPgRaDe", "WebSocket"),
            ("sec-websocket-version", "13"),
            ("SEC-WEBSOCKET-KEY", KEY),
            ("sec-websocket-protocol", "harw.session.v1"),
        ]);
        allowed(on_path(&p, &hs))?;
        Ok(())
    }

    #[test]
    fn duplicate_singleton_headers_are_ambiguous_and_denied() -> TestResult {
        let p = policy(&[])?;
        let cases: Vec<((&str, &str), WsDeny)> = vec![
            (("upgrade", "websocket"), WsDeny::NotAnUpgrade),
            (("sec-websocket-version", "13"), WsDeny::WrongVersion),
            (("SEC-WEBSOCKET-KEY", KEY), WsDeny::BadKey),
            (("host", "evil.example.org"), WsDeny::BadHost),
            (("Content-Length", "5"), WsDeny::BadHeader),
            (("Transfer-Encoding", "chunked"), WsDeny::BadHeader),
        ];
        for (extra, want) in cases {
            ensure(
                denied(&on_path(&p, &with(&[extra])), want),
                "duplicate or body",
            )?;
        }
        Ok(())
    }

    #[test]
    fn origin_is_refused_by_default_and_only_exact_allowlist_entries_pass() -> TestResult {
        let closed = policy(&[])?;
        for o in ["https://app.example.org", "null", "", "*"] {
            ensure(
                denied(
                    &on_path(&closed, &with(&[("Origin", o)])),
                    WsDeny::OriginRefused,
                ),
                o,
            )?;
        }
        let open = policy(&["https://App.Example.org"])?;
        allowed(on_path(
            &open,
            &with(&[("origin", "https://app.example.org")]),
        ))?;
        for o in [
            "http://app.example.org",
            "https://app.example.org:8443",
            "https://app.example.org.evil.test",
            "https://other.example.org",
        ] {
            ensure(
                denied(
                    &on_path(&open, &with(&[("Origin", o)])),
                    WsDeny::OriginRefused,
                ),
                o,
            )?;
        }
        let twice = with(&[
            ("Origin", "https://app.example.org"),
            ("Origin", "https://app.example.org"),
        ]);
        ensure(
            denied(&on_path(&open, &twice), WsDeny::OriginRefused),
            "repeated Origin",
        )?;
        for bad in [
            "*",
            "null",
            "app.example.org",
            "ftp://a.example.org",
            "https://a.example.org/x",
            "https://u@a.example.org",
        ] {
            ensure(
                matches!(policy(&[bad]), Err(WsConfigError::BadOrigin(_))),
                bad,
            )?;
        }
        Ok(())
    }

    #[test]
    fn smuggled_identity_headers_never_reach_the_upstream() -> TestResult {
        let p = policy(&[])?;
        let hs = with(&[
            ("x-harw-user", "owner"),
            ("X-HARW-Tenant", "other"),
            ("X-Harw-Principal", "root"),
            ("Authorization", "Bearer t"),
            ("Cookie", "s=1"),
            ("X-Forwarded-User", "root"),
            ("Sec-WebSocket-Extensions", "permessage-deflate"),
            ("X-Request-Id", "kept"),
        ]);
        let (route, _) = allowed(on_path(&p, &hs))?;
        let n = names(&route);
        for gone in [
            "x-harw-user",
            "x-harw-tenant",
            "x-harw-principal",
            "authorization",
            "cookie",
            "x-forwarded-user",
            "sec-websocket-extensions",
        ] {
            ensure(!n.contains(&gone.to_owned()), gone)?;
        }
        ensure(
            n.contains(&"x-request-id".to_owned()),
            "ordinary header kept",
        )?;
        ensure(
            route
                .forward_headers
                .contains(&("x-forwarded-for".to_owned(), "203.0.113.9".to_owned())),
            "proxy-set forwarding header",
        )
    }

    #[test]
    fn host_and_path_are_normalized_before_matching() -> TestResult {
        let p = policy(&[])?;
        allowed(run(&p, "GET", "/v1/./session-ws", &good()))?;
        allowed(run(&p, "GET", "/v1/%73ession-ws", &good()))?;
        ensure(
            denied(
                &run(&p, "GET", "/v1/session-ws?token=x", &good()),
                WsDeny::NoRoute,
            ),
            "query refused",
        )?;
        ensure(
            denied(
                &run(&p, "GET", "/v1/session-ws/x", &good()),
                WsDeny::NoRoute,
            ),
            "longer path",
        )?;
        ensure(
            denied(
                &run(&p, "GET", "/v1/%2e%2e/session-ws", &good()),
                WsDeny::BadPath,
            ),
            "encoded traversal",
        )?;
        let hs = good();
        let on_host = |host: &str| {
            p.decide(&WsRequest {
                method: "GET",
                host,
                target: SESSION_WS_PATH,
                headers: &hs,
                client: client(),
            })
        };
        ensure(
            denied(&on_host("evil.example.org"), WsDeny::UnknownHost),
            "other host",
        )?;
        ensure(
            denied(&on_host("hub.example.org, evil.test"), WsDeny::BadHost),
            "host list",
        )?;
        allowed(on_host("HUB.Example.org."))?;
        Ok(())
    }

    #[test]
    fn oversize_or_inconsistent_limits_are_rejected_at_configuration() -> TestResult {
        let base = WsLimits::SESSION_V1;
        let c = WsLimits::CEILING;
        let cases = [
            WsLimits {
                max_message_bytes: c.max_message_bytes + 1,
                ..base
            },
            WsLimits {
                max_message_bytes: 0,
                ..base
            },
            WsLimits {
                max_frame_bytes: base.max_message_bytes + 1,
                ..base
            },
            WsLimits {
                max_in_flight: 0,
                ..base
            },
            WsLimits {
                max_attachments: c.max_attachments + 1,
                ..base
            },
            WsLimits {
                ping_interval: Duration::ZERO,
                ..base
            },
            WsLimits {
                idle_timeout: base.ping_interval,
                ..base
            },
            WsLimits {
                idle_timeout: c.idle_timeout + Duration::from_secs(1),
                ..base
            },
            WsLimits {
                hello_timeout: Duration::ZERO,
                ..base
            },
        ];
        for l in cases {
            ensure(
                matches!(
                    WsPolicy::new("r", "hub.example.org", SESSION_WS_PATH, l, &[]),
                    Err(WsConfigError::BadLimits(_))
                ),
                "must be rejected",
            )?;
        }
        ensure(
            WsPolicy::new("r", "hub.example.org", SESSION_WS_PATH, c, &[]).is_ok(),
            "the ceiling is admitted",
        )?;
        ensure(
            matches!(
                WsPolicy::new("", "hub.example.org", SESSION_WS_PATH, base, &[]),
                Err(WsConfigError::BadId)
            ),
            "id",
        )?;
        ensure(
            matches!(
                WsPolicy::new("r", "a b", SESSION_WS_PATH, base, &[]),
                Err(WsConfigError::BadHost)
            ),
            "host",
        )?;
        ensure(
            matches!(
                WsPolicy::new("r", "hub.example.org", "/a/../b", base, &[]),
                Err(WsConfigError::BadPath)
            ),
            "path",
        )
    }

    #[test]
    fn too_many_headers_and_control_characters_are_denied() -> TestResult {
        let p = policy(&[])?;
        let mut many = good();
        for i in 0..MAX_UPGRADE_HEADERS {
            many.push((format!("x-n{i}"), "v".into()));
        }
        ensure(denied(&on_path(&p, &many), WsDeny::BadHeader), "too many")?;
        ensure(
            denied(
                &on_path(&p, &with(&[("X-A", "a\r\nX-B: b")])),
                WsDeny::BadHeader,
            ),
            "CRLF",
        )
    }
}
