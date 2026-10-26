#![allow(dead_code, reason = "h7: slot renderer not mounted in app.rs yet")]

//! Reine Textprojektion der Recency-Slots (h7), ohne ratatui.
//!
//! Slot 1 (neueste Agentenmeldung) steht vor Slot 2 (die davor). Jeder belegte
//! Slot wird zu einer Zeile `"{slot} {child}: {text}"` aufbereitet. Child und
//! Text stammen von Agenten und gelten als nicht vertrauenswürdig, daher laufen
//! beide durch [`sanitize_inline`]. Danach wird die Zeile auf eine Anzeigebreite
//! in Spalten gekürzt und mit `…` abgeschlossen.

use unicode_width::UnicodeWidthChar;

use crate::recency::{RecencyEntry, RecencySlots};
use crate::sanitize::sanitize_inline;

/// Kennzeichen für eine gekürzte Zeile.
const ELLIPSIS: char = '…';

/// Eine aufbereitete Zeile eines Recency-Slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotLine {
    /// Slot-Nummer: `1` für die neueste, `2` für die vorherige Meldung.
    pub(crate) slot: u8,
    /// Terminal-sicherer, auf die Breite gekürzter Anzeigetext.
    pub(crate) text: String,
}

/// Wandelt die belegten Recency-Slots in Textzeilen um, neueste zuerst.
///
/// # Argumente
/// - `slots`: Die Slots mit den letzten zwei Agentenmeldungen.
/// - `max_width`: Maximale Anzeigebreite in Spalten. `0` liefert keine Zeilen.
///
/// # Rückgabe
/// Zeilen für Slot 1 und Slot 2, nur für belegte Slots. Jede Zeile ist höchstens
/// `max_width` Spalten breit; gekürzte Zeilen enden mit `…`.
#[must_use]
pub(crate) fn slot_lines(slots: &RecencySlots, max_width: u16) -> Vec<SlotLine> {
    if max_width == 0 {
        return Vec::new();
    }
    [(1u8, slots.slot1()), (2u8, slots.slot2())]
        .into_iter()
        .filter_map(|(slot, entry)| {
            entry.map(|entry| SlotLine {
                slot,
                text: slot_text(slot, entry, max_width),
            })
        })
        .collect()
}

/// Baut den Anzeigetext eines Slots und kürzt ihn auf `max_width` Spalten.
fn slot_text(slot: u8, entry: &RecencyEntry, max_width: u16) -> String {
    let raw = format!(
        "{slot} {}: {}",
        sanitize_inline(&entry.child),
        sanitize_inline(&entry.text)
    );
    truncate_to_width(&raw, usize::from(max_width))
}

/// Kürzt `text` auf höchstens `max` Spalten und hängt bei Kürzung `…` an.
///
/// # Beschreibung
/// Geschnitten wird nur zwischen Zeichen, nie innerhalb eines Zeichens. Ein
/// breites Zeichen (CJK, Breite 2), das nicht mehr in das Budget passt, wird
/// samt Rest weggelassen, damit die Gesamtbreite nie `max` überschreitet.
fn truncate_to_width(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_owned();
    }
    let ellipsis_width = UnicodeWidthChar::width(ELLIPSIS).unwrap_or(1);
    let budget = max.saturating_sub(ellipsis_width);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used.saturating_add(ch_width) > budget {
            break;
        }
        out.push(ch);
        used = used.saturating_add(ch_width);
    }
    out.push(ELLIPSIS);
    out
}

/// Anzeigebreite von `text` in Spalten.
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .fold(0usize, usize::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::{SlotLine, display_width, slot_lines};
    use crate::recency::{RecencyEntry, RecencySlots};

    fn entry(child: &str, text: &str) -> RecencyEntry {
        RecencyEntry {
            child: child.to_owned(),
            text: text.to_owned(),
        }
    }

    /// Slot-Nummer und Text je Zeile, ohne Indizierung.
    fn pairs(lines: &[SlotLine]) -> Vec<(u8, &str)> {
        lines
            .iter()
            .map(|line| (line.slot, line.text.as_str()))
            .collect()
    }

    #[test]
    fn empty_slots_render_nothing() {
        let slots = RecencySlots::new();
        assert!(slot_lines(&slots, 80).is_empty());
    }

    #[test]
    fn one_push_fills_slot_1_only() {
        let mut slots = RecencySlots::new();
        slots.push(entry("agent", "A"));
        let lines = slot_lines(&slots, 80);
        assert_eq!(pairs(&lines), [(1, "1 agent: A")]);
    }

    #[test]
    fn two_pushes_put_newest_in_slot_1() {
        let mut slots = RecencySlots::new();
        slots.push(entry("agent", "A"));
        slots.push(entry("agent", "B"));
        let lines = slot_lines(&slots, 80);
        assert_eq!(pairs(&lines), [(1, "1 agent: B"), (2, "2 agent: A")]);
    }

    #[test]
    fn escape_sequence_is_sanitized() {
        let mut slots = RecencySlots::new();
        slots.push(entry("agent", "\u{1b}]52;c;AA==\u{07}x"));
        let lines = slot_lines(&slots, 80);
        assert_eq!(pairs(&lines), [(1, "1 agent: ⟨ESC⟩x")]);
        assert!(lines.iter().all(|line| !line.text.contains('\u{1b}')));
    }

    #[test]
    fn cjk_truncation_never_exceeds_width() {
        let mut slots = RecencySlots::new();
        slots.push(entry("a", "日本語日本語日本語"));
        // Budget für Text vor `…`: 10 - 1 = 9 Spalten. "1 a: " belegt 5, dann
        // passt "日" (2) und "本" (2) bis 9, "語" (2) nicht mehr.
        let lines = slot_lines(&slots, 10);
        assert_eq!(pairs(&lines), [(1, "1 a: 日本…")]);
        for max in 1u16..=24 {
            for line in slot_lines(&slots, max) {
                assert!(
                    display_width(&line.text) <= usize::from(max),
                    "Breite {} überschreitet {max}: {:?}",
                    display_width(&line.text),
                    line.text
                );
            }
        }
    }

    #[test]
    fn width_zero_renders_nothing() {
        let mut slots = RecencySlots::new();
        slots.push(entry("agent", "A"));
        assert!(slot_lines(&slots, 0).is_empty());
    }
}
