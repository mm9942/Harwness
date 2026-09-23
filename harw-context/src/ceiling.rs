//! Die Obergrenze dessen, was ein Agent überhaupt sehen darf.
//!
//! # Verantwortungsbereich
//! Besitzt [`ContextCeiling`] und [`CeilingViolation`]. Eine `ContextCeiling`
//! wird beim Handoff an ein Kind **im selben Schritt geschnitten** wie die
//! Berechtigungen (vgl. `harw_authority::PermissionSet` /
//! `harw_authority::NetworkScope`, deren Vorbild diese Datei folgt): ein Kind
//! kann seine Decke nie anheben, weil es keine Methode gibt, mit der es das
//! täte. Es gibt bewusst kein `union`, kein `widen`, keinen öffentlichen
//! Konstruktor, der zwei Decken zu einer größeren verschmilzt.
//!
//! # Exportierte Typen
//! [`ContextCeiling`], [`CeilingViolation`], die Konstanten
//! [`ROOT_CONTEXT_SECTIONS`] (Obergrenze jeder lokal-vertrauten Wurzeldecke)
//! und [`LEGACY_V1_SECTION`] (Sektion der v1-Brücke, W3 C-PROTO / F-163).
//!
//! # Kosten
//! [`ContextCeiling::admits`] vertraut `Fragment::cost` nicht blind: die
//! effektiven Kosten sind nie kleiner als `ceil(body.len() / 4)` (F-147).
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

/// Section every v1 context fragment is bridged into.
///
/// # Description
/// `harw_extension_api::fragment_from_v1` assigns this section to every
/// legacy `ContextFragment`. It lives here, next to [`ROOT_CONTEXT_SECTIONS`],
/// so the bridge and the root ceiling cannot drift apart again (finding
/// F-163: the bridge used `legacy.v1`, no root ceiling contained it, and every
/// production fragment was omitted as `BelowCeiling` in child sessions).
pub const LEGACY_V1_SECTION: &str = "legacy.v1";

/// Upper bound of sections a locally trusted root session may expose.
///
/// # Description
/// The single source of truth for root ceilings (W3 C-PROTO, F-163/F-144 in
/// the remediation plan). Entry points build their root
/// [`ContextCeiling::sections`] from this list instead of private literals:
///
/// - `task.objective`: the child's assignment; without it no child can work.
/// - `task.read_scope`: the read restriction handed to the child.
/// - `new.trigger_return`: the return protocol of worker definitions.
/// - `history.tail`: the rendered conversation tail (same literal as
///   `harw_core::HISTORY_TAIL_SECTION`; `harw-context` cannot depend on
///   `harw-core`, the equality is asserted in `harw-core`'s tests).
/// - [`LEGACY_V1_SECTION`]: data from v1 context providers. These fragments are
///   always `TrustClass::Data`, so admitting the section never lets content
///   reach the instruction block.
///
/// Deliberately absent: `credential.*`, `secret.*`, full or sibling
/// transcripts, `plan.current`, `web.fetch_allowlist`, and every other
/// provider section. A ceiling is an upper bound, not a wish list.
///
/// # Examples
/// ```rust
/// use harw_context::ceiling::{LEGACY_V1_SECTION, ROOT_CONTEXT_SECTIONS};
///
/// assert!(ROOT_CONTEXT_SECTIONS.contains(&LEGACY_V1_SECTION));
/// assert!(ROOT_CONTEXT_SECTIONS.contains(&"history.tail"));
/// ```
pub const ROOT_CONTEXT_SECTIONS: &[&str] = &[
    "task.objective",
    "task.read_scope",
    "new.trigger_return",
    "history.tail",
    LEGACY_V1_SECTION,
];

