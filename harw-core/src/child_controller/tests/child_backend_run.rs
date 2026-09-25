//! Welle 6: ein über den [`crate::child_backend::ChildBackend`] laufendes
//! Kind (`ManagedAgentSpawner::run_child_via_backend`).
//!
//! Befund: der Spawner baute die [`ChildRunSpec`] mit
//! `ChildBackendRights::default()` — ein job-gestartetes Kind eines
//! kompilierten Elternteils durfte gar nichts. Diese Tests fahren den echten
//! Pfad (Admission unter einer externen Wurzel, `run_child`) gegen ein
//! Backend-Doppel, das jede empfangene [`ChildRunSpec`] aufzeichnet und ein
//! vorgegebenes Drehbuch abspielt.

use harw_agent_dsl::roles::AgentRoleId;
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use harw_extension_api::{ToolExecutor, ToolProvider, ToolSpec};
use harw_tools::{FunctionToolSpec, JsonSchema};
use tokio::sync::oneshot;

use super::*;
use crate::child_approval::{ChildApprovalAnswer, ChildApprovalBroker, ChildApprovalRequest};
use crate::child_backend::{
    ChildBackend, ChildBackendFuture, ChildBackendRights, ChildIo, ChildRunOutcome, ChildRunSpec,
    ChildRunStatus,
};
use crate::child_comms::ChildEndStatus;

/// Werkzeuge der Kind-Registry: `fs.write` schneidet der Elternteil weg,
/// `web.fetch` admittiert die Agent-IR des Kindes nicht.
const REGISTRY_TOOLS: &[&str] = &["fs.read", "fs.write", "host.sudo_exec", "web.fetch"];

/// Agent-IR des Kindes.
const CHILD_IR: &str = r#"
[tools]
admitted = ["fs.read", "fs.write", "host.sudo_exec"]
"#;

/// Was das Backend-Doppel bei einem Lauf tut.
enum Script {
    /// Liefert sofort dieses Ergebnis.
    Return(ChildRunOutcome),
    /// Meldet ein Ereignis, stellt eine Frage und bittet um eine Freigabe;
    /// endet `Completed` mit `"<antwort>|<entscheidung>"`.
    Converse,
    /// Wartet auf `spec.cancel` und endet `Cancelled`.
    AwaitCancel,
}

/// Backend-Doppel: zeichnet jede [`ChildRunSpec`] auf und spielt je Lauf
/// das nächste [`Script`] ab (ohne Drehbuch: `Completed` ohne Text).
struct FakeBackend {
    scripts: Mutex<VecDeque<Script>>,
    specs: Mutex<Vec<ChildRunSpec>>,
}

