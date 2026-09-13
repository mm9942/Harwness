//! Extension-Trait `OpContextPlanExt` — bindet Plan-Dienste an einen `OpContext`.
//!
//! # Verantwortungsbereich
//! `harw-operations` kennt weder `harw-plan` noch diese Bridge — der
//! [`OpContext`] hält seine Dienste in einer typisierten [`ServiceMap`]. Dieser
//! Trait ist die öffentliche Leseseite für Plan-Store, Goal-Store,
//! Finding-Store und Plan-Konfiguration; [`register_plan_services`] ist die
//! zugehörige Schreibseite für die Composition-Root.
//!
//! Muster: `harw-core-bridge/src/context_ext.rs` (`OpContextCoreExt`).
//!
//! # Warum gebündelt registrieren
//! Die vier Dienste gehören zusammen: ein Plan ohne Goal-Store kann seine
//! Zielerreichung nicht bewerten, ein Plan ohne Finding-Store kann keine
//! Evidenz ablegen. [`register_plan_services`] trägt sie unter einem einzigen
//! exklusiven `&mut ServiceMap`-Borrow ein, damit kein Zwischenzustand mit
//! halber Ausstattung beobachtbar ist.
//!
//! # Exportierte Typen
//! [`OpContextPlanExt`], [`register_plan_services`].
//!
//! # Concurrency
//! Alle Getter geben `Arc`-Klone zurück (`Arc::clone` auf den Zeiger, nie auf
//! die inneren Daten). Die Stores sind selbst `Send + Sync`.
//!
//! # Fehler
//! Die `require_*`-Varianten geben [`PlanBridgeError::ServiceMissing`] zurück,
//! wenn die Composition-Root den jeweiligen Dienst nicht eingetragen hat.

use std::sync::Arc;

use harw_operations::context::{OpContext, ServiceMap};
use harw_plan::goal::GoalStore;
use harw_plan::{PlanStore, PlanToolConfig};

use crate::error::PlanBridgeError;
use crate::finding_store::FindingStore;

/// Dienstname des Plan-Stores in Fehlermeldungen.
const SERVICE_PLAN_STORE: &str = "plan_store";

/// Dienstname des Goal-Stores in Fehlermeldungen.
const SERVICE_GOAL_STORE: &str = "goal_store";

/// Dienstname des Finding-Stores in Fehlermeldungen.
const SERVICE_FINDING_STORE: &str = "finding_store";

/// Dienstname der Plan-Konfiguration in Fehlermeldungen.
const SERVICE_PLAN_CONFIG: &str = "plan_config";

/// Erweitert [`OpContext`] um Zugriff auf die Plan-Laufzeit.
///
/// # Description
/// Die Dienste liegen weiterhin generisch in der [`ServiceMap`] unter ihren
/// konkreten Typen (`Arc<dyn PlanStore>`, `Arc<dyn GoalStore>`,
/// `Arc<FindingStore>`, `PlanToolConfig`) — es gibt weder String-Schlüssel noch
/// einen zweiten Zugriffspfad.
///
/// Die vier `Option`-Getter sind der Vertrag aus AP W3-06..13. Die
/// `require_*`-Varianten sind Bequemlichkeit für Aufrufer, die ohne den Dienst
/// ohnehin nicht weiterarbeiten können, und sind der Konstruktionsort von
/// [`PlanBridgeError::ServiceMissing`].
///
/// # Concurrency
/// Alle Methoden nehmen `&self` und sind aus mehreren Threads sicher.
pub trait OpContextPlanExt {
    /// Gibt den registrierten Plan-Store zurück, falls vorhanden.
    ///
    /// # Returns
    /// `Some(Arc<dyn PlanStore>)`, wenn die Composition-Root ihn eingetragen
    /// hat, sonst `None`.
    fn plan_store(&self) -> Option<Arc<dyn PlanStore>>;

    /// Gibt den registrierten Goal-Store zurück, falls vorhanden.
    ///
    /// # Returns
    /// `Some(Arc<dyn GoalStore>)` oder `None`.
    fn goal_store(&self) -> Option<Arc<dyn GoalStore>>;

