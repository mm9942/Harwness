//! `web.fetch` — Abruf über den Egress-Client mit Cache, Limits und Redirect-Kontrolle.
//!
//! # Verantwortung
//! Dieses Modul besitzt die Netz-Ausgangstür des Crates: [`WebFetcher`] hält
//! den HTTP-Client aus [`harw_egress::build_client`], führt die Redirect-Kette
//! von Hand, erzwingt Content-Type- und Byte-Grenzen, verwaltet den
//! Antwort-Cache über [`crate::cache`] und bereitet Körper über
//! [`crate::html`] auf. [`crate::docs_rs`] und [`crate::crates_io`] bauen nur
//! URLs und formatieren Ergebnisse.
//!
//! # Schlüsseltypen
//! - [`WebFetcher`] — Abruf-Motor (Client, Policy, Cache, Limits, Scope).
//! - [`WebFetchOptions`] — TTL, Byte-Limits, `http`-Freigabe.
//! - [`FetchedDocument`] — Ergebnis eines Abrufs.
//! - [`OutputFormat`] — `text` / `markdown` / `raw`.
//! - [`FetchArgs`] / `WebFetchTool` — Argumente und `ToolExecutor` von `web.fetch`.
//!
//! # Sicherheitskontrakt
//! 1. **Nur der Egress-Client.** Der `reqwest::Client` entsteht ausschließlich
//!    über [`harw_egress::build_client`] (Scoped-Resolver: Allowlist und
//!    Adressklassen je DNS-Antwort, kein Proxy, keine automatischen
//!    Redirects). Die [`EgressPolicy`] ist Konstruktor-Parameter; es gibt
//!    **keine** Voreinstellung — ohne [`install_fetcher`]/[`crate::configure`]
//!    scheitert jeder Aufruf mit [`WebToolError::NotConfigured`].
//! 2. **Jeder Hop wird vor dem Senden geprüft.** Start-URL und jedes
//!    Redirect-Ziel laufen durch [`crate::hop::check_hop`] (Schema,
//!    `EgressPolicy::check_url`, Sandbox-Scope). IP-Literale umgehen den
//!    Resolver; diese Prüfung ist für sie die einzige. Höchstens
//!    [`MAX_REDIRECTS`] Weiterleitungen.
//! 3. **Cache ist isoliert und wird neu geprüft.** Schlüssel =
//!    BLAKE3(Policy-Digest ‖ Mandant/Workspace/Netz-Scope ‖ Anfrage-URL). Ein
//!    Treffer wird nur verwendet, wenn **jede** URL seiner gespeicherten Kette
//!    (inklusive Endziel) erneut die Hop-Prüfung besteht — auch beim
//!    Fail-open nach Transportfehlern.
//! 4. **Kappung.** Bytes beim Lesen ([`push_chunk`], vor dem Dekodieren),
//!    Text nach der Aufbereitung ([`crate::html::truncate_utf8`],
//!    UTF-8-grenzsicher).
//! 5. **Content-Type-Positivliste.** Nur Text-, JSON- und XML-artige Typen.
//! 6. **Blockierendes außerhalb des Reaktors.** HTML-Parsing, Markdown und
//!    Cache-I/O laufen über [`run_blocking`] (`spawn_blocking`).
//! 7. **Meldungen ohne interne Adressen** (siehe [`crate::hop`]).
//!
//! # Nebenläufigkeit
//! [`WebFetcher`] ist `Send + Sync`; die Policy liegt in einem `Arc`,
//! `reqwest::Client` ist ein refcounted Handle. [`WebFetcher::scoped`] teilt
//! Client und Policy. Der prozessweite Basis-Fetcher liegt in einem
//! `OnceLock<Arc<WebFetcher>>`.
//!
//! # Fehler
//! Alle Varianten aus [`WebToolError`]. Nur Transportfehler fallen auf einen
//! (erneut geprüften) veralteten Cache-Eintrag zurück.
//!
//! # Examples
//! ```rust,no_run
//! use std::{path::PathBuf, sync::Arc};
//! use harw_egress::EgressPolicy;
//! use harw_authority::NetworkScope;
//! use harw_tool_web::cache::CacheScope;
//! use harw_tool_web::fetch::{OutputFormat, WebFetchOptions, WebFetcher};
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let policy = Arc::new(EgressPolicy::new(vec!["docs.rs".to_owned()], false)?);
//! let base = WebFetcher::new(policy, PathBuf::from("/home/u/.harw/cache"), WebFetchOptions::default())?;
//! let network = NetworkScope::from_hosts(["docs.rs".to_owned()]);
//! let scoped = base.scoped(network.clone(), CacheScope::new("t", "w", &network), None);
//! let doc = scoped.fetch("https://docs.rs/serde/latest/serde/", OutputFormat::Markdown).await?;
//! println!("{}", doc.body);
//! # Ok(())
//! # }
//! ```

use crate::cache::{
    CACHE_FORMAT_VERSION, CacheEntry, CacheScope, cache_key, cache_path, entry_is_fresh,
    entry_matches, now_secs, read_cache_entry, to_hex, write_cache_entry,
};
use crate::error::{WebToolError, WebToolResult};
use crate::hop::{HopTarget, check_hop, map_send_error, resolve_location};
use crate::html::{html_to_markdown, html_to_text, truncate_utf8};
use harw_egress::EgressPolicy;
use harw_macros::Tool;
use harw_authority::NetworkScope;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Standard-Byte-Obergrenze einer Antwort (1 MiB).
pub const DEFAULT_MAX_BYTES: usize = 1_048_576;

/// Harte Byte-Obergrenze einer Antwort (8 MiB), unabhängig von Argumenten.
pub const HARD_MAX_BYTES: usize = 8 * 1_048_576;

/// Standard-Obergrenze des aufbereiteten Texts an das Modell (64 KiB).
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Harte Obergrenze des aufbereiteten Texts (1 MiB).
pub const HARD_MAX_OUTPUT_BYTES: usize = 1_048_576;

/// Standard-Cache-TTL in Sekunden.
pub const DEFAULT_TTL_SECS: u64 = 3_600;

/// Timeout einer einzelnen HTTP-Anfrage.
pub const REQUEST_TIMEOUT_SECS: u64 = 20;

/// Gesamt-Deadline über alle Redirect-Hops eines Abrufs.
pub const TOTAL_DEADLINE_SECS: u64 = 45;

/// Maximale Anzahl gefolgter Weiterleitungen (jeder Hop wird erneut geprüft).
pub const MAX_REDIRECTS: usize = 5;

/// User-Agent aller ausgehenden Anfragen (crates.io verlangt einen).
const USER_AGENT_VALUE: &str = concat!("harwness-web-tools/", env!("CARGO_PKG_VERSION"));

/// Medientypen zusätzlich zu `text/*` und den `+json`/`+xml`-Suffixen.
const ALLOWED_MEDIA_TYPES: &[&str] = &[
    "application/json",
    "application/xml",
    "application/xhtml+xml",
    "application/javascript",
];

/// Der prozessweit geteilte Basis-Fetcher (ohne Sandbox-Scope).
static SHARED_FETCHER: OnceLock<Arc<WebFetcher>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Argumente, Ausgabeform, Optionen
// ---------------------------------------------------------------------------

/// Argumente des Tools `web.fetch`.
///
/// # Description
/// Vom Modell geliefert und damit **untrusted**. Prüfungen laufen im
/// Makro-Prolog (Permission, Host) und in [`WebFetcher`].
#[derive(Debug, Clone, Tool, Deserialize)]
#[tool(
    name = "web.fetch",
    description = "Lädt eine HTTPS-Ressource und liefert Text oder Markdown."
)]
pub struct FetchArgs {
    /// Vollständige HTTPS-URL.
    pub url: String,
    /// Ausgabeform: "text" (Standard), "markdown" oder "raw".
    #[tool(default = "text")]
    #[serde(default)]
    pub format: Option<String>,
    /// Byte-Obergrenze der Antwort (kann das konfigurierte Limit nur senken).
    #[tool(default = 1048576)]
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_bytes: Option<usize>,
}

