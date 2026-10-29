//! `FixtureBpfLoader`: ein `BpfLoader`, der aus einer Liste vorbereiteter
//! Ereignisse liefert — kein Kernel, keine Berechtigung, keine
//! eBPF-Toolchain.
//!
//! # Verantwortungsbereich
//! Diese Datei ist Teil der **normalen** API dieser Crate, nicht hinter
//! `#[cfg(test)]`: `harw-dod-procmon` (AW7-01b) und `harw-dod-flow`
//! (AW7-01c) sind eigene Crates und brauchen [`FixtureBpfLoader`] als echte
//! Abhängigkeit für ihre eigenen Tests, nicht nur für die Tests dieser
//! Crate.
//!
//! # Zwei Betriebsmodi
//! - [`FixtureBpfLoader::new`]: [`crate::loader::BpfLoader::load`] gelingt
//!   immer und liefert die konfigurierten Ereignisse einmalig über
//!   [`crate::loader::BpfLoader::read_events`] — genau der Fall eines Hosts
//!   **mit** der nötigen Fähigkeit.
//! - [`FixtureBpfLoader::without_capability`] — [`crate::loader::BpfLoader::load`]
//!   liefert immer [`crate::error::BpfError::CapabilityUnavailable`] — genau
//!   der dokumentierte, lauffähige Fall eines Hosts **ohne** `CAP_BPF`
//!   (siehe `crate`-Moduldoku).
//!
//! # Grenzen dieser Fixture
//! Ein Griff trägt keine Beziehung zu einer bestimmten Teilmenge der
//! konfigurierten Ereignisse — alle über [`FixtureBpfLoader::new`] geladenen
//! Griffe teilen sich dieselbe Warteschlange. Das genügt dem in
//! `harw_dod_signals::sensor` festgehaltenen Grundsatz, dass eine
//! Sensor-Crate genau eine Quelle liest: wer mehrere Programme mit
//! unterschiedlichen erwarteten Ereignissen gegeneinander testen will,
//! braucht mehrere `FixtureBpfLoader`-Instanzen, keine gemeinsame.
//!
//! # Scope-Map-Simulation (PL-97 E3b)
//! [`FixtureBpfLoader::load_contract`] lädt einen
//! [`crate::BpfObjectContract`] wie `RealBpfLoader::load_contract` (gleiche
//! Validierung), legt für den Griff aber nur eine simulierte
//! `SCOPE_CGROUP_IDS`-Map mit dem Scope des Vertrags an. Über
//! [`crate::loader::BpfScopeWriter`] angewandte Änderungen landen in dieser
//! Map und zusätzlich in einem geordneten Protokoll
//! ([`FixtureBpfLoader::scope_map_ops`]), damit Tests jedes Einfügen und
//! Entfernen beobachten können. Die Map hat dieselbe feste Kapazität wie das
//! C-Objekt ([`crate::MAX_SCOPE_CGROUP_IDS`]); ein Einfügen darüber hinaus
//! scheitert wie im Kernel. Dieselbe Schreibregel wie beim realen Lader gilt:
//! ein Host-Scope und ein cgroup-Scope ohne `include_descendants` sind nach
//! dem Laden fest, und eine explizite Wurzel wird nie entfernt.
//!
//! # Exportierte Typen
//! [`FixtureBpfLoader`], [`ScopeMapOp`].
//!
//! # Nebenläufigkeit
//! `Send + Sync`: die Warteschlange steckt hinter einem `std::sync::Mutex`,
//! weil [`crate::loader::BpfLoader::read_events`] nur `&self` bekommt (siehe
//! Trait-Vertrag) und die Warteschlange beim Lesen verändert (Drain). Ein
//! vergifteter Mutex (nach einem Panic in einem anderen Thread, während der
//! die Sperre gehalten wurde) blockiert diese Fixture nicht: sie holt den
//! inneren Zustand über `PoisonError::into_inner` zurück, statt zu
//! `panic`en — ein Test-Doppelgänger, der selbst zur Fehlerquelle wird,
//! wäre schlechter als einer, der Vergiftung schlicht ignoriert.
//!
//! # Fehler
//! [`crate::error::BpfError::CapabilityUnavailable`] aus
//! [`FixtureBpfLoader::without_capability`]; [`FixtureBpfLoader::read_events`]
//! schlägt nie fehl.
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::{BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
//! use harw_dod_bpf::event::RawBpfEvent;
//! use harw_dod_bpf::fixture::FixtureBpfLoader;
//! use harw_types::SensorId;
//! use std::borrow::Cow;
//! use std::time::Duration;
//!
//! let event = RawBpfEvent {
//!     pid: 42,
//!     comm: "sshd".to_owned(),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     payload: vec![],
//! };
//! let loader = FixtureBpfLoader::new(vec![event.clone()]);
//!
//! let spec = BpfProgramSpec::new(
//!     SensorId::from_str("procmon-0"),
//!     BpfProgramKind::Tracepoint,
//!     "syscalls:sys_enter_execve",
//!     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
//! );
//! let handle = loader.load(&spec).expect("fixture loader with capability always succeeds");
//! let events = loader.read_events(&handle, Duration::from_millis(0)).unwrap();
//! assert_eq!(events, vec![event]);
//! ```

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::contract::BpfObjectContract;
use crate::error::BpfError;
use crate::event::RawBpfEvent;
use crate::handle::BpfHandle;
use crate::loader::{BpfLoader, BpfScopeWriter};
use crate::profile::{MAX_SCOPE_CGROUP_IDS, ScopeDelta, ScopeWritePolicy};
use crate::spec::BpfProgramSpec;

/// One change applied to a simulated scope map, in application order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeMapOp {
    /// `cgroup_id` was inserted into the map of the handle with id `handle`.
    Insert { handle: u64, cgroup_id: u64 },
    /// `cgroup_id` was removed from the map of the handle with id `handle`.
    Remove { handle: u64, cgroup_id: u64 },
}

