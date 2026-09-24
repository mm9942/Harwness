//! Drosselung der Job-Meldungen — rein, mit übergebener Uhrzeit testbar.
//!
//! # Regeln (Plan R9, Teil F)
//! - **Fortschritt:** höchstens alle `every` Sekunden, und nur wenn sich
//!   seit der letzten Meldung etwas geändert hat ([`ProgressKey`]: erkannter
//!   Fortschritt, Zeilenzahlen, Warn-/Fehlerzähler). `every = 0` schaltet
//!   periodische Meldungen ab.
//! - **Fehler:** sofort, aber entprellt: die erste Fehlerzeile öffnet ein
//!   Fenster von `error_debounce`; alles, was darin ankommt, geht als **eine**
//!   Meldung hinaus. Zwischen zwei Fehlermeldungen liegen mindestens
//!   `error_min_interval` (ein Build mit 500 Fehlern erzeugt keine 500
//!   Meldungen). Höchstens `max_error_lines` Zeilen je Meldung, der Rest
//!   wird nur gezählt.
//! - **Ende:** immer (siehe [`NotifyThrottle::flush_errors`] davor).
//!
//! # Nebenläufigkeit
//! Gehört genau einem Überwachungs-Task; kein `Sync` nötig.

use crate::progress::ProgressSnapshot;
use std::time::{Duration, Instant};

/// Parameter der Drosselung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleConfig {
    /// Mindestabstand periodischer Fortschrittsmeldungen (`0` = aus).
    pub every: Duration,
    /// Sammelfenster ab der ersten Fehlerzeile.
    pub error_debounce: Duration,
    /// Mindestabstand zwischen zwei Fehlermeldungen.
    pub error_min_interval: Duration,
    /// Höchstzahl Fehlerzeilen je Meldung.
    pub max_error_lines: usize,
}

/// Was sich für die Frage „hat sich etwas geändert?" zählt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProgressKey {
    /// Erkannter Fortschritt.
    pub progress: Option<ProgressSnapshot>,
    /// Vollständige Ausgabezeilen (stdout + stderr).
    pub lines: u64,
    /// Warnzeilen.
    pub warnings: u64,
    /// Fehlerzeilen.
    pub errors: u64,
}

/// Eine fällige Fehlermeldung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorBatch {
    /// Gesammelte Zeilen (höchstens `max_error_lines`).
    pub lines: Vec<String>,
    /// Im Fenster gesehene Fehlerzeilen, auch die nicht mitgeschickten.
    pub in_batch: u64,
}

/// Zustand der Drosselung eines Jobs.
///
/// # Examples
/// ```rust
/// use std::time::{Duration, Instant};
/// use harw_tool_job::{NotifyThrottle, ProgressKey, ThrottleConfig};
///
/// let config = ThrottleConfig {
///     every: Duration::from_secs(60),
///     error_debounce: Duration::from_secs(2),
///     error_min_interval: Duration::from_secs(20),
///     max_error_lines: 8,
/// };
/// let t0 = Instant::now();
/// let mut throttle = NotifyThrottle::new(config, t0);
/// let changed = ProgressKey { lines: 10, ..ProgressKey::default() };
/// assert!(!throttle.progress_due(t0 + Duration::from_secs(30), &changed));
/// assert!(throttle.progress_due(t0 + Duration::from_secs(61), &changed));
/// ```
#[derive(Debug, Clone)]
pub struct NotifyThrottle {
    config: ThrottleConfig,
    last_progress_at: Instant,
    last_progress_key: ProgressKey,
    pending_errors: Vec<String>,
    pending_count: u64,
    pending_since: Option<Instant>,
    last_error_at: Option<Instant>,
}

impl NotifyThrottle {
    /// Beginnt mit „nichts gemeldet" zum Zeitpunkt `now` (Start des Jobs).
    #[must_use]
    pub fn new(config: ThrottleConfig, now: Instant) -> Self {
        Self {
            config,
            last_progress_at: now,
            last_progress_key: ProgressKey::default(),
            pending_errors: Vec::new(),
            pending_count: 0,
            pending_since: None,
            last_error_at: None,
        }
    }

    /// Ob eine periodische Fortschrittsmeldung fällig ist; merkt sie sich
    /// als gesendet, wenn ja.
    pub fn progress_due(&mut self, now: Instant, key: &ProgressKey) -> bool {
        if self.config.every.is_zero() {
            return false;
        }
        let elapsed = now.saturating_duration_since(self.last_progress_at);
        if elapsed < self.config.every || *key == self.last_progress_key {
            return false;
        }
        self.last_progress_at = now;
        self.last_progress_key = key.clone();
        true
    }

    /// Nimmt eine Fehlerzeile in das laufende Sammelfenster auf.
    pub fn record_error(&mut self, line: impl Into<String>, now: Instant) {
        if self.pending_since.is_none() {
            self.pending_since = Some(now);
        }
        self.pending_count = self.pending_count.saturating_add(1);
        if self.pending_errors.len() < self.config.max_error_lines {
            self.pending_errors.push(line.into());
        }
    }

