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
//! # Strukturierte Seitendatei
//! Jeder [`append`] schreibt zusätzlich eine JSON-Zeile
//! ([`DiaryRecord`]: Zeit, Trigger, Text) nach
//! `diary/<agent-id>/<YYYY-MM-DD>.jsonl`. [`read_day_entries`] liest die
//! Einträge strukturiert und fällt auf das Zurückparsen des Markdowns
//! zurück, wenn die Seitendatei fehlt oder nicht zum `entry_count` der
//! Markdown-Datei passt (Altbestand, abgebrochener Append). Das Markdown
//! bleibt die kanonische, indexierte Quelle.
//!
//! # Concurrency
//! [`append`] und [`gc`] serialisieren ihre Read-Modify-Write-Zyklen über
//! [`crate::lock::KnowledgeLock`] auf der Tages- bzw. Rollup-Datei —
//! prozessübergreifend und zwischen Threads.
//!
//! # Fehler
//! Jeder fehlschlagende Pfad liefert [`crate::error::KnowledgeError`] /
//! [`crate::error::KnowledgeResult`].

use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::KnowledgeResult;
use crate::lock::KnowledgeLock;
use crate::store::{KnowledgeStore, parse_frontmatter};
use crate::visibility::{AgentId, VisibilityScope};

/// Why a diary entry was appended rather than edited in place.
///
/// Serialisiert als [`Self::label`] (`end-of-session`, `compaction`,
/// `dream-reflection`, `manual`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

    /// Umkehrung von [`Self::label`]; `None` für unbekannte Bezeichnungen.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        [
            Self::EndOfSession,
            Self::Compaction,
            Self::DreamReflection,
            Self::Manual,
        ]
        .into_iter()
        .find(|trigger| trigger.label() == label.trim())
    }
}

/// Ein Diary-Eintrag in strukturierter Form (eine Zeile der
/// `.jsonl`-Seitendatei bzw. aus dem Markdown zurückgewonnen).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiaryRecord {
    /// Zeitpunkt des Eintrags.
    pub recorded_at: Timestamp,
    /// Auslöser.
    pub trigger: DiaryTrigger,
    /// Eintragstext.
    pub text: String,
}

/// Ein Diary-Tag in strukturierter Form ([`read_day_entries`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiaryDay {
    /// Halter des Tagebuchs.
    pub agent_id: AgentId,
    /// Datum `YYYY-MM-DD`.
    pub date: String,
    /// Sichtbarkeit der Tagesdatei (aus dem Markdown-Frontmatter).
    pub visibility: VisibilityScope,
    /// Einträge in Schreibreihenfolge.
    pub entries: Vec<DiaryRecord>,
    /// `true`, wenn die Einträge aus der `.jsonl`-Seitendatei stammen;
    /// `false` beim Rückfall auf das Markdown.
    pub structured: bool,
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
/// - [`crate::error::KnowledgeError::Io`]\: Lese-/Schreib-/Rename-Fehler,
///   `TimedOut`, wenn die Tagessperre nicht rechtzeitig frei wird.
/// - [`crate::error::KnowledgeError::MalformedFrontmatter`] /
///   [`crate::error::KnowledgeError::Frontmatter`]: eine bestehende
///   Tagesdatei ließ sich nicht als gültiges Frontmatter-Dokument parsen.
///
/// # Nebenläufigkeit
/// Der Read-Modify-Write-Zyklus läuft unter [`KnowledgeLock`] auf der
/// Tagesdatei; gleichzeitige `append`-Aufrufe (auch aus anderen Prozessen)
/// verlieren keinen Eintrag. Die Sperre ist nicht wiedereintrittsfähig.
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
    let _lock = KnowledgeLock::for_target(&path)?;

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
        .insert(DATE_KEY.to_owned(), serde_json::Value::String(date.clone()));
    artifact.frontmatter.extra.insert(
        ENTRY_COUNT_KEY.to_owned(),
        serde_json::Value::from(next_count),
    );

    store.write_artifact(&path, &artifact)?;
    // Erst nach dem kanonischen Markdown: scheitert dieser Schritt, passt die
    // Seitendatei nicht mehr zum `entry_count` und `read_day_entries` fällt
    // auf das Markdown zurück — es geht nichts verloren.
    append_record(
        &store.diary_log_path(&entry.agent_id, &date),
        &DiaryRecord {
            recorded_at: entry.recorded_at,
            trigger: entry.trigger,
            text: entry.body.clone(),
        },
    )?;
    Ok(artifact)
}

