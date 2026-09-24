//! Montage der Hintergrund-Agenten-Steuerung (Runde 5, Teil K).
//!
//! # Beschreibung
//! Zwei Dinge:
//! - [`orchestration_limits`] übersetzt `[agents]` (`harw_config::AgentLimitsToml`,
//!   geklemmt und konsistent) in die Grenzen, die der `ManagedAgentSpawner`
//!   bei jeder Admission durchsetzt. Die allgemeine Tiefe (`max_spawn_depth`)
//!   läuft über [`crate::budget::child_limits`] in `ChildLimits::max_depth`.
//! - [`install_agent_background_tools`] hängt `agent.status` (lesend) und
//!   `agent.cancel` (immer mit Freigabe) an die Wurzel-Registry — nur an der
//!   TUI-Wurzel, denn nur dort laufen Orchestratoren im Hintergrund
//!   (`assembly.rs`, Schritt 12).
//!
//! # Nebenläufigkeit
//! Der Provider hält den Spawner über eine Quelle (`Fn`), wie
//! `agent_result_wiring`.

use std::sync::Arc;

use harw_config::ResolvedConfig;
use harw_core::OrchestrationLimits;
use harw_core::child_controller::ManagedAgentSpawner;
use harw_extension_api::ExtensionRegistryBuilder;
use harw_operations::OpContext;
use harw_operations::adapter::ModelToolProvider;
use harw_operations::context::ServiceMap;
use harw_operations::operation::Operation;

/// Die Orchestrierungsgrenzen aus `[agents]`.
///
/// # Returns
/// [`OrchestrationLimits`] mit den geklemmten, konsistenten Werten.
#[must_use]
pub fn orchestration_limits(config: &ResolvedConfig) -> OrchestrationLimits {
    let effective = config.harness.agents.effective();
    OrchestrationLimits {
        max_root_orchestrators: usize::try_from(effective.max_root_orchestrators)
            .unwrap_or(usize::MAX),
        max_sub_orchestrators: usize::try_from(effective.max_sub_orchestrators)
            .unwrap_or(usize::MAX),
        max_sub_orchestrator_depth: effective.max_sub_orchestrator_depth,
    }
}

/// Baut den [`ModelToolProvider`] mit den zugelassenen Werkzeugen aus
/// `harw_core_bridge::AGENT_BACKGROUND_TOOLS`.
///
/// # Argumente
/// - `admitted`: die Werkzeugnamen, die die Wurzel führen darf.
/// - `spawner`: liefert den Spawner je Aufruf; `None` lässt die Operationen
///   fail-closed mit `NotAvailable` scheitern.
#[must_use]
pub fn agent_background_provider<S>(admitted: &[&str], spawner: S) -> ModelToolProvider
where
    S: Fn() -> Option<Arc<ManagedAgentSpawner>> + Send + Sync + 'static,
{
    let mut operations: Vec<Arc<dyn Operation>> = Vec::new();
    if admitted.contains(&harw_core_bridge::AGENT_STATUS_TOOL) {
        operations.push(Arc::new(harw_core_bridge::AgentStatusOperation));
    }
    if admitted.contains(&harw_core_bridge::AGENT_CANCEL_TOOL) {
        operations.push(Arc::new(harw_core_bridge::AgentCancelOperation));
    }
    ModelToolProvider::new(operations, move |execution_context| {
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

/// Hängt die zugelassenen Hintergrund-Werkzeuge an `builder`; ohne
/// zugelassenes Werkzeug bleibt `builder` unverändert.
#[must_use]
pub fn install_agent_background_tools<S>(
    builder: ExtensionRegistryBuilder,
    admitted: &[&str],
    spawner: S,
) -> ExtensionRegistryBuilder
where
    S: Fn() -> Option<Arc<ManagedAgentSpawner>> + Send + Sync + 'static,
{
    if admitted.is_empty() {
        return builder;
    }
    builder.tool_provider(Arc::new(agent_background_provider(admitted, spawner)))
}

#[cfg(test)]
mod tests {
    use super::{agent_background_provider, orchestration_limits};

    #[test]
    fn provider_offers_exactly_the_admitted_background_tools() {
        let both = agent_background_provider(harw_core_bridge::AGENT_BACKGROUND_TOOLS, || None);
        let names: Vec<String> = harw_extension_api::ToolProvider::tools(&both)
            .iter()
            .map(|spec| spec.name().to_owned())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|name| name == "agent.status"));
        assert!(names.iter().any(|name| name == "agent.cancel"));

        let status_only = agent_background_provider(&["agent.status"], || None);
        let names: Vec<String> = harw_extension_api::ToolProvider::tools(&status_only)
            .iter()
            .map(|spec| spec.name().to_owned())
            .collect();
        assert_eq!(names, vec!["agent.status".to_owned()]);
    }

    #[test]
    fn orchestration_limits_follow_the_agents_section() {
        let config = harw_config::ResolvedConfig::default();
        assert_eq!(
            orchestration_limits(&config),
            harw_core::OrchestrationLimits::default()
        );
        let mut raised = harw_config::ResolvedConfig::default();
        raised.harness.agents.max_root_orchestrators = Some(2);
        raised.harness.agents.max_sub_orchestrators = Some(99);
        let limits = orchestration_limits(&raised);
        assert_eq!(limits.max_root_orchestrators, 2);
        assert_eq!(limits.max_sub_orchestrators, 6, "geklemmt");
    }
}
