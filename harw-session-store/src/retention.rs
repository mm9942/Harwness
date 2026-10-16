//! Retention sweeps for the session store (`harw-retention` classes
//! `session_transcripts`, `session_corrupt_backups`, `freeze_resolved`).
//!
//! All three classes are **security-relevant and opt-in**: with the default
//! config nothing is ever deleted, [`SweepMode::DryRun`] only reports, and
//! [`SweepMode::Apply`] is refused with [`RetentionError::OptInRequired`]
//! unless the config says `enabled = true`.
//!
//! The age/count/bytes/`keep_newest` *selection* is `harw_retention::sweep`
//! run as a dry plan; this module then adds the store-specific safety checks
//! and performs each delete itself, independently, failing closed:
//!
//! - **Transcripts**: a transcript is never deleted while its session may be
//!   in use: the `<id>.jsonl.lock` is held by a writer, a caller-supplied
//!   `is_attached` hook says so, the session has an unresolved approval
//!   (`approvals/<id>/*.pending.json` without `.resolved.json`), the
//!   approvals directory cannot be read, or the file changed since planning.
//!   An apply holds the transcript lock across the unlink, so no append can
//!   interleave. The `.lock` file itself is left in place (removing a lock
//!   file others may have opened is racy).
//! - **Corrupt backups** (`<name>.corrupt-<ts>[-<n>]`): immutable quarantine
//!   files; only regular files that did not change since planning are removed.
//! - **Freeze records**: only `*.resolved.json` is ever touched, never an
//!   `.active.json`. A resolved record whose key still has an `.active.json`
//!   (a crash between persist and remove) is kept, because deleting it would
//!   resurrect the freeze; an unparsable resolved record is kept; the freeze
//!   directory lock is held for the whole apply (contended = abort, nothing
//!   deleted).
//!
//! Deadline: checked before every item; on expiry the remaining items are
//! left untouched (`timed_out`). Each delete is independent, so a partial run
//! leaves a consistent store.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use fs4::FileExt;
use harw_retention::{
    CLASSES, Class, ClassConfig, ItemError, Removal, ResolvedClass, RetentionError, SweepMode,
};
use harw_types::SessionId;

use crate::freeze::{FreezeResolution, FreezeStore};
use crate::store::{lock_transcript, safe_session_component};

/// Which store class to sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreClass {
    /// `session_transcripts`: `<id>.jsonl` below the sessions directory.
    SessionTranscripts,
    /// `session_corrupt_backups`: `<name>.corrupt-<ts>` below the sessions
    /// directory.
    SessionCorruptBackups,
    /// `freeze_resolved`: `<home>/freeze/*.resolved.json`.
    FreezeResolved,
}

impl StoreClass {
    /// The `harw-retention` class id (also the `[retention.<id>]` key).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::SessionTranscripts => "session_transcripts",
            Self::SessionCorruptBackups => "session_corrupt_backups",
            Self::FreezeResolved => "freeze_resolved",
        }
    }

    fn class(self) -> Option<&'static Class> {
        CLASSES.iter().find(|class| class.id == self.id())
    }
}

/// One item the sweep deliberately did not remove although the policy
/// selected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protected {
    /// The file that was kept.
    pub path: PathBuf,
    /// Why (stable short text).
    pub reason: &'static str,
}

/// Outcome of one class sweep. In dry-run mode `removed` lists what *would*
/// be removed (after the safety checks).
#[derive(Debug, Default)]
pub struct StoreSweepReport {
    /// The class id.
    pub class: &'static str,
    /// Nothing was deleted.
    pub dry_run: bool,
    /// Removed (or, dry run, removable) files.
    pub removed: Vec<Removal>,
    /// Selected by the policy but kept for safety.
    pub protected: Vec<Protected>,
    /// Per-item failures (the sweep went on).
    pub errors: Vec<ItemError>,
    /// Files in scope that stay.
    pub kept: usize,
    /// Entries that were not candidates (symlinks, locks, directories, ...).
    pub skipped: usize,
    /// The deadline expired; the rest was left untouched.
    pub timed_out: bool,
}

/// Optional hooks of a sweep.
#[derive(Default)]
pub struct SweepOptions<'a> {
    /// Returns `true` for a session id (file stem) that a live host has
    /// attached; such a transcript is never deleted.
    pub is_attached: Option<&'a dyn Fn(&str) -> bool>,
}

