//! `FindingThrottle`: Entprellung wiederholter Befunde (PL-100 WP0.4).
//!
//! # Zweck
//! Seit der Delta-Auswertung (WP0.1) läuft jedes Ereignis genau einmal durch
//! die Regeln. Ein lauter Dienst oder ein Angreifer erzeugt aber viele
//! **gleichartige** Ereignisse — etwa dieselbe ausgehende Verbindung alle
//! paar Sekunden. Dieses Modul fasst sie über einen [`DedupKey`] zusammen:
//! innerhalb der Abklingzeit (`cooldown`, Vorgabe
//! [`DEFAULT_COOLDOWN_SECS`] = 10 min) zählt eine Wiederholung nur `occurrences` hoch und wird weder ins
//! Journal (`warn`) noch in den Befundexport geschrieben. Gemeldet wird sie
//! erst wieder, wenn die Abklingzeit abgelaufen ist oder sich der Befund
//! **echt** geändert hat (siehe unten). Der Exporter erhält je gemeldetem
//! Befund einen [`ThrottleStamp`] mit `dedup_key`, `occurrences` und
//! `first_seen` (Export v3).
//!
//! # Der `dedup_key`
//! Die ersten 16 Byte eines BLAKE3-Digests (über
//! [`harw_types::ContentDigest`], keine neue Abhängigkeit) über eine
//! längenpräfixierte Kodierung von Domäne, `rule_id`, `uid`, `cgroup_kid`
//! und grobem Subjekt ([`CoarseSubject`]), als 32 Hex-Zeichen
//! (Kleinbuchstaben). Das grobe Subjekt:
//! - **Egress:** Netz `/24` (IPv4, auch IPv4-gemappte IPv6-Adressen) bzw.
//!   `/64` (IPv6) plus Zielport — ein neuer Port ist damit immer ein neuer
//!   Schlüssel. Ein Ziel ohne IP-Literal geht nur als Digest ein.
//! - **Drift:** ein Digest aus Quelle (Sensor-ID) und Detailtext plus
//!   Drift-Klasse ([`harw_dod_signals::DriftSeverity`]). Der Detailtext ist
//!   das einzige objektbezogene Merkmal, das ein Drift-Ereignis trägt; er
//!   geht **nur** als Digest ein.
//! - **exec / file-write:** Digest des Pfads; **Listener:** Port;
//!   **Anmeldung:** Ergebnis; **SensorDegraded:** Digest der Sensor-ID.
//! - **Keinem einzelnen Ereignis zuordenbar** (Sample-Regeln, mehrdeutige
//!   Zuordnung): Digest der Befundzusammenfassung — für die Sample-Regeln
//!   eine stabile Funktion der Metrik.
//!
//! Roher Angreifertext erscheint weder im Schlüssel noch im gespeicherten
//! Zustand noch im Journal.
//!
//! # Echte Änderung
//! Eine Wiederholung innerhalb der Abklingzeit wird trotzdem gemeldet, wenn
//! - ihre Härte stärker ist als jede bisher gemeldete
//!   (`Inferred` < `Correlated` < `Observed`), oder
//! - ihre Schwere höher ist als jede bisher gemeldete.
//!
//! Ein neuer Zielport ist bereits ein neuer Schlüssel (siehe oben).
//!
//! # Zeitbasis
//! Die Abklingzeit läuft in Sentinel-Zeit (`now` des Zyklus), nicht in
//! Ereigniszeit: ein Erzeuger kann `observed_at` nicht so wählen, dass er
//! sich selbst unterdrückt. `first_seen` ist dagegen die früheste
//! Ereigniszeit des Schlüssels. Läuft die Uhr rückwärts (`now` vor der
//! letzten Meldung), wird gemeldet statt unbestimmt lange unterdrückt.
//!
//! # Zustand
//! JSON in `<home>/`[`DEFAULT_STATE_FILE_NAME`] (`--home`, im Betrieb das
//! Zustandsverzeichnis `@STATEDIR@`), Modus [`STATE_FILE_MODE`] (`0600`),
//! atomar ersetzt (temporäre Geschwisterdatei, `sync_data`, `rename`; siehe
//! `crate::health_export`). Gespeichert werden nur Schlüssel, Zähler,
//! Zeitstempel sowie stärkste Härte und höchste Schwere — kein Rohtext.
//! Höchstens [`ThrottleConfig::max_keys`] Schlüssel (Vorgabe [`MAX_KEYS`]);
//! beim Überlauf fällt der am längsten nicht gesehene heraus (LRU).
//! Geschrieben wird am Zyklusende, wenn sich etwas geändert hat **und**
//! entweder in diesem Zyklus etwas gemeldet wurde oder seit dem letzten
//! Schreiben [`PERSIST_MIN_INTERVAL`] vergangen ist. Ein fehlender Zustand
//! ist der Normalfall beim ersten Start; ein defekter, zu großer oder
//! fremder Zustand wird mit einer Warnung (ohne Pfad) verworfen, nie fatal.
//!
//! # Nebenläufigkeit
//! Lebt ausschließlich im Sammelthread in `crate::findings::RuleCycle`
//! (`&mut self`); kein interner Lock.
//!
//! # Fehler
//! Keine nach außen. Lade- und Schreibfehler werden geloggt (die erste
//! Schreibstörung als `warn`, Wiederholungen als `debug`) und beim nächsten
//! fälligen Zyklus erneut versucht.
//!
//! # Examples
//! ```rust,ignore
//! let mut throttle = FindingThrottle::open(ThrottleConfig::default(), None);
//! let key = DedupKey::derive("egress-flow", Some(1000), None, &subject);
//! let first = throttle.observe(key, Hardness::Observed, Severity::High, at, now);
//! assert!(first.emits());
//! let repeat = throttle.observe(key, Hardness::Observed, Severity::High, at, now);
//! assert!(!repeat.emits());
//! assert_eq!(repeat.stamp.occurrences, 2);
//! ```

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use harw_dod_signals::{AuthOutcome, DriftSeverity, EventKind, Hardness, SecurityEvent, Severity};
use harw_types::ContentDigest;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::health_export;

/// Vorgabe der Abklingzeit in Sekunden (10 min); `--finding-cooldown-secs`.
pub const DEFAULT_COOLDOWN_SECS: u64 = 600;

/// Größte zulässige Abklingzeit in Sekunden (7 Tage).
pub const MAX_COOLDOWN_SECS: u64 = 7 * 24 * 60 * 60;

/// Höchstzahl gehaltener (und gespeicherter) Schlüssel.
pub const MAX_KEYS: usize = 4096;

/// Dateiname des Zustands unter `--home`.
pub const DEFAULT_STATE_FILE_NAME: &str = "finding-throttle.json";

/// Schemaversion des gespeicherten Zustands.
pub const STATE_SCHEMA_VERSION: u32 = 1;

