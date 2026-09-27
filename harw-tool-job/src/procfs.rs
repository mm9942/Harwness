//! Prozessidentität und Signale an Prozessgruppen (ohne `unsafe`).
//!
//! # Verantwortungsbereich
//! - [`JobSignal`]: die Signale, die `job.stop` senden darf.
//! - [`OwnLeader`] *(crate-intern)*: Gruppenführer eines Jobs, den **diese**
//!   Sitzung gestartet hat. Unter Linux hält er einen pidfd, der direkt nach
//!   dem Start geöffnet wird (das Kind ist dann noch nicht eingesammelt, die
//!   PID kann also nicht wiederverwendet sein; Job-Runtime-Doc §2.1, §9).
//!   Signale an die Gruppe gehen nur, solange der pidfd zeigt, dass der
//!   Führer lebt, oder — nach seinem Ende — nur, wenn die PID nicht
//!   inzwischen einem anderen Prozess gehört.
//! - [`Recovered`] *(crate-intern)*: Job einer früheren Sitzung. Eine PID
//!   allein beweist nichts (Job-Runtime-Doc §15, §25): jedes Signal geht
//!   über `harw_job_linux::LinuxRecoveryIdentity::open_verified` — pidfd
//!   öffnen, **danach** Startzeit/Programm/cgroup vergleichen, nur bei
//!   Übereinstimmung signalisieren.
//! - [`signal_child_group`]: dieselbe pidfd-Prüfung für ein
//!   `tokio::process::Child`, das der Aufrufer selbst gestartet und noch
//!   nicht eingesammelt hat.
//!
//! # Plattform
//! Die Identitätsprüfung gibt es nur unter Linux (`harw-job-linux`). Anderswo
//! werden eigene Kinder wie bisher über `kill(-pgid)` gesteuert, Jobs
//! früherer Sitzungen gelten beim Neuladen als `unknown` und werden nie
//! signalisiert.

use crate::model::{JobId, JobProcessIdentity};
use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process_group};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use tracing::debug;

#[cfg(target_os = "linux")]
use harw_job_linux::proc::snapshot;
#[cfg(target_os = "linux")]
use harw_job_linux::{
    IdentityCheck, LinuxProcess, LinuxRecoveryIdentity, ProcessError, RecoveryError, SignalKind,
};

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

    #[cfg(target_os = "linux")]
    fn to_kind(self) -> SignalKind {
        match self {
            Self::Term => SignalKind::Term,
            Self::Int => SignalKind::Int,
            Self::Hup => SignalKind::Hup,
            Self::Kill => SignalKind::Kill,
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

/// Ergebnis der Identitätsprüfung eines nicht (mehr) beaufsichtigten
/// Prozesses.
// Ohne `/proc` entstehen `Alive`/`Exited` nie.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Liveness {
    /// Der persistierte Prozess läuft noch.
    Alive,
    /// Der persistierte Prozess ist beendet.
    Exited,
    /// Die PID gehört inzwischen einem anderen Prozess, oder die Identität
    /// ist nicht beweisbar (keine Startzeit bekannt).
    Mismatch(String),
    /// `/proc/<pid>` ist nicht prüfbar (Rechte, andere Plattform).
    Unverifiable(String),
}

/// Warum ein Signal nicht zugestellt wurde.
#[derive(Debug)]
pub(crate) enum SignalError {
    /// Der Prozess (bzw. die Gruppe) existiert nicht mehr.
    Exited,
    /// Bewusst nicht signalisiert: die Identität passt nicht oder ist nicht
    /// beweisbar (Job-Runtime-Doc §25).
    Refused(String),
    /// Anderer Betriebssystemfehler.
    Io(io::Error),
}

impl fmt::Display for SignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited => f.write_str("process has already exited"),
            Self::Refused(reason) => write!(f, "not signalled: {reason}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl From<SignalError> for io::Error {
    fn from(value: SignalError) -> Self {
        match value {
            SignalError::Exited => io::Error::from_raw_os_error(3), // ESRCH
            SignalError::Refused(reason) => io::Error::new(io::ErrorKind::PermissionDenied, reason),
            SignalError::Io(err) => err,
        }
    }
}

/// Fasst ein Gruppen- und ein Direktsignal zusammen: zugestellt, wenn eines
/// davon ankam.
fn either(
    group: Result<(), SignalError>,
    direct: Result<(), SignalError>,
) -> Result<(), SignalError> {
    match (group, direct) {
        (Ok(()), _) | (_, Ok(())) => Ok(()),
        (Err(SignalError::Exited), Err(other)) | (Err(other), Err(_)) => Err(other),
    }
}

fn pid_of(raw: u32) -> Result<Pid, SignalError> {
    i32::try_from(raw)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| {
            SignalError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid pid {raw}"),
            ))
        })
}

