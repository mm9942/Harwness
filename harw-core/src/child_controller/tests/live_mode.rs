//! Runde 9, E6: ein Moduswechsel der Oberfläche erreicht laufende Kinder.
//!
//! Beleg aus dem Live-Lauf: die Sitzung stand beim Start im Plan-Modus, die
//! UIA startete einen Root-Orchestrator im Hintergrund, die Nutzerin schaltete
//! danach auf `work` — das Kind blieb im alten Modus, ohne Schreib- und
//! Delegationswerkzeuge. Die Tests fahren den echten Kind-Pfad
//! (`admit` → `run_child` in einer eigenen Task wie ein Hintergrund-Kind) und
//! lesen die Werkzeugfläche aus den Modellanfragen jeder Runde.
#![allow(clippy::type_complexity)]

use std::sync::atomic::{AtomicUsize, Ordering};

use harw_agent_dsl::roles::AgentRoleId;
use harw_extension_api::{
    ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolProvider, ToolSpec,
};
use harw_tools::{FunctionToolSpec, JsonSchema};

use super::*;
use crate::live_mode::LiveModeBroadcast;
use crate::mode::InteractionMode;

/// Werkzeuge der Kind-Registry: ein lesendes, zwei schreibende/ausführende
/// und das Delegationswerkzeug.
const TOOLS: &[&str] = &["fs.read", "fs.write", "shell.exec", "delegate_wave"];

/// Beobachtung einer Modellrunde: sichtbare Werkzeugnamen und der Verlauf
/// (Debug-Form), wie das Modell ihn bekommt.
type Round = (Vec<String>, String);

/// Modell des Kindes: veröffentlicht in Runde `i` den Modus `switches[i]`
/// (wie ein Wechsel der Nutzerin, während das Kind läuft) und ruft `fs.read`;
/// nach der letzten Umschaltung antwortet es mit Text.
struct SwitchingModel {
    live: LiveModeBroadcast,
    switches: Vec<InteractionMode>,
    calls: AtomicUsize,
    rounds: Arc<Mutex<Vec<Round>>>,
}

impl ModelProvider for SwitchingModel {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        let round = self.calls.fetch_add(1, Ordering::SeqCst);
        let names = request
            .tools
            .iter()
            .map(|spec| spec.name().to_owned())
            .collect();
        self.rounds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((names, format!("{:?}", request.history)));
        let switch = self.switches.get(round).copied();
        Box::pin(async move {
            match switch {
                Some(mode) => {
                    self.live.publish(mode);
                    Ok(ModelResponse {
                        message: None,
                        tool_calls: vec![ToolCall {
                            id: ToolCallId::new(),
                            name: ToolName::new("fs.read"),
                            arguments: serde_json::json!({}),
                        }],
                        usage: TokenUsage::default(),
                        ..Default::default()
                    })
                }
                None => Ok(ModelResponse::text("fertig")),
            }
        })
    }
}

struct ProbeTools;

impl ToolProvider for ProbeTools {
    fn tools(&self) -> Vec<ToolSpec> {
        TOOLS
            .iter()
            .map(|name| {
                ToolSpec::Function(FunctionToolSpec {
                    name: ToolName::new(*name),
                    description: format!("Testwerkzeug {name}"),
                    parameters: JsonSchema::default(),
                    strict: false,
                })
            })
            .collect()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        TOOLS
            .contains(&name.as_str())
            .then(|| Arc::new(ProbeExecutor) as Arc<dyn ToolExecutor>)
    }
}

struct ProbeExecutor;

impl ToolExecutor for ProbeExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            Ok(ToolOutput::Text {
                content: "ok".to_owned(),
            })
        })
    }
}

/// Registry-Fabrik des Orchestrator-Kindes: [`ProbeTools`] und das
/// [`SwitchingModel`].
struct SwitchingRegistry {
    live: LiveModeBroadcast,
    switches: Vec<InteractionMode>,
    rounds: Arc<Mutex<Vec<Round>>>,
}

impl ChildRegistryFactory for SwitchingRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default()
            .tool_provider(Arc::new(ProbeTools))
            .build())
    }

    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::new(SwitchingModel {
            live: self.live.clone(),
            switches: self.switches.clone(),
            calls: AtomicUsize::new(0),
            rounds: Arc::clone(&self.rounds),
        }))
    }
}

/// UIA-Wurzel (extern) mit einem `root-orchestrator`, dessen Modell die
/// Modi `switches` nacheinander veröffentlicht.
fn live_mode_spawner(
    switches: Vec<InteractionMode>,
) -> TestResult<(
    Arc<ManagedAgentSpawner>,
    SessionId,
    SandboxSpec,
    Arc<Mutex<Vec<Round>>>,
)> {
    let (events, _receiver) = mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    let sandbox = test_sandbox(PermissionSet::from_policy([
        Permission::ReadWorkspace,
        Permission::WriteWorkspace,
    ]))?;
    let uia = SessionId::new();
    let rounds = Arc::new(Mutex::new(Vec::new()));
    let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative());
    let live = spawner.live_mode().clone();
    let spawner = spawner
        .with_role(
            "root-orchestrator",
            AgentRole::Agent {
                name: "root-orchestrator".to_owned(),
            },
            AgentRoleId::RootOrchestrator,
            Arc::new(SwitchingRegistry {
                live,
                switches,
                rounds: Arc::clone(&rounds),
            }),
        )
        .with_external_root_parent(
            uia.clone(),
            external_root_context(sandbox.clone(), AgentRoleId::UserInterface),
            None,
            SessionActivation::default(),
        )
        .map_err(ctx("die UIA-Wurzel registriert sich"))?;
    Ok((Arc::new(spawner), uia, sandbox, rounds))
}