/// Hängt eine JSON-Zeile an eine `.jsonl`-Seitendatei an.
fn append_record(path: &Path, record: &DiaryRecord) -> KnowledgeResult<()> {
    use std::io::Write;

    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// Liest einen Diary-Tag strukturiert.
///
/// # Beschreibung
/// Bevorzugt die `.jsonl`-Seitendatei; sie gilt nur, wenn ihre Zeilenzahl
/// dem `entry_count` der Markdown-Datei entspricht und jede Zeile gültig ist.
/// Sonst werden die `### HH:MM:SS — <trigger>`-Abschnitte des Markdowns
/// zurückgeparst (Altbestand); ein unbekannter Trigger wird dabei zu
/// [`DiaryTrigger::Manual`]. Reiner Lesezugriff.
///
/// # Rückgabe
/// `None`, wenn es für den Tag keine Markdown-Datei gibt.
///
/// # Fehler
/// Wie [`read_day`].
pub fn read_day_entries(
    store: &KnowledgeStore,
    agent_id: &AgentId,
    date: &str,
) -> KnowledgeResult<Option<DiaryDay>> {
    let Some(artifact) = read_day(store, agent_id, date)? else {
        return Ok(None);
    };
    let expected = entry_count(&artifact.frontmatter);
    let structured = read_records(&store.diary_log_path(agent_id, date))
        .filter(|records| records.len() as u64 == expected && expected > 0);
    let (entries, structured) = match structured {
        Some(records) => (records, true),
        None => (parse_markdown_entries(date, &artifact), false),
    };
    Ok(Some(DiaryDay {
        agent_id: agent_id.clone(),
        date: date.to_owned(),
        visibility: artifact.frontmatter.visibility.clone(),
        entries,
        structured,
    }))
}

/// Liest alle Zeilen einer `.jsonl`-Seitendatei; `None` bei fehlender Datei
/// oder irgendeiner ungültigen Zeile (dann gilt das Markdown).
fn read_records(path: &Path) -> Option<Vec<DiaryRecord>> {
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<DiaryRecord>(line).ok())
        .collect()
}

/// Zerlegt einen Tages-Body in `### HH:MM:SS — <trigger>`-Einträge.
fn parse_markdown_entries(date: &str, artifact: &KnowledgeArtifact) -> Vec<DiaryRecord> {
    let mut entries: Vec<DiaryRecord> = Vec::new();
    for line in artifact.body.lines() {
        let header = line
            .strip_prefix("### ")
            .and_then(|rest| rest.split_once(" — "));
        match header {
            Some((time, trigger)) => {
                let recorded_at = format!("{date}T{}Z", time.trim())
                    .parse::<Timestamp>()
                    .unwrap_or(artifact.frontmatter.created_at);
                entries.push(DiaryRecord {
                    recorded_at,
                    trigger: DiaryTrigger::from_label(trigger).unwrap_or(DiaryTrigger::Manual),
                    text: String::new(),
                });
            }
            None => {
                if let Some(entry) = entries.last_mut()
                    && (!entry.text.is_empty() || !line.trim().is_empty())
                {
                    entry.text.push_str(line);
                    entry.text.push('\n');
                }
            }
        }
    }
    for entry in &mut entries {
        let trimmed = entry.text.trim_end().to_owned();
        entry.text = trimmed;
    }
    entries
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
    Ok(Some(store.read_artifact(
        &path,
        id,
        ArtifactKind::DiaryEntry,
    )?))
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
/// Jeder Tag wird unter [`KnowledgeLock`] auf Tages- und Rollup-Datei
/// übernommen (Reihenfolge Tag → Rollup); parallele `append`-/`gc`-Aufrufe
/// sind damit sicher.
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
        // Sperrreihenfolge Tag → Rollup; `append` hält nur die Tagessperre.
        let _day_lock = KnowledgeLock::for_target(&path)?;
        let _rollup_lock = KnowledgeLock::for_target(&rollup_path)?;
        if !path.is_file() {
            // Ein paralleler `gc` hat den Tag inzwischen übernommen.
            continue;
        }
        roll_day_into_month(
            store,
            agent_id,
            &date,
            &year_month,
            &path,
            &rollup_path,
            now,
        )?;
        let day_log = store.diary_log_path(agent_id, &date);
        if day_log.is_file() {
            let lines = std::fs::read_to_string(&day_log)?;
            append_raw(&store.diary_rollup_log_path(agent_id, &year_month), &lines)?;
        }
        std::fs::remove_file(&path)?;
        if day_log.is_file() {
            std::fs::remove_file(&day_log)?;
        }

        report.rolled_up_days.push(date);
        if !report.touched_months.contains(&year_month) {
            report.touched_months.push(year_month);
        }
    }

    Ok(report)
}

