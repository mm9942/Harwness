//! Peer bridge: file-based mailbox between external peer agents and live
//! harw TUI sessions.
//!
//! Layout below `<profile_dir>/peer-bridge/` (all directories 0700, all files
//! 0600, every open symlink-safe via `harw_fsutil::open_nofollow`):
//!
//! - `inbox/<session>.jsonl`: queue of [`PeerInboundMessage`] lines. The serve
//!   side appends ([`append_inbound`]), the TUI reads and truncates
//!   ([`PeerInboxHandle::take_new`]).
//! - `inbox/<session>.live`: JSON [`PeerLiveInfo`]. The TUI holds an exclusive
//!   `flock` on it for its whole lifetime; a held lock means "live".
//! - `outbox/<peer>.jsonl` (rotated to `.jsonl.1`, serialized by the sidecar
//!   `.jsonl.lock`): durable outbox of [`PeerOutboundMessage`] records.
//! - `spool/<peer>.jsonl` (rotated to `.1`, sidecar `.lock`): spool lines for
//!   `tail -F` consumers ([`append_spool_line`]).
//!
//! Warnings never contain message text or ids, only byte lengths.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use harw_fsutil::{
    OpenMode, ensure_owned_directory, ensure_private_regular, open_dir_nofollow, open_nofollow,
};
use harw_types::SessionId;
use serde::{Deserialize, Serialize};

use crate::store::safe_session_component;

/// Name of the peer bridge directory below the profile directory.
pub const PEER_BRIDGE_DIR: &str = "peer-bridge";
/// Record version written into every peer bridge record (`v`).
pub const PEER_RECORD_VERSION: u32 = 1;
/// Maximum length of a peer id in bytes.
pub const MAX_PEER_ID_BYTES: usize = 64;
/// Maximum length of a message id in bytes.
pub const MAX_MESSAGE_ID_BYTES: usize = 128;
/// Number of `(from, id)` pairs remembered for inbox deduplication.
pub const PEER_DEDUP_WINDOW: usize = 1024;

/// Upper bound of bytes read from the inbox per [`PeerInboxHandle::take_new`].
const MAX_TAKE_BYTES: u64 = 1 << 20;
/// Upper bound of bytes read from a `.live` file.
const MAX_LIVE_BYTES: u64 = 4096;
/// Chunk size used when counting queued inbox lines.
const COUNT_CHUNK_BYTES: usize = 64 * 1024;

const INBOX_DIR: &str = "inbox";
const OUTBOX_DIR: &str = "outbox";
const SPOOL_DIR: &str = "spool";

/// Paths of one peer bridge directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerBridgeLayout {
    root: PathBuf,
}

impl PeerBridgeLayout {
    /// Layout rooted at `root` (the `peer-bridge` directory itself).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Layout rooted at `<profile_dir>/peer-bridge`.
    pub fn under_profile(profile_dir: &Path) -> Self {
        Self::new(profile_dir.join(PEER_BRIDGE_DIR))
    }

    /// Root directory of this layout.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Spool file of `peer` (`<root>/spool/<peer>.jsonl`). Creates nothing.
    ///
    /// # Errors
    /// [`PeerBridgeError::InvalidName`] with kind `"peer"` for an invalid id.
    pub fn spool_path(&self, peer: &str) -> PeerBridgeResult<PathBuf> {
        if !is_valid_peer_id(peer) {
            return Err(PeerBridgeError::InvalidName { kind: "peer" });
        }
        Ok(self.root.join(SPOOL_DIR).join(format!("{peer}.jsonl")))
    }

    fn inbox_dir(&self) -> PathBuf {
        self.root.join(INBOX_DIR)
    }

    fn outbox_dir(&self) -> PathBuf {
        self.root.join(OUTBOX_DIR)
    }

    fn live_path(&self, session: &str) -> PathBuf {
        self.inbox_dir().join(format!("{session}.live"))
    }

    fn inbox_path(&self, session: &str) -> PathBuf {
        self.inbox_dir().join(format!("{session}.jsonl"))
    }

    fn outbox_path(&self, peer: &str) -> PathBuf {
        self.outbox_dir().join(format!("{peer}.jsonl"))
    }
}

/// `true` for 1..=64 bytes of ASCII alphanumerics, `_` or `-`.
#[must_use]
pub fn is_valid_peer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PEER_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// `true` for 1..=128 bytes of ASCII alphanumerics, `.`, `_`, `:` or `-`.
#[must_use]
pub fn is_valid_message_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_MESSAGE_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// Content of `inbox/<session>.live`, written by the TUI that holds the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerLiveInfo {
    /// Record version ([`PEER_RECORD_VERSION`]).
    pub v: u32,
    /// Session id (equals the file stem).
    pub session: String,
    /// RFC 3339 timestamp at which the TUI registered.
    pub started_at: String,
    /// Working directory of the TUI, if known.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// One message from a peer, queued for a live session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerInboundMessage {
    /// Record version ([`PEER_RECORD_VERSION`]).
    pub v: u32,
    /// Message id chosen by the peer (see [`is_valid_message_id`]).
    pub id: String,
    /// Sending peer id (see [`is_valid_peer_id`]).
    pub from: String,
    /// RFC 3339 timestamp of reception.
    pub received_at: String,
    /// Whether the message may start an automatic turn.
    pub wake: bool,
    /// Message text (untrusted).
    pub text: String,
}

