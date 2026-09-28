//! Review-gated output from a governed dream job.
//!
//! Ein [`DreamReport`] wird als gewöhnliches Knowledge-Artefakt
//! (`ArtifactKind::DreamReport`) mit gültigem YAML-Frontmatter unter
//! `dreams/<YYYY-MM-DD>/<work-id>.md` abgelegt — geschrieben ausschließlich über
//! [`crate::store::KnowledgeStore::write_dream_report`], das den gemeinsamen
//! Frontmatter-Writer nutzt. Dadurch kann
//! [`crate::index::KnowledgeIndex::rebuild`] jeden Bericht wieder einlesen.
//!
//! # Strukturierte Seitendatei
//! Neben dem Markdown liegt `dreams/<YYYY-MM-DD>/<work-id>.json`
//! ([`DreamReportData`]): die Vorschläge als typisierte Liste
//! ([`DreamSuggestion`] mit Art, Id, Text und Review-Status), damit Review
//! und TUI nicht das Markdown zurückparsen müssen. Die Datei endet nicht auf
//! `.md` und bleibt damit außerhalb des Index. Lesen über
//! [`read_report_data`], Review-Entscheid über [`set_suggestion_status`]
//! (unter [`crate::lock::KnowledgeLock`]).

use harw_job_core::WorkId;
use serde::{Deserialize, Serialize};

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::lock::KnowledgeLock;
use crate::store::{KnowledgeStore, ensure_path_component, write_atomic};
use crate::visibility::{AgentId, VisibilityScope};

/// Schema-Version von [`DreamReportData`].
pub const DREAM_DATA_SCHEMA_VERSION: u32 = 1;

/// Art eines strukturierten Traumvorschlags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamSuggestionKind {
    /// Topic anlegen/ergänzen (`knowledge/topics/…`, Ziel: Topic-Id).
    Topic,
    /// Palace-Knoten promoten/ergänzen (Ziel: Knoten- bzw. Topic-Id).
    Palace,
    /// Reflexion als Diary-Eintrag (`DiaryTrigger::DreamReflection`).
    DiaryReflection,
    /// Idee für eine neue Skill.
    SkillIdea,
    /// Idee für einen neuen Agenten bzw. eine Rolle.
    AgentIdea,
    /// Offener Faden / Nacharbeit.
    FollowUp,
    /// Wissenspflege (Rollup, Aufbewahrung, Veraltungskandidat).
    Maintenance,
}

impl DreamSuggestionKind {
    /// Serialisierte Bezeichnung (`topic`, `diary_reflection`, …).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Topic => "topic",
            Self::Palace => "palace",
            Self::DiaryReflection => "diary_reflection",
            Self::SkillIdea => "skill_idea",
            Self::AgentIdea => "agent_idea",
            Self::FollowUp => "follow_up",
            Self::Maintenance => "maintenance",
        }
    }
}

/// Review-Status eines Traumvorschlags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamSuggestionStatus {
    /// Noch nicht entschieden.
    #[default]
    Pending,
    /// Angenommen (der Schreibpfad läuft außerhalb dieser Crate).
    Accepted,
    /// Abgelehnt.
    Rejected,
}

impl DreamSuggestionStatus {
    /// Serialisierte Bezeichnung (`pending`, `accepted`, `rejected`).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }
}

/// Ein einzelner, review-pflichtiger Vorschlag eines Traumlaufs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamSuggestion {
    /// Im Bericht eindeutige Id (`p1`, `p2`, …).
    pub id: String,
    /// Art des Vorschlags.
    pub kind: DreamSuggestionKind,
    /// Optionales Ziel (z. B. `topic/runtime`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Vorschlagstext.
    pub text: String,
    /// Review-Status.
    #[serde(default)]
    pub status: DreamSuggestionStatus,
    /// Begründung bzw. Ergebnis der Review-Entscheidung (etwa der
    /// Ablehnungsgrund oder der geschriebene Pfad einer Annahme).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_note: Option<String>,
}

/// Strukturierte Seitendatei eines Traumberichts (`<work-id>.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamReportData {
    /// [`DREAM_DATA_SCHEMA_VERSION`] beim Schreiben.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Job-Id (Dateiname).
    pub work_id: String,
    /// Erstellungszeitpunkt (bestimmt das Datumsverzeichnis, UTC).
    pub created_at: jiff::Timestamp,
    /// Zusammenfassung (wie im Markdown).
    pub summary: String,
    /// Alle Vorschläge in Berichtsreihenfolge.
    #[serde(default)]
    pub suggestions: Vec<DreamSuggestion>,
}

