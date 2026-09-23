//! Durable, restart-safe, single-winner pairing store (spec §3.2).
//!
//! # Responsibility
//! This module owns the *durability* of channel pairing: it issues single-use,
//! short-TTL codes, redeems them exactly once under concurrency, records and
//! revokes the authoritative `PeerId -> TenantId` bindings, and gates replayed
//! inbound updates. The pure code lifecycle (rendering, TTL, validation) is
//! delegated to [`crate::pairing::PairingCode`]; this store owns filesystem
//! locking and atomic record replacement, mirroring the `harw-session-store`
//! `JobStore` lock/persist idiom.
//!
//! # Key types
//! - [`PairingStore`] — the durable store rooted at a directory.
//!
//! # Durability model
//! Each table (`codes`, `bindings`, `claims`) keeps a `records/` tree of atomic
//! JSON files plus a `locks/` tree of advisory `fs4` exclusive locks. Mutations
//! take the per-key lock, read-modify-write via a synced sibling temp file, then
//! release the lock. An append-only journal under `journal/` records every
//! transition through `harw-session-store`'s [`TranscriptStore`] so history is
//! preserved even when the mutable projection is flipped or revoked.
//!
//! # Concurrency
//! [`PairingStore`] is `Clone` and cheap to share (`Send + Sync`, it holds only a
//! [`PathBuf`]). Single-winner redemption is enforced by `fs4` advisory
//! file locks, which conflict across open file descriptions even within one
//! process, so concurrent redeemers of the same code contend on the lock and
//! exactly one wins.
//!
//! # Errors
//! All fallible paths return [`ChannelResult`]; failures surface as
//! [`ChannelError`] variants (`PairingInvalid`, `PairingExpired`,
//! `PairingAlreadyRedeemed`, `PairingBindingNotFound`, `PairingContended`,
//! `Time`, and the `#[from]` `Io` / `Serde` / `SessionStore` conversions).
//!
//! # Examples
//! ```rust,no_run
//! use harw_channel::PairingStore;
//! use harw_channel::ids::{ChannelId, PeerId, TenantId};
//! use jiff::Timestamp;
//!
//! let dir = tempfile::tempdir()?;
//! let store = PairingStore::new(dir.path());
//! let channel = ChannelId::from_str("telegram:ops");
//! let tenant = TenantId::from_str("ops");
//! let now = Timestamp::now();
//! let code = store.issue_code(&channel, &tenant, b"seed12345", now)?;
//! let binding = store.redeem_once(&channel, &code, &PeerId::from_str("100"), now)?;
//! assert_eq!(binding.tenant, tenant);
//! # Ok::<(), harw_channel::ChannelError>(())
//! ```

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs4::FileExt;
use harw_session_store::{RecordKind, TranscriptRecord, TranscriptStore};
use harw_types::SessionId;
use jiff::Timestamp;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::error::{ChannelError, ChannelResult};
use crate::ids::{ChannelId, PeerId, TenantId, ThreadRef};
use crate::pairing::{PairingCode, PairingRecord};

// Field separator used when composing a filesystem-safe record key. Chosen to
// not collide with id value space; the whole composite is hex-encoded anyway.
const KEY_SEP: char = '\u{1f}';

// Durable, mutable projection of one issued pairing code. Append-only history
// lives in the journal; this file is flipped to consumed on redemption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct IssuedCodeRecord {
    channel: ChannelId,
    code: String,
    tenant: TenantId,
    issued_at: Timestamp,
    expires_at: Timestamp,
    consumed_at: Option<Timestamp>,
    consumed_by: Option<PeerId>,
}

// Durable, mutable projection of one PeerId -> TenantId binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct BindingRecord {
    channel: ChannelId,
    peer: PeerId,
    tenant: TenantId,
    bound_at: Timestamp,
    revoked_at: Option<Timestamp>,
}

// Durable replay marker for one (channel, update_id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ClaimRecord {
    channel: ChannelId,
    update_id: String,
    received_at: Timestamp,
}