/// Modus einer neu angelegten Zustandsdatei: nur der Sentinel liest und
/// schreibt (die Lesergruppe des Zustandsverzeichnisses sieht sie nicht).
pub const STATE_FILE_MODE: u32 = 0o600;

/// Größte akzeptierte Zustandsdatei in Byte (2 MiB; [`MAX_KEYS`] Einträge
/// belegen rund 1 MiB).
pub const MAX_STATE_BYTES: u64 = 2 * 1024 * 1024;

/// Mindestabstand zwischen zwei Schreibvorgängen, solange nichts gemeldet
/// wurde (nur Zählerstände ändern sich).
pub const PERSIST_MIN_INTERVAL: SignedDuration = SignedDuration::from_secs(60);

/// Länge eines [`DedupKey`] in Byte.
pub const DEDUP_KEY_BYTES: usize = 16;

/// Länge der Hex-Form eines [`DedupKey`].
const DEDUP_KEY_HEX_LEN: usize = DEDUP_KEY_BYTES * 2;

/// Domänentrenner des Schlüssel-Urbilds; eine geänderte Kodierung bekommt
/// eine neue Version, damit alte und neue Schlüssel nie zusammenfallen.
const DEDUP_KEY_DOMAIN: &[u8] = b"harw-sentinel/finding-dedup-key/v1";

/// Netzmaske des groben IPv4-Subjekts (`/24`).
const IPV4_COARSE_MASK: u32 = 0xFFFF_FF00;

/// Präfixlänge des groben IPv4-Subjekts.
const IPV4_COARSE_PREFIX: u8 = 24;

/// Netzmaske des groben IPv6-Subjekts (`/64`).
const IPV6_COARSE_MASK: u128 = u128::MAX << 64;

/// Präfixlänge des groben IPv6-Subjekts.
const IPV6_COARSE_PREFIX: u8 = 64;

// Kennbytes der Subjektarten im Schlüssel-Urbild. Stabil: eine Änderung
// verschiebt jeden gespeicherten Schlüssel.
const TAG_EGRESS_NETWORK: u8 = 1;
const TAG_EGRESS_NAME: u8 = 2;
const TAG_DRIFT: u8 = 3;
const TAG_EXEC: u8 = 4;
const TAG_FILE_WRITE: u8 = 5;
const TAG_LISTENER: u8 = 6;
const TAG_AUTH: u8 = 7;
const TAG_SENSOR_DEGRADED: u8 = 8;
const TAG_SUMMARY: u8 = 9;

/// Einstellungen eines [`FindingThrottle`].
///
/// # Examples
/// ```rust,ignore
/// let config = ThrottleConfig::with_cooldown_secs(300);
/// assert_eq!(config.max_keys, MAX_KEYS);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleConfig {
    /// Abklingzeit, in der Wiederholungen nur gezählt werden. `0` meldet
    /// jede Wiederholung (zählt aber weiter `occurrences`).
    pub cooldown: Duration,
    /// Höchstzahl gehaltener Schlüssel (LRU). `0` hält keinen Schlüssel und
    /// meldet damit jeden Befund.
    pub max_keys: usize,
}

impl ThrottleConfig {
    /// Einstellungen mit der Abklingzeit `secs` und [`MAX_KEYS`] Schlüsseln.
    ///
    /// # Arguments
    /// - `secs` (`u64`): Abklingzeit in Sekunden (`--finding-cooldown-secs`).
    #[must_use]
    pub const fn with_cooldown_secs(secs: u64) -> Self {
        Self {
            cooldown: Duration::from_secs(secs),
            max_keys: MAX_KEYS,
        }
    }
}

impl Default for ThrottleConfig {
    /// [`DEFAULT_COOLDOWN_SECS`] und [`MAX_KEYS`].
    fn default() -> Self {
        Self::with_cooldown_secs(DEFAULT_COOLDOWN_SECS)
    }
}

/// Der Entprell-Schlüssel eines Befundes (siehe Moduldoku).
///
/// # Description
/// 16 Byte, angezeigt und gespeichert als 32 Hex-Zeichen in
/// Kleinbuchstaben. Gleiche Regel, gleicher Akteur und gleiches grobes
/// Subjekt ergeben denselben Schlüssel.
///
/// # Concurrency
/// Reiner Werttyp, `Copy + Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DedupKey([u8; DEDUP_KEY_BYTES]);

impl DedupKey {
    /// Bildet den Schlüssel aus Regel, Akteur und grobem Subjekt.
    ///
    /// # Arguments
    /// - `rule_id` (`&str`): die auslösende Regel.
    /// - `uid` (`Option<u32>`): `Actor::uid` des auslösenden Ereignisses,
    ///   falls eindeutig bekannt.
    /// - `cgroup_kid` (`Option<u64>`): Kernel-ID der Cgroup, falls bekannt.
    /// - `subject` (`&CoarseSubject`): das grobe Subjekt.
    ///
    /// # Returns
    /// Die ersten [`DEDUP_KEY_BYTES`] Byte des BLAKE3-Digests über die
    /// längenpräfixierte Kodierung aller Teile.
    #[must_use]
    pub fn derive(
        rule_id: &str,
        uid: Option<u32>,
        cgroup_kid: Option<u64>,
        subject: &CoarseSubject,
    ) -> Self {
        let mut preimage = Vec::with_capacity(128);
        put_field(&mut preimage, DEDUP_KEY_DOMAIN);
        put_field(&mut preimage, rule_id.as_bytes());
        match uid {
            Some(uid) => {
                preimage.push(1);
                preimage.extend_from_slice(&uid.to_be_bytes());
            }
            None => preimage.push(0),
        }
        match cgroup_kid {
            Some(kid) => {
                preimage.push(1);
                preimage.extend_from_slice(&kid.to_be_bytes());
            }
            None => preimage.push(0),
        }
        subject.encode_into(&mut preimage);
        let digest = ContentDigest::of(&preimage);
        let mut key = [0_u8; DEDUP_KEY_BYTES];
        for (slot, byte) in key.iter_mut().zip(digest.as_bytes()) {
            *slot = *byte;
        }
        Self(key)
    }

    /// Liest die Hex-Form zurück (genau 32 Hex-Zeichen, Kleinbuchstaben);
    /// auch für die Exporttests (`crate::export`), die eine feste Zeile
    /// nachbauen.
    ///
    /// # Returns
    /// `None` bei jeder anderen Eingabe.
    pub(crate) fn parse_hex(text: &str) -> Option<Self> {
        if text.len() != DEDUP_KEY_HEX_LEN {
            return None;
        }
        let mut key = [0_u8; DEDUP_KEY_BYTES];
        for (slot, pair) in key.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
            let &[high, low] = pair else {
                return None;
            };
            *slot = (lower_hex_value(high)? << 4) | lower_hex_value(low)?;
        }
        Some(Self(key))
    }
}

