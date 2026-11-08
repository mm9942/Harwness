//! Sicherheitsereignisse: wer etwas ausgelöst hat und welcher Art es ist (Contract-Master §G).
//!
//! # Verantwortungsbereich
//! Trägt [`Actor`] (samt optionalem [`ProcessContext`]), [`AuthOutcome`],
//! [`EventKind`] und [`SecurityEvent`] —
//! den zweiten, seltenen und wichtigen Beobachtungsstrom neben
//! [`crate::sample::HostSample`] (siehe dortige Moduldoku für die Begründung
//! der Trennung).
//!
//! # Nebenläufigkeit
//! Reine Datentypen: `Send + Sync` automatisch, kein internes Locking.
//!
//! # Fehler
//! Keine eigenen — dieses Modul definiert nur Daten und liest keine Quelle.
//! `#[serde(deny_unknown_fields)]` auf [`SecurityEvent`], [`EventKind`],
//! [`Actor`] und [`ProcessContext`] lässt `serde_json` beim Deserialisieren
//! mit einem Serde-Fehler scheitern, wenn ein unbekanntes Feld auftaucht.
//!
//! # Prozesskontext (PL-100 WP0.15)
//! [`Actor::process`] ist **optional** und wird nur serialisiert, wenn er
//! gesetzt ist (`skip_serializing_if`). Damit gilt:
//! - Bestands-JSON ohne `process` bleibt lesbar (`serde(default)`).
//! - Ein `Actor` ohne Prozesskontext serialisiert byte-gleich wie vor
//!   diesem Feld; Digests über solche Ereignisse ändern sich nicht.
//! - Ein Leser **vor** diesem Feld lehnt nur Ereignisse **mit**
//!   Prozesskontext ab (`deny_unknown_fields`). Ausgerollt wird deshalb
//!   lesend zuerst (Leitprinzip I11): erst der Sentinel, dann die Sonden.
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
//!     actor: Some(Actor { uid: 1000, auid: Some(1000), cgroup: None, process: None }),
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
/// let root_via_sudo = Actor { uid: 0, auid: Some(1000), cgroup: None, process: None };
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
    /// Der auslösende Prozess, falls der Sensor ihn kennt (PL-100 WP0.15,
    /// Korrelation von Befund und Prozess).
    ///
    /// `None`, wenn der Sensor keinen Prozess beobachtet (etwa ein
    /// Anmeldeversuch aus dem Audit-Log ohne pid). Optional und nur
    /// serialisiert, wenn gesetzt — siehe Moduldoku, Abschnitt
    /// „Prozesskontext", für die Wire-Verträglichkeit. **PID-Wiederverwendung:**
    /// eine pid ist nur zum Beobachtungszeitpunkt eindeutig; wer sie später
    /// nutzt (etwa für ein Signal), muss sie erneut gegen `cgroup` und
    /// Startzeit prüfen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessContext>,
}