impl FakeBackend {
    fn new(scripts: Vec<Script>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            specs: Mutex::new(Vec::new()),
        })
    }

    fn specs(&self) -> Vec<ChildRunSpec> {
        self.specs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl ChildBackend for FakeBackend {
    fn run<'a>(&'a self, spec: ChildRunSpec, io: &'a dyn ChildIo) -> ChildBackendFuture<'a> {
        self.specs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(spec.clone());
        let script = self
            .scripts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front();
        Box::pin(async move {
            match script {
                Some(Script::Return(outcome)) => outcome,
                Some(Script::Converse) => {
                    io.on_event(serde_json::json!({ "assistant_text": "Zwischenstand" }));
                    let answer = io
                        .on_question("q-1".to_owned(), "Tabelle A oder B?".to_owned())
                        .await;
                    let decision = io
                        .on_approval_request(
                            "call-1".to_owned(),
                            "fs.read".to_owned(),
                            "src/lib.rs".to_owned(),
                        )
                        .await;
                    outcome(
                        ChildRunStatus::Completed,
                        Some(format!("{answer}|{decision}")),
                        None,
                    )
                }
                Some(Script::AwaitCancel) => {
                    spec.cancel.cancelled().await;
                    outcome(
                        ChildRunStatus::Cancelled {
                            reason: "parent".to_owned(),
                        },
                        None,
                        None,
                    )
                }
                None => outcome(ChildRunStatus::Completed, None, None),
            }
        })
    }
}

fn outcome(
    status: ChildRunStatus,
    text: Option<String>,
    continuation: Option<String>,
) -> ChildRunOutcome {
    ChildRunOutcome {
        status,
        text,
        usage: ChildUsage::default(),
        continuation,
    }
}

/// Werkzeuge ohne Ausführer — es zählt nur die Fläche.
struct NamedTools;

impl ToolProvider for NamedTools {
    fn tools(&self) -> Vec<ToolSpec> {
        REGISTRY_TOOLS
            .iter()
            .map(|name| {
                ToolSpec::Function(FunctionToolSpec {
                    name: ToolName::new(*name),
                    description: "Testwerkzeug".to_owned(),
                    parameters: JsonSchema::default(),
                    strict: false,
                })
            })
            .collect()
    }

    fn executor(&self, _name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        None
    }
}

struct BackendChildRegistry {
    ir: ExecutableAgentIr,
}

impl ChildRegistryFactory for BackendChildRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default()
            .tool_provider(Arc::new(NamedTools))
            .build())
    }

    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::new(EchoModelProvider::new("verdichtet")))
    }

    fn executable_agent_ir(&self, _role: &str) -> Option<&ExecutableAgentIr> {
        Some(&self.ir)
    }
}

/// Freigabe-Broker-Doppel: zeichnet auf und gibt jede Frage frei.
struct ApprovingBroker {
    seen: Mutex<Vec<ChildApprovalRequest>>,
}

impl ChildApprovalBroker for ApprovingBroker {
    fn submit(
        &self,
        request: ChildApprovalRequest,
    ) -> Option<oneshot::Receiver<ChildApprovalAnswer>> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request);
        let (reply, receiver) = oneshot::channel();
        let _ = reply.send(ChildApprovalAnswer::Approve);
        Some(receiver)
    }
}

struct Fixture {
    spawner: ManagedAgentSpawner,
    backend: Arc<FakeBackend>,
    parent: SessionId,
    child: SessionId,
    parent_activation: SessionActivation,
}

/// Externe Wurzel (Lesen + Schreiben, ohne `fs.write` in ihrer Aktivierung)
/// mit einem admittierten, backend-gefahrenen `worker`-Kind.
fn backend_fixture(scripts: Vec<Script>, mode: Option<ApprovalModeCell>) -> TestResult<Fixture> {
    let (events, _receiver) = mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    let sandbox = test_sandbox(PermissionSet::from_policy([
        Permission::ReadWorkspace,
        Permission::WriteWorkspace,
    ]))?;
    let parent = SessionId::new();
    let mut parent_activation = SessionActivation::default();
    parent_activation.disable_tool(ToolName::new("fs.write"));
    let backend = FakeBackend::new(scripts);
    let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
        .with_role(
            "worker",
            AgentRole::Agent {
                name: "worker".to_owned(),
            },
            AgentRoleId::Worker,
            Arc::new(BackendChildRegistry {
                ir: test_agent_ir(CHILD_IR)?,
            }),
        )
        .with_child_backend(Arc::clone(&backend) as Arc<dyn ChildBackend>)
        .with_external_root_parent(
            parent.clone(),
            external_root_context(sandbox.clone(), AgentRoleId::RootOrchestrator),
            None,
            parent_activation.clone(),
        )
        .map_err(ctx("trusted external root registers during construction"))?;
    let spawner = match mode {
        Some(mode) => spawner.with_approval_mode(mode),
        None => spawner,
    };
    let child = spawner
        .admit("worker", spawn_input(parent.clone()), sandbox, None)
        .map_err(ctx("the backend child is admitted"))?;
    Ok(Fixture {
        spawner,
        backend,
        parent,
        child,
        parent_activation,
    })
}

