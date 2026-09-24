//! Runde 9, E7: ein Kind darf **während seines eigenen Turns** Kinder
//! starten.
//!
//! Beleg aus dem Live-Lauf: der Matrix-Game-Master lief als Kind der UIA; aus
//! `matrix.run` heraus startete er seine Sitz-Agenten mit sich selbst als
//! Elternteil. Seine Session lag in diesem Moment nicht im Manager (sie ist
//! für den Turn entnommen), die Admission lehnte jeden Sitz sofort mit
//! „unknown child parent“ ab, und jeder Sitz passte in jeder Phase.
//!
//! Die Tests fahren den echten Pfad: externe UIA-Wurzel → admittierter
//! Root-Orchestrator → sein Turn ruft ein Werkzeug, das einen Worker mit dem
//! laufenden Orchestrator als Elternteil admittiert.
#![allow(clippy::type_complexity)]

use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::{AtomicUsize, Ordering};

use harw_agent_dsl::roles::AgentRoleId;
use harw_extension_api::{
    ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolProvider, ToolSpec,
};
use harw_tools::{FunctionToolSpec, JsonSchema};

use super::*;

/// Name des Test-Werkzeugs, das aus dem laufenden Turn einen Worker startet.
const SPAWN_TOOL: &str = "probe.spawn_worker";

/// Aus dem laufenden Turn beobachtet: sichtbare Delegationsziele und das
/// Ergebnis der Admission.
type SpawnOutcome = (Vec<String>, Result<SessionId, String>);

/// Modell des Orchestrators: erst ein Aufruf von [`SPAWN_TOOL`], dann Text.
struct SpawnThenDone {
    calls: AtomicUsize,
}

impl ModelProvider for SpawnThenDone {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        let round = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if round == 0 {
                Ok(ModelResponse {
                    message: None,
                    tool_calls: vec![ToolCall {
                        id: ToolCallId::new(),
                        name: ToolName::new(SPAWN_TOOL),
                        arguments: serde_json::json!({}),
                    }],
                    usage: TokenUsage::default(),
                    ..Default::default()
                })
            } else {
                Ok(ModelResponse::text("fertig"))
            }
        })
    }
}

/// Registry des Orchestrators: genau [`SPAWN_TOOL`], dessen Ausführer über
/// einen schwach gehaltenen Spawner-Slot (wie in der Runtime) admittiert.
struct SpawningRegistry {
    slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>>,
    outcomes: Arc<Mutex<Vec<SpawnOutcome>>>,
}

impl ChildRegistryFactory for SpawningRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default()
            .tool_provider(Arc::new(SpawnTool {
                slot: Arc::clone(&self.slot),
                outcomes: Arc::clone(&self.outcomes),
            }))
            .build())
    }

    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::new(SpawnThenDone {
            calls: AtomicUsize::new(0),
        }))
    }
}

struct SpawnTool {
    slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>>,
    outcomes: Arc<Mutex<Vec<SpawnOutcome>>>,
}

impl ToolProvider for SpawnTool {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(SPAWN_TOOL),
            description: "startet einen Worker aus dem laufenden Turn".to_owned(),
            parameters: JsonSchema::default(),
            strict: false,
        })]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        (name.as_str() == SPAWN_TOOL).then(|| {
            Arc::new(SpawnExecutor {
                slot: Arc::clone(&self.slot),
                outcomes: Arc::clone(&self.outcomes),
            }) as Arc<dyn ToolExecutor>
        })
    }
}

struct SpawnExecutor {
    slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>>,
    outcomes: Arc<Mutex<Vec<SpawnOutcome>>>,
}

impl ToolExecutor for SpawnExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let visible = self
                .slot
                .get()
                .and_then(Weak::upgrade)
                .and_then(|spawner| {
                    spawner
                        .visible_delegation_target_names(context.session_id())
                        .ok()
                })
                .unwrap_or_default();
            let cancel = CancelToken::new();
            let outcome = match self.slot.get().and_then(Weak::upgrade) {
                // Genau wie `SpawnerDriver` bzw. das Agent-Werkzeug: der
                // aufrufende (laufende) Turn ist der Elternteil, die Sandbox
                // ist die des Turns.
                Some(spawner) => spawner
                    .spawn_child_or_wait(
                        "worker",
                        spawn_input(context.session_id().clone()),
                        context.sandbox().clone(),
                        None,
                        Duration::from_millis(50),
                        &cancel,
                    )
                    .await
                    .map(ChildGuard::keep)
                    .map_err(|error| error.message),
                None => Err("kein Spawner im Slot".to_owned()),
            };
            let text = match &outcome {
                Ok(child) => format!("gestartet: {child}"),
                Err(error) => format!("abgelehnt: {error}"),
            };
            self.outcomes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((visible, outcome));
            Ok(ToolOutput::Text { content: text })
        })
    }
}

