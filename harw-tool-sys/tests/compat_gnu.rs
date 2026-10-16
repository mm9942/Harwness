//! Kreuzprüfung gegen das echte System und die GNU-/procps-Werkzeuge (nur Tests).
//!
//! Die `sys.*`-Werkzeuge lesen `/proc` selbst. Diese Tests starten reale
//! Hilfsprozesse (`sleep`), öffnen echte Sockets und Dateien und vergleichen
//! das Ergebnis mit `ps`, `pgrep`, `lsof`, `free`, `uname`, `id`, `date`,
//! `which`, `uptime` und `env`, soweit sie auf dem Testrechner vorhanden sind.
//! Fehlt ein Befehl, wird der Vergleich übersprungen und auf stderr gemeldet
//! (`skipped: <befehl>`), nie still als bestanden gewertet.

use harw_tool_sys::procfs::ProcFs;
use harw_tools::ToolOutput;
use serde_json::{Value, json};
use std::process::{Child, Command, Stdio};

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "I/O error",
    Json(serde_json::Error) => "JSON error"
);

fn gnu(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C.UTF-8")
        .env("TZ", "UTC")
        .output()
        .ok()?;
    if !output.status.success() && output.stdout.is_empty() {
        eprintln!(
            "skipped: {program} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn value(output: ToolOutput) -> TestResult<Value> {
    match output {
        ToolOutput::Json { content } => Ok(content),
        other => Err(TestError::Unexpected(format!("not JSON: {other:?}"))),
    }
}

/// Ein Hilfsprozess, der beim Verlassen des Tests beendet wird.
struct Sleeper(Child);

impl Sleeper {
    fn start(seconds: &str) -> TestResult<Self> {
        let child = Command::new("sleep")
            .arg(seconds)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        // Warten, bis der Prozess nach dem exec schläft (Lader fertig, Speicher stabil).
        let pid = child.id();
        for _ in 0..400 {
            let settled = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|s| s.contains("(sleep) S "))
                && std::fs::read(format!("/proc/{pid}/cmdline"))
                    .is_ok_and(|b| b.starts_with(b"sleep"));
            if settled {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(Self(child))
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn ps_matches_procps_for_a_real_child() -> TestResult {
    let child = Sleeper::start("31")?;
    let pid = i64::from(child.pid());
    let args = serde_json::from_value(
        json!({"pids": [pid], "fields": ["pid", "ppid", "user", "name", "state", "command", "rss_kib", "threads"]}),
    )?;
    let got = value(harw_tool_sys::ps::run(&ProcFs::real(), &args))?;
    assert_eq!(got["count"], 1, "{got}");
    let p = &got["processes"][0];
    assert_eq!(p["ppid"], i64::from(std::process::id()));
    assert_eq!(p["name"], "sleep");
    assert_eq!(p["command"], "sleep 31");
    assert_eq!(p["state"], "S");
    let pid_arg = pid.to_string();
    let Some(expected) = gnu(
        "ps",
        &[
            "-o",
            "pid=,ppid=,user:20=,comm=,s=,args=,rss=,nlwp=",
            "-p",
            &pid_arg,
        ],
    ) else {
        eprintln!("skipped: ps");
        return Ok(());
    };
    let fields: Vec<&str> = expected.split_whitespace().collect();
    assert_eq!(fields[0], pid_arg);
    assert_eq!(fields[1], std::process::id().to_string());
    assert_eq!(p["user"].as_str(), Some(fields[2]));
    assert_eq!(p["name"].as_str(), Some(fields[3]));
    assert_eq!(p["state"].as_str(), Some(fields[4]));
    assert_eq!(fields[5..7].join(" "), "sleep 31");
    assert_eq!(p["rss_kib"].to_string(), fields[7], "rss");
    assert_eq!(p["threads"].to_string(), fields[8], "threads");
    Ok(())
}

#[test]
fn ps_process_set_matches_procps() -> TestResult {
    let Some(expected) = gnu("ps", &["-e", "-o", "pid="]) else {
        eprintln!("skipped: ps");
        return Ok(());
    };
    let theirs: std::collections::BTreeSet<i64> = expected
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect();
    let got = value(harw_tool_sys::ps::run(
        &ProcFs::real(),
        &serde_json::from_value(json!({"fields": ["pid"], "limit": 2000}))?,
    ))?;
    let ours: std::collections::BTreeSet<i64> = got["processes"]
        .as_array()
        .map(|a| a.iter().filter_map(|p| p["pid"].as_i64()).collect())
        .unwrap_or_default();
    // Prozesse kommen und gehen (ps selbst, Aufräumen): kleine Abweichungen sind normal.
    let only_theirs = theirs.difference(&ours).count();
    let only_ours = ours.difference(&theirs).count();
    assert!(
        only_theirs <= 3 && only_ours <= 3,
        "ours {} / theirs {}: only_theirs {only_theirs}, only_ours {only_ours}",
        ours.len(),
        theirs.len()
    );
    Ok(())
}

#[test]
fn pgrep_matches_procps() -> TestResult {
    let child = Sleeper::start("32")?;
    let pid = i64::from(child.pid());
    let got = value(harw_tool_sys::pgrep::run(
        &ProcFs::real(),
        i32::try_from(std::process::id()).unwrap_or(0),
        &serde_json::from_value(json!({"pattern": "sleep 32", "full": true}))?,
    ))?;
    assert_eq!(got["pids"], json!([pid]));
    let Some(expected) = gnu("pgrep", &["-f", "sleep 32"]) else {
        eprintln!("skipped: pgrep");
        return Ok(());
    };
    assert_eq!(expected.trim(), pid.to_string());
    let by_name = value(harw_tool_sys::pgrep::run(
        &ProcFs::real(),
        0,
        &serde_json::from_value(json!({"pattern": "sleep", "exact": true, "newest": true}))?,
    ))?;
    let theirs = gnu("pgrep", &["-x", "-n", "sleep"]).unwrap_or_default();
    assert_eq!(by_name["pids"][0].to_string(), theirs.trim());
    Ok(())
}

#[test]
fn lsof_sees_the_same_files_as_lsof() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().canonicalize()?.join("held.txt");
    let held = std::fs::File::create(&path)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let pid = i64::from(std::process::id());
    let uid = rustix::process::geteuid().as_raw();
    let got = value(harw_tool_sys::lsof::run(
        &ProcFs::real(),
        uid,
        &serde_json::from_value(json!({"pid": pid, "limit": 2000}))?,
    ))?;
    let fds = got["descriptors"].as_array().cloned().unwrap_or_default();
    let file = fds
        .iter()
        .find(|d| d["target"].as_str() == path.to_str())
        .ok_or(TestError::Unexpected(format!(
            "held file not listed: {got}"
        )))?;
    assert_eq!(file["type"], "file");
    assert_eq!(file["access"], "w");
    let socket = fds
        .iter()
        .find(|d| {
            d["socket"]
                .as_str()
                .is_some_and(|s| s.contains(&format!("127.0.0.1:{port}")) && s.contains("LISTEN"))
        })
        .ok_or(TestError::Unexpected(format!("listener not listed: {got}")))?;
    assert_eq!(socket["type"], "socket");
    if let Some(theirs) = gnu("lsof", &["-n", "-P", "-p", &pid.to_string(), "-Fn"]) {
        assert!(
            theirs.contains(&format!("n{}", path.display())),
            "lsof does not list the file: {theirs}"
        );
        assert!(
            theirs.contains(&format!("127.0.0.1:{port}")),
            "lsof does not list the socket"
        );
    } else {
        eprintln!("skipped: lsof");
    }
    drop(held);
    // Fremde Prozesse (PID 1 gehört root) werden abgelehnt, sofern wir nicht root sind.
    if uid != 0 {
        let foreign = harw_tool_sys::lsof::run(
            &ProcFs::real(),
            uid,
            &serde_json::from_value(json!({"pid": 1}))?,
        );
        assert!(matches!(foreign, ToolOutput::Error { .. }));
    }
    Ok(())
}

#[test]
fn ss_sees_real_sockets() -> TestResult {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let client = std::net::TcpStream::connect(("127.0.0.1", port))?;
    let (accepted, _) = listener.accept()?;
    let udp = std::net::UdpSocket::bind("127.0.0.1:0")?;
    let udp_port = udp.local_addr()?.port();
    let ss = |args: Value| -> TestResult<Value> {
        value(harw_tool_sys::net::run(
            &ProcFs::real(),
            &serde_json::from_value(args)?,
        ))
    };
    let listening = ss(json!({"listening": true, "tcp": true, "ipv4": true, "port": port}))?;
    assert_eq!(listening["count"], 1, "{listening}");
    assert_eq!(listening["sockets"][0]["state"], "LISTEN");
    assert_eq!(listening["sockets"][0]["local_addr"], "127.0.0.1");
    assert_eq!(
        listening["sockets"][0]["uid"],
        rustix::process::geteuid().as_raw()
    );
    let connected = ss(json!({"tcp": true, "ipv4": true, "port": port}))?;
    // Beide Enden der Verbindung (Client und akzeptierter Socket) stehen auf ESTABLISHED.
    assert_eq!(connected["count"], 2, "{connected}");
    assert!(
        connected["sockets"]
            .as_array()
            .is_some_and(|s| s.iter().all(|x| x["state"] == "ESTABLISHED"))
    );
    let udp_sockets = ss(json!({"udp": true, "listening": true, "port": udp_port}))?;
    assert_eq!(udp_sockets["count"], 1, "{udp_sockets}");
    assert_eq!(udp_sockets["sockets"][0]["state"], "UNCONN");
    drop((client, accepted, udp));
    Ok(())
}

#[test]
fn free_uname_id_date_which_uptime_env_match_the_gnu_tools() -> TestResult {
    // free
    if let Some(expected) = gnu("free", &["-b"]) {
        let mem = expected
            .lines()
            .find(|l| l.starts_with("Mem:"))
            .unwrap_or("");
        let f: Vec<u64> = mem
            .split_whitespace()
            .skip(1)
            .filter_map(|n| n.parse().ok())
            .collect();
        let got = value(harw_tool_sys::sysinfo::run_free(
            &ProcFs::real(),
            &serde_json::from_value(json!({}))?,
        ))?;
        assert_eq!(got["total_bytes"].as_u64(), Some(f[0]));
        let theirs_avail = f[5];
        let ours_avail = got["available_bytes"].as_u64().unwrap_or(0);
        assert!(
            theirs_avail.abs_diff(ours_avail) < 256 * 1024 * 1024,
            "{theirs_avail} vs {ours_avail}"
        );
    } else {
        eprintln!("skipped: free");
    }
    // uname
    for (flag, field) in [
        ("-s", "kernel_name"),
        ("-n", "nodename"),
        ("-r", "kernel_release"),
        ("-v", "kernel_version"),
        ("-m", "machine"),
        ("-o", "operating_system"),
    ] {
        let Some(expected) = gnu("uname", &[flag]) else {
            eprintln!("skipped: uname");
            break;
        };
        let got = value(harw_tool_sys::sysinfo::run_uname(&serde_json::from_value(
            json!({field: true}),
        )?))?;
        assert_eq!(got["text"].as_str(), Some(expected.trim()), "uname {flag}");
    }
    // `-a` enthält bei GNU je nach Distribution auch `-p`/`-i`; die sechs Felder bleiben in Reihenfolge.
    if let Some(expected) = gnu("uname", &["-s", "-n", "-r", "-v", "-m", "-o"]) {
        let got = value(harw_tool_sys::sysinfo::run_uname(&serde_json::from_value(
            json!({"all": true}),
        )?))?;
        assert_eq!(got["text"].as_str(), Some(expected.trim()));
    }
    // id / whoami
    let identity = harw_tool_sys::id::current_identity();
    let db = harw_tool_fsread::users::UserDb::load();
    let id = |args: Value| -> TestResult<String> {
        let out = value(harw_tool_sys::id::run(
            &identity,
            &db,
            &serde_json::from_value(args)?,
        ))?;
        Ok(out["text"].as_str().unwrap_or("").to_owned())
    };
    for (flags, ours) in [
        (vec!["-u"], json!({"user": true})),
        (vec!["-g"], json!({"group": true})),
        (vec!["-G"], json!({"groups": true})),
        (vec!["-un"], json!({"user": true, "name": true})),
        (vec!["-gn"], json!({"group": true, "name": true})),
        (vec!["-Gn"], json!({"groups": true, "name": true})),
        (vec!["-ur"], json!({"user": true, "real": true})),
    ] {
        let Some(expected) = gnu("id", &flags) else {
            eprintln!("skipped: id");
            break;
        };
        assert_eq!(id(ours.clone())?, expected.trim(), "id {flags:?}");
    }
    if let Some(expected) = gnu("whoami", &[]) {
        assert_eq!(id(json!({"whoami": true}))?, expected.trim());
    }
    // date
    let epoch = 1_700_000_000i64;
    let format = "%Y-%m-%d %H:%M:%S %j %a %A %b %B %e %I %p %y %u %w %Z %F %T %D %R %s";
    if let Some(expected) = gnu(
        "date",
        &["-u", "-d", &format!("@{epoch}"), &format!("+{format}")],
    ) {
        let got = value(harw_tool_sys::date::run(
            (0, 0),
            &serde_json::from_value(json!({"epoch_secs": epoch, "format": format}))?,
        ))?;
        assert_eq!(
            got["formatted"].as_str(),
            Some(expected.trim_end_matches('\n'))
        );
    } else {
        eprintln!("skipped: date");
    }
    if let Some(expected) = gnu("date", &["-d", &format!("@{epoch}"), "+%H:%M %z"]) {
        // Mit TZ=UTC liefert GNU date +0000; wir erwarten dasselbe bei Offset 0.
        let got = value(harw_tool_sys::date::run(
            (0, 0),
            &serde_json::from_value(json!({"epoch_secs": epoch, "format": "%H:%M %z"}))?,
        ))?;
        assert_eq!(got["formatted"].as_str(), Some(expected.trim()));
    }
    // which
    if let Some(expected) = gnu("which", &["-a", "sh"]) {
        let got = value(harw_tool_sys::which::run(
            std::env::var_os("PATH"),
            &serde_json::from_value(json!({"names": ["sh"], "all": true}))?,
        ))?;
        let ours: Vec<&str> = got["results"][0]["paths"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let theirs: Vec<&str> = expected.lines().collect();
        assert_eq!(ours, theirs);
    } else {
        eprintln!("skipped: which");
    }
    // uptime
    if let Some(expected) = gnu("uptime", &["-s"]) {
        let got = value(harw_tool_sys::sysinfo::run_uptime(
            &ProcFs::real(),
            now(),
            &serde_json::from_value(json!({}))?,
        ))?;
        let since = got["up_since"]
            .as_str()
            .unwrap_or("")
            .replace('T', " ")
            .trim_end_matches('Z')
            .to_owned();
        // Auf die Sekunde kann es wegen Rundung abweichen: Datum und Minute müssen passen.
        assert_eq!(&since[..16], &expected.trim()[..16], "uptime -s");
    } else {
        eprintln!("skipped: uptime");
    }
    // env: gleiche Namen, nicht maskierte Werte identisch
    if let Some(expected) = gnu("env", &[]) {
        let got = value(harw_tool_sys::env::run(
            std::env::vars_os().collect(),
            &serde_json::from_value(json!({"limit": 1000}))?,
        ))?;
        let ours = got["variables"].as_array().cloned().unwrap_or_default();
        for line in expected.lines() {
            let Some((name, val)) = line.split_once('=') else {
                continue;
            };
            if name == "LC_ALL" || name == "TZ" || name == "_" || name == "PWD" || name == "OLDPWD"
            {
                continue;
            }
            if let Some(entry) = ours.iter().find(|e| e["name"] == name) {
                if entry["masked"] == false && !val.contains('\n') {
                    assert_eq!(entry["value"].as_str(), Some(val), "{name}");
                }
            }
        }
    } else {
        eprintln!("skipped: env");
    }
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

#[test]
fn top_agrees_with_the_process_count() -> TestResult {
    let _busy = Sleeper::start("33")?;
    let got = value(harw_tool_sys::top::run(
        &ProcFs::real(),
        &serde_json::from_value(json!({"interval_ms": 100, "limit": 10}))?,
        std::thread::sleep,
    ))?;
    let total = got["tasks"]["total"].as_u64().unwrap_or(0);
    let Some(expected) = gnu("ps", &["-e", "-o", "pid="]) else {
        eprintln!("skipped: ps");
        return Ok(());
    };
    let theirs = expected.split_whitespace().count() as u64;
    assert!(total.abs_diff(theirs) <= 5, "top {total} vs ps {theirs}");
    Ok(())
}
