//! Outcome-Tracker für das Memory-Subsystem.
//!
//! # Verantwortungsbereich
//! Implementiert die letzte Phase der Epistemic-Pipeline aus `epistemic.rs`:
//! Beobachtung → Hypothese → Evidenz → Lesson → **Outcome-Prüfung → bestätigen /
//! korrigieren / verwerfen**.
//!
//! Lessons (via [`crate::epistemic::EpistemicSignal`]) werden nach der Promotion
//! als [`PendingOutcomeCheck`] registriert. Sobald Evidenz vorliegt, wird ein
//! [`OutcomeReport`] gemeldet, der den Pending-Eintrag auflöst und in die
//! History verschiebt. Überfällige Checks werden automatisch als
//! [`crate::epistemic::OutcomeVerdict::Inconclusive`] abgeschlossen.
//!
//! # Schlüsseltypen
//! - [`PendingOutcomeCheck`] — registrierte, noch ungelöste Prüfung.
//! - [`OutcomeReport`]       — Ergebnis-Meldung, die eine Prüfung auflöst.
//! - [`OutcomeTracker`]      — Haupt-Zustandsmaschine (In-Memory, JSON-persistent).
//! - [`VerdictStats`]        — aggregierte Zähler je Verdikt.
//!
//! # Nebenläufigkeit
//! Alle Typen sind reine Wert-Typen (`Send + Sync`). Der Aufrufer ist für
//! Locking verantwortlich, wenn eine [`OutcomeTracker`]-Instanz über Threads
//! hinweg mutiert wird. Kein interner `Mutex`, keine Threads, keine Channels.
//!
//! # Fehler
//! Dieses Modul produziert keine eigenen Fehler. Serde-Fehler entstehen beim
//! Deserialisieren und werden an den Aufrufer propagiert.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_memory::outcome_tracker::{OutcomeTracker, PendingOutcomeCheck, OutcomeReport};
//! use harw_memory::epistemic::OutcomeVerdict;
//! use time::OffsetDateTime;
//!
//! let mut tracker = OutcomeTracker::new();
//! let now = OffsetDateTime::now_utc();
//!
//! tracker.track(PendingOutcomeCheck {
//!     signal_id: "sig-1".to_owned(),
//!     context: "Wurde auf Task-42 angewandt.".to_owned(),
//!     applied_at: now,
//!     deadline: now + time::Duration::days(7),
//! });
//!
//! let existed = tracker.report(OutcomeReport {
//!     signal_id: "sig-1".to_owned(),
//!     verdict: OutcomeVerdict::Confirmed,
//!     note: Some("Hat funktioniert.".to_owned()),
//!     reported_at: now,
//! });
//! assert!(existed);
//! assert!(tracker.pending.is_empty());
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use time::OffsetDateTime;

// ─────────────────────────────────────────────────────────────────────────────
// PendingOutcomeCheck
// ─────────────────────────────────────────────────────────────────────────────

/// Vorgemerkte Prüfung: eine Lesson wurde promotiert und wartet auf Outcome-Evidenz.
///
/// # Beschreibung
/// Wird nach der Promotion eines [`crate::epistemic::EpistemicSignal`] angelegt.
/// Hält den Kontext der Anwendung und eine Frist fest, nach der — ohne explizites
/// [`OutcomeReport`] — automatisch [`crate::epistemic::OutcomeVerdict::Inconclusive`]
/// eingetragen wird.
///
/// # Serialisierung
/// RFC-3339-Zeitstempel für `applied_at` und `deadline`.
///
/// # Nebenläufigkeit
/// Reiner Wert-Typ, `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingOutcomeCheck {
    /// Referenz auf `EpistemicSignal.id`.
    pub signal_id: String,
    /// Kurzbeschreibung, worauf die Lesson angewandt wurde.
    pub context: String,
    /// Zeitpunkt der Anwendung (UTC, RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub applied_at: OffsetDateTime,
    /// Frist bis zur Outcome-Prüfung (UTC, RFC 3339).
    ///
    /// Nach Ablauf ohne gemeldetes Ergebnis gilt das Verdikt automatisch als
    /// [`crate::epistemic::OutcomeVerdict::Inconclusive`].
    #[serde(with = "time::serde::rfc3339")]
    pub deadline: OffsetDateTime,
}