fn from_errno(errno: rustix::io::Errno) -> SignalError {
    if errno == rustix::io::Errno::SRCH {
        SignalError::Exited
    } else {
        SignalError::Io(io::Error::from(errno))
    }
}

/// `kill(-pgid, signal)` ohne jede Prüfung — nur über die geprüften Wege
/// unten aufrufen.
fn kill_group_unchecked(pgid: u32, signal: JobSignal) -> Result<(), SignalError> {
    kill_process_group(pid_of(pgid)?, signal.to_rustix()).map_err(from_errno)
}

/// Ob es noch Prozesse in der Gruppe `pgid` gibt (Zombies zählen mit, bis
/// sie eingesammelt sind). Reine Beobachtung, sendet nichts.
#[must_use]
pub(crate) fn group_exists(pgid: u32) -> bool {
    pid_of(pgid).is_ok_and(|pid| test_kill_process_group(pid).is_ok())
}

/// Liest die Identität von `pid` (Startzeit, Programm, cgroup-v2-Pfad).
#[cfg(target_os = "linux")]
#[must_use]
pub(crate) fn capture_identity(pid: u32) -> Option<JobProcessIdentity> {
    let snap = snapshot(pid).ok()?;
    Some(JobProcessIdentity {
        start_ticks: snap.start_time_ticks,
        executable: snap.executable,
        cgroup_path: snap.cgroup_v2_path,
    })
}

/// Ohne `/proc` gibt es keine Identität.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub(crate) fn capture_identity(_pid: u32) -> Option<JobProcessIdentity> {
    None
}

#[cfg(target_os = "linux")]
fn from_process_error(error: ProcessError) -> SignalError {
    match error {
        ProcessError::AlreadyExited { .. } => SignalError::Exited,
        other => SignalError::Io(io::Error::other(other.to_string())),
    }
}

/// Gruppenführer eines Jobs, den diese Sitzung selbst gestartet hat.
///
/// # Description
/// Muss **direkt nach dem Start** über [`OwnLeader::adopt`] entstehen,
/// solange das Kind noch nicht eingesammelt ist: dann gehört die PID sicher
/// unserem Kind, und der pidfd hält genau diesen Prozess fest, auch über ein
/// späteres Einsammeln (durch den Überwachungs-Task oder tokios
/// Waisen-Einsammler nach `detach`) hinaus.
#[derive(Debug)]
pub(crate) struct OwnLeader {
    /// PID des Führers = PGID der Job-Gruppe.
    pgid: u32,
    /// Startzeit beim Start (erkennt PID-Wiederverwendung nach dem Ende).
    start_ticks: Option<u64>,
    /// pidfd des Führers (`None`, wenn der Kernel `pidfd_open` nicht kann).
    #[cfg(target_os = "linux")]
    process: Option<LinuxProcess>,
}

impl OwnLeader {
    /// Übernimmt unser eben gestartetes, noch nicht eingesammeltes Kind
    /// `pid` und erfasst seine Identität für `meta.json`.
    pub(crate) fn adopt(pid: u32) -> (Self, Option<JobProcessIdentity>) {
        // Erst der pidfd, dann die Beobachtung: der pidfd hält den Prozess.
        #[cfg(target_os = "linux")]
        let process = match LinuxProcess::open(pid) {
            Ok(process) => Some(process),
            Err(err) => {
                debug!(pid, error = %err, "job: no pidfd for own child; using the process group only");
                None
            }
        };
        let identity = capture_identity(pid);
        let leader = Self {
            pgid: pid,
            start_ticks: identity.as_ref().map(|identity| identity.start_ticks),
            #[cfg(target_os = "linux")]
            process,
        };
        (leader, identity)
    }

    /// Ob der Führer beendet ist (ein Zombie zählt als beendet).
    #[cfg(target_os = "linux")]
    #[must_use]
    pub(crate) fn has_exited(&self) -> bool {
        if let Some(process) = &self.process {
            if let Ok(exited) = process.has_exited() {
                return exited;
            }
        }
        match snapshot(self.pgid) {
            Ok(snap) => {
                snap.state.has_terminated() || Some(snap.start_time_ticks) != self.start_ticks
            }
            Err(_) => true,
        }
    }

    /// Ob der Führer beendet ist (ohne `/proc`: existiert die PID noch?).
    #[cfg(not(target_os = "linux"))]
    #[must_use]
    pub(crate) fn has_exited(&self) -> bool {
        pid_of(self.pgid)
            .map(|pid| rustix::process::test_kill_process(pid).is_err())
            .unwrap_or(true)
    }

