//! Der Agenten-Roster: alle startbaren Agenten eines Laufs — eingebaute
//! Rollen plus benutzerdefinierte Agenten aus Profil und vertrautem Projekt
//! (Plan R9, Teil B).
//!
//! # Warum
//! Bis Plan R9 war nur [`role_names::ALL`] startbar. Eine Definition unter
//! `~/.harw/profiles/<p>/agents/<name>/definition.toml` oder
//! `<projekt>/.harw/agents/<name>/definition.toml` wurde zwar entdeckt und
//! gesenkt (`harw_config::ResolvedConfig::executable_agents`), war aber nie
//! ein Spawn-Ziel.
//!
//! # Regeln für benutzerdefinierte Agenten
//! Jede gesenkte Definition mit Rolle `worker`, `uia-worker` oder
//! `child-orchestrator` wird ein Spawn-Ziel unter ihrer `specialization`:
//!
//! - **Namenskollision:** trägt sie den Namen einer eingebauten Rolle oder
//!   Basis, gewinnt die eingebaute; die Definition wird mit Warnung
//!   übersprungen. Zwei benutzerdefinierte Agenten mit derselben
//!   Spezialisierung: der mit der kleineren `DefinitionId` gewinnt, der
//!   andere wird gemeldet.
//! - **Basisrolle:** die nächste eingebaute Rolle in ihrer `extends`-Kette
//!   (aus dem Auflösungs-Trace der IR). Erweitert sie nur eine Basis, gilt
//!   für Worker der generische read-only Worker [`GENERIC_WORKER_BASE`]
//!   (Spawn-Tiefe höchstens 1), für Child-Orchestratoren die Basis
//!   `child-orchestrator-base` mit der Planungsoberfläche von
//!   [`GENERIC_CHILD_ORCHESTRATOR_BASE`], für UIA-Worker
//!   [`GENERIC_UIA_WORKER_BASE`]. Die Organisationsrolle der Basis muss der
//!   eigenen gleichen, sonst wird die Definition übersprungen.
//! - **Generischer Schreib-Worker:** admittiert ein Worker, der nur
//!   `worker-base` erweitert, `fs.write`/`fs.edit`, gilt statt der
//!   Analyst-Decke die eines schreibenden Workers
//!   ([`GENERIC_WRITING_WORKER_PROFILE`] = `WorkspaceEdit`, dazu
//!   [`GENERIC_WORKER_WRITE_TOOLS`]) — Nutzerentscheidung Plan R9 für
//!   Schreib-Worker wie `synthesis-writer`. `shell.*` und `web.*` bleiben
//!   draußen; jeder Schreibzugriff läuft wie bei jeder anderen Rolle durch
//!   Sandbox und Freigabekette des Kindes.
//! - **Querschnittswerkzeuge:** [`ALWAYS_AVAILABLE_TOOLS`] (`skills.*`,
//!   `agents.catalog`/`agents.delegate`) werden nicht vom Schnitt entfernt:
//!   admittiert die Basisrolle sie, bekommt der Agent sie auch ohne eigene
//!   Aufzählung — außer er verbietet sie ausdrücklich.
//! - **Rechtedecke:** Registry-Profil, Reducer, Composition-Werkzeuge und
//!   Delegationsziele sind die der Basisrolle
//!   ([`AgentRoster::base_role`]). Die IR wird unter die der Basis geklemmt
//!   ([`ExecutableAgentIr::clamped_to`]): Werkzeuge nur im Schnitt,
//!   Budget und `max_depth` höchstens die der Basis. Eine Definition kann
//!   ihre Rechte damit nur verengen, nie erweitern. Rechte über der Basis
//!   gibt es weiterhin nur über den Steward und `user_required`.
//!
//! # Konsumenten
//! `harw-runtime` registriert [`AgentRoster::names`] im Spawner und reicht
//! [`AgentRoster::definitions`] an die Kind-Fabriken. Welle 2 (Katalog,
//! `agents.delegate`) liest [`AgentRoster::entries`].
//!
//! # Nebenläufigkeit
//! Nach [`AgentRoster::build`] unveränderlich; `Send + Sync`.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use harw_agent_dsl::ExecutableAgentIr;
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::roles::AgentRoleId;
use harw_authority::Permission;
use harw_config::AgentDefinitionMeta;

use crate::embedded_agents::{
    BASE_DEFINITION_NAMES, CHILD_ORCHESTRATOR_BASE_NAME, WORKER_BASE_NAME, builtin_agent_toml,
    builtin_base_definitions,
};
use crate::error::RegistryDefaultsError;
use crate::profile::{RegistryProfile, profile_for_role, role_names};

/// Basisrolle eines benutzerdefinierten Workers, der nur `worker-base`
/// erweitert: read-only Workspace-Zugriff ohne Netz (`ReadOnlyExplore`, Web
/// ausdrücklich verboten).
pub const GENERIC_WORKER_BASE: &str = role_names::ANALYST;

/// Spawn-Tiefe, die ein generischer benutzerdefinierter Worker höchstens
/// bekommt (die generische Basis [`GENERIC_WORKER_BASE`] darf zwei).
pub const GENERIC_WORKER_MAX_DEPTH: u32 = 1;

