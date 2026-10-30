//! Fester Not-Aus: **2× Ctrl+C beendet sofort die gesamte KI-Arbeit**.
//!
//! # Verantwortung
//! Zwei Ctrl+C-Drücke innerhalb von [`DOUBLE_PRESS_WINDOW`] lösen — in jedem
//! UI-Zustand, auch bei offenem Dialog, Overlay oder hängendem Async-Loop —
//! den harten Abbruch aus (PL-97 C4b), **sofern KI-Arbeit läuft** (siehe
//! „Nur bei laufender Arbeit"):
//!
//! 1. **Kooperativ, sofort**: alle Hintergrund-Agenten der Wurzelsitzung
//!    werden abgebrochen. Den laufenden Turn (samt Token-Baum seiner
//!    synchronen Kind-Agenten) hat schon der erste Druck in der
//!    Event-Schleife kooperativ abgebrochen.
//! 2. **Jobs über den Job-Pfad**: jeder laufende verwaltete Job — auch der
//!    Prozess eines job-gebundenen Kind-Agenten — wird mit
//!    `JobManager::stop(…, JobSignal::Kill)` beendet. Der Job-Pfad kennt
//!    Prozessgruppe, pidfd-Identität und cgroup des Jobs (E1-Identitäten);
//!    der rohe Prozessbaum wird für Jobs **nie** betreten.
//!    Nach dem ersten Durchgang wird erneut gelistet: ein Job, der während
//!    des Abbruchs gestartet wurde, wird ebenfalls gestoppt.
//! 3. **Rückfallebene**: alle übrigen Nachkommen von harw in **derselben**
//!    cgroup werden per `SIGKILL` beendet ([`crate::process_tree`]) — Prozesse
//!    ohne Job-Identität, die nicht auf einen Abbruch hören oder deren
//!    Elternprozess schon tot ist. Job-Teilbäume der `JobManager`-Jobs bleiben
//!    ausgespart; Nachkommen in einer Versuchs-cgroup der Job-Laufzeit
//!    (`harw-job-<attempt>`, z. B. `shell.exec` über den Laufzeit-Port) enden
//!    über das `cgroup.kill` ihrer cgroup; andere fremde cgroups bleiben
//!    unberührt.
//! 4. **Geordneter Abgang mit Frist**: der Not-Aus weckt
//!    [`HardKill::tripped`]; die Composition-Root verlässt darauf die
//!    Event-Schleife in jedem Zustand (auch mitten im Turn oder bei offenem
//!    Dialog), stellt das Terminal zurück, speichert die Sitzung und beendet
//!    mit Exit-Code 130 ([`HardKill::exit_if_tripped`]). Hängt die
//!    Async-Laufzeit, erzwingt ein Wächter-Thread nach [`EXIT_GRACE`] die
//!    Terminal-Wiederherstellung und `exit(130)` — dann ohne gespeicherte
//!    Sitzung (best effort). Vor jedem Exit kehrt ein letzter Durchgang
//!    ([`HardKill::final_sweep`]) Jobs und Prozesse nach, die im Exit-Fenster
//!    noch entstanden sind.
//!
//! # Nur bei laufender Arbeit
//! Läuft weder ein Turn noch ein verwalteter Job, ist 2× Ctrl+C das normale
//! Beenden der Event-Schleife (Exit-Code 0, Jobs-Ablösevertrag von
//! `jobs_glue`) — der Detektor löst dann nicht aus. Maßgeblich ist der Stand
//! beim **ersten** Druck oder beim zweiten: der erste Druck bricht den Turn
//! kooperativ ab, der zweite muss dessen Reste trotzdem erreichen. Den
//! Turn-Zustand meldet die Fußzeile ([`publish_work_running`]) — dieselbe
//! Bedingung, unter der sie „nochmal Ctrl+C: alles sofort beenden" zeigt —,
//! laufende Jobs prüft der Detektor selbst am `JobManager`.
//!
//! # Warum im Eingabe-Thread
//! Der Detektor sitzt im blockierenden Reader-Thread
//! ([`crate::input_reader`]) und **nicht** in der Event-Schleife. So zählt er
//! jeden Druck, bevor ein Dialog ihn schlucken kann, und löst auch dann aus,
//! wenn die Async-Laufzeit gerade blockiert ist — genau der Fall, in dem man
//! einen Not-Aus braucht. Beide Drücke laufen danach unverändert weiter in die
//! Event-Schleife (kooperativer Abbruch, Beenden).
//!
//! # Dauerhafte Jobs
//! Arbeit aus `harw-job-store` (Lease und Claim, `JobMutationScope`) ist kein
//! verwalteter Job dieser Sitzung und wird nicht angefasst; ihre Prozesse
//! liegen in eigenen cgroups, die die Rückfallebene auslässt (PL-97 §8.3).
//!
//! # Nebenläufigkeit
//! [`HardKill`] ist ein billig klonbarer Griff (`Arc`). Alle Methoden sind
//! thread-sicher; [`HardKill::trip`] blockiert höchstens
//! [`JOB_STOP_BUDGET`] + [`KILL_BUDGET`] und braucht keine Tokio-Laufzeit des
//! Aufrufers: die Job-Stopps laufen auf einem eigenen Thread mit eigener
//! Laufzeit. Es hält nirgends eine Sperre über einen Signalaufruf hinweg.
//!
//! # Fehler
//! Es gibt keinen Fehlerpfad nach außen: ein Not-Aus darf nie an einer
//! Teilaufgabe scheitern. Jede Stufe läuft unabhängig von den anderen;
//! Fehlschläge werden protokolliert.

