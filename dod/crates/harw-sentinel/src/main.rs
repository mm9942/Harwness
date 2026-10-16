//! `harw-sentinel`: die unprivilegierte Sammelstelle des Ausbauprogramms
//! (Knoten AW2-19).
//!
//! # Das erste vollständig unprivilegierte Binary
//! Dieses Binary hält **keine einzige erhöhte Fähigkeit**. Jede
//! [`harw_dod_cap::Capability`] sitzt in dem Sensor, der sie beim Aufbau
//! seines [`harw_dod_cap::SensorHandle`] gebunden hat
//! ([`harw_dod_sentinel`]-Moduldoku, Abschnitt „Der Sentinel ist
//! unprivilegiert") — dieser Prozess ruft nur `poll()` auf bereits
//! gebundene `dyn harw_dod_signals::Sensor`-Trait-Objekte und liest
//! nirgends selbst aus `/proc`, `/sys`, einem Netlink-Socket oder einer
//! Datei. [`sensors::build_sensors`] registriert ausschließlich Sensoren,
//! deren [`harw_dod_cap::Capability::class`]
//! `harw_dod_cap::CapabilityClass::Unprivileged` ist — die Abnahme dieses
//! Knotens, siehe `sensors`-Moduldoku und
//! [`sensors::tests::test_all_registered_sensors_are_unprivileged`]. Das
//! ist der Grund, warum Bibliothek ([`harw_dod_sentinel`], Knoten AW2-18)
//! und Binary (dieser Knoten) getrennte Knoten sind: die Bibliothek ist
//! ohne Berechtigungen testbar, dieses Binary trägt den Empfangspfad und
//! die Landlock-Bindung.
//!
//! # IPC-Empfangspfad: `SOCK_SEQPACKET`, nicht ein Ringpuffer
//! Die privilegierten Sonden dieses Programms (`harw-probe-fs`,
//! `harw-probe-bpf`, künftig ein `harw-probe-netlink`) senden ihre
//! Ereignisse über einen `SOCK_SEQPACKET`-Unix-Socket an diesen Prozess
//! (Modul [`ipc`]). Drei Gründe, warum kein Ringpuffer im geteilten
//! Speicher: **Nachrichtengrenzen** (ein `recv()` liefert genau ein
//! Ereignis, keinen Byte-Strom, den dieser Prozess neu zerlegen müsste),
//! **`SO_PEERCRED`** (der Kernel verbürgt, welcher Prozess sendet — ein
//! geteiltes Speichersegment hätte keine eingebaute Herkunftsprüfung), und
//! **keine gemeinsam beschriebene Fläche zwischen Berechtigungsklassen**
//! (ein Ringpuffer im `shm`-Segment wäre eine Fläche, die ein
//! privilegierter Sender und dieser unprivilegierte Leser gemeinsam
//! beschreiben — jeder Größen-/Offsetfehler auf einer Seite wird sofort ein
//! Speicherfehler auf der anderen; ein Unix-Socket hält die Adressräume
//! vollständig getrennt). Siehe [`ipc`]-Moduldoku für die vollständige
//! Begründung — und für den Weg, auf dem ein empfangenes Ereignis seit
//! diesem Umbau zusätzlich den Sentinel-Ringpuffer erreicht (Abschnitt
//! „Externe Ereignisse erreichen jetzt den Ringpuffer" unten).
//!
//! **Push-only:** dieser Prozess **empfängt** von den Sonden, **sendet
//! ihnen aber nie etwas** — eine Sonde mit `CAP_SYS_ADMIN`/`CAP_BPF`, die
//! Anweisungen von einem unprivilegierten Prozess entgegennähme, wäre ein
//! Angriffsziel. [`ipc::IpcListener`] und [`ipc::IpcConnection`] haben
//! deshalb keine `send`/`write`-Methode — eine Typ-Eigenschaft, keine
//! Konvention (siehe [`ipc`]-Moduldoku).
//!
//! # Landlock: Degradation statt Startfehler
//! Dieser Prozess beschränkt sich beim Start über
//! [Landlock](https://docs.kernel.org/userspace-api/landlock.html) auf
//! seinen tatsächlichen Lese-/Schreibbereich (Modul [`sandbox`]). Anders
//! als die drei privilegierten Binaries dieses Programms, die ohne ihre
//! jeweilige Kernelfähigkeit hart abbrechen, **degradiert** dieser Prozess
//! bei fehlender Landlock-Unterstützung nur (`SandboxOutcome::NotEnforced`
//! oder `Failed`) — er läuft ohne die zusätzliche Schranke weiter, statt
//! sich selbst abzuschalten. Ein Sicherheitssammler, der auf einem Host
//! ohne Landlock-Unterstützung gar nicht erst startet, verliert auf genau
//! diesem Host jede Beobachtung, die er sonst geliefert hätte — der
//! teurere Fehler. Siehe [`sandbox`]-Moduldoku für die vollständige
//! Begründung. Das resultierende `EventKind::SensorDegraded`-Ereignis war
//! lange nur über `tracing` beobachtbar; seit diesem Umbau erreicht es
//! zusätzlich `Sentinel::buffer()` (Abschnitt „Externe Ereignisse erreichen
//! jetzt den Ringpuffer" unten).
//!
//! # Externe Ereignisse erreichen jetzt den Ringpuffer
//! Zwei Ereignisquellen entstanden bislang außerhalb von
//! [`harw_dod_sentinel::Sentinel::poll_all`] und erreichten
//! `Sentinel::buffer()` deshalb nie: die über [`ipc`] empfangenen
//! `SecurityEvent`s der privilegierten Sonden, und das
//! `EventKind::SensorDegraded`-Ereignis dieses Binaries selbst, falls seine
//! Landlock-Selbstbeschränkung beim Start degradiert (siehe oben). Beide
//! nutzen jetzt [`harw_dod_sentinel::Sentinel::record_external_event`] —
//! den zweiten, dafür vorgesehenen Schreibweg in den Puffer, neben
//! `poll_all` (siehe dortige Crate-Moduldoku, Abschnitt „Zwei Schreibwege
//! in den Puffer").
//!
//! ## Wie die IPC-Inbox `run()` erreicht
//! [`spawn_ipc_if_available`] baute die `Arc<Mutex<ipc::IpcInbox>>` bislang
//! rein lokal und ließ sie beim Verlassen der Funktion fallen — der
//! Empfangsthread füllte eine Inbox, die niemand je wieder las. Die Funktion
//! gibt das Handle jetzt als `Option<`[`IpcInboxHandle`]`>` zurück (`None`,
//! wenn das Binden scheitert oder — auf jeder Nicht-Linux-Plattform — der
//! Empfangspfad gar nicht existiert); [`run`] hält dieses Handle und reicht
//! es an jeden [`poll_once`]-Aufruf weiter.
//!
//! ## Reihenfolge im Zyklus (Frage 1)
//! [`poll_once`] ruft zuerst `Sentinel::poll_all` auf, **danach** erst
//! [`drain_external_events`]. Ein in der Inbox wartendes Ereignis landet
//! damit im Puffer **nach** den Sensormessungen desselben Zyklus, nicht
//! davor. Begründung: ein extern gemeldetes Ereignis — eine privilegierte
//! Sonde, die tatsächlich etwas beobachtet hat, oder dieses Binary selbst im
//! Landlock-Fall — ist die seltenere, gezieltere Meldung, während
//! `poll_all` bei jedem Zyklus routinemäßig Samples/Events erzeugt. Bei
//! einem vollen Ereignis-Ring verdrängt jeder weitere Schreibzugriff still
//! den jeweils ältesten Eintrag (siehe Frage 2); würde die Inbox zuerst
//! geleert, wären es exakt diese selteneren, gezielteren externen Ereignisse,
//! die ein nachfolgender `poll_all`-Schwall dieses Zyklus als Erstes
//! verdrängen könnte. Die gewählte Reihenfolge kehrt dieses Risiko um:
//! `poll_all`s routinemäßige Ereignisse stehen näher am Verdrängungsrand.
//!
//! ## Überlauf bei einem vollen Puffer (Frage 2)
//! Die IPC-Inbox fasst [`IPC_INBOX_CAPACITY`] Ereignisse. Übersteigt die
//! Summe aus `poll_all`-Ereignissen und in einem Zyklus entleerten
//! Inbox-Einträgen die konfigurierte Ereignis-Ringkapazität
//! (`SentinelConfig::event_capacity`), verdrängt
//! `Sentinel::record_external_event` still den ältesten Eintrag — **exakt**
//! dasselbe Verhalten wie `poll_all` selbst (siehe dessen Methodendoku in
//! `harw_dod_sentinel`). Das wird hier bewusst hingenommen, statt eine
//! zweite, eigene Kappungsgrenze einzuführen: eine zweite Grenze müsste
//! selbst wieder entscheiden, was sie beim Erreichen verwirft — ein zweites
//! Verdrängungsproblem, kein gelöstes. Ein Schwall aus einer Sonde kann
//! dadurch im Extremfall Sensormessungen desselben Zyklus verdrängen; bei
//! der Standardkapazität (`EvidenceBuffer::DEFAULT_EVENT_CAPACITY` = 4096
//! gegenüber [`IPC_INBOX_CAPACITY`] = 256) ist das praktisch unerreichbar,
//! bleibt aber bei einer bewusst klein konfigurierten Ereignis-Ringkapazität
//! ein Restrisiko — eines, das diese Implementierung nicht stiller macht als
//! jeder andere Schreibzugriff auf denselben Puffer.
//!
//! ## Der Landlock-Fall entsteht vor dem `Sentinel` (Frage 3)
//! [`sandbox::restrict_self`] läuft in [`run`] **vor** `Sentinel::new` — zu
//! diesem Zeitpunkt existiert noch keine Instanz, der ein Ereignis übergeben
//! werden könnte. [`run`] baut das Ereignis trotzdem sofort
//! (`sandbox::landlock_degraded_event`) und hält es in einer lokalen
//! Variable zwischengespeichert; unmittelbar nach `Sentinel::new` — vor dem
//! ersten [`poll_once`]-Aufruf — reicht es es per `record_external_event`
//! nach. Es erscheint dadurch garantiert im allerersten `freeze()`, ohne
//! dass sich die bestehende Startreihenfolge (Socket vor Landlock vor
//! Sensor-Registrierung) ändern musste.
//!
//! ## `tracing::warn!` bleibt zusätzlich stehen (Frage 4)
//! Für beide Ereignisse (IPC wie Landlock) bleibt die vorhandene
//! `tracing`-Meldung unverändert bestehen, **neben** dem neuen Weg in den
//! Puffer. Verschiedene Leser rechtfertigen das: `tracing` bedient den
//! Betreiber, der diesen Prozess in Echtzeit über sein Log beobachtet (etwa
//! über `journalctl -f`, lange bevor der nächste `freeze()` überhaupt
//! zieht); der Ringpuffer-/`freeze()`-Weg bedient das zitierfähige
//! `SecurityEvidence`-Artefakt, das eine spätere, von der Prozesslaufzeit
//! entkoppelte Prüfung liest. Zwei Wege für dieselbe Information sind hier
//! kein Widerspruch, weil kein Leser den anderen ersetzt.
//!
//! ## Warum `Sentinel` nicht von einem `Mutex` umschlossen wird
//! `record_external_event` verlangt `&mut self`, exakt wie `poll_all` — die
//! Bibliothek legt bewusst kein zweites Schutzmodell neben dem von
//! `EvidenceBuffer` an (kein internes Locking, ausschließlich über `&mut
//! self` geschützt). Ein `Mutex<Sentinel>` wäre genau das: ein zweites
//! Schutzmodell, das den Poll-Pfad (diesen Sammelthread) und einen
//! Empfangspfad an einer Stelle serialisierte, an der es niemand erwartet,
//! und zwei Schleifen aneinanderkettete, die heute unabhängig laufen
//! (IPC-Annahme/-Empfang je Verbindung in eigenen Threads, Sammelschleife im
//! Hauptthread). Stattdessen bleibt die Sperre exakt dort, wo sie bereits
//! sitzt: [`ipc::IpcInbox`] hinter ihrem eigenen `Arc<Mutex<_>>`. Der
//! Empfangsthread sperrt nur die Inbox, schreibt sein Ereignis hinein und
//! gibt die Sperre sofort wieder frei; [`drain_external_events`] sperrt
//! dieselbe Inbox erneut, entleert sie **innerhalb dieser einen Sperrspanne**
//! in ein lokales `Vec`, gibt die Sperre frei und übergibt die entleerten
//! Ereignisse danach lock-frei per `&mut self` an den `Sentinel` — aus genau
//! dem Thread, der auch `poll_all` aufruft. `Sentinel` selbst wird damit
//! ausschließlich von diesem einen Thread angefasst; kein zweiter Thread
//! hält je eine Referenz darauf.
//!
//! # Regelauswertung nach jedem `freeze()` (Behebung von Bruch 1)
//! [`poll_once`] ruft nach einem erfolgreichen [`Sentinel::freeze`]
//! zusätzlich [`findings::report_findings`] auf das eingefrorene
//! [`harw_dod_signals::SecurityEvidence`] auf — dieselbe injizierte `now`,
//! kein zweiter Systemuhr-Zugriff. Vorher hing hinter `Sentinel`/
//! `EvidenceBuffer` kein Regelwerk: `harw_dod_rules::run_rules` hatte
//! workspace-weit keinen Produktionsaufrufer, jede Fundstelle lag in der
//! Definition selbst, einem Doctest oder einem `#[cfg(test)]`-Modul. Dieser
//! Sentinel bleibt dabei unprivilegiert und setzt nichts durch: er meldet
//! zertifizierte Befunde (`Finding<RuleChecked>`) über denselben
//! `TelemetrySink`, über den er bereits Sensor-Metriken schreibt — siehe
//! [`findings`]-Moduldoku für die vollständige Begründung, das gemessene
//! Abhängigkeitsgewicht dieser Kante, und was zwischen einem Befund und
//! einer durchgesetzten Aktion weiterhin fehlt (Bruch 2, ausdrücklich nicht
//! Teil dieses Knotens).
//!
//! # DoD-Systemkonfiguration und Poll-Taktung
//! [`main`] lädt vor jedem anderen Startschritt (Sink, Socket, Landlock) den
//! gemeinsamen DoD-Vertrag aus [`harw_dod_config`] — denselben, den auch die
//! BPF-Sonde liest ([`load_dod_settings`]):
//!
//! - **Quelle.** Ein expliziter administrativer Pfad geht über
//!   [`harw_dod_config::load_config`]; ohne ihn gilt ausschließlich der feste
//!   Pfad [`harw_dod_config::SYSTEM_CONFIG_PATH`] über
//!   [`harw_dod_config::load_system_config`]. Die Kommandozeile trägt heute
//!   noch kein `--config`-Flag — [`main`] übergibt deshalb stets `None`.
//! - **Fehlt die Systemkonfiguration** (`ENOENT` auf irgendeinem Pfadglied),
//!   startet dieser Prozess mit den eingebauten Vorgaben und meldet das per
//!   `tracing::warn!` deutlich — dieses Binary ist unprivilegiert und seine
//!   Sensoren hängen an keinem Profil, ein Host ohne DoD-Installation soll
//!   nicht jede Beobachtung verlieren. Es gibt dann **keinen**
//!   Konfigurations-Digest; eine Bereitschaftsaussage gegenüber der Sonde
//!   darf daraus nicht abgeleitet werden.
//! - **Nicht vertrauenswürdig oder ungültig** (fremder Eigentümer,
//!   gruppen-/weltschreibbar, Symlink, Parse-/Schemafehler, fehlendes oder
//!   unbekanntes `active_profile`, sowie jeder Lesefehler eines explizit
//!   angegebenen Pfads) — **fail closed**: [`main`] beendet sich mit
//!   `ExitCode::FAILURE`, bevor irgendein Socket gebunden wird. Ein
//!   Sicherheitssammler, der still mit einer manipulierten oder halb
//!   gültigen Konfiguration liefe, wäre der teurere Fehler als einer, der
//!   gar nicht startet.
//! - **`SentinelConfig`.** Der DoD-Vertrag (Schema-Version 1) trägt heute
//!   kein Feld für Rückversuchspolitik oder Ringkapazitäten;
//!   [`DodSettings::sentinel_config`] liefert deshalb bewusst
//!   `SentinelConfig::default()`, protokolliert aber Profil und Digest, damit
//!   Sentinel und Sonde dieselbe Auflösung belegen.
//! - **Programmweiter Poll-Abstand.** Auch der DoD-Vertrag legt keinen
//!   Poll-Abstand fest. Maßgeblich ist deshalb das, was die Sammelschleife
//!   tatsächlich taktet: `Sentinel::poll_all` hat keine eigene Uhr, sondern
//!   wird von [`run`] je Runde aufgerufen, getrennt durch
//!   `std::thread::sleep(`[`PollTiming::interval`]`)` — und dieser Wert
//!   stammt aus `--interval-secs` (Vorgabe `cli::DEFAULT_INTERVAL_SECS`).
//!   [`PollTiming`] ist die eine Stelle, aus der jedes zeitgebundene
//!   Sensorfenster abgeleitet wird: ein Rückschaufenster ist
//!   `max(Vorgabe, 2 × Poll-Abstand)` ([`PollTiming::lookback_for`]), damit
//!   aufeinanderfolgende Polls sich stets überlappen statt eine Lücke zu
//!   lassen — selbst wenn eine Runde sich um bis zu einen vollen Abstand
//!   verspätet.
//!
//! # Fehler
//! [`error::SentinelBinError`] — ausschließlich die beiden Startpfade ohne
//! Degradationsoption: Root-Space-Auflösung und Öffnen des
//! Telemetrie-Sinks. Ein fehlender Landlock-Support oder ein nicht
//! bindbarer IPC-Socket sind **kein** `Err` dieses Binaries (siehe oben).
//! Eine nicht vertrauenswürdige oder ungültige DoD-Konfiguration beendet den
//! Prozess ebenfalls, wird aber direkt in [`main`] als
//! [`harw_dod_config::ConfigError`] geloggt (siehe oben), weil
//! `SentinelBinError` dafür noch keine Variante trägt.
//!
//! # Examples
//! ```text
//! $ harw-sentinel --log info --home /var/lib/harwness --interval-secs 5
//! $ harw-sentinel --once --home /var/lib/harwness
//! ```

