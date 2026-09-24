//! `agent.result` — liefert den ungekürzten Antworttext eines eigenen,
//! abgeschlossenen Kind-Laufs (Runde 5, Teil H).
//!
//! # Verantwortungsbereich
//! Die Freitext-Rückgabe eines Kindes an seinen Elternteil wird auf
//! `CHILD_RETURN_MAX_BYTES` gekürzt (Kopf und Schluss je zur Hälfte, siehe
//! `harw_core::child_controller::cap_child_return_text_for_child`). Die
//! Kürzungsmarke nennt dieses Werkzeug mit der `child_id`; damit lädt der
//! Elternteil den vollständigen Text nach — auf Wunsch seitenweise:
//!
//! ```text
//! agent.result { child_id, offset?: n, max_bytes?: n, part?: "result"|"journal" }
//! ```
//!
//! Runde 5, Teil M: `part: "journal"` liefert statt des Antworttexts das
//! vollständige Aktivitätsjournal eines eigenen (laufenden oder beendeten)
//! Kindes — auch nach einem Abbruch, bei dem es keinen Antworttext gibt.
//!
//! # Sicherheit
//! Rein lesend. Die aufrufende Sitzung kommt aus dem Ausführungskontext
//! (`OpContext::session_id`), nie aus Modell-Argumenten; der Spawner gibt den
//! Text nur heraus, wenn diese Sitzung der Elternteil des Kindes ist
//! (`ManagedAgentSpawner::child_result_text`). Eine fremde oder unbekannte
//! Kind-ID bekommt immer dieselbe Meldung — kein Orakel über fremde Kinder.
//!
//! # Nebenläufigkeit
//! Zustandslos; die Operation liest nur kurz das Ergebnisarchiv des Spawners.

use std::sync::OnceLock;

use harw_core::child_controller::AGENT_RESULT_TOOL;
use harw_operations::context::OpContext;
use harw_operations::error::OpError;
use harw_operations::op_schema::{integer_schema, object_schema, string_schema};
use harw_operations::operation::{
    ApprovalPolicy, ArgsSchemaFn, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
    OperationDomain, OperationMeta, PermissionTier, Surface,
};
use harw_types::SessionId;
use serde_json::{Map, Value, json};

use crate::context_ext::OpContextCoreExt;

/// Standard- und Höchstgröße einer Seite in Bytes. Bleibt unter der
/// Werkzeugergebnis-Grenze der Wurzel (64 KiB) und der Kind-Turns, damit der
/// Text nicht ein zweites Mal gekürzt wird.
pub const AGENT_RESULT_MAX_PAGE_BYTES: usize = 48 * 1024;

/// Die geparsten Argumente von `agent.result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResultRequest {
    /// Die Kind-ID aus der Kürzungsmarke.
    pub child_id: String,
    /// Byte-Versatz, ab dem geliefert wird (Standard 0).
    pub offset: usize,
    /// Seitengröße in Bytes (Standard und Obergrenze
    /// [`AGENT_RESULT_MAX_PAGE_BYTES`]).
    pub max_bytes: usize,
    /// Runde 5, Teil M: welcher Teil geliefert wird (Vorgabe: Antworttext).
    pub part: AgentResultPart,
}

/// Runde 5, Teil M: welcher Teil eines Kind-Laufs geliefert wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentResultPart {
    /// Der ungekürzte Antworttext (Vorgabe).
    #[default]
    Result,
    /// Das vollständige Aktivitätsjournal.
    Journal,
}

