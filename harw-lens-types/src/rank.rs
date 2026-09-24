//! Rangvokabular für `harw-lens-types`.
//!
//! # Verantwortungsbereich
//! Besitzt die Typen, gegen die `harw-lens-rank` (AW0-09, vier reine
//! Funktionen: `rrf_fuse`, `mmr`, `collapse`, `pack`) und `harw-context`
//! (AW0-04, `Fragment::cost`) arbeiten: [`Ranked`], [`CostEstimate`],
//! [`CostEstimator`], [`BytesOverFour`], [`BudgetSpec`], [`CollapsePolicy`],
//! [`EdgeKind`], [`EdgeIndex`] und [`Packed`]. Diese Typen liegen bewusst
//! hier und nicht in `harw-context`: `pack` aus `harw-lens-rank` nennt sie,
//! und `harw-context` ruft `pack` auf — lägen sie in `harw-context`,
//! definierten Kontext und Lens einander gegenseitig.
//!
//! # Verbindliche Signaturen
//! `CostEstimator::estimate`, `BudgetSpec::tighten`, `Ranked` und `Packed`
//! sind zwischen AW0-08 (dieser Crate) und AW0-04/AW0-09 (parallele
//! Konsumenten) eingefroren. Eine Änderung dieser Signaturen bricht die
//! parallel laufende Implementierung dieser Knoten.
//!
//! # Nebenläufigkeit
//! [`CostEstimator`] ist `Send + Sync`: Kostenschätzer werden über
//! `&dyn CostEstimator` geteilt und aus mehreren Threads aufgerufen. Alle
//! übrigen Typen dieses Moduls sind reine Daten ohne interne
//! Veränderlichkeit; [`EdgeIndex`] wird über `&mut self` befüllt und danach
//! nur noch gelesen.
//!
//! Contract-Master Abschnitt C (AW0-08, `docs/design/build-history.md`).

use std::collections::{HashMap, HashSet};

use crate::chunk::{Chunk, ChunkDigest};

/// Ein Kandidat mit Rangwert.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    /// Der bewertete Chunk.
    pub chunk: Chunk,
    /// Der Rangwert. Höher heißt relevanter; die Interpretationshoheit über
    /// die Skala liegt bei der Funktion, die diesen Wert gesetzt hat.
    pub score: f32,
}

/// Was ein Kandidat kostet.
///
/// # Description
/// Eine Einheit ohne festgelegte Bedeutung außerhalb der Vereinbarung
/// zwischen einem [`CostEstimator`] und dem [`BudgetSpec`], gegen das
/// `pack` (`harw-lens-rank`) das Ergebnis prüft. Für [`BytesOverFour`] ist
/// eine Einheit vier Bytes Text.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct CostEstimate(pub u32);

/// Schätzt die Kosten eines Textes.
///
/// # Concurrency
/// `Send + Sync`: Implementierungen werden hinter `&dyn CostEstimator`
/// geteilt und aus mehreren Threads aufgerufen. `estimate` nimmt `&self` und
/// darf keinen veränderlichen Zustand über den Aufruf hinaus führen.
pub trait CostEstimator: Send + Sync {
    /// Die geschätzten Kosten von `text`.
    ///
    /// # Arguments
    /// - `text` (`&str`): der zu bewertende Text.
    ///
    /// # Returns
    /// Die geschätzten Kosten als [`CostEstimate`].
    fn estimate(&self, text: &str) -> CostEstimate;
}

/// Vier Bytes je Einheit. Die billige, deterministische Voreinstellung.
#[derive(Debug, Clone, Copy, Default)]
pub struct BytesOverFour;

impl CostEstimator for BytesOverFour {
    fn estimate(&self, text: &str) -> CostEstimate {
        CostEstimate(u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX))
    }
}

/// Ein Kostenbudget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetSpec {
    /// Die Gesamtkosten, die nicht überschritten werden dürfen.
    pub total: u32,
}

impl BudgetSpec {
    /// Punktweises Minimum. Es gibt bewusst kein `widen`.
    ///
    /// # Description
    /// Ein Budget kann auf keinem Weg wachsen, wenn es mit einem anderen
    /// verschärft wird — das ist eine Typ-Eigenschaft dieser Methode, keine
    /// Konvention der Aufrufer.
    ///
    /// # Arguments
    /// - `other` (`BudgetSpec`): das Budget, gegen das verschärft wird.
    ///
    /// # Returns
    /// Ein neues `BudgetSpec` mit `total = self.total.min(other.total)`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_types::BudgetSpec;
    ///
    /// let a = BudgetSpec { total: 100 };
    /// let b = BudgetSpec { total: 40 };
    /// assert_eq!(a.tighten(b), BudgetSpec { total: 40 });
    /// ```
    #[must_use]
    pub fn tighten(self, other: Self) -> Self {
        Self {
            total: self.total.min(other.total),
        }
    }
}

