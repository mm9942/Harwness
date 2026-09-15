//! Sicherheitsereignisse: wer etwas ausgelöst hat und welcher Art es ist (Contract-Master §G).
//!
//! # Verantwortungsbereich
//! Trägt [`Actor`], [`AuthOutcome`], [`EventKind`] und [`SecurityEvent`] —
//! den zweiten, seltenen und wichtigen Beobachtungsstrom neben
//! [`crate::sample::HostSample`] (siehe dortige Moduldoku für die Begründung
//! der Trennung).
//!
//! # Nebenläufigkeit
//! Reine Datentypen: `Send + Sync` automatisch, kein internes Locking.
//!
//! # Fehler
//! Keine eigenen — dieses Modul definiert nur Daten und liest keine Quelle.
//! `#[serde(deny_unknown_fields)]` auf [`SecurityEvent`] und [`EventKind`]
//! lässt `serde_json` beim Deserialisieren mit einem Serde-Fehler scheitern,
//! wenn ein unbekanntes Feld auftaucht.
//!
//! # Wire-Stabilität von `EventKind`
//! `EventKind` landet in Dateien (Belege, Fixtures) und in einem
//! Wire-Protokoll (Sentinel → Regelwerk). Die kebab-case-Form mit
//! `tag = "kind"` ist deshalb Teil der öffentlichen Fläche dieser Crate,
//! nicht nur ein Implementierungsdetail von `serde` — jede Variante hat einen
//! Golden-Test auf die exakte JSON-Zeichenkette (siehe Tests unten), nicht
//! nur einen Roundtrip.
//!
//! # Examples
//! ```rust
//! use harw_dod_signals::{Actor, AuthOutcome, EventKind, SecurityEvent};
//! use harw_types::SensorId;
//!
//! let event = SecurityEvent {
//!     sensor: SensorId::from_str("authlog-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: Some(Actor { uid: 1000, auid: Some(1000), cgroup: None }),
//!     kind: EventKind::AuthEvent { outcome: AuthOutcome::Failure },
//! };
//! assert!(matches!(event.kind, EventKind::AuthEvent { .. }));
//! ```

use harw_types::{CgroupId, ContentDigest, SensorId};
use jiff::Timestamp;

/// Wer etwas ausgelöst hat.
///
/// # Description
/// Trägt sowohl die aktuelle Ausführungs-UID als auch — wo verfügbar — die
/// Anmelde-UID. Beide können auseinanderfallen, und genau das ist der Punkt.
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::Actor;
///
/// let root_via_sudo = Actor { uid: 0, auid: Some(1000), cgroup: None };
/// assert_ne!(root_via_sudo.uid, root_via_sudo.auid.unwrap());
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    /// Die tatsächliche Ausführungs-UID zum Zeitpunkt des Ereignisses. Ein
    /// `sudo`-Aufruf ändert diesen Wert auf die Ziel-UID (typischerweise 0).
    pub uid: u32,
    /// Die Anmelde-UID (`auid`, vom Kernel-Audit-Subsystem vergeben): der
    /// Wert, den ein `sudo` **nicht** verändert. Ein Prozess, der über `sudo`
    /// zu `root` eskaliert, behält seine ursprüngliche `auid` — sie ist damit
    /// die einzige Kennung, die beantwortet, wer die Kette wirklich
    /// angestoßen hat. `None`, wenn der Sensor keine Audit-Herkunft besitzt
    /// (z. B. ein Prozess ohne Login-Session, etwa ein System-Dienst).
    pub auid: Option<u32>,
    /// Die cgroup des Akteurs, falls bekannt. **Kernel-ID-Wiederverwendung:**
    /// [`CgroupId`] identifiziert eine Gruppe nur innerhalb eines kurzen
    /// Zeitfensters eindeutig — der Kernel vergibt IDs entfernter cgroups
    /// nach hinreichendem Abstand erneut. Ein Konsument, der zwei `Actor`
    /// über die Zeit hinweg derselben cgroup zuordnen will, braucht
    /// zusätzlichen disambiguierenden Kontext (siehe [`CgroupId`]-Dokumentation
    /// in `harw-types`).
    pub cgroup: Option<CgroupId>,
}

