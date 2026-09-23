//! Sammelschleife: pollt die Sensoren gleichförmig, leert die realen
//! v1-Wire-Ringpuffer und leitet jedes geformte Ereignis an die Senke weiter.
//!
//! # Verantwortungsbereich
//! Zwei Wege führen hier zur Senke:
//!
//! - **Sensor-Weg.** [`run_once`] führt genau eine Erfassungsrunde über eine
//!   Liste von `harw_dod_signals::Sensor`-Trait-Objekten aus:
//!   `sensor.poll(now)` je Sensor, jedes dabei geformte Ereignis wird an
//!   [`crate::sink::EventSink`] weitergereicht. Diese Funktion iteriert
//!   generisch über `Vec<std::sync::Arc<dyn harw_dod_signals::Sensor>>`.
//!   Produktiv tragen diesen Weg nur degradierte Sensoren
//!   (`crate::sensors::UnavailableSensor`); geladene Sensoren laufen
//!   ausschließlich über den Wire-Weg.
//! - **Wire-Weg.** [`drain_wire_once`] liest je [`WireSource`] (ein Sensor,
//!   dessen v1-Objekte über `harw_dod_bpf::RealBpfLoader::load_contracts`
//!   geladen sind) die versionierten Wire-Records aller Griffe, formt sie
//!   über einen privaten v1-Wandler zu `SecurityEvent`s und rechnet die
//!   Verlustzähler der Ladeschicht ab. [`unload_all`] entfernt beim
//!   kontrollierten Stopp alle Griffe wieder.
//!
//! Die Schleife selbst lebt nicht hier: `main::collect_forever` ruft je
//! Runde [`run_once`] und [`drain_wire_once`] auf, entscheidet über
//! vorübergehende Fehler und liest die Systemuhr (Kompositionswurzel).
//!
//! # Zeitbudget einer Wire-Runde
//! [`WIRE_ROUND_BUDGET`] (200 ms, dieselbe Wartezeit wie
//! `harw_dod_procmon::DEFAULT_READ_TIMEOUT`) wird gleichmäßig auf **alle**
//! Griffe aller Quellen verteilt. `RealBpfLoader::read_wire_events` kehrt
//! früher zurück, sobald ein Griff Records liefert; eine Runde dauert also
//! höchstens ungefähr das Budget, nie Budget × Griffzahl. Die Zeitbeziehung
//! (`harw_dod_bpf::KernelTimeMapper`) wird **je Runde neu** gemessen. Lässt
//! sie sich nicht messen, bleiben die Records im Ringpuffer (kein Verlust),
//! die Runde wartet ihr Budget ab und rechnet nur die Verlustzähler ab.
//!
//! # Der v1-Wandler (`WireEvent` → `SecurityEvent`)
//! - **`TcpConnect`** wird über `harw_dod_flow::parse_tcp_connect_v1`
//!   gedeutet und über `harw_dod_flow::to_security_event` gemeldet — dieselbe
//!   Melderegel wie der Flow-Sensor: ein Connect ist per Programmvertrag
//!   ausgehendes TCP; gemeldet wird nur ein Ziel, das
//!   `NetworkScope::allows_addr` **nicht** erlaubt. Trägt die Quelle keinen
//!   `net_scope`, gilt `NetworkScope::empty()`: jedes Ziel wird gemeldet
//!   (lieber zu viel melden als still verschweigen).
//! - **`Exec`** wird über `harw_dod_procmon::parse_exec_v1` gedeutet. Das
//!   Schema `EventKind::ProcessExec { path: String, argv_digest }` kann weder
//!   einen fehlenden Pfad noch „argv nicht erhoben" ausdrücken. Deshalb:
//!   - Nur ein vollständig erfasster, nicht-leerer Pfad
//!     (`ExecutablePathCapture::Captured`) wird als `ProcessExec` gemeldet.
//!     Ein möglicherweise abgeschnittener Pfad
//!     (`PossiblyTruncated`) oder ein nicht lesbarer Pfad (`Unavailable`,
//!     ebenso ein leerer) wird **nicht** als Pfad ausgegeben — ein
//!     abgeschnittener Pfad sähe aus wie ein anderes, existierendes Programm,
//!     ein leerer wie Evidenz für ein Programm ohne Pfad. Solche Records
//!     zählen als *verworfen* und degradieren den Sensor für diese Runde
//!     (siehe unten) — der Sentinel erfährt also, dass Evidenz fehlt.
//!   - `argv_digest` trägt für **jedes** v1-Exec denselben festen
//!     **Markierungswert** [`argv_not_collected_marker`]: den Digest des
//!     domänengetrennten Etiketts `ARGV_NOT_COLLECTED_MARKER`, nicht den
//!     Digest irgendeiner Kommandozeile. V1 erhebt kein argv
//!     (`harw_dod_procmon::ArgvCapture::NotCollected`); der Markierungswert
//!     ist die einzige Aussage dazu und ausdrücklich **kein** Beleg für die
//!     Gleichheit zweier Kommandozeilen. Insbesondere ist er nicht der Digest
//!     eines leeren Puffers (der sähe aus wie „argv war leer"). Sobald das
//!     Schema „nicht erhoben" direkt ausdrücken kann (etwa
//!     `argv_digest: Option<ContentDigest>`), entfällt der Markierungswert.
//! - **`ProcessExit`** wird über `harw_dod_procmon::parse_process_exit_v1`
//!   geprüft, aber nicht gemeldet: `EventKind` hat keine Variante für ein
//!   Prozessende. Das ist kein Verlust, sondern eine Schemagrenze.
//! - Ein Record, dessen Payload der art-spezifische Parser ablehnt, zählt als
//!   *verworfen* (fehlerhaft) und degradiert den Sensor für diese Runde.
//!
//! Der Akteur entspricht `harw_dod_flow::to_security_event`: `uid` aus dem
//! Wire-Kopf, `auid` und `cgroup` unbekannt (`None`). `observed_at` ist die
//! vom Lader gemessene Abbildung des Kernel-`ktime`; die
//! `TimeConfidence` hat im `SecurityEvent`-Schema kein Feld und geht auf
//! diesem Sendeweg verloren.
//!
//! # Verlustabrechnung
//! `RealBpfLoader::loss_counters` liefert je Griff kumulative Zähler
//! (`exec`, `process_exit`, `tcp_connect`) und den **laderweiten** Zähler
//! `invalid_wire_events`. Je Griff hält [`WireSource`] den zuletzt gesehenen
//! Stand; die Differenz wird mit `saturating_sub` gebildet (ein kleiner
//! gewordener Zähler ergibt 0, nie einen Überlauf). Der laderweite Zähler
//! wird **einmal je Lader** abgerechnet: die Runde startet beim höchsten je
//! gespeicherten Stand und rechnet jeden Zuwachs genau dem Griff zu, nach
//! dessen Lesevorgang er erstmals sichtbar wird (die Lesevorgänge laufen
//! nacheinander, der Zähler wächst nur in `read_wire_events`). Die Summe
//! über alle Griffe einer Runde ist damit genau der Zuwachs des Laders —
//! nie mehrfach gezählt. Dafür müssen alle übergebenen Quellen denselben
//! Lader teilen (so baut sie `main`).
//!
//! Hat ein Sensor in einer Runde Verlust oder verworfene Records, loggt
//! diese Datei **eine** `tracing::warn`-Zeile mit den Zahlen und sendet
//! **genau ein** `EventKind::SensorDegraded { sensor }` für diesen Sensor —
//! auch wenn mehrere Griffe oder mehrere Quellen desselben Sensors betroffen
//! sind.
//!
//! # Ohne Kernel, ohne Socket testbar
//! Der Sensor-Weg wird mit reinen Mock-Implementierungen von
//! `harw_dod_signals::Sensor` und mit `crate::sensors::UnavailableSensor`
//! getestet (dessen erste Lesung kommt ohne Kernel und ohne Wartezeit aus).
//! Der Wire-Weg wird über seine reinen Bausteine getestet (Wandler,
//! Verlustdifferenz, Degradierungsauswahl); kein Test lädt ein echtes
//! eBPF-Programm, öffnet einen echten Socket oder bindet Landlock.
//!
//! # Exportierte Typen
//! [`run_once`], [`WireSource`], [`WIRE_ROUND_BUDGET`],
//! [`drain_wire_once`], [`unload_all`].
//!
//! # Nebenläufigkeit
//! [`run_once`] hat keine innere Veränderlichkeit.
//! [`WireSource`] hält seinen letzten Verluststand hinter einem `Mutex`
//! (vergiftet → der innere Wert wird weiterverwendet, wie in
//! `RealBpfLoader`); [`drain_wire_once`] ist für genau einen Aufrufer je
//! Quellenliste gedacht (die Sammelschleife).
//!
//! # Fehler
//! [`crate::error::ProbeError`], durchgereicht aus `sensor.poll` (über
//! `harw_dod_cap::SensorError`), aus `RealBpfLoader::read_wire_events`/
//! `loss_counters` (über `harw_dod_bpf::BpfError` →
//! [`crate::error::ProbeError::BpfLoad`]) oder aus
//! [`crate::sink::EventSink::send`].
//!
//! # Examples
//! ```rust,ignore
//! use crate::collect::{drain_wire_once, run_once, unload_all, WireSource};
//! ```

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use harw_authority::NetworkScope;
use harw_dod_bpf::{
    BpfHandle, BpfLossCounters, KernelTimeMapper, RealBpfLoader, TimedWireEvent, WireEventType,
};
use harw_dod_flow::{Direction, FlowEvent, Protocol};
use harw_dod_procmon::ExecutablePathCapture;
use harw_dod_signals::{Actor, EventKind, SecurityEvent, Sensor};
use harw_types::{ContentDigest, SensorId};
use jiff::Timestamp;

