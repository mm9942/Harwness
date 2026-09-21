//! Re-export von [`harw_types::cancel`] — die eigentliche Definition lebt
//! dort, damit sowohl `harw-core` als auch `harw-tools` (das nicht von
//! `harw-core` abhängen darf) `CancelToken` nutzen können. Dieser Pfad
//! bleibt für Bestandscode erhalten: `harw_core::cancel::{CancelToken,
//! CancelReason}` funktioniert unverändert für alle vorhandenen Aufrufer
//! (u. a. `harw-provider-http/src/retry.rs`, `harw-tui/src/app.rs`,
//! `harw-core/src/model.rs`, `harw-core/src/turn_loop.rs`).
//!
//! Verschoben im Rahmen von F-160/G-017: `harw-core` hängt bereits von
//! `harw-tools` ab (normale, nicht Dev-Abhängigkeit), sodass `CancelToken`
//! nicht länger ausschließlich hier definiert sein konnte, ohne
//! `ToolExecutionContext` (`harw-tools/src/executor.rs`) den Zugriff zu
//! versperren. `harw-types` hängt von keiner der beiden Crates ab und beide
//! hängen bereits von `harw-types` ab — der naheliegende gemeinsame Ort.
pub use harw_types::cancel::{CancelReason, CancelToken};
