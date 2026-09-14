//! `/agent` — Child-Agent-Management (list/stop gegen `ManagedAgentSpawner`).
//!
//! # Verantwortungsbereich
//! Implementiert die `agent`-Operation gemäß Harwness Plan v2.
//! Exponiert einen **Command** `/agent` mit `channel_parity`-Sichtbarkeit.
//! Das Modell darf diese Operation **nicht** selbst aufrufen — kein
//! `model_tool`-Attribut, da sich das Modell nicht selbst manipulieren darf.
//!
//! # Schlüsseltypen
//! - [`AgentArgs`] — deserialisierbare Eingabe-Argumente mit typisierten Feldern
//!   für Action (`list`, `stop`, `budget`), optionalem Ziel-Agent-ID und
//!   optionalem Wert (z. B. Budget-Grenze).
//! - `AgentOperation` — vom `#[operation]`-Makro erzeugter Implementations-Struct.
//!
//! # Nebenläufigkeit
//! `AgentOperation` ist ein Unit-Struct ohne inneren Zustand → `Send + Sync`.
//!
//! # Fehler
//! - [`harw_operations::OpError::InvalidArguments`]: wenn `json_args` nicht in
//!   [`AgentArgs`] deserialisiert werden kann (wird vom Makro gehandhabt).
//!
//! # Verfügbarkeit
//! `list` und `stop` lesen den `Arc<ManagedAgentSpawner>`-Service aus
//! [`OpContext`], falls die TUI-Kompositionswurzel einen konfiguriert hat.
//! Ohne registrierten Spawner liefert die Operation weiterhin fail-closed
//! [`OpError::NotAvailable`]. `budget` bleibt vorerst [`OpError::NotAvailable`].
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::agent::AgentArgs;
//!
//! let args = AgentArgs {
//!     action: Some("stop".to_owned()),
//!     target: Some("abc-42".to_owned()),
//!     value: None,
//! };
//! assert_eq!(args.action.as_deref(), Some("stop"));
//! assert_eq!(args.target.as_deref(), Some("abc-42"));
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

/// Eingabe-Argumente für die `agent`-Operation.
///
/// # Beschreibung
/// Trägt das optionale Sub-Kommando (`action`), das optionale Ziel-Agent-Objekt
/// (`target`, z. B. eine Agent-ID) sowie einen optionalen dritten Wert (`value`,
/// z. B. ein Budget-Limit).  Die Felder werden durch das [`harw_macros::FromRawArgs`]-Derive
/// direkt aus den tokenisierten TUI-Rohargumenten befüllt — jedes Feld erhält
/// genau das Token an der entsprechenden Position (0-basiert).
///
/// # Felder
/// - `action` (`Option<String>`): Token 0 — Sub-Kommando. Gültige Werte:
///   `"list"` (Standard), `"stop"`, `"budget"`. `None` wird intern als `"list"` behandelt.
/// - `target` (`Option<String>`): Token 1 — Ziel-Agent-ID (z. B. `"abc-42"`).
///   Relevant für `stop` und `budget`; bei `list` ignoriert.
/// - `value` (`Option<String>`): Token 2 — dritter Parameter (z. B. Budget-Grenze).
///   Relevant für `budget`; bei `list` und `stop` ignoriert.
///
/// # Verfügbarkeit
/// Validierung und Routing der Sub-Kommandos gegen den `ManagedAgentSpawner`
/// sind noch nicht verfügbar, weil die Boundary noch nicht in [`OpContext`]
/// verdrahtet ist. Die Argumente werden weiterhin geparst, aber nicht in die
/// Fehlermeldung übernommen.
///
/// # Beispiel
/// ```rust
/// use harw_ops::agent::AgentArgs;
/// use harw_operations::FromRawArgs;
///
/// // "/agent stop abc-42" → tokens = ["stop", "abc-42"]
/// let args = AgentArgs::from_raw_args(&["stop".to_owned(), "abc-42".to_owned()]).unwrap();
/// assert_eq!(args.action.as_deref(), Some("stop"));
/// assert_eq!(args.target.as_deref(), Some("abc-42"));
/// assert!(args.value.is_none());
/// ```
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct AgentArgs {
    /// Sub-Kommando: `"list"` (Standard), `"stop"`, `"budget"`. Token 0.
    #[serde(default)]
    #[raw(first)]
    pub action: Option<String>,
    /// Ziel-Agent-ID (z. B. `"abc-42"`). Token 1. `None` wenn nicht angegeben.
    #[serde(default)]
    #[raw(nth = 1)]
    pub target: Option<String>,
    /// Dritter Parameter (z. B. Budget-Grenze). Token 2. `None` wenn nicht angegeben.
    #[serde(default)]
    #[raw(nth = 2)]
    pub value: Option<String>,
}

