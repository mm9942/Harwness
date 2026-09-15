//! Formung: rohes fanotify-Ereignis → `SecurityEvent`.
//!
//! # Verantwortungsbereich
//! [`shape_event`] ist der eine Einstiegspunkt des Formungsteils: er
//! verkettet [`crate::fdpath::interpret_fd_target`],
//! [`harw_dod_cap::ReadScope::allows`], [`crate::mask::interpret_mask`] und
//! [`crate::loginuid::resolve_loginuid`] zu einem vollständigen
//! `harw_dod_signals::SecurityEvent`. Reine Funktion auf einem [`RawFsEvent`]
//! und den übergebenen Kontextwerten — kein Betriebssystemaufruf außer dem
//! privilegienlosen loginuid-Lesezugriff.
//!
//! # Warum ein außerhalb des Bereichs liegender Pfad kein Fehler ist
//! Ein `fanotify`-Wächter beobachtet typischerweise mehr, als am Ende
//! gemeldet werden soll (z. B. eine ganze Partition, während nur ein
//! Unterverzeichnis sicherheitsrelevant ist). Ein Ereignis außerhalb des
//! konfigurierten `ReadScope` ist deshalb ein normaler, erwarteter Fall —
//! kein Fehlschlag der Formung. [`shape_event`] liefert dafür `Ok(None)`,
//! nicht `Err(..)`.
//!
//! # Exportierte Elemente
//! [`shape_event`].
//!
//! # Nebenläufigkeit
//! Zustandslos; von jedem Thread parallel aufrufbar. Der einzige I/O-Zugriff
//! (loginuid) öffnet und schließt seine eigene Datei je Aufruf.
//!
//! # Fehler
//! [`crate::error::FsMonError::MalformedSource`], wenn die Ereignismaske
//! keine unterstützte Zugriffsart trägt (siehe
//! [`crate::mask::interpret_mask`]).
//!
//! # Examples
//! ```rust
//! use std::path::Path;
//! use harw_dod_cap::ReadScope;
//! use harw_dod_fsmon::raw::RawFsEvent;
//! use harw_dod_fsmon::shape::shape_event;
//! use harw_types::SensorId;
//!
//! let raw = RawFsEvent {
//!     mask: 0x08, // FAN_CLOSE_WRITE
//!     pid: 1,
//!     uid: 0,
//!     fd_target: "/srv/data/report.csv".to_owned(),
//! };
//! let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
//! let sensor = SensorId::from_str("fsmon-0");
//!
//! let event = shape_event(&raw, &scope, Path::new("/proc"), &sensor, jiff::Timestamp::UNIX_EPOCH)
//!     .expect("Schreibmaske muss formbar sein")
//!     .expect("Pfad liegt im Bereich");
//! assert!(matches!(event.kind, harw_dod_signals::EventKind::FileWrite { .. }));
//! ```

use std::path::Path;

use harw_dod_cap::ReadScope;
use harw_dod_signals::{Actor, SecurityEvent};
use harw_types::SensorId;
use jiff::Timestamp;

use crate::error::FsMonError;
use crate::fdpath::interpret_fd_target;
use crate::loginuid::resolve_loginuid;
use crate::mask::interpret_mask;
use crate::raw::RawFsEvent;

