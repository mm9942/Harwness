//! Die eine Kind-Registry-Fabrik der Runtime (`RuntimeChildRegistryFactory`).
//!
//! # Beschreibung
//! Bis zu dieser Welle montierten drei Einstiege ihre Kind-Registries je
//! selbst: `harw-tui/src/app.rs:1649-1793` (`TuiChildRegistryFactory`),
//! `harw-cli/src/chat.rs:499-591` (`OneShotChildRegistryFactory`) und
//! `harw-cli/src/job_worker.rs:~924` (Plan-Knoten). Alle drei teilten sich
//! zwei Befunde:
//!
//! 1. **Fallback statt Fehler (R4).** `profile_for_role(role).unwrap_or_default()`
//!    (`harw-tui/src/app.rs:1732`, `harw-cli/src/chat.rs:575`) gab einer
//!    *unbekannten* Rolle — auch einer aus einer repo-lokalen
//!    `./.harw/agents/*/agent.toml` — das vollständige Coding-Profil
//!    inklusive `fs.write` und `shell.exec`. Seit W2A-04 hat
//!    [`RegistryProfile`] kein `Default` mehr; diese Fabrik scheitert
//!    stattdessen (fail-closed).
//! 2. **Zweite Projekterkennung (G-071/R5).** Beide riefen
//!    `assemble_registry(profile, self.discovery_cwd.clone(), overrides)`
//!    auf, das je Kind erneut `discover_project` ausführte — ein Kind konnte
//!    damit in einem anderen Projekt-Root landen als sein Elternteil (die TUI
//!    prüfte das nach, die CLI gar nicht). Diese Fabrik hält den
//!    [`ProjectContext`] des Elternteils und benutzt (seit Teil B4)
//!    `assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits`
//!    (`harw_registry_defaults::profile`), das **keinen Pfad** entgegennimmt
//!    und deshalb gar nicht erkennen *kann*.
//! 3. **Kette nicht vererbt (F-018).** `assemble_registry_for_project`
//!    registriert nur die `DefaultApprovalPolicy`. Die Config-Politik des
//!    Elternteils erreichte den Fan-out nie. Diese Fabrik installiert
//!    [`ApprovalChain::for_child`] in **jede** Kind-Registry.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};

use harw_agent_dsl::ExecutableAgentIr;
use harw_catalog::{
    CatalogSnapshot, SkillRuntimeSnapshot, SpawnCapabilitySnapshot, load_skill_runtime_snapshot,
    resolve_skill_directory,
};
use harw_config::{
    InternalModelPoint, ResolvedConfig, ResolvedInternalModel, SkillToml, resolve_internal_model,
};
use harw_core::{
    ChildRegistryFactory, ManagedAgentSpawner, ModelProvider, PinnedModelProvider, StateStore,
};
use harw_extension_api::{
    AgentSpawnError, AgentSpawner, ExtensionRegistry, SpawnFuture, SpawnInput,
};
use harw_operations::OpContext;
use harw_operations::adapter::ModelToolProvider;
use harw_operations::context::ServiceMap;
use harw_operations::operation::Operation;
use harw_project_discovery::ProjectContext;
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    HostPermitWiring, IdentityOverrides,
    assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits,
    composition_tools_for_role, profile_for_role, role_names,
};

use crate::approval::ApprovalChain;
use crate::error::{RuntimeError, RuntimeResult};

/// Bildet eine Kind-Rolle auf ihre interne Modellstelle ab (Addendum C).
///
/// # Description
/// Reine Zuordnungsfunktion, unabhängig von jeder Konfiguration: `explorer`
/// → [`InternalModelPoint::Explorer`]; die drei Recherche-Rollen
/// `researcher-web`, `researcher-deps` und `analyst` (sie teilen sich den
/// Recherche-Befund-Vertrag) → [`InternalModelPoint::Research`];
/// `memory-steward` → [`InternalModelPoint::MemoryConsolidation`];
/// `agent-steward` (Addendum K, eigene Organisationsrolle
/// `AgentRoleId::AgentSteward`) → [`InternalModelPoint::WorkerComplex`]:
/// Validieren, Rechte-Delta und Vorschlagsentscheidung verlangen eigenes
/// Urteilsvermögen, auch wenn der Auftrag klein aussieht.
///
/// Die gesamte `uia-worker`-Rollenfamilie (`AgentRoleId::UiaWorker`:
/// [`role_names::UIA_WORKER`], [`role_names::UIA_EXPLORER`],
/// [`role_names::UIA_WRITER`], [`role_names::UIA_SHELL_WORKER`],
/// [`role_names::UIA_LATEX_WRITER`]) fällt **absichtlich** auf `None` durch (Welle 3a) — sie hängt seither nicht mehr
/// an einer über [`resolve_internal_models_for_children`] aufgelösten
/// internen Modellstelle des Eltern-Modells, sondern bekommt ihr eigenes,
/// von der UIA-Sitzung abgeleitetes Modell direkt über die eigene
/// Kind-Registry-Fabrik der Rolle (`RuntimeAssemblyBuilder::build`,
/// `build_spawner`s `uia_worker_factory`; das Modell selbst entsteht über
/// [`crate::model::build_uia_model_with_resolver`] +
/// [`crate::model::build_uia_worker_model`]). Jede andere Rolle (inklusive
/// `planner`, `executor`, `security-*` und unbekannter/repo-lokaler Rollen)
/// liefert hier `None`. Orchestrator-Rollen bekommen ihre Stelle nicht über
/// den Namen, sondern über die Organisationsrolle ihrer eingebauten
/// Definition ([`orchestrator_point_for_organizational_role`], R1); die
/// Fabrik kombiniert beide Zuordnungen.
///
/// # Returns
/// `Some(point)` für eine der oben genannten Rollen, sonst `None`.
pub fn internal_point_for_role(role: &str) -> Option<InternalModelPoint> {
    match role {
        r if r == role_names::EXPLORER => Some(InternalModelPoint::Explorer),
        r if r == role_names::RESEARCHER_WEB
            || r == role_names::RESEARCHER_DEPS
            || r == role_names::ANALYST =>
        {
            Some(InternalModelPoint::Research)
        }
        r if r == role_names::MEMORY_STEWARD => Some(InternalModelPoint::MemoryConsolidation),
        r if r == role_names::AGENT_STEWARD => Some(InternalModelPoint::WorkerComplex),
        _ => None,
    }
}

/// Bildet die Organisationsrolle einer Orchestrator-Definition auf ihre
/// interne Modellstelle ab (R1).
///
/// # Description
/// Reine Zuordnung über die **gesenkte** Organisationsrolle
/// ([`ExecutableAgentIr::role`]), nicht über den Rollennamen: jede
/// Definition mit `role = "root-orchestrator"` →
/// [`InternalModelPoint::RootOrchestrator`], jede mit
/// `role = "child-orchestrator"` (darunter `coding-orchestrator`,
/// `research-orchestrator`, `analysis-orchestrator`) →
/// [`InternalModelPoint::SubOrchestrator`]. Ohne explizite Wahl lösen beide
/// Stellen auf das Hauptmodell auf
/// ([`InternalModelPoint::uses_openrouter_default`] ist für sie `false`) —
/// das Verhalten bleibt dann das bisherige Eltern-Modell.
///
/// # Arguments
/// - `role` (`harw_agent_dsl::roles::AgentRoleId`): die Organisationsrolle.
///
/// # Returns
/// `Some(point)` für die beiden Orchestrator-Rollen, sonst `None`.
#[must_use]
pub fn orchestrator_point_for_organizational_role(
    role: harw_agent_dsl::roles::AgentRoleId,
) -> Option<InternalModelPoint> {
    use harw_agent_dsl::roles::AgentRoleId;

    match role {
        AgentRoleId::RootOrchestrator => Some(InternalModelPoint::RootOrchestrator),
        AgentRoleId::ChildOrchestrator => Some(InternalModelPoint::SubOrchestrator),
        AgentRoleId::UserInterface
        | AgentRoleId::Worker
        | AgentRoleId::UiaWorker
        | AgentRoleId::AgentSteward => None,
    }
}

/// Ob die Kind-Registry von `role` die `delegate_wave`-Fläche bekommt.
///
/// # Description
/// Genau dann, wenn die Composition-Tools der Rolle
/// ([`composition_tools_for_role`]) [`harw_core_bridge::DELEGATE_WAVE_TOOL`]
/// enthalten — heute der Root-Orchestrator und jeder Child-Orchestrator.
/// Worker bekommen nie eine Delegationsoberfläche.
#[must_use]
pub fn role_gets_delegate_wave(role: &str) -> bool {
    composition_tools_for_role(role).contains(&harw_core_bridge::DELEGATE_WAVE_TOOL)
}

/// Deckelt die höchstens gleichzeitig laufende Anzahl Instanzen einer Rolle
/// (Welle 6a, Singleton-Erzwingung).
///
/// # Description
/// Der Nutzer verlangt, dass UIA und die gesamte `uia-worker`-Rollenfamilie
/// (`AgentRoleId::UiaWorker`: [`role_names::UIA_WORKER`],
/// [`role_names::UIA_EXPLORER`], [`role_names::UIA_WRITER`],
/// [`role_names::UIA_SHELL_WORKER`], [`role_names::UIA_LATEX_WRITER`]) **nie**
/// mit mehr als einer gleichzeitig
/// laufenden Instanz gefanoutet werden dürfen — unabhängig vom
/// Aufrufer-`max_parallel`-Wert (`analyze(max_parallel: N)`, `explore`,
/// `delegate_task` o. ä.). Diese Funktion selbst deckelt **nichts**: sie ist
/// die reine, getestete Zuordnungsregel, die ein späterer Aufrufer außerhalb
/// dieses Crates (`harw-core/src/child_controller.rs::run_children`,
/// `harw-core-bridge/src/agent_tool.rs::fanout_children` — beide liegen
/// außerhalb dieser Welle) auslesen und seinen `max_parallel`-Parameter
/// darauf klemmen kann, z. B.
/// `let effective = max_parallel.min(max_concurrent_instances_for_role(role, &definitions));`.
///
/// Die Organisationsrolle wird genau wie an jeder anderen Stelle dieser
/// Datei ermittelt ([`ChildRegistryFactory::build_registry`],
/// `assembly.rs::build_spawner`s Registrierungsschleife): über die
/// eingebaute, gesenkte [`ExecutableAgentIr`] der Rolle, fail-closed auf
/// [`AgentRoleId::Worker`] für eine unbekannte/nicht eingebaute Rolle. Das
/// trifft automatisch alle vier UIA-Spezialisierungen, ohne eine Namensliste
/// zu pflegen.
///
/// # Arguments
/// - `role` (`&str`): der registrierte Rollenname.
/// - `definitions` (`&HashMap<String, ExecutableAgentIr>`): die gesenkten
///   eingebauten Rollen — dieselbe Quelle, die
///   [`RuntimeChildRegistryFactory::build_registry`] und
///   `assembly.rs::build_spawner` für die Organisationsrolle einer Rolle
///   lesen (`RuntimeChildRegistryFactory::builtin_definitions` bzw.
///   `SpawnerInputs::definitions`).
///
/// # Returns
/// `1`, wenn die Organisationsrolle von `role` laut `definitions`
/// [`AgentRoleId::UiaWorker`] ist; sonst `usize::MAX` — keine zusätzliche
/// Deckelung für jede andere Rolle, inklusive unbekannter Rollen.
#[must_use]
pub fn max_concurrent_instances_for_role(
    role: &str,
    definitions: &HashMap<String, ExecutableAgentIr>,
) -> usize {
    let organizational_role = definitions.get(role).map_or(
        harw_agent_dsl::roles::AgentRoleId::Worker,
        ExecutableAgentIr::role,
    );
    if organizational_role == harw_agent_dsl::roles::AgentRoleId::UiaWorker {
        1
    } else {
        usize::MAX
    }
}

/// Wählt den Schreibmodus des `agent-steward` nach der Organisationsrolle
/// seines unmittelbaren Elternteils (Nachtrag K2).
///
/// # Description
/// Nur ein von der UIA selbst gestarteter Steward committet dauerhafte
/// Definitionen sofort (`DefinitionWriteMode::Commit`); jeder andere
/// Elternteil — inklusive Root-Orchestrator und einer unbekannten/nicht
/// aufgelösten Rolle — bleibt fail-closed im Vorschlagsmodus
/// (`DefinitionWriteMode::ProposalOnly`), dessen Ergebnisse erst nach
/// UIA-Prüfung wirksam werden.
///
/// # Verdrahtung (Welle FANIN-K/FANIN-RT, schließt die vorherige Lücke)
/// Diese reine, getestete Zuordnungsregel wird jetzt an der Montagestelle
/// aufgerufen:
/// [`RuntimeChildRegistryFactory::build_registry_with_capabilities_for_parent`]
/// ruft sie mit `parent.role` aus dem vom Spawner gereichten
/// [`harw_core::ParentGrant`] auf, um den `mode` der
/// [`harw_registry_defaults::profile::AgentDefinitionAccess`] eines
/// `agent-steward`-Kindes zu bestimmen.
///
/// # Arguments
/// - `parent_role` (`Option<harw_agent_dsl::roles::AgentRoleId>`): die
///   Organisationsrolle des unmittelbaren Elternteils, sofern bekannt.
///
/// # Returns
/// `DefinitionWriteMode::Commit` nur für
/// `Some(AgentRoleId::UserInterface)`, sonst
/// `DefinitionWriteMode::ProposalOnly`.
#[must_use]
pub fn definition_write_mode_for_parent_role(
    parent_role: Option<harw_agent_dsl::roles::AgentRoleId>,
) -> harw_registry_defaults::agent_definition_tools::DefinitionWriteMode {
    use harw_agent_dsl::roles::AgentRoleId;
    use harw_registry_defaults::agent_definition_tools::DefinitionWriteMode;

    match parent_role {
        Some(AgentRoleId::UserInterface) => DefinitionWriteMode::Commit,
        _ => DefinitionWriteMode::ProposalOnly,
    }
}

/// Löst die für Kind-Rollen relevanten internen Modellstellen einmalig gegen
/// die aufgelöste Konfiguration auf.
///
/// # Description
/// Wird an der Montagestelle aufgerufen (`harw-runtime/src/assembly.rs`,
/// `build_spawner`) und das Ergebnis über
/// [`RuntimeChildRegistryFactory::with_internal_models`] in die Fabrik
/// gegeben. Löst die Stellen auf, die [`internal_point_for_role`] zuordnet
/// ([`InternalModelPoint::Explorer`], [`InternalModelPoint::Research`],
/// [`InternalModelPoint::MemoryConsolidation`]), sowie zusätzlich die beiden
/// Worker-Modellstufen ([`InternalModelPoint::WorkerSimple`],
/// [`InternalModelPoint::WorkerComplex`], Addendum D+E) und die beiden
/// Orchestrator-Stellen ([`InternalModelPoint::RootOrchestrator`],
/// [`InternalModelPoint::SubOrchestrator`], R1, über
/// [`orchestrator_point_for_organizational_role`]) — die übrigen
/// Stellen (`SessionTitle`, `CompactionSummary`, `DreamReflection`) haben
/// eigene Aufrufstellen außerhalb dieser Fabrik.
///
/// # Arguments
/// - `config` (`&harw_config::ResolvedConfig`): die aufgelöste Konfiguration
///   des Elternteils.
///
/// # Returns
/// Eine Abbildung von Stelle auf ihr aufgelöstes Ergebnis, fertig zum
/// Nachschlagen in [`RuntimeChildRegistryFactory::model_for`].
#[must_use]
pub fn resolve_internal_models_for_children(
    config: &harw_config::ResolvedConfig,
) -> HashMap<InternalModelPoint, ResolvedInternalModel> {
    [
        InternalModelPoint::Explorer,
        InternalModelPoint::Research,
        InternalModelPoint::MemoryConsolidation,
        InternalModelPoint::WorkerSimple,
        InternalModelPoint::WorkerComplex,
        InternalModelPoint::RootOrchestrator,
        InternalModelPoint::SubOrchestrator,
    ]
    .into_iter()
    .map(|point| (point, resolve_internal_model(config, point)))
    .collect()
}