fn default_schema_version() -> u32 {
    DREAM_DATA_SCHEMA_VERSION
}

impl DreamReportData {
    /// Datumsverzeichnis (`YYYY-MM-DD`, UTC aus `created_at`).
    #[must_use]
    pub fn report_date(&self) -> String {
        self.created_at.strftime("%Y-%m-%d").to_string()
    }

    /// Nächste freie Vorschlags-Id (`p<n>`, größtes `n` + 1).
    #[must_use]
    pub fn next_suggestion_id(&self) -> String {
        let highest = self
            .suggestions
            .iter()
            .filter_map(|suggestion| suggestion.id.strip_prefix('p'))
            .filter_map(|number| number.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        format!("p{}", highest.saturating_add(1))
    }
}

/// Autor-Id, unter der Traumberichte im Frontmatter geführt werden.
pub const DREAM_AUTHOR_AGENT_ID: &str = "dream";

/// Wert des `extra.kind`-Frontmatter-Felds eines Traumberichts.
pub const DREAM_REPORT_KIND: &str = "dream_report";

/// A proposal emitted by a dream job; it is not a committed knowledge write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamProposal {
    pub artifact_id: ArtifactId,
    pub summary: String,
}

/// The durable report a dream job may write to its own output location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamReport {
    /// Governte Job-Id; zugleich Dateiname (`dreams/<datum>/<work_id>.md`).
    pub work_id: WorkId,
    /// Erstellungszeitpunkt; bestimmt `created_at` und das Datumsverzeichnis (UTC).
    pub created_at: jiff::Timestamp,
    pub summary: String,
    pub proposed_topic_updates: Vec<DreamProposal>,
    pub proposed_palace_promotions: Vec<DreamProposal>,
    pub follow_ups: Vec<String>,
}

impl DreamReport {
    #[must_use]
    pub fn has_proposals(&self) -> bool {
        !self.proposed_topic_updates.is_empty() || !self.proposed_palace_promotions.is_empty()
    }

    /// Datumsverzeichnis des Berichts (`YYYY-MM-DD`, UTC aus `created_at`).
    #[must_use]
    pub fn report_date(&self) -> String {
        self.created_at.strftime("%Y-%m-%d").to_string()
    }

    /// Artefakt-Id, die [`crate::index::KnowledgeIndex::rebuild`] für den
    /// geschriebenen Bericht ableitet (`dream/<YYYY-MM-DD>/<work-id>`).
    #[must_use]
    pub fn artifact_id(&self) -> ArtifactId {
        ArtifactId::new(format!(
            "dream/{}/{}",
            self.report_date(),
            self.work_id.as_str()
        ))
    }

    /// Frontmatter des Berichts: `visibility: operator_only`, `created_at`,
    /// Tags `dream`/`review-gated` und in `extra` die Felder `kind:
    /// dream_report`, `work_id` und `review_gated: true`.
    #[must_use]
    pub fn frontmatter(&self) -> Frontmatter {
        let mut frontmatter = Frontmatter::new(
            AgentId::new(DREAM_AUTHOR_AGENT_ID),
            VisibilityScope::OperatorOnly,
            self.created_at,
        );
        frontmatter.tags = vec!["dream".to_owned(), "review-gated".to_owned()];
        frontmatter.extra.insert(
            "kind".to_owned(),
            serde_json::Value::from(DREAM_REPORT_KIND),
        );
        frontmatter.extra.insert(
            "work_id".to_owned(),
            serde_json::Value::from(self.work_id.as_str()),
        );
        frontmatter
            .extra
            .insert("review_gated".to_owned(), serde_json::Value::Bool(true));
        frontmatter
    }

