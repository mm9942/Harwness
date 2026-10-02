//! Signal-Promotion-Auswertung (Track A), siehe
//! `docs/design/memory-promotion.md` §1/§2 und `philosophy.md` §4
//! („Memory-Konsolidierung ist ein langlebiger Workflow").
//!
//! # Verantwortungsbereich
//! - [`evaluate_signals`] wertet eine Menge zeitgestempelter Signale
//!   innerhalb eines Zeitfensters aus und liefert Promotion-Kandidaten
//!   (WARM-Ziel je Namespace) — reine Funktion, kein I/O.
//! - [`apply_promotion`] schreibt neue WARM-Dateien für Kandidaten, die noch
//!   nicht materialisiert sind (die Datei selbst ist die Idempotenz-Grenze,
//!   wie bei [`crate::file_store::FileMemoryStore`]s bestehender
//!   Muster-Promotion).
//!
//! # Design-Abweichung von `docs/design/memory-promotion.md` §1
//! Die dortige Skizze schreibt `evaluate_signals(signals: &[Signal], ...)`.
//! [`crate::types::Signal`] trägt jedoch **keinen** Zeitstempel (es ist ein
//! reines Nutzinhalt-Enum, append-only ins Signal-Log geschrieben ohne
//! Zeitfeld) — eine Fensterauswertung („3× in 7 Tagen") ist ohne
//! Aufzeichnungszeitpunkt nicht möglich. Diese Funktion nimmt deshalb
//! [`SignalOccurrence`] (Signal + Zeitpunkt) entgegen statt nackter
//! `Signal`e. Eine echte Anbindung an das Signal-Log müsste dessen
//! JSONL-Format um ein Zeitfeld erweitern — das ist bewusst **nicht** Teil
//! dieser Änderung (siehe Moduldoku unten, „Nicht wired").
//!
//! # Nicht wired: Verhältnis zu `FileMemoryStore::maintain()`
//! `FileMemoryStore::promote_pattern_hints` (privat, aufgerufen aus
//! `maintain()`) deckt bereits denselben Kernfall ab —
//! „N gleiche `PatternHint`-Signale → eine neue WARM-Datei" — allerdings
//! ungefenstert (zählt alle jemals aufgezeichneten Treffer, nicht nur die
//! der letzten `window`-Tage) und nur für `Signal::PatternHint`, nicht für
//! `Correction`/`Reflection`. Diese Funktionen hier sind bewusst **nicht**
//! in `maintain()` verdrahtet: ein Ersatz der bestehenden Promotion durch
//! die gefensterte Variante würde voraussetzen, dass das Signal-Log auf der
//! Platte Zeitstempel trägt — eine Formatänderung von
//! `signals/*.jsonl`, die ohne Compiler-/Testlauf-Zugriff ein zu großes
//! Regressionsrisiko für die bestehende, bereits getestete
//! `promote_pattern_hints`/`maintain()`-Kette wäre. Ein künftiger Umbau, der
//! Zeitstempel im Signal-Log einführt, kann [`evaluate_signals`] direkt
//! wiederverwenden.
//!
//! WARM→COLD-Verdrängung nach Nutzung ([`PromotionOutcome::demoted_to_cold`]/
//! [`PromotionOutcome::archived_warnings`]) ist ebenfalls nicht in
//! [`apply_promotion`] implementiert: sie braucht eine `last_used`-Spur je
//! Namespace-Datei, die es für das WARM/COLD-Tier — anders als für Fakten,
//! siehe [`crate::facts::FactStore::usage`] — im bestehenden Code noch nicht
//! gibt. `apply_promotion` liefert diese Felder deshalb immer `0`/leer.
//!
//! # Nebenläufigkeit
//! [`evaluate_signals`] ist eine reine Funktion. [`apply_promotion`] schreibt
//! über [`harw_fsutil::write_atomic`] (Tempdatei + `rename`, für sich atomar)
//! und serialisiert selbst nicht zwischen mehreren Prozessen — dafür ist
//! (analog zu [`crate::consolidation::ConsolidationLock`]) der Aufrufer
//! zuständig.
//!
//! # Fehler
//! [`crate::error::MemoryError::Io`] aus [`apply_promotion`].

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use time::{Duration, OffsetDateTime};

use crate::error::{MemoryError, MemoryResult};
use crate::facts::{Fact, FactScope, FactStore, find_repo_specific_reference, redact};
use crate::types::Signal;

