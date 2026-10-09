//! Plan R9, Teil C/E1: die echte Kette UIA → Root-Orchestrator →
//! `delegate_wave` **mitten im Turn** des Orchestrators.
//!
//! Beleg aus dem Live-Lauf: der Root-Orchestrator (Kind der UIA) rief
//! `delegate_wave` und bekam „no delegation capability“ — seine Sitzung lag
//! für den Turn nicht im Manager, und im Plan-Modus fiel ohnehin jedes Ziel
//! weg. Die Tests fahren `run_child` des Orchestrators; sein Werkzeug ruft
//! [`super::delegate_wave`] mit dem Kontext des laufenden Turns, genau wie
//! die Laufzeit (`harw-runtime::children::delegate_wave_provider`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use harw_agent_dsl::roles::AgentRoleId;
use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_catalog::AgentSuggestions;
use harw_core::child_controller::{ChildRegistryFactory, ManagedAgentSpawner};
use harw_core::{
    ChildLimits, InMemoryStateStore, InteractionMode, ModelFuture, ModelProvider, ModelRequest,
    ModelResponse, SessionActivation, SessionManager, SpawnContext, StateStore, TurnInput,
    TurnOutcome,
};
use harw_extension_api::{
    AgentSpawnError, AgentSpawner, DelegationTargetInfo, ExtensionRegistry,
    ExtensionRegistryBuilder, SpawnInput, ToolCall, ToolExecutionContext, ToolExecutor,
    ToolExecutorFuture, ToolName, ToolOutput, ToolProvider, ToolSpec,
};
use harw_operations::context::{OpContext, ServiceMap};
use harw_types::{
    AgentRole, ApprovalActor, SessionId, TenantId, TokenUsage, ToolCallId, WorkspaceId,
};
use serde_json::{Value, json};

use super::{DelegateWavePolicy, DelegateWaveReport, DelegateWaveRequest, delegate_wave};
use crate::context_ext::OpContextCoreExt;
use crate::test_support::{TestError, TestResult, ctx};

/// Ergebnis eines `delegate_wave`-Aufrufs aus dem laufenden Turn.
type WaveOutcome = Result<DelegateWaveReport, String>;

/// Modell des Orchestrators: Runde 1 ruft `delegate_wave` mit `arguments`,
/// danach Text.
struct WaveModel {
    arguments: Value,
    calls: AtomicUsize,
}

impl ModelProvider for WaveModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        let round = self.calls.fetch_add(1, Ordering::SeqCst);
        let arguments = self.arguments.clone();
        Box::pin(async move {
            if round == 0 {
                Ok(ModelResponse {
                    message: None,
                    tool_calls: vec![ToolCall {
                        id: ToolCallId::new(),
                        name: ToolName::new(super::DELEGATE_WAVE_TOOL),
                        arguments,
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

/// Modell eines Worker-Kindes: antwortet sofort mit Text.
struct AnswerModel;

impl ModelProvider for AnswerModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move { Ok(ModelResponse::text("Befund: alles gelesen")) })
    }
}

/// Gemeinsamer Zustand der Kette.
#[derive(Clone)]
struct Wiring {
    slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>>,
    store: Arc<dyn StateStore>,
    policy: DelegateWavePolicy,
    outcomes: Arc<Mutex<Vec<WaveOutcome>>>,
}

/// Registry des Orchestrators: genau `delegate_wave`.
struct OrchestratorRegistry {
    wiring: Wiring,
    arguments: Value,
}

impl ChildRegistryFactory for OrchestratorRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default()
            .tool_provider(Arc::new(WaveTool {
                wiring: self.wiring.clone(),
            }))
            .build())
    }

    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::new(WaveModel {
            arguments: self.arguments.clone(),
            calls: AtomicUsize::new(0),
        }))
    }
}

/// Registry der Worker: keine Werkzeuge, ein antwortendes Modell. Zählt, wie
/// viele Worker-Kinder tatsächlich gestartet wurden.
struct WorkerRegistry {
    started: Arc<AtomicUsize>,
}

impl ChildRegistryFactory for WorkerRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        Ok(ExtensionRegistryBuilder::default().build())
    }

    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::new(AnswerModel))
    }
}

struct WaveTool {
    wiring: Wiring,
}