/// Werkzeugfläche und Sandbox, mit denen das Kind in-process liefe.
fn in_process_surface(fixture: &Fixture) -> TestResult<(BTreeSet<String>, SandboxSpec)> {
    let manager = fixture
        .spawner
        .manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let session = manager
        .get(&fixture.child)
        .map_err(ctx("child is manager-owned"))?;
    let surface = crate::turn_loop::collect_tools(session)
        .map_err(ctx("collect_tools"))?
        .iter()
        .map(|spec| spec.name().to_owned())
        .collect();
    let sandbox = session
        .spawn_context()
        .map(|context| context.sandbox.clone())
        .ok_or(TestError::Missing("child sandbox"))?;
    Ok((surface, sandbox))
}

fn only_spec(backend: &FakeBackend) -> TestResult<ChildRunSpec> {
    let mut specs = backend.specs();
    if specs.len() != 1 {
        return Err(TestError::Unexpected(format!(
            "expected exactly one run, got {}",
            specs.len()
        )));
    }
    specs.pop().ok_or(TestError::Missing("recorded spec"))
}

fn child_status(fixture: &Fixture) -> TestResult<ChildStatus> {
    fixture
        .spawner
        .child_record(&fixture.child)
        .map(|record| record.status)
        .ok_or(TestError::Missing("child record"))
}

/// (a) Die Rechte der Spec sind genau die in-process Fläche des Kindes —
/// nie mehr, als der Elternteil hat; `full_access` folgt dem Freigabemodus.
#[tokio::test]
async fn the_backend_gets_the_in_process_rights_never_more_than_the_parent() -> TestResult {
    let mode = ApprovalModeCell::new(ApprovalMode::FullAccess);
    let fixture = backend_fixture(Vec::new(), Some(mode.clone()))?;
    let (surface, sandbox) = in_process_surface(&fixture)?;

    fixture
        .spawner
        .run_child(
            &fixture.child,
            &InMemoryStateStore::new(),
            TurnInput::default(),
        )
        .await
        .map_err(|error| TestError::Unexpected(error.message))?;
    let rights = only_spec(&fixture.backend)?.rights;

    assert_eq!(rights.tools, surface);
    assert!(rights.tools.contains("fs.read"), "{:?}", rights.tools);
    assert!(
        !rights.tools.contains("fs.write"),
        "the parent's activation cuts fs.write"
    );
    assert!(
        !rights.tools.contains("web.fetch"),
        "the child's IR does not admit web.fetch"
    );
    for tool in &rights.tools {
        assert!(
            fixture
                .parent_activation
                .is_tool_enabled(&ToolName::new(tool.as_str())),
            "{tool} exceeds the parent"
        );
    }
    assert_eq!(
        rights.write,
        sandbox.permissions().contains(Permission::WriteWorkspace)
    );
    assert!(!rights.shell, "neither sandbox grants ExecuteProcess");
    assert!(!rights.network_open);
    assert!(rights.network_hosts.is_empty());
    assert_eq!(rights.host, rights.tools.contains("host.sudo_exec"));
    assert!(rights.full_access);

    // Ohne Full Access keine automatische Freigabe; die Fläche bleibt.
    mode.set(ApprovalMode::AlwaysAsk);
    let asking = fixture
        .spawner
        .backend_rights(&fixture.child, &fixture.parent);
    assert!(!asking.full_access);
    assert_eq!(asking.tools, surface);
    // Unbekannter Elternteil: fail-closed.
    assert_eq!(
        fixture
            .spawner
            .backend_rights(&fixture.child, &SessionId::new()),
        ChildBackendRights::default()
    );
    Ok(())
}

