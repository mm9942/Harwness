//! Datenmodell der Job-Verwaltung: [`JobId`], [`JobState`], [`JobOwner`],
//! [`JobMeta`] (Inhalt von `meta.json`), [`JobEndReason`] (Endgrund) und
//! [`JobStatus`] (Sicht für Werkzeuge und Oberfläche).
//!
//! # Nebenläufigkeit
//! Reine Werttypen, `Send + Sync`.

use crate::progress::ProgressSnapshot;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// Formatversion von `meta.json`.
///
/// # Description
/// - `1`: Prozessidentität nur als `pid` + `proc_start_ticks`.
/// - `2`: zusätzlich [`JobMeta::identity`] (Startzeit, Programm, cgroup-v2-
///   Pfad; Job-Runtime-Doc §15). `proc_start_ticks` wird weiter
///   geschrieben, damit ältere harw-Versionen v2-Dateien lesen können.
/// - Ohne Versionssprung (serde-Vorgabe) ergänzt:
///   [`JobMeta::stragglers_reaped`], [`JobMeta::log_truncated`] und
///   [`JobMeta::launch_warnings`]. Ältere Dateien laden mit `false` bzw.
///   leer; ältere harw-Versionen ignorieren die Felder (`JobMeta` hat kein
///   `deny_unknown_fields`).
///
/// Beide Versionen werden gelesen; [`JobMeta::process_identity`] liefert für
/// v1 eine Identität ohne Programm und cgroup.
pub const META_VERSION: u32 = 2;

/// Höchstlänge einer [`JobId`].
const MAX_JOB_ID_LEN: usize = 64;

/// Kennung eines Jobs; zugleich der Verzeichnisname unter `<state>/jobs/`.
///
/// # Description
/// Nur `[A-Za-z0-9_-]`, 1–64 Zeichen: eine aus Modell-Argumenten geparste
/// Kennung kann deshalb nie aus dem Job-Verzeichnis herausführen (`..`, `/`).
///
/// # Examples
/// ```rust
/// use harw_tool_job::JobId;
///
/// assert!(JobId::parse("job-20260924-101112-001").is_some());
/// assert!(JobId::parse("../etc").is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct JobId(String);

impl JobId {
    /// Prüft und übernimmt eine Kennung.
    ///
    /// # Returns
    /// `None` für leere, zu lange oder unzulässige Zeichen enthaltende Werte.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let valid = !raw.is_empty()
            && raw.len() <= MAX_JOB_ID_LEN
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        valid.then(|| Self(raw.to_owned()))
    }

    /// Die Kennung als Text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for JobId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value).ok_or_else(|| format!("invalid job id `{value}`"))
    }
}

impl From<JobId> for String {
    fn from(value: JobId) -> Self {
        value.0
    }
}

/// Zustand eines Jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Angelegt, Prozess noch nicht gestartet.
    Queued,
    /// Läuft unter Aufsicht dieser harw-Sitzung.
    Running,
    /// Mit Exit-Code 0 beendet.
    Succeeded,
    /// Mit Exit-Code ≠ 0 oder durch ein fremdes Signal beendet, oder der
    /// Start schlug fehl.
    Failed,
    /// Durch `job.stop` (bzw. die Oberfläche) beendet.
    Stopped,
    /// Aus einer früheren harw-Sitzung (oder abgelöst): der Prozess lebt
    /// noch, wird aber nicht mehr beaufsichtigt (kein Exit-Code verfügbar).
    Detached,
    /// Aus einer früheren harw-Sitzung: der Prozess existiert nicht mehr
    /// (oder die PID gehört inzwischen einem anderen Prozess); das Ergebnis
    /// ist unbekannt.
    Unknown,
}

impl JobState {
    /// `true` für Endzustände (nichts läuft mehr).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Stopped | Self::Unknown
        )
    }

    /// Kurzname (`running`, `succeeded`, …).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Detached => "detached",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Besitzer eines Jobs: die erzeugende Agenten-Sitzung und ihre Elternkette.