use std::collections::HashSet;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use harw_core::ManagedAgentSpawner;
use harw_tool_job::{Caller, JobId, JobManager, JobSignal, JobState};
use harw_types::SessionId;

use crate::process_tree::{self, Census};

/// Prozessweiter Stand „KI-Arbeit läuft" aus Sicht der Event-Schleife (Turn
/// oder Job), zuletzt gemeldet von der Fußzeile ([`publish_work_running`]).
///
/// Prozessweit, weil die TUI genau eine Event-Schleife pro Prozess hat und
/// die Fußzeile keinen [`HardKill`]-Griff hält. Nur [`HardKill::live`] liest
/// ihn; Test-Griffe haben einen eigenen Schalter.
static WORK_RUNNING: AtomicBool = AtomicBool::new(false);

/// Meldet, ob gerade ein Turn oder ein Job läuft. Ruft die Fußzeile bei jedem
/// Zeichnen auf, mit derselben Bedingung wie ihr Not-Aus-Hinweis.
pub(crate) fn publish_work_running(running: bool) {
    WORK_RUNNING.store(running, Ordering::SeqCst);
}

/// Höchstdauer des letzten Nachkehr-Durchgangs vor einem Exit
/// ([`HardKill::final_sweep`]), je Stufe.
const FINAL_SWEEP_BUDGET: Duration = Duration::from_millis(200);

/// Zeitfenster, in dem ein zweites Ctrl+C den Not-Aus auslöst. Dasselbe
/// Fenster gilt für den „nochmal Ctrl+C"-Hinweis der Event-Schleife.
pub(crate) const DOUBLE_PRESS_WINDOW: Duration = Duration::from_secs(2);

/// Höchstdauer, die [`HardKill::trip`] auf die Job-Stopps wartet.
///
/// `SIGKILL` geht sofort an jede Job-Gruppe; die Frist deckt nur das
/// Nachhalten des Endzustands. Was danach noch läuft, beendet der Job-Pfad im
/// Hintergrund weiter.
const JOB_STOP_BUDGET: Duration = Duration::from_millis(800);

/// Höchstdauer des Prozessbaum-Kills (Scannen, Einfrieren, Nachkehren).
const KILL_BUDGET: Duration = Duration::from_millis(400);

/// Frist für den geordneten Abgang, danach erzwingt der Wächter den Exit.
///
/// Reicht für das Speichern der Sitzung; der Kill selbst ist da längst durch.
const EXIT_GRACE: Duration = Duration::from_millis(1500);

