//! Parsen, Auflösen und Senken der DSL-Agentendefinitionen einer
//! aufgelösten Konfiguration (Plan R9, Teil B; #22 Welle 1B).
//!
//! # Warum hier
//! `harw-config` liegt in der Infrastruktur-Schicht (I) und darf die
//! Agenten-DSL (`harw-agent-dsl`, Schicht C) nicht kennen
//! (`xtask/arch-policy.toml`). Die Discovery reicht die gefundenen
//! Definitionen deshalb **ungeparst** weiter
//! ([`harw_config::ResolvedConfig::agent_sources`]); dieses Modul (Schicht A)
//! macht daraus die typisierte IR — mit genau den Regeln, die früher in
//! `harw_config::discovery` galten:
//!
//! - Eine nicht parsbare `agents/<name>/definition.toml` ist ein harter
//!   Fehler (fail-closed); eine nicht parsbare flache Altdatei
//!   `agents/<name>.toml` wird mit Warnung übersprungen, ebenso eine flache
//!   Datei, deren ID im selben Layer schon als Verzeichnisdefinition
//!   vorliegt.
//! - Jede Definition wird über den eingebauten Definitionen auf
//!   [`DefinitionLayer::BuiltIn`] aufgelöst (damit `extends` auf
//!   `harwness.agent.worker-base@1` & Co. auflöst) und mit `lower_v2` über
//!   Instruktionsdateien und Kontextprogramm-Bibliothek gesenkt.
//! - `active_agent_definition`/`active_uia_definition` werden gegen die
//!   gesenkten Definitionen geprüft ([`ConfigAgents::validate_selection`]).
//!
//! # Eingebaute Schicht
//! Die eingebauten Definitionen kommen aus demselben eingebetteten Baum wie
//! [`crate::embedded_agents`] (`agents/`), gesammelt mit der
//! „Verzeichniskonvention“: jede `.toml`-Datei außer unter den reservierten
//! Wurzeln `family/`, `families/`, `organization/` und `context-programs/`.
//! Sie dienen nur als `extends`-/Mixin-Ziele. Welche Rolle tatsächlich
//! startbar ist und mit welchen Rechten, entscheiden weiterhin
//! [`crate::embedded_agents::builtin_agent_definitions`] und
//! [`crate::roster::AgentRoster`].
//!
//! # Nebenläufigkeit
//! Die eingebaute Schicht wird höchstens einmal pro Prozess geparst
//! (`OnceLock`); danach reine, threadsichere Sicht.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use harw_agent_dsl::bind::ContextProgramLibrary;
use harw_agent_dsl::diagnostics::{Severity, SourceFile};
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::lower_v2::{
    FsInstructionsLoader, LowerSources, diagnostic_for_error, lower_v2,
};
use harw_agent_dsl::parse::parse_toml;
use harw_agent_dsl::raw::RawAgentDefinition;
use harw_agent_dsl::resolve::resolve_definition;
use harw_agent_dsl::roles::AgentRoleId;
use harw_agent_dsl::{AgentIr, ExecutableAgentIr};
use harw_config::{
    AgentDefinitionSources, AgentSourceForm, AgentSourceLayer, CONTEXT_PROGRAMS_DIR, ConfigError,
    ConfigResult, ContextProgramSource, HarnessConfig, ResolvedConfig,
};
use include_dir::{Dir, DirEntry};
use time::OffsetDateTime;

use crate::embedded_agents::{AGENTS_DIR, NON_ROLE_ROOT_DIRS};

// ─── Eingebaute Schicht ─────────────────────────────────────────────────────

/// Cache für [`builtin_definition_layers`].
static BUILTIN_LAYERS: OnceLock<Vec<RawAgentDefinition>> = OnceLock::new();

/// Cache für [`builtin_source_files`].
static BUILTIN_SOURCE_FILES: OnceLock<Vec<(SourceFile, Option<String>)>> = OnceLock::new();

/// Sammelt rekursiv jede `.toml`-Datei unter `dir`.
fn collect(dir: &'static Dir<'static>, out: &mut Vec<(&'static str, &'static str)>) {
    for entry in dir.entries() {
        match entry {
            DirEntry::Dir(sub) => collect(sub, out),
            DirEntry::File(file) => {
                let Some(path) = file.path().to_str() else {
                    continue;
                };
                if !path.ends_with(".toml") {
                    continue;
                }
                if let Some(contents) = file.contents_utf8() {
                    out.push((path, contents));
                }
            }
        }
    }
}

/// Die Rohquellen aller eingebetteten Agentendefinitionen als
/// `(relativer Pfad, TOML)`, nach Pfad sortiert.
fn builtin_agent_sources() -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    for entry in AGENTS_DIR.entries() {
        match entry {
            DirEntry::File(file) => {
                let Some(path) = file.path().to_str() else {
                    continue;
                };
                if !path.ends_with(".toml") {
                    continue;
                }
                if let Some(contents) = file.contents_utf8() {
                    out.push((path, contents));
                }
            }
            DirEntry::Dir(sub) => {
                let reserved = sub
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| NON_ROLE_ROOT_DIRS.contains(&name));
                if !reserved {
                    collect(sub, &mut out);
                }
            }
        }
    }
    out.sort_by_key(|(path, _)| *path);
    out
}

/// Die geparsten eingebauten Agentendefinitionen (Basen und Rollen), bereit
/// als [`DefinitionLayer::BuiltIn`]-Schicht.
///
/// # Description
/// Eine Datei, die nicht parst, wird mit einer Warnung übersprungen: sie ist
/// ein Defekt der eingebauten Schicht, den
/// [`crate::embedded_agents::builtin_agent_definitions`] ohnehin hart meldet.
/// Das Senken der Nutzerdefinitionen darf daran nicht scheitern, solange
/// keine sie erweitert (dann meldet die Auflösung `MissingBase` mit dem
/// Namen).
fn builtin_definition_layers() -> &'static [RawAgentDefinition] {
    BUILTIN_LAYERS
        .get_or_init(|| {
            builtin_agent_sources()
                .into_iter()
                .filter_map(|(path, source)| match parse_toml(source) {
                    Ok(raw) => Some(raw),
                    Err(error) => {
                        tracing::warn!(
                            path,
                            %error,
                            "config.builtin_agent_definition.unparsable"
                        );
                        None
                    }
                })
                .collect()
        })
        .as_slice()
}

