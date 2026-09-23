//! `web.search` — Websuche über konfigurierbare Such-Backends.
//!
//! # Verantwortung
//! Dieses Modul liefert zu einer Suchanfrage eine kompakte Trefferliste
//! (`title`, `url`, `snippet`), damit ein Rechercheur anschließend gezielt
//! `web.fetch`/`web.docs_rs` aufrufen kann. Unterstützte Backends
//! ([`SearchBackend`]):
//!
//! | Backend | Anfrage | Schlüssel |
//! |---|---|---|
//! | Brave | `GET https://api.search.brave.com/res/v1/web/search?q=…&count=…` | Header `X-Subscription-Token` |
//! | Tavily | `POST https://api.tavily.com/search` (JSON) | `Authorization: Bearer …` |
//! | SearXNG | `GET {endpoint}/search?format=json&q=…` | keiner |
//! | DuckDuckGo | `GET https://html.duckduckgo.com/html/?q=…` (HTML) | keiner (Fallback, Standard) |
//!
//! # Schlüsseltypen
//! - [`WebSearchConfig`] / [`install_search_config`] — prozessweite Konfiguration.
//! - [`SearchBackend`] — Auswahl des Backends.
//! - [`SearchArgs`] / `WebSearchTool` — Argumente und `ToolExecutor` von `web.search`.
//! - [`SearchResult`] / [`SearchResponse`] — Ausgabeform.
//!
//! # Sicherheitskontrakt
//! 1. **Berechtigung vor Argumenten** über
//!    `#[harw_macros::tool(permission = "network_access")]`.
//! 2. **Nur der Egress-Client.** Der `reqwest::Client` entsteht ausschließlich
//!    über [`harw_egress::build_client`] mit der Policy des über
//!    [`crate::configure`] installierten Basis-Fetchers. Ohne diesen scheitert
//!    jeder Aufruf mit [`WebToolError::NotConfigured`] — auch der
//!    schlüssellose DuckDuckGo-Fallback.
//! 3. **Jede Anfrage-URL wird vor dem Senden geprüft** — Sandbox-Hostcheck
//!    ([`harw_tools::require_host_access`]) und
//!    [`crate::fetch::WebFetcher::check_target`] (Schema, Egress-Policy,
//!    Sandbox-Scope). Weiterleitungen werden **nicht** gefolgt; ein `3xx` ist
//!    ein [`WebToolError::UpstreamStatus`].
//! 4. **Zugangsdaten verlassen den Prozess nur im Header.** Der API-Schlüssel
//!    steht nie in einer URL, nie in einer Meldung und nie im Log; `Debug` von
//!    [`WebSearchConfig`] schwärzt ihn, der Header ist als `sensitive`
//!    markiert.
//! 5. **Kappung und Positivliste** wie bei `web.fetch`: Byte-Limit beim Lesen
//!    ([`crate::fetch::push_chunk`]), Content-Type über
//!    [`crate::fetch::check_content_type`], Einzel-Timeout
//!    [`REQUEST_TIMEOUT_SECS`] und Gesamt-Deadline [`TOTAL_DEADLINE_SECS`].
//! 6. **Bereinigte Treffer.** Titel und Snippets werden von HTML befreit,
//!    Steuer- und Bidi-Zeichen entfernt und gekappt ([`MAX_SNIPPET_CHARS`]);
//!    Treffer-URLs müssen `http(s)` mit Host und ohne Userinfo sein.
//! 7. **Kein Cache.** Suchergebnisse sind flüchtig und Anfragen tragen
//!    Zugangsdaten; nichts wird auf Platte geschrieben.
//!
//! # Nebenläufigkeit
//! Die Konfiguration liegt in einem prozessweiten `RwLock`; der Client in
//! einem `OnceLock`. `WebSearchTool` ist `parallel_safe`.
//!
//! # Fehler
//! Alle Fehler sind [`WebToolError`]-Varianten; an der Tool-Grenze werden sie
//! zu `Ok(ToolOutput::error(...))`.
//!
//! # Examples
//! ```rust
//! use harw_tool_web::search::{SearchBackend, WebSearchConfig, install_search_config};
//!
//! install_search_config(WebSearchConfig {
//!     provider: SearchBackend::Searxng,
//!     endpoint: Some("https://searx.example.org".into()),
//!     api_key: None,
//!     max_results: 8,
//! });
//! ```

use crate::error::{WebToolError, WebToolResult};
use crate::fetch::{
    REQUEST_TIMEOUT_SECS, TOTAL_DEADLINE_SECS, WebFetcher, check_content_type, push_chunk,
    run_blocking, scoped_fetcher,
};
use crate::hop::{HopTarget, map_send_error};
use harw_egress::EgressPolicy;
use harw_macros::Tool;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

/// Standard-Trefferzahl.
pub const DEFAULT_MAX_RESULTS: u8 = 8;

/// Harte Obergrenze der Trefferzahl.
pub const HARD_MAX_RESULTS: u8 = 20;

/// Maximale Länge der Suchanfrage in Zeichen.
pub const MAX_QUERY_CHARS: usize = 400;

/// Maximale Länge eines Snippets in Zeichen.
pub const MAX_SNIPPET_CHARS: usize = 300;

/// Maximale Länge eines Titels in Zeichen.
pub const MAX_TITLE_CHARS: usize = 200;

/// Maximale Länge einer Treffer-URL in Bytes.
const MAX_RESULT_URL_BYTES: usize = 2_048;

/// Maximale Länge des `site`-Arguments (DNS-Grenze).
const MAX_SITE_CHARS: usize = 253;

/// Byte-Obergrenze einer Suchantwort (unabhängig vom Fetch-Limit höchstens 2 MiB).
const MAX_RESPONSE_BYTES: usize = 2 * 1_048_576;

/// Standard-Endpunkt der Brave-Search-API.
const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

/// Standard-Endpunkt der Tavily-API.
const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";

/// Standard-Endpunkt der DuckDuckGo-HTML-Suche.
const DUCKDUCKGO_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

/// User-Agent aller Suchanfragen (identisch zu `web.fetch`).
const USER_AGENT_VALUE: &str = concat!("harwness-web-tools/", env!("CARGO_PKG_VERSION"));

/// Prozessweite Suchkonfiguration; `None` bedeutet [`WebSearchConfig::default`].
static SEARCH_CONFIG: RwLock<Option<WebSearchConfig>> = RwLock::new(None);