/// Baut die Werkzeug-Registry jedes Kind-Agenten eines Runtime-Laufs.
///
/// # Nebenläufigkeit
/// `Send + Sync`: alle Felder sind nach der Konstruktion unveränderlich; die
/// einzige Zustandsänderung pro Aufruf ist die frische
/// [`ApprovalModeCell`](harw_extension_api::approval_mode::ApprovalModeCell)
/// in [`ApprovalChain::for_child`], die das Kind allein besitzt.
pub struct RuntimeChildRegistryFactory {
    /// Der **einmal** ermittelte Projektkontext des Elternteils.
    project: ProjectContext,
    /// Der Modellanbieter des Eltern-Turns; jedes Kind benutzt denselben.
    model: Arc<dyn ModelProvider>,
    /// Die eingebauten Rollen, einmalig zu [`ExecutableAgentIr`] gesenkt.
    builtin_definitions: HashMap<String, ExecutableAgentIr>,
    /// Die Freigabekette der Wurzel; je Kind wird daraus
    /// [`ApprovalChain::for_child`] abgeleitet.
    chain: ApprovalChain,
    /// Aufgelöste interne Modellstellen (Addendum C), je Rolle über
    /// [`internal_point_for_role`] nachgeschlagen. Leer, solange
    /// [`Self::with_internal_models`] nicht aufgerufen wurde — `model_for`
    /// fällt dann für jede Rolle unverändert auf das Eltern-Modell zurück.
    internal_models: HashMap<InternalModelPoint, ResolvedInternalModel>,
    /// Das Agentendefinitions-Verzeichnis des aktiven Profils
    /// (`<profil>/agents`, Welle FANIN-K/FANIN-RT, Fan-in-Zusatzpunkt aus
    /// K-B/K-C: „`profile_agents_dir` ist immer `None` [in
    /// `harw-registry-defaults`] — im Runtime-Pfad setzen"). `None`, solange
    /// [`Self::with_profile_agents_dir`] nicht aufgerufen wurde oder kein
    /// Profil ermittelbar war; nur für [`role_names::AGENT_STEWARD`]
    /// relevant ([`Self::build_registry_with_capabilities_for_parent`]).
    profile_agents_dir: Option<std::path::PathBuf>,
    /// Die aufgelöste `[browser]`-Konfiguration des Elternlaufs. Sie wird nur
    /// für die exklusive `uia-worker`-Registry ausgewertet.
    browser: harw_config::BrowserSection,
    /// Rückwärtsreferenz auf den fertigen Managed-Spawner. Sie erlaubt auch
    /// einem gestarteten Root-Orchestrator, seine eigenen Kinder zu starten,
    /// obwohl die Kind-Registry gebaut wird, bevor der Spawner selbst in der
    /// Assembly vollständig konstruiert ist.
    spawner_slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>>,
    /// Die aufgelöste Config des Elternlaufs, nur für
    /// [`ChildRegistryFactory::reasoning_effort_defaults_for_role_task`]
    /// (Welle 8: Rangfolge Provider > Modell > Agent > Rolle) — liefert die
    /// `providers`-/`models`-Tabellen, in denen `default_reasoning_effort`
    /// je Provider-/Modell-ID hinterlegt ist. `None`, solange
    /// [`Self::with_reasoning_effort_config`] nicht aufgerufen wurde; die
    /// Trait-Methode fällt dann für jede Rolle auf `(None, None)` zurück
    /// (Kompatibilitäts-Default, § dort) — dieselbe fail-open-Bedeutung wie
    /// bei [`Self::internal_models`].
    reasoning_effort_config: Option<Arc<harw_config::ResolvedConfig>>,
    /// Explizite Provider-/Modell-Auswahl für den Hauptmodell-Fallback
    /// (`ResolvedInternalModel::is_main_model`) in
    /// [`Self::reasoning_effort_defaults_for_point`] (Teil D, schließt die
    /// Effort-Lücke für Kinder auf dem Hauptmodell). `None`, solange
    /// [`Self::with_main_model_selection`] nicht aufgerufen wurde — die
    /// Auswahl wird dann bei jedem Nachschlagen direkt aus
    /// `config.harness.default_provider`/`default_model` abgeleitet
    /// (derselbe Default, den eine explizit mit diesen Werten aufgerufene
    /// Fabrik ergäbe).
    main_model_selection: Option<(Option<String>, Option<String>)>,
    /// Die Modellkennung, die der **ungepinnte** Kind-Provider dieser Fabrik
    /// tatsächlich anspricht (Teil C, [`Self::with_effective_main_model`]):
    /// Grundlage für das Kontextfenster eines Kindes ohne Rollen-Pin.
    /// `None`: keine Aussage, der Spawner folgt dem Modell des Elternteils.
    effective_main_model: Option<String>,
    /// Das Sandbox-Profil, mit dem jeder für ein Kind gebaute
    /// [`harw_tool_shell::ShellToolProvider`] montiert wird (Teil B4). Vorgabe
    /// [`harw_sandbox::SandboxProfile::Strict`] — bit-identisch zum
    /// Verhalten vor dieser Ergänzung — solange
    /// [`Self::with_host_permits`] nicht aufgerufen wurde.
    sandbox_profile: harw_sandbox::SandboxProfile,
    /// Die einmal je Lauf instanziierte Host-Permit-Verdrahtung (Ledger,
    /// Sitzungs-Registry, Fragekanal-Sender), die an jeden mit
    /// [`harw_sandbox::SandboxProfile::Host`] gebauten
    /// [`harw_tool_shell::ShellToolProvider`] gehängt wird (Teil B4). `None`,
    /// solange [`Self::with_host_permits`] nicht aufgerufen wurde — Host-
    /// Ausführung bleibt dann fail-closed, genau wie vor dieser Ergänzung.
    host_permit_wiring: Option<HostPermitWiring>,
    /// Eingefrorener Skill-Katalog samt Skill-Wurzeln (Welle 4, „Skills
    /// erreichen Agenten“). `None`, solange [`Self::with_skill_catalog`]
    /// nicht aufgerufen wurde — dann bekommt kein Kind Skill-Fragmente und
    /// [`ChildRegistryFactory::capability_snapshot`] bleibt `None`, genau wie
    /// vor dieser Ergänzung.
    skill_catalog: Option<SkillCatalogWiring>,
    /// Der geteilte `StateStore` des Laufs für die `delegate_wave`-Fläche der
    /// Orchestrator-Kinder (R1/B). `None`, solange
    /// [`Self::with_delegate_wave_store`] nicht aufgerufen wurde — die
    /// Operation wird dennoch montiert, scheitert dann aber beim Aufruf
    /// fail-closed mit `OpError::NotAvailable` (kein StateStore).
    delegate_wave_store: Option<Arc<dyn StateStore>>,
    /// Wissensspeicher (und optional Kanban-Ledger) für die lesenden
    /// Wissenswerkzeuge der Kinder (Plan Teil D). `None`, solange
    /// [`Self::with_knowledge`] nicht aufgerufen wurde — dann bekommt kein
    /// Kind `workbench.show`/`diary.read`/`palace.*`/`kanban.*`.
    knowledge: Option<ChildKnowledgeWiring>,
}

/// Wissensquellen der lesenden Kind-Werkzeuge (Plan Teil D).
#[derive(Clone)]
struct ChildKnowledgeWiring {
    store: Arc<harw_knowledge::KnowledgeStore>,
    kanban_ledger: Option<Arc<dyn harw_knowledge::kanban::lifecycle::JobTransitions>>,
}

/// Der Skill-Katalog einer Kind-Fabrik: ein über die ganze Lebensdauer des
/// Spawners unveränderlicher [`CatalogSnapshot`] und die vertrauten
/// Config-Layer, aus denen die Skill-Verzeichnisse aufgelöst werden.
#[derive(Debug, Clone)]
struct SkillCatalogWiring {
    catalog: CatalogSnapshot,
    roots: Vec<PathBuf>,
}

impl std::fmt::Debug for RuntimeChildRegistryFactory {
    /// Zeigt nur Kennzahlen: `ModelProvider` ist ein Trait-Objekt ohne
    /// `Debug`, und die gesenkten Rollen gehören nicht in ein Log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeChildRegistryFactory")
            .field("project_root", &self.project.project_root)
            .field("builtin_roles", &self.builtin_definitions.len())
            .field("chain", &self.chain)
            .finish()
    }
}

/// Quelle der lesenden Wissenswerkzeuge für Kinder (Plan Teil D):
/// Wissensspeicher plus optionales Kanban-Ledger.
pub type ChildKnowledgeSource = (
    Arc<harw_knowledge::KnowledgeStore>,
    Option<Arc<dyn harw_knowledge::kanban::lifecycle::JobTransitions>>,
);

impl RuntimeChildRegistryFactory {
    /// Senkt die eingebauten Agentendefinitionen einmalig und hält sie für die
    /// Lebensdauer des Spawners.
    ///
    /// # Argumente
    /// - `project` ([`ProjectContext`]): der bereits erkannte Projektkontext
    ///   des Elternteils. Eigentum geht über; es wird **nie** erneut erkannt.
    /// - `model` (`Arc<dyn ModelProvider>`): der Anbieter des Eltern-Turns.
    /// - `chain` ([`ApprovalChain`]): die Wurzelkette.
    ///
    /// # Rückgabe
    /// `Ok(Self)` mit allen eingebauten Rollen gesenkt.
    ///
    /// # Fehler
    /// [`RuntimeError::Registry`], wenn eine eingebettete Agentendefinition
    /// nicht senkt.
    pub fn new(
        project: ProjectContext,
        model: Arc<dyn ModelProvider>,
        chain: ApprovalChain,
    ) -> RuntimeResult<Self> {
        let builtin_definitions =
            builtin_agent_definitions(&HashMap::new()).map_err(|error| RuntimeError::Registry {
                detail: format!("could not lower builtin agent definitions: {error}"),
            })?;
        Ok(Self::with_definitions(
            project,
            model,
            chain,
            builtin_definitions,
        ))
    }

    /// Wie [`Self::new`], aber mit bereits gesenkten Definitionen.
    ///
    /// # Beschreibung
    /// [`builtin_agent_definitions`] senkt bei jedem Aufruf den kompletten
    /// eingebetteten Rollensatz. Die Montage braucht ihn ohnehin schon, um
    /// `--agent` aufzulösen (`assembly::resolve_active_agent`); sie reicht das
    /// Ergebnis hier herein, statt dieselbe Arbeit ein zweites Mal zu tun
    /// (Befund Z2c-07).
    ///
    /// # Argumente
    /// - `definitions` (`HashMap<String, ExecutableAgentIr>`): die gesenkten
    ///   Rollen; Eigentum geht über. Der Aufrufer haftet dafür, dass sie aus
    ///   [`builtin_agent_definitions`] stammen — diese Fabrik prüft es nicht
    ///   und kann es nicht prüfen.
    #[must_use]
    pub fn with_definitions(
        project: ProjectContext,
        model: Arc<dyn ModelProvider>,
        chain: ApprovalChain,
        definitions: HashMap<String, ExecutableAgentIr>,
    ) -> Self {
        Self {
            project,
            model,
            builtin_definitions: definitions,
            chain,
            internal_models: HashMap::new(),
            profile_agents_dir: None,
            browser: harw_config::BrowserSection::default(),
            spawner_slot: Arc::new(OnceLock::new()),
            reasoning_effort_config: None,
            main_model_selection: None,
            effective_main_model: None,
            sandbox_profile: harw_sandbox::SandboxProfile::Strict,
            host_permit_wiring: None,
            skill_catalog: None,
            delegate_wave_store: None,
            knowledge: None,
        }
    }

    /// Ergänzt die aufgelösten internen Modellstellen (Addendum C).
    ///
    /// # Description
    /// Reiner Erbauer-Schritt: [`Self::new`]/[`Self::with_definitions`]
    /// bleiben unverändert, damit bestehende Aufrufer (Montage in
    /// `assembly.rs`) nur diese eine zusätzliche Kettung ergänzen müssen.
    /// Ohne Aufruf bleibt [`Self::internal_models`] leer und `model_for`
    /// liefert für jede Rolle unverändert das Eltern-Modell — bit-identisch
    /// zum bisherigen Verhalten.
    ///
    /// # Arguments
    /// - `models` (`HashMap<InternalModelPoint, ResolvedInternalModel>`):
    ///   typischerweise das Ergebnis von
    ///   [`resolve_internal_models_for_children`].
    #[must_use]
    pub fn with_internal_models(
        mut self,
        models: HashMap<InternalModelPoint, ResolvedInternalModel>,
    ) -> Self {
        self.internal_models = models;
        self
    }

    /// Ergänzt das Agentendefinitions-Verzeichnis des aktiven Profils
    /// (`<profil>/agents`).
    ///
    /// # Description
    /// Reiner Erbauer-Schritt, analog [`Self::with_internal_models`]:
    /// [`Self::new`]/[`Self::with_definitions`] bleiben unverändert. Der
    /// Montage-Aufrufer (`assembly.rs::build_spawner`) kennt `home` und das
    /// aktive Profil bereits (`harw_home::paths::active_profile_name` +
    /// [`harw_home::paths::profile_dir`]); diese Fabrik hält nur das
    /// Ergebnis, ohne selbst eine `harw-home`-Abhängigkeit zu benötigen.
    /// Ohne Aufruf bleibt [`Self::profile_agents_dir`] `None` — dieselbe
    /// fail-closed-Bedeutung wie ein nicht ermittelbares Profilverzeichnis.
    ///
    /// # Arguments
    /// - `profile_agents_dir` (`Option<std::path::PathBuf>`): typischerweise
    ///   `profile_dir(home, profile_name).ok().map(|dir| dir.join("agents"))`.
    #[must_use]
    pub fn with_profile_agents_dir(
        mut self,
        profile_agents_dir: Option<std::path::PathBuf>,
    ) -> Self {
        self.profile_agents_dir = profile_agents_dir;
        self
    }

    /// Übernimmt die aufgelöste `[browser]`-Konfiguration des Elternlaufs.
    #[must_use]
    pub fn with_browser_config(mut self, browser: harw_config::BrowserSection) -> Self {
        self.browser = browser;
        self
    }

    /// Verbindet die Kind-Registries mit dem später fertig gebauten Spawner.
    #[must_use]
    pub fn with_spawner_slot(
        mut self,
        spawner_slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>>,
    ) -> Self {
        self.spawner_slot = spawner_slot;
        self
    }

