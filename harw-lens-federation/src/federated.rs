//! Föderierte Abfrage über mehrere Indizes: eine Frage rein, eine Antwort raus.
//!
//! # Verantwortungsbereich
//! Besitzt [`federated_query`] und [`federated_pack`] — die einzigen beiden
//! Einstiegspunkte dieser Crate. [`federated_query`] löst mehrere
//! [`IndexSelector`] auf, fragt jeden abfragbaren Index über
//! [`harw_lens_query::query`] ab und verschmilzt die Ergebnisse über
//! [`harw_lens_rank::rrf_fuse`]. [`federated_pack`] füllt aus dem
//! verschmolzenen Ergebnis ein einziges Budget über [`harw_lens_rank::pack`].
//! Diese Datei implementiert **weder** RRF noch Budgetfüllung neu — sie ruft
//! beide vorhandenen Funktionen auf.
//!
//! # Warum Assembly und Lens dieselbe `pack`-Funktion aufrufen sollten
//! `harw_lens_rank::pack` ist nach `docs/aw-contract-master.md` (Abschnitt D)
//! der einzige echte Cross-Subsystem-Vertrag zwischen dem Retrieval-Layer
//! (Lens) und der Kontextmontage (`harw-core::Assembly`): beide füllen ein
//! Kostenbudget mit einer Rangliste. Riefe jede Seite eine eigene
//! Implementierung derselben Idee auf, drifteten beide unweigerlich
//! auseinander -- und die Drift wäre unsichtbar, weil beide Seiten weiterhin
//! eine plausible Auswahl liefern würden, nur eine andere. [`federated_pack`]
//! ist deshalb ein direkter, unveränderter Aufruf von [`harw_lens_rank::pack`]
//! (siehe `test_federated_pack_delegates_to_lens_rank_pack_byte_for_byte`
//! unten) -- die Hälfte des Vertrags, die diese Crate tatsächlich besitzt.
//!
//! **Eigene Prüfung, nicht nur behauptet:** Stand dieses Knotens ruft
//! `harw-core::context_budget::Assembly::budget` `harw_lens_rank::pack`
//! **nicht** auf. Der eigene `//!`-Block dieser Datei
//! (`harw-core/src/context_budget.rs`, Abschnitt „Warum `harw_lens_rank::pack`
//! nicht wiederverwendet wird") begründet das ausdrücklich und bewusst: `pack`
//! schätzt Kosten aus Text neu statt das bereits vorhandene `Fragment::cost`
//! zu übernehmen, und kennt nur ein flaches Gesamtbudget statt Budgets je
//! Sektion plus einen `must_include`-Hart-Fehler-Pfad. `Assembly::budget`
//! trägt deshalb eine eigene, mit derselben Grundidee neu geschriebene
//! Greedy-Füllung. Der in `docs/aw-contract-master.md` beschriebene Vertrag
//! „Assembly ruft dieselbe `pack`-Funktion wie Lens" hält also **heute noch
//! nicht vollständig** -- diese Crate kann das nicht selbst schließen, weil
//! `harw-core` außerhalb ihres Schreibbereichs liegt (ein anderer Knoten
//! arbeitet dort parallel an `turn_loop.rs`/`context_budget.rs`) und weil das
//! Schließen entweder eine reichere `pack`-Signatur (Sektionsbudgets,
//! `must_include`, vorab berechnete Kosten) oder eine bewusste Akzeptanz der
//! Divergenz verlangt -- beides eine Entscheidung für die Eigner von
//! `harw-lens-rank`/`harw-context`/`harw-core`, nicht für diesen Knoten
//! allein. Diese Datei hält den Befund fest, statt ihn zu verschweigen oder
//! die Aussage des Contract-Masters unbelegt zu wiederholen; siehe
//! `test_federated_pack_delegates_to_lens_rank_pack_byte_for_byte` für den
//! Teil des Vertrags, der tatsächlich gilt: **diese** Crate ruft `pack` direkt
//! auf, statt es zweitzuschreiben.
//!
//! # Die Sichtbarkeitsfrage: ein unsichtbarer Index bricht die ganze Anfrage ab
//! [`harw_lens_query::resolve_index`] prüft den [`ReadScope`] als erste
//! Handlung und liefert bei Verstoß
//! [`harw_lens_query::QueryError::IndexNotVisible`] -- nie `Ok(vec![])` (siehe
//! dessen Moduldokumentation für die volle Begründung: eine leere
//! Trefferliste ist von „nichts gefunden" nicht zu unterscheiden). Eine
//! Föderation über mehrere Indizes ist genau der Ort, an dem diese Trennung
//! kippen könnte: würde [`federated_query`] einen unsichtbaren Selektor
//! stillschweigend überspringen und mit den übrigen Indizes fortfahren, hielte
//! der Aufrufer eine **unvollständige** Antwort für eine **vollständige** --
//! er hat keine Möglichkeit, „drei von vier Indizes durchsucht" von „alle vier
//! durchsucht" zu unterscheiden. Diese Datei trifft deshalb dieselbe
//! Entscheidung wie `resolve_index` selbst: der erste unsichtbare Selektor
//! bricht `federated_query` mit einem Fehler ab, **bevor** irgendein anderer
//! Index befragt wird. Das ist die einzige Wahl, die mit `resolve_index`s
//! eigener Logik konsistent ist -- ein Aufrufer, der Teilergebnisse über
//! sichtbare Indizes will, muss `federated_query` selbst pro Sichtbarkeit
//! aufrufen und die Fehler seiner Selektoren einzeln behandeln.
//!
//! # Die Provenienzfrage über mehrere Indizes
//! `harw_lens_query::query` prüft eine übergebene [`QueryProvenance`] gegen
//! **einen** Indexmanifest und lehnt die gesamte Abfrage bei Abweichung ab
//! (siehe `QueryProvenance`s Moduldokumentation zu K43: die Provenienz muss
//! vom Aufrufer kommen, nicht aus dem durchsuchten Index abgeleitet werden,
//! sonst prüft der Vergleich den Index gegen sich selbst). Bei mehreren
//! Indizes gilt dieselbe Prüfung **verschärft**: eine Provenienz, viele
//! Indizes, und jeder einzelne muss unabhängig dagegen geprüft werden --
//! [`federated_query`] ruft [`harw_lens_types::IndexManifest::compatible_with`]
//! für **jeden** aufgelösten Index einzeln auf, bevor er ihn abfragt. Anders
//! als eine unsichtbare Sichtbarkeit (ein Autorisierungsverstoß, siehe oben)
//! ist ein abweichendes Modell kein Zugriffsverstoß, sondern ein
//! Datenintegritätsproblem: der Index selbst ist erlaubt, seine Treffer wären
//! in diesem Vektorraum aber Unsinn, der wie Treffer aussieht. Ein
//! inkompatibler Index wird deshalb nicht mitfusioniert (`skipped`), ohne die
//! gesamte Föderation abzubrechen -- die übrigen, kompatiblen Indizes liefern
//! weiterhin eine vollständige, nur eben um diesen einen Index verkleinerte
//! Antwort, und `skipped` macht diese Verkleinerung für den Aufrufer
//! sichtbar, statt sie zu verschweigen.
//!
//! # Determinismus
//! Gleiche Frage, gleiche Indizes, gleiche Provenienz -> gleiche Reihenfolge,
//! zweimal. [`rrf_fuse`] ist bereits reihenfolgeunabhängig über seine
//! Eingabelisten (siehe dessen Moduldokumentation). Diese Datei verlässt sich
//! trotzdem nicht auf eine zufällige oder Map-Iterationsreihenfolge über die
//! Selektoren: [`federated_query`] sortiert `selectors` ausdrücklich nach
//! `(index_name, visibility)`, **bevor** irgendein Index aufgelöst wird. Das
//! bestimmt sowohl die deterministische Reihenfolge von [`FederatedOutcome::queried`]
//! und [`FederatedOutcome::skipped`] als auch -- weil ein unsichtbarer
//! Selektor die gesamte Anfrage abbricht -- **welcher** Selektor bei mehreren
//! unsichtbaren Kandidaten den Fehler auslöst: immer derselbe, für dieselbe
//! Eingabemenge. Keine Systemuhr, keine Zufallszahl.
//!
//! # Nebenläufigkeit
//! [`federated_query`] und [`federated_pack`] halten keinen Zustand zwischen
//! Aufrufen; sicher aus mehreren Threads parallel aufrufbar, solange
//! `embedder` es selbst ist (`Send + Sync`, siehe `harw_lens_embed::Embedder`).
//!
//! # Fehler
//! Siehe [`crate::FederationError`] für die vollständige Variantenliste.

