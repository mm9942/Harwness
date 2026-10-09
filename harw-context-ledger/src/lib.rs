//! Context ledger: one record per context fragment and turn.
//!
//! # Responsibility
//! The context strand has five places that decide what a model sees, and none
//! of them remembered what was offered, what was left out and why. The ledger
//! is that memory: the turn loop records, per turn, every fragment it
//! **offered**, every one it **omitted** (with the reason) and, later, the ones
//! that were **used**. Learning, tuning and `harw doctor` read it; nothing else
//! depends on it.
//!
//! # Privacy
//! Entries carry labels, provider names, trust classes, sizes and reasons —
//! **never fragment content**. The file sink is bounded (one rotation) and lives
//! under a directory the caller chooses.
//!
//! # Use
//! A [`LedgerSink`] is injected into the session as an optional service (like
//! the tool-outcome observer). [`MemoryLedger`] collects entries for tests,
//! [`FileLedger`] appends JSON lines.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// What happened to a fragment in a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerKind {
    /// The fragment was part of the assembled context.
    Offered,
    /// The fragment was left out (see [`LedgerEntry::reason`]).
    Omitted,
    /// The fragment was referenced by the model's answer or tool calls.
    Used,
}

/// One ledger record. Labels and sizes only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    /// Wall clock, milliseconds since the Unix epoch.
    pub ts_unix_ms: u64,
    /// Session id.
    pub session_id: String,
    /// Turn id.
    pub turn_id: String,
    /// What happened.
    pub kind: LedgerKind,
    /// Fragment label (never content).
    pub label: String,
    /// Provider that produced the fragment, when known.
    pub provider: Option<String>,
    /// Trust class (`instruction`, `evidence`, `data`), when known.
    pub trust: Option<String>,
    /// Size in bytes, when known.
    pub bytes: Option<u64>,
    /// Estimated tokens, when known.
    pub tokens: Option<u32>,
    /// Why it was omitted, for [`LedgerKind::Omitted`].
    pub reason: Option<String>,
}

impl LedgerEntry {
    /// Entry of `kind` for `label`, stamped with the current time.
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        kind: LedgerKind,
        label: impl Into<String>,
    ) -> Self {
        Self {
            ts_unix_ms: now_ms(),
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            kind,
            label: label.into(),
            provider: None,
            trust: None,
            bytes: None,
            tokens: None,
            reason: None,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Receiver of ledger entries. Implementations must not block for long and
/// must never fail the turn: errors are swallowed or logged by the sink.
pub trait LedgerSink: Send + Sync {
    /// Records `entries` (one turn's worth).
    fn record(&self, entries: &[LedgerEntry]);
}

/// In-memory sink for tests and for readers that aggregate in process.
#[derive(Debug, Default)]
pub struct MemoryLedger {
    entries: Mutex<Vec<LedgerEntry>>,
}

impl MemoryLedger {
    /// Empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of all recorded entries.
    #[must_use]
    pub fn entries(&self) -> Vec<LedgerEntry> {
        self.entries.lock().map(|e| e.clone()).unwrap_or_default()
    }
}

impl LedgerSink for MemoryLedger {
    fn record(&self, entries: &[LedgerEntry]) {
        if let Ok(mut all) = self.entries.lock() {
            all.extend_from_slice(entries);
        }
    }
}

/// File name of the active ledger.
pub const LEDGER_FILE: &str = "context-ledger.jsonl";
/// File name of the single rotated ledger.
pub const LEDGER_ROTATED: &str = "context-ledger.jsonl.1";
/// Default size at which the active file rotates.
pub const DEFAULT_MAX_BYTES: u64 = 4 * 1_048_576;

/// JSON-lines sink under a directory. Bounded: when the active file exceeds
/// `max_bytes` it is renamed over the previous rotation, so the ledger never
/// holds more than about `2 * max_bytes`.
#[derive(Debug)]
pub struct FileLedger {
    dir: PathBuf,
    max_bytes: u64,
    file: Mutex<Option<File>>,
}

impl FileLedger {
    /// Opens (creating) `dir`; entries append to `<dir>/context-ledger.jsonl`.
    ///
    /// # Errors
    /// I/O error when the directory cannot be created.
    pub fn open(dir: &Path, max_bytes: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            max_bytes: max_bytes.max(1024),
            file: Mutex::new(None),
        })
    }

    fn active(&self) -> PathBuf {
        self.dir.join(LEDGER_FILE)
    }

    fn append(&self, entries: &[LedgerEntry]) -> std::io::Result<()> {
        let mut guard = self
            .file
            .lock()
            .map_err(|_| std::io::Error::other("ledger lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.active())?,
            );
        }
        let mut text = String::new();
        for entry in entries {
            let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
            text.push_str(&line);
            text.push('\n');
        }
        if let Some(file) = guard.as_mut() {
            file.write_all(text.as_bytes())?;
            if file.metadata()?.len() > self.max_bytes {
                *guard = None;
                std::fs::rename(self.active(), self.dir.join(LEDGER_ROTATED))?;
            }
        }
        Ok(())
    }
}