/// Die eingebauten Definitionen als Quelldateien für `lower_v2`, jeweils mit
/// der deklarierten `id` (einmal je Prozess ermittelt).
///
/// # Description
/// Der Pfad ist `builtin/<relativer Pfad>` — ein Etikett für Spannen in
/// Diagnosen, kein Dateisystempfad. Alle Dateien liegen auf
/// [`DefinitionLayer::BuiltIn`].
fn builtin_source_files() -> &'static [(SourceFile, Option<String>)] {
    BUILTIN_SOURCE_FILES
        .get_or_init(|| {
            builtin_agent_sources()
                .into_iter()
                .map(|(path, source)| {
                    let file = SourceFile::new(
                        DefinitionLayer::BuiltIn,
                        format!("builtin/{path}"),
                        source,
                    );
                    let id = file.declared_id();
                    (file, id)
                })
                .collect()
        })
        .as_slice()
}

/// Die eingebauten Kontextprogramme als `(Name, TOML)`: jede `.toml`-Datei
/// direkt unter `agents/context-programs/`, Name ist der Dateistamm, nach
/// Namen sortiert.
fn builtin_context_program_sources() -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if let Some(dir) = AGENTS_DIR.get_dir(CONTEXT_PROGRAMS_DIR) {
        for entry in dir.entries() {
            let DirEntry::File(file) = entry else {
                continue;
            };
            let path = file.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }
            let (Some(stem), Some(contents)) = (
                path.file_stem().and_then(|stem| stem.to_str()),
                file.contents_utf8(),
            ) else {
                continue;
            };
            out.push((stem, contents));
        }
    }
    out.sort_by_key(|(name, _)| *name);
    out
}

/// Die Kontextprogramm-Bibliothek der eingebauten Schicht: alle eingebauten
/// Programme auf [`DefinitionLayer::BuiltIn`]. Eigene Programme der Layer
/// fügt [`context_program_library`] hinzu.
///
/// # Errors
/// Die Meldung des DSL-Crates, wenn ein eingebautes Programm nicht parst
/// (ein Defekt der eingebetteten Dateien).
fn builtin_context_program_library() -> Result<ContextProgramLibrary, String> {
    ContextProgramLibrary::from_builtin_sources(&builtin_context_program_sources())
        .map_err(|error| error.to_string())
}

// ─── Gesenkte Definitionen der Konfiguration ───────────────────────────────

/// Nicht ausführbare Angaben einer aufgelösten DSL-Agentendefinition
/// (Plan R9, Teil B).
///
/// # Description
/// Seit #22 Welle 1B eine dünne Sicht auf die typisierte IR
/// ([`ConfigAgents::agent_irs`]): Name, Beschreibung, Instruktionen und
/// Delegationsziele werden aus der [`AgentIr`] übernommen; nur die
/// Herkunftsschicht kommt aus der Discovery selbst.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentDefinitionMeta {
    /// Optionaler Anzeigename (`name = "..."`).
    pub name: Option<String>,
    /// Optionale Beschreibung (`description = "..."`).
    pub description: Option<String>,
    /// Die höchste Schicht, aus der die Definition stammt.
    pub layer: Option<DefinitionLayer>,
    /// Instruktionstext der IR (`instructions_file`, relativ zum
    /// Agentenordner der Definition, die das Feld setzt, sonst ein
    /// `system.md` neben der Definition), falls nicht leer.
    pub instructions: Option<String>,
    /// `[delegation].targets` der aufgelösten Definition, falls vorhanden —
    /// die namentliche Zielliste eines Orchestrators (ein Filter über seine
    /// sichtbaren Ziele, nie eine Erweiterung).
    pub delegation_targets: Option<Vec<String>>,
}

impl AgentDefinitionMeta {
    /// Die Sicht auf eine [`AgentIr`] (#22 Welle 1B): Name, Beschreibung,
    /// nicht leerer Instruktionstext und `[delegation].targets` aus der IR,
    /// dazu die Herkunftsschicht.
    #[must_use]
    pub fn from_ir(ir: &AgentIr, layer: Option<DefinitionLayer>) -> Self {
        let text = ir.instructions.text.as_str();
        Self {
            name: ir.name.clone(),
            description: ir.description.clone(),
            layer,
            instructions: (!text.trim().is_empty()).then(|| text.to_owned()),
            delegation_targets: ir.spawn.delegation_targets.clone(),
        }
    }
}

/// Die gesenkten DSL-Agentendefinitionen einer Konfiguration, alle nach
/// ihrer kanonischen `DefinitionId` geschlüsselt.
///
/// # Description
/// Früher Felder von `harw_config::ResolvedConfig`; seit die Konfiguration
/// die Definitionen ungeparst weiterreicht, baut sie
/// [`ConfigAgents::from_config`] (bzw. [`ConfigAgents::lower`]) aus
/// [`ResolvedConfig::agent_sources`]. Ein eingebetteter Lauf (`harw-runtime`,
/// `load_config_embedded`) füllt die Felder direkt aus seinen IRs.
#[derive(Debug, Clone, Default)]
pub struct ConfigAgents {
    /// Die Laufzeitsicht jeder Definition (`ExecutableAgentIr::from(&ir)`
    /// der [`Self::agent_irs`]).
    pub executable_agents: HashMap<String, ExecutableAgentIr>,
    /// Die typisierte Agent-IR v2 jeder Definition (#22 Welle 1B), gesenkt
    /// mit `harw_agent_dsl::lower_v2::lower_v2` über den eingebauten
    /// Definitionen, den Instruktionsdateien der Agentenordner und der
    /// Kontextprogramm-Bibliothek (eingebaute Programme plus
    /// `agents/context-programs/*.toml` der vertrauten Layer).
    pub agent_irs: HashMap<String, AgentIr>,
    /// Agentenordner der final aufgelösten Definitionen. Der Runtime-Pfad
    /// verwendet ihn ausschließlich für die optionalen, benutzerpflegbaren
    /// UIA-Dateien `Personality.md` und `USER.md` sowie das UIA-Gedächtnis.
    pub agent_definition_dirs: HashMap<String, PathBuf>,
    /// Menschenlesbare Angaben und Instruktionstext der Definitionen. Die
    /// ausführbare IR trägt weder Beschreibung noch Instruktionen; der Roster
    /// ([`crate::roster`]) liest sie hier.
    pub agent_definition_meta: HashMap<String, AgentDefinitionMeta>,
}

