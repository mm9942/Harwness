//! [`JobManager`] — Prozess-Jobs einer harw-Sitzung.
//!
//! # Verantwortungsbereich
//! - Start eines bereits geprüften Prozesses ([`crate::PreparedJob`]) mit
//!   stdout/stderr direkt in `<state>/jobs/<job_id>/{stdout,stderr}.log`
//!   (Dateien, keine Pipes: ein abgelöster Job überlebt das Ende von harw,
//!   ohne an einer vollen Pipe zu hängen).
//! - Überwachung je Job in einem Tokio-Task: Logs mitlesen, Fortschritt und
//!   Fehler erkennen ([`crate::progress`]), gedrosselt melden
//!   ([`crate::throttle`], [`crate::JobNotifier`]), Ende einsammeln.
//! - `meta.json` bei Start, Fortschritt (höchstens alle
//!   [`META_PERSIST_INTERVAL`]), Stop und Ende; beim Start einer neuen
//!   Sitzung werden frühere Jobs geladen ([`JobState::Detached`], wenn der
//!   Prozess noch lebt **und** seine Identität — Startzeit, Programm,
//!   cgroup (`meta.json` v2) — passt, sonst [`JobState::Unknown`]; bei einer
//!   wiederverwendeten PID zusätzlich eine Warnung).
//! - Stop: Signal an die ganze Prozessgruppe, nach der Gnadenfrist SIGKILL.
//!   Eigene Kinder werden über einen direkt nach dem Start geöffneten pidfd
//!   gesteuert ([`crate::procfs`]); Jobs früherer Sitzungen nur nach
//!   bewiesener Identität (Job-Runtime-Doc §15, §25) — eine PID allein
//!   löst nie ein Signal aus.
//! - Ende ohne Stop: leben danach noch Prozesse der Job-Gruppe, werden sie
//!   beendet (SIGTERM, nach der Gnadenfrist SIGKILL über denselben
//!   pidfd-/PID-Wiederverwendungs-Schutz); `meta.json` vermerkt das
//!   (`stragglers_reaped`), der Besitzer bekommt eine Warnung.
//! - Log-Budget: `stdout.log`/`stderr.log` werden mit `O_APPEND` angelegt
//!   und je Abfrageintervall sowie einmal nach dem Prozessende auf
//!   [`JobManagerConfig::max_log_bytes`] plus eine Markerzeile gekürzt
//!   (`log_truncated`, Warnung an den Besitzer); der Job läuft weiter.
//! - Hinweise des Startwegs ([`JobManager::start_with_warnings`], z. B. Start
//!   ohne rlimits) landen in `meta.json` (`launch_warnings`) und gehen als
//!   Warnung an den Besitzer.
//! - Besitz: nur Erzeuger und Vorfahren ([`JobOwner::may_control`]); die
//!   Oberfläche handelt als [`Caller::Operator`].
//! - Anhalten (PL-100 WP 3.5): [`JobManager::suspend`] friert die eigene,
//!   von der Runtime bewiesene Job-cgroup ein ([`crate::JobFreezer`],
//!   `cgroup.freeze`), [`JobManager::resume`] taut sie auf; dieselbe
//!   Besitzprüfung wie [`JobManager::stop`]. harw übernimmt und hebt nie
//!   einen Freeze, den es nicht selbst vermerkt hat
//!   ([`JobError::FrozenElsewhere`]), auch keine Eindämmung des Wardens,
//!   die nach harws Anhalten auf dieselbe cgroup kam
//!   ([`JobError::ContainmentHeld`]). Eingefroren wird erst, wenn die
//!   Absicht dauerhaft in `meta.json` steht ([`SuspendPhase::Intent`]): die
//!   Datei und danach ihr Eintrag im Job-Verzeichnis per `fsync` gesichert
//!   ([`JobError::SuspendNotRecorded`]); nach dem Einfrieren wird ebenso
//!   dauerhaft die Wirkung vermerkt ([`SuspendPhase::Applied`],
//!   [`JobError::SuspendAppliedNotRecorded`]). Die cgroup wird vor dem
//!   Vermerk und noch einmal direkt vor dem Einfrieren geprüft: ein Freeze
//!   außerhalb von harw, der während der `fsync`s kam, wird so nie zu
//!   harws eigenem (offen bleibt nur das Fenster von Mikrosekunden bis zum
//!   Schreiben). Nur ein Anhalten in der Phase `applied` ist harws eigenes:
//!   nur das taut `job.resume` auf, auch nach einem Neustart. Eines, von dem nur die Absicht vermerkt ist, bleibt
//!   ungeklärt ([`JobError::SuspensionUnresolved`]): `job.resume` schreibt
//!   dann nie `cgroup.freeze`, ein erneutes `job.suspend` friert nur eine
//!   nicht eingefrorene cgroup selbst ein (und klärt es so). Ein Stop taut
//!   einen angehaltenen Job nie auf: er endet eingefroren mit SIGKILL.
//!   Haltezeit zählt nicht als aktive Laufzeit
//!   ([`JobMeta::active_secs_at`], Wanduhr-Semantik in [`crate::model`]).
//! - Neuladen: ob ein Job einer früheren Sitzung noch lebt, prüft
//!   `recovered_liveness` über `/proc` (nur Tests ersetzen die Prüfung,
//!   `JobManager::with_liveness`).
//!
//! # Nebenläufigkeit
//! `Send + Sync`; interne `std::sync::Mutex` werden nie über ein `await`
//! gehalten, der [`JobNotifier`] wird ohne gehaltene Sperre aufgerufen.
//! Warten (`stop`, `wait`) läuft über einen `watch`-Kanal je Job.

use crate::event::{JobEvent, JobNotification, JobNotifier};
use crate::freeze::{FreezeError, JobFreezer, leaf_beneath};
use crate::launcher::{PreparedJob, PreparedLaunch};
use crate::logs::{LogFollower, clip_line, tail_of_file};
use crate::model::{
    DedicatedCgroupProof, JobId, JobMeta, JobOwner, JobProcessIdentity, JobState, JobStatus,
    META_FILE, META_VERSION, ManagedJobOrigin, STDERR_LOG, STDOUT_LOG, SuspendPhase,
};
use crate::procfs::{JobSignal, Liveness, OwnLeader, Recovered, SignalError};
use crate::progress::{
    ProgressSnapshot, ProgressSource, ProgressTracker, Severity, detect_severity,
};
use crate::throttle::{NotifyThrottle, ProgressKey, ThrottleConfig};
use jiff::Timestamp;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Wie oft `meta.json` während des Laufs höchstens neu geschrieben wird.
pub const META_PERSIST_INTERVAL: Duration = Duration::from_secs(10);
/// Zeilen im Speicher-Ringpuffer je Job.
const TAIL_CAPACITY: usize = 200;
/// Zeilen in [`JobStatus::last_lines`].
const STATUS_TAIL_LINES: usize = 8;
/// Höchstlänge einer Zeile in Ringpuffer/Meldungen.
const EVENT_LINE_CHARS: usize = 400;
/// Wartezeit nach SIGKILL, bis der Job als beendet gesehen wird.
const KILL_WAIT: Duration = Duration::from_secs(3);
/// Abfrageintervall für Prozesse ohne eigenen Überwachungs-Task.
const FOREIGN_POLL: Duration = Duration::from_millis(200);
/// Wie oft die Überwachung die Identität des Gruppenführers neu liest (ein
/// `exec` ändert das Programm; `meta.json` soll die letzte Beobachtung
/// tragen, damit ein Neustart den Job wiedererkennt).
const IDENTITY_REFRESH: Duration = Duration::from_secs(2);
/// Vorgabe-Höchstlänge einer stdout-Zeile eines [`JobManager::start_piped`]-Jobs
/// in Bytes (ohne `\n`): 1 MiB, gleich `harw-agent-runner`s
/// `child_protocol::MAX_FRAME_BYTES`.
pub const DEFAULT_MAX_PIPED_LINE_BYTES: usize = 1024 * 1024;
/// Vorgabe für [`JobManagerConfig::max_log_bytes`]: 64 MiB je Logdatei.
pub const DEFAULT_MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;

/// Einstellungen der Job-Verwaltung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobManagerConfig {
    /// Zustandsverzeichnis; Jobs liegen unter `<state_dir>/jobs/<job_id>/`.
    pub state_dir: PathBuf,
    /// Gnadenfrist zwischen SIGTERM (bzw. INT/HUP) und SIGKILL.
    pub stop_grace: Duration,
    /// Abfrageintervall der Überwachung (Logs mitlesen, Fälligkeiten).
    pub poll_interval: Duration,
    /// Vorgabe für `notify_every_secs`.
    pub default_notify_every: Duration,
    /// Untergrenze für `notify_every_secs` (außer `0` = aus).
    pub min_notify_every: Duration,
    /// Obergrenze für `notify_every_secs`.
    pub max_notify_every: Duration,
    /// Sammelfenster für Fehlerzeilen.
    pub error_debounce: Duration,
    /// Mindestabstand zweier Fehlermeldungen.
    pub error_min_interval: Duration,
    /// Höchstzahl Fehlerzeilen je Meldung.
    pub max_error_lines: usize,
    /// Zeilen im Ende-Bericht ([`JobEvent::Finished`]).
    pub finish_tail_lines: usize,
    /// Höchstzahl gleichzeitig laufender Jobs dieser Sitzung. Die Montage
    /// setzt ihn aus `[jobs] max_running` (1..=256, Vorgabe 16).
    pub max_running_jobs: usize,
    /// Wanduhr-Budget, aus dem die CPU-rlimit eines Jobs wächst.
    pub cpu_budget_secs: u64,
    /// Byte-Budget je Logdatei (`stdout.log`, `stderr.log`); darüber kürzt
    /// die Überwachung mit einer Markerzeile. Vorgabe 64 MiB.
    pub max_log_bytes: u64,
}

impl JobManagerConfig {
    /// Vorgaben: Gnadenfrist 5 s, Abfrage 250 ms, Meldung alle 60 s
    /// (erlaubt 10 s – 1 h), Fehler nach 2 s gesammelt und höchstens alle
    /// 20 s, 20 Zeilen im Ende-Bericht, 16 laufende Jobs, CPU-Budget 24 h,
    /// 64 MiB Log-Budget je Datei.
    #[must_use]
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            stop_grace: Duration::from_secs(5),
            poll_interval: Duration::from_millis(250),
            default_notify_every: Duration::from_secs(60),
            min_notify_every: Duration::from_secs(10),
            max_notify_every: Duration::from_secs(3600),
            error_debounce: Duration::from_secs(2),
            error_min_interval: Duration::from_secs(20),
            max_error_lines: 8,
            finish_tail_lines: 20,
            max_running_jobs: 16,
            cpu_budget_secs: 24 * 3600,
            max_log_bytes: DEFAULT_MAX_LOG_BYTES,
        }
    }

    /// Das Verzeichnis aller Jobs (`<state_dir>/jobs`).
    #[must_use]
    pub fn jobs_dir(&self) -> PathBuf {
        self.state_dir.join("jobs")
    }

    /// Wirksamer Meldeabstand für einen angefragten Wert in Sekunden.
    #[must_use]
    pub fn effective_notify_every(&self, requested_secs: Option<u64>) -> Duration {
        match requested_secs {
            None => self.default_notify_every,
            Some(0) => Duration::ZERO,
            Some(secs) => {
                Duration::from_secs(secs).clamp(self.min_notify_every, self.max_notify_every)
            }
        }
    }
}

/// Wer eine Operation auslöst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller<'a> {
    /// Ein Agent (Sitzungskennung aus dem Ausführungskontext).
    Agent(&'a str),
    /// Die Nutzerin über die Oberfläche (`/jobs`): sieht und steuert alles.
    Operator,
}

impl Caller<'_> {
    fn may_control(self, owner: &JobOwner) -> bool {
        match self {
            Self::Agent(session) => owner.may_control(session),
            Self::Operator => true,
        }
    }
}

/// Anfrage zum Start eines Jobs (Werte bereits geprüft).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    /// Anzeigename.
    pub name: String,
    /// Befehl, wie der Agent ihn angegeben hat (Anzeige/`meta.json`).
    pub command: String,
    /// Arbeitsverzeichnis (absolut), falls angegeben.
    pub cwd: Option<PathBuf>,
    /// Namen gesetzter Umgebungsvariablen.
    pub env_keys: Vec<String>,
    /// Meldeabstand (`0` = keine periodischen Meldungen).
    pub notify_every: Duration,
    /// Besitzer.
    pub owner: JobOwner,
}

/// R18 (P5/P6): Herkunft eines Jobs — der Werkzeugaufruf, der ihn
/// gestartet hat. Die TUI verknüpft darüber einen Job mit seiner
/// Werkzeugzelle; `meta.json` und [`JobEvent::Started`] tragen sie.
///
/// Ein eigener Typ statt neuer Felder in [`StartRequest`], damit die
/// bestehenden Aufrufer unverändert bleiben; wer [`JobManager::start`]
/// nutzt, startet ohne Herkunft (alle Felder `None`).
///
/// Which origin to pass (W20-A): a model tool sets `tool` and `call_id`
/// (e.g. `job.start`, `agents.build`), an operator command sets
/// `kind: Some(ManagedJobOrigin::Operator)`, the job child backend uses
/// [`JobOrigin::agent_child`]. A job started without any of them has
/// unknown provenance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobOrigin {
    /// Id des startenden Werkzeugaufrufs (`ToolCall::id`).
    pub call_id: Option<String>,
    /// Name des startenden Werkzeugs (z. B. `job.start`).
    pub tool: Option<String>,
    /// Anzeigename des besitzenden Agenten, falls bekannt.
    pub owner_agent: Option<String>,
    /// W20-A: explicit typed provenance for starts that are not tool calls
    /// (child agents, operator commands). `None` derives it from `tool` /
    /// `call_id` (see [`JobOrigin::managed`]).
    pub kind: Option<ManagedJobOrigin>,
}

impl JobOrigin {
    /// Origin of a job that backs the child agent run `child_session_id`
    /// (W20-A; `harw-agent-runner`'s job child backend).
    #[must_use]
    pub fn agent_child(child_session_id: impl Into<String>) -> Self {
        Self {
            kind: Some(ManagedJobOrigin::AgentChild {
                child_session_id: child_session_id.into(),
            }),
            ..Self::default()
        }
    }

    /// The typed provenance this origin records ([`JobMeta::origin`]):
    /// [`JobOrigin::kind`] if set, else `tool` / `call_id` mapped by
    /// [`ManagedJobOrigin::from_legacy`]; `None` when nothing is known.
    #[must_use]
    pub fn managed(&self) -> Option<ManagedJobOrigin> {
        self.kind.clone().or_else(|| {
            ManagedJobOrigin::from_legacy(self.tool.as_deref(), self.call_id.as_deref())
        })
    }
}

/// Ergebnis von [`JobManager::start_piped`]: derselbe Job wie [`JobManager::start`]
/// (Prozessgruppe, Logs, `meta.json`, Überwachung, Ereignisse), zusätzlich
/// mit offener stdin und einem Zeilenstrom der stdout — für einen Job, dessen
/// Prozess über sein eigenes Protokoll auf seiner stdio spricht (ein
/// job-gebundenes Kind, `harw-agent-runner::job_child_backend`).
///
/// # Description
/// stdout wird zusätzlich vollständig in `STDOUT_LOG` mitgeschrieben
/// (`job.logs`/`job.status` sehen sie wie bei jedem anderen Job); stderr
/// geht wie bisher direkt in `STDERR_LOG`. Der Überwachungs-Task erkennt
/// weiterhin Ende, Prozessgruppe und Ereignisse.
#[derive(Debug)]
pub struct PipedJob {
    /// Kennung.
    pub job_id: JobId,
    /// Zustand direkt nach dem Start.
    pub status: JobStatus,
    /// stdin des Kindes; der Aufrufer schreibt sein Protokoll hinein.
    pub stdin: ChildStdin,
    /// Zeilen der stdout, in Ankunftsreihenfolge (dieselben Zeilen landen
    /// auch in `STDOUT_LOG`). Endet (liefert `None`), wenn der Prozess seine
    /// stdout schließt. Eine zu lange oder nicht-UTF-8-Zeile liefert genau
    /// ein `Err` ([`PipedLineError`]); danach endet der Strom, und die
    /// stdout-Pipe ist geschlossen (der Prozess sieht beim nächsten
    /// Schreiben `EPIPE`/`SIGPIPE`).
    pub stdout_lines: mpsc::UnboundedReceiver<Result<String, PipedLineError>>,
}

/// Warum der stdout-Zeilenstrom eines [`PipedJob`] vorzeitig endete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, harw_macros::HarwError)]
pub enum PipedLineError {
    /// Eine Zeile wurde länger als `limit` Bytes (ohne `\n`), bevor ihr `\n`
    /// kam; gelesen wurde höchstens `limit` Bytes davon.
    #[msg("job stdout line exceeds the {limit}-byte limit")]
    TooLong {
        /// Die überschrittene Grenze.
        limit: usize,
    },
    /// Eine Zeile war kein gültiges UTF-8.
    #[msg("job stdout line is not valid UTF-8")]
    InvalidUtf8,
}

/// Ergebnis von [`JobManager::detach_all`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DetachSummary {
    /// Abgelöste Jobs.
    pub detached: usize,
    /// Davon Sandbox-Jobs, die trotzdem mit harw enden (`--die-with-parent`).
    pub ends_with_harw: usize,
}

