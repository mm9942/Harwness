//! `sys.lsof` — offene Dateihandles **eines eigenen Prozesses** (`lsof -p`).
//!
//! Liest `/proc/<pid>/fd`, `fdinfo` und `cwd`/`exe`/`root`. Nur Prozesse des
//! eigenen effektiven Benutzers werden untersucht (das erzwingt der Kernel
//! ohnehin, hier zusätzlich vorab geprüft und klar gemeldet). Sockets werden
//! über ihre Inode-Nummer mit `/proc/net/*` abgeglichen.
//!
//! Gemeldet werden nur Namen und Modi, nie Inhalte. Verschwindende
//! Deskriptoren (Wettlauf) werden übersprungen.

use crate::net::{read_inet, read_unix};
use crate::procfs::ProcFs;
use harw_tool_fsread::blocking::run_blocking;
use harw_tool_fsread::budget::{Collector, fail, limit_or, ok};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Name des Werkzeugs.
pub const TOOL: &str = "sys.lsof";

/// Standard-Limit.
pub const DEFAULT_LIMIT: usize = 200;

/// Hartes Limit.
pub const HARD_LIMIT: usize = 2_000;

/// Höchstzahl gelesener Deskriptoren (Schutz vor Prozessen mit Millionen FDs).
pub const MAX_FDS_SCANNED: usize = 20_000;

/// Argumente für `sys.lsof`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct LsofArgs {
    /// -p: the process ID to inspect (must belong to the current user).
    pub pid: i64,
    /// Maximum number of file descriptors listed (default 200, hard 2000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
}

/// Teilt ein Link-Ziel wie `socket:[123]` in (Art, Nummer).
fn bracketed(target: &str) -> Option<(&str, u64)> {
    let (kind, rest) = target.split_once(":[")?;
    let number = rest.strip_suffix(']')?.parse().ok()?;
    Some((kind, number))
}

/// Liest `flags:` und `pos:` aus einer `fdinfo`-Datei.
fn parse_fdinfo(text: &str) -> (Option<u32>, Option<u64>) {
    let mut flags = None;
    let mut pos = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("flags:") {
            flags = u32::from_str_radix(rest.trim(), 8).ok();
        } else if let Some(rest) = line.strip_prefix("pos:") {
            pos = rest.trim().parse().ok();
        }
    }
    (flags, pos)
}

fn access_mode(flags: u32) -> &'static str {
    match flags & 0o3 {
        0 => "r",
        1 => "w",
        2 => "rw",
        _ => "?",
    }
}