/// Simulierte Scope-Map eines über `load_contract` geladenen Griffs.
#[derive(Debug)]
struct FixtureScopeMap {
    /// Beim Laden aus dem Vertrags-Scope festgelegt, wie beim realen Lader.
    policy: ScopeWritePolicy,
    ids: BTreeSet<u64>,
}

/// Der gesamte veränderliche Scope-Zustand dieser Fixture.
#[derive(Debug, Default)]
struct FixtureScopes {
    maps: HashMap<u64, FixtureScopeMap>,
    ops: Vec<ScopeMapOp>,
    failures_pending: usize,
}

/// Ein `BpfLoader`, der aus einer Liste vorbereiteter Ereignisse liefert.
///
/// # Description
/// Siehe Moduldoku für die beiden Betriebsmodi und ihre Grenzen.
#[derive(Debug)]
pub struct FixtureBpfLoader {
    capability_available: bool,
    events: Mutex<VecDeque<RawBpfEvent>>,
    scopes: Mutex<FixtureScopes>,
}

impl FixtureBpfLoader {
    /// Baut eine Fixture, die Programme erfolgreich lädt.
    ///
    /// # Arguments
    /// - `events` (`Vec<RawBpfEvent>`): die Ereignisse, die
    ///   [`crate::loader::BpfLoader::read_events`] beim ersten Aufruf
    ///   (drainend, siehe [`Self::read_events`]) zurückgibt.
    ///
    /// # Returns
    /// Eine `FixtureBpfLoader`, deren `load` immer gelingt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_bpf::fixture::FixtureBpfLoader;
    ///
    /// let _loader = FixtureBpfLoader::new(Vec::new());
    /// ```
    #[must_use]
    pub fn new(events: Vec<RawBpfEvent>) -> Self {
        Self {
            capability_available: true,
            events: Mutex::new(events.into()),
            scopes: Mutex::new(FixtureScopes::default()),
        }
    }

