//! PL-90 W03d: Geschwister-Isolation der Kind-Registries.
//!
//! # Beschreibung
//! Zwei Kinder derselben Fabrik und desselben Elternteils sind der Fall, in
//! dem Zustand zwischen Geschwistern durchsickern könnte: ein zwischen den
//! Kindern geteilter Provider, ein gecachter Spawner oder eine für die Rolle
//! einmal gebaute und dann geteilte Identität. Diese Datei hält fest, dass die
//! Fabrik je Kind alles neu baut — auch die Agent-Id: sie ist laufgebunden
//! (`<rolle>.<uuid>`), zwei Geschwister **derselben** Rolle unterscheiden sich
//! also, und ein `SelfOnly`-Eintrag des einen ist für das andere unsichtbar.
//!
//! # Grenzen
//! - Sitzungs-Ids entstehen im Spawner (`ManagedAgentSpawner::admit`), nicht in
//!   der Fabrik; ohne laufenden Spawner stellt dieses Fixture keine Kind-
//!   Sitzung bereit. Die Sitzungstrennung deckt `harw-core` ab.
//! - Die `ExtensionRegistry` hält keine Dienste. Die Identität eines Kindes
//!   liegt in den `OpContext`-Fabriken seiner Operations-Werkzeuge; hier
//!   erreichbar ist sie über
//!   [`RuntimeChildRegistryFactory::pending_run_identity`] — dieselbe
//!   `Arc`-Instanz, die `build_registry` in diese Fabriken und in den
//!   Diary-Provider gelegt hat. Dass die Instanz in den `ServiceMap`s landet,
//!   prüfen die Unit-Tests in `harw-runtime/src/children.rs`.
//! - `ancestors` ist leer (`SpawnInput` trägt keine Elternrolle); das ist
//!   fail-closed und für die Geschwister-Isolation ohne Belang.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{TestError, TestResult, ctx};
use harw_core::{ChildRegistryFactory, EchoModelProvider};
use harw_extension_api::allow_rules::AllowRuleSet;
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_extension_api::contributors::ToolProvider;
use harw_extension_api::{AgentSpawner, ExtensionRegistry, SpawnInput};
use harw_knowledge::artifact::{
    ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact, RecallQuery,
};
use harw_knowledge::caller_identity::CallerIdentity;
use harw_knowledge::memory::recall::search;
use harw_knowledge::{AgentRoleRef, KnowledgeIndex, KnowledgeStore, VisibilityScope};
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

/// Ein Kind montieren: die Registry und die Identität, die `build_registry`
/// für genau dieses Kind gemintet hat.
fn build(
    factory: &RuntimeChildRegistryFactory,
    role: &str,
    parent: &SessionId,
) -> TestResult<(ExtensionRegistry, Arc<CallerIdentity>)> {
    let registry = factory
        .build_registry(role, &spawn_input(parent), None)
        .map_err(|error| TestError::Unexpected(error.message))?;
    let identity = factory
        .pending_run_identity(role)
        .ok_or(TestError::Missing(
            "vorgemerkte Identität nach build_registry",
        ))?;
    Ok((registry, identity))
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
    let (twin_a, identity_a) = build(&factory, role_names::CODING_ORCHESTRATOR, &parent)?;
    let (twin_b, identity_b) = build(&factory, role_names::CODING_ORCHESTRATOR, &parent)?;
    let (other, identity_other) = build(&factory, role_names::RESEARCH_ORCHESTRATOR, &parent)?;

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

    // Identität: je Kind eine neue Instanz mit eigener Agent-Id — auch bei
    // gleicher Rolle.
    assert!(
        !Arc::ptr_eq(&identity_a, &identity_b),
        "Geschwister derselben Rolle teilen keine Identitätsinstanz"
    );
    assert!(
        !Arc::ptr_eq(&identity_a, &identity_other),
        "Geschwister verschiedener Rollen teilen keine Identitätsinstanz"
    );
    assert_ne!(
        identity_a.agent_id, identity_b.agent_id,
        "Geschwister derselben Rolle haben verschiedene Agent-Ids"
    );
    assert_ne!(
        identity_a.agent_id, identity_other.agent_id,
        "Geschwister verschiedener Rollen haben verschiedene Agent-Ids"
    );
    let coding_prefix = format!("{}.", role_names::CODING_ORCHESTRATOR);
    let research_prefix = format!("{}.", role_names::RESEARCH_ORCHESTRATOR);
    assert!(identity_a.agent_id.as_str().starts_with(&coding_prefix));
    assert!(identity_b.agent_id.as_str().starts_with(&coding_prefix));
    assert!(
        identity_other
            .agent_id
            .as_str()
            .starts_with(&research_prefix)
    );
    // Die Rolle ist das gemeinsame Merkmal (Rollenfreigaben gelten für alle
    // Geschwister dieser Rolle), nicht die Agent-Id.
    assert_eq!(
        identity_a.role,
        Some(AgentRoleRef::new(role_names::CODING_ORCHESTRATOR))
    );
    assert_eq!(identity_a.role, identity_b.role);
    assert_eq!(
        identity_other.role,
        Some(AgentRoleRef::new(role_names::RESEARCH_ORCHESTRATOR))
    );
    // Weder Vorfahren noch Operator-Recht werden von einem Geschwister geerbt.
    for identity in [&identity_a, &identity_b, &identity_other] {
        assert!(identity.ancestors.is_empty());
        assert!(!identity.operator);
        assert!(
            harw_knowledge::diary::validate_agent_id(&identity.agent_id).is_ok(),
            "die Agent-Id ist eine sichere Pfadkomponente"
        );
    }
    Ok(())
}