/// Basisrolle (Profil, Reducer, Delegationsziele) eines benutzerdefinierten
/// Child-Orchestrators, der nur `child-orchestrator-base` erweitert:
/// `Planning` ohne Netz-Durchreichung.
pub const GENERIC_CHILD_ORCHESTRATOR_BASE: &str = role_names::ANALYSIS_ORCHESTRATOR;

/// Basisrolle eines benutzerdefinierten UIA-Workers ohne eingebaute
/// UIA-Worker-Rolle in seiner Kette: die read-only Erkundung der UIA.
pub const GENERIC_UIA_WORKER_BASE: &str = role_names::UIA_EXPLORER;

/// Die Schreibwerkzeuge, die ein generischer Worker (nur `worker-base`)
/// über der Decke von [`GENERIC_WORKER_BASE`] behalten darf, wenn seine
/// Definition sie admittiert (Nutzerentscheidung Plan R9: ein
/// benutzerdefinierter Schreib-Worker wie `synthesis-writer` schreibt in
/// seinen Auftrag). Keine Shell, kein Netz.
pub const GENERIC_WORKER_WRITE_TOOLS: &[&str] = &["fs.write", "fs.edit"];

/// Das Registry-Profil eines generischen schreibenden Workers:
/// Workspace lesen und schreiben, ohne `shell.*` und ohne `web.*`.
pub const GENERIC_WRITING_WORKER_PROFILE: RegistryProfile = RegistryProfile::WorkspaceEdit;

/// Querschnittliche Katalogwerkzeuge, die jeder benutzerdefinierte Agent
/// erhält, sobald seine Basisrolle sie admittiert — auch wenn seine eigene
/// Definition sie nicht aufzählt, und nie, wenn sie sie ausdrücklich
/// verbietet. Rein lesend bzw. an Sichtbarkeit und Organisationsrolle
/// gebunden; sie erweitern keine Rechte über die Basis hinaus.
pub const ALWAYS_AVAILABLE_TOOLS: &[&str] = &[
    "skills.search",
    "skills.load",
    "agents.catalog",
    "agents.delegate",
];

/// Was die Laufzeit für einen benutzerdefinierten Agenten zusätzlich zur
/// geklemmten IR braucht ([`AgentRoster::custom_wiring`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomAgentWiring {
    /// Die eingebaute Rolle, deren namensgebundene Tabellen gelten
    /// (Composition-Werkzeuge, Reducer, Delegationsziele, interne
    /// Modellstelle).
    pub base_role: String,
    /// Das Registry-Profil, mit dem die Kind-Registry montiert wird.
    pub profile: RegistryProfile,
    /// Instruktionstext aus `instructions_file`, falls vorhanden.
    pub instructions: Option<String>,
    /// Eigene `[delegation].targets` der Definition, falls vorhanden; sonst
    /// gelten die der Basisrolle
    /// ([`crate::authority::delegation_targets_for_role`]).
    pub delegation_targets: Option<Vec<String>>,
}

/// Woher ein Roster-Eintrag stammt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterSource {
    /// Eine eingebaute Rolle aus [`role_names::ALL`].
    BuiltIn,
    /// Eine benutzerdefinierte Definition aus Profil oder vertrautem Projekt.
    Custom {
        /// Die kanonische `DefinitionId` (`ResolvedConfig::executable_agents`).
        definition_id: String,
        /// Die höchste Schicht, aus der die Definition stammt.
        layer: Option<DefinitionLayer>,
    },
}

/// Ein startbarer Agent, wie ihn Katalog und Delegation sehen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    /// Der Spawn-Name (Rollenname bzw. `specialization`).
    pub name: String,
    /// Die Organisationsrolle aus der IR.
    pub role: AgentRoleId,
    /// Menschenlesbare Beschreibung aus der Definition, falls vorhanden.
    pub description: Option<String>,
    /// Die fest gebundenen Skills der Definition.
    pub skills: Vec<String>,
    /// Die eingebaute Rolle, deren Profil, Reducer und Composition-Werkzeuge
    /// gelten. Für eingebaute Rollen der eigene Name.
    pub base_role: String,
    /// Das Registry-Profil der Basisrolle.
    pub profile: RegistryProfile,
    /// Die effektiv admittierten Werkzeuge (geklemmte IR).
    pub tools: Vec<String>,
    /// `true`, wenn kein admittiertes Werkzeug schreibt oder Prozesse startet.
    pub read_only: bool,
    /// Spawn-Tiefe unterhalb dieses Agenten (geklemmt).
    pub max_depth: Option<u32>,
    /// Token-Budget (geklemmt), falls die Definition eines trägt.
    pub budget_tokens: Option<u64>,
    /// Herkunft.
    pub source: RosterSource,
}

impl RosterEntry {
    /// Eine einzeilige Zusammenfassung der Rechte, z. B.
    /// `"read-only exploration agent; read-only; 18 tools"`.
    #[must_use]
    pub fn profile_summary(&self) -> String {
        format!(
            "{}; {}; {} tools",
            self.profile.role_description(),
            if self.read_only {
                "read-only"
            } else {
                "writes/executes"
            },
            self.tools.len()
        )
    }