/// Lower bound for the cost of a fragment, derived from its body.
///
/// # Description
/// `Fragment::cost` is reported by the provider. A provider declaring
/// `cost: 0` for a large body would bypass every budget check (finding F-147).
/// The effective cost is therefore never below `ceil(body.len() / 4)` — the
/// same unit as `harw_lens_types::BytesOverFour`. Rendering overhead (header,
/// fences) is added by the renderer in `harw-core`, which knows the format.
fn effective_cost(fragment: &Fragment) -> u32 {
    let body_floor = u32::try_from(fragment.body.len().div_ceil(4)).unwrap_or(u32::MAX);
    fragment.cost.0.max(body_floor)
}

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
    /// nicht höher als der Rang von [`Self::max_trust`]; passen die
    /// effektiven Kosten in [`ContextBudgetSpec::section_budget`] für die
    /// Sektion des Fragments. Effektive Kosten sind
    /// `max(fragment.cost, ceil(body.len() / 4))` — ein Provider kann sein
    /// Budget nicht durch zu niedrig gemeldete Kosten umgehen (F-147). Alle drei Grenzen sind inklusiv — ein
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
        let cost = effective_cost(fragment);
        if cost > allowed {
            return Err(CeilingViolation::OverBudget {
                section: fragment.section.clone(),
                cost: harw_lens_types::CostEstimate(cost),
                budget: allowed,
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{CeilingViolation, ContextCeiling, LEGACY_V1_SECTION, ROOT_CONTEXT_SECTIONS};
    use crate::budget::ContextBudgetSpec;
    use crate::fragment::{
        Fragment, FragmentLabel, FragmentOrigin, SectionName, Stability, TrustClass,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_lens_types::BudgetSpec;

    fn section(name: &str) -> TestResult<SectionName> {
        SectionName::try_new(name).map_err(ctx("valid section name"))
    }

    fn ceiling(
        sections: &[&str],
        max_trust: TrustClass,
        per_section: &[(&str, u32)],
    ) -> TestResult<ContextCeiling> {
        Ok(ContextCeiling {
            sections: sections
                .iter()
                .map(|s| section(s))
                .collect::<TestResult<_>>()?,
            max_trust,
            budget: ContextBudgetSpec {
                total: BudgetSpec { total: 1_000 },
                per_section: per_section
                    .iter()
                    .map(|(name, value)| section(name).map(|s| (s, *value)))
                    .collect::<TestResult<_>>()?,
            },
        })
    }

    fn fragment(section_name: &str, trust: TrustClass, cost: u32) -> TestResult<Fragment> {
        Ok(Fragment {
            label: FragmentLabel::try_new("turn-1").map_err(ctx("valid fragment label"))?,
            section: section(section_name)?,
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
        })
    }

    #[test]
    fn test_intersect_is_commutative() -> TestResult {
        let a = ceiling(
            &["history.tail", "plan.current"],
            TrustClass::Instruction,
            &[("history.tail", 100)],
        )?;
        let b = ceiling(
            &["plan.current", "other"],
            TrustClass::Data,
            &[("plan.current", 40)],
        )?;

        assert_eq!(a.intersect(&b), b.intersect(&a));
        Ok(())
    }

    #[test]
    fn test_intersect_is_idempotent() -> TestResult {
        let a = ceiling(
            &["history.tail"],
            TrustClass::Evidence,
            &[("history.tail", 100)],
        )?;
        assert_eq!(a.intersect(&a), a);
        Ok(())
    }

    #[test]
    fn test_intersect_result_is_subset_of_both_inputs() -> TestResult {
        let a = ceiling(
            &["history.tail", "plan.current"],
            TrustClass::Instruction,
            &[("history.tail", 100)],
        )?;
        let b = ceiling(
            &["plan.current", "other"],
            TrustClass::Data,
            &[("plan.current", 40)],
        )?;

        let intersected = a.intersect(&b);

        assert!(intersected.sections.is_subset(&a.sections));
        assert!(intersected.sections.is_subset(&b.sections));
        assert!(intersected.max_trust.trust_rank() <= a.max_trust.trust_rank());
        assert!(intersected.max_trust.trust_rank() <= b.max_trust.trust_rank());
        assert!(intersected.budget.total.total <= a.budget.total.total);
        assert!(intersected.budget.total.total <= b.budget.total.total);
        Ok(())
    }

    #[test]
    fn test_admits_rejects_section_not_in_ceiling() -> TestResult {
        let ceiling = ceiling(&["history.tail"], TrustClass::Instruction, &[])?;
        let fragment = fragment("plan.current", TrustClass::Data, 1)?;

        assert!(matches!(
            ceiling.admits(&fragment),
            Err(CeilingViolation::SectionNotAllowed { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_admits_rejects_trust_above_max_trust() -> TestResult {
        let ceiling = ceiling(&["history.tail"], TrustClass::Data, &[])?;
        let fragment = fragment("history.tail", TrustClass::Instruction, 1)?;

        assert!(matches!(
            ceiling.admits(&fragment),
            Err(CeilingViolation::TrustExceeded { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_admits_rejects_cost_over_section_budget() -> TestResult {
        let ceiling = ceiling(
            &["history.tail"],
            TrustClass::Instruction,
            &[("history.tail", 10)],
        )?;
        let fragment = fragment("history.tail", TrustClass::Instruction, 11)?;

        assert!(matches!(
            ceiling.admits(&fragment),
            Err(CeilingViolation::OverBudget { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_admits_accepts_fragment_exactly_at_all_three_boundaries() -> TestResult {
        let ceiling = ceiling(
            &["history.tail"],
            TrustClass::Evidence,
            &[("history.tail", 10)],
        )?;
        let fragment = fragment("history.tail", TrustClass::Evidence, 10)?;

        assert_eq!(ceiling.admits(&fragment), Ok(()));
        Ok(())
    }

    #[test]
    fn test_admits_rejects_under_declared_cost_via_body_floor() -> TestResult {
        // Body "fragment body" is 13 bytes => floor ceil(13/4) = 4 units.
        let ceiling = ceiling(
            &["history.tail"],
            TrustClass::Instruction,
            &[("history.tail", 3)],
        )?;
        let fragment = fragment("history.tail", TrustClass::Data, 0)?;

        match ceiling.admits(&fragment) {
            Err(CeilingViolation::OverBudget { cost, budget, .. }) => {
                assert_eq!(cost.0, 4);
                assert_eq!(budget, 3);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "a cost-0 claim must not bypass the section budget, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_root_context_sections_are_valid_unique_and_contain_legacy_v1() {
        let mut seen = std::collections::BTreeSet::new();
        for name in ROOT_CONTEXT_SECTIONS {
            assert!(
                SectionName::try_new(*name).is_ok(),
                "{name} must be a valid section name"
            );
            assert!(seen.insert(*name), "{name} must appear only once");
        }
        assert!(ROOT_CONTEXT_SECTIONS.contains(&LEGACY_V1_SECTION));
        assert!(ROOT_CONTEXT_SECTIONS.contains(&"history.tail"));
        for forbidden in [
            "credential.tokens",
            "secret.values",
            "plan.current",
            "web.fetch_allowlist",
        ] {
            assert!(
                !ROOT_CONTEXT_SECTIONS.contains(&forbidden),
                "{forbidden} must stay outside"
            );
        }
    }

    #[test]
    fn test_root_context_sections_ceiling_admits_legacy_v1_data_fragment() -> TestResult {
        let ceiling = ceiling(ROOT_CONTEXT_SECTIONS, TrustClass::Instruction, &[])?;
        let fragment = fragment(LEGACY_V1_SECTION, TrustClass::Data, 4)?;

        assert_eq!(ceiling.admits(&fragment), Ok(()));
        Ok(())
    }

    #[test]
    fn test_context_ceiling_serde_roundtrip() -> TestResult {
        let ceiling = ceiling(
            &["history.tail"],
            TrustClass::Evidence,
            &[("history.tail", 10)],
        )?;
        let json = serde_json::to_string(&ceiling).map_err(ctx("ceiling must serialize"))?;
        let restored: ContextCeiling =
            serde_json::from_str(&json).map_err(ctx("ceiling must deserialize"))?;
        assert_eq!(ceiling, restored);
        Ok(())
    }

    #[test]
    fn test_context_ceiling_deserialize_rejects_unknown_field() -> TestResult {
        let ceiling = ceiling(&["history.tail"], TrustClass::Evidence, &[])?;
        let mut value =
            serde_json::to_value(&ceiling).map_err(ctx("ceiling must serialize to value"))?;
        value
            .as_object_mut()
            .ok_or(TestError::Missing("ceiling serializes to an object"))?
            .insert("unexpected".to_owned(), serde_json::json!(true));

        let result: Result<ContextCeiling, _> = serde_json::from_value(value);
        assert!(result.is_err());
        Ok(())
    }
}