#![forbid(unsafe_code)]

mod cli;
mod error;
mod export;
mod findings;
mod sandbox;
mod sensors;

#[cfg(target_os = "linux")]
mod ipc;

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::Mutex;
use std::time::Duration;

use clap::Parser as _;
use harw_dod_config::{
    Config, ConfigError, ResolvedObservationProfile, SYSTEM_CONFIG_PATH, load_config,
    load_system_config,
};
use harw_dod_sentinel::{Sentinel, SentinelConfig};
use harw_observe::TelemetrySink;
use harw_observe_file::FileSink;
use jiff::Timestamp;

use cli::Cli;
use error::{SentinelBinError, SentinelBinResult};
use sensors::SensorRoots;

/// Größenschwelle für die Rotation der aktiven Telemetrie-Datei.
///
/// Übernommen aus dem Aufrufbeispiel von
/// [`harw_observe_file::FileSink::open`] (`10 * 1024 * 1024`); kein
/// `--flag` dieses Binaries überschreibt sie — sollte sich das als zu
/// klein/groß erweisen, ist das eine spätere, gezielte Änderung, keine
/// Konfigurationsfläche, die dieser Knoten vorwegnehmen soll.
const TELEMETRY_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Wirksame Obergrenze rotierter Telemetrie-Dateien: `--telemetry-max-files`,
/// sonst das Klassenlimit von `telemetry_rotated` (Retention-Vorgabe; dieses
/// Binary liest keine `[retention]`-Konfiguration).
fn telemetry_max_files(flag: Option<u64>) -> Option<usize> {
    let n = flag.or_else(|| {
        harw_retention::policy_for(
            &harw_retention::RetentionConfig::default(),
            "telemetry_rotated",
        )
        .and_then(|class| class.max_files)
    })?;
    usize::try_from(n).ok()
}

