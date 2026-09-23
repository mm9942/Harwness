//! Glob-Selektor über Sektions- und Fragmentnamen.
//!
//! # Verantwortungsbereich
//! Besitzt [`Selector`]: ein Glob-Muster, das gegen Sektions- oder
//! Fragmentnamen geprüft wird (z. B. um eine `ContextCeiling` auf
//! `"history.*"` einzuschränken).
//!
//! # Wiederverwendung des Glob-Abgleichs — zweite Implementierung
//! Der Workspace besitzt bereits einen Glob-Abgleich mit identischer
//! `*`/`**`/`?`-Semantik: `ScopeMatcher::matches_glob` in
//! `harw-plan/src/admission.rs` (nicht, wie im Auftrag vermutet, in
//! `harw-ops/src/analyze.rs` — dort wird `ScopeMatcher` nur *aufgerufen*,
//! definiert ist es in `harw-plan`). `harw-plan` steht nicht in der
//! freigegebenen Abhängigkeitsliste dieser Crate (`serde`, `jiff`,
//! `harw-types`, `harw-lens-types`, `harw-macros`) und wäre auch
//! architektonisch falsch herum: `harw-plan` ist ein Orchestrierungs-Crate
//! auf einer höheren Ebene, `harw-context` reines L1-Vokabular. Diese Datei
//! implementiert deshalb `*`, `**` und `?` **gleichbedeutend** zur
//! `ScopeMatcher`-Fassung neu (segmentweiser DP-Abgleich über `/`), ohne
//! deren Pfad-spezifische Normalisierung (keine Ablehnung von `..` oder
//! absoluten Pfaden — Sektions- und Fragmentnamen sind keine Dateipfade und
//! ein `..`-Segment ist hier kein Sicherheitsproblem).
//!
//! # Exportierte Typen
//! [`Selector`].
//!
//! # Nebenläufigkeit
//! Reiner Werttyp ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Fehler
//! [`crate::error::ContextError`] beim Konstruieren aus einem leeren oder
//! steuerzeichenhaltigen Muster.
//!
//! # Examples
//! ```rust
//! use harw_context::Selector;
//!
//! let selector = Selector::try_new("history.*").unwrap();
//! assert!(selector.matches("history.tail"));
//! assert!(!selector.matches("plan.current"));
//! ```

use crate::error::{ContextError, validate_name};

/// Ein Glob über Sektions- und Fragmentnamen.
///
/// # Description
/// Unterstützt `*` (beliebig viele Zeichen außer `/`), `**` (beliebig viele
/// Zeichen einschließlich `/`, also über Segmentgrenzen hinweg) und `?`
/// (genau ein Zeichen außer `/`). Alles andere ist ein Literal. Kein
/// Brace-Expansion, keine Zeichenklassen — dieselbe Mini-Syntax wie
/// `harw_plan::admission::ScopeMatcher::matches_glob` (siehe Moduldoku).
///
/// Ein Muster ohne `/` verhält sich auf einem `/`-freien Namen wie ein
/// gewöhnlicher Wildcard-Abgleich innerhalb eines einzigen Segments; `/`
/// wird nur relevant, wenn Sektions- oder Fragmentnamen selbst `/`
/// enthalten.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Selector(String);

impl Selector {
    /// Konstruiert einen Selektor aus einem Glob-Muster.
    ///
    /// # Arguments
    /// - `pattern` (`impl Into<String>`): das Muster, z. B. `"history.*"`.
    ///
    /// # Returns
    /// `Ok(Self)`, wenn das getrimmte Muster nicht leer ist und kein
    /// Steuerzeichen enthält.
    ///
    /// # Errors
    /// [`ContextError::InvalidName`] mit `kind = "selector pattern"`, wenn
    /// das Muster leer oder steuerzeichenhaltig ist.
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::Selector;
    /// assert!(Selector::try_new("**").is_ok());
    /// assert!(Selector::try_new("").is_err());
    /// ```
    pub fn try_new(pattern: impl Into<String>) -> Result<Self, ContextError> {
        validate_name("selector pattern", pattern).map(Self)
    }