/// Durable, restart-safe, single-winner pairing store (spec §3.2).
///
/// # Description
/// Owns the on-disk lifecycle of channel pairing codes, `PeerId -> TenantId`
/// bindings, and replay-claim markers. All mutations are serialized per key by
/// an `fs4` advisory lock and made durable via synced temp-file rename, so the
/// store is correct across process restarts and under concurrent redeemers.
///
/// # Concurrency
/// `Clone` + `Send + Sync`: cheap to share across threads (holds only a
/// [`PathBuf`]). Single-winner semantics come from advisory file locking.
///
/// # Examples
/// ```rust,no_run
/// use harw_channel::PairingStore;
/// let store = PairingStore::new(std::path::Path::new("/var/lib/harw/pairing"));
/// let _ = store.root();
/// ```
#[derive(Debug, Clone)]
pub struct PairingStore {
    root: PathBuf,
}

impl PairingStore {
    /// Creates a store rooted at `root` (directories are created lazily on write).
    ///
    /// # Arguments
    /// - `root` (`&Path`): borrowed directory under which the `codes`,
    ///   `bindings`, `claims`, and `journal` trees live. Copied into an owned
    ///   [`PathBuf`]; the directory need not exist yet.
    ///
    /// # Returns
    /// A ready-to-use [`PairingStore`].
    ///
    /// # Concurrency
    /// Pure and lock-free; safe to call from any thread.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_channel::PairingStore;
    /// let store = PairingStore::new(std::path::Path::new("/tmp/pairing"));
    /// # let _ = store;
    /// ```
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Borrows the store root.
    ///
    /// # Returns
    /// The `&Path` this store is anchored at.
    ///
    /// # Concurrency
    /// Pure; safe to call from any thread.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_channel::PairingStore;
    /// let store = PairingStore::new(std::path::Path::new("/tmp/pairing"));
    /// assert_eq!(store.root(), std::path::Path::new("/tmp/pairing"));
    /// ```
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Issues a single-use, short-TTL code bound to `tenant` on `channel` (§3.2).
    ///
    /// # Description
    /// Renders a code from `entropy`, computes the expiry from
    /// [`PairingCode::default_ttl`], and atomically persists the issued-code
    /// record under the per-key lock, then journals a `pairing.code.issued`
    /// lifecycle event. Returns the rendered code string to present to the peer.
    ///
    /// # Arguments
    /// - `channel` (`&ChannelId`): the channel scope the code is valid against.
    /// - `tenant` (`&TenantId`): the tenant a redeemer will be bound to.
    /// - `entropy` (`&[u8]`): caller-supplied random bytes (see [`PairingCode::format_code`]).
    /// - `now` (`Timestamp`): issuance time; expiry is `now + default_ttl`.
    ///
    /// # Returns
    /// `Ok(String)` — the rendered pairing code to present to the remote peer.
    ///
    /// # Errors
    /// - [`ChannelError::Time`]: expiry timestamp overflow.
    /// - [`ChannelError::PairingInvalid`]: an astronomically-unlikely entropy
    ///   collision with an already-issued code on this channel.
    /// - [`ChannelError::PairingContended`]: the per-key lock is already held.
    /// - [`ChannelError::Io`] / [`ChannelError::Serde`] / [`ChannelError::SessionStore`]:
    ///   persistence or journalling failure.
    ///
    /// # Concurrency
    /// Acquires the per-code advisory exclusive lock for the write; the journal
    /// append takes its own transcript lock. Safe to call from many threads.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_channel::PairingStore;
    /// use harw_channel::ids::{ChannelId, TenantId};
    /// use jiff::Timestamp;
    /// let store = PairingStore::new(std::path::Path::new("/tmp/pairing"));
    /// let code = store.issue_code(
    ///     &ChannelId::from_str("telegram:ops"),
    ///     &TenantId::from_str("ops"),
    ///     b"seed12345",
    ///     Timestamp::now(),
    /// )?;
    /// # let _ = code;
    /// # Ok::<(), harw_channel::ChannelError>(())
    /// ```
    pub fn issue_code(
        &self,
        channel: &ChannelId,
        tenant: &TenantId,
        entropy: &[u8],
        now: Timestamp,
    ) -> ChannelResult<String> {
        let code = PairingCode::format_code(entropy);
        let expires_at =
            now.checked_add(PairingCode::default_ttl())
                .map_err(|e| ChannelError::Time {
                    detail: e.to_string(),
                })?;
        let record = IssuedCodeRecord {
            channel: channel.clone(),
            code: code.clone(),
            tenant: tenant.clone(),
            issued_at: now,
            expires_at,
            consumed_at: None,
            consumed_by: None,
        };
        let paths = self.paths("codes", &[channel.as_str(), &code]);
        let lock = self.lock(&paths.lock, channel, "code")?;
        let result = (|| {
            if paths.record.exists() {
                // Astronomically-unlikely entropy collision; treat as invalid.
                return Err(ChannelError::PairingInvalid {
                    channel: channel.clone(),
                    code: code.clone(),
                });
            }
            persist_json(&paths.record, &record)
        })();
        unlock(lock, result)?;
        self.journal_append(channel, now, "pairing.code.issued", &record)?;
        Ok(code)
    }

