//! Konsolidierung von Fakten (Langzeitgedächtnis v3, Phase 2), siehe
//! `docs/design/memory-v3-ltm.md` §5.3 (Konsolidierung) und §7 (Invarianten).
//!
//! # Verantwortungsbereich
//! Dieses Modul enthält die **reine, modellfreie** Logik der Konsolidierung:
//! - [`plan_consolidation`] erkennt Merge-Kandidaten (gleicher Name, sehr
//!   ähnliche Beschreibung, gleiche Quelle plus gleicher Typ) und
//!   Widersprüche zwischen bestehenden Fakten und neuen Kandidaten aus
//!   `facts/_incoming/`, ohne einen Modellaufruf.
//! - [`apply_plan`] führt einen so entstandenen [`ConsolidationPlan`] gegen
//!   einen [`FactStore`] aus (`write`/`delete`, danach `write_index`).
//! - [`ConsolidationLock`] erzwingt „eine Konsolidierung je Wurzel" (§5.3)
//!   über eine exklusiv angelegte Lock-Datei mit Verfall nach 30 Minuten.
//! - [`steward_prompt`] baut nur den *Text* des Agentenauftrags nach §5.3 —
//!   dieses Modul ruft den `memory-steward`-Agenten nicht selbst auf.
//!
//! Bewusst außerhalb dieses Moduls: die Entscheidung, ob ein bestehender
//! Fakt *gelöscht* statt nur im Vertrauen gesenkt wird, ist laut §5.3 eine
//! Ermessensfrage des `memory-steward`-Agenten („veraltete Fakten senken
//! (`confidence`) oder löschen"), keine deterministisch aus zwei Fakten
//! ableitbare Regel. [`plan_consolidation`] liefert deshalb `deletions`
//! immer leer zurück; ein Aufrufer, der eine Steward-Entscheidung ausführen
//! will, befüllt `ConsolidationPlan::deletions` selbst, bevor er
//! [`apply_plan`] aufruft — das bleibt trotzdem reine Struktur-Manipulation,
//! kein Modellaufruf innerhalb dieses Moduls.
//!
//! # Nebenläufigkeit
//! [`plan_consolidation`] ist eine reine Funktion ohne I/O. [`apply_plan`]
//! nutzt ausschließlich [`FactStore`]s bereits atomare Operationen
//! (`write`/`delete`/`write_index`), serialisiert aber selbst nicht über
//! mehrere Prozesse — dafür ist [`ConsolidationLock`] da: exklusives Anlegen
//! der Lock-Datei über `OpenOptions::create_new` (atomar auf POSIX- und
//! Windows-Dateisystemen), genau ein Erwerber pro Wurzel.
//!
//! # Fehler
//! [`MemoryError::Io`] (Lock-Datei, `FactStore`-Operationen),
//! [`MemoryError::LockContention`] (Lock bereits frisch gehalten); alle
//! übrigen Fehler von [`FactStore::write`]/[`FactStore::delete`]/
//! [`FactStore::write_index`] werden durchgereicht.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{MemoryError, MemoryResult};
use crate::facts::{Fact, FactStore};

// ---------------------------------------------------------------------
// Plan-Datentypen
// ---------------------------------------------------------------------

/// Ergebnis von [`plan_consolidation`]: was zusammengeführt, was
/// unverändert übernommen, was gelöscht und was als Widerspruch markiert
/// werden soll.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConsolidationPlan {
    /// Erkannte Merge-Vorgänge (Buchführung: wer wurde in wen verschmolzen
    /// und warum) — der tatsächliche verschmolzene Fakt liegt in `updates`.
    pub merges: Vec<Merge>,
    /// Fakten, die über [`FactStore::write`] geschrieben werden sollen —
    /// sowohl frisch übernommene Kandidaten als auch Merge-Ergebnisse.
    pub updates: Vec<Fact>,
    /// Namen, die über [`FactStore::delete`] entfernt werden sollen. Von
    /// [`plan_consolidation`] selbst nie befüllt, siehe Moduldoku.
    pub deletions: Vec<String>,
    /// Erkannte Widersprüche, die *nicht* automatisch aufgelöst wurden.
    pub conflicts: Vec<Conflict>,
}