/// Hängt rohe `.jsonl`-Zeilen an eine Seitendatei an (Rollup).
fn append_raw(path: &Path, lines: &str) -> KnowledgeResult<()> {
    use std::io::Write;

    if lines.trim().is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(lines.as_bytes())?;
    if !lines.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    Ok(())
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

/// Obergrenze eines automatischen Eintrags ([`record`]) in Bytes; längere
/// Texte werden zeichensicher gekürzt und mit `…` markiert.
pub const MAX_AUTOMATIC_ENTRY_BYTES: usize = 4 * 1024;

/// Prüft, dass eine Agent-Id als einzelne Pfadkomponente taugt
/// (`diary/<agent-id>/…`).
///
/// # Fehler
/// [`crate::error::KnowledgeError::Io`] mit `InvalidInput` für leere Ids,
/// `.`/`..`, Pfadtrenner oder Steuerzeichen.
pub fn validate_agent_id(agent_id: &AgentId) -> KnowledgeResult<()> {
    let raw = agent_id.as_str();
    let unsafe_id = raw.trim().is_empty()
        || raw == "."
        || raw == ".."
        || raw.chars().any(|c| c == '/' || c == '\\' || c.is_control());
    if unsafe_id {
        return Err(invalid_input(format!("ungültige Agent-Id '{raw}'")));
    }
    Ok(())
}

fn invalid_input(detail: String) -> crate::error::KnowledgeError {
    crate::error::KnowledgeError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        detail,
    ))
}

/// Kürzt `text` zeichensicher auf höchstens `max_bytes` Bytes (inklusive
/// der `…`-Markierung).
#[must_use]
pub fn truncate_entry_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    const MARK: &str = "…";
    let mut end = max_bytes.saturating_sub(MARK.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARK}", text[..end].trim_end())
}

/// Schreibt einen automatischen Diary-Eintrag (Compaction, Sitzungsende,
/// Traum-Reflexion).
///
/// # Beschreibung
/// Gemeinsamer Schreibweg der Automatik (Plan D3): trimmt `text`, kürzt ihn
/// auf [`MAX_AUTOMATIC_ENTRY_BYTES`] und hängt ihn über [`append`] mit
/// Sichtbarkeit `OperatorOnly` an das Tagebuch von `agent_id` an — der
/// Operator liest es mit `/diary`, der Agent selbst nur über das
/// Lese-Werkzeug `diary.read`, das die Agent-Id statt der Sichtbarkeit
/// prüft.
///
/// # Fehler
/// - [`crate::error::KnowledgeError::Io`] mit `InvalidInput`: leerer Text
///   oder ungültige Agent-Id ([`validate_agent_id`]).
/// - alle Fehler von [`append`].
pub fn record(
    store: &KnowledgeStore,
    agent_id: &AgentId,
    trigger: DiaryTrigger,
    text: &str,
    now: Timestamp,
) -> KnowledgeResult<KnowledgeArtifact> {
    validate_agent_id(agent_id)?;
    let text = text.trim();
    if text.is_empty() {
        return Err(invalid_input(format!(
            "leerer Diary-Eintrag ({})",
            trigger.label()
        )));
    }
    append(
        store,
        &DiaryEntry {
            agent_id: agent_id.clone(),
            recorded_at: now,
            trigger,
            visibility: VisibilityScope::OperatorOnly,
            body: truncate_entry_text(text, MAX_AUTOMATIC_ENTRY_BYTES),
        },
    )
}

