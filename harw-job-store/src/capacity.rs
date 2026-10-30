//! Shared destination slot reservations, using the existing durable record owner.
//!
//! This is a storage prerequisite, not placement or execution authority. The
//! trusted host configures one canonical store root and owns policy updates.
//! Every client for a node must use that root. Slots are explicit admission
//! units, not claims about measured CPU, RAM, sandbox enforcement or provider
//! quota. No provider permits are acquired here.
//! The outer capacity root must be durably provisioned (including syncing its
//! parent directory) by trusted host setup before admission. These constructors
//! open/create the root but only publish and sync the layout beneath it.
//!
//! `begin_start` durably records an uncertain effect before returning one start
//! grant. After that boundary, expiry never frees capacity: the existing job
//! supervisor must confirm effect absence through fenced reconciliation. A lost
//! acknowledgement consequently cannot authorize a duplicate start.
//! This node-local ledger does not fence one job across multiple destinations;
//! the existing admission owner must coordinate destination binding and validate
//! the live job claim before any future executor integration.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use harw_job_core::LeaseToken;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{RecordStore, StoreError, StoreRecord};

/// Bound journal growth rather than forgetting fencing tombstones silently.
const MAX_ATTEMPTS: usize = 4096;

/// Host-owned admission state, independent of caller-provided offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapacityNodeState {
    Active,
    Draining,
    Revoked,
}

/// Explicit host policy. Only the trusted destination owner may publish this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeCapacityPolicy {
    pub node: String,
    pub revision: u64,
    pub state: CapacityNodeState,
    pub slots: u64,
    pub observed_at: Timestamp,
    /// Exclusive freshness deadline; future observations are also refused.
    pub valid_until: Timestamp,
}

/// Request bound to an existing durable job claim; never a new job identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityRequest {
    pub attempt: LeaseToken,
    pub slots: u64,
    pub expires_at: Timestamp,
}

/// Exact credential required by start and reconciliation, including attempt fencing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapacityLease {
    pub node: String,
    pub epoch: u64,
    pub policy_revision: u64,
    pub attempt: LeaseToken,
    pub slots: u64,
    pub expires_at: Timestamp,
}

/// Durable recovery projection for the existing job supervisor. Read-only
/// snapshots are never permission to launch or to reuse occupied slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityRecoveryEntry {
    pub lease: CapacityLease,
    /// True requires confirmed effect absence even when the lease has expired.
    pub may_have_started: bool,
}

/// Evidence supplied by the trusted existing executor/supervisor, never by a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectAbsence {
    /// Valid only before the durable start boundary.
    NeverStarted,
    /// The existing supervisor has established no effect remains.
    ConfirmedStopped,
}

/// Precise refusal; callers must not silently fall back to another executor.
#[derive(Debug)]
pub enum CapacityError {
    Store(StoreError),
    InvalidPolicy,
    InvalidRequest,
    PolicyChanged,
    NodeUnavailable,
    StalePolicy,
    Full,
    LedgerFull,
    Overflow,
    StaleToken,
    Expired,
    AlreadyStarting,
    Released,
    EffectUncertain,
}

impl From<StoreError> for CapacityError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

impl fmt::Display for CapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "destination capacity refused: {self:?}")
    }
}

impl std::error::Error for CapacityError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Reserved,
    Starting,
    Released,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    lease: CapacityLease,
    phase: Phase,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    policy: NodeCapacityPolicy,
    epoch: u64,
    attempts: BTreeMap<String, Entry>,
}

impl StoreRecord for Ledger {
    fn record_id(&self) -> &str {
        &self.policy.node
    }
}

// Create-only bootstrap journal: never update/load through quarantine mechanics.
// Missing/corrupt capacity records must not silently reset fencing or occupancy.
#[derive(Serialize, Deserialize)]
struct Bootstrap {
    node: String,
}
impl StoreRecord for Bootstrap {
    fn record_id(&self) -> &str {
        &self.node
    }
}

