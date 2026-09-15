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
//!    [`ProjectContext`] des Elternteils und benutzt
//!    [`assemble_registry_for_project`], das **keinen Pfad** entgegennimmt und
//!    deshalb gar nicht erkennen *kann*.
//! 3. **Kette nicht vererbt (F-018).** `assemble_registry_for_project`
//!    registriert nur die `DefaultApprovalPolicy`. Die Config-Politik des
//!    Elternteils erreichte den Fan-out nie. Diese Fabrik installiert
//!    [`ApprovalChain::for_child`] in **jede** Kind-Registry.

use std::collections::HashMap;
use std::sync::Arc;

use harw_agent_dsl::ExecutableAgentIr;
use harw_config::{InternalModelPoint, ResolvedInternalModel, resolve_internal_model};
use harw_core::{ChildRegistryFactory, ModelProvider, PinnedModelProvider};
use harw_extension_api::{AgentSpawnError, ExtensionRegistry, SpawnInput};
use harw_project_discovery::ProjectContext;
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    IdentityOverrides, assemble_registry_for_project, profile_for_role, role_names,
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
/// `uia-worker` (Addendum J, exklusiver Schnellhelfer der UIA, eigene
/// Organisationsrolle `AgentRoleId::UiaWorker`) →
/// [`InternalModelPoint::WorkerSimple`], die einfache Worker-Modellstufe —
/// er ist für kleine Schnelleingriffe gedacht, nicht für tiefe Recherche
/// oder Konsolidierung. `agent-steward` (Addendum K, eigene
/// Organisationsrolle `AgentRoleId::AgentSteward`) →
/// [`InternalModelPoint::WorkerComplex`]: Validieren, Rechte-Delta und
/// Vorschlagsentscheidung verlangen eigenes Urteilsvermögen, auch wenn der
/// Auftrag klein aussieht. Jede andere Rolle (inklusive `planner`,
/// `executor`, `security-*` und unbekannter/repo-lokaler Rollen) bleibt
/// unverändert beim Eltern-Modell — `None`.
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
        r if r == role_names::UIA_WORKER => Some(InternalModelPoint::WorkerSimple),
        r if r == role_names::AGENT_STEWARD => Some(InternalModelPoint::WorkerComplex),
        _ => None,
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
    pub fn with_profile_agents_dir(mut self, profile_agents_dir: Option<std::path::PathBuf>) -> Self {
        self.profile_agents_dir = profile_agents_dir;
        self
    }

    /// Löst `point` gegen [`Self::internal_models`] auf und liefert entweder
    /// einen [`PinnedModelProvider`] oder — falls die Stelle unbekannt/nicht
    /// aufgelöst ist oder das Hauptmodell erzwingt — das Eltern-Modell.
    ///
    /// # Description
    /// Gemeinsame Hilfsfunktion für [`ChildRegistryFactory::model_for`] und
    /// [`ChildRegistryFactory::model_for_task`], damit beide dieselbe
    /// Pinning-Logik nutzen und nicht auseinanderlaufen.
    fn pinned_model_for_point(&self, role: &str, point: InternalModelPoint) -> Arc<dyn ModelProvider> {
        let Some(resolved) = self.internal_models.get(&point) else {
            return Arc::clone(&self.model);
        };
        if resolved.is_main_model() {
            return Arc::clone(&self.model);
        }
        let provider_id = resolved.provider.as_deref().map(harw_types::ProviderId::from);
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
            organizational_role: self.builtin_definitions.get(role).map(ExecutableAgentIr::role),
            ..IdentityOverrides::default()
        };
        // Eine Kette je Kind: `for_child` löst die Modus-Zelle
        // ([`ApprovalModeCell::detached`]), Geschwister beeinflussen sich also
        // nicht.
        let child_chain = self.chain.for_child();
        let assembled = assemble_registry_for_project(
            profile,
            &self.project,
            overrides,
            child_chain.mode().clone(),
        )
        .map_err(|error| AgentSpawnError {
            message: format!("could not assemble child registry for role '{role}': {error}"),
        })?;
        // `install_over_default`, nicht `install`: `assemble_registry_for_project`
        // hat die `DefaultApprovalPolicy` über `child_chain.mode()` bereits
        // registriert (harw-registry-defaults/src/profile.rs:921-922). Eine
        // zweite wäre wirkungsgleich, aber eine Dublette (Befund Z2c-06).
        let registry = child_chain
            .install_over_default(assembled.registry)
            .build();
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
            organizational_role: self.builtin_definitions.get(role).map(ExecutableAgentIr::role),
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
        let registry = child_chain.install_over_default(assembled.registry).build();
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn internal_point_for_role_maps_uia_worker_to_worker_simple() {
        assert_eq!(
            internal_point_for_role(role_names::UIA_WORKER),
            Some(InternalModelPoint::WorkerSimple)
        );
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
}
