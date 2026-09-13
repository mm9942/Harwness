//! Der Regel-Trait und sein Eingabekontext (Knoten AW4-03, Contract-Master §G.1).
//!
//! # Verantwortungsbereich
//! [`Rule`] ist die eine Schnittstelle, über die aus Beobachtungen Befunde
//! werden. [`RuleContext`] bündelt alles, was eine Regel zur Auswertung
//! braucht — nichts, was eine Regel selbst beschaffen müsste.
//!
//! # Reine Funktionen — die wichtigste Auflage dieser Crate
//! Eine [`Rule::evaluate`]-Implementierung darf kein I/O ausführen, kein Netz
//! ansprechen, keinen Subprozess starten und **nie die Systemuhr lesen**.
//! `now` ist ein Feld von [`RuleContext`], keine Methode, die intern
//! `jiff::Timestamp::now()` aufrufen könnte — eine Regel bekommt die Zeit nur
//! injiziert. Diese Auflage ist keine Stilfrage: eine Regel, die ihre
//! Umgebung selbst liest, ist gegen keinen goldenen Fall mehr prüfbar, und
//! goldene Fälle (feste Eingabe, festes erwartetes Ergebnis) sind die einzige
//! Art, eine Sicherheitsregel verlässlich zu prüfen. Jede Regel dieser Crate
//! trägt deshalb einen Test, der sie zweimal mit identischem `RuleContext`
//! auswertet und byteidentische Ergebnisse erwartet (siehe die Tests in
//! [`crate::rules`]).
//!
//! # Nebenläufigkeit
//! `Rule: Send + Sync`, damit ein Aufrufer (z. B. [`crate::engine::run_rules`])
//! mehrere Regeln parallel über denselben, unveränderlichen [`RuleContext`]
//! laufen lassen kann, ohne Sperren. Jede konkrete Regel in dieser Crate ist
//! ein zustandsloser Unit-Struct und damit trivial `Send + Sync`.
//!
//! # Fehler
//! Keine. [`Rule::evaluate`] liefert `Vec<Finding<Raw>>`, kein `Result` — eine
//! Regel, die nichts auslöst, liefert einen leeren Vektor, keinen Fehler.
//!
//! # Examples
//! ```rust
//! use harw_dod_rules::rule::{Rule, RuleContext};
//! use harw_dod_rules::rules::EgressFlowRule;
//! use harw_sandbox::NetworkScope;
//!
//! let scope = NetworkScope::empty();
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &[],
//!     baselines: &[],
//!     network_scope: &scope,
//! };
//! assert!(EgressFlowRule.evaluate(&ctx).is_empty());
//! ```

use harw_dod_signals::{HostSample, SecurityEvent};
use harw_sandbox::NetworkScope;
use jiff::Timestamp;

use crate::baseline::Baseline;
use crate::finding::{Finding, Raw};

/// Eine Sicherheitsregel: reine Funktion von Beobachtungen zu Befunden.
///
/// # Description
/// Jede Implementierung prüft [`RuleContext`] und liefert die dabei
/// ausgelösten Befunde als `Finding<Raw>` — noch nicht zertifiziert (siehe
/// [`crate::finding`]-Moduldoku). Siehe Moduldoku für die Reinheitsauflage.
///
/// # Errors
/// Keine eigenen Fehler; siehe Moduldoku.
///
/// # Concurrency
/// `Send + Sync`, siehe Moduldoku.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::rule::{Rule, RuleContext};
/// use harw_dod_rules::rules::StructureDriftRule;
/// use harw_sandbox::NetworkScope;
///
/// let scope = NetworkScope::empty();
/// let ctx = RuleContext {
///     now: jiff::Timestamp::UNIX_EPOCH,
///     samples: &[],
///     events: &[],
///     baselines: &[],
///     network_scope: &scope,
/// };
/// assert_eq!(StructureDriftRule.id(), "structure-drift");
/// ```
pub trait Rule: Send + Sync {
    /// Die stabile Kennung dieser Regel.
    ///
    /// # Returns
    /// Ein statischer, nicht-leerer Bezeichner (z. B. `"egress-flow"`). Wird
    /// unverändert in jeden von dieser Regel erzeugten [`Finding::rule_id`]
    /// übernommen.
    fn id(&self) -> &'static str;

    /// Prüft den Kontext und liefert die ausgelösten Befunde.
    ///
    /// # Arguments
    /// - `ctx` (`&RuleContext<'_>`): die Beobachtungen und der Bezugsrahmen,
    ///   gegen die diese Regel prüft.
    ///
    /// # Returns
    /// Die ausgelösten Befunde als `Vec<Finding<Raw>>`. Leer, wenn nichts
    /// ausgelöst hat.
    ///
    /// # Errors
    /// Keine — siehe Moduldoku.
    fn evaluate(&self, ctx: &RuleContext<'_>) -> Vec<Finding<Raw>>;
}

/// Der Eingabekontext einer Regelauswertung.
///
/// # Description
/// Bündelt alle Beobachtungen und den Bezugsrahmen, den eine Regel dieser
/// Crate braucht. `network_scope` ist ein Zusatz gegenüber der ursprünglichen
/// Skizze (die dort vorgesehene Lücke „was die konkreten Regeln unten sonst
/// brauchen“) — [`crate::rules::EgressFlowRule`] braucht ihn, um
/// `harw_sandbox::NetworkScope::allows`/`allows_addr` zu benutzen, statt die
/// Bereichsprüfung selbst zweitzuschreiben.
///
/// # Errors
/// Keine eigenen Fehler.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::rule::RuleContext;
/// use harw_sandbox::NetworkScope;
///
/// let scope = NetworkScope::empty();
/// let ctx = RuleContext {
///     now: jiff::Timestamp::UNIX_EPOCH,
///     samples: &[],
///     events: &[],
///     baselines: &[],
///     network_scope: &scope,
/// };
/// assert!(ctx.samples.is_empty());
/// ```
#[derive(Debug, Clone)]
pub struct RuleContext<'a> {
    /// **Injizierte** Zeit. Eine Regel liest nie die Systemuhr.
    pub now: Timestamp,
    /// Die zu prüfenden Messwerte.
    pub samples: &'a [HostSample],
    /// Die zu prüfenden Sicherheitsereignisse.
    pub events: &'a [SecurityEvent],
    /// Die bekannten Baselines, gegen die eine Regel abweichende
    /// Beobachtungen bewerten kann (siehe [`crate::baseline`]-Moduldoku).
    pub baselines: &'a [Baseline],
    /// Der erlaubte Netzbereich, gegen den
    /// [`crate::rules::EgressFlowRule`] beobachtete Ziele prüft.
    pub network_scope: &'a NetworkScope,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rule_context_exposes_given_slices() {
        let scope = NetworkScope::empty();
        let ctx = RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &[],
            events: &[],
            baselines: &[],
            network_scope: &scope,
        };
        assert!(ctx.samples.is_empty());
        assert!(ctx.events.is_empty());
        assert!(ctx.baselines.is_empty());
        assert_eq!(ctx.now, Timestamp::UNIX_EPOCH);
    }
}
