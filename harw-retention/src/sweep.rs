//! The sweep engine: plan first, then delete independent files.
//!
//! Safety properties:
//! - never follows symlinks (`symlink_metadata`/`lstat`; only regular files
//!   are candidates; a symlinked sweep directory is refused);
//! - unlink only: no directory is ever removed, nothing is truncated;
//! - skips `*.lock` files and dot-files (temp/in-flight writes);
//! - the plan is computed without touching the disk, each delete is
//!   independent and idempotent (an already-missing file is not an error),
//!   and the deadline is re-checked before every delete, so a timed-out
//!   sweep leaves a consistent partial result.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::policy::{ItemError, Removal, RemovalReason, Report, RetentionError, RetentionPolicy};

struct Candidate {
    path: PathBuf,
    bytes: u64,
    mtime: SystemTime,
}

/// Sweeps one directory (non-recursive) according to `policy`.
///
/// `now` is injected for deterministic tests. A missing directory yields an
/// empty report. See the module docs for the safety properties.
///
/// # Errors
/// [`RetentionError::Io`] when the directory cannot be inspected or listed,
/// [`RetentionError::NotADirectory`] when it is a symlink or not a
/// directory. Per-file failures are collected in [`Report::errors`].
pub fn sweep(
    dir: &Path,
    policy: &RetentionPolicy,
    now: SystemTime,
) -> Result<Report, RetentionError> {
    let deadline = policy.deadline;
    sweep_with(dir, policy, now, &mut || {
        deadline.is_some_and(|d| Instant::now() >= d)
    })
}

/// [`sweep`] with an injectable "deadline expired" probe, called before
/// every delete (also what makes partial timeouts testable).
fn sweep_with(
    dir: &Path,
    policy: &RetentionPolicy,
    now: SystemTime,
    expired: &mut dyn FnMut() -> bool,
) -> Result<Report, RetentionError> {
    let mut report = Report {
        dry_run: policy.dry_run,
        ..Report::default()
    };
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_dir() => {}
        Ok(_) => return Err(RetentionError::NotADirectory(dir.to_path_buf())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(source) => {
            return Err(RetentionError::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    }
    let mut candidates = collect(dir, policy, &mut report)?;
    // Newest first; ties broken by path so the plan is deterministic.
    candidates.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));

    let planned = plan(&candidates, policy, now);
    let mut gone = vec![false; candidates.len()];
    let mut removed_flags = vec![false; candidates.len()];
    for (idx, reason) in planned {
        if expired() {
            report.timed_out = true;
            break;
        }
        let Some(candidate) = candidates.get(idx) else {
            continue;
        };
        if !policy.dry_run {
            match unlink_regular(&candidate.path) {
                Unlink::Removed => {}
                Unlink::AlreadyGone => {
                    gone[idx] = true;
                    continue;
                }
                Unlink::Failed(message) => {
                    report.errors.push(ItemError {
                        path: candidate.path.clone(),
                        message,
                    });
                    continue;
                }
            }
        }
        removed_flags[idx] = true;
        report.removed.push(Removal {
            path: candidate.path.clone(),
            bytes: candidate.bytes,
            reason,
        });
    }
    for (idx, candidate) in candidates.iter().enumerate() {
        if !removed_flags[idx] && !gone[idx] {
            report.kept += 1;
            report.kept_bytes = report.kept_bytes.saturating_add(candidate.bytes);
        }
    }
    Ok(report)
}

fn collect(
    dir: &Path,
    policy: &RetentionPolicy,
    report: &mut Report,
) -> Result<Vec<Candidate>, RetentionError> {
    let entries = fs::read_dir(dir).map_err(|source| RetentionError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                report.errors.push(ItemError {
                    path: dir.to_path_buf(),
                    message: e.to_string(),
                });
                continue;
            }
        };
        let path = entry.path();
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            report.skipped += 1;
            continue;
        };
        if name.starts_with('.') || name.ends_with(".lock") {
            report.skipped += 1;
            continue;
        }
        // `symlink_metadata`: a symlink is reported as a symlink, never
        // resolved.
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                report.errors.push(ItemError {
                    path,
                    message: e.to_string(),
                });
                continue;
            }
        };
        if !meta.file_type().is_file() {
            report.skipped += 1;
            continue;
        }
        if !policy.name_match.matches(name) {
            continue;
        }
        let mtime = match meta.modified() {
            Ok(t) => t,
            Err(e) => {
                report.errors.push(ItemError {
                    path,
                    message: e.to_string(),
                });
                continue;
            }
        };
        out.push(Candidate {
            path,
            bytes: meta.len(),
            mtime,
        });
    }
    Ok(out)
}

