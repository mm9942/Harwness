//! Klassifikation eines Terminals in eine [`Placement`] anhand von Spalten-
//! und Zeilenzahl: breite Seitenspalte (`WideSide`), angedockter
//! Portrait-Dock (`PortraitDock`), Zusammenfassungszeile (`Summary`) oder
//! kompakter Fallback (`Compact`) ganz ohne Agenten-Flaeche.
//!
//! Die Konstanten und Formeln fuer `WideSide` und den Fallback sind
//! bewusste Spiegelungen (keine Re-Exports -- diese Crate hat keine
//! Abhaengigkeiten und kann `harw-tui` nicht importieren) der heutigen
//! Geometrie in `harw-tui/src/panes.rs` und `harw-tui/src/app.rs`, damit die
//! breite und die einspaltige Darstellung byte-identisch zu heute bleiben.

/// Kleinste Terminalbreite, ab der die rechte Agenten-Spalte neben dem Chat
/// erscheint (spiegelt `AGENTS_PANEL_MIN_TERMINAL_WIDTH` aus
/// `harw-tui/src/panes.rs`).
pub const WIDE_AGENT_PANEL_MIN_COLS: u16 = 100;
/// Kleinste Terminalbreite, ab der bei mittlerer Breite ein oben
/// angedockter Portrait-Dock statt des Zusammenfassungs-/Kompakt-Fallbacks
/// erscheint (siehe `docs/planning/68-mobile-tui/README.md`).
pub const PHONE_DOCK_MIN_COLS: u16 = 60;
/// Kleinste Terminalhoehe, ab der der Portrait-Dock statt des Fallbacks
/// erscheint.
pub const PHONE_DOCK_MIN_ROWS: u16 = 28;
/// Mindesthoehe des Portrait-Docks (und zugleich Mindesthoehe des
/// verbleibenden Chat-Arbeitsbereichs darunter).
pub const PHONE_DOCK_MIN_HEIGHT: u16 = 8;
/// Maximalhoehe des Portrait-Docks.
pub const PHONE_DOCK_MAX_HEIGHT: u16 = 15;

/// Breite des Agenten-Panels in der breiten Seitenspalten-Darstellung.
///
/// Bewusste Spiegelung von `AGENTS_WIDTH` (`harw-tui/src/panes.rs:41`),
/// kein Re-Export: diese Crate hat keine Abhaengigkeit auf `harw-tui`.
const AGENTS_WIDTH: u16 = 44;
/// Mindestbreite, die der Chat neben dem Agenten-Panel behaelt.
///
/// Bewusste Spiegelung von `MIN_CHAT_WIDTH` (`harw-tui/src/panes.rs:31`).
const MIN_CHAT_WIDTH: u16 = 48;
/// Kleinste Terminalhoehe, ab der die zusammengeklappte
/// Agenten-Zusammenfassung ueber der Statuszeile erscheint.
///
/// Bewusste Spiegelung von `AGENTS_SUMMARY_MIN_HEIGHT`
/// (`harw-tui/src/panes.rs:39`, verwendet in `harw-tui/src/app.rs:9511`).
const AGENTS_SUMMARY_MIN_HEIGHT: u16 = 16;

/// Welche Platzierung ein Terminal anhand seiner Groesse bekommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Breite Seitenspalte: Agenten-Panel rechts neben dem Chat (ab
    /// [`WIDE_AGENT_PANEL_MIN_COLS`] Spalten), wie heute in
    /// `harw-tui/src/panes.rs`.
    WideSide,
    /// Oben angedockter Portrait-Dock (Telefone/schmale, hohe Terminals).
    /// `split` gibt an, ob der Dock intern in Agenten-/Jobs-Spalte
    /// aufgeteilt ist (siehe [`crate::split_dock`]).
    PortraitDock { split: bool },
    /// Zusammengeklappte Agenten-Zusammenfassungszeile ueber der
    /// Statuszeile, wie heute in `harw-tui/src/app.rs`.
    Summary,
    /// Kompakter Fallback ohne jede Agenten-Flaeche (zu wenig Hoehe fuer
    /// die Zusammenfassungszeile).
    Compact,
}