/// One node ledger per record; fs4 locks serialize independent processes.
/// Use a dedicated capacity root, separate from job records.
pub struct FsNodeCapacityStore {
    records: RecordStore<Ledger>,
}

impl FsNodeCapacityStore {
    /// Use an already-open capability root without ambient filesystem access.
    #[must_use]
    pub fn from_dir(root: cap_std::fs::Dir) -> Self {
        Self {
            records: RecordStore::from_dir(root),
        }
    }

    /// Open/create a root. Trusted setup must durably provision this outer
    /// directory in its parent before enabling any admission consumer.
    pub fn create_ambient(path: &Path) -> Result<Self, CapacityError> {
        Ok(Self {
            records: RecordStore::create_ambient(path)?,
        })
    }

    /// Host bootstrap, never overwriting an existing ledger or its fencing epoch.
    /// A create-only witness survives automatic corruption quarantine. Failure
    /// between witness and ledger publication requires trusted repair; retrying
    /// initialization never resets ambiguous capacity state.
    pub fn initialize(&self, policy: NodeCapacityPolicy) -> Result<(), CapacityError> {
        validate_policy(&policy)?;
        self.records
            .root_dir()
            .create_dir_all("capacity-bootstrap")
            .map_err(StoreError::from)?;
        let bootstrap = RecordStore::<Bootstrap>::from_dir(
            self.records
                .root_dir()
                .open_dir("capacity-bootstrap")
                .map_err(StoreError::from)?,
        );
        bootstrap.create(&Bootstrap {
            node: policy.node.clone(),
        })?;
        // RecordStore fsyncs the record's directory. Publish the newly created
        // bootstrap layout and its parent before any ledger can become durable.
        crate::fsops::sync_dir(bootstrap.root_dir()).map_err(StoreError::from)?;
        crate::fsops::sync_dir(self.records.root_dir()).map_err(StoreError::from)?;
        self.records.create(&Ledger {
            policy,
            epoch: 0,
            attempts: BTreeMap::new(),
        })?;
        Ok(())
    }

    /// Recover outstanding reservations after restart, in stable work-id order.
    /// This includes expired uncertain effects and expired never-started entries;
    /// use fenced reconciliation for changes, never this snapshot for admission.
    pub fn load_recovery_set(
        &self,
        node: &str,
    ) -> Result<Vec<CapacityRecoveryEntry>, CapacityError> {
        let ledger = self.records.load(node)?;
        Ok(ledger
            .attempts
            .into_values()
            .filter(|entry| entry.phase != Phase::Released)
            .map(|entry| CapacityRecoveryEntry {
                lease: entry.lease,
                may_have_started: entry.phase == Phase::Starting,
            })
            .collect())
    }

    /// Trusted host policy publication using compare-and-swap revision fencing.
    /// Existing uncertain effects remain charged even after drain or revocation.
    pub fn update_policy(
        &self,
        expected_revision: u64,
        policy: NodeCapacityPolicy,
    ) -> Result<(), CapacityError> {
        validate_policy(&policy)?;
        self.records.update_locked(&policy.node, |ledger| {
            if ledger.policy.revision != expected_revision || policy.revision <= expected_revision {
                return Err(CapacityError::PolicyChanged);
            }
            ledger.policy = policy.clone();
            Ok(())
        })
    }

