//! Explicit object contract.  An ELF is accepted only if its selected program,
//! named maps, and ABI agree with the profile being attached.

use crate::{BpfError, BpfProgramKind, BpfProgramSource, BpfScope, WIRE_VERSION_V1};
use harw_types::SensorId;

/// Exact map names emitted by every v1 C object.  The loader rejects an
/// object that has an additional or substituted map so an arbitrary ELF
/// cannot smuggle a broader collection channel into the probe.
pub const EVENTS_MAP_NAME: &str = "EVENTS";
pub const SCOPE_MAP_NAME: &str = "SCOPE_CGROUP_IDS";
pub const LOSS_COUNTS_MAP_NAME: &str = "LOSS_COUNTS";
pub const SEQUENCE_MAP_NAME: &str = "SEQUENCE";
pub const REQUIRED_MAP_NAMES: [&str; 4] = [
    EVENTS_MAP_NAME,
    SCOPE_MAP_NAME,
    LOSS_COUNTS_MAP_NAME,
    SEQUENCE_MAP_NAME,
];

pub const EXEC_PROGRAM_NAME: &str = "dod_sched_process_exec";
pub const EXIT_PROGRAM_NAME: &str = "dod_sched_process_exit";
pub const TCP_V4_CONNECT_PROGRAM_NAME: &str = "dod_tcp_v4_connect";
pub const TCP_V6_CONNECT_PROGRAM_NAME: &str = "dod_tcp_v6_connect";
pub const EXEC_ATTACH_POINT: &str = "sched:sched_process_exec";
pub const EXIT_ATTACH_POINT: &str = "sched:sched_process_exit";
pub const TCP_V4_CONNECT_ATTACH_POINT: &str = "tcp_v4_connect";
pub const TCP_V6_CONNECT_ATTACH_POINT: &str = "tcp_v6_connect";

#[derive(Debug, Clone, PartialEq)]
pub struct BpfObjectContract {
    pub sensor: SensorId,
    pub program_name: String,
    pub kind: BpfProgramKind,
    pub attach_point: String,
    pub source: BpfProgramSource,
    pub wire_version: u16,
    pub scope: BpfScope,
}

impl BpfObjectContract {
    #[must_use]
    pub fn new(
        sensor: SensorId,
        program_name: impl Into<String>,
        kind: BpfProgramKind,
        attach_point: impl Into<String>,
        source: BpfProgramSource,
        scope: BpfScope,
    ) -> Self {
        Self { sensor, program_name: program_name.into(), kind, attach_point: attach_point.into(), source, wire_version: WIRE_VERSION_V1, scope }
    }

    pub fn validate(&self) -> Result<(), BpfError> {
        if self.program_name.is_empty()
            || self.attach_point.is_empty()
            || self.wire_version != WIRE_VERSION_V1
            || !is_exact_program_contract(&self.program_name, self.kind, &self.attach_point)
        {
            return Err(BpfError::InvalidProgramContract);
        }
        self.scope.validate()
    }

    /// The exact, ordered map set expected from the standalone C build.
    /// Ordering makes this API useful for deterministic tests; the loader
    /// itself compares as a set because ELF map iteration is not ordered.
    #[must_use]
    pub const fn required_map_names(&self) -> [&'static str; 4] {
        REQUIRED_MAP_NAMES
    }
}

fn is_exact_program_contract(name: &str, kind: BpfProgramKind, attach_point: &str) -> bool {
    matches!(
        (name, kind, attach_point),
        (EXEC_PROGRAM_NAME, BpfProgramKind::Tracepoint, EXEC_ATTACH_POINT)
            | (EXIT_PROGRAM_NAME, BpfProgramKind::Tracepoint, EXIT_ATTACH_POINT)
            | (TCP_V4_CONNECT_PROGRAM_NAME, BpfProgramKind::FEntry, TCP_V4_CONNECT_ATTACH_POINT)
            | (TCP_V6_CONNECT_PROGRAM_NAME, BpfProgramKind::FEntry, TCP_V6_CONNECT_ATTACH_POINT)
    )
}

#[cfg(test)]
mod tests {
    use super::{BpfObjectContract, EVENTS_MAP_NAME, LOSS_COUNTS_MAP_NAME, SEQUENCE_MAP_NAME, SCOPE_MAP_NAME};
    use crate::{BpfProgramKind, BpfProgramSource, BpfScope};
    use harw_types::SensorId;
    use std::borrow::Cow;

    #[test]
    fn contract_accepts_only_the_named_v1_maps_and_a_selected_program() {
        let contract = BpfObjectContract::new(
            SensorId::from_str("procmon-0"),
            "dod_sched_process_exec",
            BpfProgramKind::Tracepoint,
            "sched:sched_process_exec",
            BpfProgramSource::Embedded(Cow::Borrowed(b"ELF")),
            BpfScope::Host,
        );

        assert_eq!(contract.required_map_names(), [EVENTS_MAP_NAME, SCOPE_MAP_NAME, LOSS_COUNTS_MAP_NAME, SEQUENCE_MAP_NAME]);
        assert!(contract.validate().is_ok());
    }

    #[test]
    fn contract_knows_fentry_as_the_connect_hook_kind() {
        let contract = BpfObjectContract::new(
            SensorId::from_str("flow-0"),
            "dod_tcp_v4_connect",
            BpfProgramKind::FEntry,
            "tcp_v4_connect",
            BpfProgramSource::Embedded(Cow::Borrowed(b"ELF")),
            BpfScope::Host,
        );
        assert!(contract.validate().is_ok());
    }

    #[test]
    fn c_wire_header_and_rust_contract_share_v1_names_and_size() {
        let header = include_str!("../../../bpf/include/harw_dod_wire_v1.h");
        let sources = concat!(
            include_str!("../../../bpf/src/exec.bpf.c"),
            include_str!("../../../bpf/src/exit.bpf.c"),
            include_str!("../../../bpf/src/tcp_v4_connect.bpf.c"),
            include_str!("../../../bpf/src/tcp_v6_connect.bpf.c"),
        );
        assert!(header.contains("#define HWD_WIRE_HEADER_LEN_V1 56"));
        assert!(header.contains("#define HWD_MAX_SCOPE_CGROUP_IDS 16384"));
        for name in ["EVENTS", "SCOPE_CGROUP_IDS", "LOSS_COUNTS", "SEQUENCE"] {
            assert!(header.contains(name));
        }
        for name in ["dod_sched_process_exec", "dod_sched_process_exit", "dod_tcp_v4_connect", "dod_tcp_v6_connect"] {
            assert!(sources.contains(name));
        }
    }
}