    /// Ergänzt die aufgelöste Config für Provider-/Modell-Reasoning-Effort-
    /// Defaults (Welle 8).
    ///
    /// # Description
    /// Reiner Erbauer-Schritt, analog [`Self::with_internal_models`]:
    /// [`Self::new`]/[`Self::with_definitions`] bleiben unverändert. Ohne
    /// Aufruf bleibt [`Self::reasoning_effort_config`] `None` und
    /// [`ChildRegistryFactory::reasoning_effort_defaults_for_role_task`]
    /// liefert für jede Rolle `(None, None)` — bit-identisch zum
    /// Kompatibilitäts-Default der Trait-Methode.
    ///
    /// # Arguments
    /// - `config` (`Arc<harw_config::ResolvedConfig>`): dieselbe aufgelöste
    ///   Konfiguration, aus der auch [`resolve_internal_models_for_children`]
    ///   gespeist wird.
    #[must_use]
    pub fn with_reasoning_effort_config(
        mut self,
        config: Arc<harw_config::ResolvedConfig>,
    ) -> Self {
        self.reasoning_effort_config = Some(config);
        self
    }

    /// Legt die Provider-/Modell-Auswahl fest, die für den Hauptmodell-
    /// Fallback (`resolved.is_main_model()`) in
    /// [`Self::reasoning_effort_defaults_for_point`] verwendet wird (Teil D).
    ///
    /// # Description
    /// Schließt die Effort-Lücke für Kinder auf dem Hauptmodell: bis zu
    /// dieser Ergänzung lieferte [`Self::reasoning_effort_defaults_for_point`]
    /// im `resolved.is_main_model()`-Fall bedingungslos `(None, None)`, obwohl
    /// das Kind tatsächlich mit dem Hauptmodell dieser Fabrik läuft — dessen
    /// `default_reasoning_effort` wurde nie nachgeschlagen, weil
    /// [`harw_config::ResolvedInternalModel`] für den Hauptmodell-Fallback
    /// selbst kein `provider`/`model` trägt (siehe
    /// [`harw_config::resolve_internal_model`]).
    ///
    /// Die Montage (`assembly.rs`) ruft diese Methode für **beide**
    /// Fabrik-Arten mit der jeweils zutreffenden Auswahl auf:
    /// - Hauptfabrik (gewöhnliche Kinder): `provider =
    ///   config.harness.default_provider`, `model =
    ///   config.harness.default_model`.
    /// - UIA-Worker-Fabrik (`uia-worker`-Rollenfamilie): `provider =
    ///   config.harness.uia_provider` (mit demselben Fallback auf
    ///   `default_provider`, den die Montage bereits an anderer Stelle
    ///   durchsetzt), `model = uia_worker_model`, oder — falls `None` —
    ///   `uia_model`/`default_model`.
    ///
    /// Ohne Aufruf leitet [`Self::reasoning_effort_defaults_for_point`] die
    /// Auswahl bei jedem Nachschlagen selbst aus
    /// `config.harness.default_provider`/`default_model` ab — bit-identisch
    /// zu einer explizit mit genau diesen Werten aufgerufenen Fabrik.
    ///
    /// # Arguments
    /// - `provider` (`Option<String>`): die Provider-ID des effektiven
    ///   Hauptmodells dieser Fabrik, oder `None`, wenn keine gewählt ist.
    /// - `model` (`Option<String>`): die Modell-ID des effektiven
    ///   Hauptmodells dieser Fabrik, oder `None`.
    #[must_use]
    pub fn with_main_model_selection(
        mut self,
        provider: Option<String>,
        model: Option<String>,
    ) -> Self {
        self.main_model_selection = Some((provider, model));
        self
    }

    /// Setzt die Modellkennung, die der ungepinnte Kind-Provider dieser
    /// Fabrik tatsächlich anspricht (Teil C).
    ///
    /// # Description
    /// Für die Hauptfabrik das effektive Vorgabemodell des Wurzel-Baums
    /// (einschließlich des Rückfalls auf das erste nutzbare Katalogmodell),
    /// für die UIA-Worker-Fabrik das Modell der UIA-Sitzung. Der Spawner
    /// budgetiert damit das Kontextfenster eines Kindes ohne Rollen-Pin
    /// ([`ChildRegistryFactory::main_model_for_task`]) und setzt es als
    /// `active_model` des Kindes. Ohne Aufruf liefert die Fabrik keine
    /// Aussage.
    ///
    /// # Arguments
    /// - `model` (`Option<String>`): die Modellkennung oder `None`.
    #[must_use]
    pub fn with_effective_main_model(mut self, model: Option<String>) -> Self {
        self.effective_main_model = model;
        self
    }

    /// Ergänzt Sandbox-Profil und Host-Permit-Verdrahtung, mit denen jede
    /// Kind-Registry montiert wird (Teil B4).
    ///
    /// # Description
    /// Reiner Erbauer-Schritt, analog [`Self::with_internal_models`]:
    /// [`Self::new`]/[`Self::with_definitions`] bleiben unverändert. Ohne
    /// Aufruf bleiben [`Self::sandbox_profile`]
    /// [`harw_sandbox::SandboxProfile::Strict`] und
    /// [`Self::host_permit_wiring`] `None` — [`Self::build_registry`] montiert
    /// dann bit-identisch zum Verhalten vor dieser Ergänzung.
    ///
    /// Mit Aufruf reicht [`Self::build_registry`] beide Werte an
    /// [`assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits`]
    /// durch; nur ein mit [`harw_sandbox::SandboxProfile::Host`] gebauter
    /// `ShellToolProvider` hängt die Verdrahtung tatsächlich an (siehe deren
    /// Dokumentation) — `uia-shell-worker` und `host-process-worker` erreicht
    /// diese Ergänzung damit auf demselben Weg wie jedes andere Kind.
    ///
    /// # Arguments
    /// - `sandbox_profile` ([`harw_sandbox::SandboxProfile`]): das Profil, mit
    ///   dem jeder für ein Kind gebaute `ShellToolProvider` montiert wird.
    /// - `wiring` (`Option<`[`HostPermitWiring`]`>`): die einmal je Lauf
    ///   instanziierte Host-Permit-Verdrahtung des Elternteils
    ///   (`HostPermitWiring` ist bereits `#[derive(Clone)]` — Ledger und
    ///   Sitzungs-Registry sind `Arc`, der Fragekanal-Sender ist ein
    ///   `mpsc::UnboundedSender`-Klon; kein zusätzliches `Arc` um den
    ///   gesamten Typ nötig). `None` verhält sich wie ohne Aufruf dieser
    ///   Methode.
    #[must_use]
    pub fn with_host_permits(
        mut self,
        sandbox_profile: harw_sandbox::SandboxProfile,
        wiring: Option<HostPermitWiring>,
    ) -> Self {
        self.sandbox_profile = sandbox_profile;
        self.host_permit_wiring = wiring;
        self
    }

    /// Hinterlegt den Skill-Katalog, aus dem jedes Kind seine direkt
    /// konfigurierten Skills als Instruktionsfragmente bekommt (Welle 4).
    ///
    /// # Description
    /// Reiner Erbauer-Schritt, analog [`Self::with_internal_models`]. Friert
    /// `config` einmalig als [`CatalogSnapshot`] ein; eine spätere Änderung
    /// des Live-Katalogs wirkt erst in der nächsten Montage. Für eine Rolle,
    /// die in `agents/<rolle>/agent.toml` Skills führt, lädt
    /// [`ChildRegistryFactory::build_registry`] deren Anweisungen (über
    /// [`harw_config::load_skill_instructions`], symlink- und traversalfest)
    /// und hängt sie als Fragmente mit SHA-256-Provenienz an die Identität
    /// des Kindes; [`ChildRegistryFactory::capability_snapshot`] liefert
    /// denselben Satz als eingefrorenen Spawn-Vertrag.
    ///
    /// # Arguments
    /// - `config` (`&ResolvedConfig`): die bereits validierte Konfiguration
    ///   des Elternlaufs.
    /// - `skill_roots` (`Vec<PathBuf>`): die vertrauten Config-Layer in
    ///   aufsteigender Präzedenz (`ConfigTrustReport::layers`) — dieselbe
    ///   Liste, aus der die Discovery `config.skills` gelesen hat.
    ///
    /// # Errors
    /// [`RuntimeError::Registry`], wenn die Konfiguration nicht als Katalog
    /// einfrierbar ist (ungültige Verweise).
    pub fn with_skill_catalog(
        mut self,
        config: &ResolvedConfig,
        skill_roots: Vec<PathBuf>,
    ) -> RuntimeResult<Self> {
        let catalog =
            CatalogSnapshot::from_config(config).map_err(|error| RuntimeError::Registry {
                detail: format!("could not freeze the skill catalog for children: {error}"),
            })?;
        self.skill_catalog = Some(SkillCatalogWiring {
            catalog,
            roots: skill_roots,
        });
        Ok(self)
    }

    /// Hinterlegt den geteilten `StateStore` für die `delegate_wave`-Fläche
    /// der Orchestrator-Kinder.
    ///
    /// # Description
    /// Reiner Erbauer-Schritt, analog [`Self::with_internal_models`]. Jede
    /// Orchestrator-Registry (`composition_tools_for_role(role)` enthält
    /// `delegate_wave`) bekommt eine
    /// [`harw_core_bridge::DelegateWaveOperation`], deren `OpContext` den
    /// `ManagedAgentSpawner` (über den schwachen Spawner-Slot, erst beim
    /// Aufruf aufgelöst) und diesen `StateStore` trägt — genau die beiden
    /// Dienste, die `delegate_wave` verlangt.
    ///
    /// # Arguments
    /// - `state_store` (`Arc<dyn StateStore>`): derselbe Store wie der der
    ///   Wurzel (`RuntimeServicesParts::state_store`).
    #[must_use]
    pub fn with_delegate_wave_store(mut self, state_store: Arc<dyn StateStore>) -> Self {
        self.delegate_wave_store = Some(state_store);
        self
    }

    /// Hinterlegt den Wissensspeicher für die lesenden Wissenswerkzeuge der
    /// Kinder (Plan Teil D: „Agenten dürfen Workbench, Palace und Diary
    /// lesen“; Kanban nur der Root-Orchestrator).
    ///
    /// # Beschreibung
    /// Reiner Erbauer-Schritt. [`ChildRegistryFactory::build_registry`]
    /// montiert danach für eine Rolle aus
    /// [`harw_registry_defaults::profile::knowledge_tools_for_role`] genau die
    /// Provider, deren Werkzeuge die eingebaute Definition der Rolle
    /// admittiert (`[tools].admitted`) und deren Rechteklasse
    /// (`ReadWorkspace`) das Profil der Rolle trägt — nie pauschal. Die
    /// Werkzeuge sind rein lesend: `workbench.show` auf das Projekt des
    /// Elternteils gedeckelt, `diary.read` auf die Einträge des Kindes
    /// (Agent-Id = Rollenname), `palace.*` auf `established`-Knoten,
    /// `kanban.*` (nur Root-Orchestrator, jeder Aufruf fragt) ohne Übergang.
    ///
    /// # Arguments
    /// - `store` (`Arc<KnowledgeStore>`): derselbe Speicher wie
    ///   `RuntimeServices::knowledge_store` der Wurzel.
    /// - `kanban_ledger`: das Job-Ledger für den Kartenzustand (nur
    ///   `snapshot`); `None` → gebundene Karten zeigen `unknown`.
    #[must_use]
    pub fn with_knowledge(
        mut self,
        store: Arc<harw_knowledge::KnowledgeStore>,
        kanban_ledger: Option<Arc<dyn harw_knowledge::kanban::lifecycle::JobTransitions>>,
    ) -> Self {
        self.knowledge = Some(ChildKnowledgeWiring {
            store,
            kanban_ledger,
        });
        self
    }

    /// Wie [`Self::with_knowledge`], aber mit optionaler Quelle — `None`
    /// lässt die Fabrik unverändert (Montage ohne Profilverzeichnis).
    #[must_use]
    pub fn with_optional_knowledge(self, knowledge: Option<ChildKnowledgeSource>) -> Self {
        match knowledge {
            Some((store, kanban_ledger)) => self.with_knowledge(store, kanban_ledger),
            None => self,
        }
    }

    /// Hängt die lesenden Wissens-Provider an die Kind-Registry von `role`
    /// (siehe [`Self::with_knowledge`]).
    ///
    /// # Rückgabe
    /// Der Builder, ergänzt um jeden zugelassenen Provider; unverändert ohne
    /// Wissensspeicher, für eine Rolle ohne Wissenszugang, ohne eingebaute
    /// Definition oder ohne `ReadWorkspace` im Profil.
    fn with_knowledge_readers(
        &self,
        role: &str,
        profile: harw_registry_defaults::RegistryProfile,
        mut builder: harw_extension_api::ExtensionRegistryBuilder,
    ) -> harw_extension_api::ExtensionRegistryBuilder {
        let Some(knowledge) = &self.knowledge else {
            return builder;
        };
        let offered = harw_registry_defaults::profile::knowledge_tools_for_role(role);
        let Some(ir) = self.builtin_definitions.get(role) else {
            return builder;
        };
        let granted = profile.required_permissions();
        // Nur Werkzeuge, die die Rolle angeboten bekommt, ihre Definition
        // admittiert und deren Recht ihr Profil trägt.
        let admits = |names: &[&str], permissions: &[Option<harw_authority::Permission>]| {
            permissions
                .iter()
                .flatten()
                .all(|needed| granted.contains(*needed))
                && names.iter().any(|name| {
                    offered.contains(name)
                        && ir
                            .tool_surface()
                            .admitted()
                            .iter()
                            .any(|admitted| admitted == name)
                })
        };
        let store = &knowledge.store;
        if admits(
            harw_registry_defaults::WorkbenchReadToolProvider::TOOL_NAMES,
            harw_registry_defaults::WorkbenchReadToolProvider::TOOL_PERMISSIONS,
        ) {
            builder = builder.tool_provider(Arc::new(
                harw_registry_defaults::WorkbenchReadToolProvider::new(Arc::clone(store))
                    .with_project(harw_knowledge::workbench::WorkbenchScope::project_for_path(
                        &self.project.project_root,
                    )),
            ));
        }
        if admits(
            harw_registry_defaults::DiaryToolProvider::TOOL_NAMES,
            harw_registry_defaults::DiaryToolProvider::TOOL_PERMISSIONS,
        ) {
            builder =
                builder.tool_provider(Arc::new(harw_registry_defaults::DiaryToolProvider::new(
                    Arc::clone(store),
                    harw_knowledge::AgentId::new(role.to_owned()),
                )));
        }
        if admits(
            harw_registry_defaults::PalaceToolProvider::TOOL_NAMES,
            harw_registry_defaults::PalaceToolProvider::TOOL_PERMISSIONS,
        ) {
            builder = builder.tool_provider(Arc::new(
                harw_registry_defaults::PalaceToolProvider::new(Arc::clone(store)),
            ));
        }
        if admits(
            harw_registry_defaults::KanbanReadToolProvider::TOOL_NAMES,
            harw_registry_defaults::KanbanReadToolProvider::TOOL_PERMISSIONS,
        ) {
            builder = builder.tool_provider(Arc::new(
                harw_registry_defaults::KanbanReadToolProvider::new(Arc::clone(store))
                    .with_ledger(knowledge.kanban_ledger.clone()),
            ));
        }
        builder
    }

