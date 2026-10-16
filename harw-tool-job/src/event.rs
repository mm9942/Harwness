//! Job-Ereignisse und ihre Zustellung ([`JobNotifier`]).
//!
//! # Verantwortungsbereich
//! Die Job-Verwaltung erzeugt [`JobEvent`]s (gedrosselt, siehe
//! [`crate::throttle`]) und übergibt sie samt Besitzer als
//! [`JobNotification`] an genau einen [`JobNotifier`]. Die Montage
//! implementiert ihn: sie spritzt [`JobEvent::render_note`] als
//! Systemnotiz in den besitzenden Agenten ein — oder, wenn dieser schon
//! beendet ist, entlang [`crate::JobOwner::delivery_chain`] in den nächsten
//! lebenden Vorfahren (zuletzt UIA/TUI).
//!
//! # Nebenläufigkeit
//! [`JobNotifier::notify`] wird aus Überwachungs-Tasks aufgerufen und darf
//! nicht blockieren (z. B. in einen `mpsc`-Kanal schieben).

use crate::model::{JobId, JobOwner, JobState};
use crate::progress::ProgressSnapshot;
use serde::Serialize;
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Ein Ereignis eines Jobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobEvent {
    /// Der Prozess wurde gestartet.
    Started {
        /// Kennung.
        job_id: JobId,
        /// Anzeigename.
        name: String,
        /// Befehl.
        command: String,
        /// PID (= PGID).
        pid: Option<u32>,
        /// Direkt auf dem Host (ohne `bwrap`).
        executed_on_host: bool,
        /// R18 (TUI-07): `call_id` des startenden Werkzeugaufrufs, wie in
        /// [`crate::JobMeta::origin_call_id`]; `None` ohne Werkzeugaufruf.
        #[serde(skip_serializing_if = "Option::is_none")]
        origin_call_id: Option<String>,
        /// R18: Name des startenden Werkzeugs (z. B. `job.start`).
        #[serde(skip_serializing_if = "Option::is_none")]
        origin_tool: Option<String>,
        /// R18: Anzeigename des besitzenden Agenten, falls bekannt.
        #[serde(skip_serializing_if = "Option::is_none")]
        owner_agent: Option<String>,
    },
    /// Periodischer Fortschritt (nur bei Änderung).
    Progress {
        /// Kennung.
        job_id: JobId,
        /// Anzeigename.
        name: String,
        /// Laufzeit bisher.
        elapsed_secs: u64,
        /// Erkannter Fortschritt.
        progress: Option<ProgressSnapshot>,
        /// Ausgabezeilen bisher (stdout + stderr).
        lines: u64,
        /// Warnzeilen bisher.
        warnings: u64,
        /// Fehlerzeilen bisher.
        errors: u64,
        /// Letzte Ausgabezeile.
        last_line: Option<String>,
    },
    /// Erkannte Fehlerzeilen (entprellt gesammelt).
    ErrorLines {
        /// Kennung.
        job_id: JobId,
        /// Anzeigename.
        name: String,
        /// Mitgeschickte Zeilen.
        lines: Vec<String>,
        /// Fehlerzeilen in diesem Fenster (auch nicht mitgeschickte).
        in_batch: u64,
        /// Fehlerzeilen seit Start.
        total_errors: u64,
    },
    /// Hinweis zum Job selbst (z. B. SIGTERM ignoriert → SIGKILL).
    Warning {
        /// Kennung.
        job_id: JobId,
        /// Anzeigename.
        name: String,
        /// Meldung.
        message: String,
    },
    /// Der Job ist beendet (wird immer gemeldet).
    Finished {
        /// Kennung.
        job_id: JobId,
        /// Anzeigename.
        name: String,
        /// Endzustand.
        state: JobState,
        /// Exit-Code, falls bekannt.
        exit_code: Option<i32>,
        /// Beendendes Signal, falls bekannt.
        signal: Option<i32>,
        /// Laufzeit.
        duration_secs: u64,
        /// Letzte Ausgabezeilen.
        tail: Vec<String>,
    },
}