/// Pure planning step over newest-first candidates. Returns
/// `(index, reason)` pairs, oldest file first. Never touches the disk.
fn plan(
    candidates: &[Candidate],
    policy: &RetentionPolicy,
    now: SystemTime,
) -> Vec<(usize, RemovalReason)> {
    let protected = policy.keep_newest;
    let mut reason: Vec<Option<RemovalReason>> = vec![None; candidates.len()];

    // 1. Age. A future mtime counts as age zero.
    if let Some(max_age) = policy.max_age {
        for (idx, c) in candidates.iter().enumerate().skip(protected) {
            let age = now.duration_since(c.mtime).unwrap_or(Duration::ZERO);
            if age > max_age {
                reason[idx] = Some(RemovalReason::Age);
            }
        }
    }
    // 2. Count: drop the oldest survivors beyond `max_files`.
    if let Some(max_files) = policy.max_files {
        let mut survivors = reason.iter().filter(|r| r.is_none()).count();
        for idx in (protected..candidates.len()).rev() {
            if survivors <= max_files {
                break;
            }
            if reason[idx].is_none() {
                reason[idx] = Some(RemovalReason::Count);
                survivors -= 1;
            }
        }
    }
    // 3. Bytes: drop the oldest survivors until the total fits.
    if let Some(max_bytes) = policy.max_bytes {
        let mut total: u64 = candidates
            .iter()
            .zip(&reason)
            .filter(|(_, r)| r.is_none())
            .map(|(c, _)| c.bytes)
            .fold(0, u64::saturating_add);
        for idx in (protected..candidates.len()).rev() {
            if total <= max_bytes {
                break;
            }
            if reason[idx].is_none() {
                reason[idx] = Some(RemovalReason::Bytes);
                total = total.saturating_sub(candidates[idx].bytes);
            }
        }
    }
    // Oldest first.
    (0..candidates.len())
        .rev()
        .filter_map(|idx| reason[idx].map(|r| (idx, r)))
        .collect()
}

enum Unlink {
    Removed,
    AlreadyGone,
    Failed(String),
}

/// Re-checks with `lstat` right before the unlink (the file may have been
/// replaced by a symlink or directory since planning) and unlinks only a
/// regular file. `remove_file` never follows a symlink anyway.
fn unlink_regular(path: &Path) -> Unlink {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => {}
        Ok(_) => return Unlink::Failed("no longer a regular file; left untouched".to_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Unlink::AlreadyGone,
        Err(e) => return Unlink::Failed(e.to_string()),
    }
    match fs::remove_file(path) {
        Ok(()) => Unlink::Removed,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Unlink::AlreadyGone,
        Err(e) => Unlink::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    harw_test_support::define_test_error!(pub(crate));

    fn ctx_io<T>(r: std::io::Result<T>, what: &'static str) -> TestResult<T> {
        r.map_err(|e| TestError::Context {
            context: what,
            source: e.to_string(),
        })
    }

    #[test]
    fn test_deadline_expiring_mid_sweep_returns_partial_report() -> TestResult {
        let tmp = ctx_io(tempfile::tempdir(), "tempdir")?;
        let now = SystemTime::now();
        for i in 0..5u64 {
            let path = tmp.path().join(format!("f{i}.log"));
            let file = ctx_io(File::create(&path), "create")?;
            ctx_io(
                file.set_modified(now - Duration::from_secs(1000 + i)),
                "set_modified",
            )?;
        }
        let policy = RetentionPolicy {
            max_files: Some(0),
            keep_newest: 0,
            ..RetentionPolicy::unlimited()
        };
        let mut budget = 2;
        let mut expired = || {
            if budget == 0 {
                true
            } else {
                budget -= 1;
                false
            }
        };
        let report = sweep_with(tmp.path(), &policy, now, &mut expired)
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(report.timed_out);
        assert_eq!(report.removed.len(), 2);
        assert_eq!(report.kept, 3);
        // Oldest first.
        assert!(report.removed[0].path.ends_with("f4.log"));
        assert!(report.removed[1].path.ends_with("f3.log"));
        let left = ctx_io(std::fs::read_dir(tmp.path()), "read_dir")?.count();
        assert_eq!(left, 3);
        // Re-running finishes the job (independent, idempotent deletes).
        let again =
            sweep(tmp.path(), &policy, now).map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(!again.timed_out);
        assert_eq!(again.removed.len(), 3);
        Ok(())
    }
}
