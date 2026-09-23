//! Budgets für Kontext-Sektionen.
//!
//! # Verantwortungsbereich
//! Besitzt [`ContextBudgetSpec`]: ein Gesamtbudget
//! ([`harw_lens_types::BudgetSpec`]) plus optionale Feinabstufungen je
//! Sektion. Diese Datei trifft keine Packentscheidung — welche Fragmente
//! ein Budget tatsächlich ausschöpfen, entscheidet `pack` in
//! `harw-lens-rank` bzw. `Assembly<...>` in `harw-core`.
//!
//! # Die eine Zusage: `tighten` kann nie weiten
//! [`ContextBudgetSpec::tighten`] ist ein punktweises Minimum über Gesamt-
//! und Sektionsbudgets. Das ist eine Typ-Eigenschaft, keine Konvention: es
//! gibt schlicht kein `widen` und keinen anderen öffentlichen Konstruktor,
//! der aus zwei Budgets ein größeres macht. Fehlt eine Sektion in einem der
//! beiden Budgets, überlebt sie mit dem Wert der Seite, die sie kennt — das
//! ist kein Sonderfall des Minimums, sondern die einzig sinnvolle Lesart:
//! ein fehlender Eintrag ist keine Null-Grenze, sondern „diese Seite äußert
//! keine eigene Meinung zu dieser Sektion".
//!
//! # Exportierte Typen
//! [`ContextBudgetSpec`].
//!
//! # Nebenläufigkeit
//! Reiner Werttyp ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Fehler
//! Keine — alle Operationen in dieser Datei sind total.
//!
//! # Examples
//! ```rust
//! use harw_context::{ContextBudgetSpec, SectionName};
//! use harw_lens_types::BudgetSpec;
//! use std::collections::BTreeMap;
//!
//! let mut per_section = BTreeMap::new();
//! per_section.insert(SectionName::try_new("history.tail").unwrap(), 100);
//!
//! let generous = ContextBudgetSpec { total: BudgetSpec { total: 1_000 }, per_section };
//! let strict = ContextBudgetSpec { total: BudgetSpec { total: 200 }, per_section: BTreeMap::new() };
//!
//! let tightened = generous.tighten(&strict);
//! assert_eq!(tightened.total.total, 200);
//! ```

use std::collections::BTreeMap;

use crate::fragment::SectionName;

/// Ein Budget je Sektion plus Gesamtbudget.
///
/// # Description
/// `total` begrenzt die Summe über alle Sektionen, `per_section` verfeinert
/// einzelne Sektionen zusätzlich. Eine Sektion ohne Eintrag in
/// `per_section` hat keine eigene Feinabstufung — ihre effektive Grenze ist
/// [`Self::section_budget`], das in diesem Fall auf `total.total`
/// zurückfällt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBudgetSpec {
    pub total: harw_lens_types::BudgetSpec,
    pub per_section: BTreeMap<SectionName, u32>,
}

impl ContextBudgetSpec {
    /// Punktweises Minimum über Gesamt- und Sektionsbudgets.
    ///
    /// # Description
    /// Fehlt eine Sektion in einem der beiden Budgets, gilt der vorhandene
    /// Wert der Seite, die sie kennt. **Es gibt kein `widen`:** ein Budget
    /// kann auf keinem Weg wachsen, und das ist eine Typ-Eigenschaft, keine
    /// Konvention (siehe Moduldokumentation).
    ///
    /// # Arguments
    /// - `other` (`&Self`): das zweite Budget, mit dem verengt wird.
    ///
    /// # Returns
    /// Ein neues `ContextBudgetSpec`, dessen `total` das Minimum beider
    /// Gesamtbudgets ist und dessen `per_section`-Einträge nie größer sind
    /// als der entsprechende Eintrag in `self` oder `other` (sofern
    /// vorhanden).
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::ContextBudgetSpec;
    /// use harw_lens_types::BudgetSpec;
    /// use std::collections::BTreeMap;
    ///
    /// let a = ContextBudgetSpec { total: BudgetSpec { total: 500 }, per_section: BTreeMap::new() };
    /// let b = ContextBudgetSpec { total: BudgetSpec { total: 300 }, per_section: BTreeMap::new() };
    /// assert_eq!(a.tighten(&b).total.total, 300);
    /// ```
    #[must_use]
    pub fn tighten(&self, other: &Self) -> Self {
        let total = self.total.tighten(other.total);

        let mut per_section = self.per_section.clone();
        for (section, other_value) in &other.per_section {
            per_section
                .entry(section.clone())
                .and_modify(|value| *value = (*value).min(*other_value))
                .or_insert(*other_value);
        }

        Self { total, per_section }
    }