use std::path::Path;

use harw_lens_embed::{Embedder, EmbeddingDescriptor};
use harw_lens_index::VectorIndex;
use harw_lens_query::{query, resolve_index, IndexSelector, QueryProvenance, ReadScope};
use harw_lens_rank::{pack, rrf_fuse};
use harw_lens_types::{
    BudgetSpec, CollapsePolicy, CostEstimator, EdgeIndex, IndexManifest, LensTypesError, Packed,
    Ranked,
};

use crate::error::FederationError;

/// Warum ein Index von der Fusion ausgeschlossen wurde.
///
/// # Description
/// Geschlossen: aktuell entsteht ein Ausschluss ausschließlich über eine
/// abweichende [`QueryProvenance`] gegenüber dem Manifest des Index (siehe die
/// Moduldokumentation, Abschnitt „Die Provenienzfrage über mehrere Indizes").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// [`IndexManifest::compatible_with`] lehnte diesen Index gegenüber der
    /// übergebenen [`QueryProvenance`] ab.
    IncompatibleManifest {
        /// Der Name des abweichenden Feldes (`"model"` oder
        /// `"chunker_version"`), unverändert aus
        /// [`harw_lens_types::LensTypesError::ManifestMismatch`] übernommen.
        field: &'static str,
    },
}

