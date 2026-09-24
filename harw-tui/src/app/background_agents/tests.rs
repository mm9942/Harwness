//! Tests der Hintergrund-Agenten in der TUI (Runde 5, Teil K).

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crossterm::event::{KeyCode, KeyEvent};
use harw_agent_dsl::roles::AgentRoleId;
use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_catalog::AgentSuggestions;
use harw_core::{
    ChildLimits, ChildRegistryFactory, EchoModelProvider, InMemoryStateStore, ModelFuture,
    ModelProvider, ModelRequest, ModelResponse, SessionActivation, SessionManager, SpawnContext,
};
use harw_extension_api::{
    AgentSpawnError, AgentSpawner, ExtensionRegistry, ExtensionRegistryBuilder, SpawnInput,
};
use harw_types::{AgentRole, ApprovalActor, TenantId, WorkspaceId};

use super::*;
use crate::test_support::{TestError, TestResult, ctx};

// ── Fixtures ──────────────────────────────────────────────────────────────────

fn test_sandbox() -> TestResult<SandboxSpec> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "harw-tui-background-test-{}-{}",
        std::process::id(),
        id
    ));
    std::fs::create_dir_all(root.join("workspace")).map_err(ctx("temp workspace dir"))?;
    let registry = WorkspaceRegistry::build(
        &root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("tui-background-test"),
            workspace: WorkspaceId::from_str("workspace"),
            root: PathBuf::from("workspace"),
        }],
    )
    .map_err(ctx("workspace registry build"))?;
    let binding = registry
        .resolve(
            &TenantId::from_str("tui-background-test"),
            &WorkspaceId::from_str("workspace"),
        )
        .map_err(ctx("resolve workspace binding"))?;
    Ok(SandboxSpec::from_resolved(
        binding,
        PermissionSet::from_policy([Permission::ReadWorkspace]),
    ))
}

/// Kind-Registry mit Echo-Modell (endet sofort) oder hängendem Modell
/// (endet nur durch Abbruch).
struct TestChildRegistry {
    hanging: bool,
}

/// Modell, das nie antwortet — nur der kooperative Abbruch beendet den Turn.
struct HangingModel;

impl ModelProvider for HangingModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move {
            std::future::pending::<()>().await;
            Ok(ModelResponse::text("unreachable"))
        })
    }
}

impl ChildRegistryFactory for TestChildRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default().build())
    }

    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        if self.hanging {
            Ok(Arc::new(HangingModel))
        } else {
            Ok(Arc::new(EchoModelProvider::new(
                "Ergebnis des Orchestrators",
            )))
        }
    }
}

/// UIA-Wurzel mit Spawner (Rollen `root-orchestrator`, `uia-worker`), Launcher
/// und passender `ChatApp`.
struct Fixture {
    session: AgentSession,
    spawner: Arc<ManagedAgentSpawner>,
    launcher: BackgroundLauncher,
    app: ChatApp,
    sandbox: SandboxSpec,
}

fn fixture(hanging: bool) -> TestResult<Fixture> {
    let sandbox = test_sandbox()?;
    let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let session = AgentSession::new(
        AgentRole::Assistant,
        None,
        ExtensionRegistryBuilder::default().build(),
        events.clone(),
    );
    let root = session.id().clone();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    let role = |name: &str| AgentRole::Agent {
        name: name.to_owned(),
    };
    let spawner = Arc::new(
        ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "root-orchestrator",
                role("root-orchestrator"),
                AgentRoleId::RootOrchestrator,
                Arc::new(TestChildRegistry { hanging }),
            )
            .with_role(
                "uia-worker",
                role("uia-worker"),
                AgentRoleId::UiaWorker,
                Arc::new(TestChildRegistry { hanging }),
            )
            .with_external_root_parent(
                root.clone(),
                SpawnContext {
                    sandbox: sandbox.clone(),
                    suggestions: None,
                    capability_snapshot: None,
                    approval_actor: Some(ApprovalActor::Operator {
                        id: "tui-background-test".to_owned(),
                    }),
                    organizational_role: AgentRoleId::UserInterface,
                    allowed_child_orchestrators: Vec::new(),
                    trace: None,
                    ceiling: None,
                },
                None,
                SessionActivation::default(),
            )
            .map_err(ctx("UIA-Wurzel registriert"))?,
    );
    let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let launcher = BackgroundLauncher::new(
        root.clone(),
        Arc::clone(&spawner),
        store,
        None,
        OrchestratorRoles::builtin(),
    );
    let app = ChatApp::new(Vec::new(), sandbox.clone(), root)
        .with_managed_spawner(Some(Arc::clone(&spawner)));
    Ok(Fixture {
        session,
        spawner,
        launcher,
        app,
        sandbox,
    })
}

