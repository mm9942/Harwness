//! Closed route table and the forward/refuse decision.

use std::fmt;
use std::net::SocketAddr;

use harw_egress::{AddrClass, classify};

use crate::normalize::{normalize_host, normalize_path};

/// How far an upstream address may reach. Loopback is always allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamScope {
    /// Loopback only (the default for a local service).
    Loopback,
    /// Loopback plus private, unique-local and CGNAT (e.g. a Tailscale node).
    Private,
    /// Everything above plus global unicast. Never metadata, link-local,
    /// multicast, documentation, benchmark, reserved, unspecified or broadcast.
    Public,
}

impl UpstreamScope {
    /// Whether `addr`'s class is admitted by this scope. Shared with
    /// [`crate::UpstreamGroup`] so route and group use one rule.
    #[must_use]
    pub fn admits_addr(self, addr: SocketAddr) -> bool {
        self.admits(classify(addr.ip()))
    }

    fn admits(self, class: AddrClass) -> bool {
        match class {
            AddrClass::Loopback => true,
            AddrClass::Private | AddrClass::UniqueLocal | AddrClass::Cgnat => {
                matches!(self, Self::Private | Self::Public)
            }
            AddrClass::Public => matches!(self, Self::Public),
            AddrClass::LinkLocal
            | AddrClass::CloudMetadata
            | AddrClass::Multicast
            | AddrClass::Documentation
            | AddrClass::Benchmark
            | AddrClass::Reserved
            | AddrClass::Unspecified
            | AddrClass::Broadcast => false,
        }
    }
}

/// One route: host, path prefix, upstream and per-route limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Unique id (logs, audit).
    pub id: String,
    /// Exact host (normalized form, no port).
    pub host: String,
    /// Path prefix matched per segment (`/api` matches `/api` and `/api/x`, not
    /// `/apix`). `/` matches everything.
    pub path_prefix: String,
    /// Literal upstream address; hostnames are not accepted.
    pub upstream: SocketAddr,
    /// How far the upstream address may reach.
    pub scope: UpstreamScope,
    /// Remove the matched prefix before forwarding.
    pub strip_prefix: bool,
    /// Allowed methods, upper case. Must not be empty; `CONNECT` and `TRACE`
    /// are never allowed.
    pub methods: Vec<String>,
    /// Request body limit in bytes; must be greater than zero.
    pub max_body_bytes: u64,
}

/// Why a route table was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// Empty id or duplicate id.
    BadId(String),
    /// Host is not a valid normalized host without port.
    BadHost(String),
    /// Path prefix is not a normalized absolute path.
    BadPrefix(String),
    /// Upstream address class is not admitted by the route scope.
    UpstreamNotAllowed(String, AddrClass),
    /// Method list empty, forbidden or malformed.
    BadMethods(String),
    /// Body limit is zero.
    BadBodyLimit(String),
    /// Two routes with the same host and prefix.
    Duplicate(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadId(r) => write!(f, "route {r:?}: empty or duplicate id"),
            Self::BadHost(r) => write!(f, "route {r:?}: invalid host"),
            Self::BadPrefix(r) => write!(f, "route {r:?}: invalid path prefix"),
            Self::UpstreamNotAllowed(r, c) => {
                write!(
                    f,
                    "route {r:?}: upstream address class {c:?} is not allowed by its scope"
                )
            }
            Self::BadMethods(r) => write!(f, "route {r:?}: invalid method list"),
            Self::BadBodyLimit(r) => write!(f, "route {r:?}: body limit must be positive"),
            Self::Duplicate(r) => write!(f, "route {r:?}: duplicate host and prefix"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// A validated, closed route table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteTable {
    routes: Vec<Route>,
}

/// The parts of a request the decision needs.
#[derive(Debug, Clone, Copy)]
pub struct RequestHead<'a> {
    /// Method as sent.
    pub method: &'a str,
    /// The single `Host` header value as sent.
    pub host: &'a str,
    /// The request target (`path[?query]`) as sent.
    pub target: &'a str,
}

/// Why a request is refused, with the status an adapter should answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Malformed or ambiguous `Host` (400).
    BadHost,
    /// Malformed target or traversal attempt (400).
    BadPath,
    /// No route for this host (421 Misdirected Request).
    UnknownHost,
    /// Host known, no route for this path (404).
    NoRoute,
    /// Method not allowed on the matched route (405).
    MethodNotAllowed,
    /// Malformed header name or value (400).
    BadHeader,
}

impl Refusal {
    /// HTTP status code for the refusal.
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            Self::BadHost | Self::BadPath | Self::BadHeader => 400,
            Self::UnknownHost => 421,
            Self::NoRoute => 404,
            Self::MethodNotAllowed => 405,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::BadHost => "malformed or ambiguous Host header",
            Self::BadPath => "malformed request target",
            Self::UnknownHost => "no route for this host",
            Self::NoRoute => "no route for this path",
            Self::MethodNotAllowed => "method not allowed on this route",
            Self::BadHeader => "malformed header",
        };
        f.write_str(text)
    }
}

