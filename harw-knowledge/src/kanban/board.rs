//! Typed board, lane and card model plus durable persistence (§1.2, §6.1).
//!
//! # Karte = Sicht über den Job-Zustand (§6.3)
//! „The card is a view over `WorkId` state plus kanban-specific fields
//! (`title`, `body`, `parents`), never an independent source of truth that
//! could drift from the job ledger." Dieses Modul setzt genau das um:
//! - [`CardRecord`] ist das, was **gespeichert** wird: Id, Lane, `title`,
//!   `body`, `parents`, `tags`, Sichtbarkeit und die Bindung `work_id`. Kein
//!   Zustand, kein Halter, kein Retry-Zähler.
//! - [`Card`] ist die **Sicht**: ein [`CardRecord`] plus der aus dem
//!   Job-Ledger abgeleitete [`CardState`], Halter (`assignee`) und
//!   `retry_count` (= `attempts` des Jobs). Erzeugt wird sie nur über
//!   [`CardRecord::view`] aus einem [`JobSnapshot`].
//!
//! Zustandsableitung ([`CardRecord::view`]):
//! 1. Trägt die Karte das Tag [`ARCHIVED_TAG`] → `Archived` (Archivieren ist
//!    ein Board-Konzept, kein Job-Zustand; siehe [`CardState::from_job_state`]).
//! 2. Ohne `work_id` → `Triage` (noch keine Arbeit im Governance-Sinn).
//! 3. Mit `work_id` → [`CardState::from_job_state`] über den Snapshot.
//! 4. Mit `work_id`, aber ohne Snapshot → Fehler: die Karte zeigt nie einen
//!    Zustand, dem das Ledger widerspricht — auch keinen geratenen.
//!
//! # Persistenz
//! `Board` samt `Lane`s als `kanban/boards/<board-id>/board.toml`, jede Karte
//! als `KnowledgeArtifact` der Art [`ArtifactKind::KanbanCard`] unter
//! `kanban/boards/<board-id>/cards/<card-id>.md`. Die kartenspezifischen
//! Felder liegen im `frontmatter.extra`-Escape-Hatch (ein Frontmatter-Schema
//! für jede Surface, §1.2); `tags`/`visibility` in den kanonischen Feldern,
//! der Body ist reiner Freitext. Ältere Dateien mit `state`/`assignee`/
//! `retry_count` im `extra` werden gelesen (die Felder ignoriert) und beim
//! nächsten [`save_card`] bereinigt.

use serde::{Deserialize, Serialize};

use harw_job_runtime::{JobState, WorkId};

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::kanban::lifecycle::JobTransitions;
use crate::store::KnowledgeStore;
use crate::visibility::{AgentId, AgentRoleRef, VisibilityScope};

id_newtype!(BoardId);
id_newtype!(LaneId);
id_newtype!(CardId);

/// Tag, das eine Karte als archiviert markiert (Board-Konzept, §6.3 archive).
pub const ARCHIVED_TAG: &str = "archived";

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

impl CardState {
    /// Leitet den Kartenzustand aus dem Job-Zustand ab (§6.3).
    ///
    /// # Beschreibung
    /// | `JobState` | `CardState` |
    /// |---|---|
    /// | `Pending` | `Todo` |
    /// | `Ready` | `Ready` |
    /// | `Running` | `Running` |
    /// | `Completed` | `Done` |
    /// | `Blocked` | `Blocked { block_reason \|\| Dependency }` |
    /// | `Failed` | `Blocked { block_reason \|\| Transient }` |
    /// | `Cancelled` | `Archived` |
    ///
    /// `Failed` (Retries erschöpft) wird als blockiert gezeigt, weil beide
    /// Folge-Pfeile aus §6.3 passen: `unblock` (erneut versuchen) oder
    /// `archive` (aufgeben).
    ///
    /// # Argumente
    /// - `state` ([`JobState`]): Zustand laut Ledger.
    /// - `block_reason` (`Option<BlockKind>`): vom Ledger mitgeführter Grund
    ///   eines `Blocked`/`Failed`-Jobs, falls bekannt.
    #[must_use]
    pub fn from_job_state(state: JobState, block_reason: Option<BlockKind>) -> Self {
        match state {
            JobState::Pending => Self::Todo,
            JobState::Ready => Self::Ready,
            JobState::Running => Self::Running,
            JobState::Completed => Self::Done,
            JobState::Blocked => Self::Blocked {
                reason_kind: block_reason.unwrap_or(BlockKind::Dependency),
            },
            JobState::Failed => Self::Blocked {
                reason_kind: block_reason.unwrap_or(BlockKind::Transient),
            },
            JobState::Cancelled => Self::Archived,
        }
    }