impl JobEvent {
    /// Die Kennung des betroffenen Jobs.
    #[must_use]
    pub fn job_id(&self) -> &JobId {
        match self {
            Self::Started { job_id, .. }
            | Self::Progress { job_id, .. }
            | Self::ErrorLines { job_id, .. }
            | Self::Warning { job_id, .. }
            | Self::Finished { job_id, .. } => job_id,
        }
    }

    /// `true` für [`JobEvent::Finished`].
    #[must_use]
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }

    /// Text der Systemnotiz, die der besitzende Agent erhält.
    ///
    /// # Examples
    /// ```rust
    /// use harw_tool_job::{JobEvent, JobId, JobState};
    ///
    /// # fn main() -> Result<(), String> {
    /// let job_id = JobId::parse("job-1").ok_or("invalid id")?;
    /// let note = JobEvent::Finished {
    ///     job_id,
    ///     name: "build".into(),
    ///     state: JobState::Failed,
    ///     exit_code: Some(3),
    ///     signal: None,
    ///     duration_secs: 12,
    ///     tail: vec!["boom".into()],
    /// }
    /// .render_note();
    /// assert!(note.contains("exit code 3"));
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn render_note(&self) -> String {
        match self {
            Self::Started {
                job_id,
                name,
                command,
                pid,
                executed_on_host,
                ..
            } => {
                let place = if *executed_on_host { "host" } else { "sandbox" };
                let pid = pid.map_or_else(|| "?".to_owned(), |pid| pid.to_string());
                format!(
                    "[job {job_id} \"{name}\"] started ({place}, pid {pid}): {command}. You will get \
                     progress notes and a note when it ends; use job.status / job.logs \
                     instead of polling."
                )
            }
            Self::Progress {
                job_id,
                name,
                elapsed_secs,
                progress,
                lines,
                warnings,
                errors,
                last_line,
            } => {
                let mut note = format!(
                    "[job {job_id} \"{name}\"] running {}: ",
                    format_duration(*elapsed_secs)
                );
                match progress {
                    Some(progress) => note.push_str(&progress.render()),
                    None => note.push_str("no recognised progress"),
                }
                note.push_str(&format!(
                    "; {lines} lines, {warnings} warnings, {errors} errors"
                ));
                if let Some(last) = last_line {
                    note.push_str(&format!("; last line: {last}"));
                }
                note
            }
            Self::ErrorLines {
                job_id,
                name,
                lines,
                in_batch,
                total_errors,
            } => {
                let mut note = format!(
                    "[job {job_id} \"{name}\"] {in_batch} error line(s) ({total_errors} total), \
                     still running:"
                );
                for line in lines {
                    note.push_str("\n  ");
                    note.push_str(line);
                }
                note
            }
            Self::Warning {
                job_id,
                name,
                message,
            } => format!("[job {job_id} \"{name}\"] {message}"),
            Self::Finished {
                job_id,
                name,
                state,
                exit_code,
                signal,
                duration_secs,
                tail,
            } => {
                let mut note = format!(
                    "[job {job_id} \"{name}\"] finished: {state} after {}",
                    format_duration(*duration_secs)
                );
                if let Some(code) = exit_code {
                    note.push_str(&format!(", exit code {code}"));
                }
                if let Some(signal) = signal {
                    note.push_str(&format!(", signal {signal}"));
                }
                if tail.is_empty() {
                    note.push_str(" (no output)");
                } else {
                    note.push_str(&format!(". Last {} lines:", tail.len()));
                    for line in tail {
                        note.push_str("\n  ");
                        note.push_str(line);
                    }
                }
                note
            }
        }
    }
}