/// Der Prozess hinter einem [`Actor`] (PL-100 WP0.15).
///
/// # Description
/// Trägt, was ein Sensor über den auslösenden Prozess **beobachtet** hat —
/// nie Geratenes. Jedes Feld außer `pid` ist optional, weil nicht jede
/// Quelle alles liefert: der eBPF-Wire-Kopf trägt `tgid`, die Eltern-`tgid`
/// und die **reale** UID, aber keine effektive UID; ein Flow-Ereignis trägt
/// keinen `comm`.
///
/// `comm` ist **angreiferkontrolliert** (jeder Prozess setzt seinen Namen
/// selbst, `prctl(PR_SET_NAME)`). Der erzeugende Sensor escapt ihn deshalb,
/// bevor er hier landet: unsichtbare und steuernde Zeichen erscheinen als
/// sichtbares `\u{…}`, die Länge ist auf [`Self::COMM_MAX_CHARS`] Zeichen
/// begrenzt (gemeinsamer Escaper `harw-untrusted-text`, PL-100 WP0.8).
/// Diese Crate selbst escapt nicht — sie ist Vokabular ohne Parselogik.
///
/// # Errors
/// Keine eigenen; `#[serde(deny_unknown_fields)]` lässt Deserialisierung
/// mit unbekanntem Feld scheitern.
///
/// # Concurrency
/// Reiner Werttyp: `Send + Sync`.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::{Actor, ProcessContext};
///
/// let process = ProcessContext::from_pid(4_242).map(|p| ProcessContext {
///     ppid: Some(1),
///     comm: Some("sshd".to_owned()),
///     ..p
/// });
/// let actor = Actor { uid: 0, auid: None, cgroup: None, process };
/// assert_eq!(actor.process.as_ref().map(|p| p.pid), Some(4_242));
/// assert_eq!(ProcessContext::from_pid(0), None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessContext {
    /// Die Prozess-ID im Sinn des Userlands (Kernel: `tgid`), nie `0`.
    pub pid: u32,
    /// Die Prozess-ID des Elternprozesses (Kernel: `tgid` von
    /// `real_parent`), falls bekannt. Nie `Some(0)`: eine nicht lesbare
    /// Eltern-ID ist `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ppid: Option<u32>,
    /// Der Prozessname (`comm`, höchstens `TASK_COMM_LEN` Byte im Kernel),
    /// **bereits escapt** und auf [`Self::COMM_MAX_CHARS`] Zeichen begrenzt.
    /// `None`, wenn die Quelle keinen Namen liefert oder er leer war.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comm: Option<String>,
    /// Die effektive UID zum Beobachtungszeitpunkt, falls die Quelle sie
    /// kennt. Nicht zu verwechseln mit [`Actor::uid`], die je nach Quelle
    /// die reale UID ist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub euid: Option<u32>,
}

impl ProcessContext {
    /// Linux' `TASK_COMM_LEN`: die Breite des Kernel-Feldes `comm` in Byte,
    /// abschließendes NUL eingerechnet.
    ///
    /// # Description
    /// Ein Erzeuger kürzt einen rohen `comm` auf höchstens so viele Byte,
    /// bevor er ihn escapt — mehr kann der Kernel nicht geliefert haben.
    pub const KERNEL_COMM_BYTES: usize = 16;

    /// Obergrenze für [`Self::comm`] **nach** dem Escapen, in Zeichen.
    ///
    /// # Description
    /// Dieselbe Grenze wie `process.comm` in der Evidence-Card des
    /// Findings-Exports v3, damit ein Konsument den Wert ohne zweites
    /// Kürzen übernehmen kann.
    pub const COMM_MAX_CHARS: usize = 32;

    /// Baut einen Prozesskontext, der nur die pid kennt.
    ///
    /// # Arguments
    /// - `pid` (`u32`): die Prozess-ID (Kernel: `tgid`).
    ///
    /// # Returns
    /// `Some` mit allen optionalen Feldern `None`; `None` für `pid == 0`
    /// (der Idle-Task ist nie ein Akteur, eine `0` heißt „unbekannt").
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_signals::ProcessContext;
    ///
    /// assert_eq!(ProcessContext::from_pid(7).map(|p| p.pid), Some(7));
    /// assert_eq!(ProcessContext::from_pid(0), None);
    /// ```
    #[must_use]
    pub fn from_pid(pid: u32) -> Option<Self> {
        (pid != 0).then_some(Self {
            pid,
            ppid: None,
            comm: None,
            euid: None,
        })
    }
}

/// Prefix that marks an [`Actor::cgroup`] value as a **raw kernel cgroup
/// id** forwarded by a privileged probe, not as a resolved cgroup path.
///
/// # Description
/// PL-97 E4: the BPF probe sees only the numeric cgroup id in its wire
/// header; mapping it to a path is the unprivileged sentinel's job. The id
/// travels in the existing `cgroup` field (`"cgroup-id:<decimal>"`) instead
/// of a new field, because [`Actor`] is `deny_unknown_fields`: a new field
/// would make an older sentinel reject every event, while this encoding is
/// accepted by every reader of the existing schema. (The later, optional
/// [`Actor::process`] field is skipped when absent, so only events that
/// actually carry a process context need a reader that knows it.) A resolved path is
/// always absolute (starts with `/`), so the two forms never collide.
pub const RAW_CGROUP_ID_PREFIX: &str = "cgroup-id:";

