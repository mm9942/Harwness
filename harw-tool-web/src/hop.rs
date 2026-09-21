//! Egress-Prüfung je Anfrage-Hop (Start-URL und jede Weiterleitung).
//!
//! # Verantwortung
//! Dieses Modul entscheidet, ob eine konkrete URL gesendet werden darf (F-035,
//! F-062). [`check_hop`] verknüpft drei unabhängige Grenzen, die **alle**
//! bestehen müssen:
//! 1. **Schema:** `https`; `http` nur mit ausdrücklicher Freigabe.
//! 2. **Egress-Policy:** [`harw_egress::EgressPolicy::check_url`] — Allowlist,
//!    lokale Namen und Adressklassen. Für IP-Literale ist das die **einzige**
//!    Adressprüfung: der HTTP-Connector ruft für IP-Literale keinen Resolver
//!    auf (hyper-util `SocketAddrs::try_parse`, siehe Ledger C-EGRESS §4). Die
//!    Prüfung muss deshalb vor jedem Senden und für jedes Redirect-Ziel laufen.
//! 3. **Sandbox-Scope:** [`harw_authority::NetworkScope::allows`] des aktiven
//!    Tool-Aufrufs.
//!
//! [`map_send_error`] findet Ablehnungen des Scoped-Resolvers aus
//! [`harw_egress::build_client`] in der `source()`-Kette eines
//! `reqwest::Error` wieder, damit sie als Ablehnung (nicht als
//! Transportfehler mit Cache-Fail-open) gemeldet werden.
//!
//! # Fehlermeldungen ohne interne Adressen
//! Begründungen nennen Hostnamen und Adressklassen, nie IP-Adressen. URLs aus
//! `Location`-Headern erscheinen nie in Meldungen (Platzhalter
//! `<Weiterleitung n>`); `reqwest`-Fehler verlieren ihre URL (`without_url`).
//!
//! # Nebenläufigkeit
//! Reine Funktionen ohne Zustand; von jedem Thread aufrufbar.
//!
//! # Examples
//! ```rust
//! use harw_egress::EgressPolicy;
//! use harw_authority::NetworkScope;
//! use harw_tool_web::hop::check_hop;
//!
//! let policy = EgressPolicy::new(vec!["docs.rs".to_owned()], false).unwrap();
//! let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
//! assert!(check_hop(&policy, &scope, false, "https://docs.rs/serde/", 0).is_ok());
//! assert!(check_hop(&policy, &scope, false, "https://127.0.0.1/", 0).is_err());
//! ```

use crate::error::{WebToolError, WebToolResult};
use harw_egress::{EgressError, EgressPolicy, EgressUrl, EgressUrlError};
use harw_authority::NetworkScope;
use std::net::IpAddr;

/// Ein geprüftes Anfrageziel.
///
/// # Description
/// Entsteht ausschließlich über [`check_hop`]; `url` ist die vom WHATWG-Parser
/// normalisierte Form, die tatsächlich gesendet wird.
///
/// # Concurrency
/// Reine Daten; `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HopTarget {
    url: String,
    host: String,
}

impl HopTarget {
    /// Die normalisierte, geprüfte URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Der normalisierte Host (ohne Port).
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
}

/// Liefert die in Fehlermeldungen zulässige Bezeichnung eines Hops.
///
/// Hop `0` ist die vom Aufrufer gelieferte URL; jedes spätere Ziel stammt vom
/// Server und wird nur als Platzhalter genannt.
fn hop_label(url: &str, hop: usize) -> String {
    if hop == 0 {
        url.to_owned()
    } else {
        format!("<Weiterleitung {hop}>")
    }
}

/// Ersetzt IP-Literale in Meldungen durch einen neutralen Platzhalter.
fn redact_host(host: &str) -> String {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<IpAddr>().is_ok() {
        "<IP-Literal>".to_owned()
    } else {
        host.to_owned()
    }
}