/// Ausgabeform eines abgerufenen Dokuments.
///
/// # Description
/// `Raw` liefert den Körper unverändert, `Text` den sichtbaren Text, `Markdown`
/// konvertiert HTML. Für Nicht-HTML sind alle drei identisch mit `Raw`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Sichtbarer Text (Standard).
    #[default]
    Text,
    /// HTML nach Markdown konvertiert.
    Markdown,
    /// Unveränderter Antwort-Körper.
    Raw,
}

impl OutputFormat {
    /// Übersetzt den Argument-String in eine Ausgabeform.
    ///
    /// # Arguments
    /// - `value` (`Option<&str>`): Feld `format`; `None`/leer bedeutet `Text`.
    ///
    /// # Returns
    /// `Some(OutputFormat)` für bekannte Werte, sonst `None`.
    ///
    /// # Concurrency
    /// Rein.
    ///
    /// # Examples
    /// ```rust
    /// use harw_tool_web::fetch::OutputFormat;
    ///
    /// assert_eq!(OutputFormat::parse(None), Some(OutputFormat::Text));
    /// assert_eq!(OutputFormat::parse(Some("MarkDown")), Some(OutputFormat::Markdown));
    /// assert_eq!(OutputFormat::parse(Some("pdf")), None);
    /// ```
    #[must_use]
    pub fn parse(value: Option<&str>) -> Option<Self> {
        let normalized = value.map(str::trim).unwrap_or("text").to_ascii_lowercase();
        match normalized.as_str() {
            "" | "text" => Some(Self::Text),
            "markdown" | "md" => Some(Self::Markdown),
            "raw" => Some(Self::Raw),
            _ => None,
        }
    }
}

/// Limits und Schalter eines [`WebFetcher`].
///
/// # Description
/// Enthält **keine** Ziel-Freigaben — die kommen ausschließlich aus der
/// [`EgressPolicy`] und dem Sandbox-Scope. `Default` ist deshalb unkritisch:
/// TTL [`DEFAULT_TTL_SECS`], [`DEFAULT_MAX_BYTES`],
/// [`DEFAULT_MAX_OUTPUT_BYTES`], `allow_http = false`.
///
/// # Concurrency
/// Reine Daten; `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebFetchOptions {
    /// Lebensdauer eines Cache-Eintrags vor dem nächsten Conditional-GET.
    pub ttl: Duration,
    /// Byte-Obergrenze einer Antwort (auf [`HARD_MAX_BYTES`] gedeckelt).
    pub max_bytes: usize,
    /// Obergrenze des aufbereiteten Texts (auf [`HARD_MAX_OUTPUT_BYTES`] gedeckelt).
    pub max_output_bytes: usize,
    /// `http://` zusätzlich zu `https://` erlauben. Nur für ausdrücklich
    /// freigegebene Umgebungen (z. B. lokale Spiegel); Standard `false`.
    pub allow_http: bool,
}

impl Default for WebFetchOptions {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(DEFAULT_TTL_SECS),
            max_bytes: DEFAULT_MAX_BYTES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            allow_http: false,
        }
    }
}

impl WebFetchOptions {
    /// Deckelt die Limits auf die harten Obergrenzen (mindestens 1 Byte).
    fn clamped(self) -> Self {
        Self {
            max_bytes: self.max_bytes.clamp(1, HARD_MAX_BYTES),
            max_output_bytes: self.max_output_bytes.clamp(1, HARD_MAX_OUTPUT_BYTES),
            ..self
        }
    }
}

// ---------------------------------------------------------------------------
// Ergebnis-Typ
// ---------------------------------------------------------------------------

/// Ein abgerufenes (oder aus dem Cache bedientes) Dokument.
///
/// # Description
/// Aus [`WebFetcher::fetch`] ist `body` aufbereitet und gekappt; aus
/// [`WebFetcher::fetch_source`] ist `body` der Rohkörper (nur byte-gekappt).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FetchedDocument {
    /// Das Endziel (nach allen Weiterleitungen, normalisiert).
    pub url: String,
    /// HTTP-Status (`304` nach erfolgreichem Conditional-GET).
    pub status: u16,
    /// Medientyp ohne Parameter, z. B. `text/html`.
    pub content_type: String,
    /// Der Körper.
    pub body: String,
    /// Ob der Körper aus dem Cache stammt.
    pub from_cache: bool,
    /// `ETag` der Antwort, falls vorhanden.
    pub etag: Option<String>,
    /// Ob `body` nach der Aufbereitung gekappt wurde.
    pub truncated: bool,
}

/// Rohergebnis eines Netzabrufs, bevor Cache und Formatierung greifen.
#[derive(Debug)]
enum RawOutcome {
    /// Der Server meldete `304 Not Modified` auf den Conditional-GET.
    NotModified,
    /// Der Server lieferte einen Körper.
    Body {
        status: u16,
        chain: Vec<String>,
        content_type: String,
        etag: Option<String>,
        bytes: Vec<u8>,
    },
}

// ---------------------------------------------------------------------------
// Reine Prüf- und Hilfsfunktionen
// ---------------------------------------------------------------------------

/// Prüft den `Content-Type` einer Antwort gegen die Positivliste.
///
/// # Description
/// Nur der Medientyp vor `;`, ASCII-kleingeschrieben. Angenommen: `text/*`,
/// die interne Liste, `+json`, `+xml`. Fehlend oder leer ist Ablehnung.
///
/// # Arguments
/// - `raw` (`&str`): roher Header-Wert.
/// - `host` (`&str`): antwortender Host (nur Meldung).
///
/// # Returns
/// Den normalisierten Medientyp.
///
/// # Errors
/// - [`WebToolError::UnexpectedContentType`]: Typ nicht auf der Liste.
///
/// # Concurrency
/// Rein.
///
/// # Examples
/// ```rust
/// use harw_tool_web::fetch::check_content_type;
///
/// assert_eq!(check_content_type("text/html; charset=utf-8", "docs.rs").unwrap(), "text/html");
/// assert!(check_content_type("application/octet-stream", "docs.rs").is_err());
/// ```
pub fn check_content_type(raw: &str, host: &str) -> WebToolResult<String> {
    let media = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    let accepted = !media.is_empty()
        && (media.starts_with("text/")
            || media.ends_with("+json")
            || media.ends_with("+xml")
            || ALLOWED_MEDIA_TYPES.contains(&media.as_str()));

    if accepted {
        Ok(media)
    } else {
        Err(WebToolError::UnexpectedContentType {
            content_type: media,
            host: host.to_owned(),
        })
    }
}