impl AgentResultRequest {
    /// Parst die Modell-Argumente (geschlossenes Schema).
    ///
    /// # Beschreibung
    /// `offset` und `max_bytes` sind optional; `null` gilt als „nicht
    /// gesetzt“. `max_bytes` wird auf [`AGENT_RESULT_MAX_PAGE_BYTES`]
    /// gedeckelt, `0` gilt ebenfalls als „nicht gesetzt“.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] bei fehlender/leerer `child_id`,
    /// negativen oder nicht ganzzahligen Zahlen und unbekannten Feldern.
    pub fn parse(args: &Value) -> Result<Self, OpError> {
        let object = args
            .as_object()
            .ok_or_else(|| invalid("expects an object {child_id, offset?, max_bytes?}"))?;
        reject_unknown_fields(object)?;
        let child_id = object
            .get("child_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| invalid("`child_id` is required (from the truncation marker)"))?
            .to_owned();
        let offset = optional_usize(object, "offset")?.unwrap_or(0);
        let max_bytes = optional_usize(object, "max_bytes")?
            .filter(|bytes| *bytes > 0)
            .map_or(AGENT_RESULT_MAX_PAGE_BYTES, |bytes| {
                bytes.min(AGENT_RESULT_MAX_PAGE_BYTES)
            });
        let part = match object.get("part") {
            None | Some(Value::Null) => AgentResultPart::Result,
            Some(Value::String(part)) if matches!(part.as_str(), "result" | "text") => {
                AgentResultPart::Result
            }
            Some(Value::String(part)) if part == "journal" => AgentResultPart::Journal,
            Some(_) => return Err(invalid("`part` must be \"result\", \"journal\" or null")),
        };
        Ok(Self {
            child_id,
            offset,
            max_bytes,
            part,
        })
    }
}

/// Liest ein optionales, nicht negatives Ganzzahl-Feld; fehlend oder `null`
/// ist `None`.
fn optional_usize(object: &Map<String, Value>, field: &str) -> Result<Option<usize>, OpError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|number| usize::try_from(number).ok())
            .map(Some)
            .ok_or_else(|| invalid(&format!("`{field}` must be a non-negative integer or null"))),
    }
}

/// Lehnt jedes Feld außer `child_id`/`offset`/`max_bytes` ab.
fn reject_unknown_fields(object: &Map<String, Value>) -> Result<(), OpError> {
    const ALLOWED: [&str; 4] = ["child_id", "offset", "max_bytes", "part"];
    match object.keys().find(|key| !ALLOWED.contains(&key.as_str())) {
        Some(key) => Err(invalid(&format!(
            "does not accept the field `{key}`; allowed are {ALLOWED:?}"
        ))),
        None => Ok(()),
    }
}

/// Baut einen Argumentfehler.
fn invalid(message: &str) -> OpError {
    OpError::InvalidArguments(format!("{AGENT_RESULT_TOOL}: {message}"))
}

/// Eine ausgeschnittene Seite des Textes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResultPage {
    /// Der Seitentext.
    pub text: String,
    /// Tatsächlicher Start (auf eine UTF-8-Zeichengrenze abgerundet).
    pub start: usize,
    /// Ende (exklusiv).
    pub end: usize,
    /// Gesamtlänge des ungekürzten Textes in Bytes.
    pub total: usize,
}

impl AgentResultPage {
    /// `Some(end)`, solange nach dieser Seite noch Text folgt.
    #[must_use]
    pub fn next_offset(&self) -> Option<usize> {
        (self.end < self.total).then_some(self.end)
    }
}

/// Schneidet die Seite `[offset, offset + max_bytes)` aus `text`, ohne ein
/// UTF-8-Zeichen zu zerschneiden.
///
/// # Beschreibung
/// Start und Ende werden auf Zeichengrenzen abgerundet; liegt das Ende dann
/// auf dem Start (ein Zeichen größer als `max_bytes`), wird das eine Zeichen
/// trotzdem geliefert, damit das Blättern immer vorankommt. Ein `offset`
/// hinter dem Textende liefert eine leere Seite.
#[must_use]
pub fn page_of(text: &str, offset: usize, max_bytes: usize) -> AgentResultPage {
    let total = text.len();
    let start = floor_boundary(text, offset.min(total));
    let mut end = floor_boundary(text, start.saturating_add(max_bytes).min(total));
    if end <= start && start < total {
        end = ceil_boundary(text, start + 1);
    }
    AgentResultPage {
        text: text[start..end].to_owned(),
        start,
        end,
        total,
    }
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index.min(text.len())
}

