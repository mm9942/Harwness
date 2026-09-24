//! `/kanban` — Board-Projektion des Arbeitsgraphen
//! (`docs/design/knowledge-surfaces.md` §6, `interaction-contract.md` §2.2).
//!
//! # Subcommands
//! - (bare) / `list [--board=<b>] [--all]` / `show` (ohne Karte) — Karten je
//!   Spalte (`CardState`); `--all` zeigt im Text auch archivierte.
//!   `OpOutput::data` = `{"board":{"id","name"},"lanes":[{"id","title",
//!   "kind":"status|worker","state","worker_role"}],"cards":[{"id","lane_id",
//!   "title","state","assignee","tags","retry_count","blocked_reason",
//!   "work_id"}]}` (Kartenzustand `unknown` ohne Ledger).
//! - `boards` — alle Boards.
//! - `create <title> [--lane=<LaneRef>] [--assignee=<AgentRoleRef>]
//!   [--parent=<CardRef>]… [--tag=<t>]… [--board=<b>]` — legt eine Karte an.
//!   Ohne `--lane` landet sie in `triage`; `--assignee=<role>` routet sie in
//!   die Worker-Lane `worker/<role>` (wird bei Bedarf angelegt). In einer
//!   Worker-Lane wird sie sofort Arbeit: Job anlegen (`Todo`) und — wenn alle
//!   Eltern `Done` sind — `Ready` (Vertrag: „already a `WorkId` if the lane
//!   is a worker lane").
//! - `show <CardRef>` — voller Kartenzustand samt Ledger-Snapshot.
//! - `todo <CardRef>` / `ready <CardRef>` — §6.3 `Triage -> Todo` bzw.
//!   `Todo -> Ready` (Eltern-Gate). Nicht im Vertrag, aber ohne sie gäbe es
//!   für Status-Lanes keinen Weg in die Warteschlange.
//! - `claim|complete|unblock|archive <CardRef>`,
//!   `block <CardRef> [<BlockKind>] [--reason=<BlockKind>]` (Vorgabe `Dependency`
//!   nur bei `move … blocked`; `block` verlangt einen Grund).
//! - TUI-Aliasse: `add <title>` = `create`, `done <CardRef>` = `complete`,
//!   `move <CardRef> <todo|ready|running|done|blocked|archived> [<BlockKind>]`
//!   wählt den passenden §6.3-Übergang (`ready` aus `blocked` = `unblock`,
//!   aus `running` = `reclaim`).
//!
//! `LaneRef` darf `board:<b>/<lane>` sein und wählt damit das Board.
//!
//! # Karte = Sicht über das Ledger
//! Jede Zustandsänderung läuft über `harw_knowledge::kanban::lifecycle` und
//! damit über [`JobTransitions`] (`Arc<dyn JobTransitions>` aus dem
//! Kontext). Ohne Ledger sind alle Übergänge [`OpError::NotAvailable`];
//! `list`/`show` zeigen gebundene Karten dann als „unbekannt", nie mit einem
//! geratenen Zustand. `create` in einer Status-Lane braucht kein Ledger.
//!
//! # Freigabe-Gate bei `claim` (§6.4)
//! Worker-Lanes gelten als `RiskLevel::High` (fail-closed: die Rolle →
//! Risiko-Auflösung existiert in dieser Schicht noch nicht). `/kanban claim`
//! ist ein ausdrückliches Operator-Kommando (kein Modell-Werkzeug); das
//! Kommando selbst ist die Freigabe und wird als
//! `ApprovalProof(ApprovedOnce, <operator>)` übergeben. Ein automatischer
//! Dispatcher muss eine echte Freigabe beibringen.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Knowledge-Store bzw. kein Job-Ledger.
//! - [`OpError::InvalidArguments`] — Grammatik, unbekannte Karte/Lane,
//!   unzulässiger Übergang, Gates.
//! - [`OpError::Execution`] — Ein-/Ausgabe- oder Ledger-Fehler.

use std::sync::Arc;

use harw_knowledge::kanban::board::{
    self, BlockKind, Board, BoardId, Card, CardId, CardRecord, CardState, Lane, LaneId, LaneKind,
};
use harw_knowledge::kanban::lifecycle::{self, ApprovalProof, JobTransitions};
use harw_knowledge::{AgentId, AgentRoleRef, KnowledgeStore, VisibilityScope};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_types::{ReviewDecision, RiskLevel};

use crate::workbench::{
    caller_agent, knowledge_store, map_knowledge_error, split_flag, split_repeated_flag,
};

/// Board, das ohne `--board` gilt.
pub const DEFAULT_BOARD: &str = "default";

/// Lane, in der eine Karte ohne `--lane` landet.
const DEFAULT_LANE: &str = "triage";

