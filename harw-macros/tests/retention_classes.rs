//! Expansion tests for `retention_classes!`: the one declaration list must
//! yield the config struct (serde defaults, `deny_unknown_fields`), the
//! `CLASSES` registry and `policy_for` with the opt-in rules.

use std::path::PathBuf;
use std::time::Duration;

use harw_retention::{ClassKind, NameMatch, Roots, SweepMode};

harw_test_support::define_test_error!(pub(crate));

fn logs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("logs")]
}

fn evidence(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("evidence")]
}

harw_macros::retention_classes! {
    config = DemoConfig;

    /// Plain logs.
    demo_log: ephemeral {
        dir = logs,
        name = prefix_suffix("demo", ".log"),
        max_age_secs = 3_600,
        max_bytes = 1_048_576,
        max_files = 5,
    }
    /// Findings that must never vanish by default.
    demo_evidence: security_relevant {
        dir = evidence,
        name = any,
        max_age_secs = 86_400,
        max_bytes = none,
        max_files = none,
        keep_newest = 3,
    }
}

#[test]
fn test_registry_lists_classes_in_declaration_order() -> TestResult {
    let ids: Vec<_> = CLASSES.iter().map(|c| c.id).collect();
    assert_eq!(ids, ["demo_log", "demo_evidence"]);
    assert_eq!(CLASSES[0].kind, ClassKind::Ephemeral);
    assert_eq!(CLASSES[1].kind, ClassKind::SecurityRelevant);
    assert_eq!(
        CLASSES[0].name_match,
        NameMatch::prefix_suffix("demo", ".log")
    );
    assert_eq!(CLASSES[0].defaults.max_files, Some(5));
    assert_eq!(CLASSES[0].defaults.keep_newest, 1);
    assert_eq!(CLASSES[1].defaults.keep_newest, 3);
    let roots = Roots {
        home: PathBuf::from("/h"),
        project: None,
    };
    assert_eq!(
        (CLASSES[1].resolve_dirs)(&roots),
        [PathBuf::from("/h/evidence")]
    );
    Ok(())
}

#[test]
fn test_config_defaults_from_empty_input() -> TestResult {
    let cfg: DemoConfig =
        serde_json::from_str("{}").map_err(|e| TestError::Unexpected(e.to_string()))?;
    assert_eq!(cfg, DemoConfig::default());
    assert!(cfg.validate().is_ok());
    assert!(cfg.class_config("demo_log").is_some());
    assert!(cfg.class_config("nope").is_none());
    Ok(())
}

#[test]
fn test_config_rejects_unknown_fields_at_both_levels() {
    assert!(serde_json::from_str::<DemoConfig>(r#"{"bogus": {}}"#).is_err());
    assert!(serde_json::from_str::<DemoConfig>(r#"{"demo_log": {"bogus": 1}}"#).is_err());
}

#[test]
fn test_policy_for_applies_defaults_and_opt_in_rules() -> TestResult {
    let cfg = DemoConfig::default();
    let log = policy_for(&cfg, "demo_log").ok_or(TestError::Missing("demo_log"))?;
    assert!(log.enabled);
    assert!(log.policy(SweepMode::Apply, None).is_ok());
    let ev = policy_for(&cfg, "demo_evidence").ok_or(TestError::Missing("demo_evidence"))?;
    assert!(!ev.enabled);
    assert!(ev.policy(SweepMode::Apply, None).is_err());
    let dry = ev
        .policy(SweepMode::DryRun, None)
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    assert_eq!(dry.max_age, Some(Duration::from_secs(86_400)));
    assert_eq!(dry.keep_newest, 3);
    assert!(policy_for(&cfg, "nope").is_none());
    assert_eq!(resolve_all(&cfg).len(), 2);
    Ok(())
}

#[test]
fn test_overrides_and_explicit_optin_flow_through() -> TestResult {
    let cfg: DemoConfig = serde_json::from_str(
        r#"{"demo_log": {"max_files": 2}, "demo_evidence": {"enabled": true, "max_files": 10}}"#,
    )
    .map_err(|e| TestError::Unexpected(e.to_string()))?;
    let log = policy_for(&cfg, "demo_log").ok_or(TestError::Missing("demo_log"))?;
    assert_eq!(log.max_files, Some(2));
    assert_eq!(log.max_age_secs, Some(3_600));
    let ev = policy_for(&cfg, "demo_evidence").ok_or(TestError::Missing("demo_evidence"))?;
    assert!(ev.enabled && ev.explicit_optin);
    assert!(ev.policy(SweepMode::Apply, None).is_ok());
    assert_eq!(ev.max_files, Some(10));
    Ok(())
}

#[test]
fn test_validate_rejects_zero_limits_and_serialization_omits_unset() -> TestResult {
    let cfg: DemoConfig = serde_json::from_str(r#"{"demo_log": {"max_bytes": 0}}"#)
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    let Err(msg) = cfg.validate() else {
        return Err(TestError::Unexpected("zero limit must fail".to_owned()));
    };
    assert!(msg.starts_with("retention.demo_log."), "{msg}");
    let out = serde_json::to_string(&DemoConfig::default())
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    assert_eq!(out, "{}");
    Ok(())
}