/// Prüft eine URL vollständig, bevor sie gesendet wird.
///
/// # Description
/// Reihenfolge: Parsen ([`EgressUrl::parse`]) → Schema (`https`, `http` nur
/// mit `allow_http`) → [`EgressPolicy::check_url`] (Allowlist, lokale Namen,
/// Adressklasse von IP-Literalen) → [`NetworkScope::allows`] des
/// Sandbox-Aufrufs. Muss für die Start-URL, jedes Redirect-Ziel und jede
/// URL eines Cache-Treffers aufgerufen werden.
///
/// # Arguments
/// - `policy` (`&EgressPolicy`): prozessweite Egress-Policy (geborgt).
/// - `scope` (`&NetworkScope`): Scope der aktiven Sandbox (geborgt).
/// - `allow_http` (`bool`): `http://` zusätzlich zu `https://` erlauben.
/// - `url` (`&str`): die zu prüfende URL; umgebender Leerraum wird entfernt.
/// - `hop` (`usize`): `0` für die Start-URL, `n` für die n-te Weiterleitung
///   (steuert nur die Fehlermeldung).
///
/// # Returns
/// Das geprüfte [`HopTarget`].
///
/// # Errors
/// - [`WebToolError::SchemeNotAllowed`]: Schema nicht erlaubt.
/// - [`WebToolError::HostNotResolvable`]: URL unparsebar, Userinfo, kein Host.
/// - [`WebToolError::EgressDenied`]: Policy lehnt Host oder Adressklasse ab.
/// - [`WebToolError::RedirectHostNotAllowed`]: Host außerhalb des Sandbox-Scopes.
///
/// # Concurrency
/// Rein; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_egress::EgressPolicy;
/// use harw_authority::NetworkScope;
/// use harw_tool_web::{WebToolError, hop::check_hop};
///
/// let policy = EgressPolicy::new(vec!["docs.rs".to_owned()], false).unwrap();
/// let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
/// let err = check_hop(&policy, &scope, false, "http://docs.rs/", 0).unwrap_err();
/// assert!(matches!(err, WebToolError::SchemeNotAllowed { .. }));
/// ```
pub fn check_hop(
    policy: &EgressPolicy,
    scope: &NetworkScope,
    allow_http: bool,
    url: &str,
    hop: usize,
) -> WebToolResult<HopTarget> {
    let trimmed = url.trim();
    let label = hop_label(trimmed, hop);

    let parsed = EgressUrl::parse(trimmed).map_err(|error| url_error(&error, &label))?;
    let scheme_ok = parsed.is_https() || (allow_http && parsed.as_url().scheme() == "http");
    if !scheme_ok {
        return Err(WebToolError::SchemeNotAllowed { url: label });
    }

    let checked = policy
        .check_url(trimmed)
        .map_err(|error| egress_denial(&error, &label))?;

    let host = checked.host_str();
    if !scope.allows(&host) {
        return Err(WebToolError::RedirectHostNotAllowed {
            url: label,
            host: redact_host(&host),
        });
    }

    Ok(HopTarget {
        url: checked.as_url().as_str().to_owned(),
        host,
    })
}

/// Löst einen `Location`-Header gegen die aktuelle URL auf.
///
/// # Description
/// Relative und absolute Ziele werden per WHATWG `join` aufgelöst. Das
/// Ergebnis ist **ungeprüft** und muss durch [`check_hop`].
///
/// # Arguments
/// - `current` (`&str`): die bereits geprüfte URL des aktuellen Hops.
/// - `location` (`&str`): der rohe `Location`-Wert.
/// - `hop` (`usize`): Nummer der Weiterleitung, die daraus entsteht.
///
/// # Returns
/// Die absolute Ziel-URL als `String`.
///
/// # Errors
/// - [`WebToolError::HostNotResolvable`]: Basis oder Ziel unparsebar.
///
/// # Concurrency
/// Rein; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_tool_web::hop::resolve_location;
///
/// let next = resolve_location("https://docs.rs/a/b", "../c", 1).unwrap();
/// assert_eq!(next, "https://docs.rs/c");
/// ```
pub fn resolve_location(current: &str, location: &str, hop: usize) -> WebToolResult<String> {
    let label = hop_label(location, hop);
    let base = reqwest::Url::parse(current)
        .map_err(|_| WebToolError::HostNotResolvable { url: label.clone() })?;
    let next = base
        .join(location.trim())
        .map_err(|_| WebToolError::HostNotResolvable { url: label })?;
    Ok(next.to_string())
}

/// Übersetzt einen URL-Parserfehler in eine Ablehnung.
fn url_error(error: &EgressUrlError, label: &str) -> WebToolError {
    match error {
        EgressUrlError::UnsupportedScheme(_) => WebToolError::SchemeNotAllowed {
            url: label.to_owned(),
        },
        EgressUrlError::Parse
        | EgressUrlError::UserinfoPresent
        | EgressUrlError::MissingHost
        | EgressUrlError::InvalidHost => WebToolError::HostNotResolvable {
            url: label.to_owned(),
        },
    }
}

