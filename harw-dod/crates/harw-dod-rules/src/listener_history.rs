//! Persistierter Listener-Zustand für [`crate::rules::ListenerNewRule`]
//! (PL-100 WP1.6).
//!
//! # Verantwortungsbereich
//! [`ListenerHistory`] merkt sich, welche Listener ([`ListenerKey`]) auf
//! diesem Host schon beobachtet wurden. Die Regel `listener-new` **liest**
//! sie nur ([`ListenerHistory::is_primed`], [`ListenerHistory::contains`]);
//! **gelernt** wird ausschließlich über [`ListenerHistory::observe`], das
//! der Sentinel nach jedem Auswertungszyklus mit den Ereignissen dieses
//! Zyklus aufruft. So bleibt jede Regel eine reine Funktion (siehe
//! [`crate::rule`]-Moduldoku), und der Zustand liegt beim Aufrufer, der ihn
//! auch lädt und speichert ([`ListenerHistory::from_json`],
//! [`ListenerHistory::to_json`]).
//!
//! # Erster Start
//! Eine frische Historie ([`ListenerHistory::new`]) ist **nicht vorbereitet**
//! (`is_primed() == false`): ohne Vorwissen wäre beim ersten Start jeder
//! längst laufende Dienst ein „neuer" Listener. Solange die Historie nicht
//! vorbereitet ist, meldet `listener-new` nichts. Der **erste** Aufruf von
//! [`ListenerHistory::observe`] ist der Lernzyklus: er übernimmt alle
//! Listener dieses Zyklus und markiert die Historie als vorbereitet (auch
//! wenn der Zyklus keinen Listener enthielt — ein Host ohne Listener soll
//! seinen ersten Listener melden, nicht still lernen). Der Zeitpunkt wird
//! mitgespeichert ([`ListenerHistory::primed_at`]) und überlebt so einen
//! Neustart.
//!
//! # Schlüssel
//! Ein [`ListenerKey`] ist (Besitzer, Protokoll, Port, Bind-Klasse). Der
//! Besitzer ist die rohe Kernel-cgroup-ID, falls der Akteur eine trägt
//! (`harw_dod_signals::Actor::raw_cgroup_id`), sonst seine UID, und
//! [`ListenerOwner::Unknown`], wenn das Ereignis keinen Akteur hat. Letzteres
//! ist der Regelfall für Listener fremder Benutzer: der unprivilegierte
//! Listener-Sensor darf deren `/proc/<pid>/fd` nicht lesen und meldet sie ohne
//! Akteur. Solche Ereignisse zu verwerfen, machte die Regel genau für
//! root-Dienste und fremde Konten blind; sie zählen deshalb mit unbekanntem
//! Besitzer.
//!
//! # Flüchtige Ports
//! Der Listener-Sensor meldet auch nicht verbundene UDP-Sockets, und die
//! liegen oft auf einem zufälligen flüchtigen Port (mDNS-, WebRTC- und
//! NTP-Clients, Testserver mit `bind(…, 0)`). Für einen **als UDP bekannten**
//! Listener mit einem Port im flüchtigen Bereich der
//! [`crate::rule::RuleConfig`] trägt der Schlüssel deshalb nur
//! [`ListenerPort::Ephemeral`] statt des Ports — ein grober Schlüssel je
//! (Besitzer, Bind-Klasse). Ein TCP-Listener ruft `listen()` bewusst auf und
//! behält seinen genauen Port; bekannte und registrierte Ports außerhalb des
//! Bereichs ebenso.
//!
//! Ein Listener mit **unbekanntem** Protokoll (heute: jeder, siehe unten)
//! behält ebenfalls seinen genauen Port. Ein grober Schlüssel machte die
//! Regel sonst nach dem ersten zufälligen Port eines Besitzers blind für
//! **jeden** weiteren Listener dieses Besitzers im ganzen flüchtigen Bereich —
//! und weil der unprivilegierte Sensor fremde Listener ohne Besitzer meldet
//! ([`ListenerOwner::Unknown`]), hostweit, auch für eine TCP-Hintertür auf
//! Port 50050 (Bedrohung B19, „keine stille Blindheit"). Den Lärm genauer
//! flüchtiger Ports dämpft stattdessen die Regel (`Anomaly`/`Low`, siehe
//! [`crate::rules::ListenerNewRule`]). Die Historie bleibt dabei begrenzt und
//! umgewälzt wird nur, was nicht mehr offen ist: der Sensor meldet jeden
//! offenen Listener in jedem Zyklus erneut, sodass die LRU-Verdrängung zuerst
//! die längst geschlossenen zufälligen Ports trifft.
//!
//! Lernen und Auswerten müssen **dieselbe** Konfiguration verwenden
//! ([`ListenerHistory::observe_with_config`] mit der `rule_config` des
//! Kontexts), sonst passen gelernte und geprüfte Schlüssel nicht zusammen.
//!
//! **Bekannte Grenze:** `EventKind::ListenerOpened` trägt heute nur den
//! Port. Protokoll und Bind-Adresse erfasst der Sensor (noch) nicht; bis er
//! es tut, ist beides [`ListenerProtocol::Unknown`] bzw.
//! [`BindClass::Unknown`]. Sobald das Ereignis die Felder trägt, liefert
//! [`ListenerKey::from_event`] andere Schlüssel — dann steigt
//! [`ListenerHistory::FORMAT_VERSION`], damit eine alte Historie abgelehnt
//! und neu gelernt wird, statt jeden bekannten Listener einmal als neu zu
//! melden.
//!
//! # Grenze und Verdrängung
//! Höchstens [`ListenerHistory::MAX_KEYS`] Schlüssel. Wird die Grenze
//! überschritten, fallen die am längsten nicht mehr gesehenen Schlüssel
//! heraus (LRU nach `last_seen`; bei gleichem `last_seen` der weiter vorne
//! stehende Eintrag — deterministisch). `last_seen` ist die Ereigniszeit,
//! höchstens aber das `now` des Aufrufs. Vor dem Lernen kappt
//! [`ListenerHistory::observe`] außerdem jedes **vorhandene** `last_seen` auf
//! `now`: weder ein in die Zukunft datiertes Ereignis noch eine geladene
//! Datei mit Zukunftsstempeln noch ein Zurückstellen der Uhr kann einen
//! Eintrag so unverdrängbar machen und stattdessen den gerade gemeldeten
//! Listener verdrängen (der dann in jedem Zyklus erneut „neu" wäre).
//!
//! # Aufwand
//! Ein Hash-Index über die Schlüssel macht [`ListenerHistory::contains`] und
//! das Lernen eines Ereignisses O(1). Auch während eines einzigen
//! [`ListenerHistory::observe`]-Aufrufs wächst die Historie höchstens auf
//! das Doppelte von [`ListenerHistory::MAX_KEYS`], bevor verdrängt wird: ein
//! Zyklus mit zehntausenden Listenern (ein unprivilegierter Prozess mit
//! vielen Sockets) kostet so linear viel Zeit und beschränkten Speicher
//! (Leitprinzip I10, Bedrohung B10).
//!
//! # Format
//! JSON, Version [`ListenerHistory::FORMAT_VERSION`]:
//! `{"version":1,"primed_at":<Zeit|null>,"entries":[{"key":{...},"last_seen":<Zeit>}]}`.
//! Unbekannte oder **fehlende** Felder (auch `primed_at`: eine abgeschnittene
//! Datei darf nicht still als „nicht vorbereitet" gelten), eine fremde
//! Version, mehr als [`ListenerHistory::MAX_KEYS`] Einträge und doppelte
//! Schlüssel werden abgelehnt — sowohl von [`ListenerHistory::from_json`] als
//! auch von der `Deserialize`-Implementierung. [`ListenerHistory::from_json`]
//! lehnt außerdem eine Eingabe über [`ListenerHistory::MAX_JSON_BYTES`] ab,
//! **bevor** es sie parst: eine aufgeblähte Datei wird nie vollständig in
//! den Speicher gelesen. Wer die `Deserialize`-Implementierung direkt nutzt,
//! begrenzt die Eingabe selbst.
//!
//! # Nebenläufigkeit
//! Reiner Wert ohne innere Veränderlichkeit, `Send + Sync`. Lernen braucht
//! `&mut self`; Regeln bekommen nur `&ListenerHistory` über
//! [`crate::rule::RuleContext::listener_history`].
//!
//! # Fehler
//! [`ListenerHistoryError`] beim Lesen und Schreiben des JSON-Formats.
//! Inhaltsfrei — in `Display` **und** `Debug` und ohne Fehlerquelle: ein
//! JSON-Fehler trägt nur Kategorie, Zeile und Spalte, nie den Text des
//! `serde_json`-Fehlers (der Dateiinhalt zitiert). Ein Lesefehler darf beim
//! Aufrufer nie still in einen neuen Lernzyklus münden (siehe
//! [`ListenerHistoryError`]). [`ListenerHistory::observe`],
//! [`ListenerHistory::contains`] und [`ListenerHistory::is_primed`] sind
//! total.
//!
//! # Examples
//! ```rust
//! use harw_dod_rules::{ListenerHistory, ListenerKey};
//! use harw_dod_signals::{Actor, EventKind, SecurityEvent};
//! use harw_types::SensorId;
//!
//! # fn main() -> Result<(), harw_dod_rules::ListenerHistoryError> {
//! let event = SecurityEvent {
//!     sensor: SensorId::from_str("listener-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: Some(Actor { uid: 1000, auid: None, cgroup: None, process: None }),
//!     kind: EventKind::ListenerOpened { port: 8080 },
//! };
//! let mut history = ListenerHistory::new();
//! assert!(!history.is_primed());
//!
//! // Der Sentinel lernt nach dem Zyklus.
//! history.observe(std::slice::from_ref(&event), jiff::Timestamp::UNIX_EPOCH);
//! assert!(history.is_primed());
//! let key = ListenerKey::from_event(&event, None);
//! assert!(key.is_some_and(|key| history.contains(&key)));
//!
//! // Persistieren und wieder laden.
//! let bytes = history.to_json()?;
//! assert_eq!(ListenerHistory::from_json(&bytes)?, history);
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fmt;

