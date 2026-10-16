//! `sys.ss` — lesende Socket-Liste wie `ss`/`netstat` aus `/proc/net/*`.
//!
//! TCP und UDP (IPv4/IPv6) sowie Unix-Sockets. Ohne `-p`: die Zuordnung
//! Socket → Prozess braucht erhöhte Rechte und wird nicht angeboten (die
//! Inode-Nummer steht im Ergebnis und kann mit `sys.lsof` für eigene
//! Prozesse abgeglichen werden).
//!
//! Standard sind **verbundene** Sockets; `listening` (`-l`) zeigt nur
//! lauschende, `all` (`-a`) beides. Ohne Protokollwahl gelten TCP und UDP.

use crate::procfs::ProcFs;
use harw_tool_fsread::blocking::run_blocking;
use harw_tool_fsread::budget::{Collector, fail, flag, limit_or, ok};
use harw_tool_fsread::users::UserDb;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::{Ipv4Addr, Ipv6Addr};

/// Name des Werkzeugs.
pub const TOOL: &str = "sys.ss";

/// Standard-Limit.
pub const DEFAULT_LIMIT: usize = 200;

/// Hartes Limit.
pub const HARD_LIMIT: usize = 2_000;

/// Argumente für `sys.ss`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct SsArgs {
    /// -t / --tcp: list TCP sockets.
    #[serde(default)]
    pub tcp: Option<bool>,
    /// -u / --udp: list UDP sockets.
    #[serde(default)]
    pub udp: Option<bool>,
    /// -x / --unix: list Unix domain sockets.
    #[serde(default)]
    pub unix: Option<bool>,
    /// -l / --listening: only listening sockets (TCP LISTEN, UDP unconnected, Unix listening).
    #[serde(default)]
    pub listening: Option<bool>,
    /// -a / --all: listening and connected sockets.
    #[serde(default)]
    pub all: Option<bool>,
    /// -4 / --ipv4: only IPv4 sockets.
    #[serde(default)]
    pub ipv4: Option<bool>,
    /// -6 / --ipv6: only IPv6 sockets.
    #[serde(default)]
    pub ipv6: Option<bool>,
    /// Only sockets whose local or remote port equals this (1-65535).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_u64"
    )]
    pub port: Option<u64>,
    /// Maximum number of sockets (default 200, hard 2000).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub limit: Option<usize>,
}

/// Ein Internet-Socket aus `/proc/net/{tcp,udp}[6]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InetSocket {
    /// `tcp`, `tcp6`, `udp`, `udp6`.
    pub proto: &'static str,
    /// Lokale Adresse (Text).
    pub local_addr: String,
    /// Lokaler Port.
    pub local_port: u16,
    /// Entfernte Adresse.
    pub remote_addr: String,
    /// Entfernter Port.
    pub remote_port: u16,
    /// Zustandsname.
    pub state: &'static str,
    /// Sende-Warteschlange.
    pub tx_queue: u64,
    /// Empfangs-Warteschlange.
    pub rx_queue: u64,
    /// Besitzer-UID.
    pub uid: u32,
    /// Inode.
    pub inode: u64,
}

/// Ein Unix-Socket aus `/proc/net/unix`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnixSocket {
    /// `stream`, `dgram`, `seqpacket`, `unknown`.
    pub kind: &'static str,
    /// Zustandsname.
    pub state: &'static str,
    /// Pfad (leer, anonym; `@…` abstrakt).
    pub path: String,
    /// Inode.
    pub inode: u64,
    /// Lauscht der Socket?
    pub listening: bool,
}

/// Zustandsname eines TCP-Sockets.
fn tcp_state(code: &str) -> &'static str {
    match code {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" => "LISTEN",
        "0B" => "CLOSING",
        "0C" => "NEW_SYN_RECV",
        _ => "UNKNOWN",
    }
}

/// Zustandsname eines UDP-Sockets (`ss`-Schreibweise).
fn udp_state(code: &str) -> &'static str {
    match code {
        "01" => "ESTAB",
        "07" => "UNCONN",
        _ => "UNKNOWN",
    }
}

/// Wortweise Hex-Adresse → Bytes (die Wörter stehen in Host-Reihenfolge).
fn words_to_bytes(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 8 != 0 || hex.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(8) {
        let word = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
        bytes.extend_from_slice(&if cfg!(target_endian = "little") {
            word.to_le_bytes()
        } else {
            word.to_be_bytes()
        });
    }
    Some(bytes)
}

