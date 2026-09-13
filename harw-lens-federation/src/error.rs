//! Fehlertyp für `harw-lens-federation`.
//!
//! # Verantwortungsbereich
//! Ein einziges Enum [`FederationError`] für alle Fehler dieser Crate. Die
//! Föderation selbst kennt keinen neuen Fehlerfall gegenüber
//! `harw-lens-query`: **jede** Fehlerursache, die eine einzelne Abfrage
//! (`resolve_index`, `query`) liefern kann, ist auch für [`crate::federated_query`]
//! gültig — nur eben für einen von mehreren Indizes. `Display`, die
//! `Debug`-Delegation und `std::error::Error` werden nicht von Hand
//! geschrieben, sondern von `#[derive(harw_macros::HarwError)]` erzeugt
//! (Muster: `harw-lens-query/src/error.rs`). Kein `anyhow`, kein `thiserror`.
//! Weil der Enum-Name auf `Error` endet, erzeugt das Makro außerdem den
//! Typalias `FederationResult<T>`.
//!
//! # Warum `IndexNotVisible` hier nicht neu auftaucht
//! [`harw_lens_query::QueryError::IndexNotVisible`] entsteht bereits in
//! [`harw_lens_query::resolve_index`], **bevor** [`crate::federated_query`]
//! überhaupt zur nächsten Sichtbarkeitsprüfung kommt. Weil diese Variante
//! über `#[from]` unverändert durchgereicht wird, kann ein Aufrufer weiterhin
//! `matches!(err, FederationError::Query(QueryError::IndexNotVisible { .. }))`
//! prüfen -- ein zweites, gleichbedeutendes `FederationError`-Vokabular für
//! denselben Fall wäre nur eine Übersetzungsstufe, die nichts hinzufügt und
//! bei jeder Änderung an `QueryError` neu synchron gehalten werden müsste.
//!
//! Exportierte Typen: [`FederationError`], [`FederationResult`].
//!
//! # Nebenläufigkeit
//! `FederationError` ist reine Daten (`Debug`) ohne interne Veränderlichkeit
//! und ohne Einschränkung zwischen Threads teilbar. Es leitet weder `Clone`
//! noch `PartialEq` ab, weil die gewickelte [`harw_lens_query::QueryError`]
//! selbst keines von beidem anbietet.
//!
//! # Examples
//! ```rust
//! use harw_lens_federation::FederationError;
//! use harw_lens_query::QueryError;
//!
//! let err: FederationError = QueryError::EmptyEmbedding.into();
//! assert!(err.to_string().contains("no vector"));
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate (erzeugt den Typalias `FederationResult<T>`).
///
/// # Description
/// Ein einziger Wickel um [`harw_lens_query::QueryError`]: die Föderation
/// fügt keinen neuen Fehlerfall hinzu, sie multipliziert nur die Stelle, an
/// der derselbe Fehler entstehen kann (einer von mehreren Indizes statt
/// genau einem).
#[derive(Debug, HarwError)]
pub enum FederationError {
    /// Fehler aus `harw-lens-query`, entstanden bei der Auflösung oder
    /// Abfrage **eines** der in `federated_query` übergebenen Selektoren;
    /// deferiert `Display`/`source()` an den inneren Fehler.
    #[from]
    Query(harw_lens_query::QueryError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_query::QueryError;

    #[test]
    fn test_federation_error_from_query_error_delegates_display() {
        let inner = QueryError::EmptyEmbedding;
        let expected = inner.to_string();
        let err: FederationError = inner.into();
        assert_eq!(err.to_string(), expected);
    }

    #[test]
    fn test_federation_error_from_index_not_visible_matches_through() {
        let inner = QueryError::IndexNotVisible {
            index_name: "knowledge.palace".to_owned(),
            visibility: "operator-only".to_owned(),
        };
        let err: FederationError = inner.into();
        assert!(matches!(
            err,
            FederationError::Query(QueryError::IndexNotVisible { .. })
        ));
    }
}
