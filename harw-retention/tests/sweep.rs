//! Behavioural tests of `harw_retention::sweep` and the class opt-in rules.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use harw_retention::{
    Class, ClassConfig, ClassDefaults, ClassKind, NameMatch, RemovalReason, ResolvedClass,
    RetentionError, RetentionPolicy, Roots, SweepMode, sweep,
};

harw_test_support::define_test_error!(pub(crate));

fn io<T>(r: std::io::Result<T>, what: &'static str) -> TestResult<T> {
    r.map_err(|e| TestError::Context {
        context: what,
        source: e.to_string(),
    })
}

/// Creates `name` with `bytes` bytes and an mtime `age_secs` before `now`.
fn make(
    dir: &Path,
    name: &str,
    bytes: usize,
    now: SystemTime,
    age_secs: u64,
) -> TestResult<PathBuf> {
    let path = dir.join(name);
    io(fs::write(&path, vec![b'x'; bytes]), "write")?;
    let file = io(File::options().write(true).open(&path), "open")?;
    io(
        file.set_modified(now - Duration::from_secs(age_secs)),
        "set_modified",
    )?;
    Ok(path)
}

fn run(
    dir: &Path,
    policy: &RetentionPolicy,
    now: SystemTime,
) -> TestResult<harw_retention::Report> {
    sweep(dir, policy, now).map_err(|e| TestError::Unexpected(e.to_string()))
}

fn names(report: &harw_retention::Report) -> Vec<String> {
    report
        .removed
        .iter()
        .filter_map(|r| {
            r.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
        })
        .collect()
}

#[test]
fn test_age_removes_only_old_files() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let now = SystemTime::now();
    make(tmp.path(), "new.log", 1, now, 10)?;
    let old = make(tmp.path(), "old.log", 1, now, 5_000)?;
    make(tmp.path(), "older.log", 1, now, 6_000)?;
    let policy = RetentionPolicy {
        max_age: Some(Duration::from_secs(1_000)),
        ..RetentionPolicy::unlimited()
    };
    let report = run(tmp.path(), &policy, now)?;
    assert_eq!(names(&report), ["older.log", "old.log"]);
    assert!(
        report
            .removed
            .iter()
            .all(|r| r.reason == RemovalReason::Age)
    );
    assert_eq!(report.kept, 1);
    assert!(!old.exists());
    assert!(tmp.path().join("new.log").exists());
    Ok(())
}

#[test]
fn test_count_removes_oldest_beyond_limit() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let now = SystemTime::now();
    for i in 0..5u64 {
        make(tmp.path(), &format!("f{i}.log"), 1, now, 100 * (i + 1))?;
    }
    let policy = RetentionPolicy {
        max_files: Some(2),
        ..RetentionPolicy::unlimited()
    };
    let report = run(tmp.path(), &policy, now)?;
    assert_eq!(names(&report), ["f4.log", "f3.log", "f2.log"]);
    assert!(
        report
            .removed
            .iter()
            .all(|r| r.reason == RemovalReason::Count)
    );
    assert!(tmp.path().join("f0.log").exists());
    assert!(tmp.path().join("f1.log").exists());
    Ok(())
}

#[test]
fn test_bytes_removes_oldest_until_total_fits() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let now = SystemTime::now();
    make(tmp.path(), "a.log", 40, now, 10)?;
    make(tmp.path(), "b.log", 40, now, 20)?;
    make(tmp.path(), "c.log", 40, now, 30)?;
    let policy = RetentionPolicy {
        max_bytes: Some(80),
        ..RetentionPolicy::unlimited()
    };
    let report = run(tmp.path(), &policy, now)?;
    assert_eq!(names(&report), ["c.log"]);
    assert_eq!(report.removed[0].reason, RemovalReason::Bytes);
    assert_eq!(report.removed_bytes(), 40);
    assert_eq!(report.kept_bytes, 80);
    Ok(())
}

