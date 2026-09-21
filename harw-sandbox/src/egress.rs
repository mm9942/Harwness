//! Kanonische Auswertung ausgehender URL-Ziele (Egress).
//!
//! # Verantwortung
//! Eine Netzgrenze ist nur so gut wie die Übereinstimmung zwischen dem Host,
//! der *geprüft* wird, und dem Host, zu dem *verbunden* wird. Dieses Modul
//! parst URLs deshalb ausschließlich mit `url::Url::parse` — demselben
//! WHATWG-Parser, den `reqwest` beim Verbindungsaufbau verwendet. Eine eigene
//! String-Zerlegung (wie früher in `harw_tools::host_from_url`) weicht bei
//! Randfällen ab: für `https://evil.com\@docs.rs/` behandelt der WHATWG-Parser
//! `\` bei Spezial-Schemata wie `/`, der Host ist also `evil.com`, nicht
//! `docs.rs`.
//!
//! # Zentrale Typen
//! - [`EgressUrl`] — geparste, auf `http`/`https` beschränkte URL ohne Userinfo
//!   mit normalisiertem Host und effektivem Port.
//! - [`EgressHost`] — Domain (Punycode, kleingeschrieben, ohne abschließenden
//!   Punkt) oder IP-Adresse.
//! - [`EgressUrlError`] — Ablehnungsgründe ohne Echo der Eingabe.
//! - [`harw_authority::host_matches_suffix`] — die *eine* Punktgrenzen-Regel
//!   für Host-Suffixe.
//!
//! # Nebenläufigkeit
//! Reine Daten und reine Funktionen: `Send + Sync`, keine Sperren.
//!
//! # Beispiele
//! ```rust
//! use harw_authority::host_matches_suffix;
//! use harw_sandbox::egress::{EgressHost, EgressUrl};
//!
//! let url = EgressUrl::parse("https://evil.com\\@docs.rs/").unwrap();
//! assert_eq!(url.host_str(), "evil.com");
//! assert!(!host_matches_suffix("docs.rs", &url.host_str()));
//! assert!(matches!(url.host(), EgressHost::Domain(_)));
//! ```

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[cfg(test)]
use harw_authority::host_matches_suffix;

/// Ablehnungsgründe von [`EgressUrl::parse`].
///
/// # Beschreibung
/// Die `Display`-Texte wiederholen die Eingabe bewusst nicht: eine abgelehnte
/// URL kann Zugangsdaten (Userinfo) oder vom Modell gesteuerte Inhalte tragen,
/// die nicht in Fehlermeldungen oder Logs landen sollen. Auch
/// [`Self::UnsupportedScheme`] trägt das Schema nur für programmatische
/// Auswertung, nicht im `Display`-Text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressUrlError {
    /// Der WHATWG-Parser lehnt die Eingabe ab (z. B. relative URL ohne Schema,
    /// leerer Host, ungültiges IPv6-Literal, verbotene Zeichen im Host).
    Parse,
    /// Das Schema ist weder `http` noch `https`. Trägt das (vom Parser
    /// kleingeschriebene) Schema.
    UnsupportedScheme(String),
    /// Benutzername oder Passwort sind nicht leer.
    UserinfoPresent,
    /// Die URL hat keinen Host.
    MissingHost,
    /// Der Host ist nach der Normalisierung kein verwendbarer Name (z. B. nur
    /// Punkte oder leere Labels wie in `a..b`).
    InvalidHost,
}

impl fmt::Display for EgressUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse => write!(f, "URL ist nicht parsebar"),
            Self::UnsupportedScheme(_) => write!(
                f,
                "URL-Schema ist für ausgehende Verbindungen nicht erlaubt (nur http/https)"
            ),
            Self::UserinfoPresent => write!(f, "URL enthält Benutzerdaten (Userinfo)"),
            Self::MissingHost => write!(f, "URL enthält keinen Host"),
            Self::InvalidHost => write!(f, "URL enthält keinen gültigen Host"),
        }
    }
}