/// Schreib-API für die Traum-Reflexion (D5): [`record`] mit
/// [`DiaryTrigger::DreamReflection`].
///
/// # Fehler
/// Wie [`record`].
pub fn record_dream_reflection(
    store: &KnowledgeStore,
    agent_id: &AgentId,
    text: &str,
    now: Timestamp,
) -> KnowledgeResult<KnowledgeArtifact> {
    record(store, agent_id, DiaryTrigger::DreamReflection, text, now)
}

/// Alle Agenten mit einem Diary-Verzeichnis, sortiert.
///
/// # Fehler
/// [`crate::error::KnowledgeError::Io`] beim Auflisten; ein fehlendes
/// `diary/`-Verzeichnis ergibt eine leere Liste.
pub fn list_agents(store: &KnowledgeStore) -> KnowledgeResult<Vec<AgentId>> {
    let dir = store.root().join("diary");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut agents = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            let agent = AgentId::new(name);
            if validate_agent_id(&agent).is_ok() {
                agents.push(agent);
            }
        }
    }
    agents.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(agents)
}

/// Alle Tagesdaten (`YYYY-MM-DD`) eines Agenten, aufsteigend. Aufgeräumte
/// Tage (Rollup) zählen nicht dazu.
///
/// # Fehler
/// [`crate::error::KnowledgeError::Io`] beim Auflisten.
pub fn list_days(store: &KnowledgeStore, agent_id: &AgentId) -> KnowledgeResult<Vec<String>> {
    validate_agent_id(agent_id)?;
    let dir = store.root().join("diary").join(agent_id.as_str());
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut days = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        let is_day = path.is_file() && path.extension().is_some_and(|ext| ext == "md");
        if !is_day {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            && is_day_key(stem)
        {
            days.push(stem.to_owned());
        }
    }
    days.sort();
    Ok(days)
}

/// `true` für ein wohlgeformtes `YYYY-MM-DD` aus Ziffern.
#[must_use]
pub fn is_day_key(raw: &str) -> bool {
    raw.len() == DAY_STEM_LEN
        && raw.char_indices().all(|(index, c)| match index {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        })
}

/// Liest alle Tage eines Agenten im Bereich `from..=to` strukturiert
/// ([`read_day_entries`]), aufsteigend.
///
/// # Beschreibung
/// Reiner Lesezugriff; prüft keine Sichtbarkeit — das ist Sache des
/// Aufrufers (`DiaryDay::visibility`). Aufgeräumte Tage (Rollup) sind nicht
/// enthalten.
///
/// # Fehler
/// - `InvalidInput`, wenn `from`/`to` kein `YYYY-MM-DD` ist oder die
///   Agent-Id ungültig ist.
/// - wie [`read_day_entries`].
pub fn read_range(
    store: &KnowledgeStore,
    agent_id: &AgentId,
    from: &str,
    to: &str,
) -> KnowledgeResult<Vec<DiaryDay>> {
    for bound in [from, to] {
        if !is_day_key(bound) {
            return Err(invalid_input(format!(
                "ungültiges Datum '{bound}' (YYYY-MM-DD)"
            )));
        }
    }
    let mut days = Vec::new();
    for date in list_days(store, agent_id)? {
        if date.as_str() < from || date.as_str() > to {
            continue;
        }
        if let Some(day) = read_day_entries(store, agent_id, &date)? {
            days.push(day);
        }
    }
    Ok(days)
}

