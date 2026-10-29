//! Datenmodell der Job-Verwaltung: [`JobId`], [`JobState`], [`JobOwner`],
//! [`JobMeta`] (Inhalt von `meta.json`), [`ManagedJobOrigin`] (Herkunft),
//! [`JobEndReason`] (Endgrund) und [`JobStatus`] (Sicht für Werkzeuge und
//! Oberfläche).
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
///   [`JobMeta::stragglers_reaped`], [`JobMeta::log_truncated`],
///   [`JobMeta::launch_warnings`], [`JobMeta::origin`] (W20-A) und
///   [`JobProcessIdentity::dedicated_cgroup`] (PL-97 E4). Ältere
///   Dateien laden mit `false` bzw. leer; ältere harw-Versionen ignorieren
///   die Felder (`JobMeta` hat kein `deny_unknown_fields`).
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
    /// R18 (TUI-07): `call_id` des Werkzeugaufrufs, der den Job startete.
    /// Die Oberfläche verknüpft darüber Job und Werkzeugzelle und zeigt den
    /// Aufruf in der Job-Detailansicht zuerst. Additiv: fehlt in
    /// `meta.json` älterer Versionen und bei Starts ohne Werkzeugaufruf
    /// (Operator-Wege, Kind-Backends); dann nicht serialisiert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_call_id: Option<String>,
    /// R18: Name des startenden Werkzeugs (z. B. `job.start`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_tool: Option<String>,
    /// R18: Anzeigename des besitzenden Agenten, falls bekannt (sonst gilt
    /// [`JobOwner::session`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent: Option<String>,
    /// W20-A: typed provenance of the job ([`ManagedJobOrigin`]).
    ///
    /// Additive like the R18 fields: missing in older `meta.json` (then
    /// [`JobMeta::resolved_origin`] falls back to `origin_tool` /
    /// `origin_call_id`) and not serialized when unset. An origin kind this
    /// version does not know reads as unset instead of making the whole
    /// record unreadable (a running job must stay visible and stoppable).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_origin_lenient"
    )]
    pub origin: Option<ManagedJobOrigin>,
}

/// Typed provenance of a managed job (W20-A): what started it.
///
/// # Description
/// Persisted as [`JobMeta::origin`] and carried by
/// [`crate::JobEvent::Started`]. Records written before the field existed
/// only carry the legacy R18 fields `origin_tool` / `origin_call_id`;
/// [`JobMeta::resolved_origin`] maps those to [`ManagedJobOrigin::Tool`]. A
/// record with neither has **unknown** provenance (`None`); consumers must
/// never guess it from the job name.
///
/// # Examples
/// ```rust
/// use harw_tool_job::ManagedJobOrigin;
///
/// # fn main() -> Result<(), serde_json::Error> {
/// let origin = ManagedJobOrigin::AgentChild {
///     child_session_id: "child-1".into(),
/// };
/// let json = serde_json::to_string(&origin)?;
/// assert_eq!(json, r#"{"kind":"agent_child","child_session_id":"child-1"}"#);
/// assert_eq!(serde_json::from_str::<ManagedJobOrigin>(&json)?, origin);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ManagedJobOrigin {
    /// The job runs a child agent (the job child backend of
    /// `harw-agent-runner`).
    AgentChild {
        /// Session id of the child agent run this job backs.
        child_session_id: String,
    },
    /// Started by a model tool call (e.g. `job.start`).
    Tool {
        /// Name of the starting tool; empty when a legacy record kept only
        /// the `call_id`.
        tool: String,
        /// `call_id` of the starting tool call, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
    },
    /// Started by the operator (a TUI/CLI command), not by a model.
    Operator,
}

impl ManagedJobOrigin {
    /// Maps the legacy R18 fields (`origin_tool`, `origin_call_id`) to a
    /// typed origin.
    ///
    /// # Returns
    /// [`ManagedJobOrigin::Tool`] when at least one of them is set (only a
    /// tool call records a `call_id`), `None` (unknown provenance) when both
    /// are absent.
    #[must_use]
    pub fn from_legacy(tool: Option<&str>, call_id: Option<&str>) -> Option<Self> {
        if tool.is_none() && call_id.is_none() {
            return None;
        }
        Some(Self::Tool {
            tool: tool.unwrap_or_default().to_owned(),
            call_id: call_id.map(str::to_owned),
        })
    }
}