/// Übersetzt eine Egress-Ablehnung in eine adressfreie Tool-Meldung.
fn egress_denial(error: &EgressError, label: &str) -> WebToolError {
    let reason = match error {
        EgressError::InvalidUrl(inner) => return url_error(inner, label),
        EgressError::HostNotAllowed { host } => {
            format!("Host {} ist nicht in der Egress-Allowlist", redact_host(host))
        }
        EgressError::LocalHostName { .. } => {
            "lokale Hostnamen sind ohne Freigabe privater Ziele nicht erlaubt".to_owned()
        }
        EgressError::AddressDenied { class, .. } => {
            format!("Zieladresse der Klasse {class} ist nicht erlaubt")
        }
        EgressError::NoPermittedAddress { host, .. } => format!(
            "die DNS-Auflösung von {} lieferte keine zulässige Adresse",
            redact_host(host)
        ),
        EgressError::Lookup { host, .. } => {
            format!("die DNS-Auflösung von {} ist fehlgeschlagen", redact_host(host))
        }
        EgressError::InvalidAllowEntry { .. } => {
            return WebToolError::NotConfigured {
                what: "ungültiger Eintrag in der Egress-Allowlist",
            };
        }
        EgressError::ClientBuild(_) => {
            return WebToolError::NotConfigured {
                what: "der Egress-HTTP-Client ließ sich nicht bauen",
            };
        }
    };
    WebToolError::EgressDenied { reason }
}

/// Sucht eine Resolver-Ablehnung in der `source()`-Kette eines Sendefehlers.
///
/// `EgressError::Lookup` (DNS nicht erreichbar) bleibt ein Transportfehler.
fn denial_in_chain(error: &reqwest::Error, hop: usize) -> Option<WebToolError> {
    let label = format!("<Hop {hop}>");
    let mut cause = std::error::Error::source(error);
    while let Some(current) = cause {
        if let Some(egress) = current.downcast_ref::<EgressError>() {
            return match egress {
                EgressError::Lookup { .. } => None,
                other => Some(egress_denial(other, &label)),
            };
        }
        cause = current.source();
    }
    None
}

