//! Audit-Backend: Anmeldeereignisse über den `AUDIT`-Netlink-Kanal.
//!
//! # Verantwortungsbereich
//! [`AuditBackend`] implementiert [`crate::backend::AuthBackend`] über die
//! gelandete Zugriffsschicht `harw-dod-netlink`: es liest Rohrecords über
//! `harw_dod_netlink::AuditSource`, parst sie mit
//! `harw_dod_netlink::parse_record` (**kein Zweitparsen** — siehe
//! `harw-dod-netlink`-Crate-Dokumentation) und behält nur die Records, die
//! ein `success`-Feld tragen — der Marker, den diese Crate als „das war ein
//! Anmeldeversuch" liest. Ein Record ohne `success`-Feld (z. B. ein reiner
//! `SYSCALL`-Record) ist für diese Crate kein Anmeldeereignis und wird
//! stillschweigend übersprungen, nicht als Fehler gewertet.
//!
//! # `auid`-Normalisierung: bereits erledigt, hier nicht wiederholt
//! `harw_dod_netlink::parse_record` normalisiert den Kernel-Sentinelwert
//! `4294967295` bereits zu `None` (siehe dortige Dokumentation und Tests).
//! Dieses Modul übernimmt `AuditFields::auid` unverändert in
//! `harw_dod_signals::Actor::auid`, ohne die Prüfung ein zweites Mal
//! durchzuführen — zwei Stellen für dieselbe Regel driften sonst
//! auseinander.
//!
//! # Warum `uid` und der Zeitstempel Pflicht sind
//! Ein Record mit `success`-Feld, aber ohne `uid` oder ohne `msg`-Zeitstempel
//! kann nicht zu einem vollständigen [`crate::record::AuthRecord`] werden —
//! `harw_dod_signals::Actor::uid` ist kein `Option`, und ein Ereignis ohne
//! Zeitpunkt ließe sich weder gegen `since` filtern noch später korrelieren.
//! Beide Fälle ergeben deshalb
//! [`harw_dod_cap::SensorError::MalformedSource`], inhaltsfrei — nie den
//! Recordinhalt selbst.
//!
//! # Exportierte Typen
//! [`AuditBackend`], [`DEFAULT_READ_TIMEOUT`].
//!
//! # Nebenläufigkeit
//! `AuditBackend: Send + Sync`, weil `harw_dod_netlink::AuditSource: Send +
//! Sync` verlangt. Zustandslos zwischen Aufrufen: [`AuditBackend`] hält
//! keinen internen Fortschrittszeiger, `since` kommt bei jedem Aufruf vom
//! Aufrufer.
//!
//! # Fehler
//! [`crate::error::AuthlogError`] — Fehler aus `harw_dod_netlink::NetlinkError`
//! werden auf diese eine, inhaltsfreie Fehlermenge abgebildet (private
//! Zuordnungsfunktion in diesem Modul).
//!
//! # Examples
//! ```rust
//! use harw_dod_authlog::{AuditBackend, AuthBackend};
//! use harw_dod_netlink::{FixtureAuditSource, RawRecord};
//!
//! let source = FixtureAuditSource::new([RawRecord::new(
//!     "type=USER_LOGIN msg=audit(1699999999.000:1): auid=1000 uid=0 success=yes",
//! )]);
//! let backend = AuditBackend::new(Box::new(source));
//!
//! let events = backend
//!     .read_events(jiff::Timestamp::UNIX_EPOCH)
//!     .expect("Fixture liefert wohlgeformte Records");
//! // Der sudo-Fall: uid ist 0, auid bleibt der ursprüngliche Anmelder.
//! assert_eq!(events[0].actor.uid, 0);
//! assert_eq!(events[0].actor.auid, Some(1000));
//! ```

use std::time::Duration;

use harw_dod_cap::{Capability, SensorError};
use harw_dod_netlink::{AuditSource, NetlinkError, parse_record};
use harw_dod_signals::{Actor, AuthOutcome};
use jiff::Timestamp;

use crate::backend::AuthBackend;
use crate::error::AuthlogError;
use crate::record::AuthRecord;

/// Voreingestellte Wartezeit für [`AuditBackend::new`]: **eine Sekunde**.
///
/// Ein Sentinel pollt Sensoren periodisch; eine Sekunde ist kurz genug, um
/// einen Poll-Zyklus nicht spürbar zu verzögern, und lang genug, um dem
/// Kernel-Audit-Subsystem Zeit für neu anfallende Records zu geben.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Anmeldeereignisse über den `AUDIT`-Netlink-Kanal.
///
/// # Description
/// Hält eine beliebige `harw_dod_netlink::AuditSource` hinter einem
/// Trait-Objekt — in Produktion `harw_dod_netlink::socket::NetlinkAuditSource`
/// (nur unter Linux, braucht `CAP_AUDIT_READ`/root), in Tests
/// `harw_dod_netlink::FixtureAuditSource`. Diese Crate bindet sich nie an
/// eine konkrete Quelle.
pub struct AuditBackend {
    source: Box<dyn AuditSource>,
    timeout: Duration,
}

