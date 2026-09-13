//! `web.fetch` — HTTPS-Abruf mit Cache, Byte-Limit und Redirect-Kontrolle.
//!
//! # Verantwortung
//! Dieses Modul besitzt die **einzige** Netz-Ausgangstür des Crates:
//! [`WebFetcher`] baut den `reqwest::Client`, führt die Redirect-Kette von
//! Hand, erzwingt Schema-, Host-, Content-Type- und Byte-Grenzen und verwaltet
//! den Antwort-Cache. [`crate::docs_rs`] und [`crate::crates_io`] bauen nur
//! URLs und formatieren Ergebnisse; sie sprechen nie selbst mit dem Netz.
//!
//! # Schlüsseltypen
//! - [`WebFetcher`] — der Abruf-Motor mit Client, Cache-Verzeichnis, TTL,
//!   Byte-Limit und Host-Allowlist.
//! - [`FetchedDocument`] — das Ergebnis eines Abrufs.
//! - [`CacheEntry`] — das on-disk-Format eines Cache-Eintrags.
//! - [`OutputFormat`] — `text` / `markdown` / `raw`.
//! - [`FetchArgs`] / `WebFetchTool` — Argument-Struktur und generierter
//!   `ToolExecutor` des Tools `web.fetch`.
//!
//! # Sicherheitskontrakt
//! 1. **Permission und Host über das Makro.** `#[harw_macros::tool(permission =
//!    "network_access", host_from = "url")]` erzeugt den `require_permission`-
//!    Prolog *vor* der Deserialisierung und den `require_host_access`-Prolog
//!    danach. Ein nicht extrahierbarer Host bricht dort fail-closed ab.
//! 2. **Nur `https://`.** [`validate_target`] lehnt jedes andere Schema ab,
//!    bevor eine Verbindung entsteht; zusätzlich ist der Client mit
//!    `https_only(true)` gebaut.
//! 3. **Jeder Redirect-Hop wird erneut geprüft.** Der Client folgt Redirects
//!    *nicht* selbst (`redirect::Policy::none()`); [`WebFetcher`] führt die
//!    Kette mit maximal [`MAX_REDIRECTS`] Hops und prüft bei jedem Hop erneut
//!    Schema, Host-Extraktion und [`harw_sandbox::NetworkScope`].
//! 4. **Limit beim Lesen.** Der Körper wird chunk-weise akkumuliert; sobald
//!    das Limit überschritten würde, bricht der Stream ab
//!    ([`WebToolError::ResponseTooLarge`]) — die Bytes werden nicht erst
//!    vollständig gepuffert.
//! 5. **Content-Type-Positivliste.** Nur Text-, JSON- und XML-artige Typen
//!    werden angenommen; ein fehlender Content-Type gilt als Ablehnung.
//! 6. **Fail-closed ohne Scope.** [`WebFetcher::with_defaults`] setzt
//!    [`harw_sandbox::NetworkScope::empty`]; ohne einen per
//!    [`WebFetcher::scoped`] übernommenen Sandbox-Scope ist kein einziger Host
//!    erreichbar.
//! 7. **Die Host-Prüfung bleibt namensgebunden, nicht adressgebunden.**
//!    [`WebFetcher::fetch`] und die Redirect-Kette prüfen jeden Hop
//!    ausschließlich über [`harw_sandbox::NetworkScope::allows`] gegen den
//!    Hostnamen. Die tatsächlich verbundene Adresse
//!    (`reqwest::Response::remote_addr`) wäre technisch erreichbar, wird aber
//!    bewusst *nicht* gegen [`harw_sandbox::NetworkScope::allows_addr`]
//!    geprüft: ein per [`harw_sandbox::NetworkScope::from_hosts`] gebauter
//!    Scope — der einzige, den dieses Tool je erhält — trägt ausschließlich
//!    [`harw_sandbox::EgressTarget::DnsSuffix`]-Ziele, nie
//!    [`harw_sandbox::EgressTarget::Cidr`]-Ziele. `allows_addr` würde für
//!    jede einzelne Anfrage `false` liefern und den Abruf fälschlich
//!    verwerfen, nicht ihn sicherer machen — die beiden Prüfungen sind
//!    getrennte Achsen (siehe [`harw_sandbox::EgressTarget`]), keine
//!    austauschbaren Varianten derselben Prüfung. Eine nach der Namensprüfung
//!    geänderte Auflösung (DNS-Rebinding) bleibt deshalb unentdeckt; ein
//!    echter Schutz dagegen bräuchte einen Resolver, der die Adresse *vor*
//!    dem Verbindungsaufbau gegen eine für den jeweiligen Host modellierte
//!    Grenze validiert — eine Erweiterung, die [`NetworkScope`] heute nicht
//!    ausdrückt und die dieses Modul nicht im Alleingang einführt.
//!
//! # Nebenläufigkeit
//! [`WebFetcher`] ist `Send + Sync` und über `Arc` teilbar; `reqwest::Client`
//! ist intern refcounted, [`WebFetcher::scoped`] teilt ihn deshalb ohne neue
//! Verbindungspools. Der Cache wird atomar (Temp-Datei + Rename) geschrieben;
//! es gibt keine prozessweite Sperre — konkurrierende Schreiber überschreiben
//! sich höchstens gegenseitig mit gleichwertigen Daten.
//!
//! # Fehler
//! Alle Varianten aus [`WebToolError`]. Netz-Transportfehler fallen fail-open
//! auf einen veralteten Cache-Eintrag zurück; Ablehnungen aus
//! Sicherheitsgründen tun das nie (siehe [`WebToolError::is_transport`]).
//!
//! # Examples
//! ```rust,no_run
//! use harw_tool_web::fetch::{OutputFormat, WebFetcher};
//! use harw_sandbox::NetworkScope;
//!
//! # async fn demo() -> Result<(), harw_tool_web::WebToolError> {
//! let base = WebFetcher::with_defaults()?;
//! let scoped = base.scoped(NetworkScope::from_hosts(["docs.rs".to_owned()]), None);
//! let doc = scoped.fetch("https://docs.rs/serde/latest/serde/", OutputFormat::Markdown).await?;
//! println!("{}", doc.body);
//! # Ok(())
//! # }
//! ```

use crate::error::{WebToolError, WebToolResult};
use harw_macros::Tool;
use harw_sandbox::{EgressUrl, EgressUrlError, NetworkScope};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Standard-Byte-Obergrenze einer Antwort (1 MiB).
pub const DEFAULT_MAX_BYTES: usize = 1_048_576;

/// Harte Obergrenze: kein Aufruf darf mehr anfordern, egal was in den
/// Argumenten steht (8 MiB).
pub const HARD_MAX_BYTES: usize = 8 * 1_048_576;

/// Standard-Cache-TTL in Sekunden.
pub const DEFAULT_TTL_SECS: u64 = 3_600;

/// Timeout einer einzelnen HTTP-Anfrage.
pub const REQUEST_TIMEOUT_SECS: u64 = 20;

/// Gesamt-Deadline über alle Redirect-Hops eines Abrufs.
pub const TOTAL_DEADLINE_SECS: u64 = 45;

/// Maximale Anzahl gefolgter Weiterleitungen (jeder Hop wird erneut geprüft).
pub const MAX_REDIRECTS: usize = 3;

/// Unterverzeichnis des Cache-Verzeichnisses, in dem Antworten landen.
const CACHE_SUBDIR: &str = "web";

/// User-Agent aller ausgehenden Anfragen. crates.io lehnt Anfragen ohne
/// aussagekräftigen User-Agent ab.
const USER_AGENT: &str = concat!("harwness-web-tools/", env!("CARGO_PKG_VERSION"));

/// Medientypen, die zusätzlich zu `text/*` und den `+json`/`+xml`-Suffixen
/// angenommen werden.
const ALLOWED_MEDIA_TYPES: &[&str] = &[
    "application/json",
    "application/xml",
    "application/xhtml+xml",
    "application/javascript",
];

/// Prozesslokale Sequenz für kollisionsfreie Cache-Temporärdateien.
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Der prozessweit geteilte Basis-Fetcher (ohne Host-Scope).
static SHARED_FETCHER: OnceLock<Arc<WebFetcher>> = OnceLock::new();

/// Hex-Alphabet für die Cache-Dateinamen.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

// ---------------------------------------------------------------------------
// Argument-Struktur und Ausgabeform
// ---------------------------------------------------------------------------

/// Argumente des Tools `web.fetch`.
///
/// # Description
/// Wird vom Modell geliefert und ist damit **untrusted**. Die
/// Sicherheitsprüfungen laufen im Makro-Prolog (Permission, Host) und in
/// [`WebFetcher`] (Schema, Redirects, Limits, Content-Type).
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
    /// Byte-Obergrenze der Antwort (Default aus Konfiguration, hart gedeckelt).
    #[tool(default = 1048576)]
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

