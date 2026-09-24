//! Fehlertyp der Fassade: `LensError`.
//!
//! # Verantwortungsbereich
//! Ein einziges Enum [`LensError`] für die zwei Aufrufpfade dieser Fassade
//! ([`crate::build`] und [`crate::ask`]). `Display`, die `Debug`-Ableitung
//! und `std::error::Error` werden nicht von Hand geschrieben, sondern von
//! `#[derive(harw_macros::HarwError)]` erzeugt (Muster:
//! `harw-lens-query/src/error.rs`, `harw-lens-source/src/error.rs`). Kein
//! `anyhow`, kein `thiserror`. Weil der Enum-Name auf `Error` endet, erzeugt
//! das Makro außerdem den Typalias [`LensResult`].
//!
//! # Verdichten oder durchreichen?
//! Beides, an unterschiedlichen Stellen — das ist die eigentliche
//! Entscheidung dieses Moduls:
//!
//! - **Verdichtet** wird auf der äußeren Ebene: zehn Crates, aber nur zwei
//!   Varianten, [`LensError::Source`] und [`LensError::Query`] — je eine
//!   für den Bau- und den Abfragepfad. Das ist die Grenze, die die
//!   Projektregel meint: ein Aufrufer von [`crate::build`]/[`crate::ask`]
//!   muss nicht wissen, dass hinter `harw-lens-source` selbst noch
//!   `harw-lens-store`, `harw-lens-index`, `harw-lens-embed`, `std::io` und
//!   `serde_json` stecken — diese Verdichtung fand schon in
//!   `harw-lens-source`/`harw-lens-query` statt, `LensError` wiederholt sie
//!   nicht, sondern übernimmt genau deren beide bereits verdichteten
//!   Fehlertypen unverändert.
//! - **Durchgereicht** wird auf der inneren Ebene: [`LensError::Source`]
//!   und [`LensError::Query`] wickeln [`SourceError`]/[`QueryError`] über
//!   `#[from]` **unverändert** ein, statt sie auf eine Zeichenkette oder
//!   eine grobe Kategorie abzubilden. Genau das verhindert den Verlust, vor
//!   dem der Auftrag warnt: ein Aufrufer, der einen Selektor außerhalb
//!   seines [`crate::ReadScope`] anfragt, erhält
//!   `LensError::Query(QueryError::IndexNotVisible { .. })` — unterscheidbar
//!   per `matches!` von z. B. `LensError::Query(QueryError::Index(_))` (ein
//!   fehlender oder inkompatibler Index; siehe die Tests in
//!   `tests/facade.rs`, insbesondere
//!   `test_index_not_visible_is_distinguishable_from_missing_index_error`
//!   und `test_source_error_is_distinguishable_from_query_error`). Eine
//!   Fassade, die stattdessen `LensError::Failed(String)` einführt oder die
//!   inneren Varianten selbst noch einmal in eigene `LensError`-Varianten
//!   umschreibt, würde entweder diese Unterscheidung zerstören oder die
//!   Verdichtung, die `harw-lens-source`/`harw-lens-query` bereits geleistet
//!   haben, ein zweites Mal (und unvollständig) nachbauen.
//!
//! [`SourceError`] und [`QueryError`] selbst werden deshalb aus dieser Crate
//! re-exportiert (siehe `crate`-Modulwurzel): ohne diesen Re-Export müsste
//! ein Aufrufer, der eine innere Variante wie `IndexNotVisible` per `match`
//! benennen will, `harw-lens-query` selbst als Abhängigkeit hinzufügen —
//! genau die Kenntnis von Innencrate-Namen, die diese Fassade abschaffen
//! soll.
//!
//! # Nebenläufigkeit
//! `LensError` ist reine Daten (`Debug`) ohne interne Veränderlichkeit und
//! ohne Einschränkung zwischen Threads teilbar. Es leitet weder `Clone` noch
//! `PartialEq` ab, weil beide gewickelten Fehlertypen
//! ([`SourceError`], [`QueryError`]) selbst keines von beidem anbieten.
//!
//! # Examples
//! ```rust
//! use harw_lens::{LensError, QueryError};
//!
//! let err = LensError::Query(QueryError::IndexNotVisible {
//!     index_name: "knowledge.palace".to_owned(),
//!     visibility: "operator-only".to_owned(),
//! });
//! assert!(err.to_string().contains("operator-only"));
//! assert!(matches!(err, LensError::Query(QueryError::IndexNotVisible { .. })));
//! ```

