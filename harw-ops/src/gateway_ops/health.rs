//! `gateway.health` — one read that answers "how is the gateway
//! communication doing?" from the local harw home (coordinator addendum to
//! R18 F1).
//!
//! Field evidence: a live UIA had to dig by hand (`process.list`,
//! `ls -la ~/.harw/*.sock; ps aux | grep harw`, `ls -lat ~/.harw/logs`,
//! `tail -c 4000 ~/.harw/logs/tui.log`) to find a running gateway, a live
//! `sentinel.sock`, a stale `web.sock` that was a regular file and a 9.6 MB
//! `tui.log`. This operation reports exactly that in one call:
//!
//! - **daemon**: the `harw gateway` process (process table scan on Linux,
//!   plus the detached backend's PID file `<home>/run/harw-gateway.pid`);
//! - **session gateway**: the R18 `GatewayPort` status when this runtime is
//!   connected to one (otherwise "not connected");
//! - **sockets**: every `*.sock` directly under the home and `<home>/run`,
//!   the well-known `web.sock`/`sentinel.sock`, and the `[infrastructure]`
//!   sockets, each with its kind (socket / regular file / missing / …) and
//!   whether it accepts a connection;
//! - **logs**: the harw log files with their sizes;
//! - **problems**: a compact list of what looks wrong.
//!
//! # Boundaries
//! Reads file metadata and `/proc/<pid>/cmdline` only; connects to a socket
//! without sending a byte and closes it immediately. Starts no process and
//! changes nothing. Paths and names are shown through
//! [`super::sanitize`]; no file content is read (that is `gateway.logs`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use harw_config::{ChannelToml, ResolvedConfig};
use harw_home::ResolvedHomeContext;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_protocol::GatewayPort;
use serde_json::{Value, json};

use super::logs::{human_size, list_log_files};
use super::{GatewayReadArgs, call, render_status, sanitize};

/// Upper bound for one socket connection probe.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// A log file at least this large is reported as a problem.
pub const LARGE_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Most sockets listed (a home full of sockets is itself a finding).
const MAX_SOCKETS: usize = 32;

/// Most log files listed.
const MAX_LOG_FILES_SHOWN: usize = 10;

/// Longest path or name shown.
const MAX_PATH_TEXT: usize = 256;

/// PID file of the detached gateway backend (`harw-cli/src/lifecycle.rs`).
const GATEWAY_PID_FILE: &str = "harw-gateway.pid";

/// `harw gateway <action>` lifecycle commands; a process running one of
/// them is a short-lived CLI call, not the daemon.
const GATEWAY_LIFECYCLE_ACTIONS: [&str; 7] = [
    "install", "start", "stop", "restart", "enable", "disable", "status",
];

/// Sockets a harw home usually carries, with their owner.
const KNOWN_SOCKETS: [(&str, &str); 2] = [
    ("web.sock", "web UI control socket (`harw web`)"),
    ("sentinel.sock", "harw-sentinel"),
];

