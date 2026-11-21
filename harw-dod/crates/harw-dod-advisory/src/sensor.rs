//! `AdvisorySensor`: gesperrte crates.io-Pakete gegen eine lokale
//! RustSec-Datenbank abgleichen.
//!
//! # Takt: eine Prüfung je [`RESCAN_INTERVAL`]
//! Der Sentinel ruft jeden Sensor alle paar Sekunden ab (`harw-sentinel`:
//! 5 s) und kennt keinen eigenen Takt je Sensor. Lockfiles und Datenbank
//! ändern sich aber selten, und jede Prüfung meldet alle Treffer erneut —
//! ohne Drossel bis zu 65 Ereignisse je Abruf, rund 780 je Minute, genug,
//! um den Ereignisring des Sentinels (4096) in wenigen Minuten
//! umzuwälzen und die Belege anderer Sensoren zu verdrängen (B2/B10).
//! Deshalb prüft ein Abruf nur, wenn noch keine Prüfung gelang, seit der
//! letzten gelungenen mindestens [`RESCAN_INTERVAL`] vergangen ist oder
//! `now` **vor** ihr liegt (die Uhr wurde zurückgestellt). Dazwischen
//! liefert er eine leere Lesung, ohne das Dateisystem zu berühren. Gemessen
//! wird an der injizierten Zeit `now`; der Sensor liest nie die Uhr. Ein
//! gescheiterter Abruf zählt nicht als Prüfung: der nächste fällige Abruf
//! versucht es erneut (den Abstand bestimmt dann die Wiederholungsregel des
//! Sentinels).
//!
//! # Ablauf einer Prüfung
//! 1. **Datenbank prüfen.** `<db_path>/crates` muss sich im Lesebereich
//!    auflösen lassen, ein Verzeichnis mit mindestens einem Eintrag sein und
//!    nach einer advisory-db aussehen: unter den ersten
//!    [`DATABASE_PROBE_ENTRIES`] Crate-Verzeichnissen (sortiert) liegt in
//!    mindestens einem eine `RUSTSEC-*`-Datei. Sonst scheitert der Abruf
//!    (siehe „Fehler“): ein fehlender, leerer oder falscher Checkout (etwa
//!    das `crates/` eines Rust-Workspaces) darf nicht als „keine Lücken“
//!    erscheinen.
//! 2. **Lockfiles lesen.** Jedes konfigurierte `Cargo.lock` über
//!    [`crate::read::read_regular_file`] (blockiert nie auf einer FIFO) und
//!    [`crate::lockfile::parse_lockfile`]. Ein unlesbares, zu großes oder
//!    kaputtes Lockfile wird übersprungen und in
//!    [`LOCKFILE_ERRORS_METRIC`] gezählt; ebenso eines mit ungültigen
//!    Einträgen (die gültigen werden trotzdem geprüft) und eines mit mehr
//!    als [`crate::MAX_LOCKED_CRATES`] verschiedenen Crate-Namen (die ersten
//!    in Dateireihenfolge werden geprüft, der Rest nicht) — sonst bestimmte
//!    ein beschreibbares Lockfile, wie viele Verzeichnisse ein Abruf
//!    auflistet.
//! 3. **Advisories je Crate, Budget je Lockfile.** Die Lockfiles werden in
//!    Konfigurationsreihenfolge abgearbeitet, darin die Crate-Namen
//!    sortiert. Für jeden Crate-Namen, der in diesem Abruf noch nicht
//!    vollständig gelesen wurde, genau ein Glob
//!    `<db_path>/crates/<name>/RUSTSEC-*` über
//!    [`harw_dod_readfs::glob::glob`] — nie über die ganze Datenbank (die
//!    hat weit mehr als `MAX_GLOB_CANDIDATES` Einträge). Ausgewertet werden
//!    `.md`- und `.toml`-Dateien außer dem Entwurf `RUSTSEC-0000-0000`.
//!    Jedes Lockfile hat sein **eigenes** Budget von
//!    [`crate::AdvisoryConfig::max_advisories`] Dateien; was darüber liegt,
//!    wird in [`TRUNCATED_ADVISORIES_METRIC`] gezählt. Ein Lockfile in einem
//!    beschreibbaren Projektverzeichnis, das viele Crates mit Advisories
//!    nennt, erschöpft damit nur sein eigenes Budget und kürzt nie die
//!    Prüfung der übrigen Lockfiles. Ist das Budget eines Lockfiles
//!    erschöpft, wird für seine übrigen, noch nicht gelesenen Crates kein
//!    Verzeichnis mehr aufgelistet; jede solche Crate zählt einmal in
//!    [`TRUNCATED_ADVISORIES_METRIC`]. Ein vollständig gelesenes
//!    Crate-Verzeichnis wird je Prüfung nur einmal gelesen und für weitere
//!    Lockfiles wiederverwendet — die Gesamtarbeit bleibt durch die Größe
//!    der Datenbank begrenzt. Eine unlesbare oder kaputte Datei, eine Datei,
//!    die ein anderes Paket nennt, und ein nicht auflistbares
//!    Crate-Verzeichnis zählen in [`MALFORMED_ADVISORIES_METRIC`].
//! 4. **Abgleich.** Jede meldbare Advisory
//!    ([`crate::advisory::Advisory::is_reportable`]) gegen jede gesperrte
//!    Version dieser Crate ([`crate::versions::AffectedVersions`]) — alle
//!    Fassungen, nicht nur die erste. Gleiche `(Crate, Version, ID)`-Treffer
//!    aus mehreren Lockfiles werden als **ein** Ereignis gemeldet — ein
//!    Ereignis nennt nie einen Pfad und könnte sie ohnehin nicht
//!    unterscheiden —, zusammengefasst aber erst in Schritt 5, nach der
//!    Rangbildung.
//! 5. **Begrenzen und ordnen.** Höchstens
//!    `min(`[`MAX_EVENTS_PER_POLL`]`, max_advisories)` verschiedene Treffer
//!    werden je Prüfung gemeldet: der Ereignisring des Sentinels ist
//!    endlich (B2/B10), und `max_advisories` ist der Ausgabeknopf des
//!    Betriebs. Übersteigen die Treffer die Grenze, bleiben die schwersten
//!    (`High` vor `Medium`/`Unknown` vor `Low`). Innerhalb einer
//!    Schwerestufe werden die Lockfiles reihum bedient (der erste Treffer
//!    jedes Lockfiles vor dem zweiten irgendeines Lockfiles, dann nach
//!    Crate, Version, ID): ein Lockfile mit vielen alphabetisch frühen
//!    Treffern verdrängt die eines anderen nicht (B20). Der Rang wird
//!    **vor** dem Zusammenfassen bestimmt, je Vorkommen: ein Treffer, den
//!    mehrere Lockfiles enthalten, zählt mit seinem besten Rang. Sonst
//!    könnte ein vorn konfiguriertes, beschreibbares Lockfile dieselbe
//!    verwundbare Version hinter vielen frühen Treffern verstecken und sie
//!    so auch dem Lockfile des Betriebs nehmen. Die übrigen verschiedenen
//!    Treffer werden in [`TRUNCATED_EVENTS_METRIC`] gezählt. Gemeldet wird
//!    sortiert nach (Crate, Version, ID).
//! 6. **Alter der Datenbank.** Das Alter ist der Abstand von `now` zur
//!    jüngsten Änderungszeit von `<db_path>/crates` und (falls vorhanden)
//!    `<db_path>/.git/FETCH_HEAD`; jedes `git fetch`/`git pull` erneuert
//!    Letztere, ein neu hinzugekommenes Crate-Verzeichnis Erstere. Es ist
//!    eine Näherung für „wann wurde zuletzt abgeglichen“, keine Aussage
//!    über den Inhalt. Älter als [`STALE_DATABASE_AGE`] (oder nicht
//!    bestimmbar) gilt als veraltet.
//!
//! # Was gemeldet wird
//! - Je Treffer ein `SecurityEvent` mit `actor: None`, `observed_at = now`
//!   (injiziert) und [`EventKind::StructureDrift`]: `severity` aus der
//!   Advisory, `detail` = `advisory <ID> <crate>@<version>: <Titel>` bzw.
//!   `advisory <ID> <crate>@<version> (<art>): <Titel>` für informative
//!   Advisories — durch [`harw_untrusted_text::escape_line`], höchstens
//!   [`MAX_DETAIL_CHARS`] Zeichen, ohne Pfad.
//! - Genau ein `SensorDegraded`, sobald einer der vier Zähler über null
//!   liegt oder die Datenbank veraltet ist (Prinzip B19/B20: Kürzung und
//!   Blindheit werden sichtbar).
//! - In jeder Prüfung die vier Zähler und das Alter der Datenbank in
//!   Sekunden ([`DATABASE_AGE_METRIC`]) als `HostSample`, auch mit dem
//!   Wert 0.
//! - Zwischen zwei Prüfungen nichts (leere Lesung).
//!
//! `structure-drift` (`harw-dod-rules`) macht aus jedem Ereignis einen
//! Befund, mit Härte `Inferred` ([`ADVISORY_HARDNESS`]); laut Routing
//! (README §3.3) wird `Contain` dafür zu `Escalate` herabgestuft.
//!
//! # Warum `Capability::ReadWorkspaceGraph`
//! Es gibt (noch) keine eigene Advisory-Fähigkeit. Ein Lockfile *ist* der
//! aufgelöste Abhängigkeitsgraph; die Datenbank wird unter derselben,
//! unprivilegierten Fähigkeit gelesen. Damit darf der Sensor in den
//! unprivilegierten `harw-sentinel` (siehe dessen
//! `test_all_registered_sensors_are_unprivileged`).
//!
//! # Kosten
//! Zwischen zwei Prüfungen nichts außer einer Sperre und einem Vergleich.
//! Je Prüfung ein Glob (ein `read_dir`) je gesperrter crates.io-Crate, die
//! noch Budget hat, höchstens [`crate::MAX_LOCKED_CRATES`] je Lockfile, und
//! bis zu [`DATABASE_PROBE_ENTRIES`] plus eins für die Datenbankprüfung,
//! das Parsen jedes Lockfiles (höchstens 1 MiB), höchstens
//! `max_advisories` Advisory-Dateien je Lockfile und insgesamt nie mehr
//! Dateien, als die Datenbank für die gesperrten Crates enthält, sowie zwei
//! `stat`-Aufrufe für das Alter. Der einzige Zustand zwischen Abrufen ist
//! der Zeitpunkt der letzten Prüfung; jede Prüfung meldet alle Treffer
//! erneut.
//!
//! # Bewusste Ausnahme von „kein direkter `std::fs`-Zugriff“
//! Für das Alter liest der Sensor die Änderungszeit über
//! `std::fs::metadata` — nur auf dem Ergebnis von `ReadScope::resolve`,
//! also auf einem geprüften, kanonischen Pfad im Lesebereich (dieselbe
//! Begründung wie in [`crate::read`]). Das ist kein Lesen der Uhr: „jetzt“
//! bleibt das injizierte `now`.
//!
//! # Nebenläufigkeit
//! `Send + Sync`. Bis auf den Zeitpunkt der letzten Prüfung (hinter einem
//! `Mutex`, nur kurz für Lesen bzw. Schreiben gehalten, nie während der
//! Prüfung) unveränderlich nach der Konstruktion. [`Sensor::poll`] nimmt
//! `&self` und öffnet je Aufruf eigene Deskriptoren; zwei gleichzeitige,
//! fällige Abrufe prüfen beide. Eine vergiftete Sperre wird übernommen —
//! sie schützt nur ein `Option<Timestamp>`, das nie halb geschrieben sein
//! kann.
//!
//! # Fehler
//! [`Sensor::poll`] scheitert nur in einer fälligen Prüfung und nur, wenn
//! die Datenbank selbst fehlt, nicht
//! nach einer advisory-db aussieht oder außerhalb des Bereichs liegt:
//! [`SensorError::Io`] (vorübergehend, etwa ein noch nicht geklonter
//! Checkout), [`SensorError::MalformedSource`] (`crates` ist eine Datei,
//! leer oder gehört zu einem anderen Repository) bzw.
//! [`SensorError::OutsideScope`] (dauerhaft, etwa ein relativer oder nicht
//! kanonischer `db_path`), sowie [`SensorError::ToolFault`] für einen
//! `db_path`, der kein UTF-8 ist. Kein Fehler einer einzelnen Datei
//! erreicht den Aufrufer.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_advisory::{AdvisoryConfig, AdvisorySensor, DEFAULT_MAX_ADVISORIES};
//! use harw_dod_signals::Sensor;
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let sensor = AdvisorySensor::new(
//!     SensorId::from_str("advisory-0"),
//!     AdvisoryConfig {
//!         lockfiles: vec![PathBuf::from("/srv/projekt/Cargo.lock")],
//!         db_path: PathBuf::from("/var/lib/harw-dod/advisory-db"),
//!         max_advisories: DEFAULT_MAX_ADVISORIES,
//!     },
//! );
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH)?;
//! assert_eq!(reading.samples.len(), 5);
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{
    DriftSeverity, EventKind, Hardness, HostSample, SecurityEvent, Sensor, SensorReading,
};
use harw_types::SensorId;
use jiff::{SignedDuration, Timestamp};
use semver::Version;