/// Wie lange der Wächter auf die Terminal-Wiederherstellung wartet, bevor er
/// trotzdem beendet (ein blockierter `stdout`-Lock darf den Exit nicht halten).
const TERMINAL_RESTORE_WAIT: Duration = Duration::from_millis(250);

/// Exit-Code nach dem harten Abbruch: 128 + SIGINT, wie bei einem per Ctrl+C
/// beendeten Programm.
pub(crate) const EXIT_CODE: i32 = 130;

/// Hinweiszeile auf stderr, sobald der Prozess nach dem Not-Aus endet.
const EXIT_NOTICE: &str =
    "harw: hard stop (Ctrl+C twice) — all agent work, its jobs and processes were stopped.";

/// Ob `key` ein Ctrl+C ist — dieselbe Abgrenzung wie in der Event-Schleife
/// (`Ctrl` + `c`/`C`), damit Detektor und UI nie auseinanderlaufen.
pub(crate) fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'C'))
}

// ─────────────────────────────────────────────────────────────────────────────
// Doppeldruck-Detektor
// ─────────────────────────────────────────────────────────────────────────────

/// Erkennt zwei Ctrl+C-Drücke innerhalb eines Zeitfensters.
///
/// Reine Zustandsmaschine mit eingespeister Uhrzeit — ohne Terminal und ohne
/// Threads testbar.
#[derive(Debug)]
pub(crate) struct CtrlCDetector {
    window: Duration,
    first: Option<Instant>,
}

impl CtrlCDetector {
    /// Neuer, nicht scharfgestellter Detektor.
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            window,
            first: None,
        }
    }

    /// Ob ein erster Druck noch gilt (scharfgestellt).
    pub(crate) fn is_armed(&self, now: Instant) -> bool {
        self.first
            .is_some_and(|first| now.saturating_duration_since(first) <= self.window)
    }

    /// Registriert einen Ctrl+C-Druck.
    ///
    /// # Returns
    /// `true`, wenn dieser Druck den zweiten innerhalb des Fensters bildet. Der
    /// Detektor ist danach wieder frisch: ein dritter Druck zählt als neuer
    /// erster.
    pub(crate) fn press(&mut self, now: Instant) -> bool {
        if self.is_armed(now) {
            self.first = None;
            true
        } else {
            self.first = Some(now);
            false
        }
    }

    /// Jede andere Taste macht die Scharfstellung rückgängig — wie der
    /// „nochmal Ctrl+C"-Hinweis der Event-Schleife.
    pub(crate) fn disarm(&mut self) {
        self.first = None;
    }
}

/// Prüft ein rohes Terminal-Ereignis auf den Doppeldruck und löst bei Treffer
/// den Not-Aus aus.
///
/// Wird vom Reader-Thread für **jedes** Ereignis aufgerufen, **bevor** es in die
/// Event-Schleife weitergereicht wird. Nur `Press` zählt (kein `Repeat` einer
/// gehaltenen Taste, kein `Release`); Maus-, Resize- und Paste-Ereignisse
/// ändern nichts.
///
/// Ohne laufende Arbeit (siehe Moduldoku, „Nur bei laufender Arbeit") löst
/// auch der zweite Druck nichts aus: er läuft als normales Beenden in die
/// Event-Schleife.
///
/// # Returns
/// `true`, wenn dieses Ereignis den Not-Aus ausgelöst hat.
pub(crate) fn screen_event(
    detector: &mut CtrlCDetector,
    hard_kill: &HardKill,
    event: &Event,
    now: Instant,
) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    if key.kind != KeyEventKind::Press {
        return false;
    }
    if !is_ctrl_c(key) {
        detector.disarm();
        return false;
    }
    if detector.press(now) {
        return hard_kill.trip_if_working();
    }
    // Erster Druck: den Baum festhalten, solange die Elternprozesse noch
    // leben — der kooperative Abbruch, den dieser Druck gleich auslöst, kann
    // sie zu Waisen machen.
    hard_kill.note_first_press();
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Kill-Switch
// ─────────────────────────────────────────────────────────────────────────────