    /// Die interne Modellstelle eines Kind-Starts mit Aufgabenkomplexität
    /// (Addendum D+E): die Stelle der Rolle ([`Self::internal_point_for_child`]),
    /// sonst — nur für Worker-Rollen laut eingebauter IR —
    /// [`InternalModelPoint::WorkerSimple`] bzw.
    /// [`InternalModelPoint::WorkerComplex`]; sonst `None` (Eltern-Modell).
    fn internal_point_for_task(
        &self,
        role: &str,
        complexity: Option<harw_core::TaskComplexity>,
    ) -> Option<InternalModelPoint> {
        if let Some(point) = self.internal_point_for_child(role) {
            return Some(point);
        }
        let is_worker = self
            .builtin_definitions
            .get(role)
            .is_some_and(|ir| ir.role() == harw_agent_dsl::roles::AgentRoleId::Worker);
        if !is_worker {
            return None;
        }
        Some(match complexity {
            Some(harw_core::TaskComplexity::Simple) => InternalModelPoint::WorkerSimple,
            Some(harw_core::TaskComplexity::Complex) | None => InternalModelPoint::WorkerComplex,
        })
    }

    /// Die interne Modellstelle einer Kind-Rolle (Addendum C + R1).
    ///
    /// # Description
    /// Zuerst die namensbasierte Zuordnung [`internal_point_for_role`];
    /// sonst die Orchestrator-Stellen über die Organisationsrolle der
    /// eingebauten Definition
    /// ([`orchestrator_point_for_organizational_role`]). Eine Rolle ohne
    /// eingebaute Definition bekommt nie eine Orchestrator-Stelle.
    fn internal_point_for_child(&self, role: &str) -> Option<InternalModelPoint> {
        internal_point_for_role(role).or_else(|| {
            self.builtin_definitions
                .get(role)
                .map(ExecutableAgentIr::role)
                .and_then(orchestrator_point_for_organizational_role)
        })
    }

    /// Die `delegate_wave`-Fläche einer Orchestrator-Registry.
    ///
    /// # Description
    /// Baut einen [`ModelToolProvider`] mit genau einer
    /// [`harw_core_bridge::DelegateWaveOperation`]. Die Politik liefert die
    /// Reducer-Kennung je Zielrolle
    /// ([`harw_registry_defaults::authority_reducer_for_role`]) und die
    /// deklarierten Ziele je Aufruferrolle
    /// ([`harw_registry_defaults::authority::delegation_targets_for_role`]).
    /// Der `OpContext` entsteht je Aufruf aus dem
    /// [`harw_extension_api::ToolExecutionContext`] (Sitzung, Turn, Sandbox,
    /// `CancelToken` des Turn-Loops — dasselbe Muster wie
    /// `assembly.rs::install_operation_model_tools`); seine `ServiceMap`
    /// trägt `Arc<ManagedAgentSpawner>` (nur, solange der Spawner lebt) und
    /// `Arc<dyn StateStore>` (sofern hinterlegt). Fehlt einer der beiden
    /// Dienste, scheitert `delegate_wave` selbst fail-closed mit
    /// `OpError::NotAvailable`.
    fn delegate_wave_provider(&self) -> ModelToolProvider {
        let policy = harw_core_bridge::DelegateWavePolicy::new(
            |role: &str| {
                harw_registry_defaults::authority_reducer_for_role(role).map(|reducer| reducer.id())
            },
            harw_registry_defaults::authority::delegation_targets_for_role,
        );
        let operation: Arc<dyn Operation> =
            Arc::new(harw_core_bridge::DelegateWaveOperation::new(policy));
        let slot = Arc::clone(&self.spawner_slot);
        let state_store = self.delegate_wave_store.clone();
        ModelToolProvider::new([operation], move |execution_context| {
            let mut services = ServiceMap::new();
            if let Some(spawner) = slot.get().and_then(Weak::upgrade) {
                services.insert(spawner);
            }
            if let Some(store) = &state_store {
                services.insert(Arc::clone(store));
            }
            let ctx = OpContext::new(
                execution_context.session_id().clone(),
                execution_context.turn_id().clone(),
                execution_context.sandbox().clone(),
                services,
            );
            // Wie `install_operation_model_tools`: ohne den `CancelToken` des
            // Turn-Loops sähe `fanout_children` nie den echten Turn-Abbruch.
            match execution_context.cancel() {
                Some(cancel) => ctx.with_cancel_token(cancel.clone()),
                None => ctx,
            }
        })
    }

    /// Die Skill-Fragmente einer Kind-Rolle: die direkt im Katalog
    /// (`agents/<rolle>/agent.toml`) konfigurierten Skills, vereinigt mit den
    /// Skills der eingebauten Agent-Definition der Rolle
    /// ([`ExecutableAgentIr::skills`]).
    ///
    /// # Description
    /// Reihenfolge: Katalog-Skills zuerst, danach die Definitions-Skills, die
    /// der Katalog nicht schon nennt (Duplikate zählen einmal). Deaktivierte
    /// Skills werden wie im Katalogpfad übersprungen. Leer, wenn weder der
    /// Katalog noch die Definition Skills für die Rolle führen.
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn ein aktivierter Skill nicht geladen werden
    /// kann oder ein Definitions-Skill im Katalog unbekannt ist — fail-closed:
    /// ein Kind startet nie mit einem Teil seiner Skills. Ebenso, wenn die
    /// Definition der Rolle Skills verlangt, aber kein Skill-Katalog
    /// hinterlegt ist ([`Self::with_skill_catalog`]): ohne Katalog lässt sich
    /// weder „aktiviert" prüfen noch der Skill laden.
    fn skill_fragments_for_role(&self, role: &str) -> Result<Vec<String>, AgentSpawnError> {
        let definition_skills: &[String] = self
            .builtin_definitions
            .get(role)
            .map(ExecutableAgentIr::skills)
            .unwrap_or(&[]);
        let Some(wiring) = &self.skill_catalog else {
            if definition_skills.is_empty() {
                return Ok(Vec::new());
            }
            return Err(AgentSpawnError {
                message: format!(
                    "child role '{role}' requires the skills [{}] from its agent definition, \
                     but no skill catalog is wired: refusing to start it without them",
                    definition_skills.join(", ")
                ),
            });
        };
        let catalog_skills = wiring.catalog.direct_skills_of(role).unwrap_or(&[]);
        let names = merge_skill_names(catalog_skills, definition_skills);
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let snapshots =
            load_enabled_skills(|name| wiring.catalog.skill(name), &wiring.roots, &names).map_err(
                |detail| AgentSpawnError {
                    message: format!("could not load the skills of child role '{role}': {detail}"),
                },
            )?;
        for snapshot in &snapshots {
            tracing::debug!(
                role,
                skill = %snapshot.name,
                sha256 = %snapshot.sha256,
                "runtime.child_skill.injected"
            );
        }
        Ok(snapshots
            .iter()
            .map(SkillRuntimeSnapshot::instruction_fragment)
            .collect())
    }

    /// Liefert Provider-/Modell-Reasoning-Effort-Defaults für eine bereits
    /// aufgelöste interne Modellstelle.
    ///
    /// # Description
    /// `(None, None)`, wenn [`Self::with_reasoning_effort_config`] nie
    /// aufgerufen wurde oder `point` in [`Self::internal_models`] nicht
    /// aufgelöst ist. Trägt die aufgelöste Stelle das Hauptmodell
    /// ([`ResolvedInternalModel::is_main_model`] — sie selbst führt dann kein
    /// eigenes `provider`/`model`), bestimmt diese Methode Provider und
    /// Modell stattdessen aus [`Self::main_model_selection`] (Teil D, schließt
    /// die zuvor offene Effort-Lücke für Kinder auf dem Hauptmodell): mit über
    /// [`Self::with_main_model_selection`] gesetzter Auswahl genau diese
    /// Werte, sonst abgeleitet aus
    /// `config.harness.default_provider`/`default_model`. In jedem Fall
    /// (Hauptmodell-Fallback wie regulär aufgelöste Stelle): die
    /// `default_reasoning_effort`-Felder der in
    /// `config.providers`/`config.models` unter der so bestimmten ID
    /// hinterlegten Einträge, je `None` bei fehlendem Eintrag oder fehlender
    /// ID.
    fn reasoning_effort_defaults_for_point(
        &self,
        point: InternalModelPoint,
    ) -> (
        Option<harw_types::ReasoningEffort>,
        Option<harw_types::ReasoningEffort>,
    ) {
        let Some(config) = self.reasoning_effort_config.as_deref() else {
            return (None, None);
        };
        let Some(resolved) = self.internal_models.get(&point) else {
            return (None, None);
        };
        if resolved.is_main_model() {
            let (provider_id, model_id): (Option<&str>, Option<&str>) =
                match &self.main_model_selection {
                    Some((provider, model)) => (provider.as_deref(), model.as_deref()),
                    None => (
                        config.harness.default_provider.as_deref(),
                        config.harness.default_model.as_deref(),
                    ),
                };
            let provider_default = provider_id
                .and_then(|id| config.providers.get(id))
                .and_then(|provider| provider.default_reasoning_effort);
            let model_default = model_id
                .and_then(|id| config.models.get(id))
                .and_then(|model| model.default_reasoning_effort);
            return (provider_default, model_default);
        }
        let provider_default = resolved
            .provider
            .as_deref()
            .and_then(|id| config.providers.get(id))
            .and_then(|provider| provider.default_reasoning_effort);
        let model_default = resolved
            .model
            .as_deref()
            .and_then(|id| config.models.get(id))
            .and_then(|model| model.default_reasoning_effort);
        (provider_default, model_default)
    }

    /// Löst `point` gegen [`Self::internal_models`] auf und liefert entweder
    /// einen [`PinnedModelProvider`] oder — falls die Stelle unbekannt/nicht
    /// aufgelöst ist oder das Hauptmodell erzwingt — das Eltern-Modell.
    ///
    /// # Description
    /// Gemeinsame Hilfsfunktion für [`ChildRegistryFactory::model_for`] und
    /// [`ChildRegistryFactory::model_for_task`], damit beide dieselbe
    /// Pinning-Logik nutzen und nicht auseinanderlaufen.
    fn pinned_model_for_point(
        &self,
        role: &str,
        point: InternalModelPoint,
    ) -> Arc<dyn ModelProvider> {
        let Some(resolved) = self.internal_models.get(&point) else {
            return Arc::clone(&self.model);
        };
        if resolved.is_main_model() {
            return Arc::clone(&self.model);
        }
        let provider_id = resolved
            .provider
            .as_deref()
            .map(harw_types::ProviderId::from);
        let model_id = resolved.model.as_deref().map(harw_types::ModelId::from);
        tracing::debug!(
            role,
            point = point.key(),
            model = resolved.model.as_deref().unwrap_or(""),
            "children.internal_model"
        );
        Arc::new(PinnedModelProvider::new(
            Arc::clone(&self.model),
            provider_id,
            model_id,
        ))
    }
}

