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
//!    [`MAX_REDIRECTS`] Weiterleitungen. Unter `web.fetch` muss im offenen
//!    Recherche-Netz jedes Redirect-Ziel außerdem eine freigegebene Domain
//!    treffen ([`WebFetcher::with_redirect_approvals`], [`crate::open_web`]);
//!    eine Freigabe gilt nie jedem Ziel des Servers.
//! 3. **Cache ist isoliert und wird neu geprüft.** Schlüssel =
//!    BLAKE3(Policy-Digest ‖ Mandant/Workspace/Netz-Scope ‖ Anfrage-URL). Ein
//!    Treffer wird nur verwendet, wenn **jede** URL seiner gespeicherten Kette
//!    (inklusive Endziel) erneut die Hop-Prüfung besteht (einschließlich der
//!    Domain-Freigabe der Redirect-Ziele) — auch beim Fail-open nach
//!    Transportfehlern.
//! 4. **Kappung.** Bytes beim Lesen ([`push_chunk`], vor dem Dekodieren),
//!    Text nach der Aufbereitung ([`crate::html::truncate_utf8`],
//!    UTF-8-grenzsicher).
//! 5. **Content-Type-Positivliste.** Text-, JSON- und XML-artige Typen sowie
//!    PDF (`application/pdf`, `application/x-pdf`, `application/octet-stream`
//!    nur mit `%PDF-`-Signatur, siehe [`classify_content_type`]). PDF-Körper
//!    unterliegen **demselben** Byte-Limit und werden vor dem Cachen lokal zu
//!    Text extrahiert ([`extract_pdf_text`]); der Cache enthält nie PDF-Bytes.
//! 6. **Blockierendes außerhalb des Reaktors.** HTML-Parsing, Markdown,
//!    PDF-Extraktion und Cache-I/O laufen über [`run_blocking`]
//!    (`spawn_blocking`).
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
use crate::open_web::OpenWebAccess;
use harw_authority::NetworkScope;
use harw_egress::EgressPolicy;
use harw_macros::Tool;
use harw_tool_doc::native::extract_pages;
use harw_tool_doc::types::ExtractedDocument;
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

/// Normalisierter Medientyp aller PDF-Antworten (auch im Cache-Eintrag).
pub const PDF_MEDIA_TYPE: &str = "application/pdf";

/// Signatur am Anfang jeder PDF-Datei (für das Sniffen von `octet-stream`).
const PDF_MAGIC: &[u8] = b"%PDF-";

/// Hinweis für PDFs ohne extrahierbaren Text (gescannte Seiten).
const PDF_OCR_HINT: &str = "gescannte PDFs ohne Textebene bitte mit `doc.read_pdf` (OCR) lesen";

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
    description = "Lädt eine HTTPS-Ressource und liefert Text oder Markdown; PDFs werden als Text extrahiert."
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

/// Einordnung einer Antwort anhand ihres `Content-Type`.
///
/// # Description
/// Ergebnis von [`classify_content_type`]; steuert, ob der Körper als Text
/// dekodiert, als PDF extrahiert oder erst nach dem Sniffen der Signatur
/// angenommen wird.
///
/// # Concurrency
/// Reine Daten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentClass {
    /// Text-, JSON- oder XML-artiger Typ (normalisierter Medientyp).
    Text(String),
    /// Ausdrücklich deklariertes PDF (`application/pdf`, `application/x-pdf`).
    Pdf,
    /// `application/octet-stream`: nur als PDF zulässig, wenn der Körper mit
    /// `%PDF-` beginnt ([`is_pdf_magic`]).
    SniffPdf,
}

/// Ordnet den `Content-Type` einer Antwort für `web.fetch` ein.
///
/// # Description
/// Erweitert [`check_content_type`] (unverändert Text-only, auch für
/// `web.search`) um PDF: `application/pdf` und `application/x-pdf` gelten als
/// PDF, `application/octet-stream` nur unter Vorbehalt der Signaturprüfung.
/// Alles andere wird wie bisher abgelehnt.
///
/// # Arguments
/// - `raw` (`&str`): roher Header-Wert.
/// - `host` (`&str`): antwortender Host (nur Meldung).
///
/// # Returns
/// Die [`ContentClass`] der Antwort.
///
/// # Errors
/// - [`WebToolError::UnexpectedContentType`]: Typ weder Text noch PDF.
///
/// # Concurrency
/// Rein.
///
/// # Examples
/// ```rust
/// use harw_tool_web::fetch::{classify_content_type, ContentClass};
///
/// assert_eq!(classify_content_type("application/PDF", "a.test").unwrap(), ContentClass::Pdf);
/// assert_eq!(
///     classify_content_type("application/octet-stream", "a.test").unwrap(),
///     ContentClass::SniffPdf
/// );
/// assert!(classify_content_type("image/png", "a.test").is_err());
/// ```
pub fn classify_content_type(raw: &str, host: &str) -> WebToolResult<ContentClass> {
    let media = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match media.as_str() {
        "application/pdf" | "application/x-pdf" => Ok(ContentClass::Pdf),
        "application/octet-stream" => Ok(ContentClass::SniffPdf),
        _ => check_content_type(raw, host).map(ContentClass::Text),
    }
}

/// Sagt, ob `bytes` mit der PDF-Signatur `%PDF-` beginnen.
///
/// # Examples
/// ```rust
/// use harw_tool_web::fetch::is_pdf_magic;
///
/// assert!(is_pdf_magic(b"%PDF-1.4\n"));
/// assert!(!is_pdf_magic(b"PK\x03\x04"));
/// ```
#[must_use]
pub fn is_pdf_magic(bytes: &[u8]) -> bool {
    bytes.starts_with(PDF_MAGIC)
}

/// Prüft den Medientyp eines Cache-Eintrags.
///
/// # Description
/// Wie [`check_content_type`], zusätzlich ist [`PDF_MEDIA_TYPE`] erlaubt —
/// solche Einträge enthalten ausschließlich extrahierten Text.
fn check_cached_content_type(content_type: &str, host: &str) -> WebToolResult<()> {
    if content_type == PDF_MEDIA_TYPE {
        return Ok(());
    }
    check_content_type(content_type, host).map(|_| ())
}

