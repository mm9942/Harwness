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
//! - **Rechtedecke:** Reducer und Composition-Werkzeuge sind die der
//!   Basisrolle ([`AgentRoster::base_role`]). Die typisierte IR wird unter
//!   die der Basis geklemmt ([`AgentIr::clamped_to`]): Werkzeuge nur im
//!   Schnitt, Budget und `max_depth` höchstens die der Basis. Eine
//!   Definition kann ihre Rechte damit nur verengen, nie erweitern. Rechte
//!   über der Basis gibt es weiterhin nur über den Steward und
//!   `user_required`.
//! - **Aus der IR (#22 Welle 1B):** Instruktionen, Beschreibung,
//!   Delegationsziele (`[delegation].targets`, sonst die der Basisrolle) und
//!   das Rechte-Manifest kommen aus der geklemmten [`AgentIr`]
//!   ([`RosterEntry::ir`]); das Registry-Profil eines eigenen Agenten folgt
//!   aus Manifest plus Basisrolle ([`custom_registry_profile`]).
//!
//! # Konsumenten
//! `harw-runtime` registriert [`AgentRoster::names`] im Spawner und reicht
//! [`AgentRoster::definitions`] an die Kind-Fabriken. Welle 2 (Katalog,
//! `agents.delegate`) liest [`AgentRoster::entries`].
//!
//! # Nebenläufigkeit
//! Nach [`AgentRoster::build`] unveränderlich; `Send + Sync`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use harw_agent_dsl::ir_v2::{NetworkMode, Permissions};
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::roles::AgentRoleId;
use harw_agent_dsl::{AgentIr, ExecutableAgentIr};
use harw_authority::Permission;
use harw_config::AgentDefinitionMeta;
use time::OffsetDateTime;

use crate::embedded_agents::{
    BASE_DEFINITION_NAMES, CHILD_ORCHESTRATOR_BASE_NAME, WORKER_BASE_NAME, builtin_agent_irs,
    builtin_base_irs,
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
    /// Das Registry-Profil, mit dem die Kind-Registry montiert wird
    /// ([`custom_registry_profile`]).
    pub profile: RegistryProfile,
    /// Instruktionstext der IR (`instructions_file` bzw. `system.md`), falls
    /// nicht leer.
    pub instructions: Option<String>,
    /// Eigene `[delegation].targets` der IR, falls vorhanden; sonst gelten
    /// die der Basisrolle
    /// ([`crate::authority::delegation_targets_for_role`]).
    pub delegation_targets: Option<Vec<String>>,
    /// Die geklemmte, typisierte IR des Agenten (#22 Welle 1B).
    pub ir: Arc<AgentIr>,
}

/// Woher ein Roster-Eintrag stammt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterSource {
    /// Eine eingebaute Rolle aus [`role_names::ALL`].
    BuiltIn,
    /// Eine benutzerdefinierte Definition aus Profil oder vertrautem Projekt.
    Custom {
        /// Die kanonische `DefinitionId` (`ResolvedConfig::agent_irs`).
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
    /// Menschenlesbare Beschreibung aus der IR, falls vorhanden.
    pub description: Option<String>,
    /// Die fest gebundenen Skills der Definition.
    pub skills: Vec<String>,
    /// Die eingebaute Rolle, deren Reducer und Composition-Werkzeuge
    /// gelten. Für eingebaute Rollen der eigene Name.
    pub base_role: String,
    /// Das Registry-Profil: für eingebaute Rollen das ihrer Rollentabelle,
    /// für benutzerdefinierte aus Rechte-Manifest und Basisrolle
    /// ([`custom_registry_profile`]).
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
    /// Die (für benutzerdefinierte Agenten geklemmte) typisierte IR — Quelle
    /// von Instruktionen, Delegationszielen und Rechte-Manifest (#22 Welle 1B).
    pub ir: Arc<AgentIr>,
}