/// Ausgabeform eines abgerufenen Dokuments.
///
/// # Description
/// `Raw` liefert den Körper unverändert, `Text` zieht bei HTML den sichtbaren
/// Text heraus, `Markdown` konvertiert HTML nach Markdown. Für Nicht-HTML-
/// Antworten sind alle drei Formen identisch mit `Raw`.
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
    /// - `value` (`Option<&str>`): der Wert des Feldes `format`; `None` und ein
    ///   leerer String bedeuten [`OutputFormat::Text`].
    ///
    /// # Returns
    /// `Some(OutputFormat)` für bekannte Werte, `None` für alles andere. Der
    /// Aufrufer meldet `None` als Tool-Fehler, statt still auf einen Default
    /// zu fallen.
    ///
    /// # Concurrency
    /// Reine Funktion; von jedem Thread aufrufbar.
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

// ---------------------------------------------------------------------------
// Ergebnis- und Cache-Typen
// ---------------------------------------------------------------------------

/// Ein abgerufenes (oder aus dem Cache bedientes) Dokument.
///
/// # Description
/// `body` ist bereits gemäß [`OutputFormat`] aufbereitet; der Rohkörper liegt
/// nur im Cache. `status` ist `200` für Cache-Treffer, `304` nach einem
/// erfolgreichen Conditional-GET.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FetchedDocument {
    /// Die tatsächlich abgerufene URL (nach allen Weiterleitungen).
    pub url: String,
    /// Der HTTP-Statuscode der Antwort.
    pub status: u16,
    /// Der Medientyp ohne Parameter, z. B. `text/html`.
    pub content_type: String,
    /// Der aufbereitete Körper.
    pub body: String,
    /// Ob der Körper aus dem Cache stammt.
    pub from_cache: bool,
    /// Der `ETag` der Antwort, falls der Server einen geliefert hat.
    pub etag: Option<String>,
}

/// On-disk-Format eines Cache-Eintrags.
///
/// # Description
/// Gespeichert wird der **Rohkörper**, nicht die aufbereitete Ausgabe: derselbe
/// Eintrag bedient sonst `format = "text"` und `format = "markdown"` nicht
/// gleichermaßen korrekt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    /// Die URL, zu der dieser Eintrag gehört (nach Weiterleitungen).
    pub url: String,
    /// Der `ETag` für den nächsten Conditional-GET.
    pub etag: Option<String>,
    /// Unix-Zeitstempel des Abrufs in Sekunden.
    pub fetched_at: u64,
    /// Der Medientyp ohne Parameter.
    pub content_type: String,
    /// Der unveränderte Antwort-Körper.
    pub body: String,
}

/// Rohergebnis eines Netzabrufs, bevor Cache und Formatierung greifen.
#[derive(Debug)]
enum RawOutcome {
    /// Der Server meldete `304 Not Modified`.
    NotModified,
    /// Der Server lieferte einen Körper.
    Body {
        status: u16,
        final_url: String,
        content_type: String,
        etag: Option<String>,
        bytes: Vec<u8>,
    },
}

// ---------------------------------------------------------------------------
// Reine Prüf- und Hilfsfunktionen (ohne Netz, ohne Dateisystem)
// ---------------------------------------------------------------------------

/// Prüft Schema und Host-Extrahierbarkeit einer Ziel-URL.
///
/// # Description
/// Erste und strengste Prüfung der Ausgangstür: nur `https://` ist erlaubt
/// (Vergleich ASCII-case-insensitiv, damit `HTTPS://` nicht durchfällt), und
/// der Host muss sich mit [`harw_sandbox::egress::EgressUrl::parse`] bestimmen
/// lassen — demselben WHATWG-Parser, den `reqwest` beim Verbindungsaufbau
/// verwendet. Eine URL mit Userinfo (`user:pw@host`) wird abgelehnt statt die
/// Zugangsdaten stillschweigend abzuschneiden.
///
/// # Arguments
/// - `url` (`&str`): die zu prüfende URL, mit oder ohne umgebende Leerzeichen.
///
/// # Returns
/// Den kleingeschriebenen Hostnamen ohne Port; URLs mit Userinfo werden
/// abgelehnt statt die Zugangsdaten abzuschneiden.
///
/// # Errors
/// - [`WebToolError::SchemeNotAllowed`]: das Schema ist nicht `https://`.
/// - [`WebToolError::HostNotResolvable`]: aus der URL ließ sich kein Host lesen
///   (auch bei Userinfo, fehlendem Host oder einem für den Parser ungültigen
///   Host).
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_tool_web::fetch::validate_target;
///
/// assert_eq!(validate_target("https://docs.rs/serde/").unwrap(), "docs.rs");
/// assert!(validate_target("http://docs.rs/").is_err());
/// ```
pub fn validate_target(url: &str) -> WebToolResult<String> {
    let trimmed = url.trim();
    let parsed = EgressUrl::parse(trimmed).map_err(|error| match error {
        EgressUrlError::UnsupportedScheme(_) => WebToolError::SchemeNotAllowed {
            url: trimmed.to_owned(),
        },
        EgressUrlError::Parse
        | EgressUrlError::UserinfoPresent
        | EgressUrlError::MissingHost
        | EgressUrlError::InvalidHost => WebToolError::HostNotResolvable {
            url: trimmed.to_owned(),
        },
    })?;

    if !parsed.is_https() {
        return Err(WebToolError::SchemeNotAllowed {
            url: trimmed.to_owned(),
        });
    }

    Ok(parsed.host_str())
}

/// Prüft den `Content-Type` einer Antwort gegen die Positivliste.
///
/// # Description
/// Verglichen wird nur der Medientyp vor dem ersten `;` (Parameter wie
/// `charset=utf-8` werden verworfen), ASCII-kleingeschrieben. Angenommen
/// werden `text/*`, die Einträge aus der internen Liste sowie strukturierte
/// Suffixe `+json` und `+xml`. Ein fehlender oder leerer Content-Type gilt als
/// Ablehnung — der Körper wird als `String` weitergereicht, ein unbekanntes
/// Binärformat hat darin nichts zu suchen.
///
/// # Arguments
/// - `raw` (`&str`): der rohe Header-Wert, ggf. leer.
/// - `host` (`&str`): der antwortende Host, nur für die Fehlermeldung.
///
/// # Returns
/// Den normalisierten Medientyp ohne Parameter.
///
/// # Errors
/// - [`WebToolError::UnexpectedContentType`]: der Typ steht nicht auf der Liste.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_tool_web::fetch::check_content_type;
///
/// assert_eq!(
///     check_content_type("text/html; charset=utf-8", "docs.rs").unwrap(),
///     "text/html"
/// );
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
/// Das Byte-Limit wird **beim Lesen** durchgesetzt: der Aufrufer bricht den
/// Stream ab, sobald diese Funktion `Err` liefert, statt erst die vollständige
/// Antwort zu puffern. Die Addition ist sättigend, damit ein manipuliertes
/// `Content-Length` keinen Überlauf erzeugen kann.
///
/// # Arguments
/// - `buffer` (`&mut Vec<u8>`): der Akkumulator, wird bei Erfolg verlängert.
/// - `chunk` (`&[u8]`): der neu gelesene Abschnitt.
/// - `limit` (`usize`): die Obergrenze in Bytes für den gesamten Puffer.
/// - `host` (`&str`): der antwortende Host, nur für die Fehlermeldung.
///
/// # Returns
/// `Ok(())`, wenn der Chunk vollständig übernommen wurde.
///
/// # Errors
/// - [`WebToolError::ResponseTooLarge`]: der Chunk würde das Limit sprengen.
///   Der Puffer bleibt in diesem Fall unverändert.
///
/// # Concurrency
/// Reine Funktion auf dem übergebenen Puffer; kein geteilter Zustand.
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

/// Zieht den sichtbaren Text aus einem HTML-Dokument.
///
/// # Description
/// Verwendet `scraper`, wählt `body` und fällt auf das Wurzelelement zurück,
/// wenn kein `body` vorhanden ist. Aufeinanderfolgende Leerzeichen werden zu
/// einem einzelnen zusammengefasst, Leerzeilen bleiben als Absatztrenner
/// erhalten.
///
/// # Arguments
/// - `html` (`&str`): das Quelldokument.
///
/// # Returns
/// Den extrahierten Text. Nie ein Fehler: ein unparsebares Dokument liefert
/// höchstens einen leeren String.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_tool_web::fetch::html_to_text;
///
/// let text = html_to_text("<html><body><p>Hallo</p></body></html>");
/// assert!(text.contains("Hallo"));
/// ```
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let document = scraper::Html::parse_document(html);

    let raw = match scraper::Selector::parse("body") {
        Ok(selector) => match document.select(&selector).next() {
            Some(body) => body.text().collect::<Vec<_>>().join(" "),
            None => document.root_element().text().collect::<Vec<_>>().join(" "),
        },
        // Ein statischer Selektor kann nicht scheitern; der Zweig existiert nur,
        // um ohne `unwrap()` auszukommen.
        Err(_) => document.root_element().text().collect::<Vec<_>>().join(" "),
    };

    collapse_whitespace(&raw)
}