impl ChildRegistryFactory for RuntimeChildRegistryFactory {
    /// Baut die Registry eines Kindes nach dem Profil seiner Rolle.
    ///
    /// # Argumente
    /// - `role` (`&str`): der registrierte Rollenname.
    /// - `_input` (`&SpawnInput`): ungenutzt — die Registry hängt allein an
    ///   `role`, nie an Modell-JSON.
    /// - `_suggestions` (`Option<&harw_catalog::AgentSuggestions>`): ungenutzt;
    ///   ein Vorschlag wird hier nie zu einem registrierten Werkzeug.
    ///
    /// # Rückgabe
    /// `Ok(ExtensionRegistry)` mit genau den Tool-Providern des Profils aus
    /// [`profile_for_role`], erweitert um die Kind-Freigabekette.
    ///
    /// # Rechte und Netz
    /// Die Registry legt nur fest, welche Werkzeuge das Kind **sieht**; was
    /// es tatsächlich darf, entscheidet die Sandbox, mit der
    /// [`ManagedAgentSpawner`] das Kind admittiert — über den Handoff die
    /// Sandbox des Elternteils, geprüft mit `ensure_child_of`, also nie mehr
    /// Rechte und nie mehr Hosts als der Elternteil. Das gilt insbesondere
    /// für `uia-worker`/`uia-writer`, deren Profile seit der
    /// Nutzerentscheidung „kurz online recherchieren, manchmal
    /// Abhängigkeiten hinzufügen“ `web.fetch`/`web.search` und die lesenden
    /// `deps.*` registrieren: `NetworkAccess` samt Host-Scope kommt nur vom
    /// Elternteil (registry-seitiger Reducer `ReadExplore`), und jeder Abruf
    /// läuft zusätzlich durch die prozessweite Egress-Policy
    /// (`harw_registry_defaults::install_web_tools`).
    ///
    /// # Fehler
    /// [`AgentSpawnError`], wenn `role` **keine** bekannte Rolle ist
    /// (fail-closed, kein `unwrap_or_default`) oder die Montage scheitert.
    fn build_registry(
        &self,
        role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&harw_catalog::AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        let profile = profile_for_role(role).ok_or_else(|| AgentSpawnError {
            message: format!(
                "refusing to assemble a child registry for unknown role '{role}': \
                 no registry profile is declared for it"
            ),
        })?;
        let overrides = IdentityOverrides {
            agent_name: Some(role.to_owned()),
            // Addendum F+G: die organisatorische Rolle eines Kindes ist die
            // Rolle seiner eingebauten (bereits gesenkten) Agent-IR; eine
            // unbekannte Rolle scheitert oben bereits an `profile_for_role`,
            // eine bekannte Rolle ohne eingebaute Definition (repo-lokal)
            // bleibt bewusst `None` — kein Rollen-Regelwerk ohne Rolle.
            organizational_role: self
                .builtin_definitions
                .get(role)
                .map(ExecutableAgentIr::role),
            // Welle 4: die direkt konfigurierten Skills der Rolle als
            // Instruktionsfragmente (leer ohne Skill-Katalog).
            extra_context: self.skill_fragments_for_role(role)?,
            ..IdentityOverrides::default()
        };
        // Eine Kette je Kind: `for_child` legt eine Folgezelle an
        // ([`ApprovalModeCell::follower`], gedeckelt auf
        // `ApprovalMode::Delegated`). Eine Umstellung der Wurzel erreicht
        // laufende Kinder sofort; ein `set` im Kind koppelt nur dieses Kind ab
        // — Geschwister bleiben voneinander unabhängig, folgen aber weiter der
        // Wurzel.
        let child_chain = self.chain.for_child();
        // Teil B4: ruft dieselbe Delegationskette wie zuvor
        // `assemble_registry_for_project` mit exakt deren bisherigen
        // Default-Werten auf (`granted = profile.required_permissions()`,
        // `access = None`, `sandbox_profile = SandboxProfile::Strict`,
        // `host_permits = None` — siehe die Delegation
        // `assemble_registry_for_project` →
        // `assemble_registry_for_project_with_definition_access(.., None)` →
        // `assemble_registry_for_sandbox_with_definition_access(..,
        // &profile.required_permissions(), ..)` →
        // `..._and_sandbox_profile(.., &SandboxProfile::Strict)` →
        // `..._and_permits(.., None)` in
        // harw-registry-defaults/src/profile.rs), ergänzt um
        // `self.sandbox_profile`/`self.host_permit_wiring` — ohne
        // [`Self::with_host_permits`] bleiben das `SandboxProfile::Strict`/
        // `None` und das Verhalten ist bit-identisch zu vorher.
        let assembled =
            assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
                profile,
                &self.project,
                overrides,
                child_chain.mode().clone(),
                &profile.required_permissions(),
                None,
                &self.sandbox_profile,
                self.host_permit_wiring.clone(),
            )
            .map_err(|error| AgentSpawnError {
                message: format!("could not assemble child registry for role '{role}': {error}"),
            })?;
        // `install_over_default`, nicht `install`:
        // `assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits`
        // hat die `DefaultApprovalPolicy` über `child_chain.mode()` bereits
        // registriert (harw-registry-defaults/src/profile.rs, `approval_handler`
        // in der letzten Stufe der Delegationskette). Eine zweite wäre
        // wirkungsgleich, aber eine Dublette (Befund Z2c-06).
        let mut registry_builder = child_chain
            .install_over_default(assembled.registry)
            .spawner(Arc::new(DeferredManagedSpawner {
                slot: Arc::clone(&self.spawner_slot),
            }));
        #[cfg(feature = "browser")]
        if self.browser.grants_role(role) {
            let browser_provider =
                harw_registry_defaults::profile::browser_tool_provider_for_config(&self.browser)
                    .map_err(|error| AgentSpawnError {
                        message: format!(
                            "could not assemble browser provider for role '{role}': {error}"
                        ),
                    })?;
            if let Some(provider) = browser_provider {
                registry_builder = registry_builder.tool_provider(provider);
            }
        }
        // `delegate_wave` nur für Orchestrator-Rollen (Root oder Child); die
        // `SessionActivation` des Kindes schaltet das Werkzeug zusätzlich nur
        // frei, wenn seine Definition es admittiert.
        if role_gets_delegate_wave(role) {
            registry_builder =
                registry_builder.tool_provider(Arc::new(self.delegate_wave_provider()));
        }
        // Plan Teil D: lesende Wissenswerkzeuge nur für zugelassene Rollen,
        // deren Definition sie admittiert (siehe `with_knowledge_readers`).
        registry_builder = self.with_knowledge_readers(role, profile, registry_builder);
        let registry = registry_builder.build();
        tracing::debug!(
            role,
            profile = ?profile,
            project_root = %self.project.project_root.display(),
            "runtime.child_registry.assembled"
        );
        Ok(registry)
    }

    /// Friert die direkt konfigurierten Skills der Rolle als Spawn-Vertrag
    /// ein (Welle 4).
    ///
    /// # Beschreibung
    /// Nur aus vertrauenswürdigem Zustand — dem bei
    /// [`Self::with_skill_catalog`] eingefrorenen Katalog, nie aus
    /// Modell-JSON. `Ok(None)` ohne Katalog oder für eine Rolle ohne
    /// `agent.toml`; dann gilt unverändert der Kompatibilitäts-Default.
    ///
    /// # Fehler
    /// [`AgentSpawnError`], wenn der Katalog für die Rolle inkonsistent ist.
    fn capability_snapshot(
        &self,
        role: &str,
        _input: &SpawnInput,
    ) -> Result<Option<SpawnCapabilitySnapshot>, AgentSpawnError> {
        let Some(wiring) = &self.skill_catalog else {
            return Ok(None);
        };
        wiring
            .catalog
            .direct_skills_snapshot(role)
            .map_err(|error| AgentSpawnError {
                message: format!("could not freeze the capabilities of role '{role}': {error}"),
            })
    }

    /// Liefert den Modellanbieter für eine Kind-Rolle.
    ///
    /// # Beschreibung
    /// [`internal_point_for_role`] bzw. — für Orchestrator-Definitionen —
    /// [`orchestrator_point_for_organizational_role`] (R1) bildet `role` auf
    /// eine interne Modellstelle ab (Addendum C). Ist keine Stelle zuständig, keine
    /// aufgelöste Stelle bekannt (`self.internal_models` leer, z. B. weil
    /// [`Self::with_internal_models`] nie aufgerufen wurde) oder die
    /// aufgelöste Stelle das Hauptmodell ([`ResolvedInternalModel::is_main_model`]),
    /// liefert diese Funktion unverändert den Eltern-Modellanbieter — sonst
    /// einen [`PinnedModelProvider`], der jede Anfrage auf den aufgelösten
    /// Provider/Modell fest verdrahtet.
    ///
    /// # Fehler
    /// Nie: jede Auflösung fällt bei Unklarheit auf das Eltern-Modell
    /// zurück, statt zu scheitern.
    fn model_for(&self, role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        let Some(point) = self.internal_point_for_child(role) else {
            return Ok(Arc::clone(&self.model));
        };
        Ok(self.pinned_model_for_point(role, point))
    }

    /// Liefert den Modellanbieter für eine Kind-Rolle unter Berücksichtigung
    /// der Aufgabenkomplexität (Addendum D+E).
    ///
    /// # Beschreibung
    /// Eine Rolle mit eigener interner Modellstelle
    /// ([`internal_point_for_role`], z. B. `explorer`, die Recherche-Rollen,
    /// `memory-steward`) hat Vorrang — `complexity` wird dann ignoriert,
    /// unverändert wie [`Self::model_for`]. Sonst: ist die Rolle laut ihrer
    /// eingebauten [`ExecutableAgentIr`] eine Worker-Rolle
    /// (`role() == AgentRoleId::Worker`), wird
    /// [`InternalModelPoint::WorkerSimple`] bei
    /// `Some(TaskComplexity::Simple)` bzw.
    /// [`InternalModelPoint::WorkerComplex`] bei
    /// `Some(TaskComplexity::Complex)` oder `None` verwendet. Ist die Rolle
    /// weder eine bekannte interne Stelle noch (laut eingebauter IR) eine
    /// Worker-Rolle, bleibt es unverändert beim Eltern-Modellanbieter.
    ///
    /// # Fehler
    /// Nie: jede Auflösung fällt bei Unklarheit auf das Eltern-Modell zurück.
    fn model_for_task(
        &self,
        role: &str,
        complexity: Option<harw_core::TaskComplexity>,
    ) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        match self.internal_point_for_task(role, complexity) {
            Some(point) => Ok(self.pinned_model_for_point(role, point)),
            None => Ok(Arc::clone(&self.model)),
        }
    }

    /// Das Modell, das ein ungepinntes Kind dieser Rolle ruft (Teil C).
    ///
    /// # Beschreibung
    /// [`Self::with_effective_main_model`], solange die Rolle nicht auf eine
    /// aufgelöste, vom Hauptmodell abweichende interne Modellstelle fällt —
    /// dort bestimmt der Pin das Modell (oder, bei einer Stelle ohne eigenes
    /// Modell, der gepinnte Provider), und das Hauptmodell wäre die falsche
    /// Aussage.
    fn main_model_for_task(
        &self,
        role: &str,
        complexity: Option<harw_core::TaskComplexity>,
    ) -> Option<String> {
        let pinned_point = self
            .internal_point_for_task(role, complexity)
            .and_then(|point| self.internal_models.get(&point))
            .is_some_and(|resolved| !resolved.is_main_model());
        if pinned_point {
            return None;
        }
        self.effective_main_model.clone()
    }

    /// Die eingebaute Agent-IR einer Rolle.
    ///
    /// # Rückgabe
    /// `Some(&ExecutableAgentIr)` für eine eingebaute Rolle, sonst `None`.
    /// Der Kern zieht daraus Tool-Aktivierung, Budget und Pause-Sperre des
    /// Kindes; die Aktivierung wird danach zusätzlich mit der der Wurzel
    /// geschnitten (`harw-core/src/child_controller.rs`, W2A-02).
    fn executable_agent_ir(&self, role: &str) -> Option<&ExecutableAgentIr> {
        self.builtin_definitions.get(role)
    }

    /// Liefert Provider-/Modell-Reasoning-Effort-Defaults für eine Kind-Rolle
    /// ohne Aufgabenkomplexität (Welle 8).
    ///
    /// # Beschreibung
    /// Wie [`Self::model_for`]: hat `role` eine eigene interne Modellstelle
    /// ([`internal_point_for_role`]), gelten deren Defaults
    /// ([`Self::reasoning_effort_defaults_for_point`]). Sonst — auch für eine
    /// Worker-Rolle ohne bekannte Komplexität — `(None, None)`, weil ohne
    /// Komplexität nicht zwischen `WorkerSimple`/`WorkerComplex`
    /// unterschieden werden kann; [`Self::reasoning_effort_defaults_for_role_task`]
    /// ist die vollständige Fassung.
    fn reasoning_effort_defaults_for_role(
        &self,
        role: &str,
    ) -> (
        Option<harw_types::ReasoningEffort>,
        Option<harw_types::ReasoningEffort>,
    ) {
        let Some(point) = self.internal_point_for_child(role) else {
            return (None, None);
        };
        self.reasoning_effort_defaults_for_point(point)
    }

    /// Wie [`Self::reasoning_effort_defaults_for_role`], zusätzlich mit der
    /// Aufgabenkomplexität (Addendum D+E) — spiegelt exakt die Auswahllogik
    /// von [`Self::model_for_task`]: eine bekannte interne Modellstelle hat
    /// Vorrang; sonst wird bei einer laut eingebauter [`ExecutableAgentIr`]
    /// registrierten Worker-Rolle [`InternalModelPoint::WorkerSimple`] bzw.
    /// [`InternalModelPoint::WorkerComplex`] verwendet; jede andere Rolle
    /// (auch eine, die auf das Eltern-Hauptmodell zurückfällt) liefert
    /// `(None, None)` — die Provider-/Modell-ID des aktiven Hauptmodells ist
    /// dieser Fabrik nicht bekannt (siehe Feld-Doku
    /// [`Self::reasoning_effort_config`]); für die UIA-Wurzelsitzung selbst
    /// löst `crate::assembly` diese Ebene direkt gegen die Config auf, nicht
    /// über diese Fabrik.
    fn reasoning_effort_defaults_for_role_task(
        &self,
        role: &str,
        complexity: Option<harw_core::TaskComplexity>,
    ) -> (
        Option<harw_types::ReasoningEffort>,
        Option<harw_types::ReasoningEffort>,
    ) {
        if let Some(point) = self.internal_point_for_child(role) {
            return self.reasoning_effort_defaults_for_point(point);
        }
        let is_worker = self
            .builtin_definitions
            .get(role)
            .is_some_and(|ir| ir.role() == harw_agent_dsl::roles::AgentRoleId::Worker);
        if !is_worker {
            return (None, None);
        }
        let point = match complexity {
            Some(harw_core::TaskComplexity::Simple) => InternalModelPoint::WorkerSimple,
            Some(harw_core::TaskComplexity::Complex) | None => InternalModelPoint::WorkerComplex,
        };
        self.reasoning_effort_defaults_for_point(point)
    }

    /// Baut die Registry eines Kindes unter Kenntnis der effektiven Rechte
    /// seines unmittelbaren Elternteils (Welle FANIN-K/FANIN-RT, Nachtrag
    /// K3 "Schärfung der Steward-Prüfung").
    ///
    /// # Description
    /// Für jede Rolle außer [`role_names::AGENT_STEWARD`] unverändert wie
    /// bisher — delegiert an [`Self::build_registry`] (`parent` bleibt
    /// ungenutzt, `harw-core`s Default-Methode täte hier nichts anderes; die
    /// explizite Delegation vermeidet nur eine zusätzliche Indirektion über
    /// den `capability_snapshot`-Pfad).
    ///
    /// Für `agent-steward` baut diese Fabrik die Registry stattdessen über
    /// [`assemble_registry_for_project_with_definition_access`] mit einer
    /// gesetzten [`AgentDefinitionAccess`]:
    /// - `project_agents_dir` = `<projekt>/.harw/agents`.
    /// - `profile_agents_dir` = [`Self::profile_agents_dir`] (vom
    ///   Montage-Aufrufer gesetzt; `None`, wenn kein Profil ermittelbar war).
    /// - `mode` = [`definition_write_mode_for_parent_role`]`(parent.role)`.
    /// - `ceiling` = die Urheber-Decke des Elternteils
    ///   ([`harw_registry_defaults::agent_definition_tools::DefinitionAuthorCeiling`]),
    ///   aus den effektiven Rechten aus `parent` gebaut — **nur**, wenn
    ///   `parent.role` bekannt ist (`Some`); ein unbekannter Eltern-Rollen-Wert
    ///   (`None`) lässt die Decke `None` und damit den Provider fail-closed
    ///   auf `agents.validate`/`agents.list_proposals` beschränkt, genau wie
    ///   Nachtrag K3 es fordert ("Fehlt sie → fail-closed").
    ///
    /// # Arguments
    /// - `role` (`&str`): der registrierte Rollenname.
    /// - `input` (`&SpawnInput`): unverändert an [`Self::build_registry`]
    ///   durchgereicht für jede Rolle außer `agent-steward`.
    /// - `suggestions` (`Option<&harw_catalog::AgentSuggestions>`): wie
    ///   `build_registry`, nie ein registriertes Werkzeug.
    /// - `parent` (`&harw_core::ParentGrant`): die effektiven Rechte des
    ///   unmittelbaren Elternteils dieses Kindes.
    ///
    /// # Returns
    /// `Ok(ExtensionRegistry)` wie [`Self::build_registry`].
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn `role` keine bekannte Rolle ist oder die
    /// Montage scheitert (dieselben Fälle wie [`Self::build_registry`]).
    fn build_registry_with_capabilities_for_parent(
        &self,
        role: &str,
        input: &SpawnInput,
        suggestions: Option<&harw_catalog::AgentSuggestions>,
        parent: &harw_core::ParentGrant,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        if role != role_names::AGENT_STEWARD {
            return self.build_registry(role, input, suggestions);
        }

        let profile = profile_for_role(role).ok_or_else(|| AgentSpawnError {
            message: format!(
                "refusing to assemble a child registry for unknown role '{role}': \
                 no registry profile is declared for it"
            ),
        })?;
        let overrides = IdentityOverrides {
            agent_name: Some(role.to_owned()),
            organizational_role: self
                .builtin_definitions
                .get(role)
                .map(ExecutableAgentIr::role),
            extra_context: self.skill_fragments_for_role(role)?,
            ..IdentityOverrides::default()
        };
        let child_chain = self.chain.for_child();

        // Nachtrag K3: ohne bekannte Eltern-Rolle bleibt die Decke `None`
        // (fail-closed) — niemand verleiht Rechte, deren Urheber sich nicht
        // einmal einer Rolle zuordnen lässt.
        let ceiling = parent.role.map(|parent_role| {
            harw_registry_defaults::agent_definition_tools::DefinitionAuthorCeiling {
                role: parent_role,
                tools: parent.tools.clone(),
                permissions: parent.permissions.clone(),
                max_depth: parent.max_depth,
                budget_tokens: parent.budget_tokens,
                effort_cap: parent.reasoning_effort.clone(),
            }
        });
        let access = harw_registry_defaults::profile::AgentDefinitionAccess {
            project_agents_dir: Some(self.project.project_root.join(".harw").join("agents")),
            profile_agents_dir: self.profile_agents_dir.clone(),
            mode: definition_write_mode_for_parent_role(parent.role),
            ceiling,
        };

        let assembled =
            harw_registry_defaults::profile::assemble_registry_for_project_with_definition_access(
                profile,
                &self.project,
                overrides,
                child_chain.mode().clone(),
                Some(access),
            )
            .map_err(|error| AgentSpawnError {
                message: format!("could not assemble child registry for role '{role}': {error}"),
            })?;
        // `install_over_default`: siehe Begründung in `build_registry`.
        let registry = child_chain
            .install_over_default(assembled.registry)
            .spawner(Arc::new(DeferredManagedSpawner {
                slot: Arc::clone(&self.spawner_slot),
            }))
            .build();
        tracing::debug!(
            role,
            profile = ?profile,
            project_root = %self.project.project_root.display(),
            parent_role = ?parent.role,
            "runtime.child_registry.agent_steward_assembled_for_parent"
        );
        Ok(registry)
    }
}

