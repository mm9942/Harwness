//! Tests zu Kind-Freigaben, Lease-Herzschlag und entkoppeltem
//! Hintergrund-Token (Runde 5, Teil O).
//!
//! Als Untermodul der Controller-Tests, damit die vorhandene
//! Testinfrastruktur ([`runnable_children`], [`AskUserApproval`]) und die
//! privaten Registries des Spawners nutzbar sind.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use harw_extension_api::{
    ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolProvider, ToolSpec,
};
use harw_protocol::items::{ToolCallResult, TurnItem};
use harw_tools::{FunctionToolSpec, JsonSchema};
use tokio::sync::oneshot;

use super::*;
use crate::child_approval::{
    ChildApprovalAnswer, ChildApprovalBroker, ChildApprovalRequest, REASON_CHILD_APPROVAL_TIMEOUT,
    REASON_CHILD_APPROVAL_UNDELIVERABLE, REASON_TOOL_OUTSIDE_ROLE,
};

/// Name des schreibenden Test-Werkzeugs der Kind-Rolle.
const PROBE_TOOL: &str = "probe.write";

/// Modell: erst ein Aufruf von `tool`, danach eine Textantwort.
struct OneToolThenDone {
    tool: &'static str,
    calls: AtomicUsize,
}

impl ModelProvider for OneToolThenDone {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        let round = self.calls.fetch_add(1, Ordering::SeqCst);
        let tool = self.tool;
        Box::pin(async move {
            if round == 0 {
                Ok(ModelResponse {
                    message: None,
                    tool_calls: vec![ToolCall {
                        id: ToolCallId::new(),
                        name: ToolName::new(tool),
                        arguments: serde_json::json!({ "path": "src/lib.rs" }),
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

struct OneToolRegistry {
    tool: &'static str,
}

impl ChildRegistryFactory for OneToolRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default().build())
    }

    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::new(OneToolThenDone {
            tool: self.tool,
            calls: AtomicUsize::new(0),
        }))
    }
}

/// Werkzeug der Kind-Rolle; zählt seine Ausführungen.
struct ProbeTool {
    runs: Arc<AtomicUsize>,
}

impl ToolProvider for ProbeTool {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(PROBE_TOOL),
            description: "schreibt probeweise".to_owned(),
            parameters: JsonSchema::default(),
            strict: false,
        })]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        (name.as_str() == PROBE_TOOL).then(|| {
            Arc::new(ProbeExecutor {
                runs: Arc::clone(&self.runs),
            }) as Arc<dyn ToolExecutor>
        })
    }
}

struct ProbeExecutor {
    runs: Arc<AtomicUsize>,
}

impl ToolExecutor for ProbeExecutor {
    fn execute<'a>(
        &'a self,
        _ctx: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(ToolOutput::Text {
                content: "geschrieben".to_owned(),
            })
        })
    }
}

/// Broker-Doppel: zeichnet Fragen auf und antwortet fest (oder gar nicht).
struct ScriptedBroker {
    answer: Option<ChildApprovalAnswer>,
    seen: Mutex<Vec<ChildApprovalRequest>>,
    /// Hält unbeantwortete Antwortkanäle offen (Zeitablauf-Test).
    parked: Mutex<Vec<oneshot::Sender<ChildApprovalAnswer>>>,
}

impl ScriptedBroker {
    fn answering(answer: Option<ChildApprovalAnswer>) -> Arc<Self> {
        Arc::new(Self {
            answer,
            seen: Mutex::new(Vec::new()),
            parked: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<ChildApprovalRequest> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl ChildApprovalBroker for ScriptedBroker {
    fn submit(
        &self,
        request: ChildApprovalRequest,
    ) -> Option<oneshot::Receiver<ChildApprovalAnswer>> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request);
        let (reply, receiver) = oneshot::channel();
        match &self.answer {
            Some(answer) => {
                let _ = reply.send(answer.clone());
            }
            None => self
                .parked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(reply),
        }
        Some(receiver)
    }
}

/// Ein pausierverbotenes Kind mit `PROBE_TOOL` und einer Freigabekette, die
/// immer fragt.
fn asking_child(runs: &Arc<AtomicUsize>) -> TestResult<(ManagedAgentSpawner, SessionId)> {
    let runs = Arc::clone(runs);
    let (spawner, children) = runnable_children(
        Arc::new(OneToolRegistry { tool: PROBE_TOOL }),
        false,
        1,
        move || {
            ExtensionRegistryBuilder::default()
                .tool_provider(Arc::new(ProbeTool {
                    runs: Arc::clone(&runs),
                }))
                .approval_handler(Arc::new(AskUserApproval))
                .build()
        },
    )?;
    let child = children
        .first()
        .cloned()
        .ok_or(TestError::Missing("ein admittiertes Kind"))?;
    Ok((spawner, child))
}

/// Das letzte Werkzeugergebnis im Verlauf des (zurückgelegten) Kindes.
fn last_tool_result(
    spawner: &ManagedAgentSpawner,
    child: &SessionId,
) -> TestResult<ToolCallResult> {
    let manager = spawner
        .manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let session = manager
        .get(child)
        .map_err(ctx("Kind liegt wieder im Manager"))?;
    session
        .history()
        .items()
        .iter()
        .rev()
        .find_map(|item| match item {
            TurnItem::ToolResult(result) => Some(result.result.clone()),
            _ => None,
        })
        .ok_or(TestError::Missing("ein Werkzeugergebnis im Kind-Verlauf"))
}

#[tokio::test]
async fn an_approved_child_request_runs_the_tool_and_names_the_requester() -> TestResult {
    let runs = Arc::new(AtomicUsize::new(0));
    let (spawner, child) = asking_child(&runs)?;
    let broker = ScriptedBroker::answering(Some(ChildApprovalAnswer::Approve));
    spawner.attach_child_approval_broker(broker.clone());
    let store = InMemoryStateStore::new();

    let run = spawner
        .run_child(&child, &store, TurnInput::user("arbeite"))
        .await
        .map_err(ctx("das Kind läuft nach der Freigabe zu Ende"))?;

    assert!(matches!(run.outcome, TurnOutcome::Completed));
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "das Werkzeug lief genau einmal"
    );
    let seen = broker.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].role, "worker");
    assert_eq!(seen[0].tree_path, vec!["worker".to_owned()]);
    assert_eq!(seen[0].call.name.as_str(), PROBE_TOOL);
    assert_eq!(seen[0].requester_label(), "worker (Wurzel › worker)");
    assert!(last_tool_result(&spawner, &child)?.is_success());
    Ok(())
}