/// Hängt einen Chunk an den Puffer an, sofern das Limit das zulässt.
///
/// # Description
/// Setzt das Byte-Limit **beim Lesen** durch (vor jedem Dekodieren); die
/// Addition ist sättigend.
///
/// # Arguments
/// - `buffer` (`&mut Vec<u8>`): Akkumulator.
/// - `chunk` (`&[u8]`): neu gelesener Abschnitt.
/// - `limit` (`usize`): Obergrenze für den gesamten Puffer.
/// - `host` (`&str`): antwortender Host (nur Meldung).
///
/// # Returns
/// `Ok(())`, wenn der Chunk vollständig übernommen wurde.
///
/// # Errors
/// - [`WebToolError::ResponseTooLarge`]: Limit überschritten; Puffer unverändert.
///
/// # Concurrency
/// Rein auf dem übergebenen Puffer.
///
/// # Examples
/// ```rust
/// use harw_tool_web::fetch::push_chunk;
///
/// let mut buffer = Vec::new();
/// assert!(push_chunk(&mut buffer, b"abc", 4, "docs.rs").is_ok());
/// assert!(push_chunk(&mut buffer, b"de", 4, "docs.rs").is_err());
/// assert_eq!(buffer, b"abc");
/// ```
pub fn push_chunk(
    buffer: &mut Vec<u8>,
    chunk: &[u8],
    limit: usize,
    host: &str,
) -> WebToolResult<()> {
    if buffer.len().saturating_add(chunk.len()) > limit {
        return Err(WebToolError::ResponseTooLarge {
            limit,
            host: host.to_owned(),
        });
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

/// Sagt, ob ein Medientyp HTML enthält.
#[must_use]
fn is_html(content_type: &str) -> bool {
    content_type == "text/html" || content_type == "application/xhtml+xml"
}

/// Bereitet einen Rohkörper gemäß Ausgabeform auf.
///
/// # Description
/// HTML wird für `Text`/`Markdown` bereinigt (script/style/noscript/template
/// und versteckte Elemente entfernt, siehe [`crate::html`]); Nicht-HTML und
/// `Raw` bleiben unverändert.
///
/// # Arguments
/// - `raw` (`&str`): Antwort-Körper.
/// - `content_type` (`&str`): normalisierter Medientyp.
/// - `format` ([`OutputFormat`]): gewünschte Ausgabeform.
///
/// # Returns
/// Den aufbereiteten Körper.
///
/// # Errors
/// - [`WebToolError::Io`]: Markdown-Konvertierung schlug fehl.
///
/// # Concurrency
/// Rein, aber blockierend; im async-Kontext über [`run_blocking`].
///
/// # Examples
/// ```rust
/// use harw_tool_web::fetch::{render, OutputFormat};
///
/// assert_eq!(render("<p>x</p>", "text/html", OutputFormat::Raw).unwrap(), "<p>x</p>");
/// ```
pub fn render(raw: &str, content_type: &str, format: OutputFormat) -> WebToolResult<String> {
    match format {
        OutputFormat::Raw => Ok(raw.to_owned()),
        OutputFormat::Text if is_html(content_type) => Ok(html_to_text(raw)),
        OutputFormat::Text => Ok(raw.to_owned()),
        OutputFormat::Markdown if is_html(content_type) => html_to_markdown(raw),
        OutputFormat::Markdown => Ok(raw.to_owned()),
    }
}

/// Bereitet ein Rohdokument auf und kappt den Text UTF-8-grenzsicher.
///
/// # Arguments
/// - `document` ([`FetchedDocument`]): Rohdokument aus
///   [`WebFetcher::fetch_source`] (Eigentum geht über).
/// - `format` ([`OutputFormat`]): Ausgabeform.
/// - `max_output_bytes` (`usize`): Obergrenze des Texts in Bytes.
///
/// # Returns
/// Das Dokument mit aufbereitetem, gekapptem `body` und gesetztem `truncated`.
///
/// # Errors
/// - [`WebToolError::Io`]: Markdown-Konvertierung schlug fehl.
///
/// # Concurrency
/// Blockierend; über [`run_blocking`] aufrufen.
///
/// # Examples
/// ```rust
/// use harw_tool_web::fetch::{finish_document, FetchedDocument, OutputFormat};
///
/// let raw = FetchedDocument {
///     url: "https://docs.rs/".into(), status: 200, content_type: "text/plain".into(),
///     body: "äöü".into(), from_cache: false, etag: None, truncated: false,
/// };
/// let done = finish_document(raw, OutputFormat::Text, 3).unwrap();
/// assert_eq!((done.body.as_str(), done.truncated), ("ä", true));
/// ```
pub fn finish_document(
    mut document: FetchedDocument,
    format: OutputFormat,
    max_output_bytes: usize,
) -> WebToolResult<FetchedDocument> {
    let rendered = render(&document.body, &document.content_type, format)?;
    let (kept, truncated) = truncate_utf8(&rendered, max_output_bytes);
    document.body = kept.to_owned();
    document.truncated = truncated;
    Ok(document)
}

/// Führt blockierende Arbeit auf dem Blocking-Pool von Tokio aus.
///
/// # Description
/// Wrapper um `tokio::task::spawn_blocking` für HTML-Parsing, Markdown,
/// JSON-Verdichtung und Cache-I/O, damit der async-Reaktor nicht blockiert.
///
/// # Arguments
/// - `task` (`&'static str`): Name der Aufgabe für Fehlermeldungen.
/// - `job` (`F`): die Arbeit; muss `Send + 'static` sein.
///
/// # Returns
/// Das Ergebnis von `job`.
///
/// # Errors
/// - Fehler von `job`.
/// - [`WebToolError::BlockingTask`]: die Aufgabe geriet in Panik oder wurde
///   abgebrochen.
///
/// # Panics
/// Außerhalb eines Tokio-Runtimes (wie `spawn_blocking`).
///
/// # Concurrency
/// Belegt einen Thread des Blocking-Pools für die Dauer von `job`.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_web::fetch::run_blocking;
///
/// # async fn demo() -> Result<(), harw_tool_web::WebToolError> {
/// let n = run_blocking("zaehlen", || Ok(21 * 2)).await?;
/// assert_eq!(n, 42);
/// # Ok(())
/// # }
/// ```
pub async fn run_blocking<T, F>(task: &'static str, job: F) -> WebToolResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> WebToolResult<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(job).await {
        Ok(result) => result,
        Err(_) => Err(WebToolError::BlockingTask { task }),
    }
}

/// Baut aus einem Cache-Eintrag das Rohdokument.
fn document_from_entry(entry: &CacheEntry, status: u16, from_cache: bool) -> FetchedDocument {
    FetchedDocument {
        url: entry.final_url().to_owned(),
        status,
        content_type: entry.content_type.clone(),
        body: entry.body.clone(),
        from_cache,
        etag: entry.etag.clone(),
        truncated: false,
    }
}

// ---------------------------------------------------------------------------
// WebFetcher
// ---------------------------------------------------------------------------

/// Der Abruf-Motor: Egress-Client, Policy, Cache, Limits und Sandbox-Scope.
///
/// # Description
/// Ein über [`Self::new`] gebauter Fetcher trägt einen **leeren**
/// Sandbox-Scope und erreicht nichts. Pro Tool-Aufruf leitet
/// [`Self::scoped`] eine Kopie mit dem [`NetworkScope`] und [`CacheScope`] des
/// Aufrufs ab. Ein Ziel muss sowohl die [`EgressPolicy`] als auch den
/// Sandbox-Scope bestehen.
///
/// # Concurrency
/// `Send + Sync`; Client und Policy werden zwischen Kopien geteilt.
pub struct WebFetcher {
    /// HTTP-Client aus `harw_egress::build_client` (keine Redirects, kein Proxy).
    client: reqwest::Client,
    /// Prozessweite Egress-Policy.
    policy: Arc<EgressPolicy>,
    /// `policy.digest()`, einmal berechnet.
    policy_digest: [u8; 32],
    /// Basis-Cache-Verzeichnis (vorgesehen: `harw_home::paths::cache_dir`).
    cache_dir: PathBuf,
    /// Limits und Schalter.
    options: WebFetchOptions,
    /// Sandbox-Scope des Aufrufs; leer bedeutet: nichts erreichbar.
    network: NetworkScope,
    /// Cache-Isolationsbereich des Aufrufs.
    cache_scope: CacheScope,
}

impl std::fmt::Debug for WebFetcher {
    /// Zeigt Konfiguration ohne Client und ohne Hostlisten.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebFetcher")
            .field("cache_dir", &self.cache_dir)
            .field("ttl_secs", &self.options.ttl.as_secs())
            .field("max_bytes", &self.options.max_bytes)
            .field("max_output_bytes", &self.options.max_output_bytes)
            .field("allow_http", &self.options.allow_http)
            .field("policy_hosts", &self.policy.allow_hosts().len())
            .field("allowed_targets", &self.network.targets().count())
            .finish()
    }
}

