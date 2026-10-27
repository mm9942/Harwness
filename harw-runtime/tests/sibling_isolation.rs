//! PL-90 W03d: Geschwister-Isolation der Kind-Registries.
//!
//! # Beschreibung
//! Zwei Kinder derselben Fabrik und desselben Elternteils sind der Fall, in
//! dem Zustand zwischen Geschwistern durchsickern könnte: ein zwischen den
//! Kindern geteilter Provider, ein gecachter Spawner oder eine für die Rolle
//! einmal gebaute und dann geteilte Identität. Diese Datei hält fest, dass die
//! Fabrik je Kind alles neu baut.
//!
//! # Grenzen
//! - Sitzungs-Ids entstehen im Spawner (`ManagedAgentSpawner::admit`), nicht in
//!   der Fabrik; ohne laufenden Spawner stellt dieses Fixture keine Kind-
//!   Sitzung bereit. Die Sitzungstrennung deckt `harw-core` ab.
//! - Die `ExtensionRegistry` hält keine Dienste. Die Identität eines Kindes
//!   liegt in den `OpContext`-Fabriken seiner Operations-Werkzeuge; hier
//!   erreichbar ist sie über [`RuntimeChildRegistryFactory::child_caller_identity`],
//!   dieselbe Funktion, die `build_registry` dafür aufruft. Dass die Instanz
//!   in den `ServiceMap`s landet, prüfen die Unit-Tests in
//!   `harw-runtime/src/children.rs`.
//! - Die Identität ist rollengebunden: Geschwister **derselben** Rolle tragen
//!   dieselbe `agent_id` (aber nie dieselbe Instanz). Ihre `SelfOnly`-Einträge
//!   sind füreinander sichtbar; Geschwister **verschiedener** Rollen sehen
//!   einander nie.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{TestError, TestResult, ctx};
use harw_core::{ChildRegistryFactory, EchoModelProvider};
use harw_extension_api::allow_rules::AllowRuleSet;
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_extension_api::contributors::ToolProvider;
use harw_extension_api::{AgentSpawner, ExtensionRegistry, SpawnInput};
use harw_knowledge::{AgentId, AgentRoleRef};
use harw_project_discovery::ProjectContext;
use harw_registry_defaults::profile::role_names;
use harw_runtime::approval::ApprovalChain;
use harw_runtime::children::RuntimeChildRegistryFactory;
use harw_runtime::spec::AskResolution;
use harw_types::{SessionId, ToolCallId};

/// Eine Fabrik ohne montierte Laufzeit: Echo-Modell, Projekt = dieses Crate.
fn factory() -> TestResult<RuntimeChildRegistryFactory> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project = ProjectContext {
        cwd: root.clone(),
        project_root: root,
        docs: Vec::new(),
    };
    let chain = ApprovalChain::for_root(
        &harw_config::ResolvedConfig::default(),
        AskResolution::Interactive,
        ApprovalModeCell::default(),
        None,
        AllowRuleSet::new(),
    );
    RuntimeChildRegistryFactory::new(project, Arc::new(EchoModelProvider::new("echo")), chain)
        .map_err(ctx("Fabrik"))
}

fn spawn_input(parent: &SessionId) -> SpawnInput {
    SpawnInput {
        parent_session_id: parent.clone(),
        handoff_call_id: ToolCallId::new(),
        instructions: None,
        context: serde_json::Value::Null,
        ceiling: None,
    }
}

fn build(
    factory: &RuntimeChildRegistryFactory,
    role: &str,
    parent: &SessionId,
) -> TestResult<ExtensionRegistry> {
    factory
        .build_registry(role, &spawn_input(parent), None)
        .map_err(|error| TestError::Unexpected(error.message))
}

