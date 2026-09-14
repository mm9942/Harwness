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
//! `/export [--tools] [--datei <pfad>]` und liefert einen maschinenlesbaren
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
//!   "include_tool_calls": false,
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
//! - `include_tool_calls` entspricht 1:1
//!   `harw_tui::export::ExportOptions::include_tool_calls`.
//! - `path`: `Some(pfad)` bei `--datei <pfad>` (roh, wie eingegeben — weder
//!   kanonisiert noch auf Traversal geprüft; das übernimmt
//!   `harw_tui::export::write_export`), sonst `None`. Ist `path` `None`,
//!   zeigt die TUI den Auswahl-Dialog „in die Zwischenablage kopieren" /
//!   „als Datei speichern" / „abbrechen"; ist `path` gesetzt, überspringt sie
//!   den Dialog und schreibt direkt dorthin.
//!
//! `args.include_reasoning` ist bewusst **kein** Argument dieser Operation:
//! Denkschritte offenzulegen ist eine sensiblere Entscheidung als
//! Werkzeugaufrufe und bleibt vorerst ausschließlich der TUI-Dialogseite
//! vorbehalten (`harw_tui::export::ExportOptions::include_reasoning`, dort
//! Default `false`).

use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};

/// Argumente für `/export`.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ExportArgs {
    /// `--tools`: Werkzeugaufrufe im Export einschließen.
    #[serde(default)]
    pub include_tool_calls: bool,
    /// `--datei <pfad>`: Zielpfad, roh wie eingegeben. `None` bedeutet, dass
    /// die TUI den Auswahl-Dialog zeigen soll.
    #[serde(default)]
    pub path: Option<String>,
}

impl FromRawArgs for ExportArgs {
    /// Parst `--tools` und `--datei <pfad>` in beliebiger Reihenfolge.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`]: `--datei` ohne folgenden Pfad, oder ein
    /// unbekanntes Token.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        let mut include_tool_calls = false;
        let mut path = None;
        let mut index = 0;
        while index < tokens.len() {
            match tokens[index].as_str() {
                "--tools" => {
                    include_tool_calls = true;
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
                        "/export: unbekanntes Argument '{other}' (erlaubt: --tools, --datei <pfad>)"
                    )));
                }
            }
        }
        Ok(Self {
            include_tool_calls,
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
        "include_tool_calls": args.include_tool_calls,
        "path": args.path,
    });
    let text = match &args.path {
        Some(path) => format!(
            "Export angefordert: Datei {path} (Werkzeugaufrufe: {}).",
            if args.include_tool_calls { "ja" } else { "nein" }
        ),
        None => "Export angefordert: Zielauswahl folgt (Zwischenablage/Datei/abbrechen).".to_owned(),
    };
    Ok(OpOutput {
        text,
        data: Some(data),
    })
}

#[cfg(test)]
mod tests {
    use super::{ExportArgs, export};
    use crate::testutil::toks;
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
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
    fn test_export_args_from_raw_args_defaults_are_off_and_no_path() {
        let args = ExportArgs::from_raw_args(&toks(&[])).expect("parse");
        assert_eq!(args, ExportArgs::default());
        assert!(!args.include_tool_calls);
        assert!(args.path.is_none());
    }

    #[test]
    fn test_export_args_from_raw_args_parses_tools_flag() {
        let args = ExportArgs::from_raw_args(&toks(&["--tools"])).expect("parse");
        assert!(args.include_tool_calls);
        assert!(args.path.is_none());
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
        assert_eq!(data["include_tool_calls"], serde_json::json!(false));
        assert_eq!(data["path"], serde_json::Value::Null);
        assert!(output.text.contains("Zielauswahl"));
    }

    #[tokio::test]
    async fn export_with_path_and_tools_returns_matching_marker() {
        let ctx = test_context();
        let args = ExportArgs {
            include_tool_calls: true,
            path: Some("/tmp/export.md".to_owned()),
        };
        let output = export(&ctx, args).await.expect("export");
        let data = output.data.expect("data marker must be present");
        assert_eq!(data["kind"], serde_json::json!("export.request"));
        assert_eq!(data["include_tool_calls"], serde_json::json!(true));
        assert_eq!(data["path"], serde_json::json!("/tmp/export.md"));
        assert!(output.text.contains("/tmp/export.md"));
        assert!(output.text.contains("ja"));
    }
}
