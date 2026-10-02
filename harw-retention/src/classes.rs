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
//! Security-relevant classes (`dod_spool`, `sentinel_export`,
//! `freeze_resolved`, `session_transcripts`, `session_corrupt_backups`) are
//! opt-in: never deleted unless config says `enabled = true`. Their limits
//! below only take effect after that opt-in.

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

fn dod_spool_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("dod").join("spool")]
}

fn sentinel_export_dirs(roots: &Roots) -> Vec<PathBuf> {
    vec![roots.home.join("dod").join("export")]
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
    /// DoD sentinel spool (security evidence; opt-in).
    dod_spool: security_relevant {
        dir = dod_spool_dirs,
        name = any,
        max_age_secs = 30 * 86_400,
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
