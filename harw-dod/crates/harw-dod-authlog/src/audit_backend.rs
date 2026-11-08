//! Audit-Backend: Anmeldeereignisse über den `AUDIT`-Netlink-Kanal.
//!
//! # Verantwortungsbereich
//! [`AuditBackend`] implementiert [`crate::backend::AuthBackend`] über die
//! gelandete Zugriffsschicht `harw-dod-netlink`: es liest Rohrecords über
//! `harw_dod_netlink::AuditSource`, parst sie mit
//! `harw_dod_netlink::parse_record` (**kein Zweitparsen** — siehe
//! `harw-dod-netlink`-Crate-Dokumentation) und wandelt jeden Record mit
//! [`auth_record_from_raw`] in ein [`crate::record::AuthRecord`]. Beide
//! Eingabeformen gelten: die auditd-Textform (`type=… msg=audit(…): …`) und
//! die Kernel-Netlink-Nutzlast (`audit(…): …`, Recordart aus dem
//! `nlmsghdr`). [`auth_record_from_raw`] ist zugleich die öffentliche
//! Einzelrecord-Umwandlung, die `harw-probe-netlink` für seinen Datenstrom
//! benutzt — eine Stelle für die Regel, nicht zwei.
//!
//! # Welche Records Anmeldeereignisse sind
//! Ausschließlich die Recordarten in [`LOGIN_RECORD_TYPES`]: `USER_AUTH`
//! (eine PAM-Authentisierung, etwa die Passwortprüfung von `sshd`, `su` oder
//! `sudo`) und `USER_LOGIN` (das Ergebnis einer Anmeldung bei `sshd`,
//! `login`, einem Display-Manager). Jeder andere Record — `SYSCALL`,
//! `PROCTITLE`, das Kernel-`LOGIN` (die `auid`-Zuweisung, die jede Anmeldung
//! ohnehin als `USER_LOGIN` begleitet), `USER_CMD` (ein über `sudo`
//! ausgeführter Befehl ist keine Anmeldung) — ist für diese Crate kein
//! Anmeldeereignis und wird übersprungen, **nicht** als Fehler gewertet.
//! Früher galt „trägt ein `success`-Feld" als Marker; das machte jeden
//! `SYSCALL`-Record zu einem Anmeldeereignis und übersah jeden echten
//! `USER_*`-Record, der sein Ergebnis als `res=` trägt.
//!
//! Bei einem Netlink-Record steht die Recordart im `nlmsghdr`; sie wird
//! **vor** dem Parsen geprüft. Ein Record einer anderen Art wird deshalb nie
//! geparst und kann auch nie als fehlerhaft zählen — die Zählung
//! `malformed` sagt nur etwas über Anmelderecords. Nur die auditd-Textform
//! muss erst geparst werden, weil ihre Recordart im Text steht.
//!
//! # Mehrere Ereignisse je Anmeldeversuch
//! Ein Anmeldeversuch erzeugt in der Regel **mehr als ein**
//! Anmeldeereignis, und diese Crate fasst sie bewusst nicht zusammen (das
//! Ereignis trägt keine Stufe, an der man sie unterscheiden könnte):
//! - Ein falsches Passwort über `sshd` ergibt ein `USER_AUTH res=failed`
//!   (PAM) **und** ein `USER_LOGIN res=failed` (sshd); `sshd` meldet
//!   außerdem je gescheiterter Authentisierungsmethode ein weiteres
//!   `USER_LOGIN res=failed`.
//! - Eine erfolgreiche Anmeldung ergibt ein `USER_AUTH res=success` und ein
//!   `USER_LOGIN res=success`.
//!
//! `Actor::uid` ist dabei die UID des **meldenden Dienstes** (bei `sshd`
//! meist 0), nicht die des Zielkontos; `Actor::auid` ist die Anmelde-UID
//! dieses Prozesses (vor der Anmeldung oft `None`). Eine Regel, die
//! gescheiterte Anmeldungen zählt (Brute-Force, Rate), muss deshalb mit
//! etwa zwei oder mehr Ereignissen je Rateversuch rechnen und darf
//! `uid == 0` nicht als „Anmeldung als root" lesen.
//!
//! # `auid`-Normalisierung: bereits erledigt, hier nicht wiederholt
//! `harw_dod_netlink::parse_record` normalisiert den Kernel-Sentinelwert
//! `4294967295` bereits zu `None` (siehe dortige Dokumentation und Tests).
//! Dieses Modul übernimmt `AuditFields::auid` unverändert in
//! `harw_dod_signals::Actor::auid`, ohne die Prüfung ein zweites Mal
//! durchzuführen — zwei Stellen für dieselbe Regel driften sonst
//! auseinander.
//!
//! # Prozesskontext (PL-100 WP0.15)
//! Der Kernel schreibt die `pid` des meldenden Prozesses (`sshd`, `sudo`, …)
//! in den äußeren, von ihm geformten Teil jedes Records. Sie wird als
//! `harw_dod_signals::ProcessContext` mit nur der `pid` übernommen
//! (`ProcessContext::from_pid`, eine `0` bleibt „unbekannt"). Kein `comm`,
//! keine Kommandozeile.
//!
//! # Was ein fehlerhafter Record ist
//! Ein Anmelderecord ohne Ergebnis, ohne `uid` oder ohne Zeitstempel kann
//! nicht zu einem vollständigen [`crate::record::AuthRecord`] werden —
//! `harw_dod_signals::Actor::uid` ist kein `Option`, und ein Ereignis ohne
//! Zeitpunkt ließe sich weder gegen `since` filtern noch später korrelieren.
//! Zusammen mit nicht parsebaren Records ergibt das
//! [`harw_dod_cap::SensorError::MalformedSource`], inhaltsfrei — nie den
//! Recordinhalt selbst. [`AuditBackend`] überspringt solche Records und
//! **zählt** sie ([`crate::backend::AuthBatch::malformed`]): ein kaputter
//! Record lässt den Abruf der übrigen nicht mehr scheitern.
//!
//! # Exportierte Typen
//! [`AuditBackend`], [`DEFAULT_READ_TIMEOUT`], [`LOGIN_RECORD_TYPES`], die
//! freien Funktionen [`auth_record_from_raw`] und [`is_login_record_type`].
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
//! let source = FixtureAuditSource::new([
//!     // auditd-Textform.
//!     RawRecord::new(
//!         "type=USER_LOGIN msg=audit(1699999999.000:1): auid=1000 uid=0 success=yes",
//!     ),
//!     // Kernel-Netlink-Nutzlast: 1100 = USER_AUTH, Ergebnis in msg='…'.
//!     RawRecord::from_netlink(
//!         1100,
//!         "audit(1699999999.456:2): pid=4300 uid=0 auid=1000 msg='op=PAM:authentication res=failed'",
//!     ),
//! ]);
//! let backend = AuditBackend::new(Box::new(source));
//!
//! let events = backend.read_events(jiff::Timestamp::UNIX_EPOCH)?;
//! assert_eq!(events.len(), 2);
//! // Der sudo-Fall: uid ist 0, auid bleibt der ursprüngliche Anmelder.
//! assert!(events.iter().all(|event| event.actor.uid == 0));
//! assert!(events.iter().all(|event| event.actor.auid == Some(1000)));
//! # Ok::<(), harw_dod_authlog::AuthlogError>(())
//! ```