use crate::error::ProbeError;
use crate::sink::EventSink;

/// Führt eine einzelne Erfassungsrunde über alle übergebenen Sensoren aus.
///
/// # Description
/// Pollt jeden Sensor in `sensors` genau einmal (in Reihenfolge der Liste)
/// und sendet jedes dabei geformte Ereignis über `sink`. Bricht beim ersten
/// Fehler ab — sowohl ein Sensorfehler als auch ein Sendefehler beenden
/// diese Runde sofort, ohne die verbleibenden Sensoren oder Ereignisse noch
/// zu bearbeiten.
///
/// # Arguments
/// - `sensors` (`&[std::sync::Arc<dyn harw_dod_signals::Sensor>]`): die
///   Sensoren, die in dieser Runde je einmal ausgelesen werden.
/// - `sink` (`&dyn crate::sink::EventSink`): die Senke, an die jedes
///   geformte Ereignis dieser Runde gesendet wird.
/// - `now` (`jiff::Timestamp`): injizierte Zeit, unverändert an jedes
///   `sensor.poll` weitergereicht.
///
/// # Returns
/// Die Anzahl der in dieser Runde gesendeten Ereignisse.
///
/// # Errors
/// - [`ProbeError::Sensor`]: wenn ein `sensor.poll` scheitert.
/// - Der Fehler von [`EventSink::send`]: wenn das Senden eines Ereignisses
///   scheitert.
pub fn run_once(
    sensors: &[Arc<dyn Sensor>],
    sink: &dyn EventSink,
    now: Timestamp,
) -> Result<usize, ProbeError> {
    let mut sent = 0usize;
    for sensor in sensors {
        let reading = sensor.poll(now)?;
        for event in &reading.events {
            sink.send(event)?;
            sent += 1;
        }
    }
    Ok(sent)
}

/// Gesamtes Zeitbudget einer [`drain_wire_once`]-Runde über alle Griffe.
///
/// # Description
/// Wird gleichmäßig auf alle Griffe aller Quellen verteilt (siehe
/// Moduldoku, Abschnitt „Zeitbudget einer Wire-Runde"). Derselbe Wert wie
/// `harw_dod_procmon::DEFAULT_READ_TIMEOUT`.
pub const WIRE_ROUND_BUDGET: Duration = Duration::from_millis(200);