impl RosterEntry {
    /// Eine einzeilige Zusammenfassung der Rechte aus dem Rechte-Manifest der
    /// IR, z. B. `"read-only exploration agent; read-only; 18 tools"` (mit
    /// `"; network"`, wenn das Manifest Netzwerkzeuge admittiert).
    #[must_use]
    pub fn profile_summary(&self) -> String {
        format!(
            "{}; {}; {} tools{}",
            self.profile.role_description(),
            if self.read_only {
                "read-only"
            } else {
                "writes/executes"
            },
            self.tools.len(),
            if self.ir.permissions.network.mode == NetworkMode::Off {
                ""
            } else {
                "; network"
            }
        )
    }

    /// `true` für einen benutzerdefinierten Agenten.
    #[must_use]
    pub fn is_custom(&self) -> bool {
        matches!(self.source, RosterSource::Custom { .. })
    }

    /// Der nicht leere Instruktionstext der IR.
    #[must_use]
    pub fn instructions(&self) -> Option<&str> {
        let text = self.ir.instructions.text.as_str();
        (!text.trim().is_empty()).then_some(text)
    }
}

/// Der Roster aller startbaren Agenten eines Laufs.
#[derive(Debug, Clone, Default)]
pub struct AgentRoster {
    /// Einträge nach Spawn-Name.
    entries: BTreeMap<String, RosterEntry>,
    /// Die Laufzeitsicht der (für benutzerdefinierte Agenten geklemmten) IRs
    /// nach Spawn-Name, `ExecutableAgentIr::from(&entry.ir)`.
    definitions: HashMap<String, ExecutableAgentIr>,
    /// Instruktionstexte benutzerdefinierter Agenten nach Spawn-Name (aus
    /// der IR, für [`Self::all_instructions`]).
    instructions: HashMap<String, String>,
}