    /// Markdown-Body des Berichts (ohne Frontmatter).
    #[must_use]
    pub fn render_body(&self) -> String {
        let mut body = format!(
            "# Traumbericht {}\n\n\
             - Zeit: {}\n\
             - Status: **review-gated** (keine automatische Übernahme in Memory/Palace)\n\n\
             ## Zusammenfassung\n\n{}\n",
            self.work_id.as_str(),
            self.created_at.strftime("%Y-%m-%d %H:%M:%S UTC"),
            self.summary.trim_end(),
        );
        push_proposals(
            &mut body,
            "Vorgeschlagene Topic-Updates",
            &self.proposed_topic_updates,
        );
        push_proposals(
            &mut body,
            "Vorgeschlagene Palace-Promotionen",
            &self.proposed_palace_promotions,
        );
        body.push_str("\n## Offene Fäden\n\n");
        if self.follow_ups.is_empty() {
            body.push_str("_Keine._\n");
        } else {
            for follow_up in &self.follow_ups {
                body.push_str("- ");
                body.push_str(follow_up.trim());
                body.push('\n');
            }
        }
        body.push_str(
            "\n_Keine automatisch übernommenen Änderungen. Prüfe den Bericht und \
             promote Inhalte bei Bedarf manuell nach `topics/` oder `palace/`._\n",
        );
        body
    }

    /// Die Vorschläge des Berichts als typisierte Liste in
    /// Berichtsreihenfolge: Topic-Updates, Palace-Promotionen, offene Fäden;
    /// Ids `p1`, `p2`, …, Status [`DreamSuggestionStatus::Pending`].
    #[must_use]
    pub fn suggestions(&self) -> Vec<DreamSuggestion> {
        let proposals = self
            .proposed_topic_updates
            .iter()
            .map(|proposal| (DreamSuggestionKind::Topic, proposal))
            .chain(
                self.proposed_palace_promotions
                    .iter()
                    .map(|proposal| (DreamSuggestionKind::Palace, proposal)),
            )
            .map(|(kind, proposal)| {
                (
                    kind,
                    Some(proposal.artifact_id.as_str().to_owned()),
                    proposal.summary.trim().to_owned(),
                )
            });
        let follow_ups = self.follow_ups.iter().map(|follow_up| {
            (
                DreamSuggestionKind::FollowUp,
                None,
                follow_up.trim().to_owned(),
            )
        });
        proposals
            .chain(follow_ups)
            .enumerate()
            .map(|(index, (kind, target, text))| DreamSuggestion {
                id: format!("p{}", index + 1),
                kind,
                target,
                text,
                status: DreamSuggestionStatus::Pending,
                decision_note: None,
            })
            .collect()
    }

    /// Die strukturierte Seitendatei dieses Berichts ([`DreamReportData`]).
    #[must_use]
    pub fn to_data(&self) -> DreamReportData {
        DreamReportData {
            schema_version: DREAM_DATA_SCHEMA_VERSION,
            work_id: self.work_id.as_str().to_owned(),
            created_at: self.created_at,
            summary: self.summary.clone(),
            suggestions: self.suggestions(),
        }
    }

    /// Der Bericht als [`KnowledgeArtifact`] (`ArtifactKind::DreamReport`).
    #[must_use]
    pub fn to_artifact(&self) -> KnowledgeArtifact {
        KnowledgeArtifact::new(
            self.artifact_id(),
            ArtifactKind::DreamReport,
            self.frontmatter(),
            self.render_body(),
        )
    }

    /// Vollständiges Markdown-Dokument: `---`-YAML-Frontmatter plus Body,
    /// gerendert über [`crate::store::render_frontmatter`].
    ///
    /// # Errors
    /// [`crate::error::KnowledgeError::Frontmatter`] bei einem YAML-Encode-Fehler.
    pub fn render_markdown(&self) -> KnowledgeResult<String> {
        crate::store::render_frontmatter(&self.frontmatter(), &self.render_body())
    }
}

/// Zerlegt eine Traumbericht-Id `dream/<YYYY-MM-DD>/<work-id>` in
/// `(datum, work_id)`.
#[must_use]
pub fn split_report_id(id: &ArtifactId) -> Option<(String, String)> {
    let rest = id.as_str().strip_prefix("dream/")?;
    let (date, work_id) = rest.split_once('/')?;
    if date.is_empty() || work_id.is_empty() || work_id.contains('/') {
        return None;
    }
    Some((date.to_owned(), work_id.to_owned()))
}

