//! Fehlertyp für `harw-lens-query`.
//!
//! # Verantwortungsbereich
//! Ein einziges Enum [`QueryError`] für alle Fehler dieser Crate. Die
//! wichtigste Variante ist [`QueryError::IndexNotVisible`]: ein Selektor
//! außerhalb des Lesebereichs eines Aufrufers muss **diesen** Fehler
//! liefern, niemals eine leere Trefferliste — siehe die Modul-Dokumentation
//! von `crate` und von [`crate::resolve_index`]. `Display`, die
//! `Debug`-Delegation und `std::error::Error` werden nicht von Hand
//! geschrieben, sondern von `#[derive(harw_macros::HarwError)]` erzeugt
//! (Muster: `harw-lens-types/src/error.rs`, `harw-lens-index/src/error.rs`).
//! Kein `anyhow`, kein `thiserror`. Weil der Enum-Name auf `Error` endet,
//! erzeugt das Makro außerdem den Typalias `QueryResult<T>`.
//!
//! Exportierte Typen: [`QueryError`], [`QueryResult`].
//!
//! # Nebenläufigkeit
//! `QueryError` ist reine Daten (`Debug`) ohne interne Veränderlichkeit und
//! ohne Einschränkung zwischen Threads teilbar. Es leitet weder `Clone` noch
//! `PartialEq` ab, weil mehrere `#[from]`-Varianten fremde Fehlertypen
//! wickeln, die selbst keines von beidem anbieten
//! (`harw_home::HomeError`, `harw_lens_store::LensStoreError`,
//! `harw_lens_index::IndexError`).
//!
//! # Examples
//! ```rust
//! use harw_lens_query::QueryError;
//!
//! let err = QueryError::IndexNotVisible {
//!     index_name: "knowledge.palace".to_owned(),
//!     visibility: "operator-only".to_owned(),
//! };
//! assert!(err.to_string().contains("operator-only"));
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate (erzeugt den Typalias `QueryResult<T>`).
///
/// # Description
/// Jede Variante trägt den vollständigen Kontext, der zum Verstehen des
/// Fehlers ohne Quellcode-Lektüre nötig ist.
#[derive(Debug, HarwError)]
pub enum QueryError {
    /// Ein [`crate::IndexSelector`] fragt einen Index/Sichtbarkeit an, die
    /// außerhalb des [`crate::ReadScope`] des Aufrufers liegt. Entsteht
    /// ausschließlich in [`crate::resolve_index`], **bevor** irgendein
    /// Dateisystemzugriff stattfindet — ein zu weit gefasster Selektor
    /// wird abgelehnt, nicht stillschweigend auf eine leere Trefferliste
    /// abgebildet.
    ///
    /// # Arguments
    /// - `index_name` (`String`): der angefragte Indexname.
    /// - `visibility` (`String`): die angefragte, nicht freigegebene
    ///   Sichtbarkeit.
    #[msg(
        "selector requests index '{index_name}' at visibility '{visibility}', which is outside the caller's read scope"
    )]
    IndexNotVisible {
        /// Der angefragte Indexname.
        index_name: String,
        /// Die angefragte, nicht freigegebene Sichtbarkeit.
        visibility: String,
    },

    /// Ein [`harw_lens_embed::Embedder`] hat für den (einzelnen) Abfragetext
    /// keinen Vektor geliefert — ein Vertragsbruch des Embedders, kein aus
    /// Nutzereingaben erreichbarer Zustand. Entsteht ausschließlich in
    /// [`crate::query`].
    #[msg("embedder returned no vector for the query text")]
    EmptyEmbedding,

    /// Fehler bei der Auflösung eines Sichtbarkeits-Indexpfads (ungültiger
    /// Sichtbarkeitsname); deferiert `Display`/`source()` an den inneren
    /// Fehler.
    #[from]
    Home(harw_home::HomeError),

    /// Fehler aus `harw-lens-store` beim Öffnen des Sichtbarkeits-Stores;
    /// deferiert `Display`/`source()` an den inneren Fehler.
    #[from]
    Store(harw_lens_store::LensStoreError),

    /// Fehler aus `harw-lens-index` beim Laden des Index oder bei der Suche
    /// selbst (insbesondere ein abgelehntes Manifest); deferiert
    /// `Display`/`source()` an den inneren Fehler.
    #[from]
    Index(harw_lens_index::IndexError),

    /// Fehler aus `harw-lens-embed` beim Einbetten des Abfragetexts;
    /// deferiert `Display`/`source()` an den inneren Fehler.
    #[from]
    Embed(harw_lens_embed::EmbedError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_error_display_index_not_visible() {
        let err = QueryError::IndexNotVisible {
            index_name: "knowledge.palace".to_owned(),
            visibility: "operator-only".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "selector requests index 'knowledge.palace' at visibility 'operator-only', which is outside the caller's read scope"
        );
    }

    #[test]
    fn test_query_error_display_empty_embedding() {
        let err = QueryError::EmptyEmbedding;
        assert_eq!(
            err.to_string(),
            "embedder returned no vector for the query text"
        );
    }

    #[test]
    fn test_query_error_from_home() {
        let home_err = harw_home::HomeError::NoHomeDirectory;
        let err: QueryError = home_err.into();
        assert!(matches!(err, QueryError::Home(_)));
    }
}