/// Fehler der Job-Verwaltung.
#[derive(Debug, harw_macros::HarwError)]
pub enum JobError {
    /// Unbekannte Kennung **oder** fremder Job (bewusst nicht unterschieden).
    #[msg("unknown job `{0}` (or not started by you or one of your sub-agents)")]
    NotFound(String),
    /// Zu viele laufende Jobs; die Meldung nennt den Schlüssel
    /// `[jobs] max_running`, der die Obergrenze setzt.
    #[msg(
        "too many running jobs (max {max}, set by `[jobs] max_running`); wait for one to finish or stop one"
    )]
    Capacity {
        /// Obergrenze ([`JobManagerConfig::max_running_jobs`]).
        max: usize,
    },
    /// Der Prozess konnte nicht gestartet werden.
    #[msg("failed to start the job: {0}")]
    Spawn(String),
    /// Dateisystemfehler.
    #[msg("{context}: {source}")]
    Io {
        /// Was gerade geschah.
        context: &'static str,
        /// Ursache.
        #[source]
        source: io::Error,
    },
    /// `job.suspend`: der Job läuft nicht unter Aufsicht dieser Sitzung
    /// (wartet, abgelöst oder schon beendet) und kann nicht angehalten
    /// werden.
    #[msg(
        "job `{job}` cannot be suspended in state {state}; only running jobs of this session can"
    )]
    NotSuspendable {
        /// Kennung des Jobs.
        job: String,
        /// Sein Zustand zum Zeitpunkt der Prüfung.
        state: JobState,
    },
    /// `job.suspend`/`job.resume`: keine von der Runtime bewiesene eigene
    /// cgroup des Jobs direkt unter der delegierten Wurzel des Freezers;
    /// es wird nichts eingefroren oder aufgetaut (fail closed).
    #[msg(
        "job `{0}` has no own cgroup beneath the delegated cgroup root; it cannot be suspended or resumed"
    )]
    NoJobCgroup(String),
    /// `job.suspend`/`job.resume`: kein Freezer installiert (keine
    /// delegierte cgroup-v2-Wurzel für Jobs, `[jobs] cgroup_root`, oder kein
    /// Linux).
    #[msg("job suspension is not available: jobs have no delegated cgroup v2 root here")]
    SuspendUnavailable,
    /// `job.suspend`/`job.resume`: die Job-cgroup ist eingefroren (oder
    /// friert gerade ein), ohne dass harw das vermerkt hat — ein Operator,
    /// der Warden oder eine eingefrorene Vorfahren-cgroup; bei
    /// `job.suspend` auch, wenn der Eindämmungsindex sie schon als vom
    /// Warden gehalten zeigt. harw übernimmt diesen Freeze nicht und hebt
    /// ihn nie auf (A3/O3: gefroren bleibt gefroren bis zur Aufhebung durch
    /// seinen Urheber).
    #[msg(
        "job `{0}` is frozen by someone other than harw (an operator, the Warden or a parent cgroup); harw neither takes over nor lifts that freeze"
    )]
    FrozenElsewhere(String),
    /// `job.resume`: harw hat den Job angehalten, aber ein Eigentümer
    /// außerhalb von harw (der Warden) hält dieselbe cgroup inzwischen
    /// eingedämmt (Marker im Eindämmungsindex,
    /// [`crate::JobFreezer::containment_held`]). cgroup v2 hat nur ein
    /// Freeze-Bit je cgroup; harws Auftauen höbe diese Eindämmung mit auf.
    /// Nichts wird aufgetaut, der Job bleibt angehalten.
    #[msg(
        "job `{0}` is held by a containment outside harw (the Warden); job.resume refused, the job stays suspended"
    )]
    ContainmentHeld(String),
    /// `job.suspend`: das Anhalten ließ sich nicht dauerhaft in `meta.json`
    /// vermerken (Absicht vor Wirkung): Schreiben, `fsync` oder `rename` der
    /// Datei scheiterte, oder danach der `fsync` des Job-Verzeichnisses, ohne
    /// den die Umbenennung einen Stromausfall nicht sicher übersteht. Es
    /// wurde nichts eingefroren, der Job läuft weiter.
    #[msg(
        "job `{job}` was not suspended: the suspension could not be recorded ({source}); nothing was frozen"
    )]
    SuspendNotRecorded {
        /// Kennung des Jobs.
        job: String,
        /// Ursache des Schreib- bzw. Sync-Fehlers.
        #[source]
        source: io::Error,
    },
    /// `job.suspend`: der Vermerk stand schon unter `meta.json` (die
    /// Umbenennung war erfolgt), aber der `fsync` des Job-Verzeichnisses
    /// scheiterte, und auch die Rücknahme des Vermerks ließ sich nicht
    /// schreiben. Es wurde nichts eingefroren, im Speicher läuft der Job
    /// weiter; `meta.json` sagt bis zum nächsten erfolgreichen
    /// Schreibzugriff „angehalten“, aber nur als Absicht
    /// ([`SuspendPhase::Intent`]). In dieser Sitzung zählt der Speicher:
    /// `job.resume` taut nichts auf ([`SuspendTransition::NotSuspended`],
    /// bei einem Freeze außerhalb von harw [`JobError::FrozenElsewhere`]).
    /// Startet harw neu, bevor ein Schreibzugriff die Datei berichtigt, lädt
    /// es den Job mit lebendem Prozess als ungeklärt angehalten
    /// ([`JobState::Detached`] mit der Absicht, Hinweis an den Besitzer):
    /// `job.resume` lehnt dann ab ([`JobError::SuspensionUnresolved`]) und
    /// schreibt nie `cgroup.freeze`, auch nicht, wenn inzwischen jemand
    /// anderes (etwa ein Operator ohne Marker im Eindämmungsindex) die
    /// cgroup eingefroren hat (A3/O3). Geklärt wird es von Hand (siehe
    /// [`JobError::SuspensionUnresolved`]).
    #[msg(
        "job `{job}` was not suspended: the suspension could not be recorded durably ({source}) and its record could not be taken back ({persist}); nothing was frozen"
    )]
    SuspendRollbackNotRecorded {
        /// Kennung des Jobs.
        job: String,
        /// Warum der `fsync` des Job-Verzeichnisses scheiterte.
        #[source]
        source: io::Error,
        /// Warum die Rücknahme nicht geschrieben wurde.
        persist: io::Error,
    },
    /// `job.suspend`: das Einfrieren schlug fehl, und auch die Rücknahme
    /// des schon geschriebenen Vermerks ließ sich nicht speichern.
    /// `meta.json` sagt deshalb weiter „angehalten“, obwohl nichts
    /// eingefroren ist, aber nur als Absicht ([`SuspendPhase::Intent`]); im
    /// Speicher läuft der Job weiter, der nächste erfolgreiche
    /// Schreibzugriff berichtigt die Datei. Ein Neustart dazwischen lädt
    /// den Job als ungeklärt angehalten: `job.resume` lehnt ab
    /// ([`JobError::SuspensionUnresolved`]) und taut nie auf.
    #[msg(
        "freeze of job `{job}` failed: {source}; the suspension record could not be taken back ({persist})"
    )]
    RollbackNotRecorded {
        /// Kennung des Jobs.
        job: String,
        /// Warum das Einfrieren scheiterte.
        #[source]
        source: FreezeError,
        /// Warum die Rücknahme nicht geschrieben wurde.
        persist: io::Error,
    },
    /// `job.suspend`: die erneute Prüfung direkt vor dem Einfrieren (nach dem
    /// dauerhaften Vermerk der Absicht) lehnte ab — die cgroup war inzwischen
    /// außerhalb von harw eingefroren, der Eindämmungsindex hielt sie, oder
    /// ihr Zustand war nicht lesbar (`refusal`: [`JobError::FrozenElsewhere`]
    /// bzw. [`JobError::Freeze`] mit `probe`) —, und die Rücknahme des
    /// Vermerks dieses frischen Anhaltens ließ sich nicht schreiben. harw hat
    /// nichts eingefroren; im Speicher läuft der Job weiter, `meta.json` sagt
    /// bis zum nächsten erfolgreichen Schreibzugriff „angehalten“, aber nur
    /// als Absicht ([`SuspendPhase::Intent`]). Ein Neustart dazwischen lädt
    /// den Job als ungeklärt angehalten: `job.resume` lehnt ab
    /// ([`JobError::SuspensionUnresolved`]) und hebt den Freeze außerhalb von
    /// harw nie auf.
    #[msg(
        "job `{job}` was not suspended and harw froze nothing: {refusal}; the suspension record could not be taken back ({persist})"
    )]
    SuspendRefusedRollbackNotRecorded {
        /// Kennung des Jobs.
        job: String,
        /// Die Ablehnung der erneuten Prüfung.
        #[source]
        refusal: Box<JobError>,
        /// Warum die Rücknahme nicht geschrieben wurde.
        persist: io::Error,
    },
    /// `job.suspend`: der Job ist eingefroren, aber dass der Freeze harws
    /// eigener ist ([`SuspendPhase::Applied`]), ließ sich danach nicht
    /// dauerhaft vermerken (Schreiben, `fsync`, `rename` oder der `fsync`
    /// des Job-Verzeichnisses scheiterte). Es wird **nicht** aufgetaut: der
    /// Job bleibt eingefroren, im Speicher und (nach bestem Bemühen) in
    /// `meta.json` steht nur die Absicht ([`SuspendPhase::Intent`]). Das
    /// Anhalten ist damit ungeklärt: `job.resume` lehnt ab
    /// ([`JobError::SuspensionUnresolved`]), in dieser Sitzung wie nach
    /// einem Neustart, bis es von Hand geklärt ist.
    #[msg(
        "job `{job}` is frozen, but harw could not record that the freeze is its own ({source}); its suspension stays unresolved: job.resume is refused, job.stop kills the job without thawing it"
    )]
    SuspendAppliedNotRecorded {
        /// Kennung des Jobs.
        job: String,
        /// Warum der Vermerk der Wirkung nicht dauerhaft geschrieben wurde.
        #[source]
        source: io::Error,
    },
    /// `job.suspend`: wie [`JobError::SuspendAppliedNotRecorded`] (der Job
    /// ist eingefroren, es wird nicht aufgetaut, im Speicher gilt die
    /// Absicht, `job.resume` lehnt ab), aber auch die Rücknahme des
    /// Vermerks auf die Absicht ließ sich nicht schreiben. Bis zum nächsten
    /// erfolgreichen Schreibzugriff kann `meta.json` deshalb
    /// [`SuspendPhase::Applied`] sagen (die umbenannte Datei, deren
    /// Verzeichnis-Sync scheiterte, oder eine Schreibung der Überwachung):
    /// Speicher und Platte weichen ab, die Platte ist die freizügigere
    /// Seite (nach einem Neustart dürfte `job.resume` auftauen). Dass der
    /// Freeze harws eigener ist, stimmt in diesem Moment, aber nur so lange,
    /// wie niemand die cgroup von Hand auftaut: wer das Anhalten von Hand
    /// klärt (siehe [`JobError::SuspensionUnresolved`]) und dazu auftaut,
    /// gibt gleich danach ein neues `job.suspend` (sein dauerhafter Vermerk
    /// berichtigt die Datei) oder beendet den Job mit `job.stop`; sonst
    /// könnte nach einem Neustart das veraltete `applied` einen späteren
    /// Freeze außerhalb von harw auftauen lassen. Jeder andere erfolgreiche
    /// Schreibzugriff (etwa der Überwachung) berichtigt die Datei ebenso.
    #[msg(
        "job `{job}` is frozen, but harw could not record that the freeze is its own ({source}) and could not take its record back to the intent ({persist}); its suspension stays unresolved: job.resume is refused, job.stop kills the job without thawing it"
    )]
    SuspendAppliedRollbackNotRecorded {
        /// Kennung des Jobs.
        job: String,
        /// Warum der Vermerk der Wirkung nicht dauerhaft geschrieben wurde.
        #[source]
        source: io::Error,
        /// Warum die Rücknahme auf die Absicht nicht geschrieben wurde.
        persist: io::Error,
    },
    /// `job.resume` (und ein erneutes `job.suspend` auf eine eingefrorene
    /// cgroup): das Anhalten ist **ungeklärt**. `meta.json` vermerkt nur
    /// die Absicht ([`SuspendPhase::Intent`], auch ein Eintrag von vor der
    /// Phase), nicht, dass harw eingefroren hat: das Einfrieren scheiterte
    /// oder wurde nie ausgeführt und seine Rücknahme ließ sich nicht
    /// schreiben, oder harw hat eingefroren und konnte das nicht vermerken
    /// ([`JobError::SuspendAppliedNotRecorded`],
    /// [`JobError::SuspendAppliedRollbackNotRecorded`]). Ein Freeze der cgroup kann
    /// deshalb auch einem Operator gehören; harw taut nichts auf und
    /// schreibt `cgroup.freeze` nicht (A3/O3).
    ///
    /// # Description
    /// Klären von Hand (es gibt bewusst kein „trotzdem fortsetzen“):
    /// - `job.stop` beendet den Job eingefroren (SIGKILL, kein Auftauen);
    ///   endet er, schließt harw das Anhalten selbst.
    /// - Ist die cgroup nicht eingefroren (`cgroup.freeze` = 0, nichts friert
    ///   ein, kein Marker im Eindämmungsindex), friert ein erneutes
    ///   `job.suspend` sie selbst ein, vermerkt das dauerhaft und klärt so
    ///   das Anhalten ([`SuspendPhase::Applied`]); danach taut `job.resume`
    ///   wie gewohnt auf. Geprüft wird vor dem Vermerk der Absicht und noch
    ///   einmal direkt vor dem Schreiben von `cgroup.freeze` (nach dessen
    ///   `fsync`s); offen bleibt nur das Fenster von Mikrosekunden zwischen
    ///   dieser Prüfung und harws Schreiben (cgroup v2 kennt kein
    ///   Vergleichen-und-Setzen des Bits): ein Freeze ohne Marker genau darin
    ///   machte harws Schreiben wirkungslos und würde doch als harws eigener
    ///   vermerkt. Ein Freeze des Wardens bleibt auch dann geschützt (sein
    ///   Marker kommt vor seinem Freeze, `job.resume` fragt den Index vor dem
    ///   Auftauen). Ist sie eingefroren, lässt sich nicht belegen,
    ///   wessen Freeze das ist: `job.suspend` lehnt ebenso ab. Wer den Job
    ///   fortsetzen will, hebt den Freeze selbst auf (als sein Urheber, bzw.
    ///   nach Prüfung von Hand über `cgroup.freeze`), gibt dann ein neues
    ///   `job.suspend` und danach `job.resume`.
    #[msg(
        "job `{0}` has an unresolved suspension: only harw's intent to freeze it is recorded, not that harw's freeze took effect, so harw cannot prove a freeze of its cgroup is its own; nothing is thawed (job.stop kills it without thawing; job.suspend re-freezes it only while its cgroup is not frozen)"
    )]
    SuspensionUnresolved(String),
    /// `job.resume`: bevor aufgetaut wird, muss `meta.json` aufhören, den
    /// Freeze als harws eigenen zu führen (wieder nur die Absicht,
    /// [`SuspendPhase::Intent`]); sonst könnte nach dem Auftauen ein
    /// veralteter Vermerk [`SuspendPhase::Applied`] einen späteren Freeze
    /// außerhalb von harw nach einem Neustart als harws eigenen auftauen
    /// lassen. Dieser Vermerk ließ sich nicht schreiben: es wurde nichts
    /// aufgetaut, der Job bleibt angehalten (harws eigenes Anhalten).
    #[msg(
        "job `{job}` was not resumed: harw could not record that it releases its freeze ({source}); nothing was thawed, the job stays suspended"
    )]
    ResumeNotRecorded {
        /// Kennung des Jobs.
        job: String,
        /// Warum der Vermerk nicht geschrieben wurde.
        #[source]
        source: io::Error,
    },
    /// `job.suspend`: ein Stop des Jobs läuft schon (Gnadenfrist); ihn jetzt
    /// einzufrieren hielte seinen Signal-Handler an.
    #[msg("job `{0}` is being stopped; it cannot be suspended")]
    StopPending(String),
    /// Einfrieren, Auftauen oder Prüfen der Job-cgroup schlug fehl.
    #[msg("{operation} of job `{job}` failed: {source}")]
    Freeze {
        /// Kennung des Jobs.
        job: String,
        /// `freeze`, `thaw`, `probe` (Prüfung auf einen fremden Freeze oder
        /// eine Eindämmung außerhalb von harw) oder `refreeze` (Wieder-
        /// einfrieren, nachdem während des Auftauens eine Eindämmung
        /// auftauchte).
        operation: &'static str,
        /// Ursache.
        #[source]
        source: FreezeError,
    },
}

/// Was `job.suspend` bzw. `job.resume` bewirkt hat (PL-100 WP 3.5).
///
/// # Description
/// Die beiden `Already…`/`Not…`-Fälle sind keine Fehler: wiederholtes
/// Anhalten bzw. Fortsetzen ist idempotent und ändert nichts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspendTransition {
    /// `job.suspend` hat die eigene cgroup des Jobs eingefroren.
    Suspended,
    /// `job.suspend` auf einen schon angehaltenen Job: nichts geändert.
    AlreadySuspended,
    /// `job.resume` hat die eigene cgroup des Jobs aufgetaut.
    Resumed,
    /// `job.resume` auf einen nicht angehaltenen Job: nichts geändert.
    NotSuspended,
}

impl SuspendTransition {
    /// Kurzname (`suspended`, `already_suspended`, `resumed`,
    /// `not_suspended`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Suspended => "suspended",
            Self::AlreadySuspended => "already_suspended",
            Self::Resumed => "resumed",
            Self::NotSuspended => "not_suspended",
        }
    }

    /// Ob der Aufruf etwas geändert hat.
    #[must_use]
    pub fn changed(self) -> bool {
        matches!(self, Self::Suspended | Self::Resumed)
    }
}

/// Ergebnis von [`JobManager::suspend`] und [`JobManager::resume`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendOutcome {
    /// Was geschah.
    pub transition: SuspendTransition,
    /// Zustand des Jobs danach.
    pub status: JobStatus,
    /// Sicht des Kernels nach dem Aufruf (`cgroup.events` `frozen`, auch
    /// über eine Vorfahren-cgroup); `None`, wenn sie nicht geprüft wurde
    /// (kein Freezer, keine bewiesene cgroup, beendeter Job) oder nicht
    /// lesbar ist. Einfrieren ist asynchron: `Some(false)` direkt nach dem
    /// Anhalten heißt „hält gerade an“; `Some(true)` nach dem Fortsetzen
    /// heißt, ein Freeze außerhalb von harw hält den Job weiter.
    pub frozen: Option<bool>,
}

/// Hinweis an den Besitzer, wenn ein Job einer früheren Sitzung noch
/// angehalten ist (B13: kein stilles Einfrieren ohne Ende).
const SUSPENDED_ON_RELOAD: &str = "was suspended when the previous harw session ended; its \
     processes stay frozen until job.resume (job.stop kills it without thawing)";

/// Hinweis an den Besitzer, wenn ein Job einer früheren Sitzung ungeklärt
/// angehalten ist ([`SuspendPhase::Intent`]): `job.resume` lehnt ab, bis
/// das Anhalten von Hand geklärt ist ([`JobError::SuspensionUnresolved`]).
const SUSPENSION_UNRESOLVED_ON_RELOAD: &str = "had an unresolved suspension when the previous \
     harw session ended (only harw's intent to freeze it is recorded, not that its freeze took \
     effect); job.resume is refused and thaws nothing, job.stop kills it without thawing, \
     job.suspend re-freezes it only while its cgroup is not frozen";

/// Identitätsprüfung eines Jobs ohne eigenen pidfd (frühere Sitzung): im
/// Betrieb immer [`recovered_liveness`] (`/proc`); nur Tests ersetzen sie
/// (`JobManager::with_liveness`), weil ein Testprozess nicht in der
/// gefälschten Job-cgroup laufen kann, die seine Identität nennt.
type LivenessCheck = fn(&JobMeta) -> Liveness;

fn io_err(context: &'static str) -> impl FnOnce(io::Error) -> JobError {
    move |source| JobError::Io { context, source }
}