/// One message from harw to a peer, stored in the outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerOutboundMessage {
    /// Record version ([`PEER_RECORD_VERSION`]).
    pub v: u32,
    /// Per-peer sequence number, starting at 1.
    pub seq: u64,
    /// Message id; `harw-{seq}` when stored with an empty id.
    pub id: String,
    /// Sender (normally `"harw"`).
    pub from: String,
    /// Receiving peer id.
    pub to: String,
    /// Session that sent the message.
    pub session: String,
    /// RFC 3339 timestamp of sending.
    pub sent_at: String,
    /// Message text.
    pub text: String,
    /// Id of the inbound message this answers, if any.
    #[serde(default)]
    pub reply_to: Option<String>,
}

/// Limits enforced by [`append_inbound`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerInboxLimits {
    /// Maximum text length in bytes.
    pub max_text_bytes: usize,
    /// Maximum number of queued lines.
    pub queue_cap: usize,
}

/// Errors of the peer bridge store.
#[derive(Debug)]
pub enum PeerBridgeError {
    /// A session, peer or message id is not acceptable.
    InvalidName {
        /// `"session"`, `"peer"` or `"message id"`.
        kind: &'static str,
    },
    /// The target session has no live TUI.
    NotLive,
    /// The session inbox already holds `queue_cap` lines.
    InboxFull,
    /// The message text exceeds the configured maximum.
    TooLarge {
        /// Actual text length in bytes.
        bytes: usize,
        /// Allowed maximum in bytes.
        max: usize,
    },
    /// Filesystem error.
    Io(io::Error),
    /// JSON serialization error.
    Encode(serde_json::Error),
}

impl fmt::Display for PeerBridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { kind } => write!(f, "peer bridge: invalid {kind} name"),
            Self::NotLive => f.write_str("peer bridge: session is not live"),
            Self::InboxFull => f.write_str("peer bridge: inbox is full"),
            Self::TooLarge { bytes, max } => {
                write!(f, "peer bridge: message of {bytes} bytes exceeds {max}")
            }
            Self::Io(error) => write!(f, "peer bridge: io: {error}"),
            Self::Encode(error) => write!(f, "peer bridge: encode: {error}"),
        }
    }
}

impl std::error::Error for PeerBridgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Encode(error) => Some(error),
            Self::InvalidName { .. } | Self::NotLive | Self::InboxFull | Self::TooLarge { .. } => {
                None
            }
        }
    }
}

impl From<io::Error> for PeerBridgeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for PeerBridgeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encode(error)
    }
}

/// Result type of the peer bridge store.
pub type PeerBridgeResult<T> = Result<T, PeerBridgeError>;

fn session_component(session: &SessionId) -> PeerBridgeResult<&str> {
    safe_session_component(session).map_err(|_| PeerBridgeError::InvalidName { kind: "session" })
}

fn len_u64(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn ensure_private_dir(path: &Path) -> PeerBridgeResult<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let dir = File::from(open_dir_nofollow(path)?);
    ensure_owned_directory(&dir)?;
    Ok(())
}

fn open_private(path: &Path, mode: OpenMode) -> io::Result<File> {
    let file = open_nofollow(path, mode)?;
    ensure_private_regular(&file)?;
    Ok(file)
}

/// Read-write, create if missing, never truncate, no append.
const fn read_write_create() -> OpenMode {
    OpenMode {
        read: true,
        write: true,
        create: true,
        ..OpenMode::read_only()
    }
}

/// Length of `path` without following a symlink; 0 when missing.
fn file_len(path: &Path) -> io::Result<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

/// Opens and locks the sidecar `<path>.lock` (exclusive or shared).
fn lock_sidecar(path: &Path, exclusive: bool) -> PeerBridgeResult<File> {
    let lock = open_private(&suffixed(path, ".lock"), read_write_create())?;
    if exclusive {
        fs4::FileExt::lock(&lock)?;
    } else {
        fs4::FileExt::lock_shared(&lock)?;
    }
    Ok(lock)
}

/// Renames `path` to `<path>.1` when appending `add` bytes would exceed `cap`.
fn rotate_if_needed(path: &Path, add: u64, cap: u64) -> PeerBridgeResult<()> {
    let len = file_len(path)?;
    if len > 0 && len.saturating_add(add) > cap {
        std::fs::rename(path, suffixed(path, ".1"))?;
    }
    Ok(())
}