/// Egress-Client der Suche samt Digest der Policy, aus der er gebaut wurde.
static SEARCH_CLIENT: OnceLock<([u8; 32], reqwest::Client)> = OnceLock::new();

// ---------------------------------------------------------------------------
// Konfiguration
// ---------------------------------------------------------------------------

/// Das Such-Backend.
///
/// # Description
/// `DuckDuckGo` ist der schlüssellose Standard. Brave und Tavily benötigen
/// einen API-Schlüssel, SearXNG einen Endpunkt.
///
/// # Concurrency
/// Reine Daten; `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchBackend {
    /// Brave Search API.
    Brave,
    /// Tavily Search API.
    Tavily,
    /// Selbst betriebene SearXNG-Instanz (JSON-Ausgabe muss aktiviert sein).
    Searxng,
    /// DuckDuckGo-HTML-Seite (ohne Schlüssel).
    #[default]
    DuckDuckGo,
}

impl SearchBackend {
    /// Der stabile Name des Backends in der Tool-Ausgabe.
    ///
    /// # Returns
    /// `"brave"`, `"tavily"`, `"searxng"` oder `"duckduckgo"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Brave => "brave",
            Self::Tavily => "tavily",
            Self::Searxng => "searxng",
            Self::DuckDuckGo => "duckduckgo",
        }
    }
}

/// Konfiguration von `web.search`.
///
/// # Description
/// `endpoint` überschreibt den Standard-Endpunkt (für SearXNG Pflicht, die
/// Basis-URL der Instanz). `api_key` ist für Brave und Tavily Pflicht und wird
/// nie protokolliert. `max_results` ist die Standard-Trefferzahl (auf
/// `1..=`[`HARD_MAX_RESULTS`] gedeckelt). Der Ziel-Host muss zusätzlich in der
/// Egress-Policy und im Sandbox-Scope freigegeben sein.
///
/// # Concurrency
/// Reine Daten.
#[derive(Clone, PartialEq, Eq)]
pub struct WebSearchConfig {
    /// Das verwendete Backend.
    pub provider: SearchBackend,
    /// Optionaler Endpunkt (für SearXNG die Basis-URL der Instanz).
    pub endpoint: Option<String>,
    /// API-Schlüssel (Brave, Tavily); wird nie ausgegeben.
    pub api_key: Option<String>,
    /// Standard-Trefferzahl.
    pub max_results: u8,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            provider: SearchBackend::DuckDuckGo,
            endpoint: None,
            api_key: None,
            max_results: DEFAULT_MAX_RESULTS,
        }
    }
}

impl std::fmt::Debug for WebSearchConfig {
    /// Zeigt die Konfiguration mit geschwärztem API-Schlüssel.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSearchConfig")
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint)
            .field(
                "api_key",
                &self.api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("max_results", &self.max_results)
            .finish()
    }
}

/// Hinterlegt die prozessweite Suchkonfiguration.
///
/// # Description
/// Ersetzt eine zuvor hinterlegte Konfiguration. Ohne Aufruf gilt
/// [`WebSearchConfig::default`] (DuckDuckGo). Die Egress-Policy selbst kommt
/// weiterhin ausschließlich aus [`crate::configure`].
///
/// # Arguments
/// - `cfg` ([`WebSearchConfig`]): die neue Konfiguration.
///
/// # Concurrency
/// Thread-sicher über `RwLock`; ein vergifteter Lock wird übernommen.
pub fn install_search_config(cfg: WebSearchConfig) {
    let mut guard = SEARCH_CONFIG
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(cfg);
}

/// Liefert eine Kopie der aktuell wirksamen Suchkonfiguration.
///
/// # Returns
/// Die hinterlegte oder die Standard-Konfiguration.
///
/// # Concurrency
/// Thread-sicher über `RwLock`.
#[must_use]
pub fn search_config() -> WebSearchConfig {
    SEARCH_CONFIG
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Argumente und Ergebnis
// ---------------------------------------------------------------------------

/// Argumente des Tools `web.search`.
///
/// # Description
/// Vom Modell geliefert und damit **untrusted**; geprüft in
/// [`SearchQuery::from_args`].
#[derive(Debug, Clone, Tool, Deserialize)]
#[tool(
    name = "web.search",
    description = "Searches the web and returns a short list of results (title, url, snippet). Use web.fetch to read a result."
)]
pub struct SearchArgs {
    /// Search query (1 to 400 characters).
    pub query: String,
    /// Maximum number of results (1 to 20, default 8).
    #[tool(default = 8)]
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub max_results: Option<u64>,
    /// Restrict results to this domain, e.g. "docs.rs".
    #[serde(default)]
    pub site: Option<String>,
}

/// Ein einzelner Suchtreffer (bereinigt).
///
/// # Concurrency
/// Reine Daten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchResult {
    /// Titel (ohne HTML, gekappt).
    pub title: String,
    /// Normalisierte `http(s)`-URL.
    pub url: String,
    /// Kurzbeschreibung (ohne HTML, höchstens [`MAX_SNIPPET_CHARS`] Zeichen).
    pub snippet: String,
}

/// Die Tool-Ausgabe von `web.search`.
///
/// # Concurrency
/// Reine Daten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchResponse {
    /// Name des Backends ([`SearchBackend::as_str`]).
    pub provider: &'static str,
    /// Die gesendete Suchanfrage (ohne `site:`-Zusatz).
    pub query: String,
    /// Die Treffer.
    pub results: Vec<SearchResult>,
}

/// Eine geprüfte Suchanfrage.
///
/// # Concurrency
/// Reine Daten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Getrimmte Anfrage, 1..=[`MAX_QUERY_CHARS`] Zeichen, ohne Steuerzeichen.
    pub query: String,
    /// Trefferzahl in `1..=`[`HARD_MAX_RESULTS`].
    pub max_results: u8,
    /// Normalisierte Domain-Einschränkung.
    pub site: Option<String>,
}