/// Liest den Modus einer Kind-Sitzung aus dem Manager.
fn child_mode(spawner: &ManagedAgentSpawner, child: &SessionId) -> TestResult<InteractionMode> {
    let manager = spawner
        .manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(manager
        .get(child)
        .map_err(ctx("Kind liegt im Manager"))?
        .mode())
}

fn has(names: &[String], tool: &str) -> bool {
    names.iter().any(|name| name == tool)
}

/// Start im Plan-Modus, Kind im Hintergrund, Wechsel nach `work` → ab der
/// nächsten Runde sieht das Kind Schreib- und Delegationswerkzeuge und hat
/// die Notiz bekommen; zurück nach `plan` sind sie wieder verborgen.
#[tokio::test]
async fn a_running_background_child_follows_plan_to_work_and_back() -> TestResult {
    let (spawner, uia, sandbox, rounds) =
        live_mode_spawner(vec![InteractionMode::Work, InteractionMode::Plan])?;
    // Die Sitzung stand beim Start des Kindes im Plan-Modus.
    spawner.live_mode().publish(InteractionMode::Plan);
    let child = spawner
        .admit("root-orchestrator", spawn_input(uia), sandbox, None)
        .map_err(ctx("der Orchestrator ist Kind der UIA"))?;
    assert_eq!(child_mode(&spawner, &child)?, InteractionMode::Plan);

    // Wie ein Hintergrund-Kind: eigener Task, der Elternteil wartet nicht.
    let store = Arc::new(InMemoryStateStore::new());
    let background = {
        let spawner = Arc::clone(&spawner);
        let child = child.clone();
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            spawner
                .run_child(&child, store.as_ref(), TurnInput::user("arbeite"))
                .await
        })
    };
    let run = background
        .await
        .map_err(|error| TestError::Unexpected(format!("Hintergrund-Task: {error}")))?
        .map_err(|error| TestError::Unexpected(format!("Kind-Lauf: {}", error.message)))?;
    assert!(
        matches!(run.outcome, TurnOutcome::Completed),
        "{:?}",
        run.outcome
    );

    let rounds = rounds
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let [(plan, _), (work, work_history), (plan_again, plan_history)] = rounds.as_slice() else {
        return Err(TestError::Unexpected(format!(
            "drei Modellrunden erwartet: {rounds:?}"
        )));
    };
    // Runde 1 (plan): nur lesen.
    assert!(has(plan, "fs.read"), "{plan:?}");
    for hidden in ["fs.write", "shell.exec"] {
        assert!(!has(plan, hidden), "Plan-Modus verbirgt {hidden}: {plan:?}");
    }
    // Runde 2 (work): Schreib- und Delegationswerkzeuge sind zurück.
    for tool in ["fs.read", "fs.write", "shell.exec", "delegate_wave"] {
        assert!(has(work, tool), "work zeigt {tool}: {work:?}");
    }
    assert!(
        work_history.contains("Modus geändert: plan → work"),
        "{work_history}"
    );
    // Runde 3 (wieder plan): erneut verborgen.
    for hidden in ["fs.write", "shell.exec"] {
        assert!(
            !has(plan_again, hidden),
            "zurück im Plan-Modus ist {hidden} verborgen: {plan_again:?}"
        );
    }
    assert!(
        plan_history.contains("Modus geändert: work → plan"),
        "{plan_history}"
    );
    assert_eq!(child_mode(&spawner, &child)?, InteractionMode::Plan);
    Ok(())
}

/// Ein neues Kind startet im aktuellen Live-Modus, nicht im Modus vom
/// Sitzungsstart; ein ruhendes Kind übernimmt den Wechsel erst an seiner
/// nächsten Runden-Grenze.
#[test]
fn a_new_child_starts_in_the_current_live_mode() -> TestResult {
    let (spawner, uia, sandbox, _rounds) = live_mode_spawner(Vec::new())?;
    spawner.live_mode().publish(InteractionMode::Plan);
    let early = spawner
        .admit(
            "root-orchestrator",
            spawn_input(uia.clone()),
            sandbox.clone(),
            None,
        )
        .map_err(ctx("erstes Kind"))?;
    assert_eq!(child_mode(&spawner, &early)?, InteractionMode::Plan);

    spawner.live_mode().publish(InteractionMode::Work);
    let late = spawner
        .admit("root-orchestrator", spawn_input(uia), sandbox, None)
        .map_err(ctx("zweites Kind"))?;
    assert_eq!(child_mode(&spawner, &late)?, InteractionMode::Work);
    // Das früher gestartete Kind hat noch keine Runde gedreht.
    assert_eq!(child_mode(&spawner, &early)?, InteractionMode::Plan);

    let mut manager = spawner
        .manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let session = manager.get_mut(&early).map_err(ctx("erstes Kind"))?;
    assert_eq!(
        session.sync_live_mode(),
        Some((InteractionMode::Plan, InteractionMode::Work))
    );
    assert_eq!(session.sync_live_mode(), None, "genau einmal je Wechsel");
    Ok(())
}

/// Ohne Veröffentlichung behält ein Kind seinen eigenen Modus (Default).
#[test]
fn without_a_publication_children_keep_their_own_mode() -> TestResult {
    let (spawner, uia, sandbox, _rounds) = live_mode_spawner(Vec::new())?;
    let child = spawner
        .admit("root-orchestrator", spawn_input(uia), sandbox, None)
        .map_err(ctx("Kind"))?;
    assert_eq!(child_mode(&spawner, &child)?, InteractionMode::default());
    Ok(())
}
