//! Mistral-OCR-Anbindung für `doc.read_pdf`.
//!
//! # Quelle
//! Design: `doc_read_pdf_design.md`, Abschnitt „Mistral OCR —
//! `harw-tool-doc/src/mistral.rs` (Owner W1c)“.
//!
//! # Verantwortung
//! Dieses Modul besitzt den gesamten Netzpfad zur Mistral-OCR-API: den Bau
//! eines Egress-geprüften `reqwest::Client` ([`MistralOcrClient::new`]), den
//! Upload/Inline-Versand einer PDF-Datei und das Parsen der Antwort
//! ([`MistralOcrClient::extract_pages`]) sowie die prozessweite Installation
//! über [`install_mistral_ocr`]/[`mistral_ocr`]. Es trifft keine
//! Sandbox-/Permission-Entscheidungen und liest keine Dateien — das besitzt
//! `crate::tool`.
//!
//! # Schlüsseltypen
//! - [`MistralOcrConfig`] — Basis-URL, API-Schlüssel, Modellname.
//! - [`MistralOcrClient`] — Egress-geprüfter HTTP-Client für `/ocr`,
//!   `/files` und `/files/{id}/url`.
//! - [`install_mistral_ocr`] / [`mistral_ocr`] — prozessweiter
//!   `OnceLock<Arc<MistralOcrClient>>`, analog zu
//!   `harw_tool_web::fetch::{install_fetcher, shared_fetcher}`.
//!
//! # Sicherheitskontrakt
//! Der `reqwest::Client` entsteht ausschließlich über
//! [`harw_egress::build_client`] mit einer Allowlist, die nur den Host der
//! konfigurierten Basis-URL enthält (keine privaten Adressklassen). Der
//! API-Schlüssel wird nur beim Setzen des `Authorization`-Headers offengelegt
//! (`secrecy::ExposeSecret`) und der Header-Wert danach als sensitiv markiert
//! (`HeaderValue::set_sensitive(true)`), damit er in `Debug`-Ausgaben und
//! HTTP/2-HPACK-Indizierung nicht auftaucht.
//!
//! # Fehler
//! Alle Fehler sind Varianten von [`DocToolError`], insbesondere
//! [`DocToolError::MistralApi`] (Fehlerantwort der API),
//! [`DocToolError::MistralResponse`] (unerwartete/unlesbare Antwort),
//! [`DocToolError::InvalidBaseUrl`] und [`DocToolError::AlreadyConfigured`].
//!
//! # Nebenläufigkeit
//! [`MistralOcrClient`] ist `Send + Sync` (nur unveränderliche Felder plus
//! ein geteilter `reqwest::Client`) und für parallele Anfragen gedacht. Die
//! prozessweite Installation läuft über ein `static OnceLock`, threadsicher
//! ohne zusätzliche Sperren.
//!
//! # Examples
//! ```rust,no_run
//! use harw_tool_doc::{DEFAULT_OCR_MODEL, install_mistral_ocr};
//! use harw_tool_doc::mistral::MistralOcrConfig;
//! use secrecy::SecretString;
//!
//! # fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! install_mistral_ocr(MistralOcrConfig {
//!     base_url: "https://api.mistral.ai/v1".to_owned(),
//!     api_key: SecretString::new("sk-...".into()),
//!     model: DEFAULT_OCR_MODEL.to_owned(),
//! })?;
//! # Ok(())
//! # }
//! ```

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use harw_egress::EgressPolicy;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use crate::error::{DocToolError, DocToolResult};
use crate::types::{Backend, ExtractedDocument, ExtractedPage, PageRange};

/// Voreingestelltes OCR-Modell, wenn der Aufrufer keines wählt.
pub const DEFAULT_OCR_MODEL: &str = "mistral-ocr-latest";

/// Obergrenze für den Inline-Base64-Versand einer PDF-Datei im Request-Body.
///
/// Dateien bis einschließlich dieser Größe gehen als `data:`-URL im
/// `/ocr`-Aufruf mit; größere Dateien laufen über `/files` (Upload, signierte
/// URL, anschließende Löschung).
pub const INLINE_LIMIT_BYTES: u64 = 10 * 1024 * 1024;

// Obergrenze je HTTP-Anfrage an die Mistral-API (Design-Vorgabe: 120 s).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