/// Ein Signal zusammen mit dem Zeitpunkt seiner Aufzeichnung — siehe
/// Moduldoku „Design-Abweichung".
#[derive(Clone, Debug)]
pub struct SignalOccurrence {
    /// Das aufgezeichnete Signal.
    pub signal: Signal,
    /// Zeitpunkt der Aufzeichnung (UTC).
    pub at: OffsetDateTime,
}

/// Ein erkannter Promotion-Kandidat: `hit_count` gleichartige Signale
/// innerhalb des Auswertungsfensters, die denselben `namespace`-Slot
/// belegen sollen.
#[derive(Clone, Debug, PartialEq)]
pub struct PromotionCandidate {
    /// Ziel-Namespace unterhalb `warm/`, z. B. `"correction/das-ist-falsch"`.
    pub namespace: String,
    /// Aggregierter Markdown-Inhalt der neuen WARM-Datei (eine Zeile je
    /// Treffer, chronologisch aufsteigend).
    pub content: String,
    /// Anzahl der Signale, die zu diesem Kandidaten beigetragen haben.
    pub hit_count: usize,
}

/// Ergebnis eines einzelnen Promotion-Durchlaufs
/// ([`apply_promotion`]/künftiger Cold-Sweep).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromotionOutcome {
    /// Anzahl neu angelegter WARM-Dateien.
    pub warm_created: usize,
    /// Anzahl WARM→COLD-Demotions. Immer `0` in dieser Iteration, siehe
    /// Moduldoku.
    pub demoted_to_cold: usize,
    /// Namespaces, deren COLD-Verweildauer die Warnschwelle (90 Tage)
    /// überschritten hat. Immer leer in dieser Iteration, siehe Moduldoku.
    pub archived_warnings: Vec<String>,
}

/// Liefert das Gruppierungspräfix plus die Identitäts-Zeichenkette eines
/// Signals (der Anteil, der zwei Signale als „dasselbe wiederkehrende
/// Muster" erkennbar macht).
fn identity(signal: &Signal) -> &str {
    match signal {
        Signal::Correction { text, .. } => text,
        Signal::Reflection { context, .. } => context,
        Signal::PatternHint { key, .. } => key,
    }
}

/// Liefert den Freitext-Anteil eines Signals, der in den aggregierten
/// WARM-Inhalt übernommen wird.
fn detail(signal: &Signal) -> &str {
    match signal {
        Signal::Correction { text, .. } => text,
        Signal::Reflection { lesson, .. } => lesson,
        Signal::PatternHint { note, .. } => note,
    }
}

/// Wertet `signals` seit `now - window` aus und liefert alle Namespace-Slots,
/// die mindestens `threshold`-mal innerhalb dieses Fensters getroffen wurden.
///
/// # Beschreibung
/// Gruppiert nach `(signal.kind_label(), identity(signal))`, wobei jede
/// Gruppe zum Namespace `"<kind_label>/<slugify(identity)>"` wird (z. B.
/// `"correction/das-ist-falsch"`). Signale außerhalb `[now - window, now]`
/// (zu alt, oder mit einem Zeitpunkt in der Zukunft) werden verworfen, bevor
/// gruppiert wird — siehe Track-A-Testfall „3 Keys über 8 Tage verteilt →
/// kein Kandidat" (das älteste der drei fällt aus dem 7-Tage-Fenster,
/// verbleibende 2 unterschreiten den Schwellwert).
///
/// `threshold == 0` deaktiviert die Auswertung (liefert immer `Vec::new()`),
/// analog zu [`crate::heartbeat::tick`]s Schutz vor einem fehlerhaften
/// Runtime-Override.
///
/// Kein I/O, deterministisch bei gleicher Eingabe (Ergebnis nach `namespace`
/// sortiert).
#[must_use]
pub fn evaluate_signals(
    signals: &[SignalOccurrence],
    now: OffsetDateTime,
    window: Duration,
    threshold: usize,
) -> Vec<PromotionCandidate> {
    if threshold == 0 {
        return Vec::new();
    }

    let mut groups: HashMap<String, Vec<&SignalOccurrence>> = HashMap::new();
    for occurrence in signals {
        let age = now - occurrence.at;
        if age < Duration::ZERO || age > window {
            continue;
        }
        let namespace = format!(
            "{}/{}",
            occurrence.signal.kind_label(),
            crate::facts::slugify(identity(&occurrence.signal))
        );
        groups.entry(namespace).or_default().push(occurrence);
    }

    let mut candidates: Vec<PromotionCandidate> = groups
        .into_iter()
        .filter(|(_, hits)| hits.len() >= threshold)
        .map(|(namespace, mut hits)| {
            hits.sort_by_key(|hit| hit.at);
            let content = hits
                .iter()
                .map(|hit| format!("- {}", detail(&hit.signal)))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            let hit_count = hits.len();
            PromotionCandidate {
                namespace,
                content,
                hit_count,
            }
        })
        .collect();
    candidates.sort_by(|a, b| a.namespace.cmp(&b.namespace));
    candidates
}