impl LedgerSink for FileLedger {
    fn record(&self, entries: &[LedgerEntry]) {
        if entries.is_empty() {
            return;
        }
        // A broken ledger must never break a turn.
        let _ = self.append(entries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_test_support::define_test_error;

    define_test_error!(pub(crate));

    fn entry(kind: LedgerKind, label: &str) -> LedgerEntry {
        LedgerEntry::new("s1", "t1", kind, label)
    }

    #[test]
    fn memory_ledger_collects_entries() {
        let ledger = MemoryLedger::new();
        ledger.record(&[
            entry(LedgerKind::Offered, "a"),
            entry(LedgerKind::Omitted, "b"),
        ]);
        let all = ledger.entries();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].kind, LedgerKind::Omitted);
    }

    #[test]
    fn file_ledger_writes_json_lines_without_content() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ledger = FileLedger::open(dir.path(), DEFAULT_MAX_BYTES).map_err(ctx("open"))?;
        let mut e = entry(LedgerKind::Omitted, "memory-facts");
        e.reason = Some("over-budget".to_owned());
        e.bytes = Some(120);
        ledger.record(&[e.clone()]);
        let text = std::fs::read_to_string(dir.path().join(LEDGER_FILE)).map_err(ctx("read"))?;
        let back: LedgerEntry = serde_json::from_str(text.trim()).map_err(ctx("parse"))?;
        assert_eq!(back, e);
        assert!(!text.contains("body"), "no content field exists");
        Ok(())
    }

    #[test]
    fn file_ledger_rotates_once_and_stays_bounded() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ledger = FileLedger::open(dir.path(), 1024).map_err(ctx("open"))?;
        for n in 0..200 {
            ledger.record(&[entry(LedgerKind::Offered, &format!("label-{n}"))]);
        }
        let active = std::fs::metadata(dir.path().join(LEDGER_FILE))
            .map(|m| m.len())
            .unwrap_or(0);
        assert!(active <= 2048, "active file bounded, got {active}");
        assert!(
            dir.path().join(LEDGER_ROTATED).exists(),
            "one rotation exists"
        );
        let files = std::fs::read_dir(dir.path()).map_err(ctx("list"))?.count();
        assert!(files <= 2, "never more than active + one rotation");
        Ok(())
    }

    #[test]
    fn unknown_fields_are_rejected_and_empty_batches_write_nothing() -> TestResult {
        let bad = r#"{"ts_unix_ms":1,"session_id":"s","turn_id":"t","kind":"offered","label":"l","provider":null,"trust":null,"bytes":null,"tokens":null,"reason":null,"body":"x"}"#;
        assert!(serde_json::from_str::<LedgerEntry>(bad).is_err());
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ledger = FileLedger::open(dir.path(), DEFAULT_MAX_BYTES).map_err(ctx("open"))?;
        ledger.record(&[]);
        assert!(!dir.path().join(LEDGER_FILE).exists());
        Ok(())
    }
}
