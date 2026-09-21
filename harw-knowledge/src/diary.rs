//! Typed diary entries and append-only daily persistence (§3 of
//! `docs/design/knowledge-surfaces.md`).
//!
//! # Verantwortung
//! Dieses Modul besitzt die Diary-Oberfläche: eine append-only Tagesdatei je
//! Agent (`knowledge/diary/<agent-id>/<YYYY-MM-DD>.md`, §3.1) sowie die
//! Retention/Rollup-Regel aus §3.3. Persistenz delegiert vollständig an
//! [`crate::store::KnowledgeStore`]'s Atomic-Write- und
//! Frontmatter-Helfer — dieses Modul öffnet nie selbst eine Datei zum
//! Schreiben, außer über `write_atomic`/`write_artifact`.
//!
//! # Concurrency
//! Reine, threadsichere Datenfunktionen ohne eigenes Locking. Gleichzeitige
//! `append`-/`gc`-Aufrufe für denselben Agenten müssen vom Aufrufer
//! serialisiert werden (read-modify-write auf derselben Tages-/Rollup-Datei).
//!
//! # Fehler
//! Jeder fehlschlagende Pfad liefert [`crate::error::KnowledgeError`] /
//! [`crate::error::KnowledgeResult`].

use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::KnowledgeResult;
use crate::store::{parse_frontmatter, KnowledgeStore};
use crate::visibility::{AgentId, VisibilityScope};

/// Why a diary entry was appended rather than edited in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiaryTrigger {
    EndOfSession,
    Compaction,
    DreamReflection,
    Manual,
}

impl DiaryTrigger {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::EndOfSession => "end-of-session",
            Self::Compaction => "compaction",
            Self::DreamReflection => "dream-reflection",
            Self::Manual => "manual",
        }
    }
}

/// One immutable append in a per-agent, per-day diary file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiaryEntry {
    pub agent_id: AgentId,
    pub recorded_at: Timestamp,
    pub trigger: DiaryTrigger,
    pub visibility: VisibilityScope,
    pub body: String,
}

impl DiaryEntry {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "### {} — {}\n\n{}\n",
            self.recorded_at.strftime("%H:%M:%S"),
            self.trigger.label(),
            self.body
        )
    }
}

/// Standard-Aufbewahrungsfenster für Diary-Tagesdateien in Tagen (§3.3).
pub const DEFAULT_DIARY_RETENTION_DAYS: i64 = 90;

/// Länge eines `YYYY-MM-DD`-Dateinamensstamms.
const DAY_STEM_LEN: usize = 10;

/// Länge eines `YYYY-MM`-Präfixes eines Tagesdatums.
const MONTH_PREFIX_LEN: usize = 7;

/// Frontmatter-Schlüssel im `extra`-Escape-Hatch der Tages-/Rollup-Datei.
const AGENT_ID_KEY: &str = "agent_id";
const DATE_KEY: &str = "date";
const MONTH_KEY: &str = "month";
const ENTRY_COUNT_KEY: &str = "entry_count";

/// Formatiert einen Zeitstempel als `YYYY-MM-DD` (Name der Tagesdatei).
fn day_key(at: Timestamp) -> String {
    at.strftime("%Y-%m-%d").to_string()
}

/// Baut die stabile Artefakt-ID einer Diary-Datei, exakt im Format, das
/// `KnowledgeIndex::rebuild` beim Einlesen aus dem Pfad ableitet
/// (`diary/<agent-id>/<local-suffix>`).
fn diary_artifact_id(agent_id: &AgentId, local_suffix: &str) -> ArtifactId {
    ArtifactId::new(format!("diary/{}/{local_suffix}", agent_id.as_str()))
}