/// Präfix der Worker-Lanes (`worker/<role>`).
const WORKER_LANE_PREFIX: &str = "worker/";

/// Obergrenze eines Kartentitels in Bytes.
const MAX_TITLE_BYTES: usize = 512;

/// Argument-Container für `/kanban`: rohe Tokens.
#[derive(Debug, Default, serde::Deserialize)]
pub struct KanbanArgs {
    /// Alle Tokens nach `/kanban`.
    #[serde(default)]
    pub tokens: Vec<String>,
}

impl harw_operations::FromRawArgs for KanbanArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            tokens: tokens.to_vec(),
        })
    }
}

/// Führt `/kanban` aus.
///
/// # Fehler
/// Siehe Moduldoku.
#[operation(
    name = "kanban",
    summary = "Kanban: list, boards, create, show, todo, ready, claim, complete, block, unblock, archive.",
    domain = "knowledge",
    permission = "operator",
    command(path = "/kanban", visibility = "channel_parity")
)]
async fn kanban(ctx: &OpContext, args: KanbanArgs) -> Result<OpOutput, OpError> {
    let store = knowledge_store(ctx)?;
    let jobs = ctx.service::<Arc<dyn JobTransitions>>().cloned();
    run_kanban(
        &store,
        jobs.as_deref(),
        &caller_agent(ctx),
        &args.tokens,
        jiff::Timestamp::now(),
    )
}

/// Der reine Kern von `/kanban` (testbar ohne `OpContext`).
///
/// # Argumente
/// - `jobs` — das Job-Ledger, falls registriert.
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_kanban(
    store: &KnowledgeStore,
    jobs: Option<&dyn JobTransitions>,
    caller: &AgentId,
    tokens: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let (board_flag, rest) = split_flag(tokens, "--board=");
    let mut board_id = BoardId::new(board_flag.unwrap_or_else(|| DEFAULT_BOARD.to_owned()));
    let sub = rest.first().map(String::as_str).unwrap_or("list");
    let tail = rest.get(1..).unwrap_or_default();
    match sub {
        "list" => list(store, jobs, &board_id, tail),
        "show" if tail.is_empty() => list(store, jobs, &board_id, tail),
        "boards" => boards(store),
        "create" | "add" => create(store, jobs, caller, &mut board_id, tail, now),
        "show" => show(store, jobs, &board_id, tail),
        "todo" | "ready" | "claim" | "complete" | "done" | "block" | "unblock" | "archive"
        | "move" => {
            let request = parse_transition(sub, tail)?;
            let jobs = jobs.ok_or_else(|| {
                OpError::NotAvailable(
                    "kein Job-Ledger (JobTransitions) im Kontext — Kartenübergänge laufen nur über das Ledger"
                        .to_owned(),
                )
            })?;
            transition(store, jobs, caller, &board_id, &request, now)
        }
        other => Err(OpError::InvalidArguments(format!(
            "unbekannter /kanban-Subcommand: {other} (list, boards, create|add, show, todo, ready, claim, complete|done, block, unblock, archive, move)"
        ))),
    }
}

/// Ein kanonischer §6.3-Übergang.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Todo,
    Ready,
    Claim,
    Complete,
    Block(BlockKind),
    Unblock,
    Archive,
}

/// Ein aufgelöster Übergangswunsch: Karte plus Aktion.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TransitionRequest {
    card: CardId,
    action: Action,
}

/// Parst `todo|ready|claim|complete|done|unblock|archive <card>`,
/// `block <card> [<kind>] [--reason=<kind>]` und `move <card> <state> [<kind>]`.
fn parse_transition(sub: &str, tail: &[String]) -> Result<TransitionRequest, OpError> {
    let (reason_flag, rest) = split_flag(tail, "--reason=");
    let card = card_ref(&rest, &format!("/kanban {sub} <card>"))?;
    let extra = rest.get(1..).unwrap_or_default();
    let parse_reason = |raw: &str| {
        BlockKind::parse(raw).ok_or_else(|| {
            OpError::InvalidArguments(format!("unbekannter Grund '{raw}' ({})", reason_names()))
        })
    };
    let action = match sub {
        "todo" => Action::Todo,
        "ready" => Action::Ready,
        "claim" => Action::Claim,
        "complete" | "done" => Action::Complete,
        "unblock" => Action::Unblock,
        "archive" => Action::Archive,
        "block" => {
            let raw = reason_flag
                .or_else(|| extra.first().cloned())
                .ok_or_else(|| {
                    OpError::InvalidArguments(format!(
                        "Aufruf: /kanban block <card> <{}>",
                        reason_names()
                    ))
                })?;
            Action::Block(parse_reason(&raw)?)
        }
        "move" => {
            let target = extra.first().ok_or_else(|| {
                OpError::InvalidArguments(
                    "Aufruf: /kanban move <card> <todo|ready|running|done|blocked|archived>"
                        .to_owned(),
                )
            })?;
            match target.to_ascii_lowercase().as_str() {
                "todo" => Action::Todo,
                "ready" => Action::Ready,
                "running" => Action::Claim,
                "done" => Action::Complete,
                "archived" => Action::Archive,
                "blocked" => {
                    let raw = reason_flag.or_else(|| extra.get(1).cloned());
                    Action::Block(match raw {
                        Some(raw) => parse_reason(&raw)?,
                        None => BlockKind::Dependency,
                    })
                }
                other => {
                    return Err(OpError::InvalidArguments(format!(
                        "unbekannter Zielzustand '{other}' (todo, ready, running, done, blocked, archived)"
                    )));
                }
            }
        }
        other => {
            return Err(OpError::InvalidArguments(format!(
                "unbekannter Übergang: {other}"
            )));
        }
    };
    Ok(TransitionRequest { card, action })
}