/// Welche Basis ein benutzerdefinierter Agent in seiner Kette trägt.
enum Ancestor<'a> {
    /// Eine eingebaute, startbare Rolle.
    Role(&'a str),
    /// Einer der Basis-Layer ([`BASE_DEFINITION_NAMES`]).
    Base(&'a str),
}

impl AgentRoster {
    /// Baut den Roster aus den eingebauten Rollen und der aufgelösten Config
    /// (Kompatibilitätsweg für Aufrufer mit der Laufzeitsicht).
    ///
    /// # Description
    /// `builtin` bestimmt nur, **welche** eingebauten Rollen aufgenommen
    /// werden; ihre typisierte IR wird über
    /// [`crate::embedded_agents::builtin_agent_irs`] gesenkt (die Sicht ist
    /// golden-geprüft gleich). Wer die IRs schon hat, nimmt
    /// [`Self::from_irs`].
    ///
    /// # Errors
    /// Wie [`Self::build`], dazu das Senken der eingebauten Rollen.
    pub fn from_config(
        builtin: &HashMap<String, ExecutableAgentIr>,
        config: &harw_config::ResolvedConfig,
    ) -> Result<Self, RegistryDefaultsError> {
        let irs: HashMap<String, AgentIr> = builtin_agent_irs(OffsetDateTime::now_utc())?
            .into_iter()
            .filter(|(name, _)| builtin.contains_key(name))
            .collect();
        Self::from_irs(&irs, config)
    }

    /// Baut den Roster aus den typisierten eingebauten Rollen
    /// ([`crate::embedded_agents::builtin_agent_irs`]) und der aufgelösten
    /// Config (`ResolvedConfig::agent_irs`, #22 Welle 1B).
    ///
    /// # Errors
    /// Wie [`Self::build`].
    pub fn from_irs(
        builtin: &HashMap<String, AgentIr>,
        config: &harw_config::ResolvedConfig,
    ) -> Result<Self, RegistryDefaultsError> {
        Self::build(builtin, &config.agent_irs, &config.agent_definition_meta)
    }

    /// Baut den Roster.
    ///
    /// # Arguments
    /// - `builtin`: die typisierten eingebauten Rollen
    ///   ([`crate::embedded_agents::builtin_agent_irs`]), nach Rollenname.
    /// - `local`: die typisierten Definitionen der vertrauten Layer
    ///   (`ResolvedConfig::agent_irs`), nach `DefinitionId`.
    /// - `meta`: Herkunftsschicht dazu (`ResolvedConfig::agent_definition_meta`),
    ///   nach `DefinitionId`. Beschreibung, Instruktionen und
    ///   Delegationsziele liest der Roster aus der IR.
    ///
    /// # Errors
    /// [`RegistryDefaultsError`], wenn eine eingebaute Basis nicht senkt
    /// (Defekt der eingebetteten Dateien). Eine ungeeignete
    /// benutzerdefinierte Definition ist nie ein Fehler, sondern wird mit
    /// Warnung übersprungen.
    pub fn build(
        builtin: &HashMap<String, AgentIr>,
        local: &HashMap<String, AgentIr>,
        meta: &HashMap<String, AgentDefinitionMeta>,
    ) -> Result<Self, RegistryDefaultsError> {
        let mut roster = Self::default();
        for (name, ir) in builtin {
            let Some(profile) = profile_for_role(name) else {
                tracing::warn!(role = %name, "registry.roster.builtin_role_without_profile");
                continue;
            };
            roster.insert(
                entry_for(
                    name,
                    Arc::new(ir.clone()),
                    name,
                    profile,
                    RosterSource::BuiltIn,
                ),
                false,
            );
        }
        if local.is_empty() {
            return Ok(roster);
        }

        let bases = builtin_base_irs(OffsetDateTime::now_utc())?;
        let builtin_by_id: HashMap<String, &str> = builtin
            .iter()
            .map(|(name, ir)| (ir.id.to_string(), name.as_str()))
            .collect();
        let base_by_id: HashMap<String, &str> = bases
            .iter()
            .map(|(name, ir)| (ir.id.to_string(), name.as_str()))
            .collect();

        let mut sorted: Vec<(&String, &AgentIr)> = local.iter().collect();
        sorted.sort_by_key(|(id, _)| *id);
        for (id, ir) in sorted {
            if !matches!(
                ir.role,
                AgentRoleId::Worker | AgentRoleId::UiaWorker | AgentRoleId::ChildOrchestrator
            ) {
                continue;
            }
            let name = ir.specialization.clone();
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
                    role = ?ir.role,
                    "registry.roster.no_matching_base_role: skipped"
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
                        .tools
                        .admitted
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
            // #22 Welle 1B: das Profil folgt dem Rechte-Manifest der
            // geklemmten IR plus der Basisrolle, nie weiter als die Basis.
            let Some(profile) = custom_registry_profile(base_role, &clamped.permissions, writes)
            else {
                tracing::warn!(
                    agent = %name,
                    base = %base_role,
                    "registry.roster.base_role_without_profile: skipped"
                );
                continue;
            };
            let entry = entry_for(
                &name,
                Arc::new(clamped),
                base_role,
                profile,
                RosterSource::Custom {
                    definition_id: id.clone(),
                    layer: meta.get(id).and_then(|meta| meta.layer),
                },
            );
            tracing::debug!(
                agent = %name,
                base = %base_role,
                definition = %id,
                profile = ?profile,
                "registry.roster.custom_agent_registered"
            );
            roster.insert(entry, true);
        }
        Ok(roster)
    }

    /// Nimmt einen Eintrag samt Laufzeitsicht (und bei eigenen Agenten
    /// Instruktionen) auf.
    fn insert(&mut self, entry: RosterEntry, custom: bool) {
        if custom && let Some(text) = entry.instructions() {
            self.instructions
                .insert(entry.name.clone(), text.to_owned());
        }
        self.definitions.insert(
            entry.name.clone(),
            ExecutableAgentIr::from(entry.ir.as_ref()),
        );
        self.entries.insert(entry.name.clone(), entry);
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

    /// Die Laufzeitsicht aller Einträge nach Spawn-Name — eingebaute
    /// unverändert, benutzerdefinierte geklemmt. Die Kind-Fabriken lesen
    /// daraus Aktivierung, Budget, Skills und Organisationsrolle.
    #[must_use]
    pub fn definitions(&self) -> &HashMap<String, ExecutableAgentIr> {
        &self.definitions
    }

    /// Die typisierte IR eines Eintrags (#22 Welle 1B).
    #[must_use]
    pub fn ir(&self, name: &str) -> Option<&Arc<AgentIr>> {
        self.entries.get(name).map(|entry| &entry.ir)
    }

    /// Die typisierten IRs aller Einträge nach Spawn-Name.
    #[must_use]
    pub fn irs(&self) -> HashMap<String, Arc<AgentIr>> {
        self.entries
            .iter()
            .map(|(name, entry)| (name.clone(), Arc::clone(&entry.ir)))
            .collect()
    }

    /// Die eingebaute Rolle, deren namensgebundene Tabellen (Reducer,
    /// Composition-Werkzeuge, Delegationsziele) für `name` gelten.
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

    /// Der Instruktionstext eines benutzerdefinierten Agenten (aus der IR),
    /// falls vorhanden.
    #[must_use]
    pub fn instructions(&self, name: &str) -> Option<&str> {
        self.instructions.get(name).map(String::as_str)
    }

    /// Die Delegationsziele eines Eintrags: die `[delegation].targets` seiner
    /// IR, sonst die seiner Basisrolle bzw. der eingebauten Rolle selbst.
    /// `None` heißt: keine Deklaration.
    #[must_use]
    pub fn delegation_targets(&self, name: &str) -> Option<Vec<String>> {
        let entry = self.entries.get(name)?;
        if let Some(targets) = &entry.ir.spawn.delegation_targets {
            return Some(targets.clone());
        }
        crate::authority::delegation_targets_for_role(&entry.base_role)
    }

    /// Alle Instruktionstexte benutzerdefinierter Agenten nach Spawn-Name.
    #[must_use]
    pub fn all_instructions(&self) -> &HashMap<String, String> {
        &self.instructions
    }

    /// Die Laufzeit-Verdrahtung aller benutzerdefinierten Agenten nach
    /// Spawn-Name (Basisrolle, Profil, Instruktionen, IR).
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
                        instructions: entry.instructions().map(str::to_owned),
                        delegation_targets: entry.ir.spawn.delegation_targets.clone(),
                        ir: Arc::clone(&entry.ir),
                    },
                )
            })
            .collect()
    }
}

