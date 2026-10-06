//! Tests für den Doppeldruck-Detektor und den Kill-Switch.

use super::*;
use crate::test_support::{TestError, TestResult};
use crossterm::event::{KeyEventState, MouseEvent, MouseEventKind};

const WINDOW: Duration = Duration::from_secs(2);

fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind,
        state: KeyEventState::NONE,
    })
}

fn ctrl_c() -> Event {
    key(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        KeyEventKind::Press,
    )
}

// ── Detektor ─────────────────────────────────────────────────────────────────

#[test]
fn two_presses_inside_the_window_complete_a_double_press() {
    let mut detector = CtrlCDetector::new(WINDOW);
    let t0 = Instant::now();
    assert!(
        !detector.press(t0),
        "der erste Druck allein löst nichts aus"
    );
    assert!(detector.press(t0 + Duration::from_millis(300)));
}

#[test]
fn a_press_after_the_window_starts_over() {
    let mut detector = CtrlCDetector::new(WINDOW);
    let t0 = Instant::now();
    assert!(!detector.press(t0));
    assert!(
        !detector.press(t0 + WINDOW + Duration::from_millis(1)),
        "zu spät: zählt als neuer erster Druck"
    );
    assert!(detector.press(t0 + WINDOW + Duration::from_millis(500)));
}

#[test]
fn the_window_edge_is_inclusive() {
    let mut detector = CtrlCDetector::new(WINDOW);
    let t0 = Instant::now();
    detector.press(t0);
    assert!(detector.press(t0 + WINDOW));
}

#[test]
fn a_double_press_resets_so_a_third_press_is_a_new_first() {
    let mut detector = CtrlCDetector::new(WINDOW);
    let t0 = Instant::now();
    detector.press(t0);
    assert!(detector.press(t0 + Duration::from_millis(10)));
    assert!(!detector.press(t0 + Duration::from_millis(20)));
}

#[test]
fn disarm_forgets_the_first_press() {
    let mut detector = CtrlCDetector::new(WINDOW);
    let t0 = Instant::now();
    detector.press(t0);
    detector.disarm();
    assert!(!detector.press(t0 + Duration::from_millis(10)));
}

// ── screen_event ─────────────────────────────────────────────────────────────

#[test]
fn screen_event_trips_on_the_second_ctrl_c_only() {
    let hard_kill = HardKill::inert();
    let mut detector = CtrlCDetector::new(WINDOW);
    let t0 = Instant::now();
    assert!(!screen_event(&mut detector, &hard_kill, &ctrl_c(), t0));
    assert!(!hard_kill.is_tripped());
    assert!(screen_event(
        &mut detector,
        &hard_kill,
        &ctrl_c(),
        t0 + Duration::from_millis(250)
    ));
    assert!(hard_kill.is_tripped());
}

