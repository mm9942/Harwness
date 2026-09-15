//! Die sieben Prüfungen, die [`crate::sensor_suite!`] erzeugt, als aufrufbare
//! Funktionen.
//!
//! # Verantwortungsbereich
//! Jede `assert_*`-Funktion hier implementiert genau eine der sieben
//! Fixture-Prüfungen (plus die optionale achte, „Adversarial") aus der
//! [`crate`]-Moduldoku. [`crate::sensor_suite!`] generiert für jede nur einen
//! dünnen `#[test]`-Wrapper, der die passende Funktion mit dem konkreten
//! Sensor-Typ aufruft — die gesamte Prüflogik steht hier, nicht im Makro,
//! damit sie wie gewöhnlicher Rust-Code lesbar, testbar und dokumentierbar
//! bleibt.
//!
//! Jede Funktion **paniert** bei einer verletzten Prüfung (über `assert!`/
//! `panic!`), statt einen `Result` zurückzugeben — das ist die Form, die
//! `#[test]`-Funktionen erwarten. Ein Aufrufer außerhalb eines Tests (z. B.
//! ein eigenes Diagnosewerkzeug) kann jede Funktion trotzdem direkt rufen und
//! den Panic selbst über `std::panic::catch_unwind` einfangen — genau so
//! belegt diese Crate ihre eigenen roten Zweige (siehe `tests/` im
//! Quellbaum).
//!
//! # Nebenläufigkeit
//! Zustandslose freie Funktionen; `Send + Sync` ohne innere Veränderlichkeit.
//! Jeder Aufruf baut seinen eigenen `ReadScope` und seine eigene
//! Sensor-Instanz; parallele Aufrufe verschiedener Threads (z. B. durch
//! `cargo test` mit mehreren Testthreads) stören sich nicht, solange sie
//! nicht gleichzeitig in dasselbe Fixture-Verzeichnis schreiben — was keine
//! dieser Funktionen tut.
//!
//! # Fehler
//! Kein `Result`; siehe oben. Interne Lese-/Schreibfehler auf den
//! Fixture-Dateien werden über `panic!` mit einer erklärenden Meldung
//! gemeldet, nie stillschweigend verschluckt.
//!
//! # Bereichsdichtheit vs. leerer Bereich — zwei Behauptungen (K48)
//! [`assert_scope_containment`] einerseits und [`assert_scope_tightness`]/
//! [`assert_empty_scope_is_ok`] andererseits prüfen zwei **verschiedene**
//! Dinge, die vor diesem Umbau in einer einzigen Zusicherung steckten
//! (`assert_scope_tightness` maß beides zugleich und war dabei für einen
//! Teil der Sensoren falsch — siehe Knoten K48 in `docs/aw-plan.md`):
//!
//! - **Bereichsdichtheit** ist die Sicherheitszusage des gesamten
//!   Sensor-Teilbaums und die eigentliche Frage: *liest der Sensor
//!   irgendetwas, das außerhalb seines `ReadScope` liegt?* Das gilt für
//!   **jeden** Sensor, ausnahmslos, unabhängig davon, was eine leere Quelle
//!   für ihn bedeutet. [`assert_scope_containment`] misst genau das — nicht
//!   über den *Ausgang* eines Polls auf einem isoliert leeren Verzeichnis,
//!   sondern indem sie einen Bereich auf ein echt leeres Unterverzeichnis
//!   richtet, das **im selben Baum liegt wie die echten Quelldateien** eines
//!   Falls: findet der Sensor sie trotzdem, hat er außerhalb seines
//!   Bereichs gelesen — unabhängig davon, ob dabei `Ok(...)` oder ein
//!   bestimmter `Err(...)` herauskäme.
//! - **„Leerer Bereich ergibt einen Fehler"** ist dagegen keine universelle
//!   Wahrheit, sondern eine Eigenschaft des **konkreten Sensors**.
//!   `harw-dod-thermal` und `harw-dod-cpu` melden eine leere Quelle zu Recht
//!   als `Err(SensorError::SourceUnavailable)` — jeder Linux-Host hat
//!   Thermalzonen bzw. `/proc/stat`, eine leere Quelle ist dort ein
//!   Anzeichen für einen kaputten Bereich. `harw-dod-gpu` dagegen **muss**
//!   `Ok(SensorReading::default())` melden: die meisten Hosts (Server,
//!   CI-Läufer, Container) haben keine GPU, und das ist der gesunde
//!   Normalfall, kein Fehler. Von innerhalb `poll()` sind ein leerer, aber
//!   legitim fehlender Bereich und ein leerer, aber fälschlich am Bereich
//!   vorbei nicht gefundener Bereich **identisch** — ein
//!   `Err(SourceUnavailable)` auf einem isoliert leeren Verzeichnis war
//!   deshalb immer nur ein **Stellvertreter** für Bereichsdichtheit, und ein
//!   schlechter für jeden Sensor, dessen Quelle legitim fehlen darf.
//!   [`EmptyScopeExpectation`] macht die Wahl zwischen beiden Ausprägungen
//!   explizit im Testaufruf von [`crate::sensor_suite!`] selbst (Feld
//!   `empty_scope`), nicht in einem stillen Vorgabewert — und in beiden
//!   Ausprägungen wird tatsächlich etwas geprüft ([`assert_scope_tightness`]
//!   bzw. [`assert_empty_scope_is_ok`]), nie nichts.

use std::path::{Path, PathBuf};

use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_signals::{EventKind, Sensor, SensorReading};
use jiff::Timestamp;

use crate::build_handle;
use crate::fixture_io::{read_expected, reading_to_json};

/// Textmarkierung, die [`assert_content_freedom`] in eine Arbeitskopie des
/// Fixture-Baums schreibt, um Rohtext-Lecks nachzuweisen.
///
/// # Description
/// Öffentlich, damit ein Sensor-Autor, der eine eigene, zusätzliche
/// Inhaltsfreiheits-Prüfung außerhalb von [`crate::sensor_suite!`] schreiben
/// will, dieselbe Markierung wiederverwenden kann, statt eine eigene zu
/// erfinden, die versehentlich mit echtem Quellinhalt kollidiert.
///
/// # Examples
/// ```rust
/// assert!(harw_dod_fixtures::harness::CONTENT_CANARY.starts_with("HARW-FIXTURE"));
/// ```
pub const CONTENT_CANARY: &str = "HARW-FIXTURE-CONTENT-CANARY-7f3c9d21";

/// Mindestlänge eines Datei-Inhalts, ab der [`assert_error_case`] ihn als
/// Leck-Kandidat gegen die Fehlermeldung prüft. Verhindert Fehlalarme durch
/// triviale, häufig vorkommende Kurzsequenzen (z. B. eine einzelne Ziffer).
const MIN_LEAK_CHECK_LEN: usize = 3;

/// Der reservierte Verzeichnisname, den [`assert_scope_containment`] als
/// echt leeres Unterverzeichnis in eine Arbeitskopie jedes Falls einhängt.
///
/// # Description
/// Öffentlich aus demselben Grund wie [`CONTENT_CANARY`]: ein Fixture-Fall,
/// der zufällig ein eigenes Unterverzeichnis mit genau diesem Namen enthält,
/// würde die Prüfung verfälschen (die Kopie enthielte dann keinen echt
/// leeren Bereich mehr, sondern echten Fixture-Inhalt an der Stelle, die als
/// leer gelten soll). Ein Sensor-Autor kann den Namen hier nachschlagen, um
/// ihn in eigenen Fixture-Bäumen zu vermeiden.
///
/// # Examples
/// ```rust
/// assert!(harw_dod_fixtures::harness::SCOPE_CONTAINMENT_PROBE_DIR.starts_with("__harw"));
/// ```
pub const SCOPE_CONTAINMENT_PROBE_DIR: &str = "__harw-fixture-scope-containment-probe__";