///
/// # Description
/// `ancestors` ist nächstliegend zuerst geordnet (Elternteil, Großelternteil,
/// …, Wurzel/UIA) und wird beim Start **einmal** über
/// [`crate::JobLineage`] festgehalten. Steuern (`status`, `logs`, `stop`,
/// `wait`) dürfen genau der Erzeuger und seine Vorfahren; Geschwister und
/// Kinder des Erzeugers nicht.
///
/// # Examples
/// ```rust
/// use harw_tool_job::JobOwner;
///
/// let owner = JobOwner::new("child", vec!["parent".into(), "uia".into()]);
/// assert!(owner.may_control("child"));
/// assert!(owner.may_control("uia"));
/// assert!(!owner.may_control("sibling"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOwner {
    /// Sitzungskennung des erzeugenden Agenten.
    pub session: String,
    /// Elternkette, nächstliegend zuerst.
    #[serde(default)]
    pub ancestors: Vec<String>,
}

impl JobOwner {
    /// Baut einen Besitzer.
    #[must_use]
    pub fn new(session: impl Into<String>, ancestors: Vec<String>) -> Self {
        Self {
            session: session.into(),
            ancestors,
        }
    }

    /// Ob `caller` diesen Job steuern darf (Erzeuger oder Vorfahr).
    #[must_use]
    pub fn may_control(&self, caller: &str) -> bool {
        !caller.is_empty()
            && (self.session == caller || self.ancestors.iter().any(|entry| entry == caller))
    }

    /// Erzeuger und Vorfahren in Zustellreihenfolge: zuerst der Erzeuger,
    /// dann der nächste Vorfahr, … (für die Weiterleitung einer Meldung,
    /// wenn der Erzeuger bereits beendet ist).
    pub fn delivery_chain(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.session.as_str()).chain(self.ancestors.iter().map(String::as_str))
    }
}

/// Inhalt von `<state>/jobs/<job_id>/meta.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobMeta {
    /// Formatversion ([`META_VERSION`]).
    pub version: u32,
    /// Kennung.
    pub job_id: JobId,
    /// Anzeigename.
    pub name: String,
    /// Befehl, wie der Agent ihn angegeben hat (bei `argv` shell-gequotet).
    pub command: String,
    /// Arbeitsverzeichnis (absolut), falls angegeben.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Namen der gesetzten Umgebungsvariablen (Werte werden nie gespeichert).
    #[serde(default)]
    pub env_keys: Vec<String>,
    /// Zustand.
    pub state: JobState,
    /// PID des Gruppenführers (= PGID).
    #[serde(default)]
    pub pid: Option<u32>,
    /// Startzeit des Prozesses in Clock-Ticks seit Boot (`/proc/<pid>/stat`,
    /// Feld 22); erkennt PID-Wiederverwendung nach einem Neustart von harw.
    #[serde(default)]
    pub proc_start_ticks: Option<u64>,
    /// Wiederherstellungsidentität des Gruppenführers (`meta.json` v2);
    /// `None` in v1-Dateien und auf Plattformen ohne `/proc`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<JobProcessIdentity>,
    /// `true`, wenn der Prozess direkt auf dem Host läuft (ohne `bwrap`).
    #[serde(default)]
    pub executed_on_host: bool,
    /// Kennung der harw-Instanz, die den Job gestartet hat.
    pub harw_instance: String,
    /// Besitzer.
    pub owner: JobOwner,
    /// Anlagezeitpunkt.
    pub created_at: Timestamp,
    /// Startzeitpunkt des Prozesses.
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// Endzeitpunkt (sofern bekannt).
    #[serde(default)]
    pub ended_at: Option<Timestamp>,
    /// Exit-Code (sofern bekannt).
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Beendendes Signal (sofern bekannt).
    #[serde(default)]
    pub signal: Option<i32>,
    /// `job.stop` wurde angefordert.
    #[serde(default)]
    pub stop_requested: bool,
    /// Vom Nutzer abgelöst (`detach`): läuft nach dem Ende von harw weiter.
    #[serde(default)]
    pub detached: bool,
    /// Periodische Fortschrittsmeldung alle so vielen Sekunden (0 = aus).
    pub notify_every_secs: u64,
    /// Zuletzt erkannter Fortschritt.
    #[serde(default)]
    pub progress: Option<ProgressSnapshot>,
    /// Zahl erkannter Warnzeilen.
    #[serde(default)]
    pub warnings: u64,
    /// Zahl erkannter Fehlerzeilen.
    #[serde(default)]
    pub errors: u64,
    /// Fehlermeldung, falls der Start selbst scheiterte.
    #[serde(default)]
    pub launch_error: Option<String>,
    /// Nach dem normalen Ende lebten noch Prozesse der Gruppe; sie wurden
    /// beendet (SIGTERM, nach der Gnadenfrist SIGKILL).
    #[serde(default)]
    pub stragglers_reaped: bool,
    /// `stdout.log` oder `stderr.log` wurde am Byte-Budget gekürzt
    /// (Markerzeile am Ende).
    #[serde(default)]
    pub log_truncated: bool,
    /// Hinweise des Startwegs (z. B. „ohne rlimits“); nur Texte, nie Werte
    /// von Umgebungsvariablen.
    #[serde(default)]
    pub launch_warnings: Vec<String>,
}

