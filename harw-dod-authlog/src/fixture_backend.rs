//! `FixtureAuthBackend`: eine feste, deterministische Anmelde-Quelle für Tests.
//!
//! # Verantwortungsbereich
//! **Keine Wegwerfattrappe:** Teil der normalen, öffentlichen API dieser
//! Crate, nicht hinter `#[cfg(test)]` — spätere Knoten (Regeln, Sentinel)
//! brauchen sie für ihre eigenen Tests, ohne selbst eine Audit- oder
//! Journal-Quelle simulieren zu müssen (Muster:
//! `harw_dod_netlink::FixtureAuditSource`).
//!
//! `capability()` ist bei Konstruktion frei wählbar: eine
//! `FixtureAuthBackend` kann damit sowohl das (nicht gebaute) Journal-
//! Backend als auch das Audit-Backend gegenüber einem Konsumenten
//! simulieren, der nur gegen [`crate::backend::AuthBackend`] programmiert
//! und die konkrete Quelle nie kennt.
//!
//! # Warum `since` hier gefiltert wird, anders als bei `FixtureAuditSource`
//! `harw_dod_netlink::FixtureAuditSource` ignoriert `timeout` und liefert
//! immer alle Records — sie ist eine untere Schicht, für die Filterung nicht
//! ihre Aufgabe ist (das übernimmt [`crate::audit_backend::AuditBackend`]
//! eine Ebene darüber). `FixtureAuthBackend` steht dagegen auf derselben
//! Ebene wie `AuditBackend` selbst — ein Konsument, der inkrementelles
//! Pollen testet, muss sich auf dieselbe `since`-Filterung verlassen können
//! wie beim echten Backend.
//!
//! # Exportierte Typen
//! [`FixtureAuthBackend`].
//!
//! # Nebenläufigkeit
//! Hält ihre Records unveränderlich (kein `Mutex`, keine innere
//! Veränderlichkeit) und liefert bei jedem Aufruf dieselbe, gefilterte
//! Ansicht zurück — sicher aus beliebig vielen Threads gleichzeitig lesbar.
//!
//! # Fehler
//! Keine — [`FixtureAuthBackend::read_events`] scheitert nie.
//!
//! # Examples
//! ```rust
//! use harw_dod_authlog::{AuthBackend, AuthRecord, FixtureAuthBackend};
//! use harw_dod_cap::Capability;
//! use harw_dod_signals::{Actor, AuthOutcome};
//!
//! let backend = FixtureAuthBackend::new(
//!     [AuthRecord {
//!         observed_at: jiff::Timestamp::UNIX_EPOCH,
//!         actor: Actor {
//!             uid: 0,
//!             auid: Some(1000),
//!             cgroup: None,
//!         },
//!         outcome: AuthOutcome::Success,
//!     }],
//!     Capability::ReadJournal,
//! );
//!
//! assert_eq!(backend.capability(), Capability::ReadJournal);
//! let events = backend
//!     .read_events(jiff::Timestamp::UNIX_EPOCH)
//!     .expect("Fixture scheitert nie");
//! assert_eq!(events.len(), 1);
//! ```

use jiff::Timestamp;

use harw_dod_cap::Capability;

use crate::backend::AuthBackend;
use crate::error::AuthlogError;
use crate::record::AuthRecord;

/// Eine feste Liste von [`AuthRecord`]en, gefiltert nach `since` wie ein
/// echtes Backend.
///
/// # Description
/// **Keine Wegwerfattrappe** — siehe Moduldokumentation. `capability()` ist
/// frei wählbar und simuliert damit wahlweise das Journal- oder das
/// Audit-Backend gegenüber einem Konsumenten, der nur gegen
/// [`AuthBackend`] programmiert.
#[derive(Debug, Clone)]
pub struct FixtureAuthBackend {
    records: Vec<AuthRecord>,
    capability: Capability,
}

impl FixtureAuthBackend {
    /// Baut eine Attrappe aus einer festen Liste von Records.
    ///
    /// # Arguments
    /// - `records` (`impl IntoIterator<Item = AuthRecord>`): die Records, aus
    ///   denen jeder [`Self::read_events`]-Aufruf nach `since` filtert.
    /// - `capability` (`harw_dod_cap::Capability`): die Fähigkeit, die
    ///   [`Self::capability`] zurückgibt.
    ///
    /// # Returns
    /// Eine `FixtureAuthBackend`, deren [`Self::read_events`] stets aus genau
    /// dieser Liste filtert.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_authlog::{AuthRecord, FixtureAuthBackend};
    /// use harw_dod_cap::Capability;
    ///
    /// let _backend =
    ///     FixtureAuthBackend::new(Vec::<AuthRecord>::new(), Capability::ReadAuditNetlink);
    /// ```
    #[must_use]
    pub fn new(records: impl IntoIterator<Item = AuthRecord>, capability: Capability) -> Self {
        Self {
            records: records.into_iter().collect(),
            capability,
        }
    }
}

