//! Offene Listener-Sockets mit cgroup-Bezug (Knoten AW2-14).
//!
//! # Zweck
//! Ein Prozess, der plötzlich lauscht, ist eines der wenigen Signale, die
//! eine Hintertür zuverlässig erzeugt — sie muss erreichbar sein, um nützlich
//! zu sein. Dieser Sensor meldet zwei Dinge je offenem Listener-Socket:
//! welcher Port lauscht, und — soweit auflösbar — zu welcher cgroup der
//! dahinterliegende Prozess gehört. Die cgroup beantwortet "wessen Prozess
//! ist das", ohne dass ein Konsument die vollständige Prozessliste
//! durchgehen muss.
//!
//! # Verantwortungsbereich
//! [`ListenerSensor`] (Modul [`sensor`]) implementiert
//! [`harw_dod_signals::Sensor`] und orchestriert zwei eigenständige Schritte:
//!
//! - [`procnet`]: liest `/proc/net/{tcp,tcp6,udp,udp6}` und liefert Port und
//!   Socket-Inode jeder "lauschend"-Zeile — Zustand `0A` (`TCP_LISTEN`) für
//!   die beiden TCP-Tabellen, Zustand `07` (UDP-Äquivalent, siehe
//!   [`procnet`]-Moduldokumentation, Abschnitt „F-065-Nachtrag") für die
//!   beiden UDP-Tabellen.
//! - [`owner`]: löst — für genau diese Inodes, in einem einzigen Durchlauf
//!   über `/proc/<pid>/fd/*` — die besitzende PID sowie deren reale UID und
//!   cgroup auf.
//!
//! # Was dieser Sensor ausdrücklich **nicht** meldet
//! Dieser Sensor liest die **Listener-Tabelle**, nie die
//! **Verbindungstabelle**. Er meldet, *dass* ein Port lauscht — nie, wohin
//! eine Verbindung geht, mit wem gesprochen wird, oder was übertragen wird.
//! Kein Feld eines von diesem Sensor erzeugten
//! [`harw_dod_signals::SecurityEvent`] trägt eine entfernte Adresse, einen
//! entfernten Port oder Verbindungsinhalt — nicht aus Sparsamkeit, sondern
//! als Berechtigungsgrenze: ein Sensor, der Verbindungsziele läse, wäre ein
//! Netzbeobachter mit einer grundlegend anderen Rechtematrix als ein
//! unprivilegierter Leser der Listener-Tabelle. [`procnet::parse_listener_line`]
//! liest die `rem_address`-Spalte jeder Quellzeile technisch bedingt ein
//! (sie liegt vor der ausgewerteten `st`-Spalte), reicht ihren Wert aber an
//! keiner Stelle dieser Crate weiter — siehe `test_poll_emitted_result_contains_no_destination_address`
//! in [`sensor`] für den Nachweis am materialisierten Ergebnis.
//!
//! # Kostenverhalten der cgroup-Auflösung
//! Die Umkehrung "Socket-Inode → besitzender Prozess" existiert im Kernel
//! nicht direkt; im ungünstigen Fall bedeutet ihre Auflösung, alle PIDs auf
//! dem Host mal alle ihre Dateideskriptoren zu prüfen — ein teurer Durchlauf.
//! [`owner::resolve_owners`] sammelt deshalb **einmal** die Menge aller in
//! einem Abruf gesuchten Inodes und durchläuft `/proc` **einmal** dafür,
//! statt je gefundenem Socket erneut zu suchen, und bricht ab, sobald jede
//! gesuchte Inode einem Prozess zugeordnet ist (siehe dortige
//! Moduldokumentation für Details). Ein Sensor, der bei jedem Abruf eine
//! Sekunde braucht, wird von seinem Betreiber abgeschaltet — und meldet
//! danach gar nichts mehr; dieses Kostenmodell ist deshalb Teil der
//! Spezifikation dieses Knotens, nicht nur eine Optimierung.
//!
//! Ein `/proc/<pid>/fd`, das nicht gelistet werden kann (fremder Benutzer,
//! oder der Prozess ist inzwischen beendet), wird übersprungen — das ist die
//! Voreinstellung eines unprivilegierten Sensors, kein Ausfall dieses
//! Abrufs. Ein Listener, dessen Besitzer sich so nicht auflösen lässt, wird
//! trotzdem gemeldet, ohne `Actor`. Ein Listener mit auflösbarem Besitzer,
//! aber ohne auflösbare cgroup, wird ebenfalls gemeldet, mit `cgroup: None`
//! (siehe [`owner`]-Moduldokumentation für die genaue Unterscheidung).
//!
//! # Kardinalität
//! Siehe [`sensor`]-Moduldokumentation: ein Ereignis je offenem Listener,
//! der Port im Ereignisrumpf statt als Metrik-Label — diese Crate registriert
//! bewusst keine `harw-observe`-Metrik über den Port.
//!
//! # Genau eine Fähigkeit, keine Sensor-Abhängigkeit
//! Diese Crate benutzt ausschließlich [`harw_dod_cap::Capability::ReadProcNet`]
//! und hängt an keiner anderen Sensor-Crate — insbesondere nicht an
//! `harw-dod-cgroup`, obwohl beide thematisch cgroups berühren. Ein Sensor
//! kennt laut Vertrag keine andere Sensor-Crate (siehe
//! `harw_dod_signals::sensor`-Moduldokumentation); die cgroup-Zuordnung
//! dieser Crate entsteht deshalb ausschließlich aus dem, was innerhalb ihres
//! eigenen `/proc`-Lesebereichs steht (`/proc/<pid>/cgroup`), nie aus einem
//! Aufruf in eine andere Sensor-Crate hinein.
//!
//! # Nebenläufigkeit
//! [`ListenerSensor`] ist `Send + Sync`; [`harw_dod_signals::Sensor::poll`]
//! nimmt `&self` und hält keinen Zustand über einen Abruf hinaus.
//! Konkurrierende Aufrufe auf demselben Sensor sind sicher.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — inhaltsfrei, wie vom `Sensor`-Vertrag
//! verlangt. Ausschließlich das Lesen der vier Listener-Tabellen selbst kann
//! einen Abruf scheitern lassen; jede Besitzer-, UID- oder
//! cgroup-Auflösung ist bestmöglich und scheitert nie den gesamten Abruf
//! (siehe [`owner`]-Moduldokumentation).
//!
//! # Examples
//! ```rust,no_run
//! use std::path::PathBuf;
//! use harw_dod_listener::ListenerSensor;
//! use harw_dod_signals::{EventKind, Sensor};
//! use harw_types::SensorId;
//!
//! let sensor = ListenerSensor::new(SensorId::from_str("listener-0"), PathBuf::from("/proc"));
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH)?;
//! for event in &reading.events {
//!     if let EventKind::ListenerOpened { port } = &event.kind {
//!         println!("Port {port} lauscht");
//!     }
//! }
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

pub mod owner;
pub mod procnet;
pub mod sensor;

pub use sensor::ListenerSensor;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