    /// Atomically charge explicit slots. Duplicate identical requests return the
    /// same credential. Expired, never-started reservations stop consuming slots;
    /// their tombstones remain so stale attempts cannot regain start authority.
    /// `clock` must read the trusted destination clock live on each invocation;
    /// it is checked again immediately before durable publication.
    pub fn reserve(
        &self,
        node: &str,
        request: CapacityRequest,
        clock: impl Fn() -> Timestamp,
    ) -> Result<CapacityLease, CapacityError> {
        let now = clock();
        if request.slots == 0 || request.expires_at <= now || request.attempt.nonce.is_empty() {
            return Err(CapacityError::InvalidRequest);
        }
        let bounds = Cell::new(None);
        self.records.update_locked_checked(
            node,
            |ledger| {
                eligible(ledger, now)?;
                bounds.set(Some((
                    ledger.policy.observed_at,
                    ledger.policy.valid_until,
                    request.expires_at,
                )));
                for entry in ledger.attempts.values_mut() {
                    if entry.phase == Phase::Reserved && entry.lease.expires_at <= now {
                        entry.phase = Phase::Released;
                    }
                }
                let id = request.attempt.work_id.as_str().to_owned();
                if let Some(entry) = ledger.attempts.get(&id) {
                    if entry.lease.attempt == request.attempt {
                        if entry.phase == Phase::Released {
                            return Err(CapacityError::Released);
                        }
                        if entry.lease.expires_at <= now {
                            return Err(CapacityError::Expired);
                        }
                        if entry.lease.slots != request.slots
                            || entry.lease.expires_at != request.expires_at
                        {
                            return Err(CapacityError::InvalidRequest);
                        }
                        if entry.lease.policy_revision != ledger.policy.revision {
                            return Err(CapacityError::PolicyChanged);
                        }
                        return Ok(entry.lease.clone());
                    }
                    if request.attempt.epoch <= entry.lease.attempt.epoch {
                        return Err(CapacityError::StaleToken);
                    }
                    if occupies(entry, now) {
                        return Err(CapacityError::EffectUncertain);
                    }
                } else if ledger.attempts.len() >= MAX_ATTEMPTS {
                    return Err(CapacityError::LedgerFull);
                }
                let used = ledger
                    .attempts
                    .values()
                    .filter(|entry| occupies(entry, now))
                    .try_fold(0_u64, |sum, entry| {
                        sum.checked_add(entry.lease.slots)
                            .ok_or(CapacityError::Overflow)
                    })?;
                if used
                    .checked_add(request.slots)
                    .ok_or(CapacityError::Overflow)?
                    > ledger.policy.slots
                {
                    return Err(CapacityError::Full);
                }
                ledger.epoch = ledger.epoch.checked_add(1).ok_or(CapacityError::Overflow)?;
                let lease = CapacityLease {
                    node: node.to_owned(),
                    epoch: ledger.epoch,
                    policy_revision: ledger.policy.revision,
                    attempt: request.attempt,
                    slots: request.slots,
                    expires_at: request.expires_at,
                };
                ledger.attempts.insert(
                    id,
                    Entry {
                        lease: lease.clone(),
                        phase: Phase::Reserved,
                    },
                );
                Ok(lease)
            },
            || commit_fresh(bounds.get(), clock()),
        )
    }

    /// Revalidate stored host policy and cross the start boundary once. The
    /// caller must still verify backend restrictions before creating the effect.
    /// `clock` must read a trusted live clock, including at the commit check.
    pub fn begin_start(
        &self,
        lease: &CapacityLease,
        clock: impl Fn() -> Timestamp,
    ) -> Result<(), CapacityError> {
        let now = clock();
        let bounds = Cell::new(None);
        self.records.update_locked_checked(
            &lease.node,
            |ledger| {
                eligible(ledger, now)?;
                if ledger.policy.revision != lease.policy_revision {
                    return Err(CapacityError::PolicyChanged);
                }
                bounds.set(Some((
                    ledger.policy.observed_at,
                    ledger.policy.valid_until,
                    lease.expires_at,
                )));
                let entry = exact_entry(ledger, lease)?;
                if lease.expires_at <= now {
                    return Err(CapacityError::Expired);
                }
                match entry.phase {
                    Phase::Reserved => {
                        entry.phase = Phase::Starting;
                        Ok(())
                    }
                    Phase::Starting => Err(CapacityError::AlreadyStarting),
                    Phase::Released => Err(CapacityError::Released),
                }
            },
            || commit_fresh(bounds.get(), clock()),
        )
    }