    /// Atomically consumes a still-valid, unredeemed code, binding `actor` to the
    /// code's tenant (§3.2). Single-winner under concurrency.
    ///
    /// # Description
    /// Under the per-code lock, validates scope, redemption state, and expiry,
    /// then flips the code to consumed and writes the authoritative binding
    /// record under its own lock. Journals a `pairing.redeemed` event and
    /// returns the resulting [`PairingRecord`].
    ///
    /// # Arguments
    /// - `channel` (`&ChannelId`): the channel the code must be scoped to.
    /// - `code` (`&str`): the presented code string.
    /// - `actor` (`&PeerId`): the peer to bind on success.
    /// - `now` (`Timestamp`): redemption time, checked against expiry.
    ///
    /// # Returns
    /// `Ok(PairingRecord)` describing the new `actor -> tenant` binding.
    ///
    /// # Errors
    /// - [`ChannelError::PairingInvalid`]: unknown code or wrong channel scope.
    /// - [`ChannelError::PairingAlreadyRedeemed`]: the code was already consumed.
    /// - [`ChannelError::PairingExpired`]: the code's TTL elapsed before `now`.
    /// - [`ChannelError::PairingContended`]: a lock was already held.
    /// - [`ChannelError::Io`] / [`ChannelError::Serde`] / [`ChannelError::SessionStore`]:
    ///   persistence or journalling failure.
    ///
    /// # Concurrency
    /// Serializes redeemers of the same code on the per-code advisory lock;
    /// exactly one caller wins a race, the rest observe a contention or
    /// already-redeemed error.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_channel::PairingStore;
    /// use harw_channel::ids::{ChannelId, PeerId};
    /// use jiff::Timestamp;
    /// let store = PairingStore::new(std::path::Path::new("/tmp/pairing"));
    /// let binding = store.redeem_once(
    ///     &ChannelId::from_str("telegram:ops"),
    ///     "7F2K-9QRT",
    ///     &PeerId::from_str("100"),
    ///     Timestamp::now(),
    /// )?;
    /// # let _ = binding;
    /// # Ok::<(), harw_channel::ChannelError>(())
    /// ```
    pub fn redeem_once(
        &self,
        channel: &ChannelId,
        code: &str,
        actor: &PeerId,
        now: Timestamp,
    ) -> ChannelResult<PairingRecord> {
        let paths = self.paths("codes", &[channel.as_str(), code]);
        let lock = self.lock(&paths.lock, channel, "code")?;
        let consume = (|| {
            let mut record: IssuedCodeRecord =
                read_json(&paths.record)?.ok_or_else(|| ChannelError::PairingInvalid {
                    channel: channel.clone(),
                    code: code.to_owned(),
                })?;
            if &record.channel != channel || record.code != code {
                return Err(ChannelError::PairingInvalid {
                    channel: channel.clone(),
                    code: code.to_owned(),
                });
            }
            if record.consumed_at.is_some() {
                return Err(ChannelError::PairingAlreadyRedeemed {
                    channel: channel.clone(),
                    code: code.to_owned(),
                });
            }
            if now > record.expires_at {
                return Err(ChannelError::PairingExpired {
                    code: code.to_owned(),
                });
            }
            record.consumed_at = Some(now);
            record.consumed_by = Some(actor.clone());
            persist_json(&paths.record, &record)?;
            Ok(record.tenant)
        })();
        let tenant = unlock(lock, consume)?;

        let binding = BindingRecord {
            channel: channel.clone(),
            peer: actor.clone(),
            tenant: tenant.clone(),
            bound_at: now,
            revoked_at: None,
        };
        let bpaths = self.paths("bindings", &[channel.as_str(), actor.as_str()]);
        let block = self.lock(&bpaths.lock, channel, "binding")?;
        let bresult = persist_json(&bpaths.record, &binding);
        unlock(block, bresult)?;

        self.journal_append(channel, now, "pairing.redeemed", &binding)?;
        Ok(PairingRecord {
            channel: channel.clone(),
            peer: actor.clone(),
            tenant,
            bound_at: now,
        })
    }