// Länge, auf die eine unstrukturierte Fehlerantwort gekürzt wird (Zeichen,
// nicht Bytes).
const ERROR_BODY_MAX_CHARS: usize = 500;

/// Konfiguration für [`MistralOcrClient::new`].
#[derive(Clone)]
pub struct MistralOcrConfig {
    /// Basis-URL der Mistral-API, z. B. `"https://api.mistral.ai/v1"`. Ein
    /// abschließender `/` ist erlaubt und wird beim Bau entfernt.
    pub base_url: String,
    /// API-Schlüssel; wird nur beim Setzen des `Authorization`-Headers
    /// offengelegt.
    pub api_key: SecretString,
    /// Modellname für `/ocr`-Anfragen, z. B. [`DEFAULT_OCR_MODEL`].
    pub model: String,
}

/// Egress-geprüfter HTTP-Client für die Mistral-OCR-API.
///
/// # Concurrency
/// `Send + Sync`; der innere `reqwest::Client` teilt seinen
/// Verbindungspool zwischen Klonen und parallelen Aufrufen.
pub struct MistralOcrClient {
    client: Client,
    // Basis-URL ohne abschließenden `/`, z. B. `"https://api.mistral.ai/v1"`.
    base_url: String,
    api_key: SecretString,
    model: String,
}