/// Ergebnis von [`maintain`]: ein [`DiaryGcReport`] je Agent mit Wirkung.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiaryMaintenanceReport {
    /// Angewandtes Aufbewahrungsfenster in Tagen.
    pub retention_days: i64,
    /// Nur Agenten, bei denen mindestens ein Tag aufgeräumt wurde.
    pub agents: Vec<DiaryGcReport>,
}

impl DiaryMaintenanceReport {
    /// Summe aller aufgeräumten Tage.
    #[must_use]
    pub fn rolled_up_days(&self) -> usize {
        self.agents
            .iter()
            .map(|report| report.rolled_up_days.len())
            .sum()
    }
}

/// Diary-Wartung für alle Agenten (Plan D3/D5): Tage älter als
/// `retention_days` wandern per [`gc`] in die Monats-Rollups.
///
/// # Beschreibung
/// Ruft [`gc`] für jeden Agenten aus [`list_agents`]; `retention_days`
/// stammt aus `[knowledge.diary] retention_days`
/// (Default [`DEFAULT_DIARY_RETENTION_DAYS`]). Ein Fehler bei einem Agenten
/// bricht die Wartung ab (die bereits übernommenen Tage bleiben konsistent,
/// weil [`gc`] je Tag atomar arbeitet).
///
/// # Fehler
/// Wie [`gc`] und [`list_agents`].
pub fn maintain(
    store: &KnowledgeStore,
    now: Timestamp,
    retention_days: i64,
) -> KnowledgeResult<DiaryMaintenanceReport> {
    let mut report = DiaryMaintenanceReport {
        retention_days: retention_days.max(0),
        agents: Vec::new(),
    };
    for agent in list_agents(store)? {
        let agent_report = gc(store, &agent, now, retention_days)?;
        if !agent_report.rolled_up_days.is_empty() {
            report.agents.push(agent_report);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn temporary_root(label: &str) -> TestResult<PathBuf> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(crate::test_support::ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root)
            .map_err(crate::test_support::ctx("create temporary knowledge root"))?;
        Ok(root)
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
    fn test_append_creates_a_day_file_with_frontmatter_and_header() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-append-new")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let at = Timestamp::from_second(1_700_000_000)
            .map_err(crate::test_support::ctx("valid timestamp"))?;

        let artifact = append(
            &store,
            &entry(&agent, at, DiaryTrigger::EndOfSession, "Erster Eintrag."),
        )
        .map_err(crate::test_support::ctx("append creates a fresh day file"))?;

        assert!(artifact.body.contains("### "));
        assert!(artifact.body.contains("end-of-session"));
        assert!(artifact.body.contains("Erster Eintrag."));
        assert_eq!(
            artifact
                .frontmatter
                .extra
                .get("entry_count")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert!(store.diary_path(&agent, &day_key(at)).is_file());

        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_append_is_additive_within_the_same_day_and_bumps_entry_count() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-append-additive")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let morning = Timestamp::from_second(1_700_000_000)
            .map_err(crate::test_support::ctx("valid timestamp"))?;
        let evening = morning
            .checked_add(SignedDuration::from_secs(3600))
            .map_err(crate::test_support::ctx("same-day offset"))?;

        append(
            &store,
            &entry(&agent, morning, DiaryTrigger::Manual, "Erstens."),
        )
        .map_err(crate::test_support::ctx("first append"))?;
        let artifact = append(
            &store,
            &entry(&agent, evening, DiaryTrigger::Compaction, "Zweitens."),
        )
        .map_err(crate::test_support::ctx("second append"))?;

        assert!(artifact.body.contains("Erstens."));
        assert!(artifact.body.contains("Zweitens."));
        assert_eq!(
            artifact
                .frontmatter
                .extra
                .get("entry_count")
                .and_then(|v| v.as_u64()),
            Some(2)
        );

        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_read_day_entries_prefers_the_structured_side_file() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-structured")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let at = Timestamp::from_second(1_700_000_000)
            .map_err(crate::test_support::ctx("valid timestamp"))?;
        // Ein Text, der wie ein Markdown-Eintragskopf aussieht, zerlegt nur
        // das Rückparsen — die Seitendatei bleibt exakt.
        let tricky = "Erstens.\n### 00:00:00 — manual\nkein Kopf";
        append(&store, &entry(&agent, at, DiaryTrigger::Compaction, tricky))?;
        let day = read_day_entries(&store, &agent, &day_key(at))?
            .ok_or(TestError::Missing("day exists"))?;
        assert!(day.structured);
        assert_eq!(day.visibility, VisibilityScope::SelfOnly);
        assert_eq!(
            day.entries,
            vec![DiaryRecord {
                recorded_at: at,
                trigger: DiaryTrigger::Compaction,
                text: tricky.to_owned(),
            }]
        );
        let line = std::fs::read_to_string(store.diary_log_path(&agent, &day_key(at)))?;
        assert!(line.contains("\"trigger\":\"compaction\""), "{line}");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn test_read_day_entries_falls_back_to_markdown_for_legacy_days() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-legacy")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let at = Timestamp::from_second(1_700_000_000)
            .map_err(crate::test_support::ctx("valid timestamp"))?;
        append(&store, &entry(&agent, at, DiaryTrigger::Manual, "Eins."))?;
        append(
            &store,
            &entry(&agent, at, DiaryTrigger::EndOfSession, "Zwei."),
        )?;
        // Altbestand: keine Seitendatei.
        std::fs::remove_file(store.diary_log_path(&agent, &day_key(at)))?;
        let day = read_day_entries(&store, &agent, &day_key(at))?
            .ok_or(TestError::Missing("day exists"))?;
        assert!(!day.structured);
        assert_eq!(day.entries.len(), 2);
        assert_eq!(day.entries[0].text, "Eins.");
        assert_eq!(day.entries[0].recorded_at, at);
        assert_eq!(day.entries[1].trigger, DiaryTrigger::EndOfSession);
        assert_eq!(read_day_entries(&store, &agent, "2020-01-01")?, None);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn test_parallel_appends_lose_no_entry() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-parallel")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let at = Timestamp::from_second(1_700_000_000)
            .map_err(crate::test_support::ctx("valid timestamp"))?;
        let handles: Vec<_> = (0..6)
            .map(|index| {
                let store = store.clone();
                let entry = entry(&agent, at, DiaryTrigger::Manual, &format!("E{index}"));
                std::thread::spawn(move || append(&store, &entry).map(|_| ()))
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .map_err(|_| TestError::Unexpected("thread panicked".to_owned()))??;
        }
        let day = read_day_entries(&store, &agent, &day_key(at))?
            .ok_or(TestError::Missing("day exists"))?;
        assert!(day.structured);
        assert_eq!(day.entries.len(), 6);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn test_read_day_returns_none_for_a_day_that_never_happened() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-read-missing")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");

        let result = read_day(&store, &agent, "2020-01-01")
            .map_err(crate::test_support::ctx("missing day is not an error"))?;

        assert!(result.is_none());
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_read_day_returns_the_written_artifact() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-read-hit")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let at = Timestamp::from_second(1_700_000_000)
            .map_err(crate::test_support::ctx("valid timestamp"))?;
        append(
            &store,
            &entry(&agent, at, DiaryTrigger::DreamReflection, "Muster erkannt."),
        )
        .map_err(crate::test_support::ctx("append"))?;

        let day = read_day(&store, &agent, &day_key(at))
            .map_err(crate::test_support::ctx("read succeeds"))?
            .ok_or(TestError::Missing("day exists"))?;

        assert!(day.body.contains("dream-reflection"));
        assert!(day.body.contains("Muster erkannt."));
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_gc_rolls_up_old_days_and_removes_the_source_file() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-gc-rollup")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let now =
            Timestamp::from_second(1_700_000_000).map_err(crate::test_support::ctx("valid now"))?;
        let old = now
            .checked_sub(SignedDuration::from_secs(200 * 86_400))
            .map_err(crate::test_support::ctx("far past timestamp"))?;
        append(
            &store,
            &entry(&agent, old, DiaryTrigger::EndOfSession, "Alter Eintrag."),
        )
        .map_err(crate::test_support::ctx("append old day"))?;
        let old_path = store.diary_path(&agent, &day_key(old));
        assert!(old_path.is_file());

        let report = gc(&store, &agent, now, DEFAULT_DIARY_RETENTION_DAYS)
            .map_err(crate::test_support::ctx("gc succeeds"))?;

        assert_eq!(report.rolled_up_days, vec![day_key(old)]);
        assert_eq!(report.touched_months.len(), 1);
        assert!(
            !old_path.is_file(),
            "the rolled-up day file must be removed"
        );

        let year_month = day_key(old)[..7].to_owned();
        let rollup_path = store.diary_rollup_path(&agent, &year_month);
        let rollup_content = std::fs::read_to_string(&rollup_path)
            .map_err(crate::test_support::ctx("rollup file exists"))?;
        assert!(rollup_content.contains("Alter Eintrag."));
        assert!(rollup_content.contains(&format!("## {}", day_key(old))));
        assert!(!store.diary_log_path(&agent, &day_key(old)).is_file());
        let rollup_log = std::fs::read_to_string(store.diary_rollup_log_path(&agent, &year_month))
            .map_err(crate::test_support::ctx("rollup side file exists"))?;
        assert!(rollup_log.contains("Alter Eintrag."));

        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_gc_accumulates_multiple_days_into_the_same_monthly_rollup() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-gc-accumulate")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let now =
            Timestamp::from_second(1_700_000_000).map_err(crate::test_support::ctx("valid now"))?;
        let day_a = now
            .checked_sub(SignedDuration::from_secs(200 * 86_400))
            .map_err(crate::test_support::ctx("far past timestamp a"))?;
        let day_b = day_a
            .checked_add(SignedDuration::from_secs(86_400))
            .map_err(crate::test_support::ctx("far past timestamp b, same month"))?;
        append(
            &store,
            &entry(&agent, day_a, DiaryTrigger::Manual, "Tag A."),
        )
        .map_err(crate::test_support::ctx("append day a"))?;
        append(
            &store,
            &entry(&agent, day_b, DiaryTrigger::Manual, "Tag B."),
        )
        .map_err(crate::test_support::ctx("append day b"))?;

        let report = gc(&store, &agent, now, DEFAULT_DIARY_RETENTION_DAYS)
            .map_err(crate::test_support::ctx("gc succeeds"))?;

        assert_eq!(report.rolled_up_days.len(), 2);
        assert_eq!(report.touched_months.len(), 1);
        let year_month = &report.touched_months[0];
        let rollup_content = std::fs::read_to_string(store.diary_rollup_path(&agent, year_month))
            .map_err(crate::test_support::ctx("rollup exists"))?;
        assert!(rollup_content.contains("Tag A."));
        assert!(rollup_content.contains("Tag B."));
        assert_eq!(
            {
                let (frontmatter, _) = parse_frontmatter(&rollup_content)
                    .map_err(crate::test_support::ctx("parses"))?;
                frontmatter
                    .extra
                    .get("entry_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_default()
            },
            2
        );

        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_gc_leaves_recent_days_untouched() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-gc-recent")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("agent-1");
        let now =
            Timestamp::from_second(1_700_000_000).map_err(crate::test_support::ctx("valid now"))?;
        append(&store, &entry(&agent, now, DiaryTrigger::Manual, "Heute."))
            .map_err(crate::test_support::ctx("append today"))?;

        let report = gc(&store, &agent, now, DEFAULT_DIARY_RETENTION_DAYS)
            .map_err(crate::test_support::ctx("gc succeeds"))?;

        assert!(report.rolled_up_days.is_empty());
        assert!(store.diary_path(&agent, &day_key(now)).is_file());

        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_gc_on_a_missing_agent_directory_is_a_no_op() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-gc-missing")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("never-wrote-anything");

        let report = gc(
            &store,
            &agent,
            Timestamp::now(),
            DEFAULT_DIARY_RETENTION_DAYS,
        )
        .map_err(crate::test_support::ctx(
            "gc on a missing directory succeeds as a no-op",
        ))?;

        assert!(report.rolled_up_days.is_empty());
        assert!(report.touched_months.is_empty());
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_record_writes_trimmed_bounded_operator_visible_entries() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-record")?;
        let store = KnowledgeStore::new(&root);
        let agent = AgentId::new("explorer");
        let now =
            Timestamp::from_second(1_700_000_000).map_err(crate::test_support::ctx("valid now"))?;
        let long = "ä".repeat(MAX_AUTOMATIC_ENTRY_BYTES);
        record(&store, &agent, DiaryTrigger::Compaction, &long, now)
            .map_err(crate::test_support::ctx("record compaction"))?;
        record_dream_reflection(&store, &agent, "  Traum  ", now)
            .map_err(crate::test_support::ctx("record reflection"))?;

        let day = read_day_entries(&store, &agent, &day_key(now))
            .map_err(crate::test_support::ctx("read day"))?
            .ok_or(TestError::Missing("day"))?;
        assert!(day.structured);
        assert_eq!(day.visibility, VisibilityScope::OperatorOnly);
        assert_eq!(day.entries.len(), 2);
        assert_eq!(day.entries[0].trigger, DiaryTrigger::Compaction);
        assert!(day.entries[0].text.len() <= MAX_AUTOMATIC_ENTRY_BYTES);
        assert!(day.entries[0].text.ends_with('…'));
        assert_eq!(day.entries[1].trigger, DiaryTrigger::DreamReflection);
        assert_eq!(day.entries[1].text, "Traum");

        assert!(record(&store, &agent, DiaryTrigger::EndOfSession, "   ", now).is_err());
        assert!(
            record(
                &store,
                &AgentId::new("../x"),
                DiaryTrigger::Manual,
                "x",
                now
            )
            .is_err()
        );
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }

    #[test]
    fn test_read_range_and_maintain_cover_every_agent() -> TestResult {
        let root = temporary_root("harw-knowledge-diary-maintain")?;
        let store = KnowledgeStore::new(&root);
        let day = 86_400;
        let now = Timestamp::from_second(1_700_000_000 + 200 * day)
            .map_err(crate::test_support::ctx("valid now"))?;
        let old =
            Timestamp::from_second(1_700_000_000).map_err(crate::test_support::ctx("valid old"))?;
        let recent = Timestamp::from_second(1_700_000_000 + 199 * day)
            .map_err(crate::test_support::ctx("valid recent"))?;
        for name in ["a", "b"] {
            let agent = AgentId::new(name);
            record(&store, &agent, DiaryTrigger::Manual, "alt", old)
                .map_err(crate::test_support::ctx("old entry"))?;
            record(&store, &agent, DiaryTrigger::Manual, "neu", recent)
                .map_err(crate::test_support::ctx("recent entry"))?;
        }
        let a = AgentId::new("a");
        let all = read_range(&store, &a, "2000-01-01", "2999-12-31")
            .map_err(crate::test_support::ctx("range"))?;
        assert_eq!(all.len(), 2);
        let only_recent = read_range(&store, &a, &day_key(recent), &day_key(now))
            .map_err(crate::test_support::ctx("recent range"))?;
        assert_eq!(only_recent.len(), 1);
        assert!(read_range(&store, &a, "gestern", "heute").is_err());

        let report = maintain(&store, now, DEFAULT_DIARY_RETENTION_DAYS)
            .map_err(crate::test_support::ctx("maintain"))?;
        assert_eq!(report.agents.len(), 2);
        assert_eq!(report.rolled_up_days(), 2);
        assert_eq!(
            list_days(&store, &a).map_err(crate::test_support::ctx("days"))?,
            vec![day_key(recent)]
        );
        assert_eq!(
            list_agents(&store).map_err(crate::test_support::ctx("agents"))?,
            vec![a.clone(), AgentId::new("b")]
        );
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove temporary root"))?;
        Ok(())
    }
}