/// Ein einzelner Merge-Vorgang: `sources` (Fakt-Namen) wurden zu `target`
/// (ebenfalls ein Fakt-Name, meist einer der `sources`) verschmolzen.
#[derive(Clone, Debug, PartialEq)]
pub struct Merge {
    /// Name des Fakts, unter dem das Merge-Ergebnis geschrieben wird.
    pub target: String,
    /// Namen aller an diesem Merge beteiligten Fakten (inklusive `target`).
    pub sources: Vec<String>,
    /// Warum diese Fakten als Duplikate erkannt wurden.
    pub reason: MergeReason,
}

/// Begründung, warum zwei Fakten als Duplikate gelten, siehe
/// `docs/design/memory-v3-ltm.md` §5.3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeReason {
    /// Beide Fakten tragen denselben `name`.
    SameName,
    /// Die `description`n sind nach Jaccard-Ähnlichkeit (≥ 0.8 über
    /// normalisierten Wortmengen) praktisch identisch.
    SimilarDescription,
    /// Beide Fakten haben denselben [`crate::facts::FactType`] und
    /// überschneidende `sources` — derselbe Beleg, derselbe Typ, vermutlich
    /// derselbe Sachverhalt.
    SameSourceAndType,
}

/// Ein erkannter Widerspruch zwischen zwei Fakten zum selben Thema, der
/// *nicht* still überschrieben werden darf — siehe `docs/design/
/// memory-v3-ltm.md` §5.3 („Widersprüche ... markieren und auflösen").
#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    /// Name des bestehenden Fakts.
    pub left: String,
    /// Name des neuen Kandidaten.
    pub right: String,
    /// Menschenlesbare Begründung der Heuristik, die den Widerspruch erkannt
    /// hat.
    pub note: String,
}

/// Zähler-Ergebnis eines [`apply_plan`]-Laufs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConsolidationReport {
    /// Anzahl der im Plan enthaltenen Merge-Vorgänge (`plan.merges.len()`).
    pub merged: usize,
    /// Anzahl der über [`FactStore::write`] geschriebenen Fakten.
    pub written: usize,
    /// Anzahl der über [`FactStore::delete`] tatsächlich gelöschten Fakten.
    pub deleted: usize,
    /// Anzahl der im Plan verbliebenen, nicht aufgelösten Widersprüche.
    pub conflicts: usize,
}

// ---------------------------------------------------------------------
// plan_consolidation
// ---------------------------------------------------------------------

/// Schwelle für die Jaccard-Ähnlichkeit zweier `description`n, ab der sie
/// als Merge-Kandidat gelten (§5.3: „sehr ähnliche description").
const DESCRIPTION_JACCARD_THRESHOLD: f64 = 0.8;

/// Mindest-`confidence` beider Fakten, damit eine Tag-Überschneidung als
/// Widerspruch statt als Zufallstreffer gilt (§5.3-Heuristik).
const CONFLICT_MIN_CONFIDENCE: f32 = 0.6;

