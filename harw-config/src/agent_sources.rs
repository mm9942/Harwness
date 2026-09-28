//! Rohquellen der DSL-Agentendefinitionen und Kontextprogramme der
//! vertrauten Layer (Plan R9, Teil B; #22 Welle 1B).
//!
//! # Verantwortlichkeit
//! Diese Crate liegt in der Infrastruktur-Schicht (I) und kennt die
//! Agenten-DSL (`harw-agent-dsl`, Schicht C) **nicht**. Sie findet und liest
//! die Dateien nur — `agents/<name>/definition.toml`, die ältere flache Form
//! `agents/<name>.toml` und `agents/context-programs/*.toml` — und reicht sie
//! **ungeparst** als [`AgentDefinitionSources`] weiter
//! ([`crate::ResolvedConfig::agent_sources`]). Parsen, Auflösen über den
//! eingebauten Definitionen und Senken zur IR übernimmt der Konsument
//! (`harw_registry_defaults::config_agents`).
//!
//! # Reihenfolge
//! Innerhalb eines Layers stehen zuerst die Verzeichnisdefinitionen, danach
//! die flachen Dateien, jeweils nach Dateinamen sortiert; die Layer folgen in
//! aufsteigender Präzedenz. Der Konsument braucht genau diese Reihenfolge,
//! um die Regel „das Verzeichnisformat gewinnt im selben Layer“ anzuwenden.

use std::path::{Path, PathBuf};

use crate::discovery::{read_file, read_sorted_dir_entries};
use crate::error::{ConfigError, ConfigResult};

/// Reserviertes Unterverzeichnis der Kontextprogramm-Bibliothek unter
/// `agents/`.
pub const CONTEXT_PROGRAMS_DIR: &str = "context-programs";

/// Top-Level-Schlüssel einer `definition.toml`, der eine Instruktionsdatei
/// relativ zum Agentenordner benennt (Plan R9, Teil B).
pub const INSTRUCTIONS_FILE_KEY: &str = "instructions_file";

/// Die Schicht, aus der eine Rohquelle stammt.
///
/// # Description
/// Spiegel der vertrauten Schichten der DSL (`harw_agent_dsl::layers::
/// DefinitionLayer`), ohne die eingebaute Schicht: die liefert der Konsument
/// selbst. Nur ein konfigurierter letzter Layer (bei mehr als einem Layer)
/// ist projektvertraut; ein einzelner Layer bleibt vertraglich
/// nutzerglobal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentSourceLayer {
    /// Home- oder Profil-Layer.
    UserGlobal,
    /// Der letzte von mehreren vertrauten Layern.
    Project,
    /// `<projekt>/.harw/state/runs/<run_id>/` (`scope = "run"`).
    RunLocal,
}

/// Die Dateiform einer Definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSourceForm {
    /// Kanonisch: `agents/<name>/definition.toml`. Eine nicht parsbare
    /// Datei dieser Form ist ein harter Fehler (fail-closed).
    Directory,
    /// Das ältere flache Format `agents/<name>.toml`. Eine nicht parsbare
    /// Datei dieser Form wird mit Warnung übersprungen (sie kann eine
    /// beliebige andere TOML-Datei sein); das Verzeichnisformat derselben
    /// ID im selben Layer gewinnt.
    LegacyFlat,
}

/// Eine gefundene, ungeparste Agentendefinition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinitionSource {
    /// Die Schicht der Quelle.
    pub layer: AgentSourceLayer,
    /// Das Layer-Verzeichnis, in dem die Datei gefunden wurde (Grenze der
    /// Regel „Verzeichnisformat gewinnt im selben Layer“).
    pub base: PathBuf,
    /// Die Dateiform.
    pub form: AgentSourceForm,
    /// Pfad der Datei.
    pub path: PathBuf,
    /// Der unveränderte Quelltext.
    pub text: String,
}

/// Ein gefundenes, ungeparstes eigenes Kontextprogramm
/// (`agents/context-programs/<name>.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextProgramSource {
    /// Die Schicht der Quelle.
    pub layer: AgentSourceLayer,
    /// Der Name, mit dem eine Definition per `[context] program = "<name>"`
    /// bindet: der Dateistamm.
    pub name: String,
    /// Pfad der Datei.
    pub path: PathBuf,
    /// Der unveränderte Quelltext.
    pub text: String,
}