/// Liest den bisherigen `entry_count` aus der Frontmatter, `0` falls fehlend.
fn entry_count(frontmatter: &Frontmatter) -> u64 {
    frontmatter
        .extra
        .get(ENTRY_COUNT_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// Hängt einen [`DiaryEntry`] an die Tagesdatei seines Halters an.
///
/// # Beschreibung
/// Diary-Dateien sind append-only (§3.1): bestehende Einträge werden nie
/// verändert, es wird nur ein neuer `### HH:MM:SS — <trigger>`-Abschnitt an
/// den bestehenden Body angehängt. Existiert für den Tag von
/// `entry.recorded_at` noch keine Datei, wird eine neue mit frischer
/// Frontmatter angelegt. Die Frontmatter trägt `agent_id`, `date` und einen
/// mitgeführten `entry_count` im `extra`-Escape-Hatch von [`Frontmatter`] —
/// bewusst kein eigenes Diary-Frontmatter-Schema neben dem kanonischen aus
/// §1.2. Persistiert wird ausschließlich über
/// [`crate::store::KnowledgeStore::write_artifact`], also über denselben
/// atomaren Temp-Write-plus-Rename wie jede andere Surface.
///
/// # Argumente
/// - `store` (`&KnowledgeStore`): der Wissensspeicher, unter dem
///   `diary/<agent-id>/<YYYY-MM-DD>.md` liegt.
/// - `entry` (`&DiaryEntry`): der anzuhängende Eintrag.
///
/// # Rückgabe
/// Das vollständige, nach dem Anhängen persistierte [`KnowledgeArtifact`] der
/// Tagesdatei.
///
/// # Fehler
/// - [`crate::error::KnowledgeError::Io`]\: Lese-/Schreib-/Rename-Fehler.
/// - [`crate::error::KnowledgeError::MalformedFrontmatter`] /
///   [`crate::error::KnowledgeError::Frontmatter`]: eine bestehende
///   Tagesdatei ließ sich nicht als gültiges Frontmatter-Dokument parsen.
///
/// # Nebenläufigkeit
/// Kein eigenes Locking; diese Funktion liest, ändert im Speicher und
/// schreibt dann atomar zurück — gleichzeitige `append`-Aufrufe für denselben
/// Tag desselben Agenten muss der Aufrufer serialisieren.
///
/// # Examples
/// ```rust,no_run
/// use harw_knowledge::diary::{DiaryEntry, DiaryTrigger, append};
/// use harw_knowledge::{AgentId, KnowledgeStore, VisibilityScope};
///
/// let store = KnowledgeStore::new(std::path::Path::new("/tmp/knowledge"));
/// let entry = DiaryEntry {
///     agent_id: AgentId::new("agent-1"),
///     recorded_at: jiff::Timestamp::now(),
///     trigger: DiaryTrigger::EndOfSession,
///     visibility: VisibilityScope::SelfOnly,
///     body: "Session abgeschlossen.".to_owned(),
/// };
/// let _artifact = append(&store, &entry)?;
/// # Ok::<(), harw_knowledge::KnowledgeError>(())
/// ```
pub fn append(store: &KnowledgeStore, entry: &DiaryEntry) -> KnowledgeResult<KnowledgeArtifact> {
    let date = day_key(entry.recorded_at);
    let path = store.diary_path(&entry.agent_id, &date);
    let id = diary_artifact_id(&entry.agent_id, &date);

    let mut artifact = if path.is_file() {
        store.read_artifact(&path, id, ArtifactKind::DiaryEntry)?
    } else {
        let frontmatter = Frontmatter::new(
            entry.agent_id.clone(),
            entry.visibility.clone(),
            entry.recorded_at,
        );
        KnowledgeArtifact::new(id, ArtifactKind::DiaryEntry, frontmatter, String::new())
    };

    let next_count = entry_count(&artifact.frontmatter) + 1;
    artifact.body.push_str(&entry.render());
    artifact.body.push('\n');
    artifact.frontmatter.touch(entry.recorded_at);
    artifact.frontmatter.extra.insert(
        AGENT_ID_KEY.to_owned(),
        serde_json::Value::String(entry.agent_id.as_str().to_owned()),
    );
    artifact
        .frontmatter
        .extra
        .insert(DATE_KEY.to_owned(), serde_json::Value::String(date));
    artifact.frontmatter.extra.insert(
        ENTRY_COUNT_KEY.to_owned(),
        serde_json::Value::from(next_count),
    );

    store.write_artifact(&path, &artifact)?;
    Ok(artifact)
}

/// Liest die Tagesdatei eines Agenten, falls vorhanden.
///
/// # Beschreibung
/// Reiner Lesezugriff über [`crate::store::KnowledgeStore::read_artifact`];
/// erzeugt oder verändert nichts. Ein noch nicht begonnener Diary-Tag ist ein
/// gültiger, erwarteter Zustand und wird daher als `Ok(None)` gemeldet, nicht
/// als Fehler.
///
/// # Argumente
/// - `store` (`&KnowledgeStore`): der Wissensspeicher.
/// - `agent_id` (`&AgentId`): der Halter, dessen Diary gelesen wird.
/// - `date` (`&str`): Datum im Format `YYYY-MM-DD`.
///
/// # Rückgabe
/// `Some(artifact)`, wenn der Tag existiert, sonst `None`.
///
/// # Fehler
/// [`crate::error::KnowledgeError::Io`] auf Lesefehler,
/// [`crate::error::KnowledgeError::MalformedFrontmatter`] /
/// [`crate::error::KnowledgeError::Frontmatter`] auf einen kaputten Fence
/// bzw. YAML-Fehler.
///
/// # Nebenläufigkeit
/// Reiner Lesezugriff, sicher aus mehreren Threads parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_knowledge::diary::read_day;
/// use harw_knowledge::{AgentId, KnowledgeStore};
///
/// let store = KnowledgeStore::new(std::path::Path::new("/tmp/knowledge"));
/// let _day = read_day(&store, &AgentId::new("agent-1"), "2026-09-21")?;
/// # Ok::<(), harw_knowledge::KnowledgeError>(())
/// ```
pub fn read_day(
    store: &KnowledgeStore,
    agent_id: &AgentId,
    date: &str,
) -> KnowledgeResult<Option<KnowledgeArtifact>> {
    let path = store.diary_path(agent_id, date);
    if !path.is_file() {
        return Ok(None);
    }
    let id = diary_artifact_id(agent_id, date);
    Ok(Some(store.read_artifact(&path, id, ArtifactKind::DiaryEntry)?))
}

/// Ergebnis eines `gc`-Laufs: welche Tagesdateien in welche Monats-Rollups
/// eingegangen sind (§3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiaryGcReport {
    /// Der Halter, für den aufgeräumt wurde.
    pub agent_id: AgentId,
    /// Tagesdateien (`YYYY-MM-DD`), die in ein Rollup übernommen und danach
    /// entfernt wurden, in aufsteigender Reihenfolge.
    pub rolled_up_days: Vec<String>,
    /// Monatliche Rollup-Dateien (`YYYY-MM`), die dabei angelegt oder erweitert wurden.
    pub touched_months: Vec<String>,
}

/// Komprimiert Diary-Tagesdateien, die älter als `retention_days` sind, in
/// monatliche Rollup-Dateien, statt sie zu löschen.
///
/// # Beschreibung
/// Implementiert §3.3: Tagesdateien in `diary/<agent-id>/`, deren
/// `YYYY-MM-DD`-Dateiname strikt vor `now - retention_days` liegt, werden
/// unter einer eigenen `## <date>`-Zwischenüberschrift an
/// `diary/<agent-id>/rollup/<YYYY-MM>.md` angehängt (mehrere aufgeräumte Tage
/// desselben Monats landen additiv in derselben Rollup-Datei), sodass nichts
/// Durables verloren geht, das Tagesverzeichnis aber nicht unbeschränkt
/// wächst. Erst nach erfolgreichem Rollup-Schreiben wird die
/// Quell-Tagesdatei entfernt. Die Rollup-Datei führt ihrerseits einen
/// kumulierten `entry_count` fort. Nur direkte Kinder von
/// `diary/<agent-id>/` werden als Tagesdateien betrachtet, sodass das
/// `rollup/`-Unterverzeichnis selbst nie erneut aufgeräumt wird.
///
/// # Argumente
/// - `store` (`&KnowledgeStore`): der Wissensspeicher.
/// - `agent_id` (`&AgentId`): der Halter, dessen Diary aufgeräumt wird.
/// - `now` (`Timestamp`): Referenzzeitpunkt für die Aufbewahrungsgrenze.
/// - `retention_days` (`i64`): Aufbewahrungsfenster in Tagen; negative Werte
///   werden auf `0` geklemmt (alles vorhandene wird sofort rollup-fähig).
///
/// # Rückgabe
/// Ein [`DiaryGcReport`] mit den verschobenen Tagen und berührten Monaten.
/// Ein fehlendes Agenten-Verzeichnis liefert einen leeren Report.
///
/// # Fehler
/// - [`crate::error::KnowledgeError::Time`]: die Cutoff-Berechnung hat den
///   von `jiff` darstellbaren Zeitbereich überschritten.
/// - [`crate::error::KnowledgeError::Io`]\: Lese-/Schreib-/Löschfehler.
/// - [`crate::error::KnowledgeError::MalformedFrontmatter`] /
///   [`crate::error::KnowledgeError::Frontmatter`]: eine Tages- oder
///   Rollup-Datei ließ sich nicht parsen.
///
/// # Panics
/// Keine.
///
/// # Nebenläufigkeit
/// Kein eigenes Locking; ein `gc`-Lauf darf nicht parallel zu einem `append`
/// oder einem weiteren `gc`-Lauf für denselben Agenten laufen.
///
/// # Examples
/// ```rust,no_run
/// use harw_knowledge::diary::{DEFAULT_DIARY_RETENTION_DAYS, gc};
/// use harw_knowledge::{AgentId, KnowledgeStore};
///
/// let store = KnowledgeStore::new(std::path::Path::new("/tmp/knowledge"));
/// let _report = gc(
///     &store,
///     &AgentId::new("agent-1"),
///     jiff::Timestamp::now(),
///     DEFAULT_DIARY_RETENTION_DAYS,
/// )?;
/// # Ok::<(), harw_knowledge::KnowledgeError>(())
/// ```
pub fn gc(
    store: &KnowledgeStore,
    agent_id: &AgentId,
    now: Timestamp,
    retention_days: i64,
) -> KnowledgeResult<DiaryGcReport> {
    let retention_days = retention_days.max(0);
    let cutoff_span = SignedDuration::from_secs(retention_days.saturating_mul(86_400));
    let cutoff = now.checked_sub(cutoff_span)?;
    let cutoff_date = day_key(cutoff);

    let agent_dir = store.root().join("diary").join(agent_id.as_str());
    let mut report = DiaryGcReport {
        agent_id: agent_id.clone(),
        rolled_up_days: Vec::new(),
        touched_months: Vec::new(),
    };

    if !agent_dir.is_dir() {
        return Ok(report);
    }

    let mut eligible: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&agent_dir)? {
        let entry = entry?;
        let path = entry.path();
        let is_day_file = path.is_file() && path.extension().is_some_and(|ext| ext == "md");
        if !is_day_file {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem.len() == DAY_STEM_LEN && stem < cutoff_date.as_str() {
            eligible.push((stem.to_owned(), path));
        }
    }
    // Chronological order, so a monthly rollup accumulates its days in the
    // order they originally happened rather than directory-listing order.
    eligible.sort();

    for (date, path) in eligible {
        let year_month = date[..MONTH_PREFIX_LEN].to_owned();
        let rollup_path = store.diary_rollup_path(agent_id, &year_month);
        roll_day_into_month(store, agent_id, &date, &year_month, &path, &rollup_path, now)?;
        std::fs::remove_file(&path)?;

        report.rolled_up_days.push(date);
        if !report.touched_months.contains(&year_month) {
            report.touched_months.push(year_month);
        }
    }

    Ok(report)
}

/// Hängt eine einzelne Tagesdatei an ihre monatliche Rollup-Datei an.
///
/// Reine Hilfsfunktion für [`gc`]; entfernt die Quelldatei nicht selbst — das
/// bleibt Sache des Aufrufers, damit eine fehlgeschlagene Rollup-Schreibung
/// nie eine Tagesdatei verliert, ohne dass ihr Inhalt bereits durable ist.
fn roll_day_into_month(
    store: &KnowledgeStore,
    agent_id: &AgentId,
    date: &str,
    year_month: &str,
    day_path: &Path,
    rollup_path: &Path,
    now: Timestamp,
) -> KnowledgeResult<()> {
    let day_content = std::fs::read_to_string(day_path)?;
    let (day_frontmatter, day_body) = parse_frontmatter(&day_content)?;
    let day_entry_count = entry_count(&day_frontmatter).max(1);

    let mut rollup = if rollup_path.is_file() {
        let existing = std::fs::read_to_string(rollup_path)?;
        let (frontmatter, body) = parse_frontmatter(&existing)?;
        KnowledgeArtifact::new(
            diary_artifact_id(agent_id, &format!("rollup/{year_month}")),
            ArtifactKind::DiaryEntry,
            frontmatter,
            body,
        )
    } else {
        let frontmatter =
            Frontmatter::new(agent_id.clone(), day_frontmatter.visibility.clone(), now);
        KnowledgeArtifact::new(
            diary_artifact_id(agent_id, &format!("rollup/{year_month}")),
            ArtifactKind::DiaryEntry,
            frontmatter,
            String::new(),
        )
    };

    rollup.body.push_str(&format!("## {date}\n\n"));
    rollup.body.push_str(&day_body);
    if !day_body.ends_with('\n') {
        rollup.body.push('\n');
    }

    let accumulated = entry_count(&rollup.frontmatter) + day_entry_count;
    rollup.frontmatter.touch(now);
    rollup.frontmatter.extra.insert(
        AGENT_ID_KEY.to_owned(),
        serde_json::Value::String(agent_id.as_str().to_owned()),
    );
    rollup.frontmatter.extra.insert(
        MONTH_KEY.to_owned(),
        serde_json::Value::String(year_month.to_owned()),
    );
    rollup.frontmatter.extra.insert(
        ENTRY_COUNT_KEY.to_owned(),
        serde_json::Value::from(accumulated),
    );

    store.write_artifact(rollup_path, &rollup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_root(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temporary knowledge root");
        root
    }

    fn entry(agent: &AgentId, at: Timestamp, trigger: DiaryTrigger, body: &str) -> DiaryEntry {
        DiaryEntry {
            agent_id: agent.clone(),
            recorded_at: at,
            trigger,
            visibility: VisibilityScope::SelfOnly,
            body: body.to_owned(),
        }
    }

    #[test]
    fn test_append_creates_a_day_file_with_frontmatter_and_header() {
        let root = temporary_root("harw-knowledge-diary-append-new");
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let at = Timestamp::from_second(1_700_000_000).expect("valid timestamp");

        let artifact = append(&store, &entry(&agent, at, DiaryTrigger::EndOfSession, "Erster Eintrag."))
            .expect("append creates a fresh day file");

        assert!(artifact.body.contains("### "));
        assert!(artifact.body.contains("end-of-session"));
        assert!(artifact.body.contains("Erster Eintrag."));
        assert_eq!(
            artifact.frontmatter.extra.get("entry_count").and_then(|v| v.as_u64()),
            Some(1)
        );
        assert!(store.diary_path(&agent, &day_key(at)).is_file());

        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_append_is_additive_within_the_same_day_and_bumps_entry_count() {
        let root = temporary_root("harw-knowledge-diary-append-additive");
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let morning = Timestamp::from_second(1_700_000_000).expect("valid timestamp");
        let evening = morning
            .checked_add(SignedDuration::from_secs(3600))
            .expect("same-day offset");

        append(&store, &entry(&agent, morning, DiaryTrigger::Manual, "Erstens.")).expect("first append");
        let artifact = append(
            &store,
            &entry(&agent, evening, DiaryTrigger::Compaction, "Zweitens."),
        )
        .expect("second append");

        assert!(artifact.body.contains("Erstens."));
        assert!(artifact.body.contains("Zweitens."));
        assert_eq!(
            artifact.frontmatter.extra.get("entry_count").and_then(|v| v.as_u64()),
            Some(2)
        );

        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_read_day_returns_none_for_a_day_that_never_happened() {
        let root = temporary_root("harw-knowledge-diary-read-missing");
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");

        let result = read_day(&store, &agent, "2020-01-01").expect("missing day is not an error");

        assert!(result.is_none());
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_read_day_returns_the_written_artifact() {
        let root = temporary_root("harw-knowledge-diary-read-hit");
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let at = Timestamp::from_second(1_700_000_000).expect("valid timestamp");
        append(&store, &entry(&agent, at, DiaryTrigger::DreamReflection, "Muster erkannt.")).expect("append");

        let day = read_day(&store, &agent, &day_key(at))
            .expect("read succeeds")
            .expect("day exists");

        assert!(day.body.contains("dream-reflection"));
        assert!(day.body.contains("Muster erkannt."));
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_gc_rolls_up_old_days_and_removes_the_source_file() {
        let root = temporary_root("harw-knowledge-diary-gc-rollup");
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let now = Timestamp::from_second(1_700_000_000).expect("valid now");
        let old = now
            .checked_sub(SignedDuration::from_secs(200 * 86_400))
            .expect("far past timestamp");
        append(&store, &entry(&agent, old, DiaryTrigger::EndOfSession, "Alter Eintrag.")).expect("append old day");
        let old_path = store.diary_path(&agent, &day_key(old));
        assert!(old_path.is_file());

        let report = gc(&store, &agent, now, DEFAULT_DIARY_RETENTION_DAYS).expect("gc succeeds");

        assert_eq!(report.rolled_up_days, vec![day_key(old)]);
        assert_eq!(report.touched_months.len(), 1);
        assert!(!old_path.is_file(), "the rolled-up day file must be removed");

        let year_month = day_key(old)[..7].to_owned();
        let rollup_path = store.diary_rollup_path(&agent, &year_month);
        let rollup_content = std::fs::read_to_string(&rollup_path).expect("rollup file exists");
        assert!(rollup_content.contains("Alter Eintrag."));
        assert!(rollup_content.contains(&format!("## {}", day_key(old))));

        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_gc_accumulates_multiple_days_into_the_same_monthly_rollup() {
        let root = temporary_root("harw-knowledge-diary-gc-accumulate");
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let now = Timestamp::from_second(1_700_000_000).expect("valid now");
        let day_a = now
            .checked_sub(SignedDuration::from_secs(200 * 86_400))
            .expect("far past timestamp a");
        let day_b = day_a
            .checked_add(SignedDuration::from_secs(86_400))
            .expect("far past timestamp b, same month");
        append(&store, &entry(&agent, day_a, DiaryTrigger::Manual, "Tag A.")).expect("append day a");
        append(&store, &entry(&agent, day_b, DiaryTrigger::Manual, "Tag B.")).expect("append day b");

        let report = gc(&store, &agent, now, DEFAULT_DIARY_RETENTION_DAYS).expect("gc succeeds");

        assert_eq!(report.rolled_up_days.len(), 2);
        assert_eq!(report.touched_months.len(), 1);
        let year_month = &report.touched_months[0];
        let rollup_content =
            std::fs::read_to_string(store.diary_rollup_path(&agent, year_month)).expect("rollup exists");
        assert!(rollup_content.contains("Tag A."));
        assert!(rollup_content.contains("Tag B."));
        assert_eq!(
            {
                let (frontmatter, _) = parse_frontmatter(&rollup_content).expect("parses");
                frontmatter
                    .extra
                    .get("entry_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default()
            },
            2
        );

        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_gc_leaves_recent_days_untouched() {
        let root = temporary_root("harw-knowledge-diary-gc-recent");
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let now = Timestamp::from_second(1_700_000_000).expect("valid now");
        append(&store, &entry(&agent, now, DiaryTrigger::Manual, "Heute.")).expect("append today");

        let report = gc(&store, &agent, now, DEFAULT_DIARY_RETENTION_DAYS).expect("gc succeeds");

        assert!(report.rolled_up_days.is_empty());
        assert!(store.diary_path(&agent, &day_key(now)).is_file());

        std::fs::remove_dir_all(root).expect("remove temporary root");
    }

    #[test]
    fn test_gc_on_a_missing_agent_directory_is_a_no_op() {
        let root = temporary_root("harw-knowledge-diary-gc-missing");
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("never-wrote-anything");

        let report = gc(&store, &agent, Timestamp::now(), DEFAULT_DIARY_RETENTION_DAYS)
            .expect("gc on a missing directory succeeds as a no-op");

        assert!(report.rolled_up_days.is_empty());
        assert!(report.touched_months.is_empty());
        std::fs::remove_dir_all(root).expect("remove temporary root");
    }
}
