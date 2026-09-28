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
//! - [`ConsolidationBaseline`] hält einen Content-Digest je Fakt
//!   (`state.json` unterhalb der Fakt-Wurzel) und liefert per
//!   [`ConsolidationBaseline::diff`] die seit dem letzten Lauf geänderten
//!   Fakt-Namen — **kein** Git-Aufruf (§5.3 nennt „den Diff seit der letzten
//!   Baseline" als Steward-Eingabe; dieses Modul bildet das über einen
//!   eigenen Digest ab statt über `git diff`, siehe [`fact_digest`]).
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
//! mehrere Prozesse — dafür ist [`ConsolidationLock`] da: der je Erwerb
//! eindeutige Lock-Inhalt (`pid=`/`seq=`/`created=`) wird vollständig in eine
//! Tempdatei geschrieben und erst danach über `hard_link` auf die Lock-Datei
//! verknüpft (atomar auf POSIX- und Windows-Dateisystemen, scheitert mit
//! `AlreadyExists`, wenn die Lock-Datei bereits besteht; ohne harte Links
//! Rückfall auf `create_new`). Eine gerade freigegebene Lock-Datei wird neu
//! angelegt statt gelöscht, eine verwaiste nur nach Inhaltsvergleich
//! aufgebrochen, und die Freigabe löscht nur die Datei mit dem eigenen
//! Inhalt — genau ein Erwerber pro Wurzel. Restfenster: zwei gleichzeitige
//! Aufbrecher derselben verwaisten Datei plus ein dritter Erwerber im selben
//! Moment (siehe `ConsolidationLock::break_stale`); eine beratende OS-Sperre
//! schlösse das, ist aber keine Abhängigkeit dieses Crates.
//!
//! # Fehler
//! [`MemoryError::Io`] (Lock-Datei, `FactStore`-Operationen),
//! [`MemoryError::LockContention`] (Lock bereits frisch gehalten); alle
//! übrigen Fehler von [`FactStore::write`]/[`FactStore::delete`]/
//! [`FactStore::write_index`] werden durchgereicht.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
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
            .or_else(|| {
                find_similar_description(existing, &matched, inc)
                    .map(|f| (f, MergeReason::SimilarDescription))
            })
            .or_else(|| {
                find_same_source_and_type(existing, &matched, inc)
                    .map(|f| (f, MergeReason::SameSourceAndType))
            });

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
fn find_same_name<'a>(
    existing: &'a [Fact],
    matched: &HashSet<String>,
    inc: &Fact,
) -> Option<&'a Fact> {
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
        !matched.contains(&e.name)
            && e.fact_type == inc.fact_type
            && slices_overlap(&e.sources, &inc.sources)
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
    let newer = if other.updated >= target.updated {
        other
    } else {
        target
    };
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
pub fn apply_plan(
    store: &FactStore,
    plan: &ConsolidationPlan,
) -> MemoryResult<ConsolidationReport> {
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
/// [`plan_consolidation`]-Lauf), `changed_since_baseline` die Fakt-Namen aus
/// [`ConsolidationBaseline::diff`] (§5.3: „den Diff seit der letzten
/// Baseline"). Der Text verlangt ausdrücklich: Duplikate verschmelzen statt
/// anhäufen, Widersprüche auflösen, veraltete Fakten im Vertrauen senken oder
/// löschen, `MEMORY.md` neu schreiben.
#[must_use]
pub fn steward_prompt(
    index: &str,
    incoming: &[Fact],
    affected: &[Fact],
    changed_since_baseline: &[String],
) -> String {
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
        out.push_str("(keine)\n\n");
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
        out.push('\n');
    }

    out.push_str("## Diff seit letzter Baseline\n\n");
    if changed_since_baseline.is_empty() {
        out.push_str("(keine Änderungen seit der letzten Baseline)\n");
    } else {
        for name in changed_since_baseline {
            out.push_str(&format!("- `{name}`\n"));
        }
    }
    out
}

// ---------------------------------------------------------------------
// ConsolidationBaseline — Diff seit der letzten Konsolidierung
// ---------------------------------------------------------------------

/// Name der Baseline-Manifest-Datei unterhalb einer Fakt-Wurzel.
///
/// Bewusst `state.json` (statt z. B. `baseline.json`): der Aufrufer soll
/// genau eine Zustandsdatei je Fakt-Wurzel pflegen müssen, keine zwei.
const BASELINE_FILE_NAME: &str = "state.json";

/// Content-Digest-Manifest der Fakt-Wurzel zum Zeitpunkt der letzten
/// Konsolidierung (§5.3: „den Diff seit der letzten Baseline").
///
/// # Beschreibung
/// Ersetzt eine git-basierte Baseline durch ein reines Digest-Manifest:
/// [`Self::from_facts`] baut eine Baseline aus dem aktuellen Fakten-Bestand,
/// [`Self::read`]/[`Self::write`] persistieren sie unter
/// `<root>/state.json`, [`Self::diff`] liefert die Namen aller Fakten, deren
/// Digest von der Baseline abweicht (neu oder geändert). Kein `git`-Aufruf,
/// keine externe Hash-Crate — `harw-memory` hat keine, siehe [`fact_digest`].
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ConsolidationBaseline {
    /// Fakt-Name → Content-Digest (siehe [`fact_digest`]), sortiert für
    /// stabile Serialisierung.
    #[serde(default)]
    pub digests: BTreeMap<String, String>,
}

