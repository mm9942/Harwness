//! `doc.read_pdf` — Tool-Executor für seitenweises PDF-Lesen.
//!
//! Spezifikationsquelle: `doc_read_pdf_design.md`, Abschnitt „Tool —
//! `harw-tool-doc/src/tool.rs` + `provider.rs`".
//!
//! # Verantwortung
//! Dieses Modul besitzt den `doc.read_pdf`-Executor:
//! - [`DocReadPdfExecutor`]: implementiert [`ToolExecutor`]; öffnet eine PDF-
//!   Datei aus dem Workspace symlinkfrei, wählt das Backend (Mistral OCR,
//!   sonst lokal über `oxidize-pdf`) und formatiert das Ergebnis.
//!
//! # Ablauf (`DocReadPdfExecutor::execute`)
//! 1. `harw_tools::sandbox_guard::require_permission(ctx,
//!    Permission::ReadWorkspace, "doc.read_pdf")` — vor jeder
//!    Deserialisierung modell-kontrollierter Argumente.
//! 2. Datei sicher öffnen: Pfad → relativ (lokaler Nachbau von
//!    `harw-tool-fs`s `tree::normalize_relative`, siehe
//!    [`normalize_relative`]), dann `harw_fsutil::open_dir_nofollow` +
//!    `harw_fsutil::open_beneath` (lokaler Nachbau von `harw-tool-fs`s
//!    `Workspace::open(...).open_any(...)`, siehe [`open_beneath_root`]).
//!    `harw-tool-fs` selbst wird dafür weder verändert noch als Abhängigkeit
//!    referenziert — beide Hilfsfunktionen sind bewusste Duplikate.
//!    Größe ≤ [`MAX_PDF_BYTES`], sonst [`DocToolError::TooLarge`]. Lesen
//!    läuft über `tokio::task::spawn_blocking`. Magic `%PDF-` geprüft, sonst
//!    [`DocToolError::NotAPdf`].
//! 3. Backend: `backend != "native"` UND [`crate::mistral::mistral_ocr`]
//!    liefert `Some` → Mistral OCR. Bei einem Mistral-Fehler fällt die
//!    Ausführung auf die lokale Extraktion zurück; die Ausgabe erhält eine
//!    Hinweiszeile. Lokale Extraktion läuft in `spawn_blocking` mit
//!    `tokio::time::timeout`.
//! 4. Ausgabe formatieren (Kopf, Seiten, Kürzung auf `max_chars`), siehe
//!    [`render_output`].
//!
//! # Schlüsseltypen
//! - [`DocReadPdfExecutor`]
//!
//! # Nebenläufigkeit
//! [`DocReadPdfExecutor`] ist `Send + Sync` (Unit-Struct) und `parallel_safe`
//! (reiner Lesezugriff). Datei-I/O und native Extraktion laufen über
//! `tokio::task::spawn_blocking`.
//!
//! # Fehler
//! Permission-, Argument-, Datei- und Extraktionsfehler münden alle in
//! `Ok(ToolOutput::error(...))` (Muster von `harw-tool-web` übernommen:
//! `web_fetch` in `harw-tool-web/src/fetch.rs`). Nur fehlerhafte JSON-
//! Argumente liefern `Err(ToolsError::InvalidArguments)`.

use crate::error::{DocToolError, DocToolResult};
use crate::types::{ExtractedDocument, PageRange};
use harw_tools::{
    FunctionToolSpec, Permission, ToolCall, ToolName, ToolOutput, ToolSpec, ToolsError,
    executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture},
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Obergrenze der lesbaren PDF-Größe (Design: 50 MB).
pub const MAX_PDF_BYTES: u64 = 50 * 1024 * 1024;

/// Vorgabe für `max_chars`, wenn das Modell keinen Wert übergibt.
const DEFAULT_MAX_CHARS: u64 = 60_000;

/// Harte Obergrenze für `max_chars`, unabhängig vom Modell-Wunsch.
const MAX_MAX_CHARS: u64 = 200_000;

// Konstanten-Invariante: `clamp(1, MAX_MAX_CHARS)` weiter unten kann nur
// dann nicht wegen `min > max` panicken, wenn dies zur Compile-Zeit gilt.
const _: () = assert!(1 <= MAX_MAX_CHARS);