impl ToolProvider for WaveTool {
    fn tools(&self) -> Vec<ToolSpec> {
        // Das echte Schema der Operation (wie `delegate_wave_provider`).
        let operation: Arc<dyn harw_operations::operation::Operation> = Arc::new(
            super::DelegateWaveOperation::new(self.wiring.policy.clone()),
        );
        harw_operations::adapter::ModelToolProvider::new([operation], |execution_context| {
            OpContext::new(
                execution_context.session_id().clone(),
                execution_context.turn_id().clone(),
                execution_context.sandbox().clone(),
                ServiceMap::new(),
            )
        })
        .tools()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        (name.as_str() == super::DELEGATE_WAVE_TOOL).then(|| {
            Arc::new(WaveExecutor {
                wiring: self.wiring.clone(),
            }) as Arc<dyn ToolExecutor>
        })
    }
}

struct WaveExecutor {
    wiring: Wiring,
}

impl ToolExecutor for WaveExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            // Wie `delegate_wave_provider`: der laufende Turn ist der
            // Aufrufer, Sandbox und Abbruch kommen aus seinem Kontext.
            let mut services = ServiceMap::new();
            if let Some(spawner) = self.wiring.slot.get().and_then(Weak::upgrade) {
                <OpContext as OpContextCoreExt>::register_agent_tool_services(
                    &mut services,
                    spawner,
                    Arc::clone(&self.wiring.store),
                );
            }
            let op_context = OpContext::new(
                context.session_id().clone(),
                context.turn_id().clone(),
                context.sandbox().clone(),
                services,
            );
            let op_context = match context.cancel() {
                Some(cancel) => op_context.with_cancel_token(cancel.clone()),
                None => op_context,
            };
            let outcome = match DelegateWaveRequest::parse(&call.arguments) {
                Ok(request) => delegate_wave(&op_context, &request, &self.wiring.policy)
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let text = match &outcome {
                Ok(report) => report.to_json().to_string(),
                Err(error) => error.clone(),
            };
            self.wiring
                .outcomes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(outcome);
            Ok(ToolOutput::Text { content: text })
        })
    }
}

fn test_sandbox(tmp: &std::path::Path) -> TestResult<SandboxSpec> {
    std::fs::create_dir_all(tmp.join("ws")).map_err(ctx("Test-Workspace anlegen"))?;
    let registry = WorkspaceRegistry::build(
        tmp,
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
    Ok(SandboxSpec::from_resolved(
        binding,
        PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ]),
    ))
}

fn info(name: &str, read_only: bool) -> DelegationTargetInfo {
    DelegationTargetInfo {
        name: name.to_owned(),
        role: "worker".to_owned(),
        read_only,
        ..DelegationTargetInfo::default()
    }
}

/// Wie der Plan-Modus in den Baum kommt.
#[derive(Clone, Copy)]
enum Mode {
    /// Kein Plan-Modus.
    Work,
    /// Die Oberfläche veröffentlicht `plan` (Runde 9, E6).
    PlanLive,
    /// Die Wurzel meldet ihren Plan-Modus (`note_caller_mode`).
    PlanNoted,
}

/// Fährt UIA → Root-Orchestrator → `delegate_wave(arguments)` und liefert den
/// Bericht der Welle.
async fn run_chain(mode: Mode, arguments: Value) -> TestResult<WaveOutcome> {
    run_chain_with(mode, arguments, ChildLimits::conservative())
        .await
        .map(|(outcome, _started)| outcome)
}

