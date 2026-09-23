//! Versionierte, push-only Nachrichten zwischen privilegierter Probe und
//! Sentinel.  Der Transport traegt den aufgeloesten Profilstempel neben dem
//! Ereignis, damit der Sentinel keine Daten einer anderen Konfiguration als
//! vollstaendige Beobachtungskette akzeptiert.

use crate::SecurityEvent;
use harw_types::SensorId;

/// Profil- und Konfigurationsbindung jeder Probe-Nachricht.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeStamp {
    /// Der explizit gewaehlte Profilname.
    pub profile_id: String,
    /// Hexkodierter Digest der exakt vertrauenswuerdig gelesenen TOML-Bytes.
    pub config_digest: String,
}

/// Vertrauensgrad der Umrechnung von `bpf_ktime_get_ns()` nach Echtzeit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TimeMappingConfidence {
    /// Die monotone/realtime-Beziehung wurde ohne bekannte Unterbrechung gemessen.
    Measured,
    /// Suspend oder ein Zeitsprung macht die angezeigte Echtzeit unsicher.
    Uncertain,
}

/// Verlustzaehler eines BPF-Objekts zum Zeitpunkt einer Probe-Nachricht.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BpfLossSnapshot {
    pub exec: u64,
    pub process_exit: u64,
    pub tcp_connect: u64,
    pub invalid_wire_events: u64,
}

/// Zusatzdaten des v1-BPF-Wirevertrags, die ein vorhandenes
/// `SecurityEvent` noch nicht ausdruecken kann.  Sie bleiben am IPC-Ereignis
/// und werden vom Sentinel mit geloggt; sie duerfen nicht erfunden werden.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BpfEventMetadata {
    pub wire_version: u16,
    pub sequence: u64,
    pub tgid: u32,
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub cgroup_id: u64,
    pub time_confidence: TimeMappingConfidence,
    pub loss: BpfLossSnapshot,
    /// `true`, wenn der begrenzte Exec-Pfad abgeschnitten wurde.
    pub exec_path_truncated: bool,
    /// In Wire-v1 werden Argumente nie erhoben; ein leerer Digest darf dies
    /// nicht vortaeuschen.
    pub argv_not_collected: bool,
}

/// Sichtbares Lebenszeichen einer aktiv angehaengten Sonde.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeHeartbeat {
    pub sensor: SensorId,
    pub sequence: u64,
    pub loss: BpfLossSnapshot,
}

/// Geschlossener IPC-Wirevertrag.  Der Sentinel akzeptiert nur Nachrichten,
/// deren [`ProbeStamp`] zu seinem eigenen aufgeloesten Profil passt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "type")]
pub enum ProbeMessage {
    Event {
        stamp: ProbeStamp,
        event: SecurityEvent,
        #[serde(default)]
        metadata: Option<BpfEventMetadata>,
    },
    Heartbeat {
        stamp: ProbeStamp,
        heartbeat: ProbeHeartbeat,
    },
}

impl ProbeMessage {
    #[must_use]
    pub fn stamp(&self) -> &ProbeStamp {
        match self {
            Self::Event { stamp, .. } | Self::Heartbeat { stamp, .. } => stamp,
        }
    }

    #[must_use]
    pub fn sensor(&self) -> &SensorId {
        match self {
            Self::Event { event, .. } => &event.sensor,
            Self::Heartbeat { heartbeat, .. } => &heartbeat.sensor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BpfEventMetadata, BpfLossSnapshot, ProbeMessage, ProbeStamp, TimeMappingConfidence,
    };
    use crate::test_support::{TestResult, ctx};
    use crate::{EventKind, SecurityEvent};
    use harw_types::SensorId;
    use jiff::Timestamp;

    #[test]
    fn event_message_keeps_profile_digest_and_v1_identity_metadata() -> TestResult {
        let message = ProbeMessage::Event {
            stamp: ProbeStamp {
                profile_id: "selected-services".into(),
                config_digest: "ab".repeat(32),
            },
            event: SecurityEvent {
                sensor: SensorId::from_str("probe-bpf-procmon-0"),
                observed_at: Timestamp::UNIX_EPOCH,
                actor: None,
                kind: EventKind::SensorDegraded {
                    sensor: SensorId::from_str("probe-bpf-procmon-0"),
                },
            },
            metadata: Some(BpfEventMetadata {
                wire_version: 1,
                sequence: 99,
                tgid: 10,
                pid: 11,
                ppid: 9,
                uid: 1000,
                cgroup_id: 77,
                time_confidence: TimeMappingConfidence::Measured,
                loss: BpfLossSnapshot::default(),
                exec_path_truncated: true,
                argv_not_collected: true,
            }),
        };

        let encoded = serde_json::to_vec(&message).map_err(ctx("ProbeMessage serializes"))?;
        let decoded: ProbeMessage =
            serde_json::from_slice(&encoded).map_err(ctx("ProbeMessage round trips"))?;
        assert_eq!(decoded, message);
        assert_eq!(message.stamp().profile_id, "selected-services");
        Ok(())
    }
}
