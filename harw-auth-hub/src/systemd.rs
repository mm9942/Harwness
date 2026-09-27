//! systemd socket activation (`LISTEN_PID`/`LISTEN_FDS`), without `unsafe`.
//!
//! Same approach as `dod/crates/harw-warden/src/systemd.rs`: the two
//! environment variables are validated by hand in pure, testable functions
//! ([`verify_listen_pid`], [`verify_listen_fds_count`]); only the final
//! integer → `OwnedFd` conversion, which cannot be written without `unsafe`,
//! is delegated to the dependency-free leaf crate `sd-listen-fds`. That crate
//! checks `LISTEN_PID` itself as well; the own check exists for distinct,
//! testable diagnostics (a foreign `LISTEN_PID` must be a startup error, not
//! "no socket").
//!
//! The hub expects exactly one `ListenStream=` `AF_UNIX` socket.
//!
//! [`acquire_listen_socket`] reads the process environment; call it once,
//! early, before other threads could inherit or change it.

use std::os::fd::OwnedFd;

use crate::error::HubError;

/// Check the raw `LISTEN_PID` value against the own pid.
///
/// # Errors
///
/// [`HubError::Systemd`] if it is missing, malformed or names another
/// process.
pub fn verify_listen_pid(raw: Option<&str>, current_pid: u32) -> Result<(), HubError> {
    let raw = raw.ok_or_else(|| HubError::Systemd("LISTEN_PID is not set".to_owned()))?;
    let pid: u32 = raw
        .parse()
        .map_err(|_| HubError::Systemd("LISTEN_PID is malformed".to_owned()))?;
    if pid != current_pid {
        return Err(HubError::Systemd(
            "LISTEN_PID names another process (inherited activation environment)".to_owned(),
        ));
    }
    Ok(())
}

/// Parse the raw `LISTEN_FDS` value.
///
/// # Errors
///
/// [`HubError::Systemd`] if it is missing or malformed.
pub fn verify_listen_fds_count(raw: Option<&str>) -> Result<u32, HubError> {
    let raw = raw.ok_or_else(|| HubError::Systemd("LISTEN_FDS is not set".to_owned()))?;
    raw.parse()
        .map_err(|_| HubError::Systemd("LISTEN_FDS is malformed".to_owned()))
}

/// Take the single socket systemd passed to this process.
///
/// # Errors
///
/// [`HubError::Systemd`] if the environment is invalid or does not carry
/// exactly one descriptor.
pub fn acquire_listen_socket() -> Result<OwnedFd, HubError> {
    let listen_pid = std::env::var("LISTEN_PID").ok();
    verify_listen_pid(listen_pid.as_deref(), std::process::id())?;
    let listen_fds = std::env::var("LISTEN_FDS").ok();
    let declared = verify_listen_fds_count(listen_fds.as_deref())?;
    if declared != 1 {
        return Err(HubError::Systemd(format!(
            "expected exactly 1 socket, LISTEN_FDS={declared}"
        )));
    }
    let mut fds = sd_listen_fds::get().map_err(|error| HubError::Systemd(format!("{error}")))?;
    if fds.len() != 1 {
        return Err(HubError::Systemd(format!(
            "expected exactly 1 socket, got {}",
            fds.len()
        )));
    }
    let (_name, fd) = fds
        .pop()
        .ok_or_else(|| HubError::Systemd("no socket passed".to_owned()))?;
    Ok(fd.into_std())
}

#[cfg(test)]
mod tests {
    use super::{verify_listen_fds_count, verify_listen_pid};
    use crate::error::HubError;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn missing_or_malformed_listen_pid_is_an_error() {
        assert!(matches!(
            verify_listen_pid(None, 42),
            Err(HubError::Systemd(_))
        ));
        assert!(matches!(
            verify_listen_pid(Some("x"), 42),
            Err(HubError::Systemd(_))
        ));
    }

    #[test]
    fn foreign_listen_pid_is_an_error() {
        assert!(matches!(
            verify_listen_pid(Some("999999"), 42),
            Err(HubError::Systemd(_))
        ));
    }

    #[test]
    fn own_listen_pid_is_accepted() {
        assert!(verify_listen_pid(Some("42"), 42).is_ok());
    }

    #[test]
    fn listen_fds_count_parses() -> TestResult {
        assert_eq!(verify_listen_fds_count(Some("1")).map_err(ctx("count"))?, 1);
        assert!(matches!(
            verify_listen_fds_count(None),
            Err(HubError::Systemd(_))
        ));
        assert!(matches!(
            verify_listen_fds_count(Some("one")),
            Err(HubError::Systemd(_))
        ));
        Ok(())
    }
}