impl MistralOcrClient {
    /// Baut den Client zu einer [`MistralOcrConfig`].
    ///
    /// # Description
    /// Extrahiert den Host aus `config.base_url` und baut den
    /// `reqwest::Client` über
    /// `harw_egress::build_client(Arc::new(EgressPolicy::new(vec![host],
    /// false)?))` — die Allowlist enthält ausschließlich diesen Host, private
    /// Adressklassen bleiben gesperrt.
    ///
    /// # Arguments
    /// - `config` ([`MistralOcrConfig`]): Basis-URL, API-Schlüssel, Modell
    ///   (Eigentum geht an den Client über).
    ///
    /// # Returns
    /// Den einsatzbereiten [`MistralOcrClient`].
    ///
    /// # Errors
    /// - [`DocToolError::InvalidBaseUrl`]: `base_url` ist keine gültige URL
    ///   oder enthält keinen Host.
    /// - [`DocToolError::Egress`]: die Egress-Policy lässt sich nicht bauen
    ///   (ungültiger Host).
    /// - [`DocToolError::Http`]: der `reqwest::Client` lässt sich nicht
    ///   bauen.
    ///
    /// # Concurrency
    /// Reine Konstruktion, keine Netzanfrage.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_doc::mistral::{MistralOcrClient, MistralOcrConfig};
    /// use secrecy::SecretString;
    ///
    /// # fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MistralOcrClient::new(MistralOcrConfig {
    ///     base_url: "https://api.mistral.ai/v1".to_owned(),
    ///     api_key: SecretString::new("sk-...".into()),
    ///     model: "mistral-ocr-latest".to_owned(),
    /// })?;
    /// # let _ = client;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(config: MistralOcrConfig) -> DocToolResult<Self> {
        let host = base_url_host(&config.base_url)?;
        let policy = Arc::new(EgressPolicy::new(vec![host], false)?);
        let client = harw_egress::build_client(policy)?;
        Ok(Self {
            client,
            base_url: normalize_base_url(&config.base_url),
            api_key: config.api_key,
            model: config.model,
        })
    }

    /// Extrahiert Seitentext per Mistral OCR.
    ///
    /// # Description
    /// Dateien bis [`INLINE_LIMIT_BYTES`] gehen als Base64-`data:`-URL direkt
    /// im `/ocr`-Aufruf mit; größere Dateien laufen über `POST {base}/files`
    /// (Upload), `GET {base}/files/{id}/url?expiry=1` (signierte URL) und
    /// danach **immer** `DELETE {base}/files/{id}` — auch wenn der
    /// OCR-Aufruf selbst scheitert. Ist `range` gesetzt und hat ein
    /// geschlossenes Ende (`last` ist `Some`), wird `pages` (0-basiert) an
    /// die API gesendet; bei offenem Ende (`last == None`) oder `range ==
    /// None` verarbeitet die API das ganze Dokument und die Filterung nach
    /// `range` passiert clientseitig.
    ///
    /// # Arguments
    /// - `bytes` (`&[u8]`): der vollständige PDF-Inhalt (geborgt).
    /// - `file_name` (`&str`): Dateiname für den Upload-Pfad (nur bei Dateien
    ///   über [`INLINE_LIMIT_BYTES`] relevant).
    /// - `range` (`Option<`[`PageRange`]`>`): 1-basierter Seitenbereich;
    ///   `None` bedeutet das ganze Dokument.
    ///
    /// # Returns
    /// Ein [`ExtractedDocument`] mit `backend == `[`Backend::MistralOcr`],
    /// aufsteigend sortierten Seiten und `total_pages`, wenn die Anfrage ohne
    /// `pages`-Filter erfolgte.
    ///
    /// # Errors
    /// - [`DocToolError::MistralApi`]: die API antwortete mit einem
    ///   Status außerhalb von 2xx.
    /// - [`DocToolError::MistralResponse`]: die Antwort war kein
    ///   auswertbares JSON in der erwarteten Form.
    /// - [`DocToolError::Http`]: Transportfehler oder Timeout.
    ///
    /// # Concurrency
    /// Nebenläufig aufrufbar; jeder Aufruf ist unabhängig.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_doc::mistral::{MistralOcrClient, MistralOcrConfig};
    /// use secrecy::SecretString;
    ///
    /// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MistralOcrClient::new(MistralOcrConfig {
    ///     base_url: "https://api.mistral.ai/v1".to_owned(),
    ///     api_key: SecretString::new("sk-...".into()),
    ///     model: "mistral-ocr-latest".to_owned(),
    /// })?;
    /// let doc = client.extract_pages(b"%PDF-1.4 ...", "report.pdf", None).await?;
    /// # let _ = doc;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn extract_pages(
        &self,
        bytes: &[u8],
        file_name: &str,
        range: Option<PageRange>,
    ) -> DocToolResult<ExtractedDocument> {
        let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if byte_len <= INLINE_LIMIT_BYTES {
            let document_url = inline_document_url(bytes);
            self.run_ocr(&document_url, range).await
        } else {
            self.extract_pages_via_upload(bytes, file_name, range).await
        }
    }

    // Pfad für Dateien über `INLINE_LIMIT_BYTES`: Upload, signierte URL, OCR,
    // danach immer Löschung (auch bei Fehlschlag des OCR-Aufrufs).
    async fn extract_pages_via_upload(
        &self,
        bytes: &[u8],
        file_name: &str,
        range: Option<PageRange>,
    ) -> DocToolResult<ExtractedDocument> {
        let file_id = self.upload_file(bytes, file_name).await?;
        let outcome = async {
            let document_url = self.signed_file_url(&file_id).await?;
            self.run_ocr(&document_url, range).await
        }
        .await;
        if let Err(error) = self.delete_file(&file_id).await {
            tracing::warn!(
                file_id = %file_id,
                error = %error,
                "doc.read_pdf: Mistral-Datei-Löschung fehlgeschlagen"
            );
        }
        outcome
    }

    // Sendet `POST {base}/ocr` mit dem gegebenen `document_url` und wertet
    // die Antwort aus.
    async fn run_ocr(
        &self,
        document_url: &str,
        range: Option<PageRange>,
    ) -> DocToolResult<ExtractedDocument> {
        let pages = zero_based_pages(range);
        let body = ocr_request_body(&self.model, document_url, pages.as_deref());
        let response = self
            .authorized_request(Method::POST, "ocr")?
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(api_error(status, response).await);
        }
        let text = response.text().await?;
        parse_ocr_response(&text, range, pages.is_some())
    }

    // Lädt eine Datei per Multipart-Upload hoch und liefert ihre Mistral-Datei-ID.
    async fn upload_file(&self, bytes: &[u8], file_name: &str) -> DocToolResult<String> {
        let part = Part::bytes(bytes.to_vec())
            .file_name(file_name.to_owned())
            .mime_str("application/pdf")
            .map_err(|error| DocToolError::MistralResponse {
                reason: format!("Multipart-Teil für Upload ungültig: {error}"),
            })?;
        let form = Form::new().text("purpose", "ocr").part("file", part);
        let response = self
            .authorized_request(Method::POST, "files")?
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(api_error(status, response).await);
        }
        let text = response.text().await?;
        parse_file_id(&text)
    }

    // Holt die kurzlebige signierte URL einer hochgeladenen Datei.
    async fn signed_file_url(&self, file_id: &str) -> DocToolResult<String> {
        let path = format!("files/{file_id}/url?expiry=1");
        let response = self.authorized_request(Method::GET, &path)?.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(api_error(status, response).await);
        }
        let text = response.text().await?;
        parse_signed_url(&text)
    }

    // Löscht eine hochgeladene Datei; Aufrufer behandeln Fehler nur als Warnung.
    async fn delete_file(&self, file_id: &str) -> DocToolResult<()> {
        let path = format!("files/{file_id}");
        let response = self
            .authorized_request(Method::DELETE, &path)?
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(api_error(status, response).await);
        }
        Ok(())
    }

    // Baut eine Anfrage mit `Authorization: Bearer <key>` (sensitiv markiert)
    // und dem Anfrage-Timeout.
    fn authorized_request(&self, method: Method, path: &str) -> DocToolResult<RequestBuilder> {
        let url = format!("{}/{path}", self.base_url);
        let mut header_value =
            HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret())).map_err(
                |_| DocToolError::MistralResponse {
                    reason: "API-Schlüssel enthält ungültige Header-Zeichen".to_owned(),
                },
            )?;
        header_value.set_sensitive(true);
        Ok(self
            .client
            .request(method, url)
            .header(AUTHORIZATION, header_value)
            .timeout(REQUEST_TIMEOUT))
    }
}

