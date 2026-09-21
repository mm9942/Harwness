//! Typed board, lane, and card state, plus their durable persistence (§1.2,
//! §6.1). Lifecycle execution (the §6.3 transitions and §6.4 gates) lives in
//! [`crate::kanban::lifecycle`]; this module owns only the data shapes and
//! how they round-trip through [`crate::store::KnowledgeStore`] — `Board`
//! metadata (plus its full `Lane` records) as `kanban/boards/<board-id>/
//! board.toml`, and each `Card` as a `KnowledgeArtifact` of kind
//! [`crate::artifact::ArtifactKind::KanbanCard`] at
//! `kanban/boards/<board-id>/cards/<card-id>.md` — frontmatter for the
//! structured fields, markdown body for the card's free-form description,
//! exactly the split `diary.rs` already uses for its day files.
//!
//! # Warum die kartenspezifischen Felder in `frontmatter.extra` liegen
//! `Frontmatter` (§1.2) ist bewusst *ein* Schema für jede Surface dieser
//! Crate; ein card-eigenes Frontmatter-Schema daneben würde diese Garantie
//! brechen. Wie `diary.rs`s `entry_count`/`date` landen `title`, `lane_id`,
//! `state`, `work_id`, `parents`, `assignee` und `retry_count` deshalb im
//! `extra`-Escape-Hatch — der Body bleibt reiner Freitext (`Card::body`).

use serde::{Deserialize, Serialize};

use harw_job_runtime::WorkId;

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::store::KnowledgeStore;
use crate::visibility::{AgentId, AgentRoleRef, VisibilityScope};

id_newtype!(BoardId);
id_newtype!(LaneId);
id_newtype!(CardId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneKind {
    Status(CardState),
    Worker { agent_role: AgentRoleRef },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    Triage,
    Todo,
    Ready,
    Running,
    Blocked { reason_kind: BlockKind },
    Done,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Dependency,
    NeedsInput,
    Capability,
    Transient,
    ReviewRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    pub id: BoardId,
    pub name: String,
    pub lanes: Vec<LaneId>,
    pub visibility: VisibilityScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub id: LaneId,
    pub board_id: BoardId,
    pub kind: LaneKind,
    pub bound_worker: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: CardId,
    pub lane_id: LaneId,
    pub title: String,
    pub body: String,
    pub work_id: Option<WorkId>,
    pub state: CardState,
    pub parents: Vec<CardId>,
    pub assignee: Option<AgentId>,
    pub tags: Vec<String>,
    pub visibility: VisibilityScope,
    /// How many times [`crate::kanban::lifecycle::reclaim`] has moved this
    /// card back from `Running` to `Ready` after a dead/timed-out holder
    /// (§6.3 "reclaim: dead/timeout ... [retry-counted]"). Zero for a card
    /// that has never been reclaimed.
    pub retry_count: u32,
}

impl Card {
    /// A card with unresolved parent work must not enter the ready queue.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, CardState::Done | CardState::Archived)
    }
}

// --- Persistence (§1.2 on-disk layout, §6.1) --------------------------------

/// On-disk shape of `kanban/boards/<board-id>/board.toml`: the [`Board`] plus
/// the full [`Lane`] records it references by [`LaneId`] alone in
/// [`Board::lanes`]. §1.2 lists exactly one metadata file per board, so a
/// lane's full definition (its [`LaneKind`], its `bound_worker`) travels
/// alongside the board rather than needing a second per-lane file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BoardRecord {
    board: Board,
    lanes: Vec<Lane>,
}

