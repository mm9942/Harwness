//! Signal-Promotion-Auswertung (Track A), siehe
//! `docs/design/track-a-memory-promotion.md` §1/§2 und `philosophy.md` §4
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
//! # Design-Abweichung von `track-a-memory-promotion.md` §1
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
pub fn apply_promotion(candidates: &[PromotionCandidate], warm_dir: &Path) -> MemoryResult<PromotionOutcome> {
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
        .map_err(|e| MemoryError::Io { path: target, source: e })?;
        warm_created += 1;
    }

    Ok(PromotionOutcome {
        warm_created,
        demoted_to_cold: 0,
        archived_warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("harw-promote-{tag}-{}-{id}", std::process::id()));
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
        assert_eq!(candidates.len(), 1, "PatternHint-Treffer hat nur 1 Vorkommen");
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
        assert!(candidates.is_empty(), "zukuenftiger Zeitstempel wird verworfen");
    }

    // -- apply_promotion ------------------------------------------------

    #[test]
    fn apply_promotion_writes_new_candidate_files() {
        let dir = tmp_dir("write");
        let candidates = vec![PromotionCandidate {
            namespace: "pattern_hint/greeting".to_owned(),
            content: "- hello\n".to_owned(),
            hit_count: 3,
        }];
        let outcome = apply_promotion(&candidates, &dir).unwrap();
        assert_eq!(outcome.warm_created, 1);
        assert_eq!(outcome.demoted_to_cold, 0);
        assert!(outcome.archived_warnings.is_empty());
        assert_eq!(
            fs::read_to_string(dir.join("pattern_hint/greeting.md")).unwrap(),
            "- hello\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_promotion_does_not_overwrite_existing_warm_file() {
        let dir = tmp_dir("idempotent");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(dir.join("pattern_hint")).unwrap();
        fs::write(dir.join("pattern_hint/greeting.md"), "- bereits vorhanden\n").unwrap();

        let candidates = vec![PromotionCandidate {
            namespace: "pattern_hint/greeting".to_owned(),
            content: "- neuer Inhalt\n".to_owned(),
            hit_count: 5,
        }];
        let outcome = apply_promotion(&candidates, &dir).unwrap();
        assert_eq!(outcome.warm_created, 0, "bestehende Datei bleibt unangetastet");
        assert_eq!(
            fs::read_to_string(dir.join("pattern_hint/greeting.md")).unwrap(),
            "- bereits vorhanden\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