/// Maps `[session] retention_days` onto the `session_transcripts` class
/// config: an explicit `[retention.session_transcripts] max_age_secs` wins;
/// otherwise `retention_days` days become the age limit. `0` days is ignored
/// (the class default stays), it never means "delete everything". This does
/// not enable the class.
#[must_use]
pub fn transcript_class_config(retention_days: u32, base: &ClassConfig) -> ClassConfig {
    let mut cfg = base.clone();
    if cfg.max_age_secs.is_none() && retention_days > 0 {
        cfg.max_age_secs = Some(u64::from(retention_days).saturating_mul(86_400));
    }
    cfg
}

/// Sweeps one store class under `root` with the current time and no hooks.
///
/// `root` is the directory the owning store is constructed with: the
/// sessions directory (`TranscriptStore::new`) for the two session classes,
/// the harw home (`FreezeStore::new`) for `freeze_resolved`.
///
/// # Errors
/// [`RetentionError::OptInRequired`] for [`SweepMode::Apply`] without
/// `enabled = true`; [`RetentionError::Io`]/[`RetentionError::NotADirectory`]
/// when the directory cannot be listed or the freeze lock is contended
/// (nothing is deleted then).
pub fn retention_sweep(
    root: &Path,
    class: StoreClass,
    cfg: &ClassConfig,
    mode: SweepMode,
    deadline: Option<Instant>,
) -> Result<StoreSweepReport, RetentionError> {
    retention_sweep_at(
        root,
        class,
        cfg,
        mode,
        deadline,
        SystemTime::now(),
        &SweepOptions::default(),
    )
}

/// [`retention_sweep`] with an injected clock and hooks.
///
/// # Errors
/// See [`retention_sweep`].
pub fn retention_sweep_at(
    root: &Path,
    class: StoreClass,
    cfg: &ClassConfig,
    mode: SweepMode,
    deadline: Option<Instant>,
    now: SystemTime,
    options: &SweepOptions<'_>,
) -> Result<StoreSweepReport, RetentionError> {
    let Some(declared) = class.class() else {
        return Err(RetentionError::UnknownClass(class.id().to_owned()));
    };
    let resolved = ResolvedClass::resolve(declared, cfg);
    // Refuses Apply without explicit opt-in before anything is touched.
    let policy = resolved.policy(mode, deadline)?;
    let apply = mode == SweepMode::Apply;
    let dir = match class {
        StoreClass::FreezeResolved => root.join("freeze"),
        _ => root.to_path_buf(),
    };

    // Snapshot first, plan second: a write between the two is detected by
    // the pre-delete recheck.
    let snapshot = if apply {
        snapshot_mtimes(&dir)
    } else {
        HashMap::new()
    };
    let mut plan_policy = policy.clone();
    plan_policy.dry_run = true;
    let plan = harw_retention::sweep(&dir, &plan_policy, now)?;

    let mut report = StoreSweepReport {
        class: class.id(),
        dry_run: !apply,
        kept: plan.kept,
        skipped: plan.skipped,
        errors: plan.errors,
        timed_out: plan.timed_out,
        ..StoreSweepReport::default()
    };

    // Apply on the freeze store serialises with resolve/reconcile.
    let freeze_lock = if apply && class == StoreClass::FreezeResolved && !plan.removed.is_empty() {
        match FreezeStore::new(root).lock() {
            Ok(lock) => Some(lock),
            Err(error) => {
                return Err(RetentionError::Io {
                    path: dir,
                    source: std::io::Error::other(format!("freeze store busy: {error}")),
                });
            }
        }
    } else {
        None
    };

    for removal in plan.removed {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            report.timed_out = true;
            break;
        }
        let outcome = match class {
            StoreClass::SessionTranscripts => {
                transcript_item(&dir, &removal.path, apply, &snapshot, options)
            }
            StoreClass::SessionCorruptBackups => backup_item(&removal.path, apply, &snapshot),
            StoreClass::FreezeResolved => freeze_item(&removal.path, apply, &snapshot),
        };
        match outcome {
            Item::Removed => report.removed.push(removal),
            Item::Protected(reason) => {
                report.kept += 1;
                report.protected.push(Protected {
                    path: removal.path,
                    reason,
                });
            }
            Item::Failed(message) => {
                report.kept += 1;
                report.errors.push(ItemError {
                    path: removal.path,
                    message,
                });
            }
        }
    }
    if let Some(lock) = freeze_lock {
        let _ = FileExt::unlock(&lock);
    }
    Ok(report)
}

