//! Bausteine der Statuszeile: Modus/Freigabe und aktives Modell.
//!
//! Reine Textfunktionen ohne Zustand; `app.rs` setzt die Segmente zur
//! eigentlichen Statuszeile zusammen.

use crate::i18n::{tr, tr_args};
use harw_core::InteractionMode;
use harw_extension_api::approval_mode::ApprovalMode;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Platzhalter für einen unbekannten Wert.
const UNKNOWN: &str = "–";
/// Auslassungszeichen beim Kürzen.
const ELLIPSIS: char = '…';

/// Segment für den 30s-Zwischenstand eines Kind-Agenten.
///
/// Verdichtet die von einem Kind-Agenten gemeldeten `parent.message
/// {kind: "info"}`-Zwischenstände (Drossel: `PARENT_INFO_MIN_INTERVAL`,
/// 30 s) zu einem kurzen Statussegment. Der Info-Text wird auf
/// Whitespace normalisiert und auf `max_chars` Zeichen gekürzt (Ellipsis);
/// `None` meldet das Kind ohne aktuelle Meldung im 30s-Fenster.
///
/// # Argumente
/// - `child`: Anzeigename des Kind-Agenten.
/// - `latest_info`: letzter konsolidierter Info-Text oder `None`.
/// - `age_secs`: Alter der Meldung in Sekunden (0 = gerade eben).
/// - `max_chars`: Obergrenze für den erzeugten Text in Zeichen.
#[must_use]
pub(crate) fn child_info_segment(
    child: &str,
    latest_info: Option<&str>,
    age_secs: u64,
    max_chars: usize,
) -> String {
    let age = match age_secs {
        0 => tr("chrome.now", "now").to_owned(),
        secs => tr_args(
            "chrome.seconds",
            "{seconds}s",
            &[("seconds", &secs.to_string())],
        ),
    };
    let info = match latest_info.map(str::trim) {
        None | Some("") => tr("chrome.no_message", "no message").to_owned(),
        Some(text) => {
            let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            shorten(&collapsed, max_chars)
        }
    };
    format!("{child} · {info} · {age}")
}

#[cfg(test)]
mod child_info_tests {
    use super::child_info_segment;

    #[test]
    fn none_and_empty_info_show_no_message() {
        assert_eq!(
            child_info_segment("researcher", None, 12, 40),
            "researcher · no message · 12s"
        );
        assert_eq!(
            child_info_segment("researcher", Some("   "), 0, 40),
            "researcher · no message · now"
        );
    }

    #[test]
    fn long_info_is_truncated_with_ellipsis() {
        let out = child_info_segment("writer", Some("uuuu vvvv wwww"), 5, 10);
        // Budget 10 Zeichen inklusive `…`: 9 Zeichen "uuuu vvvv" + `…`.
        assert_eq!(out, "writer · uuuu vvvv… · 5s");
        let info = out
            .strip_prefix("writer · ")
            .and_then(|rest| rest.strip_suffix(" · 5s"))
            .unwrap_or_default();
        assert_eq!(info.chars().count(), 10);
        assert_eq!(out.chars().count(), 24);
    }

    #[test]
    fn normal_info_is_collapsed_and_kept() {
        assert_eq!(
            child_info_segment("analyst", Some(" scanne  Dateien\nim Ordner "), 0, 40),
            "analyst · scanne Dateien im Ordner · now"
        );
    }
}

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
    if pending {
        tr_args(
            "chrome.mode_pending",
            "Mode: {mode} · Approval: {approval} (pending)",
            &[("mode", mode.as_str()), ("approval", approval)],
        )
    } else {
        tr_args(
            "chrome.mode",
            "Mode: {mode} · Approval: {approval}",
            &[("mode", mode.as_str()), ("approval", approval)],
        )
    }
}

/// Kurzform von [`mode_segment`] für schmale Fenster: `chat · full`.
#[must_use]
pub(crate) fn mode_segment_short(
    mode: InteractionMode,
    approval: Option<ApprovalMode>,
    pending: bool,
) -> String {
    let approval = approval.map_or(UNKNOWN, ApprovalMode::as_str);
    if pending {
        tr_args(
            "chrome.mode_short_pending",
            "{mode} · {approval} (pend.)",
            &[("mode", mode.as_str()), ("approval", approval)],
        )
    } else {
        format!("{} · {approval}", mode.as_str())
    }
}

/// Segment mit dem aktiven Modell, höchstens `max_chars` Zeichen lang.
///
/// # Beschreibung
/// Vollform `provider/model`; ohne Anbieter nur `model`, ohne Modell
/// `kein Modell`. Passt die Vollform nicht, wird zuerst der Anbieter
/// weggelassen; passt auch das Modell nicht, wird es zeichensicher (nach
/// Graphemgrenzen und Terminalspalten) gekürzt und mit `…`
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
        (_, None) => tr("chrome.no_model", "no model").to_owned(),
    };
    if display_width(&full) <= max_chars {
        return full;
    }
    let fallback = model.unwrap_or(full.as_str());
    shorten(fallback, max_chars)
}

