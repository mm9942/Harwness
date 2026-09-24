//! `KanbanReadToolProvider` — die Lese-Werkzeuge `kanban.list` und
//! `kanban.show` (Plan D2, `docs/design/knowledge-surfaces.md` §6).
//!
//! # Verantwortung
//! Agenten dürfen das Kanban **lesen** (Entscheidung der Nutzerin, Teil D):
//! - `kanban.list` — Überblick über ein Board: Boards, Spalten (Status- und
//!   Worker-Lanes samt Rolle) und Karten mit Zustand, gedeckelt auf
//!   [`MAX_LIST_CARDS`] Karten und [`MAX_OUTPUT_BYTES`].
//! - `kanban.show` — eine Karte: Titel, Text, Zustand, Belege, letzte
//!   Kommentare, Verlauf und Ergebnis, gedeckelt auf [`MAX_OUTPUT_BYTES`].
//!
//! Beide Werkzeuge schreiben nie: kein Übergang, kein Kommentar, keine
//! Freigabe. Schreibwege bleiben Operator-Befehle (`/kanban …`).
//!
//! # Zustand
//! Der Kartenzustand ist eine Sicht über das Job-Ledger. Mit Ledger
//! ([`KanbanReadToolProvider::with_ledger`]) wird er über
//! `JobTransitions::snapshot` gelesen (nur lesend); ohne Ledger zeigen
//! gebundene Karten `unknown` — nie einen geratenen Zustand.
//!
//! # Sichtbarkeit
//! Karten und Boards tragen heute `OperatorOnly`; `SelfOnly` sähe nichts. Die
//! Nutzerin hat Agenten das Lesen des Kanbans ausdrücklich erlaubt, darum
//! filtern die Werkzeuge nicht nach `VisibilityScope`. Sie liefern keine
//! fremden Dateiinhalte, nur die Kartendaten selbst.
//!
//! # Rechteklasse
//! Rein lesend, [`Permission::ReadWorkspace`]
//! ([`KanbanReadToolProvider::TOOL_PERMISSIONS`]). Trotzdem bewusst **nicht**
//! in `AUTO_APPROVED_TOOLS` (Entscheidung der Nutzerin): jeder Aufruf fragt.
//!
//! # Nutzungsregel
//! Kanban nur benutzen, wenn die Nutzerin ausdrücklich darum bittet (Board
//! ansehen, Karte anlegen, Worker starten) — nie von selbst Aufgaben aufs
//! Board legen oder daraus ableiten ([`USAGE_RULE`], steht in beiden
//! Werkzeugbeschreibungen). Angeboten werden die Werkzeuge nur der
//! UIA-Wurzel und dem Root-Orchestrator
//! (`crate::profile::KANBAN_READ_TOOLS`).
//!
//! # Fehler
//! Kein Aufruf gibt `Err` zurück: ungültige Argumente und Speicherfehler
//! werden als [`ToolOutput::error`] gemeldet.

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_extension_api::contributors::ToolProvider;
use harw_extension_api::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};
use harw_knowledge::KnowledgeStore;
use harw_knowledge::kanban::board::{self, BoardId, CardId, CardRecord, CardState, LaneKind};
use harw_knowledge::kanban::lifecycle::JobTransitions;
use harw_knowledge::kanban::notes;
use harw_tools::{AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType, Permission};
use serde::Deserialize;

/// Name des Überblick-Werkzeugs.
pub const KANBAN_LIST: &str = "kanban.list";

/// Name des Karten-Werkzeugs.
pub const KANBAN_SHOW: &str = "kanban.show";

/// Die Nutzungsregel beider Werkzeuge (Entscheidung der Nutzerin), wörtlich
/// in den Beschreibungen.
pub const USAGE_RULE: &str = "Kanban nur benutzen, wenn die Nutzerin ausdrücklich darum bittet \
     (Board ansehen, Karte anlegen, Worker starten) – nie von selbst Aufgaben aufs Board legen \
     oder daraus ableiten.";

/// Board ohne Angabe (wie `/kanban`).
pub const DEFAULT_BOARD: &str = "default";

/// Höchstzahl gelieferter Karten in `kanban.list`.
pub const MAX_LIST_CARDS: usize = 100;

/// Obergrenze der Ausgabe je Aufruf in Bytes (serialisiertes JSON).
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024;

/// Obergrenze des Kartentexts in `kanban.show` in Bytes.
const SHOW_BODY_BYTES: usize = 3 * 1024;

