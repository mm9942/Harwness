//! `sys.free`, `sys.uptime` und `sys.uname` — Systemzustand in reinem Rust.
//!
//! - `free`: Speicher aus `/proc/meminfo` (alle Werte in Bytes; `used` ist wie
//!   in procps-ng ≥ 4 `total - available`).
//! - `uptime`: `/proc/uptime` und `/proc/loadavg`, dazu der Startzeitpunkt.
//! - `uname`: `uname(2)` über `rustix`; Reihenfolge wie `uname -a`
//!   (Kernelname, Hostname, Release, Version, Maschine, Betriebssystem).

use crate::procfs::ProcFs;
use harw_tool_fsread::blocking::run_blocking;
use harw_tool_fsread::budget::{fail, flag, ok};
use harw_tool_fsread::meta::{human_size, iso_utc};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

/// Name von `sys.free`.
pub const FREE_TOOL: &str = "sys.free";

/// Name von `sys.uptime`.
pub const UPTIME_TOOL: &str = "sys.uptime";

/// Name von `sys.uname`.
pub const UNAME_TOOL: &str = "sys.uname";

/// Argumente für `sys.free`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct FreeArgs {
    /// -h: add human-readable strings (1024-based) next to the byte values.
    #[serde(default)]
    pub human: Option<bool>,
}

/// Argumente für `sys.uptime`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct UptimeArgs {}

/// Argumente für `sys.uname`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct UnameArgs {
    /// -a / --all: everything, in uname -a order.
    #[serde(default)]
    pub all: Option<bool>,
    /// -s / --kernel-name (the default when nothing is selected).
    #[serde(default)]
    pub kernel_name: Option<bool>,
    /// -n / --nodename: network node hostname.
    #[serde(default)]
    pub nodename: Option<bool>,
    /// -r / --kernel-release.
    #[serde(default)]
    pub kernel_release: Option<bool>,
    /// -v / --kernel-version.
    #[serde(default)]
    pub kernel_version: Option<bool>,
    /// -m / --machine: hardware architecture.
    #[serde(default)]
    pub machine: Option<bool>,
    /// -o / --operating-system.
    #[serde(default)]
    pub operating_system: Option<bool>,
}

fn kib_to_bytes(kib: u64) -> u64 {
    kib.saturating_mul(1024)
}

/// Führt `sys.free` aus.
#[must_use]
pub fn run_free(procfs: &ProcFs, args: &FreeArgs) -> ToolOutput {
    let mem = procfs.meminfo();
    if mem.is_empty() {
        return fail(FREE_TOOL, "/proc/meminfo is not readable");
    }
    let get = |key: &str| kib_to_bytes(mem.get(key).copied().unwrap_or(0));
    let total = get("MemTotal");
    let free = get("MemFree");
    let buffers = get("Buffers");
    let cached = get("Cached") + get("SReclaimable");
    let available = if mem.contains_key("MemAvailable") {
        get("MemAvailable")
    } else {
        free + buffers + cached
    };
    let used = total.saturating_sub(available);
    let swap_total = get("SwapTotal");
    let swap_free = get("SwapFree");
    let swap_used = swap_total.saturating_sub(swap_free);
    let mut data = json!({
        "total_bytes": total,
        "used_bytes": used,
        "free_bytes": free,
        "shared_bytes": get("Shmem"),
        "buffers_bytes": buffers,
        "cached_bytes": cached,
        "buff_cache_bytes": buffers + cached,
        "available_bytes": available,
        "swap_total_bytes": swap_total,
        "swap_used_bytes": swap_used,
        "swap_free_bytes": swap_free,
        "used_percent": if total == 0 { 0.0 } else { (used as f64 / total as f64 * 1000.0).round() / 10.0 },
        "truncated": false,
    });
    if flag(args.human) {
        if let Some(map) = data.as_object_mut() {
            for (key, bytes) in [
                ("total", total),
                ("used", used),
                ("free", free),
                ("available", available),
                ("buff_cache", buffers + cached),
                ("swap_total", swap_total),
                ("swap_used", swap_used),
            ] {
                map.insert(format!("{key}_human"), json!(human_size(bytes)));
            }
        }
    }
    ok(
        FREE_TOOL,
        format!(
            "{} used of {}, {} available",
            human_size(used),
            human_size(total),
            human_size(available)
        ),
        data,
    )
}

/// `up 3 days, 4:05` wie `uptime -p`-ähnlich.
#[must_use]
pub fn pretty_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = secs % 86_400 / 3600;
    let minutes = secs % 3600 / 60;
    let plural = |n: u64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    match (days, hours, minutes) {
        (0, 0, m) => format!("up {}", plural(m, "minute")),
        (0, h, m) => format!("up {h}:{m:02}"),
        (d, h, m) => format!("up {}, {h}:{m:02}", plural(d, "day")),
    }
}