/// Kürzt `text` auf höchstens `max_chars` Zeichen inklusive `…`.
fn shorten(text: &str, max_chars: usize) -> String {
    fit_width(text, max_chars)
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
    for grapheme in text.graphemes(true) {
        let width = grapheme.width();
        if used + width + 1 > max {
            break;
        }
        used += width;
        out.push_str(grapheme);
    }
    out.push(ELLIPSIS);
    out
}

/// Harw-DoD-Marke der Statuszeile (PL-97 D5): `(voll, kurz)`.
///
/// # Beschreibung
/// Voll ` ⛨ 2 high `, aus einer ungeprüften Exportquelle
/// ` ⛨ 2 high (unverified) `; kurz ` ⛨ 2 ` bzw. ` ⛨ 2? `. Ist die Zahl eine
/// Untergrenze (älterer Export nicht gelesen), steht `≥ 2`. Die Marke steht
/// nach der Host-Warnung und vor Plan- und Goal-Marke
/// (`app::status_mark_spans`); passt die volle Form nicht, folgt die kurze,
/// sonst entfällt sie.
///
/// # Argumente
/// - `count`: Text der Anzahl (`2` oder `≥ 2`).
/// - `verified`: `false` für eine ungeprüfte Exportquelle.
#[must_use]
pub(crate) fn dod_mark_text(count: &str, verified: bool) -> (String, String) {
    if verified {
        (
            tr_args("chrome.dod_high", " ⛨ {count} high ", &[("count", count)]),
            format!(" ⛨ {count} "),
        )
    } else {
        (
            tr_args(
                "chrome.dod_unverified",
                " ⛨ {count} high (unverified) ",
                &[("count", count)],
            ),
            format!(" ⛨ {count}? "),
        )
    }
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
    fn clipping_respects_graphemes_and_cell_budgets() {
        for text in [
            "漢字漢字漢字",
            "नमस्ते क्षि हिन्दी",
            "e\u{301}e\u{301}e\u{301}",
            "👩‍💻👨‍👩‍👧‍👦👍🏽",
            "a漢e\u{301}क्षि👩‍💻",
        ] {
            for budget in [0, 1, 8, 20, 40] {
                let clipped = fit_width(text, budget);
                assert!(
                    display_width(&clipped) <= budget,
                    "{text:?}: {budget}: {clipped:?}"
                );
                if clipped != text && !clipped.is_empty() {
                    let prefix = clipped.strip_suffix('…').expect("ellipsis");
                    assert!(
                        text.grapheme_indices(true)
                            .any(|(index, _)| index == prefix.len()),
                        "split grapheme: {clipped:?}"
                    );
                }
                assert_eq!(model_segment(None, Some(text), budget), clipped);
            }
        }
        assert_eq!(fit_width("e\u{301}abc", 2), "e\u{301}…");
        assert_eq!(fit_width("👩‍💻abc", 3), "👩‍💻…");
        // Provider is discarded before clipping the model, measured in cells.
        assert_eq!(model_segment(Some("provider"), Some("漢字"), 4), "漢字");
        crate::i18n::with_language(harw_config::UiLanguage::Hindi, || {
            assert!(
                mode_segment(InteractionMode::Work, Some(ApprovalMode::FullAccess), true)
                    .contains("लंबित")
            );
            assert!(child_info_segment("payload-child", None, 0, 20).contains("कोई संदेश नहीं"));
        });
    }

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
    fn dod_mark_text_has_full_and_short_forms() {
        assert_eq!(
            dod_mark_text("2", true),
            (" ⛨ 2 high ".to_owned(), " ⛨ 2 ".to_owned())
        );
        assert_eq!(
            dod_mark_text("≥ 3", false),
            (
                " ⛨ ≥ 3 high (unverified) ".to_owned(),
                " ⛨ ≥ 3? ".to_owned()
            )
        );
        let (full, short) = dod_mark_text("2", true);
        assert!(display_width(&short) < display_width(&full));
    }

    #[test]
    fn mode_segment_shows_mode_and_approval() {
        assert_eq!(
            mode_segment(InteractionMode::Plan, Some(ApprovalMode::Delegated), false),
            "Mode: plan · Approval: auto"
        );
        assert_eq!(
            mode_segment(InteractionMode::Work, Some(ApprovalMode::FullAccess), true),
            "Mode: work · Approval: full (pending)"
        );
        assert_eq!(
            mode_segment(InteractionMode::Chat, None, false),
            "Mode: chat · Approval: –"
        );
    }

    #[test]
    fn model_segment_prefers_full_form() {
        assert_eq!(
            model_segment(Some("anthropic"), Some("claude-x"), 40),
            "anthropic/claude-x"
        );
        assert_eq!(model_segment(None, Some("gpt"), 40), "gpt");
        assert_eq!(model_segment(Some("openai"), None, 40), "no model");
        assert_eq!(model_segment(Some(" "), Some(" "), 40), "no model");
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