/// Kapazität der IPC-Empfangs-Inbox ([`ipc::IpcInbox`]).
#[cfg(target_os = "linux")]
const IPC_INBOX_CAPACITY: usize = 256;

/// Geteiltes Handle auf die IPC-Empfangs-Inbox, wie es
/// [`spawn_ipc_if_available`] zurückgibt und [`poll_once`]/
/// [`drain_external_events`] entgegennehmen.
///
/// Auf Linux die tatsächliche, hinter einem `Mutex` gesperrte
/// [`ipc::IpcInbox`] (siehe Moduldoku, Abschnitt „Warum `Sentinel` nicht von
/// einem `Mutex` umschlossen wird" — die Sperre sitzt hier, nicht um
/// `Sentinel`). Auf jeder anderen Plattform ein unbewohnter Platzhalter:
/// [`ipc`] wird dort gar nicht erst kompiliert, also kann dieser Typ dort
/// nie einen Wert tragen — jeder Aufrufer erhält von
/// [`spawn_ipc_if_available`] dort stets `None`.
#[cfg(target_os = "linux")]
type IpcInboxHandle = Arc<Mutex<ipc::IpcInbox>>;

/// Siehe die Linux-Fassung.
#[cfg(not(target_os = "linux"))]
type IpcInboxHandle = ();

/// Faktor zwischen Poll-Abstand und Mindest-Rückschaufenster eines Sensors.
///
/// Zwei volle Abstände: ein Fenster von genau einem Abstand überlappt nur,
/// solange jede Runde pünktlich läuft; verspätet sich eine Runde (langsamer
/// Sensor, Last), entstünde eine Lücke. Der doppelte Abstand toleriert eine
/// Verspätung bis zu einem vollen weiteren Abstand, ohne ein Ereignis zu
/// verpassen — Doppelzählungen im Überlappungsbereich sind der billigere
/// Fehler (siehe `harw_dod_authlog::sensor`-Moduldoku, Abschnitt „woher kommt
/// `since`?").
const LOOKBACK_POLL_FACTOR: u32 = 2;

/// Der programmweite Poll-Abstand dieses Prozesses.
///
/// # Description
/// Weder `harw_dod_config::Config` noch `SentinelConfig` kennen einen
/// Poll-Abstand: `Sentinel::poll_all` hat keine eigene Uhr, sondern wird
/// von [`run`] aufgerufen, getrennt durch `std::thread::sleep(interval)`.
/// Genau dieser Schlafabstand (`--interval-secs`) ist deshalb der
/// maßgebliche Poll-Abstand, und dieser Typ ist die einzige Stelle, aus der
/// zeitgebundene Sensorfenster abgeleitet werden (siehe Moduldoku, Abschnitt
/// „DoD-Systemkonfiguration und Poll-Taktung").
///
/// # Concurrency
/// Reiner Werttyp, `Copy + Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PollTiming {
    interval: Duration,
}

impl PollTiming {
    /// Baut die Taktung aus dem über `--interval-secs` gewählten Wert.
    ///
    /// # Arguments
    /// - `secs` (`u64`): Sekunden zwischen zwei Poll-Runden.
    #[must_use]
    const fn from_interval_secs(secs: u64) -> Self {
        Self {
            interval: Duration::from_secs(secs),
        }
    }

    /// Abstand zwischen zwei Poll-Runden — exakt der Wert, den die
    /// Dauerschleife in [`run`] schläft.
    #[must_use]
    const fn interval(&self) -> Duration {
        self.interval
    }

    /// Effektives Rückschaufenster eines Sensors mit Vorgabe `default`.
    ///
    /// # Description
    /// `max(default, LOOKBACK_POLL_FACTOR × interval)` — ein Sensor mit
    /// großzügiger Vorgabe behält sie, ein zu knappes Fenster wird auf den
    /// doppelten Poll-Abstand angehoben (siehe [`LOOKBACK_POLL_FACTOR`]).
    /// Gedacht für `with_lookback`-Konstruktoren wie
    /// `harw_dod_authlog::AuthlogSensor::with_lookback`. Die Multiplikation
    /// sättigt, statt bei einem absurd großen `--interval-secs` überzulaufen.
    ///
    /// **Nicht** für Lese-Timeouts (`with_timeout` von `harw-dod-procmon`/
    /// `harw-dod-flow`): ein Timeout blockiert die Runde und muss **unter**
    /// dem Poll-Abstand bleiben, nicht darüber.
    ///
    /// # Arguments
    /// - `default` (`std::time::Duration`): die Vorgabe des Sensors.
    #[must_use]
    fn lookback_for(&self, default: Duration) -> Duration {
        default.max(self.interval.saturating_mul(LOOKBACK_POLL_FACTOR))
    }
}