    /// Baut eine Fixture, die den Betriebsfall „keine Berechtigung" simuliert.
    ///
    /// # Description
    /// Jeder Aufruf von [`crate::loader::BpfLoader::load`] liefert
    /// [`BpfError::CapabilityUnavailable`] — genau das Verhalten, das ein
    /// Sentinel auf einem Host ohne `CAP_BPF` sehen muss, um den Sensor als
    /// degradiert zu melden statt zu scheitern.
    ///
    /// # Returns
    /// Eine `FixtureBpfLoader`, deren `load` immer
    /// [`BpfError::CapabilityUnavailable`] liefert.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_bpf::{BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
    /// use harw_dod_bpf::error::BpfError;
    /// use harw_dod_bpf::fixture::FixtureBpfLoader;
    /// use harw_types::SensorId;
    /// use std::borrow::Cow;
    ///
    /// let loader = FixtureBpfLoader::without_capability();
    /// let spec = BpfProgramSpec::new(
    ///     SensorId::from_str("flow-0"),
    ///     BpfProgramKind::SocketFilter,
    ///     "eth0",
    ///     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
    /// );
    /// assert!(matches!(loader.load(&spec), Err(BpfError::CapabilityUnavailable)));
    /// ```
    #[must_use]
    pub fn without_capability() -> Self {
        Self {
            capability_available: false,
            events: Mutex::new(VecDeque::new()),
            scopes: Mutex::new(FixtureScopes::default()),
        }
    }

    /// Loads a contract object the way `RealBpfLoader::load_contract` does,
    /// without a kernel: the contract is validated, then a simulated scope
    /// map is seeded from `contract.scope` (`{0}` for
    /// [`crate::BpfScope::Host`]; roots plus attach-time descendants for a
    /// cgroup scope) and the scope's write policy is recorded.
    ///
    /// # Errors
    /// [`BpfError::CapabilityUnavailable`] for
    /// [`Self::without_capability`]; [`BpfError::InvalidProgramContract`]
    /// when `contract` fails [`BpfObjectContract::validate`].
    pub fn load_contract(&self, contract: &BpfObjectContract) -> Result<BpfHandle, BpfError> {
        if !self.capability_available {
            return Err(BpfError::CapabilityUnavailable);
        }
        contract.validate()?;
        let map = FixtureScopeMap {
            policy: ScopeWritePolicy::for_scope(&contract.scope),
            ids: contract.scope.map_ids().into_iter().collect(),
        };
        let handle = BpfHandle::new(
            contract.sensor.clone(),
            contract.kind,
            contract.attach_point.clone(),
        );
        self.lock_scopes().maps.insert(handle.id(), map);
        Ok(handle)
    }

    /// The current content of the simulated scope map of `handle`, or
    /// `None` when `handle` was not loaded through [`Self::load_contract`].
    #[must_use]
    pub fn scope_ids(&self, handle: &BpfHandle) -> Option<BTreeSet<u64>> {
        self.lock_scopes()
            .maps
            .get(&handle.id())
            .map(|map| map.ids.clone())
    }

    /// Every insert/remove applied through [`BpfScopeWriter`] so far, in
    /// order (no-op removals of absent ids are not recorded).
    #[must_use]
    pub fn scope_map_ops(&self) -> Vec<ScopeMapOp> {
        self.lock_scopes().ops.clone()
    }

    /// Makes the next `count` calls of
    /// [`BpfScopeWriter::apply_scope_delta`] fail with
    /// [`BpfError::ScopeMapUpdateFailed`] before changing anything.
    pub fn fail_next_scope_updates(&self, count: usize) {
        self.lock_scopes().failures_pending = count;
    }