/// Die vom Sensor-Autor erklärte Erwartung an einen leeren `ReadScope`.
///
/// # Description
/// Trennt seit Knoten K48 die beiden Behauptungen, die
/// `assert_scope_tightness` zuvor in einer Zusicherung bündelte (siehe
/// [`crate::harness`]-Moduldoku, Abschnitt „Bereichsdichtheit vs. leerer
/// Bereich"): Bereichsdichtheit ([`assert_scope_containment`]) gilt für
/// jeden Sensor unbedingt; **welches** Verhalten bei einer leeren Quelle
/// richtig ist, hängt dagegen vom Sensor ab und wird über dieses Feld im
/// Testaufruf von [`crate::sensor_suite!`] ausdrücklich benannt — nie über
/// einen stillen Vorgabewert, der eine Prüfung unbemerkt abschaltet. In
/// beiden Fällen wird tatsächlich etwas geprüft; es gibt keine dritte,
/// „nichts prüfen"-Ausprägung.
///
/// # Errors
/// Nicht zutreffend — dies ist ein Auswahltyp, keine Funktion.
///
/// # Examples
/// Ein Sensor, dessen Quelle auf jedem Host existiert (z. B. `/proc/stat`),
/// erklärt eine leere Quelle als Fehler — das ist zugleich der Vorgabewert,
/// wenn `empty_scope` im Makroaufruf ganz fehlt:
/// ```rust
/// use harw_dod_fixtures::harness::EmptyScopeExpectation;
///
/// let expectation = EmptyScopeExpectation::SourceUnavailableIsError;
/// assert_eq!(expectation, EmptyScopeExpectation::SourceUnavailableIsError);
/// ```
///
/// Ein Sensor, dessen Quelle legitim fehlen darf (z. B. `harw-dod-gpu` auf
/// einem Host ohne GPU), erklärt das Gegenteil ausdrücklich:
/// ```rust
/// use harw_dod_fixtures::harness::EmptyScopeExpectation;
///
/// let expectation = EmptyScopeExpectation::EmptySourceIsNormal;
/// assert_ne!(expectation, EmptyScopeExpectation::SourceUnavailableIsError);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyScopeExpectation {
    /// Eine leere Quelle ist ein Fehler: [`assert_scope_tightness`] verlangt
    /// `Err(SensorError::SourceUnavailable)`. Heutiges, seit K48
    /// unverändertes Verhalten — und der Vorgabewert, wenn eine
    /// [`crate::sensor_suite!`]-Aufrufstelle das Feld `empty_scope` weglässt.
    SourceUnavailableIsError,
    /// Eine leere Quelle ist der Normalfall: [`assert_empty_scope_is_ok`]
    /// verlangt `Ok(SensorReading::default())` — kein Fehler, aber auch
    /// keine erfundenen Werte. Beispiel: `harw-dod-gpu` auf einem Host ohne
    /// GPU.
    EmptySourceIsNormal,
}

/// Prüfung 1 — Determinismus.
///
/// # Description
/// Für jeden normalen Fixture-Fall unter `fixtures_root` (jedes Verzeichnis
/// außer `malformed/` und `adversarial/`, siehe [`crate`]-Moduldoku): baut
/// einen [`ReadScope`] auf `tree/`, pollt den Sensor **zweimal** mit
/// derselben, aus `expect.json` gelesenen `now`, und verlangt, dass beide
/// Ergebnisse **exakt** gleich sind und **exakt** dem in `expect.json`
/// hinterlegten `SensorReading` entsprechen. Ein Sensor, der die Systemuhr
/// selbst liest oder anderweitig nicht-deterministisch ist, fällt hier auf.
///
/// # Arguments
/// - `build` (`impl Fn(SensorHandle<Bound>) -> S`): baut die konkrete
///   Sensor-Instanz aus einem gebundenen Griff.
/// - `capability` (`Capability`): die Fähigkeit, mit der der Testgriff
///   gebaut wird.
/// - `fixtures_root` (`&Path`): das `fixtures/`-Wurzelverzeichnis.
///
/// # Returns
/// `()` — kein normaler Fixture-Fall verletzt die Prüfung.
///
/// # Panics
/// Wenn kein normaler Fixture-Fall existiert, ein Poll fehlschlägt, zwei
/// Polls mit gleichem `now` unterschiedliche Ergebnisse liefern, oder das
/// Ergebnis von `expect.json` abweicht.
///
/// # Examples
/// ```rust,no_run
/// use harw_dod_cap::Capability;
/// # #[derive(Debug)]
/// # struct Demo(harw_dod_cap::SensorHandle<harw_dod_cap::Bound>);
/// # impl harw_dod_signals::Sensor for Demo {
/// #     fn handle(&self) -> &harw_dod_cap::SensorHandle<harw_dod_cap::Bound> { &self.0 }
/// #     fn poll(&self, _now: jiff::Timestamp) -> Result<harw_dod_signals::SensorReading, harw_dod_cap::SensorError> {
/// #         Ok(harw_dod_signals::SensorReading::default())
/// #     }
/// # }
/// harw_dod_fixtures::harness::assert_determinism(
///     Demo,
///     Capability::ReadSysfsThermal,
///     std::path::Path::new("fixtures"),
/// );
/// ```
pub fn assert_determinism<S, F>(build: F, capability: Capability, fixtures_root: &Path)
where
    S: Sensor,
    F: Fn(SensorHandle<Bound>) -> S,
{
    let cases = list_normal_cases(fixtures_root);
    assert!(
        !cases.is_empty(),
        "kein normaler Fixture-Fall unter {}: mindestens ein <fall>/tree + <fall>/expect.json wird für die Determinismus-Prüfung gebraucht",
        fixtures_root.display()
    );

    for case in cases {
        let expected = read_expected(&case).unwrap_or_else(|e| {
            panic!("Fall {}: expect.json unlesbar: {e}", case.display())
        });
        let scope = ReadScope::from_roots([case.join("tree")]);
        let sensor = build(build_handle(capability, scope));

        let first = sensor.poll(expected.now).unwrap_or_else(|e| {
            panic!("Fall {}: erster Poll fehlgeschlagen: {e}", case.display())
        });
        let second = sensor.poll(expected.now).unwrap_or_else(|e| {
            panic!("Fall {}: zweiter Poll fehlgeschlagen: {e}", case.display())
        });
        assert_eq!(
            first,
            second,
            "Fall {}: zwei Polls mit demselben injizierten now ergaben unterschiedliche SensorReadings — Determinismus verletzt",
            case.display()
        );

        let expected_reading = expected.reading.into_reading();
        assert_eq!(
            first,
            expected_reading,
            "Fall {}: SensorReading weicht von expect.json ab",
            case.display()
        );
    }
}

