//! Brücke von der Katalog-[`ContextPolicy`] zu Kontextprogramm-Vorgaben.
//!
//! # Responsibility
//! Dieses Modul besitzt genau ein öffentliches Symbol: [`program_defaults_for`].
//! Es übersetzt die Kontextfensterstrategie, die der Katalog einem Modell
//! zuschreibt ([`ContextPolicy`] aus [`crate::runtime`]), in eine
//! [`harw_context::ContextBudgetSpec`] — die Vorgabe, mit der ein
//! Kontextprogramm arbeiten darf.
//!
//! # Die eine Zusage: nur verengen, nie weiten
//! Der Katalog sagt, was ein Modell mit einer gegebenen `ContextPolicy`
//! verträgt. Ein Kontextprogramm, das mehr verlangt, verspräche Kontext, den
//! das Modell nicht sicher aufnehmen kann — das ist genau der Fehler, den
//! [`harw_context::ContextBudgetSpec::tighten`] strukturell ausschließt: es
//! gibt kein `widen` und keinen anderen öffentlichen Konstruktor, der aus
//! zwei Budgets ein größeres macht (siehe `harw-context/src/budget.rs`).
//!
//! [`program_defaults_for`] macht sich dieselbe Struktur zu eigen, statt sie
//! nur zu versprechen: die drei Stufen werden nicht unabhängig als freie
//! Literale zurückgegeben, sondern von der großzügigsten Stufe
//! (`BroadContext`) absteigend über `tighten` erzeugt. Ein Tippfehler in
//! einer der rohen Stufen-Konstanten kann dadurch niemals eine höhere Stufe
//! übertreffen — die Rangfolge `TightSelect <= Balanced <= BroadContext`
//! ist eine Typ-Eigenschaft dieser Funktion, keine Konvention, die beim
//! nächsten Edit brechen könnte.
//!
//! # Warum genau ein Symbol
//! Zwei Wege von [`ContextPolicy`] zu Vorgaben wären zwei Stellen, an denen
//! die Grenze unterschiedlich gezogen werden könnte — und damit zwei
//! konkurrierende Antworten auf dieselbe Frage. Es gibt daher keine zweite
//! Hilfsfunktion, die dieselbe Übersetzung anders vornimmt; jeder Aufrufer,
//! der Katalog-Vorgaben für eine `ContextPolicy` braucht, geht über
//! [`program_defaults_for`].
//!
//! # Nebenläufigkeit
//! Rein funktional, kein Zustand: `Send + Sync` ohne Einschränkung.
//!
//! # Fehler
//! Keine — die Funktion ist für jede [`ContextPolicy`]-Variante total.
//!
//! # Examples
//! ```rust
//! use harw_model_catalog::context_defaults::program_defaults_for;
//! use harw_model_catalog::runtime::ContextPolicy;
//!
//! let tight = program_defaults_for(&ContextPolicy::TightSelect);
//! let broad = program_defaults_for(&ContextPolicy::BroadContext);
//! assert!(tight.total.total <= broad.total.total);
//! ```

use std::collections::BTreeMap;

use harw_context::ContextBudgetSpec;
use harw_lens_types::BudgetSpec;

use crate::runtime::ContextPolicy;

/// Rohvorgabe für `ContextPolicy::BroadContext` — die großzügigste Stufe.
///
/// Modelle mit nachgewiesener Long-Context-Stärke dürfen laut Katalog
/// (`runtime.rs`, Dokumentation von [`ContextPolicy::BroadContext`]) mit
/// großen, kohärenten Fenstern arbeiten. Dieser Wert ist die Obergrenze, von
/// der [`program_defaults_for`] alle anderen Stufen abwärts verengt — er
/// selbst wird gegen nichts verengt, weil er per Definition die weiteste
/// Vorgabe ist, die dieser Katalog jemals ausgibt.
/// 512k lässt große Repositories, Architektur-Notizen und mehrere relevante
/// Dateien im selben Arbeitskontext zu. Die effektive Obergrenze bleibt durch
/// das deklarierte Kontextfenster des gewählten Modells begrenzt.
const BROAD_CONTEXT_RAW_TOTAL: u32 = 512_000;

