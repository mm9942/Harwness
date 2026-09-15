//! systemd-Socket-Aktivierung: `LISTEN_FDS`/`LISTEN_PID` lesen, genau einen
//! Deskriptor herausgeben — hart, ohne `unsafe`.
//!
//! # Warum diese zwei Variablen von Hand gelesen werden
//! Der Brief für diesen Knoten verlangt eine begründete Entscheidung,
//! bevor eine Crate für das Lesen der Aktivierungsumgebung aufgenommen
//! wird: „`LISTEN_FDS` selbst zu lesen sind wenige Zeilen (zwei
//! Umgebungsvariablen, ein fester Start-Deskriptor 3)". Das stimmt für das
//! **Parsen** — [`verify_listen_pid`] und [`verify_listen_fds_count`] sind
//! reine, kein `unsafe` benötigende Funktionen über zwei
//! `std::env::var`-Aufrufe. Beide sind hier deshalb von Hand geschrieben,
//! nicht über eine Crate.
//!
//! # Warum die eigentliche Deskriptor-Umwandlung trotzdem eine Crate ist
//! Was sich **nicht** ohne `unsafe` lösen lässt, ist der letzte Schritt:
//! aus der Ganzzahl `3` (dem von systemd garantierten ersten
//! Aktivierungs-Deskriptor) einen `std::os::fd::OwnedFd` zu machen. Rust
//! kennt dafür nur `OwnedFd::from_raw_fd`/`BorrowedFd::borrow_raw` — beide
//! `unsafe fn`, weil der Compiler dem Aufrufer glauben muss, dass die
//! Ganzzahl tatsächlich einen gültigen, exklusiv gehörenden Deskriptor
//! bezeichnet. Es gibt keinen Weg, diese eine Umwandlung in Rust
//! auszudrücken, ohne irgendwo ein `unsafe`-Schlüsselwort zu schreiben —
//! `#![forbid(unsafe_code)]` (diese Crate) und `unsafe_code = "forbid"`
//! (Workspace-Lint) verbieten das in dieser Crate vollständig.
//!
//! Die Crate `sd-listen-fds` (Version 0.2.0) löst genau dieses eine
//! Problem und **keines** darüber hinaus. Ihr vollständiger Quelltext
//! (nachgelesen über `docs.rs/crate/sd-listen-fds/0.2.0/source/src/lib.rs`,
//! `cargo add`/`cargo search` konnten hier nicht laufen) zeigt den
//! entscheidenden Ausschnitt:
//!
//! ```text
//! let fd = OwnedFd {
//!     inner: unsafe {
//!         mem::transmute::<u32, std::os::fd::OwnedFd>(SD_LISTEN_FDS_START + i)
//!     },
//! };
//! ```
//!
//! Genau ein `unsafe`-Block, in einer fremden, für dieses eine Problem
//! geschriebenen Crate — nicht in dieser. Die Crate hat laut eigener
//! Beschreibung **keine einzige** weitere Abhängigkeit (weder `libc` noch
//! sonst etwas): das kleinstmögliche Blatt für dieses eine Problem, kein
//! Mehrzweck-Socket-Aktivierungs-Framework.
//!
//! # Warum die `LISTEN_PID`-Prüfung hier ein zweites Mal steht
//! Derselbe Quelltext zeigt, dass `sd_listen_fds::get()` `LISTEN_PID`
//! bereits **selbst** gegen `std::process::id()` prüft:
//!
//! ```text
//! let Ok(pid) = env::var("LISTEN_PID") else { return Ok(Vec::new()); };
//! let pid = pid.parse::<u32>().map_err(|_| Error::MalformedEnv)?;
//! if pid != std::process::id() {
//!     return Ok(Vec::new());
//! }
//! ```
//!
//! Das ist exakt die im Auftrag benannte Falle: „ohne sie erbt ein
//! Kindprozess die Variablen und hält fremde Deskriptoren für seine
//! eigenen". `sd_listen_fds::get()` vermeidet sie bereits korrekt — ein
//! `LISTEN_PID`, das nicht zur eigenen Prozess-ID passt, liefert `Ok(vec![])`,
//! **nicht** die geerbten Deskriptoren. Trotzdem verlässt sich dieses Modul
//! nicht allein darauf, aus zwei Gründen:
//!
//! 1. **Testbarkeit ohne Umgebungsmutation.** Seit Rust Edition 2024 (dieser
//!    Workspace: `edition = "2024"`) sind `std::env::set_var`/`remove_var`
//!    `unsafe fn` — in dieser Crate verboten. Ein Test, der die reale
//!    Prozessumgebung mutiert, um „`LISTEN_PID` zeigt auf einen fremden
//!    Prozess" nachzustellen, ist deshalb hier nicht schreibbar. Die
//!    Vorprüfung in diesem Modul ([`verify_listen_pid`]) nimmt die rohen
//!    Werte stattdessen als Parameter entgegen (kein `std::env`-Zugriff in
//!    der geprüften Funktion selbst) und ist dadurch mit einem fest
//!    codierten, garantiert unpassenden Wert testbar — siehe
//!    `test_listen_pid_pointing_at_a_foreign_process_is_a_clean_error`
//!    unten, der Test, der die Falle absichert.
//! 2. **Unterscheidbare Diagnose.** `sd_listen_fds::get()` liefert für
//!    „gar kein `LISTEN_PID`" und „`LISTEN_PID` zeigt auf einen fremden
//!    Prozess" dasselbe `Ok(vec![])` — aus Sicht der Fremdcrate identisch.
//!    Diese Crate unterscheidet beide Fälle als eigene
//!    [`crate::error::WardenBinError`]-Varianten
//!    ([`crate::error::WardenBinError::ListenPidMissing`] vs.
//!    [`crate::error::WardenBinError::ListenPidForeign`]), damit ein
//!    Betreiber aus der Fehlermeldung ablesen kann, welcher der beiden
//!    Fälle vorlag, statt nur „kein Deskriptor erhalten".
//!
//! # Nebenläufigkeit
//! [`acquire_listen_socket`] liest Prozessumgebung und darf nur einmal, vor
//! jedem weiteren Thread, aus dem Hauptthread aufgerufen werden — dieselbe
//! Voraussetzung wie bei jeder Landlock-Selbstbeschränkung.
//!
//! # Fehler
//! Siehe [`crate::error::WardenBinError`], Varianten `ListenPid*`,
//! `ListenFds*`, `UnexpectedListenFdCount`.

