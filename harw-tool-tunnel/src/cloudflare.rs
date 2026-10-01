//! Cloudflare-Tunnel (`cloudflared`) — reiner Policy-Kern, ohne Prozesse/I/O.
//!
//! Anders als ein SSH-`-L`-Forward (`policy`) macht `cloudflared` einen lokalen
//! Dienst aus dem Internet erreichbar. Das ist eine eigene Risikoklasse und
//! deshalb in `docs/design/tunnel-policy-v2-cloudflare.md` getrennt geregelt:
//! Ursprung nur Loopback, Port-Allowlist, Freigabe vor jedem Start und bei
//! jeder Änderung, Token nur als Dateireferenz (nie im Argumentvektor, nie in
//! Logs), erwartete öffentliche Hostnamen vorab festgelegt.

use std::fmt;
use std::net::IpAddr;

/// Warum eine Cloudflare-Spezifikation abgelehnt wurde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudflareError {
    /// Ursprung ist keine `http(s)://`-URL auf Loopback mit Port.
    OriginNotLoopback(String),
    /// Ursprungsport steht nicht in der Allowlist.
    PortNotAllowed(u16),
    /// Token-Referenz ist kein absoluter Pfad (sieht nach Tokeninhalt aus).
    TokenNotAFileRef,
    /// Benannter Tunnel ohne mindestens einen erwarteten öffentlichen Hostnamen.
    NoExpectedHostname,
    /// Hostname ungültig (leer, Leerraum, `-`-Präfix, Wildcard).
    InvalidHostname(String),
    /// Freigabe passt nicht zur wirksamen Konfiguration.
    ApprovalMismatch,
}

impl fmt::Display for CloudflareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OriginNotLoopback(o) => write!(f, "origin {o:?} is not a loopback http(s) url"),
            Self::PortNotAllowed(p) => write!(f, "origin port {p} is not allowed"),
            Self::TokenNotAFileRef => f.write_str("token must be an absolute file path reference"),
            Self::NoExpectedHostname => {
                f.write_str("named tunnel needs an expected public hostname")
            }
            Self::InvalidHostname(h) => write!(f, "invalid hostname {h:?}"),
            Self::ApprovalMismatch => f.write_str("approval does not match the effective config"),
        }
    }
}

impl std::error::Error for CloudflareError {}

/// Loopback-Ursprung des Forwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    https: bool,
    ip: IpAddr,
    port: u16,
}

impl Origin {
    /// Akzeptiert nur `http(s)://127.0.0.1:PORT` bzw. `[::1]:PORT`
    /// (ohne Pfad/Userinfo); `localhost` wird bewusst nicht aufgelöst.
    pub fn parse(raw: &str) -> Result<Self, CloudflareError> {
        let bad = || CloudflareError::OriginNotLoopback(raw.to_owned());
        let (https, rest) = if let Some(r) = raw.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = raw.strip_prefix("http://") {
            (false, r)
        } else {
            return Err(bad());
        };
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        let (host, port) = if let Some(r) = rest.strip_prefix('[') {
            let (h, p) = r.split_once("]:").ok_or_else(bad)?;
            (h, p)
        } else {
            rest.rsplit_once(':').ok_or_else(bad)?
        };
        if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
            return Err(bad());
        }
        let port: u16 = port.parse().map_err(|_| bad())?;
        let ip: IpAddr = host.parse().map_err(|_| bad())?;
        if !ip.is_loopback() || port == 0 {
            return Err(bad());
        }
        Ok(Self { https, ip, port })
    }

    /// Ursprungsport.
    pub fn port(&self) -> u16 {
        self.port
    }

    fn url(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        match self.ip {
            IpAddr::V4(a) => format!("{scheme}://{a}:{}", self.port),
            IpAddr::V6(a) => format!("{scheme}://[{a}]:{}", self.port),
        }
    }
}

/// Verweis auf eine Token-Datei. Der Tokeninhalt wird nie gehalten.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenFileRef(String);

impl TokenFileRef {
    /// Nur absolute Pfade; alles andere könnte der Token selbst sein.
    pub fn new(path: &str) -> Result<Self, CloudflareError> {
        let p = path.trim();
        if !p.starts_with('/') || p.chars().any(char::is_control) || p.starts_with("//") {
            return Err(CloudflareError::TokenNotAFileRef);
        }
        Ok(Self(p.to_owned()))
    }

    fn path(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TokenFileRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenFileRef(<redacted>)")
    }
}