/// Schreibt für jeden Kandidaten in `candidates`, dessen Ziel-Datei unter
/// `warm_dir` noch nicht existiert, eine neue WARM-Datei
/// `<namespace>.md` mit `candidate.content` (atomar).
///
/// # Beschreibung
/// Bereits vorhandene WARM-Dateien werden **nicht** überschrieben — die
/// materialisierte Datei ist die Idempotenz-Grenze, wie bei
/// `FileMemoryStore::promote_pattern_hints`. Legt
/// `warm_dir` sowie etwaige Namespace-Unterordner an, falls sie fehlen.
///
/// `demoted_to_cold`/`archived_warnings` im Ergebnis sind immer `0`/leer,
/// siehe Moduldoku.
///
/// # Errors
/// [`MemoryError::Io`], wenn `warm_dir` (oder ein Namespace-Unterordner)
/// nicht angelegt werden kann, oder eine Kandidat-Datei nicht geschrieben
/// werden kann.
pub fn apply_promotion(
    candidates: &[PromotionCandidate],
    warm_dir: &Path,
) -> MemoryResult<PromotionOutcome> {
    fs::create_dir_all(warm_dir).map_err(|e| MemoryError::Io {
        path: warm_dir.to_path_buf(),
        source: e,
    })?;

    let mut warm_created = 0usize;
    for candidate in candidates {
        let target = warm_dir.join(format!("{}.md", candidate.namespace));
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| MemoryError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        harw_fsutil::write_atomic(
            &target,
            candidate.content.as_bytes(),
            harw_fsutil::AtomicWriteOptions::with_mode(0o600),
        )
        .map_err(|e| MemoryError::Io {
            path: target,
            source: e,
        })?;
        warm_created += 1;
    }

    Ok(PromotionOutcome {
        warm_created,
        demoted_to_cold: 0,
        archived_warnings: Vec::new(),
    })
}

// ---------------------------------------------------------------------
// Explizite Promotion Projekt → Global (`/memory promote --to-global`)
// ---------------------------------------------------------------------

/// Präfix des Herkunftsvermerks in `sources` eines promoteten Fakts.
pub const PROMOTED_FROM_PREFIX: &str = "promoted_from:";

/// Fehler von [`promote_fact_to_global`].
#[derive(Debug)]
pub enum GlobalPromotionError {
    /// Im Projekt-Store gibt es keinen Fakt dieses Namens.
    NotFound {
        /// Gesuchter Name.
        name: String,
    },
    /// Der Fakt ist nicht global-tauglich (absoluter Pfad, repo-spezifischer
    /// Verweis oder Geheimnis). Es wurde nichts geschrieben.
    Rejected {
        /// Fakt-Name.
        name: String,
        /// Grund (nennt nie den Geheimnis-Inhalt).
        reason: String,
    },
    /// Frist abgelaufen; der globale Store blieb unverändert.
    Deadline(crate::consolidation::DeadlineExceeded),
    /// Speicherfehler, inkl. [`MemoryError::LockContention`].
    Memory(MemoryError),
}