/// Prüfung 2 — Inhaltsfreiheit.
///
/// # Description
/// Kopiert `tree/` jedes normalen Falls in ein temporäres Verzeichnis und
/// hängt an jede reguläre Datei die Markierung [`CONTENT_CANARY`] als neue
/// Zeile an (das erste Zeilenende bleibt erhalten, sodass
/// `harw_dod_readfs::read_first_line`/`parse_i64`/`parse_u64` unbeeinflusst
/// bleiben, siehe [`crate`]-Moduldoku). Pollt den Sensor gegen diese Kopie
/// und durchsucht sowohl das erfolgreiche `SensorReading` (JSON-serialisiert)
/// als auch — defensiv — `Display`/`Debug` eines etwaigen Fehlers nach der
/// Markierung. Findet sie sich wieder, hat der Sensor Rohtext der Quelle in
/// ein Feld eines `HostSample` oder `SecurityEvent` übernommen.
///
/// # Arguments
/// Siehe [`assert_determinism`].
///
/// # Returns
/// `()` — kein normaler Fixture-Fall verletzt die Prüfung.
///
/// # Panics
/// Wenn kein normaler Fixture-Fall existiert, die Kanarienkopie nicht
/// angelegt werden kann, oder [`CONTENT_CANARY`] im Poll-Ergebnis erscheint.
///
/// # Examples
/// Siehe [`assert_determinism`] für den Aufbau des `build`-Arguments;
/// `assert_content_freedom` wird identisch aufgerufen.
pub fn assert_content_freedom<S, F>(build: F, capability: Capability, fixtures_root: &Path)
where
    S: Sensor,
    F: Fn(SensorHandle<Bound>) -> S,
{
    let cases = list_normal_cases(fixtures_root);
    assert!(
        !cases.is_empty(),
        "kein normaler Fixture-Fall unter {}: mindestens ein Fall wird für die Inhaltsfreiheits-Prüfung gebraucht",
        fixtures_root.display()
    );

    for case in cases {
        let expected = read_expected(&case).unwrap_or_else(|e| {
            panic!("Fall {}: expect.json unlesbar: {e}", case.display())
        });

        let canary_copy = tempfile::tempdir().unwrap_or_else(|e| {
            panic!("Fall {}: Kanarienkopie: temp-Verzeichnis fehlgeschlagen: {e}", case.display())
        });
        copy_tree(&case.join("tree"), canary_copy.path()).unwrap_or_else(|e| {
            panic!("Fall {}: Kopieren des Fixture-Baums fehlgeschlagen: {e}", case.display())
        });
        inject_content_canary(canary_copy.path()).unwrap_or_else(|e| {
            panic!("Fall {}: Kanarienmarkierung schreiben fehlgeschlagen: {e}", case.display())
        });

        let scope = ReadScope::from_roots([canary_copy.path().to_path_buf()]);
        let sensor = build(build_handle(capability, scope));
        let outcome = sensor.poll(expected.now);
        let rendered = render_outcome(&outcome);

        assert!(
            !rendered.contains(CONTENT_CANARY),
            "Fall {}: Rohtext der Quelle (Kanarienmarkierung) im Poll-Ergebnis gefunden — Inhaltsfreiheit verletzt: {rendered}",
            case.display()
        );
    }
}

/// Prüfung 3a — Bereichsdichtheit (Containment).
///
/// # Description
/// Misst die eigentliche Sicherheitszusage — *liest der Sensor irgendetwas,
/// das außerhalb seines `ReadScope` liegt?* — ohne sich auf den *Ausgang*
/// eines Polls auf einem isoliert leeren Verzeichnis zu verlassen (das misst
/// [`assert_scope_tightness`]/[`assert_empty_scope_is_ok`]; siehe
/// [`crate::harness`]-Moduldoku, Abschnitt „Bereichsdichtheit vs. leerer
/// Bereich", für die Begründung, warum das zwei verschiedene Behauptungen
/// sind). Für jeden normalen Fixture-Fall: kopiert `tree/` in ein temporäres
/// Verzeichnis (wie [`assert_content_freedom`]) und hängt darin zusätzlich
/// [`SCOPE_CONTAINMENT_PROBE_DIR`] als **echt leeres** Unterverzeichnis ein —
/// eine Wurzel, die im selben Baum liegt wie die echten Quelldateien dieses
/// Falls, aber selbst nichts enthält. Baut einen [`ReadScope`] **nur** auf
/// dieses leere Unterverzeichnis (nicht auf `tree/` selbst) und pollt den
/// Sensor dagegen.
///
/// Liefert der Sensor `Ok(reading)` mit einem nicht-leeren `SensorReading`,
/// ist das ein Beweis für einen Bereichsverstoß: die gegebene Wurzel enthält
/// nachweislich keine einzige Datei, also kann jeder gemeldete Wert nur aus
/// den Geschwisterdateien in `tree/` stammen — genau die Fehlerklasse eines
/// Sensors, der sein Suchmuster fest verdrahtet und den `ReadScope` nur als
/// nachträglichen Filter benutzt, statt seine Wurzel ausschließlich aus
/// `scope.roots()` abzuleiten. Jedes `Err(...)` gilt dagegen als bestanden,
/// unabhängig von der konkreten Variante: ob der Sensor selbst
/// `SourceUnavailable` meldet oder `ReadScope::open`/`allows` einen
/// fehlgeleiteten Zugriff mit `OutsideScope` abfängt — in beiden Fällen ist
/// kein Fremdinhalt entkommen.
///
/// # Arguments
/// - `build` (`impl Fn(SensorHandle<Bound>) -> S`): siehe
///   [`assert_determinism`].
/// - `capability` (`Capability`): siehe [`assert_determinism`].
/// - `fixtures_root` (`&Path`): das `fixtures/`-Wurzelverzeichnis.
///
/// # Returns
/// `()` — kein normaler Fixture-Fall verletzt die Prüfung.
///
/// # Panics
/// Wenn kein normaler Fixture-Fall existiert, die Arbeitskopie nicht
/// angelegt werden kann, oder ein Poll gegen die leere Probe-Wurzel ein
/// nicht-leeres `SensorReading` liefert.
///
/// # Examples
/// Siehe [`assert_determinism`] für den Aufbau des `build`-Arguments;
/// `assert_scope_containment` wird identisch aufgerufen.
pub fn assert_scope_containment<S, F>(build: F, capability: Capability, fixtures_root: &Path)
where
    S: Sensor,
    F: Fn(SensorHandle<Bound>) -> S,
{
    let cases = list_normal_cases(fixtures_root);
    assert!(
        !cases.is_empty(),
        "kein normaler Fixture-Fall unter {}: mindestens ein Fall wird für die Bereichsdichtheit-Prüfung gebraucht",
        fixtures_root.display()
    );

    for case in cases {
        let probe = tempfile::tempdir().unwrap_or_else(|e| {
            panic!("Fall {}: Probe-Kopie: temp-Verzeichnis fehlgeschlagen: {e}", case.display())
        });
        let tree_copy = probe.path().join("tree");
        copy_tree(&case.join("tree"), &tree_copy).unwrap_or_else(|e| {
            panic!("Fall {}: Kopieren des Fixture-Baums fehlgeschlagen: {e}", case.display())
        });

        let empty_root = tree_copy.join(SCOPE_CONTAINMENT_PROBE_DIR);
        std::fs::create_dir_all(&empty_root).unwrap_or_else(|e| {
            panic!(
                "Fall {}: leere Probe-Wurzel {} anlegen fehlgeschlagen: {e}",
                case.display(),
                empty_root.display()
            )
        });

        let scope = ReadScope::from_roots([empty_root]);
        let sensor = build(build_handle(capability, scope));
        let result = sensor.poll(Timestamp::UNIX_EPOCH);

        if let Ok(reading) = &result {
            assert!(
                reading.samples.is_empty() && reading.events.is_empty(),
                "Fall {}: ein ReadScope auf ein echt leeres Unterverzeichnis von tree/ lieferte ein nicht-leeres SensorReading ({reading:?}) — der Sensor hat Daten außerhalb seines Bereichs gelesen (im selben Baum liegen die echten Quelldateien dieses Falls)",
                case.display()
            );
        }
        // Jedes Err(...) gilt als bestanden — siehe Funktionsdoku.
    }
}

