//! Anmerkungen einer Kanban-Karte: Kommentare, Belegverweise, Verlauf und
//! Ergebnis (Plan D2).
//!
//! # Beschreibung
//! Die Karte bleibt eine Sicht über das Job-Ledger (siehe
//! [`crate::kanban::board`]): hier liegt **kein** Zustand, nur was Menschen
//! und der Kanban-Worker an die Karte heften.
//! - `comments` — Kommentare mit Zeit und Autor (`/kanban comment`).
//! - `evidence` — Belegverweise (Pfade/URLs, `/kanban evidence`).
//! - `history` — Verlauf: Freigabe angefragt/erteilt/abgelehnt, Start,
//!   Abschluss, Fehlschlag, Bearbeitung (Vertrag: „Verlauf in `show`").
//! - `result` — der Abschnitt „Ergebnis" des letzten Worker-Laufs.
//!
//! Alles liegt im `frontmatter.extra` der Kartendatei unter den Schlüsseln
//! [`COMMENTS_KEY`], [`EVIDENCE_KEY`], [`HISTORY_KEY`] und [`RESULT_KEY`].
//! [`crate::kanban::board::save_card`] liest das bestehende Frontmatter und
//! lässt diese Schlüssel unangetastet.
//!
//! # Deckel
//! Höchstens [`MAX_COMMENTS`] Kommentare und [`MAX_HISTORY`]
//! Verlaufseinträge (die ältesten fallen weg), [`MAX_EVIDENCE`] Belege; Texte
//! werden auf [`MAX_COMMENT_BYTES`] bzw. [`MAX_RESULT_BYTES`] gekürzt.
//!
//! # Nebenläufigkeit
//! [`update_card`] führt Lesen, Ändern und Schreiben unter derselben
//! prozessübergreifenden [`KnowledgeLock`] wie `save_card` aus
//! (`cards/.<card-id>.md.lock`). Nicht wiedereintrittsfähig: innerhalb der
//! Änderungsfunktion darf `save_card` für dieselbe Karte nicht aufgerufen
//! werden.
//!
//! # Fehler
//! Ungültige Eingaben (leerer Text, Steuerzeichen im Beleg, volle
//! Belegliste) sind [`KnowledgeError::Io`] mit `InvalidInput`; ein nicht
//! lesbarer Anmerkungsblock ist [`KnowledgeError::MalformedFrontmatter`]
//! (fail-closed: nie still verwerfen und überschreiben).

use serde::{Deserialize, Serialize};

use harw_job_runtime::WorkId;

use crate::artifact::{ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::kanban::board::{
    self, BoardId, CardId, CardRecord, apply_record, ensure_component, kanban_card_artifact_id,
    record_from_artifact,
};
use crate::lock::KnowledgeLock;
use crate::store::KnowledgeStore;

/// `extra`-Schlüssel der Kommentare.
pub const COMMENTS_KEY: &str = "comments";
/// `extra`-Schlüssel der Belegverweise.
pub const EVIDENCE_KEY: &str = "evidence";
/// `extra`-Schlüssel des Verlaufs.
pub const HISTORY_KEY: &str = "history";
/// `extra`-Schlüssel des Ergebnisses.
pub const RESULT_KEY: &str = "result";

/// Höchstzahl gespeicherter Kommentare.
pub const MAX_COMMENTS: usize = 200;
/// Höchstzahl gespeicherter Verlaufseinträge.
pub const MAX_HISTORY: usize = 200;
/// Höchstzahl der Belegverweise.
pub const MAX_EVIDENCE: usize = 50;
/// Obergrenze eines Kommentars in Bytes.
pub const MAX_COMMENT_BYTES: usize = 4 * 1024;
/// Obergrenze eines Belegverweises in Bytes.
pub const MAX_EVIDENCE_BYTES: usize = 1024;
/// Obergrenze eines Verlaufsdetails in Bytes.
pub const MAX_HISTORY_DETAIL_BYTES: usize = 1024;
/// Obergrenze der Ergebniszusammenfassung in Bytes.
pub const MAX_RESULT_BYTES: usize = 16 * 1024;
/// Obergrenze des Kartentexts (`body`) in Bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Ein Kommentar an einer Karte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardComment {
    /// Zeitpunkt.
    pub at: jiff::Timestamp,
    /// Autor (Principal-Id bzw. `operator`).
    pub author: String,
    /// Text (gekürzt auf [`MAX_COMMENT_BYTES`]).
    pub text: String,
}