impl std::fmt::Display for GlobalPromotionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { name } => write!(f, "kein Projekt-Fakt '{name}' gefunden"),
            Self::Rejected { name, reason } => {
                write!(f, "Fakt '{name}' nicht global promotebar: {reason}")
            }
            Self::Deadline(e) => e.fmt(f),
            Self::Memory(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for GlobalPromotionError {}

impl From<MemoryError> for GlobalPromotionError {
    fn from(e: MemoryError) -> Self {
        Self::Memory(e)
    }
}

impl From<crate::consolidation::DeadlineExceeded> for GlobalPromotionError {
    fn from(e: crate::consolidation::DeadlineExceeded) -> Self {
        Self::Deadline(e)
    }
}

/// Ergebnis einer erfolgreichen Promotion.
#[derive(Clone, Debug, PartialEq)]
pub struct GlobalPromotion {
    /// Der globale Fakt (so, wie er im Ziel-Store liegt).
    pub fact: Fact,
    /// `true`, wenn bereits ein inhaltsgleicher globaler Fakt existierte
    /// (nichts neu geschrieben).
    pub already_present: bool,
}

/// Gibt den Ablehnungsgrund zurück, wenn `fact` nicht global promotebar ist.
///
/// Geprüft werden `name`, `description`, `body` und `tags` auf absolute
/// Pfade/repo-spezifische Referenzen
/// ([`crate::facts::find_repo_specific_reference`]) und auf Geheimnisse
/// (Text ändert sich unter [`crate::facts::redact`]). `sources` des
/// Quellfakts werden nie übernommen und daher nicht geprüft.
#[must_use]
pub fn global_promotion_rejection(fact: &Fact) -> Option<String> {
    let fields = std::iter::once(("name", fact.name.as_str()))
        .chain(std::iter::once(("description", fact.description.as_str())))
        .chain(std::iter::once(("body", fact.body.as_str())))
        .chain(fact.tags.iter().map(|t| ("tag", t.as_str())));
    for (field, text) in fields {
        // `FactStore::write` schwärzt bereits beim Speichern: ein gespeicherter
        // Fakt trägt das Geheimnis nicht mehr, aber den `[redacted]`-Marker.
        if text.contains("[redacted]") || redact(text) != text {
            return Some(format!("{field} enthält ein Geheimnis"));
        }
        if let Some(token) = find_repo_specific_reference(text) {
            return Some(format!(
                "{field} enthält einen Pfad/repo-spezifischen Verweis ('{token}')"
            ));
        }
    }
    None
}

/// Kopiert den Projekt-Fakt `name` in den globalen Store unter
/// `global_root`, mit Herkunftsvermerk `promoted_from:<project_label>/<name>`.
///
/// # Beschreibung
/// Reihenfolge „erst prüfen, dann anwenden": Fakt lesen, Tauglichkeit prüfen
/// ([`global_promotion_rejection`]), Frist prüfen, den
/// [`crate::consolidation::ConsolidationLock`] der globalen Wurzel nehmen
/// (wartet bis `deadline`), Zielnamen auflösen (inhaltsgleich vorhanden →
/// nichts schreiben; Namenskollision mit anderem Inhalt → Suffix `-2`, `-3`,
/// …), Frist erneut prüfen und **direkt** in den Ziel-Store schreiben (keine
/// Zwischenablage). Der Quellfakt bleibt unverändert; `sources` werden nicht
/// übernommen, nur der Herkunftsvermerk.
///
/// # Errors
/// [`GlobalPromotionError`]: `NotFound`, `Rejected`, `Deadline` (global
/// unverändert), `Memory` (u. a. Lock bis zur Frist belegt).
pub fn promote_fact_to_global(
    project_store: &FactStore,
    global_root: &Path,
    name: &str,
    project_label: &str,
    now: OffsetDateTime,
    deadline: crate::consolidation::Deadline,
) -> Result<GlobalPromotion, GlobalPromotionError> {
    let source = project_store
        .read(name)?
        .ok_or_else(|| GlobalPromotionError::NotFound {
            name: name.to_owned(),
        })?;
    if let Some(reason) = global_promotion_rejection(&source) {
        return Err(GlobalPromotionError::Rejected {
            name: name.to_owned(),
            reason,
        });
    }
    deadline.check("promote-validate")?;

    let global = FactStore::open(global_root, FactScope::Global)?;
    let lock = crate::consolidation::ConsolidationLock::acquire_until(global_root, deadline.clone())?;
    let result: Result<GlobalPromotion, GlobalPromotionError> = (|| {
        let body = source.body.trim_end_matches('\n');
        let mut target = source.name.clone();
        for suffix in 2u32..=50 {
            match global.read(&target)? {
                None => break,
                Some(existing)
                    if existing.body.trim_end_matches('\n') == body
                        && existing.description == source.description =>
                {
                    return Ok(GlobalPromotion {
                        fact: existing,
                        already_present: true,
                    });
                }
                Some(_) => {
                    let base: String = source.name.chars().take(55).collect();
                    target = format!("{}-{suffix}", base.trim_end_matches('-'));
                }
            }
        }
        // Commit-Punkt.
        deadline.check("promote-write")?;
        let label = project_label.replace(['/', '\\'], "_");
        let fact = Fact {
            name: target,
            description: source.description.clone(),
            fact_type: source.fact_type,
            scope: FactScope::Global,
            created: now,
            updated: now,
            confidence: source.confidence,
            sources: vec![format!("{PROMOTED_FROM_PREFIX}{label}/{}", source.name)],
            tags: source.tags.clone(),
            body: source.body.clone(),
        };
        global.write(&fact)?;
        Ok(GlobalPromotion {
            fact,
            already_present: false,
        })
    })();
    let _ = lock.release();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("harw-promote-{tag}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn pattern(key: &str, note: &str, at: OffsetDateTime) -> SignalOccurrence {
        SignalOccurrence {
            signal: Signal::PatternHint {
                key: key.to_owned(),
                note: note.to_owned(),
            },
            at,
        }
    }

    fn now() -> OffsetDateTime {
        time::macros::datetime!(2026-07-16 12:00:00 UTC)
    }

    // -- evaluate_signals ---------------------------------------------------

    #[test]
    fn evaluate_signals_promotes_at_threshold_within_window() {
        let now = now();
        let signals = vec![
            pattern("greeting", "first", now - Duration::days(6)),
            pattern("greeting", "second", now - Duration::days(3)),
            pattern("greeting", "third", now),
        ];
        let candidates = evaluate_signals(&signals, now, Duration::days(7), 3);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].namespace, "pattern_hint/greeting");
        assert_eq!(candidates[0].hit_count, 3);
        assert_eq!(candidates[0].content, "- first\n- second\n- third\n");
    }

    #[test]
    fn evaluate_signals_below_threshold_yields_no_candidate() {
        let now = now();
        let signals = vec![
            pattern("greeting", "first", now - Duration::days(1)),
            pattern("greeting", "second", now),
        ];
        let candidates = evaluate_signals(&signals, now, Duration::days(7), 3);
        assert!(candidates.is_empty(), "2 < Schwellwert 3 — kein Kandidat");
    }

    #[test]
    fn evaluate_signals_ignores_occurrences_outside_window() {
        let now = now();
        // Verteilt über 8 Tage: das aelteste faellt aus dem 7-Tage-Fenster.
        let signals = vec![
            pattern("greeting", "too-old", now - Duration::days(8)),
            pattern("greeting", "second", now - Duration::days(3)),
            pattern("greeting", "third", now),
        ];
        let candidates = evaluate_signals(&signals, now, Duration::days(7), 3);
        assert!(
            candidates.is_empty(),
            "nur 2 von 3 liegen im Fenster — unter dem Schwellwert"
        );
    }

    #[test]
    fn evaluate_signals_groups_different_signal_kinds_separately() {
        let now = now();
        let signals = vec![
            SignalOccurrence {
                signal: Signal::Correction {
                    text: "nicht bevormunden".to_owned(),
                    context: None,
                },
                at: now,
            },
            SignalOccurrence {
                signal: Signal::Correction {
                    text: "nicht bevormunden".to_owned(),
                    context: None,
                },
                at: now - Duration::days(1),
            },
            SignalOccurrence {
                signal: Signal::Correction {
                    text: "nicht bevormunden".to_owned(),
                    context: None,
                },
                at: now - Duration::days(2),
            },
            pattern("nicht-bevormunden", "anderer Sachverhalt", now),
        ];
        let candidates = evaluate_signals(&signals, now, Duration::days(7), 3);
        assert_eq!(
            candidates.len(),
            1,
            "PatternHint-Treffer hat nur 1 Vorkommen"
        );
        assert_eq!(candidates[0].namespace, "correction/nicht-bevormunden");
        assert_eq!(candidates[0].hit_count, 3);
    }

    #[test]
    fn evaluate_signals_zero_threshold_disables_evaluation() {
        let now = now();
        let signals = vec![
            pattern("greeting", "a", now),
            pattern("greeting", "b", now),
            pattern("greeting", "c", now),
        ];
        assert!(evaluate_signals(&signals, now, Duration::days(7), 0).is_empty());
    }

    #[test]
    fn evaluate_signals_ignores_future_dated_occurrences() {
        let now = now();
        let signals = vec![
            pattern("greeting", "a", now + Duration::days(1)),
            pattern("greeting", "b", now),
            pattern("greeting", "c", now),
        ];
        let candidates = evaluate_signals(&signals, now, Duration::days(7), 3);
        assert!(
            candidates.is_empty(),
            "zukuenftiger Zeitstempel wird verworfen"
        );
    }

    // -- apply_promotion ------------------------------------------------

    #[test]
    fn apply_promotion_writes_new_candidate_files() -> TestResult {
        let dir = tmp_dir("write");
        let candidates = vec![PromotionCandidate {
            namespace: "pattern_hint/greeting".to_owned(),
            content: "- hello\n".to_owned(),
            hit_count: 3,
        }];
        let outcome = apply_promotion(&candidates, &dir).map_err(ctx("apply_promotion"))?;
        assert_eq!(outcome.warm_created, 1);
        assert_eq!(outcome.demoted_to_cold, 0);
        assert!(outcome.archived_warnings.is_empty());
        assert_eq!(
            fs::read_to_string(dir.join("pattern_hint/greeting.md"))
                .map_err(ctx("read greeting.md"))?,
            "- hello\n"
        );
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn apply_promotion_does_not_overwrite_existing_warm_file() -> TestResult {
        let dir = tmp_dir("idempotent");
        fs::create_dir_all(&dir).map_err(ctx("create dir"))?;
        fs::create_dir_all(dir.join("pattern_hint")).map_err(ctx("create pattern_hint dir"))?;
        fs::write(
            dir.join("pattern_hint/greeting.md"),
            "- bereits vorhanden\n",
        )
        .map_err(ctx("write greeting.md"))?;

        let candidates = vec![PromotionCandidate {
            namespace: "pattern_hint/greeting".to_owned(),
            content: "- neuer Inhalt\n".to_owned(),
            hit_count: 5,
        }];
        let outcome = apply_promotion(&candidates, &dir).map_err(ctx("apply_promotion"))?;
        assert_eq!(
            outcome.warm_created, 0,
            "bestehende Datei bleibt unangetastet"
        );
        assert_eq!(
            fs::read_to_string(dir.join("pattern_hint/greeting.md"))
                .map_err(ctx("read greeting.md"))?,
            "- bereits vorhanden\n"
        );
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }
}

