//! Chat-first-Layout mit Mini-Live-Bereich (PL-94 W02 U0).
//!
//! Reine Geometrie: der Chat bekommt immer zuerst seinen Mindestplatz, nur
//! der Rest der Hoehe (das "Budget") wird an optionale Bloecke oberhalb des
//! Chats verteilt. Prioritaet von hoch nach niedrig:
//!
//! 1. Karten-Zeile (`CARDS_ROWS`, ganz oder gar nicht)
//! 2. Finished-Streifen (1 Zeile)
//! 3. Live-Kopfzeile (1 Zeile)
//! 4. Live-Koerper (bis zu `MINI_LIVE_MAX_BODY_LINES` Zeilen)
//!
//! Passt ein Block nicht mehr ins Restbudget, entfallen er und alle Bloecke
//! niedrigerer Prioritaet. Der Chat erhaelt den gesamten Rest.
//!
//! Vertikale Reihenfolge von oben nach unten: Karten, Finished, Live
//! (Kopf und Koerper als ein Rechteck), Chat.
//!
//! Invarianten (fuer jede Eingabe, auch entartete):
//! - Alle Rechtecke haben `x == area.x` und `width == area.width`.
//! - Die Rechtecke sind paarweise disjunkt und liegen vollstaendig in `area`.
//! - Ihre Vereinigung ist genau `area` (Hoehen summieren sich zu
//!   `area.height`).
//! - `chat.height >= min(area.height, max(min_chat_rows, MINI_LIVE_MIN_CHAT_ROWS))`.
//! - Bei Breite 0 oder Hoehe 0 gibt es keine optionalen Bloecke und
//!   `chat == area`.
//!
//! Keine E/A, keine Panics, nur saturierende Arithmetik, nur `std`.

use crate::Rect;
use crate::dock::{DockSplit, split_dock};

/// Absolute Untergrenze fuer die Chat-Hoehe in Zeilen: auch wenn der Aufrufer
/// weniger verlangt, bekommt der Chat mindestens so viel Platz (sofern die
/// Flaeche so hoch ist).
pub const MINI_LIVE_MIN_CHAT_ROWS: u16 = 8;

/// Maximale Anzahl Zeilen des Live-Koerpers unterhalb der Live-Kopfzeile.
pub const MINI_LIVE_MAX_BODY_LINES: u16 = 4;

/// Hoehe der Karten-Zeile in Zeilen: eine Titelzeile plus 3 Inhaltszeilen.
/// Die Karten-Zeile wird ganz oder gar nicht gezeigt.
pub const CARDS_ROWS: u16 = 4;

/// Eingabe fuer [`chat_first`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatFirstInput {
    /// Gesamte verfuegbare Flaeche.
    pub area: Rect,
    /// Gewuenschte Zeilenzahl des Live-Koerpers (wird auf
    /// `MINI_LIVE_MAX_BODY_LINES` begrenzt; 0 heisst: hoechstens Kopfzeile).
    pub live_body_lines_wanted: u16,
    /// Gewuenschte Mindesthoehe des Chats; wirkt als
    /// `max(min_chat_rows, MINI_LIVE_MIN_CHAT_ROWS)`.
    pub min_chat_rows: u16,
}

/// Darstellungsstufe des Live-Bereichs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveMode {
    /// Kein Live-Bereich.
    Hidden,
    /// Nur die Kopfzeile (1 Zeile).
    Header,
    /// Kopfzeile plus Koerper mit `lines` Zeilen (1..=`MINI_LIVE_MAX_BODY_LINES`).
    Body {
        /// Zeilenzahl des Koerpers ohne Kopfzeile.
        lines: u16,
    },
}

