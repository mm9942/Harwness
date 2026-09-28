//! Plan R9, Teil F: Job-Verwaltung der harw-Sitzung und Zustellung ihrer
//! Ereignisse an die Agenten (`harw-tool-job`).
//!
//! # Verantwortungsbereich
//! - [`SessionJobs`]: genau eine [`JobManager`] je Montage (Wurzel und alle
//!   Kinder teilen sie), Logs unter `<projekt>/.harw/state/jobs/<job_id>/`.
//! - [`JobEventRouter`]: Zustellung jedes [`JobNotification`] als
//!   Systemnotiz an die **erste noch lebende** Sitzung entlang
//!   [`harw_tool_job::JobOwner::delivery_chain`] — ein laufendes Kind über
//!   sein Postfach ([`ChildComms::deliver_note_to_running_child`], gelesen an
//!   seiner nächsten Runden-Grenze), die Wurzel über die Notizen-Warteschlange,
//!   die die TUI abholt ([`JobEventRouter::take_root_notes`]). Endet die Kette
//!   ohne lebenden Empfänger, landet die Notiz bei der Wurzel (UIA). Jedes
//!   Ereignis erscheint zusätzlich in [`JobEventRouter::take_ui_events`] und
//!   als Invalidierungssignal `Knowledge { area: "jobs" }` auf dem
//!   Agenten-Bus (Jobs-Gruppe des Agenten-Panels).
//! - [`JobEventRouter::lineage`]: die Elternkette einer Sitzung über die
//!   Journale des Spawners (Besitz: Erzeuger plus Vorfahren).
//!
//! # Nebenläufigkeit
//! Der [`harw_tool_job::JobNotifier`] der Verwaltung ist ein
//! [`ChannelNotifier`] (blockiert nie); ein Tokio-Task
//! ([`spawn_forwarder`]) liest den Kanal und ruft [`JobEventRouter::route`].
//! Spawner-Kommunikation und Wurzel-Id werden nach dem Bau des Spawners über
//! [`JobEventRouter::bind`] nachgetragen; bis dahin gepufferte Ereignisse
//! bleiben im Kanal. Alle Sperren sind Blätter und werden nie über ein
//! `await` gehalten.

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use harw_core::{AgentEventHub, ChildComms};
use harw_tool_job::{
    ChannelNotifier, FnLineage, JobEvent, JobLineage, JobManager, JobManagerConfig, JobNotification,
};
use harw_types::SessionId;
use tokio::sync::{Notify, mpsc};

/// Höchstzahl gepufferter Notizen an die Wurzel bzw. Oberflächen-Ereignisse
/// (älteste fallen zuerst heraus).
pub const JOB_EVENT_BUFFER: usize = 256;

/// Höchstzahl Schritte beim Ablaufen der Elternkette (Schutz gegen Zyklen).
const MAX_LINEAGE_HOPS: usize = 64;

/// Bereich des Invalidierungssignals auf dem Agenten-Bus.
pub const JOBS_EVENT_AREA: &str = "jobs";

/// Wohin ein Job-Ereignis zugestellt wurde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobDelivery {
    /// In das Postfach dieses laufenden Kindes.
    Child(String),
    /// An die Wurzel (UIA): Besitzer, Vorfahr oder letzter Ausweg.
    Root,
}