#[test]
fn test_enforcement_order_is_age_then_count_then_bytes() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let now = SystemTime::now();
    make(tmp.path(), "a.log", 50, now, 10)?;
    make(tmp.path(), "b.log", 50, now, 20)?;
    make(tmp.path(), "c.log", 50, now, 30)?;
    make(tmp.path(), "d.log", 50, now, 40)?;
    make(tmp.path(), "e.log", 50, now, 9_000)?;
    let policy = RetentionPolicy {
        max_age: Some(Duration::from_secs(1_000)),
        max_files: Some(3),
        max_bytes: Some(100),
        ..RetentionPolicy::unlimited()
    };
    let report = run(tmp.path(), &policy, now)?;
    let reasons: Vec<_> = report
        .removed
        .iter()
        .map(|r| (r.path.file_name().map(|n| n.to_owned()), r.reason))
        .collect();
    assert_eq!(reasons.len(), 3);
    assert_eq!(report.removed[0].reason, RemovalReason::Age); // e
    assert_eq!(report.removed[1].reason, RemovalReason::Count); // d
    assert_eq!(report.removed[2].reason, RemovalReason::Bytes); // c
    assert_eq!(names(&report), ["e.log", "d.log", "c.log"]);
    Ok(())
}

#[test]
fn test_keep_newest_is_exempt_from_every_rule() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let now = SystemTime::now();
    make(tmp.path(), "newest.log", 1_000, now, 9_000)?;
    make(tmp.path(), "second.log", 1_000, now, 9_500)?;
    let policy = RetentionPolicy {
        max_age: Some(Duration::from_secs(1)),
        max_bytes: Some(1),
        max_files: Some(0),
        keep_newest: 1,
        ..RetentionPolicy::unlimited()
    };
    let report = run(tmp.path(), &policy, now)?;
    assert_eq!(names(&report), ["second.log"]);
    assert!(tmp.path().join("newest.log").exists());
    Ok(())
}

#[test]
fn test_name_match_limits_scope() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let now = SystemTime::now();
    let mine = make(tmp.path(), "rotated-1.jsonl", 1, now, 5_000)?;
    let other = make(tmp.path(), "keep-me.txt", 1, now, 5_000)?;
    let other2 = make(tmp.path(), "rotated-1.txt", 1, now, 5_000)?;
    let policy = RetentionPolicy {
        max_age: Some(Duration::from_secs(1)),
        keep_newest: 0,
        name_match: NameMatch::prefix_suffix("rotated-", ".jsonl"),
        ..RetentionPolicy::unlimited()
    };
    run(tmp.path(), &policy, now)?;
    assert!(!mine.exists());
    assert!(other.exists());
    assert!(other2.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn test_symlink_is_not_followed_and_not_deleted() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let outside = io(tempfile::tempdir(), "tempdir")?;
    let now = SystemTime::now();
    let target = make(outside.path(), "precious.log", 10, now, 9_000)?;
    let link = tmp.path().join("link.log");
    io(std::os::unix::fs::symlink(&target, &link), "symlink")?;
    let dir_target = outside.path().join("subdir");
    io(fs::create_dir(&dir_target), "mkdir")?;
    io(
        std::os::unix::fs::symlink(&dir_target, tmp.path().join("dirlink.log")),
        "symlink dir",
    )?;
    let policy = RetentionPolicy {
        max_age: Some(Duration::from_secs(1)),
        max_files: Some(0),
        max_bytes: Some(0),
        keep_newest: 0,
        ..RetentionPolicy::unlimited()
    };
    let report = run(tmp.path(), &policy, now)?;
    assert!(report.removed.is_empty());
    assert_eq!(report.skipped, 2);
    assert!(target.exists());
    assert!(fs::symlink_metadata(&link).is_ok());
    assert!(dir_target.is_dir());

    // A symlinked sweep directory is refused outright.
    let dirlink = outside.path().join("swept");
    io(std::os::unix::fs::symlink(tmp.path(), &dirlink), "symlink")?;
    assert!(matches!(
        sweep(&dirlink, &policy, now),
        Err(RetentionError::NotADirectory(_))
    ));
    Ok(())
}

#[test]
fn test_lock_and_dot_temp_files_are_skipped() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let now = SystemTime::now();
    let lock = make(tmp.path(), "state.lock", 1, now, 9_000)?;
    let dot = make(tmp.path(), ".tmp-write", 1, now, 9_000)?;
    let victim = make(tmp.path(), "x.log", 1, now, 9_000)?;
    let policy = RetentionPolicy {
        max_age: Some(Duration::from_secs(1)),
        keep_newest: 0,
        ..RetentionPolicy::unlimited()
    };
    let report = run(tmp.path(), &policy, now)?;
    assert_eq!(names(&report), ["x.log"]);
    assert_eq!(report.skipped, 2);
    assert!(lock.exists());
    assert!(dot.exists());
    assert!(!victim.exists());
    Ok(())
}