/// Wie schwer eine Strukturabweichung wiegt.
///
/// # Description
/// Bewusst **grob** und quellenunabhängig: der Typ liegt in dieser Crate,
/// damit ein Sensor seine eigene, feinere Einstufung (etwa
/// `harw_dod_workspace::VersionSeverity` mit ihren SemVer-Feinheiten) beim
/// Melden **hierauf abbildet**, statt sie in Prosa aufzulösen und einer Regel
/// das Zurückraten zu überlassen.
///
/// Die Abbildung ist damit eine ausdrückliche Entscheidung des Sensors, an
/// genau einer Stelle, statt eine implizite Vereinbarung über Textformate
/// zwischen zwei Crates, die einander nicht kennen.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum DriftSeverity {
    /// Die Schwere ist nicht bekannt.
    ///
    /// Der Rückfall für Bestandsdatensätze aus der Zeit vor diesem Feld — und
    /// **nicht** dasselbe wie [`Self::Low`]. Wer eine unbekannte Schwere wie
    /// eine geringe behandelt, redet einen Befund klein, über den nie jemand
    /// entschieden hat.
    #[default]
    Unknown,
    /// Erwartbar und rückwärtskompatibel, etwa ein Patch-Sprung.
    Low,
    /// Bemerkenswert, aber innerhalb der Zusagen der Quelle.
    Medium,
    /// Bricht eine Zusage oder erweitert die Angriffsfläche — etwa ein
    /// API-brechender Versionssprung oder eine neue Abhängigkeit.
    High,
}

/// Ausgang eines Anmeldeversuchs.
///
/// # Description
/// Geschlossen: ein Anmeldeversuch endet entweder erfolgreich oder nicht,
/// nie in einem dritten Zustand, der hier modelliert werden müsste.
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::AuthOutcome;
///
/// let outcome = AuthOutcome::Failure;
/// assert_eq!(
///     serde_json::to_string(&outcome).expect("serializes"),
///     r#""failure""#
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthOutcome {
    /// Die Anmeldung ist gelungen.
    Success,
    /// Die Anmeldung ist gescheitert.
    Failure,
}

