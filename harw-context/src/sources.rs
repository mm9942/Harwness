//! Die Tabelle der Kontextquellen: jede Quelle einmal deklariert.
//!
//! `context_sources!` erzeugt aus einer Liste die Konstanten je Quelle,
//! die Tabelle [`SOURCES`] und [`source`] (Suche nach Namensraum). Anbieter
//! beziehen ihren Namensraum und ihre Vertrauensobergrenze aus der Tabelle;
//! Ledger, Doctor und Konfiguration lesen dieselbe Liste.

use crate::fragment::TrustClass;

/// Eine deklarierte Kontextquelle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextSource {
    /// Kennung in Konfiguration und Ledger.
    pub id: &'static str,
    /// Namensraum des `ContextProvider`.
    pub namespace: &'static str,
    /// Höchste Vertrauensklasse, die diese Quelle beanspruchen darf.
    pub max_trust: TrustClass,
    /// Anteil am Kontextbudget in Prozent (0 = kein eigener Anteil).
    pub budget_share_pct: u8,
    /// Ob die Quelle ohne ausdrückliche Freigabe aktiv ist.
    pub default_enabled: bool,
}

/// Deklariert die Kontextquellen (siehe Moduldoku).
///
/// ```text
/// context_sources! {
///     memory_facts: { namespace = "...", trust = Data, budget_share_pct = 10, default_enabled = true }
/// }
/// ```
macro_rules! context_sources {
    ($( $id:ident : { namespace = $ns:literal, trust = $trust:ident,
        budget_share_pct = $share:literal, default_enabled = $on:literal } )+) => {
        $(
            #[doc = concat!("Quelle `", stringify!($id), "`.")]
            #[allow(non_upper_case_globals)]
            pub const $id: ContextSource = ContextSource {
                id: stringify!($id),
                namespace: $ns,
                max_trust: TrustClass::$trust,
                budget_share_pct: $share,
                default_enabled: $on,
            };
        )+
        /// Alle Quellen in Deklarationsreihenfolge.
        pub static SOURCES: &[ContextSource] = &[ $( $id ),+ ];
    };
}

context_sources! {
    memory_facts: { namespace = "harw.runtime.memory_facts", trust = Data, budget_share_pct = 10, default_enabled = true }
    repo_tree: { namespace = "harw.runtime.repo_tree", trust = Data, budget_share_pct = 5, default_enabled = true }
    pinned_plan: { namespace = "plan.pinned", trust = Evidence, budget_share_pct = 5, default_enabled = true }
    knowledge: { namespace = "knowledge", trust = Data, budget_share_pct = 10, default_enabled = true }
    security_signals: { namespace = "harw.runtime.security_signals", trust = Evidence, budget_share_pct = 2, default_enabled = false }
    skills_triggered: { namespace = "skills.triggered", trust = Instruction, budget_share_pct = 3, default_enabled = true }
}

/// Ids of sources produced by the harness core itself, never by a
/// `ContextProvider`. Only these may declare [`TrustClass::Instruction`]:
/// `skills_triggered` carries text of bundled or trusted-layer skills that
/// `harw-core` selects deterministically (PL-95 S3); an imported skill is
/// lowered to [`TrustClass::Evidence`] by the producer.
pub const CORE_OWNED_SOURCES: &[&str] = &["skills_triggered"];

impl ContextSource {
    /// This source's share of `total` (any unit), rounded down.
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::sources::skills_triggered;
    ///
    /// assert_eq!(skills_triggered.share_of(100_000), 3_000);
    /// assert_eq!(skills_triggered.share_of(0), 0);
    /// ```
    #[must_use]
    pub const fn share_of(&self, total: u64) -> u64 {
        let pct = self.budget_share_pct as u64;
        (total / 100)
            .saturating_mul(pct)
            .saturating_add(total % 100 * pct / 100)
    }
}

/// Sucht eine Quelle über ihren Namensraum.
#[must_use]
pub fn source(namespace: &str) -> Option<&'static ContextSource> {
    SOURCES.iter().find(|s| s.namespace == namespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_unique_and_security_signals_is_opt_in() {
        let mut ids: Vec<_> = SOURCES.iter().map(|s| s.id).collect();
        let mut spaces: Vec<_> = SOURCES.iter().map(|s| s.namespace).collect();
        ids.sort_unstable();
        spaces.sort_unstable();
        ids.dedup();
        spaces.dedup();
        assert_eq!(ids.len(), SOURCES.len());
        assert_eq!(spaces.len(), SOURCES.len());
        assert!(
            SOURCES
                .iter()
                .any(|s| s.id == "security_signals" && !s.default_enabled)
        );
        assert_eq!(source("knowledge"), Some(&knowledge));
        assert!(source("nope").is_none());
    }

    #[test]
    fn no_source_claims_instruction_trust_and_shares_fit_the_budget() {
        for s in SOURCES {
            if CORE_OWNED_SOURCES.contains(&s.id) {
                continue;
            }
            assert_ne!(s.max_trust, TrustClass::Instruction, "{}", s.id);
        }
        let total: u32 = SOURCES.iter().map(|s| u32::from(s.budget_share_pct)).sum();
        assert!(total <= 100);
    }

    #[test]
    fn skills_triggered_is_the_only_core_owned_source_and_matches_its_section() {
        assert_eq!(CORE_OWNED_SOURCES, &["skills_triggered"]);
        assert_eq!(
            skills_triggered.namespace,
            crate::ceiling::SKILLS_TRIGGERED_SECTION
        );
        assert_eq!(skills_triggered.max_trust, TrustClass::Instruction);
        assert_eq!(skills_triggered.budget_share_pct, 3);
        assert!(
            SOURCES
                .iter()
                .any(|s| s.id == "skills_triggered" && s.default_enabled)
        );
        assert_eq!(source("skills.triggered"), Some(&skills_triggered));
    }

    #[test]
    fn share_of_rounds_down_and_never_overflows() {
        assert_eq!(skills_triggered.share_of(32_768 * 4), 3_932);
        assert_eq!(skills_triggered.share_of(99), 2);
        assert_eq!(knowledge.share_of(u64::MAX), u64::MAX / 100 * 10 + 1);
    }
}