/// Eingabegroessen fuer [`classify`]: Terminalabmessungen sowie die von der
/// aufrufenden Seite reservierten Zeilen fuer Status- und Eingabezeile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutInput {
    pub cols: u16,
    pub rows: u16,
    pub status_rows: u16,
    pub composer_rows: u16,
}

/// Ergebnis von [`classify`]: die gewaehlte Platzierung sowie die
/// resultierenden Teilflaechen.
///
/// `agents` ist der eine generische Slot fuer "die agenten-artige Flaeche":
/// das rechte Panel bei `WideSide`, der gesamte obere Dock bei
/// `PortraitDock` (aufrufende Seite reicht ihn an [`crate::split_dock`]
/// weiter), und `None` bei `Summary`/`Compact` -- die einzeilige
/// Zusammenfassung wird dort von der aufrufenden Seite selbst in ihrer
/// eigenen Flaeche gezeichnet, genau wie heute, wo die eingeklappte
/// Zusammenfassung ebenfalls kein eigenes `PaneAreas`-Rechteck bekommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenLayout {
    pub placement: Placement,
    pub chat: crate::Rect,
    pub agents: Option<crate::Rect>,
    pub status: crate::Rect,
    pub composer: crate::Rect,
}

/// Klassifiziert ein Terminal anhand seiner Groesse in eine [`ScreenLayout`].
///
/// Reine Funktion: keine E/A, keine Panics, nur saettigende Arithmetik.
/// Schwellwerte und Formeln siehe [`WIDE_AGENT_PANEL_MIN_COLS`],
/// [`PHONE_DOCK_MIN_COLS`]/[`PHONE_DOCK_MIN_ROWS`] sowie die private
/// Fallback-Formel (Spiegelung von `harw-tui/src/app.rs:9504-9519`).
pub fn classify(input: LayoutInput) -> ScreenLayout {
    let LayoutInput {
        cols,
        rows,
        status_rows,
        composer_rows,
    } = input;

    let bottom = status_rows.saturating_add(composer_rows);
    let body_rows = rows.saturating_sub(bottom);

    let status = crate::Rect {
        x: 0,
        y: rows.saturating_sub(bottom),
        width: cols,
        height: status_rows,
    };
    let composer = crate::Rect {
        x: 0,
        y: rows.saturating_sub(composer_rows),
        width: cols,
        height: composer_rows,
    };

    if cols >= WIDE_AGENT_PANEL_MIN_COLS {
        return wide_side(cols, body_rows, status, composer);
    }
    if cols >= PHONE_DOCK_MIN_COLS && rows >= PHONE_DOCK_MIN_ROWS {
        return portrait_dock(cols, body_rows, status, composer);
    }
    fallback(cols, body_rows, rows, status_rows, composer_rows, status, composer)
}

/// Breite Seitenspalte: Agenten-Panel rechts, wie `harw-tui/src/panes.rs`
/// (Funktion `split`) es heute berechnet.
fn wide_side(cols: u16, body_rows: u16, status: crate::Rect, composer: crate::Rect) -> ScreenLayout {
    let budget = cols.saturating_sub(MIN_CHAT_WIDTH);
    let agents_width = if budget >= AGENTS_WIDTH { AGENTS_WIDTH } else { 0 };
    let agents = if agents_width > 0 {
        Some(crate::Rect {
            x: cols.saturating_sub(agents_width),
            y: 0,
            width: agents_width,
            height: body_rows,
        })
    } else {
        None
    };
    let chat = crate::Rect {
        x: 0,
        y: 0,
        width: cols.saturating_sub(agents_width),
        height: body_rows,
    };
    ScreenLayout {
        placement: Placement::WideSide,
        chat,
        agents,
        status,
        composer,
    }
}