impl fmt::Display for DedupKey {
    /// 32 Hex-Zeichen in Kleinbuchstaben.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Wert einer Hex-Ziffer `0-9`/`a-f`; Großbuchstaben werden abgelehnt, weil
/// dieses Modul nur Kleinbuchstaben schreibt.
fn lower_hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(byte.wrapping_sub(b'a').wrapping_add(10)),
        _ => None,
    }
}

/// Hängt `bytes` mit einem 4-Byte-Längenpräfix (Big Endian) an `buf` an.
fn put_field(buf: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(bytes);
}

/// Das grobe Subjekt eines Befundes — was „dasselbe" bedeutet.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Der `dedup_key`". Jede Variante trägt nur
/// grobe oder gehashte Merkmale, nie Rohtext eines Erzeugers.
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoarseSubject {
    /// Ausgehender Fluss zu einem IP-Literal: Netz und Zielport.
    EgressNetwork {
        /// Netzadresse (`/24` bzw. `/64` maskiert).
        network: IpAddr,
        /// Präfixlänge der Maske (24 oder 64).
        prefix_len: u8,
        /// Zielport.
        port: u16,
    },
    /// Ausgehender Fluss zu einem Ziel ohne IP-Literal.
    EgressName {
        /// Digest des Ziels.
        destination: ContentDigest,
        /// Zielport.
        port: u16,
    },
    /// Strukturdrift.
    Drift {
        /// Digest aus Sensor-ID und Detailtext (längenpräfixiert).
        object: ContentDigest,
        /// Drift-Klasse des Ereignisses.
        class: DriftSeverity,
    },
    /// Programmausführung.
    Exec {
        /// Digest des ausgeführten Pfads.
        path: ContentDigest,
    },
    /// Dateischreibzugriff.
    FileWrite {
        /// Digest des geschriebenen Pfads.
        path: ContentDigest,
    },
    /// Neuer lauschender Port.
    Listener {
        /// Der Port.
        port: u16,
    },
    /// Anmeldeversuch.
    Auth {
        /// Erfolg oder Fehlschlag.
        outcome: AuthOutcome,
    },
    /// Ein Sensor meldet sich beeinträchtigt.
    SensorDegraded {
        /// Digest der Sensor-ID.
        sensor: ContentDigest,
    },
    /// Keinem einzelnen Ereignis zuordenbar.
    Summary {
        /// Digest der Befundzusammenfassung.
        summary: ContentDigest,
    },
}

impl CoarseSubject {
    /// Das grobe Subjekt des auslösenden Ereignisses.
    ///
    /// # Arguments
    /// - `event` (`&SecurityEvent`): das Ereignis, das den Befund ausgelöst
    ///   hat.
    #[must_use]
    pub fn of_event(event: &SecurityEvent) -> Self {
        match &event.kind {
            EventKind::EgressFlow { destination, port } => egress_subject(destination, *port),
            EventKind::StructureDrift { severity, detail } => {
                let mut object = Vec::new();
                put_field(&mut object, event.sensor.as_str().as_bytes());
                put_field(&mut object, detail.as_bytes());
                Self::Drift {
                    object: ContentDigest::of(&object),
                    class: *severity,
                }
            }
            EventKind::ProcessExec { path, .. } => Self::Exec {
                path: ContentDigest::of(path.as_bytes()),
            },
            EventKind::FileWrite { path } => Self::FileWrite {
                path: ContentDigest::of(path.as_bytes()),
            },
            EventKind::ListenerOpened { port } => Self::Listener { port: *port },
            EventKind::AuthEvent { outcome } => Self::Auth { outcome: *outcome },
            EventKind::SensorDegraded { sensor } => Self::SensorDegraded {
                sensor: ContentDigest::of(sensor.as_str().as_bytes()),
            },
        }
    }

    /// Das Subjekt eines keinem Einzelereignis zuordenbaren Befundes.
    ///
    /// # Arguments
    /// - `summary` (`&str`): die Befundzusammenfassung; geht nur als Digest
    ///   ein.
    #[must_use]
    pub fn of_summary(summary: &str) -> Self {
        Self::Summary {
            summary: ContentDigest::of(summary.as_bytes()),
        }
    }

    /// Hängt die stabile Kodierung dieses Subjekts an `buf` an.
    fn encode_into(&self, buf: &mut Vec<u8>) {
        match self {
            Self::EgressNetwork {
                network,
                prefix_len,
                port,
            } => {
                buf.push(TAG_EGRESS_NETWORK);
                match network {
                    IpAddr::V4(v4) => put_field(buf, &v4.octets()),
                    IpAddr::V6(v6) => put_field(buf, &v6.octets()),
                }
                buf.push(*prefix_len);
                buf.extend_from_slice(&port.to_be_bytes());
            }
            Self::EgressName { destination, port } => {
                buf.push(TAG_EGRESS_NAME);
                put_field(buf, destination.as_bytes());
                buf.extend_from_slice(&port.to_be_bytes());
            }
            Self::Drift { object, class } => {
                buf.push(TAG_DRIFT);
                put_field(buf, object.as_bytes());
                buf.push(drift_class_code(*class));
            }
            Self::Exec { path } => {
                buf.push(TAG_EXEC);
                put_field(buf, path.as_bytes());
            }
            Self::FileWrite { path } => {
                buf.push(TAG_FILE_WRITE);
                put_field(buf, path.as_bytes());
            }
            Self::Listener { port } => {
                buf.push(TAG_LISTENER);
                buf.extend_from_slice(&port.to_be_bytes());
            }
            Self::Auth { outcome } => {
                buf.push(TAG_AUTH);
                buf.push(auth_outcome_code(*outcome));
            }
            Self::SensorDegraded { sensor } => {
                buf.push(TAG_SENSOR_DEGRADED);
                put_field(buf, sensor.as_bytes());
            }
            Self::Summary { summary } => {
                buf.push(TAG_SUMMARY);
                put_field(buf, summary.as_bytes());
            }
        }
    }
}

/// Grobes Subjekt eines ausgehenden Flusses (Netz + Port bzw. Ziel-Digest).
fn egress_subject(destination: &str, port: u16) -> CoarseSubject {
    match destination.parse::<IpAddr>() {
        Ok(addr) => {
            let (network, prefix_len) = coarse_network(addr);
            CoarseSubject::EgressNetwork {
                network,
                prefix_len,
                port,
            }
        }
        Err(_) => CoarseSubject::EgressName {
            destination: ContentDigest::of(destination.as_bytes()),
            port,
        },
    }
}

/// Maskiert `addr` auf `/24` (IPv4, auch IPv4-gemappt) bzw. `/64` (IPv6).
fn coarse_network(addr: IpAddr) -> (IpAddr, u8) {
    let v4 = match addr {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4,
            None => {
                let masked = Ipv6Addr::from(u128::from(v6) & IPV6_COARSE_MASK);
                return (IpAddr::V6(masked), IPV6_COARSE_PREFIX);
            }
        },
    };
    let masked = Ipv4Addr::from(u32::from(v4) & IPV4_COARSE_MASK);
    (IpAddr::V4(masked), IPV4_COARSE_PREFIX)
}

