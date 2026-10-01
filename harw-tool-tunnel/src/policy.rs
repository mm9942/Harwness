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
    /// Hostname-Ziel ohne ausdrückliche Zustimmung zur Auflösung auf der
    /// Gegenseite: `ssh -L` löst den Namen auf dem SSH-Server auf, das lässt
    /// sich lokal weder prüfen noch festnageln.
    HostnameNotVerifiable,
    /// Zwei überlappende Allowlist-Einträge widersprechen sich.
    ConflictingRules,
    /// Ein Allowlist-Eintrag ist ungültig (Host, Portbereich).
    InvalidRule,
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
            Self::HostnameNotVerifiable => f.write_str(
                "hostname target is resolved on the remote ssh server and cannot be verified; \
                 use a literal ip or opt in with allow_remote_resolution",
            ),
            Self::ConflictingRules => f.write_str("overlapping allowlist rules conflict"),
            Self::InvalidRule => f.write_str("invalid allowlist rule"),
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
    fn bounds(&self) -> (u16, u16) {
        match *self {
            Self::One(a) => (a, a),
            Self::Range(a, b) => (a, b),
        }
    }

    fn overlaps(&self, other: &Self) -> bool {
        let (a, b) = self.bounds();
        let (c, d) = other.bounds();
        a <= d && c <= b
    }

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
    /// Zustimmung, dass ein Hostname erst auf dem SSH-Server aufgelöst wird.
    /// Ohne sie sind nur Literal-IP-Ziele zulässig, denn das Ergebnis der
    /// Auflösung (auch Rebinding) ist lokal nicht prüfbar. Mit ihr gilt die
    /// Default-Sperre für private Ziele für diesen Namen **nicht** als
    /// durchgesetzt.
    pub allow_remote_resolution: bool,
}

/// Wirksame Ziel-Allowlist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Allowlist {
    /// Einträge; leer erlaubt nichts.
    pub rules: Vec<TargetRule>,
}

impl Allowlist {
    /// Strukturprüfung: gültige Hosts und Portbereiche, keine widersprüchlich
    /// überlappenden Einträge (gleicher Host, sich schneidende Ports,
    /// unterschiedliche Erlaubnisse). Reihenfolge darf keine Rolle spielen.
    pub fn validate(&self) -> Result<(), PolicyError> {
        for r in &self.rules {
            host_field("rule_host", &r.host).map_err(|_| PolicyError::InvalidRule)?;
            let (lo, hi) = match r.ports {
                Ports::One(a) => (a, a),
                Ports::Range(a, b) => (a, b),
            };
            if lo == 0 || lo > hi {
                return Err(PolicyError::InvalidRule);
            }
        }
        for (i, a) in self.rules.iter().enumerate() {
            for b in &self.rules[i + 1..] {
                if a.host.eq_ignore_ascii_case(&b.host)
                    && a.ports.overlaps(&b.ports)
                    && (a.allow_private != b.allow_private
                        || a.allow_remote_resolution != b.allow_remote_resolution)
                {
                    return Err(PolicyError::ConflictingRules);
                }
            }
        }
        Ok(())
    }
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

fn is_ip_literal(host: &str) -> bool {
    host.trim_matches(|c| c == '[' || c == ']')
        .parse::<IpAddr>()
        .is_ok()
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
            if let Some(v4) = a.to_ipv4_mapped() {
                return v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified();
            }
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
        allow.validate()?;
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
        if !is_ip_literal(&self.target_host) && !rule.allow_remote_resolution {
            return Err(PolicyError::HostnameNotVerifiable);
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
    /// Allowlist; Grundlage der Freigabe (§4). Jedes Textfeld ist
    /// längenpräfixiert, damit die Kodierung injektiv ist. Sie enthält die
    /// Schlüsselreferenz (Bindung an die gewählte Identität) und ist deshalb
    /// nur crate-intern sichtbar und wird nie ausgegeben.
    pub(crate) fn canonical(&self, allow: &Allowlist) -> String {
        fn t(s: &str) -> String {
            format!("{}:{}", s.len(), s)
        }
        let mut rules: Vec<String> = allow
            .rules
            .iter()
            .map(|r| {
                let (lo, hi) = r.ports.bounds();
                format!(
                    "[{}|{lo}-{hi}|{}|{}]",
                    t(&r.host.to_ascii_lowercase()),
                    r.allow_private,
                    r.allow_remote_resolution
                )
            })
            .collect();
        rules.sort();
        rules.dedup();
        format!(
            "bind={};local={};target={}|{};ssh={}|{};key={};allow={}",
            self.bind.spec(),
            self.local_port,
            t(&self.target_host.to_ascii_lowercase()),
            self.target_port,
            t(self.ssh_user.as_deref().unwrap_or("")),
            t(&self.ssh_host.to_ascii_lowercase()),
            t(self.key.as_ref().map_or("", KeyRef::path)),
            rules.concat()
        )
    }
}

/// Erteilte Freigabe für genau eine wirksame Konfiguration (§4).
#[derive(Clone, PartialEq, Eq)]
pub struct Approval {
    canonical: String,
}

impl fmt::Debug for Approval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Approval(<redacted>)")
    }
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
    use crate::test_support::{TestResult, ensure};

