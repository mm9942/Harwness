//! Speicherdruck aus `/proc/meminfo` — Knoten **AW2-09**.
//!
//! # Zweck
//! Diese Crate implementiert [`harw_dod_signals::Sensor`] für genau **eine**
//! Quelle (`/proc/meminfo`) und genau **eine** Fähigkeit
//! ([`harw_dod_cap::Capability::ReadProcMeminfo`]). Sie kennt keine andere
//! Sensor-Crate (Contract-Master §F/§G, Vorgabe C7) und ruft nie `std::fs`
//! direkt auf — jeder Dateizugriff läuft über
//! [`harw_dod_readfs::read_key_values`].
//!
//! # Das `/proc/meminfo`-Format
//! `/proc/meminfo` ist eine Datei mit über fünfzig `Schlüssel: Wert`-Zeilen,
//! eine je Zeile, in fester, aber pro Kernel-Version leicht variierender
//! Reihenfolge. Ein typischer Ausschnitt (gekürzt):
//!
//! ```text
//! MemTotal:       16316360 kB
//! MemFree:         2549408 kB
//! MemAvailable:    9321236 kB
//! Buffers:          412080 kB
//! Cached:          3877300 kB
//! SwapTotal:       2097148 kB
//! SwapFree:        1048574 kB
//! HugePages_Total:       0
//! ```
//!
//! Jede Zeile trägt einen Schlüssel (linksbündiger Name, gefolgt von `:`)
//! und einen Wert. **Die Einheit steht im Wert, nicht im Schlüssel:** fast
//! jede Zeile endet auf ` kB`, einige — etwa `HugePages_Total` — sind eine
//! nackte Zahl ohne Einheit. Ein Parser, der blind das letzte Feld
//! abschneidet, verfälscht die zweite Sorte; einer, der blind ` kB`
//! erwartet, scheitert an ihr (siehe [`sensor`] für die private
//! `parse_meminfo_value`-Funktion, die die Entscheidung trifft, wie diese
//! Crate beide Formen behandelt).
//!
//! # Einheitenbehandlung — und die `kB`-Ungenauigkeit des Kernels
//! Die private `parse_meminfo_value`-Funktion in [`sensor`] erkennt zwei
//! Formen: `"<zahl> kB"` und `"<zahl>"` ohne Einheit.
//!
//! - **Mit `kB`:** Der Kernel meldet hier historisch **Kibibyte (2^10 = 1024
//!   Bytes)**, nicht den SI-Kilobyte (1000 Bytes) — eine Ungenauigkeit der
//!   Bezeichnung, die seit Jahrzehnten unverändert in `/proc/meminfo` steht.
//!   Diese Crate meldet ihre ausgewählten Felder in **Bytes** und rechnet
//!   deshalb mit dem Faktor **1024**, nicht 1000. Wer das später „korrigiert“
//!   auf den Faktor 1000, weil `kB` im SI-System 1000 bedeutet, liegt um
//!   **2,4 %** daneben — das ist keine Rundung, sondern ein Fehler, der still
//!   in jeden nachgelagerten Schwellwert einfließt.
//! - **Ohne Einheit:** Der nackte Zahlenwert wird unverändert übernommen,
//!   **ohne** Multiplikation. Diese Crate erfindet keinen Umrechnungsfaktor
//!   für eine Einheit, die die Quelle nicht nennt — eine geratene Umrechnung
//!   wäre falscher als eine dokumentierte Weitergabe des Rohwerts. In der
//!   Praxis betrifft das keines der fünf unten ausgewählten Felder (die
//!   tragen in jeder bekannten Kernel-Version durchgehend `kB`); die Form
//!   existiert, damit ein Wert ohne Einheit nicht scheitert, sondern
//!   sinnvoll behandelt wird, falls sich das je ändert.
//!
//! Jede andere Form (mehr als zwei durch Leerraum getrennte Felder, ein
//! unbekanntes Einheitssuffix außer `kB`, oder ein nicht-numerischer
//! Zahlenteil) gilt als fehlerhaft geformt
//! ([`harw_dod_cap::SensorError::MalformedSource`]).
//!
//! # Auswahl — welche Zeilen gemeldet werden, und warum
//! `/proc/meminfo` trägt auf modernen Kerneln über fünfzig Zeilen. Alle zu
//! melden ist die falsche Voreinstellung: es bläht den Metrikstrom auf, und
//! die meisten Werte (`Dirty`, `KernelStack`, `DirectMap4k`, …) beschreiben
//! keinen Speicherdruck, sondern interne Buchführung des Kernels, die kein
//! Verbraucher dieses Sensors braucht. Diese Crate meldet deshalb genau
//! **fünf** Felder, die zusammen die klassische Speicherdruck-Frage
//! beantworten — „wie viel Speicher ist noch nutzbar, und wie viel
//! Auslagerungsreserve steht dahinter":
//!
//! - `MemTotal` — die Gesamtgröße des physischen Speichers. Der Bezugswert,
//!   ohne den `MemFree`/`MemAvailable` nicht einordenbar sind.
//! - `MemFree` — vollständig ungenutzter Speicher. Der klassische, aber für
//!   sich genommen irreführende Wert: ein Linux-Kernel hält absichtlich
//!   wenig Speicher komplett frei, weil er lieber cacht.
//! - `MemAvailable` — die modernere, seit Kernel 3.14 verfügbare Schätzung
//!   dessen, was einer neuen Anwendung tatsächlich zur Verfügung stünde
//!   (`MemFree` plus reklaimierbare Caches). Der eigentliche
//!   Speicherdruck-Indikator, den `MemFree` allein nicht liefert.
//! - `SwapTotal` — die konfigurierte Auslagerungsreserve. Ohne diesen Wert
//!   ist `SwapFree` nicht einordenbar (0 kB frei bei 0 kB total heißt „kein
//!   Swap konfiguriert", nicht „Swap erschöpft").
//! - `SwapFree` — noch nicht genutzte Auslagerungsreserve.
//!
//! Diese fünf sind sowohl auf einem Host mit als auch ohne konfiguriertes
//! Swap vorhanden (`SwapTotal`/`SwapFree` sind auch bei `CONFIG_SWAP`-losen
//! Systemen mit dem Wert `0 kB` präsent, nicht als fehlende Zeile — siehe
//! den `no-swap`-Fixture-Fall). Die **Kardinalität ist damit fest auf
//! höchstens fünf** unterschiedliche Metriknamen je Abruf begrenzt,
//! unabhängig von Kernel-Version oder Host (siehe
//! [`sensor::MAX_CARDINALITY`]). Eine unbekannte Zeile (jede der übrigen
//! fünfzig-plus Zeilen, und jede Zeile, die ein künftiger Kernel neu
//! hinzufügt) wird stillschweigend übergangen, nicht zum Fehler — ein
//! Sensor, der an einer neuen, ihm unbekannten Zeile scheitert, fiele nach
//! jedem Kernel-Update aus.
//!
//! # Warum kein `#[derive(harw_macros::SensorSource)]`
//! `harw-macros` (Knoten AW2-06) deckt laut eigener Moduldoku genau einen
//! Fall ab: eine glob-adressierte Menge von Dateien, aus der **je Treffer
//! genau ein Skalar** über `parse_i64`/`parse_u64` gelesen wird, unter
//! **einem zur Kompilierzeit festen** Metriknamen für alle Treffer — der
//! sysfs-Fall. `/proc/meminfo` ist demgegenüber **eine** Datei mit **vielen**
//! `Schlüssel: Wert`-Zeilen, aus der **fünf verschiedene, namentlich
//! ausgewählte** Felder gelesen werden, jedes unter einem **eigenen**
//! Metriknamen und mit einer **Einheitenumrechnung**, die vom Wertinhalt
//! abhängt (`kB` vorhanden oder nicht). Dem Makro fehlen dafür drei
//! Attributformen, die es heute nicht kennt: eine Schlüsselauswahl innerhalb
//! einer Datei (statt eines Glob-Treffers je Datei), ein Metrikname je
//! ausgewähltem Schlüssel (statt eines einzigen für alle Treffer), und ein
//! bedingter Skalierungsfaktor (statt des unveränderten `wert as f64`).
//! Diese Crate implementiert [`harw_dod_signals::Sensor`] deshalb **von
//! Hand** (siehe [`sensor`]) und nutzt [`harw_dod_readfs::read_key_values`] —
//! eine Funktion, deren eigene Moduldoku wörtlich `/proc/meminfo` als
//! Beispiel nennt. Dieselbe Beobachtung gilt hier wie schon in
//! `harw-dod-cpu` (siehe dessen Moduldoku, Abschnitt „Warum kein
//! `#[derive(SensorSource)]`") und `harw-dod-thermal`: das Makro trägt
//! bislang nur den einfachsten Fall.
//!
//! # Nebenläufigkeit
//! [`sensor::MemorySensor`] hält keinen veränderlichen Zustand: `Send +
//! Sync` automatisch, `poll` nimmt `&self` und ist aus mehreren Threads
//! gleichzeitig sicher aufrufbar.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — inhaltsfrei, wie vom `Sensor`-Trait
//! vorgeschrieben. Diese Crate definiert keinen eigenen Fehlertyp: jeder
//! Fehlerpfad in [`sensor::MemorySensor::poll`] mündet in eine der vier
//! bestehenden `SensorError`-Varianten.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_memory::MemorySensor;
//! use harw_dod_signals::Sensor;
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
//! let handle = SensorHandle::new(SensorId::from_str("memory-0"), Capability::ReadProcMeminfo)
//!     .bind(scope);
//! let sensor = MemorySensor::from(handle);
//!
//! let reading = sensor.poll(jiff::Timestamp::now())?;
//! for sample in &reading.samples {
//!     println!("{}: {} bytes", sample.metric, sample.value);
//! }
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

pub mod sensor;

pub use sensor::MemorySensor;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