/// Domänengetrenntes Etikett, dessen Digest der feste
/// `argv_digest`-Markierungswert jedes v1-Exec ist.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Der v1-Wandler". Das Etikett ist **keine**
/// Kommandozeile und wird nie als solche behauptet.
const ARGV_NOT_COLLECTED_MARKER: &[u8] = b"harw-probe-bpf/wire-v1/argv-not-collected";

/// Der feste `argv_digest`-Markierungswert für „v1 erhebt kein argv".
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Der v1-Wandler": derselbe Wert für jedes
/// v1-Exec, ausdrücklich kein Beleg für die Gleichheit zweier
/// Kommandozeilen und nicht der Digest eines leeren Puffers.
///
/// # Returns
/// `ContentDigest::of(ARGV_NOT_COLLECTED_MARKER)`.
#[must_use]
pub fn argv_not_collected_marker() -> ContentDigest {
    ContentDigest::of(ARGV_NOT_COLLECTED_MARKER)
}

/// Ein Sensor, dessen v1-Objekte über den realen Lader geladen sind.
///
/// # Description
/// Bündelt die [`SensorId`], unter der die geformten Ereignisse gemeldet
/// werden, die Griffe aus `RealBpfLoader::load_contracts` und — nur für den
/// Flow-Sensor — den Zielbereich der Melderegel. Hält je Griff den zuletzt
/// abgerechneten Verluststand (siehe Moduldoku, Abschnitt
/// „Verlustabrechnung"); dieser Stand ist privat und startet bei null, weil
/// die Zähler eines frisch geladenen Objekts bei null beginnen.
///
/// # Fields
/// - `sensor` (`SensorId`): meldende Kennung.
/// - `handles` (`Vec<BpfHandle>`): alle Griffe dieses Sensors.
/// - `net_scope` (`Option<NetworkScope>`): Zielbereich für
///   `TcpConnect`-Records; `None` → `NetworkScope::empty()` (alles melden).
#[derive(Debug)]
pub struct WireSource {
    /// Die Kennung, unter der Ereignisse und Degradierungen gemeldet werden.
    pub sensor: SensorId,
    /// Die Griffe aus `RealBpfLoader::load_contracts`.
    pub handles: Vec<BpfHandle>,
    /// Zielbereich der Flow-Melderegel; `None` für den Prozess-Sensor.
    pub net_scope: Option<NetworkScope>,
    /// Zuletzt abgerechneter Verluststand, ein Eintrag je Griff (gleicher
    /// Index wie `handles`).
    last_loss: Mutex<Vec<BpfLossCounters>>,
}

impl WireSource {
    /// Baut eine Quelle mit Verluststand null für jeden Griff.
    ///
    /// # Arguments
    /// - `sensor` (`SensorId`): meldende Kennung.
    /// - `handles` (`Vec<BpfHandle>`): die geladenen Griffe dieses Sensors.
    /// - `net_scope` (`Option<NetworkScope>`): siehe Typ-Doku.
    ///
    /// # Returns
    /// Die neue Quelle.
    #[must_use]
    pub fn new(sensor: SensorId, handles: Vec<BpfHandle>, net_scope: Option<NetworkScope>) -> Self {
        let last_loss = Mutex::new(vec![BpfLossCounters::default(); handles.len()]);
        Self {
            sensor,
            handles,
            net_scope,
            last_loss,
        }
    }

    /// Sperrt den Verluststand; ein vergifteter `Mutex` liefert seinen
    /// inneren Wert (dieselbe Haltung wie `RealBpfLoader`).
    fn lock_last_loss(&self) -> MutexGuard<'_, Vec<BpfLossCounters>> {
        self.last_loss
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Warum ein Wire-Record nicht als Ereignis gemeldet werden konnte, obwohl
/// er Evidenz trug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rejection {
    /// Der art-spezifische Parser hat den Payload abgelehnt.
    Malformed,
    /// Exec ohne lesbaren (oder mit leerem) Pfad.
    ExecPathUnavailable,
    /// Exec mit möglicherweise abgeschnittenem Pfad.
    ExecPathTruncated,
}

/// Ergebnis des v1-Wandlers für einen Record.
#[derive(Debug, Clone, PartialEq)]
enum Conversion {
    /// Ein zu meldendes Ereignis.
    Report(SecurityEvent),
    /// Gültig, aber nach Melderegel oder Schemagrenze kein Ereignis
    /// (erlaubtes Ziel, Prozessende).
    NotReportable,
    /// Evidenz, die nicht ehrlich darstellbar war; degradiert den Sensor.
    Rejected(Rejection),
}