impl ConsolidationBaseline {
    /// Baut eine Baseline aus dem aktuellen Zustand von `facts`.
    #[must_use]
    pub fn from_facts(facts: &[Fact]) -> Self {
        Self {
            digests: facts
                .iter()
                .map(|f| (f.name.clone(), fact_digest(f)))
                .collect(),
        }
    }

    /// Liest die Baseline unterhalb `root` (`<root>/state.json`).
    ///
    /// Fehlt die Datei, liefert dies eine leere Baseline (erster Lauf) statt
    /// eines Fehlers — [`Self::diff`] meldet dann jeden bestehenden Fakt als
    /// „geändert", was für den allerersten Konsolidierungslauf korrekt ist.
    ///
    /// # Errors
    /// [`MemoryError::Io`] bei sonstigen Lesefehlern; [`MemoryError::Serde`]
    /// bei fehlerhaftem JSON.
    pub fn read(root: impl AsRef<Path>) -> MemoryResult<Self> {
        let path = root.as_ref().join(BASELINE_FILE_NAME);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| MemoryError::Serde {
                context: "consolidation baseline lesen",
                source: e,
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(MemoryError::Io { path, source: e }),
        }
    }

    /// Schreibt die Baseline atomar unterhalb `root` (`<root>/state.json`,
    /// Tempdatei + `rename`).
    ///
    /// # Errors
    /// [`MemoryError::Io`] bei Schreib-/Rename-Fehlern.
    pub fn write(&self, root: impl AsRef<Path>) -> MemoryResult<()> {
        let path = root.as_ref().join(BASELINE_FILE_NAME);
        let payload = serde_json::to_vec_pretty(self).map_err(|e| MemoryError::Serde {
            context: "consolidation baseline schreiben",
            source: e,
        })?;
        harw_fsutil::write_atomic(
            &path,
            &payload,
            harw_fsutil::AtomicWriteOptions::with_mode(0o600),
        )
        .map_err(|e| MemoryError::Io { path, source: e })
    }

    /// Liefert die Namen aller Fakten in `current`, deren Digest von dieser
    /// Baseline abweicht oder die neu hinzugekommen sind — sortiert für
    /// deterministische Ausgabe.
    ///
    /// Gelöschte Fakten (in der Baseline, aber nicht mehr in `current`)
    /// werden hier bewusst nicht gemeldet: ein gelöschter Fakt braucht keine
    /// Steward-Aufmerksamkeit mehr, er ist bereits fort.
    #[must_use]
    pub fn diff(&self, current: &[Fact]) -> Vec<String> {
        let mut changed: Vec<String> = current
            .iter()
            .filter(|fact| {
                self.digests
                    .get(&fact.name)
                    .is_none_or(|digest| *digest != fact_digest(fact))
            })
            .map(|fact| fact.name.clone())
            .collect();
        changed.sort();
        changed
    }
}

/// Deterministischer Content-Digest eines Fakts (FNV-1a, 64-bit) über alle
/// inhaltsrelevanten Felder (`description`, `body`, `fact_type`,
/// `confidence`, `sources`, `tags` — nicht `created`/`updated`, damit ein
/// reiner Zeitstempel-Touch keinen Digest-Wechsel auslöst).
///
/// Kein `git`-Aufruf, keine externe Hash-Crate (`harw-memory` hat keine
/// `sha2`/`blake3`-Abhängigkeit) — für reine Änderungserkennung reicht ein
/// nicht-kryptographischer Digest.
#[must_use]
pub fn fact_digest(fact: &Fact) -> String {
    let mut hasher = Fnv1a64::new();
    hasher.write(fact.description.as_bytes());
    hasher.write(b"\x1f");
    hasher.write(fact.body.as_bytes());
    hasher.write(b"\x1f");
    hasher.write(fact.fact_type.as_str().as_bytes());
    hasher.write(b"\x1f");
    hasher.write(&fact.confidence.to_bits().to_le_bytes());
    hasher.write(b"\x1f");
    for source in &fact.sources {
        hasher.write(source.as_bytes());
        hasher.write(b",");
    }
    hasher.write(b"\x1f");
    for tag in &fact.tags {
        hasher.write(tag.as_bytes());
        hasher.write(b",");
    }
    format!("{:016x}", hasher.finish())
}

/// Minimaler FNV-1a-64-Hasher für [`fact_digest`]. Kein
/// `std::hash::Hasher`-Trait nötig — dieses Modul braucht nur
/// `write(bytes)`/`finish() -> u64`, keine Interoperabilität mit
/// `HashMap`/`Hash`.
struct Fnv1a64(u64);

