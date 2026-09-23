//! `web.docs_rs` — offizielle Crate-Dokumentation von docs.rs als Markdown.
//!
//! Spezifikationsquelle: AP W2-04..08, Abschnitt „4. `docs_rs.rs`".
//!
//! # Verantwortung
//! Dieses Modul baut die docs.rs-URL, holt sie über [`crate::fetch::WebFetcher`]
//! als Rohdokument, schneidet den Doku-Bereich (`#main-content`, Fallback
//! `body`) heraus und konvertiert ihn über [`crate::html::html_to_markdown`]
//! (bereinigt: script/style/noscript/versteckte Elemente) nach Markdown —
//! blockierend auf dem Blocking-Pool ([`crate::fetch::run_blocking`]). Es besitzt **keinen**
//! eigenen HTTP-Zugriff — die gesamte Netz-Ausgangstür liegt in
//! [`crate::fetch`].
//!
//! # Schlüsseltypen
//! - [`DocsRsArgs`] — Argument-Struktur des Tools.
//! - `WebDocsRsTool` — der von `#[harw_macros::tool]` erzeugte `ToolExecutor`.
//! - [`docs_rs_url`] — reine URL-Konstruktion inklusive Eingabevalidierung.
//!
//! # Sicherheitskontrakt
//! - Der Permission-Prolog (`network_access`) stammt aus dem Makro.
//! - `host_from` ist hier **nicht** anwendbar: die URL entsteht erst im Rumpf
//!   und existiert nicht als Argumentfeld. Der Host-Check läuft deshalb von
//!   Hand über [`harw_tools::host_from_url`] +
//!   [`harw_tools::require_host_access`], fail-closed bei nicht extrahierbarem
//!   Host — semantisch identisch zum generierten Prolog.
//! - Crate-Name, Version und Item-Pfad werden gegen eine Positivliste von
//!   Zeichen geprüft, bevor sie in eine URL gelangen; `..`, `//`, Schrägstriche
//!   im Crate-Namen und alles außerhalb der Positivliste führen zur Ablehnung.
//!
//! # Nebenläufigkeit
//! `WebDocsRsTool` ist eine Unit-Struktur, damit `Send + Sync + Copy`; das Tool
//! ist `parallel_safe` (reiner Lesezugriff).
//!
//! # Fehler
//! Alle Fehler münden in `Ok(ToolOutput::error(...))`; das Tool panickt nie.
//!
//! # Examples
//! ```rust
//! use harw_tool_web::docs_rs::docs_rs_url;
//!
//! assert_eq!(
//!     docs_rs_url("harw-tools", None, None).as_deref(),
//!     Some("https://docs.rs/harw-tools/latest/harw_tools/")
//! );
//! ```

use crate::fetch::{run_blocking, scoped_fetcher};
use crate::html::html_to_markdown;
use harw_macros::Tool;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::{Deserialize, Serialize};

/// Der einzige Host, den dieses Tool anspricht.
pub const DOCS_RS_HOST: &str = "docs.rs";

/// Obergrenze der zurückgegebenen Markdown-Zeichen.
pub const MAX_DOC_CHARS: usize = 20_000;

/// Obergrenze für Crate-Namen und Versionsangaben.
const MAX_SEGMENT_LEN: usize = 64;

/// Obergrenze für den Item-Pfad.
const MAX_ITEM_PATH_LEN: usize = 256;

/// Hinweiszeile, die an gekürzte Ausgaben angehängt wird.
const TRUNCATION_HINT: &str = "\n\n_[gekürzt: die Seite ist länger als das Ausgabelimit. Mit `item_path` gezielter anfragen.]_";

