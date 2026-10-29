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

// ── cgroup-Grenze ────────────────────────────────────────────────────────────

/// cgroup-Quelle ohne Wissen: keine Grenze.
fn no_cgroup(_pid: u32) -> Option<String> {
    None
}

#[test]
fn parse_cgroup_reads_the_v2_line_only() {
    assert_eq!(
        parse_cgroup("12:memory:/legacy\n0::/user.slice/harw.scope\n").as_deref(),
        Some("/user.slice/harw.scope")
    );
    assert_eq!(parse_cgroup("12:memory:/legacy\n"), None, "nur cgroup v1");
    assert_eq!(parse_cgroup("0::\n"), None, "leerer Pfad");
}

#[test]
fn targets_never_enter_a_process_in_a_foreign_cgroup() {
    //      10 (harw)
    //     /  \
    //   11    12  (12: eigene Job-cgroup, darunter 13)
    //          |
    //          13
    let table = [entry(10, 1), entry(11, 10), entry(12, 10), entry(13, 12)];
    let cgroup = |pid: u32| -> Option<String> {
        Some(match pid {
            12 | 13 => "/harw.slice/jobs/harw-job-7".to_owned(),
            _ => "/user.slice/harw.scope".to_owned(),
        })
    };
    assert_eq!(
        pids(&targets(
            &table,
            &[10],
            &HashSet::new(),
            Some("/user.slice/harw.scope"),
            &cgroup
        )),
        vec![11],
        "der Job-Teilbaum mit bekannter cgroup bleibt dem Job-Pfad"
    );
}

#[test]
fn targets_without_an_own_cgroup_are_the_plain_descendants() {
    let table = [entry(10, 1), entry(11, 10), entry(12, 11)];
    let foreign = |_pid: u32| Some("/elsewhere".to_owned());
    assert_eq!(
        pids(&targets(&table, &[10], &HashSet::new(), None, &foreign)),
        vec![11, 12]
    );
}

#[test]
fn targets_keep_processes_whose_cgroup_is_unreadable() {
    let table = [entry(10, 1), entry(11, 10)];
    assert_eq!(
        pids(&targets(
            &table,
            &[10],
            &HashSet::new(),
            Some("/user.slice/harw.scope"),
            &no_cgroup
        )),
        vec![11],
        "ein Prozess ohne lesbare cgroup gilt als Teil des eigenen Baums"
    );
}

#[test]
fn census_does_not_note_a_process_in_a_foreign_cgroup() {
    let mut census = Census::default();
    let table = [entry(10, 1), entry(11, 10), entry(12, 10)];
    let cgroup =
        |pid: u32| -> Option<String> { Some(if pid == 12 { "/job" } else { "/own" }.to_owned()) };
    census.note_from(&table, 10, &HashSet::new(), Some("/own"), &cgroup);
    let orphaned = [entry(11, 1), entry(12, 1)];
    assert_eq!(census.survivors(&orphaned, &HashSet::new()), vec![11]);
}

#[test]
fn job_cgroup_finds_the_attempt_level_but_never_harws_own() {
    assert_eq!(
        job_cgroup("/harw.slice/jobs/harw-job-a1", "/user.slice/harw.scope").as_deref(),
        Some("/harw.slice/jobs/harw-job-a1")
    );
    assert_eq!(
        job_cgroup("/harw.slice/jobs/harw-job-a1/sub", "/user.slice/harw.scope").as_deref(),
        Some("/harw.slice/jobs/harw-job-a1"),
        "eine Unter-cgroup endet mit ihrem Versuch"
    );
    assert_eq!(
        job_cgroup("/system.slice/other.service", "/user.slice/harw.scope"),
        None,
        "keine Versuchs-cgroup"
    );
    assert_eq!(
        job_cgroup("/jobs/harw-job-outer/inner", "/jobs/harw-job-outer/harw"),
        None,
        "harw läuft selbst in dieser Job-cgroup: nie die eigene beenden"
    );
    assert_eq!(
        job_cgroup(
            "/jobs/harw-job-outer/harw-job-inner",
            "/jobs/harw-job-outer/harw"
        )
        .as_deref(),
        Some("/jobs/harw-job-outer/harw-job-inner")
    );
    assert_eq!(
        job_cgroup("/jobs/../harw-job-x", "/own"),
        None,
        "Pfade mit `..` gelten als unbekannt"
    );
}