impl WebFetcher {
    /// Baut einen Fetcher mit Egress-Policy und leerem Sandbox-Scope.
    ///
    /// # Description
    /// Der Client entsteht ausschließlich über [`harw_egress::build_client`].
    /// Limits werden auf die harten Obergrenzen gedeckelt.
    ///
    /// # Arguments
    /// - `policy` (`Arc<EgressPolicy>`): Egress-Policy (aus `[network]`); geteilt.
    /// - `cache_dir` (`PathBuf`): Basis-Cache-Verzeichnis unter `HARW_HOME`
    ///   (`harw_home::paths::cache_dir(home)`); Einträge liegen in `web/`.
    /// - `options` ([`WebFetchOptions`]): Limits und Schalter.
    ///
    /// # Returns
    /// Einen Fetcher, der ohne [`Self::scoped`] kein Ziel erreicht.
    ///
    /// # Errors
    /// - [`WebToolError::NotConfigured`]: der Egress-Client ließ sich nicht bauen.
    ///
    /// # Concurrency
    /// Baut einen eigenen Verbindungspool; pro Prozess genügt ein Basis-Fetcher.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use std::{path::PathBuf, sync::Arc};
    /// use harw_egress::EgressPolicy;
    /// use harw_tool_web::fetch::{WebFetchOptions, WebFetcher};
    ///
    /// let policy = Arc::new(EgressPolicy::new(vec!["crates.io".into()], false).unwrap());
    /// let fetcher = WebFetcher::new(policy, PathBuf::from("/home/u/.harw/cache"), WebFetchOptions::default())?;
    /// # Ok::<(), harw_tool_web::WebToolError>(())
    /// ```
    pub fn new(
        policy: Arc<EgressPolicy>,
        cache_dir: PathBuf,
        options: WebFetchOptions,
    ) -> WebToolResult<Self> {
        let client = harw_egress::build_client(Arc::clone(&policy)).map_err(|error| {
            tracing::error!(error = %error, "web.fetch.client_build_failed");
            WebToolError::NotConfigured {
                what: "der Egress-HTTP-Client ließ sich nicht bauen",
            }
        })?;
        let policy_digest = policy.digest();
        Ok(Self {
            client,
            policy,
            policy_digest,
            cache_dir,
            options: options.clamped(),
            network: NetworkScope::empty(),
            cache_scope: CacheScope::unbound(),
        })
    }

    /// Leitet eine Kopie mit Sandbox-Scope, Cache-Scope und optionalem Limit ab.
    ///
    /// # Description
    /// Client und Policy werden geteilt. `max_bytes` kann das Limit nur senken.
    ///
    /// # Arguments
    /// - `network` ([`NetworkScope`]): Scope der aktiven Sandbox.
    /// - `cache_scope` ([`CacheScope`]): Isolationsbereich des Aufrufs.
    /// - `max_bytes` (`Option<usize>`): angefordertes Byte-Limit.
    ///
    /// # Returns
    /// Den gebundenen Fetcher.
    ///
    /// # Concurrency
    /// `Send + Sync`; teilt Client und Policy mit `self`.
    #[must_use]
    pub fn scoped(
        &self,
        network: NetworkScope,
        cache_scope: CacheScope,
        max_bytes: Option<usize>,
    ) -> Self {
        let limit = max_bytes
            .map_or(self.options.max_bytes, |requested| requested.min(self.options.max_bytes))
            .max(1);
        Self {
            client: self.client.clone(),
            policy: Arc::clone(&self.policy),
            policy_digest: self.policy_digest,
            cache_dir: self.cache_dir.clone(),
            options: WebFetchOptions {
                max_bytes: limit,
                ..self.options
            },
            network,
            cache_scope,
        }
    }