/// Formatiert ein extrahiertes PDF als Markdown-artigen Text.
///
/// # Description
/// Kopfzeile `PDF · <N> Seiten · <url>`, danach je Seite ein Abschnitt
/// `## Seite n`. Enthält keine Seite Text, folgt ein Hinweis auf
/// `doc.read_pdf` mit OCR.
///
/// # Arguments
/// - `document` (`&ExtractedDocument`): Ergebnis von
///   `harw_tool_doc::native::extract_pages`.
/// - `url` (`&str`): Endziel des Abrufs (Kopfzeile).
///
/// # Returns
/// Den aufbereiteten Text (ungekappt).
///
/// # Concurrency
/// Rein.
#[must_use]
pub fn render_pdf_text(document: &ExtractedDocument, url: &str) -> String {
    let total = document
        .total_pages
        .map_or(document.pages.len(), |pages| pages as usize);
    let mut out = format!("PDF · {total} Seiten · {url}\n");
    for page in &document.pages {
        out.push_str(&format!("\n## Seite {}\n\n", page.number));
        let text = page.text.trim();
        if !text.is_empty() {
            out.push_str(text);
            out.push('\n');
        }
    }
    if document
        .pages
        .iter()
        .all(|page| page.text.trim().is_empty())
    {
        out.push_str(&format!("\n_Kein Text extrahierbar — {PDF_OCR_HINT}._\n"));
    }
    out
}

