//! `sys.ps` — `ps` in reinem Rust über `/proc`.
//!
//! Filter nach PID, PPID, Benutzer, Name, Zustand und (maskiertem) Befehl;
//! Sortierung nach `cpu`, `mem`, `start`, `pid`, `name`, `rss`; wählbare
//! Felder; optional als Baum (`--forest`).
//!
//! # Semantik der Zahlen
//! - `cpu_percent`: CPU-Zeit geteilt durch die Lebensdauer (wie `ps`), kann bei
//!   mehreren Threads über 100 liegen; für Momentanwerte `sys.top`.
//! - `mem_percent`: Resident Set geteilt durch `MemTotal`.
//! - `start`: ISO-8601 UTC, aus Boot-Zeit plus Start-Ticks.
//!
//! # Sicherheit
//! `command` ist über [`crate::mask`] maskiert; `command_contains` filtert den
//! **maskierten** Text (kein Orakel für Geheimnisse). Verschwindende Prozesse
//! zählen als `vanished`.

use crate::mask::masked_command;
use crate::procfs::{Clock, ProcFs, ProcInfo};
use harw_tool_fsread::blocking::run_blocking;
use harw_tool_fsread::budget::{Collector, choice, fail, flag, limit_or, ok};
use harw_tool_fsread::meta::iso_utc;
use harw_tool_fsread::users::UserDb;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashSet};

/// Name des Werkzeugs.
pub const TOOL: &str = "sys.ps";

/// Standard-Limit.
pub const DEFAULT_LIMIT: usize = 100;

/// Hartes Limit.
pub const HARD_LIMIT: usize = 2_000;

/// Zeichen der Kommandozeile in der Ausgabe.
pub const COMMAND_CHARS: usize = 512;

/// Alle wählbaren Felder.
pub const ALL_FIELDS: &[&str] = &[
    "pid",
    "ppid",
    "user",
    "uid",
    "state",
    "name",
    "command",
    "cpu_percent",
    "mem_percent",
    "rss_kib",
    "vsz_kib",
    "threads",
    "start",
    "elapsed_secs",
    "cpu_time_secs",
];

/// Standardfelder.
pub const DEFAULT_FIELDS: &[&str] = &[
    "pid",
    "ppid",
    "user",
    "state",
    "cpu_percent",
    "mem_percent",
    "rss_kib",
    "start",
    "name",
    "command",
];

/// Erlaubte Zustandszeichen.
const STATES: &str = "RSDZTtXxIWKPU";

/// Argumente für `sys.ps`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct PsArgs {
    /// Only these process IDs (positive integers, at most 256).
    #[serde(default)]
    pub pids: Option<Vec<i64>>,
    /// Only children of these parent process IDs (at most 256).
    #[serde(default)]
    pub ppids: Option<Vec<i64>>,
    /// Only processes of this user (name or numeric uid).
    #[serde(default)]
    pub user: Option<String>,
    /// Only processes whose command name (comm, 15 chars) equals this exactly.
    #[serde(default)]
    pub name: Option<String>,
    /// Only processes whose command name contains this text (case-insensitive).
    #[serde(default)]
    pub name_contains: Option<String>,
    /// Only processes whose masked command line contains this text (case-insensitive).
    #[serde(default)]
    pub command_contains: Option<String>,
    /// Only these states, one letter each: R running, S sleeping, D disk wait, Z zombie, T stopped, I idle, X dead.
    #[serde(default)]
    pub states: Option<Vec<String>>,
    /// Sort key: cpu, mem, rss, start, pid or name (default pid). cpu/mem/rss sort descending, start ascending (oldest first).
    #[serde(default)]
    pub sort: Option<String>,
    /// Reverse the sort order.
    #[serde(default)]
    pub reverse: Option<bool>,
    /// Fields to return, any of: pid, ppid, user, uid, state, name, command, cpu_percent, mem_percent, rss_kib, vsz_kib, threads, start, elapsed_secs, cpu_time_secs.
    #[serde(default)]
    pub fields: Option<Vec<String>>,
    /// Maximum number of processes returned (default 100, hard 2000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
    /// --forest: order as a process tree and add a depth field.
    #[serde(default)]
    pub tree: Option<bool>,
}

