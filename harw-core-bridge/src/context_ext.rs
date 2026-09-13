//! Extension-Trait `OpContextCoreExt` — bindet Core-Services an einen `OpContext`.
//!
//! # Beschreibung
//! Diese beiden Getter waren zuvor direkt an [`OpContext`] gehängt und zwangen
//! `harw-operations`, gegen `harw-core` zu linken. Als Extension-Trait auf dem
//! `harw-operations::OpContext` (der intern nur eine typisierte `ServiceMap`
//! ist), leben sie hier und lassen die Kern-Abhängigkeit sauber invertieren.

use std::sync::Arc;

use harw_core::child_controller::ManagedAgentSpawner;
use harw_core::state_store::StateStore;
use harw_operations::context::{OpContext, ServiceMap};

/// Erweitert [`OpContext`] um Zugriff auf Core-spezifische Services.
///
/// # Beschreibung
/// Die Services werden weiterhin über die generische `ServiceMap` gehalten
/// (`Arc<ManagedAgentSpawner>` und `Arc<dyn StateStore>`). Dieser Trait ist die
/// öffentliche Schnittstelle für Konsumenten, die auf die Core-Runtime
/// zugreifen wollen.
pub trait OpContextCoreExt {
    /// Gibt den registrierten [`ManagedAgentSpawner`] zurück, falls vorhanden.
    fn managed_spawner(&self) -> Option<Arc<ManagedAgentSpawner>>;

    /// Gibt den registrierten [`StateStore`] zurück, falls vorhanden.
    fn state_store(&self) -> Option<Arc<dyn StateStore>>;

    /// Registriert die vollständige, von [`crate::AgentToolAdapter`] benötigte
    /// Core-Laufzeit-Ausstattung in einer [`ServiceMap`].
    ///
    /// Composition Roots müssen diesen gebündelten Einstieg verwenden, statt
    /// Spawner und Store unabhängig zu registrieren. Die Einfügungen erfolgen
    /// unter demselben exklusiven `&mut ServiceMap`-Borrow; für einen regulär
    /// zurückkehrenden Aufruf ist damit kein Zwischenzustand beobachtbar. Dies
    /// ist absichtlich ein atomar-in-der-Absicht gebündelter API-Aufruf, keine
    /// transaktionale `ServiceMap`-Primitive. Die Services werden ausschließlich unter ihren
    /// konkreten Typen `Arc<ManagedAgentSpawner>` und `Arc<dyn StateStore>`
    /// registriert — es gibt weder String-Schlüssel noch einen alternativen
    /// Zugriffspfad.
    fn register_agent_tool_services(
        services: &mut ServiceMap,
        managed_spawner: Arc<ManagedAgentSpawner>,
        state_store: Arc<dyn StateStore>,
    ) where
        Self: Sized;
}

impl OpContextCoreExt for OpContext {
    fn managed_spawner(&self) -> Option<Arc<ManagedAgentSpawner>> {
        self.service::<Arc<ManagedAgentSpawner>>().cloned()
    }

    fn state_store(&self) -> Option<Arc<dyn StateStore>> {
        self.service::<Arc<dyn StateStore>>().cloned()
    }

    fn register_agent_tool_services(
        services: &mut ServiceMap,
        managed_spawner: Arc<ManagedAgentSpawner>,
        state_store: Arc<dyn StateStore>,
    ) {
        services.insert(managed_spawner);
        services.insert(state_store);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use harw_core::{ChildLimits, InMemoryStateStore, ManagedAgentSpawner, SessionManager};
    use harw_operations::context::{OpContext, ServiceMap};
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};

    use super::OpContextCoreExt;

    fn context_with_services(services: ServiceMap) -> (OpContext, PathBuf) {
        static CONTEXT_COUNTER: AtomicU64 = AtomicU64::new(0);

        let id = CONTEXT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-core-bridge-context-ext-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("workspace"))
            .expect("test workspace directory must be creatable");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .expect("test workspace registration must be valid");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .expect("registered test workspace must resolve");

        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
        )
    }

    #[test]
    fn paired_registration_retains_the_exact_trusted_arcs() {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let managed_spawner = Arc::new(ManagedAgentSpawner::new(
            Arc::new(std::sync::Mutex::new(SessionManager::new(event_tx))),
            ChildLimits::conservative(),
        ));
        let state_store: Arc<dyn harw_core::StateStore> = Arc::new(InMemoryStateStore::new());
        let mut services = ServiceMap::new();

        <OpContext as OpContextCoreExt>::register_agent_tool_services(
            &mut services,
            Arc::clone(&managed_spawner),
            Arc::clone(&state_store),
        );

        let registered_spawner = services
            .get::<Arc<ManagedAgentSpawner>>()
            .expect("paired registration must store the managed spawner");
        let registered_store = services
            .get::<Arc<dyn harw_core::StateStore>>()
            .expect("paired registration must store the state store");

        assert!(Arc::ptr_eq(registered_spawner, &managed_spawner));
        assert!(Arc::ptr_eq(registered_store, &state_store));
    }

    #[test]
    fn paired_registration_preserves_unrelated_services() {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let managed_spawner = Arc::new(ManagedAgentSpawner::new(
            Arc::new(std::sync::Mutex::new(SessionManager::new(event_tx))),
            ChildLimits::conservative(),
        ));
        let state_store: Arc<dyn harw_core::StateStore> = Arc::new(InMemoryStateStore::new());
        let mut services = ServiceMap::new();
        services.insert("unrelated service".to_owned());

        <OpContext as OpContextCoreExt>::register_agent_tool_services(
            &mut services,
            managed_spawner,
            state_store,
        );

        assert_eq!(
            services.get::<String>().map(String::as_str),
            Some("unrelated service")
        );
        assert!(services.get::<Arc<ManagedAgentSpawner>>().is_some());
        assert!(services.get::<Arc<dyn harw_core::StateStore>>().is_some());
    }

    #[test]
    fn context_getters_return_the_exact_registered_arcs() {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let managed_spawner = Arc::new(ManagedAgentSpawner::new(
            Arc::new(std::sync::Mutex::new(SessionManager::new(event_tx))),
            ChildLimits::conservative(),
        ));
        let state_store: Arc<dyn harw_core::StateStore> = Arc::new(InMemoryStateStore::new());
        let mut services = ServiceMap::new();
        <OpContext as OpContextCoreExt>::register_agent_tool_services(
            &mut services,
            Arc::clone(&managed_spawner),
            Arc::clone(&state_store),
        );
        let (context, root) = context_with_services(services);

        let registered_spawner = context
            .managed_spawner()
            .expect("registered managed spawner must be retrievable");
        let registered_store = context
            .state_store()
            .expect("registered state store must be retrievable");
        std::fs::remove_dir_all(root).ok();

        assert!(Arc::ptr_eq(&registered_spawner, &managed_spawner));
        assert!(Arc::ptr_eq(&registered_store, &state_store));
    }
}