// ---------------------------------------------------------------------------
// Prozessweite Installation
// ---------------------------------------------------------------------------

static MISTRAL_OCR: OnceLock<Arc<MistralOcrClient>> = OnceLock::new();

/// Hinterlegt den prozessweiten Mistral-OCR-Client.
///
/// # Description
/// Baut den Client über [`MistralOcrClient::new`] und hinterlegt ihn in
/// einem `static OnceLock`. Gedacht für einen einzigen Aufruf beim Boot
/// (`harw-cli`); ein zweiter Aufruf scheitert, ohne den bestehenden Client zu
/// ersetzen.
///
/// # Arguments
/// - `config` ([`MistralOcrConfig`]): Basis-URL, API-Schlüssel, Modell.
///
/// # Returns
/// `Ok(())`, wenn dieser Aufruf den Client installiert hat.
///
/// # Errors
/// - Jeder Fehler aus [`MistralOcrClient::new`] (ungültige Basis-URL,
///   Egress-Policy, Client-Bau).
/// - [`DocToolError::AlreadyConfigured`]: bereits ein Client installiert.
///
/// # Concurrency
/// Threadsicher über [`OnceLock`]; nur der erste erfolgreiche Aufruf gewinnt.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_doc::mistral::{DEFAULT_OCR_MODEL, MistralOcrConfig, install_mistral_ocr};
/// use secrecy::SecretString;
///
/// # fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// install_mistral_ocr(MistralOcrConfig {
///     base_url: "https://api.mistral.ai/v1".to_owned(),
///     api_key: SecretString::new("sk-...".into()),
///     model: DEFAULT_OCR_MODEL.to_owned(),
/// })?;
/// # Ok(())
/// # }
/// ```
pub fn install_mistral_ocr(config: MistralOcrConfig) -> DocToolResult<()> {
    let client = Arc::new(MistralOcrClient::new(config)?);
    MISTRAL_OCR
        .set(client)
        .map_err(|_| DocToolError::AlreadyConfigured)
}

/// Liefert den prozessweit installierten Mistral-OCR-Client, falls einer
/// installiert wurde.
///
/// # Returns
/// `Some(Arc<MistralOcrClient>)`, wenn [`install_mistral_ocr`] zuvor
/// erfolgreich war, sonst `None` (kein Mistral-Provider konfiguriert —
/// Aufrufer fallen dann auf die native Extraktion zurück).
///
/// # Concurrency
/// Threadsicher über [`OnceLock`]; beliebig oft von mehreren Threads
/// aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_doc::mistral::mistral_ocr;
///
/// if let Some(client) = mistral_ocr() {
///     # let _ = client;
///     // Mistral OCR verwenden.
/// }
/// ```
#[must_use]
pub fn mistral_ocr() -> Option<Arc<MistralOcrClient>> {
    MISTRAL_OCR.get().map(Arc::clone)
}

// ---------------------------------------------------------------------------
// Reine Hilfsfunktionen (netzfrei, unit-testbar)
// ---------------------------------------------------------------------------