/// Legt eine Logdatei im frisch angelegten Job-Verzeichnis an. `O_APPEND`
/// ist Pflicht: kürzt die Überwachung die Datei (`set_len`), schreibt der
/// Job danach am neuen Ende weiter statt hinter einem Loch.
fn create_log(path: &Path) -> io::Result<File> {
    OpenOptions::new().append(true).create_new(true).open(path)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the monitor waits on: a command the job runtime runs.
enum Waiter {
    Runtime(harw_command::RunFuture<'static>),
}

impl Waiter {
    /// The exit status; cancel-safe (polling again continues the wait).
    async fn wait(&mut self) -> io::Result<ExitStatus> {
        match self {
            Self::Runtime(done) => {
                let outcome = done.as_mut().await;
                match outcome.end {
                    harw_command::CommandEnd::Exited => {
                        Ok(ExitStatus::from_raw(outcome.exit_code << 8))
                    }
                    harw_command::CommandEnd::Signaled(signal) => Ok(ExitStatus::from_raw(signal)),
                    // The runtime killed it (cancel, deadline): a SIGKILL.
                    harw_command::CommandEnd::Cancelled
                    | harw_command::CommandEnd::TimedOut
                    | harw_command::CommandEnd::OutputLimit => Ok(ExitStatus::from_raw(9)),
                    harw_command::CommandEnd::Failed(message) => Err(io::Error::other(message)),
                }
            }
        }
    }
}

/// How long a background job may run before the runtime's deadline (jobs have
/// no wall limit of their own; stop is explicit).
///
/// # Description
/// This is the job runtime's `wall_timeout` for every job of this manager
/// (`harw-job-runtime`, `coordinator/runner.rs`, `run_attempt`). It is a
/// ten-year backstop, not a budget: the runtime arms it once when the attempt
/// starts and knows nothing about `job.suspend`, so it would count suspended
/// time — which is why it must stay far beyond any real run. The budget
/// semantics of a suspension (PL-100 WP 3.5: suspended time never counts as
/// active time, a suspended job never runs out of wall time, `job.resume`
/// continues the remaining budget) are defined on the job record
/// ([`JobMeta::active_secs_at`], module docs of [`crate::model`]).
const JOB_RUNTIME_DEADLINE: Duration = Duration::from_secs(10 * 365 * 24 * 3600);

/// Hands a prepared job to `port`: argv, working directory and environment
/// from the prepared command, output appended to the job's log files. With
/// a chain submission (PL-97 E2b) the request carries its workspace sandbox
/// and resources instead of running as a host command.
async fn start_on_runtime(
    port: Option<Arc<dyn harw_command::CommandPort>>,
    prepared: &PreparedJob,
    sandboxed: Option<harw_tool_shell::SandboxedLaunch>,
    dir: &Path,
    piped: bool,
) -> Result<harw_command::StartedCommand, String> {
    let Some(port) = port else {
        return Err("no job runtime is installed; jobs start only through it".to_owned());
    };
    let request = command_request(prepared, sandboxed, dir, piped);
    port.start(request).await
}

/// The command request of a prepared job ([`start_on_runtime`]).
fn command_request(
    prepared: &PreparedJob,
    sandboxed: Option<harw_tool_shell::SandboxedLaunch>,
    dir: &Path,
    piped: bool,
) -> harw_command::CommandRequest {
    let std = prepared.command.as_std();
    let mut request = harw_command::CommandRequest::new(
        std.get_program().to_string_lossy().into_owned(),
        // No explicit directory: the child inherits ours, as a spawn would.
        std.get_current_dir().map_or_else(
            || std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            Path::to_path_buf,
        ),
        JOB_RUNTIME_DEADLINE,
    );
    request.args = std
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    request.env = if prepared.env_cleared {
        Vec::new()
    } else {
        std::env::vars().collect()
    };
    for (name, value) in std.get_envs() {
        let name = name.to_string_lossy().into_owned();
        request.env.retain(|(existing, _)| *existing != name);
        if let Some(value) = value {
            request
                .env
                .push((name, value.to_string_lossy().into_owned()));
        }
    }
    if piped {
        request.stdin = harw_command::CommandStdin::Pipe;
        request.output = harw_command::CommandOutput::StdoutPipe {
            stderr: dir.join(STDERR_LOG),
        };
    } else {
        request.output = harw_command::CommandOutput::Files {
            stdout: dir.join(STDOUT_LOG),
            stderr: dir.join(STDERR_LOG),
        };
    }
    if let Some(sandboxed) = sandboxed {
        let (sandbox, limits) = sandboxed.into_parts();
        request.sandbox = sandbox;
        request.limits = limits;
    }
    request
}

/// Veränderlicher Zustand eines Jobs.
#[derive(Debug)]
struct EntryState {
    meta: JobMeta,
    stdout_lines: u64,
    stderr_lines: u64,
    tail: VecDeque<String>,
    milestones: u64,
    /// Ein Überwachungs-Task dieser Sitzung beaufsichtigt den Prozess.
    monitored: bool,
}

/// Ein Job im Register.
#[derive(Debug)]
struct JobEntry {
    dir: PathBuf,
    state: Mutex<EntryState>,
    // Serialises the `meta.json` writes of this job: the snapshot is taken
    // and written under it, so writes reach the disk in snapshot order (the
    // last one carries the newest state) and never share `meta.json.tmp`.
    // Taken before `state`, never while `state` is held.
    persist_lock: Mutex<()>,
    // Test seam: how many of the next durable writes (`try_persist`,
    // `try_persist_durable`) fail before touching the disk (a deferred write
    // error, e.g. ENOSPC at `fsync`, that no real directory reproduces on
    // demand).
    #[cfg(test)]
    fail_durable_writes: AtomicUsize,
    changes: watch::Sender<u64>,
    monitor: Mutex<Option<JoinHandle<()>>>,
    /// Gruppenführer, falls diese Sitzung den Job selbst gestartet hat
    /// (pidfd, bleibt auch nach `detach_all` gültig); `None` für Jobs
    /// früherer Sitzungen — die werden nur nach Identitätsprüfung gesteuert.
    leader: Option<OwnLeader>,
}

impl JobEntry {
    fn new(dir: PathBuf, meta: JobMeta, monitored: bool, leader: Option<OwnLeader>) -> Self {
        let (changes, _receiver) = watch::channel(0);
        Self {
            leader,
            dir,
            state: Mutex::new(EntryState {
                meta,
                stdout_lines: 0,
                stderr_lines: 0,
                tail: VecDeque::new(),
                milestones: 0,
                monitored,
            }),
            persist_lock: Mutex::new(()),
            #[cfg(test)]
            fail_durable_writes: AtomicUsize::new(0),
            changes,
            monitor: Mutex::new(None),
        }
    }

    fn bump(&self) {
        self.changes.send_modify(|generation| *generation += 1);
    }

    fn meta(&self) -> JobMeta {
        lock(&self.state).meta.clone()
    }

    fn state(&self) -> JobState {
        lock(&self.state).meta.state
    }

    /// Schreibt `meta.json` nach bestem Bemühen; ein Fehler wird nur
    /// gewarnt (Fortschritt, Ende, Ablösen: der nächste Schreibzugriff holt
    /// es nach). Wo eine Wirkung vom Vermerk abhängt (`job.suspend`), gilt
    /// [`JobEntry::try_persist`].
    ///
    /// # Concurrency
    /// Momentaufnahme und Schreiben unter `persist_lock`: ein Schreiber mit
    /// älterem Stand (die Überwachung) landet nie nach einem neueren.
    fn persist(&self) {
        let _writing = lock(&self.persist_lock);
        let meta = self.meta();
        if let Err(err) = write_meta(&self.dir, &meta) {
            warn!(job_id = %meta.job_id, error = %err, "job meta.json not written");
        }
    }

    /// Schreibt `meta.json` atomar und dauerhaft ([`write_meta_synced`]:
    /// `fsync` vor dem `rename`, damit auch verzögerte Schreibfehler eines
    /// vollen oder entfernten Datenträgers hier ankommen) und meldet den
    /// Fehler. Scheitert es, liegt auf der Platte unverändert der vorige
    /// Vermerk. Das Job-Verzeichnis wird nicht synchronisiert: die
    /// Umbenennung kann ein Stromausfall noch zurücknehmen. Wo das nicht
    /// sein darf (die Absicht von `job.suspend`), gilt
    /// [`JobEntry::try_persist_durable`].
    ///
    /// # Errors
    /// Der Schreib-, Sync- bzw. Umbenennungsfehler.
    ///
    /// # Concurrency
    /// Wie [`JobEntry::persist`] unter `persist_lock`: nach `Ok` kann kein
    /// älterer Stand den Vermerk mehr überschreiben, jeder spätere
    /// Schreibzugriff nimmt den Stand von dann.
    fn try_persist(&self) -> io::Result<()> {
        let writing = lock(&self.persist_lock);
        self.write_synced(&writing)
    }

    /// Wie [`JobEntry::try_persist`], danach wird das Job-Verzeichnis mit
    /// `dir_sync` synchronisiert (`fsync` auf das Verzeichnis): erst dann
    /// ist auch die Umbenennung von `meta.json` dauerhaft, und ein
    /// Stromausfall bringt den vorigen Vermerk nicht mehr zurück. Für den
    /// Vermerk, von dem eine Wirkung abhängt (die Absicht von `job.suspend`
    /// vor `cgroup.freeze` = 1).
    ///
    /// # Errors
    /// [`DurableWriteError::NotWritten`], wenn Schreiben, `fsync` oder
    /// `rename` der Datei scheiterte (auf der Platte liegt unverändert der
    /// vorige Vermerk); [`DurableWriteError::NotDurable`], wenn der neue
    /// Vermerk schon unter `meta.json` steht, der Sync des Verzeichnisses
    /// aber scheiterte (ob er einen Absturz übersteht, ist offen).
    ///
    /// # Concurrency
    /// Schreiben und Verzeichnis-Sync unter einer einzigen Haltung von
    /// `persist_lock`: kein anderer Schreiber (die Überwachung, ohne
    /// `fsync`) benennt dazwischen eine Datei um, deren Verzeichniseintrag
    /// der Sync sonst dauerhaft machte, ohne dass ihr Inhalt es ist.
    fn try_persist_durable(&self, dir_sync: &dyn DirSync) -> Result<(), DurableWriteError> {
        let writing = lock(&self.persist_lock);
        self.write_synced(&writing)
            .map_err(DurableWriteError::NotWritten)?;
        dir_sync
            .sync_dir(&self.dir)
            .map_err(DurableWriteError::NotDurable)
    }

    /// Gemeinsamer Kern von [`JobEntry::try_persist`] und
    /// [`JobEntry::try_persist_durable`]: schreibt den aktuellen Stand mit
    /// [`write_meta_synced`]. `_writing` belegt, dass der Aufrufer
    /// `persist_lock` hält.
    ///
    /// # Errors
    /// Der Schreib-, Sync- bzw. Umbenennungsfehler (in Tests auch ein
    /// injizierter, `JobEntry::fail_next_durable_writes`).
    fn write_synced(&self, _writing: &MutexGuard<'_, ()>) -> io::Result<()> {
        #[cfg(test)]
        self.injected_write_failure()?;
        write_meta_synced(&self.dir, &self.meta())
    }

    /// Test seam: the next `count` durable writes ([`JobEntry::try_persist`],
    /// [`JobEntry::try_persist_durable`]) fail without touching the disk.
    #[cfg(test)]
    fn fail_next_durable_writes(&self, count: usize) {
        self.fail_durable_writes.store(count, Ordering::SeqCst);
    }

    /// Test seam: consumes one armed failure of
    /// [`JobEntry::fail_next_durable_writes`].
    ///
    /// # Errors
    /// An injected I/O error while a failure is armed.
    #[cfg(test)]
    fn injected_write_failure(&self) -> io::Result<()> {
        let armed = self
            .fail_durable_writes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok();
        if armed {
            Err(io::Error::other("injected durable write failure"))
        } else {
            Ok(())
        }
    }

    fn status(&self) -> JobStatus {
        let state = lock(&self.state);
        let meta = state.meta.clone();
        let runtime_secs = meta.started_at.map(|start| {
            let end = meta.ended_at.unwrap_or_else(Timestamp::now);
            u64::try_from(end.as_second().saturating_sub(start.as_second())).unwrap_or(0)
        });
        let runtime_secs = if meta.state == JobState::Unknown && meta.ended_at.is_none() {
            None
        } else {
            runtime_secs
        };
        let mut last_lines: Vec<String> = state
            .tail
            .iter()
            .skip(state.tail.len().saturating_sub(STATUS_TAIL_LINES))
            .cloned()
            .collect();
        let (stdout_lines, stderr_lines) = (state.stdout_lines, state.stderr_lines);
        drop(state);
        if last_lines.is_empty() {
            last_lines = tail_of_file(&self.dir.join(STDOUT_LOG), STATUS_TAIL_LINES / 2);
            last_lines.extend(
                tail_of_file(&self.dir.join(STDERR_LOG), STATUS_TAIL_LINES / 2)
                    .into_iter()
                    .map(|line| format!("[stderr] {line}")),
            );
        }
        JobStatus {
            meta,
            runtime_secs,
            stdout_lines,
            stderr_lines,
            last_lines,
            log_dir: self.dir.clone(),
        }
    }
}

/// Schreibt `meta.json` atomar (temporäre Datei + `rename`).
fn write_meta(dir: &Path, meta: &JobMeta) -> io::Result<()> {
    store_meta(dir, meta, false)
}

/// Wie [`write_meta`], aber die temporäre Datei wird vor dem `rename` mit
/// `fsync` geschrieben: Fehler, die erst beim Zurückschreiben auftreten
/// (voller Datenträger, Kontingent, NFS/FUSE), kommen so beim Aufrufer an,
/// statt einen nie geschriebenen Vermerk als dauerhaft erscheinen zu
/// lassen. Für Vermerke, von denen eine Wirkung abhängt (`job.suspend`).
fn write_meta_synced(dir: &Path, meta: &JobMeta) -> io::Result<()> {
    store_meta(dir, meta, true)
}

/// Gemeinsamer Kern von [`write_meta`] und [`write_meta_synced`].
fn store_meta(dir: &Path, meta: &JobMeta, sync: bool) -> io::Result<()> {
    let text = serde_json::to_vec_pretty(meta).map_err(io::Error::other)?;
    let tmp = dir.join(format!("{META_FILE}.tmp"));
    let mut file = File::create(&tmp)?;
    file.write_all(&text)?;
    if sync {
        file.sync_all()?;
    }
    drop(file);
    fs::rename(&tmp, dir.join(META_FILE))
}

/// Macht die Einträge eines Job-Verzeichnisses dauerhaft (PL-100 WP 3.5):
/// nach dem `rename` von `meta.json` steht der neue Vermerk erst nach einem
/// `fsync` des Verzeichnisses sicher auf der Platte; vorher kann ein
/// Stromausfall den alten Eintrag zurückbringen.
///
/// # Description
/// Naht für Tests (Fehlerinjektion, Aufrufzähler; `JobManager::set_dir_sync`
/// nur unter `cfg(test)`); in Betrieb immer [`SyncDirectory`].
///
/// # Concurrency
/// `Send + Sync`: der [`JobManager`] hält sie hinter einem `Arc` und ruft
/// sie aus dem Thread von `job.suspend` auf.
trait DirSync: Send + Sync {
    /// Synchronisiert das Verzeichnis `dir` (`fsync` auf das Verzeichnis).
    ///
    /// # Errors
    /// Der Öffnungs- bzw. Sync-Fehler.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
}

/// Die [`DirSync`] des Betriebs: öffnet `dir` nur lesend und ruft
/// `sync_all` (`fsync`) auf dem Verzeichnis-Deskriptor.
///
/// # Description
/// - Das Verzeichnis wird wie in [`store_meta`] über seinen Pfad geöffnet,
///   ohne eigenen Symlink-Schutz: harw legt das Job-Verzeichnis selbst
///   unter `<state>/jobs/` an, und kein Zugriff auf `meta.json` prüft es
///   heute auf einen Symlink; ein solcher Schutz gehört zu allen Zugriffen,
///   nicht nur zu diesem.
/// - Jeder Fehler kommt beim Aufrufer an, auch `EINVAL` eines Dateisystems,
///   das `fsync` auf Verzeichnissen nicht kann: `job.suspend` friert dann
///   nicht ein (fail closed).
struct SyncDirectory;

impl DirSync for SyncDirectory {
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        File::open(dir)?.sync_all()
    }
}

/// Warum [`JobEntry::try_persist_durable`] scheiterte: vor oder nach der
/// Umbenennung von `meta.json`. Davon hängt ab, ob auf der Platte schon der
/// neue Vermerk steht, den der Aufrufer zurücknehmen muss.
#[derive(Debug)]
enum DurableWriteError {
    /// Schreiben, `fsync` oder `rename` der Datei scheiterte; `meta.json`
    /// trägt unverändert den vorigen Vermerk.
    NotWritten(io::Error),
    /// `meta.json` trägt den neuen Vermerk, aber der `fsync` des
    /// Job-Verzeichnisses scheiterte: ein Absturz kann ihn noch zurücknehmen.
    NotDurable(io::Error),
}

fn read_meta(dir: &Path) -> Option<JobMeta> {
    let text = fs::read(dir.join(META_FILE)).ok()?;
    serde_json::from_slice(&text).ok()
}

/// Kleinste Obergrenze laufender Prozess-Jobs.
pub const MIN_RUNNING_JOBS: usize = 1;
/// Größte Obergrenze laufender Prozess-Jobs (wie `[jobs] max_running`).
pub const MAX_RUNNING_JOBS: usize = 256;

/// Verwaltet die Prozess-Jobs einer harw-Sitzung.
pub struct JobManager {
    config: JobManagerConfig,
    instance: String,
    notifier: Arc<dyn JobNotifier>,
    jobs: Mutex<BTreeMap<JobId, Arc<JobEntry>>>,
    counter: AtomicU64,
    // Zur Laufzeit verstellbare Obergrenze (Start: `config.max_running_jobs`).
    max_running: AtomicUsize,
    // Starts admitted but not yet inserted into `jobs`; reservations and
    // running entries share the same capacity and are counted under `jobs`.
    starting: AtomicUsize,
    // Eigener Kommando-Port; `None`: der installierte (`harw_command::installed`).
    command_port: Option<Arc<dyn harw_command::CommandPort>>,
    // Freezer für `job.suspend`/`job.resume` (PL-100 WP 3.5); `None`: nicht
    // verfügbar. Gesetzt von der Montage über `set_freezer`.
    freezer: Mutex<Option<Arc<dyn JobFreezer>>>,
    // Serialisiert `suspend`/`resume`: Prüfung, Vermerk und Kernel-Zugriff
    // eines Aufrufs verschränken sich nie mit denen eines anderen. Nie über
    // ein `await` gehalten (beide sind synchron).
    suspend_ops: Mutex<()>,
    // Verzeichnis-Sync des Vermerks von `job.suspend` vor dem Einfrieren
    // (`SyncDirectory`); nur Tests ersetzen ihn (`set_dir_sync`).
    dir_sync: Mutex<Arc<dyn DirSync>>,
    // Lebt der Prozess eines Jobs ohne eigenen pidfd noch (Neuladen,
    // `refresh_foreign`, Stop)? Im Betrieb `recovered_liveness`; nur Tests
    // setzen eine andere (`with_liveness`).
    liveness: LivenessCheck,
}

/// Releases admission on every early return, including cancellation while
/// awaiting the command port. A successful start releases it under the jobs
/// lock, after inserting its running entry, so capacity never has a gap.
struct StartSlot<'a> {
    starting: &'a AtomicUsize,
}

impl Drop for StartSlot<'_> {
    fn drop(&mut self) {
        self.starting.fetch_sub(1, Ordering::SeqCst);
    }
}

impl fmt::Debug for JobManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobManager")
            .field("instance", &self.instance)
            .field("jobs_dir", &self.config.jobs_dir())
            .finish_non_exhaustive()
    }
}

impl JobManager {
    /// Legt `<state_dir>/jobs` an und lädt Jobs früherer Sitzungen.
    ///
    /// # Errors
    /// Wenn das Job-Verzeichnis nicht angelegt werden kann.
    pub fn new(config: JobManagerConfig, notifier: Arc<dyn JobNotifier>) -> io::Result<Arc<Self>> {
        Self::build(config, notifier, None, recovered_liveness)
    }