/// Verweigert Child-Agent-Management, solange die Boundary nicht verfügbar ist.
///
/// # Beschreibung
/// `ManagedAgentSpawner` ist noch nicht in [`OpContext`] komponiert. Daher
/// werden alle erfolgreich geparsten Aufrufe fail-closed mit einem statischen
/// [`OpError::NotAvailable`] beendet; [`AgentArgs::action`],
/// [`AgentArgs::target`] und [`AgentArgs::value`] werden nicht ausgewertet.
///
/// **Command only**: Das Modell darf diese Operation nicht selbst aufrufen, da
/// sich das Modell nicht selbst manipulieren darf. Kein `model_tool`-Attribut.
///
/// # Argumente
/// - `_ctx` (`&OpContext`): Session-Kontext (aktuell ungenutzt).
/// - `_args` (`AgentArgs`): Typisierte Sub-Kommando-Argumente. `action`,
///   `target` und `value` werden bis zur Boundary-Integration nicht ausgewertet.
///
/// # Rückgabe
/// Immer [`OpError::NotAvailable`], solange die Child-Agent-Management-Boundary
/// nicht in [`OpContext`] verfügbar ist.
///
/// # Fehler
/// Gibt [`OpError::InvalidArguments`] zurück, wenn `json_args` nicht in
/// [`AgentArgs`] deserialisiert werden kann (wird vom Makro gehandhabt), oder
/// [`OpError::NotAvailable`] für jede erfolgreich geparste Invocation.
///
/// # Nebenläufigkeit
/// Zustandslos; keine Locks, keine Threads, kein geteilter Zustand.
///
/// # Ausstehend
/// Ein echter Body darf erst mit einer expliziten `ManagedAgentSpawner`-
/// Boundary in [`OpContext`] ergänzt werden.
///
/// # Beispiel
/// ```rust,no_run
/// // Wird indirekt über Operation::run aufgerufen.
/// ```
#[operation(
    name = "agent",
    summary = "Child-Agent-Management: list/stop gegen den registrierten ManagedAgentSpawner.",
    domain = "agents",
    permission = "operator",
    command(path = "/agent", visibility = "channel_parity")
)]
async fn agent(ctx: &OpContext, args: AgentArgs) -> Result<OpOutput, OpError> {
    let Some(spawner) = ctx.service::<std::sync::Arc<harw_core::ManagedAgentSpawner>>() else {
        return Err(OpError::NotAvailable(
            "child-agent management is not available".to_owned(),
        ));
    };

    match args.action.as_deref().unwrap_or("list") {
        "list" => {
            let children = spawner.list_children_for(ctx.session_id());
            if children.is_empty() {
                return Ok(OpOutput::from("Keine aktiven Child-Agents.".to_owned()));
            }
            let mut lines = vec![format!("{} aktive(r) Child-Agent(s):", children.len())];
            for record in &children {
                lines.push(format!(
                    "- {} (role={}, depth={}, lease_expires_at={})",
                    record.child, record.role, record.depth, record.lease_expires_at
                ));
            }
            Ok(OpOutput::from(lines.join("
")))
        }
        "stop" => {
            let Some(target) = args.target.as_deref() else {
                return Err(OpError::InvalidArguments(
                    "action 'stop' requires a target agent ID".to_owned(),
                ));
            };
            let child_id = harw_types::SessionId::from_str(target.to_owned());
            if spawner.request_cancellation(&child_id) {
                Ok(OpOutput::from(format!(
                    "Cancellation für Child-Agent {target} angefordert."
                )))
            } else {
                Err(OpError::InvalidArguments(format!(
                    "no admitted child agent found for target '{target}'"
                )))
            }
        }
        "budget" => Err(OpError::NotAvailable(
            "child-agent budget adjustment is not available".to_owned(),
        )),
        unknown => Err(OpError::InvalidArguments(format!(
            "unknown /agent action '{unknown}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::AgentArgs;
    use crate::testutil::toks;
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_context() -> (OpContext, std::path::PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-agent-test-{}-{id}", std::process::id()));
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
    fn test_agent_args_from_raw_args_sets_action() {
        let args = AgentArgs::from_raw_args(&toks(&["list"]));
        match args {
            Ok(a) => {
                assert_eq!(a.action.as_deref(), Some("list"));
                assert!(a.target.is_none());
                assert!(a.value.is_none());
            }
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn test_agent_args_from_raw_args_stop_preserves_target() {
        let args = AgentArgs::from_raw_args(&toks(&["stop", "abc-42"]));
        match args {
            Ok(a) => {
                assert_eq!(a.action.as_deref(), Some("stop"));
                assert_eq!(a.target.as_deref(), Some("abc-42"));
                assert!(a.value.is_none());
            }
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn test_agent_args_from_raw_args_budget_preserves_target_and_value() {
        let args = AgentArgs::from_raw_args(&toks(&["budget", "abc-42", "8k"]));
        match args {
            Ok(a) => {
                assert_eq!(a.action.as_deref(), Some("budget"));
                assert_eq!(a.target.as_deref(), Some("abc-42"));
                assert_eq!(a.value.as_deref(), Some("8k"));
            }
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn test_agent_args_from_raw_args_empty_tokens_sets_action_none() {
        let args = AgentArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => {
                assert!(a.action.is_none());
                assert!(a.target.is_none());
                assert!(a.value.is_none());
            }
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[tokio::test]
    async fn agent_default_and_list_return_not_available() {
        let (ctx, root) = test_context();
        let expected = "child-agent management is not available";
        assert!(matches!(
            super::agent(&ctx, AgentArgs::default()).await,
            Err(OpError::NotAvailable(message)) if message == expected
        ));
        assert!(matches!(
            super::agent(
                &ctx,
                AgentArgs {
                    action: Some("list".to_owned()),
                    target: None,
                    value: None,
                },
            )
            .await,
            Err(OpError::NotAvailable(message)) if message == expected
        ));
        std::fs::remove_dir_all(root).expect("remove test workspace");
    }

    #[tokio::test]
    async fn agent_budget_does_not_leak_target_or_value() {
        let (ctx, root) = test_context();
        let action = "budget";
        let target = "sensitive-agent-id";
        let value = "secret-budget";
        let result = super::agent(
            &ctx,
            AgentArgs {
                action: Some(action.to_owned()),
                target: Some(target.to_owned()),
                value: Some(value.to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert_eq!(message, "child-agent management is not available");
                assert!(!message.contains(action));
                assert!(!message.contains(target));
                assert!(!message.contains(value));
            }
            other => panic!("expected NotAvailable, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn agent_list_with_spawner_service_reports_no_active_children() {
        use harw_core::{ChildLimits, ManagedAgentSpawner, SessionManager};
        use harw_operations::context::ServiceMap;
        use std::sync::{Arc, Mutex};

        let (ctx, root) = test_context();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(event_tx)));
        let spawner = Arc::new(ManagedAgentSpawner::new(manager, ChildLimits::conservative()));

        let mut services = ServiceMap::new();
        services.insert(spawner);
        let ctx = OpContext::new(
            ctx.session_id().clone(),
            ctx.turn_id().clone(),
            ctx.sandbox().clone(),
            services,
        );

        let result = super::agent(&ctx, AgentArgs::default()).await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Ok(output) => assert!(output.text.contains("Keine aktive")),
            other => panic!("expected Ok empty listing, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn agent_stop_with_spawner_service_and_unknown_target_is_invalid_argument() {
        use harw_core::{ChildLimits, ManagedAgentSpawner, SessionManager};
        use harw_operations::context::ServiceMap;
        use std::sync::{Arc, Mutex};

        let (ctx, root) = test_context();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(event_tx)));
        let spawner = Arc::new(ManagedAgentSpawner::new(manager, ChildLimits::conservative()));

        let mut services = ServiceMap::new();
        services.insert(spawner);
        let ctx = OpContext::new(
            ctx.session_id().clone(),
            ctx.turn_id().clone(),
            ctx.sandbox().clone(),
            services,
        );

        let result = super::agent(
            &ctx,
            AgentArgs {
                action: Some("stop".to_owned()),
                target: Some("unknown-child".to_owned()),
                value: None,
            },
        )
        .await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
    }
}