enum Item {
    Removed,
    Protected(&'static str),
    Failed(String),
}

fn snapshot_mtimes(dir: &Path) -> HashMap<PathBuf, SystemTime> {
    let mut map = HashMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return map;
    };
    for entry in entries.filter_map(Result::ok) {
        if let Ok(meta) = fs::symlink_metadata(entry.path()) {
            if meta.file_type().is_file() {
                if let Ok(mtime) = meta.modified() {
                    map.insert(entry.path(), mtime);
                }
            }
        }
    }
    map
}

/// Still a regular file with the mtime seen before planning. `Ok(false)`
/// means the file is gone (nothing to do); `Err` is a protect reason.
fn unchanged(
    path: &Path,
    apply: bool,
    snapshot: &HashMap<PathBuf, SystemTime>,
) -> Result<bool, &'static str> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("cannot stat"),
    };
    if !meta.file_type().is_file() {
        return Err("not a regular file");
    }
    if apply {
        let current = meta.modified().map_err(|_| "no mtime")?;
        if snapshot.get(path) != Some(&current) {
            return Err("changed since planning");
        }
    }
    Ok(true)
}

fn remove_regular(path: &Path) -> Item {
    match fs::remove_file(path) {
        Ok(()) => Item::Removed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Item::Removed,
        Err(error) => Item::Failed(error.to_string()),
    }
}

fn backup_item(path: &Path, apply: bool, snapshot: &HashMap<PathBuf, SystemTime>) -> Item {
    match unchanged(path, apply, snapshot) {
        Err(reason) => Item::Protected(reason),
        Ok(false) => Item::Removed,
        Ok(true) if apply => remove_regular(path),
        Ok(true) => Item::Removed,
    }
}

fn transcript_item(
    dir: &Path,
    path: &Path,
    apply: bool,
    snapshot: &HashMap<PathBuf, SystemTime>,
    options: &SweepOptions<'_>,
) -> Item {
    let Some(stem) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".jsonl"))
    else {
        return Item::Protected("unexpected file name");
    };
    let id = SessionId::from_str(stem);
    if safe_session_component(&id).is_err() {
        return Item::Protected("unsafe session id");
    }
    if options.is_attached.is_some_and(|attached| attached(stem)) {
        return Item::Protected("session attached");
    }
    match pending_approvals(dir, stem) {
        Ok(false) => {}
        Ok(true) => return Item::Protected("unresolved approval"),
        Err(()) => return Item::Protected("approvals unreadable"),
    }
    let lock_path = path.with_extension("jsonl.lock");
    if !apply {
        return match lock_held(&lock_path) {
            Ok(false) => match unchanged(path, false, snapshot) {
                Ok(_) => Item::Removed,
                Err(reason) => Item::Protected(reason),
            },
            Ok(true) => Item::Protected("session active (locked)"),
            Err(()) => Item::Protected("lock state unknown"),
        };
    }
    let lock = match lock_transcript(path, &id) {
        Ok(lock) => lock,
        Err(crate::error::SessionStoreError::LockContended { .. }) => {
            return Item::Protected("session active (locked)");
        }
        Err(_) => return Item::Protected("lock state unknown"),
    };
    let item = match unchanged(path, true, snapshot) {
        Err(reason) => Item::Protected(reason),
        Ok(false) => Item::Removed,
        Ok(true) => remove_regular(path),
    };
    let _ = FileExt::unlock(&lock);
    item
}

/// Whether `<dir>/approvals/<stem>` has a `*.pending.json` without its
/// `*.resolved.json`. `Err(())`: unreadable (fail closed).
fn pending_approvals(dir: &Path, stem: &str) -> Result<bool, ()> {
    let session_dir = dir.join("approvals").join(stem);
    let entries = match fs::read_dir(&session_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(()),
    };
    for entry in entries {
        let entry = entry.map_err(|_| ())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(());
        };
        if let Some(request) = name.strip_suffix(".pending.json") {
            let resolved = session_dir.join(format!("{request}.resolved.json"));
            match fs::symlink_metadata(resolved) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
                Err(_) => return Err(()),
            }
        }
    }
    Ok(false)
}

/// Read-only probe of a transcript lock (never creates the lock file).
fn lock_held(lock_path: &Path) -> Result<bool, ()> {
    match fs::symlink_metadata(lock_path) {
        Ok(meta) if meta.file_type().is_file() => {}
        Ok(_) => return Err(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(()),
    }
    let file = OpenOptions::new()
        .read(true)
        .open(lock_path)
        .map_err(|_| ())?;
    match FileExt::try_lock(&file) {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            Ok(false)
        }
        Err(fs4::TryLockError::WouldBlock) => Ok(true),
        Err(fs4::TryLockError::Error(_)) => Err(()),
    }
}

