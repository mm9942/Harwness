//! `agent.cancel` (und die gemeinsamen Namen/Argumente mit `agent.status`)
//! — Steuerung der Hintergrund-Agenten (Runde 5, Teil K). `agent.status`
//! selbst lebt in `crate::agent_status` (eigene Erweiterungsstelle).
//!
//! # Verantwortungsbereich
//! Ein Orchestrator, den die UIA-Wurzel in der TUI startet, läuft im
//! Hintergrund weiter (siehe `harw_core::background_children`). Die UIA
//! braucht dafür zwei Werkzeuge:
//!
//! ```text
//! agent.status { child_id? }   // lesend, auto-freigegeben
//! agent.cancel { child_id }    // braucht immer eine Freigabe
//! ```
//!
//! `agent.status` listet laufende und zuletzt beendete Hintergrund-Läufe der
//! **aufrufenden** Sitzung mit Fortschritt (Werkzeugaufrufe, Tokens, Laufzeit,
//! letzter Schritt). `agent.cancel` bricht einen eigenen, laufenden
//! Hintergrund-Lauf ab. Das Ergebnis eines beendeten Laufs liefert weiterhin
//! `agent.result`.
//!
//! # Sicherheit
//! Die aufrufende Sitzung kommt aus dem Ausführungskontext
//! (`OpContext::session_id`), nie aus Modell-Argumenten. Fremde und
//! unbekannte Kind-IDs bekommen dieselbe Meldung. `agent.cancel` deklariert
//! `approval = always` und steht in `ALWAYS_ASK_TOOLS` — nie automatisch.
//!
//! # Nebenläufigkeit
//! Zustandslos; beide Operationen lesen nur kurz das Register des Spawners.

use std::sync::OnceLock;

use harw_operations::context::OpContext;
use harw_operations::error::OpError;
use harw_operations::op_schema::{object_schema, string_schema};
use harw_operations::operation::{
    ApprovalPolicy, ArgsSchemaFn, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
    OperationDomain, OperationMeta, PermissionTier, Surface,
};
use serde_json::{Map, Value, json};

use crate::context_ext::OpContextCoreExt;

/// Name des lesenden Werkzeugs.
pub const AGENT_STATUS_TOOL: &str = "agent.status";

/// Name des abbrechenden Werkzeugs.
pub const AGENT_CANCEL_TOOL: &str = "agent.cancel";

/// Beide Werkzeugnamen (Montage und Rechte-Tabellen).
pub const AGENT_BACKGROUND_TOOLS: &[&str] = &[AGENT_STATUS_TOOL, AGENT_CANCEL_TOOL];

/// Liest das optionale bzw. Pflichtfeld `child_id` (geschlossenes Schema).
pub(crate) fn parse_child_id(
    tool: &str,
    args: &Value,
    required: bool,
) -> Result<Option<String>, OpError> {
    let invalid = |message: &str| OpError::InvalidArguments(format!("{tool}: {message}"));
    let object: &Map<String, Value> = args
        .as_object()
        .ok_or_else(|| invalid("expects an object {child_id}"))?;
    if let Some(key) = object.keys().find(|key| key.as_str() != "child_id") {
        return Err(invalid(&format!(
            "does not accept the field `{key}`; allowed is `child_id`"
        )));
    }
    let child_id = match object.get("child_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if !id.trim().is_empty() => Some(id.trim().to_owned()),
        Some(_) => return Err(invalid("`child_id` must be a non-empty string or null")),
    };
    if required && child_id.is_none() {
        return Err(invalid("`child_id` is required"));
    }
    Ok(child_id)
}