/// Alle Rohquellen eines Discovery-Laufs, in der Reihenfolge des
/// Moduldokuments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentDefinitionSources {
    /// Die gefundenen Agentendefinitionen.
    pub definitions: Vec<AgentDefinitionSource>,
    /// Die gefundenen eigenen Kontextprogramme.
    pub context_programs: Vec<ContextProgramSource>,
}

impl AgentDefinitionSources {
    /// `true`, wenn keine Agentendefinition gefunden wurde (eigene
    /// Kontextprogramme allein ergeben keine Agenten).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }
}

/// Liest die Agentendefinitionen und eigenen Kontextprogramme eines Layers.
///
/// # Errors
/// [`ConfigError::ReadFailed`] bei Lesefehlern. Der Inhalt wird hier nicht
/// geprüft.
pub(crate) fn discover_layer_agent_sources(
    base: &Path,
    layer: AgentSourceLayer,
    target: &mut AgentDefinitionSources,
) -> ConfigResult<()> {
    discover_definition_sources(base, layer, &mut target.definitions)?;
    discover_context_program_sources(base, layer, &mut target.context_programs)
}

/// Liest `agents/<name>/definition.toml` und `agents/<name>.toml` eines
/// Layers: erst alle Verzeichnisdefinitionen, dann alle flachen Dateien.
fn discover_definition_sources(
    base: &Path,
    layer: AgentSourceLayer,
    target: &mut Vec<AgentDefinitionSource>,
) -> ConfigResult<()> {
    let dir = base.join("agents");
    if !dir.exists() {
        return Ok(());
    }

    let mut flat_files: Vec<PathBuf> = Vec::new();
    for entry in read_sorted_dir_entries(&dir)? {
        let entry_path = entry.path();
        let file_type = entry.file_type().map_err(|error| ConfigError::ReadFailed {
            path: entry_path.display().to_string(),
            reason: error.to_string(),
        })?;
        if file_type.is_file() {
            if entry_path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
                flat_files.push(entry_path);
            }
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }

        let definition_path = entry_path.join("definition.toml");
        if !definition_path.exists() {
            continue;
        }
        let text = read_file(&definition_path)?;
        target.push(AgentDefinitionSource {
            layer,
            base: base.to_path_buf(),
            form: AgentSourceForm::Directory,
            path: definition_path,
            text,
        });
    }

    for flat_path in flat_files {
        let text = read_file(&flat_path)?;
        target.push(AgentDefinitionSource {
            layer,
            base: base.to_path_buf(),
            form: AgentSourceForm::LegacyFlat,
            path: flat_path,
            text,
        });
    }
    Ok(())
}

/// Liest die eigenen Kontextprogramme eines Layers:
/// `agents/context-programs/*.toml`, Name ist der Dateistamm (#22 Welle 1B).
fn discover_context_program_sources(
    base: &Path,
    layer: AgentSourceLayer,
    target: &mut Vec<ContextProgramSource>,
) -> ConfigResult<()> {
    let dir = base.join("agents").join(CONTEXT_PROGRAMS_DIR);
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in read_sorted_dir_entries(&dir)? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") || !path.is_file() {
            continue;
        }
        let Some(name) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        let text = read_file(&path)?;
        target.push(ContextProgramSource {
            layer,
            name,
            path,
            text,
        });
    }
    Ok(())
}

