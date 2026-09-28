//! Durable topology store: one JSON state file, atomically replaced.
//!
//! # Layout
//! ```text
//! <state_dir>/            mode 0700, created if missing, never a symlink
//!     state.json          the topology (mode 0600, written via temp + rename)
//!     state.lock          single-writer lock (fs4, held for the store's life)
//! ```
//!
//! # Durability
//! Every mutation runs on a **copy** of the in-memory state. The copy is
//! serialised to a fresh temp file in the same directory (`tempfile`, mode
//! `0600`), the temp file is `fsync`ed and renamed over `state.json`. That
//! rename is the commit point: once it returns without error, the new state
//! is the durable truth on disk. A failure *before* the rename (serialising,
//! creating or writing the temp file) leaves the old complete file on disk
//! *and* the old state in memory, unchanged — the daemon never serves a
//! state it could not persist, and the caller may retry the same mutation.
//! The directory itself is `fsync`ed *after* the rename, to make the new
//! directory entry survive a crash; the in-memory state is updated before
//! that call, not after, so a directory-fsync failure can never make memory
//! run ahead of a write that in fact did not happen, nor can it make the
//! caller believe a committed write was lost. Such a failure is still
//! reported as an error (so operators see it, and the API answers it with
//! the code `durability_unconfirmed` rather than `internal`), but it does
//! not roll back the in-memory state and a retry of the same logical change
//! must not be treated as a no-op. Same contract as `harw-job-store::fsops` /
//! `harw-session-store::durability`, reimplemented here because those crates
//! carry session/job semantics this daemon must not depend on.
//!
//! # Corruption
//! A state file that is unreadable, not valid JSON, carries an unknown
//! field, a wrong format tag or version, or is internally inconsistent
//! (duplicate ids, dangling zone/node references) is rejected with
//! [`NetsecError::StoreCorrupt`]. The daemon refuses to start; it never
//! resets topology silently.
//!
//! # Concurrency
//! A `std::sync::Mutex` serialises mutations inside the process; the fs4
//! lock serialises processes. All methods are blocking — async callers run
//! them on `spawn_blocking`.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fs4::FileExt;
use harw_types::NodeId;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{NetsecError, NetsecResult};
use crate::ids::RouteId;
use crate::model::{NodeRecord, NodeRegistration, Route, TransitionRecord, Zone};
use crate::state_machine::{NodeEvent, NodeState, next_state};

/// File name of the state file inside the state directory.
pub const STATE_FILE: &str = "state.json";
/// File name of the single-writer lock inside the state directory.
pub const LOCK_FILE: &str = "state.lock";
/// Format tag written into every state file.
pub const STATE_FORMAT: &str = "harw-netsec.state";
/// Current state file version.
pub const STATE_VERSION: u32 = 1;
/// Upper bound for a state file that will be read at all.
pub const MAX_STATE_BYTES: u64 = 32 * 1024 * 1024;
/// `context` of the [`NetsecError::Io`] a mutation returns when its rename
/// already committed the new state file but the directory fsync after it
/// failed: the change is applied, only its survival across a crash is
/// unconfirmed. `crate::server` matches on this exact value to answer
/// `durability_unconfirmed` instead of the generic `internal`, so this is
/// the only place the text may be spelled out.
pub(crate) const DIR_FSYNC_CONTEXT: &str = "fsync state directory (state file already replaced)";

/// On-disk form of the state.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateFile {
    format: String,
    version: u32,
    zones: Vec<Zone>,
    nodes: Vec<NodeRecord>,
    routes: Vec<Route>,
}

/// In-memory topology.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkState {
    zones: BTreeMap<String, Zone>,
    nodes: BTreeMap<String, NodeRecord>,
    routes: BTreeMap<String, Route>,
}

impl NetworkState {
    /// All nodes, ordered by id.
    pub fn nodes(&self) -> impl Iterator<Item = &NodeRecord> {
        self.nodes.values()
    }

