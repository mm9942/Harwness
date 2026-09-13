//! Die Obergrenze dessen, was ein Agent überhaupt sehen darf.
//!
//! # Verantwortungsbereich
//! Besitzt [`ContextCeiling`] und [`CeilingViolation`]. Eine `ContextCeiling`
//! wird beim Handoff an ein Kind **im selben Schritt geschnitten** wie die
//! Berechtigungen (vgl. `harw_sandbox::PermissionSet` /
//! `harw_sandbox::NetworkScope`, deren Vorbild diese Datei folgt): ein Kind
//! kann seine Decke nie anheben, weil es keine Methode gibt, mit der es das
//! täte. Es gibt bewusst kein `union`, kein `widen`, keinen öffentlichen
//! Konstruktor, der zwei Decken zu einer größeren verschmilzt.
//!
//! # Exportierte Typen
//! [`ContextCeiling`], [`CeilingViolation`].
//!
//! # Nebenläufigkeit
//! Reine Werttypen ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Fehler
//! [`CeilingViolation`] — kein `#[derive(HarwError)]`-`...Error`-Namensmuster
//! (kein `ContextResult`-Alias, weil der Name nicht auf `Error` endet),
//! trotzdem mit `Display`/`std::error::Error` über dieselbe Ableitung wie
//! jeder andere Fehlertyp im Projekt.
//!
//! # Examples
//! ```rust
//! use harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass};
//! use harw_lens_types::BudgetSpec;
//! use std::collections::{BTreeMap, BTreeSet};
//!
//! let history = SectionName::try_new("history.tail").unwrap();
//! let mut sections = BTreeSet::new();
//! sections.insert(history.clone());
//!
//! let ceiling = ContextCeiling {
//!     sections,
//!     max_trust: TrustClass::Evidence,
//!     budget: ContextBudgetSpec { total: BudgetSpec { total: 1_000 }, per_section: BTreeMap::new() },
//! };
//!
//! let narrower = ceiling.intersect(&ceiling);
//! assert_eq!(narrower, ceiling);
//! ```

use std::collections::BTreeSet;

use crate::budget::ContextBudgetSpec;
use crate::fragment::{Fragment, SectionName, TrustClass};

/// Warum eine Decke ein Fragment ablehnt.
///
/// # Description
/// Eine Ablehnung ohne Grund ist wertlos für die Fehlersuche: jede Variante
/// nennt genau den einen der drei von [`ContextCeiling::admits`] geprüften
/// Aspekte, der verletzt wurde, samt der Werte, die zur Ablehnung führten.
#[derive(Debug, Clone, PartialEq, Eq, harw_macros::HarwError)]
pub enum CeilingViolation {
    /// Die Sektion des Fragments liegt nicht in [`ContextCeiling::sections`].
    #[msg("section '{section}' is not part of the ceiling's allowed sections")]
    SectionNotAllowed {
        /// Die Sektion des abgelehnten Fragments.
        section: SectionName,
    },

    /// [`crate::fragment::TrustClass::trust_rank`] des Fragments übersteigt
    /// den Rang von [`ContextCeiling::max_trust`].
    #[msg("fragment trust {trust:?} exceeds the ceiling's max_trust {max_trust:?}")]
    TrustExceeded {
        /// Die Vertrauensklasse des abgelehnten Fragments.
        trust: TrustClass,
        /// Die maximal zugelassene Vertrauensklasse der Decke.
        max_trust: TrustClass,
    },

    /// Die Kosten des Fragments übersteigen das effektive Sektionsbudget
    /// ([`ContextBudgetSpec::section_budget`]).
    #[msg("fragment cost {cost:?} exceeds the section budget of {budget} for section '{section}'")]
    OverBudget {
        /// Die Sektion des abgelehnten Fragments.
        section: SectionName,
        /// Die geschätzten Kosten des abgelehnten Fragments.
        cost: harw_lens_types::CostEstimate,
        /// Das effektive Budget der Sektion, das überschritten wurde.
        budget: u32,
    },
}

/// Die Obergrenze dessen, was ein Agent überhaupt sehen darf.
///
/// # Description
/// Wird beim Handoff an ein Kind im selben Schritt geschnitten wie die
/// Berechtigungen — ein Kind kann seine Decke nie anheben, weil es keine
/// API dafür gibt (siehe Moduldokumentation).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCeiling {
    pub sections: BTreeSet<SectionName>,
    pub max_trust: TrustClass,
    pub budget: ContextBudgetSpec,
}

