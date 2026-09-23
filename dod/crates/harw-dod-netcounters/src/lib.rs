//! Netzschnittstellen-Zähler aus `/proc/net/dev` — Knoten **AW2-11**.
//!
//! # Zweck
//! Diese Crate implementiert [`harw_dod_signals::Sensor`] für genau **eine**
//! Quelle (`/proc/net/dev`) und genau **eine** Fähigkeit
//! ([`harw_dod_cap::Capability::ReadProcNetDev`]). Sie kennt keine andere
//! Sensor-Crate (Contract-Master §F/§G, Vorgabe C7) und ruft nie `std::fs`
//! direkt auf — jeder Dateizugriff läuft über
//! [`harw_dod_readfs::read_line_fields`].
//!
//! # Was dieser Sensor ausdrücklich NICHT meldet
//! **Dieser Sensor zählt nur.** `/proc/net/dev` trägt für jede
//! Netzwerkschnittstelle ausschließlich kumulative Byte-, Paket- und
//! Fehlerzähler — keine Gegenstelle, keine IP-Adresse, keinen Port, keinen
//! Verbindungszustand und keinen Paketinhalt. Diese Crate liest keine andere
//! `/proc/net/*`-Datei (insbesondere nicht `/proc/net/tcp`, `/proc/net/udp`
//! oder `/proc/net/route`, die tatsächlich Adressen und Ports trügen) und
//! erfindet auch selbst keine: das einzige aus der Quelle übernommene
//! Textfeld ist der Schnittstellenname (z. B. `eth0`), der durch die private
//! Funktion `sanitize_label` (`sensor.rs`) auf `[a-z0-9_]` reduziert wird,
//! bevor er in einen Metriknamen einfließt — selbst ein ungewöhnlicher,
//! adressartiger Schnittstellenname (z. B. eine VLAN-Subschnittstelle
//! `1.2.3.4`, syntaktisch vom Kernel erlaubt) kann die Punkte darin nicht bis
//! in den emittierten
//! Metriknamen tragen. Ein Sensor, der Verbindungsziele meldete, wäre ein
//! Netzbeobachter mit einer ganz anderen, weiterreichenden Rechtematrix — und
//! diese Crate behauptet die kleinere. Siehe den Test
//! `test_serialized_reading_never_contains_address_or_port_pattern` in
//! `sensor.rs` für die Prüfung, die das serialisierte Ergebnis genau danach
//! durchsucht.
//!
//! # Quellenwahl — `/proc/net/dev`, nicht `/sys/class/net/*/statistics/*`
//! Zwei Wege standen zur Wahl:
//!
//! - **`/proc/net/dev`** (gewählt): eine einzige Datei, spaltenförmig, alle
//!   Schnittstellen in einem Lesevorgang.
//! - **`/sys/class/net/*/statistics/rx_bytes`** (verworfen): ein Attribut je
//!   Datei, ein Skalar je Datei — genau die Form, für die
//!   `#[derive(harw_macros::SensorSource)]` gebaut ist.
//!
//! Der zweite Weg passt zum Makro, scheitert aber an derselben Lücke, die
//! `harw-dod-thermal` bereits für Zonenlabels dokumentiert hat (siehe dessen
//! `lib.rs`-Moduldoku, Abschnitt „Warum kein `#[derive(SensorSource)]`"):
//! `#[source(glob = ..., parse = ..., metric = ...)]` erlaubt **ein** Feld
//! mit **einem** zur Kompilierzeit festen Metriknamen für **alle** Treffer
//! des Glob-Musters (`harw-macros/src/sensor_source.rs`,
//! `expand_sensor_source`: „SensorSource erlaubt genau ein Feld mit
//! `#[source(...)]`; ein Sensor mit zwei Quellen ist zwei Sensoren"). Ein
//! Muster `net/*/statistics/rx_bytes` träfe zwar mehrere Schnittstellen,
//! aber jeder Treffer bekäme **denselben** Metriknamen `rx_bytes` — die acht
//! Schnittstellen `eth0` und `eth1` wären dann als zwei `HostSample`s mit
//! identischem `metric` ununterscheidbar. Und selbst wenn das Makro ein
//! Label pro Treffer erlaubte: ein vollständiges Zählerbild braucht acht
//! verschiedene Zähler je Schnittstelle (siehe unten), also acht getrennte
//! `#[source(...)]`-Felder — das Makro erlaubt aber **genau eines** je
//! Struct. `/proc/net/dev` selbst ist außerdem gar keine glob-adressierbare
//! Menge von Dateien, sondern eine einzige Datei mit vielen Werten in
//! Spalten und Zeilen — derselbe Formunterschied, den bereits
//! `harw-dod-cpu` für `/proc/stat` dokumentiert (dessen `lib.rs`-Moduldoku,
//! Abschnitt „Warum kein `#[derive(SensorSource)]`"): das Makro liest
//! ausschließlich über `parse_i64`/`parse_u64`, die beide nur die *erste*
//! Zeile einer Datei als Ganzes parsen, nie eine von mehreren Spalten einer
//! Zeile adressieren. Diese Crate implementiert [`harw_dod_signals::Sensor`]
//! deshalb **von Hand** (siehe [`sensor`]) und nutzt
//! [`harw_dod_readfs::read_line_fields`] — eine Funktion, deren eigene
//! Moduldoku `/proc/net/dev` wörtlich als Beispiel für genau diesen Fall
//! nennt.
//!
//! # Das `/proc/net/dev`-Format
//! Zwei feste Kopfzeilen, danach je Schnittstelle eine Zeile mit `name:` und
//! sechzehn Zahlen (acht Empfangs-, acht Sendespalten):
//!
//! ```text
//! Inter-|   Receive                                                |  Transmit
//!  face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
//!     lo:  733258    5340    0    0    0     0          0         0   733258    5340    0    0    0     0       0          0
//!   eth0: 1234567    8901    2    3    0     0          0         5  7654321    4321    1    7    0     0       0          0
//! ```
//!
//! Der Doppelpunkt klebt **immer** unmittelbar am Namen (der Kernel formatiert
//! mit `%6s:`, ohne Leerzeichen dazwischen); bei einem langen
//! Schnittstellennamen **und** einem ersten Zählerwert, der die volle
//! Spaltenbreite ausfüllt, kann sogar die erste Zahl noch am selben
//! Whitespace-Token kleben (`eth0:1234` statt `eth0: 1234`) — siehe die
//! private Funktion `parse_one_interface_row` (`sensor.rs`) für die
//! Behandlung. Die Feldzahl je
//! Zeile wird **nicht** geprüft: ein künftiger Kernel kann weitere Spalten
//! anhängen, ohne dass dieser Sensor daran scheitert (siehe
//! [`harw_dod_readfs::read_line_fields`]-Moduldoku, die dieselbe Robustheit
//! für `/proc/stat` und `/proc/diskstats` fordert). Die beiden Kopfzeilen
//! selbst sind seit Linux 2.2 fest verdrahteter Kernel-Text
//! (`net/core/net-procfs.c`) — diese Crate überspringt sie **positionsbasiert**
//! (die ersten zwei Zeilen, private Konstante `HEADER_LINE_COUNT` in
//! `sensor.rs`), nicht über
//! Inhaltserkennung: fehlt eine der beiden Kopfzeilen, verschiebt sich die
//! Zeilenzuordnung, und die dadurch fälschlich als Datenzeile gelesene
//! Kopfzeile (oder das Fehlen jeder verbleibenden Datenzeile) löst
//! `SensorError::MalformedSource` aus, statt still eine Schnittstelle zu
//! verlieren (siehe `fixtures/malformed/missing-header`).
//!
//! # Gemeldete Zähler — acht von sechzehn Spalten je Schnittstelle
//! Diese Crate meldet nicht alle sechzehn Spalten, sondern eine feste
//! Auswahl der acht operativ aussagekräftigsten: `rx_bytes`, `rx_packets`,
//! `rx_errs`, `rx_drop`, `tx_bytes`, `tx_packets`, `tx_errs`, `tx_drop`
//! (private Konstante `FIELD_METRICS` in `sensor.rs`). Die verbleibenden acht Spalten
//! (`fifo`, `frame`, `compressed`, `multicast` auf der Empfangsseite;
//! `fifo`, `colls`, `carrier`, `compressed` auf der Sendeseite) sind in der
//! Praxis selten ausgewertet und auf den meisten Schnittstellentypen
//! durchgehend `0` — sie mitzuführen würde die Kardinalität dieses Sensors
//! verdoppeln, ohne einen entsprechenden Erkenntnisgewinn (siehe Abschnitt
//! „Kardinalität" unten). Jeder gemeldete Zähler trägt das sanitisierte
//! Schnittstellenlabel als Suffix (`rx_bytes_eth0`, analog zu
//! `harw-dod-thermal`s Zonenlabel-Suffix) — ohne dieses Suffix wären zwei
//! Schnittstellen unter demselben Metriknamen ununterscheidbar.
//!
//! # Kardinalität — eine feste Obergrenze je Poll
//! Die Zahl der Schnittstellen ist auf einem gewöhnlichen Host klein (`lo`
//! plus eine Handvoll physischer/virtueller Adapter), auf einem
//! Container-Host mit vielen `veth`-Paaren aber **unbegrenzt** — genau der
//! Fall, gegen den die `Cardinality`-Deklaration gebaut wurde (siehe
//! `harw-dod-fixtures`-Moduldoku, Prüfung „Kardinalität"). Diese Crate wählt
//! deshalb eine **feste Obergrenze der Schnittstellenzahl** (private
//! Konstante `MAX_INTERFACES` in `sensor.rs`) statt eines Aggregats über alle
//! Schnittstellen: ein Aggregat verlöre genau das Signal, für das dieser
//! Sensor gebaut ist — eine einzelne kompromittierte `veth`-Schnittstelle
//! mit ungewöhnlichem Datenverkehr wäre in einer Summe über hundert
//! Schnittstellen unsichtbar. Statt einer Auswahl nach Schnittstellenart
//! (die eine zusätzliche, fragile Namenskonvention voraussetzen würde —
//! Linux erzwingt kein Präfixschema für `veth`, `docker0`, Bridges oder
//! VLANs) sortiert diese Crate alle gefundenen Schnittstellen **nach Namen**
//! und behält nur die ersten `MAX_INTERFACES` — deterministisch
//! und unabhängig von der Zeilenreihenfolge in `/proc/net/dev` (private
//! Funktion `cap_and_sort_interfaces` in `sensor.rs`). Überzählige
//! Schnittstellen werden
//! **nicht** gemeldet und lösen **keinen** Fehler aus: das ist eine
//! bewusste Schutzmaßnahme gegen eine Zeitreihendatenbank-Explosion, keine
//! fehlerhafte Quelle. Mit acht Zählern je Schnittstelle ergibt sich die
//! öffentlich exportierte [`sensor::MAX_CARDINALITY`] `= FIELD_METRICS.len()
//! * MAX_INTERFACES` — der Fixture-seitige Spiegel, den
//! `harw_dod_fixtures::sensor_suite!` als `max_cardinality` erwartet.
//!
//! # Kumulative Zähler, keine Rate
//! Alle sechzehn Spalten einer `/proc/net/dev`-Zeile wachsen **monoton**
//! seit dem Systemstart der jeweiligen Schnittstelle; sie sind keine
//! Durchsatzangabe. Eine Rate (Bytes pro Sekunde) zu melden hieße, zwei
//! Abrufe zu vergleichen — also Zustand über einen Poll hinaus zu halten.
//! Das verbietet sich aus demselben Grund, den bereits `harw-dod-cpu`
//! dokumentiert (dessen `lib.rs`-Moduldoku, „Entscheidung 1"):
//! [`harw_dod_signals::Sensor::poll`] nimmt `&self`, nicht `&mut self`, und
//! die Fixture-Harness (`harw_dod_fixtures::sensor_suite!`) prüft jeden
//! Sensor gegen einen **einzelnen**, eingefrorenen `tree/`-Zustand — ein
//! Sensor mit Zustand über den Abruf hinaus ist gegen ein solches Fixture
//! nicht prüfbar. **Diese Crate meldet deshalb die rohen kumulativen
//! Zähler, unverändert.** Die Bildung einer Rate ist bewusst Sache des
//! Verbrauchers (z. B. `harw-dod-rules`), der ohnehin schon Zustand über
//! mehrere Beobachtungen hinweg hält. Wer das hier „verbessert", indem er
//! zwei interne Polls vergleicht, bricht sowohl die `&self`-Objektsicherheit
//! als auch die Fixture-Prüfbarkeit.
//!
//! # Nebenläufigkeit
//! [`sensor::NetCountersSensor`] hält ausschließlich einen unveränderlichen
//! `harw_dod_cap::SensorHandle`: `Send + Sync` ohne inneres Locking.
//! [`sensor::NetCountersSensor::poll`] öffnet und schließt seine eigene
//! Datei je Aufruf; parallele Aufrufe auf derselben Instanz stören sich
//! nicht.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — inhaltsfrei, kein Feldwert, kein Pfad,
//! keine gelesene Zeile erscheint in einer Fehlermeldung. Diese Crate
//! definiert keinen eigenen Fehlertyp. Siehe
//! [`sensor::NetCountersSensor::poll`] für die vollständige Zuordnung.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_signals::Sensor;
//! use harw_dod_netcounters::NetCountersSensor;
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
//! let handle = SensorHandle::new(SensorId::from_str("netcounters-0"), Capability::ReadProcNetDev)
//!     .bind(scope);
//! let sensor = NetCountersSensor::from(handle);
//! let reading = sensor.poll(jiff::Timestamp::now())?;
//! for sample in &reading.samples {
//!     println!("{}: {}", sample.metric, sample.value);
//! }
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

pub mod sensor;

pub use sensor::NetCountersSensor;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
