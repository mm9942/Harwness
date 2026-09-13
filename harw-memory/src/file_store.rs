//! Datei-basierter Standard-Backend für [`crate::store::Memory`].
//!
//! # Verantwortungsbereich
//! Persistiert HOT/WARM/COLD-Tiers und ein append-only Signal-Log unter einer
//! Wurzel (`<HARWNESS_HOME>/memory/`). Alle Schreiboperationen sind atomar
//! (tmp + rename); Wartung läuft unter einem serialisierenden Lock, dessen
//! Fortschritt in `workflow.json` persistiert wird.
//!
//! # Layout
//! ```text
//! <root>/
//!   HOT.md
//!   INDEX.md
//!   warm/{namespace}.md
//!   cold/{namespace}.md
//!   signals/{corrections,reflections,patterns}.jsonl
//!   state.json
//!   workflow.json
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[cfg(any(
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "haiku",
    target_os = "illumos",
    target_os = "ios",
    target_os = "linux",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "solaris",
))]
use std::os::unix::fs::OpenOptionsExt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{MemoryError, MemoryResult};
use crate::store::Memory;
use crate::types::{Entry, MaintenanceReport, RecallQuery, Signal, Stats, Tier};
use crate::workflow::{WorkflowMarker, WorkflowStep};

/// Persistenter Zustands-/Zählerblock (`state.json`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedState {
    #[serde(with = "time::serde::rfc3339::option", default)]
    last_maintenance: Option<OffsetDateTime>,
    signals_seen: u64,
    promoted_to_hot: u64,
    demoted_to_cold: u64,
    warm_created: u64,
}

/// Signal-Log-Datei-Namen (Enum-getrennt für stabiles Routing).
const SIGNAL_FILE_CORRECTION: &str = "corrections.jsonl";
const SIGNAL_FILE_REFLECTION: &str = "reflections.jsonl";
const SIGNAL_FILE_PATTERN: &str = "patterns.jsonl";

#[cfg(any(target_os = "android", target_os = "linux"))]
const O_NOFOLLOW: i32 = 0o400000;

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "haiku",
    target_os = "illumos",
    target_os = "ios",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "solaris",
))]
const O_NOFOLLOW: i32 = 0x100;

/// Datei-basiertes Memory-Backend.
///
/// # Beschreibung
/// Standard-Implementierung, die alle Daten unter `root/` ablegt. `hot()` liest
/// die `HOT.md` bei jedem Aufruf frisch (die Datei ist per Konstruktion klein
/// genug, ~2 KB); `recall()` iteriert `warm/` und optional `cold/`; `record()`
/// hängt an das jeweilige Signal-Log an.
///
/// # Nebenläufigkeit
/// `Send + Sync`. `record()` und `maintain()` serialisieren über einen internen
/// `Mutex`; Reads (`hot`, `recall`, `stats`) sind lock-frei — sie sehen den
/// zuletzt committeten Zustand.
pub struct FileMemoryStore {
    root: PathBuf,
    lock: Mutex<()>,
}

impl FileMemoryStore {
    /// Erstellt einen Store an `root`, legt fehlende Unterverzeichnisse an.
    ///
    /// # Fehler
    /// [`MemoryError::Io`] wenn ein Verzeichnis nicht angelegt werden kann.
    pub fn open(root: impl AsRef<Path>) -> MemoryResult<Self> {
        let root = root.as_ref().to_path_buf();
        for sub in ["warm", "cold", "signals"] {
            let dir = root.join(sub);
            if !dir.exists() {
                fs::create_dir_all(&dir).map_err(|e| MemoryError::Io {
                    path: dir.clone(),
                    source: e,
                })?;
            }
        }
        Ok(Self {
            root,
            lock: Mutex::new(()),
        })
    }

