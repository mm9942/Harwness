//! Egress-Policy: Host-Allowlist plus Adressklassen-Regel.
//!
//! # Verantwortung
//! [`EgressPolicy`] entscheidet für URLs ([`EgressPolicy::check_url`]) und
//! aufgelöste Socket-Adressen ([`EgressPolicy::check_addr`]), ob eine
//! ausgehende Verbindung erlaubt ist, und liefert einen stabilen Digest über
//! ihre kanonische Form ([`EgressPolicy::digest`]).
//!
//! # Regeln
//! - **Allowlist:** Ein Host ist erlaubt, wenn er per
//!   [`host_matches_suffix`] auf einen Eintrag passt (`docs.rs` trifft
//!   `docs.rs` und `*.docs.rs`, nie `notdocs.rs`; IP-Einträge nur exakt). Eine
//!   leere Allowlist erlaubt nichts.
//! - **Adressklassen:** [`AddrClass::Public`] ist immer erlaubt. Mit
//!   `allow_private` zusätzlich [`AddrClass::Loopback`],
//!   [`AddrClass::Private`], [`AddrClass::Cgnat`] und
//!   [`AddrClass::UniqueLocal`]. Nie erlaubt: Cloud-Metadaten, Link-Local,
//!   Multicast, Dokumentation, Benchmark, Reserved, Unspecified, Broadcast —
//!   auch nicht, wenn die Adresse wörtlich in der Allowlist steht.
//! - **Lokale Namen:** `localhost`/`*.localhost` gelten ohne `allow_private`
//!   als abgelehnt, bevor überhaupt aufgelöst wird.
//! - **Offenes öffentliches Web** ([`EgressPolicy::with_open_public`],
//!   `[network].research_web = "open"`): zusätzlich zur Allowlist ist jeder
//!   öffentliche DNS-Name erlaubt ([`harw_authority::is_public_dns_name`]:
//!   nie IP-Literale, lokale oder reservierte Namen). Für einen solchen,
//!   **nicht** gelisteten Namen lässt der Resolver ausschließlich
//!   [`AddrClass::Public`] zu — auch mit `allow_private`, das nur für
//!   ausdrücklich gelistete Hosts gilt (kein SSRF über DNS ins eigene Netz).
//!
//! # Kanonische Form
//! Allowlist-Einträge werden beim Bau normalisiert (WHATWG-Host-Parser wie
//! bei URLs: IDNA-Punycode, Kleinschreibung, IPv4-Kurzformen, ein
//! abschließender Punkt entfernt), sortiert und dedupliziert. Reihenfolge,
//! Groß-/Kleinschreibung und Duplikate ändern daher weder Verhalten noch
//! Digest.
//!
//! # Nebenläufigkeit
//! Unveränderlich nach dem Bau; `Send + Sync`, als `Arc<EgressPolicy>` teilbar.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use harw_authority::{host_matches_suffix, is_public_dns_name};
use harw_sandbox::{EgressHost, EgressUrl};

use crate::classify::{AddrClass, classify};
use crate::error::EgressError;

// Domänentrenner des Digests; eine neue kanonische Form bekommt eine neue
// Versionsnummer.
const DIGEST_DOMAIN: &[u8] = b"harw:egress-policy:v1\0";

// Anhang des Digests bei offenem öffentlichem Web.
const OPEN_PUBLIC_MARKER: &[u8] = b"\0open-public";

/// Unveränderliche Egress-Policy aus Host-Allowlist und Privat-Schalter.
///
/// # Invarianten
/// `allow_hosts` ist normalisiert, sortiert und frei von Duplikaten; jeder
/// Eintrag ist entweder ein Domain-Name aus `[a-z0-9._-]` ohne leere Labels
/// oder die `std`-Darstellung einer IP-Adresse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressPolicy {
    allow_hosts: Vec<String>,
    allow_private: bool,
    /// Offenes öffentliches Web (siehe Moduldoku); `false` = nur Allowlist.
    open_public: bool,
}