    /// Die effektive Budgetgrenze für eine einzelne Sektion.
    ///
    /// # Arguments
    /// - `section` (`&SectionName`): die zu prüfende Sektion.
    ///
    /// # Returns
    /// Den Eintrag aus `per_section`, falls vorhanden; sonst `total.total`
    /// als Rückfallgrenze — eine Sektion ohne eigene Feinabstufung ist
    /// höchstens durch das Gesamtbudget begrenzt, nie unbegrenzt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::{ContextBudgetSpec, SectionName};
    /// use harw_lens_types::BudgetSpec;
    /// use std::collections::BTreeMap;
    ///
    /// let spec = ContextBudgetSpec { total: BudgetSpec { total: 500 }, per_section: BTreeMap::new() };
    /// let section = SectionName::try_new("plan.current").unwrap();
    /// assert_eq!(spec.section_budget(&section), 500);
    /// ```
    #[must_use]
    pub fn section_budget(&self, section: &SectionName) -> u32 {
        self.per_section
            .get(section)
            .copied()
            .unwrap_or(self.total.total)
    }
}

#[cfg(test)]
mod tests {
    use super::ContextBudgetSpec;
    use crate::fragment::SectionName;
    use crate::test_support::{TestResult, ctx};
    use harw_lens_types::BudgetSpec;
    use std::collections::BTreeMap;

    fn section(name: &str) -> TestResult<SectionName> {
        SectionName::try_new(name).map_err(ctx("valid section name"))
    }

    #[test]
    fn test_tighten_returns_minimum_per_shared_section() -> TestResult {
        let history = section("history.tail")?;

        let mut a_sections = BTreeMap::new();
        a_sections.insert(history.clone(), 100);
        let a = ContextBudgetSpec {
            total: BudgetSpec { total: 1_000 },
            per_section: a_sections,
        };

        let mut b_sections = BTreeMap::new();
        b_sections.insert(history.clone(), 40);
        let b = ContextBudgetSpec {
            total: BudgetSpec { total: 1_000 },
            per_section: b_sections,
        };

        let tightened = a.tighten(&b);
        assert_eq!(tightened.per_section.get(&history), Some(&40));
        Ok(())
    }

    #[test]
    fn test_tighten_keeps_section_present_only_in_one_side() -> TestResult {
        let only_in_a = section("plan.current")?;
        let only_in_b = section("history.tail")?;

        let mut a_sections = BTreeMap::new();
        a_sections.insert(only_in_a.clone(), 77);
        let a = ContextBudgetSpec {
            total: BudgetSpec { total: 1_000 },
            per_section: a_sections,
        };

        let mut b_sections = BTreeMap::new();
        b_sections.insert(only_in_b.clone(), 55);
        let b = ContextBudgetSpec {
            total: BudgetSpec { total: 1_000 },
            per_section: b_sections,
        };

        let tightened = a.tighten(&b);
        assert_eq!(tightened.per_section.get(&only_in_a), Some(&77));
        assert_eq!(tightened.per_section.get(&only_in_b), Some(&55));
        Ok(())
    }

    #[test]
    fn test_tighten_result_never_exceeds_either_input() -> TestResult {
        // Ein Alias statt eines vierfach geschachtelten Tupeltyps -- clippy
        // hat recht, dass die rohe Form nicht mehr lesbar ist.
        type SectionBudgets<'a> = Vec<(&'a str, u32)>;
        type TightenCase<'a> = (u32, u32, SectionBudgets<'a>, SectionBudgets<'a>);
        let cases: Vec<TightenCase<'_>> = vec![
            (1_000, 300, vec![("a", 50)], vec![("a", 200), ("b", 10)]),
            (10, 10, vec![], vec![("a", 0)]),
            (
                500,
                500,
                vec![("a", 500), ("b", 1)],
                vec![("a", 1), ("b", 500)],
            ),
        ];

        for (total_a, total_b, sections_a, sections_b) in cases {
            let a_per_section: BTreeMap<SectionName, u32> = sections_a
                .iter()
                .map(|(name, value)| section(name).map(|s| (s, *value)))
                .collect::<TestResult<BTreeMap<_, _>>>()?;
            let a = ContextBudgetSpec {
                total: BudgetSpec { total: total_a },
                per_section: a_per_section,
            };
            let b_per_section: BTreeMap<SectionName, u32> = sections_b
                .iter()
                .map(|(name, value)| section(name).map(|s| (s, *value)))
                .collect::<TestResult<BTreeMap<_, _>>>()?;
            let b = ContextBudgetSpec {
                total: BudgetSpec { total: total_b },
                per_section: b_per_section,
            };

            let tightened = a.tighten(&b);

            assert!(tightened.total.total <= total_a);
            assert!(tightened.total.total <= total_b);

            for (name, value) in &tightened.per_section {
                if let Some(a_value) = a.per_section.get(name) {
                    assert!(value <= a_value);
                }
                if let Some(b_value) = b.per_section.get(name) {
                    assert!(value <= b_value);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn test_section_budget_falls_back_to_total_when_section_absent() -> TestResult {
        let spec = ContextBudgetSpec {
            total: BudgetSpec { total: 500 },
            per_section: BTreeMap::new(),
        };
        assert_eq!(spec.section_budget(&section("plan.current")?), 500);
        Ok(())
    }

    #[test]
    fn test_section_budget_prefers_explicit_entry_over_total() -> TestResult {
        let mut per_section = BTreeMap::new();
        per_section.insert(section("history.tail")?, 40);
        let spec = ContextBudgetSpec {
            total: BudgetSpec { total: 500 },
            per_section,
        };
        assert_eq!(spec.section_budget(&section("history.tail")?), 40);
        Ok(())
    }
}