    /// Das Basis-Cache-Verzeichnis.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Das durchgesetzte Byte-Limit.
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        self.options.max_bytes
    }

    /// Die wirksamen Optionen.
    #[must_use]
    pub fn options(&self) -> WebFetchOptions {
        self.options
    }

    /// Die Egress-Policy.
    #[must_use]
    pub fn policy(&self) -> &EgressPolicy {
        &self.policy
    }

    /// Der Cache-Isolationsbereich.
    #[must_use]
    pub fn cache_scope(&self) -> CacheScope {
        self.cache_scope
    }

    /// Prüft ein Ziel mit Policy, Scope und Schema-Regel dieses Fetchers.
    ///
    /// # Arguments
    /// - `url` (`&str`): die URL.
    /// - `hop` (`usize`): `0` Start, `n` n-te Weiterleitung.
    ///
    /// # Returns
    /// Das geprüfte [`HopTarget`].
    ///
    /// # Errors
    /// Siehe [`check_hop`].
    ///
    /// # Concurrency
    /// Rein auf `&self`.
    pub fn check_target(&self, url: &str, hop: usize) -> WebToolResult<HopTarget> {
        check_hop(&self.policy, &self.network, self.options.allow_http, url, hop)
    }

    /// Prüft, ob ein gelesener Cache-Eintrag verwendet werden darf.
    ///
    /// # Description
    /// Formale Übereinstimmung ([`entry_matches`]), Kettenlänge höchstens
    /// [`MAX_REDIRECTS`]` + 1`, **jede** URL der Kette besteht erneut
    /// [`Self::check_target`] in unveränderter Normalform, der Medientyp steht
    /// noch auf der Positivliste und der Körper passt ins aktuelle Byte-Limit.
    ///
    /// # Arguments
    /// - `entry` (`&CacheEntry`): gelesener Eintrag.
    /// - `key_hex` (`&str`): erwarteter Schlüssel.
    /// - `request_url` (`&str`): normalisierte Anfrage-URL.
    ///
    /// # Returns
    /// `Ok(())`, wenn der Eintrag verwendbar ist.
    ///
    /// # Errors
    /// - [`WebToolError::CacheCorrupt`]: formale Abweichung.
    /// - Ablehnungen aus [`check_hop`] / [`check_content_type`].
    /// - [`WebToolError::ResponseTooLarge`]: Körper größer als das Limit.
    ///
    /// # Concurrency
    /// Rein auf `&self`.
    pub fn validate_cached(
        &self,
        entry: &CacheEntry,
        key_hex: &str,
        request_url: &str,
    ) -> WebToolResult<()> {
        let mismatch = || WebToolError::CacheCorrupt {
            path: key_hex.to_owned(),
        };
        if !entry_matches(entry, key_hex, request_url) || entry.chain.len() > MAX_REDIRECTS + 1 {
            return Err(mismatch());
        }
        let mut last_host = String::new();
        for (hop, url) in entry.chain.iter().enumerate() {
            let checked = self.check_target(url, hop)?;
            if checked.url() != url {
                return Err(mismatch());
            }
            last_host = checked.host().to_owned();
        }
        check_content_type(&entry.content_type, &last_host)?;
        if entry.body.len() > self.options.max_bytes {
            return Err(WebToolError::ResponseTooLarge {
                limit: self.options.max_bytes,
                host: last_host,
            });
        }
        Ok(())
    }

    /// Holt eine Ressource, aufbereitet und gekappt.
    ///
    /// # Description
    /// [`Self::fetch_source`], danach [`finish_document`] auf dem Blocking-Pool
    /// (HTML-Bereinigung, Text/Markdown, Kappung auf `max_output_bytes`).
    ///
    /// # Arguments
    /// - `url` (`&str`): vollständige URL.
    /// - `format` ([`OutputFormat`]): Ausgabeform.
    ///
    /// # Returns
    /// Ein [`FetchedDocument`] mit aufbereitetem Körper.
    ///
    /// # Errors
    /// Siehe [`Self::fetch_source`]; zusätzlich [`WebToolError::BlockingTask`].
    ///
    /// # Concurrency
    /// Nebenläufig aufrufbar; benötigt Tokio mit `rt` und `time`.
    pub async fn fetch(&self, url: &str, format: OutputFormat) -> WebToolResult<FetchedDocument> {
        let source = self.fetch_source(url).await?;
        let max_output = self.options.max_output_bytes;
        run_blocking("web-render", move || finish_document(source, format, max_output)).await
    }

    /// Holt den Rohkörper einer Ressource (Cache, Conditional-GET, Redirects).
    ///
    /// # Description
    /// Ablauf: Start-URL prüfen → Schlüssel → Cache lesen und **validieren**
    /// ([`Self::validate_cached`]) → frischer Treffer sofort → sonst
    /// Conditional-GET unter [`TOTAL_DEADLINE_SECS`] → `304` erneuert den
    /// Zeitstempel, `2xx` schreibt den Eintrag. Bei Transportfehlern Fail-open
    /// nur auf den validierten Eintrag. `body` ist **nicht** textgekappt; der
    /// Aufrufer kappt, bevor er ihn an das Modell gibt.
    ///
    /// # Arguments
    /// - `url` (`&str`): vollständige URL.
    ///
    /// # Returns
    /// Das Rohdokument (`truncated == false`).
    ///
    /// # Errors
    /// - [`WebToolError::SchemeNotAllowed`], [`WebToolError::HostNotResolvable`],
    ///   [`WebToolError::EgressDenied`], [`WebToolError::RedirectHostNotAllowed`]:
    ///   Start- oder Redirect-Ziel abgelehnt.
    /// - [`WebToolError::TooManyRedirects`], [`WebToolError::ResponseTooLarge`],
    ///   [`WebToolError::UnexpectedContentType`], [`WebToolError::UpstreamStatus`],
    ///   [`WebToolError::UpstreamTimeout`], [`WebToolError::Http`],
    ///   [`WebToolError::BlockingTask`].
    ///
    /// # Concurrency
    /// Nebenläufig aufrufbar; Cache-I/O auf dem Blocking-Pool.
    pub async fn fetch_source(&self, url: &str) -> WebToolResult<FetchedDocument> {
        let start = self.check_target(url, 0)?;
        let key = cache_key(&self.policy_digest, &self.cache_scope, start.url());
        let key_hex = to_hex(&key);
        let path = cache_path(&self.cache_dir, &key);

        let cached = self.load_cached(path.clone(), &key_hex, start.url()).await;

        if let Some(entry) = cached.as_ref() {
            if entry_is_fresh(entry, self.options.ttl) {
                tracing::debug!(host = %start.host(), "web.fetch.cache_hit");
                return Ok(document_from_entry(entry, entry.status, true));
            }
        }

        let etag = cached.as_ref().and_then(|entry| entry.etag.clone());
        let deadline = Duration::from_secs(TOTAL_DEADLINE_SECS);
        let outcome =
            match tokio::time::timeout(deadline, self.fetch_chain(&start, etag.as_deref())).await {
                Ok(result) => result,
                Err(_) => Err(WebToolError::UpstreamTimeout {
                    host: start.host().to_owned(),
                    seconds: TOTAL_DEADLINE_SECS,
                }),
            };

        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(err) if err.is_transport() => {
                // Fail-open nur bei Transportfehlern und nur auf einen Eintrag,
                // der die Egress-Prüfung eben erneut bestanden hat.
                let Some(entry) = cached else {
                    return Err(err);
                };
                tracing::warn!(host = %start.host(), error = %err, "web.fetch.stale_cache_fallback");
                return Ok(document_from_entry(&entry, entry.status, true));
            }
            Err(err) => return Err(err),
        };

        let (entry, status, from_cache) = match outcome {
            RawOutcome::NotModified => {
                let Some(entry) = cached else {
                    return Err(WebToolError::UpstreamStatus {
                        status: 304,
                        host: start.host().to_owned(),
                    });
                };
                tracing::debug!(host = %start.host(), "web.fetch.not_modified");
                let refreshed = CacheEntry {
                    fetched_at: now_secs(),
                    ..entry
                };
                (refreshed, 304, true)
            }
            RawOutcome::Body {
                status,
                chain,
                content_type,
                etag,
                bytes,
            } => {
                tracing::info!(
                    host = %start.host(),
                    status,
                    bytes = bytes.len(),
                    hops = chain.len().saturating_sub(1),
                    "web.fetch.done"
                );
                let entry = CacheEntry {
                    format_version: CACHE_FORMAT_VERSION,
                    key: key_hex,
                    chain,
                    status,
                    etag,
                    fetched_at: now_secs(),
                    content_type,
                    body: String::from_utf8_lossy(&bytes).into_owned(),
                };
                (entry, status, false)
            }
        };

        let document = document_from_entry(&entry, status, from_cache);
        self.store(path, entry).await;
        Ok(document)
    }

    /// Liest und validiert einen Cache-Eintrag; jede Abweichung ist ein Miss.
    async fn load_cached(
        &self,
        path: PathBuf,
        key_hex: &str,
        request_url: &str,
    ) -> Option<CacheEntry> {
        let entry = match run_blocking("web-cache-read", move || read_cache_entry(&path)).await {
            Ok(Some(entry)) => entry,
            Ok(None) => return None,
            Err(err) => {
                tracing::warn!(error = %err, "web.cache.unreadable");
                return None;
            }
        };
        match self.validate_cached(&entry, key_hex, request_url) {
            Ok(()) => Some(entry),
            Err(err) => {
                tracing::warn!(error = %err, "web.cache.discarded");
                None
            }
        }
    }

    /// Schreibt einen Eintrag auf dem Blocking-Pool; Fehler werden protokolliert.
    async fn store(&self, path: PathBuf, entry: CacheEntry) {
        let base = self.cache_dir.clone();
        let result =
            run_blocking("web-cache-write", move || write_cache_entry(&base, &path, &entry)).await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "web.cache.write_failed");
        }
    }

    /// Führt die Redirect-Kette und liest den Körper unter dem Byte-Limit.
    async fn fetch_chain(&self, start: &HopTarget, etag: Option<&str>) -> WebToolResult<RawOutcome> {
        let mut current = start.clone();
        let mut chain = vec![start.url().to_owned()];
        let mut hop = 0usize;

        loop {
            let mut request = self
                .client
                .get(current.url())
                .header(reqwest::header::USER_AGENT, USER_AGENT_VALUE)
                .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS));
            if hop == 0 {
                if let Some(tag) = etag {
                    request = request.header(reqwest::header::IF_NONE_MATCH, tag);
                }
            }

            let response = request
                .send()
                .await
                .map_err(|error| map_send_error(error, hop))?;
            let status = response.status();

            // 304 ist nur als Antwort auf den eigenen Conditional-GET gültig.
            if status == reqwest::StatusCode::NOT_MODIFIED && hop == 0 && etag.is_some() {
                return Ok(RawOutcome::NotModified);
            }

            if status.is_redirection() {
                if hop >= MAX_REDIRECTS {
                    return Err(WebToolError::TooManyRedirects {
                        url: format!("<Weiterleitung {}>", hop + 1),
                    });
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let Some(location) = location else {
                    return Err(WebToolError::UpstreamStatus {
                        status: status.as_u16(),
                        host: current.host().to_owned(),
                    });
                };
                let next_hop = hop + 1;
                let next_url = resolve_location(current.url(), &location, next_hop)?;
                // Pflicht: jedes Redirect-Ziel vor dem Senden prüfen (IP-Literale
                // erreichen den Resolver nie).
                let next = self.check_target(&next_url, next_hop)?;
                tracing::debug!(from = %current.host(), to = %next.host(), hop = next_hop, "web.fetch.redirect");
                chain.push(next.url().to_owned());
                current = next;
                hop = next_hop;
                continue;
            }

            if !status.is_success() {
                return Err(WebToolError::UpstreamStatus {
                    status: status.as_u16(),
                    host: current.host().to_owned(),
                });
            }

            let raw_content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let content_type = check_content_type(&raw_content_type, current.host())?;

            let limit = self.options.max_bytes;
            if let Some(announced) = response.content_length() {
                if announced > u64::try_from(limit).unwrap_or(u64::MAX) {
                    return Err(WebToolError::ResponseTooLarge {
                        limit,
                        host: current.host().to_owned(),
                    });
                }
            }

            let response_etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);

            let mut response = response;
            let mut buffer: Vec<u8> = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| map_send_error(error, hop))?
            {
                push_chunk(&mut buffer, &chunk, limit, current.host())?;
            }

            return Ok(RawOutcome::Body {
                status: status.as_u16(),
                chain,
                content_type,
                etag: response_etag,
                bytes: buffer,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Prozessweiter Fetcher
// ---------------------------------------------------------------------------

/// Liefert den prozessweit installierten Basis-Fetcher.
///
/// # Description
/// Es gibt **keinen** Default: ohne [`install_fetcher`] bzw.
/// [`crate::configure`] existiert keine Egress-Policy, und jeder Abruf
/// scheitert fail-closed.
///
/// # Returns
/// Einen `Arc` auf den Basis-Fetcher.
///
/// # Errors
/// - [`WebToolError::NotConfigured`]: kein Fetcher installiert.
///
/// # Concurrency
/// Thread-sicher über [`OnceLock`].
pub fn shared_fetcher() -> WebToolResult<Arc<WebFetcher>> {
    SHARED_FETCHER
        .get()
        .map(Arc::clone)
        .ok_or(WebToolError::NotConfigured {
            what: "keine Egress-Policy installiert (harw_tool_web::configure fehlt)",
        })
}

/// Hinterlegt den prozessweiten Basis-Fetcher.
///
/// # Description
/// Nur der erste Aufruf gewinnt; danach bleibt der Fetcher unveränderlich.
///
/// # Arguments
/// - `fetcher` (`Arc<WebFetcher>`): der zu hinterlegende Fetcher.
///
/// # Returns
/// `Ok(())`, wenn dieser Aufruf den Fetcher gesetzt hat.
///
/// # Errors
/// Gibt den übergebenen Fetcher zurück, wenn bereits einer gesetzt war.
///
/// # Concurrency
/// Thread-sicher über [`OnceLock`].
pub fn install_fetcher(fetcher: Arc<WebFetcher>) -> Result<(), Arc<WebFetcher>> {
    SHARED_FETCHER.set(fetcher)
}

/// Leitet aus dem Tool-Kontext einen sandbox-gebundenen Fetcher ab.
///
/// # Description
/// Übernimmt [`NetworkScope`] und [`CacheScope`] (Mandant, Workspace,
/// Netz-Scope) des Aufrufs, damit Allowlist und Cache-Isolation an den Aufruf
/// gebunden sind.
///
/// # Arguments
/// - `context` (`&ToolExecutionContext`): die vom Harness etablierte Autorität.
/// - `max_bytes` (`Option<usize>`): vom Modell angefordertes Byte-Limit.
///
/// # Returns
/// Den gebundenen Fetcher.
///
/// # Errors
/// - [`WebToolError::NotConfigured`]: kein Basis-Fetcher installiert.
///
/// # Concurrency
/// Nebenläufig aufrufbar.
pub fn scoped_fetcher(
    context: &ToolExecutionContext,
    max_bytes: Option<usize>,
) -> WebToolResult<WebFetcher> {
    let shared = shared_fetcher()?;
    Ok(shared.scoped(
        context.sandbox().network_scope().clone(),
        CacheScope::from_context(context),
        max_bytes,
    ))
}

/// Serialisiert ein [`FetchedDocument`] als Tool-Ausgabe.
///
/// # Arguments
/// - `document` (`&FetchedDocument`): das aufbereitete Dokument.
///
/// # Returns
/// Eine JSON-[`ToolOutput`].
///
/// # Concurrency
/// Rein.
#[must_use]
pub fn document_output(document: &FetchedDocument) -> ToolOutput {
    ToolOutput::json(serde_json::json!({
        "url": document.url,
        "status": document.status,
        "content_type": document.content_type,
        "from_cache": document.from_cache,
        "etag": document.etag,
        "truncated": document.truncated,
        "body": document.body,
    }))
}

// ---------------------------------------------------------------------------
// Tool `web.fetch`
// ---------------------------------------------------------------------------

/// Führt `web.fetch` aus.
///
/// Permission- und Host-Prolog stammen aus `#[harw_macros::tool]`; hier bleiben
/// Formatwahl, Scope-Ableitung und Abruf.
#[harw_macros::tool(
    name = "web.fetch",
    description = "Lädt eine HTTPS-Ressource und liefert Text oder Markdown.",
    permission = "network_access",
    host_from = "url",
    parallel_safe
)]
async fn web_fetch(
    context: &ToolExecutionContext,
    args: FetchArgs,
) -> Result<ToolOutput, ToolsError> {
    let Some(output_format) = OutputFormat::parse(args.format.as_deref()) else {
        return Ok(ToolOutput::error(format!(
            "Tool '{}': unbekanntes Ausgabeformat, erlaubt sind 'text', 'markdown' und 'raw'",
            WebFetchTool::NAME
        )));
    };

    let fetcher = match scoped_fetcher(context, args.max_bytes) {
        Ok(fetcher) => fetcher,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };

    match fetcher.fetch(&args.url, output_format).await {
        Ok(document) => Ok(document_output(&document)),
        Err(err) => Ok(ToolOutput::error(err.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{ErrorKind, Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;
    use std::time::Instant;
    use tempfile::TempDir;

    // --- Hilfen -------------------------------------------------------------

    /// Minimaler HTTP/1.1-Server auf Loopback: beantwortet genau
    /// `responses.len()` Verbindungen der Reihe nach und liefert die
    /// empfangenen Anfrageköpfe zurück (Abbruch nach 10 s ohne Verbindung).
    struct TestServer {
        base: String,
        handle: JoinHandle<Vec<String>>,
    }

    impl TestServer {
        fn spawn(responses: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("Loopback-Bind");
            let base = format!("http://{}", listener.local_addr().expect("Adresse"));
            listener.set_nonblocking(true).expect("nonblocking");
            let handle = std::thread::spawn(move || {
                let mut requests = Vec::new();
                for response in responses {
                    let deadline = Instant::now() + Duration::from_secs(10);
                    let stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break Some(stream),
                            Err(err)
                                if err.kind() == ErrorKind::WouldBlock
                                    && Instant::now() < deadline =>
                            {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => break None,
                        }
                    };
                    let Some(mut stream) = stream else { break };
                    stream.set_nonblocking(false).ok();
                    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    let mut head = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => head.extend_from_slice(&chunk[..n]),
                        }
                    }
                    requests.push(String::from_utf8_lossy(&head).into_owned());
                    stream.write_all(response.as_bytes()).ok();
                    stream.flush().ok();
                }
                requests
            });
            Self { base, handle }
        }

        fn requests(self) -> Vec<String> {
            self.handle.join().expect("Server-Thread")
        }
    }

    fn http_response(status_line: &str, headers: &[(&str, &str)], body: &str) -> String {
        let mut out = format!("HTTP/1.1 {status_line}\r\n");
        for (name, value) in headers {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        out
    }

    fn loopback_options() -> WebFetchOptions {
        WebFetchOptions {
            allow_http: true,
            ..WebFetchOptions::default()
        }
    }

    /// Fetcher mit Policy `hosts` (+ `allow_private`), gleichem Sandbox-Scope
    /// und festem Cache-Scope.
    fn fetcher_for(dir: &Path, hosts: &[&str], allow_private: bool, options: WebFetchOptions) -> WebFetcher {
        let owned: Vec<String> = hosts.iter().map(|host| (*host).to_owned()).collect();
        let policy = Arc::new(EgressPolicy::new(owned.clone(), allow_private).expect("Policy"));
        let network = NetworkScope::from_hosts(owned);
        let scope = CacheScope::new("tenant", "workspace", &network);
        WebFetcher::new(policy, dir.to_path_buf(), options)
            .expect("Client baut")
            .scoped(network, scope, None)
    }

    fn entry_for(fetcher: &WebFetcher, chain: Vec<String>, body: &str, fetched_at: u64) -> (PathBuf, CacheEntry) {
        let request = chain.first().cloned().unwrap_or_default();
        let key = cache_key(&fetcher.policy().digest(), &fetcher.cache_scope(), &request);
        let entry = CacheEntry {
            format_version: CACHE_FORMAT_VERSION,
            key: to_hex(&key),
            chain,
            status: 200,
            etag: Some("\"v1\"".to_owned()),
            fetched_at,
            content_type: "text/plain".to_owned(),
            body: body.to_owned(),
        };
        (cache_path(fetcher.cache_dir(), &key), entry)
    }

    // --- Content-Type und Byte-Limit ----------------------------------------

    /// Text- und JSON-artige Typen sind erlaubt, Parameter werden verworfen.
    #[test]
    fn test_check_content_type_accepts_text_and_json() {
        assert_eq!(
            check_content_type("text/html; charset=utf-8", "docs.rs").expect("erlaubt"),
            "text/html"
        );
        assert_eq!(
            check_content_type("application/vnd.api+json", "crates.io").expect("erlaubt"),
            "application/vnd.api+json"
        );
    }

    /// Binärformate und fehlender Header werden abgelehnt.
    #[test]
    fn test_check_content_type_rejects_binary_and_missing() {
        assert!(matches!(
            check_content_type("application/octet-stream", "docs.rs"),
            Err(WebToolError::UnexpectedContentType { .. })
        ));
        assert!(matches!(
            check_content_type("", "docs.rs"),
            Err(WebToolError::UnexpectedContentType { .. })
        ));
    }

    /// Ein Chunk-Stream bricht exakt an der Grenze ab, ohne Teilübernahme.
    #[test]
    fn test_push_chunk_stops_at_limit_and_keeps_buffer_intact() {
        let mut buffer = Vec::new();
        assert!(push_chunk(&mut buffer, b"aaaa", 10, "docs.rs").is_ok());
        assert!(push_chunk(&mut buffer, b"bbbb", 10, "docs.rs").is_ok());
        let err = push_chunk(&mut buffer, b"cccc", 10, "docs.rs").expect_err("Limit");
        assert!(matches!(err, WebToolError::ResponseTooLarge { limit: 10, .. }));
        assert_eq!(buffer.len(), 8);
        assert!(push_chunk(&mut Vec::new(), b"x", 0, "docs.rs").is_err());
    }

    // --- Aufbereitung -------------------------------------------------------

    /// `raw` lässt HTML unangetastet; Nicht-HTML bleibt in allen Formen gleich.
    #[test]
    fn test_render_raw_and_non_html_pass_through() {
        let html = "<p>x</p><script>y</script>";
        assert_eq!(render(html, "text/html", OutputFormat::Raw).expect("raw"), html);
        let json = r#"{"a":"<script>"}"#;
        for format in [OutputFormat::Text, OutputFormat::Markdown, OutputFormat::Raw] {
            assert_eq!(render(json, "application/json", format).expect("json"), json);
        }
    }

    /// Text-Form entfernt script/style.
    #[test]
    fn test_render_text_strips_script_and_style() {
        let html = "<body><style>p{}</style><p>sichtbar</p><script>unsichtbar</script></body>";
        assert_eq!(render(html, "text/html", OutputFormat::Text).expect("text"), "sichtbar");
    }

    /// `finish_document` kappt nach der Aufbereitung an einer Zeichengrenze.
    #[test]
    fn test_finish_document_truncates_on_multibyte_boundary() {
        let raw = FetchedDocument {
            url: "https://docs.rs/".to_owned(),
            status: 200,
            content_type: "text/html".to_owned(),
            body: "<body><script>xxxxxxxx</script><p>ääää</p></body>".to_owned(),
            from_cache: false,
            etag: None,
            truncated: false,
        };
        let done = finish_document(raw, OutputFormat::Text, 5).expect("fertig");
        assert_eq!(done.body, "ää");
        assert!(done.truncated);
    }

    /// Die Formatwahl akzeptiert die dokumentierten Werte und lehnt andere ab.
    #[test]
    fn test_output_format_parse_known_and_unknown_values() {
        assert_eq!(OutputFormat::parse(Some("  ")), Some(OutputFormat::Text));
        assert_eq!(OutputFormat::parse(Some("RAW")), Some(OutputFormat::Raw));
        assert_eq!(OutputFormat::parse(Some("md")), Some(OutputFormat::Markdown));
        assert_eq!(OutputFormat::parse(Some("pdf")), None);
    }

    // --- Konfiguration ------------------------------------------------------

    /// Ohne installierten Fetcher gibt es keinen Default (fail-closed).
    #[test]
    fn test_shared_fetcher_without_configuration_is_not_configured() {
        // Kein Test dieses Crates installiert einen Fetcher.
        assert!(matches!(shared_fetcher(), Err(WebToolError::NotConfigured { .. })));
    }

    /// Limits werden gedeckelt; `scoped` kann nur senken.
    #[test]
    fn test_scoped_caps_requested_max_bytes() {
        let dir = TempDir::new().expect("Tempdir");
        let policy = Arc::new(EgressPolicy::new(vec!["docs.rs".to_owned()], false).expect("Policy"));
        let options = WebFetchOptions {
            max_bytes: usize::MAX,
            max_output_bytes: 0,
            ..WebFetchOptions::default()
        };
        let base = WebFetcher::new(policy, dir.path().to_path_buf(), options).expect("Client");
        assert_eq!(base.max_bytes(), HARD_MAX_BYTES);
        assert_eq!(base.options().max_output_bytes, 1);

        let scope = CacheScope::unbound();
        assert_eq!(base.scoped(NetworkScope::empty(), scope, Some(usize::MAX)).max_bytes(), HARD_MAX_BYTES);
        assert_eq!(base.scoped(NetworkScope::empty(), scope, Some(2_048)).max_bytes(), 2_048);
        assert_eq!(base.scoped(NetworkScope::empty(), scope, Some(0)).max_bytes(), 1);
    }

    /// Ohne Sandbox-Scope ist kein Host erreichbar, auch wenn die Policy ihn erlaubt.
    #[tokio::test]
    async fn test_fetch_without_scope_denies_every_host() {
        let dir = TempDir::new().expect("Tempdir");
        let policy = Arc::new(EgressPolicy::new(vec!["docs.rs".to_owned()], false).expect("Policy"));
        let fetcher = WebFetcher::new(policy, dir.path().to_path_buf(), WebFetchOptions::default())
            .expect("Client");
        let err = fetcher
            .fetch("https://docs.rs/serde/", OutputFormat::Text)
            .await
            .expect_err("leerer Scope");
        assert!(matches!(err, WebToolError::RedirectHostNotAllowed { .. }), "{err:?}");
    }

    /// IP-Literal-URL wird vor jedem Netzkontakt abgelehnt, ohne Adresse in der Meldung.
    #[tokio::test]
    async fn test_fetch_rejects_ip_literal_url_before_network() {
        let dir = TempDir::new().expect("Tempdir");
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1", "169.254.169.254"], false, WebFetchOptions::default());
        for url in ["https://127.0.0.1/", "https://169.254.169.254/latest/meta-data/"] {
            let err = fetcher.fetch(url, OutputFormat::Raw).await.expect_err(url);
            assert!(matches!(err, WebToolError::EgressDenied { .. }), "{url}: {err:?}");
            let message = err.to_string();
            assert!(!message.contains("127.0.0.1") && !message.contains("169.254"), "{message}");
        }
    }

    /// `http://` ohne Freigabe scheitert am Schema.
    #[tokio::test]
    async fn test_fetch_rejects_http_without_opt_in() {
        let dir = TempDir::new().expect("Tempdir");
        let fetcher = fetcher_for(dir.path(), &["docs.rs"], false, WebFetchOptions::default());
        let err = fetcher
            .fetch("http://docs.rs/serde/", OutputFormat::Text)
            .await
            .expect_err("http");
        assert!(matches!(err, WebToolError::SchemeNotAllowed { .. }), "{err:?}");
    }

    // --- Redirects gegen einen Loopback-Server ------------------------------

    /// F-035: Redirect auf den Metadaten-Endpunkt wird vor dem Senden abgelehnt.
    #[tokio::test]
    async fn test_fetch_redirect_to_metadata_ip_is_rejected() {
        let server = TestServer::spawn(vec![http_response(
            "302 Found",
            &[("Location", "http://169.254.169.254/latest/meta-data/")],
            "",
        )]);
        let dir = TempDir::new().expect("Tempdir");
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1", "169.254.169.254"], true, loopback_options());

        let err = fetcher
            .fetch(&format!("{}/start", server.base), OutputFormat::Raw)
            .await
            .expect_err("Metadaten-Redirect");
        assert!(matches!(err, WebToolError::EgressDenied { .. }), "{err:?}");
        assert!(!err.to_string().contains("169.254"), "{err}");
        assert_eq!(server.requests().len(), 1, "nur die Start-Anfrage darf rausgehen");
    }

    /// Redirect auf einen Host außerhalb der Policy wird abgelehnt.
    #[tokio::test]
    async fn test_fetch_redirect_to_host_outside_policy_is_rejected() {
        let server = TestServer::spawn(vec![http_response(
            "301 Moved Permanently",
            &[("Location", "https://evil.test/steal")],
            "",
        )]);
        let dir = TempDir::new().expect("Tempdir");
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options());

        let err = fetcher
            .fetch(&format!("{}/start", server.base), OutputFormat::Raw)
            .await
            .expect_err("fremder Host");
        assert!(matches!(err, WebToolError::EgressDenied { .. }), "{err:?}");
        assert!(!err.to_string().contains("steal"), "{err}");
        assert_eq!(server.requests().len(), 1);
    }

    /// Mehr als `MAX_REDIRECTS` Weiterleitungen brechen ab.
    #[tokio::test]
    async fn test_fetch_redirect_chain_beyond_limit_is_rejected() {
        let responses = (0..=MAX_REDIRECTS)
            .map(|_| http_response("302 Found", &[("Location", "/loop")], ""))
            .collect();
        let server = TestServer::spawn(responses);
        let dir = TempDir::new().expect("Tempdir");
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options());

        let err = fetcher
            .fetch(&format!("{}/start", server.base), OutputFormat::Raw)
            .await
            .expect_err("Kette zu lang");
        assert!(matches!(err, WebToolError::TooManyRedirects { .. }), "{err:?}");
        assert_eq!(server.requests().len(), MAX_REDIRECTS + 1);
    }

    /// Erfolgreicher Abruf über einen erlaubten Redirect: Kette im Cache,
    /// Text bereinigt und UTF-8-sicher gekappt.
    #[tokio::test]
    async fn test_fetch_follows_allowed_redirect_and_caps_text() {
        let server = TestServer::spawn(vec![
            http_response("302 Found", &[("Location", "/ziel")], ""),
            http_response(
                "200 OK",
                &[("Content-Type", "text/html; charset=utf-8")],
                "<html><body><script>GEHEIM</script><p>ääää</p></body></html>",
            ),
        ]);
        let dir = TempDir::new().expect("Tempdir");
        let options = WebFetchOptions {
            max_output_bytes: 5,
            ..loopback_options()
        };
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, options);
        let start = format!("{}/start", server.base);
        let target = format!("{}/ziel", server.base);

        let document = fetcher.fetch(&start, OutputFormat::Text).await.expect("Abruf");
        assert_eq!(document.url, target);
        assert_eq!(document.body, "ää");
        assert!(document.truncated);
        assert!(!document.from_cache);
        assert_eq!(server.requests().len(), 2);

        // Zweiter Abruf: frischer, validierter Cache-Treffer ohne Netz; `raw`
        // liefert den gespeicherten Rohkörper, gekappt auf 5 Bytes.
        let again = fetcher.fetch(&start, OutputFormat::Raw).await.expect("Cache");
        assert!(again.from_cache);
        assert_eq!(again.url, target);
        assert_eq!(again.body, "<html");
        assert!(again.truncated);
    }

    // --- Cache-Validierung --------------------------------------------------

    /// F-035: ein frischer Treffer, dessen Endziel nun verboten ist, wird
    /// verworfen und neu geholt.
    #[tokio::test]
    async fn test_fetch_discards_cache_hit_with_forbidden_final_target() {
        let server = TestServer::spawn(vec![http_response(
            "200 OK",
            &[("Content-Type", "text/plain")],
            "frisch",
        )]);
        let dir = TempDir::new().expect("Tempdir");
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options());
        let start = fetcher
            .check_target(&format!("{}/doc", server.base), 0)
            .expect("Start erlaubt");
        let (path, poisoned) = entry_for(
            &fetcher,
            vec![start.url().to_owned(), "https://evil.test/doc".to_owned()],
            "vergiftet",
            now_secs(),
        );
        write_cache_entry(fetcher.cache_dir(), &path, &poisoned).expect("Cache schreiben");

        let document = fetcher.fetch(start.url(), OutputFormat::Raw).await.expect("Abruf");
        assert!(!document.from_cache, "der vergiftete Eintrag darf nicht bedienen");
        assert_eq!(document.body, "frisch");
        assert_eq!(server.requests().len(), 1);
    }

    /// Ein verworfener Eintrag rettet auch keinen Transportfehler (kein Fail-open).
    #[tokio::test]
    async fn test_fetch_stale_fallback_never_uses_forbidden_entry() {
        let closed = TcpListener::bind("127.0.0.1:0").expect("Bind");
        let base = format!("http://{}", closed.local_addr().expect("Adresse"));
        drop(closed);
        let dir = TempDir::new().expect("Tempdir");
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options());
        let start = fetcher.check_target(&format!("{base}/doc"), 0).expect("Start");
        let (path, poisoned) = entry_for(
            &fetcher,
            vec![start.url().to_owned(), "http://10.0.0.1.nip.io/x".to_owned()],
            "vergiftet",
            0,
        );
        write_cache_entry(fetcher.cache_dir(), &path, &poisoned).expect("Cache schreiben");

        let err = fetcher.fetch(start.url(), OutputFormat::Raw).await.expect_err("kein Fail-open");
        assert!(err.is_transport(), "{err:?}");
    }

    /// Ein gültiger frischer Eintrag wird ohne Netz bedient.
    #[tokio::test]
    async fn test_fetch_serves_valid_fresh_cache_entry_without_network() {
        let closed = TcpListener::bind("127.0.0.1:0").expect("Bind");
        let base = format!("http://{}", closed.local_addr().expect("Adresse"));
        drop(closed);
        let dir = TempDir::new().expect("Tempdir");
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options());
        let start = fetcher.check_target(&format!("{base}/doc"), 0).expect("Start");
        let (path, entry) = entry_for(&fetcher, vec![start.url().to_owned()], "gecacht", now_secs());
        write_cache_entry(fetcher.cache_dir(), &path, &entry).expect("Cache schreiben");

        let document = fetcher.fetch(start.url(), OutputFormat::Raw).await.expect("Cache");
        assert!(document.from_cache);
        assert_eq!(document.body, "gecacht");
        assert_eq!(document.etag.as_deref(), Some("\"v1\""));
    }

    /// Ein Eintrag unter anderer Policy ist unter der neuen Policy unsichtbar.
    #[test]
    fn test_validate_cached_rejects_entry_from_other_policy_key() {
        let dir = TempDir::new().expect("Tempdir");
        let narrow = fetcher_for(dir.path(), &["docs.rs"], false, WebFetchOptions::default());
        let wide = fetcher_for(dir.path(), &["docs.rs", "evil.test"], false, WebFetchOptions::default());
        let url = "https://docs.rs/";
        let (_, entry) = entry_for(&wide, vec![url.to_owned()], "x", now_secs());
        let narrow_key = to_hex(&cache_key(&narrow.policy().digest(), &narrow.cache_scope(), url));

        assert!(matches!(
            narrow.validate_cached(&entry, &narrow_key, url),
            Err(WebToolError::CacheCorrupt { .. })
        ));
        assert!(wide.validate_cached(&entry, &entry.key, url).is_ok());
    }

    /// Conditional-GET: `304` erneuert den Zeitstempel und bedient den Eintrag.
    #[tokio::test]
    async fn test_fetch_conditional_get_refreshes_cache_on_304() {
        let server = TestServer::spawn(vec![http_response("304 Not Modified", &[], "")]);
        let dir = TempDir::new().expect("Tempdir");
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options());
        let start = fetcher.check_target(&format!("{}/doc", server.base), 0).expect("Start");
        let (path, stale) = entry_for(&fetcher, vec![start.url().to_owned()], "alt", 1);
        write_cache_entry(fetcher.cache_dir(), &path, &stale).expect("Cache schreiben");

        let document = fetcher.fetch(start.url(), OutputFormat::Raw).await.expect("304");
        assert_eq!(document.status, 304);
        assert!(document.from_cache);
        assert_eq!(document.body, "alt");

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].to_ascii_lowercase().contains("if-none-match: \"v1\""), "{}", requests[0]);
        let refreshed = read_cache_entry(&path).expect("lesen").expect("vorhanden");
        assert!(refreshed.fetched_at > 1);
    }

    // --- Tool-Deklaration ---------------------------------------------------

    /// Das Tool deklariert Berechtigung wie zugesagt.
    #[test]
    fn test_web_fetch_tool_declares_network_permission() {
        assert_eq!(WebFetchTool::NAME, "web.fetch");
        assert_eq!(WebFetchTool::PERMISSION, Some(harw_tools::Permission::NetworkAccess));
    }

    // `PARALLEL_SAFE` ist eine makro-generierte `const bool`; die Assertion
    // bricht den Build, sobald `web.fetch` nicht mehr parallelsicher ist.
    const _: () = assert!(WebFetchTool::PARALLEL_SAFE);

    /// Das Schema bewirbt `url` als Pflichtfeld und kennt die Optionen.
    #[test]
    fn test_fetch_args_schema_requires_url_only() {
        let harw_tools::ToolSpec::Function(spec) = WebFetchTool::spec();
        assert_eq!(spec.name.as_str(), "web.fetch");
        let required = spec.parameters.required.expect("required-Liste");
        assert_eq!(required, vec!["url".to_owned()]);
        let properties = spec.parameters.properties.expect("properties");
        assert!(properties.contains_key("format"));
        assert!(properties.contains_key("max_bytes"));
    }
}