// Extrahiert den Host aus einer Basis-URL für die Egress-Allowlist.
fn base_url_host(base_url: &str) -> DocToolResult<String> {
    let parsed = reqwest::Url::parse(base_url).map_err(|error| DocToolError::InvalidBaseUrl {
        base_url: base_url.to_owned(),
        reason: error.to_string(),
    })?;
    parsed
        .host_str()
        .map(str::to_owned)
        .ok_or_else(|| DocToolError::InvalidBaseUrl {
            base_url: base_url.to_owned(),
            reason: "kein Host in der Basis-URL".to_owned(),
        })
}

// Entfernt einen abschließenden `/` von der Basis-URL.
fn normalize_base_url(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_owned()
}

// 0-basierte Seitenindizes für den `pages`-Parameter, oder `None`, wenn kein
// (geschlossener) Bereich angefragt wurde — siehe `MistralOcrClient::extract_pages`.
fn zero_based_pages(range: Option<PageRange>) -> Option<Vec<u32>> {
    let range = range?;
    let last = range.last?;
    Some((range.first.saturating_sub(1)..=last.saturating_sub(1)).collect())
}

// Baut den `/ocr`-Anfragekörper.
fn ocr_request_body(model: &str, document_url: &str, pages: Option<&[u32]>) -> Value {
    let mut body = serde_json::json!({
        "model": model,
        "document": {
            "type": "document_url",
            "document_url": document_url,
        },
        "include_image_base64": false,
    });
    if let Some(pages) = pages {
        body["pages"] = serde_json::json!(pages);
    }
    body
}

// Base64-`data:`-URL für den Inline-Versand einer PDF-Datei.
fn inline_document_url(bytes: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:application/pdf;base64,{encoded}")
}

// Wertet die `/ocr`-Antwort aus: Seiten, clientseitige Filterung nach
// `range`, `total_pages` nur ohne serverseitigen `pages`-Filter.
fn parse_ocr_response(
    body: &str,
    range: Option<PageRange>,
    pages_filter_sent: bool,
) -> DocToolResult<ExtractedDocument> {
    let value: Value = serde_json::from_str(body).map_err(response_parse_error)?;
    let raw_pages = value
        .get("pages")
        .and_then(Value::as_array)
        .ok_or_else(|| DocToolError::MistralResponse {
            reason: "Antwort enthält kein 'pages'-Array".to_owned(),
        })?;
    let mut pages = Vec::with_capacity(raw_pages.len());
    for entry in raw_pages {
        pages.push(extracted_page_from_json(entry)?);
    }
    let total_pages = if pages_filter_sent {
        None
    } else {
        Some(u32::try_from(pages.len()).unwrap_or(u32::MAX))
    };
    let mut filtered: Vec<ExtractedPage> = match range {
        Some(range) => pages
            .into_iter()
            .filter(|page| range.contains(page.number))
            .collect(),
        None => pages,
    };
    filtered.sort_by_key(|page| page.number);
    Ok(ExtractedDocument {
        total_pages,
        pages: filtered,
        backend: Backend::MistralOcr,
    })
}

// Liest einen einzelnen Seiteneintrag (`{"index":u32,"markdown":String,...}`).
fn extracted_page_from_json(entry: &Value) -> DocToolResult<ExtractedPage> {
    let index = entry.get("index").and_then(Value::as_u64).ok_or_else(|| {
        DocToolError::MistralResponse {
            reason: "Seiteneintrag ohne numerischen 'index'".to_owned(),
        }
    })?;
    let index = u32::try_from(index).map_err(|_| DocToolError::MistralResponse {
        reason: "Seitenindex überschreitet den gültigen Wertebereich".to_owned(),
    })?;
    let markdown = entry
        .get("markdown")
        .and_then(Value::as_str)
        .ok_or_else(|| DocToolError::MistralResponse {
            reason: "Seiteneintrag ohne 'markdown'".to_owned(),
        })?;
    Ok(ExtractedPage {
        number: index.saturating_add(1),
        text: markdown.to_owned(),
    })
}

// Liest die Datei-ID aus der Antwort von `POST {base}/files`.
fn parse_file_id(body: &str) -> DocToolResult<String> {
    let value: Value = serde_json::from_str(body).map_err(response_parse_error)?;
    value
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| DocToolError::MistralResponse {
            reason: "Upload-Antwort enthält keine 'id'".to_owned(),
        })
}

