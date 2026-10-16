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
            assert_ne!(s.max_trust, TrustClass::Instruction, "{}", s.id);
        }
        let total: u32 = SOURCES.iter().map(|s| u32::from(s.budget_share_pct)).sum();
        assert!(total <= 100);
    }
}