    /// Signal an den Führer selbst (über den pidfd, falls vorhanden) — für
    /// den Fall, dass er seine Gruppe verlassen hat. Nur aufrufen, solange er
    /// lebt.
    fn signal_leader(&self, signal: JobSignal) -> Result<(), SignalError> {
        #[cfg(target_os = "linux")]
        if let Some(process) = &self.process {
            return process.signal(signal.to_kind()).map_err(from_process_error);
        }
        kill_process(pid_of(self.pgid)?, signal.to_rustix()).map_err(from_errno)
    }

    /// `Some(grund)`, wenn die PID des (beendeten) Führers inzwischen einem
    /// anderen Prozess gehört — dann ist `-pgid` nicht mehr unsere Gruppe.
    ///
    /// Solange die Gruppe Mitglieder hat, vergibt der Kernel ihre PGID nicht
    /// neu; existiert unter der PID gar kein Prozess, ist die Gruppe also
    /// (noch) unsere. Existiert einer, muss es unser Führer sein (gleiche
    /// Startzeit, z. B. als Zombie).
    #[cfg(target_os = "linux")]
    fn pgid_reused(&self) -> Option<String> {
        match snapshot(self.pgid) {
            Ok(snap) if Some(snap.start_time_ticks) == self.start_ticks => None,
            Ok(snap) => Some(format!(
                "PID {} now belongs to another process (start time {} instead of {:?})",
                self.pgid, snap.start_time_ticks, self.start_ticks
            )),
            Err(RecoveryError::AlreadyExited { .. }) => None,
            Err(err) => Some(err.to_string()),
        }
    }

    /// Ohne `/proc` nicht prüfbar (Verhalten wie vor `harw-job-linux`).
    #[cfg(not(target_os = "linux"))]
    fn pgid_reused(&self) -> Option<String> {
        None
    }

    /// Sendet `signal` an die ganze Job-Gruppe (und den Führer direkt).
    ///
    /// # Errors
    /// [`SignalError::Exited`], wenn nichts mehr lebt;
    /// [`SignalError::Refused`], wenn die PID des beendeten Führers
    /// inzwischen einem fremden Prozess gehört; sonst der OS-Fehler.
    pub(crate) fn signal_group(&self, signal: JobSignal) -> Result<(), SignalError> {
        if !self.has_exited() {
            // Der Führer lebt; er ist unser Kind (höchstens noch nicht
            // eingesammelt), seine PGID gehört also sicher uns.
            let group = kill_group_unchecked(self.pgid, signal);
            let direct = self.signal_leader(signal);
            return either(group, direct);
        }
        self.signal_orphaned_group(signal)
    }

    /// Signal an die Gruppe, deren Führer bereits beendet ist (verbliebene
    /// Nachfahren) — nur nach der Prüfung auf PID-Wiederverwendung.
    fn signal_orphaned_group(&self, signal: JobSignal) -> Result<(), SignalError> {
        if let Some(reason) = self.pgid_reused() {
            return Err(SignalError::Refused(reason));
        }
        kill_group_unchecked(self.pgid, signal)
    }

    /// Ob noch Mitglieder **unserer** Gruppe leben.
    #[must_use]
    pub(crate) fn stragglers_remain(&self) -> bool {
        group_exists(self.pgid) && self.pgid_reused().is_none()
    }

    /// Beendet verbliebene Mitglieder der Gruppe (SIGKILL), sofern es noch
    /// unsere Gruppe ist.
    pub(crate) fn kill_stragglers(&self) {
        if !group_exists(self.pgid) {
            return;
        }
        match self.signal_group(JobSignal::Kill) {
            Ok(()) | Err(SignalError::Exited) => {}
            Err(err) => {
                debug!(pgid = self.pgid, error = %err, "job stop: straggler SIGKILL skipped")
            }
        }
    }

    /// Liest die Identität des noch lebenden Führers neu (ein `exec`, etwa
    /// von `setsid` oder `sh -c`, ändert das Programm). `None`, wenn er
    /// beendet ist oder die PID nicht mehr ihm gehört.
    #[must_use]
    pub(crate) fn refresh_identity(&self) -> Option<JobProcessIdentity> {
        if self.has_exited() {
            return None;
        }
        capture_identity(self.pgid)
            .filter(|identity| Some(identity.start_ticks) == self.start_ticks)
    }
}