/// (a) Ohne angebundene Freigabemodus-Zelle nie `full_access`.
#[test]
fn without_an_approval_mode_the_backend_never_gets_full_access() -> TestResult {
    let fixture = backend_fixture(Vec::new(), None)?;
    let rights = fixture
        .spawner
        .backend_rights(&fixture.child, &fixture.parent);
    assert!(!rights.full_access);
    assert!(rights.tools.contains("fs.read"), "{:?}", rights.tools);
    Ok(())
}

/// (b) `Completed` wird die Antwort des Kindes.
#[tokio::test]
async fn a_completed_backend_run_maps_to_the_answer() -> TestResult {
    let fixture = backend_fixture(
        vec![Script::Return(outcome(
            ChildRunStatus::Completed,
            Some("Die Antwort".to_owned()),
            None,
        ))],
        None,
    )?;
    let result = fixture
        .spawner
        .run_child(
            &fixture.child,
            &InMemoryStateStore::new(),
            TurnInput::default(),
        )
        .await
        .map_err(|error| TestError::Unexpected(error.message))?;
    assert!(matches!(result.outcome, TurnOutcome::Completed));
    assert_eq!(result.full_text.as_deref(), Some("Die Antwort"));
    assert!(!result.budget_exhausted);
    assert!(matches!(child_status(&fixture)?, ChildStatus::Completed));
    assert!(fixture.spawner.comms.end_report(&fixture.child).is_none());
    Ok(())
}

/// (c) `Crashed` und `Failed` werden ein `ChildEnd` mit der Ursache aus
/// `to_child_end_cause`.
#[tokio::test]
async fn crashed_and_failed_backend_runs_become_a_child_end_with_the_matching_cause() -> TestResult
{
    let statuses = [
        ChildRunStatus::Crashed {
            exit_code: Some(137),
            stderr_tail: "panicked at src/main.rs".to_owned(),
        },
        ChildRunStatus::Failed {
            reason: "provider timeout".to_owned(),
        },
    ];
    for status in statuses {
        let cause = status
            .to_child_end_cause()
            .ok_or(TestError::Missing("non-regular end has a cause"))?;
        let fixture = backend_fixture(vec![Script::Return(outcome(status, None, None))], None)?;
        let result = fixture
            .spawner
            .run_child(
                &fixture.child,
                &InMemoryStateStore::new(),
                TurnInput::default(),
            )
            .await;
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "a crashed/failed backend run must not succeed".to_owned(),
            ));
        };
        let report = fixture
            .spawner
            .comms
            .end_report(&fixture.child)
            .ok_or(TestError::Missing("end report"))?;
        assert_eq!(report.status, cause.status());
        assert_eq!(report.status, ChildEndStatus::Failed);
        assert_eq!(report.reason, cause.reason_de());
        assert!(matches!(child_status(&fixture)?, ChildStatus::Failed));
        if let crate::child_comms::ChildEndCause::TurnError(reason) = &cause {
            assert_eq!(&error.message, reason);
        }
    }
    Ok(())
}

