//! Auflösung eines [`IndexSelector`] zu einem tatsächlichen, geladenen Index.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`resolve_index`]: die eine Stelle, an der ein
//! Sichtbarkeits-Selektor gegen den [`ReadScope`] des Aufrufers geprüft wird,
//! **bevor** irgendein Dateisystemzugriff stattfindet. Ein Selektor
//! außerhalb des Lesebereichs liefert
//! [`crate::QueryError::IndexNotVisible`] — niemals ein stilles Weglassen als
//! leere Trefferliste. Siehe die Moduldokumentation von `crate` für die
//! volle Begründung.
//!
//! # Nebenläufigkeit
//! [`resolve_index`] ist zustandslos zwischen Aufrufen; paralleler Zugriff
//! auf denselben oder unterschiedliche Indizes ist sicher (siehe
//! `harw_lens_store::LensStore`s Nebenläufigkeitsgarantien).
//!
//! # Fehler
//! Siehe [`crate::QueryError`] für die vollständige Variantenliste.
//!
//! # Examples
//! ```rust
//! use harw_lens_query::{resolve_index, IndexSelector, QueryError, ReadScope};
//!
//! let home = std::path::Path::new("/does/not/matter/for/this/check");
//! let selector = IndexSelector::new("knowledge.palace", "operator-only");
//! let scope = ReadScope::single("workspace");
//!
//! // Der Lesebereich verweigert die Sichtbarkeit -- der Fehler entsteht,
//! // bevor `home` überhaupt betrachtet wird.
//! let err = resolve_index(home, &selector, &scope).unwrap_err();
//! assert!(matches!(err, QueryError::IndexNotVisible { .. }));
//! ```

use std::path::Path;

use harw_lens_index::FlatIndex;
use harw_lens_store::LensStore;

use crate::error::QueryError;
use crate::selector::{IndexSelector, ReadScope};

/// Löst einen [`IndexSelector`] gegen einen [`ReadScope`] auf und lädt den
/// resultierenden [`FlatIndex`].
///
/// # Description
/// Prüft zuerst [`ReadScope::allows`] — **vor** jedem Dateisystemzugriff.
/// Erst wenn die Sichtbarkeit freigegeben ist, wird
/// [`harw_home::paths::visibility_index_dir`] aufgelöst, ein
/// [`harw_lens_store::LensStore`] darüber geöffnet und der benannte
/// [`FlatIndex`] geladen. Diese Reihenfolge ist die eigentliche
/// Sicherheitszusage dieser Funktion: ein zu weit gefasster Selektor scheitert
/// an der Prüfung, nicht erst (oder gar nicht) am fehlenden Index.
///
/// # Arguments
/// - `home` (`&Path`): der Root-Space, unter dem
///   [`harw_home::paths::visibility_index_dir`] aufgelöst wird.
/// - `selector` (`&IndexSelector`): welcher Index, welche Sichtbarkeit.
/// - `scope` (`&ReadScope`): welche Sichtbarkeiten der Aufrufer befragen
///   darf.
///
/// # Returns
/// Den geladenen [`FlatIndex`] für `selector`.
///
/// # Errors
/// - [`QueryError::IndexNotVisible`]: `selector.visibility` ist nicht in
///   `scope` freigegeben.
/// - [`QueryError::Home`]: `selector.visibility` ist als Sichtbarkeitsname
///   ungültig (Traversal-Schutz).
/// - [`QueryError::Store`]: der Sichtbarkeits-Store lässt sich nicht öffnen.
/// - [`QueryError::Index`]: kein Index namens `selector.index_name` in
///   diesem Sichtbarkeits-Store, oder ein Persistenzfehler beim Laden.
///
/// # Examples
/// Siehe die Moduldokumentation.
pub fn resolve_index(
    home: &Path,
    selector: &IndexSelector,
    scope: &ReadScope,
) -> Result<FlatIndex, QueryError> {
    if !scope.allows(&selector.visibility) {
        return Err(QueryError::IndexNotVisible {
            index_name: selector.index_name.clone(),
            visibility: selector.visibility.clone(),
        });
    }

    let store_root = harw_home::paths::visibility_index_dir(home, &selector.visibility)?;
    let store = LensStore::open(&store_root)?;
    let index = FlatIndex::load(&store, &selector.index_name)?;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_index_rejects_visibility_outside_scope_before_touching_disk() {
        // A path that cannot possibly exist: proves the rejection happens
        // before any filesystem access, not because the directory is missing.
        let home = Path::new("/definitely/does/not/exist/harw-lens-query-test");
        let selector = IndexSelector::new("knowledge.palace", "operator-only");
        let scope = ReadScope::single("workspace");

        let err = resolve_index(home, &selector, &scope).expect_err("must be rejected");
        assert!(matches!(
            err,
            QueryError::IndexNotVisible {
                ref index_name,
                ref visibility,
            } if index_name == "knowledge.palace" && visibility == "operator-only"
        ));
    }

    #[test]
    fn test_resolve_index_missing_index_within_allowed_scope_is_an_index_error() {
        let home = tempfile::tempdir().expect("tempdir");
        let selector = IndexSelector::new("does-not-exist", "workspace");
        let scope = ReadScope::single("workspace");

        let err = resolve_index(home.path(), &selector, &scope).expect_err("no such index");
        assert!(matches!(err, QueryError::Index(_)));
    }
}