/// Vereinigt Katalog- und Definitions-Skills einer Rolle: Katalog zuerst,
/// dann jeder Definitions-Skill, den der Katalog nicht schon nennt; Duplikate
/// innerhalb einer Quelle zählen ebenfalls einmal.
fn merge_skill_names(catalog: &[String], definition: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(catalog.len() + definition.len());
    for name in catalog.iter().chain(definition) {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

/// Lädt die aktivierten Skills aus `names` als eingefrorene Snapshots.
///
/// # Beschreibung
/// Doppelte Namen zählen einmal, deaktivierte Skills werden übersprungen.
/// Ein Name, den der Katalog nicht kennt oder dessen Verzeichnis sich in den
/// Wurzeln nicht findet, ist ein Fehler (fail-closed).
fn load_enabled_skills<'a>(
    lookup: impl Fn(&str) -> Option<&'a SkillToml>,
    roots: &[PathBuf],
    names: &[String],
) -> Result<Vec<SkillRuntimeSnapshot>, String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut snapshots = Vec::new();
    for name in names {
        if !seen.insert(name.as_str()) {
            continue;
        }
        let skill = lookup(name).ok_or_else(|| format!("skill '{name}' is not configured"))?;
        if !skill.enabled {
            continue;
        }
        let directory = resolve_skill_directory(roots, name)
            .map_err(|error| format!("skill '{name}': {error}"))?
            .ok_or_else(|| {
                format!("skill '{name}' has no skills/*/skill.toml in any trusted config layer")
            })?;
        let snapshot = load_skill_runtime_snapshot(&directory, skill)
            .map_err(|error| format!("skill '{name}': {error}"))?;
        snapshots.push(snapshot);
    }
    Ok(snapshots)
}

/// Rendert die aktivierten Skills `skill_names` als Instruktionsfragmente
/// mit SHA-256-Provenienz (Welle 4).
///
/// # Beschreibung
/// Gemeinsamer Weg für Wurzel und Kinder: dieselbe Auflösung
/// ([`harw_catalog::resolve_skill_directory`]) und dieselbe Ladefunktion
/// ([`harw_catalog::load_skill_runtime_snapshot`] über
/// [`harw_config::load_skill_instructions`]) wie in
/// [`RuntimeChildRegistryFactory`]. Das Ergebnis gehört in
/// `IdentityOverrides::extra_context`.
///
/// # Arguments
/// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration.
/// - `skill_roots` (`&[PathBuf]`): die vertrauten Config-Layer in
///   aufsteigender Präzedenz (`ConfigTrustReport::layers`).
/// - `skill_names` (`&[String]`): die zu ladenden Skills.
///
/// # Errors
/// [`RuntimeError::Registry`], wenn ein aktivierter Skill nicht geladen
/// werden kann.
pub fn skill_instruction_fragments(
    config: &ResolvedConfig,
    skill_roots: &[PathBuf],
    skill_names: &[String],
) -> RuntimeResult<Vec<String>> {
    let snapshots = load_enabled_skills(|name| config.skills.get(name), skill_roots, skill_names)
        .map_err(|detail| RuntimeError::Registry {
        detail: format!("could not load skills: {detail}"),
    })?;
    Ok(snapshots
        .iter()
        .map(SkillRuntimeSnapshot::instruction_fragment)
        .collect())
}

/// Die Skill-Fragmente eines benannten Agenten aus `agents/<agent>/agent.toml`
/// (Feld `skills`); leer, wenn die Konfiguration keinen solchen Agenten kennt.
///
/// # Errors
/// Wie [`skill_instruction_fragments`].
pub fn agent_skill_fragments(
    config: &ResolvedConfig,
    skill_roots: &[PathBuf],
    agent: &str,
) -> RuntimeResult<Vec<String>> {
    match config.agents.get(agent) {
        Some(agent) => skill_instruction_fragments(config, skill_roots, &agent.skills),
        None => Ok(Vec::new()),
    }
}

/// Die Fragmente **aller** aktivierten Skills der Konfiguration, nach Namen
/// sortiert — für eine Wurzel (etwa die UIA), die ohne eigenes `agent.toml`
/// den gesamten aktivierten Katalog sehen soll.
///
/// # Errors
/// Wie [`skill_instruction_fragments`].
pub fn enabled_skill_fragments(
    config: &ResolvedConfig,
    skill_roots: &[PathBuf],
) -> RuntimeResult<Vec<String>> {
    let mut names: Vec<String> = config
        .skills
        .values()
        .filter(|skill| skill.enabled)
        .map(|skill| skill.name.clone())
        .collect();
    names.sort();
    skill_instruction_fragments(config, skill_roots, &names)
}

/// Spawner-Adapter für Kind-Registries.
///
/// Die Runtime baut die Registry eines Kindes vor dem `ManagedAgentSpawner`,
/// der diese Registry-Fabrik besitzt. Ein `OnceLock<Weak<_>>` schließt diesen
/// Zyklus ohne starke Referenzschleife und ohne eine unautorisierte Fallback-
/// Implementierung. Ist der Spawner noch nicht verbunden, schlägt der Aufruf
/// explizit fehl.
struct DeferredManagedSpawner {
    slot: Arc<OnceLock<Weak<ManagedAgentSpawner>>>,
}

