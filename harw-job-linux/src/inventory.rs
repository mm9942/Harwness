//! Pure process inventory reducer for the Linux user-workload host tests
//! H04 (`stale_pid_event_cannot_mutate_new_incarnation`) and H05
//! (`event_loss_requires_visible_resync`) of the
//! `docs/planning/linux-user-cloud-control` dossier.
//!
//! # Placement is provisional
//! The dossier (`05-rust-integration.md`) proposes dedicated
//! `harw-host-model` / `harw-host-inventory` crates for this logic. Until
//! those exist the reducer lives here; it is platform-neutral, so the module
//! is not gated on `target_os`.
//!
//! # Contract
//! - A process incarnation is identified by [`ProcessKey`]
//!   (`boot_id`, `tgid`, `birth_ticks`). A bare tgid never identifies a
//!   process. A key whose `birth_ticks` is `None` is *unknown*: its record
//!   is read-only and can never be changed by later events.
//! - [`InventoryState::apply_observation`] reduces one event. Events for a
//!   different incarnation of the same tgid are [`ApplyOutcome::Stale`] and
//!   mutate nothing. The one exception: a `Created` event on the same boot
//!   with a strictly newer, known `birth_ticks` supersedes the old record
//!   (PID reuse discovered through events).
//! - Missing information is never turned into a death: `Unreadable` and
//!   "missing from a snapshot" both yield
//!   [`ProcessObservationState::Unverifiable`]; only an explicit
//!   `ExitConfirmed` observation yields `ExitConfirmed`.
//! - Loss of events is visible: [`InventoryState::note_gap`] forces
//!   [`Coverage::ResyncRequired`]; hitting the capacity bound degrades
//!   coverage to [`Coverage::Degraded`] instead of dropping records.
//!   [`InventoryState::reconcile_snapshot`] is the only way back to
//!   [`Coverage::Reconciled`].
//!
//! The reducer reads no clock and performs no I/O.

use std::collections::{BTreeMap, BTreeSet};

/// Identity of one process incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessKey {
    /// Boot identity of the host the observation belongs to.
    pub boot_id: u64,
    /// Thread group id (the PID as seen by userspace).
    pub tgid: u32,
    /// Process start time in clock ticks. `None` means unknown and makes
    /// the record read-only.
    pub birth_ticks: Option<u64>,
}

/// What the inventory knows about the liveness of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessObservationState {
    /// Observed alive.
    Live,
    /// Exit was explicitly confirmed. Terminal for this incarnation.
    ExitConfirmed,
    /// Liveness cannot be established (unreadable, or missing from a
    /// snapshot). Not the same as dead.
    Unverifiable,
}

/// One input event for the reducer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// A process was created.
    Created(ProcessKey),
    /// A process executed a new image.
    Exec(ProcessKey),
    /// One task (thread) of the process exited; the process may live on.
    TaskExit(ProcessKey),
    /// The exit of the whole process was confirmed.
    ExitConfirmed(ProcessKey),
    /// The process could not be read.
    Unreadable(ProcessKey),
}

impl Observation {
    /// The key this observation refers to.
    #[must_use]
    pub fn key(self) -> ProcessKey {
        match self {
            Self::Created(key)
            | Self::Exec(key)
            | Self::TaskExit(key)
            | Self::ExitConfirmed(key)
            | Self::Unreadable(key) => key,
        }
    }
}

/// Result of [`InventoryState::apply_observation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The observation changed the inventory.
    Applied,
    /// The observation was already reflected; nothing changed.
    Duplicate,
    /// The observation concerns another incarnation (or a read-only or
    /// terminal record); nothing changed.
    Stale,
    /// Coverage is [`Coverage::ResyncRequired`]; nothing changed.
    ResyncRequired,
    /// A new record would exceed the capacity; coverage is now
    /// [`Coverage::Degraded`] and nothing was changed or dropped.
    CapacityExceeded,
}