impl Fixture {
    /// Admittiert ein Kind der Wurzel und legt den zugehörigen Handoff-Aufruf
    /// in den Verlauf (wie `transfer_to_<role>`).
    async fn handoff(
        &mut self,
        role: &str,
        arguments: serde_json::Value,
    ) -> TestResult<(SessionId, ToolCallId)> {
        let call_id = ToolCallId::new();
        self.session.history_mut().push_tool_call(
            call_id.clone(),
            format!("transfer_to_{role}"),
            arguments.clone(),
        );
        let child = self
            .spawner
            .spawn_child(
                role,
                SpawnInput {
                    parent_session_id: self.session.id().clone(),
                    handoff_call_id: call_id.clone(),
                    instructions: Some("Auftrag".to_owned()),
                    context: arguments,
                    ceiling: None,
                },
                self.sandbox.clone(),
                None,
            )
            .await
            .map_err(ctx("Kind wird admittiert"))?;
        Ok((child, call_id))
    }
}

/// Wartet (begrenzt) auf die Abschlussmeldung eines Hintergrund-Laufs.
async fn wait_until_finished(spawner: &ManagedAgentSpawner, parent: &SessionId) -> TestResult {
    let registry = Arc::clone(spawner.background_children());
    for _ in 0..50 {
        if registry.has_notices(parent) {
            return Ok(());
        }
        let _ = tokio::time::timeout(Duration::from_millis(100), registry.notified()).await;
    }
    Err(TestError::Missing("der Hintergrund-Lauf meldet sich nicht"))
}

// ── Entscheidung ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_uia_orchestrator_handoff_returns_immediately_with_running_status() -> TestResult {
    let mut fx = fixture(true)?;
    let (child, call_id) = fx
        .handoff("root-orchestrator", json!({ "task": "Baue X" }))
        .await?;
    let result = fx
        .launcher
        .try_launch(&fx.session, &child, &call_id, "root-orchestrator")
        .ok_or(TestError::Missing("Orchestrator läuft im Hintergrund"))?;
    match result {
        ToolCallResult::Success { value } => {
            assert_eq!(value["status"], json!("running"));
            assert_eq!(value["child_id"], json!(child.as_str()));
            assert!(
                value["hint"]
                    .as_str()
                    .is_some_and(|hint| hint.contains("agent.status"))
            );
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartet sofortiges Erfolgsergebnis, nicht {other:?}"
            )));
        }
    }
    // Das Kind läuft weiter und bleibt admittiert (der Eltern-Turn endet).
    assert!(fx.spawner.background_children().is_detached(&child));
    assert!(fx.spawner.child_record(&child).is_some());
    assert!(has_running(&fx.app));
    // Aufräumen: Abbruch beendet den hängenden Lauf.
    assert_eq!(cancel_all(Some(&fx.spawner), fx.session.id(), "test"), 1);
    wait_until_finished(&fx.spawner, fx.session.id()).await
}

#[tokio::test]
async fn workers_stay_synchronous_even_when_background_is_requested() -> TestResult {
    let mut fx = fixture(false)?;
    let (child, call_id) = fx
        .handoff("uia-worker", json!({ "task": "kurz", "background": true }))
        .await?;
    assert!(
        fx.launcher
            .try_launch(&fx.session, &child, &call_id, "uia-worker")
            .is_none()
    );
    assert!(!fx.spawner.background_children().is_detached(&child));
    Ok(())
}