/// Eine Zeile mit berechneten Werten.
#[derive(Debug, Clone)]
pub struct Row {
    /// Rohdaten.
    pub info: ProcInfo,
    /// Benutzername oder numerische UID.
    pub user: String,
    /// CPU-Prozent (Lebensdauer).
    pub cpu_percent: f64,
    /// RAM-Prozent.
    pub mem_percent: f64,
    /// Resident Set in KiB.
    pub rss_kib: u64,
    /// Virtuelle Größe in KiB.
    pub vsz_kib: u64,
    /// Start (Unix-Sekunden), falls die Boot-Zeit bekannt ist.
    pub start_epoch: Option<i64>,
    /// Lebensdauer in Sekunden.
    pub elapsed_secs: f64,
    /// Verbrauchte CPU-Zeit in Sekunden.
    pub cpu_secs: f64,
    /// Maskierte, gekürzte Kommandozeile (bei Kernel-Threads `[name]`).
    pub command: String,
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Berechnet die Anzeigewerte eines Prozesses.
#[must_use]
pub fn make_row(info: ProcInfo, clock: &Clock, db: &UserDb) -> Row {
    let ticks = clock.ticks_per_sec as f64;
    let start_secs = info.stat.starttime as f64 / ticks;
    let elapsed = (clock.uptime_secs - start_secs).max(0.0);
    let cpu_secs = (info.stat.utime + info.stat.stime) as f64 / ticks;
    let cpu_percent = if elapsed > 0.0 {
        cpu_secs / elapsed * 100.0
    } else {
        0.0
    };
    let rss_kib = info.rss_kib(clock.page_size);
    let mem_percent = if clock.mem_total_kib > 0 {
        rss_kib as f64 / clock.mem_total_kib as f64 * 100.0
    } else {
        0.0
    };
    let command = if info.cmdline.is_empty() {
        format!("[{}]", info.stat.comm)
    } else {
        masked_command(&info.cmdline, COMMAND_CHARS)
    };
    Row {
        user: db.user_or_id(info.uid),
        cpu_percent: round1(cpu_percent),
        mem_percent: round1(mem_percent),
        rss_kib,
        vsz_kib: info.stat.vsize / 1024,
        start_epoch: clock.boot_time.map(|boot| boot + start_secs as i64),
        elapsed_secs: round1(elapsed),
        cpu_secs: round1(cpu_secs),
        command,
        info,
    }
}

fn parse_ids(field: &str, ids: Option<&Vec<i64>>) -> Result<Option<HashSet<i32>>, String> {
    let Some(ids) = ids else { return Ok(None) };
    if ids.len() > 256 {
        return Err(format!("too many {field} (max 256)"));
    }
    let mut set = HashSet::new();
    for id in ids {
        let id = i32::try_from(*id)
            .ok()
            .filter(|id| *id >= 0)
            .ok_or_else(|| {
                format!("invalid {field} entry {id}: must be a non-negative process ID")
            })?;
        set.insert(id);
    }
    Ok(Some(set))
}

fn select_fields(requested: Option<&Vec<String>>) -> Result<Vec<&'static str>, String> {
    let Some(requested) = requested else {
        return Ok(DEFAULT_FIELDS.to_vec());
    };
    if requested.is_empty() {
        return Err("fields must not be empty".to_owned());
    }
    let mut out: Vec<&'static str> = Vec::new();
    for field in requested {
        let known = ALL_FIELDS
            .iter()
            .find(|candidate| **candidate == field.as_str())
            .ok_or_else(|| {
                format!(
                    "unknown field '{}': expected any of {}",
                    field.chars().take(32).collect::<String>(),
                    ALL_FIELDS.join(", ")
                )
            })?;
        if !out.contains(known) {
            out.push(known);
        }
    }
    Ok(out)
}