    /// One node by id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&NodeRecord> {
        self.nodes.get(id)
    }

    /// All zones, ordered by id.
    pub fn zones(&self) -> impl Iterator<Item = &Zone> {
        self.zones.values()
    }

    /// All routes, ordered by id.
    pub fn routes(&self) -> impl Iterator<Item = &Route> {
        self.routes.values()
    }

    /// Number of registered nodes (any state, including revoked).
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn from_file(file: StateFile) -> Result<Self, String> {
        if file.format != STATE_FORMAT {
            return Err(format!("unexpected format tag {:?}", file.format));
        }
        if file.version != STATE_VERSION {
            return Err(format!("unsupported version {}", file.version));
        }
        let mut state = Self::default();
        for zone in file.zones {
            zone.validate().map_err(|error| error.to_string())?;
            let key = zone.id.as_str().to_owned();
            if state.zones.insert(key.clone(), zone).is_some() {
                return Err(format!("duplicate zone {key:?}"));
            }
        }
        for node in file.nodes {
            node.validate().map_err(|error| error.to_string())?;
            if !state.zones.contains_key(node.zone.as_str()) {
                return Err(format!(
                    "node {:?} references unknown zone",
                    node.id.as_str()
                ));
            }
            let key = node.id.as_str().to_owned();
            if state.nodes.insert(key.clone(), node).is_some() {
                return Err(format!("duplicate node {key:?}"));
            }
        }
        for route in file.routes {
            state
                .check_route(&route)
                .map_err(|error| format!("route {:?}: {error}", route.id.as_str()))?;
            let key = route.id.as_str().to_owned();
            if state.routes.insert(key.clone(), route).is_some() {
                return Err(format!("duplicate route {key:?}"));
            }
        }
        Ok(state)
    }

    fn to_file(&self) -> StateFile {
        StateFile {
            format: STATE_FORMAT.to_owned(),
            version: STATE_VERSION,
            zones: self.zones.values().cloned().collect(),
            nodes: self.nodes.values().cloned().collect(),
            routes: self.routes.values().cloned().collect(),
        }
    }

    fn check_route(&self, route: &Route) -> NetsecResult<()> {
        if !self.zones.contains_key(route.zone.as_str()) {
            return Err(NetsecError::UnknownZone {
                zone: route.zone.as_str().to_owned(),
            });
        }
        let live = |id: &NodeId| {
            self.nodes
                .get(id.as_str())
                .is_some_and(|node| node.state != NodeState::Revoked)
        };
        if !live(&route.target) {
            return Err(NetsecError::InvalidRoute {
                reason: "target node is unknown or revoked",
            });
        }
        if let Some(via) = &route.via {
            if via == &route.target {
                return Err(NetsecError::InvalidRoute {
                    reason: "relay equals target",
                });
            }
            if !live(via) {
                return Err(NetsecError::InvalidRoute {
                    reason: "relay node is unknown or revoked",
                });
            }
        }
        Ok(())
    }
}

/// The durable topology store.
#[derive(Debug)]
pub struct NetsecStore {
    dir: PathBuf,
    path: PathBuf,
    max_nodes: usize,
    state: Mutex<NetworkState>,
    /// Held for the store's lifetime; dropping it releases the fs4 lock.
    _lock: File,
}