use harw_dod_signals::{EventKind, SecurityEvent};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::rule::RuleConfig;

/// Wem ein Listener gehört.
///
/// # Description
/// Rohe Kernel-cgroup-ID, falls bekannt, sonst UID, sonst unbekannt — siehe
/// Moduldoku, Abschnitt „Schlüssel". Wire-Form extern getaggt in kebab-case
/// (`{"cgroup-kid":42}`, `{"uid":1000}`, `"unknown"`).
///
/// # Examples
/// ```rust
/// use harw_dod_rules::ListenerOwner;
///
/// assert_ne!(ListenerOwner::Uid(1000), ListenerOwner::CgroupKid(1000));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListenerOwner {
    /// Rohe Kernel-cgroup-ID (cgroup v2: Inode des cgroup-Verzeichnisses).
    CgroupKid(u64),
    /// Reale UID des besitzenden Prozesses.
    Uid(u32),
    /// Der Sensor konnte keinen Besitzer auflösen.
    Unknown,
}

/// Transportprotokoll eines Listeners.
///
/// # Description
/// Heute immer [`Self::Unknown`], weil `EventKind::ListenerOpened` das
/// Protokoll nicht trägt (siehe Moduldoku, „Bekannte Grenze").
///
/// # Examples
/// ```rust
/// use harw_dod_rules::ListenerProtocol;
///
/// assert_eq!(ListenerProtocol::Tcp.as_str(), "tcp");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListenerProtocol {
    /// TCP (`/proc/net/tcp`, `/proc/net/tcp6`).
    Tcp,
    /// UDP (`/proc/net/udp`, `/proc/net/udp6`).
    Udp,
    /// Vom Ereignis nicht geliefert.
    Unknown,
}

impl ListenerProtocol {
    /// Feste, kleingeschriebene Kennung für Zusammenfassungen.
    ///
    /// # Returns
    /// `"tcp"`, `"udp"` oder `"unknown"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
            Self::Unknown => "unknown",
        }
    }
}

/// Auf welche lokale Adresse ein Listener gebunden ist, grob klassifiziert.
///
/// # Description
/// `Loopback` (nur vom Host erreichbar), `Any` (`0.0.0.0`/`::`, alle
/// Schnittstellen), `Specific` (eine bestimmte, nicht-Loopback-Adresse).
/// Heute immer [`Self::Unknown`] (siehe Moduldoku, „Bekannte Grenze").
///
/// # Examples
/// ```rust
/// use harw_dod_rules::BindClass;
///
/// assert_eq!(BindClass::Loopback.as_str(), "loopback");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BindClass {
    /// Loopback-Adresse (`127.0.0.0/8`, `::1`).
    Loopback,
    /// Platzhalteradresse: alle Schnittstellen.
    Any,
    /// Eine bestimmte Adresse außerhalb von Loopback.
    Specific,
    /// Vom Ereignis nicht geliefert.
    Unknown,
}

impl BindClass {
    /// Feste, kleingeschriebene Kennung für Zusammenfassungen.
    ///
    /// # Returns
    /// `"loopback"`, `"any"`, `"specific"` oder `"unknown"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Any => "any",
            Self::Specific => "specific",
            Self::Unknown => "unknown",
        }
    }
}

/// Der Port-Teil eines [`ListenerKey`].
///
/// # Description
/// `Exact` trägt den genauen lokalen Port, `Ephemeral` steht für **jeden**
/// Port im flüchtigen Bereich der [`RuleConfig`] — nur für einen als UDP
/// bekannten Listener, siehe Moduldoku, Abschnitt „Flüchtige Ports".
/// Wire-Form extern getaggt in kebab-case (`{"exact":8080}`, `"ephemeral"`).
///
/// # Examples
/// ```rust
/// use harw_dod_rules::ListenerPort;
///
/// assert_ne!(ListenerPort::Exact(40_000), ListenerPort::Ephemeral);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListenerPort {
    /// Ein genauer lokaler Port.
    Exact(u16),
    /// Irgendein Port im flüchtigen Bereich (grober Schlüssel).
    Ephemeral,
}

/// Identität eines Listeners: (Besitzer, Protokoll, Port, Bind-Klasse).
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Schlüssel". Ein reiner Wert ohne Invariante;
/// trägt keinen angreiferkontrollierten Text.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::{BindClass, ListenerKey, ListenerOwner, ListenerPort, ListenerProtocol};
///
/// let key = ListenerKey {
///     owner: ListenerOwner::Uid(1000),
///     protocol: ListenerProtocol::Unknown,
///     port: ListenerPort::Exact(8080),
///     bind: BindClass::Unknown,
/// };
/// assert_eq!(key.port, ListenerPort::Exact(8080));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenerKey {
    /// Wem der Listener gehört.
    pub owner: ListenerOwner,
    /// Transportprotokoll.
    pub protocol: ListenerProtocol,
    /// Lokaler Port, oder der grobe Schlüssel für flüchtige Ports.
    pub port: ListenerPort,
    /// Grobe Klasse der gebundenen Adresse.
    pub bind: BindClass,
}

