//! Reine Textprojektion der Recency-Slots (h7), ohne ratatui.
//!
//! Slot 1 (neueste Agentenmeldung) steht vor Slot 2 (die davor). Jeder belegte
//! Slot wird zu einer Zeile `"{slot} {kennung}: {text}"` aufbereitet. Rolle,
//! Child und Text stammen von Agenten und gelten als nicht vertrauenswürdig,
//! daher laufen sie durch [`sanitize_inline`]. Danach wird die Zeile auf eine
//! Anzeigebreite in Spalten gekürzt und bei Kürzung mit `…` abgeschlossen.
//!
//! # Kurzkennung statt Sitzungs-UUID
//! Im Betrieb ist `RecencyEntry::child` die 36 Zeichen lange Sitzungs-UUID des
//! meldenden Kindes. Würde sie vollständig angezeigt, bliebe in einem schmalen
//! Streifen (38 Spalten bei 40×60, 44 bei 46×78) kaum Platz für die Meldung
//! selbst. Als Kennung dient deshalb die Rolle des Kindes
//! (`RecencyEntry::role`, höchstens [`ROLE_MAX_COLS`] Spalten), solange sie
//! nicht leer ist, sonst die ersten [`ID_MAX_COLS`] Spalten der Sitzungs-ID.
//! Die Kennung wird ohne Auslassungszeichen auf ihre Höchstbreite geschnitten.
//! Reicht die Breite nicht, um der Meldung mindestens [`MIN_TEXT_COLS`]
//! Spalten zu lassen, schrumpft zuerst die Kennung (bis auf [`ID_MIN_COLS`]
//! Spalten) und erst danach wird der Text gekürzt.
//!
//! # Anzeigebreite
//! Gemessen wird immer die fertige Zeichenfolge mit [`UnicodeWidthStr`], nicht
//! die Summe der Zeichenbreiten: Präsentationssequenzen (`❤` + U+FE0F), Keycaps
//! und ZWJ-Sequenzen sind als Ganzes breiter oder schmaler als ihre Teile.
//! Geschnitten wird nur an Graphem-Grenzen.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::recency::{RecencyEntry, RecencySlots};
use crate::sanitize::sanitize_inline;

/// Kennzeichen für eine gekürzte Zeile.
const ELLIPSIS: &str = "…";

/// Höchstbreite der Kurzkennung in Spalten, wenn sie aus der Sitzungs-ID stammt
/// (deren erste Zeichen).
const ID_MAX_COLS: usize = 8;

/// Höchstbreite der Kurzkennung in Spalten, wenn sie die Rolle des Kindes ist.
/// Rollen sind lesbare Namen (`uia-writer`) und bekommen deshalb mehr Platz als
/// eine abgekürzte UUID.
const ROLE_MAX_COLS: usize = 12;

/// Mindestbreite der Kurzkennung: darunter wird nicht mehr die Kennung, sondern
/// der Text gekürzt.
const ID_MIN_COLS: usize = 4;