/// Argumente des Tools `web.docs_rs`.
///
/// # Description
/// Alle drei Felder sind modell-kontrolliert und werden von [`docs_rs_url`]
/// validiert, bevor daraus eine URL entsteht.
#[derive(Debug, Clone, Tool, Deserialize)]
#[tool(
    name = "web.docs_rs",
    description = "Holt die offizielle Dokumentation einer Crate von docs.rs als Markdown."
)]
pub struct DocsRsArgs {
    /// Name der Crate, z. B. `serde` oder `harw-tools`.
    pub crate_name: String,
    /// Version, z. B. `1.0.219`; ohne Angabe wird `latest` verwendet.
    #[serde(default)]
    pub version: Option<String>,
    /// Pfad innerhalb der Crate-Doku, z. B. `struct.Value.html` oder `de/index.html`.
    #[serde(default)]
    pub item_path: Option<String>,
}

/// JSON-Ausgabestruktur für `web.docs_rs`.
#[derive(Debug, Clone, Serialize)]
struct DocsRsResult {
    /// Die tatsächlich abgerufene URL.
    url: String,
    /// Die angefragte Crate.
    crate_name: String,
    /// Die verwendete Version (`latest`, wenn keine angegeben war).
    version: String,
    /// Ob die Antwort aus dem lokalen Cache stammt.
    from_cache: bool,
    /// Ob der Inhalt gekürzt wurde.
    truncated: bool,
    /// Die Dokumentation als Markdown.
    markdown: String,
}

/// Prüft einen Crate-Namen gegen die zulässige Zeichenmenge.
///
/// crates.io erlaubt ASCII-Buchstaben, Ziffern, `-` und `_`. Alles andere
/// (Schrägstriche, Punkte, Prozentzeichen) könnte den URL-Pfad verlassen.
fn is_safe_crate_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SEGMENT_LEN
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Prüft eine Versionsangabe gegen die zulässige Zeichenmenge.
///
/// Erlaubt sind Semver-Zeichen sowie das Schlüsselwort `latest`; `..` ist
/// ausgeschlossen, damit kein Pfad-Traversal entsteht.
fn is_safe_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SEGMENT_LEN
        && !value.contains("..")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
}

/// Prüft einen Item-Pfad gegen die zulässige Zeichenmenge.
///
/// Erlaubt sind Pfadsegmente aus Buchstaben, Ziffern, `.`, `_`, `-` und `/`.
/// `..` und `//` sind ausgeschlossen; ein absoluter Pfad wird vom Aufrufer
/// entschärft, indem führende `/` entfernt werden.
fn is_safe_item_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ITEM_PATH_LEN
        && !value.contains("..")
        && !value.contains("//")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

/// Baut die docs.rs-URL für eine Crate.
///
/// # Description
/// Schema: `https://docs.rs/<crate>/<version|latest>/<crate_mit_unterstrichen>/`
/// plus optionalem Item-Pfad. Der erste Pfadabschnitt ist der Crate-Name **wie
/// veröffentlicht** (mit Bindestrichen), der dritte der Modulname, in dem
/// Bindestriche zu Unterstrichen werden — genau so legt docs.rs die Seiten ab.
///
/// # Arguments
/// - `crate_name` (`&str`): Name der Crate; nur `[A-Za-z0-9_-]`.
/// - `version` (`Option<&str>`): Version; `None` oder leer bedeutet `latest`.
/// - `item_path` (`Option<&str>`): Pfad innerhalb der Doku; führende `/` werden
///   entfernt.
///
/// # Returns
/// `Some(String)` mit der vollständigen URL, oder `None`, wenn eine der
/// Eingaben die Zeichen-Positivliste verletzt. `None` ist eine Ablehnung, kein
/// Fehlschlag: der Aufrufer meldet sie als Tool-Fehler.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_tool_web::docs_rs::docs_rs_url;
///
/// assert_eq!(
///     docs_rs_url("serde", Some("1.0.219"), Some("struct.Error.html")).as_deref(),
///     Some("https://docs.rs/serde/1.0.219/serde/struct.Error.html")
/// );
/// assert_eq!(docs_rs_url("../etc", None, None), None);
/// ```
#[must_use]
pub fn docs_rs_url(
    crate_name: &str,
    version: Option<&str>,
    item_path: Option<&str>,
) -> Option<String> {
    let name = crate_name.trim();
    if !is_safe_crate_name(name) {
        return None;
    }

    let version = match version.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) if is_safe_version(value) => value,
        Some(_) => return None,
        None => "latest",
    };

    // docs.rs legt das Modul unter dem Crate-Namen mit Unterstrichen ab.
    let module = name.replace('-', "_");
    let mut url = format!("https://{DOCS_RS_HOST}/{name}/{version}/{module}/");

    if let Some(item) = item_path.map(str::trim).filter(|value| !value.is_empty()) {
        let item = item.trim_start_matches('/');
        if !is_safe_item_path(item) {
            return None;
        }
        url.push_str(item);
    }

    Some(url)
}

