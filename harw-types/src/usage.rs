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
}

impl TokenUsage {
    /// Summe aus Input- und Output-Tokens (ohne Caching-Korrektur).
    #[must_use]
    pub fn total(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Akkumuliert eine weitere Nutzung in `self`.
    pub fn add(&mut self, other: &TokenUsage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.reasoning_tokens = sum_opt(self.reasoning_tokens, other.reasoning_tokens);
        self.cached_tokens = sum_opt(self.cached_tokens, other.cached_tokens);
        self.cache_write_tokens = sum_opt(self.cache_write_tokens, other.cache_write_tokens);
    }
}

fn sum_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0).saturating_add(y.unwrap_or(0))),
    }
}