/// Counts `\n` in `file` from offset 0, stopping once `cap` is reached.
fn count_lines_up_to(file: &File, cap: usize) -> io::Result<usize> {
    let mut reader: &File = file;
    reader.seek(SeekFrom::Start(0))?;
    let mut chunk = vec![0_u8; COUNT_CHUNK_BYTES];
    let mut count = 0_usize;
    while count < cap {
        let read = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let newlines = chunk
            .get(..read)
            .map_or(0, |bytes| bytes.iter().filter(|byte| **byte == b'\n').count());
        count = count.saturating_add(newlines);
    }
    Ok(count)
}

/// Returns the complete lines (up to the last `\n`) of `bytes`.
fn complete_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let end = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index.saturating_add(1));
    bytes
        .get(..end)
        .unwrap_or(&[])
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
}

/// Reads all complete outbound records of `path`; a missing file is empty.
fn read_outbound_records(path: &Path) -> PeerBridgeResult<Vec<PeerOutboundMessage>> {
    let file = match open_private(path, OpenMode::read_only()) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    (&file).read_to_end(&mut bytes)?;
    let mut records = Vec::new();
    for line in complete_lines(&bytes) {
        match serde_json::from_slice::<PeerOutboundMessage>(line) {
            Ok(record) => records.push(record),
            Err(_) => {
                tracing::warn!(bytes = line.len(), "peer bridge: skipped malformed outbox line");
            }
        }
    }
    Ok(records)
}

fn max_seq(path: &Path) -> PeerBridgeResult<Option<u64>> {
    Ok(read_outbound_records(path)?
        .iter()
        .map(|record| record.seq)
        .max())
}

/// Lists all sessions whose `.live` file is currently locked by a TUI,
/// sorted by session. Malformed or unlocked entries are skipped; a missing
/// `inbox/` directory gives an empty list.
///
/// # Errors
/// [`PeerBridgeError::Io`] when the inbox directory cannot be read.
pub fn list_live_sessions(layout: &PeerBridgeLayout) -> PeerBridgeResult<Vec<PeerLiveInfo>> {
    let entries = match std::fs::read_dir(layout.inbox_dir()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut sessions = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".live")) else {
            continue;
        };
        if let Some(info) = read_live_entry(&entry.path(), stem) {
            sessions.push(info);
        }
    }
    sessions.sort_by(|left, right| left.session.cmp(&right.session));
    Ok(sessions)
}

fn read_live_entry(path: &Path, stem: &str) -> Option<PeerLiveInfo> {
    let id = SessionId::from_str(stem);
    if safe_session_component(&id).is_err() {
        return None;
    }
    let file = open_private(path, OpenMode::read_only()).ok()?;
    match fs4::FileExt::try_lock_shared(&file) {
        // Nobody holds the exclusive lock: stale file. Dropping releases ours.
        Ok(()) | Err(fs4::TryLockError::Error(_)) => return None,
        Err(fs4::TryLockError::WouldBlock) => {}
    }
    let mut bytes = Vec::new();
    (&file).take(MAX_LIVE_BYTES).read_to_end(&mut bytes).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let info: PeerLiveInfo = serde_json::from_str(text.trim()).ok()?;
    (info.v == PEER_RECORD_VERSION && info.session == stem).then_some(info)
}

/// Appends `message` to the inbox of the live `session`.
///
/// Never creates directories; the session must be held by a live
/// [`PeerInboxHandle`].
///
/// # Errors
/// [`PeerBridgeError::InvalidName`] for a bad session, peer or message id,
/// [`PeerBridgeError::TooLarge`], [`PeerBridgeError::NotLive`],
/// [`PeerBridgeError::InboxFull`], [`PeerBridgeError::Io`] (also for a
/// symlinked or non-private inbox) and [`PeerBridgeError::Encode`].
pub fn append_inbound(
    layout: &PeerBridgeLayout,
    session: &SessionId,
    message: &PeerInboundMessage,
    limits: PeerInboxLimits,
) -> PeerBridgeResult<()> {
    let component = session_component(session)?;
    if !is_valid_peer_id(&message.from) {
        return Err(PeerBridgeError::InvalidName { kind: "peer" });
    }
    if !is_valid_message_id(&message.id) {
        return Err(PeerBridgeError::InvalidName { kind: "message id" });
    }
    if message.text.len() > limits.max_text_bytes {
        return Err(PeerBridgeError::TooLarge {
            bytes: message.text.len(),
            max: limits.max_text_bytes,
        });
    }

    let live = match open_nofollow(&layout.live_path(component), OpenMode::read_only()) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(PeerBridgeError::NotLive);
        }
        Err(error) => return Err(error.into()),
    };
    match fs4::FileExt::try_lock_shared(&live) {
        // We got the lock, so no TUI holds it; dropping `live` releases it.
        Ok(()) => return Err(PeerBridgeError::NotLive),
        Err(fs4::TryLockError::WouldBlock) => {}
        Err(fs4::TryLockError::Error(error)) => return Err(error.into()),
    }

    let inbox_mode = OpenMode {
        read: true,
        create: false,
        ..OpenMode::append_create(0o600)
    };
    let inbox = match open_nofollow(&layout.inbox_path(component), inbox_mode) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(PeerBridgeError::NotLive);
        }
        Err(error) => return Err(error.into()),
    };
    ensure_private_regular(&inbox)?;
    fs4::FileExt::lock(&inbox)?;
    if inbox.metadata()?.nlink() == 0 {
        // The TUI unlinked the inbox in Drop while we waited for the lock.
        return Err(PeerBridgeError::NotLive);
    }
    if count_lines_up_to(&inbox, limits.queue_cap)? >= limits.queue_cap {
        return Err(PeerBridgeError::InboxFull);
    }
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    (&inbox).write_all(&line)?;
    inbox.sync_data()?;
    Ok(())
}

