//! `web.crates_io` — kompakte Crate-Metadaten von der crates.io-API.
//!
//! Spezifikationsquelle: AP W2-04..08, Abschnitt „5. `crates_io.rs`".
//!
//! # Verantwortung
//! Dieses Modul baut die API-URL, holt die JSON-Antwort über
//! [`crate::fetch::WebFetcher`] und verdichtet sie zu [`CrateSummary`]. Es
//! verarbeitet **kein HTML** und hat keinen eigenen HTTP-Zugriff.
//!
//! # Schlüsseltypen
//! - [`CratesIoArgs`] — Argument-Struktur des Tools.
//! - [`CrateSummary`] — die verdichtete Antwort.
//! - `WebCratesIoTool` — der von `#[harw_macros::tool]` erzeugte `ToolExecutor`.
//!
//! # Sicherheitskontrakt
//! - Der Permission-Prolog (`network_access`) stammt aus dem Makro.
//! - `host_from` ist nicht anwendbar (die URL entsteht erst im Rumpf); der
//!   Host-Check läuft von Hand über [`harw_tools::host_from_url`] +
//!   [`harw_tools::require_host_access`], fail-closed bei nicht extrahierbarem
//!   Host.
//! - Der Crate-Name wird gegen die crates.io-Zeichenmenge geprüft, bevor er in
//!   den URL-Pfad gelangt.
//! - Die API antwortet mit `application/json`; die Content-Type-Positivliste
//!   in [`crate::fetch`] lässt Binärformate gar nicht erst durch.
//!
//! # Nebenläufigkeit
//! `WebCratesIoTool` ist eine Unit-Struktur, damit `Send + Sync + Copy`; das
//! Tool ist `parallel_safe` (reiner Lesezugriff).
//!
//! # Fehler
//! Alle Fehler münden in `Ok(ToolOutput::error(...))`; das Tool panickt nie.
//!
//! # Examples
//! ```rust
//! use harw_tool_web::crates_io::crates_io_url;
//!
//! assert_eq!(
//!     crates_io_url("serde").as_deref(),
//!     Some("https://crates.io/api/v1/crates/serde")
//! );
//! ```

use crate::error::WebToolResult;
use crate::fetch::{run_blocking, scoped_fetcher};
use harw_macros::Tool;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::{Deserialize, Serialize};

/// Der einzige Host, den dieses Tool anspricht.
pub const CRATES_IO_HOST: &str = "crates.io";

/// Obergrenze für Crate-Namen (crates.io erlaubt maximal 64 Zeichen).
const MAX_CRATE_NAME_LEN: usize = 64;

/// Argumente des Tools `web.crates_io`.
#[derive(Debug, Clone, Tool, Deserialize)]
#[tool(
    name = "web.crates_io",
    description = "Liefert kompakte Metadaten einer Crate von crates.io (Version, MSRV, Lizenz, Repository)."
)]
pub struct CratesIoArgs {
    /// Name der Crate auf crates.io, z. B. `serde`.
    pub crate_name: String,
}

/// Verdichtete Metadaten einer Crate.
///
/// # Description
/// Bewusst schmal: genug, damit ein Rechercheur eine Dependency-Entscheidung
/// treffen kann, ohne dass die Antwort das Kontextfenster füllt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CrateSummary {
    /// Der Name der Crate, wie crates.io ihn führt.
    pub name: String,
    /// Die neueste stabile Version (ohne Pre-Release), falls vorhanden.
    pub latest_stable: Option<String>,
    /// Die insgesamt neueste Version, auch wenn sie ein Pre-Release ist.
    pub newest: Option<String>,
    /// Anzahl aller veröffentlichten Versionen.
    pub versions_count: usize,
    /// Die Lizenz der neuesten stabilen Version.
    pub license: Option<String>,
    /// Die MSRV (`rust-version`) der neuesten stabilen Version, falls gesetzt.
    pub rust_version: Option<String>,
    /// Ob die neueste stabile Version zurückgezogen (`yanked`) ist.
    pub yanked: bool,
    /// Das Repository der Crate.
    pub repository: Option<String>,
    /// Die Kurzbeschreibung der Crate.
    pub description: Option<String>,
}

