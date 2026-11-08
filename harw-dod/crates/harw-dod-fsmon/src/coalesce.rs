//! Zusammenfassen wiederholter Schreibereignisse je (Pfad, pid) — PL-100
//! WP1.6.
//!
//! # Warum
//! Jeder einzelne `write(2)` eines Prozesses erzeugt ein `FAN_MODIFY`. Der
//! Kernel fasst gleiche Ereignisse nur zusammen, solange sie noch in seiner
//! Warteschlange stehen; über zwei Leserunden der Sonde hinweg kommt jede
//! Wiederholung einzeln an. Ein Prozess, der eine Datei Zeile für Zeile
//! schreibt, würde so den Ringpuffer und die IPC-Inbox des Sentinels mit
//! inhaltsgleichen `FileWrite`-Ereignissen fluten und dort andere Ereignisse
//! verdrängen (Bedrohung B2/B10).
//!
//! # Regel
//! [`WriteCoalescer::admit`] lässt das erste Schreibereignis eines
//! Schlüssels (Pfad, pid) durch und verwirft jedes weitere desselben
//! Schlüssels, solange seit dem letzten durchgelassenen weniger als das
//! Fenster ([`COALESCE_WINDOW`]) vergangen ist. `FAN_MODIFY` und
//! `FAN_CLOSE_WRITE` ergeben dasselbe `EventKind::FileWrite { path }` —
//! deshalb zählt der Schlüssel, nicht das Bit. Ein Dauerschreiber erzeugt so
//! höchstens ein Ereignis je Fenster und Datei; ein anderer Prozess oder ein
//! anderer Pfad ist ein anderer Schlüssel und wird nie verdeckt.
//!
//! # Grenzen und Richtung im Zweifel
//! - Höchstens [`MAX_COALESCE_KEYS`] Schlüssel werden gemerkt; abgelaufene
//!   werden erst entfernt, wenn die Grenze erreicht ist. Ist sie danach
//!   immer noch erreicht, wird ein neuer Schlüssel durchgelassen, aber nicht
//!   gemerkt — im Zweifel ein Ereignis zu viel, nie eines zu wenig.
//! - Läuft die injizierte Zeit rückwärts (Uhrsprung), gilt das Ereignis als
//!   außerhalb des Fensters und wird durchgelassen.
//!
//! # Exportierte Elemente
//! [`WriteCoalescer`], [`COALESCE_WINDOW`], [`MAX_COALESCE_KEYS`].
//!
//! # Nebenläufigkeit
//! [`WriteCoalescer`] ist ein veränderlicher Zustand ohne eigene Sperre;
//! [`crate::sensor::FsMonSensor`] hält ihn hinter einem `Mutex`.
//!
//! # Fehler
//! Keine.
//!
//! # Examples
//! ```rust
//! use harw_dod_fsmon::coalesce::WriteCoalescer;
//! use jiff::{SignedDuration, Timestamp};
//!
//! # fn main() -> Result<(), jiff::Error> {
//! let mut coalescer = WriteCoalescer::new();
//! let t0 = Timestamp::UNIX_EPOCH;
//! assert!(coalescer.admit("/etc/hosts", 42, t0));
//! assert!(!coalescer.admit("/etc/hosts", 42, t0));
//! assert!(coalescer.admit("/etc/hosts", 43, t0));
//! let later = t0.checked_add(SignedDuration::from_secs(5))?;
//! assert!(coalescer.admit("/etc/hosts", 42, later));
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;

use jiff::{SignedDuration, Timestamp};

/// Fenster, in dem ein weiteres Schreibereignis desselben Prozesses auf
/// denselben Pfad als Wiederholung gilt.
pub const COALESCE_WINDOW: SignedDuration = SignedDuration::from_secs(2);

/// Höchstzahl gleichzeitig gemerkter (Pfad, pid)-Schlüssel.
pub const MAX_COALESCE_KEYS: usize = 4096;