#[tokio::test]
async fn background_false_forces_a_synchronous_orchestrator() -> TestResult {
    let mut fx = fixture(false)?;
    let (child, call_id) = fx
        .handoff(
            "root-orchestrator",
            json!({ "task": "X", "background": false }),
        )
        .await?;
    assert!(
        fx.launcher
            .try_launch(&fx.session, &child, &call_id, "root-orchestrator")
            .is_none()
    );
    assert!(!fx.spawner.background_children().is_detached(&child));
    Ok(())
}

#[test]
fn only_the_tui_root_and_orchestrator_targets_run_in_the_background() -> TestResult {
    let fx = fixture(false)?;
    assert!(
        fx.launcher
            .wants_background(true, "root-orchestrator", None)
    );
    assert!(
        fx.launcher
            .wants_background(true, "root-orchestrator", Some(true))
    );
    assert!(
        !fx.launcher
            .wants_background(true, "root-orchestrator", Some(false))
    );
    assert!(!fx.launcher.wants_background(true, "uia-worker", Some(true)));
    // Nicht die Wurzel (z. B. ein Orchestrator, der selbst Kinder startet):
    // synchron.
    assert!(
        !fx.launcher
            .wants_background(false, "coding-orchestrator", None)
    );
    Ok(())
}

#[tokio::test]
async fn the_limit_applies_while_a_background_orchestrator_runs() -> TestResult {
    let mut fx = fixture(true)?;
    let (child, call_id) = fx
        .handoff("root-orchestrator", json!({ "task": "A" }))
        .await?;
    fx.launcher
        .try_launch(&fx.session, &child, &call_id, "root-orchestrator")
        .ok_or(TestError::Missing("erster läuft im Hintergrund"))?;
    let second = fx
        .handoff("root-orchestrator", json!({ "task": "B" }))
        .await;
    assert!(second.is_err(), "zweiter Root-Orchestrator wird abgelehnt");
    // UIA-Worker bleiben erlaubt.
    fx.handoff("uia-worker", json!({ "task": "C" })).await?;
    cancel_all(Some(&fx.spawner), fx.session.id(), "test");
    wait_until_finished(&fx.spawner, fx.session.id()).await
}

#[tokio::test]
async fn background_rights_equal_the_synchronous_admission() -> TestResult {
    let mut fx = fixture(true)?;
    let (child, call_id) = fx
        .handoff("root-orchestrator", json!({ "task": "A" }))
        .await?;
    let before = fx
        .spawner
        .child_record(&child)
        .ok_or(TestError::Missing("admittiert"))?;
    fx.launcher
        .try_launch(&fx.session, &child, &call_id, "root-orchestrator")
        .ok_or(TestError::Missing("Hintergrund"))?;
    let after = fx
        .spawner
        .child_record(&child)
        .ok_or(TestError::Missing("weiter admittiert"))?;
    // Keine Rechte-Erweiterung: Rolle, Tiefe, Budget, Tiefendecke und
    // Pause-Erlaubnis sind die der Admission.
    assert_eq!(before.role, after.role);
    assert_eq!(before.depth, after.depth);
    assert_eq!(before.budget, after.budget);
    assert_eq!(before.depth_ceiling, after.depth_ceiling);
    assert_eq!(before.allow_pause, after.allow_pause);
    assert_eq!(before.parent, after.parent);
    cancel_all(Some(&fx.spawner), fx.session.id(), "test");
    wait_until_finished(&fx.spawner, fx.session.id()).await
}