/// Art eines Sicherheitsereignisses. Geschlossen.
///
/// # Description
/// Jede Variante entspricht genau einer Beobachtungsart, die eine
/// Sensor-Crate melden darf. `ProcessExec::argv_digest` trägt bewusst einen
/// [`ContentDigest`] und **nicht** die rohe Kommandozeile: die Kommandozeile
/// ist angreiferkontrolliert und enthält regelmäßig Geheimnisse
/// (Zugangstoken als Argument, eingebettete Passwörter). Ein Digest belegt
/// Gleichheit — zwei Ereignisse hatten dieselbe Kommandozeile — ohne den
/// Inhalt selbst zu transportieren oder dauerhaft zu speichern.
///
/// # Errors
/// Keine eigenen Fehler; `#[serde(deny_unknown_fields)]` lässt
/// Deserialisierung mit unbekanntem Feld scheitern.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::EventKind;
/// use harw_types::ContentDigest;
///
/// let kind = EventKind::FileWrite { path: "/etc/passwd".to_owned() };
/// assert_eq!(
///     serde_json::to_string(&kind).expect("serializes"),
///     r#"{"kind":"file-write","path":"/etc/passwd"}"#
/// );
/// let _ = ContentDigest::of(b"unused in this example");
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "kind")]
pub enum EventKind {
    /// Ein Prozess wurde gestartet.
    ProcessExec {
        /// Pfad des ausgeführten Programms.
        path: String,
        /// Digest der Kommandozeile (`argv`), nicht die Kommandozeile selbst
        /// — siehe Typ-Doku oben.
        argv_digest: ContentDigest,
    },
    /// Eine Datei wurde geschrieben.
    FileWrite {
        /// Pfad der geschriebenen Datei.
        path: String,
    },
    /// Ausgehender Netzwerkverkehr zu einem neuen Ziel.
    EgressFlow {
        /// Zieladresse oder -host.
        destination: String,
        /// Zielport.
        port: u16,
    },
    /// Ein Listener wurde auf einem Port geöffnet.
    ListenerOpened {
        /// Der geöffnete Port.
        port: u16,
    },
    /// Ein Anmeldeversuch wurde beobachtet.
    AuthEvent {
        /// Erfolg oder Fehlschlag der Anmeldung.
        outcome: AuthOutcome,
    },
    /// Eine überwachte Struktur (Konfiguration, Workspace-Layout, …) ist
    /// abgewichen.
    StructureDrift {
        /// Wie schwer die Abweichung wiegt — **strukturiert**, nicht aus dem
        /// Text ableitbar.
        ///
        /// Ohne dieses Feld musste eine Regel die Schwere aus den festen
        /// Textpräfixen von [`Self::StructureDrift::detail`] zurückgewinnen.
        /// Das ist genau die Kopplung, gegen die dieses Programm antritt: der
        /// Sensor **hat** die Information strukturiert, flacht sie zu Prosa
        /// ab, und die Regel liest sie durch Mustervergleich wieder heraus.
        /// Ändert jemand eine Formulierung, stuft die Regel still falsch ein
        /// — und niemand erfährt es.
        ///
        /// `#[serde(default)]`, damit Bestandsdateien ohne dieses Feld lesbar
        /// bleiben; sie fallen dann auf [`DriftSeverity::Unknown`] zurück.
        #[serde(default)]
        severity: DriftSeverity,
        /// Freitext-Beschreibung der Abweichung, **für Menschen**. Enthält
        /// keine Geheimnisse — Sensoren, die das nicht garantieren können,
        /// melden hier keine Rohdaten, sondern eine bereits redigierte
        /// Zusammenfassung.
        ///
        /// **Keine Regel wertet diesen Text aus.** Er ist Beschreibung, nicht
        /// Datenquelle.
        detail: String,
    },
    /// Ein Sensor hat sich selbst als beeinträchtigt gemeldet.
    SensorDegraded {
        /// Welcher Sensor betroffen ist.
        sensor: SensorId,
    },
}