/// Wandelt einen v1-Wire-Record in ein `SecurityEvent` (oder keins).
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Der v1-Wandler". Reine Funktion.
///
/// # Arguments
/// - `timed` (`&TimedWireEvent`): Record samt gemessenem `observed_at`.
/// - `sensor` (`&SensorId`): meldende Kennung.
/// - `net_scope` (`Option<&NetworkScope>`): Zielbereich der Flow-Melderegel.
///
/// # Returns
/// Die [`Conversion`] dieses Records.
fn convert_wire_event(
    timed: &TimedWireEvent,
    sensor: &SensorId,
    net_scope: Option<&NetworkScope>,
) -> Conversion {
    match timed.event.event_type {
        WireEventType::Exec => {
            let Ok(exec) = harw_dod_procmon::parse_exec_v1(&timed.event) else {
                return Conversion::Rejected(Rejection::Malformed);
            };
            let path = match (exec.path_capture, exec.path) {
                (ExecutablePathCapture::Captured, Some(path)) if !path.is_empty() => path,
                (ExecutablePathCapture::PossiblyTruncated, _) => {
                    return Conversion::Rejected(Rejection::ExecPathTruncated);
                }
                _ => return Conversion::Rejected(Rejection::ExecPathUnavailable),
            };
            Conversion::Report(SecurityEvent {
                sensor: sensor.clone(),
                observed_at: timed.observed_at,
                actor: Some(Actor {
                    uid: exec.task.uid,
                    auid: None,
                    cgroup: None,
                }),
                kind: EventKind::ProcessExec {
                    path,
                    argv_digest: argv_not_collected_marker(),
                },
            })
        }
        WireEventType::ProcessExit => match harw_dod_procmon::parse_process_exit_v1(&timed.event) {
            Ok(_) => Conversion::NotReportable,
            Err(_) => Conversion::Rejected(Rejection::Malformed),
        },
        WireEventType::TcpConnect => {
            let Ok(connect) = harw_dod_flow::parse_tcp_connect_v1(&timed.event) else {
                return Conversion::Rejected(Rejection::Malformed);
            };
            let flow = FlowEvent {
                pid: connect.task.tgid,
                uid: connect.task.uid,
                protocol: Protocol::Tcp,
                direction: Direction::Outbound,
                remote_addr: connect.remote_addr,
                remote_port: connect.remote_port,
            };
            let empty_scope;
            let scope = match net_scope {
                Some(scope) => scope,
                None => {
                    empty_scope = NetworkScope::empty();
                    &empty_scope
                }
            };
            match harw_dod_flow::to_security_event(&flow, sensor, timed.observed_at, scope) {
                Some(event) => Conversion::Report(event),
                None => Conversion::NotReportable,
            }
        }
    }
}

/// Zuwachs der Verlustzähler eines Sensors in einer Runde.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LossDelta {
    exec: u64,
    process_exit: u64,
    tcp_connect: u64,
    invalid_wire_events: u64,
}

impl LossDelta {
    /// Addiert `other` sättigend auf.
    fn accumulate(&mut self, other: Self) {
        self.exec = self.exec.saturating_add(other.exec);
        self.process_exit = self.process_exit.saturating_add(other.process_exit);
        self.tcp_connect = self.tcp_connect.saturating_add(other.tcp_connect);
        self.invalid_wire_events = self
            .invalid_wire_events
            .saturating_add(other.invalid_wire_events);
    }

    /// Ob irgendein Zähler gewachsen ist.
    fn is_zero(&self) -> bool {
        self.exec == 0
            && self.process_exit == 0
            && self.tcp_connect == 0
            && self.invalid_wire_events == 0
    }
}

/// Differenz eines Griffs gegenüber seinem letzten Stand.
///
/// # Description
/// Je-Art-Zähler: `current - previous`, sättigend. Laderweiter Zähler
/// `invalid_wire_events`: nur der Teil oberhalb von `invalid_seen` (dem
/// höchsten in dieser Runde bereits abgerechneten Stand); `invalid_seen`
/// wird danach angehoben. So wird der laderweite Zuwachs genau einmal
/// abgerechnet, egal wie viele Griffe ihn melden.
///
/// # Arguments
/// - `previous` (`&BpfLossCounters`): zuletzt abgerechneter Stand des Griffs.
/// - `current` (`&BpfLossCounters`): gerade gelesener Stand des Griffs.
/// - `invalid_seen` (`&mut u64`): bereits abgerechneter laderweiter Stand.
///
/// # Returns
/// Der [`LossDelta`] dieses Griffs.
fn handle_loss_delta(
    previous: &BpfLossCounters,
    current: &BpfLossCounters,
    invalid_seen: &mut u64,
) -> LossDelta {
    let invalid_wire_events = current.invalid_wire_events.saturating_sub(*invalid_seen);
    *invalid_seen = (*invalid_seen).max(current.invalid_wire_events);
    LossDelta {
        exec: current.exec.saturating_sub(previous.exec),
        process_exit: current.process_exit.saturating_sub(previous.process_exit),
        tcp_connect: current.tcp_connect.saturating_sub(previous.tcp_connect),
        invalid_wire_events,
    }
}

/// Höchster bereits abgerechneter laderweiter `invalid_wire_events`-Stand
/// einer Quelle (0, wenn noch nie abgerechnet).
fn invalid_baseline(last: &[BpfLossCounters]) -> u64 {
    last.iter()
        .map(|counters| counters.invalid_wire_events)
        .max()
        .unwrap_or(0)
}

/// Verworfene Records eines Sensors in einer Runde.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Rejections {
    malformed: u64,
    exec_path_unavailable: u64,
    exec_path_truncated: u64,
}

impl Rejections {
    /// Zählt einen verworfenen Record.
    fn record(&mut self, rejection: Rejection) {
        let slot = match rejection {
            Rejection::Malformed => &mut self.malformed,
            Rejection::ExecPathUnavailable => &mut self.exec_path_unavailable,
            Rejection::ExecPathTruncated => &mut self.exec_path_truncated,
        };
        *slot = slot.saturating_add(1);
    }

    /// Ob irgendein Record verworfen wurde.
    fn is_zero(&self) -> bool {
        self.malformed == 0 && self.exec_path_unavailable == 0 && self.exec_path_truncated == 0
    }
}

/// Bilanz eines Sensors in einer Runde.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SensorRound {
    loss: LossDelta,
    rejections: Rejections,
}

impl SensorRound {
    /// Ob dieser Sensor in dieser Runde Evidenz verloren hat.
    fn is_degraded(&self) -> bool {
        !self.loss.is_zero() || !self.rejections.is_zero()
    }
}

/// Teilt das Rundenbudget gleichmäßig auf `handle_count` Griffe auf.
///
/// # Returns
/// `budget / handle_count`; bei 0 Griffen das ganze Budget.
fn per_handle_timeout(budget: Duration, handle_count: usize) -> Duration {
    let divisor = u32::try_from(handle_count).unwrap_or(u32::MAX).max(1);
    budget / divisor
}