/// Parst `ADDR:PORT` (Hex) einer `/proc/net`-Zeile.
#[must_use]
pub fn parse_endpoint(field: &str) -> Option<(String, u16)> {
    let (addr, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let bytes = words_to_bytes(addr)?;
    let text = match bytes.len() {
        4 => Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string(),
        16 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&bytes);
            Ipv6Addr::from(octets).to_string()
        }
        _ => return None,
    };
    Some((text, port))
}

/// Parst `/proc/net/tcp`, `tcp6`, `udp`, `udp6`; kaputte Zeilen werden übersprungen.
#[must_use]
pub fn parse_inet(text: &str, proto: &'static str) -> Vec<InetSocket> {
    let udp = proto.starts_with("udp");
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 {
            continue;
        }
        let (Some((local_addr, local_port)), Some((remote_addr, remote_port))) =
            (parse_endpoint(f[1]), parse_endpoint(f[2]))
        else {
            continue;
        };
        let (tx, rx) = f[4].split_once(':').unwrap_or(("0", "0"));
        out.push(InetSocket {
            proto,
            local_addr,
            local_port,
            remote_addr,
            remote_port,
            state: if udp {
                udp_state(f[3])
            } else {
                tcp_state(f[3])
            },
            tx_queue: u64::from_str_radix(tx, 16).unwrap_or(0),
            rx_queue: u64::from_str_radix(rx, 16).unwrap_or(0),
            uid: f[7].parse().unwrap_or(0),
            inode: f[9].parse().unwrap_or(0),
        });
    }
    out
}

/// Parst `/proc/net/unix`.
#[must_use]
pub fn parse_unix(text: &str) -> Vec<UnixSocket> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 7 {
            continue;
        }
        let flags = u32::from_str_radix(f[3], 16).unwrap_or(0);
        let kind = match f[4] {
            "0001" => "stream",
            "0002" => "dgram",
            "0005" => "seqpacket",
            _ => "unknown",
        };
        let listening = flags & 0x0001_0000 != 0;
        let state = if listening {
            "LISTEN"
        } else {
            match f[5] {
                "01" => "UNCONN",
                "02" => "CONNECTING",
                "03" => "ESTAB",
                "04" => "DISCONNECTING",
                _ => "UNKNOWN",
            }
        };
        out.push(UnixSocket {
            kind,
            state,
            path: f.get(7).map(|p| (*p).to_owned()).unwrap_or_default(),
            inode: f[6].parse().unwrap_or(0),
            listening,
        });
    }
    out
}

/// Alle Internet-Sockets eines `/proc`-Baums.
#[must_use]
pub fn read_inet(procfs: &ProcFs) -> Vec<InetSocket> {
    let mut all = Vec::new();
    for (file, proto) in [
        ("net/tcp", "tcp"),
        ("net/tcp6", "tcp6"),
        ("net/udp", "udp"),
        ("net/udp6", "udp6"),
    ] {
        if let Some(text) = procfs.read_file(file) {
            all.extend(parse_inet(&text, proto));
        }
    }
    all
}

/// Alle Unix-Sockets eines `/proc`-Baums.
#[must_use]
pub fn read_unix(procfs: &ProcFs) -> Vec<UnixSocket> {
    procfs
        .read_file("net/unix")
        .map(|t| parse_unix(&t))
        .unwrap_or_default()
}

fn inet_listening(socket: &InetSocket) -> bool {
    matches!(socket.state, "LISTEN" | "UNCONN")
}

fn inet_json(socket: &InetSocket, db: &UserDb) -> Value {
    json!({
        "proto": socket.proto,
        "state": socket.state,
        "local_addr": socket.local_addr,
        "local_port": socket.local_port,
        "remote_addr": socket.remote_addr,
        "remote_port": socket.remote_port,
        "rx_queue": socket.rx_queue,
        "tx_queue": socket.tx_queue,
        "uid": socket.uid,
        "user": db.user_or_id(socket.uid),
        "inode": socket.inode,
    })
}