impl ListenerKey {
    /// Leitet den Schlüssel eines `ListenerOpened`-Ereignisses ab.
    ///
    /// # Arguments
    /// - `event` (`&SecurityEvent`): ein beliebiges Ereignis.
    /// - `config` (`Option<&RuleConfig>`): liefert den flüchtigen
    ///   Portbereich; `None` heißt [`RuleConfig::default`].
    ///
    /// # Returns
    /// `Some` für `EventKind::ListenerOpened`, sonst `None`. Der Besitzer ist
    /// die rohe cgroup-ID des Akteurs, sonst seine UID, ohne Akteur
    /// [`ListenerOwner::Unknown`]; Protokoll und Bind-Klasse sind
    /// `Unknown` (siehe Moduldoku). Ein Port im flüchtigen Bereich wird nur
    /// für ein als UDP bekanntes Protokoll zu [`ListenerPort::Ephemeral`] —
    /// heute also nie: der genaue Port bleibt erhalten.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_rules::{ListenerKey, ListenerOwner, ListenerPort};
    /// use harw_dod_signals::{Actor, EventKind, SecurityEvent};
    /// use harw_types::SensorId;
    ///
    /// let listener = |port| SecurityEvent {
    ///     sensor: SensorId::from_str("listener-0"),
    ///     observed_at: jiff::Timestamp::UNIX_EPOCH,
    ///     actor: Some(Actor { uid: 0, auid: None, cgroup: Actor::raw_cgroup(77), process: None }),
    ///     kind: EventKind::ListenerOpened { port },
    /// };
    /// let key = ListenerKey::from_event(&listener(22), None);
    /// assert_eq!(key.map(|key| key.owner), Some(ListenerOwner::CgroupKid(77)));
    /// assert_eq!(key.map(|key| key.port), Some(ListenerPort::Exact(22)));
    /// // Unbekanntes Protokoll: auch ein flüchtiger Port bleibt genau.
    /// let random = ListenerKey::from_event(&listener(50_050), None);
    /// assert_eq!(random.map(|key| key.port), Some(ListenerPort::Exact(50_050)));
    /// ```
    #[must_use]
    pub fn from_event(event: &SecurityEvent, config: Option<&RuleConfig>) -> Option<Self> {
        let EventKind::ListenerOpened { port } = &event.kind else {
            return None;
        };
        let owner = event
            .actor
            .as_ref()
            .map_or(ListenerOwner::Unknown, |actor| {
                actor
                    .raw_cgroup_id()
                    .map_or(ListenerOwner::Uid(actor.uid), ListenerOwner::CgroupKid)
            });
        // Heute liefert das Ereignis weder Protokoll noch Bind-Adresse.
        let protocol = ListenerProtocol::Unknown;
        let bind = BindClass::Unknown;
        Some(Self {
            owner,
            protocol,
            port: port_part(*port, protocol, config),
            bind,
        })
    }
}

// Der Port-Teil eines Schlüssels: grob nur für einen flüchtigen Port eines
// als UDP **bekannten** Listeners. TCP und ein unbekanntes Protokoll behalten
// den genauen Port (siehe Moduldoku, „Flüchtige Ports").
fn port_part(port: u16, protocol: ListenerProtocol, config: Option<&RuleConfig>) -> ListenerPort {
    if protocol == ListenerProtocol::Udp && is_ephemeral_port(port, config) {
        ListenerPort::Ephemeral
    } else {
        ListenerPort::Exact(port)
    }
}

/// Ob `port` im flüchtigen Bereich der Konfiguration liegt.
///
/// # Arguments
/// - `port` (`u16`): ein lokaler Port.
/// - `config` (`Option<&RuleConfig>`): `None` heißt
///   [`RuleConfig::DEFAULT_EPHEMERAL_PORTS`].
///
/// # Returns
/// `true` im flüchtigen Bereich (Grenzen inklusiv).
pub(crate) fn is_ephemeral_port(port: u16, config: Option<&RuleConfig>) -> bool {
    match config {
        Some(config) => config.is_ephemeral_port(port),
        None => RuleConfig::DEFAULT_EPHEMERAL_PORTS.contains(&port),
    }
}

// Ein gelernter Schlüssel samt Zeitpunkt seiner letzten Beobachtung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListenerEntry {
    // Der gelernte Listener.
    key: ListenerKey,
    // Letzte Beobachtung (Ereigniszeit, höchstens `now` des Aufrufs).
    last_seen: Timestamp,
}

/// Die auf diesem Host bereits gesehenen Listener — Zustand der Regel
/// `listener-new`.
///
/// # Description
/// Siehe Moduldoku: Lernen nur über [`Self::observe`], höchstens
/// [`Self::MAX_KEYS`] Schlüssel mit LRU-Verdrängung, JSON-Persistenz über
/// [`Self::from_json`]/[`Self::to_json`], O(1) je Ereignis über einen
/// Hash-Index.
///
/// # Errors
/// Nur beim Lesen/Schreiben, siehe [`ListenerHistoryError`].
///
/// # Concurrency
/// Reiner Wert, `Send + Sync`.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ListenerHistoryWire")]
pub struct ListenerHistory {
    // Formatversion; immer `FORMAT_VERSION`.
    version: u32,
    // Zeitpunkt des Lernzyklus; `None` heißt „nicht vorbereitet".
    primed_at: Option<Timestamp>,
    // Gelernte Schlüssel, nach einem Aufruf höchstens `MAX_KEYS`, ohne
    // Duplikate. Die Reihenfolge ist der Gleichstandsbrecher der Verdrängung.
    entries: Vec<ListenerEntry>,
    // Schlüssel → Position in `entries`; immer deckungsgleich mit
    // `entries` und deshalb nicht serialisiert (wird beim Lesen neu
    // aufgebaut).
    #[serde(skip)]
    index: HashMap<ListenerKey, usize>,
}

// Ungeprüfte Wire-Form; wird nur über `TryFrom` (Prüfung von Version,
// Grenze und Eindeutigkeit) zu einer `ListenerHistory`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListenerHistoryWire {
    // Siehe `ListenerHistory::version`.
    version: u32,
    // Siehe `ListenerHistory::primed_at`. Pflichtfeld (`null` erlaubt): ein
    // `Option`-Feld machte serde bei fehlendem Schlüssel still zu `None`, und
    // eine abgeschnittene Datei lernte im nächsten Zyklus jeden gerade offenen
    // Listener ungeprüft. Der Wire-Typ ist deshalb kein `Option`, sondern
    // [`PrimedAtWire`]; ein fehlender Schlüssel ist ein Fehler.
    primed_at: PrimedAtWire,
    // Siehe `ListenerHistory::entries`.
    entries: Vec<ListenerEntry>,
}

// Wire-Form von `primed_at`: ein Zeitstempel oder ausdrücklich `null`.
// Ungetaggt, damit das Dateiformat (`to_json` schreibt `Option<Timestamp>`)
// gleich bleibt; anders als ein `Option`-Feld wird ein fehlender Schlüssel
// nicht zu `None`, sondern zu „missing field“.
#[derive(Deserialize)]
#[serde(untagged)]
enum PrimedAtWire {
    // Gelernt seit diesem Zeitpunkt.
    At(Timestamp),
    // `null`: noch nicht geprimt.
    Unprimed(()),
}

impl PrimedAtWire {
    // Zurück in die Form von `ListenerHistory::primed_at`.
    fn into_option(self) -> Option<Timestamp> {
        match self {
            Self::At(at) => Some(at),
            Self::Unprimed(()) => None,
        }
    }
}

impl TryFrom<ListenerHistoryWire> for ListenerHistory {
    type Error = ListenerHistoryError;

    fn try_from(wire: ListenerHistoryWire) -> Result<Self, Self::Error> {
        if wire.version != Self::FORMAT_VERSION {
            return Err(ListenerHistoryError::UnsupportedVersion);
        }
        if wire.entries.len() > Self::MAX_KEYS {
            return Err(ListenerHistoryError::TooManyEntries);
        }
        let mut index = HashMap::with_capacity(wire.entries.len());
        for (position, entry) in wire.entries.iter().enumerate() {
            if index.insert(entry.key, position).is_some() {
                return Err(ListenerHistoryError::DuplicateKey);
            }
        }
        Ok(Self {
            version: wire.version,
            primed_at: wire.primed_at.into_option(),
            entries: wire.entries,
            index,
        })
    }
}

impl Default for ListenerHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl ListenerHistory {
    /// Aktuelle Formatversion des JSON-Formats (siehe Moduldoku).
    pub const FORMAT_VERSION: u32 = 1;

    /// Höchstzahl gemerkter Schlüssel.
    pub const MAX_KEYS: usize = 4096;

    /// Höchstgröße der Eingabe von [`Self::from_json`] in Bytes (1 MiB).
    ///
    /// # Description
    /// Ein Eintrag mit den breitesten Werten (20-stellige cgroup-ID,
    /// Zeitstempel mit Nanosekunden) braucht kompakt geschrieben gut
    /// 160 Bytes; [`Self::MAX_KEYS`] solcher Einträge (rund 0,7 MiB) passen mit
    /// Spielraum für Leerraum einer von Hand bearbeiteten Datei hinein.
    pub const MAX_JSON_BYTES: usize = 1_048_576;

