//! systemd state of the Harw-DoD units.
//!
//! One read-only `systemctl show -p ActiveState,SubState,UnitFileState`
//! query for the six units of the suite. `systemctl` is looked up in the
//! given search path (absolute entries only); when it is absent, every unit
//! is [`UnitStatus::NotAvailable`] — a host without systemd is a normal
//! state, not an error. The query is bounded in time and output size.

use std::ffi::OsStr;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::DodClientError;
use crate::fsio::sanitize;

/// The six units of the suite, in display order.
pub const DOD_UNITS: [&str; 6] = [
    "harw-dod.target",
    "harw-sentinel.service",
    "harw-probe-fs.service",
    "harw-probe-bpf.service",
    "harw-warden.socket",
    "harw-warden.service",
];

/// Properties queried per unit.
pub const SHOW_PROPERTIES: &str = "ActiveState,SubState,UnitFileState";

/// Longest wait for `systemctl show`.
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(3);

/// Poll interval while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Most stdout bytes read from `systemctl`.
const MAX_OUTPUT_BYTES: u64 = 16 * 1024;

/// Longest kept property value, in bytes.
const MAX_VALUE_BYTES: usize = 64;

/// What systemd reports for one unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UnitStatus {
    /// systemd answered.
    Reported {
        /// `ActiveState` (`active`, `inactive`, `failed`, ...).
        active_state: String,
        /// `SubState` (`running`, `dead`, `listening`, ...).
        sub_state: String,
        /// `UnitFileState` (`enabled`, `disabled`, `static`, ...); `None`
        /// when systemd knows no unit file (not installed).
        unit_file_state: Option<String>,
    },
    /// No `systemctl` on this host.
    NotAvailable,
    /// `systemctl` ran but gave no usable answer.
    Unknown {
        /// Sanitized description.
        reason: String,
    },
}

/// One unit and its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitState {
    /// Unit name, e.g. `harw-sentinel.service`.
    pub unit: String,
    /// What systemd reports.
    pub status: UnitStatus,
}

impl UnitState {
    /// `true` when systemd reports `ActiveState=active`.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(&self.status, UnitStatus::Reported { active_state, .. } if active_state == "active")
    }

    /// Whether a unit file is installed; `None` when systemd could not be
    /// asked.
    #[must_use]
    pub const fn is_installed(&self) -> Option<bool> {
        match &self.status {
            UnitStatus::Reported {
                unit_file_state, ..
            } => Some(unit_file_state.is_some()),
            UnitStatus::NotAvailable | UnitStatus::Unknown { .. } => None,
        }
    }

    /// `UnitFileState`, if reported.
    #[must_use]
    pub fn unit_file_state(&self) -> Option<&str> {
        match &self.status {
            UnitStatus::Reported {
                unit_file_state, ..
            } => unit_file_state.as_deref(),
            UnitStatus::NotAvailable | UnitStatus::Unknown { .. } => None,
        }
    }
}

/// Queries the six DoD units, looking `systemctl` up in `$PATH`.
///
/// # Errors
/// See [`unit_states_in`].
pub fn unit_states() -> Result<Vec<UnitState>, DodClientError> {
    unit_states_in(std::env::var_os("PATH").as_deref())
}

