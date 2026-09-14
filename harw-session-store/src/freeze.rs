//! Durable freeze records: which cgroup was frozen for which finding, when,
//! and whether the freeze is still in force.
//!
//! # Keying — `(CgroupId, FindingId, frozen_at)`, never `CgroupId` alone
//!
//! `harw_types::CgroupId` documents that the kernel **reuses** cgroup IDs
//! once the originating cgroup is removed and enough time or ID-space
//! pressure has passed. A store that keys a freeze by `CgroupId` alone makes
//! two mistakes, one merely wrong and one dangerous:
//!
//! - Keying a *conflict check* by `CgroupId` alone means a brand-new cgroup
//!   that happens to inherit a recycled ID can never be frozen for a second,
//!   unrelated finding while an old, unrelated freeze record for the same
//!   number is still sitting on disk — a false conflict against a record
//!   that has nothing to do with the process actually running today.
//! - Worse: a lookup keyed by `CgroupId` alone can hand a **freshly created**
//!   cgroup an **old** freeze record — or, on resolution, let a brand-new
//!   process's freeze be silently lifted by a resolve call that was really
//!   meant for the long-gone cgroup that used to hold this ID. That is a
//!   defense-evasion bug, not a cosmetic one.
//!
//! `frozen_at` is the part of the key that defeats ID reuse: the kernel can
//! recycle a `CgroupId`, but it cannot make two distinct freeze events share
//! the same instant. `FindingId` is included because a single cgroup can
//! legitimately be frozen for more than one concurrent finding (a memory
//! violation and a network-policy violation are independent facts about the
//! same cgroup) — collapsing those into one record would silently drop one
//! of them. This is spelled out in `docs/aw-contract-master.md` §B.1:
//! `Freeze` is keyed by `(CgroupId, FindingId, frozen_at)`, never by
//! `CgroupId` alone.
//!
//! Concretely, the on-disk file name is
//! `<cgroup>.<finding>.<encoded-frozen-at>.<state>.json`. Each of the three
//! key components is validated by [`safe_component`] to contain only ASCII
//! alphanumerics, `-` and `_` — which, in particular, excludes `.` — so the
//! `.`-joined file name can never be ambiguous about where one component
//! ends and the next begins.
//!
//! # What is carried over from `ChildLeaseStore` (`child_lease.rs`), and what is not
//!
//! Carried over, unchanged in spirit:
//! - **State lives in the file suffix**, not in a Rust type: `.active.json`
//!   for a freeze still in force, `.resolved.json` for one that is not. This
//!   matches `harw_home::paths::freeze_dir`'s doc, which already commits to
//!   exactly these two suffixes (freeze has no analogue of `ChildLeaseStore`'s
//!   third, `.expired.json` state — see "Expiry" below for why).
//! - **`fs4` advisory exclusive lock** around every writing operation that
//!   performs more than one filesystem step ([`FreezeStore::resolve`],
//!   [`FreezeStore::reconcile_expired`]) — and, just as deliberately, *no*
//!   lock around [`FreezeStore::freeze`], because `ChildLeaseStore::admit`
//!   does not lock either: a single atomic `create_new` needs no additional
//!   serialization. See "Concurrency" below for the precise mapping.
//! - **`tempfile` + no-clobber persist, never a direct write to the target
//!   path.** The terminal (`.resolved.json`) record is always written to a
//!   sibling temp file first and persisted into place with
//!   `persist_noclobber`, which fails if the destination already exists —
//!   the same "exactly one winner" guarantee `ChildLeaseStore::complete`
//!   relies on.
//! - **Symlink defense via [`is_regular_file`]**: every read and every
//!   existence check uses `symlink_metadata` and requires a regular file, so
//!   a symlink planted at a record path is neither read nor silently
//!   overwritten.
//! - **Reconciliation before any other operation is meaningful** — see
//!   "Expiry" below for what freeze reconciliation actually reconciles.
//!
//! Deliberately *not* carried over:
//! - **The transition mechanics differ slightly because freeze has only two
//!   states, not three.** `ChildLeaseStore` has an *intermediate* terminal
//!   state (`.expired.json`) reached by a plain, content-preserving
//!   `std::fs::rename` (cheap because the lease's bytes do not change), and
//!   a *final* state (`.completed.json`) reached by writing a new
//!   `ChildLeaseCompletionRecord` (content changes — a `completed_at` field
//!   is added) via persist-then-remove-source. A freeze only ever has one
//!   terminal state, `.resolved.json`, and reaching it always adds new
//!   content (`resolved_at`, `outcome`). So every freeze transition in this
//!   module uses the "persist new record, then remove the source" shape of
//!   `ChildLeaseStore::complete`, never the plain-`rename` shape of
//!   `ChildLeaseStore::claim_expired`. Both shapes are single-delivery-safe;
//!   this module simply never needed the cheaper one.
//! - **The double `fsync`.** Every durable write here calls `sync_data` on
//!   the file *and* opens and `sync_all`s the parent directory afterwards,
//!   via `crate::durability::sync_parent_directory`. Without the second
//!   fsync, a `rename` or `persist` that is visible in a directory listing
//!   is not guaranteed to survive a power loss on common filesystems: the
//!   directory entry update needs its own durability barrier.
//!
//!   This module originally reported that `ChildLeaseStore`, `ApprovalStore`
//!   and `JobStore` synced only the record file and never the parent
//!   directory, while `TranscriptStore` (`store.rs`) already did the full
//!   two-step. **That report was acted on**: all four now sync the parent,
//!   and the helper — which stood byte-identical in five files — lives in
//!   `crate::durability`, documented there with the reason it exists. A
//!   durability guarantee kept in five copies drifts.
//! - **No `PhantomData` typestate.** The original plan for this node
//!   sketched `Freeze<Active|Resolved>` in the shape of
//!   `harw-provider/src/marker.rs`'s zero-sized markers or
//!   `harw_dod_cap::SensorHandle<S>`'s sealed `HandleState` trait. Both of
//!   those typestates guard a *live handle* threaded through a call chain,
//!   where the type system can prove a transition happened because the old
//!   value was consumed by `self` and a new one returned. A `Freeze` is not
//!   that: it is a plain serde record that is *discovered* by reading a file
//!   whose name already told the caller which suffix it has — `active()`
//!   only ever reads `.active.json` files, `reconcile_expired` only ever
//!   promotes a `.active.json` file to `.resolved.json`. Bolting a
//!   `PhantomData<S>` marker onto that record would require a fallible
//!   runtime conversion at exactly the point (deserialization from disk)
//!   where the state is *discovered*, not asserted — buying a compile-time
//!   guarantee nowhere the compiler could not already see the answer from
//!   which method the caller called. `ChildLeaseRecord` reached the same
//!   conclusion for the same reason; `Freeze` follows it here for the same
//!   reason, not merely because it followed it there.
//!
//! # Expiry — what "expired" means for a freeze, and why it differs from a lease
//!
//! A `ChildLeaseRecord` expires because the thing it is a proxy for — a
//! running child process — has a deadline: if the deadline passes, the
//! parent is entitled to assume the child is gone. A freeze is not a proxy
//! for a process; it is a **security control**. Two failure modes pull in
//! opposite directions:
//!
//! 1. If a freeze expired merely because the orchestrator process
//!    *restarted*, a restart would double as an attacker's escape hatch —
//!    the defense would lift itself the moment the process serving it came
//!    back up, for no reason connected to whether the underlying finding was
//!    resolved. So freeze expiry must **never** be driven by process uptime
//!    or by elapsed wall-clock time alone as a default.
//! 2. But a freeze nobody ever revisits — because the operator forgot, or
//!    the escalation path that was supposed to review it never ran — must
//!    not pin a cgroup down forever either, especially once the underlying
//!    resource might legitimately need to be reused.
//!
//! The resolution here: [`Freeze::expires_at`] is `Option<jiff::Timestamp>`,
//! defaulting to `None`. **A freeze with no `expires_at` never expires on
//! its own, no matter how many times the process restarts or how much time
//! passes** — lifting it requires an explicit [`FreezeStore::resolve`] call
//! from a caller that has actually reviewed the finding. `None` is not "not
//! yet decided"; it is the *recommended* value for a genuine security
//! freeze. A caller that wants a time-bounded freeze — the deliberate
//! trade-off against failure mode 2 — sets `expires_at` explicitly at
//! creation time to a policy-chosen deadline; [`FreezeStore::reconcile_expired`]
//! then durably promotes exactly those freezes, and only those, to
//! `.resolved.json` with [`FreezeResolutionOutcome::Expired`] once `now` has
//! passed that deadline. Because the deadline is a persisted timestamp
//! compared against an *injected* `now` (never `Timestamp::now()` read
//! inside this module), reconciliation is restart-safe and deterministic:
//! it depends only on what was written to disk and the `now` the caller
//! supplies, never on how long the process itself has been running.
//!
//! Reconciliation is not invoked automatically from
//! [`FreezeStore::freeze`], [`FreezeStore::active`] or
//! [`FreezeStore::resolve`] — exactly as `ChildLeaseStore::claim_expired` is
//! a separate call the orchestrator makes on startup rather than a hook
//! inside `admit`/`complete`. The consumer for this store, node AW5-03
//! (`harw-dod-escalate`), exposes its own `reconcile_expired_freezes(store:
//! &FreezeStore, now)` free function that is expected to call
//! [`FreezeStore::reconcile_expired`] once, before any other freeze
//! operation, on startup.
//!
//! # Concurrency
//!
//! [`FreezeStore`] holds no interior mutability; it is `Send + Sync` because
//! `PathBuf` is. Locking mirrors `ChildLeaseStore` exactly, including which
//! operations *don't* lock:
//! - [`FreezeStore::freeze`] takes **no** lock, exactly like
//!   `ChildLeaseStore::admit` — its only correctness requirement is that
//!   `create_new` on the `.active.json` path is an atomic, kernel-enforced
//!   compare-and-swap, so it needs no additional serialization.
//! - [`FreezeStore::resolve`] and [`FreezeStore::reconcile_expired`] each
//!   take the store's `fs4` advisory exclusive lock (a `.lock` sidecar file
//!   under the store root) for their whole body, exactly like
//!   `ChildLeaseStore::complete`/`claim_expired` — both perform a
//!   check-then-act sequence (read the active record, persist a new
//!   resolution, remove the source) that needs the lock to stay atomic as a
//!   whole against a concurrent `resolve()`/`reconcile_expired()` on the
//!   *same* key.
//! - [`FreezeStore::active`] takes no lock either; it is safe to call
//!   concurrently with any writer because every record file this module
//!   produces is written via `create_new`/no-clobber-persist, so a reader
//!   can only ever observe a fully-old or fully-new file, never a torn
//!   write.
//!
//! Two `FreezeStore` handles pointed at *different* directories never
//! contend on anything. Multiple processes may share one directory; the
//! `fs4` lock is process-wide advisory, matching `ChildLeaseStore`.
//!
//! One narrow race falls out of `freeze()` taking no lock: if a `freeze()`
//! call and a `resolve()`/`reconcile_expired()` call race for the *exact
//! same* `(cgroup, finding, frozen_at)` triple, and the resolve/reconcile
//! side wins by removing the `.active.json` file *between* `freeze()`'s
//! resolved-path check and its `create_new`, `freeze()` can end up
//! recreating an `.active.json` for a key that was just resolved. In
//! practice this requires the caller to reuse an identical `frozen_at` —
//! ordinarily a fresh, effectively-unique instant per call — for two
//! genuinely different freeze attempts, which is the caller's own
//! responsibility to avoid, exactly as `ChildLeaseStore::admit`'s
//! lock-free `create_new` carries an analogous narrow window.
//!
//! # Integrität (A-STORE, F-179)
//!
//! [`FreezeStore::freeze`] schreibt **nicht** mehr per `create_new` +
//! `write_all` direkt ins Ziel — ein Absturz dazwischen hinterließ eine leere
//! oder halbe `.active.json`, an der jeder Scan und damit die Freeze-Abwehr
//! insgesamt scheiterte. Stattdessen: temp-Datei + `sync_all` +
//! no-replace-rename (`crate::store::persist_noclobber`) — dasselbe
//! Kernel-seitige „genau ein Gewinner“ wie `create_new`, aber atomar. Wo in
//! diesem Modul noch „`create_new`“ steht, ist diese Operation gemeint.
//!
//! Defekte Altdateien (undekodierbar, unsichere Schlüsselteile, Dateiname
//! passt nicht zum Inhalt) werden in [`FreezeStore::active`] und
//! [`FreezeStore::reconcile_expired`] nach `<name>.corrupt-<ts>` verschoben
//! und mit `tracing::error!` gemeldet, statt den ganzen Scan abzubrechen.
//! **Sicherheitshinweis:** ein so beiseitegelegter Datensatz erscheint nicht
//! mehr in `active()`; der Kernel-Freeze selbst bleibt unberührt, die Datei
//! bleibt zur Prüfung erhalten, und die `error!`-Meldung ist das Signal an
//! den Betreiber.
//!
//! # Errors
//!
//! Every fallible operation returns [`crate::error::SessionStoreResult`].
//! See [`SessionStoreError::FreezeAlreadyExists`],
//! [`SessionStoreError::FreezeAlreadyResolved`],
//! [`SessionStoreError::FreezeNotFound`],
//! [`SessionStoreError::FreezeLockContended`] and
//! [`SessionStoreError::UnsafeFreezePath`] for the variants specific to this
//! module; filesystem and JSON failures surface as
//! [`SessionStoreError::Io`] / [`SessionStoreError::Serde`].
//!
//! # Examples
//!
//! ```rust
//! use harw_session_store::{Freeze, FreezeStore};
//! use harw_types::{CgroupId, FindingId};
//! use jiff::Timestamp;
//!
//! let temp = tempfile::tempdir().unwrap();
//! let store = FreezeStore::new(temp.path());
//! let record = Freeze {
//!     cgroup: CgroupId::from_str("cgroup-42"),
//!     finding: FindingId::from_str("finding-1"),
//!     frozen_at: Timestamp::now(),
//!     expires_at: None,
//! };
//!
//! store.freeze(&record).unwrap();
//! assert_eq!(store.active().unwrap(), vec![record.clone()]);
//!
//! let resolution = store
//!     .resolve(&record.cgroup, &record.finding, record.frozen_at, Timestamp::now())
//!     .unwrap();
//! assert!(store.active().unwrap().is_empty());
//! assert_eq!(resolution.freeze, record);
//! ```

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs4::FileExt;
use harw_types::{CgroupId, FindingId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use crate::error::{SessionStoreError, SessionStoreResult};
use crate::store::{persist_noclobber, quarantine_file};

/// One durable record: which cgroup was frozen, for which finding, when, and
/// (optionally) when the freeze auto-lifts.
///
/// # Description
/// The `(cgroup, finding, frozen_at)` triple is the on-disk key — see the
/// module docs for why all three parts are required and `CgroupId` alone is
/// not enough. `expires_at` defaults to `None`, meaning the freeze never
/// lifts on its own; see the module docs ("Expiry") for the reasoning.
///
/// # Arguments
/// - `cgroup` (`CgroupId`): the frozen control group.
/// - `finding` (`FindingId`): the detection that caused the freeze.
/// - `frozen_at` (`jiff::Timestamp`): when the freeze took effect; also the
///   key component that defeats `CgroupId` reuse.
/// - `expires_at` (`Option<jiff::Timestamp>`): `None` (recommended default)
///   means the freeze never expires on its own. `Some(t)` opts into
///   [`FreezeStore::reconcile_expired`] durably lifting the freeze once an
///   injected `now >= t`.
///
/// # Returns
/// Not applicable — `Freeze` is a plain data record, not an operation.
///
/// # Errors
/// None — constructing a value performs no I/O or validation; validation
/// happens in [`FreezeStore`] methods that place a `Freeze` on disk.
///
/// # Concurrency
/// A plain, `Clone`-able value type with no interior mutability; `Send` and
/// `Sync` whenever `CgroupId`, `FindingId` and `jiff::Timestamp` are (they
/// are).
///
/// # Examples
/// ```rust
/// use harw_session_store::Freeze;
/// use harw_types::{CgroupId, FindingId};
/// use jiff::Timestamp;
///
/// let record = Freeze {
///     cgroup: CgroupId::from_str("cgroup-1"),
///     finding: FindingId::from_str("finding-1"),
///     frozen_at: Timestamp::now(),
///     expires_at: None,
/// };
/// assert_eq!(record.expires_at, None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Freeze {
    pub cgroup: CgroupId,
    pub finding: FindingId,
    pub frozen_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

/// Why a freeze reached its terminal (`.resolved.json`) state.
///
/// # Description
/// [`Lifted`](FreezeResolutionOutcome::Lifted) is a deliberate
/// [`FreezeStore::resolve`] call by a reviewer/caller.
/// [`Expired`](FreezeResolutionOutcome::Expired) is
/// [`FreezeStore::reconcile_expired`] durably promoting a freeze whose
/// caller-chosen `expires_at` has passed. There is no third outcome:
/// restart never produces one (see module docs, "Expiry").
///
/// # Arguments
/// None — a unit-variant enum.
///
/// # Returns
/// Not applicable — `FreezeResolutionOutcome` is a plain data value, not an
/// operation.
///
/// # Errors
/// None.
///
/// # Concurrency
/// `Copy`, zero-sized in effect; trivially `Send + Sync`.
///
/// # Examples
/// ```rust
/// use harw_session_store::FreezeResolutionOutcome;
///
/// let outcome = FreezeResolutionOutcome::Lifted;
/// assert_ne!(outcome, FreezeResolutionOutcome::Expired);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreezeResolutionOutcome {
    Lifted,
    Expired,
}