/// Konvertiert HTML nach Markdown.
///
/// # Description
/// **Einziger Berührungspunkt mit der Crate `htmd`.** Sollte sich deren API
/// ändern, ist nur diese Funktion anzupassen.
///
/// # Arguments
/// - `html` (`&str`): das Quelldokument oder ein Fragment.
///
/// # Returns
/// Das erzeugte Markdown.
///
/// # Errors
/// - [`WebToolError::Io`]: `htmd` meldet einen Schreibfehler beim Aufbau des
///   Ergebnisses.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_web::fetch::html_to_markdown;
///
/// let md = html_to_markdown("<h1>Titel</h1>").unwrap_or_default();
/// assert!(md.contains("Titel"));
/// ```
pub fn html_to_markdown(html: &str) -> WebToolResult<String> {
    // TODO(cargo add): API von `htmd::convert` nach dem Auflösen der Version
    // gegen docs.rs verifizieren (erwartet: `fn convert(&str) -> Result<String, std::io::Error>`).
    htmd::convert(html).map_err(WebToolError::from)
}

/// Fasst Whitespace-Folgen zusammen und erhält Absatzgrenzen.
fn collapse_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_space = false;
    let mut newlines = 0usize;

    for ch in input.chars() {
        if ch == '\n' {
            newlines += 1;
            pending_space = false;
            continue;
        }
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if !out.is_empty() {
            if newlines >= 2 {
                out.push_str("\n\n");
            } else if newlines == 1 || pending_space {
                out.push(' ');
            }
        }
        newlines = 0;
        pending_space = false;
        out.push(ch);
    }

    out
}

/// Bereitet einen Rohkörper gemäß Ausgabeform auf.
///
/// # Description
/// Nicht-HTML-Antworten werden in allen drei Formen unverändert
/// durchgereicht — eine JSON-Antwort durch einen HTML-Parser zu schicken wäre
/// verlustbehaftet.
///
/// # Arguments
/// - `raw` (`&str`): der unveränderte Antwort-Körper.
/// - `content_type` (`&str`): der normalisierte Medientyp.
/// - `format` ([`OutputFormat`]): die gewünschte Ausgabeform.
///
/// # Returns
/// Den aufbereiteten Körper.
///
/// # Errors
/// - [`WebToolError::Io`]: die Markdown-Konvertierung schlug fehl.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_tool_web::fetch::{render, OutputFormat};
///
/// let raw = render("<p>x</p>", "text/html", OutputFormat::Raw).unwrap();
/// assert_eq!(raw, "<p>x</p>");
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

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Aktuelle Unix-Zeit in Sekunden; vor der Epoche `0`.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|delta| delta.as_secs())
        .unwrap_or(0)
}

/// SHA-256 einer Zeichenkette als Kleinbuchstaben-Hex.
fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();

    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX_DIGITS[usize::from(byte >> 4)] as char);
        out.push(HEX_DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// Das Unterverzeichnis, in dem Antworten liegen.
fn cache_subdir(base: &Path) -> PathBuf {
    base.join(CACHE_SUBDIR)
}

/// Der Cache-Pfad einer URL.
///
/// # Description
/// `<cache_dir>/web/<sha256(url)>.json`. Der Hash macht den Dateinamen
/// unabhängig von Pfad-, Query- und Längenbeschränkungen des Dateisystems und
/// verhindert, dass eine modell-kontrollierte URL zu einem Pfad-Traversal wird.
///
/// # Arguments
/// - `base` (`&Path`): das Basis-Cache-Verzeichnis.
/// - `url` (`&str`): die URL, die als Cache-Schlüssel dient.
///
/// # Returns
/// Den vollständigen Pfad des Cache-Eintrags.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use std::path::Path;
/// use harw_tool_web::fetch::cache_path;
///
/// let path = cache_path(Path::new("/tmp/c"), "https://docs.rs/");
/// assert!(path.starts_with("/tmp/c/web"));
/// assert_eq!(path.extension().and_then(|e| e.to_str()), Some("json"));
/// ```
#[must_use]
pub fn cache_path(base: &Path, url: &str) -> PathBuf {
    cache_subdir(base).join(format!("{}.json", sha256_hex(url)))
}

/// Weist ein Cache-Artefakt zurück, das ein Symlink ist.
///
/// Schutz gegen TOCTOU: ein untergeschobener Symlink würde sonst einen
/// beliebigen Pfad außerhalb des Cache-Verzeichnisses lesen oder überschreiben.
fn reject_symlink(path: &Path) -> WebToolResult<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(WebToolError::Io(io::Error::new(
            ErrorKind::InvalidInput,
            "symlinked web cache artifact",
        ))),
        Ok(_) => Ok(()),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(WebToolError::Io(source)),
    }
}

/// Liest einen Cache-Eintrag, falls vorhanden.
///
/// # Description
/// Ein fehlender Eintrag ist `Ok(None)`. Ein vorhandener, aber unlesbarer oder
/// syntaktisch kaputter Eintrag ist [`WebToolError::CacheCorrupt`] — der
/// Aufrufer behandelt das als Cache-Miss, protokolliert es aber, statt es
/// stillschweigend zu verschlucken.
///
/// # Arguments
/// - `path` (`&Path`): der Pfad des Cache-Eintrags.
///
/// # Returns
/// `Some(CacheEntry)` bei einem gültigen Eintrag, sonst `None`.
///
/// # Errors
/// - [`WebToolError::Io`]: der Pfad ist ein Symlink oder nicht lesbar.
/// - [`WebToolError::CacheCorrupt`]: der Inhalt ist kein gültiger Eintrag.
///
/// # Concurrency
/// Blockierender Datei-Zugriff ohne Sperre; sicher von jedem Thread.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_tool_web::fetch::read_cache_entry;
///
/// let entry = read_cache_entry(Path::new("/tmp/c/web/abc.json")).unwrap_or(None);
/// assert!(entry.is_none() || entry.is_some());
/// ```
pub fn read_cache_entry(path: &Path) -> WebToolResult<Option<CacheEntry>> {
    reject_symlink(path)?;

    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(WebToolError::Io(source)),
    };

    match serde_json::from_str::<CacheEntry>(&raw) {
        Ok(entry) => Ok(Some(entry)),
        Err(_) => Err(WebToolError::CacheCorrupt {
            path: path.display().to_string(),
        }),
    }
}

/// Schreibt einen Cache-Eintrag atomar.
///
/// # Description
/// Serialisiert nach JSON, schreibt in eine frisch angelegte Temp-Datei im
/// selben Verzeichnis (`create_new`, damit kein untergeschobenes Artefakt
/// geöffnet wird) und ersetzt das Ziel per `rename`. Vor und nach dem Schreiben
/// wird der Zielpfad erneut auf Symlinks geprüft.
///
/// # Arguments
/// - `base` (`&Path`): das Basis-Cache-Verzeichnis; wird bei Bedarf angelegt.
/// - `path` (`&Path`): der Zielpfad, üblicherweise aus [`cache_path`].
/// - `entry` (`&CacheEntry`): der zu schreibende Eintrag.
///
/// # Returns
/// `Ok(())`, wenn der Eintrag vollständig ersetzt wurde.
///
/// # Errors
/// - [`WebToolError::Io`]: Verzeichnis, Temp-Datei oder Rename schlugen fehl,
///   oder ein Artefakt war ein Symlink.
/// - [`WebToolError::Json`]: der Eintrag ließ sich nicht serialisieren.
///
/// # Concurrency
/// Ohne Sperre; konkurrierende Schreiber nutzen dank Sequenz-Zähler und
/// `create_new` verschiedene Temp-Dateien und überschreiben am Ende höchstens
/// gleichwertige Daten.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_tool_web::fetch::{cache_path, write_cache_entry, CacheEntry};
///
/// let base = Path::new("/tmp/c");
/// let entry = CacheEntry {
///     url: "https://docs.rs/".to_owned(),
///     etag: None,
///     fetched_at: 0,
///     content_type: "text/html".to_owned(),
///     body: "<html></html>".to_owned(),
/// };
/// write_cache_entry(base, &cache_path(base, &entry.url), &entry).ok();
/// ```
pub fn write_cache_entry(base: &Path, path: &Path, entry: &CacheEntry) -> WebToolResult<()> {
    let directory = cache_subdir(base);
    fs::create_dir_all(&directory)?;
    reject_symlink(path)?;

    let payload = serde_json::to_vec(entry)?;
    let (temp_path, mut temp_file) = create_temp_file(&directory)?;
    temp_file.write_all(&payload)?;
    drop(temp_file);

    // Zwischen Anlegen und Rename könnte das Ziel durch einen Symlink ersetzt
    // worden sein — erneut prüfen, bevor ersetzt wird.
    if let Err(err) = reject_symlink(path) {
        fs::remove_file(&temp_path).ok();
        return Err(err);
    }

    fs::rename(&temp_path, path).map_err(WebToolError::from)
}