/// Was [`HardKill::trip`] tatsächlich tut.
#[derive(Debug, Clone, Copy)]
struct Mode {
    /// Verwaltete Jobs über `JobManager::stop` beenden.
    stop_jobs: bool,
    /// Den übrigen Prozessbaum beenden. Aus in Tests ohne eigene Wurzel.
    kill_processes: bool,
    /// Frist bis zum erzwungenen `exit`. `None`: nie beenden (Tests).
    exit_after: Option<Duration>,
}

/// Veränderlicher Teil, den die Composition-Root nachführt.
#[derive(Default)]
struct Bindings {
    /// Spawner und Wurzelsitzung für den Abbruch der Hintergrund-Agenten.
    spawner: Option<(Arc<ManagedAgentSpawner>, SessionId)>,
    /// Job-Verwaltung der Sitzung (Wurzel und Kinder teilen sie).
    jobs: Option<Arc<JobManager>>,
    /// Beim ersten Druck festgehaltene Nachkommen (siehe [`Census`]); jeder
    /// erste Druck ersetzt die Aufstellung.
    census: Census,
    /// Ob beim ersten Druck Arbeit lief.
    work_at_arm: bool,
}

/// Woher [`HardKill`] den Turn-Teil von „Arbeit läuft" nimmt.
enum WorkSource {
    /// Die Fußzeile ([`publish_work_running`]).
    Footer,
    /// Eigener Schalter (Tests).
    #[cfg(test)]
    Flag(AtomicBool),
}

struct Inner {
    mode: Mode,
    /// Wurzel des zu beendenden Prozessbaums (im Betrieb: der eigene Prozess).
    root_pid: u32,
    tripped: AtomicBool,
    /// Weckt [`HardKill::tripped`].
    tripped_notify: tokio::sync::Notify,
    work: WorkSource,
    bindings: Mutex<Bindings>,
}

/// Griff auf den prozessweiten Not-Aus. Klonen teilt den Zustand.
#[derive(Clone)]
pub(crate) struct HardKill {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for HardKill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HardKill")
            .field("tripped", &self.is_tripped())
            .field("stop_jobs", &self.inner.mode.stop_jobs)
            .field("kill_processes", &self.inner.mode.kill_processes)
            .finish_non_exhaustive()
    }
}

/// Ergebnis der Job-Stufe von [`HardKill::trip`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct JobStopReport {
    /// Laufende Jobs, denen der Job-Pfad `SIGKILL` geschickt hat.
    requested: usize,
    /// Davon innerhalb von [`JOB_STOP_BUDGET`] in einem Endzustand gemeldet.
    settled: usize,
}

impl HardKill {
    fn with(mode: Mode, root_pid: u32, work: WorkSource) -> Self {
        Self {
            inner: Arc::new(Inner {
                mode,
                root_pid,
                tripped: AtomicBool::new(false),
                tripped_notify: tokio::sync::Notify::new(),
                work,
                bindings: Mutex::new(Bindings::default()),
            }),
        }
    }

    /// Der echte Not-Aus: stoppt Jobs, beendet den Prozessbaum und erzwingt
    /// den Exit.
    pub(crate) fn live() -> Self {
        Self::with(
            Mode {
                stop_jobs: true,
                kill_processes: true,
                exit_after: Some(EXIT_GRACE),
            },
            std::process::id(),
            WorkSource::Footer,
        )
    }

    /// Nur Tests: wirkungsloser Not-Aus, der nichts stoppt und nie beendet.
    #[cfg(test)]
    pub(crate) fn inert() -> Self {
        Self::with(
            Mode {
                stop_jobs: false,
                kill_processes: false,
                exit_after: None,
            },
            std::process::id(),
            WorkSource::Flag(AtomicBool::new(true)),
        )
    }

    /// Nur Tests: stoppt die gebundenen Jobs und beendet den Baum unter
    /// `root_pid` (statt unter dem Testprozess), ohne je zu beenden.
    #[cfg(test)]
    pub(crate) fn for_test(root_pid: u32) -> Self {
        Self::with(
            Mode {
                stop_jobs: true,
                kill_processes: true,
                exit_after: None,
            },
            root_pid,
            WorkSource::Flag(AtomicBool::new(true)),
        )
    }