/// Führt `agent.cancel` aus.
///
/// # Errors
/// [`OpError::NotAvailable`] ohne Spawner oder für eine fremde/unbekannte
/// `child_id` (immer dieselbe Meldung).
pub fn agent_cancel(ctx: &OpContext, child_id: &str) -> Result<OpOutput, OpError> {
    let spawner = ctx.managed_spawner().ok_or_else(|| {
        OpError::NotAvailable("kein Agent-Spawner in diesem Kontext konfiguriert".to_owned())
    })?;
    let requested = spawner
        .cancel_background_child(ctx.session_id(), child_id)
        .map_err(|error| {
            OpError::NotAvailable(format!("{AGENT_CANCEL_TOOL}: {}", error.message))
        })?;
    let text = if requested {
        format!("Abbruch des Hintergrund-Agenten {child_id} angefordert.")
    } else {
        format!("Der Hintergrund-Agent {child_id} läuft nicht mehr.")
    };
    Ok(OpOutput {
        text,
        data: Some(json!({ "child_id": child_id, "cancelled": requested })),
    })
}

/// Die Operation `agent.cancel` als Modell-Werkzeug (immer mit Freigabe).
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentCancelOperation;

const AGENT_CANCEL_ARGS_SCHEMA: ArgsSchemaFn = || {
    object_schema(
        vec![(
            "child_id",
            string_schema("ID des eigenen, laufenden Hintergrund-Agenten."),
        )],
        &["child_id"],
    )
};

