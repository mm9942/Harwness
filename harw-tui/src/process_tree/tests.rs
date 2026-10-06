//! Tests für den Prozessbaum-Abbruch: reine Logik und echte Prozessbäume.
//!
//! Die Kill-Tests zielen **nie** auf den Testprozess selbst: jeder startet
//! einen eigenen Hilfsprozess als Wurzel und beendet nur dessen Nachkommen.

use super::*;
use crate::test_support::{TestError, TestResult, ctx};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

/// Bequemer Konstruktor für Momentaufnahmen.
fn entry(pid: u32, ppid: u32) -> ProcEntry {
    ProcEntry {
        pid,
        ppid,
        start_ticks: u64::from(pid) * 10,
        zombie: false,
    }
}

fn pids(entries: &[ProcEntry]) -> Vec<u32> {
    let mut pids: Vec<u32> = entries.iter().map(|entry| entry.pid).collect();
    pids.sort_unstable();
    pids
}

// ── Parser ───────────────────────────────────────────────────────────────────

/// Eine realistische stat-Zeile: Zustand `S`, Eltern 7, Startzeit 4242.
fn stat_line(pid: u32, comm: &str, state: &str, ppid: u32, start: u64) -> String {
    // Felder 5..=21 (17 Stück) zwischen PPID und Startzeit.
    let filler = ["0"; 17].join(" ");
    format!("{pid} ({comm}) {state} {ppid} {filler} {start} 0 0")
}

#[test]
fn parse_stat_reads_state_parent_and_start_ticks() -> TestResult {
    let parsed = parse_stat(42, &stat_line(42, "cargo", "S", 7, 4242))
        .ok_or(TestError::Missing("parsed entry"))?;
    assert_eq!(
        parsed,
        ProcEntry {
            pid: 42,
            ppid: 7,
            start_ticks: 4242,
            zombie: false
        }
    );
    Ok(())
}

#[test]
fn parse_stat_survives_hostile_program_names() -> TestResult {
    // Leerzeichen und Klammern im Namen dürfen die Felder nicht verschieben.
    let parsed = parse_stat(9, &stat_line(9, "a) (b c) S 1", "R", 3, 77))
        .ok_or(TestError::Missing("parsed entry"))?;
    assert_eq!(
        (parsed.ppid, parsed.start_ticks, parsed.zombie),
        (3, 77, false)
    );
    Ok(())
}

#[test]
fn parse_stat_marks_zombies_and_dead() -> TestResult {
    for state in ["Z", "X"] {
        let parsed = parse_stat(5, &stat_line(5, "x", state, 1, 1))
            .ok_or(TestError::Missing("parsed entry"))?;
        assert!(parsed.zombie, "Zustand {state} muss als beendet gelten");
    }
    Ok(())
}

#[test]
fn parse_stat_rejects_foreign_or_truncated_records() {
    assert!(
        parse_stat(1, &stat_line(2, "x", "S", 1, 1)).is_none(),
        "falsche PID"
    );
    assert!(parse_stat(1, "1 (x) S 1 2 3").is_none(), "abgeschnitten");
    assert!(parse_stat(1, "garbage").is_none());
}

#[test]
fn parse_ps_reads_pid_ppid_and_zombie_state() {
    let parsed = parse_ps("  101     1 Ss\n  102   101 Z+\n  103   101 R\nnot a line\n");
    assert_eq!(parsed.len(), 3);
    assert!(!parsed[0].zombie);
    assert!(parsed[1].zombie);
    assert_eq!(
        (parsed[2].pid, parsed[2].ppid, parsed[2].start_ticks),
        (103, 101, 0)
    );
}

// ── Baum ─────────────────────────────────────────────────────────────────────

#[test]
fn descendants_walks_every_level_and_excludes_the_root() {
    //      10
    //     /  \
    //   11    12
    //   |
    //   13        20 (fremd)
    let table = [
        entry(10, 1),
        entry(11, 10),
        entry(12, 10),
        entry(13, 11),
        entry(20, 1),
    ];
    assert_eq!(
        pids(&descendants(&table, &[10], &HashSet::new())),
        vec![11, 12, 13]
    );
}

#[test]
fn descendants_prunes_the_whole_subtree_of_a_protected_process() {
    let table = [
        entry(10, 1),
        entry(11, 10),
        entry(12, 10),
        entry(13, 11),
        entry(14, 13),
    ];
    let protected: HashSet<u32> = [11].into_iter().collect();
    assert_eq!(
        pids(&descendants(&table, &[10], &protected)),
        vec![12],
        "11 samt 13 und 14 bleiben unberührt"
    );
}