/// How far the inventory can be trusted to be complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// No snapshot has been reconciled yet.
    Bootstrapping,
    /// Reconciled against a snapshot of the given generation.
    Reconciled {
        /// Snapshot generation.
        generation: u64,
    },
    /// Records may be missing (capacity exceeded).
    Degraded {
        /// Generation of the last reconcile (0 before the first).
        generation: u64,
    },
    /// Events were lost; a reconcile is required before trusting events.
    ResyncRequired {
        /// Generation of the last reconcile (0 before the first).
        generation: u64,
    },
}

impl Coverage {
    fn generation(self) -> u64 {
        match self {
            Self::Bootstrapping => 0,
            Self::Reconciled { generation }
            | Self::Degraded { generation }
            | Self::ResyncRequired { generation } => generation,
        }
    }
}

/// Inventory record of one process incarnation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Identity of the incarnation.
    pub key: ProcessKey,
    /// Liveness knowledge.
    pub state: ProcessObservationState,
    /// Number of `Exec` observations applied.
    pub exec_count: u32,
    /// Whether any task exit was observed. Does not end the process.
    pub task_exited: bool,
}

impl Record {
    fn live(key: ProcessKey) -> Self {
        Self {
            key,
            state: ProcessObservationState::Live,
            exec_count: 0,
            task_exited: false,
        }
    }

    fn from_observation(obs: Observation) -> Self {
        let mut record = Self::live(obs.key());
        match obs {
            Observation::Created(_) => {}
            Observation::Exec(_) => record.exec_count = 1,
            Observation::TaskExit(_) => record.task_exited = true,
            Observation::ExitConfirmed(_) => record.state = ProcessObservationState::ExitConfirmed,
            Observation::Unreadable(_) => record.state = ProcessObservationState::Unverifiable,
        }
        record
    }

    /// A record without a known birth cannot be told apart from its
    /// successors, so it is never modified by events.
    fn is_read_only(&self) -> bool {
        self.key.birth_ticks.is_none()
    }

    /// Whether `key` provably names a different incarnation.
    fn provably_differs(&self, key: &ProcessKey) -> bool {
        self.key.boot_id != key.boot_id
            || matches!(
                (self.key.birth_ticks, key.birth_ticks),
                (Some(old), Some(new)) if old != new
            )
    }
}

/// Bounded process inventory keyed by tgid.
#[derive(Debug, Clone)]
pub struct InventoryState {
    records: BTreeMap<u32, Record>,
    capacity: usize,
    coverage: Coverage,
}