// --- Lesen -------------------------------------------------------------------

fn boards(store: &KnowledgeStore) -> Result<OpOutput, OpError> {
    let ids = board::list_board_ids(store).map_err(map_knowledge_error)?;
    if ids.is_empty() {
        return Ok(OpOutput::from("Keine Boards.".to_owned()));
    }
    let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
    Ok(OpOutput::from(format!("Boards: {}", names.join(", "))))
}

/// Zustandsbezeichnung einer Karte; `None`, wenn sie ohne Ledger unbekannt ist.
fn view(record: &CardRecord, jobs: Option<&dyn JobTransitions>) -> Result<Option<Card>, OpError> {
    match (jobs, &record.work_id) {
        (Some(jobs), _) => record
            .view_with(jobs)
            .map(Some)
            .map_err(map_knowledge_error),
        (None, None) => record.view(None).map(Some).map_err(map_knowledge_error),
        (None, Some(_)) => Ok(None),
    }
}

fn state_text(state: &CardState) -> String {
    match state {
        CardState::Blocked { reason_kind } => format!("blocked ({})", reason_kind.label()),
        other => other.label().to_owned(),
    }
}

/// Nutzlast der Board-Ansicht für die TUI (siehe Moduldoku).
fn board_data(
    store: &KnowledgeStore,
    board_id: &BoardId,
    views: &[(CardRecord, Option<Card>)],
) -> Result<serde_json::Value, OpError> {
    let exists = board::list_board_ids(store)
        .map_err(map_knowledge_error)?
        .contains(board_id);
    let (name, lanes) = if exists {
        let (loaded, lanes) = board::load_board(store, board_id).map_err(map_knowledge_error)?;
        (loaded.name, lanes)
    } else {
        (board_id.to_string(), default_lanes(board_id))
    };
    let lanes: Vec<serde_json::Value> = lanes
        .iter()
        .map(|lane| match &lane.kind {
            LaneKind::Status(state) => serde_json::json!({
                "id": lane.id.as_str(),
                "title": lane.id.as_str(),
                "kind": "status",
                "state": state.label(),
                "worker_role": serde_json::Value::Null,
            }),
            LaneKind::Worker { agent_role } => serde_json::json!({
                "id": lane.id.as_str(),
                "title": lane.id.as_str(),
                "kind": "worker",
                "state": serde_json::Value::Null,
                "worker_role": agent_role.as_str(),
            }),
        })
        .collect();
    let cards: Vec<serde_json::Value> = views
        .iter()
        .map(|(record, card)| {
            let state = card.as_ref().map_or("unknown", |card| card.state.label());
            let blocked_reason = match card.as_ref().map(|card| card.state) {
                Some(CardState::Blocked { reason_kind }) => Some(reason_kind.label()),
                _ => None,
            };
            serde_json::json!({
                "id": record.id.as_str(),
                "lane_id": record.lane_id.as_str(),
                "title": record.title,
                "state": state,
                "assignee": card.as_ref().and_then(|card| card.assignee.as_ref()).map(AgentId::as_str),
                "tags": record.tags,
                "retry_count": card.as_ref().map_or(0, |card| card.retry_count),
                "blocked_reason": blocked_reason,
                "work_id": record.work_id.as_ref().map(|work_id| work_id.as_str()),
            })
        })
        .collect();
    Ok(serde_json::json!({
        "board": { "id": board_id.as_str(), "name": name },
        "lanes": lanes,
        "cards": cards,
    }))
}

