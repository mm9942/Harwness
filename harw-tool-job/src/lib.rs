//! `harw-tool-job` — eingebautes Job-System für lang laufende Prozesse
//! (Plan R9, Teil F): Builds, Paket-Restores, Testläufe laufen als Job statt
//! in `tmux`, und der besitzende Agent erfährt zuverlässig, was läuft, wie
//! weit es ist und wie es endete.
//!
//! # Werkzeuge ([`JobToolProvider`])
//! - `job.start {command|argv, cwd?, name, env?, notify_every_secs?}` → `job_id`
//! - `job.status {job_id}`
//! - `job.logs {job_id, stream?, tail?, since_line?, grep?}`
//! - `job.stop {job_id, signal?}` — TERM (bzw. INT/HUP/KILL) an die ganze
//!   Prozessgruppe, nach der Gnadenfrist SIGKILL (kein `ExecuteProcess`
//!   nötig: nur eigene Jobs bzw. die der Nachfahren)
//! - `job.list {kind?}` — ohne `kind` nur die Übersicht je Art mit Zählern
//!   pro Zustand; mit `kind` die Zeilen dieser Art
//! - `job.wait {job_id, timeout_secs}` — kurzes Abfragen (höchstens
//!   [`MAX_WAIT_SECS`] = 60 s, R18 F8) bis Ende oder Meilenstein; das Ende
//!   eines Jobs kommt ohnehin als Notiz (siehe „Meldungen“), Agenten
//!   blockieren also nicht in `job.wait`-Schleifen
//!
//! # Rechte
//! `job.start` läuft über denselben Weg wie `shell.exec`
//! ([`ShellJobLauncher`] → `harw_tool_shell::ShellToolProvider::prepare_background_launch`):
//! sudo-Verbot, `ExecuteProcess`, Sandbox/Host-Lease/Permit. Steuern dürfen
//! nur Erzeuger und Vorfahren ([`JobOwner`], [`JobLineage`]).
//!
//! # Ablage
//! `<state>/jobs/<job_id>/{stdout.log, stderr.log, meta.json}`; Logs und
//! Metadaten überleben das Ende des Agenten und von harw. Beim nächsten
//! Start lädt [`JobManager::new`] frühere Jobs (`detached`, wenn der Prozess
//! noch lebt **und** seine Identität passt, sonst `unknown`; eine
//! wiederverwendete PID wird nie signalisiert).
//!
//! # Meldungen
//! [`JobEvent`]s (Start, gedrosselter Fortschritt, entprellte Fehlerzeilen,
//! Hinweise, Ende mit Exit-Code/Dauer/letzten 20 Zeilen) gehen an einen
//! [`JobNotifier`], den die Montage implementiert. Die Montage stellt das
//! Ende zu wie eine Nachricht: in das Postfach des laufenden Besitzers
//! (gelesen an seiner nächsten Runden-Grenze) bzw. an die Wurzel, wo es im
//! Leerlauf einen Auto-Turn auslöst (`harw-runtime` `job_wiring`).
//!
//! # Herkunft
//! [`JobOrigin`] (Aufruf-Id und Werkzeug des startenden Aufrufs) steht in
//! `meta.json` und im `Started`-Ereignis; `job.start` setzt sie aus dem
//! Werkzeugaufruf.
//!
//! # Module
//! - [`model`] — [`JobId`], [`JobState`], [`JobOwner`], [`JobMeta`], [`JobStatus`]
//! - [`progress`] — reine Fortschritts-/Fehlererkennung
//! - [`throttle`] — reine Drosselung der Meldungen
//! - [`event`] — [`JobEvent`], [`JobNotifier`] und Standard-Notifier
//! - [`logs`] — Mitlesen und begrenzte Abfragen der Logdateien
//! - [`procfs`] — [`JobSignal`]; intern Prozessidentität und Signale an
//!   Prozessgruppen (Linux über `harw-job-linux`: pidfd, Wiederherstellungs-
//!   identität)
//! - [`launcher`] — [`JobLauncher`], [`ShellJobLauncher`]
//! - [`manager`] — [`JobManager`]
//! - [`tools`] — [`JobToolProvider`]
//!
//! Keine `tmux`-Abhängigkeit.

#![forbid(unsafe_code)]

pub mod event;
pub mod launcher;
pub mod logs;
pub mod manager;
pub mod model;
pub mod procfs;
pub mod progress;
pub mod throttle;
pub mod tools;

pub use event::{
    ChannelNotifier, JobEvent, JobNotification, JobNotifier, NoopNotifier, RecordingNotifier,
    format_duration,
};
pub use launcher::{JobLauncher, LaunchFuture, PreparedJob, ShellJobLauncher};
pub use logs::{LogQuery, LogSlice};
pub use manager::{
    Caller, DEFAULT_MAX_PIPED_LINE_BYTES, DetachSummary, JobError, JobManager, JobManagerConfig,
    JobOrigin, PipedJob, PipedLineError, StartRequest, WaitOutcome,
};
pub use model::{JobId, JobMeta, JobOwner, JobProcessIdentity, JobState, JobStatus};
pub use procfs::JobSignal;
pub use progress::{
    ProgressSnapshot, ProgressSource, ProgressTracker, ProgressUpdate, Severity, detect_progress,
    detect_severity,
};
pub use throttle::{ErrorBatch, NotifyThrottle, ProgressKey, ThrottleConfig};
pub use tools::{
    FnLineage, JOB_CONTROL_TOOLS, JOB_LIST_TOOL, JOB_LOGS_TOOL, JOB_READ_TOOLS, JOB_START_TOOL,
    JOB_STATUS_TOOL, JOB_STOP_TOOL, JOB_TOOL_NAMES, JOB_WAIT_TOOL, JobLineage, JobToolProvider,
    MAX_WAIT_SECS, NoLineage, job_start_command_text, job_tools, shell_quote,
};

#[cfg(test)]
mod test_support;