impl std::error::Error for EgressUrlError {}

/// Normalisierter Ziel-Host einer [`EgressUrl`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EgressHost {
    /// ASCII-Domain: IDNA-Punycode, kleingeschrieben, ohne abschließenden Punkt.
    Domain(String),
    /// IPv4- oder IPv6-Literal (auch WHATWG-Kurzformen wie `0x7f.1`).
    Ip(IpAddr),
}

impl EgressHost {
    /// Entscheidet, ob der Host die eigene Maschine bezeichnet.
    ///
    /// # Returns
    /// `true` für `localhost` und `*.localhost` (RFC 6761), für `127.0.0.0/8`,
    /// für `::1` und für IPv4-gemappte Loopback-Adressen (`::ffff:127.0.0.1`).
    ///
    /// Weil [`EgressHost::Domain`] frei konstruierbar ist (z. B. aus
    /// Konfiguration), setzt der Domain-Vergleich die Normalform **nicht**
    /// voraus: er ist ASCII-case-insensitiv und toleriert einen abschließenden
    /// Punkt (`LOCALHOST`, `localhost.`, `Api.LocalHost.`).
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_sandbox::egress::EgressUrl;
    ///
    /// assert!(EgressUrl::parse("http://api.localhost/").unwrap().host().is_loopback());
    /// assert!(EgressUrl::parse("http://[::1]:8080/").unwrap().host().is_loopback());
    /// ```
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        match self {
            Self::Domain(name) => domain_is_localhost(name),
            Self::Ip(IpAddr::V4(v4)) => v4_in(*v4, [127, 0, 0, 0], 8),
            Self::Ip(IpAddr::V6(v6)) => match v4_mapped(*v6) {
                Some(v4) => v4_in(v4, [127, 0, 0, 0], 8),
                None => *v6 == Ipv6Addr::LOCALHOST,
            },
        }
    }

    /// Entscheidet, ob der Host kein gewöhnliches öffentliches Internet-Ziel ist.
    ///
    /// # Beschreibung
    /// Fail-closed-Klassifikation für SSRF-Schutz. Als privat oder speziell
    /// gelten:
    /// - **IPv4:** `0.0.0.0/8`, `10.0.0.0/8`, `100.64.0.0/10` (CGNAT),
    ///   `127.0.0.0/8`, `169.254.0.0/16` (Link-Local inkl. Cloud-Metadaten
    ///   `169.254.169.254`), `172.16.0.0/12`, `192.0.0.0/24`,
    ///   `192.0.2.0/24`, `192.88.99.0/24` (6to4-Relay-Anycast, RFC 7526),
    ///   `198.18.0.0/15`, `198.51.100.0/24`, `203.0.113.0/24`,
    ///   `192.168.0.0/16`, `224.0.0.0/4` (Multicast), `240.0.0.0/4` (reserviert,
    ///   inkl. Broadcast `255.255.255.255`).
    /// - **IPv6:** `::/96` (inkl. `::` und `::1`, veraltete IPv4-kompatible
    ///   Adressen), `::ffff:0:0/96` (IPv4-gemappt — die eingebettete Adresse wird
    ///   nach den IPv4-Regeln bewertet), `::ffff:0:0:0/96` (SIIT
    ///   IPv4-übersetzt, RFC 6145), `64:ff9b::/96` und `64:ff9b:1::/48`
    ///   (NAT64/SIIT), `100::/64` (Discard), `2001::/32` (Teredo),
    ///   `2001:db8::/32` (Dokumentation), `2002::/16` (6to4), `fc00::/7` (ULA,
    ///   inkl. `fd00:ec2::254`), `fe80::/10` (Link-Local), `fec0::/10`
    ///   (veraltetes Site-Local), `ff00::/8` (Multicast).
    ///
    ///   Die Übergangsbereiche (SIIT, NAT64, Teredo, 6to4) betten IPv4-Adressen
    ///   ein (6to4: `2002:AABB:CCDD::` ≙ `AA.BB.CC.DD`, also z. B.
    ///   `2002:a00:1::` ≙ `10.0.0.1`). Sie werden **unabhängig** von der
    ///   eingebetteten Adresse vollständig als speziell gewertet: das deckt jede
    ///   eingebettete private Adresse ab, und öffentliche Ziele sind über diese
    ///   veralteten bzw. netzinternen Übersetzer ohnehin nicht zuverlässig
    ///   erreichbar (fail closed).
    /// - **Domains:** nur `localhost` und `*.localhost`. Eine Domain kann per DNS
    ///   auf jede Adresse zeigen; wer SSRF verhindern will, muss zusätzlich die
    ///   *aufgelöste* Adresse als [`EgressHost::Ip`] prüfen.
    ///
    /// Alle Prüfungen sind Präfixvergleiche auf `u32`/`u128`; es werden keine
    /// erst nach Rust 1.85 stabilisierten `std::net`-Methoden benötigt.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_sandbox::egress::EgressUrl;
    ///
    /// let meta = EgressUrl::parse("http://169.254.169.254/latest/").unwrap();
    /// assert!(meta.host().is_private_or_special());
    /// assert!(!EgressUrl::parse("https://docs.rs/").unwrap().host().is_private_or_special());
    /// ```
    #[must_use]
    pub fn is_private_or_special(&self) -> bool {
        match self {
            Self::Domain(_) => self.is_loopback(),
            Self::Ip(IpAddr::V4(v4)) => v4_is_private_or_special(*v4),
            Self::Ip(IpAddr::V6(v6)) => v6_is_private_or_special(*v6),
        }
    }
}