/// Local health of the gateway: daemon process, session gateway status,
/// sockets under the harw home, log sizes and a list of problems.
///
/// # Errors
/// [`OpError::NotAvailable`] without the bound harw home. An unreachable
/// socket, a missing process table or a failing session gateway are
/// findings in the output, never an error of the operation.
#[operation(
    name = "gateway.health",
    summary = "Diagnose der Gateway-Kommunikation in einem Aufruf: läuft der Gateway-Daemon (PID), welche Sockets unter dem harw-Home existieren (Socket, veraltete normale Datei, fehlt) und ob sie Verbindungen annehmen, Größe der Logdateien und eine kurze Problemliste. Use this instead of ps/ls/tail through the shell. Use this instead of running `harw gateway …`/`harw channel …` through `shell.exec`; the `harw` binary is not available inside the sandbox.",
    domain = "execution",
    permission = "maintainer",
    command(path = "/gateway-health", visibility = "tui_only", busy = "immediate"),
    model_tool(readonly, approval = "none"),
    // Liest nur Metadaten, Prozesstabelle und einen Verbindungsversuch je
    // Socket (ohne Nutzdaten); keine Mutation.
    web(path = "/api/gateway/health", method = "get", approval = "none")
)]
async fn gateway_health(ctx: &OpContext, _args: GatewayReadArgs) -> Result<OpOutput, OpError> {
    let home = home_context(ctx)?.home.clone();
    let config = ctx.service::<Arc<ResolvedConfig>>().cloned();
    let port = ctx.service::<Arc<dyn GatewayPort>>().cloned();

    let mut problems = Vec::new();
    let daemon = probe_daemon(&home);
    daemon_problems(&daemon, config.as_deref(), &mut problems);

    let session_gateway = match &port {
        None => SessionGateway::NotConnected,
        Some(port) => {
            match call("gateway.status", false, port.status()).await {
                Ok(status) => {
                    if status.draining {
                        problems.push(
                            "the session gateway is draining: new turns and tool calls are refused"
                                .to_owned(),
                        );
                    }
                    if !status.sandbox_available {
                        problems.push("the session gateway has no sandbox: every gateway tool call is refused".to_owned());
                    }
                    SessionGateway::Status(render_status("session gateway:", &status))
                }
                Err(error) => {
                    let text = sanitize(&error.to_string(), MAX_PATH_TEXT);
                    problems.push(format!("the session gateway does not answer: {text}"));
                    SessionGateway::Failed(text)
                }
            }
        }
    };

    let mut sockets = Vec::new();
    for candidate in socket_candidates(&home, config.as_deref()) {
        sockets.push(probe_socket(candidate).await);
    }
    for socket in &sockets {
        socket_problems(socket, &mut problems);
    }

    let logs_dir = harw_home::paths::logs_dir(&home);
    let mut logs = list_log_files(&logs_dir);
    logs.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    logs.truncate(MAX_LOG_FILES_SHOWN);
    for log in &logs {
        if log.size >= LARGE_LOG_BYTES {
            problems.push(format!(
                "{} is {} — see gateway.logs for its repeated entries",
                sanitize(&log.name, MAX_PATH_TEXT),
                human_size(log.size)
            ));
        }
    }

    let home_text = sanitize(&home.display().to_string(), MAX_PATH_TEXT);
    let mut text = format!("Gateway health (home {home_text}):");
    text.push_str(&format!("\ndaemon           {}", daemon.summary()));
    text.push_str(&format!("\npid file         {}", daemon.pid_file.summary()));
    match &session_gateway {
        SessionGateway::NotConnected => {
            text.push_str("\nsession gateway  not connected (this runtime is not attached to one)");
        }
        SessionGateway::Failed(error) => {
            text.push_str(&format!("\nsession gateway  not answering ({error})"));
        }
        SessionGateway::Status((status_text, _)) => {
            for line in status_text.lines() {
                text.push('\n');
                text.push_str(line);
            }
        }
    }
    text.push_str("\nsockets:");
    if sockets.is_empty() {
        text.push_str(" none found");
    }
    for socket in &sockets {
        text.push_str(&format!("\n  {}", socket.line()));
    }
    text.push_str("\nlogs:");
    if logs.is_empty() {
        text.push_str(" none");
    }
    for log in &logs {
        text.push_str(&format!(
            "\n  {:<24} {}",
            sanitize(&log.name, MAX_PATH_TEXT),
            human_size(log.size)
        ));
    }
    if problems.is_empty() {
        text.push_str("\nproblems: none");
    } else {
        text.push_str("\nproblems:");
        for problem in &problems {
            text.push_str(&format!("\n  - {problem}"));
        }
    }

    let data = json!({
        "home": home_text,
        "daemon": daemon.data(),
        "session_gateway": match &session_gateway {
            SessionGateway::NotConnected => json!({ "connected": false }),
            SessionGateway::Failed(error) => json!({ "connected": true, "error": error }),
            SessionGateway::Status((_, status)) => json!({ "connected": true, "status": status }),
        },
        "sockets": sockets.iter().map(SocketReport::data).collect::<Vec<_>>(),
        "logs": logs
            .iter()
            .map(|log| json!({ "file": sanitize(&log.name, MAX_PATH_TEXT), "size_bytes": log.size }))
            .collect::<Vec<_>>(),
        "problems": problems,
    });
    Ok(OpOutput {
        text,
        data: Some(data),
    })
}