/// Betriebsart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Schnelltunnel mit zufälliger `trycloudflare.com`-Adresse, ohne Konto.
    Quick,
    /// Benannter Tunnel mit Token-Datei; die öffentlichen Hostnamen sind in
    /// Cloudflare konfiguriert und hier als Erwartung festgehalten.
    Named {
        /// Token-Dateireferenz.
        token: TokenFileRef,
        /// Erwartete öffentliche Hostnamen (exakt, ohne Wildcard).
        hostnames: Vec<String>,
    },
}

/// Vollständige Beschreibung eines Cloudflare-Tunnels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudflareSpec {
    /// Lokaler Ursprung.
    pub origin: Origin,
    /// Betriebsart.
    pub mode: Mode,
}

fn valid_hostname(h: &str) -> bool {
    !h.is_empty()
        && !h.starts_with('-')
        && !h.contains('*')
        && h.contains('.')
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

impl CloudflareSpec {
    /// Prüfung gegen die Ursprungsport-Allowlist; für Start **und** Reconnect.
    pub fn check(&self, allowed_origin_ports: &[u16]) -> Result<(), CloudflareError> {
        if !allowed_origin_ports.contains(&self.origin.port()) {
            return Err(CloudflareError::PortNotAllowed(self.origin.port()));
        }
        if let Mode::Named { hostnames, .. } = &self.mode {
            if hostnames.is_empty() {
                return Err(CloudflareError::NoExpectedHostname);
            }
            for h in hostnames {
                if !valid_hostname(h) {
                    return Err(CloudflareError::InvalidHostname(h.clone()));
                }
            }
        }
        Ok(())
    }

    /// Argumentliste für `cloudflared` (ohne Programmnamen). Der Token geht
    /// ausschließlich als Dateireferenz mit, nie als Wert.
    pub fn args(&self) -> Vec<String> {
        let mut a = vec!["tunnel".to_owned(), "--no-autoupdate".to_owned()];
        match &self.mode {
            Mode::Quick => {
                a.push("--url".to_owned());
                a.push(self.origin.url());
            }
            Mode::Named { token, .. } => {
                a.push("run".to_owned());
                a.push("--token-file".to_owned());
                a.push(token.path().to_owned());
            }
        }
        a
    }

    /// Kanonische Beschreibung für die Freigabe. Der Tokenpfad fehlt bewusst;
    /// Hostnamen sind sortiert und kleingeschrieben.
    pub fn canonical(&self, allowed_origin_ports: &[u16]) -> String {
        let mut ports = allowed_origin_ports.to_vec();
        ports.sort_unstable();
        let mode = match &self.mode {
            Mode::Quick => "quick".to_owned(),
            Mode::Named { hostnames, .. } => {
                let mut h: Vec<String> = hostnames.iter().map(|x| x.to_ascii_lowercase()).collect();
                h.sort();
                format!("named[{}]", h.join(","))
            }
        };
        format!(
            "cf;origin={};mode={mode};ports={ports:?}",
            self.origin.url()
        )
    }
}

/// Freigabe für genau eine wirksame Cloudflare-Konfiguration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudflareApproval {
    canonical: String,
}

impl CloudflareApproval {
    /// Freigabe für die aktuelle Konfiguration.
    pub fn grant(spec: &CloudflareSpec, allowed_origin_ports: &[u16]) -> Self {
        Self {
            canonical: spec.canonical(allowed_origin_ports),
        }
    }

    /// Gilt nur bei unveränderter Konfiguration (Start und Reconnect).
    pub fn verify(
        &self,
        spec: &CloudflareSpec,
        allowed_origin_ports: &[u16],
    ) -> Result<(), CloudflareError> {
        if self.canonical == spec.canonical(allowed_origin_ports) {
            Ok(())
        } else {
            Err(CloudflareError::ApprovalMismatch)
        }
    }
}

