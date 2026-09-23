//! `AuthRecord`: ein beobachteter Anmeldeversuch, wie ihn ein
//! [`crate::backend::AuthBackend`] liefert.
//!
//! # Verantwortungsbereich
//! Trägt ausschließlich Daten, keine Parselogik: wann ein Anmeldeversuch
//! beobachtet wurde, wer ihn ausgelöst hat (`harw_dod_signals::Actor`, mit
//! `uid` und `auid` getrennt geführt — siehe Crate-Dokumentation für die
//! Begründung, warum beide Felder auseinanderfallen können) und ob er gelang
//! oder scheiterte (`harw_dod_signals::AuthOutcome`). Der natürliche nächste
//! Schritt für einen Konsumenten ist, aus einem `AuthRecord` und einer
//! `harw_types::SensorId` ein `harw_dod_signals::SecurityEvent` mit
//! `EventKind::AuthEvent { outcome }` zu bauen — das geschieht bewusst
//! **nicht** hier, weil diese Crate keine `SensorId` besitzt (sie liegt beim
//! aufrufenden Sensor/Sentinel, nicht beim Backend).
//!
//! # Nebenläufigkeit
//! Reiner, unveränderlicher Werttyp: `Send + Sync` automatisch, kein
//! internes Locking.
//!
//! # Fehler
//! Keine eigenen — dieses Modul definiert nur Daten und liest keine Quelle.
//!
//! # Examples
//! ```rust
//! use harw_dod_authlog::AuthRecord;
//! use harw_dod_signals::{Actor, AuthOutcome};
//!
//! let record = AuthRecord {
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: Actor {
//!         uid: 0,
//!         auid: Some(1000),
//!         cgroup: None,
//!     },
//!     outcome: AuthOutcome::Success,
//! };
//! assert_ne!(record.actor.uid, record.actor.auid.unwrap());
//! ```

use harw_dod_signals::{Actor, AuthOutcome};
use jiff::Timestamp;

/// Ein beobachteter Anmeldeversuch.
///
/// # Description
/// Der gemeinsame Ergebnistyp beider `AuthBackend`-Implementierungen dieser
/// Crate. `actor.auid` ist bereits normalisiert: der Kernel-Sentinelwert für
/// „keine Anmelde-UID gesetzt" (`4294967295`) erscheint hier als `None`, nie
/// als der rohe Zahlenwert (siehe Crate-Dokumentation für die Begründung).
#[derive(Debug, Clone, PartialEq)]
pub struct AuthRecord {
    /// Wann der Anmeldeversuch beobachtet wurde.
    pub observed_at: Timestamp,
    /// Wer den Anmeldeversuch ausgelöst hat. `actor.uid` ist die aktuelle
    /// Ausführungs-UID, `actor.auid` die Anmelde-UID — beide können nach
    /// einem `sudo`-artigen Rechtewechsel auseinanderfallen.
    pub actor: Actor,
    /// Ob der Anmeldeversuch gelang oder scheiterte.
    pub outcome: AuthOutcome,
}

#[cfg(test)]
mod tests {
    use super::AuthRecord;
    use crate::test_support::{TestError, TestResult};
    use harw_dod_signals::{Actor, AuthOutcome};
    use jiff::Timestamp;

    fn record() -> AuthRecord {
        AuthRecord {
            observed_at: Timestamp::UNIX_EPOCH,
            actor: Actor {
                uid: 0,
                auid: Some(1000),
                cgroup: None,
            },
            outcome: AuthOutcome::Success,
        }
    }

    #[test]
    fn test_auth_record_equality_compares_all_fields() {
        let a = record();
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn test_auth_record_uid_and_auid_can_diverge() -> TestResult {
        let record = record();
        let auid = record
            .actor
            .auid
            .ok_or(TestError::Missing("auid present"))?;
        assert_ne!(record.actor.uid, auid);
        Ok(())
    }

    #[test]
    fn test_auth_record_inequality_when_outcome_differs() {
        let mut other = record();
        other.outcome = AuthOutcome::Failure;
        assert_ne!(record(), other);
    }
}
