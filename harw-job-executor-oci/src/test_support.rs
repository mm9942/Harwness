//! Test error type and a fake Docker-compatible engine on a Unix socket.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

harw_test_support::define_test_error!(pub(crate));

/// What the fake engine does.
#[derive(Debug, Clone, Default)]
pub(crate) struct Behavior {
    /// Report `Memory: 0` although the create asked for a limit.
    pub(crate) drop_memory: bool,
    /// The image is not present locally.
    pub(crate) image_missing: bool,
    /// Exit code the container ends with right after start.
    pub(crate) exit_code: i64,
    /// Keep running until stopped/killed.
    pub(crate) run_forever: bool,
    /// Answer inspect with a body this large.
    pub(crate) huge_inspect: Option<usize>,
    /// Replace the owner label in inspect.
    pub(crate) spoof_owner: bool,
    /// Report a different image id than the pinned image's.
    pub(crate) wrong_image: bool,
}

#[derive(Debug, Clone)]
struct Container {
    labels: serde_json::Value,
    host: serde_json::Value,
    running: bool,
    exit_code: Option<i64>,
}

#[derive(Debug, Default)]
struct State {
    seq: u32,
    containers: HashMap<String, Container>,
    log: Vec<(String, String)>,
    behavior: Behavior,
}

/// A fake engine listening on a temp-dir socket.
pub(crate) struct FakeEngine {
    pub(crate) socket: PathBuf,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    _dir: tempfile::TempDir,
}

/// Image id (config digest) of the fake image; differs from the manifest digest.
pub(crate) const IMAGE_ID_HEX: &str =
    "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
pub(crate) const IMAGE_HEX: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

impl FakeEngine {
    pub(crate) fn start(behavior: Behavior) -> TestResult<Self> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let socket = dir.path().join("engine.sock");
        let listener = UnixListener::bind(&socket).map_err(ctx("bind"))?;
        let state = Arc::new(Mutex::new(State {
            behavior,
            ..State::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (st, sp) = (Arc::clone(&state), Arc::clone(&stop));
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if sp.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(conn) = conn {
                    let st = Arc::clone(&st);
                    std::thread::spawn(move || serve(conn, &st));
                }
            }
        });
        Ok(Self {
            socket,
            state,
            stop,
            _dir: dir,
        })
    }

    /// `(method, path)` of every request so far.
    pub(crate) fn requests(&self) -> Vec<(String, String)> {
        self.state.lock().map(|s| s.log.clone()).unwrap_or_default()
    }

    pub(crate) fn saw(&self, method: &str, needle: &str) -> bool {
        self.requests()
            .iter()
            .any(|(m, p)| m == method && p.contains(needle))
    }

    pub(crate) fn host_config(&self) -> TestResult<serde_json::Value> {
        let state = self.state.lock().map_err(|_| TestError::Missing("lock"))?;
        state
            .containers
            .values()
            .next()
            .map(|c| c.host.clone())
            .ok_or(TestError::Missing("container"))
    }

    pub(crate) fn container_count(&self) -> usize {
        self.state.lock().map(|s| s.containers.len()).unwrap_or(0)
    }

    pub(crate) fn set_behavior(&self, f: impl FnOnce(&mut Behavior)) {
        if let Ok(mut s) = self.state.lock() {
            f(&mut s.behavior);
        }
    }
}

impl Drop for FakeEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&self.socket);
    }
}

fn respond(conn: &mut UnixStream, status: u16, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(body);
}

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![kind, 0, 0, 0];
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn read_request(conn: &mut UnixStream) -> Option<(String, String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if conn.read(&mut byte).ok()? == 0 {
            return None;
        }
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_owned(), first.next()?.to_owned());
    let len = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    conn.read_exact(&mut body).ok()?;
    Some((method, path, body))
}

fn container_id(path: &str) -> Option<String> {
    let rest = path.split("/containers/").nth(1)?;
    Some(rest.split(['/', '?']).next()?.to_owned())
}