#[cfg(test)]
mod global_tests {
    use super::*;
    use crate::capture::{consolidate_global_memories, consolidate_memories_with_deadline};
    use crate::consolidation::{ConsolidationError, ConsolidationLock, Deadline};
    use crate::facts::FactType;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::sync::{Arc, Barrier};
    use std::time::Duration as StdDuration;

    fn root(tag: &str) -> TestResult<tempfile::TempDir> {
        tempfile::Builder::new()
            .prefix(&format!("harw-g4-{tag}-"))
            .tempdir()
            .map_err(ctx("tempdir"))
    }

    fn mk(name: &str, desc: &str, body: &str, scope: FactScope, tags: &[&str]) -> Fact {
        let now = OffsetDateTime::now_utc();
        Fact {
            name: name.to_owned(),
            description: desc.to_owned(),
            fact_type: FactType::Fact,
            scope,
            created: now,
            updated: now,
            confidence: 0.9,
            sources: vec![format!("session:{name}")],
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            body: body.to_owned(),
        }
    }

    fn file_count(dir: &Path) -> usize {
        fs::read_dir(dir).map_or(0, |d| d.count())
    }

    #[test]
    fn path_and_secret_facts_are_rejected_without_writing() -> TestResult {
        let proj = root("rej-p")?;
        let glob = root("rej-g")?;
        let ps = FactStore::open(proj.path(), FactScope::Project).map_err(ctx("open"))?;
        for (name, body) in [
            ("abs-path", "Config liegt in /home/alice/repo/config.toml"),
            ("rel-path", "Siehe harw-ops/src/memory.rs fuer Details"),
            ("secret", "token = sk-abcdefghijklmnopqrstuvwxyz0123"),
        ] {
            ps.write(&mk(name, "d", body, FactScope::Project, &[]))
                .map_err(ctx("write"))?;
            let res = promote_fact_to_global(
                &ps,
                glob.path(),
                name,
                "proj",
                OffsetDateTime::now_utc(),
                Deadline::default_deletion(),
            );
            match res {
                Err(GlobalPromotionError::Rejected { .. }) => {}
                other => {
                    return Err(TestError::Unexpected(format!("{name}: {other:?}")));
                }
            }
        }
        let gs = FactStore::open(glob.path(), FactScope::Global).map_err(ctx("open g"))?;
        assert!(gs.list().map_err(ctx("list"))?.is_empty());
        Ok(())
    }