/// Plant die Konsolidierung von `incoming` (Kandidaten aus
/// `facts/_incoming/`) gegen `existing` (bereits gespeicherte Fakten).
///
/// # Beschreibung
/// Für jeden Kandidaten in `incoming` wird — in dieser Reihenfolge — nach
/// einem bereits gematchten (siehe unten) Fakt in `existing` gesucht:
/// 1. gleicher `name` → [`MergeReason::SameName`];
/// 2. sonst: `description` mit Jaccard-Ähnlichkeit ≥
///    [`DESCRIPTION_JACCARD_THRESHOLD`] über kleingeschriebenen,
///    satzzeichenfreien Wortmengen → [`MergeReason::SimilarDescription`];
/// 3. sonst: gleicher [`crate::facts::FactType`] plus mindestens eine
///    gemeinsame `source` → [`MergeReason::SameSourceAndType`].
///
/// Ein `existing`-Fakt wird höchstens einmal verwendet (erster Treffer in
/// Kandidaten-Reihenfolge gewinnt ihn); spätere Kandidaten sehen ihn dann
/// nicht mehr als Merge-Ziel. Bei einem Treffer entsteht ein [`Merge`]-
/// Eintrag und das Ergebnis von [`merge_facts`] landet in `updates`.
///
/// Findet sich kein Merge-Ziel, wird der Kandidat gegen jeden noch nicht
/// gematchten `existing`-Fakt auf einen Widerspruch geprüft (§5.3-Heuristik:
/// überschneidende `tags`, beide `confidence` ≥
/// [`CONFLICT_MIN_CONFIDENCE`], unterschiedliche erste Zeile des `body`) und
/// unverändert nach `updates` übernommen.
///
/// `deletions` bleibt immer leer — siehe Moduldoku.
///
/// Kein I/O, kein Modellaufruf, deterministisch bei gleicher Eingabe.
#[must_use]
pub fn plan_consolidation(existing: &[Fact], incoming: &[Fact]) -> ConsolidationPlan {
    let mut merges = Vec::new();
    let mut updates = Vec::new();
    let mut conflicts = Vec::new();
    let mut matched: HashSet<String> = HashSet::new();

    for inc in incoming {
        let candidate = find_same_name(existing, &matched, inc)
            .map(|f| (f, MergeReason::SameName))
            .or_else(|| find_similar_description(existing, &matched, inc).map(|f| (f, MergeReason::SimilarDescription)))
            .or_else(|| find_same_source_and_type(existing, &matched, inc).map(|f| (f, MergeReason::SameSourceAndType)));

        if let Some((exist, reason)) = candidate {
            matched.insert(exist.name.clone());
            merges.push(Merge {
                target: exist.name.clone(),
                sources: vec![exist.name.clone(), inc.name.clone()],
                reason,
            });
            updates.push(merge_facts(exist, inc));
            continue;
        }

        for exist in existing {
            if matched.contains(&exist.name) {
                continue;
            }
            if is_conflict(exist, inc) {
                conflicts.push(Conflict {
                    left: exist.name.clone(),
                    right: inc.name.clone(),
                    note: format!(
                        "gemeinsame Tags, aber abweichende erste Zeile: {:?} vs. {:?}",
                        first_line(&exist.body),
                        first_line(&inc.body)
                    ),
                });
            }
        }
        updates.push(inc.clone());
    }

    ConsolidationPlan {
        merges,
        updates,
        deletions: Vec::new(),
        conflicts,
    }
}

/// Sucht in `existing` den ersten noch nicht gematchten Fakt mit
/// `name == inc.name`.
fn find_same_name<'a>(existing: &'a [Fact], matched: &HashSet<String>, inc: &Fact) -> Option<&'a Fact> {
    existing
        .iter()
        .find(|e| !matched.contains(&e.name) && e.name == inc.name)
}

/// Sucht in `existing` den ersten noch nicht gematchten Fakt, dessen
/// `description` zu `inc.description` eine Jaccard-Ähnlichkeit ≥
/// [`DESCRIPTION_JACCARD_THRESHOLD`] hat.
fn find_similar_description<'a>(
    existing: &'a [Fact],
    matched: &HashSet<String>,
    inc: &Fact,
) -> Option<&'a Fact> {
    existing.iter().find(|e| {
        !matched.contains(&e.name)
            && jaccard_similarity(&e.description, &inc.description) >= DESCRIPTION_JACCARD_THRESHOLD
    })
}

/// Sucht in `existing` den ersten noch nicht gematchten Fakt mit gleichem
/// `fact_type` und mindestens einer gemeinsamen `source`.
fn find_same_source_and_type<'a>(
    existing: &'a [Fact],
    matched: &HashSet<String>,
    inc: &Fact,
) -> Option<&'a Fact> {
    existing.iter().find(|e| {
        !matched.contains(&e.name) && e.fact_type == inc.fact_type && slices_overlap(&e.sources, &inc.sources)
    })
}

/// Widerspruchs-Heuristik aus §5.3: überschneidende `tags`, beide
/// `confidence` ≥ [`CONFLICT_MIN_CONFIDENCE`], unterschiedliche erste Zeile
/// des `body`.
fn is_conflict(a: &Fact, b: &Fact) -> bool {
    slices_overlap(&a.tags, &b.tags)
        && a.confidence >= CONFLICT_MIN_CONFIDENCE
        && b.confidence >= CONFLICT_MIN_CONFIDENCE
        && first_line(&a.body) != first_line(&b.body)
}