    /// Kurze, stabile Bezeichnung (Spaltenname, Fehlermeldungen).
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Triage => "triage",
            Self::Todo => "todo",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Blocked { .. } => "blocked",
            Self::Done => "done",
            Self::Archived => "archived",
        }
    }
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

impl BlockKind {
    /// Alle Varianten in stabiler Reihenfolge.
    pub const ALL: [Self; 5] = [
        Self::Dependency,
        Self::NeedsInput,
        Self::Capability,
        Self::Transient,
        Self::ReviewRequired,
    ];

    /// Bezeichnung in der Kommandogrammatik (`--reason=`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dependency => "Dependency",
            Self::NeedsInput => "NeedsInput",
            Self::Capability => "Capability",
            Self::Transient => "Transient",
            Self::ReviewRequired => "ReviewRequired",
        }
    }

    /// Umkehrung von [`Self::label`], ohne Groß-/Kleinschreibung.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.label().eq_ignore_ascii_case(raw.trim()))
    }
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

/// Was von einer Karte gespeichert wird — ausschließlich Kanban-Felder plus
/// die `work_id`-Bindung, nie ein Zustand (siehe Moduldoku).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardRecord {
    pub id: CardId,
    pub lane_id: LaneId,
    pub title: String,
    pub body: String,
    /// Bindung an den Job im Ledger; `None` solange die Karte in Triage ist.
    pub work_id: Option<WorkId>,
    pub parents: Vec<CardId>,
    pub tags: Vec<String>,
    pub visibility: VisibilityScope,
}

/// Was das Job-Ledger über den gebundenen Job einer Karte weiß.
///
/// # Beschreibung
/// Wird von [`JobTransitions::snapshot`] geliefert; ein Adapter über
/// `harw-job-runtime` füllt `state`/`attempts` aus dem `Job`, `holder` aus
/// dem aktiven `Lease` und `block_reason` aus seiner eigenen Buchführung
/// (der `JobState` selbst trägt keinen Grund).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSnapshot {
    /// Zustand laut Ledger.
    pub state: JobState,
    /// Aktueller Lease-Halter, falls der Job läuft.
    pub holder: Option<AgentId>,
    /// Anzahl bisheriger Fehlversuche/Reclaims.
    pub attempts: u32,
    /// Grund eines `Blocked`/`Failed`-Jobs, falls bekannt.
    pub block_reason: Option<BlockKind>,
}

impl JobSnapshot {
    /// Snapshot eines frischen Jobs im Zustand `state`.
    #[must_use]
    pub fn new(state: JobState) -> Self {
        Self {
            state,
            holder: None,
            attempts: 0,
            block_reason: None,
        }
    }
}

impl CardRecord {
    /// `true`, wenn die Karte das Tag [`ARCHIVED_TAG`] trägt.
    #[must_use]
    pub fn is_archived(&self) -> bool {
        self.tags.iter().any(|tag| tag == ARCHIVED_TAG)
    }

    /// Baut die Sicht aus diesem Datensatz und dem Ledger-Snapshot.
    ///
    /// # Argumente
    /// - `job` (`Option<&JobSnapshot>`): Snapshot des gebundenen Jobs; muss
    ///   `Some` sein, wenn `work_id` gesetzt ist.
    ///
    /// # Fehler
    /// [`KnowledgeError::ArtifactNotFound`], wenn `work_id` gesetzt ist, das
    /// Ledger den Job aber nicht kennt — die Karte zeigt dann keinen Zustand.
    pub fn view(&self, job: Option<&JobSnapshot>) -> KnowledgeResult<Card> {
        let (state, assignee, retry_count) = match (&self.work_id, job) {
            (None, _) => (CardState::Triage, None, 0),
            (Some(_), Some(job)) => (
                CardState::from_job_state(job.state, job.block_reason),
                job.holder.clone(),
                job.attempts,
            ),
            (Some(work_id), None) => {
                return Err(KnowledgeError::ArtifactNotFound(format!(
                    "job {work_id} of card {}",
                    self.id
                )));
            }
        };
        let state = if self.is_archived() {
            CardState::Archived
        } else {
            state
        };
        Ok(Card {
            id: self.id.clone(),
            lane_id: self.lane_id.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            work_id: self.work_id.clone(),
            state,
            parents: self.parents.clone(),
            assignee,
            tags: self.tags.clone(),
            visibility: self.visibility.clone(),
            retry_count,
        })
    }