impl Actor {
    /// Encodes a raw kernel cgroup id for [`Actor::cgroup`].
    ///
    /// # Arguments
    /// - `cgroup_id` (`u64`): the kernel cgroup id (cgroup v2: the inode
    ///   number of the cgroup directory). `0` is never a valid id.
    ///
    /// # Returns
    /// `Some` raw-id marker (see [`RAW_CGROUP_ID_PREFIX`]); `None` for `0`
    /// (an absent id stays absent).
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_signals::Actor;
    ///
    /// let actor = Actor { uid: 1000, auid: None, cgroup: Actor::raw_cgroup(42), process: None };
    /// assert_eq!(actor.raw_cgroup_id(), Some(42));
    /// assert_eq!(Actor::raw_cgroup(0), None);
    /// ```
    #[must_use]
    pub fn raw_cgroup(cgroup_id: u64) -> Option<CgroupId> {
        if cgroup_id == 0 {
            return None;
        }
        CgroupId::try_from_str(format!("{RAW_CGROUP_ID_PREFIX}{cgroup_id}")).ok()
    }

    /// Decodes a raw kernel cgroup id from [`Actor::cgroup`].
    ///
    /// # Returns
    /// The id when `cgroup` carries a well-formed raw-id marker (decimal,
    /// non-zero); `None` for no cgroup, a resolved path or anything else.
    #[must_use]
    pub fn raw_cgroup_id(&self) -> Option<u64> {
        let digits = self
            .cgroup
            .as_ref()?
            .as_str()
            .strip_prefix(RAW_CGROUP_ID_PREFIX)?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u64>().ok().filter(|id| *id != 0)
    }
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
    ///
    /// **Kein Eins-zu-eins mit Versuchen:** ein einzelner Anmeldeversuch
    /// ergibt je nach Quelle mehrere `AuthEvent`s mit demselben `outcome` —
    /// bei Audit-Records etwa ein PAM-`USER_AUTH` und ein `USER_LOGIN` des
    /// Dienstes, bei `sshd` zusätzlich eines je gescheiterter Methode. Der
    /// `actor` ist der meldende Dienst (bei `sshd` meist `uid` 0), nicht das
    /// Zielkonto. Eine Regel, die gescheiterte Anmeldungen zählt, muss mit
    /// dieser Vielfachheit rechnen und darf `uid == 0` nicht als „Anmeldung
    /// als root" lesen (siehe `harw_dod_authlog::LOGIN_RECORD_TYPES`).
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
    use crate::test_support::{TestError, TestResult, ctx};

    fn digest() -> ContentDigest {
        ContentDigest::of(b"/usr/bin/curl --data secret=redacted")
    }

    #[test]
    fn test_event_kind_process_exec_json_is_stable() -> TestResult {
        let kind = EventKind::ProcessExec {
            path: "/usr/bin/curl".to_owned(),
            argv_digest: digest(),
        };
        let json = serde_json::to_string(&kind).map_err(ctx("EventKind serializes"))?;
        assert_eq!(
            json,
            format!(
                r#"{{"kind":"process-exec","path":"/usr/bin/curl","argv_digest":"{digest}"}}"#,
                digest = digest()
            )
        );
        Ok(())
    }