    /// Test seam: like [`JobManager::new`], but whether the process of a job
    /// without an own pidfd (a previous session) is still alive is decided
    /// by `liveness` instead of `/proc` — when the jobs are loaded and on
    /// every later refresh or stop. A test process cannot run in the fake
    /// job cgroup its recorded identity names, so a restart with a live,
    /// proven job is only reproducible this way.
    ///
    /// # Errors
    /// If the jobs directory cannot be created.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn with_liveness(
        config: JobManagerConfig,
        notifier: Arc<dyn JobNotifier>,
        liveness: LivenessCheck,
    ) -> io::Result<Arc<Self>> {
        Self::build(config, notifier, None, liveness)
    }

    /// Wie [`JobManager::new`], startet Jobs aber über `port` statt über den
    /// prozessweit installierten Kommando-Port (Einbettung, Tests).
    ///
    /// # Errors
    /// Wenn das Job-Verzeichnis nicht angelegt werden kann.
    pub fn with_command_port(
        config: JobManagerConfig,
        notifier: Arc<dyn JobNotifier>,
        port: Arc<dyn harw_command::CommandPort>,
    ) -> io::Result<Arc<Self>> {
        Self::build(config, notifier, Some(port), recovered_liveness)
    }

    fn build(
        config: JobManagerConfig,
        notifier: Arc<dyn JobNotifier>,
        command_port: Option<Arc<dyn harw_command::CommandPort>>,
        liveness: LivenessCheck,
    ) -> io::Result<Arc<Self>> {
        let jobs_dir = config.jobs_dir();
        fs::create_dir_all(&jobs_dir)?;
        let instance = format!(
            "harw-{}-{}",
            std::process::id(),
            Timestamp::now().as_millisecond()
        );
        let config_max_running = config
            .max_running_jobs
            .clamp(MIN_RUNNING_JOBS, MAX_RUNNING_JOBS);
        let dir_sync: Arc<dyn DirSync> = Arc::new(SyncDirectory);
        let manager = Arc::new(Self {
            config,
            instance,
            notifier,
            jobs: Mutex::new(BTreeMap::new()),
            counter: AtomicU64::new(0),
            max_running: AtomicUsize::new(config_max_running),
            starting: AtomicUsize::new(0),
            command_port,
            freezer: Mutex::new(None),
            suspend_ops: Mutex::new(()),
            dir_sync: Mutex::new(dir_sync),
            liveness,
        });
        manager.reload(&jobs_dir);
        Ok(manager)
    }

    /// Die Einstellungen.
    #[must_use]
    pub fn config(&self) -> &JobManagerConfig {
        &self.config
    }

    /// Der Kommando-Port, über den Jobs starten.
    fn port(&self) -> Option<Arc<dyn harw_command::CommandPort>> {
        self.command_port.clone().or_else(harw_command::installed)
    }

    /// The backend this manager's command port runs a workspace sandbox
    /// request with `profile` on (PL-97 E2b,
    /// [`harw_command::CommandPort::workspace_backend`]); `None`: there is
    /// no port, or it runs no workspace requests.
    #[must_use]
    pub fn workspace_backend(
        &self,
        profile: harw_command::SandboxProfileName,
    ) -> Option<harw_command::BackendKind> {
        self.port().and_then(|port| port.workspace_backend(profile))
    }

    /// Kennung dieser harw-Instanz (steht in `meta.json`).
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Lädt `meta.json` (v1 oder v2) aller Job-Verzeichnisse; nicht beendete
    /// Jobs werden [`JobState::Detached`] (Prozess lebt noch, Identität
    /// bewiesen) oder [`JobState::Unknown`]. Passt die Identität nicht (PID
    /// wiederverwendet, keine Startzeit), geht zusätzlich eine
    /// [`JobEvent::Warning`] an den Besitzer; der Prozess wird nie
    /// signalisiert. Temporäre Dateien (`meta.json.tmp`) und Verzeichnisse
    /// ohne lesbare `meta.json` werden übergangen.
    fn reload(&self, jobs_dir: &Path) {
        let Ok(entries) = fs::read_dir(jobs_dir) else {
            return;
        };
        let mut loaded = 0usize;
        let mut warnings = Vec::new();
        for dir_entry in entries.flatten() {
            let dir = dir_entry.path();
            let Some(mut meta) = read_meta(&dir) else {
                continue;
            };
            let dir_name = dir.file_name().and_then(|name| name.to_str());
            if dir_name != Some(meta.job_id.as_str()) {
                warn!(dir = %dir.display(), "job meta.json does not match its directory; skipped");
                continue;
            }
            if !meta.state.is_terminal() {
                let liveness = (self.liveness)(&meta);
                let next = if liveness == Liveness::Alive {
                    JobState::Detached
                } else {
                    JobState::Unknown
                };
                match (&liveness, meta.pid) {
                    (Liveness::Mismatch(reason), Some(pid)) => {
                        // Closures carry no cognitive-complexity increment; the
                        // wording depends on whether an identity was recorded.
                        let message = meta.process_identity().map_or_else(
                            || format!("PID {pid} not controlled: {reason}"),
                            |_| format!("PID {pid} reused, not controlled: {reason}"),
                        );
                        warn!(job_id = %meta.job_id, pid, %reason, "previous job: identity mismatch");
                        warnings.push(JobNotification {
                            owner: meta.owner.clone(),
                            event: JobEvent::Warning {
                                job_id: meta.job_id.clone(),
                                name: meta.name.clone(),
                                message,
                            },
                        });
                    }
                    (Liveness::Unverifiable(reason), Some(pid)) => {
                        warn!(job_id = %meta.job_id, pid, %reason, "previous job: identity not verifiable");
                    }
                    _ => {}
                }
                if next == JobState::Detached && meta.suspended_at.is_some() {
                    // Only a recorded effect is harw's own freeze; an intent
                    // (or a record without a phase) stays unresolved, and
                    // job.resume refuses it (fail closed).
                    let message = if meta.suspension_phase() == Some(SuspendPhase::Applied) {
                        info!(job_id = %meta.job_id, "previous job is still suspended (cgroup frozen)");
                        SUSPENDED_ON_RELOAD
                    } else {
                        warn!(job_id = %meta.job_id, "previous job has an unresolved suspension; job.resume stays refused");
                        SUSPENSION_UNRESOLVED_ON_RELOAD
                    };
                    warnings.push(JobNotification {
                        owner: meta.owner.clone(),
                        event: JobEvent::Warning {
                            job_id: meta.job_id.clone(),
                            name: meta.name.clone(),
                            message: message.to_owned(),
                        },
                    });
                }
                if next == JobState::Unknown {
                    // The process is gone (or not provably ours): an open
                    // suspension ends here, its time is booked.
                    meta.end_suspension(Timestamp::now());
                }
                if next != meta.state {
                    meta.state = next;
                    if let Err(err) = write_meta(&dir, &meta) {
                        warn!(job_id = %meta.job_id, error = %err, "job meta.json not updated");
                    }
                }
            }
            let id = meta.job_id.clone();
            lock(&self.jobs).insert(id, Arc::new(JobEntry::new(dir, meta, false, None)));
            loaded += 1;
        }
        if loaded > 0 {
            info!(loaded, "jobs from previous sessions loaded");
        }
        for notification in warnings {
            self.notifier.notify(notification);
        }
    }

    fn entry(&self, id: &JobId, caller: Caller<'_>) -> Result<Arc<JobEntry>, JobError> {
        let entry = lock(&self.jobs).get(id).cloned();
        match entry {
            Some(entry) if caller.may_control(&lock(&entry.state).meta.owner) => Ok(entry),
            _ => Err(JobError::NotFound(id.to_string())),
        }
    }

    /// Aktuelle Obergrenze gleichzeitig laufender Jobs (`[jobs] max_running`
    /// als Start, danach per [`Self::set_max_running`] verstellbar).
    #[must_use]
    pub fn max_running(&self) -> usize {
        self.max_running.load(Ordering::SeqCst)
    }

    /// Setzt die Obergrenze zur Laufzeit (auf `1..=256` geklemmt) und gibt den
    /// wirksamen Wert zurück. Erhöhen erlaubt sofort weitere Starts; Senken
    /// beendet keinen laufenden Job, neue Starts werden abgewiesen, bis
    /// genug Jobs fertig sind.
    pub fn set_max_running(&self, max: usize) -> usize {
        let max = max.clamp(MIN_RUNNING_JOBS, MAX_RUNNING_JOBS);
        self.max_running.store(max, Ordering::SeqCst);
        max
    }

    /// Zahl der Jobs, die diese Sitzung gerade beaufsichtigt; angehaltene
    /// ([`JobState::Suspended`]) zählen mit, ihr Platz unter
    /// `max_running` bleibt belegt (ihre Prozesse leben, `job.resume` darf die
    /// Grenze nicht überschreiten).
    #[must_use]
    pub fn running_count(&self) -> usize {
        Self::count_running(&lock(&self.jobs))
    }

    fn count_running(jobs: &BTreeMap<JobId, Arc<JobEntry>>) -> usize {
        jobs.values()
            .filter(|entry| {
                let state = lock(&entry.state);
                state.monitored && !state.meta.state.is_terminal()
            })
            .count()
    }

    /// Prüft vorab, ob noch ein Job starten darf (vor einer Host-Freigabe).
    ///
    /// Advisory only: the start itself reserves capacity atomically. Pending
    /// command submissions already occupy capacity even before they run.
    ///
    /// # Errors
    /// [`JobError::Capacity`].
    pub fn check_capacity(&self) -> Result<(), JobError> {
        let jobs = lock(&self.jobs);
        let max = self.max_running();
        if Self::count_running(&jobs) + self.starting.load(Ordering::SeqCst) >= max {
            return Err(JobError::Capacity { max });
        }
        Ok(())
    }

    /// Count and reserve under the insertion lock; never hold that lock
    /// across the asynchronous command submission.
    fn reserve_slot(&self) -> Result<StartSlot<'_>, JobError> {
        let jobs = lock(&self.jobs);
        let max = self.max_running();
        if Self::count_running(&jobs) + self.starting.load(Ordering::SeqCst) >= max {
            return Err(JobError::Capacity { max });
        }
        self.starting.fetch_add(1, Ordering::SeqCst);
        Ok(StartSlot {
            starting: &self.starting,
        })
    }

    fn allocate(&self) -> Result<(JobId, PathBuf), JobError> {
        let jobs_dir = self.config.jobs_dir();
        let stamp = Timestamp::now().strftime("%Y%m%d-%H%M%S").to_string();
        loop {
            let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
            let raw = format!("job-{stamp}-{n:03}");
            let id = JobId::parse(&raw).ok_or_else(|| JobError::Spawn(format!("bad id {raw}")))?;
            let dir = jobs_dir.join(id.as_str());
            match fs::create_dir(&dir) {
                Ok(()) => return Ok((id, dir)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(JobError::Io {
                        context: "create job directory",
                        source,
                    });
                }
            }
        }
    }

    /// Startet einen geprüften Prozess als Job, ohne Hinweise des Startwegs
    /// (siehe [`JobManager::start_with_warnings`]).
    ///
    /// # Description
    /// Muss innerhalb einer Tokio-Runtime laufen (Überwachungs-Task).
    /// stdout/stderr gehen in `O_APPEND`-Logdateien, die die Überwachung am
    /// Byte-Budget ([`JobManagerConfig::max_log_bytes`]) kürzt; Prozesse, die
    /// nach einem Ende ohne Stop noch in der Gruppe leben, beendet sie.
    ///
    /// # Errors
    /// [`JobError::Capacity`], [`JobError::Io`] (Verzeichnis/Logdateien) oder
    /// [`JobError::Spawn`]; ein gescheiterter Start bleibt als
    /// [`JobState::Failed`] mit `launch_error` sichtbar.
    pub async fn start(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
    ) -> Result<JobStatus, JobError> {
        self.start_with_warnings(request, prepared, Vec::new())
            .await
    }

    /// Wie [`JobManager::start`], zusätzlich mit Hinweisen des Startwegs
    /// ([`crate::launcher::PreparedLaunch::warnings`]): sie landen in
    /// `meta.json` (`launch_warnings`, auch bei gescheitertem Start) und
    /// gehen nach `Started` je als [`JobEvent::Warning`] an den Besitzer.
    ///
    /// # Errors
    /// Wie [`JobManager::start`].
    pub async fn start_with_warnings(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
        warnings: Vec<String>,
    ) -> Result<JobStatus, JobError> {
        self.start_with_origin(request, prepared, warnings, JobOrigin::default())
            .await
    }

    /// Wie [`JobManager::start_with_warnings`], zusätzlich mit der Herkunft
    /// ([`JobOrigin`]): sie landet in `meta.json` (`origin_call_id`,
    /// `origin_tool`, `owner_agent`, typisiert in `origin`) und im
    /// [`JobEvent::Started`].
    ///
    /// [`JobManager::start`] and [`JobManager::start_with_warnings`] record
    /// no origin: their jobs have unknown provenance (W20-A), so every
    /// producer that knows its origin (model tools, operator commands)
    /// should start through this method (see [`JobOrigin`]).
    ///
    /// # Errors
    /// Wie [`JobManager::start`].
    pub async fn start_with_origin(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
        warnings: Vec<String>,
        origin: JobOrigin,
    ) -> Result<JobStatus, JobError> {
        self.start_prepared(request, prepared, None, warnings, origin)
            .await
    }

    /// Wie [`JobManager::start_with_origin`] für einen ganzen
    /// [`PreparedLaunch`] (`job.start`): nimmt er die Sandbox-Kette
    /// ([`PreparedLaunch::sandboxed`], PL-97 E2b), geht der Job mit seiner
    /// Workspace-Sandbox und seinen Ressourcen an den Kommando-Port statt
    /// als Host-Job. Hinweise des Startwegs wie bei
    /// [`JobManager::start_with_warnings`].
    ///
    /// # Errors
    /// Wie [`JobManager::start`].
    pub async fn start_launch_with_origin(
        self: &Arc<Self>,
        request: StartRequest,
        launch: PreparedLaunch,
        origin: JobOrigin,
    ) -> Result<JobStatus, JobError> {
        self.start_prepared(
            request,
            launch.job,
            launch.sandboxed,
            launch.warnings,
            origin,
        )
        .await
    }

    async fn start_prepared(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
        sandboxed: Option<harw_tool_shell::SandboxedLaunch>,
        warnings: Vec<String>,
        origin: JobOrigin,
    ) -> Result<JobStatus, JobError> {
        let slot = self.reserve_slot()?;
        let managed_origin = origin.managed();
        let (id, dir) = self.allocate()?;
        // The runtime appends the process output to these files (they outlive
        // this process); created here for the `O_EXCL` guarantee and the mode.
        drop(create_log(&dir.join(STDOUT_LOG)).map_err(io_err("create stdout.log"))?);
        drop(create_log(&dir.join(STDERR_LOG)).map_err(io_err("create stderr.log"))?);

        let mut meta = JobMeta {
            version: META_VERSION,
            job_id: id.clone(),
            name: request.name,
            command: request.command,
            cwd: request.cwd.map(|cwd| cwd.display().to_string()),
            env_keys: request.env_keys,
            state: JobState::Queued,
            pid: None,
            proc_start_ticks: None,
            identity: None,
            executed_on_host: prepared.executed_on_host,
            harw_instance: self.instance.clone(),
            owner: request.owner,
            created_at: Timestamp::now(),
            started_at: None,
            ended_at: None,
            exit_code: None,
            signal: None,
            stop_requested: false,
            detached: false,
            notify_every_secs: request.notify_every.as_secs(),
            progress: None,
            warnings: 0,
            errors: 0,
            launch_error: None,
            stragglers_reaped: false,
            log_truncated: false,
            launch_warnings: warnings,
            origin_call_id: origin.call_id,
            origin_tool: origin.tool,
            owner_agent: origin.owner_agent,
            origin: managed_origin,
            suspended_at: None,
            suspended_secs: 0,
            suspend_phase: None,
        };

        // The job runtime starts the process (PL-93): session, pidfd, cgroup and
        // the output files are its business; this keeps ownership, logs,
        // progress and notifications.
        let started = match start_on_runtime(self.port(), &prepared, sandboxed, &dir, false).await {
            Ok(started) => started,
            Err(message) => {
                warn!(job_id = %id, error = %message, "job spawn failed");
                meta.state = JobState::Failed;
                meta.ended_at = Some(Timestamp::now());
                meta.launch_error = Some(message.clone());
                let entry = Arc::new(JobEntry::new(dir, meta, false, None));
                entry.persist();
                lock(&self.jobs).insert(id, entry);
                return Err(JobError::Spawn(message));
            }
        };

        let pid = started.pid;
        let leader = adopt_child(&mut meta, pid, started.cgroup.as_ref());
        meta.state = JobState::Running;
        meta.started_at = Some(Timestamp::now());
        let entry = Arc::new(JobEntry::new(dir, meta.clone(), true, leader));
        entry.persist();
        {
            let mut jobs = lock(&self.jobs);
            jobs.insert(id.clone(), Arc::clone(&entry));
            drop(slot);
        }
        info!(job_id = %id, pid, executed_on_host = meta.executed_on_host, "job started");

        self.notifier.notify(JobNotification {
            owner: meta.owner.clone(),
            event: JobEvent::Started {
                job_id: id.clone(),
                name: meta.name.clone(),
                command: meta.command.clone(),
                pid,
                executed_on_host: meta.executed_on_host,
                origin_call_id: meta.origin_call_id.clone(),
                origin_tool: meta.origin_tool.clone(),
                owner_agent: meta.owner_agent.clone(),
                origin: meta.resolved_origin(),
            },
        });
        for text in &meta.launch_warnings {
            warn!(job_id = %id, warning = %text, "job started with launch warning");
            self.notifier.notify(JobNotification {
                owner: meta.owner.clone(),
                event: JobEvent::Warning {
                    job_id: id.clone(),
                    name: meta.name.clone(),
                    message: text.clone(),
                },
            });
        }

        let monitor = Monitor {
            entry: Arc::clone(&entry),
            notifier: Arc::clone(&self.notifier),
            config: self.config.clone(),
            notify_every: request.notify_every,
        };
        let handle = tokio::spawn(monitor.run(Waiter::Runtime(started.done)));
        *lock(&entry.monitor) = Some(handle);
        Ok(entry.status())
    }

    /// Wie [`JobManager::start`], aber stdin bleibt offen (`Stdio::piped()`
    /// statt `Stdio::null()`) und stdout wird zusätzlich zu `STDOUT_LOG` als
    /// Zeilenstrom zurückgegeben; stderr geht unverändert direkt in
    /// `STDERR_LOG`. Für einen job-gebundenen Kindprozess, der sein eigenes
    /// Protokoll über stdio spricht (`harw-agent-runner::job_child_backend`):
    /// derselbe Prozessgruppen-/Log-/Überwachungsweg wie jeder andere Job.
    ///
    /// # Description
    /// Muss innerhalb einer Tokio-Runtime laufen (Überwachungs- und
    /// Mitschreib-Task).
    ///
    /// # Errors
    /// [`JobError::Capacity`], [`JobError::Io`] (Verzeichnis/Logdateien) oder
    /// [`JobError::Spawn`] (Start scheiterte oder eine der stdio-Pipes fehlt);
    /// ein gescheiterter Start bleibt als [`JobState::Failed`] mit
    /// `launch_error` sichtbar.
    pub async fn start_piped(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
    ) -> Result<PipedJob, JobError> {
        self.start_piped_with_line_limit(request, prepared, DEFAULT_MAX_PIPED_LINE_BYTES)
            .await
    }

    /// Wie [`JobManager::start_piped`], aber mit eigener Höchstlänge einer
    /// stdout-Zeile (`max_line_bytes`, ohne `\n`). Eine längere Zeile wird
    /// nie über die Grenze hinaus gepuffert: das Mitschreib-Task hört auf zu
    /// lesen, vermerkt es in `STDOUT_LOG`, schickt
    /// [`PipedLineError::TooLong`] und schließt Strom und Pipe.
    ///
    /// # Errors
    /// Wie [`JobManager::start_piped`].
    pub async fn start_piped_with_line_limit(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
        max_line_bytes: usize,
    ) -> Result<PipedJob, JobError> {
        self.start_piped_with_origin(request, prepared, max_line_bytes, JobOrigin::default())
            .await
    }

    /// Like [`JobManager::start_piped_with_line_limit`], additionally with
    /// the job's provenance ([`JobOrigin`], W20-A): it lands in `meta.json`
    /// and in [`JobEvent::Started`] exactly as for
    /// [`JobManager::start_with_origin`]. The job child backend passes
    /// [`JobOrigin::agent_child`].
    ///
    /// # Errors
    /// Like [`JobManager::start_piped`].
    pub async fn start_piped_with_origin(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
        max_line_bytes: usize,
        origin: JobOrigin,
    ) -> Result<PipedJob, JobError> {
        self.start_piped_prepared(request, prepared, None, max_line_bytes, origin)
            .await
    }

    /// Like [`JobManager::start_piped_with_origin`] for a whole
    /// [`PreparedLaunch`] (PL-97 E2b): with a chain submission
    /// ([`PreparedLaunch::sandboxed`], e.g.
    /// [`crate::launcher::child_agent_launch`]) the job goes to the command
    /// port with its workspace sandbox and resources instead of as a host
    /// command. The launch's warnings are not recorded (as for every piped
    /// job).
    ///
    /// # Errors
    /// Like [`JobManager::start_piped`].
    pub async fn start_piped_launch_with_origin(
        self: &Arc<Self>,
        request: StartRequest,
        launch: PreparedLaunch,
        max_line_bytes: usize,
        origin: JobOrigin,
    ) -> Result<PipedJob, JobError> {
        self.start_piped_prepared(
            request,
            launch.job,
            launch.sandboxed,
            max_line_bytes,
            origin,
        )
        .await
    }

    async fn start_piped_prepared(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
        sandboxed: Option<harw_tool_shell::SandboxedLaunch>,
        max_line_bytes: usize,
        origin: JobOrigin,
    ) -> Result<PipedJob, JobError> {
        let slot = self.reserve_slot()?;
        let managed_origin = origin.managed();
        let (id, dir) = self.allocate()?;
        let stdout_log_path = dir.join(STDOUT_LOG);
        let stdout_log = create_log(&stdout_log_path).map_err(io_err("create stdout.log"))?;
        drop(create_log(&dir.join(STDERR_LOG)).map_err(io_err("create stderr.log"))?);

        let mut meta = JobMeta {
            version: META_VERSION,
            job_id: id.clone(),
            name: request.name,
            command: request.command,
            cwd: request.cwd.map(|cwd| cwd.display().to_string()),
            env_keys: request.env_keys,
            state: JobState::Queued,
            pid: None,
            proc_start_ticks: None,
            identity: None,
            executed_on_host: prepared.executed_on_host,
            harw_instance: self.instance.clone(),
            owner: request.owner,
            created_at: Timestamp::now(),
            started_at: None,
            ended_at: None,
            exit_code: None,
            signal: None,
            stop_requested: false,
            detached: false,
            notify_every_secs: request.notify_every.as_secs(),
            progress: None,
            warnings: 0,
            errors: 0,
            launch_error: None,
            stragglers_reaped: false,
            log_truncated: false,
            launch_warnings: Vec::new(),
            origin_call_id: origin.call_id,
            origin_tool: origin.tool,
            owner_agent: origin.owner_agent,
            origin: managed_origin,
            suspended_at: None,
            suspended_secs: 0,
            suspend_phase: None,
        };

        // The runtime starts the process and hands over its stdin/stdout pipes;
        // what flows through them never becomes part of a job record.
        let mut started =
            match start_on_runtime(self.port(), &prepared, sandboxed, &dir, true).await {
                Ok(started) => started,
                Err(message) => return Err(self.record_piped_launch_failure(dir, meta, message)),
            };
        let (Some(stdin), Some(stdout)) = (started.stdin.take(), started.stdout.take()) else {
            (started.cancel)();
            return Err(self.record_piped_launch_failure(
                dir,
                meta,
                "the runtime handed over no stdio pipes".to_owned(),
            ));
        };

        let pid = started.pid;
        let leader = adopt_child(&mut meta, pid, started.cgroup.as_ref());
        meta.state = JobState::Running;
        meta.started_at = Some(Timestamp::now());
        let entry = Arc::new(JobEntry::new(dir, meta.clone(), true, leader));
        entry.persist();
        {
            let mut jobs = lock(&self.jobs);
            jobs.insert(id.clone(), Arc::clone(&entry));
            drop(slot);
        }
        info!(job_id = %id, pid, executed_on_host = meta.executed_on_host, "piped job started");

        self.notifier.notify(JobNotification {
            owner: meta.owner.clone(),
            event: JobEvent::Started {
                job_id: id.clone(),
                name: meta.name.clone(),
                command: meta.command.clone(),
                pid,
                executed_on_host: meta.executed_on_host,
                origin_call_id: meta.origin_call_id.clone(),
                origin_tool: meta.origin_tool.clone(),
                owner_agent: meta.owner_agent.clone(),
                origin: meta.resolved_origin(),
            },
        });

        let (line_tx, line_rx) = mpsc::unbounded_channel();
        let tee = tokio::spawn(tee_stdout(stdout, stdout_log, line_tx, max_line_bytes));

        let monitor = Monitor {
            entry: Arc::clone(&entry),
            notifier: Arc::clone(&self.notifier),
            config: self.config.clone(),
            notify_every: request.notify_every,
        };
        let handle = tokio::spawn(monitor.run_piped(Waiter::Runtime(started.done), tee));
        *lock(&entry.monitor) = Some(handle);
        Ok(PipedJob {
            job_id: id,
            status: entry.status(),
            stdin,
            stdout_lines: line_rx,
        })
    }

    /// Verbucht einen gescheiterten `start_piped`-Aufruf genau wie [`JobManager::start`]
    /// es für seinen eigenen Startfehler tut: Job bleibt als [`JobState::Failed`]
    /// sichtbar, mit `launch_error`.
    fn record_piped_launch_failure(
        &self,
        dir: PathBuf,
        mut meta: JobMeta,
        error: String,
    ) -> JobError {
        warn!(job_id = %meta.job_id, error = %error, "piped job spawn failed");
        meta.state = JobState::Failed;
        meta.ended_at = Some(Timestamp::now());
        meta.launch_error = Some(error.clone());
        let entry = Arc::new(JobEntry::new(dir, meta.clone(), false, None));
        entry.persist();
        lock(&self.jobs).insert(meta.job_id, entry);
        JobError::Spawn(error)
    }

    /// Zustand eines Jobs.
    ///
    /// # Errors
    /// [`JobError::NotFound`] für unbekannte oder fremde Jobs.
    pub fn status(&self, id: &JobId, caller: Caller<'_>) -> Result<JobStatus, JobError> {
        let entry = self.entry(id, caller)?;
        refresh_foreign(&entry, self.liveness);
        Ok(entry.status())
    }

    /// Alle Jobs, die `caller` steuern darf (älteste zuerst).
    #[must_use]
    pub fn list(&self, caller: Caller<'_>) -> Vec<JobStatus> {
        let entries: Vec<Arc<JobEntry>> = lock(&self.jobs).values().cloned().collect();
        entries
            .into_iter()
            .filter(|entry| caller.may_control(&lock(&entry.state).meta.owner))
            .map(|entry| {
                refresh_foreign(&entry, self.liveness);
                entry.status()
            })
            .collect()
    }

    /// Pfad des Log-Verzeichnisses eines Jobs (nach Besitzprüfung).
    ///
    /// # Errors
    /// [`JobError::NotFound`] für unbekannte oder fremde Jobs.
    pub fn log_dir(&self, id: &JobId, caller: Caller<'_>) -> Result<PathBuf, JobError> {
        Ok(self.entry(id, caller)?.dir.clone())
    }

    /// Stoppt einen Job: `signal` an die ganze Prozessgruppe, nach der
    /// Gnadenfrist SIGKILL. Bei einem bereits beendeten Job werden nur noch
    /// verbliebene Gruppenmitglieder beendet. Ein angehaltener Job
    /// ([`JobMeta::is_suspended`]) wird nie aufgetaut: er bekommt statt
    /// `signal` sofort SIGKILL und endet eingefroren (PL-100 WP 3.5); für
    /// ein geordnetes Ende zuerst [`JobManager::resume`] (nur bei harws
    /// eigenem Anhalten; ein ungeklärtes beendet allein der Stop, ohne
    /// aufzutauen).
    ///
    /// # Description
    /// Ein Job einer früheren Sitzung wird nur signalisiert, wenn seine
    /// persistierte Identität nach dem Öffnen eines pidfd bewiesen ist. Passt
    /// sie nicht (PID wiederverwendet), wird nichts gesendet: der Job wird
    /// [`JobState::Unknown`] und der Besitzer erhält eine
    /// [`JobEvent::Warning`].
    ///
    /// # Errors
    /// [`JobError::NotFound`] für unbekannte oder fremde Jobs.
    pub async fn stop(
        &self,
        id: &JobId,
        caller: Caller<'_>,
        signal: JobSignal,
    ) -> Result<JobStatus, JobError> {
        self.stop_reporting_signal(id, caller, signal)
            .await
            .map(|(status, _)| status)
    }

    /// Wie [`JobManager::stop`]; liefert zusätzlich das Signal, mit dem der
    /// Stop tatsächlich begann.
    ///
    /// # Returns
    /// Den Status danach und `signal`, für einen angehaltenen Job aber
    /// [`JobSignal::Kill`] (er wird eingefroren beendet, nie aufgetaut).
    ///
    /// # Errors
    /// [`JobError::NotFound`] für unbekannte oder fremde Jobs.
    pub async fn stop_reporting_signal(
        &self,
        id: &JobId,
        caller: Caller<'_>,
        signal: JobSignal,
    ) -> Result<(JobStatus, JobSignal), JobError> {
        let entry = self.entry(id, caller)?;
        let sent = self.stop_entry(&entry, signal).await;
        Ok((entry.status(), sent))
    }

    /// Installiert den Freezer für `job.suspend`/`job.resume` (PL-100
    /// WP 3.5); ein späterer Aufruf ersetzt ihn.
    ///
    /// # Description
    /// Die Montage übergibt einen `CgroupFreezer` über dieselbe
    /// delegierte cgroup-Wurzel, unter der die Job-Runtime die Job-cgroups
    /// anlegt (`[jobs] cgroup_root`); er liest den Eindämmungsindex des
    /// Wardens ([`crate::CONTAINMENT_HOLDS_DIR`]), damit `job.resume` nie
    /// eine Warden-Eindämmung aufhebt. Ohne Freezer lehnen
    /// [`JobManager::suspend`] und [`JobManager::resume`] mit
    /// [`JobError::SuspendUnavailable`] ab.
    pub fn set_freezer(&self, freezer: Arc<dyn JobFreezer>) {
        *lock(&self.freezer) = Some(freezer);
    }

    /// Der installierte Freezer, falls vorhanden.
    fn freezer(&self) -> Option<Arc<dyn JobFreezer>> {
        lock(&self.freezer).as_ref().map(Arc::clone)
    }

    /// Der Verzeichnis-Sync für den Vermerk von `job.suspend`
    /// ([`SyncDirectory`], in Tests ersetzbar).
    fn dir_sync(&self) -> Arc<dyn DirSync> {
        Arc::clone(&lock(&self.dir_sync))
    }

    /// Test seam: replaces the directory sync that makes the rename of
    /// `job.suspend`'s record durable before the freeze (fault injection,
    /// call counting).
    #[cfg(all(test, target_os = "linux"))]
    fn set_dir_sync(&self, dir_sync: Arc<dyn DirSync>) {
        *lock(&self.dir_sync) = dir_sync;
    }

    /// Freezer und Blattname der bewiesenen eigenen cgroup von `entry`.
    ///
    /// # Errors
    /// [`JobError::SuspendUnavailable`] ohne Freezer,
    /// [`JobError::NoJobCgroup`] ohne bewiesene cgroup
    /// ([`JobMeta::job_cgroup_path`]) direkt unter der Wurzel des Freezers.
    fn freeze_target(&self, entry: &JobEntry) -> Result<(Arc<dyn JobFreezer>, String), JobError> {
        let freezer = self.freezer().ok_or(JobError::SuspendUnavailable)?;
        let state = lock(&entry.state);
        let leaf = state
            .meta
            .job_cgroup_path()
            .and_then(|path| leaf_beneath(path, freezer.delegated_root()))
            .map(str::to_owned)
            .ok_or_else(|| JobError::NoJobCgroup(state.meta.job_id.to_string()))?;
        drop(state);
        Ok((freezer, leaf))
    }

    /// Hält einen Job an (`job.suspend`, PL-100 WP 3.5): friert seine
    /// eigene cgroup ein (`cgroup.freeze` = 1). Die Prozesse bleiben am
    /// Leben und behalten ihren Zustand; [`JobManager::resume`] setzt fort.
    ///
    /// # Description
    /// - Besitz: dieselbe Prüfung wie [`JobManager::stop`] — unbekannte und
    ///   fremde Jobs bekommen [`JobError::NotFound`].
    /// - Neu angehalten wird nur ein [`JobState::Running`]-Job dieser
    ///   Sitzung ohne laufenden Stop; er wird [`JobState::Suspended`],
    ///   `suspended_at` beginnt in der Phase [`SuspendPhase::Intent`].
    /// - Ziel ist ausschließlich die von der Runtime bewiesene eigene cgroup
    ///   ([`JobMeta::job_cgroup_path`]) direkt unter der Wurzel des
    ///   installierten [`JobFreezer`]; ohne Beweis wird nichts eingefroren.
    ///   Nie harws eigene oder die Sitzungs-cgroup.
    /// - Fremder Freeze: ist die cgroup schon eingefroren (oder friert
    ///   gerade ein), ohne dass harw das vermerkt hat, wird abgelehnt
    ///   ([`JobError::FrozenElsewhere`]); harw übernimmt ihn nicht und hebt
    ///   ihn später nie auf. Ebenso, wenn der Eindämmungsindex die cgroup
    ///   schon als vom Warden gehalten zeigt (sein Marker kommt vor seinem
    ///   Freeze, [`JobFreezer::containment_held`]). Ist der Index nicht
    ///   lesbar, wird trotzdem eingefroren (Warnung im Log): Anhalten ist
    ///   die sichere Richtung, und [`JobManager::resume`] fragt den Index
    ///   vor dem Auftauen erneut und taut ohne lesbaren Index nicht auf.
    ///   Geprüft wird vor dem Vermerk der Absicht und noch einmal direkt vor
    ///   dem Schreiben von `cgroup.freeze`, nach den `fsync`s des Vermerks:
    ///   ein Freeze außerhalb von harw (oder ein Marker des Wardens), der in
    ///   dieser Zeit kam, hält harw ebenso auf, statt dass harws dann
    ///   wirkungsloses Schreiben als harws eigener Freeze vermerkt würde. Ein
    ///   frisches Anhalten wird dann wie nach einem gescheiterten Einfrieren
    ///   zurückgenommen, im Speicher und auf der Platte (ließ sich die
    ///   Rücknahme nicht schreiben,
    ///   [`JobError::SuspendRefusedRollbackNotRecorded`]); ein ungeklärtes
    ///   bleibt, wie es war. Offen bleibt nur das Fenster von Mikrosekunden
    ///   zwischen der zweiten Prüfung und dem Schreiben (cgroup v2 kennt kein
    ///   Vergleichen-und-Setzen des Bits): ein Freeze ohne Marker genau darin
    ///   würde als harws eigener vermerkt. Ein Freeze des Wardens ist auch
    ///   dann geschützt: sein Marker kommt vor seinem Freeze, und
    ///   [`JobManager::resume`] fragt den Index vor dem Auftauen.
    /// - Absicht vor Wirkung: die Absicht ([`SuspendPhase::Intent`]) wird
    ///   dauerhaft in `meta.json` vermerkt, bevor der Kernel einfriert:
    ///   `fsync` der Datei, `rename`, danach `fsync` des Job-Verzeichnisses
    ///   (erst er macht die Umbenennung absturzsicher; ohne ihn könnte ein
    ///   Stromausfall den vorigen Vermerk ohne `suspended_at` zurückbringen,
    ///   während die cgroup eingefroren ist). Ein Schreiber mit älterem
    ///   Stand (die Überwachung) kann den Vermerk danach nicht mehr
    ///   überschreiben. Lässt sich der Vermerk nicht schreiben (volles oder
    ///   schreibgeschütztes Zustandsverzeichnis), wird nichts eingefroren
    ///   ([`JobError::SuspendNotRecorded`]); der zurückgenommene Stand wird
    ///   nach bestem Bemühen geschrieben (eine nicht synchronisierte
    ///   Schreibung der Überwachung kann „angehalten“ schon abgelegt haben).
    ///   Scheitert erst der `fsync` des Verzeichnisses, steht „angehalten“
    ///   schon unter `meta.json`: auch dann wird nichts eingefroren
    ///   ([`JobError::SuspendNotRecorded`]), und der Vermerk wird wie nach
    ///   einem gescheiterten Einfrieren zurückgenommen, im Speicher und auf
    ///   der Platte; lässt sich die Rücknahme nicht schreiben, meldet das
    ///   [`JobError::SuspendRollbackNotRecorded`].
    ///   Scheitert das Einfrieren, wird der Vermerk zurückgenommen, im
    ///   Speicher und auf der Platte; lässt sich die Rücknahme nicht
    ///   schreiben, meldet das [`JobError::RollbackNotRecorded`]. Ein
    ///   Absturz dazwischen hinterlässt so nie einen eingefrorenen Job ohne
    ///   Vermerk. Bleibt eine nicht zurückgenommene Absicht stehen, ist sie
    ///   nach einem Neustart ein ungeklärtes Anhalten, das `job.resume`
    ///   ablehnt: harw hebt nie einen Freeze auf, den es nicht belegt.
    /// - Wirkung nach dem Einfrieren: erst danach wird, ebenso dauerhaft,
    ///   [`SuspendPhase::Applied`] vermerkt; nur damit ist das Anhalten
    ///   harws eigenes, das [`JobManager::resume`] auftauen darf (auch nach
    ///   einem Neustart). Lässt sich das nicht dauerhaft schreiben, wird
    ///   **nicht** aufgetaut: der Job bleibt eingefroren, im Speicher und
    ///   (synchronisiert geschrieben) auf der Platte steht wieder die
    ///   Absicht, das Anhalten ist ungeklärt
    ///   ([`JobError::SuspendAppliedNotRecorded`]; ließ sich auch diese
    ///   Rücknahme nicht schreiben,
    ///   [`JobError::SuspendAppliedRollbackNotRecorded`]).
    /// - Idempotent: ein schon angehaltener Job, dessen Anhalten harws
    ///   eigenes ist ([`SuspendPhase::Applied`]), ergibt
    ///   [`SuspendTransition::AlreadySuspended`]; mit bewiesener cgroup wird
    ///   `cgroup.freeze` = 1 erneut geschrieben (ein außerhalb von harw
    ///   aufgehobener Freeze gilt wieder) und die Kernel-Sicht gemeldet.
    /// - Ungeklärtes Anhalten ([`SuspendPhase::Intent`], auch nach einem
    ///   Neustart, für einen abgelösten Job und für einen Eintrag ohne
    ///   Phase): es wird nicht bloß erneut eingefroren, sondern das ganze
    ///   Anhalten wiederholt. Ist die cgroup schon eingefroren (oder friert
    ///   sie ein) oder hält der Eindämmungsindex sie, lässt sich nicht
    ///   belegen, wessen Freeze das ist: abgelehnt
    ///   ([`JobError::SuspensionUnresolved`]), ohne Schreibzugriff auf die
    ///   cgroup. Sonst friert harw selbst ein — Absicht dauerhaft, erneute
    ///   Prüfung, Einfrieren, Wirkung dauerhaft wie oben — und das Anhalten
    ///   ist geklärt ([`SuspendTransition::Suspended`]). Scheitert ein
    ///   Schritt oder zeigt die erneute Prüfung die cgroup nun als gehalten
    ///   (dann ebenso [`JobError::SuspensionUnresolved`], ohne Schreibzugriff
    ///   auf die cgroup), bleibt es ungeklärt wie zuvor; zurückgenommen wird
    ///   nichts.
    /// - Wanduhr: Haltezeit zählt nicht als aktive Laufzeit
    ///   ([`JobMeta::active_secs_at`]); eingefrorene Prozesse verbrauchen kein
    ///   CPU-Budget. Der Platz unter `max_running` bleibt belegt.
    ///
    /// # Errors
    /// [`JobError::NotFound`], [`JobError::NotSuspendable`],
    /// [`JobError::StopPending`], [`JobError::SuspendUnavailable`],
    /// [`JobError::NoJobCgroup`], [`JobError::FrozenElsewhere`],
    /// [`JobError::SuspensionUnresolved`],
    /// [`JobError::SuspendNotRecorded`],
    /// [`JobError::SuspendRollbackNotRecorded`],
    /// [`JobError::RollbackNotRecorded`],
    /// [`JobError::SuspendRefusedRollbackNotRecorded`],
    /// [`JobError::SuspendAppliedNotRecorded`],
    /// [`JobError::SuspendAppliedRollbackNotRecorded`],
    /// [`JobError::Freeze`] (auch `probe`, wenn der Kernel-Zustand nicht
    /// lesbar ist: ohne ihn wird nicht eingefroren).
    ///
    /// # Concurrency
    /// Synchron und blockierend (wenige cgroupfs-Zugriffe; vor und nach dem
    /// Einfrieren je ein `fsync` von `meta.json` und einer des
    /// Job-Verzeichnisses unter der Schreibsperre des Jobs; beim
    /// Rücknehmen bis zu etwa 100 ms Warten auf die Sperre des
    /// Eindämmungsindex): asynchrone Aufrufer nehmen `spawn_blocking`.
    /// Aufrufe von `suspend` und `resume` laufen nacheinander, die Sperre
    /// des Jobs wird nie über einen Kernel-Zugriff gehalten. Endet der Job
    /// zwischen Vermerk und Einfrieren, wird wieder aufgetaut (außer eine
    /// Eindämmung außerhalb von harw hält die cgroup inzwischen) und
    /// [`JobError::NotSuspendable`] gemeldet; wird er dazwischen abgelöst,
    /// bleibt er angehalten.
    pub fn suspend(&self, id: &JobId, caller: Caller<'_>) -> Result<SuspendOutcome, JobError> {
        let entry = self.entry(id, caller)?;
        let _serial = lock(&self.suspend_ops);
        refresh_foreign(&entry, self.liveness);
        let (state, monitored, stop_requested, phase) = {
            let guard = lock(&entry.state);
            (
                guard.meta.state,
                guard.monitored,
                guard.meta.stop_requested,
                guard.meta.suspension_phase(),
            )
        };
        // Only harw's own, recorded freeze is re-asserted. An unresolved
        // suspension (only its intent is recorded) is suspended again from
        // scratch: harw cannot tell whether a freeze of the cgroup is its own.
        let unresolved = match phase {
            Some(SuspendPhase::Applied) => return self.reassert_suspension(id, &entry),
            Some(SuspendPhase::Intent) if stop_requested => {
                return Err(JobError::StopPending(id.to_string()));
            }
            Some(SuspendPhase::Intent) => true,
            None => {
                check_suspendable(id, state, monitored, stop_requested)?;
                false
            }
        };
        let (freezer, leaf) = self.freeze_target(&entry)?;
        refuse_held_cgroup(freezer.as_ref(), &leaf, id, unresolved)?;
        // Intent before effect: the record says "suspended" (intent) before
        // the kernel freezes, so no frozen job is ever left without a
        // record. A fresh suspension is marked here (and taken back when a
        // later step fails); an unresolved one keeps its record as it is.
        let marked_at = Timestamp::now();
        let since = {
            let mut guard = lock(&entry.state);
            if unresolved {
                // Its end (monitor, refresh) closed it in the meantime.
                if !guard.meta.is_suspension_unresolved() {
                    return Err(JobError::NotSuspendable {
                        job: id.to_string(),
                        state: guard.meta.state,
                    });
                }
            } else {
                check_suspendable(
                    id,
                    guard.meta.state,
                    guard.monitored,
                    guard.meta.stop_requested,
                )?;
                guard.meta.state = JobState::Suspended;
            }
            guard.meta.begin_suspension(marked_at);
            guard.meta.suspended_at.unwrap_or(marked_at)
        };
        let fresh = (!unresolved).then_some(marked_at);
        self.record_intent(&entry, id, fresh)?;
        // Probe again right before the kernel write: the durable intent took
        // two `fsync`s, and a freeze put on the cgroup outside harw in that
        // window (an operator, no Warden marker) would turn harw's write into
        // a no-op that is then recorded as harw's own (`applied`), so a later
        // job.resume would thaw it (A3/O3). Only the window between this
        // probe and the write stays open (cgroup v2 has no compare-and-set
        // for `cgroup.freeze`).
        if let Err(refusal) = refuse_held_cgroup(freezer.as_ref(), &leaf, id, unresolved) {
            return Err(undo_refused_intent(&entry, id, fresh, refusal));
        }
        if let Err(source) = freezer.freeze(&leaf) {
            return Err(undo_failed_freeze(&entry, id, fresh, source));
        }
        // Effect: only a suspension still open after the freeze becomes
        // harw's own. The job may have ended between the record and the
        // freeze (its monitor closed the suspension): thaw again so no
        // frozen cgroup outlives a refusal, unless a containment outside
        // harw (the Warden) holds the cgroup by now. A job detached in
        // between stays suspended.
        let ended = {
            let mut guard = lock(&entry.state);
            let open = guard
                .meta
                .set_suspension_phase(since, SuspendPhase::Applied);
            (!open).then_some(guard.meta.state)
        };
        if let Some(state) = ended {
            undo_freeze_of_ended_job(freezer.as_ref(), &leaf, id);
            return Err(JobError::NotSuspendable {
                job: id.to_string(),
                state,
            });
        }
        // The effect must be as durable as the intent: only `applied` lets
        // job.resume thaw, also after a restart. Without it nothing is
        // thawed; the suspension stays unresolved (fail closed).
        if let Err(err) = entry.try_persist_durable(self.dir_sync().as_ref()) {
            return Err(applied_not_recorded(&entry, id, since, err));
        }
        entry.bump();
        info!(job_id = %id, unresolved, "job suspended (cgroup frozen)");
        let frozen = freezer.is_frozen(&leaf).ok();
        Ok(suspend_outcome(
            &entry,
            SuspendTransition::Suspended,
            frozen,
        ))
    }

    /// Vermerkt die Absicht von `job.suspend` dauerhaft in `meta.json`,
    /// bevor der Kernel einfriert: Datei und Verzeichniseintrag
    /// (`JobEntry::try_persist_durable` mit dem Verzeichnis-Sync des
    /// Managers). Scheitert das, wird nicht eingefroren; ein frisches
    /// Anhalten (`fresh` = sein Beginn) wird dann im Speicher und auf der
    /// Platte zurückgenommen, ein ungeklärtes bleibt, wie es vermerkt ist.
    ///
    /// # Errors
    /// [`JobError::SuspendNotRecorded`]; [`JobError::SuspendRollbackNotRecorded`],
    /// wenn nach einem gescheiterten Verzeichnis-Sync auch die Rücknahme
    /// nicht geschrieben wurde.
    fn record_intent(
        &self,
        entry: &JobEntry,
        id: &JobId,
        fresh: Option<Timestamp>,
    ) -> Result<(), JobError> {
        // Durable means the file (`fsync` before the rename) and its
        // directory entry (`fsync` of the job directory after the rename):
        // otherwise a power loss can bring back the previous record, without
        // `suspended_at`, while `cgroup.freeze` = 1 holds.
        match entry.try_persist_durable(self.dir_sync().as_ref()) {
            Ok(()) => Ok(()),
            // Not renamed: the atomic rename usually leaves the previous
            // record on disk, but a monitor write (not synced) that ran
            // between the mark and this write may have stored "suspended"
            // already; so the taken-back state is written too, best effort
            // (a failure is only logged: nothing is frozen, and the next
            // write corrects the record).
            Err(DurableWriteError::NotWritten(source)) => {
                if let Some(marked_at) = fresh {
                    rollback_suspension(entry, marked_at);
                    entry.persist();
                    entry.bump();
                }
                warn!(job_id = %id, "job.suspend aborted: the suspension could not be recorded; nothing frozen");
                debug!(job_id = %id, error = %source, "job.suspend: meta.json not written");
                Err(JobError::SuspendNotRecorded {
                    job: id.to_string(),
                    source,
                })
            }
            // Renamed, but the directory entry may not survive a crash:
            // `meta.json` says "suspended" (intent) while nothing is
            // frozen. A fresh mark is taken back as after a failed freeze:
            // in memory, then on disk with a synced write (`fsync` +
            // `rename`), and that is said when it fails
            // (`SuspendRollbackNotRecorded`: in this session job.resume
            // goes by memory and thaws nothing; after a restart the stale
            // intent is an unresolved suspension that job.resume refuses).
            // The rollback's own directory entry is not synced: only an OS
            // crash or a power loss can lose it, and either one also ends
            // the job's processes, so a reload finds them gone and closes
            // the stale suspension (`JobState::Unknown`).
            Err(DurableWriteError::NotDurable(source)) => {
                warn!(job_id = %id, "job.suspend aborted: the suspension record could not be made durable; nothing frozen");
                debug!(job_id = %id, error = %source, "job.suspend: job directory not synced after writing meta.json");
                if let Some(marked_at) = fresh {
                    rollback_suspension(entry, marked_at);
                    entry.bump();
                    if let Err(persist) = entry.try_persist() {
                        warn!(job_id = %id, "job.suspend: the suspension record could not be taken back");
                        debug!(job_id = %id, error = %persist, "job.suspend: rollback of meta.json not written");
                        return Err(JobError::SuspendRollbackNotRecorded {
                            job: id.to_string(),
                            source,
                            persist,
                        });
                    }
                }
                Err(JobError::SuspendNotRecorded {
                    job: id.to_string(),
                    source,
                })
            }
        }
    }

    /// `job.suspend` auf einen Job, dessen Anhalten harws eigenes ist
    /// ([`SuspendPhase::Applied`]): schreibt `cgroup.freeze` = 1 erneut
    /// (idempotent; ein außerhalb von harw aufgehobener Freeze gilt danach
    /// wieder) und meldet die Kernel-Sicht. Ohne Freezer oder bewiesene
    /// cgroup bleibt nur der Vermerk (`frozen` = `None`).
    ///
    /// # Errors
    /// [`JobError::Freeze`], wenn das erneute Einfrieren scheitert.
    fn reassert_suspension(
        &self,
        id: &JobId,
        entry: &JobEntry,
    ) -> Result<SuspendOutcome, JobError> {
        let frozen = match self.freeze_target(entry) {
            Ok((freezer, leaf)) => {
                if matches!(freezer.freeze_requested(&leaf), Ok(false)) {
                    info!(job_id = %id, "job.suspend: freeze was lifted outside harw; frozen again");
                }
                freezer.freeze(&leaf).map_err(freeze_error(id, "freeze"))?;
                freezer.is_frozen(&leaf).ok()
            }
            Err(JobError::SuspendUnavailable | JobError::NoJobCgroup(_)) => None,
            Err(other) => return Err(other),
        };
        Ok(suspend_outcome(
            entry,
            SuspendTransition::AlreadySuspended,
            frozen,
        ))
    }

    /// Setzt einen angehaltenen Job fort (`job.resume`, PL-100 WP 3.5):
    /// taut seine eigene cgroup auf (`cgroup.freeze` = 0).
    ///
    /// # Description
    /// - Besitz und Ziel wie [`JobManager::suspend`].
    /// - Angehalten heißt [`JobState::Suspended`] (wird wieder
    ///   [`JobState::Running`]) oder ein abgelöster Job mit `suspended_at`
    ///   (angehalten abgelöst bzw. aus einer früheren Sitzung; bleibt
    ///   [`JobState::Detached`]).
    /// - Aufgetaut wird nur harws eigenes Anhalten
    ///   ([`SuspendPhase::Applied`]). Ist nur die Absicht vermerkt
    ///   ([`SuspendPhase::Intent`], auch ein Eintrag ohne Phase, auch nach
    ///   einem Neustart), ist das Anhalten ungeklärt: abgelehnt
    ///   ([`JobError::SuspensionUnresolved`]), ohne jeden Zugriff auf die
    ///   cgroup; der Job bleibt, wie er ist. Geklärt wird es von Hand (siehe
    ///   dort).
    /// - Idempotent: ein nicht angehaltener Job ergibt
    ///   [`SuspendTransition::NotSuspended`] ohne Schreibzugriff. Ist seine
    ///   bewiesene cgroup trotzdem eingefroren (ein Freeze, den harw nicht
    ///   vermerkt hat), wird nichts aufgetaut:
    ///   [`JobError::FrozenElsewhere`].
    /// - Freigabe vor Wirkung: bevor aufgetaut wird, vermerkt `meta.json`
    ///   wieder nur die Absicht (`fsync` der Datei, `rename`). So bleibt nie
    ///   ein `applied` stehen, nachdem harw aufgetaut hat: ein späterer
    ///   Freeze außerhalb von harw wäre sonst nach einem Neustart als harws
    ///   eigener aufzutauen. Lässt sich das nicht schreiben, wird nichts
    ///   aufgetaut ([`JobError::ResumeNotRecorded`]). Wird danach nicht
    ///   aufgetaut (Eindämmung, Prüf- oder Auftaufehler) oder sofort wieder
    ///   eingefroren, gilt wieder `applied` (nach bestem Bemühen auch auf
    ///   der Platte; scheitert das, bleibt der Vermerk ungeklärt, die
    ///   sichere Seite). Nach einem gescheiterten Wiedereinfrieren bleibt es
    ///   ungeklärt: harws Freeze hält dann nicht mehr.
    /// - Eindämmung nach dem Anhalten: cgroup v2 hat ein Freeze-Bit je
    ///   cgroup. Hält der Warden die cgroup inzwischen selbst eingedämmt
    ///   (Marker im Eindämmungsindex, [`JobFreezer::containment_held`]),
    ///   wird nicht aufgetaut und der Job bleibt angehalten
    ///   ([`JobError::ContainmentHeld`]). Gefragt wird unter der geteilten
    ///   Sperre des Index ([`JobFreezer::containment_lease`]; der Warden
    ///   legt Marker und Freeze unter der exklusiven an, wenn er sie
    ///   bekommt) vor dem Auftauen und direkt danach; taucht der Marker
    ///   dazwischen doch auf, wird sofort wieder eingefroren. Die Sperre ist
    ///   nach bestem Bemühen: hält ein anderer Prozess sie länger als etwa
    ///   100 ms, geht es ohne sie weiter (Warnung im Log), dann schützt
    ///   allein die erneute Prüfung. Ist der Index nicht lesbar oder nicht
    ///   vertrauenswürdig, wird nicht aufgetaut ([`JobError::Freeze`] mit
    ///   `probe`). Scheitert das Wiedereinfrieren (`refreeze`), meldet das
    ///   Log das auf `error`. Ist die cgroup nach dem Auftauen verschwunden
    ///   (der Job endete), gilt das als fortgesetzt: es bleibt nichts
    ///   einzudämmen.
    /// - Wanduhr: die Haltezeit wird in `suspended_secs` gebucht; das
    ///   restliche Budget läuft vom Stand vor dem Anhalten weiter.
    ///
    /// # Errors
    /// [`JobError::NotFound`], [`JobError::SuspensionUnresolved`],
    /// [`JobError::SuspendUnavailable`], [`JobError::NoJobCgroup`],
    /// [`JobError::FrozenElsewhere`], [`JobError::ResumeNotRecorded`],
    /// [`JobError::ContainmentHeld`], [`JobError::Freeze`] (`probe`,
    /// `thaw`, `refreeze`).
    ///
    /// # Concurrency
    /// Wie [`JobManager::suspend`]; zusätzlich wartet `resume` höchstens
    /// etwa 100 ms blockierend auf die Sperre des Eindämmungsindex, solange
    /// ein anderer Prozess sie exklusiv hält. Aufgetaut wird nach der
    /// Freigabe und vor dem abschließenden Vermerk: ein Absturz dazwischen
    /// lässt ein ungeklärtes Anhalten stehen, das `job.resume` danach
    /// ablehnt (nie ein `applied`, das einen fremden Freeze aufzutauen
    /// erlaubte). Lässt sich der Vermerk nach dem Auftauen nicht schreiben,
    /// bleibt der Job fortgesetzt und `meta.json` sagt bis zum nächsten
    /// Schreibzugriff „angehalten“, aber nur als Absicht (die sichere
    /// Seite; gewarnt wird im Log).
    pub fn resume(&self, id: &JobId, caller: Caller<'_>) -> Result<SuspendOutcome, JobError> {
        let entry = self.entry(id, caller)?;
        let _serial = lock(&self.suspend_ops);
        refresh_foreign(&entry, self.liveness);
        let (phase, since, terminal) = {
            let guard = lock(&entry.state);
            (
                guard.meta.suspension_phase(),
                guard.meta.suspended_at,
                guard.meta.state.is_terminal(),
            )
        };
        let since = match (phase, since) {
            (None, _) => return self.resume_unsuspended(id, &entry, terminal),
            (Some(SuspendPhase::Applied), Some(since)) => since,
            // Only the intent is recorded: harw cannot prove that a freeze
            // of the cgroup is its own (A3/O3), so nothing is thawed and
            // the cgroup is not even looked at.
            _ => {
                warn!(job_id = %id, "job.resume refused: the suspension is unresolved; nothing thawed");
                return Err(JobError::SuspensionUnresolved(id.to_string()));
            }
        };
        let (freezer, leaf) = self.freeze_target(&entry)?;
        release_suspension(&entry, id, since)?;
        // The freeze bit is shared: never lift a containment the Warden put
        // on the cgroup after harw suspended it (A3/O3).
        match thaw_unless_held(freezer.as_ref(), &leaf, id) {
            Ok(()) => {}
            Err(ThawRefusal::Held) => {
                // Not thawed, or frozen again at once: harw's freeze holds.
                restore_suspension(&entry, id, since);
                warn!(job_id = %id, "job.resume refused: a containment outside harw holds the job cgroup");
                return Err(JobError::ContainmentHeld(id.to_string()));
            }
            Err(ThawRefusal::Failed { operation, source }) => {
                // After a failed refreeze harw's freeze no longer holds: the
                // suspension stays unresolved. Otherwise nothing was thawed
                // (or it was frozen again at once).
                if operation != "refreeze" {
                    restore_suspension(&entry, id, since);
                }
                log_lost_refreeze(id, operation);
                debug!(job_id = %id, operation, error = %source, "job.resume: not resumed");
                return Err(JobError::Freeze {
                    job: id.to_string(),
                    operation,
                    source,
                });
            }
        }
        let transition = if close_suspension(&entry) {
            SuspendTransition::Resumed
        } else {
            SuspendTransition::NotSuspended
        };
        if transition.changed() {
            // Thawed before the record (see Concurrency). A failed write
            // leaves "suspended" on disk while the job runs, but only as the
            // intent written before the thaw: the safe side (job.resume
            // refuses it, a stop kills instead of terminating), and the next
            // write corrects it.
            if let Err(err) = entry.try_persist() {
                warn!(job_id = %id, "job.resume: thawed, but the record still says suspended (unresolved) until the next write");
                debug!(job_id = %id, error = %err, "job.resume: meta.json not written");
            }
            entry.bump();
            info!(job_id = %id, "job resumed (cgroup thawed)");
        }
        let frozen = freezer.is_frozen(&leaf).ok();
        Ok(suspend_outcome(&entry, transition, frozen))
    }

    /// `job.resume` auf einen nicht angehaltenen Job: nichts wird
    /// geschrieben. Ein Freeze, den jemand anderes hält, wird gemeldet,
    /// nie aufgehoben.
    ///
    /// # Errors
    /// [`JobError::FrozenElsewhere`], wenn die bewiesene cgroup eines nicht
    /// beendeten Jobs eingefroren ist (oder einfriert).
    fn resume_unsuspended(
        &self,
        id: &JobId,
        entry: &JobEntry,
        terminal: bool,
    ) -> Result<SuspendOutcome, JobError> {
        let target = (!terminal)
            .then(|| self.freeze_target(entry).ok())
            .flatten();
        let frozen = match target {
            Some((freezer, leaf)) => match probe_held(freezer.as_ref(), &leaf) {
                Ok(true) => {
                    warn!(job_id = %id, "job.resume refused: the job cgroup is frozen outside harw");
                    return Err(JobError::FrozenElsewhere(id.to_string()));
                }
                Ok(false) => Some(false),
                Err(_) => None,
            },
            None => None,
        };
        Ok(suspend_outcome(
            entry,
            SuspendTransition::NotSuspended,
            frozen,
        ))
    }

    /// Stoppt `entry` (siehe [`JobManager::stop`]) und liefert das Signal,
    /// mit dem der Stop begann: für einen angehaltenen Job SIGKILL statt
    /// `requested` — er wird nie aufgetaut, damit ein zur Eindämmung
    /// angehaltener Job (A1) keinen Augenblick mehr läuft; SIGKILL beendet
    /// auch eingefrorene Prozesse (cgroup v2). Die Job-Runtime räumt die
    /// cgroup danach ab (`cgroup.kill`, `rmdir`).
    async fn stop_entry(&self, entry: &Arc<JobEntry>, requested: JobSignal) -> JobSignal {
        let (signal, job_id) = {
            let guard = lock(&entry.state);
            (
                stop_signal_for(&guard.meta, requested),
                guard.meta.job_id.clone(),
            )
        };
        if signal != requested {
            info!(
                job_id = %job_id,
                requested = requested.name(),
                "job stop: suspended job is killed while frozen (no thaw)"
            );
        }
        self.deliver_stop(entry, signal).await;
        signal
    }

    /// Sendet `signal` an die Gruppe von `entry`, wartet die Gnadenfrist ab
    /// und eskaliert zu SIGKILL (Kern von [`JobManager::stop`]).
    async fn deliver_stop(&self, entry: &Arc<JobEntry>, signal: JobSignal) {
        let (meta, monitored) = {
            let state = lock(&entry.state);
            (state.meta.clone(), state.monitored)
        };
        let Some(pid) = meta.pid else {
            return;
        };
        let grace = if signal == JobSignal::Kill {
            KILL_WAIT
        } else {
            self.config.stop_grace
        };

        if monitored && !meta.state.is_terminal() {
            lock(&entry.state).meta.stop_requested = true;
            entry.persist();
            if let Err(err) = signal_entry(entry, &meta, signal) {
                debug!(job_id = %meta.job_id, error = %err, "job stop: signal failed");
            }
            if !wait_terminal(entry, grace).await {
                self.escalate_kill(entry, &meta, signal);
                wait_terminal(entry, KILL_WAIT).await;
            }
            if let Some(leader) = &entry.leader {
                leader.kill_stragglers();
            }
            return;
        }

        if !monitored && meta.state == JobState::Detached {
            // Abgelöster Job dieser Sitzung (eigener pidfd) oder Job einer
            // früheren Sitzung (nur nach bewiesener Identität).
            let mut refusal = None;
            let sent = match signal_entry(entry, &meta, signal) {
                Ok(()) => true,
                Err(SignalError::Io(err)) => {
                    debug!(job_id = %meta.job_id, error = %err, "job stop: signal failed");
                    true
                }
                Err(SignalError::Exited) => false,
                Err(SignalError::Refused(reason)) => {
                    refusal = Some(reason);
                    false
                }
            };
            if sent {
                if !wait_detached_exit(entry, &meta, self.liveness, grace).await {
                    self.escalate_kill(entry, &meta, signal);
                    wait_detached_exit(entry, &meta, self.liveness, KILL_WAIT).await;
                }
                if let Some(leader) = &entry.leader {
                    leader.kill_stragglers();
                }
            }
            {
                let mut state = lock(&entry.state);
                state.meta.stop_requested = sent;
                state.meta.state = if sent {
                    JobState::Stopped
                } else {
                    JobState::Unknown
                };
                let ended_at = Timestamp::now();
                state.meta.ended_at = Some(ended_at);
                state.meta.end_suspension(ended_at);
                state.milestones += 1;
            }
            entry.persist();
            entry.bump();
            if let Some(reason) = refusal {
                warn!(job_id = %meta.job_id, pid, %reason, "job stop refused: process identity");
                self.notifier.notify(JobNotification {
                    owner: meta.owner.clone(),
                    event: JobEvent::Warning {
                        job_id: meta.job_id.clone(),
                        name: meta.name.clone(),
                        message: format!("job.stop sent no signal: {reason}"),
                    },
                });
            }
            return;
        }

        if let Some(leader) = entry.leader.as_ref().filter(|_| meta.state.is_terminal()) {
            leader.kill_stragglers();
        }
    }

    fn escalate_kill(&self, entry: &Arc<JobEntry>, meta: &JobMeta, signal: JobSignal) {
        if signal == JobSignal::Kill {
            return;
        }
        if let Err(err) = signal_entry(entry, meta, JobSignal::Kill) {
            debug!(job_id = %meta.job_id, error = %err, "job stop: SIGKILL failed");
        }
        let message = format!(
            "did not exit within {}s after {}; sent SIGKILL to its process group",
            self.config.stop_grace.as_secs(),
            signal.name()
        );
        warn!(job_id = %meta.job_id, "{message}");
        let owner = lock(&entry.state).meta.owner.clone();
        self.notifier.notify(JobNotification {
            owner,
            event: JobEvent::Warning {
                job_id: meta.job_id.clone(),
                name: meta.name.clone(),
                message,
            },
        });
    }

    /// Waits for a job to reach a terminal state, for at most `timeout`.
    ///
    /// # Description
    /// Public for the runner crate but deliberately NOT a model-facing tool:
    /// delegated work is background-only and the model never blocks on it
    /// (results arrive as notifications). This exists solely for process
    /// supervision in `harw-agent-runner`, which needs the exit code of a
    /// crashed job-managed child within a few seconds. It returns the current
    /// status either way; the caller inspects `status.meta.state` to see
    /// whether the job ended.
    ///
    /// # Errors
    /// [`JobError::NotFound`] for unknown or foreign jobs.
    pub async fn await_exit(
        &self,
        id: &JobId,
        caller: Caller<'_>,
        timeout: Duration,
    ) -> Result<JobStatus, JobError> {
        let entry = self.entry(id, caller)?;
        refresh_foreign(&entry, self.liveness);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut receiver = entry.changes.subscribe();
        let monitored = lock(&entry.state).monitored;
        loop {
            let state = lock(&entry.state).meta.state;
            if state.is_terminal() {
                return Ok(entry.status());
            }
            if !monitored && state != JobState::Queued {
                // No monitor task: probe for signs of life.
                let step = tokio::time::Instant::now() + FOREIGN_POLL;
                tokio::time::sleep_until(step.min(deadline)).await;
                refresh_foreign(&entry, self.liveness);
                if tokio::time::Instant::now() >= deadline {
                    return Ok(entry.status());
                }
                continue;
            }
            match tokio::time::timeout_at(deadline, receiver.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => return Ok(entry.status()),
            }
        }
    }

    /// Löst alle laufenden Jobs von dieser Sitzung (harw beendet sich, die
    /// Nutzerin wählte „weiterlaufen lassen"). Host-Jobs laufen weiter und
    /// sind nach dem nächsten Start über `meta.json` sichtbar; Sandbox-Jobs
    /// enden trotzdem mit harw (`bwrap --die-with-parent`). Ein angehaltener
    /// Job bleibt eingefroren (`suspended_at` bleibt stehen, `meta.json`
    /// sagt `detached`, die Phase des Anhaltens bleibt); der nächste Start
    /// meldet das dem Besitzer, `job.resume` taut ihn auf, wenn das
    /// Anhalten harws eigenes ist ([`SuspendPhase::Applied`]).
    pub fn detach_all(&self) -> DetachSummary {
        let entries: Vec<Arc<JobEntry>> = lock(&self.jobs).values().cloned().collect();
        let mut summary = DetachSummary::default();
        for entry in entries {
            let detach = {
                let mut state = lock(&entry.state);
                let running = state.monitored && !state.meta.state.is_terminal();
                if running {
                    state.monitored = false;
                    state.meta.detached = true;
                    state.meta.state = JobState::Detached;
                    // Letzte Beobachtung für den nächsten Start festhalten.
                    if let Some(fresh) = entry.leader.as_ref().and_then(OwnLeader::refresh_identity)
                    {
                        let fresh = carry_cgroup_proof(state.meta.identity.as_ref(), fresh);
                        state.meta.proc_start_ticks = Some(fresh.start_ticks);
                        state.meta.identity = Some(fresh);
                    }
                    if !state.meta.executed_on_host {
                        summary.ends_with_harw += 1;
                    }
                }
                running
            };
            if detach {
                if let Some(handle) = lock(&entry.monitor).take() {
                    handle.abort();
                }
                entry.persist();
                entry.bump();
                summary.detached += 1;
            }
        }
        summary
    }

    /// Stoppt alle von dieser Sitzung beaufsichtigten Jobs (harw beendet
    /// sich, die Nutzerin wählte „stoppen").
    ///
    /// # Returns
    /// Zahl der gestoppten Jobs.
    pub async fn stop_all(&self) -> usize {
        let entries: Vec<Arc<JobEntry>> = lock(&self.jobs)
            .values()
            .filter(|entry| {
                let state = lock(&entry.state);
                state.monitored && !state.meta.state.is_terminal()
            })
            .cloned()
            .collect();
        let count = entries.len();
        for entry in &entries {
            self.stop_entry(entry, JobSignal::Term).await;
        }
        count
    }

    /// Test seam: registers `meta` as a job without a process of this
    /// session (no pidfd, no monitor task) and writes its `meta.json`.
    ///
    /// # Errors
    /// [`JobError::Io`] if the job directory cannot be created.
    #[cfg(test)]
    pub(crate) fn insert_test_job(
        &self,
        meta: JobMeta,
        monitored: bool,
    ) -> Result<JobId, JobError> {
        let dir = self.config.jobs_dir().join(meta.job_id.as_str());
        fs::create_dir_all(&dir).map_err(io_err("create test job directory"))?;
        let id = meta.job_id.clone();
        let entry = Arc::new(JobEntry::new(dir, meta, monitored, None));
        entry.persist();
        lock(&self.jobs).insert(id.clone(), entry);
        Ok(id)
    }

    /// Test seam: like [`JobManager::insert_test_job`], with our own,
    /// not yet reaped child `leader_pid` adopted as the group leader
    /// (pidfd), as for a job this session started and then detached.
    ///
    /// # Errors
    /// [`JobError::Io`] if the job directory cannot be created.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn insert_test_job_led(
        &self,
        meta: JobMeta,
        monitored: bool,
        leader_pid: u32,
    ) -> Result<JobId, JobError> {
        let dir = self.config.jobs_dir().join(meta.job_id.as_str());
        fs::create_dir_all(&dir).map_err(io_err("create test job directory"))?;
        let id = meta.job_id.clone();
        let (leader, _identity) = OwnLeader::adopt(leader_pid);
        let entry = Arc::new(JobEntry::new(dir, meta, monitored, Some(leader)));
        entry.persist();
        lock(&self.jobs).insert(id.clone(), entry);
        Ok(id)
    }
}