impl ConfigAgents {
    /// Senkt die Agentendefinitionen einer aufgelösten Konfiguration
    /// ([`ResolvedConfig::agent_sources`]).
    ///
    /// # Errors
    /// Wie [`Self::lower`].
    pub fn from_config(config: &ResolvedConfig) -> ConfigResult<Self> {
        Self::lower(&config.agent_sources)
    }

    /// Wie [`Self::from_config`], prüft danach die gewählten Definitionen
    /// ([`Self::validate_selection`]) — zusammen genau die Prüfungen, die
    /// `discover_config` und `ResolvedConfig::validate` früher für
    /// Agentendefinitionen ausführten.
    ///
    /// # Errors
    /// Wie [`Self::lower`] und [`Self::validate_selection`].
    pub fn from_config_validated(config: &ResolvedConfig) -> ConfigResult<Self> {
        let agents = Self::from_config(config)?;
        agents.validate_selection(&config.harness)?;
        Ok(agents)
    }

    /// Parst, löst auf und senkt die Rohquellen.
    ///
    /// # Description
    /// Jede Definitions-ID wird über den eingebauten Definitionen und allen
    /// gefundenen Definitionen aufgelöst (siehe
    /// `resolve_discovered_definition`). Ohne Definitionen bleibt das
    /// Ergebnis leer; die eigenen Kontextprogramme werden dann nicht einmal
    /// geparst.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] mit den gerenderten Diagnosen (Code,
    /// `datei:zeile:spalte`, Hilfetext), wenn eine Verzeichnisdefinition nicht
    /// parst, ein Kontextprogramm nicht parst oder einen Namen doppelt
    /// vergibt, oder Auflösung bzw. Senken scheitern.
    pub fn lower(sources: &AgentDefinitionSources) -> ConfigResult<Self> {
        let definition_layers = parse_definition_sources(sources)?;
        let mut agents = Self::default();
        if definition_layers.is_empty() {
            return Ok(agents);
        }
        let library = context_program_library(&sources.context_programs)?;
        let stack = DefinitionStack::new(&definition_layers);
        for (id, definitions) in &definition_layers {
            let (ir, meta, definition_dir) =
                resolve_discovered_definition(id, definitions, &stack, &library)?;
            agents
                .agent_definition_dirs
                .insert(id.clone(), definition_dir);
            agents.agent_definition_meta.insert(id.clone(), meta);
            agents
                .executable_agents
                .insert(id.clone(), ExecutableAgentIr::from(&ir));
            agents.agent_irs.insert(id.clone(), ir);
        }
        Ok(agents)
    }

    /// Prüft `active_agent_definition` und `active_uia_definition` gegen die
    /// gesenkten Definitionen.
    ///
    /// # Description
    /// Nutzer-Definitionen tragen eine versionierte Id (`…@N`) und müssen
    /// hier auflösbar sein. Schlichte Namen (z. B. `root-orchestrator`,
    /// gesetzt per `/agent use`) bezeichnen mitgelieferte Rollen; sie prüft
    /// die Runtime beim Start (`resolve_active_agent`, fail-closed). Eine
    /// gewählte UIA muss existieren und die Rolle `user-interface` tragen.
    ///
    /// # Errors
    /// - [`ConfigError::UnresolvedRef`] (`kind` = `"agent definition"` bzw.
    ///   `"UIA definition"`), wenn die Auswahl nicht auflöst.
    /// - [`ConfigError::Invalid`], wenn die UIA eine andere Rolle trägt.
    pub fn validate_selection(&self, harness: &HarnessConfig) -> ConfigResult<()> {
        if let Some(definition) = &harness.active_agent_definition {
            let versioned_or_known =
                definition.contains('@') || self.executable_agents.contains_key(definition);
            if versioned_or_known && !self.executable_agents.contains_key(definition) {
                return Err(ConfigError::UnresolvedRef {
                    kind: "agent definition".to_owned(),
                    reference: definition.clone(),
                });
            }
        }
        if let Some(definition) = &harness.active_uia_definition {
            let Some(ir) = self.executable_agents.get(definition) else {
                return Err(ConfigError::UnresolvedRef {
                    kind: "UIA definition".to_owned(),
                    reference: definition.clone(),
                });
            };
            let role = ir.role();
            if role != AgentRoleId::UserInterface {
                return Err(ConfigError::Invalid(format!(
                    "UIA definition '{definition}' must have role 'user-interface', found '{role:?}'"
                )));
            }
        }
        Ok(())
    }
}

/// Findet und senkt die Definitionen eines einzelnen Laufs (`scope = "run"`,
/// Plan R9, Teil B).
///
/// # Description
/// Liest die Rohquellen über [`harw_config::discover_run_agent_sources`]
/// (`<projekt>/.harw/state/runs/<run_id>/agents/…`) und löst sie als
/// [`DefinitionLayer::RunLocal`] über den eingebauten Definitionen auf.
///
/// # Grenze
/// Die Laufzeit kennt heute keine Lauf-ID, die sie dem schreibenden Modell
/// vorgibt: `run_id` ist ein frei gewählter Slug des Aufrufers. Diese
/// Funktion ist deshalb der dokumentierte Einstieg für einen Aufrufer, der
/// eine Lauf-ID besitzt; die Montage (`harw-runtime`) ruft sie noch nicht auf.
///
/// # Arguments
/// - `project_harw_dir` (`&Path`): das `.harw`-Verzeichnis des Projekts.
/// - `run_id` (`&str`): die Lauf-ID; nur `[a-z0-9-]`, sonst leer.
///
/// # Returns
/// Die gesenkten Definitionen nach `DefinitionId`, dazu ihre
/// [`AgentDefinitionMeta`]. Leer, wenn es das Verzeichnis nicht gibt.
///
/// # Errors
/// Lesefehler sowie alles, was [`ConfigAgents::lower`] meldet.
pub fn discover_run_agent_definitions(
    project_harw_dir: &Path,
    run_id: &str,
) -> ConfigResult<HashMap<String, (ExecutableAgentIr, AgentDefinitionMeta)>> {
    let sources = harw_config::discover_run_agent_sources(project_harw_dir, run_id)?;
    let mut agents = ConfigAgents::lower(&sources)?;
    Ok(agents
        .executable_agents
        .into_iter()
        .map(|(id, executable)| {
            let meta = agents.agent_definition_meta.remove(&id).unwrap_or_default();
            (id, (executable, meta))
        })
        .collect())
}