// ─────────────────────────────────────────────────────────────────────────────
// OutcomeReport
// ─────────────────────────────────────────────────────────────────────────────

/// Ergebnis-Meldung, die eine [`PendingOutcomeCheck`] auflöst.
///
/// # Beschreibung
/// Wird nach Ablauf einer Beobachtungsperiode erstellt und an
/// [`OutcomeTracker::report`] übergeben. Der Tracker entfernt den
/// zugehörigen Pending-Eintrag und hängt diesen Report an die History.
///
/// # Serialisierung
/// RFC-3339-Zeitstempel für `reported_at`.
///
/// # Nebenläufigkeit
/// Reiner Wert-Typ, `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutcomeReport {
    /// Referenz auf `EpistemicSignal.id` — muss mit dem Pending-Eintrag übereinstimmen.
    pub signal_id: String,
    /// Bewertung des Outcomes.
    pub verdict: crate::epistemic::OutcomeVerdict,
    /// Optionale Freitextnotiz zur Begründung.
    pub note: Option<String>,
    /// Zeitpunkt der Meldung (UTC, RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub reported_at: OffsetDateTime,
}

// ─────────────────────────────────────────────────────────────────────────────
// VerdictStats
// ─────────────────────────────────────────────────────────────────────────────

/// Aggregierte Zähler je [`crate::epistemic::OutcomeVerdict`] über alle History-Reports.
///
/// # Beschreibung
/// Liefert eine schnelle Übersicht, wie häufig Lessons bestätigt, widerlegt
/// oder nicht messbar abgeschlossen wurden. Wird über [`OutcomeTracker::verdict_stats`]
/// berechnet.
///
/// # Nebenläufigkeit
/// `Copy`-Typ, `Send + Sync`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictStats {
    /// Anzahl Reports mit Verdikt [`crate::epistemic::OutcomeVerdict::Confirmed`].
    pub confirmed: u32,
    /// Anzahl Reports mit Verdikt [`crate::epistemic::OutcomeVerdict::Inconclusive`].
    pub inconclusive: u32,
    /// Anzahl Reports mit Verdikt [`crate::epistemic::OutcomeVerdict::Refuted`].
    pub refuted: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
// OutcomeTracker
// ─────────────────────────────────────────────────────────────────────────────

/// Hauptzustandsmaschine für Outcome-Prüfungen.
///
/// # Beschreibung
/// Verwaltet zwei Sammlungen:
/// - `pending` — laufende Checks, die noch auf Evidenz warten (Key: `signal_id`).
/// - `history` — abgeschlossene Reports (chronologisch angehängt).
///
/// Die Struktur ist rein im Speicher; für Persistenz wird JSON-Serialisierung
/// über `serde_json` empfohlen. Das komplette Roundtrip-Verhalten ist durch
/// `serde` abgedeckt.
///
/// # Nebenläufigkeit
/// Reine Wert-Typ-Struktur, `Send + Sync`. Kein interner Mutex. Der Aufrufer
/// ist für Locking verantwortlich, wenn die Instanz geteilt und mutiert wird.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::outcome_tracker::OutcomeTracker;
/// let tracker = OutcomeTracker::new();
/// assert!(tracker.pending.is_empty());
/// assert!(tracker.history.is_empty());
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutcomeTracker {
    /// Laufende Checks, noch ohne Ergebnis. Key = `signal_id`.
    pub pending: HashMap<String, PendingOutcomeCheck>,
    /// Abgeschlossene Outcome-Reports (chronologisch).
    pub history: Vec<OutcomeReport>,
}