impl EgressPolicy {
    /// Baut eine Policy aus Allowlist-Einträgen und dem Privat-Schalter.
    ///
    /// # Description
    /// Jeder Eintrag ist ein Domain-Name oder DNS-Suffix (`docs.rs`,
    /// `Bücher.de.`), ein IPv4-Literal oder ein IPv6-Literal mit oder ohne
    /// eckige Klammern. Schema, Pfad, Port, Userinfo, Wildcards (`*`),
    /// Prozentkodierung, Leerraum, führende Punkte und leere Labels werden
    /// abgelehnt — ein Konfigurationsfehler soll laut scheitern statt still
    /// etwas anderes zu erlauben.
    ///
    /// # Arguments
    /// - `allow_hosts` (`Vec<String>`): erlaubte Hosts bzw. DNS-Suffixe
    ///   (Eigentum wird übernommen). Leer bedeutet: kein Ziel erlaubt.
    /// - `allow_private` (`bool`): erlaubt zusätzlich Loopback, RFC-1918,
    ///   CGNAT und ULA sowie lokale Namen.
    ///
    /// # Returns
    /// Die kanonische [`EgressPolicy`].
    ///
    /// # Errors
    /// - [`EgressError::InvalidAllowEntry`]: ein Eintrag ist kein gültiger Host.
    ///
    /// # Concurrency
    /// Reine Funktion.
    ///
    /// # Examples
    /// ```rust
    /// use harw_egress::EgressPolicy;
    ///
    /// let a = EgressPolicy::new(vec!["Docs.RS.".into(), "crates.io".into()], false).unwrap();
    /// let b = EgressPolicy::new(vec!["crates.io".into(), "docs.rs".into()], false).unwrap();
    /// assert_eq!(a.digest(), b.digest());
    /// assert!(EgressPolicy::new(vec!["*.docs.rs".into()], false).is_err());
    /// ```
    pub fn new(allow_hosts: Vec<String>, allow_private: bool) -> Result<Self, EgressError> {
        let mut normalized = allow_hosts
            .iter()
            .map(|entry| normalize_allow_entry(entry.as_str()))
            .collect::<Result<Vec<String>, EgressError>>()?;
        normalized.sort();
        normalized.dedup();
        Ok(Self {
            allow_hosts: normalized,
            allow_private,
            open_public: false,
        })
    }

    /// Schaltet das offene öffentliche Web ein oder aus (siehe Moduldoku).
    ///
    /// # Description
    /// Mit `open = true` besteht jeder öffentliche DNS-Name
    /// ([`harw_authority::is_public_dns_name`]) die Host-Prüfung, auch ohne
    /// Allowlist-Eintrag. IP-Literale, `localhost` und lokale bzw.
    /// reservierte Namen bleiben abgelehnt, und die aufgelösten Adressen
    /// eines nicht gelisteten Namens müssen [`AddrClass::Public`] sein.
    ///
    /// # Examples
    /// ```rust
    /// use harw_egress::EgressPolicy;
    ///
    /// let open = EgressPolicy::new(Vec::new(), true).unwrap().with_open_public(true);
    /// assert!(open.check_url("https://www.destatis.de/DE/Home/").is_ok());
    /// assert!(open.check_url("http://127.0.0.1/").is_err());
    /// assert!(open.check_url("http://printer.local/").is_err());
    /// assert!(EgressPolicy::new(Vec::new(), false).unwrap().check_url("https://example.org/").is_err());
    /// ```
    #[must_use]
    pub fn with_open_public(mut self, open: bool) -> Self {
        self.open_public = open;
        self
    }

    /// Ob das offene öffentliche Web eingeschaltet ist.
    #[must_use]
    pub fn open_public(&self) -> bool {
        self.open_public
    }

    /// Ob `host` (normalisiert) ausdrücklich auf der Allowlist steht.
    #[must_use]
    pub fn is_allowlisted(&self, host: &str) -> bool {
        self.allow_hosts
            .iter()
            .any(|allowed| host_matches_suffix(allowed, host))
    }

    /// Die normalisierten, sortierten Allowlist-Einträge.
    #[must_use]
    pub fn allow_hosts(&self) -> &[String] {
        &self.allow_hosts
    }

    /// Ob private Adressklassen und lokale Namen erlaubt sind.
    #[must_use]
    pub fn allow_private(&self) -> bool {
        self.allow_private
    }