    /// Nur Tests: setzt den Turn-Teil von „Arbeit läuft" (Vorgabe: `true`).
    #[cfg(test)]
    pub(crate) fn set_work_running_for_test(&self, running: bool) {
        if let WorkSource::Flag(flag) = &self.inner.work {
            flag.store(running, Ordering::SeqCst);
        }
    }

    fn bindings(&self) -> MutexGuard<'_, Bindings> {
        self.inner
            .bindings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Ob der Not-Aus ausgelöst wurde.
    pub(crate) fn is_tripped(&self) -> bool {
        self.inner.tripped.load(Ordering::SeqCst)
    }

    /// Wartet, bis der Not-Aus ausgelöst ist (sofort fertig, wenn er es schon
    /// ist).
    ///
    /// Die Composition-Root wartet darauf neben der Event-Schleife und
    /// verlässt sie beim Auslösen in jedem Zustand — so führt jeder Not-Aus
    /// zum geordneten Abgang, auch wenn ein Dialog den zweiten Druck
    /// geschluckt hat oder die App ihn nicht als Beenden wertet.
    ///
    /// # Cancel safety
    /// Abbruchsicher; ohne verpasstes Wecken (das Warten wird vor der
    /// Prüfung angemeldet).
    pub(crate) async fn tripped(&self) {
        let notified = self.inner.tripped_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_tripped() {
            return;
        }
        notified.await;
    }

    /// Ob gerade KI-Arbeit läuft: ein Turn (Fußzeile bzw. Test-Schalter) oder
    /// ein laufender verwalteter Job.
    fn work_running(&self, jobs: Option<&Arc<JobManager>>) -> bool {
        let turn = match &self.inner.work {
            WorkSource::Footer => WORK_RUNNING.load(Ordering::SeqCst),
            #[cfg(test)]
            WorkSource::Flag(flag) => flag.load(Ordering::SeqCst),
        };
        turn || jobs.is_some_and(|jobs| !running_job_ids(jobs).is_empty())
    }

    /// Zweiter Druck: löst den Not-Aus aus, wenn beim ersten oder jetzt
    /// Arbeit läuft; sonst bleibt es beim normalen Beenden.
    ///
    /// # Returns
    /// `true` nur für den Aufruf, der ihn tatsächlich ausgelöst hat.
    pub(crate) fn trip_if_working(&self) -> bool {
        let (armed, jobs) = {
            let mut bindings = self.bindings();
            (
                std::mem::take(&mut bindings.work_at_arm),
                bindings.jobs.clone(),
            )
        };
        if armed || self.work_running(jobs.as_ref()) {
            self.trip()
        } else {
            false
        }
    }

    /// Bindet Spawner, Wurzelsitzung und Job-Verwaltung der aktuellen Sitzung.
    ///
    /// Die Composition-Root ruft das beim Start und nach jedem
    /// Sitzungswechsel (`/new`, `/resume`) auf, damit der Eingabe-Thread nie
    /// veraltete Griffe hält.
    pub(crate) fn bind_runtime(
        &self,
        spawner: Option<Arc<ManagedAgentSpawner>>,
        root: SessionId,
        jobs: Option<Arc<JobManager>>,
    ) {
        let mut bindings = self.bindings();
        bindings.spawner = spawner.map(|spawner| (spawner, root));
        bindings.jobs = jobs;
    }

    /// Prozesse, deren Teilbaum der rohe Baum-Kill nie betritt: die
    /// Gruppenführer aller noch nicht beendeten verwalteten Jobs (auch
    /// abgelöster). Sie gehören dem Job-Pfad.
    fn job_leaders(jobs: Option<&Arc<JobManager>>) -> HashSet<u32> {
        jobs.map(|jobs| {
            jobs.list(Caller::Operator)
                .into_iter()
                .filter(|status| !status.meta.state.is_terminal())
                .filter_map(|status| status.meta.pid)
                .collect()
        })
        .unwrap_or_default()
    }