impl std::error::Error for Refusal {}

/// What to forward and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    /// Matched route id.
    pub route_id: String,
    /// Upstream address.
    pub upstream: SocketAddr,
    /// Path (and query) to send upstream, normalized.
    pub upstream_target: String,
    /// Normalized original host (with port if sent), for `X-Forwarded-Host`.
    pub original_host: String,
    /// Body limit for this request.
    pub max_body_bytes: u64,
}

/// The outcome of [`RouteTable::decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Forward the request.
    Forward(Forward),
    /// Refuse it.
    Refuse(Refusal),
}

fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

impl RouteTable {
    /// Validate `routes` and build the table.
    pub fn new(routes: Vec<Route>) -> Result<Self, ConfigError> {
        let mut ids = std::collections::HashSet::new();
        let mut keys = std::collections::HashSet::new();
        for r in &routes {
            if r.id.is_empty() || !ids.insert(r.id.clone()) {
                return Err(ConfigError::BadId(r.id.clone()));
            }
            let host = normalize_host(&r.host)
                .filter(|h| h.port.is_none() && h.host == r.host)
                .ok_or_else(|| ConfigError::BadHost(r.id.clone()))?;
            let (prefix, query) = normalize_path(&r.path_prefix)
                .ok_or_else(|| ConfigError::BadPrefix(r.id.clone()))?;
            let prefix_ok = query.is_none()
                && (prefix == "/" || (!prefix.ends_with('/') && prefix == r.path_prefix));
            if !prefix_ok {
                return Err(ConfigError::BadPrefix(r.id.clone()));
            }
            let class = classify(r.upstream.ip());
            if r.upstream.port() == 0 || !r.scope.admits(class) {
                return Err(ConfigError::UpstreamNotAllowed(r.id.clone(), class));
            }
            let methods_ok = !r.methods.is_empty()
                && r.methods.iter().all(|m| {
                    !m.is_empty()
                        && m.bytes().all(|b| b.is_ascii_uppercase())
                        && !matches!(m.as_str(), "CONNECT" | "TRACE")
                });
            if !methods_ok {
                return Err(ConfigError::BadMethods(r.id.clone()));
            }
            if r.max_body_bytes == 0 {
                return Err(ConfigError::BadBodyLimit(r.id.clone()));
            }
            if !keys.insert((host.host, prefix)) {
                return Err(ConfigError::Duplicate(r.id.clone()));
            }
        }
        Ok(Self { routes })
    }