/// Schreibt die strukturierte Seitendatei eines Berichts atomar nach
/// `dreams/<date>/<work_id>.json`.
///
/// # Rückgabe
/// Den geschriebenen Pfad.
///
/// # Fehler
/// [`KnowledgeError::Io`] (`InvalidInput`) für unsichere `date`/`work_id`;
/// sonst JSON-/Schreibfehler.
pub fn write_report_data(
    store: &KnowledgeStore,
    date: &str,
    work_id: &str,
    data: &DreamReportData,
) -> KnowledgeResult<std::path::PathBuf> {
    ensure_path_component(date)?;
    ensure_path_component(work_id)?;
    let path = store.dream_data_path(date, work_id);
    let rendered = serde_json::to_string_pretty(data)?;
    write_atomic(&path, &rendered)?;
    Ok(path)
}

/// Liest die strukturierte Seitendatei eines Berichts.
///
/// # Rückgabe
/// `None`, wenn es (etwa bei Altbeständen) keine Seitendatei gibt; der
/// Aufrufer fällt dann auf das Markdown zurück.
///
/// # Fehler
/// [`KnowledgeError::Io`] (`InvalidInput`) für unsichere Komponenten, Lese-
/// oder JSON-Fehler.
pub fn read_report_data(
    store: &KnowledgeStore,
    date: &str,
    work_id: &str,
) -> KnowledgeResult<Option<DreamReportData>> {
    ensure_path_component(date)?;
    ensure_path_component(work_id)?;
    let path = store.dream_data_path(date, work_id);
    if !path.is_file() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(Some(serde_json::from_str(&content)?))
}

/// Entscheidet einen offenen Vorschlag (`pending -> accepted|rejected`).
///
/// # Beschreibung
/// Read-Modify-Write der Seitendatei unter [`KnowledgeLock`]. Führt keinen
/// Schreibpfad des Vorschlags aus — das ist Sache des Aufrufers (Review-Op).
///
/// # Rückgabe
/// Den entschiedenen Vorschlag.
///
/// # Fehler
/// - [`KnowledgeError::ArtifactNotFound`]: keine Seitendatei oder keine
///   Vorschlags-Id `suggestion_id`.
/// - [`KnowledgeError::IllegalTransition`]: Vorschlag ist bereits
///   entschieden oder `status` ist `pending`.
/// - Lese-/Schreib-/JSON-Fehler.
pub fn set_suggestion_status(
    store: &KnowledgeStore,
    date: &str,
    work_id: &str,
    suggestion_id: &str,
    status: DreamSuggestionStatus,
) -> KnowledgeResult<DreamSuggestion> {
    decide_suggestion(store, date, work_id, suggestion_id, status, None)
}

/// Wie [`set_suggestion_status`], hält zusätzlich eine Entscheidungsnotiz
/// fest ([`DreamSuggestion::decision_note`], etwa den Ablehnungsgrund oder
/// das Ergebnis des Schreibpfads). Leere Notizen werden verworfen.
///
/// # Fehler
/// Wie [`set_suggestion_status`].
pub fn decide_suggestion(
    store: &KnowledgeStore,
    date: &str,
    work_id: &str,
    suggestion_id: &str,
    status: DreamSuggestionStatus,
    note: Option<&str>,
) -> KnowledgeResult<DreamSuggestion> {
    ensure_path_component(date)?;
    ensure_path_component(work_id)?;
    let path = store.dream_data_path(date, work_id);
    let _lock = KnowledgeLock::for_target(&path)?;
    let mut data = read_report_data(store, date, work_id)?
        .ok_or_else(|| KnowledgeError::ArtifactNotFound(format!("dream/{date}/{work_id}")))?;
    let suggestion = data
        .suggestions
        .iter_mut()
        .find(|suggestion| suggestion.id == suggestion_id)
        .ok_or_else(|| {
            KnowledgeError::ArtifactNotFound(format!("dream/{date}/{work_id}#{suggestion_id}"))
        })?;
    if suggestion.status != DreamSuggestionStatus::Pending
        || status == DreamSuggestionStatus::Pending
    {
        return Err(KnowledgeError::IllegalTransition {
            from: suggestion.status.label().to_owned(),
            to: status.label().to_owned(),
        });
    }
    suggestion.status = status;
    suggestion.decision_note = note
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(|note| clamp_chars(note, MAX_DECISION_NOTE_CHARS));
    let decided = suggestion.clone();
    write_report_data(store, date, work_id, &data)?;
    Ok(decided)
}

/// Höchstlänge einer Entscheidungsnotiz (Zeichen).
const MAX_DECISION_NOTE_CHARS: usize = 500;