    /// Borrowt das Rohmuster als String-Slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Trifft dieser Selektor den genannten Namen?
    ///
    /// # Arguments
    /// - `name` (`&str`): der zu prüfende Sektions- oder Fragmentname.
    ///
    /// # Returns
    /// `true`, wenn `name` vollständig auf das Muster passt. Ein Treffer
    /// verlangt einen vollständigen Match, kein Präfix — `"history"` matcht
    /// `"history.tail"` **nicht**, nur `"history*"` oder `"history.*"` tun
    /// das.
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::Selector;
    /// let selector = Selector::try_new("history.*").unwrap();
    /// assert!(selector.matches("history.tail"));
    /// assert!(!selector.matches("history"));
    /// ```
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        glob_matches(&self.0, name)
    }
}

impl std::fmt::Display for Selector {
    /// Schreibt das rohe Muster ohne Anführungszeichen oder Typ-Wrapper.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Segmentweiser Glob-Abgleich über `/`. `**` matcht null oder mehr ganze
/// Segmente, alle anderen Segmente werden über [`segment_matches`]
/// geprüft. Mirror der Algorithmusstruktur von
/// `harw_plan::admission::glob_matches`, ohne dessen Pfad-Normalisierung.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern_segments: Vec<&str> = pattern.split('/').collect();
    let name_segments: Vec<&str> = name.split('/').collect();

    let p_len = pattern_segments.len();
    let n_len = name_segments.len();

    // dp[i][j] = pattern_segments[i..] matcht name_segments[j..].
    let mut dp = vec![vec![false; n_len + 1]; p_len + 1];
    dp[p_len][n_len] = true;

    for i in (0..=p_len).rev() {
        for j in (0..=n_len).rev() {
            if i == p_len && j == n_len {
                continue; // Basisfall bereits gesetzt.
            }
            dp[i][j] = if i == p_len {
                false
            } else if pattern_segments[i] == "**" {
                dp[i + 1][j] || (j < n_len && dp[i][j + 1])
            } else {
                j < n_len
                    && segment_matches(pattern_segments[i], name_segments[j])
                    && dp[i + 1][j + 1]
            };
        }
    }

    dp[0][0]
}

/// Klassischer Wildcard-Abgleich innerhalb eines Segments: `*` beliebig
/// viele Zeichen, `?` genau ein Zeichen, sonst Literalvergleich.
fn segment_matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_idx: Option<usize> = None;
    let mut match_idx = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_idx = Some(pi);
            match_idx = ti;
            pi += 1;
        } else if let Some(si) = star_idx {
            pi = si + 1;
            match_idx += 1;
            ti = match_idx;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }

    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::Selector;
    use crate::error::ContextError;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_matches_exact_literal_hit() -> TestResult {
        let selector = Selector::try_new("history.tail").map_err(ctx("history.tail"))?;
        assert!(selector.matches("history.tail"));
        Ok(())
    }

    #[test]
    fn test_matches_star_hit_within_segment() -> TestResult {
        let selector = Selector::try_new("history.*").map_err(ctx("history.*"))?;
        assert!(selector.matches("history.tail"));
        assert!(selector.matches("history.head"));
        Ok(())
    }

    #[test]
    fn test_matches_non_hit_different_literal() -> TestResult {
        let selector = Selector::try_new("history.*").map_err(ctx("history.*"))?;
        assert!(!selector.matches("plan.current"));
        Ok(())
    }

    #[test]
    fn test_matches_prefix_without_boundary_is_not_a_hit() -> TestResult {
        // "history" ist ein Literal ohne Wildcard: ein Präfix-Treffer ohne
        // explizite Grenze (`*`) darf nicht als Match zählen.
        let selector = Selector::try_new("history").map_err(ctx("history"))?;
        assert!(!selector.matches("history.tail"));
        assert!(selector.matches("history"));
        Ok(())
    }

    #[test]
    fn test_matches_double_star_crosses_segment_boundary() -> TestResult {
        let selector = Selector::try_new("plan/**").map_err(ctx("plan/**"))?;
        assert!(selector.matches("plan/a/b/c"));
        assert!(selector.matches("plan"));
        Ok(())
    }

    #[test]
    fn test_try_new_rejects_empty_pattern() {
        assert!(matches!(
            Selector::try_new(""),
            Err(ContextError::InvalidName { .. })
        ));
    }
}
