//! `agent.message` und `parent.message` — Kommunikation zwischen Elternteil
//! und Kind; Journal-Felder für `agent.status` (Runde 5, Teil M).
//!
//! # Verantwortungsbereich
//! ```text
//! agent.message  { child_id, text }              // Eltern → eigenes Kind
//! parent.message { text, kind?: info|question }  // Kind → direkter Elternteil
//! ```
//!
//! - `agent.message` legt den Text in das Postfach des Kindes; das Kind
//!   liest ihn an seiner nächsten Runden-Grenze als
//!   „[Nachricht von <rolle>] …". Wartet das Kind auf eine Antwort
//!   (`parent.message {kind: "question"}`), beantwortet der Text sie sofort.
//!   So gibt die Nutzerin über die UIA Kurskorrekturen.
//! - `parent.message {kind: "info"}` meldet einen Zwischenstand
//!   (höchstens einmal je 30 s); `{kind: "question"}` wartet bis zu 10 min
//!   auf die Antwort und liefert sonst „keine Antwort – arbeite mit einer
//!   begründeten Annahme weiter".
//! - [`journal_status_fields`]/[`journal_status_line`] hängen das
//!   Aktivitätsjournal an `agent.status` (Schritte, letzte Einträge,
//!   aktueller Schritt, geänderte Dateien).
//!
//! # Sicherheit
//! Beide Werkzeuge transportieren nur Text zwischen direkt verbundenen
//! Sitzungen und verleihen keine Rechte. Die aufrufende Sitzung kommt aus
//! dem Ausführungskontext (`OpContext::session_id`), nie aus
//! Modell-Argumenten; die Eltern-Kind-Bindung prüft der Spawner. Fremde,
//! unbekannte und nicht fortsetzbare Kinder bekommen dieselbe Ablehnung.
//! Runde 9, E3: ein eigenes, beendetes Kind erreicht diese Operation gar
//! nicht — der Turn-Loop setzt es über `continue_from` mit der Nachricht als
//! Auftrag fort (`harw_core::turn_loop`, `message_resume_handoff`). Nachrichten
//! sind auf 4 KiB gedeckelt, Postfächer begrenzt. Keine Freigabepflicht
//! (`AUTO_APPROVED_TOOLS`), nie in `ALWAYS_ASK_TOOLS`.
//!
//! # Nebenläufigkeit
//! Zustandslos; `parent.message {kind: "question"}` wartet asynchron, ohne
//! einen Lock zu halten.

use std::sync::OnceLock;

use harw_core::child_comms::{
    AGENT_MESSAGE_TOOL, ChildJournal, PARENT_MESSAGE_TOOL, PARENT_QUESTION_TIMEOUT,
    ParentMessageKind,
};
use harw_operations::context::OpContext;
use harw_operations::error::OpError;
use harw_operations::op_schema::{object_schema, string_schema};
use harw_operations::operation::{
    ApprovalPolicy, ArgsSchemaFn, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
    OperationDomain, OperationMeta, PermissionTier, Surface,
};
use serde_json::{Map, Value, json};

use crate::context_ext::OpContextCoreExt;

/// Beide Werkzeugnamen (Montage und Rechte-Tabellen).
pub const AGENT_MESSAGING_TOOLS: &[&str] = &[AGENT_MESSAGE_TOOL, PARENT_MESSAGE_TOOL];

/// Wie viele jüngste Journal-Einträge `agent.status` zeigt.
pub const STATUS_RECENT_ENTRIES: usize = 8;

/// Liest ein Pflicht-Textfeld.
fn required_str<'a>(
    tool: &str,
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, OpError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OpError::InvalidArguments(format!("{tool}: `{field}` is required")))
}

/// Lehnt unbekannte Felder ab (geschlossenes Schema).
fn closed_object<'a>(
    tool: &str,
    args: &'a Value,
    allowed: &[&str],
) -> Result<&'a Map<String, Value>, OpError> {
    let object = args
        .as_object()
        .ok_or_else(|| OpError::InvalidArguments(format!("{tool}: expects an object")))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(OpError::InvalidArguments(format!(
            "{tool}: does not accept the field `{key}`; allowed are {allowed:?}"
        )));
    }
    Ok(object)
}

/// Die geparsten Argumente von `agent.message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMessageRequest {
    /// Das eigene Kind.
    pub child_id: String,
    /// Der Text (Deckel prüft der Spawner).
    pub text: String,
}