    /// Wie [`Self::view`], liest den Snapshot aber selbst über `jobs`.
    ///
    /// # Fehler
    /// Wie [`Self::view`], plus jeder Fehler aus [`JobTransitions::snapshot`].
    pub fn view_with(&self, jobs: &dyn JobTransitions) -> KnowledgeResult<Card> {
        let snapshot = match &self.work_id {
            Some(work_id) => jobs.snapshot(work_id)?,
            None => None,
        };
        self.view(snapshot.as_ref())
    }
}

/// Die Sicht auf eine Karte: gespeicherte Kanban-Felder plus der aus dem
/// Job-Ledger abgeleitete Zustand. Nie direkt persistiert — siehe
/// [`Card::record`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: CardId,
    pub lane_id: LaneId,
    pub title: String,
    pub body: String,
    pub work_id: Option<WorkId>,
    /// Abgeleitet (siehe [`CardRecord::view`]).
    pub state: CardState,
    pub parents: Vec<CardId>,
    /// Abgeleitet: aktueller Lease-Halter laut Ledger.
    pub assignee: Option<AgentId>,
    pub tags: Vec<String>,
    pub visibility: VisibilityScope,
    /// Abgeleitet: `attempts` des gebundenen Jobs (§6.3 "retry-counted").
    pub retry_count: u32,
}