// ── Ergebnis-Zustellung ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_finished_background_agent_starts_an_auto_turn_when_idle() -> TestResult {
    let mut fx = fixture(false)?;
    let (child, call_id) = fx
        .handoff("root-orchestrator", json!({ "task": "Baue X" }))
        .await?;
    fx.launcher
        .try_launch(&fx.session, &child, &call_id, "root-orchestrator")
        .ok_or(TestError::Missing("Hintergrund"))?;
    wait_until_finished(&fx.spawner, fx.session.id()).await?;
    // Nach dem Ende ist die Admission freigegeben.
    assert!(fx.spawner.child_record(&child).is_none());

    assert!(collect_finished(&mut fx.app));
    // Runde 6, Teil C: Hintergrund-Ende samt Benachrichtigungstext im Export.
    let markdown = crate::app::build_export(
        &fx.app,
        &crate::export::ExportOptions::default(),
        crate::app::ExportOutputFormat::Markdown,
    );
    assert!(
        markdown.contains("Ergebnis des Orchestrators"),
        "Hintergrund-Ende fehlt im Export: {markdown}"
    );
    assert!(fx.app.export_entries.iter().any(|entry| matches!(
        entry,
        crate::export::ExportEntry::Agent(agent)
            if agent.agent_id == child.as_str()
                && agent.summary.as_deref().is_some_and(|text| text.contains("fertig"))
    )));
    let auto = take_auto_turn(&mut fx.app).ok_or(TestError::Missing("Auto-Turn im Leerlauf"))?;
    assert_eq!(auto, AUTO_TURN_PROMPT);
    let turn_text = attach_queued_notices(&mut fx.app, auto);
    assert!(turn_text.starts_with("[Hintergrund-Agent root-orchestrator"));
    assert!(turn_text.contains("fertig"));
    assert!(turn_text.contains("Ergebnis des Orchestrators"));
    assert!(turn_text.ends_with(AUTO_TURN_PROMPT));
    // Einmal zugestellt: kein zweiter Auto-Turn.
    assert!(take_auto_turn(&mut fx.app).is_none());
    Ok(())
}

#[test]
fn a_notice_while_busy_joins_the_next_turn_as_context() -> TestResult {
    let mut fx = fixture(false)?;
    enqueue_background_notice(
        &mut fx.app,
        "[Hintergrund-Agent root-orchestrator x fertig nach 3 s]\nErgebnis",
        true,
    );
    // Die Nutzerin hat während des Turns schon weitergeschrieben: ihr Turn
    // ist der nächste und trägt die Meldung als Kontext.
    let turn_text = attach_queued_notices(&mut fx.app, "und jetzt?".to_owned());
    assert!(turn_text.starts_with("[Hintergrund-Agent root-orchestrator x fertig"));
    assert!(turn_text.ends_with("und jetzt?"));
    assert!(
        take_auto_turn(&mut fx.app).is_none(),
        "kein zusätzlicher Auto-Turn"
    );
    // Eine Meldung ohne Auto-Turn wartet still auf den nächsten Turn.
    enqueue_background_notice(&mut fx.app, "leise", false);
    assert!(take_auto_turn(&mut fx.app).is_none());
    assert!(attach_queued_notices(&mut fx.app, "t".to_owned()).starts_with("leise"));
    Ok(())
}

#[test]
fn classify_outcome_maps_success_failure_and_cancellation() {
    let (status, text) = classify_outcome(Ok(ToolCallResult::success(json!("fertig"))));
    assert_eq!(
        (status, text.as_str()),
        (BackgroundStatus::Completed, "fertig")
    );
    let (status, _) =
        classify_outcome(Ok(ToolCallResult::error("child agent was cancelled: User")));
    assert_eq!(status, BackgroundStatus::Cancelled);
    let (status, _) = classify_outcome(Ok(ToolCallResult::error("child agent failed: x")));
    assert_eq!(status, BackgroundStatus::Failed);
    let (status, text) = classify_outcome(Err(AgentSpawnError {
        message: "weg".to_owned(),
    }));
    assert_eq!((status, text.as_str()), (BackgroundStatus::Failed, "weg"));
}

