//! Reine Layout-Klassifikation fuer `harw-tui`: welche Platzierung (breite
//! Seitenspalte, angedockter Portrait-Dock, Zusammenfassungszeile, oder
//! kompakter Fallback) ein Terminal aus Spalten-/Zeilenzahl bekommt, sowie
//! die interne Aufteilung des Docks in Agenten-/Jobs-Spalte.
//!
//! Keine E/A, keine Panics, keine dritten Typen in der oeffentlichen API,
//! keine Abhaengigkeiten ausser `std` (Ring F, siehe
//! `xtask/arch-policy.toml`). `harw-tui` bleibt Eigentuemer von `AgentMonitor`
//! und den Job-Zeilen; diese Crate liefert nur Geometrie.
//!
//! Siehe `docs/planning/68-mobile-tui/README.md` und
//! `docs/planning/68-mobile-tui/contracts/W00-phone-top-dock.md` fuer die
//! Herleitung der Schwellwerte.
//!
//! Mission Control geometry (portrait stack, three-column landscape, shallow
//! landscape, compact summary) is computed by [`classify_mission`]; see
//! `docs/planning/97-harw-dod-mission-control-chain/README.md` §3.3.2.

mod dock;
mod mini_live;
mod mission;
mod placement;

pub use dock::{DockSplit, PHONE_DOCK_SPLIT_MIN_COLS, split_dock};
pub use mini_live::{
    CARDS_ROWS, ChatFirstInput, ChatFirstLayout, LiveMode, MINI_LIVE_MAX_BODY_LINES,
    MINI_LIVE_MIN_CHAT_ROWS, chat_first,
};
pub use mission::{
    FINISHED_COLLAPSED_ROWS, FINISHED_EXPANDED_MAX_ROWS, FINISHED_EXPANDED_MIN_ROWS,
    LANDSCAPE_CENTER_MIN_COLS, LANDSCAPE_CHAT_MIN_COLS, LANDSCAPE_DENSE_CENTER_MIN_COLS,
    LANDSCAPE_DENSE_CHAT_MIN_COLS, LANDSCAPE_DENSE_RIGHT_MIN_COLS, LANDSCAPE_MIN_ROWS_BODY,
    LANDSCAPE_RIGHT_MIN_COLS, MissionInput, MissionLayout, MissionPlacement, Orientation,
    classify_mission, orientation,
};
pub use placement::{
    LayoutInput, PHONE_DOCK_MAX_HEIGHT, PHONE_DOCK_MIN_COLS, PHONE_DOCK_MIN_HEIGHT,
    PHONE_DOCK_MIN_ROWS, Placement, ScreenLayout, WIDE_AGENT_PANEL_MIN_COLS, classify,
};

/// Rechteck in Zellenkoordinaten (Spalten/Zeilen), Ursprung oben links --
/// wie `ratatui::layout::Rect`, aber ohne die Abhaengigkeit: diese Crate hat
/// keine dritten Typen in ihrer oeffentlichen API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