/// Wie Duplikate zusammenfallen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollapsePolicy {
    /// Zwei Kandidaten mit identischem [`ChunkDigest`] gelten als Duplikat.
    ByDigest,
    /// Zwei Kandidaten mit identischer Quelle und identischem Byte-Bereich
    /// gelten als Duplikat, unabhängig vom Digest.
    BySourceAndSpan,
}

/// Art einer Kante zwischen zwei Chunks.
///
/// # Description
/// Geschlossen: eine neue Kantenart ist eine Entscheidung, kein freier
/// String. [`EdgeKind::Contradicts`] trägt eine harte Garantie: `collapse`
/// (`harw-lens-rank`) darf zwei Chunks, die über eine `Contradicts`-Kante
/// verbunden sind, niemals zu einem Kandidaten zusammenfallen lassen — sonst
/// verschwindet einer von zwei sich widersprechenden Befunden stillschweigend
/// aus dem Kontext.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// Zwei Chunks beziehen sich auf denselben Sachverhalt, ohne Widerspruch.
    /// Ungerichtet.
    References,
    /// Zwei Chunks widersprechen sich inhaltlich. Muss beim Entdoppeln
    /// (`collapse`) erhalten bleiben: diese Kante darf nie kollabieren.
    /// Ungerichtet.
    Contradicts,
    /// Der erste Chunk wurde vom zweiten abgelöst. **Gerichtet** — und das
    /// ist der Grund, warum diese Art nicht im ungerichteten Kantensatz
    /// liegt: verlöre sie ihre Richtung, könnte `collapse` auf die *ältere*
    /// Fassung zusammenfallen und damit genau das Gegenteil dessen tun,
    /// wofür es die Kante gibt.
    SupersededBy,
}

/// Kanten zwischen Chunks, für die Relationsexpansion.
///
/// # Description
/// Ungerichtet gespeichert: eine eingefügte Kante zwischen `a` und `b` ist
/// von `a` nach `b` und von `b` nach `a` gleichermaßen sichtbar, weil weder
/// „bezieht sich auf" noch „widerspricht" eine Richtung hat.
#[derive(Debug, Clone, Default)]
pub struct EdgeIndex {
    /// Ungerichtete Kanten: `References` und `Contradicts`. Kanonisch nach
    /// Digest-Ordnung normalisiert, damit `(a, b)` und `(b, a)` dieselbe
    /// Kante sind.
    edges: HashSet<(ChunkDigest, ChunkDigest, EdgeKind)>,
    /// Gerichtete Ablösungen: Schlüssel ist der **abgelöste** (ältere) Chunk,
    /// Wert der **ablösende** (neuere).
    ///
    /// Getrennt gehalten, weil die Richtung hier bedeutungstragend ist. Läge
    /// die Ablösung im ungerichteten Satz, könnte `collapse` nicht
    /// entscheiden, welche der beiden Fassungen überlebt — und die falsche
    /// Wahl löscht genau die neuere.
    superseded: HashMap<ChunkDigest, ChunkDigest>,
}

impl EdgeIndex {
    /// Baut einen leeren Kantenindex.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fügt eine ungerichtete Kante zwischen `a` und `b` ein.
    ///
    /// # Arguments
    /// - `a` / `b` (`ChunkDigest`): die verbundenen Chunks. Reihenfolge ist
    ///   ohne Bedeutung.
    /// - `kind` (`EdgeKind`): die Art der Beziehung.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_types::{ChunkDigest, EdgeIndex, EdgeKind};
    /// use harw_digest::ContentDigest;
    ///
    /// let a = ChunkDigest(ContentDigest::of(b"a"));
    /// let b = ChunkDigest(ContentDigest::of(b"b"));
    /// let mut edges = EdgeIndex::new();
    /// edges.insert(a, b, EdgeKind::Contradicts);
    /// assert!(edges.contradicts(a, b));
    /// assert!(edges.contradicts(b, a));
    /// ```
    pub fn insert(&mut self, a: ChunkDigest, b: ChunkDigest, kind: EdgeKind) {
        match kind {
            // Gerichtet: `a` wurde von `b` abgeloest.
            EdgeKind::SupersededBy => {
                self.superseded.insert(a, b);
            }
            EdgeKind::References | EdgeKind::Contradicts => {
                self.edges.insert(Self::normalize(a, b, kind));
            }
        }
    }

