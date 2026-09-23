//! Auflösung der loginuid: der eine Wert, den `sudo` nicht verändert.
//!
//! # Warum die loginuid Sorgfalt verdient
//! Die loginuid (`/proc/<pid>/loginuid`, vom Kernel-Audit-Subsystem gesetzt)
//! ist die Anmelde-UID — im Unterschied zur laufenden Ausführungs-UID
//! (`Actor::uid`), die ein `sudo`-Aufruf auf die Ziel-UID (typischerweise
//! `0`) umschreibt. Sie ist damit die einzige Kennung, die beantwortet, wer
//! eine Kette wirklich angestoßen hat.
//!
//! # Die `4294967295`-Falle
//! Ein Prozess ohne Login-Session (z. B. ein Systemdienst, der nie über eine
//! Anmeldung gestartet wurde) trägt `/proc/<pid>/loginuid` mit dem Wert
//! `4294967295` — das ist `-1` interpretiert als `u32`
//! (`std::u32::MAX`). Dieser Wert bedeutet **„nicht gesetzt"**, nicht „UID
//! 4294967295" — eine UID in dieser Größenordnung existiert auf keinem
//! realen System. Wer diesen Sonderfall nicht behandelt, meldet eine
//! erfundene Nutzer-Identität, die wie eine gültige, aber absurd hohe UID
//! aussieht und deshalb nie aus Versehen auffällt. [`resolve_loginuid`]
//! deutet genau diesen Wert als [`None`], nie als `Some(4294967295)`.
//!
//! # Warum diese Funktion `Option<u32>` statt `Result<..., FsMonError>`
//! **liefert**
//! Die loginuid ist Anreicherung, kein hartes Erfordernis: ein bereits
//! beendeter Prozess (`/proc/<pid>/loginuid` existiert nicht mehr), eine
//! fehlende Berechtigung oder ein unerwarteter Dateiinhalt sind für den
//! Aufrufer ununterscheidbar von „nicht gesetzt" — in allen drei Fällen weiß
//! diese Funktion die loginuid schlicht nicht. Ein `Result`, das diese drei
//! Fälle als Fehler behandelt, würde den Formungspfad (`shape_event`) an
//! einer Stelle scheitern lassen, an der ein `SecurityEvent` ohne `auid`
//! immer noch ein vollständiges, sinnvolles Ereignis ist.
//!
//! # Verantwortungsbereich
//! Liest `/proc/<pid>/loginuid` ausschließlich über
//! [`harw_dod_readfs::read_first_line`], nie über `std::fs` direkt (siehe
//! Vertragsvorgabe in der Crate-Dokumentation).
//!
//! # Exportierte Elemente
//! [`resolve_loginuid`], [`LOGINUID_UNSET`].
//!
//! # Nebenläufigkeit
//! Zustandslos; jeder Aufruf öffnet und schließt seine eigene Datei über
//! [`harw_dod_cap::ReadScope::open`]. Von jedem Thread parallel aufrufbar.
//!
//! # Fehler
//! Keine — siehe Begründung oben.
//!
//! # Examples
//! ```rust,no_run
//! use std::path::Path;
//! use harw_dod_fsmon::loginuid::resolve_loginuid;
//!
//! let auid = resolve_loginuid(Path::new("/proc"), 1234);
//! assert!(auid.is_none() || auid.is_some());
//! ```

use std::path::Path;

use harw_dod_cap::ReadScope;

/// Der Wert, den der Kernel in `/proc/<pid>/loginuid` einträgt, wenn keine
/// Anmelde-UID gesetzt wurde: `-1` interpretiert als `u32`. Siehe Moduldoku,
/// Abschnitt „Die `4294967295`-Falle".
pub const LOGINUID_UNSET: u32 = u32::MAX;