    #[test]
    fn clean_fact_is_copied_with_source_retained() -> TestResult {
        let proj = root("ok-p")?;
        let glob = root("ok-g")?;
        let ps = FactStore::open(proj.path(), FactScope::Project).map_err(ctx("open"))?;
        ps.write(&mk(
            "short-commits",
            "Bevorzuge kurze Commit-Messages",
            "Kurze Commit-Messages im Imperativ.",
            FactScope::Project,
            &["style"],
        ))
        .map_err(ctx("write"))?;
        let done = promote_fact_to_global(
            &ps,
            glob.path(),
            "short-commits",
            "myrepo",
            OffsetDateTime::now_utc(),
            Deadline::default_deletion(),
        )
        .map_err(ctx("promote"))?;
        assert!(!done.already_present);
        let gs = FactStore::open(glob.path(), FactScope::Global).map_err(ctx("open g"))?;
        let read = gs
            .read("short-commits")
            .map_err(ctx("read"))?
            .ok_or(TestError::Missing("global fact"))?;
        assert_eq!(read.scope, FactScope::Global);
        assert_eq!(read.sources, vec!["promoted_from:myrepo/short-commits"]);
        // Quelle unveraendert, zweiter Lauf idempotent.
        assert!(ps.read("short-commits").map_err(ctx("src"))?.is_some());
        let again = promote_fact_to_global(
            &ps,
            glob.path(),
            "short-commits",
            "myrepo",
            OffsetDateTime::now_utc(),
            Deadline::default_deletion(),
        )
        .map_err(ctx("again"))?;
        assert!(again.already_present);
        Ok(())
    }