/// Teilschema der crates.io-Antwort: das `crate`-Objekt.
#[derive(Debug, Clone, Deserialize)]
struct CrateMeta {
    /// Der kanonische Crate-Name.
    name: String,
    /// Kurzbeschreibung.
    #[serde(default)]
    description: Option<String>,
    /// Repository-URL.
    #[serde(default)]
    repository: Option<String>,
    /// Neueste stabile Version.
    #[serde(default)]
    max_stable_version: Option<String>,
    /// Neueste Version insgesamt.
    #[serde(default)]
    newest_version: Option<String>,
}

/// Teilschema der crates.io-Antwort: ein Eintrag aus `versions`.
#[derive(Debug, Clone, Deserialize)]
struct VersionMeta {
    /// Die Versionsnummer.
    num: String,
    /// Ob die Version zurückgezogen wurde.
    #[serde(default)]
    yanked: bool,
    /// Die Lizenz dieser Version.
    #[serde(default)]
    license: Option<String>,
    /// Die MSRV dieser Version.
    #[serde(default)]
    rust_version: Option<String>,
}

/// Das für dieses Tool relevante Teilschema der crates.io-Antwort.
#[derive(Debug, Clone, Deserialize)]
struct CratesIoResponse {
    /// Das `crate`-Objekt (Rust-Schlüsselwort, deshalb umbenannt).
    #[serde(rename = "crate")]
    krate: CrateMeta,
    /// Alle veröffentlichten Versionen.
    #[serde(default)]
    versions: Vec<VersionMeta>,
}

/// Prüft einen Crate-Namen gegen die von crates.io zugelassene Zeichenmenge.
fn is_safe_crate_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CRATE_NAME_LEN
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Baut die crates.io-API-URL für eine Crate.
///
/// # Arguments
/// - `crate_name` (`&str`): Name der Crate; nur `[A-Za-z0-9_-]`, maximal 64
///   Zeichen.
///
/// # Returns
/// `Some(String)` mit der API-URL, oder `None`, wenn der Name die
/// Zeichen-Positivliste verletzt (Ablehnung, kein Fehlschlag).
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_tool_web::crates_io::crates_io_url;
///
/// assert_eq!(
///     crates_io_url("serde_json").as_deref(),
///     Some("https://crates.io/api/v1/crates/serde_json")
/// );
/// assert_eq!(crates_io_url("../etc"), None);
/// ```
#[must_use]
pub fn crates_io_url(crate_name: &str) -> Option<String> {
    let name = crate_name.trim();
    if !is_safe_crate_name(name) {
        return None;
    }
    Some(format!("https://{CRATES_IO_HOST}/api/v1/crates/{name}"))
}

/// Verdichtet eine crates.io-API-Antwort zu [`CrateSummary`].
///
/// # Description
/// Als „neueste stabile Version" gilt `crate.max_stable_version`; fehlt sie,
/// wird der erste `versions`-Eintrag ohne Pre-Release-Kennzeichen (`-` in der
/// Versionsnummer) und ohne `yanked` genommen. Lizenz, MSRV und `yanked`
/// stammen aus genau diesem Versionseintrag — nicht aus einem beliebigen
/// anderen, sonst würde eine später gelockerte Lizenz einer alten Version
/// gemeldet.
///
/// # Arguments
/// - `raw_json` (`&str`): der unveränderte Antwort-Körper der API.
///
/// # Returns
/// Die verdichteten Metadaten.
///
/// # Errors
/// - [`crate::WebToolError::Json`]: die Antwort entspricht nicht dem erwarteten
///   Teilschema (fehlendes `crate.name`, kaputtes JSON).
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_tool_web::crates_io::summarize;
///
/// let raw = r#"{"crate":{"name":"serde","max_stable_version":"1.0.219"},
///               "versions":[{"num":"1.0.219","license":"MIT OR Apache-2.0"}]}"#;
/// let summary = summarize(raw).unwrap();
/// assert_eq!(summary.latest_stable.as_deref(), Some("1.0.219"));
/// ```
pub fn summarize(raw_json: &str) -> WebToolResult<CrateSummary> {
    let response: CratesIoResponse = serde_json::from_str(raw_json)?;

    let latest_stable = response
        .krate
        .max_stable_version
        .clone()
        .or_else(|| first_stable_version(&response.versions));

    // Lizenz/MSRV/yanked kommen aus genau der Version, die als „latest stable"
    // gemeldet wird; ohne Treffer bleiben sie leer statt geraten.
    let selected = latest_stable
        .as_deref()
        .and_then(|wanted| response.versions.iter().find(|entry| entry.num == wanted));

    Ok(CrateSummary {
        name: response.krate.name,
        latest_stable,
        newest: response.krate.newest_version,
        versions_count: response.versions.len(),
        license: selected.and_then(|entry| entry.license.clone()),
        rust_version: selected.and_then(|entry| entry.rust_version.clone()),
        yanked: selected.is_some_and(|entry| entry.yanked),
        repository: response.krate.repository,
        description: response.krate.description,
    })
}