    /// Fenced compensation/recovery, including after expiry, drain or revocation.
    /// Disconnect and lease expiry alone are not evidence of effect absence.
    pub fn reconcile_absent(
        &self,
        lease: &CapacityLease,
        evidence: EffectAbsence,
    ) -> Result<(), CapacityError> {
        self.records.update_locked(&lease.node, |ledger| {
            let entry = exact_entry(ledger, lease)?;
            if entry.phase == Phase::Starting && evidence == EffectAbsence::NeverStarted {
                return Err(CapacityError::EffectUncertain);
            }
            entry.phase = Phase::Released;
            Ok(())
        })
    }
}

// The caller supplies a trusted live destination clock, sampled again after
// temp-file fsync immediately before publication. Static timestamps are for tests.
fn commit_fresh(
    bounds: Option<(Timestamp, Timestamp, Timestamp)>,
    now: Timestamp,
) -> Result<(), CapacityError> {
    let (observed, policy_until, lease_until) = bounds.ok_or(CapacityError::InvalidRequest)?;
    if now < observed || now >= policy_until {
        return Err(CapacityError::StalePolicy);
    }
    if now >= lease_until {
        return Err(CapacityError::Expired);
    }
    Ok(())
}

fn validate_policy(policy: &NodeCapacityPolicy) -> Result<(), CapacityError> {
    crate::validate_id(&policy.node)?;
    if policy.revision == 0 || policy.slots == 0 || policy.valid_until <= policy.observed_at {
        return Err(CapacityError::InvalidPolicy);
    }
    Ok(())
}

fn eligible(ledger: &Ledger, now: Timestamp) -> Result<(), CapacityError> {
    if ledger.policy.state != CapacityNodeState::Active {
        return Err(CapacityError::NodeUnavailable);
    }
    if now < ledger.policy.observed_at || now >= ledger.policy.valid_until {
        return Err(CapacityError::StalePolicy);
    }
    Ok(())
}

fn occupies(entry: &Entry, now: Timestamp) -> bool {
    entry.phase == Phase::Starting
        || (entry.phase == Phase::Reserved && now < entry.lease.expires_at)
}

