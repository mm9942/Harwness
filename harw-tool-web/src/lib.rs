//! `harw-tool-web` — Recherche-Werkzeuge für Sub-Agenten ohne Browser-Stack.
//!
//! Spezifikationsquelle: AP W2-04..08.
//!
//! # Zweck
//! Dieses Crate ist die Grundlage für „Planung hängt an Recherche": ein
//! Sub-Agent (`researcher-web`) holt damit offizielle Dokumentation und
//! Crate-Metadaten, **ohne** WebDriver, Headless-Browser oder JavaScript-
//! Ausführung. Es stellt drei Tools bereit:
//!
//! | Tool | Zweck |
//! |---|---|
//! | `web.fetch` | eine HTTPS-Ressource als Text, Markdown oder Rohtext |
//! | `web.docs_rs` | die offizielle Crate-Doku von docs.rs als Markdown |
//! | `web.crates_io` | kompakte Crate-Metadaten (Version, MSRV, Lizenz, Repo) |
//!
//! # Sicherheitskontrakt
//! Dieses Crate ist die **einzige Netz-Ausgangstür** des Harness und
//! entsprechend fail-closed ausgelegt:
//!
//! 1. **Berechtigung vor Argumenten.** Jedes Tool trägt
//!    `#[harw_macros::tool(permission = "network_access")]`; der generierte
//!    Prolog prüft [`harw_sandbox::Permission::NetworkAccess`], *bevor*
//!    modell-kontrollierte Argumente deserialisiert werden.
//! 2. **Host-Allowlist.** `web.fetch` nutzt `host_from = "url"` und damit den
//!    generierten [`harw_tools::require_host_access`]-Prolog. `web.docs_rs` und
//!    `web.crates_io` bauen ihre URL erst im Rumpf und können `host_from` nicht
//!    verwenden; sie führen dieselbe Prüfung dort von Hand aus — inklusive
//!    fail-closed-Abbruch, wenn [`harw_tools::host_from_url`] `None` liefert.
//! 3. **Nur `https://`.** Jedes andere Schema wird abgelehnt, bevor eine
//!    Verbindung entsteht; zusätzlich ist der Client mit `https_only(true)`
//!    gebaut.
//! 4. **Redirects werden von Hand verfolgt.** Höchstens
//!    [`fetch::MAX_REDIRECTS`] Hops, und **jeder** Hop durchläuft erneut
//!    Schema-Prüfung, Host-Extraktion und
//!    [`harw_sandbox::NetworkScope::allows`]. Ein Redirect ist damit kein Weg
//!    an der Allowlist vorbei.
//! 5. **Byte-Limit beim Lesen.** Der Antwort-Körper wird chunk-weise
//!    akkumuliert und der Stream abgebrochen, sobald das Limit überschritten
//!    würde — nicht erst nach vollständigem Puffern. Ein modell-gesetztes
//!    `max_bytes` kann das Limit nur senken, nie über
//!    [`fetch::HARD_MAX_BYTES`] heben.
//! 6. **Content-Type-Positivliste.** Nur Text-, JSON- und XML-artige Typen
//!    werden angenommen; ein fehlender Content-Type gilt als Ablehnung.
//! 7. **Cache ohne Symlink-Falle.** Einträge liegen unter
//!    `<cache_dir>/web/<sha256(url)>.json`, werden atomar (Temp-Datei +
//!    Rename) geschrieben und vor jedem Lesen und Ersetzen auf Symlinks
//!    geprüft (TOCTOU-Schutz nach dem Vorbild von
//!    `harw-model-catalog/src/models_dev.rs`).
//! 8. **Fail-open nur bei Transportfehlern.** Ein veralteter Cache-Eintrag
//!    rettet einen Netzausfall, aber niemals eine Ablehnung aus
//!    Sicherheitsgründen (siehe [`WebToolError::is_transport`]).
//!
//! # Schlüsseltypen
//! - [`WebToolProvider`] — der `ToolProvider` über alle drei Tools.
//! - [`WebFetcher`] — der Abruf-Motor mit Cache, Limits und Host-Scope.
//! - [`FetchedDocument`], [`CacheEntry`], [`OutputFormat`] — Ergebnis-, Cache-
//!   und Formattypen.
//! - [`CrateSummary`] — die verdichtete crates.io-Antwort.
//! - [`WebToolError`] / `WebToolResult` — crate-weiter Fehlertyp.
//!
//! # Nebenläufigkeit
//! Alle öffentlichen Typen sind `Send + Sync`. [`WebToolProvider`] ist
//! zustandslos; der HTTP-Client und das Cache-Verzeichnis liegen in einem
//! prozessweiten [`fetch::shared_fetcher`] (`OnceLock<Arc<WebFetcher>>`), damit
//! alle Tools einen Verbindungspool teilen. Pro Tool-Aufruf wird daraus mit
//! [`fetch::scoped_fetcher`] eine Kopie mit dem [`harw_sandbox::NetworkScope`]
//! der aktiven Sandbox abgeleitet — die Allowlist ist damit an den Aufruf
//! gebunden, nicht prozessweit eingefroren. Alle drei Tools sind
//! `parallel_safe`. Es gibt keine Sperren auf heißen Pfaden.
//!
//! # Fehler
//! Alle Fehler dieses Crates sind Varianten von [`WebToolError`]. Auf der
//! Tool-Grenze werden sie zu `Ok(ToolOutput::error(...))` — ein
//! fehlgeschlagener Abruf beendet den Turn nicht, sondern gibt dem Modell eine
//! lesbare Begründung.
//!
//! # Examples
//! ```rust,no_run
//! use harw_extension_api::contributors::ToolProvider;
//! use harw_tool_web::WebToolProvider;
//!
//! let provider = WebToolProvider::new();
//! for spec in provider.tools() {
//!     println!("{}", spec.name());
//! }
//! ```

#![forbid(unsafe_code)]

pub mod crates_io;
pub mod docs_rs;
pub mod error;
pub mod fetch;
pub mod provider;

pub use crates_io::{CrateSummary, CratesIoArgs, WebCratesIoTool, crates_io_url, summarize};
pub use docs_rs::{DocsRsArgs, WebDocsRsTool, docs_rs_url, extract_main_content};
pub use error::{WebToolError, WebToolResult};
pub use fetch::{
    CacheEntry, FetchArgs, FetchedDocument, OutputFormat, WebFetchTool, WebFetcher,
    install_fetcher, scoped_fetcher, shared_fetcher,
};
pub use provider::{WebToolProvider, configure};
