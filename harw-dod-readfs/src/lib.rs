//! Getypte Lesezugriffe unter einem `ReadScope`.
//!
//! # Zweck
//! `harw-dod-readfs` ist **die einzige Dateisystem-Naht der
//! unprivilegierten Sensoren**. Elf Sensor-Crates lesen ihre Quellen
//! ausschließlich über die Funktionen dieser Crate; **kein Sensor ruft je
//! `std::fs` direkt auf.** Das ist der Grund, warum es diese Crate gibt:
//! eine Bereichsprüfung, die in elf Crates wiederholt wird, ist elfmal eine
//! Gelegenheit, sie falsch zu machen. Hier steht sie genau einmal.
//!
//! # Verantwortungsbereich
//! - [`read`]: getypte Leser, die auf
//!   [`harw_dod_cap::ReadScope::open`] aufbauen — Symlink-Auflösung und
//!   Bereichsprüfung passieren dort, **nicht** hier noch einmal.
//! - [`glob`]: Musterabgleich relativ zum Bereich, mit einem eigenen,
//!   kleinen Wildcard-Matcher statt einer neuen Abhängigkeit (Begründung in
//!   der Modul-Dokumentation von [`glob`]).
//! - [`error`]: [`ReadFsError`], die einzige Fehlermenge dieser Crate.
//!
//! Was diese Crate **nicht** tut: Bereiche konstruieren oder schneiden (das
//! ist [`harw_dod_cap::ReadScope::from_roots`] /
//! [`harw_dod_cap::ReadScope::intersection`]), Symlinks auflösen (das
//! erledigt [`harw_dod_cap::ReadScope::open`] bereits, **bevor** die
//! Bereichsprüfung greift), oder Sensordaten interpretieren
//! (`HostSample`/`SecurityEvent` liegen in `harw-dod-signals`).
//!
//! # Exportierte Typen
//! [`ReadFsError`] und `ReadFsResult<T>` (vom `HarwError`-Derive erzeugter
//! Alias), sowie die freien Funktionen [`read_to_string`], [`read_lines`],
//! [`read_first_line`], [`parse_i64`], [`parse_u64`], [`read_key_values`],
//! die Konstante [`MAX_READ_BYTES`], und [`glob::glob`] (bewusst nicht am
//! Crate-Wurzel re-exportiert, siehe unten).
//!
//! # Nebenläufigkeit
//! Zustandslose freie Funktionen über `&ReadScope` und `&Path`. `Send +
//! Sync` ohne innere Veränderlichkeit, keine Sperren, keine Threads. Jeder
//! Aufruf öffnet und schließt seine eigene Datei bzw. sein eigenes
//! Verzeichnis-Handle; parallele Aufrufe verschiedener Threads stören sich
//! nicht.
//!
//! # Fehler
//! [`ReadFsError`] — inhaltsfrei: kein aufgelöster Dateisystempfad, kein
//! Dateiinhalt und keine gelesene Zeile erscheinen in einer Fehlermeldung.
//! Die einzige Ausnahme sind die beiden Glob-Musterfehler, deren Meldung das
//! vom Aufrufer selbst getippte, nicht aufgelöste Muster nennt (siehe die
//! Modul-Dokumentation von [`error`]).
//!
//! # Warum `glob::glob` nicht am Crate-Wurzel re-exportiert ist
//! Diese Crate deklariert sowohl ein Modul `glob` als auch — darin — eine
//! Funktion `glob`. Rust erlaubt einen gleichnamigen Typ- und
//! Wert-Namensraum-Eintrag im selben Sichtbarkeitsbereich (die
//! `syn`-Crate macht das ebenso mit `syn::parse`, Modul und Funktion), ein
//! `pub use glob::glob;` hier wäre also vermutlich zulässig. Da diese Crate
//! aber **kein `cargo` ausführen darf**, um das für dieses Projekt zu
//! verifizieren, bleibt [`glob::glob`] bewusst nur über den Modulpfad
//! erreichbar — die sichere Wahl ohne Kompilierprobe.
//!
//! # Examples
//! ```rust,no_run
//! use std::path::Path;
//! use harw_dod_cap::ReadScope;
//!
//! let scope = ReadScope::from_roots([Path::new("/sys/class/thermal").to_path_buf()]);
//! let millidegrees = harw_dod_readfs::parse_i64(
//!     &scope,
//!     Path::new("/sys/class/thermal/thermal_zone0/temp"),
//! )?;
//! # Ok::<(), harw_dod_readfs::ReadFsError>(())
//! ```

pub mod error;
pub mod glob;
pub mod read;

pub use error::{ReadFsError, ReadFsResult};
pub use read::{
    MAX_READ_BYTES, parse_i64, parse_u64, read_first_line, read_key_values, read_line_fields,
    read_lines, read_to_string,
};