/// Eine für ausgehende Verbindungen zugelassene, geparste URL.
///
/// # Invarianten
/// - Schema ist `http` oder `https`.
/// - Benutzername und Passwort sind leer.
/// - [`Self::host`] ist der normalisierte Host genau der URL, die
///   [`Self::as_url`] liefert — wer prüft und verbindet, muss beide aus
///   derselben `EgressUrl` beziehen.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EgressUrl {
    url: url::Url,
    host: EgressHost,
    port: u16,
}

impl EgressUrl {
    /// Parst und normalisiert eine absolute `http`/`https`-URL.
    ///
    /// # Beschreibung
    /// Parsing ausschließlich über `url::Url::parse` (WHATWG). Daraus folgt u. a.:
    /// `\` beendet bei Spezial-Schemata die Autorität, prozentkodierte Hosts
    /// werden dekodiert, internationalisierte Namen werden per IDNA zu Punycode,
    /// Großbuchstaben werden kleingeschrieben und IPv4-Kurzformen zu Adressen.
    /// Zusätzlich entfernt diese Funktion genau einen abschließenden Punkt einer
    /// Domain und lehnt Domains mit leeren Labels ab.
    ///
    /// # Arguments
    /// - `input` (`&str`): die rohe URL. Führende/abschließende C0-Steuerzeichen
    ///   und Leerzeichen entfernt der WHATWG-Parser selbst.
    ///
    /// # Returns
    /// Die geprüfte [`EgressUrl`].
    ///
    /// # Errors
    /// - [`EgressUrlError::Parse`]: WHATWG-Parser lehnt ab (auch schema-lose Eingaben).
    /// - [`EgressUrlError::UnsupportedScheme`]: Schema ist nicht `http`/`https`.
    /// - [`EgressUrlError::UserinfoPresent`]: Benutzername oder Passwort gesetzt.
    /// - [`EgressUrlError::MissingHost`]: kein Host vorhanden.
    /// - [`EgressUrlError::InvalidHost`]: Domain nach Normalisierung unbrauchbar.
    ///
    /// # Panics
    /// Nie.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_sandbox::egress::{EgressUrl, EgressUrlError};
    ///
    /// let url = EgressUrl::parse("HTTPS://Docs.RS./serde").unwrap();
    /// assert_eq!(url.host_str(), "docs.rs");
    /// assert_eq!(url.port(), 443);
    /// assert_eq!(
    ///     EgressUrl::parse("https://user@docs.rs/"),
    ///     Err(EgressUrlError::UserinfoPresent)
    /// );
    /// ```
    pub fn parse(input: &str) -> Result<Self, EgressUrlError> {
        let url = url::Url::parse(input).map_err(|_| EgressUrlError::Parse)?;

        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(EgressUrlError::UnsupportedScheme(scheme.to_owned()));
        }