/// Woher die geladenen DoD-Einstellungen stammen.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigOrigin {
    /// Ein explizit übergebener administrativer Pfad
    /// ([`harw_dod_config::load_config`]).
    Explicit(std::path::PathBuf),
    /// Der feste Systempfad [`harw_dod_config::SYSTEM_CONFIG_PATH`].
    System,
    /// Der feste Systempfad existiert nicht; eingebaute Vorgaben ohne
    /// Profil und ohne Digest.
    BuiltinDefaults,
}

impl ConfigOrigin {
    /// Der gelesene Pfad, oder `None` bei eingebauten Vorgaben.
    #[must_use]
    fn source_path(&self) -> Option<&Path> {
        match self {
            Self::Explicit(path) => Some(path),
            Self::System => Some(Path::new(SYSTEM_CONFIG_PATH)),
            Self::BuiltinDefaults => None,
        }
    }
}

/// Die für diesen Lauf aufgelösten DoD-Einstellungen.
///
/// # Description
/// Trägt das aktive Beobachtungsprofil (oder `None` im Fall
/// [`ConfigOrigin::BuiltinDefaults`]) und leitet daraus die
/// `SentinelConfig` ab. Wird in [`main`] vor jedem anderen Startschritt
/// gebaut (siehe Moduldoku).
#[derive(Debug, Clone)]
struct DodSettings {
    origin: ConfigOrigin,
    profile: Option<ResolvedObservationProfile>,
}

impl DodSettings {
    /// Eingebaute Vorgaben für einen Host ohne Systemkonfiguration.
    #[must_use]
    const fn builtin_defaults() -> Self {
        Self {
            origin: ConfigOrigin::BuiltinDefaults,
            profile: None,
        }
    }

    /// Die aus dem DoD-Vertrag abgeleitete `SentinelConfig`.
    ///
    /// # Description
    /// Schema-Version 1 des DoD-Vertrags trägt weder Rückversuchspolitik noch
    /// Ringkapazitäten — jede Profilauflösung ergibt deshalb bewusst
    /// `SentinelConfig::default()`. Erhält der Vertrag solche Felder, ist
    /// dies die eine Stelle, an der sie übernommen werden.
    #[must_use]
    fn sentinel_config(&self) -> SentinelConfig {
        SentinelConfig::default()
    }
}

/// Lädt und löst die DoD-Konfiguration für diesen Lauf auf.
///
/// # Description
/// `explicit = Some(path)` liest über [`harw_dod_config::load_config`]
/// (jeder Fehler, auch eine fehlende Datei, ist fail closed — der
/// Administrator hat diesen Pfad ausdrücklich verlangt). `None` liest den
/// festen Systempfad über [`harw_dod_config::load_system_config`]; nur dort
/// führt eine fehlende Datei zu [`DodSettings::builtin_defaults`] mit einer
/// deutlichen Warnung. Siehe [`settings_from_loaded`].
///
/// # Errors
/// Jeder [`harw_dod_config::ConfigError`] außer einer fehlenden
/// Systemkonfiguration.
fn load_dod_settings(explicit: Option<&Path>) -> Result<DodSettings, ConfigError> {
    match explicit {
        Some(path) => settings_from_loaded(
            load_config(path),
            ConfigOrigin::Explicit(path.to_path_buf()),
        ),
        None => settings_from_loaded(load_system_config(), ConfigOrigin::System),
    }
}

/// Wandelt ein Ladeergebnis in [`DodSettings`] um — getrennt von
/// [`load_dod_settings`], damit die Fallback-/Fail-closed-Entscheidung ohne
/// Zugriff auf `/etc` testbar ist.
///
/// # Arguments
/// - `loaded` (`Result<harw_dod_config::Config, harw_dod_config::ConfigError>`):
///   das Ergebnis von `load_config`/`load_system_config`.
/// - `origin` (`ConfigOrigin`): die gelesene Quelle; nur
///   [`ConfigOrigin::System`] kennt den Fallback auf Vorgaben.
///
/// # Errors
/// Jeder Lese-, Vertrauens-, Parse- oder Auflösungsfehler, außer `NotFound`
/// beim festen Systempfad. Ein fehlendes oder unbekanntes `active_profile`
/// ist ebenfalls ein Fehler — `resolve_active` fällt nie auf ein anderes
/// Profil zurück, und dieser Prozess auch nicht.
fn settings_from_loaded(
    loaded: Result<Config, ConfigError>,
    origin: ConfigOrigin,
) -> Result<DodSettings, ConfigError> {
    match loaded {
        Ok(config) => {
            let profile = config.resolve_active()?;
            Ok(DodSettings {
                origin,
                profile: Some(profile),
            })
        }
        Err(ConfigError::Io(error))
            if origin == ConfigOrigin::System && error.kind() == std::io::ErrorKind::NotFound =>
        {
            tracing::warn!(
                path = SYSTEM_CONFIG_PATH,
                error = %error,
                "DoD system configuration not found; running with built-in defaults \
                 (no active profile, no config digest -- probe readiness cannot be matched)"
            );
            Ok(DodSettings::builtin_defaults())
        }
        Err(error) => Err(error),
    }
}

/// Protokolliert Herkunft, Profil und Digest der geladenen Einstellungen.
///
/// Sentinel und Sonde müssen denselben Digest melden (siehe
/// `harw_dod_config::ResolvedObservationProfile`); diese Zeile ist der
/// Sentinel-Anteil dieses Abgleichs.
fn log_dod_settings(settings: &DodSettings, timing: PollTiming) {
    let sentinel_config = settings.sentinel_config();
    match &settings.profile {
        Some(profile) => tracing::info!(
            origin = ?settings.origin,
            source = ?settings.origin.source_path(),
            profile = profile.profile_id().as_str(),
            config_digest = %profile.config_digest(),
            scope = ?profile.scope(),
            sensors = ?profile.sensors(),
            ?sentinel_config,
            poll_interval = ?timing.interval(),
            lookback_floor = ?timing.lookback_for(Duration::ZERO),
            "DoD configuration loaded"
        ),
        None => tracing::warn!(
            origin = ?settings.origin,
            ?sentinel_config,
            poll_interval = ?timing.interval(),
            lookback_floor = ?timing.lookback_for(Duration::ZERO),
            "DoD configuration absent; using built-in defaults"
        ),
    }
}

/// Einstiegspunkt.
///
/// # Description
/// Parst die Kommandozeile ohne `clap::Parser::parse` (das bei einem
/// Fehler oder `--help`/`--version` selbst `std::process::exit` aufriefe
/// und diese Funktion nie zu ihrem eigenen `ExitCode` zurückkehren ließe),
/// initialisiert `tracing`, bedient das Unterkommando `completions`, lädt
/// sonst die DoD-Konfiguration ([`load_dod_settings`], fail closed — siehe
/// Moduldoku) und delegiert an [`run`].
///
/// # Returns
/// `ExitCode::SUCCESS` bei normalem Ende (nur nach `--once` oder
/// `completions`; die Dauerschleife endet sonst nur durch ein Signal).
/// `ExitCode::FAILURE` bei einem CLI-Parse-Fehler, einer nicht
/// vertrauenswürdigen oder ungültigen DoD-Konfiguration oder einem
/// [`error::SentinelBinError`].
fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            if matches!(
                err.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                print!("{err}");
                return ExitCode::SUCCESS;
            }
            eprint!("{err}");
            return ExitCode::FAILURE;
        }
    };

    init_tracing(cli.log);

    // `completions` läuft vor dem Konfigurationsladen und jedem Sink-,
    // Socket- oder Landlock-Schritt: es schreibt nur ein Skript (bzw.
    // installiert es) und beendet sich — ein Host ohne DoD-Konfiguration soll
    // trotzdem Completions erzeugen können.
    let result = if cli.command.is_some() {
        run_completions_command(&cli)
    } else {
        // `--config` wählt einen expliziten, vertrauten Pfad; ohne ihn gilt
        // ausschließlich der feste Systempfad.
        let settings = match load_dod_settings(cli.config.as_deref()) {
            Ok(settings) => settings,
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "refusing to start: DoD configuration is untrusted or invalid"
                );
                return ExitCode::FAILURE;
            }
        };
        run(cli, settings)
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "harw-sentinel exiting");
            ExitCode::FAILURE
        }
    }
}