/// Führt `sys.ss` gegen `procfs` aus.
#[must_use]
pub fn run(procfs: &ProcFs, args: &SsArgs) -> ToolOutput {
    let (tcp, udp, unix) = (flag(args.tcp), flag(args.udp), flag(args.unix));
    let (listening, all) = (flag(args.listening), flag(args.all));
    if listening && all {
        return fail(TOOL, "listening and all are mutually exclusive");
    }
    if flag(args.ipv4) && flag(args.ipv6) {
        return fail(TOOL, "ipv4 and ipv6 are mutually exclusive");
    }
    if let Some(port) = args.port {
        if !(1..=65_535).contains(&port) {
            return fail(
                TOOL,
                format!("port must be between 1 and 65535, got {port}"),
            );
        }
    }
    let (want_tcp, want_udp) = if !tcp && !udp && !unix {
        (true, true)
    } else {
        (tcp, udp)
    };
    let limit = limit_or(args.limit, DEFAULT_LIMIT, HARD_LIMIT);
    let db = UserDb::load();
    let mut out = Collector::new(limit);
    let mut matched = 0usize;

    let mut inet = read_inet(procfs);
    inet.retain(|s| {
        let family_ok = if flag(args.ipv4) {
            !s.proto.ends_with('6')
        } else if flag(args.ipv6) {
            s.proto.ends_with('6')
        } else {
            true
        };
        let proto_ok = if s.proto.starts_with("tcp") {
            want_tcp
        } else {
            want_udp
        };
        let state_ok = all || (listening == inet_listening(s));
        let port_ok = args
            .port
            .is_none_or(|p| u64::from(s.local_port) == p || u64::from(s.remote_port) == p);
        family_ok && proto_ok && state_ok && port_ok
    });
    inet.sort_by(|a, b| {
        (
            a.proto,
            a.local_port,
            &a.local_addr,
            a.remote_port,
            &a.remote_addr,
            a.inode,
        )
            .cmp(&(
                b.proto,
                b.local_port,
                &b.local_addr,
                b.remote_port,
                &b.remote_addr,
                b.inode,
            ))
    });
    for socket in &inet {
        matched += 1;
        if !out.truncated() {
            out.push(inet_json(socket, &db));
        }
    }
    if unix && args.port.is_none() && !flag(args.ipv4) && !flag(args.ipv6) {
        let mut sockets = read_unix(procfs);
        sockets.retain(|s| all || listening == s.listening);
        sockets.sort_by(|a, b| (&a.path, a.inode).cmp(&(&b.path, b.inode)));
        for socket in &sockets {
            matched += 1;
            if !out.truncated() {
                out.push(json!({
                    "proto": "unix",
                    "type": socket.kind,
                    "state": socket.state,
                    "path": socket.path,
                    "inode": socket.inode,
                }));
            }
        }
    }
    let truncated = out.truncated();
    let stopped = out.stop_reason();
    let count = out.len();
    ok(
        TOOL,
        format!(
            "{count} of {matched} sockets{}",
            if truncated { " (truncated)" } else { "" }
        ),
        json!({
            "sockets": out.into_items(),
            "count": count,
            "matched": matched,
            "truncated": truncated,
            "stopped": stopped,
        }),
    )
}