fn freeze_item(path: &Path, apply: bool, snapshot: &HashMap<PathBuf, SystemTime>) -> Item {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Item::Protected("unexpected file name");
    };
    let Some(stem) = name.strip_suffix(".resolved.json") else {
        return Item::Protected("not a resolved record");
    };
    // A still-present `.active.json` for the same key would reappear if the
    // resolved marker went away.
    let active = path.with_file_name(format!("{stem}.active.json"));
    match fs::symlink_metadata(&active) {
        Ok(_) => return Item::Protected("active record still present"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Item::Protected("cannot check active record"),
    }
    match unchanged(path, apply, snapshot) {
        Err(reason) => return Item::Protected(reason),
        Ok(false) => return Item::Removed,
        Ok(true) => {}
    }
    let parsed = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<FreezeResolution>(&bytes).ok());
    if parsed.is_none() {
        return Item::Protected("unreadable resolved record");
    }
    if apply {
        remove_regular(path)
    } else {
        Item::Removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::freeze::Freeze;
    use crate::record::{RecordKind, TranscriptRecord};
    use crate::store::TranscriptStore;
    use crate::test_support::{TestResult, ctx};
    use harw_types::{CgroupId, FindingId, ThreadRef};
    use std::time::Duration;

    fn age(path: &Path, days: u64) -> TestResult {
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_modified(SystemTime::now() - Duration::from_secs(days * 86_400))?;
        Ok(())
    }

    fn append(store: &TranscriptStore, session: &str) -> TestResult {
        store.append(&TranscriptRecord::new(
            SessionId::from_str(session),
            ThreadRef::from_str("root"),
            0,
            jiff::Timestamp::now(),
            RecordKind::Turn,
            serde_json::json!({}),
        ))?;
        Ok(())
    }

    fn enabled() -> ClassConfig {
        ClassConfig {
            enabled: Some(true),
            keep_newest: Some(1),
            ..ClassConfig::default()
        }
    }

    fn sweep_t(
        store: &TranscriptStore,
        class: StoreClass,
        cfg: &ClassConfig,
        mode: SweepMode,
        options: &SweepOptions<'_>,
    ) -> TestResult<StoreSweepReport> {
        retention_sweep_at(
            store.root(),
            class,
            cfg,
            mode,
            None,
            SystemTime::now(),
            options,
        )
        .map_err(ctx("sweep"))
    }

    fn names(report: &StoreSweepReport) -> Vec<String> {
        let mut out: Vec<String> = report
            .removed
            .iter()
            .filter_map(|r| {
                r.path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn default_config_never_deletes_in_apply() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        append(&store, "old-a")?;
        append(&store, "fresh")?;
        age(&temp.path().join("old-a.jsonl"), 400)?;
        std::fs::write(temp.path().join("x.jsonl.corrupt-1"), b"x")?;
        age(&temp.path().join("x.jsonl.corrupt-1"), 400)?;
        for class in [
            StoreClass::SessionTranscripts,
            StoreClass::SessionCorruptBackups,
        ] {
            let result =
                store.retention_sweep(class, &ClassConfig::default(), SweepMode::Apply, None);
            assert!(matches!(result, Err(RetentionError::OptInRequired(_))));
        }
        let off = ClassConfig {
            enabled: Some(false),
            ..ClassConfig::default()
        };
        let result =
            store.retention_sweep(StoreClass::SessionTranscripts, &off, SweepMode::Apply, None);
        assert!(matches!(result, Err(RetentionError::OptInRequired(_))));
        assert!(temp.path().join("old-a.jsonl").exists());
        assert!(temp.path().join("x.jsonl.corrupt-1").exists());
        Ok(())
    }

    #[test]
    fn dry_run_lists_expired_without_deleting() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        append(&store, "old-a")?;
        append(&store, "fresh")?;
        age(&temp.path().join("old-a.jsonl"), 400)?;
        let cfg = ClassConfig {
            keep_newest: Some(1),
            ..ClassConfig::default()
        };
        let report = sweep_t(
            &store,
            StoreClass::SessionTranscripts,
            &cfg,
            SweepMode::DryRun,
            &SweepOptions::default(),
        )?;
        assert!(report.dry_run);
        assert_eq!(names(&report), vec!["old-a.jsonl".to_owned()]);
        assert!(temp.path().join("old-a.jsonl").exists());
        Ok(())
    }

    #[test]
    fn enabled_deletes_only_expired_inactive_transcripts() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        for id in [
            "old-gone",
            "old-locked",
            "old-attached",
            "old-pending",
            "old-resolved",
            "fresh",
        ] {
            append(&store, id)?;
        }
        let pending = temp.path().join("approvals").join("old-pending");
        std::fs::create_dir_all(&pending)?;
        std::fs::write(pending.join("req-1.pending.json"), b"{}")?;
        let resolved = temp.path().join("approvals").join("old-resolved");
        std::fs::create_dir_all(&resolved)?;
        std::fs::write(resolved.join("req-1.pending.json"), b"{}")?;
        std::fs::write(resolved.join("req-1.resolved.json"), b"{}")?;
        for id in [
            "old-gone",
            "old-locked",
            "old-attached",
            "old-pending",
            "old-resolved",
        ] {
            age(&temp.path().join(format!("{id}.jsonl")), 400)?;
        }
        std::fs::write(temp.path().join("notes.txt"), b"n")?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            temp.path().join("fresh.jsonl"),
            temp.path().join("link.jsonl"),
        )?;

        let held = lock_transcript(
            &temp.path().join("old-locked.jsonl"),
            &SessionId::from_str("old-locked"),
        )?;
        let attached = |id: &str| id == "old-attached";
        let options = SweepOptions {
            is_attached: Some(&attached),
        };
        let report = sweep_t(
            &store,
            StoreClass::SessionTranscripts,
            &enabled(),
            SweepMode::Apply,
            &options,
        )?;
        let _ = FileExt::unlock(&held);

        assert_eq!(
            names(&report),
            vec!["old-gone.jsonl".to_owned(), "old-resolved.jsonl".to_owned()]
        );
        let mut protected: Vec<_> = report.protected.iter().map(|p| p.reason).collect();
        protected.sort_unstable();
        assert_eq!(
            protected,
            vec![
                "session active (locked)",
                "session attached",
                "unresolved approval"
            ]
        );
        assert!(!temp.path().join("old-gone.jsonl").exists());
        assert!(!temp.path().join("old-resolved.jsonl").exists());
        for id in ["old-locked", "old-attached", "old-pending", "fresh"] {
            assert!(temp.path().join(format!("{id}.jsonl")).exists(), "{id}");
        }
        assert!(temp.path().join("notes.txt").exists());
        let again = sweep_t(
            &store,
            StoreClass::SessionTranscripts,
            &enabled(),
            SweepMode::Apply,
            &SweepOptions::default(),
        )?;
        assert!(names(&again).contains(&"old-locked.jsonl".to_owned()));
        Ok(())
    }

    #[test]
    fn dry_run_marks_locked_sessions_protected() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        append(&store, "old-locked")?;
        append(&store, "fresh")?;
        age(&temp.path().join("old-locked.jsonl"), 400)?;
        let held = lock_transcript(
            &temp.path().join("old-locked.jsonl"),
            &SessionId::from_str("old-locked"),
        )?;
        let report = sweep_t(
            &store,
            StoreClass::SessionTranscripts,
            &enabled(),
            SweepMode::DryRun,
            &SweepOptions::default(),
        )?;
        let _ = FileExt::unlock(&held);
        assert!(report.removed.is_empty());
        assert_eq!(report.protected.len(), 1);
        Ok(())
    }

    #[test]
    fn corrupt_backups_are_opt_in_and_leave_transcripts_alone() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        append(&store, "s1")?;
        age(&temp.path().join("s1.jsonl"), 400)?;
        let old = temp.path().join("s1.jsonl.corrupt-100");
        let recent = temp.path().join("s1.jsonl.corrupt-200");
        std::fs::write(&old, b"torn")?;
        std::fs::write(&recent, b"torn")?;
        age(&old, 100)?;
        let report = sweep_t(
            &store,
            StoreClass::SessionCorruptBackups,
            &enabled(),
            SweepMode::Apply,
            &SweepOptions::default(),
        )?;
        assert_eq!(names(&report), vec!["s1.jsonl.corrupt-100".to_owned()]);
        assert!(!old.exists());
        assert!(recent.exists());
        assert!(temp.path().join("s1.jsonl").exists());
        Ok(())
    }

    fn freeze_rec(finding: &str, secs: i64) -> TestResult<Freeze> {
        Ok(Freeze {
            cgroup: CgroupId::from_str("cg-1"),
            finding: FindingId::from_str(finding),
            frozen_at: jiff::Timestamp::from_second(secs).map_err(ctx("ts"))?,
            expires_at: None,
        })
    }

    fn listing(store: &FreezeStore, suffix: &str) -> TestResult<Vec<String>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(store.root())? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with(suffix) {
                out.push(name);
            }
        }
        out.sort();
        Ok(out)
    }

    #[test]
    fn freeze_resolved_sweep_never_touches_unresolved_records() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = FreezeStore::new(temp.path());
        let now = jiff::Timestamp::now();
        let old_resolved = freeze_rec("f-old", 1_000)?;
        let fresh_resolved = freeze_rec("f-fresh", 2_000)?;
        let stale_active = freeze_rec("f-stale", 3_000)?;
        let unresolved = freeze_rec("f-open", 4_000)?;
        for rec in [&old_resolved, &fresh_resolved, &stale_active, &unresolved] {
            store.freeze(rec)?;
        }
        for rec in [&old_resolved, &fresh_resolved, &stale_active] {
            store.resolve(&rec.cgroup, &rec.finding, rec.frozen_at, now)?;
        }
        // Crash simulation: a resolved key whose `.active.json` is still there.
        let stale_active_path = store.root().join(format!(
            "cg-1.f-stale.p{:020}.active.json",
            3_000_i64 * 1_000_000_000
        ));
        std::fs::write(&stale_active_path, serde_json::to_vec(&stale_active)?)?;
        let fresh_path = store.root().join(format!(
            "cg-1.f-fresh.p{:020}.resolved.json",
            2_000_i64 * 1_000_000_000
        ));
        for entry in std::fs::read_dir(store.root())? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") && path != fresh_path {
                age(&path, 400)?;
            }
        }
        let active_before = listing(&store, ".active.json")?;
        assert_eq!(active_before.len(), 2);

        let refused = store.retention_sweep(&ClassConfig::default(), SweepMode::Apply, None);
        assert!(matches!(refused, Err(RetentionError::OptInRequired(_))));
        assert_eq!(listing(&store, ".resolved.json")?.len(), 3);

        let dry_cfg = ClassConfig {
            keep_newest: Some(1),
            ..ClassConfig::default()
        };
        let dry = store
            .retention_sweep(&dry_cfg, SweepMode::DryRun, None)
            .map_err(ctx("dry"))?;
        assert_eq!(dry.removed.len(), 1);
        assert_eq!(listing(&store, ".resolved.json")?.len(), 3);

        let report = store
            .retention_sweep(&enabled(), SweepMode::Apply, None)
            .map_err(ctx("apply"))?;
        assert_eq!(report.removed.len(), 1);
        assert!(
            report
                .protected
                .iter()
                .any(|p| p.reason == "active record still present")
        );
        assert_eq!(listing(&store, ".resolved.json")?.len(), 2);
        assert_eq!(listing(&store, ".active.json")?, active_before);
        assert!(fresh_path.exists());
        assert_eq!(store.active()?.len(), 1, "unresolved freeze stays in force");
        Ok(())
    }

    #[test]
    fn expired_deadline_leaves_everything_untouched() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = TranscriptStore::new(temp.path());
        append(&store, "old-a")?;
        append(&store, "fresh")?;
        age(&temp.path().join("old-a.jsonl"), 400)?;
        let report = store
            .retention_sweep(
                StoreClass::SessionTranscripts,
                &enabled(),
                SweepMode::Apply,
                Some(Instant::now()),
            )
            .map_err(ctx("sweep"))?;
        assert!(report.timed_out);
        assert!(temp.path().join("old-a.jsonl").exists());
        Ok(())
    }

    #[test]
    fn retention_days_maps_to_age_unless_overridden() -> TestResult {
        let base = ClassConfig::default();
        assert_eq!(
            transcript_class_config(30, &base).max_age_secs,
            Some(30 * 86_400)
        );
        assert_eq!(transcript_class_config(0, &base).max_age_secs, None);
        let explicit = ClassConfig {
            max_age_secs: Some(5),
            ..ClassConfig::default()
        };
        assert_eq!(transcript_class_config(30, &explicit).max_age_secs, Some(5));
        assert!(transcript_class_config(30, &base).enabled.is_none());
        Ok(())
    }
}
