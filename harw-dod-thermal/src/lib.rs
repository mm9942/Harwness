//! Thermalzonen aus sysfs: die eine Quelle `/sys/class/thermal`, die eine
//! Fähigkeit `Capability::ReadSysfsThermal` (Knoten AW2-07).
//!
//! # Zweck
//! [`ThermalSensor`] liest jede sichtbare Thermalzone unterhalb der
//! Bereichswurzel und meldet ihre Temperatur als
//! `harw_dod_signals::HostSample`. Genau eine Quelle, genau eine Fähigkeit —
//! siehe `harw_dod_signals::sensor`-Moduldoku für das, was ein Sensor NICHT
//! darf. Diese Crate kennt keine andere Sensor-Crate (Contract-Master §G,
//! Regel C7) und ruft nirgends `std::fs` direkt auf — jeder Zugriff läuft
//! über `harw-dod-readfs` (siehe `sensor`-Moduldoku für die Details).
//!
//! # sysfs-Format
//! Jede Zone ist ein eigenes Verzeichnis unterhalb der Bereichswurzel (real:
//! `/sys/class/thermal`), typischerweise `thermal_zone0`, `thermal_zone1`,
//! ... Zwei Dateien je Zone sind für diese Crate relevant:
//!
//! - `temp` — eine Ganzzahl in **Millidegree Celsius**, gefolgt von einem
//!   Zeilenumbruch. Beispiel: `45000\n` bedeutet 45,0 °C. Ohne diese Datei
//!   gilt die Zone als fehlerhaft geformt (siehe [`ThermalSensor::poll`]).
//! - `type` — der kernelvergebene Name der Zone, eine Zeile Text. Beispiel:
//!   `x86_pkg_temp\n`. Optional aus Sicht dieser Crate: fehlt sie, wird die
//!   Zone trotzdem gemeldet, mit einem Ersatzlabel (siehe unten).
//!
//! # Einheitenumrechnung — Grad statt Millidegree, mit Begründung
//! [`ThermalSensor::poll`] rechnet den sysfs-Rohwert (Millidegree Celsius) in
//! **Grad Celsius** um (Division durch 1000) und meldet ihn unter der Metrik
//! `temperature_celsius_<zone>`. Die Alternative — den Rohwert unverändert
//! unter einem ehrlichen Namen wie `temperature_millicelsius_<zone>` zu
//! führen — wäre in sich konsistent, widerspräche aber der bereits
//! etablierten Konvention dieses Workspace: `harw_dod_signals::HostSample`s
//! eigene Dokumentation und Tests (`harw-dod-signals/src/sample.rs`)
//! verwenden den Metriknamen `temperature_celsius` durchgehend mit Werten im
//! Grad-Bereich (z. B. `42.5`), nicht im Millidegree-Bereich (`42500.0`).
//! Einen Sensor zu bauen, der unter einem Namen, der anderswo im selben
//! Contract bereits „Grad Celsius" bedeutet, einen um den Faktor 1000
//! verschobenen Wert liefert, wäre genau der in der Aufgabenstellung
//! ausdrücklich genannte Fehler: ein Schwellwert, der anderswo in „Grad"
//! gedacht ist, würde auf diesen Sensor angewendet, ohne dass der
//! Unterschied auffiele — bis zum ersten falschen Alarm oder der ersten
//! ausbleibenden Warnung.
//!
//! # Zonenlabel — warum ein Label vertretbar ist, und wie es kodiert wird
//! `harw_dod_signals::HostSample` hat **kein** eigenes Label-/Tag-Feld (nur
//! `sensor`, `observed_at`, `metric`, `value`) — anders, als man für eine
//! Prometheus-artige Zeitreihe erwarten könnte. Mehrere Zonen ohne
//! Unterscheidung zu melden wäre aber nutzlos: „45 °C" ist, ohne zu wissen,
//! ob das der CPU-Kern oder das WLAN-Modul ist, keine verwertbare
//! Beobachtung. Der einzige verbleibende Ort für eine Unterscheidung ist
//! deshalb der `metric`-String selbst: [`ThermalSensor::poll`] hängt an den
//! festen Präfix `temperature_celsius_` ein sanitisiertes Zonenlabel an
//! (erster Zeileninhalt von `type`, oder — falls diese Datei fehlt, leer ist
//! oder aus einem anderen Grund nicht lesbar ist — der Zonen-
//! Verzeichnisname selbst, z. B. `thermal_zone0`). Das Label wird auf
//! `[a-z0-9_]` reduziert und auf 64 Zeichen gekürzt (Schutz gegen einen
//! ungewöhnlichen `type`-Inhalt, siehe `sensor`-Moduldoku).
//!
//! Das ist nur vertretbar, **weil** die Zonenzahl klein und über die
//! Laufzeit eines Hosts stabil ist (typischerweise ein niedriger
//! einstelliger bis niedriger zweistelliger Wert — siehe
//! `max_cardinality: 16` beim `sensor_suite!`-Aufruf in `sensor.rs`) —
//! anders als etwa Prozess- oder Verbindungszähler, die pro Poll unbegrenzt
//! wachsen könnten. Ein Sensor, der pro Zone einen eigenen Metriknamen
//! führt, ohne dass die Zonenzahl begrenzt ist, würde die
//! Zeitreihendatenbank sprengen (siehe `harw-dod-fixtures`-Moduldoku,
//! Prüfung „Kardinalität").
//!
//! Der Präfix wird **immer** angehängt, auch bei genau einer sichtbaren
//! Zone: ein Host mit heute einer Zone kann morgen eine zweite bekommen
//! (Docking, Firmware-Update), und eine Sonderregel „nur bei genau einer
//! Zone kein Suffix" würde den Metriknamen der ersten Zone genau dann
//! ändern, wenn eine zweite hinzukommt — eine stillschweigende Änderung
//! eines bereits beobachteten Zeitreihennamens.
//!
//! # Warum kein `#[derive(harw_macros::SensorSource)]`
//! `#[derive(harw_macros::SensorSource)]` (siehe dessen Moduldoku,
//! `harw-macros/src/sensor_source.rs`) erzeugt für jeden Treffer eines
//! einzigen `#[source(glob = ..., parse = ..., metric = ...)]`-Feldes ein
//! `HostSample` mit **demselben, zur Kompilierzeit festen** `metric`-Namen
//! und dem **unveränderten** geparsten Rohwert (`value: wert as f64`, ohne
//! Umrechnung). Zwei der drei zentralen Anforderungen dieses Knotens fehlen
//! also im Makro:
//!
//! 1. **Keine Umrechnung.** Das Makro hat keinen Attributschlüssel, der
//!    einen Skalierungsfaktor oder eine Transformation des geparsten Werts
//!    ausdrückt — der rohe Millidegree-Wert ginge unverändert in `value`.
//! 2. **Kein Label pro Treffer.** `metric` ist ein einzelnes `LitStr`, für
//!    alle Treffer eines Glob-Musters identisch — es gibt keinen Weg, für
//!    jeden Treffer (jede Zone) einen zweiten, zusammengehörigen Lesevorgang
//!    (`type`) auszuführen und dessen Ergebnis einfließen zu lassen.
//!
//! Eine dritte, unabhängige Beobachtung machte einen makro-erzeugten Sensor
//! ohnehin gegen `harw_dod_fixtures::sensor_suite!` unbrauchbar, selbst ohne
//! die beiden obigen Lücken: das Makro verdrahtet das `#[source(glob =
//! "...")]`-Muster als **zur Kompilierzeit festen Text**, unverändert an
//! `harw_dod_readfs::glob::glob` weitergereicht. `glob::glob`s eigene
//! Moduldoku ist hier ausdrücklich: die Suche beginnt **immer bei `/`**,
//! unabhängig vom `ReadScope`; das Muster ist der volle Pfad ohne führenden
//! `/`, nicht ein Suffix relativ zur Bereichswurzel — die Bereichswurzel
//! wirkt ausschließlich als nachträglicher Filter auf bereits gefundene
//! Treffer. Ein fest verdrahtetes Muster wie
//! `"sys/class/thermal/thermal_zone*/temp"` findet deshalb in Produktion
//! (Bereichswurzel = `/sys/class/thermal`, zufällig identisch mit dem
//! hartkodierten Pfad) etwas — aber in der Fixture-Prüfung (Bereichswurzel =
//! ein beliebiges `fixtures/<fall>/tree` irgendwo im Checkout) **nichts**,
//! weil die eigentliche Verzeichnissuche nie im Fixture-Baum stattfindet,
//! sondern immer im echten `/`. Das Ergebnis wäre kein Fehler, sondern ein
//! still leeres `Ok(SensorReading { samples: vec![], events: vec![] })` —
//! die Determinismus-Prüfung schlüge dann nicht am Vergleich der beiden
//! Polls fehl (beide leer, „konsistent" aus Versehen), sondern erst am
//! Vergleich mit `expect.json`. [`ThermalSensor::poll`] umgeht das, indem es
//! sein Glob-Muster **zur Laufzeit** aus der ersten tatsächlichen
//! Bereichswurzel (`ReadScope::roots`) plus einem festen Suffix
//! (`thermal_zone*`) baut, statt den vollen Pfad fest zu verdrahten — siehe
//! die private Funktion `relative_pattern` in `sensor.rs`. Das macht
//! denselben Code für Produktion (Wurzel `/sys/class/thermal`) und
//! Fixture-Test (Wurzel `fixtures/<fall>/tree`) korrekt, ohne
//! Fallunterscheidung.
//!
//! Diese dritte Beobachtung betrifft **jeden** über dieses Makro gebauten,
//! glob-basierten Sensor, nicht nur diesen.
//!
//! # Nebenläufigkeit
//! [`ThermalSensor`] hält ausschließlich einen unveränderlichen
//! `harw_dod_cap::SensorHandle`: `Send + Sync` ohne inneres Locking.
//! [`ThermalSensor::poll`] öffnet und schließt seine eigenen Dateien je
//! Aufruf; parallele Aufrufe auf derselben Instanz (der Sentinel hält
//! Sensoren hinter `Arc`, siehe `harw_dod_signals::sensor`-Moduldoku) stören
//! sich nicht.
//!
//! # Fehler
//! `harw_dod_cap::SensorError` — inhaltsfrei, kein Feldwert, kein Pfad,
//! keine gelesene Zeile erscheint in einer Fehlermeldung. Siehe
//! `harw_dod_signals::Sensor::poll` für die vollständige Fehlerbedeutung und
//! die Doku von [`ThermalSensor::poll`] für die drei hier tatsächlich
//! auftretenden Varianten.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::scope::AliasRoot;
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_signals::Sensor;
//! use harw_dod_thermal::ThermalSensor;
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! // sysfs-Klasseneinträge sind Symlinks nach `/sys/devices/...` (F-005) —
//! // `ReadScope::from_roots` allein prüft gegen die nicht kanonisierte
//! // Klassenwurzel und würde jeden Treffer verwerfen; `AliasRoot::sysfs_class`
//! // baut den Bereich, der das Symlink-Ziel korrekt zulässt.
//! let alias = AliasRoot::sysfs_class(PathBuf::from("/sys/class/thermal"))
//!     .expect("gültige sysfs-Klassenwurzel");
//! let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);
//! let handle = SensorHandle::new(SensorId::from_str("thermal-0"), Capability::ReadSysfsThermal)
//!     .bind(scope);
//! let sensor = ThermalSensor::from(handle);
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH)?;
//! for sample in &reading.samples {
//!     println!("{}: {} °C", sample.metric, sample.value);
//! }
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

mod sensor;

pub use sensor::ThermalSensor;