/// The TUI side of one session inbox. Holds the exclusive `.live` lock for
/// its lifetime; dropping it unregisters the session.
#[derive(Debug)]
pub struct PeerInboxHandle {
    session: SessionId,
    live_path: PathBuf,
    inbox_path: PathBuf,
    live: File,
    inbox: File,
    cursor: u64,
    seen: VecDeque<(String, String)>,
    seen_set: HashSet<(String, String)>,
}

impl PeerInboxHandle {
    /// Registers `session` as live and opens its inbox. Lines already queued
    /// in an existing inbox are kept and delivered.
    ///
    /// # Errors
    /// [`PeerBridgeError::InvalidName`] for a bad session id;
    /// [`PeerBridgeError::Io`] (kind `WouldBlock`) when another handle holds
    /// the session, or on any filesystem error.
    pub fn create(
        layout: &PeerBridgeLayout,
        session: &SessionId,
        cwd: Option<String>,
    ) -> PeerBridgeResult<Self> {
        let component = session_component(session)?;
        ensure_private_dir(&layout.inbox_dir())?;

        let live_path = layout.live_path(component);
        let live_mode = OpenMode {
            read: false,
            write: true,
            create: true,
            ..OpenMode::read_only()
        };
        let live = open_private(&live_path, live_mode)?;
        match fs4::FileExt::try_lock(&live) {
            Ok(()) => {}
            Err(fs4::TryLockError::WouldBlock) => {
                return Err(PeerBridgeError::Io(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "peer bridge: session inbox already held",
                )));
            }
            Err(fs4::TryLockError::Error(error)) => return Err(error.into()),
        }
        live.set_len(0)?;
        let info = PeerLiveInfo {
            v: PEER_RECORD_VERSION,
            session: component.to_owned(),
            started_at: jiff::Timestamp::now().to_string(),
            cwd,
        };
        let mut bytes = serde_json::to_vec(&info)?;
        bytes.push(b'\n');
        (&live).write_all(&bytes)?;
        live.sync_data()?;

        let inbox_path = layout.inbox_path(component);
        let inbox = open_private(&inbox_path, read_write_create())?;

        Ok(Self {
            session: session.clone(),
            live_path,
            inbox_path,
            live,
            inbox,
            cursor: 0,
            seen: VecDeque::new(),
            seen_set: HashSet::new(),
        })
    }

    /// Session this handle holds.
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    /// Returns all new, valid, not yet seen messages in file order and
    /// truncates the inbox once everything has been consumed.
    ///
    /// # Errors
    /// [`PeerBridgeError::Io`] on lock, read or truncate failures.
    pub fn take_new(&mut self) -> PeerBridgeResult<Vec<PeerInboundMessage>> {
        fs4::FileExt::lock(&self.inbox)?;
        let result = self.take_new_locked();
        let unlocked = fs4::FileExt::unlock(&self.inbox);
        let messages = result?;
        unlocked?;
        Ok(messages)
    }

    fn take_new_locked(&mut self) -> PeerBridgeResult<Vec<PeerInboundMessage>> {
        let len = self.inbox.metadata()?.len();
        if len < self.cursor {
            self.cursor = 0;
        }
        (&self.inbox).seek(SeekFrom::Start(self.cursor))?;
        let mut bytes = Vec::new();
        (&self.inbox).take(MAX_TAKE_BYTES).read_to_end(&mut bytes)?;

        let mut consumed = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index.saturating_add(1));
        if consumed == 0 && len_u64(bytes.len()) >= MAX_TAKE_BYTES {
            tracing::warn!(
                bytes = bytes.len(),
                "peer bridge: dropped oversized inbox chunk without newline"
            );
            consumed = bytes.len();
        }

        let mut messages = Vec::new();
        for line in complete_lines(bytes.get(..consumed).unwrap_or(&[])) {
            let message = match serde_json::from_slice::<PeerInboundMessage>(line) {
                Ok(message)
                    if message.v == PEER_RECORD_VERSION
                        && is_valid_peer_id(&message.from)
                        && is_valid_message_id(&message.id) =>
                {
                    message
                }
                _ => {
                    tracing::warn!(bytes = line.len(), "peer bridge: skipped malformed inbox line");
                    continue;
                }
            };
            let key = (message.from.clone(), message.id.clone());
            if !self.seen_set.insert(key.clone()) {
                continue;
            }
            self.seen.push_back(key);
            while self.seen.len() > PEER_DEDUP_WINDOW {
                if let Some(old) = self.seen.pop_front() {
                    self.seen_set.remove(&old);
                }
            }
            messages.push(message);
        }

        self.cursor = self.cursor.saturating_add(len_u64(consumed));
        if self.cursor == len {
            self.inbox.set_len(0)?;
            self.cursor = 0;
        }
        Ok(messages)
    }
}