/// Führt `agent.result` gegen den Spawner des Kontexts aus.
///
/// # Errors
/// [`OpError::NotAvailable`] ohne Spawner oder wenn das Kind kein eigenes,
/// abgeschlossenes Kind des Aufrufers ist (bzw. sein Ergebnis nicht mehr
/// vorrätig ist).
pub fn agent_result(ctx: &OpContext, request: &AgentResultRequest) -> Result<OpOutput, OpError> {
    let spawner = ctx.managed_spawner().ok_or_else(|| {
        OpError::NotAvailable("kein Agent-Spawner in diesem Kontext konfiguriert".to_owned())
    })?;
    let child = SessionId::try_from_str(request.child_id.clone())
        .map_err(|_| invalid("`child_id` is not a valid session id"))?;
    let text = match request.part {
        AgentResultPart::Result => spawner
            .child_result_text(ctx.session_id(), &child)
            .map_err(|error| {
                OpError::NotAvailable(format!("{AGENT_RESULT_TOOL}: {}", error.message))
            })?,
        // Runde 5, Teil M: das Aktivitätsjournal, nur für den Elternteil.
        AgentResultPart::Journal => spawner
            .child_journal_for(ctx.session_id(), child.as_str())
            .map(|journal| journal.render_full())
            .ok_or_else(|| {
                OpError::NotAvailable(format!(
                    "{AGENT_RESULT_TOOL}: kein Aktivitätsjournal eines eigenen Kindes mit der ID \
                     {child} (oder es ist nicht mehr vorrätig)"
                ))
            })?,
    };
    let page = page_of(&text, request.offset, request.max_bytes);
    let mut rendered = page.text.clone();
    if let Some(next) = page.next_offset() {
        rendered.push_str(&format!(
            "\n[{AGENT_RESULT_TOOL}: Bytes {}–{} von {}; weiter mit offset={next}]",
            page.start, page.end, page.total
        ));
    }
    Ok(OpOutput {
        text: rendered,
        data: Some(json!({
            "child_id": request.child_id,
            "offset": page.start,
            "returned_bytes": page.end - page.start,
            "total_bytes": page.total,
            "next_offset": page.next_offset(),
        })),
    })
}

/// Die Operation `agent.result` als Modell-Werkzeug.
///
/// # Beschreibung
/// Die Composition-Root hängt sie überall dort an, wo eine Sitzung Kinder
/// starten darf: an die Wurzel mit Spawner und an die Orchestrator-Rollen
/// (`harw_registry_defaults::profile::child_result_tools_for_role`). Rein
/// lesend (`readonly = true`, keine Freigabe-Deklaration) und deshalb in
/// `AUTO_APPROVED_TOOLS`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentResultOperation;