/// Bedient das Unterkommando `completions`.
///
/// # Returns
/// `Ok(())` nach geschriebenem/installiertem Skript, ebenso ohne
/// `completions`-Unterkommando (No-Op).
///
/// # Errors
/// [`error::SentinelBinError::Completions`].
fn run_completions_command(cli: &Cli) -> SentinelBinResult<()> {
    let Some(harw_completions::CompletionsSubcommand::Completions(args)) = &cli.command else {
        return Ok(());
    };
    harw_completions::run_completions(
        &mut <Cli as clap::CommandFactory>::command(),
        "harw-sentinel",
        args,
        &harw_completions::HomeEnv::from_process(),
        &mut std::io::stdout().lock(),
    )?;
    Ok(())
}

/// Initialisiert den globalen `tracing`-Subscriber.
///
/// # Description
/// Muss genau einmal aufgerufen werden, als erste Anweisung nach dem
/// Parsen der Kommandozeile. Anders als `harw-cli::init_tracing` gibt es
/// hier keine interaktive Alternate-Screen-Rücksicht (dieses Binary ist ein
/// Hintergrunddienst, keine TUI) — die angeforderte Stufe wird unverändert
/// verwendet, nie stillschweigend auf `warn` reduziert.
///
/// # Arguments
/// - `level` (`cli::LogLevel`): die über `--log` angeforderte Stufe. Ein
///   ungültiger Rohwert erreicht diese Funktion nie — `clap` hat ihn bereits
///   beim Parsen abgelehnt (siehe [`cli::LogLevel`]-Moduldoku).
///
/// # Panics
/// Panics, falls bereits ein globaler Subscriber installiert ist (nur
/// erreichbar, wenn diese Funktion zweimal aufgerufen wird — ein
/// Programmierfehler).
fn init_tracing(level: cli::LogLevel) {
    use tracing_subscriber::EnvFilter;

    let filter =
        EnvFilter::try_new(level.as_filter_directive()).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

/// Führt den eigentlichen Sammelbetrieb aus.
///
/// # Description
/// Reihenfolge: Root-Space auflösen, Telemetrie-Sink öffnen (legt dabei
/// `harw_home::paths::telemetry_dir(home)` und damit auch `home` selbst
/// an, über `FileSink::open`s eigenes `create_dir_all` — diese Funktion
/// legt kein Verzeichnis ein zweites Mal an), den IPC-Empfangssocket binden
/// (best effort — ein Fehlschlag degradiert, siehe unten), **dann erst**
/// über Landlock selbst einschränken (nach allen Schritten, die noch neue
/// Dateien/Sockets öffnen müssen — bereits offene Deskriptoren bleiben nach
/// einer Landlock-Regel benutzbar, neue Pfade außerhalb der Regel nicht
/// mehr), Sensoren registrieren, dann entweder eine einzelne Runde
/// (`--once`) oder die Dauerschleife.
///
/// Der IPC-Empfangssocket wird dabei nicht nur gebunden, sondern sein
/// Inbox-Handle auch zurückgehalten — [`poll_once`] entleert es jeden
/// Zyklus in den `Sentinel`-Puffer (siehe Moduldoku, Abschnitt „Externe
/// Ereignisse erreichen jetzt den Ringpuffer"). Degradiert die
/// Landlock-Selbstbeschränkung, wird das resultierende Ereignis lokal
/// zwischengespeichert und unmittelbar nach `Sentinel::new` — vor dem
/// ersten Poll-Zyklus — in den Puffer nachgereicht, weil zum Zeitpunkt des
/// Selbstbeschränkungsversuchs noch kein `Sentinel` existiert.
///
/// Die `SentinelConfig` stammt aus `settings` ([`DodSettings::sentinel_config`]),
/// der Schlafabstand der Dauerschleife aus [`PollTiming`] — derselbe Wert,
/// aus dem Sensor-Rückschaufenster abgeleitet werden (siehe Moduldoku,
/// Abschnitt „DoD-Systemkonfiguration und Poll-Taktung").
///
/// # Arguments
/// - `cli` (`cli::Cli`): die geparste Kommandozeile.
/// - `settings` (`DodSettings`): die bereits in [`main`] geladene und
///   aufgelöste DoD-Konfiguration.
///
/// # Errors
/// [`error::SentinelBinError::Home`], [`error::SentinelBinError::Sink`] —
/// siehe dortige Dokumentation. Ein nicht bindbarer IPC-Socket oder eine
/// fehlende Landlock-Unterstützung sind **kein** `Err` (siehe
/// Moduldoku).
fn run(cli: Cli, settings: DodSettings) -> SentinelBinResult<()> {
    let timing = PollTiming::from_interval_secs(cli.interval_secs);
    log_dod_settings(&settings, timing);

    let home = match cli.home.clone() {
        Some(path) => path,
        None => harw_home::paths::home_dir().map_err(SentinelBinError::from)?,
    };

    let telemetry_dir = harw_home::paths::telemetry_dir(&home);
    let sink: Arc<dyn TelemetrySink> = Arc::new(
        FileSink::open(&telemetry_dir, TELEMETRY_MAX_BYTES)
            .map_err(SentinelBinError::from)?
            .with_max_rotated_files(telemetry_max_files(cli.telemetry_max_files)),
    );
    tracing::info!(dir = %telemetry_dir.display(), "telemetry sink opened");

    let socket_path = cli
        .socket
        .clone()
        .unwrap_or_else(|| home.join(cli::DEFAULT_SOCKET_FILE_NAME));
    let inbox = spawn_ipc_if_available(&socket_path);

    // Optionaler Befund-Export (`--findings-export`): vor Landlock öffnen,
    // damit ein Konfigurationsfehler sofort im Log steht; das Verzeichnis
    // bleibt danach für Rotation/Neuanlage beschreibbar (Regel unten).
    let mut exporter = build_findings_exporter(&cli, sink.as_ref());

    let mut roots = sandbox::default_roots(
        &cli.proc_root,
        &cli.thermal_root,
        &cli.workspace_root,
        &cli.blockio_root,
        &cli.gpu_root,
        &cli.cgroup_root,
        &home,
    );
    if let Some(dir) = exporter
        .as_ref()
        .and_then(export::FindingsExporter::directory)
    {
        roots.push(sandbox::SandboxRoot::read_write(dir.to_path_buf()));
    }
    let outcome = sandbox::restrict_self(&roots);
    // `Sentinel` existiert an dieser Stelle noch nicht (siehe unten) — ein
    // degradiertes Ergebnis wird deshalb zwischengespeichert, statt sofort
    // per `record_external_event` übergeben zu werden (Frage 3 der
    // Moduldoku, Abschnitt „Externe Ereignisse erreichen jetzt den
    // Ringpuffer"). Das `tracing::warn!` bleibt zusätzlich stehen (Frage 4
    // ebenda).
    let landlock_event = if outcome.is_degraded() {
        let event = sandbox::landlock_degraded_event(Timestamp::now());
        tracing::warn!(
            ?event,
            ?outcome,
            "running without landlock self-restriction"
        );
        Some(event)
    } else {
        tracing::info!(?outcome, "landlock self-restriction applied");
        None
    };

    let sensor_roots = SensorRoots {
        proc_root: cli.proc_root.clone(),
        thermal_root: cli.thermal_root.clone(),
        workspace_root: cli.workspace_root.clone(),
        blockio_root: cli.blockio_root.clone(),
        gpu_root: cli.gpu_root.clone(),
        cgroup_root: cli.cgroup_root.clone(),
        home: home.clone(),
    };
    // Keiner der hier registrierten Sensoren hat heute ein Rückschaufenster
    // (`with_lookback`); sobald `harw-dod-authlog` o. Ä. hier gebaut wird,
    // erhält er `timing.lookback_for(<Vorgabe>)` — siehe `PollTiming`.
    let sensor_list = sensors::build_sensors(&sensor_roots);
    tracing::info!(
        sensor_count = sensor_list.len(),
        all_unprivileged = sensors::all_unprivileged(&sensor_list),
        "sensors registered"
    );

    let mut sentinel = Sentinel::new(sensor_list, Arc::clone(&sink), settings.sentinel_config());

    // Der zwischengespeicherte Landlock-Befund wird hier nachgereicht — vor
    // dem ersten Poll-Zyklus, direkt nachdem der `Sentinel` überhaupt zum
    // ersten Mal existiert. Damit erscheint er garantiert im allerersten
    // `freeze()`, statt bei einem verpassten ersten Zyklus verlorenzugehen.
    if let Some(event) = landlock_event {
        sentinel.record_external_event(event);
    }

    if cli.once {
        poll_once(
            &mut sentinel,
            inbox.as_ref(),
            sink.as_ref(),
            exporter.as_mut(),
        );
        return Ok(());
    }

    loop {
        poll_once(
            &mut sentinel,
            inbox.as_ref(),
            sink.as_ref(),
            exporter.as_mut(),
        );
        std::thread::sleep(timing.interval());
    }
}

/// Führt genau eine Poll-Runde aus, loggt sie und friert den Puffer ein.
///
/// # Description
/// `now` wird genau hier gelesen — dies ist der Kompositionswurzel-Ort, der
/// laut `harw_dod_sentinel`-Moduldoku die Systemuhr lesen darf; jede
/// aufgerufene Bibliothekscrate bekommt `now` nur injiziert.
///
/// Reihenfolge (Frage 1 der Moduldoku, Abschnitt „Externe Ereignisse
/// erreichen jetzt den Ringpuffer"): zuerst [`Sentinel::poll_all`], **dann**
/// [`drain_external_events`] — ein zum Aufrufzeitpunkt in der Inbox
/// wartendes Ereignis landet damit nach den Sensormessungen desselben
/// Zyklus im Puffer, nicht davor. Beide Schritte laufen vor [`Sentinel::freeze`],
/// sodass ein danach gezogener Beleg immer beide Quellen dieses Zyklus
/// enthält. Erst nach einem erfolgreichen `freeze()` läuft
/// [`findings::report_findings`] gegen das eingefrorene
/// [`harw_dod_signals::SecurityEvidence`] — siehe Moduldoku, Abschnitt
/// „Regelauswertung nach jedem `freeze()`", und [`findings`]-Moduldoku für
/// die vollständige Begründung.
///
/// # Arguments
/// - `sentinel` (`&mut harw_dod_sentinel::Sentinel`): die Sammelstelle.
/// - `inbox` (`Option<&`[`IpcInboxHandle`]`>`): das Handle der
///   IPC-Empfangs-Inbox, falls der Empfangspfad auf dieser Plattform
///   existiert und der Socket gebunden werden konnte (`None` sonst — dann
///   ist dieser Poll-Zyklus rein sensor-basiert).
/// - `sink` (`&dyn harw_observe::TelemetrySink`): dasselbe Sink-Objekt, das
///   [`run`] beim Start geöffnet hat — der Ausgabeweg für gemeldete Befunde
///   (siehe [`findings`]-Moduldoku).
/// - `exporter` (`Option<&mut export::FindingsExporter>`): der optionale
///   JSON-Lines-Befundexport (`--findings-export`); `None` exportiert nichts.
fn poll_once(
    sentinel: &mut Sentinel,
    inbox: Option<&IpcInboxHandle>,
    sink: &dyn TelemetrySink,
    exporter: Option<&mut export::FindingsExporter>,
) {
    let now = Timestamp::now();
    let reading = sentinel.poll_all(now);
    tracing::debug!(
        samples = reading.samples.len(),
        events = reading.events.len(),
        degraded_sensors = sentinel.degraded_count(),
        "poll cycle complete"
    );
    for event in &reading.events {
        tracing::warn!(?event, "security event observed");
    }

    drain_external_events(sentinel, inbox);

    match sentinel.freeze(now) {
        Ok(evidence) => {
            tracing::debug!(
                samples = evidence.samples.len(),
                events = evidence.events.len(),
                "buffer frozen into evidence"
            );
            let reported = findings::report_findings(sink, &evidence, now, exporter);
            tracing::debug!(
                reported,
                "security rules evaluated against this cycle's evidence"
            );
        }
        Err(error) => {
            tracing::warn!(error = %error, "failed to freeze buffer into evidence this round");
        }
    }
}

/// Baut den optionalen Befund-Export aus `--findings-export`/`--findings-host`.
///
/// # Description
/// Ohne `--findings-export` `None` — nichts wird exportiert. Mit dem Flag
/// wird das `host`-Feld über [`export::resolve_host`] bestimmt; ist es
/// weder angegeben noch aus [`export::HOSTNAME_PATH`] lesbar, bleibt der
/// Export mit einer Warnung aus (ein falscher `host` wäre für den Hub
/// wertlos). Sonst wird die Datei über
/// [`export::FindingsExporter::prepare`] vorab geöffnet; ein Fehler dabei
/// wird gezählt und geloggt, beendet den Prozess aber nie.
fn build_findings_exporter(
    cli: &Cli,
    sink: &dyn TelemetrySink,
) -> Option<export::FindingsExporter> {
    let path = cli.findings_export.clone()?;
    let Some(host) =
        export::resolve_host(cli.findings_host.as_ref(), Path::new(export::HOSTNAME_PATH))
    else {
        tracing::warn!(
            path = %path.display(),
            "findings export disabled: no --findings-host and the kernel hostname is unavailable"
        );
        return None;
    };
    let mut exporter =
        export::FindingsExporter::new(path, host).with_keep(cli.findings_export_keep);
    exporter.prepare(sink);
    tracing::info!(
        path = %exporter.path().display(),
        max_bytes = export::MAX_EXPORT_BYTES,
        keep = cli.findings_export_keep,
        "findings export enabled"
    );
    Some(exporter)
}

/// Entleert die IPC-Empfangs-Inbox in [`Sentinel::buffer`], über den
/// zweiten Schreibweg [`Sentinel::record_external_event`].
///
/// # Description
/// Sperrt `inbox` genau einmal, entleert sie **innerhalb dieser einen
/// Sperrspanne** in ein lokales `Vec` (über [`ipc::IpcInbox::drain`]) und
/// gibt die Sperre danach sofort wieder frei, bevor `sentinel` überhaupt
/// berührt wird — siehe Moduldoku, Abschnitt „Warum `Sentinel` nicht von
/// einem `Mutex` umschlossen wird", für die Begründung, warum die Sperre
/// hier endet, statt auf `Sentinel` ausgedehnt zu werden. Ein vergifteter
/// Mutex (nur erreichbar, wenn ein Empfangsthread mit gehaltener Sperre
/// paniert) wird wiederhergestellt statt die wartenden Ereignisse
/// stillschweigend zu verwerfen — der zugrunde liegende `VecDeque`-Zustand
/// bleibt in diesem Fall unversehrt, nur die Panik-Herkunft ist unbekannt.
///
/// `inbox == None` (kein Empfangspfad auf dieser Plattform, oder das Binden
/// des Sockets ist beim Start gescheitert) ist ein No-Op, kein Fehler.
///
/// # Arguments
/// - `sentinel` (`&mut harw_dod_sentinel::Sentinel`): Ziel jedes entleerten
///   Ereignisses.
/// - `inbox` (`Option<&`[`IpcInboxHandle`]`>`): siehe [`poll_once`].
///
/// # Concurrency
/// Läuft ausschließlich aus dem Thread, der auch [`poll_once`]/`poll_all`
/// aufruft (siehe Moduldoku). Der einzige gesperrte Abschnitt ist der
/// `inbox.lock()`-Aufruf selbst; `sentinel.record_external_event` läuft
/// danach lock-frei.
#[cfg(target_os = "linux")]
fn drain_external_events(sentinel: &mut Sentinel, inbox: Option<&IpcInboxHandle>) {
    let Some(inbox) = inbox else {
        return;
    };

    let received = {
        let mut guard = match inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!(
                    "ipc inbox mutex poisoned by a panicked thread; recovering its contents"
                );
                poisoned.into_inner()
            }
        };
        // Der häufigste Fall: nichts angekommen. Dann gar nicht erst
        // entleeren -- `drain()` legt sonst je Zyklus einen leeren `Vec` an.
        if guard.is_empty() && guard.dropped() == 0 {
            return;
        }
        // Auslastung und Verdrängung ablesen, solange die Sperre noch
        // gehalten wird -- danach ist die Inbox leer und beides wäre weg.
        let held = guard.len();
        let capacity = guard.capacity();
        let dropped = guard.dropped();
        let drained = guard.drain();
        (drained, held, capacity, dropped)
    };
    let (received, held, capacity, dropped) = received;

    if !received.is_empty() {
        tracing::debug!(
            held,
            capacity,
            "draining externally received events from the ipc inbox"
        );
    }
    // Erwartet wird null. Jede Verdrängung heißt, dass eine privilegierte
    // Sonde etwas beobachtet hat, das nie im Ringpuffer ankam -- und ein
    // `freeze()` danach ist unvollständig, ohne dass irgendetwas rot wird.
    if dropped > 0 {
        tracing::warn!(
            dropped,
            capacity,
            "ipc inbox overflowed; observations from a privileged probe were lost"
        );
    }

    for item in received {
        tracing::debug!(
            sensor = %item.event.sensor.as_str(),
            peer_pid = item.peer.pid,
            "recording externally received security event into the evidence buffer"
        );
        sentinel.record_external_event(item.event);
    }
}