/// Art eines Verlaufseintrags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryEvent {
    /// Der Worker hat eine Freigabe angefragt.
    ApprovalRequested,
    /// Der Operator hat freigegeben.
    Approved,
    /// Der Operator hat abgelehnt.
    Rejected,
    /// Der Rollen-Agent wurde gestartet.
    Started,
    /// Der Lauf endete erfolgreich.
    Completed,
    /// Der Lauf endete mit einem Teilergebnis (Budget erschöpft).
    PartiallyCompleted,
    /// Der Lauf ist fehlgeschlagen.
    Failed,
    /// Der Lauf wurde blockiert (z. B. Review nötig).
    Blocked,
    /// Der Kartentext wurde bearbeitet.
    Edited,
}

impl HistoryEvent {
    /// Deutsche Bezeichnung für `show`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ApprovalRequested => "Freigabe angefragt",
            Self::Approved => "freigegeben",
            Self::Rejected => "abgelehnt",
            Self::Started => "gestartet",
            Self::Completed => "abgeschlossen",
            Self::PartiallyCompleted => "teilweise erledigt",
            Self::Failed => "fehlgeschlagen",
            Self::Blocked => "blockiert",
            Self::Edited => "bearbeitet",
        }
    }
}

/// Ein Verlaufseintrag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Zeitpunkt.
    pub at: jiff::Timestamp,
    /// Art.
    pub event: HistoryEvent,
    /// Wer (Operator-Id, Worker-Id).
    pub actor: String,
    /// Freitext (gekürzt auf [`MAX_HISTORY_DETAIL_BYTES`]).
    #[serde(default)]
    pub detail: String,
    /// Betroffener Job, falls bekannt.
    #[serde(default)]
    pub work_id: Option<String>,
}

impl HistoryEntry {
    /// Baut einen Eintrag; `detail` wird gekürzt.
    #[must_use]
    pub fn new(
        at: jiff::Timestamp,
        event: HistoryEvent,
        actor: impl Into<String>,
        detail: impl AsRef<str>,
        work_id: Option<&WorkId>,
    ) -> Self {
        Self {
            at,
            event,
            actor: actor.into(),
            detail: clip(detail.as_ref(), MAX_HISTORY_DETAIL_BYTES),
            work_id: work_id.map(|work_id| work_id.as_str().to_owned()),
        }
    }
}

/// Ausgang des letzten Worker-Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    /// Vollständig erledigt.
    Succeeded,
    /// Teilergebnis: das Budget war erschöpft (`budget_exhausted`).
    Partial,
    /// Fehlgeschlagen.
    Failed,
}

impl ResultStatus {
    /// Stabile Kurzbezeichnung (JSON, TUI).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }

    /// Deutsche Bezeichnung für `show`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "erledigt",
            Self::Partial => "teilweise erledigt",
            Self::Failed => "fehlgeschlagen",
        }
    }
}

/// Der Abschnitt „Ergebnis" einer Karte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardResult {
    /// Zeitpunkt.
    pub at: jiff::Timestamp,
    /// Ausgang.
    pub status: ResultStatus,
    /// Zusammenfassung bzw. Fehlergrund (gekürzt auf [`MAX_RESULT_BYTES`]).
    pub summary: String,
    /// Rolle, die gearbeitet hat.
    #[serde(default)]
    pub role: Option<String>,
    /// Job des Laufs.
    #[serde(default)]
    pub work_id: Option<String>,
}

