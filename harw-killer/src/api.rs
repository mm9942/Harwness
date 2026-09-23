//! Programmatische Schnittstelle für Agenten: Vorschau und Beenden eigener Prozesse.
//!
//! Die API verwendet exakt dieselbe Auswahl wie die CLI ([`crate::process::select`]:
//! procfs-Snapshot, Schutz von PID 1, eigenem Prozess und allen Vorfahren,
//! Start-Tick-/UID-Prüfung rund um das Öffnen des pidfd) und dieselbe Engine
//! ([`crate::engine::terminate`]: KILL, Warten bis `timeout`, erneutes KILL an
//! Überlebende über dasselbe gehaltene pidfd, Warten bis `kill_wait`). Es gibt keinen
//! Fallback auf `kill(numeric_pid)`.
//!
//! Unterschiede zur CLI:
//! - Es gibt keine interaktive Bestätigung; der Aufruf von [`kill_own`] selbst ist
//!   die ausdrückliche Absicht des Aufrufers.
//! - Die API verwendet **niemals** sudo oder sonstige Privilegien. Ziele, deren
//!   effektive UID nicht der effektiven UID des aufrufenden Prozesses entspricht,
//!   erhalten [`KillResult::Error`] mit `"fremder Prozess: sudo nicht erlaubt"`.
//!   Das gilt ausdrücklich auch dann, wenn der Aufrufer als root läuft: root beendet
//!   über diese API nur Prozesse mit effektiver UID 0.
//! - Eine leere Auswahl (weder Namen noch PIDs) ist ein Fehler; es wird nie
//!   „alles“ ausgewählt.
//!
//! Die Funktionen blockieren den aufrufenden Thread (procfs-Lesen, Polling in
//! höchstens 20-ms-Schritten); es werden keine Threads oder Locks erzeugt.
//! Diagnosen laufen über `tracing`; ein Subscriber wird hier nicht installiert.
//!
//! # Beispiele
//! ```no_run
//! use harw_killer::api::{KillOptions, Selection, kill_own, preview};
//! let sel = Selection { names: vec!["sleep".to_owned()], pids: Vec::new(), uid: None };
//! let targets = preview(&sel)?;
//! println!("{} Ziele", targets.len());
//! let reports = kill_own(&sel, &KillOptions::default())?;
//! for report in &reports {
//!     println!("{} -> {:?}", report.target.pid, report.result);
//! }
//! # Ok::<(), harw_killer::Error>(())
//! ```

use crate::{
    cli::Cli,
    engine,
    error::{Error, Result},
    process,
    types::{Completion, Outcome, Process, Target},
    typestate::plan::Plan,
};
use clap::Parser;
use serde::Serialize;
use std::time::Duration;

/// Fehlermeldung für Ziele, die einem anderen Benutzer gehören.
const FOREIGN: &str = "fremder Prozess: sudo nicht erlaubt";

/// Generische Prozessauswahl: exakte Namen und/oder PIDs, optional nach UID gefiltert.
///
/// Namen und PIDs bilden eine Vereinigung; `uid` schränkt beide ein (effektive UID).
/// Namen sind exakte Executable-Basenamen (kein Pfad, kein Regex). Mindestens ein
/// Name oder eine PID ist Pflicht. Die Werte werden nur geborgt; die Auswahl
/// selbst bleibt beim Aufrufer.
///
/// # Beispiele
/// ```
/// let sel = harw_killer::api::Selection { names: vec!["cargo".to_owned()], pids: vec![1234], uid: Some(1000) };
/// assert_eq!(sel.pids, [1234]);
/// ```
// Debug/Clone/Default/PartialEq/Eq sind reine Wertsemantik ohne Ressourcen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Exakte Executable-Namen (Basename, nicht leer, ohne `/`).
    pub names: Vec<String>,
    /// Explizite PIDs im Bereich `1..=i32::MAX`.
    pub pids: Vec<i32>,
    /// Optionaler Filter auf die effektive UID der Ziele.
    pub uid: Option<u32>,
}