/// Wählt die Degradierungsmeldungen einer Runde aus.
///
/// # Description
/// Genau ein `EventKind::SensorDegraded` je betroffener `SensorId`, in
/// Reihenfolge des ersten Auftretens — auch wenn mehrere Quellen dieselbe
/// Kennung tragen.
///
/// # Arguments
/// - `rounds` (`&[(&SensorId, SensorRound)]`): Bilanz je Quelle.
/// - `now` (`Timestamp`): `observed_at` der Meldungen.
///
/// # Returns
/// Die zu sendenden Meldungen.
fn degraded_events(rounds: &[(&SensorId, SensorRound)], now: Timestamp) -> Vec<SecurityEvent> {
    let mut affected: Vec<&SensorId> = Vec::new();
    for (sensor, round) in rounds {
        if round.is_degraded() && !affected.contains(sensor) {
            affected.push(*sensor);
        }
    }
    affected
        .into_iter()
        .map(|sensor| SecurityEvent {
            sensor: sensor.clone(),
            observed_at: now,
            actor: None,
            kind: EventKind::SensorDegraded {
                sensor: sensor.clone(),
            },
        })
        .collect()
}

/// Leert die v1-Ringpuffer aller Quellen genau einmal.
///
/// # Description
/// Je Griff: Records über `RealBpfLoader::read_wire_events` lesen (Budget
/// [`WIRE_ROUND_BUDGET`] geteilt durch die Griffzahl, Zeitbeziehung je Runde
/// neu gemessen), jeden Record über den v1-Wandler formen und melden, danach
/// `RealBpfLoader::loss_counters` abrechnen. Am Ende der Runde je
/// betroffenem Sensor eine `tracing::warn`-Zeile und genau ein
/// `EventKind::SensorDegraded`. Siehe Moduldoku für alle Einzelheiten.
///
/// # Arguments
/// - `loader` (`&RealBpfLoader`): der **eine** Lader, der alle Griffe in
///   `sources` geladen hat.
/// - `sources` (`&[WireSource]`): die zu leerenden Quellen.
/// - `sink` (`&dyn EventSink`): Ziel aller geformten Ereignisse.
///
/// # Returns
/// Die Anzahl der gesendeten Ereignisse, einschließlich
/// Degradierungsmeldungen.
///
/// # Errors
/// - [`ProbeError::BpfLoad`]: `read_wire_events` oder `loss_counters`
///   scheitert (z. B. `UnknownHandle` nach einem vorzeitigen `unload`).
/// - Der Fehler von [`EventSink::send`].
///
/// Ein Fehler beendet die Runde sofort; bereits gesendete Ereignisse bleiben
/// gesendet.
pub fn drain_wire_once(
    loader: &RealBpfLoader,
    sources: &[WireSource],
    sink: &dyn EventSink,
) -> Result<usize, ProbeError> {
    let handle_count: usize = sources.iter().map(|source| source.handles.len()).sum();
    if handle_count == 0 {
        return Ok(0);
    }
    let per_handle = per_handle_timeout(WIRE_ROUND_BUDGET, handle_count);
    let mapper = KernelTimeMapper::sample();
    if mapper.is_none() {
        tracing::warn!(
            "kernel time mapping could not be sampled; wire records stay queued this round"
        );
    }

    let mut invalid_seen = sources
        .iter()
        .map(|source| invalid_baseline(&source.lock_last_loss()))
        .max()
        .unwrap_or(0);
    let mut sent = 0usize;
    let mut rounds: Vec<(&SensorId, SensorRound)> = Vec::with_capacity(sources.len());

    for source in sources {
        let mut round = SensorRound::default();
        for (index, handle) in source.handles.iter().enumerate() {
            match mapper {
                Some(mapper) => {
                    let records = loader.read_wire_events(handle, per_handle, mapper)?;
                    for timed in &records {
                        match convert_wire_event(timed, &source.sensor, source.net_scope.as_ref()) {
                            Conversion::Report(event) => {
                                sink.send(&event)?;
                                sent += 1;
                            }
                            Conversion::NotReportable => {}
                            Conversion::Rejected(rejection) => round.rejections.record(rejection),
                        }
                    }
                }
                // Ohne Zeitbeziehung nicht lesen (Records bleiben im
                // Ringpuffer), aber das Budget einhalten, damit die
                // Sammelschleife nicht leer dreht.
                None => std::thread::sleep(per_handle),
            }

            let current = loader.loss_counters(handle)?;
            let mut last = source.lock_last_loss();
            if last.len() <= index {
                last.resize(index + 1, BpfLossCounters::default());
            }
            if let Some(previous) = last.get_mut(index) {
                round
                    .loss
                    .accumulate(handle_loss_delta(previous, &current, &mut invalid_seen));
                *previous = current;
            }
        }

        if round.is_degraded() {
            tracing::warn!(
                sensor = %source.sensor.as_str(),
                lost_exec = round.loss.exec,
                lost_process_exit = round.loss.process_exit,
                lost_tcp_connect = round.loss.tcp_connect,
                invalid_wire_events = round.loss.invalid_wire_events,
                rejected_malformed = round.rejections.malformed,
                rejected_exec_path_unavailable = round.rejections.exec_path_unavailable,
                rejected_exec_path_truncated = round.rejections.exec_path_truncated,
                "bpf sensor lost evidence this round; reporting it as degraded"
            );
        }
        rounds.push((&source.sensor, round));
    }

    for event in degraded_events(&rounds, Timestamp::now()) {
        sink.send(&event)?;
        sent += 1;
    }
    Ok(sent)
}

