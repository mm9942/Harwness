//! Fester Not-Aus: **2× Ctrl+C beendet sofort die gesamte KI-Arbeit**.
//!
//! # Verantwortung
//! Zwei Ctrl+C-Drücke innerhalb von [`DOUBLE_PRESS_WINDOW`] lösen — in jedem
//! UI-Zustand, auch bei offenem Dialog, Overlay oder hängendem Async-Loop —
//! den harten Abbruch aus:
//!
//! 1. **Kooperativ, sofort**: das `CancelToken` des laufenden Turns (und damit
//!    der Token-Baum aller Kind-Agenten) wird mit [`CancelReason::Shutdown`]
//!    abgebrochen, ebenso alle Hintergrund-Agenten. Jeder cancel-fähige
//!    `await` (Modellaufruf, Shell-Wartepunkte, Kind-Läufe) wacht auf.
//! 2. **Nicht kooperativ, sofort**: der gesamte Prozessbaum unter harw wird
//!    per `SIGKILL` beendet ([`crate::process_tree`]) — auch Prozesse, die
//!    nicht auf einen Abbruch hören oder deren Elternprozess schon tot ist.
//!    Ausgenommen sind nur die ablösbaren Hintergrund-Jobs der Nutzerin
//!    (siehe unten).
//! 3. **Geordneter Abgang mit Frist**: die Async-Schleife sieht
//!    [`HardKill::is_tripped`], beendet den Turn, speichert die Sitzung und
//!    verlässt die TUI. Hängt das, erzwingt ein Wächter-Thread nach
//!    [`EXIT_GRACE`] die Terminal-Wiederherstellung und `exit(130)`.
//!
//! # Warum im Eingabe-Thread
//! Der Detektor sitzt im blockierenden Reader-Thread
//! ([`crate::input_reader`]) und **nicht** in der Event-Schleife. So zählt er
//! jeden Druck, bevor ein Dialog ihn schlucken kann, und löst auch dann aus,
//! wenn die Async-Laufzeit gerade blockiert ist — genau der Fall, in dem man
//! einen Not-Aus braucht. Der erste Druck läuft unverändert weiter in die
//! Event-Schleife (kooperativer Abbruch, Dialog ablehnen, Hinweis).
//!
//! # Hintergrund-Jobs der Nutzerin
//! „Doppeltes Ctrl+C beendet harw und löst laufende Jobs ab; sie laufen
//! weiter" ist ein bestehender Vertrag (`jobs_glue::detach_for_exit`). Der
//! harte Abbruch bricht ihn nicht: die Teilbäume der Jobs aus
//! `JobManager::detachable_leader_pids` bleiben verschont und werden
//! abgelöst. Die Prozesse job-gebundener **Kind-Agenten** zählen nicht dazu
//! und sterben mit dem Baum.
//!
//! # Nebenläufigkeit
//! [`HardKill`] ist ein billig klonbarer Griff (`Arc`). Alle Methoden sind
//! thread-sicher; [`HardKill::trip`] blockiert höchstens [`KILL_BUDGET`] und
//! braucht keine Tokio-Laufzeit. Es hält nirgends eine Sperre über einen
//! Signalaufruf hinweg.
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
use harw_core::cancel::{CancelReason, CancelToken};
use harw_tool_job::JobManager;
use harw_types::SessionId;

use crate::process_tree::{self, Census};

/// Zeitfenster, in dem ein zweites Ctrl+C den Not-Aus auslöst. Dasselbe
/// Fenster gilt für den „Ctrl+C again to quit"-Hinweis der Event-Schleife.
pub(crate) const DOUBLE_PRESS_WINDOW: Duration = Duration::from_secs(2);

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
const EXIT_CODE: i32 = 130;

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
    /// „Ctrl+C again to quit"-Hinweis der Event-Schleife.
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
        hard_kill.trip();
        return true;
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
    /// Prozessbaum beenden. Aus in Tests und Einbettungen ohne eigenen Prozess.
    kill_processes: bool,
    /// Frist bis zum erzwungenen `exit`. `None`: nie beenden (Tests).
    exit_after: Option<Duration>,
}

