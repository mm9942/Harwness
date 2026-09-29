//! `gateway.logs` — a bounded, deduplicated, redacted tail of the harw log
//! files (coordinator addendum to R18 F1).
//!
//! Field evidence: a 9.6 MB `tui.log` full of one repeated warning
//! (`unresolved catalog reference — affected entry disabled … provider
//! 'zhipu'`) that a UIA could only read through `tail -c 4000` in the shell.
//! This operation reads the end of each log file under `<home>/logs`, keeps
//! the lines matching an optional level and substring filter, folds repeated
//! lines into one entry with a count (`×312 …`) and returns at most `lines`
//! entries per file.
//!
//! # Bounds
//! - At most [`TAIL_WINDOW_BYTES`] from the end of each file are read.
//! - `lines` defaults to [`DEFAULT_LOG_LINES`] and must not exceed
//!   [`MAX_LOG_LINES`] (a larger value is refused, never silently clamped).
//! - At most [`MAX_LOG_FILES`] files (newest first) without a `file`
//!   argument.
//! - Each shown line is bounded to [`MAX_LINE_CHARS`] characters.
//!
//! # Secrets
//! Every shown line goes through [`super::sanitize`]: control characters and
//! ANSI colour codes are removed and token-shaped substrings are replaced by
//! `[redacted]` (`harw_memory::redact`).
//!
//! # Files
//! Only regular files directly in `<home>/logs` whose name contains `.log`
//! are read; a `file` argument must be a plain file name (no path, no
//! leading dot) and symlinks are refused.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use harw_home::ResolvedHomeContext;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use serde::Deserialize;
use serde_json::{Value, json};

use super::sanitize;

/// Entries per file without a `lines` argument.
pub const DEFAULT_LOG_LINES: u64 = 50;

/// Largest accepted `lines` argument.
pub const MAX_LOG_LINES: u64 = 500;

/// Bytes read from the end of each log file.
pub const TAIL_WINDOW_BYTES: u64 = 1024 * 1024;

/// Longest shown log line.
pub const MAX_LINE_CHARS: usize = 400;

/// Most files read without a `file` argument.
pub const MAX_LOG_FILES: usize = 8;

/// Longest accepted `file` / `contains` argument.
const MAX_ARG_CHARS: usize = 128;

/// Arguments of `gateway.logs`.
///
/// Command form: `/gateway-logs [file] [lines] [level] [text…]`, e.g.
/// `/gateway-logs tui.log 100 warn zhipu`.
#[derive(Debug, Default, Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct GatewayLogsArgs {
    /// Log file name in `<home>/logs` (e.g. `tui.log`). Default: every log
    /// file, newest first.
    #[serde(default)]
    pub file: Option<String>,
    /// Distinct entries per file (default 50, max 500).
    #[serde(default)]
    pub lines: Option<u64>,
    /// Minimum level: `error`, `warn`, `info`, `debug` or `trace`.
    #[serde(default)]
    pub level: Option<String>,
    /// Only lines containing this text (case-insensitive).
    #[serde(default)]
    pub contains: Option<String>,
}

impl harw_operations::FromRawArgs for GatewayLogsArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        let mut args = Self::default();
        let mut text = Vec::new();
        for token in tokens {
            if args.lines.is_none() && token.chars().all(|c| c.is_ascii_digit()) {
                args.lines = token.parse().ok();
            } else if args.level.is_none() && Level::parse(token).is_ok() {
                args.level = Some(token.clone());
            } else if args.file.is_none() && token.contains(".log") {
                args.file = Some(token.clone());
            } else {
                text.push(token.as_str());
            }
        }
        if !text.is_empty() {
            args.contains = Some(text.join(" "));
        }
        Ok(args)
    }
}