/// Stabiler Code einer Drift-Klasse im Schlüssel-Urbild.
const fn drift_class_code(class: DriftSeverity) -> u8 {
    match class {
        DriftSeverity::Unknown => 0,
        DriftSeverity::Low => 1,
        DriftSeverity::Medium => 2,
        DriftSeverity::High => 3,
    }
}

/// Stabiler Code eines Anmeldeergebnisses im Schlüssel-Urbild.
const fn auth_outcome_code(outcome: AuthOutcome) -> u8 {
    match outcome {
        AuthOutcome::Success => 0,
        AuthOutcome::Failure => 1,
    }
}

/// Stärke einer Härte: höher ist stärker (`Observed` am stärksten).
///
/// Erschöpfendes `match`: die `Ord`-Ordnung von [`Hardness`] läuft
/// umgekehrt (`Observed` ist dort am kleinsten) und wird hier bewusst nicht
/// benutzt.
const fn hardness_strength(hardness: Hardness) -> u8 {
    match hardness {
        Hardness::Observed => 3,
        Hardness::Correlated => 2,
        Hardness::Inferred => 1,
    }
}

/// Die stärkere zweier Härten.
const fn stronger_hardness(a: Hardness, b: Hardness) -> Hardness {
    if hardness_strength(b) > hardness_strength(a) {
        b
    } else {
        a
    }
}

/// Was der Exporter zu einem Befund aus dem Throttle erfährt.
///
/// # Concurrency
/// Reiner Werttyp, `Copy + Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleStamp {
    /// Der Entprell-Schlüssel.
    pub dedup_key: DedupKey,
    /// Wie oft dieser Schlüssel seit `first_seen` aufgetreten ist,
    /// einschließlich dieses Befundes (mindestens 1).
    pub occurrences: u64,
    /// Früheste Ereigniszeit dieses Schlüssels.
    pub first_seen: Timestamp,
}

/// Warum ein Befund gemeldet wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitReason {
    /// Der Schlüssel war unbekannt.
    New,
    /// Die Abklingzeit seit der letzten Meldung ist abgelaufen.
    CooldownElapsed,
    /// Die Härte ist stärker als jede bisher gemeldete.
    HardnessIncreased,
    /// Die Schwere ist höher als jede bisher gemeldete.
    SeverityIncreased,
    /// Die Uhr steht vor der letzten Meldung (Rücksprung); melden ist der
    /// sichere Rückfall.
    ClockWentBack,
}

impl EmitReason {
    /// Stabile Kurzform für Journal und Diagnose.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::CooldownElapsed => "cooldown-elapsed",
            Self::HardnessIncreased => "hardness-increased",
            Self::SeverityIncreased => "severity-increased",
            Self::ClockWentBack => "clock-went-back",
        }
    }
}

/// Melden oder nur zählen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleVerdict {
    /// Melden (Journal, Export), mit Begründung.
    Emit(EmitReason),
    /// Nur gezählt; weder Journal-`warn` noch Export.
    Suppress,
}

/// Ergebnis von [`FindingThrottle::observe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleDecision {
    /// Schlüssel, Anzahl und erste Sichtung für den Exporter.
    pub stamp: ThrottleStamp,
    /// Melden oder unterdrücken.
    pub verdict: ThrottleVerdict,
}

impl ThrottleDecision {
    /// Die Entscheidung ohne Throttle: melden, erste Sichtung = `observed_at`.
    ///
    /// # Arguments
    /// - `dedup_key` (`DedupKey`): der Schlüssel des Befundes.
    /// - `observed_at` (`Timestamp`): Ereigniszeit des Befundes.
    #[must_use]
    pub const fn unthrottled(dedup_key: DedupKey, observed_at: Timestamp) -> Self {
        Self {
            stamp: ThrottleStamp {
                dedup_key,
                occurrences: 1,
                first_seen: observed_at,
            },
            verdict: ThrottleVerdict::Emit(EmitReason::New),
        }
    }

    /// Ob der Befund gemeldet wird.
    #[cfg(test)]
    #[must_use]
    pub const fn emits(&self) -> bool {
        matches!(self.verdict, ThrottleVerdict::Emit(_))
    }
}

/// Gehaltener Zustand eines Schlüssels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    // Auftreten seit `first_seen`, mindestens 1.
    occurrences: u64,
    // Früheste Ereigniszeit.
    first_seen: Timestamp,
    // Späteste Ereigniszeit (LRU-Ordnung beim Laden).
    last_seen: Timestamp,
    // Sentinel-Zeit der letzten Meldung.
    last_emitted: Timestamp,
    // Stärkste bisher gemeldete Härte.
    hardness: Hardness,
    // Höchste bisher gemeldete Schwere.
    severity: Severity,
    // LRU-Marke; Schlüssel in `FindingThrottle::lru`.
    tick: u64,
}

/// Gespeicherter Zustand (Schema [`STATE_SCHEMA_VERSION`]).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateDocument {
    schema_version: u32,
    entries: Vec<StateEntry>,
}

/// Ein gespeicherter Schlüssel.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateEntry {
    dedup_key: String,
    occurrences: u64,
    first_seen: Timestamp,
    last_seen: Timestamp,
    last_emitted: Timestamp,
    hardness: Hardness,
    severity: Severity,
}

/// Entprellt wiederholte Befunde über [`DedupKey`] (siehe Moduldoku).
///
/// # Description
/// Hält höchstens [`ThrottleConfig::max_keys`] Schlüssel mit LRU-Verdrängung
/// und speichert sie optional als JSON (siehe Moduldoku, Abschnitt
/// „Zustand").
///
/// # Concurrency
/// `&mut self` für jede Änderung; lebt im Sammelthread.
#[derive(Debug)]
pub struct FindingThrottle {
    // Abklingzeit als vorzeichenbehaftete Dauer (Vergleich mit
    // `Timestamp::duration_since`).
    cooldown: SignedDuration,
    // Höchstzahl gehaltener Schlüssel.
    max_keys: usize,
    // Zustand je Schlüssel.
    entries: HashMap<DedupKey, Entry>,
    // LRU-Index: Marke → Schlüssel, älteste Marke zuerst.
    lru: BTreeMap<u64, DedupKey>,
    // Nächste LRU-Marke.
    next_tick: u64,
    // Zustandsdatei; `None` hält nur im Speicher.
    state_path: Option<PathBuf>,
    // Seit dem letzten Schreiben geändert.
    dirty: bool,
    // Seit dem letzten Schreiben wurde etwas gemeldet.
    emitted_since_persist: bool,
    // Zeitpunkt des letzten Schreibversuchs.
    last_persist: Option<Timestamp>,
    // Der letzte Schreibversuch scheiterte (begrenzt die Warnungen).
    failing: bool,
}