impl AgentMessageRequest {
    /// Parst die Modell-Argumente.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] bei fehlenden Feldern oder unbekannten
    /// Feldern.
    pub fn parse(args: &Value) -> Result<Self, OpError> {
        let object = closed_object(AGENT_MESSAGE_TOOL, args, &["child_id", "text"])?;
        Ok(Self {
            child_id: required_str(AGENT_MESSAGE_TOOL, object, "child_id")?.to_owned(),
            text: required_str(AGENT_MESSAGE_TOOL, object, "text")?.to_owned(),
        })
    }
}

/// Die geparsten Argumente von `parent.message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentMessageRequest {
    /// Der Text.
    pub text: String,
    /// `info` (Vorgabe) oder `question`.
    pub kind: ParentMessageKind,
}

impl ParentMessageRequest {
    /// Parst die Modell-Argumente.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] bei fehlendem Text, unbekanntem `kind`
    /// oder unbekannten Feldern.
    pub fn parse(args: &Value) -> Result<Self, OpError> {
        let object = closed_object(PARENT_MESSAGE_TOOL, args, &["text", "kind"])?;
        let kind = match object.get("kind") {
            None | Some(Value::Null) => ParentMessageKind::Info,
            Some(Value::String(kind)) => {
                ParentMessageKind::parse(Some(kind)).map_err(|message| {
                    OpError::InvalidArguments(format!("{PARENT_MESSAGE_TOOL}: {message}"))
                })?
            }
            Some(_) => {
                return Err(OpError::InvalidArguments(format!(
                    "{PARENT_MESSAGE_TOOL}: `kind` must be \"info\" or \"question\""
                )));
            }
        };
        Ok(Self {
            text: required_str(PARENT_MESSAGE_TOOL, object, "text")?.to_owned(),
            kind,
        })
    }
}

/// Führt `agent.message` aus.
///
/// # Errors
/// [`OpError::NotAvailable`] ohne Spawner, für fremde/unbekannte/beendete
/// Kinder (immer dieselbe Meldung), bei zu langem Text oder vollem Postfach.
pub fn agent_message(ctx: &OpContext, request: &AgentMessageRequest) -> Result<OpOutput, OpError> {
    let spawner = ctx.managed_spawner().ok_or_else(|| {
        OpError::NotAvailable("kein Agent-Spawner in diesem Kontext konfiguriert".to_owned())
    })?;
    // Runde 9, E3: die offene Frage vor der Zustellung merken, damit der
    // Elternteil sieht, worauf sein Text als Antwort ging.
    let question = harw_types::SessionId::try_from_str(request.child_id.clone())
        .ok()
        .and_then(|child| spawner.child_comms().pending_question_excerpt(&child));
    let delivery = spawner
        .send_message_to_child(ctx.session_id(), &request.child_id, &request.text)
        .map_err(|error| {
            OpError::NotAvailable(format!("{AGENT_MESSAGE_TOOL}: {}", error.message))
        })?;
    let text = match delivery {
        harw_core::MessageDelivery::AnsweredQuestion => format!(
            "Als Antwort auf die offene Frage „{}“ an {} zugestellt; das Kind arbeitet damit \
             sofort weiter.",
            question.as_deref().unwrap_or("…"),
            request.child_id
        ),
        harw_core::MessageDelivery::Queued => format!(
            "Nachricht an {} eingereiht; das Kind liest sie an seiner nächsten Runden-Grenze.",
            request.child_id
        ),
    };
    Ok(OpOutput {
        text,
        data: Some(json!({ "child_id": request.child_id, "delivery": delivery.as_str() })),
    })
}

/// Führt `parent.message` aus.
///
/// # Errors
/// [`OpError::NotAvailable`] ohne Spawner, wenn die Sitzung keinen
/// Elternteil hat, bei zu langem Text, Rate-Limit oder schon offener Frage.
pub async fn parent_message(
    ctx: &OpContext,
    request: &ParentMessageRequest,
) -> Result<OpOutput, OpError> {
    let spawner = ctx.managed_spawner().ok_or_else(|| {
        OpError::NotAvailable("kein Agent-Spawner in diesem Kontext konfiguriert".to_owned())
    })?;
    let unavailable = |error: harw_extension_api::AgentSpawnError| {
        OpError::NotAvailable(format!("{PARENT_MESSAGE_TOOL}: {}", error.message))
    };
    match request.kind {
        ParentMessageKind::Info => {
            spawner
                .post_info_to_parent(ctx.session_id(), &request.text)
                .map_err(unavailable)?;
            Ok(OpOutput {
                text: "Nachricht an den Elternteil gesendet.".to_owned(),
                data: Some(json!({ "kind": "info", "delivered": true })),
            })
        }
        ParentMessageKind::Question => {
            let (answer, answered) = spawner
                .ask_parent(ctx.session_id(), &request.text, PARENT_QUESTION_TIMEOUT)
                .await
                .map_err(unavailable)?;
            let text = if answered {
                format!("Antwort des Elternteils: {answer}")
            } else {
                answer
            };
            Ok(OpOutput {
                text,
                data: Some(json!({ "kind": "question", "answered": answered })),
            })
        }
    }
}