/// Formt ein rohes fanotify-Ereignis zu einem [`SecurityEvent`], sofern der
/// aufgelöste Pfad im überwachten Bereich liegt.
///
/// # Description
/// Ablauf: (1) das Dateideskriptor-Ziel deuten, (2) gegen `watch_scope`
/// prüfen — liegt der Pfad außerhalb, wird das Ereignis verworfen
/// (`Ok(None)`), (3) die Maske deuten, (4) die loginuid des Auslösers
/// anreichern. Kein Schritt liest oder überträgt Dateiinhalt — nur Pfade und
/// Zahlen.
///
/// # Arguments
/// - `raw` (`&RawFsEvent`): das rohe Ereignis.
/// - `watch_scope` (`&harw_dod_cap::ReadScope`): der Bereich, gegen den der
///   aufgelöste Pfad geprüft wird. Eine reine, syntaktische Prüfung
///   ([`harw_dod_cap::ReadScope::allows`]) — der von `/proc/self/fd/<n>`
///   gelieferte Pfad ist bereits vom Kernel aufgelöst und enthält keine noch
///   zu prüfenden Symlinks.
/// - `proc_root` (`&std::path::Path`): die Wurzel für die loginuid-Auflösung
///   (siehe [`crate::loginuid::resolve_loginuid`]).
/// - `sensor` (`&harw_types::SensorId`): die Kennung des Sensors, der in das
///   erzeugte `SecurityEvent` übernommen wird.
/// - `now` (`jiff::Timestamp`): injizierte Beobachtungszeit — diese Funktion
///   liest nie die Systemuhr selbst.
///
/// # Returns
/// `Ok(Some(event))`, wenn der Pfad im Bereich liegt und die Maske
/// interpretierbar ist; `Ok(None)`, wenn der Pfad außerhalb des Bereichs
/// liegt.
///
/// # Errors
/// [`FsMonError::MalformedSource`], wenn die Ereignismaske keine
/// unterstützte Zugriffsart trägt.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust
/// use std::path::Path;
/// use harw_dod_cap::ReadScope;
/// use harw_dod_fsmon::raw::RawFsEvent;
/// use harw_dod_fsmon::shape::shape_event;
/// use harw_types::SensorId;
///
/// let raw = RawFsEvent {
///     mask: 0x02,
///     pid: 7,
///     uid: 0,
///     fd_target: "/etc/passwd".to_owned(),
/// };
/// let outside_scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
/// let sensor = SensorId::from_str("fsmon-0");
///
/// let dropped = shape_event(
///     &raw,
///     &outside_scope,
///     Path::new("/proc"),
///     &sensor,
///     jiff::Timestamp::UNIX_EPOCH,
/// )
/// .expect("Formung selbst schlägt hier nicht fehl");
/// assert!(dropped.is_none());
/// ```
pub fn shape_event(
    raw: &RawFsEvent,
    watch_scope: &ReadScope,
    proc_root: &Path,
    sensor: &SensorId,
    now: Timestamp,
) -> Result<Option<SecurityEvent>, FsMonError> {
    let path = interpret_fd_target(&raw.fd_target);

    if !watch_scope.allows(Path::new(&path)) {
        return Ok(None);
    }

    let kind = interpret_mask(raw.mask, path)?;
    let auid = resolve_loginuid(proc_root, raw.pid);
    let actor = Actor {
        uid: raw.uid,
        auid,
        cgroup: None,
    };

    Ok(Some(SecurityEvent {
        sensor: sensor.clone(),
        observed_at: now,
        actor: Some(actor),
        kind,
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use harw_dod_cap::ReadScope;
    use harw_dod_signals::EventKind;
    use harw_types::SensorId;
    use jiff::Timestamp;

    use super::shape_event;
    use crate::error::FsMonError;
    use crate::raw::RawFsEvent;

    fn sensor() -> SensorId {
        SensorId::from_str("fsmon-0")
    }

    fn write_event(fd_target: &str, mask: u64) -> RawFsEvent {
        RawFsEvent {
            mask,
            pid: 4242,
            uid: 1000,
            fd_target: fd_target.to_owned(),
        }
    }

    #[test]
    fn test_shape_event_write_mask_produces_file_write_event() {
        let proc_root = tempfile::tempdir().expect("proc-tempdir");
        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let raw = write_event("/srv/data/report.csv", 0x08 /* FAN_CLOSE_WRITE */);

        let event = shape_event(&raw, &scope, proc_root.path(), &sensor(), Timestamp::UNIX_EPOCH)
            .expect("Schreibmaske muss formbar sein")
            .expect("Pfad liegt im Bereich");

        match event.kind {
            EventKind::FileWrite { path } => assert_eq!(path, "/srv/data/report.csv"),
            other => panic!("erwartet FileWrite, erhalten {other:?}"),
        }
        assert_eq!(event.sensor, sensor());
    }

    #[test]
    fn test_shape_event_path_outside_scope_is_dropped() {
        let proc_root = tempfile::tempdir().expect("proc-tempdir");
        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let raw = write_event("/etc/passwd", 0x08);

        let result = shape_event(&raw, &scope, proc_root.path(), &sensor(), Timestamp::UNIX_EPOCH)
            .expect("Formung selbst schlägt hier nicht fehl");

        assert!(result.is_none());
    }

    #[test]
    fn test_shape_event_unrecognized_mask_is_malformed_source_error() {
        let proc_root = tempfile::tempdir().expect("proc-tempdir");
        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let raw = write_event("/srv/data/report.csv", 0x01 /* FAN_ACCESS */);

        let err = shape_event(&raw, &scope, proc_root.path(), &sensor(), Timestamp::UNIX_EPOCH)
            .expect_err("unerwartete Ereignisform muss scheitern");

        assert!(matches!(err, FsMonError::MalformedSource));
    }

    #[test]
    fn test_shape_event_enriches_actor_with_resolved_loginuid() {
        let proc_root = tempfile::tempdir().expect("proc-tempdir");
        let pid_dir = proc_root.path().join("4242");
        fs::create_dir_all(&pid_dir).expect("pid dir");
        fs::write(pid_dir.join("loginuid"), "1000\n").expect("loginuid schreiben");

        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let raw = write_event("/srv/data/report.csv", 0x08);

        let event = shape_event(&raw, &scope, proc_root.path(), &sensor(), Timestamp::UNIX_EPOCH)
            .expect("formbar")
            .expect("im Bereich");

        let actor = event.actor.expect("Actor muss gesetzt sein");
        assert_eq!(actor.uid, 1000);
        assert_eq!(actor.auid, Some(1000));
    }

    #[test]
    fn test_shape_event_unset_loginuid_yields_none_auid_not_sentinel() {
        let proc_root = tempfile::tempdir().expect("proc-tempdir");
        let pid_dir = proc_root.path().join("4242");
        fs::create_dir_all(&pid_dir).expect("pid dir");
        fs::write(pid_dir.join("loginuid"), "4294967295\n").expect("loginuid schreiben");

        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let raw = write_event("/srv/data/report.csv", 0x08);

        let event = shape_event(&raw, &scope, proc_root.path(), &sensor(), Timestamp::UNIX_EPOCH)
            .expect("formbar")
            .expect("im Bereich");

        assert_eq!(event.actor.expect("Actor gesetzt").auid, None);
    }

    /// Zusage der Crate: kein emittiertes Feld enthält Dateiinhalt.
    #[test]
    fn test_shape_event_never_carries_file_content() {
        let proc_root = tempfile::tempdir().expect("proc-tempdir");
        let watched = tempfile::tempdir().expect("watched-tempdir");
        let target = watched.path().join("report.csv");
        fs::write(&target, "TOP-SECRET-CONTENT").expect("Zieldatei mit Inhalt schreiben");

        let scope = ReadScope::from_roots([watched.path().to_path_buf()]);
        let raw = write_event(target.to_str().expect("utf8 Pfad"), 0x08);

        let event = shape_event(&raw, &scope, proc_root.path(), &sensor(), Timestamp::UNIX_EPOCH)
            .expect("formbar")
            .expect("im Bereich");

        let json = serde_json::to_string(&event).expect("SecurityEvent serialisiert");
        assert!(
            !json.contains("TOP-SECRET-CONTENT"),
            "Ereignis darf keinen Dateiinhalt tragen: {json}"
        );
    }
}