impl FindingThrottle {
    /// Baut einen Throttle und lädt, falls angegeben, den gespeicherten
    /// Zustand.
    ///
    /// # Arguments
    /// - `config` (`ThrottleConfig`): Abklingzeit und Schlüsselgrenze.
    /// - `state_path` (`Option<PathBuf>`): die Zustandsdatei (im Betrieb
    ///   `<home>/`[`DEFAULT_STATE_FILE_NAME`]); `None` hält nur im Speicher.
    ///
    /// # Returns
    /// Den Throttle; ein fehlender Zustand ergibt einen leeren, ein
    /// unbrauchbarer einen leeren mit einer Warnung (ohne Pfad).
    #[must_use]
    pub fn open(config: ThrottleConfig, state_path: Option<PathBuf>) -> Self {
        let mut throttle = Self {
            cooldown: SignedDuration::try_from(config.cooldown).unwrap_or(SignedDuration::MAX),
            max_keys: config.max_keys,
            entries: HashMap::new(),
            lru: BTreeMap::new(),
            next_tick: 0,
            state_path,
            dirty: false,
            emitted_since_persist: false,
            last_persist: None,
            failing: false,
        };
        let Some(path) = throttle.state_path.clone() else {
            return throttle;
        };
        match read_state(&path) {
            Ok(None) => {
                tracing::debug!("no finding throttle state yet; starting empty");
            }
            Ok(Some(document)) => throttle.restore(document),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "finding throttle state unusable; starting empty"
                );
            }
        }
        throttle
    }

    /// Anzahl gehaltener Schlüssel.
    #[cfg(test)]
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.entries.len()
    }

    /// Verbucht einen Befund und entscheidet, ob er gemeldet wird.
    ///
    /// # Description
    /// Siehe Moduldoku, Abschnitte „Echte Änderung" und „Zeitbasis".
    ///
    /// # Arguments
    /// - `dedup_key` (`DedupKey`): Schlüssel des Befundes.
    /// - `hardness` (`Hardness`), `severity` (`Severity`): die des Befundes.
    /// - `observed_at` (`Timestamp`): Ereigniszeit des Befundes.
    /// - `now` (`Timestamp`): Sentinel-Zeit dieses Zyklus.
    ///
    /// # Returns
    /// Die Entscheidung samt [`ThrottleStamp`].
    pub fn observe(
        &mut self,
        dedup_key: DedupKey,
        hardness: Hardness,
        severity: Severity,
        observed_at: Timestamp,
        now: Timestamp,
    ) -> ThrottleDecision {
        if !self.entries.contains_key(&dedup_key) {
            return self.insert_new(dedup_key, hardness, severity, observed_at, now);
        }
        let tick = self.bump_tick();
        let cooldown = self.cooldown;
        let Some(entry) = self.entries.get_mut(&dedup_key) else {
            // Eben geprüft; melden ist der sichere Rückfall.
            return ThrottleDecision::unthrottled(dedup_key, observed_at);
        };
        let previous_tick = entry.tick;
        entry.tick = tick;
        entry.occurrences = entry.occurrences.saturating_add(1);
        entry.first_seen = entry.first_seen.min(observed_at);
        entry.last_seen = entry.last_seen.max(observed_at);

        let verdict = if hardness_strength(hardness) > hardness_strength(entry.hardness) {
            ThrottleVerdict::Emit(EmitReason::HardnessIncreased)
        } else if severity > entry.severity {
            ThrottleVerdict::Emit(EmitReason::SeverityIncreased)
        } else {
            let elapsed = now.duration_since(entry.last_emitted);
            if elapsed.is_negative() {
                ThrottleVerdict::Emit(EmitReason::ClockWentBack)
            } else if elapsed >= cooldown {
                ThrottleVerdict::Emit(EmitReason::CooldownElapsed)
            } else {
                ThrottleVerdict::Suppress
            }
        };
        let emitted = matches!(verdict, ThrottleVerdict::Emit(_));
        if emitted {
            entry.last_emitted = now;
            entry.hardness = stronger_hardness(entry.hardness, hardness);
            entry.severity = entry.severity.max(severity);
        }
        let stamp = ThrottleStamp {
            dedup_key,
            occurrences: entry.occurrences,
            first_seen: entry.first_seen,
        };

        self.lru.remove(&previous_tick);
        self.lru.insert(tick, dedup_key);
        self.dirty = true;
        if emitted {
            self.emitted_since_persist = true;
        }
        ThrottleDecision { stamp, verdict }
    }

    /// Schreibt den Zustand, wenn er geändert ist und fällig wird.
    ///
    /// # Description
    /// Fällig ist er, wenn seit dem letzten Schreiben etwas gemeldet wurde
    /// oder [`PERSIST_MIN_INTERVAL`] vergangen ist (bzw. die Uhr
    /// zurücksprang). Ohne Zustandsdatei ein No-Op. Ein Fehlschlag wird
    /// geloggt und beim nächsten fälligen Zyklus wiederholt.
    ///
    /// # Arguments
    /// - `now` (`Timestamp`): Sentinel-Zeit dieses Zyklus.
    ///
    /// # Returns
    /// `true`, wenn der Zustand geschrieben wurde.
    pub fn persist_if_due(&mut self, now: Timestamp) -> bool {
        if !self.dirty {
            return false;
        }
        let Some(path) = self.state_path.as_deref() else {
            return false;
        };
        let interval_elapsed = self.last_persist.is_none_or(|last| {
            let since = now.duration_since(last);
            since.is_negative() || since >= PERSIST_MIN_INTERVAL
        });
        if !self.emitted_since_persist && !interval_elapsed {
            return false;
        }
        let result = self.encode().and_then(|bytes| write_state(path, &bytes));
        self.last_persist = Some(now);
        self.emitted_since_persist = false;
        match result {
            Ok(()) => {
                self.dirty = false;
                if self.failing {
                    tracing::info!("finding throttle state writable again");
                }
                self.failing = false;
                true
            }
            Err(error) => {
                if self.failing {
                    tracing::debug!(error = %error, "finding throttle state still not writable");
                } else {
                    tracing::warn!(
                        error = %error,
                        "finding throttle state not writable; repeats may be reported again after a restart"
                    );
                }
                self.failing = true;
                false
            }
        }
    }

    /// Nimmt einen neuen Schlüssel auf (mit LRU-Verdrängung).
    fn insert_new(
        &mut self,
        dedup_key: DedupKey,
        hardness: Hardness,
        severity: Severity,
        observed_at: Timestamp,
        now: Timestamp,
    ) -> ThrottleDecision {
        if self.max_keys == 0 {
            return ThrottleDecision::unthrottled(dedup_key, observed_at);
        }
        while self.entries.len() >= self.max_keys {
            let Some((_, oldest)) = self.lru.pop_first() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        let tick = self.bump_tick();
        self.entries.insert(
            dedup_key,
            Entry {
                occurrences: 1,
                first_seen: observed_at,
                last_seen: observed_at,
                last_emitted: now,
                hardness,
                severity,
                tick,
            },
        );
        self.lru.insert(tick, dedup_key);
        self.dirty = true;
        self.emitted_since_persist = true;
        ThrottleDecision::unthrottled(dedup_key, observed_at)
    }

    /// Vergibt die nächste LRU-Marke.
    fn bump_tick(&mut self) -> u64 {
        let tick = self.next_tick;
        self.next_tick = self.next_tick.saturating_add(1);
        tick
    }

    /// Übernimmt einen gelesenen Zustand: gültige Einträge, die neuesten
    /// höchstens `max_keys`, in LRU-Reihenfolge nach `last_seen`.
    fn restore(&mut self, document: StateDocument) {
        if document.schema_version != STATE_SCHEMA_VERSION {
            tracing::warn!(
                schema_version = document.schema_version,
                expected = STATE_SCHEMA_VERSION,
                "finding throttle state has an unknown schema version; starting empty"
            );
            return;
        }
        let mut by_key: HashMap<DedupKey, Entry> = HashMap::new();
        let mut rejected = 0_u64;
        for stored in document.entries {
            let Some(key) = DedupKey::parse_hex(&stored.dedup_key) else {
                rejected = rejected.saturating_add(1);
                continue;
            };
            by_key.insert(
                key,
                Entry {
                    occurrences: stored.occurrences.max(1),
                    first_seen: stored.first_seen,
                    last_seen: stored.last_seen,
                    last_emitted: stored.last_emitted,
                    hardness: stored.hardness,
                    severity: stored.severity,
                    tick: 0,
                },
            );
        }
        let mut ordered: Vec<(DedupKey, Entry)> = by_key.into_iter().collect();
        ordered.sort_by(|(key_a, a), (key_b, b)| {
            a.last_seen.cmp(&b.last_seen).then_with(|| key_a.cmp(key_b))
        });
        let skip = ordered.len().saturating_sub(self.max_keys);
        for (key, mut entry) in ordered.into_iter().skip(skip) {
            let tick = self.bump_tick();
            entry.tick = tick;
            self.entries.insert(key, entry);
            self.lru.insert(tick, key);
        }
        tracing::info!(
            keys = self.entries.len(),
            dropped = skip,
            rejected,
            "finding throttle state restored"
        );
    }

    /// Kodiert den Zustand (LRU-Reihenfolge, älteste zuerst).
    fn encode(&self) -> io::Result<Vec<u8>> {
        let entries = self
            .lru
            .values()
            .filter_map(|key| self.entries.get(key).map(|entry| (key, entry)))
            .map(|(key, entry)| StateEntry {
                dedup_key: key.to_string(),
                occurrences: entry.occurrences,
                first_seen: entry.first_seen,
                last_seen: entry.last_seen,
                last_emitted: entry.last_emitted,
                hardness: entry.hardness,
                severity: entry.severity,
            })
            .collect();
        let document = StateDocument {
            schema_version: STATE_SCHEMA_VERSION,
            entries,
        };
        let mut bytes = serde_json::to_vec(&document).map_err(io::Error::other)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

/// Liest den gespeicherten Zustand.
///
/// # Returns
/// `Ok(None)`, wenn es die Datei nicht gibt.
///
/// # Errors
/// Kein reguläres File (etwa ein Symlink), größer als [`MAX_STATE_BYTES`],
/// I/O- oder JSON-Fehler.
fn read_state(path: &Path) -> io::Result<Option<StateDocument>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !meta.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "finding throttle state is not a regular file",
        ));
    }
    if meta.len() > MAX_STATE_BYTES {
        return Err(state_too_large());
    }
    let mut text = String::new();
    File::open(path)?
        .take(MAX_STATE_BYTES.saturating_add(1))
        .read_to_string(&mut text)?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_STATE_BYTES {
        return Err(state_too_large());
    }
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Fehler für eine übergroße Zustandsdatei.
fn state_too_large() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "finding throttle state exceeds its size limit",
    )
}

