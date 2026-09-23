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
use std::sync::{Arc, OnceLock, Weak};

use harw_agent_dsl::ExecutableAgentIr;
use harw_config::{InternalModelPoint, ResolvedInternalModel, resolve_internal_model};
use harw_core::{ChildRegistryFactory, ManagedAgentSpawner, ModelProvider, PinnedModelProvider};
use harw_extension_api::{
    AgentSpawnError, AgentSpawner, ExtensionRegistry, SpawnFuture, SpawnInput,
};
use harw_project_discovery::ProjectContext;
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    HostPermitWiring, IdentityOverrides,
    assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits,
    profile_for_role, role_names,
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
/// [`role_names::UIA_WRITER`], [`role_names::UIA_SHELL_WORKER`]) fällt
/// **absichtlich** auf `None` durch (Welle 3a) — sie hängt seither nicht mehr
/// an einer über [`resolve_internal_models_for_children`] aufgelösten
/// internen Modellstelle des Eltern-Modells, sondern bekommt ihr eigenes,
/// von der UIA-Sitzung abgeleitetes Modell direkt über die eigene
/// Kind-Registry-Fabrik der Rolle (`RuntimeAssemblyBuilder::build`,
/// `build_spawner`s `uia_worker_factory`; das Modell selbst entsteht über
/// [`crate::model::build_uia_model_with_resolver`] +
/// [`crate::model::build_uia_worker_model`]). Jede andere Rolle (inklusive
/// `planner`, `executor`, `security-*` und unbekannter/repo-lokaler Rollen)
/// bleibt unverändert beim Eltern-Modell — `None`.
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

/// Deckelt die höchstens gleichzeitig laufende Anzahl Instanzen einer Rolle
/// (Welle 6a, Singleton-Erzwingung).
///
/// # Description
/// Der Nutzer verlangt, dass UIA und die gesamte `uia-worker`-Rollenfamilie
/// (`AgentRoleId::UiaWorker`: [`role_names::UIA_WORKER`],
/// [`role_names::UIA_EXPLORER`], [`role_names::UIA_WRITER`],
/// [`role_names::UIA_SHELL_WORKER`]) **nie** mit mehr als einer gleichzeitig
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
/// [`InternalModelPoint::WorkerComplex`], Addendum D+E) — die übrigen
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
            sandbox_profile: harw_sandbox::SandboxProfile::Strict,
            host_permit_wiring: None,
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
            ..IdentityOverrides::default()
        };
        // Eine Kette je Kind: `for_child` löst die Modus-Zelle
        // ([`ApprovalModeCell::detached`]), Geschwister beeinflussen sich also
        // nicht.
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
        let registry = registry_builder.build();
        tracing::debug!(
            role,
            profile = ?profile,
            project_root = %self.project.project_root.display(),
            "runtime.child_registry.assembled"
        );
        Ok(registry)
    }

    /// Liefert den Modellanbieter für eine Kind-Rolle.
    ///
    /// # Beschreibung
    /// [`internal_point_for_role`] bildet `role` auf eine interne
    /// Modellstelle ab (Addendum C). Ist keine Stelle zuständig, keine
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
        let Some(point) = internal_point_for_role(role) else {
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
        if let Some(point) = internal_point_for_role(role) {
            return Ok(self.pinned_model_for_point(role, point));
        }
        let is_worker = self
            .builtin_definitions
            .get(role)
            .is_some_and(|ir| ir.role() == harw_agent_dsl::roles::AgentRoleId::Worker);
        if !is_worker {
            return Ok(Arc::clone(&self.model));
        }
        let point = match complexity {
            Some(harw_core::TaskComplexity::Simple) => InternalModelPoint::WorkerSimple,
            Some(harw_core::TaskComplexity::Complex) | None => InternalModelPoint::WorkerComplex,
        };
        Ok(self.pinned_model_for_point(role, point))
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
        let Some(point) = internal_point_for_role(role) else {
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
        if let Some(point) = internal_point_for_role(role) {
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
    fn reasoning_effort_defaults_for_role_task_yields_none_for_main_model_fallback() -> TestResult {
        // A role with no internal-model-point override that is not a builtin
        // `worker` role falls back to the parent's main model — whose
        // provider/model id this factory does not track (see field doc on
        // `reasoning_effort_config`). `planner` is deliberately *not* used
        // here: its builtin definition has `role = "worker"`, so it takes the
        // `WorkerSimple`/`WorkerComplex` path exactly like `model_for_task`.
        let factory = factory_with_worker_complex_effort_defaults(Some("high"), Some("high"))?;
        for role in [
            // Builtin definition, but not a worker role.
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
}
