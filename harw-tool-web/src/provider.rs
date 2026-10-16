//! `WebToolProvider` — aggregierter `ToolProvider` der vier Web-Tools.
//!
//! Spezifikationsquelle: AP W2-04..08, Abschnitt „6. `provider.rs`".
//!
//! # Verantwortung
//! Dieses Modul bündelt `web.fetch`, `web.docs_rs` und `web.crates_io` zu einem
//! [`harw_extension_api::contributors::ToolProvider`]. Es trifft keine
//! Netz-Entscheidungen; die liegen vollständig in [`crate::fetch`].
//!
//! # Provider-Entscheidung: `tool_provider!` statt handgeschrieben
//! `harw_tools::tool_provider!` erzeugt eine **Unit-Struktur** und
//! instanziiert jedes Tool per `Default` — ein Provider, der einen
//! `Arc<WebFetcher>` trägt, könnte diesen Zustand also gar nicht an die
//! makro-generierten Executors weiterreichen, weil auch die von
//! `#[harw_macros::tool]` erzeugten Wrapper Unit-Strukturen sind. Die
//! Alternative wäre gewesen, alle vier Executors von Hand zu schreiben, um
//! ihnen ein `Arc<WebFetcher>`-Feld zu geben — um den Preis, dass die
//! Sicherheits-Prologe (Permission vor Deserialisierung, fail-closed
//! Host-Extraktion) ebenfalls handgeschrieben und damit pro Tool erneut
//! auditierbar wären. Für die **einzige Netz-Ausgangstür** des Harness ist das
//! der schlechtere Tausch.
//!
//! Der gemeinsame Zustand liegt deshalb nicht im Provider, sondern in einem
//! prozessweiten [`crate::fetch::shared_fetcher`] (`OnceLock<Arc<WebFetcher>>`):
//! ein Verbindungspool, eine Egress-Policy, ein Cache-Verzeichnis, gesetzt über
//! [`configure`] bzw. [`crate::fetch::install_fetcher`] (ohne Aufruf kein
//! Abruf). Der geteilte Fetcher trägt **keinen** Sandbox-Scope; jeder Tool-Aufruf leitet mit
//! [`crate::fetch::scoped_fetcher`] eine Kopie ab, die den
//! [`harw_authority::NetworkScope`] der aktiven Sandbox trägt. Damit ist die
//! Allowlist pro Aufruf gebunden und nicht prozessweit eingefroren.
//!
//! # Schlüsseltypen
//! - [`WebToolProvider`] — die generierte Provider-Struktur.
//!
//! # Nebenläufigkeit
//! [`WebToolProvider`] ist zustandslos und damit `Send + Sync + Copy`. Alle
//! vier Tools sind `parallel_safe` (reine Lesezugriffe).
//!
//! # Fehler
//! Der Provider selbst erzeugt keine Fehler; unbekannte Tool-Namen liefern
//! `None` bzw. `false` (fail closed).
//!
//! # Examples
//! ```rust,no_run
//! use harw_extension_api::contributors::ToolProvider;
//! use harw_tool_web::WebToolProvider;
//!
//! let provider = WebToolProvider::new();
//! assert_eq!(provider.tools().len(), 4);
//! ```

use crate::crates_io::WebCratesIoTool;
use crate::docs_rs::WebDocsRsTool;
use crate::error::WebToolResult;
use crate::fetch::{WebFetchOptions, WebFetchTool, WebFetcher, install_fetcher};
use crate::search::WebSearchTool;
use harw_egress::EgressPolicy;
use std::path::PathBuf;
use std::sync::Arc;

harw_tools::tool_provider! {
    /// Stellt die vier Recherche-Tools `web.fetch`, `web.docs_rs`, `web.crates_io`
    /// und `web.search` bereit.
    ///
    /// # Description
    /// Zustandslose Unit-Struktur; der geteilte HTTP-Client und der
    /// Antwort-Cache liegen in [`crate::fetch::shared_fetcher`]. Siehe die
    /// Modul-Doku für die Begründung dieser Aufteilung.
    ///
    /// # Concurrency
    /// `Send + Sync + Copy`; kein geteilter veränderlicher Zustand im Provider.
    pub struct WebToolProvider {
        WebFetchTool,
        WebDocsRsTool,
        WebCratesIoTool,
        WebSearchTool,
    }
}