/// Kürzt `text` auf höchstens `max` Zeichen (mit `…`).
fn clamp_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut clamped: String = text.chars().take(max).collect();
    clamped.push('…');
    clamped
}

// ---------------------------------------------------------------------------
// Scheduler-Zustand (`dreams/state.json`)
// ---------------------------------------------------------------------------

/// Dateiname des Scheduler-Zustands unter `dreams/`.
pub const DREAM_STATE_FILE: &str = "state.json";

/// Ergebnis des letzten Traumlaufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DreamRunStatus {
    /// Bericht geschrieben.
    Succeeded,
    /// Lauf abgebrochen (Fehler steht in [`DreamSchedulerState::last_error`]).
    Failed,
}

impl DreamRunStatus {
    /// Serialisierte Bezeichnung.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

/// Markierung eines gerade laufenden Traums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamRunningMarker {
    /// Job-Id des Laufs.
    pub work_id: String,
    /// Startzeitpunkt.
    pub started_at: jiff::Timestamp,
    /// Auslöser (`idle`, `schedule`, `manual`).
    pub trigger: String,
}

/// Dauerhafter Zustand des Traum-Schedulers (`dreams/state.json`).
///
/// # Beschreibung
/// Überlebt einen Neustart: Cooldown und Cron-Entscheidung rechnen ab
/// [`Self::last_run_at`]. Eine [`Self::running`]-Markierung eines
/// abgestürzten Prozesses ist veraltet, sobald sie älter als die
/// Lauf-Obergrenze ist; der Aufrufer bewertet das (siehe
/// [`Self::running_marker`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamSchedulerState {
    /// Beginn des letzten Laufs (erfolgreich oder nicht).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<jiff::Timestamp>,
    /// Job-Id des letzten Laufs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_work_id: Option<String>,
    /// Ergebnis des letzten Laufs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<DreamRunStatus>,
    /// Auslöser des letzten Laufs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_trigger: Option<String>,
    /// Fehlermeldung des letzten Laufs, falls er scheiterte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Id des zuletzt geschriebenen Berichts (`dream/<datum>/<work-id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_report_id: Option<String>,
    /// Gerade laufender Traum, falls einer läuft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running: Option<DreamRunningMarker>,
    /// Anzahl aller begonnenen Läufe.
    #[serde(default)]
    pub runs: u64,
}

impl DreamSchedulerState {
    /// Die Laufmarkierung, sofern sie zum Zeitpunkt `now` noch nicht älter
    /// als `stale_after` ist (eine ältere stammt von einem abgestürzten
    /// Prozess).
    #[must_use]
    pub fn running_marker(
        &self,
        now: jiff::Timestamp,
        stale_after: jiff::SignedDuration,
    ) -> Option<&DreamRunningMarker> {
        self.running
            .as_ref()
            .filter(|marker| now.duration_since(marker.started_at) <= stale_after)
    }
}

/// Pfad des Scheduler-Zustands (`<root>/dreams/state.json`).
#[must_use]
pub fn scheduler_state_path(store: &KnowledgeStore) -> std::path::PathBuf {
    store.root().join("dreams").join(DREAM_STATE_FILE)
}

/// Liest den Scheduler-Zustand; eine fehlende Datei ergibt den leeren
/// Zustand.
///
/// # Fehler
/// Lese- oder JSON-Fehler einer vorhandenen Datei.
pub fn read_scheduler_state(store: &KnowledgeStore) -> KnowledgeResult<DreamSchedulerState> {
    let path = scheduler_state_path(store);
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(serde_json::from_str(&content)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(DreamSchedulerState::default())
        }
        Err(error) => Err(KnowledgeError::Io(error)),
    }
}

/// Ändert den Scheduler-Zustand atomar unter [`KnowledgeLock`].
///
/// # Beschreibung
/// Read-Modify-Write: ein unlesbarer (defekter) Zustand wird durch den
/// leeren ersetzt, statt den Scheduler dauerhaft zu blockieren.
///
/// # Rückgabe
/// Den geschriebenen Zustand.
///
/// # Fehler
/// Sperr-, JSON- und Schreibfehler.
pub fn update_scheduler_state(
    store: &KnowledgeStore,
    change: impl FnOnce(&mut DreamSchedulerState),
) -> KnowledgeResult<DreamSchedulerState> {
    let path = scheduler_state_path(store);
    let _lock = KnowledgeLock::for_target(&path)?;
    let mut state = read_scheduler_state(store).unwrap_or_default();
    change(&mut state);
    write_atomic(&path, &serde_json::to_string_pretty(&state)?)?;
    Ok(state)
}