    // Länge, ab der `observe_with_config` schon **während** eines Aufrufs
    // verdrängt: hält auch einen Zyklus mit sehr vielen neuen Schlüsseln bei
    // beschränktem Speicher (siehe Moduldoku, „Aufwand").
    const EVICTION_TRIGGER: usize = Self::MAX_KEYS.saturating_mul(2);

    /// Eine leere, nicht vorbereitete Historie (erster Start).
    ///
    /// # Returns
    /// Eine Historie ohne Schlüssel mit `is_primed() == false`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_rules::ListenerHistory;
    ///
    /// assert!(ListenerHistory::new().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: Self::FORMAT_VERSION,
            primed_at: None,
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Liest eine persistierte Historie.
    ///
    /// # Arguments
    /// - `bytes` (`&[u8]`): JSON im Format aus der Moduldoku.
    ///
    /// # Returns
    /// Die geprüfte Historie.
    ///
    /// # Errors
    /// - [`ListenerHistoryError::TooLarge`]: mehr als
    ///   [`Self::MAX_JSON_BYTES`] Bytes (geprüft vor dem Parsen).
    /// - [`ListenerHistoryError::Json`]: kein gültiges JSON dieses Formats
    ///   (auch: unbekanntes oder fehlendes Feld, ungültiger Zeitstempel).
    /// - [`ListenerHistoryError::UnsupportedVersion`]: fremde Formatversion.
    /// - [`ListenerHistoryError::TooManyEntries`]: mehr als
    ///   [`Self::MAX_KEYS`] Einträge.
    /// - [`ListenerHistoryError::DuplicateKey`]: ein Schlüssel kommt doppelt vor.
    ///
    /// # Examples
    /// Siehe Moduldoku.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ListenerHistoryError> {
        if bytes.len() > Self::MAX_JSON_BYTES {
            return Err(ListenerHistoryError::TooLarge);
        }
        let wire: ListenerHistoryWire =
            serde_json::from_slice(bytes).map_err(|error| ListenerHistoryError::json(&error))?;
        Self::try_from(wire)
    }

    /// Schreibt diese Historie als JSON.
    ///
    /// # Returns
    /// Die JSON-Bytes im Format aus der Moduldoku; höchstens
    /// [`Self::MAX_JSON_BYTES`] Bytes, weil die Historie höchstens
    /// [`Self::MAX_KEYS`] Einträge trägt.
    ///
    /// # Errors
    /// [`ListenerHistoryError::Json`], falls die Serialisierung scheitert
    /// (mit den Typen dieses Moduls nicht zu erwarten).
    ///
    /// # Examples
    /// Siehe Moduldoku.
    pub fn to_json(&self) -> Result<Vec<u8>, ListenerHistoryError> {
        serde_json::to_vec(self).map_err(|error| ListenerHistoryError::json(&error))
    }

    /// Lernt die Listener der Ereignisse eines Zyklus mit der
    /// Vorgabe-Konfiguration.
    ///
    /// # Description
    /// Dasselbe wie [`Self::observe_with_config`] mit `config = None`
    /// (flüchtiger Portbereich [`RuleConfig::DEFAULT_EPHEMERAL_PORTS`]). Passt
    /// zu einem [`crate::rule::RuleContext`] ohne `rule_config` oder mit
    /// Vorgabe-Bereich.
    ///
    /// # Arguments
    /// - `events` (`&[SecurityEvent]`): die Ereignisse des gerade
    ///   ausgewerteten Zyklus; andere Ereignisarten werden übergangen.
    /// - `now` (`jiff::Timestamp`): injizierte Zeit dieses Zyklus.
    ///
    /// # Concurrency
    /// `&mut self`: der Aufrufer serialisiert Lernen und Auswerten.
    ///
    /// # Examples
    /// Siehe Moduldoku.
    pub fn observe(&mut self, events: &[SecurityEvent], now: Timestamp) {
        self.observe_with_config(events, now, None);
    }

    /// Lernt die Listener der Ereignisse eines Zyklus.
    ///
    /// # Description
    /// Zuerst wird jedes vorhandene `last_seen` auf `now` gekappt (siehe
    /// Moduldoku, „Grenze und Verdrängung"). Dann wird jedes
    /// `ListenerOpened`-Ereignis über [`ListenerKey::from_event`] (mit
    /// `config`) zu einem Schlüssel; ein bekannter Schlüssel bekommt ein
    /// neueres `last_seen`, ein unbekannter wird angehängt (beides O(1) über
    /// den Index). `last_seen` ist die Ereigniszeit, höchstens `now`. Der
    /// erste Aufruf markiert die Historie als vorbereitet (siehe Moduldoku,
    /// „Erster Start"). Über [`Self::MAX_KEYS`] hinausgehende Schlüssel werden
    /// nach LRU verdrängt — am Ende des Aufrufs und, bei sehr vielen neuen
    /// Schlüsseln, schon zwischendurch (siehe Moduldoku, „Aufwand").
    ///
    /// # Arguments
    /// - `events` (`&[SecurityEvent]`): die Ereignisse des gerade
    ///   ausgewerteten Zyklus; andere Ereignisarten werden übergangen.
    /// - `now` (`jiff::Timestamp`): injizierte Zeit dieses Zyklus.
    /// - `config` (`Option<&RuleConfig>`): **dieselbe** Konfiguration wie
    ///   `rule_config` im [`crate::rule::RuleContext`] des Zyklus; sonst
    ///   passen gelernte und geprüfte Schlüssel flüchtiger Ports nicht
    ///   zusammen.
    ///
    /// # Concurrency
    /// `&mut self`: der Aufrufer serialisiert Lernen und Auswerten.
    ///
    /// # Examples
    /// Siehe Moduldoku.
    pub fn observe_with_config(
        &mut self,
        events: &[SecurityEvent],
        now: Timestamp,
        config: Option<&RuleConfig>,
    ) {
        for entry in &mut self.entries {
            entry.last_seen = entry.last_seen.min(now);
        }
        for event in events {
            let Some(key) = ListenerKey::from_event(event, config) else {
                continue;
            };
            let seen = event.observed_at.min(now);
            match self.index.get(&key).copied() {
                Some(position) => {
                    // Der Index zeigt immer auf einen vorhandenen Eintrag.
                    if let Some(entry) = self.entries.get_mut(position) {
                        entry.last_seen = entry.last_seen.max(seen);
                    }
                }
                None => {
                    self.index.insert(key, self.entries.len());
                    self.entries.push(ListenerEntry {
                        key,
                        last_seen: seen,
                    });
                    if self.entries.len() >= Self::EVICTION_TRIGGER {
                        self.evict_to_bound();
                    }
                }
            }
        }
        if self.primed_at.is_none() {
            self.primed_at = Some(now);
        }
        self.evict_to_bound();
    }

    /// Ob der Lernzyklus stattgefunden hat.
    ///
    /// # Returns
    /// `false` vor dem ersten [`Self::observe`] — dann meldet
    /// `listener-new` nichts (siehe Moduldoku).
    #[must_use]
    pub fn is_primed(&self) -> bool {
        self.primed_at.is_some()
    }

    /// Zeitpunkt des Lernzyklus.
    ///
    /// # Returns
    /// Das `now` des ersten [`Self::observe`]-Aufrufs, oder `None`.
    #[must_use]
    pub fn primed_at(&self) -> Option<Timestamp> {
        self.primed_at
    }

    /// Ob `key` bereits gelernt ist.
    ///
    /// # Arguments
    /// - `key` (`&ListenerKey`): der gesuchte Schlüssel.
    ///
    /// # Returns
    /// `true`, wenn `key` gemerkt ist. O(1) über den Index.
    #[must_use]
    pub fn contains(&self, key: &ListenerKey) -> bool {
        self.index.contains_key(key)
    }

    /// Zahl gemerkter Schlüssel.
    ///
    /// # Returns
    /// Höchstens [`Self::MAX_KEYS`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Ob kein Schlüssel gemerkt ist.
    ///
    /// # Returns
    /// `true` ohne Schlüssel (unabhängig von [`Self::is_primed`]).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // Verdrängt die am längsten nicht gesehenen Schlüssel, bis höchstens
    // `MAX_KEYS` übrig sind, und baut den Index neu auf. Stabile Sortierung:
    // bei gleichem `last_seen` fällt der weiter vorne stehende Eintrag
    // zuerst. O(n log n) für n ≤ `EVICTION_TRIGGER`.
    fn evict_to_bound(&mut self) {
        let excess = self.entries.len().saturating_sub(Self::MAX_KEYS);
        if excess == 0 {
            return;
        }
        self.entries.sort_by_key(|entry| entry.last_seen);
        self.entries = std::mem::take(&mut self.entries)
            .into_iter()
            .skip(excess)
            .collect();
        self.index = self
            .entries
            .iter()
            .enumerate()
            .map(|(position, entry)| (entry.key, position))
            .collect();
    }
}