#[test]
fn descendants_skips_zombies_but_still_reaches_past_them() {
    let mut zombie = entry(11, 10);
    zombie.zombie = true;
    let table = [entry(10, 1), zombie, entry(12, 11)];
    assert_eq!(pids(&descendants(&table, &[10], &HashSet::new())), vec![12]);
}

#[test]
fn descendants_never_returns_pid_zero_or_one() {
    let table = [entry(10, 1), entry(1, 10), entry(0, 10), entry(11, 10)];
    assert_eq!(pids(&descendants(&table, &[10], &HashSet::new())), vec![11]);
}

#[test]
fn descendants_terminates_on_a_cyclic_snapshot() {
    // Inkonsistente Momentaufnahme: 11 und 12 nennen einander Eltern.
    let table = [entry(10, 1), entry(11, 12), entry(12, 11), entry(13, 10)];
    assert_eq!(pids(&descendants(&table, &[10], &HashSet::new())), vec![13]);
}

#[test]
fn descendants_accepts_several_roots() {
    let table = [entry(10, 1), entry(11, 10), entry(30, 1), entry(31, 30)];
    assert_eq!(
        pids(&descendants(&table, &[10, 30], &HashSet::new())),
        vec![11, 31]
    );
}

// ── Census ───────────────────────────────────────────────────────────────────

#[test]
fn census_finds_a_noted_process_again_only_with_the_same_start_time() {
    let mut census = Census::default();
    let table = [entry(10, 1), entry(11, 10)];
    census.note_from(&table, 10, &HashSet::new());

    // 11 lebt weiter, nun an init umgehängt: weiterhin auffindbar.
    let orphaned = [entry(11, 1)];
    assert_eq!(census.survivors(&orphaned, &HashSet::new()), vec![11]);

    // Dieselbe PID mit anderer Startzeit ist ein fremder Prozess.
    let mut reused = entry(11, 1);
    reused.start_ticks += 1;
    assert!(census.survivors(&[reused], &HashSet::new()).is_empty());
}

#[test]
fn census_ignores_zombies_protected_processes_and_unverifiable_sources() {
    let mut census = Census::default();
    let mut no_identity = entry(12, 10);
    no_identity.start_ticks = 0;
    let table = [entry(10, 1), entry(11, 10), no_identity, entry(13, 10)];
    let protected: HashSet<u32> = [13].into_iter().collect();
    census.note_from(&table, 10, &protected);

    let mut zombie = entry(11, 1);
    zombie.zombie = true;
    assert!(
        census.survivors(&[zombie], &HashSet::new()).is_empty(),
        "Zombie"
    );
    assert!(
        census.survivors(&[no_identity], &HashSet::new()).is_empty(),
        "keine Identität"
    );
    assert!(
        census
            .survivors(&[entry(13, 1)], &HashSet::new())
            .is_empty(),
        "geschützt"
    );
}

// ── Echte Prozesse ───────────────────────────────────────────────────────────

/// Startet eine Shell als Wurzel eines kleinen Baums.
fn spawn_root(script: &str) -> TestResult<Child> {
    Command::new("/bin/sh")
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| TestError::Unexpected(format!("spawn: {error}")))
}

/// Ob der Prozess läuft (nicht fehlt, kein Zombie).
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|text| parse_stat(pid, &text))
        .is_some_and(|entry| !entry.zombie)
}

/// Wartet bis zu zwei Sekunden darauf, dass `check` wahr wird.
fn eventually(check: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    check()
}

/// Wartet, bis der Baum unter `root` mindestens `count` Nachkommen hat.
fn wait_for_descendants(root: u32, count: usize) -> TestResult<Vec<ProcEntry>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let table = snapshot().map_err(ctx("snapshot"))?;
        let found = descendants(&table, &[root], &HashSet::new());
        if found.len() >= count {
            return Ok(found);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(TestError::Missing("Nachkommen des Hilfsprozesses"))
}

/// Beendet den Hilfsprozess und sammelt ihn ein.
fn reap(mut root: Child) {
    let _ = root.kill();
    let _ = root.wait();
}