/// Hinterlegt Egress-Policy, Cache-Verzeichnis und Limits für alle Web-Tools.
///
/// # Description
/// Baut einen [`WebFetcher`] mit dem Client aus [`harw_egress::build_client`]
/// und installiert ihn als prozessweiten Basis-Fetcher. Nur der erste Aufruf
/// gewinnt. Ohne Aufruf ist **kein** Abruf möglich (fail-closed). Vorgesehene
/// Quellen: Policy aus `[network]` (`harw_config::NetworkSection`:
/// `allow_hosts`, `allow_private`), Cache-Verzeichnis
/// `harw_home::paths::cache_dir(home)` — die Verdrahtung liegt in
/// Registry-Defaults/Runtime.
///
/// # Arguments
/// - `policy` (`Arc<EgressPolicy>`): Egress-Policy; geteilt.
/// - `cache_dir` (`PathBuf`): Basis-Cache-Verzeichnis unter `HARW_HOME`.
/// - `options` ([`WebFetchOptions`]): Limits und Schalter.
///
/// # Returns
/// `Ok(())`, wenn dieser Aufruf die Konfiguration gesetzt hat.
///
/// # Errors
/// - [`crate::WebToolError::NotConfigured`]: der Client ließ sich nicht bauen.
/// - [`crate::WebToolError::Io`] (`AlreadyExists`): es war bereits ein Fetcher
///   installiert; die übergebene Konfiguration greift **nicht**.
///
/// # Concurrency
/// Thread-sicher über `OnceLock`.
///
/// # Examples
/// ```rust,no_run
/// use std::{path::PathBuf, sync::Arc};
/// use harw_egress::EgressPolicy;
/// use harw_tool_web::{WebFetchOptions, configure};
///
/// let policy = Arc::new(EgressPolicy::new(vec!["docs.rs".into()], false).unwrap());
/// configure(policy, PathBuf::from("/home/u/.harw/cache"), WebFetchOptions::default())?;
/// # Ok::<(), harw_tool_web::WebToolError>(())
/// ```
pub fn configure(
    policy: Arc<EgressPolicy>,
    cache_dir: PathBuf,
    options: WebFetchOptions,
) -> WebToolResult<()> {
    let fetcher = Arc::new(WebFetcher::new(policy, cache_dir, options)?);
    install_fetcher(fetcher).map_err(|_| {
        crate::WebToolError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "web tool fetcher was already configured",
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_extension_api::contributors::ToolProvider;
    use harw_tools::{ToolName, ToolSpec};

    /// Der Provider bewirbt genau die vier Tools in Deklarationsreihenfolge.
    #[test]
    fn test_web_tool_provider_lists_four_tools() {
        assert_eq!(
            WebToolProvider::TOOL_NAMES,
            &["web.fetch", "web.docs_rs", "web.crates_io", "web.search"]
        );

        let provider = WebToolProvider::new();
        // `tools()` muss gebunden werden: die `&str` in `names` zeigen in diesen Vec.
        let specs = provider.tools();
        let names: Vec<&str> = specs.iter().map(ToolSpec::name).collect();
        assert_eq!(
            names,
            vec!["web.fetch", "web.docs_rs", "web.crates_io", "web.search"]
        );
    }

    /// Jedes beworbene Tool ist auch auflösbar — sonst bewirbt der Provider
    /// einen Namen, den er nicht bedienen kann.
    #[test]
    fn test_web_tool_provider_resolves_every_advertised_tool() {
        let provider = WebToolProvider::new();
        for spec in provider.tools() {
            assert!(
                provider.executor(&ToolName::new(spec.name())).is_some(),
                "beworbenes Tool {:?} muss auflösbar sein",
                spec.name()
            );
        }
    }

    /// Unbekannte Namen fallen nicht auf ein Default-Tool zurück.
    #[test]
    fn test_web_tool_provider_unknown_name_returns_none() {
        let provider = WebToolProvider::new();
        assert!(provider.executor(&ToolName::new("web.unbekannt")).is_none());
        assert!(!provider.parallel_safe(&ToolName::new("web.unbekannt")));
    }

    /// Alle vier Tools sind reine Lesezugriffe und damit parallelsicher.
    #[test]
    fn test_web_tool_provider_all_tools_are_parallel_safe() {
        let provider = WebToolProvider::new();
        for name in WebToolProvider::TOOL_NAMES {
            assert!(
                provider.parallel_safe(&ToolName::new(*name)),
                "{name} sollte parallelsicher sein"
            );
        }
    }

    /// Audit: **jedes** Tool dieses Providers muss eine Berechtigung
    /// deklarieren — ein ungeschütztes Netz-Tool wäre ein Sicherheitsloch.
    #[test]
    fn test_web_tool_provider_every_tool_declares_network_access() {
        let unguarded: Vec<&str> = WebToolProvider::TOOL_NAMES
            .iter()
            .zip(WebToolProvider::TOOL_PERMISSIONS)
            .filter(|(_, permission)| {
                !matches!(**permission, Some(harw_tools::Permission::NetworkAccess))
            })
            .map(|(name, _)| *name)
            .collect();

        assert!(
            unguarded.is_empty(),
            "diese Tools deklarieren nicht NetworkAccess: {unguarded:?}"
        );
    }

    /// `new()` und `default()` liefern denselben zustandslosen Provider.
    #[test]
    fn test_web_tool_provider_is_default_constructible() {
        assert_eq!(
            format!("{:?}", WebToolProvider::new()),
            format!("{:?}", WebToolProvider)
        );
    }
}