use crate::error::WardenBinError;

/// Prüft den rohen `LISTEN_PID`-Wert gegen die eigene Prozess-ID.
///
/// # Description
/// Reine Funktion ohne `std::env`-Zugriff — siehe Moduldoku, Abschnitt
/// „Warum die `LISTEN_PID`-Prüfung hier ein zweites Mal steht", für die
/// Testbarkeitsbegründung.
///
/// # Arguments
/// - `raw` (`Option<&str>`): der rohe Wert von `LISTEN_PID`, oder `None`,
///   wenn die Variable nicht gesetzt ist.
/// - `current_pid` (`u32`): die eigene Prozess-ID (`std::process::id()`).
///
/// # Returns
/// `Ok(())`, wenn `raw` die eigene Prozess-ID nennt.
///
/// # Errors
/// - [`WardenBinError::ListenPidMissing`]: `raw` ist `None`.
/// - [`WardenBinError::ListenPidMalformed`]: `raw` ist kein gültiges `u32`.
/// - [`WardenBinError::ListenPidForeign`]: `raw` nennt eine andere
///   Prozess-ID.
pub(crate) fn verify_listen_pid(raw: Option<&str>, current_pid: u32) -> Result<(), WardenBinError> {
    let raw = raw.ok_or(WardenBinError::ListenPidMissing)?;
    let pid: u32 = raw.parse().map_err(|_| WardenBinError::ListenPidMalformed)?;
    if pid != current_pid {
        return Err(WardenBinError::ListenPidForeign);
    }
    Ok(())
}

/// Parst den rohen `LISTEN_FDS`-Wert als Deskriptor-Anzahl.
///
/// # Arguments
/// - `raw` (`Option<&str>`): der rohe Wert von `LISTEN_FDS`, oder `None`,
///   wenn die Variable nicht gesetzt ist.
///
/// # Returns
/// Die deklarierte Anzahl übergebener Deskriptoren.
///
/// # Errors
/// - [`WardenBinError::ListenFdsMissing`]: `raw` ist `None`.
/// - [`WardenBinError::ListenFdsMalformed`]: `raw` ist kein gültiges `u32`.
pub(crate) fn verify_listen_fds_count(raw: Option<&str>) -> Result<u32, WardenBinError> {
    let raw = raw.ok_or(WardenBinError::ListenFdsMissing)?;
    raw.parse().map_err(|_| WardenBinError::ListenFdsMalformed)
}