    #[test]
    fn expired_deadline_aborts_promotion_and_leaves_global_unchanged() -> TestResult {
        let proj = root("dl-p")?;
        let glob = root("dl-g")?;
        let ps = FactStore::open(proj.path(), FactScope::Project).map_err(ctx("open"))?;
        ps.write(&mk("pref", "d", "Kurz halten.", FactScope::Project, &[]))
            .map_err(ctx("write"))?;
        let res = promote_fact_to_global(
            &ps,
            glob.path(),
            "pref",
            "p",
            OffsetDateTime::now_utc(),
            Deadline::after(StdDuration::ZERO),
        );
        assert!(matches!(res, Err(GlobalPromotionError::Deadline(_))));
        assert_eq!(file_count(&glob.path().join("facts")), 0);
        assert!(!glob.path().join("consolidation.lock").exists());
        Ok(())
    }

    #[test]
    fn duplicate_global_facts_merge() -> TestResult {
        let glob = root("dup")?;
        let gs = FactStore::open(glob.path(), FactScope::Global).map_err(ctx("open"))?;
        gs.write(&mk(
            "a-pref",
            "Bevorzuge kurze Commit Messages im Imperativ",
            "Kurz.",
            FactScope::Global,
            &[],
        ))
        .map_err(ctx("w1"))?;
        gs.write(&mk(
            "b-pref",
            "Bevorzuge kurze Commit Messages im Imperativ",
            "Kurz.",
            FactScope::Global,
            &[],
        ))
        .map_err(ctx("w2"))?;
        let report = consolidate_global_memories(glob.path()).map_err(ctx("consolidate"))?;
        assert_eq!(report.merged, 1);
        assert_eq!(report.deleted, 1);
        let left = gs.list().map_err(ctx("list"))?;
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].sources.len(), 2);
        assert!(!glob.path().join("consolidation.lock").exists());
        Ok(())
    }

    #[test]
    fn contradicting_global_facts_are_recorded_in_conflicts_json() -> TestResult {
        let glob = root("conf")?;
        let gs = FactStore::open(glob.path(), FactScope::Global).map_err(ctx("open"))?;
        let a = mk(
            "tabs",
            "Einrueckung mit Tabs",
            "Nutze Tabs.",
            FactScope::Global,
            &["indent"],
        );
        let b = mk(
            "spaces",
            "Zeilenabstand mit Leerzeichen setzen",
            "Nutze Spaces.",
            FactScope::Global,
            &["indent"],
        );
        gs.write(&a).map_err(ctx("wa"))?;
        gs.write(&b).map_err(ctx("wb"))?;
        consolidate_global_memories(glob.path()).map_err(ctx("c1"))?;
        consolidate_global_memories(glob.path()).map_err(ctx("c2"))?;
        let raw = fs::read(glob.path().join("facts").join("_conflicts.json"))
            .map_err(ctx("read conflicts"))?;
        let list: Vec<serde_json::Value> = serde_json::from_slice(&raw).map_err(ctx("parse"))?;
        assert_eq!(list.len(), 1, "conflict must be recorded once: {list:?}");
        assert_eq!(gs.list().map_err(ctx("list"))?.len(), 2);
        Ok(())
    }

    #[test]
    fn parallel_lock_acquisition_yields_exactly_one_contention() -> TestResult {
        let glob = root("lock")?;
        let path = Arc::new(glob.path().to_path_buf());
        let start = Arc::new(Barrier::new(2));
        let done = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (path, start, done) = (path.clone(), start.clone(), done.clone());
                std::thread::spawn(move || {
                    start.wait();
                    let r = ConsolidationLock::try_acquire(path.as_path());
                    // Der Gewinner haelt den Lock, bis beide versucht haben.
                    done.wait();
                    r.map(|l| {
                        let _ = l.release();
                    })
                })
            })
            .collect();
        let mut ok = 0;
        let mut contention = 0;
        for h in handles {
            match h.join() {
                Ok(Ok(())) => ok += 1,
                Ok(Err(MemoryError::LockContention { .. })) => contention += 1,
                other => return Err(TestError::Unexpected(format!("{other:?}"))),
            }
        }
        assert_eq!((ok, contention), (1, 1));
        Ok(())
    }

    #[test]
    fn expired_deadline_aborts_consolidation_and_keeps_store() -> TestResult {
        let glob = root("cdl")?;
        let gs = FactStore::open(glob.path(), FactScope::Global).map_err(ctx("open"))?;
        for n in ["x-one", "y-two"] {
            gs.write(&mk(
                n,
                "Gleiche Beschreibung hier",
                "Gleich.",
                FactScope::Global,
                &[],
            ))
            .map_err(ctx("w"))?;
        }
        let res = consolidate_memories_with_deadline(
            glob.path(),
            FactScope::Global,
            Deadline::after(StdDuration::ZERO),
        );
        match res {
            Err(ConsolidationError::Deadline(_)) => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        assert_eq!(gs.list().map_err(ctx("list"))?.len(), 2);
        assert!(!glob.path().join("consolidation.lock").exists());
        // Erfolgspfad mit Frist schreibt direkt ins Ziel.
        let ok = consolidate_memories_with_deadline(
            glob.path(),
            FactScope::Global,
            Deadline::default_deletion(),
        )
        .map_err(ctx("ok run"))?;
        assert_eq!(ok.merged, 1);
        assert_eq!(gs.list().map_err(ctx("list2"))?.len(), 1);
        Ok(())
    }

    #[test]
    fn cancelled_promotion_leaves_global_unchanged() -> TestResult {
        let proj = root("cancel-p")?;
        let glob = root("cancel-g")?;
        let ps = FactStore::open(proj.path(), FactScope::Project).map_err(ctx("open"))?;
        ps.write(&mk("pref", "d", "Kurz halten.", FactScope::Project, &[]))
            .map_err(ctx("write"))?;
        let flag = crate::consolidation::CancelFlag::new();
        flag.cancel();
        let res = promote_fact_to_global(
            &ps,
            glob.path(),
            "pref",
            "p",
            OffsetDateTime::now_utc(),
            Deadline::default_deletion().with_cancel(flag),
        );
        assert!(
            matches!(&res, Err(GlobalPromotionError::Deadline(d)) if d.cancelled),
            "{res:?}"
        );
        assert_eq!(file_count(&glob.path().join("facts")), 0);
        Ok(())
    }

    #[test]
    fn lock_wait_stops_when_cancelled() -> TestResult {
        let glob = root("cancel-wait")?;
        let held = ConsolidationLock::try_acquire(glob.path()).map_err(ctx("hold"))?;
        let flag = crate::consolidation::CancelFlag::new();
        flag.cancel();
        let started = std::time::Instant::now();
        let res = ConsolidationLock::acquire_until(
            glob.path(),
            Deadline::default_deletion().with_cancel(flag),
        );
        assert!(matches!(res, Err(MemoryError::LockContention { .. })));
        assert!(started.elapsed() < StdDuration::from_secs(5));
        let _ = held.release();
        Ok(())
    }

    #[test]
    fn acquire_until_waits_then_reports_contention() -> TestResult {
        let glob = root("wait")?;
        let held = ConsolidationLock::try_acquire(glob.path()).map_err(ctx("hold"))?;
        let res = ConsolidationLock::acquire_until(
            glob.path(),
            Deadline::after(StdDuration::from_millis(60)),
        );
        assert!(matches!(res, Err(MemoryError::LockContention { .. })));
        let _ = held.release();
        let _lock = ConsolidationLock::acquire_until(glob.path(), Deadline::default_deletion())
            .map_err(ctx("acquire after release"))?;
        Ok(())
    }
}