/// Beobachtete Metadaten eines ausgewählten Prozesses; keine Signal-Capability.
///
/// Die PID ist nur Anzeige-/Berichtsdatum. Signale laufen ausschließlich über
/// das intern gehaltene pidfd.
// Serialize erzeugt ein JSON-Objekt mit denselben Feldnamen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TargetInfo {
    /// Prozess-ID zum Auswahlzeitpunkt.
    pub pid: i32,
    /// Elternprozess-ID zum Auswahlzeitpunkt.
    pub ppid: i32,
    /// Effektive UID zum Auswahlzeitpunkt.
    pub uid: u32,
    /// Executable-Basename bzw. Kernel-`comm` als Rückfall.
    pub name: String,
    /// Kommandozeile (argv, mit Leerzeichen verbunden; leer, wenn unlesbar).
    pub command: String,
}

/// Endergebnis für ein Ziel.
///
/// Serialisiert als `{"status": "<snake_case>"}` bzw. bei Fehlern als
/// `{"status": "error", "detail": "<Text>"}`.
// Adjazent getaggte Serde-Serialisierung mit snake_case-Variantennamen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum KillResult {
    /// Exit nach dem ersten KILL beobachtet.
    Killed,
    /// Exit erst nach dem zweiten KILL beobachtet.
    KilledAfterRetry,
    /// Prozess war bereits beendet bzw. der Kernel meldete ESRCH.
    AlreadyExited,
    /// Exit innerhalb der Wartegrenzen nicht beobachtet (z. B. D-State).
    Survived,
    /// Fehler für dieses Ziel (z. B. fremder Prozess, Signal verweigert).
    Error(String),
}

impl KillResult {
    /// Wahr nur, wenn der Exit beobachtet oder vom Kernel gemeldet wurde.
    ///
    /// Borgt `self`; infallibel, ohne Allokation.
    ///
    /// # Beispiele
    /// ```
    /// assert!(harw_killer::api::KillResult::Killed.success());
    /// assert!(!harw_killer::api::KillResult::Survived.success());
    /// ```
    pub fn success(&self) -> bool {
        match self {
            Self::Killed | Self::KilledAfterRetry | Self::AlreadyExited => true,
            Self::Survived | Self::Error(_) => false,
        }
    }
}

/// Ergebnisbericht für ein ausgewähltes Ziel.
// Serialize erzeugt `{"target": {...}, "result": {...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KillReport {
    /// Metadaten des Ziels zum Auswahlzeitpunkt.
    pub target: TargetInfo,
    /// Ergebnis der KILL/Warten/KILL-Sequenz.
    pub result: KillResult,
}

/// Zeitgrenzen der Terminierung; Standard wie in der CLI: 5 s / 2 s.
///
/// # Beispiele
/// ```
/// let opts = harw_killer::api::KillOptions::default();
/// assert_eq!(opts.timeout, std::time::Duration::from_secs(5));
/// assert_eq!(opts.kill_wait, std::time::Duration::from_secs(2));
/// ```
// Reine Wertsemantik; Copy, da nur zwei Durations enthalten sind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillOptions {
    /// Wartezeit nach dem ersten KILL, bevor Überlebende erneut KILL erhalten.
    pub timeout: Duration,
    /// Wartezeit nach dem zweiten KILL, bevor ein Ziel als `Survived` gilt.
    pub kill_wait: Duration,
}

impl Default for KillOptions {
    /// 5 Sekunden Timeout und 2 Sekunden Nachwartezeit (CLI-Standardwerte).
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            kill_wait: Duration::from_secs(2),
        }
    }
}

/// Wählt Ziele aus und liefert ihre Metadaten; sendet **nie** ein Signal.
///
/// Die gehaltenen pidfds werden vor der Rückkehr geschlossen; das Ergebnis ist
/// eine Momentaufnahme, keine Capability.
///
/// # Errors
/// [`Error::InvalidInput`] bei leerer oder ungültiger Auswahl (leerer Name, Pfad,
/// PID außerhalb `1..=i32::MAX`); sonst dieselben Fehler wie die CLI-Auswahl
/// (procfs-/pidfd-Fehler, unlesbare Vorfahren).
///
/// # Beispiele
/// ```no_run
/// let sel = harw_killer::api::Selection { names: vec!["sleep".to_owned()], ..Default::default() };
/// let targets = harw_killer::api::preview(&sel)?;
/// # Ok::<(), harw_killer::Error>(())
/// ```
pub fn preview(sel: &Selection) -> Result<Vec<TargetInfo>> {
    let cli = to_cli(sel)?;
    let targets = process::select(&cli)?;
    targets.iter().map(|t| info(&t.process)).collect()
}

