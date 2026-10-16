//! `sys.pgrep` — `pgrep` in reinem Rust.
//!
//! Sucht Prozesse per regulärem Ausdruck gegen den Namen (`comm`) oder mit
//! `full` (`-f`) gegen die **maskierte** Kommandozeile. Der eigene Prozess wird
//! wie bei `pgrep` ausgeschlossen. Der Ausdruck ist auf 256 Zeichen und
//! 1 MiB kompiliertes Programm begrenzt (kein Regex-Bombing).

use crate::mask::masked_command;
use crate::procfs::{ProcFs, ProcInfo};
use harw_tool_fsread::blocking::run_blocking;
use harw_tool_fsread::budget::{Collector, fail, flag, limit_or, ok};
use harw_tool_fsread::users::UserDb;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;

/// Name des Werkzeugs.
pub const TOOL: &str = "sys.pgrep";

/// Standard-Limit.
pub const DEFAULT_LIMIT: usize = 100;

/// Hartes Limit.
pub const HARD_LIMIT: usize = 1_000;

/// Höchstlänge des Ausdrucks.
pub const MAX_PATTERN_CHARS: usize = 256;

/// Argumente für `sys.pgrep`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct PgrepArgs {
    /// Regular expression (Rust regex syntax, at most 256 characters) matched against the process name.
    pub pattern: String,
    /// -f / --full: match against the full (masked) command line instead of the name.
    #[serde(default)]
    pub full: Option<bool>,
    /// -x / --exact: the pattern must match the whole name / command line.
    #[serde(default)]
    pub exact: Option<bool>,
    /// -i / --ignore-case: case-insensitive matching.
    #[serde(default)]
    pub ignore_case: Option<bool>,
    /// -u / --euid: only processes of this user (name or numeric uid).
    #[serde(default)]
    pub user: Option<String>,
    /// -P / --parent: only children of these parent PIDs (at most 64).
    #[serde(default)]
    pub ppids: Option<Vec<i64>>,
    /// -n / --newest: only the most recently started match.
    #[serde(default)]
    pub newest: Option<bool>,
    /// -o / --oldest: only the longest running match.
    #[serde(default)]
    pub oldest: Option<bool>,
    /// Maximum number of matches (default 100, hard 1000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
}

/// Führt `sys.pgrep` aus; `own_pid` wird ausgeschlossen.
#[must_use]
pub fn run(procfs: &ProcFs, own_pid: i32, args: &PgrepArgs) -> ToolOutput {
    match build(procfs, own_pid, args) {
        Ok(output) => output,
        Err(message) => fail(TOOL, message),
    }
}

fn build(procfs: &ProcFs, own_pid: i32, args: &PgrepArgs) -> Result<ToolOutput, String> {
    if args.pattern.is_empty()
        || args.pattern.chars().count() > MAX_PATTERN_CHARS
        || args.pattern.contains('\0')
    {
        return Err(format!(
            "pattern must be 1-{MAX_PATTERN_CHARS} characters without NUL"
        ));
    }
    if flag(args.newest) && flag(args.oldest) {
        return Err("newest and oldest are mutually exclusive".to_owned());
    }
    let full = flag(args.full);
    let source = if flag(args.exact) {
        format!("^(?:{})$", args.pattern)
    } else {
        args.pattern.clone()
    };
    let regex = RegexBuilder::new(&source)
        .case_insensitive(flag(args.ignore_case))
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
        .map_err(|error| {
            format!("invalid pattern: {error}")
                .chars()
                .take(300)
                .collect::<String>()
        })?;
    let mut parents: HashSet<i32> = HashSet::new();
    if let Some(ppids) = &args.ppids {
        if ppids.len() > 64 {
            return Err("too many ppids (max 64)".to_owned());
        }
        for ppid in ppids {
            parents.insert(
                i32::try_from(*ppid)
                    .ok()
                    .filter(|p| *p >= 0)
                    .ok_or_else(|| format!("invalid ppid {ppid}"))?,
            );
        }
    }
    let db = UserDb::load();
    let want_uid = match args.user.as_deref() {
        None => None,
        Some(user) => match user.parse::<u32>() {
            Ok(uid) => Some(uid),
            Err(_) => Some(db.uid_of(user).ok_or_else(|| {
                format!(
                    "unknown user '{}'",
                    user.chars().take(32).collect::<String>()
                )
            })?),
        },
    };
    let limit = limit_or(args.limit, DEFAULT_LIMIT, HARD_LIMIT);

    let (all, vanished) = procfs.processes(full);
    let mut matches: Vec<ProcInfo> = all
        .into_iter()
        .filter(|info| info.stat.pid != own_pid)
        .filter(|info| parents.is_empty() || parents.contains(&info.stat.ppid))
        .filter(|info| want_uid.is_none_or(|uid| info.uid == uid))
        .filter(|info| {
            if full {
                let text = if info.cmdline.is_empty() {
                    info.stat.comm.clone()
                } else {
                    masked_command(&info.cmdline, 4096)
                };
                regex.is_match(&text)
            } else {
                regex.is_match(&info.stat.comm)
            }
        })
        .collect();
    matches.sort_by_key(|info| (info.stat.starttime, info.stat.pid));
    let total = matches.len();
    if flag(args.newest) {
        matches = matches.pop().into_iter().collect();
    } else if flag(args.oldest) {
        matches.truncate(1);
    }
    matches.sort_by_key(|info| info.stat.pid);

    let mut out = Collector::new(limit);
    let mut pids = Vec::new();
    for info in &matches {
        let mut entry =
            json!({"pid": info.stat.pid, "name": info.stat.comm, "ppid": info.stat.ppid});
        if full {
            entry["command"] = json!(masked_command(&info.cmdline, 256));
        }
        if !out.push(entry) {
            break;
        }
        pids.push(info.stat.pid);
    }
    let truncated = out.truncated();
    let stopped = out.stop_reason();
    let count = out.len();
    Ok(ok(
        TOOL,
        format!(
            "{count} matching processes{}",
            if truncated { " (truncated)" } else { "" }
        ),
        json!({
            "pids": pids,
            "matches": out.into_items(),
            "count": count,
            "matched": total,
            "vanished": vanished,
            "truncated": truncated,
            "stopped": stopped,
        }),
    ))
}