/// Wie [`run_chain`], mit frei wählbaren Kind-Grenzen; liefert zusätzlich die
/// Zahl tatsächlich gestarteter Worker-Kinder.
async fn run_chain_with(
    mode: Mode,
    arguments: Value,
    limits: ChildLimits,
) -> TestResult<(WaveOutcome, usize)> {
    let started = Arc::new(AtomicUsize::new(0));
    let tmp = std::env::temp_dir().join(format!(
        "harw-delegate-wave-chain-{}-{}",
        std::process::id(),
        SessionId::new()
    ));
    let sandbox = test_sandbox(&tmp)?;
    let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let wiring = Wiring {
        slot: Arc::new(OnceLock::new()),
        store: Arc::clone(&store),
        policy: DelegateWavePolicy::new(
            |role| match role {
                "explorer" => Some("reduce_to_read_explore"),
                "executor" => Some("reduce_to_read_execute"),
                _ => None,
            },
            // Der Root-Orchestrator deklariert nichts: es gilt die
            // sichtbare Menge.
            |_caller| None,
        ),
        outcomes: Arc::new(Mutex::new(Vec::new())),
    };
    let uia = SessionId::new();
    let role = |name: &str| AgentRole::Agent {
        name: name.to_owned(),
    };
    let spawner = ManagedAgentSpawner::new(manager, limits)
        .with_role(
            "root-orchestrator",
            role("root-orchestrator"),
            AgentRoleId::RootOrchestrator,
            Arc::new(OrchestratorRegistry {
                wiring: wiring.clone(),
                arguments,
            }),
        )
        .with_role(
            "explorer",
            role("explorer"),
            AgentRoleId::Worker,
            Arc::new(WorkerRegistry {
                started: Arc::clone(&started),
            }),
        )
        .with_role(
            "executor",
            role("executor"),
            AgentRoleId::Worker,
            Arc::new(WorkerRegistry {
                started: Arc::clone(&started),
            }),
        )
        .with_delegation_catalog([
            info("root-orchestrator", true),
            info("explorer", true),
            info("executor", false),
        ])
        .with_external_root_parent(
            uia.clone(),
            SpawnContext {
                sandbox: sandbox.clone(),
                suggestions: None,
                capability_snapshot: None,
                approval_actor: Some(ApprovalActor::Operator {
                    id: "chain-test-operator".to_owned(),
                }),
                organizational_role: AgentRoleId::UserInterface,
                allowed_child_orchestrators: Vec::new(),
                trace: None,
                ceiling: None,
            },
            None,
            SessionActivation::default(),
        )
        .map_err(ctx("die UIA-Wurzel registriert sich"))?;
    let spawner = Arc::new(spawner);
    wiring
        .slot
        .set(Arc::downgrade(&spawner))
        .map_err(|_| TestError::Unexpected("Spawner-Slot doppelt gesetzt".to_owned()))?;
    match mode {
        Mode::Work => {}
        Mode::PlanLive => spawner.live_mode().publish(InteractionMode::Plan),
        Mode::PlanNoted => AgentSpawner::note_caller_mode(spawner.as_ref(), &uia, true),
    }

    // UIA → Root-Orchestrator (derselbe Weg wie `transfer_to_root-orchestrator`).
    let orchestrator = AgentSpawner::spawn_child(
        spawner.as_ref(),
        "root-orchestrator",
        SpawnInput {
            parent_session_id: uia,
            handoff_call_id: ToolCallId::new(),
            instructions: Some("Erkunde und plane".to_owned()),
            context: Value::Null,
            ceiling: None,
        },
        sandbox,
        None,
    )
    .await
    .map_err(|error| TestError::Unexpected(format!("Root-Orchestrator: {}", error.message)))?;
    let run = spawner
        .run_child(&orchestrator, store.as_ref(), TurnInput::user("los"))
        .await
        .map_err(|error| TestError::Unexpected(format!("Orchestrator-Lauf: {}", error.message)))?;
    let _ = std::fs::remove_dir_all(&tmp);
    if !matches!(run.outcome, TurnOutcome::Completed) {
        return Err(TestError::Unexpected(format!("{:?}", run.outcome)));
    }
    let mut outcomes = wiring
        .outcomes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    match (outcomes.pop(), outcomes.is_empty()) {
        (Some(outcome), true) => Ok((outcome, started.load(Ordering::SeqCst))),
        other => Err(TestError::Unexpected(format!(
            "genau ein delegate_wave-Aufruf erwartet: {other:?}"
        ))),
    }
}

fn wave(roles: &[&str]) -> Value {
    let targets: Vec<Value> = roles
        .iter()
        .map(|role| json!({ "role": role, "task": format!("Auftrag für {role}") }))
        .collect();
    json!({ "targets": targets, "join": "collect" })
}

fn statuses(report: &DelegateWaveReport) -> Vec<(String, &'static str)> {
    report
        .targets
        .iter()
        .map(|target| (target.role.clone(), target.status.label()))
        .collect()
}

/// Plan-Modus (live veröffentlicht): `explorer` läuft mitten im Turn des
/// Orchestrators, `executor` wird mit der Plan-Modus-Meldung abgelehnt.
#[tokio::test]
async fn plan_mode_chain_delegates_explorer_and_refuses_executor() -> TestResult {
    let outcome = run_chain(Mode::PlanLive, wave(&["explorer", "executor"])).await?;
    let report = outcome.map_err(TestError::Unexpected)?;
    assert_eq!(
        statuses(&report),
        vec![
            ("explorer".to_owned(), "completed"),
            ("executor".to_owned(), "unavailable"),
        ]
    );
    let rendered = report.to_json();
    assert_eq!(
        rendered["results"][1]["error"],
        json!(
            "Plan-Modus: nur lesende Ziele delegierbar (explorer); Schreib-/Ausführungsziele \
             erst nach Planfreigabe."
        )
    );
    Ok(())
}

/// Derselbe Plan-Modus, diesmal von der Wurzel gemeldet (ohne Live-Modus):
/// der Orchestrator erbt ihn über seine Kette.
#[tokio::test]
async fn plan_mode_noted_by_the_root_is_inherited_by_the_orchestrator() -> TestResult {
    let outcome = run_chain(Mode::PlanNoted, wave(&["explorer", "executor"])).await?;
    let report = outcome.map_err(TestError::Unexpected)?;
    assert_eq!(
        statuses(&report),
        vec![
            ("explorer".to_owned(), "completed"),
            ("executor".to_owned(), "unavailable"),
        ]
    );
    Ok(())
}