fn list(
    store: &KnowledgeStore,
    jobs: Option<&dyn JobTransitions>,
    board_id: &BoardId,
    tail: &[String],
) -> Result<OpOutput, OpError> {
    let show_archived = tail.iter().any(|token| token == "--all");
    let records = board::list_card_records(store, board_id).map_err(map_knowledge_error)?;
    let views = records
        .into_iter()
        .map(|record| view(&record, jobs).map(|card| (record, card)))
        .collect::<Result<Vec<_>, OpError>>()?;
    let data = Some(board_data(store, board_id, &views)?);
    if views.is_empty() {
        return Ok(OpOutput {
            text: format!("Board {board_id} hat keine Karten."),
            data,
        });
    }
    let columns = [
        "triage", "todo", "ready", "running", "blocked", "done", "archived",
    ];
    let mut grouped: Vec<(&str, Vec<String>)> =
        columns.iter().map(|column| (*column, Vec::new())).collect();
    let mut unknown = Vec::new();
    for (record, card) in &views {
        let line = format!("{}  {}", record.id, record.title);
        match card {
            Some(card) => {
                let label = card.state.label();
                if let Some((_, lines)) = grouped.iter_mut().find(|(column, _)| *column == label) {
                    let suffix = match card.state {
                        CardState::Blocked { reason_kind } => format!(" [{}]", reason_kind.label()),
                        _ => String::new(),
                    };
                    lines.push(format!("{line}{suffix}"));
                }
            }
            None => unknown.push(line),
        }
    }
    let mut out = format!("Board {board_id}\n");
    for (column, lines) in &grouped {
        if lines.is_empty() || (*column == "archived" && !show_archived) {
            continue;
        }
        out.push_str(&format!("{column} ({})\n", lines.len()));
        for line in lines {
            out.push_str(&format!("  {line}\n"));
        }
    }
    if !unknown.is_empty() {
        out.push_str(&format!(
            "unbekannt — kein Job-Ledger ({})\n",
            unknown.len()
        ));
        for line in &unknown {
            out.push_str(&format!("  {line}\n"));
        }
    }
    Ok(OpOutput { text: out, data })
}

fn card_ref(tail: &[String], grammar: &str) -> Result<CardId, OpError> {
    tail.first()
        .map(|raw| CardId::new(raw.trim()))
        .ok_or_else(|| OpError::InvalidArguments(format!("Aufruf: {grammar}")))
}

fn load_record(
    store: &KnowledgeStore,
    board_id: &BoardId,
    card_id: &CardId,
) -> Result<CardRecord, OpError> {
    board::load_card_record(store, board_id, card_id).map_err(|error| match error {
        harw_knowledge::KnowledgeError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            OpError::InvalidArguments(format!("keine Karte '{card_id}' auf Board {board_id}"))
        }
        other => map_knowledge_error(other),
    })
}

fn show(
    store: &KnowledgeStore,
    jobs: Option<&dyn JobTransitions>,
    board_id: &BoardId,
    tail: &[String],
) -> Result<OpOutput, OpError> {
    let card_id = card_ref(tail, "/kanban show <card>")?;
    let record = load_record(store, board_id, &card_id)?;
    let mut out = format!(
        "{} — {}\nBoard: {board_id}\nLane: {}\n",
        record.id, record.title, record.lane_id
    );
    match view(&record, jobs)? {
        Some(card) => {
            out.push_str(&format!("Zustand: {}\n", state_text(&card.state)));
            if let Some(assignee) = &card.assignee {
                out.push_str(&format!("Halter: {assignee}\n"));
            }
            out.push_str(&format!("Versuche: {}\n", card.retry_count));
        }
        None => out.push_str("Zustand: unbekannt (kein Job-Ledger)\n"),
    }
    match &record.work_id {
        Some(work_id) => out.push_str(&format!("WorkId: {work_id}\n")),
        None => out.push_str("WorkId: — (noch keine Arbeit)\n"),
    }
    if !record.parents.is_empty() {
        let parents: Vec<String> = record.parents.iter().map(ToString::to_string).collect();
        out.push_str(&format!("Eltern: {}\n", parents.join(", ")));
    }
    if !record.tags.is_empty() {
        out.push_str(&format!("Tags: {}\n", record.tags.join(", ")));
    }
    if !record.body.trim().is_empty() {
        out.push_str(&format!("\n{}\n", record.body.trim()));
    }
    Ok(OpOutput::from(out))
}

// --- Anlegen -----------------------------------------------------------------

/// Die Status-Lanes eines neuen Boards, eine je Spalte.
fn default_lanes(board_id: &BoardId) -> Vec<Lane> {
    [
        ("triage", CardState::Triage),
        ("todo", CardState::Todo),
        ("ready", CardState::Ready),
        ("running", CardState::Running),
        (
            "blocked",
            CardState::Blocked {
                reason_kind: BlockKind::Dependency,
            },
        ),
        ("done", CardState::Done),
    ]
    .into_iter()
    .map(|(id, state)| Lane {
        id: LaneId::new(id),
        board_id: board_id.clone(),
        kind: LaneKind::Status(state),
        bound_worker: None,
    })
    .collect()
}