#[tokio::test]
async fn a_rejected_child_request_becomes_a_tool_error_not_a_crash() -> TestResult {
    let runs = Arc::new(AtomicUsize::new(0));
    let (spawner, child) = asking_child(&runs)?;
    spawner.attach_child_approval_broker(ScriptedBroker::answering(Some(
        ChildApprovalAnswer::Reject {
            reason: Some("nicht jetzt".to_owned()),
        },
    )));
    let store = InMemoryStateStore::new();

    let run = spawner
        .run_child(&child, &store, TurnInput::user("arbeite"))
        .await
        .map_err(ctx("eine Ablehnung beendet das Kind regulär"))?;

    assert!(matches!(run.outcome, TurnOutcome::Completed));
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "abgelehnt heißt nicht ausgeführt"
    );
    let result = last_tool_result(&spawner, &child)?;
    assert!(!result.is_success());
    assert!(format!("{result:?}").contains("nicht jetzt"), "{result:?}");
    Ok(())
}

#[tokio::test]
async fn an_unanswered_child_request_times_out_into_a_tool_error() -> TestResult {
    let runs = Arc::new(AtomicUsize::new(0));
    let (spawner, child) = asking_child(&runs)?;
    spawner.attach_child_approval_broker(ScriptedBroker::answering(None));
    // Produktiv 10 min; hier kurz, damit der Test schnell bleibt.
    spawner
        .child_approval_relay()
        .set_timeout(Duration::from_millis(50));
    let store = InMemoryStateStore::new();

    let run = spawner
        .run_child(&child, &store, TurnInput::user("arbeite"))
        .await
        .map_err(ctx("ein Zeitablauf beendet das Kind regulär"))?;

    assert!(matches!(run.outcome, TurnOutcome::Completed));
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let result = last_tool_result(&spawner, &child)?;
    assert!(
        format!("{result:?}").contains(REASON_CHILD_APPROVAL_TIMEOUT),
        "{result:?}"
    );
    Ok(())
}