use harw_macros::HarwError;

use harw_lens_query::QueryError;
use harw_lens_source::SourceError;

/// Fehler dieser Fassade (erzeugt den Typalias [`LensResult`]).
///
/// # Description
/// Genau zwei Varianten, eine je Aufrufpfad ([`crate::build`],
/// [`crate::ask`]); siehe den `//!`-Block dieses Moduls für die Begründung,
/// warum das weder eine vollständige Verdichtung noch ein reines
/// Durchreichen ist, sondern beides an unterschiedlichen Stellen.
#[derive(Debug, HarwError)]
pub enum LensError {
    /// Fehler beim Bauen eines Index über [`crate::build`]; deferiert
    /// `Display`/`source()` unverändert an [`SourceError`], das bereits
    /// alle darunterliegenden Fehler von `harw-lens-store`,
    /// `harw-lens-index`, `harw-lens-embed`, `std::io` und `serde_json`
    /// kennt.
    #[from]
    Source(SourceError),

    /// Fehler bei einer Abfrage über [`crate::ask`]; deferiert
    /// `Display`/`source()` unverändert an [`QueryError`], das bereits alle
    /// darunterliegenden Fehler von `harw-home`, `harw-lens-store`,
    /// `harw-lens-index` und `harw-lens-embed` kennt — insbesondere
    /// [`QueryError::IndexNotVisible`], das [`crate::ask`] nie stillschweigend
    /// auf eine leere Trefferliste abbildet.
    #[from]
    Query(QueryError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lens_error_display_delegates_to_wrapped_query_error() {
        let err = LensError::Query(QueryError::IndexNotVisible {
            index_name: "knowledge.palace".to_owned(),
            visibility: "operator-only".to_owned(),
        });
        assert_eq!(
            err.to_string(),
            "selector requests index 'knowledge.palace' at visibility 'operator-only', which is outside the caller's read scope"
        );
    }

    #[test]
    fn test_lens_error_display_delegates_to_wrapped_source_error() {
        let err = LensError::Source(SourceError::EmbedderCountMismatch {
            expected: 3,
            actual: 1,
        });
        assert_eq!(
            err.to_string(),
            "embedder returned 1 vector(s) for 3 requested text(s)"
        );
    }

    #[test]
    fn test_lens_error_from_query_error_wraps_as_query_variant() {
        let inner = QueryError::EmptyEmbedding;
        let err: LensError = inner.into();
        assert!(matches!(err, LensError::Query(QueryError::EmptyEmbedding)));
    }

    #[test]
    fn test_lens_error_from_source_error_wraps_as_source_variant() {
        let inner = SourceError::MissingEmbedding {
            digest: "deadbeef".to_owned(),
        };
        let err: LensError = inner.into();
        assert!(matches!(
            err,
            LensError::Source(SourceError::MissingEmbedding { .. })
        ));
    }

    #[test]
    fn test_source_and_query_variants_are_distinguishable() {
        let source_err: LensError = SourceError::MissingEmbedding {
            digest: "deadbeef".to_owned(),
        }
        .into();
        let query_err: LensError = QueryError::EmptyEmbedding.into();
        assert!(matches!(source_err, LensError::Source(_)));
        assert!(matches!(query_err, LensError::Query(_)));
        assert!(!matches!(source_err, LensError::Query(_)));
    }
}
