//! `harw-tool-process` — stellt die Bibliotheks-API von `harw-killer` als
//! Agent-Werkzeuge bereit: `process.list` (rein lesende Vorschau, sendet nie
//! ein Signal) und `process.kill` (SIGKILL → Warten → zweites SIGKILL, nur
//! für Prozesse des eigenen effektiven Benutzers, nie über sudo).
//!
//! Beide Werkzeuge verlangen [`harw_authority::Permission::ExecuteProcess`]
//! und sind nur unter Linux funktionsfähig; auf anderen Zielen liefern sie
//! einen Tool-Fehler.

mod provider;

pub use provider::{
    DEFAULT_KILL_WAIT_SECS, DEFAULT_TIMEOUT_SECS, MAX_KILL_WAIT_SECS, MAX_TIMEOUT_SECS,
    PROCESS_TOOL_NAMES, ProcessToolProvider,
};