use crate::advisory::{Advisory, AdvisoryFormat, Informational, parse_advisory};
use crate::config::{AdvisoryConfig, MAX_EVENTS_PER_POLL, MAX_LOCKED_CRATES};
use crate::fault::SourceFault;
use crate::lockfile::{is_valid_crate_name, parse_lockfile};
use crate::read::read_regular_file;

/// Die epistemische Härte der Ereignisse dieses Sensors: `Inferred`.
///
/// # Description
/// Der Sensor beobachtet keine Ausnutzung, sondern schließt aus einer
/// Versionsübereinstimmung mit einer fremden Datenbank auf Verwundbarkeit —
/// dieselbe Begründung wie `harw_dod_scanreport::REPORT_HARDNESS`. Die
/// Regel `structure-drift` leitet für jede Sensor-Kennung außer
/// `workspace-drift` ohnehin `Inferred` ab (fail-closed); diese Konstante
/// hält die Absicht fest.
pub const ADVISORY_HARDNESS: Hardness = Hardness::Inferred;

/// Metrik: Lockfiles, die nicht (vollständig) gelesen werden konnten.
pub const LOCKFILE_ERRORS_METRIC: &str = "advisory.lockfile_errors";

/// Metrik: Advisory-Dateien bzw. Crate-Verzeichnisse, die übersprungen
/// wurden, weil sie unlesbar oder kaputt waren.
pub const MALFORMED_ADVISORIES_METRIC: &str = "advisory.malformed_advisories";

/// Metrik: Advisory-Dateien, die wegen `max_advisories` nicht gelesen
/// wurden, und Crates, deren Verzeichnis deshalb nicht mehr aufgelistet
/// wurde (je Lockfile gezählt).
pub const TRUNCATED_ADVISORIES_METRIC: &str = "advisory.truncated_advisories";

/// Metrik: verschiedene Treffer, die wegen der Ereignisgrenze
/// `min(`[`MAX_EVENTS_PER_POLL`]`, max_advisories)` nicht gemeldet wurden.
pub const TRUNCATED_EVENTS_METRIC: &str = "advisory.truncated_events";

/// Metrik: Alter der Datenbank in Sekunden (siehe Moduldoku, Schritt 6);
/// nie negativ, an `u32::MAX` gesättigt, ein nicht bestimmbares Alter
/// ebenfalls `u32::MAX`.
pub const DATABASE_AGE_METRIC: &str = "advisory.database_age_seconds";

/// Ab diesem Alter gilt die Datenbank als veraltet und der Abruf meldet
/// `SensorDegraded`: **14 Tage**.
///
/// # Description
/// RustSec veröffentlicht mehrere Advisories je Woche; ein Checkout, der
/// zwei Wochen lang nicht abgeglichen wurde, übersieht mit hoher
/// Wahrscheinlichkeit bekannte Lücken. Kürzer würde ein wöchentlicher
/// Abgleich mit etwas Verspätung schon als Störung gelten.
pub const STALE_DATABASE_AGE: SignedDuration = SignedDuration::from_hours(336);

/// Mindestabstand zweier Prüfungen: **15 Minuten**, gemessen an der
/// injizierten Zeit (siehe Moduldoku, „Takt").
///
/// # Description
/// Eine neue Advisory oder ein geändertes Lockfile erscheint spätestens nach
/// dieser Zeit — gegenüber den Tagen, die eine Advisory bis zum Abgleich der
/// lokalen Datenbank braucht, vernachlässigbar. Gegenüber dem 5-s-Takt des
/// Sentinels sinkt die Zahl der Ereignisse auf ein Hundertachtzigstel:
/// höchstens `MAX_EVENTS_PER_POLL + 1` je Viertelstunde.
pub const RESCAN_INTERVAL: SignedDuration = SignedDuration::from_mins(15);

/// Wie viele Einträge von `<db_path>/crates` (sortiert) die
/// Datenbankprüfung höchstens nach einer `RUSTSEC-*`-Datei durchsucht.
pub const DATABASE_PROBE_ENTRIES: usize = 3;

/// Höchstlänge von `StructureDrift::detail` in Zeichen (nach dem Escapen).
pub const MAX_DETAIL_CHARS: usize = 512;

// Das Verzeichnis der crates.io-Advisories in der Datenbank.
const CRATES_DIR: &str = "crates";

// Das Git-Verzeichnis eines Checkouts relativ zur Datenbankwurzel.
const GIT_DIR: &str = ".git";

// Von jedem `git fetch`/`git pull` erneuert (relativ zu `GIT_DIR`).
const FETCH_HEAD: &str = "FETCH_HEAD";

// Glob für die Advisory-Dateien einer Crate (eine Komponente).
const ADVISORY_FILE_GLOB: &str = "RUSTSEC-*";

// Präfix der Entwurfsvorlage in der Datenbank, nie eine echte Advisory.
const DRAFT_PREFIX: &str = "RUSTSEC-0000-0000.";

/// Ein Sensor, der `Cargo.lock`-Dateien gegen einen lokalen
/// RustSec-Checkout abgleicht.
///
/// # Description
/// Siehe Moduldoku. Trägt [`Capability::ReadWorkspaceGraph`], gebunden an
/// [`AdvisoryConfig::read_scope`].
///
/// # Concurrency
/// `Send + Sync`; veränderlich ist nur der Zeitpunkt der letzten Prüfung
/// (siehe Moduldoku, „Nebenläufigkeit").
#[derive(Debug)]
pub struct AdvisorySensor {
    handle: SensorHandle<Bound>,
    config: AdvisoryConfig,
    // Zeitpunkt der letzten gelungenen Prüfung; `None` vor der ersten.
    last_scan: Mutex<Option<Timestamp>>,
}

impl AdvisorySensor {
    /// Baut den Sensor aus seiner Konfiguration.
    ///
    /// # Description
    /// Bindet den Griff an [`AdvisoryConfig::read_scope`]. Greift nicht aufs
    /// Dateisystem zu; Pfade werden erst beim Abruf geprüft.
    ///
    /// # Arguments
    /// - `id` (`SensorId`): Kennung dieses Sensors, etwa `advisory-0`.
    /// - `config` (`AdvisoryConfig`): Lockfiles, Datenbank, Obergrenze.
    ///
    /// # Returns
    /// Einen gebundenen `AdvisorySensor`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_advisory::{AdvisoryConfig, AdvisorySensor};
    /// use harw_dod_cap::{Capability, CapabilityClass};
    /// use harw_dod_signals::Sensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// let sensor = AdvisorySensor::new(
    ///     SensorId::from_str("advisory-0"),
    ///     AdvisoryConfig {
    ///         lockfiles: vec![PathBuf::from("/srv/projekt/Cargo.lock")],
    ///         db_path: PathBuf::from("/var/lib/harw-dod/advisory-db"),
    ///         max_advisories: 16,
    ///     },
    /// );
    /// assert_eq!(sensor.handle().capability(), Capability::ReadWorkspaceGraph);
    /// assert_eq!(sensor.handle().capability().class(), CapabilityClass::Unprivileged);
    /// ```
    #[must_use]
    pub fn new(id: SensorId, config: AdvisoryConfig) -> Self {
        let scope = config.read_scope();
        Self {
            handle: SensorHandle::new(id, Capability::ReadWorkspaceGraph).bind(scope),
            config,
            last_scan: Mutex::new(None),
        }
    }

    // Ob bei `now` eine Prüfung fällig ist (siehe Moduldoku, „Takt"). Eine
    // vergiftete Sperre wird übernommen: sie schützt nur einen `Copy`-Wert.
    fn scan_due(&self, now: Timestamp) -> bool {
        let last = *self
            .last_scan
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        last.is_none_or(|last| {
            let elapsed = now.duration_since(last);
            elapsed.is_negative() || elapsed >= RESCAN_INTERVAL
        })
    }

