//! Versioned, byte-addressed ring-buffer ABI shared by the BPF objects and
//! the privileged loader.  This is deliberately *not* a `repr(C)` Rust
//! struct: padding and target ABI are not part of the wire contract.

use crate::error::BpfError;

/// Magic at the start of every DoD BPF v1 record (`HDOD`).
pub const WIRE_MAGIC: [u8; 4] = *b"HDOD";
/// The only wire ABI accepted by this build of the loader.
pub const WIRE_VERSION_V1: u16 = 1;
/// Exact v1 header size.  The payload starts at this byte, independent of
/// Rust/C layout or target pointer width.  Bytes 54..56 are mandatory zero
/// reserved bytes; they prevent a later ABI from being accepted as v1 merely
/// because its prefix happens to match.
pub const WIRE_HEADER_LEN_V1: usize = 56;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 4;
const TYPE_OFFSET: usize = 6;
const FLAGS_OFFSET: usize = 7;
const LENGTH_OFFSET: usize = 8;
const KTIME_OFFSET: usize = 12;
const SEQUENCE_OFFSET: usize = 20;
const TGID_OFFSET: usize = 28;
const PID_OFFSET: usize = 32;
const PPID_OFFSET: usize = 36;
const UID_OFFSET: usize = 40;
const CGROUP_ID_OFFSET: usize = 44;
const PAYLOAD_LEN_OFFSET: usize = 52;
const RESERVED_OFFSET: usize = 54;

/// A record kind emitted by a DoD BPF object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WireEventType {
    /// `sched_process_exec`, after a successful exec.
    Exec = 1,
    /// A task is leaving through `sched_process_exit`.
    ProcessExit = 2,
    /// An outbound TCP connect observed in the initiating task context.
    TcpConnect = 3,
}

impl TryFrom<u8> for WireEventType {
    type Error = BpfError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Exec),
            2 => Ok(Self::ProcessExit),
            3 => Ok(Self::TcpConnect),
            _ => Err(BpfError::UnsupportedWireEvent),
        }
    }
}

/// Identity captured in the hook's task context.  It must not be filled from
/// a later socket-state callback, which may run in softirq/kernel-worker
/// context and therefore describe the wrong actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskIdentity {
    pub tgid: u32,
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub cgroup_id: u64,
}