/// Create or overwrite a board's `board.toml` (§1.2), including its lanes.
///
/// # Beschreibung
/// Serialisiert `board` und `lanes` gemeinsam als `BoardRecord` nach TOML und
/// schreibt sie atomar über [`crate::store::write_atomic`] — denselben
/// Temp-Write-plus-Rename-Pfad wie jede andere Surface dieser Crate. Ein
/// erneuter Aufruf für dieselbe `board_id` überschreibt die vorhandene Datei
/// vollständig (kein Merge); das Zusammenführen mit dem bisherigen Zustand
/// ist Sache des Aufrufers.
///
/// # Argumente
/// - `store` (`&KnowledgeStore`): der Wissensspeicher.
/// - `board` (`&Board`): die zu persistierenden Board-Metadaten.
/// - `lanes` (`&[Lane]`): die vollständigen Lane-Datensätze des Boards.
///
/// # Rückgabe
/// `Ok(())` nach erfolgreichem atomarem Schreiben.
///
/// # Fehler
/// - [`KnowledgeError::TomlEncode`]: `board`/`lanes` ließen sich nicht als
///   TOML kodieren.
/// - [`KnowledgeError::Io`]: Schreib-/Rename-Fehler.
///
/// # Nebenläufigkeit
/// Kein eigenes Locking; parallele `save_board`-Aufrufe für dieselbe
/// `board_id` muss der Aufrufer serialisieren (wie bei jedem anderen
/// Read-Modify-Write dieser Crate).
///
/// # Examples
/// ```rust,no_run
/// use harw_knowledge::kanban::board::{save_board, Board, BoardId};
/// use harw_knowledge::{KnowledgeStore, VisibilityScope};
///
/// let store = KnowledgeStore::new(std::path::Path::new("/tmp/knowledge"));
/// let board = Board {
///     id: BoardId::new("board-1"),
///     name: "Sprint".to_owned(),
///     lanes: Vec::new(),
///     visibility: VisibilityScope::SelfOnly,
/// };
/// save_board(&store, &board, &[])?;
/// # Ok::<(), harw_knowledge::KnowledgeError>(())
/// ```
pub fn save_board(store: &KnowledgeStore, board: &Board, lanes: &[Lane]) -> KnowledgeResult<()> {
    let record = BoardRecord {
        board: board.clone(),
        lanes: lanes.to_vec(),
    };
    let rendered = toml::to_string_pretty(&record)?;
    let path = board_toml_path(store, &board.id);
    crate::store::write_atomic(&path, &rendered)
}

/// Read a board's `board.toml` back into its [`Board`] and full [`Lane`] list.
///
/// # Beschreibung
/// Inverse von [`save_board`]. Reiner Lesezugriff; erzeugt oder verändert
/// nichts.
///
/// # Argumente
/// - `store` (`&KnowledgeStore`): der Wissensspeicher.
/// - `board_id` (`&BoardId`): das zu lesende Board.
///
/// # Rückgabe
/// `(Board, Vec<Lane>)` in der Reihenfolge, in der `save_board` sie erhalten hat.
///
/// # Fehler
/// - [`KnowledgeError::Io`]: die Datei existiert nicht oder ist nicht lesbar.
/// - [`KnowledgeError::TomlDecode`]: der Inhalt ist kein gültiges `BoardRecord`-TOML.
///
/// # Nebenläufigkeit
/// Reiner Lesezugriff, sicher aus mehreren Threads parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_knowledge::kanban::board::{load_board, BoardId};
/// use harw_knowledge::KnowledgeStore;
///
/// let store = KnowledgeStore::new(std::path::Path::new("/tmp/knowledge"));
/// let (_board, _lanes) = load_board(&store, &BoardId::new("board-1"))?;
/// # Ok::<(), harw_knowledge::KnowledgeError>(())
/// ```
pub fn load_board(store: &KnowledgeStore, board_id: &BoardId) -> KnowledgeResult<(Board, Vec<Lane>)> {
    let path = board_toml_path(store, board_id);
    let content = std::fs::read_to_string(&path)?;
    let record: BoardRecord = toml::from_str(&content)?;
    Ok((record.board, record.lanes))
}

/// List every board id that has a `board.toml` under `kanban/boards/` (§1.2).
///
/// # Beschreibung
/// Ein einziger, nicht-rekursiver Verzeichnis-Scan von
/// `<root>/kanban/boards/`; jeder unmittelbare Unterordner mit einer
/// `board.toml`-Datei zählt als Board. Ein fehlendes `kanban/boards/`-
/// Verzeichnis ist ein gültiger, leerer Zustand (kein Fehler) — dieselbe
/// Konvention wie [`crate::diary::gc`] für ein fehlendes Agenten-Verzeichnis.
///
/// # Argumente
/// - `store` (`&KnowledgeStore`): der Wissensspeicher.
///
/// # Rückgabe
/// Board-Ids in aufsteigender lexikographischer Reihenfolge.
///
/// # Fehler
/// [`KnowledgeError::Io`] auf einen Verzeichnis-Lesefehler.
///
/// # Nebenläufigkeit
/// Reiner Lesezugriff, sicher aus mehreren Threads parallel aufrufbar.
pub fn list_board_ids(store: &KnowledgeStore) -> KnowledgeResult<Vec<BoardId>> {
    let boards_dir = store.root().join("kanban").join("boards");
    if !boards_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(&boards_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if entry.path().join("board.toml").is_file() {
            ids.push(BoardId::new(name));
        }
    }
    ids.sort();
    Ok(ids)
}

