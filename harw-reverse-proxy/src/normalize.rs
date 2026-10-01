//! Host and path normalization. Matching and forwarding use the normalized form.

use std::net::IpAddr;

/// A validated `Host` header value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHost {
    /// Lowercase host without port, brackets removed for IPv6 literals and one
    /// trailing dot removed.
    pub host: String,
    /// Explicit port, if present.
    pub port: Option<u16>,
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_'
}

/// Validate and normalize a raw `Host` header value (a single value).
///
/// Rejects empty values, lists (`,`), whitespace, control characters, userinfo
/// (`@`), path or query characters, bare IPv6 without brackets and invalid
/// ports.
pub fn normalize_host(raw: &str) -> Option<NormalizedHost> {
    if raw.is_empty()
        || raw.chars().any(|c| {
            c.is_control() || c.is_whitespace() || matches!(c, ',' | '@' | '/' | '\\' | '?' | '#')
        })
    {
        return None;
    }
    let (host_part, port_part) = if let Some(rest) = raw.strip_prefix('[') {
        let (inside, after) = rest.split_once(']')?;
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':')?),
        };
        (inside.to_owned(), port)
    } else {
        match raw.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h.to_owned(), Some(p)),
            Some(_) => return None,
            None => (raw.to_owned(), None),
        }
    };
    let port = match port_part {
        None => None,
        Some(p) => {
            if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let n: u16 = p.parse().ok()?;
            if n == 0 {
                return None;
            }
            Some(n)
        }
    };
    let host = host_part.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host).to_owned();
    if host.is_empty() {
        return None;
    }
    // An IP literal is stored in its canonical text form, so `[0:0:0:0:0:0:0:1]`
    // and `[::1]` are the same host.
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(NormalizedHost {
            host: ip.to_string(),
            port,
        });
    }
    let valid = host.chars().all(is_name_char) && !host.starts_with('.') && !host.contains("..");
    valid.then_some(NormalizedHost { host, port })
}

/// Normalize a host from **configuration** (no port). Accepts the wire form
/// (`[::1]`, `app.example.org`) and, for convenience, a bare IPv6 literal
/// (`::1`), which is not valid in a `Host` header. Returns the normalized host
/// without brackets, the form stored in routes and compared against requests.
pub fn normalize_config_host(raw: &str) -> Option<String> {
    if let Ok(ip) = raw.trim().parse::<IpAddr>() {
        return Some(ip.to_string());
    }
    normalize_host(raw)
        .filter(|h| h.port.is_none())
        .map(|h| h.host)
}

/// `host` as it appears in a `Host` header or `X-Forwarded-Host`: IPv6 literals
/// get their brackets back, an explicit port is appended.
pub fn format_authority(host: &str, port: Option<u16>) -> String {
    let h = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    match port {
        Some(p) => format!("{h}:{p}"),
        None => h,
    }
}