/// Löst die loginuid (Anmelde-UID) eines Prozesses auf.
///
/// # Description
/// Liest `<proc_root>/<pid>/loginuid` als erste Zeile über
/// [`harw_dod_readfs::read_first_line`] und parst sie als `u32`. Ergibt der
/// gelesene Wert [`LOGINUID_UNSET`] (`4294967295`), liefert diese Funktion
/// [`None`] — nie `Some(4294967295)` (siehe Moduldoku). Jeder andere
/// Fehlschlag (Datei fehlt, weil der Prozess bereits beendet ist, fehlende
/// Berechtigung, nicht parsbarer Inhalt) führt ebenfalls zu [`None`], nie zu
/// einem `Result::Err` — siehe Moduldoku für die Begründung.
///
/// # Arguments
/// - `proc_root` (`&std::path::Path`): die Wurzel, unter der Prozess-
///   Verzeichnisse liegen (in Produktion `/proc`; Tests übergeben ein
///   `tempfile`-Verzeichnis mit derselben Struktur). Der Lesebereich für
///   [`harw_dod_readfs::read_first_line`] wird intern exakt auf diese Wurzel
///   beschränkt.
/// - `pid` (`u32`): die Prozess-ID, deren loginuid aufgelöst werden soll.
///
/// # Returns
/// `Some(uid)` mit der aufgelösten Anmelde-UID, oder [`None`], wenn sie
/// nicht gesetzt oder nicht ermittelbar ist.
///
/// # Errors
/// Keine — diese Funktion gibt nie ein `Result::Err` zurück, siehe Moduldoku
/// für die Begründung. Jeder Fehlschlag (Datei fehlt, keine Berechtigung,
/// nicht parsbarer Inhalt) erscheint als [`None`].
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust
/// use std::fs;
/// use harw_dod_fsmon::loginuid::resolve_loginuid;
///
/// let root = tempfile::tempdir().expect("tempdir");
/// let pid_dir = root.path().join("4321");
/// fs::create_dir_all(&pid_dir).expect("pid dir");
/// fs::write(pid_dir.join("loginuid"), "1000\n").expect("write loginuid");
///
/// assert_eq!(resolve_loginuid(root.path(), 4321), Some(1000));
/// ```
#[must_use]
pub fn resolve_loginuid(proc_root: &Path, pid: u32) -> Option<u32> {
    let scope = ReadScope::from_roots([proc_root.to_path_buf()]);
    let path = proc_root.join(pid.to_string()).join("loginuid");

    let raw = harw_dod_readfs::read_first_line(&scope, &path).ok()?;
    let value: u32 = raw.trim().parse().ok()?;

    if value == LOGINUID_UNSET {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{LOGINUID_UNSET, resolve_loginuid};
    use crate::test_support::{TestResult, ctx};

    fn write_loginuid(root: &std::path::Path, pid: u32, content: &str) -> TestResult {
        let pid_dir = root.join(pid.to_string());
        fs::create_dir_all(&pid_dir).map_err(ctx("pid dir anlegen"))?;
        fs::write(pid_dir.join("loginuid"), content).map_err(ctx("loginuid schreiben"))?;
        Ok(())
    }

    /// Der wichtigste Test dieser Crate: `4294967295` ist "nicht gesetzt",
    /// keine gültige UID.
    #[test]
    fn test_resolve_loginuid_unset_sentinel_is_none() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_loginuid(root.path(), 100, "4294967295\n")?;

        assert_eq!(resolve_loginuid(root.path(), 100), None);
        assert_eq!(LOGINUID_UNSET, 4_294_967_295);
        Ok(())
    }

    /// Abgrenzung zum vorigen Fall: `0` ist eine gültige, gesetzte UID (root),
    /// nicht der Sonderfall "nicht gesetzt".
    #[test]
    fn test_resolve_loginuid_zero_is_some_zero() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_loginuid(root.path(), 200, "0\n")?;

        assert_eq!(resolve_loginuid(root.path(), 200), Some(0));
        Ok(())
    }

    #[test]
    fn test_resolve_loginuid_ordinary_value_is_some() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_loginuid(root.path(), 300, "1000\n")?;

        assert_eq!(resolve_loginuid(root.path(), 300), Some(1000));
        Ok(())
    }

    /// Ein bereits beendeter Prozess hinterlässt kein `/proc/<pid>/loginuid`
    /// mehr — das ist `None`, kein Fehler.
    #[test]
    fn test_resolve_loginuid_missing_file_is_none_not_error() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        // Kein Verzeichnis für die PID angelegt.
        assert_eq!(resolve_loginuid(root.path(), 999), None);
        Ok(())
    }

    #[test]
    fn test_resolve_loginuid_malformed_content_is_none() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write_loginuid(root.path(), 400, "nicht-numerisch\n")?;

        assert_eq!(resolve_loginuid(root.path(), 400), None);
        Ok(())
    }
}