/// Wählt Ziele aus und beendet ausschließlich eigene (gleiche effektive UID).
///
/// Ablauf wie in der CLI: Auswahl mit gehaltenen pidfds, dann für eigene Ziele
/// KILL, Warten bis `opts.timeout`, KILL an Überlebende, Warten bis
/// `opts.kill_wait`. Fremde Ziele werden nicht signalisiert und erhalten
/// `KillResult::Error("fremder Prozess: sudo nicht erlaubt")` — auch wenn der
/// Aufrufer root ist (dann gelten nur UID-0-Prozesse als eigen). sudo wird nie
/// gestartet. Die Berichte sind nach PID sortiert. Eine leere Trefferliste ist
/// kein Fehler, sondern ein leerer Vektor.
///
/// # Errors
/// [`Error::InvalidInput`] bei leerer oder ungültiger Auswahl; Auswahlfehler wie
/// bei [`preview`]. Fehler einzelner Ziele während der Terminierung werden als
/// [`KillResult::Error`] berichtet, nicht als `Err`.
///
/// # Beispiele
/// ```no_run
/// use harw_killer::api::{KillOptions, Selection, kill_own};
/// let sel = Selection { pids: vec![4242], ..Default::default() };
/// let reports = kill_own(&sel, &KillOptions::default())?;
/// # Ok::<(), harw_killer::Error>(())
/// ```
pub fn kill_own(sel: &Selection, opts: &KillOptions) -> Result<Vec<KillReport>> {
    let cli = to_cli(sel)?;
    // Der API-Aufruf selbst ist die ausdrückliche Absicht; keine Rückfrage.
    let plan = Plan::new(process::select(&cli)?).approve();
    let euid = rustix::process::geteuid().as_raw();
    let (own, foreign): (Vec<&Target>, Vec<&Target>) = plan
        .targets()
        .iter()
        .partition(|t| t.process.uid == euid);
    tracing::info!(
        own = own.len(),
        foreign = foreign.len(),
        "api termination starting"
    );
    let handles: Vec<_> = own.iter().map(|t| (t.process.pid, &t.fd)).collect();
    let mut outcomes = engine::terminate(&handles, opts.timeout, opts.kill_wait);
    let mut reports = Vec::with_capacity(plan.targets().len());
    for target in own {
        let result = match outcomes.iter().position(|o| o.pid == target.process.pid) {
            Some(index) => to_result(outcomes.swap_remove(index)),
            None => KillResult::Error("kein Ergebnis der Terminierung".to_owned()),
        };
        reports.push(KillReport {
            target: info(&target.process)?,
            result,
        });
    }
    for target in foreign {
        tracing::warn!(pid = target.process.pid, "foreign process skipped; api never uses sudo");
        reports.push(KillReport {
            target: info(&target.process)?,
            result: KillResult::Error(FOREIGN.to_owned()),
        });
    }
    reports.sort_by_key(|r| r.target.pid);
    Ok(reports)
}

// Übersetzt die Auswahl in dieselbe validierte CLI-Struktur wie der Binary-Pfad.
fn to_cli(sel: &Selection) -> Result<Cli> {
    if sel.names.is_empty() && sel.pids.is_empty() {
        return Err(Error::InvalidInput {
            field: "selection",
            reason: "mindestens ein Name oder eine PID ist erforderlich".to_owned(),
        });
    }
    if let Some(pid) = sel.pids.iter().find(|&&pid| pid <= 0) {
        return Err(Error::InvalidInput {
            field: "pid",
            reason: format!("{pid} liegt nicht im Bereich 1..=2147483647"),
        });
    }
    let mut args = vec!["killer".to_owned()];
    // `--flag=value` verhindert, dass Werte mit führendem `-` als Flags gelten.
    args.extend(sel.names.iter().map(|name| format!("--process={name}")));
    args.extend(sel.pids.iter().map(|pid| format!("--pid={pid}")));
    if let Some(uid) = sel.uid {
        args.push(format!("--uid={uid}"));
    }
    Cli::try_parse_from(args).map_err(|error| Error::InvalidInput {
        field: "selection",
        reason: error.to_string().trim_end().to_owned(),
    })
}

