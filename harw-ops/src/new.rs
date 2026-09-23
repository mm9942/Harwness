//! `/new`-Operation: Katalogeintrag und Rückfall für Flächen ohne Sitzungswechsel.
//!
//! # Verantwortlichkeit
//! Dieses Modul registriert den `/new`-Command als reine Governance-Operation
//! (kein ModelTool). Es parst optional einen Titel/Alias für eine neue Session,
//! kann aber keine Session erzeugen oder in sie wechseln.
//!
//! # Schlüsseltypen
//! - [`NewArgs`] — deserialisierbare Argumente des Commands
//! - `new` — die asynchrone Handler-Funktion, registriert via `#[operation]`
//!
//! # Nebenläufigkeit
//! Die Funktion ist zustandslos und `Send + Sync`-kompatibel. Keine Locks,
//! keine Threads.
//!
//! # Fehlertypen
//! [`OpError::NotAvailable`], weil [`OpContext`] keine `SessionManager`-Boundary
//! für das Erzeugen oder Wechseln einer TUI/Core-Session bereitstellt.
//!
//! # Wo `/new` wirklich wirkt
//! Die TUI fängt `/new` vor der Operations-Pipeline ab und montiert eine
//! frische Wurzelsitzung (`TuiRunOutcome::NewSession`, gleicher Austausch
//! wie `/resume`). Flächen ohne lebende Sitzung (Web, Telegram, Jobs)
//! erreichen diesen Handler und bekommen weiterhin `NotAvailable`.
//!
//! # Beispiel
//! ```no_run
//! // Wird intern vom Harwness-Dispatcher aufgerufen, nicht direkt.
//! // Nutzer-Input: /new --title "Mein Projekt"
//! // Ausgabe: OpError::NotAvailable
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

/// Argumente für den `/new`-Command.
///
/// # Felder
/// - `title` (`Option<String>`): Optionaler Anzeigename / Alias für die neue
///   Session. Der Wert wird weiterhin geparst, aber nicht verwendet, solange
///   die SessionManager-Boundary nicht verfügbar ist.
///
/// # Serde
/// Alle Felder haben `#[serde(default)]`, sodass ein leerer JSON-Body `{}`
/// oder ein fehlender Body korrekt deserialisiert wird.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct NewArgs {
    /// Optionaler Titel/Alias für die neue Session.
    #[serde(default)]
    #[raw(join)]
    pub title: Option<String>,
}

/// Fordert den Start einer neuen Harwness-Session an.
///
/// # Beschreibung
/// Der Command ist als reine Governance-Operation klassifiziert (`permission =
/// "operator"`). Er liegt **außerhalb des Modell-Wirkbereichs** — Session-Start
/// ist eine Plattform-Entscheidung, kein LLM-Auftrag.
///
/// Der aktuelle [`OpContext`] stellt keine `SessionManager`-Boundary bereit. Der
/// Handler kann deshalb weder eine TUI/Core-Session erstellen noch in sie
/// wechseln und lehnt jede Invocation mit [`OpError::NotAvailable`] ab. Diese
/// fail-closed Antwort enthält keinen benutzerkontrollierten Session-Titel.
///
/// # Argumente
/// - `_ctx` (`&OpContext`): Operator-Kontext ohne verfügbare SessionManager-Boundary.
/// - `args` (`NewArgs`): Deserialisierte Command-Argumente; enthält den
///   optionalen Session-Titel.
///
/// # Rückgabe
/// Immer `Err(OpError::NotAvailable)`.
///
/// # Fehler
/// [`OpError::NotAvailable`], solange der Kontext keine SessionManager-Boundary
/// für das Erzeugen und Wechseln von Sessions bereitstellt.
///
/// # Nebenläufigkeit
/// Zustandslos; sicher für parallele Aufrufe aus mehreren Threads.
///
/// # Ausstehend
/// SessionManager-Anbindung: Turn-Counter-Reset, Kontext-Flush,
/// Session-ID-Vergabe und TUI/Core-Sessionwechsel.
///
/// # Beispiel
/// ```no_run
/// // Wird vom Harwness-Dispatcher aufgerufen:
/// // /new --title "Sprint 42"
/// // → OpError::NotAvailable
/// ```
#[operation(
    name = "new",
    summary = "Startet eine neue Sitzung (in der TUI; andere Flächen: nicht verfügbar).",
    domain = "session",
    permission = "operator",
    command(path = "/new", visibility = "channel_parity")
)]
async fn new(_ctx: &OpContext, _args: NewArgs) -> Result<OpOutput, OpError> {
    Err(OpError::NotAvailable(
        "session manager is not available in the operation context".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{NewArgs, new};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_context() -> TestResult<(OpContext, PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "harw-new-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        ))
    }

    #[test]
    fn test_new_args_from_raw_args_joins_tokens_with_space() -> TestResult {
        let args =
            NewArgs::from_raw_args(&toks(&["a", "b"])).map_err(ctx("NewArgs::from_raw_args"))?;
        assert_eq!(args.title.as_deref(), Some("a b"));
        Ok(())
    }

    #[test]
    fn test_new_args_from_raw_args_empty_tokens_sets_title_none() -> TestResult {
        let args = NewArgs::from_raw_args(&toks(&[])).map_err(ctx("NewArgs::from_raw_args"))?;
        assert!(args.title.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn new_without_title_returns_not_available() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let result = new(&op_ctx, NewArgs::default()).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        assert!(matches!(result, Err(OpError::NotAvailable(_))));
        Ok(())
    }

    #[tokio::test]
    async fn new_with_title_returns_not_available_without_title_leakage() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let title = "title-must-not-appear-in-error";
        let result = new(
            &op_ctx,
            NewArgs {
                title: Some(title.to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(!message.contains(title));
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "expected NotAvailable, got: {other:?}"
            ))),
        }
    }
}