#[test]
fn test_dry_run_deletes_nothing() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let now = SystemTime::now();
    let a = make(tmp.path(), "a.log", 5, now, 9_000)?;
    let b = make(tmp.path(), "b.log", 5, now, 9_001)?;
    let policy = RetentionPolicy {
        max_age: Some(Duration::from_secs(1)),
        keep_newest: 0,
        dry_run: true,
        ..RetentionPolicy::unlimited()
    };
    let report = run(tmp.path(), &policy, now)?;
    assert!(report.dry_run);
    assert_eq!(report.removed.len(), 2);
    assert_eq!(report.kept, 0);
    assert!(a.exists() && b.exists());
    Ok(())
}

#[test]
fn test_expired_deadline_returns_timed_out_and_touches_nothing() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let now = SystemTime::now();
    let a = make(tmp.path(), "a.log", 5, now, 9_000)?;
    let policy = RetentionPolicy {
        max_age: Some(Duration::from_secs(1)),
        keep_newest: 0,
        deadline: Some(Instant::now()),
        ..RetentionPolicy::unlimited()
    };
    let report = run(tmp.path(), &policy, now)?;
    assert!(report.timed_out);
    assert!(report.removed.is_empty());
    assert_eq!(report.kept, 1);
    assert!(a.exists());
    Ok(())
}

#[test]
fn test_missing_directory_is_an_empty_report() -> TestResult {
    let tmp = io(tempfile::tempdir(), "tempdir")?;
    let report = run(
        &tmp.path().join("nope"),
        &RetentionPolicy::unlimited(),
        SystemTime::now(),
    )?;
    assert!(report.removed.is_empty() && report.errors.is_empty() && !report.timed_out);
    Ok(())
}

fn no_dirs(_: &Roots) -> Vec<PathBuf> {
    Vec::new()
}

static SECURITY: Class = Class {
    id: "evidence",
    kind: ClassKind::SecurityRelevant,
    name_match: NameMatch::any(),
    defaults: ClassDefaults {
        max_age_secs: Some(10),
        max_bytes: None,
        max_files: Some(1),
        keep_newest: 1,
    },
    resolve_dirs: no_dirs,
};

static EPHEMERAL: Class = Class {
    id: "scratch",
    kind: ClassKind::Ephemeral,
    name_match: NameMatch::any(),
    defaults: ClassDefaults {
        max_age_secs: Some(10),
        max_bytes: None,
        max_files: None,
        keep_newest: 1,
    },
    resolve_dirs: no_dirs,
};

#[test]
fn test_security_class_refuses_apply_without_explicit_optin() -> TestResult {
    let unset = ResolvedClass::resolve(&SECURITY, &ClassConfig::default());
    assert!(!unset.enabled);
    assert!(matches!(
        unset.policy(SweepMode::Apply, None),
        Err(RetentionError::OptInRequired("evidence"))
    ));
    // Dry run is always allowed and still carries the limits.
    let dry = unset
        .policy(SweepMode::DryRun, None)
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    assert!(dry.dry_run);
    assert_eq!(dry.max_files, Some(1));
    assert!(unset.doctor_warning().is_some());

    let off = ClassConfig {
        enabled: Some(false),
        ..ClassConfig::default()
    };
    assert!(
        ResolvedClass::resolve(&SECURITY, &off)
            .policy(SweepMode::Apply, None)
            .is_err()
    );

    let on = ClassConfig {
        enabled: Some(true),
        ..ClassConfig::default()
    };
    let resolved = ResolvedClass::resolve(&SECURITY, &on);
    assert!(resolved.explicit_optin);
    let policy = resolved
        .policy(SweepMode::Apply, None)
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    assert!(!policy.dry_run);
    assert_eq!(policy.max_age, Some(Duration::from_secs(10)));
    Ok(())
}

#[test]
fn test_ephemeral_class_defaults_on_and_overrides_apply() -> TestResult {
    let resolved = ResolvedClass::resolve(
        &EPHEMERAL,
        &ClassConfig {
            max_files: Some(3),
            ..ClassConfig::default()
        },
    );
    assert!(resolved.enabled);
    assert!(!resolved.explicit_optin);
    let policy = resolved
        .policy(SweepMode::Apply, None)
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    assert_eq!(policy.max_files, Some(3));
    assert_eq!(policy.max_age, Some(Duration::from_secs(10)));
    let off = ResolvedClass::resolve(
        &EPHEMERAL,
        &ClassConfig {
            enabled: Some(false),
            ..ClassConfig::default()
        },
    );
    assert!(matches!(
        off.policy(SweepMode::Apply, None),
        Err(RetentionError::Disabled("scratch"))
    ));
    Ok(())
}
