//! Aufteilung des Portrait-Docks in Agenten- und Jobs-Spalte.
//!
//! Siehe `docs/planning/68-mobile-tui/README.md` Abschnitt 3.3: ab
//! `PHONE_DOCK_SPLIT_MIN_COLS` Spalten stehen Agenten links und Jobs rechts
//! nebeneinander (je ~50/50 Breite), darunter bleibt es eine einzige
//! kombinierte Liste. `placement.rs` nutzt dieselbe Konstante fuer das
//! `split`-Flag von `Placement::PortraitDock`, damit Aufteilung und
//! Anzeige-Entscheidung nie an zwei unabhaengig verstellbaren Zahlen haengen.

/// Mindestbreite des Docks in Spalten, ab der Agenten und Jobs nebeneinander
/// statt kombiniert dargestellt werden (siehe Moduldoku oben).
pub const PHONE_DOCK_SPLIT_MIN_COLS: u16 = 68;

/// Ergebnis der Dock-Aufteilung: entweder zwei nebeneinanderliegende
/// Rechtecke (Agenten links, Jobs rechts) oder ein einzelnes kombiniertes
/// Rechteck fuer schmale Docks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSplit {
    /// Agenten- und Jobs-Spalte nebeneinander (Agenten links, Jobs rechts).
    Split {
        /// Linke Spalte fuer den Agenten-Monitor.
        agents: crate::Rect,
        /// Rechte Spalte fuer die Job-Liste.
        jobs: crate::Rect,
    },
    /// Eine kombinierte Liste ueber die volle Dock-Breite (zu schmal zum Teilen).
    Combined(crate::Rect),
}

/// Teilt das Dock-Rechteck gemaess `PHONE_DOCK_SPLIT_MIN_COLS` auf.
///
/// Unterhalb der Schwelle bleibt das Rechteck unveraendert (`Combined`,
/// keine Beschneidung). Ab der Schwelle wird links/rechts geteilt, wobei die
/// Agenten-Spalte bei ungerader Breite die kleinere (abgerundete) Haelfte
/// erhaelt und die Jobs-Spalte den Rest -- beide Breiten summieren sich
/// immer exakt zur Dock-Breite. Reine, panic-freie Funktion mit sicherer
/// (saturierender) Arithmetik, auch fuer entartete Eingaben wie Breite 0.
pub fn split_dock(dock: crate::Rect) -> DockSplit {
    if dock.width < PHONE_DOCK_SPLIT_MIN_COLS {
        return DockSplit::Combined(dock);
    }
    let agents_width = dock.width / 2;
    let jobs_width = dock.width.saturating_sub(agents_width);
    let agents = crate::Rect {
        width: agents_width,
        ..dock
    };
    let jobs = crate::Rect {
        x: dock.x.saturating_add(agents_width),
        width: jobs_width,
        ..dock
    };
    DockSplit::Split { agents, jobs }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    /// Test-Fehlertyp dieser Datei: ersetzt `panic!`/`unwrap`/`expect` in
    /// Tests. Fehlschlaege werden als `Err` mit Kontext gemeldet.
    #[derive(Debug)]
    struct TestError(String);

    impl fmt::Display for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for TestError {}

    type TestResult<T = ()> = Result<T, TestError>;

    fn fail(context: &str) -> TestError {
        TestError(context.to_string())
    }

    fn base_dock(width: u16) -> crate::Rect {
        crate::Rect {
            x: 3,
            y: 5,
            width,
            height: 12,
        }
    }

    #[test]
    fn split_dock_combines_below_68_columns() -> TestResult {
        let dock = base_dock(67);
        match split_dock(dock) {
            DockSplit::Combined(rect) if rect == dock => Ok(()),
            other => Err(fail(&format!("expected Combined(dock), got {other:?}"))),
        }
    }

    #[test]
    fn split_dock_splits_agents_left_jobs_right_at_and_above_68_columns() -> TestResult {
        let dock = base_dock(68);
        match split_dock(dock) {
            DockSplit::Split { agents, jobs } => {
                if agents.width + jobs.width != dock.width {
                    return Err(fail("widths did not sum to dock width"));
                }
                if jobs.x != dock.x.saturating_add(agents.width) {
                    return Err(fail("jobs.x did not start right after agents"));
                }
                Ok(())
            }
            other => Err(fail(&format!("expected Split, got {other:?}"))),
        }
    }

    #[test]
    fn split_dock_never_loses_or_overlaps_columns_on_odd_widths() -> TestResult {
        let dock = base_dock(69);
        match split_dock(dock) {
            DockSplit::Split { agents, jobs } => {
                let sum = agents.width + jobs.width;
                if sum != dock.width {
                    return Err(fail("widths did not sum to dock width"));
                }
                let diff = agents.width.abs_diff(jobs.width);
                if diff > 1 {
                    return Err(fail("widths differ by more than one column"));
                }
                Ok(())
            }
            other => Err(fail(&format!("expected Split, got {other:?}"))),
        }
    }

    #[test]
    fn split_dock_returns_the_untouched_rect_when_combined() -> TestResult {
        let dock = base_dock(10);
        match split_dock(dock) {
            DockSplit::Combined(rect) if rect == dock => Ok(()),
            other => Err(fail(&format!(
                "expected the untouched input rect, got {other:?}"
            ))),
        }
    }

    #[test]
    fn split_dock_handles_a_zero_width_dock_without_panicking() -> TestResult {
        let dock = crate::Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        match split_dock(dock) {
            DockSplit::Combined(rect) if rect == dock => Ok(()),
            other => Err(fail(&format!("expected Combined(dock), got {other:?}"))),
        }
    }
}
