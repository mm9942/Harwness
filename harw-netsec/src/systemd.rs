//! systemd socket activation (`--systemd-socket`).
//!
//! # Contract
//! Same pattern as `harw-warden` (`dod/crates/harw-warden/src/systemd.rs`):
//! the activation environment is checked explicitly first — `LISTEN_PID`
//! must name this process and `LISTEN_FDS` must be exactly `1` — and only
//! then does `sd_listen_fds::get()` turn descriptor 3 into an owned fd, so
//! this crate needs no `unsafe`. `sd_listen_fds::get()` alone would silently
//! return an empty list for a foreign or missing `LISTEN_PID`; here that is a
//! start-up error, because a daemon started with `--systemd-socket` that
//! quietly binds nothing is indistinguishable from a healthy one.
//!
//! Activation is only used when requested on the command line. There is no
//! automatic fallback between activation and binding a path.
//!
//! ```ini
//! # harw-netsec.socket
//! [Socket]
//! ListenStream=/run/harw/infra/network.sock
//! SocketMode=0660
//! SocketUser=harw-netsec
//! SocketGroup=harw-network
//! DirectoryMode=0750
//! RemoveOnStop=yes
//! ```

use std::os::unix::net::UnixListener;

use crate::error::{NetsecError, NetsecResult};

/// Validates the raw activation environment.
///
/// # Errors
/// [`NetsecError::Activation`] if `LISTEN_PID` is missing, malformed or
/// foreign, or `LISTEN_FDS` is missing, malformed or not `1`.
pub fn check_activation_env(
    listen_pid: Option<&str>,
    listen_fds: Option<&str>,
    own_pid: u32,
) -> NetsecResult<()> {
    let fail = |reason: &str| {
        Err(NetsecError::Activation {
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

/// Takes over the one socket passed by systemd.
///
/// # Description
/// Returns a non-blocking `std` listener; the caller converts it with
/// `tokio::net::UnixListener::from_std` inside the runtime. The descriptor
/// must be a Unix stream socket (`ListenStream=` with a path); anything else
/// is rejected.
///
/// # Errors
/// [`NetsecError::Activation`] or [`NetsecError::Io`].
pub fn listener_from_systemd() -> NetsecResult<UnixListener> {
    let listen_pid = std::env::var("LISTEN_PID").ok();
    let listen_fds = std::env::var("LISTEN_FDS").ok();
    check_activation_env(
        listen_pid.as_deref(),
        listen_fds.as_deref(),
        std::process::id(),
    )?;
    let mut fds = sd_listen_fds::get().map_err(|error| NetsecError::Activation {
        reason: error.to_string(),
    })?;
    if fds.len() != 1 {
        return Err(NetsecError::Activation {
            reason: format!("expected one descriptor, got {}", fds.len()),
        });
    }
    let (_name, fd) = fds.pop().ok_or_else(|| NetsecError::Activation {
        reason: "descriptor list was empty".to_owned(),
    })?;
    let listener = UnixListener::from(fd.into_std());
    // `local_addr` fails for a descriptor that is not an AF_UNIX socket.
    listener.local_addr().map_err(|_| NetsecError::Activation {
        reason: "passed descriptor is not a Unix socket".to_owned(),
    })?;
    listener
        .set_nonblocking(true)
        .map_err(NetsecError::io("set activated socket non-blocking"))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_activation_env_is_accepted() {
        assert!(check_activation_env(Some("42"), Some("1"), 42).is_ok());
    }

    #[test]
    fn test_invalid_activation_envs_are_rejected() {
        let cases: [(Option<&str>, Option<&str>); 7] = [
            (None, Some("1")),
            (Some("x"), Some("1")),
            (Some("41"), Some("1")),
            (Some("42"), None),
            (Some("42"), Some("x")),
            (Some("42"), Some("0")),
            (Some("42"), Some("2")),
        ];
        for (pid, fds) in cases {
            assert!(
                matches!(
                    check_activation_env(pid, fds, 42),
                    Err(NetsecError::Activation { .. })
                ),
                "{pid:?} {fds:?}"
            );
        }
    }
}