/// Aktualisiert einen nicht beaufsichtigten, als laufend geführten Job.
fn refresh_foreign(entry: &Arc<JobEntry>, liveness: LivenessCheck) {
    let changed = {
        let mut state = lock(&entry.state);
        if state.monitored || state.meta.state != JobState::Detached {
            return;
        }
        let alive = match &entry.leader {
            Some(leader) => !leader.has_exited(),
            None => liveness(&state.meta) == Liveness::Alive,
        };
        if alive {
            false
        } else {
            state.meta.state = JobState::Unknown;
            // The process is gone: an open suspension ends here.
            state.meta.end_suspension(Timestamp::now());
            state.milestones += 1;
            true
        }
    };
    if changed {
        entry.persist();
        entry.bump();
    }
}

/// Wartet höchstens `limit`, bis der Job einen Endzustand hat.
async fn wait_terminal(entry: &Arc<JobEntry>, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    let mut receiver = entry.changes.subscribe();
    loop {
        if entry.state().is_terminal() {
            return true;
        }
        match tokio::time::timeout_at(deadline, receiver.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => return entry.state().is_terminal(),
        }
    }
}

/// Schließt das Anhalten von `entry` nach dem Auftauen ab: Haltezeit
/// buchen, [`JobState::Suspended`] wird wieder [`JobState::Running`] (ein
/// abgelöster Job bleibt [`JobState::Detached`]).
///
/// # Returns
/// Ob der Job angehalten war.
fn close_suspension(entry: &JobEntry) -> bool {
    let mut guard = lock(&entry.state);
    if !guard.meta.is_suspended() {
        return false;
    }
    guard.meta.end_suspension(Timestamp::now());
    if guard.meta.state == JobState::Suspended {
        guard.meta.state = JobState::Running;
    }
    true
}

