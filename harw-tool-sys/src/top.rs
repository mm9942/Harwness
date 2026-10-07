//! `sys.top` — Schnappschuss wie `top -b -n 1` mit echter Intervallmessung.
//!
//! Zwei Messungen im Abstand `interval_ms` (Standard 500, 100 bis 2000):
//! CPU-Prozent je Prozess sind der Zuwachs an CPU-Ticks geteilt durch die
//! gemessene Wanduhrzeit (100 % = ein voller Kern, wie `top` im Irix-Modus);
//! die Gesamtauslastung kommt aus der Differenz der `cpu`-Zeile von
//! `/proc/stat`. Dazu Lastdurchschnitt, Taskzahlen nach Zustand und Speicher.
//! Kommandozeilen sind maskiert.

use crate::mask::masked_command;
use crate::procfs::{Clock, CpuTimes, ProcFs, ProcInfo};
use harw_tool_fsread::blocking::run_blocking;
use harw_tool_fsread::budget::{Collector, choice, fail, limit_or, ok};
use harw_tool_fsread::users::UserDb;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Name des Werkzeugs.
pub const TOOL: &str = "sys.top";

/// Standard-Intervall.
pub const DEFAULT_INTERVAL_MS: u64 = 500;

/// Kleinstes Intervall.
pub const MIN_INTERVAL_MS: u64 = 100;

/// Größtes Intervall.
pub const MAX_INTERVAL_MS: u64 = 2_000;

/// Standard-Zeilenzahl.
pub const DEFAULT_LIMIT: usize = 15;

/// Hartes Limit.
pub const HARD_LIMIT: usize = 100;

/// Argumente für `sys.top`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct TopArgs {
    /// Number of processes listed (default 15, hard 100).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
    /// Sort key: 'cpu' (default) or 'mem' (resident memory), both descending.
    #[serde(default)]
    pub sort: Option<String>,
    /// Measurement interval in milliseconds between the two samples (100-2000, default 500).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub interval_ms: Option<u64>,
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Ein Prozess aus dem zweiten Sample mit Zuwachs.
struct Sampled {
    info: ProcInfo,
    delta_ticks: u64,
}

/// Zuwachs an CPU-Ticks je PID zwischen zwei Messungen.
fn deltas(first: &[ProcInfo], second: Vec<ProcInfo>) -> Vec<Sampled> {
    let before: HashMap<i32, (u64, u64)> = first
        .iter()
        .map(|p| (p.stat.pid, (p.stat.starttime, p.stat.utime + p.stat.stime)))
        .collect();
    second
        .into_iter()
        .map(|info| {
            let now = info.stat.utime + info.stat.stime;
            // Gleiche PID mit anderer Startzeit = wiederverwendete PID: Basis 0.
            let base = before
                .get(&info.stat.pid)
                .filter(|(start, _)| *start == info.stat.starttime)
                .map_or(0, |(_, ticks)| *ticks);
            Sampled {
                delta_ticks: now.saturating_sub(base),
                info,
            }
        })
        .collect()
}

/// Führt `sys.top` gegen `procfs` aus. `sleep` wartet das Intervall ab
/// (in Tests ersetzbar).
#[must_use]
pub fn run(procfs: &ProcFs, args: &TopArgs, sleep: impl FnOnce(Duration)) -> ToolOutput {
    match build(procfs, args, sleep) {
        Ok(output) => output,
        Err(message) => fail(TOOL, message),
    }
}