    fn hot_path(&self) -> PathBuf {
        self.root.join("HOT.md")
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    fn workflow_path(&self) -> PathBuf {
        self.root.join("workflow.json")
    }

    fn signal_file(&self, signal: &Signal) -> PathBuf {
        let name = match signal {
            Signal::Correction { .. } => SIGNAL_FILE_CORRECTION,
            Signal::Reflection { .. } => SIGNAL_FILE_REFLECTION,
            Signal::PatternHint { .. } => SIGNAL_FILE_PATTERN,
        };
        self.root.join("signals").join(name)
    }

    fn tier_dir(&self, tier: Tier) -> Option<PathBuf> {
        match tier {
            Tier::Hot => None,
            Tier::Warm => Some(self.root.join("warm")),
            Tier::Cold => Some(self.root.join("cold")),
        }
    }

    /// Validiert einen Namespace (verwendet in Zukunft für Schreib-APIs auf WARM/COLD).
    #[allow(dead_code)]
    fn sanitized_namespace(raw: &str) -> MemoryResult<String> {
        if raw.is_empty() || raw.contains("..") || raw.starts_with('/') || raw.contains('\\') {
            return Err(MemoryError::InvalidNamespace {
                namespace: raw.to_owned(),
            });
        }
        for ch in raw.chars() {
            if !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '/' || ch == '.') {
                return Err(MemoryError::InvalidNamespace {
                    namespace: raw.to_owned(),
                });
            }
        }
        Ok(raw.to_owned())
    }

    fn read_state(&self) -> MemoryResult<PersistedState> {
        let path = self.state_path();
        let Some(bytes) =
            read_optional_file_without_following_symlinks(&path).map_err(|e| MemoryError::Io {
                path: path.clone(),
                source: e,
            })?
        else {
            return Ok(PersistedState::default());
        };
        serde_json::from_slice(&bytes).map_err(|e| MemoryError::Serde {
            context: "read_state",
            source: e,
        })
    }

    fn write_state(&self, state: &PersistedState) -> MemoryResult<()> {
        let path = self.state_path();
        let payload = serde_json::to_vec_pretty(state).map_err(|e| MemoryError::Serde {
            context: "write_state",
            source: e,
        })?;
        atomic_write(&path, &payload)
    }

    fn read_workflow(&self) -> MemoryResult<Option<WorkflowMarker>> {
        let path = self.workflow_path();
        let Some(bytes) =
            read_optional_file_without_following_symlinks(&path).map_err(|e| MemoryError::Io {
                path: path.clone(),
                source: e,
            })?
        else {
            return Ok(None);
        };
        let marker: WorkflowMarker =
            serde_json::from_slice(&bytes).map_err(|e| MemoryError::Serde {
                context: "read_workflow",
                source: e,
            })?;
        Ok(Some(marker))
    }

    fn write_workflow(&self, marker: &WorkflowMarker) -> MemoryResult<()> {
        let path = self.workflow_path();
        let payload = serde_json::to_vec_pretty(marker).map_err(|e| MemoryError::Serde {
            context: "write_workflow",
            source: e,
        })?;
        atomic_write(&path, &payload)
    }

    fn pending_signal_count(&self) -> MemoryResult<usize> {
        let mut count = 0;
        for name in [
            SIGNAL_FILE_CORRECTION,
            SIGNAL_FILE_REFLECTION,
            SIGNAL_FILE_PATTERN,
        ] {
            let path = self.root.join("signals").join(name);
            let Some(file) = open_optional_file_without_following_symlinks(&path).map_err(|e| {
                MemoryError::Io {
                    path: path.clone(),
                    source: e,
                }
            })?
            else {
                continue;
            };
            count += BufReader::new(file).lines().count();
        }
        let seen = self.read_state()?.signals_seen as usize;
        Ok(count.saturating_sub(seen))
    }