    /// Sperrt den Scope-Zustand; ein vergifteter `Mutex` liefert seinen
    /// inneren Wert (siehe Moduldoku, „Nebenläufigkeit“).
    fn lock_scopes(&self) -> MutexGuard<'_, FixtureScopes> {
        self.scopes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl BpfScopeWriter for FixtureBpfLoader {
    /// See [`BpfScopeWriter::apply_scope_delta`]. Mirrors the real loader:
    /// invalid deltas, objects with a fixed scope (host, or cgroups without
    /// `include_descendants`) and removals of an explicit root are refused
    /// before anything changes, removals run before insertions, and an
    /// insertion beyond
    /// [`MAX_SCOPE_CGROUP_IDS`] fails like a full kernel map (earlier
    /// entries of the same delta stay applied).
    fn apply_scope_delta(&self, handle: &BpfHandle, delta: &ScopeDelta) -> Result<(), BpfError> {
        delta.validate()?;
        let mut scopes = self.lock_scopes();
        let FixtureScopes {
            maps,
            ops,
            failures_pending,
        } = &mut *scopes;
        let map = maps.get_mut(&handle.id()).ok_or(BpfError::UnknownHandle)?;
        map.policy.admit(delta)?;
        if *failures_pending > 0 {
            *failures_pending -= 1;
            return Err(BpfError::ScopeMapUpdateFailed);
        }
        for id in &delta.remove {
            if map.ids.remove(id) {
                ops.push(ScopeMapOp::Remove {
                    handle: handle.id(),
                    cgroup_id: *id,
                });
            }
        }
        for id in &delta.insert {
            if map.ids.contains(id) {
                continue;
            }
            if map.ids.len() >= MAX_SCOPE_CGROUP_IDS {
                return Err(BpfError::ScopeMapUpdateFailed);
            }
            map.ids.insert(*id);
            ops.push(ScopeMapOp::Insert {
                handle: handle.id(),
                cgroup_id: *id,
            });
        }
        Ok(())
    }
}

impl BpfLoader for FixtureBpfLoader {
    /// Siehe [`crate::loader::BpfLoader::load`].
    ///
    /// # Errors
    /// [`BpfError::CapabilityUnavailable`], wenn diese Fixture über
    /// [`Self::without_capability`] gebaut wurde.
    fn load(&self, program: &BpfProgramSpec) -> Result<BpfHandle, BpfError> {
        if !self.capability_available {
            return Err(BpfError::CapabilityUnavailable);
        }
        Ok(BpfHandle::new(
            program.sensor.clone(),
            program.kind,
            program.attach_point.clone(),
        ))
    }