/// Liest die vollständige systemd-Aktivierungsumgebung und liefert den
/// einen erwarteten Deskriptor als `std::os::fd::OwnedFd`.
///
/// # Description
/// Reihenfolge: zuerst [`verify_listen_pid`], dann [`verify_listen_fds_count`]
/// (beide gegen die tatsächliche Prozessumgebung, siehe Moduldoku), dann
/// erst `sd_listen_fds::get()` für die eigentliche, `unsafe`-freie
/// Deskriptor-Umwandlung (siehe Moduldoku, Abschnitt „Warum die eigentliche
/// Deskriptor-Umwandlung trotzdem eine Crate ist"). Verlangt am Ende genau
/// einen Eintrag — dieses Binary erwartet exakt einen `SOCK_SEQPACKET`-
/// Socket, keinen benannten (`LISTEN_FDNAMES`) oder mehrfachen.
///
/// # Returns
/// Den einen übergebenen Deskriptor.
///
/// # Errors
/// Siehe [`crate::error::WardenBinError`], Varianten `ListenPid*`,
/// `ListenFds*`, [`WardenBinError::UnexpectedListenFdCount`],
/// [`WardenBinError::ListenFdsAcquisitionFailed`].
pub fn acquire_listen_socket() -> Result<std::os::fd::OwnedFd, WardenBinError> {
    let current_pid = std::process::id();
    let listen_pid = std::env::var("LISTEN_PID").ok();
    verify_listen_pid(listen_pid.as_deref(), current_pid)?;

    let listen_fds = std::env::var("LISTEN_FDS").ok();
    let declared_count = verify_listen_fds_count(listen_fds.as_deref())?;
    if declared_count != 1 {
        return Err(WardenBinError::UnexpectedListenFdCount {
            actual: declared_count as usize,
        });
    }

    let mut fds = sd_listen_fds::get()?;
    match fds.len() {
        1 => {
            // `sd_listen_fds::get()` liefert je Eintrag ein Paar
            // `(Option<String>, sd_listen_fds::OwnedFd)` -- der erste Teil ist
            // der von systemd vergebene Name (`FileDescriptorName=`), den wir
            // hier nicht brauchen.
            //
            // `sd_listen_fds::OwnedFd` ist ein eigener Typ, nicht der aus
            // `std`. `into_std()` gibt den echten `std::os::fd::OwnedFd`
            // heraus, ohne dass wir `unsafe` brauchen -- die Alternative
            // `into_raw()` plus `from_raw_fd` wäre genau das, was
            // `#![forbid(unsafe_code)]` hier verbietet.
            let (_name, fd) = fds.pop().expect("Länge unmittelbar zuvor geprüft");
            Ok(fd.into_std())
        }
        actual => Err(WardenBinError::UnexpectedListenFdCount { actual }),
    }
}

#[cfg(test)]
mod tests {
    use super::{verify_listen_fds_count, verify_listen_pid};
    use crate::error::WardenBinError;

    #[test]
    fn test_missing_listen_pid_is_a_clean_error() {
        assert!(matches!(
            verify_listen_pid(None, 42),
            Err(WardenBinError::ListenPidMissing)
        ));
    }

    #[test]
    fn test_malformed_listen_pid_is_a_clean_error() {
        assert!(matches!(
            verify_listen_pid(Some("not-a-pid"), 42),
            Err(WardenBinError::ListenPidMalformed)
        ));
    }

    #[test]
    fn test_listen_pid_pointing_at_a_foreign_process_is_a_clean_error() {
        // Der Test, der die im Auftrag benannte Falle absichert: ein
        // `LISTEN_PID`, das nicht der eigenen Prozess-ID entspricht, muss
        // als Startfehler behandelt werden — nicht stillschweigend
        // akzeptiert werden, als gehörten die Deskriptoren diesem Prozess.
        // Siehe Moduldoku, Abschnitt „Warum die `LISTEN_PID`-Prüfung hier
        // ein zweites Mal steht".
        let result = verify_listen_pid(Some("999999"), 42);
        assert!(matches!(result, Err(WardenBinError::ListenPidForeign)));
    }

    #[test]
    fn test_matching_listen_pid_is_accepted() {
        assert!(verify_listen_pid(Some("42"), 42).is_ok());
    }

    #[test]
    fn test_missing_listen_fds_is_a_clean_error() {
        assert!(matches!(
            verify_listen_fds_count(None),
            Err(WardenBinError::ListenFdsMissing)
        ));
    }

    #[test]
    fn test_malformed_listen_fds_is_a_clean_error() {
        assert!(matches!(
            verify_listen_fds_count(Some("not-a-number")),
            Err(WardenBinError::ListenFdsMalformed)
        ));
    }

    #[test]
    fn test_valid_listen_fds_count_parses() {
        assert_eq!(verify_listen_fds_count(Some("1")).unwrap(), 1);
    }

    // `acquire_listen_socket` selbst wird hier bewusst nicht getestet: es
    // liest die reale Prozessumgebung und öffnet im Erfolgsfall einen
    // echten, von systemd übergebenen Deskriptor — beides nach
    // Aufgabenstellung untersagt bzw. in einer Testumgebung ohnehin nie
    // vorhanden. Jede Entscheidung, die es trifft, ist über
    // `verify_listen_pid`/`verify_listen_fds_count` einzeln oben geprüft
    // (Muster: `harw-probe-fs::main`, dessen `run` aus demselben Grund
    // ebenfalls ungetestet bleibt).
}