// ── Steuerung ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn agent_cancel_command_cancels_the_own_background_run() -> TestResult {
    let mut fx = fixture(true)?;
    let (child, call_id) = fx
        .handoff("root-orchestrator", json!({ "task": "A" }))
        .await?;
    fx.launcher
        .try_launch(&fx.session, &child, &call_id, "root-orchestrator")
        .ok_or(TestError::Missing("Hintergrund"))?;
    // Die Task anlaufen lassen (das Modell hängt dann).
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    apply_agents_command(&mut fx.app, &format!("cancel {child}"));
    wait_until_finished(&fx.spawner, fx.session.id()).await?;
    let notices = fx
        .spawner
        .background_children()
        .take_notices(fx.session.id());
    assert_eq!(notices.len(), 1);
    assert!(
        matches!(
            notices[0].status,
            BackgroundStatus::Cancelled | BackgroundStatus::Failed
        ),
        "abgebrochen, nicht fertig: {:?}",
        notices[0].status
    );
    Ok(())
}

#[tokio::test]
async fn new_session_cancels_running_background_agents() -> TestResult {
    let mut fx = fixture(true)?;
    let (child, call_id) = fx
        .handoff("root-orchestrator", json!({ "task": "A" }))
        .await?;
    fx.launcher
        .try_launch(&fx.session, &child, &call_id, "root-orchestrator")
        .ok_or(TestError::Missing("Hintergrund"))?;
    // `/new` und `/resume` rufen genau das für die alte Sitzung auf.
    assert_eq!(
        cancel_all(Some(&fx.spawner), fx.session.id(), "session switch"),
        1
    );
    wait_until_finished(&fx.spawner, fx.session.id()).await?;
    assert!(fx.spawner.child_record(&child).is_none());
    assert!(!has_running(&fx.app));
    Ok(())
}

#[tokio::test]
async fn quit_asks_once_while_background_agents_run() -> TestResult {
    let mut fx = fixture(true)?;
    assert!(confirm_quit(&mut fx.app), "ohne Hintergrund sofort");
    let (child, call_id) = fx
        .handoff("root-orchestrator", json!({ "task": "A" }))
        .await?;
    fx.launcher
        .try_launch(&fx.session, &child, &call_id, "root-orchestrator")
        .ok_or(TestError::Missing("Hintergrund"))?;
    assert!(!confirm_quit(&mut fx.app), "erstes /quit fragt nach");
    assert!(confirm_quit(&mut fx.app), "zweites /quit beendet");
    cancel_all(Some(&fx.spawner), fx.session.id(), "test");
    wait_until_finished(&fx.spawner, fx.session.id()).await
}

#[test]
fn the_status_line_names_background_agents() {
    assert_eq!(status_suffix_for(0, 0), "");
    assert_eq!(status_suffix_for(1, 0), "");
    assert_eq!(status_suffix_for(2, 0), " | 2 Agenten aktiv");
    assert_eq!(status_suffix_for(1, 1), " | 1 im Hintergrund");
    assert_eq!(
        status_suffix_for(3, 1),
        " | 3 Agenten aktiv · 1 im Hintergrund"
    );
}

// ── Freigaben im Leerlauf ─────────────────────────────────────────────────────

#[tokio::test]
async fn an_idle_sudo_prompt_is_shown_and_answered() -> TestResult {
    let fx = fixture(false)?;
    let (sender, receiver) = harw_tool_shell::sudo_prompt_channel();
    let mut app = fx
        .app
        .with_sudo_prompts(Some(receiver), Duration::from_secs(600));
    let (prompt, answer) = harw_tool_shell::SudoPrompt::new(
        "child".to_owned(),
        "uia-shell-worker".to_owned(),
        vec!["apt-get".to_owned(), "install".to_owned(), "rg".to_owned()],
        PathBuf::from("/workspace"),
        "Werkzeug fehlt".to_owned(),
        false,
    );
    sender
        .send(prompt)
        .map_err(|_| TestError::Missing("sudo-Kanal offen"))?;
    let (_host_sender, mut host_prompts) = harw_tool_shell::host_permit_prompt_channel();

    // Kein Turn läuft: der Leerlauf holt die Frage ab und zeigt das Fenster.
    assert!(poll_idle_prompts(&mut app, &mut host_prompts));
    assert!(app.sudo.is_open());
    // Zeichnen geht am Fenster vorbei an die normale Schleife …
    assert!(route_idle_prompt_event(&mut app, TuiEvent::Draw).is_err());
    // … jede Taste gehört dem Fenster; Esc beantwortet mit Ablehnung.
    let routed = route_idle_prompt_event(
        &mut app,
        TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
    );
    assert!(matches!(routed, Ok(true)));
    assert!(!app.sudo.is_open());
    assert!(
        answer.into_answer().await.is_none(),
        "fail-closed: Ablehnung"
    );
    Ok(())
}