/// Die erste nicht zurückgezogene Version ohne Pre-Release-Kennzeichen.
fn first_stable_version(versions: &[VersionMeta]) -> Option<String> {
    versions
        .iter()
        .find(|entry| !entry.yanked && !entry.num.contains('-'))
        .map(|entry| entry.num.clone())
}

/// Führt `web.crates_io` aus.
///
/// Der Permission-Prolog stammt aus dem Makro; der Host-Check erfolgt von Hand,
/// weil die URL erst hier entsteht (siehe Modul-Doku).
#[harw_macros::tool(
    name = "web.crates_io",
    description = "Liefert kompakte Metadaten einer Crate von crates.io (Version, MSRV, Lizenz, Repository).",
    permission = "network_access",
    parallel_safe
)]
async fn web_crates_io(
    context: &ToolExecutionContext,
    args: CratesIoArgs,
) -> Result<ToolOutput, ToolsError> {
    let Some(url) = crates_io_url(&args.crate_name) else {
        return Ok(ToolOutput::error(format!(
            "Tool '{}': ungültiger Crate-Name",
            WebCratesIoTool::NAME
        )));
    };

    // Fail-closed: ohne extrahierbaren Host wird die Allowlist nicht geprüft.
    let Some(host) = harw_tools::host_from_url(&url) else {
        return Ok(ToolOutput::error(format!(
            "Tool '{}': aus der gebauten URL ließ sich kein Hostname lesen",
            WebCratesIoTool::NAME
        )));
    };
    if let Some(denied) = harw_tools::require_host_access(context, &host, WebCratesIoTool::NAME) {
        return Ok(denied);
    }

    let fetcher = match scoped_fetcher(context, None) {
        Ok(fetcher) => fetcher,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };

    // Rohkörper: die Antwort ist JSON und darf nicht durch einen HTML-Pfad
    // laufen; die Verdichtung begrenzt die Ausgabegröße.
    let document = match fetcher.fetch_source(&url).await {
        Ok(document) => document,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };

    // JSON bis zum Byte-Limit zu parsen ist CPU-gebunden (F-169).
    let body = document.body;
    let summary = match run_blocking("crates-io-summary", move || summarize(&body)).await {
        Ok(summary) => summary,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };

    match serde_json::to_value(&summary) {
        Ok(value) => Ok(ToolOutput::json(value)),
        Err(err) => Err(ToolsError::from(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::WebToolError;
    use crate::test_support::{TestError, TestResult, ctx};

    const SAMPLE: &str = r#"{
        "crate": {
            "name": "serde",
            "description": "A generic serialization/deserialization framework",
            "repository": "https://github.com/serde-rs/serde",
            "max_stable_version": "1.0.219",
            "newest_version": "1.0.220-alpha.1"
        },
        "versions": [
            { "num": "1.0.220-alpha.1", "yanked": false, "license": "MIT", "rust_version": "1.31" },
            { "num": "1.0.219", "yanked": false, "license": "MIT OR Apache-2.0", "rust_version": "1.31" },
            { "num": "1.0.218", "yanked": true, "license": "MIT OR Apache-2.0" }
        ]
    }"#;

    // --- URL-Bau ------------------------------------------------------------

    /// Der Name landet unverändert im API-Pfad.
    #[test]
    fn test_crates_io_url_builds_api_path() {
        assert_eq!(
            crates_io_url("serde").as_deref(),
            Some("https://crates.io/api/v1/crates/serde")
        );
    }

    /// Bindestriche bleiben erhalten — crates.io kennt keine Unterstrich-Form.
    #[test]
    fn test_crates_io_url_keeps_dashes() {
        assert_eq!(
            crates_io_url("harw-tool-web").as_deref(),
            Some("https://crates.io/api/v1/crates/harw-tool-web")
        );
    }

    /// Umgebende Leerzeichen werden entfernt.
    #[test]
    fn test_crates_io_url_trims_input() {
        assert_eq!(
            crates_io_url("  serde  ").as_deref(),
            Some("https://crates.io/api/v1/crates/serde")
        );
    }

    /// Pfad-Traversal und Host-Injektion werden abgelehnt.
    #[test]
    fn test_crates_io_url_rejects_unsafe_names() {
        assert_eq!(crates_io_url("../../admin"), None);
        assert_eq!(crates_io_url("serde/../evil"), None);
        assert_eq!(crates_io_url("a@evil.test"), None);
        assert_eq!(crates_io_url("serde?x=1"), None);
        assert_eq!(crates_io_url(""), None);
        assert_eq!(crates_io_url("   "), None);
        assert_eq!(crates_io_url(&"a".repeat(MAX_CRATE_NAME_LEN + 1)), None);
    }

    /// Jede gebaute URL ist https auf crates.io.
    #[test]
    fn test_crates_io_url_is_always_https_on_crates_io() -> TestResult {
        let url = crates_io_url("serde").ok_or(TestError::Missing("gültige URL"))?;
        assert!(url.starts_with("https://"));
        assert_eq!(
            harw_tools::host_from_url(&url).as_deref(),
            Some(CRATES_IO_HOST)
        );
        Ok(())
    }

    // --- Verdichtung --------------------------------------------------------

    /// Die stabile Version, ihre Lizenz und ihre MSRV werden korrekt gewählt.
    #[test]
    fn test_summarize_picks_max_stable_version_metadata() -> TestResult {
        let summary = summarize(SAMPLE).map_err(ctx("gültige Antwort"))?;

        assert_eq!(summary.name, "serde");
        assert_eq!(summary.latest_stable.as_deref(), Some("1.0.219"));
        assert_eq!(summary.newest.as_deref(), Some("1.0.220-alpha.1"));
        assert_eq!(summary.versions_count, 3);
        assert_eq!(summary.license.as_deref(), Some("MIT OR Apache-2.0"));
        assert_eq!(summary.rust_version.as_deref(), Some("1.31"));
        assert!(!summary.yanked);
        assert_eq!(
            summary.repository.as_deref(),
            Some("https://github.com/serde-rs/serde")
        );
        assert!(summary.description.is_some());
        Ok(())
    }

    /// Ohne `max_stable_version` wird die erste stabile, nicht zurückgezogene
    /// Version genommen.
    #[test]
    fn test_summarize_falls_back_to_first_stable_version() -> TestResult {
        let raw = r#"{
            "crate": { "name": "beta-only" },
            "versions": [
                { "num": "2.0.0-rc.1", "yanked": false },
                { "num": "1.4.0", "yanked": true },
                { "num": "1.3.0", "yanked": false, "license": "Apache-2.0" }
            ]
        }"#;

        let summary = summarize(raw).map_err(ctx("gültige Antwort"))?;
        assert_eq!(summary.latest_stable.as_deref(), Some("1.3.0"));
        assert_eq!(summary.license.as_deref(), Some("Apache-2.0"));
        assert!(!summary.yanked);
        Ok(())
    }

    /// Ein zurückgezogenes `max_stable_version` wird als solches gemeldet.
    #[test]
    fn test_summarize_reports_yanked_latest_stable() -> TestResult {
        let raw = r#"{
            "crate": { "name": "zurueckgezogen", "max_stable_version": "0.9.0" },
            "versions": [ { "num": "0.9.0", "yanked": true, "license": "MIT" } ]
        }"#;

        let summary = summarize(raw).map_err(ctx("gültige Antwort"))?;
        assert!(summary.yanked, "yanked muss durchgereicht werden");
        assert_eq!(summary.latest_stable.as_deref(), Some("0.9.0"));
        Ok(())
    }

    /// Fehlende optionale Felder werden zu `None`, nicht zu einem Fehler.
    #[test]
    fn test_summarize_tolerates_missing_optional_fields() -> TestResult {
        let raw = r#"{ "crate": { "name": "minimal" } }"#;
        let summary = summarize(raw).map_err(ctx("minimale Antwort ist gültig"))?;

        assert_eq!(summary.name, "minimal");
        assert_eq!(summary.latest_stable, None);
        assert_eq!(summary.newest, None);
        assert_eq!(summary.versions_count, 0);
        assert_eq!(summary.license, None);
        assert_eq!(summary.rust_version, None);
        assert!(!summary.yanked);
        Ok(())
    }

    /// Fehlt `crate.name`, ist die Antwort ungültig.
    #[test]
    fn test_summarize_rejects_response_without_crate_name() -> TestResult {
        let Err(err) = summarize(r#"{ "crate": {} }"#) else {
            return Err(TestError::Unexpected(
                "ohne crate.name ist die Antwort unbrauchbar".into(),
            ));
        };
        assert!(matches!(err, WebToolError::Json(_)), "{err:?}");
        Ok(())
    }

    /// Kaputtes JSON wird als `Json`-Fehler gemeldet.
    #[test]
    fn test_summarize_rejects_broken_json() -> TestResult {
        let Err(err) = summarize("{ kein json") else {
            return Err(TestError::Unexpected("kaputtes JSON muss scheitern".into()));
        };
        assert!(matches!(err, WebToolError::Json(_)), "{err:?}");
        Ok(())
    }

    /// Die Zusammenfassung serialisiert zu flachem JSON ohne HTML-Reste.
    #[test]
    fn test_crate_summary_serializes_to_flat_json() -> TestResult {
        let summary = summarize(SAMPLE).map_err(ctx("gültige Antwort"))?;
        let value = serde_json::to_value(&summary).map_err(ctx("Serialisierung"))?;

        assert_eq!(value["name"], "serde");
        assert_eq!(value["latest_stable"], "1.0.219");
        assert_eq!(value["versions_count"], 3);
        assert_eq!(value["yanked"], false);
        Ok(())
    }

    // --- Tool-Deklaration ---------------------------------------------------

    /// Das Tool deklariert Netz-Berechtigung und Parallelsicherheit (Letztere
    /// als Compile-Zeit-Assertion, siehe unten).
    #[test]
    fn test_web_crates_io_tool_declares_network_permission() {
        assert_eq!(WebCratesIoTool::NAME, "web.crates_io");
        assert_eq!(
            WebCratesIoTool::PERMISSION,
            Some(harw_tools::Permission::NetworkAccess)
        );
    }

    // `PARALLEL_SAFE` is a macro-generated `const bool` (see
    // `#[harw_macros::tool]`), so any `assert!` on it is compile-time-constant
    // by construction — exactly what clippy's `assertions_on_constants` flags
    // as checking nothing at runtime. A `const` assertion embraces that fact
    // instead of fighting it: it fails to *compile* the moment
    // `WebCratesIoTool` stops declaring itself parallel-safe (other tools in
    // the workspace do declare `parallel_safe = false`, see
    // harw-tools/src/provider_macro.rs, so this is a real per-tool fact, not a
    // type-level tautology), which is a strictly stronger guarantee than the
    // runtime assertion it replaces.
    const _: () = assert!(WebCratesIoTool::PARALLEL_SAFE);

    /// `crate_name` ist das einzige Feld und Pflicht.
    #[test]
    fn test_crates_io_args_schema_requires_crate_name() -> TestResult {
        let harw_tools::ToolSpec::Function(spec) = WebCratesIoTool::spec();
        assert_eq!(spec.name.as_str(), "web.crates_io");
        let required = spec
            .parameters
            .required
            .ok_or(TestError::Missing("required-Liste"))?;
        assert_eq!(required, vec!["crate_name".to_owned()]);
        Ok(())
    }

    // --- Tests, die Netzzugriff bräuchten -----------------------------------

    /// Benötigt echten Netzzugriff auf die crates.io-API.
    ///
    /// Kein `unimplemented!` mehr (Bible R089/R101/R165): `#[ignore]` hält
    /// den Test ohnehin aus dem Default-Lauf heraus; würde er dennoch mit
    /// `--ignored` ausgeführt, meldet er sich als `Err` statt zu paniken.
    #[tokio::test]
    #[ignore = "benötigt echten Netzzugriff auf crates.io"]
    async fn test_web_crates_io_fetches_live_metadata() -> TestResult {
        Err(TestError::Unexpected(
            "echter Netzzugriff nicht erlaubt".to_owned(),
        ))
    }
}
