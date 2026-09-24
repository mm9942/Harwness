//! Montage von `agent.message` und `parent.message` (Runde 5, Teil M).
//!
//! # Beschreibung
//! - `agent.message` (`harw_core_bridge::AgentMessageOperation`) schickt
//!   einem eigenen, laufenden Kind eine Nachricht. Zugelassen überall, wo
//!   Kinder gestartet werden dürfen: an der Wurzel mit Spawner und
//!   Modell-Werkzeugfläche (`assembly.rs`, Schritt 12, neben `agent.result`)
//!   und an den Kind-Registries der Rollen aus
//!   [`harw_registry_defaults::profile::child_message_tools_for_role`]
//!   (`children.rs`).
//! - `parent.message` (`harw_core_bridge::ParentMessageOperation`) meldet
//!   dem direkten Elternteil einen Zwischenstand oder stellt eine Frage.
//!   Zugelassen an den Kind-Registries der Rollen aus
//!   [`harw_registry_defaults::profile::parent_message_tools_for_role`]; nie
//!   an der Wurzel (sie hat keinen Elternteil).
//!
//! Beide Operationen brauchen nur den Spawner im `OpContext`; die aufrufende
//! Sitzung kommt aus dem [`harw_extension_api::ToolExecutionContext`], nie
//! aus Modell-Argumenten.
//!
//! # Nebenläufigkeit
//! Der Provider hält den Spawner über eine Quelle (`Fn`), wie
//! `agent_result_wiring` — Kind-Registries referenzieren ihn schwach.

use std::sync::Arc;

use harw_core::child_controller::ManagedAgentSpawner;
use harw_operations::OpContext;
use harw_operations::adapter::ModelToolProvider;
use harw_operations::context::ServiceMap;
use harw_operations::operation::Operation;

/// Baut einen [`ModelToolProvider`] mit genau `operation`.
fn provider_with<S>(operation: Arc<dyn Operation>, spawner: S) -> ModelToolProvider
where
    S: Fn() -> Option<Arc<ManagedAgentSpawner>> + Send + Sync + 'static,
{
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

/// Der Provider mit genau `agent.message`.
///
/// # Argumente
/// - `spawner`: liefert den Spawner je Aufruf; `None` lässt die Operation
///   fail-closed mit `NotAvailable` scheitern.
#[must_use]
pub fn agent_message_provider<S>(spawner: S) -> ModelToolProvider
where
    S: Fn() -> Option<Arc<ManagedAgentSpawner>> + Send + Sync + 'static,
{
    provider_with(Arc::new(harw_core_bridge::AgentMessageOperation), spawner)
}

/// Der Provider mit genau `parent.message`.
///
/// # Argumente
/// - `spawner`: wie bei [`agent_message_provider`].
#[must_use]
pub fn parent_message_provider<S>(spawner: S) -> ModelToolProvider
where
    S: Fn() -> Option<Arc<ManagedAgentSpawner>> + Send + Sync + 'static,
{
    provider_with(Arc::new(harw_core_bridge::ParentMessageOperation), spawner)
}

#[cfg(test)]
mod tests {
    use super::{agent_message_provider, parent_message_provider};

    #[test]
    fn providers_offer_exactly_their_tool() {
        let names = |provider: &harw_operations::adapter::ModelToolProvider| {
            harw_extension_api::ToolProvider::tools(provider)
                .iter()
                .map(|spec| spec.name().to_owned())
                .collect::<Vec<String>>()
        };
        assert_eq!(
            names(&agent_message_provider(|| None)),
            vec![harw_core_bridge::AGENT_MESSAGE_TOOL.to_owned()]
        );
        assert_eq!(
            names(&parent_message_provider(|| None)),
            vec![harw_core_bridge::PARENT_MESSAGE_TOOL.to_owned()]
        );
        assert_eq!(
            harw_registry_defaults::profile::CHILD_MESSAGE_TOOLS,
            &[harw_core_bridge::AGENT_MESSAGE_TOOL]
        );
        assert_eq!(
            harw_registry_defaults::profile::PARENT_MESSAGE_TOOLS,
            &[harw_core_bridge::PARENT_MESSAGE_TOOL]
        );
    }
}
