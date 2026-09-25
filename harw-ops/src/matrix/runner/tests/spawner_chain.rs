//! Runde 9, E7: der echte [`SpawnerDriver`]-Pfad in der Kette
//! UIA → `matrix-game-master` (Kind) → Sitz-Agenten.
//!
//! Live-Befund: jeder Sitz-Aufruf scheiterte mit „Spawn fehlgeschlagen:
//! unknown child parent: <game-master>“, weil die Session des Game Masters
//! während seines Turns (in dem `matrix.run` läuft) nicht im Manager lag. Die
//! Tests bauen genau diese Kette nach: eine externe UIA-Wurzel, der Game
//! Master als ihr admittiertes Kind, dessen Turn ein Werkzeug ruft, das wie
//! `matrix.run` den [`OpContext`] aus dem Ausführungskontext baut und
//! [`MatrixRun::step`] mit dem echten Spawner fährt. Die Sitze sind echte
//! Kind-Agenten mit einem geskripteten Modell.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, Weak};

use harw_agent_dsl::roles::AgentRoleId;
use harw_authority::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
use harw_catalog::AgentSuggestions;
use harw_core::child_controller::{ChildLimits, ChildRegistryFactory};
use harw_core::{
    InMemoryStateStore, ModelFuture, ModelProvider, ModelRequest, ModelResponse, SessionActivation,
    SessionManager, SpawnContext,
};
use harw_core_bridge::OpContextCoreExt;
use harw_extension_api::{
    AgentSpawnError, ExtensionRegistry, ExtensionRegistryBuilder, SpawnInput, ToolProvider,
};
use harw_operations::ServiceMap;
use harw_tools::{
    FunctionToolSpec, JsonSchema, ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture,
    ToolName, ToolOutput, ToolSpec,
};
use harw_types::{AgentRole, ApprovalActor, TenantId, TokenUsage};

use super::*;

/// Werkzeug des Game Masters im Test: spielt wie `matrix.run` Phasen.
const STEP_TOOL: &str = "probe.matrix_steps";

/// Gemeinsamer Zustand zwischen Test und Werkzeug.
#[derive(Default)]
struct Shared {
    run: Mutex<Option<MatrixRun>>,
    reports: Mutex<Vec<StepReport>>,
    errors: Mutex<Vec<String>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Sitz-Modell: liest den verlangten Vertrag aus dem letzten Turn-Prompt
/// und antwortet minimal gültig.
struct SeatModel;

fn seat_answer(contract: &str) -> String {
    match contract {
        "briefing_ack" => r#"{"ack":true,"intent":"Wir halten Kurs."}"#.to_owned(),
        "negotiation_request" => r#"{"requests":[]}"#.to_owned(),
        "negotiation_message" => r#"{"messages":[]}"#.to_owned(),
        "player_argument" => r#"{"action":"Die Fraktion verstärkt ihre Präsenz am Hafen.","pros":["Sie hat Leute vor Ort.","Die Lage verlangt Handeln."]}"#.to_owned(),
        "counter_argument" => r#"{"counters":[]}"#.to_owned(),
        other => format!("unbekannter Vertrag `{other}`"),
    }
}

impl ModelProvider for SeatModel {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        // Turn-Prompts enden mit „… nach Vertrag `<name>`.“; der letzte
        // Treffer im Verlauf ist der aktuelle Aufruf.
        let history = format!("{:?}", request.history);
        let contract = history
            .rsplit_once("nach Vertrag `")
            .and_then(|(_, rest)| rest.split('`').next())
            .unwrap_or_default()
            .to_owned();
        Box::pin(async move { Ok(ModelResponse::text(seat_answer(&contract))) })
    }
}

/// Modell des Game Masters: erst [`STEP_TOOL`], dann eine Textantwort.
struct GameMasterModel {
    calls: AtomicUsize,
}

impl ModelProvider for GameMasterModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        let round = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if round == 0 {
                Ok(ModelResponse {
                    message: None,
                    tool_calls: vec![ToolCall {
                        id: ToolCallId::new(),
                        name: ToolName::new(STEP_TOOL),
                        arguments: json!({}),
                    }],
                    usage: TokenUsage::default(),
                    ..Default::default()
                })
            } else {
                Ok(ModelResponse::text("Runde gespielt."))
            }
        })
    }
}

/// Registry-Factory für Game Master und Sitze.
struct ChainRegistry {
    slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>>,
    store: Arc<dyn StateStore>,
    shared: Arc<Shared>,
}