/// Findet die Rohquellen eines einzelnen Laufs (`scope = "run"`, Plan R9,
/// Teil B).
///
/// # Description
/// `agents.write_definition` mit `scope = "run"` legt Definitionen unter
/// `<projekt>/.harw/state/runs/<run_id>/agents/<name>/definition.toml` ab
/// (ältere Stände: `…/agents/<name>.toml`). Alle Quellen liegen auf
/// [`AgentSourceLayer::RunLocal`]. Gesenkt werden sie vom Konsumenten
/// (`harw_registry_defaults::config_agents::discover_run_agent_definitions`).
///
/// # Arguments
/// - `project_harw_dir` (`&Path`): das `.harw`-Verzeichnis des Projekts.
/// - `run_id` (`&str`): die Lauf-ID; nur `[a-z0-9-]`, sonst leer.
///
/// # Returns
/// Die Rohquellen; leer bei ungültiger Lauf-ID oder fehlendem Verzeichnis.
///
/// # Errors
/// [`ConfigError::ReadFailed`] bei Lesefehlern.
pub fn discover_run_agent_sources(
    project_harw_dir: &Path,
    run_id: &str,
) -> ConfigResult<AgentDefinitionSources> {
    let valid_run_id = !run_id.is_empty()
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    let mut sources = AgentDefinitionSources::default();
    if !valid_run_id {
        return Ok(sources);
    }
    let run_base = project_harw_dir.join("state").join("runs").join(run_id);
    discover_layer_agent_sources(&run_base, AgentSourceLayer::RunLocal, &mut sources)?;
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn write(path: &Path, contents: &str) -> TestResult {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ctx("Verzeichnis anlegen"))?;
        }
        std::fs::write(path, contents).map_err(ctx("Datei schreiben"))?;
        Ok(())
    }

    #[test]
    fn layer_sources_list_directories_before_flat_files_and_stay_unparsed() -> TestResult {
        let base = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let agents = base.path().join("agents");
        write(
            &agents.join("a-flat.toml"),
            "title = \"keine Definition\"\n",
        )?;
        write(
            &agents.join("b-dir").join("definition.toml"),
            "id = [kaputt",
        )?;
        write(
            &agents.join("c-dir").join("agent.toml"),
            "name = \"legacy\"\n",
        )?;
        write(
            &agents.join(CONTEXT_PROGRAMS_DIR).join("digest.toml"),
            "schema = \"harwness.context/v1\"\n",
        )?;
        write(&agents.join(CONTEXT_PROGRAMS_DIR).join("notes.md"), "# x\n")?;

        let mut sources = AgentDefinitionSources::default();
        discover_layer_agent_sources(base.path(), AgentSourceLayer::UserGlobal, &mut sources)
            .map_err(ctx("Quellen lesen"))?;

        let forms: Vec<(AgentSourceForm, String)> = sources
            .definitions
            .iter()
            .map(|source| {
                (
                    source.form,
                    source
                        .path
                        .strip_prefix(&agents)
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            forms,
            [
                (
                    AgentSourceForm::Directory,
                    "b-dir/definition.toml".to_owned()
                ),
                (AgentSourceForm::LegacyFlat, "a-flat.toml".to_owned()),
            ]
        );
        // Ungeparst: auch ein kaputter Quelltext kommt unverändert an.
        assert_eq!(sources.definitions[0].text, "id = [kaputt");
        assert!(sources.definitions.iter().all(|source| source.layer
            == AgentSourceLayer::UserGlobal
            && source.base == base.path()));
        assert_eq!(sources.context_programs.len(), 1);
        assert_eq!(sources.context_programs[0].name, "digest");
        Ok(())
    }

    #[test]
    fn run_sources_are_run_local_and_reject_invalid_run_ids() -> TestResult {
        let project_harw = tempfile::tempdir().map_err(ctx("tempdir"))?;
        write(
            &project_harw
                .path()
                .join("state")
                .join("runs")
                .join("run-7")
                .join("agents")
                .join("scratch")
                .join("definition.toml"),
            "id = \"user.agent.scratch@1\"\n",
        )?;

        let found =
            discover_run_agent_sources(project_harw.path(), "run-7").map_err(ctx("run-7"))?;
        assert_eq!(found.definitions.len(), 1);
        assert_eq!(found.definitions[0].layer, AgentSourceLayer::RunLocal);
        assert!(
            discover_run_agent_sources(project_harw.path(), "other-run")
                .map_err(ctx("other-run"))?
                .is_empty()
        );
        assert!(
            discover_run_agent_sources(project_harw.path(), "../escape")
                .map_err(ctx("escape"))?
                .is_empty()
        );
        Ok(())
    }
}
