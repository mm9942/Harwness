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
//!
//! # Nebenläufigkeit
//! `Send + Sync`; interne `std::sync::Mutex` werden nie über ein `await`
//! gehalten, der [`JobNotifier`] wird ohne gehaltene Sperre aufgerufen.
//! Warten (`stop`, `wait`) läuft über einen `watch`-Kanal je Job.

use crate::event::{JobEvent, JobNotification, JobNotifier};
use crate::launcher::PreparedJob;
use crate::logs::{LogFollower, clip_line, tail_of_file};
use crate::model::{
    JobId, JobMeta, JobOwner, JobProcessIdentity, JobState, JobStatus, META_FILE, META_VERSION,
    STDERR_LOG, STDOUT_LOG,
};
use crate::procfs::{JobSignal, Liveness, OwnLeader, Recovered, SignalError};
use crate::progress::{
    ProgressSnapshot, ProgressSource, ProgressTracker, Severity, detect_severity,
};
use crate::throttle::{NotifyThrottle, ProgressKey, ThrottleConfig};
use harw_types::cancel::CancelToken;
use jiff::Timestamp;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

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
/// bestehenden Aufrufer (Operator-Wege, Kind-Backends) unverändert bleiben;
/// sie starten ohne Herkunft (alle Felder `None`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobOrigin {
    /// Id des startenden Werkzeugaufrufs (`ToolCall::id`).
    pub call_id: Option<String>,
    /// Name des startenden Werkzeugs (z. B. `job.start`).
    pub tool: Option<String>,
    /// Anzeigename des besitzenden Agenten, falls bekannt.
    pub owner_agent: Option<String>,
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

/// Ausgang von [`JobManager::wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// Der Job ist beendet.
    Finished,
    /// Ein Meilenstein: neue Fehlermeldung, Fortschritt überschreitet eine
    /// 10-%-Stufe oder die Phase wechselt (z. B. cargo `Finished`).
    Milestone,
    /// Die Wartezeit lief ab.
    Timeout,
    /// Der Aufruf wurde abgebrochen.
    Cancelled,
}

impl WaitOutcome {
    /// Kurzname.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Milestone => "milestone",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }
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
}

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

    fn persist(&self) {
        let meta = self.meta();
        if let Err(err) = write_meta(&self.dir, &meta) {
            warn!(job_id = %meta.job_id, error = %err, "job meta.json not written");
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
    let text = serde_json::to_vec_pretty(meta).map_err(io::Error::other)?;
    let tmp = dir.join(format!("{META_FILE}.tmp"));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, dir.join(META_FILE))
}

fn read_meta(dir: &Path) -> Option<JobMeta> {
    let text = fs::read(dir.join(META_FILE)).ok()?;
    serde_json::from_slice(&text).ok()
}