    fn rule(host: &str, ports: Ports, private: bool, remote: bool) -> TargetRule {
        TargetRule {
            host: host.to_owned(),
            ports,
            allow_private: private,
            allow_remote_resolution: remote,
        }
    }

    fn allow(host: &str, port: u16, private: bool) -> Allowlist {
        Allowlist {
            rules: vec![rule(host, Ports::One(port), private, true)],
        }
    }

    fn spec(host: &str, port: u16) -> Result<TunnelSpec, PolicyError> {
        Ok(TunnelSpec {
            bind: BindAddr::V4,
            local_port: 8443,
            target_host: host.to_owned(),
            target_port: port,
            ssh_host: "jump.example.org".to_owned(),
            ssh_user: Some("mia".to_owned()),
            key: Some(KeyRef::new("/home/mia/.ssh/id_ed25519")?),
        })
    }

    #[test]
    fn bind_defaults_to_loopback_and_rejects_wider() -> TestResult {
        ensure(
            BindAddr::parse(None) == Ok(BindAddr::V4),
            "default is v4 loopback",
        )?;
        ensure(
            BindAddr::parse(Some("::1")) == Ok(BindAddr::V6),
            "::1 accepted",
        )?;
        for bad in ["0.0.0.0", "::", "192.168.1.5", "localhost", "example.org"] {
            ensure(
                matches!(
                    BindAddr::parse(Some(bad)),
                    Err(PolicyError::BindNotLoopback(_))
                ),
                bad,
            )?;
        }
        Ok(())
    }

    #[test]
    fn allowlist_needs_host_and_port() -> TestResult {
        let a = allow("db.example.org", 5432, false);
        ensure(
            spec("db.example.org", 5432)?.check(&a).is_ok(),
            "exact match",
        )?;
        ensure(
            spec("DB.Example.org", 5432)?.check(&a).is_ok(),
            "case-insensitive",
        )?;
        ensure(
            spec("db.example.org", 5433)?.check(&a) == Err(PolicyError::TargetNotAllowed),
            "port",
        )?;
        ensure(
            spec("other.example.org", 5432)?.check(&a) == Err(PolicyError::TargetNotAllowed),
            "host",
        )?;
        ensure(
            spec("db.example.org", 5432)?.check(&Allowlist::default())
                == Err(PolicyError::TargetNotAllowed),
            "empty list allows nothing",
        )
    }

    #[test]
    fn no_implicit_wildcards() -> TestResult {
        let a = allow("example.org", 22, false);
        ensure(
            spec("sub.example.org", 22)?.check(&a) == Err(PolicyError::TargetNotAllowed),
            "wildcard",
        )
    }

    #[test]
    fn private_and_loopback_literals_need_explicit_permission() -> TestResult {
        for host in [
            "10.0.0.5",
            "192.168.1.2",
            "172.16.0.1",
            "127.0.0.1",
            "localhost",
            "::1",
            "fd00::1",
            "169.254.1.1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:192.168.1.1",
            "[::ffff:127.0.0.1]",
        ] {
            ensure(
                spec(host, 80)?.check(&allow(host, 80, false))
                    == Err(PolicyError::PrivateTargetNotPermitted),
                host,
            )?;
            ensure(spec(host, 80)?.check(&allow(host, 80, true)).is_ok(), host)?;
        }
        ensure(
            spec("8.8.8.8", 53)?
                .check(&allow("8.8.8.8", 53, false))
                .is_ok(),
            "public ip",
        )?;
        ensure(
            spec("::ffff:8.8.8.8", 53)?
                .check(&allow("::ffff:8.8.8.8", 53, false))
                .is_ok(),
            "mapped public",
        )
    }