    /// Promotes repeated pattern hints into their durable WARM namespace.
    ///
    /// Signal logs are append-only, so the promotion target itself is the
    /// idempotency boundary: an already materialized namespace is not promoted
    /// again on later maintenance runs.
    fn promote_pattern_hints(&self, threshold: u32) -> MemoryResult<usize> {
        let path = self.root.join("signals").join(SIGNAL_FILE_PATTERN);
        let Some(file) =
            open_optional_file_without_following_symlinks(&path).map_err(|e| MemoryError::Io {
                path: path.clone(),
                source: e,
            })?
        else {
            return Ok(0);
        };
        let mut patterns: std::collections::HashMap<String, (u32, String)> =
            std::collections::HashMap::new();
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|e| MemoryError::Io {
                path: path.clone(),
                source: e,
            })?;
            let signal: Signal = serde_json::from_str(&line).map_err(|e| MemoryError::Serde {
                context: "read pattern signal",
                source: e,
            })?;
            if let Signal::PatternHint { key, note } = signal {
                let entry = patterns.entry(key).or_insert((0, note.clone()));
                entry.0 = entry.0.saturating_add(1);
                entry.1 = note;
            }
        }

        let warm_dir = self.root.join("warm");
        let mut promoted = 0;
        for (key, (count, note)) in patterns {
            if count < threshold {
                continue;
            }
            let namespace = Self::sanitized_namespace(&format!("domain/{key}"))?;
            let target = warm_dir.join(format!("{namespace}.md"));
            if target.exists() {
                continue;
            }
            let Some(parent) = target.parent() else {
                continue;
            };
            fs::create_dir_all(parent).map_err(|e| MemoryError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
            atomic_write(&target, format!("{note}\n").as_bytes())?;
            promoted += 1;
        }
        Ok(promoted)
    }

    /// Demotes the oldest HOT lines to WARM until the HOT line budget holds.
    ///
    /// The file store represents HOT as ordered Markdown rather than individual
    /// entries, so line order is the only durable recency signal available.
    fn demote_hot_overflow(&self, max_lines: usize) -> MemoryResult<usize> {
        let hot_path = self.hot_path();
        let Some(raw_hot) =
            read_optional_string_without_following_symlinks(&hot_path).map_err(|e| {
                MemoryError::Io {
                    path: hot_path.clone(),
                    source: e,
                }
            })?
        else {
            return Ok(0);
        };
        let lines: Vec<&str> = raw_hot.lines().collect();
        let demoted = lines.len().saturating_sub(max_lines);
        if demoted == 0 {
            return Ok(0);
        }

        let inactive_path = self.root.join("warm").join("inactive.md");
        let mut inactive = if inactive_path.exists() {
            fs::read_to_string(&inactive_path).map_err(|e| MemoryError::Io {
                path: inactive_path.clone(),
                source: e,
            })?
        } else {
            String::new()
        };
        inactive.push_str(&lines[..demoted].join("\n"));
        inactive.push('\n');
        atomic_write(&inactive_path, inactive.as_bytes())?;

        let retained = lines[demoted..].join("\n");
        let retained = if retained.is_empty() {
            retained
        } else {
            format!("{retained}\n")
        };
        atomic_write(&hot_path, retained.as_bytes())?;
        Ok(demoted)
    }

    fn list_namespaces(&self, tier: Tier) -> MemoryResult<Vec<(String, PathBuf)>> {
        let Some(dir) = self.tier_dir(tier) else {
            return Ok(Vec::new());
        };
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let metadata = fs::symlink_metadata(&dir).map_err(|e| MemoryError::Io {
            path: dir.clone(),
            source: e,
        })?;
        if metadata.file_type().is_symlink() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        walk_dir(&dir, &dir, &mut out)?;
        Ok(out)
    }
}