    /// Durable, restart-correct read of a peer's active tenant binding (§3.2).
    ///
    /// # Description
    /// Reads the binding record from disk (not memory), returning the bound
    /// tenant only if the binding exists and is not revoked.
    ///
    /// # Arguments
    /// - `channel` (`&ChannelId`): the channel scope.
    /// - `peer` (`&PeerId`): the peer whose binding is queried.
    ///
    /// # Returns
    /// `Ok(Some(TenantId))` if actively bound, `Ok(None)` if never paired or
    /// currently revoked.
    ///
    /// # Errors
    /// - [`ChannelError::Io`] / [`ChannelError::Serde`]: read or decode failure.
    ///
    /// # Concurrency
    /// Lock-free point read; may observe an in-flight mutation as either its
    /// pre- or post-state, never a torn record (writes are atomic renames).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_channel::PairingStore;
    /// use harw_channel::ids::{ChannelId, PeerId};
    /// let store = PairingStore::new(std::path::Path::new("/tmp/pairing"));
    /// let tenant = store.lookup_binding(
    ///     &ChannelId::from_str("telegram:ops"),
    ///     &PeerId::from_str("100"),
    /// )?;
    /// # let _ = tenant;
    /// # Ok::<(), harw_channel::ChannelError>(())
    /// ```
    pub fn lookup_binding(
        &self,
        channel: &ChannelId,
        peer: &PeerId,
    ) -> ChannelResult<Option<TenantId>> {
        let paths = self.paths("bindings", &[channel.as_str(), peer.as_str()]);
        let record: Option<BindingRecord> = read_json(&paths.record)?;
        Ok(record.and_then(|r| {
            if r.revoked_at.is_some() {
                None
            } else {
                Some(r.tenant)
            }
        }))
    }