/// Wählt die Felder einer Zeile.
#[must_use]
pub fn row_json(row: &Row, fields: &[&str]) -> Value {
    let mut map = Map::new();
    for field in fields {
        let value = match *field {
            "pid" => json!(row.info.stat.pid),
            "ppid" => json!(row.info.stat.ppid),
            "user" => json!(row.user),
            "uid" => json!(row.info.uid),
            "state" => json!(row.info.stat.state.to_string()),
            "name" => json!(row.info.stat.comm),
            "command" => json!(row.command),
            "cpu_percent" => json!(row.cpu_percent),
            "mem_percent" => json!(row.mem_percent),
            "rss_kib" => json!(row.rss_kib),
            "vsz_kib" => json!(row.vsz_kib),
            "threads" => json!(row.info.stat.threads),
            "start" => row
                .start_epoch
                .map_or(Value::Null, |epoch| json!(iso_utc(epoch))),
            "elapsed_secs" => json!(row.elapsed_secs),
            "cpu_time_secs" => json!(row.cpu_secs),
            _ => continue,
        };
        map.insert((*field).to_owned(), value);
    }
    Value::Object(map)
}

/// Führt `sys.ps` gegen `procfs` aus.
#[must_use]
pub fn run(procfs: &ProcFs, args: &PsArgs) -> ToolOutput {
    match build(procfs, args) {
        Ok(output) => output,
        Err(message) => fail(TOOL, message),
    }
}