fn home_context(ctx: &OpContext) -> Result<&ResolvedHomeContext, OpError> {
    ctx.service::<Arc<ResolvedHomeContext>>()
        .map(AsRef::as_ref)
        .ok_or_else(|| {
            OpError::NotAvailable("the harw home is not bound in this context".to_owned())
        })
}

enum SessionGateway {
    NotConnected,
    Failed(String),
    Status((String, Value)),
}

// ── Daemon ───────────────────────────────────────────────────────────────────

/// State of the detached backend's PID file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PidFileState {
    /// No PID file (normal under systemd/launchd).
    Absent,
    /// Present but not a positive integer.
    Invalid,
    /// The process exists.
    Alive(u32),
    /// The process is gone: a stale PID file.
    Stale(u32),
    /// Liveness cannot be checked on this platform.
    Unchecked(u32),
}

impl PidFileState {
    fn summary(self) -> String {
        match self {
            Self::Absent => "none (normal under systemd/launchd)".to_owned(),
            Self::Invalid => "unreadable (not a pid)".to_owned(),
            Self::Alive(pid) => format!("pid {pid} (alive)"),
            Self::Stale(pid) => format!("pid {pid} (stale: the process is gone)"),
            Self::Unchecked(pid) => format!("pid {pid} (liveness not checkable here)"),
        }
    }

    fn data(self) -> Value {
        match self {
            Self::Absent => json!({ "state": "absent" }),
            Self::Invalid => json!({ "state": "invalid" }),
            Self::Alive(pid) => json!({ "state": "alive", "pid": pid }),
            Self::Stale(pid) => json!({ "state": "stale", "pid": pid }),
            Self::Unchecked(pid) => json!({ "state": "unchecked", "pid": pid }),
        }
    }
}

/// What the process table and the PID file say about the gateway daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DaemonReport {
    /// PIDs of `harw gateway` daemon processes; `None` when the process
    /// table cannot be read (non-Linux, restricted `/proc`).
    pub(crate) processes: Option<Vec<u32>>,
    pub(crate) pid_file: PidFileState,
}

impl DaemonReport {
    fn running(&self) -> Option<bool> {
        match (&self.processes, self.pid_file) {
            (Some(pids), _) if !pids.is_empty() => Some(true),
            (_, PidFileState::Alive(_)) => Some(true),
            (Some(_), _) => Some(false),
            (None, _) => None,
        }
    }

    fn summary(&self) -> String {
        match (self.running(), &self.processes) {
            (Some(true), Some(pids)) if !pids.is_empty() => {
                let pids: Vec<String> = pids.iter().map(u32::to_string).collect();
                format!("running (pid {})", pids.join(", "))
            }
            (Some(true), _) => "running (per pid file)".to_owned(),
            (Some(false), _) => "not running".to_owned(),
            (None, _) => "unknown (process table not readable here)".to_owned(),
        }
    }

    fn data(&self) -> Value {
        json!({
            "running": self.running(),
            "pids": self.processes,
            "pid_file": self.pid_file.data(),
        })
    }
}

fn probe_daemon(home: &Path) -> DaemonReport {
    let pid_path = home.join("run").join(GATEWAY_PID_FILE);
    let pid_file = match std::fs::read_to_string(&pid_path) {
        Err(_) => PidFileState::Absent,
        Ok(raw) => match raw.trim().parse::<u32>().ok().filter(|pid| *pid > 0) {
            None => PidFileState::Invalid,
            Some(pid) => match pid_alive(pid) {
                Some(true) => PidFileState::Alive(pid),
                Some(false) => PidFileState::Stale(pid),
                None => PidFileState::Unchecked(pid),
            },
        },
    };
    DaemonReport {
        processes: scan_gateway_processes(),
        pid_file,
    }
}

/// `Some(alive)` where `/proc` exists, `None` elsewhere.
fn pid_alive(pid: u32) -> Option<bool> {
    let proc_root = Path::new("/proc");
    if !proc_root.join("self").exists() {
        return None;
    }
    Some(proc_root.join(pid.to_string()).exists())
}