    /// Prüft eine URL vollständig, bevor sie gesendet wird.
    ///
    /// # Description
    /// Parst mit [`EgressUrl::parse`] (WHATWG, `http`/`https`, keine
    /// Userinfo). Für IP-Literale wird die Adresse sofort per
    /// [`Self::check_addr`] geprüft — der HTTP-Connector ruft für IP-Literale
    /// keinen Resolver auf. Danach muss der normalisierte Host auf die
    /// Allowlist passen; lokale Namen scheitern ohne `allow_private`. Für
    /// Domain-Namen prüft der Resolver aus [`crate::build_client`] die
    /// aufgelösten Adressen beim Verbindungsaufbau.
    ///
    /// Jedes Redirect-Ziel muss erneut durch diese Funktion.
    ///
    /// # Arguments
    /// - `url` (`&str`): die rohe URL (geborgt).
    ///
    /// # Returns
    /// Die geprüfte [`EgressUrl`]; zu senden ist [`EgressUrl::as_url`].
    ///
    /// # Errors
    /// - [`EgressError::InvalidUrl`]: Parser, Schema, Userinfo oder Host ungültig.
    /// - [`EgressError::AddressDenied`]: IP-Literal einer unzulässigen Klasse.
    /// - [`EgressError::LocalHostName`]: `localhost` ohne `allow_private`.
    /// - [`EgressError::HostNotAllowed`]: Host nicht in der Allowlist.
    ///
    /// # Concurrency
    /// Reine Funktion auf `&self`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_egress::{EgressError, EgressPolicy};
    ///
    /// let policy = EgressPolicy::new(vec!["docs.rs".into()], false).unwrap();
    /// assert!(policy.check_url("https://static.docs.rs/x.css").is_ok());
    /// assert!(matches!(
    ///     policy.check_url("https://evil.com\\@docs.rs/"),
    ///     Err(EgressError::HostNotAllowed { .. })
    /// ));
    /// ```
    pub fn check_url(&self, url: &str) -> Result<EgressUrl, EgressError> {
        let parsed = EgressUrl::parse(url)?;
        if let EgressHost::Ip(ip) = parsed.host() {
            self.check_addr(SocketAddr::new(*ip, parsed.port()))?;
        }
        self.check_host(&parsed.host_str())?;
        Ok(parsed)
    }

    /// Prüft eine konkrete Zieladresse gegen die Adressklassen-Regel.
    ///
    /// # Description
    /// Klassifiziert die IP per [`classify`] (IPv4-gemappt und NAT64 nach
    /// eingebetteter IPv4). Der Port wird nicht bewertet.
    ///
    /// # Arguments
    /// - `addr` (`SocketAddr`): die Adresse, zu der verbunden werden soll.
    ///
    /// # Returns
    /// `Ok(())`, wenn die Klasse erlaubt ist.
    ///
    /// # Errors
    /// - [`EgressError::AddressDenied`]: Klasse unzulässig.
    ///
    /// # Concurrency
    /// Reine Funktion auf `&self`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_egress::EgressPolicy;
    ///
    /// let open = EgressPolicy::new(Vec::new(), true).unwrap();
    /// assert!(open.check_addr("10.0.0.1:443".parse().unwrap()).is_ok());
    /// assert!(open.check_addr("169.254.169.254:80".parse().unwrap()).is_err());
    /// ```
    pub fn check_addr(&self, addr: SocketAddr) -> Result<(), EgressError> {
        let ip = addr.ip();
        let class = classify(ip);
        if class_permitted(class, self.allow_private) {
            Ok(())
        } else {
            Err(EgressError::AddressDenied { addr: ip, class })
        }
    }