/// Eine persistierte [`ListenerHistory`] ist unlesbar oder unzulässig.
///
/// # Description
/// Inhaltsfrei — in `Display`, in `Debug` und ohne Fehlerquelle: keine
/// Meldung interpoliert einen Wert aus der Datei. Ein JSON-Fehler trägt
/// deshalb nicht den `serde_json::Error` (dessen Meldung Dateiinhalt
/// zitiert, etwa `invalid type: string "…"`), sondern nur dessen Kategorie,
/// Zeile und Spalte.
///
/// Der Aufrufer (Sentinel) kann nach einem Lesefehler nur mit
/// [`ListenerHistory::new`] neu beginnen — und der folgende Lernzyklus
/// übernimmt **jeden** in diesem Moment offenen Listener ungeprüft, auch eine
/// gerade geöffnete Hintertür. Er darf das deshalb nie still tun, sondern
/// muss den Verlust sichtbar machen (etwa als `SensorDegraded`-Ereignis, das
/// [`crate::rules::SensorDegradedRule`] meldet, oder als Health-Hinweis).
///
/// # Errors
/// Von [`ListenerHistory::from_json`], [`ListenerHistory::to_json`] und der
/// `Deserialize`-Implementierung erzeugt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerHistoryError {
    /// Die Eingabe von [`ListenerHistory::from_json`] ist größer als
    /// [`ListenerHistory::MAX_JSON_BYTES`]; sie wurde nicht geparst.
    TooLarge,
    /// Kein gültiges JSON dieses Formats, oder die Serialisierung scheiterte.
    Json {
        /// Kategorie des `serde_json`-Fehlers (Syntax, Daten, Dateiende, I/O).
        category: serde_json::error::Category,
        /// Einsbasierte Zeile, an der der Fehler erkannt wurde.
        line: usize,
        /// Einsbasierte Spalte, an der der Fehler erkannt wurde.
        column: usize,
    },
    /// Die Formatversion ist nicht [`ListenerHistory::FORMAT_VERSION`].
    UnsupportedVersion,
    /// Mehr als [`ListenerHistory::MAX_KEYS`] Einträge.
    TooManyEntries,
    /// Ein Schlüssel kommt mehrfach vor.
    DuplicateKey,
}

impl ListenerHistoryError {
    // Übernimmt aus einem `serde_json`-Fehler nur die inhaltsfreien Teile.
    fn json(error: &serde_json::Error) -> Self {
        Self::Json {
            category: error.classify(),
            line: error.line(),
            column: error.column(),
        }
    }
}

// Feste Kennung einer `serde_json`-Fehlerkategorie.
fn category_label(category: serde_json::error::Category) -> &'static str {
    match category {
        serde_json::error::Category::Io => "i/o",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "unexpected end of input",
    }
}

impl fmt::Display for ListenerHistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("listener history exceeds its size bound"),
            Self::Json {
                category,
                line,
                column,
            } => write!(
                f,
                "listener history is not valid json of the expected format \
                 ({} error at line {line}, column {column})",
                category_label(*category)
            ),
            Self::UnsupportedVersion => {
                f.write_str("listener history has an unsupported format version")
            }
            Self::TooManyEntries => f.write_str("listener history exceeds its entry bound"),
            Self::DuplicateKey => f.write_str("listener history contains a duplicate key"),
        }
    }
}