/// Das Registry-Profil eines benutzerdefinierten Agenten aus seinem
/// Rechte-Manifest und seiner Basisrolle (#22 Welle 1B).
///
/// # Description
/// Ausgangspunkt ist das Profil der Basisrolle (bzw. für einen generischen
/// schreibenden Worker [`GENERIC_WRITING_WORKER_PROFILE`]); das Manifest
/// kann es nur **verengen**, nie erweitern:
/// - Nutzt das Manifest kein einziges Werkzeug des Basisprofils und
///   verlangt weder Lesen, Schreiben, Shell, Host noch Netz, gilt
///   [`RegistryProfile::NoTools`] — die Kind-Registry montiert dann keinen
///   Werkzeug-Provider, dessen Rechte der Agent nicht braucht.
/// - Sonst bleibt es beim Basisprofil; die Aktivierung der Sitzung schaltet
///   ohnehin nur die admittierten Werkzeuge frei, und die Rechte der
///   Kind-Registry werden zusätzlich auf das Manifest geschnitten
///   ([`crate::authority::granted_for_ir`]).
///
/// Eingebaute Rollen nutzen weiterhin ausschließlich [`profile_for_role`]
/// (keine Verhaltensänderung); der Test
/// `builtin_manifests_stay_within_their_registry_profiles` hält fest, dass
/// ihr Manifest innerhalb dessen liegt, was ihr Profil gewährt.
///
/// # Returns
/// `None`, wenn die Basisrolle kein Profil hat.
#[must_use]
pub fn custom_registry_profile(
    base_role: &str,
    permissions: &Permissions,
    generic_writer: bool,
) -> Option<RegistryProfile> {
    let base = if generic_writer {
        GENERIC_WRITING_WORKER_PROFILE
    } else {
        profile_for_role(base_role)?
    };
    let base_tools = base.tool_names();
    let uses_profile_tool = permissions
        .tools
        .iter()
        .any(|tool| base_tools.contains(&tool.as_str()));
    let needs_rights = permissions.filesystem.read
        || permissions.filesystem.write
        || permissions.shell
        || permissions.host
        || permissions.network.mode != NetworkMode::Off;
    if !uses_profile_tool && !needs_rights {
        return Some(RegistryProfile::NoTools);
    }
    Some(base)
}