/// Lädt ein Board oder legt es mit den Status-Lanes an.
fn ensure_board(store: &KnowledgeStore, board_id: &BoardId) -> Result<(Board, Vec<Lane>), OpError> {
    let exists = board::list_board_ids(store)
        .map_err(map_knowledge_error)?
        .contains(board_id);
    if exists {
        return board::load_board(store, board_id).map_err(map_knowledge_error);
    }
    let lanes = default_lanes(board_id);
    let created = Board {
        id: board_id.clone(),
        name: board_id.to_string(),
        lanes: lanes.iter().map(|lane| lane.id.clone()).collect(),
        visibility: VisibilityScope::OperatorOnly,
    };
    board::save_board(store, &created, &lanes).map_err(map_knowledge_error)?;
    Ok((created, lanes))
}

/// Löst eine `LaneRef` auf; legt `worker/<role>`-Lanes bei Bedarf an.
fn resolve_lane(
    store: &KnowledgeStore,
    board_state: &mut (Board, Vec<Lane>),
    lane_ref: &str,
) -> Result<Lane, OpError> {
    if let Some(lane) = board_state
        .1
        .iter()
        .find(|lane| lane.id.as_str() == lane_ref)
    {
        return Ok(lane.clone());
    }
    let role = lane_ref
        .strip_prefix(WORKER_LANE_PREFIX)
        .filter(|role| !role.is_empty() && !role.chars().any(|c| c.is_control() || c == '/'));
    let Some(role) = role else {
        let known: Vec<&str> = board_state.1.iter().map(|lane| lane.id.as_str()).collect();
        return Err(OpError::InvalidArguments(format!(
            "unbekannte Lane '{lane_ref}' (bekannt: {}; oder worker/<rolle>)",
            known.join(", ")
        )));
    };
    let lane = Lane {
        id: LaneId::new(lane_ref),
        board_id: board_state.0.id.clone(),
        kind: LaneKind::Worker {
            agent_role: AgentRoleRef::new(role),
        },
        bound_worker: None,
    };
    board_state.0.lanes.push(lane.id.clone());
    board_state.1.push(lane.clone());
    board::save_board(store, &board_state.0, &board_state.1).map_err(map_knowledge_error)?;
    Ok(lane)
}

fn create(
    store: &KnowledgeStore,
    jobs: Option<&dyn JobTransitions>,
    caller: &AgentId,
    board_id: &mut BoardId,
    tail: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let (lane_flag, rest) = split_flag(tail, "--lane=");
    let (assignee_flag, rest) = split_flag(&rest, "--assignee=");
    let (parent_flags, rest) = split_repeated_flag(&rest, "--parent=");
    let (tags, rest) = split_repeated_flag(&rest, "--tag=");
    let title: String = rest
        .join(" ")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned();
    if title.is_empty() {
        return Err(OpError::InvalidArguments(
            "Aufruf: /kanban create <title> [--lane=<lane>] [--assignee=<rolle>] [--parent=<card>]"
                .to_owned(),
        ));
    }
    if title.len() > MAX_TITLE_BYTES {
        return Err(OpError::InvalidArguments(format!(
            "Titel ist länger als {MAX_TITLE_BYTES} Bytes"
        )));
    }

    let worker_lane = assignee_flag.map(|role| format!("{WORKER_LANE_PREFIX}{role}"));
    let mut lane_ref = match (lane_flag, worker_lane) {
        (Some(lane), Some(worker)) if lane != worker => {
            return Err(OpError::InvalidArguments(format!(
                "--lane={lane} widerspricht --assignee (Lane {worker})"
            )));
        }
        (Some(lane), _) => lane,
        (None, Some(worker)) => worker,
        (None, None) => DEFAULT_LANE.to_owned(),
    };
    if let Some(qualified) = lane_ref.strip_prefix("board:") {
        let Some((board, lane)) = qualified.split_once('/') else {
            return Err(OpError::InvalidArguments(format!(
                "ungültige LaneRef '{lane_ref}' (board:<board>/<lane>)"
            )));
        };
        *board_id = BoardId::new(board);
        lane_ref = lane.to_owned();
    }

    let mut board_state = ensure_board(store, board_id)?;
    let lane = resolve_lane(store, &mut board_state, &lane_ref)?;

    let parents: Vec<CardId> = parent_flags
        .iter()
        .map(|raw| CardId::new(raw.trim()))
        .collect();
    for parent in &parents {
        load_record(store, board_id, parent)?;
    }

    let record = CardRecord {
        id: board::next_card_id(store, board_id).map_err(map_knowledge_error)?,
        lane_id: lane.id.clone(),
        title,
        body: String::new(),
        work_id: None,
        parents,
        tags,
        visibility: VisibilityScope::OperatorOnly,
    };
    board::save_card(store, board_id, &record, caller, now).map_err(map_knowledge_error)?;

    let is_worker_lane = matches!(lane.kind, LaneKind::Worker { .. });
    let state = match (is_worker_lane, jobs) {
        (true, Some(jobs)) => make_work(store, jobs, caller, board_id, &record, now)?,
        (true, None) => {
            return Ok(OpOutput::from(format!(
                "{} angelegt in {} (triage — kein Job-Ledger, noch keine Arbeit).",
                record.id, lane.id
            )));
        }
        (false, _) => CardState::Triage,
    };
    Ok(OpOutput::from(format!(
        "{} angelegt in {} ({}).",
        record.id,
        lane.id,
        state_text(&state)
    )))
}