/// The terminal, auditable record left behind once a freeze is resolved.
///
/// # Description
/// Written to `.resolved.json` by both [`FreezeStore::resolve`] (with
/// [`FreezeResolutionOutcome::Lifted`]) and
/// [`FreezeStore::reconcile_expired`] (with
/// [`FreezeResolutionOutcome::Expired`]). Retains the original [`Freeze`] in
/// full, so the resolved file alone answers "what was frozen, why, and how
/// did it end" without needing the (by then deleted) `.active.json` source.
///
/// # Arguments
/// - `freeze` (`Freeze`): the record as it was while active.
/// - `resolved_at` (`jiff::Timestamp`): when this resolution was recorded —
///   the caller-supplied `now` for `Expired`, or the caller-supplied
///   `resolved_at` for `Lifted`.
/// - `outcome` (`FreezeResolutionOutcome`): why the freeze ended.
///
/// # Returns
/// Not applicable — `FreezeResolution` is a plain data record, not an
/// operation.
///
/// # Errors
/// None.
///
/// # Concurrency
/// A plain value type; `Send + Sync` wherever its fields are.
///
/// # Examples
/// ```rust
/// use harw_session_store::{Freeze, FreezeResolution, FreezeResolutionOutcome};
/// use harw_types::{CgroupId, FindingId};
/// use jiff::Timestamp;
///
/// let freeze = Freeze {
///     cgroup: CgroupId::from_str("cgroup-1"),
///     finding: FindingId::from_str("finding-1"),
///     frozen_at: Timestamp::now(),
///     expires_at: None,
/// };
/// let resolution = FreezeResolution {
///     freeze: freeze.clone(),
///     resolved_at: Timestamp::now(),
///     outcome: FreezeResolutionOutcome::Lifted,
/// };
/// assert_eq!(resolution.freeze, freeze);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreezeResolution {
    pub freeze: Freeze,
    pub resolved_at: Timestamp,
    pub outcome: FreezeResolutionOutcome,
}