impl ContextCeiling {
    /// Schnitt zweier Decken. Es gibt bewusst keine Vereinigung.
    ///
    /// # Arguments
    /// - `other` (`&Self`): die zweite Decke, mit der geschnitten wird.
    ///
    /// # Returns
    /// Eine neue `ContextCeiling`, deren `sections` die Schnittmenge beider
    /// Eingaben sind, deren `max_trust` der niedrigere der beiden Ränge
    /// (siehe [`TrustClass::trust_rank`]) ist und deren `budget` über
    /// [`ContextBudgetSpec::tighten`] verengt wurde. Das Ergebnis ist eine
    /// Teilmenge beider Eingaben in jeder der drei Dimensionen.
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::{ContextBudgetSpec, ContextCeiling, TrustClass};
    /// use harw_lens_types::BudgetSpec;
    /// use std::collections::BTreeMap;
    ///
    /// let parent = ContextCeiling {
    ///     sections: std::collections::BTreeSet::new(),
    ///     max_trust: TrustClass::Instruction,
    ///     budget: ContextBudgetSpec { total: BudgetSpec { total: 1_000 }, per_section: BTreeMap::new() },
    /// };
    /// let child = ContextCeiling {
    ///     sections: std::collections::BTreeSet::new(),
    ///     max_trust: TrustClass::Data,
    ///     budget: ContextBudgetSpec { total: BudgetSpec { total: 200 }, per_section: BTreeMap::new() },
    /// };
    /// let intersected = parent.intersect(&child);
    /// assert_eq!(intersected.max_trust, TrustClass::Data);
    /// assert_eq!(intersected.budget.total.total, 200);
    /// ```
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        let sections = self
            .sections
            .intersection(&other.sections)
            .cloned()
            .collect();

        let max_trust = if self.max_trust.trust_rank() <= other.max_trust.trust_rank() {
            self.max_trust
        } else {
            other.max_trust
        };

        let budget = self.budget.tighten(&other.budget);