/// Siehe die Linux-Fassung. `inbox` ist auf dieser Plattform stets `None`
/// (siehe [`IpcInboxHandle`], [`spawn_ipc_if_available`]) — diese Funktion
/// ist entsprechend ein reines No-Op.
#[cfg(not(target_os = "linux"))]
fn drain_external_events(_sentinel: &mut Sentinel, _inbox: Option<&IpcInboxHandle>) {}

/// Bindet den IPC-Empfangssocket, falls möglich, startet seine
/// Annahmeschleife im Hintergrund und gibt das Inbox-Handle zurück.
///
/// # Description
/// Best effort: ein Fehlschlag wird geloggt, degradiert diesen Prozess
/// aber nicht — die Sammelschleife läuft unverändert weiter, nur ohne
/// externe Ereignisse aus dieser Quelle. Anders als zuvor bleibt die
/// `Arc<Mutex<ipc::IpcInbox>>` nicht mehr lokal in dieser Funktion
/// gefangen: sie wird zurückgegeben, damit [`run`] sie an [`poll_once`]
/// weiterreichen kann (siehe Moduldoku, Abschnitt „Wie die IPC-Inbox
/// `run()` erreicht").
///
/// # Arguments
/// - `socket_path` (`&std::path::Path`): Zielpfad des Sockets
///   (`--socket` oder `<home>/`[`cli::DEFAULT_SOCKET_FILE_NAME`]).
///
/// # Returns
/// `Some(inbox)`, wenn der Socket erfolgreich gebunden wurde; `None`, wenn
/// das Binden scheitert. Auf jeder Nicht-Linux-Plattform stets `None` mit
/// einer entsprechenden Logmeldung, weil [`ipc`] dort gar nicht erst
/// kompiliert wird (siehe [`ipc`]-Moduldoku).
#[cfg(target_os = "linux")]
fn spawn_ipc_if_available(socket_path: &std::path::Path) -> Option<IpcInboxHandle> {
    match ipc::IpcListener::bind(socket_path) {
        Ok(listener) => {
            let inbox: IpcInboxHandle =
                Arc::new(Mutex::new(ipc::IpcInbox::new(IPC_INBOX_CAPACITY)));
            // Der Pfad kommt vom Listener, nicht aus `socket_path`: gemeldet
            // wird damit, woran tatsächlich gebunden wurde, nicht was
            // angefordert war. Die beiden können auseinandergehen, und dann
            // ist die maßgebliche Angabe die des Listeners.
            tracing::info!(path = %listener.path().display(), "ipc receive socket bound");
            let _handle = ipc::spawn_receive_loop(listener, Arc::clone(&inbox));
            Some(inbox)
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                path = %socket_path.display(),
                "ipc receive socket unavailable; probes cannot reach this sentinel this run"
            );
            None
        }
    }
}