impl SearchQuery {
    /// Prüft und normalisiert die Tool-Argumente.
    ///
    /// # Arguments
    /// - `args` (`&SearchArgs`): Argumente des Modells.
    /// - `default_max` (`u8`): Standard-Trefferzahl aus der Konfiguration.
    ///
    /// # Returns
    /// Die geprüfte Anfrage.
    ///
    /// # Errors
    /// Eine Meldung für das Modell, wenn `query` leer/zu lang oder `site`
    /// keine gültige Domain ist.
    ///
    /// # Concurrency
    /// Rein.
    pub fn from_args(args: &SearchArgs, default_max: u8) -> Result<Self, String> {
        let query: String = args
            .query
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .trim()
            .to_owned();
        let count = query.chars().count();
        if count == 0 {
            return Err("query must not be empty".to_owned());
        }
        if count > MAX_QUERY_CHARS {
            return Err(format!("query exceeds {MAX_QUERY_CHARS} characters"));
        }

        let requested = args
            .max_results
            .unwrap_or_else(|| u64::from(default_max.clamp(1, HARD_MAX_RESULTS)));
        let max_results = u8::try_from(requested.clamp(1, u64::from(HARD_MAX_RESULTS)))
            .unwrap_or(HARD_MAX_RESULTS);

        let site = match args.site.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => Some(normalize_site(raw).ok_or_else(|| {
                "site must be a plain domain name such as 'docs.rs'".to_owned()
            })?),
        };

        Ok(Self {
            query,
            max_results,
            site,
        })
    }

    /// Die an das Backend gesendete Anfrage (mit `site:`-Operator, außer Tavily).
    fn effective_query(&self, backend: SearchBackend) -> String {
        match (&self.site, backend) {
            (Some(site), backend) if backend != SearchBackend::Tavily => {
                format!("{} site:{site}", self.query)
            }
            _ => self.query.clone(),
        }
    }
}

/// Normalisiert eine Domain-Einschränkung (nur `[a-z0-9.-]`, mindestens ein Punkt).
fn normalize_site(raw: &str) -> Option<String> {
    let site = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    let valid = !site.is_empty()
        && site.len() <= MAX_SITE_CHARS
        && site.contains('.')
        && site
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && site
            .split('.')
            .all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-'));
    valid.then_some(site)
}

// ---------------------------------------------------------------------------
// Bereinigung
// ---------------------------------------------------------------------------

/// Sagt, ob ein Zeichen unsichtbar formatiert (Zero-Width, Bidi-Steuerung).
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// Entfernt HTML (nur bei `is_html`), Steuer- und Bidi-Zeichen, faltet
/// Leerraum und kappt auf `max_chars` Zeichen (mit `…` bei Kappung).
///
/// `is_html = false` für bereits aus dem DOM gelesenen Text: ein erneutes
/// Parsen würde dekodierte Entities (`&lt;` → `<`) doppelt interpretieren.
///
/// # Concurrency
/// Rein, aber bei HTML-Anteil parsend; im async-Kontext über `run_blocking`.
fn clean_text(raw: &str, max_chars: usize, is_html: bool) -> String {
    let text = if is_html && (raw.contains('<') || raw.contains('&')) {
        let fragment = scraper::Html::parse_fragment(raw);
        fragment.root_element().text().collect::<String>()
    } else {
        raw.to_owned()
    };
    let mut collapsed = String::with_capacity(text.len().min(max_chars * 4));
    for word in text
        .chars()
        .filter(|c| !is_format_char(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
    {
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
    }
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let mut kept: String = collapsed.chars().take(max_chars.saturating_sub(1)).collect();
    kept.truncate(kept.trim_end().len());
    kept.push('…');
    kept
}

/// Prüft eine Treffer-URL: `http(s)`, Host vorhanden, keine Userinfo.
///
/// # Returns
/// Die normalisierte URL, sonst `None`.
fn sanitize_result_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_RESULT_URL_BYTES {
        return None;
    }
    let parsed = reqwest::Url::parse(trimmed).ok()?;
    let scheme_ok = matches!(parsed.scheme(), "http" | "https");
    let has_host = parsed.host_str().is_some_and(|host| !host.is_empty());
    let no_userinfo = parsed.username().is_empty() && parsed.password().is_none();
    (scheme_ok && has_host && no_userinfo).then(|| parsed.to_string())
}

/// Sagt, ob eine (bereits geprüfte) URL zur Domain `site` gehört.
fn url_matches_site(url: &str, site: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == site || host.ends_with(&format!(".{site}")))
}

/// Sammelt Rohtreffer bereinigt, dedupliziert, gefiltert und begrenzt.
///
/// `is_html` gibt an, ob Titel und Snippets noch HTML-Markup enthalten können
/// (JSON-Backends) oder bereits reiner Text sind (DuckDuckGo-DOM).
fn collect_results<I>(
    raw: I,
    limit: usize,
    site: Option<&str>,
    is_html: bool,
) -> Vec<SearchResult>
where
    I: IntoIterator<Item = (String, String, String)>,
{
    let mut results: Vec<SearchResult> = Vec::new();
    for (title, url, snippet) in raw {
        if results.len() >= limit {
            break;
        }
        let Some(url) = sanitize_result_url(&url) else {
            continue;
        };
        if site.is_some_and(|site| !url_matches_site(&url, site)) {
            continue;
        }
        if results.iter().any(|existing| existing.url == url) {
            continue;
        }
        let mut title = clean_text(&title, MAX_TITLE_CHARS, is_html);
        if title.is_empty() {
            title.clone_from(&url);
        }
        results.push(SearchResult {
            title,
            url,
            snippet: clean_text(&snippet, MAX_SNIPPET_CHARS, is_html),
        });
    }
    results
}

// ---------------------------------------------------------------------------
// Reine Parser
// ---------------------------------------------------------------------------