fn build(
    procfs: &ProcFs,
    args: &TopArgs,
    sleep: impl FnOnce(Duration),
) -> Result<ToolOutput, String> {
    let sort = choice("sort", args.sort.as_deref(), &["cpu", "mem"], "cpu")?;
    let interval_ms = args.interval_ms.unwrap_or(DEFAULT_INTERVAL_MS);
    if !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&interval_ms) {
        return Err(format!(
            "interval_ms must be between {MIN_INTERVAL_MS} and {MAX_INTERVAL_MS}, got {interval_ms}"
        ));
    }
    let limit = limit_or(args.limit, DEFAULT_LIMIT, HARD_LIMIT);

    let clock = procfs.clock();
    let started = Instant::now();
    let (first, _) = procfs.processes(false);
    let cpu_before = procfs.cpu().map(|(times, _, cpus)| (times, cpus));
    sleep(Duration::from_millis(interval_ms));
    let (second, vanished) = procfs.processes(true);
    let cpu_after = procfs.cpu();
    let wall = started.elapsed().as_secs_f64().max(0.001);

    let db = UserDb::load();
    let mut states: HashMap<char, usize> = HashMap::new();
    for info in &second {
        *states.entry(info.stat.state).or_default() += 1;
    }
    let total_tasks = second.len();
    let mut rows = deltas(&first, second);
    let ticks = clock.ticks_per_sec as f64;
    let cpu_pct = |delta: u64| delta as f64 / ticks / wall * 100.0;
    rows.sort_by(|a, b| {
        use std::cmp::Ordering;
        let primary = if sort == "mem" {
            b.info
                .rss_kib(clock.page_size)
                .cmp(&a.info.rss_kib(clock.page_size))
        } else {
            cpu_pct(b.delta_ticks)
                .partial_cmp(&cpu_pct(a.delta_ticks))
                .unwrap_or(Ordering::Equal)
        };
        primary.then(a.info.stat.pid.cmp(&b.info.stat.pid))
    });
    let mut out = Collector::new(limit);
    for row in &rows {
        if !out.push(entry(row, &clock, &db, cpu_pct(row.delta_ticks))) {
            break;
        }
    }

    let cpu_summary = match (cpu_before, cpu_after) {
        (Some((before, cpus)), Some((after, _, _))) => Some(cpu_summary(before, after, cpus)),
        _ => None,
    };
    let mem = procfs.meminfo();
    let kib = |key: &str| mem.get(key).copied().unwrap_or(0).saturating_mul(1024);
    let total = kib("MemTotal");
    let available = kib("MemAvailable");
    let load = procfs.loadavg().map(|l| json!([l.one, l.five, l.fifteen]));
    let count = out.len();
    let truncated = out.truncated();
    let stopped = out.stop_reason();
    let state_count = |c: char| states.get(&c).copied().unwrap_or(0);
    Ok(ok(
        TOOL,
        format!("{total_tasks} tasks, top {count} by {sort} over {interval_ms} ms"),
        json!({
            "processes": out.into_items(),
            "count": count,
            "sort": sort,
            "interval_ms": interval_ms,
            "measured_secs": round1(wall * 10.0) / 10.0,
            "load_average": load,
            "tasks": {
                "total": total_tasks,
                "running": state_count('R'),
                "sleeping": state_count('S') + state_count('D') + state_count('I'),
                "stopped": state_count('T') + state_count('t'),
                "zombie": state_count('Z'),
                "vanished": vanished,
            },
            "cpu": cpu_summary,
            "memory": {
                "total_bytes": total,
                "available_bytes": available,
                "used_bytes": total.saturating_sub(available),
                "swap_total_bytes": kib("SwapTotal"),
                "swap_free_bytes": kib("SwapFree"),
            },
            "truncated": truncated,
            "stopped": stopped,
        }),
    ))
}

fn entry(row: &Sampled, clock: &Clock, db: &UserDb, cpu: f64) -> Value {
    let info = &row.info;
    let rss_kib = info.rss_kib(clock.page_size);
    let mem_percent = if clock.mem_total_kib > 0 {
        rss_kib as f64 / clock.mem_total_kib as f64 * 100.0
    } else {
        0.0
    };
    let command = if info.cmdline.is_empty() {
        format!("[{}]", info.stat.comm)
    } else {
        masked_command(&info.cmdline, 256)
    };
    json!({
        "pid": info.stat.pid,
        "user": db.user_or_id(info.uid),
        "state": info.stat.state.to_string(),
        "cpu_percent": round1(cpu),
        "mem_percent": round1(mem_percent),
        "rss_kib": rss_kib,
        "threads": info.stat.threads,
        "name": info.stat.comm,
        "command": command,
    })
}