    /// Appends a revocation for a peer's binding (§3.2).
    ///
    /// # Description
    /// Marks the binding revoked under the per-binding lock; after this call
    /// [`lookup_binding`](Self::lookup_binding) returns `Ok(None)` for the peer,
    /// while the append-only journal preserves the full history. Journals a
    /// `pairing.revoked` event.
    ///
    /// # Arguments
    /// - `channel` (`&ChannelId`): the channel scope.
    /// - `peer` (`&PeerId`): the peer whose binding is revoked.
    /// - `now` (`Timestamp`): revocation time.
    ///
    /// # Returns
    /// `Ok(())` on success.
    ///
    /// # Errors
    /// - [`ChannelError::PairingBindingNotFound`]: no binding exists for the peer.
    /// - [`ChannelError::PairingContended`]: the per-binding lock was held.
    /// - [`ChannelError::Io`] / [`ChannelError::Serde`] / [`ChannelError::SessionStore`]:
    ///   persistence or journalling failure.
    ///
    /// # Concurrency
    /// Serializes with concurrent redemptions/revocations of the same binding
    /// via the per-binding advisory lock.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_channel::PairingStore;
    /// use harw_channel::ids::{ChannelId, PeerId};
    /// use jiff::Timestamp;
    /// let store = PairingStore::new(std::path::Path::new("/tmp/pairing"));
    /// store.revoke(
    ///     &ChannelId::from_str("telegram:ops"),
    ///     &PeerId::from_str("100"),
    ///     Timestamp::now(),
    /// )?;
    /// # Ok::<(), harw_channel::ChannelError>(())
    /// ```
    pub fn revoke(&self, channel: &ChannelId, peer: &PeerId, now: Timestamp) -> ChannelResult<()> {
        let paths = self.paths("bindings", &[channel.as_str(), peer.as_str()]);
        let lock = self.lock(&paths.lock, channel, "binding")?;
        let mutate = (|| {
            let mut record: BindingRecord =
                read_json(&paths.record)?.ok_or_else(|| ChannelError::PairingBindingNotFound {
                    channel: channel.clone(),
                    peer: peer.clone(),
                })?;
            record.revoked_at = Some(now);
            persist_json(&paths.record, &record)?;
            Ok(record)
        })();
        let record = unlock(lock, mutate)?;
        self.journal_append(channel, now, "pairing.revoked", &record)?;
        Ok(())
    }

    /// Channel-scoped replay gate for one raw update id (§3.2).
    ///
    /// # Description
    /// Records `(channel, raw_update_id)` durably the first time it is seen.
    /// Under the per-claim lock, a fresh claim persists a marker and journals a
    /// `pairing.update.claimed` event; a repeat delivery observes the existing
    /// marker and is rejected.
    ///
    /// # Arguments
    /// - `channel` (`&ChannelId`): the channel scope (claims are independent per channel).
    /// - `raw_update_id` (`&str`): the transport's raw update identifier.
    /// - `received_at` (`Timestamp`): first-seen time recorded in the marker.
    ///
    /// # Returns
    /// `Ok(true)` the first time this exact `(channel, raw_update_id)` is claimed,
    /// `Ok(false)` on any later delivery.
    ///
    /// # Errors
    /// - [`ChannelError::PairingContended`]: the per-claim lock was held.
    /// - [`ChannelError::Io`] / [`ChannelError::Serde`] / [`ChannelError::SessionStore`]:
    ///   persistence or journalling failure.
    ///
    /// # Concurrency
    /// Serializes concurrent claimants of the same key on the per-claim advisory
    /// lock; exactly one observes `true`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_channel::PairingStore;
    /// use harw_channel::ids::ChannelId;
    /// use jiff::Timestamp;
    /// let store = PairingStore::new(std::path::Path::new("/tmp/pairing"));
    /// let fresh = store.claim_once(&ChannelId::from_str("telegram:ops"), "42", Timestamp::now())?;
    /// assert!(fresh);
    /// # Ok::<(), harw_channel::ChannelError>(())
    /// ```
    pub fn claim_once(
        &self,
        channel: &ChannelId,
        raw_update_id: &str,
        received_at: Timestamp,
    ) -> ChannelResult<bool> {
        let paths = self.paths("claims", &[channel.as_str(), raw_update_id]);
        let lock = self.lock(&paths.lock, channel, "claim")?;
        let claim = (|| {
            if paths.record.exists() {
                return Ok(false);
            }
            let record = ClaimRecord {
                channel: channel.clone(),
                update_id: raw_update_id.to_owned(),
                received_at,
            };
            persist_json(&paths.record, &record)?;
            Ok(true)
        })();
        let fresh = unlock(lock, claim)?;
        if fresh {
            let record = ClaimRecord {
                channel: channel.clone(),
                update_id: raw_update_id.to_owned(),
                received_at,
            };
            self.journal_append(channel, received_at, "pairing.update.claimed", &record)?;
        }
        Ok(fresh)
    }