/// Prüfung 3b, Ausprägung [`EmptyScopeExpectation::SourceUnavailableIsError`].
///
/// # Description
/// Baut einen [`ReadScope`] auf ein frisches, tatsächlich leeres
/// Verzeichnis (kein Fixture-Bezug nötig) und verlangt, dass der Sensor
/// `Err(SensorError::SourceUnavailable)` meldet — nicht `Ok(...)`, nicht
/// `Err(SensorError::Io(_))`. Gilt nur für Sensoren, die diese Ausprägung
/// über `empty_scope` erklären (bzw. das Feld weglassen, siehe
/// [`EmptyScopeExpectation`]); die Gegenausprägung prüft
/// [`assert_empty_scope_is_ok`]. Diese Funktion prüft **nicht** mehr die
/// allgemeine Bereichsdichtheit — das übernimmt seit K48
/// [`assert_scope_containment`], unbedingt und für jeden Sensor (siehe
/// [`crate::harness`]-Moduldoku, Abschnitt „Bereichsdichtheit vs. leerer
/// Bereich").
///
/// # Arguments
/// - `build` (`impl Fn(SensorHandle<Bound>) -> S`): siehe
///   [`assert_determinism`].
/// - `capability` (`Capability`): siehe [`assert_determinism`].
///
/// # Returns
/// `()` — der Sensor meldet `SourceUnavailable` für einen leeren Bereich.
///
/// # Panics
/// Wenn das Ergebnis nicht `Err(SensorError::SourceUnavailable)` ist.
///
/// # Examples
/// Siehe [`assert_determinism`] für den Aufbau des `build`-Arguments; diese
/// Funktion braucht kein `fixtures_root`.
pub fn assert_scope_tightness<S, F>(build: F, capability: Capability)
where
    S: Sensor,
    F: Fn(SensorHandle<Bound>) -> S,
{
    let empty = tempfile::tempdir()
        .unwrap_or_else(|e| panic!("leeres Verzeichnis für Scope-Dichtheit-Prüfung: {e}"));
    let scope = ReadScope::from_roots([empty.path().to_path_buf()]);
    let sensor = build(build_handle(capability, scope));

    let result = sensor.poll(Timestamp::UNIX_EPOCH);
    assert!(
        matches!(result, Err(SensorError::SourceUnavailable)),
        "ein Sensor mit leerem ReadScope muss Err(SourceUnavailable) melden, nicht {result:?} — sonst liest er am Bereich vorbei an den echten Host"
    );
}

/// Prüfung 3b, Ausprägung [`EmptyScopeExpectation::EmptySourceIsNormal`].
///
/// # Description
/// Gegenstück zu [`assert_scope_tightness`]: baut ebenfalls einen
/// [`ReadScope`] auf ein frisches, tatsächlich leeres Verzeichnis, verlangt
/// hier aber `Ok(SensorReading::default())` — kein Fehler, aber auch keine
/// erfundenen Werte. Ein Sensor, der bei fehlender Quelle einen Fehler
/// meldet (obwohl er „leere Quelle ist normal" erklärt hat), fällt hier
/// ebenso durch wie einer, der irgendwelche Platzhalter- oder Zufallswerte
/// zurückgibt statt eines wirklich leeren `SensorReading`. Gilt nur für
/// Sensoren, die diese Ausprägung über `empty_scope` erklären (siehe
/// [`EmptyScopeExpectation`]) — z. B. `harw-dod-gpu` auf einem Host ohne
/// GPU.
///
/// # Arguments
/// - `build` (`impl Fn(SensorHandle<Bound>) -> S`): siehe
///   [`assert_determinism`].
/// - `capability` (`Capability`): siehe [`assert_determinism`].
///
/// # Returns
/// `()` — der Sensor meldet `Ok(SensorReading::default())` für einen leeren
/// Bereich.
///
/// # Panics
/// Wenn das Ergebnis nicht exakt `Ok(SensorReading { samples: vec![],
/// events: vec![] })` ist — also bei jedem `Err(...)` und bei jedem `Ok(...)`
/// mit mindestens einem Sample oder Event.
///
/// # Examples
/// Siehe [`assert_determinism`] für den Aufbau des `build`-Arguments; diese
/// Funktion braucht kein `fixtures_root`.
pub fn assert_empty_scope_is_ok<S, F>(build: F, capability: Capability)
where
    S: Sensor,
    F: Fn(SensorHandle<Bound>) -> S,
{
    let empty = tempfile::tempdir()
        .unwrap_or_else(|e| panic!("leeres Verzeichnis für Prüfung 'leere Quelle ist normal': {e}"));
    let scope = ReadScope::from_roots([empty.path().to_path_buf()]);
    let sensor = build(build_handle(capability, scope));

    let result = sensor.poll(Timestamp::UNIX_EPOCH);
    match result {
        Ok(reading) => {
            assert!(
                reading.samples.is_empty() && reading.events.is_empty(),
                "ein Sensor, der 'leere Quelle ist normal' erklärt (EmptyScopeExpectation::EmptySourceIsNormal), muss bei leerem ReadScope Ok(SensorReading::default()) liefern, nicht {reading:?} — erfundene Werte ohne echte Quelle sind ebenso ein Fehler wie ein gemeldeter Err"
            );
        }
        Err(err) => panic!(
            "ein Sensor, der 'leere Quelle ist normal' erklärt (EmptyScopeExpectation::EmptySourceIsNormal), muss bei leerem ReadScope Ok(SensorReading::default()) liefern, nicht Err({err:?})"
        ),
    }
}

/// Prüfung 4 — Redaktion.
///
/// # Description
/// Pollt jeden normalen Fall gegen seinen eigenen, kanonisierten `tree/`-
/// Pfad (kein zusätzliches Kopieren nötig — der bereits eingecheckte
/// Fixture-Pfad ist selbst schon maschinen- und checkout-spezifisch genug,
/// um als Stellvertreter für einen echten, absoluten Host-Pfad zu dienen)
/// und verlangt, dass dieser rohe, absolute Pfad **nirgends** im
/// serialisierten Ergebnis erscheint. Ein Sensor, der Pfad- oder
/// Namensfelder (`EventKind::FileWrite::path` u. Ä.) unverändert aus dem
/// aufgelösten Dateisystempfad befüllt statt aus einer stabilen,
/// redigierten Form, fällt hier auf.
///
/// # Arguments
/// Siehe [`assert_determinism`].
///
/// # Returns
/// `()` — kein normaler Fixture-Fall verletzt die Prüfung.
///
/// # Panics
/// Wenn kein normaler Fixture-Fall existiert oder der rohe `tree/`-Pfad im
/// Poll-Ergebnis erscheint.
///
/// # Examples
/// Siehe [`assert_determinism`] für den Aufbau des `build`-Arguments;
/// `assert_redaction` wird identisch aufgerufen.
pub fn assert_redaction<S, F>(build: F, capability: Capability, fixtures_root: &Path)
where
    S: Sensor,
    F: Fn(SensorHandle<Bound>) -> S,
{
    let cases = list_normal_cases(fixtures_root);
    assert!(
        !cases.is_empty(),
        "kein normaler Fixture-Fall unter {}: mindestens ein Fall wird für die Redaktions-Prüfung gebraucht",
        fixtures_root.display()
    );

    for case in cases {
        let expected = read_expected(&case).unwrap_or_else(|e| {
            panic!("Fall {}: expect.json unlesbar: {e}", case.display())
        });
        let tree = case.join("tree");
        let canonical = tree.canonicalize().unwrap_or_else(|e| {
            panic!("Fall {}: tree/ kanonisieren fehlgeschlagen: {e}", case.display())
        });

        let scope = ReadScope::from_roots([canonical.clone()]);
        let sensor = build(build_handle(capability, scope));
        let outcome = sensor.poll(expected.now);
        let rendered = render_outcome(&outcome);

        let raw_path = canonical.to_string_lossy().into_owned();
        assert!(
            !rendered.contains(raw_path.as_str()),
            "Fall {}: roher, absoluter Host-Pfad '{raw_path}' im Poll-Ergebnis gefunden — Redaktion verletzt: {rendered}",
            case.display()
        );
    }
}

