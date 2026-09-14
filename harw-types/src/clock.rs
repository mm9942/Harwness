//! Injizierbare Wanduhr (`Clock`) für serverseitig vergebene Zeitstempel.
//!
//! # Beschreibung
//! Sicherheitsrelevante Zeitpunkte (Auflösung einer Freigabe, TTL-Prüfung)
//! dürfen nie vom Client stammen (Befund F-122). Stores bekommen deshalb eine
//! Zeitquelle über dieses Trait gereicht; die Kompositionsstelle liefert
//! [`SystemClock`], Tests eine eigene feste Uhr.
//!
//! # Verantwortungsbereich
//! Nur das Trait und die Systemuhr. Monotone Uhren (Throttling, Timeouts)
//! gehören nicht hierher.
//!
//! # Nebenläufigkeit
//! [`Clock`] verlangt `Send + Sync`, damit eine Implementierung über
//! `Arc<dyn Clock>` oder `&dyn Clock` aus beliebigen Threads gleichzeitig
//! genutzt werden kann. [`SystemClock`] ist zustandslos und `Copy`.
//!
//! # Fehler
//! Keine — [`Clock::now`] ist unfehlbar.
//!
//! # Examples
//! ```rust
//! use harw_types::{Clock, SystemClock};
//!
//! let clock: &dyn Clock = &SystemClock;
//! assert!(clock.now() > jiff::Timestamp::UNIX_EPOCH);
//! ```

/// Zeitquelle für serverseitig vergebene Wanduhr-Zeitstempel.
///
/// # Description
/// Implementierungen liefern den aktuellen Zeitpunkt ihrer Uhr. Aufrufer
/// dürfen keine Monotonie annehmen (die Systemuhr kann springen).
///
/// # Concurrency
/// `Send + Sync`: gleichzeitige Aufrufe aus mehreren Threads sind erlaubt.
///
/// # Examples
/// ```rust
/// use harw_types::Clock;
///
/// struct Epoch;
/// impl Clock for Epoch {
///     fn now(&self) -> jiff::Timestamp {
///         jiff::Timestamp::UNIX_EPOCH
///     }
/// }
/// assert_eq!(Epoch.now(), jiff::Timestamp::UNIX_EPOCH);
/// ```
pub trait Clock: Send + Sync {
    /// Returns the current timestamp of this clock.
    ///
    /// # Returns
    /// Den aktuellen Zeitpunkt (`jiff::Timestamp`).
    fn now(&self) -> jiff::Timestamp;
}

/// Systemuhr: liefert `jiff::Timestamp::now()`.
///
/// # Description
/// Die einzige produktive Implementierung; gehört an die Kompositionsstelle
/// (Server, CLI, TUI), nie in Wire- oder Modelldaten.
///
/// # Concurrency
/// Zustandslos, `Copy`, threadsicher.
///
/// # Examples
/// ```rust
/// use harw_types::{Clock, SystemClock};
///
/// let before = jiff::Timestamp::now();
/// assert!(SystemClock.now() >= before);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> jiff::Timestamp {
        jiff::Timestamp::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Feste Testuhr; bewusst nur im Testmodul (Orchestrator-Entscheidung C-APPR).
    struct FixedClock(jiff::Timestamp);

    impl Clock for FixedClock {
        fn now(&self) -> jiff::Timestamp {
            self.0
        }
    }

    #[test]
    fn test_system_clock_now_is_not_before_a_prior_reading() {
        let before = jiff::Timestamp::now();
        let observed = SystemClock.now();
        assert!(observed >= before);
    }

    #[test]
    fn test_clock_is_usable_as_trait_object() {
        let at = jiff::Timestamp::constant(1_700_000_000, 0);
        let clock: &dyn Clock = &FixedClock(at);
        assert_eq!(clock.now(), at);
        assert_eq!(clock.now(), at);
    }
}
