//! `harw-tool-fsread` — typisierte, rein lesende Dateisystem-Werkzeuge.
//!
//! Siehe `docs/planning/93-common-command-tools/README.md`. Die Familie
//! `fsread.*` ersetzt rohe `ls`/`stat`/`find`/`du`/`df`/`wc`/`head`/`tail`/
//! `cat`/`file`/`tree`/`readlink`/`realpath`/`sha256sum`/`b3sum`/`diff`/`jq`-
//! Aufrufe durch Werkzeuge mit typisierten Argumenten und begrenzter,
//! strukturierter Ausgabe. Alles ist reines Rust (`std::fs`, `rustix`,
//! `harw-fsutil`, `harw-digest`); es wird **kein** externer Befehl gestartet.
//!
//! # Gemeinsame Bausteine (von den Schwester-Crates wiederverwendet)
//! - [`scope`] — Auflösung eines Modell-Pfads gegen die Workspace-Wurzel.
//! - [`budget`] — Ausgabegrenzen, [`budget::Collector`], `ok`/`fail`.
//! - [`meta`] — `stat`-Sicht, Rechte-, Zeit- und Größenformate.
//! - [`walk`] — begrenzter, symlinkfester Walk mit Metadaten.
//! - [`users`] — Namen zu uid/gid aus `/etc/passwd`/`/etc/group`.
//! - [`blocking`] — `spawn_blocking`-Hilfe.
//!
//! # Sicherheit
//! `unsafe` ist verboten. Alle Pfade laufen durch [`scope::Scope`]; Symlinks
//! werden nie blind gefolgt, Geheimnis-Pfade nie gelesen.

#![forbid(unsafe_code)]

pub mod blocking;
pub mod budget;
pub mod cat;
pub mod df;
pub mod diff;
pub mod du;
pub mod file;
pub mod find;
pub mod hash;
pub mod headtail;
pub mod io;
pub mod json;
pub mod links;
pub mod ls;
pub mod meta;
pub mod provider;
pub mod scope;
pub mod stat;
pub mod textdiff;
pub mod tree;
pub mod users;
pub mod walk;
pub mod wc;

pub use provider::{FSREAD_TOOL_NAMES, FsreadToolProvider};

#[cfg(test)]
mod test_support;