fn build(procfs: &ProcFs, args: &PsArgs) -> Result<ToolOutput, String> {
    let pids = parse_ids("pids", args.pids.as_ref())?;
    let ppids = parse_ids("ppids", args.ppids.as_ref())?;
    let sort = choice(
        "sort",
        args.sort.as_deref(),
        &["pid", "cpu", "mem", "rss", "start", "name"],
        "pid",
    )?;
    let fields = select_fields(args.fields.as_ref())?;
    let limit = limit_or(args.limit, DEFAULT_LIMIT, HARD_LIMIT);
    let reverse = flag(args.reverse);
    let tree = flag(args.tree);
    let mut states: HashSet<char> = HashSet::new();
    for state in args.states.iter().flatten() {
        let mut chars = state.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if STATES.contains(c) => {
                states.insert(c);
            }
            _ => {
                return Err(format!(
                    "invalid state '{}': expected one letter of {STATES}",
                    state.chars().take(8).collect::<String>()
                ));
            }
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
    let name_contains = args.name_contains.as_deref().map(str::to_lowercase);
    let command_contains = args.command_contains.as_deref().map(str::to_lowercase);
    for text in [
        args.name.as_deref(),
        args.name_contains.as_deref(),
        args.command_contains.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if text.is_empty() || text.len() > 256 || text.contains('\0') {
            return Err("name filters must be 1-256 bytes without NUL".to_owned());
        }
    }

    let clock = procfs.clock();
    let need_cmdline = true;
    let (all, vanished) = procfs.processes(need_cmdline);
    let scanned = all.len();
    let mut rows: Vec<Row> = all
        .into_iter()
        .filter(|info| pids.as_ref().is_none_or(|set| set.contains(&info.stat.pid)))
        .filter(|info| {
            ppids
                .as_ref()
                .is_none_or(|set| set.contains(&info.stat.ppid))
        })
        .filter(|info| want_uid.is_none_or(|uid| info.uid == uid))
        .filter(|info| {
            args.name
                .as_deref()
                .is_none_or(|name| info.stat.comm == name)
        })
        .filter(|info| {
            name_contains
                .as_deref()
                .is_none_or(|needle| info.stat.comm.to_lowercase().contains(needle))
        })
        .filter(|info| states.is_empty() || states.contains(&info.stat.state))
        .map(|info| make_row(info, &clock, &db))
        .filter(|row| {
            command_contains
                .as_deref()
                .is_none_or(|needle| row.command.to_lowercase().contains(needle))
        })
        .collect();
    let matched = rows.len();

    rows.sort_by(|a, b| {
        use std::cmp::Ordering;
        let float = |x: f64, y: f64| y.partial_cmp(&x).unwrap_or(Ordering::Equal);
        let primary = match sort {
            "cpu" => float(a.cpu_percent, b.cpu_percent),
            "mem" => float(a.mem_percent, b.mem_percent),
            "rss" => b.rss_kib.cmp(&a.rss_kib),
            "start" => a.info.stat.starttime.cmp(&b.info.stat.starttime),
            "name" => a.info.stat.comm.cmp(&b.info.stat.comm),
            _ => Ordering::Equal,
        };
        let ordering = primary.then(a.info.stat.pid.cmp(&b.info.stat.pid));
        if reverse {
            ordering.reverse()
        } else {
            ordering
        }
    });

    let ordered: Vec<(usize, Row)> = if tree {
        forest(rows)
    } else {
        rows.into_iter().map(|row| (0, row)).collect()
    };

    let mut out = Collector::new(limit);
    for (depth, row) in &ordered {
        let mut value = row_json(row, &fields);
        if tree {
            if let Some(map) = value.as_object_mut() {
                map.insert("depth".to_owned(), json!(depth));
            }
        }
        if !out.push(value) {
            break;
        }
    }
    let truncated = out.truncated();
    let stopped = out.stop_reason();
    let count = out.len();
    Ok(ok(
        TOOL,
        format!(
            "{count} of {matched} matching processes ({scanned} scanned){}",
            if truncated { ", truncated" } else { "" }
        ),
        json!({
            "processes": out.into_items(),
            "count": count,
            "matched": matched,
            "scanned": scanned,
            "vanished": vanished,
            "sort": sort,
            "fields": fields,
            "truncated": truncated,
            "stopped": stopped,
        }),
    ))
}

/// Ordnet Zeilen als Prozessbaum (Vorordnung, Geschwister in Eingabereihenfolge).
fn forest(rows: Vec<Row>) -> Vec<(usize, Row)> {
    let present: HashSet<i32> = rows.iter().map(|r| r.info.stat.pid).collect();
    let mut children: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let ppid = row.info.stat.ppid;
        if present.contains(&ppid) && ppid != row.info.stat.pid {
            children.entry(ppid).or_default().push(index);
        } else {
            roots.push(index);
        }
    }
    let mut order: Vec<(usize, usize)> = Vec::with_capacity(rows.len());
    let mut visited = vec![false; rows.len()];
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|i| (0usize, i)).collect();
    while let Some((depth, index)) = stack.pop() {
        if visited[index] {
            continue;
        }
        visited[index] = true;
        order.push((depth, index));
        if let Some(kids) = children.get(&rows[index].info.stat.pid) {
            for kid in kids.iter().rev() {
                stack.push((depth + 1, *kid));
            }
        }
    }
    // PID-Zyklen (nur in kaputten Bäumen): nicht erreichte Zeilen hinten anhängen.
    for (index, seen) in visited.iter().enumerate() {
        if !seen {
            order.push((0, index));
        }
    }
    let mut slots: Vec<Option<Row>> = rows.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|(depth, index)| slots[index].take().map(|row| (depth, row)))
        .collect()
}