impl OutcomeTracker {
    /// Erstellt einen leeren [`OutcomeTracker`].
    ///
    /// # Beschreibung
    /// Equivalent zu `Self::default()`.
    ///
    /// # Rückgabe
    /// Neue, leere [`OutcomeTracker`]-Instanz.
    ///
    /// # Nebenläufigkeit
    /// Rein allokierend, thread-safe.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_memory::outcome_tracker::OutcomeTracker;
    /// let t = OutcomeTracker::new();
    /// assert!(t.pending.is_empty());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Registriert eine Lesson zur späteren Outcome-Prüfung.
    ///
    /// # Beschreibung
    /// Fügt `check` unter `check.signal_id` in `pending` ein. Falls für diese
    /// `signal_id` bereits ein Eintrag vorhanden ist, wird er ersetzt — die
    /// Deadline wird aktualisiert.
    ///
    /// # Argumente
    /// - `check` ([`PendingOutcomeCheck`]): Ownership wird übernommen.
    ///
    /// # Nebenläufigkeit
    /// Erfordert `&mut self`; der Aufrufer ist für Synchronisation verantwortlich.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_memory::outcome_tracker::{OutcomeTracker, PendingOutcomeCheck};
    /// use time::OffsetDateTime;
    ///
    /// let mut t = OutcomeTracker::new();
    /// t.track(PendingOutcomeCheck {
    ///     signal_id: "s1".to_owned(),
    ///     context: "ctx".to_owned(),
    ///     applied_at: OffsetDateTime::now_utc(),
    ///     deadline: OffsetDateTime::now_utc(),
    /// });
    /// assert_eq!(t.pending.len(), 1);
    /// ```
    pub fn track(&mut self, check: PendingOutcomeCheck) {
        self.pending.insert(check.signal_id.clone(), check);
    }

    /// Meldet ein Outcome und löst den zugehörigen Pending-Eintrag auf.
    ///
    /// # Beschreibung
    /// Entfernt den Eintrag mit `report.signal_id` aus `pending` und hängt
    /// `report` an `history` an. Falls kein Pending-Eintrag für die ID
    /// existiert, wird `report` trotzdem an die History angehängt und
    /// `false` zurückgegeben.
    ///
    /// # Argumente
    /// - `report` ([`OutcomeReport`]): Ownership wird übernommen.
    ///
    /// # Rückgabe
    /// `true` wenn ein Pending-Eintrag gefunden und entfernt wurde, `false` sonst.
    ///
    /// # Nebenläufigkeit
    /// Erfordert `&mut self`; der Aufrufer ist für Synchronisation verantwortlich.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_memory::outcome_tracker::{OutcomeTracker, OutcomeReport};
    /// use harw_memory::epistemic::OutcomeVerdict;
    /// use time::OffsetDateTime;
    ///
    /// let mut t = OutcomeTracker::new();
    /// let existed = t.report(OutcomeReport {
    ///     signal_id: "unknown".to_owned(),
    ///     verdict: OutcomeVerdict::Inconclusive,
    ///     note: None,
    ///     reported_at: OffsetDateTime::now_utc(),
    /// });
    /// assert!(!existed);
    /// ```
    pub fn report(&mut self, report: OutcomeReport) -> bool {
        let existed = self.pending.remove(&report.signal_id).is_some();
        self.history.push(report);
        existed
    }

    /// Findet alle Pending-Checks, deren Deadline vor `now` liegt.
    ///
    /// # Beschreibung
    /// Gibt Referenzen auf alle [`PendingOutcomeCheck`]-Einträge zurück, deren
    /// `deadline < now`. Die Reihenfolge ist unbestimmt (HashMap-abhängig).
    ///
    /// # Argumente
    /// - `now` ([`OffsetDateTime`]): Referenzzeitpunkt; wird nicht intern abgerufen
    ///   (testbar).
    ///
    /// # Rückgabe
    /// `Vec<&PendingOutcomeCheck>` — leer wenn keine überfälligen Einträge.
    ///
    /// # Nebenläufigkeit
    /// Nur lesender Zugriff; thread-safe.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_memory::outcome_tracker::{OutcomeTracker, PendingOutcomeCheck};
    /// use time::{OffsetDateTime, Duration};
    ///
    /// let mut t = OutcomeTracker::new();
    /// let now = OffsetDateTime::now_utc();
    /// t.track(PendingOutcomeCheck {
    ///     signal_id: "s1".to_owned(),
    ///     context: "ctx".to_owned(),
    ///     applied_at: now,
    ///     deadline: now - Duration::hours(1),
    /// });
    /// assert_eq!(t.overdue(now).len(), 1);
    /// ```
    pub fn overdue(&self, now: OffsetDateTime) -> Vec<&PendingOutcomeCheck> {
        self.pending
            .values()
            .filter(|check| check.deadline < now)
            .collect()
    }