    // --- internals -----------------------------------------------------------

    // Filesystem paths (record + lock) for one table key.
    fn paths(&self, table: &str, parts: &[&str]) -> RecordPaths {
        let name = key_filename(parts);
        let records = self.root.join(table).join("records");
        let locks = self.root.join(table).join("locks");
        RecordPaths {
            record: records.join(&name).with_extension("json"),
            lock: locks.join(&name).with_extension("lock"),
        }
    }

    // On-demand append-only journal (never held open across calls).
    fn journal_append<T: Serialize>(
        &self,
        channel: &ChannelId,
        now: Timestamp,
        event: &str,
        payload: &T,
    ) -> ChannelResult<()> {
        let body = serde_json::json!({
            "event": event,
            "channel": channel.as_str(),
            "data": serde_json::to_value(payload)?,
        });
        let record = TranscriptRecord::new(
            pairing_journal_session_id(channel),
            ThreadRef::from_str(""),
            0,
            now,
            RecordKind::Lifecycle,
            body,
        );
        TranscriptStore::new(&self.root.join("journal")).append(&record)?;
        Ok(())
    }

    // Acquires the per-key advisory exclusive lock (JobStore idiom).
    fn lock(&self, lock_path: &Path, channel: &ChannelId, resource: &str) -> ChannelResult<File> {
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        FileExt::try_lock(&file).map_err(|error| match error {
            fs4::TryLockError::WouldBlock => ChannelError::PairingContended {
                channel: channel.clone(),
                resource: resource.to_owned(),
            },
            fs4::TryLockError::Error(error) => ChannelError::Io(error),
        })?;
        Ok(file)
    }
}

// Record + lock path pair for one table key.
struct RecordPaths {
    record: PathBuf,
    lock: PathBuf,
}

// Hex-encodes the composite key so any id (colons, dashes, unicode) is a safe,
// injective, collision-free filename.
fn key_filename(parts: &[&str]) -> String {
    let joined = parts.join(&KEY_SEP.to_string());
    let mut out = String::with_capacity(joined.len() * 2);
    for byte in joined.as_bytes() {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

// Pairing journal session identifiers are persisted through TranscriptStore,
// whose filenames intentionally accept only portable safe components. Reuse
// the injective hexadecimal key encoding so arbitrary channel identifiers
// cannot introduce separators into the transcript path.
fn pairing_journal_session_id(channel: &ChannelId) -> SessionId {
    SessionId::from_str(format!("pairing-{}", key_filename(&[channel.as_str()])))
}

// Reads a JSON record, mapping a missing file to Ok(None).
fn read_json<T: DeserializeOwned>(path: &Path) -> ChannelResult<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ChannelError::Io(error)),
    }
}

// Atomically writes a JSON record via a synced sibling temp file + rename.
fn persist_json<T: Serialize>(path: &Path, value: &T) -> ChannelResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let parent = path.parent().ok_or_else(|| {
        ChannelError::Io(std::io::Error::other("pairing record path has no parent"))
    })?;
    let mut temp = NamedTempFile::new_in(parent)?;
    serde_json::to_writer(temp.as_file_mut(), value)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|error| ChannelError::Io(error.error))?;
    Ok(())
}