/// File-backed freeze ledger, keyed by `(CgroupId, FindingId, frozen_at)`.
///
/// # Description
/// See the module docs for the full rationale (keying, mechanics adopted
/// from `ChildLeaseStore`, expiry policy, concurrency model). A store can be
/// reconstructed on startup; the caller is expected to call
/// [`FreezeStore::reconcile_expired`] once, before any other freeze
/// operation, exactly as callers of `ChildLeaseStore` call
/// `claim_expired`/`reconcile_expired_leases` before resuming parent
/// sessions.
///
/// # Arguments
/// None — no public fields; construct via [`FreezeStore::new`].
///
/// # Returns
/// Not applicable — `FreezeStore` is a handle type, not an operation.
///
/// # Errors
/// None from the type itself; see individual methods.
///
/// # Concurrency
/// See the module docs, "Concurrency".
///
/// # Examples
/// ```rust
/// use harw_session_store::FreezeStore;
///
/// let temp = tempfile::tempdir().unwrap();
/// let store = FreezeStore::new(temp.path());
/// assert!(store.active().unwrap().is_empty());
/// ```
pub struct FreezeStore {
    root: PathBuf,
}

impl FreezeStore {
    /// Constructs a store rooted at `root.join("freeze")`.
    ///
    /// # Description
    /// Mirrors `ChildLeaseStore::new`/`ApprovalStore::new`/`JobStore::new`:
    /// `root` is the shared parent directory (typically the harw home
    /// root), and this store derives its own canonical subdirectory name,
    /// structurally the same directory `harw_home::paths::freeze_dir(home)`
    /// resolves to (`home.join("freeze")`). This crate does not depend on
    /// `harw-home` — exactly as none of its sibling stores do — so the name
    /// is duplicated here rather than imported. The directory is not
    /// created eagerly; the first write creates it.
    ///
    /// # Arguments
    /// - `root` (`&Path`): the parent directory under which `freeze/` is
    ///   created on first write.
    ///
    /// # Returns
    /// A `FreezeStore` handle. No filesystem access occurs yet.
    ///
    /// # Errors
    /// None — infallible.
    ///
    /// # Concurrency
    /// Cheap and side-effect-free; safe to call from any thread.
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::FreezeStore;
    ///
    /// let temp = tempfile::tempdir().unwrap();
    /// let store = FreezeStore::new(temp.path());
    /// assert_eq!(store.root(), temp.path().join("freeze"));
    /// ```
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.join("freeze"),
        }
    }

    /// Borrows the store's root directory (`<root>/freeze`).
    ///
    /// # Description
    /// Exposed for tests and diagnostics, exactly as
    /// `ChildLeaseStore::root` is.
    ///
    /// # Arguments
    /// None.
    ///
    /// # Returns
    /// The directory this store reads from and writes to.
    ///
    /// # Errors
    /// None.
    ///
    /// # Concurrency
    /// A plain borrow; safe from any thread.
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::FreezeStore;
    ///
    /// let temp = tempfile::tempdir().unwrap();
    /// let store = FreezeStore::new(temp.path());
    /// assert!(store.root().ends_with("freeze"));
    /// ```
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Persists a new freeze before any escalation action assumes it is in
    /// force.
    ///
    /// # Description
    /// Writes `record` to its `.active.json` path via `create_new` (fails
    /// if anything — regular file or otherwise — already occupies that
    /// exact `(cgroup, finding, frozen_at)` path), then `sync_data`s the
    /// file and fsyncs the parent directory (see module docs on the double
    /// fsync). A freeze already resolved for the same key is also rejected:
    /// re-freezing under an identical key would silently discard the
    /// original resolution's audit trail.
    ///
    /// # Arguments
    /// - `record` (`&Freeze`): the freeze to persist.
    ///
    /// # Returns
    /// `Ok(())` once the `.active.json` file and its parent directory are
    /// durably synced.
    ///
    /// # Errors
    /// - [`SessionStoreError::FreezeAlreadyExists`]: a record (active or
    ///   resolved) already exists for the same key.
    /// - [`SessionStoreError::Io`]: filesystem failure, or the target path
    ///   is occupied by a non-regular file (symlink defense).
    /// - [`SessionStoreError::Serde`]: `record` failed to encode as JSON.
    /// - [`SessionStoreError::UnsafeFreezePath`]: `cgroup` or `finding`
    ///   contains bytes outside the safe filesystem-component alphabet.
    ///
    /// # Concurrency
    /// Takes **no** advisory lock — exactly like `ChildLeaseStore::admit`.
    /// `create_new` is itself an atomic, kernel-enforced compare-and-swap
    /// against the filesystem namespace: two concurrent `freeze()` calls for
    /// the same key can never both succeed, so no additional serialization
    /// is needed. Safe to call concurrently with `freeze()`, `resolve()` or
    /// `reconcile_expired()` for *different* keys; see the module docs for
    /// the one narrow same-key race this implies (a `freeze()` racing a
    /// `resolve()`/`reconcile_expired()` for the exact same
    /// `(cgroup, finding, frozen_at)` triple).
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::{Freeze, FreezeStore};
    /// use harw_types::{CgroupId, FindingId};
    /// use jiff::Timestamp;
    ///
    /// let temp = tempfile::tempdir().unwrap();
    /// let store = FreezeStore::new(temp.path());
    /// let record = Freeze {
    ///     cgroup: CgroupId::from_str("cgroup-1"),
    ///     finding: FindingId::from_str("finding-1"),
    ///     frozen_at: Timestamp::now(),
    ///     expires_at: None,
    /// };
    ///
    /// store.freeze(&record).unwrap();
    /// assert_eq!(store.active().unwrap(), vec![record]);
    /// ```
    pub fn freeze(&self, record: &Freeze) -> SessionStoreResult<()> {
        self.ensure_root()?;
        let active_path = self.active_path(&record.cgroup, &record.finding, record.frozen_at)?;
        let resolved_path =
            self.resolved_path(&record.cgroup, &record.finding, record.frozen_at)?;
        if is_regular_file(&resolved_path)? {
            return Err(SessionStoreError::FreezeAlreadyExists {
                cgroup: record.cgroup.clone(),
                finding: record.finding.clone(),
                frozen_at: record.frozen_at,
            });
        }
        if path_exists(&resolved_path)? {
            return Err(non_regular_freeze_path_error());
        }
        let bytes = serde_json::to_vec(record)?;
        // F-179: atomar (temp + no-replace-rename, Datei- und Eltern-fsync)
        // statt `create_new` + `write_all` direkt ins Ziel.
        match persist_noclobber(&active_path, &bytes) {
            Ok(()) => Ok(()),
            Err(SessionStoreError::PersistTargetExists { .. }) => {
                if is_regular_file(&active_path)? {
                    Err(SessionStoreError::FreezeAlreadyExists {
                        cgroup: record.cgroup.clone(),
                        finding: record.finding.clone(),
                        frozen_at: record.frozen_at,
                    })
                } else {
                    Err(non_regular_freeze_path_error())
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Lists every freeze that is still in force.
    ///
    /// # Description
    /// Reads every `.active.json` file whose key has no corresponding
    /// `.resolved.json` file yet — a resolution record wins over a stale
    /// active file a crash could have left between a resolve's write and
    /// its source cleanup, mirroring `ChildLeaseStore::active`. Ordered by
    /// `(cgroup, finding, frozen_at)` for deterministic output.
    ///
    /// # Arguments
    /// None.
    ///
    /// # Returns
    /// All currently-active freezes, sorted by cgroup, then finding, then
    /// `frozen_at`. Empty if the store directory does not exist yet.
    ///
    /// # Errors
    /// - [`SessionStoreError::Io`]: filesystem failure while listing the
    ///   directory or reading a record.
    /// - [`SessionStoreError::Serde`]: a stored record failed to decode.
    ///
    /// # Concurrency
    /// Read-only; takes no lock. Safe to call while another thread or
    /// process holds the write lock: every record file this module produces
    /// is written via `create_new`/no-clobber-persist, so a concurrent
    /// reader observes either the fully-old or the fully-new content, never
    /// a torn write.
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::FreezeStore;
    ///
    /// let temp = tempfile::tempdir().unwrap();
    /// let store = FreezeStore::new(temp.path());
    /// assert!(store.active().unwrap().is_empty());
    /// ```
    pub fn active(&self) -> SessionStoreResult<Vec<Freeze>> {
        self.records_with_suffix(".active.json")
    }

    /// Deliberately lifts an active freeze after review.
    ///
    /// # Description
    /// Reads the `.active.json` record for `(cgroup, finding, frozen_at)`,
    /// wraps it in a [`FreezeResolution`] with
    /// [`FreezeResolutionOutcome::Lifted`], persists that to
    /// `.resolved.json` via no-clobber create (the "exactly one winner"
    /// write), fsyncs, then removes the `.active.json` source. The
    /// resolution file is written *before* the source is removed, so a
    /// crash between the two never loses the resolution — a later
    /// `resolve()` or `reconcile_expired()` retry simply observes the
    /// `.resolved.json` file already present and fails/skips instead of
    /// duplicating the resolution.
    ///
    /// # Arguments
    /// - `cgroup` (`&CgroupId`): the frozen control group.
    /// - `finding` (`&FindingId`): the finding that caused the freeze.
    /// - `frozen_at` (`jiff::Timestamp`): the freeze's original timestamp —
    ///   part of the lookup key.
    /// - `resolved_at` (`jiff::Timestamp`): when this resolution is
    ///   recorded; injected by the caller, never read from the system
    ///   clock here.
    ///
    /// # Returns
    /// The [`FreezeResolution`] that was durably written.
    ///
    /// # Errors
    /// - [`SessionStoreError::FreezeAlreadyResolved`]: this key was already
    ///   resolved (by an earlier `resolve()` or by `reconcile_expired()`).
    /// - [`SessionStoreError::FreezeNotFound`]: no `.active.json` record
    ///   exists for this key (including when the path is occupied by a
    ///   non-regular file — the symlink defense refuses to read it).
    /// - [`SessionStoreError::Io`]: filesystem failure.
    /// - [`SessionStoreError::Serde`]: the stored active record failed to
    ///   decode.
    ///
    /// # Concurrency
    /// Holds the store's advisory lock for its duration. Safe to call
    /// concurrently with `freeze()`, another `resolve()` or
    /// `reconcile_expired()` targeting a *different* key.
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::{Freeze, FreezeStore};
    /// use harw_types::{CgroupId, FindingId};
    /// use jiff::Timestamp;
    ///
    /// let temp = tempfile::tempdir().unwrap();
    /// let store = FreezeStore::new(temp.path());
    /// let record = Freeze {
    ///     cgroup: CgroupId::from_str("cgroup-1"),
    ///     finding: FindingId::from_str("finding-1"),
    ///     frozen_at: Timestamp::now(),
    ///     expires_at: None,
    /// };
    /// store.freeze(&record).unwrap();
    ///
    /// let resolution = store
    ///     .resolve(&record.cgroup, &record.finding, record.frozen_at, Timestamp::now())
    ///     .unwrap();
    /// assert_eq!(resolution.freeze, record);
    /// assert!(store.active().unwrap().is_empty());
    /// ```
    pub fn resolve(
        &self,
        cgroup: &CgroupId,
        finding: &FindingId,
        frozen_at: Timestamp,
        resolved_at: Timestamp,
    ) -> SessionStoreResult<FreezeResolution> {
        self.ensure_root()?;
        let lock = self.lock()?;
        let result = (|| {
            let resolved_path = self.resolved_path(cgroup, finding, frozen_at)?;
            if is_regular_file(&resolved_path)? {
                return Err(SessionStoreError::FreezeAlreadyResolved {
                    cgroup: cgroup.clone(),
                    finding: finding.clone(),
                    frozen_at,
                });
            }
            if path_exists(&resolved_path)? {
                return Err(non_regular_freeze_path_error());
            }
            let active_path = self.active_path(cgroup, finding, frozen_at)?;
            if !is_regular_file(&active_path)? {
                return Err(SessionStoreError::FreezeNotFound {
                    cgroup: cgroup.clone(),
                    finding: finding.clone(),
                    frozen_at,
                });
            }
            let freeze = read_freeze(&active_path)?;
            let resolution = FreezeResolution {
                freeze,
                resolved_at,
                outcome: FreezeResolutionOutcome::Lifted,
            };
            persist_json(&resolved_path, &resolution)?;
            match std::fs::remove_file(&active_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(SessionStoreError::Io(error)),
            }
            Ok(resolution)
        })();
        unlock(lock, result)
    }

    /// Durably promotes every active freeze whose caller-chosen `expires_at`
    /// has passed, leaving every other active freeze untouched.
    ///
    /// # Description
    /// Scans every `.active.json` record. A record with `expires_at: None`
    /// is skipped unconditionally — see the module docs, "Expiry", for why
    /// a freeze without an explicit deadline must never be reconciled away.
    /// A record with `Some(expires_at)` where `now < expires_at` is also
    /// left untouched. Only records where `now >= expires_at` are promoted:
    /// each is wrapped in a [`FreezeResolution`] with
    /// [`FreezeResolutionOutcome::Expired`], written to `.resolved.json` via
    /// no-clobber create, fsynced, and then its `.active.json` source is
    /// removed. A key already resolved (by a concurrent `resolve()`, or a
    /// previous reconciliation) is skipped rather than treated as an error —
    /// this is a bulk sweep, not a targeted operation on one key.
    ///
    /// This is the method node AW5-03's `reconcile_expired_freezes(store:
    /// &FreezeStore, now)` is expected to call once at startup, before any
    /// other freeze operation.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): the instant to reconcile against,
    ///   injected by the caller so reconciliation is deterministic and
    ///   testable against a fixture rather than the system clock.
    ///
    /// # Returns
    /// The freezes that were promoted to `.resolved.json` by this call
    /// (their in-memory `Freeze` value, as it was while active — not the
    /// wrapping [`FreezeResolution`]). Empty if nothing was due.
    ///
    /// # Errors
    /// - [`SessionStoreError::Io`]: filesystem failure while listing the
    ///   directory, reading a record, or persisting a resolution.
    /// - [`SessionStoreError::Serde`]: a stored record failed to decode or
    ///   a resolution failed to encode.
    ///
    /// # Concurrency
    /// Holds the store's advisory lock for its duration, exactly like
    /// `freeze()` and `resolve()`. Idempotent for a fixed `now`: a second
    /// call with the same `now` after the first has completed returns an
    /// empty `Vec`, because every record it would have promoted has already
    /// moved to `.resolved.json`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::{Freeze, FreezeStore};
    /// use harw_types::{CgroupId, FindingId};
    /// use jiff::{SignedDuration, Timestamp};
    ///
    /// let temp = tempfile::tempdir().unwrap();
    /// let store = FreezeStore::new(temp.path());
    /// let now = Timestamp::now();
    /// let bounded = Freeze {
    ///     cgroup: CgroupId::from_str("cgroup-1"),
    ///     finding: FindingId::from_str("finding-1"),
    ///     frozen_at: now.checked_sub(SignedDuration::from_secs(120)).unwrap(),
    ///     expires_at: Some(now.checked_sub(SignedDuration::from_secs(1)).unwrap()),
    /// };
    /// store.freeze(&bounded).unwrap();
    ///
    /// let expired = store.reconcile_expired(now).unwrap();
    /// assert_eq!(expired, vec![bounded]);
    /// assert!(store.active().unwrap().is_empty());
    /// ```
    pub fn reconcile_expired(&self, now: Timestamp) -> SessionStoreResult<Vec<Freeze>> {
        self.ensure_root()?;
        let lock = self.lock()?;
        let result = (|| {
            let mut expired = Vec::new();
            for entry in std::fs::read_dir(&self.root)? {
                let entry = entry?;
                let path = entry.path();
                if !has_suffix(&path, ".active.json") || !is_regular_file(&path)? {
                    continue;
                }
                let Some(freeze) = self.load_scanned(&path, "active")? else {
                    continue;
                };
                let Some(expires_at) = freeze.expires_at else {
                    continue;
                };
                if now < expires_at {
                    continue;
                }
                let resolved_path =
                    self.resolved_path(&freeze.cgroup, &freeze.finding, freeze.frozen_at)?;
                if path_exists(&resolved_path)? {
                    continue;
                }
                let resolution = FreezeResolution {
                    freeze: freeze.clone(),
                    resolved_at: now,
                    outcome: FreezeResolutionOutcome::Expired,
                };
                persist_json(&resolved_path, &resolution)?;
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(SessionStoreError::Io(error)),
                }
                expired.push(freeze);
            }
            Ok(expired)
        })();
        unlock(lock, result)
    }

    fn records_with_suffix(&self, suffix: &str) -> SessionStoreResult<Vec<Freeze>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if !has_suffix(&path, suffix) || !is_regular_file(&path)? {
                continue;
            }
            let state = suffix
                .strip_prefix('.')
                .and_then(|rest| rest.strip_suffix(".json"))
                .unwrap_or(suffix);
            let Some(freeze) = self.load_scanned(&path, state)? else {
                continue;
            };
            let resolved_path =
                self.resolved_path(&freeze.cgroup, &freeze.finding, freeze.frozen_at)?;
            if !is_regular_file(&resolved_path)? {
                records.push(freeze);
            }
        }
        records.sort_by(|left, right| {
            left.cgroup
                .as_str()
                .cmp(right.cgroup.as_str())
                .then_with(|| left.finding.as_str().cmp(right.finding.as_str()))
                .then_with(|| left.frozen_at.cmp(&right.frozen_at))
        });
        Ok(records)
    }

    // Liest einen beim Scan gefundenen Datensatz; defekte Dateien werden in
    // Quarantäne verschoben (`error!`, sicherheitsrelevant) und übersprungen.
    fn load_scanned(&self, path: &Path, state: &str) -> SessionStoreResult<Option<Freeze>> {
        let detail = match read_freeze(path) {
            Ok(freeze) => match self.path(
                &freeze.cgroup,
                &freeze.finding,
                freeze.frozen_at,
                state,
            ) {
                Ok(expected) if expected == path => return Ok(Some(freeze)),
                Ok(_) => "freeze file name does not match its key".to_owned(),
                Err(error) => error.to_string(),
            },
            Err(SessionStoreError::Serde(error)) => error.to_string(),
            Err(SessionStoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        match quarantine_file(path) {
            Ok(Some(quarantine)) => tracing::error!(
                path = %path.display(),
                quarantine = %quarantine.display(),
                detail = %detail,
                "corrupt freeze record quarantined; it is no longer listed as active"
            ),
            Ok(None) => {}
            Err(error) => tracing::error!(
                path = %path.display(),
                error = %error,
                detail = %detail,
                "corrupt freeze record could not be quarantined; skipped"
            ),
        }
        Ok(None)
    }

    fn ensure_root(&self) -> SessionStoreResult<()> {
        std::fs::create_dir_all(&self.root)?;
        Ok(())
    }

    fn lock(&self) -> SessionStoreResult<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(".lock"))?;
        FileExt::try_lock(&file).map_err(|error| match error {
            fs4::TryLockError::WouldBlock => SessionStoreError::FreezeLockContended,
            fs4::TryLockError::Error(error) => SessionStoreError::Io(error),
        })?;
        Ok(file)
    }

    fn active_path(
        &self,
        cgroup: &CgroupId,
        finding: &FindingId,
        frozen_at: Timestamp,
    ) -> SessionStoreResult<PathBuf> {
        self.path(cgroup, finding, frozen_at, "active")
    }

    fn resolved_path(
        &self,
        cgroup: &CgroupId,
        finding: &FindingId,
        frozen_at: Timestamp,
    ) -> SessionStoreResult<PathBuf> {
        self.path(cgroup, finding, frozen_at, "resolved")
    }

    fn path(
        &self,
        cgroup: &CgroupId,
        finding: &FindingId,
        frozen_at: Timestamp,
        state: &str,
    ) -> SessionStoreResult<PathBuf> {
        let cgroup_component = safe_component(cgroup.as_str())?;
        let finding_component = safe_component(finding.as_str())?;
        let frozen_at_component = encode_frozen_at(frozen_at);
        Ok(self.root.join(format!(
            "{cgroup_component}.{finding_component}.{frozen_at_component}.{state}.json"
        )))
    }
}