/// Obergrenze der Ergebniszusammenfassung in `kanban.show` in Bytes.
const SHOW_RESULT_BYTES: usize = 2 * 1024;

/// Kommentare und Verlaufseinträge, die `kanban.show` höchstens zeigt.
const SHOW_TAIL: usize = 5;

/// Obergrenze eines Kommentar-/Verlaufstexts in `kanban.show` in Bytes.
const SHOW_LINE_BYTES: usize = 300;

/// Die Kanban-Lese-Werkzeuge über einem geteilten Wissensspeicher.
#[derive(Clone)]
pub struct KanbanReadToolProvider {
    store: Arc<KnowledgeStore>,
    ledger: Option<Arc<dyn JobTransitions>>,
}

impl std::fmt::Debug for KanbanReadToolProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KanbanReadToolProvider")
            .field("root", &self.store.root())
            .field("ledger", &self.ledger.is_some())
            .finish()
    }
}

impl KanbanReadToolProvider {
    /// Die Werkzeugnamen in Provider-Reihenfolge.
    pub const TOOL_NAMES: &'static [&'static str] = &[KANBAN_LIST, KANBAN_SHOW];

    /// Die Rechteklasse je Werkzeug, parallel zu [`Self::TOOL_NAMES`].
    pub const TOOL_PERMISSIONS: &'static [Option<Permission>] = &[
        Some(Permission::ReadWorkspace),
        Some(Permission::ReadWorkspace),
    ];

    /// Baut den Provider ohne Ledger (gebundene Karten zeigen `unknown`).
    ///
    /// # Argumente
    /// - `store` (`Arc<KnowledgeStore>`): derselbe Speicher wie
    ///   `RuntimeServices::knowledge_store`.
    #[must_use]
    pub fn new(store: Arc<KnowledgeStore>) -> Self {
        Self {
            store,
            ledger: None,
        }
    }

    /// Hängt das Kanban-Job-Ledger an; gelesen wird nur `snapshot`.
    #[must_use]
    pub fn with_ledger(mut self, ledger: Option<Arc<dyn JobTransitions>>) -> Self {
        self.ledger = ledger;
        self
    }
}

impl ToolProvider for KanbanReadToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![list_spec(), show_spec()]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        let tool = match name.as_str() {
            KANBAN_LIST => KanbanTool::List,
            KANBAN_SHOW => KanbanTool::Show,
            _ => return None,
        };
        Some(Arc::new(KanbanReadExecutor {
            store: Arc::clone(&self.store),
            ledger: self.ledger.clone(),
            tool,
        }))
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        Self::TOOL_NAMES.contains(&name.as_str())
    }
}

fn property(schema_type: JsonSchemaType, description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(schema_type),
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

fn object_schema(props: BTreeMap<String, JsonSchema>, required: &[&str]) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(props),
        required: Some(required.iter().map(|name| (*name).to_owned()).collect()),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
    .into_strict()
}

fn list_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "board".to_owned(),
        property(JsonSchemaType::String, "Board-Id; ohne Angabe \"default\"."),
    );
    props.insert(
        "lane".to_owned(),
        property(
            JsonSchemaType::String,
            "Nur Karten dieser Lane (z. B. \"todo\" oder \"worker/explorer\").",
        ),
    );
    props.insert(
        "include_archived".to_owned(),
        property(
            JsonSchemaType::Boolean,
            "Auch archivierte Karten zeigen (Vorgabe: nein).",
        ),
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(KANBAN_LIST),
        description: format!(
            "Liest ein Kanban-Board (nur lesend): vorhandene Boards, Spalten (Status- und \
             Worker-Lanes mit Rolle) und Karten mit Zustand. Höchstens {MAX_LIST_CARDS} \
             Karten und {} KiB. Details einer Karte liefert kanban.show. {USAGE_RULE}",
            MAX_OUTPUT_BYTES / 1024
        ),
        parameters: object_schema(props, &[]),
        strict: true,
    })
}

fn show_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "card".to_owned(),
        property(JsonSchemaType::String, "Karten-Id, z. B. \"card-3\"."),
    );
    props.insert(
        "board".to_owned(),
        property(JsonSchemaType::String, "Board-Id; ohne Angabe \"default\"."),
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(KANBAN_SHOW),
        description: format!(
            "Liest eine Kanban-Karte (nur lesend): Titel, Text, Zustand, Belege, letzte \
             Kommentare, Verlauf und Ergebnis des letzten Worker-Laufs. Gekürzt auf {} KiB. \
             {USAGE_RULE}",
            MAX_OUTPUT_BYTES / 1024
        ),
        parameters: object_schema(props, &["card"]),
        strict: true,
    })
}