/// PIDs of running `harw gateway` daemons, or `None` without a readable
/// process table. The calling process itself is never listed.
fn scan_gateway_processes() -> Option<Vec<u32>> {
    let entries = std::fs::read_dir("/proc").ok()?;
    let own = std::process::id();
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if is_gateway_daemon_cmdline(&cmdline) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    Some(pids)
}

/// Whether a NUL-separated `/proc/<pid>/cmdline` is a `harw gateway` daemon:
/// the program is `harw…`, one argument is exactly `gateway`, and it is not
/// followed by a lifecycle action (`harw gateway status` is a CLI call, not
/// the daemon).
///
/// # Example
/// ```rust
/// use harw_ops::gateway_ops::health::is_gateway_daemon_cmdline;
/// assert!(is_gateway_daemon_cmdline(b"/usr/bin/harw\0gateway\0"));
/// assert!(!is_gateway_daemon_cmdline(b"harw\0gateway\0status\0"));
/// assert!(!is_gateway_daemon_cmdline(b"grep\0harw\0gateway\0"));
/// ```
#[must_use]
pub fn is_gateway_daemon_cmdline(raw: &[u8]) -> bool {
    let args: Vec<&[u8]> = raw
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .collect();
    let Some(&path) = args.first() else {
        return false;
    };
    let program = path.rsplit(|byte| *byte == b'/').next().unwrap_or(path);
    if !program.starts_with(b"harw") {
        return false;
    }
    let Some(position) = args.iter().skip(1).position(|arg| *arg == b"gateway") else {
        return false;
    };
    match args.get(position + 2) {
        Some(next) => !GATEWAY_LIFECYCLE_ACTIONS
            .iter()
            .any(|action| action.as_bytes() == *next),
        None => true,
    }
}

fn daemon_problems(
    daemon: &DaemonReport,
    config: Option<&ResolvedConfig>,
    problems: &mut Vec<String>,
) {
    match daemon.pid_file {
        PidFileState::Stale(pid) => problems.push(format!(
            "stale pid file run/{GATEWAY_PID_FILE}: process {pid} is gone"
        )),
        PidFileState::Invalid => {
            problems.push(format!(
                "pid file run/{GATEWAY_PID_FILE} does not hold a pid"
            ));
        }
        _ => {}
    }
    if daemon.running() == Some(false) {
        let enabled = config.map_or(0, |config| {
            config
                .channels
                .values()
                .filter(|channel| match channel {
                    ChannelToml::Telegram(telegram) => telegram.enabled,
                })
                .count()
        });
        if enabled > 0 {
            problems.push(format!(
                "no gateway daemon is running, but {enabled} enabled channel(s) depend on it — \
                 they receive no messages"
            ));
        }
    }
}

// ── Sockets ──────────────────────────────────────────────────────────────────

/// Kind of a filesystem entry at a socket path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    Socket,
    RegularFile,
    Directory,
    Symlink,
    Other,
    Missing,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Socket => "socket",
            Self::RegularFile => "regular file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Other => "other file type",
            Self::Missing => "missing",
        }
    }
}

/// The entry at `path` without following a symlink.
pub(crate) fn entry_kind(path: &Path) -> EntryKind {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return EntryKind::Missing;
    };
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_file() {
        EntryKind::RegularFile
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if is_socket(&file_type) {
        EntryKind::Socket
    } else {
        EntryKind::Other
    }
}

#[cfg(unix)]
fn is_socket(file_type: &std::fs::FileType) -> bool {
    use std::os::unix::fs::FileTypeExt;
    file_type.is_socket()
}

#[cfg(not(unix))]
fn is_socket(_file_type: &std::fs::FileType) -> bool {
    false
}

/// A socket path worth reporting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SocketCandidate {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    /// Who owns it (`web UI …`, `[infrastructure] auth_socket`, `found in home`).
    pub(crate) origin: String,
    /// Configured explicitly: missing is a problem.
    pub(crate) expected: bool,
}