/// Rohvorgabe für `ContextPolicy::Balanced` — ausgewogenes Fenster.
///
/// Wird nie direkt zurückgegeben; [`program_defaults_for`] verengt die
/// `BroadContext`-Obergrenze auf diesen Wert per `tighten`, sodass
/// `Balanced` strukturell nie größer als `BroadContext` sein kann, selbst
/// wenn dieser Wert versehentlich zu groß gewählt würde.
const BALANCED_RAW_TOTAL: u32 = 128_000;

/// Rohvorgabe für `ContextPolicy::TightSelect` — enges, fokussiertes Fenster.
///
/// Wird nie direkt zurückgegeben; [`program_defaults_for`] verengt zuerst auf
/// [`BALANCED_RAW_TOTAL`] und dann auf diesen Wert, sodass `TightSelect`
/// strukturell nie größer als `Balanced` oder `BroadContext` sein kann.
const TIGHT_SELECT_RAW_TOTAL: u32 = 32_000;

/// Baut eine [`ContextBudgetSpec`] mit gegebenem Gesamtbudget und ohne
/// Sektions-Feinabstufung — der Katalog kennt keine konkreten Sektionsnamen,
/// das bleibt Sache des Kontextprogramms selbst.
fn total_only(total: u32) -> ContextBudgetSpec {
    ContextBudgetSpec {
        total: BudgetSpec { total },
        per_section: BTreeMap::new(),
    }
}

/// Übersetzt eine Katalog-[`ContextPolicy`] in ihre Kontextprogramm-Vorgabe.
///
/// # Description
/// Dies ist der einzige Weg von einer [`ContextPolicy`] zu einer
/// [`ContextBudgetSpec`] in diesem Crate (siehe Moduldokumentation, Abschnitt
/// „Warum genau ein Symbol"). Die drei Stufen werden nicht unabhängig
/// zurückgegeben, sondern strukturell verengt: `BroadContext` ist die rohe
/// Obergrenze; `Balanced` ist `BroadContext.tighten(&Balanced-Rohwert)`;
/// `TightSelect` ist `Balanced.tighten(&TightSelect-Rohwert)`. Da
/// [`harw_context::ContextBudgetSpec::tighten`] ein punktweises Minimum ohne
/// Gegenstück ist, kann das Ergebnis für keine Stufe größer ausfallen als
/// die jeweils großzügigere Stufe — unabhängig davon, wie die rohen
/// Stufen-Konstanten gepflegt werden.
///
/// Ein Kontextprogramm, das mehr Budget will, als diese Funktion für die
/// `ContextPolicy` des gewählten Modells vorgibt, muss sein Wunschbudget
/// über `program_defaults_for(policy).tighten(&wunsch)` führen — das
/// Ergebnis ist dann nie größer als die hier zurückgegebene Vorgabe.
///
/// # Arguments
/// - `policy` (`&ContextPolicy`): die Kontextfensterstrategie, die der
///   Katalog einem Modell zuschreibt (siehe `runtime::profile_for`).
///
/// # Returns
/// Eine [`ContextBudgetSpec`] ohne Sektions-Feinabstufung (`per_section` ist
/// leer): der Katalog äußert nur eine Meinung zum Gesamtbudget, keine zu
/// konkreten Sektionsnamen, die dem Kontextprogramm gehören.
///
/// # Panics
/// Keine. Die Funktion ist für jede [`ContextPolicy`]-Variante total.
///
/// # Concurrency
/// Rein funktional, kein Zustand. Thread-safe ohne Einschränkungen.
///
/// # Examples
/// ```rust
/// use harw_model_catalog::context_defaults::program_defaults_for;
/// use harw_model_catalog::runtime::ContextPolicy;
///
/// let tight = program_defaults_for(&ContextPolicy::TightSelect);
/// let balanced = program_defaults_for(&ContextPolicy::Balanced);
/// let broad = program_defaults_for(&ContextPolicy::BroadContext);
/// assert!(tight.total.total <= balanced.total.total);
/// assert!(balanced.total.total <= broad.total.total);
/// ```
#[must_use]
pub fn program_defaults_for(policy: &ContextPolicy) -> ContextBudgetSpec {
    let broad_context = total_only(BROAD_CONTEXT_RAW_TOTAL);

    match policy {
        ContextPolicy::BroadContext => broad_context,
        ContextPolicy::Balanced => broad_context.tighten(&total_only(BALANCED_RAW_TOTAL)),
        ContextPolicy::TightSelect => broad_context
            .tighten(&total_only(BALANCED_RAW_TOTAL))
            .tighten(&total_only(TIGHT_SELECT_RAW_TOTAL)),
    }
}