fn walk_dir(root: &Path, current: &Path, out: &mut Vec<(String, PathBuf)>) -> MemoryResult<()> {
    let entries = fs::read_dir(current).map_err(|e| MemoryError::Io {
        path: current.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| MemoryError::Io {
            path: current.to_path_buf(),
            source: e,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| MemoryError::Io {
            path: path.clone(),
            source: e,
        })?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            walk_dir(root, &path, out)?;
        } else if file_type.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md") {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let ns = rel.with_extension("").to_string_lossy().replace('\\', "/");
            out.push((ns, path));
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> MemoryResult<()> {
    let tmp = path.with_extension("tmp");
    reject_symlink(path).map_err(|e| MemoryError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    reject_symlink(&tmp).map_err(|e| MemoryError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let mut file =
        open_without_following_symlinks(&mut options, &tmp).map_err(|e| MemoryError::Io {
            path: tmp.clone(),
            source: e,
        })?;
    file.write_all(bytes).map_err(|e| MemoryError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    file.sync_all().map_err(|e| MemoryError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    fs::rename(&tmp, path).map_err(|e| MemoryError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

fn open_optional_file_without_following_symlinks(path: &Path) -> io::Result<Option<File>> {
    match open_file_without_following_symlinks(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_optional_file_without_following_symlinks(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let Some(mut file) = open_optional_file_without_following_symlinks(path)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

fn read_optional_string_without_following_symlinks(path: &Path) -> io::Result<Option<String>> {
    read_optional_file_without_following_symlinks(path)?
        .map(|bytes| {
            String::from_utf8(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        })
        .transpose()
}

fn open_file_without_following_symlinks(path: &Path) -> io::Result<File> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    open_without_following_symlinks(&mut options, path)
}

fn open_without_following_symlinks(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    #[cfg(any(
        target_os = "android",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "haiku",
        target_os = "illumos",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "solaris",
    ))]
    options.custom_flags(O_NOFOLLOW);

    options.open(path)
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "memory file must not be a symbolic link: {}",
                path.display()
            ),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

impl Memory for FileMemoryStore {
    fn hot(&self) -> MemoryResult<String> {
        let path = self.hot_path();
        let Some(content) =
            read_optional_string_without_following_symlinks(&path).map_err(|e| {
                MemoryError::Io {
                    path: path.clone(),
                    source: e,
                }
            })?
        else {
            return Ok(String::new());
        };
        let lines = count_lines(&content);
        let limit = Tier::Hot.max_lines().unwrap_or(usize::MAX);
        if lines > limit {
            return Err(MemoryError::TierOverflow {
                tier: "hot".to_owned(),
                observed_lines: lines,
                limit_lines: limit,
            });
        }
        Ok(content)
    }

    fn recall<'a>(&self, query: RecallQuery<'a>) -> MemoryResult<Vec<Entry>> {
        let mut hits = Vec::new();
        let mut tiers = vec![Tier::Warm];
        if query.include_cold {
            tiers.push(Tier::Cold);
        }
        for tier in tiers {
            for (ns, path) in self.list_namespaces(tier)? {
                if let Some(want) = query.namespace {
                    if ns != want && !ns.starts_with(&format!("{want}/")) {
                        continue;
                    }
                }
                let content = fs::read_to_string(&path).map_err(|e| MemoryError::Io {
                    path: path.clone(),
                    source: e,
                })?;
                if !query.keywords.is_empty() {
                    let hay = content.to_lowercase();
                    if !query
                        .keywords
                        .iter()
                        .all(|kw| hay.contains(&kw.to_lowercase()))
                    {
                        continue;
                    }
                }
                hits.push(Entry {
                    namespace: ns,
                    content,
                    tier,
                    last_used: None,
                    usage_count: 0,
                });
                if hits.len() >= query.limit && query.limit > 0 {
                    return Ok(hits);
                }
            }
        }
        Ok(hits)
    }

    fn record(&self, signal: Signal) -> MemoryResult<()> {
        let path = self.signal_file(&signal);
        let _guard = self.lock.lock().map_err(|_| MemoryError::LockContention {
            attempted: "record",
        })?;
        let line = serde_json::to_string(&signal).map_err(|e| MemoryError::Serde {
            context: "record_serialize",
            source: e,
        })?;
        reject_symlink(&path).map_err(|e| MemoryError::Io {
            path: path.clone(),
            source: e,
        })?;
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        let mut file =
            open_without_following_symlinks(&mut options, &path).map_err(|e| MemoryError::Io {
                path: path.clone(),
                source: e,
            })?;
        writeln!(file, "{line}").map_err(|e| MemoryError::Io {
            path: path.clone(),
            source: e,
        })?;
        file.sync_all().map_err(|e| MemoryError::Io {
            path: path.clone(),
            source: e,
        })?;
        tracing::debug!(kind = signal.kind_label(), "memory: signal recorded");
        Ok(())
    }

    fn maintain(&self) -> MemoryResult<MaintenanceReport> {
        let _guard = self.lock.lock().map_err(|_| MemoryError::LockContention {
            attempted: "maintain",
        })?;
        let owner = format!("pid-{}", std::process::id());
        // Wiederanlauf: bestehenden Marker inspizieren.
        let mut marker = match self.read_workflow()? {
            Some(m) if m.step != WorkflowStep::DbCommitted => {
                tracing::info!(
                    job = %m.job_id,
                    step = m.step.as_str(),
                    "memory.maintain: resuming previous workflow"
                );
                m
            }
            _ => WorkflowMarker::idle(
                format!("job-{}", OffsetDateTime::now_utc().unix_timestamp_nanos()),
                owner,
            ),
        };

        // Claimed
        if marker.step == WorkflowStep::Idle {
            marker.advance();
            self.write_workflow(&marker)?;
        }

        // WorkspaceSynced — offene Signale zählen. Sie bleiben bis nach dem
        // Heartbeat offen, damit dessen Promotionsentscheidung diesen Lauf
        // tatsächlich sieht.
        let mut state = self.read_state()?;
        let signals_before = state.signals_seen;
        let pending = self.pending_signal_count()?;
        if marker.step == WorkflowStep::Claimed {
            marker.advance();
            self.write_workflow(&marker)?;
        }

        // AgentCompleted — M1: keine LLM-Konsolidierung, nur passieren.
        if marker.step == WorkflowStep::WorkspaceSynced {
            marker.advance();
            self.write_workflow(&marker)?;
        }

        // BaselineCommitted — M1: rein die Signal-Buchführung committen.
        // (Regel-basierte Promotion/Decay kommt in M3; Milestone-Doku.)
        if marker.step == WorkflowStep::AgentCompleted {
            marker.advance();
            self.write_workflow(&marker)?;
        }

        // Heartbeat-Tick: regelbasierte Promotion/Demotion nach den Konventionen
        // aus `docs/design/memory-v2.md` §6. Muss innerhalb des `_guard`-Locks
        // laufen, damit `stats()` einen kohärenten Snapshot sieht.
        let cfg = crate::heartbeat::HeartbeatConfig::default();
        let demoted = self.demote_hot_overflow(cfg.hot_max_lines)?;
        let promoted = self.promote_pattern_hints(cfg.promote_threshold)?;
        let mut hb = crate::heartbeat::tick(self, OffsetDateTime::now_utc(), cfg)?;
        hb.promoted = promoted;
        hb.demoted = demoted;

        // DbCommitted: only mark signals seen after their promotion decision
        // and durable tier mutations have completed.
        state.signals_seen = signals_before.saturating_add(pending as u64);
        state.demoted_to_cold = state.demoted_to_cold.saturating_add(demoted as u64);
        state.warm_created = state.warm_created.saturating_add(promoted as u64);
        state.last_maintenance = Some(OffsetDateTime::now_utc());
        self.write_state(&state)?;
        marker.advance();
        self.write_workflow(&marker)?;
        tracing::debug!(
            promoted = hb.promoted,
            demoted = hb.demoted,
            archived = hb.archived,
            hot_lines_after = hb.hot_lines_after,
            "memory.maintain: heartbeat tick"
        );

        Ok(MaintenanceReport {
            signals_processed: pending,
            promoted_to_hot: 0,
            demoted_to_cold: hb.demoted,
            warm_created: promoted,
            heartbeat: Some(hb),
        })
    }

    fn stats(&self) -> MemoryResult<Stats> {
        let hot_path = self.hot_path();
        let hot_content = match read_optional_string_without_following_symlinks(&hot_path) {
            Ok(Some(content)) => content,
            Ok(None) => String::new(),
            Err(e) => {
                return Err(MemoryError::Io {
                    path: hot_path.clone(),
                    source: e,
                });
            }
        };
        let warm = self.list_namespaces(Tier::Warm)?;
        let mut warm_total_lines = 0;
        for (_, path) in &warm {
            let content = fs::read_to_string(path).map_err(|e| MemoryError::Io {
                path: path.clone(),
                source: e,
            })?;
            warm_total_lines += count_lines(&content);
        }
        let cold = self.list_namespaces(Tier::Cold)?;
        let pending = self.pending_signal_count()?;
        Ok(Stats {
            hot_lines: count_lines(&hot_content),
            warm_namespaces: warm.len(),
            warm_total_lines,
            cold_namespaces: cold.len(),
            pending_signals: pending,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("harw-file-store-{tag}-{}-{id}", std::process::id()))
    }

    #[test]
    fn maintain_counts_pending_before_promoting_pattern_hints() {
        let root = tmp_root("promote");
        let store = FileMemoryStore::open(&root).unwrap();
        for note in ["first", "second", "third"] {
            store
                .record(Signal::PatternHint {
                    key: "greeting".to_owned(),
                    note: note.to_owned(),
                })
                .unwrap();
        }

        let report = store.maintain().unwrap();
        assert_eq!(report.signals_processed, 3);
        assert_eq!(
            report
                .heartbeat
                .as_ref()
                .map(|heartbeat| heartbeat.promoted),
            Some(1)
        );
        assert_eq!(report.warm_created, 1);
        assert_eq!(
            fs::read_to_string(root.join("warm/domain/greeting.md")).unwrap(),
            "third\n"
        );
        assert_eq!(store.stats().unwrap().pending_signals, 0);

        let second = store.maintain().unwrap();
        assert_eq!(second.signals_processed, 0);
        assert_eq!(second.warm_created, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn maintain_demotes_hot_overflow_into_warm() {
        let root = tmp_root("demote");
        let store = FileMemoryStore::open(&root).unwrap();
        let hot = (0..102)
            .map(|line| format!("line-{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(root.join("HOT.md"), format!("{hot}\n")).unwrap();

        let report = store.maintain().unwrap();
        assert_eq!(
            report.heartbeat.as_ref().map(|heartbeat| heartbeat.demoted),
            Some(2)
        );
        assert_eq!(store.stats().unwrap().hot_lines, 100);
        assert_eq!(
            fs::read_to_string(root.join("warm/inactive.md")).unwrap(),
            "line-0\nline-1\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stats_counts_hot_overflow_without_relaxing_hot() {
        let root = tmp_root("stats-overflow");
        let store = FileMemoryStore::open(&root).unwrap();
        let hot = (0..101)
            .map(|line| format!("line-{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(root.join("HOT.md"), format!("{hot}\n")).unwrap();

        assert!(matches!(
            store.hot(),
            Err(MemoryError::TierOverflow {
                tier,
                observed_lines: 101,
                limit_lines: 100,
            }) if tier == "hot"
        ));
        assert_eq!(store.stats().unwrap().hot_lines, 101);

        let report = store.maintain().unwrap();
        assert_eq!(
            report.heartbeat.as_ref().map(|heartbeat| heartbeat.demoted),
            Some(1)
        );
        assert_eq!(store.stats().unwrap().hot_lines, 100);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn namespace_discovery_skips_symlinked_files_and_directories() {
        use std::os::unix::fs::symlink;

        let root = tmp_root("symlink-guard");
        let outside = tmp_root("symlink-target");
        let store = FileMemoryStore::open(&root).unwrap();

        fs::create_dir_all(root.join("warm/nested")).unwrap();
        fs::write(root.join("warm/nested/kept.md"), "warm content\n").unwrap();
        fs::write(root.join("cold/kept.md"), "cold content\n").unwrap();

        fs::create_dir_all(outside.join("directory")).unwrap();
        fs::write(outside.join("secret.md"), "outside file\n").unwrap();
        fs::write(outside.join("directory/secret.md"), "outside directory\n").unwrap();
        symlink(outside.join("secret.md"), root.join("warm/linked.md")).unwrap();
        symlink(outside.join("directory"), root.join("cold/linked")).unwrap();

        let entries = store
            .recall(RecallQuery {
                namespace: None,
                keywords: &[],
                include_cold: true,
                limit: 10,
            })
            .unwrap();
        let namespaces: Vec<_> = entries
            .iter()
            .map(|entry| entry.namespace.as_str())
            .collect();
        assert_eq!(namespaces, ["nested/kept", "kept"]);
        assert!(
            entries
                .iter()
                .all(|entry| !entry.content.contains("outside"))
        );

        let stats = store.stats().unwrap();
        assert_eq!(stats.warm_namespaces, 1);
        assert_eq!(stats.cold_namespaces, 1);

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn namespace_discovery_skips_a_symlinked_tier_root() {
        use std::os::unix::fs::symlink;

        let root = tmp_root("symlinked-tier-root");
        let outside = tmp_root("symlinked-tier-target");
        let store = FileMemoryStore::open(&root).unwrap();

        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.md"), "outside warm content\n").unwrap();
        fs::remove_dir(root.join("warm")).unwrap();
        symlink(&outside, root.join("warm")).unwrap();
        fs::write(root.join("cold/kept.md"), "cold content\n").unwrap();

        let entries = store
            .recall(RecallQuery {
                namespace: None,
                keywords: &[],
                include_cold: true,
                limit: 10,
            })
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].namespace, "kept");
        assert_eq!(entries[0].tier, Tier::Cold);
        assert_eq!(store.stats().unwrap().warm_namespaces, 0);

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn fixed_authority_files_reject_symlink_indirection() {
        use std::os::unix::fs::symlink;

        let root = tmp_root("fixed-file-symlinks");
        let outside = tmp_root("fixed-file-targets");
        let store = FileMemoryStore::open(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let hot_target = outside.join("hot.md");
        let state_target = outside.join("state.json");
        let workflow_target = outside.join("workflow.json");
        let signal_target = outside.join("corrections.jsonl");
        for target in [&hot_target, &state_target, &workflow_target, &signal_target] {
            fs::write(target, "outside authority\n").unwrap();
        }
        symlink(&hot_target, root.join("HOT.md")).unwrap();
        symlink(&state_target, root.join("state.json")).unwrap();
        symlink(&workflow_target, root.join("workflow.json")).unwrap();
        symlink(&signal_target, root.join("signals/corrections.jsonl")).unwrap();

        assert!(matches!(
            store.hot(),
            Err(MemoryError::Io { path, .. }) if path == root.join("HOT.md")
        ));
        assert!(matches!(
            store.stats(),
            Err(MemoryError::Io { path, .. }) if path == root.join("HOT.md")
        ));
        assert!(matches!(
            atomic_write(&root.join("HOT.md"), b"replacement\n"),
            Err(MemoryError::Io { path, .. }) if path == root.join("HOT.md")
        ));
        assert!(matches!(
            store.read_state(),
            Err(MemoryError::Io { path, .. }) if path == root.join("state.json")
        ));
        assert!(matches!(
            store.write_state(&PersistedState::default()),
            Err(MemoryError::Io { path, .. }) if path == root.join("state.json")
        ));
        assert!(matches!(
            store.read_workflow(),
            Err(MemoryError::Io { path, .. }) if path == root.join("workflow.json")
        ));
        assert!(matches!(
            store.write_workflow(&WorkflowMarker::idle("job", "test")),
            Err(MemoryError::Io { path, .. }) if path == root.join("workflow.json")
        ));
        assert!(matches!(
            store.record(Signal::Correction {
                text: "correction".to_owned(),
                context: None,
            }),
            Err(MemoryError::Io { path, .. }) if path == root.join("signals/corrections.jsonl")
        ));
        assert!(matches!(
            store.pending_signal_count(),
            Err(MemoryError::Io { path, .. }) if path == root.join("signals/corrections.jsonl")
        ));

        for target in [&hot_target, &state_target, &workflow_target, &signal_target] {
            assert_eq!(fs::read_to_string(target).unwrap(), "outside authority\n");
        }

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
}