/// Tail of the harw log files: filtered, repeated lines folded with a
/// count, secrets redacted.
///
/// # Errors
/// - [`OpError::NotAvailable`] without the bound harw home.
/// - [`OpError::InvalidArguments`] for `lines` outside `1..=500`, an unknown
///   level, an invalid or unknown `file`.
/// - [`OpError::Execution`] when a selected file cannot be read.
#[operation(
    name = "gateway.logs",
    summary = "Zeigt das Ende der harw-Logdateien (Standard 50, max. 500 Einträge je Datei), optional nach Level und Text gefiltert; wiederholte Zeilen werden mit Anzahl zusammengefasst (×312 …), Geheimnisse geschwärzt. Use this instead of ps/ls/tail through the shell. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "maintainer",
    command(path = "/gateway-logs", visibility = "tui_only", busy = "immediate"),
    model_tool(readonly, approval = "none"),
    // Liest nur das Ende der Logdateien, keine Mutation.
    web(path = "/api/gateway/logs", method = "get", approval = "none")
)]
async fn gateway_logs(ctx: &OpContext, args: GatewayLogsArgs) -> Result<OpOutput, OpError> {
    let limit = match args.lines {
        None => DEFAULT_LOG_LINES,
        Some(lines) if (1..=MAX_LOG_LINES).contains(&lines) => lines,
        Some(_) => {
            return Err(OpError::InvalidArguments(format!(
                "lines must be between 1 and {MAX_LOG_LINES}"
            )));
        }
    };
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let level = args
        .level
        .as_deref()
        .map(str::trim)
        .filter(|level| !level.is_empty())
        .map(Level::parse)
        .transpose()?;
    let contains = args
        .contains
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| {
            if text.chars().count() > MAX_ARG_CHARS {
                Err(OpError::InvalidArguments(format!(
                    "contains must be at most {MAX_ARG_CHARS} characters"
                )))
            } else {
                Ok(text.to_lowercase())
            }
        })
        .transpose()?;
    let home = ctx
        .service::<Arc<ResolvedHomeContext>>()
        .ok_or_else(|| {
            OpError::NotAvailable("the harw home is not bound in this context".to_owned())
        })?
        .home
        .clone();
    let dir = harw_home::paths::logs_dir(&home);
    let dir_text = sanitize(&dir.display().to_string(), 256);

    let files = match args
        .file
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
    {
        Some(name) => vec![named_log_file(&dir, name)?],
        None => {
            let mut files = list_log_files(&dir);
            files.truncate(MAX_LOG_FILES);
            files
        }
    };
    if files.is_empty() {
        return Ok(OpOutput {
            text: format!("No harw log files in {dir_text}."),
            data: Some(json!({ "dir": dir_text, "files": [] })),
        });
    }

    let filter = LineFilter { level, contains };
    let mut sections = Vec::with_capacity(files.len());
    let mut entries = Vec::with_capacity(files.len());
    for file in &files {
        let tail = read_tail(&file.path, TAIL_WINDOW_BYTES).map_err(|error| {
            OpError::Execution(format!(
                "cannot read {}: {}",
                sanitize(&file.name, MAX_ARG_CHARS),
                error.kind()
            ))
        })?;
        let digest = digest_lines(tail.text.lines(), &filter, limit);
        let (section, data) = render_file(file, &tail, &digest);
        sections.push(section);
        entries.push(data);
    }
    Ok(OpOutput {
        text: sections.join("\n"),
        data: Some(json!({ "dir": dir_text, "files": entries })),
    })
}

// ── Files ────────────────────────────────────────────────────────────────────

/// One log file in `<home>/logs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogFile {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
    pub(crate) modified: Option<SystemTime>,
}

fn is_log_name(name: &str) -> bool {
    !name.starts_with('.')
        && name.contains(".log")
        && name.len() <= MAX_ARG_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Regular log files directly in `dir` (symlinks skipped), newest first.
/// A missing or unreadable directory yields an empty list.
pub(crate) fn list_log_files(dir: &Path) -> Vec<LogFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<LogFile> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            if !is_log_name(&name) {
                return None;
            }
            let meta = std::fs::symlink_metadata(entry.path()).ok()?;
            meta.file_type().is_file().then(|| LogFile {
                name,
                path: entry.path(),
                size: meta.len(),
                modified: meta.modified().ok(),
            })
        })
        .collect();
    files.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.name.cmp(&b.name))
    });
    files
}