/// Führt `sys.lsof` gegen `procfs` aus; `own_uid` ist die effektive UID.
#[must_use]
pub fn run(procfs: &ProcFs, own_uid: u32, args: &LsofArgs) -> ToolOutput {
    let Some(pid) = i32::try_from(args.pid).ok().filter(|p| *p > 0) else {
        return fail(
            TOOL,
            format!("invalid pid {}: must be a positive process ID", args.pid),
        );
    };
    let Some(info) = procfs.process(pid, false) else {
        return fail(TOOL, format!("no such process: {pid}"));
    };
    if info.uid != own_uid {
        return fail(
            TOOL,
            format!(
                "process {pid} belongs to uid {}; sys.lsof only inspects processes of the current user (uid {own_uid})",
                info.uid
            ),
        );
    }
    let limit = limit_or(args.limit, DEFAULT_LIMIT, HARD_LIMIT);
    let base = procfs.root().join(pid.to_string());

    let mut sockets: HashMap<u64, String> = HashMap::new();
    for s in read_inet(procfs) {
        sockets.insert(
            s.inode,
            format!(
                "{} {}:{} -> {}:{} {}",
                s.proto, s.local_addr, s.local_port, s.remote_addr, s.remote_port, s.state
            ),
        );
    }
    for s in read_unix(procfs) {
        sockets.insert(
            s.inode,
            format!("unix {} {} {}", s.kind, s.state, s.path)
                .trim_end()
                .to_owned(),
        );
    }

    let link = |name: &str| -> Value {
        match std::fs::read_link(base.join(name)) {
            Ok(target) => json!(target.to_string_lossy()),
            Err(_) => Value::Null,
        }
    };
    let links = json!({"cwd": link("cwd"), "exe": link("exe"), "root": link("root")});

    let mut fds: Vec<u32> = match std::fs::read_dir(base.join("fd")) {
        Ok(dir) => dir
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()))
            .take(MAX_FDS_SCANNED)
            .collect(),
        Err(error) => {
            return fail(
                TOOL,
                format!(
                    "cannot read the descriptors of process {pid}: {}",
                    harw_tool_fsread::scope::io_message(&error)
                ),
            );
        }
    };
    fds.sort_unstable();
    let total = fds.len();
    let mut out = Collector::new(limit);
    let mut vanished = 0usize;
    for fd in fds {
        let Ok(target) = std::fs::read_link(base.join("fd").join(fd.to_string())) else {
            vanished += 1;
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        let (flags, pos) = std::fs::read_to_string(base.join("fdinfo").join(fd.to_string()))
            .map(|t| parse_fdinfo(&t))
            .unwrap_or((None, None));
        let deleted = target.ends_with(" (deleted)");
        let (kind, detail) = match bracketed(&target) {
            Some(("socket", inode)) => ("socket", sockets.get(&inode).cloned()),
            Some(("pipe", _)) => ("pipe", None),
            Some(("anon_inode", _)) => ("anon_inode", None),
            _ if target.starts_with("anon_inode:") => ("anon_inode", None),
            _ if target.starts_with("/dev/") => ("device", None),
            _ if target.starts_with('/') => {
                let is_dir = std::fs::metadata(base.join("fd").join(fd.to_string()))
                    .map(|m| m.is_dir())
                    .unwrap_or(false);
                (if is_dir { "dir" } else { "file" }, None)
            }
            _ => ("other", None),
        };
        let mut entry = json!({"fd": fd, "type": kind, "target": target, "deleted": deleted});
        if let Some(flags) = flags {
            entry["access"] = json!(access_mode(flags));
            entry["flags_octal"] = json!(format!("{flags:o}"));
        }
        if let Some(pos) = pos {
            entry["offset"] = json!(pos);
        }
        if let Some(detail) = detail {
            entry["socket"] = json!(detail);
        }
        if !out.push(entry) {
            break;
        }
    }
    let truncated = out.truncated();
    let stopped = out.stop_reason();
    let count = out.len();
    ok(
        TOOL,
        format!(
            "{count} of {total} descriptors of process {pid} ({}){}",
            info.stat.comm,
            if truncated { ", truncated" } else { "" }
        ),
        json!({
            "pid": pid,
            "name": info.stat.comm,
            "links": links,
            "descriptors": out.into_items(),
            "count": count,
            "total": total,
            "vanished": vanished,
            "truncated": truncated,
            "stopped": stopped,
        }),
    )
}