impl CardResult {
    /// Baut ein Ergebnis; `summary` wird gekürzt.
    #[must_use]
    pub fn new(
        at: jiff::Timestamp,
        status: ResultStatus,
        summary: impl AsRef<str>,
        role: Option<String>,
        work_id: Option<&WorkId>,
    ) -> Self {
        Self {
            at,
            status,
            summary: clip(summary.as_ref(), MAX_RESULT_BYTES),
            role,
            work_id: work_id.map(|work_id| work_id.as_str().to_owned()),
        }
    }
}

/// Alle Anmerkungen einer Karte.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardNotes {
    /// Kommentare, älteste zuerst.
    pub comments: Vec<CardComment>,
    /// Belegverweise in Einfügereihenfolge, ohne Duplikate.
    pub evidence: Vec<String>,
    /// Verlauf, älteste zuerst.
    pub history: Vec<HistoryEntry>,
    /// Ergebnis des letzten Worker-Laufs.
    pub result: Option<CardResult>,
}

impl CardNotes {
    /// Hängt einen Kommentar an (älteste fallen über [`MAX_COMMENTS`] weg).
    ///
    /// # Fehler
    /// `InvalidInput` bei leerem Text oder leerem Autor.
    pub fn add_comment(
        &mut self,
        at: jiff::Timestamp,
        author: &str,
        text: &str,
    ) -> KnowledgeResult<()> {
        let text = text.trim();
        if text.is_empty() || author.trim().is_empty() {
            return Err(invalid("Kommentar und Autor dürfen nicht leer sein"));
        }
        self.comments.push(CardComment {
            at,
            author: author.trim().to_owned(),
            text: clip(text, MAX_COMMENT_BYTES),
        });
        let overflow = self.comments.len().saturating_sub(MAX_COMMENTS);
        self.comments.drain(..overflow);
        Ok(())
    }

    /// Fügt einen Belegverweis hinzu.
    ///
    /// # Rückgabe
    /// `true`, wenn er neu war; `false`, wenn er schon vorhanden war.
    ///
    /// # Fehler
    /// `InvalidInput` bei leerem, zu langem oder Steuerzeichen enthaltendem
    /// Verweis sowie bei voller Liste ([`MAX_EVIDENCE`]).
    pub fn add_evidence(&mut self, reference: &str) -> KnowledgeResult<bool> {
        let reference = reference.trim();
        if reference.is_empty() || reference.chars().any(char::is_control) {
            return Err(invalid(
                "Belegverweis muss ein nicht leerer Pfad oder eine URL ohne Steuerzeichen sein",
            ));
        }
        if reference.len() > MAX_EVIDENCE_BYTES {
            return Err(invalid(&format!(
                "Belegverweis ist länger als {MAX_EVIDENCE_BYTES} Bytes"
            )));
        }
        if self.evidence.iter().any(|known| known == reference) {
            return Ok(false);
        }
        if self.evidence.len() >= MAX_EVIDENCE {
            return Err(invalid(&format!(
                "die Karte trägt bereits {MAX_EVIDENCE} Belegverweise"
            )));
        }
        self.evidence.push(reference.to_owned());
        Ok(true)
    }

    /// Hängt einen Verlaufseintrag an (älteste fallen über [`MAX_HISTORY`] weg).
    pub fn record(&mut self, entry: HistoryEntry) {
        self.history.push(entry);
        let overflow = self.history.len().saturating_sub(MAX_HISTORY);
        self.history.drain(..overflow);
    }

    /// Liest die Anmerkungen aus einem Frontmatter.
    ///
    /// # Fehler
    /// [`KnowledgeError::MalformedFrontmatter`], wenn ein vorhandener Schlüssel
    /// nicht dekodierbar ist.
    pub fn from_frontmatter(frontmatter: &Frontmatter) -> KnowledgeResult<Self> {
        let extra = &frontmatter.extra;
        Ok(Self {
            comments: decode(extra.get(COMMENTS_KEY), COMMENTS_KEY)?.unwrap_or_default(),
            evidence: decode(extra.get(EVIDENCE_KEY), EVIDENCE_KEY)?.unwrap_or_default(),
            history: decode(extra.get(HISTORY_KEY), HISTORY_KEY)?.unwrap_or_default(),
            result: decode(extra.get(RESULT_KEY), RESULT_KEY)?,
        })
    }