impl AuditBackend {
    /// Baut ein Audit-Backend mit [`DEFAULT_READ_TIMEOUT`].
    ///
    /// # Arguments
    /// - `source` (`Box<dyn harw_dod_netlink::AuditSource>`): die Quelle für
    ///   Rohrecords.
    ///
    /// # Returns
    /// Ein `AuditBackend`, dessen `read_events` `source` mit
    /// [`DEFAULT_READ_TIMEOUT`] abruft.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_authlog::AuditBackend;
    /// use harw_dod_netlink::FixtureAuditSource;
    ///
    /// let _backend = AuditBackend::new(Box::new(FixtureAuditSource::default()));
    /// ```
    #[must_use]
    pub fn new(source: Box<dyn AuditSource>) -> Self {
        Self::with_timeout(source, DEFAULT_READ_TIMEOUT)
    }

    /// Baut ein Audit-Backend mit einer eigenen Wartezeit.
    ///
    /// # Arguments
    /// - `source` (`Box<dyn harw_dod_netlink::AuditSource>`): die Quelle für
    ///   Rohrecords.
    /// - `timeout` (`std::time::Duration`): maximale Wartezeit je
    ///   `read_records`-Aufruf, siehe
    ///   `harw_dod_netlink::AuditSource::read_records`.
    ///
    /// # Returns
    /// Ein `AuditBackend`, dessen `read_events` `source` mit `timeout`
    /// abruft.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_authlog::AuditBackend;
    /// use harw_dod_netlink::FixtureAuditSource;
    /// use std::time::Duration;
    ///
    /// let _backend = AuditBackend::with_timeout(
    ///     Box::new(FixtureAuditSource::default()),
    ///     Duration::from_millis(50),
    /// );
    /// ```
    #[must_use]
    pub fn with_timeout(source: Box<dyn AuditSource>, timeout: Duration) -> Self {
        Self { source, timeout }
    }
}

impl std::fmt::Debug for AuditBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Zeigt nur die Wartezeit — die gehaltene Quelle ist ein
        // Trait-Objekt ohne `Debug`-Zusage und könnte ohnehin Recordinhalt
        // puffern.
        f.debug_struct("AuditBackend")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl AuthBackend for AuditBackend {
    /// Liest und parst Rohrecords aus der gehaltenen Quelle, siehe
    /// Moduldokumentation für die Filter- und Normalisierungsregeln.
    ///
    /// # Errors
    /// [`AuthlogError`], abgebildet aus `harw_dod_netlink::NetlinkError`;
    /// zusätzlich [`harw_dod_cap::SensorError::MalformedSource`], wenn ein
    /// Record ein `success`-Feld, aber kein `uid`- oder kein
    /// `msg`-Zeitstempelfeld trägt.
    fn read_events(&self, since: Timestamp) -> Result<Vec<AuthRecord>, AuthlogError> {
        let raw_records = self
            .source
            .read_records(self.timeout)
            .map_err(map_netlink_error)?;

        let mut events = Vec::with_capacity(raw_records.len());
        for raw in &raw_records {
            let fields = parse_record(raw).map_err(map_netlink_error)?;

            let Some(success) = fields.success else {
                // Kein Anmeldeereignis (z. B. ein reiner SYSCALL-Record).
                continue;
            };
            let uid = fields
                .uid
                .ok_or_else(|| AuthlogError::from(SensorError::MalformedSource))?;
            let observed_at = fields
                .timestamp
                .ok_or_else(|| AuthlogError::from(SensorError::MalformedSource))?;

            if observed_at < since {
                continue;
            }

            events.push(AuthRecord {
                observed_at,
                actor: Actor {
                    uid,
                    auid: fields.auid,
                    cgroup: None,
                },
                outcome: if success {
                    AuthOutcome::Success
                } else {
                    AuthOutcome::Failure
                },
            });
        }

        Ok(events)
    }

    /// Liefert immer `harw_dod_cap::Capability::ReadAuditNetlink`.
    fn capability(&self) -> Capability {
        Capability::ReadAuditNetlink
    }
}