/// Merkt sich je (Pfad, pid) den Zeitpunkt des zuletzt durchgelassenen
/// Schreibereignisses.
///
/// # Description
/// Siehe Moduldoku für Regel, Grenzen und die Richtung im Zweifel.
///
/// # Concurrency
/// Kein inneres Locking; `Send`, von genau einem Aufrufer gleichzeitig zu
/// benutzen.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone)]
pub struct WriteCoalescer {
    // Fensterlänge; nie negativ (siehe `with_window`).
    window: SignedDuration,
    // Letztes durchgelassenes Ereignis je (Pfad, pid).
    last_admitted: HashMap<(String, u32), Timestamp>,
    // Zahl der seit dem Bau verworfenen Wiederholungen.
    coalesced: u64,
}

impl Default for WriteCoalescer {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteCoalescer {
    /// Baut einen Zusammenfasser mit [`COALESCE_WINDOW`].
    ///
    /// # Returns
    /// Einen leeren Zusammenfasser.
    #[must_use]
    pub fn new() -> Self {
        Self::with_window(COALESCE_WINDOW)
    }

    /// Baut einen Zusammenfasser mit eigenem Fenster.
    ///
    /// # Arguments
    /// - `window` (`jiff::SignedDuration`): die Fensterlänge; ein negativer
    ///   Wert gilt als Null (jedes Ereignis wird durchgelassen).
    ///
    /// # Returns
    /// Einen leeren Zusammenfasser.
    #[must_use]
    pub fn with_window(window: SignedDuration) -> Self {
        Self {
            window: if window.is_negative() {
                SignedDuration::ZERO
            } else {
                window
            },
            last_admitted: HashMap::new(),
            coalesced: 0,
        }
    }

    /// Entscheidet, ob ein Schreibereignis durchgelassen wird.
    ///
    /// # Arguments
    /// - `path` (`&str`): der geschriebene Pfad, wie ihn
    ///   [`crate::shape::shape_event`] meldet.
    /// - `pid` (`u32`): der schreibende Prozess (`0`: unbekannt; dann ist
    ///   `0` Teil des Schlüssels).
    /// - `now` (`jiff::Timestamp`): injizierte Beobachtungszeit.
    ///
    /// # Returns
    /// `true`, wenn das Ereignis weitergereicht werden soll; `false` für
    /// eine Wiederholung innerhalb des Fensters.
    pub fn admit(&mut self, path: &str, pid: u32, now: Timestamp) -> bool {
        let key = (path.to_owned(), pid);
        if let Some(last) = self.last_admitted.get(&key)
            && self.within_window(*last, now)
        {
            self.coalesced = self.coalesced.saturating_add(1);
            return false;
        }
        if self.last_admitted.len() >= MAX_COALESCE_KEYS && !self.last_admitted.contains_key(&key) {
            self.prune(now);
        }
        if self.last_admitted.len() < MAX_COALESCE_KEYS || self.last_admitted.contains_key(&key) {
            self.last_admitted.insert(key, now);
        }
        true
    }

    /// Zahl der seit dem Bau verworfenen Wiederholungen.
    ///
    /// # Returns
    /// Einen monoton wachsenden, saturierenden Zähler.
    #[must_use]
    pub const fn coalesced_total(&self) -> u64 {
        self.coalesced
    }

    /// Zahl der aktuell gemerkten Schlüssel.
    ///
    /// # Returns
    /// Höchstens [`MAX_COALESCE_KEYS`].
    #[must_use]
    pub fn tracked_keys(&self) -> usize {
        self.last_admitted.len()
    }

    // Ob `now` weniger als ein Fenster nach `last` liegt (eine rückwärts
    // laufende Zeit liegt nie im Fenster).
    fn within_window(&self, last: Timestamp, now: Timestamp) -> bool {
        let elapsed = now.duration_since(last);
        !elapsed.is_negative() && elapsed < self.window
    }

    // Entfernt jeden Schlüssel, dessen Fenster abgelaufen ist.
    fn prune(&mut self, now: Timestamp) {
        let window = self.window;
        self.last_admitted.retain(|_, last| {
            let elapsed = now.duration_since(*last);
            !elapsed.is_negative() && elapsed < window
        });
    }
}

#[cfg(test)]
mod tests {
    use jiff::{SignedDuration, Timestamp};

    use super::{COALESCE_WINDOW, MAX_COALESCE_KEYS, WriteCoalescer};
    use crate::test_support::{TestResult, ctx};

    // `UNIX_EPOCH + millis`.
    fn at(millis: i64) -> TestResult<Timestamp> {
        Timestamp::UNIX_EPOCH
            .checked_add(SignedDuration::from_millis(millis))
            .map_err(ctx("timestamp in range"))
    }