// ---------------------------------------------------------------------------
// Palace-Veraltungskandidaten (Wissenspflege, D5)
// ---------------------------------------------------------------------------

/// Ein Kandidat für die Palace-/Topic-Pflege — nie eine Löschung, nur ein
/// Hinweis für den Review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleCandidate {
    /// Betroffener Knoten bzw. betroffenes Thema.
    pub id: ArtifactId,
    /// Deutsche Begründung.
    pub reason: String,
}

/// Sucht Veraltungskandidaten unter Palace-Knoten und Themen.
///
/// # Beschreibung
/// - `provisional` seit mehr als `max_age_days` unverändert → promoten
///   oder verwerfen.
/// - `established` mit einem Verweis auf einen `superseded` Eintrag →
///   Verweis nachziehen.
///
/// Reine Lesefunktion über dem Index; sortiert nach Id, höchstens `limit`
/// Einträge.
#[must_use]
pub fn palace_stale_candidates(
    index: &crate::index::KnowledgeIndex,
    now: jiff::Timestamp,
    max_age_days: i64,
    limit: usize,
) -> Vec<StaleCandidate> {
    use crate::memory::palace::{PalaceStatus, artifact_status};

    let max_age = max_age_days.max(0).saturating_mul(86_400);
    let mut found: Vec<StaleCandidate> = Vec::new();
    let mut artifacts: Vec<&KnowledgeArtifact> = index
        .iter()
        .filter(|artifact| {
            matches!(
                artifact.kind,
                ArtifactKind::PalaceNode | ArtifactKind::TopicMemory
            )
        })
        .collect();
    artifacts.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    for artifact in artifacts {
        match artifact_status(artifact) {
            PalaceStatus::Provisional => {
                let age = now
                    .as_second()
                    .saturating_sub(artifact.frontmatter.updated_at.as_second());
                if age > max_age {
                    found.push(StaleCandidate {
                        id: artifact.id.clone(),
                        reason: format!(
                            "seit {} Tagen provisional und unverändert — promoten (/palace promote) oder verwerfen",
                            age / 86_400
                        ),
                    });
                }
            }
            PalaceStatus::Established => {
                for link in &artifact.frontmatter.links {
                    let superseded = index
                        .get(link)
                        .is_some_and(|target| artifact_status(target) == PalaceStatus::Superseded);
                    if superseded {
                        found.push(StaleCandidate {
                            id: artifact.id.clone(),
                            reason: format!(
                                "verweist auf den abgelösten Eintrag {} — Verweis nachziehen",
                                link.as_str()
                            ),
                        });
                    }
                }
            }
            PalaceStatus::Superseded => {}
        }
        if found.len() >= limit {
            found.truncate(limit);
            break;
        }
    }
    found
}