/// `true`, wenn `a` und `b` mindestens ein gemeinsames Element enthalten
/// (exakter String-Vergleich — genutzt für `sources` und `tags`).
fn slices_overlap(a: &[String], b: &[String]) -> bool {
    a.iter().any(|x| b.contains(x))
}

/// Erste Zeile von `body`, getrimmt — Vergleichsgrundlage der
/// Widerspruchs-Heuristik.
fn first_line(body: &str) -> &str {
    body.lines().next().unwrap_or("").trim()
}

/// Zerlegt `text` in eine kleingeschriebene, satzzeichenfreie Wortmenge
/// (jedes Nicht-alphanumerische-Zeichen wird zum Trenner).
fn normalized_word_set(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Jaccard-Ähnlichkeit zweier Texte über ihre normalisierten Wortmengen
/// (`|A ∩ B| / |A ∪ B|`); `0.0`, wenn beide Wortmengen leer sind.
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let set_a = normalized_word_set(a);
    let set_b = normalized_word_set(b);
    let union = set_a.union(&set_b).count();
    if union == 0 {
        return 0.0;
    }
    let intersection = set_a.intersection(&set_b).count();
    intersection as f64 / union as f64
}

/// Verschmilzt `target` und `other` zu einem Fakt: Der Name bleibt
/// `target.name`; Text (`description`, `body`, `fact_type`) kommt vom Fakt
/// mit dem späteren `updated` („neuerer Text gewinnt"); `confidence` ist das
/// Maximum beider; `sources`/`tags` werden vereinigt (`target` zuerst,
/// danach neue Einträge aus `other` in ihrer ursprünglichen Reihenfolge);
/// `created` ist das Minimum beider (der wahre Ursprung bleibt erhalten);
/// `scope` bleibt `target.scope`.
fn merge_facts(target: &Fact, other: &Fact) -> Fact {
    let newer = if other.updated >= target.updated { other } else { target };
    let mut sources = target.sources.clone();
    for s in &other.sources {
        if !sources.contains(s) {
            sources.push(s.clone());
        }
    }
    let mut tags = target.tags.clone();
    for t in &other.tags {
        if !tags.contains(t) {
            tags.push(t.clone());
        }
    }
    Fact {
        name: target.name.clone(),
        description: newer.description.clone(),
        fact_type: newer.fact_type,
        scope: target.scope,
        created: target.created.min(other.created),
        updated: newer.updated,
        confidence: target.confidence.max(other.confidence),
        sources,
        tags,
        body: newer.body.clone(),
    }
}

// ---------------------------------------------------------------------
// apply_plan
// ---------------------------------------------------------------------

/// Führt einen [`ConsolidationPlan`] gegen `store` aus.
///
/// # Beschreibung
/// Schreibt jeden Fakt aus `plan.updates` über [`FactStore::write`],
/// löscht danach jeden Namen aus `plan.deletions` über
/// [`FactStore::delete`] und regeneriert abschließend explizit den Index
/// über [`FactStore::write_index`] (auch wenn `write`/`delete` ihn bereits
/// selbst regenerieren — nach §5.3 muss `MEMORY.md` nach jeder
/// Konsolidierung sicher aktuell sein, unabhängig davon, ob `updates` und
/// `deletions` leer sind).
///
/// # Errors
/// Jeder Fehler von [`FactStore::write`], [`FactStore::delete`] oder
/// [`FactStore::write_index`] wird unverändert durchgereicht — bereits
/// geschriebene/gelöschte Fakten aus demselben Plan bleiben dabei wirksam
/// (kein Rollback).
pub fn apply_plan(store: &FactStore, plan: &ConsolidationPlan) -> MemoryResult<ConsolidationReport> {
    for fact in &plan.updates {
        store.write(fact)?;
    }
    let mut deleted = 0usize;
    for name in &plan.deletions {
        if store.delete(name)? {
            deleted += 1;
        }
    }
    store.write_index()?;
    Ok(ConsolidationReport {
        merged: plan.merges.len(),
        written: plan.updates.len(),
        deleted,
        conflicts: plan.conflicts.len(),
    })
}