/// Verwaltet die Prozess-Jobs einer harw-Sitzung.
pub struct JobManager {
    config: JobManagerConfig,
    instance: String,
    notifier: Arc<dyn JobNotifier>,
    jobs: Mutex<BTreeMap<JobId, Arc<JobEntry>>>,
    counter: AtomicU64,
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
        let jobs_dir = config.jobs_dir();
        fs::create_dir_all(&jobs_dir)?;
        let instance = format!(
            "harw-{}-{}",
            std::process::id(),
            Timestamp::now().as_millisecond()
        );
        let manager = Arc::new(Self {
            config,
            instance,
            notifier,
            jobs: Mutex::new(BTreeMap::new()),
            counter: AtomicU64::new(0),
        });
        manager.reload(&jobs_dir);
        Ok(manager)
    }

    /// Die Einstellungen.
    #[must_use]
    pub fn config(&self) -> &JobManagerConfig {
        &self.config
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
                let liveness = recovered_liveness(&meta);
                let next = if liveness == Liveness::Alive {
                    JobState::Detached
                } else {
                    JobState::Unknown
                };
                match (&liveness, meta.pid) {
                    (Liveness::Mismatch(reason), Some(pid)) => {
                        let message = if meta.process_identity().is_some() {
                            format!("PID {pid} reused, not controlled: {reason}")
                        } else {
                            format!("PID {pid} not controlled: {reason}")
                        };
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

    /// Zahl der Jobs, die diese Sitzung gerade beaufsichtigt.
    #[must_use]
    pub fn running_count(&self) -> usize {
        lock(&self.jobs)
            .values()
            .filter(|entry| {
                let state = lock(&entry.state);
                state.monitored && !state.meta.state.is_terminal()
            })
            .count()
    }

    /// Prüft vorab, ob noch ein Job starten darf (vor einer Host-Freigabe).
    ///
    /// # Errors
    /// [`JobError::Capacity`].
    pub fn check_capacity(&self) -> Result<(), JobError> {
        let max = self.config.max_running_jobs;
        if self.running_count() >= max {
            return Err(JobError::Capacity { max });
        }
        Ok(())
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
    pub fn start(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
    ) -> Result<JobStatus, JobError> {
        self.start_with_warnings(request, prepared, Vec::new())
    }

    /// Wie [`JobManager::start`], zusätzlich mit Hinweisen des Startwegs
    /// ([`crate::launcher::PreparedLaunch::warnings`]): sie landen in
    /// `meta.json` (`launch_warnings`, auch bei gescheitertem Start) und
    /// gehen nach `Started` je als [`JobEvent::Warning`] an den Besitzer.
    ///
    /// # Errors
    /// Wie [`JobManager::start`].
    pub fn start_with_warnings(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
        warnings: Vec<String>,
    ) -> Result<JobStatus, JobError> {
        self.start_with_origin(request, prepared, warnings, JobOrigin::default())
    }

    /// Wie [`JobManager::start_with_warnings`], zusätzlich mit der Herkunft
    /// ([`JobOrigin`]): sie landet in `meta.json` (`origin_call_id`,
    /// `origin_tool`, `owner_agent`) und im [`JobEvent::Started`].
    ///
    /// # Errors
    /// Wie [`JobManager::start`].
    pub fn start_with_origin(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
        warnings: Vec<String>,
        origin: JobOrigin,
    ) -> Result<JobStatus, JobError> {
        self.check_capacity()?;
        let (id, dir) = self.allocate()?;
        let stdout = create_log(&dir.join(STDOUT_LOG)).map_err(io_err("create stdout.log"))?;
        let stderr = create_log(&dir.join(STDERR_LOG)).map_err(io_err("create stderr.log"))?;

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
        };

        let mut command = prepared.command;
        command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(false);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                warn!(job_id = %id, error = %err, "job spawn failed");
                meta.state = JobState::Failed;
                meta.ended_at = Some(Timestamp::now());
                meta.launch_error = Some(err.to_string());
                let entry = Arc::new(JobEntry::new(dir, meta, false, None));
                entry.persist();
                lock(&self.jobs).insert(id, entry);
                return Err(JobError::Spawn(err.to_string()));
            }
        };

        let pid = child.id();
        let leader = adopt_child(&mut meta, pid);
        meta.state = JobState::Running;
        meta.started_at = Some(Timestamp::now());
        let entry = Arc::new(JobEntry::new(dir, meta.clone(), true, leader));
        entry.persist();
        lock(&self.jobs).insert(id.clone(), Arc::clone(&entry));
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
        let handle = tokio::spawn(monitor.run(child));
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
    pub fn start_piped(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
    ) -> Result<PipedJob, JobError> {
        self.start_piped_with_line_limit(request, prepared, DEFAULT_MAX_PIPED_LINE_BYTES)
    }

    /// Wie [`JobManager::start_piped`], aber mit eigener Höchstlänge einer
    /// stdout-Zeile (`max_line_bytes`, ohne `\n`). Eine längere Zeile wird
    /// nie über die Grenze hinaus gepuffert: das Mitschreib-Task hört auf zu
    /// lesen, vermerkt es in `STDOUT_LOG`, schickt
    /// [`PipedLineError::TooLong`] und schließt Strom und Pipe.
    ///
    /// # Errors
    /// Wie [`JobManager::start_piped`].
    pub fn start_piped_with_line_limit(
        self: &Arc<Self>,
        request: StartRequest,
        prepared: PreparedJob,
        max_line_bytes: usize,
    ) -> Result<PipedJob, JobError> {
        self.check_capacity()?;
        let (id, dir) = self.allocate()?;
        let stdout_log_path = dir.join(STDOUT_LOG);
        let stdout_log = create_log(&stdout_log_path).map_err(io_err("create stdout.log"))?;
        let stderr = create_log(&dir.join(STDERR_LOG)).map_err(io_err("create stderr.log"))?;

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
            origin_call_id: None,
            origin_tool: None,
            owner_agent: None,
        };

        let mut command = prepared.command;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(false);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                return Err(self.record_piped_launch_failure(dir, meta, err.to_string()));
            }
        };
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            // `Stdio::piped()` above always gives both back; unreachable in
            // practice, but a job that spawned without stdio it needs must
            // not linger.
            let _ = child.start_kill();
            return Err(self.record_piped_launch_failure(
                dir,
                meta,
                "child process is missing a stdio pipe".to_owned(),
            ));
        };

        let pid = child.id();
        let leader = adopt_child(&mut meta, pid);
        meta.state = JobState::Running;
        meta.started_at = Some(Timestamp::now());
        let entry = Arc::new(JobEntry::new(dir, meta.clone(), true, leader));
        entry.persist();
        lock(&self.jobs).insert(id.clone(), Arc::clone(&entry));
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
        let handle = tokio::spawn(monitor.run_piped(child, tee));
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
        refresh_foreign(&entry);
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
                refresh_foreign(&entry);
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
    /// verbliebene Gruppenmitglieder beendet.
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
        let entry = self.entry(id, caller)?;
        self.stop_entry(&entry, signal).await;
        Ok(entry.status())
    }

    async fn stop_entry(&self, entry: &Arc<JobEntry>, signal: JobSignal) {
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
                if !wait_detached_exit(entry, &meta, grace).await {
                    self.escalate_kill(entry, &meta, signal);
                    wait_detached_exit(entry, &meta, KILL_WAIT).await;
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
                state.meta.ended_at = Some(Timestamp::now());
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

    /// Wartet begrenzt auf das Ende oder den nächsten Meilenstein.
    ///
    /// # Errors
    /// [`JobError::NotFound`] für unbekannte oder fremde Jobs.
    pub async fn wait(
        &self,
        id: &JobId,
        caller: Caller<'_>,
        timeout: Duration,
        cancel: Option<&CancelToken>,
    ) -> Result<(WaitOutcome, JobStatus), JobError> {
        let entry = self.entry(id, caller)?;
        refresh_foreign(&entry);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut receiver = entry.changes.subscribe();
        let start_milestones = lock(&entry.state).milestones;
        let monitored = lock(&entry.state).monitored;
        loop {
            let (state, milestones) = {
                let state = lock(&entry.state);
                (state.meta.state, state.milestones)
            };
            if state.is_terminal() {
                return Ok((WaitOutcome::Finished, entry.status()));
            }
            if milestones > start_milestones {
                return Ok((WaitOutcome::Milestone, entry.status()));
            }
            if !monitored && state != JobState::Queued {
                // Kein Überwachungs-Task: Lebenszeichen abfragen.
                let step = tokio::time::Instant::now() + FOREIGN_POLL;
                let until = step.min(deadline);
                tokio::select! {
                    () = tokio::time::sleep_until(until) => {}
                    () = cancelled(cancel) => return Ok((WaitOutcome::Cancelled, entry.status())),
                }
                refresh_foreign(&entry);
                if tokio::time::Instant::now() >= deadline && !entry.state().is_terminal() {
                    return Ok((WaitOutcome::Timeout, entry.status()));
                }
                continue;
            }
            tokio::select! {
                changed = tokio::time::timeout_at(deadline, receiver.changed()) => {
                    match changed {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) | Err(_) => {
                            let outcome = if entry.state().is_terminal() {
                                WaitOutcome::Finished
                            } else {
                                WaitOutcome::Timeout
                            };
                            return Ok((outcome, entry.status()));
                        }
                    }
                }
                () = cancelled(cancel) => return Ok((WaitOutcome::Cancelled, entry.status())),
            }
        }
    }

    /// Löst alle laufenden Jobs von dieser Sitzung (harw beendet sich, die
    /// Nutzerin wählte „weiterlaufen lassen"). Host-Jobs laufen weiter und
    /// sind nach dem nächsten Start über `meta.json` sichtbar; Sandbox-Jobs
    /// enden trotzdem mit harw (`bwrap --die-with-parent`).
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
}

