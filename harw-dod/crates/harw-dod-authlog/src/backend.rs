//! `trait AuthBackend`: die eine Schnittstelle für beide Anmelde-Quellen.
//!
//! # Verantwortungsbereich
//! Dieser Trait ist die dokumentierte Ausnahme von „genau eine Quelle je
//! Sensor-Crate" (siehe Crate-Dokumentation für die volle Begründung): das
//! Journal- und das Audit-Backend beantworten dieselbe Frage — wer hat sich
//! wann erfolgreich oder erfolglos angemeldet? — aus zwei verschiedenen
//! Quellen, hinter demselben Trait. Jede Implementierung hält für sich
//! trotzdem **genau eine** `harw_dod_cap::Capability`; die Rechtematrix
//! bleibt damit exakt.
//!
//! # Ein fehlerhafter Record blendet den Abruf nicht
//! [`AuthBackend::read_batch`] liefert neben den Anmeldeereignissen die Zahl
//! der übersprungenen, fehlerhaften Records ([`AuthBatch::malformed`]). Ein
//! einzelner kaputter Record lässt damit nicht den ganzen Abruf scheitern,
//! wird aber auch nicht still verschluckt: der Aufrufer
//! ([`crate::sensor::AuthlogSensor`]) meldet die Zahl als Messwert. Die
//! Vorgabe-Implementierung für Backends ohne eigene Zählung ruft
//! [`AuthBackend::read_events`] auf und meldet `0`.
//!
//! # Exportierte Typen
//! [`AuthBackend`], [`AuthBatch`].
//!
//! # Nebenläufigkeit
//! `Send + Sync`: ein Sentinel darf ein Backend aus einem beliebigen Thread
//! pollen.
//!
//! # Fehler
//! [`crate::error::AuthlogError`], inhaltsfrei.
//!
//! # Examples
//! ```rust
//! use harw_dod_authlog::{AuthBackend, AuthRecord, FixtureAuthBackend};
//! use harw_dod_cap::Capability;
//!
//! let backend: Box<dyn AuthBackend> = Box::new(FixtureAuthBackend::new(
//!     Vec::<AuthRecord>::new(),
//!     Capability::ReadAuditNetlink,
//! ));
//! assert_eq!(backend.capability(), Capability::ReadAuditNetlink);
//! ```

use jiff::Timestamp;

use harw_dod_cap::Capability;

use crate::error::AuthlogError;
use crate::record::AuthRecord;

/// Das Ergebnis eines Abrufs: Anmeldeereignisse und übersprungene Records.
///
/// # Description
/// `records` trägt die Anmeldeereignisse in Quellreihenfolge, `malformed`
/// die Zahl der Records, die nicht parsebar oder als Anmeldeereignis
/// unvollständig waren (ohne Ergebnis, `uid` oder Zeitstempel) und deshalb
/// übersprungen wurden. Inhaltsfrei: kein übersprungener Record wird
/// mitgeführt, nur seine Anzahl.
///
/// # Concurrency
/// Reiner Werttyp: `Send + Sync`.
///
/// # Examples
/// ```rust
/// use harw_dod_authlog::AuthBatch;
///
/// let batch = AuthBatch::default();
/// assert!(batch.records.is_empty());
/// assert_eq!(batch.malformed, 0);
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuthBatch {
    /// Die Anmeldeereignisse mit `observed_at >= since`.
    pub records: Vec<AuthRecord>,
    /// Wie viele Records dieses Abrufs fehlerhaft waren und übersprungen
    /// wurden.
    pub malformed: usize,
}