/// Welches Werkzeug ein Executor bedient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KanbanTool {
    List,
    Show,
}

struct KanbanReadExecutor {
    store: Arc<KnowledgeStore>,
    ledger: Option<Arc<dyn JobTransitions>>,
    tool: KanbanTool,
}

impl ToolExecutor for KanbanReadExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let arguments = call.arguments.clone();
        Box::pin(async move {
            let ledger = self.ledger.as_deref();
            Ok(match self.tool {
                KanbanTool::List => execute_list(&self.store, ledger, arguments),
                KanbanTool::Show => execute_show(&self.store, ledger, arguments),
            })
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    board: Option<String>,
    #[serde(default)]
    lane: Option<String>,
    #[serde(default)]
    include_archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowArgs {
    card: String,
    #[serde(default)]
    board: Option<String>,
}

fn parse_args<T: serde::de::DeserializeOwned + Default>(
    tool: &str,
    arguments: serde_json::Value,
) -> Result<T, ToolOutput> {
    if arguments.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(arguments)
        .map_err(|error| ToolOutput::error(format!("{tool}: ungültige Argumente: {error}")))
}

/// Board-Id aus dem Argument; leer bzw. fehlend heißt `default`.
fn board_of(raw: Option<&str>) -> BoardId {
    BoardId::new(
        raw.map(str::trim)
            .filter(|board| !board.is_empty())
            .unwrap_or(DEFAULT_BOARD),
    )
}

/// Zustand einer Karte als Text; `unknown`, wenn er ohne Ledger nicht bekannt ist.
fn state_of(record: &CardRecord, ledger: Option<&dyn JobTransitions>) -> (String, Option<String>) {
    let view = match (ledger, &record.work_id) {
        (Some(ledger), _) => record.view_with(ledger).ok(),
        (None, None) => record.view(None).ok(),
        (None, Some(_)) => None,
    };
    match view.map(|card| card.state) {
        Some(CardState::Blocked { reason_kind }) => {
            ("blocked".to_owned(), Some(reason_kind.label().to_owned()))
        }
        Some(state) => (state.label().to_owned(), None),
        None => ("unknown".to_owned(), None),
    }
}

/// Kern von `kanban.list` (testbar ohne Sandbox-Kontext).
fn execute_list(
    store: &KnowledgeStore,
    ledger: Option<&dyn JobTransitions>,
    arguments: serde_json::Value,
) -> ToolOutput {
    let fail = |detail: String| ToolOutput::error(format!("{KANBAN_LIST}: {detail}"));
    let args: ListArgs = match parse_args(KANBAN_LIST, arguments) {
        Ok(args) => args,
        Err(output) => return output,
    };
    let board_id = board_of(args.board.as_deref());
    let boards = match board::list_board_ids(store) {
        Ok(boards) => boards,
        Err(error) => return fail(error.to_string()),
    };
    let board_names: Vec<&str> = boards.iter().map(BoardId::as_str).collect();
    if !boards.contains(&board_id) {
        return ToolOutput::json(serde_json::json!({
            "board": board_id.as_str(),
            "boards": board_names,
            "lanes": [],
            "cards": [],
            "total": 0,
            "truncated": false,
        }));
    }
    let lanes = match board::load_board(store, &board_id) {
        Ok((_, lanes)) => lanes,
        Err(error) => return fail(error.to_string()),
    };
    let records = match board::list_card_records(store, &board_id) {
        Ok(records) => records,
        Err(error) => return fail(error.to_string()),
    };
    let include_archived = args.include_archived.unwrap_or(false);
    let lane_filter = args
        .lane
        .as_deref()
        .map(str::trim)
        .filter(|lane| !lane.is_empty());
    let mut cards: Vec<serde_json::Value> = Vec::new();
    let mut total = 0usize;
    let mut lane_counts: BTreeMap<String, usize> = BTreeMap::new();
    for record in &records {
        if lane_filter.is_some_and(|lane| lane != record.lane_id.as_str()) {
            continue;
        }
        let (state, blocked_reason) = state_of(record, ledger);
        if state == "archived" && !include_archived {
            continue;
        }
        total += 1;
        *lane_counts
            .entry(record.lane_id.as_str().to_owned())
            .or_default() += 1;
        if cards.len() >= MAX_LIST_CARDS {
            continue;
        }
        let card_notes = notes::load_notes(store, &board_id, &record.id).unwrap_or_default();
        cards.push(serde_json::json!({
            "id": record.id.as_str(),
            "lane": record.lane_id.as_str(),
            "title": notes::clip(&record.title, 200),
            "state": state,
            "blocked_reason": blocked_reason,
            "tags": record.tags,
            "comments": card_notes.comments.len(),
            "result": card_notes.result.as_ref().map(|result| result.status.key()),
        }));
    }
    let lanes: Vec<serde_json::Value> = lanes
        .iter()
        .map(|lane| {
            let (kind, role) = match &lane.kind {
                LaneKind::Status(_) => ("status", None),
                LaneKind::Worker { agent_role } => ("worker", Some(agent_role.as_str())),
            };
            serde_json::json!({
                "id": lane.id.as_str(),
                "kind": kind,
                "role": role,
                "cards": lane_counts.get(lane.id.as_str()).copied().unwrap_or(0),
            })
        })
        .collect();
    let mut truncated = total > cards.len();
    loop {
        let value = serde_json::json!({
            "board": board_id.as_str(),
            "boards": board_names,
            "lanes": lanes,
            "cards": cards,
            "total": total,
            "truncated": truncated,
        });
        if cards.is_empty() || value.to_string().len() <= MAX_OUTPUT_BYTES {
            return ToolOutput::json(value);
        }
        cards.pop();
        truncated = true;
    }
}

/// Kern von `kanban.show` (testbar ohne Sandbox-Kontext).
fn execute_show(
    store: &KnowledgeStore,
    ledger: Option<&dyn JobTransitions>,
    arguments: serde_json::Value,
) -> ToolOutput {
    let fail = |detail: String| ToolOutput::error(format!("{KANBAN_SHOW}: {detail}"));
    let args: ShowArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(error) => return fail(format!("ungültige Argumente: {error}")),
    };
    let board_id = board_of(args.board.as_deref());
    let card_id = CardId::new(args.card.trim());
    let record = match board::load_card_record(store, &board_id, &card_id) {
        Ok(record) => record,
        Err(harw_knowledge::KnowledgeError::Io(io))
            if matches!(
                io.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
            ) =>
        {
            return fail(format!("keine Karte '{card_id}' auf Board {board_id}"));
        }
        Err(error) => return fail(error.to_string()),
    };
    let card_notes = match notes::load_notes(store, &board_id, &card_id) {
        Ok(card_notes) => card_notes,
        Err(error) => return fail(error.to_string()),
    };
    let (state, blocked_reason) = state_of(&record, ledger);
    let tail = |len: usize| len.saturating_sub(SHOW_TAIL);
    let comments: Vec<serde_json::Value> = card_notes
        .comments
        .iter()
        .skip(tail(card_notes.comments.len()))
        .map(|comment| {
            serde_json::json!({
                "at": comment.at.to_string(),
                "author": comment.author,
                "text": notes::clip(&comment.text, SHOW_LINE_BYTES),
            })
        })
        .collect();
    let history: Vec<serde_json::Value> = card_notes
        .history
        .iter()
        .skip(tail(card_notes.history.len()))
        .map(|entry| {
            serde_json::json!({
                "at": entry.at.to_string(),
                "event": entry.event.label(),
                "actor": entry.actor,
                "detail": notes::clip(&entry.detail, SHOW_LINE_BYTES),
            })
        })
        .collect();
    let result = card_notes.result.as_ref().map(|result| {
        serde_json::json!({
            "status": result.status.key(),
            "at": result.at.to_string(),
            "role": result.role,
            "summary": notes::clip(result.summary.trim(), SHOW_RESULT_BYTES),
        })
    });
    let mut body_bytes = SHOW_BODY_BYTES;
    loop {
        let value = serde_json::json!({
            "board": board_id.as_str(),
            "id": record.id.as_str(),
            "lane": record.lane_id.as_str(),
            "title": record.title,
            "state": state,
            "blocked_reason": blocked_reason,
            "work_id": record.work_id.as_ref().map(|work_id| work_id.as_str()),
            "parents": record.parents.iter().map(CardId::as_str).collect::<Vec<_>>(),
            "tags": record.tags,
            "body": notes::clip(record.body.trim(), body_bytes),
            "evidence": card_notes.evidence,
            "comments": comments,
            "comment_count": card_notes.comments.len(),
            "history": history,
            "result": result,
        });
        if body_bytes == 0 || value.to_string().len() <= MAX_OUTPUT_BYTES {
            return ToolOutput::json(value);
        }
        body_bytes /= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_knowledge::kanban::board::{Board, Lane, LaneId, save_board, save_card};
    use harw_knowledge::kanban::lifecycle::InMemoryJobTransitions;
    use harw_knowledge::kanban::notes::{HistoryEntry, HistoryEvent};
    use harw_knowledge::{AgentId, AgentRoleRef, VisibilityScope};

    fn temporary_store(label: &str) -> TestResult<Arc<KnowledgeStore>> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-registry-kanban-{label}-{}-{nonce}",
            std::process::id()
        ));
        Ok(Arc::new(KnowledgeStore::new(&root)))
    }

    fn json(output: ToolOutput) -> TestResult<serde_json::Value> {
        match output {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("expected json: {other:?}"))),
        }
    }

    fn is_error(output: &ToolOutput) -> bool {
        !matches!(output, ToolOutput::Json { .. })
    }

    /// Board `default` mit einer Status- und einer Worker-Lane und drei Karten.
    fn seed(store: &KnowledgeStore, ledger: &InMemoryJobTransitions) -> TestResult {
        let board_id = BoardId::new(DEFAULT_BOARD);
        let lanes = vec![
            Lane {
                id: LaneId::new("triage"),
                board_id: board_id.clone(),
                kind: LaneKind::Status(CardState::Triage),
                bound_worker: None,
            },
            Lane {
                id: LaneId::new("worker/explorer"),
                board_id: board_id.clone(),
                kind: LaneKind::Worker {
                    agent_role: AgentRoleRef::new("explorer"),
                },
                bound_worker: None,
            },
        ];
        save_board(
            store,
            &Board {
                id: board_id.clone(),
                name: "Default".to_owned(),
                lanes: lanes.iter().map(|lane| lane.id.clone()).collect(),
                visibility: VisibilityScope::OperatorOnly,
            },
            &lanes,
        )
        .map_err(ctx("save board"))?;
        let author = AgentId::new("operator");
        let now = jiff::Timestamp::now();
        let placeholder = CardRecord {
            id: CardId::new("card-2"),
            lane_id: LaneId::new("worker/explorer"),
            title: String::new(),
            body: String::new(),
            work_id: None,
            parents: Vec::new(),
            tags: Vec::new(),
            visibility: VisibilityScope::OperatorOnly,
        };
        let work_id = ledger.create(&placeholder).map_err(ctx("create job"))?;
        ledger.mark_ready(&work_id).map_err(ctx("ready job"))?;
        for (id, lane, work, tags) in [
            ("card-1", "triage", None, Vec::new()),
            (
                "card-2",
                "worker/explorer",
                Some(work_id.clone()),
                Vec::new(),
            ),
            ("card-3", "triage", None, vec!["archived".to_owned()]),
        ] {
            save_card(
                store,
                &board_id,
                &CardRecord {
                    id: CardId::new(id),
                    lane_id: LaneId::new(lane),
                    title: format!("Titel {id}"),
                    body: "Text".to_owned(),
                    work_id: work,
                    parents: Vec::new(),
                    tags,
                    visibility: VisibilityScope::OperatorOnly,
                },
                &author,
                now,
            )
            .map_err(ctx("save card"))?;
        }
        notes::update_card(
            store,
            &board_id,
            &CardId::new("card-2"),
            now,
            |_, card_notes| {
                card_notes.add_comment(now, "operator", "bitte gründlich")?;
                card_notes.add_evidence("docs/plan.md")?;
                card_notes.record(HistoryEntry::new(
                    now,
                    HistoryEvent::ApprovalRequested,
                    "worker",
                    "Rolle explorer",
                    None,
                ));
                Ok(())
            },
        )
        .map_err(ctx("notes"))?;
        Ok(())
    }

    #[test]
    fn provider_lists_two_read_only_tools() {
        let provider =
            KanbanReadToolProvider::new(Arc::new(KnowledgeStore::new(std::path::Path::new("/x"))));
        let names: Vec<String> = provider
            .tools()
            .iter()
            .map(|spec| {
                let ToolSpec::Function(function) = spec;
                function.name.as_str().to_owned()
            })
            .collect();
        assert_eq!(names, vec![KANBAN_LIST.to_owned(), KANBAN_SHOW.to_owned()]);
        assert!(provider.executor(&ToolName::new(KANBAN_LIST)).is_some());
        assert!(provider.executor(&ToolName::new(KANBAN_SHOW)).is_some());
        assert!(provider.executor(&ToolName::new("kanban.move")).is_none());
        assert!(provider.parallel_safe(&ToolName::new(KANBAN_SHOW)));
        // Nutzungsregel der Nutzerin steht in beiden Beschreibungen.
        for spec in provider.tools() {
            let ToolSpec::Function(function) = spec;
            assert!(
                function.description.contains(USAGE_RULE),
                "{}",
                function.name
            );
        }
        assert_eq!(
            KanbanReadToolProvider::TOOL_PERMISSIONS,
            &[
                Some(Permission::ReadWorkspace),
                Some(Permission::ReadWorkspace)
            ]
        );
    }

    #[test]
    fn list_shows_lanes_cards_and_ledger_state() -> TestResult {
        let store = temporary_store("list")?;
        let ledger = InMemoryJobTransitions::new();
        seed(&store, &ledger)?;

        let content = json(execute_list(&store, Some(&ledger), serde_json::Value::Null))?;
        assert_eq!(content["board"], "default");
        assert_eq!(content["boards"][0], "default");
        assert_eq!(content["total"], 2, "archived cards are hidden");
        assert_eq!(content["lanes"][1]["role"], "explorer");
        assert_eq!(content["lanes"][1]["cards"], 1);
        assert_eq!(content["cards"][1]["state"], "ready");
        assert_eq!(content["cards"][1]["comments"], 1);

        // Ohne Ledger: gebundene Karte „unknown", nie geraten.
        let blind = json(execute_list(&store, None, serde_json::json!({})))?;
        assert_eq!(blind["cards"][1]["state"], "unknown");

        let all = json(execute_list(
            &store,
            None,
            serde_json::json!({ "include_archived": true, "lane": "triage", "board": null }),
        ))?;
        assert_eq!(all["total"], 2);
        assert_eq!(all["cards"][1]["state"], "archived");

        let missing = json(execute_list(
            &store,
            None,
            serde_json::json!({ "board": "gibt-es-nicht" }),
        ))?;
        assert_eq!(missing["total"], 0);
        assert!(is_error(&execute_list(
            &store,
            None,
            serde_json::json!({ "frob": 1 })
        )));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn show_returns_notes_and_rejects_unknown_cards() -> TestResult {
        let store = temporary_store("show")?;
        let ledger = InMemoryJobTransitions::new();
        seed(&store, &ledger)?;
        let content = json(execute_show(
            &store,
            Some(&ledger),
            serde_json::json!({ "card": "card-2" }),
        ))?;
        assert_eq!(content["title"], "Titel card-2");
        assert_eq!(content["state"], "ready");
        assert_eq!(content["evidence"][0], "docs/plan.md");
        assert_eq!(content["comments"][0]["text"], "bitte gründlich");
        assert_eq!(content["history"][0]["event"], "Freigabe angefragt");
        assert!(content["result"].is_null());

        for arguments in [
            serde_json::json!({ "card": "card-9" }),
            serde_json::json!({ "card": "../x" }),
            serde_json::json!({}),
        ] {
            assert!(is_error(&execute_show(&store, None, arguments)));
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn outputs_are_capped() -> TestResult {
        let store = temporary_store("caps")?;
        let ledger = InMemoryJobTransitions::new();
        seed(&store, &ledger)?;
        let board_id = BoardId::new(DEFAULT_BOARD);
        let now = jiff::Timestamp::now();
        for index in 10..(10 + MAX_LIST_CARDS + 20) {
            save_card(
                &store,
                &board_id,
                &CardRecord {
                    id: CardId::new(format!("card-{index}")),
                    lane_id: LaneId::new("triage"),
                    title: "x".repeat(150),
                    body: String::new(),
                    work_id: None,
                    parents: Vec::new(),
                    tags: Vec::new(),
                    visibility: VisibilityScope::OperatorOnly,
                },
                &AgentId::new("operator"),
                now,
            )
            .map_err(ctx("save card"))?;
        }
        let output = execute_list(&store, None, serde_json::Value::Null);
        let content = json(output)?;
        assert_eq!(content["truncated"], true);
        assert!(content.to_string().len() <= MAX_OUTPUT_BYTES);

        notes::update_card(
            &store,
            &board_id,
            &CardId::new("card-1"),
            now,
            |record, _| {
                record.body = "y".repeat(40 * 1024);
                Ok(())
            },
        )
        .map_err(ctx("long body"))?;
        let shown = json(execute_show(
            &store,
            None,
            serde_json::json!({ "card": "card-1" }),
        ))?;
        assert!(shown.to_string().len() <= MAX_OUTPUT_BYTES);
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }
}