/// Aktualisiert einen nicht beaufsichtigten, als laufend geführten Job.
fn refresh_foreign(entry: &Arc<JobEntry>) {
    let changed = {
        let mut state = lock(&entry.state);
        if state.monitored || state.meta.state != JobState::Detached {
            return;
        }
        let alive = match &entry.leader {
            Some(leader) => !leader.has_exited(),
            None => recovered_liveness(&state.meta) == Liveness::Alive,
        };
        if alive {
            false
        } else {
            state.meta.state = JobState::Unknown;
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

/// Übernimmt das eben gestartete Kind `pid` (pidfd, Identität) und trägt
/// PID und Identität in `meta` ein.
fn adopt_child(meta: &mut JobMeta, pid: Option<u32>) -> Option<OwnLeader> {
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
    leader
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
/// seine PID nicht mehr ihm gehört).
fn detached_exited(entry: &JobEntry, meta: &JobMeta) -> bool {
    match &entry.leader {
        Some(leader) => leader.has_exited(),
        None => recovered_liveness(meta) != Liveness::Alive,
    }
}

/// Wartet höchstens `limit`, bis ein nicht beaufsichtigter Prozess weg ist.
async fn wait_detached_exit(entry: &JobEntry, meta: &JobMeta, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if detached_exited(entry, meta) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(FOREIGN_POLL).await;
    }
}

async fn cancelled(cancel: Option<&CancelToken>) {
    match cancel {
        Some(cancel) => cancel.cancelled().await,
        None => std::future::pending().await,
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
    async fn run(self, child: Child) {
        self.run_inner(child, None).await;
    }

    /// Wie [`Monitor::run`], aber wartet nach dem Prozessende zusätzlich auf
    /// `tee` (das Mitschreib-Task von [`JobManager::start_piped`]), damit die
    /// letzte Ausgabe sicher in `STDOUT_LOG` steht, bevor der Ende-Bericht
    /// den Tail liest.
    async fn run_piped(self, child: Child, tee: JoinHandle<()>) {
        self.run_inner(child, Some(tee)).await;
    }

    async fn run_inner(self, mut child: Child, tee: Option<JoinHandle<()>>) {
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
        // Wartende (`job.wait`) nur bei einem neuen Meilenstein wecken.
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
            state.meta.ended_at = Some(Timestamp::now());
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

#[cfg(test)]
mod error_display_tests {
    use super::{JobError, PipedLineError};
    use std::error::Error as _;

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