/// Ein Job ohne eigenen pidfd (frühere Sitzung): PID + persistierte
/// Identität.
// Ohne `/proc` werden PID und Kennungen nie gelesen.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Recovered<'a> {
    /// PID des Führers (= PGID).
    pub(crate) pid: u32,
    /// Persistierte Identität (`None`: nie steuerbar).
    pub(crate) identity: Option<&'a JobProcessIdentity>,
    /// harw-Instanz, die den Job startete (Runner-Kennung).
    pub(crate) instance: &'a str,
    /// Job-Kennung (Attempt-Kennung).
    pub(crate) job_id: &'a JobId,
}

/// Grund, warum ein Job ohne Startzeit nie gesteuert wird.
const NO_START_TIME: &str = "no recorded start time (a PID alone is no proof)";

impl Recovered<'_> {
    #[cfg(target_os = "linux")]
    fn recovery_identity(&self) -> Result<LinuxRecoveryIdentity, SignalError> {
        let Some(identity) = self.identity else {
            return Err(SignalError::Refused(NO_START_TIME.to_owned()));
        };
        let runner_id = harw_job_core::RunnerId::new(self.instance)
            .or_else(|_| harw_job_core::RunnerId::new("harw-tool-job"))
            .map_err(|err| SignalError::Refused(err.to_string()))?;
        let attempt_id = harw_job_core::AttemptId::new(self.job_id.as_str())
            .map_err(|err| SignalError::Refused(err.to_string()))?;
        Ok(LinuxRecoveryIdentity {
            pid: self.pid,
            process_start_time: Some(identity.start_ticks),
            cgroup_path: identity.cgroup_path.clone(),
            executable: identity.executable.clone(),
            runner_id,
            attempt_id,
        })
    }

    /// Prüft, ob der persistierte Prozess noch läuft (nur Beobachtung).
    #[cfg(target_os = "linux")]
    #[must_use]
    pub(crate) fn liveness(&self) -> Liveness {
        let recovery = match self.recovery_identity() {
            Ok(recovery) => recovery,
            Err(SignalError::Refused(reason)) if self.identity.is_none() => {
                return Liveness::Mismatch(reason);
            }
            Err(err) => return Liveness::Unverifiable(err.to_string()),
        };
        match recovery.verify() {
            Ok(IdentityCheck::Alive) => Liveness::Alive,
            Ok(IdentityCheck::Exited) | Err(RecoveryError::AlreadyExited { .. }) => {
                Liveness::Exited
            }
            Ok(IdentityCheck::Mismatch(reason)) => Liveness::Mismatch(reason.to_string()),
            Ok(other) => Liveness::Unverifiable(format!("{other:?}")),
            Err(err) => Liveness::Unverifiable(err.to_string()),
        }
    }

    /// Ohne `/proc` ist kein früherer Prozess prüfbar.
    #[cfg(not(target_os = "linux"))]
    #[must_use]
    pub(crate) fn liveness(&self) -> Liveness {
        if self.identity.is_none() {
            return Liveness::Mismatch(NO_START_TIME.to_owned());
        }
        Liveness::Unverifiable("process identity is only verifiable on Linux".to_owned())
    }

    /// Sendet `signal` an die Gruppe des persistierten Prozesses — nur, wenn
    /// seine Identität nach dem Öffnen eines pidfd bewiesen ist; der pidfd
    /// bleibt bis nach dem Gruppensignal offen.
    ///
    /// # Errors
    /// [`SignalError::Refused`] bei fehlender oder abweichender Identität
    /// (nichts wurde gesendet), [`SignalError::Exited`], OS-Fehler.
    #[cfg(target_os = "linux")]
    pub(crate) fn signal_group(&self, signal: JobSignal) -> Result<(), SignalError> {
        let recovery = self.recovery_identity()?;
        let leader = match recovery.open_verified() {
            Ok(leader) => leader,
            Err(RecoveryError::AlreadyExited { .. }) => return Err(SignalError::Exited),
            Err(RecoveryError::IdentityMismatch { pid, reason }) => {
                return Err(SignalError::Refused(format!(
                    "PID {pid} reused, not controlled: {reason}"
                )));
            }
            Err(err) => {
                return Err(SignalError::Refused(format!(
                    "identity of PID {} not verifiable, not controlled: {err}",
                    self.pid
                )));
            }
        };
        // Identität bewiesen und der Führer über den pidfd festgehalten:
        // solange er lebt, ist `-pid` seine Gruppe.
        let group = kill_group_unchecked(self.pid, signal);
        let direct = leader.signal(signal.to_kind()).map_err(from_process_error);
        either(group, direct)
    }

    /// Ohne `/proc` wird ein früherer Prozess nie signalisiert.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn signal_group(&self, _signal: JobSignal) -> Result<(), SignalError> {
        Err(SignalError::Refused(
            "process identity is only verifiable on Linux".to_owned(),
        ))
    }
}

