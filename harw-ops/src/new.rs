//! Implementierung der `/new`-Operation — lehnt nicht verfügbare Session-Starts ab.
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
//! # Ausstehende Integration
//! Die eigentliche SessionManager-Verdrahtung (Turn-Counter-Reset, Kontext-
//! Flush, Session-ID-Vergabe und TUI/Core-Wechsel) liegt außerhalb des aktuellen
//! `OpContext`-Vertrags. Bis diese Boundary explizit verfügbar ist, schlägt der
//! Handler fail-closed fehl.
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
    summary = "Neue Session ist nicht verfügbar: OpContext hat keinen SessionManager.",
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
    use crate::testutil::toks;
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_context() -> (OpContext, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "harw-new-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("workspace")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .expect("resolve workspace binding");
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        )
    }

    #[test]
    fn test_new_args_from_raw_args_joins_tokens_with_space() {
        let args = NewArgs::from_raw_args(&toks(&["a", "b"]));
        match args {
            Ok(a) => assert_eq!(a.title.as_deref(), Some("a b")),
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn test_new_args_from_raw_args_empty_tokens_sets_title_none() {
        let args = NewArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.title.is_none()),
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[tokio::test]
    async fn new_without_title_returns_not_available() {
        let (ctx, root) = test_context();
        let result = new(&ctx, NewArgs::default()).await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        assert!(matches!(result, Err(OpError::NotAvailable(_))));
    }

    #[tokio::test]
    async fn new_with_title_returns_not_available_without_title_leakage() {
        let (ctx, root) = test_context();
        let title = "title-must-not-appear-in-error";
        let result = new(
            &ctx,
            NewArgs {
                title: Some(title.to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Err(OpError::NotAvailable(message)) => assert!(!message.contains(title)),
            other => panic!("expected NotAvailable, got: {other:?}"),
        }
    }
}