/// Prüfung 5 — Kardinalität.
///
/// # Description
/// Pollt jeden normalen Fall und zählt die Anzahl **verschiedener
/// Labelkombinationen** im Ergebnis: je ein Eintrag pro unterschiedlichem
/// `HostSample::metric`-Wert und je ein Eintrag pro unterschiedlicher
/// `EventKind`-Variante unter den beobachteten `SecurityEvent`s (siehe
/// [`distinct_label_count`]). Verlangt, dass diese Zahl `max_cardinality`
/// nicht überschreitet.
///
/// # Warum `usize` statt `harw_observe::Cardinality`
/// `harw-dod-fixtures` hängt bewusst nicht von `harw-observe` ab (siehe
/// Vorgabe „darf selbst nichts Schweres ziehen"). Die tatsächliche, über
/// `harw_macros::metrics!` deklarierte `Cardinality` eines Sensors lebt in
/// dessen eigener `MetricKey`-Konstante; dieser Parameter ist der
/// Fixture-seitige Spiegel dieser Zahl, den ein Sensor-Autor beim
/// `sensor_suite!`-Aufruf einträgt.
///
/// # Arguments
/// - `build`, `capability`, `fixtures_root`: siehe [`assert_determinism`].
/// - `max_cardinality` (`usize`): die vom Sensor-Autor deklarierte
///   Obergrenze verschiedener Labelkombinationen **je Poll**.
///
/// # Returns
/// `()` — kein normaler Fixture-Fall überschreitet `max_cardinality`.
///
/// # Panics
/// Wenn kein normaler Fixture-Fall existiert oder ein Fall mehr als
/// `max_cardinality` verschiedene Labelkombinationen erzeugt.
///
/// # Examples
/// Siehe [`assert_determinism`] für den Aufbau des `build`-Arguments.
pub fn assert_cardinality<S, F>(
    build: F,
    capability: Capability,
    fixtures_root: &Path,
    max_cardinality: usize,
) where
    S: Sensor,
    F: Fn(SensorHandle<Bound>) -> S,
{
    let cases = list_normal_cases(fixtures_root);
    assert!(
        !cases.is_empty(),
        "kein normaler Fixture-Fall unter {}: mindestens ein Fall wird für die Kardinalitäts-Prüfung gebraucht",
        fixtures_root.display()
    );

    for case in cases {
        let expected = read_expected(&case).unwrap_or_else(|e| {
            panic!("Fall {}: expect.json unlesbar: {e}", case.display())
        });
        let scope = ReadScope::from_roots([case.join("tree")]);
        let sensor = build(build_handle(capability, scope));
        let reading = sensor.poll(expected.now).unwrap_or_else(|e| {
            panic!("Fall {}: Poll fehlgeschlagen: {e}", case.display())
        });

        let distinct = distinct_label_count(&reading);
        assert!(
            distinct <= max_cardinality,
            "Fall {}: {distinct} verschiedene Labelkombinationen überschreiten die deklarierte Cardinality {max_cardinality}",
            case.display()
        );
    }
}

/// Prüfung 6 — Fehlerfall.
///
/// # Description
/// Für jeden Fall unter `fixtures_root/malformed/` (jeder direkte
/// Unterordner ist selbst ein Baum, analog zu `tree/` bei einem normalen
/// Fall — siehe [`crate`]-Moduldoku): baut einen [`ReadScope`] darauf und
/// verlangt `Err(SensorError::MalformedSource)` — nicht `Ok(...)`, nicht
/// `Err(SensorError::Io(_))`, kein Panic. Prüft zusätzlich, dass keiner der
/// rohen Dateiinhalte dieses Falls (ab [`MIN_LEAK_CHECK_LEN`] Zeichen) in der
/// `Display`- oder `Debug`-Form des Fehlers auftaucht.
///
/// # Arguments
/// Siehe [`assert_determinism`], jedoch bezieht sich `fixtures_root` hier
/// auf `fixtures_root/malformed/`.
///
/// # Returns
/// `()` — jeder Fall unter `malformed/` verletzt die Prüfung nicht.
///
/// # Panics
/// Wenn `fixtures_root/malformed/` keinen Fall enthält, ein Fall nicht mit
/// `Err(SensorError::MalformedSource)` scheitert, oder die Fehlermeldung
/// Dateiinhalt des Falls enthält.
///
/// # Examples
/// Siehe [`assert_determinism`] für den Aufbau des `build`-Arguments.
pub fn assert_error_case<S, F>(build: F, capability: Capability, fixtures_root: &Path)
where
    S: Sensor,
    F: Fn(SensorHandle<Bound>) -> S,
{
    let malformed_root = fixtures_root.join("malformed");
    let cases = list_subdirs(&malformed_root);
    assert!(
        !cases.is_empty(),
        "fixtures/malformed unter {} muss mindestens einen Fall enthalten (Prüfung 'Fehlerfall')",
        malformed_root.display()
    );

    for case in cases {
        let raw_contents = collect_file_contents(&case);
        let scope = ReadScope::from_roots([case.clone()]);
        let sensor = build(build_handle(capability, scope));
        let result = sensor.poll(Timestamp::UNIX_EPOCH);

        assert!(
            matches!(result, Err(SensorError::MalformedSource)),
            "Fall {}: erwartet Err(MalformedSource), erhalten {result:?}",
            case.display()
        );

        if let Err(err) = &result {
            let rendered = format!("{err}{err:?}");
            for content in &raw_contents {
                assert!(
                    !rendered.contains(content.as_str()),
                    "Fall {}: Fehlermeldung enthält Dateiinhalt des Falls — Inhaltsfreiheit im Fehlerfall verletzt",
                    case.display()
                );
            }
        }
    }
}

/// Zusätzliche, optionale Prüfung — Adversarial.
///
/// # Description
/// Existiert `fixtures_root/adversarial/` nicht, tut diese Funktion nichts
/// (kein Fehlschlag: die Prüfung ist optional, siehe [`crate`]-Moduldoku).
/// Andernfalls wird jeder Unterordner darin — analog zu `malformed/` ein
/// eigenständiger Baum — gepollt, und `poll()` darf **unter keinen
/// Umständen** panieren, unabhängig davon, ob das Ergebnis `Ok` oder `Err`
/// ist.
///
/// # Arguments
/// Siehe [`assert_error_case`], jedoch bezieht sich `fixtures_root` hier auf
/// `fixtures_root/adversarial/`.
///
/// # Returns
/// `()` — kein Fall unter `adversarial/` löst einen Panic aus (oder das
/// Verzeichnis fehlt).
///
/// # Panics
/// Wenn `poll()` für einen Fall unter `adversarial/` paniert.
///
/// # Examples
/// Siehe [`assert_determinism`] für den Aufbau des `build`-Arguments.
pub fn assert_adversarial_survives<S, F>(build: F, capability: Capability, fixtures_root: &Path)
where
    S: Sensor,
    F: Fn(SensorHandle<Bound>) -> S,
{
    let adversarial_root = fixtures_root.join("adversarial");
    if !adversarial_root.is_dir() {
        return;
    }

    for case in list_subdirs(&adversarial_root) {
        let scope = ReadScope::from_roots([case.clone()]);
        let sensor = build(build_handle(capability, scope));

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sensor.poll(Timestamp::UNIX_EPOCH)
        }));
        assert!(
            outcome.is_ok(),
            "Fall {}: poll() ist paniert — ein adversarial erzeugter Baum darf niemals einen Panic auslösen",
            case.display()
        );
    }
}

// ---------------------------------------------------------------------
// Interne Helfer
// ---------------------------------------------------------------------

/// Listet die normalen Fixture-Fälle unter `fixtures_root`: jedes direkte
/// Unterverzeichnis außer `malformed` und `adversarial`, sortiert für
/// deterministische Iteration.
fn list_normal_cases(fixtures_root: &Path) -> Vec<PathBuf> {
    list_subdirs(fixtures_root)
        .into_iter()
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name != "malformed" && name != "adversarial"
        })
        .collect()
}

