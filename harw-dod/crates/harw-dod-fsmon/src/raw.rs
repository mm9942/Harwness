//! Rohe fanotify-Ereignisse und ihre Quelle: der Bindungsteil, klein und
//! hinter einem Trait.
//!
//! # Verantwortungsbereich
//! [`RawFsEvent`] trägt genau die vier Rohgrößen, aus denen der Formungsteil
//! ([`crate::shape::shape_event`]) ein `harw_dod_signals::SecurityEvent`
//! baut: eine Ereignismaske, eine Prozess-ID, eine Ausführungs-UID und ein
//! bereits gelesenes Dateideskriptor-Ziel. Alles vier sind Zahlen oder eine
//! Zeichenkette — **kein Feld trägt Dateiinhalt**.
//!
//! [`FsEventSource`] ist die einzige Schnittstelle zu einer Quelle solcher
//! Ereignisse. Sie hat **eine** Methode, die ausschließlich Daten
//! **herausliefert**: `read_events`. Es gibt keine zweite Methode, die
//! irgendetwas von außen entgegennähme — siehe die Crate-Dokumentation,
//! Abschnitt „Push-only", für die Begründung.
//!
//! [`FixtureFsEventSource`] ist die eine Implementierung dieser Crate: eine
//! Quelle, die eine vorab konfigurierte Liste von [`RawFsEvent`] ausliefert.
//! Sie steht in der **normalen** API, nicht hinter `#[cfg(test)]` — der
//! Binary-Knoten (fanotify-Bindung) und die Regel-Crate brauchen sie für
//! ihre eigenen Prüfungen gegen deterministische, privilegienlose Eingaben.
//!
//! # Warum die echte fanotify-Bindung nicht hier steht
//! Sie existiert inzwischen — über `nix::sys::fanotify` — aber im
//! privilegierten Binary-Knoten `harw-probe-fs` (`src/source.rs`), nicht in
//! dieser Crate. Siehe Crate-Dokumentation, Abschnitt „Stand der
//! fanotify-Bindung", für Begründung und Quellen.
//!
//! # Exportierte Typen
//! [`RawFsEvent`], [`FsEventSource`], [`FixtureFsEventSource`].
//!
//! # Nebenläufigkeit
//! [`RawFsEvent`] ist ein reiner Wert: `Send + Sync`, `Clone`. `FsEventSource:
//! Send + Sync` — eine künftige echte Bindung kann hinter `Arc` gehalten und
//! aus einem Sammelthread aufgerufen werden. [`FixtureFsEventSource`] hält
//! ihre Ereignisliste unveränderlich (kein inneres Locking nötig) und ist
//! deshalb ebenfalls `Send + Sync` ohne zusätzliche Synchronisation.
//!
//! # Fehler
//! [`crate::error::FsMonError`] — siehe dessen Moduldoku.
//!
//! # Examples
//! ```rust
//! use harw_dod_fsmon::raw::{FixtureFsEventSource, FsEventSource, RawFsEvent};
//! use std::time::Duration;
//!
//! let events = vec![RawFsEvent {
//!     mask: 0x08, // FAN_CLOSE_WRITE
//!     pid: 4242,
//!     uid: 1000,
//!     fd_target: "/etc/passwd".to_owned(),
//! }];
//! let source = FixtureFsEventSource::new(events.clone());
//!
//! let read = source.read_events(Duration::from_millis(10)).expect("Fixture liefert immer");
//! assert_eq!(read, events);
//! ```

use std::time::Duration;

use crate::error::FsMonError;

/// Ein rohes fanotify-Ereignis: die vier Ausgangsgrößen des Formungsteils.
///
/// # Description
/// Enthält ausschließlich Zahlen und eine bereits aufgelöste Zeichenkette —
/// **niemals** Dateiinhalt. `fd_target` ist das rohe Linkziel des
/// Dateideskriptors, wie es der Bindungsteil über `/proc/self/fd/<n>`
/// gelesen hat (siehe [`crate::fdpath`] für die Deutung dieses Werts).
///
/// # Errors
/// Keine eigenen — dieser Typ ist ein reiner Datencontainer.
///
/// # Examples
/// ```rust
/// use harw_dod_fsmon::raw::RawFsEvent;
///
/// let event = RawFsEvent {
///     mask: 0x02, // FAN_MODIFY
///     pid: 100,
///     uid: 0,
///     fd_target: "/etc/shadow".to_owned(),
/// };
/// assert_eq!(event.pid, 100);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFsEvent {
    /// Die rohe fanotify-Ereignismaske, unverändert wie vom Kernel geliefert.
    pub mask: u64,
    /// Die Prozess-ID, die fanotify als Auslöser meldet.
    pub pid: u32,
    /// Die Ausführungs-UID des Auslösers (`Actor::uid`) — von der loginuid
    /// zu unterscheiden, siehe [`crate::loginuid`].
    pub uid: u32,
    /// Das rohe Linkziel des Dateideskriptor-Symlinks, wie vom Bindungsteil
    /// gelesen. Siehe [`crate::fdpath::interpret_fd_target`] für die
    /// Deutung.
    pub fd_target: String,
}