/// Listet Prozesse wie `ps`.
#[harw_macros::tool(
    name = "sys.ps",
    description = "Lists processes like ps from /proc: filter by pids, ppids, user, name, name_contains, masked command text and states; sort by cpu, mem, rss, start, pid or name; choose fields; tree=true for a process tree. Use when you need to find or inspect running processes instead of running ps. Returns JSON {processes:[{pid, ppid, user, state, cpu_percent, mem_percent, rss_kib, start, name, command}], count, matched, truncated}; command lines are masked for secrets, output is bounded (default 100, hard 2000), vanishing processes are tolerated. Read-only, Linux only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_ps(_context: &ToolExecutionContext, args: PsArgs) -> Result<ToolOutput, ToolsError> {
    run_blocking(TOOL, move || run(&ProcFs::real(), &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, error_of, json_of};
    use std::fs;
    use std::path::Path;

    #[allow(clippy::too_many_arguments)]
    fn add(
        root: &Path,
        pid: i32,
        ppid: i32,
        comm: &str,
        state: char,
        uid: u32,
        utime: u64,
        rss_pages: u64,
        start: u64,
        cmd: &[&str],
    ) -> TestResult {
        let dir = root.join(pid.to_string());
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("stat"),
            format!(
                "{pid} ({comm}) {state} {ppid} 1 1 0 -1 0 0 0 0 0 {utime} 0 0 0 20 0 1 0 {start} 4096000 {rss_pages} 0"
            ),
        )?;
        fs::write(
            dir.join("status"),
            format!("Name:\t{comm}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )?;
        fs::write(
            dir.join("cmdline"),
            cmd.iter().map(|c| format!("{c}\0")).collect::<String>(),
        )?;
        Ok(())
    }

    fn proc_tree() -> TestResult<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        let r = dir.path();
        fs::write(r.join("meminfo"), "MemTotal: 1048576 kB\n")?;
        fs::write(r.join("uptime"), "1000.00 500.00\n")?;
        fs::write(
            r.join("stat"),
            "cpu  1 0 1 8 0 0 0 0 0 0\ncpu0 1 0 1 8 0 0 0 0 0 0\nbtime 1700000000\n",
        )?;
        // Startticks relativ zu uptime 1000 s bei 100 Ticks/s ⇒ starttime ≤ 100000.
        add(r, 1, 0, "init", 'S', 0, 100, 256, 100, &["/sbin/init"])?;
        add(
            r,
            10,
            1,
            "sshd",
            'S',
            0,
            500,
            1024,
            2000,
            &["/usr/sbin/sshd", "-D"],
        )?;
        add(r, 20, 10, "bash", 'S', 1000, 50, 512, 30000, &["-bash"])?;
        add(
            r,
            21,
            20,
            "curl",
            'R',
            1000,
            9000,
            4096,
            90000,
            &[
                "curl",
                "--token",
                "SECRETVALUE1",
                "--password=hunter2",
                "https://u:pw@example.com/x",
            ],
        )?;
        add(r, 30, 1, "kworker/0:1", 'I', 0, 5, 0, 500, &[])?;
        add(r, 40, 1, "zombie", 'Z', 1000, 0, 0, 95000, &[])?;
        Ok(dir)
    }

    fn ps(root: &Path, args: Value) -> TestResult<Value> {
        json_of(run(&ProcFs::at(root), &serde_json::from_value(args)?))
    }

    fn pids(value: &Value) -> TestResult<Vec<i64>> {
        Ok(value["processes"]
            .as_array()
            .ok_or(TestError::Missing("processes"))?
            .iter()
            .filter_map(|p| p["pid"].as_i64())
            .collect())
    }

    #[test]
    fn default_listing_sorted_by_pid_with_default_fields() -> TestResult {
        let dir = proc_tree()?;
        let value = ps(dir.path(), json!({}))?;
        assert_eq!(pids(&value)?, vec![1, 10, 20, 21, 30, 40]);
        let first = &value["processes"][0];
        for field in DEFAULT_FIELDS {
            assert!(first.get(*field).is_some(), "missing {field}: {first}");
        }
        assert_eq!(first["name"], "init");
        assert_eq!(first["command"], "/sbin/init");
        assert_eq!(value["processes"][4]["command"], "[kworker/0:1]");
        assert_eq!(value["matched"], 6);
        Ok(())
    }

    #[test]
    fn filters_by_pid_ppid_name_user_state_command() -> TestResult {
        let dir = proc_tree()?;
        let p = dir.path();
        assert_eq!(pids(&ps(p, json!({"pids": [10, 21, 999]}))?)?, vec![10, 21]);
        assert_eq!(pids(&ps(p, json!({"ppids": [1]}))?)?, vec![10, 30, 40]);
        assert_eq!(pids(&ps(p, json!({"name": "bash"}))?)?, vec![20]);
        assert_eq!(pids(&ps(p, json!({"name": "ba"}))?)?, Vec::<i64>::new());
        assert_eq!(pids(&ps(p, json!({"name_contains": "SSH"}))?)?, vec![10]);
        assert_eq!(pids(&ps(p, json!({"user": "1000"}))?)?, vec![20, 21, 40]);
        assert_eq!(pids(&ps(p, json!({"states": ["R", "Z"]}))?)?, vec![21, 40]);
        assert_eq!(
            pids(&ps(p, json!({"command_contains": "sshd -D"}))?)?,
            vec![10]
        );
        Ok(())
    }

    #[test]
    fn sorts_by_cpu_mem_start_name_and_reverse() -> TestResult {
        let dir = proc_tree()?;
        let p = dir.path();
        assert_eq!(
            pids(&ps(p, json!({"sort": "cpu", "limit": 2}))?)?,
            vec![21, 10]
        );
        assert_eq!(
            pids(&ps(p, json!({"sort": "mem", "limit": 2}))?)?,
            vec![21, 10]
        );
        assert_eq!(pids(&ps(p, json!({"sort": "rss", "limit": 1}))?)?, vec![21]);
        assert_eq!(
            pids(&ps(p, json!({"sort": "start"}))?)?,
            vec![1, 30, 10, 20, 21, 40]
        );
        assert_eq!(
            pids(&ps(
                p,
                json!({"sort": "start", "reverse": true, "limit": 1})
            )?)?,
            vec![40]
        );
        assert_eq!(
            pids(&ps(p, json!({"sort": "name", "limit": 3}))?)?,
            vec![20, 21, 1]
        );
        Ok(())
    }

    #[test]
    fn computed_values_follow_ps_formulas() -> TestResult {
        let dir = proc_tree()?;
        let value = ps(
            dir.path(),
            json!({"pids": [21], "fields": ["cpu_percent", "mem_percent", "rss_kib", "vsz_kib", "start", "elapsed_secs", "cpu_time_secs", "threads", "uid"]}),
        )?;
        let p = &value["processes"][0];
        // Erwartung aus denselben Systemkonstanten wie das Werkzeug (Ticks, Seitengröße).
        let clock = ProcFs::real().clock();
        let ticks = clock.ticks_per_sec as f64;
        let cpu_secs = 9000.0 / ticks;
        let elapsed = 1000.0 - 90000.0 / ticks;
        let rss_kib = 4096 * clock.page_size / 1024;
        assert_eq!(p["cpu_time_secs"], round1(cpu_secs));
        assert_eq!(p["elapsed_secs"], round1(elapsed));
        assert_eq!(p["cpu_percent"], round1(cpu_secs / elapsed * 100.0));
        assert_eq!(p["rss_kib"], rss_kib);
        assert_eq!(
            p["mem_percent"],
            round1(rss_kib as f64 / 1_048_576.0 * 100.0)
        );
        assert_eq!(p["vsz_kib"], 4000);
        assert_eq!(
            p["start"],
            iso_utc(1_700_000_000 + (90000.0 / ticks) as i64)
        );
        assert_eq!(p["uid"], 1000);
        assert_eq!(p["threads"], 1);
        Ok(())
    }

    #[test]
    fn command_lines_are_masked_and_filters_do_not_probe_secrets() -> TestResult {
        let dir = proc_tree()?;
        let value = ps(dir.path(), json!({"pids": [21]}))?;
        let command = value["processes"][0]["command"].as_str().unwrap_or("");
        for leaked in ["SECRETVALUE1", "hunter2", "pw@"] {
            assert!(!command.contains(leaked), "{leaked} leaked: {command}");
        }
        assert!(command.contains("--token ***") && command.contains("--password=***"));
        // Das Original darf kein Filtertreffer sein (kein Orakel).
        assert!(pids(&ps(dir.path(), json!({"command_contains": "hunter2"}))?)?.is_empty());
        assert!(
            pids(&ps(
                dir.path(),
                json!({"command_contains": "SECRETVALUE1"})
            )?)?
            .is_empty()
        );
        assert_eq!(
            pids(&ps(dir.path(), json!({"command_contains": "--token ***"}))?)?,
            vec![21]
        );
        Ok(())
    }

    #[test]
    fn tree_orders_children_under_parents_with_depth() -> TestResult {
        let dir = proc_tree()?;
        let value = ps(dir.path(), json!({"tree": true, "fields": ["pid"]}))?;
        let rows: Vec<(i64, i64)> = value["processes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| {
                        (
                            p["pid"].as_i64().unwrap_or(-1),
                            p["depth"].as_i64().unwrap_or(-1),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            rows,
            vec![(1, 0), (10, 1), (20, 2), (21, 3), (30, 1), (40, 1)]
        );
        let filtered = ps(
            dir.path(),
            json!({"tree": true, "fields": ["pid"], "pids": [20, 21]}),
        )?;
        assert_eq!(filtered["processes"][0]["depth"], 0);
        assert_eq!(filtered["processes"][1]["depth"], 1);
        Ok(())
    }

    #[test]
    fn pid_cycles_and_self_parents_do_not_hang_the_tree() -> TestResult {
        let dir = proc_tree()?;
        add(dir.path(), 50, 51, "a", 'S', 0, 1, 1, 1, &["a"])?;
        add(dir.path(), 51, 50, "b", 'S', 0, 1, 1, 1, &["b"])?;
        add(dir.path(), 52, 52, "c", 'S', 0, 1, 1, 1, &["c"])?;
        let value = ps(
            dir.path(),
            json!({"tree": true, "fields": ["pid"], "pids": [50, 51, 52]}),
        )?;
        assert_eq!(pids(&value)?.len(), 3);
        Ok(())
    }

    #[test]
    fn limit_vanished_and_fields_are_reported() -> TestResult {
        let dir = proc_tree()?;
        fs::create_dir_all(dir.path().join("77"))?;
        let value = ps(
            dir.path(),
            json!({"limit": 2, "fields": ["pid", "name", "pid"]}),
        )?;
        assert_eq!(value["count"], 2);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["stopped"], "entry_limit");
        assert_eq!(value["vanished"], 1);
        assert_eq!(value["fields"], json!(["pid", "name"]));
        assert!(value["processes"][0].get("command").is_none());
        Ok(())
    }

    #[test]
    fn hard_limit_caps_hostile_limits() -> TestResult {
        let dir = proc_tree()?;
        for pid in 1000..(1000 + i32::try_from(HARD_LIMIT).unwrap_or(0) + 100) {
            add(dir.path(), pid, 1, "bulk", 'S', 0, 1, 1, 1, &["bulk"])?;
        }
        let value = ps(dir.path(), json!({"limit": 9_999_999, "fields": ["pid"]}))?;
        assert_eq!(value["count"], HARD_LIMIT);
        assert_eq!(value["truncated"], true);
        Ok(())
    }

    #[test]
    fn invalid_arguments_are_rejected() -> TestResult {
        let dir = proc_tree()?;
        let p = ProcFs::at(dir.path());
        let many: Vec<i64> = (1..=300).collect();
        for bad in [
            json!({"sort": "bogus"}),
            json!({"fields": ["pid", "password"]}),
            json!({"fields": []}),
            json!({"states": ["Q"]}),
            json!({"states": ["RS"]}),
            json!({"pids": [-1]}),
            json!({"pids": [4294967296i64]}),
            json!({"pids": many}),
            json!({"user": "no-such-user-xyz"}),
            json!({"name": ""}),
            json!({"name_contains": "a\u{0}b"}),
        ] {
            let output = run(&p, &serde_json::from_value(bad.clone())?);
            error_of(output).map_err(|e| TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        assert!(serde_json::from_value::<PsArgs>(json!({"signal": 9})).is_err());
        Ok(())
    }

    #[test]
    fn real_proc_lists_this_process_with_plausible_values() -> TestResult {
        let me = i64::from(std::process::id());
        let value = json_of(run(
            &ProcFs::real(),
            &serde_json::from_value(
                json!({"pids": [me], "fields": ["pid", "ppid", "name", "state", "rss_kib", "threads", "command", "user"]}),
            )?,
        ))?;
        assert_eq!(value["count"], 1);
        let p = &value["processes"][0];
        assert_eq!(p["pid"], me);
        assert!(p["rss_kib"].as_u64().is_some_and(|r| r > 0));
        assert!(p["threads"].as_u64().is_some_and(|t| t >= 1));
        assert!(p["command"].as_str().is_some_and(|c| !c.is_empty()));
        Ok(())
    }
}