/// (d) Ereignis, Frage und Freigabe des Backends erreichen Beobachter,
/// `ask_parent` und den Freigabe-Relais über `ControllerChildIo`.
#[tokio::test]
async fn events_questions_and_approvals_reach_the_parent_relays() -> TestResult {
    let fixture = backend_fixture(vec![Script::Converse], None)?;
    let broker = Arc::new(ApprovingBroker {
        seen: Mutex::new(Vec::new()),
    });
    fixture
        .spawner
        .attach_child_approval_broker(Arc::clone(&broker) as Arc<dyn ChildApprovalBroker>);
    let store = InMemoryStateStore::new();
    let run = fixture
        .spawner
        .run_child(&fixture.child, &store, TurnInput::default());
    let answer = async {
        for _ in 0..500 {
            if fixture.spawner.comms.has_pending_question(&fixture.child) {
                break;
            }
            tokio::task::yield_now().await;
        }
        let seen_text = fixture
            .spawner
            .comms
            .journal(&fixture.child)
            .and_then(|journal| journal.last_assistant().map(str::to_owned));
        let delivered =
            fixture
                .spawner
                .send_message_to_child(&fixture.parent, fixture.child.as_str(), "B");
        (seen_text, delivered)
    };
    let (result, (seen_text, delivered)) = tokio::join!(run, answer);

    // Beobachter: der Assistententext des Ereignisses liegt im Journal.
    assert_eq!(seen_text.as_deref(), Some("Zwischenstand"));
    // `ask_parent`: die Frage wurde über `agent.message` beantwortet.
    assert_eq!(
        delivered.map_err(|error| TestError::Unexpected(error.message))?,
        crate::child_comms::MessageDelivery::AnsweredQuestion
    );
    let result = result.map_err(|error| TestError::Unexpected(error.message))?;
    assert_eq!(result.full_text.as_deref(), Some("B|approve"));
    // Freigabe-Relais: genau eine Frage, mit Kind und Werkzeug.
    let seen = broker
        .seen
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert_eq!(seen.len(), 1);
    let request = seen.first().ok_or(TestError::Missing("approval request"))?;
    assert_eq!(request.child, fixture.child);
    assert_eq!(request.role, "worker");
    assert_eq!(request.call.name.as_str(), "fs.read");
    Ok(())
}

/// (e) Ein Abbruch des Kindes erreicht `spec.cancel`.
#[tokio::test]
async fn cancelling_the_child_reaches_the_spec_cancel_token() -> TestResult {
    let fixture = backend_fixture(vec![Script::AwaitCancel], None)?;
    let store = InMemoryStateStore::new();
    let run = fixture
        .spawner
        .run_child(&fixture.child, &store, TurnInput::default());
    let cancel = async {
        for _ in 0..500 {
            if !fixture.backend.specs().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        fixture.spawner.request_cancellation(&fixture.child)
    };
    let (result, cancelled) = tokio::join!(run, cancel);
    assert!(cancelled);
    assert!(result.is_err(), "a cancelled run must not succeed");
    let spec = only_spec(&fixture.backend)?;
    assert!(spec.cancel.is_cancelled());
    assert!(matches!(child_status(&fixture)?, ChildStatus::Cancelled));
    let report = fixture
        .spawner
        .comms
        .end_report(&fixture.child)
        .ok_or(TestError::Missing("end report"))?;
    assert_eq!(report.status, ChildEndStatus::Cancelled);
    Ok(())
}

/// (f) Das Fortsetzungs-Token eines Laufs wird gespeichert und geht als
/// `continue_from` in den nächsten Lauf desselben Kindes.
#[tokio::test]
async fn the_continuation_token_is_stored_for_the_next_run() -> TestResult {
    let fixture = backend_fixture(
        vec![Script::Return(outcome(
            ChildRunStatus::BudgetExhausted,
            Some("Teilergebnis".to_owned()),
            Some("tok-1".to_owned()),
        ))],
        None,
    )?;
    let store = InMemoryStateStore::new();
    let first = fixture
        .spawner
        .run_child(&fixture.child, &store, TurnInput::default())
        .await
        .map_err(|error| TestError::Unexpected(error.message))?;
    assert!(first.budget_exhausted);
    assert_eq!(first.full_text.as_deref(), Some("Teilergebnis"));

    fixture
        .spawner
        .run_child(&fixture.child, &store, TurnInput::default())
        .await
        .map_err(|error| TestError::Unexpected(error.message))?;
    let specs = fixture.backend.specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(
        specs.first().and_then(|spec| spec.continue_from.clone()),
        None
    );
    assert_eq!(
        specs.get(1).and_then(|spec| spec.continue_from.clone()),
        Some("tok-1".to_owned())
    );
    // Verbraucht: ohne neues Token keine weitere Fortsetzung.
    assert_eq!(fixture.spawner.take_backend_resume(&fixture.child), None);
    Ok(())
}