    // Merkt `now` als Zeitpunkt der letzten gelungenen Prüfung.
    fn record_scan(&self, now: Timestamp) {
        *self
            .last_scan
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(now);
    }

    // Eine vollständige Prüfung (siehe Moduldoku, „Ablauf einer Prüfung").
    fn scan(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let scope = self.handle.scope();
        let crates_dir = self.config.db_path.join(CRATES_DIR);
        // Schritt 1: ohne Datenbank kein „alles in Ordnung“.
        scope.resolve(&crates_dir)?;
        let glob_base = glob_base(&crates_dir)?;
        check_database(scope, &glob_base)?;

        let mut counters = Counters::default();
        let lockfiles = self.read_lockfiles(scope, &mut counters);
        let matches = self.collect_matches(scope, &glob_base, &lockfiles, &mut counters);
        let database_age = self.database_age(scope, &crates_dir, now);
        Ok(self.build_reading(now, matches, counters, database_age))
    }

    // Schritt 2: jedes Lockfile einzeln lesen; je Lockfile Crate-Name →
    // gesperrte Versionen (nur die lesbaren Lockfiles), höchstens
    // `MAX_LOCKED_CRATES` Namen in Dateireihenfolge.
    fn read_lockfiles(&self, scope: &ReadScope, counters: &mut Counters) -> Vec<LockedCrates> {
        let mut lockfiles = Vec::new();
        for path in &self.config.lockfiles {
            match read_regular_file(scope, path).and_then(|content| parse_lockfile(&content)) {
                Ok(parsed) => {
                    let mut incomplete = parsed.invalid_entries > 0;
                    let mut locked = LockedCrates::new();
                    for krate in parsed.crates {
                        if locked.len() >= MAX_LOCKED_CRATES && !locked.contains_key(&krate.name) {
                            incomplete = true;
                            continue;
                        }
                        locked.entry(krate.name).or_default().insert(krate.version);
                    }
                    if incomplete {
                        bump(&mut counters.lockfile_errors);
                    }
                    lockfiles.push(locked);
                }
                // Der Grund (`SourceFault`) ist inhaltsfrei und wird bewusst
                // nur gezählt; Pfad und Inhalt verlassen den Sensor nie.
                Err(_) => bump(&mut counters.lockfile_errors),
            }
        }
        lockfiles
    }

    // Schritte 3 und 4: Advisories je Crate lesen (Budget je Lockfile) und
    // abgleichen.
    fn collect_matches(
        &self,
        scope: &ReadScope,
        glob_base: &str,
        lockfiles: &[LockedCrates],
        counters: &mut Counters,
    ) -> Vec<Match> {
        // Vollständig gelesene Crate-Verzeichnisse: nur die meldbaren
        // Advisories, je Prüfung einmal gelesen.
        let mut scanned: BTreeMap<String, Vec<Advisory>> = BTreeMap::new();
        let mut matches = Vec::new();

        for (source, locked) in lockfiles.iter().enumerate() {
            let mut budget = self.config.max_advisories;
            for (name, versions) in locked {
                let found = Found {
                    name,
                    versions,
                    source,
                };
                if let Some(advisories) = scanned.get(name) {
                    push_matches(&mut matches, &found, advisories);
                    continue;
                }
                // Budget erschöpft: kein Verzeichnis mehr auflisten; die
                // Crate zählt einmal als gekürzt.
                if budget == 0 {
                    bump(&mut counters.truncated_advisories);
                    continue;
                }
                let scan = scan_crate(scope, glob_base, name, &mut budget, counters);
                push_matches(&mut matches, &found, &scan.advisories);
                // Nur ein vollständig gelesenes Verzeichnis wird für spätere
                // Lockfiles gemerkt. Bei erschöpftem Budget liest ein
                // späteres Lockfile es mit seinem eigenen Budget erneut.
                if scan.complete {
                    scanned.insert(name.clone(), scan.advisories);
                }
            }
        }

        // Jedes Vorkommen bleibt erhalten (je Lockfile ein Eintrag):
        // zusammengefasst wird erst nach der Rangbildung in `cap_matches`.
        matches
    }

    // Schritt 5 und Ausgabe: begrenzen, ordnen, Ereignisse und Samples bauen.
    fn build_reading(
        &self,
        now: Timestamp,
        mut matches: Vec<Match>,
        mut counters: Counters,
        database_age: Option<SignedDuration>,
    ) -> SensorReading {
        counters.truncated_events = cap_matches(
            &mut matches,
            MAX_EVENTS_PER_POLL.min(self.config.max_advisories),
        );
        let stale = database_age.is_none_or(|age| age >= STALE_DATABASE_AGE);

        let id = self.handle.id();
        let mut events: Vec<SecurityEvent> = matches
            .iter()
            .map(|found| SecurityEvent {
                sensor: id.clone(),
                observed_at: now,
                actor: None,
                kind: EventKind::StructureDrift {
                    severity: found.severity,
                    detail: found.detail(),
                },
            })
            .collect();
        if counters.any() || stale {
            events.push(SecurityEvent {
                sensor: id.clone(),
                observed_at: now,
                actor: None,
                kind: EventKind::SensorDegraded { sensor: id.clone() },
            });
        }

        let samples: Vec<HostSample> = [
            (
                LOCKFILE_ERRORS_METRIC,
                count_value(counters.lockfile_errors),
            ),
            (
                MALFORMED_ADVISORIES_METRIC,
                count_value(counters.malformed_advisories),
            ),
            (
                TRUNCATED_ADVISORIES_METRIC,
                count_value(counters.truncated_advisories),
            ),
            (
                TRUNCATED_EVENTS_METRIC,
                count_value(counters.truncated_events),
            ),
            (DATABASE_AGE_METRIC, age_value(database_age)),
        ]
        .into_iter()
        .map(|(metric, value)| HostSample {
            sensor: id.clone(),
            observed_at: now,
            metric: Cow::Borrowed(metric),
            value,
        })
        .collect();

        SensorReading { samples, events }
    }

    // Schritt 6: das Alter der Datenbank relativ zu `now`, nie negativ;
    // `None`, wenn keine Änderungszeit lesbar ist.
    fn database_age(
        &self,
        scope: &ReadScope,
        crates_dir: &Path,
        now: Timestamp,
    ) -> Option<SignedDuration> {
        let fetch_head = self.config.db_path.join(GIT_DIR).join(FETCH_HEAD);
        [crates_dir, fetch_head.as_path()]
            .into_iter()
            .filter_map(|path| modified(scope, path))
            .max()
            .map(|newest| now.duration_since(newest).max(SignedDuration::ZERO))
    }
}

impl Sensor for AdvisorySensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Gleicht alle Lockfiles gegen die Datenbank ab, wenn eine Prüfung
    /// fällig ist.
    ///
    /// # Description
    /// Siehe Moduldoku, Abschnitte „Takt“ und „Ablauf einer Prüfung“:
    /// fällig ist eine Prüfung vor der ersten gelungenen, nach
    /// [`RESCAN_INTERVAL`] und wenn `now` vor der letzten liegt; sonst ist
    /// die Lesung leer. Nur eine gelungene Prüfung zählt.
    ///
    /// # Errors
    /// Nur in einer fälligen Prüfung und nur wenn `<db_path>/crates` sich
    /// nicht auflösen lässt ([`SensorError::Io`],
    /// [`SensorError::OutsideScope`]), nicht nach einer advisory-db aussieht
    /// ([`SensorError::MalformedSource`]) oder `db_path` kein UTF-8 ist
    /// ([`SensorError::ToolFault`]).
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        if !self.scan_due(now) {
            return Ok(SensorReading::default());
        }
        let reading = self.scan(now)?;
        self.record_scan(now);
        Ok(reading)
    }
}

// Die gesperrten Versionen eines Lockfiles je Crate-Name.
type LockedCrates = BTreeMap<String, BTreeSet<Version>>;

// Die vier Zähler eines Abrufs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Counters {
    lockfile_errors: usize,
    malformed_advisories: usize,
    truncated_advisories: usize,
    truncated_events: usize,
}

impl Counters {
    // Ob irgendein Zähler über null liegt (dann `SensorDegraded`).
    fn any(&self) -> bool {
        self.lockfile_errors > 0
            || self.malformed_advisories > 0
            || self.truncated_advisories > 0
            || self.truncated_events > 0
    }
}

// Eine Crate eines Lockfiles beim Abgleich.
struct Found<'a> {
    // Der Crate-Name.
    name: &'a str,
    // Ihre gesperrten Versionen in diesem Lockfile.
    versions: &'a BTreeSet<Version>,
    // Index des Lockfiles in der Liste der lesbaren Lockfiles.
    source: usize,
}

// Schritt 4: jede gesperrte Version gegen jede Advisory einer Crate.
fn push_matches(matches: &mut Vec<Match>, found: &Found<'_>, advisories: &[Advisory]) {
    for advisory in advisories {
        for version in found.versions {
            if advisory.versions.is_vulnerable(version) {
                matches.push(Match {
                    name: found.name.to_owned(),
                    version: version.clone(),
                    id: advisory.id.clone(),
                    title: advisory.title.clone(),
                    severity: advisory.severity,
                    informational: advisory.informational,
                    source: found.source,
                });
            }
        }
    }
}

// Das Ergebnis des Lesens eines Crate-Verzeichnisses.
struct CrateScan {
    // Die meldbaren Advisories, in Dateireihenfolge.
    advisories: Vec<Advisory>,
    // `false`, wenn das Budget vor der letzten Datei erschöpft war.
    complete: bool,
}