/// Listet offene Dateien eines eigenen Prozesses wie `lsof -p`.
#[harw_macros::tool(
    name = "sys.lsof",
    description = "Lists the open file descriptors of one of your own processes like lsof -p: fd number, type (file, dir, socket, pipe, device, anon_inode), target path, access mode, offset and, for sockets, the matching connection; plus the cwd, exe and root links. Use when you need to know which files or sockets a process holds open (for example before killing it or when a file is busy) instead of running lsof. Only processes of the current user are inspected. Returns JSON {descriptors:[...], links, count, total, truncated}. Names and modes only, never contents. Linux only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_lsof(
    _context: &ToolExecutionContext,
    args: LsofArgs,
) -> Result<ToolOutput, ToolsError> {
    let uid = rustix::process::geteuid().as_raw();
    run_blocking(TOOL, move || run(&ProcFs::real(), uid, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, error_of, json_of};
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    fn fake(root: &Path, pid: i32, uid: u32) -> TestResult {
        let base = root.join(pid.to_string());
        fs::create_dir_all(base.join("fd"))?;
        fs::create_dir_all(base.join("fdinfo"))?;
        fs::write(
            base.join("stat"),
            format!("{pid} (app) S 1 1 1 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 100 4096 1 0"),
        )?;
        fs::write(
            base.join("status"),
            format!("Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )?;
        symlink("/work/dir", base.join("cwd"))?;
        symlink("/usr/bin/app", base.join("exe"))?;
        symlink("/", base.join("root"))?;
        for (fd, target, flags, pos) in [
            (0, "/dev/null", "0100000", 0),
            (1, "/var/log/app.log", "0102002", 4096),
            (2, "pipe:[999]", "01", 0),
            (3, "socket:[22222]", "02000002", 0),
            (4, "socket:[55555]", "02000002", 0),
            (5, "anon_inode:[eventfd]", "02", 0),
            (6, "/tmp/gone (deleted)", "0100000", 7),
        ] {
            symlink(target, base.join("fd").join(fd.to_string()))?;
            fs::write(
                base.join("fdinfo").join(fd.to_string()),
                format!("pos:\t{pos}\nflags:\t{flags}\nmnt_id:\t1\n"),
            )?;
        }
        fs::create_dir_all(root.join("net"))?;
        fs::write(
            root.join("net/tcp"),
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   1: 0100007F:D431 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 22222 1 0\n",
        )?;
        fs::write(
            root.join("net/unix"),
            "Num RefCount Protocol Flags Type St Inode Path\n0000: 00000002 00000000 00010000 0001 01 55555 /run/x.sock\n",
        )?;
        Ok(())
    }

    fn lsof(root: &Path, uid: u32, args: Value) -> TestResult<Value> {
        json_of(run(&ProcFs::at(root), uid, &serde_json::from_value(args)?))
    }

    #[test]
    fn lists_descriptors_with_types_modes_and_socket_details() -> TestResult {
        let dir = tempfile::tempdir()?;
        fake(dir.path(), 4242, 1000)?;
        let value = lsof(dir.path(), 1000, json!({"pid": 4242}))?;
        assert_eq!(value["total"], 7);
        let fds = value["descriptors"]
            .as_array()
            .ok_or(TestError::Missing("descriptors"))?;
        let kinds: Vec<&str> = fds.iter().filter_map(|f| f["type"].as_str()).collect();
        assert_eq!(
            kinds,
            vec![
                "device",
                "file",
                "pipe",
                "socket",
                "socket",
                "anon_inode",
                "file"
            ]
        );
        assert_eq!(fds[1]["access"], "rw");
        assert_eq!(fds[1]["offset"], 4096);
        assert_eq!(fds[2]["access"], "w");
        assert_eq!(fds[0]["access"], "r");
        assert_eq!(
            fds[3]["socket"],
            "tcp 127.0.0.1:54321 -> 127.0.0.1:8080 ESTABLISHED"
        );
        assert_eq!(fds[4]["socket"], "unix stream LISTEN /run/x.sock");
        assert_eq!(fds[6]["deleted"], true);
        assert_eq!(value["links"]["cwd"], "/work/dir");
        assert_eq!(value["links"]["exe"], "/usr/bin/app");
        assert_eq!(value["name"], "app");
        Ok(())
    }

    #[test]
    fn foreign_and_missing_processes_are_refused() -> TestResult {
        let dir = tempfile::tempdir()?;
        fake(dir.path(), 4242, 0)?;
        let foreign = run(
            &ProcFs::at(dir.path()),
            1000,
            &serde_json::from_value(json!({"pid": 4242}))?,
        );
        assert!(error_of(foreign)?.contains("only inspects processes of the current user"));
        for bad in [
            json!({"pid": 99999}),
            json!({"pid": 0}),
            json!({"pid": -5}),
            json!({"pid": 4294967296i64}),
        ] {
            error_of(run(
                &ProcFs::at(dir.path()),
                0,
                &serde_json::from_value(bad.clone())?,
            ))
            .map_err(|e| TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        assert!(serde_json::from_value::<LsofArgs>(json!({"pid": 1, "all": true})).is_err());
        assert!(serde_json::from_value::<LsofArgs>(json!({})).is_err());
        Ok(())
    }

    #[test]
    fn limit_truncates_and_unreadable_fd_dir_is_an_error() -> TestResult {
        let dir = tempfile::tempdir()?;
        fake(dir.path(), 4242, 1000)?;
        let value = lsof(dir.path(), 1000, json!({"pid": 4242, "limit": 3}))?;
        assert_eq!(value["count"], 3);
        assert_eq!(value["truncated"], true);
        fs::remove_dir_all(dir.path().join("4242/fd"))?;
        error_of(run(
            &ProcFs::at(dir.path()),
            1000,
            &serde_json::from_value(json!({"pid": 4242}))?,
        ))?;
        Ok(())
    }

    #[test]
    fn parses_fdinfo_and_bracketed_targets() -> TestResult {
        assert_eq!(
            parse_fdinfo("pos:\t12\nflags:\t0100002\n"),
            (Some(0o100002), Some(12))
        );
        assert_eq!(parse_fdinfo("junk"), (None, None));
        assert_eq!(bracketed("socket:[123]"), Some(("socket", 123)));
        assert_eq!(bracketed("socket:[x]"), None);
        assert_eq!(bracketed("/path"), None);
        Ok(())
    }

    #[test]
    fn real_own_process_lists_standard_descriptors() -> TestResult {
        let me = i64::from(std::process::id());
        let uid = rustix::process::geteuid().as_raw();
        let value = lsof(Path::new("/proc"), uid, json!({"pid": me, "limit": 50}))?;
        assert!(value["total"].as_u64().is_some_and(|t| t >= 3), "{value}");
        assert!(value["links"]["exe"].is_string());
        Ok(())
    }
}