/// Nimmt im Speicher den Vermerk eines Anhaltens zurück, der sich nicht
/// schreiben oder nicht dauerhaft machen ließ (Verzeichnis-`fsync` nach dem
/// `rename`) oder dessen Einfrieren scheiterte (nur den bei `marked_at`
/// gesetzten, samt seiner Phase; ein inzwischen beendeter Job hat ihn schon
/// selbst geschlossen). Die Platte schreibt der Aufrufer.
fn rollback_suspension(entry: &JobEntry, marked_at: Timestamp) {
    let mut guard = lock(&entry.state);
    if guard.meta.suspended_at == Some(marked_at) {
        guard.meta.suspended_at = None;
        guard.meta.suspend_phase = None;
        if guard.meta.state == JobState::Suspended {
            guard.meta.state = JobState::Running;
        }
    }
}

/// Lehnt das Einfrieren durch `job.suspend` ab (geprüft vor dem Vermerk der
/// Absicht und noch einmal direkt vor dem Schreiben von `cgroup.freeze`),
/// wenn ein Freeze, den harw
/// nicht als seinen eigenen vermerkt hat, die cgroup `leaf` schon hält
/// (A3/O3: übernommen, höbe ihn ein späteres `job.resume` auf), oder wenn
/// der Eindämmungsindex sie als vom Warden gehalten zeigt (sein Marker
/// kommt vor seinem Freeze; das geteilte Bit höbe mit seiner Freigabe
/// harws Anhalten still mit auf). Ein nicht lesbarer Index hält nicht auf:
/// Einfrieren ist die sichere Richtung, und `job.resume` fragt ihn erneut
/// und taut ohne ihn nicht auf.
///
/// # Errors
/// [`JobError::FrozenElsewhere`], für ein ungeklärtes Anhalten
/// (`unresolved`) [`JobError::SuspensionUnresolved`]: ob ein vorhandener
/// Freeze harws eigener ist, lässt sich dann nicht belegen.
/// [`JobError::Freeze`] mit `probe`, wenn der Kernel-Zustand nicht lesbar
/// ist (ohne ihn wird nicht eingefroren).
fn refuse_held_cgroup(
    freezer: &dyn JobFreezer,
    leaf: &str,
    id: &JobId,
    unresolved: bool,
) -> Result<(), JobError> {
    let refused = || {
        if unresolved {
            JobError::SuspensionUnresolved(id.to_string())
        } else {
            JobError::FrozenElsewhere(id.to_string())
        }
    };
    if probe_held(freezer, leaf).map_err(freeze_error(id, "probe"))? {
        warn!(job_id = %id, unresolved, "job.suspend refused: the job cgroup is frozen by a freeze harw has not recorded as its own");
        return Err(refused());
    }
    match freezer.containment_held(leaf) {
        Ok(false) => Ok(()),
        Ok(true) => {
            warn!(job_id = %id, unresolved, "job.suspend refused: a containment outside harw holds the job cgroup");
            Err(refused())
        }
        Err(err) => {
            warn!(job_id = %id, "job.suspend: containment index not readable; freezing anyway, job.resume stays refused until it is");
            debug!(job_id = %id, error = %err, "job.suspend: containment index probe failed");
            Ok(())
        }
    }
}

/// `job.suspend` nach einem gescheiterten Einfrieren: ein frisches Anhalten
/// (`fresh` = sein Beginn) wird im Speicher und auf der Platte
/// zurückgenommen (der dauerhafte Vermerk sagt sonst „angehalten“, obwohl
/// nichts eingefroren ist); ein ungeklärtes bleibt, wie es vermerkt ist.
///
/// # Returns
/// [`JobError::RollbackNotRecorded`], wenn die Rücknahme nicht geschrieben
/// wurde, sonst [`JobError::Freeze`] mit `freeze`.
fn undo_failed_freeze(
    entry: &JobEntry,
    id: &JobId,
    fresh: Option<Timestamp>,
    source: FreezeError,
) -> JobError {
    if let Some(marked_at) = fresh {
        rollback_suspension(entry, marked_at);
        entry.bump();
        if let Err(persist) = entry.try_persist() {
            warn!(job_id = %id, "job.suspend: freeze failed and the suspension record could not be taken back");
            debug!(job_id = %id, error = %persist, "job.suspend: rollback of meta.json not written");
            return JobError::RollbackNotRecorded {
                job: id.to_string(),
                source,
                persist,
            };
        }
    }
    JobError::Freeze {
        job: id.to_string(),
        operation: "freeze",
        source,
    }
}

