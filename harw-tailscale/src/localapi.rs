//! Minimal client for `tailscaled`'s LocalAPI.
//!
//! # Description
//! `tailscaled` serves a local HTTP API on a Unix socket. harw only reads
//! from it (`/localapi/v0/status`, `/localapi/v0/whois`), which `tailscaled`
//! permits to unprivileged users. Requests are plain HTTP/1.0 with
//! `Connection: close`, so the response ends at EOF; chunked bodies are
//! decoded anyway. Parsing is split into pure functions so it is tested
//! without a running `tailscaled`.

use serde::Deserialize;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;

/// Where `tailscaled` usually puts its socket on Linux.
pub const SOCKET_CANDIDATES: [&str; 2] = [
    "/var/run/tailscale/tailscaled.sock",
    "/run/tailscale/tailscaled.sock",
];

/// Environment variable that overrides the socket path.
pub const SOCKET_ENV: &str = "HARW_TAILSCALE_SOCKET";

/// Upper bound for one LocalAPI response (4 MiB).
const MAX_RESPONSE_BYTES: u64 = 4 << 20;

/// Time budget for one LocalAPI request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Finds the `tailscaled` socket: [`SOCKET_ENV`] if set, else the first
/// existing entry of [`SOCKET_CANDIDATES`].
#[must_use]
pub fn find_socket() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV) {
        return Some(PathBuf::from(path));
    }
    SOCKET_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}

/// A LocalAPI failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalApiError {
    /// The socket could not be reached (not running, no permission).
    Unreachable(String),
    /// The request did not finish within the time budget.
    Timeout,
    /// The response is not valid HTTP or JSON.
    Malformed(String),
    /// `whois` knows no node for the address.
    NotFound,
    /// Any other HTTP status.
    Status(u16),
}

impl fmt::Display for LocalApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(detail) => write!(f, "tailscaled not reachable: {detail}"),
            Self::Timeout => write!(f, "tailscaled did not answer in time"),
            Self::Malformed(detail) => write!(f, "unreadable tailscaled answer: {detail}"),
            Self::NotFound => write!(f, "tailscaled knows no node for this address"),
            Self::Status(code) => write!(f, "tailscaled answered HTTP {code}"),
        }
    }
}

impl std::error::Error for LocalApiError {}

/// This node's view of the tailnet (`/localapi/v0/status`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// `Running`, `NeedsLogin`, `Stopped`, …
    pub backend_state: String,
    /// MagicDNS name of this node, without the trailing dot.
    pub dns_name: String,
    /// This node's tailnet addresses.
    pub ips: Vec<IpAddr>,
    /// Name of the tailnet, if reported.
    pub tailnet: Option<String>,
}

impl Status {
    /// Whether the node is connected (`BackendState == "Running"`).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.backend_state == "Running"
    }
}

/// Who is behind a tailnet address (`/localapi/v0/whois`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhoIs {
    /// Login of the node's owner, e.g. `mia@example.com`, or
    /// `tagged-devices` for tagged nodes.
    pub login: String,
    /// Display name of the owner.
    pub display_name: String,
    /// MagicDNS name of the node, without the trailing dot.
    pub node: String,
}

#[derive(Deserialize)]
struct RawStatus {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "Self")]
    self_node: Option<RawNode>,
    #[serde(rename = "CurrentTailnet")]
    current_tailnet: Option<RawTailnet>,
}

#[derive(Deserialize)]
struct RawNode {
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
}

#[derive(Deserialize)]
struct RawTailnet {
    #[serde(rename = "Name", default)]
    name: String,
}

#[derive(Deserialize)]
struct RawWhoIs {
    #[serde(rename = "Node")]
    node: RawWhoIsNode,
    #[serde(rename = "UserProfile")]
    user: RawUser,
}

#[derive(Deserialize)]
struct RawWhoIsNode {
    #[serde(rename = "Name", default)]
    name: String,
}

#[derive(Deserialize)]
struct RawUser {
    #[serde(rename = "LoginName", default)]
    login_name: String,
    #[serde(rename = "DisplayName", default)]
    display_name: String,
}