/// Bildet einen `harw_dod_netlink::NetlinkError` auf [`AuthlogError`] ab.
///
/// # Description
/// `NetlinkError::MalformedRecord` wird zu
/// [`harw_dod_cap::SensorError::MalformedSource`] — dieselbe, inhaltsfreie
/// Vokabel, die [`AuthBackend::read_events`](crate::backend::AuthBackend::read_events)
/// oben auch für ein unvollständiges, aber grammatikalisch gültiges Record
/// benutzt. `NetlinkError::Sensor` wird unverändert durchgereicht.
fn map_netlink_error(err: NetlinkError) -> AuthlogError {
    match err {
        NetlinkError::MalformedRecord => AuthlogError::from(SensorError::MalformedSource),
        NetlinkError::Sensor(inner) => AuthlogError::from(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_dod_netlink::{FixtureAuditSource, RawRecord};

    fn backend_with(records: Vec<RawRecord>) -> AuditBackend {
        AuditBackend::new(Box::new(FixtureAuditSource::new(records)))
    }

    #[test]
    fn test_capability_is_read_audit_netlink() {
        let backend = backend_with(Vec::new());
        assert_eq!(backend.capability(), Capability::ReadAuditNetlink);
    }

    #[test]
    fn test_read_events_successful_login_yields_success_outcome() {
        let backend = backend_with(vec![RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=1000 uid=1000 success=yes",
        )]);
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("wohlgeformter Record");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].outcome, AuthOutcome::Success);
    }

    #[test]
    fn test_read_events_failed_login_yields_failure_outcome() {
        let backend = backend_with(vec![RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=1000 uid=1000 success=no",
        )]);
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("wohlgeformter Record");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].outcome, AuthOutcome::Failure);
    }

    #[test]
    fn test_read_events_auid_sentinel_max_value_is_none() {
        // Der wichtigste Test dieser Crate zusammen mit dem sudo-Fall unten:
        // 4294967295 (u32::MAX, -1 als u32) heißt "keine Anmelde-UID
        // gesetzt" und muss None ergeben.
        let backend = backend_with(vec![RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=4294967295 uid=0 success=yes",
        )]);
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("wohlgeformter Record");
        assert_eq!(events[0].actor.auid, None);
    }

    #[test]
    fn test_read_events_auid_zero_is_some_zero() {
        // Abgrenzung zum Sentinelwert: auid=0 ist root, der sich tatsächlich
        // angemeldet hat, kein "nicht gesetzt".
        let backend = backend_with(vec![RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=0 uid=0 success=yes",
        )]);
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("wohlgeformter Record");
        assert_eq!(events[0].actor.auid, Some(0));
    }

    #[test]
    fn test_read_events_sudo_case_keeps_uid_and_auid_separate() {
        // Der Test, der die Crate rechtfertigt: uid=0 (root durch sudo),
        // auid=1000 (wer die Kette wirklich ausgelöst hat) — beide Werte
        // erscheinen getrennt, und auid ist der, der nicht null ist.
        let backend = backend_with(vec![RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=1000 uid=0 success=yes",
        )]);
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("wohlgeformter Record");
        assert_eq!(events[0].actor.uid, 0);
        assert_eq!(events[0].actor.auid, Some(1000));
        assert_ne!(events[0].actor.auid.expect("auid present"), 0);
    }

    #[test]
    fn test_read_events_skips_records_without_success_field() {
        let backend = backend_with(vec![RawRecord::new(
            "type=SYSCALL msg=audit(1699999999.000:1): auid=1000 uid=1000",
        )]);
        let events = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect("wohlgeformter Record ohne success wird übersprungen");
        assert!(events.is_empty());
    }

    #[test]
    fn test_read_events_filters_by_since() {
        let backend = backend_with(vec![
            RawRecord::new(
                "type=USER_LOGIN msg=audit(1000.000:1): auid=1000 uid=1000 success=yes",
            ),
            RawRecord::new(
                "type=USER_LOGIN msg=audit(2000.000:2): auid=1000 uid=1000 success=yes",
            ),
        ]);
        let since = Timestamp::new(1500, 0).expect("gültiger Zeitstempel");
        let events = backend.read_events(since).expect("wohlgeformte Records");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].observed_at.as_second(), 2000);
    }

    #[test]
    fn test_read_events_malformed_record_yields_malformed_source() {
        let backend = backend_with(vec![RawRecord::new(
            "type=USER_LOGIN auid=not-a-number",
        )]);
        let err = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect_err("nicht-numerischer auid muss scheitern");
        assert!(matches!(
            err,
            AuthlogError::Sensor(SensorError::MalformedSource)
        ));
    }

    #[test]
    fn test_read_events_missing_uid_yields_malformed_source() {
        let backend = backend_with(vec![RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=1000 success=yes",
        )]);
        let err = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect_err("fehlendes uid-Feld muss scheitern");
        assert!(matches!(
            err,
            AuthlogError::Sensor(SensorError::MalformedSource)
        ));
    }

    #[test]
    fn test_read_events_missing_timestamp_yields_malformed_source() {
        let backend = backend_with(vec![RawRecord::new(
            "type=USER_LOGIN auid=1000 uid=1000 success=yes",
        )]);
        let err = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect_err("fehlender Zeitstempel muss scheitern");
        assert!(matches!(
            err,
            AuthlogError::Sensor(SensorError::MalformedSource)
        ));
    }

    #[test]
    fn test_error_message_never_contains_username_hostname_or_terminal() {
        let backend = backend_with(vec![RawRecord::new(
            r#"type=USER_LOGIN acct="alice" hostname=workstation42 terminal=pts/3 auid=not-a-number"#,
        )]);
        let err = backend
            .read_events(Timestamp::UNIX_EPOCH)
            .expect_err("kaputter Record muss scheitern");
        let message = err.to_string();
        assert!(!message.contains("alice"));
        assert!(!message.contains("workstation42"));
        assert!(!message.contains("pts/3"));
    }
}