/// Sendet `signal` an die Prozessgruppe eines Kindes, das der Aufrufer selbst
/// gestartet hat (als Gruppenführer, `process_group(0)`).
///
/// # Description
/// Nur solange `child` noch nicht eingesammelt ist, liefert
/// `Child::id` eine PID — genau dann gehört sie sicher diesem Kind. Unter
/// Linux wird sofort ein pidfd geöffnet und die Gruppe nur signalisiert,
/// solange der Führer lebt (bzw. danach nur, wenn die PID nicht
/// wiederverwendet ist). Ein bereits beendetes Kind ist kein Fehler.
///
/// # Errors
/// `PermissionDenied`, wenn die Gruppe nicht mehr sicher dem Kind gehört;
/// sonst der OS-Fehler.
pub fn signal_child_group(child: &tokio::process::Child, signal: JobSignal) -> io::Result<()> {
    let Some(pid) = child.id() else {
        return Ok(());
    };
    let (leader, _identity) = OwnLeader::adopt(pid);
    match leader.signal_group(signal) {
        Ok(()) | Err(SignalError::Exited) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Test-Helfer: Startzeit von `pid`.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn test_start_ticks(pid: u32) -> Option<u64> {
    capture_identity(pid).map(|identity| identity.start_ticks)
}

/// Test-Helfer: lebt `pid` (kein Zombie) und hat — falls angegeben — die
/// Startzeit `ticks`?
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn test_process_alive(pid: u32, ticks: Option<u64>) -> bool {
    snapshot(pid).is_ok_and(|snap| {
        !snap.state.has_terminated()
            && ticks.is_none_or(|expected| expected == snap.start_time_ticks)
    })
}

/// Test-Helfer ohne `/proc`: existiert `pid`?
#[cfg(all(test, not(target_os = "linux")))]
pub(crate) fn test_process_alive(pid: u32, _ticks: Option<u64>) -> bool {
    pid_of(pid).is_ok_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::test_support::{TestError, TestResult, ctx};

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
    fn test_own_process_identity() -> TestResult {
        let pid = std::process::id();
        let identity = capture_identity(pid).ok_or(TestError::Missing("own identity"))?;
        assert!(identity.executable.is_some());
        assert!(test_process_alive(pid, Some(identity.start_ticks)));
        assert!(!test_process_alive(pid, Some(identity.start_ticks + 1)));
        assert!(!test_process_alive(u32::MAX / 2, None));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_recovered_self_is_alive_and_wrong_ticks_mismatch() -> TestResult {
        let pid = std::process::id();
        let identity = capture_identity(pid).ok_or(TestError::Missing("own identity"))?;
        let job_id = JobId::parse("job-self").ok_or(TestError::Missing("job id"))?;
        let alive = Recovered {
            pid,
            identity: Some(&identity),
            instance: "harw-test",
            job_id: &job_id,
        };
        assert_eq!(alive.liveness(), Liveness::Alive);
        let wrong = JobProcessIdentity {
            start_ticks: identity.start_ticks + 1,
            ..identity.clone()
        };
        let reused = Recovered {
            identity: Some(&wrong),
            ..alive
        };
        assert!(matches!(reused.liveness(), Liveness::Mismatch(_)));
        // Würde das Signal gesendet, endete dieser Testlauf mit SIGKILL.
        assert!(matches!(
            reused.signal_group(JobSignal::Kill),
            Err(SignalError::Refused(_))
        ));
        let no_identity = Recovered {
            identity: None,
            ..alive
        };
        assert!(matches!(no_identity.liveness(), Liveness::Mismatch(_)));
        assert!(matches!(
            no_identity.signal_group(JobSignal::Kill),
            Err(SignalError::Refused(_))
        ));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_own_leader_signals_group_and_detects_exit() -> TestResult {
        use std::os::unix::process::CommandExt as _;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .map_err(ctx("spawn sleep"))?;
        let (leader, identity) = OwnLeader::adopt(child.id());
        assert!(identity.is_some());
        assert_eq!(leader.refresh_identity(), identity);
        assert!(!leader.has_exited());
        leader
            .signal_group(JobSignal::Kill)
            .map_err(|err| TestError::Unexpected(err.to_string()))?;
        let status = child.wait().map_err(ctx("wait"))?;
        assert!(!status.success());
        assert!(leader.has_exited());
        assert_eq!(leader.refresh_identity(), None);
        assert!(!leader.stragglers_remain());
        Ok(())
    }
}