impl ChildRegistryFactory for ChainRegistry {
    fn build_registry(
        &self,
        role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        let builder = ExtensionRegistryBuilder::default();
        Ok(if role == role_names::MATRIX_GAME_MASTER {
            builder
                .tool_provider(Arc::new(StepTool {
                    slot: Arc::clone(&self.slot),
                    store: Arc::clone(&self.store),
                    shared: Arc::clone(&self.shared),
                }))
                .build()
        } else {
            builder.build()
        })
    }

    fn model_for(&self, role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(if role == role_names::MATRIX_GAME_MASTER {
            Arc::new(GameMasterModel {
                calls: AtomicUsize::new(0),
            })
        } else {
            Arc::new(SeatModel)
        })
    }
}

#[derive(Clone)]
struct StepTool {
    slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>>,
    store: Arc<dyn StateStore>,
    shared: Arc<Shared>,
}

impl ToolProvider for StepTool {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(STEP_TOOL),
            description: "spielt Phasen wie matrix.run".to_owned(),
            parameters: JsonSchema::default(),
            strict: false,
        })]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        (name.as_str() == STEP_TOOL).then(|| Arc::new(self.clone()) as Arc<dyn ToolExecutor>)
    }
}

impl ToolExecutor for StepTool {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let Some(spawner) = self.slot.get().and_then(Weak::upgrade) else {
                return Ok(ToolOutput::error("kein Spawner"));
            };
            // Wie `matrix_game_master_provider` in der Runtime: Sitzung,
            // Turn und Sandbox aus dem Ausführungskontext des Game Masters.
            let mut services = ServiceMap::new();
            <OpContext as OpContextCoreExt>::register_agent_tool_services(
                &mut services,
                spawner,
                Arc::clone(&self.store),
            );
            let ctx = OpContext::new(
                context.session_id().clone(),
                context.turn_id().clone(),
                context.sandbox().clone(),
                services,
            );
            let Some(mut run) = lock(&self.shared.run).take() else {
                return Ok(ToolOutput::error("kein Lauf"));
            };
            let mut lines = Vec::new();
            for _ in 0..10 {
                match run.step(&ctx).await {
                    Ok(report) => {
                        lines.push(report.summary());
                        let phase = report.phase;
                        lock(&self.shared.reports).push(report);
                        if phase == Phase::Argumente {
                            break;
                        }
                    }
                    Err(error) => {
                        lock(&self.shared.errors).push(error.to_string());
                        break;
                    }
                }
            }
            *lock(&self.shared.run) = Some(run);
            Ok(ToolOutput::text(lines.join("\n")))
        })
    }
}

/// Test-Sandbox (nur lesend) über einem temporären Workspace.
fn chain_sandbox(root: &Path) -> Result<SandboxSpec, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(root.join("ws"))?;
    let registry = WorkspaceRegistry::build(
        root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("test-tenant"),
            workspace: WorkspaceId::from_str("ws"),
            root: PathBuf::from("ws"),
        }],
    )?;
    let binding = registry.resolve(
        &TenantId::from_str("test-tenant"),
        &WorkspaceId::from_str("ws"),
    )?;
    Ok(SandboxSpec::from_resolved(
        binding,
        PermissionSet::from_policy([Permission::ReadWorkspace]),
    ))
}