/// Der Provider, der `delegate_wave` trägt (je Kind neu gebaut).
fn delegate_wave_provider(registry: &ExtensionRegistry) -> TestResult<&Arc<dyn ToolProvider>> {
    registry
        .tool_providers()
        .iter()
        .find(|provider| {
            provider
                .tools()
                .iter()
                .any(|spec| spec.name() == harw_core_bridge::DELEGATE_WAVE_TOOL)
        })
        .ok_or(TestError::Missing("delegate_wave-Provider"))
}

fn spawner_of(registry: &ExtensionRegistry) -> TestResult<&Arc<dyn AgentSpawner>> {
    registry
        .spawner()
        .ok_or(TestError::Missing("Spawner der Kind-Registry"))
}

#[test]
fn siblings_do_not_share_session_registry_or_identity() -> TestResult {
    let factory = factory()?;
    // Dasselbe Elternteil, dieselbe Fabrik: genau dort könnte Zustand
    // zwischen Geschwistern durchsickern.
    let parent = SessionId::new();

    // Zwei Geschwister derselben Rolle und ein Geschwister einer anderen.
    let twin_a = build(&factory, role_names::CODING_ORCHESTRATOR, &parent)?;
    let twin_b = build(&factory, role_names::CODING_ORCHESTRATOR, &parent)?;
    let other = build(&factory, role_names::RESEARCH_ORCHESTRATOR, &parent)?;

    // Registry: kein Operations-Provider und kein Spawner wird geteilt.
    let wave_a = delegate_wave_provider(&twin_a)?;
    let wave_b = delegate_wave_provider(&twin_b)?;
    let wave_other = delegate_wave_provider(&other)?;
    assert!(
        !Arc::ptr_eq(wave_a, wave_b),
        "Geschwister derselben Rolle teilen keinen Operations-Provider"
    );
    assert!(
        !Arc::ptr_eq(wave_a, wave_other),
        "Geschwister verschiedener Rollen teilen keinen Operations-Provider"
    );
    assert!(
        !Arc::ptr_eq(spawner_of(&twin_a)?, spawner_of(&twin_b)?),
        "Geschwister teilen keinen Spawner-Handle"
    );
    assert!(
        !Arc::ptr_eq(spawner_of(&twin_a)?, spawner_of(&other)?),
        "Geschwister teilen keinen Spawner-Handle"
    );

    // Identität: je Kind eine neue Instanz, nie eine geteilte.
    let identity_a = factory.child_caller_identity(role_names::CODING_ORCHESTRATOR);
    let identity_b = factory.child_caller_identity(role_names::CODING_ORCHESTRATOR);
    let identity_other = factory.child_caller_identity(role_names::RESEARCH_ORCHESTRATOR);
    assert!(
        !Arc::ptr_eq(&identity_a, &identity_b),
        "Geschwister derselben Rolle teilen keine Identitätsinstanz"
    );
    assert!(
        !Arc::ptr_eq(&identity_a, &identity_other),
        "Geschwister verschiedener Rollen teilen keine Identitätsinstanz"
    );
    assert_eq!(
        identity_a.agent_id,
        AgentId::new(role_names::CODING_ORCHESTRATOR)
    );
    assert_eq!(
        identity_other.agent_id,
        AgentId::new(role_names::RESEARCH_ORCHESTRATOR)
    );
    assert_ne!(
        identity_a.agent_id, identity_other.agent_id,
        "Geschwister verschiedener Rollen unterscheiden sich in der Agent-Id"
    );
    assert_eq!(
        identity_other.role,
        Some(AgentRoleRef::new(role_names::RESEARCH_ORCHESTRATOR))
    );
    // Bekannte Grenze (rollengebunden, siehe Moduldoku): dieselbe Rolle ergibt
    // dieselbe Agent-Id; unterschieden werden die Instanzen nur über das Objekt.
    assert_eq!(identity_a.agent_id, identity_b.agent_id);
    // Weder Vorfahren noch Operator-Recht werden von einem Geschwister geerbt.
    for identity in [&identity_a, &identity_b, &identity_other] {
        assert!(identity.ancestors.is_empty());
        assert!(!identity.operator);
    }
    Ok(())
}
