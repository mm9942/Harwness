//! Bausteine der Statuszeile: Modus/Freigabe und aktives Modell.
//!
//! Reine Textfunktionen ohne Zustand; `app.rs` setzt die Segmente zur
//! eigentlichen Statuszeile zusammen.

use harw_core::InteractionMode;
use harw_extension_api::approval_mode::ApprovalMode;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Platzhalter für einen unbekannten Wert.
const UNKNOWN: &str = "–";
/// Auslassungszeichen beim Kürzen.
const ELLIPSIS: char = '…';

/// Segment `Modus: x · Freigabe: y` für die Statuszeile.
///
/// # Argumente
/// - `mode`: aktiver Interaktionsmodus (kanonischer Kurzname, z. B. `plan`).
/// - `approval`: aktiver Freigabemodus (`ask`/`auto`/`full`); `None` → `–`.
/// - `pending`: eine Moduswechsel-Anfrage wartet noch (z. B. bis Turn-Ende);
///   hängt ` (ausstehend)` an.
#[must_use]
pub(crate) fn mode_segment(
    mode: InteractionMode,
    approval: Option<ApprovalMode>,
    pending: bool,
) -> String {
    let approval = approval.map_or(UNKNOWN, ApprovalMode::as_str);
    let mut text = format!("Modus: {} · Freigabe: {approval}", mode.as_str());
    if pending {
        text.push_str(" (ausstehend)");
    }
    text
}

/// Kurzform von [`mode_segment`] für schmale Fenster: `chat · full`.
#[must_use]
pub(crate) fn mode_segment_short(
    mode: InteractionMode,
    approval: Option<ApprovalMode>,
    pending: bool,
) -> String {
    let approval = approval.map_or(UNKNOWN, ApprovalMode::as_str);
    let mut text = format!("{} · {approval}", mode.as_str());
    if pending {
        text.push_str(" (ausst.)");
    }
    text
}

/// Segment mit dem aktiven Modell, höchstens `max_chars` Zeichen lang.
///
/// # Beschreibung
/// Vollform `provider/model`; ohne Anbieter nur `model`, ohne Modell
/// `kein Modell`. Passt die Vollform nicht, wird zuerst der Anbieter
/// weggelassen; passt auch das Modell nicht, wird es zeichensicher (nach
/// Unicode-Skalaren, nie mitten in einem UTF-8-Zeichen) gekürzt und mit `…`
/// abgeschlossen. `max_chars == 0` liefert einen leeren Text.
#[must_use]
pub(crate) fn model_segment(
    provider: Option<&str>,
    model: Option<&str>,
    max_chars: usize,
) -> String {
    let provider = provider.map(str::trim).filter(|value| !value.is_empty());
    let model = model.map(str::trim).filter(|value| !value.is_empty());
    let full = match (provider, model) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        (None, Some(model)) => model.to_owned(),
        (_, None) => "kein Modell".to_owned(),
    };
    if full.chars().count() <= max_chars {
        return full;
    }
    let fallback = model.unwrap_or(full.as_str());
    shorten(fallback, max_chars)
}

/// Kürzt `text` auf höchstens `max_chars` Zeichen inklusive `…`.
fn shorten(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut shortened: String = text.chars().take(max_chars - 1).collect();
    shortened.push(ELLIPSIS);
    shortened
}

/// Anzeigebreite von `text` in Terminalspalten.
#[must_use]
pub(crate) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Kürzt `text` auf höchstens `max` Spalten Anzeigebreite und schließt mit
/// `…` ab, wenn gekürzt wurde (nie mitten in einem Zeichen).
#[must_use]
pub(crate) fn fit_width(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0);
        if used + width + 1 > max {
            break;
        }
        used += width;
        out.push(ch);
    }
    out.push(ELLIPSIS);
    out
}

/// Ein Abschnitt der Statuszeile mit Kürzungsstufen.
///
/// `variants` geht von der Vollform zur kürzesten Form; eine leere
/// Zeichenkette bedeutet „entfällt“. Je kleiner `priority`, desto früher
/// wird der Abschnitt gekürzt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusSegment {
    /// Kürzungsstufen, lang → kurz.
    pub(crate) variants: Vec<String>,
    /// Vorrang beim Behalten (höher = länger sichtbar).
    pub(crate) priority: u8,
}

impl StatusSegment {
    /// Abschnitt mit den gegebenen Stufen.
    pub(crate) fn new(priority: u8, variants: Vec<String>) -> Self {
        Self { variants, priority }
    }

    /// Abschnitt, der nie gekürzt wird.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn fixed(text: impl Into<String>) -> Self {
        Self {
            variants: vec![text.into()],
            priority: u8::MAX,
        }
    }

    /// Abschnitt, der als Ganzes entfallen darf.
    pub(crate) fn optional(priority: u8, text: impl Into<String>) -> Self {
        Self {
            variants: vec![text.into(), String::new()],
            priority,
        }
    }
}