/// Macht eine Karte einer Worker-Lane zu Arbeit: Job anlegen, dann `Ready`,
/// sofern alle Eltern `Done` sind.
fn make_work(
    store: &KnowledgeStore,
    jobs: &dyn JobTransitions,
    caller: &AgentId,
    board_id: &BoardId,
    record: &CardRecord,
    now: jiff::Timestamp,
) -> Result<CardState, OpError> {
    let mut card = record.view(None).map_err(map_knowledge_error)?;
    lifecycle::triage_to_todo(jobs, &mut card).map_err(map_knowledge_error)?;
    board::save_card(store, board_id, &card.record(), caller, now).map_err(map_knowledge_error)?;
    let parent_states = parent_states(store, jobs, board_id, &card)?;
    if parent_states
        .iter()
        .all(|state| matches!(state, CardState::Done))
    {
        lifecycle::todo_to_ready(jobs, &mut card, &parent_states).map_err(map_knowledge_error)?;
    }
    Ok(card.state)
}

fn parent_states(
    store: &KnowledgeStore,
    jobs: &dyn JobTransitions,
    board_id: &BoardId,
    card: &Card,
) -> Result<Vec<CardState>, OpError> {
    card.parents
        .iter()
        .map(|parent| {
            load_record(store, board_id, parent)?
                .view_with(jobs)
                .map(|parent| parent.state)
                .map_err(map_knowledge_error)
        })
        .collect()
}

// --- Übergänge ---------------------------------------------------------------

fn transition(
    store: &KnowledgeStore,
    jobs: &dyn JobTransitions,
    caller: &AgentId,
    board_id: &BoardId,
    request: &TransitionRequest,
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let record = load_record(store, board_id, &request.card)?;
    let mut card = record.view_with(jobs).map_err(map_knowledge_error)?;
    let mut record_changed = false;
    match request.action {
        Action::Todo => {
            lifecycle::triage_to_todo(jobs, &mut card).map_err(map_knowledge_error)?;
            record_changed = true;
        }
        Action::Ready => match card.state {
            CardState::Blocked { .. } => {
                lifecycle::unblock(jobs, &mut card).map_err(map_knowledge_error)?;
            }
            CardState::Running => {
                lifecycle::reclaim(jobs, &mut card).map_err(map_knowledge_error)?;
            }
            _ => {
                let states = parent_states(store, jobs, board_id, &card)?;
                lifecycle::todo_to_ready(jobs, &mut card, &states).map_err(map_knowledge_error)?;
            }
        },
        Action::Claim => {
            let worker_lane = board::list_board_ids(store)
                .map_err(map_knowledge_error)?
                .contains(board_id)
                && board::load_board(store, board_id)
                    .map_err(map_knowledge_error)?
                    .1
                    .iter()
                    .any(|lane| {
                        lane.id == card.lane_id && matches!(lane.kind, LaneKind::Worker { .. })
                    });
            let (risk, proof) = if worker_lane {
                (
                    RiskLevel::High,
                    Some(ApprovalProof::new(
                        ReviewDecision::ApprovedOnce,
                        caller.clone(),
                    )),
                )
            } else {
                (RiskLevel::Low, None)
            };
            lifecycle::claim(jobs, &mut card, caller, risk, proof.as_ref())
                .map_err(map_knowledge_error)?;
        }
        Action::Complete => lifecycle::complete(jobs, &mut card).map_err(map_knowledge_error)?,
        Action::Block(reason) => {
            lifecycle::block(jobs, &mut card, reason).map_err(map_knowledge_error)?;
        }
        Action::Unblock => lifecycle::unblock(jobs, &mut card).map_err(map_knowledge_error)?,
        Action::Archive => {
            let children = board::list_card_records(store, board_id)
                .map_err(map_knowledge_error)?
                .into_iter()
                .filter(|other| other.parents.contains(&card.id))
                .map(|child| child.view_with(jobs).map_err(map_knowledge_error))
                .collect::<Result<Vec<Card>, OpError>>()?;
            let unresolved = children.iter().filter(|child| !child.is_terminal()).count();
            lifecycle::archive(jobs, &mut card, unresolved).map_err(map_knowledge_error)?;
            record_changed = true;
        }
    }
    if record_changed {
        board::save_card(store, board_id, &card.record(), caller, now)
            .map_err(map_knowledge_error)?;
    }
    Ok(OpOutput::from(format!(
        "{} → {}",
        card.id,
        state_text(&card.state)
    )))
}