/// Ein von der Fusion ausgeschlossener Index, samt Begründung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedIndex {
    /// Der ausgeschlossene Selektor.
    pub selector: IndexSelector,
    /// Warum er ausgeschlossen wurde.
    pub reason: SkipReason,
}

/// Das Ergebnis einer föderierten Abfrage über mehrere Indizes.
///
/// # Description
/// `fused` ist bereits über [`harw_lens_rank::rrf_fuse`] verschmolzen und
/// enthält damit auch die je Index bereits über
/// [`harw_lens_query::query`] entdoppelten Treffer. `queried` und `skipped`
/// zusammen decken **jeden** Eintrag von `selectors` genau einmal ab -- kein
/// Selektor verschwindet stillschweigend zwischen Aufruf und Ergebnis.
#[derive(Debug, Clone, PartialEq)]
pub struct FederatedOutcome {
    /// Die über alle abgefragten Indizes verschmolzene Rangliste.
    pub fused: Vec<Ranked>,
    /// Die tatsächlich abgefragten Selektoren, sortiert nach
    /// `(index_name, visibility)`.
    pub queried: Vec<IndexSelector>,
    /// Die ausgeschlossenen Selektoren, samt Begründung, in derselben
    /// Sortierung.
    pub skipped: Vec<SkippedIndex>,
}

/// Vergleicht zwei Selektoren nach `(index_name, visibility)` -- die
/// ausdrückliche Sortierung, auf der die Determinismus-Zusage dieser Datei
/// beruht (siehe Moduldokumentation, Abschnitt „Determinismus"). Kein
/// `derive(Ord)` auf [`IndexSelector`] selbst: das ist `harw-lens-query`s Typ,
/// und diese Sortierung ist eine Eigenschaft von `federated_query`, nicht des
/// Selektors an sich.
fn selector_sort_key(selector: &IndexSelector) -> (&str, &str) {
    (&selector.index_name, &selector.visibility)
}