/// Die Rechtedecke eines benutzerdefinierten Agenten.
struct Ceiling<'a> {
    /// Eingebaute Rolle für namensgebundene Tabellen.
    base_role: &'a str,
    /// Die IR, unter die geklemmt wird.
    ir: &'a AgentIr,
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
fn job_companion_tools<'a>(clamped: &AgentIr, ceiling: &'a AgentIr) -> Vec<&'a str> {
    let admits_shell = clamped
        .tools
        .admitted
        .iter()
        .any(|tool| tool == "shell.exec");
    ceiling
        .tools
        .admitted
        .iter()
        .map(String::as_str)
        .filter(|tool| crate::profile::JOB_TOOLS.contains(tool))
        .filter(|tool| admits_shell || *tool != harw_tool_job::JOB_START_TOOL)
        .collect()
}

/// Bestimmt Basisrolle, Rechtedecke und optionale Tiefenobergrenze eines
/// benutzerdefinierten Agenten. `None`, wenn keine passende Basis existiert.
fn ceiling_for<'a>(
    ir: &AgentIr,
    builtin_by_id: &HashMap<String, &'a str>,
    base_by_id: &HashMap<String, &'a str>,
    builtin: &'a HashMap<String, AgentIr>,
    bases: &'a HashMap<String, AgentIr>,
) -> Option<Ceiling<'a>> {
    let ancestor = ir
        .trace
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
    let role = ir.role;
    match ancestor {
        Some(Ancestor::Role(name)) => {
            let ceiling = builtin.get(name)?;
            (ceiling.role == role).then_some(Ceiling {
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
/// Ein generischer Worker, dessen Rechte-Manifest Schreibzugriff trägt
/// (`fs.write`/`fs.edit` admittiert, nicht verboten), bekommt die Decke eines
/// schreibenden Workers ([`GENERIC_WRITING_WORKER_PROFILE`] plus
/// [`GENERIC_WORKER_WRITE_TOOLS`] über der Analyst-Decke); Shell und Netz
/// bleiben draußen.
fn generic_ceiling<'a>(
    ir: &AgentIr,
    builtin: &'a HashMap<String, AgentIr>,
    bases: &'a HashMap<String, AgentIr>,
) -> Option<Ceiling<'a>> {
    match ir.role {
        AgentRoleId::Worker => Some(Ceiling {
            base_role: GENERIC_WORKER_BASE,
            ir: builtin.get(GENERIC_WORKER_BASE)?,
            depth_cap: Some(GENERIC_WORKER_MAX_DEPTH),
            writes: ir.permissions.filesystem.write,
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
    ir: Arc<AgentIr>,
    base_role: &str,
    profile: RegistryProfile,
    source: RosterSource,
) -> RosterEntry {
    let tools = ir.tools.admitted.clone();
    // Registry-genau über `tool_permission`, zusätzlich jedes schreibende
    // oder ausführende Merkmal des Rechte-Manifests (#22 Welle 1B).
    let writes_by_tool = tools.iter().any(|tool| {
        matches!(
            crate::authority::tool_permission(tool),
            Some(Permission::WriteWorkspace | Permission::ExecuteProcess)
        )
    });
    let permissions = &ir.permissions;
    let writes_by_manifest = permissions.filesystem.write
        || permissions.shell
        || permissions.host
        || !permissions.filesystem.other_write_tools.is_empty();
    RosterEntry {
        name: name.to_owned(),
        role: ir.role,
        description: ir.description.clone(),
        skills: ir.skill_names(),
        base_role: base_role.to_owned(),
        profile,
        tools,
        read_only: !(writes_by_tool || writes_by_manifest),
        max_depth: ir.spawn.max_depth,
        budget_tokens: ir
            .spawn
            .budget
            .as_ref()
            .and_then(|budget| budget.max_tokens),
        source,
        ir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn builtin() -> TestResult<HashMap<String, AgentIr>> {
        builtin_agent_irs(OffsetDateTime::now_utc()).map_err(ctx("eingebaute Rollen senken"))
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
        let roster = AgentRoster::from_irs(&builtin, &config).map_err(ctx("Roster"))?;
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
            .spawn
            .budget
            .as_ref()
            .and_then(|budget| budget.max_tokens);
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
        let roster = AgentRoster::from_irs(&builtin, &config).map_err(ctx("Roster"))?;
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
        let roster = AgentRoster::from_irs(&builtin, &config).map_err(ctx("Roster"))?;
        let entry = roster
            .entry("web-scout")
            .ok_or(TestError::Missing("web-scout"))?;
        assert_eq!(entry.base_role, role_names::EXPLORER);
        assert_eq!(entry.profile, RegistryProfile::ReadOnlyExplore);
        assert_eq!(entry.tools, builtin[role_names::EXPLORER].tools.admitted);
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
        let roster = AgentRoster::from_irs(&builtin, &config).map_err(ctx("Roster"))?;
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
        let roster = AgentRoster::from_irs(&builtin, &config).map_err(ctx("Roster"))?;
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
        let roster = AgentRoster::from_irs(&builtin, &config).map_err(ctx("Roster"))?;
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
        let roster = AgentRoster::from_irs(&builtin, &config).map_err(ctx("Roster"))?;
        assert!(roster.entry("own-root").is_none());
        assert!(roster.entry("fake-lead").is_none());
        Ok(())
    }

    /// #22 Welle 1B: Instruktionen, Beschreibung und Rechte-Manifest eines
    /// eigenen Agenten stammen aus seiner (geklemmten) IR; `system.md`
    /// neben der Definition wird ohne `instructions_file` gefunden.
    #[test]
    fn custom_agent_entry_carries_the_clamped_ir_and_its_instructions() -> TestResult {
        let builtin = builtin()?;
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = home.path().join("agents").join("note-taker");
        std::fs::create_dir_all(&dir).map_err(ctx("Agentenordner"))?;
        std::fs::write(
            dir.join("definition.toml"),
            worker(
                "user.agent.note-taker@1",
                "note-taker",
                "harwness.agent.worker-base@1",
                r#""fs.read", "fs.grep", "shell.exec""#,
            ),
        )
        .map_err(ctx("definition.toml"))?;
        std::fs::write(dir.join("system.md"), "Schreibe knappe Notizen.")
            .map_err(ctx("system.md"))?;
        let config =
            harw_config::discover_config(&[home.path().to_path_buf()]).map_err(ctx("Discovery"))?;
        let roster = AgentRoster::from_irs(&builtin, &config).map_err(ctx("Roster"))?;
        let entry = roster
            .entry("note-taker")
            .ok_or(TestError::Missing("note-taker"))?;
        assert_eq!(entry.instructions(), Some("Schreibe knappe Notizen."));
        assert_eq!(
            roster.instructions("note-taker"),
            Some("Schreibe knappe Notizen.")
        );
        assert_eq!(entry.ir.tools.admitted, entry.tools);
        assert!(
            !entry.ir.permissions.shell,
            "shell.exec liegt über der Decke"
        );
        assert!(entry.ir.verify_snapshot());
        assert_eq!(
            roster
                .definitions()
                .get("note-taker")
                .map(ExecutableAgentIr::snapshot_id),
            Some(ExecutableAgentIr::from(entry.ir.as_ref()).snapshot_id())
        );
        let wiring = roster.custom_wiring();
        let wired = wiring
            .get("note-taker")
            .ok_or(TestError::Missing("wiring"))?;
        assert_eq!(
            wired.instructions.as_deref(),
            Some("Schreibe knappe Notizen.")
        );
        assert!(entry.profile_summary().contains("read-only"));
        Ok(())
    }

    /// #22 Welle 1B: ein eigener Worker, dessen Manifest kein Werkzeug des
    /// Basisprofils und kein Recht braucht, bekommt `NoTools` statt des
    /// Analyst-Profils — das Profil folgt dem Manifest, nie weiter als die
    /// Basis.
    #[test]
    fn custom_profile_follows_the_manifest_and_never_widens() -> TestResult {
        let builtin = builtin()?;
        let (_home, config) = discover(&[(
            "skill-reader",
            worker(
                "user.agent.skill-reader@1",
                "skill-reader",
                "harwness.agent.worker-base@1",
                r#""skills.search""#,
            )
            .as_str(),
        )])?;
        let roster = AgentRoster::from_irs(&builtin, &config).map_err(ctx("Roster"))?;
        let entry = roster
            .entry("skill-reader")
            .ok_or(TestError::Missing("skill-reader"))?;
        assert_eq!(entry.profile, RegistryProfile::NoTools);
        assert!(
            RegistryProfile::NoTools
                .required_permissions()
                .is_subset_of(
                    &profile_for_role(GENERIC_WORKER_BASE)
                        .ok_or(TestError::Missing("analyst profile"))?
                        .required_permissions()
                )
        );

        let reader = builtin
            .get(role_names::EXPLORER)
            .ok_or(TestError::Missing("explorer"))?;
        assert_eq!(
            custom_registry_profile(role_names::EXPLORER, &reader.permissions, false),
            Some(RegistryProfile::ReadOnlyExplore)
        );
        assert_eq!(
            custom_registry_profile("unbekannt", &reader.permissions, false),
            None
        );
        Ok(())
    }

    /// #22 Welle 1B, Gegenprobe: das Rechte-Manifest jeder eingebauten Rolle
    /// liegt innerhalb dessen, was ihr Registry-Profil (plus die
    /// rollengebundenen Composition-Werkzeuge) gewährt. Eine Abweichung
    /// zwischen IR und Rollentabelle wird hier sichtbar.
    #[test]
    fn builtin_manifests_stay_within_their_registry_profiles() -> TestResult {
        use crate::profile::{
            child_message_tools_for_role, child_result_tools_for_role, composition_tools_for_role,
            job_control_tools_for_role, knowledge_tools_for_role, matrix_tools_for_role,
            parent_message_tools_for_role, skill_catalog_tools_for_role, sudo_tools_for_role,
        };
        let builtin = builtin()?;
        assert_eq!(builtin.len(), role_names::ALL.len());
        for (name, ir) in &builtin {
            let profile = profile_for_role(name)
                .ok_or_else(|| TestError::Unexpected(format!("{name}: kein Profil")))?;
            let surface: Vec<&str> = profile
                .tool_names()
                .into_iter()
                .chain(composition_tools_for_role(name).iter().copied())
                .chain(knowledge_tools_for_role(name).iter().copied())
                .chain(sudo_tools_for_role(name).iter().copied())
                .chain(child_result_tools_for_role(name).iter().copied())
                .chain(child_message_tools_for_role(name).iter().copied())
                .chain(parent_message_tools_for_role(name).iter().copied())
                .chain(matrix_tools_for_role(name).iter().copied())
                .chain(skill_catalog_tools_for_role(name).iter().copied())
                .chain(job_control_tools_for_role(name).iter().copied())
                .collect();
            let granted = crate::authority::permissions_of(&surface);
            let manifest = &ir.permissions;
            for tool in &manifest.tools {
                assert!(
                    surface.contains(&tool.as_str()),
                    "{name}: Manifest-Werkzeug {tool} fehlt in {profile:?}"
                );
            }
            if manifest.filesystem.write {
                assert!(
                    granted.contains(Permission::WriteWorkspace),
                    "{name}: write"
                );
            }
            if manifest.shell || manifest.host {
                assert!(
                    granted.contains(Permission::ExecuteProcess),
                    "{name}: shell/host"
                );
            }
            if manifest.network.mode != NetworkMode::Off {
                assert!(
                    granted.contains(Permission::NetworkAccess),
                    "{name}: network"
                );
            }
            if manifest.filesystem.read {
                assert!(
                    granted.contains(Permission::ReadWorkspace)
                        || granted.contains(Permission::ReadCargoRegistry),
                    "{name}: read"
                );
            }
        }
        Ok(())
    }
}