fn named_log_file(dir: &Path, name: &str) -> Result<LogFile, OpError> {
    if !is_log_name(name) {
        return Err(OpError::InvalidArguments(
            "file must be a plain log file name such as `tui.log` (letters, digits, `.`, `_`, \
             `-`; no path)"
                .to_owned(),
        ));
    }
    let path = dir.join(name);
    let meta = std::fs::symlink_metadata(&path).map_err(|_| {
        OpError::InvalidArguments(format!(
            "no log file `{name}` in the harw log directory; call gateway.logs without `file` \
             to list them"
        ))
    })?;
    if !meta.file_type().is_file() {
        return Err(OpError::InvalidArguments(format!(
            "`{name}` is not a regular file (symlinks are not followed)"
        )));
    }
    Ok(LogFile {
        name: name.to_owned(),
        path,
        size: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// The end of one file.
pub(crate) struct Tail {
    pub(crate) text: String,
    pub(crate) size: u64,
    pub(crate) scanned: u64,
}

/// Reads at most `window` bytes from the end of `path`. A partial first line
/// (the window started mid-line) is dropped; invalid UTF-8 is replaced.
pub(crate) fn read_tail(path: &Path, window: u64) -> std::io::Result<Tail> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let start = size.saturating_sub(window);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(window).read_to_end(&mut bytes)?;
    let scanned = u64::try_from(bytes.len()).unwrap_or(window);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        text = match text.find('\n') {
            Some(newline) => text.split_off(newline + 1),
            None => String::new(),
        };
    }
    Ok(Tail {
        text,
        size,
        scanned,
    })
}

// ── Filtering and folding ────────────────────────────────────────────────────

/// Log level, most severe first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    fn parse(value: &str) -> Result<Self, OpError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" | "err" => Ok(Self::Error),
            "warn" | "warning" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(OpError::InvalidArguments(
                "level must be one of: error, warn, info, debug, trace".to_owned(),
            )),
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "ERROR" => Some(Self::Error),
            "WARN" | "WARNING" => Some(Self::Warn),
            "INFO" => Some(Self::Info),
            "DEBUG" => Some(Self::Debug),
            "TRACE" => Some(Self::Trace),
            _ => None,
        }
    }
}

/// Which lines to keep.
pub(crate) struct LineFilter {
    /// Keep lines at this level or more severe; lines without a level are
    /// dropped while a level is set.
    pub(crate) level: Option<Level>,
    /// Lower-cased substring.
    pub(crate) contains: Option<String>,
}

/// One folded entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) count: usize,
    pub(crate) text: String,
}

/// Result of folding one file's tail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Digest {
    /// Matching lines in the scanned window.
    pub(crate) matched: usize,
    /// Distinct entries among them.
    pub(crate) distinct: usize,
    /// The last `limit` distinct entries, oldest first.
    pub(crate) entries: Vec<Entry>,
}

/// Filters, normalizes and folds `lines`. Two lines are the same entry when
/// they are equal after removing ANSI codes, the leading timestamp and
/// repeated whitespace (JSON lines: without their time fields). Entries are
/// ordered by their last occurrence; the newest `limit` are kept.
pub(crate) fn digest_lines<'a>(
    lines: impl Iterator<Item = &'a str>,
    filter: &LineFilter,
    limit: usize,
) -> Digest {
    let mut groups: Vec<(String, usize, usize)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut matched = 0usize;
    for (position, raw) in lines.enumerate() {
        let normalized = normalize(raw);
        if normalized.is_empty() {
            continue;
        }
        if let Some(wanted) = filter.level {
            match detect_level(&normalized) {
                Some(level) if level <= wanted => {}
                _ => continue,
            }
        }
        if let Some(text) = &filter.contains {
            if !normalized.to_lowercase().contains(text.as_str()) {
                continue;
            }
        }
        matched += 1;
        match index.get(&normalized) {
            Some(&slot) => {
                if let Some(group) = groups.get_mut(slot) {
                    group.1 += 1;
                    group.2 = position;
                }
            }
            None => {
                index.insert(normalized.clone(), groups.len());
                groups.push((normalized, 1, position));
            }
        }
    }
    let distinct = groups.len();
    groups.sort_by_key(|group| group.2);
    let skip = groups.len().saturating_sub(limit);
    let entries = groups
        .into_iter()
        .skip(skip)
        .map(|(text, count, _)| Entry {
            count,
            text: sanitize(&text, MAX_LINE_CHARS),
        })
        .collect();
    Digest {
        matched,
        distinct,
        entries,
    }
}

/// Removes ANSI escape sequences (`ESC [ … letter`).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn looks_like_timestamp(token: &str) -> bool {
    token.starts_with(|c: char| c.is_ascii_digit())
        && token.contains([':', '-'])
        && token
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | '.' | 'T' | 'Z' | '+' | ','))
}

