//! Managed installation and removal of shell completion scripts.
//!
//! Spec source: harw-completions CODING DESIGN, section 1, subsection `src/install.rs`.
//!
//! Responsibilities:
//! - sweep legacy completion files for a binary, removing only files that are provably
//!   harw/clap generated ([`crate::shell::is_ours`]); foreign files are reported and left alone,
//! - write the canonical completion script atomically (temp file + rename, permissions preserved),
//! - maintain a managed rc block (`# >>> harw completions (<bin>) >>>` ... `# <<< ... <<<`) and remove
//!   legacy `source <(<bin> completion ...)` one-liners, backing rc files up to `<rc>.harw-bak`,
//! - drop stale zsh `.zcompdump*` caches that reference `_<bin>`.
//!
//! `dry_run` performs zero filesystem mutations but produces the same [`InstallReport`].
//! Privileges are never escalated: a permission-denied removal yields a `sudo rm <path>` hint.
//!
//! Concurrency: plain synchronous filesystem I/O; no threads, no shared state.
//! Errors: [`CompletionError::Io`] for unexpected I/O failures, [`CompletionError::ForeignTarget`]
//! when the canonical path holds a file that is not harw-managed, plus whatever
//! [`resolve_locations`] reports.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clap_complete::Shell;
use tracing::{debug, info, warn};

use crate::error::{CompletionError, CompletionResult, io_err};
use crate::locations::{BlockPlacement, HomeEnv, Locations, RcBlock, resolve_locations};
use crate::shell::{block_begin, block_end, generate_script, is_ours};

/// Reason recorded for files that exist but were not written by harw/clap.
const FOREIGN_REASON: &str = "not a harw-managed completion file; left untouched";

/// Line appended inside a `BeforeCompinit` block when the rc file never runs `compinit`.
const COMPINIT_FALLBACK: &str = "autoload -Uz compinit && compinit";

/// Suffix of rc backups written before an rc file is modified.
const BACKUP_SUFFIX: &str = ".harw-bak";

/// Prefix of zsh completion dump caches.
const ZCOMPDUMP_PREFIX: &str = ".zcompdump";

/// Options controlling [`install`] / [`uninstall`] (design doc section 1, `src/install.rs`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstallOptions {
    /// Report what would happen without touching the filesystem.
    pub dry_run: bool,
}

/// A path that was inspected but deliberately left alone, with the reason why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPath {
    /// The inspected path.
    pub path: PathBuf,
    /// Human-readable reason the path was not touched.
    pub reason: String,
}

/// Outcome of an [`install`] or [`uninstall`] run (design doc section 1, `src/install.rs`).
///
/// In dry-run mode the report lists what *would* happen; its contents are otherwise identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// Binary the completions belong to.
    pub bin: String,
    /// Target shell.
    pub shell: Shell,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Whether this was an uninstall.
    pub uninstall: bool,
    /// Canonical script path (install only; set even when the content was already up to date).
    pub written: Option<PathBuf>,
    /// Removed (or, in dry-run mode, removable) completion files.
    pub removed: Vec<PathBuf>,
    /// Paths that were inspected but left untouched.
    pub skipped: Vec<SkippedPath>,
    /// rc files whose content changed.
    pub rc_updated: Vec<PathBuf>,
    /// Backups created before rc files were modified.
    pub backups: Vec<PathBuf>,
    /// Removed zsh completion dump caches.
    pub caches_removed: Vec<PathBuf>,
    /// Follow-up hints for the user (e.g. `sudo rm <path>`).
    pub hints: Vec<String>,
    /// Final instruction, e.g. "run `exec zsh` to reload completions in this shell".
    pub next_step: Option<String>,
}

impl InstallReport {
    // Empty report for one run.
    fn new(bin: &str, shell: Shell, dry_run: bool, uninstall: bool) -> Self {
        Self {
            bin: bin.to_owned(),
            shell,
            dry_run,
            uninstall,
            written: None,
            removed: Vec::new(),
            skipped: Vec::new(),
            rc_updated: Vec::new(),
            backups: Vec::new(),
            caches_removed: Vec::new(),
            hints: Vec::new(),
            next_step: None,
        }
    }

