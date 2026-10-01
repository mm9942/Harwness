//! Reiner Policy-Kern des Tunnel-Werkzeugs (`docs/design/tunnel-policy-v1.md`).
//!
//! Enthält nur Validierung und Konstruktion, keine Prozesse und kein I/O:
//! Loopback-Bindung (§3), Ziel-Allowlist mit Default-Verbot für private und
//! Loopback-Ziele (§2), kanonische Freigabe-Zeichenkette (§4), Keyfile-
//! **Referenz** statt Inhalt samt Redaktion (§7) und die Argumentliste für
//! `ssh -L` (§1/§8). Job-/WorkDriver-Anbindung und Reconnect folgen im
//! Lifecycle-Knoten; sie rufen diese Prüfungen bei Start **und** Reconnect auf.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Warum eine Tunnel-Spezifikation abgelehnt wurde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// Bind-Adresse ist nicht `127.0.0.1` oder `::1`.
    BindNotLoopback(String),
    /// Ein Feld ist leer, enthält Steuerzeichen/Leerraum oder beginnt mit `-`.
    InvalidField(&'static str),
    /// Port 0 ist kein gültiger Forward-Port.
    InvalidPort(&'static str),
    /// Ziel steht in keinem Allowlist-Eintrag (Host und Port müssen passen).
    TargetNotAllowed,
    /// Privates/Loopback-Ziel ohne ausdrückliche Erlaubnis im Eintrag.
    PrivateTargetNotPermitted,
    /// Die Keyfile-Referenz sieht nach Schlüsselinhalt aus.
    KeyMaterialNotAllowed,
    /// Keine gültige Freigabe für die wirksame Konfiguration.
    ApprovalMismatch,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BindNotLoopback(a) => write!(f, "bind address {a:?} is not loopback"),
            Self::InvalidField(n) => write!(f, "invalid value for {n}"),
            Self::InvalidPort(n) => write!(f, "invalid port for {n}"),
            Self::TargetNotAllowed => f.write_str("target is not in the allowlist"),
            Self::PrivateTargetNotPermitted => {
                f.write_str("private or loopback target is not explicitly permitted")
            }
            Self::KeyMaterialNotAllowed => {
                f.write_str("key reference looks like key material; pass a file path")
            }
            Self::ApprovalMismatch => f.write_str("approval does not match the effective config"),
        }
    }
}

impl std::error::Error for PolicyError {}

/// Lokale Bind-Adresse; ausschließlich Loopback ist darstellbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindAddr {
    /// `127.0.0.1`
    V4,
    /// `::1`
    V6,
}

impl BindAddr {
    /// Fehlende Adresse wird auf `127.0.0.1` festgelegt, nie breiter (§3).
    pub fn parse(raw: Option<&str>) -> Result<Self, PolicyError> {
        match raw.map(str::trim) {
            None | Some("") => Ok(Self::V4),
            Some(s) => match s.parse::<IpAddr>() {
                Ok(IpAddr::V4(a)) if a == Ipv4Addr::LOCALHOST => Ok(Self::V4),
                Ok(IpAddr::V6(a)) if a == Ipv6Addr::LOCALHOST => Ok(Self::V6),
                _ => Err(PolicyError::BindNotLoopback(s.to_owned())),
            },
        }
    }

    fn spec(self) -> &'static str {
        match self {
            Self::V4 => "127.0.0.1",
            Self::V6 => "[::1]",
        }
    }
}

/// Verweis auf eine Schlüsseldatei. Der Inhalt wird nie gehalten (§7).
#[derive(Clone, PartialEq, Eq)]
pub struct KeyRef(String);

impl KeyRef {
    /// Lehnt Werte ab, die wie Schlüsselinhalt aussehen oder keine Pfade sind.
    pub fn new(path: &str) -> Result<Self, PolicyError> {
        let p = path.trim();
        if p.is_empty() || p.starts_with('-') || p.chars().any(char::is_control) {
            return Err(PolicyError::KeyMaterialNotAllowed);
        }
        let upper = p.to_ascii_uppercase();
        if upper.contains("PRIVATE KEY") || upper.contains("BEGIN ") {
            return Err(PolicyError::KeyMaterialNotAllowed);
        }
        Ok(Self(p.to_owned()))
    }

    fn path(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for KeyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyRef(<redacted>)")
    }
}

/// Erlaubte Zielports eines Allowlist-Eintrags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ports {
    /// Genau ein Port.
    One(u16),
    /// Geschlossener Bereich `lo..=hi`.
    Range(u16, u16),
}

impl Ports {
    fn contains(&self, p: u16) -> bool {
        match *self {
            Self::One(a) => a == p,
            Self::Range(lo, hi) => lo <= p && p <= hi,
        }
    }
}

