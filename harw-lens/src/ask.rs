//! `ask`: stellt eine Frage an einen Index über die Fassade.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`ask`] — eine reine Weiterleitung an
//! [`harw_lens_query::query_scoped`], ohne eigene Logik. Diese Datei
//! implementiert weder Ähnlichkeitssuche noch Entdoppeln noch die
//! Sichtbarkeitsprüfung selbst; sie übersetzt nur den Fehlertyp
//! ([`harw_lens_query::QueryError`] zu [`crate::LensError`], über `#[from]`
//! in [`crate::LensError::Query`]).
//!
//! # Die zwei Auflagen dieses Knotens
//! 1. **`scope: &ReadScope` ist ein Pflichtparameter, keine Option.** Wer
//!    ihn vergessen könnte, hätte irgendwann ohne Sichtbarkeitsgrenze
//!    abgefragt. [`ask`] übernimmt dafür exakt
//!    [`harw_lens_query::query_scoped`]s Reihenfolge: `scope` steht direkt
//!    neben `selector`, nicht hinter einem `Option<ReadScope>` mit
//!    stillschweigendem „alles erlaubt"-Standard.
//! 2. **Ein Selektor außerhalb von `scope` liefert einen Fehler, nie eine
//!    leere Trefferliste.** [`ask`] reicht
//!    [`harw_lens_query::QueryError::IndexNotVisible`] unverändert durch
//!    (siehe [`crate::LensError::Query`]) — es gibt in dieser Datei keine
//!    Stelle, an der ein Fehler in `Ok(vec![])` umgewandelt wird.
//!
//! # Warum `edges`/`collapse_policy` explizite Parameter sind, kein
//! versteckter Standard
//! [`harw_lens_types::EdgeIndex`] leitet sich zum Zeitpunkt dieses
//! Ausbauprogramms **nicht automatisch** aus `harw-knowledge`-Daten ab —
//! `harw-lens-source`s Moduldokumentation (Abschnitt „Was diese Crate nicht
//! tut") begründet das ausführlich: es fehlt eine öffentliche Bindung
//! zwischen `PalaceNode` und dem tatsächlich persistierten Artefaktformat.
//! Würde [`ask`] intern still `EdgeIndex::default()` einsetzen, verschwiege
//! es genau diese Lücke — der Aufrufer bekäme ein Kollabieren ohne jede
//! `SupersededBy`/`Contradicts`-Kante, ohne je entscheiden zu können, ob das
//! gewollt ist. Aus demselben Grund bleibt `collapse_policy` explizit: nach
//! welcher Regel Duplikate zusammenfallen, ist eine Entscheidung, die dem
//! Aufrufer gehört, nicht dieser Fassade.
//!
//! # Nebenläufigkeit
//! [`ask`] hält keinen Zustand zwischen Aufrufen; sicher aus mehreren
//! Threads parallel aufrufbar, solange `embedder` es selbst ist (`Embedder`
//! ist `Send + Sync`, siehe `harw-lens-embed`).
//!
//! # Fehler
//! Siehe [`crate::LensError`].

use std::path::Path;

use harw_lens_embed::{Embedder, EmbeddingDescriptor};
use harw_lens_query::{query_scoped, IndexSelector, QueryProvenance, ReadScope};
use harw_lens_types::{CollapsePolicy, EdgeIndex, Ranked};

use crate::LensResult;