/// `job.suspend`: die erneute Prüfung direkt vor dem Einfrieren (nach dem
/// dauerhaften Vermerk der Absicht) lehnte ab (`refusal` aus
/// [`refuse_held_cgroup`]: ein Freeze außerhalb von harw oder ein Marker des
/// Wardens kam während der `fsync`s, oder der Kernel-Zustand ist nicht
/// lesbar); es wird nicht eingefroren. Ein frisches Anhalten (`fresh` = sein
/// Beginn) wird wie nach einem gescheiterten Einfrieren im Speicher und auf
/// der Platte zurückgenommen; ein ungeklärtes bleibt, wie es vermerkt ist.
///
/// # Returns
/// `refusal`; [`JobError::SuspendRefusedRollbackNotRecorded`], wenn die
/// Rücknahme nicht geschrieben wurde (auf der Platte bleibt dann die
/// Absicht, nach einem Neustart ein ungeklärtes Anhalten).
fn undo_refused_intent(
    entry: &JobEntry,
    id: &JobId,
    fresh: Option<Timestamp>,
    refusal: JobError,
) -> JobError {
    if let Some(marked_at) = fresh {
        rollback_suspension(entry, marked_at);
        entry.bump();
        if let Err(persist) = entry.try_persist() {
            warn!(job_id = %id, "job.suspend refused before freezing, and the suspension record could not be taken back");
            debug!(job_id = %id, error = %persist, "job.suspend: rollback of meta.json not written");
            return JobError::SuspendRefusedRollbackNotRecorded {
                job: id.to_string(),
                refusal: Box::new(refusal),
                persist,
            };
        }
    }
    refusal
}

/// `job.suspend`: der Job endete zwischen Vermerk und Einfrieren (seine
/// Überwachung schloss das Anhalten). harws eben gesetzter Freeze wird
/// wieder aufgehoben, damit keine eingefrorene cgroup eine Ablehnung
/// überdauert, außer eine Eindämmung außerhalb von harw (der Warden) hält
/// die cgroup inzwischen. Nur Log, kein Fehler: der Aufrufer meldet
/// [`JobError::NotSuspendable`].
fn undo_freeze_of_ended_job(freezer: &dyn JobFreezer, leaf: &str, id: &JobId) {
    match thaw_unless_held(freezer, leaf, id) {
        Ok(())
        | Err(ThawRefusal::Failed {
            source: FreezeError::Gone,
            ..
        }) => {}
        Err(ThawRefusal::Held) => {
            warn!(job_id = %id, "job.suspend: job ended while being frozen; a containment outside harw holds the cgroup, not thawed");
        }
        Err(ThawRefusal::Failed { operation, source }) => {
            log_lost_refreeze(id, operation);
            warn!(job_id = %id, operation, "job.suspend: job ended while being frozen; undo thaw failed");
            debug!(job_id = %id, operation, error = %source, "job.suspend: undo thaw failed");
        }
    }
}

/// `job.suspend`: der Job ist eingefroren, aber [`SuspendPhase::Applied`]
/// (seit `since`) ließ sich nicht dauerhaft schreiben. Es wird nicht
/// aufgetaut. Im Speicher gilt wieder die Absicht, damit `job.resume` in
/// dieser Sitzung so ablehnt wie nach einem Neustart; auf die Platte kommt
/// sie mit einer synchronisierten Schreibung (`JobEntry::try_persist`, wie
/// jede andere Rücknahme dieses Ablaufs): die umbenannte Datei oder eine
/// Schreibung der Überwachung kann schon `applied` tragen, was zwar
/// stimmt, aber einen Neustart auftauen ließe, was diese Sitzung ablehnt.
/// Ein verzögerter Schreibfehler dieser Rücknahme geht so nicht still
/// verloren.
///
/// # Returns
/// [`JobError::SuspendAppliedNotRecorded`] mit dem Schreib- bzw.
/// Sync-Fehler; [`JobError::SuspendAppliedRollbackNotRecorded`], wenn
/// auch die Rücknahme auf die Absicht nicht geschrieben wurde.
fn applied_not_recorded(
    entry: &JobEntry,
    id: &JobId,
    since: Timestamp,
    err: DurableWriteError,
) -> JobError {
    lock(&entry.state)
        .meta
        .set_suspension_phase(since, SuspendPhase::Intent);
    let rollback = entry.try_persist();
    entry.bump();
    let source = match err {
        DurableWriteError::NotWritten(source) | DurableWriteError::NotDurable(source) => source,
    };
    warn!(job_id = %id, "job.suspend: the job cgroup is frozen, but harw could not record that the freeze is its own; the suspension stays unresolved and job.resume refuses it");
    debug!(job_id = %id, error = %source, "job.suspend: applied record not written");
    match rollback {
        Ok(()) => JobError::SuspendAppliedNotRecorded {
            job: id.to_string(),
            source,
        },
        Err(persist) => {
            warn!(job_id = %id, "job.suspend: the record could not be taken back to the intent either; meta.json may say applied until the next write");
            debug!(job_id = %id, error = %persist, "job.suspend: rollback of meta.json to the intent not written");
            JobError::SuspendAppliedRollbackNotRecorded {
                job: id.to_string(),
                source,
                persist,
            }
        }
    }
}

/// `job.resume` vor dem Auftauen: das Anhalten seit `since` gilt wieder nur
/// als Absicht, im Speicher und danach auf der Platte (synchronisierte
/// Schreibung, `JobEntry::try_persist`; ihren Verzeichniseintrag kann nur
/// ein Absturz des Systems verlieren, der auch die Prozesse beendet). So
/// trägt `meta.json` nach dem Auftauen nie mehr `applied`.
///
/// # Errors
/// [`JobError::ResumeNotRecorded`], wenn der Vermerk nicht geschrieben
/// wurde: nichts ist aufgetaut, im Speicher gilt wieder `applied`.
fn release_suspension(entry: &JobEntry, id: &JobId, since: Timestamp) -> Result<(), JobError> {
    // `false` only when the job ended in between (its end closed the
    // suspension): nothing is left to release, the write records the end.
    lock(&entry.state)
        .meta
        .set_suspension_phase(since, SuspendPhase::Intent);
    if let Err(source) = entry.try_persist() {
        lock(&entry.state)
            .meta
            .set_suspension_phase(since, SuspendPhase::Applied);
        warn!(job_id = %id, "job.resume refused: the release of harw's freeze could not be recorded; nothing thawed");
        debug!(job_id = %id, error = %source, "job.resume: meta.json not written");
        return Err(JobError::ResumeNotRecorded {
            job: id.to_string(),
            source,
        });
    }
    Ok(())
}

/// `job.resume` ohne Auftauen (Eindämmung, Prüf- oder Auftaufehler) bzw.
/// sofort wieder eingefroren: harws Freeze hält, das Anhalten seit `since`
/// gilt wieder als [`SuspendPhase::Applied`], im Speicher und nach bestem
/// Bemühen auf der Platte. Scheitert das Schreiben, sagt `meta.json` bis
/// zum nächsten Schreibzugriff nur die Absicht (ungeklärt, die sichere
/// Seite: ein Neustart taut dann nicht auf).
fn restore_suspension(entry: &JobEntry, id: &JobId, since: Timestamp) {
    lock(&entry.state)
        .meta
        .set_suspension_phase(since, SuspendPhase::Applied);
    if let Err(err) = entry.try_persist() {
        warn!(job_id = %id, "job.resume: not resumed, and the record still says unresolved until the next write");
        debug!(job_id = %id, error = %err, "job.resume: meta.json not restored");
    }
}

/// Ob `job.suspend` einen Job in diesem Zustand anhalten darf: nur einen
/// laufenden, beaufsichtigten Job ohne laufenden Stop.
///
/// # Errors
/// [`JobError::NotSuspendable`], [`JobError::StopPending`].
fn check_suspendable(
    id: &JobId,
    state: JobState,
    monitored: bool,
    stop_requested: bool,
) -> Result<(), JobError> {
    if !monitored || state != JobState::Running {
        return Err(JobError::NotSuspendable {
            job: id.to_string(),
            state,
        });
    }
    if stop_requested {
        return Err(JobError::StopPending(id.to_string()));
    }
    Ok(())
}

/// Ob `leaf` von einem Freeze gehalten wird: angefordert (`cgroup.freeze`,
/// auch solange er noch einfriert) oder laut Kernel eingefroren (auch über
/// eine Vorfahren-cgroup).
///
/// # Errors
/// Der erste Fehler der beiden Prüfungen.
fn probe_held(freezer: &dyn JobFreezer, leaf: &str) -> Result<bool, FreezeError> {
    Ok(freezer.freeze_requested(leaf)? || freezer.is_frozen(leaf)?)
}

/// Warum [`thaw_unless_held`] die cgroup nicht aufgetaut gelassen hat.
#[derive(Debug)]
enum ThawRefusal {
    /// Eine Eindämmung außerhalb von harw (der Warden) hält die cgroup:
    /// nicht aufgetaut bzw. sofort wieder eingefroren.
    Held,
    /// Prüfen (`probe`), Auftauen (`thaw`) oder Wiedereinfrieren
    /// (`refreeze`) schlug fehl. Nach `probe` vor dem Auftauen ist nichts
    /// geschehen; nach `probe` danach ist wieder eingefroren.
    Failed {
        /// `probe`, `thaw` oder `refreeze`.
        operation: &'static str,
        /// Ursache.
        source: FreezeError,
    },
}

/// Taut harws eigenen Freeze von `leaf` auf, außer eine Eindämmung
/// außerhalb von harw (der Warden) hält dieselbe cgroup.
///
/// # Description
/// cgroup v2 hat ein Freeze-Bit je cgroup, das harws Anhalten und eine
/// Warden-Eindämmung teilen. Deshalb:
/// 0. Zuerst wird die geteilte Sperre des Eindämmungsindex genommen
///    ([`JobFreezer::containment_lease`]) und bis zum Ende gehalten. Der
///    Warden legt Marker und Freeze unter der exklusiven Sperre an, wenn er
///    sie bekommt; dann kann zwischen Prüfung und Auftauen keine Eindämmung
///    dazukommen. Die Sperre ist nach bestem Bemühen: jeder lokale Prozess
///    kann sie halten. Ist sie nicht zu bekommen (umkämpfter Lease), geht
///    es ohne sie weiter (Warnung mit `job_id`), denn sonst könnte jeder
///    Nutzer jedes `job.resume` blockieren; es schützt dann allein Schritt 2.
///    Ist der Index nicht vertrauenswürdig, wird nicht aufgetaut (`probe`).
/// 1. Vor dem Auftauen wird der Eindämmungsindex gefragt
///    ([`JobFreezer::containment_held`]); gehalten → nichts geschieht.
/// 2. Nach dem Auftauen wird erneut gefragt. Der Warden legt seinen Marker
///    an, *bevor* er einfriert; ein Freeze, der zwischen Prüfung und
///    Auftauen ohne Sperre kam, zeigt sich also jetzt. Dann wird sofort
///    wieder eingefroren und [`ThawRefusal::Held`] gemeldet. Kommt der
///    Marker erst nach der zweiten Prüfung, friert der Warden selbst danach
///    ein.
///
/// Jeder Fehler einer Prüfung zählt als „vielleicht gehalten“ (fail
/// closed): vorher wird nicht aufgetaut, nachher wieder eingefroren. Nur
/// [`FreezeError::Gone`] nach dem Auftauen (bei der zweiten Prüfung oder
/// beim Wiedereinfrieren) zählt als aufgetaut: die cgroup ist mit dem Job
/// verschwunden, es bleibt nichts einzudämmen und nichts zu melden.
///
/// # Errors
/// [`ThawRefusal::Held`] bzw. [`ThawRefusal::Failed`] mit der
/// gescheiterten Operation.
fn thaw_unless_held(freezer: &dyn JobFreezer, leaf: &str, id: &JobId) -> Result<(), ThawRefusal> {
    // Held until the function returns, after the re-check (and refreeze).
    let lease = freezer.containment_lease().map_err(thaw_failed("probe"))?;
    if lease.is_contended() {
        warn!(job_id = %id, "containment index lock not obtained; thawing with check and re-check only");
    }
    if freezer
        .containment_held(leaf)
        .map_err(thaw_failed("probe"))?
    {
        return Err(ThawRefusal::Held);
    }
    freezer.thaw(leaf).map_err(thaw_failed("thaw"))?;
    let after = freezer.containment_held(leaf);
    if matches!(after, Ok(false) | Err(FreezeError::Gone)) {
        // Not held, or the cgroup is gone with the job: nothing to restore.
        return Ok(());
    }
    // Held now, or unknown: restore the freeze before anything else.
    match freezer.freeze(leaf) {
        Ok(()) => {}
        // Removed right after the re-check: no process is left to contain.
        Err(FreezeError::Gone) => return Ok(()),
        Err(source) => {
            return Err(ThawRefusal::Failed {
                operation: "refreeze",
                source,
            });
        }
    }
    match after {
        Err(source) => Err(ThawRefusal::Failed {
            operation: "probe",
            source,
        }),
        Ok(_) => Err(ThawRefusal::Held),
    }
}

/// Bildet einen [`FreezeError`] von `operation` auf
/// [`ThawRefusal::Failed`] ab.
fn thaw_failed(operation: &'static str) -> impl FnOnce(FreezeError) -> ThawRefusal {
    move |source| ThawRefusal::Failed { operation, source }
}

/// Meldet auf `error`, wenn [`thaw_unless_held`] aufgetaut hat und danach
/// nicht wieder einfrieren konnte (`refreeze`): die cgroup läuft, obwohl
/// eine Eindämmung außerhalb von harw sie halten soll (oder das nicht
/// auszuschließen ist), und der Vermerk sagt weiter „angehalten“. Ohne
/// Pfad und Fehlertext; die Einzelheiten stehen auf `debug`.
fn log_lost_refreeze(id: &JobId, operation: &'static str) {
    if operation == "refreeze" {
        error!(
            job_id = %id,
            "job cgroup thawed and could not be frozen again; a containment outside harw may be lifted"
        );
    }
}

/// Bildet einen [`FreezeError`] von `operation` auf `job` auf
/// [`JobError::Freeze`] ab.
fn freeze_error(id: &JobId, operation: &'static str) -> impl FnOnce(FreezeError) -> JobError {
    let job = id.to_string();
    move |source| JobError::Freeze {
        job,
        operation,
        source,
    }
}

/// Das Signal, mit dem ein Stop von `meta` beginnt: `requested`, für einen
/// angehaltenen Job aber SIGKILL (er wird nie aufgetaut, PL-100 WP 3.5).
fn stop_signal_for(meta: &JobMeta, requested: JobSignal) -> JobSignal {
    if meta.is_suspended() {
        JobSignal::Kill
    } else {
        requested
    }
}

/// Baut das Ergebnis von `job.suspend`/`job.resume` mit dem aktuellen
/// Zustand von `entry`.
fn suspend_outcome(
    entry: &JobEntry,
    transition: SuspendTransition,
    frozen: Option<bool>,
) -> SuspendOutcome {
    SuspendOutcome {
        transition,
        status: entry.status(),
        frozen,
    }
}

/// Übernimmt das eben gestartete Kind `pid` (pidfd, Identität) und trägt
/// PID und Identität in `meta` ein; `cgroup` ist die Bescheinigung der
/// eigenen Job-cgroup aus genau diesem Start (PL-97 E4, siehe
/// [`attach_cgroup_proof`]).
fn adopt_child(
    meta: &mut JobMeta,
    pid: Option<u32>,
    cgroup: Option<&harw_command::AttemptCgroup>,
) -> Option<OwnLeader> {
    meta.pid = pid;
    let (leader, identity) = match pid {
        Some(pid) => {
            let (leader, identity) = OwnLeader::adopt(pid);
            (Some(leader), identity)
        }
        None => (None, None),
    };
    meta.proc_start_ticks = identity.as_ref().map(|identity| identity.start_ticks);
    meta.identity = identity;
    attach_cgroup_proof(meta, cgroup);
    leader
}

/// Records the runtime's dedicated-cgroup attestation of this job's own
/// launch as [`DedicatedCgroupProof`], bound to `meta.job_id` (PL-97 E4).
///
/// # Description
/// Fail-closed: without an attestation (no delegated root, no cgroup
/// created) or without a captured identity nothing is written, and an
/// earlier proof is cleared. Whether the proof matches the observed cgroup
/// is decided when it is read ([`crate::JobStatus::job_cgroup_path`]).
fn attach_cgroup_proof(meta: &mut JobMeta, cgroup: Option<&harw_command::AttemptCgroup>) {
    let job_id = meta.job_id.clone();
    if let Some(identity) = meta.identity.as_mut() {
        identity.dedicated_cgroup = cgroup.map(|cgroup| DedicatedCgroupProof {
            job_id: Some(job_id),
            attempt_id: cgroup.attempt_id.clone(),
            delegated_root: cgroup.delegated_root.clone(),
        });
    }
}

/// A freshly observed identity keeps the dedicated-cgroup proof of the
/// launch (`/proc` cannot reproduce it); a moved process no longer matches
/// it and is then not attributed ([`crate::JobStatus::job_cgroup_path`]).
fn carry_cgroup_proof(
    previous: Option<&JobProcessIdentity>,
    mut fresh: JobProcessIdentity,
) -> JobProcessIdentity {
    fresh.dedicated_cgroup = previous.and_then(|identity| identity.dedicated_cgroup.clone());
    fresh
}

/// Identitätsprüfung eines Jobs ohne eigenen pidfd (frühere Sitzung).
fn recovered_liveness(meta: &JobMeta) -> Liveness {
    let Some(pid) = meta.pid else {
        return Liveness::Exited;
    };
    let identity: Option<JobProcessIdentity> = meta.process_identity();
    Recovered {
        pid,
        identity: identity.as_ref(),
        instance: &meta.harw_instance,
        job_id: &meta.job_id,
    }
    .liveness()
}

/// Signal an die Gruppe eines Jobs: über den eigenen pidfd, sonst nur nach
/// bewiesener Identität.
fn signal_entry(entry: &JobEntry, meta: &JobMeta, signal: JobSignal) -> Result<(), SignalError> {
    if let Some(leader) = &entry.leader {
        return leader.signal_group(signal);
    }
    let Some(pid) = meta.pid else {
        return Err(SignalError::Exited);
    };
    let identity = meta.process_identity();
    Recovered {
        pid,
        identity: identity.as_ref(),
        instance: &meta.harw_instance,
        job_id: &meta.job_id,
    }
    .signal_group(signal)
}

/// Ob der Gruppenführer eines nicht beaufsichtigten Jobs beendet ist (oder
/// seine PID nicht mehr ihm gehört); ohne eigenen pidfd entscheidet
/// `liveness`.
fn detached_exited(entry: &JobEntry, meta: &JobMeta, liveness: LivenessCheck) -> bool {
    match &entry.leader {
        Some(leader) => leader.has_exited(),
        None => liveness(meta) != Liveness::Alive,
    }
}

/// Wartet höchstens `limit`, bis ein nicht beaufsichtigter Prozess weg ist.
async fn wait_detached_exit(
    entry: &JobEntry,
    meta: &JobMeta,
    liveness: LivenessCheck,
    limit: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if detached_exited(entry, meta, liveness) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(FOREIGN_POLL).await;
    }
}

/// Liest Zeilen aus der stdout-Pipe eines `start_piped`-Jobs, schreibt jede
/// vollständig (mit Zeilenende) in `stdout_log` und schickt sie zusätzlich an
/// `sender` — bis die Pipe schließt (Prozessende) oder ein Lesefehler
/// auftritt. Ist der Empfänger bereits verworfen, wird trotzdem bis zum Ende
/// weiter mitgeschrieben (nur `STDOUT_LOG` zählt dann noch).
///
/// # Description
/// Anders als `AsyncBufReadExt::lines` puffert es eine Zeile höchstens bis
/// `max_line_bytes`: wird sie länger (oder ist sie kein UTF-8), vermerkt es
/// das in `stdout_log`, schickt genau ein `Err` und endet. Damit schließt es
/// die Pipe; der Rest der Zeile wird nie gelesen, und Überwachung und
/// `stop` laufen wie bei jedem anderen Job weiter.
async fn tee_stdout(
    stdout: ChildStdout,
    mut stdout_log: File,
    sender: mpsc::UnboundedSender<Result<String, PipedLineError>>,
    max_line_bytes: usize,
) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        let failure = match read_bounded_line(&mut reader, &mut buf, max_line_bytes).await {
            Ok(BoundedLine::Line) => match String::from_utf8(std::mem::take(&mut buf)) {
                Ok(line) => {
                    if let Err(err) = writeln!(stdout_log, "{line}") {
                        debug!(error = %err, "job stdout tee: log write failed");
                    }
                    let _ = sender.send(Ok(line));
                    continue;
                }
                Err(_) => PipedLineError::InvalidUtf8,
            },
            Ok(BoundedLine::Eof) => break,
            Ok(BoundedLine::TooLong) => PipedLineError::TooLong {
                limit: max_line_bytes,
            },
            Err(err) => {
                debug!(error = %err, "job stdout tee: read failed");
                break;
            }
        };
        warn!(error = %failure, "job stdout tee: stopped reading stdout");
        if let Err(err) = writeln!(stdout_log, "[harw] {failure}; stopped reading stdout") {
            debug!(error = %err, "job stdout tee: log write failed");
        }
        let _ = sender.send(Err(failure));
        break;
    }
}

/// Ausgang von [`read_bounded_line`].
enum BoundedLine {
    /// Eine Zeile (ohne `\n` und ohne abschließendes `\r`) steht im Puffer.
    Line,
    /// Sauberes Ende der Pipe ohne offene Bytes.
    Eof,
    /// Die Zeile wurde länger als die Grenze; ihr Rest ist ungelesen.
    TooLong,
}

/// Liest die nächste Zeile aus `reader` nach `buf` (vorher geleert) und
/// kopiert dabei höchstens `limit` Inhalts-Bytes (ohne `\n`). Eine letzte
/// Zeile ohne `\n` vor dem Pipe-Ende zählt als Zeile (wie bei `lines()`).
async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    limit: usize,
) -> io::Result<BoundedLine> {
    buf.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if buf.is_empty() {
                BoundedLine::Eof
            } else {
                BoundedLine::Line
            });
        }
        let newline = available.iter().position(|&byte| byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        if buf.len().saturating_add(content_len) > limit {
            return Ok(BoundedLine::TooLong);
        }
        buf.extend_from_slice(&available[..content_len]);
        reader.consume(newline.map_or(content_len, |index| index + 1));
        if newline.is_some() {
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(BoundedLine::Line);
        }
    }
}

