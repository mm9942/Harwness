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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use harw_agent_dsl::ExecutableAgentIr;
use harw_catalog::{
    CatalogSnapshot, SkillIndex, SkillRuntimeSnapshot, SpawnCapabilitySnapshot,
    load_skill_runtime_snapshot, resolve_skill_directory,
};
use harw_config::{
    InternalModelPoint, ResolvedConfig, ResolvedInternalModel, SkillToml, resolve_internal_model,
};
use harw_core::{
    ChildRegistryFactory, ManagedAgentSpawner, ModelProvider, PinnedModelProvider, StateStore,
};
use harw_extension_api::{
    AgentJobFuture, AgentJobSubmitter, AgentSpawnError, AgentSpawner, ExtensionRegistry,
    SpawnFuture, SpawnInput,
};
use harw_knowledge::caller_identity::CallerIdentity;
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
/// Zustandsänderungen pro Aufruf sind die frische
/// [`ApprovalModeCell`](harw_extension_api::approval_mode::ApprovalModeCell)
/// in [`ApprovalChain::for_child`], die das Kind allein besitzt, und die
/// vorgemerkte Lauf-Identität zwischen `build_registry` und
/// `child_session_observers` (siehe [`Self::child_caller_identity`]).
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
    /// Deferred weak reference to the runtime-wide durable agent-job submitter.
    /// Child registries receive the same async-by-default scheduling contract
    /// as the root without creating a strong registry/spawner/runtime cycle.
    agent_job_submitter_slot: Arc<OnceLock<Weak<dyn AgentJobSubmitter>>>,
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
    /// Der gebundene Root-Space der Wurzel, gesetzt über
    /// [`Self::with_home_context`]. Die Matrix-Werkzeuge des Game Masters
    /// lesen ihren Speicher (`<profil>/knowledge/matrix`) ausschließlich von
    /// dort. `None`: `matrix.start`/`matrix.draft_scenario` scheitern
    /// fail-closed mit `OpError::NotAvailable`, `matrix.status` listet keine
    /// Entwürfe; ein Rückfall auf `HARW_HOME` findet nicht statt.
    home_context: Option<Arc<harw_home::ResolvedHomeContext>>,
    /// Wissensspeicher (und optional Kanban-Ledger) für die lesenden
    /// Wissenswerkzeuge der Kinder (Plan Teil D). `None`, solange
    /// [`Self::with_knowledge`] nicht aufgerufen wurde — dann bekommt kein
    /// Kind `workbench.show`/`diary.read`/`palace.*`/`kanban.*`.
    knowledge: Option<ChildKnowledgeWiring>,
    /// Runde 5, Teil C: automatische Diary-Einträge der Kinder (Verdichtung,
    /// Sitzungsende; Agent-Id = laufgebundene Id des Kindes, siehe
    /// [`Self::child_caller_identity`]). `None`, solange
    /// [`Self::with_child_diary`] nicht aufgerufen wurde — dann schreibt kein
    /// Kind ins Tagebuch.
    diary: Option<Arc<crate::diary_wiring::DiaryRecorder>>,
    /// Runde 5, Teil G: eigene Modellwahl je UIA-Worker-Rolle. Nur bei der
    /// UIA-Worker-Fabrik gesetzt ([`Self::with_uia_worker_routing`]); dann
    /// bestimmt sie das Modell **jedes** Kindes dieser Fabrik beim Start.
    uia_worker_routing: Option<Arc<crate::uia_worker_routing::UiaWorkerRouting>>,
    /// Live-Modellwahl der Montage ([`Self::with_live_models`]): generische
    /// Controller-Auswahl und Rollenwahl für **neu** gestartete Kinder. Nur
    /// bei der Fabrik des Wurzel-Baums gesetzt; `None` → Stand des Starts.
    live_models: Option<Arc<crate::live_model::LiveModelRouting>>,
    /// Plan R9, Teil B: benutzerdefinierte Agenten aus dem Roster
    /// ([`harw_registry_defaults::AgentRoster::custom_wiring`]), nach
    /// Spawn-Name. Ihre geklemmte IR liegt in [`Self::builtin_definitions`];
    /// hier stehen Basisrolle (für alle namensgebundenen Tabellen), Profil und
    /// Instruktionen. Leer, solange [`Self::with_custom_agents`] nicht
    /// aufgerufen wurde.
    custom_agents: HashMap<String, harw_registry_defaults::CustomAgentWiring>,
    /// PL-90 W03d: die in [`ChildRegistryFactory::build_registry`] gerade
    /// gemintete Lauf-Identität je Rolle, bis
    /// [`ChildRegistryFactory::child_session_observers`] sie derselben
    /// Admission zuordnet. Der `ChildController` hält zwischen beiden Aufrufen
    /// den Sitzungs-Lock, die Aufrufe verschränken sich also nie; ein
    /// gescheiterter Admission-Rest wird von der nächsten Montage derselben
    /// Rolle überschrieben. Nie ein Schlüssel aus Modell-JSON.
    pending_run_identities: Mutex<HashMap<String, Arc<CallerIdentity>>>,
    /// Die vom Controller gemeldete Kind-Sitzung je Rolle
    /// ([`ChildRegistryFactory::announce_child_session`]), bis
    /// [`ChildRegistryFactory::build_registry`] sie entnimmt und daraus die
    /// Lauf-Identität ableitet. Der Controller meldet unmittelbar vor dem Bau
    /// derselben Admission und unter demselben Sitzungs-Lock; `build_registry`
    /// entnimmt den Eintrag als Erstes, ein Rest kann also nur von einer
    /// Admission stammen, die den Bau nie erreicht, und wird von der nächsten
    /// Meldung derselben Rolle überschrieben. Bei einer durablen
    /// Wiederherstellung ist es die ursprüngliche Kind-ID, die Identität also
    /// dieselbe wie vor dem Neustart. Nie ein Wert aus Modell-JSON.
    announced_child_sessions: Mutex<HashMap<String, harw_types::SessionId>>,
    /// PL-100 WP 1.5: der beim Registry-Bau einer Admission (mit
    /// `ParentGrant`) für die gemeldete Kind-Sitzung gebaute Skill-Injektor
    /// je Rolle, bis [`ChildRegistryFactory::child_session_observers`] ihn
    /// für genau diese Sitzung entnimmt. Gleiches Muster wie
    /// [`Self::pending_run_identities`]: der Controller hält zwischen beiden
    /// Aufrufen den Sitzungs-Lock; ein Rest einer gescheiterten Admission
    /// passt nicht zur Kind-ID der nächsten und wird verworfen.
    ///
    /// `child_session_observers` übergibt den entnommenen Injektor im Feld
    /// `harw_core::ChildSessionObservers::skill_injector` und die Deckung
    /// seiner Fähigkeits-Labels in `skill_label_backing`; `admit_inner`
    /// (`harw-core/src/child_controller.rs`) schneidet ihn dort mit der
    /// Kind-Aktivierung und hängt ihn per `AgentSession::with_skill_injector`
    /// an die Kind-Sitzung.
    pending_skill_injectors: Mutex<HashMap<String, PendingSkillInjector>>,
}

/// PL-100 WP 1.5: ein für genau eine Kind-Sitzung gebauter Skill-Injektor,
/// bis der Controller die Beobachter dieser Sitzung abholt.
struct PendingSkillInjector {
    /// Die Kind-Sitzung, für die der Injektor gebaut wurde.
    child: harw_types::SessionId,
    /// Der Injektor samt Deckung seiner Fähigkeits-Labels.
    injection: ChildSkillInjection,
}