impl AuthBackend for FixtureAuthBackend {
    /// Liefert die bei [`Self::new`] eingesetzten Records mit
    /// `observed_at >= since`, unverändert.
    ///
    /// # Errors
    /// Keine — diese Implementierung schlägt nie fehl.
    fn read_events(&self, since: Timestamp) -> Result<Vec<AuthRecord>, AuthlogError> {
        Ok(self
            .records
            .iter()
            .filter(|record| record.observed_at >= since)
            .cloned()
            .collect())
    }

    /// Liefert die bei [`Self::new`] übergebene Fähigkeit.
    fn capability(&self) -> Capability {
        self.capability
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_dod_signals::{Actor, AuthOutcome};

    fn record(second: i64, uid: u32, auid: Option<u32>, outcome: AuthOutcome) -> AuthRecord {
        AuthRecord {
            observed_at: Timestamp::new(second, 0).expect("gültiger Zeitstempel"),
            actor: Actor { uid, auid, cgroup: None },
            outcome,
        }
    }

    #[test]
    fn test_capability_reflects_constructor_argument_for_journal() {
        let backend = FixtureAuthBackend::new(Vec::<AuthRecord>::new(), Capability::ReadJournal);
        assert_eq!(backend.capability(), Capability::ReadJournal);
    }

    #[test]
    fn test_capability_reflects_constructor_argument_for_audit_netlink() {
        let backend =
            FixtureAuthBackend::new(Vec::<AuthRecord>::new(), Capability::ReadAuditNetlink);
        assert_eq!(backend.capability(), Capability::ReadAuditNetlink);
    }

    #[test]
    fn test_read_events_returns_success_and_failure_outcomes_unchanged() {
        let backend = FixtureAuthBackend::new(
            vec![
                record(1, 1000, Some(1000), AuthOutcome::Success),
                record(2, 1000, Some(1000), AuthOutcome::Failure),
            ],
            Capability::ReadJournal,
        );
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("Fixture scheitert nie");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].outcome, AuthOutcome::Success);
        assert_eq!(events[1].outcome, AuthOutcome::Failure);
    }

    #[test]
    fn test_read_events_preserves_auid_sentinel_normalization() {
        let backend = FixtureAuthBackend::new(
            vec![record(1, 0, None, AuthOutcome::Success)],
            Capability::ReadJournal,
        );
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("Fixture scheitert nie");
        assert_eq!(events[0].actor.auid, None);
    }

    #[test]
    fn test_read_events_preserves_auid_zero() {
        let backend = FixtureAuthBackend::new(
            vec![record(1, 0, Some(0), AuthOutcome::Success)],
            Capability::ReadJournal,
        );
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("Fixture scheitert nie");
        assert_eq!(events[0].actor.auid, Some(0));
    }

    #[test]
    fn test_read_events_sudo_case_keeps_uid_and_auid_separate() {
        let backend = FixtureAuthBackend::new(
            vec![record(1, 0, Some(1000), AuthOutcome::Success)],
            Capability::ReadAuditNetlink,
        );
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("Fixture scheitert nie");
        assert_eq!(events[0].actor.uid, 0);
        assert_eq!(events[0].actor.auid, Some(1000));
    }

    #[test]
    fn test_read_events_filters_by_since() {
        let backend = FixtureAuthBackend::new(
            vec![
                record(1000, 1000, Some(1000), AuthOutcome::Success),
                record(2000, 1000, Some(1000), AuthOutcome::Success),
            ],
            Capability::ReadJournal,
        );
        let since = Timestamp::new(1500, 0).expect("gültiger Zeitstempel");
        let events = backend.read_events(since).expect("Fixture scheitert nie");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].observed_at.as_second(), 2000);
    }

    #[test]
    fn test_read_events_returns_same_records_on_repeated_calls() {
        let backend = FixtureAuthBackend::new(
            vec![record(1, 1000, Some(1000), AuthOutcome::Success)],
            Capability::ReadJournal,
        );
        let first = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("erster Aufruf");
        let second = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("zweiter Aufruf");
        assert_eq!(first, second);
    }

    #[test]
    fn test_read_events_empty_fixture_returns_empty_vec() {
        let backend = FixtureAuthBackend::new(Vec::<AuthRecord>::new(), Capability::ReadJournal);
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("Fixture scheitert nie");
        assert!(events.is_empty());
    }
}