fn cpu_summary(before: CpuTimes, after: CpuTimes, cpus: usize) -> Value {
    let total = after.total().saturating_sub(before.total());
    let pct = |a: u64, b: u64| {
        if total == 0 {
            0.0
        } else {
            round1(a.saturating_sub(b) as f64 / total as f64 * 100.0)
        }
    };
    json!({
        "cpus": cpus,
        "user_percent": pct(after.user, before.user),
        "system_percent": pct(after.system, before.system),
        "idle_percent": pct(after.idle, before.idle),
        "iowait_percent": pct(after.iowait, before.iowait),
        "steal_percent": pct(after.steal, before.steal),
    })
}

/// Zeigt die aktivsten Prozesse wie `top`.
#[harw_macros::tool(
    name = "sys.top",
    description = "Takes a top-like snapshot with a real measurement interval: busiest processes by cpu or mem (interval_ms 100-2000), overall CPU split, load average, task counts by state and memory. Use when you need to know what is using CPU or memory right now instead of running top. Returns JSON {processes:[{pid,user,cpu_percent,mem_percent,rss_kib,name,command}], cpu, load_average, tasks, memory}; 100% means one full core, command lines are masked. Read-only, Linux only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_top(_context: &ToolExecutionContext, args: TopArgs) -> Result<ToolOutput, ToolsError> {
    run_blocking(TOOL, move || {
        run(&ProcFs::real(), &args, std::thread::sleep)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, error_of, json_of};
    use std::fs;
    use std::path::Path;

    fn write_proc(
        root: &Path,
        pid: i32,
        comm: &str,
        state: char,
        utime: u64,
        rss: u64,
    ) -> TestResult {
        let dir = root.join(pid.to_string());
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("stat"),
            format!(
                "{pid} ({comm}) {state} 1 1 1 0 -1 0 0 0 0 0 {utime} 0 0 0 20 0 1 0 100 4096 {rss} 0"
            ),
        )?;
        fs::write(dir.join("status"), "Uid:\t0\t0\t0\t0\n")?;
        fs::write(dir.join("cmdline"), format!("{comm}\0--password=pw\0"))?;
        Ok(())
    }

    fn base(root: &Path) -> TestResult {
        fs::write(
            root.join("meminfo"),
            "MemTotal: 1000 kB\nMemAvailable: 400 kB\nSwapTotal: 100 kB\nSwapFree: 50 kB\n",
        )?;
        fs::write(root.join("uptime"), "1000.00 1.00\n")?;
        fs::write(root.join("loadavg"), "0.10 0.20 0.30 1/10 99\n")?;
        fs::write(
            root.join("stat"),
            "cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 100 0 100 800 0 0 0 0 0 0\nbtime 1700000000\n",
        )?;
        write_proc(root, 1, "idle", 'S', 10, 1)?;
        write_proc(root, 2, "busy", 'R', 100, 8)?;
        write_proc(root, 3, "fat", 'S', 10, 100)?;
        write_proc(root, 4, "gone", 'Z', 0, 0)?;
        Ok(())
    }

    fn top(root: &Path, args: Value, advance: impl FnOnce(Duration)) -> TestResult<Value> {
        json_of(run(
            &ProcFs::at(root),
            &serde_json::from_value(args)?,
            advance,
        ))
    }

    #[test]
    fn cpu_percent_is_the_tick_delta_over_wall_time() -> TestResult {
        let dir = tempfile::tempdir()?;
        base(dir.path())?;
        let root = dir.path().to_path_buf();
        let clock = ProcFs::real().clock();
        let value = top(dir.path(), json!({"limit": 3}), move |interval| {
            assert_eq!(interval, Duration::from_millis(DEFAULT_INTERVAL_MS));
            // Zwischen den Messungen verbraucht `busy` 40 Ticks, `idle` 5; /proc/stat wächst um 200 Ticks.
            let _ = write_proc(&root, 2, "busy", 'R', 140, 8);
            let _ = write_proc(&root, 1, "idle", 'S', 15, 1);
            let _ = fs::write(
                root.join("stat"),
                "cpu  150 0 150 900 0 0 0 0 0 0\ncpu0 150 0 150 900 0 0 0 0 0 0\nbtime 1700000000\n",
            );
            std::thread::sleep(Duration::from_millis(100));
        })?;
        let rows = value["processes"]
            .as_array()
            .ok_or(TestError::Missing("processes"))?;
        assert_eq!(rows[0]["pid"], 2);
        assert_eq!(rows[1]["pid"], 1);
        // Gemessene Zeit ≥ 0.1 s; 40 Ticks bei 100 Hz = 0.4 s CPU => höchstens 400 %.
        let measured = value["measured_secs"].as_f64().unwrap_or(0.0).max(0.1);
        let expected = 40.0 / clock.ticks_per_sec as f64 / measured * 100.0;
        let got = rows[0]["cpu_percent"].as_f64().unwrap_or(-1.0);
        assert!(
            (got - expected).abs() < expected * 0.5 + 1.0,
            "got {got}, expected ~{expected}"
        );
        assert!(rows[1]["cpu_percent"].as_f64().unwrap_or(99.0) < got);
        assert_eq!(value["cpu"]["user_percent"], 25.0);
        assert_eq!(value["cpu"]["system_percent"], 25.0);
        assert_eq!(value["cpu"]["idle_percent"], 50.0);
        assert_eq!(value["cpu"]["cpus"], 1);
        assert_eq!(value["tasks"]["total"], 4);
        assert_eq!(value["tasks"]["running"], 1);
        assert_eq!(value["tasks"]["zombie"], 1);
        assert_eq!(value["tasks"]["sleeping"], 2);
        assert_eq!(value["load_average"], json!([0.1, 0.2, 0.3]));
        assert_eq!(value["memory"]["used_bytes"], 600 * 1024);
        Ok(())
    }

    #[test]
    fn sort_by_memory_and_masking() -> TestResult {
        let dir = tempfile::tempdir()?;
        base(dir.path())?;
        let value = top(dir.path(), json!({"sort": "mem", "limit": 2}), |_| {})?;
        assert_eq!(value["processes"][0]["pid"], 3);
        assert_eq!(value["processes"][1]["pid"], 2);
        let text = serde_json::to_string(&value)?;
        assert!(
            !text.contains("=pw") && text.contains("--password=***"),
            "{text}"
        );
        assert_eq!(value["truncated"], true);
        Ok(())
    }

    #[test]
    fn reused_pids_do_not_inherit_old_ticks() -> TestResult {
        let dir = tempfile::tempdir()?;
        base(dir.path())?;
        let root = dir.path().to_path_buf();
        let value = top(dir.path(), json!({"limit": 4}), move |_| {
            // PID 1 „startet neu“ (andere Startzeit): Basis ist 0, nicht der alte Zählerstand 10.
            let _ = fs::write(
                root.join("1/stat"),
                "1 (idle) S 1 1 1 0 -1 0 0 0 0 0 12 0 0 0 20 0 1 0 999 4096 1 0",
            );
        })?;
        let row = value["processes"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["pid"] == 1))
            .ok_or(TestError::Missing("pid 1"))?;
        assert!(row["cpu_percent"].as_f64().unwrap_or(0.0) > 0.0);
        Ok(())
    }

    #[test]
    fn interval_and_sort_are_validated() -> TestResult {
        let dir = tempfile::tempdir()?;
        base(dir.path())?;
        for bad in [
            json!({"interval_ms": 0}),
            json!({"interval_ms": 99}),
            json!({"interval_ms": 2001}),
            json!({"interval_ms": u64::MAX}),
            json!({"sort": "pid"}),
        ] {
            let output = run(
                &ProcFs::at(dir.path()),
                &serde_json::from_value(bad.clone())?,
                |_| {},
            );
            error_of(output).map_err(|e| TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        assert!(serde_json::from_value::<TopArgs>(json!({"batch": true})).is_err());
        Ok(())
    }

    #[test]
    fn real_proc_snapshot_has_plausible_shape() -> TestResult {
        let value = json_of(run(
            &ProcFs::real(),
            &serde_json::from_value(json!({"interval_ms": 100, "limit": 5}))?,
            std::thread::sleep,
        ))?;
        assert!(value["tasks"]["total"].as_u64().is_some_and(|t| t >= 1));
        assert!(
            value["memory"]["total_bytes"]
                .as_u64()
                .is_some_and(|t| t > 0)
        );
        assert!(
            value["processes"]
                .as_array()
                .is_some_and(|p| !p.is_empty() && p.len() <= 5)
        );
        Ok(())
    }
}
