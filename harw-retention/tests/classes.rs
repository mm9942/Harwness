//! Tests of the declared class table and the macro-generated config.

use std::fs::{self, File};
use std::time::{Duration, SystemTime};

use harw_retention::{
    CLASSES, ClassKind, RetentionConfig, RetentionError, Roots, SweepMode, policy_for, resolve_all,
};

harw_test_support::define_test_error!(pub(crate));

const EPHEMERAL: [&str; 5] = [
    "tui_log",
    "telemetry_rotated",
    "job_logs",
    "bug_reports",
    "scan_reports",
];
const SECURITY: [&str; 5] = [
    "dod_spool",
    "sentinel_export",
    "freeze_resolved",
    "session_transcripts",
    "session_corrupt_backups",
];

#[test]
fn test_table_declares_the_initial_classes_with_the_decided_kinds() {
    assert_eq!(CLASSES.len(), 10);
    for id in EPHEMERAL {
        assert!(
            CLASSES
                .iter()
                .any(|c| c.id == id && c.kind == ClassKind::Ephemeral),
            "{id} must be ephemeral"
        );
    }
    for id in SECURITY {
        assert!(
            CLASSES
                .iter()
                .any(|c| c.id == id && c.kind == ClassKind::SecurityRelevant),
            "{id} must be security-relevant"
        );
    }
}

#[test]
fn test_defaults_ephemeral_on_security_off_and_apply_refused() -> TestResult {
    let cfg: RetentionConfig = toml::from_str("").map_err(ctx("empty toml"))?;
    assert_eq!(cfg, RetentionConfig::default());
    assert!(cfg.validate().is_ok());
    let all = resolve_all(&cfg);
    assert_eq!(all.len(), 10);
    for class in &all {
        match class.class.kind {
            ClassKind::Ephemeral => {
                assert!(class.enabled, "{}", class.class.id);
                assert!(class.policy(SweepMode::Apply, None).is_ok());
                // Every ephemeral class ships real default limits.
                assert!(class.max_age_secs.is_some() && class.max_bytes.is_some());
                assert!(class.doctor_warning().is_none());
            }
            ClassKind::SecurityRelevant => {
                assert!(!class.enabled, "{}", class.class.id);
                assert!(matches!(
                    class.policy(SweepMode::Apply, None),
                    Err(RetentionError::OptInRequired(_))
                ));
                assert!(class.policy(SweepMode::DryRun, None).is_ok());
                assert!(class.doctor_warning().is_some());
            }
        }
    }
    Ok(())
}

#[test]
fn test_config_parses_overrides_and_rejects_unknown_keys() -> TestResult {
    let cfg: RetentionConfig = toml::from_str(
        "[dod_spool]\nenabled = true\nmax_files = 5\n[tui_log]\nmax_age_secs = 60\n",
    )
    .map_err(ctx("toml"))?;
    let spool = policy_for(&cfg, "dod_spool").ok_or(TestError::Missing("dod_spool"))?;
    assert!(spool.enabled && spool.explicit_optin);
    assert_eq!(spool.max_files, Some(5));
    let tui = policy_for(&cfg, "tui_log").ok_or(TestError::Missing("tui_log"))?;
    assert_eq!(tui.max_age_secs, Some(60));
    assert_eq!(tui.max_files, Some(5), "unset keys keep the class default");

    assert!(toml::from_str::<RetentionConfig>("[bogus]\nx = 1").is_err());
    assert!(toml::from_str::<RetentionConfig>("[tui_log]\nbogus = 1").is_err());
    let zero: RetentionConfig = toml::from_str("[tui_log]\nmax_files = 0").map_err(ctx("toml"))?;
    assert!(zero.validate().is_err());
    Ok(())
}

#[test]
fn test_class_sweep_walks_profile_directories_and_matches_names() -> TestResult {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let sessions = home
        .path()
        .join("profiles")
        .join("default")
        .join("sessions");
    fs::create_dir_all(&sessions).map_err(ctx("mkdir"))?;
    let now = SystemTime::now();
    let old = now - Duration::from_secs(400 * 86_400);
    for name in ["a.jsonl.corrupt-1", "b.jsonl", "c.jsonl"] {
        let file = File::create(sessions.join(name)).map_err(ctx("create"))?;
        file.set_modified(old).map_err(ctx("mtime"))?;
    }
    let roots = Roots {
        home: home.path().to_path_buf(),
        project: None,
    };

    // Opted-out: dry run reports, Apply refused, nothing deleted.
    let cfg = RetentionConfig::default();
    let backups = policy_for(&cfg, "session_corrupt_backups").ok_or(TestError::Missing("class"))?;
    assert!(backups.sweep(&roots, SweepMode::Apply, None, now).is_err());
    let outcomes = backups
        .sweep(&roots, SweepMode::DryRun, None, now)
        .map_err(ctx("dry run"))?;
    assert_eq!(outcomes.len(), 1);
    let report = outcomes[0].result.as_ref().map_err(ctx("report"))?;
    assert_eq!(report.removed.len(), 0, "newest is kept (keep_newest = 1)");
    assert_eq!(sessions.read_dir().map_err(ctx("ls"))?.count(), 3);

    // Opted-in transcripts: only `*.jsonl` matched, keep_newest = 5 keeps all.
    let cfg: RetentionConfig =
        toml::from_str("[session_transcripts]\nenabled = true\nkeep_newest = 1")
            .map_err(ctx("toml"))?;
    let transcripts = policy_for(&cfg, "session_transcripts").ok_or(TestError::Missing("class"))?;
    let outcomes = transcripts
        .sweep(&roots, SweepMode::Apply, None, now)
        .map_err(ctx("apply"))?;
    let report = outcomes[0].result.as_ref().map_err(ctx("report"))?;
    assert_eq!(report.removed.len(), 1);
    assert!(sessions.join("a.jsonl.corrupt-1").exists());
    Ok(())
}