// Kopiert Anzeige-Metadaten; PIDs sind durch die Auswahl auf i32 begrenzt.
fn info(process: &Process) -> Result<TargetInfo> {
    let pid = i32::try_from(process.pid).map_err(|_| Error::ProcFormat {
        pid: process.pid,
        field: "PID range",
    })?;
    let ppid = i32::try_from(process.ppid).map_err(|_| Error::ProcFormat {
        pid: process.pid,
        field: "PPID range",
    })?;
    Ok(TargetInfo {
        pid,
        ppid,
        uid: process.uid,
        name: process.name.clone(),
        command: process.command.clone(),
    })
}

// Bildet den internen Engine-Bericht verlustfrei auf das API-Ergebnis ab.
fn to_result(outcome: Outcome) -> KillResult {
    match outcome.outcome {
        Completion::Killed => KillResult::Killed,
        Completion::KilledAfterRetry => KillResult::KilledAfterRetry,
        Completion::AlreadyExited => KillResult::AlreadyExited,
        Completion::Survived => KillResult::Survived,
        Completion::Error => KillResult::Error(
            outcome
                .detail
                .unwrap_or_else(|| "unbekannter Fehler".to_owned()),
        ),
    }
}

#[cfg(test)]
mod tests {
    //! Reine Tests der Abbildungen; echte Prozesse nur in `#[ignore]`-Tests.
    use super::*;
    use std::process::{Child, Command};

    // Leere Auswahl darf nie zu einer Gesamtauswahl werden.
    #[test]
    fn test_empty_selection_rejected() -> std::result::Result<(), String> {
        let sel = Selection::default();
        for result in [
            preview(&sel).map(|_| ()),
            kill_own(&sel, &KillOptions::default()).map(|_| ()),
        ] {
            match result {
                Err(Error::InvalidInput { field: "selection", .. }) => {}
                other => return Err(format!("unerwartet: {other:?}")),
            }
        }
        // Ein UID-Filter allein ist ebenfalls keine Auswahl.
        let uid_only = Selection {
            uid: Some(1000),
            ..Selection::default()
        };
        if !matches!(to_cli(&uid_only), Err(Error::InvalidInput { .. })) {
            return Err("UID-Filter allein akzeptiert".to_owned());
        }
        Ok(())
    }

    // Ungültige Namen und PIDs werden vor jedem procfs-Zugriff abgewiesen.
    #[test]
    fn test_invalid_selectors_rejected() {
        for sel in [
            Selection { pids: vec![0], ..Selection::default() },
            Selection { pids: vec![-1], ..Selection::default() },
            Selection { names: vec![String::new()], ..Selection::default() },
            Selection { names: vec!["/bin/sleep".to_owned()], ..Selection::default() },
        ] {
            assert!(matches!(to_cli(&sel), Err(Error::InvalidInput { .. })), "{sel:?}");
        }
    }

    // Die Übersetzung erhält Namen (auch mit führendem `-`), PIDs und UID.
    #[test]
    fn test_selection_to_cli() -> Result<()> {
        let cli = to_cli(&Selection {
            names: vec!["cargo".to_owned(), "-weird".to_owned()],
            pids: vec![42, 43],
            uid: Some(1000),
        })?;
        assert_eq!(cli.process, ["cargo", "-weird"]);
        assert_eq!(cli.pid, [42, 43]);
        assert_eq!(cli.uid, Some(1000));
        assert!(!cli.no_sudo || cli.helper.is_empty());
        assert!(cli.helper.is_empty());
        Ok(())
    }