    #[test]
    fn hostname_targets_need_remote_resolution_opt_in() -> TestResult {
        let strict = Allowlist {
            rules: vec![rule("db.example.org", Ports::One(5432), false, false)],
        };
        ensure(
            spec("db.example.org", 5432)?.check(&strict) == Err(PolicyError::HostnameNotVerifiable),
            "hostname without opt-in",
        )?;
        let opted = Allowlist {
            rules: vec![rule("db.example.org", Ports::One(5432), false, true)],
        };
        ensure(
            spec("db.example.org", 5432)?.check(&opted).is_ok(),
            "hostname with opt-in",
        )?;
        let ip = Allowlist {
            rules: vec![rule("8.8.8.8", Ports::One(53), false, false)],
        };
        ensure(
            spec("8.8.8.8", 53)?.check(&ip).is_ok(),
            "literal ip needs no opt-in",
        )
    }

    #[test]
    fn port_ranges_and_invalid_rules() -> TestResult {
        let a = Allowlist {
            rules: vec![rule("h.example.org", Ports::Range(8000, 8010), false, true)],
        };
        ensure(spec("h.example.org", 8005)?.check(&a).is_ok(), "in range")?;
        ensure(
            spec("h.example.org", 8011)?.check(&a).is_err(),
            "out of range",
        )?;
        for bad in [
            rule("h.example.org", Ports::Range(10, 5), false, true),
            rule("h.example.org", Ports::One(0), false, true),
            rule("", Ports::One(1), false, true),
            rule("-x", Ports::One(1), false, true),
            rule("a b", Ports::One(1), false, true),
        ] {
            ensure(
                Allowlist { rules: vec![bad] }.validate() == Err(PolicyError::InvalidRule),
                "invalid rule rejected",
            )?;
        }
        Ok(())
    }

    #[test]
    fn conflicting_overlapping_rules_are_rejected_regardless_of_order() -> TestResult {
        let a = rule("10.0.0.5", Ports::One(80), true, true);
        let b = rule("10.0.0.5", Ports::Range(70, 90), false, true);
        let fwd = Allowlist {
            rules: vec![a.clone(), b.clone()],
        };
        let rev = Allowlist { rules: vec![b, a] };
        ensure(
            fwd.validate() == Err(PolicyError::ConflictingRules),
            "forward",
        )?;
        ensure(
            rev.validate() == Err(PolicyError::ConflictingRules),
            "reverse",
        )?;
        ensure(
            spec("10.0.0.5", 80)?.check(&fwd) == Err(PolicyError::ConflictingRules),
            "check rejects",
        )?;
        let same = Allowlist {
            rules: vec![
                rule("a.example.org", Ports::One(1), false, true),
                rule("A.example.org", Ports::One(1), false, true),
            ],
        };
        ensure(same.validate().is_ok(), "identical duplicates are harmless")
    }

    #[test]
    fn rejects_argument_injection_and_zero_ports() -> TestResult {
        let a = allow("x", 1, false);
        let base = spec("x", 1)?;
        for (field, s) in [
            (
                "target_host",
                TunnelSpec {
                    target_host: "-oProxyCommand=x".into(),
                    ..base.clone()
                },
            ),
            (
                "ssh_host",
                TunnelSpec {
                    ssh_host: "-J evil".into(),
                    ..base.clone()
                },
            ),
            (
                "ssh_user",
                TunnelSpec {
                    ssh_user: Some("a@b".into()),
                    ..base.clone()
                },
            ),
            (
                "target_host",
                TunnelSpec {
                    target_host: "a b".into(),
                    ..base.clone()
                },
            ),
        ] {
            ensure(s.check(&a) == Err(PolicyError::InvalidField(field)), field)?;
        }
        ensure(
            TunnelSpec {
                local_port: 0,
                ..base.clone()
            }
            .check(&a)
            .is_err(),
            "local port 0",
        )?;
        ensure(
            TunnelSpec {
                target_port: 0,
                ..base
            }
            .check(&a)
            .is_err(),
            "target port 0",
        )
    }