/// Zeitbudget der nativen Extraktion (`native::extract_pages` läuft in
/// `spawn_blocking`, mit `tokio::time::timeout` um dieses Budget herum).
const NATIVE_TIMEOUT_SECS: u64 = 60;

/// Höchstlänge des in der Rückfall-Hinweiszeile zitierten Mistral-Fehlers.
const FALLBACK_REASON_MAX_CHARS: usize = 160;

/// Deserialisierte Argumente für `doc.read_pdf`.
#[derive(Debug, Deserialize)]
struct DocReadPdfArgs {
    /// Pfad zur PDF-Datei, relativ zum Workspace-Root (wie bei `fs.read`).
    path: String,
    /// Optionaler Seitenbereich, z. B. `"1-5"`, `"3"`, `"4-"` (1-basiert).
    /// Fehlt er, wird das gesamte Dokument gelesen.
    pages: Option<String>,
    /// Zeichen-Obergrenze der Ausgabe (Vorgabe 60000, Obergrenze 200000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    max_chars: Option<u64>,
    /// `"auto"` (Vorgabe, nutzt Mistral OCR falls konfiguriert) oder
    /// `"native"` (erzwingt lokale Extraktion).
    backend: Option<String>,
}

// ---------------------------------------------------------------------------
// Sicheres Öffnen (lokaler Nachbau von harw-tool-fs/src/tree.rs — siehe
// Moduldoku, Schritt 2; harw-tool-fs selbst bleibt unverändert).
// ---------------------------------------------------------------------------

/// Prüft einen vom Modell gelieferten Pfad lexikalisch und normalisiert ihn.
///
/// Nur normale Komponenten und `.` sind erlaubt; abschließende `/` werden
/// verworfen. Ein leeres Ergebnis bezeichnet die Workspace-Wurzel.
///
/// # Errors
/// Menschenlesbarer Text für leere, absolute oder `..`-haltige Pfade.
fn normalize_relative(input: &str) -> Result<PathBuf, String> {
    if input.is_empty() {
        return Err("leerer Pfad ist nicht erlaubt".to_owned());
    }
    let mut normalized = PathBuf::new();
    for component in Path::new(input).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("'{input}': `..` ist nicht erlaubt"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("'{input}': absolute Pfade sind nicht erlaubt"));
            }
        }
    }
    Ok(normalized)
}

/// `rel` in der Form, die `harw_fsutil::open_beneath` erwartet (`.` für die
/// Wurzel).
fn beneath_arg(rel: &Path) -> &Path {
    if rel.as_os_str().is_empty() {
        Path::new(".")
    } else {
        rel
    }
}

/// Öffnet `rel` symlinkfrei unterhalb von `canonical_root`: kein Pfadglied
/// darf ein Symlink sein, kein Pfad darf die Wurzel verlassen.
///
/// # Errors
/// Fehler von `harw_fsutil::open_dir_nofollow`/`harw_fsutil::open_beneath`
/// (z. B. `ELOOP` bei Symlinks).
fn open_beneath_root(canonical_root: &Path, rel: &Path) -> io::Result<File> {
    let root_fd = harw_fsutil::open_dir_nofollow(canonical_root)?;
    harw_fsutil::open_beneath(
        root_fd.as_fd(),
        beneath_arg(rel),
        harw_fsutil::OpenMode::read_only(),
    )
}

/// Öffnet und liest die PDF-Datei blockierend (Design-Schritt 2); läuft im
/// Blocking-Pool über [`read_pdf_bytes`].
///
/// Kappt bereits beim Lesen auf `MAX_PDF_BYTES + 1` Bytes (`take`), damit
/// eine zwischen `open` und `read` wachsende Datei den Speicher nicht
/// sprengt — dasselbe Muster wie `harw-tool-fs/src/read.rs`.
fn read_pdf_bytes_blocking(canonical_root: &Path, raw_path: &str) -> DocToolResult<Vec<u8>> {
    let relative = normalize_relative(raw_path)
        .map_err(|reason| DocToolError::Io(io::Error::new(io::ErrorKind::InvalidInput, reason)))?;

    let file = open_beneath_root(canonical_root, &relative).map_err(|err| {
        DocToolError::Io(io::Error::new(
            err.kind(),
            format!(
                "'{raw_path}' kann nicht geöffnet werden (Symlinks werden nicht verfolgt): {err}"
            ),
        ))
    })?;

    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(DocToolError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("'{raw_path}' ist keine reguläre Datei"),
        )));
    }

    let mut bytes = Vec::new();
    file.take(MAX_PDF_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let actual_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_len > MAX_PDF_BYTES {
        return Err(DocToolError::TooLarge {
            path: raw_path.to_owned(),
            bytes: actual_len,
            limit: MAX_PDF_BYTES,
        });
    }

    if !bytes.starts_with(b"%PDF-") {
        return Err(DocToolError::NotAPdf {
            path: raw_path.to_owned(),
        });
    }
    Ok(bytes)
}

