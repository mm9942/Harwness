//! `/export` — fordert einen Export der Sitzung an (keine Datei-E/A hier).
//!
//! Spec-Quelle: Contract `harw-scopes-contract.md`, „Nachträgliche
//! Entscheidungen" (Slice E1: Auswahl „in die Zwischenablage kopieren" /
//! „als Datei speichern" / „abbrechen"), Slice B4.
//!
//! # Verantwortung
//! Diese Operation schreibt **keine** Datei und kopiert **nicht** in die
//! Zwischenablage — beides gehört der TUI (`harw_tui::export`,
//! `harw_tui::clipboard`; siehe deren Moduldoku: „kennt keine `HistoryCell`-
//! oder `app.rs`-Typen — die spätere Verdrahtung übersetzt den Verlauf in
//! `ExportEntry`-Werte"). Diese Operation parst nur
//! `/export [--format md|markdown|json] [--tools|--no-tools]
//! [--reasoning-summary] [--datei <pfad>]` und liefert einen maschinenlesbaren
//! Marker über [`OpOutput::data`], den die künftige `app.rs`-Verdrahtung
//! abfängt, um den Auswahl-Dialog zu öffnen bzw. direkt in die genannte
//! Datei zu schreiben.
//!
//! # Vertrag für die TUI-Verdrahtung (für die Slice, die `app.rs` verkabelt)
//! `OpOutput.data` ist bei Erfolg immer `Some(value)` mit der Form:
//!
//! ```json
//! {
//!   "kind": "export.request",
//!   "format": "markdown",
//!   "include_tool_calls": true,
//!   "include_reasoning_summary": false,
//!   "path": null
//! }
//! ```
//!
//! - `kind` ist der stabile Diskriminator, den `app.rs` abfragt. Das ist die
//!   strukturierte Variante desselben Musters, das `app.rs` heute für
//!   `/goal check` verwendet (`is_goal_check_command`/`goal_cell_for_command`,
//!   `harw-tui/src/app.rs`): dort erkennt `app.rs` die rohe Befehlszeile
//!   direkt, weil `/goal check` keine Argumente mit Leerzeichen trägt.
//!   `/export --datei <pfad>` dagegen kann Leerzeichen im Pfad enthalten,
//!   die aus reinem Text nicht mehr eindeutig zurückzugewinnen wären — daher
//!   liefert diese Operation den Marker strukturiert über `data`, statt dass
//!   `app.rs` die rohe Zeile erneut parsen müsste.
//! - `format` ist der kanonische Ausgabeformatname (`"markdown"` oder
//!   `"json"`). Die Eingabe-Aliasform `md` wird als `"markdown"`
//!   transportiert, damit der Marker eindeutig bleibt.
//! - `include_tool_calls` entspricht 1:1
//!   `harw_tui::export::ExportOptions::include_tool_calls` und ist für
//!   vollständige Transkripte standardmäßig `true`. `--tools` bleibt als
//!   Legacy-Schalter akzeptiert; `--no-tools` ist der explizite Opt-out.
//! - `include_reasoning_summary` darf nur sichere, vom Provider gelieferte
//!   Summary-Texte anfordern. Der Marker bietet absichtlich keine Option für
//!   rohe Chain-of-Thought-/Denkschritt-Daten.
//! - `path`: `Some(pfad)` bei `--datei <pfad>` (roh, wie eingegeben — weder
//!   kanonisiert noch auf Traversal geprüft; das übernimmt
//!   `harw_tui::export::write_export`), sonst `None`. Ist `path` `None`,
//!   zeigt die TUI den Auswahl-Dialog „in die Zwischenablage kopieren" /
//!   „als Datei speichern" / „abbrechen"; ist `path` gesetzt, überspringt sie
//!   den Dialog und schreibt direkt dorthin.
//!
//! `--reasoning-summary` ist bewusst enger als eine generische
//! `--reasoning`-Option: Die spätere TUI-Verdrahtung darf dafür ausschließlich
//! bereits vorliegende Summary-Texte verwenden und niemals rohe
//! Chain-of-Thought-Daten exportieren.

use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};

