//! Selektoren und Lesebereiche: welcher Index, und wer darf ihn befragen.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`IndexSelector`] (welcher Index, welche
//! Sichtbarkeit) und [`ReadScope`] (welche Sichtbarkeiten der Aufrufer
//! befragen darf). Beide sind reine Daten; die Durchsetzung des
//! Lesebereichs — die eigentliche Sicherheitsentscheidung — liegt in
//! [`crate::resolve_index`], nicht hier.
//!
//! # Nebenläufigkeit
//! [`IndexSelector`] und [`ReadScope`] sind reine Daten (`Clone`,
//! `PartialEq`) ohne interne Veränderlichkeit und ohne Einschränkung
//! zwischen Threads teilbar.
//!
//! # Fehler
//! Dieses Modul selbst erzeugt keine Fehler.
//!
//! # Examples
//! ```rust
//! use harw_lens_query::{IndexSelector, ReadScope};
//!
//! let selector = IndexSelector::new("knowledge.palace", "operator-only");
//! let scope = ReadScope::new(["workspace".to_owned()]);
//! assert!(!scope.allows(&selector.visibility));
//! ```

use std::collections::HashSet;

/// Welchen Index und welche Sichtbarkeit eine Abfrage befragen soll.
///
/// # Description
/// Trägt keinen Bezug zu einer tatsächlichen Datei oder einem geöffneten
/// Store — die Auflösung übernimmt [`crate::resolve_index`], nachdem
/// [`ReadScope::allows`] den Selektor freigegeben hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSelector {
    /// Name des angefragten Index (z. B. `"docs.design"`,
    /// `"knowledge.palace"`).
    pub index_name: String,
    /// Der angefragte Sichtbarkeits-Bucket (z. B. `"workspace"`,
    /// `"operator-only"`).
    pub visibility: String,
}

impl IndexSelector {
    /// Baut einen Selektor aus Indexname und Sichtbarkeit.
    ///
    /// # Arguments
    /// - `index_name` (`impl Into<String>`): der angefragte Indexname.
    /// - `visibility` (`impl Into<String>`): die angefragte Sichtbarkeit.
    ///
    /// # Returns
    /// Ein neuer `IndexSelector`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_query::IndexSelector;
    ///
    /// let selector = IndexSelector::new("docs.design", "workspace");
    /// assert_eq!(selector.index_name, "docs.design");
    /// ```
    #[must_use]
    pub fn new(index_name: impl Into<String>, visibility: impl Into<String>) -> Self {
        Self {
            index_name: index_name.into(),
            visibility: visibility.into(),
        }
    }
}

/// Der Lesebereich eines Aufrufers: welche Sichtbarkeits-Buckets er befragen
/// darf.
///
/// # Description
/// Eine geschlossene Freigabeliste, keine Ausschlussliste — ein
/// [`IndexSelector`], dessen `visibility` hier nicht enthalten ist, wird von
/// [`crate::resolve_index`] mit
/// [`crate::QueryError::IndexNotVisible`] abgelehnt, nicht stillschweigend
/// mit einer leeren Trefferliste beantwortet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReadScope {
    allowed_visibilities: HashSet<String>,
}

impl ReadScope {
    /// Baut einen Lesebereich aus einer Menge freigegebener Sichtbarkeiten.
    ///
    /// # Arguments
    /// - `allowed` (`impl IntoIterator<Item = String>`): die freigegebenen
    ///   Sichtbarkeits-Bucket-Namen.
    ///
    /// # Returns
    /// Ein neuer `ReadScope`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_query::ReadScope;
    ///
    /// let scope = ReadScope::new(["workspace".to_owned()]);
    /// assert!(scope.allows("workspace"));
    /// assert!(!scope.allows("operator-only"));
    /// ```
    #[must_use]
    pub fn new(allowed: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed_visibilities: allowed.into_iter().collect(),
        }
    }

    /// Baut einen Lesebereich, der genau eine Sichtbarkeit freigibt.
    ///
    /// # Arguments
    /// - `visibility` (`impl Into<String>`): die einzige freigegebene
    ///   Sichtbarkeit.
    ///
    /// # Returns
    /// Ein neuer `ReadScope` mit genau einem freigegebenen Bucket.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_query::ReadScope;
    ///
    /// let scope = ReadScope::single("workspace");
    /// assert!(scope.allows("workspace"));
    /// ```
    #[must_use]
    pub fn single(visibility: impl Into<String>) -> Self {
        Self::new([visibility.into()])
    }

    /// Prüft, ob `visibility` in diesem Lesebereich freigegeben ist.
    ///
    /// # Arguments
    /// - `visibility` (`&str`): der zu prüfende Sichtbarkeits-Bucket-Name.
    ///
    /// # Returns
    /// `true`, wenn `visibility` bei der Konstruktion freigegeben wurde.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_query::ReadScope;
    ///
    /// let scope = ReadScope::single("workspace");
    /// assert!(scope.allows("workspace"));
    /// assert!(!scope.allows("operator-only"));
    /// ```
    #[must_use]
    pub fn allows(&self, visibility: &str) -> bool {
        self.allowed_visibilities.contains(visibility)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_selector_new_stores_both_fields() {
        let selector = IndexSelector::new("docs.design", "workspace");
        assert_eq!(selector.index_name, "docs.design");
        assert_eq!(selector.visibility, "workspace");
    }

    #[test]
    fn test_read_scope_allows_only_configured_visibilities() {
        let scope = ReadScope::new(["workspace".to_owned(), "operator-only".to_owned()]);
        assert!(scope.allows("workspace"));
        assert!(scope.allows("operator-only"));
        assert!(!scope.allows("something-else"));
    }

    #[test]
    fn test_read_scope_default_allows_nothing() {
        let scope = ReadScope::default();
        assert!(!scope.allows("workspace"));
    }

    #[test]
    fn test_read_scope_single_allows_exactly_one_visibility() {
        let scope = ReadScope::single("workspace");
        assert!(scope.allows("workspace"));
        assert!(!scope.allows("operator-only"));
    }
}