/// Queries the six DoD units, looking `systemctl` up in `search_path`
/// (a `PATH`-style list; relative entries are ignored).
///
/// # Errors
/// [`DodClientError::Systemctl`] when `systemctl` exists but cannot be
/// started or waited for (e.g. permission denied). Absence yields
/// [`UnitStatus::NotAvailable`]; a non-zero exit, a timeout or unparsable
/// output yield [`UnitStatus::Unknown`].
pub fn unit_states_in(search_path: Option<&OsStr>) -> Result<Vec<UnitState>, DodClientError> {
    let Some(systemctl) = search_path.and_then(|path| find_executable("systemctl", path)) else {
        return Ok(all(&UnitStatus::NotAvailable));
    };
    let mut child = Command::new(&systemctl)
        .arg("show")
        .arg("-p")
        .arg(SHOW_PROPERTIES)
        .args(DOD_UNITS)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| DodClientError::Systemctl {
            reason: sanitize(&error.to_string(), 256, ' '),
        })?;
    let deadline = Instant::now() + SYSTEMCTL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // Best effort: the child is ours and is reaped right after.
                let _ = child.kill();
                let _ = child.wait();
                return Ok(all(&unknown("systemctl show timed out")));
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(error) => {
                return Err(DodClientError::Systemctl {
                    reason: sanitize(&error.to_string(), 256, ' '),
                });
            }
        }
    };
    if !status.success() {
        let code = status
            .code()
            .map_or_else(|| "a signal".to_owned(), |code| format!("status {code}"));
        return Ok(all(&unknown(&format!("systemctl show exited with {code}"))));
    }
    let mut output = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        // A read error leaves `output` short; the parser then reports Unknown.
        let _ = stdout.take(MAX_OUTPUT_BYTES).read_to_end(&mut output);
    }
    Ok(parse_show_output(&String::from_utf8_lossy(&output)))
}

/// Parses `systemctl show` output for [`DOD_UNITS`]: one block of
/// `Key=Value` lines per unit, in argument order, separated by blank lines.
#[must_use]
pub fn parse_show_output(text: &str) -> Vec<UnitState> {
    let blocks: Vec<&str> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|block| !block.is_empty())
        .collect();
    if blocks.len() != DOD_UNITS.len() {
        return all(&unknown("unexpected systemctl show output"));
    }
    DOD_UNITS
        .iter()
        .zip(blocks)
        .map(|(unit, block)| UnitState {
            unit: (*unit).to_owned(),
            status: parse_block(block),
        })
        .collect()
}

fn parse_block(block: &str) -> UnitStatus {
    let mut active_state = None;
    let mut sub_state = None;
    let mut unit_file_state = None;
    for line in block.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = clean_value(value);
        match key.trim() {
            "ActiveState" => active_state = Some(value),
            "SubState" => sub_state = Some(value),
            "UnitFileState" => unit_file_state = Some(value),
            _ => {}
        }
    }
    match (active_state, sub_state) {
        (Some(active_state), Some(sub_state)) if !active_state.is_empty() => UnitStatus::Reported {
            active_state,
            sub_state,
            unit_file_state: unit_file_state.filter(|state| !state.is_empty()),
        },
        _ => unknown("ActiveState/SubState missing"),
    }
}

/// Keeps a property value only if it is a short ASCII token.
fn clean_value(value: &str) -> String {
    let value = value.trim();
    if value.len() <= MAX_VALUE_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        value.to_owned()
    } else {
        "?".to_owned()
    }
}

fn unknown(reason: &str) -> UnitStatus {
    UnitStatus::Unknown {
        reason: reason.to_owned(),
    }
}

fn all(status: &UnitStatus) -> Vec<UnitState> {
    DOD_UNITS
        .iter()
        .map(|unit| UnitState {
            unit: (*unit).to_owned(),
            status: status.clone(),
        })
        .collect()
}

