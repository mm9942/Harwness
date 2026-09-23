//! Relationsvorschläge zwischen Chunks — vorschlagen, nicht schreiben.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`suggest_relations`]. Diese Funktion **schreibt**
//! keinen `EdgeIndex` und keine Datei — sie gibt Tripel zurück, über die ein
//! Aufrufer (außerhalb dieser Crate) selbst entscheidet, ob sie in einen
//! `EdgeIndex` übernommen werden. Grund: eine automatisch gezogene Kante,
//! die niemand geprüft hat, wird beim Retrieval genauso behandelt wie eine
//! bestätigte — und eine falsche Kante (insbesondere eine, die eine
//! Aussage über *Inhalt* träfe, etwa „widerspricht sich") lässt beim
//! Entdoppeln (`collapse` in `harw-lens-rank`) genau die Fassung
//! verschwinden, die man behalten wollte. Deshalb schlägt diese Funktion
//! ausschließlich [`EdgeKind::References`] vor — nie [`EdgeKind::Contradicts`]
//! (eine reine Zerlegung kann keine Aussage über inhaltlichen Widerspruch
//! treffen) und nie eine „SupersededBy"-Kante (dieser Wert existiert nicht
//! einmal als [`EdgeKind`]-Variante — nachträglich könnte er es, deshalb
//! steht die Begründung dennoch hier: „supersedes" ist eine Aussage über
//! *welche von zwei Fassungen aktuell ist*, eine Information, die eine
//! Zerlegung strukturell nicht besitzt).
//!
//! # Nebenläufigkeit
//! [`suggest_relations`] ist eine reine Funktion ohne interne
//! Veränderlichkeit; sicher aus mehreren Threads parallel aufrufbar.
//!
//! # Fehler
//! Keine. Die Funktion gibt kein `Result` zurück; jede `&[Chunk]`-Eingabe
//! (auch leer oder mit einem einzigen Element) ist gültig.
//!
//! # Examples
//! ```rust
//! use harw_lens_chunk::{chunk_markdown, suggest_relations};
//! use harw_lens_types::{EdgeKind, SourceRef};
//!
//! let source = SourceRef::File { path: "doc.md".to_owned() };
//! let chunks = chunk_markdown(&source, "# A\ntext\n\n# B\nmore\n");
//! let edges = suggest_relations(&chunks);
//! assert!(edges.iter().all(|(_, _, kind)| matches!(kind, EdgeKind::References)));
//! ```

use harw_lens_types::{Chunk, ChunkDigest, EdgeKind};