/// Baut das Manifest, gegen das ein Index für **diese** Abfrage geprüft wird.
///
/// # Description
/// Übernimmt `model`/`chunker_version` aus der vom Aufrufer mitgebrachten
/// [`QueryProvenance`] -- niemals aus `index.manifest()` abgeleitet, aus
/// demselben Grund wie in `harw_lens_query::query` (K43: sonst prüfte der
/// Vergleich den Index gegen sich selbst und könnte nie fehlschlagen). Die
/// übrigen Felder werden von `index_manifest` übernommen, weil
/// `compatible_with` ohnehin nur `model`/`chunker_version` vergleicht.
fn provenance_manifest(index_manifest: &IndexManifest, provenance: &QueryProvenance) -> IndexManifest {
    IndexManifest {
        model: provenance.model.clone(),
        chunker_version: provenance.chunker_version,
        ..index_manifest.clone()
    }
}

/// Übersetzt das Ergebnis von [`IndexManifest::compatible_with`] in einen
/// [`SkipReason`].
///
/// # Description
/// `compatible_with` liefert nach seiner eigenen Implementierung
/// ausschließlich [`LensTypesError::ManifestMismatch`]; die beiden übrigen
/// Varianten von [`LensTypesError`] (`InvalidSpan`, `SpanOutOfBounds`) sind
/// bei diesem Aufruf strukturell unerreichbar (Muster: `harw-lens-index`s
/// `IndexError::from_manifest_check`).
fn skip_reason_from_manifest_check(err: LensTypesError) -> SkipReason {
    match err {
        LensTypesError::ManifestMismatch { field } => SkipReason::IncompatibleManifest { field },
        LensTypesError::InvalidSpan { .. } | LensTypesError::SpanOutOfBounds { .. } => {
            SkipReason::IncompatibleManifest { field: "unknown" }
        }
    }
}