    /// Ein Dauerschreiber ergibt ein Ereignis je Fenster und Datei.
    #[test]
    fn test_repeated_writes_of_one_process_are_coalesced_within_the_window() -> TestResult {
        let mut coalescer = WriteCoalescer::new();
        assert!(coalescer.admit("/etc/cron.d/job", 7, at(0)?));
        for millis in [100, 500, 1_000, 1_999] {
            assert!(
                !coalescer.admit("/etc/cron.d/job", 7, at(millis)?),
                "{millis}"
            );
        }
        assert_eq!(coalescer.coalesced_total(), 4);
        // Genau ein Fenster nach dem letzten durchgelassenen: wieder durch.
        let window_ms = i64::try_from(COALESCE_WINDOW.as_millis()).map_err(ctx("window"))?;
        assert!(coalescer.admit("/etc/cron.d/job", 7, at(window_ms)?));
        assert!(!coalescer.admit("/etc/cron.d/job", 7, at(window_ms.saturating_add(10))?));
        Ok(())
    }

    /// Ein anderer Prozess oder ein anderer Pfad wird nie verdeckt.
    #[test]
    fn test_other_pids_and_other_paths_are_separate_keys() -> TestResult {
        let mut coalescer = WriteCoalescer::new();
        let now = at(0)?;
        assert!(coalescer.admit("/root/.bashrc", 1, now));
        assert!(coalescer.admit("/root/.bashrc", 2, now));
        assert!(coalescer.admit("/root/.profile", 1, now));
        assert!(!coalescer.admit("/root/.bashrc", 1, now));
        assert_eq!(coalescer.tracked_keys(), 3);
        Ok(())
    }

    /// Eine rückwärts laufende Uhr lässt das Ereignis durch.
    #[test]
    fn test_a_clock_jump_backwards_admits_the_event() -> TestResult {
        let mut coalescer = WriteCoalescer::new();
        assert!(coalescer.admit("/etc/hosts", 9, at(10_000)?));
        assert!(coalescer.admit("/etc/hosts", 9, at(9_000)?));
        Ok(())
    }

    /// Ein Fenster der Länge null fasst nichts zusammen.
    #[test]
    fn test_a_zero_or_negative_window_coalesces_nothing() -> TestResult {
        for window in [SignedDuration::ZERO, SignedDuration::from_secs(-1)] {
            let mut coalescer = WriteCoalescer::with_window(window);
            assert!(coalescer.admit("/etc/hosts", 1, at(0)?));
            assert!(coalescer.admit("/etc/hosts", 1, at(0)?));
            assert_eq!(coalescer.coalesced_total(), 0);
        }
        Ok(())
    }

    /// Volle Tabelle: abgelaufene Schlüssel fallen weg; bleibt sie voll, wird
    /// ein neuer Schlüssel durchgelassen, aber nicht gemerkt (nie ein
    /// verlorenes Ereignis).
    #[test]
    fn test_a_full_table_prunes_expired_keys_and_never_drops_a_new_key() -> TestResult {
        let mut coalescer = WriteCoalescer::new();
        let start = at(0)?;
        for pid in 0..MAX_COALESCE_KEYS {
            let pid = u32::try_from(pid).map_err(ctx("pid"))?;
            assert!(coalescer.admit("/etc/hosts", pid, start));
        }
        assert_eq!(coalescer.tracked_keys(), MAX_COALESCE_KEYS);

        // Noch im Fenster: der neue Schlüssel kommt durch, wird nicht gemerkt.
        assert!(coalescer.admit("/etc/passwd", 1, at(100)?));
        assert!(coalescer.admit("/etc/passwd", 1, at(200)?));
        assert_eq!(coalescer.tracked_keys(), MAX_COALESCE_KEYS);

        // Nach dem Fenster: die alten Schlüssel fallen weg, der neue wird
        // gemerkt und fasst wieder zusammen.
        assert!(coalescer.admit("/etc/passwd", 1, at(5_000)?));
        assert_eq!(coalescer.tracked_keys(), 1);
        assert!(!coalescer.admit("/etc/passwd", 1, at(5_100)?));
        Ok(())
    }
}