/// Spalten (inklusive `…`), die der Meldung mindestens bleiben, solange die
/// Zeile neben Slot-Nummer und kürzester Kennung überhaupt so viel Platz hat.
const MIN_TEXT_COLS: usize = 12;

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
/// `max_width` Spalten breit (gemessen mit [`UnicodeWidthStr`]); gekürzte Zeilen
/// enden mit `…`.
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
///
/// # Beschreibung
/// Die Kennung ([`label_source`]: Rolle mit höchstens [`ROLE_MAX_COLS`] Spalten,
/// sonst Sitzungs-ID mit höchstens [`ID_MAX_COLS`]) wird ohne Auslassungszeichen
/// geschnitten. Bleiben der Meldung dabei weniger als
/// `min(Textbreite, MIN_TEXT_COLS)` Spalten, schrumpft die Kennung bis auf
/// [`ID_MIN_COLS`]; was dann noch fehlt, geht zulasten des Textes (Kürzung mit
/// `…`). Ist die Zeile so schmal, dass schon Slot-Nummer und Kennung nicht
/// passen, wird die ganze Zeile gekürzt.
fn slot_text(slot: u8, entry: &RecencyEntry, max_width: u16) -> String {
    let max = usize::from(max_width);
    let (source, label_max_cols) = label_source(entry);
    let text = sanitize_inline(&entry.text);
    let prefix_of = |id: &str| format!("{slot} {id}: ");

    // Platz für die Kennung: Zeilenbreite ohne feste Zeichen (Slot, Trenner)
    // und ohne die Spalten, die die Meldung mindestens bekommen soll.
    let fixed_cols = display_width(&prefix_of(""));
    let text_cols_wanted = display_width(&text).min(MIN_TEXT_COLS);
    let id_room = max
        .saturating_sub(fixed_cols)
        .saturating_sub(text_cols_wanted);
    let label = clip_to_width(&source, id_room.clamp(ID_MIN_COLS, label_max_cols));

    let prefix = prefix_of(label.as_str());
    let prefix_cols = display_width(&prefix);
    let line = if prefix_cols < max {
        let text = truncate_to_width(&text, max.saturating_sub(prefix_cols));
        format!("{prefix}{text}")
    } else {
        format!("{prefix}{text}")
    };
    // Letzte Absicherung: die zusammengesetzte Zeile wird als Ganzes gemessen
    // (ein Zeichen an der Nahtstelle von Präfix und Text könnte sonst anders
    // breit sein als beide Teile einzeln). Passt sie, bleibt sie unverändert.
    truncate_to_width(&line, max)
}

/// Quelle der Kennung samt ihrer Höchstbreite: die bereinigte Rolle des Kindes
/// ([`ROLE_MAX_COLS`]), falls sie nach dem Bereinigen und Trimmen nicht leer
/// ist, sonst die bereinigte Sitzungs-ID ([`ID_MAX_COLS`]).
fn label_source(entry: &RecencyEntry) -> (String, usize) {
    let role = sanitize_inline(&entry.role);
    let role = role.trim();
    if role.is_empty() {
        (sanitize_inline(&entry.child), ID_MAX_COLS)
    } else {
        (role.to_owned(), ROLE_MAX_COLS)
    }
}

/// Kürzt `text` auf höchstens `max` Spalten und hängt bei Kürzung `…` an.
///
/// # Beschreibung
/// Geschnitten wird nur an Graphem-Grenzen, nie innerhalb eines Graphems
/// (Emoji-Sequenz, Zeichen mit Kombinationszeichen). Ein breites Graphem (CJK,
/// Breite 2), das nicht mehr in das Budget passt, wird samt Rest weggelassen.
/// Die Breite der Ausgabe wird als fertige Zeichenfolge gemessen; sie ist nie
/// größer als `max`. Passt nicht einmal `…` in `max`, ist die Ausgabe leer.
fn truncate_to_width(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_owned();
    }
    let ellipsis_width = display_width(ELLIPSIS);
    if ellipsis_width > max {
        return String::new();
    }
    let mut kept = clip_to_width(text, max.saturating_sub(ellipsis_width));
    loop {
        let candidate = format!("{kept}{ELLIPSIS}");
        if display_width(&candidate) <= max {
            return candidate;
        }
        let Some(cut) = last_grapheme_start(&kept) else {
            return String::new();
        };
        kept.truncate(cut);
    }
}

/// Schneidet `text` ohne Auslassungszeichen auf höchstens `max` Spalten.
///
/// # Beschreibung
/// Graphem für Graphem wird übernommen, solange die Summe der Graphembreiten
/// passt. Weil die Breite einer Folge nicht in jedem Fall die Summe ihrer Teile
/// ist, wird das Ergebnis anschließend als Ganzes gemessen und notfalls um
/// weitere Grapheme am Ende gekürzt.
fn clip_to_width(text: &str, max: usize) -> String {
    let mut kept = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        let width = display_width(grapheme);
        if used.saturating_add(width) > max {
            break;
        }
        kept.push_str(grapheme);
        used = used.saturating_add(width);
    }
    while display_width(&kept) > max {
        let Some(cut) = last_grapheme_start(&kept) else {
            break;
        };
        kept.truncate(cut);
    }
    kept
}

/// Byte-Offset, an dem das letzte Graphem von `text` beginnt; `None` bei leerem Text.
fn last_grapheme_start(text: &str) -> Option<usize> {
    text.grapheme_indices(true)
        .next_back()
        .map(|(index, _)| index)
}