/// Oben angedockter Portrait-Dock: Dock ueber der vollen Breite, Chat
/// direkt darunter, Status/Eingabe ganz unten. Hoehe des Docks: ein Drittel
/// der Arbeitsflaeche, begrenzt auf [`PHONE_DOCK_MIN_HEIGHT`]..=
/// [`PHONE_DOCK_MAX_HEIGHT`], mit einer Mindesthoehe fuer den verbleibenden
/// Chat-Bereich (siehe `docs/planning/68-mobile-tui/README.md`).
fn portrait_dock(cols: u16, body_rows: u16, status: crate::Rect, composer: crate::Rect) -> ScreenLayout {
    let target = body_rows / 3;
    let mut dock_height = target
        .clamp(PHONE_DOCK_MIN_HEIGHT, PHONE_DOCK_MAX_HEIGHT)
        .min(body_rows);
    if body_rows.saturating_sub(dock_height) < PHONE_DOCK_MIN_HEIGHT {
        dock_height = body_rows.saturating_sub(PHONE_DOCK_MIN_HEIGHT);
    }
    let chat_height = body_rows.saturating_sub(dock_height);
    let dock_rect = crate::Rect {
        x: 0,
        y: 0,
        width: cols,
        height: dock_height,
    };
    let chat = crate::Rect {
        x: 0,
        y: dock_height,
        width: cols,
        height: chat_height,
    };
    let split = cols >= crate::dock::PHONE_DOCK_SPLIT_MIN_COLS;
    ScreenLayout {
        placement: Placement::PortraitDock { split },
        chat,
        agents: Some(dock_rect),
        status,
        composer,
    }
}