impl AgentSpawner for DeferredManagedSpawner {
    fn spawn_child<'a>(
        &'a self,
        role: &'a str,
        input: SpawnInput,
        sandbox: harw_authority::SandboxSpec,
        suggestions: Option<harw_catalog::AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        Box::pin(async move {
            let spawner =
                self.slot
                    .get()
                    .and_then(Weak::upgrade)
                    .ok_or_else(|| AgentSpawnError {
                        message: "managed child spawner is not available".to_owned(),
                    })?;
            spawner.spawn_child(role, input, sandbox, suggestions).await
        })
    }

    fn child_finished(&self, child: &harw_types::SessionId) {
        if let Some(spawner) = self.slot.get().and_then(Weak::upgrade) {
            spawner.child_finished(child);
        }
    }

    fn child_completed(
        &self,
        child: &harw_types::SessionId,
        completed_at: jiff::Timestamp,
    ) -> Result<(), AgentSpawnError> {
        let spawner = self
            .slot
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| AgentSpawnError {
                message: "managed child spawner is not available".to_owned(),
            })?;
        spawner.child_completed(child, completed_at)
    }

    fn delegation_target_names(&self, parent_session_id: &harw_types::SessionId) -> Vec<String> {
        self.slot
            .get()
            .and_then(Weak::upgrade)
            .map_or_else(Vec::new, |spawner| {
                spawner.delegation_target_names(parent_session_id)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn internal_point_for_role_maps_explorer() {
        assert_eq!(
            internal_point_for_role(role_names::EXPLORER),
            Some(InternalModelPoint::Explorer)
        );
    }

    #[test]
    fn internal_point_for_role_maps_research_finding_roles() {
        for role in [
            role_names::RESEARCHER_WEB,
            role_names::RESEARCHER_DEPS,
            role_names::ANALYST,
        ] {
            assert_eq!(
                internal_point_for_role(role),
                Some(InternalModelPoint::Research),
                "role {role} should map to InternalModelPoint::Research"
            );
        }
    }

    #[test]
    fn internal_point_for_role_maps_memory_steward() {
        assert_eq!(
            internal_point_for_role(role_names::MEMORY_STEWARD),
            Some(InternalModelPoint::MemoryConsolidation)
        );
    }

    #[test]
    fn internal_point_for_role_leaves_the_entire_uia_worker_family_unmapped() {
        for role in [
            role_names::UIA_WORKER,
            role_names::UIA_EXPLORER,
            role_names::UIA_WRITER,
            role_names::UIA_SHELL_WORKER,
            role_names::UIA_LATEX_WRITER,
        ] {
            assert_eq!(
                internal_point_for_role(role),
                None,
                "role {role} must stay unmapped — it gets its own uia-derived model \
                 via `build_spawner`'s `uia_worker_factory`, not an internal model point"
            );
        }
    }

    #[test]
    fn internal_point_for_role_maps_agent_steward_to_worker_complex() {
        assert_eq!(
            internal_point_for_role(role_names::AGENT_STEWARD),
            Some(InternalModelPoint::WorkerComplex)
        );
    }

    #[test]
    fn definition_write_mode_for_parent_role_commits_only_for_the_uia() {
        use harw_agent_dsl::roles::AgentRoleId;
        use harw_registry_defaults::agent_definition_tools::DefinitionWriteMode;

        assert_eq!(
            definition_write_mode_for_parent_role(Some(AgentRoleId::UserInterface)),
            DefinitionWriteMode::Commit
        );
        for parent in [
            None,
            Some(AgentRoleId::RootOrchestrator),
            Some(AgentRoleId::ChildOrchestrator),
            Some(AgentRoleId::Worker),
            Some(AgentRoleId::UiaWorker),
            Some(AgentRoleId::AgentSteward),
        ] {
            assert_eq!(
                definition_write_mode_for_parent_role(parent),
                DefinitionWriteMode::ProposalOnly,
                "{parent:?} muss fail-closed im Vorschlagsmodus bleiben"
            );
        }
    }

    #[test]
    fn internal_point_for_role_leaves_other_roles_unmapped() {
        for role in [
            role_names::PLANNER,
            role_names::EXECUTOR,
            role_names::SECURITY_EGRESS_TRIAGE,
            "some-repo-local-role",
        ] {
            assert_eq!(
                internal_point_for_role(role),
                None,
                "role {role} should stay on the parent model"
            );
        }
    }

    /// Die gesenkten eingebauten Rollen einer leeren Konfiguration — dieselbe
    /// Quelle, die `assembly.rs::build_spawner` für `definitions` benutzt.
    fn builtin_definitions() -> TestResult<HashMap<String, ExecutableAgentIr>> {
        builtin_agent_definitions(&HashMap::new()).map_err(ctx("eingebaute Rollen senken"))
    }

    #[test]
    fn max_concurrent_instances_for_role_caps_the_entire_uia_worker_family_at_one() -> TestResult {
        let definitions = builtin_definitions()?;
        for role in [
            role_names::UIA_WORKER,
            role_names::UIA_EXPLORER,
            role_names::UIA_WRITER,
            role_names::UIA_SHELL_WORKER,
            role_names::UIA_LATEX_WRITER,
        ] {
            assert_eq!(
                max_concurrent_instances_for_role(role, &definitions),
                1,
                "role {role} (AgentRoleId::UiaWorker) must never fan out beyond one instance"
            );
        }
        Ok(())
    }

    #[test]
    fn max_concurrent_instances_for_role_leaves_other_roles_unbounded() -> TestResult {
        let definitions = builtin_definitions()?;
        for role in [
            role_names::EXPLORER,
            role_names::PLANNER,
            role_names::EXECUTOR,
            role_names::AGENT_STEWARD,
            "some-repo-local-role",
        ] {
            assert_eq!(
                max_concurrent_instances_for_role(role, &definitions),
                usize::MAX,
                "role {role} must not be capped by the uia-worker singleton rule"
            );
        }
        Ok(())
    }

    // -------------------------------------------------------------------
    // reasoning_effort_defaults_for_role(_task) — Welle 8, Provider-/
    // Modell-Ebene der Rangfolge Provider > Modell > Agent > Rolle.
    // -------------------------------------------------------------------

    /// Ein Projektkontext ohne echte Diskovery — nur die Felder, die
    /// [`RuntimeChildRegistryFactory`] tatsächlich liest.
    fn test_project() -> ProjectContext {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        ProjectContext {
            cwd: root.clone(),
            project_root: root,
            docs: Vec::new(),
        }
    }

    fn test_chain(config: &harw_config::ResolvedConfig) -> crate::approval::ApprovalChain {
        crate::approval::ApprovalChain::for_root(
            config,
            crate::spec::AskResolution::Interactive,
            harw_extension_api::approval_mode::ApprovalModeCell::default(),
            None,
            harw_extension_api::allow_rules::AllowRuleSet::new(),
        )
    }

    fn test_provider_toml(
        default_reasoning_effort: Option<&str>,
    ) -> TestResult<harw_config::ProviderToml> {
        let default_reasoning_effort = default_reasoning_effort
            .map(|label| label.parse())
            .transpose()
            .map_err(ctx("test fixture uses a valid ReasoningEffort label"))?;
        Ok(harw_config::ProviderToml {
            stream: None,
            name: "acme".to_owned(),
            api: "openai-chat".to_owned(),
            base_url: "https://example.invalid/v1".to_owned(),
            auth: None,
            auth_header: None,
            api_key: None,
            headers: HashMap::new(),
            models: Vec::new(),
            enabled: true,
            origin_allowlist: Default::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
            default_reasoning_effort,
            gateway_identity_headers: false,
        })
    }

    fn test_model_toml(
        default_reasoning_effort: Option<&str>,
    ) -> TestResult<harw_config::ModelToml> {
        let default_reasoning_effort = default_reasoning_effort
            .map(|label| label.parse())
            .transpose()
            .map_err(ctx("test fixture uses a valid ReasoningEffort label"))?;
        Ok(harw_config::ModelToml {
            stream: None,
            rate_limit: None,
            id: "acme-model".to_owned(),
            name: None,
            provider: "acme".to_owned(),
            aliases: Vec::new(),
            context_window: None,
            max_tokens: None,
            prompt_caching: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: Default::default(),
            default_reasoning_effort,
        })
    }

    /// Baut eine Fabrik mit genau einer aufgelösten internen Modellstelle
    /// (`WorkerComplex`) und der zugehörigen Provider-/Modell-Config, in der
    /// `default_reasoning_effort` gesetzt ist.
    fn factory_with_worker_complex_effort_defaults(
        provider_effort: Option<&str>,
        model_effort: Option<&str>,
    ) -> TestResult<RuntimeChildRegistryFactory> {
        let mut config = harw_config::ResolvedConfig::default();
        config
            .providers
            .insert("acme".to_owned(), test_provider_toml(provider_effort)?);
        config
            .models
            .insert("acme-model".to_owned(), test_model_toml(model_effort)?);

        let chain = test_chain(&config);
        let mut internal_models = HashMap::new();
        internal_models.insert(
            InternalModelPoint::WorkerComplex,
            harw_config::ResolvedInternalModel {
                point: InternalModelPoint::WorkerComplex,
                provider: Some("acme".to_owned()),
                model: Some("acme-model".to_owned()),
                source: harw_config::InternalModelSource::Explicit,
            },
        );

        Ok(RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            chain,
        )
        .map_err(ctx("factory builds"))?
        .with_internal_models(internal_models)
        .with_reasoning_effort_config(Arc::new(config)))
    }

    #[test]
    fn reasoning_effort_defaults_for_role_task_uses_worker_complex_point_for_agent_steward()
    -> TestResult {
        // `agent-steward` maps directly to `InternalModelPoint::WorkerComplex`
        // (see `internal_point_for_role_maps_agent_steward_to_worker_complex`),
        // so it exercises the point-lookup path without needing to guess a
        // `TaskComplexity`.
        let factory = factory_with_worker_complex_effort_defaults(Some("high"), Some("low"))?;
        let (provider_default, model_default) =
            factory.reasoning_effort_defaults_for_role_task(role_names::AGENT_STEWARD, None);
        assert_eq!(provider_default, Some(harw_types::ReasoningEffort::High));
        assert_eq!(model_default, Some(harw_types::ReasoningEffort::Low));
        Ok(())
    }

    #[test]
    fn reasoning_effort_defaults_for_role_task_selects_worker_complex_for_complex_worker()
    -> TestResult {
        let factory = factory_with_worker_complex_effort_defaults(Some("xhigh"), None)?;
        let (provider_default, model_default) = factory.reasoning_effort_defaults_for_role_task(
            role_names::EXECUTOR,
            Some(harw_core::TaskComplexity::Complex),
        );
        assert_eq!(provider_default, Some(harw_types::ReasoningEffort::Xhigh));
        assert_eq!(model_default, None);
        Ok(())
    }

    #[test]
    fn reasoning_effort_defaults_for_role_task_yields_none_for_worker_simple_when_only_complex_is_configured()
    -> TestResult {
        // Only `WorkerComplex` was given a resolved model in the fixture —
        // `WorkerSimple` stays unresolved, so a simple-complexity worker must
        // fall through to (None, None), not accidentally inherit the
        // complex-tier defaults.
        let factory = factory_with_worker_complex_effort_defaults(Some("xhigh"), Some("xhigh"))?;
        let (provider_default, model_default) = factory.reasoning_effort_defaults_for_role_task(
            role_names::EXECUTOR,
            Some(harw_core::TaskComplexity::Simple),
        );
        assert_eq!(provider_default, None);
        assert_eq!(model_default, None);
        Ok(())
    }

    #[test]
    fn reasoning_effort_defaults_for_role_task_yields_none_without_reasoning_effort_config()
    -> TestResult {
        let chain = test_chain(&harw_config::ResolvedConfig::default());
        let mut internal_models = HashMap::new();
        internal_models.insert(
            InternalModelPoint::WorkerComplex,
            harw_config::ResolvedInternalModel {
                point: InternalModelPoint::WorkerComplex,
                provider: Some("acme".to_owned()),
                model: Some("acme-model".to_owned()),
                source: harw_config::InternalModelSource::Explicit,
            },
        );
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            chain,
        )
        .map_err(ctx("factory builds"))?
        .with_internal_models(internal_models);
        // `with_reasoning_effort_config` was never called.
        let (provider_default, model_default) =
            factory.reasoning_effort_defaults_for_role_task(role_names::AGENT_STEWARD, None);
        assert_eq!(provider_default, None);
        assert_eq!(model_default, None);
        Ok(())
    }

    #[test]
    fn main_model_for_task_names_the_main_model_only_for_unpinned_roles() -> TestResult {
        let chain = test_chain(&harw_config::ResolvedConfig::default());
        let mut internal_models = HashMap::new();
        internal_models.insert(
            InternalModelPoint::WorkerComplex,
            harw_config::ResolvedInternalModel {
                point: InternalModelPoint::WorkerComplex,
                provider: Some("acme".to_owned()),
                model: Some("acme-model".to_owned()),
                source: harw_config::InternalModelSource::Explicit,
            },
        );
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            chain,
        )
        .map_err(ctx("factory builds"))?
        .with_internal_models(internal_models)
        .with_effective_main_model(Some("main-model".to_owned()));
        // Teil C: ohne Pin ruft das Kind das Hauptmodell der Fabrik.
        assert_eq!(
            factory.main_model_for_task(role_names::ROOT_ORCHESTRATOR, None),
            Some("main-model".to_owned())
        );
        assert_eq!(
            factory.main_model_for_task("some-repo-local-role", None),
            Some("main-model".to_owned())
        );
        // Eine aufgelöste, abweichende Stelle pinnt — dort gilt der Pin.
        assert_eq!(
            factory.main_model_for_task(role_names::AGENT_STEWARD, None),
            None
        );
        assert_eq!(
            factory
                .pinned_model_for_task(role_names::AGENT_STEWARD, None)
                .as_deref(),
            Some("acme-model")
        );
        Ok(())
    }

    #[test]
    fn reasoning_effort_defaults_for_role_task_yields_none_for_main_model_fallback() -> TestResult {
        // A role with no internal-model-point override that is not a builtin
        // `worker` role falls back to the parent's main model — whose
        // provider/model id this factory does not track (see field doc on
        // `reasoning_effort_config`). `planner` is deliberately *not* used
        // here: its builtin definition has `role = "worker"`, so it takes the
        // `WorkerSimple`/`WorkerComplex` path exactly like `model_for_task`.
        let factory = factory_with_worker_complex_effort_defaults(Some("high"), Some("high"))?;
        for role in [
            // Builtin definition, but not a worker role; its own
            // `RootOrchestrator` point (R1) is unresolved in this fixture.
            role_names::ROOT_ORCHESTRATOR,
            // Repo-local role without a builtin definition.
            "some-repo-local-role",
        ] {
            let (provider_default, model_default) =
                factory.reasoning_effort_defaults_for_role_task(role, None);
            assert_eq!(provider_default, None, "role {role}");
            assert_eq!(model_default, None, "role {role}");
        }
        Ok(())
    }

    // -------------------------------------------------------------------
    // Teil D — Effort-Lücke: `reasoning_effort_defaults_for_point` im
    // `resolved.is_main_model()`-Fall.
    // -------------------------------------------------------------------

    /// Baut eine Fabrik mit `InternalModelPoint::Explorer` als
    /// Hauptmodell-Fallback (`source = MainModel`, `provider`/`model` =
    /// `None`, wie [`harw_config::resolve_internal_model`] ihn tatsächlich
    /// liefert) über der übergebenen Config.
    fn factory_with_explorer_main_model_fallback(
        config: harw_config::ResolvedConfig,
    ) -> TestResult<RuntimeChildRegistryFactory> {
        let chain = test_chain(&config);
        let mut internal_models = HashMap::new();
        internal_models.insert(
            InternalModelPoint::Explorer,
            harw_config::ResolvedInternalModel {
                point: InternalModelPoint::Explorer,
                provider: None,
                model: None,
                source: harw_config::InternalModelSource::MainModel,
            },
        );
        Ok(RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            chain,
        )
        .map_err(ctx("factory builds"))?
        .with_internal_models(internal_models)
        .with_reasoning_effort_config(Arc::new(config)))
    }

    #[test]
    fn reasoning_effort_defaults_for_role_uses_provider_default_for_main_model_fallback_without_selection()
    -> TestResult {
        // Ohne `with_main_model_selection` leitet der Hauptmodell-Fallback
        // Provider/Modell aus `config.harness.default_provider`/
        // `default_model` ab — derselbe Default, den die Hauptfabrik trägt.
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("acme".to_owned());
        config
            .providers
            .insert("acme".to_owned(), test_provider_toml(Some("high"))?);
        let factory = factory_with_explorer_main_model_fallback(config)?;
        let (provider_default, model_default) =
            factory.reasoning_effort_defaults_for_role(role_names::EXPLORER);
        assert_eq!(provider_default, Some(harw_types::ReasoningEffort::High));
        assert_eq!(model_default, None);
        Ok(())
    }

    #[test]
    fn reasoning_effort_defaults_for_role_uses_model_default_for_main_model_fallback_when_provider_has_none()
    -> TestResult {
        // `with_main_model_selection` liefert eine ausdrückliche Auswahl (wie
        // die UIA-Worker-Fabrik sie über `uia_provider`/`uia_worker_model`
        // setzt): der Provider ist konfiguriert, trägt aber kein
        // `default_reasoning_effort`, das Modell schon.
        let mut config = harw_config::ResolvedConfig::default();
        config
            .providers
            .insert("acme".to_owned(), test_provider_toml(None)?);
        config
            .models
            .insert("acme-model".to_owned(), test_model_toml(Some("low"))?);
        let factory = factory_with_explorer_main_model_fallback(config)?
            .with_main_model_selection(Some("acme".to_owned()), Some("acme-model".to_owned()));
        let (provider_default, model_default) =
            factory.reasoning_effort_defaults_for_role(role_names::EXPLORER);
        assert_eq!(provider_default, None);
        assert_eq!(model_default, Some(harw_types::ReasoningEffort::Low));
        Ok(())
    }

    #[test]
    fn reasoning_effort_defaults_for_role_yields_none_for_unknown_main_model_provider_and_model()
    -> TestResult {
        let config = harw_config::ResolvedConfig::default();
        let factory = factory_with_explorer_main_model_fallback(config)?.with_main_model_selection(
            Some("missing-provider".to_owned()),
            Some("missing-model".to_owned()),
        );
        let (provider_default, model_default) =
            factory.reasoning_effort_defaults_for_role(role_names::EXPLORER);
        assert_eq!(provider_default, None);
        assert_eq!(model_default, None);
        Ok(())
    }

    // -------------------------------------------------------------------
    // Teil B4 — `with_host_permits`: Sandbox-Profil + Host-Permit-Verdrahtung
    // der Kind-Fabrik.
    // -------------------------------------------------------------------

    #[test]
    fn with_host_permits_defaults_to_strict_and_none_without_call() -> TestResult {
        let config = harw_config::ResolvedConfig::default();
        let chain = test_chain(&config);
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            chain,
        )
        .map_err(ctx("factory builds"))?;
        assert_eq!(
            factory.sandbox_profile,
            harw_sandbox::SandboxProfile::Strict
        );
        assert!(factory.host_permit_wiring.is_none());
        Ok(())
    }

    #[test]
    fn with_host_permits_stores_sandbox_profile_and_wiring_when_called() -> TestResult {
        let config = harw_config::ResolvedConfig::default();
        let chain = test_chain(&config);
        let ledger = Arc::new(harw_sandbox::ProcessPermitLedger::default());
        let registry = Arc::new(harw_sandbox::HostPermitSessionRegistry::default());
        // Der Empfänger wird sofort verworfen: dieser Test prüft nur, dass die
        // Fabrik die Verdrahtung speichert, nicht dass ein tatsächlicher
        // Host-Permit-Dialog beantwortet wird (das deckt
        // harw-registry-defaults/src/profile.rs's `permits_wiring`-Testmodul
        // bereits ab).
        let (sender, receiver) = harw_tool_shell::host_permit_prompt_channel();
        drop(receiver);
        let wiring = HostPermitWiring::new(Arc::clone(&ledger), Arc::clone(&registry), sender);

        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            chain,
        )
        .map_err(ctx("factory builds"))?
        .with_host_permits(harw_sandbox::SandboxProfile::Host, Some(wiring));

        assert_eq!(factory.sandbox_profile, harw_sandbox::SandboxProfile::Host);
        assert!(factory.host_permit_wiring.is_some());
        Ok(())
    }

    // -------------------------------------------------------------------
    // Welle 4 — Skills erreichen Agenten.
    // -------------------------------------------------------------------

    /// Ein Layer mit den Skills `review` (aktiviert) und `off` (deaktiviert)
    /// sowie eine Konfiguration, in der `agent` beide direkt führt.
    fn skill_fixture(agent: &str) -> TestResult<(tempfile::TempDir, harw_config::ResolvedConfig)> {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut config = harw_config::ResolvedConfig::default();
        for (name, enabled, body) in [("review", true, "Read tests first.\n"), ("off", false, "x")]
        {
            let dir = layer.path().join("skills").join(name);
            std::fs::create_dir_all(&dir).map_err(ctx("mkdir skill"))?;
            std::fs::write(
                dir.join("skill.toml"),
                format!("name = \"{name}\"\nenabled = {enabled}\ndescription = \"{name} skill\"\n"),
            )
            .map_err(ctx("write manifest"))?;
            std::fs::write(dir.join("instructions.md"), body).map_err(ctx("write body"))?;
            config.skills.insert(
                name.to_owned(),
                SkillToml {
                    name: name.to_owned(),
                    enabled,
                    description: format!("{name} skill"),
                    instructions_file: None,
                    tools: Vec::new(),
                    mcps: Vec::new(),
                },
            );
        }
        config.agents.insert(
            agent.to_owned(),
            harw_config::AgentToml {
                name: agent.to_owned(),
                role: "worker".to_owned(),
                description: String::new(),
                system_file: None,
                providers: Vec::new(),
                models: Vec::new(),
                skills: vec!["review".to_owned(), "off".to_owned(), "review".to_owned()],
                suggestions: harw_config::AgentSuggestionsToml::default(),
                primary_provider: None,
                secondary_providers: Vec::new(),
                timeout_seconds: 120,
                max_retries: 2,
            },
        );
        Ok((layer, config))
    }

    #[test]
    fn child_registry_skill_fragments_carry_enabled_skills_with_sha256() -> TestResult {
        let (layer, config) = skill_fixture(role_names::EXPLORER)?;
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?
        .with_skill_catalog(&config, vec![layer.path().to_path_buf()])
        .map_err(ctx("skill catalog"))?;

        let fragments = factory
            .skill_fragments_for_role(role_names::EXPLORER)
            .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
        assert_eq!(fragments.len(), 1, "{fragments:?}");
        assert!(fragments[0].starts_with("# Skill: review (sha256 "));
        assert!(fragments[0].contains("Read tests first."));
        assert!(
            factory
                .skill_fragments_for_role(role_names::PLANNER)
                .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?
                .is_empty(),
            "a role without agent.toml gets no skill fragments"
        );
        Ok(())
    }

    #[test]
    fn child_registry_without_skill_catalog_injects_nothing() -> TestResult {
        let config = harw_config::ResolvedConfig::default();
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?;
        assert!(
            factory
                .skill_fragments_for_role(role_names::EXPLORER)
                .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?
                .is_empty()
        );
        Ok(())
    }

    /// Eine gesenkte Test-Definition für `role` mit den DSL-Skills `skills`
    /// (ohne `time`-Abhängigkeit: Trace bleibt leer).
    fn ir_with_skills(skills: &[&str]) -> TestResult<ExecutableAgentIr> {
        let list = skills
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let raw = harw_agent_dsl::parse::parse_toml(&format!(
            "schema = \"harwness.agent/v1\"\n\
             id = \"harwness.agent.skill-child@1\"\n\
             version = \"1.0.0\"\n\
             role = \"worker\"\n\
             specialization = \"skill-child\"\n\
             skills = [{list}]\n"
        ))
        .map_err(ctx("parse test definition"))?;
        let mut config = raw.tables;
        config.insert(
            harw_agent_dsl::skills::SKILLS_CONFIG_KEY.to_owned(),
            toml::Value::Array(raw.skills.into_iter().map(toml::Value::String).collect()),
        );
        let resolved = harw_agent_dsl::resolved::ResolvedAgentDefinition {
            id: raw.id,
            version: raw.version,
            role: raw.role,
            specialization: raw.specialization,
            name: raw.name,
            description: raw.description,
            reasoning_effort: raw.reasoning_effort,
            authority: harw_agent_dsl::authority::AuthorityCeiling::default(),
            trace: harw_agent_dsl::resolved::ResolutionTrace { steps: Vec::new() },
            config,
        };
        harw_agent_dsl::lower(&resolved).map_err(ctx("lower test definition"))
    }

    #[test]
    fn merge_skill_names_puts_catalog_first_and_dedupes() {
        let merged = merge_skill_names(
            &["review".to_owned(), "off".to_owned(), "review".to_owned()],
            &["docs".to_owned(), "review".to_owned()],
        );
        assert_eq!(merged, ["review", "off", "docs"]);
    }

    #[test]
    fn child_registry_injects_definition_skills_deduped_with_catalog() -> TestResult {
        let (layer, config) = skill_fixture(role_names::EXPLORER)?;
        let mut factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?
        .with_skill_catalog(&config, vec![layer.path().to_path_buf()])
        .map_err(ctx("skill catalog"))?;
        // Katalog führt `review` für den Explorer; die Definition verlangt
        // es ebenfalls — es darf nur einmal injiziert werden.
        factory.builtin_definitions.insert(
            role_names::EXPLORER.to_owned(),
            ir_with_skills(&["review"])?,
        );
        // Planner: kein agent.toml, nur die Definition nennt `review`.
        factory.builtin_definitions.insert(
            role_names::PLANNER.to_owned(),
            ir_with_skills(&["review", "off"])?,
        );

        let explorer = factory
            .skill_fragments_for_role(role_names::EXPLORER)
            .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
        assert_eq!(explorer.len(), 1, "{explorer:?}");
        assert!(explorer[0].starts_with("# Skill: review (sha256 "));

        let planner = factory
            .skill_fragments_for_role(role_names::PLANNER)
            .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
        assert_eq!(planner.len(), 1, "disabled `off` is skipped: {planner:?}");
        assert!(planner[0].contains("Read tests first."));
        Ok(())
    }

    #[test]
    fn child_registry_fails_closed_on_unknown_definition_skill() -> TestResult {
        let (layer, config) = skill_fixture(role_names::EXPLORER)?;
        let mut factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?
        .with_skill_catalog(&config, vec![layer.path().to_path_buf()])
        .map_err(ctx("skill catalog"))?;
        factory.builtin_definitions.insert(
            role_names::PLANNER.to_owned(),
            ir_with_skills(&["missing"])?,
        );
        let Err(error) = factory.skill_fragments_for_role(role_names::PLANNER) else {
            return Err(crate::test_support::TestError::Unexpected(
                "an unknown definition skill must refuse the spawn".to_owned(),
            ));
        };
        assert!(error.message.contains("missing"), "{}", error.message);
        Ok(())
    }

    #[test]
    fn child_registry_without_catalog_refuses_definition_skills() -> TestResult {
        let config = harw_config::ResolvedConfig::default();
        let mut factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?;
        factory
            .builtin_definitions
            .insert(role_names::PLANNER.to_owned(), ir_with_skills(&["review"])?);
        assert!(
            factory
                .skill_fragments_for_role(role_names::PLANNER)
                .is_err()
        );
        assert!(
            factory
                .skill_fragments_for_role(role_names::EXPLORER)
                .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?
                .is_empty(),
            "roles without definition skills stay unaffected"
        );
        Ok(())
    }

    #[test]
    fn skill_fragment_helpers_serve_root_agents_and_fail_closed() -> TestResult {
        let (layer, config) = skill_fixture("emily")?;
        let roots = vec![layer.path().to_path_buf()];
        let fragments =
            agent_skill_fragments(&config, &roots, "emily").map_err(ctx("agent fragments"))?;
        assert_eq!(fragments.len(), 1);
        assert!(
            agent_skill_fragments(&config, &roots, "nobody")
                .map_err(ctx("unknown agent"))?
                .is_empty()
        );
        let all = enabled_skill_fragments(&config, &roots).map_err(ctx("enabled fragments"))?;
        assert_eq!(all, fragments);
        // Ein aktivierter Skill ohne auffindbares Verzeichnis: fail-closed.
        assert!(skill_instruction_fragments(&config, &[], &["review".to_owned()]).is_err());
        Ok(())
    }

    // -------------------------------------------------------------------
    // R1 — Orchestrator-Modellstellen und `delegate_wave` (B).
    // -------------------------------------------------------------------

    /// Eine Fabrik über den eingebauten Definitionen mit einer explizit
    /// aufgelösten Orchestrator-Stelle `point` (Provider `acme`, Modell
    /// `acme-model`, beide mit `default_reasoning_effort`).
    fn factory_with_orchestrator_point(
        point: InternalModelPoint,
    ) -> TestResult<RuntimeChildRegistryFactory> {
        let mut config = harw_config::ResolvedConfig::default();
        config
            .providers
            .insert("acme".to_owned(), test_provider_toml(Some("high"))?);
        config
            .models
            .insert("acme-model".to_owned(), test_model_toml(Some("low"))?);
        let chain = test_chain(&config);
        let mut internal_models = HashMap::new();
        internal_models.insert(
            point,
            harw_config::ResolvedInternalModel {
                point,
                provider: Some("acme".to_owned()),
                model: Some("acme-model".to_owned()),
                source: harw_config::InternalModelSource::Explicit,
            },
        );
        Ok(RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            chain,
        )
        .map_err(ctx("factory builds"))?
        .with_internal_models(internal_models)
        .with_reasoning_effort_config(Arc::new(config)))
    }

    #[test]
    fn orchestrator_point_for_organizational_role_maps_only_orchestrators() {
        use harw_agent_dsl::roles::AgentRoleId;

        assert_eq!(
            orchestrator_point_for_organizational_role(AgentRoleId::RootOrchestrator),
            Some(InternalModelPoint::RootOrchestrator)
        );
        assert_eq!(
            orchestrator_point_for_organizational_role(AgentRoleId::ChildOrchestrator),
            Some(InternalModelPoint::SubOrchestrator)
        );
        for role in [
            AgentRoleId::UserInterface,
            AgentRoleId::Worker,
            AgentRoleId::UiaWorker,
            AgentRoleId::AgentSteward,
        ] {
            assert_eq!(
                orchestrator_point_for_organizational_role(role),
                None,
                "{role:?}"
            );
        }
    }

    #[test]
    fn builtin_orchestrators_resolve_to_their_orchestrator_points() -> TestResult {
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
        )
        .map_err(ctx("factory builds"))?;
        assert_eq!(
            factory.internal_point_for_child(role_names::ROOT_ORCHESTRATOR),
            Some(InternalModelPoint::RootOrchestrator)
        );
        for role in [
            role_names::CODING_ORCHESTRATOR,
            role_names::RESEARCH_ORCHESTRATOR,
            role_names::ANALYSIS_ORCHESTRATOR,
        ] {
            assert_eq!(
                factory.internal_point_for_child(role),
                Some(InternalModelPoint::SubOrchestrator),
                "role {role}"
            );
        }
        for role in role_names::CHILD_ORCHESTRATORS {
            assert_eq!(
                factory.internal_point_for_child(role),
                Some(InternalModelPoint::SubOrchestrator),
                "role {role}"
            );
        }
        // Die namensbasierten Stellen bleiben unverändert; Worker und
        // unbekannte Rollen bekommen keine Orchestrator-Stelle.
        assert_eq!(
            factory.internal_point_for_child(role_names::EXPLORER),
            Some(InternalModelPoint::Explorer)
        );
        assert_eq!(factory.internal_point_for_child(role_names::EXECUTOR), None);
        assert_eq!(
            factory.internal_point_for_child("some-repo-local-role"),
            None
        );
        Ok(())
    }

    #[test]
    fn reasoning_effort_defaults_follow_the_orchestrator_points() -> TestResult {
        let root = factory_with_orchestrator_point(InternalModelPoint::RootOrchestrator)?;
        assert_eq!(
            root.reasoning_effort_defaults_for_role_task(role_names::ROOT_ORCHESTRATOR, None),
            (
                Some(harw_types::ReasoningEffort::High),
                Some(harw_types::ReasoningEffort::Low)
            )
        );
        // Die Root-Stelle gilt nicht für Child-Orchestratoren.
        assert_eq!(
            root.reasoning_effort_defaults_for_role(role_names::CODING_ORCHESTRATOR),
            (None, None)
        );

        let sub = factory_with_orchestrator_point(InternalModelPoint::SubOrchestrator)?;
        for role in [
            role_names::CODING_ORCHESTRATOR,
            role_names::RESEARCH_ORCHESTRATOR,
            role_names::ANALYSIS_ORCHESTRATOR,
        ] {
            assert_eq!(
                sub.reasoning_effort_defaults_for_role_task(
                    role,
                    Some(harw_core::TaskComplexity::Simple)
                ),
                (
                    Some(harw_types::ReasoningEffort::High),
                    Some(harw_types::ReasoningEffort::Low)
                ),
                "role {role}"
            );
        }
        assert_eq!(
            sub.reasoning_effort_defaults_for_role(role_names::ROOT_ORCHESTRATOR),
            (None, None)
        );
        Ok(())
    }

    #[test]
    fn orchestrators_keep_the_parent_model_on_the_main_model_fallback() -> TestResult {
        // Ohne explizite Wahl lösen beide Orchestrator-Stellen auf das
        // Hauptmodell auf — das Kind behält exakt den Eltern-Anbieter.
        let config = harw_config::ResolvedConfig::default();
        let parent: Arc<dyn ModelProvider> = Arc::new(harw_core::EchoModelProvider::new("echo"));
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::clone(&parent),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?
        .with_internal_models(resolve_internal_models_for_children(&config));
        for role in [
            role_names::ROOT_ORCHESTRATOR,
            role_names::CODING_ORCHESTRATOR,
            role_names::RESEARCH_ORCHESTRATOR,
            role_names::ANALYSIS_ORCHESTRATOR,
        ] {
            let model = factory
                .model_for_task(role, Some(harw_core::TaskComplexity::Complex))
                .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
            assert!(Arc::ptr_eq(&model, &parent), "role {role}");
            let model = factory
                .model_for(role)
                .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
            assert!(Arc::ptr_eq(&model, &parent), "role {role}");
        }
        Ok(())
    }

    #[test]
    fn explicit_orchestrator_point_pins_the_child_model() -> TestResult {
        let factory = factory_with_orchestrator_point(InternalModelPoint::SubOrchestrator)?;
        let pinned = factory
            .model_for_task(role_names::CODING_ORCHESTRATOR, None)
            .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
        let unpinned = factory
            .model_for_task(role_names::ROOT_ORCHESTRATOR, None)
            .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
        assert!(
            !Arc::ptr_eq(&pinned, &unpinned),
            "a resolved SubOrchestrator point must pin, the unresolved root point must not"
        );
        Ok(())
    }

    #[test]
    fn delegate_wave_is_offered_only_to_orchestrators() -> TestResult {
        assert!(role_gets_delegate_wave(role_names::ROOT_ORCHESTRATOR));
        for role in role_names::CHILD_ORCHESTRATORS {
            assert!(role_gets_delegate_wave(role), "role {role}");
        }
        for role in [
            role_names::EXPLORER,
            role_names::EXECUTOR,
            role_names::PLANNER,
            role_names::UIA_WORKER,
            role_names::AGENT_STEWARD,
            "some-repo-local-role",
        ] {
            assert!(!role_gets_delegate_wave(role), "role {role}");
        }

        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
        )
        .map_err(ctx("factory builds"))?
        .with_delegate_wave_store(Arc::new(harw_core::InMemoryStateStore::default()));
        let names: Vec<String> =
            harw_extension_api::ToolProvider::tools(&factory.delegate_wave_provider())
                .iter()
                .map(|spec| spec.name().to_owned())
                .collect();
        assert_eq!(names, vec![harw_core_bridge::DELEGATE_WAVE_TOOL.to_owned()]);
        Ok(())
    }

    /// Die Werkzeugnamen, die [`RuntimeChildRegistryFactory::with_knowledge_readers`]
    /// für `role` an einen leeren Builder hängt.
    fn knowledge_tools_mounted_for(
        factory: &RuntimeChildRegistryFactory,
        role: &str,
    ) -> Vec<String> {
        let Some(profile) = profile_for_role(role) else {
            return Vec::new();
        };
        let registry = factory
            .with_knowledge_readers(
                role,
                profile,
                harw_extension_api::ExtensionRegistryBuilder::default(),
            )
            .build();
        let mut names: Vec<String> = registry
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|spec| spec.name().to_owned())
            .collect();
        names.sort();
        names
    }

    /// Plan Teil D: eine Leserolle bekommt mit Wissensspeicher genau die
    /// lesenden Wissenswerkzeuge, die ihre Definition admittiert; eine Rolle
    /// außerhalb von `knowledge_tools_for_role` (hier `researcher-web`,
    /// `executor`, `uia-latex-writer`) bekommt keines, und ohne
    /// `with_knowledge` bekommt niemand etwas.
    #[test]
    fn knowledge_readers_are_mounted_only_for_admitting_reader_roles() -> TestResult {
        let without = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
        )
        .map_err(ctx("factory builds"))?;
        assert!(knowledge_tools_mounted_for(&without, role_names::EXPLORER).is_empty());

        let store = Arc::new(harw_knowledge::KnowledgeStore::new(std::path::Path::new(
            "/nonexistent/r4/knowledge",
        )));
        let factory = without.with_knowledge(store, None);
        for role in harw_registry_defaults::profile::KNOWLEDGE_READER_ROLES {
            let mut expected: Vec<String> =
                harw_registry_defaults::profile::knowledge_tools_for_role(role)
                    .iter()
                    .map(|tool| (*tool).to_owned())
                    .collect();
            expected.sort();
            assert_eq!(
                knowledge_tools_mounted_for(&factory, role),
                expected,
                "{role}"
            );
        }
        // Kanban nur für den Root-Orchestrator (ausdrücklicher Nutzerwunsch).
        assert!(
            knowledge_tools_mounted_for(&factory, role_names::ROOT_ORCHESTRATOR)
                .iter()
                .any(|tool| tool == "kanban.list")
        );
        assert!(
            !knowledge_tools_mounted_for(&factory, role_names::EXPLORER)
                .iter()
                .any(|tool| tool.starts_with("kanban."))
        );
        for role in [
            role_names::RESEARCHER_WEB,
            role_names::EXECUTOR,
            role_names::UIA_LATEX_WRITER,
            role_names::MATRIX_PLAYER,
            "some-repo-local-role",
        ] {
            assert!(
                knowledge_tools_mounted_for(&factory, role).is_empty(),
                "{role}"
            );
        }
        Ok(())
    }
}