#[test]
fn screen_event_accepts_an_uppercase_ctrl_c_like_the_event_loop_does() {
    let hard_kill = HardKill::inert();
    let mut detector = CtrlCDetector::new(WINDOW);
    let upper = key(
        KeyCode::Char('C'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        KeyEventKind::Press,
    );
    let t0 = Instant::now();
    screen_event(&mut detector, &hard_kill, &upper, t0);
    assert!(screen_event(&mut detector, &hard_kill, &ctrl_c(), t0));
}

#[test]
fn another_key_between_the_presses_disarms() {
    let hard_kill = HardKill::inert();
    let mut detector = CtrlCDetector::new(WINDOW);
    let t0 = Instant::now();
    let letter = key(KeyCode::Char('x'), KeyModifiers::NONE, KeyEventKind::Press);
    screen_event(&mut detector, &hard_kill, &ctrl_c(), t0);
    screen_event(&mut detector, &hard_kill, &letter, t0);
    assert!(!screen_event(&mut detector, &hard_kill, &ctrl_c(), t0));
    assert!(!hard_kill.is_tripped());
}

#[test]
fn a_plain_c_without_control_is_not_a_ctrl_c() {
    let hard_kill = HardKill::inert();
    let mut detector = CtrlCDetector::new(WINDOW);
    let plain = key(KeyCode::Char('c'), KeyModifiers::NONE, KeyEventKind::Press);
    let t0 = Instant::now();
    screen_event(&mut detector, &hard_kill, &plain, t0);
    assert!(!screen_event(&mut detector, &hard_kill, &plain, t0));
}

#[test]
fn key_repeat_and_release_never_count() {
    let hard_kill = HardKill::inert();
    let mut detector = CtrlCDetector::new(WINDOW);
    let repeat = key(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        KeyEventKind::Repeat,
    );
    let release = key(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        KeyEventKind::Release,
    );
    let t0 = Instant::now();
    screen_event(&mut detector, &hard_kill, &ctrl_c(), t0);
    assert!(
        !screen_event(&mut detector, &hard_kill, &repeat, t0),
        "eine gehaltene Taste ist kein zweiter Druck"
    );
    assert!(!screen_event(&mut detector, &hard_kill, &release, t0));
    assert!(!hard_kill.is_tripped());
}

#[test]
fn mouse_and_resize_events_do_not_disarm() {
    let hard_kill = HardKill::inert();
    let mut detector = CtrlCDetector::new(WINDOW);
    let t0 = Instant::now();
    screen_event(&mut detector, &hard_kill, &ctrl_c(), t0);
    let mouse = Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert!(!screen_event(&mut detector, &hard_kill, &mouse, t0));
    assert!(!screen_event(
        &mut detector,
        &hard_kill,
        &Event::Resize(80, 24),
        t0
    ));
    assert!(screen_event(&mut detector, &hard_kill, &ctrl_c(), t0));
}

// ── Kill-Switch ──────────────────────────────────────────────────────────────

#[test]
fn trip_cancels_the_bound_turn_with_shutdown_and_is_idempotent() {
    let hard_kill = HardKill::inert();
    let cancel = CancelToken::new();
    let child = cancel.child();
    hard_kill.set_turn_cancel(Some(cancel.clone()));

    assert!(!hard_kill.is_tripped());
    assert!(hard_kill.trip(), "der erste Aufruf löst aus");
    assert!(!hard_kill.trip(), "jeder weitere ist wirkungslos");

    assert!(hard_kill.is_tripped());
    assert_eq!(cancel.reason(), Some(CancelReason::Shutdown));
    assert!(child.is_cancelled(), "Kind-Token erben den Abbruch");
}

#[test]
fn trip_keeps_an_earlier_user_cancel_reason() {
    // Das erste Ctrl+C hat den Turn schon mit `User` abgebrochen: die erste
    // Begründung gewinnt (siehe `CancelToken::cancel`).
    let hard_kill = HardKill::inert();
    let cancel = CancelToken::new();
    cancel.cancel(CancelReason::User);
    hard_kill.set_turn_cancel(Some(cancel.clone()));
    hard_kill.trip();
    assert_eq!(cancel.reason(), Some(CancelReason::User));
    assert!(cancel.is_cancelled());
}

#[test]
fn a_cleared_turn_token_is_not_cancelled() {
    let hard_kill = HardKill::inert();
    let cancel = CancelToken::new();
    hard_kill.set_turn_cancel(Some(cancel.clone()));
    hard_kill.set_turn_cancel(None);
    hard_kill.trip();
    assert!(
        !cancel.is_cancelled(),
        "ein beendeter Turn bleibt unberührt"
    );
}

#[test]
fn an_inert_trip_never_touches_processes() -> TestResult {
    let mut root = spawn_tree("sleep 300 & wait")?;
    let hard_kill = HardKill::inert();
    hard_kill.trip();
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        root.try_wait()
            .map_err(|e| TestError::Unexpected(e.to_string()))?
            .is_none(),
        "der Hilfsprozess läuft weiter"
    );
    // Aufräumen: erst den Baum (das `sleep` der Shell), dann die Wurzel — sonst
    // bliebe das Kind als Waise zurück.
    process_tree::kill_tree(
        root.id(),
        &HashSet::new(),
        &process_tree::Census::default(),
        Duration::from_secs(2),
    );
    let _ = root.kill();
    let _ = root.wait();
    Ok(())
}

#[tokio::test]
async fn tripped_resolves_immediately_when_already_tripped() -> TestResult {
    let hard_kill = HardKill::inert();
    hard_kill.trip();
    tokio::time::timeout(Duration::from_secs(1), hard_kill.tripped())
        .await
        .map_err(|_| TestError::Unexpected("tripped() hing nach trip()".into()))
}