impl NetsecStore {
    /// Opens (or creates) the store in `dir`.
    ///
    /// # Description
    /// Creates `dir` with mode `0700` if missing, takes the single-writer
    /// lock and loads `state.json` (an absent file is an empty topology).
    ///
    /// # Errors
    /// [`NetsecError::SymlinkRejected`], [`NetsecError::StoreLocked`],
    /// [`NetsecError::StoreTooLarge`], [`NetsecError::StoreCorrupt`] or
    /// [`NetsecError::Io`].
    pub fn open(dir: &Path, max_nodes: usize) -> NetsecResult<Self> {
        reject_symlink(dir)?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(NetsecError::io("create state directory"))?;
        let lock_path = dir.join(LOCK_FILE);
        reject_symlink(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(NetsecError::io("open state lock"))?;
        FileExt::try_lock(&lock).map_err(|error| match error {
            fs4::TryLockError::WouldBlock => NetsecError::StoreLocked {
                path: lock_path.clone(),
            },
            fs4::TryLockError::Error(source) => NetsecError::Io {
                context: "lock state",
                source,
            },
        })?;
        let path = dir.join(STATE_FILE);
        let state = load(&path)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            path,
            max_nodes,
            state: Mutex::new(state),
            _lock: lock,
        })
    }

    /// Path of the state file.
    #[must_use]
    pub fn state_path(&self) -> &Path {
        &self.path
    }

    /// A consistent copy of the current topology.
    ///
    /// # Errors
    /// [`NetsecError::Poisoned`].
    pub fn snapshot(&self) -> NetsecResult<NetworkState> {
        self.state
            .lock()
            .map(|state| state.clone())
            .map_err(|_| NetsecError::Poisoned)
    }

    /// One node by id.
    ///
    /// # Errors
    /// [`NetsecError::NodeNotFound`] or [`NetsecError::Poisoned`].
    pub fn node(&self, id: &NodeId) -> NetsecResult<NodeRecord> {
        let state = self.state.lock().map_err(|_| NetsecError::Poisoned)?;
        state
            .node(id.as_str())
            .cloned()
            .ok_or_else(|| NetsecError::NodeNotFound {
                id: id.as_str().to_owned(),
            })
    }

    /// Inserts or updates the configured zones (never removes one, since
    /// nodes and routes may still reference it).
    ///
    /// # Errors
    /// Validation or persistence errors.
    pub fn ensure_zones(&self, zones: &[Zone]) -> NetsecResult<()> {
        self.mutate(|state| {
            for zone in zones {
                zone.validate()?;
                state
                    .zones
                    .insert(zone.id.as_str().to_owned(), zone.clone());
            }
            Ok(())
        })
    }

    /// Registers a new node in [`NodeState::Pending`].
    ///
    /// # Errors
    /// [`NetsecError::NodeExists`], [`NetsecError::UnknownZone`],
    /// [`NetsecError::CapacityExhausted`], validation or persistence errors.
    pub fn register_node(
        &self,
        registration: NodeRegistration,
        now: Timestamp,
    ) -> NetsecResult<NodeRecord> {
        let record = registration.into_record(now)?;
        let max_nodes = self.max_nodes;
        self.mutate(move |state| {
            if state.nodes.contains_key(record.id.as_str()) {
                return Err(NetsecError::NodeExists {
                    id: record.id.as_str().to_owned(),
                });
            }
            if !state.zones.contains_key(record.zone.as_str()) {
                return Err(NetsecError::UnknownZone {
                    zone: record.zone.as_str().to_owned(),
                });
            }
            if state.nodes.len() >= max_nodes {
                return Err(NetsecError::CapacityExhausted { limit: max_nodes });
            }
            state
                .nodes
                .insert(record.id.as_str().to_owned(), record.clone());
            Ok(record)
        })
    }

    /// Applies a state-machine event to a node.
    ///
    /// # Description
    /// The target state comes exclusively from
    /// [`crate::state_machine::next_state`]. Revoking a node also removes
    /// every route that targets it or relays through it.
    ///
    /// # Errors
    /// [`NetsecError::NodeNotFound`], [`NetsecError::InvalidTransition`],
    /// validation or persistence errors.
    pub fn apply_event(
        &self,
        id: &NodeId,
        event: NodeEvent,
        now: Timestamp,
        by_uid: Option<u32>,
        reason: Option<String>,
    ) -> NetsecResult<NodeRecord> {
        if let Some(reason) = &reason {
            crate::model::check_text("reason", reason, crate::model::MAX_REASON_CHARS)?;
        }
        self.mutate(move |state| {
            let node =
                state
                    .nodes
                    .get_mut(id.as_str())
                    .ok_or_else(|| NetsecError::NodeNotFound {
                        id: id.as_str().to_owned(),
                    })?;
            let from = node.state;
            let to = next_state(from, event)?;
            node.state = to;
            node.last_transition = Some(TransitionRecord {
                from,
                to,
                event,
                at: now,
                by_uid,
                reason,
            });
            let record = node.clone();
            if to == NodeState::Revoked {
                state.routes.retain(|_, route| !route.references(id));
            }
            Ok(record)
        })
    }

    /// Inserts or replaces a route.
    ///
    /// # Errors
    /// [`NetsecError::UnknownZone`], [`NetsecError::InvalidRoute`] or
    /// persistence errors.
    pub fn upsert_route(&self, route: Route) -> NetsecResult<()> {
        self.mutate(move |state| {
            state.check_route(&route)?;
            state.routes.insert(route.id.as_str().to_owned(), route);
            Ok(())
        })
    }

    /// Removes a route; returns whether it existed.
    ///
    /// # Errors
    /// Persistence errors.
    pub fn remove_route(&self, id: &RouteId) -> NetsecResult<bool> {
        self.mutate(|state| Ok(state.routes.remove(id.as_str()).is_some()))
    }

    /// Runs `change` on a copy of the state; persists and publishes the copy
    /// only if `change` succeeded and actually changed something.
    ///
    /// The rename inside [`write_and_rename`] is the commit point: on
    /// success the in-memory state is updated to match *before* the
    /// directory fsync runs, so a directory-fsync failure is reported to
    /// the caller without leaving memory behind a write that already
    /// happened on disk (see the module doc).
    fn mutate<T>(
        &self,
        change: impl FnOnce(&mut NetworkState) -> NetsecResult<T>,
    ) -> NetsecResult<T> {
        let mut current = self.state.lock().map_err(|_| NetsecError::Poisoned)?;
        let mut next = current.clone();
        let output = change(&mut next)?;
        if next != *current {
            write_and_rename(&self.dir, &self.path, &next)?;
            *current = next;
            fsync_state_dir(&self.dir)?;
        }
        Ok(output)
    }
}