    /// Baut einen Kantenindex aus zwei Listen.
    ///
    /// # Description
    /// Der bequeme Konstruktor fuer Aufrufer, die ihre Kanten bereits
    /// gesammelt haben — insbesondere `collapse` in `harw-lens-rank` und
    /// dessen Tests.
    ///
    /// # Arguments
    /// - `superseded` (`(alt, neu)`): gerichtete Abloesungen. Das **erste**
    ///   Element ist der abgeloeste, das zweite der abloesende Chunk. Die
    ///   Reihenfolge ist hier bedeutungstragend; wer sie vertauscht, laesst
    ///   `collapse` auf die aeltere Fassung zusammenfallen.
    /// - `contradicts` (`(a, b)`): ungerichtete Widersprueche.
    ///
    /// # Returns
    /// Ein `EdgeIndex` mit genau diesen Kanten.
    #[must_use]
    pub fn from_edges(
        superseded: impl IntoIterator<Item = (ChunkDigest, ChunkDigest)>,
        contradicts: impl IntoIterator<Item = (ChunkDigest, ChunkDigest)>,
    ) -> Self {
        let mut index = Self::new();
        for (old, new) in superseded {
            index.insert(old, new, EdgeKind::SupersededBy);
        }
        for (a, b) in contradicts {
            index.insert(a, b, EdgeKind::Contradicts);
        }
        index
    }

    /// Der Chunk, der `chunk` abgeloest hat — falls es einen gibt.
    ///
    /// # Description
    /// Der eine gerichtete Zugriff des Index. `collapse` fragt damit: „darf
    /// dieser Kandidat zugunsten eines anderen wegfallen, und wenn ja,
    /// zugunsten welches?"
    ///
    /// # Arguments
    /// - `chunk` (`ChunkDigest`): der moeglicherweise abgeloeste Chunk.
    ///
    /// # Returns
    /// `Some(neuer)` wenn eine Abloesung eingetragen ist, sonst `None`.
    #[must_use]
    pub fn superseded_by(&self, chunk: ChunkDigest) -> Option<ChunkDigest> {
        self.superseded.get(&chunk).copied()
    }

    /// Prüft, ob `a` und `b` über eine Kante der Art `kind` verbunden sind.
    ///
    /// # Arguments
    /// - `a` / `b` (`ChunkDigest`): die zu prüfenden Chunks. Reihenfolge ist
    ///   ohne Bedeutung.
    /// - `kind` (`EdgeKind`): die gesuchte Kantenart.
    ///
    /// # Returns
    /// `true`, wenn eine passende Kante eingefügt wurde.
    #[must_use]
    pub fn has_edge(&self, a: ChunkDigest, b: ChunkDigest, kind: EdgeKind) -> bool {
        match kind {
            // Gerichtet: nur `a` abgeloest von `b` zaehlt, nicht umgekehrt.
            EdgeKind::SupersededBy => self.superseded.get(&a) == Some(&b),
            EdgeKind::References | EdgeKind::Contradicts => {
                self.edges.contains(&Self::normalize(a, b, kind))
            }
        }
    }

    /// Prüft speziell auf eine [`EdgeKind::Contradicts`]-Kante zwischen `a`
    /// und `b`.
    ///
    /// # Description
    /// Der einzige Aufruf, den `collapse` (`harw-lens-rank`) vor dem
    /// Zusammenfallen zweier Kandidaten braucht: sagt `true`, verbietet das
    /// Kollabieren dieses Paares.
    ///
    /// # Arguments
    /// - `a` / `b` (`ChunkDigest`): die zu prüfenden Chunks. Reihenfolge ist
    ///   ohne Bedeutung.
    ///
    /// # Returns
    /// `true`, wenn `a` und `b` über eine `Contradicts`-Kante verbunden sind.
    #[must_use]
    pub fn contradicts(&self, a: ChunkDigest, b: ChunkDigest) -> bool {
        self.has_edge(a, b, EdgeKind::Contradicts)
    }

    /// Bringt ein Kantentripel in eine kanonische, reihenfolgeunabhängige
    /// Form.
    fn normalize(
        a: ChunkDigest,
        b: ChunkDigest,
        kind: EdgeKind,
    ) -> (ChunkDigest, ChunkDigest, EdgeKind) {
        if a <= b { (a, b, kind) } else { (b, a, kind) }
    }
}