    /// Gibt den registrierten Finding-Store zurück, falls vorhanden.
    ///
    /// # Returns
    /// `Some(Arc<FindingStore>)` oder `None`.
    fn finding_store(&self) -> Option<Arc<FindingStore>>;

    /// Gibt die registrierte Plan-Konfiguration zurück, falls vorhanden.
    ///
    /// # Returns
    /// Einen Klon der [`PlanToolConfig`] oder `None`. Die Konfiguration ist
    /// ein kleiner Werttyp; ein Klon ist billiger als ein `Arc`.
    fn plan_config(&self) -> Option<PlanToolConfig>;

    /// Wie [`Self::plan_store`], aber mit Fehler statt `None`.
    ///
    /// # Errors
    /// [`PlanBridgeError::ServiceMissing`] mit `service = "plan_store"`.
    fn require_plan_store(&self) -> Result<Arc<dyn PlanStore>, PlanBridgeError> {
        self.plan_store().ok_or(PlanBridgeError::ServiceMissing {
            service: SERVICE_PLAN_STORE,
        })
    }

    /// Wie [`Self::goal_store`], aber mit Fehler statt `None`.
    ///
    /// # Errors
    /// [`PlanBridgeError::ServiceMissing`] mit `service = "goal_store"`.
    fn require_goal_store(&self) -> Result<Arc<dyn GoalStore>, PlanBridgeError> {
        self.goal_store().ok_or(PlanBridgeError::ServiceMissing {
            service: SERVICE_GOAL_STORE,
        })
    }

    /// Wie [`Self::finding_store`], aber mit Fehler statt `None`.
    ///
    /// # Errors
    /// [`PlanBridgeError::ServiceMissing`] mit `service = "finding_store"`.
    fn require_finding_store(&self) -> Result<Arc<FindingStore>, PlanBridgeError> {
        self.finding_store().ok_or(PlanBridgeError::ServiceMissing {
            service: SERVICE_FINDING_STORE,
        })
    }

    /// Wie [`Self::plan_config`], aber mit Fehler statt `None`.
    ///
    /// # Errors
    /// [`PlanBridgeError::ServiceMissing`] mit `service = "plan_config"`.
    fn require_plan_config(&self) -> Result<PlanToolConfig, PlanBridgeError> {
        self.plan_config().ok_or(PlanBridgeError::ServiceMissing {
            service: SERVICE_PLAN_CONFIG,
        })
    }
}

impl OpContextPlanExt for OpContext {
    fn plan_store(&self) -> Option<Arc<dyn PlanStore>> {
        self.service::<Arc<dyn PlanStore>>().map(Arc::clone)
    }

    fn goal_store(&self) -> Option<Arc<dyn GoalStore>> {
        self.service::<Arc<dyn GoalStore>>().map(Arc::clone)
    }

    fn finding_store(&self) -> Option<Arc<FindingStore>> {
        self.service::<Arc<FindingStore>>().map(Arc::clone)
    }

    fn plan_config(&self) -> Option<PlanToolConfig> {
        self.service::<PlanToolConfig>().cloned()
    }
}