// Liest die signierte URL aus der Antwort von `GET {base}/files/{id}/url`.
fn parse_signed_url(body: &str) -> DocToolResult<String> {
    let value: Value = serde_json::from_str(body).map_err(response_parse_error)?;
    value
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| DocToolError::MistralResponse {
            reason: "Antwort auf signierte URL enthält keine 'url'".to_owned(),
        })
}

// Baut einen `DocToolError::MistralResponse` aus einem `serde_json`-Parsefehler.
fn response_parse_error(error: serde_json::Error) -> DocToolError {
    DocToolError::MistralResponse {
        reason: format!("Antwort ist kein gültiges JSON: {error}"),
    }
}

// Liest Status und Fehlermeldung einer Nicht-2xx-Antwort. Der Körper wird
// gelesen, aber nie über die extrahierte Meldung hinaus weitergereicht.
async fn api_error(status: StatusCode, response: Response) -> DocToolError {
    let status_code = status.as_u16();
    let body = response.text().await.unwrap_or_else(|_| String::new());
    DocToolError::MistralApi {
        status: status_code,
        message: parse_api_error_message(&body),
    }
}

// Liest `{"message": ...}` aus einem Fehlerkörper, sonst den Rohtext, auf
// `ERROR_BODY_MAX_CHARS` Zeichen gekürzt.
fn parse_api_error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| truncate_chars(body, ERROR_BODY_MAX_CHARS))
}