/// Entfernt alle Griffe aller Quellen aus dem Lader.
///
/// # Description
/// Für den kontrollierten Stopp: `RealBpfLoader::unload` löst die
/// Anheftungen jedes Griffs. Ein Fehler (etwa `UnknownHandle` für einen
/// bereits entfernten Griff) wird geloggt und hält das Entfernen der
/// übrigen Griffe nicht auf.
///
/// # Arguments
/// - `loader` (`&RealBpfLoader`): der Lader, der die Griffe geladen hat.
/// - `sources` (`&[WireSource]`): die Quellen, deren Griffe entfernt werden.
pub fn unload_all(loader: &RealBpfLoader, sources: &[WireSource]) {
    for source in sources {
        for handle in &source.handles {
            if let Err(error) = loader.unload(handle) {
                tracing::warn!(
                    sensor = %source.sensor.as_str(),
                    attach_point = handle.attach_point(),
                    error = %error,
                    "failed to unload a bpf handle during controlled stop"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
    use harw_dod_signals::{EventKind, SecurityEvent, Sensor, SensorReading};
    use harw_types::{ContentDigest, SensorId};
    use jiff::Timestamp;

    use super::run_once;
    use crate::error::ProbeError;
    use crate::sensors::UnavailableSensor;
    use crate::sink::EventSink;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Test-Senke: zeichnet jedes gesendete Ereignis auf, statt es zu
    /// übertragen. Öffnet keinen Socket.
    #[derive(Default)]
    struct RecordingSink {
        sent: Mutex<Vec<SecurityEvent>>,
    }

    impl EventSink for RecordingSink {
        fn send(&self, event: &SecurityEvent) -> Result<(), ProbeError> {
            self.sent
                .lock()
                .map_err(|_| ProbeError::SentinelSendFailed)?
                .push(event.clone());
            Ok(())
        }
    }

    #[derive(Debug)]
    struct StaticSensor {
        handle: SensorHandle<Bound>,
        events: Vec<SecurityEvent>,
    }

    impl Sensor for StaticSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
            Ok(SensorReading {
                samples: Vec::new(),
                events: self.events.clone(),
            })
        }
    }

    fn static_sensor(sensor_id: &str, events: Vec<SecurityEvent>) -> Arc<dyn Sensor> {
        let handle = SensorHandle::new(SensorId::from_str(sensor_id), Capability::LoadBpfProgram)
            .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
        Arc::new(StaticSensor { handle, events })
    }

    fn sample_event(sensor_id: &str) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str(sensor_id),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::ProcessExec {
                path: "/usr/sbin/sshd".to_owned(),
                argv_digest: ContentDigest::of(b"unused in this test"),
            },
        }
    }

    #[test]
    fn test_run_once_forwards_every_event_from_every_sensor_to_the_sink() -> TestResult {
        let sensors = vec![
            static_sensor(
                "probe-bpf-procmon-0",
                vec![sample_event("probe-bpf-procmon-0")],
            ),
            static_sensor("probe-bpf-flow-0", vec![sample_event("probe-bpf-flow-0")]),
        ];
        let sink = RecordingSink::default();

        let sent = run_once(&sensors, &sink, Timestamp::UNIX_EPOCH)
            .map_err(ctx("static sensors never fail"))?;

        assert_eq!(sent, 2);
        let recorded = sink
            .sent
            .lock()
            .map_err(ctx("test mutex is never poisoned"))?;
        assert_eq!(recorded.len(), 2);
        Ok(())
    }

    #[test]
    fn test_run_once_forwards_zero_events_for_sensors_with_no_reading() -> TestResult {
        let sensors = vec![static_sensor("probe-bpf-procmon-0", Vec::new())];
        let sink = RecordingSink::default();

        let sent = run_once(&sensors, &sink, Timestamp::UNIX_EPOCH)
            .map_err(ctx("static sensor never fails"))?;
        assert_eq!(sent, 0);
        Ok(())
    }

    #[test]
    fn test_run_once_stops_at_the_first_sink_error() -> TestResult {
        struct FailingSink;
        impl EventSink for FailingSink {
            fn send(&self, _event: &SecurityEvent) -> Result<(), ProbeError> {
                Err(ProbeError::SentinelSendFailed)
            }
        }

        let sensors = vec![static_sensor(
            "probe-bpf-procmon-0",
            vec![sample_event("probe-bpf-procmon-0")],
        )];

        let Err(err) = run_once(&sensors, &FailingSink, Timestamp::UNIX_EPOCH) else {
            return Err(TestError::Unexpected(
                "a sink failure must propagate".into(),
            ));
        };
        assert!(matches!(err, ProbeError::SentinelSendFailed));
        Ok(())
    }

    #[test]
    fn test_run_once_propagates_a_sensor_error() -> TestResult {
        #[derive(Debug)]
        struct FailingSensor {
            handle: SensorHandle<Bound>,
        }
        impl Sensor for FailingSensor {
            fn handle(&self) -> &SensorHandle<Bound> {
                &self.handle
            }
            fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
                Err(SensorError::OutsideScope)
            }
        }

        let handle = SensorHandle::new(SensorId::from_str("failing-0"), Capability::LoadBpfProgram)
            .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
        let sensors: Vec<Arc<dyn Sensor>> = vec![Arc::new(FailingSensor { handle })];
        let sink = RecordingSink::default();

        let Err(err) = run_once(&sensors, &sink, Timestamp::UNIX_EPOCH) else {
            return Err(TestError::Unexpected("sensor error must propagate".into()));
        };
        assert!(matches!(err, ProbeError::Sensor(SensorError::OutsideScope)));
        Ok(())
    }

    /// Der generische `Sensor`-Zweig trägt im Betrieb nur degradierte
    /// Sensoren: deren einmalige `SensorDegraded`-Meldung muss unter der
    /// Kennung des vertretenen Sensors bei der Senke ankommen.
    #[test]
    fn test_run_once_forwards_the_degradation_of_an_unavailable_sensor() -> TestResult {
        let id = SensorId::from_str("probe-bpf-flow-0");
        let sensors: Vec<Arc<dyn Sensor>> =
            vec![Arc::new(UnavailableSensor::new(id.clone(), Vec::new()))];
        let sink = RecordingSink::default();

        let sent = run_once(&sensors, &sink, Timestamp::UNIX_EPOCH)
            .map_err(ctx("an unavailable sensor never fails to poll"))?;
        assert_eq!(sent, 1);

        let recorded = sink
            .sent
            .lock()
            .map_err(ctx("test mutex is never poisoned"))?;
        let event = recorded
            .first()
            .ok_or(TestError::Missing("the degradation event"))?;
        assert_eq!(event.sensor, id);
        assert_eq!(event.kind, EventKind::SensorDegraded { sensor: id.clone() });
        Ok(())
    }
}