impl AgentResultOperation {
    /// Baut die Operation.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Das geschlossene Argument-Schema.
const AGENT_RESULT_ARGS_SCHEMA: ArgsSchemaFn = || {
    object_schema(
        vec![
            (
                "child_id",
                string_schema("Kind-ID aus der Kürzungsmarke der Kind-Antwort."),
            ),
            (
                "offset",
                integer_schema("Byte-Versatz, ab dem geliefert wird (Standard 0; null = 0)."),
            ),
            (
                "max_bytes",
                integer_schema(
                    "Seitengröße in Bytes (Standard und Höchstwert 49152; null = Standard).",
                ),
            ),
            (
                "part",
                string_schema(
                    "\"result\" (Vorgabe: ungekürzter Antworttext) oder \"journal\" \
                     (vollständiges Aktivitätsjournal, auch nach einem Abbruch).",
                ),
            ),
        ],
        &["child_id"],
    )
};

impl Operation for AgentResultOperation {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: AGENT_RESULT_TOOL,
            summary: "Liefert den ungekürzten Antworttext eines eigenen, abgeschlossenen \
                      Kind-Agenten (child_id aus der Kürzungsmarke), optional seitenweise.",
            domain: OperationDomain::Agents,
            permission: PermissionTier::Observer,
            surfaces: vec![Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::None,
            }],
            category: OperationCategory::Agent,
            args_schema: Some(AGENT_RESULT_ARGS_SCHEMA),
            ..OperationMeta::default()
        })
    }

    fn run<'a>(&'a self, ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
        Box::pin(async move {
            if !input.invocation.is_model_tool() {
                return Err(invalid("is only available as a model tool"));
            }
            let request = AgentResultRequest::parse(input.invocation.json_args())?;
            agent_result(ctx, &request)
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
    use harw_operations::operation::{OpInput, Operation, Surface};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use serde_json::json;

    use super::{AGENT_RESULT_MAX_PAGE_BYTES, AgentResultOperation, AgentResultRequest, page_of};
    use crate::context_ext::OpContextCoreExt;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_parse_treats_null_as_unset_and_caps_the_page() -> TestResult {
        let request = AgentResultRequest::parse(&json!({
            "child_id": "abc", "offset": null, "max_bytes": null
        }))
        .map_err(ctx("null gilt als nicht gesetzt"))?;
        assert_eq!(request.child_id, "abc");
        assert_eq!(request.offset, 0);
        assert_eq!(request.max_bytes, AGENT_RESULT_MAX_PAGE_BYTES);

        let request = AgentResultRequest::parse(&json!({
            "child_id": "abc", "offset": 10, "max_bytes": 10_000_000
        }))
        .map_err(ctx("große Seite wird gedeckelt"))?;
        assert_eq!(request.offset, 10);
        assert_eq!(request.max_bytes, AGENT_RESULT_MAX_PAGE_BYTES);

        let request = AgentResultRequest::parse(&json!({ "child_id": "abc", "max_bytes": 0 }))
            .map_err(ctx("0 gilt als nicht gesetzt"))?;
        assert_eq!(request.max_bytes, AGENT_RESULT_MAX_PAGE_BYTES);
        Ok(())
    }

    #[test]
    fn test_parse_rejects_malformed_arguments() {
        for args in [
            json!({}),
            json!({ "child_id": "  " }),
            json!({ "child_id": "a", "offset": -1 }),
            json!({ "child_id": "a", "offset": "3" }),
            json!({ "child_id": "a", "role": "explorer" }),
            json!("a"),
        ] {
            assert!(
                matches!(
                    AgentResultRequest::parse(&args),
                    Err(OpError::InvalidArguments(_))
                ),
                "{args}"
            );
        }
    }

    #[test]
    fn test_page_of_respects_char_boundaries_and_always_advances() {
        let text = "a€b"; // 'a' (1) + '€' (3) + 'b' (1)
        let page = page_of(text, 0, 2);
        assert_eq!(page.text, "a");
        assert_eq!(page.next_offset(), Some(1));
        // Ein Zeichen größer als die Seite wird trotzdem geliefert.
        let page = page_of(text, 1, 1);
        assert_eq!(page.text, "€");
        assert_eq!(page.next_offset(), Some(4));
        // Ein Versatz mitten im Zeichen wird abgerundet.
        let page = page_of(text, 2, 10);
        assert_eq!(page.text, "€b");
        assert_eq!(page.next_offset(), None);
        // Hinter dem Ende: leere Seite.
        let page = page_of(text, 99, 10);
        assert!(page.text.is_empty());
        assert_eq!(page.next_offset(), None);
    }

    #[test]
    fn test_operation_is_a_read_only_model_tool() {
        let operation = AgentResultOperation::new();
        let meta = operation.meta();
        assert_eq!(meta.name, "agent.result");
        assert!(
            meta.surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { readonly: true, .. }))
        );
        assert!(meta.args_schema.is_some());
    }

    /// Kontext mit echtem Spawner ohne Rollen.
    fn runtime_ctx(session: SessionId) -> TestResult<(OpContext, PathBuf, Box<dyn std::any::Any>)> {
        let tmp = std::env::temp_dir().join(format!(
            "harw-agent-result-test-{}-{}",
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
            spawner,
            store,
        );
        let ctx = OpContext::new(session, TurnId::new(), sandbox, services);
        Ok((ctx, tmp, Box::new(event_rx)))
    }

    #[tokio::test]
    async fn test_unknown_or_foreign_child_is_not_available() -> TestResult {
        let (runtime, tmp, _events) = runtime_ctx(SessionId::new())?;
        let result = AgentResultOperation::new()
            .run(
                &runtime,
                OpInput::model_tool(json!({ "child_id": SessionId::new().as_str() })),
            )
            .await;
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(
                    message.contains("kein abgeschlossenes eigenes Kind"),
                    "{message}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet NotAvailable, bekommen {other:?}"
                )));
            }
        }
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }

    #[tokio::test]
    async fn test_operation_without_spawner_is_not_available() -> TestResult {
        let (runtime, tmp, _events) = runtime_ctx(SessionId::new())?;
        let bare = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            runtime.sandbox().clone(),
            ServiceMap::new(),
        );
        let result = AgentResultOperation::new()
            .run(&bare, OpInput::model_tool(json!({ "child_id": "x" })))
            .await;
        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "{result:?}"
        );
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }

    #[tokio::test]
    async fn test_operation_rejects_command_invocations() -> TestResult {
        let (runtime, tmp, _events) = runtime_ctx(SessionId::new())?;
        let result = AgentResultOperation::new()
            .run(&runtime, OpInput::command("/agent.result", Vec::new()))
            .await;
        assert!(
            matches!(result, Err(OpError::InvalidArguments(_))),
            "{result:?}"
        );
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }

    /// Runde 5, Teil M: `part: "journal"` liefert das volle Journal eines
    /// eigenen, abgebrochenen Kindes — nie das eines fremden.
    #[tokio::test]
    async fn test_journal_part_returns_the_full_journal_of_an_own_child() -> TestResult {
        let root = SessionId::new();
        let (runtime, tmp, _events) = runtime_ctx(root.clone())?;
        let spawner = runtime
            .managed_spawner()
            .ok_or(TestError::Missing("Spawner im Kontext"))?;
        let child = SessionId::new();
        let comms = spawner.child_comms();
        comms.open_journal(&child, &root, "root-orchestrator", Some("Umsetzen"));
        comms.record_tool_outcome(&child, "fs.edit", &json!({ "path": "lib.rs" }), true, "ok");
        comms.finalize_end(
            &child,
            &harw_core::ChildEndCause::Cancelled {
                reason: "Nutzerin".to_owned(),
            },
            None,
            None,
        );
        comms.close_journal(&child);
        let output = AgentResultOperation::new()
            .run(
                &runtime,
                OpInput::model_tool(json!({ "child_id": child.as_str(), "part": "journal" })),
            )
            .await
            .map_err(ctx("eigenes Journal ist lesbar"))?;
        assert!(output.text.contains("fs.edit(lib.rs)"), "{}", output.text);
        assert!(
            output.text.contains("[child_end status=cancelled"),
            "{}",
            output.text
        );

        let foreign = SessionId::new();
        comms.open_journal(&foreign, &SessionId::new(), "explorer", None);
        let result = AgentResultOperation::new()
            .run(
                &runtime,
                OpInput::model_tool(json!({ "child_id": foreign.as_str(), "part": "journal" })),
            )
            .await;
        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "{result:?}"
        );
        assert!(AgentResultRequest::parse(&json!({ "child_id": "x", "part": "diff" })).is_err());
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }
}