// Kürzt `input` char-sicher auf höchstens `max_chars` Zeichen.
fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        input.to_owned()
    } else {
        input.chars().take(max_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_base_url_host_extracts_hostname() -> TestResult {
        let host = base_url_host("https://api.mistral.ai/v1").map_err(ctx("gültige Basis-URL"))?;
        assert_eq!(host, "api.mistral.ai");
        Ok(())
    }

    #[test]
    fn test_base_url_host_rejects_url_without_host() -> TestResult {
        let result = base_url_host("not-a-url");
        let Err(err) = result else {
            return Err(TestError::Unexpected(
                "erwartet InvalidBaseUrl, aber die URL wurde akzeptiert".into(),
            ));
        };
        assert!(
            matches!(err, DocToolError::InvalidBaseUrl { .. }),
            "{err:?}"
        );
        Ok(())
    }

    #[test]
    fn test_normalize_base_url_strips_trailing_slash() {
        assert_eq!(
            normalize_base_url("https://api.mistral.ai/v1/"),
            "https://api.mistral.ai/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.mistral.ai/v1"),
            "https://api.mistral.ai/v1"
        );
    }

    #[test]
    fn test_zero_based_pages_none_without_range() {
        assert_eq!(zero_based_pages(None), None);
    }

    #[test]
    fn test_zero_based_pages_none_for_open_end() -> TestResult {
        let range = PageRange::parse("4-").map_err(ctx("gültiger offener Bereich"))?;
        assert_eq!(zero_based_pages(Some(range)), None);
        Ok(())
    }

    #[test]
    fn test_zero_based_pages_closed_range_is_inclusive_zero_based() -> TestResult {
        let range = PageRange::parse("2-5").map_err(ctx("gültiger geschlossener Bereich"))?;
        assert_eq!(zero_based_pages(Some(range)), Some(vec![1, 2, 3, 4]));
        Ok(())
    }

    #[test]
    fn test_ocr_request_body_without_pages_omits_field() {
        let body = ocr_request_body(
            "mistral-ocr-latest",
            "data:application/pdf;base64,AA==",
            None,
        );
        assert_eq!(body["model"], "mistral-ocr-latest");
        assert_eq!(body["document"]["type"], "document_url");
        assert_eq!(
            body["document"]["document_url"],
            "data:application/pdf;base64,AA=="
        );
        assert_eq!(body["include_image_base64"], false);
        assert!(body.get("pages").is_none(), "{body:?}");
    }

    #[test]
    fn test_ocr_request_body_with_pages_includes_zero_based_indices() {
        let body = ocr_request_body("m", "u", Some(&[1, 2, 3]));
        assert_eq!(body["pages"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn test_inline_document_url_encodes_base64() {
        let url = inline_document_url(b"hi");
        assert_eq!(url, "data:application/pdf;base64,aGk=");
    }

    #[test]
    fn test_parse_ocr_response_without_filter_sets_total_pages() -> TestResult {
        let body = r#"{"pages":[{"index":0,"markdown":"a"},{"index":1,"markdown":"b"}]}"#;
        let doc = parse_ocr_response(body, None, false).map_err(ctx("gültige Antwort"))?;
        assert_eq!(doc.total_pages, Some(2));
        assert_eq!(doc.pages.len(), 2);
        assert_eq!(doc.pages[0].number, 1);
        assert_eq!(doc.pages[0].text, "a");
        assert_eq!(doc.pages[1].number, 2);
        assert_eq!(doc.backend, Backend::MistralOcr);
        Ok(())
    }

    #[test]
    fn test_parse_ocr_response_with_filter_sent_has_no_total_pages() -> TestResult {
        let body = r#"{"pages":[{"index":1,"markdown":"b"}]}"#;
        let doc = parse_ocr_response(body, None, true).map_err(ctx("gültige Antwort"))?;
        assert_eq!(doc.total_pages, None);
        Ok(())
    }

    #[test]
    fn test_parse_ocr_response_filters_and_sorts_by_range() -> TestResult {
        let body = r#"{"pages":[{"index":2,"markdown":"c"},{"index":0,"markdown":"a"},{"index":1,"markdown":"b"}]}"#;
        let range = PageRange::parse("2-3").map_err(ctx("gültiger Bereich"))?;
        let doc = parse_ocr_response(body, Some(range), false).map_err(ctx("gültige Antwort"))?;
        let numbers: Vec<u32> = doc.pages.iter().map(|page| page.number).collect();
        assert_eq!(numbers, vec![2, 3]);
        Ok(())
    }

    #[test]
    fn test_parse_ocr_response_rejects_missing_pages_array() -> TestResult {
        let result = parse_ocr_response("{}", None, false);
        let Err(err) = result else {
            return Err(TestError::Unexpected(
                "erwartet MistralResponse, aber leeres Objekt wurde akzeptiert".into(),
            ));
        };
        assert!(
            matches!(err, DocToolError::MistralResponse { .. }),
            "{err:?}"
        );
        Ok(())
    }

    #[test]
    fn test_parse_ocr_response_rejects_invalid_json() {
        let result = parse_ocr_response("not json", None, false);
        assert!(
            matches!(result, Err(DocToolError::MistralResponse { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn test_parse_file_id_reads_id_field() -> TestResult {
        let id = parse_file_id(r#"{"id":"file-123"}"#).map_err(ctx("gültige Upload-Antwort"))?;
        assert_eq!(id, "file-123");
        Ok(())
    }

    #[test]
    fn test_parse_file_id_rejects_missing_id() {
        let result = parse_file_id(r#"{"object":"file"}"#);
        assert!(
            matches!(result, Err(DocToolError::MistralResponse { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn test_parse_signed_url_reads_url_field() -> TestResult {
        let url = parse_signed_url(r#"{"url":"https://example.test/x"}"#)
            .map_err(ctx("gültige URL-Antwort"))?;
        assert_eq!(url, "https://example.test/x");
        Ok(())
    }

    #[test]
    fn test_parse_signed_url_rejects_missing_url() {
        let result = parse_signed_url(r#"{"expiry":1}"#);
        assert!(
            matches!(result, Err(DocToolError::MistralResponse { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn test_parse_api_error_message_prefers_json_message() {
        let message = parse_api_error_message(r#"{"object":"error","message":"invalid model"}"#);
        assert_eq!(message, "invalid model");
    }

    #[test]
    fn test_parse_api_error_message_falls_back_to_truncated_raw_text() {
        let raw = "x".repeat(600);
        let message = parse_api_error_message(&raw);
        assert_eq!(message.chars().count(), ERROR_BODY_MAX_CHARS);
    }

    #[test]
    fn test_parse_api_error_message_keeps_short_raw_text_untruncated() {
        let message = parse_api_error_message("plain text error");
        assert_eq!(message, "plain text error");
    }

    #[test]
    fn test_truncate_chars_is_char_safe() {
        // Mehrbyte-Zeichen ('ä' ist 2 Bytes in UTF-8): Kürzen auf 1 Zeichen
        // darf nicht mitten im UTF-8-Encoding schneiden.
        let truncated = truncate_chars("ä a", 1);
        assert_eq!(truncated, "ä");
    }
}