/// Eine Quelle für Dateisystem-Ereignisse.
///
/// # Description
/// Die einzige Schnittstelle zum Bindungsteil. Trägt genau eine Methode, die
/// ausschließlich Daten **herausliefert** — es gibt bewusst kein
/// `handle_command`, kein `set_watch_from`, keine zweite Methode, über die
/// von außen irgendetwas in den Wächter hineingesteuert werden könnte. Ein
/// Prozess mit `CAP_SYS_ADMIN`, der Nachrichten annimmt, ist ein
/// Angriffsziel; einer, der nur sendet, ist keins (siehe Crate-Dokumentation,
/// Abschnitt „Push-only").
///
/// # Errors
/// [`FsMonError`], wenn das Lesen der zugrunde liegenden Quelle scheitert.
///
/// # Concurrency
/// `Send + Sync`: eine Implementierung kann hinter `Arc` gehalten und aus
/// einem Sammelthread aufgerufen werden.
///
/// # Examples
/// ```rust
/// use harw_dod_fsmon::raw::{FixtureFsEventSource, FsEventSource};
/// use std::time::Duration;
///
/// let source: Box<dyn FsEventSource> = Box::new(FixtureFsEventSource::new(vec![]));
/// let events = source.read_events(Duration::from_millis(1)).expect("liest");
/// assert!(events.is_empty());
/// ```
pub trait FsEventSource: Send + Sync {
    /// Liest die nächste Charge roher Ereignisse.
    ///
    /// # Arguments
    /// - `timeout` (`std::time::Duration`): wie lange höchstens auf
    ///   Ereignisse gewartet werden soll. Eine Implementierung ohne echte
    ///   Blockierung (wie [`FixtureFsEventSource`]) darf diesen Parameter
    ///   ignorieren.
    ///
    /// # Returns
    /// Die gelesenen [`RawFsEvent`], in Ankunftsreihenfolge. Kann leer sein.
    ///
    /// # Errors
    /// [`FsMonError`], wenn das Lesen der Quelle scheitert.
    fn read_events(&self, timeout: Duration) -> Result<Vec<RawFsEvent>, FsMonError>;
}

/// Eine Quelle, die Ereignisse aus einer vorab konfigurierten Liste liefert.
///
/// # Description
/// Liefert bei jedem Aufruf von [`FsEventSource::read_events`] die
/// vollständige, unveränderte Ereignisliste zurück, mit der sie konstruiert
/// wurde — unabhängig vom übergebenen `timeout`. Steht in der normalen API
/// (nicht hinter `#[cfg(test)]`), weil sowohl der Binary-Knoten als auch die
/// Regel-Crate sie für ihre eigenen, privilegienlosen Prüfungen brauchen.
///
/// # Errors
/// [`FsEventSource::read_events`] auf dieser Quelle schlägt nie fehl.
///
/// # Concurrency
/// Hält ihre Ereignisliste unveränderlich; `Send + Sync` ohne inneres
/// Locking.
///
/// # Examples
/// ```rust
/// use harw_dod_fsmon::raw::{FixtureFsEventSource, FsEventSource, RawFsEvent};
/// use std::time::Duration;
///
/// let event = RawFsEvent {
///     mask: 0x08,
///     pid: 1,
///     uid: 0,
///     fd_target: "/etc/hosts".to_owned(),
/// };
/// let source = FixtureFsEventSource::new(vec![event.clone()]);
///
/// assert_eq!(
///     source.read_events(Duration::ZERO).expect("liest"),
///     vec![event]
/// );
/// ```
#[derive(Debug, Clone, Default)]
pub struct FixtureFsEventSource {
    events: Vec<RawFsEvent>,
}

impl FixtureFsEventSource {
    /// Erzeugt eine Fixture-Quelle mit einer festen Ereignisliste.
    ///
    /// # Arguments
    /// - `events` (`Vec<RawFsEvent>`): die Ereignisse, die jeder Aufruf von
    ///   [`FsEventSource::read_events`] zurückgeben soll.
    ///
    /// # Returns
    /// Eine neue [`FixtureFsEventSource`].
    ///
    /// # Errors
    /// Keine — der Konstruktor ist total.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_fsmon::raw::FixtureFsEventSource;
    ///
    /// let _source = FixtureFsEventSource::new(vec![]);
    /// ```
    #[must_use]
    pub fn new(events: Vec<RawFsEvent>) -> Self {
        Self { events }
    }
}

impl FsEventSource for FixtureFsEventSource {
    fn read_events(&self, _timeout: Duration) -> Result<Vec<RawFsEvent>, FsMonError> {
        Ok(self.events.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{FixtureFsEventSource, FsEventSource, RawFsEvent};
    use crate::test_support::{TestResult, ctx};

    fn sample_event() -> RawFsEvent {
        RawFsEvent {
            mask: 0x08,
            pid: 42,
            uid: 1000,
            fd_target: "/srv/data/report.csv".to_owned(),
        }
    }

    #[test]
    fn test_fixture_source_returns_configured_events() -> TestResult {
        let source = FixtureFsEventSource::new(vec![sample_event()]);
        let read = source
            .read_events(Duration::from_millis(5))
            .map_err(ctx("Fixture-Quelle scheitert nie"))?;
        assert_eq!(read, vec![sample_event()]);
        Ok(())
    }

    #[test]
    fn test_fixture_source_empty_list_returns_empty_vec() -> TestResult {
        let source = FixtureFsEventSource::new(vec![]);
        let read = source
            .read_events(Duration::ZERO)
            .map_err(ctx("Fixture-Quelle scheitert nie"))?;
        assert!(read.is_empty());
        Ok(())
    }

    #[test]
    fn test_fixture_source_ignores_timeout_and_is_repeatable() -> TestResult {
        let source = FixtureFsEventSource::new(vec![sample_event()]);
        let first = source
            .read_events(Duration::from_secs(0))
            .map_err(ctx("liest"))?;
        let second = source
            .read_events(Duration::from_secs(60))
            .map_err(ctx("liest"))?;
        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn test_fixture_source_is_usable_as_boxed_trait_object() -> TestResult {
        let boxed: Box<dyn FsEventSource> =
            Box::new(FixtureFsEventSource::new(vec![sample_event()]));
        let read = boxed.read_events(Duration::ZERO).map_err(ctx("liest"))?;
        assert_eq!(read.len(), 1);
        Ok(())
    }
}
