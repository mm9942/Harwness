//! Crate-weiter Fehlertyp für `harw-tool-web`.
//!
//! # Verantwortung
//! Dieses Modul besitzt ausschließlich [`WebToolError`] — die vollständige
//! Fehlermenge der drei Web-Tools (`web.fetch`, `web.docs_rs`,
//! `web.crates_io`). Es trifft keine Netz- oder Cache-Entscheidungen; das
//! delegiert es an [`crate::fetch`], [`crate::docs_rs`] und
//! [`crate::crates_io`].
//!
//! # Schlüsseltypen
//! - [`WebToolError`] — Fehler-Enum, per `#[derive(harw_macros::HarwError)]`
//!   um `Display`, `std::error::Error` (inklusive `source()`), die `From`-Impls
//!   der `#[from]`-Varianten und den Alias `WebToolResult<T>` ergänzt.
//! - `WebToolResult<T>` — vom Derive erzeugter Ergebnis-Alias.
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`, keine Sperren,
//! keine Threads.
//!
//! # Sicherheitsrelevante Varianten
//! [`WebToolError::SchemeNotAllowed`], [`WebToolError::HostNotResolvable`],
//! [`WebToolError::RedirectHostNotAllowed`], [`WebToolError::EgressDenied`],
//! [`WebToolError::TooManyRedirects`], [`WebToolError::ResponseTooLarge`] und
//! [`WebToolError::UnexpectedContentType`] sind **Ablehnungen**, keine
//! Transportfehler. Sie dürfen nie fail-open auf einen Cache-Eintrag
//! zurückfallen; nur [`WebToolError::Http`] und
//! [`WebToolError::UpstreamTimeout`] beschreiben einen erreichbaren, aber
//! gestörten Gegenüber.
//!
//! # Examples
//! ```rust
//! use harw_tool_web::error::WebToolError;
//!
//! let err = WebToolError::SchemeNotAllowed {
//!     url: "http://example.test/x".to_owned(),
//! };
//! assert!(err.to_string().contains("https"));
//! ```

use harw_macros::HarwError;

/// Fehler aller Werkzeuge in `harw-tool-web`.
///
/// # Description
/// Jede Variante trägt den vollständigen Kontext, der zur Diagnose nötig ist,
/// ohne dass Quellcode gelesen werden muss — aber **niemals** den
/// Antwort-Körper, Header-Werte oder Zugangsdaten. Die Meldungen sind für die
/// Weitergabe an das Modell gedacht und deshalb frei von internen Pfaden außer
/// dem Cache-Pfad in [`Self::CacheCorrupt`].
///
/// # Concurrency
/// `Send + Sync`; kein geteilter veränderlicher Zustand.
#[derive(Debug, HarwError)]
pub enum WebToolError {
    /// Transportfehler von `reqwest` (DNS, TLS, Verbindungsabbruch, Timeout
    /// einer Einzelanfrage). Der einzige Fehler, bei dem ein veralteter
    /// Cache-Eintrag fail-open genutzt werden darf.
    #[from]
    #[msg("HTTP-Transportfehler: {0}")]
    Http(reqwest::Error),

    /// Lokaler Datei-Fehler beim Lesen oder Schreiben des Antwort-Caches.
    #[from]
    #[msg("Datei-Fehler im Web-Cache: {0}")]
    Io(std::io::Error),

    /// Ungültiges JSON — entweder ein Cache-Eintrag oder eine API-Antwort.
    #[from]
    #[msg("JSON-Fehler: {0}")]
    Json(serde_json::Error),

    /// Die URL verwendet ein anderes Schema als `https://` (bzw. `http://`
    /// ohne ausdrückliche Freigabe über
    /// [`crate::fetch::WebFetchOptions::allow_http`]).
    ///
    /// Fail-closed: `file://`, `data:` und alles andere werden abgelehnt,
    /// bevor eine Verbindung aufgebaut wird.
    #[msg("nur https:// ist erlaubt, abgelehnte URL: '{url}'")]
    SchemeNotAllowed {
        /// Die abgelehnte URL wie übergeben; bei Weiterleitungen nur ein
        /// Platzhalter (`<Weiterleitung n>`), nie das Ziel des Servers.
        url: String,
    },

    /// Die Egress-Richtlinie ([`harw_egress::EgressPolicy`]) lehnt das Ziel ab:
    /// Host nicht in der Allowlist, lokaler Name, unzulässige Adressklasse
    /// (Loopback, privat, Link-Local, Cloud-Metadaten …) oder eine
    /// DNS-Auflösung ohne zulässige Adresse.
    ///
    /// Die Begründung nennt nie eine aufgelöste oder wörtliche IP-Adresse,
    /// nur Hostnamen und Adressklassen.
    #[msg("Ziel durch die Egress-Richtlinie abgelehnt: {reason}")]
    EgressDenied {
        /// Menschenlesbare Begründung ohne Adressen.
        reason: String,
    },