        if !url.username().is_empty() || url.password().is_some_and(|pw| !pw.is_empty()) {
            return Err(EgressUrlError::UserinfoPresent);
        }

        let host = match url.host() {
            None => return Err(EgressUrlError::MissingHost),
            Some(url::Host::Ipv4(v4)) => EgressHost::Ip(IpAddr::V4(v4)),
            Some(url::Host::Ipv6(v6)) => EgressHost::Ip(IpAddr::V6(v6)),
            Some(url::Host::Domain(domain)) => EgressHost::Domain(normalize_domain(domain)?),
        };

        // Für `http`/`https` kennt `url` den Standardport immer; der Zweig ohne
        // Port ist daher nur eine panikfreie Absicherung.
        let port = url
            .port_or_known_default()
            .ok_or_else(|| EgressUrlError::UnsupportedScheme(scheme.to_owned()))?;

        Ok(Self { url, host, port })
    }

    /// Die geparste URL — genau diese (oder ihre Serialisierung) ist zu senden.
    #[must_use]
    pub fn as_url(&self) -> &url::Url {
        &self.url
    }

    /// Der normalisierte Host.
    #[must_use]
    pub fn host(&self) -> &EgressHost {
        &self.host
    }

    /// Der normalisierte Host als Zeichenkette für Scope-Prüfungen.
    ///
    /// # Returns
    /// Domains unverändert in Normalform; IP-Adressen in `std`-Darstellung,
    /// IPv6 **ohne** eckige Klammern (`::1`, `::ffff:10.0.0.1`).
    #[must_use]
    pub fn host_str(&self) -> String {
        match &self.host {
            EgressHost::Domain(name) => name.clone(),
            EgressHost::Ip(ip) => ip.to_string(),
        }
    }

    /// Der effektive Port (explizit oder Standardport 80/443).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `true`, wenn das Schema `https` ist.
    #[must_use]
    pub fn is_https(&self) -> bool {
        self.url.scheme() == "https"
    }
}

// `localhost` oder `*.localhost`, ASCII-case-insensitiv, ein abschließender
// Punkt toleriert. `get` statt Indexierung: an einer Nicht-Zeichengrenze gibt
// es keinen Treffer statt eines Panics.
fn domain_is_localhost(name: &str) -> bool {
    const LOCALHOST: &str = "localhost";
    const DOT_LOCALHOST: &str = ".localhost";
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.eq_ignore_ascii_case(LOCALHOST) {
        return true;
    }
    name.len()
        .checked_sub(DOT_LOCALHOST.len())
        .and_then(|start| name.get(start..))
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(DOT_LOCALHOST))
}

// Normalform einer Domain aus `url::Host::Domain`: `url` liefert für
// Spezial-Schemata bereits IDNA-Punycode in Kleinschreibung; die erneute
// ASCII-Kleinschreibung ist Absicherung. Genau ein abschließender Punkt wird
// entfernt; leere Labels (`a..b`, `.`, `docs.rs..`) oder Nicht-ASCII-Reste
// werden abgelehnt.
fn normalize_domain(domain: &str) -> Result<String, EgressUrlError> {
    let lowered = domain.to_ascii_lowercase();
    let trimmed = lowered.strip_suffix('.').unwrap_or(&lowered);
    if trimmed.is_empty() || !trimmed.is_ascii() || trimmed.split('.').any(str::is_empty) {
        return Err(EgressUrlError::InvalidHost);
    }
    Ok(trimmed.to_owned())
}

