//! Montage des Werkzeugs `agent.result` (Runde 5, Teil H).
//!
//! # Beschreibung
//! `agent.result` (`harw_core_bridge::AgentResultOperation`) liefert den
//! ungekürzten Antworttext eines eigenen, abgeschlossenen Kind-Laufs aus dem
//! Ergebnisarchiv des `ManagedAgentSpawner`. Die Operation braucht nur den
//! Spawner im `OpContext`; die aufrufende Sitzung kommt aus dem
//! [`harw_extension_api::ToolExecutionContext`], nie aus Modell-Argumenten.
//!
//! Zugelassen ist das Werkzeug überall, wo Kinder gestartet werden dürfen:
//! - an der Wurzel, sobald sie einen Spawner trägt und dem Modell Werkzeuge
//!   anbietet (`assembly.rs`, Schritt 12),
//! - an den Kind-Registries der Rollen aus
//!   [`harw_registry_defaults::profile::child_result_tools_for_role`]
//!   (`children.rs`).
//!
//! # Nebenläufigkeit
//! Der Provider hält den Spawner über eine Quelle (`Fn`), damit Kind-
//! Registries ihn schwach referenzieren können (kein Referenzzyklus
//! Registry → Spawner → Fabrik → Registry).

use std::sync::Arc;

use harw_core::child_controller::ManagedAgentSpawner;
use harw_operations::OpContext;
use harw_operations::adapter::ModelToolProvider;
use harw_operations::context::ServiceMap;
use harw_operations::operation::Operation;

/// Baut den [`ModelToolProvider`] mit genau der Operation `agent.result`.
///
/// # Argumente
/// - `spawner` (`Fn() -> Option<Arc<ManagedAgentSpawner>>`): liefert den
///   Spawner je Aufruf; `None` (Spawner schon abgebaut) lässt die Operation
///   fail-closed mit `NotAvailable` scheitern.
///
/// # Rückgabe
/// Einen Provider, dessen einziges Werkzeug `agent.result` heißt.
#[must_use]
pub fn agent_result_provider<S>(spawner: S) -> ModelToolProvider
where
    S: Fn() -> Option<Arc<ManagedAgentSpawner>> + Send + Sync + 'static,
{
    let operation: Arc<dyn Operation> = Arc::new(harw_core_bridge::AgentResultOperation::new());
    ModelToolProvider::new([operation], move |execution_context| {
        let mut services = ServiceMap::new();
        if let Some(spawner) = spawner() {
            services.insert(spawner);
        }
        OpContext::new(
            execution_context.session_id().clone(),
            execution_context.turn_id().clone(),
            execution_context.sandbox().clone(),
            services,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::agent_result_provider;

    #[test]
    fn provider_offers_exactly_agent_result() {
        let provider = agent_result_provider(|| None);
        let names: Vec<String> = harw_extension_api::ToolProvider::tools(&provider)
            .iter()
            .map(|spec| spec.name().to_owned())
            .collect();
        assert_eq!(names, vec![harw_core_bridge::AGENT_RESULT_TOOL.to_owned()]);
        assert_eq!(
            harw_registry_defaults::profile::CHILD_RESULT_TOOLS,
            &[harw_core_bridge::AGENT_RESULT_TOOL]
        );
    }
}
