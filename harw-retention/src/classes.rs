//! The initial class table: every retained data class declared once.
//!
//! `harw_macros::retention_classes!` derives [`RetentionConfig`] (the
//! `[retention]` config section), [`CLASSES`], [`policy_for`] and
//! [`resolve_all`] from this single list; config, doctor warnings and the
//! sweep job all read it.
//!
//! Directory resolvers are pure path computations (plus a non-recursive
//! listing of `profiles/` for per-profile classes) and mirror the layout of
//! `harw-home`; the owning crates may refine a resolver here without
//! touching any consumer. Limits are the *defaults*; config overrides them
//! per class (`[retention.<id>]`).
//!
//! Security-relevant classes (`dod_spool`, `sentinel_export`, `dod_verdicts`,
//! `dod_labels`, `freeze_resolved`, `session_transcripts`,
//! `session_corrupt_backups`) are opt-in: never deleted unless config says
//! `enabled = true`. Their limits below only take effect after that opt-in.
//!
//! # DoD classes (PL-100 WP0.12)
//! - `dod_spool`: 14 days, aligned with the sentinel spool retention (the
//!   escalator only needs records of findings that can still be triaged).
//!   Covers the harw-side copy under `<home>/dod/spool`; the system
//!   sentinel sweeps its own `@STATEDIR@/spool` with the same limits.
//! - `sentinel_export`: 90 days, unchanged.
//! - `dod_verdicts`: triage verdicts of the future `VerdictStore`
//!   (`<home>/dod/verdicts`), 90 days like the export they annotate.
//! - `dod_labels`: human-signed labels (`<home>/dod/labels`). No age limit —
//!   a label ends by its own expiry, never by file age — but a bounded count.
//!   Dropping a label can only re-enable notices, never suppress one.

use std::fs;
use std::path::PathBuf;

use crate::class::Roots;

fn tui_log_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("logs")]
}

fn telemetry_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("telemetry")]
}

fn bug_report_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("bug-report")]
}

fn scan_report_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("scan_reports")]
}

// The harw-side spool copy below the harw home. The system sentinel's own
// spool (`@STATEDIR@/spool`, e.g. `/var/lib/harw-dod/spool`, 0700
// harw-sentinel) is outside every harw home: the sentinel sweeps it itself
// with the same 14-day/1 GiB limits (`harw-sentinel/src/spool_writer.rs`).
fn dod_spool_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("dod").join("spool")]
}

fn sentinel_export_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("dod").join("export")]
}

// Triage verdicts (`VerdictStore`, PL-100 Phase 2) below the harw home.
fn dod_verdict_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("dod").join("verdicts")]
}

// Human-signed DoD labels (PL-100 I3) below the harw home.
fn dod_label_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("dod").join("labels")]
}

fn freeze_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("freeze")]
}

/// `<home>/profiles/<name>/<sub>` for every real (non-symlink) profile
/// directory, sorted by name. Missing `profiles/` yields no directories.
fn profile_subdirs(roots: &Roots, sub: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(roots.home.join("profiles")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| fs::symlink_metadata(e.path()).is_ok_and(|m| m.file_type().is_dir()))
        .map(|e| e.path().join(sub))
        .collect();
    dirs.sort();
    dirs
}

fn job_log_dirs(roots: &Roots) -> Vec<PathBuf> {
    profile_subdirs(roots, "jobs")
}

fn session_dirs(roots: &Roots) -> Vec<PathBuf> {
    profile_subdirs(roots, "sessions")
}

// Verified against the writers in `harw-session-store` (store-level sweeps
// with the extra active-session / unresolved-freeze safety checks live in
// `harw_session_store::retention`):
// - `session_transcripts`/`session_corrupt_backups`: `TranscriptStore` writes
//   `<sessions>/<id>.jsonl` (+ `.jsonl.lock`, `.jsonl.corrupt-<ts>[-<n>]`);
//   the CLI default is `<home>/profiles/<profile>/sessions`. A custom
//   `[session] store_dir` or the gateway's `<state_dir>/sessions` is NOT
//   covered by this home-relative resolver.
// - `freeze_resolved`: `FreezeStore::new(home)` writes `<home>/freeze/
//   <cgroup>.<finding>.<ts>.{active,resolved}.json`; the suffix matcher below
//   is exact.