/// Teilschema eines Treffers mit `title`/`url` und wahlweise `description`
/// (Brave) oder `content` (Tavily, SearXNG).
#[derive(Debug, Deserialize)]
struct JsonHit {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

impl JsonHit {
    fn into_triple(self) -> (String, String, String) {
        (
            self.title.unwrap_or_default(),
            self.url.unwrap_or_default(),
            self.description.or(self.content).unwrap_or_default(),
        )
    }
}

/// Teilschema einer Antwort mit `results` auf oberster Ebene.
#[derive(Debug, Deserialize)]
struct ResultsEnvelope {
    #[serde(default)]
    results: Vec<JsonHit>,
}

/// Teilschema der Brave-Antwort (`web.results`).
#[derive(Debug, Deserialize)]
struct BraveEnvelope {
    #[serde(default)]
    web: Option<ResultsEnvelope>,
}

/// Parst eine Brave-Antwort (`{"web":{"results":[{title,url,description}]}}`).
///
/// # Errors
/// - [`WebToolError::Json`]: kein gültiges JSON / falsches Schema.
fn parse_brave(raw: &str, limit: usize, site: Option<&str>) -> WebToolResult<Vec<SearchResult>> {
    let envelope: BraveEnvelope = serde_json::from_str(raw)?;
    let hits = envelope.web.map(|web| web.results).unwrap_or_default();
    Ok(collect_results(
        hits.into_iter().map(JsonHit::into_triple),
        limit,
        site,
        true,
    ))
}

/// Parst eine Tavily-Antwort (`{"results":[{title,url,content}]}`).
///
/// # Errors
/// - [`WebToolError::Json`]: kein gültiges JSON / falsches Schema.
fn parse_tavily(raw: &str, limit: usize, site: Option<&str>) -> WebToolResult<Vec<SearchResult>> {
    let envelope: ResultsEnvelope = serde_json::from_str(raw)?;
    Ok(collect_results(
        envelope.results.into_iter().map(JsonHit::into_triple),
        limit,
        site,
        true,
    ))
}

/// Parst eine SearXNG-JSON-Antwort (`{"results":[{title,url,content}]}`).
///
/// # Errors
/// - [`WebToolError::Json`]: kein gültiges JSON / falsches Schema.
fn parse_searxng(raw: &str, limit: usize, site: Option<&str>) -> WebToolResult<Vec<SearchResult>> {
    parse_tavily(raw, limit, site)
}

/// Löst einen DuckDuckGo-Ergebnislink auf (`//duckduckgo.com/l/?uddg=…`).
///
/// # Returns
/// Die Ziel-URL; Links auf DuckDuckGo selbst (Werbung, interne Seiten) ergeben `None`.
fn decode_ddg_href(href: &str) -> Option<String> {
    let href = href.trim();
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else if href.starts_with('/') {
        format!("https://duckduckgo.com{href}")
    } else {
        href.to_owned()
    };
    let parsed = reqwest::Url::parse(&absolute).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let is_ddg = host == "duckduckgo.com" || host.ends_with(".duckduckgo.com");
    if !is_ddg {
        return Some(absolute);
    }
    parsed
        .query_pairs()
        .find(|(key, _)| key == "uddg")
        .map(|(_, value)| value.into_owned())
}

/// Parst die DuckDuckGo-HTML-Ergebnisseite.
///
/// # Description
/// Treffer sind `.result`-Blöcke mit `a.result__a` (Titel, Redirect-Link) und
/// `.result__snippet`; Werbeblöcke (`.result--ad`) werden übersprungen.
fn parse_duckduckgo(html: &str, limit: usize, site: Option<&str>) -> Vec<SearchResult> {
    use scraper::{Html, Selector};

    let (Ok(block_sel), Ok(link_sel), Ok(snippet_sel)) = (
        Selector::parse("div.result, div.web-result"),
        Selector::parse("a.result__a"),
        Selector::parse(".result__snippet"),
    ) else {
        return Vec::new();
    };

    let document = Html::parse_document(html);
    let mut raw = Vec::new();
    for block in document.select(&block_sel) {
        if block
            .value()
            .classes()
            .any(|class| class == "result--ad" || class == "result--ad--small")
        {
            continue;
        }
        let Some(link) = block.select(&link_sel).next() else {
            continue;
        };
        let Some(url) = link.value().attr("href").and_then(decode_ddg_href) else {
            continue;
        };
        let title: String = link.text().collect();
        let snippet: String = block
            .select(&snippet_sel)
            .next()
            .map(|node| node.text().collect())
            .unwrap_or_default();
        raw.push((title, url, snippet));
    }
    collect_results(raw, limit, site, false)
}

// ---------------------------------------------------------------------------
// Anfrageplan
// ---------------------------------------------------------------------------

/// Eine vorbereitete HTTP-Anfrage an ein Backend.
#[derive(Clone, PartialEq, Eq)]
struct RequestPlan {
    /// Ungeprüfte Anfrage-URL (enthält nie den Schlüssel).
    url: String,
    /// `Some(body)` für POST mit JSON, sonst GET.
    json_body: Option<serde_json::Value>,
    /// Header-Name und geheimer Wert für die Authentisierung.
    auth: Option<(&'static str, String)>,
    /// Erwartete Antwortform.
    backend: SearchBackend,
}

impl std::fmt::Debug for RequestPlan {
    /// Schwärzt den Authentisierungswert.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestPlan")
            .field("url", &self.url)
            .field("post", &self.json_body.is_some())
            .field("auth_header", &self.auth.as_ref().map(|(name, _)| *name))
            .field("backend", &self.backend)
            .finish()
    }
}

/// Liefert den nicht-leeren API-Schlüssel oder `NotConfigured`.
fn require_key(config: &WebSearchConfig, what: &'static str) -> WebToolResult<String> {
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .ok_or(WebToolError::NotConfigured { what })
}

/// Endpunkt aus der Konfiguration oder Standard.
fn endpoint_or(config: &WebSearchConfig, default: &str) -> String {
    config
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
        .unwrap_or(default)
        .to_owned()
}

/// Hängt Query-Parameter an eine Basis-URL.
fn with_params(base: &str, params: &[(&str, &str)]) -> WebToolResult<String> {
    let mut url = reqwest::Url::parse(base).map_err(|_| WebToolError::NotConfigured {
        what: "der konfigurierte Such-Endpunkt ist keine gültige URL",
    })?;
    if !params.is_empty() {
        url.query_pairs_mut().extend_pairs(params);
    }
    Ok(url.to_string())
}

/// Baut den Anfrageplan für Backend und Anfrage.
///
/// # Errors
/// - [`WebToolError::NotConfigured`]: Schlüssel/Endpunkt fehlt oder ist ungültig.
fn build_plan(config: &WebSearchConfig, query: &SearchQuery) -> WebToolResult<RequestPlan> {
    let backend = config.provider;
    let text = query.effective_query(backend);
    let count = query.max_results.to_string();
    match backend {
        SearchBackend::Brave => {
            let key = require_key(config, "web.search: Brave benötigt einen API-Schlüssel")?;
            let url = with_params(
                &endpoint_or(config, BRAVE_ENDPOINT),
                &[("q", &text), ("count", &count)],
            )?;
            Ok(RequestPlan {
                url,
                json_body: None,
                auth: Some(("x-subscription-token", key)),
                backend,
            })
        }
        SearchBackend::Tavily => {
            let key = require_key(config, "web.search: Tavily benötigt einen API-Schlüssel")?;
            let mut body = serde_json::json!({
                "query": text,
                "max_results": query.max_results,
                "search_depth": "basic",
            });
            if let (Some(site), Some(object)) = (&query.site, body.as_object_mut()) {
                object.insert("include_domains".to_owned(), serde_json::json!([site]));
            }
            Ok(RequestPlan {
                url: with_params(&endpoint_or(config, TAVILY_ENDPOINT), &[])?,
                json_body: Some(body),
                auth: Some(("authorization", format!("Bearer {key}"))),
                backend,
            })
        }
        SearchBackend::Searxng => {
            let Some(base) = config
                .endpoint
                .as_deref()
                .map(str::trim)
                .filter(|endpoint| !endpoint.is_empty())
            else {
                return Err(WebToolError::NotConfigured {
                    what: "web.search: SearXNG benötigt einen Endpunkt",
                });
            };
            let base = format!("{}/search", base.trim_end_matches('/'));
            Ok(RequestPlan {
                url: with_params(&base, &[("format", "json"), ("q", &text)])?,
                json_body: None,
                auth: None,
                backend,
            })
        }
        SearchBackend::DuckDuckGo => Ok(RequestPlan {
            url: with_params(&endpoint_or(config, DUCKDUCKGO_ENDPOINT), &[("q", &text)])?,
            json_body: None,
            auth: None,
            backend,
        }),
    }
}

/// Parst den Antwortkörper passend zum Backend.
///
/// # Errors
/// - [`WebToolError::Json`]: JSON-Backends mit ungültiger Antwort.
fn parse_response(
    backend: SearchBackend,
    body: &str,
    limit: usize,
    site: Option<&str>,
) -> WebToolResult<Vec<SearchResult>> {
    match backend {
        SearchBackend::Brave => parse_brave(body, limit, site),
        SearchBackend::Tavily => parse_tavily(body, limit, site),
        SearchBackend::Searxng => parse_searxng(body, limit, site),
        SearchBackend::DuckDuckGo => Ok(parse_duckduckgo(body, limit, site)),
    }
}

// ---------------------------------------------------------------------------
// Netz
// ---------------------------------------------------------------------------

/// Liefert den Egress-Client für die Policy des Basis-Fetchers.
///
/// # Description
/// Der Client entsteht ausschließlich über [`harw_egress::build_client`] und
/// wird prozessweit geteilt, solange der Policy-Digest übereinstimmt.
///
/// # Errors
/// - [`WebToolError::NotConfigured`]: der Client ließ sich nicht bauen.
fn search_client(policy: &EgressPolicy) -> WebToolResult<reqwest::Client> {
    let digest = policy.digest();
    if let Some((cached_digest, client)) = SEARCH_CLIENT.get() {
        if *cached_digest == digest {
            return Ok(client.clone());
        }
    }
    let client = harw_egress::build_client(Arc::new(policy.clone())).map_err(|error| {
        tracing::error!(error = %error, "web.search.client_build_failed");
        WebToolError::NotConfigured {
            what: "der Egress-HTTP-Client ließ sich nicht bauen",
        }
    })?;
    // Nur der erste Client wird geteilt; bei abweichender Policy bleibt es beim
    // frisch gebauten Client dieses Aufrufs.
    let _ = SEARCH_CLIENT.set((digest, client.clone()));
    Ok(client)
}

/// Sendet die geprüfte Anfrage und liest den Körper unter dem Byte-Limit.
async fn send_plan(
    client: &reqwest::Client,
    target: &HopTarget,
    plan: &RequestPlan,
    limit: usize,
) -> WebToolResult<String> {
    let mut request = match &plan.json_body {
        Some(body) => client.post(target.url()).json(body),
        None => client.get(target.url()),
    }
    .header(reqwest::header::USER_AGENT, USER_AGENT_VALUE)
    .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS));

    request = match plan.backend {
        SearchBackend::DuckDuckGo => request.header(reqwest::header::ACCEPT, "text/html"),
        _ => request.header(reqwest::header::ACCEPT, "application/json"),
    };

    if let Some((name, secret)) = &plan.auth {
        let mut value = reqwest::header::HeaderValue::from_str(secret).map_err(|_| {
            WebToolError::NotConfigured {
                what: "web.search: der API-Schlüssel enthält unzulässige Zeichen",
            }
        })?;
        value.set_sensitive(true);
        request = request.header(*name, value);
    }

    let mut response = request
        .send()
        .await
        .map_err(|error| map_send_error(error, 0))?;
    let status = response.status();
    if !status.is_success() {
        // Auch 3xx: Weiterleitungen einer Such-API werden nicht gefolgt.
        return Err(WebToolError::UpstreamStatus {
            status: status.as_u16(),
            host: target.host().to_owned(),
        });
    }

    let raw_content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    check_content_type(&raw_content_type, target.host())?;

    if let Some(announced) = response.content_length() {
        if announced > u64::try_from(limit).unwrap_or(u64::MAX) {
            return Err(WebToolError::ResponseTooLarge {
                limit,
                host: target.host().to_owned(),
            });
        }
    }

    let mut buffer: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| map_send_error(error, 0))?
    {
        push_chunk(&mut buffer, &chunk, limit, target.host())?;
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// Führt eine Suche mit einem sandbox-gebundenen Fetcher aus.
///
/// # Description
/// Anfrageplan bauen → URL über [`WebFetcher::check_target`] prüfen → über den
/// Egress-Client senden (Gesamt-Deadline [`TOTAL_DEADLINE_SECS`]) → Antwort auf
/// dem Blocking-Pool parsen und bereinigen.
///
/// # Arguments
/// - `fetcher` (`&WebFetcher`): gebundener Fetcher (Policy, Scope, Limits).
/// - `config` (`&WebSearchConfig`): Backend-Konfiguration.
/// - `query` (`&SearchQuery`): geprüfte Anfrage.
///
/// # Returns
/// Die bereinigte [`SearchResponse`].
///
/// # Errors
/// - [`WebToolError::NotConfigured`]: Schlüssel/Endpunkt fehlt, Client-Bau scheiterte.
/// - Ablehnungen aus [`crate::hop::check_hop`].
/// - [`WebToolError::UpstreamStatus`], [`WebToolError::UpstreamTimeout`],
///   [`WebToolError::Http`], [`WebToolError::ResponseTooLarge`],
///   [`WebToolError::UnexpectedContentType`], [`WebToolError::Json`],
///   [`WebToolError::BlockingTask`].
///
/// # Concurrency
/// Nebenläufig aufrufbar; benötigt Tokio mit `rt` und `time`.
pub async fn search(
    fetcher: &WebFetcher,
    config: &WebSearchConfig,
    query: &SearchQuery,
) -> WebToolResult<SearchResponse> {
    let plan = build_plan(config, query)?;
    let target = fetcher.check_target(&plan.url, 0)?;
    let client = search_client(fetcher.policy())?;
    let limit = fetcher.max_bytes().min(MAX_RESPONSE_BYTES);

    let deadline = Duration::from_secs(TOTAL_DEADLINE_SECS);
    let body = match tokio::time::timeout(deadline, send_plan(&client, &target, &plan, limit)).await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(WebToolError::UpstreamTimeout {
                host: target.host().to_owned(),
                seconds: TOTAL_DEADLINE_SECS,
            });
        }
    };

    let backend = plan.backend;
    let result_limit = usize::from(query.max_results);
    let site = query.site.clone();
    let results = run_blocking("web-search-parse", move || {
        parse_response(backend, &body, result_limit, site.as_deref())
    })
    .await?;

    tracing::info!(
        backend = backend.as_str(),
        host = %target.host(),
        results = results.len(),
        "web.search.done"
    );

    Ok(SearchResponse {
        provider: backend.as_str(),
        query: query.query.clone(),
        results,
    })
}