impl Drop for PeerInboxHandle {
    fn drop(&mut self) {
        // Best effort: unlink while holding the inbox lock, so a waiting
        // writer sees nlink == 0 and reports NotLive.
        let _ = fs4::FileExt::lock(&self.inbox);
        let _ = std::fs::remove_file(&self.live_path);
        let _ = std::fs::remove_file(&self.inbox_path);
        let _ = fs4::FileExt::unlock(&self.live);
        let _ = fs4::FileExt::unlock(&self.inbox);
    }
}

/// Stores `message` in `outbox/<to>.jsonl` with the next sequence number
/// (and `id = "harw-{seq}"` when empty), rotating to `.jsonl.1` when the
/// main file would exceed `cap_bytes`. Returns the stored record.
///
/// # Errors
/// [`PeerBridgeError::InvalidName`] for a bad peer, id or `reply_to`;
/// [`PeerBridgeError::Io`] / [`PeerBridgeError::Encode`] otherwise.
pub fn append_outbound(
    layout: &PeerBridgeLayout,
    mut message: PeerOutboundMessage,
    cap_bytes: u64,
) -> PeerBridgeResult<PeerOutboundMessage> {
    if !is_valid_peer_id(&message.to) {
        return Err(PeerBridgeError::InvalidName { kind: "peer" });
    }
    if !message.id.is_empty() && !is_valid_message_id(&message.id) {
        return Err(PeerBridgeError::InvalidName { kind: "message id" });
    }
    if message
        .reply_to
        .as_deref()
        .is_some_and(|reply_to| !is_valid_message_id(reply_to))
    {
        return Err(PeerBridgeError::InvalidName { kind: "message id" });
    }

    ensure_private_dir(&layout.outbox_dir())?;
    let main = layout.outbox_path(&message.to);
    let _lock = lock_sidecar(&main, true)?;

    let last = match max_seq(&main)? {
        Some(seq) => seq,
        None => max_seq(&suffixed(&main, ".1"))?.unwrap_or(0),
    };
    let seq = last.saturating_add(1);
    message.seq = seq;
    message.v = PEER_RECORD_VERSION;
    if message.id.is_empty() {
        message.id = format!("harw-{seq}");
    }

    let mut line = serde_json::to_vec(&message)?;
    line.push(b'\n');
    rotate_if_needed(&main, len_u64(line.len()), cap_bytes)?;
    let file = open_private(&main, OpenMode::append_create(0o600))?;
    (&file).write_all(&line)?;
    file.sync_data()?;
    Ok(message)
}

/// Reads up to `max` outbox records of `peer` with `seq > after` (default 0)
/// in ascending order, from `.jsonl.1` and then `.jsonl`. Never creates
/// directories; a missing outbox gives an empty list.
///
/// # Errors
/// [`PeerBridgeError::InvalidName`] for a bad peer id;
/// [`PeerBridgeError::Io`] on filesystem errors.
pub fn read_outbound_after(
    layout: &PeerBridgeLayout,
    peer: &str,
    after: Option<u64>,
    max: usize,
) -> PeerBridgeResult<Vec<PeerOutboundMessage>> {
    if !is_valid_peer_id(peer) {
        return Err(PeerBridgeError::InvalidName { kind: "peer" });
    }
    match std::fs::symlink_metadata(layout.outbox_dir()) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    }
    let main = layout.outbox_path(peer);
    let _lock = lock_sidecar(&main, false)?;

    let after = after.unwrap_or(0);
    let mut records = read_outbound_records(&suffixed(&main, ".1"))?;
    records.extend(read_outbound_records(&main)?);
    records.retain(|record| record.to == peer && record.seq > after);
    records.sort_by_key(|record| record.seq);
    records.dedup_by_key(|record| record.seq);
    records.truncate(max);
    Ok(records)
}