/// Stellt eine Frage an einen Index.
///
/// # Description
/// Reine Weiterleitung an [`harw_lens_query::query_scoped`]: prüft zuerst
/// `selector` gegen `scope` (vor jedem Dateisystemzugriff), lädt bei
/// Freigabe den benannten Index, bettet `question` über [`descriptor`]s
/// Abfrage-Präfix ein und lässt Duplikate im Ergebnis nach
/// `collapse_policy` über `edges` zusammenfallen. Diese Fassade fügt dabei
/// **nichts** hinzu — kein zweites Ranking, kein zweites Entdoppeln.
///
/// # Arguments
/// - `home` (`&Path`): der Root-Space, unter dem
///   `harw_home::paths::visibility_index_dir` aufgelöst wird.
/// - `selector` (`&IndexSelector`): welcher Index, welche Sichtbarkeit.
/// - `scope` (`&ReadScope`): welche Sichtbarkeiten der Aufrufer befragen
///   darf — Pflichtparameter, siehe den `//!`-Block dieses Moduls.
/// - `question` (`&str`): der rohe, unpräfixierte Abfragetext.
/// - `embedder` (`&dyn Embedder`): berechnet den Abfragevektor.
/// - `descriptor` (`&EmbeddingDescriptor`): liefert das Abfrage-Präfix.
/// - `provenance` (`&QueryProvenance`): **womit der Aufrufer eingebettet
///   hat** -- Modell und Zerlegungsfassung. Wird von
///   [`harw_lens_query::query`] gegen das Manifest des aufgelösten Index
///   geprüft, bevor gerechnet wird. Dieser Wert wird bewusst **nicht** aus
///   dem Index selbst abgeleitet: genau das war der Fehler der ersten
///   Fassung von `query` (`index.manifest().clone()`), der die Prüfung
///   tautologisch machte, weil ein Index nach Definition immer mit sich
///   selbst kompatibel ist. Ein `provenance`-Parameter, der aus dem Index
///   käme, könnte nie abweichen -- die dokumentierte Zusage, eine Abfrage
///   gegen ein abweichendes Modell abzulehnen, bliebe wirkungslos. Siehe
///   [`harw_lens_query::QueryProvenance`] für die vollständige Begründung.
/// - `edges` (`&EdgeIndex`): bekannte `SupersededBy`-/`Contradicts`-Kanten
///   zwischen Chunks — expliziter Parameter, siehe den `//!`-Block dieses
///   Moduls für die Begründung.
/// - `collapse_policy` (`CollapsePolicy`): wonach Duplikate erkannt werden.
/// - `limit` (`usize`): die maximale Anzahl an Treffern, **vor** dem
///   Kollabieren.
///
/// # Returns
/// Die gefundenen [`Ranked`]-Treffer, nach `collapse_policy` entdoppelt.
///
/// # Errors
/// - [`crate::LensError::Query`] — siehe
///   [`harw_lens_query::query_scoped`]/[`harw_lens_query::resolve_index`]
///   für die vollständige Variantenliste des gewickelten
///   [`harw_lens_query::QueryError`] — insbesondere
///   [`harw_lens_query::QueryError::IndexNotVisible`], wenn
///   `selector.visibility` nicht in `scope` freigegeben ist, und
///   [`harw_lens_query::QueryError::Index`], wenn `provenance` nicht zum
///   Manifest des aufgelösten Index passt (abweichendes Modell oder
///   abweichende Zerlegungsfassung). Keiner dieser Fälle liefert **jemals**
///   `Ok(vec![])`.
///
/// # Examples
/// ```rust,no_run
/// use harw_lens::{
///     ask, IndexSelector, QueryProvenance, ReadScope, CHUNKER_VERSION, DEFAULT_VISIBILITY,
///     DOCS_DESIGN_INDEX,
/// };
/// use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
/// use harw_lens_types::{CollapsePolicy, EdgeIndex};
///
/// let home = tempfile::tempdir()?;
/// let selector = IndexSelector::new(DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY);
/// let scope = ReadScope::single(DEFAULT_VISIBILITY);
/// let embedder = DeterministicEmbedder::new(16);
/// let descriptor = EmbeddingDescriptor {
///     document_prefix: "passage: ".to_owned(),
///     query_prefix: "query: ".to_owned(),
///     normalize: false,
/// };
/// let provenance = QueryProvenance {
///     model: "test-model".to_owned(),
///     chunker_version: CHUNKER_VERSION,
/// };
///
/// // Ohne vorherigen `build` existiert der Index nicht -- das liefert
/// // einen Fehler, keine leere Liste, aber illustriert die Signatur.
/// let _ = ask(
///     home.path(),
///     &selector,
///     &scope,
///     "wie funktioniert das?",
///     &embedder,
///     &descriptor,
///     &provenance,
///     &EdgeIndex::default(),
///     CollapsePolicy::ByDigest,
///     10,
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[allow(clippy::too_many_arguments)]
pub fn ask(
    home: &Path,
    selector: &IndexSelector,
    scope: &ReadScope,
    question: &str,
    embedder: &dyn Embedder,
    descriptor: &EmbeddingDescriptor,
    provenance: &QueryProvenance,
    edges: &EdgeIndex,
    collapse_policy: CollapsePolicy,
    limit: usize,
) -> LensResult<Vec<Ranked>> {
    let hits = query_scoped(
        home,
        selector,
        scope,
        question,
        embedder,
        descriptor,
        provenance,
        edges,
        collapse_policy,
        limit,
    )?;
    Ok(hits)
}