    /// Die Web-Tools wurden nicht konfiguriert (kein
    /// [`crate::fetch::WebFetcher`] mit Egress-Policy installiert) oder die
    /// Konfiguration ist unbrauchbar. Fail-closed: ohne Policy kein Abruf.
    #[msg("Web-Tools sind nicht einsatzbereit: {what}")]
    NotConfigured {
        /// Was fehlt oder unbrauchbar ist.
        what: &'static str,
    },

    /// Eine blockierende Hintergrundaufgabe (`spawn_blocking`: Parsen,
    /// Cache-I/O) ist abgebrochen oder in Panik geraten.
    #[msg("Hintergrundaufgabe '{task}' wurde abgebrochen")]
    BlockingTask {
        /// Name der Aufgabe.
        task: &'static str,
    },

    /// Aus der URL ließ sich kein Hostname extrahieren.
    ///
    /// Fail-closed: ohne Host kann die Allowlist der Sandbox nicht geprüft
    /// werden, also wird abgebrochen statt mit leerem Host fortzufahren.
    #[msg("aus der URL '{url}' ließ sich kein Hostname lesen")]
    HostNotResolvable {
        /// Die URL, deren Host nicht bestimmbar war.
        url: String,
    },

    /// Ein Redirect zeigte auf einen Host außerhalb der Sandbox-Allowlist.
    ///
    /// Jeder Hop wird erneut gegen [`harw_sandbox::NetworkScope`] geprüft; ein
    /// Redirect ist damit kein Weg an der Allowlist vorbei.
    #[msg("Host '{host}' ist nicht in der erlaubten Host-Liste dieser Sandbox (URL '{url}')")]
    RedirectHostNotAllowed {
        /// Die URL des abgelehnten Hops (bei Weiterleitungen ein Platzhalter).
        url: String,
        /// Der abgelehnte Hostname.
        host: String,
    },

    /// Die Redirect-Kette überschritt das Limit.
    #[msg("zu viele Weiterleitungen, abgebrochen bei '{url}'")]
    TooManyRedirects {
        /// Die URL bzw. der Weiterleitungs-Platzhalter, bei dem abgebrochen wurde.
        url: String,
    },

    /// Die Antwort überschritt das Byte-Limit; der Stream wurde abgebrochen.
    #[msg("Antwort von '{host}' überschreitet das Limit von {limit} Bytes")]
    ResponseTooLarge {
        /// Das durchgesetzte Limit in Bytes.
        limit: usize,
        /// Der Host, dessen Antwort abgebrochen wurde.
        host: String,
    },

    /// Der `Content-Type` der Antwort liegt außerhalb der Positivliste.
    #[msg("unerwarteter Content-Type '{content_type}' von '{host}'")]
    UnexpectedContentType {
        /// Der gemeldete Medientyp (ohne Parameter), oder leer.
        content_type: String,
        /// Der antwortende Host.
        host: String,
    },

    /// Ein Cache-Eintrag ließ sich nicht als [`crate::cache::CacheEntry`]
    /// lesen. Der Aufrufer behandelt das als Cache-Miss und holt neu.
    ///
    /// Wird nur protokolliert, nie als Tool-Ausgabe an das Modell gegeben.
    #[msg("Cache-Eintrag '{path}' ist unlesbar oder beschädigt")]
    CacheCorrupt {
        /// Der Pfad des betroffenen Cache-Eintrags.
        path: String,
    },

    /// Der Gegenüber antwortete mit einem Status außerhalb von 2xx.
    #[msg("'{host}' antwortete mit HTTP-Status {status}")]
    UpstreamStatus {
        /// Der HTTP-Statuscode.
        status: u16,
        /// Der antwortende Host.
        host: String,
    },

    /// Die Gesamt-Deadline über alle Redirect-Hops wurde überschritten.
    ///
    /// Ergänzung gegenüber der Ursprungsspezifikation: `reqwest`s
    /// Einzelanfrage-Timeout deckt eine Redirect-Kette nicht ab, die
    /// Gesamt-Deadline in [`crate::fetch::WebFetcher::fetch`] schon.
    #[msg("'{host}' hat die Gesamt-Deadline von {seconds}s überschritten")]
    UpstreamTimeout {
        /// Der Host, auf den gewartet wurde.
        host: String,
        /// Die überschrittene Deadline in Sekunden.
        seconds: u64,
    },
}