/// Ausgabeformat des angeforderten Exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub enum ExportFormat {
    /// Menschenlesbares Markdown; `md` und `markdown` sind Eingabe-Aliase.
    #[serde(rename = "markdown", alias = "md")]
    Markdown,
    /// Strukturiertes JSON.
    #[serde(rename = "json")]
    Json,
}

impl Default for ExportFormat {
    fn default() -> Self {
        Self::Markdown
    }
}

impl ExportFormat {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "md" | "markdown" => Some(Self::Markdown),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    fn marker_name(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }
}

fn default_include_tool_calls() -> bool {
    true
}

/// Argumente für `/export`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ExportArgs {
    /// `--format md|markdown|json`: Ausgabeformat (Standard: Markdown).
    #[serde(default)]
    pub format: ExportFormat,
    /// `--tools` bzw. `--no-tools`: Werkzeugaufrufe ein-/ausschließen.
    /// Standardmäßig eingeschlossen, damit der Export ein vollständiges
    /// Transkript enthält.
    #[serde(default = "default_include_tool_calls")]
    pub include_tool_calls: bool,
    /// `--reasoning-summary`: sichere Provider-Summaries einschließen.
    /// Rohe Chain-of-Thought-Daten sind ausdrücklich nicht gemeint.
    #[serde(default)]
    pub include_reasoning_summary: bool,
    /// `--datei <pfad>`: Zielpfad, roh wie eingegeben. `None` bedeutet, dass
    /// die TUI den Auswahl-Dialog zeigen soll.
    #[serde(default)]
    pub path: Option<String>,
}

impl Default for ExportArgs {
    fn default() -> Self {
        Self {
            format: ExportFormat::default(),
            include_tool_calls: true,
            include_reasoning_summary: false,
            path: None,
        }
    }
}

impl FromRawArgs for ExportArgs {
    /// Parst die `/export`-Optionen in beliebiger Reihenfolge.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`]: fehlender Wert für `--format` oder
    /// `--datei`, ein nicht unterstütztes Format oder ein unbekanntes Token.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        let mut format = ExportFormat::default();
        let mut include_tool_calls = true;
        let mut include_reasoning_summary = false;
        let mut path = None;
        let mut index = 0;
        while index < tokens.len() {
            match tokens[index].as_str() {
                "--format" => {
                    let value = tokens.get(index + 1).ok_or_else(|| {
                        OpError::InvalidArguments(
                            "/export --format <md|markdown|json> braucht ein Format".to_owned(),
                        )
                    })?;
                    format = ExportFormat::parse(value).ok_or_else(|| {
                        OpError::InvalidArguments(format!(
                            "/export: unbekanntes Format '{value}' (erlaubt: md, markdown, json)"
                        ))
                    })?;
                    index += 2;
                }
                "--tools" => {
                    include_tool_calls = true;
                    index += 1;
                }
                "--no-tools" => {
                    include_tool_calls = false;
                    index += 1;
                }
                "--reasoning-summary" => {
                    include_reasoning_summary = true;
                    index += 1;
                }
                "--datei" => {
                    let value = tokens.get(index + 1).ok_or_else(|| {
                        OpError::InvalidArguments(
                            "/export --datei <pfad> braucht einen Pfad".to_owned(),
                        )
                    })?;
                    path = Some(value.clone());
                    index += 2;
                }
                other => {
                    return Err(OpError::InvalidArguments(format!(
                        "/export: unbekanntes Argument '{other}' (Usage: /export [--format md|markdown|json] [--tools|--no-tools] [--reasoning-summary] [--datei <pfad>])"
                    )));
                }
            }
        }
        Ok(Self {
            format,
            include_tool_calls,
            include_reasoning_summary,
            path,
        })
    }
}