    /// `true` für einen benutzerdefinierten Agenten.
    #[must_use]
    pub fn is_custom(&self) -> bool {
        matches!(self.source, RosterSource::Custom { .. })
    }
}

/// Der Roster aller startbaren Agenten eines Laufs.
#[derive(Debug, Clone, Default)]
pub struct AgentRoster {
    /// Einträge nach Spawn-Name.
    entries: BTreeMap<String, RosterEntry>,
    /// Die (für benutzerdefinierte Agenten geklemmten) IRs nach Spawn-Name.
    definitions: HashMap<String, ExecutableAgentIr>,
    /// Instruktionstext benutzerdefinierter Agenten nach Spawn-Name.
    instructions: HashMap<String, String>,
    /// Eigene `[delegation].targets` benutzerdefinierter Agenten.
    delegation_targets: HashMap<String, Vec<String>>,
}

/// Welche Basis ein benutzerdefinierter Agent in seiner Kette trägt.
enum Ancestor<'a> {
    /// Eine eingebaute, startbare Rolle.
    Role(&'a str),
    /// Einer der Basis-Layer ([`BASE_DEFINITION_NAMES`]).
    Base(&'a str),
}

impl AgentRoster {
    /// Baut den Roster aus den eingebauten Rollen und der aufgelösten Config.
    ///
    /// # Errors
    /// Wie [`Self::build`].
    pub fn from_config(
        builtin: &HashMap<String, ExecutableAgentIr>,
        config: &harw_config::ResolvedConfig,
    ) -> Result<Self, RegistryDefaultsError> {
        Self::build(
            builtin,
            &config.executable_agents,
            &config.agent_definition_meta,
        )
    }

    /// Baut den Roster.
    ///
    /// # Arguments
    /// - `builtin`: die gesenkten eingebauten Rollen
    ///   ([`crate::embedded_agents::builtin_agent_definitions`]), nach
    ///   Rollenname.
    /// - `local`: die gesenkten Definitionen der vertrauten Layer
    ///   (`ResolvedConfig::executable_agents`), nach `DefinitionId`.
    /// - `meta`: Beschreibung und Instruktionen dazu
    ///   (`ResolvedConfig::agent_definition_meta`), nach `DefinitionId`.
    ///
    /// # Errors
    /// [`RegistryDefaultsError::AgentDefinition`], wenn eine eingebaute Basis
    /// nicht senkt (Defekt der eingebetteten Dateien). Eine ungeeignete
    /// benutzerdefinierte Definition ist nie ein Fehler, sondern wird mit
    /// Warnung übersprungen.
    pub fn build(
        builtin: &HashMap<String, ExecutableAgentIr>,
        local: &HashMap<String, ExecutableAgentIr>,
        meta: &HashMap<String, AgentDefinitionMeta>,
    ) -> Result<Self, RegistryDefaultsError> {
        let mut roster = Self::default();
        for (name, ir) in builtin {
            let Some(profile) = profile_for_role(name) else {
                tracing::warn!(role = %name, "registry.roster.builtin_role_without_profile");
                continue;
            };
            roster.entries.insert(
                name.clone(),
                entry_for(
                    name,
                    ir,
                    name,
                    profile,
                    builtin_description(name),
                    RosterSource::BuiltIn,
                ),
            );
            roster.definitions.insert(name.clone(), ir.clone());
        }
        if local.is_empty() {
            return Ok(roster);
        }

        let bases = builtin_base_definitions()?;
        let builtin_by_id: HashMap<String, &str> = builtin
            .iter()
            .map(|(name, ir)| (ir.id().to_string(), name.as_str()))
            .collect();
        let base_by_id: HashMap<String, &str> = bases
            .iter()
            .map(|(name, ir)| (ir.id().to_string(), name.as_str()))
            .collect();

        let mut sorted: Vec<(&String, &ExecutableAgentIr)> = local.iter().collect();
        sorted.sort_by_key(|(id, _)| *id);
        for (id, ir) in sorted {
            if !matches!(
                ir.role(),
                AgentRoleId::Worker | AgentRoleId::UiaWorker | AgentRoleId::ChildOrchestrator
            ) {
                continue;
            }
            let name = ir.specialization().to_owned();
            if builtin_by_id.contains_key(id) || base_by_id.contains_key(id) {
                tracing::warn!(
                    definition = %id,
                    "registry.roster.local_definition_reuses_builtin_id: skipped, the built-in wins"
                );
                continue;
            }
            if builtin.contains_key(&name) || BASE_DEFINITION_NAMES.contains(&name.as_str()) {
                tracing::warn!(
                    agent = %name,
                    definition = %id,
                    "registry.roster.name_collides_with_builtin: skipped, the built-in wins"
                );
                continue;
            }
            if roster.entries.contains_key(&name) {
                tracing::warn!(
                    agent = %name,
                    definition = %id,
                    "registry.roster.duplicate_custom_name: skipped, the first definition wins"
                );
                continue;
            }
            let Some(Ceiling {
                base_role,
                ir: ceiling,
                depth_cap,
                writes,
            }) = ceiling_for(ir, &builtin_by_id, &base_by_id, builtin, &bases)
            else {
                tracing::warn!(
                    agent = %name,
                    definition = %id,
                    role = ?ir.role(),
                    "registry.roster.no_matching_base_role: skipped"
                );
                continue;
            };
            let base_profile = if writes {
                Some(GENERIC_WRITING_WORKER_PROFILE)
            } else {
                profile_for_role(base_role)
            };
            let Some(profile) = base_profile else {
                tracing::warn!(
                    agent = %name,
                    base = %base_role,
                    "registry.roster.base_role_without_profile: skipped"
                );
                continue;
            };
            let extra_allowed: &[&str] = if writes {
                GENERIC_WORKER_WRITE_TOOLS
            } else {
                &[]
            };
            let mut clamped = ir.clamped_to_with(ceiling, extra_allowed);
            if let Some(cap) = depth_cap {
                clamped = clamped.with_max_depth_at_most(cap);
            }
            let always: Vec<&str> = ALWAYS_AVAILABLE_TOOLS
                .iter()
                .copied()
                .filter(|tool| {
                    ceiling
                        .tool_surface()
                        .admitted()
                        .iter()
                        .any(|admitted| admitted == tool)
                })
                .collect();
            if !always.is_empty() {
                clamped = clamped.with_additional_admitted(&always);
            }
            // Plan R9, Teil F: `job.*` gehört zu `shell.exec` — ein eigener
            // Agent über einer Shell-Rolle behält es, auch wenn seine
            // Definition nur `shell.exec` aufzählt; über einer
            // Orchestrator-Basis die Kontrollwerkzeuge. Nie über die Decke
            // der Basis hinaus, nie gegen ein ausdrückliches `forbidden`.
            let jobs = job_companion_tools(&clamped, ceiling);
            if !jobs.is_empty() {
                clamped = clamped.with_additional_admitted(&jobs);
            }
            let definition_meta = meta.get(id);
            let entry = entry_for(
                &name,
                &clamped,
                base_role,
                profile,
                definition_meta.and_then(|meta| meta.description.clone()),
                RosterSource::Custom {
                    definition_id: id.clone(),
                    layer: definition_meta.and_then(|meta| meta.layer),
                },
            );
            if let Some(instructions) = definition_meta.and_then(|meta| meta.instructions.clone()) {
                roster.instructions.insert(name.clone(), instructions);
            }
            if let Some(targets) = definition_meta.and_then(|meta| meta.delegation_targets.clone())
            {
                roster.delegation_targets.insert(name.clone(), targets);
            }
            tracing::debug!(
                agent = %name,
                base = %base_role,
                definition = %id,
                "registry.roster.custom_agent_registered"
            );
            roster.entries.insert(name.clone(), entry);
            roster.definitions.insert(name, clamped);
        }
        Ok(roster)
    }

    /// Alle Einträge, nach Spawn-Name sortiert.
    pub fn entries(&self) -> impl Iterator<Item = &RosterEntry> {
        self.entries.values()
    }

    /// Der Eintrag eines Spawn-Namens.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&RosterEntry> {
        self.entries.get(name)
    }

    /// Alle Spawn-Namen, sortiert.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Anzahl der Einträge.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` ohne Einträge.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Die IRs aller Einträge nach Spawn-Name — eingebaute unverändert,
    /// benutzerdefinierte geklemmt. Die Kind-Fabriken lesen daraus Aktivierung,
    /// Budget, Skills und Organisationsrolle.
    #[must_use]
    pub fn definitions(&self) -> &HashMap<String, ExecutableAgentIr> {
        &self.definitions
    }

    /// Die eingebaute Rolle, deren namensgebundene Tabellen (Profil,
    /// Reducer, Composition-Werkzeuge, Delegationsziele) für `name` gelten.
    #[must_use]
    pub fn base_role(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(|entry| entry.base_role.as_str())
    }

    /// Die Zuordnung benutzerdefinierter Spawn-Namen zu ihrer Basisrolle.
    #[must_use]
    pub fn custom_bases(&self) -> HashMap<String, String> {
        self.entries
            .values()
            .filter(|entry| entry.is_custom())
            .map(|entry| (entry.name.clone(), entry.base_role.clone()))
            .collect()
    }

    /// Der Instruktionstext eines benutzerdefinierten Agenten
    /// (`instructions_file`), falls vorhanden.
    #[must_use]
    pub fn instructions(&self, name: &str) -> Option<&str> {
        self.instructions.get(name).map(String::as_str)
    }

    /// Die Delegationsziele eines Eintrags: die eigene `[delegation].targets`
    /// eines benutzerdefinierten Agenten, sonst die seiner Basisrolle bzw.
    /// der eingebauten Rolle selbst. `None` heißt: keine Deklaration.
    #[must_use]
    pub fn delegation_targets(&self, name: &str) -> Option<Vec<String>> {
        if let Some(targets) = self.delegation_targets.get(name) {
            return Some(targets.clone());
        }
        crate::authority::delegation_targets_for_role(self.base_role(name)?)
    }

    /// Alle Instruktionstexte nach Spawn-Name.
    #[must_use]
    pub fn all_instructions(&self) -> &HashMap<String, String> {
        &self.instructions
    }

    /// Die Laufzeit-Verdrahtung aller benutzerdefinierten Agenten nach
    /// Spawn-Name (Basisrolle, Profil, Instruktionen).
    #[must_use]
    pub fn custom_wiring(&self) -> HashMap<String, CustomAgentWiring> {
        self.entries
            .values()
            .filter(|entry| entry.is_custom())
            .map(|entry| {
                (
                    entry.name.clone(),
                    CustomAgentWiring {
                        base_role: entry.base_role.clone(),
                        profile: entry.profile,
                        instructions: self.instructions.get(&entry.name).cloned(),
                        delegation_targets: self.delegation_targets.get(&entry.name).cloned(),
                    },
                )
            })
            .collect()
    }
}