fn unlock<T>(lock: File, result: SessionStoreResult<T>) -> SessionStoreResult<T> {
    let unlock = FileExt::unlock(&lock).map_err(SessionStoreError::Io);
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

/// Writes `value` via `crate::store::persist_noclobber`: sibling temp file,
/// `sync_all`, no-clobber persist to `path`, parent-directory fsync.
///
/// This is the "exactly one winner" write every terminal freeze record uses
/// (see module docs).
fn persist_json<T: Serialize>(path: &Path, value: &T) -> SessionStoreResult<()> {
    let bytes = serde_json::to_vec(value)?;
    persist_noclobber(path, &bytes)
}

// Der Eltern-fsync (`crate::durability::sync_parent_directory`) läuft seit
// A-STORE ausschließlich über `crate::store::persist_noclobber`.

fn has_suffix(path: &Path, suffix: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(suffix))
}

fn path_exists(path: &Path) -> SessionStoreResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SessionStoreError::Io(error)),
    }
}

fn non_regular_freeze_path_error() -> SessionStoreError {
    SessionStoreError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "freeze path is occupied by a non-regular file",
    ))
}

fn is_regular_file(path: &Path) -> SessionStoreResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SessionStoreError::Io(error)),
    }
}

fn read_freeze(path: &Path) -> SessionStoreResult<Freeze> {
    if !is_regular_file(path)? {
        return Err(SessionStoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "freeze record is not a regular file",
        )));
    }
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Validates one filename component (`cgroup` or `finding`): non-empty,
/// ASCII alphanumeric/`-`/`_` only. Rejecting `.` is what lets
/// [`FreezeStore::path`] join components with a plain `.` unambiguously.
fn safe_component(value: &str) -> SessionStoreResult<&str> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SessionStoreError::UnsafeFreezePath(value.to_owned()));
    }
    Ok(value)
}