    // Records a path that is left untouched.
    fn skip(&mut self, path: &Path, reason: impl Into<String>) {
        let reason = reason.into();
        debug!(path = %path.display(), reason = %reason, "skipping path");
        self.skipped.push(SkippedPath {
            path: path.to_path_buf(),
            reason,
        });
    }
}

impl fmt::Display for InstallReport {
    /// Renders one item per line; verbs get a "would " prefix in dry-run mode.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let action = if self.uninstall { "uninstall" } else { "install" };
        write!(f, "harw completions {action} for {} ({})", self.shell, self.bin)?;
        if self.dry_run {
            f.write_str(" [dry run]")?;
        }
        let prefix = if self.dry_run { "would " } else { "" };
        let width = if self.dry_run { 12 } else { 7 };
        let verb = |name: &str| format!("{prefix}{name}");

        if let Some(path) = &self.written {
            write!(f, "\n  {:<width$} {}", verb("write"), path.display())?;
        }
        for path in &self.removed {
            write!(f, "\n  {:<width$} {}", verb("remove"), path.display())?;
        }
        for path in &self.rc_updated {
            write!(f, "\n  {:<width$} {}", verb("rc"), path.display())?;
            let backup = backup_path(path);
            if self.backups.contains(&backup) {
                write!(f, " (backup: {})", backup.display())?;
            }
        }
        for path in &self.caches_removed {
            write!(f, "\n  {:<width$} {}", verb("cache"), path.display())?;
        }
        for skipped in &self.skipped {
            write!(
                f,
                "\n  {:<width$} {}: {}",
                "skip",
                skipped.path.display(),
                skipped.reason
            )?;
        }
        for hint in &self.hints {
            write!(f, "\n  {:<width$} {hint}", "hint")?;
        }
        let nothing = self.written.is_none()
            && self.removed.is_empty()
            && self.rc_updated.is_empty()
            && self.caches_removed.is_empty()
            && self.skipped.is_empty()
            && self.hints.is_empty();
        if nothing {
            f.write_str("\n  nothing to do")?;
        }
        if let Some(next) = &self.next_step {
            write!(f, "\nNext: {next}.")?;
        }
        Ok(())
    }
}

/// Result of the pure rc rewrite performed by [`rewrite_rc`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcRewrite {
    /// New rc file content.
    pub content: String,
    /// A managed block begin marker had no matching end marker; the tail was kept unchanged.
    pub unterminated_block: bool,
}

/// Installs completions for `bin` in `shell`, replacing older harw-managed installations
/// (design doc section 1, `src/install.rs`, steps 1-6).
pub fn install(
    cmd: &mut clap::Command,
    bin: &str,
    shell: Shell,
    env: &HomeEnv,
    opts: &InstallOptions,
) -> CompletionResult<InstallReport> {
    let span = tracing::info_span!(
        "completions_install",
        bin = bin,
        shell = %shell,
        dry_run = opts.dry_run
    );
    let _guard = span.enter();

    let Locations {
        canonical,
        legacy,
        rc_block,
        rc_cleanup,
        cache_dirs,
        reload_hint,
        ..
    } = resolve_locations(env, bin, shell)?;
    info!(
        canonical = %canonical.display(),
        legacy = legacy.len(),
        rc_files = rc_cleanup.len(),
        "installing shell completions"
    );
    let mut report = InstallReport::new(bin, shell, opts.dry_run, false);

    // Step 3 pre-check runs before any mutation so a foreign canonical file aborts cleanly.
    let existing = read_optional(&canonical)?;
    let foreign = existing
        .as_deref()
        .is_some_and(|bytes| !is_ours(&String::from_utf8_lossy(bytes), bin));
    if foreign {
        return Err(CompletionError::ForeignTarget {
            path: canonical.display().to_string(),
        });
    }
    let script = generate_script(cmd, bin, shell);

    sweep_paths(&legacy, bin, opts.dry_run, &mut report)?;

    if existing.as_deref() == Some(script.as_slice()) {
        debug!(path = %canonical.display(), "completion script already up to date");
    } else if opts.dry_run {
        debug!(path = %canonical.display(), "dry run: would write completion script");
    } else {
        write_atomic(&canonical, &script)?;
        debug!(path = %canonical.display(), bytes = script.len(), "wrote completion script");
    }
    report.written = Some(canonical);

    update_rc_files(&rc_cleanup, bin, rc_block.as_ref(), opts.dry_run, &mut report)?;
    purge_caches(&cache_dirs, bin, opts.dry_run, &mut report)?;
    report.next_step = Some(next_step(&reload_hint));

    log_summary(&report);
    Ok(report)
}