/// `75` → `1m15s`, `3725` → `1h02m05s`.
#[must_use]
pub fn format_duration(secs: u64) -> String {
    let (hours, minutes, seconds) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m{seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// Ein Ereignis samt Besitzer, an den es zuzustellen ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobNotification {
    /// Besitzer (Erzeuger zuerst, dann Vorfahren).
    pub owner: JobOwner,
    /// Das Ereignis.
    pub event: JobEvent,
}

/// Zustellung von Job-Ereignissen an Agenten (Montage) bzw. Oberfläche.
pub trait JobNotifier: Send + Sync {
    /// Stellt eine Meldung zu. Darf nicht blockieren.
    fn notify(&self, notification: JobNotification);
}

/// Verwirft jede Meldung (Vorgabe ohne Montage).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopNotifier;

impl JobNotifier for NoopNotifier {
    fn notify(&self, _notification: JobNotification) {}
}

/// Schiebt jede Meldung in einen ungebundenen `mpsc`-Kanal (für Montage und
/// TUI: ein Task liest und leitet weiter).
#[derive(Debug, Clone)]
pub struct ChannelNotifier {
    sender: mpsc::UnboundedSender<JobNotification>,
}

impl ChannelNotifier {
    /// Baut Notifier und Empfangsseite.
    #[must_use]
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<JobNotification>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self { sender }, receiver)
    }
}

impl JobNotifier for ChannelNotifier {
    fn notify(&self, notification: JobNotification) {
        if self.sender.send(notification).is_err() {
            tracing::debug!("job notification dropped: receiver closed");
        }
    }
}

/// Merkt sich jede Meldung (Tests, Diagnose).
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    events: Mutex<Vec<JobNotification>>,
}

impl RecordingNotifier {
    /// Alle bisher zugestellten Meldungen.
    #[must_use]
    pub fn notifications(&self) -> Vec<JobNotification> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl JobNotifier for RecordingNotifier {
    fn notify(&self, notification: JobNotification) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(notification);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(5), "5s");
        assert_eq!(format_duration(75), "1m15s");
        assert_eq!(format_duration(3725), "1h02m05s");
    }

    #[test]
    fn test_render_error_and_finished_notes() -> TestResult {
        let job_id = JobId::parse("job-x").ok_or(TestError::Missing("job id"))?;
        let note = JobEvent::ErrorLines {
            job_id: job_id.clone(),
            name: "build".into(),
            lines: vec!["error: a".into()],
            in_batch: 4,
            total_errors: 9,
        }
        .render_note();
        assert!(note.contains("4 error line(s) (9 total)"));
        assert!(note.contains("error: a"));
        let finished = JobEvent::Finished {
            job_id,
            name: "build".into(),
            state: JobState::Succeeded,
            exit_code: Some(0),
            signal: None,
            duration_secs: 61,
            tail: Vec::new(),
        };
        assert!(finished.is_finished());
        assert!(
            finished
                .render_note()
                .contains("succeeded after 1m01s, exit code 0 (no output)")
        );
        Ok(())
    }

    /// R18 (TUI-07): `Started` trägt die Herkunft; ohne Herkunft bleibt die
    /// serialisierte Form die alte, die Agenten-Notiz ändert sich nicht.
    #[test]
    fn test_started_carries_origin_additively() -> TestResult {
        let job_id = JobId::parse("job-o").ok_or(TestError::Missing("job id"))?;
        let started = |origin_call_id: Option<&str>| JobEvent::Started {
            job_id: job_id.clone(),
            name: "build".into(),
            command: "make".into(),
            pid: Some(7),
            executed_on_host: false,
            origin_call_id: origin_call_id.map(str::to_owned),
            origin_tool: origin_call_id.map(|_| "job.start".to_owned()),
            owner_agent: None,
        };
        let plain = serde_json::to_value(started(None))
            .map_err(|err| TestError::Unexpected(err.to_string()))?;
        assert!(plain.get("origin_call_id").is_none(), "{plain}");
        assert!(plain.get("owner_agent").is_none(), "{plain}");
        let linked = serde_json::to_value(started(Some("call-1")))
            .map_err(|err| TestError::Unexpected(err.to_string()))?;
        assert_eq!(linked["origin_call_id"], "call-1");
        assert_eq!(linked["origin_tool"], "job.start");
        assert_eq!(
            started(Some("call-1")).render_note(),
            started(None).render_note()
        );
        Ok(())
    }
}