#[cfg(test)]
mod wire_tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    use harw_authority::{EgressTarget, NetworkScope};
    use harw_dod_bpf::{
        BpfLossCounters, TaskIdentity, TimeConfidence, TimedWireEvent, WireEvent, WireEventType,
    };
    use harw_dod_procmon::{EXEC_PATH_TRUNCATED, EXEC_PATH_UNAVAILABLE};
    use harw_dod_signals::{Actor, EventKind};
    use harw_types::{ContentDigest, SensorId};
    use jiff::Timestamp;

    use super::{
        Conversion, LossDelta, Rejection, Rejections, SensorRound, WIRE_ROUND_BUDGET,
        argv_not_collected_marker, convert_wire_event, degraded_events, handle_loss_delta,
        invalid_baseline, per_handle_timeout,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    fn task() -> TaskIdentity {
        TaskIdentity {
            tgid: 10,
            pid: 11,
            ppid: 9,
            uid: 1_000,
            cgroup_id: 77,
        }
    }

    fn timed(event_type: WireEventType, flags: u8, payload: Vec<u8>) -> TimedWireEvent {
        TimedWireEvent {
            event: WireEvent {
                event_type,
                flags,
                ktime_ns: 42,
                sequence: 7,
                task: task(),
                payload,
            },
            observed_at: Timestamp::UNIX_EPOCH,
            time_confidence: TimeConfidence::Measured,
        }
    }

    /// v1-Exec-Payload: `comm[16] | path_len:u16-le | path`.
    fn exec_payload(path: &[u8]) -> TestResult<Vec<u8>> {
        let mut payload = vec![0u8; 16];
        payload[..4].copy_from_slice(b"sshd");
        let len = u16::try_from(path.len()).map_err(ctx("test path fits in u16"))?;
        payload.extend_from_slice(&len.to_le_bytes());
        payload.extend_from_slice(path);
        Ok(payload)
    }

    /// v1-TcpConnect-Payload: `family:u8 | port:u16-be | address:[u8;16]`.
    fn tcp_v4_payload(addr: [u8; 4], port: u16) -> Vec<u8> {
        let mut payload = vec![4u8];
        payload.extend_from_slice(&port.to_be_bytes());
        let mut address = [0u8; 16];
        address[..4].copy_from_slice(&addr);
        payload.extend_from_slice(&address);
        payload
    }

    fn sensor() -> SensorId {
        SensorId::from_str("probe-bpf-test-0")
    }

    fn scope_allowing_10_0_0_0_24() -> TestResult<NetworkScope> {
        let cidr: ipnet::IpNet = "10.0.0.0/24"
            .parse()
            .map_err(ctx("valid test CIDR literal"))?;
        Ok(NetworkScope::from_targets([EgressTarget::Cidr(cidr)]))
    }

    #[test]
    fn test_captured_exec_becomes_process_exec_with_argv_marker() -> TestResult {
        let record = timed(WireEventType::Exec, 0, exec_payload(b"/usr/sbin/sshd")?);
        let Conversion::Report(event) = convert_wire_event(&record, &sensor(), None) else {
            return Err(TestError::Unexpected(
                "a fully captured exec path must be reported".into(),
            ));
        };
        assert_eq!(event.sensor, sensor());
        assert_eq!(event.observed_at, Timestamp::UNIX_EPOCH);
        assert_eq!(
            event.actor,
            Some(Actor {
                uid: 1_000,
                auid: None,
                cgroup: None
            })
        );
        assert_eq!(
            event.kind,
            EventKind::ProcessExec {
                path: "/usr/sbin/sshd".to_owned(),
                argv_digest: argv_not_collected_marker(),
            }
        );
        Ok(())
    }

    #[test]
    fn test_argv_marker_is_stable_and_not_the_digest_of_an_empty_argv() {
        assert_eq!(argv_not_collected_marker(), argv_not_collected_marker());
        assert_ne!(argv_not_collected_marker(), ContentDigest::of(b""));
    }

    #[test]
    fn test_exec_without_readable_path_is_rejected_not_reported_as_empty_path() -> TestResult {
        let unavailable = timed(
            WireEventType::Exec,
            EXEC_PATH_UNAVAILABLE,
            exec_payload(b"")?,
        );
        assert_eq!(
            convert_wire_event(&unavailable, &sensor(), None),
            Conversion::Rejected(Rejection::ExecPathUnavailable)
        );
        let empty = timed(WireEventType::Exec, 0, exec_payload(b"")?);
        assert_eq!(
            convert_wire_event(&empty, &sensor(), None),
            Conversion::Rejected(Rejection::ExecPathUnavailable)
        );
        Ok(())
    }

    #[test]
    fn test_possibly_truncated_exec_path_is_rejected() -> TestResult {
        let record = timed(
            WireEventType::Exec,
            EXEC_PATH_TRUNCATED,
            exec_payload(b"/usr/bin/ss")?,
        );
        assert_eq!(
            convert_wire_event(&record, &sensor(), None),
            Conversion::Rejected(Rejection::ExecPathTruncated)
        );
        Ok(())
    }

    #[test]
    fn test_malformed_exec_payload_is_rejected_as_malformed() {
        let record = timed(WireEventType::Exec, 0, vec![0u8; 3]);
        assert_eq!(
            convert_wire_event(&record, &sensor(), None),
            Conversion::Rejected(Rejection::Malformed)
        );
    }

    #[test]
    fn test_process_exit_is_valid_but_not_reportable() {
        let record = timed(WireEventType::ProcessExit, 0, Vec::new());
        assert_eq!(
            convert_wire_event(&record, &sensor(), None),
            Conversion::NotReportable
        );
        let malformed = timed(WireEventType::ProcessExit, 0, vec![1]);
        assert_eq!(
            convert_wire_event(&malformed, &sensor(), None),
            Conversion::Rejected(Rejection::Malformed)
        );
    }

    #[test]
    fn test_tcp_connect_outside_scope_is_reported_as_egress_flow() -> TestResult {
        let scope = scope_allowing_10_0_0_0_24()?;
        let record = timed(
            WireEventType::TcpConnect,
            0,
            tcp_v4_payload([203, 0, 113, 9], 443),
        );
        let Conversion::Report(event) = convert_wire_event(&record, &sensor(), Some(&scope)) else {
            return Err(TestError::Unexpected(
                "a destination outside the scope must be reported".into(),
            ));
        };
        assert_eq!(
            event.kind,
            EventKind::EgressFlow {
                destination: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)).to_string(),
                port: 443,
            }
        );
        assert_eq!(event.actor.map(|actor| actor.uid), Some(1_000));
        Ok(())
    }

    #[test]
    fn test_tcp_connect_inside_scope_is_not_reported() -> TestResult {
        let scope = scope_allowing_10_0_0_0_24()?;
        let record = timed(
            WireEventType::TcpConnect,
            0,
            tcp_v4_payload([10, 0, 0, 5], 443),
        );
        assert_eq!(
            convert_wire_event(&record, &sensor(), Some(&scope)),
            Conversion::NotReportable
        );
        Ok(())
    }

    #[test]
    fn test_tcp_connect_without_scope_is_always_reported() {
        let record = timed(
            WireEventType::TcpConnect,
            0,
            tcp_v4_payload([10, 0, 0, 5], 443),
        );
        assert!(matches!(
            convert_wire_event(&record, &sensor(), None),
            Conversion::Report(_)
        ));
    }

    #[test]
    fn test_malformed_tcp_connect_is_rejected() {
        let record = timed(WireEventType::TcpConnect, 0, vec![4u8; 5]);
        assert_eq!(
            convert_wire_event(&record, &sensor(), None),
            Conversion::Rejected(Rejection::Malformed)
        );
    }

    #[test]
    fn test_handle_loss_delta_is_saturating_per_kind() {
        let previous = BpfLossCounters {
            exec: 5,
            process_exit: 9,
            tcp_connect: 1,
            invalid_wire_events: 0,
        };
        let current = BpfLossCounters {
            exec: 8,
            process_exit: 3,
            tcp_connect: 1,
            invalid_wire_events: 0,
        };
        let mut invalid_seen = 0;
        let delta = handle_loss_delta(&previous, &current, &mut invalid_seen);
        assert_eq!(
            delta,
            LossDelta {
                exec: 3,
                process_exit: 0,
                tcp_connect: 0,
                invalid_wire_events: 0,
            }
        );
    }

    #[test]
    fn test_loader_wide_invalid_counter_is_accounted_once_across_handles() {
        let previous = BpfLossCounters::default();
        let mut invalid_seen = 5;
        let first = BpfLossCounters {
            invalid_wire_events: 8,
            ..BpfLossCounters::default()
        };
        let second = BpfLossCounters {
            invalid_wire_events: 8,
            ..BpfLossCounters::default()
        };
        let a = handle_loss_delta(&previous, &first, &mut invalid_seen);
        let b = handle_loss_delta(&previous, &second, &mut invalid_seen);
        assert_eq!(a.invalid_wire_events, 3);
        assert_eq!(b.invalid_wire_events, 0);
        assert_eq!(invalid_seen, 8);

        let mut total = LossDelta::default();
        total.accumulate(a);
        total.accumulate(b);
        assert_eq!(total.invalid_wire_events, 3);
    }

    #[test]
    fn test_invalid_baseline_is_the_highest_accounted_value() {
        let last = [
            BpfLossCounters {
                invalid_wire_events: 2,
                ..BpfLossCounters::default()
            },
            BpfLossCounters {
                invalid_wire_events: 7,
                ..BpfLossCounters::default()
            },
        ];
        assert_eq!(invalid_baseline(&last), 7);
        assert_eq!(invalid_baseline(&[]), 0);
    }

    #[test]
    fn test_degraded_events_emit_exactly_one_per_affected_sensor() -> TestResult {
        let procmon = SensorId::from_str("probe-bpf-procmon-0");
        let flow = SensorId::from_str("probe-bpf-flow-0");
        let quiet = SensorId::from_str("probe-bpf-quiet-0");

        let mut lossy = SensorRound::default();
        lossy.loss.exec = 2;
        let mut rejected = SensorRound {
            loss: LossDelta::default(),
            rejections: Rejections::default(),
        };
        rejected.rejections.record(Rejection::ExecPathTruncated);

        let rounds = [
            (&procmon, lossy),
            (&procmon, rejected),
            (&flow, rejected),
            (&quiet, SensorRound::default()),
        ];
        let events = degraded_events(&rounds, Timestamp::UNIX_EPOCH);
        assert_eq!(events.len(), 2);

        let first = events
            .first()
            .ok_or(TestError::Missing("procmon degraded"))?;
        assert_eq!(first.sensor, procmon);
        assert_eq!(first.actor, None);
        assert_eq!(
            first.kind,
            EventKind::SensorDegraded {
                sensor: procmon.clone()
            }
        );
        let second = events.get(1).ok_or(TestError::Missing("flow degraded"))?;
        assert_eq!(second.sensor, flow);
        Ok(())
    }

    #[test]
    fn test_quiet_round_is_not_degraded() {
        assert!(!SensorRound::default().is_degraded());
        assert!(degraded_events(&[], Timestamp::UNIX_EPOCH).is_empty());
    }

    #[test]
    fn test_per_handle_timeout_splits_the_round_budget() {
        assert_eq!(
            per_handle_timeout(WIRE_ROUND_BUDGET, 4),
            Duration::from_millis(50)
        );
        assert_eq!(per_handle_timeout(WIRE_ROUND_BUDGET, 0), WIRE_ROUND_BUDGET);
    }
}