    #[test]
    fn key_ref_rejects_key_material() -> TestResult {
        ensure(
            KeyRef::new("-----BEGIN OPENSSH PRIVATE KEY-----").is_err(),
            "pem header",
        )?;
        ensure(KeyRef::new("line1\nline2").is_err(), "newline")?;
        ensure(KeyRef::new("").is_err(), "empty")?;
        ensure(KeyRef::new("-i/evil").is_err(), "option-like")?;
        ensure(
            format!("{:?}", KeyRef::new("/k")?) == "KeyRef(<redacted>)",
            "debug redacted",
        )
    }

    #[test]
    fn ssh_args_are_loopback_only_and_end_with_separator() -> TestResult {
        let args = spec("db.example.org", 5432)?.ssh_args();
        ensure(
            args[2] == "127.0.0.1:8443:db.example.org:5432",
            "forward arg",
        )?;
        ensure(
            args.contains(&"ExitOnForwardFailure=yes".to_owned()),
            "exit on failure",
        )?;
        let n = args.len();
        ensure(
            args[n - 2] == "--" && args[n - 1] == "mia@jump.example.org",
            "separator and target",
        )?;
        let v6 = TunnelSpec {
            bind: BindAddr::V6,
            ..spec("fd00::1", 80)?
        }
        .ssh_args();
        ensure(v6[2] == "[::1]:8443:[fd00::1]:80", "v6 forward arg")
    }

    #[test]
    fn approval_is_bound_to_config_allowlist_and_key() -> TestResult {
        let a = allow("db.example.org", 5432, false);
        let s = spec("db.example.org", 5432)?;
        let ap = Approval::grant(&s, &a);
        ensure(ap.verify(&s, &a).is_ok(), "same config")?;
        let widened = Allowlist {
            rules: vec![
                a.rules[0].clone(),
                rule("x.example.org", Ports::One(1), false, true),
            ],
        };
        ensure(
            ap.verify(&s, &widened) == Err(PolicyError::ApprovalMismatch),
            "widened",
        )?;
        let moved = TunnelSpec {
            local_port: 9000,
            ..s.clone()
        };
        ensure(
            ap.verify(&moved, &a) == Err(PolicyError::ApprovalMismatch),
            "moved port",
        )?;
        let other_key = TunnelSpec {
            key: Some(KeyRef::new("/home/mia/.ssh/other")?),
            ..s.clone()
        };
        ensure(
            ap.verify(&other_key, &a) == Err(PolicyError::ApprovalMismatch),
            "swapped key",
        )?;
        let no_key = TunnelSpec {
            key: None,
            ..s.clone()
        };
        ensure(
            ap.verify(&no_key, &a) == Err(PolicyError::ApprovalMismatch),
            "removed key",
        )?;
        let two = Allowlist {
            rules: vec![
                a.rules[0].clone(),
                rule("x.example.org", Ports::One(1), false, true),
            ],
        };
        let reordered = Allowlist {
            rules: two.rules.iter().rev().cloned().collect(),
        };
        let ap2 = Approval::grant(&s, &two);
        ensure(
            ap2.verify(&s, &reordered).is_ok(),
            "reordering is not a change",
        )
    }

    #[test]
    fn canonical_encoding_is_injective_for_delimiter_hosts() -> TestResult {
        let s = spec("a.example.org", 1)?;
        let two = Allowlist {
            rules: vec![
                rule("a", Ports::One(1), false, true),
                rule("b", Ports::One(2), false, true),
            ],
        };
        let one = Allowlist {
            rules: vec![rule("a|1-1|false|true],[1:b", Ports::One(2), false, true)],
        };
        ensure(
            s.canonical(&two) != s.canonical(&one),
            "delimiter smuggling changes the encoding",
        )
    }

    #[test]
    fn approval_debug_does_not_leak_the_key_path() -> TestResult {
        let s = spec("db.example.org", 5432)?;
        let ap = Approval::grant(&s, &allow("db.example.org", 5432, false));
        let shown = format!("{ap:?}");
        ensure(
            !shown.contains(".ssh") && shown == "Approval(<redacted>)",
            "redacted",
        )
    }

    #[test]
    fn redact_removes_key_blocks_and_paths() -> TestResult {
        let k = KeyRef::new("/home/mia/.ssh/id_ed25519")?;
        let text = "using /home/mia/.ssh/id_ed25519\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\ndone";
        let out = redact(text, Some(&k));
        ensure(
            !out.contains("AAAA") && !out.contains(".ssh"),
            "secrets removed",
        )?;
        ensure(
            out.contains("<key-ref>") && out.ends_with("done"),
            "reference kept, text kept",
        )
    }
}
