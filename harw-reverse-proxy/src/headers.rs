//! Header hygiene: drop hop-by-hop and spoofable headers, set forwarding headers.

use std::net::IpAddr;

use crate::route::Refusal;

/// Scheme the client used to reach the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    /// Plain HTTP.
    Http,
    /// HTTPS.
    Https,
}

impl Proto {
    fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

/// The client as the proxy saw it (never taken from headers).
#[derive(Debug, Clone, Copy)]
pub struct ClientInfo {
    /// Peer address of the client connection.
    pub addr: IpAddr,
    /// Scheme of the client connection.
    pub proto: Proto,
}

/// Header rules for a route.
#[derive(Debug, Clone, Default)]
pub struct HeaderPolicy {
    /// Extra header names (any case) removed from client requests, in addition
    /// to the always-dropped `x-harw-*` prefix and the owned forwarding headers.
    pub strip: Vec<String>,
    /// Headers the proxy sets itself, after stripping. Names and values are
    /// validated like client headers. **Not for identity:** the proxy sets no
    /// plaintext principal, tenant or tier header (masterplan section 14), and a
    /// name with the `x-harw-` prefix is refused here (`BadHeader`).
    pub set: Vec<(String, String)>,
    /// Allow the `Upgrade` mechanism (WebSocket). Off by default.
    pub allow_upgrade: bool,
}

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Prefix of Harw-internal headers (identity, tenant, tier, ...). A client
/// never supplies these: every header with this prefix is dropped from client
/// requests, whatever its exact name (crypto masterplan section 14).
const INTERNAL_PREFIX: &str = "x-harw-";

/// Headers the proxy owns: replaced, never passed through or appended to.
const OWNED: &[&str] = &[
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-forwarded-port",
    "x-real-ip",
];

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn valid_value(v: &str) -> bool {
    !v.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

/// Sanitize the request headers for the upstream.
///
/// Refuses (`BadHeader`) a malformed name or a value with CR, LF or NUL. Drops
/// hop-by-hop headers, every header named in a `Connection` header, `Host`
/// (the adapter sets the upstream `Host` itself), the owned forwarding headers
/// and `policy.strip`; then appends the forwarding headers and `policy.set`.
/// An upgrade (`websocket` only) is forwarded only when `policy.allow_upgrade`
/// is set and the client asked for it; the proxy then emits its own
/// `Connection: Upgrade`. `Transfer-Encoding` is dropped (framing is the
/// adapter's); a message with both `Transfer-Encoding` and `Content-Length`, or
/// with repeated or non-numeric `Content-Length`, is refused.
pub fn sanitize_headers(
    headers: &[(String, String)],
    original_host: &str,
    client: ClientInfo,
    policy: &HeaderPolicy,
) -> Result<Vec<(String, String)>, Refusal> {
    for (n, v) in headers.iter().chain(policy.set.iter()) {
        if !valid_name(n) || !valid_value(v) {
            return Err(Refusal::BadHeader);
        }
    }
    if policy
        .set
        .iter()
        .any(|(n, _)| n.to_ascii_lowercase().starts_with(INTERNAL_PREFIX))
    {
        return Err(Refusal::BadHeader);
    }
    let lower = |s: &str| s.to_ascii_lowercase();
    // Names listed in `Connection` are hop-by-hop for this hop.
    let named_in_connection: Vec<String> = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("connection"))
        .flat_map(|(_, v)| v.split(','))
        .map(|t| lower(t.trim()))
        .filter(|t| !t.is_empty())
        .collect();
    let strip: Vec<String> = policy.strip.iter().map(|s| lower(s)).collect();

    // Request framing (RFC 9110 7.6.1, RFC 9112 6.3): a message with both
    // `Transfer-Encoding` and `Content-Length`, repeated or non-numeric
    // `Content-Length` values are ambiguous (smuggling) and refused. A lone
    // `Transfer-Encoding` is dropped below: outbound framing belongs to the HTTP
    // adapter, which re-frames the body it decoded.
    let has_te = headers
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("transfer-encoding"));
    let lengths: Vec<&str> = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| v.trim())
        .collect();
    let bad_length = lengths.len() > 1
        || lengths
            .iter()
            .any(|v| v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()));
    if (has_te && !lengths.is_empty()) || bad_length {
        return Err(Refusal::BadHeader);
    }
    // An upgrade is only forwarded when the client asked for one (`Connection`
    // names `upgrade` and an `Upgrade` header exists) and the route allows it.
    // The proxy then emits its own `Connection: Upgrade`; the client's
    // `Connection` value is never forwarded, so the client cannot nominate
    // headers the proxy adds as hop-by-hop. Only `websocket` is accepted
    // (`h2c` and other protocols would bypass the proxy's framing).
    let upgrade_values: Vec<&str> = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("upgrade"))
        .map(|(_, v)| v.trim())
        .collect();
    let asked_for_upgrade =
        named_in_connection.iter().any(|t| t == "upgrade") && !upgrade_values.is_empty();
    let forward_upgrade = policy.allow_upgrade && asked_for_upgrade;
    if forward_upgrade
        && !(upgrade_values.len() == 1 && upgrade_values[0].eq_ignore_ascii_case("websocket"))
    {
        return Err(Refusal::BadHeader);
    }

    let mut out: Vec<(String, String)> = Vec::with_capacity(headers.len() + 6);
    for (name, value) in headers {
        let n = lower(name);
        if n == "host"
            || HOP_BY_HOP.contains(&n.as_str())
            || named_in_connection.contains(&n)
            || OWNED.contains(&n.as_str())
            || n.starts_with(INTERNAL_PREFIX)
            || strip.contains(&n)
        {
            continue;
        }
        out.push((name.clone(), value.clone()));
    }
    if forward_upgrade {
        out.push(("connection".into(), "Upgrade".into()));
        out.push(("upgrade".into(), "websocket".into()));
    }
    out.push(("x-forwarded-for".into(), client.addr.to_string()));
    out.push(("x-forwarded-host".into(), original_host.to_owned()));
    out.push(("x-forwarded-proto".into(), client.proto.as_str().to_owned()));
    for (n, v) in &policy.set {
        // A proxy-set header replaces anything the client sent under that name.
        let ln = lower(n);
        out.retain(|(on, _)| lower(on) != ln);
        out.push((n.clone(), v.clone()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn client() -> ClientInfo {
        ClientInfo {
            addr: IpAddr::from([203, 0, 113, 9]),
            proto: Proto::Https,
        }
    }

    fn get<'a>(out: &'a [(String, String)], name: &str) -> Vec<&'a str> {
        out.iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    #[test]
    fn hop_by_hop_and_connection_listed_headers_are_dropped() -> TestResult {
        let input = h(&[
            ("Connection", "keep-alive, X-Secret"),
            ("Keep-Alive", "timeout=5"),
            ("X-Secret", "1"),
            ("Proxy-Authorization", "Basic x"),
            ("TE", "trailers"),
            ("Upgrade", "websocket"),
            ("Accept", "*/*"),
        ]);
        let out = sanitize_headers(&input, "a.example.org", client(), &HeaderPolicy::default())?;
        for gone in [
            "connection",
            "keep-alive",
            "x-secret",
            "proxy-authorization",
            "te",
            "upgrade",
        ] {
            ensure(get(&out, gone).is_empty(), gone)?;
        }
        ensure(get(&out, "accept") == ["*/*"], "ordinary header kept")
    }

    #[test]
    fn spoofed_forwarding_headers_are_replaced_not_appended() -> TestResult {
        let input = h(&[
            ("X-Forwarded-For", "6.6.6.6"),
            ("Forwarded", "for=6.6.6.6"),
            ("X-Real-IP", "6.6.6.6"),
            ("X-Forwarded-Proto", "http"),
            ("X-Forwarded-Host", "evil.example.org"),
        ]);
        let out = sanitize_headers(
            &input,
            "hub.example.org",
            client(),
            &HeaderPolicy::default(),
        )?;
        ensure(
            get(&out, "x-forwarded-for") == ["203.0.113.9"],
            "xff is the real peer only",
        )?;
        ensure(
            get(&out, "x-forwarded-host") == ["hub.example.org"],
            "host from the decision",
        )?;
        ensure(
            get(&out, "x-forwarded-proto") == ["https"],
            "proto from the connection",
        )?;
        ensure(
            get(&out, "forwarded").is_empty() && get(&out, "x-real-ip").is_empty(),
            "owned headers removed",
        )
    }

    #[test]
    fn host_is_dropped_and_configured_headers_are_stripped() -> TestResult {
        let policy = HeaderPolicy {
            strip: vec!["X-Internal-Note".into()],
            set: vec![("x-request-class".into(), "edge".into())],
            allow_upgrade: false,
        };
        let input = h(&[
            ("Host", "hub.example.org"),
            ("X-Internal-Note", "secret"),
            ("X-Request-Class", "client-chosen"),
        ]);
        let out = sanitize_headers(&input, "hub.example.org", client(), &policy)?;
        ensure(get(&out, "host").is_empty(), "host removed")?;
        ensure(
            get(&out, "x-internal-note").is_empty(),
            "configured strip applies",
        )?;
        ensure(
            get(&out, "x-request-class") == ["edge"],
            "the proxy value replaces the client value",
        )
    }

    #[test]
    fn every_x_harw_header_is_dropped_from_client_requests() -> TestResult {
        // The masterplan names these three; the prefix rule also covers names
        // added later, in any case.
        let input = h(&[
            ("x-harw-principal", "owner"),
            ("X-Harw-Tenant", "other"),
            ("X-HARW-TIER", "Owner"),
            ("x-harw-future-header", "1"),
            ("X-Harwish", "kept"),
            ("Accept", "*/*"),
        ]);
        let out = sanitize_headers(&input, "a.example.org", client(), &HeaderPolicy::default())?;
        for gone in [
            "x-harw-principal",
            "x-harw-tenant",
            "x-harw-tier",
            "x-harw-future-header",
        ] {
            ensure(get(&out, gone).is_empty(), gone)?;
        }
        ensure(
            get(&out, "x-harwish") == ["kept"],
            "the prefix is exact, not a substring match",
        )?;
        ensure(get(&out, "accept") == ["*/*"], "ordinary header kept")
    }

    #[test]
    fn the_proxy_cannot_be_configured_to_set_an_internal_header() -> TestResult {
        let policy = HeaderPolicy {
            set: vec![("X-Harw-Principal".into(), "owner".into())],
            ..HeaderPolicy::default()
        };
        ensure(
            sanitize_headers(&[], "a.example.org", client(), &policy) == Err(Refusal::BadHeader),
            "identity headers are not proxy-settable",
        )
    }

    #[test]
    fn upgrade_passes_only_when_allowed_and_requested() -> TestResult {
        let input = h(&[("Connection", "Upgrade"), ("Upgrade", "WebSocket")]);
        let allowed = HeaderPolicy {
            allow_upgrade: true,
            ..HeaderPolicy::default()
        };
        let out = sanitize_headers(&input, "a.example.org", client(), &allowed)?;
        ensure(
            get(&out, "upgrade") == ["websocket"] && get(&out, "connection") == ["Upgrade"],
            "rebuilt for a websocket route",
        )?;
        let off = sanitize_headers(&input, "a.example.org", client(), &HeaderPolicy::default())?;
        ensure(
            get(&off, "upgrade").is_empty() && get(&off, "connection").is_empty(),
            "dropped by default",
        )?;
        let no_request = h(&[("Upgrade", "websocket")]);
        let out = sanitize_headers(&no_request, "a.example.org", client(), &allowed)?;
        ensure(
            get(&out, "upgrade").is_empty() && get(&out, "connection").is_empty(),
            "no Connection: upgrade, nothing forwarded",
        )
    }

    #[test]
    fn the_client_connection_value_is_never_forwarded() -> TestResult {
        // The client nominates headers the proxy adds as hop-by-hop; the proxy
        // must emit only its own `Connection: Upgrade`.
        let input = h(&[
            ("Connection", "Upgrade, X-Request-Class, X-Forwarded-For"),
            ("Upgrade", "websocket"),
        ]);
        let policy = HeaderPolicy {
            set: vec![("x-request-class".into(), "edge".into())],
            allow_upgrade: true,
            ..HeaderPolicy::default()
        };
        let out = sanitize_headers(&input, "a.example.org", client(), &policy)?;
        ensure(
            get(&out, "connection") == ["Upgrade"],
            "exactly the proxy's own Connection value",
        )?;
        ensure(
            get(&out, "x-request-class") == ["edge"],
            "proxy-set header present",
        )?;
        ensure(
            get(&out, "x-forwarded-for") == ["203.0.113.9"],
            "forwarding header present",
        )
    }

    #[test]
    fn only_websocket_upgrades_are_forwarded() -> TestResult {
        let allowed = HeaderPolicy {
            allow_upgrade: true,
            ..HeaderPolicy::default()
        };
        for proto in ["h2c", "websocket, h2c", "TLS/1.0"] {
            let input = h(&[("Connection", "Upgrade"), ("Upgrade", proto)]);
            ensure(
                sanitize_headers(&input, "a.example.org", client(), &allowed)
                    == Err(Refusal::BadHeader),
                proto,
            )?;
            // On a route without upgrades the request is simply stripped.
            ensure(
                sanitize_headers(&input, "a.example.org", client(), &HeaderPolicy::default())
                    .is_ok(),
                "dropped, not refused, when upgrades are off",
            )?;
        }
        Ok(())
    }

    #[test]
    fn transfer_encoding_is_dropped_and_ambiguous_framing_is_refused() -> TestResult {
        let policy = HeaderPolicy::default();
        let lone = h(&[("TrAnSfEr-EnCoDiNg", "chunked"), ("Accept", "*/*")]);
        let out = sanitize_headers(&lone, "a.example.org", client(), &policy)?;
        ensure(
            get(&out, "transfer-encoding").is_empty(),
            "inbound transfer coding removed",
        )?;
        ensure(get(&out, "accept") == ["*/*"], "ordinary header kept")?;
        let both = h(&[("Transfer-Encoding", "chunked"), ("Content-Length", "5")]);
        ensure(
            sanitize_headers(&both, "a.example.org", client(), &policy) == Err(Refusal::BadHeader),
            "TE with CL",
        )?;
        for bad in [
            h(&[("Content-Length", "5"), ("content-length", "5")]),
            h(&[("Content-Length", "5"), ("Content-Length", "6")]),
            h(&[("Content-Length", "-1")]),
            h(&[("Content-Length", "")]),
            h(&[("Content-Length", "1e3")]),
        ] {
            ensure(
                sanitize_headers(&bad, "a.example.org", client(), &policy)
                    == Err(Refusal::BadHeader),
                "bad content-length",
            )?;
        }
        let ok = h(&[("Content-Length", "12")]);
        let out = sanitize_headers(&ok, "a.example.org", client(), &policy)?;
        ensure(
            get(&out, "content-length") == ["12"],
            "a single numeric length is kept for the adapter to check",
        )
    }

    #[test]
    fn malformed_headers_are_refused() -> TestResult {
        let policy = HeaderPolicy::default();
        for bad in [
            h(&[("X-A", "a\r\nX-B: b")]),
            h(&[("X-A", "a\nb")]),
            h(&[("X-A", "a\0b")]),
            h(&[("X A", "a")]),
            h(&[("", "a")]),
            h(&[("X-A:", "a")]),
        ] {
            ensure(
                sanitize_headers(&bad, "a.example.org", client(), &policy)
                    == Err(Refusal::BadHeader),
                "must be refused",
            )?;
        }
        let bad_set = HeaderPolicy {
            set: vec![("X-A".into(), "a\r\nb".into())],
            ..HeaderPolicy::default()
        };
        ensure(
            sanitize_headers(&[], "a.example.org", client(), &bad_set) == Err(Refusal::BadHeader),
            "proxy-set headers are validated too",
        )
    }
}