/// Übersetzt einen `reqwest`-Sendefehler.
///
/// # Description
/// Eine Ablehnung des Scoped-Resolvers (Allowlist, Adressklasse,
/// DNS-Rebinding auf private Adressen) wird zu
/// [`WebToolError::EgressDenied`] und damit **kein** Transportfehler — ein
/// veralteter Cache-Eintrag darf sie nicht überdecken. Alles andere wird zu
/// [`WebToolError::Http`] ohne URL im `Display`.
///
/// # Arguments
/// - `error` (`reqwest::Error`): der Sendefehler (Eigentum geht über).
/// - `hop` (`usize`): Nummer des Hops (nur für Meldungen).
///
/// # Returns
/// Den übersetzten [`WebToolError`].
///
/// # Concurrency
/// Rein; von jedem Thread aufrufbar.
#[must_use]
pub fn map_send_error(error: reqwest::Error, hop: usize) -> WebToolError {
    let denial = denial_in_chain(&error, hop);
    denial.unwrap_or_else(|| WebToolError::Http(error.without_url()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(hosts: &[&str], allow_private: bool) -> EgressPolicy {
        let hosts = hosts.iter().map(|host| (*host).to_owned()).collect();
        EgressPolicy::new(hosts, allow_private).expect("gültige Test-Policy")
    }

    fn scope(hosts: &[&str]) -> NetworkScope {
        NetworkScope::from_hosts(hosts.iter().map(|host| (*host).to_owned()))
    }

    /// Erlaubte https-URL wird normalisiert durchgelassen.
    #[test]
    fn test_check_hop_accepts_allowed_https_url() {
        let target = check_hop(
            &policy(&["docs.rs"], false),
            &scope(&["docs.rs"]),
            false,
            " HTTPS://Static.Docs.RS/x ",
            0,
        )
        .expect("erlaubt");
        assert_eq!(target.url(), "https://static.docs.rs/x");
        assert_eq!(target.host(), "static.docs.rs");
    }

    /// IP-Literale werden per Adressklasse abgelehnt, auch wenn sie in der
    /// Allowlist stehen; die Meldung nennt keine Adresse.
    #[test]
    fn test_check_hop_rejects_private_ip_literals_without_leaking_address() {
        let policy = policy(&["127.0.0.1", "10.0.0.5", "docs.rs"], false);
        let scope = scope(&["127.0.0.1", "10.0.0.5", "docs.rs"]);
        for url in [
            "https://127.0.0.1/",
            "https://10.0.0.5/admin",
            "https://[::ffff:127.0.0.1]/",
            "https://2130706433/",
        ] {
            let err = check_hop(&policy, &scope, false, url, 1).expect_err(url);
            assert!(matches!(err, WebToolError::EgressDenied { .. }), "{url}: {err:?}");
            let message = err.to_string();
            for leaked in ["127.0.0.1", "10.0.0.5", "2130706433", "::ffff"] {
                assert!(!message.contains(leaked), "{url}: Adresse in {message}");
            }
        }
    }

    /// Cloud-Metadaten bleiben auch mit `allow_private` gesperrt.
    #[test]
    fn test_check_hop_rejects_metadata_even_with_allow_private() {
        let err = check_hop(
            &policy(&["169.254.169.254"], true),
            &scope(&["169.254.169.254"]),
            true,
            "http://169.254.169.254/latest/meta-data/",
            2,
        )
        .expect_err("Metadaten-Endpunkt");
        match err {
            WebToolError::EgressDenied { reason } => {
                assert!(reason.contains("cloud-metadata"), "{reason}");
                assert!(!reason.contains("169.254"), "{reason}");
            }
            other => panic!("unerwartet: {other:?}"),
        }
    }

    /// Host außerhalb der Policy wird abgelehnt, bevor der Scope zählt.
    #[test]
    fn test_check_hop_rejects_host_outside_policy() {
        let err = check_hop(
            &policy(&["docs.rs"], false),
            &scope(&["docs.rs", "evil.test"]),
            false,
            "https://evil.test/",
            1,
        )
        .expect_err("nicht in der Policy");
        assert!(matches!(err, WebToolError::EgressDenied { .. }), "{err:?}");
    }

    /// Policy erlaubt, Sandbox-Scope nicht → abgelehnt.
    #[test]
    fn test_check_hop_rejects_host_outside_sandbox_scope() {
        let err = check_hop(
            &policy(&["docs.rs", "crates.io"], false),
            &scope(&["docs.rs"]),
            false,
            "https://crates.io/",
            0,
        )
        .expect_err("nicht im Scope");
        assert!(matches!(err, WebToolError::RedirectHostNotAllowed { .. }), "{err:?}");
    }

    /// `http://` nur mit Freigabe; andere Schemata nie.
    #[test]
    fn test_check_hop_http_requires_explicit_opt_in() {
        let policy = policy(&["docs.rs"], false);
        let scope = scope(&["docs.rs"]);
        let err = check_hop(&policy, &scope, false, "http://docs.rs/", 0).expect_err("http");
        assert!(matches!(err, WebToolError::SchemeNotAllowed { .. }), "{err:?}");
        assert!(check_hop(&policy, &scope, true, "http://docs.rs/", 0).is_ok());
        for url in ["file:///etc/passwd", "ftp://docs.rs/", "data:text/html,x"] {
            let err = check_hop(&policy, &scope, true, url, 0).expect_err(url);
            assert!(
                matches!(
                    err,
                    WebToolError::SchemeNotAllowed { .. } | WebToolError::HostNotResolvable { .. }
                ),
                "{url}: {err:?}"
            );
        }
    }

    /// Userinfo und `\\@`-Tricks landen nie beim erlaubten Host.
    #[test]
    fn test_check_hop_rejects_userinfo_and_backslash_confusion() {
        let policy = policy(&["docs.rs"], false);
        let scope = scope(&["docs.rs"]);
        assert!(matches!(
            check_hop(&policy, &scope, false, "https://user:pw@docs.rs/", 0),
            Err(WebToolError::HostNotResolvable { .. })
        ));
        assert!(matches!(
            check_hop(&policy, &scope, false, "https://evil.com\\@docs.rs/", 0),
            Err(WebToolError::EgressDenied { .. })
        ));
    }

    /// Redirect-Hop-Logik: aufgelöstes Ziel auf private IP wird abgelehnt und
    /// die Meldung nennt nur den Platzhalter.
    #[test]
    fn test_resolve_location_then_check_hop_rejects_private_redirect() {
        let policy = policy(&["docs.rs"], false);
        let scope = scope(&["docs.rs"]);
        let next = resolve_location("https://docs.rs/a", "https://192.168.1.1/x", 1)
            .expect("auflösbar");
        let err = check_hop(&policy, &scope, false, &next, 1).expect_err("privat");
        assert!(matches!(err, WebToolError::EgressDenied { .. }), "{err:?}");
        assert!(!err.to_string().contains("192.168"), "{err}");

        let relative = resolve_location("https://docs.rs/a/b", "/c?d", 1).expect("relativ");
        assert_eq!(relative, "https://docs.rs/c?d");
        assert!(check_hop(&policy, &scope, false, &relative, 1).is_ok());
    }

    /// Unauflösbare Location meldet nur den Platzhalter.
    #[test]
    fn test_resolve_location_invalid_base_uses_placeholder() {
        let err = resolve_location("kein url", "http://10.1.1.1/", 3).expect_err("Basis");
        match err {
            WebToolError::HostNotResolvable { url } => assert_eq!(url, "<Weiterleitung 3>"),
            other => panic!("unerwartet: {other:?}"),
        }
    }

    /// IP-Literale werden in Meldungen neutralisiert, Namen nicht.
    #[test]
    fn test_redact_host_hides_ip_literals_only() {
        assert_eq!(redact_host("10.0.0.1"), "<IP-Literal>");
        assert_eq!(redact_host("[::1]"), "<IP-Literal>");
        assert_eq!(redact_host("docs.rs"), "docs.rs");
    }
}