// ── Journal-Felder für `agent.status` ─────────────────────────────────────────

/// Hängt die Journal-Felder an einen `agent.status`-Eintrag:
/// `steps`, `recent` (die letzten [`STATUS_RECENT_ENTRIES`] Einträge),
/// `current_step`, `changed_files`, `last_text` und — nach einem nicht
/// regulären Ende — `end` (`status`, `reason`, `handoff_available`).
pub fn journal_status_fields(entry: &mut Map<String, Value>, journal: &ChildJournal) {
    entry.insert("steps".to_owned(), json!(journal.steps()));
    entry.insert(
        "recent".to_owned(),
        Value::Array(
            journal
                .last(STATUS_RECENT_ENTRIES)
                .iter()
                .map(harw_core::child_comms::JournalEntry::to_json)
                .collect(),
        ),
    );
    entry.insert("current_step".to_owned(), json!(journal.current_step()));
    entry.insert("changed_files".to_owned(), json!(journal.files()));
    entry.insert(
        "last_text".to_owned(),
        json!(
            journal
                .last_assistant()
                .map(|text| text.chars().take(600).collect::<String>())
        ),
    );
    if let Some(end) = journal.end() {
        entry.insert(
            "end".to_owned(),
            json!({
                "status": end.status.as_str(),
                "reason": end.reason,
                "handoff_available": end.handoff.is_some(),
            }),
        );
    }
}

/// Die Journal-Zeilen eines `agent.status`-Eintrags (eingerückt).
#[must_use]
pub fn journal_status_line(journal: &ChildJournal) -> String {
    let mut text = format!("  Journal: {} Schritte", journal.steps());
    let files = journal.files();
    if !files.is_empty() {
        text.push_str(&format!(" · geänderte Dateien: {}", files.join(", ")));
    }
    if let Some(end) = journal.end() {
        text.push_str(&format!(
            "\n  Ende: {} — {}{}",
            end.status.as_str(),
            end.reason,
            if end.handoff.is_some() {
                " · Übergabe verfügbar"
            } else {
                ""
            }
        ));
    }
    for entry in journal.last(STATUS_RECENT_ENTRIES) {
        text.push_str(&format!("\n    {}", entry.line()));
    }
    text.push_str(&format!(
        "\n  Vollständig: agent.result {{\"child_id\": \"{}\", \"part\": \"journal\"}}",
        journal.child
    ));
    text
}

/// Ein Kind ohne Hintergrund-Lauf (synchron gestartet) als
/// `agent.status`-Eintrag aus seinem Journal.
#[must_use]
pub fn journal_only_entry(journal: &ChildJournal) -> Map<String, Value> {
    let mut entry = Map::new();
    entry.insert("child_id".to_owned(), json!(journal.child.as_str()));
    entry.insert("role".to_owned(), json!(journal.role));
    let status = match (journal.end(), journal.is_running()) {
        (Some(end), _) => end.status.as_str(),
        (None, true) => "running",
        (None, false) => "completed",
    };
    entry.insert("status".to_owned(), json!(status));
    entry.insert("task".to_owned(), json!(journal.task));
    journal_status_fields(&mut entry, journal);
    entry
}

/// Die Textzeile eines Kindes ohne Hintergrund-Lauf.
#[must_use]
pub fn journal_only_line(journal: &ChildJournal) -> String {
    let status = match (journal.end(), journal.is_running()) {
        (Some(end), _) => end.status.label_de(),
        (None, true) => "läuft",
        (None, false) => "beendet",
    };
    format!(
        "- {} ({}) · {status} (synchron)\n{}",
        journal.role,
        journal.child,
        journal_status_line(journal)
    )
}