    /// Schreibt die Anmerkungen in ein Frontmatter; leere Teile entfallen.
    ///
    /// # Fehler
    /// [`KnowledgeError::MalformedFrontmatter`] bei einem Kodierfehler.
    pub fn write_into(&self, frontmatter: &mut Frontmatter) -> KnowledgeResult<()> {
        let extra = &mut frontmatter.extra;
        put(
            extra,
            COMMENTS_KEY,
            &self.comments,
            self.comments.is_empty(),
        )?;
        put(
            extra,
            EVIDENCE_KEY,
            &self.evidence,
            self.evidence.is_empty(),
        )?;
        put(extra, HISTORY_KEY, &self.history, self.history.is_empty())?;
        put(extra, RESULT_KEY, &self.result, self.result.is_none())?;
        Ok(())
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    value: Option<&serde_json::Value>,
    key: &str,
) -> KnowledgeResult<Option<T>> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| KnowledgeError::MalformedFrontmatter {
                detail: format!("kanban card '{key}' is not readable: {error}"),
            }),
    }
}

fn put<T: Serialize>(
    extra: &mut std::collections::BTreeMap<String, serde_json::Value>,
    key: &str,
    value: &T,
    empty: bool,
) -> KnowledgeResult<()> {
    if empty {
        extra.remove(key);
        return Ok(());
    }
    let encoded =
        serde_json::to_value(value).map_err(|error| KnowledgeError::MalformedFrontmatter {
            detail: format!("kanban card '{key}' is not encodable: {error}"),
        })?;
    extra.insert(key.to_owned(), encoded);
    Ok(())
}

/// Liest die Anmerkungen einer Karte (ohne Sperre, reiner Lesezugriff).
///
/// # Fehler
/// Wie [`board::load_card_record`] plus [`CardNotes::from_frontmatter`].
pub fn load_notes(
    store: &KnowledgeStore,
    board_id: &BoardId,
    card_id: &CardId,
) -> KnowledgeResult<CardNotes> {
    ensure_component(board_id.as_str())?;
    ensure_component(card_id.as_str())?;
    let path = store.kanban_card_path(board_id.as_str(), card_id.as_str());
    let artifact = store.read_artifact(
        &path,
        kanban_card_artifact_id(board_id, card_id),
        ArtifactKind::KanbanCard,
    )?;
    CardNotes::from_frontmatter(&artifact.frontmatter)
}

/// Ändert gespeicherten Datensatz und Anmerkungen einer **bestehenden**
/// Karte in einem Zug unter der Kartensperre.
///
/// # Beschreibung
/// Liest die Kartendatei, übergibt Datensatz und Anmerkungen an `change` und
/// schreibt beides atomar zurück (`updated_at = now`). Gibt `change` einen
/// Fehler zurück, wird nichts geschrieben.
///
/// # Rückgabe
/// Der Rückgabewert von `change`.
///
/// # Fehler
/// - [`KnowledgeError::Io`] (`NotFound`), wenn die Karte nicht existiert;
///   `InvalidInput` für unsichere Ids; `TimedOut` bei belegter Sperre.
/// - Jeder Fehler aus `change` und aus dem Lesen/Schreiben.
pub fn update_card<T>(
    store: &KnowledgeStore,
    board_id: &BoardId,
    card_id: &CardId,
    now: jiff::Timestamp,
    change: impl FnOnce(&mut CardRecord, &mut CardNotes) -> KnowledgeResult<T>,
) -> KnowledgeResult<T> {
    ensure_component(board_id.as_str())?;
    ensure_component(card_id.as_str())?;
    let path = store.kanban_card_path(board_id.as_str(), card_id.as_str());
    let id = kanban_card_artifact_id(board_id, card_id);
    let _lock = KnowledgeLock::for_target(&path)?;
    let artifact = store.read_artifact(&path, id.clone(), ArtifactKind::KanbanCard)?;
    let mut record = record_from_artifact(card_id, &artifact)?;
    let mut notes = CardNotes::from_frontmatter(&artifact.frontmatter)?;
    let value = change(&mut record, &mut notes)?;
    if record.body.len() > MAX_BODY_BYTES {
        return Err(invalid(&format!(
            "Kartentext ist länger als {MAX_BODY_BYTES} Bytes"
        )));
    }
    let mut frontmatter = artifact.frontmatter;
    apply_record(&mut frontmatter, &record, now);
    notes.write_into(&mut frontmatter)?;
    let updated = KnowledgeArtifact::new(id, ArtifactKind::KanbanCard, frontmatter, record.body);
    store.write_artifact(&path, &updated)?;
    Ok(value)
}

