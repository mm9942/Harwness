//! Regelbasierte Erkennung expliziter Korrektursignale in User-Nachrichten.
//!
//! # Verantwortungsbereich
//! Wandelt einen freien User-Text in ein [`Signal::Correction`] um, wenn der
//! Text eine oder mehrere Korrektur-Phrasen enthält. Bewusst konservativ und
//! rein textbasiert — keine LLM-Aufrufe, kein Silent-Inference. Wir folgen
//! damit der ClawHub-`self-improving`-Regel: "Never infer from silence".
//!
//! # Nicht-Ziele
//! - Klassifikation nach Sentiment (nutzt keine Emotion-Heuristik).
//! - Semantische Ähnlichkeit (nur wörtliche Muster).
//! - Automatische Promotion in HOT — das entscheidet `maintain()`.
//!
//! # Phrasenliste
//! Die Muster stammen aus dem ClawHub-`self-improving`-Skill (SKILL.md,
//! Abschnitt "Learning Signals") und sind bewusst mehrsprachig. Ein Match
//! genügt; Groß-/Kleinschreibung wird ignoriert.

use crate::types::Signal;

/// Explizite Korrekturphrasen (Deutsch + Englisch, gemischt).
///
/// # Beschreibung
/// Die Liste wächst konservativ. Jede Phrase muss so eng sein, dass sie in
/// einer echten Konversation praktisch nur bei einer echten Korrektur
/// auftaucht — Sätze wie „ich denke, das könnte …" gehören **nicht** dazu.
const CORRECTION_PHRASES: &[&str] = &[
    // ClawHub-Kernliste (englisch)
    "no, that's not right",
    "actually, it should be",
    "you're wrong about",
    "stop doing",
    "i told you before",
    "why do you keep",
    // Präferenz-Signale (nur mit klarer Anweisung)
    "always do",
    "never do",
    "remember that i always",
    // Deutsch
    "das ist falsch",
    "das stimmt nicht",
    "nicht so",
    "hör auf",
    "höre auf",
    "hab ich dir schon gesagt",
    "wie oft muss ich",
    "merk dir",
    "merke dir",
    "bitte immer",
    "bitte nie",
    "bitte niemals",
];

/// Prüft `text` gegen die Korrekturphrasen und liefert bei Match ein
/// [`Signal::Correction`] zurück.
///
/// # Argumente
/// - `text` (`&str`): rohe User-Nachricht.
/// - `context` (`Option<String>`): optionaler Kontext (z. B. Session-/Turn-ID),
///   der im Signal gespeichert wird.
///
/// # Rückgabe
/// - `Some(Signal::Correction { text, context })` bei Match.
/// - `None` sonst.
///
/// # Nebenläufigkeit
/// Rein funktional, thread-safe.
#[must_use]
pub fn detect_correction(text: &str, context: Option<String>) -> Option<Signal> {
    let hay = text.to_lowercase();
    if CORRECTION_PHRASES.iter().any(|p| hay.contains(p)) {
        Some(Signal::Correction {
            text: text.to_owned(),
            context,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn detects_english_correction_no_thats_not_right() {
        let s = detect_correction("No, that's not right — try again.", None);
        assert!(matches!(s, Some(Signal::Correction { .. })));
    }

    #[test]
    fn detects_german_correction_das_ist_falsch() {
        let s = detect_correction("Nein, das ist falsch, ich meinte X.", None);
        assert!(matches!(s, Some(Signal::Correction { .. })));
    }

    #[test]
    fn detects_case_insensitively() {
        let s = detect_correction("STOP DOING that please.", None);
        assert!(matches!(s, Some(Signal::Correction { .. })));
    }

    #[test]
    fn ignores_plain_chat() {
        assert!(detect_correction("Wie geht's dir?", None).is_none());
        assert!(detect_correction("Please write me a haiku.", None).is_none());
        assert!(detect_correction("I think we could try Y.", None).is_none());
    }

    #[test]
    fn attaches_context_verbatim() -> TestResult {
        let s = detect_correction("Nicht so, bitte anders.", Some("turn-42".into()))
            .ok_or(TestError::Missing("Correction-Signal"))?;
        match s {
            Signal::Correction { context, .. } => assert_eq!(context.as_deref(), Some("turn-42")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Correction, got {other:?}"
                )));
            }
        }
        Ok(())
    }
}