// ---------------------------------------------------------------------------
// Tool `web.search`
// ---------------------------------------------------------------------------

/// Führt `web.search` aus.
///
/// Der Permission-Prolog stammt aus dem Makro; der Host-Check erfolgt von Hand,
/// weil der Ziel-Host aus der Konfiguration stammt (fail-closed).
#[harw_macros::tool(
    name = "web.search",
    description = "Searches the web and returns a short list of results (title, url, snippet). Use web.fetch to read a result.",
    permission = "network_access",
    parallel_safe
)]
async fn web_search(
    context: &ToolExecutionContext,
    args: SearchArgs,
) -> Result<ToolOutput, ToolsError> {
    let config = search_config();
    let query = match SearchQuery::from_args(&args, config.max_results) {
        Ok(query) => query,
        Err(message) => {
            return Ok(ToolOutput::error(format!(
                "Tool '{}': {message}",
                WebSearchTool::NAME
            )));
        }
    };

    let plan = match build_plan(&config, &query) {
        Ok(plan) => plan,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };
    let Some(host) = harw_tools::host_from_url(&plan.url) else {
        return Ok(ToolOutput::error(format!(
            "Tool '{}': aus dem Such-Endpunkt ließ sich kein Hostname lesen",
            WebSearchTool::NAME
        )));
    };
    if let Some(denied) = harw_tools::require_host_access(context, &host, WebSearchTool::NAME) {
        return Ok(denied);
    }

    let fetcher = match scoped_fetcher(context, None) {
        Ok(fetcher) => fetcher,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };

    let response = match search(&fetcher, &config, &query).await {
        Ok(response) => response,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };

    match serde_json::to_value(&response) {
        Ok(value) => Ok(ToolOutput::json(value)),
        Err(err) => Err(ToolsError::from(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn args(query: &str, max: Option<u64>, site: Option<&str>) -> SearchArgs {
        SearchArgs {
            query: query.to_owned(),
            max_results: max,
            site: site.map(str::to_owned),
        }
    }

    fn query(text: &str, max: u8, site: Option<&str>) -> SearchQuery {
        SearchQuery {
            query: text.to_owned(),
            max_results: max,
            site: site.map(str::to_owned),
        }
    }

    fn config(provider: SearchBackend, endpoint: Option<&str>, key: Option<&str>) -> WebSearchConfig {
        WebSearchConfig {
            provider,
            endpoint: endpoint.map(str::to_owned),
            api_key: key.map(str::to_owned),
            max_results: DEFAULT_MAX_RESULTS,
        }
    }

    const BRAVE_FIXTURE: &str = r#"{
        "type": "search",
        "web": {
            "type": "search",
            "results": [
                {"title": "serde - Rust", "url": "https://docs.rs/serde/latest/serde/",
                 "description": "A <strong>generic</strong> serialization &amp; deserialization framework"},
                {"title": "Evil", "url": "javascript:alert(1)", "description": "x"},
                {"title": "Creds", "url": "https://user:pw@example.com/", "description": "x"},
                {"title": "serde duplicate", "url": "https://docs.rs/serde/latest/serde/", "description": "dup"},
                {"title": "serde on crates.io", "url": "https://crates.io/crates/serde", "description": "Crate page"}
            ]
        }
    }"#;

    const TAVILY_FIXTURE: &str = r#"{
        "query": "tokio",
        "results": [
            {"title": "Tokio", "url": "https://tokio.rs/", "content": "An asynchronous runtime\u0007 for\n\nRust", "score": 0.9},
            {"title": "tokio docs", "url": "https://docs.rs/tokio", "content": "API docs"}
        ]
    }"#;

    const SEARXNG_FIXTURE: &str = r#"{
        "query": "reqwest",
        "number_of_results": 0,
        "results": [
            {"url": "https://docs.rs/reqwest", "title": "reqwest - Rust", "content": "higher level HTTP client", "engine": "duckduckgo"},
            {"url": "ftp://files.example.org/x", "title": "ftp", "content": "no"},
            {"url": "https://github.com/seanmonstar/reqwest", "title": "", "content": "repo"}
        ]
    }"#;

    const DDG_FIXTURE: &str = r##"<!DOCTYPE html><html><body>
      <div class="results">
        <div class="result results_links results_links_deep result--ad">
          <div class="links_main result__body">
            <h2 class="result__title"><a rel="nofollow" class="result__a" href="https://duckduckgo.com/y.js?ad_domain=ads.example&amp;u3=x">Sponsored</a></h2>
            <a class="result__snippet" href="#">Buy now</a>
          </div>
        </div>
        <div class="result results_links results_links_deep web-result">
          <div class="links_main links_deep result__body">
            <h2 class="result__title">
              <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Fscraper%2Flatest%2Fscraper%2F&amp;rut=abc">scraper - <b>Rust</b></a>
            </h2>
            <a class="result__snippet" href="//duckduckgo.com/l/?uddg=x">HTML parsing and querying with <b>CSS</b> selectors.</a>
          </div>
        </div>
        <div class="result results_links results_links_deep web-result">
          <div class="links_main links_deep result__body">
            <h2 class="result__title"><a rel="nofollow" class="result__a" href="https://github.com/causal-agent/scraper">causal-agent/scraper</a></h2>
            <div class="result__snippet">Repository &lt;script&gt; text</div>
          </div>
        </div>
        <div class="result results_links web-result">
          <h2 class="result__title"><a class="result__a" href="//duckduckgo.com/l/?uddg=data%3Atext%2Fhtml%2Cx">Bad scheme</a></h2>
        </div>
      </div>
    </body></html>"##;

    // --- Parser -------------------------------------------------------------

    /// Brave: HTML wird entfernt, Entities dekodiert, unsichere/doppelte URLs verworfen.
    #[test]
    fn test_parse_brave_sanitizes_and_dedups() -> TestResult {
        let results = parse_brave(BRAVE_FIXTURE, 10, None).map_err(ctx("brave"))?;
        assert_eq!(results.len(), 2);
        let first = results.first().ok_or(TestError::Missing("erster Treffer"))?;
        assert_eq!(first.title, "serde - Rust");
        assert_eq!(first.url, "https://docs.rs/serde/latest/serde/");
        assert_eq!(
            first.snippet,
            "A generic serialization & deserialization framework"
        );
        let second = results.get(1).ok_or(TestError::Missing("zweiter Treffer"))?;
        assert_eq!(second.url, "https://crates.io/crates/serde");
        Ok(())
    }

    /// Brave: `site`-Filter und Limit greifen; fehlendes `web` ergibt keine Treffer.
    #[test]
    fn test_parse_brave_site_filter_limit_and_empty() -> TestResult {
        let results = parse_brave(BRAVE_FIXTURE, 10, Some("crates.io")).map_err(ctx("site"))?;
        assert_eq!(results.len(), 1);
        let limited = parse_brave(BRAVE_FIXTURE, 1, None).map_err(ctx("limit"))?;
        assert_eq!(limited.len(), 1);
        let empty = parse_brave(r#"{"type":"search"}"#, 10, None).map_err(ctx("leer"))?;
        assert!(empty.is_empty());
        assert!(matches!(
            parse_brave("not json", 10, None),
            Err(WebToolError::Json(_))
        ));
        Ok(())
    }

    /// Tavily: Steuerzeichen und Zeilenumbrüche werden zu einfachem Leerraum.
    #[test]
    fn test_parse_tavily_collapses_whitespace() -> TestResult {
        let results = parse_tavily(TAVILY_FIXTURE, 10, None).map_err(ctx("tavily"))?;
        assert_eq!(results.len(), 2);
        let first = results.first().ok_or(TestError::Missing("Treffer"))?;
        assert_eq!(first.snippet, "An asynchronous runtime for Rust");
        assert_eq!(first.url, "https://tokio.rs/");
        Ok(())
    }

    /// SearXNG: Nicht-http(s) wird verworfen, leerer Titel fällt auf die URL zurück.
    #[test]
    fn test_parse_searxng_filters_schemes_and_fills_title() -> TestResult {
        let results = parse_searxng(SEARXNG_FIXTURE, 10, None).map_err(ctx("searxng"))?;
        let urls: Vec<&str> = results.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(
            urls,
            vec!["https://docs.rs/reqwest", "https://github.com/seanmonstar/reqwest"]
        );
        let last = results.get(1).ok_or(TestError::Missing("Treffer"))?;
        assert_eq!(last.title, last.url);
        Ok(())
    }

    /// DuckDuckGo: Werbung übersprungen, `uddg` dekodiert, HTML entfernt.
    #[test]
    fn test_parse_duckduckgo_decodes_redirects_and_skips_ads() -> TestResult {
        let results = parse_duckduckgo(DDG_FIXTURE, 10, None);
        let urls: Vec<&str> = results.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://docs.rs/scraper/latest/scraper/",
                "https://github.com/causal-agent/scraper"
            ]
        );
        let first = results.first().ok_or(TestError::Missing("Treffer"))?;
        assert_eq!(first.title, "scraper - Rust");
        assert_eq!(first.snippet, "HTML parsing and querying with CSS selectors.");
        let second = results.get(1).ok_or(TestError::Missing("Treffer"))?;
        assert_eq!(second.snippet, "Repository <script> text");
        Ok(())
    }

    /// DuckDuckGo: `site` filtert Subdomains korrekt, fremde Hosts nicht.
    #[test]
    fn test_parse_duckduckgo_site_filter() {
        let results = parse_duckduckgo(DDG_FIXTURE, 10, Some("docs.rs"));
        assert_eq!(results.len(), 1);
        assert!(parse_duckduckgo("<html></html>", 10, None).is_empty());
    }

    /// Redirect-Dekodierung: relative, protokollrelative und direkte Links.
    #[test]
    fn test_decode_ddg_href_variants() {
        assert_eq!(
            decode_ddg_href("/l/?uddg=https%3A%2F%2Fa.example%2Fp%3Fx%3D1").as_deref(),
            Some("https://a.example/p?x=1")
        );
        assert_eq!(
            decode_ddg_href("https://b.example/").as_deref(),
            Some("https://b.example/")
        );
        assert_eq!(decode_ddg_href("https://duckduckgo.com/y.js?ad=1"), None);
    }

    // --- Bereinigung --------------------------------------------------------

    /// Kappung auf Zeichen (nicht Bytes) mit Auslassungszeichen; Bidi-Zeichen entfernt.
    #[test]
    fn test_clean_text_truncates_and_strips_format_chars() {
        let long = "ä".repeat(MAX_SNIPPET_CHARS + 50);
        let cleaned = clean_text(&long, MAX_SNIPPET_CHARS, false);
        assert_eq!(cleaned.chars().count(), MAX_SNIPPET_CHARS);
        assert!(cleaned.ends_with('…'));
        assert_eq!(clean_text("a\u{202E}b\u{200B}c", 10, false), "abc");
        assert_eq!(clean_text("  x \t y ", 10, false), "x y");
        assert_eq!(clean_text("<b>a</b> &amp; b", 10, true), "a & b");
        assert_eq!(clean_text("a <b> c", 10, false), "a <b> c");
    }

    // --- Argumente ----------------------------------------------------------

    /// Anfrage-Grenzen, Trefferzahl-Deckelung und `site`-Validierung.
    #[test]
    fn test_search_query_validation() -> TestResult {
        assert!(SearchQuery::from_args(&args("   ", None, None), 8).is_err());
        let too_long = "x".repeat(MAX_QUERY_CHARS + 1);
        assert!(SearchQuery::from_args(&args(&too_long, None, None), 8).is_err());
        let exact = "ü".repeat(MAX_QUERY_CHARS);
        assert!(SearchQuery::from_args(&args(&exact, None, None), 8).is_ok());

        let q = SearchQuery::from_args(&args(" serde\n ", None, None), 8)
            .map_err(TestError::Unexpected)?;
        assert_eq!((q.query.as_str(), q.max_results), ("serde", 8));
        let q = SearchQuery::from_args(&args("serde", Some(500), None), 8)
            .map_err(TestError::Unexpected)?;
        assert_eq!(q.max_results, HARD_MAX_RESULTS);
        let q = SearchQuery::from_args(&args("serde", Some(0), None), 8)
            .map_err(TestError::Unexpected)?;
        assert_eq!(q.max_results, 1);

        let q = SearchQuery::from_args(&args("serde", None, Some(" Docs.RS ")), 8)
            .map_err(TestError::Unexpected)?;
        assert_eq!(q.site.as_deref(), Some("docs.rs"));
        for bad in ["https://docs.rs", "docs rs", "localhost", "-a.example", "a..b"] {
            assert!(
                SearchQuery::from_args(&args("serde", None, Some(bad)), 8).is_err(),
                "{bad} muss abgelehnt werden"
            );
        }
        Ok(())
    }

    /// Argumente werden nachsichtig deserialisiert (Zahl als String).
    #[test]
    fn test_search_args_deserialize_lenient() -> TestResult {
        let parsed: SearchArgs =
            serde_json::from_str(r#"{"query":"x","max_results":"5"}"#).map_err(ctx("json"))?;
        assert_eq!(parsed.max_results, Some(5));
        assert_eq!(parsed.site, None);
        Ok(())
    }

    // --- Anfrageplan und Konfiguration --------------------------------------

    /// Brave: Schlüssel im Header, nie in der URL; ohne Schlüssel NotConfigured.
    #[test]
    fn test_build_plan_brave() -> TestResult {
        let plan = build_plan(
            &config(SearchBackend::Brave, None, Some("SECRET")),
            &query("rust async", 5, Some("docs.rs")),
        )
        .map_err(ctx("plan"))?;
        assert!(plan.url.starts_with(BRAVE_ENDPOINT));
        assert!(plan.url.contains("q=rust+async+site%3Adocs.rs"));
        assert!(plan.url.contains("count=5"));
        assert!(!plan.url.contains("SECRET"));
        assert!(!format!("{plan:?}").contains("SECRET"));
        assert!(matches!(
            build_plan(&config(SearchBackend::Brave, None, Some("  ")), &query("x", 5, None)),
            Err(WebToolError::NotConfigured { .. })
        ));
        Ok(())
    }

    /// Tavily: POST-JSON mit `include_domains`, Bearer-Header.
    #[test]
    fn test_build_plan_tavily() -> TestResult {
        let plan = build_plan(
            &config(SearchBackend::Tavily, None, Some("tvly-k")),
            &query("tokio", 3, Some("tokio.rs")),
        )
        .map_err(ctx("plan"))?;
        assert_eq!(plan.url, TAVILY_ENDPOINT);
        let body = plan.json_body.ok_or(TestError::Missing("JSON-Körper"))?;
        assert_eq!(body["query"], "tokio");
        assert_eq!(body["max_results"], 3);
        assert_eq!(body["include_domains"][0], "tokio.rs");
        assert_eq!(
            plan.auth,
            Some(("authorization", "Bearer tvly-k".to_owned()))
        );
        Ok(())
    }

    /// SearXNG braucht einen Endpunkt; DuckDuckGo ist der schlüssellose Standard.
    #[test]
    fn test_build_plan_searxng_and_duckduckgo() -> TestResult {
        assert!(matches!(
            build_plan(&config(SearchBackend::Searxng, None, None), &query("x", 5, None)),
            Err(WebToolError::NotConfigured { .. })
        ));
        let plan = build_plan(
            &config(SearchBackend::Searxng, Some("https://searx.example.org/"), None),
            &query("a&b", 5, None),
        )
        .map_err(ctx("searxng"))?;
        assert_eq!(
            plan.url,
            "https://searx.example.org/search?format=json&q=a%26b"
        );

        let plan = build_plan(&WebSearchConfig::default(), &query("x y", 5, None))
            .map_err(ctx("ddg"))?;
        assert_eq!(plan.url, "https://html.duckduckgo.com/html/?q=x+y");
        assert!(plan.auth.is_none());
        Ok(())
    }

    /// `Debug` der Konfiguration schwärzt den Schlüssel.
    #[test]
    fn test_config_debug_redacts_key() {
        let rendered = format!(
            "{:?}",
            config(SearchBackend::Brave, None, Some("very-secret-key"))
        );
        assert!(!rendered.contains("very-secret-key"));
        assert!(rendered.contains("<redacted>"));
        let default = WebSearchConfig::default();
        assert_eq!(default.provider, SearchBackend::DuckDuckGo);
        assert_eq!(default.max_results, 8);
    }

    /// Die geprüfte Plan-URL besteht die Hop-Prüfung nur bei freigegebenem Host.
    #[test]
    fn test_plan_url_passes_hop_check_only_when_allowed() -> TestResult {
        use harw_authority::NetworkScope;
        let plan = build_plan(&WebSearchConfig::default(), &query("x", 5, None))
            .map_err(ctx("plan"))?;
        let policy = EgressPolicy::new(vec!["html.duckduckgo.com".to_owned()], false)
            .map_err(ctx("policy"))?;
        let allowed = NetworkScope::from_hosts(["html.duckduckgo.com".to_owned()]);
        let target = crate::hop::check_hop(&policy, &allowed, false, &plan.url, 0)
            .map_err(ctx("erlaubt"))?;
        assert_eq!(target.host(), "html.duckduckgo.com");
        let other = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        assert!(crate::hop::check_hop(&policy, &other, false, &plan.url, 0).is_err());
        Ok(())
    }

    /// Tool-Metadaten: Name und Berechtigung.
    #[test]
    fn test_tool_name() {
        assert_eq!(WebSearchTool::NAME, "web.search");
    }
}