/// Line without colour codes, leading timestamp and repeated whitespace; a
/// JSON log line becomes `LEVEL target: message key=value…` without its time
/// fields.
pub(crate) fn normalize(raw: &str) -> String {
    let line = strip_ansi(raw);
    let trimmed = line.trim();
    if trimmed.starts_with('{') {
        if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(trimmed) {
            return normalize_json(&object);
        }
    }
    let mut tokens = trimmed.split_whitespace().peekable();
    if tokens
        .peek()
        .is_some_and(|token| looks_like_timestamp(token))
    {
        tokens.next();
    }
    tokens.collect::<Vec<_>>().join(" ")
}

fn normalize_json(object: &serde_json::Map<String, Value>) -> String {
    let text = |value: Option<&Value>| match value {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    let level = text(object.get("level"));
    let target = text(object.get("target"));
    let fields = object.get("fields").and_then(Value::as_object);
    let message = text(
        fields
            .and_then(|fields| fields.get("message"))
            .or_else(|| object.get("message"))
            .or_else(|| object.get("msg")),
    );
    let mut out = format!("{level} {target}: {message}");
    if let Some(fields) = fields {
        for (key, value) in fields {
            if key != "message" {
                out.push_str(&format!(" {key}={}", text(Some(value))));
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn detect_level(normalized: &str) -> Option<Level> {
    normalized
        .split_whitespace()
        .take(6)
        .find_map(|token| Level::from_token(token.trim_matches(|c: char| !c.is_ascii_alphabetic())))
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// `9.6 MiB`, `12.0 KiB`, `300 B`.
#[must_use]
pub(crate) fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    // Display only: the precision loss of the float conversion is irrelevant.
    #[allow(clippy::cast_precision_loss)]
    let value = bytes as f64;
    if bytes >= MIB {
        format!("{:.1} MiB", value / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", value / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn render_file(file: &LogFile, tail: &Tail, digest: &Digest) -> (String, Value) {
    let name = sanitize(&file.name, MAX_ARG_CHARS);
    let mut text = format!(
        "== {name} ({}; scanned last {}; {} matching lines, {} distinct{}) ==",
        human_size(tail.size),
        human_size(tail.scanned),
        digest.matched,
        digest.distinct,
        if digest.entries.len() < digest.distinct {
            format!(", newest {} shown", digest.entries.len())
        } else {
            String::new()
        }
    );
    if digest.entries.is_empty() {
        text.push_str("\n(no matching lines)");
    }
    for entry in &digest.entries {
        text.push('\n');
        if entry.count > 1 {
            text.push_str(&format!("×{} ", entry.count));
        }
        text.push_str(&entry.text);
    }
    let data = json!({
        "file": name,
        "size_bytes": tail.size,
        "scanned_bytes": tail.scanned,
        "matched_lines": digest.matched,
        "distinct": digest.distinct,
        "entries": digest
            .entries
            .iter()
            .map(|entry| json!({ "count": entry.count, "text": entry.text }))
            .collect::<Vec<_>>(),
    });
    (text, data)
}

#[cfg(test)]
mod tests {
    use super::{
        GatewayLogsArgs, Level, LineFilter, digest_lines, human_size, list_log_files, normalize,
        read_tail,
    };
    use crate::test_support::{TestResult, ctx};
    use crate::testutil::toks;
    use harw_operations::FromRawArgs;

    fn no_filter() -> LineFilter {
        LineFilter {
            level: None,
            contains: None,
        }
    }

    #[test]
    fn repeated_lines_fold_with_counts_despite_timestamps_and_colours() {
        let log = "2026-09-28T10:00:00.000001Z  WARN harw_model_catalog: unresolved catalog reference — affected entry disabled provider=\"zhipu\"\n\
                   2026-09-28T10:00:01.000002Z  INFO harw_cli: started\n\
                   \u{1b}[2m2026-09-28T10:00:02.000003Z\u{1b}[0m  WARN harw_model_catalog: unresolved catalog reference — affected entry disabled provider=\"zhipu\"\n";
        let digest = digest_lines(log.lines(), &no_filter(), 50);
        assert_eq!(digest.matched, 3);
        assert_eq!(digest.distinct, 2);
        // Ordered by last occurrence: `started` first, the repeated warning last.
        assert_eq!(digest.entries.len(), 2);
        assert_eq!(digest.entries[0].count, 1);
        assert_eq!(digest.entries[1].count, 2);
        assert!(digest.entries[1].text.contains("zhipu"));
        assert!(!digest.entries[1].text.contains("2026-09-28"));
    }

    #[test]
    fn level_and_substring_filters_apply() {
        let log = "t=1 ERROR a: boom\nINFO b: fine\n WARN c: Zhipu missing\nplain continuation\n";
        let warn = digest_lines(
            log.lines(),
            &LineFilter {
                level: Some(Level::Warn),
                contains: None,
            },
            50,
        );
        assert_eq!(warn.matched, 2, "{warn:?}");
        let text = digest_lines(
            log.lines(),
            &LineFilter {
                level: None,
                contains: Some("zhipu".to_owned()),
            },
            50,
        );
        assert_eq!(text.matched, 1, "{text:?}");
    }

    #[test]
    fn limit_keeps_the_newest_entries() {
        let log: Vec<String> = (0..20).map(|n| format!("INFO line {n}")).collect();
        let digest = digest_lines(log.iter().map(String::as_str), &no_filter(), 5);
        assert_eq!(digest.distinct, 20);
        assert_eq!(digest.entries.len(), 5);
        assert_eq!(digest.entries[4].text, "INFO line 19");
    }

    #[test]
    fn secrets_in_log_lines_are_redacted() {
        let log = "WARN auth: Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123\n\
                   WARN cfg: api_key=sk-live-abcdefghijklmnopqrstuvwxyz\n";
        let digest = digest_lines(log.lines(), &no_filter(), 50);
        for entry in &digest.entries {
            assert!(
                !entry.text.contains("abcdefghijklmnopqrstuvwxyz"),
                "{entry:?}"
            );
        }
    }

    #[test]
    fn json_lines_normalize_without_time_fields() {
        let a = normalize(
            r#"{"timestamp":"2026-09-28T10:00:00Z","level":"WARN","target":"cat","fields":{"message":"unresolved","provider":"zhipu"}}"#,
        );
        let b = normalize(
            r#"{"timestamp":"2026-09-28T11:00:00Z","level":"WARN","target":"cat","fields":{"message":"unresolved","provider":"zhipu"}}"#,
        );
        assert_eq!(a, b);
        assert!(a.starts_with("WARN cat: unresolved"), "{a}");
    }

    #[test]
    fn tail_reads_only_the_window_and_drops_the_partial_first_line() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("tui.log");
        std::fs::write(&path, "first line\nsecond line\nthird\n").map_err(ctx("write log"))?;
        let tail = read_tail(&path, 10).map_err(ctx("read_tail"))?;
        assert_eq!(tail.size, 29);
        assert_eq!(tail.scanned, 10);
        assert_eq!(tail.text, "third\n");
        Ok(())
    }

    #[test]
    fn only_regular_log_files_are_listed() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(dir.path().join("tui.log"), "x").map_err(ctx("write tui.log"))?;
        std::fs::write(dir.path().join("notes.txt"), "x").map_err(ctx("write notes"))?;
        std::fs::create_dir(dir.path().join("dir.log")).map_err(ctx("mkdir"))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("evil.log"))
            .map_err(ctx("symlink"))?;
        let names: Vec<String> = list_log_files(dir.path())
            .into_iter()
            .map(|file| file.name)
            .collect();
        assert_eq!(names, ["tui.log"]);
        Ok(())
    }

    #[test]
    fn command_tokens_parse_into_file_lines_level_and_text() -> TestResult {
        let args = GatewayLogsArgs::from_raw_args(&toks(&["tui.log", "100", "warn", "zhipu", "x"]))
            .map_err(ctx("from_raw_args"))?;
        assert_eq!(args.file.as_deref(), Some("tui.log"));
        assert_eq!(args.lines, Some(100));
        assert_eq!(args.level.as_deref(), Some("warn"));
        assert_eq!(args.contains.as_deref(), Some("zhipu x"));
        Ok(())
    }

    #[test]
    fn human_sizes_are_compact() {
        assert_eq!(human_size(300), "300 B");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(10_066_329), "9.6 MiB");
    }
}
