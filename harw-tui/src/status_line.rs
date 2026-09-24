//! Bausteine der Statuszeile: Modus/Freigabe und aktives Modell.
//!
//! Reine Textfunktionen ohne Zustand; `app.rs` setzt die Segmente zur
//! eigentlichen Statuszeile zusammen.

use harw_core::InteractionMode;
use harw_extension_api::approval_mode::ApprovalMode;

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

#[cfg(test)]
mod tests {
    use super::*;

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