/// Ergebnis von [`chat_first`]: alle Rechtecke von oben nach unten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatFirstLayout {
    /// Karten-Zeile (`CARDS_ROWS` hoch), aufgeteilt per `split_dock`.
    pub cards: Option<DockSplit>,
    /// Finished-Streifen (1 Zeile).
    pub finished: Option<Rect>,
    /// Live-Bereich (Kopfzeile und Koerper zusammen).
    pub live: Option<Rect>,
    /// Darstellungsstufe des Live-Bereichs; `Hidden` genau dann, wenn
    /// `live` den Wert `None` hat.
    pub live_mode: LiveMode,
    /// Chat-Bereich: der gesamte Rest der Flaeche.
    pub chat: Rect,
}

/// Berechnet das Chat-first-Layout (siehe Moduldoku fuer Prioritaeten und
/// Invarianten).
pub fn chat_first(input: ChatFirstInput) -> ChatFirstLayout {
    let area = input.area;
    if area.width == 0 || area.height == 0 {
        return ChatFirstLayout {
            cards: None,
            finished: None,
            live: None,
            live_mode: LiveMode::Hidden,
            chat: area,
        };
    }

    let floor = input.min_chat_rows.max(MINI_LIVE_MIN_CHAT_ROWS);
    let mut remaining = area.height.saturating_sub(floor);
    let mut y = area.y;
    let row = |y: u16, height: u16| Rect {
        x: area.x,
        y,
        width: area.width,
        height,
    };

    // 1. Karten-Zeile: ganz oder gar nicht.
    let cards = if remaining >= CARDS_ROWS {
        remaining = remaining.saturating_sub(CARDS_ROWS);
        let rect = row(y, CARDS_ROWS);
        y = y.saturating_add(CARDS_ROWS);
        Some(split_dock(rect))
    } else {
        None
    };

    // 2. Finished-Streifen: nur wenn die Karten sichtbar sind.
    let finished = if cards.is_some() && remaining >= 1 {
        remaining = remaining.saturating_sub(1);
        let rect = row(y, 1);
        y = y.saturating_add(1);
        Some(rect)
    } else {
        None
    };

    // 3. und 4. Live-Kopfzeile und -Koerper: nur wenn der Streifen sichtbar ist.
    let mut live = None;
    let mut live_mode = LiveMode::Hidden;
    if finished.is_some() && remaining >= 1 {
        remaining = remaining.saturating_sub(1);
        let wanted = input.live_body_lines_wanted.min(MINI_LIVE_MAX_BODY_LINES);
        let lines = wanted.min(remaining);
        let height = lines.saturating_add(1);
        live = Some(row(y, height));
        y = y.saturating_add(height);
        live_mode = if lines == 0 {
            LiveMode::Header
        } else {
            LiveMode::Body { lines }
        };
    }

    let chat_height = area.y.saturating_add(area.height).saturating_sub(y);
    ChatFirstLayout {
        cards,
        finished,
        live,
        live_mode,
        chat: row(y, chat_height),
    }
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

    fn area(width: u16, height: u16) -> Rect {
        Rect {
            x: 2,
            y: 3,
            width,
            height,
        }
    }

    fn input(width: u16, height: u16, wanted: u16, min_chat_rows: u16) -> ChatFirstInput {
        ChatFirstInput {
            area: area(width, height),
            live_body_lines_wanted: wanted,
            min_chat_rows,
        }
    }

    fn dock_rect(split: DockSplit) -> TestResult<Rect> {
        match split {
            DockSplit::Combined(rect) => Ok(rect),
            DockSplit::Split { agents, jobs } => {
                if agents.y != jobs.y || agents.height != jobs.height {
                    return Err(fail("split halves are not on the same row band"));
                }
                if jobs.x != agents.x.saturating_add(agents.width) {
                    return Err(fail("split halves are not adjacent"));
                }
                Ok(Rect {
                    width: agents.width.saturating_add(jobs.width),
                    ..agents
                })
            }
        }
    }

    /// Prueft Partition: Reihenfolge, keine Ueberlappung, innerhalb der
    /// Flaeche, Hoehensumme, Chat-Untergrenze und Konsistenz von `live_mode`.
    fn assert_partition(layout: &ChatFirstLayout, area: Rect, floor: u16) -> TestResult {
        let mut rects: Vec<Rect> = Vec::new();
        if let Some(split) = layout.cards {
            let rect = dock_rect(split)?;
            if rect.height != CARDS_ROWS {
                return Err(fail("cards row has the wrong height"));
            }
            rects.push(rect);
        }
        if let Some(rect) = layout.finished {
            if rect.height != 1 {
                return Err(fail("finished strip is not one row"));
            }
            rects.push(rect);
        }
        if let Some(rect) = layout.live {
            rects.push(rect);
        }
        rects.push(layout.chat);

        let mut next_y = u32::from(area.y);
        let mut total = 0u32;
        for rect in &rects {
            if rect.x != area.x || rect.width != area.width {
                return Err(fail(&format!("rect not full width: {rect:?}")));
            }
            if u32::from(rect.y) != next_y {
                return Err(fail(&format!("rect overlaps or leaves a gap: {rect:?}")));
            }
            next_y += u32::from(rect.height);
            total += u32::from(rect.height);
        }
        if total != u32::from(area.height) {
            return Err(fail(&format!(
                "heights sum to {total}, expected {}",
                area.height
            )));
        }
        if next_y > u32::from(area.y) + u32::from(area.height) {
            return Err(fail("rects extend beyond the area"));
        }
        if layout.chat.height < area.height.min(floor) {
            return Err(fail(&format!(
                "chat height {} below floor for area {area:?}",
                layout.chat.height
            )));
        }
        match (layout.live, layout.live_mode) {
            (None, LiveMode::Hidden) => {}
            (Some(rect), LiveMode::Header) if rect.height == 1 => {}
            (Some(rect), LiveMode::Body { lines })
                if (1..=MINI_LIVE_MAX_BODY_LINES).contains(&lines) && rect.height == lines + 1 => {}
            other => return Err(fail(&format!("inconsistent live state: {other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn table_of_terminal_sizes_partitions_the_area() -> TestResult {
        let sizes: [(u16, u16); 10] = [
            (46, 78),
            (48, 65),
            (70, 90),
            (39, 60),
            (48, 19),
            (80, 40),
            (120, 42),
            (160, 45),
            (1, 1),
            (0, 0),
        ];
        for (width, height) in sizes {
            for (wanted, min_rows) in [(0u16, 0u16), (2, 0), (4, 0), (99, 0), (99, 20), (4, 200)] {
                let inp = input(width, height, wanted, min_rows);
                let layout = chat_first(inp);
                let floor = min_rows.max(MINI_LIVE_MIN_CHAT_ROWS);
                assert_partition(&layout, inp.area, floor).map_err(|e| {
                    fail(&format!(
                        "{width}x{height} wanted={wanted} min={min_rows}: {e}"
                    ))
                })?;
            }
        }
        Ok(())
    }

    #[test]
    fn degenerate_areas_have_only_a_chat() -> TestResult {
        for (width, height) in [(0u16, 0u16), (0, 50), (50, 0)] {
            let inp = input(width, height, 4, 0);
            let layout = chat_first(inp);
            if layout.cards.is_some()
                || layout.finished.is_some()
                || layout.live.is_some()
                || layout.live_mode != LiveMode::Hidden
                || layout.chat != inp.area
            {
                return Err(fail(&format!("unexpected layout for {width}x{height}")));
            }
        }
        Ok(())
    }

    #[test]
    fn collapse_is_monotonic_as_height_shrinks() -> TestResult {
        let mut previous: Option<(bool, bool, bool, u16)> = None;
        for height in (0..=80u16).rev() {
            let inp = input(48, height, 4, 0);
            let layout = chat_first(inp);
            assert_partition(&layout, inp.area, MINI_LIVE_MIN_CHAT_ROWS)?;
            let lines = match layout.live_mode {
                LiveMode::Body { lines } => lines,
                LiveMode::Header | LiveMode::Hidden => 0,
            };
            let key = (
                layout.cards.is_some(),
                layout.finished.is_some(),
                layout.live.is_some(),
                lines,
            );
            if lines > 0 && !key.2 {
                return Err(fail("body without header"));
            }
            if key.2 && !key.1 {
                return Err(fail("live without finished strip"));
            }
            if key.1 && !key.0 {
                return Err(fail("finished strip without cards"));
            }
            if previous.is_some_and(|prev| key > prev) {
                return Err(fail(&format!(
                    "height {height}: {key:?} grew compared to {previous:?}"
                )));
            }
            previous = Some(key);
        }
        Ok(())
    }

    #[test]
    fn collapse_order_is_body_header_finished_cards() -> TestResult {
        // Floor 8: Budget = height - 8.
        let full = chat_first(input(48, 8 + 4 + 1 + 1 + 4, 4, 0));
        if full.live_mode != (LiveMode::Body { lines: 4 }) || full.cards.is_none() {
            return Err(fail("expected everything at budget 10"));
        }
        let no_body = chat_first(input(48, 8 + 4 + 1 + 1, 4, 0));
        if no_body.live_mode != LiveMode::Header || no_body.finished.is_none() {
            return Err(fail("expected header only at budget 6"));
        }
        let no_live = chat_first(input(48, 8 + 4 + 1, 4, 0));
        if no_live.live.is_some() || no_live.finished.is_none() {
            return Err(fail("expected finished without live at budget 5"));
        }
        let no_finished = chat_first(input(48, 8 + 4, 4, 0));
        if no_finished.finished.is_some() || no_finished.cards.is_none() {
            return Err(fail("expected cards only at budget 4"));
        }
        let nothing = chat_first(input(48, 8 + 3, 4, 0));
        if nothing.cards.is_some() || nothing.chat.height != 11 {
            return Err(fail("expected chat only at budget 3"));
        }
        Ok(())
    }

    #[test]
    fn narrow_width_combines_and_wide_width_splits() -> TestResult {
        match chat_first(input(67, 40, 4, 0)).cards {
            Some(DockSplit::Combined(_)) => {}
            other => return Err(fail(&format!("expected Combined at 67, got {other:?}"))),
        }
        match chat_first(input(68, 40, 4, 0)).cards {
            Some(DockSplit::Split { .. }) => Ok(()),
            other => Err(fail(&format!("expected Split at 68, got {other:?}"))),
        }
    }

    #[test]
    fn wanted_body_lines_are_clamped_to_the_maximum() -> TestResult {
        let layout = chat_first(input(80, 40, 99, 0));
        if layout.live_mode != (LiveMode::Body { lines: 4 }) {
            return Err(fail(&format!(
                "expected 4 body lines, got {:?}",
                layout.live_mode
            )));
        }
        Ok(())
    }

    #[test]
    fn zero_wanted_body_lines_give_at_most_a_header() -> TestResult {
        for height in 0..=60u16 {
            let layout = chat_first(input(80, height, 0, 0));
            match layout.live_mode {
                LiveMode::Hidden | LiveMode::Header => {}
                LiveMode::Body { .. } => {
                    return Err(fail(&format!("body shown at height {height}")));
                }
            }
        }
        let layout = chat_first(input(80, 40, 0, 0));
        if layout.live_mode != LiveMode::Header {
            return Err(fail("expected a header at height 40"));
        }
        Ok(())
    }

    #[test]
    fn larger_min_chat_rows_shrink_the_optional_blocks() -> TestResult {
        let layout = chat_first(input(80, 40, 4, 40));
        if layout.cards.is_some() || layout.chat.height != 40 {
            return Err(fail(
                "chat must take the whole area when the floor equals it",
            ));
        }
        Ok(())
    }
}