impl Operation for AgentCancelOperation {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: AGENT_CANCEL_TOOL,
            summary: "Bricht einen eigenen, laufenden Hintergrund-Agenten ab (braucht immer \
                      eine Freigabe).",
            domain: OperationDomain::Agents,
            permission: PermissionTier::Operator,
            surfaces: vec![Surface::ModelTool {
                readonly: false,
                approval: ApprovalPolicy::Always,
            }],
            category: OperationCategory::Agent,
            args_schema: Some(AGENT_CANCEL_ARGS_SCHEMA),
            ..OperationMeta::default()
        })
    }

    fn run<'a>(&'a self, ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
        Box::pin(async move {
            if !input.invocation.is_model_tool() {
                return Err(OpError::InvalidArguments(format!(
                    "{AGENT_CANCEL_TOOL}: is only available as a model tool"
                )));
            }
            let child_id = parse_child_id(AGENT_CANCEL_TOOL, input.invocation.json_args(), true)?
                .unwrap_or_default();
            agent_cancel(ctx, &child_id)
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_core::child_controller::ManagedAgentSpawner;
    use harw_core::{ChildLimits, InMemoryStateStore, SessionManager, StateStore};
    use harw_operations::context::{OpContext, ServiceMap};
    use harw_operations::error::OpError;
    use harw_operations::operation::{ApprovalPolicy, OpInput, Operation, Surface};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use serde_json::json;

    use super::{AgentCancelOperation, parse_child_id};
    use crate::agent_status::AgentStatusOperation;
    use crate::context_ext::OpContextCoreExt;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Testkontext: Kontext, Spawner, Wurzelpfad und Aufräum-Wächter.
    type RuntimeCtx = (
        OpContext,
        Arc<ManagedAgentSpawner>,
        PathBuf,
        Box<dyn std::any::Any>,
    );

    fn runtime_ctx(session: SessionId) -> TestResult<RuntimeCtx> {
        let tmp = std::env::temp_dir().join(format!(
            "harw-agent-background-test-{}-{}",
            std::process::id(),
            SessionId::new()
        ));
        std::fs::create_dir_all(tmp.join("ws")).map_err(ctx("Test-Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry aufbauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let spawner = Arc::new(ManagedAgentSpawner::new(
            Arc::new(Mutex::new(SessionManager::new(event_tx))),
            ChildLimits::conservative(),
        ));
        let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
        let mut services = ServiceMap::new();
        <OpContext as OpContextCoreExt>::register_agent_tool_services(
            &mut services,
            Arc::clone(&spawner),
            store,
        );
        let ctx = OpContext::new(session, TurnId::new(), sandbox, services);
        Ok((ctx, spawner, tmp, Box::new(event_rx)))
    }

    #[test]
    fn test_parse_child_id_is_closed_and_null_means_unset() -> TestResult {
        assert_eq!(
            parse_child_id("agent.status", &json!({}), false).map_err(ctx("leer ist erlaubt"))?,
            None
        );
        assert_eq!(
            parse_child_id("agent.status", &json!({ "child_id": null }), false)
                .map_err(ctx("null ist erlaubt"))?,
            None
        );
        assert!(parse_child_id("agent.cancel", &json!({}), true).is_err());
        assert!(parse_child_id("agent.status", &json!({ "role": "x" }), false).is_err());
        assert!(parse_child_id("agent.status", &json!({ "child_id": 3 }), false).is_err());
        Ok(())
    }

    #[test]
    fn test_status_is_read_only_and_cancel_always_asks() {
        let status = AgentStatusOperation.meta();
        assert_eq!(status.name, "agent.status");
        assert!(status.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::None
            }
        )));
        let cancel = AgentCancelOperation.meta();
        assert_eq!(cancel.name, "agent.cancel");
        assert!(cancel.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::ModelTool {
                readonly: false,
                approval: ApprovalPolicy::Always
            }
        )));
    }

    #[tokio::test]
    async fn test_status_lists_only_own_background_runs_with_progress() -> TestResult {
        let root = SessionId::new();
        let (runtime, spawner, tmp, _events) = runtime_ctx(root.clone())?;
        let mine = SessionId::new();
        let foreign = SessionId::new();
        let registry = spawner.background_children();
        registry.register(&mine, &root, "root-orchestrator", Some("Baue das Feature"));
        registry.register(&foreign, &SessionId::new(), "root-orchestrator", None);
        registry.record_progress(mine.as_str(), |progress| {
            progress.tool_calls = 4;
            progress.tokens = 1200;
            progress.last_step = Some("fs.read".to_owned());
        });

        let output = AgentStatusOperation
            .run(&runtime, OpInput::model_tool(json!({})))
            .await
            .map_err(ctx("agent.status läuft"))?;
        let agents = output
            .data
            .as_ref()
            .and_then(|data| data.get("agents"))
            .and_then(|agents| agents.as_array())
            .ok_or(TestError::Missing("agents-Liste"))?;
        assert_eq!(agents.len(), 1, "nur eigene Läufe");
        assert_eq!(agents[0]["child_id"], json!(mine.as_str()));
        assert_eq!(agents[0]["status"], json!("running"));
        assert_eq!(agents[0]["tool_calls"], json!(4));
        assert!(output.text.contains("letzter Schritt: fs.read"));

        // Eine fremde ID ist nicht sichtbar.
        let result = AgentStatusOperation
            .run(
                &runtime,
                OpInput::model_tool(json!({ "child_id": foreign.as_str() })),
            )
            .await;
        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "{result:?}"
        );
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }

    #[tokio::test]
    async fn test_cancel_only_reaches_own_background_runs() -> TestResult {
        let root = SessionId::new();
        let (runtime, spawner, tmp, _events) = runtime_ctx(root.clone())?;
        let mine = SessionId::new();
        let foreign = SessionId::new();
        let registry = spawner.background_children();
        registry.register(&mine, &root, "root-orchestrator", None);
        registry.register(&foreign, &SessionId::new(), "root-orchestrator", None);

        let foreign_result = AgentCancelOperation
            .run(
                &runtime,
                OpInput::model_tool(json!({ "child_id": foreign.as_str() })),
            )
            .await;
        assert!(
            matches!(foreign_result, Err(OpError::NotAvailable(_))),
            "{foreign_result:?}"
        );
        let output = AgentCancelOperation
            .run(
                &runtime,
                OpInput::model_tool(json!({ "child_id": mine.as_str() })),
            )
            .await
            .map_err(ctx("eigener Lauf darf abgebrochen werden"))?;
        assert_eq!(
            output.data.as_ref().and_then(|data| data.get("child_id")),
            Some(&json!(mine.as_str()))
        );
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }
}