/// Liest die PDF-Datei asynchron über den Blocking-Pool (Design-Schritt 2).
///
/// # Errors
/// - [`DocToolError::Join`]: die Blocking-Aufgabe ist abgebrochen oder in
///   Panik geraten.
/// - Alles, was [`read_pdf_bytes_blocking`] liefert.
async fn read_pdf_bytes(context: &ToolExecutionContext, raw_path: &str) -> DocToolResult<Vec<u8>> {
    let root = context.sandbox().workspace().canonical_root().to_path_buf();
    let owned_path = raw_path.to_owned();
    match tokio::task::spawn_blocking(move || read_pdf_bytes_blocking(&root, &owned_path)).await {
        Ok(result) => result,
        Err(join_err) => Err(DocToolError::Join {
            reason: join_err.to_string(),
        }),
    }
}

/// Führt `native::extract_pages` blockierend mit Zeitbudget aus
/// (Design-Schritt 3).
///
/// # Errors
/// - [`DocToolError::Timeout`]: [`NATIVE_TIMEOUT_SECS`] überschritten.
/// - [`DocToolError::Join`]: die Blocking-Aufgabe ist abgebrochen oder in
///   Panik geraten.
/// - Alles, was `native::extract_pages` liefert.
async fn extract_native_with_timeout(
    bytes: Vec<u8>,
    range: Option<PageRange>,
) -> DocToolResult<ExtractedDocument> {
    let job = tokio::task::spawn_blocking(move || crate::native::extract_pages(&bytes, range));
    match tokio::time::timeout(Duration::from_secs(NATIVE_TIMEOUT_SECS), job).await {
        Ok(Ok(result)) => result,
        Ok(Err(join_err)) => Err(DocToolError::Join {
            reason: join_err.to_string(),
        }),
        Err(_elapsed) => Err(DocToolError::Timeout {
            seconds: NATIVE_TIMEOUT_SECS,
        }),
    }
}