    /// The validated routes.
    #[must_use]
    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    /// Decide a request: normalize host and target, find the route with the
    /// longest matching prefix for that host, check the method, and build the
    /// upstream target. Pure and deterministic.
    #[must_use]
    pub fn decide(&self, request: &RequestHead<'_>) -> Decision {
        let Some(host) = normalize_host(request.host) else {
            return Decision::Refuse(Refusal::BadHost);
        };
        let Some((path, query)) = normalize_path(request.target) else {
            return Decision::Refuse(Refusal::BadPath);
        };
        let segs = path_segments(&path);
        let mut host_known = false;
        let mut best: Option<(&Route, usize)> = None;
        for route in self.routes.iter().filter(|r| r.host == host.host) {
            host_known = true;
            let prefix = path_segments(&route.path_prefix);
            if segs.len() >= prefix.len()
                && segs[..prefix.len()] == prefix[..]
                && best.is_none_or(|(_, len)| prefix.len() > len)
            {
                best = Some((route, prefix.len()));
            }
        }
        let Some((route, prefix_len)) = best else {
            return Decision::Refuse(if host_known {
                Refusal::NoRoute
            } else {
                Refusal::UnknownHost
            });
        };
        if !route.methods.iter().any(|m| m == request.method) {
            return Decision::Refuse(Refusal::MethodNotAllowed);
        }
        let mut target = if route.strip_prefix {
            let rest = segs[prefix_len..].join("/");
            let mut t = format!("/{rest}");
            if path.ends_with('/') && t.len() > 1 {
                t.push('/');
            }
            t
        } else {
            path
        };
        if let Some(q) = query {
            target.push('?');
            target.push_str(&q);
        }
        let original_host = match host.port {
            Some(p) => format!("{}:{p}", host.host),
            None => host.host,
        };
        Decision::Forward(Forward {
            route_id: route.id.clone(),
            upstream: route.upstream,
            upstream_target: target,
            original_host,
            max_body_bytes: route.max_body_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    fn route(
        id: &str,
        host: &str,
        prefix: &str,
        upstream: &str,
    ) -> Result<Route, std::net::AddrParseError> {
        Ok(Route {
            id: id.into(),
            host: host.into(),
            path_prefix: prefix.into(),
            upstream: upstream.parse()?,
            scope: UpstreamScope::Loopback,
            strip_prefix: false,
            methods: vec!["GET".into(), "POST".into()],
            max_body_bytes: 1024,
        })
    }

    fn table() -> Result<RouteTable, Box<dyn std::error::Error>> {
        Ok(RouteTable::new(vec![
            route("api", "hub.example.org", "/api", "127.0.0.1:9000")?,
            route("root", "hub.example.org", "/", "127.0.0.1:9001")?,
            route("deep", "hub.example.org", "/api/v2", "127.0.0.1:9002")?,
        ])?)
    }

    fn req<'a>(method: &'a str, host: &'a str, target: &'a str) -> RequestHead<'a> {
        RequestHead {
            method,
            host,
            target,
        }
    }

    #[test]
    fn longest_prefix_wins_per_segment() -> TestResult {
        let t = table().map_err(|e| crate::test_support::TestError(e.to_string()))?;
        let id = |h: &str, p: &str| match t.decide(&req("GET", h, p)) {
            Decision::Forward(f) => f.route_id,
            Decision::Refuse(r) => format!("{r:?}"),
        };
        ensure(
            id("hub.example.org", "/api/v2/x") == "deep",
            "deepest prefix",
        )?;
        ensure(id("hub.example.org", "/api/x") == "api", "api prefix")?;
        ensure(
            id("hub.example.org", "/apix") == "root",
            "prefix is per segment",
        )?;
        ensure(id("hub.example.org", "/other") == "root", "root route")
    }

    #[test]
    fn unknown_host_and_missing_route_are_distinct() -> TestResult {
        let t = RouteTable::new(vec![route("a", "a.example.org", "/x", "127.0.0.1:1")?])?;
        ensure(
            t.decide(&req("GET", "b.example.org", "/x")) == Decision::Refuse(Refusal::UnknownHost),
            "unknown host is 421",
        )?;
        ensure(
            t.decide(&req("GET", "a.example.org", "/y")) == Decision::Refuse(Refusal::NoRoute),
            "known host, no path route is 404",
        )?;
        ensure(
            Refusal::UnknownHost.status() == 421 && Refusal::NoRoute.status() == 404,
            "statuses",
        )
    }

    #[test]
    fn host_header_variants_match_the_same_route() -> TestResult {
        let t = table().map_err(|e| crate::test_support::TestError(e.to_string()))?;
        for host in [
            "hub.example.org",
            "HUB.Example.org",
            "hub.example.org.",
            "hub.example.org:443",
        ] {
            ensure(
                matches!(t.decide(&req("GET", host, "/api")), Decision::Forward(_)),
                host,
            )?;
        }
        Ok(())
    }

    #[test]
    fn smuggled_hosts_and_traversal_are_refused() -> TestResult {
        let t = table().map_err(|e| crate::test_support::TestError(e.to_string()))?;
        ensure(
            t.decide(&req("GET", "hub.example.org, evil.example.org", "/api"))
                == Decision::Refuse(Refusal::BadHost),
            "host list",
        )?;
        // A harmless `..` that stays inside the root is resolved, not refused,
        // and then matched on the normalized path (`/admin` -> root route).
        ensure(
            matches!(
                t.decide(&req("GET", "hub.example.org", "/api/../admin")),
                Decision::Forward(f) if f.route_id == "root" && f.upstream_target == "/admin"
            ),
            "dot segment resolved before matching",
        )?;
        for target in ["/api/../../x", "/api/%2e%2e/x", "/api%2fx", "/a\\b"] {
            ensure(
                t.decide(&req("GET", "hub.example.org", target))
                    == Decision::Refuse(Refusal::BadPath),
                target,
            )?;
        }
        Ok(())
    }

    #[test]
    fn traversal_is_resolved_before_matching() -> TestResult {
        let t = table().map_err(|e| crate::test_support::TestError(e.to_string()))?;
        // `/api/x/../../other` normalizes to `/other` and must hit the root
        // route, not the `/api` route its raw prefix suggests.
        match t.decide(&req("GET", "hub.example.org", "/api/x/../../other")) {
            Decision::Forward(f) => {
                ensure(f.route_id == "root", "matched on the normalized path")?;
                ensure(
                    f.upstream_target == "/other",
                    "upstream sees the normalized path",
                )
            }
            Decision::Refuse(r) => Err(crate::test_support::TestError(format!("{r:?}"))),
        }
    }

    #[test]
    fn methods_are_enforced() -> TestResult {
        let t = table().map_err(|e| crate::test_support::TestError(e.to_string()))?;
        ensure(
            t.decide(&req("DELETE", "hub.example.org", "/api"))
                == Decision::Refuse(Refusal::MethodNotAllowed),
            "method not listed",
        )?;
        ensure(
            t.decide(&req("get", "hub.example.org", "/api"))
                == Decision::Refuse(Refusal::MethodNotAllowed),
            "methods are case sensitive",
        )
    }

    #[test]
    fn strip_prefix_and_query() -> TestResult {
        let mut r = route("a", "a.example.org", "/svc", "127.0.0.1:1")?;
        r.strip_prefix = true;
        let t = RouteTable::new(vec![r])?;
        match t.decide(&req("GET", "a.example.org:8443", "/svc/items/1?x=1")) {
            Decision::Forward(f) => {
                ensure(
                    f.upstream_target == "/items/1?x=1",
                    "prefix stripped, query kept",
                )?;
                ensure(
                    f.original_host == "a.example.org:8443",
                    "original host with port",
                )?;
                ensure(f.max_body_bytes == 1024, "body limit")
            }
            Decision::Refuse(r) => Err(crate::test_support::TestError(format!("{r:?}"))),
        }?;
        ensure(
            matches!(t.decide(&req("GET", "a.example.org", "/svc")), Decision::Forward(f) if f.upstream_target == "/"),
            "bare prefix maps to the root",
        )
    }

    #[test]
    fn upstream_classes_are_enforced() -> TestResult {
        let cfg = |addr: &str, scope: UpstreamScope| -> Result<RouteTable, ConfigError> {
            let mut r = route("a", "a.example.org", "/", addr)
                .map_err(|_| ConfigError::BadId("addr".into()))?;
            r.scope = scope;
            RouteTable::new(vec![r])
        };
        // Always refused, whatever the scope.
        for addr in [
            "169.254.169.254:80",
            "169.254.1.1:80",
            "0.0.0.0:80",
            "224.0.0.1:80",
            "192.0.2.1:80",
            "[::ffff:169.254.169.254]:80",
        ] {
            ensure(cfg(addr, UpstreamScope::Public).is_err(), addr)?;
        }
        // Private classes need the private scope.
        for addr in [
            "10.0.0.5:80",
            "192.168.1.1:80",
            "100.64.0.1:80",
            "[fd00::1]:80",
            "[::ffff:10.0.0.1]:80",
        ] {
            ensure(cfg(addr, UpstreamScope::Loopback).is_err(), addr)?;
            ensure(cfg(addr, UpstreamScope::Private).is_ok(), addr)?;
        }
        // IPv4-mapped loopback is loopback.
        ensure(
            cfg("[::ffff:127.0.0.1]:80", UpstreamScope::Loopback).is_ok(),
            "mapped loopback",
        )?;
        // Public needs the public scope.
        ensure(
            cfg("8.8.8.8:80", UpstreamScope::Private).is_err(),
            "public under private scope",
        )?;
        ensure(
            cfg("8.8.8.8:80", UpstreamScope::Public).is_ok(),
            "public under public scope",
        )?;
        ensure(
            cfg("127.0.0.1:0", UpstreamScope::Loopback).is_err(),
            "port 0",
        )
    }

    #[test]
    fn invalid_tables_are_rejected() -> TestResult {
        let ok = || route("a", "a.example.org", "/x", "127.0.0.1:1");
        let bad = |mutate: &dyn Fn(&mut Route)| -> TestResult {
            let mut r = ok()?;
            mutate(&mut r);
            ensure(RouteTable::new(vec![r]).is_err(), "must be rejected")
        };
        bad(&|r| r.id = String::new())?;
        bad(&|r| r.host = "A.example.org".into())?;
        bad(&|r| r.host = "a.example.org:80".into())?;
        bad(&|r| r.path_prefix = "x".into())?;
        bad(&|r| r.path_prefix = "/x/".into())?;
        bad(&|r| r.path_prefix = "/x/../y".into())?;
        bad(&|r| r.path_prefix = "/x?q=1".into())?;
        bad(&|r| r.methods = vec![])?;
        bad(&|r| r.methods = vec!["CONNECT".into()])?;
        bad(&|r| r.methods = vec!["TRACE".into()])?;
        bad(&|r| r.methods = vec!["get".into()])?;
        bad(&|r| r.max_body_bytes = 0)?;
        let dup = vec![ok()?, {
            let mut r = ok()?;
            r.id = "b".into();
            r
        }];
        ensure(
            RouteTable::new(dup) == Err(ConfigError::Duplicate("b".into())),
            "duplicate host+prefix",
        )?;
        let same_id = vec![ok()?, route("a", "b.example.org", "/", "127.0.0.1:1")?];
        ensure(RouteTable::new(same_id).is_err(), "duplicate id")
    }
}