/// Sucht Prozesse wie `pgrep`.
#[harw_macros::tool(
    name = "sys.pgrep",
    description = "Finds process IDs by regular expression like pgrep: against the name or, with full=true (-f), the masked command line; -x exact, -i ignore_case, -u user, -P ppids, -n newest, -o oldest. Use when you need the PIDs of a program instead of running pgrep. Returns JSON {pids:[...], matches:[{pid,name,ppid}], count, truncated}; the calling process is excluded, patterns are length- and size-limited. Read-only, Linux only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_pgrep(
    _context: &ToolExecutionContext,
    args: PgrepArgs,
) -> Result<ToolOutput, ToolsError> {
    let own = i32::try_from(std::process::id()).unwrap_or(0);
    run_blocking(TOOL, move || run(&ProcFs::real(), own, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, error_of, json_of};
    use serde_json::Value;
    use std::fs;
    use std::path::Path;

    fn add(
        root: &Path,
        pid: i32,
        ppid: i32,
        comm: &str,
        uid: u32,
        start: u64,
        cmd: &[&str],
    ) -> TestResult {
        let dir = root.join(pid.to_string());
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("stat"),
            format!("{pid} ({comm}) S {ppid} 1 1 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 {start} 4096 1 0"),
        )?;
        fs::write(
            dir.join("status"),
            format!("Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )?;
        fs::write(
            dir.join("cmdline"),
            cmd.iter().map(|c| format!("{c}\0")).collect::<String>(),
        )?;
        Ok(())
    }

    fn tree() -> TestResult<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        add(dir.path(), 1, 0, "init", 0, 10, &["/sbin/init"])?;
        add(
            dir.path(),
            5,
            1,
            "nginx",
            0,
            20,
            &["nginx: master process", "-c", "/etc/nginx.conf"],
        )?;
        add(
            dir.path(),
            6,
            5,
            "nginx",
            33,
            30,
            &["nginx: worker process"],
        )?;
        add(
            dir.path(),
            7,
            5,
            "nginx",
            33,
            40,
            &["nginx: worker process", "--token=SECRET-VALUE-1"],
        )?;
        add(
            dir.path(),
            8,
            1,
            "Python3",
            1000,
            50,
            &["python3", "app.py"],
        )?;
        add(dir.path(), 99, 1, "me", 1000, 60, &["pgrep-self"])?;
        Ok(dir)
    }

    fn pgrep(root: &Path, args: Value) -> TestResult<Value> {
        json_of(run(&ProcFs::at(root), 99, &serde_json::from_value(args)?))
    }

    fn pids(value: &Value) -> Vec<i64> {
        value["pids"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default()
    }

    #[test]
    fn matches_names_and_excludes_itself() -> TestResult {
        let dir = tree()?;
        assert_eq!(
            pids(&pgrep(dir.path(), json!({"pattern": "nginx"}))?),
            vec![5, 6, 7]
        );
        assert_eq!(
            pids(&pgrep(dir.path(), json!({"pattern": "^ngi"}))?),
            vec![5, 6, 7]
        );
        assert_eq!(
            pids(&pgrep(dir.path(), json!({"pattern": "me"}))?),
            Vec::<i64>::new(),
            "own pid 99 is excluded"
        );
        assert_eq!(
            pids(&pgrep(dir.path(), json!({"pattern": "python"}))?),
            Vec::<i64>::new()
        );
        assert_eq!(
            pids(&pgrep(
                dir.path(),
                json!({"pattern": "python", "ignore_case": true})
            )?),
            vec![8]
        );
        assert_eq!(
            pids(&pgrep(
                dir.path(),
                json!({"pattern": "ngin", "exact": true})
            )?),
            Vec::<i64>::new()
        );
        assert_eq!(
            pids(&pgrep(
                dir.path(),
                json!({"pattern": "nginx", "exact": true})
            )?),
            vec![5, 6, 7]
        );
        Ok(())
    }

    #[test]
    fn full_command_line_filters_users_parents_newest_oldest() -> TestResult {
        let dir = tree()?;
        let p = dir.path();
        assert_eq!(
            pids(&pgrep(
                p,
                json!({"pattern": "worker process", "full": true})
            )?),
            vec![6, 7]
        );
        assert_eq!(
            pids(&pgrep(p, json!({"pattern": "nginx", "user": "33"}))?),
            vec![6, 7]
        );
        assert_eq!(
            pids(&pgrep(p, json!({"pattern": "nginx", "ppids": [1]}))?),
            vec![5]
        );
        assert_eq!(
            pids(&pgrep(p, json!({"pattern": "nginx", "newest": true}))?),
            vec![7]
        );
        assert_eq!(
            pids(&pgrep(p, json!({"pattern": "nginx", "oldest": true}))?),
            vec![5]
        );
        assert_eq!(
            pgrep(p, json!({"pattern": "nginx", "limit": 2}))?["truncated"],
            true
        );
        Ok(())
    }

    #[test]
    fn masked_command_cannot_be_probed() -> TestResult {
        let dir = tree()?;
        let p = dir.path();
        assert!(pids(&pgrep(p, json!({"pattern": "SECRET-VALUE", "full": true}))?).is_empty());
        assert_eq!(
            pids(&pgrep(
                p,
                json!({"pattern": "--token=\\*\\*\\*", "full": true})
            )?),
            vec![7]
        );
        let shown = pgrep(p, json!({"pattern": "worker", "full": true}))?;
        assert!(!serde_json::to_string(&shown)?.contains("SECRET-VALUE"));
        Ok(())
    }

    #[test]
    fn invalid_patterns_and_flags_are_rejected() -> TestResult {
        let dir = tree()?;
        let p = ProcFs::at(dir.path());
        for bad in [
            json!({"pattern": ""}),
            json!({"pattern": "("}),
            json!({"pattern": "a".repeat(MAX_PATTERN_CHARS + 1)}),
            json!({"pattern": "a\u{0}"}),
            json!({"pattern": "x", "newest": true, "oldest": true}),
            json!({"pattern": "x", "user": "nobody-such-user"}),
            json!({"pattern": "x", "ppids": [-3]}),
            json!({"pattern": "(a+)+$", "ppids": (0..65).collect::<Vec<i64>>()}),
        ] {
            let output = run(&p, 99, &serde_json::from_value(bad.clone())?);
            error_of(output).map_err(|e| TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        // Ein Ausdruck, dessen kompiliertes Programm über 1 MiB wüchse, wird abgelehnt
        // (die Voreinstellung der `regex`-Crate erlaubte 10 MiB).
        let big = run(
            &p,
            99,
            &serde_json::from_value(json!({"pattern": "\\pL{100}"}))?,
        );
        assert!(error_of(big)?.contains("invalid pattern"));
        // Katastrophales Backtracking gibt es in `regex` nicht: das läuft linear durch.
        let started = std::time::Instant::now();
        let _ = pgrep(dir.path(), json!({"pattern": "(a+)+$", "full": true}))?;
        assert!(started.elapsed().as_secs() < 5);
        assert!(serde_json::from_value::<PgrepArgs>(json!({"pattern": "x", "signal": 9})).is_err());
        Ok(())
    }
}