#[tokio::test]
async fn an_idle_host_permit_prompt_is_shown_and_approved() -> TestResult {
    let fx = fixture(false)?;
    let mut app = fx.app;
    let (host_sender, mut host_prompts) = harw_tool_shell::host_permit_prompt_channel();
    let (prompt, answer) = crate::host_permit_dialog::HostPermitPrompt::new(
        "child".to_owned(),
        "uia-shell-worker".to_owned(),
        "cargo build".to_owned(),
        PathBuf::from("/workspace"),
        crate::host_permit_dialog::HostPermitVariant::SingleExecution,
    );
    host_sender
        .send(prompt)
        .map_err(|_| TestError::Missing("Host-Permit-Kanal offen"))?;
    assert!(poll_idle_prompts(&mut app, &mut host_prompts));
    assert!(app.pending_host_permit.is_some());
    // Vor der Scharfschaltung zählt keine Taste.
    let enter = TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        route_idle_prompt_event(&mut app, enter.clone()),
        Ok(false)
    ));
    assert!(app.pending_host_permit.is_some());
    // Scharf: Enter wählt die vorausgewählte Einmal-Freigabe.
    app.background.host_permit_shown_at = Instant::now().checked_sub(Duration::from_secs(5));
    assert!(matches!(route_idle_prompt_event(&mut app, enter), Ok(true)));
    assert!(app.pending_host_permit.is_none());
    let decision = answer.await.map_err(ctx("Antwort kommt an"))?;
    assert_eq!(
        decision,
        Some(crate::host_permit_dialog::HostPermitVariant::SingleExecution)
    );
    Ok(())
}

// ── Runde 5, Teil M: Endbericht statt nackter Fehler ─────────────────────────

/// Ein abgebrochener Hintergrund-Orchestrator liefert den Endbericht mit
/// Journal (`[child_end status=cancelled …]`, Verweis auf
/// `agent.result {part: "journal"}`) — die UIA muss nicht raten.
#[tokio::test]
async fn a_cancelled_background_agent_delivers_its_journal() -> TestResult {
    let mut fx = fixture(true)?;
    let (child, call_id) = fx
        .handoff("root-orchestrator", json!({ "task": "Baue X" }))
        .await?;
    fx.launcher
        .try_launch(&fx.session, &child, &call_id, "root-orchestrator")
        .ok_or(TestError::Missing("Hintergrund"))?;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    apply_agents_command(&mut fx.app, &format!("cancel {child}"));
    wait_until_finished(&fx.spawner, fx.session.id()).await?;
    let notices = fx
        .spawner
        .background_children()
        .take_notices(fx.session.id());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].status, BackgroundStatus::Cancelled);
    assert!(
        notices[0].text.contains("[child_end status=cancelled"),
        "{}",
        notices[0].text
    );
    assert!(notices[0].text.contains("\"part\": \"journal\""));
    // Das Journal bleibt nach der Freigabe für den Elternteil lesbar.
    let journal = fx
        .spawner
        .child_journal_for(fx.session.id(), child.as_str())
        .ok_or(TestError::Missing("Journal nach dem Abbruch"))?;
    assert!(!journal.is_running());
    assert!(journal.end().is_some());
    Ok(())
}

#[test]
fn classify_outcome_reads_the_child_end_status() {
    let (status, _) = classify_outcome(Ok(ToolCallResult::error(
        "[child_end status=timeout handoff=yes] Zeitbudget 15 min erreicht\n…",
    )));
    assert_eq!(status, BackgroundStatus::Failed);
    let (status, _) = classify_outcome(Ok(ToolCallResult::error(
        "[child_end status=cancelled handoff=no] abgebrochen (Nutzerin)\n…",
    )));
    assert_eq!(status, BackgroundStatus::Cancelled);
}