/// Liest die öffentliche Adresse eines Schnelltunnels aus einer
/// `cloudflared`-Logzeile (`https://<name>.trycloudflare.com`).
pub fn parse_quick_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|' || c == '"')
        .unwrap_or(rest.len());
    let url = &rest[..end];
    let host = url.strip_prefix("https://")?;
    let label = host.strip_suffix(".trycloudflare.com")?;
    let ok = !label.is_empty()
        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !label.starts_with('-');
    ok.then(|| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick(origin: &str) -> CloudflareSpec {
        CloudflareSpec {
            origin: Origin::parse(origin).unwrap(),
            mode: Mode::Quick,
        }
    }

    fn named(hosts: &[&str]) -> CloudflareSpec {
        CloudflareSpec {
            origin: Origin::parse("http://127.0.0.1:3000").unwrap(),
            mode: Mode::Named {
                token: TokenFileRef::new("/run/secrets/cf-token").unwrap(),
                hostnames: hosts.iter().map(|s| (*s).to_owned()).collect(),
            },
        }
    }

    #[test]
    fn origin_must_be_loopback_http_with_port() {
        assert!(Origin::parse("http://127.0.0.1:3000").is_ok());
        assert!(Origin::parse("https://[::1]:8443/").is_ok());
        for bad in [
            "http://0.0.0.0:3000",
            "http://192.168.1.2:80",
            "http://example.org:80",
            "http://localhost:3000",
            "http://127.0.0.1",
            "http://127.0.0.1:0",
            "http://user@127.0.0.1:80",
            "http://127.0.0.1:80/path",
            "ftp://127.0.0.1:21",
            "127.0.0.1:3000",
            "http://127.0.0.1:99999",
        ] {
            assert!(Origin::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn token_must_be_absolute_path() {
        assert!(TokenFileRef::new("/run/secrets/t").is_ok());
        for bad in ["eyJhIjoiYWJjIn0=", "relative/path", "", "//x", "/a\nb"] {
            assert!(TokenFileRef::new(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            format!("{:?}", TokenFileRef::new("/t").unwrap()),
            "TokenFileRef(<redacted>)"
        );
    }

    #[test]
    fn port_allowlist_and_hostnames() {
        assert!(quick("http://127.0.0.1:3000").check(&[3000]).is_ok());
        assert_eq!(
            quick("http://127.0.0.1:3000").check(&[]),
            Err(CloudflareError::PortNotAllowed(3000))
        );
        assert!(named(&["app.example.org"]).check(&[3000]).is_ok());
        assert_eq!(
            named(&[]).check(&[3000]),
            Err(CloudflareError::NoExpectedHostname)
        );
        for bad in [
            "*.example.org",
            "-x.example.org",
            "a b.example.org",
            "nodot",
            "",
        ] {
            assert!(
                matches!(
                    named(&[bad]).check(&[3000]),
                    Err(CloudflareError::InvalidHostname(_))
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn args_never_contain_a_token_value() {
        assert_eq!(
            quick("http://127.0.0.1:3000").args(),
            [
                "tunnel",
                "--no-autoupdate",
                "--url",
                "http://127.0.0.1:3000"
            ]
        );
        let a = named(&["app.example.org"]).args();
        assert_eq!(
            a,
            [
                "tunnel",
                "--no-autoupdate",
                "run",
                "--token-file",
                "/run/secrets/cf-token"
            ]
        );
        assert!(!a.iter().any(|x| x == "--token"));
    }

    #[test]
    fn approval_binds_origin_mode_hostnames_and_ports() {
        let s = named(&["a.example.org", "b.example.org"]);
        let ap = CloudflareApproval::grant(&s, &[3000, 4000]);
        assert!(ap.verify(&s, &[4000, 3000]).is_ok());
        assert!(
            ap.verify(&named(&["B.example.org", "a.example.org"]), &[3000, 4000])
                .is_ok()
        );
        assert_eq!(
            ap.verify(&named(&["a.example.org"]), &[3000, 4000]),
            Err(CloudflareError::ApprovalMismatch)
        );
        assert_eq!(
            ap.verify(&s, &[3000]),
            Err(CloudflareError::ApprovalMismatch)
        );
        let moved = CloudflareSpec {
            origin: Origin::parse("http://127.0.0.1:4000").unwrap(),
            ..s.clone()
        };
        assert_eq!(
            ap.verify(&moved, &[3000, 4000]),
            Err(CloudflareError::ApprovalMismatch)
        );
        assert_eq!(
            ap.verify(&quick("http://127.0.0.1:3000"), &[3000, 4000]),
            Err(CloudflareError::ApprovalMismatch)
        );
        assert!(!s.canonical(&[3000]).contains("cf-token"));
    }

    #[test]
    fn parses_quick_tunnel_url_only_for_trycloudflare() {
        let line = "2026-10-01T20:00:00Z INF |  https://random-words-here.trycloudflare.com  |";
        assert_eq!(
            parse_quick_url(line).as_deref(),
            Some("https://random-words-here.trycloudflare.com")
        );
        assert_eq!(parse_quick_url("https://evil.example.org/x"), None);
        assert_eq!(
            parse_quick_url("https://x.trycloudflare.com.evil.org"),
            None
        );
        assert_eq!(parse_quick_url("no url here"), None);
    }
}