use std::time::Duration;

use harw_dod_cap::{Capability, SensorError};
use harw_dod_netlink::{AuditSource, NetlinkError, RawRecord, audit_type_name, parse_record};
use harw_dod_signals::{Actor, AuthOutcome, ProcessContext};
use jiff::Timestamp;

use crate::backend::{AuthBackend, AuthBatch};
use crate::error::AuthlogError;
use crate::record::AuthRecord;

/// Voreingestellte Wartezeit für [`AuditBackend::new`]: **eine Sekunde**.
///
/// Ein Sentinel pollt Sensoren periodisch; eine Sekunde ist kurz genug, um
/// einen Poll-Zyklus nicht spürbar zu verzögern, und lang genug, um dem
/// Kernel-Audit-Subsystem Zeit für neu anfallende Records zu geben.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Die Recordarten, die diese Crate als Anmeldeversuch liest.
///
/// # Description
/// Namen wie in der auditd-Textform bzw. wie
/// `harw_dod_netlink::audit_type_name` sie für die Netlink-Nummern liefert
/// (`USER_AUTH` = 1100, `USER_LOGIN` = 1112). Siehe Moduldokumentation für
/// die bewusst ausgelassenen Arten.
///
/// Beide Arten zusammen ergeben **mehrere** Ereignisse je Anmeldeversuch
/// (PAM-`USER_AUTH` plus `USER_LOGIN` des Dienstes, bei `sshd` zusätzlich
/// eines je gescheiterter Methode), und der `actor` ist der meldende
/// Dienst, nicht das Zielkonto — siehe Moduldokumentation, „Mehrere
/// Ereignisse je Anmeldeversuch".
pub const LOGIN_RECORD_TYPES: [&str; 2] = ["USER_AUTH", "USER_LOGIN"];