impl WebToolError {
    /// Gibt an, ob der Fehler ein reiner Transportfehler ist.
    ///
    /// # Description
    /// Nur bei einem Transportfehler darf [`crate::fetch::WebFetcher::fetch`]
    /// fail-open auf einen veralteten Cache-Eintrag zurückfallen. Eine
    /// Ablehnung aus Sicherheitsgründen (Schema, Host, Limit, Content-Type)
    /// muss den Aufruf scheitern lassen, sonst würde eine einmal
    /// zwischengespeicherte Antwort eine später verschärfte Allowlist
    /// unterlaufen.
    ///
    /// # Returns
    /// `true` für [`Self::Http`] und [`Self::UpstreamTimeout`], sonst `false`.
    ///
    /// # Concurrency
    /// Reine Funktion; von jedem Thread aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use harw_tool_web::error::WebToolError;
    ///
    /// let denied = WebToolError::TooManyRedirects { url: "https://a.test/".to_owned() };
    /// assert!(!denied.is_transport());
    /// ```
    #[must_use]
    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Http(_) | Self::UpstreamTimeout { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Display` nennt das erlaubte Schema, damit die Meldung ohne Quellcode
    /// verständlich ist.
    #[test]
    fn test_scheme_not_allowed_message_names_https() {
        let err = WebToolError::SchemeNotAllowed {
            url: "http://example.test/x".to_owned(),
        };
        let message = err.to_string();
        assert!(message.contains("https"), "unerwartet: {message}");
        assert!(
            message.contains("http://example.test/x"),
            "unerwartet: {message}"
        );
    }

    /// Das Byte-Limit steht in der Meldung, damit der Aufrufer weiß, worauf er
    /// stößt.
    #[test]
    fn test_response_too_large_message_contains_limit_and_host() {
        let err = WebToolError::ResponseTooLarge {
            limit: 1024,
            host: "docs.rs".to_owned(),
        };
        let message = err.to_string();
        assert!(message.contains("1024"), "unerwartet: {message}");
        assert!(message.contains("docs.rs"), "unerwartet: {message}");
    }

    /// Der abgelehnte Redirect-Host taucht mit URL in der Meldung auf.
    #[test]
    fn test_redirect_host_not_allowed_message_contains_host_and_url() {
        let err = WebToolError::RedirectHostNotAllowed {
            url: "https://evil.test/x".to_owned(),
            host: "evil.test".to_owned(),
        };
        let message = err.to_string();
        assert!(message.contains("evil.test"), "unerwartet: {message}");
    }

    /// `#[from]` erzeugt die Konvertierung, die `?` für `serde_json` braucht.
    #[test]
    fn test_from_serde_json_error_maps_to_json_variant() {
        let parse_error = serde_json::from_str::<serde_json::Value>("{ kein json")
            .expect_err("ungültiges JSON muss scheitern");
        let err = WebToolError::from(parse_error);
        assert!(matches!(err, WebToolError::Json(_)));
    }

    /// `#[from]` erzeugt die Konvertierung, die `?` für `std::io` braucht, und
    /// `source()` verweist auf den gekapselten Fehler.
    #[test]
    fn test_from_io_error_maps_to_io_variant_with_source() {
        use std::error::Error as _;

        let err = WebToolError::from(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "kein Cache",
        ));
        assert!(matches!(err, WebToolError::Io(_)));
        assert!(err.source().is_some(), "source() muss die Ursache liefern");
    }

    /// Nur Transportfehler dürfen fail-open auf den Cache fallen.
    #[test]
    fn test_is_transport_separates_denials_from_transport_faults() {
        let timeout = WebToolError::UpstreamTimeout {
            host: "docs.rs".to_owned(),
            seconds: 45,
        };
        assert!(timeout.is_transport());

        let denial = WebToolError::UnexpectedContentType {
            content_type: "application/octet-stream".to_owned(),
            host: "docs.rs".to_owned(),
        };
        assert!(
            !denial.is_transport(),
            "eine Ablehnung darf nie als Transportfehler gelten"
        );

        let status = WebToolError::UpstreamStatus {
            status: 500,
            host: "crates.io".to_owned(),
        };
        assert!(
            !status.is_transport(),
            "ein 5xx ist eine Antwort, kein Transportfehler"
        );
    }

    /// Egress-Ablehnungen sind nie Transportfehler (kein Cache-Fail-open).
    #[test]
    fn test_is_transport_egress_denied_is_not_transport() {
        let err = WebToolError::EgressDenied {
            reason: "Zieladresse der Klasse loopback ist nicht erlaubt".to_owned(),
        };
        assert!(!err.is_transport());
        assert!(err.to_string().contains("loopback"), "{err}");
        let unconfigured = WebToolError::NotConfigured { what: "keine Egress-Policy" };
        assert!(!unconfigured.is_transport());
        assert!(unconfigured.to_string().contains("keine Egress-Policy"));
    }

    /// Der vom Derive erzeugte Alias existiert und ist verwendbar.
    #[test]
    fn test_web_tool_result_alias_is_usable() {
        fn ok() -> WebToolResult<u8> {
            Ok(7)
        }
        assert_eq!(ok().ok(), Some(7));
    }
}