/// Lossless representation of one accepted ring-buffer record. `ktime_ns`
/// is monotonic kernel time, not Unix time; callers map it with
/// [`crate::time::KernelTimeMapper`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireEvent {
    pub event_type: WireEventType,
    pub flags: u8,
    pub ktime_ns: u64,
    /// A kernel-global issuance sequence.  It is intentionally kept beside
    /// the monotonic time because events from different CPUs can share a
    /// timestamp.  Consumers use it together with loss counters to make
    /// discontinuities visible, never as a persisted compatibility key.
    pub sequence: u64,
    pub task: TaskIdentity,
    pub payload: Vec<u8>,
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, BpfError> {
    let end = offset.checked_add(2).ok_or(BpfError::MalformedEvent)?;
    let value: [u8; 2] = bytes
        .get(offset..end)
        .ok_or(BpfError::MalformedEvent)?
        .try_into()
        .map_err(|_| BpfError::MalformedEvent)?;
    Ok(u16::from_le_bytes(value))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, BpfError> {
    let end = offset.checked_add(4).ok_or(BpfError::MalformedEvent)?;
    let value: [u8; 4] = bytes
        .get(offset..end)
        .ok_or(BpfError::MalformedEvent)?
        .try_into()
        .map_err(|_| BpfError::MalformedEvent)?;
    Ok(u32::from_le_bytes(value))
}

fn read_u64_le(bytes: &[u8], offset: usize) -> Result<u64, BpfError> {
    let end = offset.checked_add(8).ok_or(BpfError::MalformedEvent)?;
    let value: [u8; 8] = bytes
        .get(offset..end)
        .ok_or(BpfError::MalformedEvent)?
        .try_into()
        .map_err(|_| BpfError::MalformedEvent)?;
    Ok(u64::from_le_bytes(value))
}

/// Parse exactly one v1 record. Unknown ABI versions, unknown kinds, a
/// record length mismatch, and contradictory payload lengths are rejected;
/// accepting a prefix would make a future ABI silently look valid.
pub fn parse_wire_event(bytes: &[u8]) -> Result<WireEvent, BpfError> {
    if bytes.len() < WIRE_HEADER_LEN_V1 || bytes.get(MAGIC_OFFSET..4) != Some(WIRE_MAGIC.as_slice())
    {
        return Err(BpfError::MalformedEvent);
    }
    if read_u16_le(bytes, VERSION_OFFSET)? != WIRE_VERSION_V1 {
        return Err(BpfError::UnsupportedWireVersion);
    }
    let record_len = usize::try_from(read_u32_le(bytes, LENGTH_OFFSET)?)
        .map_err(|_| BpfError::MalformedEvent)?;
    if record_len != bytes.len() || record_len < WIRE_HEADER_LEN_V1 {
        return Err(BpfError::MalformedEvent);
    }
    let payload_len = usize::from(read_u16_le(bytes, PAYLOAD_LEN_OFFSET)?);
    if WIRE_HEADER_LEN_V1.checked_add(payload_len) != Some(record_len) {
        return Err(BpfError::MalformedEvent);
    }
    if bytes.get(RESERVED_OFFSET..WIRE_HEADER_LEN_V1) != Some(&[0, 0]) {
        return Err(BpfError::MalformedEvent);
    }

    Ok(WireEvent {
        event_type: WireEventType::try_from(
            *bytes.get(TYPE_OFFSET).ok_or(BpfError::MalformedEvent)?,
        )?,
        flags: *bytes.get(FLAGS_OFFSET).ok_or(BpfError::MalformedEvent)?,
        ktime_ns: read_u64_le(bytes, KTIME_OFFSET)?,
        sequence: read_u64_le(bytes, SEQUENCE_OFFSET)?,
        task: TaskIdentity {
            tgid: read_u32_le(bytes, TGID_OFFSET)?,
            pid: read_u32_le(bytes, PID_OFFSET)?,
            ppid: read_u32_le(bytes, PPID_OFFSET)?,
            uid: read_u32_le(bytes, UID_OFFSET)?,
            cgroup_id: read_u64_le(bytes, CGROUP_ID_OFFSET)?,
        },
        payload: bytes[WIRE_HEADER_LEN_V1..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        TaskIdentity, WIRE_HEADER_LEN_V1, WIRE_MAGIC, WIRE_VERSION_V1, WireEventType,
        parse_wire_event,
    };
    use crate::BpfError;
    use crate::test_support::{TestResult, ctx};

    fn record(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; WIRE_HEADER_LEN_V1];
        bytes[0..4].copy_from_slice(&WIRE_MAGIC);
        bytes[4..6].copy_from_slice(&WIRE_VERSION_V1.to_le_bytes());
        bytes[6] = kind;
        bytes[8..12]
            .copy_from_slice(&(WIRE_HEADER_LEN_V1 as u32 + payload.len() as u32).to_le_bytes());
        bytes[12..20].copy_from_slice(&42u64.to_le_bytes());
        bytes[20..28].copy_from_slice(&7u64.to_le_bytes());
        bytes[28..32].copy_from_slice(&10u32.to_le_bytes());
        bytes[32..36].copy_from_slice(&11u32.to_le_bytes());
        bytes[36..40].copy_from_slice(&9u32.to_le_bytes());
        bytes[40..44].copy_from_slice(&1000u32.to_le_bytes());
        bytes[44..52].copy_from_slice(&77u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn parses_explicit_v1_tcp_connect_layout() -> TestResult {
        let event = parse_wire_event(&record(3, b"tcp")).map_err(ctx("parse_wire_event"))?;
        assert_eq!(event.event_type, WireEventType::TcpConnect);
        assert_eq!(event.ktime_ns, 42);
        assert_eq!(
            event.task,
            TaskIdentity {
                tgid: 10,
                pid: 11,
                ppid: 9,
                uid: 1000,
                cgroup_id: 77
            }
        );
        assert_eq!(event.payload, b"tcp");
        Ok(())
    }

    #[test]
    fn v1_carries_a_global_sequence_and_rejects_nonzero_reserved_bytes() -> TestResult {
        let mut bytes = record(2, b"");
        // The production ABI reserves bytes 54..56 so that an incompatible
        // producer cannot silently look like v1.  Sequence 99 is deliberately
        // distinct from the monotonic timestamp above.
        bytes[20..28].copy_from_slice(&99u64.to_le_bytes());
        let event = parse_wire_event(&bytes).map_err(ctx("parse_wire_event"))?;
        assert_eq!(event.sequence, 99);

        bytes[54] = 1;
        assert!(matches!(
            parse_wire_event(&bytes),
            Err(BpfError::MalformedEvent)
        ));
        Ok(())
    }

    #[test]
    fn rejects_unknown_version_and_length_mismatch() {
        let mut version = record(1, &[]);
        version[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(
            parse_wire_event(&version),
            Err(BpfError::UnsupportedWireVersion)
        ));
        let mut length = record(1, &[]);
        length[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert!(matches!(
            parse_wire_event(&length),
            Err(BpfError::MalformedEvent)
        ));
    }
}