/// Führt `sys.uptime` aus. `now` sind Unix-Sekunden.
#[must_use]
pub fn run_uptime(procfs: &ProcFs, now: i64, _args: &UptimeArgs) -> ToolOutput {
    let Some((uptime, idle)) = procfs.uptime() else {
        return fail(UPTIME_TOOL, "/proc/uptime is not readable");
    };
    let secs = uptime.max(0.0) as u64;
    let since = now.saturating_sub(i64::try_from(secs).unwrap_or(0));
    let load = procfs.loadavg();
    let mut data = json!({
        "uptime_secs": secs,
        "pretty": pretty_uptime(secs),
        "up_since": iso_utc(since),
        "idle_secs": idle.max(0.0) as u64,
        "now": iso_utc(now),
        "truncated": false,
    });
    if let Some(load) = load {
        data["load_average"] = json!({"one": load.one, "five": load.five, "fifteen": load.fifteen});
        data["tasks_running"] = json!(load.running);
        data["tasks_total"] = json!(load.total);
    }
    let summary = match load {
        Some(l) => format!(
            "{}, load average: {:.2}, {:.2}, {:.2}",
            pretty_uptime(secs),
            l.one,
            l.five,
            l.fifteen
        ),
        None => pretty_uptime(secs),
    };
    ok(UPTIME_TOOL, summary, data)
}

/// Führt `sys.uname` aus.
#[must_use]
pub fn run_uname(args: &UnameArgs) -> ToolOutput {
    let all = flag(args.all);
    let wanted = [
        flag(args.kernel_name),
        flag(args.nodename),
        flag(args.kernel_release),
        flag(args.kernel_version),
        flag(args.machine),
        flag(args.operating_system),
    ];
    let wanted = if all {
        [true; 6]
    } else if wanted.iter().all(|w| !*w) {
        [true, false, false, false, false, false]
    } else {
        wanted
    };
    let info = rustix::system::uname();
    let text = |value: &std::ffi::CStr| value.to_string_lossy().into_owned();
    let os = if cfg!(target_os = "linux") {
        "GNU/Linux".to_owned()
    } else {
        text(info.sysname())
    };
    let all_values = [
        ("kernel_name", text(info.sysname())),
        ("nodename", text(info.nodename())),
        ("kernel_release", text(info.release())),
        ("kernel_version", text(info.version())),
        ("machine", text(info.machine())),
        ("operating_system", os),
    ];
    let mut fields: Vec<Value> = Vec::new();
    let mut line: Vec<String> = Vec::new();
    for ((name, value), selected) in all_values.into_iter().zip(wanted) {
        if selected {
            line.push(value.clone());
            fields.push(json!({"field": name, "value": value}));
        }
    }
    let text = line.join(" ");
    ok(
        UNAME_TOOL,
        text.clone(),
        json!({"fields": fields, "text": text, "truncated": false}),
    )
}

/// Zeigt den Speicherzustand wie `free`.
#[harw_macros::tool(
    name = "sys.free",
    description = "Reports memory and swap like free: total, used (total minus available), free, shared, buffers, cache, available in bytes, optionally human-readable (-h). Use when you need the current memory situation instead of running free. Returns JSON {total_bytes, used_bytes, available_bytes, swap_*_bytes, used_percent}. Read-only, Linux only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_free(
    _context: &ToolExecutionContext,
    args: FreeArgs,
) -> Result<ToolOutput, ToolsError> {
    run_blocking(FREE_TOOL, move || run_free(&ProcFs::real(), &args)).await
}

/// Zeigt Laufzeit und Last wie `uptime`.
#[harw_macros::tool(
    name = "sys.uptime",
    description = "Reports how long the system has been up, since when, and the 1/5/15 minute load averages like uptime. Use when you need system age or load instead of running uptime. Returns JSON {uptime_secs, pretty, up_since, load_average:{one,five,fifteen}, tasks_running, tasks_total}. Read-only, Linux only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_uptime(
    _context: &ToolExecutionContext,
    args: UptimeArgs,
) -> Result<ToolOutput, ToolsError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    run_blocking(UPTIME_TOOL, move || run_uptime(&ProcFs::real(), now, &args)).await
}