/// Spielt Briefing bis Argumente durch die Kette UIA → Game Master → Sitze
/// und liefert die Schritt-Berichte samt Lauf.
async fn play_through_the_chain(
    max_children: usize,
) -> Result<(Vec<StepReport>, MatrixRun), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let sandbox = chain_sandbox(tmp.path())?;
    let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    let slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>> = Arc::new(OnceLock::new());
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let shared = Arc::new(Shared::default());
    *lock(&shared.run) = Some(run()?);
    let factory: Arc<dyn ChildRegistryFactory> = Arc::new(ChainRegistry {
        slot: Arc::clone(&slot),
        store: Arc::clone(&store),
        shared: Arc::clone(&shared),
    });
    let limits = ChildLimits {
        max_active_children_per_parent: max_children,
        ..ChildLimits::conservative()
    };
    let agent = |name: &str| AgentRole::Agent {
        name: name.to_owned(),
    };
    let mut spawner = ManagedAgentSpawner::new(manager, limits).with_role(
        role_names::MATRIX_GAME_MASTER,
        agent(role_names::MATRIX_GAME_MASTER),
        AgentRoleId::RootOrchestrator,
        Arc::clone(&factory),
    );
    for seat in [role_names::MATRIX_PLAYER, role_names::MATRIX_UMPIRE] {
        spawner = spawner.with_role(seat, agent(seat), AgentRoleId::Worker, Arc::clone(&factory));
    }
    // Die UIA ist die extern gefahrene Wurzel (wie in der TUI).
    let uia = SessionId::new();
    let spawner = Arc::new(
        spawner
            .with_external_root_parent(
                uia.clone(),
                SpawnContext {
                    sandbox: sandbox.clone(),
                    suggestions: None,
                    capability_snapshot: None,
                    approval_actor: Some(ApprovalActor::Operator {
                        id: "uia".to_owned(),
                    }),
                    organizational_role: AgentRoleId::UserInterface,
                    allowed_child_orchestrators: Vec::new(),
                    trace: None,
                    ceiling: None,
                },
                None,
                SessionActivation::default(),
            )
            .map_err(|error| error.message)?,
    );
    slot.set(Arc::downgrade(&spawner))
        .map_err(|_| "Spawner-Slot doppelt gesetzt")?;

    // `transfer_to_matrix-game-master`: der Game Master ist Kind der UIA.
    let game_master = spawner
        .spawn_child_or_wait(
            role_names::MATRIX_GAME_MASTER,
            SpawnInput {
                parent_session_id: uia,
                handoff_call_id: ToolCallId::new(),
                instructions: Some("Spiele das Szenario.".to_owned()),
                context: json!({}),
                ceiling: None,
            },
            sandbox,
            None,
            Duration::from_secs(1),
            &CancelToken::new(),
        )
        .await
        .map_err(|error| error.message)?
        .keep();
    let result = spawner
        .run_child(&game_master, store.as_ref(), TurnInput::user("spiele"))
        .await
        .map_err(|error| error.message)?;
    assert!(
        matches!(result.outcome, TurnOutcome::Completed),
        "{:?}",
        result.outcome
    );
    let errors = lock(&shared.errors).clone();
    assert!(errors.is_empty(), "Schrittfehler: {errors:?}");
    let reports = std::mem::take(&mut *lock(&shared.reports));
    let run = lock(&shared.run).take().ok_or("Lauf kam nicht zurück")?;
    Ok((reports, run))
}

fn assert_seats_argued(reports: &[StepReport], run: &MatrixRun) {
    let phases: Vec<Phase> = reports.iter().map(|report| report.phase).collect();
    assert_eq!(phases.first(), Some(&Phase::Briefing), "{phases:?}");
    assert_eq!(phases.last(), Some(&Phase::Argumente), "{phases:?}");
    for report in reports {
        assert!(
            report.forfeits.is_empty(),
            "kein Sitz darf passen: {}",
            report.summary()
        );
        assert!(report.technical_stop.is_none(), "{}", report.summary());
    }
    let briefing = reports.first().map_or(0, |report| report.calls);
    assert_eq!(briefing, 5, "4 Spieler + Umpire wurden gefragt");
    let arguments = run
        .log()
        .journal
        .entries()
        .filter(|entry| matches!(entry.kind, EntryKind::ArgumentRevealed { .. }))
        .count();
    assert_eq!(arguments, 4, "jeder Spieler brachte ein Argument vor");
    assert_eq!(run.status(), RunStatus::Running);
}

/// Der Game Master als Kind der UIA startet seine Sitze; die Sitze antworten
/// und bringen Argumente vor (vorher: jeder Sitz passte in 14 ms).
#[tokio::test]
async fn seats_argue_when_the_game_master_is_a_child_of_the_uia() -> TestResult {
    let (reports, run) =
        play_through_the_chain(ChildLimits::conservative().max_active_children_per_parent).await?;
    assert_seats_argued(&reports, &run);
    Ok(())
}

/// Weniger gleichzeitige Kinder als Sitze (Fan-out-Deckel des Modellprofils,
/// oft 3–4): der Treiber verdrängt den am längsten ruhenden Sitz, statt den
/// fünften Sitz 120 s warten und dann passen zu lassen.
#[tokio::test]
async fn seats_argue_under_a_fan_out_cap_below_the_seat_count() -> TestResult {
    let (reports, run) = play_through_the_chain(3).await?;
    assert_seats_argued(&reports, &run);
    Ok(())
}
