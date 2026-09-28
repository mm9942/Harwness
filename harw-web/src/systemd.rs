//! systemd-Socket-Aktivierung für die Kontrollfläche (`harw web
//! --systemd-socket`, Crypto-Masterplan v2 §6.1/§23, H9).
//!
//! # Vertrag
//! Dasselbe Muster wie `harw-netsec/src/systemd.rs` und
//! `harw-auth-hub/src/systemd.rs`: Die Aktivierungsumgebung wird zuerst
//! explizit geprüft — `LISTEN_PID` muss diesen Prozess nennen und
//! `LISTEN_FDS` genau `1` sein — und erst dann macht `sd_listen_fds::get()`
//! aus Deskriptor 3 einen besessenen Deskriptor, sodass diese Crate kein
//! `unsafe` braucht. `sd_listen_fds::get()` allein lieferte bei fehlendem oder
//! fremdem `LISTEN_PID` still eine leere Liste; hier ist das ein Startfehler,
//! denn ein mit `--systemd-socket` gestarteter Dienst, der still nichts
//! bindet, sähe von außen genauso aus wie ein gesunder.
//!
//! Aktivierung wird nur genutzt, wenn sie auf der Kommandozeile verlangt
//! wurde. Es gibt **keinen** automatischen Rückfall zwischen Aktivierung und
//! dem Binden eines Pfads.
//!
//! ```ini
//! # harw-control.socket
//! [Socket]
//! ListenStream=/run/harw/infra/control.sock
//! SocketUser=harw-control
//! SocketGroup=harw-control-clients
//! SocketMode=0660
//! Service=harw-control.service
//! RemoveOnStop=yes
//! Accept=no
//! ```

use std::os::unix::net::UnixListener;

use crate::error::WebError;

/// Prüft die rohe Aktivierungsumgebung.
///
/// # Arguments
/// - `listen_pid` (`Option<&str>`): Wert von `LISTEN_PID`.
/// - `listen_fds` (`Option<&str>`): Wert von `LISTEN_FDS`.
/// - `own_pid` (`u32`): die eigene Prozess-ID.
///
/// # Errors
/// [`WebError::Activation`], wenn `LISTEN_PID` fehlt, fehlerhaft ist oder
/// einen anderen Prozess nennt, oder `LISTEN_FDS` fehlt, fehlerhaft ist oder
/// nicht `1` beträgt.
pub fn check_activation_env(
    listen_pid: Option<&str>,
    listen_fds: Option<&str>,
    own_pid: u32,
) -> Result<(), WebError> {
    let fail = |reason: &str| {
        Err(WebError::Activation {
            reason: reason.to_owned(),
        })
    };
    let Some(pid) = listen_pid else {
        return fail("LISTEN_PID is not set");
    };
    let Ok(pid) = pid.trim().parse::<u32>() else {
        return fail("LISTEN_PID is malformed");
    };
    if pid != own_pid {
        return fail("LISTEN_PID names another process");
    }
    let Some(fds) = listen_fds else {
        return fail("LISTEN_FDS is not set");
    };
    let Ok(fds) = fds.trim().parse::<u32>() else {
        return fail("LISTEN_FDS is malformed");
    };
    if fds != 1 {
        return fail("exactly one socket must be passed (LISTEN_FDS=1)");
    }
    Ok(())
}

/// Übernimmt den einen von systemd übergebenen Socket.
///
/// # Description
/// Liefert einen nicht-blockierenden `std`-Listener; der Aufrufer überführt
/// ihn innerhalb der `tokio`-Runtime über
/// [`crate::server::BoundWebServer::from_std_listener`]. Der Deskriptor muss
/// ein Unix-Stream-Socket sein (`ListenStream=` mit Pfad); alles andere wird
/// abgelehnt.
///
/// # Errors
/// [`WebError::Activation`], wenn die Umgebung ungültig ist (siehe
/// [`check_activation_env`]) oder der Deskriptor nicht nutzbar ist.
pub fn listener_from_systemd() -> Result<UnixListener, WebError> {
    let listen_pid = std::env::var("LISTEN_PID").ok();
    let listen_fds = std::env::var("LISTEN_FDS").ok();
    check_activation_env(
        listen_pid.as_deref(),
        listen_fds.as_deref(),
        std::process::id(),
    )?;
    let mut fds = sd_listen_fds::get().map_err(|error| WebError::Activation {
        reason: error.to_string(),
    })?;
    if fds.len() != 1 {
        return Err(WebError::Activation {
            reason: format!("expected one descriptor, got {}", fds.len()),
        });
    }
    let (_name, fd) = fds.pop().ok_or_else(|| WebError::Activation {
        reason: "descriptor list was empty".to_owned(),
    })?;
    let listener = UnixListener::from(fd.into_std());
    // `local_addr` scheitert für einen Deskriptor, der kein AF_UNIX-Socket ist.
    listener.local_addr().map_err(|_| WebError::Activation {
        reason: "passed descriptor is not a Unix socket".to_owned(),
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| WebError::Activation {
            reason: format!("could not set activated socket non-blocking: {error}"),
        })?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::check_activation_env;
    use crate::error::WebError;

    #[test]
    fn test_valid_activation_env_is_accepted() {
        assert!(check_activation_env(Some("42"), Some("1"), 42).is_ok());
        assert!(check_activation_env(Some(" 42 "), Some(" 1 "), 42).is_ok());
    }

    #[test]
    fn test_invalid_activation_envs_are_rejected() {
        let cases: [(Option<&str>, Option<&str>); 8] = [
            (None, Some("1")),
            (Some("x"), Some("1")),
            (Some("41"), Some("1")),
            (Some("42"), None),
            (Some("42"), Some("x")),
            (Some("42"), Some("0")),
            (Some("42"), Some("2")),
            (Some("-1"), Some("1")),
        ];
        for (pid, fds) in cases {
            assert!(
                matches!(
                    check_activation_env(pid, fds, 42),
                    Err(WebError::Activation { .. })
                ),
                "{pid:?} {fds:?}"
            );
        }
    }

    /// Ohne systemd-Umgebung (der Testprozess wurde nicht aktiviert) bricht
    /// die Übernahme ab, statt still einen leeren Listener zu liefern.
    #[test]
    fn test_listener_from_systemd_without_activation_fails_closed() {
        // Der Testlauf erbt kein `LISTEN_PID` für die eigene PID; selbst wenn
        // eine äußere Umgebung eins setzt, nennt es nicht diesen Prozess.
        if std::env::var("LISTEN_PID")
            .ok()
            .and_then(|pid| pid.parse::<u32>().ok())
            == Some(std::process::id())
        {
            return;
        }
        assert!(matches!(
            super::listener_from_systemd(),
            Err(WebError::Activation { .. })
        ));
    }
}