#[cfg(test)]
mod tests {
    use super::{program_defaults_for, total_only};
    use crate::runtime::ContextPolicy;
    use harw_context::ContextBudgetSpec;
    use harw_lens_types::BudgetSpec;

    /// Test 1: bekannte Politik liefert die erwarteten, dokumentierten Vorgaben.
    #[test]
    fn test_program_defaults_for_known_policies() {
        assert_eq!(
            program_defaults_for(&ContextPolicy::TightSelect)
                .total
                .total,
            32_000
        );
        assert_eq!(
            program_defaults_for(&ContextPolicy::Balanced).total.total,
            128_000
        );
        assert_eq!(
            program_defaults_for(&ContextPolicy::BroadContext)
                .total
                .total,
            512_000
        );
    }

    /// Test 2: die Rangfolge TightSelect <= Balanced <= BroadContext gilt strukturell.
    #[test]
    fn test_program_defaults_are_monotonic_across_policies() {
        let tight = program_defaults_for(&ContextPolicy::TightSelect);
        let balanced = program_defaults_for(&ContextPolicy::Balanced);
        let broad = program_defaults_for(&ContextPolicy::BroadContext);

        assert!(tight.total.total <= balanced.total.total);
        assert!(balanced.total.total <= broad.total.total);
    }

    /// Test 3 (Eigenschaft): für keine Politik und kein Wunschbudget übersteigt
    /// `catalog_default.tighten(&wunsch)` den Katalog-Default selbst — das ist
    /// die Zusage dieses Knotens, geprüft über mehrere Eingaben statt an einem
    /// Beispiel.
    #[test]
    fn test_tightening_against_catalog_default_never_exceeds_it() {
        let policies = [
            ContextPolicy::TightSelect,
            ContextPolicy::Balanced,
            ContextPolicy::BroadContext,
        ];
        let requested_totals = [0, 1, 32_000, 128_000, 512_000, 1_000_000, u32::MAX];

        for policy in policies {
            let catalog_default = program_defaults_for(&policy);
            for requested in requested_totals {
                let wanted = total_only(requested);
                let effective = catalog_default.tighten(&wanted);
                assert!(
                    effective.total.total <= catalog_default.total.total,
                    "effective budget {} exceeded catalog default {} for requested {}",
                    effective.total.total,
                    catalog_default.total.total,
                    requested
                );
            }
        }
    }

    /// Test 4: ein Programm, das mehr verlangt als der Katalog vorgibt, bekommt
    /// nach `tighten` exakt die Katalogvorgabe zurück, nicht sein Wunschbudget.
    #[test]
    fn test_greedy_program_receives_catalog_default_not_its_own_wish() {
        let catalog_default = program_defaults_for(&ContextPolicy::TightSelect);
        let greedy_wish = ContextBudgetSpec {
            total: BudgetSpec {
                total: catalog_default.total.total + 1_000_000,
            },
            per_section: std::collections::BTreeMap::new(),
        };

        let effective = catalog_default.tighten(&greedy_wish);
        assert_eq!(effective, catalog_default);
    }
}