/// Legt eine frische Temp-Datei im Cache-Verzeichnis an.
fn create_temp_file(directory: &Path) -> WebToolResult<(PathBuf, fs::File)> {
    for _ in 0..128 {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = directory.join(format!(".harw-web.{}.{sequence}.tmp", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(source) if source.kind() == ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(WebToolError::Io(source)),
        }
    }

    Err(WebToolError::Io(io::Error::new(
        ErrorKind::AlreadyExists,
        "no unique web cache temporary file could be created",
    )))
}

/// Sagt, ob ein Eintrag jünger als die TTL ist.
///
/// # Description
/// Ein Zeitstempel in der Zukunft (Uhr-Sprung) gilt als frisch, statt eine
/// Endlosschleife aus Neuabrufen auszulösen.
///
/// # Arguments
/// - `entry` (`&CacheEntry`): der zu bewertende Eintrag.
/// - `ttl` ([`Duration`]): die zulässige Lebensdauer.
///
/// # Returns
/// `true`, wenn der Eintrag ohne Netzabruf verwendet werden darf.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use std::time::Duration;
/// use harw_tool_web::fetch::{entry_is_fresh, CacheEntry};
///
/// let entry = CacheEntry {
///     url: "https://docs.rs/".to_owned(),
///     etag: None,
///     fetched_at: 0,
///     content_type: "text/html".to_owned(),
///     body: String::new(),
/// };
/// assert!(!entry_is_fresh(&entry, Duration::from_secs(1)));
/// ```
#[must_use]
pub fn entry_is_fresh(entry: &CacheEntry, ttl: Duration) -> bool {
    now_secs().saturating_sub(entry.fetched_at) < ttl.as_secs()
}

// ---------------------------------------------------------------------------
// WebFetcher
// ---------------------------------------------------------------------------

/// Der Abruf-Motor: Client, Cache, Limits und Host-Allowlist.
///
/// # Description
/// Ein `WebFetcher` ohne Host-Scope erreicht nichts —
/// [`NetworkScope::empty`] ist die Voreinstellung. Der Aufrufer leitet mit
/// [`Self::scoped`] pro Tool-Aufruf eine Kopie ab, die den
/// [`harw_sandbox::NetworkScope`] der aktiven Sandbox trägt. Damit gilt die
/// Allowlist auch für jeden Redirect-Hop, nicht nur für die vom Modell
/// gelieferte Start-URL.
///
/// # Concurrency
/// `Send + Sync`. `reqwest::Client` ist intern refcounted; [`Self::scoped`]
/// klont nur den Handle und teilt den Verbindungspool.
pub struct WebFetcher {
    /// Der HTTP-Client; folgt Redirects **nicht** selbst.
    client: reqwest::Client,
    /// Basis-Cache-Verzeichnis; Einträge liegen in `<cache_dir>/web/`.
    cache_dir: PathBuf,
    /// Lebensdauer eines Cache-Eintrags vor dem nächsten Conditional-GET.
    ttl: Duration,
    /// Byte-Obergrenze einer Antwort.
    max_bytes: usize,
    /// Erlaubte Hosts; leer bedeutet: kein Host erreichbar.
    allow_hosts: NetworkScope,
}

impl std::fmt::Debug for WebFetcher {
    /// Zeigt Konfiguration ohne den Client (der kein `Debug` mit stabiler
    /// Ausgabe liefert und Verbindungsdetails enthalten könnte).
    ///
    /// Zählt über [`harw_sandbox::NetworkScope::targets`], nicht über
    /// [`harw_sandbox::NetworkScope::hosts`]: Letzteres klappt `Host` und
    /// `DnsSuffix` zusammen und lässt `Cidr`-Ziele ganz weg — eine
    /// Diagnoseausgabe, die Ziele verschweigt, ist schlechter als keine.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebFetcher")
            .field("cache_dir", &self.cache_dir)
            .field("ttl_secs", &self.ttl.as_secs())
            .field("max_bytes", &self.max_bytes)
            .field("allowed_targets", &self.allow_hosts.targets().count())
            .finish()
    }
}

