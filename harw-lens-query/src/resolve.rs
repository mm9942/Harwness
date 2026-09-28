//! Auflösung eines [`IndexSelector`] zu einem tatsächlichen, geladenen Index.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`resolve_index`]: die eine Stelle, an der ein
//! Sichtbarkeits-Selektor gegen den [`ReadScope`] des Aufrufers geprüft wird,
//! **bevor** irgendein Dateisystemzugriff stattfindet. Ein Selektor
//! außerhalb des Lesebereichs liefert
//! [`crate::QueryError::IndexNotVisible`] — niemals ein stilles Weglassen als
//! leere Trefferliste. Dieselbe Fehlervariante entsteht auch **nach** dem
//! Laden, wenn das geladene [`harw_lens_types::IndexManifest::visibility`]
//! nicht mit `selector.visibility` übereinstimmt: der Verzeichnispfad allein
//! (welcher Sichtbarkeits-Bucket geöffnet wurde) ist kein Beweis dafür, was
//! der geladene Index tatsächlich über sich selbst behauptet. Siehe die
//! Moduldokumentation von `crate` für die volle Begründung.
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

use harw_lens_index::{FlatIndex, VectorIndex};
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
/// Nach dem Laden prüft diese Funktion zusätzlich, dass das Manifest des
/// geladenen Index dieselbe Sichtbarkeit trägt wie `selector.visibility`.
/// Der Verzeichnispfad (`visibility_index_dir`) sagt nur, welcher Bucket
/// geöffnet wurde — nicht, dass der darin liegende Index wirklich zu diesem
/// Bucket gehört. Ein durch einen Build-Fehler oder eine manuelle
/// Dateiverschiebung falsch abgelegter Index würde sonst unter dem
/// freigegebenen Namen Inhalte einer anderen Sichtbarkeit ausliefern.
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
///   `scope` freigegeben, **oder** das geladene Manifest trägt eine andere
///   Sichtbarkeit als `selector.visibility` (falsch abgelegter Index).
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

    // Der Verzeichnispfad beweist nur, welcher Sichtbarkeits-Bucket geöffnet
    // wurde -- nicht, dass der darin geladene Index wirklich zu diesem
    // Bucket gehört. Ohne diese zweite Prüfung würde ein falsch abgelegter
    // oder verschobener Index (Build-Fehler, manuelles Kopieren) unter dem
    // freigegebenen Namen Inhalte einer anderen Sichtbarkeit ausliefern.
    if index.manifest().visibility != selector.visibility {
        return Err(QueryError::IndexNotVisible {
            index_name: selector.index_name.clone(),
            visibility: selector.visibility.clone(),
        });
    }

    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_lens_types::{IndexManifest, Locality, Metric};
    use harw_types::ContentDigest;

    /// Baut ein Manifest mit frei wählbarer `visibility`, sonst mit
    /// beliebigen, aber gültigen Werten -- diese Tests prüfen nur das
    /// Verhalten von [`resolve_index`] bezüglich der Sichtbarkeit, nicht das
    /// Manifest selbst.
    fn manifest_with_visibility(visibility: &str) -> IndexManifest {
        IndexManifest {
            model: "test-model".to_owned(),
            locality: Locality::Local,
            chunker_version: 1,
            visibility: visibility.to_owned(),
            metric: Metric::Cosine,
            source_set_digest: ContentDigest::of(b"sources"),
        }
    }

    #[test]
    fn test_resolve_index_rejects_visibility_outside_scope_before_touching_disk() -> TestResult {
        // A path that cannot possibly exist: proves the rejection happens
        // before any filesystem access, not because the directory is missing.
        let home = Path::new("/definitely/does/not/exist/harw-lens-query-test");
        let selector = IndexSelector::new("knowledge.palace", "operator-only");
        let scope = ReadScope::single("workspace");

        let Err(err) = resolve_index(home, &selector, &scope) else {
            return Err(TestError::Unexpected("must be rejected".into()));
        };
        assert!(matches!(
            err,
            QueryError::IndexNotVisible {
                ref index_name,
                ref visibility,
            } if index_name == "knowledge.palace" && visibility == "operator-only"
        ));
        Ok(())
    }

    #[test]
    fn test_resolve_index_missing_index_within_allowed_scope_is_an_index_error() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let selector = IndexSelector::new("does-not-exist", "workspace");
        let scope = ReadScope::single("workspace");

        let Err(err) = resolve_index(home.path(), &selector, &scope) else {
            return Err(TestError::Unexpected("no such index".into()));
        };
        assert!(matches!(err, QueryError::Index(_)));
        Ok(())
    }

    #[test]
    fn test_resolve_index_accepts_index_whose_manifest_visibility_matches_the_bucket()
    -> TestResult {
        // The counter test to the mismatch case below: without it, a
        // rejection there could also mean `resolve_index` rejects every
        // load, not specifically a mismatching manifest.
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store_root = harw_home::paths::visibility_index_dir(home.path(), "workspace")
            .map_err(ctx("visibility_index_dir"))?;
        let store = LensStore::open(&store_root).map_err(ctx("open store"))?;
        let index = FlatIndex::build(manifest_with_visibility("workspace"), Vec::new())
            .map_err(ctx("build empty index"))?;
        index
            .save(&store, "knowledge.palace")
            .map_err(ctx("save index"))?;

        let selector = IndexSelector::new("knowledge.palace", "workspace");
        let scope = ReadScope::single("workspace");

        resolve_index(home.path(), &selector, &scope).map_err(ctx("must be accepted"))?;
        Ok(())
    }

    #[test]
    fn test_resolve_index_rejects_index_whose_manifest_visibility_does_not_match_the_bucket()
    -> TestResult {
        // An index saved into the "workspace" bucket, but whose own manifest
        // claims "operator-only" -- as could happen through a build bug or a
        // manual file move. The directory alone must not decide the answer:
        // `resolve_index` has to reject this under the authorized name
        // rather than serve content of another visibility.
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store_root = harw_home::paths::visibility_index_dir(home.path(), "workspace")
            .map_err(ctx("visibility_index_dir"))?;
        let store = LensStore::open(&store_root).map_err(ctx("open store"))?;
        let index = FlatIndex::build(manifest_with_visibility("operator-only"), Vec::new())
            .map_err(ctx("build empty index"))?;
        index
            .save(&store, "knowledge.palace")
            .map_err(ctx("save index"))?;

        let selector = IndexSelector::new("knowledge.palace", "workspace");
        let scope = ReadScope::single("workspace");

        let Err(err) = resolve_index(home.path(), &selector, &scope) else {
            return Err(TestError::Unexpected(
                "an index whose manifest visibility does not match the authorized bucket must be rejected"
                    .into(),
            ));
        };
        assert!(matches!(
            err,
            QueryError::IndexNotVisible {
                ref index_name,
                ref visibility,
            } if index_name == "knowledge.palace" && visibility == "workspace"
        ));
        Ok(())
    }
}