/// Well-known sockets, every `*.sock` under `home` and `home/run`, and the
/// configured `[infrastructure]` sockets, without duplicates, bounded.
pub(crate) fn socket_candidates(
    home: &Path,
    config: Option<&ResolvedConfig>,
) -> Vec<SocketCandidate> {
    let mut candidates: Vec<SocketCandidate> = Vec::new();
    let mut push = |candidate: SocketCandidate| {
        if candidates.len() < MAX_SOCKETS
            && !candidates.iter().any(|known| known.path == candidate.path)
        {
            candidates.push(candidate);
        }
    };
    for (name, origin) in KNOWN_SOCKETS {
        push(SocketCandidate {
            name: name.to_owned(),
            path: home.join(name),
            origin: origin.to_owned(),
            expected: false,
        });
    }
    if let Some(section) = config.and_then(|config| config.infrastructure.as_ref()) {
        for (key, path) in [
            ("auth_socket", &section.auth_socket),
            ("network_socket", &section.network_socket),
            ("security_socket", &section.security_socket),
        ] {
            if let Some(path) = path {
                push(SocketCandidate {
                    name: file_name(path),
                    path: path.clone(),
                    origin: format!("[infrastructure] {key}"),
                    expected: true,
                });
            }
        }
    }
    for dir in [home.to_path_buf(), home.join("run")] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "sock"))
            .collect();
        found.sort();
        for path in found {
            push(SocketCandidate {
                name: file_name(&path),
                path,
                origin: "found in the harw home".to_owned(),
                expected: false,
            });
        }
    }
    candidates
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// One probed socket path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SocketReport {
    pub(crate) candidate: SocketCandidate,
    pub(crate) kind: EntryKind,
    /// `Some` only for sockets: whether a connection was accepted.
    pub(crate) accepts: Option<bool>,
}

impl SocketReport {
    fn line(&self) -> String {
        let state = match (self.kind, self.accepts) {
            (EntryKind::Socket, Some(true)) => "socket, accepting connections".to_owned(),
            (EntryKind::Socket, _) => "socket, NOT accepting connections (stale)".to_owned(),
            (EntryKind::RegularFile, _) => "regular file, not a socket (stale)".to_owned(),
            (kind, _) => kind.as_str().to_owned(),
        };
        format!(
            "{:<16} {state} — {} ({})",
            sanitize(&self.candidate.name, MAX_PATH_TEXT),
            sanitize(&self.candidate.path.display().to_string(), MAX_PATH_TEXT),
            sanitize(&self.candidate.origin, MAX_PATH_TEXT),
        )
    }

    fn data(&self) -> Value {
        json!({
            "name": sanitize(&self.candidate.name, MAX_PATH_TEXT),
            "path": sanitize(&self.candidate.path.display().to_string(), MAX_PATH_TEXT),
            "origin": sanitize(&self.candidate.origin, MAX_PATH_TEXT),
            "kind": self.kind.as_str(),
            "accepts": self.accepts,
        })
    }
}

pub(crate) async fn probe_socket(candidate: SocketCandidate) -> SocketReport {
    let kind = entry_kind(&candidate.path);
    let accepts = match kind {
        EntryKind::Socket => Some(accepts_connection(&candidate.path).await),
        _ => None,
    };
    SocketReport {
        candidate,
        kind,
        accepts,
    }
}

/// Connects without sending anything and closes immediately.
#[cfg(unix)]
async fn accepts_connection(path: &Path) -> bool {
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, tokio::net::UnixStream::connect(path)).await,
        Ok(Ok(_))
    )
}

#[cfg(not(unix))]
async fn accepts_connection(_path: &Path) -> bool {
    false
}