/// Removes every harw-managed completion file, rc block, legacy rc line and cache for `bin`
/// (design doc section 1, `src/install.rs`; install minus steps 3 and block insertion).
pub fn uninstall(
    bin: &str,
    shell: Shell,
    env: &HomeEnv,
    opts: &InstallOptions,
) -> CompletionResult<InstallReport> {
    let span = tracing::info_span!(
        "completions_uninstall",
        bin = bin,
        shell = %shell,
        dry_run = opts.dry_run
    );
    let _guard = span.enter();

    let Locations {
        canonical,
        mut legacy,
        rc_cleanup,
        cache_dirs,
        reload_hint,
        ..
    } = resolve_locations(env, bin, shell)?;
    info!(
        canonical = %canonical.display(),
        legacy = legacy.len(),
        rc_files = rc_cleanup.len(),
        "uninstalling shell completions"
    );
    let mut report = InstallReport::new(bin, shell, opts.dry_run, true);

    if !legacy.contains(&canonical) {
        legacy.push(canonical);
    }
    sweep_paths(&legacy, bin, opts.dry_run, &mut report)?;
    update_rc_files(&rc_cleanup, bin, None, opts.dry_run, &mut report)?;
    purge_caches(&cache_dirs, bin, opts.dry_run, &mut report)?;
    report.next_step = Some(next_step(&reload_hint));

    log_summary(&report);
    Ok(report)
}

/// Rewrites rc `content`: drops managed blocks and legacy one-liners for `bin`, then inserts
/// `block` if given (design doc section 1, `rewrite_rc`). Pure and idempotent.
pub fn rewrite_rc(
    content: &str,
    bin: &str,
    block: Option<(&[String], BlockPlacement)>,
) -> RcRewrite {
    let begin = block_begin(bin);
    let end = block_end(bin);
    let ends_with_newline = content.ends_with('\n');

    let mut source: Vec<&str> = if content.is_empty() {
        Vec::new()
    } else {
        content.split('\n').collect()
    };
    if ends_with_newline {
        source.pop();
    }

    let mut kept: Vec<&str> = Vec::with_capacity(source.len());
    let mut unterminated_block = false;
    let mut idx = 0;
    while let Some(&line) = source.get(idx) {
        if line.trim() == begin {
            let rest = source.get(idx + 1..).unwrap_or_default();
            match rest.iter().position(|candidate| candidate.trim() == end) {
                Some(offset) => {
                    idx += offset + 2;
                    continue;
                }
                None => {
                    unterminated_block = true;
                    kept.extend(source.get(idx..).unwrap_or_default().iter().copied());
                    break;
                }
            }
        }
        if !is_legacy_rc_line(line, bin) {
            kept.push(line);
        }
        idx += 1;
    }

    let mut block_lines: Vec<&str> = Vec::new();
    let mut insert_at = kept.len();
    if let Some((body, placement)) = block.filter(|_| !unterminated_block) {
        let anchor = match placement {
            BlockPlacement::BeforeCompinit => kept.iter().position(|line| is_compinit_anchor(line)),
            BlockPlacement::End => None,
        };
        block_lines.push(&begin);
        block_lines.extend(body.iter().map(String::as_str));
        if placement == BlockPlacement::BeforeCompinit && anchor.is_none() {
            block_lines.push(COMPINIT_FALLBACK);
        }
        block_lines.push(&end);
        insert_at = anchor.unwrap_or(kept.len());
    }

    let block_added = !block_lines.is_empty();
    let (head, tail) = kept.split_at(insert_at.min(kept.len()));
    let mut out: Vec<&str> = Vec::with_capacity(kept.len() + block_lines.len());
    out.extend_from_slice(head);
    out.extend(block_lines);
    out.extend_from_slice(tail);

    if out.is_empty() {
        return RcRewrite {
            content: String::new(),
            unterminated_block,
        };
    }
    let mut text = out.join("\n");
    if ends_with_newline || block_added {
        text.push('\n');
    }
    RcRewrite {
        content: text,
        unterminated_block,
    }
}