// Keine Fehlerquelle: der `serde_json::Error` wird bewusst nicht
// aufbewahrt (siehe Typdoku).
impl std::error::Error for ListenerHistoryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx, missing};
    use harw_dod_signals::{Actor, AuthOutcome};
    use harw_types::SensorId;

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("Testzeitpunkt"))
    }

    fn uid_actor(uid: u32) -> Option<Actor> {
        Some(Actor {
            uid,
            auid: None,
            cgroup: None,
            process: None,
        })
    }

    fn listener_event(port: u16, actor: Option<Actor>, observed_at: Timestamp) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str("listener-0"),
            observed_at,
            actor,
            kind: EventKind::ListenerOpened { port },
        }
    }

    fn key(owner: ListenerOwner, port: u16) -> ListenerKey {
        ListenerKey {
            owner,
            protocol: ListenerProtocol::Unknown,
            port: ListenerPort::Exact(port),
            bind: BindClass::Unknown,
        }
    }

    fn ephemeral_key(owner: ListenerOwner) -> ListenerKey {
        ListenerKey {
            owner,
            protocol: ListenerProtocol::Unknown,
            port: ListenerPort::Ephemeral,
            bind: BindClass::Unknown,
        }
    }

    // Eine Historie mit beliebigem (auch unzulässigem) Inhalt, um die
    // Prüfung beim Lesen zu testen.
    fn raw_history(version: u32, entries: Vec<ListenerEntry>) -> ListenerHistory {
        ListenerHistory {
            version,
            primed_at: Some(Timestamp::UNIX_EPOCH),
            entries,
            // Nicht serialisiert; diese Historie wird nur geschrieben.
            index: HashMap::new(),
        }
    }

    // Prüft, dass der Index genau die Einträge abbildet.
    fn assert_index_consistent(history: &ListenerHistory) {
        assert_eq!(history.index.len(), history.entries.len());
        for (position, entry) in history.entries.iter().enumerate() {
            assert_eq!(history.index.get(&entry.key), Some(&position));
        }
    }

    #[test]
    fn test_new_history_is_empty_and_not_primed() {
        let history = ListenerHistory::new();
        assert!(history.is_empty());
        assert!(!history.is_primed());
        assert_eq!(history.primed_at(), None);
        assert_eq!(ListenerHistory::default(), history);
    }

    #[test]
    fn test_from_event_prefers_raw_cgroup_then_uid_then_unknown() -> TestResult {
        let t = at(1)?;
        let with_kid = listener_event(
            22,
            Some(Actor {
                uid: 0,
                auid: None,
                cgroup: Actor::raw_cgroup(4242),
                process: None,
            }),
            t,
        );
        // Ein aufgelöster Pfad ist keine Kernel-ID: Rückfall auf die UID.
        let with_path = listener_event(
            22,
            Some(Actor {
                uid: 1000,
                auid: None,
                cgroup: Some(harw_types::CgroupId::from_str(
                    "/user.slice/user-1000.slice",
                )),
                process: None,
            }),
            t,
        );
        let without_actor = listener_event(22, None, t);

        assert_eq!(
            ListenerKey::from_event(&with_kid, None),
            Some(key(ListenerOwner::CgroupKid(4242), 22))
        );
        assert_eq!(
            ListenerKey::from_event(&with_path, None),
            Some(key(ListenerOwner::Uid(1000), 22))
        );
        assert_eq!(
            ListenerKey::from_event(&without_actor, None),
            Some(key(ListenerOwner::Unknown, 22))
        );
        Ok(())
    }

    #[test]
    fn test_from_event_ignores_other_event_kinds() {
        let event = SecurityEvent {
            sensor: SensorId::from_str("authlog-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: uid_actor(1000),
            kind: EventKind::AuthEvent {
                outcome: AuthOutcome::Failure,
            },
        };
        assert_eq!(ListenerKey::from_event(&event, None), None);
    }

    /// Review-Befund (Runde 2, hoch): der grobe Schlüssel galt für jedes
    /// nicht als TCP bekannte Protokoll, also heute für jeden Listener — und
    /// machte die Regel nach dem ersten zufälligen Port hostweit blind für
    /// den ganzen flüchtigen Bereich. Jetzt bleibt der Port bei unbekanntem
    /// Protokoll genau.
    #[test]
    fn test_from_event_keeps_exact_ports_while_the_protocol_is_unknown() -> TestResult {
        let t = at(1)?;
        for port in [
            22_u16, 443, 8080, 32_767, 32_768, 41_234, 50_050, 60_999, 65_535,
        ] {
            assert_eq!(
                ListenerKey::from_event(&listener_event(port, uid_actor(1000), t), None),
                Some(key(ListenerOwner::Uid(1000), port)),
                "{port}"
            );
        }
        let config = RuleConfig::new()
            .with_ephemeral_ports(40_000, 50_000)
            .map_err(ctx("Portbereich"))?;
        assert_eq!(
            ListenerKey::from_event(&listener_event(45_000, None, t), Some(&config))
                .map(|key| key.port),
            Some(ListenerPort::Exact(45_000))
        );
        Ok(())
    }

    /// Der grobe Schlüssel gilt nur für ein als UDP bekanntes Protokoll im
    /// flüchtigen Bereich; TCP und ein unbekanntes Protokoll bleiben genau.
    #[test]
    fn test_port_part_is_coarse_only_for_udp_in_the_ephemeral_range() -> TestResult {
        for port in [32_768_u16, 41_234, 60_999] {
            assert_eq!(
                port_part(port, ListenerProtocol::Udp, None),
                ListenerPort::Ephemeral,
                "{port}"
            );
            assert_eq!(
                port_part(port, ListenerProtocol::Tcp, None),
                ListenerPort::Exact(port),
                "{port}"
            );
            assert_eq!(
                port_part(port, ListenerProtocol::Unknown, None),
                ListenerPort::Exact(port),
                "{port}"
            );
        }
        for port in [53_u16, 32_767, 61_000] {
            assert_eq!(
                port_part(port, ListenerProtocol::Udp, None),
                ListenerPort::Exact(port),
                "{port}"
            );
        }
        // Ein abweichender Bereich aus der Konfiguration.
        let config = RuleConfig::new()
            .with_ephemeral_ports(40_000, 50_000)
            .map_err(ctx("Portbereich"))?;
        assert_eq!(
            port_part(45_000, ListenerProtocol::Udp, Some(&config)),
            ListenerPort::Ephemeral
        );
        assert_eq!(
            port_part(35_000, ListenerProtocol::Udp, Some(&config)),
            ListenerPort::Exact(35_000)
        );
        assert!(is_ephemeral_port(45_000, Some(&config)));
        assert!(!is_ephemeral_port(35_000, Some(&config)));
        assert!(is_ephemeral_port(35_000, None));
        Ok(())
    }

    /// Regressionstest zum Befund: ein gelernter flüchtiger Port eines
    /// Besitzers (auch ein gelernter grober UDP-Schlüssel) macht einen
    /// anderen Port desselben Besitzers im Bereich nicht bekannt.
    #[test]
    fn test_learned_ephemeral_port_does_not_cover_other_ports_in_the_range() -> TestResult {
        let mut history = ListenerHistory::new();
        history.observe(&[listener_event(41_234, None, at(1)?)], at(1)?);
        assert!(history.contains(&key(ListenerOwner::Unknown, 41_234)));
        assert!(!history.contains(&key(ListenerOwner::Unknown, 50_050)));

        // Ein gelernter grober UDP-Schlüssel deckt keinen Listener mit
        // unbekanntem Protokoll ab.
        let udp = ListenerKey {
            owner: ListenerOwner::Unknown,
            protocol: ListenerProtocol::Udp,
            port: ListenerPort::Ephemeral,
            bind: BindClass::Unknown,
        };
        let bytes = raw_history(
            ListenerHistory::FORMAT_VERSION,
            vec![ListenerEntry {
                key: udp,
                last_seen: Timestamp::UNIX_EPOCH,
            }],
        )
        .to_json()
        .map_err(ctx("Historie serialisiert"))?;
        let mut with_udp = ListenerHistory::from_json(&bytes).map_err(ctx("Historie gelesen"))?;
        assert!(with_udp.contains(&udp));
        with_udp.observe(&[listener_event(50_050, None, at(2)?)], at(2)?);
        assert!(with_udp.contains(&key(ListenerOwner::Unknown, 50_050)));
        assert_eq!(with_udp.len(), 2);
        Ok(())
    }

    /// Zufällige flüchtige Ports, die wieder geschlossen werden, verdrängen
    /// keine offenen Listener: der Sensor meldet die offenen in jedem Zyklus
    /// erneut, die geschlossenen fallen per LRU zuerst heraus.
    #[test]
    fn test_closed_random_ports_are_evicted_before_open_listeners() -> TestResult {
        let mut history = ListenerHistory::new();
        // Ein offener Dienst plus genug zufällige Ports, um die Grenze zu
        // überschreiten; der Dienst wird in jedem Zyklus erneut gemeldet.
        let mut second = 1_i64;
        // 4201 zufällige Ports: 32_768 bis 36_968.
        for port in 32_768_u16..=36_968 {
            let now = at(second)?;
            history.observe(
                &[
                    listener_event(22, None, now),
                    listener_event(port, None, now),
                ],
                now,
            );
            second = second.saturating_add(1);
        }
        assert_eq!(history.len(), ListenerHistory::MAX_KEYS);
        assert!(history.contains(&key(ListenerOwner::Unknown, 22)));
        assert!(!history.contains(&key(ListenerOwner::Unknown, 32_768)));
        assert!(history.contains(&key(ListenerOwner::Unknown, 36_968)));
        assert_index_consistent(&history);
        Ok(())
    }

    /// Review-Befund (Runde 2, mittel): Lernen und `contains` waren linear
    /// je Ereignis, also O(N²) für N neue Schlüssel, und die Historie wuchs
    /// innerhalb eines Aufrufs unbegrenzt. 50 000 verschiedene Schlüssel in
    /// einem Zyklus: die Grenze hält, die jüngsten Schlüssel bleiben, der
    /// Index bleibt deckungsgleich.
    #[test]
    fn test_one_cycle_with_fifty_thousand_keys_stays_bounded_and_lru() -> TestResult {
        let mut events = Vec::with_capacity(50_000);
        for port in 1..=50_000_u16 {
            events.push(listener_event(port, None, at(i64::from(port))?));
        }
        let mut history = ListenerHistory::new();
        history.observe(&events, at(60_000)?);

        assert_eq!(history.len(), ListenerHistory::MAX_KEYS);
        assert_index_consistent(&history);
        // Die 4096 jüngsten Schlüssel: 45_905 bis 50_000.
        assert!(!history.contains(&key(ListenerOwner::Unknown, 45_904)));
        assert!(!history.contains(&key(ListenerOwner::Unknown, 1)));
        assert!(history.contains(&key(ListenerOwner::Unknown, 45_905)));
        assert!(history.contains(&key(ListenerOwner::Unknown, 50_000)));
        Ok(())
    }

    /// Nach dem Lesen arbeitet der Index wie nach dem Lernen.
    #[test]
    fn test_index_is_rebuilt_on_read() -> TestResult {
        let mut history = ListenerHistory::new();
        history.observe(
            &[
                listener_event(22, None, at(1)?),
                listener_event(8080, uid_actor(1000), at(2)?),
            ],
            at(3)?,
        );
        let bytes = history.to_json().map_err(ctx("Historie serialisiert"))?;
        let mut back = ListenerHistory::from_json(&bytes).map_err(ctx("Historie gelesen"))?;
        assert_index_consistent(&back);
        assert!(back.contains(&key(ListenerOwner::Uid(1000), 8080)));

        // Erneutes Lernen eines bekannten Schlüssels hängt nichts an.
        back.observe(&[listener_event(8080, uid_actor(1000), at(4)?)], at(4)?);
        assert_eq!(back.len(), 2);
        assert_index_consistent(&back);
        Ok(())
    }

    #[test]
    fn test_first_observe_primes_and_learns_every_listener() -> TestResult {
        let now = at(100)?;
        let events = vec![
            listener_event(22, None, at(90)?),
            listener_event(8080, uid_actor(1000), at(95)?),
        ];
        let mut history = ListenerHistory::new();

        history.observe(&events, now);

        assert!(history.is_primed());
        assert_eq!(history.primed_at(), Some(now));
        assert_eq!(history.len(), 2);
        assert!(history.contains(&key(ListenerOwner::Unknown, 22)));
        assert!(history.contains(&key(ListenerOwner::Uid(1000), 8080)));
        assert!(!history.contains(&key(ListenerOwner::Uid(1001), 8080)));
        Ok(())
    }

    /// Ein Lernzyklus ohne Listener bereitet die Historie trotzdem vor: der
    /// erste Listener eines Hosts ohne Listener wird gemeldet, nicht still
    /// gelernt.
    #[test]
    fn test_first_observe_without_listeners_still_primes() -> TestResult {
        let mut history = ListenerHistory::new();
        let first = at(10)?;
        history.observe(&[], first);
        assert!(history.is_primed());
        assert!(history.is_empty());

        // Spätere Aufrufe verschieben den Lernzeitpunkt nicht.
        history.observe(&[listener_event(22, None, at(20)?)], at(20)?);
        assert_eq!(history.primed_at(), Some(first));
        Ok(())
    }

    #[test]
    fn test_observe_keeps_the_latest_last_seen() -> TestResult {
        let mut history = ListenerHistory::new();
        history.observe(&[listener_event(22, None, at(10)?)], at(10)?);
        // Ein älteres Ereignis desselben Schlüssels verjüngt nicht.
        history.observe(&[listener_event(22, None, at(5)?)], at(20)?);
        let seen = history.entries.first().map(|entry| entry.last_seen);
        assert_eq!(seen, Some(at(10)?));

        history.observe(&[listener_event(22, None, at(15)?)], at(20)?);
        let seen = history.entries.first().map(|entry| entry.last_seen);
        assert_eq!(seen, Some(at(15)?));
        assert_eq!(history.len(), 1);
        Ok(())
    }

    /// Ein in die Zukunft datiertes Ereignis wird auf `now` begrenzt und kann
    /// sich so nicht vor der Verdrängung schützen.
    #[test]
    fn test_observe_clamps_future_event_time_to_now() -> TestResult {
        let mut history = ListenerHistory::new();
        history.observe(&[listener_event(22, None, at(1_000)?)], at(50)?);
        let seen = history.entries.first().map(|entry| entry.last_seen);
        assert_eq!(seen, Some(at(50)?));
        Ok(())
    }

    /// Review-Befund: vorhandene Einträge mit Zukunftsstempel (geladene
    /// Datei, zurückgestellte Uhr) verdrängten bei voller Historie den gerade
    /// gemeldeten Listener, der dann in jedem Zyklus erneut neu war.
    #[test]
    fn test_observe_caps_existing_future_last_seen_so_the_new_key_survives() -> TestResult {
        let future = at(1_000_000)?;
        let mut events = Vec::with_capacity(ListenerHistory::MAX_KEYS);
        for port in 1..=4096_u16 {
            events.push(listener_event(port, None, future));
        }
        // Lernen bei einer „Zukunfts"-Uhr: alle Einträge tragen `future`.
        let mut history = ListenerHistory::new();
        history.observe(&events, future);
        assert_eq!(history.len(), ListenerHistory::MAX_KEYS);

        // Uhr zurückgestellt: ein neuer Listener bei `now` < `future`.
        let now = at(100)?;
        history.observe(&[listener_event(5_000, None, now)], now);

        assert_eq!(history.len(), ListenerHistory::MAX_KEYS);
        assert!(
            history.contains(&key(ListenerOwner::Unknown, 5_000)),
            "der gerade gemeldete Listener bleibt"
        );
        assert!(
            history.entries.iter().all(|entry| entry.last_seen <= now),
            "kein Eintrag bleibt in der Zukunft"
        );
        Ok(())
    }

    /// Dasselbe für eine geladene Datei mit Zukunftsstempeln.
    #[test]
    fn test_loaded_future_timestamps_are_capped_on_observe() -> TestResult {
        let far_future = at(4_000_000_000)?;
        let entries: Vec<ListenerEntry> = (1..=4096_u16)
            .map(|port| ListenerEntry {
                key: key(ListenerOwner::Unknown, port),
                last_seen: far_future,
            })
            .collect();
        let bytes = raw_history(ListenerHistory::FORMAT_VERSION, entries)
            .to_json()
            .map_err(ctx("Historie serialisiert"))?;
        let mut history = ListenerHistory::from_json(&bytes).map_err(ctx("Historie gelesen"))?;

        let now = at(10)?;
        history.observe(&[listener_event(9_999, None, now)], now);
        assert!(history.contains(&key(ListenerOwner::Unknown, 9_999)));
        assert_eq!(history.len(), ListenerHistory::MAX_KEYS);
        Ok(())
    }

    #[test]
    fn test_history_is_bounded_and_evicts_the_least_recently_seen() -> TestResult {
        let mut events = Vec::with_capacity(ListenerHistory::MAX_KEYS);
        for port in 1..=4096_u16 {
            events.push(listener_event(port, None, at(i64::from(port))?));
        }
        let mut history = ListenerHistory::new();
        history.observe(&events, at(5_000)?);
        assert_eq!(history.len(), ListenerHistory::MAX_KEYS);

        history.observe(&[listener_event(5_000, None, at(6_000)?)], at(6_000)?);

        assert_eq!(history.len(), ListenerHistory::MAX_KEYS);
        assert!(
            !history.contains(&key(ListenerOwner::Unknown, 1)),
            "der älteste Schlüssel fällt heraus"
        );
        assert!(history.contains(&key(ListenerOwner::Unknown, 2)));
        assert!(history.contains(&key(ListenerOwner::Unknown, 4096)));
        assert!(history.contains(&key(ListenerOwner::Unknown, 5_000)));
        Ok(())
    }

    /// Bei gleichem `last_seen` fällt deterministisch der weiter vorne
    /// stehende (zuerst gelernte) Eintrag.
    #[test]
    fn test_eviction_tie_break_is_deterministic() -> TestResult {
        let same = at(7)?;
        let mut events = Vec::with_capacity(ListenerHistory::MAX_KEYS);
        for port in 1..=4096_u16 {
            events.push(listener_event(port, None, same));
        }
        let mut history = ListenerHistory::new();
        history.observe(&events, same);
        history.observe(&[listener_event(4097, None, same)], same);

        assert_eq!(history.len(), ListenerHistory::MAX_KEYS);
        assert!(!history.contains(&key(ListenerOwner::Unknown, 1)));
        assert!(history.contains(&key(ListenerOwner::Unknown, 2)));
        assert!(history.contains(&key(ListenerOwner::Unknown, 4097)));
        Ok(())
    }

    #[test]
    fn test_json_round_trip_preserves_history() -> TestResult {
        let mut history = ListenerHistory::new();
        history.observe(
            &[
                listener_event(22, None, at(1)?),
                listener_event(8080, uid_actor(1000), at(2)?),
                listener_event(
                    53,
                    Some(Actor {
                        uid: 0,
                        auid: None,
                        cgroup: Actor::raw_cgroup(9),
                        process: None,
                    }),
                    at(3)?,
                ),
            ],
            at(4)?,
        );

        let bytes = history.to_json().map_err(ctx("Historie serialisiert"))?;
        let back = ListenerHistory::from_json(&bytes).map_err(ctx("Historie gelesen"))?;
        assert_eq!(back, history);
        assert!(back.is_primed());

        // Auch die `Deserialize`-Implementierung liest dasselbe Format.
        let via_serde: ListenerHistory =
            serde_json::from_slice(&bytes).map_err(ctx("Historie über serde gelesen"))?;
        assert_eq!(via_serde, history);
        Ok(())
    }

    #[test]
    fn test_json_round_trip_of_an_unprimed_history() -> TestResult {
        let history = ListenerHistory::new();
        let bytes = history.to_json().map_err(ctx("Historie serialisiert"))?;
        let back = ListenerHistory::from_json(&bytes).map_err(ctx("Historie gelesen"))?;
        assert!(!back.is_primed());
        assert_eq!(back, history);
        Ok(())
    }

    #[test]
    fn test_listener_key_wire_form_is_stable() -> TestResult {
        let by_uid = serde_json::to_string(&key(ListenerOwner::Uid(1000), 8080))
            .map_err(ctx("Schlüssel serialisiert"))?;
        assert_eq!(
            by_uid,
            r#"{"owner":{"uid":1000},"protocol":"unknown","port":{"exact":8080},"bind":"unknown"}"#
        );
        let by_kid = serde_json::to_string(&ListenerKey {
            owner: ListenerOwner::CgroupKid(42),
            protocol: ListenerProtocol::Tcp,
            port: ListenerPort::Exact(22),
            bind: BindClass::Any,
        })
        .map_err(ctx("Schlüssel serialisiert"))?;
        assert_eq!(
            by_kid,
            r#"{"owner":{"cgroup-kid":42},"protocol":"tcp","port":{"exact":22},"bind":"any"}"#
        );
        let ephemeral = serde_json::to_string(&ephemeral_key(ListenerOwner::Unknown))
            .map_err(ctx("Schlüssel serialisiert"))?;
        assert_eq!(
            ephemeral,
            r#"{"owner":"unknown","protocol":"unknown","port":"ephemeral","bind":"unknown"}"#
        );
        let unknown =
            serde_json::to_string(&ListenerOwner::Unknown).map_err(ctx("Besitzer serialisiert"))?;
        assert_eq!(unknown, r#""unknown""#);
        Ok(())
    }

    #[test]
    fn test_from_json_rejects_unsupported_version() -> TestResult {
        let bytes = raw_history(
            ListenerHistory::FORMAT_VERSION.saturating_add(1),
            Vec::new(),
        )
        .to_json()
        .map_err(ctx("Historie serialisiert"))?;
        let Err(error) = ListenerHistory::from_json(&bytes) else {
            return Err(TestError::Unexpected("fremde Version angenommen".into()));
        };
        assert!(matches!(error, ListenerHistoryError::UnsupportedVersion));
        Ok(())
    }

    #[test]
    fn test_from_json_rejects_too_many_entries() -> TestResult {
        let mut entries = Vec::with_capacity(ListenerHistory::MAX_KEYS.saturating_add(1));
        for port in 1..=4097_u16 {
            entries.push(ListenerEntry {
                key: key(ListenerOwner::Unknown, port),
                last_seen: Timestamp::UNIX_EPOCH,
            });
        }
        let bytes = raw_history(ListenerHistory::FORMAT_VERSION, entries)
            .to_json()
            .map_err(ctx("Historie serialisiert"))?;
        let Err(error) = ListenerHistory::from_json(&bytes) else {
            return Err(TestError::Unexpected(
                "übergroße Historie angenommen".into(),
            ));
        };
        assert!(matches!(error, ListenerHistoryError::TooManyEntries));
        Ok(())
    }

    #[test]
    fn test_from_json_rejects_duplicate_keys() -> TestResult {
        let entry = ListenerEntry {
            key: key(ListenerOwner::Uid(1000), 8080),
            last_seen: Timestamp::UNIX_EPOCH,
        };
        let bytes = raw_history(ListenerHistory::FORMAT_VERSION, vec![entry, entry])
            .to_json()
            .map_err(ctx("Historie serialisiert"))?;
        let Err(error) = ListenerHistory::from_json(&bytes) else {
            return Err(TestError::Unexpected(
                "doppelter Schlüssel angenommen".into(),
            ));
        };
        assert!(matches!(error, ListenerHistoryError::DuplicateKey));
        // Die `Deserialize`-Implementierung prüft genauso.
        assert!(serde_json::from_slice::<ListenerHistory>(&bytes).is_err());
        Ok(())
    }

    /// Review-Befund: ein fehlendes `primed_at` machte serde still zu `None`
    /// — eine abgeschnittene Datei galt als „nicht vorbereitet" und lernte
    /// den nächsten Zyklus ungeprüft. `null` bleibt zulässig.
    #[test]
    fn test_from_json_rejects_missing_primed_at() -> TestResult {
        let missing_field = br#"{"version":1,"entries":[]}"#;
        assert!(matches!(
            ListenerHistory::from_json(missing_field),
            Err(ListenerHistoryError::Json { .. })
        ));
        assert!(serde_json::from_slice::<ListenerHistory>(missing_field).is_err());

        let explicit_null = br#"{"version":1,"primed_at":null,"entries":[]}"#;
        let history =
            ListenerHistory::from_json(explicit_null).map_err(ctx("null ist zulässig"))?;
        assert!(!history.is_primed());
        Ok(())
    }

    #[test]
    fn test_from_json_rejects_unknown_fields_and_garbage() {
        let unknown_field = br#"{"version":1,"primed_at":null,"entries":[],"extra":true}"#;
        assert!(matches!(
            ListenerHistory::from_json(unknown_field),
            Err(ListenerHistoryError::Json { .. })
        ));
        assert!(matches!(
            ListenerHistory::from_json(b"not json"),
            Err(ListenerHistoryError::Json {
                category: serde_json::error::Category::Syntax,
                line: 1,
                ..
            })
        ));
    }

    /// Review-Befund (Runde 2): der eingebettete `serde_json::Error` zitiert
    /// Dateiinhalt und erschien \u00fcber `Debug` und die Fehlerquelle. Jetzt ist
    /// der Fehler in `Display`, `Debug` und der Quellkette inhaltsfrei.
    #[test]
    fn test_error_is_content_free_in_display_debug_and_source() -> TestResult {
        let error = ListenerHistory::from_json(br#"{"version":"\u202e-evil"}"#)
            .err()
            .ok_or_else(missing("Lesefehler"))?;
        assert!(matches!(
            error,
            ListenerHistoryError::Json {
                category: serde_json::error::Category::Data,
                ..
            }
        ));
        for shown in [error.to_string(), format!("{error:?}")] {
            assert!(!shown.contains("evil"), "{shown}");
            assert!(!shown.contains('\u{202e}'), "{shown}");
            assert!(!shown.contains("202e"), "{shown}");
        }
        assert!(std::error::Error::source(&error).is_none());
        Ok(())
    }

    /// Review-Befund (Runde 2): eine aufgebl\u00e4hte Datei wurde vollst\u00e4ndig
    /// geparst, bevor die Eintragsgrenze griff. Jetzt wird sie vorher
    /// abgelehnt \u2014 eine volle Historie mit breitesten Werten passt dagegen.
    #[test]
    fn test_from_json_bounds_the_input_size() -> TestResult {
        let oversized = vec![b' '; ListenerHistory::MAX_JSON_BYTES.saturating_add(1)];
        assert_eq!(
            ListenerHistory::from_json(&oversized),
            Err(ListenerHistoryError::TooLarge)
        );

        let widest: Timestamp = "2026-11-08T10:00:00.123456789Z"
            .parse()
            .map_err(ctx("breitester Zeitstempel"))?;
        let mut entries = Vec::with_capacity(ListenerHistory::MAX_KEYS);
        for port in 1..=4096_u16 {
            entries.push(ListenerEntry {
                key: ListenerKey {
                    owner: ListenerOwner::CgroupKid(u64::MAX.saturating_sub(u64::from(port))),
                    protocol: ListenerProtocol::Unknown,
                    port: ListenerPort::Exact(port),
                    bind: BindClass::Specific,
                },
                last_seen: widest,
            });
        }
        let bytes = raw_history(ListenerHistory::FORMAT_VERSION, entries)
            .to_json()
            .map_err(ctx("volle Historie serialisiert"))?;
        assert!(bytes.len() <= ListenerHistory::MAX_JSON_BYTES);
        let full = ListenerHistory::from_json(&bytes).map_err(ctx("volle Historie gelesen"))?;
        assert_eq!(full.len(), ListenerHistory::MAX_KEYS);
        Ok(())
    }
}