// ── Operationen ───────────────────────────────────────────────────────────────

/// Die Operation `agent.message` (Eltern → eigenes, laufendes Kind).
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentMessageOperation;

/// Die Operation `parent.message` (Kind → direkter Elternteil).
#[derive(Debug, Default, Clone, Copy)]
pub struct ParentMessageOperation;

const AGENT_MESSAGE_ARGS_SCHEMA: ArgsSchemaFn = || {
    object_schema(
        vec![
            (
                "child_id",
                string_schema("ID eines eigenen, laufenden Kind-Agenten."),
            ),
            (
                "text",
                string_schema(
                    "Nachricht (höchstens 4 KiB): Kurskorrektur, Zusatzinformation oder die \
                     Antwort auf eine Frage des Kindes.",
                ),
            ),
        ],
        &["child_id", "text"],
    )
};

const PARENT_MESSAGE_ARGS_SCHEMA: ArgsSchemaFn = || {
    object_schema(
        vec![
            (
                "text",
                string_schema("Nachricht an den direkten Elternteil (höchstens 4 KiB)."),
            ),
            (
                "kind",
                string_schema(
                    "\"info\" (Vorgabe; Zwischenstand, höchstens einmal je 30 s) oder \
                     \"question\" (wartet bis zu 10 min auf eine Antwort).",
                ),
            ),
        ],
        &["text"],
    )
};

impl Operation for AgentMessageOperation {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: AGENT_MESSAGE_TOOL,
            summary: "Schickt einem eigenen, laufenden Kind-Agenten eine Nachricht \
                      (Kurskorrektur oder Antwort auf seine Frage); es liest sie an seiner \
                      nächsten Runden-Grenze.",
            domain: OperationDomain::Agents,
            permission: PermissionTier::Observer,
            surfaces: vec![Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::None,
            }],
            category: OperationCategory::Agent,
            args_schema: Some(AGENT_MESSAGE_ARGS_SCHEMA),
            ..OperationMeta::default()
        })
    }

    fn run<'a>(&'a self, ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
        Box::pin(async move {
            if !input.invocation.is_model_tool() {
                return Err(OpError::InvalidArguments(format!(
                    "{AGENT_MESSAGE_TOOL}: is only available as a model tool"
                )));
            }
            let request = AgentMessageRequest::parse(input.invocation.json_args())?;
            agent_message(ctx, &request)
        })
    }
}