/// Detects legacy rc one-liners such as `source <(bin completion zsh)` or
/// `eval "$(bin completions bash)"` (design doc section 1, `is_legacy_rc_line`).
pub fn is_legacy_rc_line(line: &str, bin: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || bin.is_empty() {
        return false;
    }
    let sources = trimmed.starts_with("source")
        || trimmed.starts_with("eval")
        || trimmed.starts_with(". ")
        || trimmed.contains("| source")
        || trimmed.contains("|source");
    if !sources {
        return false;
    }
    let needle = format!("{bin} completion");
    trimmed.match_indices(needle.as_str()).any(|(idx, _)| {
        trimmed
            .get(..idx)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    })
}

// True for the first uncommented line that initialises zsh completion.
fn is_compinit_anchor(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.starts_with('#') && (trimmed.contains("compinit") || trimmed.contains("oh-my-zsh.sh"))
}

// Builds the report's closing instruction.
fn next_step(reload_hint: &str) -> String {
    format!("run `{reload_hint}` to reload completions in this shell")
}

// `<path>.harw-bak`
fn backup_path(path: &Path) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(BACKUP_SUFFIX);
    PathBuf::from(name)
}

// Reads a file, mapping "not found" to `None`.
fn read_optional(path: &Path) -> CompletionResult<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io_err("read", path)(err)),
    }
}

// Step 2: remove harw-managed files among `paths`, report everything else.
fn sweep_paths(
    paths: &[PathBuf],
    bin: &str,
    dry_run: bool,
    report: &mut InstallReport,
) -> CompletionResult<()> {
    for path in paths {
        match fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                debug!(path = %path.display(), "legacy path absent");
                continue;
            }
            Err(err) => {
                report.skip(path, format!("cannot inspect: {err}"));
                continue;
            }
        }
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                report.skip(path, format!("cannot read: {err}"));
                continue;
            }
        };
        if !is_ours(&String::from_utf8_lossy(&bytes), bin) {
            report.skip(path, FOREIGN_REASON);
            continue;
        }
        if remove_owned(path, dry_run, report)? {
            debug!(path = %path.display(), dry_run, "removed harw-managed completion file");
            report.removed.push(path.to_path_buf());
        }
    }
    Ok(())
}

// Removes a file we own. Returns true when removed (or would be, in dry-run mode).
// Permission denied -> skipped + `sudo rm` hint (never escalates); vanished -> false.
fn remove_owned(path: &Path, dry_run: bool, report: &mut InstallReport) -> CompletionResult<bool> {
    if dry_run {
        return Ok(true);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            debug!(path = %path.display(), "file vanished before removal");
            Ok(false)
        }
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            warn!(path = %path.display(), "permission denied while removing completion file");
            report.skip(path, "permission denied");
            report.hints.push(format!("sudo rm {}", path.display()));
            Ok(false)
        }
        Err(err) => Err(io_err("remove", path)(err)),
    }
}