    /// Hält die aktuellen Nachkommen fest (erster Ctrl+C-Druck).
    ///
    /// Läuft im Reader-Thread, bevor der Druck weitergereicht wird; ein
    /// einziger `/proc`-Scan, ohne Signal.
    ///
    /// Ersetzt die Aufstellung des vorigen ersten Drucks: sie soll nur die
    /// Waisen des kooperativen Abbruchs **dieses** Drucks abdecken, nie einen
    /// Prozess, der seitdem bewusst abgelöst wurde.
    pub(crate) fn note_first_press(&self) {
        let jobs = self.bindings().jobs.clone();
        let work = self.work_running(jobs.as_ref());
        let mut census = Census::default();
        if self.inner.mode.kill_processes && work {
            let protected = Self::job_leaders(jobs.as_ref());
            census.note(self.inner.root_pid, &protected);
        }
        let mut bindings = self.bindings();
        bindings.census = census;
        bindings.work_at_arm = work;
    }

    /// Löst den Not-Aus aus. Idempotent.
    ///
    /// # Returns
    /// `true` nur für den Aufruf, der ihn tatsächlich ausgelöst hat.
    ///
    /// # Concurrency
    /// Blockiert höchstens [`JOB_STOP_BUDGET`] + [`KILL_BUDGET`]; aus jedem
    /// Thread aufrufbar, auch aus einer Tokio-Laufzeit heraus.
    pub(crate) fn trip(&self) -> bool {
        if self.inner.tripped.swap(true, Ordering::SeqCst) {
            return false;
        }
        let started = Instant::now();
        let (spawner, jobs, census) = {
            let bindings = self.bindings();
            (
                bindings.spawner.clone(),
                bindings.jobs.clone(),
                bindings.census.clone(),
            )
        };

        // 1. Kooperativ: Hintergrund-Agenten wecken, bevor ihre Prozesse
        //    verschwinden — so sehen sie einen Abbruch statt eines
        //    unerklärlichen Prozessendes.
        let background = spawner.map_or(0, |(spawner, root)| {
            spawner
                .cancel_all_background(&root, "hard kill (Ctrl+C x2)")
                .len()
        });

        // 2. Jobs (auch job-gebundene Kind-Agenten): nur über den Job-Pfad.
        let job_report = match (&jobs, self.inner.mode.stop_jobs) {
            (Some(jobs), true) => stop_managed_jobs(jobs, JOB_STOP_BUDGET),
            _ => JobStopReport::default(),
        };

        // 3. Rückfallebene: der übrige Baum in derselben cgroup, ohne
        //    Job-Teilbäume.
        let tree = self.inner.mode.kill_processes.then(|| {
            let protected = Self::job_leaders(jobs.as_ref());
            process_tree::kill_tree(self.inner.root_pid, &protected, &census, KILL_BUDGET)
        });

        tracing::warn!(
            elapsed_ms = started.elapsed().as_millis(),
            background_agents = background,
            jobs_stopped = job_report.requested,
            jobs_settled = job_report.settled,
            killed = tree.map(|report| report.killed),
            denied = tree.map(|report| report.denied),
            "tui.hard_kill.tripped"
        );

        // 4. Den geordneten Abgang wecken und den erzwungenen Exit absichern.
        self.inner.tripped_notify.notify_waiters();
        if let Some(grace) = self.inner.mode.exit_after {
            spawn_exit_watchdog(self.clone(), grace);
        }
        true
    }