/// Das Ergebnis von `pack`.
#[derive(Debug, Clone, PartialEq)]
pub struct Packed {
    /// Die ausgewählten Kandidaten, innerhalb des Budgets.
    pub selected: Vec<Ranked>,
    /// Die tatsächlich verbrauchten Kosten der ausgewählten Kandidaten.
    pub spent: CostEstimate,
    /// Anzahl der Kandidaten, die wegen Budgetüberschreitung nicht
    /// ausgewählt wurden.
    pub dropped: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ByteSpan, SourceRef};
    use crate::test_support::{TestResult, ctx};
    use harw_digest::ContentDigest;

    fn sample_chunk(text: &str) -> TestResult<Chunk> {
        Ok(Chunk {
            digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
            source: SourceRef::File {
                path: "a.txt".to_owned(),
            },
            span: ByteSpan::new(0, text.len()).map_err(ctx("valid span"))?,
            text: text.to_owned(),
        })
    }

    #[test]
    fn test_bytes_over_four_rounds_up() {
        assert_eq!(BytesOverFour.estimate("ab"), CostEstimate(1));
        assert_eq!(BytesOverFour.estimate("abcd"), CostEstimate(1));
        assert_eq!(BytesOverFour.estimate("abcde"), CostEstimate(2));
    }

    #[test]
    fn test_bytes_over_four_empty_text_is_free() {
        assert_eq!(BytesOverFour.estimate(""), CostEstimate(0));
    }

    #[test]
    fn test_cost_estimate_orders_by_inner_value() {
        assert!(CostEstimate(1) < CostEstimate(2));
    }

    #[test]
    fn test_budget_spec_tighten_takes_pointwise_minimum() {
        let a = BudgetSpec { total: 100 };
        let b = BudgetSpec { total: 40 };
        assert_eq!(a.tighten(b), BudgetSpec { total: 40 });
        assert_eq!(b.tighten(a), BudgetSpec { total: 40 });
    }

    #[test]
    fn test_budget_spec_tighten_is_idempotent_on_equal_budgets() {
        let a = BudgetSpec { total: 50 };
        assert_eq!(a.tighten(a), a);
    }

    #[test]
    fn test_edge_index_contradicts_is_symmetric() {
        let a = ChunkDigest(ContentDigest::of(b"a"));
        let b = ChunkDigest(ContentDigest::of(b"b"));
        let mut edges = EdgeIndex::new();
        edges.insert(a, b, EdgeKind::Contradicts);

        assert!(edges.contradicts(a, b));
        assert!(edges.contradicts(b, a));
    }

    #[test]
    fn test_edge_index_references_does_not_count_as_contradicts() {
        let a = ChunkDigest(ContentDigest::of(b"a"));
        let b = ChunkDigest(ContentDigest::of(b"b"));
        let mut edges = EdgeIndex::new();
        edges.insert(a, b, EdgeKind::References);

        assert!(!edges.contradicts(a, b));
        assert!(edges.has_edge(a, b, EdgeKind::References));
    }

    #[test]
    fn test_edge_index_unrelated_pair_has_no_edge() {
        let a = ChunkDigest(ContentDigest::of(b"a"));
        let b = ChunkDigest(ContentDigest::of(b"b"));
        let c = ChunkDigest(ContentDigest::of(b"c"));
        let mut edges = EdgeIndex::new();
        edges.insert(a, b, EdgeKind::Contradicts);

        assert!(!edges.contradicts(a, c));
        assert!(!edges.contradicts(b, c));
    }

    #[test]
    fn test_ranked_clone_and_equality() -> TestResult {
        let ranked = Ranked {
            chunk: sample_chunk("hello")?,
            score: 0.5,
        };
        assert_eq!(ranked.clone(), ranked);
        Ok(())
    }

    #[test]
    fn test_packed_equality_over_selected_spent_dropped() -> TestResult {
        let packed_a = Packed {
            selected: vec![Ranked {
                chunk: sample_chunk("hello")?,
                score: 1.0,
            }],
            spent: CostEstimate(2),
            dropped: 1,
        };
        let packed_b = Packed {
            selected: packed_a.selected.clone(),
            spent: CostEstimate(2),
            dropped: 1,
        };
        assert_eq!(packed_a, packed_b);
        Ok(())
    }
}