/// Ein Allowlist-Eintrag; Hostnamen sind exakt, ohne implizite Wildcards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRule {
    /// Vollständiger Hostname oder IP-Adresse.
    pub host: String,
    /// Erlaubte Zielports.
    pub ports: Ports,
    /// Ausdrückliche Erlaubnis für private/Loopback-Ziele (§2).
    pub allow_private: bool,
}

/// Wirksame Ziel-Allowlist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Allowlist {
    /// Einträge; leer erlaubt nichts.
    pub rules: Vec<TargetRule>,
}

/// Vollständige Beschreibung eines lokalen Forwards (`ssh -L`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelSpec {
    /// Lokale Loopback-Bindung.
    pub bind: BindAddr,
    /// Lokaler Listen-Port.
    pub local_port: u16,
    /// Zielhost auf der entfernten Seite.
    pub target_host: String,
    /// Zielport auf der entfernten Seite.
    pub target_port: u16,
    /// SSH-Server (Sprungziel des Forwards).
    pub ssh_host: String,
    /// Optionaler SSH-Benutzer.
    pub ssh_user: Option<String>,
    /// Optionale Schlüsseldatei-Referenz.
    pub key: Option<KeyRef>,
}

fn host_field(name: &'static str, v: &str) -> Result<(), PolicyError> {
    let bad = v.is_empty()
        || v.starts_with('-')
        || v.chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '@');
    if bad {
        Err(PolicyError::InvalidField(name))
    } else {
        Ok(())
    }
}