/// Die DSL-Schicht einer Rohquelle.
fn definition_layer(layer: AgentSourceLayer) -> DefinitionLayer {
    match layer {
        AgentSourceLayer::UserGlobal => DefinitionLayer::UserGlobal,
        AgentSourceLayer::Project => DefinitionLayer::Project,
        AgentSourceLayer::RunLocal => DefinitionLayer::RunLocal,
    }
}

/// Die geparsten Quellen einer Definitions-ID: Schicht, Rohform, Pfad und
/// Quelltext (der Quelltext speist Spannen und eigene Tabellen des
/// IR-v2-Senkens, #22 Welle 1B).
type DiscoveredDefinitionLayers = Vec<(DefinitionLayer, RawAgentDefinition, PathBuf, String)>;

/// Parst die Rohquellen und gruppiert sie nach Definitions-ID.
///
/// # Description
/// Eine Verzeichnisdefinition, die nicht parst, ist ein harter Fehler
/// (fail-closed). Eine flache Altdatei (`agents/<name>.toml`, bis Plan R9
/// von `agents.commit_proposal` geschrieben) wird mit einer
/// `tracing::warn`-Meldung gelesen, damit der Nutzer sie in das
/// Verzeichnisformat umzieht; nicht parsbar (sie könnte eine beliebige
/// andere TOML-Datei sein) oder im selben Layer schon als
/// Verzeichnisdefinition vorhanden, wird sie mit Warnung übersprungen.
///
/// # Errors
/// [`ConfigError::Invalid`] für eine nicht parsbare Verzeichnisdefinition.
fn parse_definition_sources(
    sources: &AgentDefinitionSources,
) -> ConfigResult<BTreeMap<String, DiscoveredDefinitionLayers>> {
    let mut target = BTreeMap::<String, DiscoveredDefinitionLayers>::new();
    let mut seen_in_layer: HashSet<(PathBuf, String)> = HashSet::new();
    for source in &sources.definitions {
        let layer = definition_layer(source.layer);
        match source.form {
            AgentSourceForm::Directory => {
                let definition = parse_toml(&source.text).map_err(|error| {
                    ConfigError::Invalid(format!(
                        "failed to parse agent definition '{}': {error}",
                        source.path.display()
                    ))
                })?;
                let id = definition.id.to_string();
                seen_in_layer.insert((source.base.clone(), id.clone()));
                target.entry(id).or_default().push((
                    layer,
                    definition,
                    source.path.clone(),
                    source.text.clone(),
                ));
            }
            AgentSourceForm::LegacyFlat => {
                let definition = match parse_toml(&source.text) {
                    Ok(definition) => definition,
                    Err(error) => {
                        tracing::warn!(
                            path = %source.path.display(),
                            %error,
                            "config.agent_definition.legacy_flat_file_unparsable_skipped"
                        );
                        continue;
                    }
                };
                let id = definition.id.to_string();
                let key = (source.base.clone(), id.clone());
                if seen_in_layer.contains(&key) {
                    tracing::warn!(
                        path = %source.path.display(),
                        id = %id,
                        "config.agent_definition.legacy_flat_file_shadowed_by_directory"
                    );
                    continue;
                }
                tracing::warn!(
                    path = %source.path.display(),
                    id = %id,
                    "config.agent_definition.legacy_flat_file: move it to agents/<name>/definition.toml"
                );
                seen_in_layer.insert(key);
                target.entry(id).or_default().push((
                    layer,
                    definition,
                    source.path.clone(),
                    source.text.clone(),
                ));
            }
        }
    }
    Ok(target)
}

/// Alle gefundenen Definitionen als Auflösungsstapel (Rohformen) und als
/// Quelldateien (#22 Welle 1B).
struct DefinitionStack {
    raw: Vec<(DefinitionLayer, RawAgentDefinition)>,
    files: Vec<SourceFile>,
}

impl DefinitionStack {
    fn new(definition_layers: &BTreeMap<String, DiscoveredDefinitionLayers>) -> Self {
        let mut raw = Vec::new();
        let mut files = Vec::new();
        for definitions in definition_layers.values() {
            for (layer, definition, path, text) in definitions {
                raw.push((*layer, definition.clone()));
                files.push(SourceFile::new(*layer, path.clone(), text.clone()));
            }
        }
        Self { raw, files }
    }
}

/// Baut die Kontextprogramm-Bibliothek: die eingebauten Programme plus die
/// eigenen Programme der vertrauten Schichten (#22 Welle 1B).
///
/// # Errors
/// [`ConfigError::Invalid`], wenn ein Programm nicht parst oder ein Name mit
/// einer anderen Programm-ID doppelt vergeben ist.
fn context_program_library(
    programs: &[ContextProgramSource],
) -> ConfigResult<ContextProgramLibrary> {
    let mut library = builtin_context_program_library().map_err(|error| {
        ConfigError::Invalid(format!("built-in context programs do not parse: {error}"))
    })?;
    for program in programs {
        library
            .insert_sources(
                definition_layer(program.layer),
                &[(program.name.as_str(), program.text.as_str())],
            )
            .map_err(|error| {
                ConfigError::Invalid(format!(
                    "context program {}: {error}",
                    program.path.display()
                ))
            })?;
    }
    Ok(library)
}