/// Signalnummer von SIGXCPU (CPU-Budget erschöpft) unter Linux.
const SIGXCPU: i32 = 24;

/// Warum ein Job endete (abgeleitet aus `meta.json`, siehe
/// [`JobMeta::end_reason`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobEndReason {
    /// Der Prozess endete selbst mit einem Exit-Code.
    Exited,
    /// Der Prozess wurde durch ein Signal beendet (nicht durch `job.stop`).
    Signal,
    /// Durch `job.stop` (bzw. die Oberfläche) beendet.
    Stopped,
    /// Nur bei SIGXCPU gemeldet (CPU-Budget erschöpft); Jobs haben keine
    /// Wanduhr-Grenze.
    Timeout,
    /// Der Start selbst scheiterte.
    LaunchError,
    /// Aus einer früheren harw-Sitzung; das Ergebnis ist unbekannt.
    Unknown,
}

impl JobEndReason {
    /// Kurzname (`exited`, `signal`, `stopped`, `timeout`, `launch-error`,
    /// `unknown`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::Signal => "signal",
            Self::Stopped => "stopped",
            Self::Timeout => "timeout",
            Self::LaunchError => "launch-error",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for JobEndReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl JobMeta {
    /// `"host"` ohne `bwrap`, sonst `"bwrap"` (gleiche Wörter wie
    /// `harw jobs list`).
    #[must_use]
    pub fn sandbox_profile(&self) -> &'static str {
        if self.executed_on_host {
            "host"
        } else {
            "bwrap"
        }
    }

    /// Warum der Job endete; `None` solange der Job nicht beendet ist.
    ///
    /// # Description
    /// Reihenfolge: Startfehler, dann `job.stop`, dann SIGXCPU (`timeout`),
    /// dann sonstiges Signal, dann Exit-Code, zuletzt `unknown` für Jobs
    /// früherer Sitzungen ohne Ergebnis.
    #[must_use]
    pub fn end_reason(&self) -> Option<JobEndReason> {
        if !self.state.is_terminal() {
            return None;
        }
        if self.launch_error.is_some() {
            return Some(JobEndReason::LaunchError);
        }
        if self.state == JobState::Stopped || self.stop_requested {
            return Some(JobEndReason::Stopped);
        }
        match (self.signal, self.exit_code) {
            (Some(SIGXCPU), _) => Some(JobEndReason::Timeout),
            (Some(_), _) => Some(JobEndReason::Signal),
            (None, Some(_)) => Some(JobEndReason::Exited),
            (None, None) if self.state == JobState::Unknown => Some(JobEndReason::Unknown),
            (None, None) => None,
        }
    }