    /// Schließt alle überfälligen Pending-Checks automatisch mit `Inconclusive` ab.
    ///
    /// # Beschreibung
    /// Alle Einträge in `pending`, deren `deadline < now`, werden entfernt und
    /// als [`crate::epistemic::OutcomeVerdict::Inconclusive`]-Report an `history`
    /// angehängt. `reported_at` wird auf `now` gesetzt, `note` auf `note`.
    ///
    /// # Argumente
    /// - `now` ([`OffsetDateTime`]): Referenzzeitpunkt.
    /// - `note` (`&str`): Notiz, die in den generierten Reports eingetragen wird.
    ///
    /// # Rückgabe
    /// Anzahl der abgeschlossenen Checks.
    ///
    /// # Nebenläufigkeit
    /// Erfordert `&mut self`; der Aufrufer ist für Synchronisation verantwortlich.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_memory::outcome_tracker::{OutcomeTracker, PendingOutcomeCheck};
    /// use time::{OffsetDateTime, Duration};
    ///
    /// let mut t = OutcomeTracker::new();
    /// let now = OffsetDateTime::now_utc();
    /// t.track(PendingOutcomeCheck {
    ///     signal_id: "s1".to_owned(),
    ///     context: "ctx".to_owned(),
    ///     applied_at: now,
    ///     deadline: now - Duration::seconds(1),
    /// });
    /// let closed = t.auto_close_overdue(now, "Frist abgelaufen");
    /// assert_eq!(closed, 1);
    /// ```
    pub fn auto_close_overdue(&mut self, now: OffsetDateTime, note: &str) -> usize {
        let overdue_ids: Vec<String> = self
            .pending
            .values()
            .filter(|check| check.deadline < now)
            .map(|check| check.signal_id.clone())
            .collect();

        let count = overdue_ids.len();
        for id in overdue_ids {
            if let Some(_check) = self.pending.remove(&id) {
                self.history.push(OutcomeReport {
                    signal_id: id,
                    verdict: crate::epistemic::OutcomeVerdict::Inconclusive,
                    note: Some(note.to_owned()),
                    reported_at: now,
                });
            }
        }
        count
    }

    /// Berechnet aggregierte Zähler je Verdikt über alle History-Reports.
    ///
    /// # Beschreibung
    /// Iteriert über `history` und zählt [`crate::epistemic::OutcomeVerdict::Confirmed`],
    /// [`crate::epistemic::OutcomeVerdict::Inconclusive`] und
    /// [`crate::epistemic::OutcomeVerdict::Refuted`] separat.
    ///
    /// # Rückgabe
    /// [`VerdictStats`] mit den aktuellen Zählern.
    ///
    /// # Nebenläufigkeit
    /// Nur lesender Zugriff; thread-safe.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_memory::outcome_tracker::OutcomeTracker;
    /// let t = OutcomeTracker::new();
    /// let stats = t.verdict_stats();
    /// assert_eq!(stats.confirmed, 0);
    /// ```
    pub fn verdict_stats(&self) -> VerdictStats {
        use crate::epistemic::OutcomeVerdict;
        let mut stats = VerdictStats::default();
        for report in &self.history {
            match report.verdict {
                OutcomeVerdict::Confirmed => stats.confirmed += 1,
                OutcomeVerdict::Inconclusive => stats.inconclusive += 1,
                OutcomeVerdict::Refuted => stats.refuted += 1,
            }
        }
        stats
    }