/// Siehe die Linux-Fassung. `harw-sentinel` läuft nur auf Linux vollständig
/// (Landlock, `SOCK_SEQPACKET`/`SO_PEERCRED`, alle Sensor-Quellen); auf
/// jeder anderen Plattform bleibt der IPC-Empfangspfad ungebunden und diese
/// Funktion gibt stets `None` zurück.
#[cfg(not(target_os = "linux"))]
fn spawn_ipc_if_available(socket_path: &std::path::Path) -> Option<IpcInboxHandle> {
    tracing::warn!(
        path = %socket_path.display(),
        "ipc receive path is only implemented on linux; running without it"
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    // Eine Laufzeit-Zusicherung über eine Konstante kann nicht fehlschlagen --
    // clippy hat recht, dass das kein Test ist. Als `const`-Zusicherung wird
    // daraus eine echte Invariante: sie scheitert beim Übersetzen, wenn
    // jemand die Konstante auf null setzt.
    const _: () = assert!(TELEMETRY_MAX_BYTES > 0);

    #[test]
    fn test_run_rejects_a_home_override_pointing_at_a_file_not_a_directory() -> TestResult {
        let file = tempfile::NamedTempFile::new().map_err(ctx("temp file"))?;
        let path = file
            .path()
            .to_str()
            .ok_or(TestError::Missing("utf8 path"))?;
        let cli = Cli::try_parse_from(["harw-sentinel", "--once", "--home", path])
            .map_err(ctx("valid cli"))?;

        let result = run(cli, DodSettings::builtin_defaults());
        assert!(
            matches!(
                result,
                Err(SentinelBinError::Sink(_)) | Err(SentinelBinError::Home(_))
            ),
            "a non-directory --home must fail to resolve or to open the telemetry sink"
        );
        Ok(())
    }

    #[test]
    fn test_landlock_degraded_event_survives_being_captured_before_the_sentinel_exists()
    -> TestResult {
        // Spiegelt die Reihenfolge in `run()`: `sandbox::restrict_self` (und
        // damit `landlock_degraded_event`) läuft, bevor `Sentinel::new` je
        // aufgerufen wird — die Antwort auf Frage 3 der Moduldoku, Abschnitt
        // „Externe Ereignisse erreichen jetzt den Ringpuffer": das Ereignis
        // wird zwischengespeichert und erst nach der Konstruktion nachgereicht.
        let landlock_event = Some(sandbox::landlock_degraded_event(Timestamp::UNIX_EPOCH));

        // An dieser Stelle existiert `sentinel` in `run()` noch nicht.
        let mut sentinel = Sentinel::new(
            Vec::new(),
            Arc::new(harw_observe::NullSink),
            SentinelConfig::default(),
        );
        if let Some(event) = landlock_event {
            sentinel.record_external_event(event);
        }

        assert_eq!(
            sentinel.buffer().event_len(),
            1,
            "the landlock event must not be lost"
        );
        let evidence = sentinel
            .freeze(Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;
        assert_eq!(evidence.events.len(), 1);
        assert_eq!(
            evidence.events[0].sensor.as_str(),
            sandbox::LANDLOCK_STATUS_SENSOR_ID
        );
        Ok(())
    }

    const HOST_PROFILE: &str = r#"
schema_version = 1
mode = "observe"
active_profile = "host"

[profiles.host]
scope = "host"
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []
"#;

    fn not_found() -> ConfigError {
        ConfigError::Io(std::io::Error::from(std::io::ErrorKind::NotFound))
    }

    #[test]
    fn test_lookback_keeps_a_generous_default_and_raises_a_tight_one() {
        let default = Duration::from_secs(300);
        assert_eq!(
            PollTiming::from_interval_secs(5).lookback_for(default),
            default
        );
        assert_eq!(
            PollTiming::from_interval_secs(200).lookback_for(default),
            Duration::from_secs(400),
            "lookback must be at least twice the poll interval"
        );
        assert_eq!(
            PollTiming::from_interval_secs(0).lookback_for(default),
            default
        );
    }

    #[test]
    fn test_lookback_saturates_instead_of_overflowing() {
        let timing = PollTiming::from_interval_secs(u64::MAX);
        assert!(timing.lookback_for(Duration::ZERO) >= timing.interval());
    }

    #[test]
    fn test_poll_interval_is_the_cli_interval() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel", "--interval-secs", "7"])
            .map_err(ctx("valid cli"))?;
        assert_eq!(
            PollTiming::from_interval_secs(cli.interval_secs).interval(),
            Duration::from_secs(7)
        );
        Ok(())
    }

    #[test]
    fn test_missing_system_config_falls_back_to_builtin_defaults() -> TestResult {
        let settings = settings_from_loaded(Err(not_found()), ConfigOrigin::System)
            .map_err(ctx("missing system config degrades"))?;
        assert_eq!(settings.origin, ConfigOrigin::BuiltinDefaults);
        assert!(settings.profile.is_none());
        assert_eq!(settings.sentinel_config(), SentinelConfig::default());
        Ok(())
    }

    #[test]
    fn test_missing_explicit_config_fails_closed() {
        let result = settings_from_loaded(
            Err(not_found()),
            ConfigOrigin::Explicit(std::path::PathBuf::from("/etc/harw-dod/other.toml")),
        );
        assert!(matches!(result, Err(ConfigError::Io(_))));
    }

    #[test]
    fn test_untrusted_system_config_fails_closed() {
        let result = settings_from_loaded(
            Err(ConfigError::UntrustedSystemPath {
                path: SYSTEM_CONFIG_PATH,
            }),
            ConfigOrigin::System,
        );
        assert!(matches!(
            result,
            Err(ConfigError::UntrustedSystemPath { .. })
        ));
    }

    #[test]
    fn test_other_io_errors_on_the_system_config_fail_closed() {
        let result = settings_from_loaded(
            Err(ConfigError::Io(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            ))),
            ConfigOrigin::System,
        );
        assert!(matches!(result, Err(ConfigError::Io(_))));
    }

    #[test]
    fn test_valid_config_resolves_the_active_profile() -> TestResult {
        let config = Config::from_toml(HOST_PROFILE).map_err(ctx("valid config"))?;
        let settings = settings_from_loaded(Ok(config), ConfigOrigin::System)
            .map_err(ctx("resolvable config"))?;
        let profile = settings
            .profile
            .as_ref()
            .ok_or(TestError::Missing("active profile"))?;
        assert_eq!(profile.profile_id().as_str(), "host");
        assert_eq!(settings.sentinel_config(), SentinelConfig::default());
        Ok(())
    }

    #[test]
    fn test_config_without_active_profile_fails_closed() -> TestResult {
        let config = Config::from_toml(&HOST_PROFILE.replace("active_profile = \"host\"\n", ""))
            .map_err(ctx("valid config without selection"))?;
        let result = settings_from_loaded(Ok(config), ConfigOrigin::System);
        assert!(matches!(result, Err(ConfigError::MissingActiveProfile)));
        Ok(())
    }

    #[test]
    fn test_explicit_relative_config_path_is_rejected() {
        let result = load_dod_settings(Some(Path::new("config.toml")));
        assert!(matches!(
            result,
            Err(ConfigError::InvalidExplicitPath { .. })
        ));
    }

    /// Deckt die IPC-Inbox-Entleerung ab — ausschließlich über
    /// [`ipc::IpcInbox::push`] direkt befüllt, nie über einen echten Socket
    /// (harte Auflage dieses Knotens).
    #[cfg(target_os = "linux")]
    mod external_events {
        use super::*;
        use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
        use harw_dod_signals::{EventKind, SecurityEvent, Sensor, SensorReading};
        use harw_types::SensorId;
        use std::path::PathBuf;

        /// Ein Mock-Sensor, dessen `poll` stets einen permanenten Fehler
        /// liefert — degradiert nach genau einem Aufruf (siehe
        /// `harw_dod_cap::SensorError::OutsideScope::permanence`) und erzeugt
        /// dabei selbst ein `EventKind::SensorDegraded`-Ereignis über
        /// `Sentinel::poll_all`, unabhängig vom hier getesteten externen Weg.
        #[derive(Debug)]
        struct AlwaysPermanentlyFailingSensor {
            handle: SensorHandle<Bound>,
        }

        impl Sensor for AlwaysPermanentlyFailingSensor {
            fn handle(&self) -> &SensorHandle<Bound> {
                &self.handle
            }

            fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
                Err(SensorError::OutsideScope)
            }
        }

        fn mock_degrading_sensor(id: &str) -> Arc<dyn Sensor> {
            let scope =
                ReadScope::from_roots([PathBuf::from("/nonexistent-harw-sentinel-mock-root")]);
            let handle =
                SensorHandle::new(SensorId::from_str(id), Capability::ReadProcStat).bind(scope);
            Arc::new(AlwaysPermanentlyFailingSensor { handle })
        }

        fn sample_probe_event(sensor_id: &str) -> SecurityEvent {
            SecurityEvent {
                sensor: SensorId::from_str(sensor_id),
                observed_at: Timestamp::UNIX_EPOCH,
                actor: None,
                kind: EventKind::SensorDegraded {
                    sensor: SensorId::from_str(sensor_id),
                },
            }
        }

        fn sample_peer() -> ipc::PeerCredentials {
            ipc::PeerCredentials {
                pid: 4242,
                uid: 1000,
                gid: 1000,
            }
        }

        #[test]
        fn test_externally_received_ipc_event_appears_in_buffer_and_freeze() -> TestResult {
            let inbox: IpcInboxHandle = Arc::new(Mutex::new(ipc::IpcInbox::new(4)));
            inbox
                .lock()
                .map_err(ctx("uncontended lock"))?
                .push(ipc::ReceivedEvent {
                    event: sample_probe_event("probe-fs-0"),
                    peer: sample_peer(),
                });

            let mut sentinel = Sentinel::new(
                Vec::new(),
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );
            poll_once(&mut sentinel, Some(&inbox), &harw_observe::NullSink, None);

            assert_eq!(sentinel.buffer().event_len(), 1);
            let evidence = sentinel
                .freeze(Timestamp::UNIX_EPOCH)
                .map_err(ctx("well-formed buffer content always encodes"))?;
            assert_eq!(evidence.events.len(), 1);
            assert_eq!(evidence.events[0].sensor.as_str(), "probe-fs-0");
            Ok(())
        }

        #[test]
        fn test_poll_once_drains_the_inbox_after_poll_all_so_internal_events_precede_external_ones()
        -> TestResult {
            let sensor = mock_degrading_sensor("mock-degrade-0");
            let mut sentinel = Sentinel::new(
                vec![sensor],
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );

            let inbox: IpcInboxHandle = Arc::new(Mutex::new(ipc::IpcInbox::new(4)));
            inbox
                .lock()
                .map_err(ctx("uncontended lock"))?
                .push(ipc::ReceivedEvent {
                    event: sample_probe_event("probe-fs-0"),
                    peer: sample_peer(),
                });

            poll_once(&mut sentinel, Some(&inbox), &harw_observe::NullSink, None);

            // Reihenfolge (Frage 1): das intern erzeugte `SensorDegraded`
            // aus `poll_all` steht vor dem extern eingespeisten Ereignis —
            // die `SensorId` unterscheidet dabei eindeutig Herkunft intern
            // (`mock-degrade-0`, ein bei `Sentinel::new` registrierter
            // Sensor) von Herkunft extern (`probe-fs-0`, kein registrierter
            // Sensor dieser Instanz).
            let ids: Vec<String> = sentinel
                .buffer()
                .events()
                .map(|event| event.sensor.as_str().to_owned())
                .collect();
            assert_eq!(
                ids,
                vec!["mock-degrade-0".to_owned(), "probe-fs-0".to_owned()]
            );
            assert_eq!(
                sentinel.degraded_count(),
                1,
                "the registered mock sensor degraded"
            );
            Ok(())
        }

        #[test]
        fn test_poll_once_with_no_bound_socket_is_a_no_op_on_the_buffer() {
            // `inbox == None` entspricht einem gescheiterten `bind()` beim
            // Start (siehe `spawn_ipc_if_available`) — kein Fehler, nur kein
            // externes Ereignis in diesem Zyklus.
            let mut sentinel = Sentinel::new(
                Vec::new(),
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );
            poll_once(&mut sentinel, None, &harw_observe::NullSink, None);
            assert_eq!(sentinel.buffer().event_len(), 0);
        }
    }
}

#[cfg(test)]
mod telemetry_max_files_tests {
    use super::telemetry_max_files;

    #[test]
    fn test_flag_overrides_class_default() {
        assert_eq!(telemetry_max_files(Some(3)), Some(3));
        assert_eq!(telemetry_max_files(None), Some(20));
    }
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