// Präfixtest `addr ∈ net/len` für IPv4. `len == 0` trifft alles; für
// `1..=32` liegt die Schiebeweite in `0..32`.
fn v4_in(addr: Ipv4Addr, net: [u8; 4], len: u32) -> bool {
    if len == 0 {
        return true;
    }
    let shift = 32_u32.saturating_sub(len);
    (u32::from(addr) ^ u32::from(Ipv4Addr::from(net))) >> shift == 0
}

// Präfixtest `addr ∈ net/len` für IPv6, analog zu `v4_in`.
fn v6_in(addr: Ipv6Addr, net: [u16; 8], len: u32) -> bool {
    if len == 0 {
        return true;
    }
    let shift = 128_u32.saturating_sub(len);
    (u128::from(addr) ^ u128::from(Ipv6Addr::from(net))) >> shift == 0
}

// Eingebettete IPv4-Adresse aus `::ffff:0:0/96`, sonst `None`. Bewusst von
// Hand statt über `Ipv6Addr::to_ipv4_mapped`, damit die Präfixregel an einer
// Stelle sichtbar bleibt.
fn v4_mapped(addr: Ipv6Addr) -> Option<Ipv4Addr> {
    if v6_in(addr, [0, 0, 0, 0, 0, 0xffff, 0, 0], 96) {
        // Die unteren 32 Bit sind die IPv4-Adresse; `as u32` schneidet genau
        // diese ab.
        Some(Ipv4Addr::from(u128::from(addr) as u32))
    } else {
        None
    }
}

// IPv4-Tabelle, siehe `EgressHost::is_private_or_special`.
const V4_SPECIAL: [([u8; 4], u32); 15] = [
    ([0, 0, 0, 0], 8),
    ([10, 0, 0, 0], 8),
    ([100, 64, 0, 0], 10),
    ([127, 0, 0, 0], 8),
    ([169, 254, 0, 0], 16),
    ([172, 16, 0, 0], 12),
    ([192, 0, 0, 0], 24),
    ([192, 0, 2, 0], 24),
    ([192, 88, 99, 0], 24),
    ([192, 168, 0, 0], 16),
    ([198, 18, 0, 0], 15),
    ([198, 51, 100, 0], 24),
    ([203, 0, 113, 0], 24),
    ([224, 0, 0, 0], 4),
    ([240, 0, 0, 0], 4),
];