/// Trägt die vollständige Plan-Ausstattung in eine [`ServiceMap`] ein.
///
/// # Description
/// Composition-Roots benutzen diesen gebündelten Einstieg, statt die vier
/// Dienste unabhängig zu registrieren. Alle Einfügungen erfolgen unter
/// demselben exklusiven `&mut ServiceMap`-Borrow; für einen regulär
/// zurückkehrenden Aufruf ist damit kein Zwischenzustand beobachtbar. Das ist
/// bewusst ein *in der Absicht* gebündelter API-Aufruf, keine transaktionale
/// `ServiceMap`-Primitive.
///
/// Bereits vorhandene, unbeteiligte Dienste bleiben erhalten.
///
/// # Arguments
/// - `services` (`&mut ServiceMap`): die zu bestückende Dienstkarte.
/// - `plan` (`Arc<dyn PlanStore>`): der Plan-Store der Session.
/// - `goal` (`Arc<dyn GoalStore>`): der Goal-Store der Session.
/// - `findings` (`Arc<FindingStore>`): die Ablage für Recherche-Artefakte.
/// - `config` (`PlanToolConfig`): die geltende Plan-Tool-Konfiguration.
///
/// # Concurrency
/// Benötigt exklusiven Zugriff auf `services`; nach dem Aufruf sind alle
/// Dienste über [`OpContextPlanExt`] aus beliebigen Threads lesbar.
///
/// # Examples
/// ```rust
/// use std::sync::Arc;
/// use harw_operations::context::ServiceMap;
/// use harw_plan::{InMemoryPlanStore, PlanStore, PlanToolConfig};
/// use harw_plan_bridge::{FindingStore, register_plan_services};
///
/// # fn demo(goal: Arc<dyn harw_plan::goal::GoalStore>) {
/// let mut services = ServiceMap::new();
/// let plan: Arc<dyn PlanStore> = Arc::new(InMemoryPlanStore::new());
/// register_plan_services(
///     &mut services,
///     plan,
///     goal,
///     Arc::new(FindingStore::new("/tmp/plans")),
///     PlanToolConfig::enabled_defaults(),
/// );
/// assert!(services.get::<PlanToolConfig>().is_some());
/// # }
/// ```
pub fn register_plan_services(
    services: &mut ServiceMap,
    plan: Arc<dyn PlanStore>,
    goal: Arc<dyn GoalStore>,
    findings: Arc<FindingStore>,
    config: PlanToolConfig,
) {
    services.insert(plan);
    services.insert(goal);
    services.insert(findings);
    services.insert(config);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::InMemoryGoalStore;
    use harw_plan::InMemoryPlanStore;

    fn populated_services() -> (ServiceMap, Arc<dyn PlanStore>, Arc<FindingStore>) {
        let mut services = ServiceMap::new();
        let plan: Arc<dyn PlanStore> = Arc::new(InMemoryPlanStore::new());
        let goal: Arc<dyn GoalStore> = Arc::new(InMemoryGoalStore::new());
        let findings = Arc::new(FindingStore::new("/tmp/harw-plan-bridge-test/plans"));

        register_plan_services(
            &mut services,
            Arc::clone(&plan),
            goal,
            Arc::clone(&findings),
            PlanToolConfig::enabled_defaults(),
        );
        (services, plan, findings)
    }

    #[test]
    fn test_register_plan_services_stores_the_exact_arcs() {
        let (services, plan, findings) = populated_services();

        let registered_plan = match services.get::<Arc<dyn PlanStore>>() {
            Some(registered) => registered,
            None => panic!("Plan-Store wurde nicht registriert"),
        };
        let registered_findings = match services.get::<Arc<FindingStore>>() {
            Some(registered) => registered,
            None => panic!("Finding-Store wurde nicht registriert"),
        };

        assert!(Arc::ptr_eq(registered_plan, &plan));
        assert!(Arc::ptr_eq(registered_findings, &findings));
        assert!(services.get::<Arc<dyn GoalStore>>().is_some());
        assert!(services.get::<PlanToolConfig>().is_some());
    }

    #[test]
    fn test_register_plan_services_preserves_unrelated_services() {
        let mut services = ServiceMap::new();
        services.insert("fremder Dienst".to_owned());

        register_plan_services(
            &mut services,
            Arc::new(InMemoryPlanStore::new()),
            Arc::new(InMemoryGoalStore::new()),
            Arc::new(FindingStore::new("/tmp/harw-plan-bridge-test/plans")),
            PlanToolConfig::enabled_defaults(),
        );

        assert_eq!(
            services.get::<String>().map(String::as_str),
            Some("fremder Dienst")
        );
        assert!(services.get::<Arc<dyn PlanStore>>().is_some());
    }

    #[test]
    fn test_registered_config_is_readable_as_a_clone() {
        let (services, _plan, _findings) = populated_services();
        let config = match services.get::<PlanToolConfig>() {
            Some(config) => config.clone(),
            None => panic!("Konfiguration wurde nicht registriert"),
        };
        assert!(config.is_enabled());
    }
}
