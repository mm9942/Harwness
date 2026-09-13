//! CPU-Auslastung aus `/proc/stat` — Knoten **AW2-08**.
//!
//! # Zweck
//! Diese Crate implementiert [`harw_dod_signals::Sensor`] für genau **eine**
//! Quelle (`/proc/stat`) und genau **eine** Fähigkeit
//! ([`harw_dod_cap::Capability::ReadProcStat`]). Sie kennt keine andere
//! Sensor-Crate (Contract-Master §F/§G, Vorgabe C7) und ruft nie `std::fs`
//! direkt auf — jeder Dateizugriff läuft über
//! [`harw_dod_readfs::read_line_fields`].
//!
//! # Das `/proc/stat`-Format
//! `/proc/stat` ist **eine** Datei mit **vielen** Werten in Spalten, nicht
//! ein einzelner Skalar wie eine sysfs-Attributdatei. Ihre erste Zeilengruppe
//! sieht so aus (gekürzt, ohne die nachfolgenden `intr`/`ctxt`/`btime`/…
//! -Zeilen, die dieser Sensor nicht liest):
//!
//! ```text
//! cpu  2255 34 2290 22625563 6290 127 456 0 0 0
//! cpu0 1132 34 1441 11311718 3675 127 438 0 0 0
//! ```
//!
//! Die Spalten der `cpu*`-Zeilen sind, in dieser Reihenfolge, `user nice
//! system idle iowait irq softirq steal guest guest_nice` — kumulative
//! Zeitzähler in **Jiffies** (Kernel-Ticks) seit dem Systemstart. Die ersten
//! vier Felder (`user nice system idle`) trägt `/proc/stat` bereits seit den
//! ältesten Kernel-2.4-Versionen; `iowait`, `irq`/`softirq` und
//! `steal`/`guest`/`guest_nice` kamen in späteren Kernel-Versionen hinzu.
//! Neuere Kernel können am Zeilenende **weitere** Spalten anhängen, deren
//! Bedeutung dieser Sensor nicht kennt — siehe die zweite Entscheidung unten
//! zur Robustheit gegen genau diesen Fall.
//!
//! # Entscheidung 1 — kumulative Zähler, keine Rate
//! Die zehn Werte einer `cpu*`-Zeile wachsen **monoton** seit dem
//! Systemstart; sie sind keine Prozentangabe. Eine Auslastung in Prozent zu
//! melden würde zwei Abrufe vergleichen — also Zustand über einen Poll
//! hinaus halten. Das verbietet sich hier aus zwei Gründen:
//!
//! 1. [`harw_dod_signals::Sensor::poll`] nimmt `&self`, nicht `&mut self`.
//!    Ein Sensor, der trotzdem Zustand hielte, bräuchte innere
//!    Veränderlichkeit (`Mutex`/`Cell`) und würde damit die in der
//!    `Sensor`-Moduldoku festgehaltene Erwartung verletzen, dass
//!    gleichzeitige `poll`-Aufrufe sicher und deterministisch sind.
//! 2. Die Fixture-Harness ([`harw_dod_fixtures::sensor_suite!`]) prüft jeden
//!    Sensor gegen einen **einzelnen**, eingefrorenen `tree/`-Zustand. Ein
//!    Sensor mit Zustand über den Abruf hinaus ist gegen ein solches
//!    Fixture nicht prüfbar — die wichtigste der sechs Harness-Prüfungen,
//!    Determinismus, setzt genau das voraus.
//!
//! **Diese Crate meldet deshalb die rohen kumulativen Zähler, unverändert,
//! als zehn `HostSample`s je Poll.** Die Bildung einer Rate (Differenz durch
//! Zeitspanne zwischen zwei Polls) ist bewusst **Sache des Verbrauchers**
//! (z. B. `harw-dod-rules`), der ohnehin schon Zustand über mehrere
//! Beobachtungen hinweg hält. Wer das hier „verbessert", indem er zwei
//! interne Polls vergleicht, bricht sowohl die Objektsicherheit als
//! `&self`-Trait als auch die Fixture-Prüfbarkeit — deshalb steht das hier
//! ausdrücklich, nicht nur als Kommentar im Code.
//!
//! # Entscheidung 2 — Aggregat, nicht je Kern
//! `/proc/stat` trägt eine `cpu`-Aggregatzeile (Summe über alle Kerne) sowie
//! je eine `cpu0`, `cpu1`, … -Zeile pro Kern. Ein Label je Kern ist auf einem
//! großen Host **unbegrenzt** — auf einer Maschine mit einigen hundert
//! Kernen wären das ebenso viele verschiedene `HostSample::metric`-
//! Kombinationen, genau die Kardinalitätsexplosion, gegen die die
//! `Cardinality`-Deklaration (`harw-observe`, Contract-Master §A.2) gebaut
//! wurde. **Dieser Sensor liest deshalb ausschließlich die `cpu`-
//! Aggregatzeile** (identifiziert am exakten ersten Feld `"cpu"`, nicht
//! `"cpu0"` o. Ä.) und ignoriert jede `cpuN`-Zeile vollständig. Die
//! Kardinalität bleibt damit **fest bei höchstens zehn** — ein `HostSample`
//! je bekanntem Spaltennamen, unabhängig von der Kernzahl des Hosts (siehe
//! [`sensor::MAX_CARDINALITY`]).
//!
//! # Warum kein `#[derive(harw_macros::SensorSource)]`
//! `harw-macros` (Knoten AW2-06) deckt laut seiner eigenen Moduldoku genau
//! einen Fall ab: eine glob-adressierte Menge von Dateien, aus der **je
//! Treffer genau ein Skalar** über `harw_dod_readfs::parse_i64`/`parse_u64`
//! gelesen wird — der sysfs-Fall (eine Datei, eine Zahl). `/proc/stat` ist
//! **eine** Datei mit **vielen** Werten in **Spalten** einer einzigen Zeile;
//! das Makro hat kein Attribut, das eine Spaltenposition innerhalb einer
//! Zeile benennen könnte, und `parse_i64`/`parse_u64` lesen ohnehin nur die
//! *erste* Zeile einer Datei als Ganzes. Diese Crate implementiert
//! [`harw_dod_signals::Sensor`] deshalb **von Hand** (siehe [`sensor`]) und
//! nutzt stattdessen [`harw_dod_readfs::read_line_fields`] — eine Funktion,
//! die genau für diesen Fall entstanden ist (siehe deren Moduldoku, die
//! wörtlich `/proc/stat` als Beispiel nennt). Was dem Makro fehlt, damit es
//! diesen Fall künftig tragen könnte: eine Attributform, die Zeile **und**
//! Spalte benennt, z. B. sinngemäß
//! `#[source(glob = "proc/stat", line_prefix = "cpu", columns = ["user_jiffies", "nice_jiffies", ...], parse = "u64")]`
//! statt der heutigen `glob = "..."/parse = "..."/metric = "..."`-Form, die
//! nur einen Skalar je Treffer kennt.
//!
//! # Nebenläufigkeit
//! [`sensor::CpuSensor`] hält keinen veränderlichen Zustand: `Send + Sync`
//! automatisch, `poll` nimmt `&self` und ist aus mehreren Threads
//! gleichzeitig sicher aufrufbar (siehe Entscheidung 1 oben).
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — inhaltsfrei, wie vom `Sensor`-Trait
//! vorgeschrieben. Diese Crate definiert keinen eigenen Fehlertyp: jeder
//! Fehlerpfad in [`sensor::CpuSensor::poll`] mündet in eine der vier
//! bestehenden `SensorError`-Varianten.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_cpu::CpuSensor;
//! use harw_dod_signals::Sensor;
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
//! let handle = SensorHandle::new(SensorId::from_str("cpu-0"), Capability::ReadProcStat)
//!     .bind(scope);
//! let sensor = CpuSensor::from(handle);
//!
//! let reading = sensor.poll(jiff::Timestamp::now())?;
//! for sample in &reading.samples {
//!     println!("{}: {}", sample.metric, sample.value);
//! }
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

pub mod sensor;

pub use sensor::CpuSensor;