impl WebFetcher {
    /// Baut einen Fetcher mit expliziter Konfiguration und leerem Host-Scope.
    ///
    /// # Arguments
    /// - `cache_dir` (`PathBuf`): Basis-Cache-Verzeichnis; Eigentum geht über.
    /// - `ttl` ([`Duration`]): Cache-Lebensdauer.
    /// - `max_bytes` (`usize`): Byte-Limit; wird auf [`HARD_MAX_BYTES`] gedeckelt.
    ///
    /// # Returns
    /// Einen Fetcher, der ohne [`Self::scoped`] keinen Host erreicht.
    ///
    /// # Errors
    /// - [`WebToolError::Http`]: der `reqwest::Client` ließ sich nicht bauen
    ///   (z. B. weil kein TLS-Backend verfügbar ist).
    ///
    /// # Concurrency
    /// Baut einen eigenen Verbindungspool; pro Prozess sollte genau ein
    /// Basis-Fetcher existieren (siehe [`shared_fetcher`]).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use std::{path::PathBuf, time::Duration};
    /// use harw_tool_web::fetch::WebFetcher;
    ///
    /// let fetcher = WebFetcher::new(PathBuf::from("/tmp/c"), Duration::from_secs(60), 1024)?;
    /// # Ok::<(), harw_tool_web::WebToolError>(())
    /// ```
    pub fn new(cache_dir: PathBuf, ttl: Duration, max_bytes: usize) -> WebToolResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(10))
            // Redirects werden von Hand verfolgt, damit jeder Hop erneut gegen
            // die Host-Allowlist geprüft wird.
            .redirect(reqwest::redirect::Policy::none())
            // Zweite Verteidigungslinie hinter `validate_target`.
            .https_only(true)
            .build()?;

        Ok(Self {
            client,
            cache_dir,
            ttl,
            max_bytes: max_bytes.min(HARD_MAX_BYTES),
            allow_hosts: NetworkScope::empty(),
        })
    }

    /// Baut einen Fetcher mit den Standardwerten des Crates.
    ///
    /// # Description
    /// Das Cache-Verzeichnis stammt aus `HARW_CACHE_DIR`, ersatzweise aus
    /// `$HOME/.harw/cache`, ersatzweise aus dem Temp-Verzeichnis. Der
    /// Host-Scope ist leer.
    ///
    /// # Returns
    /// Einen Fetcher mit [`DEFAULT_TTL_SECS`] und [`DEFAULT_MAX_BYTES`].
    ///
    /// # Errors
    /// - [`WebToolError::Http`]: der Client ließ sich nicht bauen.
    ///
    /// # Concurrency
    /// Siehe [`Self::new`].
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_web::fetch::WebFetcher;
    ///
    /// let fetcher = WebFetcher::with_defaults()?;
    /// # Ok::<(), harw_tool_web::WebToolError>(())
    /// ```
    pub fn with_defaults() -> WebToolResult<Self> {
        Self::new(
            default_cache_dir(),
            Duration::from_secs(DEFAULT_TTL_SECS),
            DEFAULT_MAX_BYTES,
        )
    }

    /// Leitet eine Kopie mit Sandbox-Host-Scope und optionalem Byte-Limit ab.
    ///
    /// # Description
    /// Der teure Teil (Verbindungspool, TLS-Konfiguration) wird geteilt; nur
    /// Scope und Limit unterscheiden sich. Das übergebene Limit wird auf das
    /// Minimum aus Anforderung und [`HARD_MAX_BYTES`] gedeckelt — ein
    /// modell-kontrolliertes `max_bytes` kann das Limit also nur senken.
    ///
    /// # Arguments
    /// - `allow_hosts` ([`NetworkScope`]): der Scope der aktiven Sandbox.
    /// - `max_bytes` (`Option<usize>`): angefordertes Limit; `None` behält das
    ///   Limit dieses Fetchers.
    ///
    /// # Returns
    /// Einen Fetcher, der genau die Hosts aus `allow_hosts` erreicht.
    ///
    /// # Concurrency
    /// `Send + Sync`; teilt den Client-Handle mit `self`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_sandbox::NetworkScope;
    /// use harw_tool_web::fetch::WebFetcher;
    ///
    /// let base = WebFetcher::with_defaults()?;
    /// let scoped = base.scoped(NetworkScope::from_hosts(["docs.rs".to_owned()]), Some(4096));
    /// # Ok::<(), harw_tool_web::WebToolError>(())
    /// ```
    #[must_use]
    pub fn scoped(&self, allow_hosts: NetworkScope, max_bytes: Option<usize>) -> Self {
        let limit = max_bytes.unwrap_or(self.max_bytes).min(HARD_MAX_BYTES);
        Self {
            client: self.client.clone(),
            cache_dir: self.cache_dir.clone(),
            ttl: self.ttl,
            max_bytes: limit.max(1),
            allow_hosts,
        }
    }

    /// Das Basis-Cache-Verzeichnis dieses Fetchers.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Das durchgesetzte Byte-Limit dieses Fetchers.
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Holt eine HTTPS-Ressource, aufbereitet in der gewünschten Ausgabeform.
    ///
    /// # Description
    /// Ablauf: Schema und Host prüfen → Host gegen die Allowlist prüfen →
    /// Cache lesen → bei frischem Eintrag sofort ausliefern → sonst
    /// Conditional-GET (`If-None-Match`, falls ein `ETag` vorliegt) unter einer
    /// Gesamt-Deadline von [`TOTAL_DEADLINE_SECS`] → `304` erneuert nur den
    /// Zeitstempel, `200` schreibt den neuen Rohkörper atomar in den Cache.
    /// Bei einem reinen Transportfehler wird fail-open auf einen vorhandenen
    /// (auch veralteten) Eintrag zurückgefallen; Ablehnungen aus
    /// Sicherheitsgründen tun das nicht.
    ///
    /// # Arguments
    /// - `url` (`&str`): vollständige `https://`-URL.
    /// - `format` ([`OutputFormat`]): gewünschte Aufbereitung des Körpers.
    ///
    /// # Returns
    /// Ein [`FetchedDocument`] mit aufbereitetem Körper.
    ///
    /// # Errors
    /// - [`WebToolError::SchemeNotAllowed`] / [`WebToolError::HostNotResolvable`]:
    ///   die URL ist nicht abrufbar.
    /// - [`WebToolError::RedirectHostNotAllowed`]: Start- oder Redirect-Host
    ///   liegt außerhalb der Allowlist.
    /// - [`WebToolError::TooManyRedirects`], [`WebToolError::ResponseTooLarge`],
    ///   [`WebToolError::UnexpectedContentType`], [`WebToolError::UpstreamStatus`],
    ///   [`WebToolError::UpstreamTimeout`], [`WebToolError::Http`],
    ///   [`WebToolError::Io`], [`WebToolError::Json`].
    ///
    /// # Panics
    /// Nie.
    ///
    /// # Concurrency
    /// Nebenläufig aufrufbar; benötigt einen Tokio-Runtime mit aktivierter
    /// Zeitachse (`time`-Feature) für die Gesamt-Deadline.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_sandbox::NetworkScope;
    /// use harw_tool_web::fetch::{OutputFormat, WebFetcher};
    ///
    /// # async fn demo() -> Result<(), harw_tool_web::WebToolError> {
    /// let fetcher = WebFetcher::with_defaults()?
    ///     .scoped(NetworkScope::from_hosts(["crates.io".to_owned()]), None);
    /// let doc = fetcher.fetch("https://crates.io/api/v1/crates/serde", OutputFormat::Raw).await?;
    /// assert_eq!(doc.status, 200);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn fetch(&self, url: &str, format: OutputFormat) -> WebToolResult<FetchedDocument> {
        let target = url.trim().to_owned();
        let host = validate_target(&target)?;
        if !self.allow_hosts.allows(&host) {
            return Err(WebToolError::RedirectHostNotAllowed { url: target, host });
        }

        let path = cache_path(&self.cache_dir, &target);
        let cached = match read_cache_entry(&path) {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(error = %err, "web.cache.unreadable");
                None
            }
        };

        if let Some(entry) = cached.as_ref() {
            if entry_is_fresh(entry, self.ttl) {
                tracing::debug!(host = %host, "web.fetch.cache_hit");
                return build_document(&target, 200, entry, true, format);
            }
        }

        let etag = cached.as_ref().and_then(|entry| entry.etag.as_deref());
        let deadline = Duration::from_secs(TOTAL_DEADLINE_SECS);
        let outcome = match tokio::time::timeout(deadline, self.fetch_raw(&target, etag)).await {
            Ok(result) => result,
            Err(_) => Err(WebToolError::UpstreamTimeout {
                host: host.clone(),
                seconds: TOTAL_DEADLINE_SECS,
            }),
        };

        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(err) if err.is_transport() => {
                // Fail-open ausschließlich bei Transportfehlern: eine Ablehnung
                // aus Sicherheitsgründen darf nie durch einen Cache-Eintrag
                // umgangen werden.
                let Some(entry) = cached.as_ref() else {
                    return Err(err);
                };
                tracing::warn!(host = %host, error = %err, "web.fetch.stale_cache_fallback");
                return build_document(&target, 200, entry, true, format);
            }
            Err(err) => return Err(err),
        };

        match outcome {
            RawOutcome::NotModified => {
                let Some(entry) = cached else {
                    // 304 ohne Cache-Eintrag: der Server hat auf einen ETag
                    // geantwortet, den wir nicht mehr auflösen können.
                    return Err(WebToolError::UpstreamStatus { status: 304, host });
                };
                let refreshed = CacheEntry {
                    fetched_at: now_secs(),
                    ..entry
                };
                if let Err(err) = write_cache_entry(&self.cache_dir, &path, &refreshed) {
                    tracing::warn!(error = %err, "web.cache.refresh_failed");
                }
                tracing::debug!(host = %host, "web.fetch.not_modified");
                build_document(&target, 304, &refreshed, true, format)
            }
            RawOutcome::Body {
                status,
                final_url,
                content_type,
                etag,
                bytes,
            } => {
                let entry = CacheEntry {
                    url: final_url.clone(),
                    etag,
                    fetched_at: now_secs(),
                    content_type,
                    body: String::from_utf8_lossy(&bytes).into_owned(),
                };
                if let Err(err) = write_cache_entry(&self.cache_dir, &path, &entry) {
                    tracing::warn!(error = %err, "web.cache.write_failed");
                }
                tracing::info!(
                    host = %host,
                    status,
                    bytes = bytes.len(),
                    "web.fetch.done"
                );
                build_document(&final_url, status, &entry, false, format)
            }
        }
    }

    /// Führt die Redirect-Kette und liest den Körper unter dem Byte-Limit.
    ///
    /// Jeder Hop durchläuft erneut [`validate_target`] und die Host-Allowlist;
    /// nach [`MAX_REDIRECTS`] Hops wird abgebrochen.
    async fn fetch_raw(&self, url: &str, etag: Option<&str>) -> WebToolResult<RawOutcome> {
        let mut current = url.trim().to_owned();
        let mut hops = 0usize;

        loop {
            let host = validate_target(&current)?;
            if !self.allow_hosts.allows(&host) {
                return Err(WebToolError::RedirectHostNotAllowed { url: current, host });
            }

            let mut request = self.client.get(&current);
            if hops == 0 {
                if let Some(tag) = etag {
                    request = request.header(reqwest::header::IF_NONE_MATCH, tag);
                }
            }

            let response = request.send().await?;
            let status = response.status();

            // 304 liegt im 3xx-Bereich, ist aber keine Weiterleitung — zuerst prüfen.
            if status == reqwest::StatusCode::NOT_MODIFIED {
                return Ok(RawOutcome::NotModified);
            }

            if status.is_redirection() {
                if hops >= MAX_REDIRECTS {
                    return Err(WebToolError::TooManyRedirects { url: current });
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let Some(location) = location else {
                    return Err(WebToolError::UpstreamStatus {
                        status: status.as_u16(),
                        host,
                    });
                };
                let Ok(base) = reqwest::Url::parse(&current) else {
                    return Err(WebToolError::HostNotResolvable { url: current });
                };
                let Ok(next) = base.join(&location) else {
                    return Err(WebToolError::HostNotResolvable { url: location });
                };
                tracing::debug!(from = %host, hop = hops + 1, "web.fetch.redirect");
                current = next.to_string();
                hops += 1;
                continue;
            }

            if !status.is_success() {
                return Err(WebToolError::UpstreamStatus {
                    status: status.as_u16(),
                    host,
                });
            }

            let raw_content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let content_type = check_content_type(&raw_content_type, &host)?;

            // Billige Vorprüfung: ein glaubwürdiges Content-Length spart den
            // Download komplett. Die verbindliche Prüfung bleibt `push_chunk`.
            if let Some(announced) = response.content_length() {
                if announced > self.max_bytes as u64 {
                    return Err(WebToolError::ResponseTooLarge {
                        limit: self.max_bytes,
                        host,
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
            while let Some(chunk) = response.chunk().await? {
                // Abbruch beim Lesen, nicht erst nach dem vollständigen Puffern.
                push_chunk(&mut buffer, &chunk, self.max_bytes, &host)?;
            }

            return Ok(RawOutcome::Body {
                status: status.as_u16(),
                final_url: current,
                content_type,
                etag: response_etag,
                bytes: buffer,
            });
        }
    }
}

/// Baut aus einem Cache-Eintrag das Ergebnis-Dokument.
fn build_document(
    url: &str,
    status: u16,
    entry: &CacheEntry,
    from_cache: bool,
    format: OutputFormat,
) -> WebToolResult<FetchedDocument> {
    Ok(FetchedDocument {
        url: url.to_owned(),
        status,
        content_type: entry.content_type.clone(),
        body: render(&entry.body, &entry.content_type, format)?,
        from_cache,
        etag: entry.etag.clone(),
    })
}

/// Das Standard-Cache-Verzeichnis dieses Crates.
///
/// Reihenfolge: `HARW_CACHE_DIR`, `$HOME/.harw/cache`, Temp-Verzeichnis.
fn default_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HARW_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".harw").join("cache");
    }
    std::env::temp_dir().join("harw-cache")
}

/// Liefert den prozessweit geteilten Basis-Fetcher.
///
/// # Description
/// Der Fetcher wird beim ersten Aufruf gebaut und danach wiederverwendet, damit
/// alle Tools denselben Verbindungspool und dasselbe Cache-Verzeichnis nutzen.
/// Er trägt **keinen** Host-Scope: Aufrufer müssen [`WebFetcher::scoped`] mit
/// dem Scope der aktiven Sandbox anwenden, sonst ist kein Host erreichbar.
/// [`install_fetcher`] erlaubt es dem Harness, vorher eine eigene
/// Konfiguration zu hinterlegen.
///
/// # Returns
/// Einen `Arc` auf den geteilten Fetcher.
///
/// # Errors
/// - [`WebToolError::Http`]: der `reqwest::Client` ließ sich nicht bauen.
///
/// # Concurrency
/// Thread-sicher über [`OnceLock`]. Bei einem Wettlauf gewinnt der zuerst
/// gesetzte Fetcher; der Verlierer wird verworfen, ohne dass ein Aufruf
/// scheitert.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_web::fetch::shared_fetcher;
///
/// let fetcher = shared_fetcher()?;
/// assert!(fetcher.max_bytes() > 0);
/// # Ok::<(), harw_tool_web::WebToolError>(())
/// ```
pub fn shared_fetcher() -> WebToolResult<Arc<WebFetcher>> {
    if let Some(existing) = SHARED_FETCHER.get() {
        return Ok(Arc::clone(existing));
    }

    let built = Arc::new(WebFetcher::with_defaults()?);
    match SHARED_FETCHER.set(Arc::clone(&built)) {
        Ok(()) => Ok(built),
        // Ein anderer Thread war schneller: dessen Fetcher gilt.
        Err(_) => Ok(Arc::clone(SHARED_FETCHER.get().unwrap_or(&built))),
    }
}

/// Hinterlegt den prozessweit geteilten Basis-Fetcher.
///
/// # Description
/// Nur der erste Aufruf gewinnt; danach bleibt der Fetcher unveränderlich. Das
/// Harness ruft die Funktion beim Start auf, wenn es Cache-Verzeichnis, TTL
/// oder Byte-Limit aus der Konfiguration setzen will.
///
/// # Arguments
/// - `fetcher` (`Arc<WebFetcher>`): der zu hinterlegende Fetcher.
///
/// # Returns
/// `Ok(())`, wenn dieser Aufruf den Fetcher gesetzt hat.
///
/// # Errors
/// Gibt den übergebenen Fetcher als `Err` zurück, wenn bereits einer gesetzt
/// war — der Aufrufer sieht damit, dass seine Konfiguration nicht greift.
///
/// # Concurrency
/// Thread-sicher über [`OnceLock`].
///
/// # Examples
/// ```rust,no_run
/// use std::sync::Arc;
/// use harw_tool_web::fetch::{install_fetcher, WebFetcher};
///
/// let fetcher = Arc::new(WebFetcher::with_defaults()?);
/// install_fetcher(fetcher).ok();
/// # Ok::<(), harw_tool_web::WebToolError>(())
/// ```
pub fn install_fetcher(fetcher: Arc<WebFetcher>) -> Result<(), Arc<WebFetcher>> {
    SHARED_FETCHER.set(fetcher)
}

/// Leitet aus dem Tool-Kontext einen sandbox-gebundenen Fetcher ab.
///
/// # Description
/// Übernimmt den [`NetworkScope`] der aktiven Sandbox, damit die Allowlist
/// nicht nur für die Start-URL (Makro-Prolog), sondern für jeden Redirect-Hop
/// gilt.
///
/// # Arguments
/// - `context` (`&ToolExecutionContext`): die vom Harness etablierte Autorität.
/// - `max_bytes` (`Option<usize>`): vom Modell angefordertes Byte-Limit.
///
/// # Returns
/// Einen Fetcher, der genau die Hosts der Sandbox erreicht.
///
/// # Errors
/// - [`WebToolError::Http`]: der geteilte Client ließ sich nicht bauen.
///
/// # Concurrency
/// Nebenläufig aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_web::fetch::scoped_fetcher;
/// use harw_tools::ToolExecutionContext;
///
/// fn demo(ctx: &ToolExecutionContext) -> Result<(), harw_tool_web::WebToolError> {
///     let fetcher = scoped_fetcher(ctx, None)?;
///     assert!(fetcher.max_bytes() > 0);
///     Ok(())
/// }
/// ```
pub fn scoped_fetcher(
    context: &ToolExecutionContext,
    max_bytes: Option<usize>,
) -> WebToolResult<WebFetcher> {
    let shared = shared_fetcher()?;
    Ok(shared.scoped(context.sandbox().network_scope().clone(), max_bytes))
}

/// Serialisiert ein [`FetchedDocument`] als Tool-Ausgabe.
///
/// # Description
/// Gemeinsame Ausgabeform aller drei Tools, damit der Aufrufer Herkunft
/// (`from_cache`), Status und Medientyp immer an derselben Stelle findet.
///
/// # Arguments
/// - `document` (`&FetchedDocument`): das aufbereitete Dokument.
///
/// # Returns
/// Eine [`ToolOutput::Json`]-Ausgabe.
///
/// # Concurrency
/// Reine Funktion; von jedem Thread aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_web::fetch::{document_output, FetchedDocument};
///
/// let doc = FetchedDocument {
///     url: "https://docs.rs/".to_owned(),
///     status: 200,
///     content_type: "text/html".to_owned(),
///     body: "x".to_owned(),
///     from_cache: false,
///     etag: None,
/// };
/// let _output = document_output(&doc);
/// ```
#[must_use]
pub fn document_output(document: &FetchedDocument) -> ToolOutput {
    ToolOutput::json(serde_json::json!({
        "url": document.url,
        "status": document.status,
        "content_type": document.content_type,
        "from_cache": document.from_cache,
        "etag": document.etag,
        "body": document.body,
    }))
}

// ---------------------------------------------------------------------------
// Tool `web.fetch`
// ---------------------------------------------------------------------------

/// Führt `web.fetch` aus.
///
/// Der Permission-Prolog (`network_access`) und der Host-Prolog (`host_from =
/// "url"`) werden von `#[harw_macros::tool]` erzeugt und laufen vor diesem
/// Rumpf; hier bleiben nur Formatwahl, Scope-Ableitung und Abruf.
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
    use harw_sandbox::EgressTarget;
    use tempfile::TempDir;

    const HTML_SNIPPET: &str = concat!(
        "<html><head><title>T</title></head><body>",
        "<h1>Titel</h1><p>Ein <strong>Absatz</strong> mit Text.</p>",
        "</body></html>"
    );

    fn sample_entry() -> CacheEntry {
        CacheEntry {
            url: "https://docs.rs/serde/latest/serde/".to_owned(),
            etag: Some("\"abc123\"".to_owned()),
            fetched_at: now_secs(),
            content_type: "text/html".to_owned(),
            body: HTML_SNIPPET.to_owned(),
        }
    }

    // --- Schema- und Host-Prüfung ------------------------------------------

    /// `http://` wird abgelehnt, bevor irgendetwas passiert.
    #[test]
    fn test_validate_target_rejects_http_scheme() {
        let err =
            validate_target("http://docs.rs/serde/").expect_err("http:// muss abgelehnt werden");
        assert!(
            matches!(err, WebToolError::SchemeNotAllowed { .. }),
            "unerwartet: {err:?}"
        );
    }

    /// Auch andere Schemata fallen durch dieselbe Prüfung.
    #[test]
    fn test_validate_target_rejects_non_http_schemes() {
        for url in ["file:///etc/passwd", "data:text/html,x", "ftp://docs.rs/"] {
            let err = validate_target(url).expect_err("nur https:// ist erlaubt");
            assert!(
                matches!(err, WebToolError::SchemeNotAllowed { .. }),
                "{url} wurde nicht als Schema-Fehler abgelehnt: {err:?}"
            );
        }
    }

    /// Das Schema wird case-insensitiv verglichen.
    #[test]
    fn test_validate_target_accepts_uppercase_scheme() {
        assert_eq!(
            validate_target("HTTPS://Docs.RS/serde/").expect("HTTPS:// ist erlaubt"),
            "docs.rs"
        );
    }

    /// Fehlender Host bricht fail-closed ab, statt mit leerem Host zu prüfen.
    #[test]
    fn test_validate_target_rejects_url_without_host() {
        let err = validate_target("https://").expect_err("ohne Host muss abgebrochen werden");
        assert!(
            matches!(err, WebToolError::HostNotResolvable { .. }),
            "unerwartet: {err:?}"
        );
    }

    /// Der Port gehört nicht in den Host der Allowlist-Prüfung.
    #[test]
    fn test_validate_target_strips_port_and_lowercases() {
        assert_eq!(
            validate_target("https://Docs.RS:8443/x").expect("gültig"),
            "docs.rs"
        );
    }

    /// Eine URL mit Userinfo wird abgelehnt statt die Zugangsdaten
    /// stillschweigend abzuschneiden (`EgressUrl::parse` ->
    /// `EgressUrlError::UserinfoPresent`, siehe `validate_target`).
    #[test]
    fn test_validate_target_rejects_userinfo() {
        let err = validate_target("https://user:pw@Docs.RS:8443/x").expect_err("Userinfo");
        assert!(
            matches!(err, WebToolError::HostNotResolvable { .. }),
            "unerwartet: {err:?}"
        );
    }

    // --- Content-Type -------------------------------------------------------

    /// Text- und JSON-artige Typen sind erlaubt, Parameter werden verworfen.
    #[test]
    fn test_check_content_type_accepts_text_and_json() {
        assert_eq!(
            check_content_type("text/html; charset=utf-8", "docs.rs").expect("erlaubt"),
            "text/html"
        );
        assert_eq!(
            check_content_type("application/json", "crates.io").expect("erlaubt"),
            "application/json"
        );
        assert_eq!(
            check_content_type("application/vnd.api+json", "crates.io").expect("erlaubt"),
            "application/vnd.api+json"
        );
    }

    /// Binärformate werden abgelehnt.
    #[test]
    fn test_check_content_type_rejects_binary() {
        let err = check_content_type("application/octet-stream", "docs.rs")
            .expect_err("Binärformate sind nicht erlaubt");
        match err {
            WebToolError::UnexpectedContentType { content_type, host } => {
                assert_eq!(content_type, "application/octet-stream");
                assert_eq!(host, "docs.rs");
            }
            other => panic!("unerwartet: {other:?}"),
        }
    }

    /// Ein fehlender Content-Type gilt als Ablehnung (fail-closed).
    #[test]
    fn test_check_content_type_rejects_missing_header() {
        let err = check_content_type("", "docs.rs").expect_err("leerer Header ist keine Erlaubnis");
        assert!(matches!(err, WebToolError::UnexpectedContentType { .. }));
    }

    // --- Byte-Limit ---------------------------------------------------------

    /// Ein simulierter Chunk-Stream bricht exakt an der Grenze ab.
    #[test]
    fn test_push_chunk_stops_at_limit_and_keeps_buffer_intact() {
        let chunks: [&[u8]; 3] = [b"aaaa", b"bbbb", b"cccc"];
        let mut buffer = Vec::new();
        let mut error = None;

        for chunk in chunks {
            if let Err(err) = push_chunk(&mut buffer, chunk, 10, "docs.rs") {
                error = Some(err);
                break;
            }
        }

        let err = error.expect("das Limit von 10 Bytes muss greifen");
        match err {
            WebToolError::ResponseTooLarge { limit, host } => {
                assert_eq!(limit, 10);
                assert_eq!(host, "docs.rs");
            }
            other => panic!("unerwartet: {other:?}"),
        }
        assert_eq!(
            buffer.len(),
            8,
            "der abgelehnte Chunk darf nicht teilweise übernommen werden"
        );
    }

    /// Genau auf dem Limit ist noch erlaubt.
    #[test]
    fn test_push_chunk_accepts_exactly_the_limit() {
        let mut buffer = Vec::new();
        assert!(push_chunk(&mut buffer, b"12345", 5, "docs.rs").is_ok());
        assert_eq!(buffer.len(), 5);
        assert!(push_chunk(&mut buffer, b"6", 5, "docs.rs").is_err());
    }

    /// Ein Limit von 0 lässt keinen einzigen Chunk durch.
    #[test]
    fn test_push_chunk_zero_limit_rejects_any_payload() {
        let mut buffer = Vec::new();
        assert!(push_chunk(&mut buffer, b"x", 0, "docs.rs").is_err());
        assert!(buffer.is_empty());
    }

    // --- HTML-Aufbereitung --------------------------------------------------

    /// Aus HTML wird sichtbarer Text ohne Tags.
    #[test]
    fn test_html_to_text_extracts_visible_text() {
        let text = html_to_text(HTML_SNIPPET);
        assert!(text.contains("Titel"), "unerwartet: {text}");
        assert!(text.contains("Absatz"), "unerwartet: {text}");
        assert!(
            !text.contains('<'),
            "Tags dürfen nicht übrig bleiben: {text}"
        );
    }

    /// HTML → Markdown erhält Überschrift und Hervorhebung.
    #[test]
    fn test_html_to_markdown_converts_snippet() {
        let markdown = html_to_markdown("<h1>Titel</h1><p>Ein <strong>Absatz</strong>.</p>")
            .expect("die Konvertierung muss gelingen");
        assert!(markdown.contains("Titel"), "unerwartet: {markdown}");
        assert!(
            markdown.contains('#') || markdown.contains("Titel\n="),
            "eine Überschrift muss als Markdown erkennbar sein: {markdown}"
        );
        assert!(
            markdown.contains("**Absatz**") || markdown.contains("__Absatz__"),
            "Hervorhebung muss erhalten bleiben: {markdown}"
        );
    }

    /// `raw` lässt den Körper unangetastet, auch bei HTML.
    #[test]
    fn test_render_raw_returns_body_unchanged() {
        let rendered = render(HTML_SNIPPET, "text/html", OutputFormat::Raw).expect("raw");
        assert_eq!(rendered, HTML_SNIPPET);
    }

    /// Nicht-HTML wird in allen Formen unverändert durchgereicht.
    #[test]
    fn test_render_passes_non_html_through() {
        let json = r#"{"a":1}"#;
        for format in [
            OutputFormat::Text,
            OutputFormat::Markdown,
            OutputFormat::Raw,
        ] {
            assert_eq!(
                render(json, "application/json", format).expect("kein Fehler"),
                json,
                "Format {format:?} darf JSON nicht verändern"
            );
        }
    }

    /// Die Formatwahl akzeptiert die dokumentierten Werte und lehnt andere ab.
    #[test]
    fn test_output_format_parse_known_and_unknown_values() {
        assert_eq!(OutputFormat::parse(None), Some(OutputFormat::Text));
        assert_eq!(OutputFormat::parse(Some("  ")), Some(OutputFormat::Text));
        assert_eq!(OutputFormat::parse(Some("RAW")), Some(OutputFormat::Raw));
        assert_eq!(
            OutputFormat::parse(Some("md")),
            Some(OutputFormat::Markdown)
        );
        assert_eq!(OutputFormat::parse(Some("pdf")), None);
    }

    // --- Cache --------------------------------------------------------------

    /// Der Cache-Pfad liegt im `web`-Unterverzeichnis und ist ein Hash.
    #[test]
    fn test_cache_path_is_hashed_json_below_web_subdir() {
        let path = cache_path(Path::new("/tmp/harw-cache"), "https://docs.rs/serde/");
        assert!(path.starts_with("/tmp/harw-cache/web"));
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("Dateiname");
        assert_eq!(name.len(), 64, "SHA-256 als Hex hat 64 Zeichen");
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Verschiedene URLs erzeugen verschiedene Cache-Dateien.
    #[test]
    fn test_cache_path_differs_per_url() {
        let base = Path::new("/tmp/harw-cache");
        assert_ne!(
            cache_path(base, "https://docs.rs/a/"),
            cache_path(base, "https://docs.rs/b/")
        );
    }

    /// Schreiben und Lesen sind zueinander invers.
    #[test]
    fn test_cache_entry_round_trips_through_temp_dir() {
        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let entry = sample_entry();
        let path = cache_path(dir.path(), &entry.url);

        write_cache_entry(dir.path(), &path, &entry).expect("Schreiben muss gelingen");
        let loaded = read_cache_entry(&path)
            .expect("Lesen muss gelingen")
            .expect("der Eintrag muss existieren");

        assert_eq!(loaded, entry);
    }

    /// Ein fehlender Eintrag ist kein Fehler.
    #[test]
    fn test_read_cache_entry_missing_file_is_none() {
        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let path = cache_path(dir.path(), "https://docs.rs/fehlt/");
        assert_eq!(read_cache_entry(&path).expect("kein Fehler"), None);
    }

    /// Kaputtes JSON meldet `CacheCorrupt` statt still zu scheitern.
    #[test]
    fn test_read_cache_entry_reports_corrupt_content() {
        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let path = cache_path(dir.path(), "https://docs.rs/kaputt/");
        fs::create_dir_all(cache_subdir(dir.path())).expect("Unterverzeichnis");
        fs::write(&path, "{ kein json").expect("Schreiben");

        let err = read_cache_entry(&path).expect_err("kaputter Cache muss gemeldet werden");
        assert!(matches!(err, WebToolError::CacheCorrupt { .. }), "{err:?}");
    }

    /// Ein zweites Schreiben ersetzt den Eintrag vollständig.
    #[test]
    fn test_write_cache_entry_replaces_previous_content() {
        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let mut entry = sample_entry();
        let path = cache_path(dir.path(), &entry.url);

        write_cache_entry(dir.path(), &path, &entry).expect("erstes Schreiben");
        entry.body = "<html><body>neu</body></html>".to_owned();
        write_cache_entry(dir.path(), &path, &entry).expect("zweites Schreiben");

        let loaded = read_cache_entry(&path)
            .expect("Lesen")
            .expect("Eintrag vorhanden");
        assert_eq!(loaded.body, entry.body);
    }

    /// Ein untergeschobener Symlink wird abgelehnt, ohne das Ziel zu berühren.
    #[cfg(unix)]
    #[test]
    fn test_write_cache_entry_rejects_symlinked_target() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let entry = sample_entry();
        let path = cache_path(dir.path(), &entry.url);
        fs::create_dir_all(cache_subdir(dir.path())).expect("Unterverzeichnis");

        let outside = dir.path().join("nicht-ueberschreiben.txt");
        fs::write(&outside, "unberührt").expect("Zieldatei");
        symlink(&outside, &path).expect("Symlink");

        let err = write_cache_entry(dir.path(), &path, &entry)
            .expect_err("ein Symlink-Ziel muss abgelehnt werden");
        assert!(matches!(err, WebToolError::Io(_)), "{err:?}");
        assert_eq!(
            fs::read_to_string(&outside).expect("Zieldatei lesbar"),
            "unberührt"
        );
    }

    /// Ein symlinkter Cache-Eintrag wird nicht gelesen.
    #[cfg(unix)]
    #[test]
    fn test_read_cache_entry_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let path = cache_path(dir.path(), "https://docs.rs/symlink/");
        fs::create_dir_all(cache_subdir(dir.path())).expect("Unterverzeichnis");

        let outside = dir.path().join("geheim.json");
        fs::write(&outside, "{}").expect("Zieldatei");
        symlink(&outside, &path).expect("Symlink");

        let err = read_cache_entry(&path).expect_err("Symlinks dürfen nicht gelesen werden");
        assert!(matches!(err, WebToolError::Io(_)), "{err:?}");
    }

    /// Ein frischer Eintrag gilt als frisch, ein alter nicht.
    #[test]
    fn test_entry_is_fresh_respects_ttl() {
        let mut entry = sample_entry();
        assert!(entry_is_fresh(&entry, Duration::from_secs(3_600)));

        entry.fetched_at = now_secs().saturating_sub(7_200);
        assert!(!entry_is_fresh(&entry, Duration::from_secs(3_600)));
    }

    /// Ein Zeitstempel in der Zukunft löst keine Neuabruf-Schleife aus.
    #[test]
    fn test_entry_is_fresh_treats_future_timestamp_as_fresh() {
        let mut entry = sample_entry();
        entry.fetched_at = now_secs().saturating_add(600);
        assert!(entry_is_fresh(&entry, Duration::from_secs(60)));
    }

    // --- Fetcher-Konfiguration ---------------------------------------------

    /// Ohne Scope ist kein Host erreichbar; `fetch` scheitert vor dem Netz.
    #[tokio::test]
    async fn test_fetch_without_scope_denies_every_host() {
        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let fetcher = WebFetcher::new(
            dir.path().to_path_buf(),
            Duration::from_secs(60),
            DEFAULT_MAX_BYTES,
        )
        .expect("Client baut");

        let err = fetcher
            .fetch("https://docs.rs/serde/", OutputFormat::Text)
            .await
            .expect_err("ein leerer Scope darf keinen Host erlauben");
        assert!(
            matches!(err, WebToolError::RedirectHostNotAllowed { .. }),
            "{err:?}"
        );
    }

    /// `http://` scheitert am Schema, bevor der Scope überhaupt zählt.
    #[tokio::test]
    async fn test_fetch_rejects_http_before_touching_the_network() {
        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let fetcher = WebFetcher::new(
            dir.path().to_path_buf(),
            Duration::from_secs(60),
            DEFAULT_MAX_BYTES,
        )
        .expect("Client baut")
        .scoped(NetworkScope::from_hosts(["docs.rs".to_owned()]), None);

        let err = fetcher
            .fetch("http://docs.rs/serde/", OutputFormat::Text)
            .await
            .expect_err("http:// muss scheitern");
        assert!(
            matches!(err, WebToolError::SchemeNotAllowed { .. }),
            "{err:?}"
        );
    }

    /// Regression: die Punktgrenzen-Regel aus `NetworkScope::allows` gilt an
    /// dieser Aufrufstelle unverändert — `evildocs.rs` teilt sich die Endung
    /// mit dem erlaubten `docs.rs`, steht aber an keiner Punktgrenze und wird
    /// deshalb abgelehnt, bevor überhaupt eine Verbindung versucht wird.
    #[tokio::test]
    async fn test_fetch_rejects_evildocs_despite_shared_suffix_with_allowed_host() {
        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let fetcher = WebFetcher::new(
            dir.path().to_path_buf(),
            Duration::from_secs(60),
            DEFAULT_MAX_BYTES,
        )
        .expect("Client baut")
        .scoped(NetworkScope::from_hosts(["docs.rs".to_owned()]), None);

        let err = fetcher
            .fetch("https://evildocs.rs/x", OutputFormat::Text)
            .await
            .expect_err("evildocs.rs darf nicht durch bloße Endung durchrutschen");
        assert!(
            matches!(err, WebToolError::RedirectHostNotAllowed { .. }),
            "{err:?}"
        );
    }

    /// `Debug` zählt über `targets()`, nicht über das verlustbehaftete
    /// `hosts()`: ein `Cidr`-Ziel darf in der Diagnoseausgabe nicht
    /// verschwinden, nur weil es kein Hostname ist.
    #[test]
    fn test_webfetcher_debug_counts_cidr_targets_via_targets_not_hosts() {
        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let scope = NetworkScope::from_targets([
            EgressTarget::DnsSuffix("docs.rs".to_owned()),
            EgressTarget::Cidr("10.0.0.0/24".parse().expect("gültiges CIDR-Literal")),
        ]);
        let fetcher = WebFetcher::new(
            dir.path().to_path_buf(),
            Duration::from_secs(60),
            DEFAULT_MAX_BYTES,
        )
        .expect("Client baut")
        .scoped(scope, None);

        let debug = format!("{fetcher:?}");
        assert!(
            debug.contains("allowed_targets: 2"),
            "das CIDR-Ziel darf in der Debug-Ausgabe nicht verloren gehen: {debug}"
        );
    }

    /// Ein frischer Cache-Eintrag wird ohne Netzzugriff bedient.
    #[tokio::test]
    async fn test_fetch_serves_fresh_cache_entry_without_network() {
        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let entry = sample_entry();
        let path = cache_path(dir.path(), &entry.url);
        write_cache_entry(dir.path(), &path, &entry).expect("Cache schreiben");

        let fetcher = WebFetcher::new(
            dir.path().to_path_buf(),
            Duration::from_secs(3_600),
            DEFAULT_MAX_BYTES,
        )
        .expect("Client baut")
        .scoped(NetworkScope::from_hosts(["docs.rs".to_owned()]), None);

        let document = fetcher
            .fetch(&entry.url, OutputFormat::Raw)
            .await
            .expect("der frische Cache muss bedienen");

        assert!(document.from_cache);
        assert_eq!(document.status, 200);
        assert_eq!(document.body, HTML_SNIPPET);
        assert_eq!(document.etag.as_deref(), Some("\"abc123\""));
    }

    /// `scoped` deckelt ein angefordertes Limit auf die harte Obergrenze.
    #[test]
    fn test_scoped_caps_requested_max_bytes() {
        let dir = TempDir::new().expect("Temp-Verzeichnis");
        let fetcher = WebFetcher::new(
            dir.path().to_path_buf(),
            Duration::from_secs(60),
            DEFAULT_MAX_BYTES,
        )
        .expect("Client baut");

        let scoped = fetcher.scoped(NetworkScope::empty(), Some(usize::MAX));
        assert_eq!(scoped.max_bytes(), HARD_MAX_BYTES);

        let lowered = fetcher.scoped(NetworkScope::empty(), Some(2_048));
        assert_eq!(lowered.max_bytes(), 2_048);

        let inherited = fetcher.scoped(NetworkScope::empty(), None);
        assert_eq!(inherited.max_bytes(), DEFAULT_MAX_BYTES);
    }

    /// Das Tool deklariert Berechtigung und Parallelität wie zugesagt
    /// (Parallelität als Compile-Zeit-Assertion, siehe unten).
    #[test]
    fn test_web_fetch_tool_declares_network_permission_and_parallel_safety() {
        assert_eq!(WebFetchTool::NAME, "web.fetch");
        assert_eq!(
            WebFetchTool::PERMISSION,
            Some(harw_tools::Permission::NetworkAccess)
        );
    }

    // `PARALLEL_SAFE` is a macro-generated `const bool` (see
    // `#[harw_macros::tool]`), so any `assert!` on it is compile-time-constant
    // by construction — exactly what clippy's `assertions_on_constants` flags
    // as checking nothing at runtime. A `const` assertion embraces that fact
    // instead of fighting it: it fails to *compile* the moment `WebFetchTool`
    // stops declaring itself parallel-safe (other tools in the workspace do
    // declare `parallel_safe = false`, see harw-tools/src/provider_macro.rs,
    // so this is a real per-tool fact, not a type-level tautology), which is a
    // strictly stronger guarantee than the runtime assertion it replaces.
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

    // --- Tests, die einen laufenden Server bräuchten ------------------------

    /// Benötigt einen lokalen HTTPS-Server, der eine Weiterleitung auf einen
    /// nicht erlaubten Host schickt. Ohne Server nicht ausführbar.
    #[tokio::test]
    #[ignore = "benötigt einen lokalen HTTPS-Testserver mit Redirect auf einen fremden Host"]
    async fn test_redirect_to_disallowed_host_is_rejected() {
        unimplemented!("Testserver fehlt");
    }

    /// Benötigt einen lokalen HTTPS-Server, der mehr als [`MAX_REDIRECTS`]
    /// Weiterleitungen erzeugt.
    #[tokio::test]
    #[ignore = "benötigt einen lokalen HTTPS-Testserver mit langer Redirect-Kette"]
    async fn test_redirect_chain_beyond_limit_is_rejected() {
        unimplemented!("Testserver fehlt");
    }

    /// Benötigt einen lokalen HTTPS-Server, der `304 Not Modified` auf ein
    /// `If-None-Match` beantwortet.
    #[tokio::test]
    #[ignore = "benötigt einen lokalen HTTPS-Testserver mit ETag-Unterstützung"]
    async fn test_conditional_get_refreshes_cache_on_304() {
        unimplemented!("Testserver fehlt");
    }
}
