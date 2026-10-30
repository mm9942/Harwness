//! Die einzige echte Konfidenz-Konvertierung im Baum:
//! `harw_research::Confidence` → `harw_types::Confidence`.
//!
//! # Vier Typen namens `Confidence` — nur ein Konvertierungspartner
//! Der Baum trägt inzwischen vier `Confidence`-artige Typen. [`epistemic_confidence_for`]
//! verbindet genau zwei davon; die anderen zwei sind bewusst **keine**
//! Konvertierungspartner:
//!
//! - [`harw_research::Confidence`] (`Low`/`Medium`/`High`/`Verified`, vier
//!   Stufen) — misst, wie tief ein Rechercheergebnis gegen Primärquellen
//!   verifiziert wurde. Die Quellskala dieser Funktion.
//! - [`harw_types::Confidence`] (`VeryLow`..`VeryHigh`, fünf Stufen) — die
//!   eine geteilte, epistemische Konfidenzskala des Baums (Knoten AW0-03b).
//!   `harw_memory::epistemic::Confidence` ist seither nur noch ein
//!   Re-Export desselben Typs unter historischem Pfad; diese Funktion
//!   verwendet den kanonischen Pfad direkt. Das Zielformat dieser Funktion.
//! - `harw_model_catalog::provenance::Confidence` — derselbe Typ wie
//!   `harw_types::Confidence` unter drittem Namen (laut `harw_types`-Moduldoku
//!   byteidentisch in Form, Reihenfolge und Ableitung). Keine dritte
//!   Konvertierung nötig, weil es keine dritte Skala ist.
//! - [`crate::baseline::PalaceStatus`] (`Established`/`Provisional`/
//!   `Superseded`) — **kein** Konvertierungspartner. `PalaceStatus` ist ein
//!   **Lebenszyklus** (ein Knoten durchläuft ihn einmal, vorwärts, nie
//!   rückwärts), keine geordnete Konfidenz-*Skala* (bei der ein Wert schlicht
//!   „mehr“ Konfidenz als ein anderer ausdrückt). Eine Abbildung etwa
//!   `High → Established` wäre eine Kategorienverwechslung: sie unterstellte,
//!   eine Recherche-Konfidenzstufe könnte über die Reife *eines
//!   Wissensknotens* entscheiden, obwohl beide Typen fachlich nichts
//!   miteinander zu tun haben. `harw_types::confidence`-Moduldoku dokumentiert
//!   genau diese Verwechslungsgefahr bereits als „bereits einmal geplant,
//!   nie umgesetzt“ — dieses Modul fügt sie nicht nachträglich hinzu.
//!
//! # Warum die Abbildung so und nicht anders läuft
//! [`epistemic_confidence_for`] ist ordnungserhaltend und namenstreu, wo
//! beide Skalen einen Namen teilen (`Low`→`Low`, `Medium`→`Medium`,
//! `High`→`High`); nur `Verified` — die stärkste Stufe der Vier-Stufen-Skala,
//! stärker als bloßes `High` — bildet auf `VeryHigh` ab, nicht auf `High`,
//! damit der Unterschied zwischen „durch eine belastbare Quelle gestützt“
//! und „gegen eine Primärquelle verifiziert“ auf der Fünf-Stufen-Skala
//! erhalten bleibt. `VeryLow` bleibt von dieser Funktion unerreicht: die
//! Vier-Stufen-Skala kennt keine Stufe unterhalb von `Low`, und diese
//! Funktion erfindet keine.
//!
//! # Nebenläufigkeit
//! Reine, totale `fn` ohne inneren Zustand: `Send + Sync`.
//!
//! # Fehler
//! Keine.
//!
//! # Examples
//! ```rust
//! use harw_dod_rules::confidence::epistemic_confidence_for;
//! use harw_research::Confidence as ResearchConfidence;
//! use harw_types::Confidence as EpistemicConfidence;
//!
//! assert_eq!(
//!     epistemic_confidence_for(ResearchConfidence::Verified),
//!     EpistemicConfidence::VeryHigh
//! );
//! ```

use harw_research::Confidence as ResearchConfidence;
use harw_types::Confidence as EpistemicConfidence;

/// Bildet eine Recherche-Konfidenzstufe auf die geteilte, fünfstufige
/// epistemische Skala ab.
///
/// # Description
/// Siehe Moduldoku für die Begründung der Abbildung und dafür, warum genau
/// dieses Typpaar — und kein drittes — hier konvertiert wird.
///
/// # Arguments
/// - `confidence` (`harw_research::Confidence`): die zu übersetzende
///   Recherche-Konfidenzstufe.
///
/// # Returns
/// Die entsprechende [`harw_types::Confidence`]: `Low → Low`,
/// `Medium → Medium`, `High → High`, `Verified → VeryHigh`.
///
/// # Errors
/// Keine — die Abbildung ist total.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::confidence::epistemic_confidence_for;
/// use harw_research::Confidence as ResearchConfidence;
/// use harw_types::Confidence as EpistemicConfidence;
///
/// assert_eq!(
///     epistemic_confidence_for(ResearchConfidence::Low),
///     EpistemicConfidence::Low
/// );
/// ```
#[must_use]
pub fn epistemic_confidence_for(confidence: ResearchConfidence) -> EpistemicConfidence {
    match confidence {
        ResearchConfidence::Low => EpistemicConfidence::Low,
        ResearchConfidence::Medium => EpistemicConfidence::Medium,
        ResearchConfidence::High => EpistemicConfidence::High,
        ResearchConfidence::Verified => EpistemicConfidence::VeryHigh,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_maps_all_four_input_levels() {
        assert_eq!(
            epistemic_confidence_for(ResearchConfidence::Low),
            EpistemicConfidence::Low
        );
        assert_eq!(
            epistemic_confidence_for(ResearchConfidence::Medium),
            EpistemicConfidence::Medium
        );
        assert_eq!(
            epistemic_confidence_for(ResearchConfidence::High),
            EpistemicConfidence::High
        );
        assert_eq!(
            epistemic_confidence_for(ResearchConfidence::Verified),
            EpistemicConfidence::VeryHigh
        );
    }

    #[test]
    fn test_preserves_order() {
        let ascending = [
            ResearchConfidence::Low,
            ResearchConfidence::Medium,
            ResearchConfidence::High,
            ResearchConfidence::Verified,
        ];
        for pair in ascending.windows(2) {
            assert!(
                epistemic_confidence_for(pair[0]) < epistemic_confidence_for(pair[1]),
                "Ordnung verletzt für {:?} -> {:?}",
                pair[0],
                pair[1]
            );
        }
    }
}