/// Reads [`JobMeta::origin`]; an unknown or malformed origin becomes `None`
/// (unknown provenance) instead of failing the whole `meta.json`.
fn deserialize_origin_lenient<'de, D>(deserializer: D) -> Result<Option<ManagedJobOrigin>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| match serde_json::from_value(value) {
        Ok(origin) => Some(origin),
        Err(err) => {
            tracing::debug!(error = %err, "job meta.json: unknown origin ignored");
            None
        }
    }))
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
                dedicated_cgroup: None,
            })
        })
    }

    /// Provenance of the job (W20-A): [`JobMeta::origin`] if set, else the
    /// legacy `origin_tool` / `origin_call_id` mapped by
    /// [`ManagedJobOrigin::from_legacy`].
    ///
    /// # Returns
    /// `None` for unknown provenance (a record without any origin field);
    /// it is never guessed from [`JobMeta::name`].
    #[must_use]
    pub fn resolved_origin(&self) -> Option<ManagedJobOrigin> {
        self.origin.clone().or_else(|| {
            ManagedJobOrigin::from_legacy(
                self.origin_tool.as_deref(),
                self.origin_call_id.as_deref(),
            )
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
    /// `/proc/<pid>/cgroup`), relativ zur Hierarchie, nicht unter dem
    /// Einhängepunkt. Ohne delegierte cgroup-Wurzel ist das die geteilte
    /// cgroup von harw bzw. der Sitzung, nicht die des Jobs (siehe
    /// [`JobStatus::job_cgroup_path`]).
    #[serde(default)]
    pub cgroup_path: Option<String>,
    /// Proof from the job runtime that [`JobProcessIdentity::cgroup_path`]
    /// is this job's own cgroup (PL-97 E4). `None` (the default, also for
    /// older records) means the path is never attributed to the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedicated_cgroup: Option<DedicatedCgroupProof>,
}

/// Runtime attestation that a job runs in its own, dedicated cgroup
/// (PL-97 E4, PR #154 review).
///
/// # Description
/// A `harw-job-*` leaf name alone proves nothing: without a delegated root
/// harw may itself run in a shared cgroup whose leaf happens to match, and
/// every job would then persist that same path. The proof therefore records
/// what the job runtime attested at launch: the attempt id it created the
/// cgroup for and the delegated root it created it beneath, bound to the
/// job it was attested for.
///
/// The attempt id is assigned by the command port (`<work>-e<epoch>`) and
/// cannot be derived from [`JobMeta`], so the proof carries the
/// [`JobId`] it belongs to; [`JobStatus::job_cgroup_path`] rejects a proof
/// stored in another job's record. The proof must be written only from the
/// launch result (`StartedCommand`) of this job's own launch through a
/// delegated root, never copied, cached or shared between jobs: uniqueness
/// of the attempt id beyond the job binding rests on that single writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DedicatedCgroupProof {
    /// The job this proof was attested for; must equal [`JobMeta::job_id`]
    /// of the record that stores it. `None` (an unbound proof) is never
    /// accepted.
    #[serde(default)]
    pub job_id: Option<JobId>,
    /// Attempt id the job runtime attested for this job's launch; its cgroup
    /// is `harw-job-<attempt id>` (characters outside `[A-Za-z0-9_.-]`
    /// replaced by `_`, as the runtime names it).
    pub attempt_id: String,
    /// The delegated cgroup root the attempt cgroup was created directly
    /// beneath, hierarchy-relative (e.g. `/harw.slice`); a leading
    /// `/sys/fs/cgroup` mount point is tolerated.
    pub delegated_root: String,
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

/// Name prefix of a dedicated job cgroup: the job runtime places each attempt
/// in its own child cgroup `harw-job-<attempt>` beneath the delegated cgroup
/// root (`harw-job-runtime`, `coordinator/linux.rs`, `cgroup_name`).
const JOB_CGROUP_PREFIX: &str = "harw-job-";

/// The conventional cgroup-v2 mount point, tolerated in front of a
/// [`DedicatedCgroupProof::delegated_root`].
const CGROUP_MOUNT: &str = "/sys/fs/cgroup";

/// The cgroup leaf the job runtime creates for `attempt_id`:
/// `harw-job-<attempt>` with every character outside `[A-Za-z0-9_.-]`
/// replaced by `_` (mirrors `cgroup_name` in `harw-job-runtime`).
fn attempt_cgroup_leaf(attempt_id: &str) -> String {
    let attempt: String = attempt_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    format!("{JOB_CGROUP_PREFIX}{attempt}")
}

/// Splits an absolute cgroup path into its components; `None` for relative
/// paths and for empty, `.` or `..` components (never resolved, never
/// trusted).
fn cgroup_components(path: &str) -> Option<Vec<&str>> {
    let rest = path.strip_prefix('/')?;
    if rest.is_empty() {
        return Some(Vec::new());
    }
    let parts: Vec<&str> = rest.split('/').collect();
    parts
        .iter()
        .all(|part| !part.is_empty() && *part != "." && *part != "..")
        .then_some(parts)
}

/// Whether `path` is proven to be the dedicated cgroup described by
/// `proof`: its parent is exactly the (non-hierarchy-root) delegated root
/// and its leaf is the cgroup the runtime names for the attested attempt.
fn is_proven_job_cgroup(path: &str, proof: &DedicatedCgroupProof) -> bool {
    if proof.attempt_id.is_empty() {
        return false;
    }
    let root = proof
        .delegated_root
        .strip_prefix(CGROUP_MOUNT)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .map_or(proof.delegated_root.as_str(), |rest| {
            if rest.is_empty() { "/" } else { rest }
        });
    let (Some(root), Some(path)) = (cgroup_components(root), cgroup_components(path)) else {
        return false;
    };
    let Some((leaf, parent)) = path.split_last() else {
        return false;
    };
    // The hierarchy root is never a delegated root, and the root must be a
    // proper ancestor: the attempt cgroup lies directly beneath it.
    !root.is_empty() && parent == root.as_slice() && *leaf == attempt_cgroup_leaf(&proof.attempt_id)
}

impl JobStatus {
    /// cgroup-v2 path of the job's group leader as persisted in its process
    /// identity ([`JobProcessIdentity::cgroup_path`], `meta.json` v2).
    ///
    /// # Description
    /// The value is what `/proc/<pid>/cgroup` reported (`0::<path>`), so it
    /// is relative to the cgroup-v2 hierarchy (e.g. `/user.slice/…`), not a
    /// path under the mount point (`/sys/fs/cgroup/…`).
    ///
    /// It is **not necessarily the job's own cgroup**: without a delegated
    /// cgroup root (the default) the job stays in harw's own or the
    /// session's shared cgroup, which other processes and jobs share.
    /// Attribution (e.g. matching findings to jobs) must use
    /// [`JobStatus::job_cgroup_path`] instead.
    ///
    /// # Returns
    /// `None` for v1 records, platforms without `/proc`, or when the path
    /// was not observed.
    #[must_use]
    pub fn cgroup_path(&self) -> Option<&str> {
        self.meta
            .identity
            .as_ref()
            .and_then(|identity| identity.cgroup_path.as_deref())
    }

    /// The job's **dedicated** cgroup: [`JobStatus::cgroup_path`] only with
    /// a runtime proof ([`JobProcessIdentity::dedicated_cgroup`]).
    ///
    /// # Description
    /// Hierarchy-relative like [`JobStatus::cgroup_path`]. Use this value to
    /// attribute cgroup-scoped observations to the job. A `harw-job-*` leaf
    /// name is no proof: harw itself may run in a shared cgroup whose leaf
    /// matches. The path counts only when the persisted proof belongs to
    /// this job ([`DedicatedCgroupProof::job_id`]), attests the attempt (the
    /// leaf is `harw-job-<attested attempt id>`) and a delegated root that
    /// is the path's direct parent. Everything else
    /// fails closed instead of assigning one shared cgroup to every job.
    ///
    /// # Returns
    /// `None` when the path is unknown, no proof is recorded, the proof
    /// belongs to another job, or it does not match the path (e.g. a leaf
    /// for another attempt).
    #[must_use]
    pub fn job_cgroup_path(&self) -> Option<&str> {
        let identity = self.meta.identity.as_ref()?;
        let proof = identity
            .dedicated_cgroup
            .as_ref()
            .filter(|proof| proof.job_id.as_ref() == Some(&self.meta.job_id))?;
        identity
            .cgroup_path
            .as_deref()
            .filter(|path| is_proven_job_cgroup(path, proof))
    }
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

    /// R18 (TUI-07): die Herkunft ist additiv — alte `meta.json` lesen sich
    /// ohne sie und schreiben sich ohne sie zurück; gesetzt überlebt sie den
    /// Rundlauf.
    #[test]
    fn test_meta_origin_is_additive_and_roundtrips() -> TestResult {
        let mut meta: JobMeta = serde_json::from_str(&v1_json("")).map_err(ctx("parse v1"))?;
        assert_eq!(meta.origin_call_id, None);
        assert_eq!(meta.origin_tool, None);
        assert_eq!(meta.owner_agent, None);
        let plain = serde_json::to_string(&meta).map_err(ctx("serialize plain"))?;
        for key in ["origin_call_id", "origin_tool", "owner_agent"] {
            assert!(!plain.contains(key), "{key} in {plain}");
        }

        meta.origin_call_id = Some("call-7".to_owned());
        meta.origin_tool = Some("job.start".to_owned());
        meta.owner_agent = Some("uia".to_owned());
        let json = serde_json::to_string(&meta).map_err(ctx("serialize origin"))?;
        let back: JobMeta = serde_json::from_str(&json).map_err(ctx("parse origin"))?;
        assert_eq!(back, meta);
        Ok(())
    }

    /// `meta.json` with only the R18 tool origin (written before W20-A).
    const LEGACY_TOOL_ORIGIN: &str = include_str!("../tests/fixtures/meta-legacy-tool-origin.json");
    /// `meta.json` v1 without any origin; its name looks like a child agent
    /// job on purpose.
    const LEGACY_NO_ORIGIN: &str = include_str!("../tests/fixtures/meta-legacy-no-origin.json");

    /// W20-A: legacy records map to `Tool` (R18 fields) or to unknown
    /// provenance (no origin field) — never guessed from the name.
    #[test]
    fn test_legacy_meta_fixtures_resolve_to_tool_or_unknown() -> TestResult {
        let tool: JobMeta =
            serde_json::from_str(LEGACY_TOOL_ORIGIN).map_err(ctx("parse legacy tool"))?;
        assert_eq!(tool.origin, None);
        assert_eq!(
            tool.resolved_origin(),
            Some(ManagedJobOrigin::Tool {
                tool: "job.start".to_owned(),
                call_id: Some("call-7".to_owned()),
            })
        );

        let plain: JobMeta =
            serde_json::from_str(LEGACY_NO_ORIGIN).map_err(ctx("parse legacy plain"))?;
        assert_eq!(plain.name, "agent-child-explorer");
        assert_eq!(plain.origin, None);
        assert_eq!(plain.resolved_origin(), None);

        // Rewriting a legacy record keeps its shape: no `origin` key appears.
        let rewritten = serde_json::to_string(&plain).map_err(ctx("serialize plain"))?;
        assert!(!rewritten.contains("\"origin\""), "{rewritten}");
        Ok(())
    }

    #[test]
    fn test_origin_from_legacy_fields() {
        assert_eq!(ManagedJobOrigin::from_legacy(None, None), None);
        assert_eq!(
            ManagedJobOrigin::from_legacy(Some("job.start"), None),
            Some(ManagedJobOrigin::Tool {
                tool: "job.start".to_owned(),
                call_id: None,
            })
        );
        assert_eq!(
            ManagedJobOrigin::from_legacy(None, Some("call-1")),
            Some(ManagedJobOrigin::Tool {
                tool: String::new(),
                call_id: Some("call-1".to_owned()),
            })
        );
    }

    /// W20-A: every typed origin survives the `meta.json` round trip in its
    /// tagged form, and the typed field wins over the legacy fields.
    #[test]
    fn test_typed_origin_roundtrips_and_wins_over_legacy() -> TestResult {
        let base: JobMeta = serde_json::from_str(&v1_json("")).map_err(ctx("parse v1"))?;
        let cases = [
            (
                ManagedJobOrigin::AgentChild {
                    child_session_id: "child-7".to_owned(),
                },
                r#""origin":{"kind":"agent_child","child_session_id":"child-7"}"#,
            ),
            (
                ManagedJobOrigin::Tool {
                    tool: "job.start".to_owned(),
                    call_id: Some("call-1".to_owned()),
                },
                r#""origin":{"kind":"tool","tool":"job.start","call_id":"call-1"}"#,
            ),
            (
                ManagedJobOrigin::Tool {
                    tool: "agents.build".to_owned(),
                    call_id: None,
                },
                r#""origin":{"kind":"tool","tool":"agents.build"}"#,
            ),
            (
                ManagedJobOrigin::Operator,
                r#""origin":{"kind":"operator"}"#,
            ),
        ];
        for (origin, expected) in cases {
            let mut meta = base.clone();
            meta.version = META_VERSION;
            meta.origin = Some(origin.clone());
            let json = serde_json::to_string(&meta).map_err(ctx("serialize"))?;
            assert!(json.contains(expected), "{json}");
            let back: JobMeta = serde_json::from_str(&json).map_err(ctx("parse back"))?;
            assert_eq!(back, meta);
            assert_eq!(back.resolved_origin(), Some(origin.clone()));

            // Legacy fields of a mixed record never override the typed one.
            meta.origin_tool = Some("shell.exec".to_owned());
            meta.origin_call_id = Some("call-legacy".to_owned());
            assert_eq!(meta.resolved_origin(), Some(origin));
        }
        Ok(())
    }

    /// An origin kind of a newer harw (or a malformed one) reads as unknown
    /// provenance; the rest of the record still loads.
    #[test]
    fn test_unknown_origin_kind_reads_as_unknown() -> TestResult {
        for origin in [
            r#""origin":{"kind":"schedule","every":"1h"},"#,
            r#""origin":{"kind":"agent_child"},"#,
            r#""origin":"tool","#,
            r#""origin":null,"#,
        ] {
            let meta: JobMeta = serde_json::from_str(&v1_json(origin)).map_err(ctx("parse"))?;
            assert_eq!(meta.origin, None, "{origin}");
            assert_eq!(meta.resolved_origin(), None, "{origin}");
            assert_eq!(meta.state, JobState::Running);
        }
        Ok(())
    }

    /// W20-A/E4: the persisted cgroup path of the process identity is
    /// surfaced through [`JobStatus::cgroup_path`]; v1 records have none.
    #[test]
    fn test_status_surfaces_persisted_cgroup_path() -> TestResult {
        let status_of = |raw: &str| -> TestResult<JobStatus> {
            Ok(JobStatus {
                meta: serde_json::from_str(raw).map_err(ctx("parse fixture"))?,
                runtime_secs: None,
                stdout_lines: 0,
                stderr_lines: 0,
                last_lines: Vec::new(),
                log_dir: PathBuf::new(),
            })
        };
        assert_eq!(
            status_of(LEGACY_TOOL_ORIGIN)?.cgroup_path(),
            Some("/harw.slice/harw-job-7")
        );
        assert_eq!(status_of(LEGACY_NO_ORIGIN)?.cgroup_path(), None);
        // A legacy record carries no dedicated-cgroup proof: never attributed.
        assert_eq!(status_of(LEGACY_TOOL_ORIGIN)?.job_cgroup_path(), None);
        assert_eq!(status_of(LEGACY_NO_ORIGIN)?.job_cgroup_path(), None);
        Ok(())
    }

    /// A status whose identity has cgroup `path` and the optional `proof`.
    fn status_with_cgroup(
        path: &str,
        proof: Option<DedicatedCgroupProof>,
    ) -> TestResult<JobStatus> {
        let mut meta: JobMeta = serde_json::from_str(&v1_json("")).map_err(ctx("parse v1"))?;
        meta.version = META_VERSION;
        meta.identity = Some(JobProcessIdentity {
            start_ticks: 1,
            executable: None,
            cgroup_path: Some(path.to_owned()),
            dedicated_cgroup: proof,
        });
        Ok(JobStatus {
            meta,
            runtime_secs: None,
            stdout_lines: 0,
            stderr_lines: 0,
            last_lines: Vec::new(),
            log_dir: PathBuf::new(),
        })
    }

    /// A proof for `job-1` (the job id of [`v1_json`]).
    fn proof(attempt_id: &str, delegated_root: &str) -> DedicatedCgroupProof {
        DedicatedCgroupProof {
            job_id: Some(JobId("job-1".to_owned())),
            attempt_id: attempt_id.to_owned(),
            delegated_root: delegated_root.to_owned(),
        }
    }

    /// PR #154 review (E4): a `harw-job-*` leaf is no proof. Without a
    /// recorded runtime proof (harw itself in a shared cgroup whose leaf
    /// matches, which every job would then persist), nothing is attributed;
    /// the raw path stays visible through [`JobStatus::cgroup_path`].
    #[test]
    fn test_shared_matching_leaf_without_proof_is_not_a_job_cgroup() -> TestResult {
        for path in [
            "/user.slice",
            "/user.slice/user-1000.slice/session-2.scope",
            "/harw.slice",
            "/harw.slice/harw-job-1",
            "/user.slice/harw-job-shared",
            "/delegated/harw-job-job_1.x-e2",
        ] {
            let status = status_with_cgroup(path, None)?;
            assert_eq!(status.cgroup_path(), Some(path), "{path:?}");
            assert_eq!(status.job_cgroup_path(), None, "{path:?}");
        }
        Ok(())
    }

    /// PR #154 review (E4): a proof only counts for the exact cgroup it
    /// attests — a leaf for another attempt, a root that is not the direct
    /// parent, or a malformed proof fails closed.
    #[test]
    fn test_forged_or_mismatched_proof_is_not_a_job_cgroup() -> TestResult {
        let cases = [
            // Leaf of another attempt.
            (
                "/harw.slice/harw-job-other-e1",
                proof("job-1-e1", "/harw.slice"),
            ),
            // Prefix of the attested attempt is still another attempt.
            (
                "/harw.slice/harw-job-job-1-e1",
                proof("job-1-e10", "/harw.slice"),
            ),
            // Root is not the direct parent.
            ("/harw.slice/x/harw-job-a", proof("a", "/harw.slice")),
            ("/harw.slice/harw-job-a", proof("a", "/other.slice")),
            (
                "/harw.slice/harw-job-a",
                proof("a", "/harw.slice/harw-job-a"),
            ),
            // The hierarchy root is never a delegated root.
            ("/harw-job-a", proof("a", "/")),
            ("/harw-job-a", proof("a", CGROUP_MOUNT)),
            // Malformed proofs or paths.
            ("/harw.slice/harw-job-", proof("", "/harw.slice")),
            ("/harw.slice/harw-job-a", proof("a", "harw.slice")),
            ("/harw.slice/../harw-job-a", proof("a", "/harw.slice/..")),
            ("/harw.slice//harw-job-a", proof("a", "/harw.slice/")),
            ("/harw.slice/harw-job-a/leaf", proof("a", "/harw.slice")),
        ];
        for (path, proof) in cases {
            let status = status_with_cgroup(path, Some(proof.clone()))?;
            assert_eq!(status.job_cgroup_path(), None, "{path:?} {proof:?}");
        }
        Ok(())
    }

    /// PR #154 review (E4): the genuine case — the leaf is
    /// `harw-job-<attested attempt>` directly beneath the attested delegated
    /// root (mount-point spelling tolerated) — is the job's cgroup, and the
    /// proof survives a `meta.json` round trip.
    #[test]
    fn test_proven_dedicated_cgroup_is_the_job_cgroup() -> TestResult {
        let cases = [
            (
                "/harw.slice/harw-job-job-1-e1",
                proof("job-1-e1", "/harw.slice"),
            ),
            (
                "/app.slice/harw.slice/harw-job-job_1.x-e2",
                proof("job:1.x-e2", "/sys/fs/cgroup/app.slice/harw.slice"),
            ),
        ];
        for (path, proof) in cases {
            let status = status_with_cgroup(path, Some(proof))?;
            assert_eq!(status.job_cgroup_path(), Some(path), "{path:?}");
            let json = serde_json::to_string(&status.meta).map_err(ctx("serialize"))?;
            let back: JobMeta = serde_json::from_str(&json).map_err(ctx("parse back"))?;
            assert_eq!(back, status.meta);
        }
        Ok(())
    }

    /// PR #154 review (E4): a proof is bound to its job. The same attested
    /// proof copied into another job's record (two jobs sharing one cgroup
    /// path and proof) is not attributed to that other job.
    #[test]
    fn test_proof_shared_by_two_jobs_only_counts_for_its_own_job() -> TestResult {
        let path = "/harw.slice/harw-job-job-1-e1";
        let shared = proof("job-1-e1", "/harw.slice");
        let own = status_with_cgroup(path, Some(shared.clone()))?;
        assert_eq!(own.job_cgroup_path(), Some(path));
        let mut other = status_with_cgroup(path, Some(shared))?;
        other.meta.job_id = JobId("job-2".to_owned());
        assert_eq!(other.cgroup_path(), Some(path));
        assert_eq!(other.job_cgroup_path(), None);
        // An unbound proof (no job id) still loads but never attributes.
        let raw = v1_json(
            r#""identity":{"start_ticks":5,"cgroup_path":"/harw.slice/harw-job-a",
                "dedicated_cgroup":{"attempt_id":"a","delegated_root":"/harw.slice"}},"#,
        );
        let meta: JobMeta = serde_json::from_str(&raw).map_err(ctx("parse unbound"))?;
        let unbound = JobStatus { meta, ..own };
        assert_eq!(unbound.cgroup_path(), Some("/harw.slice/harw-job-a"));
        assert_eq!(unbound.job_cgroup_path(), None);
        Ok(())
    }

    /// Control bytes in `meta.json` arrive as JSON escapes; they decode to
    /// the real bytes (consumers sanitize them); without a proof the path is
    /// never the job's cgroup.
    #[test]
    fn test_escaped_control_bytes_decode_in_origin_and_cgroup() -> TestResult {
        let raw = v1_json(
            r#""origin":{"kind":"tool","tool":"job.start\u001b[2J"},
               "identity":{"start_ticks":5,"cgroup_path":"/harw\u0007.slice/harw-job-3"},"#,
        );
        let meta: JobMeta = serde_json::from_str(&raw).map_err(ctx("parse escaped"))?;
        assert_eq!(
            meta.resolved_origin(),
            Some(ManagedJobOrigin::Tool {
                tool: "job.start\u{1b}[2J".to_owned(),
                call_id: None,
            })
        );
        let status = JobStatus {
            meta,
            runtime_secs: None,
            stdout_lines: 0,
            stderr_lines: 0,
            last_lines: Vec::new(),
            log_dir: PathBuf::new(),
        };
        assert_eq!(status.cgroup_path(), Some("/harw\u{7}.slice/harw-job-3"));
        assert_eq!(status.job_cgroup_path(), None);
        Ok(())
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
                dedicated_cgroup: None,
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
            dedicated_cgroup: None,
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