impl Operation for ParentMessageOperation {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: PARENT_MESSAGE_TOOL,
            summary: "Meldet dem direkten Elternteil einen Zwischenstand (info) oder stellt \
                      eine Frage (question) und wartet mit Zeitlimit auf die Antwort.",
            domain: OperationDomain::Agents,
            permission: PermissionTier::Observer,
            surfaces: vec![Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::None,
            }],
            category: OperationCategory::Agent,
            args_schema: Some(PARENT_MESSAGE_ARGS_SCHEMA),
            ..OperationMeta::default()
        })
    }

    fn run<'a>(&'a self, ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
        Box::pin(async move {
            if !input.invocation.is_model_tool() {
                return Err(OpError::InvalidArguments(format!(
                    "{PARENT_MESSAGE_TOOL}: is only available as a model tool"
                )));
            }
            let request = ParentMessageRequest::parse(input.invocation.json_args())?;
            parent_message(ctx, &request).await
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

    use super::{
        AgentMessageOperation, AgentMessageRequest, ParentMessageOperation, ParentMessageRequest,
    };
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
            "harw-agent-messaging-test-{}-{}",
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
    fn test_requests_are_closed_and_kind_defaults_to_info() -> TestResult {
        let request = AgentMessageRequest::parse(&json!({ "child_id": "c", "text": "stopp" }))
            .map_err(ctx("gültige Nachricht"))?;
        assert_eq!(request.text, "stopp");
        assert!(AgentMessageRequest::parse(&json!({ "child_id": "c" })).is_err());
        assert!(
            AgentMessageRequest::parse(&json!({ "child_id": "c", "text": "x", "role": "y" }))
                .is_err()
        );
        let info = ParentMessageRequest::parse(&json!({ "text": "Stand" }))
            .map_err(ctx("info ist Vorgabe"))?;
        assert_eq!(info.kind, harw_core::ParentMessageKind::Info);
        let question = ParentMessageRequest::parse(&json!({ "text": "A?", "kind": "question" }))
            .map_err(ctx("Frage"))?;
        assert_eq!(question.kind, harw_core::ParentMessageKind::Question);
        assert!(ParentMessageRequest::parse(&json!({ "text": "x", "kind": "order" })).is_err());
        Ok(())
    }

    #[test]
    fn test_both_tools_need_no_approval() {
        for meta in [AgentMessageOperation.meta(), ParentMessageOperation.meta()] {
            assert!(meta.surfaces.iter().any(|surface| matches!(
                surface,
                Surface::ModelTool {
                    approval: ApprovalPolicy::None,
                    ..
                }
            )));
        }
    }

    /// Eine Nachricht an ein fremdes oder unbekanntes Kind wird abgewiesen
    /// — mit derselben Meldung.
    #[tokio::test]
    async fn test_message_to_foreign_or_unknown_child_is_rejected() -> TestResult {
        let root = SessionId::new();
        let (runtime, _spawner, tmp, _events) = runtime_ctx(root)?;
        let unknown = AgentMessageOperation
            .run(
                &runtime,
                OpInput::model_tool(json!({ "child_id": SessionId::new().as_str(), "text": "x" })),
            )
            .await;
        let message = match unknown {
            Err(OpError::NotAvailable(message)) => message,
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        };
        assert!(
            message.contains("kein eigenes, laufendes Kind"),
            "{message}"
        );
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }

    /// Eine Wurzel ohne Elternteil kann `parent.message` nicht nutzen —
    /// das Werkzeug erweitert nichts.
    #[tokio::test]
    async fn test_parent_message_without_parent_is_rejected() -> TestResult {
        let root = SessionId::new();
        let (runtime, _spawner, tmp, _events) = runtime_ctx(root)?;
        let result = ParentMessageOperation
            .run(&runtime, OpInput::model_tool(json!({ "text": "hallo" })))
            .await;
        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "{result:?}"
        );
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }

    /// `agent.status` zeigt live die letzten Journal-Schritte eines
    /// laufenden Hintergrund-Kindes und seine geänderten Dateien.
    #[tokio::test]
    async fn test_status_shows_live_journal_steps() -> TestResult {
        let root = SessionId::new();
        let (runtime, spawner, tmp, _events) = runtime_ctx(root.clone())?;
        let child = SessionId::new();
        spawner
            .background_children()
            .register(&child, &root, "root-orchestrator", Some("Baue X"));
        let comms = spawner.child_comms();
        comms.open_journal(&child, &root, "root-orchestrator", Some("Baue X"));
        for index in 0..12 {
            comms.record_tool_outcome(
                &child,
                "fs.write",
                &json!({ "path": format!("src/f{index}.rs") }),
                true,
                "ok",
            );
        }
        let output = crate::AgentStatusOperation
            .run(
                &runtime,
                OpInput::model_tool(json!({ "child_id": child.as_str() })),
            )
            .await
            .map_err(ctx("agent.status läuft"))?;
        let agent = output
            .data
            .as_ref()
            .and_then(|data| data.get("agents"))
            .and_then(|agents| agents.get(0))
            .ok_or(TestError::Missing("agents[0]"))?;
        assert_eq!(agent["steps"], json!(12));
        assert_eq!(
            agent["recent"].as_array().map(Vec::len),
            Some(super::STATUS_RECENT_ENTRIES)
        );
        assert!(
            agent["changed_files"]
                .as_array()
                .is_some_and(|files| files.len() == 12)
        );
        assert!(
            output.text.contains("fs.write(src/f11.rs)"),
            "{}",
            output.text
        );

        // Ein synchron laufendes eigenes Kind ohne Hintergrund-Lauf ist
        // über sein Journal ebenfalls abfragbar.
        let sync_child = SessionId::new();
        comms.open_journal(&sync_child, &root, "uia-worker", None);
        comms.record_tool_outcome(&sync_child, "web.search", &json!({}), true, "3 Treffer");
        let output = crate::AgentStatusOperation
            .run(
                &runtime,
                OpInput::model_tool(json!({ "child_id": sync_child.as_str() })),
            )
            .await
            .map_err(ctx("agent.status für ein synchrones Kind"))?;
        assert!(output.text.contains("web.search"), "{}", output.text);
        // Fremde Journale bleiben unsichtbar.
        let foreign = SessionId::new();
        comms.open_journal(&foreign, &SessionId::new(), "explorer", None);
        let result = crate::AgentStatusOperation
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
}