fn exact_entry<'a>(
    ledger: &'a mut Ledger,
    lease: &CapacityLease,
) -> Result<&'a mut Entry, CapacityError> {
    let entry = ledger
        .attempts
        .get_mut(lease.attempt.work_id.as_str())
        .ok_or(CapacityError::StaleToken)?;
    if entry.lease != *lease {
        return Err(CapacityError::StaleToken);
    }
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::WorkId;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("fixture time")
    }

    fn policy(slots: u64) -> NodeCapacityPolicy {
        NodeCapacityPolicy {
            node: "node-1".into(),
            revision: 1,
            state: CapacityNodeState::Active,
            slots,
            observed_at: at(0),
            valid_until: at(1000),
        }
    }

    fn request(id: &str, epoch: u64, slots: u64) -> CapacityRequest {
        CapacityRequest {
            attempt: LeaseToken {
                work_id: WorkId::from_str(id),
                epoch,
                nonce: format!("nonce-{epoch}"),
            },
            slots,
            expires_at: at(20),
        }
    }

    #[test]
    fn independent_store_handles_share_one_ceiling_and_lock() -> TestResult {
        let root = tempfile::tempdir()?;
        let first = FsNodeCapacityStore::create_ambient(root.path())?;
        let second = FsNodeCapacityStore::create_ambient(root.path())?;
        first.initialize(policy(1))?;
        let lease = first.reserve("node-1", request("job-a", 1, 1), || at(1))?;
        assert_eq!(
            second.reserve("node-1", request("job-a", 1, 1), || at(1))?,
            lease
        );
        assert!(matches!(
            second.reserve("node-1", request("job-b", 1, 1), || at(1)),
            Err(CapacityError::Full)
        ));
        let lock = first.records.lock("node-1")?;
        assert!(matches!(
            second.reserve("node-1", request("job-b", 1, 1), || at(1)),
            Err(CapacityError::Store(StoreError::Contended { .. }))
        ));
        lock.release()?;
        Ok(())
    }

    #[test]
    fn uncertain_effect_survives_restart_expiry_and_duplicate_start() -> TestResult {
        let root = tempfile::tempdir()?;
        let first = FsNodeCapacityStore::create_ambient(root.path())?;
        first.initialize(policy(1))?;
        let lease = first.reserve("node-1", request("job-a", 1, 1), || at(1))?;
        first.begin_start(&lease, || at(2))?;
        assert!(matches!(
            first.begin_start(&lease, || at(2)),
            Err(CapacityError::AlreadyStarting)
        ));
        drop(first);
        let restarted = FsNodeCapacityStore::create_ambient(root.path())?;
        assert_eq!(
            restarted.load_recovery_set("node-1")?,
            vec![CapacityRecoveryEntry {
                lease: lease.clone(),
                may_have_started: true,
            }]
        );
        let mut next = request("job-b", 1, 1);
        next.expires_at = at(50);
        assert!(matches!(
            restarted.reserve("node-1", next.clone(), || at(21)),
            Err(CapacityError::Full)
        ));
        assert!(matches!(
            restarted.reconcile_absent(&lease, EffectAbsence::NeverStarted),
            Err(CapacityError::EffectUncertain)
        ));
        restarted.reconcile_absent(&lease, EffectAbsence::ConfirmedStopped)?;
        restarted.reserve("node-1", next, || at(21))?;
        Ok(())
    }

    #[test]
    fn expired_unstarted_reservation_frees_slots_but_old_credential_is_fenced() -> TestResult {
        let root = tempfile::tempdir()?;
        let store = FsNodeCapacityStore::create_ambient(root.path())?;
        store.initialize(policy(1))?;
        let old = store.reserve("node-1", request("job-a", 1, 1), || at(1))?;
        assert!(matches!(
            store.begin_start(&old, || at(20)),
            Err(CapacityError::Expired)
        ));
        let mut next = request("job-a", 2, 1);
        next.expires_at = at(50);
        let current = store.reserve("node-1", next, || at(20))?;
        assert!(current.epoch > old.epoch);
        assert!(matches!(
            store.reconcile_absent(&old, EffectAbsence::ConfirmedStopped),
            Err(CapacityError::StaleToken)
        ));
        assert!(matches!(
            store.begin_start(&old, || at(21)),
            Err(CapacityError::StaleToken)
        ));
        Ok(())
    }

    #[test]
    fn host_policy_is_revalidated_before_start_and_release_survives_revocation() -> TestResult {
        for state in [
            CapacityNodeState::Active,
            CapacityNodeState::Draining,
            CapacityNodeState::Revoked,
        ] {
            let root = tempfile::tempdir()?;
            let store = FsNodeCapacityStore::create_ambient(root.path())?;
            store.initialize(policy(1))?;
            let lease = store.reserve("node-1", request("job-a", 1, 1), || at(1))?;
            let mut updated = policy(1);
            updated.revision = 2;
            updated.state = state;
            store.update_policy(1, updated)?;
            assert!(matches!(
                store.begin_start(&lease, || at(2)),
                Err(CapacityError::PolicyChanged | CapacityError::NodeUnavailable)
            ));
            store.reconcile_absent(&lease, EffectAbsence::NeverStarted)?;
        }
        Ok(())
    }

    #[test]
    fn stale_future_policy_and_checked_slot_epoch_overflow_refuse() -> TestResult {
        let root = tempfile::tempdir()?;
        let store = FsNodeCapacityStore::create_ambient(root.path())?;
        store.initialize(policy(u64::MAX))?;
        assert!(matches!(
            store.reserve("node-1", request("job-a", 1, 1), || at(-1)),
            Err(CapacityError::StalePolicy)
        ));
        let mut stale = request("job-a", 1, 1);
        stale.expires_at = at(2000);
        assert!(matches!(
            store.reserve("node-1", stale, || at(1000)),
            Err(CapacityError::StalePolicy)
        ));
        store.reserve("node-1", request("job-a", 1, u64::MAX), || at(1))?;
        assert!(matches!(
            store.reserve("node-1", request("job-b", 1, 1), || at(1)),
            Err(CapacityError::Overflow)
        ));
        store
            .records
            .update_locked::<_, CapacityError, _>("node-1", |ledger| {
                ledger.attempts.clear();
                ledger.epoch = u64::MAX;
                Ok(())
            })?;
        assert!(matches!(
            store.reserve("node-1", request("job-b", 1, 1), || at(1)),
            Err(CapacityError::Overflow)
        ));
        Ok(())
    }

    #[test]
    fn reused_expired_slots_cannot_revive_when_clock_moves_backwards() -> TestResult {
        let root = tempfile::tempdir()?;
        let store = FsNodeCapacityStore::create_ambient(root.path())?;
        store.initialize(policy(1))?;
        let old = store.reserve("node-1", request("job-a", 1, 1), || at(1))?;
        let mut next = request("job-b", 1, 1);
        next.expires_at = at(50);
        store.reserve("node-1", next, || at(21))?;
        assert!(matches!(
            store.begin_start(&old, || at(19)),
            Err(CapacityError::Released)
        ));
        Ok(())
    }

    #[test]
    fn commit_rechecks_policy_and_lease_deadlines_without_publishing_start() -> TestResult {
        let root = tempfile::tempdir()?;
        let store = FsNodeCapacityStore::create_ambient(root.path())?;
        store.initialize(policy(1))?;
        let lease = store.reserve("node-1", request("job-a", 1, 1), || at(1))?;
        let calls = Cell::new(0);
        assert!(matches!(
            store.begin_start(&lease, || {
                let call = calls.get();
                calls.set(call + 1);
                if call == 0 { at(19) } else { at(20) }
            }),
            Err(CapacityError::Expired)
        ));
        assert!(!store.load_recovery_set("node-1")?[0].may_have_started);
        let mut late = request("job-b", 1, 1);
        late.expires_at = at(2000);
        calls.set(0);
        assert!(matches!(
            store.reserve("node-1", late, || {
                let call = calls.get();
                calls.set(call + 1);
                if call == 0 { at(999) } else { at(1000) }
            }),
            Err(CapacityError::StalePolicy)
        ));
        assert_eq!(store.load_recovery_set("node-1")?.len(), 1);
        Ok(())
    }

    #[test]
    fn quarantine_cannot_reset_bootstrap_epoch_or_uncertain_occupancy() -> TestResult {
        let root = tempfile::tempdir()?;
        let store = FsNodeCapacityStore::create_ambient(root.path())?;
        store.initialize(policy(1))?;
        let lease = store.reserve("node-1", request("job-a", 1, 1), || at(1))?;
        store.begin_start(&lease, || at(2))?;
        std::fs::write(root.path().join("records/node-1.json"), b"corrupt")?;
        assert!(matches!(
            store.reserve("node-1", request("job-b", 1, 1), || at(3)),
            Err(CapacityError::Store(StoreError::Corrupt { .. }))
        ));
        assert!(matches!(
            store.initialize(policy(1)),
            Err(CapacityError::Store(StoreError::AlreadyExists { .. }))
        ));
        assert!(matches!(
            store.reserve("node-1", request("job-b", 1, 1), || at(3)),
            Err(CapacityError::Store(StoreError::NotFound { .. }))
        ));
        Ok(())
    }
}