/// Sucht die Karte, die an `work_id` gebunden ist, über alle Boards.
///
/// # Rückgabe
/// `Some((board, record))` für die erste Karte (Boards und Karten
/// lexikographisch), sonst `None`.
///
/// # Fehler
/// Verzeichnis- oder Lesefehler aus [`board::list_board_ids`] und
/// [`board::list_card_records`].
pub fn find_card_by_work_id(
    store: &KnowledgeStore,
    work_id: &WorkId,
) -> KnowledgeResult<Option<(BoardId, CardRecord)>> {
    for board_id in board::list_board_ids(store)? {
        let found = board::list_card_records(store, &board_id)?
            .into_iter()
            .find(|record| record.work_id.as_ref() == Some(work_id));
        if let Some(record) = found {
            return Ok(Some((board_id, record)));
        }
    }
    Ok(None)
}

/// Kürzt `text` an einer Zeichengrenze auf höchstens `max` Bytes (mit `…`).
#[must_use]
pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn invalid(message: &str) -> KnowledgeError {
    KnowledgeError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        message.to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::board::{LaneId, save_card};
    use crate::test_support::{TestResult, ctx};
    use crate::visibility::{AgentId, VisibilityScope};

    fn temporary_store(label: &str) -> TestResult<KnowledgeStore> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("clock"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-kanban-notes-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("create root"))?;
        Ok(KnowledgeStore::new(&root))
    }

    fn record(id: &str) -> CardRecord {
        CardRecord {
            id: CardId::new(id),
            lane_id: LaneId::new("worker/explorer"),
            title: "Parser prüfen".to_owned(),
            body: String::new(),
            work_id: Some(WorkId::from_str("work-7")),
            parents: Vec::new(),
            tags: vec!["x".to_owned()],
            visibility: VisibilityScope::OperatorOnly,
        }
    }

    #[test]
    fn notes_round_trip_and_survive_save_card() -> TestResult {
        let store = temporary_store("roundtrip")?;
        let board_id = BoardId::new("default");
        let author = AgentId::new("operator");
        let now = jiff::Timestamp::from_second(1_700_000_000)?;
        save_card(&store, &board_id, &record("card-1"), &author, now)?;

        update_card(
            &store,
            &board_id,
            &CardId::new("card-1"),
            now,
            |record, notes| {
                record.body = "Neuer Text".to_owned();
                notes.add_comment(now, "operator", "  erster Kommentar ")?;
                assert!(notes.add_evidence("docs/a.md")?);
                assert!(!notes.add_evidence("docs/a.md")?);
                notes.record(HistoryEntry::new(
                    now,
                    HistoryEvent::ApprovalRequested,
                    "worker",
                    "Rolle explorer, Risiko low",
                    None,
                ));
                notes.result = Some(CardResult::new(
                    now,
                    ResultStatus::Partial,
                    "halb fertig",
                    Some("explorer".to_owned()),
                    None,
                ));
                Ok(())
            },
        )?;

        // Ein späteres save_card (z. B. archive) lässt die Anmerkungen stehen.
        let mut loaded = board::load_card_record(&store, &board_id, &CardId::new("card-1"))?;
        assert_eq!(loaded.body, "Neuer Text");
        loaded.tags.push("archived".to_owned());
        save_card(&store, &board_id, &loaded, &author, now)?;

        let notes = load_notes(&store, &board_id, &CardId::new("card-1"))?;
        assert_eq!(notes.comments.len(), 1);
        assert_eq!(notes.comments[0].text, "erster Kommentar");
        assert_eq!(notes.evidence, vec!["docs/a.md".to_owned()]);
        assert_eq!(notes.history[0].event, HistoryEvent::ApprovalRequested);
        assert_eq!(
            notes.result.as_ref().map(|result| result.status),
            Some(ResultStatus::Partial)
        );
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn caps_and_validation() -> TestResult {
        let now = jiff::Timestamp::from_second(1_700_000_000)?;
        let mut notes = CardNotes::default();
        for index in 0..(MAX_COMMENTS + 5) {
            notes.add_comment(now, "a", &format!("k{index}"))?;
        }
        assert_eq!(notes.comments.len(), MAX_COMMENTS);
        assert_eq!(notes.comments[0].text, "k5");
        assert!(notes.add_comment(now, "a", "   ").is_err());
        assert!(notes.add_evidence("a\u{7}b").is_err());
        assert!(
            notes
                .add_evidence(&"x".repeat(MAX_EVIDENCE_BYTES + 1))
                .is_err()
        );
        for index in 0..MAX_EVIDENCE {
            notes.add_evidence(&format!("e{index}"))?;
        }
        assert!(notes.add_evidence("eins-zu-viel").is_err());
        let long = "ä".repeat(MAX_COMMENT_BYTES);
        notes.add_comment(now, "a", &long)?;
        let last = notes.comments.last().map(|c| c.text.len()).unwrap_or(0);
        assert!(last <= MAX_COMMENT_BYTES);
        Ok(())
    }

    #[test]
    fn update_of_a_missing_card_fails_and_a_failing_change_writes_nothing() -> TestResult {
        let store = temporary_store("missing")?;
        let board_id = BoardId::new("default");
        let now = jiff::Timestamp::from_second(1_700_000_000)?;
        assert!(
            update_card(
                &store,
                &board_id,
                &CardId::new("card-9"),
                now,
                |_, _| Ok(())
            )
            .is_err()
        );
        save_card(
            &store,
            &board_id,
            &record("card-1"),
            &AgentId::new("op"),
            now,
        )?;
        let failed = update_card(
            &store,
            &board_id,
            &CardId::new("card-1"),
            now,
            |record, _| {
                record.body = "nie gespeichert".to_owned();
                Err::<(), _>(invalid("nein"))
            },
        );
        assert!(failed.is_err());
        let loaded = board::load_card_record(&store, &board_id, &CardId::new("card-1"))?;
        assert!(loaded.body.is_empty());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn find_by_work_id_scans_all_boards() -> TestResult {
        let store = temporary_store("find")?;
        let now = jiff::Timestamp::from_second(1_700_000_000)?;
        let board_id = BoardId::new("zweites");
        board::save_board(
            &store,
            &board::Board {
                id: board_id.clone(),
                name: "Zweites".to_owned(),
                lanes: Vec::new(),
                visibility: VisibilityScope::OperatorOnly,
            },
            &[],
        )?;
        save_card(
            &store,
            &board_id,
            &record("card-3"),
            &AgentId::new("op"),
            now,
        )?;
        let found = find_card_by_work_id(&store, &WorkId::from_str("work-7"))?;
        assert_eq!(
            found.map(|(board, record)| (board, record.id)),
            Some((board_id, CardId::new("card-3")))
        );
        assert!(find_card_by_work_id(&store, &WorkId::from_str("work-x"))?.is_none());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn clip_respects_char_boundaries() {
        assert_eq!(clip("abc", 10), "abc");
        let clipped = clip("äääää", 5);
        assert!(clipped.len() <= 5);
        assert!(clipped.ends_with('…'));
    }
}