/// Path to a board's `board.toml` (`kanban/boards/<board-id>/board.toml`).
fn board_toml_path(store: &KnowledgeStore, board_id: &BoardId) -> std::path::PathBuf {
    store.kanban_board_dir(board_id.as_str()).join("board.toml")
}

/// Build the stable [`ArtifactId`] of a kanban card, exactly matching the
/// convention [`crate::index::KnowledgeIndex::rebuild`] derives from the
/// on-disk path (`kanban/boards/<board-id>/cards/<card-id>.md` under prefix
/// `"kanban"` yields `kanban/<board-id>/cards/<card-id>` — see
/// [`crate::store::KnowledgeStore::context_proposal_path`]'s doc for the same
/// convention spelled out on another surface).
fn kanban_card_artifact_id(board_id: &BoardId, card_id: &CardId) -> ArtifactId {
    ArtifactId::new(format!(
        "kanban/{}/cards/{}",
        board_id.as_str(),
        card_id.as_str()
    ))
}

/// Frontmatter-`extra`-Schlüssel, unter denen `save_card`/`card_from_artifact`
/// die kartenspezifischen Felder ablegen bzw. wiederfinden (siehe Moduldoku).
const TITLE_KEY: &str = "title";
const LANE_ID_KEY: &str = "lane_id";
const STATE_KEY: &str = "state";
const WORK_ID_KEY: &str = "work_id";
const PARENTS_KEY: &str = "parents";
const ASSIGNEE_KEY: &str = "assignee";
const RETRY_COUNT_KEY: &str = "retry_count";