/// Extrahiert den Text eines PDF-Körpers und formatiert ihn.
///
/// # Description
/// Lokale Extraktion über `harw_tool_doc::native::extract_pages` (panik-
/// isoliert), danach [`render_pdf_text`] und UTF-8-sichere Kappung auf
/// `max_bytes`, damit der Cache-Eintrag dieselbe Größengrenze einhält wie ein
/// Text-Körper.
///
/// # Arguments
/// - `bytes` (`&[u8]`): der (byte-gekappte) PDF-Körper.
/// - `url` (`&str`): Endziel (Kopfzeile).
/// - `host` (`&str`): antwortender Host (nur Meldung).
/// - `max_bytes` (`usize`): Obergrenze des gespeicherten Texts.
///
/// # Returns
/// Den extrahierten Text.
///
/// # Errors
/// - [`WebToolError::UnexpectedContentType`]: das PDF ließ sich nicht lesen
///   (beschädigt, verschlüsselt, keine PDF-Signatur); die Meldung nennt die
///   Ursache und verweist auf `doc.read_pdf` mit OCR. Eine Ablehnung, kein
///   Transportfehler — also nie fail-open.
///
/// # Concurrency
/// Blockierend; über [`run_blocking`] aufrufen.
pub fn extract_pdf_text(
    bytes: &[u8],
    url: &str,
    host: &str,
    max_bytes: usize,
) -> WebToolResult<String> {
    let failure = |detail: String| WebToolError::UnexpectedContentType {
        content_type: format!(
            "{PDF_MEDIA_TYPE}: Textextraktion fehlgeschlagen ({detail}); {PDF_OCR_HINT}"
        ),
        host: host.to_owned(),
    };
    if !is_pdf_magic(bytes) {
        return Err(failure("Signatur '%PDF-' fehlt".to_owned()));
    }
    let document = extract_pages(bytes, None).map_err(|error| failure(error.to_string()))?;
    let rendered = render_pdf_text(&document, url);
    let (kept, _) = truncate_utf8(&rendered, max_bytes);
    Ok(kept.to_owned())
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
    /// Domain-Freigaben des offenen Recherche-Netzes für Redirect-Ziele (nur
    /// `web.fetch`); `None` prüft Weiterleitungen nur per [`check_hop`].
    redirect_approvals: Option<&'static OpenWebAccess>,
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
            .field("redirect_approvals", &self.redirect_approvals.is_some())
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
            redirect_approvals: None,
        })
    }

    /// Leitet eine Kopie mit Sandbox-Scope, Cache-Scope und optionalem Limit ab.
    ///
    /// # Description
    /// Client und Policy werden geteilt. `max_bytes` kann das Limit nur senken.
    /// Gebundene Redirect-Freigaben ([`Self::with_redirect_approvals`]) gehen
    /// auf die Kopie über.
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
            .map_or(self.options.max_bytes, |requested| {
                requested.min(self.options.max_bytes)
            })
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
            redirect_approvals: self.redirect_approvals,
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
        check_hop(
            &self.policy,
            &self.network,
            self.options.allow_http,
            url,
            hop,
        )
    }

    /// Bindet die Domain-Freigaben des offenen Recherche-Netzes an die
    /// Weiterleitungen dieses Fetchers.
    ///
    /// # Description
    /// Danach muss jedes Redirect-Ziel (Hop ≥ 1) nach [`Self::check_target`]
    /// auch [`OpenWebAccess::admit_redirect`] bestehen, im Netz wie bei der
    /// Validierung eines Cache-Treffers. Die Start-URL prüft `web.fetch` selbst
    /// ([`OpenWebAccess::admit_fetch`]). Nur `web.fetch` bindet Freigaben;
    /// `web.docs_rs`, `web.crates_io` und `web.search` sprechen feste Hosts an.
    ///
    /// # Concurrency
    /// `OpenWebAccess` ist `Send + Sync`; der Fetcher bleibt es.
    #[must_use]
    pub fn with_redirect_approvals(mut self, access: &'static OpenWebAccess) -> Self {
        self.redirect_approvals = Some(access);
        self
    }

    /// Domain-Freigabe eines bereits per [`Self::check_target`] geprüften
    /// Redirect-Ziels; Hop `0` und Fetcher ohne Freigaben bestehen immer.
    fn check_redirect_approval(&self, target: &HopTarget, hop: usize) -> WebToolResult<()> {
        if hop == 0 {
            return Ok(());
        }
        let Some(access) = self.redirect_approvals else {
            return Ok(());
        };
        access
            .admit_redirect(target.url())
            .map_err(|reason| WebToolError::EgressDenied { reason })
    }

    /// Prüft, ob ein gelesener Cache-Eintrag verwendet werden darf.
    ///
    /// # Description
    /// Formale Übereinstimmung ([`entry_matches`]), Kettenlänge höchstens
    /// [`MAX_REDIRECTS`]` + 1`, **jede** URL der Kette besteht erneut
    /// [`Self::check_target`] in unveränderter Normalform, jedes Redirect-Ziel
    /// bei gebundenen Freigaben ([`Self::with_redirect_approvals`]) außerdem
    /// die Domain-Freigabe, der Medientyp steht noch auf der Positivliste
    /// (Text-Typen oder [`PDF_MEDIA_TYPE`] mit extrahiertem Text) und der
    /// Körper passt ins aktuelle Byte-Limit.
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
    /// - [`WebToolError::EgressDenied`]: ein Redirect-Ziel der Kette ohne
    ///   Domain-Freigabe (nur mit [`Self::with_redirect_approvals`]).
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
            self.check_redirect_approval(&checked, hop)?;
            last_host = checked.host().to_owned();
        }
        check_cached_content_type(&entry.content_type, &last_host)?;
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
        run_blocking("web-render", move || {
            finish_document(source, format, max_output)
        })
        .await
    }

    /// Holt den Rohkörper einer Ressource (Cache, Conditional-GET, Redirects).
    ///
    /// # Description
    /// Ablauf: Start-URL prüfen → Schlüssel → Cache lesen und **validieren**
    /// ([`Self::validate_cached`]) → frischer Treffer sofort → sonst
    /// Conditional-GET unter [`TOTAL_DEADLINE_SECS`] → `304` erneuert den
    /// Zeitstempel, `2xx` schreibt den Eintrag. Bei Transportfehlern Fail-open
    /// nur auf den validierten Eintrag. `body` ist **nicht** textgekappt; der
    /// Aufrufer kappt, bevor er ihn an das Modell gibt. PDF-Körper werden vor
    /// dem Schreiben auf dem Blocking-Pool zu Text extrahiert
    /// ([`extract_pdf_text`]); Eintrag und Ergebnis tragen dann
    /// [`PDF_MEDIA_TYPE`] und den extrahierten Text, nie die Rohbytes.
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
    ///   Start- oder Redirect-Ziel abgelehnt; [`WebToolError::EgressDenied`]
    ///   auch für ein Redirect-Ziel ohne Domain-Freigabe
    ///   ([`Self::with_redirect_approvals`]).
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
                let body = if content_type == PDF_MEDIA_TYPE {
                    let final_url = chain.last().cloned().unwrap_or_default();
                    let host = start.host().to_owned();
                    let limit = self.options.max_bytes;
                    run_blocking("web-pdf-extract", move || {
                        extract_pdf_text(&bytes, &final_url, &host, limit)
                    })
                    .await?
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                };
                let entry = CacheEntry {
                    format_version: CACHE_FORMAT_VERSION,
                    key: key_hex,
                    chain,
                    status,
                    etag,
                    fetched_at: now_secs(),
                    content_type,
                    body,
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
        let result = run_blocking("web-cache-write", move || {
            write_cache_entry(&base, &path, &entry)
        })
        .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "web.cache.write_failed");
        }
    }

    /// Führt die Redirect-Kette und liest den Körper unter dem Byte-Limit.
    async fn fetch_chain(
        &self,
        start: &HopTarget,
        etag: Option<&str>,
    ) -> WebToolResult<RawOutcome> {
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
                // Offenes Recherche-Netz (nur `web.fetch`): ein Redirect-Ziel
                // braucht eine eigene Domain-Freigabe, vor dem Senden.
                self.check_redirect_approval(&next, next_hop)?;
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
            let class = classify_content_type(&raw_content_type, current.host())?;

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

            // `octet-stream` ist nur als PDF zulässig: die Signatur wird geprüft,
            // sobald genug Bytes vorliegen, damit Fremdbinärdaten nicht bis zum
            // Limit gelesen werden.
            let mut sniff_pending = class == ContentClass::SniffPdf;
            let reject_sniffed = || WebToolError::UnexpectedContentType {
                content_type: "application/octet-stream".to_owned(),
                host: current.host().to_owned(),
            };
            let mut response = response;
            let mut buffer: Vec<u8> = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| map_send_error(error, hop))?
            {
                push_chunk(&mut buffer, &chunk, limit, current.host())?;
                if sniff_pending && buffer.len() >= PDF_MAGIC.len() {
                    if !is_pdf_magic(&buffer) {
                        return Err(reject_sniffed());
                    }
                    sniff_pending = false;
                }
            }
            if sniff_pending {
                // Körper kürzer als die Signatur.
                return Err(reject_sniffed());
            }

            let content_type = match class {
                ContentClass::Text(media) => media,
                ContentClass::Pdf | ContentClass::SniffPdf => PDF_MEDIA_TYPE.to_owned(),
            };

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
    description = "Lädt eine HTTPS-Ressource und liefert Text oder Markdown; PDFs werden als Text extrahiert.",
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

    // Plan R9 (offenes Recherche-Netz): eine noch nicht freigegebene Domain
    // läuft nur mit dem vor dem Dispatch vermerkten, genehmigten Aufruf
    // (`crate::open_web`); im Modus `allowlist` ändert das nichts. Jedes
    // Redirect-Ziel braucht ebenfalls eine freigegebene Domain
    // (`WebFetcher::with_redirect_approvals`).
    if let Err(message) = crate::open_web::global().admit_fetch(&args.url) {
        return Ok(ToolOutput::error(message));
    }

    let fetcher = match scoped_fetcher(context, args.max_bytes) {
        Ok(fetcher) => fetcher.with_redirect_approvals(crate::open_web::global()),
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
    use crate::open_web::{NOT_APPROVED_PREFIX, OpenWebSettings};
    use crate::test_support::{TestError, TestResult, ctx};
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
        fn spawn(responses: Vec<String>) -> TestResult<Self> {
            let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("Loopback-Bind"))?;
            let base = format!("http://{}", listener.local_addr().map_err(ctx("Adresse"))?);
            listener.set_nonblocking(true).map_err(ctx("nonblocking"))?;
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
            Ok(Self { base, handle })
        }

        /// Wartet auf den Server-Thread; ein Panik im Thread wird als
        /// [`TestError::Context`] zurückgegeben statt weiterzureichen
        /// (`JoinError`-Payload ist `Box<dyn Any + Send>`, nicht `Display`).
        fn requests(self) -> TestResult<Vec<String>> {
            self.handle.join().map_err(|_| TestError::Context {
                context: "Server-Thread",
                source: "der Server-Thread ist paniert".to_owned(),
            })
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
    fn fetcher_for(
        dir: &Path,
        hosts: &[&str],
        allow_private: bool,
        options: WebFetchOptions,
    ) -> TestResult<WebFetcher> {
        let owned: Vec<String> = hosts.iter().map(|host| (*host).to_owned()).collect();
        let policy =
            Arc::new(EgressPolicy::new(owned.clone(), allow_private).map_err(ctx("Policy"))?);
        let network = NetworkScope::from_hosts(owned);
        let scope = CacheScope::new("tenant", "workspace", &network);
        Ok(WebFetcher::new(policy, dir.to_path_buf(), options)
            .map_err(ctx("Client baut"))?
            .scoped(network, scope, None))
    }

    fn entry_for(
        fetcher: &WebFetcher,
        chain: Vec<String>,
        body: &str,
        fetched_at: u64,
    ) -> (PathBuf, CacheEntry) {
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

    /// Ein eigener, geleakter Freigabezustand im offenen Modus je Test (der
    /// Fetcher hält `&'static`, wie der prozessweite); nie `global()`.
    fn open_approvals() -> &'static OpenWebAccess {
        let access: &'static OpenWebAccess = Box::leak(Box::default());
        access.install(OpenWebSettings {
            open: true,
            allowlisted: Vec::new(),
        });
        access
    }

    // --- Content-Type und Byte-Limit ----------------------------------------

    /// Text- und JSON-artige Typen sind erlaubt, Parameter werden verworfen.
    #[test]
    fn test_check_content_type_accepts_text_and_json() -> TestResult {
        assert_eq!(
            check_content_type("text/html; charset=utf-8", "docs.rs").map_err(ctx("erlaubt"))?,
            "text/html"
        );
        assert_eq!(
            check_content_type("application/vnd.api+json", "crates.io").map_err(ctx("erlaubt"))?,
            "application/vnd.api+json"
        );
        Ok(())
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
    fn test_push_chunk_stops_at_limit_and_keeps_buffer_intact() -> TestResult {
        let mut buffer = Vec::new();
        assert!(push_chunk(&mut buffer, b"aaaa", 10, "docs.rs").is_ok());
        assert!(push_chunk(&mut buffer, b"bbbb", 10, "docs.rs").is_ok());
        let Err(err) = push_chunk(&mut buffer, b"cccc", 10, "docs.rs") else {
            return Err(TestError::Unexpected("Err erwartet: Limit".into()));
        };
        assert!(matches!(
            err,
            WebToolError::ResponseTooLarge { limit: 10, .. }
        ));
        assert_eq!(buffer.len(), 8);
        assert!(push_chunk(&mut Vec::new(), b"x", 0, "docs.rs").is_err());
        Ok(())
    }

    // --- Aufbereitung -------------------------------------------------------

    /// `raw` lässt HTML unangetastet; Nicht-HTML bleibt in allen Formen gleich.
    #[test]
    fn test_render_raw_and_non_html_pass_through() -> TestResult {
        let html = "<p>x</p><script>y</script>";
        assert_eq!(
            render(html, "text/html", OutputFormat::Raw).map_err(ctx("raw"))?,
            html
        );
        let json = r#"{"a":"<script>"}"#;
        for format in [
            OutputFormat::Text,
            OutputFormat::Markdown,
            OutputFormat::Raw,
        ] {
            assert_eq!(
                render(json, "application/json", format).map_err(ctx("json"))?,
                json
            );
        }
        Ok(())
    }

    /// Text-Form entfernt script/style.
    #[test]
    fn test_render_text_strips_script_and_style() -> TestResult {
        let html = "<body><style>p{}</style><p>sichtbar</p><script>unsichtbar</script></body>";
        assert_eq!(
            render(html, "text/html", OutputFormat::Text).map_err(ctx("text"))?,
            "sichtbar"
        );
        Ok(())
    }

    /// `finish_document` kappt nach der Aufbereitung an einer Zeichengrenze.
    #[test]
    fn test_finish_document_truncates_on_multibyte_boundary() -> TestResult {
        let raw = FetchedDocument {
            url: "https://docs.rs/".to_owned(),
            status: 200,
            content_type: "text/html".to_owned(),
            body: "<body><script>xxxxxxxx</script><p>ääää</p></body>".to_owned(),
            from_cache: false,
            etag: None,
            truncated: false,
        };
        let done = finish_document(raw, OutputFormat::Text, 5).map_err(ctx("fertig"))?;
        assert_eq!(done.body, "ää");
        assert!(done.truncated);
        Ok(())
    }

    /// Die Formatwahl akzeptiert die dokumentierten Werte und lehnt andere ab.
    #[test]
    fn test_output_format_parse_known_and_unknown_values() {
        assert_eq!(OutputFormat::parse(Some("  ")), Some(OutputFormat::Text));
        assert_eq!(OutputFormat::parse(Some("RAW")), Some(OutputFormat::Raw));
        assert_eq!(
            OutputFormat::parse(Some("md")),
            Some(OutputFormat::Markdown)
        );
        assert_eq!(OutputFormat::parse(Some("pdf")), None);
    }

    // --- Konfiguration ------------------------------------------------------

    /// Ohne installierten Fetcher gibt es keinen Default (fail-closed).
    #[test]
    fn test_shared_fetcher_without_configuration_is_not_configured() {
        // Kein Test dieses Crates installiert einen Fetcher.
        assert!(matches!(
            shared_fetcher(),
            Err(WebToolError::NotConfigured { .. })
        ));
    }

    /// Limits werden gedeckelt; `scoped` kann nur senken.
    #[test]
    fn test_scoped_caps_requested_max_bytes() -> TestResult {
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let policy =
            Arc::new(EgressPolicy::new(vec!["docs.rs".to_owned()], false).map_err(ctx("Policy"))?);
        let options = WebFetchOptions {
            max_bytes: usize::MAX,
            max_output_bytes: 0,
            ..WebFetchOptions::default()
        };
        let base =
            WebFetcher::new(policy, dir.path().to_path_buf(), options).map_err(ctx("Client"))?;
        assert_eq!(base.max_bytes(), HARD_MAX_BYTES);
        assert_eq!(base.options().max_output_bytes, 1);

        let scope = CacheScope::unbound();
        assert_eq!(
            base.scoped(NetworkScope::empty(), scope, Some(usize::MAX))
                .max_bytes(),
            HARD_MAX_BYTES
        );
        assert_eq!(
            base.scoped(NetworkScope::empty(), scope, Some(2_048))
                .max_bytes(),
            2_048
        );
        assert_eq!(
            base.scoped(NetworkScope::empty(), scope, Some(0))
                .max_bytes(),
            1
        );
        Ok(())
    }

    /// Ohne Sandbox-Scope ist kein Host erreichbar, auch wenn die Policy ihn erlaubt.
    #[tokio::test]
    async fn test_fetch_without_scope_denies_every_host() -> TestResult {
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let policy =
            Arc::new(EgressPolicy::new(vec!["docs.rs".to_owned()], false).map_err(ctx("Policy"))?);
        let fetcher = WebFetcher::new(policy, dir.path().to_path_buf(), WebFetchOptions::default())
            .map_err(ctx("Client"))?;
        let Err(err) = fetcher
            .fetch("https://docs.rs/serde/", OutputFormat::Text)
            .await
        else {
            return Err(TestError::Unexpected("Err erwartet: leerer Scope".into()));
        };
        assert!(
            matches!(err, WebToolError::RedirectHostNotAllowed { .. }),
            "{err:?}"
        );
        Ok(())
    }

    /// IP-Literal-URL wird vor jedem Netzkontakt abgelehnt, ohne Adresse in der Meldung.
    #[tokio::test]
    async fn test_fetch_rejects_ip_literal_url_before_network() -> TestResult {
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(
            dir.path(),
            &["127.0.0.1", "169.254.169.254"],
            false,
            WebFetchOptions::default(),
        )?;
        for url in [
            "https://127.0.0.1/",
            "https://169.254.169.254/latest/meta-data/",
        ] {
            let Err(err) = fetcher.fetch(url, OutputFormat::Raw).await else {
                return Err(TestError::Unexpected(format!("Err erwartet für {url}")));
            };
            assert!(
                matches!(err, WebToolError::EgressDenied { .. }),
                "{url}: {err:?}"
            );
            let message = err.to_string();
            assert!(
                !message.contains("127.0.0.1") && !message.contains("169.254"),
                "{message}"
            );
        }
        Ok(())
    }

    /// `http://` ohne Freigabe scheitert am Schema.
    #[tokio::test]
    async fn test_fetch_rejects_http_without_opt_in() -> TestResult {
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(dir.path(), &["docs.rs"], false, WebFetchOptions::default())?;
        let Err(err) = fetcher
            .fetch("http://docs.rs/serde/", OutputFormat::Text)
            .await
        else {
            return Err(TestError::Unexpected("Err erwartet: http".into()));
        };
        assert!(
            matches!(err, WebToolError::SchemeNotAllowed { .. }),
            "{err:?}"
        );
        Ok(())
    }

    // --- Redirects gegen einen Loopback-Server ------------------------------

    /// F-035: Redirect auf den Metadaten-Endpunkt wird vor dem Senden abgelehnt.
    #[tokio::test]
    async fn test_fetch_redirect_to_metadata_ip_is_rejected() -> TestResult {
        let server = TestServer::spawn(vec![http_response(
            "302 Found",
            &[("Location", "http://169.254.169.254/latest/meta-data/")],
            "",
        )])?;
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(
            dir.path(),
            &["127.0.0.1", "169.254.169.254"],
            true,
            loopback_options(),
        )?;

        let Err(err) = fetcher
            .fetch(&format!("{}/start", server.base), OutputFormat::Raw)
            .await
        else {
            return Err(TestError::Unexpected(
                "Err erwartet: Metadaten-Redirect".into(),
            ));
        };
        assert!(matches!(err, WebToolError::EgressDenied { .. }), "{err:?}");
        assert!(!err.to_string().contains("169.254"), "{err}");
        assert_eq!(
            server.requests()?.len(),
            1,
            "nur die Start-Anfrage darf rausgehen"
        );
        Ok(())
    }

    /// Redirect auf einen Host außerhalb der Policy wird abgelehnt.
    #[tokio::test]
    async fn test_fetch_redirect_to_host_outside_policy_is_rejected() -> TestResult {
        let server = TestServer::spawn(vec![http_response(
            "301 Moved Permanently",
            &[("Location", "https://evil.test/steal")],
            "",
        )])?;
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options())?;

        let Err(err) = fetcher
            .fetch(&format!("{}/start", server.base), OutputFormat::Raw)
            .await
        else {
            return Err(TestError::Unexpected("Err erwartet: fremder Host".into()));
        };
        assert!(matches!(err, WebToolError::EgressDenied { .. }), "{err:?}");
        assert!(!err.to_string().contains("steal"), "{err}");
        assert_eq!(server.requests()?.len(), 1);
        Ok(())
    }

    /// Offenes Recherche-Netz: ein Redirect auf eine noch nicht freigegebene
    /// Domain wird vor dem Senden abgelehnt, obwohl Policy und Scope sie
    /// listen. Die Ablehnung ist kein Transportfehler und nennt nur die
    /// Domain, nie Pfad oder Query.
    #[tokio::test]
    async fn test_fetch_redirect_to_unapproved_open_web_domain_is_rejected_before_sending()
    -> TestResult {
        let server = TestServer::spawn(vec![http_response(
            "302 Found",
            &[("Location", "https://exfil.example.org/leak?q=geheim")],
            "",
        )])?;
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(
            dir.path(),
            &["127.0.0.1", "exfil.example.org"],
            true,
            loopback_options(),
        )?
        .with_redirect_approvals(open_approvals());

        let Err(err) = fetcher
            .fetch(&format!("{}/start", server.base), OutputFormat::Raw)
            .await
        else {
            return Err(TestError::Unexpected(
                "Err erwartet: Redirect ohne Domain-Freigabe".into(),
            ));
        };
        assert!(matches!(err, WebToolError::EgressDenied { .. }), "{err:?}");
        assert!(!err.is_transport(), "{err:?}");
        let message = err.to_string();
        assert!(message.contains(NOT_APPROVED_PREFIX), "{message}");
        assert!(message.contains("exfil.example.org"), "{message}");
        assert!(
            !message.contains("leak") && !message.contains("geheim"),
            "{message}"
        );
        assert_eq!(
            server.requests()?.len(),
            1,
            "das Redirect-Ziel darf nie gesendet werden"
        );
        Ok(())
    }

    /// Die Freigabeprüfung greift nur für Redirect-Ziele (Hop ≥ 1) mit
    /// öffentlichem DNS-Namen an einem Fetcher mit Freigaben; eine
    /// freigegebene (Eltern-)Domain besteht.
    #[test]
    fn test_redirect_approval_gate_skips_start_non_public_ungated_and_granted_targets()
    -> TestResult {
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let hosts = ["127.0.0.1", "exfil.example.org"];
        let access = open_approvals();
        let gated = fetcher_for(dir.path(), &hosts, true, loopback_options())?
            .with_redirect_approvals(access);
        let loopback = gated
            .check_target("http://127.0.0.1:9/ziel", 1)
            .map_err(ctx("Loopback-Ziel"))?;
        let foreign = gated
            .check_target("https://exfil.example.org/x", 1)
            .map_err(ctx("fremdes Ziel"))?;

        assert!(gated.check_redirect_approval(&loopback, 1).is_ok());
        assert!(gated.check_redirect_approval(&foreign, 0).is_ok());
        assert!(matches!(
            gated.check_redirect_approval(&foreign, 1),
            Err(WebToolError::EgressDenied { .. })
        ));

        let ungated = fetcher_for(dir.path(), &hosts, true, loopback_options())?;
        assert!(ungated.check_redirect_approval(&foreign, 1).is_ok());

        access.grant("example.org");
        assert!(gated.check_redirect_approval(&foreign, 1).is_ok());
        Ok(())
    }

    /// Mehr als `MAX_REDIRECTS` Weiterleitungen brechen ab.
    #[tokio::test]
    async fn test_fetch_redirect_chain_beyond_limit_is_rejected() -> TestResult {
        let responses = (0..=MAX_REDIRECTS)
            .map(|_| http_response("302 Found", &[("Location", "/loop")], ""))
            .collect();
        let server = TestServer::spawn(responses)?;
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options())?;

        let Err(err) = fetcher
            .fetch(&format!("{}/start", server.base), OutputFormat::Raw)
            .await
        else {
            return Err(TestError::Unexpected("Err erwartet: Kette zu lang".into()));
        };
        assert!(
            matches!(err, WebToolError::TooManyRedirects { .. }),
            "{err:?}"
        );
        assert_eq!(server.requests()?.len(), MAX_REDIRECTS + 1);
        Ok(())
    }

    /// Erfolgreicher Abruf über einen erlaubten Redirect: Kette im Cache,
    /// Text bereinigt und UTF-8-sicher gekappt.
    #[tokio::test]
    async fn test_fetch_follows_allowed_redirect_and_caps_text() -> TestResult {
        let server = TestServer::spawn(vec![
            http_response("302 Found", &[("Location", "/ziel")], ""),
            http_response(
                "200 OK",
                &[("Content-Type", "text/html; charset=utf-8")],
                "<html><body><script>GEHEIM</script><p>ääää</p></body></html>",
            ),
        ])?;
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let options = WebFetchOptions {
            max_output_bytes: 5,
            ..loopback_options()
        };
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, options)?;
        let start = format!("{}/start", server.base);
        let target = format!("{}/ziel", server.base);

        let document = fetcher
            .fetch(&start, OutputFormat::Text)
            .await
            .map_err(ctx("Abruf"))?;
        assert_eq!(document.url, target);
        assert_eq!(document.body, "ää");
        assert!(document.truncated);
        assert!(!document.from_cache);
        assert_eq!(server.requests()?.len(), 2);

        // Zweiter Abruf: frischer, validierter Cache-Treffer ohne Netz; `raw`
        // liefert den gespeicherten Rohkörper, gekappt auf 5 Bytes.
        let again = fetcher
            .fetch(&start, OutputFormat::Raw)
            .await
            .map_err(ctx("Cache"))?;
        assert!(again.from_cache);
        assert_eq!(again.url, target);
        assert_eq!(again.body, "<html");
        assert!(again.truncated);
        Ok(())
    }

    // --- Cache-Validierung --------------------------------------------------

    /// F-035: ein frischer Treffer, dessen Endziel nun verboten ist, wird
    /// verworfen und neu geholt.
    #[tokio::test]
    async fn test_fetch_discards_cache_hit_with_forbidden_final_target() -> TestResult {
        let server = TestServer::spawn(vec![http_response(
            "200 OK",
            &[("Content-Type", "text/plain")],
            "frisch",
        )])?;
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options())?;
        let start = fetcher
            .check_target(&format!("{}/doc", server.base), 0)
            .map_err(ctx("Start erlaubt"))?;
        let (path, poisoned) = entry_for(
            &fetcher,
            vec![start.url().to_owned(), "https://evil.test/doc".to_owned()],
            "vergiftet",
            now_secs(),
        );
        write_cache_entry(fetcher.cache_dir(), &path, &poisoned).map_err(ctx("Cache schreiben"))?;

        let document = fetcher
            .fetch(start.url(), OutputFormat::Raw)
            .await
            .map_err(ctx("Abruf"))?;
        assert!(
            !document.from_cache,
            "der vergiftete Eintrag darf nicht bedienen"
        );
        assert_eq!(document.body, "frisch");
        assert_eq!(server.requests()?.len(), 1);
        Ok(())
    }

    /// Ein verworfener Eintrag rettet auch keinen Transportfehler (kein Fail-open).
    #[tokio::test]
    async fn test_fetch_stale_fallback_never_uses_forbidden_entry() -> TestResult {
        let closed = TcpListener::bind("127.0.0.1:0").map_err(ctx("Bind"))?;
        let base = format!("http://{}", closed.local_addr().map_err(ctx("Adresse"))?);
        drop(closed);
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options())?;
        let start = fetcher
            .check_target(&format!("{base}/doc"), 0)
            .map_err(ctx("Start"))?;
        let (path, poisoned) = entry_for(
            &fetcher,
            vec![
                start.url().to_owned(),
                "http://10.0.0.1.nip.io/x".to_owned(),
            ],
            "vergiftet",
            0,
        );
        write_cache_entry(fetcher.cache_dir(), &path, &poisoned).map_err(ctx("Cache schreiben"))?;

        let Err(err) = fetcher.fetch(start.url(), OutputFormat::Raw).await else {
            return Err(TestError::Unexpected("Err erwartet: kein Fail-open".into()));
        };
        assert!(err.is_transport(), "{err:?}");
        Ok(())
    }

    /// Ein gültiger frischer Eintrag wird ohne Netz bedient.
    #[tokio::test]
    async fn test_fetch_serves_valid_fresh_cache_entry_without_network() -> TestResult {
        let closed = TcpListener::bind("127.0.0.1:0").map_err(ctx("Bind"))?;
        let base = format!("http://{}", closed.local_addr().map_err(ctx("Adresse"))?);
        drop(closed);
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options())?;
        let start = fetcher
            .check_target(&format!("{base}/doc"), 0)
            .map_err(ctx("Start"))?;
        let (path, entry) = entry_for(
            &fetcher,
            vec![start.url().to_owned()],
            "gecacht",
            now_secs(),
        );
        write_cache_entry(fetcher.cache_dir(), &path, &entry).map_err(ctx("Cache schreiben"))?;

        let document = fetcher
            .fetch(start.url(), OutputFormat::Raw)
            .await
            .map_err(ctx("Cache"))?;
        assert!(document.from_cache);
        assert_eq!(document.body, "gecacht");
        assert_eq!(document.etag.as_deref(), Some("\"v1\""));
        Ok(())
    }

    /// Ein Eintrag unter anderer Policy ist unter der neuen Policy unsichtbar.
    #[test]
    fn test_validate_cached_rejects_entry_from_other_policy_key() -> TestResult {
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let narrow = fetcher_for(dir.path(), &["docs.rs"], false, WebFetchOptions::default())?;
        let wide = fetcher_for(
            dir.path(),
            &["docs.rs", "evil.test"],
            false,
            WebFetchOptions::default(),
        )?;
        let url = "https://docs.rs/";
        let (_, entry) = entry_for(&wide, vec![url.to_owned()], "x", now_secs());
        let narrow_key = to_hex(&cache_key(
            &narrow.policy().digest(),
            &narrow.cache_scope(),
            url,
        ));

        assert!(matches!(
            narrow.validate_cached(&entry, &narrow_key, url),
            Err(WebToolError::CacheCorrupt { .. })
        ));
        assert!(wide.validate_cached(&entry, &entry.key, url).is_ok());
        Ok(())
    }

    /// Offenes Recherche-Netz: ein Treffer, dessen Kette über eine noch nicht
    /// freigegebene Domain führt, wird abgelehnt (Ablehnung, also auch kein
    /// Fail-open); ohne Freigaben oder nach der Freigabe ist er gültig.
    #[test]
    fn test_validate_cached_rejects_chain_through_unapproved_open_web_domain() -> TestResult {
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let hosts = ["127.0.0.1", "exfil.example.org"];
        let access = open_approvals();
        let gated = fetcher_for(dir.path(), &hosts, true, loopback_options())?
            .with_redirect_approvals(access);
        let start = gated
            .check_target("http://127.0.0.1:9/doc", 0)
            .map_err(ctx("Start"))?;
        let (_, entry) = entry_for(
            &gated,
            vec![
                start.url().to_owned(),
                "https://exfil.example.org/x".to_owned(),
            ],
            "x",
            now_secs(),
        );

        assert!(matches!(
            gated.validate_cached(&entry, &entry.key, start.url()),
            Err(WebToolError::EgressDenied { .. })
        ));
        let ungated = fetcher_for(dir.path(), &hosts, true, loopback_options())?;
        assert!(
            ungated
                .validate_cached(&entry, &entry.key, start.url())
                .is_ok()
        );

        access.grant("exfil.example.org");
        assert!(
            gated
                .validate_cached(&entry, &entry.key, start.url())
                .is_ok()
        );
        Ok(())
    }

    /// Conditional-GET: `304` erneuert den Zeitstempel und bedient den Eintrag.
    #[tokio::test]
    async fn test_fetch_conditional_get_refreshes_cache_on_304() -> TestResult {
        let server = TestServer::spawn(vec![http_response("304 Not Modified", &[], "")])?;
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options())?;
        let start = fetcher
            .check_target(&format!("{}/doc", server.base), 0)
            .map_err(ctx("Start"))?;
        let (path, stale) = entry_for(&fetcher, vec![start.url().to_owned()], "alt", 1);
        write_cache_entry(fetcher.cache_dir(), &path, &stale).map_err(ctx("Cache schreiben"))?;

        let document = fetcher
            .fetch(start.url(), OutputFormat::Raw)
            .await
            .map_err(ctx("304"))?;
        assert_eq!(document.status, 304);
        assert!(document.from_cache);
        assert_eq!(document.body, "alt");

        let requests = server.requests()?;
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("if-none-match: \"v1\""),
            "{}",
            requests[0]
        );
        let refreshed = read_cache_entry(&path)
            .map_err(ctx("lesen"))?
            .ok_or(TestError::Missing("vorhanden"))?;
        assert!(refreshed.fetched_at > 1);
        Ok(())
    }

    // --- PDF ----------------------------------------------------------------

    /// Baut ein minimales, gültiges einseitiges PDF (Helvetica, ein `Tj`)
    /// mit korrekt berechneter Xref-Tabelle.
    fn minimal_pdf(text: &str) -> String {
        let content = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", index + 1));
        }
        let xref_at = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        pdf
    }

    /// PDF-Typen werden erkannt, `octet-stream` nur zum Sniffen zugelassen,
    /// Text bleibt Text, andere Binärtypen werden abgelehnt.
    #[test]
    fn test_classify_content_type_pdf_octet_stream_text_and_binary() -> TestResult {
        assert_eq!(
            classify_content_type("application/pdf", "a.test").map_err(ctx("pdf"))?,
            ContentClass::Pdf
        );
        assert_eq!(
            classify_content_type("Application/X-PDF; name=x.pdf", "a.test")
                .map_err(ctx("x-pdf"))?,
            ContentClass::Pdf
        );
        assert_eq!(
            classify_content_type("application/octet-stream", "a.test").map_err(ctx("octet"))?,
            ContentClass::SniffPdf
        );
        assert_eq!(
            classify_content_type("text/html; charset=utf-8", "a.test").map_err(ctx("html"))?,
            ContentClass::Text("text/html".to_owned())
        );
        for rejected in ["image/png", "application/zip", ""] {
            assert!(
                matches!(
                    classify_content_type(rejected, "a.test"),
                    Err(WebToolError::UnexpectedContentType { .. })
                ),
                "{rejected}"
            );
        }
        // `check_content_type` (auch von `web.search` genutzt) bleibt Text-only.
        assert!(check_content_type("application/pdf", "a.test").is_err());
        Ok(())
    }

    /// Die Signaturprüfung erkennt `%PDF-` und nichts sonst.
    #[test]
    fn test_is_pdf_magic_sniffs_signature() {
        assert!(is_pdf_magic(minimal_pdf("x").as_bytes()));
        assert!(!is_pdf_magic(b"%PD"));
        assert!(!is_pdf_magic(b"\x89PNG\r\n"));
        assert!(!is_pdf_magic(b" %PDF-1.4"));
    }

    /// Cache-Einträge mit PDF-Medientyp sind zulässig, `octet-stream` nicht.
    #[test]
    fn test_check_cached_content_type_allows_pdf_text_entries() {
        assert!(check_cached_content_type(PDF_MEDIA_TYPE, "a.test").is_ok());
        assert!(check_cached_content_type("text/plain", "a.test").is_ok());
        assert!(check_cached_content_type("application/octet-stream", "a.test").is_err());
    }

    /// Kopfzeile und Seitenabschnitte; leere Seiten führen zum OCR-Hinweis.
    #[test]
    fn test_render_pdf_text_header_sections_and_ocr_hint() {
        let document = ExtractedDocument {
            total_pages: Some(2),
            pages: vec![
                harw_tool_doc::types::ExtractedPage {
                    number: 1,
                    text: "  Erste Seite \n".to_owned(),
                },
                harw_tool_doc::types::ExtractedPage {
                    number: 2,
                    text: "Zweite".to_owned(),
                },
            ],
            backend: harw_tool_doc::types::Backend::Native,
        };
        let rendered = render_pdf_text(&document, "https://a.test/x.pdf");
        assert_eq!(
            rendered,
            "PDF · 2 Seiten · https://a.test/x.pdf\n\n## Seite 1\n\nErste Seite\n\n## Seite 2\n\nZweite\n"
        );

        let scanned = ExtractedDocument {
            total_pages: Some(1),
            pages: vec![harw_tool_doc::types::ExtractedPage {
                number: 1,
                text: " \n".to_owned(),
            }],
            backend: harw_tool_doc::types::Backend::Native,
        };
        let rendered = render_pdf_text(&scanned, "https://a.test/s.pdf");
        assert!(rendered.starts_with("PDF · 1 Seiten · https://a.test/s.pdf\n"));
        assert!(rendered.contains("doc.read_pdf"), "{rendered}");
    }

    /// Ein echtes Mini-PDF wird extrahiert und gerendert.
    #[test]
    fn test_extract_pdf_text_from_minimal_pdf() -> TestResult {
        let pdf = minimal_pdf("Hallo PDF");
        let text = extract_pdf_text(pdf.as_bytes(), "https://a.test/d.pdf", "a.test", 4_096)
            .map_err(ctx("Extraktion"))?;
        assert!(
            text.starts_with("PDF · 1 Seiten · https://a.test/d.pdf\n"),
            "{text}"
        );
        assert!(text.contains("## Seite 1"), "{text}");
        assert!(text.contains("Hallo"), "{text}");

        // Kappung auf das Byte-Limit, UTF-8-sicher ("·" ist zwei Bytes).
        let capped = extract_pdf_text(pdf.as_bytes(), "https://a.test/d.pdf", "a.test", 5)
            .map_err(ctx("gekappt"))?;
        assert_eq!(capped, "PDF ");
        Ok(())
    }

    /// Kaputte PDFs liefern einen klaren Fehler mit OCR-Hinweis, keine Panik.
    #[test]
    fn test_extract_pdf_text_failure_is_clear_error() {
        for bytes in [&b"%PDF-1.4\nkaputt"[..], &b"kein pdf"[..], &b""[..]] {
            let Err(err) = extract_pdf_text(bytes, "https://a.test/k.pdf", "a.test", 4_096) else {
                continue;
            };
            assert!(!err.is_transport());
            let message = err.to_string();
            assert!(message.contains("doc.read_pdf"), "{message}");
            assert!(message.contains("a.test"), "{message}");
        }
        assert!(extract_pdf_text(b"kein pdf", "https://a.test/", "a.test", 64).is_err());
    }

    /// Ende-zu-Ende über Loopback: `octet-stream` mit PDF-Signatur wird
    /// extrahiert; der Cache enthält den Text, nicht die Rohbytes, und ein
    /// zweiter Abruf wird (erneut validiert) aus dem Cache bedient.
    #[tokio::test]
    async fn test_fetch_pdf_octet_stream_extracts_and_caches_text() -> TestResult {
        let pdf = minimal_pdf("Hallo PDF");
        let server = TestServer::spawn(vec![http_response(
            "200 OK",
            &[("Content-Type", "application/octet-stream")],
            &pdf,
        )])?;
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options())?;
        let start = fetcher
            .check_target(&format!("{}/d.pdf", server.base), 0)
            .map_err(ctx("Start"))?;

        let document = fetcher
            .fetch(start.url(), OutputFormat::Text)
            .await
            .map_err(ctx("Abruf"))?;
        assert_eq!(document.content_type, PDF_MEDIA_TYPE);
        assert!(!document.from_cache);
        assert!(
            document
                .body
                .starts_with(&format!("PDF · 1 Seiten · {}\n", start.url())),
            "{}",
            document.body
        );
        assert!(document.body.contains("Hallo"), "{}", document.body);
        assert_eq!(server.requests()?.len(), 1);

        let key = cache_key(
            &fetcher.policy().digest(),
            &fetcher.cache_scope(),
            start.url(),
        );
        let stored = read_cache_entry(&cache_path(fetcher.cache_dir(), &key))
            .map_err(ctx("lesen"))?
            .ok_or(TestError::Missing("Cache-Eintrag"))?;
        assert_eq!(stored.content_type, PDF_MEDIA_TYPE);
        assert!(!stored.body.contains("%PDF"), "Rohbytes im Cache");
        assert!(stored.body.starts_with("PDF · "));

        let again = fetcher
            .fetch(start.url(), OutputFormat::Raw)
            .await
            .map_err(ctx("Cache"))?;
        assert!(again.from_cache);
        assert_eq!(again.body, document.body);
        Ok(())
    }

    /// `octet-stream` ohne PDF-Signatur wird abgelehnt und nicht gecacht.
    #[tokio::test]
    async fn test_fetch_octet_stream_without_pdf_magic_is_rejected() -> TestResult {
        let server = TestServer::spawn(vec![http_response(
            "200 OK",
            &[("Content-Type", "application/octet-stream")],
            "PK\u{3}\u{4}binaer",
        )])?;
        let dir = TempDir::new().map_err(ctx("Tempdir"))?;
        let fetcher = fetcher_for(dir.path(), &["127.0.0.1"], true, loopback_options())?;
        let start = fetcher
            .check_target(&format!("{}/x.bin", server.base), 0)
            .map_err(ctx("Start"))?;

        let Err(err) = fetcher.fetch(start.url(), OutputFormat::Text).await else {
            return Err(TestError::Unexpected("Err erwartet: kein PDF".into()));
        };
        assert!(
            matches!(err, WebToolError::UnexpectedContentType { .. }),
            "{err:?}"
        );
        server.requests()?;
        let key = cache_key(
            &fetcher.policy().digest(),
            &fetcher.cache_scope(),
            start.url(),
        );
        assert!(
            read_cache_entry(&cache_path(fetcher.cache_dir(), &key))
                .map_err(ctx("lesen"))?
                .is_none()
        );
        Ok(())
    }

    // --- Tool-Deklaration ---------------------------------------------------

    /// Das Tool deklariert Berechtigung wie zugesagt.
    #[test]
    fn test_web_fetch_tool_declares_network_permission() {
        assert_eq!(WebFetchTool::NAME, "web.fetch");
        assert_eq!(
            WebFetchTool::PERMISSION,
            Some(harw_tools::Permission::NetworkAccess)
        );
    }

    // `PARALLEL_SAFE` ist eine makro-generierte `const bool`; die Assertion
    // bricht den Build, sobald `web.fetch` nicht mehr parallelsicher ist.
    const _: () = assert!(WebFetchTool::PARALLEL_SAFE);

    /// Das Schema bewirbt `url` als Pflichtfeld und kennt die Optionen.
    #[test]
    fn test_fetch_args_schema_requires_url_only() -> TestResult {
        let harw_tools::ToolSpec::Function(spec) = WebFetchTool::spec();
        assert_eq!(spec.name.as_str(), "web.fetch");
        let required = spec
            .parameters
            .required
            .ok_or(TestError::Missing("required-Liste"))?;
        assert_eq!(required, vec!["url".to_owned()]);
        let properties = spec
            .parameters
            .properties
            .ok_or(TestError::Missing("properties"))?;
        assert!(properties.contains_key("format"));
        assert!(properties.contains_key("max_bytes"));
        Ok(())
    }
}