/// PL-100 WP 1.5: der Skill-Injektor einer Kind-Sitzung
/// ([`RuntimeChildRegistryFactory::child_skill_injector`]) und die Werkzeuge,
/// die jedes seiner aufrufbaren Fähigkeits-Labels decken.
///
/// # Description
/// Beides geht zusammen an den Kern
/// (`harw_core::ChildSessionObservers::skill_injector` und
/// `::skill_label_backing`). `admit_inner` lässt ein Label nach dem
/// Aktivierungsschnitt nur stehen, solange mindestens eines seiner
/// deckenden Werkzeuge freigeschaltet bleibt (Review-Befund: ein Kind, dem
/// die Eltern-Aktivierung alle Schreibwerkzeuge sperrt, bekommt keinen
/// Skill für `filesystem.write`).
struct ChildSkillInjection {
    /// Der Injektor mit frischem Sitzungszustand.
    injector: Arc<harw_core::SkillInjector>,
    /// Je aufrufbarem Label die aufrufbaren Werkzeuge mit seinem Recht
    /// ([`CapabilityLabels::backing`]).
    label_backing: BTreeMap<String, BTreeSet<String>>,
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
    /// Plan R9, Teil A: der lesende Skill-Katalog über dieselben Layer plus
    /// eingebettetem Bündel — Quelle für `skills.search`/`skills.load` und
    /// Rückfall für fest gebundene Skills, die kein Layer führt.
    index: Arc<SkillIndex>,
    /// PL-100 WP 1.5: `[skills]` des Elternlaufs für den Skill-Injektor der
    /// Kinder (dieselben Einstellungen wie der Injektor der Wurzel).
    injection: harw_core::SkillInjectionSettings,
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
            agent_job_submitter_slot: Arc::new(OnceLock::new()),
            reasoning_effort_config: None,
            main_model_selection: None,
            effective_main_model: None,
            sandbox_profile: harw_sandbox::SandboxProfile::Strict,
            host_permit_wiring: None,
            skill_catalog: None,
            delegate_wave_store: None,
            home_context: None,
            knowledge: None,
            diary: None,
            uia_worker_routing: None,
            live_models: None,
            custom_agents: HashMap::new(),
            pending_run_identities: Mutex::new(HashMap::new()),
            announced_child_sessions: Mutex::new(HashMap::new()),
            pending_skill_injectors: Mutex::new(HashMap::new()),
        }
    }

    /// Hängt die Live-Modellwahl der Montage an (Live-Modellwechsel).
    ///
    /// # Description
    /// Danach bestimmen [`crate::live_model::LiveModelRouting::set_live_main`]
    /// (Hauptmodell ungepinnter Rollen) und
    /// [`harw_ops::live_model::LiveModelControl::set_internal_model`]
    /// (Rollenwahl) das Modell jedes **neu** gestarteten Kindes; die über
    /// [`Self::with_internal_models`] gesetzten Stellen gelten dann nicht
    /// mehr. Ohne Live-Wahl bleibt alles beim Stand des Starts.
    #[must_use]
    pub fn with_live_models(mut self, live: Arc<crate::live_model::LiveModelRouting>) -> Self {
        self.live_models = Some(live);
        self
    }

    /// Hängt die benutzerdefinierten Agenten des Rosters an (Plan R9, Teil B).
    ///
    /// # Description
    /// `custom_agents` stammt aus
    /// [`harw_registry_defaults::AgentRoster::custom_wiring`]; die geklemmten
    /// IRs derselben Namen müssen in den Definitionen dieser Fabrik liegen
    /// ([`Self::with_definitions`] mit [`harw_registry_defaults::AgentRoster::definitions`]).
    /// Für jeden solchen Namen gelten danach Profil und Instruktionen des
    /// Roster-Eintrags und die namensgebundenen Tabellen seiner Basisrolle.
    #[must_use]
    pub fn with_custom_agents(
        mut self,
        custom_agents: HashMap<String, harw_registry_defaults::CustomAgentWiring>,
    ) -> Self {
        self.custom_agents = custom_agents;
        self
    }

    /// Die eingebaute Rolle, deren namensgebundene Tabellen (Composition-
    /// Werkzeuge, Reducer, Delegationsziele, interne Modellstelle) für
    /// `role` gelten: für einen benutzerdefinierten Agenten seine Basisrolle,
    /// sonst `role` selbst (Plan R9, Teil B).
    fn lookup_role<'a>(&'a self, role: &'a str) -> &'a str {
        self.custom_agents
            .get(role)
            .map_or(role, |wiring| wiring.base_role.as_str())
    }

    /// Baut die vertrauenswürdige, **laufgebundene** Aufruferidentität
    /// eines neuen Kindes dieser Rolle (PL-90 W03d: Lauf-Isolation der
    /// Wissensflächen).
    ///
    /// # Beschreibung
    /// Die Identität entsteht ausschließlich hier, aus dem registrierten
    /// Rollennamen und einer frischen Sitzungs-ID — nie aus Modell-JSON oder
    /// `SpawnInput.context`:
    /// - `agent_id` ist `run_agent_id(role, kind)`, also
    ///   `<rolle>.<kind-sitzungs-id>` (UUID v4 aus `harw-types`, als
    ///   Pfadkomponente sicher). Jedes Kind bekommt eine eigene Id; zwei
    ///   gleichzeitige Geschwister **derselben** Rolle vergleichen sich in
    ///   `is_visible` also nie als Autor und sehen einander `SelfOnly`-
    ///   Einträge nicht. Dieselbe Id tragen Diary-Autor (`diary.read`,
    ///   Recorder) und die `OpContext`-Dienste der Operations-Werkzeuge des
    ///   Kindes; `build_registry` baut sie genau einmal je Kind und hält sie
    ///   für dessen ganzen Lauf. Weil sie aus der Kind-Sitzung folgt, die der
    ///   Controller vor dem Bau meldet
    ///   ([`ChildRegistryFactory::announce_child_session`]) und bei einer
    ///   durablen Wiederherstellung unverändert wiederherstellt, trägt ein
    ///   wiederhergestelltes Kind dieselbe Id wie vor dem Absturz und
    ///   behält sein Tagebuch und seine `SelfOnly`-Einträge;
    /// - `role` ist die Basisrolle ([`Self::lookup_role`]); für einen
    ///   benutzerdefinierten Agenten ist das seine eingebaute Rolle, nicht
    ///   sein eigener Name — wie bei allen namensgebundenen Tabellen
    ///   (Plan R9, Teil B) gilt eine Rollenfreigabe also auch für Agenten
    ///   auf dieser Basis (und für alle Geschwister dieser Rolle);
    /// - `ancestors` bleibt **leer**: `SpawnInput` trägt nur die
    ///   `parent_session_id`, keine Elternrolle oder -Id, und die Id des
    ///   Elternteils ist hier nicht vertrauenswürdig herleitbar. Das ist
    ///   fail-closed (ein Kind sieht den `DescendantTree` seines Elternteils
    ///   nicht), öffnet aber nichts: Geschwister sehen einander über
    ///   `SelfOnly` und `DescendantTree` nie, weil der Autor eines
    ///   Geschwisters weder der Aufrufer ist noch in dessen Vorfahren steht.
    ///
    /// Dieser Einstieg ohne Sitzung leitet die Id aus einer **frischen**
    /// Sitzungs-ID ab: jeder Aufruf liefert eine **neue** Instanz mit
    /// **neuer** Id; die Fabrik cacht nichts je Rolle. Öffentlich, damit Tests
    /// die Identität einer Montage prüfen können; ein Aufruf außerhalb von
    /// `build_registry` bindet nichts.
    #[must_use]
    pub fn child_caller_identity(&self, role: &str) -> Arc<CallerIdentity> {
        self.caller_identity_for_child(role, &harw_types::SessionId::new())
    }

    /// Wie [`Self::child_caller_identity`], aber für die Kind-Sitzung `child`:
    /// dieselbe Sitzung ergibt dieselbe `agent_id`, verschiedene Sitzungen
    /// verschiedene (siehe [`run_agent_id`]).
    fn caller_identity_for_child(
        &self,
        role: &str,
        child: &harw_types::SessionId,
    ) -> Arc<CallerIdentity> {
        Arc::new(CallerIdentity::new(run_agent_id(role, child)).with_role(
            harw_knowledge::AgentRoleRef::new(self.lookup_role(role).to_owned()),
        ))
    }

    /// Merkt die vom Controller gemeldete Kind-Sitzung vor, bis
    /// [`ChildRegistryFactory::build_registry`] sie entnimmt (siehe
    /// [`ChildRegistryFactory::announce_child_session`]).
    fn note_announced_child_session(&self, role: &str, child: &harw_types::SessionId) {
        let mut announced = self
            .announced_child_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        announced.insert(role.to_owned(), child.clone());
    }

    /// Entnimmt die für `role` gemeldete Kind-Sitzung; `None`, wenn kein
    /// Controller eine gemeldet hat (etwa bei einem direkten
    /// `build_registry`-Aufruf ohne Admission).
    fn take_announced_child_session(&self, role: &str) -> Option<harw_types::SessionId> {
        let mut announced = self
            .announced_child_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        announced.remove(role)
    }

    /// Merkt die Identität des soeben gebauten Kindes vor, bis
    /// [`ChildRegistryFactory::child_session_observers`] sie seiner Sitzung
    /// zuordnet (gleiches Muster wie `host_escalation_wiring::note_spawn`).
    fn note_run_identity(&self, role: &str, identity: Arc<CallerIdentity>) {
        let mut pending = self
            .pending_run_identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.insert(role.to_owned(), identity);
    }

    /// Entnimmt die vorgemerkte Identität von `role`; `None`, wenn für diese
    /// Montage keine vorgemerkt wurde.
    fn take_run_identity(&self, role: &str) -> Option<Arc<CallerIdentity>> {
        let mut pending = self
            .pending_run_identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.remove(role)
    }

    /// Die Identität, die der letzte `build_registry`-Aufruf für `role`
    /// montiert hat und die noch keiner Sitzung zugeordnet wurde.
    ///
    /// # Beschreibung
    /// Dieselbe `Arc`-Instanz, die in den `OpContext`-Diensten dieses Kindes
    /// steckt; ihre `agent_id` ist zugleich die des Diary-Providers und des
    /// Diary-Recorders. Nur zwischen `build_registry` und
    /// `child_session_observers` derselben Admission aussagekräftig (danach
    /// entnimmt der Observer-Haken sie) und für Tests gedacht: die
    /// `ExtensionRegistry` selbst hält keine Dienste.
    #[must_use]
    pub fn pending_run_identity(&self, role: &str) -> Option<Arc<CallerIdentity>> {
        let pending = self
            .pending_run_identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.get(role).cloned()
    }

    /// Das Registry-Profil von `role`: für einen benutzerdefinierten Agenten
    /// das seines Roster-Eintrags, sonst [`profile_for_role`].
    fn registry_profile_for(&self, role: &str) -> Option<harw_registry_defaults::RegistryProfile> {
        match self.custom_agents.get(role) {
            Some(wiring) => Some(wiring.profile),
            None => profile_for_role(role),
        }
    }

    /// Die Rechte, mit denen die Kind-Registry von `role` montiert wird
    /// (#22 Welle 1B): die Rechte von `profile`, geschnitten mit den
    /// Rechte-Labels aus `[authority] capabilities` der IR und — für einen
    /// benutzerdefinierten Agenten — mit den Rechten seines Rechte-Manifests
    /// ([`harw_registry_defaults::authority::granted_for_capabilities`]).
    /// Nie mehr als `profile.required_permissions()`.
    fn granted_for(
        &self,
        role: &str,
        profile: harw_registry_defaults::RegistryProfile,
    ) -> harw_authority::PermissionSet {
        let rights = profile.required_permissions();
        if let Some(wiring) = self.custom_agents.get(role) {
            return harw_registry_defaults::authority::granted_for_ir(&rights, &wiring.ir, true);
        }
        match self.builtin_definitions.get(role) {
            Some(ir) => harw_registry_defaults::authority::granted_for_capabilities(
                &rights,
                &ir.authority().capabilities,
                None,
            ),
            None => rights,
        }
    }

    /// Aktuelle Auflösung einer internen Modellstelle (live, sonst Start).
    fn resolved_point(&self, point: InternalModelPoint) -> Option<ResolvedInternalModel> {
        match &self.live_models {
            Some(live) => live.resolved_internal_model(point),
            None => self.internal_models.get(&point).cloned(),
        }
    }

    /// Der Modellanbieter eines Kindes auf dem Hauptmodell: mit Live-Wahl
    /// ein Pin auf deren Provider/Modell, sonst das Eltern-Modell.
    fn main_model(&self) -> Arc<dyn ModelProvider> {
        match &self.live_models {
            Some(live) => live.main_model_provider(&self.model),
            None => Arc::clone(&self.model),
        }
    }

    /// Provider/Modell des Hauptmodells für die Effort-Vorgaben: Live-Wahl,
    /// sonst [`Self::main_model_selection`], sonst die Config-Vorgabe.
    fn main_selection(
        &self,
        config: &harw_config::ResolvedConfig,
    ) -> (Option<String>, Option<String>) {
        if let Some((provider, model)) = self.live_models.as_ref().and_then(|live| live.live_main())
        {
            return (provider, Some(model));
        }
        match &self.main_model_selection {
            Some((provider, model)) => (provider.clone(), model.clone()),
            None => (
                config.harness.default_provider.clone(),
                config.harness.default_model.clone(),
            ),
        }
    }

    /// Runde 5, Teil G: hängt die Modellwahl der UIA-Worker-Rollen an.
    ///
    /// # Description
    /// Nur für die UIA-Worker-Fabrik der Montage: jedes über diese Fabrik
    /// gestartete Kind bekommt sein Modell dann aus
    /// [`crate::uia_worker_routing::UiaWorkerRouting::model_for_role`] —
    /// eigene feste Wahl oder „wie UIA“ (live). Ohne Aufruf bleibt das
    /// bisherige Verhalten (Eltern-Modell dieser Fabrik).
    #[must_use]
    pub fn with_uia_worker_routing(
        mut self,
        routing: Arc<crate::uia_worker_routing::UiaWorkerRouting>,
    ) -> Self {
        self.uia_worker_routing = Some(routing);
        self
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
    /// Connects child registries to the runtime-wide durable agent-job submitter.
    #[must_use]
    pub fn with_agent_job_submitter_slot(
        mut self,
        slot: Arc<OnceLock<Weak<dyn AgentJobSubmitter>>>,
    ) -> Self {
        self.agent_job_submitter_slot = slot;
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
    /// PL-100 WP 1.5: hält außerdem `[skills]` aus `config` fest; für jedes
    /// admittierte Kind, das den Skill-Katalog bekommt, baut die Fabrik damit
    /// einen eigenen Skill-Injektor (siehe `Self::child_skill_injector`),
    /// den `child_session_observers` der Kind-Sitzung übergibt.
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
        let index = Arc::new(SkillIndex::build(&skill_roots));
        self.skill_catalog = Some(SkillCatalogWiring {
            catalog,
            roots: skill_roots,
            index,
            injection: harw_core::SkillInjectionSettings::from_config(&config.harness.skills),
        });
        Ok(self)
    }

    /// Hängt `skills.search`/`skills.load` (Plan R9, Teil A) an die
    /// Kind-Registry von `role`.
    ///
    /// # Beschreibung
    /// Nur mit Skill-Katalog ([`Self::with_skill_catalog`]) und für jede
    /// Rolle aus [`harw_registry_defaults::profile::skill_catalog_tools_for_role`]
    /// — eingebaut oder eigene Definition. Bewusst **unabhängig** von
    /// `[tools].admitted`: die Sitzung schaltet beide Werkzeuge immer frei
    /// ([`harw_core::activation::ALWAYS_AVAILABLE_TOOLS`]), damit kein
    /// Zuschnitt einer Definition (etwa das Klemmen auf die Basisrolle) sie
    /// entfernt. Ein ausdrückliches `forbidden` der Definition gewinnt: dann
    /// bleibt auch die Kontextzeile weg.
    ///
    /// # Rückgabe
    /// Der Builder, ergänzt um den Katalog-Provider, oder unverändert.
    fn with_skill_catalog_tools(
        &self,
        role: &str,
        builder: harw_extension_api::ExtensionRegistryBuilder,
    ) -> harw_extension_api::ExtensionRegistryBuilder {
        match self.skill_catalog_index_for(role) {
            Some(index) => builder.tool_provider(Arc::new(
                harw_registry_defaults::SkillCatalogToolProvider::new(index),
            )),
            None => builder,
        }
    }

    /// Der Skill-Index, wenn `role` die Katalog-Werkzeuge bekommt (siehe
    /// [`Self::with_skill_catalog_tools`]).
    ///
    /// PL-100 WP 1.5 (Review-Befund): geprüft wird auch die Basisrolle
    /// ([`Self::lookup_role`]). Ein benutzerdefinierter Agent über einer
    /// Rolle aus
    /// [`harw_registry_defaults::profile::SKILL_CATALOG_EXCLUDED_ROLES`]
    /// (etwa über `security-egress-triage` oder `intel-scout`) bekommt wie
    /// seine Basis weder Katalog-Werkzeuge noch Katalog-Zeile noch
    /// Skill-Injektor: Sensortext aus `dod.*` soll ihm keinen Skill zuspielen
    /// (THREAT-MODEL B3).
    fn skill_catalog_index_for(&self, role: &str) -> Option<Arc<SkillIndex>> {
        let wiring = self.skill_catalog.as_ref()?;
        if harw_registry_defaults::profile::skill_catalog_tools_for_role(self.lookup_role(role))
            .is_empty()
        {
            return None;
        }
        let offered = harw_registry_defaults::profile::skill_catalog_tools_for_role(role);
        let forbidden = self.builtin_definitions.get(role).is_some_and(|ir| {
            offered.iter().any(|tool| {
                ir.tool_surface()
                    .forbidden()
                    .iter()
                    .any(|forbidden| forbidden == tool)
            })
        });
        (!offered.is_empty() && !forbidden).then(|| Arc::clone(&wiring.index))
    }

    /// Die Kontextfragmente einer Kind-Rolle: fest gebundene Skills
    /// ([`Self::skill_fragments_for_role`]) und — wenn die Rolle den
    /// Skill-Katalog bekommt — die einzeilige Katalog-Zeile
    /// ([`harw_registry_defaults::skill_catalog_hint`]).
    fn skill_context_for_role(&self, role: &str) -> Result<Vec<String>, AgentSpawnError> {
        let mut fragments = self.skill_fragments_for_role(role)?;
        if let Some(index) = self.skill_catalog_index_for(role) {
            fragments.push(harw_registry_defaults::skill_catalog_hint(index.len()));
        }
        Ok(fragments)
    }

    /// PL-100 WP 1.5 (PL-97 §8.1): der Skill-Injektor einer Kind-Sitzung von
    /// `role` über ihrer fertigen `registry` — wie der Injektor jeder
    /// Wurzel-Sitzung (`assembly.rs`, `with_skill_injector`), damit getriggerte
    /// Skill-Auszüge (Werkzeug-Erstnutzung, Befehle, Fehlercodes,
    /// Stichwörter) auch Kinder erreichen.
    ///
    /// # Description
    /// - Nur mit Skill-Katalog ([`Self::with_skill_catalog`]), nur mit
    ///   `[skills] triggered_injection` und nur für eine Rolle, die den
    ///   Skill-Katalog bekommt ([`Self::skill_catalog_index_for`]) — wie an der
    ///   Wurzel, die den Injektor nur zusammen mit dem L0-Katalog trägt. Die
    ///   Rollen aus
    ///   [`harw_registry_defaults::profile::SKILL_CATALOG_EXCLUDED_ROLES`]
    ///   (Triage, `intel-scout`, `context-steward`, Matrix-Sitze) und
    ///   benutzerdefinierte Agenten über ihnen bekommen deshalb keinen: ihr
    ///   Kontext bleibt bei ihren fest gebundenen Skills (etwa `harw-dod` der
    ///   Triage-Rollen, [`Self::skill_fragments_for_role`]), und Sensor- oder
    ///   Werkzeugtext kann ihnen keinen weiteren Skill zuspielen.
    /// - Werkzeug-Filter ([`harw_core::skill_injection::ToolAvailability`]):
    ///   registriert sind die Werkzeuge, die das Kind höchstens aufrufen kann
    ///   ([`child_callable_tools`]: Werkzeuge der Registry, geschnitten mit
    ///   der Werkzeugoberfläche seiner Definition und mit den Rechten des
    ///   Elternteils, `parent.permissions` — die Kind-Sandbox ist nie weiter,
    ///   `ensure_child_of`). Ein Skill, der nur Werkzeuge nennt, die das Kind
    ///   nicht aufrufen kann, wird nie eingespielt. Bekannt
    ///   ([`ToolAvailability`] `known`) sind der Werkzeugkatalog und alle
    ///   Werkzeuge der Registry, damit ein weggeschnittenes Werkzeug nie als
    ///   „unbekannt" durchrutscht.
    /// - Abstrakte Fähigkeits-Labels der Skills (`filesystem.read`,
    ///   `filesystem.write`, `process.execute`, …;
    ///   [`child_capability_labels`]) sind bekannt und gelten als aufrufbar,
    ///   wenn ein aufrufbares Werkzeug das Recht des Labels trägt. Labels mit
    ///   Schreib- oder Ausführungsrecht gelten all-of
    ///   ([`ToolAvailability::with_required`]): ein lesendes Kind bekommt
    ///   keinen Skill, der `filesystem.write` nennt, auch nicht neben
    ///   `filesystem.read`. Je aufrufbarem Label gehen die aufrufbaren
    ///   Werkzeuge mit seinem Recht als Deckung mit
    ///   ([`ChildSkillInjection::label_backing`]); der Kern lässt das Label
    ///   nach dem Aktivierungsschnitt nur stehen, solange eines davon
    ///   freigeschaltet bleibt.
    /// - Bewusst nicht hier: der Schnitt der Kind-Aktivierung mit der des
    ///   Elternteils (`narrow_base_activation` in `admit_inner`,
    ///   `harw-core/src/child_controller.rs`). `parent.tools` ist die
    ///   Eltern-**Registry** geschnitten mit der Eltern-Aktivierung; ein nur
    ///   dem Kind registriertes Werkzeug (`cargo.test_one` des
    ///   `test-engineer`) fehlt darin, bleibt aber aufrufbar. Den exakten
    ///   Schnitt macht der Kern bei der Übergabe (`admit_inner`, nach
    ///   `narrow_base_activation`) über
    ///   [`harw_core::SkillInjector::narrowed_to_callable`].
    /// - Fest gebundene Skills der Rolle ([`Self::fixed_skill_names`]) stehen
    ///   schon vollständig in ihren Instruktionen; der Injektor spielt sie
    ///   nie ein zweites Mal ein
    ///   ([`harw_core::SkillInjector::with_excluded_skills`]).
    /// - Ein Injektor je Kind, mit frischem Sitzungszustand über dem
    ///   gemeinsamen Index der Fabrik (wie je Wurzel-Sitzung).
    ///
    /// # Arguments
    /// - `role` (`&str`): der registrierte Rollenname.
    /// - `registry` (`&ExtensionRegistry`): die fertige Registry des Kindes.
    /// - `parent` (`&harw_core::ParentGrant`): die Rechte des unmittelbaren
    ///   Elternteils (vom Controller geprüft) als Obergrenze der Kind-Rechte.
    ///
    /// # Returns
    /// Den Injektor samt Deckung seiner Labels; `None`, wenn eine der
    /// Bedingungen fehlt.
    ///
    /// [`ToolAvailability`]: harw_core::skill_injection::ToolAvailability
    /// [`ToolAvailability::with_required`]: harw_core::skill_injection::ToolAvailability::with_required
    fn child_skill_injector(
        &self,
        role: &str,
        registry: &ExtensionRegistry,
        parent: &harw_core::ParentGrant,
    ) -> Option<ChildSkillInjection> {
        let wiring = self.skill_catalog.as_ref()?;
        if !wiring.injection.enabled {
            return None;
        }
        let index = self.skill_catalog_index_for(role)?;
        let registered = child_registry_tool_names(registry);
        let callable = child_callable_tools(
            &registered,
            self.builtin_definitions.get(role),
            &parent.permissions,
        );
        let mut known: BTreeSet<String> = harw_registry_defaults::tool_index::catalog_tool_names()
            .into_iter()
            .map(str::to_owned)
            .chain(registered)
            .collect();
        let labels = child_capability_labels(&index, &known, &callable);
        known.extend(labels.known);
        let availability = harw_core::skill_injection::ToolAvailability::new(
            callable.into_iter().chain(labels.callable),
            known,
        )
        .with_required(labels.required);
        Some(ChildSkillInjection {
            injector: Arc::new(
                harw_core::SkillInjector::new(index, wiring.injection)
                    .with_tool_availability(availability)
                    .with_excluded_skills(self.fixed_skill_names(role)),
            ),
            label_backing: labels.backing,
        })
    }

    /// PL-100 WP 1.5: die Namen der fest gebundenen Skills von `role` — die
    /// direkt konfigurierten des Katalogs vor denen der eingebauten
    /// Definition, ohne Dubletten (dieselbe Liste, aus der
    /// [`Self::skill_fragments_for_role`] die Instruktionsfragmente lädt).
    fn fixed_skill_names(&self, role: &str) -> Vec<String> {
        let definition: &[String] = self
            .builtin_definitions
            .get(role)
            .map(ExecutableAgentIr::skills)
            .unwrap_or(&[]);
        let catalog: &[String] = self
            .skill_catalog
            .as_ref()
            .and_then(|wiring| wiring.catalog.direct_skills_of(role))
            .unwrap_or(&[]);
        merge_skill_names(catalog, definition)
    }

    /// Merkt den Skill-Injektor der gerade gebauten Kind-Registry für die
    /// gemeldete Kind-Sitzung `child` vor, bis
    /// [`ChildRegistryFactory::child_session_observers`] ihn entnimmt.
    ///
    /// # Description
    /// Nur eine Admission meldet die Sitzung **und** reicht den `ParentGrant`
    /// durch (`build_registry_from_capability_snapshot_for_parent`). Ohne
    /// gemeldete Sitzung (direkter Bau ohne Admission) oder ohne
    /// Eltern-Grant (ohne ihn ist der Werkzeug-Filter nicht prüfbar,
    /// fail-closed) wird nichts vorgemerkt und ein Rest derselben Rolle
    /// verworfen.
    fn note_child_skill_injector(
        &self,
        role: &str,
        child: Option<&harw_types::SessionId>,
        registry: &ExtensionRegistry,
        parent: Option<&harw_core::ParentGrant>,
    ) {
        let injector = child.zip(parent).and_then(|(child, parent)| {
            self.child_skill_injector(role, registry, parent)
                .map(|injection| PendingSkillInjector {
                    child: child.clone(),
                    injection,
                })
        });
        let mut pending = self
            .pending_skill_injectors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match injector {
            Some(injector) => {
                pending.insert(role.to_owned(), injector);
            }
            None => {
                pending.remove(role);
            }
        }
    }

    /// Entnimmt den für `role` vorgemerkten Skill-Injektor, wenn er für die
    /// Sitzung `child` gebaut wurde; ein Injektor einer anderen Sitzung (Rest
    /// einer gescheiterten Admission) wird verworfen, nie übergeben.
    ///
    /// # Returns
    /// Den Injektor genau dieser Kind-Sitzung samt Deckung seiner Labels,
    /// sonst `None`. Der Eintrag der Rolle ist danach in jedem Fall entfernt.
    fn take_child_skill_injector(
        &self,
        role: &str,
        child: &harw_types::SessionId,
    ) -> Option<ChildSkillInjection> {
        let pending = self
            .pending_skill_injectors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(role)?;
        if pending.child == *child {
            Some(pending.injection)
        } else {
            tracing::debug!(role, "runtime.child_skill_injector.stale_dropped");
            None
        }
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

    /// Hinterlegt den gebundenen Root-Space der Wurzel für die
    /// Matrix-Werkzeuge des Game Masters.
    ///
    /// # Description
    /// Reiner Erbauer-Schritt, analog [`Self::with_delegate_wave_store`].
    /// Danach trägt der `OpContext` jedes Matrix-Werkzeugs
    /// (`matrix_game_master_provider`) diesen Kontext; ohne Aufruf
    /// scheitern die Operationen, die den Matrix-Speicher brauchen,
    /// fail-closed statt auf `HARW_HOME` zurückzufallen.
    ///
    /// # Arguments
    /// - `context` (`Arc<ResolvedHomeContext>`): dieselbe Instanz, die die
    ///   Montage an [`crate::RuntimeServices::with_home_context`] übergibt.
    #[must_use]
    pub fn with_home_context(mut self, context: Arc<harw_home::ResolvedHomeContext>) -> Self {
        self.home_context = Some(context);
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
    /// (laufgebundene Agent-Id, nie der bloße Rollenname), `palace.*` auf
    /// `established`-Knoten,
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

    /// Hinterlegt den Diary-Recorder für die automatischen Einträge der
    /// Kinder (Runde 5, Teil C).
    ///
    /// # Beschreibung
    /// Jedes über diese Fabrik admittierte Kind wird mit seiner laufgebundenen
    /// Agent-Id (`<rolle>.<uuid>`, PL-90 W03d: dieselbe Id wie in
    /// `diary.read` und `CallerIdentity`) beim Recorder angemeldet
    /// ([`crate::diary_wiring::DiaryRecorder::session_started`]); Verdichtung
    /// und Runden zählt der Recorder über die Beobachter, die
    /// [`ChildRegistryFactory::child_session_observers`] an die Kind-Sitzung
    /// hängt. Die Freigabe des Kindes
    /// ([`ChildRegistryFactory::child_session_released`]) schreibt höchstens
    /// einen Sitzungsende-Eintrag — und keinen, wenn das Kind keine Runde
    /// beendet hat.
    ///
    /// # Arguments
    /// - `recorder`: derselbe Recorder darf mehrere Fabriken bedienen
    ///   (Zähler je Sitzung); `None` lässt die Fabrik unverändert.
    #[must_use]
    pub fn with_child_diary(
        mut self,
        recorder: Option<Arc<crate::diary_wiring::DiaryRecorder>>,
    ) -> Self {
        if recorder.is_some() {
            self.diary = recorder;
        }
        self
    }

    /// Hängt die lesenden Wissens-Provider an die Kind-Registry von `role`
    /// (siehe [`Self::with_knowledge`]).
    ///
    /// `agent` ist die laufgebundene Agent-Id des Kindes
    /// ([`Self::child_caller_identity`]): unter ihr liest `diary.read`.
    ///
    /// # Rückgabe
    /// Der Builder, ergänzt um jeden zugelassenen Provider; unverändert ohne
    /// Wissensspeicher, für eine Rolle ohne Wissenszugang, ohne eingebaute
    /// Definition oder ohne `ReadWorkspace` im Profil.
    fn with_knowledge_readers(
        &self,
        role: &str,
        agent: &harw_knowledge::AgentId,
        profile: harw_registry_defaults::RegistryProfile,
        mut builder: harw_extension_api::ExtensionRegistryBuilder,
    ) -> harw_extension_api::ExtensionRegistryBuilder {
        let Some(knowledge) = &self.knowledge else {
            return builder;
        };
        let offered =
            harw_registry_defaults::profile::knowledge_tools_for_role(self.lookup_role(role));
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
            builder = builder.tool_provider(Arc::new(
                harw_registry_defaults::DiaryToolProvider::new(Arc::clone(store), agent.clone()),
            ));
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

    /// Hängt `host.sudo_exec` (`harw_tool_shell::SudoToolProvider`, Runde 5
    /// Teil B) an die Kind-Registry von `role`.
    ///
    /// # Beschreibung
    /// Fail-closed in drei Stufen: ohne sudo-Fragekanal in der
    /// Host-Permit-Verdrahtung (jeder Nicht-TUI-Einstieg) gibt es kein
    /// Werkzeug; eine Rolle außerhalb von
    /// [`harw_registry_defaults::profile::sudo_tools_for_role`] bekommt es nie;
    /// eine Rolle ohne eingebaute Definition, die es admittiert, ebenfalls
    /// nicht.
    ///
    /// # Rückgabe
    /// Der Builder, ergänzt um genau einen Provider oder unverändert.
    fn with_sudo_exec(
        &self,
        role: &str,
        builder: harw_extension_api::ExtensionRegistryBuilder,
    ) -> harw_extension_api::ExtensionRegistryBuilder {
        let Some(sender) = self
            .host_permit_wiring
            .as_ref()
            .and_then(|wiring| wiring.sudo_prompts.clone())
        else {
            return builder;
        };
        let offered = harw_registry_defaults::profile::sudo_tools_for_role(self.lookup_role(role));
        let admitted = self.builtin_definitions.get(role).is_some_and(|ir| {
            offered.iter().any(|tool| {
                ir.tool_surface()
                    .admitted()
                    .iter()
                    .any(|admitted| admitted == tool)
            })
        });
        if !admitted {
            return builder;
        }
        builder.tool_provider(Arc::new(harw_tool_shell::SudoToolProvider::new(
            sender, role,
        )))
    }

    /// Plan R9, Teil F: hängt die Job-Kontrollwerkzeuge
    /// ([`harw_registry_defaults::profile::JOB_CONTROL_TOOLS`]) an die
    /// Kind-Registry einer Orchestrator-Rolle.
    ///
    /// # Beschreibung
    /// Fail-closed wie [`Self::with_sudo_exec`]: ohne Job-Verwaltung in der
    /// Host-Permit-Verdrahtung (jeder Nicht-TUI-Einstieg) gibt es nichts; eine
    /// Rolle außerhalb von
    /// [`harw_registry_defaults::profile::job_control_tools_for_role`], eine
    /// Rolle, deren Profil selbst `shell.exec` (und damit `job.*`) trägt, und
    /// eine Rolle ohne eingebaute Definition, die die Werkzeuge admittiert,
    /// bekommen keinen Provider. Besitzprüfung: nur Jobs der eigenen
    /// Nachfahren (Elternkette der Job-Verdrahtung).
    fn with_job_control(
        &self,
        role: &str,
        profile: harw_registry_defaults::RegistryProfile,
        builder: harw_extension_api::ExtensionRegistryBuilder,
    ) -> harw_extension_api::ExtensionRegistryBuilder {
        let Some(jobs) = self
            .host_permit_wiring
            .as_ref()
            .and_then(|wiring| wiring.jobs.as_ref())
        else {
            return builder;
        };
        if profile.registered_tool_names().contains(&"shell.exec") {
            return builder;
        }
        let offered =
            harw_registry_defaults::profile::job_control_tools_for_role(self.lookup_role(role));
        let admitted = self.builtin_definitions.get(role).is_some_and(|ir| {
            offered.iter().any(|tool| {
                ir.tool_surface()
                    .admitted()
                    .iter()
                    .any(|admitted| admitted == tool)
            })
        });
        if !admitted {
            return builder;
        }
        builder.tool_provider(jobs.control_provider())
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
        internal_point_for_role(self.lookup_role(role)).or_else(|| {
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
    ///
    /// PL-90 W03d: dazu trägt jede `ServiceMap` die `identity` dieses Kindes
    /// (`Arc<CallerIdentity>`, siehe [`Self::child_caller_identity`]), damit
    /// Wissens-Operationen, die über diesen Kontext laufen, identitätsgebunden
    /// lesen ([`delegate_wave_services`]).
    /// Gibt der Kind-Registry den durablen Job-Submitter der Wurzel — aber
    /// nur, wenn die Wurzel einen hat.
    ///
    /// # Description
    /// Früher bekam jede Kind-Registry den aufgeschobenen Submitter, auch
    /// ohne Job-Store oder Freigabe-Akteur an der Wurzel. Die frühe Ablehnung
    /// „kein Hintergrund-Ausführer" griff dann nie; ein Kind wurde
    /// admittiert und scheiterte erst danach mit „durable agent job
    /// submitter is not available". Ohne Submitter lehnt der Turn-Loop die
    /// Delegation jetzt *vor* der Admission ab.
    fn with_deferred_job_submitter(
        &self,
        builder: harw_extension_api::ExtensionRegistryBuilder,
    ) -> harw_extension_api::ExtensionRegistryBuilder {
        if self
            .agent_job_submitter_slot
            .get()
            .and_then(Weak::upgrade)
            .is_some()
        {
            builder.agent_job_submitter(Arc::new(DeferredAgentJobSubmitter {
                slot: Arc::clone(&self.agent_job_submitter_slot),
            }))
        } else {
            builder
        }
    }

    fn delegate_wave_provider(&self, identity: Arc<CallerIdentity>) -> ModelToolProvider {
        let policy = delegate_wave_policy(&self.custom_agents);
        let operation: Arc<dyn Operation> =
            Arc::new(harw_core_bridge::DelegateWaveOperation::new(policy));
        let slot = Arc::clone(&self.spawner_slot);
        let state_store = self.delegate_wave_store.clone();
        ModelToolProvider::new([operation], move |execution_context| {
            let services = delegate_wave_services(&slot, state_store.as_ref(), &identity);
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

    /// Die Matrix-Werkzeuge des Game Masters (Runde 7, Teil M).
    ///
    /// # Description
    /// Baut einen [`ModelToolProvider`] mit genau den sechs Operationen aus
    /// `harw_ops::matrix::game_master::game_master_operations`
    /// (`matrix.draft_scenario`, `matrix.status`, `matrix.start`,
    /// `matrix.run`, `matrix.finish`, `matrix.add_fact`) — nur für die Rolle
    /// `matrix-game-master` montiert, nie über einen allgemeinen Provider
    /// (Kind-Registries tragen sonst keine Operations-Werkzeuge). Die
    /// Freigabe folgt der Deklaration der Operationen: `status`/`draft`/
    /// `add_fact` sind auto-freigegeben, `start`/`run`/`finish` laufen durch
    /// die Freigabekette des Kindes. Der `OpContext` entsteht je Aufruf wie
    /// bei [`Self::delegate_wave_provider`]; seine Dienste stellt
    /// [`game_master_services`] zusammen: Spawner (schwach gehalten) und
    /// `StateStore` für die Sitz-Agenten sowie den gebundenen Root-Space
    /// ([`Self::with_home_context`]) für den Matrix-Speicher. Sandbox und
    /// `CancelToken` kommen aus dem Ausführungskontext.
    ///
    /// PL-90 W03d: `identity` ist die Identität dieses Kindes
    /// ([`Self::child_caller_identity`]); sie liegt in jeder `ServiceMap`.
    fn matrix_game_master_provider(&self, identity: Arc<CallerIdentity>) -> ModelToolProvider {
        let slot = Arc::clone(&self.spawner_slot);
        let state_store = self.delegate_wave_store.clone();
        let home_context = self.home_context.clone();
        ModelToolProvider::new(
            harw_ops::matrix::game_master::game_master_operations(),
            move |execution_context| {
                let services = game_master_services(
                    &slot,
                    state_store.as_ref(),
                    home_context.as_ref(),
                    &identity,
                );
                let ctx = OpContext::new(
                    execution_context.session_id().clone(),
                    execution_context.turn_id().clone(),
                    execution_context.sandbox().clone(),
                    services,
                );
                match execution_context.cancel() {
                    Some(cancel) => ctx.with_cancel_token(cancel.clone()),
                    None => ctx,
                }
            },
        )
    }

    /// Die lesenden Harw-DoD-Werkzeuge der Triage-Rollen (PL-97 D4,
    /// PL-100 WP1.2).
    ///
    /// # Description
    /// Baut einen [`ModelToolProvider`] mit genau den sechs Operationen aus
    /// `harw_ops::dod`: den drei lesenden `dod.status`, `dod.findings` und
    /// `dod.sensors` (wie die Wurzel) und den drei Triage-Werkzeugen
    /// `dod.finding`, `dod.related` und `dod.labels`
    /// (`harw_ops::dod::dod_triage_operations`, nie in einer Root-Session)
    /// — montiert nur für die Rollen aus
    /// [`harw_registry_defaults::profile::dod_tools_for_role`] (die vier
    /// `security-*-triage`-Rollen). Alle sechs sind lesend und ohne Freigabe
    /// (`model_tool(readonly, approval = "none")`, in `AUTO_APPROVED_TOOLS`),
    /// tragen kein Sandbox-Recht und erreichen weder Warden noch
    /// Eskalationsleiter (Invariante S1). `dod.labels` findet im `OpContext`
    /// keine Label-Quelle und antwortet deshalb bis PL-100 Phase 2.5 mit
    /// `source: "unavailable"`. Der `OpContext` entsteht je Aufruf
    /// wie bei [`Self::matrix_game_master_provider`]; seine Dienste
    /// ([`dod_services`]) sind der gebundene Root-Space
    /// ([`Self::with_home_context`]), aus dem `harw_ops::dod::dod_service`
    /// den geteilten `DodService` (5-s-Cache, `[dod] export_dir` des
    /// Root-Space) ableitet, und die Identität des Kindes. Ohne Root-Space
    /// scheitert jeder Aufruf fail-closed mit `OpError::NotAvailable` — nie
    /// ein Rückfall auf `HARW_HOME`.
    fn dod_read_provider(&self, identity: Arc<CallerIdentity>) -> ModelToolProvider {
        let home_context = self.home_context.clone();
        let [finding, related, labels] = harw_ops::dod::dod_triage_operations();
        let operations: [Arc<dyn Operation>; 6] = [
            Arc::new(harw_ops::dod::DodStatusOperation),
            Arc::new(harw_ops::dod::DodFindingsOperation),
            Arc::new(harw_ops::dod::DodSensorsOperation),
            finding,
            related,
            labels,
        ];
        ModelToolProvider::new(operations, move |execution_context| {
            let services = dod_services(home_context.as_ref(), &identity);
            let ctx = OpContext::new(
                execution_context.session_id().clone(),
                execution_context.turn_id().clone(),
                execution_context.sandbox().clone(),
                services,
            );
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
                kind: Default::default(),
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
        let snapshots = load_enabled_skills(
            |name| wiring.catalog.skill(name),
            &wiring.roots,
            Some(&wiring.index),
            &names,
        )
        .map_err(|detail| AgentSpawnError {
            kind: Default::default(),
            message: format!("could not load the skills of child role '{role}': {detail}"),
        })?;
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
        let Some(resolved) = self.resolved_point(point) else {
            return (None, None);
        };
        if resolved.is_main_model() {
            let (provider_id, model_id) = self.main_selection(config);
            let provider_default = provider_id
                .as_deref()
                .and_then(|id| config.providers.get(id))
                .and_then(|provider| provider.default_reasoning_effort);
            let model_default = model_id
                .as_deref()
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
        let Some(resolved) = self.resolved_point(point) else {
            return self.main_model();
        };
        if resolved.is_main_model() {
            return self.main_model();
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

/// Leitet die laufgebundene Agent-Id eines Kindes aus seiner Sitzung ab:
/// `<rolle>.<kind-sitzungs-id>` (PL-90 W03d).
///
/// # Description
/// Die Kind-Sitzungs-ID ist über eine durable Wiederherstellung stabil
/// (`recover_root_child` stellt `ChildRecoveryView::child` wieder her) und
/// sonst eine UUID v4 aus den `harw-types`-Ids (keine neue Abhängigkeit), die
/// der Controller je Kind genau einmal vergibt. Dieselbe Sitzung ergibt
/// deshalb vor und nach einem Neustart dieselbe Id, und zwei Kinder
/// derselben Rolle bekommen nie dieselbe Id — ohne dass etwas Neues
/// persistiert werden müsste.
///
/// Pfadsicherheit: die Id ist eine einzelne Pfadkomponente
/// (`diary/<agent-id>/…`, [`harw_knowledge::diary::validate_agent_id`]).
/// Pfadtrenner, `:` und Steuerzeichen im Rollennamen werden durch `-`
/// ersetzt. Die Sitzungs-ID einer Wiederherstellung stammt aus einem
/// gespeicherten Snapshot und wird deshalb zeichenweise kodiert: `%`, `.`,
/// Pfadtrenner, `:` und Steuerzeichen werden zu `%XX` (Hexcode des Zeichens).
/// Die Kodierung ist injektiv — verschiedene Sitzungs-IDs bleiben
/// verschieden — und lässt eine UUID unverändert.
///
/// # Arguments
/// - `role`: der registrierte Rollenname des Kindes.
/// - `child`: die Sitzung des Kindes.
fn run_agent_id(role: &str, child: &harw_types::SessionId) -> harw_knowledge::AgentId {
    let component: String = role
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let mut run = String::with_capacity(child.as_str().len());
    for c in child.as_str().chars() {
        if matches!(c, '%' | '.' | '/' | '\\' | ':') || c.is_control() {
            run.push_str(&format!("%{:02X}", u32::from(c)));
        } else {
            run.push(c);
        }
    }
    harw_knowledge::AgentId::new(format!("{component}.{run}"))
}

/// Die Dienste des `OpContext` eines Matrix-Werkzeugs des Game Masters.
///
/// # Description
/// Legt den Spawner (aus dem schwachen Slot, erst hier aufgelöst), den
/// `StateStore` und den gebundenen Root-Space in die [`ServiceMap`], jeden
/// nur, wenn er vorhanden ist. Fehlt ein Dienst, scheitert die Operation,
/// die ihn verlangt, fail-closed mit `OpError::NotAvailable`; insbesondere
/// liest ohne `home_context` keine Operation den Matrix-Speicher über
/// `HARW_HOME`. Dazu kommt immer die Identität des Kindes (PL-90 W03d).
///
/// # Arguments
/// - `slot`: die Rückwärtsreferenz auf den Managed-Spawner.
/// - `state_store`: der geteilte `StateStore` des Laufs, falls gesetzt.
/// - `home_context`: der gebundene Root-Space der Wurzel, falls gesetzt.
/// - `identity`: die vertrauenswürdige Identität dieses Kindes (immer
///   gesetzt; dieselbe Instanz, nicht eine Kopie).
fn game_master_services(
    slot: &OnceLock<Weak<ManagedAgentSpawner>>,
    state_store: Option<&Arc<dyn StateStore>>,
    home_context: Option<&Arc<harw_home::ResolvedHomeContext>>,
    identity: &Arc<CallerIdentity>,
) -> ServiceMap {
    let mut services = ServiceMap::new();
    if let Some(spawner) = slot.get().and_then(Weak::upgrade) {
        services.insert(spawner);
    }
    if let Some(store) = state_store {
        services.insert(Arc::clone(store));
    }
    if let Some(context) = home_context {
        services.insert(Arc::clone(context));
    }
    services.insert(Arc::clone(identity));
    services
}

/// Die Dienste des `OpContext` der lesenden `dod.*`-Werkzeuge (PL-97 D4).
///
/// # Description
/// Nur der gebundene Root-Space (falls gesetzt) und die Identität des
/// Kindes — kein Spawner, kein `StateStore`: die Operationen lesen allein
/// den Harw-DoD-Export über `harw_ops::dod::dod_service`.
///
/// # Arguments
/// - `home_context`: der gebundene Root-Space der Wurzel, falls gesetzt.
/// - `identity`: die vertrauenswürdige Identität dieses Kindes.
fn dod_services(
    home_context: Option<&Arc<harw_home::ResolvedHomeContext>>,
    identity: &Arc<CallerIdentity>,
) -> ServiceMap {
    let mut services = ServiceMap::new();
    if let Some(context) = home_context {
        services.insert(Arc::clone(context));
    }
    services.insert(Arc::clone(identity));
    services
}

/// Die `delegate_wave`-Policy: Reducer und Delegationsziele je Rolle, mit
/// benutzerdefinierten Agenten über ihre Basisrolle (Plan R9, Teile B/C).
fn delegate_wave_policy(
    custom_agents: &HashMap<String, harw_registry_defaults::CustomAgentWiring>,
) -> harw_core_bridge::DelegateWavePolicy {
    // Plan R9, Teil B: ein benutzerdefinierter Agent (als Ziel oder als
    // Aufrufer) gilt mit Reducer und Delegationszielen seiner Basisrolle.
    // Eigene `[delegation].targets` eines benutzerdefinierten Aufrufers
    // gehen vor (sie filtern nur, was sichtbar und registriert ist).
    let custom: Arc<HashMap<String, harw_registry_defaults::CustomAgentWiring>> =
        Arc::new(custom_agents.clone());
    let custom_names: Arc<std::collections::HashSet<String>> =
        Arc::new(custom_agents.keys().cloned().collect());
    let target_custom = Arc::clone(&custom);
    let policy = harw_core_bridge::DelegateWavePolicy::new(
        move |role: &str| {
            let role = target_custom
                .get(role)
                .map_or(role, |wiring| wiring.base_role.as_str());
            harw_registry_defaults::authority_reducer_for_role(role).map(|reducer| reducer.id())
        },
        move |role: &str| match custom.get(role) {
            Some(wiring) => wiring.delegation_targets.clone().or_else(|| {
                harw_registry_defaults::authority::delegation_targets_for_role(&wiring.base_role)
            }),
            None => harw_registry_defaults::authority::delegation_targets_for_role(role),
        },
    );
    // Plan R9, Teil C: benutzerdefinierte Agenten bleiben sichtbar, wenn
    // eine eingebaute Deklaration nur eingebaute Rollen nennt.
    let custom_targets = Arc::clone(&custom_names);
    policy.with_custom_targets(move |role: &str| custom_targets.contains(role))
}

/// `delegate_wave` für einen Orchestrator, der selbst die Wurzelsitzung ist
/// (`--agent root-orchestrator`).
///
/// # Description
/// Bisher bekamen nur Kind-Registries das Werkzeug; ein Root-Orchestrator
/// als Wurzel sah es in seiner Aktivierung, konnte es aber nicht ausführen,
/// während ihn der Lesewächter nach dem Lesebudget genau dorthin schickte.
/// Die Wurzel hält den Spawner ohnehin stark; eine Lauf-Identität für
/// Wissens-Operationen hat sie (noch) nicht (PL-90/05), `delegate_wave`
/// braucht keine.
pub(crate) fn root_delegate_wave_provider(
    spawner: Arc<ManagedAgentSpawner>,
    state_store: Arc<dyn StateStore>,
    custom_agents: &HashMap<String, harw_registry_defaults::CustomAgentWiring>,
) -> ModelToolProvider {
    let policy = delegate_wave_policy(custom_agents);
    let operation: Arc<dyn Operation> =
        Arc::new(harw_core_bridge::DelegateWaveOperation::new(policy));
    ModelToolProvider::new([operation], move |execution_context| {
        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&spawner));
        services.insert(Arc::clone(&state_store));
        let ctx = OpContext::new(
            execution_context.session_id().clone(),
            execution_context.turn_id().clone(),
            execution_context.sandbox().clone(),
            services,
        );
        match execution_context.cancel() {
            Some(cancel) => ctx.with_cancel_token(cancel.clone()),
            None => ctx,
        }
    })
}

/// Die Dienste des `OpContext` von `delegate_wave` (PL-90 W03d).
///
/// # Description
/// Spawner (aus dem schwachen Slot, erst hier aufgelöst), `StateStore` — je
/// nur, wenn vorhanden — und die Identität des Kindes. Die Identität fehlt nie:
/// sie hängt nicht an einem Laufzeitdienst, sondern an der Rolle, mit der die
/// Registry gebaut wurde. Fehlen Spawner oder `StateStore`, scheitert
/// `delegate_wave` selbst fail-closed (`OpError::NotAvailable`).
///
/// # Arguments
/// - `slot`: die Rückwärtsreferenz auf den Managed-Spawner.
/// - `state_store`: der geteilte `StateStore` des Laufs, falls gesetzt.
/// - `identity`: die vertrauenswürdige Identität dieses Kindes.
fn delegate_wave_services(
    slot: &OnceLock<Weak<ManagedAgentSpawner>>,
    state_store: Option<&Arc<dyn StateStore>>,
    identity: &Arc<CallerIdentity>,
) -> ServiceMap {
    let mut services = ServiceMap::new();
    if let Some(spawner) = slot.get().and_then(Weak::upgrade) {
        services.insert(spawner);
    }
    if let Some(store) = state_store {
        services.insert(Arc::clone(store));
    }
    services.insert(Arc::clone(identity));
    services
}

impl RuntimeChildRegistryFactory {
    /// Der eine Bauweg einer Kind-Registry hinter
    /// [`ChildRegistryFactory::build_registry`] und — für jede Rolle außer
    /// [`role_names::AGENT_STEWARD`] —
    /// [`ChildRegistryFactory::build_registry_from_capability_snapshot_for_parent`].
    ///
    /// # Description
    /// Werkzeuge, Rechte, Identität und Instruktionen hängen allein an `role`
    /// (siehe die Dokumentation von `build_registry`). `parent` ändert an der
    /// Registry nichts; es schneidet nur den Werkzeug-Filter des
    /// Skill-Injektors des Kindes (PL-100 WP 1.5,
    /// [`Self::child_skill_injector`]): ohne `parent` wird keiner gebaut.
    ///
    /// # Arguments
    /// - `role` (`&str`): der registrierte Rollenname.
    /// - `input` (`&SpawnInput`): der vertrauenswürdige Spawn-Auftrag (nur
    ///   `parent_session_id` und der Auftrag für den Klassifizierer).
    /// - `parent` (`Option<&harw_core::ParentGrant>`): der geprüfte
    ///   Eltern-Kontext der Admission, `None` bei einem direkten Bau.
    ///
    /// # Returns
    /// Die fertige Registry des Kindes.
    ///
    /// # Errors
    /// [`AgentSpawnError`] wie `build_registry`.
    fn build_child_registry(
        &self,
        role: &str,
        input: &SpawnInput,
        parent: Option<&harw_core::ParentGrant>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        // Die gemeldete Kind-Sitzung gehört genau diesem Bau: als Erstes
        // entnehmen, auch wenn der Bau gleich scheitert, damit nichts für einen
        // späteren Bau derselben Rolle hängen bleibt.
        let announced_child = self.take_announced_child_session(role);
        // Plan R9, Teil B: ein benutzerdefinierter Agent läuft mit dem Profil
        // seines Roster-Eintrags; alle namensgebundenen Tabellen
        // (Composition-Werkzeuge, Matrix, Wissen, sudo) gelten für seine
        // Basisrolle (`lookup`). Identität, IR, Skills und Instruktionen
        // bleiben die des Agenten selbst (`role`).
        let lookup = self.lookup_role(role);
        let profile = self
            .registry_profile_for(role)
            .ok_or_else(|| AgentSpawnError {
                kind: Default::default(),
                message: format!(
                    "refusing to assemble a child registry for unknown role '{role}': \
                 no registry profile is declared for it"
                ),
            })?;
        // PL-90 W03d: die `ExtensionRegistry` kennt keine Dienste — sie halten
        // allein die `OpContext`-Fabriken der Operations-Werkzeuge eines
        // Kindes (`delegate_wave`, Matrix-Werkzeuge). Die laufgebundene
        // Identität (`<rolle>.<kind-sitzungs-id>`) entsteht hier genau einmal
        // je Kind (nie aus `input`/Modell-JSON, nie zwischen Geschwistern
        // geteilt), bleibt für dessen ganzen Lauf gleich und wandert in die
        // `ServiceMap` jeder dieser Fabriken, in den Diary-Provider
        // (`with_knowledge_readers`) und — über `note_run_identity` — in den
        // Diary-Recorder (`child_session_observers`). Sie folgt aus der
        // Kind-Sitzung, die der Controller gemeldet hat — bei einer durablen
        // Wiederherstellung die ursprüngliche, die Identität also dieselbe wie
        // vor dem Neustart; ohne Meldung (direkter Aufruf ohne Admission) aus
        // einer frischen Sitzungs-ID. Eine Elternrolle ist aus `SpawnInput`
        // nicht herleitbar; siehe [`Self::child_caller_identity`].
        // PL-100 WP 1.5: nur eine gemeldete Sitzung bekommt einen
        // Skill-Injektor (siehe `note_child_skill_injector` unten).
        let announced = announced_child.is_some();
        let child_session = announced_child.unwrap_or_default();
        let identity = self.caller_identity_for_child(role, &child_session);
        let overrides = IdentityOverrides {
            agent_name: Some(role.to_owned()),
            // PL-100 O11: Rollen, deren Profil ein fremdes Rollenbild trägt
            // (`intel-scout`/`context-steward` laufen mit `NoTools`, dessen
            // Beschreibung „parameter-only triage agent" ist), bekommen ihre
            // eigene Beschreibung; sonst gilt die des Profils (`None`).
            role_description: harw_registry_defaults::profile::role_description_for_role(lookup)
                .map(str::to_owned),
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
            // Instruktionsfragmente (leer ohne Skill-Katalog). Runde 7,
            // Teil M: rollenspezifische Regeln (Game Master) stehen davor.
            extra_context: {
                let mut fragments = self.skill_context_for_role(role)?;
                if let Some(prompt) =
                    harw_registry_defaults::embedded_agents::builtin_role_prompt(role)
                {
                    fragments.insert(0, prompt.to_owned());
                }
                // Plan R9, Teil B: die Arbeitsanweisung eines
                // benutzerdefinierten Agenten (aus seiner IR:
                // `instructions_file` bzw. `system.md`, #22 Welle 1B).
                if let Some(instructions) = self
                    .custom_agents
                    .get(role)
                    .and_then(|wiring| wiring.instructions.as_deref())
                {
                    fragments.insert(0, format!("# Arbeitsanweisung ({role})\n{instructions}"));
                }
                // #22 Welle 1B: `job.goal_kind` und `spawn.workspace_hint`
                // der IR als Auftragsrahmen des Kindes.
                if let Some(note) = self
                    .builtin_definitions
                    .get(role)
                    .and_then(|ir| spawn_metadata_note(role, ir))
                {
                    fragments.push(note);
                }
                fragments
            },
        };
        // Eine Kette je Kind: `for_child` legt eine Folgezelle an
        // ([`ApprovalModeCell::follower`], seit 2026-09-24 ungedeckelt —
        // unter `FullAccess` fragt auch kein Kind). Eine Umstellung der Wurzel erreicht
        // laufende Kinder sofort; ein `set` im Kind koppelt nur dieses Kind ab
        // — Geschwister bleiben voneinander unabhängig, folgen aber weiter der
        // Wurzel.
        // Runde 7, Teil A6: der Auftrag des Kindes geht an seinen
        // Auto-Modus-Klassifizierer (eigener Ring der letzten Aufrufe).
        let child_chain = self.chain.for_child_with_mandate(
            harw_extension_api::auto_mode::ChildMandate::from_spawn_input(role, input),
        );
        // Runde 5, Teil N: Elternteil und Rolle für den Baum-Pfad vormerken;
        // `child_session_observers` bindet danach die Kind-ID.
        crate::host_escalation_wiring::note_spawn(
            self.host_permit_wiring.as_ref(),
            &input.parent_session_id,
            role,
        );
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
        // #22 Welle 1B: die Rechte der Kind-Registry sind die des Profils,
        // geschnitten mit `[authority] capabilities` der IR und — für einen
        // benutzerdefinierten Agenten — mit den Rechten seines Manifests.
        // Ein reiner Schnitt: nie mehr als das Profil.
        let granted = self.granted_for(role, profile);
        let assembled =
            assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
                profile,
                &self.project,
                overrides,
                child_chain.mode().clone(),
                &granted,
                None,
                &self.sandbox_profile,
                self.host_permit_wiring.clone(),
            )
            .map_err(|error| AgentSpawnError {
                kind: Default::default(),
                message: format!("could not assemble child registry for role '{role}': {error}"),
            })?;
        // `install_over_default`, nicht `install`:
        // `assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits`
        // hat die `DefaultApprovalPolicy` über `child_chain.mode()` bereits
        // registriert (harw-registry-defaults/src/profile.rs, `approval_handler`
        // in der letzten Stufe der Delegationskette). Eine zweite wäre
        // wirkungsgleich, aber eine Dublette (Befund Z2c-06).
        let mut registry_builder = self
            .with_deferred_job_submitter(child_chain.install_over_default(assembled.registry))
            .spawner(Arc::new(DeferredManagedSpawner {
                slot: Arc::clone(&self.spawner_slot),
            }))
            // Plan R9, offenes Recherche-Netz (`[network].research_web =
            // "open"`): erste Anfrage je Domain fragt (unter `full` nicht),
            // offenes Web lesen nur Recherche-Rollen — gemessen an der
            // Basisrolle (`intel-web-researcher` → `researcher-web`). Ohne
            // offenes Netz lässt die Politik jeden Aufruf unberührt.
            .approval_handler(Arc::new(
                harw_registry_defaults::OpenWebApprovalPolicy::new(
                    child_chain.mode().clone(),
                    harw_registry_defaults::role_may_use_open_web(lookup),
                ),
            ));
        #[cfg(feature = "browser")]
        if self.browser.grants_role(role) {
            let browser_provider =
                harw_registry_defaults::profile::browser_tool_provider_for_config(&self.browser)
                    .map_err(|error| AgentSpawnError {
                        kind: Default::default(),
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
        if role_gets_delegate_wave(lookup) {
            registry_builder = registry_builder
                .tool_provider(Arc::new(self.delegate_wave_provider(Arc::clone(&identity))));
        }
        // Runde 7, Teil M: die Matrix-Werkzeuge ausschließlich für den Game
        // Master (rollengebundener `ModelToolProvider`, siehe
        // `matrix_game_master_provider`).
        if !harw_registry_defaults::profile::matrix_tools_for_role(lookup).is_empty() {
            registry_builder = registry_builder.tool_provider(Arc::new(
                self.matrix_game_master_provider(Arc::clone(&identity)),
            ));
        }
        // PL-97 D4, PL-100 WP1.2: die sechs lesenden `dod.*` (drei wie die
        // Wurzel plus die drei Triage-Werkzeuge) ausschließlich für die vier
        // Triage-Rollen (rollengebundener `ModelToolProvider`, siehe
        // `dod_read_provider`); die `SessionActivation` schaltet sie nur frei,
        // wenn die Definition sie admittiert.
        if !harw_registry_defaults::profile::dod_tools_for_role(lookup).is_empty() {
            registry_builder = registry_builder
                .tool_provider(Arc::new(self.dod_read_provider(Arc::clone(&identity))));
        }
        // PL-100 O11: `deps.locked` ausschließlich für `intel-scout`
        // (rollengebundener, auf genau dieses Werkzeug gefilterter
        // Deps-Provider neben dem leeren Profil `NoTools`). Das Recht
        // `ReadWorkspace` kommt aus der Kind-Sandbox (Reducer `ReadOnly`),
        // nicht aus dem Profil; die `SessionActivation` schaltet das Werkzeug
        // nur frei, wenn die Definition es admittiert.
        if !harw_registry_defaults::profile::lockfile_tools_for_role(lookup).is_empty() {
            registry_builder = registry_builder
                .tool_provider(harw_registry_defaults::profile::lockfile_read_provider());
        }
        // Runde 5, Teil H: `agent.result` für jede Rolle, die Kinder starten
        // darf; der Spawner wird wie bei `delegate_wave` nur schwach gehalten.
        if !harw_registry_defaults::profile::child_result_tools_for_role(lookup).is_empty() {
            let slot = Arc::clone(&self.spawner_slot);
            registry_builder = registry_builder.tool_provider(Arc::new(
                crate::agent_result_wiring::agent_result_provider(move || {
                    slot.get().and_then(Weak::upgrade)
                }),
            ));
        }
        // Runde 5, Teil M: `agent.message` für jede Rolle, die Kinder starten
        // darf, `parent.message` für jede Kind-Rolle mit Elternteil; die
        // `SessionActivation` schaltet sie nur frei, wenn die Definition sie
        // admittiert. Spawner schwach gehalten wie oben.
        if !harw_registry_defaults::profile::child_message_tools_for_role(lookup).is_empty() {
            let slot = Arc::clone(&self.spawner_slot);
            registry_builder = registry_builder.tool_provider(Arc::new(
                crate::agent_messaging_wiring::agent_message_provider(move || {
                    slot.get().and_then(Weak::upgrade)
                }),
            ));
        }
        if !harw_registry_defaults::profile::parent_message_tools_for_role(lookup).is_empty() {
            let slot = Arc::clone(&self.spawner_slot);
            registry_builder = registry_builder.tool_provider(Arc::new(
                crate::agent_messaging_wiring::parent_message_provider(move || {
                    slot.get().and_then(Weak::upgrade)
                }),
            ));
        }
        // Plan Teil D: lesende Wissenswerkzeuge nur für zugelassene Rollen,
        // deren Definition sie admittiert (siehe `with_knowledge_readers`).
        registry_builder =
            self.with_knowledge_readers(role, &identity.agent_id, profile, registry_builder);
        // Runde 5, Teil B: `host.sudo_exec` nur für die Host-Shell-Worker, nur
        // mit sudo-Fragekanal (TUI) und nur, wenn die Definition der Rolle es
        // admittiert (siehe `with_sudo_exec`).
        registry_builder = self.with_sudo_exec(role, registry_builder);
        // Plan R9, Teil F: Job-Kontrolle (`job.status/logs/stop/list/wait`)
        // für Orchestratoren ohne Shell (siehe `with_job_control`); Rollen
        // mit `shell.exec` tragen `job.*` bereits über ihr Profil.
        registry_builder = self.with_job_control(role, profile, registry_builder);
        // Plan R9, Teil A: `skills.search`/`skills.load` für jede Rolle außer
        // den dokumentierten Ausnahmen (siehe `with_skill_catalog_tools`).
        registry_builder = self.with_skill_catalog_tools(role, registry_builder);
        let registry = registry_builder.build();
        tracing::debug!(
            role,
            profile = ?profile,
            project_root = %self.project.project_root.display(),
            "runtime.child_registry.assembled"
        );
        // Erst nach erfolgreicher Montage vormerken: ein gescheiterter Bau
        // hinterlässt keine Identität für `child_session_observers`.
        self.note_run_identity(role, identity);
        // PL-100 WP 1.5: der Skill-Injektor des Kindes über genau dieser
        // Registry (Werkzeug-Filter samt Schnitt mit den Eltern-Rechten),
        // entnommen in `child_session_observers`.
        self.note_child_skill_injector(
            role,
            announced.then_some(&child_session),
            &registry,
            parent,
        );
        Ok(registry)
    }
}

impl ChildRegistryFactory for RuntimeChildRegistryFactory {
    /// Merkt die Kind-Sitzung der gerade beginnenden Admission vor, damit
    /// `build_registry` die Lauf-Identität daraus ableitet (PL-90 W03d). Bei
    /// einer durablen Wiederherstellung ist das die ursprüngliche Kind-ID —
    /// das Kind behält so seine Wissens-Identität (Tagebuch, `SelfOnly`).
    fn announce_child_session(&self, role: &str, child: &harw_types::SessionId) {
        self.note_announced_child_session(role, child);
    }

    /// Runde 5, Teil C: meldet das Kind mit seiner laufgebundenen Agent-Id
    /// (`<rolle>.<kind-sitzungs-id>`, aus `build_registry`) beim Diary-
    /// Recorder an und liefert dessen Verdichtungs- und Runden-Beobachter;
    /// ohne Recorder keine Diary-Beobachter.
    ///
    /// PL-100 WP 1.5: entnimmt außerdem den Skill-Injektor, den der
    /// Registry-Bau für genau diese Kind-Sitzung gebaut hat
    /// (`Self::child_skill_injector`), immer, auch ohne Recorder, damit kein
    /// Rest für einen späteren Bau hängen bleibt, und übergibt ihn in
    /// `ChildSessionObservers::skill_injector`. `admit_inner` schneidet ihn
    /// nach `narrow_base_activation` auf die Kind-Aktivierung
    /// ([`harw_core::SkillInjector::narrowed_to_callable`]) und hängt ihn per
    /// `AgentSession::with_skill_injector` an die Kind-Sitzung.
    ///
    /// Mit dem Injektor geht die Deckung seiner Fähigkeits-Labels in
    /// `ChildSessionObservers::skill_label_backing`: der Kern lässt ein
    /// Label wie `filesystem.write` nach dem Aktivierungsschnitt nur stehen,
    /// solange eines seiner deckenden Werkzeuge freigeschaltet bleibt.
    ///
    /// Benutzerdefinierte Agenten laufen über denselben Weg: ihre geklemmte
    /// IR liegt in `builtin_definitions`, und `child_skill_injector` prüft
    /// den Skill-Katalog an ihrer Basisrolle (`lookup_role`). Einen Injektor
    /// bekommt keine Rolle ohne Skill-Katalog
    /// ([`harw_registry_defaults::profile::SKILL_CATALOG_EXCLUDED_ROLES`])
    /// und kein benutzerdefinierter Agent über einer solchen Basisrolle.
    ///
    /// Ein Kind, das über einen `harw_core::child_backend::ChildBackend`
    /// läuft (Job-Lauf statt In-Process-Turn), trägt den Injektor zwar an
    /// seiner admittierten Sitzung, aber diese Sitzung fährt keine Runde:
    /// getriggerte Auszüge erreichen es noch nicht.
    fn child_session_observers(
        &self,
        role: &str,
        child: &harw_types::SessionId,
    ) -> harw_core::ChildSessionObservers {
        // Runde 5, Teil N: Kind-ID und Baum-Pfad für Host-Mode-Anfragen.
        crate::host_escalation_wiring::bind_child(self.host_permit_wiring.as_ref(), role, child);
        // PL-90 W03d: die in `build_registry` gemintete Lauf-Id dieses Kindes
        // (immer entnehmen, auch ohne Recorder, damit nichts hängen bleibt).
        // Ohne vorgemerkte Identität (z. B. die Steward-Sonderregistry, die
        // `build_registry` nicht durchläuft) wird die Id hier aus der
        // Kind-Sitzung abgeleitet — nie der bloße Rollenname, den sich
        // Geschwister teilen würden, und dieselbe nach einer Wiederherstellung.
        let agent = self.take_run_identity(role).map_or_else(
            || run_agent_id(role, child),
            |identity| identity.agent_id.clone(),
        );
        // PL-100 WP 1.5: der für genau diese Sitzung gebaute Skill-Injektor
        // samt Deckung seiner Labels (immer entnehmen, auch ohne Recorder);
        // `admit_inner` schneidet ihn mit der Kind-Aktivierung und hängt ihn
        // an die Kind-Sitzung.
        let (skill_injector, skill_label_backing) =
            self.take_child_skill_injector(role, child).map_or_else(
                || (None, BTreeMap::new()),
                |injection| (Some(injection.injector), injection.label_backing),
            );
        let Some(recorder) = &self.diary else {
            return harw_core::ChildSessionObservers {
                skill_injector,
                skill_label_backing,
                ..harw_core::ChildSessionObservers::default()
            };
        };
        recorder.session_started(child, Some(agent));
        harw_core::ChildSessionObservers {
            compaction: Some(recorder.compaction_observer(None)),
            tool_outcome: Some(recorder.tool_outcome_observer(None)),
            skill_injector,
            skill_label_backing,
        }
    }

    /// Runde 5, Teil C: Sitzungsende-Eintrag des Kindes (höchstens einer je
    /// Kind-Lauf, keiner ohne beendete Runde — siehe
    /// [`crate::diary_wiring::DiaryRecorder::session_closed`]).
    fn child_session_released(&self, _role: &str, child: &harw_types::SessionId) {
        // Runde 5, Teil N: freigegebene Kinder verlassen das Anfragenden-Buch.
        crate::host_escalation_wiring::release(self.host_permit_wiring.as_ref(), child);
        if let Some(recorder) = &self.diary {
            recorder.session_closed(child);
        }
    }

    /// Baut die Registry eines Kindes nach dem Profil seiner Rolle.
    ///
    /// # Argumente
    /// - `role` (`&str`): der registrierte Rollenname.
    /// - `input` (`&SpawnInput`): die Registry hängt allein an `role`, nie an
    ///   Modell-JSON; gelesen wird nur die vertrauenswürdige
    ///   `parent_session_id` (Runde 5, Teil N: Baum-Pfad für Host-Mode).
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
    ///
    /// # Identität (PL-90 W03d)
    /// Die Operations-Werkzeuge des Kindes (`delegate_wave`, Matrix-Werkzeuge)
    /// bauen ihren `OpContext` aus einer `ServiceMap`, die eine für dieses
    /// Kind neu erzeugte `Arc<CallerIdentity>` trägt
    /// ([`Self::child_caller_identity`]); kein Geschwister teilt sie, und ihre
    /// `agent_id` (`<rolle>.<kind-sitzungs-id>`) ist je Kind verschieden, auch
    /// bei gleicher Rolle, und über eine durable Wiederherstellung stabil
    /// (abgeleitet aus der Kind-Sitzung, die der Controller vor dem Bau
    /// meldet, siehe [`ChildRegistryFactory::announce_child_session`]; ohne
    /// Meldung, etwa bei einem direkten Aufruf, aus einer frischen Sitzungs-ID).
    /// Dieselbe Id ist der Autor von `diary.read` und des
    /// Diary-Recorders. Die übrigen Wissens-Lesewerkzeuge der Kinder
    /// (`palace.*`, …) laufen nicht über einen `OpContext` und lesen die
    /// Identität nicht; ihre Sicht bleibt die ihres jeweiligen Providers
    /// (`with_knowledge_readers`).
    fn build_registry(
        &self,
        role: &str,
        input: &SpawnInput,
        _suggestions: Option<&harw_catalog::AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        // PL-100 WP 1.5: ohne `ParentGrant` (kein Admissionspfad) baut die
        // Fabrik keinen Skill-Injektor (siehe `note_child_skill_injector`).
        self.build_child_registry(role, input, None)
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
                kind: Default::default(),
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
        // Runde 5, Teil G: UIA-Worker-Fabrik → eigene Wahl je Rolle.
        if let Some(routing) = &self.uia_worker_routing {
            return Ok(routing.model_for_role(role));
        }
        let Some(point) = self.internal_point_for_child(role) else {
            return Ok(self.main_model());
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
        // Runde 5, Teil G: UIA-Worker-Fabrik → eigene Wahl je Rolle.
        if let Some(routing) = &self.uia_worker_routing {
            return Ok(routing.model_for_role(role));
        }
        match self.internal_point_for_task(role, complexity) {
            Some(point) => Ok(self.pinned_model_for_point(role, point)),
            None => Ok(self.main_model()),
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
            .and_then(|point| self.resolved_point(point))
            .is_some_and(|resolved| !resolved.is_main_model());
        if pinned_point {
            return None;
        }
        // Live-Modellwechsel: das neu gewählte Hauptmodell, nicht das des
        // Starts.
        if let Some((_, model)) = self.live_models.as_ref().and_then(|live| live.live_main()) {
            return Some(model);
        }
        self.effective_main_model.clone()
    }

    /// Der Provider, den der Kind-Provider dieser Rolle fest anspricht
    /// (Pin einer Rollenwahl, der UIA-Worker-Wahl oder des Live-Wechsels).
    fn pinned_provider_for_task(
        &self,
        role: &str,
        complexity: Option<harw_core::TaskComplexity>,
    ) -> Option<String> {
        self.model_for_task(role, complexity)
            .ok()
            .and_then(|provider| provider.pinned_provider_id())
    }

    /// Der Provider, den ein ungepinntes Kind dieser Rolle ruft (Anzeige
    /// `<provider>/<modell>`): Live-Wahl, sonst die Hauptauswahl der Fabrik,
    /// sonst die Config-Vorgabe; `None` bei einer gepinnten Stelle.
    fn main_provider_for_task(
        &self,
        role: &str,
        complexity: Option<harw_core::TaskComplexity>,
    ) -> Option<String> {
        let pinned_point = self
            .internal_point_for_task(role, complexity)
            .and_then(|point| self.resolved_point(point))
            .is_some_and(|resolved| !resolved.is_main_model());
        if pinned_point {
            return None;
        }
        if let Some((provider, _)) = self.live_models.as_ref().and_then(|live| live.live_main()) {
            return provider;
        }
        self.main_model_selection
            .as_ref()
            .and_then(|(provider, _)| provider.clone())
            .or_else(|| {
                self.reasoning_effort_config
                    .as_ref()
                    .and_then(|config| config.harness.default_provider.clone())
            })
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
    /// Für jede Rolle außer [`role_names::AGENT_STEWARD`] dieselbe Registry
    /// wie [`Self::build_registry`] (über `build_child_registry`; der
    /// Snapshot bleibt wie bisher für die Registry unbeachtet). `parent`
    /// schneidet dort nur den Werkzeug-Filter des Skill-Injektors des Kindes
    /// (PL-100 WP 1.5, `child_skill_injector`), nie die Registry selbst.
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
    fn build_registry_from_capability_snapshot_for_parent(
        &self,
        role: &str,
        input: &SpawnInput,
        _snapshot: Option<&harw_catalog::SpawnCapabilitySnapshot>,
        parent: &harw_core::ParentGrant,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        if role != role_names::AGENT_STEWARD {
            // Wie bisher `build_registry_with_capabilities` → `build_registry`
            // (beide lassen Snapshot und Vorschläge für die Registry
            // unbeachtet); neu reicht der Pfad `parent` für den
            // Werkzeug-Filter des Skill-Injektors durch (PL-100 WP 1.5).
            return self.build_child_registry(role, input, Some(parent));
        }
        // Die Steward-Sonderregistry trägt keine laufgebundene Identität (keine
        // Wissens- oder Operations-Werkzeuge); die gemeldete Kind-Sitzung wird
        // entnommen, damit sie keinen späteren Bau derselben Rolle erreicht.
        // `child_session_observers` leitet die Id dieses Kindes selbst ab.
        // PL-100 WP 1.5: die Sitzung bindet nur noch den Skill-Injektor.
        let announced_child = self.take_announced_child_session(role);

        let profile = profile_for_role(role).ok_or_else(|| AgentSpawnError {
            kind: Default::default(),
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
            extra_context: self.skill_context_for_role(role)?,
            ..IdentityOverrides::default()
        };
        // Runde 7, Teil A6: wie in `build_registry`.
        let child_chain = self.chain.for_child_with_mandate(
            harw_extension_api::auto_mode::ChildMandate::from_spawn_input(role, input),
        );

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
            project_agents_dir: Some(
                self.project
                    .project_root
                    .join(harw_home::project_dir_name())
                    .join("agents"),
            ),
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
                kind: Default::default(),
                message: format!("could not assemble child registry for role '{role}': {error}"),
            })?;
        // `install_over_default`: siehe Begründung in `build_registry`.
        let registry_builder = self
            .with_deferred_job_submitter(child_chain.install_over_default(assembled.registry))
            .spawner(Arc::new(DeferredManagedSpawner {
                slot: Arc::clone(&self.spawner_slot),
            }));
        // Plan R9, Teil A: auch der Steward findet und lädt Skills.
        let registry = self
            .with_skill_catalog_tools(role, registry_builder)
            .build();
        // PL-100 WP 1.5: wie in `build_child_registry`.
        self.note_child_skill_injector(role, announced_child.as_ref(), &registry, Some(parent));
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

/// PL-100 WP 1.5: die Namen aller Werkzeuge einer fertigen Kind-Registry
/// (in Provider-Reihenfolge, mit möglichen Dubletten).
fn child_registry_tool_names(registry: &ExtensionRegistry) -> Vec<String> {
    registry
        .tool_providers()
        .iter()
        .flat_map(|provider| provider.tools())
        .map(|spec| spec.name().to_owned())
        .collect()
}

/// PL-100 WP 1.5: die Werkzeuge, die ein Kind höchstens aufrufen kann —
/// für den Werkzeug-Filter seines Skill-Injektors.
///
/// # Description
/// Aus den Werkzeugen seiner Registry (`registered`) bleibt ein Name nur,
/// wenn beide Schnitte ihn zulassen:
/// 1. die Werkzeugoberfläche der Definition `ir` ([`surface_admits`]):
///    genau die Regel, nach der
///    `harw_core::session::AgentSession::with_executable_agent_ir` die
///    Aktivierung des Kindes baut (`admitted` plus
///    [`harw_core::activation::ALWAYS_AVAILABLE_TOOLS`], `forbidden`
///    gewinnt); für einen benutzerdefinierten Agenten ist das seine
///    geklemmte IR; ohne Definition entfällt dieser Schnitt;
/// 2. die Rechte des Elternteils (`permissions`, `ParentGrant::permissions`):
///    die Kind-Sandbox ist nie weiter (`SandboxSpec::ensure_child_of` in
///    `admit_inner`). Ein Werkzeug, dessen Recht
///    ([`harw_registry_defaults::authority::tool_permission`]) darin fehlt,
///    fällt weg; ein Werkzeug ohne Sandbox-Recht (`None`, etwa die lesenden
///    `dod.*`) bleibt.
///
/// Review-Befund (PL-100 WP 1.5): weder der Reducer der Basisrolle noch die
/// Eltern-Registry (`ParentGrant::tools`) schneiden hier. Der Reducer ist
/// nicht der Weg, auf dem ein direkt gestartetes Kind seine Rechte bekommt
/// (der `test-engineer` über der Analyst-Basis hält `ExecuteProcess` für
/// `cargo.test_one`), und der Kern schneidet die Kind-Aktivierung nur mit
/// der Eltern-**Aktivierung** (`narrow_base_activation`), nicht mit der
/// Eltern-Registry: ein nur dem Kind registriertes Werkzeug bleibt
/// aufrufbar. Diesen Aktivierungsschnitt macht der Kern bei der Übergabe
/// des Injektors (`admit_inner`, nach `narrow_base_activation`, über
/// [`harw_core::SkillInjector::narrowed_to_callable`]).
///
/// # Arguments
/// - `registered` (`&[String]`): die Werkzeuge der Kind-Registry.
/// - `ir` (`Option<&ExecutableAgentIr>`): die (geklemmte) Definition.
/// - `permissions` (`&harw_authority::PermissionSet`): die Rechte des
///   Elternteils als Obergrenze.
///
/// # Returns
/// Sortiert und dublettenfrei.
fn child_callable_tools(
    registered: &[String],
    ir: Option<&ExecutableAgentIr>,
    permissions: &harw_authority::PermissionSet,
) -> Vec<String> {
    let mut names: Vec<String> = registered
        .iter()
        .filter(|name| {
            ir.is_none_or(|ir| {
                let surface = ir.tool_surface();
                surface_admits(name, surface.admitted(), surface.forbidden())
            })
        })
        .filter(|name| {
            harw_registry_defaults::authority::tool_permission(name)
                .is_none_or(|permission| permissions.contains(permission))
        })
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    names
}

/// PL-100 WP 1.5 (Review-Befund): die abstrakten Fähigkeits-Labels der
/// Skills eines Index, aufgeteilt für den Werkzeug-Filter eines Kindes
/// ([`child_capability_labels`]).
#[derive(Debug, Default, PartialEq, Eq)]
struct CapabilityLabels {
    /// Jedes deklarierte Label mit Rechtebezug
    /// ([`harw_registry_defaults::authority::capability_permission`]).
    known: Vec<String>,
    /// Die Labels, deren Recht ein aufrufbares Werkzeug des Kindes trägt.
    callable: Vec<String>,
    /// Die Labels mit Schreib- oder Ausführungsrecht (`WriteWorkspace`,
    /// `ExecuteProcess`, `ManagePlugins`, `ManageContainers`): all-of.
    required: Vec<String>,
    /// Je Label aus `callable` die aufrufbaren Werkzeuge, die sein Recht
    /// tragen (nie leer): der Kern lässt das Label nach dem
    /// Aktivierungsschnitt nur stehen, solange eines davon freigeschaltet
    /// bleibt (`harw_core::ChildSessionObservers::skill_label_backing`).
    backing: BTreeMap<String, BTreeSet<String>>,
}

/// PL-100 WP 1.5 (Review-Befund): ordnet die abstrakten Fähigkeits-Labels,
/// die Skills in `index` deklarieren (`skill.toml` `tools` und
/// `[triggers] tools`, etwa `filesystem.read`, `filesystem.write`,
/// `process.execute`), den aufrufbaren Werkzeugen eines Kindes zu.
///
/// # Description
/// Ein Label ist jeder deklarierte Name außerhalb von `tools` (Katalog und
/// Registry), dessen Recht
/// [`harw_registry_defaults::authority::capability_permission`] kennt;
/// fachliche Namen ohne Rechtebezug bleiben unbekannt und filtern nichts.
/// Aufrufbar ist ein Label, wenn ein Werkzeug aus `callable` dasselbe Recht
/// trägt ([`harw_registry_defaults::authority::tool_permission`]): ein
/// lesendes Kind mit `fs.read` deckt `filesystem.read`, nie
/// `filesystem.write`. Diese Werkzeuge sind die Deckung des Labels
/// ([`CapabilityLabels::backing`]); sperrt die Eltern-Aktivierung später
/// alle, nimmt der Kern das Label weg. Labels mit Schreib- oder
/// Ausführungsrecht werden all-of geprüft, damit
/// `[filesystem.read, filesystem.write]` ein lesendes Kind nicht über das
/// Lese-Label erreicht.
///
/// # Arguments
/// - `index` (`&SkillIndex`): der Skill-Index des Kindes.
/// - `tools` (`&BTreeSet<String>`): die bekannten Werkzeugnamen.
/// - `callable` (`&[String]`): die aufrufbaren Werkzeuge
///   ([`child_callable_tools`]).
///
/// # Returns
/// Die Labels je Klasse, sortiert und dublettenfrei.
fn child_capability_labels(
    index: &SkillIndex,
    tools: &BTreeSet<String>,
    callable: &[String],
) -> CapabilityLabels {
    use harw_authority::Permission;

    // Je Recht die aufrufbaren Werkzeuge, die es tragen.
    let mut held: BTreeMap<Permission, BTreeSet<String>> = BTreeMap::new();
    for tool in callable {
        if let Some(permission) = harw_registry_defaults::authority::tool_permission(tool) {
            held.entry(permission).or_default().insert(tool.to_owned());
        }
    }
    let declared: BTreeSet<&str> = index
        .entries()
        .flat_map(|entry| entry.tools.iter().chain(&entry.triggers.tools))
        .map(String::as_str)
        .filter(|name| !tools.contains(*name))
        .collect();
    let mut labels = CapabilityLabels::default();
    for label in declared {
        let Some(permission) = harw_registry_defaults::authority::capability_permission(label)
        else {
            continue;
        };
        labels.known.push(label.to_owned());
        if let Some(backing) = held.get(&permission) {
            labels.callable.push(label.to_owned());
            labels.backing.insert(label.to_owned(), backing.clone());
        }
        if matches!(
            permission,
            Permission::WriteWorkspace
                | Permission::ExecuteProcess
                | Permission::ManagePlugins
                | Permission::ManageContainers
        ) {
            labels.required.push(label.to_owned());
        }
    }
    labels
}

/// Ob die Werkzeugoberfläche einer Definition (`admitted`, `forbidden`) das
/// Werkzeug `name` freischaltet: `forbidden` gewinnt, sonst `admitted` oder
/// [`harw_core::activation::ALWAYS_AVAILABLE_TOOLS`].
fn surface_admits(name: &str, admitted: &[String], forbidden: &[String]) -> bool {
    !forbidden.iter().any(|tool| tool == name)
        && (admitted.iter().any(|tool| tool == name)
            || harw_core::activation::ALWAYS_AVAILABLE_TOOLS.contains(&name))
}

/// #22 Welle 1B: der Auftragsrahmen eines Kindes aus seiner IR —
/// `job.goal_kind` und `spawn.workspace_hint` — als Instruktionsfragment.
///
/// # Description
/// Die Laufzeit hat keinen Mechanismus, den Arbeitsbereich eines Kindes nach
/// einem Hinweis zu wählen: die Sandbox des Kindes ist immer ein Schnitt der
/// Eltern-Sandbox (`ManagedAgentSpawner::admit`). `workspace_hint` ist daher
/// **beratend** und wird dem Kind nur mitgeteilt; `goal_kind` benennt die Art
/// des Auftrags.
///
/// # Returns
/// `None`, wenn die IR keins von beiden trägt.
pub(crate) fn spawn_metadata_note(role: &str, ir: &ExecutableAgentIr) -> Option<String> {
    let goal_kind = ir.job_template().goal_kind();
    let workspace_hint = ir.spawn_contract().workspace_hint();
    if goal_kind.is_none() && workspace_hint.is_none() {
        return None;
    }
    let mut note = format!("# Auftragsrahmen ({role})");
    if let Some(goal_kind) = goal_kind {
        note.push_str(&format!("\n- Zielart (goal_kind): {goal_kind}"));
    }
    if let Some(hint) = workspace_hint {
        note.push_str(&format!(
            "\n- Arbeitsbereich-Hinweis (workspace_hint, beratend): {hint} — der \
             Arbeitsbereich selbst ist durch die Sandbox des Elternteils festgelegt; \
             arbeite innerhalb dieses Hinweises, soweit die Sandbox ihn umfasst."
        ));
    }
    Some(note)
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
/// Ein Name, den der Katalog nicht kennt, wird aus `index` geladen (Plan R9:
/// eingebettetes Bündel ohne Scaffold); ein dort deaktivierter Name wird
/// übersprungen. Ein Name, den weder Katalog noch Index kennen oder dessen
/// Verzeichnis sich in den Wurzeln nicht findet, ist ein Fehler
/// (fail-closed).
fn load_enabled_skills<'a>(
    lookup: impl Fn(&str) -> Option<&'a SkillToml>,
    roots: &[PathBuf],
    index: Option<&SkillIndex>,
    names: &[String],
) -> Result<Vec<SkillRuntimeSnapshot>, String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut snapshots = Vec::new();
    for name in names {
        if !seen.insert(name.as_str()) {
            continue;
        }
        let Some(skill) = lookup(name) else {
            match index {
                Some(index) if index.is_disabled(name) => continue,
                Some(index) => match index.get(name).filter(|entry| entry.name == *name) {
                    Some(entry) if entry.source.is_imported() => {
                        return Err(imported_skill_refusal(name));
                    }
                    Some(entry) => {
                        snapshots.push(entry.snapshot().clone());
                        continue;
                    }
                    None => return Err(format!("skill '{name}' is not configured")),
                },
                None => return Err(format!("skill '{name}' is not configured")),
            }
        };
        if !skill.enabled {
            continue;
        }
        let directory = resolve_skill_directory(roots, name)
            .map_err(|error| format!("skill '{name}': {error}"))?
            .ok_or_else(|| {
                format!("skill '{name}' has no skills/*/skill.toml in any trusted config layer")
            })?;
        if harw_catalog::is_imported_dir(&directory) {
            return Err(imported_skill_refusal(name));
        }
        let snapshot = load_skill_runtime_snapshot(&directory, skill)
            .map_err(|error| format!("skill '{name}': {error}"))?;
        snapshots.push(snapshot);
    }
    Ok(snapshots)
}

/// Die Absage für einen fest gebundenen importierten Skill (PL-97 H8):
/// sein Text ist Evidenz, nie Instruktion; er erreicht das Modell nur über den
/// getriggerten Injektor mit `[skills] inject_imported = true`.
fn imported_skill_refusal(name: &str) -> String {
    format!(
        "skill '{name}' is imported ({}); an imported skill cannot be bound as an \
         instruction, leave it to the triggered injector with [skills] inject_imported = true",
        harw_catalog::IMPORTED_MARKER_FILE
    )
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
    skill_instruction_fragments_with_index(config, skill_roots, None, skill_names)
}

/// Wie [`skill_instruction_fragments`], aber mit dem Skill-Index als Rückfall
/// für Namen, die die Konfiguration nicht führt (Plan R9: eingebettete
/// Skills ohne Scaffold).
///
/// # Errors
/// Wie [`skill_instruction_fragments`].
pub fn skill_instruction_fragments_with_index(
    config: &ResolvedConfig,
    skill_roots: &[PathBuf],
    index: Option<&SkillIndex>,
    skill_names: &[String],
) -> RuntimeResult<Vec<String>> {
    let snapshots = load_enabled_skills(
        |name| config.skills.get(name),
        skill_roots,
        index,
        skill_names,
    )
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

/// Die fest gebundenen Skill-Fragmente einer Wurzel (UIA oder `--agent`),
/// Plan R9, Teil A.
///
/// # Beschreibung
/// Vereinigt die Skills eines gleichnamigen Legacy-`agent.toml`
/// (`config.agents[legacy_agent].skills`, zuerst) mit dem `skills`-Feld der
/// Definition der Wurzel ([`ExecutableAgentIr::skills`]); Duplikate zählen
/// einmal. Früher suchte die Montage nur `config.agents[<Definitions-Id>]` —
/// ein Schlüssel, den es nie gibt (`config.agents` ist nach dem schlichten
/// Namen geschlüsselt) —, sodass die UIA-Wurzel nie ihre Skills bekam.
/// Namen, die die Konfiguration nicht führt, kommen aus `index` (vertraute
/// Layer plus eingebettetes Bündel).
///
/// # Errors
/// Wie [`skill_instruction_fragments`]: ein aktivierter, aber nicht
/// ladbarer oder gänzlich unbekannter Skill lässt die Montage scheitern
/// (fail-closed).
pub fn root_skill_fragments(
    config: &ResolvedConfig,
    skill_roots: &[PathBuf],
    index: &SkillIndex,
    root_ir: Option<&ExecutableAgentIr>,
    legacy_agent: Option<&str>,
) -> RuntimeResult<Vec<String>> {
    let legacy: &[String] = legacy_agent
        .and_then(|agent| config.agents.get(agent))
        .map(|agent| agent.skills.as_slice())
        .unwrap_or(&[]);
    let definition: &[String] = root_ir.map(ExecutableAgentIr::skills).unwrap_or(&[]);
    let names = merge_skill_names(legacy, definition);
    if names.is_empty() {
        return Ok(Vec::new());
    }
    skill_instruction_fragments_with_index(config, skill_roots, Some(index), &names)
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

/// Weak adapter that gives every child registry the same durable job submitter
/// as the root while avoiding a strong reference cycle.
struct DeferredAgentJobSubmitter {
    slot: Arc<OnceLock<Weak<dyn AgentJobSubmitter>>>,
}

impl AgentJobSubmitter for DeferredAgentJobSubmitter {
    fn submit_child<'a>(
        &'a self,
        child: &'a harw_types::SessionId,
        task: Option<&'a str>,
    ) -> AgentJobFuture<'a> {
        Box::pin(async move {
            let submitter =
                self.slot
                    .get()
                    .and_then(Weak::upgrade)
                    .ok_or_else(|| AgentSpawnError {
                        kind: Default::default(),
                        message: "durable agent job submitter is not available".to_owned(),
                    })?;
            submitter.submit_child(child, task).await
        })
    }
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
                        kind: Default::default(),
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
                kind: Default::default(),
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

    // Plan R9, Teil C/E1: Katalog, Plan-Modus-Regel und benannte Gründe
    // auch unterhalb der Wurzel (sonst gälte der Trait-Default: im Plan-Modus
    // kein Ziel, keine Beschreibungen).
    fn delegation_targets(
        &self,
        parent_session_id: &harw_types::SessionId,
        plan_mode: bool,
    ) -> Result<harw_extension_api::DelegationTargets, harw_extension_api::DelegationUnavailable>
    {
        match self.slot.get().and_then(Weak::upgrade) {
            Some(spawner) => spawner.delegation_targets(parent_session_id, plan_mode),
            None => Err(harw_extension_api::DelegationUnavailable::NoSpawnContext {
                detail: "managed child spawner is not available".to_owned(),
            }),
        }
    }

    fn note_caller_mode(&self, caller: &harw_types::SessionId, plan_mode: bool) {
        if let Some(spawner) = self.slot.get().and_then(Weak::upgrade) {
            spawner.note_caller_mode(caller, plan_mode);
        }
    }

    // Runde 9, E3: `agent.message` an ein eigenes, beendetes Kind wird auch
    // unterhalb der Wurzel zur Fortsetzung.
    fn resumable_child_role(
        &self,
        caller: &harw_types::SessionId,
        child_id: &str,
    ) -> Option<String> {
        self.slot
            .get()
            .and_then(Weak::upgrade)
            .and_then(|spawner| spawner.resumable_child_role(caller, child_id))
    }

    // Abschlussmeldungen der Hintergrund-Kinder eines Orchestrator-Kindes.
    fn pending_background_notices(
        &self,
        parent: &harw_types::SessionId,
    ) -> Vec<harw_extension_api::PendingBackgroundNotice> {
        self.slot
            .get()
            .and_then(Weak::upgrade)
            .map_or_else(Vec::new, |spawner| {
                spawner.pending_background_notices(parent)
            })
    }

    fn acknowledge_background_notices(
        &self,
        parent: &harw_types::SessionId,
        children: &[harw_types::SessionId],
    ) {
        if let Some(spawner) = self.slot.get().and_then(Weak::upgrade) {
            spawner.acknowledge_background_notices(parent, children);
        }
    }

    fn running_background_children(&self, parent: &harw_types::SessionId) -> usize {
        self.slot
            .get()
            .and_then(Weak::upgrade)
            .map_or(0, |spawner| spawner.running_background_children(parent))
    }

    fn cancel_background_children(&self, parent: &harw_types::SessionId, reason: &str) -> usize {
        self.slot
            .get()
            .and_then(Weak::upgrade)
            .map_or(0, |spawner| {
                spawner.cancel_background_children(parent, reason)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

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
            request_timeout_secs: None,
            stream_idle_timeout_secs: None,
            retry_timeouts: None,
            max_tokens_field: None,
            send_reasoning_effort: None,
            strict_tools: None,
            parallel_tool_calls: None,
            allow_insecure_lan: false,
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

    /// Live-Modellwechsel: ein nach `/model switch` gestartetes ungepinntes
    /// Kind (Orchestrator) spricht Provider und Modell der neuen Wahl an,
    /// nicht das Vorgabemodell des Starts; eine live gesetzte Rollenwahl
    /// (`/models set orchestrator …`) hat Vorrang.
    #[test]
    fn new_children_follow_the_live_model_switch_and_role_choice() -> TestResult {
        use harw_ops::live_model::LiveModelControl;
        let config = Arc::new(harw_config::ResolvedConfig::default());
        let chain = test_chain(&config);
        let live = Arc::new(crate::live_model::LiveModelRouting::new(Arc::clone(
            &config,
        )));
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            chain,
        )
        .map_err(ctx("factory builds"))?
        .with_internal_models(resolve_internal_models_for_children(&config))
        .with_effective_main_model(Some("start-model".to_owned()))
        .with_live_models(Arc::clone(&live));

        // Ohne Live-Wahl: Stand des Starts, kein Pin.
        assert_eq!(
            factory.main_model_for_task(role_names::ROOT_ORCHESTRATOR, None),
            Some("start-model".to_owned())
        );
        assert_eq!(
            factory.pinned_model_for_task(role_names::ROOT_ORCHESTRATOR, None),
            None
        );

        live.set_live_main(Some("openai".to_owned()), Some("gpt-5".to_owned()))
            .map_err(ctx("live main is not retired"))?;
        assert_eq!(
            factory.main_model_for_task(role_names::ROOT_ORCHESTRATOR, None),
            Some("gpt-5".to_owned())
        );
        assert_eq!(
            factory
                .pinned_model_for_task(role_names::ROOT_ORCHESTRATOR, None)
                .as_deref(),
            Some("gpt-5")
        );
        assert_eq!(
            factory
                .pinned_provider_for_task(role_names::ROOT_ORCHESTRATOR, None)
                .as_deref(),
            Some("openai")
        );

        assert!(live.set_internal_model(
            InternalModelPoint::RootOrchestrator,
            Some(harw_config::InternalModelChoice {
                provider: Some("anthropic".to_owned()),
                model: Some("claude-role".to_owned()),
            }),
        ));
        assert_eq!(
            factory
                .pinned_model_for_task(role_names::ROOT_ORCHESTRATOR, None)
                .as_deref(),
            Some("claude-role")
        );
        assert_eq!(
            factory
                .pinned_provider_for_task(role_names::ROOT_ORCHESTRATOR, None)
                .as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            factory.main_model_for_task(role_names::ROOT_ORCHESTRATOR, None),
            None
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
    // Runde 5, Teil B — `host.sudo_exec` nur mit TUI-Kanal und nur für die
    // Host-Shell-Worker.
    // -------------------------------------------------------------------

    /// Die Werkzeugnamen, die [`RuntimeChildRegistryFactory::with_sudo_exec`]
    /// für `role` an einen leeren Builder hängt.
    fn sudo_tools_mounted_for(factory: &RuntimeChildRegistryFactory, role: &str) -> Vec<String> {
        factory
            .with_sudo_exec(
                role,
                harw_extension_api::ExtensionRegistryBuilder::default(),
            )
            .build()
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|spec| spec.name().to_owned())
            .collect()
    }

    #[test]
    fn sudo_exec_is_mounted_only_with_a_tui_channel_and_only_for_host_shell_workers() -> TestResult
    {
        let config = harw_config::ResolvedConfig::default();
        let wiring = || {
            let (sender, _receiver) = harw_tool_shell::host_permit_prompt_channel();
            HostPermitWiring::new(
                Arc::new(harw_sandbox::ProcessPermitLedger::default()),
                Arc::new(harw_sandbox::HostPermitSessionRegistry::default()),
                sender,
            )
        };
        let factory = || {
            RuntimeChildRegistryFactory::new(
                test_project(),
                Arc::new(harw_core::EchoModelProvider::new("echo")),
                test_chain(&config),
            )
        };

        // Ohne Verdrahtung (kein Host-Pfad) und ohne sudo-Kanal (jeder
        // Nicht-TUI-Einstieg): nie ein Werkzeug.
        let bare = factory().map_err(ctx("factory builds"))?;
        assert!(sudo_tools_mounted_for(&bare, role_names::UIA_SHELL_WORKER).is_empty());
        let without_sudo = factory()
            .map_err(ctx("factory builds"))?
            .with_host_permits(harw_sandbox::SandboxProfile::Host, Some(wiring()));
        assert!(sudo_tools_mounted_for(&without_sudo, role_names::UIA_SHELL_WORKER).is_empty());

        // Mit sudo-Kanal: genau die Host-Shell-Worker.
        let (sudo, _sudo_receiver) = harw_tool_shell::sudo_prompt_channel();
        let with_sudo = factory().map_err(ctx("factory builds"))?.with_host_permits(
            harw_sandbox::SandboxProfile::Host,
            Some(wiring().with_sudo_prompts(Some(sudo))),
        );
        assert_eq!(
            sudo_tools_mounted_for(&with_sudo, role_names::UIA_SHELL_WORKER),
            vec![harw_tool_shell::SUDO_EXEC_TOOL.to_owned()]
        );
        for role in role_names::ALL {
            if *role == role_names::UIA_SHELL_WORKER {
                continue;
            }
            assert!(
                sudo_tools_mounted_for(&with_sudo, role).is_empty(),
                "{role} darf host.sudo_exec nie bekommen"
            );
        }
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

    /// Ein Config-Layer mit allen mitgelieferten Skills aus
    /// [`harw_home::bundled_files`] und die daraus entdeckte Konfiguration —
    /// dieselben Dateien, die `harw init` nach `~/.harw/skills` schreibt.
    fn bundled_skill_layer() -> TestResult<(tempfile::TempDir, harw_config::ResolvedConfig)> {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        for file in harw_home::bundled_files()
            .iter()
            .filter(|file| file.relative_path.starts_with("skills/"))
        {
            let target = file.target_in(layer.path());
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(ctx("mkdir bundled skill"))?;
            }
            std::fs::write(&target, file.contents).map_err(ctx("write bundled skill"))?;
        }
        let config = harw_config::discover_config(&[layer.path().to_path_buf()])
            .map_err(ctx("discover bundled skills"))?;
        Ok((layer, config))
    }

    /// Runde 7, Teil T5 / Plan R9: der LaTeX-Writer der UIA bekommt über
    /// seine eingebaute Definition fest nur `latex-report` aus den
    /// mitgelieferten Skill-Dateien — die übrigen drei lädt er bei Bedarf
    /// über `skills.load`, auf die der Skill verweist. `latex-report` trägt
    /// Vorlage, Werkzeuge und Übergabeformat (Registry-Kinder bekommen kein
    /// `system.md`). Dazu kommt die einzeilige Katalog-Zeile.
    #[test]
    fn uia_latex_writer_receives_only_latex_report_and_the_catalog_hint() -> TestResult {
        let (layer, config) = bundled_skill_layer()?;
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?
        .with_skill_catalog(&config, vec![layer.path().to_path_buf()])
        .map_err(ctx("skill catalog"))?;

        let fragments = factory
            .skill_fragments_for_role(role_names::UIA_LATEX_WRITER)
            .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
        assert_eq!(fragments.len(), 1, "{fragments:?}");
        assert!(
            fragments[0].starts_with("# Skill: latex-report (sha256 "),
            "{}",
            fragments[0].lines().next().unwrap_or_default()
        );
        for phrase in [
            "latex.template",
            "latex.check",
            "doc.read_pdf",
            "Overfull",
            "skills.load",
        ] {
            assert!(
                fragments[0].contains(phrase),
                "latex-report muss {phrase} nennen"
            );
        }
        let context = factory
            .skill_context_for_role(role_names::UIA_LATEX_WRITER)
            .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
        assert_eq!(context.len(), 2, "{context:?}");
        assert!(context[1].contains("skills.search") && !context[1].contains('\n'));
        Ok(())
    }

    /// Plan R9, Teil A: ohne Scaffold (kein Layer führt `latex-report`)
    /// lädt der Writer seinen festen Skill aus dem eingebetteten Bündel,
    /// statt den Start zu verweigern; die Rolle bekommt Katalog-Werkzeuge
    /// und -Zeile, eine Matrix-Sitz-Rolle keines von beiden.
    #[test]
    fn fixed_skills_fall_back_to_the_bundle_and_roles_get_the_catalog() -> TestResult {
        let empty = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let config = harw_config::ResolvedConfig::default();
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?
        .with_skill_catalog(&config, vec![empty.path().to_path_buf()])
        .map_err(ctx("skill catalog"))?;
        let fragments = factory
            .skill_fragments_for_role(role_names::UIA_LATEX_WRITER)
            .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
        assert_eq!(fragments.len(), 1, "{fragments:?}");
        assert!(fragments[0].starts_with("# Skill: latex-report (sha256 "));

        for role in [
            role_names::EXPLORER,
            role_names::EXECUTOR,
            role_names::UIA_WORKER,
            role_names::ROOT_ORCHESTRATOR,
            role_names::AGENT_STEWARD,
        ] {
            assert!(factory.skill_catalog_index_for(role).is_some(), "{role}");
            let registry = factory
                .with_skill_catalog_tools(
                    role,
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
            assert_eq!(names, ["skills.load", "skills.search"], "{role}");
            let context = factory
                .skill_context_for_role(role)
                .map_err(|error| crate::test_support::TestError::Unexpected(error.message))?;
            assert!(
                context
                    .last()
                    .is_some_and(|line| line.contains("Skills verfügbar")),
                "{role}: {context:?}"
            );
        }
        assert!(
            factory
                .skill_catalog_index_for(role_names::MATRIX_PLAYER)
                .is_none()
        );
        // Eine eigene (nicht eingebaute) Rolle bekommt den Katalog ebenfalls.
        assert!(factory.skill_catalog_index_for("eigene-rolle").is_some());
        // Drei Stellen nennen dieselben zwei Werkzeuge: die immer
        // freigeschaltete Aktivierung (harw-core), das Rollenangebot und der
        // Provider (harw-registry-defaults).
        assert_eq!(
            harw_core::activation::ALWAYS_AVAILABLE_TOOLS,
            harw_registry_defaults::profile::SKILL_CATALOG_TOOLS
        );
        assert_eq!(
            harw_registry_defaults::profile::SKILL_CATALOG_TOOLS,
            harw_registry_defaults::SkillCatalogToolProvider::TOOL_NAMES
        );
        Ok(())
    }

    /// Plan R9, Teil A (Bugfix): die Wurzel bekommt die festen Skills aus
    /// ihrer eigenen Definition (UIA-/`--agent`-IR), vereinigt mit einem
    /// gleichnamigen Legacy-`agent.toml` — auch ohne Scaffold aus dem Bündel.
    #[test]
    fn root_skill_fragments_read_the_root_definition() -> TestResult {
        let index = SkillIndex::build(&[]);
        let config = harw_config::ResolvedConfig::default();
        let uia = ir_with_skills(&["latex-report", "business-writing-pyramid"])?;
        let fragments = root_skill_fragments(&config, &[], &index, Some(&uia), None)
            .map_err(ctx("root fragments"))?;
        assert_eq!(fragments.len(), 2, "{fragments:?}");
        assert!(fragments[0].starts_with("# Skill: latex-report (sha256 "));
        assert!(fragments[1].starts_with("# Skill: business-writing-pyramid (sha256 "));
        assert!(
            root_skill_fragments(&config, &[], &index, None, None)
                .map_err(ctx("no root"))?
                .is_empty()
        );

        // Legacy-`agent.toml` zuerst, Definitions-Skills dedupliziert dahinter.
        let (layer, config) = skill_fixture("assistant")?;
        let roots = vec![layer.path().to_path_buf()];
        let index = SkillIndex::build(&roots);
        let root = ir_with_skills(&["review", "latex-report"])?;
        let merged = root_skill_fragments(&config, &roots, &index, Some(&root), Some("assistant"))
            .map_err(ctx("merged fragments"))?;
        assert_eq!(merged.len(), 2, "{merged:?}");
        assert!(merged[0].starts_with("# Skill: review (sha256 "));
        assert!(merged[1].starts_with("# Skill: latex-report (sha256 "));

        // Ein gänzlich unbekannter fester Skill bleibt fail-closed.
        let unknown = ir_with_skills(&["gibt-es-nicht"])?;
        assert!(root_skill_fragments(&config, &roots, &index, Some(&unknown), None).is_err());
        Ok(())
    }

    /// PL-97 H8: ein fest gebundener importierter Skill (Marker
    /// `imported.toml`) wird nie Instruktionsfragment — weder über den
    /// Index-Rückfall noch über die Konfiguration; die Montage scheitert
    /// fail-closed.
    #[test]
    fn bound_imported_skills_are_refused_as_instructions() -> TestResult {
        // Konfigurationsweg: `review` steht in `config.skills`.
        let (layer, config) = skill_fixture("assistant")?;
        let roots = vec![layer.path().to_path_buf()];
        std::fs::write(
            layer
                .path()
                .join("skills")
                .join("review")
                .join(harw_catalog::IMPORTED_MARKER_FILE),
            "",
        )
        .map_err(ctx("write marker"))?;
        let Err(error) = agent_skill_fragments(&config, &roots, "assistant") else {
            return Err(crate::test_support::TestError::Unexpected(
                "an imported skill must not become an instruction fragment".to_owned(),
            ));
        };
        assert!(error.to_string().contains("is imported"), "{error}");

        // Index-Rückfall: der Name fehlt in der Konfiguration.
        let fremd = layer.path().join("skills").join("fremd");
        std::fs::create_dir_all(&fremd).map_err(ctx("mkdir fremd"))?;
        std::fs::write(
            fremd.join("skill.toml"),
            "name = \"fremd\"\ndescription = \"Fremd.\"\n",
        )
        .map_err(ctx("write manifest"))?;
        std::fs::write(fremd.join("instructions.md"), "Foreign advice.\n")
            .map_err(ctx("write body"))?;
        std::fs::write(fremd.join(harw_catalog::IMPORTED_MARKER_FILE), "")
            .map_err(ctx("write marker"))?;
        let index = SkillIndex::build(&roots);
        let root = ir_with_skills(&["fremd"])?;
        let Err(error) = root_skill_fragments(&config, &roots, &index, Some(&root), None) else {
            return Err(crate::test_support::TestError::Unexpected(
                "an imported index skill must not become an instruction fragment".to_owned(),
            ));
        };
        assert!(error.to_string().contains("is imported"), "{error}");
        Ok(())
    }

    /// Runde 7, Teil T5: ohne Skill-Katalog startet der LaTeX-Writer nicht
    /// (fail-closed) — nie ein Kind ohne seine Skills.
    #[test]
    fn uia_latex_writer_without_skill_catalog_refuses_to_start() -> TestResult {
        let config = harw_config::ResolvedConfig::default();
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?;
        let Err(error) = factory.skill_fragments_for_role(role_names::UIA_LATEX_WRITER) else {
            return Err(crate::test_support::TestError::Unexpected(
                "uia-latex-writer must not start without its skills".to_owned(),
            ));
        };
        assert!(error.message.contains("latex-report"), "{}", error.message);
        Ok(())
    }

    #[test]
    fn skill_fragment_helpers_serve_root_agents_and_fail_closed() -> TestResult {
        let (layer, config) = skill_fixture("assistant")?;
        let roots = vec![layer.path().to_path_buf()];
        let fragments =
            agent_skill_fragments(&config, &roots, "assistant").map_err(ctx("agent fragments"))?;
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
        let provider = factory
            .delegate_wave_provider(factory.child_caller_identity(role_names::ROOT_ORCHESTRATOR));
        let names: Vec<String> = harw_extension_api::ToolProvider::tools(&provider)
            .iter()
            .map(|spec| spec.name().to_owned())
            .collect();
        assert_eq!(names, vec![harw_core_bridge::DELEGATE_WAVE_TOOL.to_owned()]);
        Ok(())
    }

    /// Runde 7, Teil M: der rollengebundene Provider des Game Masters trägt
    /// genau dessen Matrix-Werkzeuge; keine andere eingebaute Rolle bekommt
    /// ihn, und nur der Game Master bekommt eigene Rollenregeln.
    #[test]
    fn matrix_tools_are_offered_only_to_the_game_master() -> TestResult {
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
        )
        .map_err(ctx("factory builds"))?;
        let provider = factory.matrix_game_master_provider(
            factory.child_caller_identity(role_names::MATRIX_GAME_MASTER),
        );
        let names: Vec<String> = harw_extension_api::ToolProvider::tools(&provider)
            .iter()
            .map(|spec| spec.name().to_owned())
            .collect();
        assert_eq!(
            names,
            harw_registry_defaults::profile::MATRIX_GAME_MASTER_TOOLS
                .iter()
                .map(|tool| (*tool).to_owned())
                .collect::<Vec<_>>()
        );
        for role in role_names::ALL {
            let is_master = *role == role_names::MATRIX_GAME_MASTER;
            assert_eq!(
                !harw_registry_defaults::profile::matrix_tools_for_role(role).is_empty(),
                is_master,
                "{role}"
            );
            assert_eq!(
                harw_registry_defaults::embedded_agents::builtin_role_prompt(role).is_some(),
                is_master,
                "{role}"
            );
            if is_master {
                assert!(!role_gets_delegate_wave(role), "kein delegate_wave");
            }
        }
        Ok(())
    }

    /// Befund c15b: der `OpContext` der Matrix-Werkzeuge trägt den über
    /// [`RuntimeChildRegistryFactory::with_home_context`] gebundenen
    /// Root-Space (dieselbe Instanz); ohne Bindung fehlt er, statt über
    /// `HARW_HOME` ersetzt zu werden.
    #[test]
    fn game_master_services_carry_the_bound_home_context() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let project_dir = home.path().join("project");
        let project = harw_home::ProjectRoot {
            root: project_dir.clone(),
            trust_key: project_dir,
            kind: harw_home::ProjectKind::Directory,
        };
        let context = Arc::new(
            harw_home::ResolvedHomeContext::new(home.path(), "default".to_owned(), project)
                .map_err(ctx("home context"))?,
        );
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
        )
        .map_err(ctx("factory builds"))?;

        let identity = factory.child_caller_identity(role_names::MATRIX_GAME_MASTER);
        let unbound = game_master_services(
            &factory.spawner_slot,
            factory.delegate_wave_store.as_ref(),
            factory.home_context.as_ref(),
            &identity,
        );
        assert!(
            unbound
                .get::<Arc<harw_home::ResolvedHomeContext>>()
                .is_none(),
            "ohne with_home_context kein Root-Space im OpContext"
        );

        let factory = factory.with_home_context(Arc::clone(&context));
        let bound = game_master_services(
            &factory.spawner_slot,
            factory.delegate_wave_store.as_ref(),
            factory.home_context.as_ref(),
            &identity,
        );
        let found = bound
            .get::<Arc<harw_home::ResolvedHomeContext>>()
            .ok_or(TestError::Missing("Game-Master-ServiceMap ohne Root-Space"))?;
        assert!(
            Arc::ptr_eq(found, &context),
            "die ServiceMap muss dieselbe Instanz tragen"
        );
        Ok(())
    }

    /// Ein vertrauenswürdiger Spawn-Auftrag ohne Kontext: nur die
    /// Eltern-Korrelation, die `build_registry` liest.
    fn test_spawn_input() -> SpawnInput {
        SpawnInput {
            parent_session_id: harw_types::SessionId::new(),
            handoff_call_id: harw_types::ToolCallId::new(),
            instructions: None,
            context: serde_json::Value::Null,
            ceiling: None,
        }
    }

    /// Die Namen aller Werkzeuge einer fertigen Registry.
    fn registry_tool_names(registry: &ExtensionRegistry) -> Vec<String> {
        registry
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|spec| spec.name().to_owned())
            .collect()
    }

    // -------------------------------------------------------------------
    // PL-97 D4 — die lesenden Harw-DoD-Werkzeuge der Triage-Rollen.
    // -------------------------------------------------------------------

    /// Eine Fabrik wie im echten Lauf mit den mitgelieferten Skills: die
    /// Triage-Rollen verlangen ihren Definitions-Skill `harw-dod` und
    /// starten ohne Skill-Katalog fail-closed nicht.
    fn factory_with_bundled_skills() -> TestResult<RuntimeChildRegistryFactory> {
        let config = harw_config::ResolvedConfig::default();
        RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?
        .with_skill_catalog(&config, Vec::new())
        .map_err(ctx("skill catalog"))
    }

    /// Die sortierten Werkzeugnamen der Kind-Registry von `role`.
    fn served_tools(factory: &RuntimeChildRegistryFactory, role: &str) -> TestResult<Vec<String>> {
        let registry = factory
            .build_registry(role, &test_spawn_input(), None)
            .map_err(|error| TestError::Unexpected(format!("{role}: {}", error.message)))?;
        let mut names = registry_tool_names(&registry);
        names.sort();
        Ok(names)
    }

    /// Die sortierten sechs `dod.*` eines Triage-Kindes (PL-100 WP1.2): die
    /// drei lesenden der Wurzel plus `dod.finding`, `dod.related` und
    /// `dod.labels`.
    const TRIAGE_CHILD_DOD_TOOLS: [&str; 6] = [
        "dod.finding",
        "dod.findings",
        "dod.labels",
        "dod.related",
        "dod.sensors",
        "dod.status",
    ];

    /// PL-97 D4, PL-100 WP1.2: ein `security-egress-triage`-Kind bekommt
    /// genau die sechs `dod.*` (`DOD_TRIAGE_ROLE_TOOLS`: `dod.status`,
    /// `dod.findings`, `dod.sensors`, `dod.finding`, `dod.related`,
    /// `dod.labels`; lesend, ohne Freigabe) und den fest gebundenen Skill
    /// `harw-dod`; die drei übrigen Triage-Rollen ebenso, keine andere
    /// eingebaute Rolle bekommt den Provider.
    #[test]
    fn security_egress_triage_child_serves_exactly_the_dod_read_tools() -> TestResult {
        let factory = factory_with_bundled_skills()?;
        assert_eq!(
            served_tools(&factory, role_names::SECURITY_EGRESS_TRIAGE)?,
            TRIAGE_CHILD_DOD_TOOLS
        );
        let mut offered = harw_registry_defaults::profile::DOD_TRIAGE_ROLE_TOOLS.to_vec();
        offered.sort_unstable();
        assert_eq!(offered, TRIAGE_CHILD_DOD_TOOLS, "Angebot = Montage");

        let provider = factory
            .dod_read_provider(factory.child_caller_identity(role_names::SECURITY_EGRESS_TRIAGE));
        for tool in harw_registry_defaults::profile::DOD_TRIAGE_ROLE_TOOLS {
            assert_eq!(
                provider.approval_policy(&harw_tools::ToolName::new(*tool)),
                Some(harw_operations::ApprovalPolicy::None),
                "{tool}: lesend, ohne Freigabe"
            );
            assert!(
                harw_registry_defaults::AUTO_APPROVED_TOOLS.contains(tool),
                "{tool} ist auto-freigegeben"
            );
        }

        let fragments = factory
            .skill_fragments_for_role(role_names::SECURITY_EGRESS_TRIAGE)
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert_eq!(fragments.len(), 1, "{fragments:?}");
        assert!(
            fragments
                .first()
                .is_some_and(|fragment| fragment.starts_with("# Skill: harw-dod (sha256 ")),
            "{fragments:?}"
        );

        for role in [
            role_names::SECURITY_BASELINE_TRIAGE,
            role_names::SECURITY_STRUCTURE_TRIAGE,
            role_names::SECURITY_ENDPOINT_TRIAGE,
        ] {
            assert_eq!(
                served_tools(&factory, role)?,
                TRIAGE_CHILD_DOD_TOOLS,
                "{role}"
            );
        }
        for role in role_names::ALL {
            let triage = role.starts_with("security-") && role.ends_with("-triage");
            assert_eq!(
                !harw_registry_defaults::profile::dod_tools_for_role(role).is_empty(),
                triage,
                "{role}"
            );
        }
        Ok(())
    }

    /// PL-100 O11: ein `intel-scout`-Kind startet und bekommt genau sein
    /// rollengebundenes `deps.locked` (lesend, ohne Freigabe), ein
    /// `context-steward`-Kind gar kein Werkzeug. Keines von beiden bekommt
    /// `dod.*`, den Skill-Katalog oder einen fest gebundenen Skill.
    #[test]
    fn intel_scout_and_context_steward_children_serve_only_their_role_bound_tools() -> TestResult {
        let factory = factory_with_bundled_skills()?;
        assert_eq!(
            served_tools(&factory, role_names::INTEL_SCOUT)?,
            harw_registry_defaults::profile::LOCKFILE_READ_TOOLS
        );
        assert!(
            served_tools(&factory, role_names::CONTEXT_STEWARD)?.is_empty(),
            "context-steward ist werkzeuglos"
        );
        for tool in harw_registry_defaults::profile::LOCKFILE_READ_TOOLS {
            assert!(
                harw_registry_defaults::AUTO_APPROVED_TOOLS.contains(tool),
                "{tool} ist auto-freigegeben"
            );
        }
        for role in [role_names::INTEL_SCOUT, role_names::CONTEXT_STEWARD] {
            let fragments = factory
                .skill_fragments_for_role(role)
                .map_err(|error| TestError::Unexpected(error.message))?;
            assert!(fragments.is_empty(), "{role}: {fragments:?}");
            assert!(
                harw_registry_defaults::profile::dod_tools_for_role(role).is_empty(),
                "{role}"
            );
        }
        // Gegenprobe: kein anderes Kind bekommt den Lockfile-Provider
        // rollengebunden.
        for role in role_names::ALL {
            assert_eq!(
                !harw_registry_defaults::profile::lockfile_tools_for_role(role).is_empty(),
                *role == role_names::INTEL_SCOUT,
                "{role}"
            );
        }
        Ok(())
    }

    // -------------------------------------------------------------------
    // PL-100 WP 1.5 — Skill-Injektor für Kind-Sitzungen.
    // -------------------------------------------------------------------

    /// Ein Eltern-Grant einer Wurzel mit genau `tools` und allen Rechten.
    fn parent_grant<I, S>(tools: I) -> harw_core::ParentGrant
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        harw_core::ParentGrant {
            role: Some(harw_agent_dsl::roles::AgentRoleId::RootOrchestrator),
            tools: tools.into_iter().map(Into::into).collect(),
            permissions: harw_authority::PermissionSet::from_policy(
                harw_authority::Permission::ALL,
            ),
            max_depth: 2,
            budget_tokens: 0,
            reasoning_effort: None,
        }
    }

    /// Ein Eltern-Grant, der jedes Werkzeug des Katalogs und jedes Werkzeug
    /// der Kind-Registry von `role` hält: schneidet nichts weg.
    fn wide_parent_grant(
        factory: &RuntimeChildRegistryFactory,
        role: &str,
    ) -> TestResult<harw_core::ParentGrant> {
        let mut tools = served_tools(factory, role)?;
        tools.extend(
            harw_registry_defaults::tool_index::catalog_tool_names()
                .into_iter()
                .map(str::to_owned),
        );
        Ok(parent_grant(tools))
    }

    /// Spielt die Admission eines Kindes von `role` in der Reihenfolge des
    /// Controllers durch (Meldung, Registry-Bau mit Eltern-Grant über
    /// `build_registry_from_capability_snapshot_for_parent`) und entnimmt
    /// den Skill-Injektor, den die Fabrik für genau diese Kind-Sitzung
    /// vorgemerkt hat. Das ist die Fabrik-Seite allein: den Schnitt mit der
    /// Kind-Aktivierung macht erst `admit_inner` bei der Übergabe (geprüft in
    /// `harw-core/src/child_controller/tests/skill_injector_handover.rs`).
    fn admitted_skill_injector(
        factory: &RuntimeChildRegistryFactory,
        role: &str,
        parent: &harw_core::ParentGrant,
    ) -> TestResult<Option<Arc<harw_core::SkillInjector>>> {
        let child = harw_types::SessionId::new();
        factory.announce_child_session(role, &child);
        factory
            .build_registry_from_capability_snapshot_for_parent(
                role,
                &test_spawn_input(),
                None,
                parent,
            )
            .map_err(|error| TestError::Unexpected(format!("{role}: {}", error.message)))?;
        Ok(factory
            .take_child_skill_injector(role, &child)
            .map(|injection| injection.injector))
    }

    /// Prüft, dass `skill` im Index des Injektors steht (sonst wäre
    /// `tools_admit` für einen unbekannten Namen trivial `true`).
    fn assert_indexed(injector: &harw_core::SkillInjector, skill: &str) -> TestResult {
        injector
            .index()
            .get(skill)
            .map(|_| ())
            .ok_or_else(|| TestError::Unexpected(format!("bundled skill {skill} not indexed")))
    }

    /// PL-100 WP 1.5: ein Kind mit Skill-Katalog bekommt einen Injektor,
    /// dessen Werkzeug-Filter genau seine aufrufbaren Werkzeuge kennt: ein
    /// `explorer`-Kind (lesende `fsread.*`/`git.*`, kein `sys.*`, `dod.*`
    /// oder `cargo.*`) bekommt nie einen Skill für Werkzeuge, die es nicht
    /// hat. Ein Skill mit dem Label `filesystem.read` (`planning`) bleibt
    /// wählbar, weil das Kind lesen kann; einer, der zusätzlich
    /// `process.execute` nennt (`verification`), nicht.
    #[test]
    fn child_skill_injector_admits_only_skills_for_tools_the_child_can_call() -> TestResult {
        let factory = factory_with_bundled_skills()?;
        let parent = wide_parent_grant(&factory, role_names::EXPLORER)?;
        let injector = admitted_skill_injector(&factory, role_names::EXPLORER, &parent)?
            .ok_or(TestError::Missing("explorer child skill injector"))?;
        for skill in ["fsread-tools", "git-read-tools", "planning"] {
            assert_indexed(&injector, skill)?;
            assert!(injector.tools_admit(skill), "{skill}");
        }
        for skill in ["sys-tools", "harw-dod", "cargo-tools", "verification"] {
            assert_indexed(&injector, skill)?;
            assert!(!injector.tools_admit(skill), "{skill}");
        }
        Ok(())
    }

    /// PL-100 WP 1.5 (Review-Befund, Runde 2): der Werkzeug-Filter folgt
    /// den Rechten des Elternteils, nicht seiner Registry. Hält der
    /// Elternteil kein `fsread.*` in `parent.tools`, bekommt das
    /// `explorer`-Kind `fsread-tools` trotzdem (der Kern schneidet nur mit
    /// der Eltern-Aktivierung); fehlt dem Elternteil `ReadWorkspace`, fallen
    /// alle lesenden Workspace-Werkzeuge und das Label `filesystem.read`
    /// weg; ohne Rechte bleibt kein werkzeug- oder rechtegebundener Skill.
    #[test]
    fn child_skill_injector_follows_the_parent_permission_cut() -> TestResult {
        let factory = factory_with_bundled_skills()?;
        let served = served_tools(&factory, role_names::EXPLORER)?;
        assert!(served.iter().any(|tool| tool.starts_with("fsread.")));
        assert!(served.iter().any(|tool| tool.starts_with("git.")));

        let without_fsread = parent_grant(
            served
                .iter()
                .filter(|tool| !tool.starts_with("fsread."))
                .cloned(),
        );
        let injector = admitted_skill_injector(&factory, role_names::EXPLORER, &without_fsread)?
            .ok_or(TestError::Missing("explorer injector without fsread"))?;
        assert_indexed(&injector, "fsread-tools")?;
        assert!(injector.tools_admit("fsread-tools"));
        assert!(injector.tools_admit("git-read-tools"));

        let mut network_only = wide_parent_grant(&factory, role_names::EXPLORER)?;
        network_only.permissions =
            harw_authority::PermissionSet::from_policy([harw_authority::Permission::NetworkAccess]);
        let injector = admitted_skill_injector(&factory, role_names::EXPLORER, &network_only)?
            .ok_or(TestError::Missing(
                "explorer injector without ReadWorkspace",
            ))?;
        assert_indexed(&injector, "planning")?;
        for skill in ["fsread-tools", "git-read-tools", "planning"] {
            assert!(!injector.tools_admit(skill), "{skill}");
        }

        let mut empty = parent_grant(Vec::<String>::new());
        empty.permissions = harw_authority::PermissionSet::empty();
        let injector = admitted_skill_injector(&factory, role_names::EXPLORER, &empty)?.ok_or(
            TestError::Missing("explorer injector under a parent without rights"),
        )?;
        for skill in ["fsread-tools", "git-read-tools", "sys-tools", "planning"] {
            assert!(!injector.tools_admit(skill), "{skill}");
        }
        Ok(())
    }

    /// PL-100 WP 1.5: der Schnitt als reine Funktion — Definition und
    /// Rechte des Elternteils; Werkzeuge ohne Sandbox-Recht bleiben. Weder
    /// ein Reducer noch die Eltern-Registry schneiden (Review-Befund).
    #[test]
    fn child_callable_tools_applies_the_surface_and_the_parent_rights() {
        let registered: Vec<String> = [
            "fsread.ls",
            "web.fetch",
            "dod.status",
            "sys.ps",
            "fsread.ls",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let all = harw_authority::PermissionSet::from_policy(harw_authority::Permission::ALL);
        assert_eq!(
            child_callable_tools(&registered, None, &all),
            ["dod.status", "fsread.ls", "sys.ps", "web.fetch"]
        );
        let read_only =
            harw_authority::PermissionSet::from_policy([harw_authority::Permission::ReadWorkspace]);
        assert_eq!(
            child_callable_tools(&registered, None, &read_only),
            ["dod.status", "fsread.ls"]
        );
        assert_eq!(
            child_callable_tools(&registered, None, &harw_authority::PermissionSet::empty()),
            ["dod.status"]
        );
    }

    /// PL-100 WP 1.5: Werkzeug-Erstnutzung wirkt im Kind wie in der Wurzel
    /// — der erste `fsread.ls`-Aufruf eines `explorer`-Kindes spielt
    /// `fsread-tools` ein; `ps aux` über eine Shell trifft `sys-tools`, das
    /// das Kind nicht aufrufen kann, und bleibt draußen.
    #[test]
    fn child_skill_injector_injects_on_tool_first_use_and_skips_unavailable_skills() -> TestResult {
        let factory = factory_with_bundled_skills()?;
        let parent = wide_parent_grant(&factory, role_names::EXPLORER)?;
        let injector = admitted_skill_injector(&factory, role_names::EXPLORER, &parent)?
            .ok_or(TestError::Missing("explorer child skill injector"))?;
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("go");
        assert!(injector.prepare_round(&history, 0).delivered.is_empty());
        history.push_tool_call(
            harw_types::ToolCallId::from_str("c1"),
            "fsread.ls",
            serde_json::json!({ "path": "." }),
        );
        let round = injector.prepare_round(&history, 0);
        assert!(
            round.delivered.iter().any(|delivered| {
                delivered.skill == "fsread-tools"
                    && delivered.kind == harw_catalog::TriggerKind::Tool
            }),
            "{:?}",
            round.delivered
        );

        let other = admitted_skill_injector(&factory, role_names::EXPLORER, &parent)?
            .ok_or(TestError::Missing("second explorer child skill injector"))?;
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("go");
        history.push_tool_call(
            harw_types::ToolCallId::from_str("c2"),
            "shell.exec",
            serde_json::json!({ "command": "ps aux" }),
        );
        let round = other.prepare_round(&history, 0);
        assert!(
            !round
                .delivered
                .iter()
                .any(|delivered| delivered.skill == "sys-tools"),
            "{:?}",
            round.delivered
        );
        assert!(
            round.unavailable.iter().any(|skill| skill == "sys-tools"),
            "{:?}",
            round.unavailable
        );
        Ok(())
    }

    /// PL-100 WP 1.5 (Review-Befund): fest gebundene Skills einer Rolle
    /// stehen schon vollständig in ihren Instruktionen; ihr Injektor spielt
    /// sie nie ein zweites Mal ein, ein anderer Skill mit demselben Trigger
    /// kommt weiter durch.
    #[test]
    fn fixed_role_skills_are_never_injected_again() -> TestResult {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut config = harw_config::ResolvedConfig::default();
        for name in ["review", "audit"] {
            let dir = layer.path().join("skills").join(name);
            std::fs::create_dir_all(&dir).map_err(ctx("mkdir skill"))?;
            std::fs::write(
                dir.join("skill.toml"),
                format!("name = \"{name}\"\ndescription = \"{name} skill\"\n"),
            )
            .map_err(ctx("write manifest"))?;
            std::fs::write(
                dir.join("instructions.md"),
                format!("# {name}\n\nCheck the ledger.\n"),
            )
            .map_err(ctx("write body"))?;
            std::fs::write(
                dir.join("triggers.toml"),
                "[triggers]\nkeywords = [\"ledger audit\"]\npriority = 50\n",
            )
            .map_err(ctx("write triggers"))?;
        }
        config.skills.insert(
            "review".to_owned(),
            SkillToml {
                name: "review".to_owned(),
                enabled: true,
                description: "review skill".to_owned(),
                instructions_file: None,
                tools: Vec::new(),
                mcps: Vec::new(),
            },
        );
        config.agents.insert(
            role_names::EXPLORER.to_owned(),
            harw_config::AgentToml {
                name: role_names::EXPLORER.to_owned(),
                role: "worker".to_owned(),
                description: String::new(),
                system_file: None,
                providers: Vec::new(),
                models: Vec::new(),
                skills: vec!["review".to_owned()],
                suggestions: harw_config::AgentSuggestionsToml::default(),
                primary_provider: None,
                secondary_providers: Vec::new(),
                timeout_seconds: 120,
                max_retries: 2,
            },
        );
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?
        .with_skill_catalog(&config, vec![layer.path().to_path_buf()])
        .map_err(ctx("skill catalog"))?;
        assert_eq!(factory.fixed_skill_names(role_names::EXPLORER), ["review"]);

        let parent = wide_parent_grant(&factory, role_names::EXPLORER)?;
        let injector = admitted_skill_injector(&factory, role_names::EXPLORER, &parent)?
            .ok_or(TestError::Missing("explorer child skill injector"))?;
        assert_indexed(&injector, "review")?;
        assert_indexed(&injector, "audit")?;
        assert!(injector.excludes("review"));
        assert!(!injector.excludes("audit"));

        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("please run the ledger audit now");
        let round = injector.prepare_round(&history, 0);
        assert!(
            round
                .delivered
                .iter()
                .any(|delivered| delivered.skill == "audit"),
            "{:?}",
            round.delivered
        );
        assert!(
            !round
                .delivered
                .iter()
                .any(|delivered| delivered.skill == "review"),
            "{:?}",
            round.delivered
        );
        Ok(())
    }

    /// PL-100 WP 1.5: wie an der Wurzel nur zusammen mit dem Skill-Katalog
    /// — die Triage-Rollen, `intel-scout`, `context-steward` und die
    /// Matrix-Sitze bekommen keinen Injektor; der fest gebundene Skill
    /// `harw-dod` der Triage-Rollen bleibt ihr einziger Skill-Kontext.
    #[test]
    fn roles_without_the_skill_catalog_get_no_child_skill_injector() -> TestResult {
        let factory = factory_with_bundled_skills()?;
        for role in [
            role_names::SECURITY_EGRESS_TRIAGE,
            role_names::SECURITY_BASELINE_TRIAGE,
            role_names::SECURITY_STRUCTURE_TRIAGE,
            role_names::SECURITY_ENDPOINT_TRIAGE,
            role_names::INTEL_SCOUT,
            role_names::CONTEXT_STEWARD,
            role_names::MATRIX_PLAYER,
        ] {
            assert!(
                harw_registry_defaults::profile::SKILL_CATALOG_EXCLUDED_ROLES.contains(&role),
                "{role}"
            );
            let parent = wide_parent_grant(&factory, role)?;
            assert!(
                admitted_skill_injector(&factory, role, &parent)?.is_none(),
                "{role} must not get a skill injector"
            );
        }
        let fragments = factory
            .skill_fragments_for_role(role_names::SECURITY_EGRESS_TRIAGE)
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert_eq!(fragments.len(), 1, "{fragments:?}");
        assert!(
            fragments
                .first()
                .is_some_and(|fragment| fragment.starts_with("# Skill: harw-dod (sha256 ")),
            "{fragments:?}"
        );
        // Gegenprobe: eine Rolle mit Skill-Katalog bekommt einen.
        let parent = wide_parent_grant(&factory, role_names::EXPLORER)?;
        assert!(admitted_skill_injector(&factory, role_names::EXPLORER, &parent)?.is_some());
        Ok(())
    }

    /// PL-100 WP 1.5: ohne Skill-Katalog oder mit
    /// `[skills] triggered_injection = false` bekommt kein Kind einen
    /// Injektor; die Katalog-Werkzeuge bleiben davon unberührt.
    #[test]
    fn no_child_skill_injector_without_catalog_or_triggered_injection() -> TestResult {
        let config = harw_config::ResolvedConfig::default();
        let without_catalog = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
        )
        .map_err(ctx("factory builds"))?;
        let parent = wide_parent_grant(&without_catalog, role_names::EXPLORER)?;
        assert!(
            admitted_skill_injector(&without_catalog, role_names::EXPLORER, &parent)?.is_none()
        );

        let mut off = harw_config::ResolvedConfig::default();
        off.harness.skills.triggered_injection = false;
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&off),
        )
        .map_err(ctx("factory builds"))?
        .with_skill_catalog(&off, Vec::new())
        .map_err(ctx("skill catalog"))?;
        assert!(admitted_skill_injector(&factory, role_names::EXPLORER, &parent)?.is_none());
        let tools = served_tools(&factory, role_names::EXPLORER)?;
        for tool in harw_registry_defaults::profile::SKILL_CATALOG_TOOLS {
            assert!(tools.iter().any(|served| served == tool), "{tool}");
        }
        Ok(())
    }

    /// PL-100 WP 1.5: der Injektor gehört genau der Kind-Sitzung, für die die
    /// Registry gebaut wurde — jedes Kind bekommt einen eigenen; ein Bau ohne
    /// Meldung oder ohne Eltern-Grant (`build_registry`, fail-closed) merkt
    /// keinen vor; ein Rest für eine andere Sitzung wird verworfen statt
    /// übergeben.
    #[test]
    fn child_skill_injector_belongs_to_exactly_one_child_session() -> TestResult {
        let factory = factory_with_bundled_skills()?;
        let parent = wide_parent_grant(&factory, role_names::EXPLORER)?;
        let first = admitted_skill_injector(&factory, role_names::EXPLORER, &parent)?
            .ok_or(TestError::Missing("first injector"))?;
        let second = admitted_skill_injector(&factory, role_names::EXPLORER, &parent)?
            .ok_or(TestError::Missing("second injector"))?;
        assert!(!Arc::ptr_eq(&first, &second), "a fresh injector per child");

        // Bau mit Eltern-Grant, aber ohne Meldung (keine Admission).
        factory
            .build_registry_from_capability_snapshot_for_parent(
                role_names::EXPLORER,
                &test_spawn_input(),
                None,
                &parent,
            )
            .map_err(|error| TestError::Unexpected(error.message))?;
        let unannounced = harw_types::SessionId::new();
        assert!(
            factory
                .take_child_skill_injector(role_names::EXPLORER, &unannounced)
                .is_none()
        );

        // Gemeldet, aber ohne Eltern-Grant gebaut: der Werkzeug-Filter ist
        // nicht prüfbar, also kein Injektor.
        let without_grant = harw_types::SessionId::new();
        factory.announce_child_session(role_names::EXPLORER, &without_grant);
        factory
            .build_registry(role_names::EXPLORER, &test_spawn_input(), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert!(
            factory
                .take_child_skill_injector(role_names::EXPLORER, &without_grant)
                .is_none()
        );

        // Gebaut für `built`, abgeholt für `other`: verworfen, auch für
        // einen späteren Abruf von `built`.
        let built = harw_types::SessionId::new();
        let other = harw_types::SessionId::new();
        factory.announce_child_session(role_names::EXPLORER, &built);
        factory
            .build_registry_from_capability_snapshot_for_parent(
                role_names::EXPLORER,
                &test_spawn_input(),
                None,
                &parent,
            )
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert!(
            factory
                .take_child_skill_injector(role_names::EXPLORER, &other)
                .is_none()
        );
        assert!(
            factory
                .take_child_skill_injector(role_names::EXPLORER, &built)
                .is_none()
        );
        Ok(())
    }

    /// PL-100 WP 1.5: `child_session_observers` entnimmt den vorgemerkten
    /// Injektor der Sitzung immer (auch ohne Diary-Recorder) und übergibt
    /// ihn dem Kern in `ChildSessionObservers::skill_injector`; danach bleibt
    /// nichts für einen späteren Bau hängen.
    #[test]
    fn child_session_observers_hand_over_the_pending_skill_injector() -> TestResult {
        let factory = factory_with_bundled_skills()?;
        let parent = wide_parent_grant(&factory, role_names::EXPLORER)?;
        let child = harw_types::SessionId::new();
        factory.announce_child_session(role_names::EXPLORER, &child);
        factory
            .build_registry_from_capability_snapshot_for_parent(
                role_names::EXPLORER,
                &test_spawn_input(),
                None,
                &parent,
            )
            .map_err(|error| TestError::Unexpected(error.message))?;
        let observers = factory.child_session_observers(role_names::EXPLORER, &child);
        assert!(observers.compaction.is_none() && observers.tool_outcome.is_none());
        assert!(
            observers.skill_injector.is_some(),
            "the pending injector is handed over, not dropped"
        );
        // Mit dem Injektor geht die Deckung seiner Labels: `filesystem.read`
        // (etwa aus `planning`) ist bei einem `explorer` von seinen lesenden
        // `fsread.*` gedeckt, ein Schreib-Label gar nicht aufrufbar.
        let read_backing = observers
            .skill_label_backing
            .get("filesystem.read")
            .ok_or(TestError::Missing("backing of filesystem.read"))?;
        assert!(
            read_backing.iter().any(|tool| tool.starts_with("fsread.")),
            "{read_backing:?}"
        );
        assert!(
            !observers
                .skill_label_backing
                .contains_key("filesystem.write")
        );
        assert!(
            factory
                .take_child_skill_injector(role_names::EXPLORER, &child)
                .is_none(),
            "the observers call consumed the pending injector"
        );
        // Ein zweiter Abruf für dieselbe Sitzung bekommt keinen Injektor mehr.
        assert!(
            factory
                .child_session_observers(role_names::EXPLORER, &child)
                .skill_injector
                .is_none()
        );
        Ok(())
    }

    /// PL-100 WP 1.5 (Review-Befund): auch die Steward-Sonderregistry
    /// (`build_registry_from_capability_snapshot_for_parent` für
    /// `agent-steward`) merkt einen Injektor für die gemeldete Sitzung vor,
    /// geschnitten mit dem Eltern-Grant wie jedes andere Kind.
    #[test]
    fn agent_steward_child_gets_a_skill_injector_through_its_own_registry() -> TestResult {
        let factory = factory_with_bundled_skills()?;
        let parent = parent_grant(harw_registry_defaults::tool_index::catalog_tool_names());
        let injector = admitted_skill_injector(&factory, role_names::AGENT_STEWARD, &parent)?
            .ok_or(TestError::Missing("agent-steward child skill injector"))?;
        for skill in ["sys-tools", "cargo-tools", "harw-dod"] {
            assert_indexed(&injector, skill)?;
            assert!(!injector.tools_admit(skill), "{skill}");
        }
        assert!(injector.tools_admit("planning"));

        // Ohne Meldung merkt auch der Steward-Pfad nichts vor.
        factory
            .build_registry_from_capability_snapshot_for_parent(
                role_names::AGENT_STEWARD,
                &test_spawn_input(),
                None,
                &parent,
            )
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert!(
            factory
                .take_child_skill_injector(role_names::AGENT_STEWARD, &harw_types::SessionId::new())
                .is_none()
        );
        Ok(())
    }

    /// Eine Fabrik über dem Roster eines Home-Layers unter `dir/home`: die
    /// mitgelieferten Agenten `bundled_agents` (Dateien aus `harw-home`)
    /// und die Definitionen `extra` (`(Ordner, definition.toml)`) liegen
    /// dort; der Skill-Katalog liest zusätzlich die Skills `skills`
    /// (`(Name, skill.toml, triggers.toml)`) aus dem Layer `dir/skills`.
    fn factory_with_home_agents(
        dir: &std::path::Path,
        bundled_agents: &[&str],
        extra: &[(&str, &str)],
        skills: &[(&str, &str, &str)],
    ) -> TestResult<RuntimeChildRegistryFactory> {
        let home = dir.join("home");
        for agent in bundled_agents {
            let prefix = format!("agents/{agent}/");
            let mut found = false;
            for file in harw_home::bundled_files() {
                let Some(relative) = file.relative_path.strip_prefix(prefix.as_str()) else {
                    continue;
                };
                let path = home.join("agents").join(agent).join(relative);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(ctx("bundled agent dir"))?;
                }
                std::fs::write(&path, file.contents).map_err(ctx("bundled agent file"))?;
                found = true;
            }
            if !found {
                return Err(TestError::Unexpected(format!(
                    "bundled agent {agent} not found"
                )));
            }
        }
        for (name, definition) in extra {
            let path = home.join("agents").join(name);
            std::fs::create_dir_all(&path).map_err(ctx("agent dir"))?;
            std::fs::write(path.join("definition.toml"), definition)
                .map_err(ctx("agent definition"))?;
        }
        let layer = dir.join("skills");
        for (name, manifest, triggers) in skills {
            let path = layer.join("skills").join(name);
            std::fs::create_dir_all(&path).map_err(ctx("skill dir"))?;
            std::fs::write(path.join("skill.toml"), manifest).map_err(ctx("skill manifest"))?;
            std::fs::write(
                path.join("instructions.md"),
                format!("# {name}\n\nFollow the {name} steps.\n"),
            )
            .map_err(ctx("skill body"))?;
            std::fs::write(path.join("triggers.toml"), triggers).map_err(ctx("skill triggers"))?;
        }
        std::fs::create_dir_all(&home).map_err(ctx("home dir"))?;
        let config = harw_config::discover_config(&[home]).map_err(ctx("Discovery"))?;
        let agents = harw_registry_defaults::ConfigAgents::from_config(&config)
            .map_err(ctx("Agenten senken"))?;
        let builtin = builtin_agent_definitions(&HashMap::new()).map_err(ctx("Rollen"))?;
        let roster = harw_registry_defaults::AgentRoster::from_config(&builtin, &agents)
            .map_err(ctx("Roster"))?;
        RuntimeChildRegistryFactory::with_definitions(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&config),
            roster.definitions().clone(),
        )
        .with_custom_agents(roster.custom_wiring())
        .with_skill_catalog(&config, vec![layer])
        .map_err(ctx("skill catalog"))
    }

    /// PL-100 WP 1.5 (Review-Befund, Runde 2): der mitgelieferte
    /// `test-engineer` hält `cargo.test_one`, obwohl der Reducer seiner
    /// Basisrolle (`analyst`) kein `ExecuteProcess` trägt. Sein Injektor
    /// lässt `cargo-tools` zu, auch wenn der Elternteil `cargo.test_one`
    /// nicht registriert (Wurzel ohne IR, eigener Orchestrator), und spielt
    /// es beim ersten `cargo.test_one`-Aufruf ein, statt es als nicht
    /// verfügbar zu melden. Sein fest gebundener Skill `verification` bleibt
    /// vom Injektor ausgenommen.
    #[test]
    fn test_engineer_child_gets_cargo_tools_for_its_own_cargo_test_one() -> TestResult {
        const ENGINEER: &str = "test-engineer";
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let factory = factory_with_home_agents(dir.path(), &[ENGINEER], &[], &[])?;
        let base = factory.lookup_role(ENGINEER);
        assert_eq!(base, role_names::ANALYST);
        let reducer = harw_registry_defaults::authority_reducer_for_role(base)
            .ok_or(TestError::Missing("reducer of the analyst base"))?;
        let all = harw_authority::PermissionSet::from_policy(harw_authority::Permission::ALL);
        assert!(
            !reducer
                .reduce(&all)
                .contains(harw_authority::Permission::ExecuteProcess),
            "the base reducer is not where the engineer gets its rights"
        );

        let served = served_tools(&factory, ENGINEER)?;
        assert!(
            served.iter().any(|tool| tool == "cargo.test_one"),
            "{served:?}"
        );
        let parent = parent_grant(
            served
                .iter()
                .filter(|tool| tool.as_str() != "cargo.test_one")
                .cloned(),
        );
        let injector = admitted_skill_injector(&factory, ENGINEER, &parent)?
            .ok_or(TestError::Missing("test-engineer child skill injector"))?;
        assert_indexed(&injector, "cargo-tools")?;
        assert!(injector.tools_admit("cargo-tools"));
        assert!(injector.excludes("verification"));

        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("go");
        let _anchor = injector.prepare_round(&history, 0);
        history.push_tool_call(
            harw_types::ToolCallId::from_str("c1"),
            "cargo.test_one",
            serde_json::json!({ "test": "parses_route" }),
        );
        let round = injector.prepare_round(&history, 0);
        assert!(
            !round.unavailable.iter().any(|skill| skill == "cargo-tools"),
            "{:?}",
            round.unavailable
        );
        assert!(
            round.delivered.iter().any(|delivered| {
                delivered.skill == "cargo-tools"
                    && delivered.kind == harw_catalog::TriggerKind::Tool
            }),
            "{:?}",
            round.delivered
        );
        Ok(())
    }

    /// PL-100 WP 1.5 (Review-Befund, Runde 2): ein Skill, der nur abstrakte
    /// Fähigkeits-Labels nennt, erreicht ein Kind nur, wenn es die Rechte
    /// dazu über ein aufrufbares Werkzeug hat. `filesystem.write` gilt
    /// all-of: ein lesendes `explorer`-Kind bekommt `scribble`
    /// (`filesystem.read` und `filesystem.write`) nicht, auch nicht über das
    /// Lese-Label, und meldet den Treffer als nicht verfügbar; der
    /// schreibende `test-engineer` bekommt ihn. `reader` (nur
    /// `filesystem.read`) erreicht beide.
    #[test]
    fn abstract_write_labels_reach_only_children_that_can_write() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let factory = factory_with_home_agents(
            dir.path(),
            &["test-engineer"],
            &[],
            &[
                (
                    "scribble",
                    "name = \"scribble\"\ndescription = \"Scribble notes into the workspace.\"\n\
                     tools = [\"filesystem.read\", \"filesystem.write\"]\n",
                    "[triggers]\nkeywords = [\"scribble notes\"]\npriority = 50\n",
                ),
                (
                    "reader",
                    "name = \"reader\"\ndescription = \"Read the notes.\"\n\
                     tools = [\"filesystem.read\"]\n",
                    "[triggers]\nkeywords = [\"read the notes\"]\npriority = 40\n",
                ),
            ],
        )?;

        let parent = wide_parent_grant(&factory, role_names::EXPLORER)?;
        let explorer = admitted_skill_injector(&factory, role_names::EXPLORER, &parent)?
            .ok_or(TestError::Missing("explorer child skill injector"))?;
        assert_indexed(&explorer, "scribble")?;
        assert_indexed(&explorer, "reader")?;
        assert!(!explorer.tools_admit("scribble"));
        assert!(explorer.tools_admit("reader"));
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("please scribble notes for me");
        let round = explorer.prepare_round(&history, 0);
        assert!(
            !round
                .delivered
                .iter()
                .any(|delivered| delivered.skill == "scribble"),
            "{:?}",
            round.delivered
        );
        assert!(
            round.unavailable.iter().any(|skill| skill == "scribble"),
            "{:?}",
            round.unavailable
        );

        let parent = wide_parent_grant(&factory, "test-engineer")?;
        let engineer = admitted_skill_injector(&factory, "test-engineer", &parent)?
            .ok_or(TestError::Missing("test-engineer child skill injector"))?;
        assert!(engineer.tools_admit("scribble"));
        assert!(engineer.tools_admit("reader"));
        Ok(())
    }

    /// PL-100 WP 1.5 (Review-Befund, Runde 2): die Zuordnung der Labels als
    /// reine Funktion — nur Labels mit Rechtebezug, die kein Werkzeugname
    /// sind; aufrufbar über ein Werkzeug mit demselben Recht; Schreib- und
    /// Ausführungsrechte all-of.
    #[test]
    fn child_capability_labels_map_labels_onto_callable_tools() {
        let index = SkillIndex::build_with_bundle(
            &[],
            &[
                (
                    "skills/mixed/skill.toml",
                    "name = \"mixed\"\ndescription = \"Mixed labels.\"\n\
                     tools = [\"filesystem.read\", \"filesystem.write\", \"fs.read\", \
                     \"process.execute\", \"security.advisory.correlate\"]\n",
                ),
                ("skills/mixed/instructions.md", "# Mixed\n\nBody.\n"),
            ],
        );
        let tools: std::collections::BTreeSet<String> = ["fs.read", "fs.write", "shell.exec"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let labels = child_capability_labels(&index, &tools, &["fs.read".to_owned()]);
        assert_eq!(
            labels.known,
            ["filesystem.read", "filesystem.write", "process.execute"]
        );
        assert_eq!(labels.callable, ["filesystem.read"]);
        assert_eq!(labels.required, ["filesystem.write", "process.execute"]);
        assert_eq!(
            labels.backing,
            backing(&[("filesystem.read", &["fs.read"])])
        );

        let writer = child_capability_labels(
            &index,
            &tools,
            &[
                "fs.write".to_owned(),
                "shell.exec".to_owned(),
                "cargo.test_one".to_owned(),
            ],
        );
        assert_eq!(writer.callable, ["filesystem.write", "process.execute"]);
        // Review-Befund: die Deckung nennt jedes aufrufbare Werkzeug mit dem
        // Recht des Labels, damit der Kern das Label erst wegnimmt, wenn
        // die Eltern-Aktivierung alle sperrt.
        assert_eq!(
            writer.backing,
            backing(&[
                ("filesystem.write", &["fs.write"]),
                ("process.execute", &["cargo.test_one", "shell.exec"]),
            ])
        );

        let none = child_capability_labels(&index, &tools, &[]);
        assert!(none.callable.is_empty() && none.backing.is_empty());
    }

    /// Eine Label-Deckung aus `(Label, Werkzeuge)`.
    fn backing(entries: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        entries
            .iter()
            .map(|(label, tools)| {
                (
                    (*label).to_owned(),
                    tools.iter().map(|tool| (*tool).to_owned()).collect(),
                )
            })
            .collect()
    }

    /// PL-100 WP 1.5 (Review-Befund, Runde 2): ein benutzerdefinierter
    /// Agent über einer Rolle ohne Skill-Katalog (hier
    /// `security-egress-triage`) bekommt wie seine Basis weder
    /// `skills.search`/`skills.load` noch einen Skill-Injektor; Sensortext
    /// kann ihm keinen Skill zuspielen. Gegenprobe: ein Agent über
    /// `explorer` bekommt beides.
    #[test]
    fn a_custom_agent_over_an_excluded_role_gets_no_skill_catalog_or_injector() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let factory = factory_with_home_agents(
            dir.path(),
            &[],
            &[
                (
                    "egress-helper",
                    r#"schema = "harwness.agent/v1"
id = "user.agent.egress-helper@1"
version = "1.0.0"
extends = { id = "harwness.agent.security-egress-triage@1" }
role = "worker"
specialization = "egress-helper"
"#,
                ),
                (
                    "web-scout",
                    r#"schema = "harwness.agent/v1"
id = "user.agent.web-scout@1"
version = "1.0.0"
extends = { id = "harwness.agent.explorer@1" }
role = "worker"
specialization = "web-scout"
"#,
                ),
            ],
            &[],
        )?;
        assert_eq!(
            factory.lookup_role("egress-helper"),
            role_names::SECURITY_EGRESS_TRIAGE
        );
        assert_eq!(factory.lookup_role("web-scout"), role_names::EXPLORER);
        let parent = parent_grant(harw_registry_defaults::tool_index::catalog_tool_names());
        let empty = ExtensionRegistry::builder().build();

        assert!(factory.skill_catalog_index_for("egress-helper").is_none());
        assert!(
            factory
                .child_skill_injector("egress-helper", &empty, &parent)
                .is_none()
        );
        let catalog_tools = factory
            .with_skill_catalog_tools(
                "egress-helper",
                harw_extension_api::ExtensionRegistryBuilder::default(),
            )
            .build();
        assert!(registry_tool_names(&catalog_tools).is_empty());

        assert!(factory.skill_catalog_index_for("web-scout").is_some());
        assert!(
            factory
                .child_skill_injector("web-scout", &empty, &parent)
                .is_some()
        );
        Ok(())
    }

    /// PL-100 WP 1.5: ein benutzerdefinierter Agent über einer Rolle mit
    /// Skill-Katalog (`web-scout` über `explorer`) bekommt seinen Injektor
    /// auf demselben Weg wie eine eingebaute Rolle: die Admission (Meldung,
    /// Registry-Bau mit Eltern-Grant) merkt ihn für genau diese Sitzung vor,
    /// und `child_session_observers` übergibt ihn dem Kern.
    #[test]
    fn a_custom_agent_child_hands_over_its_skill_injector() -> TestResult {
        const SCOUT: &str = "web-scout";
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let factory = factory_with_home_agents(
            dir.path(),
            &[],
            &[(
                SCOUT,
                r#"schema = "harwness.agent/v1"
id = "user.agent.web-scout@1"
version = "1.0.0"
extends = { id = "harwness.agent.explorer@1" }
role = "worker"
specialization = "web-scout"
"#,
            )],
            &[],
        )?;
        assert_eq!(factory.lookup_role(SCOUT), role_names::EXPLORER);
        let parent = wide_parent_grant(&factory, SCOUT)?;
        let child = harw_types::SessionId::new();
        factory.announce_child_session(SCOUT, &child);
        factory
            .build_registry_from_capability_snapshot_for_parent(
                SCOUT,
                &test_spawn_input(),
                None,
                &parent,
            )
            .map_err(|error| TestError::Unexpected(format!("{SCOUT}: {}", error.message)))?;
        let observers = factory.child_session_observers(SCOUT, &child);
        assert!(
            observers.skill_injector.is_some(),
            "the custom agent child receives its injector"
        );
        Ok(())
    }

    /// Lässt ein `explorer`-Kind über den echten Admissionsweg zu: die
    /// Laufzeit-Fabrik mit den mitgelieferten Skills als Rolle eines
    /// [`ManagedAgentSpawner`], eine externe Wurzel (`root-orchestrator`,
    /// lokale Wurzeldecke, Sandbox mit `ReadWorkspace`), deren Aktivierung
    /// genau `disabled` sperrt, und das öffentliche
    /// [`AgentSpawner::spawn_child`]. Liefert den Skill-Injektor, den die
    /// Kind-Sitzung danach trägt.
    async fn spawned_explorer_injector(
        disabled: &[&str],
    ) -> TestResult<Arc<harw_core::SkillInjector>> {
        use harw_agent_dsl::roles::AgentRoleId;

        let factory = Arc::new(factory_with_bundled_skills()?);
        let organizational_role = factory
            .executable_agent_ir(role_names::EXPLORER)
            .map_or(AgentRoleId::Worker, ExecutableAgentIr::role);

        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).map_err(ctx("workspace dir"))?;
        let registry = harw_authority::WorkspaceRegistry::build(
            dir.path(),
            [harw_authority::WorkspaceRegistration {
                tenant: harw_types::TenantId::from_str("t"),
                workspace: harw_types::WorkspaceId::from_str("ws"),
                root: workspace,
            }],
        )
        .map_err(ctx("workspace registry"))?;
        let binding = registry
            .resolve(
                &harw_types::TenantId::from_str("t"),
                &harw_types::WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("resolve"))?;
        let sandbox = harw_authority::SandboxSpec::from_resolved(
            binding,
            harw_authority::PermissionSet::from_policy([harw_authority::Permission::ReadWorkspace]),
        );

        let mut root_activation = harw_core::SessionActivation::default();
        for tool in disabled {
            root_activation.disable_tool(harw_tools::ToolName::new(*tool));
        }
        let root = harw_types::SessionId::new();
        let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(harw_core::SessionManager::new(events)));
        let spawner =
            ManagedAgentSpawner::new(Arc::clone(&manager), harw_core::ChildLimits::conservative())
                .with_role(
                    role_names::EXPLORER,
                    harw_types::AgentRole::Agent {
                        name: role_names::EXPLORER.to_owned(),
                    },
                    organizational_role,
                    factory,
                )
                .with_external_root_parent(
                    root.clone(),
                    harw_core::SpawnContext {
                        sandbox: sandbox.clone(),
                        suggestions: None,
                        capability_snapshot: None,
                        approval_actor: Some(harw_types::ApprovalActor::Operator {
                            id: "skill-injector-handover-test".to_owned(),
                        }),
                        organizational_role: AgentRoleId::RootOrchestrator,
                        allowed_child_orchestrators: Vec::new(),
                        trace: None,
                        ceiling: Some(crate::ceiling::root_ceiling(
                            crate::spec::CeilingPolicy::LocalRoot,
                        )),
                    },
                    None,
                    root_activation,
                )
                .map_err(|error| TestError::Unexpected(error.message))?;
        let input = SpawnInput {
            parent_session_id: root,
            handoff_call_id: harw_types::ToolCallId::new(),
            instructions: None,
            context: serde_json::Value::Null,
            ceiling: None,
        };
        let child = AgentSpawner::spawn_child(&spawner, role_names::EXPLORER, input, sandbox, None)
            .await
            .map_err(|error| {
                TestError::Unexpected(format!("explorer admission: {}", error.message))
            })?;
        let guard = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let session = guard
            .get(&child)
            .map_err(ctx("the explorer child is manager-owned"))?;
        let injector = session.skill_injector().cloned().ok_or(TestError::Missing(
            "the admitted explorer child carries a skill injector",
        ))?;
        Ok(injector)
    }

    /// PL-100 WP 1.5 (Review-Befund): Ende-zu-Ende über den echten
    /// Admissionsweg ([`spawned_explorer_injector`]) statt über die einzeln
    /// gerufenen Fabrik-Methoden. Die Kind-Sitzung trägt den Injektor der
    /// Fabrik, geschnitten mit der Kind-Aktivierung, die der Kern mit der
    /// der Wurzel geschnitten hat:
    /// - sperrt die Wurzel alle `fsread.*` des Kindes, ist `fsread-tools`
    ///   nicht mehr wählbar; `git-read-tools` und `planning` (Label
    ///   `filesystem.read`, weiter von `git.*` gedeckt) bleiben;
    /// - sperrt sie jedes Werkzeug des Kindes mit `ReadWorkspace`, verliert
    ///   auch das Label `filesystem.read` seine Deckung, und `planning`
    ///   fällt weg, obwohl die Sandbox das Leserecht weiter trägt.
    ///
    /// Bricht die Verdrahtung (kein `ParentGrant` im Registry-Bau, eine
    /// andere Reihenfolge von Meldung, Bau und Beobachtern, keine Übergabe
    /// in `child_session_observers`), fehlt der Injektor oder bleibt
    /// ungeschnitten.
    #[tokio::test]
    async fn an_admitted_explorer_child_carries_the_injector_cut_by_the_root_activation()
    -> TestResult {
        let served = served_tools(&factory_with_bundled_skills()?, role_names::EXPLORER)?;
        let fsread: Vec<&str> = served
            .iter()
            .map(String::as_str)
            .filter(|tool| tool.starts_with("fsread."))
            .collect();
        assert!(!fsread.is_empty(), "{served:?}");
        assert!(
            served.iter().any(|tool| tool.starts_with("git.")),
            "{served:?}"
        );

        let injector = spawned_explorer_injector(&[]).await?;
        for skill in ["fsread-tools", "git-read-tools", "planning"] {
            assert_indexed(&injector, skill)?;
            assert!(injector.tools_admit(skill), "uncut: {skill}");
        }

        let injector = spawned_explorer_injector(&fsread).await?;
        assert!(
            !injector.tools_admit("fsread-tools"),
            "fsread.* is disabled by the root activation"
        );
        for skill in ["git-read-tools", "planning"] {
            assert!(injector.tools_admit(skill), "{skill}");
        }

        let readers: Vec<&str> = served
            .iter()
            .map(String::as_str)
            .filter(|tool| {
                harw_registry_defaults::authority::tool_permission(tool)
                    == Some(harw_authority::Permission::ReadWorkspace)
            })
            .collect();
        let injector = spawned_explorer_injector(&readers).await?;
        for skill in ["fsread-tools", "git-read-tools", "planning"] {
            assert!(!injector.tools_admit(skill), "{skill}");
        }
        Ok(())
    }

    /// PL-100 O11 (Review-Befund): `intel-scout` und `context-steward` laufen
    /// mit dem Profil `NoTools`, tragen im System-Prompt aber ihre eigene
    /// Rollenbeschreibung statt „parameter-only triage agent"; eine
    /// Triage-Rolle behält die des Profils.
    #[tokio::test]
    async fn o11_children_carry_their_own_role_description() -> TestResult {
        use harw_extension_api::InstructionsProvider as _;

        /// Der zusammengesetzte System-Prompt der Kind-Registry von `role`.
        async fn system_prompt(
            factory: &RuntimeChildRegistryFactory,
            role: &str,
        ) -> TestResult<String> {
            let registry = factory
                .build_registry(role, &test_spawn_input(), None)
                .map_err(|error| TestError::Unexpected(format!("{role}: {}", error.message)))?;
            let mut prompt = String::new();
            for provider in registry.instructions_providers() {
                prompt.push_str(&provider.load().await.system_prompt);
            }
            Ok(prompt)
        }

        let factory = factory_with_bundled_skills()?;
        let triage = harw_registry_defaults::profile::RegistryProfile::NoTools.role_description();
        for role in [role_names::INTEL_SCOUT, role_names::CONTEXT_STEWARD] {
            let description = harw_registry_defaults::profile::role_description_for_role(role)
                .ok_or(TestError::Missing("role description of an O11 role"))?;
            let prompt = system_prompt(&factory, role).await?;
            assert!(
                prompt.contains(&format!("a {description} operating")),
                "{role}: {prompt}"
            );
            assert!(!prompt.contains(triage), "{role}");
        }
        let prompt = system_prompt(&factory, role_names::SECURITY_EGRESS_TRIAGE).await?;
        assert!(prompt.contains(&format!("a {triage} operating")));
        Ok(())
    }

    /// PL-100 WP 1.5: die Werkzeugoberfläche einer Definition schneidet die
    /// Registry wie `AgentSession::with_executable_agent_ir`: `forbidden`
    /// gewinnt, sonst `admitted` oder die immer verfügbaren Katalog-Werkzeuge.
    #[test]
    fn surface_admits_follows_the_session_activation_rule() {
        let admitted = vec!["fs.read".to_owned(), "web.fetch".to_owned()];
        let forbidden = vec!["web.fetch".to_owned(), "skills.load".to_owned()];
        assert!(surface_admits("fs.read", &admitted, &forbidden));
        assert!(!surface_admits("web.fetch", &admitted, &forbidden));
        assert!(!surface_admits("fs.write", &admitted, &forbidden));
        assert!(surface_admits("skills.search", &admitted, &forbidden));
        assert!(!surface_admits("skills.load", &admitted, &forbidden));
        assert!(!surface_admits("sys.ps", &[], &[]));
    }

    /// PL-100 O8/G1 (Plan §7 „Root hat genau 3 Werkzeuge"): die
    /// Operations-Registry einer Root-Session (`harw_ops::register_all`, wie
    /// `build_operations` in `assembly.rs`) trägt als `dod.*`-Modell-Werkzeuge
    /// genau `DOD_READ_TOOLS`, keines der `DOD_TRIAGE_TOOLS`; die
    /// Kind-Fabrik bietet `dod.*` nur den vier Triage-Rollen an.
    #[test]
    fn root_sessions_expose_exactly_the_three_dod_read_tools() -> TestResult {
        let mut registry = harw_operations::registry::OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let mut root: Vec<&str> = registry
            .iter()
            .map(|operation| operation.meta())
            .filter(|meta| {
                meta.surfaces
                    .iter()
                    .any(|surface| matches!(surface, harw_operations::Surface::ModelTool { .. }))
            })
            .map(|meta| meta.name)
            .filter(|name| name.starts_with("dod."))
            .collect();
        root.sort_unstable();
        let mut expected = harw_registry_defaults::profile::DOD_READ_TOOLS.to_vec();
        expected.sort_unstable();
        assert_eq!(root, expected);
        for tool in harw_registry_defaults::profile::DOD_TRIAGE_TOOLS {
            assert!(
                registry.find_by_name(tool).is_none(),
                "{tool} in der Wurzel"
            );
        }
        assert!(
            harw_registry_defaults::profile::dod_tools_for_role(role_names::ROOT_ORCHESTRATOR)
                .is_empty()
        );
        Ok(())
    }

    /// PL-97 D4: der `OpContext` der `dod.*` trägt den gebundenen Root-Space
    /// und die Identität des Kindes; ohne Root-Space scheitert ein Aufruf
    /// fail-closed (`NotAvailable`), statt auf `HARW_HOME` auszuweichen.
    #[tokio::test]
    async fn dod_read_tools_need_the_bound_home_context() -> TestResult {
        use harw_extension_api::{ToolExecutionContext, ToolProvider as _};

        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let project_dir = home.path().join("project");
        std::fs::create_dir_all(&project_dir).map_err(ctx("project dir"))?;
        let project = harw_home::ProjectRoot {
            root: project_dir.clone(),
            trust_key: project_dir.clone(),
            kind: harw_home::ProjectKind::Directory,
        };
        let context = Arc::new(
            harw_home::ResolvedHomeContext::new(home.path(), "default".to_owned(), project)
                .map_err(ctx("home context"))?,
        );
        let factory = factory_with_bundled_skills()?;
        let identity = factory.child_caller_identity(role_names::SECURITY_EGRESS_TRIAGE);

        let unbound = dod_services(factory.home_context.as_ref(), &identity);
        assert!(
            unbound
                .get::<Arc<harw_home::ResolvedHomeContext>>()
                .is_none()
        );
        let carried = unbound
            .get::<Arc<CallerIdentity>>()
            .ok_or(TestError::Missing("dod-ServiceMap ohne Identität"))?;
        assert!(Arc::ptr_eq(carried, &identity));

        // Ohne Root-Space: der Aufruf scheitert, bevor etwas gelesen wird.
        let registry = harw_authority::WorkspaceRegistry::build(
            home.path(),
            [harw_authority::WorkspaceRegistration {
                tenant: harw_types::TenantId::from_str("t"),
                workspace: harw_types::WorkspaceId::from_str("project"),
                root: project_dir,
            }],
        )
        .map_err(ctx("workspace registry"))?;
        let binding = registry
            .resolve(
                &harw_types::TenantId::from_str("t"),
                &harw_types::WorkspaceId::from_str("project"),
            )
            .map_err(ctx("resolve"))?;
        let execution = ToolExecutionContext::new(
            harw_types::SessionId::new(),
            harw_types::TurnId::new(),
            harw_authority::SandboxSpec::from_resolved(
                binding,
                harw_authority::PermissionSet::from_policy(Vec::new()),
            ),
        );
        let provider = factory.dod_read_provider(Arc::clone(&identity));
        let name = harw_tools::ToolName::new("dod.status");
        let executor = provider
            .executor(&name)
            .ok_or(TestError::Missing("dod.status-Executor"))?;
        let call = harw_extension_api::ToolCall {
            id: harw_types::ToolCallId::new(),
            name,
            arguments: serde_json::json!({}),
        };
        match executor.execute(&execution, &call).await {
            Err(error) => assert!(
                error.to_string().contains("bound harw root space"),
                "{error}"
            ),
            Ok(output) => {
                return Err(TestError::Unexpected(format!(
                    "ohne Root-Space darf dod.status nichts liefern: {output:?}"
                )));
            }
        }

        // Mit Root-Space: dieselbe Instanz im OpContext.
        let factory = factory.with_home_context(Arc::clone(&context));
        let bound = dod_services(factory.home_context.as_ref(), &identity);
        let found = bound
            .get::<Arc<harw_home::ResolvedHomeContext>>()
            .ok_or(TestError::Missing("dod-ServiceMap ohne Root-Space"))?;
        assert!(Arc::ptr_eq(found, &context));
        Ok(())
    }

    /// PL-90 W03d: die Operations-Werkzeuge einer Kind-Registry tragen die
    /// laufgebundene Identität ihrer Rolle.
    ///
    /// Die `ExtensionRegistry` selbst hält keine Dienste; sie liegen in den
    /// `OpContext`-Fabriken der Operations-Werkzeuge (`delegate_wave`,
    /// Matrix-Werkzeuge). Geprüft wird deshalb (1) die Identität je Rolle
    /// (`<rolle>.<uuid>`, je Aufruf neu), (2) dass beide Dienst-
    /// Zusammenstellungen genau diese Instanz tragen und (3) dass die
    /// Registries dieser Rollen mit ihren Werkzeugen und je einer eigenen
    /// vorgemerkten Identität montieren.
    #[test]
    fn child_registry_carries_caller_identity_of_its_role() -> TestResult {
        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
        )
        .map_err(ctx("factory builds"))?;

        // (1) Autor ist die laufgebundene Id `<rolle>.<uuid>` (je Aufruf neu,
        // als Pfadkomponente sicher), die Rolle die Basisrolle (hier gleich),
        // Vorfahren sind nicht herleitbar, ein Kind ist nie Operator.
        for role in [
            role_names::ROOT_ORCHESTRATOR,
            role_names::MATRIX_GAME_MASTER,
            role_names::EXPLORER,
        ] {
            let identity = factory.child_caller_identity(role);
            assert!(
                identity.agent_id.as_str().starts_with(&format!("{role}.")),
                "{role}: Id {} trägt den Rollennamen als Präfix",
                identity.agent_id
            );
            assert_ne!(
                identity.agent_id.as_str(),
                role,
                "{role}: nie der bloße Rollenname, den sich Geschwister teilen"
            );
            assert!(
                harw_knowledge::diary::validate_agent_id(&identity.agent_id).is_ok(),
                "{role}: die Id ist eine sichere Pfadkomponente"
            );
            let twin = factory.child_caller_identity(role);
            assert_ne!(
                identity.agent_id, twin.agent_id,
                "{role}: jedes Kind bekommt eine eigene Id"
            );
            assert!(!Arc::ptr_eq(&identity, &twin), "{role}");
            assert_eq!(
                identity.role,
                Some(harw_knowledge::AgentRoleRef::new(role)),
                "{role}"
            );
            assert!(identity.ancestors.is_empty(), "{role}: keine Elternrolle");
            assert!(!identity.operator, "{role}: ein Kind ist nie Operator");
        }

        // (2) Beide ServiceMaps tragen dieselbe Instanz, keine Kopie.
        let identity = factory.child_caller_identity(role_names::ROOT_ORCHESTRATOR);
        let wave = delegate_wave_services(
            &factory.spawner_slot,
            factory.delegate_wave_store.as_ref(),
            &identity,
        );
        let carried = wave.get::<Arc<CallerIdentity>>().ok_or(TestError::Missing(
            "delegate_wave-ServiceMap ohne Identität",
        ))?;
        assert!(
            Arc::ptr_eq(carried, &identity),
            "delegate_wave muss dieselbe Identitätsinstanz tragen"
        );
        let master = game_master_services(
            &factory.spawner_slot,
            factory.delegate_wave_store.as_ref(),
            factory.home_context.as_ref(),
            &identity,
        );
        let carried = master
            .get::<Arc<CallerIdentity>>()
            .ok_or(TestError::Missing("Game-Master-ServiceMap ohne Identität"))?;
        assert!(
            Arc::ptr_eq(carried, &identity),
            "die Matrix-Werkzeuge müssen dieselbe Identitätsinstanz tragen"
        );

        // (3) Die Registries mit Operations-Werkzeugen montieren, jede mit
        // ihrer eigenen laufgebundenen Identität (für den Observer-Haken
        // vorgemerkt). Eine zweite Montage derselben Rolle bekommt eine
        // andere Id.
        let registry = factory
            .build_registry(role_names::ROOT_ORCHESTRATOR, &test_spawn_input(), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert!(
            registry_tool_names(&registry)
                .iter()
                .any(|name| name == harw_core_bridge::DELEGATE_WAVE_TOOL),
            "der Orchestrator trägt delegate_wave"
        );
        let first = factory
            .pending_run_identity(role_names::ROOT_ORCHESTRATOR)
            .ok_or(TestError::Missing(
                "vorgemerkte Identität nach build_registry",
            ))?;
        assert!(
            first
                .agent_id
                .as_str()
                .starts_with(&format!("{}.", role_names::ROOT_ORCHESTRATOR))
        );
        assert_eq!(
            first.role,
            Some(harw_knowledge::AgentRoleRef::new(
                role_names::ROOT_ORCHESTRATOR
            ))
        );
        let _second_registry = factory
            .build_registry(role_names::ROOT_ORCHESTRATOR, &test_spawn_input(), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let second = factory
            .pending_run_identity(role_names::ROOT_ORCHESTRATOR)
            .ok_or(TestError::Missing(
                "vorgemerkte Identität der zweiten Montage",
            ))?;
        assert_ne!(
            first.agent_id, second.agent_id,
            "zwei Montagen derselben Rolle teilen keine Agent-Id"
        );
        assert!(!Arc::ptr_eq(&first, &second));

        let registry = factory
            .build_registry(role_names::MATRIX_GAME_MASTER, &test_spawn_input(), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let names = registry_tool_names(&registry);
        assert!(
            harw_registry_defaults::profile::MATRIX_GAME_MASTER_TOOLS
                .iter()
                .all(|tool| names.iter().any(|name| name == tool)),
            "der Game Master trägt seine Matrix-Werkzeuge: {names:?}"
        );
        Ok(())
    }

    /// PL-90 W03d (Review P2): die Lauf-Identität folgt aus der vom Controller
    /// gemeldeten Kind-Sitzung ([`ChildRegistryFactory::announce_child_session`]).
    /// Eine neu gebaute Fabrik (Neustart: `recover_root_child` baut die Registry
    /// neu und stellt die ursprüngliche Kind-ID wieder her) leitet für dieselbe
    /// Sitzung dieselbe Agent-Id ab; ein Geschwister derselben Rolle mit eigener
    /// Sitzung bekommt eine andere, ein Bau ohne Meldung eine frische.
    #[test]
    fn announced_child_session_makes_the_run_identity_stable_across_a_restart() -> TestResult {
        let new_factory = || -> TestResult<RuntimeChildRegistryFactory> {
            RuntimeChildRegistryFactory::new(
                test_project(),
                Arc::new(harw_core::EchoModelProvider::new("echo")),
                test_chain(&harw_config::ResolvedConfig::default()),
            )
            .map_err(ctx("factory builds"))
        };
        // Wie die Admission: erst die Kind-Sitzung melden, dann bauen.
        let admit = |factory: &RuntimeChildRegistryFactory,
                     child: &harw_types::SessionId|
         -> TestResult<Arc<CallerIdentity>> {
            factory.announce_child_session(role_names::EXPLORER, child);
            let _registry = factory
                .build_registry(role_names::EXPLORER, &test_spawn_input(), None)
                .map_err(|error| TestError::Unexpected(error.message))?;
            factory
                .pending_run_identity(role_names::EXPLORER)
                .ok_or(TestError::Missing(
                    "vorgemerkte Identität nach build_registry",
                ))
        };

        let child = harw_types::SessionId::new();
        let before_crash = admit(&new_factory()?, &child)?;
        assert_eq!(
            before_crash.agent_id.as_str(),
            format!("{}.{}", role_names::EXPLORER, child.as_str()),
            "die Agent-Id ist <rolle>.<kind-sitzungs-id>"
        );

        // Neustart: neue Fabrik, dieselbe Kind-Sitzung (Wiederherstellung).
        let restarted = new_factory()?;
        let recovered = admit(&restarted, &child)?;
        assert_eq!(
            recovered.agent_id, before_crash.agent_id,
            "ein wiederhergestelltes Kind trägt dieselbe Agent-Id wie vor dem Absturz"
        );
        assert_eq!(recovered.role, before_crash.role);
        assert!(
            !Arc::ptr_eq(&recovered, &before_crash),
            "die Identität wird neu gebaut, nicht geteilt"
        );

        // Ein Geschwister derselben Rolle (eigene Sitzung) bleibt getrennt.
        let sibling = admit(&restarted, &harw_types::SessionId::new())?;
        assert_ne!(sibling.agent_id, recovered.agent_id);

        // Ohne Meldung leitet der Bau aus einer frischen Sitzung ab und erbt
        // nichts von der letzten Meldung.
        let _registry = restarted
            .build_registry(role_names::EXPLORER, &test_spawn_input(), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let unannounced = restarted
            .pending_run_identity(role_names::EXPLORER)
            .ok_or(TestError::Missing("vorgemerkte Identität ohne Meldung"))?;
        assert_ne!(unannounced.agent_id, sibling.agent_id);
        assert_ne!(unannounced.agent_id, recovered.agent_id);
        Ok(())
    }

    /// PL-90 W03d: die aus der Kind-Sitzung abgeleitete Agent-Id ist eine
    /// einzelne, sichere Pfadkomponente — auch für eine Sitzungs-ID, die aus
    /// einem gespeicherten Wiederherstellungs-Snapshot stammt — und die
    /// Ableitung bleibt injektiv; eine UUID bleibt unverändert.
    #[test]
    fn run_agent_id_is_a_safe_and_injective_path_component() -> TestResult {
        let id =
            |role: &str, child: &str| run_agent_id(role, &harw_types::SessionId::from_str(child));

        let uuid = harw_types::SessionId::new();
        assert_eq!(
            run_agent_id(role_names::EXPLORER, &uuid).as_str(),
            format!("{}.{}", role_names::EXPLORER, uuid.as_str())
        );

        for hostile in [
            "../../etc",
            "a/b",
            "a\\b",
            "..",
            ".",
            "a:b",
            "a\nb",
            "x%2Fy",
            "/abs",
        ] {
            let agent = id(role_names::EXPLORER, hostile);
            assert!(
                harw_knowledge::diary::validate_agent_id(&agent).is_ok(),
                "{hostile:?} → {agent}"
            );
            assert!(
                !agent.as_str().contains(['/', '\\', ':']),
                "{hostile:?} → {agent}"
            );
        }
        // Rollenname mit Pfadtrenner: wie bisher durch `-` ersetzt.
        let custom = id("my/role", "child");
        assert_eq!(custom.as_str(), "my-role.child");

        // Verschiedene Sitzungs-IDs bleiben verschieden.
        assert_ne!(id("r", "a/b"), id("r", "a-b"));
        assert_ne!(id("r", "a.b"), id("r", "a%2Eb"));
        assert_ne!(id("r", "x%2Fy"), id("r", "x/y"));
        assert_ne!(id("r", "a"), id("r", "b"));
        Ok(())
    }

    /// PL-90 W03d (Review P2): schreiben → durable Wiederherstellung → lesen.
    /// Ein Kind schreibt über den Diary-Recorder unter seiner Lauf-Id einen
    /// Verdichtungs-Eintrag; nach einem Neustart (neue Fabrik, neuer Recorder,
    /// dieselbe Kind-Sitzung) liest das wiederhergestellte Kind dasselbe
    /// Tagebuch und schreibt darin weiter. Ein Geschwister derselben Rolle hat
    /// ein eigenes, leeres Tagebuch.
    #[test]
    fn recovered_child_keeps_its_diary_and_a_sibling_stays_apart() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("temporary knowledge root"))?;
        let store = Arc::new(harw_knowledge::KnowledgeStore::new(root.path()));
        let at = 1_758_715_200;
        // Ein „Prozess“: Fabrik samt Recorder; nach dem Neustart beides neu.
        let new_process = || -> TestResult<RuntimeChildRegistryFactory> {
            let clock: crate::diary_wiring::DiaryClock = Arc::new(move || {
                jiff::Timestamp::from_second(at).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
            });
            let recorder = Arc::new(
                crate::diary_wiring::DiaryRecorder::new(
                    Arc::clone(&store),
                    harw_knowledge::AgentId::new("root".to_owned()),
                )
                .with_clock(clock),
            );
            let factory = RuntimeChildRegistryFactory::new(
                test_project(),
                Arc::new(harw_core::EchoModelProvider::new("echo")),
                test_chain(&harw_config::ResolvedConfig::default()),
            )
            .map_err(ctx("factory builds"))?;
            Ok(factory.with_child_diary(Some(recorder)))
        };
        // Wie die Admission: melden, bauen, bei den Beobachtern anmelden.
        let admit = |factory: &RuntimeChildRegistryFactory,
                     child: &harw_types::SessionId|
         -> TestResult<(harw_knowledge::AgentId, harw_core::ChildSessionObservers)> {
            factory.announce_child_session(role_names::EXPLORER, child);
            let _registry = factory
                .build_registry(role_names::EXPLORER, &test_spawn_input(), None)
                .map_err(|error| TestError::Unexpected(error.message))?;
            let agent = factory
                .pending_run_identity(role_names::EXPLORER)
                .ok_or(TestError::Missing("vorgemerkte Identität"))?
                .agent_id
                .clone();
            let observers = factory.child_session_observers(role_names::EXPLORER, child);
            Ok((agent, observers))
        };
        let compact = |observers: &harw_core::ChildSessionObservers,
                       child: &harw_types::SessionId,
                       summary: &str|
         -> TestResult {
            let observer = observers
                .compaction
                .as_ref()
                .ok_or(TestError::Missing("compaction observer"))?;
            observer.on_compacted(
                child,
                &harw_core::CompactionOutcome {
                    tokens_before: 80_000,
                    tokens_after: 9_000,
                    summarized: true,
                    summary_text: Some(summary.to_owned()),
                    ..harw_core::CompactionOutcome::default()
                },
            );
            Ok(())
        };

        // Vor dem Absturz: das Kind schreibt einen Eintrag in sein Tagebuch.
        let child = harw_types::SessionId::new();
        let (before, observers) = admit(&new_process()?, &child)?;
        compact(&observers, &child, "Vor dem Absturz: Module kartiert.")?;
        assert_eq!(diary_entries(&store, before.as_str(), at)?.len(), 1);

        // Durable Wiederherstellung: neuer Prozess, dieselbe Kind-Sitzung.
        let restarted = new_process()?;
        let (after, observers) = admit(&restarted, &child)?;
        assert_eq!(
            after, before,
            "dieselbe Wissens-Identität nach dem Neustart"
        );
        let prior = diary_entries(&store, after.as_str(), at)?;
        assert!(
            prior
                .iter()
                .any(|entry| entry.text.contains("Vor dem Absturz")),
            "das wiederhergestellte Kind liest seinen Eintrag von vor dem Absturz: {prior:?}"
        );
        compact(&observers, &child, "Nach der Wiederherstellung: weiter.")?;
        assert_eq!(
            diary_entries(&store, after.as_str(), at)?.len(),
            2,
            "das Kind schreibt im selben Tagebuch weiter"
        );

        // Ein Geschwister derselben Rolle hat ein eigenes Tagebuch.
        let sibling_session = harw_types::SessionId::new();
        let (sibling, sibling_observers) = admit(&restarted, &sibling_session)?;
        assert_ne!(sibling, after);
        assert!(
            diary_entries(&store, sibling.as_str(), at)?.is_empty(),
            "das Geschwister sieht das Tagebuch des Kindes nicht"
        );
        compact(
            &sibling_observers,
            &sibling_session,
            "Eigene Notiz des Geschwisters.",
        )?;
        assert_eq!(diary_entries(&store, sibling.as_str(), at)?.len(), 1);
        assert_eq!(diary_entries(&store, after.as_str(), at)?.len(), 2);
        Ok(())
    }

    /// PL-90 W03d: ein benutzerdefinierter Agent tritt mit einer Id auf, die
    /// seinen eigenen Namen als Präfix trägt, und mit der Basisrolle als Rolle
    /// (wie alle namensgebundenen Tabellen, Plan R9, Teil B).
    #[test]
    fn child_caller_identity_of_a_custom_agent_uses_its_base_role() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = home.path().join("agents").join("web-scout");
        std::fs::create_dir_all(&path).map_err(ctx("Agentenordner"))?;
        std::fs::write(
            path.join("definition.toml"),
            r#"schema = "harwness.agent/v1"
id = "user.agent.web-scout@1"
version = "1.0.0"
extends = { id = "harwness.agent.explorer@1" }
role = "worker"
specialization = "web-scout"
"#,
        )
        .map_err(ctx("definition"))?;
        std::fs::write(path.join("system.md"), "Kurz recherchieren.").map_err(ctx("system.md"))?;
        let config =
            harw_config::discover_config(&[home.path().to_path_buf()]).map_err(ctx("Discovery"))?;
        let agents = harw_registry_defaults::ConfigAgents::from_config(&config)
            .map_err(ctx("Agenten senken"))?;
        let builtin = builtin_agent_definitions(&HashMap::new()).map_err(ctx("Rollen"))?;
        let roster = harw_registry_defaults::AgentRoster::from_config(&builtin, &agents)
            .map_err(ctx("Roster"))?;
        let factory = RuntimeChildRegistryFactory::with_definitions(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
            roster.definitions().clone(),
        )
        .with_custom_agents(roster.custom_wiring());

        let identity = factory.child_caller_identity("web-scout");
        assert!(
            identity.agent_id.as_str().starts_with("web-scout."),
            "{}",
            identity.agent_id
        );
        assert_eq!(
            identity.role,
            Some(harw_knowledge::AgentRoleRef::new(role_names::EXPLORER))
        );
        assert!(identity.ancestors.is_empty());
        assert!(!identity.operator);
        Ok(())
    }

    /// PL-90 W03d (Audit W03d-1): der Gedächtnis-Fakten-Recall der Wurzel
    /// (`MemoryFactsContextProvider`, Namensraum
    /// `harw.runtime.memory_facts`) ist ausdrücklich nur der Wurzel
    /// vorbehalten. Keine Kind-Registry registriert einen Context-Provider in
    /// diesem Namensraum.
    #[test]
    fn child_registry_has_no_memory_facts_provider() -> TestResult {
        use harw_extension_api::types::{ContextFragment, TurnInputContext};
        use harw_extension_api::{ContextProvider, ExtFuture};

        /// Gegenprobe: ein Provider im Namensraum der Wurzel-Fakten.
        struct FactsStub;
        impl ContextProvider for FactsStub {
            fn contribute<'a>(
                &'a self,
                _ctx: &'a TurnInputContext,
            ) -> ExtFuture<'a, Vec<ContextFragment>> {
                Box::pin(async { Vec::new() })
            }

            fn namespace(&self) -> &'static str {
                harw_context::sources::memory_facts.namespace
            }
        }

        let namespace: &str = harw_context::sources::memory_facts.namespace;
        // Die Abfrage unten erkennt einen solchen Provider tatsächlich.
        let probe = ExtensionRegistry::builder()
            .context_provider(Arc::new(FactsStub))
            .map_err(|error| TestError::Unexpected(format!("{error:?}")))?
            .build();
        assert!(probe.context_provider_namespace(namespace).is_some());
        assert!(
            probe
                .context_providers()
                .iter()
                .any(|provider| provider.namespace() == namespace)
        );

        let factory = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
        )
        .map_err(ctx("factory builds"))?;
        for role in [
            role_names::ROOT_ORCHESTRATOR,
            role_names::CODING_ORCHESTRATOR,
            role_names::EXPLORER,
            role_names::EXECUTOR,
        ] {
            let registry = factory
                .build_registry(role, &test_spawn_input(), None)
                .map_err(|error| TestError::Unexpected(error.message))?;
            assert!(
                registry.context_provider_namespace(namespace).is_none(),
                "{role}: kein Fakten-Namensraum in der Kind-Registry"
            );
            assert!(
                !registry
                    .context_providers()
                    .iter()
                    .any(|provider| provider.namespace() == namespace),
                "{role}: kein Fakten-Provider in der Kind-Registry"
            );
        }
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
        let agent = factory.child_caller_identity(role).agent_id.clone();
        let registry = factory
            .with_knowledge_readers(
                role,
                &agent,
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

    // --- Runde 5, Teil C: Diary der Kind-Agenten ---

    /// Einträge des Agenten `agent` am Tag von `at` (Sekunden seit Epoche).
    fn diary_entries(
        store: &harw_knowledge::KnowledgeStore,
        agent: &str,
        at: i64,
    ) -> TestResult<Vec<harw_knowledge::diary::DiaryRecord>> {
        let date = jiff::Timestamp::from_second(at)
            .map_err(ctx("valid timestamp"))?
            .strftime("%Y-%m-%d")
            .to_string();
        Ok(harw_knowledge::diary::read_day_entries(
            store,
            &harw_knowledge::AgentId::new(agent.to_owned()),
            &date,
        )
        .map_err(ctx("read diary day"))?
        .map(|day| day.entries)
        .unwrap_or_default())
    }

    /// Ein Kind mit Runde und Verdichtung schreibt unter seiner
    /// laufgebundenen Agent-Id (`<rolle>.<uuid>`, in `build_registry`
    /// gemintet) einen `compaction`- und genau einen `end-of-session`-Eintrag,
    /// auch bei doppelter Freigabe; unter dem bloßen Rollennamen entsteht
    /// nichts, ein Geschwister derselben Rolle schreibt in sein eigenes
    /// Tagebuch; ein Kind ohne Runde schreibt nichts; ohne Recorder liefert die
    /// Fabrik keine Beobachter.
    #[test]
    fn child_diary_records_compaction_and_one_session_end_under_the_run_bound_id() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("temporary knowledge root"))?;
        let store = Arc::new(harw_knowledge::KnowledgeStore::new(root.path()));
        let at = 1_758_715_200;
        let clock: crate::diary_wiring::DiaryClock = Arc::new(move || {
            jiff::Timestamp::from_second(at).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        });
        let recorder = Arc::new(
            crate::diary_wiring::DiaryRecorder::new(
                Arc::clone(&store),
                harw_knowledge::AgentId::new("root".to_owned()),
            )
            .with_clock(clock),
        );
        let without = RuntimeChildRegistryFactory::new(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
        )
        .map_err(ctx("factory builds"))?;
        let idle = harw_types::SessionId::from_str("child-idle");
        let observers = without.child_session_observers(role_names::EXPLORER, &idle);
        assert!(observers.compaction.is_none() && observers.tool_outcome.is_none());

        let factory = without.with_child_diary(Some(Arc::clone(&recorder)));
        // Wie die Admission: erst die Registry bauen (mintet und merkt die
        // Lauf-Id vor), dann die Sitzung anmelden.
        let _registry = factory
            .build_registry(role_names::EXPLORER, &test_spawn_input(), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let run_id = factory
            .pending_run_identity(role_names::EXPLORER)
            .ok_or(TestError::Missing("vorgemerkte Identität"))?
            .agent_id
            .clone();
        let busy = harw_types::SessionId::from_str("child-busy");
        let observers = factory.child_session_observers(role_names::EXPLORER, &busy);
        assert!(
            factory.pending_run_identity(role_names::EXPLORER).is_none(),
            "der Observer-Haken entnimmt die vorgemerkte Identität"
        );
        let turns = observers
            .tool_outcome
            .ok_or(TestError::Missing("turn observer"))?;
        let compaction = observers
            .compaction
            .ok_or(TestError::Missing("compaction observer"))?;
        turns.on_turn_finished(&busy);
        compaction.on_compacted(
            &busy,
            &harw_core::CompactionOutcome {
                tokens_before: 80_000,
                tokens_after: 9_000,
                summarized: true,
                summary_text: Some("Kind hat die Module kartiert.".to_owned()),
                ..harw_core::CompactionOutcome::default()
            },
        );
        factory.child_session_released(role_names::EXPLORER, &busy);
        factory.child_session_released(role_names::EXPLORER, &busy);

        let written = diary_entries(&store, run_id.as_str(), at)?;
        let triggers: Vec<_> = written.iter().map(|entry| entry.trigger).collect();
        assert_eq!(
            triggers,
            vec![
                harw_knowledge::diary::DiaryTrigger::Compaction,
                harw_knowledge::diary::DiaryTrigger::EndOfSession,
            ],
            "{written:?}"
        );
        assert!(written[1].text.contains("1 Turns"), "{}", written[1].text);
        assert!(
            diary_entries(&store, role_names::EXPLORER, at)?.is_empty(),
            "unter dem bloßen Rollennamen entsteht kein Tagebuch"
        );
        assert!(diary_entries(&store, "root", at)?.is_empty());

        // Ein Geschwister derselben Rolle: eigene Id, eigenes Tagebuch; das
        // des ersten Kindes bleibt unverändert.
        let _sibling_registry = factory
            .build_registry(role_names::EXPLORER, &test_spawn_input(), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let sibling_id = factory
            .pending_run_identity(role_names::EXPLORER)
            .ok_or(TestError::Missing("vorgemerkte Identität des Geschwisters"))?
            .agent_id
            .clone();
        assert_ne!(sibling_id, run_id, "Geschwister teilen keine Agent-Id");
        let sibling = harw_types::SessionId::from_str("child-sibling");
        let sibling_turns = factory
            .child_session_observers(role_names::EXPLORER, &sibling)
            .tool_outcome
            .ok_or(TestError::Missing("turn observer des Geschwisters"))?;
        sibling_turns.on_turn_finished(&sibling);
        factory.child_session_released(role_names::EXPLORER, &sibling);
        assert_eq!(diary_entries(&store, sibling_id.as_str(), at)?.len(), 1);
        assert_eq!(diary_entries(&store, run_id.as_str(), at)?.len(), 2);

        // Ein Kind ohne beendete Runde hinterlässt keinen Eintrag.
        let _planner_registry = factory
            .build_registry(role_names::PLANNER, &test_spawn_input(), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let planner_id = factory
            .pending_run_identity(role_names::PLANNER)
            .ok_or(TestError::Missing("vorgemerkte Identität des Planners"))?
            .agent_id
            .clone();
        let _ = factory.child_session_observers(role_names::PLANNER, &idle);
        factory.child_session_released(role_names::PLANNER, &idle);
        assert!(diary_entries(&store, planner_id.as_str(), at)?.is_empty());
        Ok(())
    }

    /// Plan R9, Teil B: ein benutzerdefinierter Agent aus dem Roster läuft
    /// mit seiner geklemmten IR, dem Profil seines Roster-Eintrags und den
    /// namensgebundenen Tabellen seiner Basisrolle.
    #[test]
    fn custom_roster_agents_use_their_clamped_ir_and_base_role_tables() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        for (dir, source) in [
            (
                "note-taker",
                r#"schema = "harwness.agent/v1"
id = "user.agent.note-taker@1"
version = "1.0.0"
extends = { id = "harwness.agent.worker-base@1" }
role = "worker"
specialization = "note-taker"
instructions_file = "system.md"

[tools]
admitted = ["fs.read", "shell.exec"]
"#,
            ),
            (
                "web-scout",
                r#"schema = "harwness.agent/v1"
id = "user.agent.web-scout@1"
version = "1.0.0"
extends = { id = "harwness.agent.explorer@1" }
role = "worker"
specialization = "web-scout"
"#,
            ),
        ] {
            let path = home.path().join("agents").join(dir);
            std::fs::create_dir_all(&path).map_err(ctx("Agentenordner"))?;
            std::fs::write(path.join("definition.toml"), source).map_err(ctx("definition"))?;
            std::fs::write(path.join("system.md"), "Notizen knapp zusammenfassen.")
                .map_err(ctx("system.md"))?;
        }
        let config =
            harw_config::discover_config(&[home.path().to_path_buf()]).map_err(ctx("Discovery"))?;
        let agents = harw_registry_defaults::ConfigAgents::from_config(&config)
            .map_err(ctx("Agenten senken"))?;
        let builtin = builtin_agent_definitions(&HashMap::new()).map_err(ctx("Rollen"))?;
        let roster = harw_registry_defaults::AgentRoster::from_config(&builtin, &agents)
            .map_err(ctx("Roster"))?;

        let factory = RuntimeChildRegistryFactory::with_definitions(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
            roster.definitions().clone(),
        )
        .with_custom_agents(roster.custom_wiring());

        let ir = factory
            .executable_agent_ir("note-taker")
            .ok_or(crate::test_support::TestError::Missing("note-taker IR"))?;
        assert!(
            !ir.tool_surface()
                .admitted()
                .iter()
                .any(|tool| tool == "shell.exec"),
            "die Decke entfernt shell.exec"
        );
        assert_eq!(
            factory.registry_profile_for("note-taker"),
            Some(harw_registry_defaults::RegistryProfile::ReadOnlyExplore)
        );
        assert_eq!(factory.lookup_role("note-taker"), role_names::ANALYST);
        assert_eq!(
            factory.lookup_role(role_names::EXPLORER),
            role_names::EXPLORER
        );
        assert!(
            factory
                .custom_agents
                .get("note-taker")
                .and_then(|wiring| wiring.instructions.as_deref())
                .is_some_and(|text| text.contains("Notizen"))
        );
        // Die interne Modellstelle folgt der Basisrolle.
        assert_eq!(
            factory.internal_point_for_child("web-scout"),
            internal_point_for_role(role_names::EXPLORER)
        );
        // Unbekannte Namen bleiben fail-closed.
        assert_eq!(factory.registry_profile_for("not-registered"), None);
        Ok(())
    }

    /// #22 Welle 1B: `job.goal_kind` und `spawn.workspace_hint` erreichen das
    /// Kind als Auftragsrahmen (der Hinweis ausdrücklich beratend); ohne
    /// beide entsteht kein Fragment.
    #[test]
    fn spawn_metadata_note_carries_goal_kind_and_the_advisory_workspace_hint() -> TestResult {
        let raw = harw_agent_dsl::parse::parse_toml(
            "schema = \"harwness.agent/v1\"\n\
             id = \"harwness.agent.framed-child@1\"\n\
             version = \"1.0.0\"\n\
             role = \"worker\"\n\
             specialization = \"framed-child\"\n\n\
             [job]\n\
             goal_kind = \"review\"\n\n\
             [spawn]\n\
             workspace_hint = \"crates/foo\"\n",
        )
        .map_err(ctx("parse test definition"))?;
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
            config: raw.tables,
        };
        let ir = harw_agent_dsl::lower(&resolved).map_err(ctx("lower test definition"))?;
        let note = spawn_metadata_note("framed-child", &ir)
            .ok_or(crate::test_support::TestError::Missing("Auftragsrahmen"))?;
        assert!(note.contains("goal_kind): review"), "{note}");
        assert!(note.contains("crates/foo"), "{note}");
        assert!(note.contains("beratend"), "{note}");
        let plain = ir_with_skills(&[])?;
        assert!(spawn_metadata_note("skill-child", &plain).is_none());
        Ok(())
    }

    /// #22 Welle 1B: `[authority] capabilities` eines eigenen Agenten
    /// schneidet die Rechte seiner Kind-Registry — nie weiter als das
    /// Profil. Eine eingebaute Rolle ohne Autorität behält ihr Profil.
    #[test]
    fn custom_agent_authority_narrows_the_child_registry_rights() -> TestResult {
        use harw_authority::Permission;
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = home.path().join("agents").join("scribe");
        std::fs::create_dir_all(&path).map_err(ctx("Agentenordner"))?;
        std::fs::write(
            path.join("definition.toml"),
            r#"schema = "harwness.agent/v1"
id = "user.agent.scribe@1"
version = "1.0.0"
role = "worker"
specialization = "scribe"

[authority]
capabilities = ["filesystem.read"]

[tools]
admitted = ["fs.read", "fs.write"]
"#,
        )
        .map_err(ctx("definition"))?;
        let config =
            harw_config::discover_config(&[home.path().to_path_buf()]).map_err(ctx("Discovery"))?;
        let agents = harw_registry_defaults::ConfigAgents::from_config(&config)
            .map_err(ctx("Agenten senken"))?;
        let builtin = builtin_agent_definitions(&HashMap::new()).map_err(ctx("Rollen"))?;
        let roster = harw_registry_defaults::AgentRoster::from_config(&builtin, &agents)
            .map_err(ctx("Roster"))?;
        let factory = RuntimeChildRegistryFactory::with_definitions(
            test_project(),
            Arc::new(harw_core::EchoModelProvider::new("echo")),
            test_chain(&harw_config::ResolvedConfig::default()),
            roster.definitions().clone(),
        )
        .with_custom_agents(roster.custom_wiring());

        let profile = factory
            .registry_profile_for("scribe")
            .ok_or(crate::test_support::TestError::Missing("scribe profile"))?;
        assert_eq!(
            profile,
            harw_registry_defaults::RegistryProfile::WorkspaceEdit,
            "der generische Schreib-Worker"
        );
        let granted = factory.granted_for("scribe", profile);
        assert!(granted.is_subset_of(&profile.required_permissions()));
        assert!(granted.contains(Permission::ReadWorkspace));
        assert!(
            !granted.contains(Permission::WriteWorkspace),
            "die Autorität nennt nur filesystem.read"
        );

        let explorer = profile_for_role(role_names::EXPLORER)
            .ok_or(crate::test_support::TestError::Missing("explorer profile"))?;
        assert_eq!(
            factory.granted_for(role_names::EXPLORER, explorer),
            explorer.required_permissions()
        );
        Ok(())
    }
}