/// Parses a `/localapi/v0/status` body.
///
/// # Errors
/// [`LocalApiError::Malformed`] for invalid JSON; unparsable addresses are
/// skipped.
pub fn parse_status(body: &[u8]) -> Result<Status, LocalApiError> {
    let raw: RawStatus = serde_json::from_slice(body)
        .map_err(|error| LocalApiError::Malformed(error.to_string()))?;
    let (dns_name, ips) = raw.self_node.map_or((String::new(), Vec::new()), |node| {
        let ips = node
            .tailscale_ips
            .iter()
            .filter_map(|ip| ip.parse().ok())
            .collect();
        (node.dns_name.trim_end_matches('.').to_owned(), ips)
    });
    Ok(Status {
        backend_state: raw.backend_state,
        dns_name,
        ips,
        tailnet: raw
            .current_tailnet
            .map(|tailnet| tailnet.name)
            .filter(|name| !name.is_empty()),
    })
}

/// Parses a `/localapi/v0/whois` body.
///
/// # Errors
/// [`LocalApiError::Malformed`] for invalid JSON or a missing login.
pub fn parse_whois(body: &[u8]) -> Result<WhoIs, LocalApiError> {
    let raw: RawWhoIs = serde_json::from_slice(body)
        .map_err(|error| LocalApiError::Malformed(error.to_string()))?;
    if raw.user.login_name.trim().is_empty() {
        return Err(LocalApiError::Malformed("whois without login".to_owned()));
    }
    Ok(WhoIs {
        login: raw.user.login_name,
        display_name: raw.user.display_name,
        node: raw.node.name.trim_end_matches('.').to_owned(),
    })
}

/// Splits a raw HTTP response into status code and (de-chunked) body.
///
/// # Errors
/// [`LocalApiError::Malformed`] without a header terminator, a status line
/// or with a broken chunked body.
pub fn parse_http_response(raw: &[u8]) -> Result<(u16, Vec<u8>), LocalApiError> {
    let malformed = |detail: &str| LocalApiError::Malformed(detail.to_owned());
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| malformed("no header terminator"))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .filter(|_| status_line.starts_with("HTTP/"))
        .ok_or_else(|| malformed("no status line"))?;
    let chunked = lines.any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    let body = if chunked {
        dechunk(body)?
    } else {
        body.to_vec()
    };
    Ok((code, body))
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, LocalApiError> {
    let malformed = || LocalApiError::Malformed("broken chunked body".to_owned());
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(malformed)?;
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| malformed())?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            return Err(malformed());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

/// Percent-encodes a `host:port` pair for the `addr` query parameter.
fn encode_addr(addr: SocketAddr) -> String {
    addr.to_string()
        .replace('[', "%5B")
        .replace(']', "%5D")
        .replace(':', "%3A")
}

/// Client for one `tailscaled` socket.
#[derive(Debug, Clone)]
pub struct LocalApi {
    socket: PathBuf,
}

impl LocalApi {
    /// Client for `socket`.
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// Client for the socket [`find_socket`] finds.
    ///
    /// # Errors
    /// [`LocalApiError::Unreachable`] when no socket exists.
    pub fn discover() -> Result<Self, LocalApiError> {
        find_socket().map(Self::new).ok_or_else(|| {
            LocalApiError::Unreachable(format!(
                "no tailscaled socket at {} (set {SOCKET_ENV} to override)",
                SOCKET_CANDIDATES.join(" or ")
            ))
        })
    }

    /// The socket path.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// `GET /localapi/v0/status`.
    ///
    /// # Errors
    /// See [`LocalApiError`].
    pub async fn status(&self) -> Result<Status, LocalApiError> {
        let body = self.get("/localapi/v0/status").await?;
        parse_status(&body)
    }

    /// `GET /localapi/v0/whois?addr=<addr>`.
    ///
    /// # Errors
    /// [`LocalApiError::NotFound`] for an unknown address, otherwise see
    /// [`LocalApiError`].
    pub async fn whois(&self, addr: SocketAddr) -> Result<WhoIs, LocalApiError> {
        let body = self
            .get(&format!("/localapi/v0/whois?addr={}", encode_addr(addr)))
            .await?;
        parse_whois(&body)
    }