// IPv6-Tabelle ohne `::ffff:0:0/96`, das gesondert nach IPv4-Regeln bewertet
// wird. `::/96` umfasst `::` und `::1`. Übergangsbereiche mit eingebetteter
// IPv4 (SIIT, NAT64, Teredo, 6to4) sind als Ganzes speziell.
const V6_SPECIAL: [([u16; 8], u32); 12] = [
    ([0, 0, 0, 0, 0, 0, 0, 0], 96),
    ([0, 0, 0, 0, 0xffff, 0, 0, 0], 96),
    ([0x64, 0xff9b, 0, 0, 0, 0, 0, 0], 96),
    ([0x64, 0xff9b, 1, 0, 0, 0, 0, 0], 48),
    ([0x100, 0, 0, 0, 0, 0, 0, 0], 64),
    ([0x2001, 0, 0, 0, 0, 0, 0, 0], 32),
    ([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32),
    ([0x2002, 0, 0, 0, 0, 0, 0, 0], 16),
    ([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7),
    ([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10),
    ([0xfec0, 0, 0, 0, 0, 0, 0, 0], 10),
    ([0xff00, 0, 0, 0, 0, 0, 0, 0], 8),
];

fn v4_is_private_or_special(addr: Ipv4Addr) -> bool {
    V4_SPECIAL.iter().any(|(net, len)| v4_in(addr, *net, *len))
}

fn v6_is_private_or_special(addr: Ipv6Addr) -> bool {
    if let Some(v4) = v4_mapped(addr) {
        return v4_is_private_or_special(v4);
    }
    V6_SPECIAL.iter().any(|(net, len)| v6_in(addr, *net, *len))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BACKSLASH_AT: &str = "https://evil.com\\@docs.rs/";
    const BACKSLASH_DOT: &str = "https://evil.com\\.docs.rs/";

    fn parse(input: &str) -> Result<EgressUrl, EgressUrlError> {
        EgressUrl::parse(input)
    }

    fn host_of(input: &str) -> String {
        match parse(input) {
            Ok(url) => url.host_str(),
            Err(err) => panic!("{input:?} muss parsebar sein: {err:?}"),
        }
    }

    fn ip(raw: &str) -> EgressHost {
        EgressHost::Ip(raw.parse().expect("gültige Testadresse"))
    }

    fn domain(name: &str) -> EgressHost {
        EgressHost::Domain(name.to_owned())
    }

    // --- Parser-Differenzen (F-002) -----------------------------------------

    #[test]
    fn backslash_before_at_ends_authority() {
        let host = host_of(BACKSLASH_AT);
        assert_eq!(host, "evil.com");
        assert!(!host_matches_suffix("docs.rs", &host));
    }

    #[test]
    fn backslash_before_dot_suffix_ends_authority() {
        let host = host_of(BACKSLASH_DOT);
        assert_eq!(host, "evil.com");
        assert!(!host_matches_suffix("docs.rs", &host));
    }

    #[test]
    fn as_url_host_agrees_with_checked_host() {
        let url = parse(BACKSLASH_AT).unwrap();
        assert_eq!(url.as_url().host_str(), Some("evil.com"));
    }

    // --- IPv6 (F-167) -------------------------------------------------------

    #[test]
    fn ipv6_literal_with_port() {
        let url = parse("https://[::1]:8080/").unwrap();
        assert_eq!(url.host(), &ip("::1"));
        assert_eq!(url.host_str(), "::1");
        assert_eq!(url.port(), 8080);
        assert!(url.is_https());
        assert!(url.host().is_loopback());
    }

    #[test]
    fn ipv4_whatwg_shorthand_becomes_address() {
        let loopback = ip("127.0.0.1");
        assert_eq!(parse("http://0x7f.1/").unwrap().host(), &loopback);
        assert_eq!(parse("http://2130706433/").unwrap().host(), &loopback);
    }

    // --- Normalisierung -----------------------------------------------------

    #[test]
    fn userinfo_is_rejected() {
        let inputs = [
            "https://user:pw@docs.rs/",
            "https://user@docs.rs/",
            "https://:pw@docs.rs/",
            "https://docs.rs:443@evil.com/",
        ];
        for input in inputs {
            let result = parse(input);
            assert_eq!(result, Err(EgressUrlError::UserinfoPresent), "{input}");
        }
    }

    #[test]
    fn empty_userinfo_marker_is_not_userinfo() {
        // WHATWG verwirft ein leeres `@`-Präfix ohne Benutzername und
        // Passwort; es bleibt keine Userinfo übrig.
        assert_eq!(host_of("https://:@docs.rs/"), "docs.rs");
    }

    #[test]
    fn idna_becomes_punycode() {
        assert_eq!(host_of("https://bücher.de/"), "xn--bcher-kva.de");
        assert_eq!(host_of("https://BÜCHER.de/"), "xn--bcher-kva.de");
    }

    #[test]
    fn trailing_dot_is_removed() {
        let invalid = Err(EgressUrlError::InvalidHost);
        assert_eq!(host_of("https://docs.rs./x"), "docs.rs");
        assert_eq!(parse("https://docs.rs../"), invalid);
        assert_eq!(parse("https://a..b/"), invalid);
    }

    #[test]
    fn uppercase_is_lowered() {
        let url = parse("HTTPS://Docs.RS:8443/Path").unwrap();
        assert_eq!(url.host_str(), "docs.rs");
        assert_eq!(url.port(), 8443);
        assert!(url.is_https());
    }

    #[test]
    fn percent_encoded_host_is_decoded() {
        assert_eq!(host_of("https://%64ocs.rs/"), "docs.rs");
        // Ein kodierter Schrägstrich bleibt ein verbotenes Host-Zeichen.
        let encoded_slash = parse("https://evil.com%2F.docs.rs/");
        assert_eq!(encoded_slash, Err(EgressUrlError::Parse));
    }

    #[test]
    fn default_ports() {
        let http = parse("http://docs.rs/").unwrap();
        let https = parse("https://docs.rs/").unwrap();
        assert_eq!(http.port(), 80);
        assert_eq!(https.port(), 443);
        assert!(!http.is_https());
        assert!(https.is_https());
    }

    #[test]
    fn unsupported_or_missing_scheme_is_rejected() {
        let ftp = Err(EgressUrlError::UnsupportedScheme("ftp".to_owned()));
        assert_eq!(parse("ftp://docs.rs/"), ftp);
        let file = parse("file:///etc/passwd");
        assert!(matches!(file, Err(EgressUrlError::UnsupportedScheme(_))));
        assert_eq!(parse("docs.rs/x"), Err(EgressUrlError::Parse));
        assert_eq!(parse(""), Err(EgressUrlError::Parse));
        assert_eq!(parse("https://"), Err(EgressUrlError::Parse));
    }

    #[test]
    fn error_display_does_not_echo_input() {
        let text = parse("https://geheim:passwort@docs.rs/")
            .unwrap_err()
            .to_string();
        assert!(!text.contains("geheim"), "{text}");
        assert!(!text.contains("passwort"), "{text}");

        let scheme = parse("gopher://docs.rs/").unwrap_err().to_string();
        assert!(!scheme.contains("gopher"), "{scheme}");
    }

    // --- Klassifikation -----------------------------------------------------

    #[test]
    fn classification_table() {
        let special = [
            "127.0.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "255.255.255.255",
            "192.0.2.1",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.251",
            "::1",
            "::",
            "fd00:ec2::254",
            "fc00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a00:1",
            "2001:db8::1",
            "ff02::1",
            // R2-06: 6to4-Relay, SIIT, NAT64 Local-Use, Teredo, 6to4.
            "192.88.99.1",
            "::ffff:192.88.99.1",
            "::ffff:0:a00:1",
            "::ffff:0:808:808",
            "64:ff9b:1::a00:1",
            "2001::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001:0:ffff:ffff:ffff:ffff:ffff:ffff",
            "2002:a00:1::",
            "2002:7f00:1::1",
            "2002:a9fe:a9fe::",
            "2002:808:808::1",
        ];
        for raw in special {
            let host = ip(raw);
            assert!(host.is_private_or_special(), "{raw} muss speziell sein");
        }

        let public = [
            "8.8.8.8",
            "1.1.1.1",
            "172.32.0.1",
            "100.128.0.1",
            "169.255.0.1",
            "11.0.0.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
            "fe00::1",
            // R2-06: Grenzen der neuen Bereiche.
            "192.88.98.255",
            "192.88.100.1",
            "2001:1::1",
            "2001:4860:4860::8888",
            "2003::1",
            "2001:dc0::1",
            "64:ff9b:2::1",
            "::fffe:0:a00:1",
        ];
        for raw in public {
            let host = ip(raw);
            assert!(!host.is_private_or_special(), "{raw} muss öffentlich sein");
        }
    }

    #[test]
    fn loopback_classification() {
        assert!(domain("localhost").is_loopback());
        assert!(domain("api.localhost").is_loopback());
        assert!(!domain("notlocalhost").is_loopback());
        assert!(!domain("localhost.evil.com").is_loopback());
        assert!(domain("localhost").is_private_or_special());
        assert!(!domain("docs.rs").is_private_or_special());
        // R2-05: frei konstruierte, nicht normalisierte Domains.
        for name in [
            "LOCALHOST",
            "localhost.",
            "LocalHost.",
            "Api.LOCALHOST",
            "a.localhost.",
        ] {
            assert!(domain(name).is_loopback(), "{name}");
            assert!(domain(name).is_private_or_special(), "{name}");
        }
        for name in [
            "localhost..",
            "notlocalhost.",
            "LOCALHOST.evil.com",
            "xlocalhost",
            ".",
        ] {
            assert!(!domain(name).is_loopback(), "{name}");
        }
        // Schnittpunkt mitten in `ö`: kein Panic, kein Treffer.
        assert!(!domain("äölocalhost").is_loopback());
        assert!(ip("127.8.9.10").is_loopback());
        assert!(ip("::ffff:127.0.0.1").is_loopback());
        assert!(!ip("10.0.0.1").is_loopback());
        assert_eq!(host_of("http://LOCALHOST./"), "localhost");

        let mapped = parse("http://[::ffff:10.0.0.1]/").unwrap();
        assert!(mapped.host().is_private_or_special());
        assert!(!mapped.host().is_loopback());
    }

    // --- Suffix-Regel -------------------------------------------------------

    #[test]
    fn suffix_boundaries() {
        assert!(host_matches_suffix("docs.rs", "docs.rs"));
        assert!(host_matches_suffix("docs.rs", "static.docs.rs"));
        assert!(host_matches_suffix("docs.rs", "DOCS.RS"));
        assert!(host_matches_suffix("DOCS.rs", "a.b.docs.rs"));
        assert!(host_matches_suffix("docs.rs.", "docs.rs"));
        assert!(host_matches_suffix("docs.rs", "static.docs.rs."));
        assert!(!host_matches_suffix("docs.rs", "notdocs.rs"));
        assert!(!host_matches_suffix("docs.rs", "evildocs.rs"));
        assert!(!host_matches_suffix("docs.rs", "docs.rs.evil.com"));
        assert!(!host_matches_suffix("docs.rs", "rs"));
        assert!(!host_matches_suffix("", "docs.rs"));
        assert!(!host_matches_suffix(".", "docs.rs"));
        assert!(!host_matches_suffix("docs.rs", ""));
        assert!(!host_matches_suffix("docs.rs", "docs.rs.."));
        // Schnittpunkt mitten in einem Mehrbyte-Zeichen: kein Panic, kein Treffer.
        assert!(!host_matches_suffix("xs", "äs"));
        assert!(!host_matches_suffix("s", "äs"));
    }

    // R2-07: IP-Literale nur exakt, nie per Suffix.
    #[test]
    fn ip_literals_match_only_exactly() {
        assert!(!host_matches_suffix("0.0.1", "10.0.0.1"));
        assert!(!host_matches_suffix("0.1", "10.0.0.1"));
        assert!(!host_matches_suffix("1", "10.0.0.1"));
        assert!(!host_matches_suffix("0.0.1", "::ffff:10.0.0.1"));
        assert!(!host_matches_suffix("10.0.0.1", "::ffff:10.0.0.1"));
        assert!(!host_matches_suffix("0.0.1", "10.0.0.1."));
        assert!(host_matches_suffix("10.0.0.1", "10.0.0.1"));
        assert!(host_matches_suffix("10.0.0.1.", "10.0.0.1"));
        assert!(host_matches_suffix("::1", "0:0::1"));
        assert!(host_matches_suffix("2001:DB8::1", "2001:db8::1"));
        assert!(!host_matches_suffix("::1", "::2"));
        // IP-Eintrag gegen Namen und umgekehrt: nie.
        assert!(!host_matches_suffix("1.2.3.4", "a.1.2.3.4"));
        assert!(!host_matches_suffix("rs", "1.2.3.4"));
        // Domains behalten die Suffix-Regel, auch mit Ziffern-Labels.
        assert!(host_matches_suffix("0.0.1", "a.0.0.1"));
        assert!(host_matches_suffix("example.com", "10.example.com"));
    }
}