/// Persist `card` as a `KanbanCard` [`KnowledgeArtifact`] at
/// `kanban/boards/<board_id>/cards/<card.id>.md`.
///
/// # Beschreibung
/// Liest zunächst eine bestehende Kartendatei (falls vorhanden), um deren
/// `created_at`/`author_agent_id` zu erhalten — dieselbe Read-Modify-Write-
/// Konvention, die [`crate::diary::append`] für Tagesdateien nutzt. Für eine
/// neue Karte wird `author_agent_id` frisch mit `now` gesetzt. Die
/// kartenspezifischen Felder (`title`, `lane_id`, `state`, `work_id`,
/// `parents`, `assignee`, `retry_count`) werden in `frontmatter.extra`
/// geschrieben; `card.tags`/`card.visibility` landen in den entsprechenden
/// Frontmatter-Feldern; `card.body` wird unverändert als Markdown-Body
/// übernommen. Geschrieben wird ausschließlich über
/// [`crate::store::KnowledgeStore::write_artifact`] (atomar).
///
/// # Argumente
/// - `store` (`&KnowledgeStore`): der Wissensspeicher.
/// - `board_id` (`&BoardId`): das Board, dem die Karte gehört.
/// - `card` (`&Card`): der zu persistierende Kartenzustand.
/// - `author_agent_id` (`&AgentId`): Autor, falls die Karte neu angelegt wird
///   (bei einer bestehenden Karte bleibt der ursprüngliche Autor erhalten).
/// - `now` (`jiff::Timestamp`): Zeitstempel für `created_at`/`updated_at`.
///
/// # Rückgabe
/// Das vollständige, geschriebene [`KnowledgeArtifact`].
///
/// # Fehler
/// - [`KnowledgeError::Io`]: Lese-/Schreib-/Rename-Fehler.
/// - [`KnowledgeError::Frontmatter`]/[`KnowledgeError::MalformedFrontmatter`]:
///   eine bestehende Kartendatei ließ sich nicht parsen.
/// - [`KnowledgeError::Json`]: `card.state` ließ sich nicht als JSON kodieren
///   (bei den vorhandenen `CardState`-Varianten praktisch ausgeschlossen).
///
/// # Nebenläufigkeit
/// Kein eigenes Locking; parallele `save_card`-Aufrufe für dieselbe Karte
/// muss der Aufrufer serialisieren.
pub fn save_card(
    store: &KnowledgeStore,
    board_id: &BoardId,
    card: &Card,
    author_agent_id: &AgentId,
    now: jiff::Timestamp,
) -> KnowledgeResult<KnowledgeArtifact> {
    let path = store.kanban_card_path(board_id.as_str(), card.id.as_str());
    let id = kanban_card_artifact_id(board_id, &card.id);

    let mut frontmatter = if path.is_file() {
        store
            .read_artifact(&path, id.clone(), ArtifactKind::KanbanCard)?
            .frontmatter
    } else {
        Frontmatter::new(author_agent_id.clone(), card.visibility.clone(), now)
    };
    frontmatter.touch(now);
    frontmatter.visibility = card.visibility.clone();
    frontmatter.tags = card.tags.clone();
    frontmatter.extra.insert(
        TITLE_KEY.to_owned(),
        serde_json::Value::String(card.title.clone()),
    );
    frontmatter.extra.insert(
        LANE_ID_KEY.to_owned(),
        serde_json::Value::String(card.lane_id.as_str().to_owned()),
    );
    frontmatter
        .extra
        .insert(STATE_KEY.to_owned(), serde_json::to_value(&card.state)?);
    frontmatter.extra.insert(
        WORK_ID_KEY.to_owned(),
        card.work_id.as_ref().map_or(serde_json::Value::Null, |work_id| {
            serde_json::Value::String(work_id.as_str().to_owned())
        }),
    );
    frontmatter.extra.insert(
        PARENTS_KEY.to_owned(),
        serde_json::Value::Array(
            card.parents
                .iter()
                .map(|parent| serde_json::Value::String(parent.as_str().to_owned()))
                .collect(),
        ),
    );
    frontmatter.extra.insert(
        ASSIGNEE_KEY.to_owned(),
        card.assignee.as_ref().map_or(serde_json::Value::Null, |assignee| {
            serde_json::Value::String(assignee.as_str().to_owned())
        }),
    );
    frontmatter.extra.insert(
        RETRY_COUNT_KEY.to_owned(),
        serde_json::Value::from(card.retry_count),
    );

    let artifact = KnowledgeArtifact::new(id, ArtifactKind::KanbanCard, frontmatter, card.body.clone());
    store.write_artifact(&path, &artifact)?;
    Ok(artifact)
}

/// Read one card back from `kanban/boards/<board_id>/cards/<card_id>.md`.
///
/// # Fehler
/// - [`KnowledgeError::Io`]: die Datei existiert nicht oder ist nicht lesbar.
/// - [`KnowledgeError::Frontmatter`]/[`KnowledgeError::MalformedFrontmatter`]:
///   ein kaputter Fence bzw. YAML-Fehler.
/// - [`KnowledgeError::MalformedFrontmatter`]: ein von [`save_card`]
///   geschriebenes `extra`-Feld fehlt oder hat den falschen Typ.
/// - [`KnowledgeError::Json`]: `state` ließ sich nicht aus JSON dekodieren.
pub fn load_card(store: &KnowledgeStore, board_id: &BoardId, card_id: &CardId) -> KnowledgeResult<Card> {
    let path = store.kanban_card_path(board_id.as_str(), card_id.as_str());
    let id = kanban_card_artifact_id(board_id, card_id);
    let artifact = store.read_artifact(&path, id, ArtifactKind::KanbanCard)?;
    card_from_artifact(card_id, &artifact)
}

/// List every card of a board, sorted by [`CardId`].
///
/// A board with no `cards/` directory yet is a valid, empty result rather
/// than an error (the board may simply have no cards yet).
///
/// # Fehler
/// [`KnowledgeError::Io`] auf einen Verzeichnis-/Lesefehler, sowie jeder
/// Fehler, den [`load_card`] für eine einzelne Kartendatei liefern kann.
pub fn list_cards(store: &KnowledgeStore, board_id: &BoardId) -> KnowledgeResult<Vec<Card>> {
    let cards_dir = store.kanban_board_dir(board_id.as_str()).join("cards");
    if !cards_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut cards = Vec::new();
    for entry in std::fs::read_dir(&cards_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !(path.is_file() && path.extension().is_some_and(|ext| ext == "md")) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        cards.push(load_card(store, board_id, &CardId::new(stem))?);
    }
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(cards)
}