/// Fordert einen Export des Sitzungsverlaufs an; Dialog und Schreiben
/// übernimmt die TUI.
///
/// # Beschreibung
/// Diese Operation greift nicht auf den Verlauf, die Sandbox oder das
/// Dateisystem zu — sie übersetzt nur die Argumente in den in der Moduldoku
/// beschriebenen `data`-Marker. Sie kann daher nie mit
/// [`OpError::NotAvailable`] oder [`OpError::Execution`] fehlschlagen.
///
/// # Arguments
/// - `_ctx` (`&OpContext`): ungenutzt (keine Dienste erforderlich).
/// - `args` ([`ExportArgs`]): geparste Argumente.
///
/// # Returns
/// `Ok(OpOutput)` mit `data = Some({"kind": "export.request", …})` (siehe
/// Moduldoku) und einem menschenlesbaren `text` als Fallback für Flächen
/// ohne strukturierte Auswertung.
///
/// # Errors
/// Keine — außer der bereits von [`ExportArgs::from_raw_args`] gemeldeten
/// [`OpError::InvalidArguments`] bei ungültigen Tokens.
#[operation(
    name = "export",
    summary = "Fordert einen Export des Sitzungsverlaufs an; die TUI führt Dialog und Schreiben aus.",
    domain = "session",
    permission = "operator",
    command(path = "/export", visibility = "tui_only")
)]
async fn export(_ctx: &OpContext, args: ExportArgs) -> Result<OpOutput, OpError> {
    let data = serde_json::json!({
        "kind": "export.request",
        "format": args.format.marker_name(),
        "include_tool_calls": args.include_tool_calls,
        "include_reasoning_summary": args.include_reasoning_summary,
        "path": args.path,
    });
    let text = match &args.path {
        Some(path) => format!(
            "Export angefordert: Datei {path} (Format: {}, Werkzeugaufrufe: {}, Reasoning-Summary: {}).",
            args.format.marker_name(),
            if args.include_tool_calls { "ja" } else { "nein" },
            if args.include_reasoning_summary { "ja" } else { "nein" },
        ),
        None => format!(
            "Export angefordert: Zielauswahl folgt (Format: {}, Zwischenablage/Datei/abbrechen).",
            args.format.marker_name()
        ),
    };
    Ok(OpOutput {
        text,
        data: Some(data),
    })
}