impl Card {
    /// A card with unresolved parent work must not enter the ready queue.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, CardState::Done | CardState::Archived)
    }

    /// Der speicherbare Teil dieser Sicht (ohne abgeleitete Felder).
    #[must_use]
    pub fn record(&self) -> CardRecord {
        CardRecord {
            id: self.id.clone(),
            lane_id: self.lane_id.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            work_id: self.work_id.clone(),
            parents: self.parents.clone(),
            tags: self.tags.clone(),
            visibility: self.visibility.clone(),
        }
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
/// - [`KnowledgeError::Io`]\: Schreib-/Rename-Fehler.
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
pub fn load_board(
    store: &KnowledgeStore,
    board_id: &BoardId,
) -> KnowledgeResult<(Board, Vec<Lane>)> {
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

/// Frontmatter-`extra`-Schlüssel, unter denen `save_card`/`record_from_artifact`
/// die kartenspezifischen Felder ablegen bzw. wiederfinden (siehe Moduldoku).
const TITLE_KEY: &str = "title";
const LANE_ID_KEY: &str = "lane_id";
const WORK_ID_KEY: &str = "work_id";
const PARENTS_KEY: &str = "parents";

/// Abgeleitete Felder früherer Fassungen; werden beim Speichern entfernt,
/// damit keine veraltete Zweitwahrheit neben dem Ledger liegen bleibt.
const LEGACY_DERIVED_KEYS: [&str; 3] = ["state", "assignee", "retry_count"];

/// Persistiert einen [`CardRecord`] als `KanbanCard`-[`KnowledgeArtifact`]
/// unter `kanban/boards/<board_id>/cards/<record.id>.md`.
///
/// # Beschreibung
/// Read-Modify-Write wie [`crate::diary::append`]: `created_at`/Autor einer
/// bestehenden Datei bleiben erhalten; für eine neue Karte wird
/// `author_agent_id` mit `now` gesetzt. Geschrieben werden nur `title`,
/// `lane_id`, `work_id`, `parents` (im `extra`), `tags`/`visibility` (in den
/// kanonischen Feldern) und der Body — nie ein Zustand. Atomar über
/// [`KnowledgeStore::write_artifact`].
///
/// # Rückgabe
/// Das geschriebene [`KnowledgeArtifact`].
///
/// # Fehler
/// - [`KnowledgeError::Io`]: unsichere Board-/Karten-Id oder Lese-/Schreibfehler.
/// - [`KnowledgeError::Frontmatter`]/[`KnowledgeError::MalformedFrontmatter`]:
///   eine bestehende Kartendatei ließ sich nicht parsen.
///
/// # Nebenläufigkeit
/// Kein eigenes Locking; parallele Aufrufe für dieselbe Karte serialisiert
/// der Aufrufer.
pub fn save_card(
    store: &KnowledgeStore,
    board_id: &BoardId,
    record: &CardRecord,
    author_agent_id: &AgentId,
    now: jiff::Timestamp,
) -> KnowledgeResult<KnowledgeArtifact> {
    ensure_component(board_id.as_str())?;
    ensure_component(record.id.as_str())?;
    let path = store.kanban_card_path(board_id.as_str(), record.id.as_str());
    let id = kanban_card_artifact_id(board_id, &record.id);

    let mut frontmatter = if path.is_file() {
        store
            .read_artifact(&path, id.clone(), ArtifactKind::KanbanCard)?
            .frontmatter
    } else {
        Frontmatter::new(author_agent_id.clone(), record.visibility.clone(), now)
    };
    frontmatter.touch(now);
    frontmatter.visibility = record.visibility.clone();
    frontmatter.tags = record.tags.clone();
    for key in LEGACY_DERIVED_KEYS {
        frontmatter.extra.remove(key);
    }
    frontmatter.extra.insert(
        TITLE_KEY.to_owned(),
        serde_json::Value::String(record.title.clone()),
    );
    frontmatter.extra.insert(
        LANE_ID_KEY.to_owned(),
        serde_json::Value::String(record.lane_id.as_str().to_owned()),
    );
    frontmatter.extra.insert(
        WORK_ID_KEY.to_owned(),
        record
            .work_id
            .as_ref()
            .map_or(serde_json::Value::Null, |work_id| {
                serde_json::Value::String(work_id.as_str().to_owned())
            }),
    );
    frontmatter.extra.insert(
        PARENTS_KEY.to_owned(),
        serde_json::Value::Array(
            record
                .parents
                .iter()
                .map(|parent| serde_json::Value::String(parent.as_str().to_owned()))
                .collect(),
        ),
    );

    let artifact = KnowledgeArtifact::new(
        id,
        ArtifactKind::KanbanCard,
        frontmatter,
        record.body.clone(),
    );
    store.write_artifact(&path, &artifact)?;
    Ok(artifact)
}

/// Liest den gespeicherten Datensatz einer Karte (ohne Ledger-Zugriff).
///
/// # Fehler
/// - [`KnowledgeError::Io`]: unsichere Id, Datei fehlt oder ist nicht lesbar.
/// - [`KnowledgeError::Frontmatter`]/[`KnowledgeError::MalformedFrontmatter`]:
///   kaputter Fence, YAML-Fehler oder fehlendes Pflichtfeld (`title`, `lane_id`).
pub fn load_card_record(
    store: &KnowledgeStore,
    board_id: &BoardId,
    card_id: &CardId,
) -> KnowledgeResult<CardRecord> {
    ensure_component(board_id.as_str())?;
    ensure_component(card_id.as_str())?;
    let path = store.kanban_card_path(board_id.as_str(), card_id.as_str());
    let id = kanban_card_artifact_id(board_id, card_id);
    let artifact = store.read_artifact(&path, id, ArtifactKind::KanbanCard)?;
    record_from_artifact(card_id, &artifact)
}

/// Liest eine Karte als Sicht über den Ledger-Zustand.
///
/// # Fehler
/// Wie [`load_card_record`] und [`CardRecord::view_with`].
pub fn load_card(
    store: &KnowledgeStore,
    board_id: &BoardId,
    card_id: &CardId,
    jobs: &dyn JobTransitions,
) -> KnowledgeResult<Card> {
    load_card_record(store, board_id, card_id)?.view_with(jobs)
}

/// Alle gespeicherten Karten-Datensätze eines Boards, nach [`CardId`] sortiert.
///
/// Ein Board ohne `cards/`-Verzeichnis ist ein gültiger, leerer Zustand.
///
/// # Fehler
/// [`KnowledgeError::Io`] auf einen Verzeichnis-/Lesefehler sowie jeder
/// Fehler aus [`load_card_record`].
pub fn list_card_records(
    store: &KnowledgeStore,
    board_id: &BoardId,
) -> KnowledgeResult<Vec<CardRecord>> {
    ensure_component(board_id.as_str())?;
    let cards_dir = store.kanban_board_dir(board_id.as_str()).join("cards");
    if !cards_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in std::fs::read_dir(&cards_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !(path.is_file() && path.extension().is_some_and(|ext| ext == "md")) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        records.push(load_card_record(store, board_id, &CardId::new(stem))?);
    }
    records.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(records)
}

/// Alle Karten eines Boards als Sicht über den Ledger-Zustand.
///
/// # Fehler
/// Wie [`list_card_records`] und [`CardRecord::view_with`].
pub fn list_cards(
    store: &KnowledgeStore,
    board_id: &BoardId,
    jobs: &dyn JobTransitions,
) -> KnowledgeResult<Vec<Card>> {
    list_card_records(store, board_id)?
        .iter()
        .map(|record| record.view_with(jobs))
        .collect()
}

/// Die nächste freie `card-<n>`-Id eines Boards (größtes vorhandenes `n` + 1).
///
/// # Fehler
/// Wie [`list_card_records`].
pub fn next_card_id(store: &KnowledgeStore, board_id: &BoardId) -> KnowledgeResult<CardId> {
    let highest = list_card_records(store, board_id)?
        .iter()
        .filter_map(|record| record.id.as_str().strip_prefix("card-"))
        .filter_map(|suffix| suffix.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    Ok(CardId::new(format!("card-{}", highest.saturating_add(1))))
}

/// Lehnt Ids ab, die als Pfadkomponente unter dem Store-Root entkommen könnten.
fn ensure_component(component: &str) -> KnowledgeResult<()> {
    let unsafe_component = component.is_empty()
        || component == "."
        || component == ".."
        || component
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control());
    if unsafe_component {
        return Err(KnowledgeError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unsafe kanban id: {component:?}"),
        )));
    }
    Ok(())
}