/// Reconstruct a [`Card`] from a stored `KanbanCard` artifact (inverse of the
/// `frontmatter.extra` half of [`save_card`]).
fn card_from_artifact(card_id: &CardId, artifact: &KnowledgeArtifact) -> KnowledgeResult<Card> {
    let extra = &artifact.frontmatter.extra;
    let missing_field = |field: &str| KnowledgeError::MalformedFrontmatter {
        detail: format!("kanban card {} missing '{field}' in frontmatter extra", artifact.id),
    };

    let title = extra
        .get(TITLE_KEY)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| missing_field(TITLE_KEY))?
        .to_owned();
    let lane_id = extra
        .get(LANE_ID_KEY)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| missing_field(LANE_ID_KEY))?;
    let state_value = extra.get(STATE_KEY).ok_or_else(|| missing_field(STATE_KEY))?;
    let state: CardState = serde_json::from_value(state_value.clone())?;
    let work_id = extra
        .get(WORK_ID_KEY)
        .and_then(serde_json::Value::as_str)
        .map(WorkId::from_str);
    let parents = extra
        .get(PARENTS_KEY)
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(CardId::new)
                .collect()
        })
        .unwrap_or_default();
    let assignee = extra
        .get(ASSIGNEE_KEY)
        .and_then(serde_json::Value::as_str)
        .map(AgentId::new);
    let retry_count = extra
        .get(RETRY_COUNT_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .try_into()
        .unwrap_or(u32::MAX);

    Ok(Card {
        id: card_id.clone(),
        lane_id: LaneId::new(lane_id),
        title,
        body: artifact.body.clone(),
        work_id,
        state,
        parents,
        assignee,
        tags: artifact.frontmatter.tags.clone(),
        visibility: artifact.frontmatter.visibility.clone(),
        retry_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_cards_are_done_or_archived_only() {
        let card = Card {
            id: CardId::new("card-1"),
            lane_id: LaneId::new("done"),
            title: "verify".to_owned(),
            body: String::new(),
            work_id: None,
            state: CardState::Done,
            parents: Vec::new(),
            assignee: None,
            tags: Vec::new(),
            visibility: VisibilityScope::SelfOnly,
            retry_count: 0,
        };
        assert!(card.is_terminal());
    }

    fn temporary_root(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temporary knowledge root");
        root
    }

    fn sample_card(id: &str, lane: &str, state: CardState) -> Card {
        Card {
            id: CardId::new(id),
            lane_id: LaneId::new(lane),
            title: "Implement kanban persistence".to_owned(),
            body: "Detailed description of the work.".to_owned(),
            work_id: None,
            state,
            parents: Vec::new(),
            assignee: None,
            tags: Vec::new(),
            visibility: VisibilityScope::SelfOnly,
            retry_count: 0,
        }
    }

    #[test]
    fn test_save_and_load_board_round_trips_board_and_lanes() {
        let root = temporary_root("harw-knowledge-kanban-board-roundtrip");
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let lane_id = LaneId::new("triage");
        let board = Board {
            id: board_id.clone(),
            name: "Sprint 1".to_owned(),
            lanes: vec![lane_id.clone()],
            visibility: VisibilityScope::SelfOnly,
        };
        let lanes = vec![Lane {
            id: lane_id.clone(),
            board_id: board_id.clone(),
            kind: LaneKind::Status(CardState::Triage),
            bound_worker: None,
        }];

        save_board(&store, &board, &lanes).expect("save board");
        let (loaded_board, loaded_lanes) = load_board(&store, &board_id).expect("load board");

        assert_eq!(loaded_board, board);
        assert_eq!(loaded_lanes, lanes);
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_list_board_ids_returns_only_directories_with_board_toml() {
        let root = temporary_root("harw-knowledge-kanban-list-boards");
        let store = KnowledgeStore::new(&root);
        let board = Board {
            id: BoardId::new("board-a"),
            name: "A".to_owned(),
            lanes: Vec::new(),
            visibility: VisibilityScope::SelfOnly,
        };
        save_board(&store, &board, &[]).expect("save board");
        // A stray directory without a board.toml must not appear.
        std::fs::create_dir_all(store.root().join("kanban").join("boards").join("not-a-board"))
            .expect("create stray directory");

        let ids = list_board_ids(&store).expect("list boards");

        assert_eq!(ids, vec![BoardId::new("board-a")]);
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_list_board_ids_on_missing_root_is_empty() {
        let root = temporary_root("harw-knowledge-kanban-list-boards-missing");
        let store = KnowledgeStore::new(&root);

        let ids = list_board_ids(&store).expect("missing boards dir is not an error");

        assert!(ids.is_empty());
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_save_and_load_card_round_trips_every_field() {
        let root = temporary_root("harw-knowledge-kanban-card-roundtrip");
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let author = AgentId::new("agent-1");
        let mut card = sample_card("card-1", "worker-lane", CardState::Running);
        card.work_id = Some(WorkId::from_str("work-1"));
        card.parents = vec![CardId::new("card-0")];
        card.assignee = Some(AgentId::new("agent-2"));
        card.tags = vec!["review-required".to_owned()];
        card.retry_count = 2;
        let now = jiff::Timestamp::from_second(1_700_000_000).expect("valid timestamp");

        save_card(&store, &board_id, &card, &author, now).expect("save card");
        let loaded = load_card(&store, &board_id, &card.id).expect("load card");

        assert_eq!(loaded, card);
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_save_card_preserves_created_at_across_updates() {
        let root = temporary_root("harw-knowledge-kanban-card-preserve-created");
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let author = AgentId::new("agent-1");
        let card = sample_card("card-1", "todo", CardState::Todo);
        let created = jiff::Timestamp::from_second(1_700_000_000).expect("valid timestamp");
        let updated = created
            .checked_add(jiff::SignedDuration::from_secs(3600))
            .expect("valid later timestamp");

        let first = save_card(&store, &board_id, &card, &author, created).expect("first save");
        let mut moved = card.clone();
        moved.state = CardState::Ready;
        let second = save_card(&store, &board_id, &moved, &author, updated).expect("second save");

        assert_eq!(second.frontmatter.created_at, first.frontmatter.created_at);
        assert_eq!(second.frontmatter.updated_at, updated);
        assert_eq!(second.frontmatter.author_agent_id, author);
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_list_cards_returns_all_cards_sorted_by_id() {
        let root = temporary_root("harw-knowledge-kanban-list-cards");
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let author = AgentId::new("agent-1");
        let now = jiff::Timestamp::now();
        let card_b = sample_card("card-b", "todo", CardState::Todo);
        let card_a = sample_card("card-a", "todo", CardState::Todo);
        save_card(&store, &board_id, &card_b, &author, now).expect("save card b");
        save_card(&store, &board_id, &card_a, &author, now).expect("save card a");

        let cards = list_cards(&store, &board_id).expect("list cards");

        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].id, CardId::new("card-a"));
        assert_eq!(cards[1].id, CardId::new("card-b"));
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_list_cards_on_board_with_no_cards_dir_is_empty() {
        let root = temporary_root("harw-knowledge-kanban-list-cards-missing");
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");

        let cards = list_cards(&store, &board_id).expect("missing cards dir is not an error");

        assert!(cards.is_empty());
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_load_card_rejects_a_frontmatter_missing_the_state_extra_key() {
        let root = temporary_root("harw-knowledge-kanban-card-missing-state");
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let author = AgentId::new("agent-1");
        let card = sample_card("card-1", "todo", CardState::Todo);
        let now = jiff::Timestamp::now();
        let artifact = save_card(&store, &board_id, &card, &author, now).expect("save card");
        let mut broken = artifact;
        broken.frontmatter.extra.remove(STATE_KEY);
        store
            .write_artifact(&store.kanban_card_path(board_id.as_str(), card.id.as_str()), &broken)
            .expect("overwrite with broken frontmatter");

        let error = load_card(&store, &board_id, &card.id).expect_err("missing state must error");

        assert!(matches!(error, KnowledgeError::MalformedFrontmatter { .. }));
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }
}