/// Ein sicherheitsrelevantes Ereignis.
///
/// # Description
/// Getrennter Strom von [`crate::sample::HostSample`]: Messwerte sind
/// langweilig und häufig, Ereignisse sind selten und wichtig (siehe
/// `sample`-Moduldoku für die volle Begründung).
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::{EventKind, SecurityEvent};
/// use harw_types::SensorId;
///
/// let event = SecurityEvent {
///     sensor: SensorId::from_str("fsmon-0"),
///     observed_at: jiff::Timestamp::UNIX_EPOCH,
///     actor: None,
///     kind: EventKind::ListenerOpened { port: 8080 },
/// };
/// assert_eq!(event.actor, None);
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityEvent {
    /// Welcher Sensor das Ereignis erzeugt hat.
    pub sensor: SensorId,
    /// Wann das Ereignis beobachtet wurde — injizierte Zeit, siehe
    /// [`crate::sensor::Sensor::poll`].
    pub observed_at: Timestamp,
    /// Wer das Ereignis ausgelöst hat, falls bekannt (nicht jedes Ereignis
    /// hat einen Akteur, z. B. `SensorDegraded`).
    pub actor: Option<Actor>,
    /// Die Art des Ereignisses.
    pub kind: EventKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest() -> ContentDigest {
        ContentDigest::of(b"/usr/bin/curl --data secret=redacted")
    }

    #[test]
    fn test_event_kind_process_exec_json_is_stable() {
        let kind = EventKind::ProcessExec {
            path: "/usr/bin/curl".to_owned(),
            argv_digest: digest(),
        };
        let json = serde_json::to_string(&kind).expect("EventKind serializes");
        assert_eq!(
            json,
            format!(
                r#"{{"kind":"process-exec","path":"/usr/bin/curl","argv_digest":"{digest}"}}"#,
                digest = digest()
            )
        );
    }

    #[test]
    fn test_event_kind_file_write_json_is_stable() {
        let kind = EventKind::FileWrite {
            path: "/etc/shadow".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&kind).expect("EventKind serializes"),
            r#"{"kind":"file-write","path":"/etc/shadow"}"#
        );
    }

    #[test]
    fn test_event_kind_egress_flow_json_is_stable() {
        let kind = EventKind::EgressFlow {
            destination: "198.51.100.7".to_owned(),
            port: 443,
        };
        assert_eq!(
            serde_json::to_string(&kind).expect("EventKind serializes"),
            r#"{"kind":"egress-flow","destination":"198.51.100.7","port":443}"#
        );
    }

    #[test]
    fn test_event_kind_listener_opened_json_is_stable() {
        let kind = EventKind::ListenerOpened { port: 8080 };
        assert_eq!(
            serde_json::to_string(&kind).expect("EventKind serializes"),
            r#"{"kind":"listener-opened","port":8080}"#
        );
    }

    #[test]
    fn test_event_kind_auth_event_json_is_stable() {
        let kind = EventKind::AuthEvent {
            outcome: AuthOutcome::Success,
        };
        assert_eq!(
            serde_json::to_string(&kind).expect("EventKind serializes"),
            r#"{"kind":"auth-event","outcome":"success"}"#
        );
    }

    #[test]
    fn test_event_kind_structure_drift_json_is_stable() {
        let kind = EventKind::StructureDrift {
            severity: DriftSeverity::High,
            detail: "unexpected setuid binary in workspace".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&kind).expect("EventKind serializes"),
            r#"{"kind":"structure-drift","severity":"high","detail":"unexpected setuid binary in workspace"}"#
        );
    }

    #[test]
    fn test_event_kind_structure_drift_without_severity_still_deserializes() {
        // Die Bestandsform aus der Zeit vor dem strukturierten Feld. Sie muss
        // lesbar bleiben und auf `Unknown` fallen — nicht auf `Low`.
        let legacy = r#"{"kind":"structure-drift","detail":"neues Workspace-Member: a"}"#;

        let kind: EventKind = serde_json::from_str(legacy).expect("Bestandsform bleibt lesbar");

        assert!(matches!(
            kind,
            EventKind::StructureDrift {
                severity: DriftSeverity::Unknown,
                ..
            }
        ));
    }

    #[test]
    fn test_event_kind_sensor_degraded_json_is_stable() {
        let kind = EventKind::SensorDegraded {
            sensor: SensorId::from_str("thermal-0"),
        };
        assert_eq!(
            serde_json::to_string(&kind).expect("EventKind serializes"),
            r#"{"kind":"sensor-degraded","sensor":"thermal-0"}"#
        );
    }

    fn event() -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("authlog-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: Some(Actor {
                uid: 0,
                auid: Some(1000),
                cgroup: None,
            }),
            kind: EventKind::AuthEvent {
                outcome: AuthOutcome::Failure,
            },
        }
    }

    #[test]
    fn test_security_event_deserialize_accepts_well_formed_static_fixture() {
        let fixture: &'static str = r#"{
            "sensor": "authlog-0",
            "observed_at": "1970-01-01T00:00:00Z",
            "actor": {"uid": 0, "auid": 1000, "cgroup": null},
            "kind": {"kind": "auth-event", "outcome": "failure"}
        }"#;
        let parsed: SecurityEvent =
            serde_json::from_str(fixture).expect("fixture deserializes");
        assert_eq!(parsed, event());
    }

    #[test]
    fn test_security_event_deserialize_rejects_unknown_field() {
        let fixture: &'static str = r#"{
            "sensor": "authlog-0",
            "observed_at": "1970-01-01T00:00:00Z",
            "actor": null,
            "kind": {"kind": "listener-opened", "port": 22},
            "unexpected": true
        }"#;
        assert!(serde_json::from_str::<SecurityEvent>(fixture).is_err());
    }

    #[test]
    fn test_actor_auid_can_diverge_from_uid_after_privilege_escalation() {
        let actor = Actor {
            uid: 0,
            auid: Some(1000),
            cgroup: None,
        };
        assert_ne!(actor.uid, actor.auid.expect("auid present"));
    }
}