/// Rekonstruiert einen [`CardRecord`] aus einem gespeicherten `KanbanCard`-Artefakt.
fn record_from_artifact(
    card_id: &CardId,
    artifact: &KnowledgeArtifact,
) -> KnowledgeResult<CardRecord> {
    let extra = &artifact.frontmatter.extra;
    let missing_field = |field: &str| KnowledgeError::MalformedFrontmatter {
        detail: format!(
            "kanban card {} missing '{field}' in frontmatter extra",
            artifact.id
        ),
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

    Ok(CardRecord {
        id: card_id.clone(),
        lane_id: LaneId::new(lane_id),
        title,
        body: artifact.body.clone(),
        work_id,
        parents,
        tags: artifact.frontmatter.tags.clone(),
        visibility: artifact.frontmatter.visibility.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::lifecycle::InMemoryJobTransitions;
    use crate::test_support::{TestError, TestResult};

    fn temporary_root(label: &str) -> TestResult<std::path::PathBuf> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(crate::test_support::ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root)
            .map_err(crate::test_support::ctx("create temporary knowledge root"))?;
        Ok(root)
    }

    fn sample_record(id: &str, lane: &str) -> CardRecord {
        CardRecord {
            id: CardId::new(id),
            lane_id: LaneId::new(lane),
            title: "Implement kanban persistence".to_owned(),
            body: "Detailed description of the work.".to_owned(),
            work_id: None,
            parents: Vec::new(),
            tags: Vec::new(),
            visibility: VisibilityScope::SelfOnly,
        }
    }

    #[test]
    fn job_states_map_onto_card_states() {
        assert_eq!(
            CardState::from_job_state(JobState::Pending, None),
            CardState::Todo
        );
        assert_eq!(
            CardState::from_job_state(JobState::Ready, None),
            CardState::Ready
        );
        assert_eq!(
            CardState::from_job_state(JobState::Running, None),
            CardState::Running
        );
        assert_eq!(
            CardState::from_job_state(JobState::Completed, None),
            CardState::Done
        );
        assert_eq!(
            CardState::from_job_state(JobState::Blocked, Some(BlockKind::NeedsInput)),
            CardState::Blocked {
                reason_kind: BlockKind::NeedsInput
            }
        );
        assert_eq!(
            CardState::from_job_state(JobState::Failed, None),
            CardState::Blocked {
                reason_kind: BlockKind::Transient
            }
        );
        assert_eq!(
            CardState::from_job_state(JobState::Cancelled, None),
            CardState::Archived
        );
    }

    #[test]
    fn a_card_without_work_is_triage_and_an_archived_tag_wins() -> TestResult {
        let mut record = sample_record("card-1", "triage");
        assert_eq!(record.view(None)?.state, CardState::Triage);
        record.tags.push(ARCHIVED_TAG.to_owned());
        assert_eq!(record.view(None)?.state, CardState::Archived);
        Ok(())
    }

    #[test]
    fn a_bound_card_without_a_ledger_entry_has_no_state() -> TestResult {
        let mut record = sample_record("card-1", "todo");
        record.work_id = Some(WorkId::from_str("work-1"));
        let Err(error) = record.view(None) else {
            return Err(TestError::Unexpected(
                "a bound card must never guess its state".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::ArtifactNotFound(_)));
        Ok(())
    }

    #[test]
    fn the_view_takes_state_holder_and_attempts_from_the_ledger() -> TestResult {
        let mut record = sample_record("card-1", "worker/coding");
        record.work_id = Some(WorkId::from_str("work-1"));
        let snapshot = JobSnapshot {
            state: JobState::Running,
            holder: Some(AgentId::new("worker-1")),
            attempts: 2,
            block_reason: None,
        };
        let card = record.view(Some(&snapshot))?;
        assert_eq!(card.state, CardState::Running);
        assert_eq!(card.assignee, Some(AgentId::new("worker-1")));
        assert_eq!(card.retry_count, 2);
        assert_eq!(card.record(), record);
        Ok(())
    }

    #[test]
    fn block_kind_labels_round_trip() {
        for kind in BlockKind::ALL {
            assert_eq!(BlockKind::parse(kind.label()), Some(kind));
        }
        assert_eq!(BlockKind::parse("needsinput"), Some(BlockKind::NeedsInput));
        assert_eq!(BlockKind::parse("nope"), None);
    }

    #[test]
    fn test_save_and_load_board_round_trips_board_and_lanes() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-board-roundtrip")?;
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

        save_board(&store, &board, &lanes).map_err(crate::test_support::ctx("save board"))?;
        let (loaded_board, loaded_lanes) =
            load_board(&store, &board_id).map_err(crate::test_support::ctx("load board"))?;

        assert_eq!(loaded_board, board);
        assert_eq!(loaded_lanes, lanes);
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_list_board_ids_returns_only_directories_with_board_toml() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-list-boards")?;
        let store = KnowledgeStore::new(&root);
        let board = Board {
            id: BoardId::new("board-a"),
            name: "A".to_owned(),
            lanes: Vec::new(),
            visibility: VisibilityScope::SelfOnly,
        };
        save_board(&store, &board, &[]).map_err(crate::test_support::ctx("save board"))?;
        std::fs::create_dir_all(
            store
                .root()
                .join("kanban")
                .join("boards")
                .join("not-a-board"),
        )
        .map_err(crate::test_support::ctx("create stray directory"))?;

        let ids = list_board_ids(&store).map_err(crate::test_support::ctx("list boards"))?;

        assert_eq!(ids, vec![BoardId::new("board-a")]);
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_list_board_ids_on_missing_root_is_empty() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-list-boards-missing")?;
        let store = KnowledgeStore::new(&root);
        let ids = list_board_ids(&store).map_err(crate::test_support::ctx(
            "missing boards dir is not an error",
        ))?;
        assert!(ids.is_empty());
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_save_and_load_record_round_trips_every_stored_field() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-card-roundtrip")?;
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let author = AgentId::new("agent-1");
        let mut record = sample_record("card-1", "worker-lane");
        record.work_id = Some(WorkId::from_str("work-1"));
        record.parents = vec![CardId::new("card-0")];
        record.tags = vec!["review-required".to_owned()];
        let now = jiff::Timestamp::from_second(1_700_000_000)?;

        let artifact = save_card(&store, &board_id, &record, &author, now)?;
        let loaded = load_card_record(&store, &board_id, &record.id)?;

        assert_eq!(loaded, record);
        for key in LEGACY_DERIVED_KEYS {
            assert!(
                !artifact.frontmatter.extra.contains_key(key),
                "{key} is derived from the ledger and must not be stored"
            );
        }
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_save_card_strips_legacy_derived_fields() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-card-legacy")?;
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let author = AgentId::new("agent-1");
        let record = sample_record("card-1", "todo");
        let now = jiff::Timestamp::from_second(1_700_000_000)?;
        let mut artifact = save_card(&store, &board_id, &record, &author, now)?;
        artifact
            .frontmatter
            .extra
            .insert("state".to_owned(), serde_json::json!("done"));
        let path = store.kanban_card_path(board_id.as_str(), record.id.as_str());
        store.write_artifact(&path, &artifact)?;

        assert_eq!(load_card_record(&store, &board_id, &record.id)?, record);
        let resaved = save_card(&store, &board_id, &record, &author, now)?;
        assert!(!resaved.frontmatter.extra.contains_key("state"));
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_save_card_preserves_created_at_across_updates() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-card-preserve-created")?;
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let author = AgentId::new("agent-1");
        let record = sample_record("card-1", "todo");
        let created = jiff::Timestamp::from_second(1_700_000_000)?;
        let updated = created.checked_add(jiff::SignedDuration::from_secs(3600))?;

        let first = save_card(&store, &board_id, &record, &author, created)?;
        let mut moved = record.clone();
        moved.title = "renamed".to_owned();
        let second = save_card(&store, &board_id, &moved, &author, updated)?;

        assert_eq!(second.frontmatter.created_at, first.frontmatter.created_at);
        assert_eq!(second.frontmatter.updated_at, updated);
        assert_eq!(second.frontmatter.author_agent_id, author);
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_list_cards_views_every_card_through_the_ledger() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-list-cards")?;
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let author = AgentId::new("agent-1");
        let now = jiff::Timestamp::now();
        let jobs = InMemoryJobTransitions::new();
        let work_id = WorkId::from_str("work-b");
        jobs.insert(work_id.clone(), JobSnapshot::new(JobState::Ready));

        let mut card_b = sample_record("card-b", "todo");
        card_b.work_id = Some(work_id);
        let card_a = sample_record("card-a", "todo");
        save_card(&store, &board_id, &card_b, &author, now)?;
        save_card(&store, &board_id, &card_a, &author, now)?;

        let cards = list_cards(&store, &board_id, &jobs)?;

        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].id, CardId::new("card-a"));
        assert_eq!(cards[0].state, CardState::Triage);
        assert_eq!(cards[1].id, CardId::new("card-b"));
        assert_eq!(cards[1].state, CardState::Ready);
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_list_card_records_on_board_with_no_cards_dir_is_empty() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-list-cards-missing")?;
        let store = KnowledgeStore::new(&root);
        let records = list_card_records(&store, &BoardId::new("board-1"))?;
        assert!(records.is_empty());
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_next_card_id_counts_past_the_highest_numbered_card() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-next-id")?;
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let author = AgentId::new("agent-1");
        let now = jiff::Timestamp::now();
        assert_eq!(next_card_id(&store, &board_id)?, CardId::new("card-1"));
        save_card(
            &store,
            &board_id,
            &sample_record("card-7", "todo"),
            &author,
            now,
        )?;
        save_card(
            &store,
            &board_id,
            &sample_record("custom", "todo"),
            &author,
            now,
        )?;
        assert_eq!(next_card_id(&store, &board_id)?, CardId::new("card-8"));
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_unsafe_ids_are_refused() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-unsafe")?;
        let store = KnowledgeStore::new(&root);
        let result = load_card_record(&store, &BoardId::new("board-1"), &CardId::new("../x"));
        assert!(matches!(result, Err(KnowledgeError::Io(_))));
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_load_record_rejects_a_frontmatter_missing_the_title_extra_key() -> TestResult {
        let root = temporary_root("harw-knowledge-kanban-card-missing-title")?;
        let store = KnowledgeStore::new(&root);
        let board_id = BoardId::new("board-1");
        let author = AgentId::new("agent-1");
        let record = sample_record("card-1", "todo");
        let now = jiff::Timestamp::now();
        let mut broken = save_card(&store, &board_id, &record, &author, now)?;
        broken.frontmatter.extra.remove(TITLE_KEY);
        store.write_artifact(
            &store.kanban_card_path(board_id.as_str(), record.id.as_str()),
            &broken,
        )?;

        let Err(error) = load_card_record(&store, &board_id, &record.id) else {
            return Err(TestError::Unexpected("missing title must error".to_owned()));
        };

        assert!(matches!(error, KnowledgeError::MalformedFrontmatter { .. }));
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }
}