/// Appends `line` plus `\n` to the spool file `path`, rotating it to
/// `<path>.1` when it would exceed `cap_bytes`. Creates the parent
/// directory with mode 0700.
///
/// # Errors
/// [`PeerBridgeError::Io`] with kind `InvalidInput` when `line` contains a
/// newline or `path` has no parent, otherwise on filesystem errors.
pub fn append_spool_line(path: &Path, line: &[u8], cap_bytes: u64) -> PeerBridgeResult<()> {
    if line.contains(&b'\n') {
        return Err(PeerBridgeError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "peer bridge: spool line contains a newline",
        )));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "peer bridge: spool path has no parent",
            )
        })?;
    ensure_private_dir(parent)?;
    let _lock = lock_sidecar(path, true)?;

    let mut bytes = Vec::with_capacity(line.len().saturating_add(1));
    bytes.extend_from_slice(line);
    bytes.push(b'\n');
    rotate_if_needed(path, len_u64(bytes.len()), cap_bytes)?;
    let file = open_private(path, OpenMode::append_create(0o600))?;
    (&file).write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use tempfile::tempdir;

    const LIMITS: PeerInboxLimits = PeerInboxLimits {
        max_text_bytes: 16384,
        queue_cap: 64,
    };

    fn layout_in(dir: &tempfile::TempDir) -> PeerBridgeLayout {
        PeerBridgeLayout::new(dir.path().join("peer-bridge"))
    }

    fn session() -> SessionId {
        SessionId::from_str("tui-a")
    }

    fn inbound(id: &str, text: &str) -> PeerInboundMessage {
        PeerInboundMessage {
            v: 1,
            id: id.to_owned(),
            from: "codex".to_owned(),
            received_at: "2026-11-22T00:00:00Z".to_owned(),
            wake: false,
            text: text.to_owned(),
        }
    }

    fn outbound(to: &str, id: &str) -> PeerOutboundMessage {
        PeerOutboundMessage {
            v: 0,
            seq: 0,
            id: id.to_owned(),
            from: "harw".to_owned(),
            to: to.to_owned(),
            session: "tui-a".to_owned(),
            sent_at: "2026-11-22T00:00:00Z".to_owned(),
            text: "hello".to_owned(),
            reply_to: None,
        }
    }

    fn unexpected<T: fmt::Debug>(value: T) -> TestError {
        TestError::Unexpected(format!("{value:?}"))
    }

    fn write_private(path: &Path, bytes: &[u8]) -> TestResult {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        Ok(())
    }

    fn live_json(session: &str) -> TestResult<Vec<u8>> {
        Ok(serde_json::to_vec(&PeerLiveInfo {
            v: 1,
            session: session.to_owned(),
            started_at: "2026-11-22T00:00:00Z".to_owned(),
            cwd: None,
        })?)
    }

    fn mode_of(path: &Path) -> TestResult<u32> {
        Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
    }

    #[test]
    fn append_without_live_handle_is_not_live() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        match append_inbound(&layout, &session(), &inbound("m1", "hi"), LIMITS) {
            Err(PeerBridgeError::NotLive) => {}
            other => return Err(unexpected(other)),
        }

        let inbox_dir = layout.root().join("inbox");
        std::fs::create_dir_all(&inbox_dir)?;
        let live = inbox_dir.join("tui-a.live");
        let queue = inbox_dir.join("tui-a.jsonl");
        write_private(&live, &live_json("tui-a")?)?;
        write_private(&queue, b"")?;
        match append_inbound(&layout, &session(), &inbound("m1", "hi"), LIMITS) {
            Err(PeerBridgeError::NotLive) => {}
            other => return Err(unexpected(other)),
        }

        let handle = PeerInboxHandle::create(&layout, &session(), None).map_err(ctx("create"))?;
        drop(handle);
        match append_inbound(&layout, &session(), &inbound("m1", "hi"), LIMITS) {
            Err(PeerBridgeError::NotLive) => {}
            other => return Err(unexpected(other)),
        }
        if live.exists() || queue.exists() {
            return Err(TestError::Unexpected("files left after drop".to_owned()));
        }
        Ok(())
    }

    #[test]
    fn round_trip_delivers_and_truncates() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let mut handle =
            PeerInboxHandle::create(&layout, &session(), None).map_err(ctx("create"))?;
        let message = inbound("m1", "hello");
        append_inbound(&layout, &session(), &message, LIMITS).map_err(ctx("append"))?;
        let taken = handle.take_new().map_err(ctx("take"))?;
        if taken != vec![message] {
            return Err(unexpected(taken));
        }
        let len = std::fs::metadata(layout.root().join("inbox/tui-a.jsonl"))?.len();
        if len != 0 {
            return Err(unexpected(len));
        }
        if *handle.session() != session() {
            return Err(unexpected(handle.session()));
        }
        Ok(())
    }

    #[test]
    fn partial_trailing_line_waits_for_newline() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let mut handle =
            PeerInboxHandle::create(&layout, &session(), None).map_err(ctx("create"))?;
        let first = inbound("m1", "one");
        let second = inbound("m2", "two");
        append_inbound(&layout, &session(), &first, LIMITS).map_err(ctx("append"))?;

        let queue = layout.root().join("inbox/tui-a.jsonl");
        let bytes = serde_json::to_vec(&second)?;
        let (head, tail) = bytes.split_at(bytes.len() / 2);
        let mut raw = std::fs::OpenOptions::new().append(true).open(&queue)?;
        raw.write_all(head)?;

        let taken = handle.take_new().map_err(ctx("take first"))?;
        if taken != vec![first] {
            return Err(unexpected(taken));
        }
        if std::fs::metadata(&queue)?.len() == 0 {
            return Err(TestError::Unexpected("partial line was truncated".to_owned()));
        }

        raw.write_all(tail)?;
        raw.write_all(b"\n")?;
        let taken = handle.take_new().map_err(ctx("take second"))?;
        if taken != vec![second] {
            return Err(unexpected(taken));
        }
        let len = std::fs::metadata(&queue)?.len();
        if len != 0 {
            return Err(unexpected(len));
        }
        Ok(())
    }

    #[test]
    fn duplicates_are_delivered_once() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let mut handle =
            PeerInboxHandle::create(&layout, &session(), None).map_err(ctx("create"))?;
        let message = inbound("d1", "same");
        append_inbound(&layout, &session(), &message, LIMITS).map_err(ctx("append 1"))?;
        append_inbound(&layout, &session(), &message, LIMITS).map_err(ctx("append 2"))?;
        let taken = handle.take_new().map_err(ctx("take"))?;
        if taken.len() != 1 {
            return Err(unexpected(taken));
        }
        append_inbound(&layout, &session(), &message, LIMITS).map_err(ctx("append 3"))?;
        let taken = handle.take_new().map_err(ctx("take again"))?;
        if !taken.is_empty() {
            return Err(unexpected(taken));
        }
        Ok(())
    }

    #[test]
    fn queue_cap_reports_inbox_full() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let _handle = PeerInboxHandle::create(&layout, &session(), None).map_err(ctx("create"))?;
        let limits = PeerInboxLimits {
            max_text_bytes: 16384,
            queue_cap: 2,
        };
        append_inbound(&layout, &session(), &inbound("q1", "a"), limits).map_err(ctx("q1"))?;
        append_inbound(&layout, &session(), &inbound("q2", "b"), limits).map_err(ctx("q2"))?;
        match append_inbound(&layout, &session(), &inbound("q3", "c"), limits) {
            Err(PeerBridgeError::InboxFull) => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    #[test]
    fn oversized_text_is_too_large() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let limits = PeerInboxLimits {
            max_text_bytes: 4,
            queue_cap: 64,
        };
        match append_inbound(&layout, &session(), &inbound("t1", "hello"), limits) {
            Err(PeerBridgeError::TooLarge { bytes: 5, max: 4 }) => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    #[test]
    fn symlinked_inbox_is_rejected() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let _handle = PeerInboxHandle::create(&layout, &session(), None).map_err(ctx("create"))?;
        let queue = layout.root().join("inbox/tui-a.jsonl");
        std::fs::remove_file(&queue)?;
        let target = dir.path().join("target");
        write_private(&target, b"")?;
        std::os::unix::fs::symlink(&target, &queue)?;
        match append_inbound(&layout, &session(), &inbound("s1", "x"), LIMITS) {
            Err(PeerBridgeError::Io(_)) => {}
            other => return Err(unexpected(other)),
        }
        let len = std::fs::metadata(&target)?.len();
        if len != 0 {
            return Err(unexpected(len));
        }
        Ok(())
    }

    #[test]
    fn non_private_inbox_is_rejected() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let _handle = PeerInboxHandle::create(&layout, &session(), None).map_err(ctx("create"))?;
        let queue = layout.root().join("inbox/tui-a.jsonl");
        std::fs::set_permissions(&queue, std::fs::Permissions::from_mode(0o644))?;
        match append_inbound(&layout, &session(), &inbound("p1", "x"), LIMITS) {
            Err(PeerBridgeError::Io(_)) => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    #[test]
    fn invalid_names_are_rejected() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        match append_inbound(
            &layout,
            &SessionId::from_str("../x"),
            &inbound("n1", "x"),
            LIMITS,
        ) {
            Err(PeerBridgeError::InvalidName { kind: "session" }) => {}
            other => return Err(unexpected(other)),
        }
        let mut bad_from = inbound("n1", "x");
        bad_from.from = "bad/peer".to_owned();
        match append_inbound(&layout, &session(), &bad_from, LIMITS) {
            Err(PeerBridgeError::InvalidName { kind: "peer" }) => {}
            other => return Err(unexpected(other)),
        }
        match append_inbound(&layout, &session(), &inbound("", "x"), LIMITS) {
            Err(PeerBridgeError::InvalidName { kind: "message id" }) => {}
            other => return Err(unexpected(other)),
        }
        match layout.spool_path("a b") {
            Err(PeerBridgeError::InvalidName { kind: "peer" }) => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    #[test]
    fn list_live_sessions_reports_only_locked_sessions() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let empty = list_live_sessions(&layout).map_err(ctx("list empty"))?;
        if !empty.is_empty() {
            return Err(unexpected(empty));
        }

        let _handle = PeerInboxHandle::create(&layout, &session(), Some("/work".to_owned()))
            .map_err(ctx("create"))?;
        let inbox_dir = layout.root().join("inbox");
        write_private(&inbox_dir.join("tui-b.live"), &live_json("tui-b")?)?;
        write_private(&inbox_dir.join("tui-c.live"), b"{garbage")?;

        let live = list_live_sessions(&layout).map_err(ctx("list"))?;
        let [only] = live.as_slice() else {
            return Err(unexpected(live));
        };
        if only.session != "tui-a" || only.v != 1 || only.cwd.as_deref() != Some("/work") {
            return Err(unexpected(only));
        }
        Ok(())
    }

    #[test]
    fn second_create_for_live_session_fails() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let _handle = PeerInboxHandle::create(&layout, &session(), Some("/work".to_owned()))
            .map_err(ctx("create"))?;
        let before = list_live_sessions(&layout).map_err(ctx("list before"))?;
        if let Ok(second) = PeerInboxHandle::create(&layout, &session(), None) {
            return Err(unexpected(second));
        }
        let after = list_live_sessions(&layout).map_err(ctx("list after"))?;
        if before.len() != 1 || before != after {
            return Err(unexpected((before, after)));
        }
        Ok(())
    }

    #[test]
    fn outbound_assigns_sequence_and_ids() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        for expected in 1..=3_u64 {
            let stored =
                append_outbound(&layout, outbound("codex", ""), 1 << 20).map_err(ctx("append"))?;
            if stored.seq != expected || stored.id != format!("harw-{expected}") || stored.v != 1
            {
                return Err(unexpected(stored));
            }
        }
        let stored =
            append_outbound(&layout, outbound("codex", "x-1"), 1 << 20).map_err(ctx("explicit"))?;
        if stored.id != "x-1" || stored.seq != 4 {
            return Err(unexpected(stored));
        }
        Ok(())
    }

    #[test]
    fn outbound_rotates_and_reads_after() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let first =
            append_outbound(&layout, outbound("rot", ""), u64::MAX).map_err(ctx("append 1"))?;
        let one = len_u64(serde_json::to_vec(&first)?.len()) + 1;
        let cap = one + one / 2;
        let second = append_outbound(&layout, outbound("rot", ""), cap).map_err(ctx("append 2"))?;
        let third = append_outbound(&layout, outbound("rot", ""), cap).map_err(ctx("append 3"))?;
        let seqs = [first.seq, second.seq, third.seq];
        if seqs != [1, 2, 3] {
            return Err(unexpected(seqs));
        }
        if !layout.root().join("outbox/rot.jsonl.1").exists() {
            return Err(TestError::Missing("rotated outbox"));
        }

        let seqs_of = |records: &[PeerOutboundMessage]| -> Vec<u64> {
            records.iter().map(|record| record.seq).collect()
        };
        let all = read_outbound_after(&layout, "rot", None, 50).map_err(ctx("read all"))?;
        if seqs_of(&all) != vec![2, 3] {
            return Err(unexpected(all));
        }
        let after = read_outbound_after(&layout, "rot", Some(2), 50).map_err(ctx("after 2"))?;
        if seqs_of(&after) != vec![3] {
            return Err(unexpected(after));
        }
        let limited = read_outbound_after(&layout, "rot", None, 1).map_err(ctx("max 1"))?;
        if seqs_of(&limited) != vec![2] {
            return Err(unexpected(limited));
        }
        let unknown = read_outbound_after(&layout, "nobody", None, 50).map_err(ctx("unknown"))?;
        if !unknown.is_empty() {
            return Err(unexpected(unknown));
        }
        let other = PeerBridgeLayout::new(dir.path().join("other"));
        let missing = read_outbound_after(&other, "rot", None, 50).map_err(ctx("missing"))?;
        if !missing.is_empty() {
            return Err(unexpected(missing));
        }
        Ok(())
    }

    #[test]
    fn spool_rotates_with_private_modes() -> TestResult {
        let dir = tempdir()?;
        let layout = layout_in(&dir);
        let path = layout.spool_path("codex").map_err(ctx("spool path"))?;
        append_spool_line(&path, b"first-line-xx", 16).map_err(ctx("spool 1"))?;
        append_spool_line(&path, b"second-line", 16).map_err(ctx("spool 2"))?;

        let rotated = suffixed(&path, ".1");
        if !rotated.exists() {
            return Err(TestError::Missing("rotated spool"));
        }
        let content = std::fs::read(&path)?;
        if content != b"second-line\n" {
            return Err(unexpected(String::from_utf8_lossy(&content).into_owned()));
        }
        for file in [&path, &rotated] {
            let mode = mode_of(file)?;
            if mode != 0o600 {
                return Err(unexpected(mode));
            }
        }
        let spool_dir = path.parent().ok_or(TestError::Missing("spool dir"))?;
        let dir_mode = mode_of(spool_dir)?;
        if dir_mode != 0o700 {
            return Err(unexpected(dir_mode));
        }
        if append_spool_line(&path, b"bad\nline", 1 << 20).is_ok() {
            return Err(TestError::Unexpected("newline line accepted".to_owned()));
        }
        Ok(())
    }
}