    /// Die persistierte Identität des Gruppenführers: `identity` (v2), sonst
    /// aus `proc_start_ticks` (v1, ohne Programm und cgroup). `None`, wenn
    /// keine Startzeit bekannt ist — eine PID allein beweist nichts
    /// (Job-Runtime-Doc §2.1, §25), ein solcher Job wird nie signalisiert.
    #[must_use]
    pub fn process_identity(&self) -> Option<JobProcessIdentity> {
        self.identity.clone().or_else(|| {
            self.proc_start_ticks.map(|start_ticks| JobProcessIdentity {
                start_ticks,
                executable: None,
                cgroup_path: None,
            })
        })
    }
}

/// Plattformneutrale, persistierbare Identität eines Job-Prozesses
/// (`meta.json` v2, Job-Runtime-Doc §15).
///
/// # Description
/// Unter Linux wird sie beim Neuladen in eine
/// `harw_job_linux::LinuxRecoveryIdentity` übersetzt: ein früherer Job wird
/// nur gesteuert, wenn die PID nach dem Öffnen eines pidfd noch dieselbe
/// Startzeit (und, wo beide Seiten bekannt sind, dasselbe Programm und
/// dieselbe cgroup) hat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProcessIdentity {
    /// Startzeit in Clock-Ticks seit Boot (`/proc/<pid>/stat`, Feld 22).
    pub start_ticks: u64,
    /// `/proc/<pid>/exe`, zuletzt unter Aufsicht beobachtet (ein `exec` des
    /// Gruppenführers ändert es; die Überwachung schreibt es nach).
    #[serde(default)]
    pub executable: Option<String>,
    /// Pfad in der cgroup-v2-Hierarchie (`0::<pfad>` aus
    /// `/proc/<pid>/cgroup`).
    #[serde(default)]
    pub cgroup_path: Option<String>,
}

/// Sicht auf einen Job für Werkzeuge und Oberfläche.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobStatus {
    /// Persistierter Zustand.
    #[serde(flatten)]
    pub meta: JobMeta,
    /// Laufzeit in Sekunden (bis jetzt bzw. bis zum Ende), sofern bekannt.
    pub runtime_secs: Option<u64>,
    /// Vollständige stdout-Zeilen, die diese Sitzung gesehen hat.
    pub stdout_lines: u64,
    /// Vollständige stderr-Zeilen, die diese Sitzung gesehen hat.
    pub stderr_lines: u64,
    /// Letzte Ausgabezeilen (stdout und stderr in ungefährer Ankunftsfolge).
    pub last_lines: Vec<String>,
    /// Verzeichnis mit `stdout.log`, `stderr.log`, `meta.json`.
    pub log_dir: PathBuf,
}