// ---------------------------------------------------------------------
// steward_prompt
// ---------------------------------------------------------------------

/// Baut den Auftragstext für den `memory-steward`-Agenten nach §5.3.
///
/// # Beschreibung
/// Reine Textzusammenstellung — kein Modellaufruf, keine I/O. `index` ist
/// der aktuelle Inhalt von `MEMORY.md`, `incoming` die Kandidaten aus
/// `facts/_incoming/`, `affected` die davon betroffenen bestehenden Fakten
/// (typischerweise die Merge-/Konflikt-Ziele aus einem vorherigen
/// [`plan_consolidation`]-Lauf). Der Text verlangt ausdrücklich: Duplikate
/// verschmelzen statt anhäufen, Widersprüche auflösen, veraltete Fakten im
/// Vertrauen senken oder löschen, `MEMORY.md` neu schreiben.
#[must_use]
pub fn steward_prompt(index: &str, incoming: &[Fact], affected: &[Fact]) -> String {
    let mut out = String::new();
    out.push_str("# Auftrag: Gedächtnis-Konsolidierung (memory-steward)\n\n");
    out.push_str(
        "Du bist der memory-steward-Agent (Design `memory-v3-ltm.md` §5.3). \
         Deine Aufgabe: zusammenführen statt anhäufen.\n\n",
    );
    out.push_str("- Verschmelze Duplikate unter den Kandidaten und den bestehenden Fakten.\n");
    out.push_str(
        "- Löse Widersprüche auf (nutze `contradiction_index`), statt sie stillschweigend zu überschreiben.\n",
    );
    out.push_str(
        "- Senke die `confidence` veralteter Fakten oder lösche sie, wenn sie durch neuere Fakten ersetzt wurden.\n",
    );
    out.push_str("- Schreibe `MEMORY.md` danach neu (`FactStore::write_index`).\n\n");

    out.push_str("## Aktueller Index (MEMORY.md)\n\n");
    out.push_str(index);
    if !index.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');

    out.push_str("## Kandidaten (facts/_incoming/)\n\n");
    if incoming.is_empty() {
        out.push_str("(keine)\n\n");
    } else {
        for fact in incoming {
            out.push_str(&format!(
                "- `{}` ({}): {}\n",
                fact.name,
                fact.fact_type.as_str(),
                fact.description
            ));
        }
        out.push('\n');
    }

    out.push_str("## Betroffene bestehende Fakten\n\n");
    if affected.is_empty() {
        out.push_str("(keine)\n");
    } else {
        for fact in affected {
            out.push_str(&format!(
                "- `{}` ({}, confidence {:.2}): {}\n",
                fact.name,
                fact.fact_type.as_str(),
                fact.confidence,
                fact.description
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------
// ConsolidationLock
// ---------------------------------------------------------------------

/// Name der Lock-Datei unterhalb der Memory-Wurzel.
const LOCK_FILE_NAME: &str = "consolidation.lock";

/// Nach dieser Zeit gilt eine bestehende Lock-Datei als verwaist und darf
/// übernommen werden. Das Design nennt hierfür keine Frist; eine
/// Konsolidierung (§5.3) läuft in der Größenordnung von Minuten (ein
/// One-Shot-Modellaufruf plus ein paar Dateischreibvorgänge) — 30 Minuten
/// sind großzügig genug für einen ungewöhnlich langsamen Lauf und kurz
/// genug, um nach einem Absturz nicht auf Wochen zu blockieren.
const LOCK_MAX_AGE_SECS: i64 = 30 * 60;

/// Exklusiver Lock für genau eine laufende Konsolidierung je Memory-Wurzel,
/// siehe `docs/design/memory-v3-ltm.md` §5.3 („Läuft ... unter einem Lock").
pub struct ConsolidationLock {
    /// Pfad der Lock-Datei (`<root>/consolidation.lock`).
    path: PathBuf,
}

impl ConsolidationLock {
    /// Versucht, die Konsolidierungs-Lock-Datei unter `root` exklusiv
    /// anzulegen.
    ///
    /// # Beschreibung
    /// Legt `<root>/consolidation.lock` atomar an (`OpenOptions::create_new`
    /// — scheitert mit `AlreadyExists`, wenn die Datei bereits existiert),
    /// Inhalt `pid=<PID>\ncreated=<RFC3339>\n`. Existiert die Datei bereits:
    /// ist ihr `created`-Zeitstempel jünger als [`LOCK_MAX_AGE_SECS`],
    /// schlägt der Versuch fehl; ist er älter — oder der Inhalt gar nicht
    /// als Zeitstempel lesbar, was als verwaist zählt — wird die alte Datei
    /// entfernt und der Lock neu angelegt.
    ///
    /// # Errors
    /// [`MemoryError::LockContention`], wenn eine frische Lock-Datei
    /// besteht; [`MemoryError::Io`] bei sonstigen Dateisystemfehlern (Lesen,
    /// Löschen, Anlegen).
    pub fn try_acquire(root: impl AsRef<Path>) -> MemoryResult<Self> {
        let path = root.as_ref().join(LOCK_FILE_NAME);
        match Self::create_exclusive(&path) {
            Ok(()) => return Ok(Self { path }),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(MemoryError::Io { path, source: e });
            }
        }

        if Self::is_stale(&path)? {
            fs::remove_file(&path).map_err(|e| MemoryError::Io {
                path: path.clone(),
                source: e,
            })?;
            Self::create_exclusive(&path).map_err(|e| MemoryError::Io {
                path: path.clone(),
                source: e,
            })?;
            return Ok(Self { path });
        }

        Err(MemoryError::LockContention {
            attempted: "memory consolidation",
        })
    }

    /// Gibt den Lock frei (löscht die Lock-Datei).
    ///
    /// Ein bereits fehlendes File gilt nicht als Fehler (`release` ist damit
    /// idempotent, auch nach einem externen Aufräumen).
    ///
    /// # Errors
    /// [`MemoryError::Io`], wenn die Datei existiert, aber nicht gelöscht
    /// werden kann.
    pub fn release(self) -> MemoryResult<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(MemoryError::Io {
                path: self.path.clone(),
                source: e,
            }),
        }
    }

    /// Legt `path` exklusiv an (`O_EXCL`-artig über `create_new`) und
    /// schreibt PID plus Zeitstempel hinein.
    fn create_exclusive(path: &Path) -> io::Result<()> {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        let created = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "unknown".to_owned());
        writeln!(file, "pid={}", std::process::id())?;
        writeln!(file, "created={created}")?;
        Ok(())
    }

    /// `true`, wenn `path` fehlt, sein Inhalt keinen lesbaren
    /// `created=`-Zeitstempel enthält, oder dieser Zeitstempel mindestens
    /// [`LOCK_MAX_AGE_SECS`] in der Vergangenheit liegt.
    fn is_stale(path: &Path) -> MemoryResult<bool> {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(e) => {
                return Err(MemoryError::Io {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        let created = content
            .lines()
            .find_map(|line| line.strip_prefix("created="))
            .and_then(|raw| OffsetDateTime::parse(raw.trim(), &Rfc3339).ok());
        match created {
            Some(ts) => {
                let age = OffsetDateTime::now_utc() - ts;
                Ok(age.whole_seconds() >= LOCK_MAX_AGE_SECS)
            }
            None => Ok(true),
        }
    }
}

impl Drop for ConsolidationLock {
    /// Sicherheitsnetz, falls [`Self::release`] nicht explizit aufgerufen
    /// wurde: entfernt die Lock-Datei still. `Drop` kann keinen `Result`
    /// liefern — wer Löschfehler sehen will, ruft `release()` auf.
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FactScope, FactStore, FactType};
    use time::Duration;

    fn tmp_root(tag: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-consolidation-{tag}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn fact(name: &str, description: &str, body: &str) -> Fact {
        let now = OffsetDateTime::now_utc();
        Fact {
            name: name.to_owned(),
            description: description.to_owned(),
            fact_type: FactType::Fact,
            scope: FactScope::Project,
            created: now,
            updated: now,
            confidence: 0.8,
            sources: Vec::new(),
            tags: Vec::new(),
            body: body.to_owned(),
        }
    }

    // -- plan_consolidation: SameName ------------------------------------

    #[test]
    fn plan_consolidation_merges_same_name() {
        let mut existing_fact = fact("tui-approval-arming", "alte Beschreibung", "alter Text\n");
        existing_fact.confidence = 0.5;
        existing_fact.sources = vec!["session:old".to_owned()];
        existing_fact.updated = OffsetDateTime::now_utc() - Duration::hours(1);

        let mut incoming_fact = fact("tui-approval-arming", "neue Beschreibung", "neuer Text\n");
        incoming_fact.confidence = 0.9;
        incoming_fact.sources = vec!["session:new".to_owned()];

        let plan = plan_consolidation(std::slice::from_ref(&existing_fact), std::slice::from_ref(&incoming_fact));

        assert_eq!(plan.merges.len(), 1);
        assert_eq!(plan.merges[0].reason, MergeReason::SameName);
        assert_eq!(plan.merges[0].target, "tui-approval-arming");
        assert_eq!(plan.updates.len(), 1);
        let merged = &plan.updates[0];
        assert_eq!(merged.name, "tui-approval-arming");
        assert_eq!(merged.description, "neue Beschreibung", "neuerer Text muss gewinnen");
        assert_eq!(merged.confidence, 0.9, "confidence ist das Maximum");
        assert_eq!(merged.sources.len(), 2, "sources werden vereinigt");
        assert!(plan.conflicts.is_empty());
    }

    // -- plan_consolidation: Jaccard-Schwelle -----------------------------

    #[test]
    fn plan_consolidation_merges_above_jaccard_threshold() {
        let existing_fact = fact(
            "steward-merge-policy",
            "the memory steward always merges duplicate facts before writing",
            "Regel A\n",
        );
        let incoming_fact = fact(
            "duplicate-merge-note",
            "the memory steward always merges duplicate facts before saving",
            "Regel B\n",
        );

        let plan = plan_consolidation(std::slice::from_ref(&existing_fact), std::slice::from_ref(&incoming_fact));

        assert_eq!(plan.merges.len(), 1);
        assert_eq!(plan.merges[0].reason, MergeReason::SimilarDescription);
        assert_eq!(plan.merges[0].target, "steward-merge-policy");
    }

    #[test]
    fn plan_consolidation_does_not_merge_below_jaccard_threshold() {
        let existing_fact = fact(
            "topic-a",
            "the memory steward merges duplicate facts before writing",
            "Regel A\n",
        );
        let incoming_fact = fact(
            "topic-b",
            "completely unrelated text about something totally different",
            "Regel B\n",
        );

        let plan = plan_consolidation(std::slice::from_ref(&existing_fact), std::slice::from_ref(&incoming_fact));

        assert!(plan.merges.is_empty());
        assert_eq!(plan.updates.len(), 1);
        assert_eq!(plan.updates[0].name, "topic-b");
    }

    // -- plan_consolidation: Konflikt statt Überschreiben -----------------

    #[test]
    fn plan_consolidation_flags_conflict_instead_of_overwriting() {
        let mut existing_fact = fact(
            "merge-policy-existing",
            "Beschreibung A ueber Merge-Politik",
            "Policy: always merge duplicates.\n",
        );
        existing_fact.tags = vec!["memory".to_owned(), "policy".to_owned()];
        existing_fact.confidence = 0.9;
        existing_fact.fact_type = FactType::Decision;

        let mut incoming_fact = fact(
            "merge-policy-incoming",
            "Beschreibung B ganz anders formuliert und ohne Ueberschneidung",
            "Policy: never merge duplicates.\n",
        );
        incoming_fact.tags = vec!["policy".to_owned(), "steward".to_owned()];
        incoming_fact.confidence = 0.7;
        incoming_fact.fact_type = FactType::Pitfall;

        let plan = plan_consolidation(std::slice::from_ref(&existing_fact), std::slice::from_ref(&incoming_fact));

        assert!(plan.merges.is_empty(), "widerspruechliche Fakten duerfen nicht gemergt werden");
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].left, "merge-policy-existing");
        assert_eq!(plan.conflicts[0].right, "merge-policy-incoming");
        // Der Kandidat wird trotzdem uebernommen -- die Aufloesung ist
        // Sache des memory-steward-Agenten, keine stille Ueberschreibung.
        assert_eq!(plan.updates.len(), 1);
        assert_eq!(plan.updates[0].name, "merge-policy-incoming");
    }

    // -- apply_plan --------------------------------------------------------

    #[test]
    fn apply_plan_writes_updates_and_rewrites_index() {
        let root = tmp_root("apply-write");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        let f = fact("apply-target", "Beschreibung", "Text\n");

        let plan = ConsolidationPlan {
            merges: vec![Merge {
                target: "apply-target".to_owned(),
                sources: vec!["apply-target".to_owned()],
                reason: MergeReason::SameName,
            }],
            updates: vec![f],
            deletions: Vec::new(),
            conflicts: Vec::new(),
        };

        let report = apply_plan(&store, &plan).unwrap();
        assert_eq!(report.merged, 1);
        assert_eq!(report.written, 1);
        assert_eq!(report.deleted, 0);
        assert_eq!(report.conflicts, 0);

        assert!(store.read("apply-target").unwrap().is_some());
        let index = fs::read_to_string(root.join("MEMORY.md")).unwrap();
        assert!(index.contains("apply-target"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_plan_deletes_and_rewrites_index() {
        let root = tmp_root("apply-delete");
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        store.write(&fact("to-delete", "Beschreibung", "Text\n")).unwrap();
        assert!(store.read("to-delete").unwrap().is_some());

        let plan = ConsolidationPlan {
            merges: Vec::new(),
            updates: Vec::new(),
            deletions: vec!["to-delete".to_owned()],
            conflicts: Vec::new(),
        };
        let report = apply_plan(&store, &plan).unwrap();
        assert_eq!(report.deleted, 1);
        assert!(store.read("to-delete").unwrap().is_none());

        let index = fs::read_to_string(root.join("MEMORY.md")).unwrap();
        assert!(!index.contains("to-delete"));

        let _ = fs::remove_dir_all(&root);
    }

    // -- steward_prompt ------------------------------------------------

    #[test]
    fn steward_prompt_mentions_candidates_and_instructions() {
        let incoming = vec![fact("cand-1", "Neuer Kandidat", "Text\n")];
        let affected = vec![fact("aff-1", "Betroffener Fakt", "Text\n")];
        let prompt = steward_prompt("# Gedaechtnis\n", &incoming, &affected);

        assert!(prompt.contains("zusammenführen statt anhäufen"));
        assert!(prompt.contains("cand-1"));
        assert!(prompt.contains("aff-1"));
        assert!(prompt.contains("MEMORY.md"));
    }

    // -- ConsolidationLock -------------------------------------------------

    #[test]
    fn lock_prevents_second_acquire() {
        let root = tmp_root("lock-double");
        fs::create_dir_all(&root).unwrap();

        let first = ConsolidationLock::try_acquire(&root).unwrap();
        let second = ConsolidationLock::try_acquire(&root);
        assert!(matches!(second, Err(MemoryError::LockContention { .. })));

        first.release().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lock_takes_over_expired_lock() {
        let root = tmp_root("lock-expired");
        fs::create_dir_all(&root).unwrap();
        let lock_path = root.join(LOCK_FILE_NAME);
        let stale_ts = OffsetDateTime::now_utc() - Duration::minutes(45);
        fs::write(
            &lock_path,
            format!("pid=999999\ncreated={}\n", stale_ts.format(&Rfc3339).unwrap()),
        )
        .unwrap();

        let lock = ConsolidationLock::try_acquire(&root).unwrap();
        lock.release().unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lock_release_is_idempotent_after_manual_removal() {
        let root = tmp_root("lock-release-idempotent");
        fs::create_dir_all(&root).unwrap();
        let lock = ConsolidationLock::try_acquire(&root).unwrap();
        fs::remove_file(root.join(LOCK_FILE_NAME)).unwrap();
        assert!(lock.release().is_ok());
        let _ = fs::remove_dir_all(&root);
    }
}