fn serve(mut conn: UnixStream, state: &Arc<Mutex<State>>) {
    let Some((method, path, body)) = read_request(&mut conn) else {
        return;
    };
    let Ok(mut st) = state.lock() else { return };
    st.log.push((method.clone(), path.clone()));
    let behavior = st.behavior.clone();
    let tail = path.split("/containers/").nth(1).unwrap_or("");
    let action = tail
        .split('?')
        .next()
        .unwrap_or("")
        .split('/')
        .nth(1)
        .unwrap_or("");
    if path.contains("/images/") {
        if behavior.image_missing {
            return respond(&mut conn, 404, b"{}");
        }
        let doc = serde_json::json!({"Id": format!("sha256:{IMAGE_ID_HEX}"), "RepoDigests": [format!("rust@sha256:{IMAGE_HEX}")]});
        return respond(&mut conn, 200, doc.to_string().as_bytes());
    }
    if path.ends_with("/containers/create") {
        st.seq += 1;
        let id = format!("{:064x}", st.seq);
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        st.containers.insert(
            id.clone(),
            Container {
                labels: req["Labels"].clone(),
                host: req["HostConfig"].clone(),
                running: false,
                exit_code: None,
            },
        );
        return respond(
            &mut conn,
            201,
            serde_json::json!({"Id": id}).to_string().as_bytes(),
        );
    }
    if path.contains("/containers/json") {
        let ids: Vec<_> = st
            .containers
            .keys()
            .map(|id| serde_json::json!({"Id": id}))
            .collect();
        return respond(
            &mut conn,
            200,
            serde_json::Value::Array(ids).to_string().as_bytes(),
        );
    }
    let Some(id) = container_id(&path) else {
        return respond(&mut conn, 400, b"{}");
    };
    if !st.containers.contains_key(&id) {
        return respond(&mut conn, 404, b"{}");
    }
    match (method.as_str(), action) {
        ("GET", "json") => {
            let c = st.containers[&id].clone();
            if let Some(size) = behavior.huge_inspect {
                return respond(&mut conn, 200, &vec![b' '; size]);
            }
            let mut host = c.host.clone();
            if behavior.drop_memory {
                host["Memory"] = 0.into();
            }
            let mut labels = c.labels.clone();
            if behavior.spoof_owner {
                labels["harw.owner"] = "someone-else".into();
            }
            let mounts: Vec<_> = host["Tmpfs"]
                .as_object()
                .map(|m| {
                    m.keys()
                        .map(|k| serde_json::json!({"Type": "tmpfs", "Destination": k}))
                        .collect()
                })
                .unwrap_or_default();
            let doc = serde_json::json!({
                "Id": id,
                "Created": "2026-10-03T10:00:00.123456789Z",
                // Like a real engine: the local *name* here, the id below.
                "Config": {"Image": "docker.io/library/rust:latest", "Labels": labels},
                "Image": if behavior.wrong_image { format!("sha256:{}", "0".repeat(64)) } else { format!("sha256:{IMAGE_ID_HEX}") },
                "HostConfig": host,
                "State": {"Running": c.running, "ExitCode": c.exit_code.unwrap_or(0)},
                "Mounts": mounts,
            });
            respond(&mut conn, 200, doc.to_string().as_bytes());
        }
        ("POST", "start") => {
            if let Some(c) = st.containers.get_mut(&id) {
                if behavior.run_forever {
                    c.running = true;
                } else {
                    c.running = false;
                    c.exit_code = Some(behavior.exit_code);
                }
            }
            respond(&mut conn, 204, b"");
        }
        ("POST", "stop" | "kill") => {
            if let Some(c) = st.containers.get_mut(&id) {
                c.running = false;
                c.exit_code = Some(137);
            }
            respond(&mut conn, 204, b"");
        }
        ("DELETE", _) => {
            st.containers.remove(&id);
            respond(&mut conn, 204, b"");
        }
        ("POST", "wait") => {
            drop(st);
            loop {
                let code = state.lock().ok().and_then(|s| {
                    s.containers
                        .get(&id)
                        .and_then(|c| (!c.running).then_some(c.exit_code).flatten())
                });
                if let Some(code) = code {
                    return respond(
                        &mut conn,
                        200,
                        serde_json::json!({"StatusCode": code})
                            .to_string()
                            .as_bytes(),
                    );
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        ("GET", "logs") => {
            let mut payload = frame(1, b"hello\n");
            payload.extend(frame(2, b"oops\n"));
            let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            let _ = conn.write_all(head.as_bytes());
            let _ = conn.write_all(format!("{:x}\r\n", payload.len()).as_bytes());
            let _ = conn.write_all(&payload);
            let _ = conn.write_all(b"\r\n0\r\n\r\n");
        }
        _ => respond(&mut conn, 400, b"{}"),
    }
}