impl Fnv1a64 {
    /// FNV-1a-64-Offset-Basis.
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    /// FNV-1a-64-Prime.
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    const fn finish(&self) -> u64 {
        self.0
    }
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

/// Obergrenze für Namenskollisionen (`AlreadyExists`) beim Anlegen der
/// Tempdatei in [`ConsolidationLock::place_exclusive`]. Kollidiert der
/// Name trotz PID plus prozessweitem Zähler mehrfach, ist das Verzeichnis
/// vermutlich fremdbeschreibbar; nach dieser Zahl an Versuchen wird
/// abgebrochen statt endlos zu wiederholen.
const LOCK_TEMP_MAX_ATTEMPTS: u32 = 8;

/// Obergrenze der Anlegeversuche in [`ConsolidationLock::try_acquire`]. Ein
/// weiterer Versuch folgt nur, wenn die Lock-Datei zwischen gescheitertem
/// Anlegen und Einstufen verschwunden ist oder als verwaist aufgebrochen
/// wurde; wer danach immer noch verliert, steht in echtem Wettbewerb.
const LOCK_ACQUIRE_MAX_ATTEMPTS: u32 = 4;

/// Prozessweiter Zähler für eindeutige Tempdatei-Namen und Lock-Inhalte
/// (`seq=`) von [`ConsolidationLock`].
static LOCK_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Baut einen eindeutigen Tempdatei-Namen
/// (`.consolidation.lock.<pid>.<zähler>.<versuch>.tmp`) für
/// [`ConsolidationLock::place_exclusive`] und
/// [`ConsolidationLock::break_stale`].
fn temp_lock_name(attempt: u32) -> String {
    let counter = LOCK_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!(".{LOCK_FILE_NAME}.{pid}.{counter}.{attempt}.tmp")
}

/// Einstufung einer bestehenden Lock-Datei durch
/// [`ConsolidationLock::inspect`].
enum LockState {
    /// Die Datei ist inzwischen verschwunden (der Halter hat freigegeben):
    /// erneut anlegen, nichts löschen.
    Missing,
    /// Die Datei wird frisch gehalten.
    Fresh,
    /// Die Datei ist verwaist; trägt den gelesenen Inhalt, damit
    /// [`ConsolidationLock::break_stale`] nur genau diese Datei entfernt.
    Stale(Vec<u8>),
}

/// Exklusiver Lock für genau eine laufende Konsolidierung je Memory-Wurzel,
/// siehe `docs/design/memory-v3-ltm.md` §5.3 („Läuft ... unter einem Lock").
pub struct ConsolidationLock {
    /// Pfad der Lock-Datei (`<root>/consolidation.lock`).
    path: PathBuf,
    /// Eigener, je Erwerb eindeutiger Inhalt der Lock-Datei — freigegeben
    /// wird nur eine Datei mit genau diesem Inhalt.
    content: Vec<u8>,
    /// `true` nach [`Self::release`]; `Drop` räumt dann nicht erneut auf.
    released: bool,
}

impl ConsolidationLock {
    /// Versucht, die Konsolidierungs-Lock-Datei unter `root` exklusiv
    /// anzulegen.
    ///
    /// # Beschreibung
    /// Legt `<root>/consolidation.lock` an: der je Erwerb eindeutige Inhalt
    /// (`pid=<PID>\nseq=<Zähler>\ncreated=<RFC3339>\n`) wird zuerst
    /// vollständig in eine Tempdatei im selben Verzeichnis geschrieben, die
    /// Tempdatei danach über `hard_link` auf die Lock-Datei verknüpft — das
    /// scheitert mit `AlreadyExists`, wenn die Lock-Datei bereits existiert,
    /// aber ohne das Zeitfenster, in dem ein konkurrierender Leser eine
    /// leere oder nur teilweise geschriebene Lock-Datei sähe (Dateisysteme
    /// ohne harte Links: Rückfall auf `create_new`). Existiert die Datei
    /// bereits, wird sie eingestuft: ist sie inzwischen verschwunden (der
    /// Halter hat gerade freigegeben), folgt ein neuer Anlegeversuch, ohne
    /// etwas zu löschen; ist ihr `created`-Zeitstempel jünger als
    /// [`LOCK_MAX_AGE_SECS`], schlägt der Versuch fehl; ist er älter, wird
    /// die alte Datei nach Inhaltsvergleich aufgebrochen und der Lock neu
    /// angelegt. Ist der Inhalt gar nicht als Zeitstempel lesbar (fremdes
    /// Format, Korruption), zählt stattdessen die Änderungszeit (`mtime`)
    /// der Datei, statt sie sofort als verwaist einzustufen — fail-closed
    /// statt fail-open.
    ///
    /// # Errors
    /// [`MemoryError::LockContention`], wenn eine frische Lock-Datei
    /// besteht oder ein anderer Erwerber nach [`LOCK_ACQUIRE_MAX_ATTEMPTS`]
    /// Versuchen weiter zuvorkommt; [`MemoryError::Io`] bei sonstigen
    /// Dateisystemfehlern (Lesen, Umbenennen, Anlegen, Verknüpfen).
    pub fn try_acquire(root: impl AsRef<Path>) -> MemoryResult<Self> {
        let path = root.as_ref().join(LOCK_FILE_NAME);
        for _ in 0..LOCK_ACQUIRE_MAX_ATTEMPTS {
            match Self::create_exclusive(&path) {
                Ok(content) => {
                    return Ok(Self {
                        path,
                        content,
                        released: false,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(MemoryError::Io { path, source: e }),
            }
            match Self::inspect(&path)? {
                // Der Halter hat zwischen Anlegen und Einstufen freigegeben:
                // erneut anlegen statt die womöglich schon wieder frische
                // Datei eines dritten Erwerbers zu löschen.
                LockState::Missing => {}
                LockState::Fresh => break,
                LockState::Stale(seen) => Self::break_stale(&path, &seen)?,
            }
        }
        Err(MemoryError::LockContention {
            attempted: "memory consolidation",
        })
    }

    /// Gibt den Lock frei (löscht die Lock-Datei).
    ///
    /// Gelöscht wird nur eine Lock-Datei mit genau dem eigenen Inhalt: wurde
    /// der eigene Lock nach [`LOCK_MAX_AGE_SECS`] als verwaist aufgebrochen
    /// und von einem anderen Erwerber neu angelegt, bleibt dessen Datei
    /// unangetastet. Ein bereits fehlendes File gilt nicht als Fehler
    /// (`release` ist damit idempotent, auch nach einem externen Aufräumen).
    ///
    /// # Errors
    /// [`MemoryError::Io`], wenn die Datei existiert, aber nicht gelesen oder
    /// gelöscht werden kann.
    pub fn release(mut self) -> MemoryResult<()> {
        self.released = true;
        self.remove_if_owned().map_err(|e| MemoryError::Io {
            path: self.path.clone(),
            source: e,
        })
    }

    /// Löscht die Lock-Datei, sofern sie noch den eigenen Inhalt trägt;
    /// fehlt sie oder gehört sie inzwischen einem anderen Halter, bleibt sie
    /// unangetastet.
    fn remove_if_owned(&self) -> io::Result<()> {
        match fs::read(&self.path) {
            Ok(current) if current == self.content => {}
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Legt `path` exklusiv mit einem je Erwerb eindeutigen Inhalt (PID,
    /// prozessweiter Zähler, Zeitstempel) an und liefert diesen Inhalt —
    /// [`Self::remove_if_owned`] erkennt daran die eigene Lock-Datei.
    fn create_exclusive(path: &Path) -> io::Result<Vec<u8>> {
        let created = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| "unknown".to_owned());
        let pid = std::process::id();
        let seq = LOCK_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let content = format!("pid={pid}\nseq={seq}\ncreated={created}\n").into_bytes();
        Self::place_exclusive(path, &content)?;
        Ok(content)
    }

    /// Legt `path` exklusiv mit `content` an: schreibt `content` vollständig
    /// in eine Tempdatei im selben Verzeichnis und verknüpft diese erst
    /// danach über `hard_link` auf `path`.
    ///
    /// Anders als `create_new` direkt auf `path` gibt es dadurch keinen
    /// Zwischenzustand, in dem `path` schon existiert, aber noch leer oder
    /// nur teilweise beschrieben ist — ein konkurrierender `try_acquire`
    /// sieht `path` entweder gar nicht oder vollständig beschrieben.
    /// `hard_link` scheitert mit `AlreadyExists`, wenn `path` schon besteht.
    ///
    /// Dateisysteme ohne harte Links (FAT/exFAT, manche SMB-/FUSE-Mounts)
    /// lassen `hard_link` mit einem anderen Fehler scheitern; dann fällt
    /// diese Funktion auf [`Self::create_direct`] zurück. Das dort kurz
    /// sichtbare leere `path` stuft [`Self::inspect`] über die frische
    /// `mtime` als gehalten ein, nicht als verwaist.
    fn place_exclusive(path: &Path, content: &[u8]) -> io::Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));

        let mut last_err: Option<io::Error> = None;
        for attempt in 0..LOCK_TEMP_MAX_ATTEMPTS {
            let temp_path = dir.join(temp_lock_name(attempt));
            let mut file = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    last_err = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            };

            if let Err(e) = file.write_all(content) {
                drop(file);
                let _ = fs::remove_file(&temp_path);
                return Err(e);
            }
            if let Err(e) = file.sync_all() {
                drop(file);
                let _ = fs::remove_file(&temp_path);
                return Err(e);
            }
            drop(file);

            // Erst jetzt, mit vollständig geschriebenem Inhalt, wird die
            // Tempdatei auf `path` verknüpft — der einzige Schritt, der
            // `path` sichtbar macht.
            let link_result = fs::hard_link(&temp_path, path);
            let _ = fs::remove_file(&temp_path);
            return match link_result {
                Err(e) if e.kind() != io::ErrorKind::AlreadyExists => {
                    Self::create_direct(path, content)
                }
                other => other,
            };
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "consolidation lock: keine eindeutige Tempdatei anlegbar",
            )
        }))
    }

    /// Rückfall von [`Self::place_exclusive`] für Dateisysteme ohne harte
    /// Links: `create_new` direkt auf `path`, danach Inhalt schreiben und
    /// `fsync`; scheitert das Schreiben, wird die eigene Datei entfernt.
    fn create_direct(path: &Path, content: &[u8]) -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let written = file.write_all(content).and_then(|()| file.sync_all());
        if let Err(e) = written {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(e);
        }
        Ok(())
    }

    /// Stuft die bestehende Lock-Datei `path` ein: [`LockState::Missing`],
    /// wenn sie inzwischen fehlt (der Halter hat freigegeben — früher galt
    /// das als verwaist und ließ `try_acquire` die womöglich schon wieder
    /// frische Datei eines dritten Erwerbers löschen);
    /// [`LockState::Stale`], wenn ihr `created=`-Zeitstempel mindestens
    /// [`LOCK_MAX_AGE_SECS`] zurückliegt; sonst [`LockState::Fresh`].
    ///
    /// Enthält `path` **keinen** lesbaren `created=`-Zeitstempel (fremdes
    /// Format, Korruption oder eine über [`Self::create_direct`] gerade
    /// angelegte, noch unvollständige Datei), zählt die Änderungszeit
    /// (`mtime`) der Datei: erst wenn auch sie mindestens
    /// [`LOCK_MAX_AGE_SECS`] zurückliegt, gilt die Datei als verwaist
    /// (fail-closed statt fail-open).
    fn inspect(path: &Path) -> MemoryResult<LockState> {
        let content = match fs::read(path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(LockState::Missing),
            Err(e) => {
                return Err(MemoryError::Io {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        let created = String::from_utf8_lossy(&content)
            .lines()
            .find_map(|line| line.strip_prefix("created="))
            .and_then(|raw| OffsetDateTime::parse(raw.trim(), &Rfc3339).ok());
        let stamp = match created {
            Some(ts) => ts,
            None => match fs::metadata(path).and_then(|m| m.modified()) {
                Ok(mtime) => OffsetDateTime::from(mtime),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(LockState::Missing),
                // Ohne lesbare mtime lässt sich das Alter nicht bestimmen —
                // fail-closed heißt hier: nicht als verwaist einstufen, damit
                // ein unparsbarer Inhalt nicht zu vorzeitigem Aufbrechen führt.
                Err(_) => return Ok(LockState::Fresh),
            },
        };
        if (OffsetDateTime::now_utc() - stamp).whole_seconds() >= LOCK_MAX_AGE_SECS {
            Ok(LockState::Stale(content))
        } else {
            Ok(LockState::Fresh)
        }
    }

    /// Bricht die verwaiste Lock-Datei `path` auf, deren Inhalt beim
    /// Einstufen `seen` war.
    ///
    /// Statt `remove_file` (das eine inzwischen von einem anderen Aufbrecher
    /// neu angelegte, frische Datei ebenso löschen würde) wird `path` atomar
    /// auf einen eindeutigen Tempnamen umbenannt und dessen Inhalt mit
    /// `seen` verglichen: nur bei Gleichheit war es die verwaiste Datei;
    /// sonst wird der fremde Lock sofort über [`Self::place_exclusive`]
    /// zurückgelegt. Fehlt `path` bereits (ein anderer Erwerber hat
    /// aufgebrochen oder freigegeben), ist das kein Fehler. Restfenster:
    /// legt ein dritter Erwerber `path` zwischen Umbenennen und Zurücklegen
    /// neu an, scheitert das Zurücklegen mit `AlreadyExists` und zwei Halter
    /// bestehen — nur bei zwei gleichzeitigen Aufbrechern derselben
    /// verwaisten Datei plus einem dritten Erwerber im selben Moment.
    fn break_stale(path: &Path, seen: &[u8]) -> MemoryResult<()> {
        let io_err = |p: &Path, source| MemoryError::Io {
            path: p.to_path_buf(),
            source,
        };
        let grave = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(temp_lock_name(0));
        match fs::rename(path, &grave) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_err(path, e)),
        }
        let moved = fs::read(&grave).map_err(|e| io_err(grave.as_path(), e))?;
        let _ = fs::remove_file(&grave);
        if moved.as_slice() == seen {
            return Ok(());
        }
        match Self::place_exclusive(path, &moved) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(io_err(path, e)),
            _ => Ok(()),
        }
    }
}

