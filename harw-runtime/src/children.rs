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
use harw_core::{ChildRegistryFactory, ModelProvider};
use harw_extension_api::{AgentSpawnError, ExtensionRegistry, SpawnInput};
use harw_project_discovery::ProjectContext;
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    IdentityOverrides, assemble_registry_for_project, profile_for_role,
};

use crate::approval::ApprovalChain;
use crate::error::{RuntimeError, RuntimeResult};

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
        }
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

    /// Liefert den für den Eltern-Turn gewählten Modellanbieter.
    ///
    /// # Fehler
    /// Nie: der Anbieter ist zur Konstruktionszeit aufgelöst.
    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::clone(&self.model))
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
}