/// Listet alle direkten Unterverzeichnisse von `dir`, sortiert. Liefert eine
/// leere Liste, wenn `dir` nicht existiert oder nicht lesbar ist — das ist
/// bei den Aufrufstellen hier kein Fehler, sondern führt zu einer eigenen,
/// aussagekräftigen `assert!`-Meldung.
fn list_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Rendert ein Poll-Ergebnis als durchsuchbaren Text: die kompakte
/// JSON-Form bei `Ok`, `Display` und `Debug` verkettet bei `Err`.
fn render_outcome(outcome: &Result<SensorReading, SensorError>) -> String {
    match outcome {
        Ok(reading) => reading_to_json(reading).unwrap_or_default(),
        Err(err) => format!("{err}{err:?}"),
    }
}

/// Zählt die Anzahl verschiedener Labelkombinationen eines `SensorReading`:
/// ein Eintrag je unterschiedlichem `HostSample::metric` und je
/// unterschiedlicher `EventKind`-Variante unter den Events.
fn distinct_label_count(reading: &SensorReading) -> usize {
    let mut combos: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for sample in &reading.samples {
        combos.insert(format!("sample:{}", sample.metric));
    }
    for event in &reading.events {
        combos.insert(format!("event:{}", event_kind_label(&event.kind)));
    }
    combos.len()
}

/// Der stabile, kebab-case Bezeichner einer `EventKind`-Variante (identisch
/// zum `#[serde(tag = "kind")]`-Wert aus `harw_dod_signals::event`).
fn event_kind_label(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::ProcessExec { .. } => "process-exec",
        EventKind::FileWrite { .. } => "file-write",
        EventKind::EgressFlow { .. } => "egress-flow",
        EventKind::ListenerOpened { .. } => "listener-opened",
        EventKind::AuthEvent { .. } => "auth-event",
        EventKind::StructureDrift { .. } => "structure-drift",
        EventKind::SensorDegraded { .. } => "sensor-degraded",
    }
}

/// Kopiert einen Verzeichnisbaum rekursiv. Symlinks werden aufgelöst und als
/// gewöhnliche Dateien kopiert (kein Symlink im Ziel) — die Kopie dient
/// ausschließlich als Wegwerf-Arbeitskopie für [`assert_content_freedom`],
/// nicht als zweite Quelle der Wahrheit.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &dest_path)?;
        } else {
            let bytes = std::fs::read(entry.path())?;
            std::fs::write(&dest_path, bytes)?;
        }
    }
    Ok(())
}

/// Listet alle regulären Dateien unter `root`, rekursiv.
fn walk_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// Hängt [`CONTENT_CANARY`] als eigene Zeile an jede reguläre Datei unter
/// `root` an. Verändert nie die erste Zeile einer Datei — sysfs-typische
/// Leser (`read_first_line`, `parse_i64`, `parse_u64`) bleiben unbeeinflusst.
fn inject_content_canary(root: &Path) -> std::io::Result<()> {
    for file in walk_files(root)? {
        let mut content = std::fs::read(&file)?;
        content.extend_from_slice(format!("\n{CONTENT_CANARY}\n").as_bytes());
        std::fs::write(&file, content)?;
    }
    Ok(())
}