/// Keine Rechte-Erweiterung: ein Werkzeug außerhalb der Werkzeugfläche des
/// Kindes wird abgelehnt, ohne die Nutzerin überhaupt zu fragen.
#[tokio::test]
async fn a_request_outside_the_role_is_rejected_without_asking() -> TestResult {
    let runs = Arc::new(AtomicUsize::new(0));
    let (spawner, children) = runnable_children(
        Arc::new(OneToolRegistry {
            tool: "host.sudo_exec",
        }),
        false,
        1,
        || {
            ExtensionRegistryBuilder::default()
                .approval_handler(Arc::new(AskUserApproval))
                .build()
        },
    )?;
    let child = children
        .first()
        .cloned()
        .ok_or(TestError::Missing("ein admittiertes Kind"))?;
    let broker = ScriptedBroker::answering(Some(ChildApprovalAnswer::Approve));
    spawner.attach_child_approval_broker(broker.clone());
    let store = InMemoryStateStore::new();

    let run = spawner
        .run_child(&child, &store, TurnInput::user("arbeite"))
        .await
        .map_err(ctx("das Kind endet regulär"))?;

    assert!(matches!(run.outcome, TurnOutcome::Completed));
    assert!(
        broker.seen().is_empty(),
        "die Nutzerin wird gar nicht gefragt"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    let result = last_tool_result(&spawner, &child)?;
    assert!(
        format!("{result:?}").contains(REASON_TOOL_OUTSIDE_ROLE),
        "{result:?}"
    );
    Ok(())
}

/// Ohne angebundenen Kanal (Nicht-TUI) bleibt es beim fail-closed-Verhalten:
/// seit 7b8578a wird die Freigabe nicht mehr per Turn-Abbruch
/// (`awaiting_approval`) beantwortet, sondern das Kind bekommt einen
/// ablehnenden Werkzeugfehler und läuft regulär weiter. Entscheidend bleibt:
/// das freigabepflichtige Werkzeug läuft **nie** (0 Läufe), das Kind endet
/// nicht pausiert und die Ablehnung steht als Werkzeugergebnis fest.
#[tokio::test]
async fn without_a_relay_the_child_still_fails_closed() -> TestResult {
    let runs = Arc::new(AtomicUsize::new(0));
    let (spawner, child) = asking_child(&runs)?;
    let store = InMemoryStateStore::new();

    let run = spawner
        .run_child(&child, &store, TurnInput::user("arbeite"))
        .await
        .map_err(ctx("das Kind endet regulär, ohne das Werkzeug zu starten"))?;

    assert!(
        matches!(run.outcome, TurnOutcome::Completed),
        "kein pausierter oder erfolgreicher Fremdausgang: {:?}",
        run.outcome
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "das freigabepflichtige Werkzeug darf ohne Kanal nie laufen"
    );
    let result = last_tool_result(&spawner, &child)?;
    assert!(
        format!("{result:?}").contains(REASON_CHILD_APPROVAL_UNDELIVERABLE),
        "{result:?}"
    );
    Ok(())
}

/// Der Herzschlag verlängert die Lease des arbeitenden Kindes **und** seiner
/// Vorfahren; ohne ihn liefe sie mitten in der Arbeit ab.
#[test]
fn the_lease_heartbeat_keeps_a_busy_child_and_its_ancestors_alive() -> TestResult {
    let (spawner, child) = spawner_with_admitted_child()?;
    let lease = spawner.limits().lease_seconds;
    let start = Timestamp::now();
    let initial = start
        .checked_add(SignedDuration::from_secs(lease))
        .map_err(ctx("Lease-Ende berechenbar"))?;
    // Ein admittierter Orchestrator als Elternteil des Kindes.
    let orchestrator = SessionId::new();
    {
        let mut active = spawner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut parent_record = active
            .get(child.as_str())
            .cloned()
            .ok_or(TestError::Missing("Record des Kindes"))?;
        parent_record.child = orchestrator.clone();
        parent_record.lease_expires_at = initial;
        active.insert(orchestrator.as_str().to_owned(), parent_record);
        if let Some(record) = active.get_mut(child.as_str()) {
            record.lease_expires_at = initial;
            record.parent = orchestrator.clone();
        }
    }

    // Herzschlag nach zwei Dritteln der Lease (ein langes Werkzeug meldet
    // bis dahin keinen Fortschritt).
    let beat = start
        .checked_add(SignedDuration::from_secs(lease * 2 / 3))
        .map_err(ctx("Herzschlag-Zeitpunkt"))?;
    let renewed = crate::child_lease_heartbeat::renew_lease_chain(&spawner, &child, beat);
    assert_eq!(renewed, 2, "Kind und Orchestrator werden verlängert");

    // Anderthalb Lease-Dauern nach dem Start: ohne Herzschlag abgelaufen.
    let later = start
        .checked_add(SignedDuration::from_secs(lease * 3 / 2))
        .map_err(ctx("Prüfzeitpunkt"))?;
    assert!(
        later >= initial,
        "ohne Verlängerung wäre die Lease hier abgelaufen"
    );
    let expired = spawner.reap_expired(later);
    assert!(
        expired.is_empty(),
        "keine Lease läuft mitten in der Arbeit ab: {expired:?}"
    );
    for session in [&child, &orchestrator] {
        let record = spawner
            .child_record(session)
            .ok_or(TestError::Missing("noch admittiert"))?;
        assert!(record.lease_expires_at > later);
    }
    Ok(())
}

/// Ein abgekoppeltes Hintergrund-Kind überlebt den Abbruch des Turns, der es
/// gestartet hat; gezielt abbrechen lässt es sich weiterhin.
#[test]
fn a_background_child_gets_its_own_cancel_token() -> TestResult {
    let (spawner, child) = spawner_with_admitted_child()?;
    let parent_token = CancelToken::new();
    spawner
        .cancellations
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(child.as_str().to_owned(), parent_token.child());

    spawner
        .detach_for_background(&child, Some("Umsetzung"))
        .map_err(ctx("das Kind lässt sich abkoppeln"))?;
    parent_token.cancel(CancelReason::User);

    let token = spawner
        .child_cancel_token(&child)
        .ok_or(TestError::Missing("Token des Kindes"))?;
    assert!(!token.is_cancelled(), "der Turn-Abbruch reißt es nicht mit");
    assert!(spawner.request_cancellation(&child));
    assert!(token.is_cancelled(), "gezielter Abbruch wirkt weiterhin");
    Ok(())
}