// Releases the advisory lock, preferring an earlier operation error.
fn unlock<T>(lock: File, result: ChannelResult<T>) -> ChannelResult<T> {
    let released = FileExt::unlock(&lock).map_err(ChannelError::Io);
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use jiff::SignedDuration;
    use std::sync::Arc;

    // Convenience: current wall-clock time for a test.
    fn ts() -> Timestamp {
        Timestamp::now()
    }

    // Convenience: the standard ops channel used across the tests.
    fn channel_a() -> ChannelId {
        ChannelId::from_str("telegram:ops")
    }

    fn channel_b() -> ChannelId {
        ChannelId::from_str("telegram:sec")
    }

    fn tenant_ops() -> TenantId {
        TenantId::from_str("ops")
    }

    fn peer(id: &str) -> PeerId {
        PeerId::from_str(id)
    }

    #[test]
    fn test_redeem_once_binds_peer_to_tenant() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = PairingStore::new(dir.path());
        let ch = channel_a();
        let now = ts();
        let code = store
            .issue_code(&ch, &tenant_ops(), b"seed12345", now)
            .map_err(ctx("issue_code"))?;
        let binding = store
            .redeem_once(&ch, &code, &peer("100"), now)
            .map_err(ctx("redeem_once"))?;
        assert_eq!(binding.tenant, tenant_ops());
        assert_eq!(binding.peer, peer("100"));
        assert_eq!(
            store
                .lookup_binding(&ch, &peer("100"))
                .map_err(ctx("lookup_binding"))?,
            Some(tenant_ops())
        );
        Ok(())
    }

    #[test]
    fn test_expired_code_rejected() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = PairingStore::new(dir.path());
        let ch = channel_a();
        let now = ts();
        let code = store
            .issue_code(&ch, &tenant_ops(), b"seed12345", now)
            .map_err(ctx("issue_code"))?;
        let later = now
            .checked_add(SignedDuration::from_secs(16 * 60))
            .map_err(ctx("checked_add"))?;
        let result = store.redeem_once(&ch, &code, &peer("100"), later);
        assert!(matches!(result, Err(ChannelError::PairingExpired { .. })));
        Ok(())
    }

    #[test]
    fn test_already_redeemed_rejected_sequentially() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = PairingStore::new(dir.path());
        let ch = channel_a();
        let now = ts();
        let code = store
            .issue_code(&ch, &tenant_ops(), b"seed12345", now)
            .map_err(ctx("issue_code"))?;
        store
            .redeem_once(&ch, &code, &peer("100"), now)
            .map_err(ctx("redeem_once"))?;
        let second = store.redeem_once(&ch, &code, &peer("101"), now);
        assert!(matches!(
            second,
            Err(ChannelError::PairingAlreadyRedeemed { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_cross_channel_code_rejected() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = PairingStore::new(dir.path());
        let now = ts();
        let code = store
            .issue_code(&channel_a(), &tenant_ops(), b"seed12345", now)
            .map_err(ctx("issue_code"))?;
        let result = store.redeem_once(&channel_b(), &code, &peer("100"), now);
        assert!(matches!(result, Err(ChannelError::PairingInvalid { .. })));
        Ok(())
    }

    #[test]
    fn test_concurrent_double_redeem_single_winner() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(PairingStore::new(dir.path()));
        let ch = channel_a();
        let now = ts();
        let code = store
            .issue_code(&ch, &tenant_ops(), b"seed12345", now)
            .map_err(ctx("issue_code"))?;

        let mut results: Vec<ChannelResult<PairingRecord>> = Vec::new();
        let scope_result: TestResult =
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for n in 0..8u32 {
                    let store = Arc::clone(&store);
                    let ch = ch.clone();
                    let code = code.clone();
                    handles.push(scope.spawn(move || {
                        store.redeem_once(&ch, &code, &peer(&format!("2{n:02}")), now)
                    }));
                }
                for handle in handles {
                    let outcome = handle
                        .join()
                        .map_err(|_| TestError::Unexpected("Test-Thread ist paniced".to_owned()))?;
                    results.push(outcome);
                }
                Ok(())
            });
        scope_result?;

        let winners = results.iter().filter(|r| r.is_ok()).count();
        let losers = results.iter().filter(|r| r.is_err()).count();
        assert_eq!(winners, 1);
        assert_eq!(losers, 7);
        Ok(())
    }

    #[test]
    fn test_revoke_hides_binding() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = PairingStore::new(dir.path());
        let ch = channel_a();
        let now = ts();
        let code = store
            .issue_code(&ch, &tenant_ops(), b"seed12345", now)
            .map_err(ctx("issue_code"))?;
        store
            .redeem_once(&ch, &code, &peer("100"), now)
            .map_err(ctx("redeem_once"))?;
        store
            .revoke(&ch, &peer("100"), now)
            .map_err(ctx("revoke"))?;
        assert_eq!(
            store
                .lookup_binding(&ch, &peer("100"))
                .map_err(ctx("lookup_binding"))?,
            None
        );

        let missing = store.revoke(&ch, &peer("999"), now);
        assert!(matches!(
            missing,
            Err(ChannelError::PairingBindingNotFound { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_claim_once_dedups_replays() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = PairingStore::new(dir.path());
        let ch = channel_a();
        let now = ts();
        assert!(
            store
                .claim_once(&ch, "42", now)
                .map_err(ctx("claim_once"))?
        );
        assert!(
            !store
                .claim_once(&ch, "42", now)
                .map_err(ctx("claim_once"))?
        );
        assert!(
            store
                .claim_once(&ch, "43", now)
                .map_err(ctx("claim_once"))?
        );
        Ok(())
    }

    #[test]
    fn test_claim_once_is_channel_scoped() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = PairingStore::new(dir.path());
        let now = ts();
        assert!(
            store
                .claim_once(&channel_a(), "42", now)
                .map_err(ctx("claim_once"))?
        );
        assert!(
            store
                .claim_once(&channel_b(), "42", now)
                .map_err(ctx("claim_once"))?
        );
        Ok(())
    }

    #[test]
    fn pairing_journal_session_id_is_portable_and_channel_scoped() {
        let telegram = ChannelId::from_str("telegram:ops");
        let matrix = ChannelId::from_str("matrix/ops");

        let telegram_id = pairing_journal_session_id(&telegram);
        let matrix_id = pairing_journal_session_id(&matrix);

        assert!(telegram_id.as_str().starts_with("pairing-"));
        assert!(!telegram_id.as_str().contains(':'));
        assert!(!telegram_id.as_str().contains('/'));
        assert_ne!(telegram_id, matrix_id);
    }

    #[test]
    fn test_restart_safety_survives_store_recreation() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let ch = channel_a();
        let now = ts();

        let store1 = PairingStore::new(dir.path());
        let code = store1
            .issue_code(&ch, &tenant_ops(), b"seed12345", now)
            .map_err(ctx("issue_code"))?;
        store1
            .redeem_once(&ch, &code, &peer("100"), now)
            .map_err(ctx("redeem_once"))?;
        store1
            .claim_once(&ch, "99", now)
            .map_err(ctx("claim_once"))?;

        // A different peer is paired then revoked; revocation must survive restart.
        let code2 = store1
            .issue_code(&ch, &tenant_ops(), b"other6789", now)
            .map_err(ctx("issue_code"))?;
        store1
            .redeem_once(&ch, &code2, &peer("200"), now)
            .map_err(ctx("redeem_once"))?;
        store1
            .revoke(&ch, &peer("200"), now)
            .map_err(ctx("revoke"))?;

        drop(store1);
        let store2 = PairingStore::new(dir.path());

        assert_eq!(
            store2
                .lookup_binding(&ch, &peer("100"))
                .map_err(ctx("lookup_binding"))?,
            Some(tenant_ops())
        );
        assert_eq!(
            store2
                .lookup_binding(&ch, &peer("200"))
                .map_err(ctx("lookup_binding"))?,
            None
        );
        assert!(matches!(
            store2.redeem_once(&ch, &code, &peer("101"), now),
            Err(ChannelError::PairingAlreadyRedeemed { .. })
        ));
        assert!(
            !store2
                .claim_once(&ch, "99", now)
                .map_err(ctx("claim_once"))?
        );
        Ok(())
    }
}