/// Meilenstein-Schlüssel: 10-%-Stufe, sonst das erste Wort der Phase.
fn milestone_key(snapshot: &ProgressSnapshot) -> (ProgressSource, Option<u8>, String) {
    match snapshot.percent {
        Some(percent) => (snapshot.source, Some(percent / 10), String::new()),
        None => (
            snapshot.source,
            None,
            snapshot
                .phase
                .as_deref()
                .and_then(|phase| phase.split_whitespace().next())
                .unwrap_or_default()
                .to_ascii_lowercase(),
        ),
    }
}

/// Überwachung eines laufenden Jobs.
struct Monitor {
    entry: Arc<JobEntry>,
    notifier: Arc<dyn JobNotifier>,
    config: JobManagerConfig,
    notify_every: Duration,
}

/// Laufzustand des Überwachungs-Tasks.
struct MonitorState {
    stdout: LogFollower,
    stderr: LogFollower,
    tracker: ProgressTracker,
    throttle: NotifyThrottle,
    last_persist: Instant,
    last_identity_check: Instant,
    dirty: bool,
    announced_milestones: u64,
    /// `stdout.log` ist schon am Byte-Budget gekürzt.
    stdout_truncated: bool,
    /// `stderr.log` ist schon am Byte-Budget gekürzt.
    stderr_truncated: bool,
}

impl Monitor {
    async fn run(self, waiter: Waiter) {
        self.run_inner(waiter, None).await;
    }

    /// Wie [`Monitor::run`], aber wartet nach dem Prozessende zusätzlich auf
    /// `tee` (das Mitschreib-Task von [`JobManager::start_piped`]), damit die
    /// letzte Ausgabe sicher in `STDOUT_LOG` steht, bevor der Ende-Bericht
    /// den Tail liest.
    async fn run_piped(self, waiter: Waiter, tee: JoinHandle<()>) {
        self.run_inner(waiter, Some(tee)).await;
    }

    async fn run_inner(self, mut child: Waiter, tee: Option<JoinHandle<()>>) {
        let started = Instant::now();
        let mut run = MonitorState {
            stdout: LogFollower::new(self.entry.dir.join(STDOUT_LOG)),
            stderr: LogFollower::new(self.entry.dir.join(STDERR_LOG)),
            tracker: ProgressTracker::default(),
            throttle: NotifyThrottle::new(
                ThrottleConfig {
                    every: self.notify_every,
                    error_debounce: self.config.error_debounce,
                    error_min_interval: self.config.error_min_interval,
                    max_error_lines: self.config.max_error_lines,
                },
                started,
            ),
            last_persist: started,
            last_identity_check: started,
            dirty: false,
            announced_milestones: 0,
            stdout_truncated: false,
            stderr_truncated: false,
        };
        let mut ticker = tokio::time::interval(self.config.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let status = loop {
            tokio::select! {
                status = child.wait() => break status,
                _ = ticker.tick() => {
                    self.enforce_log_budget(&mut run);
                    self.ingest(&mut run, false);
                    self.periodic(&mut run, started);
                }
            }
        };
        // Vor `tee.await`: ein verbliebener Prozess, der die stdout-Pipe
        // offen hält, kann das Mitschreib-Task so nicht mehr aufhalten.
        let reaped = self.reap_stragglers().await;
        if let Some(tee) = tee {
            // Der Prozess ist beendet; das Mitschreib-Task endet, sobald es
            // das Ende seiner stdout-Pipe sieht (kurz danach). Erst danach
            // steht die letzte Ausgabe vollständig in `STDOUT_LOG`.
            let _ = tee.await;
        }
        self.enforce_log_budget(&mut run);
        self.ingest(&mut run, true);
        self.finish(&mut run, status, started, reaped);
    }

    /// Beendet nach einem Ende ohne Stop die Prozesse, die noch in der
    /// Job-Gruppe leben: SIGTERM, Abfrage bis zur Gnadenfrist, dann SIGKILL
    /// ([`OwnLeader::kill_stragglers`], mit Prüfung auf
    /// PID-Wiederverwendung).
    ///
    /// # Returns
    /// Ob verbliebene Prozesse gefunden und beendet wurden.
    async fn reap_stragglers(&self) -> bool {
        let Some(leader) = self.entry.leader.as_ref() else {
            return false;
        };
        let (stop_requested, job_id) = {
            let state = lock(&self.entry.state);
            (state.meta.stop_requested, state.meta.job_id.clone())
        };
        if stop_requested || !leader.stragglers_remain() {
            return false;
        }
        match leader.signal_group(JobSignal::Term) {
            Ok(()) | Err(SignalError::Exited) => {}
            Err(SignalError::Refused(reason)) => {
                warn!(job_id = %job_id, reason = %reason, "job stragglers not signalled");
                return false;
            }
            Err(SignalError::Io(err)) => {
                debug!(job_id = %job_id, error = %err, "job stragglers: SIGTERM failed");
            }
        }
        let deadline = tokio::time::Instant::now() + self.config.stop_grace;
        while leader.stragglers_remain() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(self.config.poll_interval).await;
        }
        if leader.stragglers_remain() {
            leader.kill_stragglers();
        }
        true
    }

    /// Hält `stdout.log` und `stderr.log` am Byte-Budget
    /// ([`JobManagerConfig::max_log_bytes`]); beim ersten Kürzen einer Datei
    /// vermerkt es `log_truncated` und warnt den Besitzer.
    fn enforce_log_budget(&self, run: &mut MonitorState) {
        let budget = self.config.max_log_bytes;
        let mut events = Vec::new();
        for is_stderr in [false, true] {
            let (name, already) = if is_stderr {
                (STDERR_LOG, run.stderr_truncated)
            } else {
                (STDOUT_LOG, run.stdout_truncated)
            };
            let path = self.entry.dir.join(name);
            let truncated = match crate::logs::enforce_log_budget(&path, budget, already) {
                Ok(truncated) => truncated,
                Err(err) => {
                    debug!(error = %err, log = name, "job log budget check failed");
                    continue;
                }
            };
            if !truncated || already {
                continue;
            }
            if is_stderr {
                run.stderr_truncated = true;
            } else {
                run.stdout_truncated = true;
            }
            run.dirty = true;
            let mut state = lock(&self.entry.state);
            state.meta.log_truncated = true;
            events.push((
                state.meta.owner.clone(),
                JobEvent::Warning {
                    job_id: state.meta.job_id.clone(),
                    name: state.meta.name.clone(),
                    message: format!(
                        "{name} exceeded the log budget of {budget} bytes; later output is \
                         discarded (the job keeps running)"
                    ),
                },
            ));
        }
        for (owner, event) in events {
            self.notifier.notify(JobNotification { owner, event });
        }
    }

    /// Liest neue Zeilen aus beiden Logs und wertet sie aus.
    fn ingest(&self, run: &mut MonitorState, final_drain: bool) {
        loop {
            let mut more = false;
            for is_stderr in [false, true] {
                let follower = if is_stderr {
                    &mut run.stderr
                } else {
                    &mut run.stdout
                };
                let chunk = match follower.read_new(final_drain) {
                    Ok(chunk) => chunk,
                    Err(err) => {
                        debug!(error = %err, "job log read failed");
                        continue;
                    }
                };
                more |= chunk.more;
                let complete = follower.complete_lines();
                self.observe_lines(run, &chunk.lines, is_stderr, complete);
            }
            if !(final_drain && more) {
                break;
            }
        }
    }

    fn observe_lines(
        &self,
        run: &mut MonitorState,
        lines: &[String],
        is_stderr: bool,
        complete: u64,
    ) {
        let now = Instant::now();
        let mut state = lock(&self.entry.state);
        if is_stderr {
            state.stderr_lines = complete;
        } else {
            state.stdout_lines = complete;
        }
        for line in lines {
            state.tail.push_back(clip_line(line, EVENT_LINE_CHARS));
            while state.tail.len() > TAIL_CAPACITY {
                state.tail.pop_front();
            }
            let mut severity = None;
            for segment in line
                .split('\r')
                .filter(|segment| !segment.trim().is_empty())
            {
                let before = run.tracker.snapshot().map(milestone_key);
                if run.tracker.observe(segment) {
                    let after = run.tracker.snapshot().map(milestone_key);
                    state.meta.progress = run.tracker.snapshot().cloned();
                    run.dirty = true;
                    if before != after {
                        state.milestones += 1;
                    }
                }
                if severity.is_none() {
                    severity = detect_severity(segment).map(|found| (found, segment.to_owned()));
                }
            }
            match severity {
                Some((Severity::Error, segment)) => {
                    state.meta.errors += 1;
                    run.throttle
                        .record_error(clip_line(segment.trim(), EVENT_LINE_CHARS), now);
                    run.dirty = true;
                }
                Some((Severity::Warning, _)) => {
                    state.meta.warnings += 1;
                    run.dirty = true;
                }
                None => {}
            }
        }
    }

    /// Fällige Meldungen und `meta.json`.
    fn periodic(&self, run: &mut MonitorState, started: Instant) {
        let now = Instant::now();
        let mut events = Vec::new();
        let (owner, milestones) = {
            let mut state = lock(&self.entry.state);
            if let Some(batch) = run.throttle.errors_due(now) {
                events.push(JobEvent::ErrorLines {
                    job_id: state.meta.job_id.clone(),
                    name: state.meta.name.clone(),
                    lines: batch.lines,
                    in_batch: batch.in_batch,
                    total_errors: state.meta.errors,
                });
                state.milestones += 1;
            }
            let key = ProgressKey {
                progress: state.meta.progress.clone(),
                lines: state.stdout_lines + state.stderr_lines,
                warnings: state.meta.warnings,
                errors: state.meta.errors,
            };
            if run.throttle.progress_due(now, &key) {
                events.push(JobEvent::Progress {
                    job_id: state.meta.job_id.clone(),
                    name: state.meta.name.clone(),
                    elapsed_secs: now.saturating_duration_since(started).as_secs(),
                    progress: state.meta.progress.clone(),
                    lines: key.lines,
                    warnings: key.warnings,
                    errors: key.errors,
                    last_line: state.tail.back().cloned(),
                });
            }
            (state.meta.owner.clone(), state.milestones)
        };
        for event in events {
            self.notifier.notify(JobNotification {
                owner: owner.clone(),
                event,
            });
        }
        if now.saturating_duration_since(run.last_identity_check) >= IDENTITY_REFRESH {
            run.last_identity_check = now;
            self.refresh_identity();
        }
        if run.dirty && now.saturating_duration_since(run.last_persist) >= META_PERSIST_INTERVAL {
            self.entry.persist();
            run.last_persist = now;
            run.dirty = false;
        }
        // Wake subscribers only on a new milestone.
        if milestones != run.announced_milestones {
            run.announced_milestones = milestones;
            self.entry.bump();
        }
    }

    /// Liest die Identität des Gruppenführers neu und schreibt `meta.json`
    /// sofort, wenn sie sich geändert hat (z. B. nach `exec`).
    fn refresh_identity(&self) {
        let Some(fresh) = self
            .entry
            .leader
            .as_ref()
            .and_then(OwnLeader::refresh_identity)
        else {
            return;
        };
        let changed = {
            let mut state = lock(&self.entry.state);
            let fresh = carry_cgroup_proof(state.meta.identity.as_ref(), fresh);
            if state.meta.identity.as_ref() == Some(&fresh) {
                false
            } else {
                state.meta.proc_start_ticks = Some(fresh.start_ticks);
                state.meta.identity = Some(fresh);
                true
            }
        };
        if changed {
            self.entry.persist();
        }
    }

    /// Endzustand, letzte Meldungen, `meta.json`; `stragglers_reaped`
    /// meldet, ob [`Monitor::reap_stragglers`] Prozesse beendet hat.
    fn finish(
        &self,
        run: &mut MonitorState,
        status: io::Result<ExitStatus>,
        started: Instant,
        stragglers_reaped: bool,
    ) {
        let (exit_code, signal) = match &status {
            Ok(status) => (status.code(), status.signal()),
            Err(err) => {
                warn!(error = %err, "job wait failed");
                (None, None)
            }
        };
        let duration_secs = started.elapsed().as_secs();
        let mut events = Vec::new();
        let owner = {
            let mut state = lock(&self.entry.state);
            if let Some(batch) = run.throttle.flush_errors() {
                events.push(JobEvent::ErrorLines {
                    job_id: state.meta.job_id.clone(),
                    name: state.meta.name.clone(),
                    lines: batch.lines,
                    in_batch: batch.in_batch,
                    total_errors: state.meta.errors,
                });
            }
            let final_state = if state.meta.stop_requested {
                JobState::Stopped
            } else if exit_code == Some(0) {
                JobState::Succeeded
            } else {
                JobState::Failed
            };
            state.meta.state = final_state;
            state.meta.exit_code = exit_code;
            state.meta.signal = signal;
            let ended_at = Timestamp::now();
            state.meta.ended_at = Some(ended_at);
            // A job that ends while suspended (SIGKILL, runtime deadline)
            // closes its suspension at its end.
            state.meta.end_suspension(ended_at);
            state.meta.progress = run.tracker.snapshot().cloned();
            state.meta.stragglers_reaped = stragglers_reaped;
            state.milestones += 1;
            if stragglers_reaped {
                info!(job_id = %state.meta.job_id, "job stragglers reaped");
                events.push(JobEvent::Warning {
                    job_id: state.meta.job_id.clone(),
                    name: state.meta.name.clone(),
                    message: format!(
                        "processes the job left running in its process group were terminated \
                         (SIGTERM, SIGKILL after {}s if still alive)",
                        self.config.stop_grace.as_secs()
                    ),
                });
            }
            let tail: Vec<String> = state
                .tail
                .iter()
                .skip(
                    state
                        .tail
                        .len()
                        .saturating_sub(self.config.finish_tail_lines),
                )
                .cloned()
                .collect();
            events.push(JobEvent::Finished {
                job_id: state.meta.job_id.clone(),
                name: state.meta.name.clone(),
                state: final_state,
                exit_code,
                signal,
                duration_secs,
                tail,
            });
            info!(
                job_id = %state.meta.job_id,
                state = %final_state,
                exit_code,
                signal,
                duration_secs,
                "job finished"
            );
            state.meta.owner.clone()
        };
        self.entry.persist();
        for event in events {
            self.notifier.notify(JobNotification {
                owner: owner.clone(),
                event,
            });
        }
        self.entry.bump();
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "linux"))]
mod suspend_tests;

#[cfg(test)]
mod error_display_tests {
    use super::{FreezeError, JobError, JobState, PipedLineError, SuspendTransition};
    use std::error::Error as _;

    /// PL-100 WP 3.5: messages and sources of the suspend errors, and the
    /// names of the transitions.
    #[test]
    fn suspend_errors_and_transitions_are_stable() {
        assert_eq!(
            JobError::NotSuspendable {
                job: "j1".to_owned(),
                state: JobState::Stopped,
            }
            .to_string(),
            "job `j1` cannot be suspended in state stopped; only running jobs of this session can"
        );
        assert_eq!(
            JobError::NoJobCgroup("j1".to_owned()).to_string(),
            "job `j1` has no own cgroup beneath the delegated cgroup root; it cannot be suspended or resumed"
        );
        assert_eq!(
            JobError::SuspendUnavailable.to_string(),
            "job suspension is not available: jobs have no delegated cgroup v2 root here"
        );
        let freeze = JobError::Freeze {
            job: "j1".to_owned(),
            operation: "freeze",
            source: FreezeError::PermissionDenied,
        };
        assert_eq!(
            freeze.to_string(),
            "freeze of job `j1` failed: permission denied on the job cgroup"
        );
        assert!(freeze.source().is_some());
        assert!(JobError::SuspendUnavailable.source().is_none());
        assert_eq!(
            JobError::FrozenElsewhere("j1".to_owned()).to_string(),
            "job `j1` is frozen by someone other than harw (an operator, the Warden or a parent cgroup); harw neither takes over nor lifts that freeze"
        );
        assert_eq!(
            JobError::StopPending("j1".to_owned()).to_string(),
            "job `j1` is being stopped; it cannot be suspended"
        );
        assert_eq!(
            JobError::ContainmentHeld("j1".to_owned()).to_string(),
            "job `j1` is held by a containment outside harw (the Warden); job.resume refused, the job stays suspended"
        );
        let not_recorded = JobError::SuspendNotRecorded {
            job: "j1".to_owned(),
            source: std::io::Error::other("disk full"),
        };
        assert_eq!(
            not_recorded.to_string(),
            "job `j1` was not suspended: the suspension could not be recorded (disk full); nothing was frozen"
        );
        assert!(not_recorded.source().is_some());
        let not_taken_back = JobError::SuspendRollbackNotRecorded {
            job: "j1".to_owned(),
            source: std::io::Error::other("dir sync failed"),
            persist: std::io::Error::other("read-only"),
        };
        assert_eq!(
            not_taken_back.to_string(),
            "job `j1` was not suspended: the suspension could not be recorded durably (dir sync failed) and its record could not be taken back (read-only); nothing was frozen"
        );
        // `source()` is the directory sync failure; the persist error is in
        // the text.
        assert_eq!(
            not_taken_back.source().map(ToString::to_string),
            Some("dir sync failed".to_owned())
        );
        let rollback = JobError::RollbackNotRecorded {
            job: "j1".to_owned(),
            source: FreezeError::PermissionDenied,
            persist: std::io::Error::other("read-only"),
        };
        assert_eq!(
            rollback.to_string(),
            "freeze of job `j1` failed: permission denied on the job cgroup; the suspension record could not be taken back (read-only)"
        );
        // `source()` is the freeze failure; the persist error is in the text.
        assert_eq!(
            rollback.source().map(ToString::to_string),
            Some(FreezeError::PermissionDenied.to_string())
        );
        // Review round 3: the probe right before the freeze refused, and the
        // rollback of the fresh mark was not written.
        let refused = JobError::SuspendRefusedRollbackNotRecorded {
            job: "j1".to_owned(),
            refusal: Box::new(JobError::FrozenElsewhere("j1".to_owned())),
            persist: std::io::Error::other("read-only"),
        };
        assert_eq!(
            refused.to_string(),
            "job `j1` was not suspended and harw froze nothing: job `j1` is frozen by someone other than harw (an operator, the Warden or a parent cgroup); harw neither takes over nor lifts that freeze; the suspension record could not be taken back (read-only)"
        );
        // `source()` is the refusal; the persist error is in the text.
        assert_eq!(
            refused.source().map(ToString::to_string),
            Some(JobError::FrozenElsewhere("j1".to_owned()).to_string())
        );
        assert_eq!(
            JobError::Freeze {
                job: "j1".to_owned(),
                operation: "probe",
                source: FreezeError::Gone,
            }
            .to_string(),
            "probe of job `j1` failed: the job cgroup does not exist (any more)"
        );
        // Review thread 4505015304: the unresolved suspension and the two
        // records around the freeze bit.
        let unresolved = JobError::SuspensionUnresolved("j1".to_owned());
        assert_eq!(
            unresolved.to_string(),
            "job `j1` has an unresolved suspension: only harw's intent to freeze it is recorded, not that harw's freeze took effect, so harw cannot prove a freeze of its cgroup is its own; nothing is thawed (job.stop kills it without thawing; job.suspend re-freezes it only while its cgroup is not frozen)"
        );
        assert!(unresolved.source().is_none());
        let applied = JobError::SuspendAppliedNotRecorded {
            job: "j1".to_owned(),
            source: std::io::Error::other("disk full"),
        };
        assert_eq!(
            applied.to_string(),
            "job `j1` is frozen, but harw could not record that the freeze is its own (disk full); its suspension stays unresolved: job.resume is refused, job.stop kills the job without thawing it"
        );
        assert_eq!(
            applied.source().map(ToString::to_string),
            Some("disk full".to_owned())
        );
        let applied_not_taken_back = JobError::SuspendAppliedRollbackNotRecorded {
            job: "j1".to_owned(),
            source: std::io::Error::other("dir sync failed"),
            persist: std::io::Error::other("read-only"),
        };
        assert_eq!(
            applied_not_taken_back.to_string(),
            "job `j1` is frozen, but harw could not record that the freeze is its own (dir sync failed) and could not take its record back to the intent (read-only); its suspension stays unresolved: job.resume is refused, job.stop kills the job without thawing it"
        );
        // `source()` is the failed applied record; the rollback error is in
        // the text.
        assert_eq!(
            applied_not_taken_back.source().map(ToString::to_string),
            Some("dir sync failed".to_owned())
        );
        let release = JobError::ResumeNotRecorded {
            job: "j1".to_owned(),
            source: std::io::Error::other("read-only"),
        };
        assert_eq!(
            release.to_string(),
            "job `j1` was not resumed: harw could not record that it releases its freeze (read-only); nothing was thawed, the job stays suspended"
        );
        assert_eq!(
            release.source().map(ToString::to_string),
            Some("read-only".to_owned())
        );

        let names: Vec<(&str, bool)> = [
            SuspendTransition::Suspended,
            SuspendTransition::AlreadySuspended,
            SuspendTransition::Resumed,
            SuspendTransition::NotSuspended,
        ]
        .into_iter()
        .map(|transition| (transition.as_str(), transition.changed()))
        .collect();
        assert_eq!(
            names,
            [
                ("suspended", true),
                ("already_suspended", false),
                ("resumed", true),
                ("not_suspended", false),
            ]
        );
    }

    #[test]
    fn display_and_source_are_stable() {
        assert_eq!(
            PipedLineError::TooLong { limit: 8 }.to_string(),
            "job stdout line exceeds the 8-byte limit"
        );
        assert_eq!(
            PipedLineError::InvalidUtf8.to_string(),
            "job stdout line is not valid UTF-8"
        );
        assert_eq!(
            JobError::NotFound("j1".to_owned()).to_string(),
            "unknown job `j1` (or not started by you or one of your sub-agents)"
        );
        assert_eq!(
            JobError::Capacity { max: 4 }.to_string(),
            "too many running jobs (max 4, set by `[jobs] max_running`); wait for one to finish or stop one"
        );
        assert_eq!(
            JobError::Spawn("x".to_owned()).to_string(),
            "failed to start the job: x"
        );
        let io = JobError::Io {
            context: "ctx",
            source: std::io::Error::other("boom"),
        };
        assert_eq!(io.to_string(), "ctx: boom");
        assert!(io.source().is_some());
        assert!(JobError::Spawn("x".to_owned()).source().is_none());
    }
}
