//! Token-Nutzungs-Aggregat.

use serde::{Deserialize, Serialize};

/// Aggregierte Token-Nutzung für einen Turn oder eine Session.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    /// Anzahl der Tokens, die beim Schreiben eines neuen Prompt-Cache-Eintrags
    /// angefallen sind (z. B. `cache_creation_input_tokens`).
    #[serde(default)]
    pub cache_write_tokens: Option<u64>,
    /// `true`, wenn der Provider Cache-Tokens **getrennt** von
    /// `input_tokens` meldet (Anthropic: `input_tokens` zählt nur den
    /// ungecachten Rest). `false` (OpenAI-Semantik): `cached_tokens` ist eine
    /// Teilmenge von `input_tokens`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cache_separate: bool,
}

impl TokenUsage {
    /// Summe aus Input- und Output-Tokens (ohne Caching-Korrektur).
    #[must_use]
    pub fn total(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Alle Prompt-Tokens dieses Aufrufs inklusive Cache-Read/-Write —
    /// die tatsächliche Kontextfenster-Belegung vor der Ausgabe.
    #[must_use]
    pub fn prompt_tokens(&self) -> u64 {
        if self.cache_separate {
            self.input_tokens
                .saturating_add(self.cached_tokens.unwrap_or(0))
                .saturating_add(self.cache_write_tokens.unwrap_or(0))
        } else {
            self.input_tokens
        }
    }

    /// Ungecachte, neu verarbeitete Eingabe-Tokens dieses Aufrufs.
    ///
    /// # Beschreibung
    /// Bei `cache_separate` (Anthropic) ist `input_tokens` bereits der
    /// ungecachte Rest; Cache-Writes kommen hinzu, weil sie erstmals
    /// verarbeitet wurden. Sonst (OpenAI-Semantik) ist `cached_tokens` eine
    /// Teilmenge von `input_tokens` und wird abgezogen. Cache-Reads zählen in
    /// beiden Fällen nicht: sie sind ein erneut gesendeter, bereits
    /// bekannter Kontext.
    #[must_use]
    pub fn uncached_input_tokens(&self) -> u64 {
        if self.cache_separate {
            self.input_tokens
                .saturating_add(self.cache_write_tokens.unwrap_or(0))
        } else {
            self.input_tokens
                .saturating_sub(self.cached_tokens.unwrap_or(0))
        }
    }

    /// Neue Tokens eines Aufrufs: [`Self::uncached_input_tokens`] plus
    /// `output_tokens`.
    ///
    /// # Beschreibung
    /// Grundlage eines Token-Budgets, das die Arbeit eines Agenten misst
    /// statt der Länge seines Verlaufs: ein wieder gesendeter, gecachter
    /// Prompt zählt nicht jede Runde erneut.
    #[must_use]
    pub fn fresh_tokens(&self) -> u64 {
        self.uncached_input_tokens()
            .saturating_add(self.output_tokens)
    }

    /// Akkumuliert eine weitere Nutzung in `self`.
    pub fn add(&mut self, other: &TokenUsage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.reasoning_tokens = sum_opt(self.reasoning_tokens, other.reasoning_tokens);
        self.cached_tokens = sum_opt(self.cached_tokens, other.cached_tokens);
        self.cache_write_tokens = sum_opt(self.cache_write_tokens, other.cache_write_tokens);
        self.cache_separate |= other.cache_separate;
    }
}

fn sum_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0).saturating_add(y.unwrap_or(0))),
    }
}

#[cfg(test)]
mod prompt_token_tests {
    use super::TokenUsage;

    #[test]
    fn prompt_tokens_respects_cache_semantics() {
        let anthropic = TokenUsage {
            input_tokens: 10,
            cached_tokens: Some(90),
            cache_write_tokens: Some(5),
            cache_separate: true,
            ..TokenUsage::default()
        };
        assert_eq!(anthropic.prompt_tokens(), 105);
        let openai = TokenUsage {
            input_tokens: 100,
            cached_tokens: Some(90),
            ..TokenUsage::default()
        };
        assert_eq!(openai.prompt_tokens(), 100);
    }

    #[test]
    fn fresh_tokens_ignore_cached_input() {
        let anthropic = TokenUsage {
            input_tokens: 10,
            output_tokens: 7,
            cached_tokens: Some(90),
            cache_write_tokens: Some(5),
            cache_separate: true,
            ..TokenUsage::default()
        };
        assert_eq!(anthropic.uncached_input_tokens(), 15);
        assert_eq!(anthropic.fresh_tokens(), 22);
        let openai = TokenUsage {
            input_tokens: 100,
            output_tokens: 3,
            cached_tokens: Some(90),
            ..TokenUsage::default()
        };
        assert_eq!(openai.uncached_input_tokens(), 10);
        assert_eq!(openai.fresh_tokens(), 13);
        let uncached = TokenUsage {
            input_tokens: 40,
            output_tokens: 2,
            ..TokenUsage::default()
        };
        assert_eq!(uncached.fresh_tokens(), 42);
    }
}