/// Die Rechtedecke eines benutzerdefinierten Agenten.
struct Ceiling<'a> {
    /// Eingebaute Rolle für namensgebundene Tabellen.
    base_role: &'a str,
    /// Die IR, unter die geklemmt wird.
    ir: &'a ExecutableAgentIr,
    /// Zusätzliche absolute Tiefenobergrenze.
    depth_cap: Option<u32>,
    /// Generischer schreibender Worker ([`GENERIC_WORKER_WRITE_TOOLS`],
    /// [`GENERIC_WRITING_WORKER_PROFILE`]).
    writes: bool,
}

/// Plan R9, Teil F: die `job.*`-Werkzeuge, die ein eigener Agent über seiner
/// Basis zusätzlich behält.
///
/// # Beschreibung
/// `job.start` nur, wenn die (geklemmte) Definition `shell.exec` admittiert;
/// die lesenden/steuernden Werkzeuge (`job.status/logs/stop/list/wait`)
/// immer. In jedem Fall nur, was die Basis selbst admittiert.
fn job_companion_tools<'a>(
    clamped: &ExecutableAgentIr,
    ceiling: &'a ExecutableAgentIr,
) -> Vec<&'a str> {
    let admits_shell = clamped
        .tool_surface()
        .admitted()
        .iter()
        .any(|tool| tool == "shell.exec");
    ceiling
        .tool_surface()
        .admitted()
        .iter()
        .map(String::as_str)
        .filter(|tool| crate::profile::JOB_TOOLS.contains(tool))
        .filter(|tool| admits_shell || *tool != harw_tool_job::JOB_START_TOOL)
        .collect()
}