impl InventoryState {
    /// Creates an empty inventory holding at most `capacity` records.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            records: BTreeMap::new(),
            capacity,
            coverage: Coverage::Bootstrapping,
        }
    }

    /// Current coverage.
    #[must_use]
    pub fn coverage(&self) -> Coverage {
        self.coverage
    }

    /// Record of `tgid`, if any.
    #[must_use]
    pub fn get(&self, tgid: u32) -> Option<&Record> {
        self.records.get(&tgid)
    }

    /// Number of records held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the inventory holds no records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Notes lost, reordered or otherwise untrusted events (H05): coverage
    /// becomes [`Coverage::ResyncRequired`] until the next reconcile.
    pub fn note_gap(&mut self) {
        self.coverage = Coverage::ResyncRequired {
            generation: self.coverage.generation(),
        };
    }

    fn degrade(&mut self) {
        self.coverage = Coverage::Degraded {
            generation: self.coverage.generation(),
        };
    }

    /// Reduces one observation. See the module docs for the rules.
    pub fn apply_observation(&mut self, obs: Observation) -> ApplyOutcome {
        if matches!(self.coverage, Coverage::ResyncRequired { .. }) {
            return ApplyOutcome::ResyncRequired;
        }
        let key = obs.key();
        if !self.records.contains_key(&key.tgid) {
            if self.records.len() >= self.capacity {
                self.degrade();
                return ApplyOutcome::CapacityExceeded;
            }
            self.records.insert(key.tgid, Record::from_observation(obs));
            return ApplyOutcome::Applied;
        }
        let Some(record) = self.records.get_mut(&key.tgid) else {
            return ApplyOutcome::Stale;
        };

        if record.key != key {
            // Only a Created with a strictly newer known birth on the same
            // boot may supersede (PID reuse); everything else is stale.
            let supersedes = matches!(obs, Observation::Created(_))
                && record.key.boot_id == key.boot_id
                && matches!(
                    (record.key.birth_ticks, key.birth_ticks),
                    (Some(old), Some(new)) if new > old
                );
            if supersedes {
                *record = Record::live(key);
                return ApplyOutcome::Applied;
            }
            return ApplyOutcome::Stale;
        }

        if matches!(obs, Observation::Created(_)) {
            return ApplyOutcome::Duplicate;
        }
        if record.is_read_only() {
            return ApplyOutcome::Stale;
        }
        let confirmed = record.state == ProcessObservationState::ExitConfirmed;
        match obs {
            Observation::Created(_) => ApplyOutcome::Duplicate,
            Observation::Exec(_) => {
                if confirmed {
                    return ApplyOutcome::Stale;
                }
                record.exec_count = record.exec_count.saturating_add(1);
                record.state = ProcessObservationState::Live;
                ApplyOutcome::Applied
            }
            Observation::TaskExit(_) => {
                if record.task_exited {
                    return ApplyOutcome::Duplicate;
                }
                record.task_exited = true;
                ApplyOutcome::Applied
            }
            Observation::ExitConfirmed(_) => {
                if confirmed {
                    return ApplyOutcome::Duplicate;
                }
                record.state = ProcessObservationState::ExitConfirmed;
                ApplyOutcome::Applied
            }
            Observation::Unreadable(_) => {
                if confirmed {
                    return ApplyOutcome::Stale;
                }
                if record.state == ProcessObservationState::Unverifiable {
                    return ApplyOutcome::Duplicate;
                }
                record.state = ProcessObservationState::Unverifiable;
                ApplyOutcome::Applied
            }
        }
    }

    /// Reconciles the inventory against a bounded scan of live processes.
    ///
    /// Records missing from `live` become `Unverifiable` (never dead) unless
    /// already `ExitConfirmed`; live keys are inserted or refreshed.
    /// Coverage becomes [`Coverage::Reconciled`] with `generation`, or
    /// [`Coverage::Degraded`] if some live key could not be recorded
    /// (capacity, or an unknown birth that cannot be matched).
    pub fn reconcile_snapshot(&mut self, live: &[ProcessKey], generation: u64) {
        let live_set: BTreeSet<ProcessKey> = live.iter().copied().collect();
        for record in self.records.values_mut() {
            if record.state != ProcessObservationState::ExitConfirmed
                && !live_set.contains(&record.key)
            {
                record.state = ProcessObservationState::Unverifiable;
            }
        }

        let mut incomplete = false;
        for key in &live_set {
            if let Some(record) = self.records.get_mut(&key.tgid) {
                if record.key == *key {
                    if record.state == ProcessObservationState::Unverifiable {
                        record.state = ProcessObservationState::Live;
                    }
                } else if record.provably_differs(key) {
                    *record = Record::live(*key);
                } else {
                    incomplete = true;
                }
            } else if self.records.len() >= self.capacity {
                incomplete = true;
            } else {
                self.records.insert(key.tgid, Record::live(*key));
            }
        }

        self.coverage = if incomplete {
            Coverage::Degraded { generation }
        } else {
            Coverage::Reconciled { generation }
        };
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn key(tgid: u32, birth: u64) -> ProcessKey {
        ProcessKey {
            boot_id: 1,
            tgid,
            birth_ticks: Some(birth),
        }
    }

    fn record(inv: &InventoryState, tgid: u32) -> TestResult<&Record> {
        inv.get(tgid).ok_or(TestError::Missing("record"))
    }

    #[test]
    fn test_stale_pid_cannot_touch_reused_pid() -> TestResult {
        let mut inv = InventoryState::new(8);
        assert_eq!(
            inv.apply_observation(Observation::Created(key(10, 100))),
            ApplyOutcome::Applied
        );
        // PID 10 is reused by a newer incarnation.
        assert_eq!(
            inv.apply_observation(Observation::Created(key(10, 200))),
            ApplyOutcome::Applied
        );
        let before = record(&inv, 10)?.clone();
        // Old evidence must not touch the replacement.
        for obs in [
            Observation::Exec(key(10, 100)),
            Observation::TaskExit(key(10, 100)),
            Observation::ExitConfirmed(key(10, 100)),
            Observation::Unreadable(key(10, 100)),
        ] {
            assert_eq!(inv.apply_observation(obs), ApplyOutcome::Stale);
        }
        assert_eq!(record(&inv, 10)?, &before);
        assert_eq!(before.key, key(10, 200));
        assert_eq!(before.state, ProcessObservationState::Live);
        Ok(())
    }

    #[test]
    fn test_other_boot_is_stale() -> TestResult {
        let mut inv = InventoryState::new(8);
        inv.apply_observation(Observation::Created(key(10, 100)));
        let other_boot = ProcessKey {
            boot_id: 2,
            ..key(10, 100)
        };
        assert_eq!(
            inv.apply_observation(Observation::ExitConfirmed(other_boot)),
            ApplyOutcome::Stale
        );
        assert_eq!(record(&inv, 10)?.state, ProcessObservationState::Live);
        Ok(())
    }

    #[test]
    fn test_duplicate_create() -> TestResult {
        let mut inv = InventoryState::new(8);
        assert_eq!(
            inv.apply_observation(Observation::Created(key(5, 50))),
            ApplyOutcome::Applied
        );
        assert_eq!(
            inv.apply_observation(Observation::Created(key(5, 50))),
            ApplyOutcome::Duplicate
        );
        assert_eq!(inv.len(), 1);
        assert_eq!(record(&inv, 5)?.exec_count, 0);
        Ok(())
    }

    #[test]
    fn test_task_exit_does_not_end_process() -> TestResult {
        let mut inv = InventoryState::new(8);
        inv.apply_observation(Observation::Created(key(7, 70)));
        assert_eq!(
            inv.apply_observation(Observation::TaskExit(key(7, 70))),
            ApplyOutcome::Applied
        );
        let rec = record(&inv, 7)?;
        assert!(rec.task_exited);
        assert_eq!(rec.state, ProcessObservationState::Live);
        Ok(())
    }

    #[test]
    fn test_unreadable_never_yields_exit_confirmed() -> TestResult {
        let mut inv = InventoryState::new(8);
        inv.apply_observation(Observation::Created(key(8, 80)));
        assert_eq!(
            inv.apply_observation(Observation::Unreadable(key(8, 80))),
            ApplyOutcome::Applied
        );
        assert_eq!(
            record(&inv, 8)?.state,
            ProcessObservationState::Unverifiable
        );

        // Unreadable for an unknown tgid is also not an exit.
        inv.apply_observation(Observation::Unreadable(key(9, 90)));
        assert_eq!(
            record(&inv, 9)?.state,
            ProcessObservationState::Unverifiable
        );

        // And it cannot downgrade a confirmed exit.
        inv.apply_observation(Observation::ExitConfirmed(key(8, 80)));
        assert_eq!(
            inv.apply_observation(Observation::Unreadable(key(8, 80))),
            ApplyOutcome::Stale
        );
        assert_eq!(
            record(&inv, 8)?.state,
            ProcessObservationState::ExitConfirmed
        );
        Ok(())
    }

    #[test]
    fn test_gap_forces_resync() -> TestResult {
        let mut inv = InventoryState::new(8);
        inv.reconcile_snapshot(&[key(1, 10)], 3);
        assert_eq!(inv.coverage(), Coverage::Reconciled { generation: 3 });
        inv.note_gap();
        assert_eq!(inv.coverage(), Coverage::ResyncRequired { generation: 3 });
        assert_eq!(
            inv.apply_observation(Observation::ExitConfirmed(key(1, 10))),
            ApplyOutcome::ResyncRequired
        );
        assert_eq!(record(&inv, 1)?.state, ProcessObservationState::Live);
        inv.reconcile_snapshot(&[key(1, 10)], 4);
        assert_eq!(inv.coverage(), Coverage::Reconciled { generation: 4 });
        assert_eq!(
            inv.apply_observation(Observation::ExitConfirmed(key(1, 10))),
            ApplyOutcome::Applied
        );
        Ok(())
    }

    #[test]
    fn test_capacity_degrades_coverage_visibly() -> TestResult {
        let mut inv = InventoryState::new(1);
        assert_eq!(
            inv.apply_observation(Observation::Created(key(1, 10))),
            ApplyOutcome::Applied
        );
        assert_eq!(
            inv.apply_observation(Observation::Created(key(2, 20))),
            ApplyOutcome::CapacityExceeded
        );
        assert_eq!(inv.coverage(), Coverage::Degraded { generation: 0 });
        assert_eq!(inv.len(), 1);
        assert!(inv.get(2).is_none());
        // Existing records are still updated.
        assert_eq!(
            inv.apply_observation(Observation::Exec(key(1, 10))),
            ApplyOutcome::Applied
        );
        Ok(())
    }

    #[test]
    fn test_exec_updates_in_place() -> TestResult {
        let mut inv = InventoryState::new(8);
        inv.apply_observation(Observation::Created(key(4, 40)));
        assert_eq!(
            inv.apply_observation(Observation::Exec(key(4, 40))),
            ApplyOutcome::Applied
        );
        assert_eq!(
            inv.apply_observation(Observation::Exec(key(4, 40))),
            ApplyOutcome::Applied
        );
        assert_eq!(inv.len(), 1);
        let rec = record(&inv, 4)?;
        assert_eq!(rec.exec_count, 2);
        assert_eq!(rec.key, key(4, 40));
        assert_eq!(rec.state, ProcessObservationState::Live);
        Ok(())
    }

    #[test]
    fn test_reconcile_marks_missing_unverifiable() -> TestResult {
        let mut inv = InventoryState::new(8);
        inv.apply_observation(Observation::Created(key(1, 10)));
        inv.apply_observation(Observation::Created(key(2, 20)));
        inv.apply_observation(Observation::Created(key(3, 30)));
        inv.apply_observation(Observation::ExitConfirmed(key(3, 30)));
        inv.reconcile_snapshot(&[key(1, 10), key(6, 60)], 9);
        assert_eq!(inv.coverage(), Coverage::Reconciled { generation: 9 });
        assert_eq!(record(&inv, 1)?.state, ProcessObservationState::Live);
        assert_eq!(
            record(&inv, 2)?.state,
            ProcessObservationState::Unverifiable
        );
        assert_eq!(
            record(&inv, 3)?.state,
            ProcessObservationState::ExitConfirmed
        );
        assert_eq!(record(&inv, 6)?.state, ProcessObservationState::Live);
        Ok(())
    }

    #[test]
    fn test_unknown_birth_is_read_only() -> TestResult {
        let mut inv = InventoryState::new(8);
        let unknown = ProcessKey {
            boot_id: 1,
            tgid: 11,
            birth_ticks: None,
        };
        assert_eq!(
            inv.apply_observation(Observation::Created(unknown)),
            ApplyOutcome::Applied
        );
        assert_eq!(
            inv.apply_observation(Observation::ExitConfirmed(unknown)),
            ApplyOutcome::Stale
        );
        assert_eq!(record(&inv, 11)?.state, ProcessObservationState::Live);
        Ok(())
    }
}