// Schritt 3 für eine Crate: `<glob_base>/<name>/RUSTSEC-*` lesen, höchstens
// `budget` Dateien (das Budget wird verbraucht).
fn scan_crate(
    scope: &ReadScope,
    glob_base: &str,
    name: &str,
    budget: &mut usize,
    counters: &mut Counters,
) -> CrateScan {
    let mut scan = CrateScan {
        advisories: Vec::new(),
        complete: true,
    };
    let pattern = format!("{glob_base}/{name}/{ADVISORY_FILE_GLOB}");
    let candidates = match harw_dod_readfs::glob::glob(scope, &pattern) {
        Ok(paths) => paths,
        // Etwa mehr als `MAX_GLOB_CANDIDATES` Dateien: das Verzeichnis zählt
        // als kaputt; der Fehler nennt das Muster (mit Pfad) und wird
        // deshalb nicht weitergegeben. Als vollständig vermerkt, damit es
        // je Prüfung nur einmal zählt.
        Err(_) => {
            bump(&mut counters.malformed_advisories);
            return scan;
        }
    };

    for path in candidates {
        let Some(format) = advisory_format(&path) else {
            continue;
        };
        let Some(left) = budget.checked_sub(1) else {
            bump(&mut counters.truncated_advisories);
            scan.complete = false;
            continue;
        };
        *budget = left;

        let parsed = read_regular_file(scope, &path)
            .and_then(|content| parse_advisory(&content, format))
            .and_then(|advisory| {
                if advisory.package == name {
                    Ok(advisory)
                } else {
                    Err(SourceFault::PackageMismatch)
                }
            });
        match parsed {
            Ok(advisory) if advisory.is_reportable() => scan.advisories.push(advisory),
            Ok(_) => {}
            // Kaputt, unlesbar oder für ein anderes Paket: gezählt, nie mit
            // Inhalt weitergegeben.
            Err(_) => bump(&mut counters.malformed_advisories),
        }
    }
    scan
}

// Schritt 1: `<glob_base>` (= `<db_path>/crates`) muss ein Verzeichnis mit
// Einträgen sein, und unter den ersten `DATABASE_PROBE_ENTRIES`
// Crate-Verzeichnissen muss eines eine `RUSTSEC-*`-Datei enthalten.
fn check_database(scope: &ReadScope, glob_base: &str) -> Result<(), SensorError> {
    let entries = match harw_dod_readfs::glob::glob(scope, &format!("{glob_base}/*")) {
        Ok(entries) => entries,
        // Mehr Einträge, als ein Glob verfolgt: sicher nicht leer; eine
        // Stichprobe ist dann nicht möglich und entfällt.
        Err(ReadFsError::GlobLimitExceeded { .. }) => return Ok(()),
        Err(_) => return Err(SensorError::MalformedSource),
    };
    let looks_like_advisory_db = entries
        .iter()
        .filter_map(|path| path.file_name()?.to_str())
        .filter(|name| is_valid_crate_name(name))
        .take(DATABASE_PROBE_ENTRIES)
        .any(|name| {
            harw_dod_readfs::glob::glob(scope, &format!("{glob_base}/{name}/{ADVISORY_FILE_GLOB}"))
                .is_ok_and(|files| !files.is_empty())
        });
    if looks_like_advisory_db {
        Ok(())
    } else {
        Err(SensorError::MalformedSource)
    }
}

// Die Änderungszeit eines Pfads im Lesebereich; `None`, wenn er fehlt,
// außerhalb liegt oder keine Zeit liefert.
fn modified(scope: &ReadScope, path: &Path) -> Option<Timestamp> {
    let resolved = scope.resolve(path).ok()?;
    let mtime = std::fs::metadata(resolved).ok()?.modified().ok()?;
    Timestamp::try_from(mtime).ok()
}

// Schritt 5: hält höchstens `limit` verschiedene Treffer — die schwersten
// zuerst, in einer Schwerestufe die Lockfiles reihum — und gibt die Zahl der
// verworfenen verschiedenen Treffer zurück. `matches` enthält jedes
// Vorkommen (derselbe Treffer einmal je Lockfile); der Rang wird je
// Vorkommen bestimmt, bevor zusammengefasst wird, damit ein Treffer mit
// seinem besten Rang zählt (B20). Danach enthält `matches` jeden gewählten
// Treffer genau einmal, sortiert nach (Crate, Version, ID).
fn cap_matches(matches: &mut Vec<Match>, limit: usize) -> usize {
    let mut occurrences = std::mem::take(matches);
    // Rang: der wievielte Treffer seines Lockfiles in seiner Stufe, nach
    // (Crate, Version, ID).
    occurrences.sort_by(|a, b| {
        priority(b.severity)
            .cmp(&priority(a.severity))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.order(b))
    });
    let mut seen: BTreeMap<(u8, usize), usize> = BTreeMap::new();
    let mut ranked: Vec<(usize, Match)> = occurrences
        .into_iter()
        .map(|found| {
            let slot = seen
                .entry((priority(found.severity), found.source))
                .or_insert(0);
            let rank = *slot;
            *slot = slot.saturating_add(1);
            (rank, found)
        })
        .collect();
    ranked.sort_by(|(rank_a, a), (rank_b, b)| {
        priority(b.severity)
            .cmp(&priority(a.severity))
            .then_with(|| rank_a.cmp(rank_b))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.order(b))
    });

    // Gierig in Rangfolge wählen; ein schon gewählter Treffer (aus einem
    // anderen Lockfile) wird übersprungen, nicht doppelt gezählt.
    let mut kept_keys: BTreeSet<MatchKey> = BTreeSet::new();
    let mut dropped_keys: BTreeSet<MatchKey> = BTreeSet::new();
    for (_, found) in ranked {
        let key = found.key();
        if kept_keys.contains(&key) {
            continue;
        }
        if kept_keys.len() < limit {
            kept_keys.insert(key);
            matches.push(found);
        } else {
            dropped_keys.insert(key);
        }
    }
    matches.sort_by(Match::order);
    dropped_keys.len()
}

// Die Identität eines Treffers über Lockfiles hinweg: Crate, Version, ID.
type MatchKey = (String, Version, String);

// Ein Treffer: eine gesperrte Version, die eine meldbare Advisory betrifft.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Match {
    name: String,
    version: Version,
    id: String,
    title: String,
    severity: DriftSeverity,
    informational: Option<Informational>,
    // Index des Lockfiles dieses Vorkommens (nur für die Kürzung).
    source: usize,
}

impl Match {
    // Die Identität über Lockfiles hinweg (siehe `MatchKey`).
    fn key(&self) -> MatchKey {
        (self.name.clone(), self.version.clone(), self.id.clone())
    }

    // Ausgabeordnung: Crate, Version, ID.
    fn order(&self, other: &Self) -> Ordering {
        self.name
            .cmp(&other.name)
            .then_with(|| self.version.cmp(&other.version))
            .then_with(|| self.id.cmp(&other.id))
    }

    // Die Beschreibung für Menschen; ID, Name und Version sind bereits
    // geprüftes ASCII, der Titel bereits entschärft — `escape_line` hält
    // die Zusage trotzdem für die ganze Zeile und kappt sie.
    fn detail(&self) -> String {
        let raw = match self.informational {
            Some(kind) => format!(
                "advisory {} {}@{} ({}): {}",
                self.id,
                self.name,
                self.version,
                kind.as_str(),
                self.title
            ),
            None => format!(
                "advisory {} {}@{}: {}",
                self.id, self.name, self.version, self.title
            ),
        };
        harw_untrusted_text::escape_line(&raw, MAX_DETAIL_CHARS)
    }
}

// Rang beim Kürzen: `Unknown` zählt wie `Medium` (die Regel stuft es so
// ein), nie wie `Low`.
fn priority(severity: DriftSeverity) -> u8 {
    match severity {
        DriftSeverity::High => 3,
        DriftSeverity::Medium | DriftSeverity::Unknown => 2,
        DriftSeverity::Low => 1,
    }
}

// Das Format einer Glob-Fundstelle; `None` für Entwürfe und fremde Endungen.
fn advisory_format(path: &Path) -> Option<AdvisoryFormat> {
    let file_name = path.file_name()?.to_str()?;
    if file_name.starts_with(DRAFT_PREFIX) {
        return None;
    }
    AdvisoryFormat::from_file_name(file_name)
}

// Das bereichsrelative Glob-Präfix für `<db_path>/crates`
// (`harw_dod_readfs::glob::glob` sucht ab `/`, ohne führenden Schrägstrich).
fn glob_base(crates_dir: &Path) -> Result<String, SensorError> {
    let text = crates_dir.to_str().ok_or(SensorError::ToolFault)?;
    let relative = text.strip_prefix('/').ok_or(SensorError::OutsideScope)?;
    Ok(relative.trim_end_matches('/').to_owned())
}

// Erhöht einen Zähler ohne Überlauf.
fn bump(counter: &mut usize) {
    *counter = counter.saturating_add(1);
}