impl Drop for ConsolidationLock {
    /// Sicherheitsnetz, falls [`Self::release`] nicht explizit aufgerufen
    /// wurde: entfernt die eigene Lock-Datei still (nur bei eigenem Inhalt,
    /// siehe [`Self::release`]). Nach `release` tut `Drop` nichts mehr — ein
    /// zweites Löschen träfe sonst womöglich schon die Datei des nächsten
    /// Halters. `Drop` kann keinen `Result` liefern — wer Löschfehler sehen
    /// will, ruft `release()` auf.
    fn drop(&mut self) {
        if !self.released {
            let _ = self.remove_if_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FactScope, FactStore, FactType};
    use crate::test_support::{TestError, TestResult, ctx};
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

        let plan = plan_consolidation(
            std::slice::from_ref(&existing_fact),
            std::slice::from_ref(&incoming_fact),
        );

        assert_eq!(plan.merges.len(), 1);
        assert_eq!(plan.merges[0].reason, MergeReason::SameName);
        assert_eq!(plan.merges[0].target, "tui-approval-arming");
        assert_eq!(plan.updates.len(), 1);
        let merged = &plan.updates[0];
        assert_eq!(merged.name, "tui-approval-arming");
        assert_eq!(
            merged.description, "neue Beschreibung",
            "neuerer Text muss gewinnen"
        );
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

        let plan = plan_consolidation(
            std::slice::from_ref(&existing_fact),
            std::slice::from_ref(&incoming_fact),
        );

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

        let plan = plan_consolidation(
            std::slice::from_ref(&existing_fact),
            std::slice::from_ref(&incoming_fact),
        );

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

        let plan = plan_consolidation(
            std::slice::from_ref(&existing_fact),
            std::slice::from_ref(&incoming_fact),
        );

        assert!(
            plan.merges.is_empty(),
            "widerspruechliche Fakten duerfen nicht gemergt werden"
        );
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
    fn apply_plan_writes_updates_and_rewrites_index() -> TestResult {
        let root = tmp_root("apply-write");
        let store = FactStore::open(&root, FactScope::Project).map_err(ctx("store öffnen"))?;
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

        let report = apply_plan(&store, &plan).map_err(ctx("plan anwenden"))?;
        assert_eq!(report.merged, 1);
        assert_eq!(report.written, 1);
        assert_eq!(report.deleted, 0);
        assert_eq!(report.conflicts, 0);

        assert!(
            store
                .read("apply-target")
                .map_err(ctx("fakt lesen"))?
                .is_some()
        );
        let index = fs::read_to_string(root.join("MEMORY.md")).map_err(ctx("index lesen"))?;
        assert!(index.contains("apply-target"));

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn apply_plan_deletes_and_rewrites_index() -> TestResult {
        let root = tmp_root("apply-delete");
        let store = FactStore::open(&root, FactScope::Project).map_err(ctx("store öffnen"))?;
        store
            .write(&fact("to-delete", "Beschreibung", "Text\n"))
            .map_err(ctx("fakt schreiben"))?;
        assert!(
            store
                .read("to-delete")
                .map_err(ctx("fakt lesen"))?
                .is_some()
        );

        let plan = ConsolidationPlan {
            merges: Vec::new(),
            updates: Vec::new(),
            deletions: vec!["to-delete".to_owned()],
            conflicts: Vec::new(),
        };
        let report = apply_plan(&store, &plan).map_err(ctx("plan anwenden"))?;
        assert_eq!(report.deleted, 1);
        assert!(
            store
                .read("to-delete")
                .map_err(ctx("fakt lesen"))?
                .is_none()
        );

        let index = fs::read_to_string(root.join("MEMORY.md")).map_err(ctx("index lesen"))?;
        assert!(!index.contains("to-delete"));

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    // -- steward_prompt ------------------------------------------------

    #[test]
    fn steward_prompt_mentions_candidates_and_instructions() {
        let incoming = vec![fact("cand-1", "Neuer Kandidat", "Text\n")];
        let affected = vec![fact("aff-1", "Betroffener Fakt", "Text\n")];
        let changed = vec!["geaenderter-fakt".to_owned()];
        let prompt = steward_prompt("# Gedaechtnis\n", &incoming, &affected, &changed);

        assert!(prompt.contains("zusammenführen statt anhäufen"));
        assert!(prompt.contains("cand-1"));
        assert!(prompt.contains("aff-1"));
        assert!(prompt.contains("geaenderter-fakt"));
        assert!(prompt.contains("MEMORY.md"));
    }

    #[test]
    fn steward_prompt_reports_no_changes_since_baseline() {
        let prompt = steward_prompt("# Gedaechtnis\n", &[], &[], &[]);
        assert!(prompt.contains("keine Änderungen seit der letzten Baseline"));
    }

    // -- ConsolidationBaseline / fact_digest -------------------------------

    #[test]
    fn fact_digest_is_stable_for_identical_content() {
        let a = fact("stable", "Beschreibung", "Text\n");
        let b = fact("stable", "Beschreibung", "Text\n");
        assert_eq!(fact_digest(&a), fact_digest(&b));
    }

    #[test]
    fn fact_digest_changes_when_body_changes() {
        let a = fact("changed", "Beschreibung", "Text A\n");
        let b = fact("changed", "Beschreibung", "Text B\n");
        assert_ne!(fact_digest(&a), fact_digest(&b));
    }

    #[test]
    fn fact_digest_ignores_created_and_updated_timestamps() {
        let mut a = fact("touch-only", "Beschreibung", "Text\n");
        let mut b = a.clone();
        b.updated += Duration::days(5);
        b.created -= Duration::days(5);
        assert_eq!(
            fact_digest(&a),
            fact_digest(&b),
            "reines Zeitstempel-Touch darf den Digest nicht ändern"
        );
        a.body.push_str("geaendert");
        assert_ne!(fact_digest(&a), fact_digest(&b));
    }

    #[test]
    fn baseline_diff_reports_new_and_changed_facts_only() {
        let unchanged = fact("unchanged", "Beschreibung", "Text\n");
        let changed_old = fact("changed", "alte Beschreibung", "alter Text\n");
        let baseline = ConsolidationBaseline::from_facts(&[unchanged.clone(), changed_old]);

        let changed_new = fact("changed", "neue Beschreibung", "neuer Text\n");
        let brand_new = fact("brand-new", "Frisch", "Frischer Text\n");
        let current = vec![unchanged, changed_new, brand_new];

        let diff = baseline.diff(&current);
        assert_eq!(diff, vec!["brand-new".to_owned(), "changed".to_owned()]);
    }

    #[test]
    fn baseline_diff_against_empty_baseline_reports_every_fact() {
        let baseline = ConsolidationBaseline::default();
        let current = vec![fact("only", "Beschreibung", "Text\n")];
        assert_eq!(baseline.diff(&current), vec!["only".to_owned()]);
    }

    #[test]
    fn baseline_read_write_round_trips() -> TestResult {
        let root = tmp_root("baseline-roundtrip");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;

        let empty = ConsolidationBaseline::read(&root).map_err(ctx("baseline lesen"))?;
        assert!(
            empty.digests.is_empty(),
            "fehlende Baseline-Datei liefert leere Baseline"
        );

        let facts = vec![fact("a", "A", "Text A\n"), fact("b", "B", "Text B\n")];
        let baseline = ConsolidationBaseline::from_facts(&facts);
        baseline.write(&root).map_err(ctx("baseline schreiben"))?;

        let read_back = ConsolidationBaseline::read(&root).map_err(ctx("baseline lesen"))?;
        assert_eq!(read_back, baseline);
        assert_eq!(read_back.digests.len(), 2);

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    // -- ConsolidationLock -------------------------------------------------

    #[test]
    fn lock_prevents_second_acquire() -> TestResult {
        let root = tmp_root("lock-double");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;

        let first = ConsolidationLock::try_acquire(&root).map_err(ctx("erstes lock"))?;
        let second = ConsolidationLock::try_acquire(&root);
        assert!(matches!(second, Err(MemoryError::LockContention { .. })));

        first.release().map_err(ctx("lock freigeben"))?;
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn lock_takes_over_expired_lock() -> TestResult {
        let root = tmp_root("lock-expired");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;
        let lock_path = root.join(LOCK_FILE_NAME);
        let stale_ts = OffsetDateTime::now_utc() - Duration::minutes(45);
        fs::write(
            &lock_path,
            format!(
                "pid=999999\ncreated={}\n",
                stale_ts
                    .format(&Rfc3339)
                    .map_err(ctx("zeitstempel formatieren"))?
            ),
        )
        .map_err(ctx("lock-datei schreiben"))?;

        let lock = ConsolidationLock::try_acquire(&root).map_err(ctx("lock erwerben"))?;
        lock.release().map_err(ctx("lock freigeben"))?;
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    /// Regressionstest: das Zeitfenster zwischen dem Anlegen der Lock-Datei
    /// und dem vollständigen Schreiben ihres Inhalts durfte den zweiten
    /// Erwerber nicht dazu bringen, die Datei als verwaist einzustufen und
    /// aufzubrechen (sonst liefen zwei Konsolidierungen gleichzeitig) — eine
    /// leere Datei mit frischem `mtime` simuliert genau dieses Fenster.
    #[test]
    fn lock_treats_freshly_created_empty_lock_file_as_contention_not_stale() -> TestResult {
        let root = tmp_root("lock-empty-window");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;
        fs::write(root.join(LOCK_FILE_NAME), b"").map_err(ctx("leere lock-datei anlegen"))?;

        let result = ConsolidationLock::try_acquire(&root);
        assert!(
            matches!(result, Err(MemoryError::LockContention { .. })),
            "eine frische, aber leere Lock-Datei darf nicht als verwaist aufgebrochen werden"
        );

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    /// Wie oben, aber mit unparsbarem statt leerem Inhalt (fremdes Format) —
    /// auch das darf bei frischem `mtime` nicht als verwaist gelten.
    #[test]
    fn lock_keeps_unparsable_but_fresh_lock_file() -> TestResult {
        let root = tmp_root("lock-unparsable-fresh");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;
        fs::write(root.join(LOCK_FILE_NAME), b"kein pid=/created= Format\n")
            .map_err(ctx("lock-datei schreiben"))?;

        let result = ConsolidationLock::try_acquire(&root);
        assert!(
            matches!(result, Err(MemoryError::LockContention { .. })),
            "unparsbarer, aber frischer Inhalt darf nicht als verwaist aufgebrochen werden"
        );

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    /// Ein unparsbarer Inhalt darf weiterhin aufgebrochen werden, sobald die
    /// `mtime` der Datei alt genug ist — der Fallback in `is_stale` ist
    /// fail-closed nach Zeit, nicht fail-open für immer.
    #[test]
    fn lock_breaks_unparsable_lock_file_once_old_enough() -> TestResult {
        let root = tmp_root("lock-unparsable-old");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;
        let lock_path = root.join(LOCK_FILE_NAME);
        fs::write(&lock_path, b"kein pid=/created= Format\n").map_err(ctx("lock-datei schreiben"))?;

        let old_mtime =
            std::time::SystemTime::now() - std::time::Duration::from_secs((LOCK_MAX_AGE_SECS + 60) as u64);
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&lock_path)
            .map_err(ctx("lock-datei öffnen"))?;
        file.set_modified(old_mtime).map_err(ctx("mtime setzen"))?;
        drop(file);

        let lock = ConsolidationLock::try_acquire(&root).map_err(ctx("lock erwerben"))?;
        lock.release().map_err(ctx("lock freigeben"))?;
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    /// Nebenläufigkeits-Stresstest nach dem Muster von
    /// `harw-knowledge/src/lock.rs` (`threads_serialize_their_read_modify_write`):
    /// mehrere Threads erhöhen eine gemeinsame Zähler-Datei nur unter dem
    /// Lock. Bricht die Exklusivität — der eigentliche in dieser Funktion
    /// behobene Befund, zwei Konsolidierungen laufen gleichzeitig gegen
    /// denselben Fact-Store —, gehen Erhöhungen durch überlappende
    /// Lese-Ändere-Schreibe-Zyklen verloren und die Endsumme stimmt nicht.
    /// Deckt auch die Folgebefunde ab: eine gerade freigegebene Lock-Datei
    /// galt als verwaist (`NotFound` beim Einstufen, danach `Io`-Fehler oder
    /// Löschen des nächsten Halters), und `Drop` nach `release` löschte ein
    /// zweites Mal — womöglich schon die Datei des nächsten Halters.
    #[test]
    fn lock_serializes_concurrent_read_modify_write_cycles() -> TestResult {
        let root = tmp_root("lock-concurrent");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;
        let counter_path = root.join("counter.txt");
        fs::write(&counter_path, "0").map_err(ctx("zähler anlegen"))?;

        const THREADS: u32 = 8;
        const ITERATIONS: u32 = 10;

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let root = root.clone();
                let counter_path = counter_path.clone();
                std::thread::spawn(move || -> TestResult {
                    for _ in 0..ITERATIONS {
                        let lock = loop {
                            match ConsolidationLock::try_acquire(&root) {
                                Ok(lock) => break lock,
                                Err(MemoryError::LockContention { .. }) => {
                                    std::thread::yield_now();
                                }
                                Err(e) => {
                                    return Err(TestError::Context {
                                        context: "lock erwerben",
                                        source: e.to_string(),
                                    });
                                }
                            }
                        };
                        let value: u32 = fs::read_to_string(&counter_path)
                            .map_err(ctx("zähler lesen"))?
                            .trim()
                            .parse()
                            .map_err(ctx("zähler parsen"))?;
                        fs::write(&counter_path, (value + 1).to_string())
                            .map_err(ctx("zähler schreiben"))?;
                        lock.release().map_err(ctx("lock freigeben"))?;
                    }
                    Ok(())
                })
            })
            .collect();

        for handle in handles {
            handle
                .join()
                .map_err(|_| TestError::Unexpected("lock-thread ist gepanict".to_owned()))??;
        }

        let total = fs::read_to_string(&counter_path).map_err(ctx("zähler lesen"))?;
        assert_eq!(
            total.trim(),
            (THREADS * ITERATIONS).to_string(),
            "ohne echte Exklusivität gehen Erhöhungen unter dem Lock verloren"
        );

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn lock_release_is_idempotent_after_manual_removal() -> TestResult {
        let root = tmp_root("lock-release-idempotent");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;
        let lock = ConsolidationLock::try_acquire(&root).map_err(ctx("lock erwerben"))?;
        fs::remove_file(root.join(LOCK_FILE_NAME)).map_err(ctx("lock-datei entfernen"))?;
        assert!(lock.release().is_ok());
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    /// Verschwindet die Lock-Datei zwischen gescheitertem Anlegen und
    /// Einstufen (der Halter gibt gerade frei), heißt das „erneut anlegen",
    /// nicht „verwaist, löschen" — und ein zweiter Aufbrecher, dem ein
    /// anderer zuvorkam, sieht darin keinen I/O-Fehler.
    #[test]
    fn missing_lock_file_is_neither_stale_nor_an_error() -> TestResult {
        let root = tmp_root("lock-missing");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;
        let lock_path = root.join(LOCK_FILE_NAME);

        let state = ConsolidationLock::inspect(&lock_path).map_err(ctx("lock einstufen"))?;
        assert!(matches!(state, LockState::Missing));
        ConsolidationLock::break_stale(&lock_path, b"created=alt\n")
            .map_err(ctx("fehlende lock-datei aufbrechen"))?;
        assert!(!lock_path.exists());

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    /// Hat ein anderer Erwerber die verwaiste Datei zwischen Einstufen und
    /// Aufbrechen schon durch seinen frischen Lock ersetzt, darf das
    /// Aufbrechen diesen Lock nicht entfernen; Tempdateien bleiben nicht
    /// liegen.
    #[test]
    fn break_stale_keeps_fresh_lock_that_replaced_the_stale_one() -> TestResult {
        let root = tmp_root("lock-break-replaced");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;
        let lock_path = root.join(LOCK_FILE_NAME);
        let holder = ConsolidationLock::try_acquire(&root).map_err(ctx("lock erwerben"))?;

        ConsolidationLock::break_stale(&lock_path, b"pid=1\ncreated=2000-01-01T00:00:00Z\n")
            .map_err(ctx("veralteten stand aufbrechen"))?;
        let on_disk = fs::read(&lock_path).map_err(ctx("lock-datei lesen"))?;
        assert_eq!(on_disk, holder.content, "der frische Lock muss zurückliegen");
        assert!(matches!(
            ConsolidationLock::try_acquire(&root),
            Err(MemoryError::LockContention { .. })
        ));

        holder.release().map_err(ctx("lock freigeben"))?;
        let left = fs::read_dir(&root).map_err(ctx("root lesen"))?.count();
        assert_eq!(left, 0, "weder Lock- noch Tempdateien dürfen liegen bleiben");

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    /// Wurde der eigene Lock als verwaist aufgebrochen und von einem anderen
    /// Erwerber neu angelegt, dürfen weder `release` noch `Drop` dessen
    /// Datei löschen.
    #[test]
    fn release_and_drop_keep_lock_file_of_another_holder() -> TestResult {
        let root = tmp_root("lock-foreign-holder");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;
        let lock_path = root.join(LOCK_FILE_NAME);
        let foreign = b"pid=424242\nseq=0\ncreated=2099-01-01T00:00:00Z\n";

        let released = ConsolidationLock::try_acquire(&root).map_err(ctx("lock erwerben"))?;
        fs::write(&lock_path, foreign).map_err(ctx("fremden lock schreiben"))?;
        released.release().map_err(ctx("lock freigeben"))?;
        assert_eq!(fs::read(&lock_path).map_err(ctx("lock-datei lesen"))?, foreign);

        fs::remove_file(&lock_path).map_err(ctx("fremden lock entfernen"))?;
        let dropped = ConsolidationLock::try_acquire(&root).map_err(ctx("lock erwerben"))?;
        fs::write(&lock_path, foreign).map_err(ctx("fremden lock schreiben"))?;
        drop(dropped);
        assert_eq!(fs::read(&lock_path).map_err(ctx("lock-datei lesen"))?, foreign);

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    /// Rückfall für Dateisysteme ohne harte Links: `create_direct` legt die
    /// Lock-Datei ebenso exklusiv und vollständig beschrieben an.
    #[test]
    fn create_direct_fallback_is_exclusive() -> TestResult {
        let root = tmp_root("lock-direct");
        fs::create_dir_all(&root).map_err(ctx("root anlegen"))?;
        let lock_path = root.join(LOCK_FILE_NAME);

        ConsolidationLock::create_direct(&lock_path, b"pid=1\n").map_err(ctx("direkt anlegen"))?;
        let second = ConsolidationLock::create_direct(&lock_path, b"pid=2\n");
        assert!(matches!(&second, Err(e) if e.kind() == io::ErrorKind::AlreadyExists));
        assert_eq!(fs::read(&lock_path).map_err(ctx("lock-datei lesen"))?, b"pid=1\n");

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