/// Veränderlicher Teil, den die Event-Schleife nachführt.
#[derive(Default)]
struct Bindings {
    /// Abbruchgriff des laufenden Turns.
    turn_cancel: Option<CancelToken>,
    /// Spawner und Wurzelsitzung für den Abbruch der Hintergrund-Agenten.
    spawner: Option<(Arc<ManagedAgentSpawner>, SessionId)>,
    /// Job-Verwaltung: welche Jobs verschont und abgelöst werden.
    jobs: Option<Arc<JobManager>>,
    /// Beim ersten Druck festgehaltene Nachkommen (siehe [`Census`]).
    census: Census,
}

struct Inner {
    mode: Mode,
    /// Wurzel des zu beendenden Prozessbaums (im Betrieb: der eigene Prozess).
    root_pid: u32,
    tripped: AtomicBool,
    /// `trip` hat die ablösbaren Jobs schon abgelöst (und Kind-Agent-Jobs
    /// getötet); die Schleife darf sie nicht noch einmal pauschal ablösen.
    jobs_settled: AtomicBool,
    wake: tokio::sync::Notify,
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
            .field("kill_processes", &self.inner.mode.kill_processes)
            .finish_non_exhaustive()
    }
}

impl HardKill {
    fn with(mode: Mode, root_pid: u32) -> Self {
        Self {
            inner: Arc::new(Inner {
                mode,
                root_pid,
                tripped: AtomicBool::new(false),
                jobs_settled: AtomicBool::new(false),
                wake: tokio::sync::Notify::new(),
                bindings: Mutex::new(Bindings::default()),
            }),
        }
    }

    /// Der echte Not-Aus: beendet den Prozessbaum und erzwingt den Exit.
    pub(crate) fn live() -> Self {
        Self::with(
            Mode {
                kill_processes: true,
                exit_after: Some(EXIT_GRACE),
            },
            std::process::id(),
        )
    }

    /// Wirkungsloser Not-Aus: bricht nur die gebundenen Token ab. Standard für
    /// jede `ChatApp` ohne Composition-Root (Tests, Einbettungen) — ein
    /// Doppeldruck darf dort keine fremden Prozesse treffen.
    pub(crate) fn inert() -> Self {
        Self::with(
            Mode {
                kill_processes: false,
                exit_after: None,
            },
            std::process::id(),
        )
    }