        Self {
            sections,
            max_trust,
            budget,
        }
    }

    /// Lässt diese Decke das Fragment zu?
    ///
    /// # Description
    /// Prüft drei Aspekte in dieser Reihenfolge: liegt die Sektion in
    /// [`Self::sections`]; ist [`TrustClass::trust_rank`] des Fragments
    /// nicht höher als der Rang von [`Self::max_trust`]; passt
    /// `fragment.cost` in [`ContextBudgetSpec::section_budget`] für die
    /// Sektion des Fragments. Alle drei Grenzen sind inklusiv — ein
    /// Fragment, das exakt an einer Grenze liegt, wird zugelassen.
    ///
    /// # Arguments
    /// - `fragment` (`&Fragment`): das zu prüfende Fragment.
    ///
    /// # Returns
    /// `Ok(())`, wenn alle drei Aspekte erfüllt sind.
    ///
    /// # Errors
    /// - [`CeilingViolation::SectionNotAllowed`]: die Sektion des Fragments
    ///   ist nicht Teil von [`Self::sections`].
    /// - [`CeilingViolation::TrustExceeded`]: das Fragment ist
    ///   vertrauenswürdiger als [`Self::max_trust`] erlaubt.
    /// - [`CeilingViolation::OverBudget`]: die Kosten des Fragments
    ///   übersteigen das effektive Budget seiner Sektion.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_context::{ContextCeiling, Fragment};
    ///
    /// fn check(ceiling: &ContextCeiling, fragment: &Fragment) {
    ///     if let Err(violation) = ceiling.admits(fragment) {
    ///         eprintln!("fragment rejected: {violation}");
    ///     }
    /// }
    /// ```
    pub fn admits(&self, fragment: &Fragment) -> Result<(), CeilingViolation> {
        if !self.sections.contains(&fragment.section) {
            return Err(CeilingViolation::SectionNotAllowed {
                section: fragment.section.clone(),
            });
        }

        if fragment.trust.trust_rank() > self.max_trust.trust_rank() {
            return Err(CeilingViolation::TrustExceeded {
                trust: fragment.trust,
                max_trust: self.max_trust,
            });
        }

        let allowed = self.budget.section_budget(&fragment.section);
        if fragment.cost.0 > allowed {
            return Err(CeilingViolation::OverBudget {
                section: fragment.section.clone(),
                cost: fragment.cost,
                budget: allowed,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{CeilingViolation, ContextCeiling};
    use crate::budget::ContextBudgetSpec;
    use crate::fragment::{Fragment, FragmentLabel, FragmentOrigin, SectionName, Stability, TrustClass};
    use harw_lens_types::BudgetSpec;

    fn section(name: &str) -> SectionName {
        SectionName::try_new(name).unwrap()
    }

    fn ceiling(sections: &[&str], max_trust: TrustClass, per_section: &[(&str, u32)]) -> ContextCeiling {
        ContextCeiling {
            sections: sections.iter().map(|s| section(s)).collect(),
            max_trust,
            budget: ContextBudgetSpec {
                total: BudgetSpec { total: 1_000 },
                per_section: per_section
                    .iter()
                    .map(|(name, value)| (section(name), *value))
                    .collect(),
            },
        }
    }

    fn fragment(section_name: &str, trust: TrustClass, cost: u32) -> Fragment {
        Fragment {
            label: FragmentLabel::try_new("turn-1").unwrap(),
            section: section(section_name),
            trust,
            stability: Stability::Stable,
            origin: FragmentOrigin {
                provider: "harw-lens".to_owned(),
                namespace: "default".to_owned(),
                produced_at: jiff::Timestamp::UNIX_EPOCH,
            },
            cost: harw_lens_types::CostEstimate(cost),
            digest: harw_types::ContentDigest::of(b"fragment body"),
            body: "fragment body".to_owned(),
        }
    }

    #[test]
    fn test_intersect_is_commutative() {
        let a = ceiling(&["history.tail", "plan.current"], TrustClass::Instruction, &[("history.tail", 100)]);
        let b = ceiling(&["plan.current", "other"], TrustClass::Data, &[("plan.current", 40)]);

        assert_eq!(a.intersect(&b), b.intersect(&a));
    }

    #[test]
    fn test_intersect_is_idempotent() {
        let a = ceiling(&["history.tail"], TrustClass::Evidence, &[("history.tail", 100)]);
        assert_eq!(a.intersect(&a), a);
    }

    #[test]
    fn test_intersect_result_is_subset_of_both_inputs() {
        let a = ceiling(&["history.tail", "plan.current"], TrustClass::Instruction, &[("history.tail", 100)]);
        let b = ceiling(&["plan.current", "other"], TrustClass::Data, &[("plan.current", 40)]);

        let intersected = a.intersect(&b);

        assert!(intersected.sections.is_subset(&a.sections));
        assert!(intersected.sections.is_subset(&b.sections));
        assert!(intersected.max_trust.trust_rank() <= a.max_trust.trust_rank());
        assert!(intersected.max_trust.trust_rank() <= b.max_trust.trust_rank());
        assert!(intersected.budget.total.total <= a.budget.total.total);
        assert!(intersected.budget.total.total <= b.budget.total.total);
    }

    #[test]
    fn test_admits_rejects_section_not_in_ceiling() {
        let ceiling = ceiling(&["history.tail"], TrustClass::Instruction, &[]);
        let fragment = fragment("plan.current", TrustClass::Data, 1);

        assert!(matches!(
            ceiling.admits(&fragment),
            Err(CeilingViolation::SectionNotAllowed { .. })
        ));
    }

    #[test]
    fn test_admits_rejects_trust_above_max_trust() {
        let ceiling = ceiling(&["history.tail"], TrustClass::Data, &[]);
        let fragment = fragment("history.tail", TrustClass::Instruction, 1);

        assert!(matches!(
            ceiling.admits(&fragment),
            Err(CeilingViolation::TrustExceeded { .. })
        ));
    }

    #[test]
    fn test_admits_rejects_cost_over_section_budget() {
        let ceiling = ceiling(&["history.tail"], TrustClass::Instruction, &[("history.tail", 10)]);
        let fragment = fragment("history.tail", TrustClass::Instruction, 11);

        assert!(matches!(
            ceiling.admits(&fragment),
            Err(CeilingViolation::OverBudget { .. })
        ));
    }

    #[test]
    fn test_admits_accepts_fragment_exactly_at_all_three_boundaries() {
        let ceiling = ceiling(&["history.tail"], TrustClass::Evidence, &[("history.tail", 10)]);
        let fragment = fragment("history.tail", TrustClass::Evidence, 10);

        assert_eq!(ceiling.admits(&fragment), Ok(()));
    }

    #[test]
    fn test_context_ceiling_serde_roundtrip() {
        let ceiling = ceiling(&["history.tail"], TrustClass::Evidence, &[("history.tail", 10)]);
        let json = serde_json::to_string(&ceiling).expect("ceiling must serialize");
        let restored: ContextCeiling = serde_json::from_str(&json).expect("ceiling must deserialize");
        assert_eq!(ceiling, restored);
    }

    #[test]
    fn test_context_ceiling_deserialize_rejects_unknown_field() {
        let ceiling = ceiling(&["history.tail"], TrustClass::Evidence, &[]);
        let mut value = serde_json::to_value(&ceiling).expect("ceiling must serialize to value");
        value
            .as_object_mut()
            .expect("ceiling serializes to an object")
            .insert("unexpected".to_owned(), serde_json::json!(true));

        let result: Result<ContextCeiling, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }
}