/// Fails with [`NetsecError::SymlinkRejected`] if `path` is a symlink.
fn reject_symlink(path: &Path) -> NetsecResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(NetsecError::SymlinkRejected {
            path: path.to_path_buf(),
        }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(NetsecError::Io {
            context: "inspect path",
            source,
        }),
    }
}

fn load(path: &Path) -> NetsecResult<NetworkState> {
    reject_symlink(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NetworkState::default());
        }
        Err(source) => {
            return Err(NetsecError::Io {
                context: "open state file",
                source,
            });
        }
    };
    let metadata = file
        .metadata()
        .map_err(NetsecError::io("inspect state file"))?;
    let corrupt = |reason: String| NetsecError::StoreCorrupt {
        path: path.to_path_buf(),
        reason,
    };
    if !metadata.is_file() {
        return Err(corrupt("not a regular file".to_owned()));
    }
    if metadata.len() > MAX_STATE_BYTES {
        return Err(NetsecError::StoreTooLarge {
            path: path.to_path_buf(),
            limit: MAX_STATE_BYTES,
        });
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(NetsecError::io("read state file"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STATE_BYTES {
        return Err(NetsecError::StoreTooLarge {
            path: path.to_path_buf(),
            limit: MAX_STATE_BYTES,
        });
    }
    let file: StateFile =
        serde_json::from_slice(&bytes).map_err(|error| corrupt(error.to_string()))?;
    NetworkState::from_file(file).map_err(corrupt)
}

/// Serialises `state` to a fresh temp file and renames it over `path`.
///
/// The rename is the commit point of a mutation: an error returned by this
/// function means the rename was never reached (or the OS rejected it), so
/// `path` still holds the previous complete file and the caller's in-memory
/// state must stay unchanged. Once this function returns `Ok`, the new file
/// is durably the one at `path`; only the containing directory entry may
/// still not survive a crash until [`fsync_state_dir`] also succeeds.
fn write_and_rename(dir: &Path, path: &Path, state: &NetworkState) -> NetsecResult<()> {
    let mut bytes =
        serde_json::to_vec_pretty(&state.to_file()).map_err(|error| NetsecError::Io {
            context: "serialize state",
            source: std::io::Error::other(error),
        })?;
    bytes.push(b'\n');
    reject_symlink(path)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".state.")
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(NetsecError::io("create temp state file"))?;
    temp.write_all(&bytes)
        .map_err(NetsecError::io("write temp state file"))?;
    temp.as_file()
        .sync_all()
        .map_err(NetsecError::io("fsync temp state file"))?;
    temp.persist(path).map_err(|error| NetsecError::Io {
        context: "rename state file",
        source: error.error,
    })?;
    Ok(())
}

/// Fsyncs `dir` so a rename that already completed survives a crash.
///
/// Must only be called after [`write_and_rename`] has already succeeded for
/// the same directory. The caller has therefore already committed the new
/// state to memory by the time this runs; a failure here is reported as
/// [`NetsecError::Io`] with context [`DIR_FSYNC_CONTEXT`] so operators and
/// clients see it, but it must never cause the caller to revert or repeat
/// that commit — the write on disk already happened.
fn fsync_state_dir(dir: &Path) -> NetsecResult<()> {
    #[cfg(test)]
    if FAIL_DIR_FSYNC.with(std::cell::Cell::get) {
        return Err(NetsecError::Io {
            context: DIR_FSYNC_CONTEXT,
            source: std::io::Error::other("injected for test"),
        });
    }
    File::open(dir)
        .and_then(|directory| directory.sync_all())
        .map_err(NetsecError::io(DIR_FSYNC_CONTEXT))
}

#[cfg(test)]
thread_local! {
    /// Test-only fault injection for [`fsync_state_dir`]. Thread-local so
    /// concurrently running tests never see each other's setting.
    static FAIL_DIR_FSYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ZoneId;
    use crate::test_support::{TestError, TestResult};

    fn ts() -> TestResult<Timestamp> {
        Timestamp::from_second(1_790_000_000).map_err(crate::test_support::ctx("timestamp"))
    }

    fn zone(id: &str) -> TestResult<Zone> {
        Ok(Zone {
            id: ZoneId::parse(id)?,
            display_name: format!("zone {id}"),
        })
    }

    fn registration(id: &str) -> TestResult<NodeRegistration> {
        Ok(NodeRegistration {
            id: crate::ids::parse_node_id(id)?,
            display_name: format!("node {id}"),
            addresses: vec!["10.0.0.7:7443".parse()?],
            zone: ZoneId::parse("local")?,
            identity_key_ref: Some("authhub:node/7".to_owned()),
        })
    }

    fn open_with_zone(dir: &Path) -> TestResult<NetsecStore> {
        let store = NetsecStore::open(dir, 16)?;
        store.ensure_zones(&[zone("local")?])?;
        Ok(store)
    }

    fn temp_files(dir: &Path) -> TestResult<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                names.push(name);
            }
        }
        Ok(names)
    }

    #[test]
    fn test_empty_directory_opens_as_empty_topology() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = NetsecStore::open(&temp.path().join("state"), 16)?;
        assert_eq!(store.snapshot()?.node_count(), 0);
        assert!(
            !store.state_path().exists(),
            "nothing written until a change"
        );
        Ok(())
    }

    #[test]
    fn test_state_survives_reopen() -> TestResult {
        let temp = tempfile::tempdir()?;
        {
            let store = open_with_zone(temp.path())?;
            store.register_node(registration("n1")?, ts()?)?;
            let id = crate::ids::parse_node_id("n1")?;
            store.apply_event(&id, NodeEvent::Activate, ts()?, Some(1000), None)?;
        }
        let store = NetsecStore::open(temp.path(), 16)?;
        let snapshot = store.snapshot()?;
        let node = snapshot.node("n1").ok_or(TestError::Missing("node n1"))?;
        assert_eq!(node.state, NodeState::Active);
        assert_eq!(
            node.last_transition.as_ref().and_then(|t| t.by_uid),
            Some(1000)
        );
        assert_eq!(snapshot.zones().count(), 1);
        Ok(())
    }

    #[test]
    fn test_failed_write_leaves_memory_and_disk_unchanged() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = open_with_zone(temp.path())?;
        store.register_node(registration("n1")?, ts()?)?;
        let before_disk = std::fs::read(store.state_path())?;
        // Replace the state file with a directory: the rename in
        // `write_and_rename` must fail (works even when the tests run as
        // root).
        std::fs::remove_file(store.state_path())?;
        std::fs::create_dir(store.state_path())?;

        let result = store.register_node(registration("n2")?, ts()?);
        assert!(matches!(result, Err(NetsecError::Io { .. })), "{result:?}");
        let snapshot = store.snapshot()?;
        assert_eq!(
            snapshot.node_count(),
            1,
            "memory must not run ahead of disk"
        );
        assert!(temp_files(temp.path())?.is_empty(), "temp file cleaned up");

        std::fs::remove_dir(store.state_path())?;
        std::fs::write(store.state_path(), &before_disk)?;
        store.register_node(registration("n2")?, ts()?)?;
        assert_eq!(store.snapshot()?.node_count(), 2);
        Ok(())
    }

    /// Sets the directory-fsync fault while `body` runs, and always clears
    /// it again afterwards (even if `body` returns early or panics), so
    /// this test can never leak the fault into another test on the same
    /// worker thread.
    fn with_failed_dir_fsync<T>(body: impl FnOnce() -> T) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                FAIL_DIR_FSYNC.with(|flag| flag.set(false));
            }
        }
        FAIL_DIR_FSYNC.with(|flag| flag.set(true));
        let _reset = Reset;
        body()
    }

    #[test]
    fn test_directory_fsync_failure_after_rename_still_commits_to_memory() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = open_with_zone(temp.path())?;
        store.register_node(registration("n1")?, ts()?)?;

        // The rename itself succeeds; only the following directory fsync is
        // made to fail. `mutate` must publish the new state to memory
        // regardless, because the rename already made it durable on disk —
        // reverting memory here would make a later mutation silently
        // overwrite the change this call reported as failed.
        let registration_n2 = registration("n2")?;
        let now = ts()?;
        let result = with_failed_dir_fsync(|| store.register_node(registration_n2, now));
        assert!(matches!(result, Err(NetsecError::Io { .. })), "{result:?}");

        let snapshot = store.snapshot()?;
        assert_eq!(
            snapshot.node_count(),
            2,
            "the rename already committed the write"
        );
        let on_disk = load(store.state_path())?;
        assert_eq!(
            on_disk, snapshot,
            "disk and memory must agree on the new state"
        );

        // The fault is scoped to the closure above; a later mutation must
        // succeed normally and must not be treated as a retry of the
        // "failed" one.
        store.register_node(registration("n3")?, ts()?)?;
        assert_eq!(store.snapshot()?.node_count(), 3);
        Ok(())
    }

    /// The error the store really returns for a failed directory fsync must
    /// reach the client as `durability_unconfirmed`, not `internal`: the
    /// store and the server mapping share [`DIR_FSYNC_CONTEXT`], and this
    /// test fails if either side stops using it.
    #[test]
    fn test_directory_fsync_failure_reaches_client_as_durability_unconfirmed() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = open_with_zone(temp.path())?;
        let registration_n1 = registration("n1")?;
        let now = ts()?;
        let error = match with_failed_dir_fsync(|| store.register_node(registration_n1, now)) {
            Err(error) => error,
            Ok(record) => {
                return Err(TestError::Unexpected(format!(
                    "expected an error, got {record:?}"
                )));
            }
        };
        assert!(
            matches!(
                &error,
                NetsecError::Io {
                    context: DIR_FSYNC_CONTEXT,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            crate::server::ApiError::from(error).code(),
            "durability_unconfirmed"
        );
        Ok(())
    }

    #[test]
    fn test_rejected_mutation_does_not_write() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = open_with_zone(temp.path())?;
        store.register_node(registration("n1")?, ts()?)?;
        let before = std::fs::read(store.state_path())?;
        let id = crate::ids::parse_node_id("n1")?;
        let result = store.apply_event(&id, NodeEvent::Drain, ts()?, None, None);
        assert!(matches!(result, Err(NetsecError::InvalidTransition { .. })));
        assert_eq!(std::fs::read(store.state_path())?, before);
        Ok(())
    }

    fn assert_corrupt(dir: &Path) -> TestResult {
        match NetsecStore::open(dir, 16) {
            Err(NetsecError::StoreCorrupt { .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_truncated_file_is_rejected() -> TestResult {
        let temp = tempfile::tempdir()?;
        {
            let store = open_with_zone(temp.path())?;
            store.register_node(registration("n1")?, ts()?)?;
        }
        let path = temp.path().join(STATE_FILE);
        let bytes = std::fs::read(&path)?;
        std::fs::write(&path, &bytes[..bytes.len() / 2])?;
        assert_corrupt(temp.path())
    }

    #[test]
    fn test_unknown_field_wrong_tag_and_version_are_rejected() -> TestResult {
        let cases = [
            r#"{"format":"harw-netsec.state","version":1,"zones":[],"nodes":[],"routes":[],"extra":1}"#,
            r#"{"format":"other","version":1,"zones":[],"nodes":[],"routes":[]}"#,
            r#"{"format":"harw-netsec.state","version":2,"zones":[],"nodes":[],"routes":[]}"#,
            "",
            "null",
        ];
        for case in cases {
            let temp = tempfile::tempdir()?;
            std::fs::write(temp.path().join(STATE_FILE), case)?;
            assert_corrupt(temp.path())?;
        }
        Ok(())
    }

    #[test]
    fn test_inconsistent_references_are_rejected() -> TestResult {
        let node = registration("n1")?.into_record(ts()?)?;
        let node_json = serde_json::to_string(&node)?;
        let zone_json = serde_json::to_string(&zone("local")?)?;
        let cases = [
            // node in a zone that does not exist
            format!(
                r#"{{"format":"harw-netsec.state","version":1,"zones":[],"nodes":[{node_json}],"routes":[]}}"#
            ),
            // duplicate node
            format!(
                r#"{{"format":"harw-netsec.state","version":1,"zones":[{zone_json}],"nodes":[{node_json},{node_json}],"routes":[]}}"#
            ),
            // route to an unknown node
            format!(
                r#"{{"format":"harw-netsec.state","version":1,"zones":[{zone_json}],"nodes":[],"routes":[{{"id":"r1","target":"ghost","zone":"local","metric":1}}]}}"#
            ),
        ];
        for case in cases {
            let temp = tempfile::tempdir()?;
            std::fs::write(temp.path().join(STATE_FILE), case)?;
            assert_corrupt(temp.path())?;
        }
        Ok(())
    }

    #[test]
    fn test_oversized_file_is_rejected_without_reading() -> TestResult {
        let temp = tempfile::tempdir()?;
        let file = File::create(temp.path().join(STATE_FILE))?;
        file.set_len(MAX_STATE_BYTES + 1)?;
        match NetsecStore::open(temp.path(), 16) {
            Err(NetsecError::StoreTooLarge { .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_symlinked_state_file_is_rejected() -> TestResult {
        let temp = tempfile::tempdir()?;
        let target = temp.path().join("elsewhere.json");
        std::fs::write(&target, "{}")?;
        std::os::unix::fs::symlink(&target, temp.path().join(STATE_FILE))?;
        match NetsecStore::open(temp.path(), 16) {
            Err(NetsecError::SymlinkRejected { .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_second_open_is_locked_out() -> TestResult {
        let temp = tempfile::tempdir()?;
        let _first = NetsecStore::open(temp.path(), 16)?;
        match NetsecStore::open(temp.path(), 16) {
            Err(NetsecError::StoreLocked { .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_register_rules() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = NetsecStore::open(temp.path(), 1)?;
        store.ensure_zones(&[zone("local")?])?;
        store.register_node(registration("n1")?, ts()?)?;
        assert!(matches!(
            store.register_node(registration("n1")?, ts()?),
            Err(NetsecError::NodeExists { .. })
        ));
        assert!(matches!(
            store.register_node(registration("n2")?, ts()?),
            Err(NetsecError::CapacityExhausted { limit: 1 })
        ));
        let mut elsewhere = registration("n3")?;
        elsewhere.zone = ZoneId::parse("dmz")?;
        let store2_dir = tempfile::tempdir()?;
        let store2 = open_with_zone(store2_dir.path())?;
        assert!(matches!(
            store2.register_node(elsewhere, ts()?),
            Err(NetsecError::UnknownZone { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_revoke_prunes_routes_through_the_node() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = open_with_zone(temp.path())?;
        store.register_node(registration("a")?, ts()?)?;
        store.register_node(registration("b")?, ts()?)?;
        store.register_node(registration("c")?, ts()?)?;
        let a = crate::ids::parse_node_id("a")?;
        let b = crate::ids::parse_node_id("b")?;
        let c = crate::ids::parse_node_id("c")?;
        let route = |id: &str, target: &NodeId, via: Option<&NodeId>| -> TestResult<Route> {
            Ok(Route {
                id: RouteId::parse(id)?,
                target: target.clone(),
                zone: ZoneId::parse("local")?,
                via: via.cloned(),
                metric: 10,
            })
        };
        store.upsert_route(route("to-a-via-b", &a, Some(&b))?)?;
        store.upsert_route(route("to-c", &c, None)?)?;
        assert!(matches!(
            store.upsert_route(route("loop", &a, Some(&a))?),
            Err(NetsecError::InvalidRoute { .. })
        ));

        store.apply_event(
            &b,
            NodeEvent::Revoke,
            ts()?,
            None,
            Some("decommissioned".to_owned()),
        )?;
        let remaining: Vec<String> = store
            .snapshot()?
            .routes()
            .map(|route| route.id.as_str().to_owned())
            .collect();
        assert_eq!(remaining, vec!["to-c".to_owned()]);
        assert!(matches!(
            store.upsert_route(route("to-b", &b, None)?),
            Err(NetsecError::InvalidRoute { .. })
        ));
        assert!(store.remove_route(&RouteId::parse("to-c")?)?);
        assert!(!store.remove_route(&RouteId::parse("to-c")?)?);
        Ok(())
    }

    #[test]
    fn test_state_file_is_private() -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let store = open_with_zone(temp.path())?;
        let mode = std::fs::metadata(store.state_path())?.permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
        Ok(())
    }
}