fn reason_names() -> String {
    BlockKind::ALL
        .iter()
        .map(|kind| kind.label())
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::run_kanban;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_knowledge::kanban::lifecycle::{InMemoryJobTransitions, JobTransitions};
    use harw_knowledge::{AgentId, KnowledgeStore};
    use harw_operations::operation::Surface;
    use harw_operations::{OpError, Operation};

    fn temporary_store(label: &str) -> TestResult<KnowledgeStore> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-ops-kanban-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("create temporary knowledge root"))?;
        Ok(KnowledgeStore::new(&root))
    }

    fn run(
        store: &KnowledgeStore,
        jobs: Option<&dyn JobTransitions>,
        tokens: &[&str],
    ) -> Result<String, OpError> {
        run_kanban(
            store,
            jobs,
            &AgentId::new("operator"),
            &toks(tokens),
            jiff::Timestamp::now(),
        )
        .map(|output| output.text)
    }

    #[test]
    fn kanban_is_command_only() {
        let surfaces = &super::KanbanOperation.meta().surfaces;
        assert!(
            !surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
    }

    #[test]
    fn full_lifecycle_through_the_ledger() -> TestResult {
        let store = temporary_store("lifecycle")?;
        let ledger = InMemoryJobTransitions::new();
        let jobs: Option<&dyn JobTransitions> = Some(&ledger);

        let created = run(&store, jobs, &["create", "Parser", "bauen"]).map_err(ctx("create"))?;
        assert_eq!(created, "card-1 angelegt in triage (triage).");
        assert_eq!(
            run(&store, jobs, &["todo", "card-1"]).map_err(ctx("todo"))?,
            "card-1 → todo"
        );
        assert_eq!(
            run(&store, jobs, &["ready", "card-1"]).map_err(ctx("ready"))?,
            "card-1 → ready"
        );
        assert_eq!(
            run(&store, jobs, &["claim", "card-1"]).map_err(ctx("claim"))?,
            "card-1 → running"
        );
        assert_eq!(
            run(&store, jobs, &["block", "card-1", "--reason=NeedsInput"]).map_err(ctx("block"))?,
            "card-1 → blocked (NeedsInput)"
        );
        run(&store, jobs, &["unblock", "card-1"]).map_err(ctx("unblock"))?;
        run(&store, jobs, &["claim", "card-1"]).map_err(ctx("claim again"))?;
        assert_eq!(
            run(&store, jobs, &["complete", "card-1"]).map_err(ctx("complete"))?,
            "card-1 → done"
        );
        let listed = run(&store, jobs, &[]).map_err(ctx("list"))?;
        assert!(listed.contains("done (1)"), "{listed}");
        assert_eq!(
            run(&store, jobs, &["archive", "card-1"]).map_err(ctx("archive"))?,
            "card-1 → archived"
        );
        let hidden = run(&store, jobs, &["list"]).map_err(ctx("list after archive"))?;
        assert!(!hidden.contains("card-1"), "{hidden}");
        let all = run(&store, jobs, &["list", "--all"]).map_err(ctx("list --all"))?;
        assert!(all.contains("archived (1)"), "{all}");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn tui_aliases_and_board_data() -> TestResult {
        let store = temporary_store("aliases")?;
        let ledger = InMemoryJobTransitions::new();
        let jobs: Option<&dyn JobTransitions> = Some(&ledger);
        run(&store, jobs, &["add", "Alias", "Karte"]).map_err(ctx("add"))?;
        assert_eq!(
            run(&store, jobs, &["move", "card-1", "todo"]).map_err(ctx("move todo"))?,
            "card-1 → todo"
        );
        run(&store, jobs, &["move", "card-1", "ready"]).map_err(ctx("move ready"))?;
        run(&store, jobs, &["move", "card-1", "running"]).map_err(ctx("move running"))?;
        assert_eq!(
            run(&store, jobs, &["block", "card-1", "Capability"]).map_err(ctx("block"))?,
            "card-1 → blocked (Capability)"
        );
        assert_eq!(
            run(&store, jobs, &["move", "card-1", "ready"])
                .map_err(ctx("move ready from blocked"))?,
            "card-1 → ready"
        );
        run(&store, jobs, &["claim", "card-1"]).map_err(ctx("claim"))?;
        assert_eq!(
            run(&store, jobs, &["done", "card-1"]).map_err(ctx("done"))?,
            "card-1 → done"
        );

        let output = run_kanban(
            &store,
            jobs,
            &AgentId::new("operator"),
            &toks(&["show"]),
            jiff::Timestamp::now(),
        )
        .map_err(ctx("board show"))?;
        let data = output.data.ok_or(TestError::Missing("board data"))?;
        assert_eq!(data["board"]["id"], "default");
        assert_eq!(data["lanes"][0]["kind"], "status");
        assert_eq!(data["cards"][0]["id"], "card-1");
        assert_eq!(data["cards"][0]["state"], "done");
        assert_eq!(data["cards"][0]["retry_count"], 0);
        assert!(data["cards"][0]["work_id"].is_string());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn worker_lane_cards_become_ready_work_immediately() -> TestResult {
        let store = temporary_store("worker")?;
        let ledger = InMemoryJobTransitions::new();
        let jobs: Option<&dyn JobTransitions> = Some(&ledger);
        let created = run(&store, jobs, &["create", "Fix", "--assignee=coding"])
            .map_err(ctx("create in worker lane"))?;
        assert_eq!(created, "card-1 angelegt in worker/coding (ready).");
        let shown = run(&store, jobs, &["show", "card-1"]).map_err(ctx("show"))?;
        assert!(shown.contains("WorkId: "), "{shown}");
        assert!(!shown.contains("noch keine Arbeit"), "{shown}");

        // Eine Kindkarte bleibt `todo`, solange ihr Elternteil nicht `done` ist.
        let child = run(
            &store,
            jobs,
            &["create", "Folge", "--lane=worker/coding", "--parent=card-1"],
        )
        .map_err(ctx("create child"))?;
        assert_eq!(child, "card-2 angelegt in worker/coding (todo).");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn archive_is_blocked_by_open_children() -> TestResult {
        let store = temporary_store("children")?;
        let ledger = InMemoryJobTransitions::new();
        let jobs: Option<&dyn JobTransitions> = Some(&ledger);
        run(&store, jobs, &["create", "Eltern", "--assignee=coding"]).map_err(ctx("parent"))?;
        run(&store, jobs, &["create", "Kind", "--parent=card-1"]).map_err(ctx("child"))?;
        run(&store, jobs, &["claim", "card-1"]).map_err(ctx("claim"))?;
        run(&store, jobs, &["complete", "card-1"]).map_err(ctx("complete"))?;
        match run(&store, jobs, &["archive", "card-1"]) {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("unresolved")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "archive with an open child must fail, got {other:?}"
                )));
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn without_a_ledger_transitions_are_unavailable_and_state_is_not_guessed() -> TestResult {
        let store = temporary_store("no-ledger")?;
        let ledger = InMemoryJobTransitions::new();
        run(
            &store,
            Some(&ledger),
            &["create", "Gebunden", "--assignee=coding"],
        )
        .map_err(ctx("create with ledger"))?;
        run(&store, None, &["create", "Frei"]).map_err(ctx("create without ledger"))?;

        match run(&store, None, &["claim", "card-1"]) {
            Err(OpError::NotAvailable(_)) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "a transition without a ledger must be NotAvailable, got {other:?}"
                )));
            }
        }
        let listed = run(&store, None, &[]).map_err(ctx("list without ledger"))?;
        assert!(
            listed.contains("unbekannt — kein Job-Ledger (1)"),
            "{listed}"
        );
        assert!(listed.contains("triage (1)"), "{listed}");
        let shown = run(&store, None, &["show", "card-1"]).map_err(ctx("show"))?;
        assert!(shown.contains("Zustand: unbekannt"), "{shown}");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn grammar_errors_are_invalid_arguments() -> TestResult {
        let store = temporary_store("grammar")?;
        let ledger = InMemoryJobTransitions::new();
        let jobs: Option<&dyn JobTransitions> = Some(&ledger);
        run(&store, jobs, &["create", "Karte"]).map_err(ctx("create"))?;
        for tokens in [
            vec!["frobnicate"],
            vec!["create"],
            vec!["create", "x", "--lane=nowhere"],
            vec!["create", "x", "--parent=card-9"],
            vec!["create", "x", "--lane=todo", "--assignee=coding"],
            vec!["show", "card-9"],
            vec!["claim", "card-1"],
            vec!["block", "card-1"],
            vec!["block", "card-1", "--reason=Bored"],
            vec!["move", "card-1"],
            vec!["move", "card-1", "sideways"],
        ] {
            match run(&store, jobs, &tokens) {
                Err(OpError::InvalidArguments(_)) => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?} must be InvalidArguments, got {other:?}"
                    )));
                }
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }
}