fn is_private_or_loopback(host: &str) -> bool {
    let h = host.trim_matches(|c| c == '[' || c == ']');
    if h.eq_ignore_ascii_case("localhost") || h.to_ascii_lowercase().ends_with(".localhost") {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => {
            a.is_loopback() || a.is_private() || a.is_link_local() || a.is_unspecified()
        }
        Ok(IpAddr::V6(a)) => {
            a.is_loopback()
                || a.is_unspecified()
                || (a.segments()[0] & 0xfe00) == 0xfc00
                || (a.segments()[0] & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
}

impl TunnelSpec {
    /// Struktur- und Feldprüfung, unabhängig von der Allowlist.
    pub fn validate_fields(&self) -> Result<(), PolicyError> {
        if self.local_port == 0 {
            return Err(PolicyError::InvalidPort("local_port"));
        }
        if self.target_port == 0 {
            return Err(PolicyError::InvalidPort("target_port"));
        }
        host_field("target_host", &self.target_host)?;
        host_field("ssh_host", &self.ssh_host)?;
        if let Some(u) = &self.ssh_user {
            host_field("ssh_user", u)?;
        }
        Ok(())
    }

    /// Start- und Reconnect-Prüfung: Felder, dann Allowlist (Host **und** Port),
    /// dann Default-Verbot für private/Loopback-Ziele.
    pub fn check(&self, allow: &Allowlist) -> Result<(), PolicyError> {
        self.validate_fields()?;
        let rule = allow
            .rules
            .iter()
            .find(|r| {
                r.host.eq_ignore_ascii_case(&self.target_host) && r.ports.contains(self.target_port)
            })
            .ok_or(PolicyError::TargetNotAllowed)?;
        if is_private_or_loopback(&self.target_host) && !rule.allow_private {
            return Err(PolicyError::PrivateTargetNotPermitted);
        }
        Ok(())
    }

    fn forward_arg(&self) -> String {
        let host = if self.target_host.contains(':') {
            format!(
                "[{}]",
                self.target_host.trim_matches(|c| c == '[' || c == ']')
            )
        } else {
            self.target_host.clone()
        };
        format!(
            "{}:{}:{}:{}",
            self.bind.spec(),
            self.local_port,
            host,
            self.target_port
        )
    }

    /// Argumentliste für `ssh` (ohne Programmnamen). Nur Referenzen, nie
    /// Schlüsselinhalt; `--` trennt Optionen vom Ziel.
    pub fn ssh_args(&self) -> Vec<String> {
        let mut a = vec![
            "-N".to_owned(),
            "-L".to_owned(),
            self.forward_arg(),
            "-o".to_owned(),
            "ExitOnForwardFailure=yes".to_owned(),
            "-o".to_owned(),
            "BatchMode=yes".to_owned(),
        ];
        if let Some(k) = &self.key {
            a.push("-i".to_owned());
            a.push(k.path().to_owned());
        }
        a.push("--".to_owned());
        a.push(match &self.ssh_user {
            Some(u) => format!("{u}@{}", self.ssh_host),
            None => self.ssh_host.clone(),
        });
        a
    }

    /// Kanonische, geordnete Beschreibung der wirksamen Konfiguration samt
    /// Allowlist; Grundlage der Freigabe (§4). Enthält den Schlüsselpfad nicht.
    pub fn canonical(&self, allow: &Allowlist) -> String {
        let mut rules: Vec<String> = allow
            .rules
            .iter()
            .map(|r| {
                let p = match r.ports {
                    Ports::One(a) => a.to_string(),
                    Ports::Range(a, b) => format!("{a}-{b}"),
                };
                format!("{}:{}:{}", r.host.to_ascii_lowercase(), p, r.allow_private)
            })
            .collect();
        rules.sort();
        format!(
            "bind={};local={};target={}:{};ssh={}@{};key={};allow=[{}]",
            self.bind.spec(),
            self.local_port,
            self.target_host.to_ascii_lowercase(),
            self.target_port,
            self.ssh_user.as_deref().unwrap_or(""),
            self.ssh_host.to_ascii_lowercase(),
            self.key.is_some(),
            rules.join(",")
        )
    }
}

/// Erteilte Freigabe für genau eine wirksame Konfiguration (§4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    canonical: String,
}

impl Approval {
    /// Freigabe für die aktuelle Konfiguration.
    pub fn grant(spec: &TunnelSpec, allow: &Allowlist) -> Self {
        Self {
            canonical: spec.canonical(allow),
        }
    }

    /// Gilt nur, solange Konfiguration **und** Allowlist unverändert sind.
    /// Für Start und Reconnect gleichermaßen.
    pub fn verify(&self, spec: &TunnelSpec, allow: &Allowlist) -> Result<(), PolicyError> {
        if self.canonical == spec.canonical(allow) {
            Ok(())
        } else {
            Err(PolicyError::ApprovalMismatch)
        }
    }
}

/// Entfernt Schlüsselinhalt und den Schlüsselpfad aus einer Ausgabe (§7).
pub fn redact(text: &str, key: Option<&KeyRef>) -> String {
    let mut out = String::new();
    let mut in_block = false;
    for line in text.lines() {
        if line.contains("-----BEGIN") && line.contains("PRIVATE KEY") {
            in_block = true;
            out.push_str("<redacted key material>\n");
            if line.contains("-----END") {
                in_block = false;
            }
            continue;
        }
        if in_block {
            if line.contains("-----END") {
                in_block = false;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if let Some(k) = key {
        out = out.replace(k.path(), "<key-ref>");
    }
    if !text.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(host: &str, port: u16, private: bool) -> Allowlist {
        Allowlist {
            rules: vec![TargetRule {
                host: host.to_owned(),
                ports: Ports::One(port),
                allow_private: private,
            }],
        }
    }

    fn spec(host: &str, port: u16) -> TunnelSpec {
        TunnelSpec {
            bind: BindAddr::V4,
            local_port: 8443,
            target_host: host.to_owned(),
            target_port: port,
            ssh_host: "jump.example.org".to_owned(),
            ssh_user: Some("mia".to_owned()),
            key: Some(KeyRef::new("/home/mia/.ssh/id_ed25519").unwrap()),
        }
    }

    #[test]
    fn bind_defaults_to_loopback_and_rejects_wider() {
        assert_eq!(BindAddr::parse(None), Ok(BindAddr::V4));
        assert_eq!(BindAddr::parse(Some("::1")), Ok(BindAddr::V6));
        for bad in ["0.0.0.0", "::", "192.168.1.5", "localhost", "example.org"] {
            assert!(matches!(
                BindAddr::parse(Some(bad)),
                Err(PolicyError::BindNotLoopback(_))
            ));
        }
    }

    #[test]
    fn allowlist_needs_host_and_port() {
        let a = allow("db.example.org", 5432, false);
        assert!(spec("db.example.org", 5432).check(&a).is_ok());
        assert!(spec("DB.Example.org", 5432).check(&a).is_ok());
        assert_eq!(
            spec("db.example.org", 5433).check(&a),
            Err(PolicyError::TargetNotAllowed)
        );
        assert_eq!(
            spec("other.example.org", 5432).check(&a),
            Err(PolicyError::TargetNotAllowed)
        );
        assert_eq!(
            spec("db.example.org", 5432).check(&Allowlist::default()),
            Err(PolicyError::TargetNotAllowed)
        );
    }

    #[test]
    fn no_implicit_wildcards() {
        let a = allow("example.org", 22, false);
        assert_eq!(
            spec("sub.example.org", 22).check(&a),
            Err(PolicyError::TargetNotAllowed)
        );
    }

    #[test]
    fn private_and_loopback_targets_need_explicit_permission() {
        for host in [
            "10.0.0.5",
            "192.168.1.2",
            "172.16.0.1",
            "127.0.0.1",
            "localhost",
            "::1",
            "fd00::1",
            "169.254.1.1",
        ] {
            let denied = allow(host, 80, false);
            assert_eq!(
                spec(host, 80).check(&denied),
                Err(PolicyError::PrivateTargetNotPermitted),
                "{host}"
            );
            assert!(
                spec(host, 80).check(&allow(host, 80, true)).is_ok(),
                "{host}"
            );
        }
        assert!(
            spec("8.8.8.8", 53)
                .check(&allow("8.8.8.8", 53, false))
                .is_ok()
        );
    }

    #[test]
    fn port_ranges() {
        let a = Allowlist {
            rules: vec![TargetRule {
                host: "h.example.org".into(),
                ports: Ports::Range(8000, 8010),
                allow_private: false,
            }],
        };
        assert!(spec("h.example.org", 8005).check(&a).is_ok());
        assert!(spec("h.example.org", 8011).check(&a).is_err());
    }

    #[test]
    fn rejects_argument_injection_and_zero_ports() {
        let a = allow("x", 1, false);
        for (field, s) in [
            (
                "target_host",
                TunnelSpec {
                    target_host: "-oProxyCommand=x".into(),
                    ..spec("x", 1)
                },
            ),
            (
                "ssh_host",
                TunnelSpec {
                    ssh_host: "-J evil".into(),
                    ..spec("x", 1)
                },
            ),
            (
                "ssh_user",
                TunnelSpec {
                    ssh_user: Some("a@b".into()),
                    ..spec("x", 1)
                },
            ),
            (
                "target_host",
                TunnelSpec {
                    target_host: "a b".into(),
                    ..spec("x", 1)
                },
            ),
        ] {
            assert_eq!(s.check(&a), Err(PolicyError::InvalidField(field)));
        }
        assert!(
            TunnelSpec {
                local_port: 0,
                ..spec("x", 1)
            }
            .check(&a)
            .is_err()
        );
        assert!(
            TunnelSpec {
                target_port: 0,
                ..spec("x", 1)
            }
            .check(&a)
            .is_err()
        );
    }

    #[test]
    fn key_ref_rejects_key_material() {
        assert!(KeyRef::new("-----BEGIN OPENSSH PRIVATE KEY-----").is_err());
        assert!(KeyRef::new("line1\nline2").is_err());
        assert!(KeyRef::new("").is_err());
        assert!(KeyRef::new("-i/evil").is_err());
        assert_eq!(
            format!("{:?}", KeyRef::new("/k").unwrap()),
            "KeyRef(<redacted>)"
        );
    }

    #[test]
    fn ssh_args_are_loopback_only_and_end_with_separator() {
        let args = spec("db.example.org", 5432).ssh_args();
        assert_eq!(args[2], "127.0.0.1:8443:db.example.org:5432");
        assert!(args.contains(&"ExitOnForwardFailure=yes".to_owned()));
        let n = args.len();
        assert_eq!(args[n - 2], "--");
        assert_eq!(args[n - 1], "mia@jump.example.org");
        let v6 = TunnelSpec {
            bind: BindAddr::V6,
            ..spec("fd00::1", 80)
        }
        .ssh_args();
        assert_eq!(v6[2], "[::1]:8443:[fd00::1]:80");
    }

    #[test]
    fn approval_is_bound_to_config_and_allowlist() {
        let a = allow("db.example.org", 5432, false);
        let s = spec("db.example.org", 5432);
        let ap = Approval::grant(&s, &a);
        assert!(ap.verify(&s, &a).is_ok());
        let widened = Allowlist {
            rules: vec![
                a.rules[0].clone(),
                TargetRule {
                    host: "x.example.org".into(),
                    ports: Ports::One(1),
                    allow_private: false,
                },
            ],
        };
        assert_eq!(ap.verify(&s, &widened), Err(PolicyError::ApprovalMismatch));
        let moved = TunnelSpec {
            local_port: 9000,
            ..s.clone()
        };
        assert_eq!(ap.verify(&moved, &a), Err(PolicyError::ApprovalMismatch));
        let reordered = Allowlist {
            rules: a.rules.iter().rev().cloned().collect(),
        };
        assert!(ap.verify(&s, &reordered).is_ok());
    }

    #[test]
    fn canonical_does_not_contain_key_path() {
        let c = spec("db.example.org", 5432).canonical(&allow("db.example.org", 5432, false));
        assert!(!c.contains(".ssh"));
    }

    #[test]
    fn redact_removes_key_blocks_and_paths() {
        let k = KeyRef::new("/home/mia/.ssh/id_ed25519").unwrap();
        let text = "using /home/mia/.ssh/id_ed25519\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\ndone";
        let out = redact(text, Some(&k));
        assert!(!out.contains("AAAA"));
        assert!(!out.contains(".ssh"));
        assert!(out.contains("<key-ref>"));
        assert!(out.ends_with("done"));
    }
}
