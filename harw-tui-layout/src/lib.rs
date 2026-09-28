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

mod dock;
mod placement;

pub use dock::{split_dock, DockSplit, PHONE_DOCK_SPLIT_MIN_COLS};
pub use placement::{
    classify, LayoutInput, Placement, ScreenLayout, PHONE_DOCK_MAX_HEIGHT, PHONE_DOCK_MIN_COLS,
    PHONE_DOCK_MIN_HEIGHT, PHONE_DOCK_MIN_ROWS, WIDE_AGENT_PANEL_MIN_COLS,
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