/// Fragt mehrere Indizes ab und verschmilzt die Ergebnisse über RRF.
///
/// # Description
/// Sortiert `selectors` zuerst ausdrücklich (siehe Moduldokumentation,
/// Abschnitt „Determinismus"). Löst dann jeden Selektor über
/// [`resolve_index`] auf: ein Selektor außerhalb von `scope` bricht die
/// **gesamte** Anfrage sofort mit
/// [`harw_lens_query::QueryError::IndexNotVisible`] ab (Abschnitt „Die
/// Sichtbarkeitsfrage" oben). Für jeden aufgelösten Index wird `provenance`
/// gegen dessen Manifest geprüft ([`IndexManifest::compatible_with`]); ein
/// abweichender Index wird nicht abgefragt, sondern in `skipped` vermerkt
/// (Abschnitt „Die Provenienzfrage" oben). Jeder verbleibende Index wird über
/// [`query`] abgefragt (inklusive Kollabieren je Index); alle Ergebnislisten
/// werden anschließend über [`rrf_fuse`] verschmolzen.
///
/// # Arguments
/// - `home` (`&Path`): der Root-Space, unter dem jeder Selektor aufgelöst
///   wird.
/// - `selectors` (`&[IndexSelector]`): die zu befragenden Indizes.
/// - `scope` (`&ReadScope`): welche Sichtbarkeiten der Aufrufer befragen darf.
/// - `question` (`&str`): der rohe, unpräfixierte Abfragetext.
/// - `embedder` (`&dyn Embedder`): berechnet den Abfragevektor je Index.
/// - `descriptor` (`&EmbeddingDescriptor`): liefert das Abfrage-Präfix.
/// - `provenance` (`&QueryProvenance`): womit der Aufrufer eingebettet hat --
///   gegen **jeden** Index einzeln geprüft.
/// - `edges` (`&EdgeIndex`): bekannte Kanten, an [`query`] je Index
///   weitergereicht.
/// - `collapse_policy` (`CollapsePolicy`): wonach Duplikate je Index erkannt
///   werden.
/// - `limit_per_index` (`usize`): maximale Treffer je Index, vor der Fusion.
/// - `rrf_k` (`f32`): die RRF-Konstante, an [`rrf_fuse`] weitergereicht.
///
/// # Returns
/// [`FederatedOutcome`] mit der verschmolzenen Rangliste sowie den
/// tatsächlich abgefragten und den ausgeschlossenen Selektoren.
///
/// # Errors
/// - [`FederationError::Query`], gewickelt um
///   [`harw_lens_query::QueryError::IndexNotVisible`]: mindestens ein
///   Selektor liegt außerhalb von `scope`.
/// - [`FederationError::Query`], gewickelt um andere
///   [`harw_lens_query::QueryError`]-Varianten: siehe [`resolve_index`] und
///   [`query`] für die vollständige Liste.
///
/// # Examples
/// ```rust
/// use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
/// use harw_lens_federation::federated_query;
/// use harw_lens_query::{IndexSelector, QueryProvenance, ReadScope};
/// use harw_lens_types::{CollapsePolicy, EdgeIndex};
///
/// let home = std::path::Path::new("/does/not/matter/for/this/check");
/// let selectors = [IndexSelector::new("knowledge.palace", "operator-only")];
/// let scope = ReadScope::single("workspace");
/// let embedder = DeterministicEmbedder::new(8);
/// let descriptor = EmbeddingDescriptor {
///     document_prefix: "passage: ".to_owned(),
///     query_prefix: "query: ".to_owned(),
///     normalize: false,
/// };
/// let provenance = QueryProvenance { model: "m".to_owned(), chunker_version: 1 };
///
/// let err = federated_query(
///     home,
///     &selectors,
///     &scope,
///     "does it matter",
///     &embedder,
///     &descriptor,
///     &provenance,
///     &EdgeIndex::default(),
///     CollapsePolicy::ByDigest,
///     10,
///     60.0,
/// )
/// .unwrap_err();
/// assert!(err.to_string().contains("outside the caller's read scope"));
/// ```
#[allow(clippy::too_many_arguments)]
pub fn federated_query(
    home: &Path,
    selectors: &[IndexSelector],
    scope: &ReadScope,
    question: &str,
    embedder: &dyn Embedder,
    descriptor: &EmbeddingDescriptor,
    provenance: &QueryProvenance,
    edges: &EdgeIndex,
    collapse_policy: CollapsePolicy,
    limit_per_index: usize,
    rrf_k: f32,
) -> Result<FederatedOutcome, FederationError> {
    let mut sorted: Vec<IndexSelector> = selectors.to_vec();
    sorted.sort_by(|a, b| selector_sort_key(a).cmp(&selector_sort_key(b)));

    let mut queried = Vec::new();
    let mut skipped = Vec::new();
    let mut lists: Vec<Vec<Ranked>> = Vec::new();

    for selector in &sorted {
        // Sichtbarkeitsverstoss bricht sofort ab -- niemals ein
        // stillschweigend gekuerztes Ergebnis (siehe Moduldokumentation).
        let index = resolve_index(home, selector, scope)?;

        let expected = provenance_manifest(index.manifest(), provenance);
        if let Err(mismatch) = index.manifest().compatible_with(&expected) {
            skipped.push(SkippedIndex {
                selector: selector.clone(),
                reason: skip_reason_from_manifest_check(mismatch),
            });
            continue;
        }

        let hits = query(
            &index,
            question,
            embedder,
            descriptor,
            provenance,
            edges,
            collapse_policy,
            limit_per_index,
        )?;
        lists.push(hits);
        queried.push(selector.clone());
    }

    let fused = rrf_fuse(&lists, rrf_k);
    Ok(FederatedOutcome {
        fused,
        queried,
        skipped,
    })
}