/// Löst eine gefundene Definition über den eingebauten Definitionen auf und
/// senkt sie zur typisierten [`AgentIr`] (Plan R9, Teil B; #22 Welle 1B).
///
/// # Description
/// Der Auflösungsstapel besteht aus
/// 1. den eingebauten Definitionen auf [`DefinitionLayer::BuiltIn`] —
///    **außer** einer eingebauten Definition mit derselben ID wie das Ziel:
///    eine lokale Definition, die eine eingebaute ID wiederverwendet, wird
///    eigenständig aufgelöst und nie als Overlay über die eingebaute Rolle
///    gelegt (die eingebaute Rolle selbst bleibt davon unberührt, siehe
///    [`crate::embedded_agents::builtin_agent_definitions`]),
/// 2. allen gefundenen Definitionen aller vertrauten Layer (`stack`), damit
///    lokale Definitionen einander erweitern können.
///
/// Gesenkt wird mit `lower_v2` über denselben Dateien (Spannen, eigene
/// Tabellen), dem [`FsInstructionsLoader`] (`instructions_file` bzw.
/// `system.md` im Agentenordner) und der Kontextprogramm-Bibliothek — eine
/// Nutzerdefinition bekommt ihr `[context] program` damit gebunden wie jede
/// eingebaute Rolle. Warnungen und Hinweise werden geloggt.
///
/// # Returns
/// Die IR, ihre [`AgentDefinitionMeta`] (Sicht auf die IR) und den
/// Agentenordner der höchsten Quelle.
///
/// # Errors
/// [`ConfigError::Invalid`] mit den gerenderten Diagnosen (Code,
/// `datei:zeile:spalte`, Hilfetext), wenn Auflösung oder Senken scheitern.
fn resolve_discovered_definition(
    id: &str,
    definitions: &DiscoveredDefinitionLayers,
    stack: &DefinitionStack,
    programs: &ContextProgramLibrary,
) -> ConfigResult<(AgentIr, AgentDefinitionMeta, PathBuf)> {
    let paths = definitions
        .iter()
        .map(|(_, _, path, _)| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let first = definitions.first().ok_or_else(|| {
        ConfigError::Invalid(format!(
            "resolved agent definition '{id}' from {paths} has no source layer"
        ))
    })?;
    let target_id = &first.1.id;
    let target_label = target_id.to_string();
    let mut resolver_layers = builtin_definition_layers()
        .iter()
        .filter(|builtin| &builtin.id != target_id)
        .map(|builtin| (DefinitionLayer::BuiltIn, builtin.clone()))
        .collect::<Vec<_>>();
    resolver_layers.extend(stack.raw.iter().cloned());
    let mut files: Vec<SourceFile> = builtin_source_files()
        .iter()
        .filter(|(_, declared)| declared.as_deref() != Some(target_label.as_str()))
        .map(|(file, _)| file.clone())
        .collect();
    files.extend(stack.files.iter().cloned());

    let resolved_definition =
        resolve_definition(target_id, &resolver_layers, OffsetDateTime::now_utc()).map_err(
            |error| {
                let diagnostic = diagnostic_for_error(&error, &files, target_id);
                ConfigError::Invalid(format!(
                    "failed to resolve agent definition '{id}' from {paths}:\n{diagnostic}"
                ))
            },
        )?;
    let loader = FsInstructionsLoader::new();
    let sources = LowerSources::new(&files)
        .with_instructions(&loader)
        .with_context_programs(programs);
    let ir = lower_v2(&resolved_definition, &sources).map_err(|diagnostics| {
        ConfigError::Invalid(format!(
            "failed to lower agent definition '{id}' from {paths}:\n{diagnostics}"
        ))
    })?;
    for diagnostic in &ir.trace.diagnostics {
        if diagnostic.severity == Severity::Warning {
            tracing::warn!(
                definition = %id,
                code = %diagnostic.code,
                "config.agent_definition.diagnostic: {diagnostic}"
            );
        } else {
            tracing::debug!(
                definition = %id,
                code = %diagnostic.code,
                "config.agent_definition.diagnostic: {diagnostic}"
            );
        }
    }
    let last_source = definitions.last().ok_or_else(|| {
        ConfigError::Invalid(format!(
            "resolved agent definition '{id}' from {paths} has no source layer"
        ))
    })?;
    let definition_dir = parent_dir(&last_source.2)?;
    let meta = AgentDefinitionMeta::from_ir(&ir, Some(last_source.0));
    Ok((ir, meta, definition_dir))
}

/// Der Elternordner einer Definitionsdatei.
fn parent_dir(path: &Path) -> ConfigResult<PathBuf> {
    path.parent().map(Path::to_path_buf).ok_or_else(|| {
        ConfigError::Invalid(format!(
            "agent definition file '{}' has no parent directory",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_config::discover_config;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    fn test_directory(label: &str) -> TestResult<PathBuf> {
        let unique = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "harw-registry-config-agents-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).map_err(ctx("Testverzeichnis anlegen"))?;
        Ok(directory)
    }

    /// Discovery über `layers`, danach das Senken der Agentendefinitionen.
    fn discover(
        layers: &[PathBuf],
        context: &'static str,
    ) -> TestResult<(ResolvedConfig, ConfigAgents)> {
        let config = discover_config(layers).map_err(ctx(context))?;
        let agents = ConfigAgents::from_config(&config).map_err(ctx("Agenten senken"))?;
        Ok((config, agents))
    }

    fn write_definition(base: &Path, directory: &str, contents: &str) -> TestResult {
        let definitions = base.join("agents").join(directory);
        std::fs::create_dir_all(&definitions).map_err(ctx("Definitionsverzeichnis anlegen"))?;
        std::fs::write(definitions.join("definition.toml"), contents)
            .map_err(ctx("definition.toml schreiben"))?;
        Ok(())
    }

    fn worker_definition(id: &str, specialization: &str, tools: &str) -> String {
        format!(
            r#"schema = "harwness.agent/v1"
id = "{id}"
version = "1.0.0"
role = "worker"
specialization = "{specialization}"

[tools]
{tools}
"#
        )
    }

    #[test]
    fn builtin_layers_contain_both_bases_but_no_context_programs() -> TestResult {
        let ids: Vec<String> = builtin_definition_layers()
            .iter()
            .map(|raw| raw.id.to_string())
            .collect();
        assert!(ids.iter().any(|id| id == "harwness.agent.worker-base@1"));
        assert!(
            ids.iter()
                .any(|id| id == "harwness.agent.child-orchestrator-base@1")
        );
        assert!(ids.iter().any(|id| id == "harwness.agent.explorer@1"));
        assert!(!ids.iter().any(|id| id.starts_with("harwness.context.")));
        Ok(())
    }

    #[test]
    fn builtin_sources_and_context_programs_are_embedded() -> TestResult {
        let files = builtin_source_files();
        assert!(
            files
                .iter()
                .any(|(_, id)| id.as_deref() == Some("harwness.agent.worker-base@1"))
        );
        assert!(
            files
                .iter()
                .all(|(file, _)| file.label().starts_with("builtin/"))
        );
        let programs = builtin_context_program_sources();
        assert!(programs.iter().any(|(name, _)| *name == "explore"));
        let library = builtin_context_program_library().map_err(TestError::Unexpected)?;
        assert!(library.names().contains(&"explore"));
        Ok(())
    }

    #[test]
    fn empty_sources_lower_to_no_agents() -> TestResult {
        let agents =
            ConfigAgents::lower(&AgentDefinitionSources::default()).map_err(ctx("senken"))?;
        assert!(agents.executable_agents.is_empty());
        assert!(agents.agent_irs.is_empty());
        assert!(agents.agent_definition_dirs.is_empty());
        assert!(agents.agent_definition_meta.is_empty());
        Ok(())
    }

    #[test]
    fn a_selected_uia_must_have_the_user_interface_role() -> TestResult {
        let base = test_directory("uia-role")?;
        let id = "user.agent.not-a-uia@1";
        std::fs::write(
            base.join("config.toml"),
            format!("active_uia_definition = {id:?}\n"),
        )
        .map_err(ctx("config.toml schreiben"))?;
        write_definition(
            &base,
            "not-a-uia",
            &worker_definition(id, "not-a-uia", "admitted = [\"fs.read\"]"),
        )?;
        let (config, agents) = discover(std::slice::from_ref(&base), "discover config")?;
        assert!(matches!(
            agents.validate_selection(&config.harness),
            Err(ConfigError::Invalid(message)) if message.contains("user-interface")
        ));
        assert!(matches!(
            ConfigAgents::default().validate_selection(&config.harness),
            Err(ConfigError::UnresolvedRef { kind, .. }) if kind == "UIA definition"
        ));
        assert!(ConfigAgents::from_config_validated(&config).is_err());
        std::fs::remove_dir_all(base).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn a_flat_file_is_shadowed_by_a_directory_definition_of_the_same_layer() -> TestResult {
        let base = test_directory("flat-shadowed")?;
        let id = "user.agent.shadowed@1";
        write_definition(
            &base,
            "shadowed",
            &worker_definition(id, "from-directory", "admitted = [\"fs.read\"]"),
        )?;
        std::fs::write(
            base.join("agents").join("shadowed.toml"),
            worker_definition(id, "from-flat-file", "admitted = [\"fs.read\"]"),
        )
        .map_err(ctx("flache Datei schreiben"))?;
        let (_, agents) = discover(std::slice::from_ref(&base), "discover config")?;
        let executable = agents
            .executable_agents
            .get(id)
            .ok_or(TestError::Missing("shadowed"))?;
        assert_eq!(executable.specialization(), "from-directory");
        std::fs::remove_dir_all(base).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }

    /// Das Bundle liegt auf der Home-Ebene; jede mitgelieferte
    /// `definition.toml` senkt über die reguläre Layer-Kette (vorher in
    /// `harw-home/src/scaffold.rs`, das die DSL nicht kennt).
    #[test]
    fn bundled_agents_lower_over_the_home_layer() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let report = harw_home::ensure_home(home.path()).map_err(ctx("scaffold home"))?;
        let (_, agents) = discover(
            &[home.path().to_path_buf(), report.profile_dir.clone()],
            "discovery over home and profile layer must succeed",
        )?;
        let implementer = agents
            .executable_agents
            .get("harwness.agent.rust-implementer@1")
            .ok_or(TestError::Missing("rust-implementer"))?;
        assert_eq!(implementer.specialization(), "rust-implementer");
        let meta = agents
            .agent_definition_meta
            .get("harwness.agent.rust-implementer@1")
            .ok_or(TestError::Missing("rust-implementer meta"))?;
        assert!(
            meta.instructions
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty()),
            "system.md wird über instructions_file geladen"
        );
        let bundled_definitions = harw_home::bundled_files()
            .iter()
            .filter(|file| file.relative_path.ends_with("/definition.toml"))
            .count();
        assert_eq!(agents.executable_agents.len(), bundled_definitions);
        Ok(())
    }

    #[test]
    fn discovers_and_lowers_an_agent_definition_without_legacy_agent_toml() -> TestResult {
        let base = test_directory("definition-discovery")?;
        let id = "harwness.agent.discovery-worker@1";
        write_definition(
            &base,
            "discovery-worker",
            &worker_definition(
                id,
                "definition-discovery",
                "admitted = [\"fs.read\"]\nforbidden = [\"network.fetch\"]",
            ),
        )?;

        let (config, agents) = discover(std::slice::from_ref(&base), "discover config")?;
        let executable = agents
            .executable_agents
            .get(id)
            .ok_or(TestError::Missing("executable agent for discovery-worker"))?;

        assert_eq!(executable.id().to_string(), id);
        assert_eq!(executable.specialization(), "definition-discovery");
        assert_eq!(executable.tool_surface().admitted(), ["fs.read"]);
        assert_eq!(executable.tool_surface().forbidden(), ["network.fetch"]);
        assert!(config.agents.is_empty());
        std::fs::remove_dir_all(base).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn profile_definition_extends_builtin_worker_base() -> TestResult {
        // Plan R9, Teil B: vorher `MissingBase`, weil die eingebauten
        // Definitionen nicht im Auflösungsstapel lagen.
        let base = test_directory("extends-worker-base")?;
        let id = "user.agent.note-taker@1";
        write_definition(
            &base,
            "note-taker",
            &format!(
                r#"schema = "harwness.agent/v1"
id = "{id}"
version = "1.0.0"
extends = {{ id = "harwness.agent.worker-base@1" }}
role = "worker"
specialization = "note-taker"
description = "Fasst Notizen zusammen."
instructions_file = "system.md"

[tools]
admitted = ["fs.read"]
"#
            ),
        )?;
        std::fs::write(
            base.join("agents").join("note-taker").join("system.md"),
            "Du fasst Notizen zusammen.",
        )
        .map_err(ctx("system.md schreiben"))?;

        let (_, agents) = discover(std::slice::from_ref(&base), "discover config")?;
        let executable = agents
            .executable_agents
            .get(id)
            .ok_or(TestError::Missing("note-taker"))?;
        assert_eq!(executable.tool_surface().admitted(), ["fs.read"]);
        // Von worker-base geerbt: Lebenszyklus und Kontext.
        assert!(!executable.lifecycle_machine().allow_pause());
        assert!(
            executable
                .context_program()
                .exclude()
                .iter()
                .any(|selector| selector == "full_parent_transcript")
        );
        let meta = agents
            .agent_definition_meta
            .get(id)
            .ok_or(TestError::Missing("meta"))?;
        assert_eq!(meta.description.as_deref(), Some("Fasst Notizen zusammen."));
        assert_eq!(
            meta.instructions.as_deref(),
            Some("Du fasst Notizen zusammen.")
        );
        std::fs::remove_dir_all(base).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn profile_definition_extends_child_orchestrator_base() -> TestResult {
        let base = test_directory("extends-child-orchestrator-base")?;
        let id = "user.agent.review-lead@1";
        write_definition(
            &base,
            "review-lead",
            &format!(
                r#"schema = "harwness.agent/v1"
id = "{id}"
version = "1.0.0"
extends = {{ id = "harwness.agent.child-orchestrator-base@1" }}
role = "child-orchestrator"
specialization = "review-lead"
"#
            ),
        )?;
        let (_, agents) = discover(std::slice::from_ref(&base), "discover config")?;
        let executable = agents
            .executable_agents
            .get(id)
            .ok_or(TestError::Missing("review-lead"))?;
        assert_eq!(executable.spawn_contract().max_depth(), Some(1));
        assert!(
            executable
                .tool_surface()
                .admitted()
                .iter()
                .any(|tool| tool == "delegate_wave")
        );
        std::fs::remove_dir_all(base).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn legacy_flat_definition_file_is_still_read() -> TestResult {
        let base = test_directory("legacy-flat-definition")?;
        let agents = base.join("agents");
        std::fs::create_dir_all(&agents).map_err(ctx("agents anlegen"))?;
        let id = "user.agent.flat-worker@1";
        std::fs::write(
            agents.join("flat-worker.toml"),
            worker_definition(id, "flat-worker", "admitted = [\"fs.read\"]"),
        )
        .map_err(ctx("flache Datei schreiben"))?;
        // Eine fremde, nicht parsbare flache Datei bricht die Discovery nicht.
        std::fs::write(agents.join("notes.toml"), "title = \"keine Definition\"\n")
            .map_err(ctx("fremde Datei schreiben"))?;

        let (_, agents) = discover(std::slice::from_ref(&base), "discover config")?;
        assert!(agents.executable_agents.contains_key(id));
        std::fs::remove_dir_all(base).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn run_scoped_definitions_are_discovered_for_their_run() -> TestResult {
        let project_harw = test_directory("run-scoped")?;
        let dir = project_harw
            .join("state")
            .join("runs")
            .join("run-7")
            .join("agents")
            .join("scratch");
        std::fs::create_dir_all(&dir).map_err(ctx("Run-Verzeichnis anlegen"))?;
        let id = "user.agent.scratch@1";
        std::fs::write(
            dir.join("definition.toml"),
            worker_definition(id, "scratch", "admitted = [\"fs.read\"]"),
        )
        .map_err(ctx("definition.toml schreiben"))?;

        let found =
            discover_run_agent_definitions(&project_harw, "run-7").map_err(ctx("run discovery"))?;
        assert!(found.contains_key(id));
        assert!(
            discover_run_agent_definitions(&project_harw, "other-run")
                .map_err(ctx("other run"))?
                .is_empty()
        );
        assert!(
            discover_run_agent_definitions(&project_harw, "../escape")
                .map_err(ctx("invalid run id"))?
                .is_empty()
        );
        std::fs::remove_dir_all(project_harw).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn selected_active_agent_definition_validates_when_discovered() -> TestResult {
        let base = test_directory("selected-agent-definition")?;
        let id = "harwness.agent.selected-worker@1";
        std::fs::write(
            base.join("config.toml"),
            format!("active_agent_definition = {id:?}\n"),
        )
        .map_err(ctx("config.toml schreiben"))?;
        write_definition(
            &base,
            "selected-worker",
            &worker_definition(id, "selected", "admitted = [\"fs.read\"]"),
        )?;

        let (config, agents) = discover(std::slice::from_ref(&base), "discover config")?;

        assert!(config.validate().is_ok());
        assert!(agents.validate_selection(&config.harness).is_ok());
        std::fs::remove_dir_all(base).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }

    #[test]
    fn validate_rejects_an_unknown_selected_agent_definition() -> TestResult {
        let mut config = ResolvedConfig::default();
        config.harness.active_agent_definition = Some("harwness.agent.missing@1".to_owned());

        assert!(matches!(
            ConfigAgents::default().validate_selection(&config.harness),
            Err(ConfigError::UnresolvedRef { kind, reference })
                if kind == "agent definition" && reference == "harwness.agent.missing@1"
        ));
        Ok(())
    }

    #[test]
    fn validate_leaves_bare_builtin_agent_names_to_the_runtime() -> TestResult {
        let mut config = ResolvedConfig::default();
        config.harness.active_agent_definition = Some("root-orchestrator".to_owned());

        assert!(
            ConfigAgents::default()
                .validate_selection(&config.harness)
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn final_layer_definition_reduces_the_tool_surface() -> TestResult {
        let user_layer = test_directory("definition-user-layer")?;
        let project_layer = test_directory("definition-project-layer")?;
        let id = "harwness.agent.layered-worker@1";
        write_definition(
            &user_layer,
            "layered-worker",
            &worker_definition(
                id,
                "user-layer",
                "admitted = [\"fs.read\", \"shell.exec\"]\nforbidden = [\"network.fetch\"]",
            ),
        )?;
        write_definition(
            &project_layer,
            "layered-worker",
            &format!(
                r#"schema = "harwness.agent/v1"
id = "{id}"
version = "1.0.1"
role = "worker"
specialization = "project-layer"

[tools]
admitted = ["fs.read"]
forbidden = ["network.fetch", "shell.exec"]

[patch.tools]
replace = {{ admitted = ["fs.read"], forbidden = ["network.fetch", "shell.exec"] }}
"#
            ),
        )?;

        let (_, agents) = discover(
            &[user_layer.clone(), project_layer.clone()],
            "Konfiguration entdecken",
        )?;
        let executable = agents
            .executable_agents
            .get(id)
            .ok_or(TestError::Missing("executable agent for layered-worker"))?;

        assert_eq!(executable.specialization(), "project-layer");
        assert_eq!(executable.tool_surface().admitted(), ["fs.read"]);
        assert_eq!(
            executable.tool_surface().forbidden(),
            ["network.fetch", "shell.exec"]
        );
        std::fs::remove_dir_all(user_layer).map_err(ctx("user_layer entfernen"))?;
        std::fs::remove_dir_all(project_layer).map_err(ctx("project_layer entfernen"))?;
        Ok(())
    }

    /// #22 Welle 1B: eine Nutzerdefinition bekommt ihr `[context] program`
    /// gebunden — ein eingebautes wie ein eigenes aus
    /// `agents/context-programs/` — und liegt als typisierte IR vor.
    #[test]
    fn user_definitions_get_their_context_program_bound() -> TestResult {
        let base = test_directory("context-program-binding")?;
        let explore_id = "user.agent.scout@1";
        write_definition(
            &base,
            "scout",
            &format!(
                r#"schema = "harwness.agent/v1"
id = "{explore_id}"
version = "1.0.0"
extends = {{ id = "harwness.agent.worker-base@1" }}
role = "worker"
specialization = "scout"

[tools]
admitted = ["fs.read"]

[context]
program = "explore"
"#
            ),
        )?;
        let own_id = "user.agent.digest@1";
        write_definition(
            &base,
            "digest",
            &format!(
                r#"schema = "harwness.agent/v1"
id = "{own_id}"
version = "1.0.0"
extends = {{ id = "harwness.agent.worker-base@1" }}
role = "worker"
specialization = "digest"

[tools]
admitted = ["fs.read"]

[context]
program = "digest"
"#
            ),
        )?;
        let programs = base.join("agents").join("context-programs");
        std::fs::create_dir_all(&programs).map_err(ctx("Programmordner"))?;
        std::fs::write(
            programs.join("digest.toml"),
            r#"schema = "harwness.context/v1"
id = "user.context.digest@1"
version = "1.0.0"

[[sections]]
name = "task.objective"
strength = "must-include"
detail = "full"
trust = "instruction"

[[sections]]
name = "plan.current"
strength = "must-include"
detail = "summary"
trust = "data"
"#,
        )
        .map_err(ctx("digest.toml"))?;

        let (_, agents) = discover(std::slice::from_ref(&base), "discover config")?;
        let scout = agents
            .agent_irs
            .get(explore_id)
            .ok_or(TestError::Missing("scout IR"))?;
        assert_eq!(scout.context.program.as_deref(), Some("explore"));
        assert_eq!(
            scout.context.program_id.as_deref(),
            Some("harwness.context.explore@1")
        );
        assert!(!scout.context.sections.is_empty());
        let digest = agents
            .agent_irs
            .get(own_id)
            .ok_or(TestError::Missing("digest IR"))?;
        assert_eq!(digest.context.program.as_deref(), Some("digest"));
        assert!(
            digest
                .context
                .must_include
                .iter()
                .any(|section| section == "task.objective")
        );
        // `plan.current` liegt außerhalb der Wurzeldecke: benannt, aber
        // zurückgestellt statt beim Start verlangt.
        assert_eq!(digest.context.deferred, ["plan.current"]);
        // Die Laufzeitsicht trägt dieselbe Politik.
        let view = agents
            .executable_agents
            .get(own_id)
            .ok_or(TestError::Missing("digest view"))?;
        assert_eq!(
            view.context_program().context_policy(),
            Some("user.context.digest@1")
        );
        std::fs::remove_dir_all(base).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }

    /// #22 Welle 1B: eine defekte Nutzerdefinition scheitert mit den
    /// gerenderten Diagnosen — stabiler Code plus `datei:zeile:spalte`.
    #[test]
    fn broken_user_definition_reports_diagnostic_codes_with_spans() -> TestResult {
        let base = test_directory("broken-definition")?;
        write_definition(
            &base,
            "broken",
            r#"schema = "harwness.agent/v1"
id = "user.agent.broken@1"
version = "1.0.0"
role = "worker"
specialization = "broken"

[tols]
admitted = ["fs.read"]

[return]
contract = "harwness.return.nope@1"
"#,
        )?;
        let config =
            discover_config(std::slice::from_ref(&base)).map_err(ctx("discover config"))?;
        let error = match ConfigAgents::from_config(&config) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "a broken definition must not load".to_owned(),
                ));
            }
            Err(error) => error.to_string(),
        };
        assert!(error.contains("HARW-SCHEMA-002"), "{error}");
        assert!(error.contains("HARW-RETURN-001"), "{error}");
        // `--> <pfad>/definition.toml:<zeile>:<spalte>`
        assert!(error.contains("definition.toml:"), "{error}");
        assert!(error.contains("-->"), "{error}");
        assert!(error.contains("user.agent.broken@1"), "{error}");
        std::fs::remove_dir_all(base).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }

    /// #22 Welle 1B: `AgentDefinitionMeta` ist eine Sicht auf die IR —
    /// Instruktionen (auch ein implizites `system.md`), Beschreibung und
    /// Delegationsziele stimmen mit ihr überein.
    #[test]
    fn definition_meta_is_a_view_of_the_ir() -> TestResult {
        let base = test_directory("meta-view")?;
        let id = "user.agent.lead@1";
        write_definition(
            &base,
            "lead",
            &format!(
                r#"schema = "harwness.agent/v1"
id = "{id}"
version = "1.0.0"
extends = {{ id = "harwness.agent.child-orchestrator-base@1" }}
role = "child-orchestrator"
specialization = "lead"
description = "Leitet eine Prüfung."

[delegation]
targets = ["explorer", "analyst"]
"#
            ),
        )?;
        std::fs::write(
            base.join("agents").join("lead").join("system.md"),
            "Plane zuerst, delegiere dann.",
        )
        .map_err(ctx("system.md"))?;
        let (_, agents) = discover(std::slice::from_ref(&base), "discover config")?;
        let ir = agents.agent_irs.get(id).ok_or(TestError::Missing("IR"))?;
        let meta = agents
            .agent_definition_meta
            .get(id)
            .ok_or(TestError::Missing("meta"))?;
        assert_eq!(meta, &AgentDefinitionMeta::from_ir(ir, meta.layer));
        assert_eq!(
            meta.instructions.as_deref(),
            Some("Plane zuerst, delegiere dann.")
        );
        assert_eq!(ir.instructions.source.as_deref(), Some("system.md"));
        assert_eq!(
            meta.delegation_targets,
            Some(vec!["explorer".to_owned(), "analyst".to_owned()])
        );
        assert_eq!(meta.layer, Some(DefinitionLayer::UserGlobal));
        std::fs::remove_dir_all(base).map_err(ctx("Testverzeichnis entfernen"))?;
        Ok(())
    }
}