/// UIA-Wurzel (extern) mit `root-orchestrator` (spawnt aus seinem Turn) und
/// `worker`.
fn running_parent_spawner() -> TestResult<(
    Arc<ManagedAgentSpawner>,
    SessionId,
    SandboxSpec,
    Arc<Mutex<Vec<SpawnOutcome>>>,
)> {
    let (events, _receiver) = mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
    let uia = SessionId::new();
    let slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>> = Arc::new(OnceLock::new());
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let role = |name: &str| AgentRole::Agent {
        name: name.to_owned(),
    };
    let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
        .with_role(
            "root-orchestrator",
            role("root-orchestrator"),
            AgentRoleId::RootOrchestrator,
            Arc::new(SpawningRegistry {
                slot: Arc::clone(&slot),
                outcomes: Arc::clone(&outcomes),
            }),
        )
        .with_role(
            "worker",
            role("worker"),
            AgentRoleId::Worker,
            Arc::new(EmptyChildRegistry),
        )
        .with_external_root_parent(
            uia.clone(),
            external_root_context(sandbox.clone(), AgentRoleId::UserInterface),
            None,
            SessionActivation::default(),
        )
        .map_err(ctx("die UIA-Wurzel registriert sich"))?;
    let spawner = Arc::new(spawner);
    slot.set(Arc::downgrade(&spawner))
        .map_err(|_| TestError::Unexpected("Spawner-Slot doppelt gesetzt".to_owned()))?;
    Ok((spawner, uia, sandbox, outcomes))
}

/// Der Orchestrator (Kind der UIA, Tiefe 1) startet aus seinem laufenden
/// Turn einen Worker (Tiefe 2) — vorher „unknown child parent“.
#[tokio::test]
async fn a_running_child_admits_its_own_child_mid_turn() -> TestResult {
    let (spawner, uia, sandbox, outcomes) = running_parent_spawner()?;
    let orchestrator = spawner
        .admit("root-orchestrator", spawn_input(uia.clone()), sandbox, None)
        .map_err(ctx("der Orchestrator ist Kind der UIA"))?;
    let store = InMemoryStateStore::new();
    let run = spawner
        .run_child(
            &orchestrator,
            &store,
            TurnInput::user("starte einen Worker"),
        )
        .await
        .map_err(|error| TestError::Unexpected(format!("Orchestrator-Lauf: {}", error.message)))?;
    assert!(
        matches!(run.outcome, TurnOutcome::Completed),
        "{:?}",
        run.outcome
    );

    let outcomes = outcomes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let [(visible, Ok(worker))] = outcomes.as_slice() else {
        return Err(TestError::Unexpected(format!(
            "genau eine erfolgreiche Admission erwartet: {outcomes:?}"
        )));
    };
    // Die Zielliste des laufenden Turns stimmt mit der Admission überein.
    assert!(
        visible.iter().any(|name| name == "worker"),
        "der laufende Orchestrator sieht seinen Worker: {visible:?}"
    );
    let record = spawner
        .child_record(worker)
        .ok_or(TestError::Missing("Worker ist admittiert"))?;
    assert_eq!(
        record.parent, orchestrator,
        "Elternteil ist der laufende Turn"
    );
    assert_eq!(record.depth, 2, "UIA → Orchestrator → Worker");

    // Nach dem Turn liegt der Orchestrator wieder im Manager; die
    // Eltern-Sicht ist verschwunden.
    assert!(spawner.checked_out_parent(&orchestrator).is_none());
    Ok(())
}

/// Die Eltern-Sicht gilt nur während des Turns: eine Session, die weder im
/// Manager liegt noch gerade läuft, bleibt ein unbekannter Elternteil.
#[test]
fn an_unknown_parent_is_still_rejected() -> TestResult {
    let (spawner, _uia, sandbox, _outcomes) = running_parent_spawner()?;
    match spawner.admit("worker", spawn_input(SessionId::new()), sandbox, None) {
        Ok(child) => Err(TestError::Unexpected(format!(
            "unbekannter Elternteil darf nichts admittieren, admittiert: {child}"
        ))),
        Err(error) => {
            assert!(
                error.message.contains("unknown child parent"),
                "{}",
                error.message
            );
            Ok(())
        }
    }
}