    #[test]
    fn test_event_kind_file_write_json_is_stable() -> TestResult {
        let kind = EventKind::FileWrite {
            path: "/etc/shadow".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&kind).map_err(ctx("EventKind serializes"))?,
            r#"{"kind":"file-write","path":"/etc/shadow"}"#
        );
        Ok(())
    }

    #[test]
    fn test_event_kind_egress_flow_json_is_stable() -> TestResult {
        let kind = EventKind::EgressFlow {
            destination: "198.51.100.7".to_owned(),
            port: 443,
        };
        assert_eq!(
            serde_json::to_string(&kind).map_err(ctx("EventKind serializes"))?,
            r#"{"kind":"egress-flow","destination":"198.51.100.7","port":443}"#
        );
        Ok(())
    }

    #[test]
    fn test_event_kind_listener_opened_json_is_stable() -> TestResult {
        let kind = EventKind::ListenerOpened { port: 8080 };
        assert_eq!(
            serde_json::to_string(&kind).map_err(ctx("EventKind serializes"))?,
            r#"{"kind":"listener-opened","port":8080}"#
        );
        Ok(())
    }

    #[test]
    fn test_event_kind_auth_event_json_is_stable() -> TestResult {
        let kind = EventKind::AuthEvent {
            outcome: AuthOutcome::Success,
        };
        assert_eq!(
            serde_json::to_string(&kind).map_err(ctx("EventKind serializes"))?,
            r#"{"kind":"auth-event","outcome":"success"}"#
        );
        Ok(())
    }

    #[test]
    fn test_event_kind_structure_drift_json_is_stable() -> TestResult {
        let kind = EventKind::StructureDrift {
            severity: DriftSeverity::High,
            detail: "unexpected setuid binary in workspace".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&kind).map_err(ctx("EventKind serializes"))?,
            r#"{"kind":"structure-drift","severity":"high","detail":"unexpected setuid binary in workspace"}"#
        );
        Ok(())
    }

    #[test]
    fn test_event_kind_structure_drift_without_severity_still_deserializes() -> TestResult {
        // Die Bestandsform aus der Zeit vor dem strukturierten Feld. Sie muss
        // lesbar bleiben und auf `Unknown` fallen — nicht auf `Low`.
        let legacy = r#"{"kind":"structure-drift","detail":"neues Workspace-Member: a"}"#;

        let kind: EventKind =
            serde_json::from_str(legacy).map_err(ctx("Bestandsform bleibt lesbar"))?;

        assert!(matches!(
            kind,
            EventKind::StructureDrift {
                severity: DriftSeverity::Unknown,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn test_event_kind_sensor_degraded_json_is_stable() -> TestResult {
        let kind = EventKind::SensorDegraded {
            sensor: SensorId::from_str("thermal-0"),
        };
        assert_eq!(
            serde_json::to_string(&kind).map_err(ctx("EventKind serializes"))?,
            r#"{"kind":"sensor-degraded","sensor":"thermal-0"}"#
        );
        Ok(())
    }

    fn event() -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("authlog-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: Some(Actor {
                uid: 0,
                auid: Some(1000),
                cgroup: None,
                process: None,
            }),
            kind: EventKind::AuthEvent {
                outcome: AuthOutcome::Failure,
            },
        }
    }

    #[test]
    fn test_security_event_deserialize_accepts_well_formed_static_fixture() -> TestResult {
        let fixture: &'static str = r#"{
            "sensor": "authlog-0",
            "observed_at": "1970-01-01T00:00:00Z",
            "actor": {"uid": 0, "auid": 1000, "cgroup": null},
            "kind": {"kind": "auth-event", "outcome": "failure"}
        }"#;
        let parsed: SecurityEvent =
            serde_json::from_str(fixture).map_err(ctx("fixture deserializes"))?;
        assert_eq!(parsed, event());
        Ok(())
    }

    #[test]
    fn test_security_event_deserialize_rejects_unknown_field() -> TestResult {
        let fixture: &'static str = r#"{
            "sensor": "authlog-0",
            "observed_at": "1970-01-01T00:00:00Z",
            "actor": null,
            "kind": {"kind": "listener-opened", "port": 22},
            "unexpected": true
        }"#;
        assert!(serde_json::from_str::<SecurityEvent>(fixture).is_err());
        Ok(())
    }

    #[test]
    fn test_actor_auid_can_diverge_from_uid_after_privilege_escalation() -> TestResult {
        let actor = Actor {
            uid: 0,
            auid: Some(1000),
            cgroup: None,
            process: None,
        };
        assert_ne!(
            actor.uid,
            actor.auid.ok_or(TestError::Missing("auid present"))?
        );
        Ok(())
    }

    #[test]
    fn test_raw_cgroup_id_round_trips_and_rejects_other_values() {
        let actor = Actor {
            uid: 1,
            auid: None,
            cgroup: Actor::raw_cgroup(4_242),
            process: None,
        };
        assert_eq!(
            actor.cgroup.as_ref().map(CgroupId::as_str),
            Some("cgroup-id:4242")
        );
        assert_eq!(actor.raw_cgroup_id(), Some(4_242));
        assert_eq!(Actor::raw_cgroup(0), None);
        for other in [
            "/sys/fs/cgroup/harw.slice",
            "cgroup-id:",
            "cgroup-id:+1",
            "cgroup-id:0",
            "cgroup-1",
        ] {
            let actor = Actor {
                uid: 1,
                auid: None,
                cgroup: Some(CgroupId::from_str(other)),
                process: None,
            };
            assert_eq!(actor.raw_cgroup_id(), None, "{other}");
        }
    }

    // --- PL-100 WP0.15: Prozesskontext ---------------------------------------

    fn actor_with_process() -> Actor {
        Actor {
            uid: 1000,
            auid: None,
            cgroup: None,
            process: Some(ProcessContext {
                pid: 4_242,
                ppid: Some(1),
                comm: Some("sshd".to_owned()),
                euid: Some(0),
            }),
        }
    }

    #[test]
    fn test_actor_without_process_serializes_byte_identical_to_legacy_form() -> TestResult {
        let actor = Actor {
            uid: 1000,
            auid: Some(1000),
            cgroup: None,
            process: None,
        };
        assert_eq!(
            serde_json::to_string(&actor).map_err(ctx("Actor serializes"))?,
            r#"{"uid":1000,"auid":1000,"cgroup":null}"#
        );
        Ok(())
    }

    #[test]
    fn test_actor_legacy_json_without_process_still_deserializes() -> TestResult {
        let legacy = r#"{"uid":0,"auid":1000,"cgroup":null}"#;
        let actor: Actor =
            serde_json::from_str(legacy).map_err(ctx("Bestandsform bleibt lesbar"))?;
        assert_eq!(actor.process, None);
        assert_eq!(actor.uid, 0);
        Ok(())
    }

    #[test]
    fn test_actor_with_process_json_is_stable_and_round_trips() -> TestResult {
        let actor = actor_with_process();
        let json = serde_json::to_string(&actor).map_err(ctx("Actor serializes"))?;
        assert_eq!(
            json,
            r#"{"uid":1000,"auid":null,"cgroup":null,"process":{"pid":4242,"ppid":1,"comm":"sshd","euid":0}}"#
        );
        let back: Actor = serde_json::from_str(&json).map_err(ctx("Actor deserializes"))?;
        assert_eq!(back, actor);
        Ok(())
    }

    #[test]
    fn test_process_context_omits_absent_optional_fields() -> TestResult {
        let process = ProcessContext::from_pid(7).ok_or(TestError::Missing("pid 7 gültig"))?;
        let json = serde_json::to_string(&process).map_err(ctx("serializes"))?;
        assert_eq!(json, r#"{"pid":7}"#);
        let back: ProcessContext = serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(back, process);
        Ok(())
    }

    #[test]
    fn test_process_context_rejects_unknown_field() {
        let fixture = r#"{"pid":7,"cmdline":"curl --data secret"}"#;
        assert!(serde_json::from_str::<ProcessContext>(fixture).is_err());
        let in_actor = r#"{"uid":1,"auid":null,"cgroup":null,"process":{"pid":7,"x":1}}"#;
        assert!(serde_json::from_str::<Actor>(in_actor).is_err());
    }

    #[test]
    fn test_process_context_from_pid_rejects_zero() {
        assert_eq!(ProcessContext::from_pid(0), None);
        assert_eq!(
            ProcessContext::from_pid(1),
            Some(ProcessContext {
                pid: 1,
                ppid: None,
                comm: None,
                euid: None,
            })
        );
    }

    #[test]
    fn test_security_event_with_process_round_trips() -> TestResult {
        let mut expected = event();
        expected.actor = Some(actor_with_process());
        let json = serde_json::to_string(&expected).map_err(ctx("event serializes"))?;
        let back: SecurityEvent = serde_json::from_str(&json).map_err(ctx("event deserializes"))?;
        assert_eq!(back, expected);
        Ok(())
    }
}