    /// Siehe [`crate::loader::BpfLoader::read_events`].
    ///
    /// # Description
    /// Entleert die konfigurierte Warteschlange vollständig und gibt genau
    /// die entnommenen Ereignisse zurück — ein zweiter Aufruf ohne
    /// zwischenzeitliches Nachfüllen liefert eine leere Liste, wie ein
    /// echter Ringpuffer, der bereits leergelesen wurde. `handle` und
    /// `timeout` werden ignoriert: diese Fixture wartet nie und
    /// unterscheidet nicht zwischen Griffen (siehe Moduldoku, „Grenzen
    /// dieser Fixture").
    ///
    /// # Errors
    /// Keine — schlägt nie fehl.
    fn read_events(
        &self,
        _handle: &BpfHandle,
        _timeout: Duration,
    ) -> Result<Vec<RawBpfEvent>, BpfError> {
        let mut queue = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(queue.drain(..).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{FixtureBpfLoader, ScopeMapOp};
    use crate::contract::{BpfObjectContract, EXEC_ATTACH_POINT, EXEC_PROGRAM_NAME};
    use crate::error::BpfError;
    use crate::event::RawBpfEvent;
    use crate::loader::{BpfLoader, BpfScopeWriter};
    use crate::profile::{BpfScope, ResolvedCgroup, ScopeDelta};
    use crate::spec::{BpfProgramKind, BpfProgramSource, BpfProgramSpec};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::SensorId;
    use std::borrow::Cow;
    use std::collections::BTreeSet;
    use std::time::Duration;

    fn exec_contract(scope: BpfScope) -> BpfObjectContract {
        BpfObjectContract::new(
            SensorId::from_str("procmon-0"),
            EXEC_PROGRAM_NAME,
            BpfProgramKind::Tracepoint,
            EXEC_ATTACH_POINT,
            BpfProgramSource::Embedded(Cow::Borrowed(b"ELF")),
            scope,
        )
    }

    fn cgroups(paths_and_ids: &[(&str, u64)]) -> Vec<ResolvedCgroup> {
        paths_and_ids
            .iter()
            .map(|(path, id)| ResolvedCgroup {
                path: (*path).to_owned(),
                id: *id,
            })
            .collect()
    }

    /// Root `/r` (id 10) with `include_descendants`, plus the attach-time
    /// descendants `/r/<id>` for every id in `descendants`.
    fn cgroup_scope(descendants: &[u64]) -> BpfScope {
        BpfScope::Cgroups {
            groups: cgroups(&[("/r", 10)]),
            include_descendants: true,
            descendants: descendants
                .iter()
                .map(|id| ResolvedCgroup {
                    path: format!("/r/{id}"),
                    id: *id,
                })
                .collect(),
        }
    }

    #[test]
    fn test_load_contract_seeds_the_simulated_scope_map() -> TestResult {
        let loader = FixtureBpfLoader::new(Vec::new());
        let cgroups = loader
            .load_contract(&exec_contract(cgroup_scope(&[11])))
            .map_err(ctx("cgroup contract loads"))?;
        let host = loader
            .load_contract(&exec_contract(BpfScope::Host))
            .map_err(ctx("host contract loads"))?;
        assert_eq!(loader.scope_ids(&cgroups), Some(BTreeSet::from([10, 11])));
        assert_eq!(loader.scope_ids(&host), Some(BTreeSet::from([0])));
        assert!(
            FixtureBpfLoader::without_capability()
                .load_contract(&exec_contract(BpfScope::Host))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn test_scope_delta_is_applied_removals_first_and_recorded() -> TestResult {
        let loader = FixtureBpfLoader::new(Vec::new());
        let handle = loader
            .load_contract(&exec_contract(cgroup_scope(&[11])))
            .map_err(ctx("contract loads"))?;
        let delta = ScopeDelta {
            insert: vec![12],
            remove: vec![11, 99],
        };
        loader
            .apply_scope_delta(&handle, &delta)
            .map_err(ctx("delta applies"))?;
        // Wiederholung ist idempotent und erzeugt keine weiteren Einträge.
        loader
            .apply_scope_delta(&handle, &delta)
            .map_err(ctx("delta re-applies"))?;
        assert_eq!(loader.scope_ids(&handle), Some(BTreeSet::from([10, 12])));
        assert_eq!(
            loader.scope_map_ops(),
            vec![
                ScopeMapOp::Remove {
                    handle: handle.id(),
                    cgroup_id: 11
                },
                ScopeMapOp::Insert {
                    handle: handle.id(),
                    cgroup_id: 12
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn test_scope_writer_refuses_host_scope_unknown_handles_and_injected_failures() -> TestResult {
        let loader = FixtureBpfLoader::new(Vec::new());
        let delta = ScopeDelta {
            insert: vec![5],
            remove: Vec::new(),
        };
        let host = loader
            .load_contract(&exec_contract(BpfScope::Host))
            .map_err(ctx("host contract loads"))?;
        assert!(matches!(
            loader.apply_scope_delta(&host, &delta),
            Err(BpfError::InvalidProgramContract)
        ));
        let legacy = loader.load(&sample_spec()).map_err(ctx("legacy load"))?;
        assert!(matches!(
            loader.apply_scope_delta(&legacy, &delta),
            Err(BpfError::UnknownHandle)
        ));

        let handle = loader
            .load_contract(&exec_contract(cgroup_scope(&[])))
            .map_err(ctx("contract loads"))?;
        loader.fail_next_scope_updates(1);
        assert!(matches!(
            loader.apply_scope_delta(&handle, &delta),
            Err(BpfError::ScopeMapUpdateFailed)
        ));
        assert_eq!(loader.scope_ids(&handle), Some(BTreeSet::from([10])));
        loader
            .apply_scope_delta(&handle, &delta)
            .map_err(ctx("next update succeeds"))?;
        assert_eq!(loader.scope_ids(&handle), Some(BTreeSet::from([5, 10])));
        Ok(())
    }

    #[test]
    fn test_scope_writer_refuses_a_fixed_cgroup_scope_and_pinned_roots() -> TestResult {
        let loader = FixtureBpfLoader::new(Vec::new());
        let fixed = loader
            .load_contract(&exec_contract(BpfScope::Cgroups {
                groups: cgroups(&[("/a", 20), ("/b", 21)]),
                include_descendants: false,
                descendants: Vec::new(),
            }))
            .map_err(ctx("fixed cgroup contract loads"))?;
        // Weder Erweitern noch Verengen eines festen Scopes, auch kein leerer
        // Delta-Aufruf.
        for delta in [
            ScopeDelta {
                insert: vec![22],
                remove: Vec::new(),
            },
            ScopeDelta {
                insert: Vec::new(),
                remove: vec![21],
            },
            ScopeDelta::default(),
        ] {
            assert!(matches!(
                loader.apply_scope_delta(&fixed, &delta),
                Err(BpfError::InvalidProgramContract)
            ));
        }
        assert_eq!(loader.scope_ids(&fixed), Some(BTreeSet::from([20, 21])));

        let dynamic = loader
            .load_contract(&exec_contract(cgroup_scope(&[11])))
            .map_err(ctx("dynamic cgroup contract loads"))?;
        // Eine Wurzel zu entfernen scheitert, bevor irgendetwas geändert wird
        // — auch der mitgeschickte, für sich zulässige Rückzug von 11.
        let removes_root = ScopeDelta {
            insert: vec![12],
            remove: vec![11, 10],
        };
        assert!(matches!(
            loader.apply_scope_delta(&dynamic, &removes_root),
            Err(BpfError::InvalidProgramContract)
        ));
        assert_eq!(loader.scope_ids(&dynamic), Some(BTreeSet::from([10, 11])));
        assert!(loader.scope_map_ops().is_empty());
        Ok(())
    }

    fn sample_spec() -> BpfProgramSpec {
        BpfProgramSpec::new(
            SensorId::from_str("procmon-0"),
            BpfProgramKind::Tracepoint,
            "syscalls:sys_enter_execve",
            BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
        )
    }

    fn sample_event(pid: u32) -> RawBpfEvent {
        RawBpfEvent {
            pid,
            comm: "sshd".to_owned(),
            observed_at: jiff::Timestamp::UNIX_EPOCH,
            payload: vec![],
        }
    }

    #[test]
    fn test_load_with_capability_succeeds_and_carries_spec_fields() -> TestResult {
        let loader = FixtureBpfLoader::new(Vec::new());
        let spec = sample_spec();
        let handle = loader
            .load(&spec)
            .map_err(ctx("fixture loader with capability succeeds"))?;
        assert_eq!(handle.sensor(), &spec.sensor);
        assert_eq!(handle.kind(), spec.kind);
        assert_eq!(handle.attach_point(), spec.attach_point);
        Ok(())
    }

    #[test]
    fn test_load_without_capability_returns_capability_unavailable() -> TestResult {
        let loader = FixtureBpfLoader::without_capability();
        let result = loader.load(&sample_spec());
        let Err(err) = result else {
            return Err(TestError::Unexpected("must fail without capability".into()));
        };
        assert!(matches!(err, BpfError::CapabilityUnavailable));
        Ok(())
    }

    #[test]
    fn test_read_events_returns_exactly_the_configured_events() -> TestResult {
        let events = vec![sample_event(1), sample_event(2)];
        let loader = FixtureBpfLoader::new(events.clone());
        let handle = loader
            .load(&sample_spec())
            .map_err(ctx("fixture loader with capability succeeds"))?;

        let read_back = loader
            .read_events(&handle, Duration::from_millis(0))
            .map_err(ctx("fixture read_events never fails"))?;
        assert_eq!(read_back, events);
        Ok(())
    }

    #[test]
    fn test_read_events_drains_the_queue_so_a_second_call_is_empty() -> TestResult {
        let loader = FixtureBpfLoader::new(vec![sample_event(1)]);
        let handle = loader
            .load(&sample_spec())
            .map_err(ctx("fixture loader with capability succeeds"))?;

        let first = loader
            .read_events(&handle, Duration::from_millis(0))
            .map_err(ctx("read_events"))?;
        assert_eq!(first.len(), 1);

        let second = loader
            .read_events(&handle, Duration::from_millis(0))
            .map_err(ctx("read_events"))?;
        assert!(second.is_empty());
        Ok(())
    }

    #[test]
    fn test_without_capability_read_events_is_always_empty() -> TestResult {
        let loader = FixtureBpfLoader::without_capability();
        // `read_events` selbst kennt keine Berechtigungsprüfung — nur `load`
        // meldet `CapabilityUnavailable`. Ein Aufrufer, der `read_events`
        // ohne vorheriges erfolgreiches `load` aufruft, bekommt schlicht die
        // (hier leere) konfigurierte Warteschlange.
        let handle = crate::handle::BpfHandle::new(
            SensorId::from_str("procmon-0"),
            BpfProgramKind::Tracepoint,
            "syscalls:sys_enter_execve",
        );
        let events = loader
            .read_events(&handle, Duration::from_millis(0))
            .map_err(ctx("read_events"))?;
        assert!(events.is_empty());
        Ok(())
    }
}