#[cfg(target_os = "linux")]
#[test]
fn kill_tree_kills_every_descendant_including_a_setsid_grandchild() -> TestResult {
    // Zwei normale Kinder und ein Enkel in eigener Sitzung (entkommt jedem
    // Gruppen-Kill, bleibt aber Nachkomme).
    let root =
        spawn_root("sleep 300 & (sleep 300; true) & setsid sleep 300 & wait; exec sleep 300")?;
    let tree = wait_for_descendants(root.id(), 3)?;

    let report = kill_tree(
        root.id(),
        &HashSet::new(),
        &Census::default(),
        Duration::from_secs(2),
    );

    assert!(
        report.killed >= tree.len(),
        "{report:?} für {} Prozesse",
        tree.len()
    );
    for process in &tree {
        assert!(
            eventually(|| !alive(process.pid)),
            "Prozess {} überlebte den Baum-Kill",
            process.pid
        );
    }
    assert!(
        alive(root.id()),
        "die Wurzel selbst darf nicht getroffen werden"
    );
    reap(root);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn kill_tree_spares_the_subtree_of_a_protected_process() -> TestResult {
    // Zwei Zweige; der erste ist ein „Hintergrund-Job" und wird geschützt.
    let root = spawn_root("(sleep 301; true) & (sleep 302; true) & wait")?;
    let tree = wait_for_descendants(root.id(), 4)?;
    let branches: Vec<&ProcEntry> = tree.iter().filter(|e| e.ppid == root.id()).collect();
    let [kept, doomed] = branches[..] else {
        return Err(TestError::Unexpected(format!("{} Zweige", branches.len())));
    };
    let kept_subtree = descendants(&tree, &[kept.pid], &HashSet::new());
    let protected: HashSet<u32> = [kept.pid].into_iter().collect();

    kill_tree(
        root.id(),
        &protected,
        &Census::default(),
        Duration::from_secs(2),
    );

    assert!(
        eventually(|| !alive(doomed.pid)),
        "ungeschützter Zweig lebt"
    );
    assert!(alive(kept.pid), "geschützter Zweig wurde getroffen");
    for process in &kept_subtree {
        assert!(
            alive(process.pid),
            "Prozess {} im geschützten Teilbaum starb",
            process.pid
        );
    }
    // Aufräumen: den geschützten Zweig bewusst selbst beenden.
    kill_tree(
        root.id(),
        &HashSet::new(),
        &Census::default(),
        Duration::from_secs(2),
    );
    reap(root);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn kill_tree_reaches_an_orphan_recorded_before_its_parent_died() -> TestResult {
    // Wurzel → Zwischenprozess → Enkel. Das erste Ctrl+C beendet nur den
    // Zwischenprozess (wie `shell.exec`s `terminate()`); der Enkel hängt dann
    // an init und ist kein Nachkomme der Wurzel mehr.
    let root = spawn_root("sh -c 'sleep 303; true' & wait; exec sleep 300")?;
    let tree = wait_for_descendants(root.id(), 2)?;
    let middle = tree
        .iter()
        .find(|e| e.ppid == root.id())
        .ok_or(TestError::Missing("Zwischenprozess"))?;
    let grandchild = tree
        .iter()
        .find(|e| e.ppid == middle.pid)
        .ok_or(TestError::Missing("Enkel"))?;

    let mut census = Census::default();
    census.note(root.id(), &HashSet::new());

    // Nur den Zwischenprozess töten: der Enkel wird verwaist.
    kill_tree_pid(middle.pid)?;
    assert!(
        eventually(|| {
            snapshot().is_ok_and(|table| {
                !descendants(&table, &[root.id()], &HashSet::new())
                    .iter()
                    .any(|e| e.pid == grandchild.pid)
            })
        }),
        "der Enkel müsste verwaist sein"
    );
    assert!(alive(grandchild.pid));

    kill_tree(root.id(), &HashSet::new(), &census, Duration::from_secs(2));

    assert!(
        eventually(|| !alive(grandchild.pid)),
        "der verwaiste Enkel überlebte, obwohl er festgehalten war"
    );
    reap(root);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn kill_tree_of_a_childless_root_is_a_cheap_noop() -> TestResult {
    // `exec`: dash forkt sonst für einen einzelnen Befehl, und das Kind wäre ein
    // Nachkomme. Hier soll die Wurzel wirklich kinderlos sein.
    let root = spawn_root("exec sleep 300")?;
    let started = Instant::now();
    let report = kill_tree(
        root.id(),
        &HashSet::new(),
        &Census::default(),
        Duration::from_secs(2),
    );
    assert_eq!(report.killed, 0);
    assert!(!report.budget_exhausted);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "leerer Baum muss sofort enden"
    );
    assert!(alive(root.id()));
    reap(root);
    Ok(())
}

/// Beendet genau einen Prozess (Test-Hilfe, ohne Identitätsprüfung).
#[cfg(target_os = "linux")]
fn kill_tree_pid(pid: u32) -> TestResult {
    let status = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .map_err(|error| TestError::Unexpected(format!("kill: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(TestError::Unexpected(format!(
            "kill -KILL {pid} schlug fehl"
        )))
    }
}