/// Füllt ein Kostenbudget mit dem verschmolzenen Ergebnis einer föderierten
/// Abfrage.
///
/// # Description
/// Ein direkter, unveränderter Aufruf von [`harw_lens_rank::pack`] auf
/// `outcome.fused` -- siehe die Moduldokumentation, Abschnitt „Warum Assembly
/// und Lens dieselbe `pack`-Funktion aufrufen sollten", für die Begründung,
/// warum diese Funktion `pack` ruft statt es zweitzuschreiben, und für den
/// dokumentierten Befund, dass die Gegenseite (`harw-core::Assembly::budget`)
/// das heute noch nicht tut.
///
/// # Arguments
/// - `outcome` (`&FederatedOutcome`): das Ergebnis von [`federated_query`].
/// - `cost` (`&dyn CostEstimator`): schätzt die Kosten eines Kandidatentexts.
/// - `budget` (`&BudgetSpec`): das verfügbare Gesamtbudget.
///
/// # Returns
/// [`Packed`] mit den aufgenommenen Kandidaten, innerhalb von `budget`. Siehe
/// [`harw_lens_rank::pack`] für die vollständige Zusage.
///
/// # Panics
/// Keine.
///
/// # Examples
/// ```rust
/// use harw_lens_federation::{federated_pack, FederatedOutcome};
/// use harw_lens_types::BudgetSpec;
///
/// let outcome = FederatedOutcome { fused: Vec::new(), queried: Vec::new(), skipped: Vec::new() };
/// let packed = federated_pack(&outcome, &harw_lens_types::BytesOverFour, &BudgetSpec { total: 100 });
/// assert!(packed.selected.is_empty());
/// ```
#[must_use]
pub fn federated_pack(outcome: &FederatedOutcome, cost: &dyn CostEstimator, budget: &BudgetSpec) -> Packed {
    pack(&outcome.fused, cost, budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_types::{ByteSpan, BytesOverFour, Chunk, ChunkDigest, SourceRef};
    use harw_types::ContentDigest;

    fn ranked(text: &str, score: f32) -> Ranked {
        Ranked {
            chunk: Chunk {
                digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
                source: SourceRef::Artifact {
                    id: "a".to_owned(),
                },
                span: ByteSpan {
                    start: 0,
                    end: text.len(),
                },
                text: text.to_owned(),
            },
            score,
        }
    }

    /// Der Beleg für die halbe Zusage, die diese Crate tatsächlich einlösen
    /// kann: `federated_pack` reimplementiert das Budgetfuellen nicht, es
    /// ruft `harw_lens_rank::pack` direkt auf und liefert byteidentisch
    /// dasselbe Ergebnis wie ein direkter Aufruf mit denselben Argumenten.
    #[test]
    fn test_federated_pack_delegates_to_lens_rank_pack_byte_for_byte() {
        let candidates = vec![ranked("alpha", 1.0), ranked("beta", 0.5)];
        let outcome = FederatedOutcome {
            fused: candidates.clone(),
            queried: Vec::new(),
            skipped: Vec::new(),
        };
        let budget = BudgetSpec { total: 2 };

        let via_federation = federated_pack(&outcome, &BytesOverFour, &budget);
        let via_lens_rank_directly = pack(&candidates, &BytesOverFour, &budget);

        assert_eq!(via_federation, via_lens_rank_directly);
    }

    #[test]
    fn test_selector_sort_key_orders_by_index_name_then_visibility() {
        let a = IndexSelector::new("docs.design", "workspace");
        let b = IndexSelector::new("knowledge.palace", "operator-only");
        assert!(selector_sort_key(&a) < selector_sort_key(&b));
    }

    #[test]
    fn test_skip_reason_from_manifest_check_translates_manifest_mismatch() {
        let inner = LensTypesError::ManifestMismatch { field: "model" };
        let reason = skip_reason_from_manifest_check(inner);
        assert_eq!(reason, SkipReason::IncompatibleManifest { field: "model" });
    }
}