    /// Löscht History-Einträge, die älter als `max_age_days` Tage sind.
    ///
    /// # Beschreibung
    /// Berechnet den Schnitt-Zeitpunkt `cutoff = now - max_age_days` und
    /// entfernt alle Reports aus `history`, deren `reported_at < cutoff`.
    /// Neuere Einträge bleiben erhalten.
    ///
    /// # Argumente
    /// - `now` ([`OffsetDateTime`]): Referenzzeitpunkt.
    /// - `max_age_days` (`i64`): Maximales Alter in Tagen. Negative Werte
    ///   führen zu einem Cutoff in der Zukunft und löschen die gesamte History.
    ///
    /// # Nebenläufigkeit
    /// Erfordert `&mut self`; der Aufrufer ist für Synchronisation verantwortlich.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_memory::outcome_tracker::OutcomeTracker;
    /// use time::OffsetDateTime;
    ///
    /// let mut t = OutcomeTracker::new();
    /// let now = OffsetDateTime::now_utc();
    /// t.prune_history(now, 30);
    /// assert!(t.history.is_empty());
    /// ```
    pub fn prune_history(&mut self, now: OffsetDateTime, max_age_days: i64) {
        let cutoff = now - time::Duration::days(max_age_days);
        self.history.retain(|report| report.reported_at >= cutoff);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// refutation_score
// ─────────────────────────────────────────────────────────────────────────────

/// Berechnet einen Widerlegungs-Score (0..=100) für ein Signal aus seinen Reports.
///
/// # Beschreibung
/// Filtert `reports` auf Einträge mit `signal_id == for_signal` und berechnet:
/// ```text
/// score = refuted_count / total_count * 100   (Integer-Division)
/// ```
/// Bei 0 passenden Reports wird 0 zurückgegeben.
///
/// # Argumente
/// - `reports` (`&[OutcomeReport]`): Slice aller zu prüfenden Reports.
/// - `for_signal` (`&str`): Die `signal_id`, für die der Score berechnet wird.
///
/// # Rückgabe
/// `u8` im Bereich `0..=100`. Höhere Werte bedeuten mehr Ablehnung.
///
/// # Nebenläufigkeit
/// Rein funktional, keine Seiteneffekte, thread-safe.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::outcome_tracker::{OutcomeReport, refutation_score};
/// use harw_memory::epistemic::OutcomeVerdict;
/// use time::OffsetDateTime;
///
/// let reports = vec![
///     OutcomeReport { signal_id: "s1".to_owned(), verdict: OutcomeVerdict::Refuted,
///                     note: None, reported_at: OffsetDateTime::now_utc() },
///     OutcomeReport { signal_id: "s1".to_owned(), verdict: OutcomeVerdict::Confirmed,
///                     note: None, reported_at: OffsetDateTime::now_utc() },
/// ];
/// // 1 von 2 widerlegt → 50
/// assert_eq!(refutation_score(&reports, "s1"), 50);
/// ```
pub fn refutation_score(reports: &[OutcomeReport], for_signal: &str) -> u8 {
    use crate::epistemic::OutcomeVerdict;
    let relevant: Vec<&OutcomeReport> = reports
        .iter()
        .filter(|r| r.signal_id == for_signal)
        .collect();

    let total = relevant.len();
    if total == 0 {
        return 0;
    }

    let refuted = relevant
        .iter()
        .filter(|r| r.verdict == OutcomeVerdict::Refuted)
        .count();

    ((refuted * 100) / total) as u8
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::epistemic::OutcomeVerdict;
    use crate::test_support::{TestError, TestResult, ctx};
    use time::{Duration, OffsetDateTime};

    // ── Hilfsfunktionen ──────────────────────────────────────────────────────

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn make_pending(signal_id: &str, deadline: OffsetDateTime) -> PendingOutcomeCheck {
        PendingOutcomeCheck {
            signal_id: signal_id.to_owned(),
            context: "Test-Kontext".to_owned(),
            applied_at: now(),
            deadline,
        }
    }

    fn make_report(signal_id: &str, verdict: OutcomeVerdict) -> OutcomeReport {
        OutcomeReport {
            signal_id: signal_id.to_owned(),
            verdict,
            note: None,
            reported_at: now(),
        }
    }

    // ── Test 1: track → report entfernt aus pending, history wächst ──────────

    #[test]
    fn track_and_report_removes_pending() {
        let mut tracker = OutcomeTracker::new();
        tracker.track(make_pending("sig-1", now() + Duration::days(1)));
        assert_eq!(
            tracker.pending.len(),
            1,
            "nach track: pending muss 1 Eintrag haben"
        );

        let existed = tracker.report(make_report("sig-1", OutcomeVerdict::Confirmed));

        assert!(
            existed,
            "report muss true zurückgeben wenn pending existiert"
        );
        assert!(
            tracker.pending.is_empty(),
            "pending muss nach report leer sein"
        );
        assert_eq!(tracker.history.len(), 1, "history muss 1 Eintrag haben");
    }

    // ── Test 2: report auf unbekannte ID → false, history bleibt leer ────────

    #[test]
    fn report_returns_false_when_no_pending() {
        let mut tracker = OutcomeTracker::new();
        let existed = tracker.report(make_report("unbekannt", OutcomeVerdict::Inconclusive));

        assert!(!existed, "report auf unbekannte ID muss false zurückgeben");
        assert_eq!(
            tracker.history.len(),
            1,
            "report wird trotzdem in history gespeichert"
        );
        assert!(tracker.pending.is_empty(), "pending bleibt leer");
    }

    // ── Test 3: Doppel-track ersetzt bestehenden Eintrag ─────────────────────

    #[test]
    fn track_double_replaces() -> TestResult {
        let mut tracker = OutcomeTracker::new();
        let deadline1 = now() + Duration::days(1);
        let deadline2 = now() + Duration::days(7);

        tracker.track(make_pending("sig-dup", deadline1));
        tracker.track(make_pending("sig-dup", deadline2));

        assert_eq!(
            tracker.pending.len(),
            1,
            "Doppel-track darf pending nicht wachsen lassen"
        );
        let entry = tracker
            .pending
            .get("sig-dup")
            .ok_or(TestError::Missing("Eintrag muss existieren"))?;
        assert_eq!(
            entry.deadline, deadline2,
            "zweite Registrierung muss deadline überschreiben"
        );
        Ok(())
    }

    // ── Test 4: overdue findet nur abgelaufene ────────────────────────────────

    #[test]
    fn overdue_finds_expired_only() {
        let mut tracker = OutcomeTracker::new();
        let t = now();

        tracker.track(make_pending("past", t - Duration::seconds(1)));
        tracker.track(make_pending("future", t + Duration::days(1)));

        let overdue = tracker.overdue(t);
        assert_eq!(overdue.len(), 1, "overdue muss genau einen Eintrag liefern");
        assert_eq!(overdue[0].signal_id, "past");
    }

    // ── Test 5: auto_close_overdue → Inconclusive in history ─────────────────

    #[test]
    fn auto_close_moves_overdue_to_history_as_inconclusive() {
        let mut tracker = OutcomeTracker::new();
        let t = now();

        tracker.track(make_pending("od-1", t - Duration::hours(2)));
        tracker.track(make_pending("od-2", t - Duration::hours(1)));
        tracker.track(make_pending("future", t + Duration::days(1)));

        let closed = tracker.auto_close_overdue(t, "automatisch abgeschlossen");

        assert_eq!(closed, 2, "auto_close_overdue muss 2 zurückgeben");
        assert_eq!(
            tracker.pending.len(),
            1,
            "nur der Zukunfts-Eintrag darf in pending verbleiben"
        );
        assert_eq!(tracker.history.len(), 2, "history muss 2 Einträge haben");
        for report in &tracker.history {
            assert_eq!(
                report.verdict,
                OutcomeVerdict::Inconclusive,
                "auto-closed Reports müssen Inconclusive sein"
            );
        }
    }

    // ── Test 6: verdict_stats zählt korrekt ──────────────────────────────────

    #[test]
    fn verdict_stats_counts_correctly() {
        let mut tracker = OutcomeTracker::new();
        tracker
            .history
            .push(make_report("a", OutcomeVerdict::Confirmed));
        tracker
            .history
            .push(make_report("b", OutcomeVerdict::Confirmed));
        tracker
            .history
            .push(make_report("c", OutcomeVerdict::Refuted));

        let stats = tracker.verdict_stats();
        assert_eq!(stats.confirmed, 2);
        assert_eq!(stats.inconclusive, 0);
        assert_eq!(stats.refuted, 1);
    }

    // ── Test 7: prune_history entfernt alte Einträge ──────────────────────────

    #[test]
    fn prune_history_removes_old() {
        let mut tracker = OutcomeTracker::new();
        let t = now();

        // Eintrag der 35 Tage alt ist
        let old_report = OutcomeReport {
            signal_id: "old".to_owned(),
            verdict: OutcomeVerdict::Confirmed,
            note: None,
            reported_at: t - Duration::days(35),
        };
        tracker.history.push(old_report);

        // Eintrag der 5 Tage alt ist
        let recent_report = OutcomeReport {
            signal_id: "recent".to_owned(),
            verdict: OutcomeVerdict::Confirmed,
            note: None,
            reported_at: t - Duration::days(5),
        };
        tracker.history.push(recent_report);

        tracker.prune_history(t, 30);

        assert_eq!(
            tracker.history.len(),
            1,
            "nur der neue Eintrag darf übrig bleiben"
        );
        assert_eq!(tracker.history[0].signal_id, "recent");
    }

    // ── Test 8: refutation_score 0 wenn keine Reports ────────────────────────

    #[test]
    fn refutation_score_zero_when_no_reports() {
        let reports: Vec<OutcomeReport> = vec![];
        assert_eq!(refutation_score(&reports, "sig-x"), 0);
    }

    // ── Test 9: refutation_score 100 wenn alle widerlegt ─────────────────────

    #[test]
    fn refutation_score_100_when_all_refuted() {
        let reports = vec![
            make_report("s1", OutcomeVerdict::Refuted),
            make_report("s1", OutcomeVerdict::Refuted),
            make_report("other", OutcomeVerdict::Confirmed),
        ];
        assert_eq!(
            refutation_score(&reports, "s1"),
            100,
            "alle relevanten Reports sind Refuted → Score muss 100 sein"
        );
    }

    // ── Test 10: JSON-Roundtrip ────────────────────────────────────────────

    #[test]
    fn serde_roundtrip_tracker() -> TestResult {
        let mut tracker = OutcomeTracker::new();
        let t = now();

        tracker.track(make_pending("sig-rt", t + Duration::days(3)));
        tracker.history.push(OutcomeReport {
            signal_id: "sig-old".to_owned(),
            verdict: OutcomeVerdict::Confirmed,
            note: Some("Hat funktioniert.".to_owned()),
            reported_at: t - Duration::days(1),
        });

        let json =
            serde_json::to_string_pretty(&tracker).map_err(ctx("Serialisierung muss klappen"))?;
        let back: OutcomeTracker =
            serde_json::from_str(&json).map_err(ctx("Deserialisierung muss klappen"))?;

        assert_eq!(back.pending.len(), tracker.pending.len());
        assert_eq!(back.history.len(), tracker.history.len());
        assert_eq!(back.history[0].signal_id, "sig-old");
        assert_eq!(back.history[0].verdict, OutcomeVerdict::Confirmed);
        assert_eq!(back.history[0].note, Some("Hat funktioniert.".to_owned()));

        let pending_back = back
            .pending
            .get("sig-rt")
            .ok_or(TestError::Missing("pending sig-rt muss vorhanden sein"))?;
        assert_eq!(pending_back.signal_id, "sig-rt");
        assert_eq!(pending_back.context, "Test-Kontext");
        Ok(())
    }
}