    /// Letzter Durchgang vor einem Exit nach dem Not-Aus: stoppt Jobs, die im
    /// Exit-Fenster noch gestartet wurden, und kehrt den Prozessbaum nach.
    ///
    /// Ohne ausgelösten Not-Aus wirkungslos. Blockiert höchstens zweimal
    /// [`FINAL_SWEEP_BUDGET`].
    pub(crate) fn final_sweep(&self) {
        if !self.is_tripped() {
            return;
        }
        let (jobs, census) = {
            let bindings = self.bindings();
            (bindings.jobs.clone(), bindings.census.clone())
        };
        let job_report = match (&jobs, self.inner.mode.stop_jobs) {
            (Some(jobs), true) => stop_managed_jobs(jobs, FINAL_SWEEP_BUDGET),
            _ => JobStopReport::default(),
        };
        let tree = self.inner.mode.kill_processes.then(|| {
            let protected = Self::job_leaders(jobs.as_ref());
            process_tree::kill_tree(self.inner.root_pid, &protected, &census, FINAL_SWEEP_BUDGET)
        });
        if job_report.requested > 0 || tree.is_some_and(|report| report.killed > 0) {
            tracing::warn!(
                jobs_stopped = job_report.requested,
                killed = tree.map(|report| report.killed),
                "tui.hard_kill.final_sweep"
            );
        }
    }

    /// Beendet den Prozess mit [`EXIT_CODE`], falls der Not-Aus ausgelöst war.
    ///
    /// Die Composition-Root ruft das nach dem geordneten Abgang auf (Terminal
    /// zurückgestellt, Sitzung gespeichert). `error` ist ein Fehler dieses
    /// Abgangs (z. B. die Sitzung ließ sich nicht speichern); er geht vor dem
    /// Exit auf stderr, weil der Aufrufer ihn nie zu sehen bekommt. Ohne
    /// Not-Aus und in Tests kehrt es zurück.
    pub(crate) fn exit_if_tripped(&self, error: Option<&dyn std::fmt::Display>) {
        if !self.is_tripped() || self.inner.mode.exit_after.is_none() {
            return;
        }
        self.final_sweep();
        let mut stderr = std::io::stderr();
        if let Some(error) = error {
            let _ = writeln!(stderr, "harw: {error}");
        }
        let _ = writeln!(stderr, "{EXIT_NOTICE}");
        std::process::exit(EXIT_CODE);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Job-Pfad
// ─────────────────────────────────────────────────────────────────────────────

/// Ob der Not-Aus einen Job in diesem Zustand stoppt: nur die von dieser
/// Sitzung beaufsichtigten laufenden (`Running`). Abgelöste Jobs früherer
/// Sitzungen sind keine Arbeit dieses Laufs. `Queued` kommt in
/// `JobManager::list` nicht vor (ein Eintrag entsteht erst als `Running` oder
/// `Failed`); ein Start, der beim Listen noch läuft, fängt der nächste
/// Durchgang ab.
fn stops(state: JobState) -> bool {
    state == JobState::Running
}

/// Die laufenden Jobs von `jobs`, die der Not-Aus stoppt.
fn running_job_ids(jobs: &JobManager) -> Vec<JobId> {
    jobs.list(Caller::Operator)
        .into_iter()
        .filter(|status| stops(status.meta.state))
        .map(|status| status.meta.job_id)
        .collect()
}

/// Stoppt jeden laufenden Job von `jobs` über `JobManager::stop` mit
/// `SIGKILL` und wartet höchstens `budget` auf die Endzustände.
///
/// Nach jedem Durchgang wird erneut gelistet: ein Job, der währenddessen
/// gestartet wurde (ein Kind-Agent oder der abgebrochene Turn ruft noch
/// `job.start`), wird im nächsten Durchgang gestoppt, solange `budget` reicht.
fn stop_managed_jobs(jobs: &Arc<JobManager>, budget: Duration) -> JobStopReport {
    let deadline = Instant::now() + budget;
    let mut requested_ids: HashSet<JobId> = HashSet::new();
    let mut report = JobStopReport::default();
    loop {
        let fresh: Vec<JobId> = running_job_ids(jobs)
            .into_iter()
            .filter(|id| !requested_ids.contains(id))
            .collect();
        if fresh.is_empty() {
            return report;
        }
        requested_ids.extend(fresh.iter().cloned());
        let pass = stop_job_pass(jobs, fresh, deadline);
        report.requested += pass.requested;
        report.settled += pass.settled;
        if Instant::now() >= deadline {
            return report;
        }
    }
}

/// Ein Durchgang von [`stop_managed_jobs`]: stoppt `ids` und wartet bis
/// `deadline` auf die Endzustände.
///
/// Die Stopps laufen auf einem eigenen Thread mit eigener Laufzeit: der
/// Aufrufer (Reader-Thread, Event-Schleife, Test) braucht keine und darf in
/// einer stecken. Was nach `deadline` noch nicht gemeldet ist, läuft dort
/// weiter; das Signal ist zu dem Zeitpunkt längst zugestellt.
fn stop_job_pass(jobs: &Arc<JobManager>, ids: Vec<JobId>, deadline: Instant) -> JobStopReport {
    let requested = ids.len();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let manager = Arc::clone(jobs);
    let spawned = std::thread::Builder::new()
        .name("harw-hard-stop-jobs".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(%error, "tui.hard_kill.job_runtime_failed");
                    return;
                }
            };
            runtime.block_on(async move {
                let mut stops = tokio::task::JoinSet::new();
                for id in ids {
                    let manager = Arc::clone(&manager);
                    stops.spawn(async move {
                        manager
                            .stop(&id, Caller::Operator, JobSignal::Kill)
                            .await
                            .map(|status| status.meta.state.is_terminal())
                    });
                }
                while let Some(joined) = stops.join_next().await {
                    match joined {
                        Ok(Ok(terminal)) => {
                            let _ = done_tx.send(terminal);
                        }
                        Ok(Err(error)) => {
                            tracing::debug!(%error, "tui.hard_kill.job_stop_failed");
                        }
                        Err(error) => {
                            tracing::debug!(%error, "tui.hard_kill.job_stop_panicked");
                        }
                    }
                }
            });
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "tui.hard_kill.job_thread_failed");
        return JobStopReport {
            requested,
            settled: 0,
        };
    }
    let mut settled = 0;
    let mut answered = 0;
    while answered < requested {
        let left = deadline.saturating_duration_since(Instant::now());
        match done_rx.recv_timeout(left) {
            Ok(terminal) => {
                answered += 1;
                settled += usize::from(terminal);
            }
            // Frist abgelaufen oder alle Stopps beantwortet (Sender fort).
            Err(_) => break,
        }
    }
    JobStopReport { requested, settled }
}