#[tokio::test]
async fn tripped_wakes_a_waiter_from_another_thread() -> TestResult {
    let hard_kill = HardKill::inert();
    let waiter = tokio::spawn({
        let hard_kill = hard_kill.clone();
        async move { hard_kill.tripped().await }
    });
    // Der Auslöser kommt aus einem fremden OS-Thread (wie der Reader-Thread).
    let trigger = hard_kill.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        trigger.trip();
    });
    tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .map_err(|_| TestError::Unexpected("Wartender wurde nicht geweckt".into()))?
        .map_err(|e| TestError::Unexpected(e.to_string()))
}

// ── Echter Prozessbaum ───────────────────────────────────────────────────────

fn spawn_tree(script: &str) -> TestResult<std::process::Child> {
    std::process::Command::new("/bin/sh")
        .args(["-c", script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| TestError::Unexpected(format!("spawn: {e}")))
}

#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|text| process_tree::parse_stat(pid, &text))
        .is_some_and(|entry| !entry.zombie)
}

/// Wartet, bis unter `root` mindestens `count` Nachkommen laufen.
#[cfg(target_os = "linux")]
fn wait_for_tree(root: u32, count: usize) -> TestResult<Vec<process_tree::ProcEntry>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let table = process_tree::snapshot().map_err(|e| TestError::Unexpected(e.to_string()))?;
        let found = process_tree::descendants(&table, &[root], &HashSet::new());
        if found.len() >= count {
            return Ok(found);
        }
        if Instant::now() > deadline {
            return Err(TestError::Unexpected(format!(
                "Testbaum nicht aufgebaut: {found:?}"
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn an_armed_trip_kills_the_tree_below_its_root_but_not_the_root() -> TestResult {
    let mut root = spawn_tree("sleep 300 & (sleep 300; true) & wait; exec sleep 300")?;
    let tree = wait_for_tree(root.id(), 3)?;

    let hard_kill = HardKill::for_test(root.id());
    let started = Instant::now();
    assert!(hard_kill.trip());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "der Kill muss sofort sein, dauerte {:?}",
        started.elapsed()
    );

    for process in &tree {
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive(process.pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!alive(process.pid), "Prozess {} lebt noch", process.pid);
    }
    assert!(alive(root.id()), "die Wurzel gehört dem Aufrufer");
    let _ = root.kill();
    let _ = root.wait();
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn the_census_from_the_first_press_lets_trip_reach_an_orphan() -> TestResult {
    // Wurzel → Zwischenprozess → Enkel. Das erste Ctrl+C löst den kooperativen
    // Abbruch aus, der nur den Zwischenprozess beendet; der Enkel wird Waise.
    let mut root = spawn_tree("sh -c 'sleep 304; true' & wait; exec sleep 300")?;
    let tree = wait_for_tree(root.id(), 2)?;
    let middle = tree
        .iter()
        .find(|e| e.ppid == root.id())
        .ok_or(TestError::Missing("Zwischenprozess"))?;
    let grandchild = tree
        .iter()
        .find(|e| e.ppid == middle.pid)
        .ok_or(TestError::Missing("Enkel"))?;

    let hard_kill = HardKill::for_test(root.id());
    let mut detector = CtrlCDetector::new(WINDOW);
    let t0 = Instant::now();
    // Erster Druck: Aufstellung.
    screen_event(&mut detector, &hard_kill, &ctrl_c(), t0);
    // „Kooperativer Abbruch": nur der Zwischenprozess stirbt.
    std::process::Command::new("kill")
        .args(["-KILL", &middle.pid.to_string()])
        .status()
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let table = process_tree::snapshot().map_err(|e| TestError::Unexpected(e.to_string()))?;
        let still_below = process_tree::descendants(&table, &[root.id()], &HashSet::new())
            .iter()
            .any(|e| e.pid == grandchild.pid);
        if !still_below {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        alive(grandchild.pid),
        "der Enkel muss als Waise noch laufen"
    );

    // Zweiter Druck: der Not-Aus.
    assert!(screen_event(
        &mut detector,
        &hard_kill,
        &ctrl_c(),
        t0 + Duration::from_millis(400)
    ));
    let deadline = Instant::now() + Duration::from_secs(2);
    while alive(grandchild.pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !alive(grandchild.pid),
        "der verwaiste Enkel überlebte den Not-Aus"
    );
    let _ = root.kill();
    let _ = root.wait();
    Ok(())
}

// ── Livemodus im Subprozess ──────────────────────────────────────────────────
//
// `HardKill::live()` beendet den Prozess (`exit(130)`) und seinen Baum — das
// lässt sich nur in einem Kindprozess prüfen. Der Test startet dafür dieses
// Test-Binary erneut und wählt per Umgebungsvariable den Kindmodus.

/// Umgebungsvariable, die [`live_trip_child_mode`] scharf schaltet.
#[cfg(target_os = "linux")]
const CHILD_MODE_ENV: &str = "HARW_HARD_KILL_CHILD_MODE";

/// Kindmodus: ohne die Umgebungsvariable ein leerer Test.
///
/// Baut einen kleinen Prozessbaum, gibt dessen PIDs aus, löst den **echten**
/// Not-Aus aus und wartet dann — der Wächter muss den Prozess beenden.
#[cfg(target_os = "linux")]
#[test]
fn live_trip_child_mode() -> TestResult {
    if std::env::var_os(CHILD_MODE_ENV).is_none() {
        return Ok(());
    }
    let root = spawn_tree("sleep 300 & sleep 300 & wait")?;
    let tree = wait_for_tree(root.id(), 2)?;
    let pids: Vec<String> = tree.iter().map(|entry| entry.pid.to_string()).collect();
    println!("PIDS {}", pids.join(" "));

    let hard_kill = HardKill::live();
    let cancel = CancelToken::new();
    hard_kill.set_turn_cancel(Some(cancel.clone()));
    hard_kill.trip();
    println!("TRIPPED cancelled={}", cancel.is_cancelled());
    // Der geordnete Abgang bleibt aus: nur der Wächter kann uns jetzt beenden.
    std::thread::sleep(Duration::from_secs(60));
    println!("SURVIVED");
    Ok(())
}

/// Beendet den Kindprozess auch dann, wenn der Test vorher fehlschlägt.
#[cfg(target_os = "linux")]
struct KillOnDrop(std::process::Child);

#[cfg(target_os = "linux")]
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn live_trip_kills_the_tree_and_the_watchdog_exits_with_130() -> TestResult {
    use std::io::{BufRead as _, BufReader};

    let exe = std::env::current_exe().map_err(|e| TestError::Unexpected(e.to_string()))?;
    let mut child = KillOnDrop(
        std::process::Command::new(exe)
            .args([
                "--exact",
                "hard_kill::tests::live_trip_child_mode",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_MODE_ENV, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| TestError::Unexpected(format!("spawn child: {e}")))?,
    );
    let stdout = child.0.stdout.take().ok_or(TestError::Missing("stdout"))?;

    // Zeilen lesen, bis der Not-Aus gemeldet ist; die PIDs merken.
    let mut pids: Vec<u32> = Vec::new();
    let mut tripped_line = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| TestError::Unexpected(e.to_string()))?;
        // libtest schreibt `test <name> ... ` ohne Zeilenumbruch vor unsere
        // Ausgabe; die Marker stehen daher nicht zwingend am Zeilenanfang.
        if let Some((_, rest)) = line.split_once("PIDS ") {
            pids = rest
                .split_whitespace()
                .filter_map(|pid| pid.parse().ok())
                .collect();
        } else if let Some((_, rest)) = line.split_once("TRIPPED ") {
            tripped_line = Some(format!("TRIPPED {rest}"));
            break;
        }
    }
    let tripped_at = Instant::now();
    assert_eq!(pids.len(), 2, "Kindbaum nicht gemeldet");
    assert_eq!(
        tripped_line.as_deref(),
        Some("TRIPPED cancelled=true"),
        "trip() muss das Turn-Token abbrechen"
    );
    for pid in &pids {
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive(*pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!alive(*pid), "Prozess {pid} des Kindbaums lebt noch");
    }

    // Der Wächter beendet den Prozess nach der Gnadenfrist — nicht früher, nicht nie.
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|e| TestError::Unexpected(e.to_string()))?
        {
            break status;
        }
        if Instant::now() > deadline {
            return Err(TestError::Unexpected(
                "der Wächter hat den Prozess nicht beendet".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let waited = tripped_at.elapsed();
    assert_eq!(status.code(), Some(130), "Exit-Code des harten Abbruchs");
    assert!(
        waited >= EXIT_GRACE - Duration::from_millis(300),
        "der Wächter darf den geordneten Abgang nicht verkürzen: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "Wächter zu spät: {waited:?}"
    );
    Ok(())
}