/// Kürzt eine Fehlermeldung zeichensicher auf höchstens
/// [`FALLBACK_REASON_MAX_CHARS`] Zeichen, für den Rückfall-Hinweis in der
/// `doc.read_pdf`-Ausgabe (`"<kurz>"` im Design-Dokument).
fn short_reason(reason: &str) -> String {
    let mut chars = reason.chars();
    let shortened: String = chars.by_ref().take(FALLBACK_REASON_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

/// Kern von `doc.read_pdf`: Backend-Wahl, Extraktion, Ausgabeformatierung
/// (Design-Schritte 2–4).
async fn run(context: &ToolExecutionContext, args: &DocReadPdfArgs) -> DocToolResult<ToolOutput> {
    let range = match args.pages.as_deref() {
        Some(raw) => Some(PageRange::parse(raw)?),
        None => None,
    };

    let bytes = read_pdf_bytes(context, &args.path).await?;

    let file_name = Path::new(&args.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(args.path.as_str());

    let force_native = args.backend.as_deref() == Some("native");
    let mistral_client = if force_native {
        None
    } else {
        crate::mistral::mistral_ocr()
    };

    let (document, fallback_note) = if let Some(client) = mistral_client {
        match client.extract_pages(&bytes, file_name, range).await {
            Ok(document) => (document, None),
            Err(err) => {
                let note = format!(
                    "[doc.read_pdf: Mistral OCR fehlgeschlagen ({}), lokaler Rückfall]",
                    short_reason(&err.to_string())
                );
                let document = extract_native_with_timeout(bytes, range).await?;
                (document, Some(note))
            }
        }
    } else {
        let document = extract_native_with_timeout(bytes, range).await?;
        (document, None)
    };

    Ok(render_output(
        &args.path,
        &document,
        args.max_chars,
        fallback_note.as_deref(),
    ))
}

/// Formatiert Kopf, Seiten und Kürzungshinweis der `doc.read_pdf`-Ausgabe
/// (Design-Schritt 4).
///
/// # Description
/// Ohne Treffer im angeforderten Bereich liefert diese Funktion eine
/// Erfolgsmeldung (`"{path} hat nur {N} Seiten"`), keinen Fehler. Seiten
/// werden erst gesammelt (`body`, gegen `max_chars` budgetiert — mindestens
/// eine Seite wird immer aufgenommen, auch wenn sie das Budget allein schon
/// sprengt), danach wird der Kopf aus dem tatsächlich enthaltenen Bereich
/// gebaut (nicht aus dem ursprünglich angeforderten). Das entspricht
/// `harw-tool-fs/src/read.rs`s Zeilenmodus: dessen Kopfzeile zählt ebenfalls
/// nicht gegen das Ausgabelimit, nur der Textkörper. Bleibt der Textkörper
/// trotzdem über dem Budget (eine einzelne Seite ist länger als `max_chars`),
/// wird zusätzlich zeichensicher mitten im Text gekappt. Ein
/// Fortsetzungshinweis (`pages="{nächste}-"`) wird angehängt, wenn nicht der
/// gesamte angeforderte Bereich enthalten ist.
fn render_output(
    path: &str,
    document: &ExtractedDocument,
    max_chars: Option<u64>,
    fallback_note: Option<&str>,
) -> ToolOutput {
    let total_display = document
        .total_pages
        .map_or_else(|| "?".to_owned(), |n| n.to_string());

    if document.pages.is_empty() {
        let mut content = format!("{path} hat nur {total_display} Seiten");
        if let Some(note) = fallback_note {
            content = format!("{note}\n{content}");
        }
        return ToolOutput::text(content);
    }

    let first_page = document.pages.first().map_or(0, |page| page.number);
    let requested_last_page = document.pages.last().map_or(0, |page| page.number);

    // `clamp` ist hier sicher: MAX_MAX_CHARS >= 1 ist eine geprüfte
    // Konstanteninvariante (siehe `const _` bei der Deklaration oben).
    let budget = usize::try_from(
        max_chars
            .unwrap_or(DEFAULT_MAX_CHARS)
            .clamp(1, MAX_MAX_CHARS),
    )
    .unwrap_or(usize::MAX);

    let mut body = String::new();
    let mut included_last_page: Option<u32> = None;
    for page in &document.pages {
        let piece = format!("\n--- Seite {} ---\n{}", page.number, page.text);
        let candidate_len = body.chars().count() + piece.chars().count();
        // Die erste Seite wird immer aufgenommen (sonst gäbe es keine
        // Ausgabe), jede weitere nur, solange das Budget reicht.
        if included_last_page.is_some() && candidate_len > budget {
            break;
        }
        body.push_str(&piece);
        included_last_page = Some(page.number);
        if candidate_len > budget {
            break;
        }
    }

    let last_page = included_last_page.unwrap_or(first_page);
    let mut header = format!(
        "{path} — Seiten {first_page}–{last_page} von {total_display} ({})",
        document.backend.label()
    );
    if let Some(note) = fallback_note {
        header = format!("{note}\n{header}");
    }

    // Auch wahr, wenn ausnahmslos alle angeforderten Seiten enthalten sind,
    // aber schon die letzte davon allein das Budget sprengt (Einzelseite
    // größer als `max_chars`).
    let truncated =
        included_last_page != Some(requested_last_page) || body.chars().count() > budget;
    let mut content = if truncated && body.chars().count() > budget {
        let safe_body: String = body.chars().take(budget).collect();
        format!("{header}{safe_body}")
    } else {
        format!("{header}{body}")
    };
    if truncated {
        let next = included_last_page.map_or(first_page, |n| n + 1);
        content.push_str(&format!(
            "\n\n[doc.read_pdf: gekürzt — weiter mit pages=\"{next}-\"]"
        ));
    }

    ToolOutput::text(content)
}

/// Baut die `ToolSpec` von `doc.read_pdf` (Design-Abschnitt „Tool", Feld
/// „Spec").
fn doc_read_pdf_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "path".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Path to the PDF file to read, relative to the workspace root.".to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "pages".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Optional 1-based page range, e.g. '1-5', '3', or '4-' (page 4 to the end). \
                 Default: the whole document."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "max_chars".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Integer),
            description: Some(
                "Maximum output length in characters. Default: 60000, hard maximum: 200000."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    props.insert(
        "backend".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "'auto' (default: uses Mistral OCR when a Mistral provider is configured) or \
                 'native' (forces local extraction)."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );

    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(DocReadPdfExecutor::NAME),
        description: "Liest eine PDF-Datei aus dem Workspace seitenweise als Text/Markdown \
             (`pages` z. B. \"1-5\", 1-basiert). Nutzt Mistral OCR (auch gescannte Seiten, \
             Tabellen), wenn ein Mistral-Provider konfiguriert ist, sonst lokale \
             Textextraktion. Für große PDFs seitenweise lesen. `fs.read` ist für PDFs \
             ungeeignet."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props),
            required: Some(vec!["path".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: false,
    })
}

/// Führt `doc.read_pdf` aus: PDF-Datei öffnen, per Mistral OCR oder lokal
/// extrahieren, Ausgabe formatieren (Design, Abschnitt „Tool").
///
/// # Description
/// Zustandslose Unit-Struktur. `execute` prüft zuerst `ReadWorkspace` (vor
/// jeder Deserialisierung modell-kontrollierter Argumente), öffnet die
/// Datei symlinkfrei unterhalb der Workspace-Wurzel, wählt das Backend
/// (Mistral OCR, falls konfiguriert und nicht per `backend: "native"`
/// erzwungen andernfalls; sonst lokal über `oxidize-pdf`) und formatiert
/// das Ergebnis.
///
/// # Concurrency
/// `Send + Sync`; über [`std::sync::Arc`] teilbar. `doc.read_pdf` ist
/// `parallel_safe` (reiner Lesezugriff).
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_doc::tool::DocReadPdfExecutor;
///
/// let _executor = DocReadPdfExecutor;
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct DocReadPdfExecutor;

impl DocReadPdfExecutor {
    /// Tool-Name, wie er in `ToolCall::name` und im generierten `ToolSpec`
    /// erscheint.
    pub const NAME: &'static str = "doc.read_pdf";

    /// Reiner Lesezugriff: parallele Aufrufe sind sicher.
    pub const PARALLEL_SAFE: bool = true;

    /// Die von [`ToolExecutor::execute`] erzwungene Berechtigung —
    /// auditierbare Deklaration für `harw_tools::tool_provider!`.
    pub const PERMISSION: Option<Permission> = Some(Permission::ReadWorkspace);

    /// Baut die `ToolSpec` dieses Tools.
    ///
    /// # Returns
    /// `ToolSpec::Function` mit dem in der Design-Doku festgelegten
    /// Beschreibungstext und Parameterschema.
    ///
    /// # Concurrency
    /// Reine Funktion.
    #[must_use]
    pub fn spec() -> ToolSpec {
        doc_read_pdf_spec()
    }
}

impl ToolExecutor for DocReadPdfExecutor {
    /// Führt eine `doc.read_pdf`-Invokation aus (Design, Abschnitt „Tool",
    /// Schritte 1–4).
    ///
    /// # Arguments
    /// - `context` (`&ToolExecutionContext`): harness-etablierte Autorität
    ///   mit Sandbox-Spec (Permissions + Workspace).
    /// - `call` (`&ToolCall`): die ungeprüfte Invokation mit JSON-Argumenten.
    ///
    /// # Returns
    /// `Ok(ToolOutput::Text { .. })` bei Erfolg — auch, wenn der
    /// angeforderte Seitenbereich außerhalb des Dokuments liegt (das ist
    /// kein Fehler). `Ok(ToolOutput::Error { .. })` bei Permission-,
    /// Argument-, Datei- oder Extraktionsfehlern.
    ///
    /// # Errors
    /// - [`ToolsError::InvalidArguments`]: fehlende oder fehlerhafte JSON-Argumente.
    ///
    /// # Concurrency
    /// Sicher für parallele Aufrufe; Datei-I/O und native Extraktion laufen
    /// im Blocking-Pool.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if let Some(denied) = harw_tools::sandbox_guard::require_permission(
                context,
                Permission::ReadWorkspace,
                Self::NAME,
            ) {
                return Ok(denied);
            }

            let args: DocReadPdfArgs = match serde_json::from_value(call.arguments.clone()) {
                Ok(args) => args,
                Err(err) => {
                    return Err(ToolsError::InvalidArguments {
                        name: Self::NAME.to_owned(),
                        reason: err.to_string(),
                    });
                }
            };

            match run(context, &args).await {
                Ok(output) => Ok(output),
                Err(err) => Ok(ToolOutput::error(err.to_string())),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::types::{Backend, ExtractedPage};

    // --- normalize_relative --------------------------------------------------

    #[test]
    fn test_normalize_relative_accepts_plain_relative_path() -> TestResult {
        let rel = normalize_relative("a/b.pdf").map_err(TestError::Unexpected)?;
        assert_eq!(rel, PathBuf::from("a/b.pdf"));
        Ok(())
    }

    #[test]
    fn test_normalize_relative_rejects_parent_dir() {
        assert!(normalize_relative("../secret.pdf").is_err());
    }

    #[test]
    fn test_normalize_relative_rejects_absolute_path() {
        assert!(normalize_relative("/etc/passwd").is_err());
    }

    #[test]
    fn test_normalize_relative_rejects_empty_path() {
        assert!(normalize_relative("").is_err());
    }

    // --- read_pdf_bytes_blocking ----------------------------------------------

    #[test]
    fn test_read_pdf_bytes_blocking_accepts_pdf_magic() -> TestResult {
        let dir = tempfile::TempDir::new().map_err(ctx("TempDir::new"))?;
        std::fs::write(dir.path().join("doc.pdf"), b"%PDF-1.4\n%%EOF")
            .map_err(ctx("Testdatei schreiben"))?;

        let bytes = read_pdf_bytes_blocking(dir.path(), "doc.pdf")
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(bytes.starts_with(b"%PDF-"));
        Ok(())
    }

    #[test]
    fn test_read_pdf_bytes_blocking_rejects_non_pdf_magic() -> TestResult {
        let dir = tempfile::TempDir::new().map_err(ctx("TempDir::new"))?;
        std::fs::write(dir.path().join("not.pdf"), b"hello world")
            .map_err(ctx("Testdatei schreiben"))?;

        let Err(err) = read_pdf_bytes_blocking(dir.path(), "not.pdf") else {
            return Err(TestError::Unexpected(
                "eine Datei ohne %PDF--Signatur muss abgelehnt werden".to_owned(),
            ));
        };
        assert!(matches!(err, DocToolError::NotAPdf { .. }));
        Ok(())
    }

    #[test]
    fn test_read_pdf_bytes_blocking_rejects_oversized_file() -> TestResult {
        let dir = tempfile::TempDir::new().map_err(ctx("TempDir::new"))?;
        let mut oversized = b"%PDF-1.4\n".to_vec();
        oversized.resize(
            usize::try_from(MAX_PDF_BYTES).unwrap_or(usize::MAX) + 16,
            b'A',
        );
        std::fs::write(dir.path().join("huge.pdf"), &oversized)
            .map_err(ctx("Testdatei schreiben"))?;

        let Err(err) = read_pdf_bytes_blocking(dir.path(), "huge.pdf") else {
            return Err(TestError::Unexpected(
                "eine Datei über MAX_PDF_BYTES muss abgelehnt werden".to_owned(),
            ));
        };
        assert!(matches!(err, DocToolError::TooLarge { .. }));
        Ok(())
    }

    #[test]
    fn test_read_pdf_bytes_blocking_rejects_missing_file() -> TestResult {
        let dir = tempfile::TempDir::new().map_err(ctx("TempDir::new"))?;
        let Err(err) = read_pdf_bytes_blocking(dir.path(), "ghost.pdf") else {
            return Err(TestError::Unexpected(
                "eine fehlende Datei muss abgelehnt werden".to_owned(),
            ));
        };
        assert!(matches!(err, DocToolError::Io(_)));
        Ok(())
    }

    #[test]
    fn test_read_pdf_bytes_blocking_rejects_path_traversal() -> TestResult {
        let dir = tempfile::TempDir::new().map_err(ctx("TempDir::new"))?;
        let Err(err) = read_pdf_bytes_blocking(dir.path(), "../outside.pdf") else {
            return Err(TestError::Unexpected(
                "ein `..`-Pfad muss abgelehnt werden".to_owned(),
            ));
        };
        assert!(matches!(err, DocToolError::Io(_)));
        Ok(())
    }

    // --- short_reason ----------------------------------------------------------

    #[test]
    fn test_short_reason_keeps_short_message_unchanged() {
        assert_eq!(short_reason("timeout"), "timeout");
    }

    #[test]
    fn test_short_reason_truncates_long_message() {
        let long = "x".repeat(500);
        let shortened = short_reason(&long);
        assert!(shortened.chars().count() <= FALLBACK_REASON_MAX_CHARS + 1);
        assert!(shortened.ends_with('…'));
    }

    // --- render_output -----------------------------------------------------

    fn document(pages: Vec<ExtractedPage>, total_pages: Option<u32>) -> ExtractedDocument {
        ExtractedDocument {
            total_pages,
            pages,
            backend: Backend::Native,
        }
    }

    #[test]
    fn test_render_output_reports_no_pages_in_range() -> TestResult {
        let doc = document(vec![], Some(3));
        let output = render_output("a.pdf", &doc, None, None);
        match output {
            ToolOutput::Text { content } => {
                assert_eq!(content, "a.pdf hat nur 3 Seiten");
            }
            other => return Err(TestError::Unexpected(format!("unerwartet: {other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_render_output_builds_header_and_pages() -> TestResult {
        let doc = document(
            vec![
                ExtractedPage {
                    number: 1,
                    text: "eins".to_owned(),
                },
                ExtractedPage {
                    number: 2,
                    text: "zwei".to_owned(),
                },
            ],
            Some(2),
        );
        let output = render_output("a.pdf", &doc, None, None);
        match output {
            ToolOutput::Text { content } => {
                assert!(
                    content.starts_with("a.pdf — Seiten 1–2 von 2 (lokal)"),
                    "{content}"
                );
                assert!(content.contains("--- Seite 1 ---\neins"), "{content}");
                assert!(content.contains("--- Seite 2 ---\nzwei"), "{content}");
                assert!(!content.contains("gekürzt"), "{content}");
            }
            other => return Err(TestError::Unexpected(format!("unerwartet: {other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_render_output_includes_fallback_note() -> TestResult {
        let doc = document(
            vec![ExtractedPage {
                number: 1,
                text: "eins".to_owned(),
            }],
            Some(1),
        );
        let note = "[doc.read_pdf: Mistral OCR fehlgeschlagen (timeout), lokaler Rückfall]";
        let output = render_output("a.pdf", &doc, None, Some(note));
        match output {
            ToolOutput::Text { content } => {
                assert!(content.starts_with(note), "{content}");
            }
            other => return Err(TestError::Unexpected(format!("unerwartet: {other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_render_output_truncates_at_max_chars_with_hint() -> TestResult {
        let doc = document(
            vec![
                ExtractedPage {
                    number: 1,
                    text: "a".repeat(50),
                },
                ExtractedPage {
                    number: 2,
                    text: "b".repeat(50),
                },
            ],
            Some(2),
        );
        // Klein genug, dass Seite 2 nicht mehr passt, aber Seite 1 schon.
        let output = render_output("a.pdf", &doc, Some(60), None);
        match output {
            ToolOutput::Text { content } => {
                assert!(content.contains("Seite 1"), "{content}");
                assert!(!content.contains("Seite 2"), "{content}");
                assert!(content.contains("gekürzt"), "{content}");
                assert!(content.contains("pages=\"2-\""), "{content}");
            }
            other => return Err(TestError::Unexpected(format!("unerwartet: {other:?}"))),
        }
        Ok(())
    }

    // --- doc_read_pdf_spec ---------------------------------------------------

    #[test]
    fn test_spec_declares_path_required_and_advertises_all_params() -> TestResult {
        let ToolSpec::Function(spec) = DocReadPdfExecutor::spec();
        assert_eq!(spec.name.as_str(), "doc.read_pdf");
        assert_eq!(spec.parameters.required, Some(vec!["path".to_owned()]));
        let props = spec
            .parameters
            .properties
            .ok_or(TestError::Missing("properties"))?;
        for key in ["path", "pages", "max_chars", "backend"] {
            assert!(props.contains_key(key), "fehlendes Feld: {key}");
        }
        assert!(spec.description.contains("fs.read"), "{}", spec.description);
        Ok(())
    }

    // --- Executor-Konstanten -------------------------------------------------

    #[test]
    fn test_executor_constants() {
        assert_eq!(DocReadPdfExecutor::NAME, "doc.read_pdf");
        const _: () = assert!(DocReadPdfExecutor::PARALLEL_SAFE);
        assert_eq!(
            DocReadPdfExecutor::PERMISSION,
            Some(Permission::ReadWorkspace)
        );
    }
}
