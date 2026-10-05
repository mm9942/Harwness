//! `harw-tool-cargo` — typisierte Cargo-Werkzeuge mit zusammengefasster Ausgabe.
//!
//! Siehe `docs/planning/93-common-command-tools/README.md`. Die Familie
//! `cargo.*` (`check`, `build`, `test`, `clippy`, `fmt_check`, `tree`, `doc`,
//! `metadata`) ersetzt rohe `cargo`-Aufrufe durch Werkzeuge mit typisierten,
//! validierten Argumenten und liefert statt des Rohlogs eine Zusammenfassung:
//! Status, Dauer, Fehler und Warnungen mit `datei:zeile:spalte`, Testzähler und
//! Fehlschläge, dazu einen gekürzten Rohlog.
//!
//! # Prozessstart
//! Dieses Crate startet **keinen** Prozess selbst. Das Kommando wird aus den
//! validierten Argumenten als fester `argv` gebaut, POSIX-sicher gequotet und
//! an einen injizierten [`ShellDelegate`] übergeben, im Betrieb der
//! `shell.exec`-Ausführer aus `harw-tool-shell` (bwrap-Sandbox, rlimits,
//! Host-Permit-Prüfung, Zeitlimit, Ausgabekappung). `--`-Argumente gibt es nur
//! über eine Allowlist ([`command::harness_arg`], [`command::lint_flag`]).
//!
//! # Sicherheit
//! `unsafe` ist verboten. Namen, Features, Profile und Testfilter sind auf
//! feste Zeichenmengen begrenzt und dürfen nicht mit `-` beginnen; Optionen,
//! die ein Werkzeug nicht kennt (`--manifest-path`, `--config`, `--fix`,
//! `--target-dir`, …), existieren nicht als Feld und werden abgelehnt.

#![forbid(unsafe_code)]

pub mod command;
pub mod parse;
pub mod plan;
pub mod report;
pub mod shell;
pub mod tools;

#[cfg(test)]
mod test_support;

pub use shell::ShellDelegate;
pub use tools::{CARGO_TOOL_NAMES, CargoToolProvider};