/// Encodes a timestamp as an injective, filesystem-safe decimal component:
/// a `p`/`m` sign prefix (never a literal `-`) followed by the zero-padded
/// magnitude of whole nanoseconds since the Unix epoch. Full nanosecond
/// precision makes this injective, so two distinct `frozen_at` values can
/// never collide on disk.
fn encode_frozen_at(frozen_at: Timestamp) -> String {
    let nanos = frozen_at.as_nanosecond();
    if nanos.is_negative() {
        format!("m{:020}", nanos.unsigned_abs())
    } else {
        format!("p{nanos:020}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::SignedDuration;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn freeze(cgroup: &str, finding: &str, frozen_at: Timestamp) -> Freeze {
        Freeze {
            cgroup: CgroupId::from_str(cgroup),
            finding: FindingId::from_str(finding),
            frozen_at,
            expires_at: None,
        }
    }

    fn root_entries(store: &FreezeStore) -> Vec<String> {
        std::fs::read_dir(store.root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_freeze_is_written_and_read_back() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let record = freeze("cgroup-1", "finding-1", Timestamp::now());

        store.freeze(&record).unwrap();

        assert_eq!(store.active().unwrap(), vec![record]);
    }

    #[test]
    fn two_freezes_on_the_same_cgroup_with_different_findings_coexist() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let now = Timestamp::now();
        let memory_finding = freeze("cgroup-shared", "finding-memory", now);
        let network_finding = freeze(
            "cgroup-shared",
            "finding-network",
            now.checked_add(SignedDuration::from_secs(1)).unwrap(),
        );

        store.freeze(&memory_finding).unwrap();
        store.freeze(&network_finding).unwrap();

        let mut active = store.active().unwrap();
        active.sort_by(|left, right| left.finding.as_str().cmp(right.finding.as_str()));
        assert_eq!(active, vec![memory_finding, network_finding]);
    }

    /// The test that carries the keying decision: a `CgroupId` the kernel
    /// has recycled must not inherit an old, already-resolved freeze record
    /// for the same (cgroup, finding) pair.
    #[test]
    fn a_reused_cgroup_id_does_not_run_into_an_old_resolved_record() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let old_frozen_at = Timestamp::now();
        let old = freeze("cgroup-reused", "finding-reused", old_frozen_at);
        store.freeze(&old).unwrap();
        let old_resolution = store
            .resolve(&old.cgroup, &old.finding, old_frozen_at, Timestamp::now())
            .unwrap();

        // Time passes; the kernel recycles `cgroup-reused` for an unrelated
        // process that trips the *same kind* of finding.
        let new_frozen_at = old_frozen_at.checked_add(SignedDuration::from_secs(3600)).unwrap();
        let new = freeze("cgroup-reused", "finding-reused", new_frozen_at);

        // Must not be rejected as a duplicate of the old, resolved freeze.
        store.freeze(&new).unwrap();

        assert_eq!(store.active().unwrap(), vec![new]);
        // The old resolution is untouched — the new freeze did not merge
        // into or overwrite it.
        assert_eq!(old_resolution.freeze, old);
        assert_eq!(old_resolution.freeze.frozen_at, old_frozen_at);
    }

    #[test]
    fn resolving_a_freeze_leaves_exactly_one_file_for_its_key() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let record = freeze("cgroup-1", "finding-1", Timestamp::now());
        store.freeze(&record).unwrap();

        store
            .resolve(&record.cgroup, &record.finding, record.frozen_at, Timestamp::now())
            .unwrap();

        let matching: Vec<String> = root_entries(&store)
            .into_iter()
            .filter(|name| name.starts_with("cgroup-1.finding-1."))
            .collect();
        assert_eq!(matching.len(), 1, "expected exactly one file, found {matching:?}");
        assert!(matching[0].ends_with(".resolved.json"));
    }

    #[test]
    fn reconcile_expired_resolves_due_freezes_and_leaves_others_active() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let now = Timestamp::now();

        let due = Freeze {
            expires_at: Some(now.checked_sub(SignedDuration::from_secs(1)).unwrap()),
            ..freeze(
                "cgroup-due",
                "finding-1",
                now.checked_sub(SignedDuration::from_secs(120)).unwrap(),
            )
        };
        let not_yet_due = Freeze {
            expires_at: Some(now.checked_add(SignedDuration::from_secs(3600)).unwrap()),
            ..freeze("cgroup-bounded-future", "finding-1", now)
        };
        let never_expires = freeze("cgroup-indefinite", "finding-1", now);
        store.freeze(&due).unwrap();
        store.freeze(&not_yet_due).unwrap();
        store.freeze(&never_expires).unwrap();

        let expired = store.reconcile_expired(now).unwrap();

        assert_eq!(expired, vec![due]);
        let mut still_active = store.active().unwrap();
        still_active.sort_by(|left, right| left.cgroup.as_str().cmp(right.cgroup.as_str()));
        // Alphabetisch: `cgroup-bounded-future` (= `not_yet_due`) steht vor
        // `cgroup-indefinite` (= `never_expires`). Die Erwartung stand zuvor
        // in Einfügereihenfolge, obwohl der Test selbst sortiert.
        assert_eq!(still_active, vec![not_yet_due, never_expires]);
    }

    #[cfg(unix)]
    #[test]
    fn active_and_resolve_ignore_a_symlinked_active_file() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        std::fs::create_dir_all(store.root()).unwrap();
        let linked = freeze("cgroup-linked", "finding-linked", Timestamp::now());
        let target = temp.path().join("outside-active.json");
        std::fs::write(&target, serde_json::to_vec(&linked).unwrap()).unwrap();
        let link_path = store.active_path(&linked.cgroup, &linked.finding, linked.frozen_at).unwrap();
        symlink(&target, &link_path).unwrap();

        assert!(store.active().unwrap().is_empty());
        assert!(matches!(
            store.resolve(&linked.cgroup, &linked.finding, linked.frozen_at, Timestamp::now()),
            Err(SessionStoreError::FreezeNotFound { .. })
        ));
        assert!(std::fs::symlink_metadata(&link_path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
    }

    #[test]
    fn no_temp_file_remains_after_a_freeze_and_a_resolve() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let record = freeze("cgroup-1", "finding-1", Timestamp::now());
        store.freeze(&record).unwrap();
        store
            .resolve(&record.cgroup, &record.finding, record.frozen_at, Timestamp::now())
            .unwrap();

        for name in root_entries(&store) {
            assert!(
                name == ".lock" || name.ends_with(".json"),
                "unexpected leftover file: {name}"
            );
        }
    }

    #[test]
    fn reconcile_expired_is_idempotent_for_the_same_store_and_now() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let now = Timestamp::now();
        let due = Freeze {
            expires_at: Some(now.checked_sub(SignedDuration::from_secs(1)).unwrap()),
            ..freeze("cgroup-1", "finding-1", now.checked_sub(SignedDuration::from_secs(60)).unwrap())
        };
        store.freeze(&due).unwrap();

        let first = store.reconcile_expired(now).unwrap();
        let second = store.reconcile_expired(now).unwrap();

        assert_eq!(first, vec![due]);
        assert!(second.is_empty());
    }

    #[test]
    fn reconcile_expired_is_deterministic_given_the_same_injected_now() {
        let now = "2026-01-01T00:00:00Z".parse::<Timestamp>().unwrap();
        let frozen_at = now.checked_sub(SignedDuration::from_secs(60)).unwrap();
        let expires_at = now.checked_sub(SignedDuration::from_secs(1)).unwrap();
        let build = |root: &Path| {
            let store = FreezeStore::new(root);
            let due = Freeze {
                expires_at: Some(expires_at),
                ..freeze("cgroup-1", "finding-1", frozen_at)
            };
            store.freeze(&due).unwrap();
            store
        };

        let temp_a = tempfile::tempdir().unwrap();
        let temp_b = tempfile::tempdir().unwrap();
        let store_a = build(temp_a.path());
        let store_b = build(temp_b.path());

        let result_a = store_a.reconcile_expired(now).unwrap();
        let result_b = store_b.reconcile_expired(now).unwrap();

        assert_eq!(result_a, result_b);
    }
}