/// Hängt einen Vorschlagsabschnitt an den Body an.
fn push_proposals(body: &mut String, heading: &str, proposals: &[DreamProposal]) {
    body.push_str("\n## ");
    body.push_str(heading);
    body.push_str("\n\n");
    if proposals.is_empty() {
        body.push_str("_Keine._\n");
        return;
    }
    for proposal in proposals {
        body.push_str("- `");
        body.push_str(proposal.artifact_id.as_str());
        body.push_str("`: ");
        body.push_str(proposal.summary.trim());
        body.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn report() -> DreamReport {
        DreamReport {
            work_id: WorkId::from_str("work-1"),
            created_at: jiff::Timestamp::UNIX_EPOCH,
            summary: "none".to_owned(),
            proposed_topic_updates: Vec::new(),
            proposed_palace_promotions: Vec::new(),
            follow_ups: Vec::new(),
        }
    }

    #[test]
    fn written_report_carries_a_structured_sidecar_with_reviewable_suggestions() -> TestResult {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(crate::test_support::ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-knowledge-dream-data-{}-{nonce}",
            std::process::id()
        ));
        let store = KnowledgeStore::new(&root);
        let mut report = report();
        report.proposed_topic_updates.push(DreamProposal {
            artifact_id: ArtifactId::new("topic/runtime"),
            summary: "Laufzeitnotiz".to_owned(),
        });
        report.follow_ups.push("Deploy prüfen".to_owned());
        store.write_dream_report(&report)?;

        let (date, work_id) = split_report_id(&report.artifact_id())
            .ok_or(crate::test_support::TestError::Missing("report id splits"))?;
        let data = read_report_data(&store, &date, &work_id)?
            .ok_or(crate::test_support::TestError::Missing("sidecar"))?;
        assert_eq!(data.suggestions.len(), 2);
        assert_eq!(data.suggestions[0].kind, DreamSuggestionKind::Topic);
        assert_eq!(data.suggestions[0].target.as_deref(), Some("topic/runtime"));
        assert_eq!(data.suggestions[1].kind, DreamSuggestionKind::FollowUp);
        assert_eq!(data.next_suggestion_id(), "p3");
        let json = serde_json::to_value(&data)?;
        assert_eq!(json["suggestions"][1]["kind"], "follow_up");
        assert_eq!(json["suggestions"][1]["status"], "pending");

        let decided = set_suggestion_status(
            &store,
            &date,
            &work_id,
            "p1",
            DreamSuggestionStatus::Accepted,
        )?;
        assert_eq!(decided.status, DreamSuggestionStatus::Accepted);
        assert!(matches!(
            set_suggestion_status(
                &store,
                &date,
                &work_id,
                "p1",
                DreamSuggestionStatus::Rejected
            ),
            Err(KnowledgeError::IllegalTransition { .. })
        ));
        assert!(matches!(
            set_suggestion_status(
                &store,
                &date,
                &work_id,
                "p9",
                DreamSuggestionStatus::Rejected
            ),
            Err(KnowledgeError::ArtifactNotFound(_))
        ));
        let reread = read_report_data(&store, &date, &work_id)?.ok_or(
            crate::test_support::TestError::Missing("sidecar after review"),
        )?;
        assert_eq!(
            reread.suggestions[0].status,
            DreamSuggestionStatus::Accepted
        );
        assert_eq!(read_report_data(&store, &date, "other")?, None);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    fn temporary_store(label: &str) -> TestResult<KnowledgeStore> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(crate::test_support::ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-knowledge-dream-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        Ok(KnowledgeStore::new(&root))
    }

    #[test]
    fn decide_suggestion_keeps_a_trimmed_note_and_drops_empty_ones() -> TestResult {
        let store = temporary_store("note")?;
        let mut report = report();
        report.follow_ups.push("A".to_owned());
        report.follow_ups.push("B".to_owned());
        store.write_dream_report(&report)?;
        let decided = decide_suggestion(
            &store,
            "1970-01-01",
            "work-1",
            "p1",
            DreamSuggestionStatus::Rejected,
            Some("  nicht relevant  "),
        )?;
        assert_eq!(decided.decision_note.as_deref(), Some("nicht relevant"));
        let decided = decide_suggestion(
            &store,
            "1970-01-01",
            "work-1",
            "p2",
            DreamSuggestionStatus::Accepted,
            Some("   "),
        )?;
        assert_eq!(decided.decision_note, None);
        let data = read_report_data(&store, "1970-01-01", "work-1")?
            .ok_or(crate::test_support::TestError::Missing("sidecar"))?;
        assert_eq!(
            data.suggestions[0].decision_note.as_deref(),
            Some("nicht relevant")
        );
        std::fs::remove_dir_all(store.root())?;
        Ok(())
    }

    #[test]
    fn scheduler_state_roundtrips_and_survives_a_corrupt_file() -> TestResult {
        let store = temporary_store("state")?;
        assert_eq!(
            read_scheduler_state(&store)?,
            DreamSchedulerState::default()
        );
        let at = jiff::Timestamp::from_second(1_000)?;
        update_scheduler_state(&store, |state| {
            state.last_run_at = Some(at);
            state.last_status = Some(DreamRunStatus::Succeeded);
            state.runs += 1;
        })?;
        let state = read_scheduler_state(&store)?;
        assert_eq!(state.last_run_at, Some(at));
        assert_eq!(state.runs, 1);
        assert!(scheduler_state_path(&store).ends_with("dreams/state.json"));

        std::fs::write(scheduler_state_path(&store), "kein json")?;
        assert!(read_scheduler_state(&store).is_err());
        let repaired = update_scheduler_state(&store, |state| state.runs = 7)?;
        assert_eq!(repaired.runs, 7);
        // Der Zustand liegt außerhalb des Index (kein `.md`).
        crate::index::KnowledgeIndex::rebuild(&store)?;
        std::fs::remove_dir_all(store.root())?;
        Ok(())
    }

    #[test]
    fn a_running_marker_goes_stale_after_the_bound() -> TestResult {
        let started_at = jiff::Timestamp::from_second(0)?;
        let state = DreamSchedulerState {
            running: Some(DreamRunningMarker {
                work_id: "dream-x".to_owned(),
                started_at,
                trigger: "manual".to_owned(),
            }),
            ..DreamSchedulerState::default()
        };
        let bound = jiff::SignedDuration::from_mins(10);
        assert!(
            state
                .running_marker(jiff::Timestamp::from_second(60)?, bound)
                .is_some()
        );
        assert!(
            state
                .running_marker(jiff::Timestamp::from_second(3_600)?, bound)
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn palace_stale_candidates_flag_old_provisional_and_superseded_links() -> TestResult {
        use crate::memory::palace::{PalaceStatus, set_status};
        use crate::memory::topic;

        let store = temporary_store("stale")?;
        let old = jiff::Timestamp::from_second(0)?;
        let now = jiff::Timestamp::from_second(40 * 86_400)?;
        let author = AgentId::new("operator");

        let mut provisional = Frontmatter::new(author.clone(), VisibilityScope::OperatorOnly, old);
        set_status(&mut provisional, PalaceStatus::Provisional);
        topic::write(&store, "alt", provisional, "alter Entwurf")?;

        let mut superseded = Frontmatter::new(author.clone(), VisibilityScope::OperatorOnly, now);
        set_status(&mut superseded, PalaceStatus::Superseded);
        topic::write(&store, "abgeloest", superseded, "abgelöst")?;

        let mut established = Frontmatter::new(author.clone(), VisibilityScope::OperatorOnly, now);
        set_status(&mut established, PalaceStatus::Established);
        established.links = vec![ArtifactId::new("topic/abgeloest")];
        topic::write(&store, "fest", established, "siehe [[topic/abgeloest]]")?;

        let mut fresh = Frontmatter::new(author, VisibilityScope::OperatorOnly, now);
        set_status(&mut fresh, PalaceStatus::Provisional);
        topic::write(&store, "frisch", fresh, "neu")?;

        let index = crate::index::KnowledgeIndex::rebuild(&store)?;
        let candidates = palace_stale_candidates(&index, now, 30, 10);
        let ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["topic/alt", "topic/fest"], "{candidates:?}");
        assert!(candidates[1].reason.contains("topic/abgeloest"));
        assert_eq!(palace_stale_candidates(&index, now, 30, 1).len(), 1);
        std::fs::remove_dir_all(store.root())?;
        Ok(())
    }

    #[test]
    fn report_with_no_proposals_is_not_committable_work() {
        assert!(!report().has_proposals());
    }

    #[test]
    fn render_markdown_produces_parseable_operator_only_frontmatter() -> TestResult {
        let mut report = report();
        report.proposed_topic_updates.push(DreamProposal {
            artifact_id: ArtifactId::new("topic/runtime"),
            summary: "Laufzeitnotiz ergänzen".to_owned(),
        });
        report.follow_ups.push("Deploy prüfen".to_owned());

        let rendered = report.render_markdown()?;
        let (frontmatter, body) = crate::store::parse_frontmatter(&rendered)?;

        assert_eq!(frontmatter.visibility, VisibilityScope::OperatorOnly);
        assert_eq!(frontmatter.created_at, jiff::Timestamp::UNIX_EPOCH);
        assert_eq!(
            frontmatter.extra.get("kind"),
            Some(&serde_json::Value::from(DREAM_REPORT_KIND))
        );
        assert_eq!(
            frontmatter.extra.get("work_id"),
            Some(&serde_json::Value::from("work-1"))
        );
        assert!(body.contains("# Traumbericht work-1"));
        assert!(body.contains("`topic/runtime`: Laufzeitnotiz ergänzen"));
        assert!(body.contains("- Deploy prüfen"));
        assert_eq!(
            report.artifact_id(),
            ArtifactId::new("dream/1970-01-01/work-1")
        );
        Ok(())
    }
}
