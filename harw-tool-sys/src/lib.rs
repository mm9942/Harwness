//! `harw-tool-sys` — typisierte, rein lesende Prozess-/System-Werkzeuge.
//!
//! Siehe `docs/planning/93-common-command-tools/README.md`. Die Familie `sys.*`
//! ersetzt rohe `ps`/`pgrep`/`top`/`free`/`uptime`/`uname`/`env`/`id`/`whoami`/
//! `ss`/`netstat`/`lsof`/`date`/`which`-Aufrufe durch Werkzeuge mit
//! typisierten Argumenten und begrenzter, strukturierter Ausgabe. Alles ist
//! reines Rust (`/proc`, `std::fs`, `rustix`); es wird **kein** externer
//! Befehl gestartet.
//!
//! # Sicherheit
//! `unsafe` ist verboten. Kommandozeilen und Umgebungen werden über
//! [`mask`] maskiert (Name und Wertform); Filter wirken nur auf den
//! maskierten Text. Alle Werkzeuge verlangen `ExecuteProcess`.

#![forbid(unsafe_code)]

pub mod date;
pub mod env;
pub mod id;
pub mod lsof;
pub mod mask;
pub mod net;
pub mod pgrep;
pub mod procfs;
pub mod provider;
pub mod ps;
pub mod sysinfo;
pub mod top;
pub mod which;

pub use provider::{SYS_TOOL_NAMES, SysToolProvider};

#[cfg(test)]
mod test_support;
