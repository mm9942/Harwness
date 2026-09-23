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
//! Dieses Crate ist eine Netz-Ausgangstür des Harness und fail-closed ausgelegt:
//!
//! 1. **Berechtigung vor Argumenten.** Jedes Tool trägt
//!    `#[harw_macros::tool(permission = "network_access")]`; der Prolog prüft
//!    [`harw_authority::Permission::NetworkAccess`] vor der Deserialisierung.
//! 2. **Egress-Client und -Policy.** Der HTTP-Client entsteht nur über
//!    [`harw_egress::build_client`] (Scoped-Resolver, Adressklassen, kein
//!    Proxy, keine Auto-Redirects). Die [`harw_egress::EgressPolicy`] wird über
//!    [`configure`] übergeben; ohne sie scheitert jeder Abruf
//!    ([`WebToolError::NotConfigured`]). Es gibt keinen „alles erlaubt“-Default.
//! 3. **Jeder Hop geprüft.** Start-URL und jedes Redirect-Ziel (höchstens
//!    [`fetch::MAX_REDIRECTS`]) laufen vor dem Senden durch
//!    [`hop::check_hop`]: Schema (`https`, `http` nur mit
//!    [`WebFetchOptions::allow_http`]), `EgressPolicy::check_url` (auch
//!    IP-Literale, die den Resolver umgehen) und Sandbox-Scope.
//! 4. **Isolierter, erneut geprüfter Cache.** Schlüssel = BLAKE3(Policy-Digest ‖
//!    Mandant/Workspace/Netz-Scope ‖ URL); ein Treffer zählt nur, wenn seine
//!    gespeicherte Kette samt Endziel die Hop-Prüfung erneut besteht. Einträge
//!    liegen unter `<HARW_HOME>/cache/web/` (`0700`/`0600`, `harw-fsutil`).
//! 5. **Kappung.** Bytes beim Lesen, Text nach der Aufbereitung (UTF-8-sicher).
//! 6. **Bereinigtes HTML.** script/style/noscript/template und versteckte
//!    Elemente erreichen das Modell nicht ([`html`]).
//! 7. **Blockierendes auf dem Blocking-Pool** ([`fetch::run_blocking`]).
//! 8. **Fail-open nur bei Transportfehlern** ([`WebToolError::is_transport`]).
//!
//! # Schlüsseltypen
//! - [`WebToolProvider`] — der `ToolProvider` über alle drei Tools.
//! - [`WebFetcher`], [`WebFetchOptions`] — Abruf-Motor und Limits.
//! - [`CacheScope`], [`CacheEntry`] — Cache-Isolation und -Format.
//! - [`FetchedDocument`], [`OutputFormat`], [`CrateSummary`].
//! - [`WebToolError`] / `WebToolResult` — crate-weiter Fehlertyp.
//!
//! # Nebenläufigkeit
//! Alle öffentlichen Typen sind `Send + Sync`. Der Basis-Fetcher liegt in einem
//! prozessweiten `OnceLock<Arc<WebFetcher>>` ([`install_fetcher`]); pro
//! Tool-Aufruf leitet [`scoped_fetcher`] eine Kopie mit Netz- und Cache-Scope
//! der aktiven Sandbox ab. Alle drei Tools sind `parallel_safe`.
//!
//! # Fehler
//! Alle Fehler sind Varianten von [`WebToolError`]; an der Tool-Grenze werden
//! sie zu `Ok(ToolOutput::error(...))`.
//!
//! # Examples
//! ```rust,no_run
//! use std::{path::PathBuf, sync::Arc};
//! use harw_egress::EgressPolicy;
//! use harw_extension_api::contributors::ToolProvider;
//! use harw_tool_web::{WebFetchOptions, WebToolProvider, configure};
//!
//! let policy = Arc::new(EgressPolicy::new(vec!["docs.rs".into()], false).unwrap());
//! configure(policy, PathBuf::from("/home/u/.harw/cache"), WebFetchOptions::default()).unwrap();
//! for spec in WebToolProvider::new().tools() {
//!     println!("{}", spec.name());
//! }
//! ```

#![forbid(unsafe_code)]

pub mod cache;
pub mod crates_io;
pub mod docs_rs;
pub mod error;
pub mod fetch;
pub mod hop;
pub mod html;
pub mod provider;

#[cfg(test)]
mod test_support;

pub use cache::{CacheEntry, CacheScope};
pub use crates_io::{CrateSummary, CratesIoArgs, WebCratesIoTool, crates_io_url, summarize};
pub use docs_rs::{DocsRsArgs, WebDocsRsTool, docs_rs_url, extract_main_content};
pub use error::{WebToolError, WebToolResult};
pub use fetch::{
    FetchArgs, FetchedDocument, OutputFormat, WebFetchOptions, WebFetchTool, WebFetcher,
    install_fetcher, scoped_fetcher, shared_fetcher,
};
pub use provider::{WebToolProvider, configure};