/// Ende-zu-Ende: zwei Kinder **derselben** Rolle; ein `SelfOnly`-Artefakt, das
/// unter der Agent-Id von Kind A im Wissensspeicher liegt, ist für A auffindbar
/// und für B nicht — obwohl beide dieselbe Rolle haben.
#[test]
fn same_role_siblings_cannot_recall_each_others_self_only_artifacts() -> TestResult {
    let factory = factory()?;
    let parent = SessionId::new();
    let (_registry_a, identity_a) = build(&factory, role_names::EXPLORER, &parent)?;
    let (_registry_b, identity_b) = build(&factory, role_names::EXPLORER, &parent)?;
    assert_ne!(identity_a.agent_id, identity_b.agent_id);
    assert_eq!(identity_a.role, identity_b.role, "gleiche Rolle");

    // Kind A schreibt eine private Notiz unter seiner eigenen Agent-Id.
    let root = tempfile::tempdir().map_err(ctx("temporary knowledge root"))?;
    let store = KnowledgeStore::new(root.path());
    let frontmatter = Frontmatter::new(
        identity_a.agent_id.clone(),
        VisibilityScope::SelfOnly,
        jiff::Timestamp::UNIX_EPOCH,
    );
    let note = KnowledgeArtifact::new(
        ArtifactId::new("topic/private-canary"),
        ArtifactKind::TopicMemory,
        frontmatter,
        "canary deploy notes of child A",
    );
    store
        .write_artifact(&store.topic_path("private-canary"), &note)
        .map_err(ctx("Notiz von Kind A schreiben"))?;
    let (index, report) =
        KnowledgeIndex::rebuild_with_report(&store).map_err(ctx("Index neu aufbauen"))?;
    assert!(
        report.is_clean() && report.indexed == 1,
        "die Notiz von A ist indiziert: {report:?}"
    );

    let recall = |identity: &Arc<CallerIdentity>| -> TestResult<usize> {
        let query = RecallQuery::new("canary", VisibilityScope::SelfOnly)
            .with_caller_identity((**identity).clone());
        let result = search(&index, &query).map_err(ctx("Recall"))?;
        Ok(result.hits.len())
    };

    assert_eq!(recall(&identity_a)?, 1, "A findet seine eigene Notiz");
    assert_eq!(
        recall(&identity_b)?,
        0,
        "B (gleiche Rolle) findet die SelfOnly-Notiz von A nicht"
    );
    // Ohne gebundene Identität bleibt die Skopus-Heuristik fail-closed.
    let anonymous = RecallQuery::new("canary", VisibilityScope::SelfOnly);
    let result = search(&index, &anonymous).map_err(ctx("Recall ohne Identität"))?;
    assert!(result.hits.is_empty());
    Ok(())
}