/// Dateiname von stdout im Job-Verzeichnis.
pub const STDOUT_LOG: &str = "stdout.log";
/// Dateiname von stderr im Job-Verzeichnis.
pub const STDERR_LOG: &str = "stderr.log";
/// Dateiname der Metadaten im Job-Verzeichnis.
pub const META_FILE: &str = "meta.json";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_job_id_rejects_path_traversal_and_bad_chars() {
        assert!(JobId::parse("job-1_a").is_some());
        assert!(JobId::parse("  job-1  ").is_some_and(|id| id.as_str() == "job-1"));
        for bad in ["", " ", "../x", "a/b", "a.b", "a b", "ä", &"x".repeat(65)] {
            assert!(JobId::parse(bad).is_none(), "accepted {bad:?}");
        }
    }

    #[test]
    fn test_owner_may_control_only_creator_and_ancestors() {
        let owner = JobOwner::new("worker", vec!["orchestrator".into(), "uia".into()]);
        assert!(owner.may_control("worker"));
        assert!(owner.may_control("orchestrator"));
        assert!(owner.may_control("uia"));
        assert!(!owner.may_control("sibling-worker"));
        assert!(!owner.may_control("grandchild"));
        assert!(!owner.may_control(""));
        let chain: Vec<&str> = owner.delivery_chain().collect();
        assert_eq!(chain, vec!["worker", "orchestrator", "uia"]);
    }

    fn v1_json(ticks: &str) -> String {
        format!(
            r#"{{"version":1,"job_id":"job-1","name":"n","command":"c","state":"running",
               "pid":42,{ticks}"harw_instance":"harw-old",
               "owner":{{"session":"s"}},"created_at":"2026-01-01T00:00:00Z",
               "notify_every_secs":60}}"#
        )
    }

    #[test]
    fn test_v1_meta_converts_to_identity_without_exe() -> TestResult {
        let meta: JobMeta = serde_json::from_str(&v1_json(r#""proc_start_ticks":777,"#))
            .map_err(ctx("parse v1"))?;
        assert_eq!(meta.version, 1);
        assert_eq!(meta.identity, None);
        assert_eq!(
            meta.process_identity(),
            Some(JobProcessIdentity {
                start_ticks: 777,
                executable: None,
                cgroup_path: None,
            })
        );
        let without: JobMeta =
            serde_json::from_str(&v1_json("")).map_err(ctx("parse v1 without ticks"))?;
        assert_eq!(without.process_identity(), None);
        Ok(())
    }

    #[test]
    fn test_v2_identity_roundtrip_and_precedence() -> TestResult {
        let mut meta: JobMeta =
            serde_json::from_str(&v1_json(r#""proc_start_ticks":1,"#)).map_err(ctx("parse v1"))?;
        let identity = JobProcessIdentity {
            start_ticks: 2,
            executable: Some("/usr/bin/sleep".to_owned()),
            cgroup_path: Some("/user.slice".to_owned()),
        };
        meta.version = META_VERSION;
        meta.identity = Some(identity.clone());
        let json = serde_json::to_string(&meta).map_err(ctx("serialize v2"))?;
        assert!(json.contains("\"identity\""));
        assert!(json.contains("\"proc_start_ticks\":1"));
        let back: JobMeta = serde_json::from_str(&json).map_err(ctx("parse v2"))?;
        assert_eq!(back, meta);
        // v2 `identity` wins over the v1 field.
        assert_eq!(back.process_identity(), Some(identity));
        Ok(())
    }

    #[test]
    fn test_state_terminal_classification() {
        assert!(!JobState::Queued.is_terminal());
        assert!(!JobState::Running.is_terminal());
        assert!(!JobState::Detached.is_terminal());
        assert!(JobState::Succeeded.is_terminal());
        assert!(JobState::Failed.is_terminal());
        assert!(JobState::Stopped.is_terminal());
        assert!(JobState::Unknown.is_terminal());
    }

    #[test]
    fn test_old_meta_defaults_new_fields() -> TestResult {
        let v1: JobMeta = serde_json::from_str(&v1_json("")).map_err(ctx("parse v1"))?;
        let v2: JobMeta = serde_json::from_str(
            &v1_json(r#""proc_start_ticks":5,"#).replace(r#""version":1"#, r#""version":2"#),
        )
        .map_err(ctx("parse v2 without new keys"))?;
        assert_eq!(v2.version, 2);
        for meta in [&v1, &v2] {
            assert!(!meta.stragglers_reaped);
            assert!(!meta.log_truncated);
            assert!(meta.launch_warnings.is_empty());
            assert_eq!(meta.sandbox_profile(), "bwrap");
            assert_eq!(meta.state, JobState::Running);
            assert_eq!(meta.end_reason(), None);
        }
        Ok(())
    }

    #[test]
    fn test_new_fields_roundtrip() -> TestResult {
        let mut meta: JobMeta = serde_json::from_str(&v1_json("")).map_err(ctx("parse v1"))?;
        meta.version = META_VERSION;
        meta.stragglers_reaped = true;
        meta.log_truncated = true;
        meta.launch_warnings = vec!["started without rlimits".to_owned()];
        let json = serde_json::to_string(&meta).map_err(ctx("serialize"))?;
        assert!(json.contains("\"stragglers_reaped\":true"));
        assert!(json.contains("\"log_truncated\":true"));
        assert!(json.contains("\"launch_warnings\":[\"started without rlimits\"]"));
        let back: JobMeta = serde_json::from_str(&json).map_err(ctx("parse back"))?;
        assert_eq!(back, meta);
        Ok(())
    }

    #[test]
    fn test_end_reason_derivation() -> TestResult {
        let base: JobMeta = serde_json::from_str(&v1_json("")).map_err(ctx("parse v1"))?;
        type Row = (
            JobState,
            Option<i32>,
            Option<i32>,
            bool,
            Option<&'static str>,
            Option<JobEndReason>,
        );
        let table: [Row; 12] = [
            (
                JobState::Succeeded,
                Some(0),
                None,
                false,
                None,
                Some(JobEndReason::Exited),
            ),
            (
                JobState::Failed,
                Some(2),
                None,
                false,
                None,
                Some(JobEndReason::Exited),
            ),
            (
                JobState::Failed,
                None,
                Some(9),
                false,
                None,
                Some(JobEndReason::Signal),
            ),
            (
                JobState::Failed,
                None,
                Some(24),
                false,
                None,
                Some(JobEndReason::Timeout),
            ),
            (
                JobState::Stopped,
                None,
                Some(15),
                false,
                None,
                Some(JobEndReason::Stopped),
            ),
            (
                JobState::Failed,
                None,
                Some(9),
                true,
                None,
                Some(JobEndReason::Stopped),
            ),
            (
                JobState::Failed,
                None,
                None,
                false,
                Some("spawn failed"),
                Some(JobEndReason::LaunchError),
            ),
            (
                JobState::Unknown,
                None,
                None,
                false,
                None,
                Some(JobEndReason::Unknown),
            ),
            (JobState::Failed, None, None, false, None, None),
            (JobState::Running, None, None, false, None, None),
            (JobState::Detached, None, None, false, None, None),
            (JobState::Queued, Some(0), None, false, None, None),
        ];
        for (state, exit_code, signal, stop_requested, launch_error, expected) in table {
            let mut meta = base.clone();
            meta.state = state;
            meta.exit_code = exit_code;
            meta.signal = signal;
            meta.stop_requested = stop_requested;
            meta.launch_error = launch_error.map(str::to_owned);
            assert_eq!(meta.end_reason(), expected, "state {state}");
        }
        let names: Vec<&str> = [
            JobEndReason::Exited,
            JobEndReason::Signal,
            JobEndReason::Stopped,
            JobEndReason::Timeout,
            JobEndReason::LaunchError,
            JobEndReason::Unknown,
        ]
        .into_iter()
        .map(JobEndReason::as_str)
        .collect();
        assert_eq!(
            names,
            [
                "exited",
                "signal",
                "stopped",
                "timeout",
                "launch-error",
                "unknown"
            ]
        );
        let json = serde_json::to_string(&JobEndReason::LaunchError).map_err(ctx("serialize"))?;
        assert_eq!(json, "\"launch-error\"");
        Ok(())
    }

    #[test]
    fn test_sandbox_profile_host_and_bwrap() -> TestResult {
        let mut meta: JobMeta = serde_json::from_str(&v1_json("")).map_err(ctx("parse v1"))?;
        assert_eq!(meta.sandbox_profile(), "bwrap");
        meta.executed_on_host = true;
        assert_eq!(meta.sandbox_profile(), "host");
        Ok(())
    }
}