/// Sammelt die getrimmten Inhalte aller regulären Dateien unter `dir`, die
/// mindestens [`MIN_LEAK_CHECK_LEN`] Zeichen lang sind. Nicht-UTF-8-Dateien
/// werden übersprungen (defensiv — keine bekannte Fixture-Quelle ist binär).
fn collect_file_contents(dir: &Path) -> Vec<String> {
    let Ok(files) = walk_files(dir) else {
        return Vec::new();
    };
    files
        .into_iter()
        .filter_map(|file| std::fs::read_to_string(&file).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| text.len() >= MIN_LEAK_CHECK_LEN)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicI64, Ordering};

    use harw_dod_cap::{Bound, Capability, SensorHandle};
    use harw_dod_signals::{HostSample, Sensor, SensorReading};
    use harw_types::SensorId;
    use jiff::Timestamp;

    use super::*;

    // ---- eine minimal korrekte Beispielimplementierung ----

    /// Liest die erste Zeile der Datei `value` unterhalb der einzigen
    /// Scope-Wurzel als `i64` und meldet sie unter einem statischen
    /// Metriknamen. Absichtlich minimal: genau die Form, die alle sieben
    /// Prüfungen bestehen soll (Ausprägung
    /// `EmptyScopeExpectation::SourceUnavailableIsError`: `GoodSensor`
    /// meldet eine fehlende Quelle als `SourceUnavailable`).
    #[derive(Debug)]
    struct GoodSensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for GoodSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for GoodSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
            let Some(root) = self.handle.scope().roots().next() else {
                return Err(SensorError::SourceUnavailable);
            };
            let path = root.join("value");
            match harw_dod_readfs::parse_i64(self.handle.scope(), &path) {
                Ok(value) => Ok(SensorReading {
                    samples: vec![HostSample {
                        sensor: self.handle.id().clone(),
                        observed_at: now,
                        metric: Cow::Borrowed("fixture_selftest_value"),
                        value: value as f64,
                    }],
                    events: vec![],
                }),
                Err(harw_dod_readfs::ReadFsError::Scope(SensorError::Io(io_err)))
                    if io_err.kind() == std::io::ErrorKind::NotFound =>
                {
                    Err(SensorError::SourceUnavailable)
                }
                Err(harw_dod_readfs::ReadFsError::Scope(inner)) => Err(inner),
                Err(_) => Err(SensorError::MalformedSource),
            }
        }
    }

    fn write_case(fixtures_root: &Path, name: &str, value_content: &str, reading: &SensorReading) {
        let case = fixtures_root.join(name);
        let tree = case.join("tree");
        std::fs::create_dir_all(&tree).expect("tree/ anlegen");
        std::fs::write(tree.join("value"), value_content).expect("value schreiben");
        crate::fixture_io::write_expected(&case, Timestamp::UNIX_EPOCH, reading)
            .expect("expect.json schreiben");
    }

    fn good_reading(value: f64) -> SensorReading {
        SensorReading {
            samples: vec![HostSample {
                sensor: SensorId::from_str(crate::FIXTURE_SENSOR_ID),
                observed_at: Timestamp::UNIX_EPOCH,
                metric: Cow::Borrowed("fixture_selftest_value"),
                value,
            }],
            events: vec![],
        }
    }

    #[test]
    fn test_good_sensor_passes_all_eight_checks() {
        let root = tempfile::tempdir().expect("tempdir");
        write_case(root.path(), "typical", "45000\n", &good_reading(45000.0));
        std::fs::create_dir_all(root.path().join("malformed/not-a-number"))
            .expect("malformed dir");
        std::fs::write(
            root.path().join("malformed/not-a-number/value"),
            "not-a-number\n",
        )
        .expect("malformed value");
        std::fs::create_dir_all(root.path().join("adversarial/overflow"))
            .expect("adversarial dir");
        std::fs::write(
            root.path().join("adversarial/overflow/value"),
            "-99999999999999999999999999\n",
        )
        .expect("adversarial value");

        assert_determinism(GoodSensor::from, Capability::ReadProcStat, root.path());
        assert_content_freedom(GoodSensor::from, Capability::ReadProcStat, root.path());
        assert_scope_containment(GoodSensor::from, Capability::ReadProcStat, root.path());
        assert_scope_tightness(GoodSensor::from, Capability::ReadProcStat);
        assert_redaction(GoodSensor::from, Capability::ReadProcStat, root.path());
        assert_cardinality(GoodSensor::from, Capability::ReadProcStat, root.path(), 4);
        assert_error_case(GoodSensor::from, Capability::ReadProcStat, root.path());
        assert_adversarial_survives(GoodSensor::from, Capability::ReadProcStat, root.path());
    }

    // ---- je ein absichtlich fehlerhafter Beispielsensor pro Prüfung ----

    #[derive(Debug)]
    struct BadDeterminismSensor {
        handle: SensorHandle<Bound>,
        counter: AtomicI64,
    }

    impl From<SensorHandle<Bound>> for BadDeterminismSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self {
                handle,
                counter: AtomicI64::new(0),
            }
        }
    }

    impl Sensor for BadDeterminismSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
            // Verstoß: der Rückgabewert ändert sich bei jedem Aufruf, ohne
            // dass sich `now` oder die Quelle ändern — genau das, was die
            // Determinismus-Prüfung verhindern soll.
            let value = self.counter.fetch_add(1, Ordering::SeqCst) as f64;
            Ok(SensorReading {
                samples: vec![HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Borrowed("bad_counter"),
                    value,
                }],
                events: vec![],
            })
        }
    }

    #[test]
    fn test_bad_determinism_sensor_fails_determinism_check() {
        let root = tempfile::tempdir().expect("tempdir");
        write_case(root.path(), "typical", "1\n", &good_reading(1.0));

        let result = std::panic::catch_unwind(|| {
            assert_determinism(BadDeterminismSensor::from, Capability::ReadProcStat, root.path());
        });
        assert!(
            result.is_err(),
            "ein nicht-deterministischer Sensor muss die Determinismus-Prüfung zum Scheitern bringen"
        );
    }

    #[derive(Debug)]
    struct BadContentFreeSensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for BadContentFreeSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for BadContentFreeSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
            let Some(root) = self.handle.scope().roots().next() else {
                return Err(SensorError::SourceUnavailable);
            };
            let raw = harw_dod_readfs::read_to_string(self.handle.scope(), &root.join("value"))
                .map_err(|_| SensorError::SourceUnavailable)?;
            // Verstoß: der volle Rohtext der Quelle landet unverändert in
            // einem Ereignisfeld statt in einem abgeleiteten Wert.
            Ok(SensorReading {
                samples: vec![],
                events: vec![harw_dod_signals::SecurityEvent {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    actor: None,
                    kind: EventKind::StructureDrift {
                        severity: harw_dod_signals::DriftSeverity::Unknown,
                        detail: raw,
                    },
                }],
            })
        }
    }

    #[test]
    fn test_bad_content_free_sensor_fails_content_freedom_check() {
        let root = tempfile::tempdir().expect("tempdir");
        write_case(
            root.path(),
            "typical",
            "1\n",
            &SensorReading {
                samples: vec![],
                events: vec![harw_dod_signals::SecurityEvent {
                    sensor: SensorId::from_str(crate::FIXTURE_SENSOR_ID),
                    observed_at: Timestamp::UNIX_EPOCH,
                    actor: None,
                    kind: EventKind::StructureDrift {
                        severity: harw_dod_signals::DriftSeverity::Unknown,
                        detail: "1\n".to_owned(),
                    },
                }],
            },
        );

        let result = std::panic::catch_unwind(|| {
            assert_content_freedom(BadContentFreeSensor::from, Capability::ReadProcStat, root.path());
        });
        assert!(
            result.is_err(),
            "ein Sensor, der Rohtext der Quelle übernimmt, muss die Inhaltsfreiheits-Prüfung zum Scheitern bringen"
        );
    }

    #[derive(Debug)]
    struct AlwaysOkSensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for AlwaysOkSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for AlwaysOkSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
            // Verstoß: ignoriert den (hier leeren) ReadScope vollständig und
            // liefert immer ein Ergebnis.
            Ok(SensorReading {
                samples: vec![HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Borrowed("always_one"),
                    value: 1.0,
                }],
                events: vec![],
            })
        }
    }

    #[test]
    fn test_always_ok_sensor_fails_scope_tightness_check() {
        let result = std::panic::catch_unwind(|| {
            assert_scope_tightness(AlwaysOkSensor::from, Capability::ReadProcStat);
        });
        assert!(
            result.is_err(),
            "ein Sensor, der auch bei leerem ReadScope Ok(...) liefert, muss die Scope-Dichtheit-Prüfung zum Scheitern bringen"
        );
    }

    #[derive(Debug)]
    struct BadRedactionSensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for BadRedactionSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for BadRedactionSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
            let root = self
                .handle
                .scope()
                .roots()
                .next()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            // Verstoß: der rohe, absolute Scope-Pfad landet unverändert in
            // einem Pfadfeld statt in redigierter Form.
            Ok(SensorReading {
                samples: vec![],
                events: vec![harw_dod_signals::SecurityEvent {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    actor: None,
                    kind: EventKind::FileWrite { path: root },
                }],
            })
        }
    }

    #[test]
    fn test_bad_redaction_sensor_fails_redaction_check() {
        let root = tempfile::tempdir().expect("tempdir");
        write_case(
            root.path(),
            "typical",
            "1\n",
            &SensorReading::default(),
        );

        let result = std::panic::catch_unwind(|| {
            assert_redaction(BadRedactionSensor::from, Capability::ReadProcStat, root.path());
        });
        assert!(
            result.is_err(),
            "ein Sensor, der den rohen Scope-Pfad emittiert, muss die Redaktions-Prüfung zum Scheitern bringen"
        );
    }

    #[derive(Debug)]
    struct BadCardinalitySensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for BadCardinalitySensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for BadCardinalitySensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
            // Verstoß: erzeugt pro Poll fünfzig verschiedene, zur Laufzeit
            // gebaute Metriknamen statt eines festen, statischen Namens.
            let samples = (0..50)
                .map(|i| HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Owned(format!("device_{i}_metric")),
                    value: f64::from(i),
                })
                .collect();
            Ok(SensorReading {
                samples,
                events: vec![],
            })
        }
    }

    #[test]
    fn test_bad_cardinality_sensor_fails_cardinality_check() {
        let root = tempfile::tempdir().expect("tempdir");
        write_case(root.path(), "typical", "1\n", &SensorReading::default());

        let result = std::panic::catch_unwind(|| {
            assert_cardinality(BadCardinalitySensor::from, Capability::ReadProcStat, root.path(), 5);
        });
        assert!(
            result.is_err(),
            "ein Sensor mit fünfzig Labelkombinationen muss die Kardinalitäts-Prüfung bei max_cardinality=5 zum Scheitern bringen"
        );
    }

    #[derive(Debug)]
    struct BadErrorMappingSensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for BadErrorMappingSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for BadErrorMappingSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
            // Verstoß: meldet nie MalformedSource, unabhängig vom Inhalt der
            // Quelle.
            Ok(SensorReading::default())
        }
    }

    #[test]
    fn test_bad_error_mapping_sensor_fails_error_case_check() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("malformed/any")).expect("malformed dir");
        std::fs::write(root.path().join("malformed/any/value"), "irrelevant\n")
            .expect("malformed value");

        let result = std::panic::catch_unwind(|| {
            assert_error_case(BadErrorMappingSensor::from, Capability::ReadProcStat, root.path());
        });
        assert!(
            result.is_err(),
            "ein Sensor, der nie MalformedSource meldet, muss die Fehlerfall-Prüfung zum Scheitern bringen"
        );
    }

    #[derive(Debug)]
    struct PanicsOnAnythingSensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for PanicsOnAnythingSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for PanicsOnAnythingSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
            panic!("dieser Sensor paniert immer — genau das prüft assert_adversarial_survives");
        }
    }

    #[test]
    fn test_panicking_sensor_fails_adversarial_survives_check() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("adversarial/weird")).expect("adversarial dir");
        std::fs::write(root.path().join("adversarial/weird/value"), "x").expect("value");

        let result = std::panic::catch_unwind(|| {
            assert_adversarial_survives(PanicsOnAnythingSensor::from, Capability::ReadProcStat, root.path());
        });
        assert!(
            result.is_err(),
            "ein Sensor, der auf einem adversarial-Baum paniert, muss die Adversarial-Prüfung zum Scheitern bringen"
        );
    }

    #[test]
    fn test_assert_adversarial_survives_is_noop_without_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        // Kein adversarial/-Verzeichnis vorhanden: darf nicht panieren.
        assert_adversarial_survives(GoodSensor::from, Capability::ReadProcStat, root.path());
    }

    #[test]
    fn test_distinct_label_count_counts_metrics_and_event_kinds() {
        let reading = SensorReading {
            samples: vec![
                HostSample {
                    sensor: SensorId::from_str("x"),
                    observed_at: Timestamp::UNIX_EPOCH,
                    metric: Cow::Borrowed("a"),
                    value: 1.0,
                },
                HostSample {
                    sensor: SensorId::from_str("x"),
                    observed_at: Timestamp::UNIX_EPOCH,
                    metric: Cow::Borrowed("a"),
                    value: 2.0,
                },
                HostSample {
                    sensor: SensorId::from_str("x"),
                    observed_at: Timestamp::UNIX_EPOCH,
                    metric: Cow::Borrowed("b"),
                    value: 3.0,
                },
            ],
            events: vec![],
        };
        assert_eq!(distinct_label_count(&reading), 2);
    }

    // ---- Prüfung 3a (Bereichsdichtheit/Containment) — K48 ----

    /// Liest über `std::fs` direkt aus dem **übergeordneten** Verzeichnis der
    /// gegebenen Bereichswurzel statt aus der Wurzel selbst — am `ReadScope`
    /// vorbei, ohne `scope.allows`/`scope.open` je zu befragen. Genau die
    /// Fehlerklasse, die [`assert_scope_containment`] fangen soll: eine
    /// Wurzelberechnung, die nicht exakt bei `scope.roots()` bleibt.
    #[derive(Debug)]
    struct LeaksParentDirSensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for LeaksParentDirSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for LeaksParentDirSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
            let Some(root) = self.handle.scope().roots().next() else {
                return Err(SensorError::SourceUnavailable);
            };
            let Some(parent) = root.parent() else {
                return Err(SensorError::SourceUnavailable);
            };
            // Verstoss: std::fs direkt, am ReadScope vorbei, eine Ebene
            // oberhalb der gegebenen Bereichswurzel.
            match std::fs::read_to_string(parent.join("value")) {
                Ok(content) => {
                    let value: f64 = content.trim().parse().unwrap_or_default();
                    Ok(SensorReading {
                        samples: vec![HostSample {
                            sensor: self.handle.id().clone(),
                            observed_at: now,
                            metric: Cow::Borrowed("leaked_value"),
                            value,
                        }],
                        events: vec![],
                    })
                }
                Err(_) => Err(SensorError::SourceUnavailable),
            }
        }
    }

    #[test]
    fn test_leaking_sensor_fails_scope_containment_check() {
        let root = tempfile::tempdir().expect("tempdir");
        write_case(root.path(), "typical", "45000\n", &good_reading(45000.0));

        let result = std::panic::catch_unwind(|| {
            assert_scope_containment(LeaksParentDirSensor::from, Capability::ReadProcStat, root.path());
        });
        assert!(
            result.is_err(),
            "ein Sensor, der Dateien oberhalb seiner Bereichswurzel liest, muss die Bereichsdichtheit-Prüfung zum Scheitern bringen"
        );
    }

    #[test]
    fn test_good_sensor_passes_scope_containment_check() {
        let root = tempfile::tempdir().expect("tempdir");
        write_case(root.path(), "typical", "45000\n", &good_reading(45000.0));
        // GoodSensor liest ausschliesslich über scope.roots()/scope selbst —
        // muss unabhaengig von der erklaerten EmptyScopeExpectation bestehen.
        assert_scope_containment(GoodSensor::from, Capability::ReadProcStat, root.path());
    }

    // ---- Prüfung 3b, Ausprägung EmptySourceIsNormal — K48 ----

    /// Verhält sich wie [`GoodSensor`], meldet eine fehlende Quelle aber als
    /// `Ok(SensorReading::default())` statt `Err(SourceUnavailable)` —
    /// GPU-artig: eine leere Quelle ist hier der Normalfall.
    #[derive(Debug)]
    struct EmptySourceIsNormalSensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for EmptySourceIsNormalSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for EmptySourceIsNormalSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
            let Some(root) = self.handle.scope().roots().next() else {
                return Ok(SensorReading::default());
            };
            let path = root.join("value");
            match harw_dod_readfs::parse_i64(self.handle.scope(), &path) {
                Ok(value) => Ok(SensorReading {
                    samples: vec![HostSample {
                        sensor: self.handle.id().clone(),
                        observed_at: now,
                        metric: Cow::Borrowed("optional_source_value"),
                        value: value as f64,
                    }],
                    events: vec![],
                }),
                Err(harw_dod_readfs::ReadFsError::Scope(SensorError::Io(io_err)))
                    if io_err.kind() == std::io::ErrorKind::NotFound =>
                {
                    Ok(SensorReading::default())
                }
                Err(harw_dod_readfs::ReadFsError::Scope(inner)) => Err(inner),
                Err(_) => Err(SensorError::MalformedSource),
            }
        }
    }

    #[test]
    fn test_empty_source_is_normal_sensor_passes_empty_scope_is_ok_check() {
        assert_empty_scope_is_ok(EmptySourceIsNormalSensor::from, Capability::ReadProcStat);
    }

    #[test]
    fn test_empty_source_is_normal_sensor_passes_scope_containment_check() {
        let root = tempfile::tempdir().expect("tempdir");
        write_case(root.path(), "typical", "45000\n", &good_reading(45000.0));
        // Bereichsdichtheit gilt unabhaengig von der erklaerten
        // EmptyScopeExpectation — auch fuer einen Sensor der Ausprägung
        // EmptySourceIsNormal.
        assert_scope_containment(EmptySourceIsNormalSensor::from, Capability::ReadProcStat, root.path());
    }

    #[test]
    fn test_good_sensor_fails_empty_scope_is_ok_check_because_it_reports_an_error() {
        // GoodSensor erklaert (implizit) "leere Quelle ist ein Fehler" und
        // meldet SourceUnavailable; gegen die Gegenausprägung geprüft, muss
        // das durchfallen.
        let result = std::panic::catch_unwind(|| {
            assert_empty_scope_is_ok(GoodSensor::from, Capability::ReadProcStat);
        });
        assert!(
            result.is_err(),
            "ein Sensor, der bei leerer Quelle einen Fehler meldet, darf die Prüfung 'leere Quelle ist normal' nicht bestehen"
        );
    }

    /// Erfindet bei leerem `ReadScope` einen Wert, statt
    /// `Ok(SensorReading::default())` zu melden.
    #[derive(Debug)]
    struct FabricatesValuesOnEmptyScopeSensor {
        handle: SensorHandle<Bound>,
    }

    impl From<SensorHandle<Bound>> for FabricatesValuesOnEmptyScopeSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }

    impl Sensor for FabricatesValuesOnEmptyScopeSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
            // Verstoss: erfindet einen Wert, obwohl der Bereich leer ist,
            // statt Ok(SensorReading::default()) zu melden.
            Ok(SensorReading {
                samples: vec![HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Borrowed("invented"),
                    value: 42.0,
                }],
                events: vec![],
            })
        }
    }

    #[test]
    fn test_fabricated_values_sensor_fails_empty_scope_is_ok_check() {
        let result = std::panic::catch_unwind(|| {
            assert_empty_scope_is_ok(FabricatesValuesOnEmptyScopeSensor::from, Capability::ReadProcStat);
        });
        assert!(
            result.is_err(),
            "ein Sensor, der bei leerem ReadScope erfundene Werte liefert, muss die Prüfung 'leere Quelle ist normal' zum Scheitern bringen"
        );
    }
}