/// Listet Sockets wie `ss`.
#[harw_macros::tool(
    name = "sys.ss",
    description = "Lists network sockets like ss/netstat from /proc/net: -t tcp, -u udp, -x unix, -l listening only, -a all, -4/-6 address family, port filter (local or remote). Use when you need to know which ports are listening or which connections exist instead of running ss or netstat. Returns JSON {sockets:[{proto,state,local_addr,local_port,remote_addr,remote_port,uid,user,inode}], count, matched, truncated}. No process names (-p needs root); read-only, Linux only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_ss(_context: &ToolExecutionContext, args: SsArgs) -> Result<ToolOutput, ToolsError> {
    run_blocking(TOOL, move || run(&ProcFs::real(), &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, error_of, json_of};
    use std::fs;

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 11111 1 0000000000000000 100 0 0 10 0\n\
   1: 0100007F:D431 0100007F:1F90 01 00000002:00000003 00:00000000 00000000  1000        0 22222 1 0000000000000000 20 4 30 10 -1\n\
   2: 0A00000A:0016 C0A80001:C350 06 00000000:00000000 03:000000F9 00000000     0        0 0 3 0000000000000000\n\
broken line\n";

    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 00000000000000000000000001000000:0050 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 33333 1 0000000000000000 100 0 0 10 0\n";

    const UDP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n\
  10: 00000000:0044 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 44444 2 0000000000000000 0\n";

    const UNIX: &str = "Num       RefCount Protocol Flags    Type St Inode Path\n\
0000000000000000: 00000002 00000000 00010000 0001 01 55555 /run/app.sock\n\
0000000000000000: 00000003 00000000 00000000 0001 03 66666\n\
0000000000000000: 00000002 00000000 00000000 0002 01 77777 @abstract\n";

    #[test]
    fn parses_hex_endpoints() -> TestResult {
        assert_eq!(
            parse_endpoint("0100007F:1F90"),
            Some(("127.0.0.1".to_owned(), 8080))
        );
        assert_eq!(
            parse_endpoint("0A00000A:0016"),
            Some(("10.0.0.10".to_owned(), 22))
        );
        assert_eq!(
            parse_endpoint("00000000000000000000000001000000:0050"),
            Some(("::1".to_owned(), 80))
        );
        for bad in [
            "",
            "0100007F",
            "0100007F:ZZ",
            "0100:50",
            "XYZ:50",
            "0100007F:10000",
        ] {
            assert_eq!(parse_endpoint(bad), None, "{bad}");
        }
        Ok(())
    }

    #[test]
    fn parses_tables_and_skips_garbage() -> TestResult {
        let tcp = parse_inet(TCP, "tcp");
        assert_eq!(tcp.len(), 3);
        assert_eq!(tcp[0].state, "LISTEN");
        assert_eq!((tcp[1].tx_queue, tcp[1].rx_queue), (2, 3));
        assert_eq!(tcp[1].remote_port, 8080);
        assert_eq!(tcp[2].state, "TIME_WAIT");
        assert_eq!(parse_inet(UDP, "udp")[0].state, "UNCONN");
        let unix = parse_unix(UNIX);
        assert_eq!(unix.len(), 3);
        assert!(unix[0].listening && unix[0].path == "/run/app.sock");
        assert_eq!(unix[1].state, "ESTAB");
        assert_eq!(unix[2].kind, "dgram");
        Ok(())
    }

    fn fake() -> TestResult<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("net"))?;
        fs::write(dir.path().join("net/tcp"), TCP)?;
        fs::write(dir.path().join("net/tcp6"), TCP6)?;
        fs::write(dir.path().join("net/udp"), UDP)?;
        fs::write(dir.path().join("net/unix"), UNIX)?;
        Ok(dir)
    }

    fn ss(dir: &std::path::Path, args: Value) -> TestResult<Value> {
        json_of(run(&ProcFs::at(dir), &serde_json::from_value(args)?))
    }

    fn inodes(value: &Value) -> Vec<u64> {
        value["sockets"]
            .as_array()
            .map(|a| a.iter().filter_map(|s| s["inode"].as_u64()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn default_shows_connected_tcp_and_udp_only() -> TestResult {
        let dir = fake()?;
        let value = ss(dir.path(), json!({}))?;
        assert_eq!(inodes(&value), vec![0, 22222]);
        let listening = ss(dir.path(), json!({"listening": true}))?;
        assert_eq!(inodes(&listening), vec![11111, 33333, 44444]);
        let all = ss(dir.path(), json!({"all": true}))?;
        assert_eq!(all["matched"], 5);
        Ok(())
    }

    #[test]
    fn protocol_family_port_and_unix_filters() -> TestResult {
        let dir = fake()?;
        let p = dir.path();
        assert_eq!(
            inodes(&ss(p, json!({"tcp": true, "listening": true}))?),
            vec![11111, 33333]
        );
        assert_eq!(
            inodes(&ss(p, json!({"udp": true, "all": true}))?),
            vec![44444]
        );
        assert_eq!(
            inodes(&ss(p, json!({"all": true, "ipv6": true}))?),
            vec![33333]
        );
        assert_eq!(
            inodes(&ss(p, json!({"all": true, "ipv4": true, "tcp": true}))?),
            vec![0, 11111, 22222]
        );
        assert_eq!(
            inodes(&ss(p, json!({"all": true, "port": 8080}))?),
            vec![11111, 22222]
        );
        let unix = ss(p, json!({"unix": true, "all": true}))?;
        assert_eq!(inodes(&unix), vec![66666, 55555, 77777]);
        let unix_listen = ss(p, json!({"unix": true, "listening": true}))?;
        assert_eq!(unix_listen["sockets"][0]["path"], "/run/app.sock");
        let limited = ss(p, json!({"all": true, "limit": 2}))?;
        assert_eq!(limited["count"], 2);
        assert_eq!(limited["truncated"], true);
        assert_eq!(limited["matched"], 5);
        Ok(())
    }

    #[test]
    fn invalid_arguments_and_missing_tables() -> TestResult {
        let dir = fake()?;
        for bad in [
            json!({"listening": true, "all": true}),
            json!({"ipv4": true, "ipv6": true}),
            json!({"port": 0}),
            json!({"port": 65536}),
        ] {
            error_of(run(
                &ProcFs::at(dir.path()),
                &serde_json::from_value(bad.clone())?,
            ))
            .map_err(|e| TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        let empty = tempfile::tempdir()?;
        assert_eq!(ss(empty.path(), json!({"all": true}))?["count"], 0);
        assert!(serde_json::from_value::<SsArgs>(json!({"processes": true})).is_err());
        Ok(())
    }

    #[test]
    fn real_proc_net_tables_parse_without_error() -> TestResult {
        let value = json_of(run(
            &ProcFs::real(),
            &serde_json::from_value(json!({"all": true, "tcp": true, "udp": true, "unix": true}))?,
        ))?;
        assert!(value["sockets"].is_array());
        Ok(())
    }
}