/// Bestimmt Basisrolle, Rechtedecke und optionale Tiefenobergrenze eines
/// benutzerdefinierten Agenten. `None`, wenn keine passende Basis existiert.
fn ceiling_for<'a>(
    ir: &ExecutableAgentIr,
    builtin_by_id: &HashMap<String, &'a str>,
    base_by_id: &HashMap<String, &'a str>,
    builtin: &'a HashMap<String, ExecutableAgentIr>,
    bases: &'a HashMap<String, ExecutableAgentIr>,
) -> Option<Ceiling<'a>> {
    let ancestor = ir
        .trace()
        .steps
        .iter()
        .rev()
        .filter(|step| step.kind == "base")
        .find_map(|step| {
            builtin_by_id
                .get(&step.source)
                .map(|name| Ancestor::Role(name))
                .or_else(|| {
                    base_by_id
                        .get(&step.source)
                        .map(|name| Ancestor::Base(name))
                })
        });
    let role = ir.role();
    match ancestor {
        Some(Ancestor::Role(name)) => {
            let ceiling = builtin.get(name)?;
            (ceiling.role() == role).then_some(Ceiling {
                base_role: name,
                ir: ceiling,
                depth_cap: None,
                writes: false,
            })
        }
        Some(Ancestor::Base(base)) => match (base, role) {
            (CHILD_ORCHESTRATOR_BASE_NAME, AgentRoleId::ChildOrchestrator) => Some(Ceiling {
                base_role: GENERIC_CHILD_ORCHESTRATOR_BASE,
                ir: bases.get(CHILD_ORCHESTRATOR_BASE_NAME)?,
                depth_cap: None,
                writes: false,
            }),
            (WORKER_BASE_NAME, _) => generic_ceiling(ir, builtin, bases),
            _ => None,
        },
        None => generic_ceiling(ir, builtin, bases),
    }
}