    // Jede Engine-Klassifikation wird ohne Verlust abgebildet.
    #[test]
    fn test_result_mapping() {
        for (completion, detail, expected) in [
            (Completion::Killed, None, KillResult::Killed),
            (Completion::KilledAfterRetry, None, KillResult::KilledAfterRetry),
            (Completion::AlreadyExited, None, KillResult::AlreadyExited),
            (Completion::Survived, Some("d".to_owned()), KillResult::Survived),
            (Completion::Error, Some("x".to_owned()), KillResult::Error("x".to_owned())),
            (
                Completion::Error,
                None,
                KillResult::Error("unbekannter Fehler".to_owned()),
            ),
        ] {
            let outcome = Outcome {
                pid: 1,
                outcome: completion,
                detail,
            };
            assert_eq!(to_result(outcome), expected);
        }
        assert!(KillResult::KilledAfterRetry.success());
        assert!(!KillResult::Error(FOREIGN.to_owned()).success());
    }

    // Standardwerte entsprechen den CLI-Standardwerten.
    #[test]
    fn test_default_options() {
        let opts = KillOptions::default();
        assert_eq!(opts.timeout, Duration::from_secs(5));
        assert_eq!(opts.kill_wait, Duration::from_secs(2));
    }

    // Das JSON-Schema verwendet snake_case-Tags.
    #[test]
    fn test_serialization_shape() -> std::result::Result<(), serde_json::Error> {
        let report = KillReport {
            target: TargetInfo {
                pid: 42,
                ppid: 7,
                uid: 1000,
                name: "sleep".to_owned(),
                command: "sleep 60".to_owned(),
            },
            result: KillResult::KilledAfterRetry,
        };
        let value = serde_json::to_value(&report)?;
        assert_eq!(value["result"]["status"], "killed_after_retry");
        assert_eq!(value["target"]["pid"], 42);
        let error = serde_json::to_value(KillResult::Error(FOREIGN.to_owned()))?;
        assert_eq!(error["status"], "error");
        assert_eq!(error["detail"], FOREIGN);
        Ok(())
    }

    // Startet ein eigenes `sleep`-Kind als Fixture.
    fn spawn_sleep() -> std::io::Result<(Child, i32)> {
        let child = Command::new("sleep").arg("60").spawn()?;
        let pid = i32::try_from(child.id()).map_err(std::io::Error::other)?;
        Ok((child, pid))
    }

    // Vorschau findet das eigene Kind und signalisiert nicht.
    #[test]
    #[ignore = "benötigt Linux procfs, pidfd und ein `sleep`-Programm im PATH"]
    fn test_preview_own_child() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (mut child, pid) = spawn_sleep()?;
        let sel = Selection {
            pids: vec![pid],
            ..Selection::default()
        };
        let targets = preview(&sel)?;
        let alive = child.try_wait()?.is_none();
        child.kill()?;
        child.wait()?;
        assert!(alive, "preview darf nicht signalisieren");
        assert!(matches!(targets.as_slice(), [t] if t.pid == pid && t.name == "sleep"));
        Ok(())
    }

    // kill_own beendet das eigene Kind über die gemeinsame Engine.
    #[test]
    #[ignore = "benötigt Linux procfs, pidfd und ein `sleep`-Programm im PATH; sendet SIGKILL an eigenes Kind"]
    fn test_kill_own_child() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (mut child, pid) = spawn_sleep()?;
        let sel = Selection {
            pids: vec![pid],
            ..Selection::default()
        };
        let opts = KillOptions {
            timeout: Duration::from_secs(2),
            kill_wait: Duration::from_secs(1),
        };
        let reports = kill_own(&sel, &opts)?;
        let status = child.wait()?;
        assert!(!status.success());
        assert!(matches!(reports.as_slice(), [r] if r.target.pid == pid && r.result.success()));
        Ok(())
    }

    // Geschützte Prozesse (eigener Prozess) werden nie ausgewählt.
    #[test]
    #[ignore = "benötigt Linux procfs und pidfd"]
    fn test_self_is_protected() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let pid = i32::try_from(std::process::id())?;
        let reports = kill_own(
            &Selection {
                pids: vec![pid, 1],
                ..Selection::default()
            },
            &KillOptions::default(),
        )?;
        assert!(reports.is_empty());
        Ok(())
    }
}