/// Schlägt Relationen zwischen benachbarten Chunks derselben Quelle vor.
///
/// # Description
/// Zerlegungsregel: `chunks` wird in maximale, zusammenhängende Läufe
/// gleicher [`SourceRef`](harw_lens_types::SourceRef) aufgeteilt (in der
/// Reihenfolge, in der sie in `chunks` stehen — typischerweise genau die
/// Reihenfolge, die [`crate::chunk_markdown`], [`crate::chunk_rust`] oder
/// [`crate::chunk_plain`] liefern). Innerhalb jedes Laufs werden zwei
/// Vorschläge gemacht, beide als [`EdgeKind::References`]:
///
/// 1. **Aufeinanderfolgende Chunks derselben Quelle.** Jedes benachbarte
///    Paar im Lauf bekommt eine Kante — zwei Chunks, die im selben
///    Dokument unmittelbar aufeinanderfolgen, beziehen sich fast immer
///    aufeinander.
/// 2. **Anker-Chunk und seine Geschwister.** Der **erste** Chunk eines
///    Laufs gilt als Anker. Bei [`crate::chunk_rust`] ist das — nach dessen
///    Konvention, den Modulkopf-Chunk immer an Index 0 zu setzen — genau
///    der `//!`-Modulkopf, sofern einer existiert: „ein Rust-Element und
///    sein Modulkopf" wird über diese Regel zu „References" vorgeschlagen.
///    Weil [`Chunk`] selbst keine Diskriminante „ist Modulkopf" trägt,
///    nutzt diese Funktion die Positionskonvention statt einer
///    Typunterscheidung — für [`crate::chunk_markdown`]/[`crate::chunk_plain`]
///    ist dieselbe Regel ein harmloser Nebeneffekt (der erste Chunk eines
///    Dokuments referenziert dann auch dessen spätere, nicht direkt
///    benachbarte Chunks), ohne eigenständigen Bedeutungsanspruch dort.
///    Die direkt benachbarten Geschwister sind über Regel 1 bereits
///    abgedeckt; diese Regel ergänzt nur die nicht-benachbarten.
///
/// **Bewusst nicht vorgeschlagen:** [`EdgeKind::Contradicts`] (eine reine
/// Zerlegung trifft keine Aussage über inhaltlichen Widerspruch) und eine
/// „SupersededBy"-Kante (kein Zerlegungsergebnis kann wissen, welche von
/// zwei Fassungen die aktuelle ist — abgesehen davon existiert diese
/// Variante aktuell nicht einmal in [`EdgeKind`]).
///
/// # Arguments
/// - `chunks` (`&[Chunk]`): die Chunks, für die Relationen vorgeschlagen
///   werden — typischerweise das Ergebnis genau eines Aufrufs von
///   [`crate::chunk_markdown`], [`crate::chunk_rust`] oder
///   [`crate::chunk_plain`].
///
/// # Returns
/// `Vec<(ChunkDigest, ChunkDigest, EdgeKind)>` — Vorschläge, keine
/// geschriebenen Kanten. Ein Aufrufer entscheidet selbst, ob und wie er sie
/// in einen `EdgeIndex` übernimmt. Enthält nie [`EdgeKind::Contradicts`].
///
/// # Examples
/// ```rust
/// use harw_lens_chunk::{chunk_rust, suggest_relations};
/// use harw_lens_types::SourceRef;
///
/// let source = SourceRef::File { path: "src/lib.rs".to_owned() };
/// let text = "//! Kopf.\n\nfn a() {}\nfn b() {}\n";
/// let chunks = chunk_rust(&source, text);
/// let edges = suggest_relations(&chunks);
/// // Modulkopf (Index 0) referenziert auch den nicht direkt benachbarten
/// // letzten Chunk, nicht nur den unmittelbar folgenden.
/// assert!(edges.iter().any(|(a, b, _)| *a == chunks[0].digest && *b == chunks[2].digest));
/// ```
#[must_use]
pub fn suggest_relations(chunks: &[Chunk]) -> Vec<(ChunkDigest, ChunkDigest, EdgeKind)> {
    let mut out = Vec::new();
    let mut run_start = 0usize;

    for i in 1..=chunks.len() {
        let run_ends_here = i == chunks.len() || chunks[i].source != chunks[run_start].source;
        if !run_ends_here {
            continue;
        }

        let run = &chunks[run_start..i];
        for pair in run.windows(2) {
            out.push((pair[0].digest, pair[1].digest, EdgeKind::References));
        }
        if run.len() > 2 {
            let anchor = &run[0];
            for other in &run[2..] {
                out.push((anchor.digest, other.digest, EdgeKind::References));
            }
        }

        run_start = i;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_lens_types::{ByteSpan, SourceRef};
    use harw_types::ContentDigest;

    fn chunk(text: &str, source: SourceRef, start: usize) -> TestResult<Chunk> {
        Ok(Chunk {
            digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
            source,
            span: ByteSpan::new(start, start + text.len()).map_err(ctx("valid span"))?,
            text: text.to_owned(),
        })
    }

    fn file(path: &str) -> SourceRef {
        SourceRef::File {
            path: path.to_owned(),
        }
    }

    #[test]
    fn test_suggest_relations_empty_input_yields_no_edges() {
        assert_eq!(suggest_relations(&[]), Vec::new());
    }

    #[test]
    fn test_suggest_relations_single_chunk_yields_no_edges() -> TestResult {
        let chunks = vec![chunk("a", file("a.txt"), 0)?];
        assert_eq!(suggest_relations(&chunks), Vec::new());
        Ok(())
    }

    #[test]
    fn test_suggest_relations_links_consecutive_chunks_of_same_source() -> TestResult {
        let chunks = vec![chunk("a", file("a.txt"), 0)?, chunk("b", file("a.txt"), 1)?];
        let edges = suggest_relations(&chunks);
        assert_eq!(
            edges,
            vec![(chunks[0].digest, chunks[1].digest, EdgeKind::References)]
        );
        Ok(())
    }

    #[test]
    fn test_suggest_relations_does_not_link_chunks_of_different_sources() -> TestResult {
        let chunks = vec![chunk("a", file("a.txt"), 0)?, chunk("b", file("b.txt"), 0)?];
        assert_eq!(suggest_relations(&chunks), Vec::new());
        Ok(())
    }

    #[test]
    fn test_suggest_relations_anchor_links_to_non_adjacent_sibling() -> TestResult {
        let chunks = vec![
            chunk("head", file("src/lib.rs"), 0)?,
            chunk("fn a", file("src/lib.rs"), 4)?,
            chunk("fn b", file("src/lib.rs"), 8)?,
        ];
        let edges = suggest_relations(&chunks);
        // Adjacent pairs (0,1) and (1,2), plus the anchor-to-non-adjacent (0,2).
        assert_eq!(edges.len(), 3);
        assert!(edges.contains(&(chunks[0].digest, chunks[1].digest, EdgeKind::References)));
        assert!(edges.contains(&(chunks[1].digest, chunks[2].digest, EdgeKind::References)));
        assert!(edges.contains(&(chunks[0].digest, chunks[2].digest, EdgeKind::References)));
        Ok(())
    }

    #[test]
    fn test_suggest_relations_never_proposes_contradicts_or_superseded_by() -> TestResult {
        let chunks = vec![
            chunk("head", file("src/lib.rs"), 0)?,
            chunk("fn a", file("src/lib.rs"), 4)?,
            chunk("fn b", file("src/lib.rs"), 8)?,
            chunk("other doc", file("other.md"), 0)?,
        ];
        let edges = suggest_relations(&chunks);
        assert!(
            !edges.is_empty(),
            "sanity: this fixture should produce edges"
        );
        // `EdgeKind` has no `SupersededBy` variant at all, so it is
        // structurally impossible to propose one; this loop documents and
        // enforces the other half of the guarantee.
        for (_, _, kind) in &edges {
            assert!(
                matches!(kind, EdgeKind::References),
                "suggest_relations must never propose EdgeKind::Contradicts"
            );
        }
        Ok(())
    }

    #[test]
    fn test_suggest_relations_resets_anchor_per_source_run() -> TestResult {
        let chunks = vec![
            chunk("a1", file("a.txt"), 0)?,
            chunk("a2", file("a.txt"), 2)?,
            chunk("a3", file("a.txt"), 4)?,
            chunk("b1", file("b.txt"), 0)?,
            chunk("b2", file("b.txt"), 2)?,
            chunk("b3", file("b.txt"), 4)?,
        ];
        let edges = suggest_relations(&chunks);
        // No cross-source edges at all.
        for (a, b, _) in &edges {
            let a_chunk = chunks
                .iter()
                .find(|c| c.digest == *a)
                .ok_or(TestError::Missing("known digest"))?;
            let b_chunk = chunks
                .iter()
                .find(|c| c.digest == *b)
                .ok_or(TestError::Missing("known digest"))?;
            assert_eq!(a_chunk.source, b_chunk.source);
        }
        // Each run of 3 produces 2 adjacent + 1 anchor edge = 3 edges; two runs = 6.
        assert_eq!(edges.len(), 6);
        Ok(())
    }
}
