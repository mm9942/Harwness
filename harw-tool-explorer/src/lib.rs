//! `harw-tool-explorer` — stellt den universellen [`harw_explorer`]-Index als
//! Agent-Werkzeuge bereit: `explore.tree`, `explore.projects`,
//! `explore.relations`, `explore.find`. Alle Werkzeuge sind rein lesend und
//! auf die beim Bau übergebene Wurzel begrenzt.

mod provider;

pub use provider::{EXPLORER_TOOL_NAMES, ExplorerToolProvider};