/// Zusammenfassungszeile oder kompakter Fallback fuer zu schmale/niedrige
/// Terminals ohne Portrait-Dock. Spiegelt woertlich die heutige Formel aus
/// `harw-tui/src/app.rs:9504-9519` (nicht die im README skizzierte
/// aspirative 20-Zeilen-Bandbreite -- siehe deltas).
#[allow(clippy::too_many_arguments)]
fn fallback(
    cols: u16,
    body_rows: u16,
    rows: u16,
    status_rows: u16,
    composer_rows: u16,
    status: crate::Rect,
    composer: crate::Rect,
) -> ScreenLayout {
    let headroom = status_rows.saturating_add(composer_rows).saturating_add(2);
    let placement = if rows >= AGENTS_SUMMARY_MIN_HEIGHT && rows > headroom {
        Placement::Summary
    } else {
        Placement::Compact
    };
    let chat = crate::Rect {
        x: 0,
        y: 0,
        width: cols,
        height: body_rows,
    };
    ScreenLayout {
        placement,
        chat,
        agents: None,
        status,
        composer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn input(cols: u16, rows: u16, status_rows: u16, composer_rows: u16) -> LayoutInput {
        LayoutInput {
            cols,
            rows,
            status_rows,
            composer_rows,
        }
    }

    #[test]
    fn classify_matches_current_wide_geometry_at_160x40_120x30_100x30() -> TestResult {
        for &(cols, rows) in &[(160u16, 40u16), (120, 30), (100, 30)] {
            let out = classify(input(cols, rows, 1, 3));
            assert_eq!(out.placement, Placement::WideSide);
            let agents = out.agents.ok_or("WideSide must have an agents rect")?;
            assert_eq!(agents.width, 44);
            assert_eq!(out.chat.width, cols - 44);
            assert_eq!(out.chat.x, 0);
            assert_eq!(agents.x, cols - 44);
        }
        Ok(())
    }

    #[test]
    fn classify_places_portrait_dock_between_60_and_100_cols_at_rows_28_or_more() {
        for &(cols, rows) in &[(99u16, 40u16), (80, 40)] {
            let out = classify(input(cols, rows, 1, 3));
            let expected_split = cols >= crate::dock::PHONE_DOCK_SPLIT_MIN_COLS;
            assert_eq!(out.placement, Placement::PortraitDock { split: expected_split });
            assert!(out.agents.is_some());
        }
    }

    #[test]
    fn classify_splits_the_dock_at_68_cols_and_combines_below_it() {
        let split = classify(input(72, 36, 1, 3));
        assert_eq!(split.placement, Placement::PortraitDock { split: true });

        let combined = classify(input(64, 36, 1, 3));
        assert_eq!(combined.placement, Placement::PortraitDock { split: false });

        // Minimale Randbedingung: Dock-Hoehe auf 8 begrenzt.
        let boundary = classify(input(60, 28, 1, 3));
        assert_eq!(boundary.placement, Placement::PortraitDock { split: false });
        assert_eq!(boundary.agents.map(|rect| rect.height), Some(8));
    }

    #[test]
    fn classify_clamps_dock_height_to_8_15_and_never_exceeds_body_rows() -> TestResult {
        for rows in [28u16, 40, 60, 200, 1000] {
            let out = classify(input(80, rows, 1, 3));
            if let Placement::PortraitDock { .. } = out.placement {
                let agents = out.agents.ok_or("PortraitDock must have agents")?;
                let bottom = 1u16.saturating_add(3);
                let body_rows = rows.saturating_sub(bottom);
                if body_rows >= PHONE_DOCK_MIN_HEIGHT * 2 {
                    assert!(agents.height >= PHONE_DOCK_MIN_HEIGHT);
                    assert!(agents.height <= PHONE_DOCK_MAX_HEIGHT);
                }
                assert!(agents.height <= body_rows);
            }
        }
        Ok(())
    }

    #[test]
    fn classify_falls_back_to_summary_or_compact_exactly_like_app_rs_today() {
        // 59x40: zu schmal fuer den Dock (cols < 60), landet im Fallback.
        let out = classify(input(59, 40, 1, 3));
        let headroom = 1u16.saturating_add(3).saturating_add(2);
        let expected = if 40 >= AGENTS_SUMMARY_MIN_HEIGHT && 40 > headroom {
            Placement::Summary
        } else {
            Placement::Compact
        };
        assert_eq!(out.placement, expected);
        assert!(out.agents.is_none());

        // 80x24: rows < 28, also kein Dock trotz ausreichender Breite.
        let out = classify(input(80, 24, 1, 3));
        assert!(out.agents.is_none());

        // 80x19: noch niedriger, muss weiterhin Fallback bleiben.
        let out = classify(input(80, 19, 1, 3));
        assert!(out.agents.is_none());
        let headroom19 = 1u16.saturating_add(3).saturating_add(2);
        let expected19 = if 19 >= AGENTS_SUMMARY_MIN_HEIGHT && 19 > headroom19 {
            Placement::Summary
        } else {
            Placement::Compact
        };
        assert_eq!(out.placement, expected19);
    }

    #[test]
    fn classify_never_produces_a_rect_that_exceeds_the_screen_or_overlaps() {
        let cases: &[(u16, u16, u16, u16)] = &[
            (160, 40, 1, 3),
            (120, 30, 1, 3),
            (100, 30, 1, 3),
            (99, 40, 1, 3),
            (80, 40, 1, 3),
            (72, 36, 1, 3),
            (64, 36, 1, 3),
            (60, 28, 1, 3),
            (59, 40, 1, 3),
            (80, 24, 1, 3),
            (80, 19, 1, 3),
            (0, 0, 0, 0),
            (1, 1, 0, 0),
            (300, 300, 5, 5),
        ];
        for &(cols, rows, status_rows, composer_rows) in cases {
            let out = classify(input(cols, rows, status_rows, composer_rows));
            assert!(out.chat.y.saturating_add(out.chat.height) <= rows);
            assert!(out.status.y.saturating_add(out.status.height) <= rows);
            assert!(out.composer.y.saturating_add(out.composer.height) <= rows);
            if let Placement::PortraitDock { .. } = out.placement {
                let agents = out.agents;
                assert!(agents.is_some());
                if let Some(agents) = agents {
                    let bottom = status_rows.saturating_add(composer_rows);
                    let body_rows = rows.saturating_sub(bottom);
                    assert!(agents.height.saturating_add(out.chat.height) <= body_rows);
                    assert_eq!(agents.y, 0);
                    assert_eq!(out.chat.y, agents.height);
                }
            }
        }
    }
}