/// Zustellung von Job-Ereignissen (siehe Moduldoku).
#[derive(Debug, Default)]
pub struct JobEventRouter {
    comms: OnceLock<Arc<ChildComms>>,
    root: OnceLock<SessionId>,
    agent_events: OnceLock<AgentEventHub>,
    root_notes: Mutex<VecDeque<JobNotification>>,
    ui_events: Mutex<VecDeque<JobNotification>>,
    wake: Notify,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl JobEventRouter {
    /// Ein ungebundener Router (Wurzel und Spawner kommen über
    /// [`Self::bind`]).
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Trägt die Kommunikation des Spawners (optional: ohne Spawner gibt es
    /// keine Kinder) und die Wurzel-Sitzung nach. Ein zweiter Aufruf ändert
    /// nichts.
    pub fn bind(&self, comms: Option<Arc<ChildComms>>, root: SessionId) {
        if let Some(comms) = comms {
            let _ = self.comms.set(comms);
        }
        let _ = self.root.set(root);
    }

    /// Hängt den Agenten-Bus an (Invalidierungssignal für die Oberfläche).
    pub fn attach_agent_events(&self, hub: AgentEventHub) {
        let _ = self.agent_events.set(hub);
    }

    /// Die Wurzel-Sitzung, sobald gebunden.
    #[must_use]
    pub fn root(&self) -> Option<&SessionId> {
        self.root.get()
    }

    /// Vorfahren von `session`: Elternteil, Großelternteil, …, Wurzel.
    ///
    /// # Beschreibung
    /// Läuft die Journale des Spawners ([`ChildComms::parent_of`]) nach oben,
    /// bis eine Sitzung ohne Journal (die Wurzel) erreicht ist. Ohne
    /// gebundenen Spawner: nur die Wurzel (falls `session` nicht selbst die
    /// Wurzel ist).
    #[must_use]
    pub fn ancestors(&self, session: &SessionId) -> Vec<String> {
        let mut chain: Vec<String> = Vec::new();
        let root = self.root.get();
        if root == Some(session) {
            return chain;
        }
        let mut cursor = session.clone();
        if let Some(comms) = self.comms.get() {
            while chain.len() < MAX_LINEAGE_HOPS {
                let Some(parent) = comms.parent_of(&cursor) else {
                    break;
                };
                if chain.iter().any(|seen| seen == parent.as_str()) {
                    break;
                }
                chain.push(parent.as_str().to_owned());
                if root == Some(&parent) {
                    return chain;
                }
                cursor = parent;
            }
        }
        if let Some(root) = root
            && !chain.iter().any(|seen| seen == root.as_str())
        {
            chain.push(root.as_str().to_owned());
        }
        chain
    }

    /// Die Elternkette als [`JobLineage`] für `job_tools`.
    #[must_use]
    pub fn lineage(self: &Arc<Self>) -> Arc<dyn JobLineage> {
        let router = Arc::clone(self);
        Arc::new(FnLineage(move |session: &SessionId| {
            router.ancestors(session)
        }))
    }

    /// Stellt eine Meldung zu (siehe Moduldoku) und meldet sie der
    /// Oberfläche.
    ///
    /// # Returns
    /// Den tatsächlichen Empfänger.
    pub fn route(&self, notification: JobNotification) -> JobDelivery {
        let note = notification.event.render_note();
        let root = self.root.get();
        let mut delivery = JobDelivery::Root;
        for session in notification.owner.delivery_chain() {
            if root.is_some_and(|root| root.as_str() == session) {
                break;
            }
            let Some(comms) = self.comms.get() else {
                continue;
            };
            let Ok(id) = SessionId::try_from_str(session) else {
                continue;
            };
            if comms.deliver_note_to_running_child(&id, &note) {
                delivery = JobDelivery::Child(session.to_owned());
                break;
            }
        }
        tracing::info!(
            job_id = %notification.event.job_id(),
            owner = %notification.owner.session,
            delivered_to = ?delivery,
            finished = notification.event.is_finished(),
            "runtime.jobs.event_routed"
        );
        if delivery == JobDelivery::Root {
            push_root_note(&mut lock(&self.root_notes), notification.clone());
        }
        {
            let mut ui = lock(&self.ui_events);
            if ui.len() >= JOB_EVENT_BUFFER {
                ui.pop_front();
            }
            ui.push_back(notification.clone());
        }
        if let Some(hub) = self.agent_events.get() {
            let agent = root.cloned().unwrap_or_else(SessionId::new);
            hub.publish_knowledge(
                agent,
                JOBS_EVENT_AREA,
                Some(notification.event.job_id().to_string()),
            );
        }
        // `notify_one` hinterlegt eine Erlaubnis, falls die Oberfläche gerade
        // nicht wartet — kein Ereignis geht zwischen Abholen und Warten
        // verloren.
        self.wake.notify_one();
        delivery
    }

    /// Entnimmt die Notizen an die Wurzel (älteste zuerst). Fortschritt
    /// desselben Jobs ist zusammengefasst: nur die jüngste
    /// Fortschrittsnotiz bleibt stehen.
    #[must_use]
    pub fn take_root_notes(&self) -> Vec<JobNotification> {
        lock(&self.root_notes).drain(..).collect()
    }

    /// Entnimmt alle Ereignisse für die Oberfläche (älteste zuerst).
    #[must_use]
    pub fn take_ui_events(&self) -> Vec<JobNotification> {
        lock(&self.ui_events).drain(..).collect()
    }

    /// Wartet auf das nächste zugestellte Ereignis.
    pub async fn notified(&self) {
        self.wake.notified().await;
    }
}

/// Reiht eine Wurzel-Notiz ein; eine noch nicht abgeholte
/// Fortschrittsnotiz desselben Jobs wird ersetzt.
fn push_root_note(queue: &mut VecDeque<JobNotification>, notification: JobNotification) {
    if matches!(notification.event, JobEvent::Progress { .. }) {
        let job_id = notification.event.job_id().clone();
        queue.retain(|queued| {
            !(matches!(queued.event, JobEvent::Progress { .. }) && *queued.event.job_id() == job_id)
        });
    }
    if queue.len() >= JOB_EVENT_BUFFER {
        queue.pop_front();
    }
    queue.push_back(notification);
}

/// Die Job-Verwaltung einer Montage samt Zustellung.
#[derive(Debug, Clone)]
pub struct SessionJobs {
    /// Die Job-Verwaltung (Wurzel und Kinder teilen sie).
    pub manager: Arc<JobManager>,
    /// Die Zustellung ihrer Ereignisse.
    pub router: Arc<JobEventRouter>,
}

impl SessionJobs {
    /// Legt `<state_dir>/jobs` an, lädt frühere Jobs und verbindet die
    /// Verwaltung über einen [`ChannelNotifier`] mit einem neuen Router.
    ///
    /// # Returns
    /// Die Verwaltung und die Empfangsseite ihrer Meldungen; der Aufrufer
    /// startet [`spawn_forwarder`], sobald eine Tokio-Laufzeit läuft.
    ///
    /// `max_running` ist `[jobs] max_running` (siehe
    /// `harw_config::harness_config::JobsToml::effective_max_running`) und
    /// wird zu `JobManagerConfig::max_running_jobs`.
    ///
    /// # Errors
    /// Wenn das Job-Verzeichnis nicht angelegt werden kann.
    pub fn open(
        state_dir: &Path,
        max_running: u32,
    ) -> io::Result<(Self, mpsc::UnboundedReceiver<JobNotification>)> {
        let (notifier, receiver) = ChannelNotifier::channel();
        let mut config = JobManagerConfig::new(state_dir);
        // `[jobs] max_running` (1–256, beim Parsen geprüft); defensiv geklemmt.
        config.max_running_jobs = usize::try_from(max_running.clamp(1, 256)).unwrap_or(16);
        let manager = JobManager::new(config, Arc::new(notifier))?;
        Ok((
            Self {
                manager,
                router: JobEventRouter::new(),
            },
            receiver,
        ))
    }

    /// Die Verdrahtung für die Registry-Montage (`job.*`).
    #[must_use]
    pub fn wiring(&self) -> harw_registry_defaults::JobWiring {
        harw_registry_defaults::JobWiring::new(Arc::clone(&self.manager), self.router.lineage())
    }
}

/// Startet den Weiterleitungs-Task (Kanal → [`JobEventRouter::route`]).
///
/// # Returns
/// `false` ohne laufende Tokio-Laufzeit (dann bleibt der Kanal ungelesen;
/// Job-Werkzeuge funktionieren weiter, nur ohne Notizen).
pub fn spawn_forwarder(
    router: Arc<JobEventRouter>,
    mut receiver: mpsc::UnboundedReceiver<JobNotification>,
) -> bool {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                while let Some(notification) = receiver.recv().await {
                    router.route(notification);
                }
                tracing::debug!("runtime.jobs.forwarder_closed");
            });
            true
        }
        Err(error) => {
            tracing::debug!(error = %error, "runtime.jobs.no_tokio_runtime_skipping_forwarder");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_tool_job::{JobId, JobOwner, JobState};

    fn job_id(raw: &str) -> TestResult<JobId> {
        JobId::parse(raw).ok_or(TestError::Missing("gültige Job-Id"))
    }

    fn finished(job: &str) -> TestResult<JobEvent> {
        Ok(JobEvent::Finished {
            job_id: job_id(job)?,
            name: "build".to_owned(),
            state: JobState::Succeeded,
            exit_code: Some(0),
            signal: None,
            duration_secs: 3,
            tail: vec!["done".to_owned()],
        })
    }

    fn progress(job: &str, lines: u64) -> TestResult<JobEvent> {
        Ok(JobEvent::Progress {
            job_id: job_id(job)?,
            name: "build".to_owned(),
            elapsed_secs: 60,
            progress: None,
            lines,
            warnings: 0,
            errors: 0,
            last_line: None,
        })
    }

    /// Plan R9, Teil F: die Notiz eines beendeten Jobs erreicht das
    /// besitzende Kind; nach dessen Ende den Elternteil; nach dessen Ende
    /// die Wurzel — und jedes Ereignis erscheint für die Oberfläche und auf
    /// dem Agenten-Bus.
    #[test]
    fn finished_job_note_reaches_owner_then_parent_then_root_and_the_ui() -> TestResult {
        let comms = Arc::new(ChildComms::default());
        let root = SessionId::new();
        let orchestrator = SessionId::new();
        let worker = SessionId::new();
        comms.open_journal(&orchestrator, &root, "coding-orchestrator", None);
        comms.open_journal(&worker, &orchestrator, "executor", None);
        let hub = AgentEventHub::default();
        let mut bus = hub.subscribe();
        let router = JobEventRouter::new();
        router.bind(Some(Arc::clone(&comms)), root.clone());
        router.attach_agent_events(hub);

        // Besitz: Erzeuger plus Vorfahren bis zur Wurzel.
        let ancestors = router.ancestors(&worker);
        assert_eq!(
            ancestors,
            vec![orchestrator.as_str().to_owned(), root.as_str().to_owned()]
        );
        assert!(router.ancestors(&root).is_empty());
        let owner = JobOwner::new(worker.as_str(), ancestors);

        let event = finished("job-1")?;
        let note = event.render_note();
        let notification = JobNotification {
            owner: owner.clone(),
            event,
        };

        // 1. Der Worker läuft: sein Postfach.
        assert_eq!(
            router.route(notification.clone()),
            JobDelivery::Child(worker.as_str().to_owned())
        );
        assert_eq!(comms.take_inbound(&worker), vec![note.clone()]);

        // 2. Der Worker ist beendet: der Orchestrator.
        comms.close_journal(&worker);
        assert_eq!(
            router.route(notification.clone()),
            JobDelivery::Child(orchestrator.as_str().to_owned())
        );
        assert_eq!(comms.take_inbound(&orchestrator), vec![note.clone()]);
        assert!(router.take_root_notes().is_empty());

        // 3. Beide beendet: die Wurzel (UIA/TUI).
        comms.close_journal(&orchestrator);
        assert_eq!(router.route(notification), JobDelivery::Root);
        let root_notes = router.take_root_notes();
        assert_eq!(root_notes.len(), 1);
        assert_eq!(root_notes[0].event.render_note(), note);

        // Die Oberfläche sieht jedes der drei Ereignisse …
        assert_eq!(router.take_ui_events().len(), 3);
        // … und der Agenten-Bus trägt das Invalidierungssignal.
        let mut signals = 0;
        while let Ok(event) = bus.try_recv() {
            if let harw_core::AgentEventKind::Knowledge { area, id } = event.kind {
                assert_eq!(area, JOBS_EVENT_AREA);
                assert_eq!(id.as_deref(), Some("job-1"));
                signals += 1;
            }
        }
        assert_eq!(signals, 3);
        Ok(())
    }

    /// Fortschritt an die Wurzel wird je Job zusammengefasst.
    #[test]
    fn root_progress_notes_are_coalesced_per_job() -> TestResult {
        let router = JobEventRouter::new();
        let root = SessionId::new();
        router.bind(None, root.clone());
        let owner = JobOwner::new(root.as_str(), Vec::new());
        for (job, lines) in [("job-a", 1), ("job-b", 1), ("job-a", 2)] {
            let event = progress(job, lines)?;
            assert_eq!(
                router.route(JobNotification {
                    owner: owner.clone(),
                    event,
                }),
                JobDelivery::Root
            );
        }
        let notes = router.take_root_notes();
        assert_eq!(notes.len(), 2);
        assert!(matches!(
            notes[1].event,
            JobEvent::Progress { lines: 2, .. }
        ));
        Ok(())
    }

    /// Ohne gebundenen Spawner endet die Kette bei der Wurzel.
    #[test]
    fn unbound_router_delivers_to_the_root() -> TestResult {
        let router = JobEventRouter::new();
        let event = finished("job-x")?;
        let owner = JobOwner::new(SessionId::new().as_str(), Vec::new());
        assert_eq!(
            router.route(JobNotification { owner, event }),
            JobDelivery::Root
        );
        assert_eq!(router.take_root_notes().len(), 1);
        Ok(())
    }

    /// `[jobs] max_running` landet unverändert in
    /// `JobManagerConfig::max_running_jobs`.
    #[test]
    fn session_jobs_open_applies_configured_max_running() -> TestResult {
        let dir = tempfile::tempdir()?;
        let (jobs, _rx) = SessionJobs::open(dir.path(), 3)?;
        assert_eq!(jobs.manager.config().max_running_jobs, 3);
        Ok(())
    }

    /// Werte außerhalb von 1..=256 werden defensiv geklemmt (die eigentliche
    /// Prüfung liegt beim Parsen von `[jobs] max_running`).
    #[test]
    fn session_jobs_open_clamps_out_of_range_max_running() -> TestResult {
        let dir_low = tempfile::tempdir()?;
        let (jobs_low, _rx_low) = SessionJobs::open(dir_low.path(), 0)?;
        assert_eq!(jobs_low.manager.config().max_running_jobs, 1);

        let dir_high = tempfile::tempdir()?;
        let (jobs_high, _rx_high) = SessionJobs::open(dir_high.path(), 1000)?;
        assert_eq!(jobs_high.manager.config().max_running_jobs, 256);
        Ok(())
    }
}