/// Finds an executable regular file `name` in the absolute entries of
/// `search_path`.
fn find_executable(name: &str, search_path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const SHOW_OUTPUT: &str = "\
ActiveState=active
SubState=active
UnitFileState=enabled

ActiveState=active
SubState=running
UnitFileState=static

ActiveState=inactive
SubState=dead
UnitFileState=

ActiveState=failed
SubState=failed
UnitFileState=static

ActiveState=inactive
SubState=dead
UnitFileState=disabled

SubState=dead
UnitFileState=disabled
";

    #[test]
    fn parses_one_block_per_unit() {
        let states = parse_show_output(SHOW_OUTPUT);
        assert_eq!(states.len(), 6);
        assert_eq!(states[0].unit, "harw-dod.target");
        assert!(states[0].is_active());
        assert_eq!(states[0].unit_file_state(), Some("enabled"));
        assert_eq!(
            states[1].status,
            UnitStatus::Reported {
                active_state: "active".into(),
                sub_state: "running".into(),
                unit_file_state: Some("static".into()),
            }
        );
        assert_eq!(states[2].is_installed(), Some(false));
        assert!(!states[3].is_active());
        assert_eq!(states[4].unit, "harw-warden.socket");
        assert!(matches!(states[5].status, UnitStatus::Unknown { .. }));
    }

    #[test]
    fn mismatched_block_count_is_unknown_for_every_unit() {
        let states = parse_show_output("ActiveState=active\nSubState=running\n");
        assert_eq!(states.len(), 6);
        assert!(
            states
                .iter()
                .all(|state| matches!(state.status, UnitStatus::Unknown { .. }))
        );
        assert!(
            parse_show_output("")
                .iter()
                .all(|state| state.is_installed().is_none())
        );
    }

    #[test]
    fn hostile_values_are_neutralized() {
        let block = "ActiveState=active\u{1b}[2J\nSubState=running\nUnitFileState=enabled";
        assert_eq!(
            parse_block(block),
            UnitStatus::Reported {
                active_state: "?".into(),
                sub_state: "running".into(),
                unit_file_state: Some("enabled".into()),
            }
        );
    }

    /// All `systemctl` process tests live in one test so no other test of
    /// this crate forks while a fake script is open for writing (ETXTBSY).
    #[test]
    fn systemctl_absent_fake_and_failing() -> TestResult {
        // Absent: an empty directory as the whole search path.
        let empty = tempfile::tempdir()?;
        let states = unit_states_in(Some(empty.path().as_os_str()))?;
        assert_eq!(states.len(), 6);
        assert!(
            states
                .iter()
                .all(|state| state.status == UnitStatus::NotAvailable)
        );
        assert!(
            unit_states_in(None)?
                .iter()
                .all(|state| state.status == UnitStatus::NotAvailable)
        );
        // Relative PATH entries are never searched.
        assert!(
            unit_states_in(Some(OsStr::new("relative/bin")))?
                .iter()
                .all(|state| state.status == UnitStatus::NotAvailable)
        );

        // A fake systemctl that checks its arguments and prints six blocks.
        let fake = tempfile::tempdir()?;
        let output = fake.path().join("show.txt");
        fs::write(&output, SHOW_OUTPUT)?;
        let script = format!(
            "#!/bin/sh\n\
             [ \"$1\" = show ] && [ \"$2\" = -p ] && [ \"$3\" = {SHOW_PROPERTIES} ] && [ \"$4\" = harw-dod.target ] && [ \"$9\" = harw-warden.service ] || exit 7\n\
             cat '{}'\n",
            output.display()
        );
        write_script(&fake.path().join("systemctl"), &script)?;
        let mut path = OsString::from("relative:");
        path.push(fake.path().as_os_str());
        let states = unit_states_in(Some(&path))?;
        assert_eq!(states, parse_show_output(SHOW_OUTPUT));
        assert!(states[1].is_active());

        // A failing systemctl (no systemd as PID 1) is Unknown, not an error.
        let failing = tempfile::tempdir()?;
        write_script(
            &failing.path().join("systemctl"),
            "#!/bin/sh\necho 'System has not been booted with systemd' >&2\nexit 1\n",
        )?;
        let states = unit_states_in(Some(failing.path().as_os_str()))?;
        assert!(states.iter().all(|state| matches!(
            &state.status,
            UnitStatus::Unknown { reason } if reason.contains("status 1")
        )));

        // A non-executable file named systemctl does not count.
        let plain = tempfile::tempdir()?;
        fs::write(plain.path().join("systemctl"), "#!/bin/sh\n")?;
        assert!(
            unit_states_in(Some(plain.path().as_os_str()))?
                .iter()
                .all(|state| state.status == UnitStatus::NotAvailable)
        );
        Ok(())
    }

    fn write_script(path: &Path, body: &str) -> TestResult {
        fs::write(path, body)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }
}