    /// Berechnet den BLAKE3-Digest der kanonischen Policy.
    ///
    /// # Description
    /// Eingabe: Domänentrenner `harw:egress-policy:v1\0`, ein Byte
    /// `allow_private` (`0`/`1`), die Anzahl der Einträge als `u64`
    /// little-endian, dann je Eintrag seine Länge als `u64` little-endian und
    /// seine Bytes. Die Längenpräfixe verhindern, dass `["ab","c"]` und
    /// `["a","bc"]` kollidieren.
    ///
    /// # Returns
    /// 32 Bytes BLAKE3.
    ///
    /// # Concurrency
    /// Reine Funktion auf `&self`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_egress::EgressPolicy;
    ///
    /// let strict = EgressPolicy::new(vec!["docs.rs".into()], false).unwrap();
    /// let open = EgressPolicy::new(vec!["docs.rs".into()], true).unwrap();
    /// assert_ne!(strict.digest(), open.digest());
    /// ```
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(DIGEST_DOMAIN);
        hasher.update(&[u8::from(self.allow_private)]);
        hasher.update(&len_le(self.allow_hosts.len()));
        for entry in &self.allow_hosts {
            hasher.update(&len_le(entry.len()));
            hasher.update(entry.as_bytes());
        }
        // Nur bei offenem Web angehängt: bestehende Digests bleiben gleich.
        if self.open_public {
            hasher.update(OPEN_PUBLIC_MARKER);
        }
        hasher.finalize().into()
    }

    /// Prüft eine aufgelöste Adresse **zu ihrem Namen**: für einen nur über
    /// das offene Web zugelassenen Namen ausschließlich [`AddrClass::Public`],
    /// sonst wie [`Self::check_addr`].
    ///
    /// # Errors
    /// [`EgressError::AddressDenied`], wenn die Klasse nicht zulässig ist.
    pub fn check_resolved(&self, host: &str, addr: SocketAddr) -> Result<(), EgressError> {
        if self.open_public && !self.is_allowlisted(host) {
            let class = classify(addr.ip());
            return if class == AddrClass::Public {
                Ok(())
            } else {
                Err(EgressError::AddressDenied {
                    addr: addr.ip(),
                    class,
                })
            };
        }
        self.check_addr(addr)
    }

    // Allowlist- und Lokalnamen-Prüfung für einen bereits normalisierten Host
    // (Domain in Normalform oder IP ohne Klammern). Auch vom Resolver genutzt,
    // der Namen aus der Anfrage-URI erhält.
    pub(crate) fn check_host(&self, host: &str) -> Result<(), EgressError> {
        if !self.allow_private && EgressHost::Domain(host.to_owned()).is_loopback() {
            return Err(EgressError::LocalHostName {
                host: host.to_owned(),
            });
        }
        if self.is_allowlisted(host) || (self.open_public && is_public_dns_name(host)) {
            Ok(())
        } else {
            Err(EgressError::HostNotAllowed {
                host: host.to_owned(),
            })
        }
    }
}

// Adressklassen-Regel, siehe Moduldoku.
const fn class_permitted(class: AddrClass, allow_private: bool) -> bool {
    match class {
        AddrClass::Public => true,
        AddrClass::Loopback | AddrClass::Private | AddrClass::Cgnat | AddrClass::UniqueLocal => {
            allow_private
        }
        AddrClass::CloudMetadata
        | AddrClass::LinkLocal
        | AddrClass::Multicast
        | AddrClass::Documentation
        | AddrClass::Benchmark
        | AddrClass::Reserved
        | AddrClass::Unspecified
        | AddrClass::Broadcast => false,
    }
}

// `usize` → 8 Bytes little-endian; `usize` ist auf allen Zielen ≤ 64 Bit.
fn len_le(len: usize) -> [u8; 8] {
    u64::try_from(len).unwrap_or(u64::MAX).to_le_bytes()
}

fn invalid(entry: &str, reason: &'static str) -> EgressError {
    EgressError::InvalidAllowEntry {
        entry: entry.to_owned(),
        reason,
    }
}

// Normalisiert einen Allowlist-Eintrag, siehe `EgressPolicy::new`.
fn normalize_allow_entry(entry: &str) -> Result<String, EgressError> {
    if entry.is_empty() {
        return Err(invalid(entry, "leerer Eintrag"));
    }
    if let Ok(ip) = entry.parse::<IpAddr>() {
        return Ok(ip.to_string());
    }
    if let Some(inner) = entry
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return inner
            .parse::<Ipv6Addr>()
            .map(|v6| IpAddr::V6(v6).to_string())
            .map_err(|_| invalid(entry, "ungültiges IPv6-Literal"));
    }
    // Vor dem WHATWG-Parser: nur Namenszeichen (Nicht-ASCII für IDNA erlaubt).
    // Das schließt Schema, Pfad, Port, Userinfo, Wildcards, Prozentkodierung
    // und Leerraum aus, die der Host-Parser teils still umdeuten würde.
    let plain = entry
        .chars()
        .all(|c| !c.is_ascii() || c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'));
    if !plain {
        return Err(invalid(
            entry,
            "nur Hostnamen, IP-Adressen oder DNS-Suffixe sind erlaubt",
        ));
    }
    if entry.starts_with('.') {
        return Err(invalid(entry, "führender Punkt"));
    }
    match url::Host::parse(entry) {
        Ok(url::Host::Domain(domain)) => normalize_domain_entry(entry, &domain),
        Ok(url::Host::Ipv4(v4)) => Ok(IpAddr::V4(v4).to_string()),
        Ok(url::Host::Ipv6(v6)) => Ok(IpAddr::V6(v6).to_string()),
        Err(_) => Err(invalid(entry, "kein gültiger Hostname")),
    }
}