#[cfg(test)]
mod tests {
    use super::{ExportArgs, ExportFormat, export};
    use crate::testutil::toks;
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_context() -> OpContext {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-export-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: std::path::PathBuf::from("workspace"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .expect("resolve workspace binding");
        OpContext::new(
            SessionId::new(),
            TurnId::new(),
            SandboxSpec::from_resolved(binding, PermissionSet::empty()),
            ServiceMap::new(),
        )
    }

    #[test]
    fn test_export_args_from_raw_args_defaults_to_markdown_with_tools_and_no_path() {
        let args = ExportArgs::from_raw_args(&toks(&[])).expect("parse");
        assert_eq!(args, ExportArgs::default());
        assert_eq!(args.format, ExportFormat::Markdown);
        assert!(args.include_tool_calls);
        assert!(!args.include_reasoning_summary);
        assert!(args.path.is_none());
    }

    #[test]
    fn test_export_args_json_without_new_fields_remains_compatible() {
        let args: ExportArgs = serde_json::from_value(serde_json::json!({
            "include_tool_calls": false,
            "path": "/tmp/legacy.md"
        }))
        .expect("deserialize legacy arguments");
        assert_eq!(args.format, ExportFormat::Markdown);
        assert!(!args.include_tool_calls);
        assert!(!args.include_reasoning_summary);
        assert_eq!(args.path.as_deref(), Some("/tmp/legacy.md"));
    }

    #[test]
    fn test_export_args_from_raw_args_parses_all_formats_and_canonicalizes_md() {
        let markdown = ExportArgs::from_raw_args(&toks(&["--format", "markdown"]))
            .expect("parse markdown");
        assert_eq!(markdown.format, ExportFormat::Markdown);

        let md = ExportArgs::from_raw_args(&toks(&["--format", "md"]))
            .expect("parse md");
        assert_eq!(md.format, ExportFormat::Markdown);

        let json = ExportArgs::from_raw_args(&toks(&["--format", "json"]))
            .expect("parse json");
        assert_eq!(json.format, ExportFormat::Json);
    }

    #[test]
    fn test_export_args_from_raw_args_parses_tools_flag() {
        let args = ExportArgs::from_raw_args(&toks(&["--tools"])).expect("parse");
        assert!(args.include_tool_calls);
        assert!(args.path.is_none());
    }

    #[test]
    fn test_export_args_from_raw_args_parses_explicit_tool_opt_out() {
        let args = ExportArgs::from_raw_args(&toks(&["--no-tools"])).expect("parse");
        assert!(!args.include_tool_calls);

        let args = ExportArgs::from_raw_args(&toks(&["--no-tools", "--tools"])).expect("parse");
        assert!(args.include_tool_calls);
    }

    #[test]
    fn test_export_args_from_raw_args_only_accepts_safe_reasoning_summary() {
        let args = ExportArgs::from_raw_args(&toks(&["--reasoning-summary"])).expect("parse");
        assert!(args.include_reasoning_summary);

        let result = ExportArgs::from_raw_args(&toks(&["--reasoning"]));
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
    }

    #[test]
    fn test_export_args_from_raw_args_parses_datei_with_path() {
        let args = ExportArgs::from_raw_args(&toks(&["--datei", "/tmp/export.md"])).expect("parse");
        assert_eq!(args.path.as_deref(), Some("/tmp/export.md"));
    }

    #[test]
    fn test_export_args_from_raw_args_accepts_both_flags_in_either_order() {
        let args = ExportArgs::from_raw_args(&toks(&["--datei", "/tmp/x.md", "--tools"])).expect("parse");
        assert!(args.include_tool_calls);
        assert_eq!(args.path.as_deref(), Some("/tmp/x.md"));

        let args = ExportArgs::from_raw_args(&toks(&["--tools", "--datei", "/tmp/y.md"])).expect("parse");
        assert!(args.include_tool_calls);
        assert_eq!(args.path.as_deref(), Some("/tmp/y.md"));
    }

    #[test]
    fn test_export_args_from_raw_args_datei_without_path_is_invalid() {
        let result = ExportArgs::from_raw_args(&toks(&["--datei"]));
        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("--datei")),
            other => panic!("expected invalid arguments, got {other:?}"),
        }
    }

    #[test]
    fn test_export_args_from_raw_args_format_without_value_is_invalid() {
        let result = ExportArgs::from_raw_args(&toks(&["--format"]));
        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("--format")),
            other => panic!("expected invalid arguments, got {other:?}"),
        }
    }

    #[test]
    fn test_export_args_from_raw_args_rejects_unknown_format() {
        let result = ExportArgs::from_raw_args(&toks(&["--format", "xml"]));
        match result {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("xml"));
                assert!(message.contains("markdown"));
            }
            other => panic!("expected invalid arguments, got {other:?}"),
        }
    }

    #[test]
    fn test_export_args_from_raw_args_rejects_unknown_token() {
        let result = ExportArgs::from_raw_args(&toks(&["--wat"]));
        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("--wat")),
            other => panic!("expected invalid arguments, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn export_without_path_returns_request_marker_with_null_path() {
        let ctx = test_context();
        let output = export(&ctx, ExportArgs::default()).await.expect("export");
        let data = output.data.expect("data marker must be present");
        assert_eq!(data["kind"], serde_json::json!("export.request"));
        assert_eq!(data["format"], serde_json::json!("markdown"));
        assert_eq!(data["include_tool_calls"], serde_json::json!(true));
        assert_eq!(data["include_reasoning_summary"], serde_json::json!(false));
        assert_eq!(data["path"], serde_json::Value::Null);
        assert!(output.text.contains("Zielauswahl"));
        assert!(output.text.contains("markdown"));
    }

    #[tokio::test]
    async fn export_with_path_and_tools_returns_matching_marker() {
        let ctx = test_context();
        let args = ExportArgs {
            format: ExportFormat::Json,
            include_tool_calls: true,
            include_reasoning_summary: true,
            path: Some("/tmp/export.md".to_owned()),
        };
        let output = export(&ctx, args).await.expect("export");
        let data = output.data.expect("data marker must be present");
        assert_eq!(data["kind"], serde_json::json!("export.request"));
        assert_eq!(data["format"], serde_json::json!("json"));
        assert_eq!(data["include_tool_calls"], serde_json::json!(true));
        assert_eq!(data["include_reasoning_summary"], serde_json::json!(true));
        assert_eq!(data["path"], serde_json::json!("/tmp/export.md"));
        assert!(output.text.contains("/tmp/export.md"));
        assert!(output.text.contains("json"));
        assert!(output.text.contains("ja"));
    }
}