// ─────────────────────────────────────────────────────────────────────────────
// Erzwungener Exit
// ─────────────────────────────────────────────────────────────────────────────

/// Startet den Wächter-Thread: nach `grace` ein letzter Nachkehr-Durchgang,
/// Terminal zurückstellen und beenden.
fn spawn_exit_watchdog(hard_kill: HardKill, grace: Duration) {
    let spawned = std::thread::Builder::new()
        .name("harw-hard-exit".to_owned())
        .spawn(move || {
            std::thread::sleep(grace);
            hard_kill.final_sweep();
            restore_terminal_bounded();
            std::process::exit(EXIT_CODE);
        });
    if let Err(error) = spawned {
        // Ohne Wächter bleibt der geordnete Abgang; mehr ist nicht zu tun.
        tracing::error!(%error, "tui.hard_kill.watchdog_spawn_failed");
    }
}

/// Stellt das Terminal zurück, wartet aber höchstens [`TERMINAL_RESTORE_WAIT`].
///
/// Läuft in einem eigenen Thread: schreibt gerade die Event-Schleife auf einen
/// vollen `stdout`, würde ein direkter Aufruf hier hängen und den Exit
/// verhindern.
fn restore_terminal_bounded() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("harw-hard-exit-tty".to_owned())
        .spawn(move || {
            restore_terminal();
            let _ = done_tx.send(());
        });
    if spawned.is_ok() {
        let _ = done_rx.recv_timeout(TERMINAL_RESTORE_WAIT);
    }
}

/// Maus-Capture, Bracketed-Paste, Alternate-Screen und Raw-Mode zurücknehmen,
/// Cursor zeigen und eine Zeile Hinweis ausgeben. Best-effort.
fn restore_terminal() {
    let mut out = std::io::stdout();
    let _ = crossterm::execute!(
        out,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show,
    );
    let _ = out.flush();
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = writeln!(std::io::stderr(), "{EXIT_NOTICE}");
}

#[cfg(test)]
mod tests;