/// Ob eine Recordart ein Anmeldeereignis ist.
///
/// # Arguments
/// - `record_type` (`&str`): `AuditFields::record_type`.
///
/// # Returns
/// `true` genau für die Einträge in [`LOGIN_RECORD_TYPES`].
///
/// # Examples
/// ```rust
/// use harw_dod_authlog::is_login_record_type;
///
/// assert!(is_login_record_type("USER_AUTH"));
/// assert!(!is_login_record_type("USER_CMD"));
/// ```
#[must_use]
pub fn is_login_record_type(record_type: &str) -> bool {
    LOGIN_RECORD_TYPES.contains(&record_type)
}

/// Wandelt einen einzelnen Audit-Record in ein Anmeldeereignis.
///
/// # Description
/// Trägt `raw` eine Recordart aus dem `nlmsghdr`, wird sie **zuerst**
/// geprüft: jede Art außerhalb von [`LOGIN_RECORD_TYPES`] (auch eine
/// unbekannte Nummer) ergibt sofort `Ok(None)`, ohne dass die Nutzlast
/// geparst wird. Sonst parst die Funktion `raw` mit
/// `harw_dod_netlink::parse_record` (beide Eingabeformen), lässt bei der
/// Textform jede Recordart außerhalb von [`LOGIN_RECORD_TYPES`] fallen und
/// baut aus einem Anmelderecord ein [`AuthRecord`]: `uid` und `auid`
/// getrennt, `pid` als Prozesskontext, das Ergebnis aus `success=`/`res=`,
/// der Zeitstempel aus `audit(…)`. Filtert **nicht** nach Zeit — das ist
/// Sache des Aufrufers.
///
/// # Arguments
/// - `raw` (`&harw_dod_netlink::RawRecord`): der Rohrecord.
///
/// # Returns
/// `Ok(Some(record))` für ein vollständiges Anmeldeereignis, `Ok(None)` für
/// jede andere Recordart.
///
/// # Errors
/// [`AuthlogError`] mit [`SensorError::MalformedSource`], wenn ein
/// Anmelderecord (bzw. ein Record der Textform) nicht parsebar ist oder ein
/// Anmelderecord kein Ergebnis, keine `uid` oder keinen Zeitstempel trägt.
/// Ein Netlink-Record einer anderen Art ist nie fehlerhaft. Inhaltsfrei.
///
/// # Concurrency
/// Zustandslos; aus beliebig vielen Threads gleichzeitig aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_dod_authlog::auth_record_from_raw;
/// use harw_dod_netlink::RawRecord;
/// use harw_dod_signals::AuthOutcome;
///
/// let raw = RawRecord::from_netlink(
///     1112,
///     "audit(1699999999.123:456): pid=4242 uid=0 auid=1000 ses=2 msg='op=login id=1000 res=success'",
/// );
/// let record = auth_record_from_raw(&raw)?.ok_or(harw_dod_authlog::AuthlogError::from(
///     harw_dod_cap::SensorError::MalformedSource,
/// ))?;
/// assert_eq!(record.outcome, AuthOutcome::Success);
/// assert_eq!(record.actor.process.map(|process| process.pid), Some(4242));
///
/// let other = RawRecord::from_netlink(1300, "audit(1699999999.123:457): uid=0 success=yes");
/// assert_eq!(auth_record_from_raw(&other)?, None);
/// # Ok::<(), harw_dod_authlog::AuthlogError>(())
/// ```
pub fn auth_record_from_raw(raw: &RawRecord) -> Result<Option<AuthRecord>, AuthlogError> {
    // Netlink: die Recordart steht im Kopf; keine Anmeldeart, kein Parsen.
    if let Some(code) = raw.msg_type()
        && !audit_type_name(code).is_some_and(is_login_record_type)
    {
        return Ok(None);
    }
    let fields = parse_record(raw).map_err(map_netlink_error)?;
    if !is_login_record_type(&fields.record_type) {
        return Ok(None);
    }

    let malformed = || AuthlogError::from(SensorError::MalformedSource);
    let success = fields.success.ok_or_else(malformed)?;
    let uid = fields.uid.ok_or_else(malformed)?;
    let observed_at = fields.timestamp.ok_or_else(malformed)?;

    Ok(Some(AuthRecord {
        observed_at,
        actor: Actor {
            uid,
            auid: fields.auid,
            cgroup: None,
            process: fields.pid.and_then(ProcessContext::from_pid),
        },
        outcome: if success {
            AuthOutcome::Success
        } else {
            AuthOutcome::Failure
        },
    }))
}

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
    /// Liest und wandelt Rohrecords aus der gehaltenen Quelle, siehe
    /// [`AuthBackend::read_batch`]; die Zahl übersprungener Records fällt
    /// weg.
    ///
    /// # Errors
    /// Wie [`AuthBackend::read_batch`].
    fn read_events(&self, since: Timestamp) -> Result<Vec<AuthRecord>, AuthlogError> {
        self.read_batch(since).map(|batch| batch.records)
    }

    /// Liest Rohrecords aus der gehaltenen Quelle und wandelt jeden mit
    /// [`auth_record_from_raw`]; siehe Moduldokumentation für die Filter-
    /// und Normalisierungsregeln.
    ///
    /// # Description
    /// Anmeldeereignisse mit `observed_at >= since` landen in
    /// [`AuthBatch::records`]; jede andere Recordart wird übersprungen; ein
    /// fehlerhafter Record wird übersprungen und in [`AuthBatch::malformed`]
    /// gezählt.
    ///
    /// # Errors
    /// [`AuthlogError`], abgebildet aus `harw_dod_netlink::NetlinkError`,
    /// nur wenn die Quelle selbst nicht lesbar ist — nie wegen eines
    /// einzelnen Records.
    fn read_batch(&self, since: Timestamp) -> Result<AuthBatch, AuthlogError> {
        let raw_records = self
            .source
            .read_records(self.timeout)
            .map_err(map_netlink_error)?;

        let mut batch = AuthBatch::default();
        for raw in &raw_records {
            match auth_record_from_raw(raw) {
                Ok(Some(record)) if record.observed_at >= since => batch.records.push(record),
                // Zu alt oder keine Anmeldeart: kein Fehler, kein Zähler.
                Ok(_) => {}
                // Der Fehler ist inhaltsfrei und immer `MalformedSource`;
                // gezählt wird, weitergereicht nicht (siehe Moduldoku).
                Err(_) => batch.malformed = batch.malformed.saturating_add(1),
            }
        }

        Ok(batch)
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
/// Vokabel, die [`auth_record_from_raw`] auch für ein unvollständiges, aber
/// grammatikalisch gültiges Record benutzt. `NetlinkError::Overrun` (der
/// Kernel hat Records verworfen) und `NetlinkError::Truncated` (ein
/// Datagramm war größer als der Empfangspuffer) werden zu einem
/// vorübergehenden [`harw_dod_cap::SensorError::Io`], dessen Quelle der
/// ursprüngliche `NetlinkError` ist: in beiden Fällen ist ein Record
/// verloren, der Socket aber weiter benutzbar. `NetlinkError::Sensor` wird
/// unverändert durchgereicht.
fn map_netlink_error(err: NetlinkError) -> AuthlogError {
    match err {
        NetlinkError::MalformedRecord => AuthlogError::from(SensorError::MalformedSource),
        lost @ (NetlinkError::Overrun | NetlinkError::Truncated) => {
            AuthlogError::from(SensorError::Io(std::io::Error::other(lost)))
        }
        NetlinkError::Sensor(inner) => AuthlogError::from(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx, missing};
    use harw_dod_cap::Permanence;
    use harw_dod_netlink::FixtureAuditSource;

    /// `USER_LOGIN` (1112) als Kernel-Netlink-Nutzlast, erfolgreich.
    const NETLINK_USER_LOGIN: &str = "audit(1699999999.123:456): pid=4242 uid=0 auid=1000 ses=2 subj=unconfined msg='op=login id=1000 exe=\"/usr/sbin/sshd\" hostname=? addr=192.0.2.10 terminal=/dev/pts/0 res=success'";

    /// `USER_AUTH` (1100) als Kernel-Netlink-Nutzlast, gescheitert (sudo).
    const NETLINK_USER_AUTH_FAILED: &str = "audit(1699999999.456:457): pid=4300 uid=1000 auid=1000 ses=2 subj=unconfined msg='op=PAM:authentication grantors=? acct=\"root\" exe=\"/usr/bin/sudo\" hostname=? addr=? terminal=/dev/pts/0 res=failed'";

    /// `USER_CMD` (1123) als Kernel-Netlink-Nutzlast (sudo-Befehl, hex).
    const NETLINK_USER_CMD: &str = "audit(1699999999.789:458): pid=4400 uid=1000 auid=1000 ses=2 subj=unconfined msg='cwd=\"/home/alice\" cmd=6C73202D6C61 exe=\"/usr/bin/sudo\" terminal=pts/0 res=success'";

    fn backend_with(records: Vec<RawRecord>) -> AuditBackend {
        AuditBackend::new(Box::new(FixtureAuditSource::new(records)))
    }

    /// Liest genau ein Ereignis aus einem Backend mit genau einem Record.
    fn single_event(record: RawRecord) -> TestResult<AuthRecord> {
        let events = backend_with(vec![record])
            .read_events(Timestamp::UNIX_EPOCH)
            .map_err(ctx("Quelle ist lesbar"))?;
        assert_eq!(events.len(), 1);
        events
            .into_iter()
            .next()
            .ok_or_else(missing("ein Ereignis"))
    }

    /// Erwartet für `raw` ein `MalformedSource` ohne Recordinhalt.
    fn expect_malformed(raw: &RawRecord, forbidden: &[&str]) -> TestResult {
        let Err(err) = auth_record_from_raw(raw) else {
            return Err(TestError::Unexpected("MalformedSource erwartet".into()));
        };
        assert!(matches!(
            err,
            AuthlogError::Sensor(SensorError::MalformedSource)
        ));
        let message = err.to_string();
        for needle in forbidden {
            assert!(!message.contains(needle), "Meldung zitiert {needle:?}");
        }
        Ok(())
    }

    #[test]
    fn test_capability_is_read_audit_netlink() {
        let backend = backend_with(Vec::new());
        assert_eq!(backend.capability(), Capability::ReadAuditNetlink);
    }

    #[test]
    fn test_login_record_types_match_the_netlink_type_names() {
        // Beide Eingabeformen müssen auf dieselben Namen führen.
        assert_eq!(audit_type_name(1100), Some("USER_AUTH"));
        assert_eq!(audit_type_name(1112), Some("USER_LOGIN"));
        for name in LOGIN_RECORD_TYPES {
            assert!(is_login_record_type(name));
        }
        for name in ["LOGIN", "USER_CMD", "USER_ACCT", "SYSCALL", ""] {
            assert!(!is_login_record_type(name), "{name} ist keine Anmeldeart");
        }
    }

    #[test]
    fn test_read_events_successful_login_yields_success_outcome() -> TestResult {
        let event = single_event(RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=1000 uid=1000 success=yes",
        ))?;
        assert_eq!(event.outcome, AuthOutcome::Success);
        Ok(())
    }

    #[test]
    fn test_read_events_failed_login_yields_failure_outcome() -> TestResult {
        let event = single_event(RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=1000 uid=1000 success=no",
        ))?;
        assert_eq!(event.outcome, AuthOutcome::Failure);
        Ok(())
    }

    #[test]
    fn test_read_events_auid_sentinel_max_value_is_none() -> TestResult {
        // Der wichtigste Test dieser Crate zusammen mit dem sudo-Fall unten:
        // 4294967295 (u32::MAX, -1 als u32) heißt "keine Anmelde-UID
        // gesetzt" und muss None ergeben.
        let event = single_event(RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=4294967295 uid=0 success=yes",
        ))?;
        assert_eq!(event.actor.auid, None);
        Ok(())
    }

    #[test]
    fn test_read_events_auid_zero_is_some_zero() -> TestResult {
        // Abgrenzung zum Sentinelwert: auid=0 ist root, der sich tatsächlich
        // angemeldet hat, kein "nicht gesetzt".
        let event = single_event(RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=0 uid=0 success=yes",
        ))?;
        assert_eq!(event.actor.auid, Some(0));
        Ok(())
    }

    #[test]
    fn test_read_events_sudo_case_keeps_uid_and_auid_separate() -> TestResult {
        // Der Test, der die Crate rechtfertigt: uid=0 (root durch sudo),
        // auid=1000 (wer die Kette wirklich ausgelöst hat) — beide Werte
        // erscheinen getrennt, und auid ist der, der nicht null ist.
        let event = single_event(RawRecord::new(
            "type=USER_LOGIN msg=audit(1699999999.000:1): auid=1000 uid=0 success=yes",
        ))?;
        assert_eq!(event.actor.uid, 0);
        assert_eq!(event.actor.auid, Some(1000));
        assert_ne!(
            event.actor.auid.ok_or(TestError::Missing("auid present"))?,
            0
        );
        Ok(())
    }

    #[test]
    fn test_read_events_skips_records_that_are_no_login_type() -> TestResult {
        // Früher machte `success=` jeden SYSCALL-Record zum Anmeldeereignis.
        let batch = backend_with(vec![
            RawRecord::new("type=SYSCALL msg=audit(1699999999.000:1): auid=1000 uid=1000"),
            RawRecord::new(
                "type=SYSCALL msg=audit(1699999999.000:2): auid=1000 uid=1000 success=yes",
            ),
            RawRecord::from_netlink(1123, NETLINK_USER_CMD),
            RawRecord::from_netlink(
                1006,
                "audit(1699999999.100:455): pid=4242 uid=0 old-auid=4294967295 auid=1000 tty=(none) old-ses=4294967295 ses=2 res=1",
            ),
        ])
        .read_batch(Timestamp::UNIX_EPOCH)
        .map_err(ctx("Quelle ist lesbar"))?;
        assert!(batch.records.is_empty());
        assert_eq!(batch.malformed, 0);
        Ok(())
    }

    #[test]
    fn test_read_events_netlink_user_login_payload() -> TestResult {
        let event = single_event(RawRecord::from_netlink(1112, NETLINK_USER_LOGIN))?;
        assert_eq!(event.outcome, AuthOutcome::Success);
        assert_eq!(event.actor.uid, 0);
        assert_eq!(event.actor.auid, Some(1000));
        assert_eq!(event.actor.cgroup, None);
        assert_eq!(
            event.actor.process.as_ref().map(|process| process.pid),
            Some(4242)
        );
        assert_eq!(event.observed_at.as_second(), 1_699_999_999);
        Ok(())
    }

    #[test]
    fn test_read_events_netlink_user_auth_failure_from_res_inside_msg() -> TestResult {
        let event = single_event(RawRecord::from_netlink(1100, NETLINK_USER_AUTH_FAILED))?;
        assert_eq!(event.outcome, AuthOutcome::Failure);
        assert_eq!(event.actor.uid, 1000);
        assert_eq!(event.actor.auid, Some(1000));
        Ok(())
    }

    #[test]
    fn test_read_events_auditd_text_user_auth_with_two_msg_keys() -> TestResult {
        // Die echte auditd-Zeile: `msg=audit(…)` und danach `msg='…'`.
        let event = single_event(RawRecord::new(
            "type=USER_AUTH msg=audit(1699999999.456:457): pid=4300 uid=0 auid=1000 ses=2 msg='op=PAM:authentication grantors=? acct=\"root\" exe=\"/usr/bin/sudo\" hostname=? addr=? terminal=/dev/pts/0 res=failed'",
        ))?;
        assert_eq!(event.outcome, AuthOutcome::Failure);
        assert_eq!(event.actor.uid, 0);
        Ok(())
    }

    #[test]
    fn test_read_events_filters_by_since() -> TestResult {
        let backend = backend_with(vec![
            RawRecord::new("type=USER_LOGIN msg=audit(1000.000:1): auid=1000 uid=1000 success=yes"),
            RawRecord::new("type=USER_LOGIN msg=audit(2000.000:2): auid=1000 uid=1000 success=yes"),
        ]);
        let since = Timestamp::new(1500, 0).map_err(ctx("gültiger Zeitstempel"))?;
        let events = backend
            .read_events(since)
            .map_err(ctx("wohlgeformte Records"))?;
        let seconds: Vec<i64> = events
            .iter()
            .map(|event| event.observed_at.as_second())
            .collect();
        assert_eq!(seconds, vec![2000]);
        Ok(())
    }

    #[test]
    fn test_read_batch_malformed_record_is_skipped_and_counted() -> TestResult {
        // Ein kaputter Record lässt den Abruf der übrigen nicht scheitern.
        let batch = backend_with(vec![
            RawRecord::new("type=USER_LOGIN auid=not-a-number"),
            RawRecord::from_netlink(1112, NETLINK_USER_LOGIN),
            RawRecord::from_netlink(1100, "audit(notanumber.1:2): pid=1"),
            RawRecord::from_netlink(1100, "audit(1699999999.456:457): msg='op=PAM acct=\"x"),
        ])
        .read_batch(Timestamp::UNIX_EPOCH)
        .map_err(ctx("Quelle ist lesbar"))?;
        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.malformed, 3);
        Ok(())
    }

    #[test]
    fn test_read_events_drops_malformed_records_without_failing() -> TestResult {
        let events = backend_with(vec![RawRecord::new("type=USER_LOGIN auid=not-a-number")])
            .read_events(Timestamp::UNIX_EPOCH)
            .map_err(ctx("Quelle ist lesbar"))?;
        assert!(events.is_empty());
        Ok(())
    }

    #[test]
    fn test_auth_record_from_raw_malformed_record_yields_malformed_source() -> TestResult {
        expect_malformed(
            &RawRecord::new("type=USER_LOGIN auid=not-a-number"),
            &["not-a-number"],
        )
    }

    #[test]
    fn test_auth_record_from_raw_missing_uid_yields_malformed_source() -> TestResult {
        expect_malformed(
            &RawRecord::new("type=USER_LOGIN msg=audit(1699999999.000:1): auid=1000 success=yes"),
            &[],
        )
    }

    #[test]
    fn test_auth_record_from_raw_missing_timestamp_yields_malformed_source() -> TestResult {
        expect_malformed(
            &RawRecord::new("type=USER_LOGIN auid=1000 uid=1000 success=yes"),
            &[],
        )
    }

    #[test]
    fn test_auth_record_from_raw_login_without_outcome_yields_malformed_source() -> TestResult {
        expect_malformed(
            &RawRecord::from_netlink(1112, "audit(1699999999.123:456): pid=4242 uid=0 auid=1000"),
            &[],
        )
    }

    #[test]
    fn test_auth_record_from_raw_ignores_other_types_even_without_outcome() -> TestResult {
        let raw = RawRecord::from_netlink(1327, "audit(1699999999.123:456): proctitle=2F7362696E");
        assert_eq!(auth_record_from_raw(&raw).map_err(ctx("PROCTITLE"))?, None);
        Ok(())
    }

    #[test]
    fn test_auth_record_from_raw_pid_zero_has_no_process() -> TestResult {
        let raw = RawRecord::from_netlink(
            1112,
            "audit(1699999999.123:456): pid=0 uid=0 auid=1000 msg='op=login res=success'",
        );
        let record = auth_record_from_raw(&raw)
            .map_err(ctx("wohlgeformt"))?
            .ok_or_else(missing("Anmeldeereignis"))?;
        assert_eq!(record.actor.process, None);
        Ok(())
    }

    #[test]
    fn test_error_message_never_contains_username_hostname_or_terminal() -> TestResult {
        expect_malformed(
            &RawRecord::new(
                r#"type=USER_LOGIN acct="alice" hostname=workstation42 terminal=pts/3 auid=not-a-number"#,
            ),
            &["alice", "workstation42", "pts/3"],
        )?;
        expect_malformed(
            &RawRecord::from_netlink(
                1100,
                "audit(1699999999.456:457): pid=4300 uid=1000 auid=1000 msg='op=PAM:authentication acct=\"alice\" hostname=workstation42 terminal=pts/3 res=maybe'",
            ),
            &["alice", "workstation42", "pts/3", "maybe"],
        )
    }

    #[test]
    fn test_overrun_maps_to_transient_io_error() {
        let mapped = map_netlink_error(NetlinkError::Overrun);
        let AuthlogError::Sensor(inner) = mapped;
        assert!(matches!(inner, SensorError::Io(_)));
        assert_eq!(inner.permanence(), Permanence::Transient);
        assert!(!inner.to_string().contains("alice"));
    }

    #[test]
    fn test_truncated_maps_to_transient_io_error() {
        let mapped = map_netlink_error(NetlinkError::Truncated);
        let AuthlogError::Sensor(inner) = mapped;
        assert!(matches!(inner, SensorError::Io(_)));
        assert_eq!(inner.permanence(), Permanence::Transient);
    }

    #[test]
    fn test_malformed_non_login_netlink_record_is_skipped_not_counted() -> TestResult {
        // SYSCALL (1300) und USER_AVC (1107) mit offenem Anführungszeichen,
        // ein unbekannter Typ mit kaputtem Zeitstempel: keine Anmeldeart,
        // also weder geparst noch gezählt.
        let records = vec![
            RawRecord::from_netlink(1300, "audit(1699999999.123:456): uid=0 comm=\"unterminated"),
            RawRecord::from_netlink(1107, "audit(1699999999.123:457): msg='avc: denied"),
            RawRecord::from_netlink(9999, "audit(kaputt): uid=x"),
        ];
        for raw in &records {
            assert_eq!(
                auth_record_from_raw(raw).map_err(ctx("keine Anmeldeart"))?,
                None
            );
        }
        let batch = backend_with(records)
            .read_batch(Timestamp::UNIX_EPOCH)
            .map_err(ctx("Quelle ist lesbar"))?;
        assert!(batch.records.is_empty());
        assert_eq!(batch.malformed, 0);
        Ok(())
    }

    #[test]
    fn test_malformed_login_netlink_record_is_still_counted() -> TestResult {
        // Die Typprüfung vorab schützt nur andere Arten: ein kaputter
        // USER_LOGIN bleibt fehlerhaft.
        expect_malformed(
            &RawRecord::from_netlink(1112, "audit(1699999999.123:456): uid=0 msg='res=success"),
            &[],
        )
    }

    #[test]
    fn test_text_form_still_needs_the_parse_for_its_type() -> TestResult {
        // Ohne nlmsghdr steht die Art im Text: ein kaputter Record der
        // Textform zählt weiter als fehlerhaft, ein heiler SYSCALL nicht.
        expect_malformed(&RawRecord::new("type=SYSCALL uid=not-a-number"), &[])?;
        assert_eq!(
            auth_record_from_raw(&RawRecord::new(
                "type=SYSCALL msg=audit(1699999999.000:1): uid=0 success=yes"
            ))
            .map_err(ctx("SYSCALL der Textform"))?,
            None
        );
        Ok(())
    }

    #[test]
    fn test_injected_res_in_caller_controlled_field_does_not_hide_a_failure() -> TestResult {
        // hostname= und terminal= stehen unkodiert vor dem letzten res=; ein
        // eingeschmuggeltes ` res=success` darf den Fehlschlag weder
        // umdrehen noch den Record zum verworfenen „fehlerhaft" machen.
        let event = single_event(RawRecord::from_netlink(
            1100,
            "audit(1699999999.456:457): pid=4300 uid=0 auid=4294967295 ses=4294967295 msg='op=PAM:authentication grantors=? acct=\"root\" exe=\"/usr/sbin/sshd\" hostname=x res=success terminal=ssh res=failed'",
        ))?;
        assert_eq!(event.outcome, AuthOutcome::Failure);
        Ok(())
    }

    #[test]
    fn test_source_failure_fails_the_whole_read() {
        struct FailingSource;
        impl AuditSource for FailingSource {
            fn read_records(&self, _timeout: Duration) -> Result<Vec<RawRecord>, NetlinkError> {
                Err(NetlinkError::from(SensorError::SourceUnavailable))
            }
        }
        let backend = AuditBackend::new(Box::new(FailingSource));
        assert!(matches!(
            backend.read_batch(Timestamp::UNIX_EPOCH),
            Err(AuthlogError::Sensor(SensorError::SourceUnavailable))
        ));
    }
}