    /// Nur Tests: beendet den Baum unter `root_pid` (statt unter dem
    /// Testprozess), ohne je zu beenden.
    #[cfg(test)]
    pub(crate) fn for_test(root_pid: u32) -> Self {
        Self::with(
            Mode {
                kill_processes: true,
                exit_after: None,
            },
            root_pid,
        )
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

    /// Ob `trip` die Jobs bereits abgewickelt hat (nur im Livebetrieb mit
    /// gebundener Job-Verwaltung). Dann ist ein weiteres `detach_all` falsch:
    /// es führte die getöteten Kind-Agent-Jobs als „abgelöst".
    pub(crate) fn jobs_settled(&self) -> bool {
        self.inner.jobs_settled.load(Ordering::SeqCst)
    }

    /// Fertig, sobald der Not-Aus ausgelöst ist (sofort, wenn schon geschehen).
    pub(crate) async fn tripped(&self) {
        loop {
            // Erst anmelden, dann prüfen: `notify_waiters` weckt jedes
            // bereits erzeugte `Notified`, auch ohne vorheriges Polling.
            let notified = self.inner.wake.notified();
            if self.is_tripped() {
                return;
            }
            notified.await;
        }
    }

    /// Führt den Abbruchgriff des laufenden Turns nach (`None` am Turn-Ende).
    pub(crate) fn set_turn_cancel(&self, cancel: Option<CancelToken>) {
        self.bindings().turn_cancel = cancel;
    }

    /// Bindet Spawner, Wurzelsitzung und Job-Verwaltung der aktuellen Sitzung.
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

    /// Prozesse, deren Teilbaum verschont bleibt: die ablösbaren Jobs.
    fn protected(&self) -> HashSet<u32> {
        self.bindings()
            .jobs
            .as_ref()
            .map(|jobs| jobs.detachable_leader_pids().into_iter().collect())
            .unwrap_or_default()
    }

    /// Hält die aktuellen Nachkommen fest (erster Ctrl+C-Druck).
    ///
    /// Läuft im Reader-Thread, bevor der Druck weitergereicht wird; ein
    /// einziger `/proc`-Scan, ohne Signal.
    pub(crate) fn note_first_press(&self) {
        if !self.inner.mode.kill_processes {
            return;
        }
        let protected = self.protected();
        let mut census = std::mem::take(&mut self.bindings().census);
        census.note(self.inner.root_pid, &protected);
        self.bindings().census = census;
    }

    /// Löst den Not-Aus aus. Idempotent.
    ///
    /// # Returns
    /// `true` nur für den Aufruf, der ihn tatsächlich ausgelöst hat.
    ///
    /// # Concurrency
    /// Blockiert höchstens [`KILL_BUDGET`]; aus jedem Thread aufrufbar.
    pub(crate) fn trip(&self) -> bool {
        if self.inner.tripped.swap(true, Ordering::SeqCst) {
            return false;
        }
        let started = Instant::now();
        let (turn_cancel, spawner, jobs, census) = {
            let bindings = self.bindings();
            (
                bindings.turn_cancel.clone(),
                bindings.spawner.clone(),
                bindings.jobs.clone(),
                bindings.census.clone(),
            )
        };

        // 1. Kooperativ: weckt jeden cancel-fähigen `await` sofort, bevor die
        //    Prozesse unter ihnen verschwinden — so sehen Werkzeuge einen
        //    Abbruch statt eines unerklärlichen Prozessendes.
        if let Some(cancel) = &turn_cancel {
            cancel.cancel(CancelReason::Shutdown);
        }
        let background = spawner.map_or(0, |(spawner, root)| {
            spawner
                .cancel_all_background(&root, "hard kill (Ctrl+C x2)")
                .len()
        });

        // 2. Nicht kooperativ: der ganze Prozessbaum, außer ablösbaren Jobs.
        let report = self.inner.mode.kill_processes.then(|| {
            let protected = jobs
                .as_ref()
                .map(|jobs| jobs.detachable_leader_pids().into_iter().collect())
                .unwrap_or_default();
            let report =
                process_tree::kill_tree(self.inner.root_pid, &protected, &census, KILL_BUDGET);
            if let Some(jobs) = &jobs {
                jobs.detach_user_jobs();
                self.inner.jobs_settled.store(true, Ordering::SeqCst);
            }
            report
        });

        tracing::warn!(
            elapsed_ms = started.elapsed().as_millis(),
            turn_cancelled = turn_cancel.is_some(),
            background_agents = background,
            killed = report.map(|report| report.killed),
            denied = report.map(|report| report.denied),
            "tui.hard_kill.tripped"
        );

        // 3. Die Event-Schleife wecken und den erzwungenen Exit absichern.
        self.inner.wake.notify_waiters();
        if let Some(grace) = self.inner.mode.exit_after {
            spawn_exit_watchdog(grace);
        }
        true
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Erzwungener Exit
// ─────────────────────────────────────────────────────────────────────────────

/// Startet den Wächter-Thread: nach `grace` Terminal zurückstellen und beenden.
fn spawn_exit_watchdog(grace: Duration) {
    let spawned = std::thread::Builder::new()
        .name("harw-hard-exit".to_owned())
        .spawn(move || {
            std::thread::sleep(grace);
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
    let _ = writeln!(
        std::io::stderr(),
        "harw: hard stop (Ctrl+C twice) — all agent work and its processes were killed."
    );
}

#[cfg(test)]
mod tests;
