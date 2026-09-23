//! `NetBackend`: der Anwendungs-Vertrag, den ein Durchsetzer-Knoten erfüllt.
//!
//! Diese Crate implementiert `NetBackend` selbst nur mit [`InspectBackend`],
//! das nichts anwendet, sondern aufzeichnet. Ein echtes Backend (nftables,
//! Netlink, ...) ist Sache von AW5-04a und lebt in einem eigenen Crate;
//! siehe die Moduldoc von `crate` für die Austausch-Zusage.

use std::sync::Mutex;

use crate::error::NetPolicyError;
use crate::plan::NetPlan;

/// Wendet einen Netzplan an.
///
/// # Description
/// Der einzige Punkt, an dem ein [`NetPlan`] auf echte Mechanik trifft.
/// Diese Crate liefert absichtlich nur eine Implementierung,
/// [`InspectBackend`], die nichts anwendet; ein echtes Backend (nftables,
/// Netlink, ...) implementiert diesen Trait in einem eigenen Crate
/// (AW5-04a). Weder dieser Trait noch [`NetPlan`]/[`crate::NetRule`]
/// enthalten backend-spezifische Typen — ein Wechsel des Backends ist
/// deshalb ein Austausch der Implementierung dieses Traits, keine
/// Umschreibung dieser Crate. Woran man das erkennt: ein neues Backend
/// kompiliert gegen dieselbe `apply`-Signatur, ohne dass `NetPlan` oder
/// `NetRule` ein Feld oder eine Variante gewinnen müsste, und die
/// bestehenden [`InspectBackend`]-Tests bleiben unverändert grün (siehe die
/// Moduldoc von `crate`, Abschnitt „Wie ein Backendwechsel aussieht").
///
/// # Errors
/// - [`NetPolicyError::BackendUnavailable`]: das Backend selbst ist gerade
///   nicht ansprechbar (fehlender Prozess, gesperrte Ressource, ...).
/// - [`NetPolicyError::PlanRejected`]: das Backend hat den Plan inhaltlich
///   geprüft und abgelehnt — zum Beispiel, weil er eine namensbasierte
///   Regel ([`crate::NetRule::AllowHost`]/[`crate::NetRule::AllowDnsSuffix`])
///   enthält, die dieses Backend nicht durchsetzen kann (siehe die Moduldoc
///   von `crate`, Abschnitt zu `DnsSuffix`). Ein Backend darf eine solche
///   Regel nie stillschweigend ignorieren und trotzdem `Ok(())` melden —
///   das wäre „alles erlauben" durch Unterlassung.
pub trait NetBackend {
    /// Wendet `plan` an oder lehnt ihn ab.
    ///
    /// # Arguments
    /// - `plan` (`&NetPlan`): der anzuwendende Plan.
    ///
    /// # Errors
    /// Siehe die Dokumentation von [`NetBackend`].
    fn apply(&self, plan: &NetPlan) -> Result<(), NetPolicyError>;
}

/// Backend, das nichts anwendet, sondern jeden angewendeten Plan
/// aufzeichnet.
///
/// # Description
/// Für Tests von Aufrufern dieses Traits — dieser Crate selbst und, laut
/// Auftrag, vor allem des Durchsetzer-Knotens AW5-04a: kein Root, kein
/// Kernel-Zustand, keine Nebenwirkung außerhalb dieses Werts. `apply`
/// schlägt nie fehl.
///
/// # Concurrency
/// Die aufgezeichneten Pläne liegen hinter einem [`Mutex`]; [`NetBackend::apply`]
/// und [`Self::recorded_plans`] sind aus mehreren Threads gleichzeitig
/// aufrufbar. Ein vergifteter Mutex (nach einem Panic während einer Sperre
/// in einem anderen Thread) wird wiederhergestellt statt die Sperre
/// dauerhaft zu blockieren — ein reines Aufzeichnungs-Backend darf durch
/// einen fremden Panic nicht unbrauchbar werden.
///
/// # Examples
/// ```rust
/// use harw_dod_netpolicy::{InspectBackend, NetBackend, NetPlan};
///
/// let backend = InspectBackend::new();
/// let plan = NetPlan { rules: vec![] };
/// backend.apply(&plan).expect("InspectBackend schlägt nie fehl");
/// assert_eq!(backend.recorded_plans(), vec![plan]);
/// ```
#[derive(Debug, Default)]
pub struct InspectBackend {
    recorded: Mutex<Vec<NetPlan>>,
}

impl InspectBackend {
    /// Erzeugt ein leeres `InspectBackend`.
    ///
    /// # Returns
    /// Ein Backend ohne aufgezeichnete Pläne.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Liefert eine Kopie aller bisher angewendeten Pläne, in
    /// Anwendungsreihenfolge.
    ///
    /// # Returns
    /// Einen `Vec<NetPlan>`; leer, wenn [`NetBackend::apply`] noch nie
    /// aufgerufen wurde.
    ///
    /// # Concurrency
    /// Sicher aus mehreren Threads gleichzeitig aufrufbar; siehe
    /// Typ-Dokumentation.
    #[must_use]
    pub fn recorded_plans(&self) -> Vec<NetPlan> {
        self.recorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl NetBackend for InspectBackend {
    /// Zeichnet `plan` auf. Ändert nie etwas außerhalb dieses Werts und
    /// schlägt nie fehl.
    fn apply(&self, plan: &NetPlan) -> Result<(), NetPolicyError> {
        self.recorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(plan.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::NetRule;
    use crate::test_support::{TestResult, ctx};

    fn sample_plan() -> NetPlan {
        NetPlan {
            rules: vec![NetRule::AllowHost {
                host: "docs.rs".to_owned(),
            }],
        }
    }

    #[test]
    fn test_inspect_backend_starts_with_no_recorded_plans() {
        let backend = InspectBackend::new();
        assert!(backend.recorded_plans().is_empty());
    }

    #[test]
    fn test_inspect_backend_records_applied_plans_without_mutating_them() {
        let backend = InspectBackend::new();
        let plan = sample_plan();

        let result = backend.apply(&plan);

        assert!(result.is_ok());
        assert_eq!(backend.recorded_plans(), vec![plan.clone()]);
        // Der übergebene Plan selbst bleibt unverändert: `apply` nimmt nur
        // eine Referenz entgegen und gibt nichts zurück, das ihn ersetzen
        // könnte.
        assert_eq!(plan, sample_plan());
    }

    #[test]
    fn test_inspect_backend_records_multiple_applications_in_order() -> TestResult {
        let backend = InspectBackend::new();
        let first = NetPlan { rules: vec![] };
        let second = sample_plan();

        backend
            .apply(&first)
            .map_err(ctx("InspectBackend never fails"))?;
        backend
            .apply(&second)
            .map_err(ctx("InspectBackend never fails"))?;

        assert_eq!(backend.recorded_plans(), vec![first, second]);
        Ok(())
    }
}