/// Die eine Schnittstelle für beide Anmelde-Quellen.
///
/// # Description
/// Siehe Moduldokumentation für die Begründung, warum genau zwei
/// Implementierungen hinter diesem einen Trait zulässig sind. Ein Aufrufer
/// programmiert ausschließlich gegen diesen Trait und bindet sich nie an
/// eine konkrete Implementierung.
pub trait AuthBackend: Send + Sync {
    /// Liest Anmeldeereignisse seit `since`.
    ///
    /// # Arguments
    /// - `since` (`jiff::Timestamp`): injizierte untere Zeitgrenze. Nur
    ///   Ereignisse mit `observed_at >= since` erscheinen im Ergebnis.
    ///
    /// # Returns
    /// Die seit `since` beobachteten [`AuthRecord`]e, in der Reihenfolge, in
    /// der die zugrunde liegende Quelle sie liefert. Kann leer sein.
    ///
    /// # Errors
    /// [`AuthlogError`], wenn die Quelle nicht lesbar ist, außerhalb des
    /// Lesebereichs liegt oder eine unerwartete Form hat. Inhaltsfrei — siehe
    /// Crate-Dokumentation, Abschnitt „Der Inhalt ist heikel".
    fn read_events(&self, since: Timestamp) -> Result<Vec<AuthRecord>, AuthlogError>;

    /// Liest Anmeldeereignisse seit `since` samt der Zahl übersprungener,
    /// fehlerhafter Records.
    ///
    /// # Description
    /// Die Vorgabe ruft [`Self::read_events`] auf und meldet
    /// `malformed = 0` — richtig für jedes Backend, das keine Records
    /// überspringt. [`crate::audit_backend::AuditBackend`] überschreibt sie
    /// und zählt (siehe Moduldokumentation).
    ///
    /// # Arguments
    /// - `since` (`jiff::Timestamp`): wie bei [`Self::read_events`].
    ///
    /// # Returns
    /// Ein [`AuthBatch`].
    ///
    /// # Errors
    /// Wie [`Self::read_events`]: nur, wenn die Quelle als Ganzes nicht
    /// lesbar ist — nie wegen eines einzelnen fehlerhaften Records.
    fn read_batch(&self, since: Timestamp) -> Result<AuthBatch, AuthlogError> {
        Ok(AuthBatch {
            records: self.read_events(since)?,
            malformed: 0,
        })
    }

    /// Die Fähigkeit, die dieses Backend benutzt.
    ///
    /// # Returns
    /// Genau eine `harw_dod_cap::Capability` — nie mehr als eine pro
    /// Implementierung (siehe Moduldokumentation).
    fn capability(&self) -> Capability;
}

#[cfg(test)]
mod tests {
    use super::AuthBackend;
    use crate::fixture_backend::FixtureAuthBackend;
    use crate::record::AuthRecord;
    use harw_dod_cap::Capability;

    #[test]
    fn test_auth_backend_is_object_safe_as_boxed_trait_object() {
        let backend: Box<dyn AuthBackend> = Box::new(FixtureAuthBackend::new(
            Vec::<AuthRecord>::new(),
            Capability::ReadJournal,
        ));
        assert_eq!(backend.capability(), Capability::ReadJournal);
    }

    #[test]
    fn test_default_read_batch_wraps_read_events_with_zero_malformed()
    -> crate::test_support::TestResult {
        let backend = FixtureAuthBackend::new(
            [AuthRecord {
                observed_at: jiff::Timestamp::UNIX_EPOCH,
                actor: harw_dod_signals::Actor {
                    uid: 0,
                    auid: Some(1000),
                    cgroup: None,
                    process: None,
                },
                outcome: harw_dod_signals::AuthOutcome::Failure,
            }],
            Capability::ReadAuditNetlink,
        );
        let batch = backend
            .read_batch(jiff::Timestamp::UNIX_EPOCH)
            .map_err(crate::test_support::ctx("Fixture scheitert nie"))?;
        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.malformed, 0);
        Ok(())
    }

    #[test]
    fn test_boxed_backends_can_be_held_in_a_heterogeneous_collection() {
        let backends: Vec<Box<dyn AuthBackend>> = vec![
            Box::new(FixtureAuthBackend::new(
                Vec::<AuthRecord>::new(),
                Capability::ReadJournal,
            )),
            Box::new(FixtureAuthBackend::new(
                Vec::<AuthRecord>::new(),
                Capability::ReadAuditNetlink,
            )),
        ];
        assert_eq!(backends.len(), 2);
    }
}