/// Ersetzt die Zustandsdatei atomar (Modus [`STATE_FILE_MODE`]).
fn write_state(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = health_export::temporary_path(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "finding throttle state path has no file name",
        )
    })?;
    health_export::write_atomically(path, &temporary, bytes, STATE_FILE_MODE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::SensorId;

    fn at(secs: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(secs).map_err(ctx("timestamp"))
    }

    fn egress(destination: &str, port: u16) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("probe-bpf-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::EgressFlow {
                destination: destination.to_owned(),
                port,
            },
        }
    }

    fn key_for(destination: &str, port: u16, uid: Option<u32>) -> DedupKey {
        DedupKey::derive(
            "egress-flow",
            uid,
            None,
            &CoarseSubject::of_event(&egress(destination, port)),
        )
    }

    fn throttle(cooldown_secs: u64, max_keys: usize) -> FindingThrottle {
        FindingThrottle::open(
            ThrottleConfig {
                cooldown: Duration::from_secs(cooldown_secs),
                max_keys,
            },
            None,
        )
    }

    #[test]
    fn test_dedup_key_is_32_lowercase_hex_and_round_trips() -> TestResult {
        let key = key_for("198.51.100.5", 443, Some(1000));
        let text = key.to_string();
        assert_eq!(text.len(), 32);
        assert!(
            text.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let parsed = DedupKey::parse_hex(&text).ok_or(TestError::Missing("parsed key"))?;
        assert_eq!(parsed, key);
        let upper = text.to_uppercase();
        if upper != text {
            assert_eq!(DedupKey::parse_hex(&upper), None, "upper case is refused");
        }
        let prefix = text.get(..31).ok_or(TestError::Missing("31 hex chars"))?;
        assert_eq!(
            DedupKey::parse_hex(&format!("{prefix}g")),
            None,
            "a non-hex character is refused"
        );
        assert_eq!(DedupKey::parse_hex("abc"), None);
        Ok(())
    }

    #[test]
    fn test_egress_key_groups_a_slash_24_and_separates_ports_and_uids() {
        let base = key_for("198.51.100.5", 443, Some(1000));
        assert_eq!(
            base,
            key_for("198.51.100.77", 443, Some(1000)),
            "same /24 and port"
        );
        assert_ne!(base, key_for("198.51.101.5", 443, Some(1000)), "other /24");
        assert_ne!(base, key_for("198.51.100.5", 8443, Some(1000)), "new port");
        assert_ne!(base, key_for("198.51.100.5", 443, Some(1001)), "other uid");
        assert_ne!(base, key_for("198.51.100.5", 443, None), "unknown uid");
        assert_eq!(
            base,
            key_for("::ffff:198.51.100.9", 443, Some(1000)),
            "an IPv4-mapped address is its IPv4 network"
        );
    }

    #[test]
    fn test_egress_key_groups_a_slash_64() {
        assert_eq!(
            key_for("2001:db8:1:2::1", 443, None),
            key_for("2001:db8:1:2:ffff::9", 443, None)
        );
        assert_ne!(
            key_for("2001:db8:1:2::1", 443, None),
            key_for("2001:db8:1:3::1", 443, None)
        );
    }

    #[test]
    fn test_cgroup_kid_and_rule_are_part_of_the_key() {
        let subject = CoarseSubject::of_event(&egress("198.51.100.5", 443));
        let base = DedupKey::derive("egress-flow", Some(1), Some(42), &subject);
        assert_ne!(
            base,
            DedupKey::derive("egress-flow", Some(1), Some(43), &subject)
        );
        assert_ne!(
            base,
            DedupKey::derive("egress-flow", Some(1), None, &subject)
        );
        assert_ne!(
            base,
            DedupKey::derive("listener-new", Some(1), Some(42), &subject)
        );
    }

    #[test]
    fn test_drift_subject_is_a_digest_never_raw_text() {
        let secret = "drift in /home/alice/.ssh/id_ed25519";
        let event = SecurityEvent {
            sensor: SensorId::from_str("scanreport-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::StructureDrift {
                severity: DriftSeverity::High,
                detail: secret.to_owned(),
            },
        };
        let subject = CoarseSubject::of_event(&event);
        let rendered = format!("{subject:?}");
        assert!(!rendered.contains("alice"), "{rendered}");
        let other_class = SecurityEvent {
            kind: EventKind::StructureDrift {
                severity: DriftSeverity::Low,
                detail: secret.to_owned(),
            },
            ..event.clone()
        };
        assert_ne!(subject, CoarseSubject::of_event(&other_class));
        assert_eq!(subject, CoarseSubject::of_event(&event));
    }

    #[test]
    fn test_repeats_within_the_cooldown_only_count() -> TestResult {
        let mut throttle = throttle(600, MAX_KEYS);
        let key = key_for("198.51.100.5", 443, Some(1000));
        let first = throttle.observe(key, Hardness::Observed, Severity::High, at(100)?, at(100)?);
        assert_eq!(first.verdict, ThrottleVerdict::Emit(EmitReason::New));
        assert_eq!(first.stamp.occurrences, 1);
        assert_eq!(first.stamp.first_seen, at(100)?);

        for second in [105, 110, 699] {
            let repeat = throttle.observe(
                key,
                Hardness::Observed,
                Severity::High,
                at(second)?,
                at(second)?,
            );
            assert_eq!(repeat.verdict, ThrottleVerdict::Suppress, "at {second}");
        }
        let after = throttle.observe(key, Hardness::Observed, Severity::High, at(700)?, at(700)?);
        assert_eq!(
            after.verdict,
            ThrottleVerdict::Emit(EmitReason::CooldownElapsed)
        );
        assert_eq!(after.stamp.occurrences, 5);
        assert_eq!(after.stamp.first_seen, at(100)?);
        assert_eq!(after.stamp.dedup_key, key);
        Ok(())
    }

    #[test]
    fn test_hardness_and_severity_increase_bypass_the_cooldown() -> TestResult {
        let mut throttle = throttle(600, MAX_KEYS);
        let key = key_for("198.51.100.5", 443, None);
        let now = at(1_000)?;
        throttle.observe(key, Hardness::Inferred, Severity::Medium, now, now);
        let weaker = throttle.observe(key, Hardness::Inferred, Severity::Low, now, now);
        assert_eq!(weaker.verdict, ThrottleVerdict::Suppress);
        let harder = throttle.observe(key, Hardness::Observed, Severity::Medium, now, now);
        assert_eq!(
            harder.verdict,
            ThrottleVerdict::Emit(EmitReason::HardnessIncreased)
        );
        let again = throttle.observe(key, Hardness::Correlated, Severity::Medium, now, now);
        assert_eq!(
            again.verdict,
            ThrottleVerdict::Suppress,
            "weaker than the strongest reported"
        );
        let worse = throttle.observe(key, Hardness::Observed, Severity::Critical, now, now);
        assert_eq!(
            worse.verdict,
            ThrottleVerdict::Emit(EmitReason::SeverityIncreased)
        );
        assert_eq!(worse.stamp.occurrences, 5);
        Ok(())
    }

    #[test]
    fn test_first_seen_is_the_earliest_event_time_and_a_clock_jump_back_emits() -> TestResult {
        let mut throttle = throttle(600, MAX_KEYS);
        let key = key_for("198.51.100.5", 443, None);
        throttle.observe(key, Hardness::Observed, Severity::High, at(500)?, at(500)?);
        let earlier = throttle.observe(key, Hardness::Observed, Severity::High, at(400)?, at(510)?);
        assert_eq!(earlier.stamp.first_seen, at(400)?);
        assert_eq!(earlier.verdict, ThrottleVerdict::Suppress);
        let back = throttle.observe(key, Hardness::Observed, Severity::High, at(10)?, at(20)?);
        assert_eq!(
            back.verdict,
            ThrottleVerdict::Emit(EmitReason::ClockWentBack)
        );
        Ok(())
    }

    #[test]
    fn test_zero_cooldown_reports_every_repeat_but_keeps_counting() -> TestResult {
        let mut throttle = throttle(0, MAX_KEYS);
        let key = key_for("198.51.100.5", 443, None);
        let now = at(1)?;
        throttle.observe(key, Hardness::Observed, Severity::High, now, now);
        let repeat = throttle.observe(key, Hardness::Observed, Severity::High, now, now);
        assert!(repeat.emits());
        assert_eq!(repeat.stamp.occurrences, 2);
        Ok(())
    }

    #[test]
    fn test_key_count_is_bounded_with_lru_eviction() -> TestResult {
        let mut throttle = throttle(600, 3);
        let now = at(1)?;
        let keys: Vec<DedupKey> = (1..=4)
            .map(|port| key_for("198.51.100.5", port, None))
            .collect();
        for key in keys.iter().take(3) {
            throttle.observe(*key, Hardness::Observed, Severity::High, now, now);
        }
        // Schlüssel 1 frisch berühren: Schlüssel 2 ist nun der älteste.
        let first = *keys.first().ok_or(TestError::Missing("key 1"))?;
        throttle.observe(first, Hardness::Observed, Severity::High, now, now);
        let fourth = *keys.get(3).ok_or(TestError::Missing("key 4"))?;
        throttle.observe(fourth, Hardness::Observed, Severity::High, now, now);
        assert_eq!(throttle.key_count(), 3);

        let second = *keys.get(1).ok_or(TestError::Missing("key 2"))?;
        let evicted = throttle.observe(second, Hardness::Observed, Severity::High, now, now);
        assert_eq!(
            evicted.verdict,
            ThrottleVerdict::Emit(EmitReason::New),
            "the least recently used key was evicted"
        );
        let kept = throttle.observe(first, Hardness::Observed, Severity::High, now, now);
        assert_eq!(kept.verdict, ThrottleVerdict::Suppress);
        assert_eq!(throttle.key_count(), 3);
        Ok(())
    }

    #[test]
    fn test_zero_max_keys_reports_everything() -> TestResult {
        let mut throttle = throttle(600, 0);
        let key = key_for("198.51.100.5", 443, None);
        let now = at(1)?;
        for _ in 0..3 {
            assert!(
                throttle
                    .observe(key, Hardness::Observed, Severity::High, now, now)
                    .emits()
            );
        }
        assert_eq!(throttle.key_count(), 0);
        Ok(())
    }

    #[test]
    fn test_state_survives_a_restart_and_keeps_suppressing() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join(DEFAULT_STATE_FILE_NAME);
        let key = key_for("198.51.100.5", 443, Some(1000));
        {
            let mut throttle = FindingThrottle::open(ThrottleConfig::default(), Some(path.clone()));
            throttle.observe(key, Hardness::Observed, Severity::High, at(100)?, at(100)?);
            throttle.observe(key, Hardness::Observed, Severity::High, at(101)?, at(101)?);
            assert!(throttle.persist_if_due(at(101)?), "an emission persists");
            assert!(
                !throttle.persist_if_due(at(102)?),
                "nothing changed since the last write"
            );
        }
        let text = fs::read_to_string(&path).map_err(ctx("read state"))?;
        assert!(text.contains(&key.to_string()));
        assert!(!text.contains("198.51"), "no raw destination in the state");

        let mut restarted = FindingThrottle::open(ThrottleConfig::default(), Some(path.clone()));
        assert_eq!(restarted.key_count(), 1);
        let repeat = restarted.observe(key, Hardness::Observed, Severity::High, at(200)?, at(200)?);
        assert_eq!(repeat.verdict, ThrottleVerdict::Suppress);
        assert_eq!(repeat.stamp.occurrences, 3);
        assert_eq!(repeat.stamp.first_seen, at(100)?);
        Ok(())
    }

    #[test]
    fn test_suppressed_only_cycles_persist_at_most_once_per_interval() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join(DEFAULT_STATE_FILE_NAME);
        let key = key_for("198.51.100.5", 443, None);
        let mut throttle = FindingThrottle::open(ThrottleConfig::default(), Some(path));
        throttle.observe(key, Hardness::Observed, Severity::High, at(100)?, at(100)?);
        assert!(throttle.persist_if_due(at(100)?));
        throttle.observe(key, Hardness::Observed, Severity::High, at(105)?, at(105)?);
        assert!(!throttle.persist_if_due(at(105)?), "only a count changed");
        assert!(throttle.persist_if_due(at(160)?), "the interval elapsed");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_state_file_is_private_and_a_symlink_is_refused() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join(DEFAULT_STATE_FILE_NAME);
        let key = key_for("198.51.100.5", 443, None);
        let mut throttle = FindingThrottle::open(ThrottleConfig::default(), Some(path.clone()));
        throttle.observe(key, Hardness::Observed, Severity::High, at(1)?, at(1)?);
        assert!(throttle.persist_if_due(at(1)?));
        let mode = fs::metadata(&path)
            .map_err(ctx("metadata"))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode & !STATE_FILE_MODE, 0, "mode {mode:o} exceeds 0600");

        let victim = dir.path().join("victim");
        fs::write(&victim, b"keep").map_err(ctx("seed victim"))?;
        let linked = dir.path().join("linked.json");
        std::os::unix::fs::symlink(&victim, &linked).map_err(ctx("symlink"))?;
        let mut refused = FindingThrottle::open(ThrottleConfig::default(), Some(linked));
        assert_eq!(refused.key_count(), 0, "a symlinked state is not read");
        refused.observe(key, Hardness::Observed, Severity::High, at(1)?, at(1)?);
        assert!(!refused.persist_if_due(at(1)?), "and not written through");
        assert_eq!(fs::read(&victim).map_err(ctx("read victim"))?, b"keep");
        Ok(())
    }

    #[test]
    fn test_garbage_or_foreign_state_starts_empty() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join(DEFAULT_STATE_FILE_NAME);
        fs::write(&path, b"{not json").map_err(ctx("write garbage"))?;
        assert_eq!(
            FindingThrottle::open(ThrottleConfig::default(), Some(path.clone())).key_count(),
            0
        );
        fs::write(&path, br#"{"schema_version":99,"entries":[]}"#).map_err(ctx("write foreign"))?;
        assert_eq!(
            FindingThrottle::open(ThrottleConfig::default(), Some(path.clone())).key_count(),
            0
        );
        let bad_key = format!(
            r#"{{"schema_version":1,"entries":[{{"dedup_key":"XYZ","occurrences":1,"first_seen":"{t}","last_seen":"{t}","last_emitted":"{t}","hardness":"observed","severity":"high"}}]}}"#,
            t = at(1)?
        );
        fs::write(&path, bad_key.as_bytes()).map_err(ctx("write bad key"))?;
        assert_eq!(
            FindingThrottle::open(ThrottleConfig::default(), Some(path)).key_count(),
            0,
            "an invalid key is skipped"
        );
        Ok(())
    }

    #[test]
    fn test_restore_keeps_the_newest_keys_within_the_bound() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join(DEFAULT_STATE_FILE_NAME);
        let mut wide = FindingThrottle::open(
            ThrottleConfig {
                cooldown: Duration::from_secs(DEFAULT_COOLDOWN_SECS),
                max_keys: 8,
            },
            Some(path.clone()),
        );
        for port in 1..=4_u16 {
            let seen = at(i64::from(port))?;
            wide.observe(
                key_for("198.51.100.5", port, None),
                Hardness::Observed,
                Severity::High,
                seen,
                seen,
            );
        }
        assert!(wide.persist_if_due(at(5)?));

        let mut narrow = FindingThrottle::open(
            ThrottleConfig {
                cooldown: Duration::from_secs(DEFAULT_COOLDOWN_SECS),
                max_keys: 2,
            },
            Some(path),
        );
        assert_eq!(narrow.key_count(), 2);
        let newest = narrow.observe(
            key_for("198.51.100.5", 4, None),
            Hardness::Observed,
            Severity::High,
            at(6)?,
            at(6)?,
        );
        assert_eq!(newest.verdict, ThrottleVerdict::Suppress, "kept");
        let oldest = narrow.observe(
            key_for("198.51.100.5", 1, None),
            Hardness::Observed,
            Severity::High,
            at(6)?,
            at(6)?,
        );
        assert_eq!(
            oldest.verdict,
            ThrottleVerdict::Emit(EmitReason::New),
            "dropped"
        );
        Ok(())
    }

    #[test]
    fn test_emit_reason_labels_are_stable() {
        assert_eq!(EmitReason::New.label(), "new");
        assert_eq!(EmitReason::CooldownElapsed.label(), "cooldown-elapsed");
        assert_eq!(EmitReason::HardnessIncreased.label(), "hardness-increased");
        assert_eq!(EmitReason::SeverityIncreased.label(), "severity-increased");
        assert_eq!(EmitReason::ClockWentBack.label(), "clock-went-back");
    }
}