/// Zeigt Kernel- und Systeminformationen wie `uname`.
#[harw_macros::tool(
    name = "sys.uname",
    description = "Reports kernel and system identification like uname: -a all, -s kernel_name (default), -n nodename, -r kernel_release, -v kernel_version, -m machine, -o operating_system. Use when you need kernel, hostname or architecture instead of running uname. Returns JSON {fields:[{field,value}], text} in uname -a order. Read-only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_uname(
    _context: &ToolExecutionContext,
    args: UnameArgs,
) -> Result<ToolOutput, ToolsError> {
    run_blocking(UNAME_TOOL, move || run_uname(&args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, json_of};
    use std::fs;

    fn fake(meminfo: &str) -> TestResult<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        fs::write(dir.path().join("meminfo"), meminfo)?;
        fs::write(dir.path().join("uptime"), "266461.78 1000.25\n")?;
        fs::write(dir.path().join("loadavg"), "0.52 0.41 0.30 2/345 6789\n")?;
        Ok(dir)
    }

    #[test]
    fn free_uses_available_for_used() -> TestResult {
        let dir = fake(
            "MemTotal: 1000 kB\nMemFree: 100 kB\nMemAvailable: 400 kB\nBuffers: 50 kB\nCached: 200 kB\nSReclaimable: 10 kB\nShmem: 5 kB\nSwapTotal: 2000 kB\nSwapFree: 1500 kB\n",
        )?;
        let value = json_of(run_free(
            &ProcFs::at(dir.path()),
            &FreeArgs { human: Some(true) },
        ))?;
        assert_eq!(value["total_bytes"], 1000 * 1024);
        assert_eq!(value["used_bytes"], 600 * 1024);
        assert_eq!(value["free_bytes"], 100 * 1024);
        assert_eq!(value["available_bytes"], 400 * 1024);
        assert_eq!(value["buff_cache_bytes"], 260 * 1024);
        assert_eq!(value["swap_used_bytes"], 500 * 1024);
        assert_eq!(value["shared_bytes"], 5 * 1024);
        assert_eq!(value["used_percent"], 60.0);
        assert_eq!(value["total_human"], "1000K");
        Ok(())
    }

    #[test]
    fn free_without_memavailable_falls_back_and_empty_proc_fails() -> TestResult {
        let dir = fake("MemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 150 kB\n")?;
        let value = json_of(run_free(&ProcFs::at(dir.path()), &FreeArgs { human: None }))?;
        assert_eq!(value["available_bytes"], 300 * 1024);
        assert!(value["total_human"].is_null());
        let broken = run_free(
            &ProcFs::at("/definitely/not/here"),
            &FreeArgs { human: None },
        );
        assert!(matches!(broken, ToolOutput::Error { .. }));
        Ok(())
    }

    #[test]
    fn uptime_is_computed_from_proc_files() -> TestResult {
        let dir = fake("MemTotal: 1 kB\n")?;
        let value = json_of(run_uptime(
            &ProcFs::at(dir.path()),
            1_800_000_000,
            &UptimeArgs {},
        ))?;
        assert_eq!(value["uptime_secs"], 266_461);
        assert_eq!(value["pretty"], "up 3 days, 2:01");
        assert_eq!(value["up_since"], iso_utc(1_800_000_000 - 266_461));
        assert_eq!(value["load_average"]["five"], 0.41);
        assert_eq!(value["tasks_total"], 345);
        assert!(matches!(
            run_uptime(&ProcFs::at("/nope"), 0, &UptimeArgs {}),
            ToolOutput::Error { .. }
        ));
        Ok(())
    }

    #[test]
    fn pretty_uptime_shapes() -> TestResult {
        assert_eq!(pretty_uptime(59), "up 0 minutes");
        assert_eq!(pretty_uptime(60), "up 1 minute");
        assert_eq!(pretty_uptime(3 * 3600 + 5 * 60), "up 3:05");
        assert_eq!(pretty_uptime(86_400), "up 1 day, 0:00");
        Ok(())
    }

    #[test]
    fn uname_selection_and_order() -> TestResult {
        let none = json_of(run_uname(&UnameArgs {
            all: None,
            kernel_name: None,
            nodename: None,
            kernel_release: None,
            kernel_version: None,
            machine: None,
            operating_system: None,
        }))?;
        assert_eq!(none["text"], "Linux");
        let all = json_of(run_uname(&UnameArgs {
            all: Some(true),
            kernel_name: None,
            nodename: None,
            kernel_release: None,
            kernel_version: None,
            machine: None,
            operating_system: None,
        }))?;
        let fields: Vec<String> = all["fields"]
            .as_array()
            .map(|m| {
                m.iter()
                    .filter_map(|f| f["field"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            fields,
            vec![
                "kernel_name",
                "nodename",
                "kernel_release",
                "kernel_version",
                "machine",
                "operating_system"
            ]
        );
        let some = json_of(run_uname(&UnameArgs {
            all: None,
            kernel_name: None,
            nodename: None,
            kernel_release: Some(true),
            kernel_version: None,
            machine: Some(true),
            operating_system: None,
        }))?;
        let text = some["text"].as_str().unwrap_or("");
        assert_eq!(text.split(' ').count(), 2, "{text}");
        assert!(serde_json::from_value::<UnameArgs>(json!({"processor": true})).is_err());
        Ok(())
    }
}