/// Die generische Decke einer Rolle ohne eingebaute Rolle in der Kette.
///
/// Ein generischer Worker, dessen Definition `fs.write`/`fs.edit`
/// admittiert, bekommt die Decke eines schreibenden Workers
/// ([`GENERIC_WRITING_WORKER_PROFILE`] plus [`GENERIC_WORKER_WRITE_TOOLS`]
/// über der Analyst-Decke); Shell und Netz bleiben draußen.
fn generic_ceiling<'a>(
    ir: &ExecutableAgentIr,
    builtin: &'a HashMap<String, ExecutableAgentIr>,
    bases: &'a HashMap<String, ExecutableAgentIr>,
) -> Option<Ceiling<'a>> {
    match ir.role() {
        AgentRoleId::Worker => Some(Ceiling {
            base_role: GENERIC_WORKER_BASE,
            ir: builtin.get(GENERIC_WORKER_BASE)?,
            depth_cap: Some(GENERIC_WORKER_MAX_DEPTH),
            writes: ir
                .tool_surface()
                .admitted()
                .iter()
                .any(|tool| GENERIC_WORKER_WRITE_TOOLS.contains(&tool.as_str())),
        }),
        AgentRoleId::UiaWorker => Some(Ceiling {
            base_role: GENERIC_UIA_WORKER_BASE,
            ir: builtin.get(GENERIC_UIA_WORKER_BASE)?,
            depth_cap: None,
            writes: false,
        }),
        AgentRoleId::ChildOrchestrator => Some(Ceiling {
            base_role: GENERIC_CHILD_ORCHESTRATOR_BASE,
            ir: bases.get(CHILD_ORCHESTRATOR_BASE_NAME)?,
            depth_cap: None,
            writes: false,
        }),
        _ => None,
    }
}

/// Baut einen Roster-Eintrag aus einer (ggf. geklemmten) IR.
fn entry_for(
    name: &str,
    ir: &ExecutableAgentIr,
    base_role: &str,
    profile: RegistryProfile,
    description: Option<String>,
    source: RosterSource,
) -> RosterEntry {
    let tools = ir.tool_surface().admitted().to_vec();
    let writes = tools.iter().any(|tool| {
        matches!(
            crate::authority::tool_permission(tool),
            Some(Permission::WriteWorkspace | Permission::ExecuteProcess)
        )
    });
    RosterEntry {
        name: name.to_owned(),
        role: ir.role(),
        description,
        skills: ir.skills().to_vec(),
        base_role: base_role.to_owned(),
        profile,
        tools,
        read_only: !writes,
        max_depth: ir.spawn_contract().max_depth(),
        budget_tokens: ir
            .spawn_contract()
            .budget()
            .and_then(harw_agent_dsl::executable::BudgetSpec::max_tokens),
        source,
    }
}