// Step 4: drop old blocks / legacy lines from every rc file and (install) insert the managed block.
fn update_rc_files(
    files: &[PathBuf],
    bin: &str,
    block: Option<&RcBlock>,
    dry_run: bool,
    report: &mut InstallReport,
) -> CompletionResult<()> {
    for file in files {
        let wanted = block
            .filter(|candidate| candidate.rc_file == *file)
            .map(|candidate| (candidate.body.as_slice(), candidate.placement));

        let (old, existed) = match fs::read(file) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => (text, true),
                Err(_) => {
                    report.skip(file, "rc file is not valid UTF-8; left untouched");
                    continue;
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if wanted.is_none() {
                    debug!(path = %file.display(), "rc file absent");
                    continue;
                }
                (String::new(), false)
            }
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                report.skip(file, "permission denied while reading rc file");
                continue;
            }
            Err(err) => return Err(io_err("read", file)(err)),
        };

        let rewrite = rewrite_rc(&old, bin, wanted);
        if rewrite.unterminated_block {
            warn!(path = %file.display(), "unterminated managed completion block");
            report.hints.push(format!(
                "unterminated managed block `{}` in {}; fix manually",
                block_begin(bin),
                file.display()
            ));
        }
        if rewrite.content == old {
            debug!(path = %file.display(), "rc file already up to date");
            continue;
        }

        if existed {
            let backup = backup_path(file);
            if !dry_run {
                fs::copy(file, &backup).map_err(io_err("back up", file))?;
            }
            debug!(path = %file.display(), backup = %backup.display(), dry_run, "rc backup");
            report.backups.push(backup);
        }
        if !dry_run {
            write_atomic(file, rewrite.content.as_bytes())?;
        }
        debug!(path = %file.display(), dry_run, "rc file updated");
        report.rc_updated.push(file.to_path_buf());
    }
    Ok(())
}

// Step 5: remove `.zcompdump*` caches that mention `_<bin>`.
fn purge_caches(
    dirs: &[PathBuf],
    bin: &str,
    dry_run: bool,
    report: &mut InstallReport,
) -> CompletionResult<()> {
    let needle = format!("_{bin}");
    for (index, dir) in dirs.iter().enumerate() {
        if dirs.get(..index).unwrap_or_default().contains(dir) {
            continue;
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                warn!(dir = %dir.display(), error = %err, "cannot scan completion cache directory");
                continue;
            }
        };
        let mut candidates: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(ZCOMPDUMP_PREFIX)
            })
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        candidates.sort();

        for path in candidates {
            if report.caches_removed.contains(&path) {
                continue;
            }
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    debug!(path = %path.display(), error = %err, "cannot read completion cache");
                    continue;
                }
            };
            if !contains_subslice(&bytes, needle.as_bytes()) {
                debug!(path = %path.display(), "completion cache does not reference binary");
                continue;
            }
            if remove_owned(&path, dry_run, report)? {
                debug!(path = %path.display(), dry_run, "removed completion cache");
                report.caches_removed.push(path);
            }
        }
    }
    Ok(())
}

// Byte-substring search.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}

// Resolves a symlinked target so the link itself (e.g. a dotfile-manager rc link) survives the rename.
fn resolve_symlink(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

// Writes `bytes` to `path` via `.{name}.harw-tmp-{pid}` + rename, preserving existing permissions.
fn write_atomic(path: &Path, bytes: &[u8]) -> CompletionResult<()> {
    let target = resolve_symlink(path);
    let parent = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let Some(file_name) = target.file_name() else {
        return Err(CompletionError::Io {
            op: "write",
            path: target.display().to_string(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"),
        });
    };
    fs::create_dir_all(parent).map_err(io_err("create directory", parent))?;

    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".harw-tmp-{}", std::process::id()));
    let temp = parent.join(temp_name);

    let result = write_and_swap(&temp, &target, bytes);
    if result.is_err() {
        match fs::remove_file(&temp) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                warn!(path = %temp.display(), error = %err, "cannot remove temporary file");
            }
        }
    }
    result
}

// Temp write, permission copy and rename for `write_atomic`.
fn write_and_swap(temp: &Path, target: &Path, bytes: &[u8]) -> CompletionResult<()> {
    fs::write(temp, bytes).map_err(io_err("write", temp))?;
    match fs::metadata(target) {
        Ok(meta) => {
            fs::set_permissions(temp, meta.permissions()).map_err(io_err("set permissions on", temp))?;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(io_err("inspect", target)(err)),
    }
    fs::rename(temp, target).map_err(io_err("rename", target))
}

// End-of-run summary with counts.
fn log_summary(report: &InstallReport) {
    info!(
        uninstall = report.uninstall,
        dry_run = report.dry_run,
        written = report.written.is_some(),
        removed = report.removed.len(),
        skipped = report.skipped.len(),
        rc_updated = report.rc_updated.len(),
        backups = report.backups.len(),
        caches_removed = report.caches_removed.len(),
        hints = report.hints.len(),
        "shell completions finished"
    );
}
