//! Test error type and a fake Kubernetes API server on loopback HTTP.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

harw_test_support::define_test_error!(pub(crate));

/// What the fake API does.
#[derive(Debug, Clone, Default)]
pub(crate) struct Behavior {
    /// A mutating webhook marks the container privileged.
    pub(crate) webhook_privileged: bool,
    /// A mutating webhook removes the scheduling gate.
    pub(crate) strip_gate: bool,
    /// Exit code of the container once it runs.
    pub(crate) exit_code: i64,
    /// The pod keeps running until deleted.
    pub(crate) run_forever: bool,
    /// Report the memory limit as `256Mi`-style quantities.
    pub(crate) normalize_memory: bool,
    /// Answer create with a body this large.
    pub(crate) huge_create: Option<usize>,
}

#[derive(Debug, Clone)]
struct Pod {
    json: Value,
    gated: bool,
    gets_after_lift: u32,
    deleted_gracefully: bool,
}

#[derive(Debug, Default)]
struct State {
    seq: u32,
    pods: HashMap<String, Pod>,
    log: Vec<(String, String)>,
    behavior: Behavior,
}

pub(crate) struct FakeApi {
    pub(crate) base: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    addr: String,
}

impl FakeApi {
    pub(crate) fn start(behavior: Behavior) -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind"))?;
        let addr = listener.local_addr().map_err(ctx("addr"))?.to_string();
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
            base: format!("http://{addr}"),
            state,
            stop,
            addr,
        })
    }

    pub(crate) fn requests(&self) -> Vec<(String, String)> {
        self.state.lock().map(|s| s.log.clone()).unwrap_or_default()
    }

    pub(crate) fn saw(&self, method: &str, needle: &str) -> bool {
        self.requests()
            .iter()
            .any(|(m, p)| m == method && p.contains(needle))
    }

    pub(crate) fn pod_count(&self) -> usize {
        self.state.lock().map(|s| s.pods.len()).unwrap_or(0)
    }

    pub(crate) fn set_behavior(&self, f: impl FnOnce(&mut Behavior)) {
        if let Ok(mut s) = self.state.lock() {
            f(&mut s.behavior);
        }
    }

    /// Recreates every pod under a fresh uid (same name).
    pub(crate) fn recreate_pods(&self) {
        if let Ok(mut s) = self.state.lock() {
            let mut seq = s.seq;
            for pod in s.pods.values_mut() {
                seq += 1;
                pod.json["metadata"]["uid"] =
                    format!("{seq:08}-ffff-ffff-ffff-ffffffffffff").into();
            }
            s.seq = seq;
        }
    }
}

impl Drop for FakeApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.addr);
    }
}

fn respond(conn: &mut TcpStream, status: u16, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(body);
}

fn read_request(conn: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
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

fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn view(pod: &Pod, behavior: &Behavior) -> Value {
    let mut json = pod.json.clone();
    if pod.gated {
        json["status"] = json!({"phase": "Pending"});
    } else if pod.deleted_gracefully {
        json["status"] = json!({"phase": "Failed", "containerStatuses": [
            {"state": {"terminated": {"exitCode": 137}}}]});
    } else if pod.gets_after_lift <= 1 || behavior.run_forever {
        json["status"] = json!({"phase": "Running"});
    } else {
        let phase = if behavior.exit_code == 0 {
            "Succeeded"
        } else {
            "Failed"
        };
        json["status"] = json!({"phase": phase, "containerStatuses": [
            {"state": {"terminated": {"exitCode": behavior.exit_code}}}]});
    }
    json
}

fn serve(mut conn: TcpStream, state: &Arc<Mutex<State>>) {
    let Some((method, path, body)) = read_request(&mut conn) else {
        return;
    };
    let Ok(mut st) = state.lock() else { return };
    st.log.push((method.clone(), path.clone()));
    let behavior = st.behavior.clone();
    let (route, query) = path.split_once('?').unwrap_or((path.as_str(), ""));
    let after = route.split("/pods").nth(1).unwrap_or("");
    let mut parts = after.trim_start_matches('/').split('/');
    let name = parts.next().unwrap_or("").to_owned();
    let sub = parts.next().unwrap_or("");

    if method == "POST" && name.is_empty() {
        st.seq += 1;
        let mut json: Value = serde_json::from_slice(&body).unwrap_or_default();
        json["metadata"]["uid"] = format!("{:08}-aaaa-bbbb-cccc-dddddddddddd", st.seq).into();
        if behavior.webhook_privileged {
            json["spec"]["containers"][0]["securityContext"]["privileged"] = true.into();
        }
        if behavior.normalize_memory {
            if let Some(m) = json["spec"]["containers"][0]["resources"]["limits"]["memory"].as_str()
            {
                if let Ok(bytes) = m.parse::<u64>() {
                    json["spec"]["containers"][0]["resources"]["limits"]["memory"] =
                        format!("{}Mi", bytes >> 20).into();
                }
            }
        }
        let gated = !behavior.strip_gate;
        if !gated {
            json["spec"]["schedulingGates"] = json!([]);
        }
        let pod_name = json["metadata"]["name"].as_str().unwrap_or("").to_owned();
        let pod = Pod {
            json: json.clone(),
            gated,
            gets_after_lift: 0,
            deleted_gracefully: false,
        };
        let mut out = view(&pod, &behavior);
        st.pods.insert(pod_name, pod);
        if let Some(size) = behavior.huge_create {
            out["padding"] = "x".repeat(size).into();
        }
        return respond(&mut conn, 201, out.to_string().as_bytes());
    }
    if method == "GET" && name.is_empty() {
        let selector = unescape(query.strip_prefix("labelSelector=").unwrap_or(""));
        let wanted: Vec<(String, String)> = selector
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let items: Vec<Value> = st
            .pods
            .values()
            .filter(|p| {
                wanted
                    .iter()
                    .all(|(k, v)| p.json["metadata"]["labels"][k.as_str()].as_str() == Some(v))
            })
            .map(|p| view(p, &behavior))
            .collect();
        return respond(
            &mut conn,
            200,
            json!({"items": items}).to_string().as_bytes(),
        );
    }
    if !st.pods.contains_key(&name) {
        return respond(&mut conn, 404, b"{}");
    }
    match (method.as_str(), sub) {
        ("GET", "log") => respond(&mut conn, 200, b"hello\n"),
        ("GET", "") => {
            if let Some(pod) = st.pods.get_mut(&name) {
                if !pod.gated {
                    pod.gets_after_lift += 1;
                }
            }
            let out = view(&st.pods[&name], &behavior);
            respond(&mut conn, 200, out.to_string().as_bytes());
        }
        ("PATCH", "") => {
            if let Some(pod) = st.pods.get_mut(&name) {
                pod.gated = false;
                pod.json["spec"]["schedulingGates"] = json!([]);
            }
            let out = view(&st.pods[&name], &behavior);
            respond(&mut conn, 200, out.to_string().as_bytes());
        }
        ("DELETE", "") => {
            if query.contains("gracePeriodSeconds=0") {
                st.pods.remove(&name);
            } else if let Some(pod) = st.pods.get_mut(&name) {
                pod.deleted_gracefully = true;
                pod.gated = false;
            }
            respond(&mut conn, 200, b"{}");
        }
        _ => respond(&mut conn, 400, b"{}"),
    }
}