/// Percent escapes that would hide a dot segment or a path separator.
fn has_hidden_separator(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let (Some(h), Some(l)) = (bytes.get(i + 1), bytes.get(i + 2)) else {
                return true; // truncated escape
            };
            if !h.is_ascii_hexdigit() || !l.is_ascii_hexdigit() {
                return true; // invalid escape
            }
            let hex = [h.to_ascii_lowercase(), l.to_ascii_lowercase()];
            if matches!(&hex, b"2e" | b"2f" | b"5c" | b"00") {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Normalize a request target (`path[?query]`): the path must be absolute,
/// free of control characters, backslashes, NUL and hidden separators
/// (`%2e`, `%2f`, `%5c`, `%00`); dot segments are resolved (RFC 3986 5.2.4) and
/// an escape above the root is refused. Empty segments (`//`) are collapsed.
/// Returns the normalized path and the untouched query.
pub fn normalize_path(target: &str) -> Option<(String, Option<String>)> {
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q.to_owned())),
        None => (target, None),
    };
    if !path.starts_with('/')
        || path
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == '#')
        || has_hidden_separator(path)
    {
        return None;
    }
    if let Some(q) = &query {
        if q.chars().any(|c| c.is_control() || c == '#') {
            return None;
        }
    }
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            s => stack.push(s),
        }
    }
    let mut out = String::from("/");
    out.push_str(&stack.join("/"));
    if path.ends_with('/') && out.len() > 1 {
        out.push('/');
    }
    Some((out, query))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn host_is_normalized() -> TestResult {
        let h = normalize_host("App.Example.org.:8443");
        ensure(
            h == Some(NormalizedHost {
                host: "app.example.org".into(),
                port: Some(8443),
            }),
            "case, trailing dot, port",
        )?;
        ensure(
            normalize_host("[::1]:80").map(|h| h.host) == Some("::1".into()),
            "ipv6 literal",
        )?;
        ensure(normalize_host("127.0.0.1").is_some(), "ipv4 literal")
    }

    #[test]
    fn ip_literals_are_canonical_and_authority_restores_brackets() -> TestResult {
        ensure(
            normalize_host("[0:0:0:0:0:0:0:1]:8443")
                == Some(NormalizedHost {
                    host: "::1".into(),
                    port: Some(8443),
                }),
            "v6 canonical",
        )?;
        // Both spellings of the IPv4-mapped loopback normalize to one host.
        let dotted = normalize_host("[::FFFF:127.0.0.1]").map(|h| h.host);
        let hex = normalize_host("[::ffff:7f00:1]").map(|h| h.host);
        ensure(
            dotted.is_some() && dotted == hex,
            "mapped spellings are one canonical host",
        )?;
        ensure(
            normalize_config_host("[::1]") == Some("::1".into()),
            "bracketed config",
        )?;
        ensure(
            normalize_config_host("::1") == Some("::1".into()),
            "bare v6 accepted in config only",
        )?;
        ensure(
            normalize_config_host("[::1]:80").is_none()
                && normalize_config_host("a.example.org:80").is_none(),
            "no port in config",
        )?;
        ensure(
            normalize_config_host("A.Example.org.") == Some("a.example.org".into()),
            "name normalized",
        )?;
        ensure(
            format_authority("::1", Some(8443)) == "[::1]:8443",
            "v6 with port",
        )?;
        ensure(format_authority("::1", None) == "[::1]", "v6 without port")?;
        ensure(
            format_authority("a.example.org", Some(80)) == "a.example.org:80",
            "name with port",
        )?;
        ensure(format_authority("127.0.0.1", None) == "127.0.0.1", "v4")
    }

    #[test]
    fn ambiguous_hosts_are_refused() -> TestResult {
        for bad in [
            "",
            "a.example.org, b.example.org",
            "user@a.example.org",
            "a b",
            "a.example.org/x",
            "a.example.org?x",
            "::1",
            "a.example.org:",
            "a.example.org:0",
            "a.example.org:99999",
            "a.example.org:80x",
            ".example.org",
            "a..example.org",
            "[::1",
            "[::1]x",
            "a\\b",
            "a.example.org\n",
        ] {
            ensure(normalize_host(bad).is_none(), bad)?;
        }
        Ok(())
    }

    #[test]
    fn path_dot_segments_are_resolved() -> TestResult {
        let p = |t: &str| normalize_path(t);
        ensure(
            p("/a/./b/../c") == Some(("/a/c".into(), None)),
            "dot segments",
        )?;
        ensure(p("/a//b") == Some(("/a/b".into(), None)), "empty segments")?;
        ensure(
            p("/a/b/") == Some(("/a/b/".into(), None)),
            "trailing slash kept",
        )?;
        ensure(p("/") == Some(("/".into(), None)), "root")?;
        ensure(
            p("/x?q=1&r=2") == Some(("/x".into(), Some("q=1&r=2".into()))),
            "query untouched",
        )
    }

    #[test]
    fn traversal_and_hidden_separators_are_refused() -> TestResult {
        for bad in [
            "/../etc/passwd",
            "/a/../../b",
            "/%2e%2e/x",
            "/%2E%2e/x",
            "/a%2fb",
            "/a%5Cb",
            "/a\\b",
            "/a%00b",
            "/a%zz",
            "/a%2",
            "relative",
            "",
            "/a\nb",
            "/a#frag",
        ] {
            ensure(normalize_path(bad).is_none(), bad)?;
        }
        ensure(
            normalize_path("/ok?x=\r\n").is_none(),
            "control character in query",
        )
    }

    #[test]
    fn ordinary_escapes_survive() -> TestResult {
        ensure(
            normalize_path("/a%20b/%7Euser") == Some(("/a%20b/%7Euser".into(), None)),
            "benign percent escapes are kept as sent",
        )
    }
}