/// Schneidet den Dokumentationsbereich aus einer docs.rs-Seite heraus.
///
/// # Description
/// Bevorzugt `#main-content` (der Container, in dem rustdoc den eigentlichen
/// Inhalt ablegt), fällt auf `body` zurück und zuletzt auf das gesamte
/// Dokument. Damit landen Navigation, Suchleiste und Skripte nicht im
/// Ergebnis.
///
/// # Arguments
/// - `html` (`&str`): das vollständige HTML-Dokument.
///
/// # Returns
/// Das innere HTML des gefundenen Containers.
///
/// # Panics
/// Nie: schlägt ein Selektor fehl, greift der nächste Fallback.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_tool_web::docs_rs::extract_main_content;
///
/// let html = "<html><body><nav>x</nav><div id=\"main-content\"><p>Doku</p></div></body></html>";
/// assert!(extract_main_content(html).contains("Doku"));
/// ```
#[must_use]
pub fn extract_main_content(html: &str) -> String {
    let document = scraper::Html::parse_document(html);

    for pattern in ["#main-content", "body"] {
        let Ok(selector) = scraper::Selector::parse(pattern) else {
            continue;
        };
        if let Some(element) = document.select(&selector).next() {
            return element.inner_html();
        }
    }

    html.to_owned()
}

/// Kürzt einen Text auf eine Zeichenzahl und hängt einen Hinweis an.
///
/// # Description
/// Gezählt werden `char`s, nicht Bytes, damit der Schnitt nie mitten in einem
/// Unicode-Zeichen liegt.
///
/// # Arguments
/// - `text` (`&str`): der zu kürzende Text.
/// - `max_chars` (`usize`): die Obergrenze in Zeichen.
///
/// # Returns
/// Ein Paar aus dem (ggf. gekürzten) Text und einem Flag, ob gekürzt wurde.
///
/// # Panics
/// Nie.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_tool_web::docs_rs::truncate_with_hint;
///
/// let (text, truncated) = truncate_with_hint("abcdef", 3);
/// assert!(truncated);
/// assert!(text.starts_with("abc"));
/// ```
#[must_use]
pub fn truncate_with_hint(text: &str, max_chars: usize) -> (String, bool) {
    let mut kept = String::new();

    for (count, ch) in text.chars().enumerate() {
        if count == max_chars {
            kept.push_str(TRUNCATION_HINT);
            return (kept, true);
        }
        kept.push(ch);
    }

    (kept, false)
}

