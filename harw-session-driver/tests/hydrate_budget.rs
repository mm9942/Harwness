//! S0 measurement: time and memory budget of hydrating one large transcript.
//!
//! Ignored by default. Run explicitly with the path of a transcript copy in
//! the environment variable `HARW_HYDRATE_BUDGET_TRANSCRIPT`:
//!
//! ```text
//! HARW_HYDRATE_BUDGET_TRANSCRIPT=/path/to/copy.jsonl \
//!   cargo test -p harw-session-driver --test hydrate_budget -- --ignored --nocapture
//! ```
//!
//! The file is copied into a temporary store (the original is never touched);
//! its file stem becomes the session id. The test prints only sizes, counts
//! and timings, never any transcript content. The result decides whether a
//! tail-limited hydration is needed. Without the variable the test is a no-op.

use std::path::{Path, PathBuf};
use std::time::Instant;

use harw_core::{AgentSession, StateStore, TranscriptStateStore};
use harw_session_store::TranscriptStore;
use harw_types::{SessionId, ThreadRef};
use tokio::sync::mpsc;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const ENV_PATH: &str = "HARW_HYDRATE_BUDGET_TRANSCRIPT";

fn thread_for(sid: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("core-session:{}", sid.as_str()))
}

/// Reads one `Vm*:` line (in KiB) from `/proc/self/status`; `None` off Linux.
fn proc_status_kib(key: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|rest| rest.trim_start_matches(':').split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

fn show_kib(value: Option<u64>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |kib| format!("{kib} KiB"))
}

fn source_path() -> Option<PathBuf> {
    std::env::var_os(ENV_PATH)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn session_id_for(path: &Path) -> Result<SessionId, String> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| "transcript path has no usable file stem".to_owned())?;
    SessionId::try_from_str(stem).map_err(|_| "file stem is not a valid session id".to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "measurement; needs HARW_HYDRATE_BUDGET_TRANSCRIPT"]
async fn hydrate_budget_reports_time_and_memory() -> TestResult {
    let Some(source) = source_path() else {
        println!("{ENV_PATH} not set; nothing to measure");
        return Ok(());
    };
    let sid = session_id_for(&source)?;
    let bytes = std::fs::metadata(&source)?.len();

    let temp = tempfile::tempdir()?;
    let store = TranscriptStore::new(temp.path());
    let target = store.transcript_path(&sid)?;
    std::fs::copy(&source, &target)?;
    let state_store = TranscriptStateStore::new(store, thread_for);

    let (event_tx, _events) = mpsc::unbounded_channel();
    let mut session = AgentSession::new_with_id(
        sid.clone(),
        harw_types::roles::AgentRole::Assistant,
        None,
        harw_extension_api::ExtensionRegistryBuilder::default().build(),
        event_tx,
    );

    let rss_before = proc_status_kib("VmRSS");

    let load_started = Instant::now();
    let history = state_store.load_history(&sid).await?;
    let load_elapsed = load_started.elapsed();
    let loaded_items = history.len();
    drop(history);

    let hydrate_started = Instant::now();
    let report = session.hydrate_from_store(&state_store).await?;
    let hydrate_elapsed = hydrate_started.elapsed();

    let rss_after = proc_status_kib("VmRSS");
    let hwm = proc_status_kib("VmHWM");
    let delta = match (rss_before, rss_after) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };

    println!("hydrate_budget: file_bytes={bytes}");
    println!("hydrate_budget: load_history_items={loaded_items}");
    println!(
        "hydrate_budget: load_history_ms={}",
        load_elapsed.as_millis()
    );
    println!(
        "hydrate_budget: hydrate_items={} repaired_tool_calls={} state_restored={} skipped={}",
        report.history_items,
        report.repaired_tool_calls.len(),
        report.state_restored,
        report.skipped
    );
    println!("hydrate_budget: hydrate_ms={}", hydrate_elapsed.as_millis());
    println!("hydrate_budget: rss_before={}", show_kib(rss_before));
    println!("hydrate_budget: rss_after={}", show_kib(rss_after));
    println!("hydrate_budget: rss_delta={}", show_kib(delta));
    println!("hydrate_budget: peak_rss_hwm={}", show_kib(hwm));
    Ok(())
}