harw_macros::retention_classes! {
    config = RetentionConfig;

    /// `tui.log` and its rotations (`tui.log.<ts>`, legacy `tui.log.1`) under
    /// `<home>/logs`; written by `harw-cli` (`TuiLogFile`).
    tui_log: ephemeral {
        dir = tui_log_dirs,
        name = prefix("tui.log"),
        max_age_secs = 14 * 86_400,
        max_bytes = 64 * 1_048_576,
        max_files = 5,
    }
    /// Telemetry `rotated-*.jsonl` under `<home>/telemetry`.
    telemetry_rotated: ephemeral {
        dir = telemetry_dirs,
        name = prefix_suffix("rotated-", ".jsonl"),
        max_age_secs = 7 * 86_400,
        max_bytes = 256 * 1_048_576,
        max_files = 20,
    }
    /// Job log files under `<home>/profiles/<name>/jobs`.
    job_logs: ephemeral {
        dir = job_log_dirs,
        name = suffix(".log"),
        max_age_secs = 14 * 86_400,
        max_bytes = 512 * 1_048_576,
        max_files = 200,
    }
    /// Locally stored bug reports `<id>.md` under `<home>/bug-report`
    /// (`harw_home::paths::bug_report_dir`; writer `harw-ops`).
    bug_reports: ephemeral {
        dir = bug_report_dirs,
        name = suffix(".md"),
        max_age_secs = 30 * 86_400,
        max_bytes = 100 * 1_048_576,
        max_files = 50,
    }
    /// Scan reports under `<home>/scan_reports`.
    scan_reports: ephemeral {
        dir = scan_report_dirs,
        name = any,
        max_age_secs = 30 * 86_400,
        max_bytes = 256 * 1_048_576,
        max_files = 100,
    }
    /// DoD sentinel spool (security evidence; opt-in). 14 days, aligned with
    /// the sentinel spool retention (PL-100 WP0.12).
    dod_spool: security_relevant {
        dir = dod_spool_dirs,
        name = any,
        max_age_secs = 14 * 86_400,
        max_bytes = 1_024 * 1_048_576,
        max_files = 10_000,
    }
    /// Sentinel export bundles (security evidence; opt-in).
    sentinel_export: security_relevant {
        dir = sentinel_export_dirs,
        name = any,
        max_age_secs = 90 * 86_400,
        max_bytes = 1_024 * 1_048_576,
        max_files = none,
    }
    /// DoD triage verdicts under `<home>/dod/verdicts` (security evidence;
    /// opt-in). 90 days, like the findings export they annotate.
    dod_verdicts: security_relevant {
        dir = dod_verdict_dirs,
        name = any,
        max_age_secs = 90 * 86_400,
        max_bytes = 256 * 1_048_576,
        max_files = none,
    }
    /// Human-signed DoD labels under `<home>/dod/labels` (security evidence;
    /// opt-in). No age limit (a label ends by its own expiry), bounded count.
    dod_labels: security_relevant {
        dir = dod_label_dirs,
        name = any,
        max_age_secs = none,
        max_bytes = none,
        max_files = 10_000,
    }
    /// Resolved freeze records `*.resolved.json` (security evidence; opt-in).
    freeze_resolved: security_relevant {
        dir = freeze_dirs,
        name = suffix(".resolved.json"),
        max_age_secs = 180 * 86_400,
        max_bytes = none,
        max_files = 1_000,
    }
    /// Session transcripts `*.jsonl` per profile (opt-in).
    session_transcripts: security_relevant {
        dir = session_dirs,
        name = suffix(".jsonl"),
        max_age_secs = 90 * 86_400,
        max_bytes = none,
        max_files = none,
        keep_newest = 5,
    }
    /// Quarantined `<name>.corrupt-<ts>` session backups (opt-in).
    session_corrupt_backups: security_relevant {
        dir = session_dirs,
        name = contains(".corrupt-"),
        max_age_secs = 30 * 86_400,
        max_bytes = 256 * 1_048_576,
        max_files = 50,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClassKind, ResolvedClass, SweepMode};

    harw_test_support::define_test_error!(pub(crate));

    const DAY: u64 = 86_400;

    fn class(cfg: &RetentionConfig, id: &'static str) -> TestResult<ResolvedClass> {
        policy_for(cfg, id).ok_or(TestError::Missing(id))
    }

    #[test]
    fn test_dod_spool_keeps_fourteen_days() -> TestResult {
        let spool = class(&RetentionConfig::default(), "dod_spool")?;
        assert_eq!(spool.class.kind, ClassKind::SecurityRelevant);
        assert_eq!(spool.max_age_secs, Some(14 * DAY));
        assert_eq!(spool.max_files, Some(10_000));
        Ok(())
    }

    #[test]
    fn test_dod_verdicts_and_labels_are_declared_security_relevant() -> TestResult {
        let cfg = RetentionConfig::default();
        let verdicts = class(&cfg, "dod_verdicts")?;
        assert_eq!(verdicts.class.kind, ClassKind::SecurityRelevant);
        assert_eq!(verdicts.max_age_secs, Some(90 * DAY));
        assert!(!verdicts.enabled, "opt-in");

        let labels = class(&cfg, "dod_labels")?;
        assert_eq!(labels.class.kind, ClassKind::SecurityRelevant);
        assert_eq!(labels.max_age_secs, None, "labels never age out");
        assert_eq!(labels.max_bytes, None);
        assert_eq!(labels.max_files, Some(10_000), "but their count is bounded");
        assert!(!labels.enabled, "opt-in");
        // Opted in, the label class has a limit, so the doctor stays quiet.
        let opted: RetentionConfig =
            toml::from_str("[dod_labels]\nenabled = true\n").map_err(ctx("toml"))?;
        let labels = class(&opted, "dod_labels")?;
        assert!(labels.doctor_warning().is_none());
        assert!(labels.policy(SweepMode::Apply, None).is_ok());
        Ok(())
    }

    #[test]
    fn test_dod_classes_resolve_below_the_home_dod_directory() -> TestResult {
        let roots = Roots {
            home: std::path::PathBuf::from("/home/u/.harw"),
            project: None,
        };
        let dod = roots.home.join("dod");
        for (id, sub) in [
            ("dod_spool", "spool"),
            ("sentinel_export", "export"),
            ("dod_verdicts", "verdicts"),
            ("dod_labels", "labels"),
        ] {
            let resolved = class(&RetentionConfig::default(), id)?;
            assert_eq!(
                (resolved.class.resolve_dirs)(&roots),
                vec![dod.join(sub)],
                "{id}"
            );
        }
        Ok(())
    }
}
