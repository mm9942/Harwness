//! Prozessidentität und Signale an Prozessgruppen (Linux, ohne `unsafe`).
//!
//! # Verantwortungsbereich
//! - [`process_start_ticks`]: Startzeit eines Prozesses aus
//!   `/proc/<pid>/stat` (Feld 22) — zusammen mit der PID eine Identität,
//!   die PID-Wiederverwendung erkennt (Neustart von harw, `meta.json`).
//! - [`is_same_process_alive`]: lebt genau dieser Prozess noch (kein Zombie)?
//! - [`signal_group`]: `kill(-pgid, sig)` über `rustix`; liefert die Gruppe
//!   nicht (mehr), trifft das Signal den Einzelprozess.
//! - [`group_exists`]: gibt es noch Mitglieder der Gruppe?
//!
//! # Plattform
//! `/proc` gibt es nur unter Linux; anderswo liefern die Lesefunktionen
//! `None`/`false` (ein Job aus einer früheren Sitzung gilt dann als
//! `unknown`).

use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process_group};
use serde::{Deserialize, Serialize};
use std::io;

/// Signale, die `job.stop` senden darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum JobSignal {
    /// SIGTERM (Vorgabe; nach der Gnadenfrist folgt SIGKILL).
    Term,
    /// SIGINT (wie Strg+C).
    Int,
    /// SIGHUP.
    Hup,
    /// SIGKILL (sofort).
    Kill,
}

impl JobSignal {
    /// Parst `TERM`, `SIGTERM`, `term`, … .
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let upper = raw.trim().to_ascii_uppercase();
        let name = upper.strip_prefix("SIG").unwrap_or(&upper);
        match name {
            "TERM" | "15" => Some(Self::Term),
            "INT" | "2" => Some(Self::Int),
            "HUP" | "1" => Some(Self::Hup),
            "KILL" | "9" => Some(Self::Kill),
            _ => None,
        }
    }

    fn to_rustix(self) -> Signal {
        match self {
            Self::Term => Signal::TERM,
            Self::Int => Signal::INT,
            Self::Hup => Signal::HUP,
            Self::Kill => Signal::KILL,
        }
    }

    /// Name wie `SIGTERM`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Int => "SIGINT",
            Self::Hup => "SIGHUP",
            Self::Kill => "SIGKILL",
        }
    }
}

fn pid_of(raw: u32) -> io::Result<Pid> {
    i32::try_from(raw)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid pid {raw}")))
}

/// Sendet `signal` an die Prozessgruppe `pgid`; existiert die Gruppe nicht
/// (mehr), an den Einzelprozess `pgid`.
///
/// # Errors
/// `ESRCH`, wenn weder Gruppe noch Prozess existieren; `EPERM` u. ä.
pub fn signal_group(pgid: u32, signal: JobSignal) -> io::Result<()> {
    let pid = pid_of(pgid)?;
    match kill_process_group(pid, signal.to_rustix()) {
        Ok(()) => Ok(()),
        Err(errno) if errno == rustix::io::Errno::SRCH => {
            kill_process(pid, signal.to_rustix()).map_err(io::Error::from)
        }
        Err(errno) => Err(io::Error::from(errno)),
    }
}

/// Ob es noch Prozesse in der Gruppe `pgid` gibt (Zombies zählen mit, bis
/// sie eingesammelt sind).
#[must_use]
pub fn group_exists(pgid: u32) -> bool {
    pid_of(pgid).is_ok_and(|pid| test_kill_process_group(pid).is_ok())
}

/// Startzeit von `pid` in Clock-Ticks seit Boot (`/proc/<pid>/stat`, Feld 22).
#[must_use]
pub fn process_start_ticks(pid: u32) -> Option<u64> {
    let (_state, ticks) = read_stat(pid)?;
    Some(ticks)
}

/// Ob der Prozess `pid` lebt, kein Zombie ist und — falls `start_ticks`
/// bekannt ist — noch derselbe Prozess ist.
#[must_use]
pub fn is_same_process_alive(pid: u32, start_ticks: Option<u64>) -> bool {
    let Some((state, ticks)) = read_stat(pid) else {
        return false;
    };
    let alive = !matches!(state, 'Z' | 'X' | 'x');
    alive && start_ticks.is_none_or(|expected| expected == ticks)
}

/// Liest Zustand (Feld 3) und Startzeit (Feld 22) aus `/proc/<pid>/stat`.
fn read_stat(pid: u32) -> Option<(char, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&text)
}

/// Reiner Parser für eine `/proc/<pid>/stat`-Zeile. Der Programmname (Feld
/// 2) steht in Klammern und darf selbst Leerzeichen/Klammern enthalten;
/// deshalb wird ab der **letzten** `)` gezählt.
fn parse_stat(text: &str) -> Option<(char, u64)> {
    let after = &text[text.rfind(')')? + 1..];
    let mut fields = after.split_whitespace();
    let state = fields.next()?.chars().next()?;
    // Nach dem Zustand (Feld 3) ist Feld 22 das 19. weitere Feld.
    let ticks = fields.nth(18)?.parse().ok()?;
    Some((state, ticks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stat_handles_parentheses_in_name() {
        let line = "1234 (my (odd) prog) S 1 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 10";
        assert_eq!(parse_stat(line), Some(('S', 987_654)));
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn test_signal_parse() {
        assert_eq!(JobSignal::parse("TERM"), Some(JobSignal::Term));
        assert_eq!(JobSignal::parse("sigkill"), Some(JobSignal::Kill));
        assert_eq!(JobSignal::parse(" int "), Some(JobSignal::Int));
        assert_eq!(JobSignal::parse("9"), Some(JobSignal::Kill));
        assert_eq!(JobSignal::parse("STOP"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_own_process_identity() {
        let pid = std::process::id();
        let ticks = process_start_ticks(pid);
        assert!(ticks.is_some());
        assert!(is_same_process_alive(pid, ticks));
        assert!(!is_same_process_alive(pid, ticks.map(|t| t + 1)));
        assert!(!is_same_process_alive(u32::MAX / 2, None));
    }
}