    /// Liefert die gesammelten Fehlerzeilen, sobald das Sammelfenster
    /// abgelaufen ist und der Mindestabstand zur letzten Fehlermeldung
    /// eingehalten ist.
    pub fn errors_due(&mut self, now: Instant) -> Option<ErrorBatch> {
        let since = self.pending_since?;
        if now.saturating_duration_since(since) < self.config.error_debounce {
            return None;
        }
        if let Some(last) = self.last_error_at {
            if now.saturating_duration_since(last) < self.config.error_min_interval {
                return None;
            }
        }
        self.last_error_at = Some(now);
        self.take_batch()
    }

    /// Gibt offene Fehlerzeilen ohne Rücksicht auf Fenster/Abstand heraus
    /// (vor der Endmeldung).
    pub fn flush_errors(&mut self) -> Option<ErrorBatch> {
        self.take_batch()
    }

    fn take_batch(&mut self) -> Option<ErrorBatch> {
        self.pending_since = None;
        let in_batch = std::mem::take(&mut self.pending_count);
        let lines = std::mem::take(&mut self.pending_errors);
        (in_batch > 0).then_some(ErrorBatch { lines, in_batch })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::ProgressSource;

    fn config() -> ThrottleConfig {
        ThrottleConfig {
            every: Duration::from_secs(60),
            error_debounce: Duration::from_secs(2),
            error_min_interval: Duration::from_secs(20),
            max_error_lines: 3,
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn key(lines: u64, percent: Option<u8>) -> ProgressKey {
        ProgressKey {
            progress: percent.map(|pct| ProgressSnapshot {
                source: ProgressSource::Generic,
                percent: Some(pct),
                done: None,
                total: None,
                phase: None,
            }),
            lines,
            warnings: 0,
            errors: 0,
        }
    }

    #[test]
    fn test_progress_only_after_period_and_only_when_changed() {
        let t0 = Instant::now();
        let mut throttle = NotifyThrottle::new(config(), t0);
        // vor Ablauf der Periode: nie
        assert!(!throttle.progress_due(t0 + secs(59), &key(5, Some(10))));
        // nach Ablauf und geändert: ja
        assert!(throttle.progress_due(t0 + secs(60), &key(5, Some(10))));
        // gleiche Periode erneut: nein
        assert!(!throttle.progress_due(t0 + secs(61), &key(6, Some(11))));
        // nächste Periode, aber unverändert: nein
        assert!(!throttle.progress_due(t0 + secs(125), &key(5, Some(10))));
        // Änderung nach verstrichener Periode geht sofort hinaus
        assert!(throttle.progress_due(t0 + secs(126), &key(7, Some(12))));
    }

    #[test]
    fn test_progress_disabled_with_zero_period() {
        let t0 = Instant::now();
        let mut throttle = NotifyThrottle::new(
            ThrottleConfig {
                every: Duration::ZERO,
                ..config()
            },
            t0,
        );
        assert!(!throttle.progress_due(t0 + secs(3600), &key(99, Some(99))));
    }

    #[test]
    fn test_errors_are_debounced_into_one_batch() {
        let t0 = Instant::now();
        let mut throttle = NotifyThrottle::new(config(), t0);
        assert_eq!(throttle.errors_due(t0), None);
        throttle.record_error("error: one", t0);
        throttle.record_error("error: two", t0 + secs(1));
        // Fenster noch offen
        assert_eq!(throttle.errors_due(t0 + secs(1)), None);
        let batch = throttle.errors_due(t0 + secs(2));
        assert_eq!(
            batch,
            Some(ErrorBatch {
                lines: vec!["error: one".into(), "error: two".into()],
                in_batch: 2
            })
        );
        // nichts mehr offen
        assert_eq!(throttle.errors_due(t0 + secs(3)), None);
    }

    #[test]
    fn test_errors_respect_min_interval_and_cap_lines() {
        let t0 = Instant::now();
        let mut throttle = NotifyThrottle::new(config(), t0);
        throttle.record_error("error: a", t0);
        assert!(throttle.errors_due(t0 + secs(2)).is_some());
        for index in 0..5 {
            throttle.record_error(format!("error: {index}"), t0 + secs(3));
        }
        // Fenster abgelaufen, aber Mindestabstand (20 s ab t0+2) nicht
        assert_eq!(throttle.errors_due(t0 + secs(10)), None);
        let batch = throttle.errors_due(t0 + secs(22));
        assert_eq!(batch.as_ref().map(|b| b.lines.len()), Some(3));
        assert_eq!(batch.map(|b| b.in_batch), Some(5));
    }

    #[test]
    fn test_flush_errors_ignores_window() {
        let t0 = Instant::now();
        let mut throttle = NotifyThrottle::new(config(), t0);
        throttle.record_error("FAILED: x.o", t0);
        assert_eq!(
            throttle.flush_errors().map(|b| b.lines),
            Some(vec!["FAILED: x.o".to_owned()])
        );
        assert_eq!(throttle.flush_errors(), None);
    }
}