/// Nur schreibende Ziele im Plan-Modus: die ganze Welle scheitert nicht
/// stumm, jedes Ziel nennt die Plan-Modus-Regel.
#[tokio::test]
async fn plan_mode_chain_with_only_writers_names_the_plan_rule() -> TestResult {
    let outcome = run_chain(Mode::PlanLive, wave(&["executor"])).await?;
    let report = outcome.map_err(TestError::Unexpected)?;
    let rendered = report.to_json();
    assert!(
        rendered["results"][0]["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("Plan-Modus: nur lesende Ziele")),
        "{rendered}"
    );
    Ok(())
}

/// Außerhalb des Plan-Modus läuft dieselbe Kette für alle sichtbaren Ziele —
/// auch mitten im Turn (früher: „no delegation capability“); ein unbekanntes
/// Ziel nennt die delegierbaren.
#[tokio::test]
async fn work_mode_chain_delegates_every_visible_target() -> TestResult {
    let outcome = run_chain(Mode::Work, wave(&["explorer", "executor", "geheim"])).await?;
    let report = outcome.map_err(TestError::Unexpected)?;
    assert_eq!(
        statuses(&report),
        vec![
            ("explorer".to_owned(), "completed"),
            ("executor".to_owned(), "completed"),
            ("geheim".to_owned(), "unavailable"),
        ]
    );
    assert_eq!(
        report.to_json()["results"][2]["error"],
        json!("Ziel geheim ist für dich nicht delegierbar; delegierbar sind: executor, explorer")
    );
    Ok(())
}

/// Fail-fast, all-or-nothing: passen nicht alle Ziele in die freien
/// Kind-Slots des Aufrufers, wird nichts gestartet und der Fehler nennt die
/// Grenze, die belegten Slots und die Folge.
#[tokio::test]
async fn a_wave_that_exceeds_the_child_capacity_starts_nothing() -> TestResult {
    let limits = ChildLimits {
        max_active_children_per_parent: 1,
        ..ChildLimits::conservative()
    };
    let (outcome, started) =
        run_chain_with(Mode::Work, wave(&["explorer", "executor"]), limits).await?;
    let Err(message) = outcome else {
        return Err(TestError::Unexpected(
            "the wave must be rejected as a whole".to_owned(),
        ));
    };
    assert_eq!(started, 0, "no child may be started: {message}");
    assert!(message.contains("active child limit reached"), "{message}");
    assert!(message.contains("2 new child(ren) requested"), "{message}");
    assert!(message.contains("nothing was started"), "{message}");
    assert!(
        message.contains(harw_extension_api::FAIL_FAST_CONSEQUENCE),
        "{message}"
    );
    Ok(())
}

/// Ein explizites `max_parallel` kleiner als die Zahl lauffähiger Ziele
/// reiht nicht ein, sondern lehnt die ganze Welle ab.
#[tokio::test]
async fn a_wave_larger_than_max_parallel_is_rejected_instead_of_queued() -> TestResult {
    let arguments = json!({
        "targets": [
            { "role": "explorer", "task": "a" },
            { "role": "explorer", "task": "b" },
            { "role": "executor", "task": "c" },
        ],
        "join": "collect",
        "max_parallel": 2,
    });
    let (outcome, started) =
        run_chain_with(Mode::Work, arguments, ChildLimits::conservative()).await?;
    let Err(message) = outcome else {
        return Err(TestError::Unexpected(
            "the wave must be rejected as a whole".to_owned(),
        ));
    };
    assert_eq!(started, 0, "{message}");
    assert!(
        message.contains("3 targets are runnable but max_parallel is 2"),
        "{message}"
    );
    assert!(message.contains("never queued"), "{message}");
    Ok(())
}

/// Ohne `max_parallel` laufen alle Ziele gleichzeitig (keine Warteschlange).
#[tokio::test]
async fn a_wave_without_max_parallel_runs_every_target_at_once() -> TestResult {
    let (outcome, started) = run_chain_with(
        Mode::Work,
        wave(&["explorer", "explorer", "executor", "executor", "explorer"]),
        ChildLimits::conservative(),
    )
    .await?;
    let report = outcome.map_err(TestError::Unexpected)?;
    assert_eq!(started, 5);
    assert_eq!(report.max_parallel, 5);
    assert!(
        report
            .targets
            .iter()
            .all(|target| target.status.label() == "completed")
    );
    Ok(())
}