/// Führt `web.docs_rs` aus.
///
/// Der Permission-Prolog stammt aus dem Makro; der Host-Check erfolgt von Hand,
/// weil die URL erst hier entsteht (siehe Modul-Doku).
#[harw_macros::tool(
    name = "web.docs_rs",
    description = "Holt die offizielle Dokumentation einer Crate von docs.rs als Markdown.",
    permission = "network_access",
    parallel_safe
)]
async fn web_docs_rs(
    context: &ToolExecutionContext,
    args: DocsRsArgs,
) -> Result<ToolOutput, ToolsError> {
    let version = args
        .version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("latest")
        .to_owned();

    let Some(url) = docs_rs_url(
        &args.crate_name,
        args.version.as_deref(),
        args.item_path.as_deref(),
    ) else {
        return Ok(ToolOutput::error(format!(
            "Tool '{}': ungültiger Crate-Name, ungültige Version oder ungültiger Item-Pfad",
            WebDocsRsTool::NAME
        )));
    };

    // Fail-closed: ohne extrahierbaren Host wird die Allowlist nicht geprüft.
    let Some(host) = harw_tools::host_from_url(&url) else {
        return Ok(ToolOutput::error(format!(
            "Tool '{}': aus der gebauten URL ließ sich kein Hostname lesen",
            WebDocsRsTool::NAME
        )));
    };
    if let Some(denied) = harw_tools::require_host_access(context, &host, WebDocsRsTool::NAME) {
        return Ok(denied);
    }

    let fetcher = match scoped_fetcher(context, None) {
        Ok(fetcher) => fetcher,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };

    // Rohkörper holen: die HTML-Struktur wird gebraucht, um `#main-content`
    // herauszuschneiden. Der Körper ist byte-gekappt, aber noch nicht
    // textgekappt; die Kappung auf `MAX_DOC_CHARS` folgt nach der Konvertierung.
    let document = match fetcher.fetch_source(&url).await {
        Ok(document) => document,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };

    // Parsen, Bereinigen (script/style/versteckte Elemente) und Konvertieren
    // sind CPU-gebunden und laufen auf dem Blocking-Pool (F-169).
    let body = document.body.clone();
    let converted = run_blocking("docs-rs-markdown", move || {
        let fragment = extract_main_content(&body);
        let markdown = html_to_markdown(&fragment)?;
        Ok(truncate_with_hint(markdown.trim(), MAX_DOC_CHARS))
    })
    .await;
    let (markdown, truncated) = match converted {
        Ok(result) => result,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };

    let result = DocsRsResult {
        url: document.url,
        crate_name: args.crate_name,
        version,
        from_cache: document.from_cache,
        truncated,
        markdown,
    };

    match serde_json::to_value(&result) {
        Ok(value) => Ok(ToolOutput::json(value)),
        Err(err) => Err(ToolsError::from(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    // --- URL-Bau ------------------------------------------------------------

    /// Ohne Version wird `latest` verwendet.
    #[test]
    fn test_docs_rs_url_without_version_uses_latest() {
        assert_eq!(
            docs_rs_url("serde", None, None).as_deref(),
            Some("https://docs.rs/serde/latest/serde/")
        );
    }

    /// Ein leerer Versions-String zählt wie „keine Version".
    #[test]
    fn test_docs_rs_url_blank_version_uses_latest() {
        assert_eq!(
            docs_rs_url("serde", Some("   "), None).as_deref(),
            Some("https://docs.rs/serde/latest/serde/")
        );
    }

    /// Mit Version steht die Version im zweiten Pfadabschnitt.
    #[test]
    fn test_docs_rs_url_with_version() {
        assert_eq!(
            docs_rs_url("serde", Some("1.0.219"), None).as_deref(),
            Some("https://docs.rs/serde/1.0.219/serde/")
        );
    }

    /// Bindestriche werden nur im Modulabschnitt zu Unterstrichen.
    #[test]
    fn test_docs_rs_url_maps_dashes_to_underscores_in_module_segment_only() {
        assert_eq!(
            docs_rs_url("harw-tool-web", Some("0.2.0"), None).as_deref(),
            Some("https://docs.rs/harw-tool-web/0.2.0/harw_tool_web/")
        );
    }

    /// Ein Item-Pfad wird angehängt.
    #[test]
    fn test_docs_rs_url_appends_item_path() {
        assert_eq!(
            docs_rs_url("serde_json", None, Some("struct.Value.html")).as_deref(),
            Some("https://docs.rs/serde_json/latest/serde_json/struct.Value.html")
        );
    }

    /// Ein führender Schrägstrich im Item-Pfad erzeugt kein `//`.
    #[test]
    fn test_docs_rs_url_strips_leading_slash_of_item_path() {
        assert_eq!(
            docs_rs_url("serde", None, Some("/de/index.html")).as_deref(),
            Some("https://docs.rs/serde/latest/serde/de/index.html")
        );
    }

    /// Pfad-Traversal im Crate-Namen wird abgelehnt.
    #[test]
    fn test_docs_rs_url_rejects_traversal_in_crate_name() {
        assert_eq!(docs_rs_url("../../etc/passwd", None, None), None);
        assert_eq!(docs_rs_url("serde/../evil", None, None), None);
    }

    /// Ein Host-Wechsel über den Crate-Namen ist unmöglich.
    #[test]
    fn test_docs_rs_url_rejects_host_injection_in_crate_name() {
        assert_eq!(docs_rs_url("evil.test%2f..", None, None), None);
        assert_eq!(docs_rs_url("a@evil.test", None, None), None);
        assert_eq!(docs_rs_url("", None, None), None);
    }

    /// Traversal im Item-Pfad und in der Version wird abgelehnt.
    #[test]
    fn test_docs_rs_url_rejects_traversal_in_version_and_item_path() {
        assert_eq!(docs_rs_url("serde", Some("../.."), None), None);
        assert_eq!(docs_rs_url("serde", None, Some("../../etc/passwd")), None);
        assert_eq!(docs_rs_url("serde", None, Some("a//b")), None);
    }

    /// Überlange Eingaben werden abgelehnt.
    #[test]
    fn test_docs_rs_url_rejects_oversized_segments() {
        let long_name = "a".repeat(MAX_SEGMENT_LEN + 1);
        assert_eq!(docs_rs_url(&long_name, None, None), None);

        let long_path = "a".repeat(MAX_ITEM_PATH_LEN + 1);
        assert_eq!(docs_rs_url("serde", None, Some(&long_path)), None);
    }

    /// Jede gebaute URL ist https und hat docs.rs als Host.
    #[test]
    fn test_docs_rs_url_is_always_https_on_docs_rs() -> TestResult {
        let url = docs_rs_url("serde", None, None).ok_or(TestError::Missing("gültige URL"))?;
        assert!(url.starts_with("https://"));
        assert_eq!(
            harw_tools::host_from_url(&url).as_deref(),
            Some(DOCS_RS_HOST)
        );
        Ok(())
    }

    // --- Inhaltsextraktion --------------------------------------------------

    /// `#main-content` wird bevorzugt, Navigation entfällt.
    #[test]
    fn test_extract_main_content_prefers_main_content_container() {
        let html = concat!(
            "<html><body><nav>NAVIGATION</nav>",
            "<div id=\"main-content\"><h1>Doku</h1></div>",
            "</body></html>"
        );
        let extracted = extract_main_content(html);
        assert!(extracted.contains("Doku"), "unerwartet: {extracted}");
        assert!(
            !extracted.contains("NAVIGATION"),
            "Navigation darf nicht mitkommen: {extracted}"
        );
    }

    /// Ohne `#main-content` greift der `body`-Fallback.
    #[test]
    fn test_extract_main_content_falls_back_to_body() {
        let html = "<html><body><p>Nur Body</p></body></html>";
        let extracted = extract_main_content(html);
        assert!(extracted.contains("Nur Body"), "unerwartet: {extracted}");
    }

    /// Das Ergebnis lässt sich weiter nach Markdown konvertieren.
    #[test]
    fn test_extract_main_content_result_converts_to_markdown() -> TestResult {
        let html = "<html><body><div id=\"main-content\"><h1>Titel</h1></div></body></html>";
        let markdown = html_to_markdown(&extract_main_content(html))
            .map_err(ctx("die Konvertierung muss gelingen"))?;
        assert!(markdown.contains("Titel"), "unerwartet: {markdown}");
        Ok(())
    }

    // --- Kürzung ------------------------------------------------------------

    /// Kurze Texte bleiben unverändert und ohne Hinweis.
    #[test]
    fn test_truncate_with_hint_leaves_short_text_untouched() {
        let (text, truncated) = truncate_with_hint("kurz", MAX_DOC_CHARS);
        assert_eq!(text, "kurz");
        assert!(!truncated);
    }

    /// Lange Texte werden gekappt und tragen die Hinweiszeile.
    #[test]
    fn test_truncate_with_hint_marks_truncated_text() {
        let source = "x".repeat(50);
        let (text, truncated) = truncate_with_hint(&source, 10);
        assert!(truncated);
        assert!(text.starts_with(&"x".repeat(10)));
        assert!(text.contains("gekürzt"), "unerwartet: {text}");
    }

    /// Der Schnitt liegt nie mitten in einem Mehrbyte-Zeichen.
    #[test]
    fn test_truncate_with_hint_cuts_on_char_boundary() {
        let source = "äöüß".repeat(10);
        let (text, truncated) = truncate_with_hint(&source, 3);
        assert!(truncated);
        assert!(text.starts_with("äöü"), "unerwartet: {text}");
    }

    // --- Tool-Deklaration ---------------------------------------------------

    /// Das Tool deklariert Netz-Berechtigung und Parallelsicherheit (Letztere
    /// als Compile-Zeit-Assertion, siehe unten).
    #[test]
    fn test_web_docs_rs_tool_declares_network_permission() {
        assert_eq!(WebDocsRsTool::NAME, "web.docs_rs");
        assert_eq!(
            WebDocsRsTool::PERMISSION,
            Some(harw_tools::Permission::NetworkAccess)
        );
    }

    // `PARALLEL_SAFE` is a macro-generated `const bool` (see
    // `#[harw_macros::tool]`), so any `assert!` on it is compile-time-constant
    // by construction — exactly what clippy's `assertions_on_constants` flags
    // as checking nothing at runtime. A `const` assertion embraces that fact
    // instead of fighting it: it fails to *compile* the moment `WebDocsRsTool`
    // stops declaring itself parallel-safe (other tools in the workspace do
    // declare `parallel_safe = false`, see harw-tools/src/provider_macro.rs,
    // so this is a real per-tool fact, not a type-level tautology), which is a
    // strictly stronger guarantee than the runtime assertion it replaces.
    const _: () = assert!(WebDocsRsTool::PARALLEL_SAFE);

    /// Nur `crate_name` ist Pflicht.
    #[test]
    fn test_docs_rs_args_schema_requires_crate_name_only() -> TestResult {
        let harw_tools::ToolSpec::Function(spec) = WebDocsRsTool::spec();
        assert_eq!(spec.name.as_str(), "web.docs_rs");
        let required = spec
            .parameters
            .required
            .ok_or(TestError::Missing("required-Liste"))?;
        assert_eq!(required, vec!["crate_name".to_owned()]);
        Ok(())
    }

    // --- Tests, die einen laufenden Server bräuchten ------------------------

    /// Benötigt Netzzugriff auf docs.rs; im CI nicht ausführbar.
    ///
    /// Kein `unimplemented!` mehr (Bible R089/R101/R165): `#[ignore]` hält
    /// den Test ohnehin aus dem Default-Lauf heraus; würde er dennoch mit
    /// `--ignored` ausgeführt, meldet er sich als `Err` statt zu paniken.
    #[tokio::test]
    #[ignore = "benötigt echten Netzzugriff auf docs.rs"]
    async fn test_web_docs_rs_fetches_live_documentation() -> TestResult {
        Err(TestError::Unexpected(
            "echter Netzzugriff nicht erlaubt".to_owned(),
        ))
    }
}