    async fn get(&self, path: &str) -> Result<Vec<u8>, LocalApiError> {
        let request = async {
            let mut stream = UnixStream::connect(&self.socket).await.map_err(|error| {
                LocalApiError::Unreachable(format!("{}: {error}", self.socket.display()))
            })?;
            let head = format!(
                "GET {path} HTTP/1.0\r\nHost: local-tailscaled.sock\r\nConnection: close\r\n\r\n"
            );
            stream
                .write_all(head.as_bytes())
                .await
                .map_err(|error| LocalApiError::Unreachable(error.to_string()))?;
            let mut raw = Vec::new();
            (&mut stream)
                .take(MAX_RESPONSE_BYTES + 1)
                .read_to_end(&mut raw)
                .await
                .map_err(|error| LocalApiError::Unreachable(error.to_string()))?;
            if raw.len() as u64 > MAX_RESPONSE_BYTES {
                return Err(LocalApiError::Malformed("response too large".to_owned()));
            }
            Ok(raw)
        };
        let raw = tokio::time::timeout(REQUEST_TIMEOUT, request)
            .await
            .map_err(|_| LocalApiError::Timeout)??;
        match parse_http_response(&raw)? {
            (200, body) => Ok(body),
            (404, _) => Err(LocalApiError::NotFound),
            (code, _) => Err(LocalApiError::Status(code)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"{"BackendState":"Running","Self":{"DNSName":"vps.tail1234.ts.net.","TailscaleIPs":["100.101.1.2","fd7a:115c:a1e0::1:2"]},"CurrentTailnet":{"Name":"mia.example"}}"#;
    const WHOIS: &str = r#"{"Node":{"Name":"s24.tail1234.ts.net.","Addresses":["100.101.9.9/32"]},"UserProfile":{"LoginName":"mia@example.com","DisplayName":"Mia"}}"#;

    #[test]
    fn status_is_parsed() -> Result<(), String> {
        let status = parse_status(STATUS.as_bytes()).map_err(|error| error.to_string())?;
        assert!(status.is_running());
        assert_eq!(status.dns_name, "vps.tail1234.ts.net");
        assert_eq!(status.ips.len(), 2);
        assert_eq!(status.tailnet.as_deref(), Some("mia.example"));
        Ok(())
    }

    #[test]
    fn whois_is_parsed_and_requires_a_login() -> Result<(), String> {
        let whois = parse_whois(WHOIS.as_bytes()).map_err(|error| error.to_string())?;
        assert_eq!(whois.login, "mia@example.com");
        assert_eq!(whois.node, "s24.tail1234.ts.net");
        let anonymous = r#"{"Node":{"Name":"x."},"UserProfile":{"LoginName":""}}"#;
        assert!(matches!(
            parse_whois(anonymous.as_bytes()),
            Err(LocalApiError::Malformed(_))
        ));
        Ok(())
    }

    #[test]
    fn http_responses_plain_and_chunked() -> Result<(), String> {
        let plain = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"a\":1}";
        let (code, body) = parse_http_response(plain).map_err(|error| error.to_string())?;
        assert_eq!((code, body.as_slice()), (200, &b"{\"a\":1}"[..]));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        let (_, body) = parse_http_response(chunked).map_err(|error| error.to_string())?;
        assert_eq!(body, b"abcde");
        let not_found = b"HTTP/1.0 404 Not Found\r\n\r\n";
        assert_eq!(
            parse_http_response(not_found).map(|(code, _)| code),
            Ok(404)
        );
        assert!(parse_http_response(b"garbage").is_err());
        Ok(())
    }

    #[test]
    fn whois_addr_is_percent_encoded() -> Result<(), String> {
        let v6: SocketAddr = "[fd7a:115c:a1e0::1]:443"
            .parse()
            .map_err(|error| format!("{error}"))?;
        assert_eq!(encode_addr(v6), "%5Bfd7a%3A115c%3Aa1e0%3A%3A1%5D%3A443");
        Ok(())
    }

    #[tokio::test]
    async fn client_talks_http_over_a_unix_socket() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let path = dir.path().join("tailscaled.sock");
        let listener = tokio::net::UnixListener::bind(&path).map_err(|error| error.to_string())?;
        let server = tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return String::new();
            };
            let mut buf = vec![0u8; 1024];
            let read = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..read]).into_owned();
            let response = format!("HTTP/1.0 200 OK\r\n\r\n{WHOIS}");
            let _ = stream.write_all(response.as_bytes()).await;
            request
        });
        let api = LocalApi::new(&path);
        let peer: SocketAddr = "100.101.9.9:51000"
            .parse()
            .map_err(|error| format!("{error}"))?;
        let whois = api.whois(peer).await.map_err(|error| error.to_string())?;
        assert_eq!(whois.login, "mia@example.com");
        let request = server.await.map_err(|error| error.to_string())?;
        assert!(
            request.starts_with("GET /localapi/v0/whois?addr=100.101.9.9%3A51000 HTTP/1.0\r\n"),
            "{request}"
        );
        assert!(request.contains("Host: local-tailscaled.sock"), "{request}");
        Ok(())
    }

    #[tokio::test]
    async fn missing_socket_is_unreachable() {
        let api = LocalApi::new("/nonexistent/tailscaled.sock");
        assert!(matches!(
            api.status().await,
            Err(LocalApiError::Unreachable(_))
        ));
    }
}
