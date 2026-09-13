//! `/compact` — Session-Kontext-Kompaktierungsoperation.
//!
//! # Verantwortungsbereich
//! Dieses Modul implementiert den `/compact`-Slash-Command (`CompactOperation`).
//! Die Operation initiiert die Kompaktierung der aktuellen Session-Historie:
//! Sie erstellt eine Zusammenfassung des bisherigen Konversationsverlaufs und
//! setzt den Turn-Kontext zurück, sodass nachfolgende Turns mit einem schlankeren
//! Kontext arbeiten können.
//!
//! Der eigentliche Session-Store-Aufruf und der Provider-Summarize-Call sind
//! noch nicht angebunden. Die Operation schlägt deshalb für jede Invocation
//! geschlossen mit [`OpError::NotAvailable`] fehl.
//!
//! # Schlüsseltypen
//! - [`CompactArgs`] — optionale Zusatzanweisung für den Kompaktierungs-Lauf.
//! - `CompactOperation` — generierter Unit-Struct (via `#[operation]`-Makro).
//!
//! # Nebenläufigkeit
//! `CompactOperation` ist `Send + Sync` (Unit-Struct ohne inneren Zustand,
//! generiert durch `#[operation]`-Makro).
//!
//! # Fehlertypen
//! Solange Session-Mutation und Provider-Summarization nicht angebunden sind,
//! wird jede Invocation mit [`OpError::NotAvailable`] abgewiesen.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::compact::CompactArgs;
//! // Die Operation wird über den harw-operations-Registry-Mechanismus aufgerufen.
//! // Direkte Nutzung über die generierte `CompactOperation`-Struct:
//! // let op = CompactOperation;
//! // let meta = op.meta();
//! // assert_eq!(meta.name, "compact");
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

// ── Args ──────────────────────────────────────────────────────────────────────

/// Argumente für die `/compact`-Operation.
///
/// # Beschreibung
/// Der Parameter [`CompactArgs::instruction`] ist optional und wird für die
/// spätere Kompaktierungs-Zusammenfassung aufbewahrt. Die derzeit nicht
/// verfügbare Operation verarbeitet ihn nicht.
///
/// # Deserialiserung
/// Das `#[operation]`-Makro deserialisiert `input.json_args` via `serde_json` in
/// diesen Typ. Bei `json_args == null` wird [`Default::default`] verwendet, was
/// `instruction = None` ergibt.
///
/// # Spec-Referenz
/// `/compact`-Command-Spec — Session-Governance, domain = "session",
/// permission = "operator".
///
/// # Beispiel
/// ```rust
/// use harw_ops::compact::CompactArgs;
///
/// let args: CompactArgs = serde_json::from_str(r#"{"instruction": "Fokus auf Fehler"}"#).unwrap();
/// assert_eq!(args.instruction.as_deref(), Some("Fokus auf Fehler"));
///
/// let default_args = CompactArgs::default();
/// assert!(default_args.instruction.is_none());
/// ```
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct CompactArgs {
    /// Optionale Zusatzanweisung für einen späteren
    /// Kompaktierungs-Zusammenfassungslauf.
    ///
    /// Sie wird derzeit nicht verarbeitet, da die Operation fail-closed
    /// abgewiesen wird.
    #[serde(default)]
    #[raw(join)]
    pub instruction: Option<String>,
}

// ── Operation ─────────────────────────────────────────────────────────────────

/// Komprimiert die aktuelle Session-Historie (Zusammenfassung + Reset des Turn-Kontexts).
///
/// # Beschreibung
/// Initiiert die Kompaktierung des laufenden Session-Kontexts. In der
/// Produktionsimplementierung wird diese Operation:
///
/// 1. Den aktuellen Konversationsverlauf aus dem Session-Store auslesen.
/// 2. Einen Provider-Summarize-Call ausführen, der eine kompakte Zusammenfassung
///    des bisherigen Verlaufs erstellt.
/// 3. Den Turn-Kontext zurücksetzen und die Zusammenfassung als neuen Startpunkt
///    einsetzen, sodass nachfolgende Turns mit weniger Token-Aufwand arbeiten.
///
/// Die Session-Store-Anbindung und der eigentliche Summarize-Call sind noch
/// nicht verfügbar. Daher wird die Operation fail-closed abgewiesen, statt
/// eine erfolgreiche Kompaktierung zu behaupten.
///
/// # Argumente
/// - `_ctx` (`&OpContext`): Ausführungskontext für die noch anzubindende
///   Session-Mutation und Provider-Summarization.
/// - `args` (`CompactArgs`): Optionale Zusatzanweisung für die spätere
///   Summarization; sie wird derzeit nicht verarbeitet.
///
/// # Rückgabe
/// - `Err(OpError::NotAvailable)`: Session-Mutation und Provider-Summarization
///   sind noch nicht verfügbar.
///
/// # Fehler
/// Die Operation gibt derzeit immer `Err(OpError::NotAvailable)` zurück. Die
/// optionale Zusatzanweisung wird nicht in die Fehlermeldung aufgenommen, da
/// sie sensible Inhalte enthalten kann.
///
/// # Nebenläufigkeit
/// Zustandslos; sicher aus mehreren Threads aufrufbar.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run() — direkte Verwendung nur im Test-Kontext.
/// ```
#[operation(
    name = "compact",
    summary = "Komprimiert die aktuelle Session-Historie (Zusammenfassung + Reset des Turn-Kontexts).",
    domain = "session",
    permission = "operator",
    command(path = "/compact", visibility = "channel_parity")
)]
async fn compact(_ctx: &OpContext, _args: CompactArgs) -> Result<OpOutput, OpError> {
    Err(OpError::NotAvailable(
        "session compaction is not available".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::CompactArgs;
    use crate::testutil::toks;
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_context() -> (OpContext, std::path::PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-compact-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("resolve workspace binding");
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        )
    }

    #[test]
    fn test_compact_args_from_raw_args_joins_tokens_with_space() {
        let args = CompactArgs::from_raw_args(&toks(&["a", "b"]));
        match args {
            Ok(a) => assert_eq!(a.instruction.as_deref(), Some("a b")),
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn test_compact_args_from_raw_args_empty_tokens_sets_instruction_none() {
        let args = CompactArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.instruction.is_none()),
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[tokio::test]
    async fn compact_without_hint_returns_not_available() {
        let (ctx, root) = test_context();

        let result = super::compact(&ctx, CompactArgs::default()).await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        assert!(
            matches!(result, Err(OpError::NotAvailable(message)) if message == "session compaction is not available")
        );
    }

    #[tokio::test]
    async fn compact_with_hint_returns_not_available_without_echoing_hint() {
        let (ctx, root) = test_context();
        let hint = "sensitive instruction that must not leak";

        let result = super::compact(
            &ctx,
            CompactArgs {
                instruction: Some(hint.to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Err(OpError::NotAvailable(message)) => assert!(!message.contains(hint)),
            other => panic!("expected NotAvailable, got: {other:?}"),
        }
    }
}