pub(crate) fn socket_problems(socket: &SocketReport, problems: &mut Vec<String>) {
    let name = sanitize(&socket.candidate.name, MAX_PATH_TEXT);
    let path = sanitize(&socket.candidate.path.display().to_string(), MAX_PATH_TEXT);
    match (socket.kind, socket.accepts) {
        (EntryKind::RegularFile, _) => problems.push(format!(
            "{name} is a regular file instead of a socket — a stale leftover; its service \
             cannot bind there until it is removed ({path})"
        )),
        (EntryKind::Socket, Some(false)) => problems.push(format!(
            "{name} is a socket but nothing accepts connections — stale, its service is not \
             running ({path})"
        )),
        (EntryKind::Directory | EntryKind::Other, _) => problems.push(format!(
            "{name} is a {} instead of a socket ({path})",
            socket.kind.as_str()
        )),
        (EntryKind::Missing, _) if socket.candidate.expected => problems.push(format!(
            "{name} is configured ({}) but missing — its service is not running ({path})",
            sanitize(&socket.candidate.origin, MAX_PATH_TEXT)
        )),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DaemonReport, EntryKind, PidFileState, entry_kind, is_gateway_daemon_cmdline, probe_socket,
        socket_candidates, socket_problems,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn cmdline_detection_accepts_the_daemon_only() {
        assert!(is_gateway_daemon_cmdline(
            b"/home/u/.cargo/bin/harw\0gateway\0"
        ));
        assert!(is_gateway_daemon_cmdline(
            b"harw\0--home\0/srv/harw\0gateway\0--otlp\0"
        ));
        assert!(!is_gateway_daemon_cmdline(b"harw\0gateway\0restart\0"));
        assert!(!is_gateway_daemon_cmdline(b"harw\0chat\0"));
        assert!(!is_gateway_daemon_cmdline(b"/bin/grep\0harw\0gateway\0"));
        assert!(!is_gateway_daemon_cmdline(b""));
    }

    #[test]
    fn daemon_summary_distinguishes_running_stopped_and_unknown() {
        let running = DaemonReport {
            processes: Some(vec![1399]),
            pid_file: PidFileState::Absent,
        };
        assert!(running.summary().contains("1399"));
        let stopped = DaemonReport {
            processes: Some(Vec::new()),
            pid_file: PidFileState::Stale(42),
        };
        assert_eq!(stopped.summary(), "not running");
        let unknown = DaemonReport {
            processes: None,
            pid_file: PidFileState::Absent,
        };
        assert!(unknown.summary().starts_with("unknown"));
    }

    #[tokio::test]
    async fn stale_regular_file_socket_is_reported_as_a_problem() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(home.path().join("web.sock"), b"").map_err(ctx("write web.sock"))?;
        let candidates = socket_candidates(home.path(), None);
        let web = candidates
            .into_iter()
            .find(|candidate| candidate.name == "web.sock")
            .ok_or(TestError::Missing("web.sock candidate"))?;
        let report = probe_socket(web).await;
        assert_eq!(report.kind, EntryKind::RegularFile);
        let mut problems = Vec::new();
        socket_problems(&report, &mut problems);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("regular file instead of a socket"),
            "{problems:?}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_socket_accepts_and_missing_socket_is_quiet() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = home.path().join("sentinel.sock");
        let _listener =
            std::os::unix::net::UnixListener::bind(&path).map_err(ctx("bind sentinel.sock"))?;
        assert_eq!(entry_kind(&path), EntryKind::Socket);
        let candidates = socket_candidates(home.path(), None);
        let mut problems = Vec::new();
        for candidate in candidates {
            let report = probe_socket(candidate).await;
            if report.candidate.name == "sentinel.sock" {
                assert_eq!(report.accepts, Some(true));
            }
            socket_problems(&report, &mut problems);
        }
        // `web.sock` is absent but not configured: no problem.
        assert!(problems.is_empty(), "{problems:?}");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_without_listener_is_stale() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = home.path().join("old.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).map_err(ctx("bind old.sock"))?);
        let candidate = socket_candidates(home.path(), None)
            .into_iter()
            .find(|candidate| candidate.name == "old.sock")
            .ok_or(TestError::Missing("old.sock candidate"))?;
        let report = probe_socket(candidate).await;
        assert_eq!(report.kind, EntryKind::Socket);
        assert_eq!(report.accepts, Some(false));
        let mut problems = Vec::new();
        socket_problems(&report, &mut problems);
        assert!(
            problems.iter().any(|p| p.contains("nothing accepts")),
            "{problems:?}"
        );
        Ok(())
    }
}