/// Baum für die cgroup-Kill-Tests:
///
/// ```text
///        10 (harw, /own)
///      /   |    \
///    11    12    14
///          |
///          13
/// ```
/// 12 und 13 liegen in einer Versuchs-cgroup von harw, 14 in einer fremden
/// cgroup außerhalb der Job-Wurzel.
fn cgroup_tree() -> (Vec<ProcEntry>, impl Fn(u32) -> Option<String>) {
    let table = vec![
        entry(10, 1),
        entry(11, 10),
        entry(12, 10),
        entry(13, 12),
        entry(14, 10),
    ];
    let cgroup = |pid: u32| -> Option<String> {
        Some(
            match pid {
                12 | 13 => "/harw.slice/jobs/harw-job-a1",
                14 => "/system.slice/other.service",
                _ => "/own",
            }
            .to_owned(),
        )
    };
    (table, cgroup)
}

#[test]
fn a_pruned_attempt_subtree_gets_the_cgroup_kill_and_a_foreign_cgroup_does_not() -> TestResult {
    let (table, cgroup) = cgroup_tree();
    let split = split_targets(&table, &[10], &HashSet::new(), Some("/own"), &cgroup);
    assert_eq!(
        pids(&split.targets),
        vec![11],
        "fremde cgroups nie über den Baum"
    );
    assert_eq!(
        split.job_cgroups,
        vec!["/harw.slice/jobs/harw-job-a1".to_owned()]
    );

    // Unechtes cgroupfs: beide cgroups existieren mit leerer `cgroup.kill`.
    let fs = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let attempt = fs.path().join("harw.slice/jobs/harw-job-a1");
    let other = fs.path().join("system.slice/other.service");
    for dir in [&attempt, &other] {
        std::fs::create_dir_all(dir).map_err(ctx("mkdir"))?;
        std::fs::write(dir.join("cgroup.kill"), "").map_err(ctx("cgroup.kill"))?;
    }
    for job in &split.job_cgroups {
        assert!(kill_cgroup(fs.path(), job), "{job}");
    }
    let read = |dir: &std::path::Path| std::fs::read_to_string(dir.join("cgroup.kill"));
    assert_eq!(read(&attempt).map_err(ctx("read attempt"))?, "1");
    assert_eq!(
        read(&other).map_err(ctx("read other"))?,
        "",
        "eine cgroup außerhalb der Job-Wurzel bleibt unberührt"
    );
    Ok(())
}

#[test]
fn kill_cgroup_never_creates_a_missing_cgroup() -> TestResult {
    let fs = tempfile::tempdir().map_err(ctx("tempdir"))?;
    assert!(!kill_cgroup(fs.path(), "/harw.slice/jobs/harw-job-gone"));
    assert!(!fs.path().join("harw.slice").exists());
    Ok(())
}

#[test]
fn census_notes_the_attempt_cgroups_of_its_descendants() {
    let (table, cgroup) = cgroup_tree();
    let mut census = Census::default();
    census.note_from(&table, 10, &HashSet::new(), Some("/own"), &cgroup);
    assert_eq!(
        census.job_cgroups().collect::<Vec<_>>(),
        vec!["/harw.slice/jobs/harw-job-a1"]
    );
}

// ── Census ───────────────────────────────────────────────────────────────────

#[test]
fn census_finds_a_noted_process_again_only_with_the_same_start_time() {
    let mut census = Census::default();
    let table = [entry(10, 1), entry(11, 10)];
    census.note_from(&table, 10, &HashSet::new(), None, &no_cgroup);

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
    census.note_from(&table, 10, &protected, None, &no_cgroup);

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