/// Anzeigebreite von `text` in Spalten, als Ganzes gemessen.
fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::{MIN_TEXT_COLS, SlotLine, slot_lines, truncate_to_width};
    use crate::recency::{RecencyEntry, RecencySlots};
    use crate::test_support::{TestError, TestResult};

    /// Sitzungs-ID im Produktionsformat (UUID v4, 36 Zeichen).
    const SESSION_ID: &str = "0196f3a2-7b1e-4c8d-9a35-5e2f8d41b6c7";

    /// Eine zweite, anders beginnende Sitzungs-ID.
    const OTHER_SESSION_ID: &str = "c2d84e10-3f6a-4b97-8e12-a47d90c5f3b1";

    /// Eine lange Meldung, die in jedem getesteten Streifen gekürzt werden muss.
    const LONG_TEXT: &str = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz";

    /// Familien-Emoji als ZWJ-Sequenz (Breite 2 als Ganzes, 6 als Zeichensumme).
    const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";

    /// Eintrag ohne Rolle: die Kennung stammt dann aus der Sitzungs-ID.
    fn entry(child: &str, text: &str) -> RecencyEntry {
        entry_with_role("", child, text)
    }

    fn entry_with_role(role: &str, child: &str, text: &str) -> RecencyEntry {
        RecencyEntry {
            child: child.to_owned(),
            role: role.to_owned(),
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

    /// Unabhängiges Messorakel: Breite der ganzen Zeichenfolge, keine Zeichensumme.
    fn cols(text: &str) -> usize {
        UnicodeWidthStr::width(text)
    }

    /// Die Slot-1-Zeile bei Breite `width` für ein Kind ohne Rolle.
    fn slot_1_line(child: &str, text: &str, width: u16) -> TestResult<String> {
        slot_1_line_with_role("", child, text, width)
    }

    /// Die Slot-1-Zeile bei Breite `width` für ein Kind mit Rolle.
    fn slot_1_line_with_role(
        role: &str,
        child: &str,
        text: &str,
        width: u16,
    ) -> TestResult<String> {
        let mut slots = RecencySlots::new();
        slots.push(entry_with_role(role, child, text));
        slot_lines(&slots, width)
            .into_iter()
            .next()
            .map(|line| line.text)
            .ok_or(TestError::Missing("Slot-1-Zeile"))
    }

    /// Spalten des Meldungsteils hinter `"<slot> <kennung>: "`.
    fn message_cols(line: &str) -> TestResult<usize> {
        let (_, message) = line
            .split_once(": ")
            .ok_or(TestError::Missing("Trenner nach der Kennung"))?;
        Ok(cols(message))
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
                    cols(&line.text) <= usize::from(max),
                    "Breite {} überschreitet {max}: {:?}",
                    cols(&line.text),
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

    #[test]
    fn session_id_is_shown_as_eight_char_short_id_when_space_allows() -> TestResult {
        let line = slot_1_line(SESSION_ID, "Migration fertig", 80)?;
        assert_eq!(line, "1 0196f3a2: Migration fertig");
        assert!(!line.contains(SESSION_ID));
        Ok(())
    }

    /// 40×60 hat 38 Spalten Budget für die Zeile: die UUID darf die Meldung nicht
    /// verdrängen (Finding P2).
    #[test]
    fn real_session_id_leaves_message_room_in_38_columns() -> TestResult {
        let line = slot_1_line(SESSION_ID, LONG_TEXT, 38)?;
        assert!(line.starts_with("1 0196f3a2: "), "Zeile: {line:?}");
        assert!(!line.contains(SESSION_ID));
        assert!(cols(&line) <= 38, "Zeile zu breit: {line:?}");
        // 38 - "1 0196f3a2: " (12) = 26 Spalten für die Meldung inklusive `…`.
        assert_eq!(message_cols(&line)?, 26, "Zeile: {line:?}");
        assert!(message_cols(&line)? >= MIN_TEXT_COLS);
        Ok(())
    }

    /// 46×78 hat 44 Spalten Budget; früher blieben bei einer langen Meldung nur
    /// drei Textzeichen plus `…` übrig.
    #[test]
    fn real_session_id_leaves_message_room_in_44_columns() -> TestResult {
        let line = slot_1_line(SESSION_ID, LONG_TEXT, 44)?;
        assert!(line.starts_with("1 0196f3a2: "), "Zeile: {line:?}");
        assert!(cols(&line) <= 44, "Zeile zu breit: {line:?}");
        assert_eq!(message_cols(&line)?, 32, "Zeile: {line:?}");
        assert!(line.ends_with('…'));
        Ok(())
    }

    #[test]
    fn short_id_shrinks_before_the_message_is_clipped() -> TestResult {
        // 20 Spalten: 4 feste Zeichen + 4 Kennungsspalten + 12 Meldungsspalten.
        assert_eq!(
            slot_1_line(SESSION_ID, LONG_TEXT, 20)?,
            "1 0196: abcdefghijk…"
        );
        // 22 Spalten: die Kennung darf bis auf 6 Spalten wachsen.
        assert_eq!(
            slot_1_line(SESSION_ID, LONG_TEXT, 22)?,
            "1 0196f3: abcdefghijk…"
        );
        Ok(())
    }

    #[test]
    fn short_message_does_not_shrink_the_id() -> TestResult {
        // Die Meldung braucht nur 2 Spalten, also bleibt die Kennung bei 8.
        assert_eq!(slot_1_line(SESSION_ID, "ok", 16)?, "1 0196f3a2: ok");
        Ok(())
    }

    #[test]
    fn message_gets_min_text_cols_for_every_width_from_20_up() -> TestResult {
        for width in 20u16..=60 {
            let line = slot_1_line(SESSION_ID, LONG_TEXT, width)?;
            assert!(
                cols(&line) <= usize::from(width),
                "Breite {} überschreitet {width}: {line:?}",
                cols(&line)
            );
            assert!(
                message_cols(&line)? >= MIN_TEXT_COLS,
                "Meldung zu kurz bei {width}: {line:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn narrow_widths_below_the_minimum_still_respect_the_budget() -> TestResult {
        for width in 1u16..20 {
            let line = slot_1_line(SESSION_ID, LONG_TEXT, width)?;
            assert!(
                cols(&line) <= usize::from(width),
                "Breite {} überschreitet {width}: {line:?}",
                cols(&line)
            );
        }
        Ok(())
    }

    #[test]
    fn both_slots_use_their_own_short_id() {
        let mut slots = RecencySlots::new();
        slots.push(entry(OTHER_SESSION_ID, "erste"));
        slots.push(entry(SESSION_ID, "zweite"));
        let lines = slot_lines(&slots, 44);
        assert_eq!(
            pairs(&lines),
            [(1, "1 0196f3a2: zweite"), (2, "2 c2d84e10: erste")]
        );
    }

    #[test]
    fn id_is_clipped_at_a_grapheme_boundary_by_columns() -> TestResult {
        // 12 Spalten breite Kennung, auf 8 Spalten gekürzt: vier CJK-Zeichen.
        assert_eq!(
            slot_1_line("日本語日本語", LONG_TEXT, 30)?,
            "1 日本語日: abcdefghijklmnopq…"
        );
        // Ein langer Rollenname wird wie eine UUID auf 8 Zeichen gekürzt.
        assert_eq!(
            slot_1_line("root-orchestrator", "Fertig", 44)?,
            "1 root-orc: Fertig"
        );
        Ok(())
    }

    #[test]
    fn role_is_preferred_over_the_session_id() -> TestResult {
        assert_eq!(
            slot_1_line_with_role("writer", SESSION_ID, "Migration fertig", 80)?,
            "1 writer: Migration fertig"
        );
        // Rollen bis 12 Spalten bleiben ganz; längere werden ohne `…` geschnitten.
        assert_eq!(
            slot_1_line_with_role("uia-writer", SESSION_ID, "fertig", 44)?,
            "1 uia-writer: fertig"
        );
        assert_eq!(
            slot_1_line_with_role("root-orchestrator", SESSION_ID, "fertig", 44)?,
            "1 root-orchest: fertig"
        );
        Ok(())
    }

    #[test]
    fn empty_or_blank_role_falls_back_to_the_short_session_id() -> TestResult {
        for role in ["", "   ", " \n\t "] {
            assert_eq!(
                slot_1_line_with_role(role, SESSION_ID, "fertig", 44)?,
                "1 0196f3a2: fertig",
                "Rolle {role:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn role_keeps_message_room_in_38_and_44_columns() -> TestResult {
        // "1 uia-writer: " belegt 14 Spalten.
        let narrow = slot_1_line_with_role("uia-writer", SESSION_ID, LONG_TEXT, 38)?;
        assert!(narrow.starts_with("1 uia-writer: "), "Zeile: {narrow:?}");
        assert!(!narrow.contains(SESSION_ID));
        assert_eq!(cols(&narrow), 38, "Zeile: {narrow:?}");
        assert_eq!(message_cols(&narrow)?, 24, "Zeile: {narrow:?}");
        let portrait = slot_1_line_with_role("uia-writer", SESSION_ID, LONG_TEXT, 44)?;
        assert_eq!(cols(&portrait), 44, "Zeile: {portrait:?}");
        assert_eq!(message_cols(&portrait)?, 30, "Zeile: {portrait:?}");
        // Eine überlange Rolle wird auf 12 Spalten geschnitten:
        // "1 root-orchest: " belegt 16 Spalten, der Meldung bleiben 22 bzw. 28.
        let long_narrow = slot_1_line_with_role("root-orchestrator", SESSION_ID, LONG_TEXT, 38)?;
        assert!(
            long_narrow.starts_with("1 root-orchest: "),
            "Zeile: {long_narrow:?}"
        );
        assert_eq!(cols(&long_narrow), 38, "Zeile: {long_narrow:?}");
        assert_eq!(message_cols(&long_narrow)?, 22, "Zeile: {long_narrow:?}");
        let long_portrait = slot_1_line_with_role("root-orchestrator", SESSION_ID, LONG_TEXT, 44)?;
        assert_eq!(cols(&long_portrait), 44, "Zeile: {long_portrait:?}");
        assert_eq!(
            message_cols(&long_portrait)?,
            28,
            "Zeile: {long_portrait:?}"
        );
        Ok(())
    }

    #[test]
    fn role_shrinks_before_the_message_is_clipped() -> TestResult {
        // 20 Spalten: 4 feste Zeichen + 4 Kennungsspalten + 12 Meldungsspalten.
        assert_eq!(
            slot_1_line_with_role("reviewer", SESSION_ID, LONG_TEXT, 20)?,
            "1 revi: abcdefghijk…"
        );
        // 24 Spalten: die Rolle darf auf 8, bei 28 auf ihre volle Breite wachsen.
        assert_eq!(
            slot_1_line_with_role("uia-writer", SESSION_ID, LONG_TEXT, 24)?,
            "1 uia-writ: abcdefghijk…"
        );
        assert_eq!(
            slot_1_line_with_role("uia-writer", SESSION_ID, LONG_TEXT, 28)?,
            "1 uia-writer: abcdefghijklm…"
        );
        for width in 20u16..=60 {
            let line = slot_1_line_with_role("uia-writer", SESSION_ID, LONG_TEXT, width)?;
            assert!(
                cols(&line) <= usize::from(width),
                "Breite {} überschreitet {width}: {line:?}",
                cols(&line)
            );
            assert!(
                message_cols(&line)? >= MIN_TEXT_COLS,
                "Meldung zu kurz bei {width}: {line:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn role_is_sanitized_and_never_leaks_control_characters() -> TestResult {
        let line = slot_1_line_with_role("ab\u{1b}]52;c;AA==\u{07}", SESSION_ID, "x", 80)?;
        assert_eq!(line, "1 ab⟨ESC⟩: x");
        assert!(!line.contains('\u{1b}') && !line.contains('\u{07}'));
        Ok(())
    }

    #[test]
    fn both_slots_prefer_their_own_role() {
        let mut slots = RecencySlots::new();
        slots.push(entry_with_role("writer", OTHER_SESSION_ID, "erste"));
        slots.push(entry_with_role("", SESSION_ID, "zweite"));
        let lines = slot_lines(&slots, 44);
        assert_eq!(
            pairs(&lines),
            [(1, "1 0196f3a2: zweite"), (2, "2 writer: erste")]
        );
    }

    /// Finding P3: `❤` + U+FE0F ist als Ganzes 2 Spalten breit, die Zeichensumme
    /// ergäbe 1. Die Zeile `"1 a: ❤️"` ist 7 Spalten breit und passt nicht in 6.
    #[test]
    fn presentation_sequence_is_measured_as_a_whole() -> TestResult {
        let heart = "\u{2764}\u{FE0F}";
        assert_eq!(cols(&format!("1 a: {heart}")), 7);
        assert_eq!(slot_1_line("a", heart, 6)?, "1 a: …");
        assert_eq!(slot_1_line("a", heart, 7)?, format!("1 a: {heart}"));
        Ok(())
    }

    #[test]
    fn presentation_sequence_with_real_session_id_stays_inside_the_strip() -> TestResult {
        let text = "xxx\u{2764}\u{FE0F}";
        for width in 1u16..=44 {
            let line = slot_1_line(SESSION_ID, text, width)?;
            assert!(
                cols(&line) <= usize::from(width),
                "Breite {} überschreitet {width}: {line:?}",
                cols(&line)
            );
        }
        Ok(())
    }

    #[test]
    fn complex_graphemes_never_exceed_the_budget() -> TestResult {
        let samples = [
            "\u{2764}\u{FE0F}".repeat(30),
            "xxx\u{2764}\u{FE0F}".to_owned(),
            "1\u{FE0F}\u{20E3}2\u{FE0F}\u{20E3}3\u{FE0F}\u{20E3}abcdefghijkl".to_owned(),
            "\u{1F1E9}\u{1F1EA}\u{1F1EB}\u{1F1F7}\u{1F1EF}\u{1F1F5}abcdefghijkl".to_owned(),
            "e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}".to_owned(),
            "\u{1F44D}\u{1F3FD}\u{1F44D}\u{1F3FD}\u{1F44D}\u{1F3FD}abcdefghijkl".to_owned(),
            format!("ab{FAMILY}cd{FAMILY}{FAMILY}{FAMILY}"),
            "日本語日本語日本語日本語日本語日本語".to_owned(),
        ];
        let roles = [
            String::new(),
            "uia-writer".to_owned(),
            "日本語日本語".to_owned(),
            "\u{2764}\u{FE0F}".repeat(8),
            format!("ab{FAMILY}{FAMILY}{FAMILY}{FAMILY}"),
        ];
        for text in &samples {
            for role in &roles {
                for child in ["a", SESSION_ID, "日本語日本語"] {
                    for width in 1u16..=60 {
                        let line = slot_1_line_with_role(role, child, text, width)?;
                        assert!(
                            cols(&line) <= usize::from(width),
                            "Breite {} überschreitet {width}: {line:?} (Rolle {role:?}, Kind {child:?}, Text {text:?})",
                            cols(&line)
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// `sanitize_inline` markiert U+200D sichtbar; das Kürzen selbst muss aber
    /// auch echte ZWJ-Sequenzen als ein Graphem behandeln und nie zerschneiden.
    #[test]
    fn zwj_sequence_is_clipped_at_grapheme_boundary() {
        assert_eq!(cols(FAMILY), 2);
        let text = format!("ab{FAMILY}cd");
        assert_eq!(truncate_to_width(&text, 6), text);
        assert_eq!(truncate_to_width(&text, 5), format!("ab{FAMILY}…"));
        assert_eq!(truncate_to_width(&text, 4), "ab…");
        for max in 0usize..=10 {
            let out = truncate_to_width(&text, max);
            assert!(
                cols(&out) <= max,
                "Breite {} überschreitet {max}: {out:?}",
                cols(&out)
            );
            assert!(
                !out.contains('\u{200D}') || out.contains(FAMILY),
                "ZWJ-Sequenz zerschnitten bei {max}: {out:?}"
            );
        }
    }

    #[test]
    fn truncate_leaves_fitting_text_untouched_and_handles_tiny_budgets() {
        assert_eq!(truncate_to_width("abc", 3), "abc");
        assert_eq!(truncate_to_width("abcd", 3), "ab…");
        assert_eq!(truncate_to_width("abcd", 1), "…");
        assert_eq!(truncate_to_width("abcd", 0), "");
        assert_eq!(truncate_to_width("", 0), "");
    }
}