// Normalform eines Domain-Eintrags wie `EgressUrl`: ein abschließender Punkt
// weg, keine leeren Labels, nur `[a-z0-9._-]` nach IDNA.
fn normalize_domain_entry(entry: &str, domain: &str) -> Result<String, EgressError> {
    let lowered = domain.to_ascii_lowercase();
    let trimmed = lowered.strip_suffix('.').unwrap_or(&lowered);
    if trimmed.is_empty() || trimmed.split('.').any(str::is_empty) {
        return Err(invalid(entry, "leeres Label"));
    }
    let ascii_name = trimmed
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'_'));
    if !ascii_name {
        return Err(invalid(entry, "kein gültiger Hostname"));
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_sandbox::egress::EgressUrlError;

    fn policy(hosts: &[&str], allow_private: bool) -> TestResult<EgressPolicy> {
        let hosts = hosts.iter().map(|h| (*h).to_owned()).collect();
        EgressPolicy::new(hosts, allow_private).map_err(ctx("gültige Test-Policy"))
    }

    #[test]
    fn test_new_normalizes_sorts_and_dedups() -> TestResult {
        let entries = [
            "Docs.RS.",
            "crates.io",
            "docs.rs",
            "[::1]",
            "0x7f.1",
            "Bücher.de",
        ];
        let p = policy(&entries, true)?;
        assert_eq!(
            p.allow_hosts(),
            [
                "127.0.0.1",
                "::1",
                "crates.io",
                "docs.rs",
                "xn--bcher-kva.de"
            ]
        );
        assert!(p.allow_private());
        Ok(())
    }

    #[test]
    fn test_new_rejects_invalid_entries() {
        let bad = [
            "",
            "*.docs.rs",
            ".docs.rs",
            "docs.rs/x",
            "https://docs.rs",
            "user@docs.rs",
            "docs.rs:443",
            " docs.rs",
            "a..b",
            "docs.rs..",
            "%64ocs.rs",
            "[not-v6]",
            "evil.com\\docs.rs",
        ];
        for entry in bad {
            let result = EgressPolicy::new(vec![entry.to_owned()], false);
            assert!(
                matches!(result, Err(EgressError::InvalidAllowEntry { .. })),
                "{entry:?} muss abgelehnt werden: {result:?}"
            );
        }
    }

    #[test]
    fn test_check_url_backslash_at_targets_evil_host() -> TestResult {
        let p = policy(&["docs.rs"], false)?;
        let result = p.check_url("https://evil.com\\@docs.rs/");
        assert!(
            matches!(&result, Err(EgressError::HostNotAllowed { host }) if host == "evil.com"),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_check_url_rejects_userinfo() -> TestResult {
        let p = policy(&["docs.rs", "evil.com"], false)?;
        let urls = [
            "https://user:pw@docs.rs/",
            "https://docs.rs:443@evil.com/",
            "https://user@docs.rs/",
        ];
        for url in urls {
            let result = p.check_url(url);
            assert!(
                matches!(
                    result,
                    Err(EgressError::InvalidUrl(EgressUrlError::UserinfoPresent))
                ),
                "{url}: {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_check_url_rejects_private_ip_literals_even_if_allowlisted() -> TestResult {
        let p = policy(
            &[
                "10.0.0.1",
                "127.0.0.1",
                "169.254.169.254",
                "::ffff:127.0.0.1",
            ],
            false,
        )?;
        let cases = [
            ("http://10.0.0.1/", AddrClass::Private),
            ("http://127.0.0.1:8080/", AddrClass::Loopback),
            ("http://0x7f.1/", AddrClass::Loopback),
            ("http://2130706433/", AddrClass::Loopback),
            (
                "http://169.254.169.254/latest/meta-data/",
                AddrClass::CloudMetadata,
            ),
            ("http://[::ffff:127.0.0.1]/", AddrClass::Loopback),
            ("http://[64:ff9b::a9fe:a9fe]/", AddrClass::CloudMetadata),
        ];
        for (url, expected) in cases {
            let result = p.check_url(url);
            let denied = matches!(
                result,
                Err(EgressError::AddressDenied { class, .. }) if class == expected
            );
            assert!(denied, "{url}: {result:?}");
        }
        Ok(())
    }

    #[test]
    fn test_check_url_private_ip_literal_with_allow_private() -> TestResult {
        let p = policy(&["10.0.0.1"], true)?;
        let url = p
            .check_url("http://10.0.0.1:8080/v1")
            .map_err(ctx("erlaubt"))?;
        assert_eq!(url.port(), 8080);
        // Metadaten bleiben auch mit `allow_private` gesperrt.
        let meta = policy(&["169.254.169.254"], true)?.check_url("http://169.254.169.254/");
        let meta_denied = matches!(
            meta,
            Err(EgressError::AddressDenied {
                class: AddrClass::CloudMetadata,
                ..
            })
        );
        assert!(meta_denied, "{meta:?}");
        // Ein nicht gelistetes, erlaubtes IP-Literal scheitert an der Allowlist.
        let other = p.check_url("http://10.0.0.2/");
        assert!(matches!(other, Err(EgressError::HostNotAllowed { .. })));
        Ok(())
    }

    #[test]
    fn test_check_url_host_suffix_boundaries() -> TestResult {
        let p = policy(&["docs.rs"], false)?;
        assert_eq!(
            p.check_url("https://docs.rs/")
                .map_err(ctx("exakt"))?
                .host_str(),
            "docs.rs"
        );
        assert!(p.check_url("https://static.docs.rs/a").is_ok());
        assert!(p.check_url("HTTPS://STATIC.DOCS.RS./a").is_ok());
        let urls = [
            "https://notdocs.rs/",
            "https://docs.rs.evil.com/",
            "https://evil.com\\.docs.rs/",
        ];
        for url in urls {
            let result = p.check_url(url);
            assert!(
                matches!(result, Err(EgressError::HostNotAllowed { .. })),
                "{url}: {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_check_url_empty_allowlist_denies_everything() -> TestResult {
        let p = policy(&[], true)?;
        let result = p.check_url("https://docs.rs/");
        assert!(matches!(result, Err(EgressError::HostNotAllowed { .. })));
        Ok(())
    }

    #[test]
    fn test_check_url_localhost_requires_allow_private() -> TestResult {
        let strict = policy(&["localhost"], false)?;
        let result = strict.check_url("http://api.localhost:3000/");
        assert!(
            matches!(result, Err(EgressError::LocalHostName { .. })),
            "{result:?}"
        );
        let open = policy(&["localhost"], true)?;
        assert!(open.check_url("http://api.localhost:3000/").is_ok());
        Ok(())
    }

    #[test]
    fn test_check_url_rejects_non_http_scheme() -> TestResult {
        let p = policy(&["docs.rs"], false)?;
        let result = p.check_url("file:///etc/passwd");
        let unsupported = matches!(
            result,
            Err(EgressError::InvalidUrl(EgressUrlError::UnsupportedScheme(
                _
            )))
        );
        assert!(unsupported, "{result:?}");
        Ok(())
    }

    #[test]
    fn test_check_addr_class_matrix() -> TestResult {
        let strict = policy(&[], false)?;
        let open = policy(&[], true)?;
        let cases = [
            ("8.8.8.8:443", true, true),
            ("[2606:4700:4700::1111]:443", true, true),
            ("127.0.0.1:80", false, true),
            ("10.0.0.1:80", false, true),
            ("100.64.0.1:80", false, true),
            ("[fd12::1]:80", false, true),
            ("[::ffff:192.168.1.1]:80", false, true),
            ("169.254.169.254:80", false, false),
            ("100.100.100.200:80", false, false),
            ("[fd00:ec2::254]:80", false, false),
            ("169.254.1.1:80", false, false),
            ("[fe80::1]:80", false, false),
            ("224.0.0.1:80", false, false),
            ("0.0.0.0:80", false, false),
            ("255.255.255.255:80", false, false),
            ("192.0.2.1:80", false, false),
            ("198.18.0.1:80", false, false),
            ("[2002:a00:1::]:80", false, false),
        ];
        for (raw, strict_ok, open_ok) in cases {
            let addr: SocketAddr = raw.parse().map_err(ctx("gültige Socket-Adresse"))?;
            assert_eq!(strict.check_addr(addr).is_ok(), strict_ok, "strict {raw}");
            assert_eq!(open.check_addr(addr).is_ok(), open_ok, "open {raw}");
        }
        Ok(())
    }

    #[test]
    fn test_open_public_admits_public_names_but_never_local_targets() -> TestResult {
        let open = policy(&["intranet.example.test"], true)?.with_open_public(true);
        assert!(open.open_public());
        for url in [
            "https://www.destatis.de/DE/Home/",
            "https://docs.rs/serde",
            "https://github.com/rust-lang/rust/blob/master/LICENSE-MIT",
        ] {
            assert!(open.check_url(url).is_ok(), "{url}");
        }
        for url in [
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://printer.local/",
            "http://svc.internal/",
            "http://intranet/",
        ] {
            assert!(open.check_url(url).is_err(), "{url}");
        }
        // Ohne `with_open_public` bleibt alles wie bisher.
        let closed = policy(&[], false)?;
        assert!(!closed.open_public());
        assert!(closed.check_url("https://www.destatis.de/").is_err());
        Ok(())
    }

    #[test]
    fn test_open_public_names_resolve_only_to_public_addresses() -> TestResult {
        // `allow_private` gilt nur für gelistete Hosts, nie für offene Namen.
        let open = policy(&["intranet.example.test"], true)?.with_open_public(true);
        let private: SocketAddr = "10.0.0.7:443".parse().map_err(ctx("Adresse"))?;
        let loopback: SocketAddr = "127.0.0.1:443".parse().map_err(ctx("Adresse"))?;
        let public: SocketAddr = "8.8.8.8:443".parse().map_err(ctx("Adresse"))?;
        assert!(open.check_resolved("www.destatis.de", public).is_ok());
        assert!(open.check_resolved("rebind.example.org", private).is_err());
        assert!(open.check_resolved("rebind.example.org", loopback).is_err());
        assert!(
            open.check_resolved("intranet.example.test", private)
                .is_ok()
        );
        let resolved = crate::client::filter_resolved(&open, "rebind.example.org", [private]);
        assert!(matches!(
            resolved,
            Err(EgressError::NoPermittedAddress { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_open_public_changes_the_digest_only_when_on() -> TestResult {
        let base = policy(&["docs.rs"], false)?;
        assert_eq!(base.digest(), base.clone().with_open_public(false).digest());
        assert_ne!(base.digest(), base.clone().with_open_public(true).digest());
        Ok(())
    }

    #[test]
    fn test_digest_is_stable_under_normalization() -> TestResult {
        let a = policy(&["docs.rs", "crates.io", "static.crates.io"], false)?;
        let b = policy(
            &["STATIC.crates.io.", "Docs.RS", "crates.io", "docs.rs"],
            false,
        )?;
        assert_eq!(a, b);
        assert_eq!(a.digest(), b.digest());
        assert_eq!(a.digest(), a.clone().digest());
        Ok(())
    }

    #[test]
    fn test_digest_distinguishes_policies() -> TestResult {
        let base = policy(&["docs.rs"], false)?;
        assert_ne!(base.digest(), policy(&["docs.rs"], true)?.digest());
        assert_ne!(base.digest(), policy(&["crates.io"], false)?.digest());
        assert_ne!(base.digest(), policy(&[], false)?.digest());
        // Längenpräfixe: gleiche Konkatenation, andere Einträge.
        assert_ne!(
            policy(&["ab", "c"], false)?.digest(),
            policy(&["a", "bc"], false)?.digest()
        );
        Ok(())
    }

    #[test]
    fn test_digest_matches_documented_encoding() -> TestResult {
        let p = policy(&["docs.rs"], true)?;
        let mut expected = Vec::new();
        expected.extend_from_slice(b"harw:egress-policy:v1\0");
        expected.push(1);
        expected.extend_from_slice(&1_u64.to_le_bytes());
        expected.extend_from_slice(&7_u64.to_le_bytes());
        expected.extend_from_slice(b"docs.rs");
        assert_eq!(p.digest(), *blake3::hash(&expected).as_bytes());
        Ok(())
    }
}