// Ein Zähler als Messwert; jenseits von `u32::MAX` gesättigt.
fn count_value(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

// Das Alter in ganzen Sekunden als Messwert; ein unbekanntes Alter und
// alles jenseits von `u32::MAX` gesättigt (das Alter ist nie negativ).
fn age_value(age: Option<SignedDuration>) -> f64 {
    let seconds = age.map_or(u32::MAX, |age| {
        u32::try_from(age.as_secs()).unwrap_or(u32::MAX)
    });
    f64::from(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        CRATES_IO, Fixture, TestError, TestResult, advisory_md, ctx, missing,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::config::DEFAULT_MAX_ADVISORIES;

    const SENSOR_ID: &str = "advisory-0";
    const CRITICAL: &str = "cvss = \"CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H\"";

    fn now() -> TestResult<Timestamp> {
        Timestamp::from_second(1_700_000_000).map_err(ctx("Zeitstempel bauen"))
    }

    fn sensor(fixture: &Fixture, lockfiles: Vec<PathBuf>, max_advisories: usize) -> AdvisorySensor {
        AdvisorySensor::new(
            SensorId::from_str(SENSOR_ID),
            AdvisoryConfig {
                lockfiles,
                db_path: fixture.db(),
                max_advisories,
            },
        )
    }

    fn drift_details(reading: &SensorReading) -> Vec<(DriftSeverity, String)> {
        reading
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::StructureDrift { severity, detail } => Some((*severity, detail.clone())),
                _ => None,
            })
            .collect()
    }

    fn degraded_count(reading: &SensorReading) -> usize {
        reading
            .events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::SensorDegraded { .. }))
            .count()
    }

    fn sample_value(reading: &SensorReading, metric: &str) -> TestResult<f64> {
        reading
            .samples
            .iter()
            .find(|sample| sample.metric == metric)
            .map(|sample| sample.value)
            .ok_or_else(missing("Metrik"))
    }

    fn assert_count(reading: &SensorReading, metric: &str, expected: f64) -> TestResult {
        let value = sample_value(reading, metric)?;
        if (value - expected).abs() > f64::EPSILON {
            return Err(TestError::Unexpected(format!(
                "{metric}: {value} statt {expected}"
            )));
        }
        Ok(())
    }

    // Die tokio-Advisory aus der echten Datenbank, gekürzt.
    fn write_tokio_advisory(fixture: &Fixture) -> TestResult<PathBuf> {
        fixture.write_advisory(
            "tokio",
            "RUSTSEC-2021-0124.md",
            &advisory_md(
                "RUSTSEC-2021-0124",
                "tokio",
                "patched = [\">= 1.8.4, < 1.9.0\", \">= 1.13.1\"]\nunaffected = [\"< 0.1.14\"]",
                CRITICAL,
                "Data race when sending and receiving after closing a `oneshot` channel",
            ),
        )
    }

    #[test]
    fn test_vulnerable_version_yields_one_structure_drift_event() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let now = now()?;

        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now)
            .map_err(ctx("Abruf"))?;

        assert_eq!(
            drift_details(&reading),
            vec![(
                DriftSeverity::High,
                "advisory RUSTSEC-2021-0124 tokio@1.8.0: Data race when sending and receiving after closing a `oneshot` channel".to_owned()
            )]
        );
        let event = reading.events.first().ok_or_else(missing("Ereignis"))?;
        assert_eq!(event.observed_at, now);
        assert_eq!(event.actor, None);
        assert_eq!(event.sensor.as_str(), SENSOR_ID);
        assert_eq!(degraded_count(&reading), 0);
        assert_eq!(reading.samples.len(), 5);
        // Die Fixture ist jünger als `now` (2023): Alter 0, nie negativ.
        for metric in [
            LOCKFILE_ERRORS_METRIC,
            MALFORMED_ADVISORIES_METRIC,
            TRUNCATED_ADVISORIES_METRIC,
            TRUNCATED_EVENTS_METRIC,
            DATABASE_AGE_METRIC,
        ] {
            assert_count(&reading, metric, 0.0)?;
        }
        Ok(())
    }

    #[test]
    fn test_patched_version_yields_no_event() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.13.1", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert!(reading.events.is_empty());
        Ok(())
    }

    #[test]
    fn test_safe_version_first_does_not_hide_a_vulnerable_one() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile(
            "Cargo.lock",
            &[
                ("tokio", "1.40.0", Some(CRATES_IO)),
                ("tokio", "1.8.0", Some(CRATES_IO)),
            ],
        )?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        let details = drift_details(&reading);
        assert_eq!(details.len(), 1);
        let (_, detail) = details.first().ok_or_else(missing("Treffer"))?;
        assert!(detail.contains("tokio@1.8.0"));
        Ok(())
    }

    #[test]
    fn test_path_and_git_packages_are_never_matched() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile(
            "Cargo.lock",
            &[
                ("tokio", "1.8.0", None),
                (
                    "tokio",
                    "1.8.1",
                    Some("git+https://github.com/tokio-rs/tokio?rev=1#1"),
                ),
            ],
        )?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert!(reading.events.is_empty());
        Ok(())
    }

    #[test]
    fn test_withdrawn_and_unreported_informational_advisories_are_skipped() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.write_advisory(
            "demo",
            "RUSTSEC-2099-0001.md",
            &advisory_md(
                "RUSTSEC-2099-0001",
                "demo",
                "patched = []",
                "withdrawn = \"2099-02-01\"",
                "Zurückgezogen",
            ),
        )?;
        fixture.write_advisory(
            "demo",
            "RUSTSEC-2099-0002.md",
            &advisory_md(
                "RUSTSEC-2099-0002",
                "demo",
                "patched = []",
                "informational = \"something-else\"",
                "Andere Art",
            ),
        )?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("demo", "1.0.0", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert!(reading.events.is_empty());
        assert_count(&reading, MALFORMED_ADVISORIES_METRIC, 0.0)?;
        Ok(())
    }

    #[test]
    fn test_informational_advisory_is_reported_as_low_with_its_kind() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.write_advisory(
            "demo",
            "RUSTSEC-2099-0003.md",
            &advisory_md(
                "RUSTSEC-2099-0003",
                "demo",
                "patched = []",
                "informational = \"unmaintained\"",
                "demo is unmaintained",
            ),
        )?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("demo", "0.3.0", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert_eq!(
            drift_details(&reading),
            vec![(
                DriftSeverity::Low,
                "advisory RUSTSEC-2099-0003 demo@0.3.0 (unmaintained): demo is unmaintained"
                    .to_owned()
            )]
        );
        Ok(())
    }

    #[test]
    fn test_events_are_sorted_by_crate_version_and_id() -> TestResult {
        let fixture = Fixture::new()?;
        for (krate, id) in [
            ("zeta", "RUSTSEC-2099-0010"),
            ("alpha", "RUSTSEC-2099-0012"),
            ("alpha", "RUSTSEC-2099-0011"),
        ] {
            fixture.write_advisory(
                krate,
                &format!("{id}.md"),
                &advisory_md(id, krate, "patched = []", "", "Titel"),
            )?;
        }
        let lock = fixture.write_lockfile(
            "Cargo.lock",
            &[
                ("zeta", "1.0.0", Some(CRATES_IO)),
                ("alpha", "1.10.0", Some(CRATES_IO)),
                ("alpha", "1.9.0", Some(CRATES_IO)),
            ],
        )?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        let order: Vec<String> = drift_details(&reading)
            .into_iter()
            .filter_map(|(_, detail)| {
                detail
                    .strip_prefix("advisory ")
                    .and_then(|rest| rest.split_once(':'))
                    .map(|(head, _)| head.to_owned())
            })
            .collect();
        assert_eq!(
            order,
            [
                "RUSTSEC-2099-0011 alpha@1.9.0",
                "RUSTSEC-2099-0012 alpha@1.9.0",
                "RUSTSEC-2099-0011 alpha@1.10.0",
                "RUSTSEC-2099-0012 alpha@1.10.0",
                "RUSTSEC-2099-0010 zeta@1.0.0",
            ]
        );
        Ok(())
    }

    // Ein Treffer für `cap_matches` aus dem Lockfile `source`.
    fn found_in(
        source: usize,
        name: &str,
        version: &str,
        severity: DriftSeverity,
    ) -> TestResult<Match> {
        Ok(Match {
            name: name.to_owned(),
            version: Version::parse(version).map_err(ctx("Version parsen"))?,
            id: "RUSTSEC-2099-0001".to_owned(),
            title: "Titel".to_owned(),
            severity,
            informational: None,
            source,
        })
    }

    // Ein Treffer für `cap_matches` aus dem ersten Lockfile.
    fn found(name: &str, version: &str, severity: DriftSeverity) -> TestResult<Match> {
        found_in(0, name, version, severity)
    }

    #[test]
    fn test_cap_matches_serves_lockfiles_round_robin_within_a_severity() -> TestResult {
        // Lockfile 0 hat drei alphabetisch frühe hohe Treffer, Lockfile 1
        // einen späten: bei Grenze 2 bleibt je einer, nicht die ersten zwei
        // nach dem Alphabet (B20).
        let mut matches = vec![
            found_in(0, "aaa", "1.0.0", DriftSeverity::High)?,
            found_in(0, "aab", "1.0.0", DriftSeverity::High)?,
            found_in(0, "aac", "1.0.0", DriftSeverity::High)?,
            found_in(1, "zzz", "1.0.0", DriftSeverity::High)?,
        ];
        assert_eq!(cap_matches(&mut matches, 2), 2);
        let kept: Vec<&str> = matches.iter().map(|found| found.name.as_str()).collect();
        assert_eq!(kept, ["aaa", "zzz"]);

        // Die Schwere geht vor: ein niedriger Treffer des zweiten Lockfiles
        // verdrängt keinen hohen des ersten.
        let mut matches = vec![
            found_in(0, "aaa", "1.0.0", DriftSeverity::High)?,
            found_in(0, "aab", "1.0.0", DriftSeverity::High)?,
            found_in(1, "zzz", "1.0.0", DriftSeverity::Low)?,
        ];
        assert_eq!(cap_matches(&mut matches, 2), 1);
        let kept: Vec<&str> = matches.iter().map(|found| found.name.as_str()).collect();
        assert_eq!(kept, ["aaa", "aab"]);
        Ok(())
    }

    #[test]
    fn test_cap_matches_keeps_the_most_severe_and_counts_the_rest() -> TestResult {
        let mut matches = vec![
            found("aaa", "1.0.0", DriftSeverity::Low)?,
            found("bbb", "2.0.0", DriftSeverity::High)?,
            found("ccc", "1.0.0", DriftSeverity::Unknown)?,
            found("bbb", "1.0.0", DriftSeverity::High)?,
        ];
        let dropped = cap_matches(&mut matches, 3);
        assert_eq!(dropped, 1);
        let kept: Vec<(&str, DriftSeverity)> = matches
            .iter()
            .map(|found| (found.name.as_str(), found.severity))
            .collect();
        // `Unknown` zählt wie `Medium` und verdrängt `Low`; Ausgabe sortiert.
        assert_eq!(
            kept,
            [
                ("bbb", DriftSeverity::High),
                ("bbb", DriftSeverity::High),
                ("ccc", DriftSeverity::Unknown),
            ]
        );
        let mut under_limit = vec![found("zzz", "1.0.0", DriftSeverity::Low)?];
        assert_eq!(cap_matches(&mut under_limit, 3), 0);
        assert_eq!(under_limit.len(), 1);
        Ok(())
    }

    #[test]
    fn test_event_limit_is_separate_from_the_advisory_budget() -> TestResult {
        // 100 verwundbare Versionen einer niedrigen und eine hohe Advisory:
        // gemeldet werden höchstens `MAX_EVENTS_PER_POLL`, der hohe Treffer
        // bleibt, der Rest wird gezählt — obwohl `max_advisories` groß ist.
        let fixture = Fixture::new()?;
        fixture.write_advisory(
            "aaa",
            "RUSTSEC-2099-0020.md",
            &advisory_md(
                "RUSTSEC-2099-0020",
                "aaa",
                "patched = []",
                "informational = \"unmaintained\"",
                "Niedrig",
            ),
        )?;
        fixture.write_advisory(
            "zzz",
            "RUSTSEC-2099-0021.md",
            &advisory_md("RUSTSEC-2099-0021", "zzz", "patched = []", CRITICAL, "Hoch"),
        )?;
        let versions: Vec<String> = (1..=100).map(|major| format!("{major}.0.0")).collect();
        let mut packages: Vec<(&str, &str, Option<&str>)> = versions
            .iter()
            .map(|version| ("aaa", version.as_str(), Some(CRATES_IO)))
            .collect();
        packages.push(("zzz", "1.0.0", Some(CRATES_IO)));
        let lock = fixture.write_lockfile("Cargo.lock", &packages)?;

        let reading = sensor(&fixture, vec![lock], DEFAULT_MAX_ADVISORIES)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        let details = drift_details(&reading);
        assert_eq!(details.len(), MAX_EVENTS_PER_POLL);
        assert_eq!(
            details
                .iter()
                .filter(|(severity, _)| *severity == DriftSeverity::High)
                .count(),
            1
        );
        let expected_dropped = 101_usize.saturating_sub(MAX_EVENTS_PER_POLL);
        assert_count(
            &reading,
            TRUNCATED_EVENTS_METRIC,
            count_value(expected_dropped),
        )?;
        assert_count(&reading, TRUNCATED_ADVISORIES_METRIC, 0.0)?;
        assert_eq!(degraded_count(&reading), 1);
        let last = reading
            .events
            .last()
            .ok_or_else(missing("letztes Ereignis"))?;
        assert!(matches!(last.kind, EventKind::SensorDegraded { .. }));
        assert_eq!(reading.events.len(), MAX_EVENTS_PER_POLL.saturating_add(1));
        Ok(())
    }

    #[test]
    fn test_one_lockfile_cannot_exhaust_the_budget_of_another() -> TestResult {
        // Ein (vom Nutzer beschreibbares) Lockfile nennt viele Crates mit
        // Advisories und steht vorn; das Lockfile des Betriebs wird trotzdem
        // vollständig geprüft.
        let fixture = Fixture::new()?;
        for (krate, id) in [
            ("aaa", "RUSTSEC-2099-0070"),
            ("aab", "RUSTSEC-2099-0071"),
            ("aac", "RUSTSEC-2099-0072"),
        ] {
            fixture.write_advisory(
                krate,
                &format!("{id}.md"),
                &advisory_md(id, krate, "patched = []", "", "Lärm"),
            )?;
        }
        write_tokio_advisory(&fixture)?;
        let noisy = fixture.write_lockfile(
            "laut.lock",
            &[
                ("aaa", "1.0.0", Some(CRATES_IO)),
                ("aab", "1.0.0", Some(CRATES_IO)),
                ("aac", "1.0.0", Some(CRATES_IO)),
            ],
        )?;
        let operator =
            fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;

        let reading = sensor(&fixture, vec![noisy, operator], 2)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        let details = drift_details(&reading);
        assert!(
            details
                .iter()
                .any(|(_, detail)| detail.contains("tokio@1.8.0")),
            "{details:?}"
        );
        // `max_advisories = 2` begrenzt auch die Ereignisse: der hohe
        // tokio-Treffer und der erste des lauten Lockfiles bleiben.
        assert_eq!(details.len(), 2);
        assert_count(&reading, TRUNCATED_ADVISORIES_METRIC, 1.0)?;
        assert_count(&reading, TRUNCATED_EVENTS_METRIC, 1.0)?;
        assert_eq!(degraded_count(&reading), 1);
        Ok(())
    }

    #[test]
    fn test_crate_read_for_one_lockfile_is_reused_without_budget() -> TestResult {
        // `tokio` ist schon für das erste Lockfile vollständig gelesen; das
        // zweite zahlt dafür nichts und kann mit Budget 1 noch `demo` lesen.
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        fixture.write_advisory(
            "demo",
            "RUSTSEC-2099-0073.md",
            &advisory_md("RUSTSEC-2099-0073", "demo", "patched = []", "", "Demo"),
        )?;
        let first = fixture.write_lockfile("a.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let second = fixture.write_lockfile(
            "b.lock",
            &[
                ("demo", "1.0.0", Some(CRATES_IO)),
                ("tokio", "1.9.0", Some(CRATES_IO)),
            ],
        )?;
        // Direkt über die Schritte 2 bis 4: mit `max_advisories = 1` ließe
        // die Ereignisgrenze nur einen der drei Treffer durch.
        let sensor = sensor(&fixture, vec![first, second], 1);
        let scope = sensor.handle().scope();
        let base = glob_base(&fixture.db().join(CRATES_DIR)).map_err(ctx("Glob-Basis"))?;
        let mut counters = Counters::default();
        let lockfiles = sensor.read_lockfiles(scope, &mut counters);
        let mut matches = sensor.collect_matches(scope, &base, &lockfiles, &mut counters);
        assert_eq!(counters, Counters::default());
        assert_eq!(cap_matches(&mut matches, usize::MAX), 0);
        let heads: Vec<String> = matches
            .iter()
            .map(|found| format!("{} {}@{}", found.id, found.name, found.version))
            .collect();
        assert_eq!(
            heads,
            [
                "RUSTSEC-2099-0073 demo@1.0.0",
                "RUSTSEC-2021-0124 tokio@1.8.0",
                "RUSTSEC-2021-0124 tokio@1.9.0",
            ]
        );
        Ok(())
    }

    #[test]
    fn test_exhausted_budget_lists_no_further_crate_directory() -> TestResult {
        // Budget 1: `aaa` verbraucht es, `bbb` (drei Dateien) wird gar nicht
        // mehr aufgelistet und zählt einmal — nicht dreimal wie nach einem
        // Glob.
        let fixture = Fixture::new()?;
        fixture.write_advisory(
            "aaa",
            "RUSTSEC-2099-0080.md",
            &advisory_md("RUSTSEC-2099-0080", "aaa", "patched = []", "", "A"),
        )?;
        for id in [
            "RUSTSEC-2099-0081",
            "RUSTSEC-2099-0082",
            "RUSTSEC-2099-0083",
        ] {
            fixture.write_advisory(
                "bbb",
                &format!("{id}.md"),
                &advisory_md(id, "bbb", "patched = []", "", "B"),
            )?;
        }
        let lock = fixture.write_lockfile(
            "Cargo.lock",
            &[
                ("aaa", "1.0.0", Some(CRATES_IO)),
                ("bbb", "1.0.0", Some(CRATES_IO)),
            ],
        )?;
        let reading = sensor(&fixture, vec![lock], 1)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert_count(&reading, TRUNCATED_ADVISORIES_METRIC, 1.0)?;
        assert_eq!(degraded_count(&reading), 1);
        Ok(())
    }

    #[test]
    fn test_advisory_file_limit_is_honoured_and_counted() -> TestResult {
        let fixture = Fixture::new()?;
        for id in [
            "RUSTSEC-2099-0030",
            "RUSTSEC-2099-0031",
            "RUSTSEC-2099-0032",
        ] {
            fixture.write_advisory(
                "demo",
                &format!("{id}.md"),
                &advisory_md(id, "demo", "patched = []", "", "Titel"),
            )?;
        }
        let lock = fixture.write_lockfile("Cargo.lock", &[("demo", "1.0.0", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![lock], 1)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert_eq!(drift_details(&reading).len(), 1);
        assert_count(&reading, TRUNCATED_ADVISORIES_METRIC, 2.0)?;
        assert_eq!(degraded_count(&reading), 1);
        Ok(())
    }

    #[test]
    fn test_zero_limit_reports_nothing_but_degrades() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![lock], 0)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert!(drift_details(&reading).is_empty());
        assert_count(&reading, TRUNCATED_ADVISORIES_METRIC, 1.0)?;
        assert_eq!(degraded_count(&reading), 1);
        Ok(())
    }

    #[test]
    fn test_unreadable_lockfiles_are_skipped_and_counted() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let good = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let missing_lock = fixture.root.join("fehlt").join("Cargo.lock");
        let directory = fixture.root.join("verzeichnis.lock");
        fs::create_dir(&directory).map_err(ctx("Verzeichnis anlegen"))?;
        let too_large = fixture.root.join("gross.lock");
        let size = usize::try_from(harw_dod_readfs::MAX_READ_BYTES)
            .map_err(ctx("Grenze passt in usize"))?
            .saturating_add(1);
        fs::write(&too_large, "#".repeat(size)).map_err(ctx("großes Lockfile schreiben"))?;
        let broken = fixture.root.join("kaputt.lock");
        fs::write(&broken, "[[package]\n").map_err(ctx("kaputtes Lockfile schreiben"))?;

        let reading = sensor(
            &fixture,
            vec![missing_lock, directory, too_large, broken, good],
            16,
        )
        .poll(now()?)
        .map_err(ctx("Abruf"))?;
        assert_eq!(drift_details(&reading).len(), 1);
        assert_count(&reading, LOCKFILE_ERRORS_METRIC, 4.0)?;
        assert_eq!(degraded_count(&reading), 1);
        Ok(())
    }

    #[test]
    fn test_lockfile_with_invalid_entries_still_reports_the_valid_ones() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile(
            "Cargo.lock",
            &[
                ("tokio", "1.8.0", Some(CRATES_IO)),
                ("../tokio", "1.8.0", Some(CRATES_IO)),
            ],
        )?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert_eq!(drift_details(&reading).len(), 1);
        assert_count(&reading, LOCKFILE_ERRORS_METRIC, 1.0)?;
        Ok(())
    }

    #[test]
    fn test_malformed_advisory_does_not_blind_its_siblings() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        fixture.write_advisory("tokio", "RUSTSEC-2099-0040.md", "kein front matter")?;
        // Nennt ein anderes Paket als sein Verzeichnis.
        fixture.write_advisory(
            "tokio",
            "RUSTSEC-2099-0041.md",
            &advisory_md("RUSTSEC-2099-0041", "other", "patched = []", "", "Fremd"),
        )?;
        // Ein Verzeichnis, das wie eine Advisory heißt.
        fs::create_dir_all(fixture.db().join("crates/tokio/RUSTSEC-2099-0042.md"))
            .map_err(ctx("Verzeichnis anlegen"))?;
        // Andere Endung und Entwurf: ignoriert, nicht gezählt.
        fixture.write_advisory("tokio", "RUSTSEC-2099-0043.md.orig", "egal")?;
        fixture.write_advisory("tokio", "RUSTSEC-0000-0000.md", "Entwurf")?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;

        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert_eq!(drift_details(&reading).len(), 1);
        assert_count(&reading, MALFORMED_ADVISORIES_METRIC, 3.0)?;
        assert_eq!(degraded_count(&reading), 1);
        Ok(())
    }

    #[test]
    fn test_crate_without_advisories_yields_nothing() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("serde", "1.0.0", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert!(reading.events.is_empty());
        assert_eq!(reading.samples.len(), 5);
        Ok(())
    }

    #[test]
    fn test_same_package_in_two_lockfiles_is_reported_once() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let first = fixture.write_lockfile("a.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let second = fixture.write_lockfile("b.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![first, second], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert_eq!(drift_details(&reading).len(), 1);
        Ok(())
    }

    #[test]
    fn test_missing_database_fails_the_poll() -> TestResult {
        let fixture = Fixture::new()?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let result = sensor(&fixture, vec![lock], 16).poll(now()?);
        assert!(matches!(result, Err(SensorError::Io(_))));
        Ok(())
    }

    #[test]
    fn test_database_crates_path_that_is_a_file_fails_the_poll() -> TestResult {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.db()).map_err(ctx("Datenbank anlegen"))?;
        fs::write(fixture.db().join("crates"), "keine Datenbank").map_err(ctx("Datei anlegen"))?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let result = sensor(&fixture, vec![lock], 16).poll(now()?);
        assert!(matches!(result, Err(SensorError::MalformedSource)));
        Ok(())
    }

    #[test]
    fn test_empty_database_fails_the_poll() -> TestResult {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.db().join("crates")).map_err(ctx("leeres crates/"))?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let result = sensor(&fixture, vec![lock], 16).poll(now()?);
        assert!(matches!(result, Err(SensorError::MalformedSource)));
        Ok(())
    }

    #[test]
    fn test_wrong_repository_fails_the_poll() -> TestResult {
        // Das `crates/` eines Rust-Workspaces statt einer advisory-db: ohne
        // diese Prüfung fände jeder Glob nichts, und der Abruf sähe aus wie
        // „keine Lücken“.
        let fixture = Fixture::new()?;
        for member in ["harw-core", "harw-cli", "tokio"] {
            let dir = fixture.db().join("crates").join(member);
            fs::create_dir_all(dir.join("src")).map_err(ctx("Mitglied anlegen"))?;
            fs::write(dir.join("Cargo.toml"), "[package]\n").map_err(ctx("Manifest"))?;
        }
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let result = sensor(&fixture, vec![lock], 16).poll(now()?);
        assert!(matches!(result, Err(SensorError::MalformedSource)));
        Ok(())
    }

    #[test]
    fn test_database_probe_looks_past_entries_without_advisories() -> TestResult {
        // Ein Eintrag ohne RUSTSEC-Datei vorn (sortiert) blendet die
        // Prüfung nicht, solange ein späterer der ersten Einträge eine hat.
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.db().join("crates").join("aaa-leer"))
            .map_err(ctx("leeres Crate-Verzeichnis"))?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert_eq!(drift_details(&reading).len(), 1);
        Ok(())
    }

    // Setzt die Änderungszeit von `path` auf `seconds` nach der Epoche.
    fn set_mtime(path: &Path, seconds: u64) -> TestResult {
        let at = std::time::UNIX_EPOCH
            .checked_add(Duration::from_secs(seconds))
            .ok_or_else(missing("Zeitpunkt im Bereich"))?;
        fs::File::open(path)
            .map_err(ctx("Pfad öffnen"))?
            .set_modified(at)
            .map_err(ctx("Änderungszeit setzen"))
    }

    #[test]
    fn test_stale_database_degrades_and_reports_its_age() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        // `crates/` zuletzt vor 30 Tagen (2 592 000 s vor `now`) geändert,
        // kein `.git/FETCH_HEAD`.
        set_mtime(&fixture.db().join("crates"), 1_697_408_000)?;

        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        // Der Treffer wird trotzdem gemeldet; die Veraltung zusätzlich.
        assert_eq!(drift_details(&reading).len(), 1);
        assert_eq!(degraded_count(&reading), 1);
        assert_count(&reading, DATABASE_AGE_METRIC, f64::from(2_592_000_u32))?;
        Ok(())
    }

    #[test]
    fn test_recent_fetch_head_keeps_the_database_fresh() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        // Kein neues Crate-Verzeichnis seit 30 Tagen, aber ein `git fetch`
        // vor einer Stunde: die Datenbank ist aktuell.
        let git = fixture.db().join(".git");
        fs::create_dir_all(&git).map_err(ctx(".git anlegen"))?;
        fs::write(git.join("FETCH_HEAD"), "").map_err(ctx("FETCH_HEAD schreiben"))?;
        // `now` minus eine Stunde bzw. minus 30 Tage.
        set_mtime(&git.join("FETCH_HEAD"), 1_699_996_400)?;
        set_mtime(&fixture.db().join("crates"), 1_697_408_000)?;

        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert_eq!(degraded_count(&reading), 0);
        assert_count(&reading, DATABASE_AGE_METRIC, 3_600.0)?;
        Ok(())
    }

    #[test]
    fn test_age_value_is_saturated_and_unknown_is_maximal() {
        assert!((age_value(Some(SignedDuration::from_secs(90))) - 90.0).abs() < f64::EPSILON);
        assert!(age_value(Some(SignedDuration::ZERO)).abs() < f64::EPSILON);
        assert!(
            (age_value(Some(SignedDuration::from_secs(i64::MAX))) - f64::from(u32::MAX)).abs()
                < f64::EPSILON
        );
        assert!((age_value(None) - f64::from(u32::MAX)).abs() < f64::EPSILON);
        // 14 Tage.
        assert_eq!(STALE_DATABASE_AGE, SignedDuration::from_secs(1_209_600));
    }

    #[test]
    fn test_relative_database_path_is_outside_the_scope() -> TestResult {
        let sensor = AdvisorySensor::new(
            SensorId::from_str(SENSOR_ID),
            AdvisoryConfig {
                lockfiles: Vec::new(),
                db_path: PathBuf::from("relativ/advisory-db"),
                max_advisories: 16,
            },
        );
        let result = sensor.poll(now()?);
        // Fehlt das Verzeichnis, scheitert schon die Auflösung (Io); existiert
        // es relativ zum Arbeitsverzeichnis, liegt es außerhalb des Bereichs.
        assert!(matches!(
            result,
            Err(SensorError::Io(_) | SensorError::OutsideScope)
        ));
        Ok(())
    }

    #[test]
    fn test_poll_is_deterministic_and_uses_the_injected_time() -> TestResult {
        // Zwei Sensoren mit derselben Konfiguration (je erste Prüfung)
        // liefern dieselbe Lesung.
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let first = sensor(&fixture, vec![lock.clone()], 16)
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("erster Abruf"))?;
        let second = sensor(&fixture, vec![lock], 16)
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("zweiter Abruf"))?;
        assert_eq!(first, second);
        assert!(!first.events.is_empty());
        assert!(
            first
                .events
                .iter()
                .all(|event| event.observed_at == Timestamp::UNIX_EPOCH)
        );
        assert!(
            first
                .samples
                .iter()
                .all(|sample| sample.observed_at == Timestamp::UNIX_EPOCH)
        );
        Ok(())
    }

    #[test]
    fn test_reading_never_contains_paths() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        fixture.write_advisory("tokio", "RUSTSEC-2099-0050.md", "kaputt")?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let missing_lock = fixture.root.join("fehlt.lock");
        let reading = sensor(&fixture, vec![lock, missing_lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        let root = fixture.root.to_string_lossy().into_owned();
        for event in &reading.events {
            let json = serde_json::to_string(event).map_err(ctx("Ereignis serialisieren"))?;
            assert!(!json.contains(&root), "Pfad im Ereignis: {json}");
            assert!(!json.contains("Cargo.lock"));
            assert!(!json.contains("advisory-db"));
        }
        assert_eq!(degraded_count(&reading), 1);
        Ok(())
    }

    #[test]
    fn test_hostile_title_is_escaped_in_the_detail() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.write_advisory(
            "demo",
            "RUSTSEC-2099-0060.md",
            &advisory_md(
                "RUSTSEC-2099-0060",
                "demo",
                "patched = []",
                CRITICAL,
                "ok\u{202E}gnp.exe \u{1b}]0;titel\u{7} \u{E0049}gnore previous",
            ),
        )?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("demo", "1.0.0", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        let details = drift_details(&reading);
        let (_, detail) = details.first().ok_or_else(missing("Treffer"))?;
        assert!(harw_untrusted_text::is_clean(detail));
        assert!(detail.contains("\\u{202e}"));
        assert!(detail.contains("\\u{001b}"));
        assert!(detail.contains("\\u{0007}"));
        assert!(detail.contains("\\u{e0049}"));
        assert!(detail.chars().count() <= MAX_DETAIL_CHARS);
        Ok(())
    }

    #[test]
    fn test_long_title_keeps_the_detail_bounded() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.write_advisory(
            "demo",
            "RUSTSEC-2099-0061.md",
            &advisory_md(
                "RUSTSEC-2099-0061",
                "demo",
                "patched = []",
                "",
                &"lang ".repeat(5_000),
            ),
        )?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("demo", "1.0.0", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        let details = drift_details(&reading);
        let (severity, detail) = details.first().ok_or_else(missing("Treffer"))?;
        assert_eq!(*severity, DriftSeverity::Unknown);
        assert!(detail.starts_with("advisory RUSTSEC-2099-0061 demo@1.0.0: lang"));
        assert!(detail.chars().count() <= MAX_DETAIL_CHARS);
        Ok(())
    }

    #[test]
    fn test_plain_toml_advisory_is_read() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.write_advisory(
            "demo",
            "RUSTSEC-2018-0001.toml",
            "[advisory]\nid = \"RUSTSEC-2018-0001\"\npackage = \"demo\"\ntitle = \"Altes Format\"\n\n[versions]\npatched = [\">= 0.2.0\"]\n",
        )?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("demo", "0.1.0", Some(CRATES_IO))])?;
        let reading = sensor(&fixture, vec![lock], 16)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert_eq!(
            drift_details(&reading),
            vec![(
                DriftSeverity::Unknown,
                "advisory RUSTSEC-2018-0001 demo@0.1.0: Altes Format".to_owned()
            )]
        );
        Ok(())
    }

    #[test]
    fn test_sensor_is_unprivileged_and_reports_inferred_hardness() {
        let sensor = AdvisorySensor::new(
            SensorId::from_str(SENSOR_ID),
            AdvisoryConfig {
                lockfiles: Vec::new(),
                db_path: PathBuf::from("/var/lib/advisory-db"),
                max_advisories: 1,
            },
        );
        assert_eq!(sensor.handle().capability(), Capability::ReadWorkspaceGraph);
        assert_eq!(
            sensor.handle().capability().class(),
            harw_dod_cap::CapabilityClass::Unprivileged
        );
        assert_eq!(ADVISORY_HARDNESS, Hardness::Inferred);
    }

    /// `now()` plus `seconds` Sekunden.
    fn later(seconds: i64) -> TestResult<Timestamp> {
        now()?
            .checked_add(SignedDuration::from_secs(seconds))
            .map_err(ctx("Zeitstempel im Bereich"))
    }

    #[test]
    fn test_polls_between_scans_are_empty_and_the_interval_rescans() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let sensor = sensor(&fixture, vec![lock], 16);
        let interval = RESCAN_INTERVAL.as_secs();

        let first = sensor.poll(later(0)?).map_err(ctx("erste Prüfung"))?;
        assert_eq!(drift_details(&first).len(), 1);
        assert_eq!(first.samples.len(), 5);

        // Der nächste Takt des Sentinels (5 s) und der letzte vor dem
        // Intervall: leer, ohne Ereignisse und ohne Samples.
        for seconds in [5, interval.saturating_sub(1)] {
            let between = sensor.poll(later(seconds)?).map_err(ctx("Abruf"))?;
            assert_eq!(between, SensorReading::default());
        }

        // Nach dem Intervall wird erneut geprüft und gemeldet.
        let at = later(interval)?;
        let again = sensor.poll(at).map_err(ctx("zweite Prüfung"))?;
        assert_eq!(drift_details(&again).len(), 1);
        assert!(again.events.iter().all(|event| event.observed_at == at));
        assert_eq!(again.samples.len(), 5);
        Ok(())
    }

    #[test]
    fn test_clock_set_back_rescans() -> TestResult {
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let sensor = sensor(&fixture, vec![lock], 16);

        let first = sensor.poll(later(3_600)?).map_err(ctx("erste Prüfung"))?;
        assert_eq!(drift_details(&first).len(), 1);
        // Die Uhr springt eine Stunde zurück: geprüft wird sofort, statt bis
        // zum Wiedererreichen der alten Zeit zu schweigen.
        let back = sensor
            .poll(later(0)?)
            .map_err(ctx("Prüfung nach Rücksprung"))?;
        assert_eq!(drift_details(&back).len(), 1);
        // Und danach wieder im Takt.
        let between = sensor.poll(later(5)?).map_err(ctx("Abruf"))?;
        assert_eq!(between, SensorReading::default());
        Ok(())
    }

    #[test]
    fn test_failed_scan_does_not_count_as_a_scan() -> TestResult {
        let fixture = Fixture::new()?;
        let lock = fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;
        let sensor = sensor(&fixture, vec![lock], 16);
        // Noch keine Datenbank: der Abruf scheitert.
        assert!(sensor.poll(later(0)?).is_err());
        // Die Datenbank ist da: der nächste Abruf prüft sofort.
        write_tokio_advisory(&fixture)?;
        let reading = sensor.poll(later(5)?).map_err(ctx("Prüfung"))?;
        assert_eq!(drift_details(&reading).len(), 1);
        Ok(())
    }

    #[test]
    fn test_shared_match_is_not_hidden_behind_an_earlier_lockfile() -> TestResult {
        // Ein vorn konfiguriertes, beschreibbares Lockfile nennt 70 frühe hohe
        // Treffer und dieselbe verwundbare tokio-Version wie das Lockfile des
        // Betriebs. Der gemeinsame Treffer zählt mit seinem Rang im Lockfile
        // des Betriebs (0), nicht mit dem im lauten (70) — er bleibt.
        let fixture = Fixture::new()?;
        let names: Vec<String> = (0..70).map(|index| format!("aaa{index:02}")).collect();
        let mut noisy: Vec<(&str, &str, Option<&str>)> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let id = format!("RUSTSEC-2099-{:04}", index.saturating_add(1000));
            fixture.write_advisory(
                name,
                &format!("{id}.md"),
                &advisory_md(&id, name, "patched = []", CRITICAL, "Lärm"),
            )?;
            noisy.push((name.as_str(), "1.0.0", Some(CRATES_IO)));
        }
        write_tokio_advisory(&fixture)?;
        noisy.push(("tokio", "1.8.0", Some(CRATES_IO)));
        let noisy = fixture.write_lockfile("laut.lock", &noisy)?;
        let operator =
            fixture.write_lockfile("Cargo.lock", &[("tokio", "1.8.0", Some(CRATES_IO))])?;

        let reading = sensor(&fixture, vec![noisy, operator], DEFAULT_MAX_ADVISORIES)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        let details = drift_details(&reading);
        assert_eq!(details.len(), MAX_EVENTS_PER_POLL);
        assert!(
            details
                .iter()
                .any(|(_, detail)| detail.contains("tokio@1.8.0")),
            "{details:?}"
        );
        // 71 verschiedene Treffer, 64 gemeldet.
        assert_count(
            &reading,
            TRUNCATED_EVENTS_METRIC,
            count_value(71_usize.saturating_sub(MAX_EVENTS_PER_POLL)),
        )?;
        assert_eq!(degraded_count(&reading), 1);
        Ok(())
    }

    #[test]
    fn test_cap_matches_ranks_each_occurrence_before_merging() -> TestResult {
        let mut matches = vec![
            found_in(0, "aaa", "1.0.0", DriftSeverity::High)?,
            found_in(0, "aab", "1.0.0", DriftSeverity::High)?,
            found_in(0, "aac", "1.0.0", DriftSeverity::High)?,
            found_in(0, "tokio", "1.8.0", DriftSeverity::High)?,
            found_in(1, "tokio", "1.8.0", DriftSeverity::High)?,
        ];
        // Vier verschiedene Treffer, Grenze 2: tokio hat im zweiten
        // Lockfile Rang 0 und bleibt neben dem ersten des ersten.
        assert_eq!(cap_matches(&mut matches, 2), 2);
        let kept: Vec<&str> = matches.iter().map(|found| found.name.as_str()).collect();
        assert_eq!(kept, ["aaa", "tokio"]);

        // Ohne Kürzung wird derselbe Treffer trotzdem nur einmal gemeldet.
        let mut matches = vec![
            found_in(0, "tokio", "1.8.0", DriftSeverity::High)?,
            found_in(1, "tokio", "1.8.0", DriftSeverity::High)?,
            found_in(2, "tokio", "1.8.0", DriftSeverity::High)?,
        ];
        assert_eq!(cap_matches(&mut matches, 2), 0);
        assert_eq!(matches.len(), 1);
        Ok(())
    }

    #[test]
    fn test_max_advisories_also_caps_the_events() -> TestResult {
        // Eine Advisory-Datei, zehn verwundbare Versionen: mit
        // `max_advisories = 3` werden höchstens drei Treffer gemeldet.
        let fixture = Fixture::new()?;
        fixture.write_advisory(
            "demo",
            "RUSTSEC-2099-0090.md",
            &advisory_md("RUSTSEC-2099-0090", "demo", "patched = []", "", "Demo"),
        )?;
        let versions: Vec<String> = (1..=10).map(|major| format!("{major}.0.0")).collect();
        let packages: Vec<(&str, &str, Option<&str>)> = versions
            .iter()
            .map(|version| ("demo", version.as_str(), Some(CRATES_IO)))
            .collect();
        let lock = fixture.write_lockfile("Cargo.lock", &packages)?;

        let reading = sensor(&fixture, vec![lock], 3)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        assert_eq!(drift_details(&reading).len(), 3);
        assert_count(&reading, TRUNCATED_EVENTS_METRIC, 7.0)?;
        assert_count(&reading, TRUNCATED_ADVISORIES_METRIC, 0.0)?;
        assert_eq!(degraded_count(&reading), 1);
        Ok(())
    }

    #[test]
    fn test_lockfile_with_too_many_crates_is_capped_and_counted() -> TestResult {
        // tokio steht vorn und wird geprüft; `demo` steht hinter mehr als
        // `MAX_LOCKED_CRATES` Namen und wird nicht mehr geprüft.
        let fixture = Fixture::new()?;
        write_tokio_advisory(&fixture)?;
        fixture.write_advisory(
            "demo",
            "RUSTSEC-2099-0091.md",
            &advisory_md("RUSTSEC-2099-0091", "demo", "patched = []", "", "Demo"),
        )?;
        let filler: Vec<String> = (0..MAX_LOCKED_CRATES)
            .map(|index| format!("fill{index:05}"))
            .collect();
        let mut packages: Vec<(&str, &str, Option<&str>)> =
            vec![("tokio", "1.8.0", Some(CRATES_IO))];
        packages.extend(
            filler
                .iter()
                .map(|name| (name.as_str(), "1.0.0", Some(CRATES_IO))),
        );
        // Eine weitere Version einer schon gezählten Crate ist kein neuer Name.
        packages.push(("tokio", "1.9.0", Some(CRATES_IO)));
        packages.push(("demo", "1.0.0", Some(CRATES_IO)));
        let lock = fixture.write_lockfile("Cargo.lock", &packages)?;

        let reading = sensor(&fixture, vec![lock], DEFAULT_MAX_ADVISORIES)
            .poll(now()?)
            .map_err(ctx("Abruf"))?;
        let heads: Vec<String> = drift_details(&reading)
            .into_iter()
            .filter_map(|(_, detail)| {
                detail
                    .strip_prefix("advisory ")
                    .and_then(|rest| rest.split_once(':'))
                    .map(|(head, _)| head.to_owned())
            })
            .collect();
        assert_eq!(
            heads,
            [
                "RUSTSEC-2021-0124 tokio@1.8.0",
                "RUSTSEC-2021-0124 tokio@1.9.0"
            ]
        );
        assert_count(&reading, LOCKFILE_ERRORS_METRIC, 1.0)?;
        assert_eq!(degraded_count(&reading), 1);
        Ok(())
    }

    #[test]
    fn test_glob_base_strips_the_root_and_rejects_relative_paths() {
        assert_eq!(
            glob_base(Path::new("/var/lib/db/crates")).ok(),
            Some("var/lib/db/crates".to_owned())
        );
        assert!(matches!(
            glob_base(Path::new("db/crates")),
            Err(SensorError::OutsideScope)
        ));
    }

    #[test]
    fn test_count_value_saturates() {
        assert!((count_value(3) - 3.0).abs() < f64::EPSILON);
        assert!((count_value(usize::MAX) - f64::from(u32::MAX)).abs() < f64::EPSILON);
    }
}