/// Setzt die Statuszeile aus `segments` zusammen und kürzt nach Vorrang,
/// bis sie in `width` Spalten passt.
///
/// # Beschreibung
/// Solange die Zeile zu breit ist, wird der Abschnitt mit dem kleinsten
/// `priority` (bei Gleichstand der weiter rechts stehende), der noch eine
/// kürzere Stufe hat, eine Stufe gekürzt. Passt es danach noch immer nicht,
/// wird hart mit `…` abgeschnitten — die Zeile wird also nie rechts
/// unkontrolliert abgeschnitten, sondern verliert zuerst Details.
#[must_use]
pub(crate) fn fit_segments(segments: &[StatusSegment], width: usize) -> String {
    let mut levels = vec![0usize; segments.len()];
    let render = |levels: &[usize]| -> String {
        segments
            .iter()
            .zip(levels)
            .filter_map(|(segment, level)| segment.variants.get(*level))
            .map(String::as_str)
            .collect()
    };
    loop {
        let text = render(levels.as_slice());
        if display_width(&text) <= width {
            return text;
        }
        let candidate = segments
            .iter()
            .enumerate()
            .filter(|(index, segment)| levels[*index] + 1 < segment.variants.len())
            .min_by(|(left_index, left), (right_index, right)| {
                left.priority
                    .cmp(&right.priority)
                    .then(right_index.cmp(left_index))
            })
            .map(|(index, _)| index);
        match candidate {
            Some(index) => levels[index] += 1,
            None => return fit_width(&text, width),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_width_counts_display_columns() {
        assert_eq!(fit_width("abc", 3), "abc");
        assert_eq!(fit_width("abcdef", 4), "abc…");
        assert_eq!(fit_width("漢字漢字", 5), "漢字…");
        assert_eq!(fit_width("abc", 0), "");
    }

    #[test]
    fn fit_segments_drops_low_priority_details_first() {
        let segments = vec![
            StatusSegment::fixed(" Shift+Tab: Full Access"),
            StatusSegment::fixed(" | Modus: chat · Freigabe: full"),
            StatusSegment::new(
                200,
                vec![" | openai/gpt-5".to_owned(), " | gpt-5".to_owned()],
            ),
            StatusSegment::new(
                10,
                vec![
                    " | Σ Tokens: 12.3k (in 10.0k, out 2.3k)".to_owned(),
                    " | Σ 12.3k".to_owned(),
                    String::new(),
                ],
            ),
            StatusSegment::optional(20, " | ctx ████░░░░ 50% / 200.0k"),
        ];
        let wide = fit_segments(&segments, 200);
        assert!(wide.contains("Σ Tokens: 12.3k (in"), "{wide}");
        let medium = fit_segments(&segments, 90);
        assert!(display_width(&medium) <= 90, "{medium}");
        assert!(medium.contains("Modus: chat · Freigabe: full"), "{medium}");
        assert!(medium.contains("openai/gpt-5"), "{medium}");
        assert!(
            !medium.contains("(in "),
            "Token-Details fallen zuerst: {medium}"
        );
        let narrow = fit_segments(&segments, 62);
        assert!(display_width(&narrow) <= 62, "{narrow}");
        assert!(narrow.contains("Freigabe: full"), "{narrow}");
        assert!(narrow.contains("gpt-5"), "{narrow}");
        assert!(!narrow.contains("Σ"), "{narrow}");
    }

    #[test]
    fn mode_segment_shows_mode_and_approval() {
        assert_eq!(
            mode_segment(InteractionMode::Plan, Some(ApprovalMode::Delegated), false),
            "Modus: plan · Freigabe: auto"
        );
        assert_eq!(
            mode_segment(InteractionMode::Work, Some(ApprovalMode::FullAccess), true),
            "Modus: work · Freigabe: full (ausstehend)"
        );
        assert_eq!(
            mode_segment(InteractionMode::Chat, None, false),
            "Modus: chat · Freigabe: –"
        );
    }

    #[test]
    fn model_segment_prefers_full_form() {
        assert_eq!(
            model_segment(Some("anthropic"), Some("claude-x"), 40),
            "anthropic/claude-x"
        );
        assert_eq!(model_segment(None, Some("gpt"), 40), "gpt");
        assert_eq!(model_segment(Some("openai"), None, 40), "kein Modell");
        assert_eq!(model_segment(Some(" "), Some(" "), 40), "kein Modell");
    }

    #[test]
    fn model_segment_drops_provider_then_truncates() {
        assert_eq!(
            model_segment(Some("openrouter"), Some("claude-x"), 10),
            "claude-x"
        );
        let short = model_segment(Some("openrouter"), Some("very-long-model-name"), 8);
        assert_eq!(short, "very-lo…");
        assert_eq!(short.chars().count(), 8);
        assert_eq!(model_segment(Some("p"), Some("abc"), 1), "…");
        assert_eq!(model_segment(Some("p"), Some("abc"), 0), "");
    }

    #[test]
    fn model_segment_is_char_safe() {
        let short = model_segment(None, Some("modèll-ßüöä-漢字漢字"), 9);
        assert_eq!(short, "modèll-ß…");
        assert_eq!(short.chars().count(), 9);
    }
}