/// Die Beschreibung einer eingebauten Rolle aus ihrer TOML-Datei.
fn builtin_description(name: &str) -> Option<String> {
    static DESCRIPTIONS: OnceLock<HashMap<&'static str, String>> = OnceLock::new();
    DESCRIPTIONS
        .get_or_init(|| {
            builtin_agent_toml()
                .iter()
                .filter_map(|(name, source)| {
                    let raw = harw_agent_dsl::parse::parse_toml(source).ok()?;
                    Some((*name, raw.description?))
                })
                .collect()
        })
        .get(name)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedded_agents::builtin_agent_definitions;
    use crate::test_support::{TestError, TestResult, ctx};

    fn builtin() -> TestResult<HashMap<String, ExecutableAgentIr>> {
        builtin_agent_definitions(&HashMap::new()).map_err(ctx("eingebaute Rollen senken"))
    }

    /// Schreibt `definitions` als Profil-Layer und entdeckt ihn.
    fn discover(
        definitions: &[(&str, &str)],
    ) -> TestResult<(tempfile::TempDir, harw_config::ResolvedConfig)> {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        for (dir, source) in definitions {
            let path = home.path().join("agents").join(dir);
            std::fs::create_dir_all(&path).map_err(ctx("Agentenordner"))?;
            std::fs::write(path.join("definition.toml"), source).map_err(ctx("definition.toml"))?;
        }
        let config =
            harw_config::discover_config(&[home.path().to_path_buf()]).map_err(ctx("Discovery"))?;
        Ok((home, config))
    }

    fn worker(id: &str, name: &str, extends: &str, tools: &str) -> String {
        format!(
            r#"schema = "harwness.agent/v1"
id = "{id}"
version = "1.0.0"
extends = {{ id = "{extends}" }}
role = "worker"
specialization = "{name}"
description = "Test-Worker {name}"
skills = ["planning"]

[tools]
admitted = [{tools}]

[spawn]
max_depth = 5

[spawn.budget]
max_tokens = 999999
"#
        )
    }

    #[test]
    fn builtin_roles_are_all_in_the_roster() -> TestResult {
        let builtin = builtin()?;
        let roster = AgentRoster::build(&builtin, &HashMap::new(), &HashMap::new())
            .map_err(ctx("Roster"))?;
        assert_eq!(roster.len(), role_names::ALL.len());
        for role in role_names::ALL {
            let entry = roster.entry(role).ok_or(TestError::Missing("Rolle"))?;
            assert_eq!(entry.source, RosterSource::BuiltIn);
            assert_eq!(entry.base_role, *role);
        }
        let explorer = roster
            .entry(role_names::EXPLORER)
            .ok_or(TestError::Missing("explorer"))?;
        assert!(explorer.description.is_some());
        assert!(explorer.read_only);
        assert!(!explorer.profile_summary().is_empty());
        Ok(())
    }

    #[test]
    fn profile_worker_extending_worker_base_is_spawnable_with_a_narrowed_profile() -> TestResult {
        let builtin = builtin()?;
        let (_home, config) = discover(&[(
            "note-taker",
            worker(
                "user.agent.note-taker@1",
                "note-taker",
                "harwness.agent.worker-base@1",
                // `shell.exec` liegt über der Decke; `web.fetch` verbietet die
                // generische Basis.
                r#""fs.read", "fs.grep", "shell.exec", "web.fetch""#,
            )
            .as_str(),
        )])?;
        let roster = AgentRoster::from_config(&builtin, &config).map_err(ctx("Roster"))?;
        let entry = roster
            .entry("note-taker")
            .ok_or(TestError::Missing("note-taker muss startbar sein"))?;
        assert!(entry.is_custom());
        assert_eq!(entry.base_role, GENERIC_WORKER_BASE);
        assert_eq!(entry.profile, RegistryProfile::ReadOnlyExplore);
        assert_eq!(entry.role, AgentRoleId::Worker);
        // Die Querschnittswerkzeuge der Basis kommen ohne eigene Aufzählung.
        assert_eq!(
            entry.tools,
            ["fs.read", "fs.grep", "skills.search", "skills.load"]
        );
        assert!(entry.read_only);
        assert_eq!(entry.skills, ["planning"]);
        assert_eq!(entry.description.as_deref(), Some("Test-Worker note-taker"));
        assert_eq!(entry.max_depth, Some(GENERIC_WORKER_MAX_DEPTH));
        let analyst_budget = builtin[GENERIC_WORKER_BASE]
            .spawn_contract()
            .budget()
            .and_then(harw_agent_dsl::executable::BudgetSpec::max_tokens);
        assert_eq!(entry.budget_tokens, analyst_budget);
        assert_eq!(roster.base_role("note-taker"), Some(GENERIC_WORKER_BASE));
        let ir = roster
            .definitions()
            .get("note-taker")
            .ok_or(TestError::Missing("IR"))?;
        assert_eq!(
            ir.tool_surface().admitted(),
            ["fs.read", "fs.grep", "skills.search", "skills.load"]
        );
        assert_eq!(ir.skills(), ["planning"]);
        Ok(())
    }

    #[test]
    fn generic_writing_worker_keeps_fs_write_but_never_shell_or_network() -> TestResult {
        // Wie `synthesis-writer` der Analyse-Familie: nur `worker-base`,
        // schreibt in seinen Auftrag.
        let builtin = builtin()?;
        let (_home, config) = discover(&[(
            "report-writer",
            worker(
                "user.agent.report-writer@1",
                "report-writer",
                "harwness.agent.worker-base@1",
                r#""fs.read", "fs.write", "fs.edit", "shell.exec", "web.fetch", "skills.search""#,
            )
            .as_str(),
        )])?;
        let roster = AgentRoster::from_config(&builtin, &config).map_err(ctx("Roster"))?;
        let entry = roster
            .entry("report-writer")
            .ok_or(TestError::Missing("report-writer"))?;
        assert_eq!(entry.profile, GENERIC_WRITING_WORKER_PROFILE);
        assert_eq!(entry.base_role, GENERIC_WORKER_BASE);
        assert_eq!(
            entry.tools,
            [
                "fs.read",
                "fs.write",
                "fs.edit",
                "skills.search",
                "skills.load"
            ]
        );
        assert!(!entry.read_only);
        let wiring = roster.custom_wiring();
        let wired = wiring
            .get("report-writer")
            .ok_or(TestError::Missing("wiring"))?;
        assert_eq!(wired.profile, RegistryProfile::WorkspaceEdit);
        assert!(
            !RegistryProfile::WorkspaceEdit
                .tool_names()
                .contains(&"shell.exec")
        );
        assert!(
            !RegistryProfile::WorkspaceEdit
                .tool_names()
                .contains(&"web.fetch")
        );
        Ok(())
    }

    #[test]
    fn worker_extending_a_builtin_role_inherits_that_role_as_ceiling() -> TestResult {
        let builtin = builtin()?;
        let (_home, config) = discover(&[(
            "web-scout",
            r#"schema = "harwness.agent/v1"
id = "user.agent.web-scout@1"
version = "1.0.0"
extends = { id = "harwness.agent.explorer@1" }
role = "worker"
specialization = "web-scout"
"#,
        )])?;
        let roster = AgentRoster::from_config(&builtin, &config).map_err(ctx("Roster"))?;
        let entry = roster
            .entry("web-scout")
            .ok_or(TestError::Missing("web-scout"))?;
        assert_eq!(entry.base_role, role_names::EXPLORER);
        assert_eq!(entry.profile, RegistryProfile::ReadOnlyExplore);
        assert_eq!(
            entry.tools,
            builtin[role_names::EXPLORER].tool_surface().admitted()
        );
        Ok(())
    }

    /// Plan R9, Teil F: ein eigener Worker über `executor`, der nur
    /// `shell.exec` aufzählt, behält `job.*` (die Klemme streicht es nicht);
    /// ohne `shell.exec` gibt es kein `job.start`.
    #[test]
    fn worker_extending_executor_keeps_job_tools_next_to_shell_exec() -> TestResult {
        let builtin = builtin()?;
        let (_home, config) = discover(&[
            (
                "build-runner",
                worker(
                    "user.agent.build-runner@1",
                    "build-runner",
                    "harwness.agent.executor@1",
                    r#""shell.exec""#,
                )
                .as_str(),
            ),
            (
                "log-reader",
                worker(
                    "user.agent.log-reader@1",
                    "log-reader",
                    "harwness.agent.executor@1",
                    r#""skills.search""#,
                )
                .as_str(),
            ),
        ])?;
        let roster = AgentRoster::from_config(&builtin, &config).map_err(ctx("Roster"))?;
        let runner = roster
            .entry("build-runner")
            .ok_or(TestError::Missing("build-runner"))?;
        assert_eq!(runner.base_role, role_names::EXECUTOR);
        for tool in crate::profile::JOB_TOOLS {
            assert!(
                runner.tools.iter().any(|admitted| admitted == tool),
                "{tool}"
            );
        }
        let reader = roster
            .entry("log-reader")
            .ok_or(TestError::Missing("log-reader"))?;
        // Über einer eingebauten Rolle gewinnt deren `[tools]`-Abschnitt; die
        // Definition kann ihn nicht verengen — die Kontrollwerkzeuge bleiben.
        assert!(reader.tools.iter().any(|tool| tool == "job.status"));
        Ok(())
    }

    #[test]
    fn name_collision_with_a_builtin_role_is_refused() -> TestResult {
        let builtin = builtin()?;
        let (_home, config) = discover(&[(
            "fake-explorer",
            worker(
                "user.agent.fake-explorer@1",
                role_names::EXPLORER,
                "harwness.agent.worker-base@1",
                r#""fs.read", "fs.write""#,
            )
            .as_str(),
        )])?;
        let roster = AgentRoster::from_config(&builtin, &config).map_err(ctx("Roster"))?;
        let entry = roster
            .entry(role_names::EXPLORER)
            .ok_or(TestError::Missing("explorer"))?;
        assert_eq!(
            entry.source,
            RosterSource::BuiltIn,
            "die eingebaute Rolle gewinnt"
        );
        assert_eq!(roster.len(), role_names::ALL.len());
        assert!(roster.custom_bases().is_empty());
        Ok(())
    }

    #[test]
    fn custom_child_orchestrator_gets_the_planning_ceiling() -> TestResult {
        let builtin = builtin()?;
        let (_home, config) = discover(&[(
            "review-lead",
            r#"schema = "harwness.agent/v1"
id = "user.agent.review-lead@1"
version = "1.0.0"
extends = { id = "harwness.agent.child-orchestrator-base@1" }
role = "child-orchestrator"
specialization = "review-lead"

[delegation]
targets = ["explorer", "analyst"]
"#,
        )])?;
        let roster = AgentRoster::from_config(&builtin, &config).map_err(ctx("Roster"))?;
        let entry = roster
            .entry("review-lead")
            .ok_or(TestError::Missing("review-lead"))?;
        assert_eq!(entry.role, AgentRoleId::ChildOrchestrator);
        assert_eq!(entry.base_role, GENERIC_CHILD_ORCHESTRATOR_BASE);
        assert_eq!(entry.profile, RegistryProfile::Planning);
        assert_eq!(entry.max_depth, Some(1));
        assert!(entry.tools.iter().any(|tool| tool == "delegate_wave"));
        // Plan R9, Teil F: Job-Kontrolle der Basis, nie `job.start`.
        for tool in crate::profile::JOB_CONTROL_TOOLS {
            assert!(
                entry.tools.iter().any(|admitted| admitted == tool),
                "{tool}"
            );
        }
        assert!(!entry.tools.iter().any(|tool| tool == "job.start"));
        assert!(entry.read_only);
        assert_eq!(
            roster.delegation_targets("review-lead"),
            Some(vec!["explorer".to_owned(), "analyst".to_owned()])
        );
        assert_eq!(
            roster.delegation_targets(role_names::ANALYSIS_ORCHESTRATOR),
            crate::authority::delegation_targets_for_role(role_names::ANALYSIS_ORCHESTRATOR)
        );
        Ok(())
    }

    #[test]
    fn root_orchestrator_and_role_mismatch_definitions_are_not_spawnable() -> TestResult {
        let builtin = builtin()?;
        let (_home, config) = discover(&[
            (
                "own-root",
                r#"schema = "harwness.agent/v1"
id = "user.agent.own-root@1"
version = "1.0.0"
role = "root-orchestrator"
specialization = "own-root"
"#,
            ),
            (
                "fake-lead",
                // Worker-Rolle über der Orchestrator-Basis: keine passende Decke.
                r#"schema = "harwness.agent/v1"
id = "user.agent.fake-lead@1"
version = "1.0.0"
extends = { id = "harwness.agent.child-orchestrator-base@1" }
role = "worker"
specialization = "fake-lead"
"#,
            ),
        ])?;
        let roster = AgentRoster::from_config(&builtin, &config).map_err(ctx("Roster"))?;
        assert!(roster.entry("own-root").is_none());
        assert!(roster.entry("fake-lead").is_none());
        Ok(())
    }
}
