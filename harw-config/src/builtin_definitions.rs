//! Die eingebauten Agentendefinitionen als unterste Auflösungsschicht
//! (Plan R9, Teil B).
//!
//! # Warum hier
//! Profil- und Projektdefinitionen (`agents/<name>/definition.toml`) sollen
//! eingebaute Basen erweitern können, etwa
//! `extends = { id = "harwness.agent.worker-base@1" }`. Vor diesem Modul
//! kannte die Discovery nur die Dateien der vertrauten Layer; die eingebauten
//! Definitionen lagen allein in `harw-registry-defaults` und fehlten im
//! Auflösungsstapel — jede solche Definition scheiterte mit `MissingBase`.
//!
//! `harw-registry-defaults` hängt selbst von dieser Crate ab; die Rohquellen
//! werden deshalb hier ein zweites Mal aus demselben Verzeichnisbaum
//! eingebettet (`include_dir!`, zur Laufzeit kein Dateizugriff). Die
//! Sammelregel ist dieselbe wie in
//! `harw_registry_defaults::embedded_agents` („Verzeichniskonvention“): jede
//! `.toml`-Datei unter `agents/` außer unter den reservierten Wurzeln
//! `family/`, `families/`, `organization/` und `context-programs/`.
//!
//! # Sicherheit
//! Die eingebauten Definitionen liegen ausschließlich auf
//! [`DefinitionLayer::BuiltIn`] — der niedrigsten Schicht. Sie dienen nur als
//! `extends`-/Mixin-Ziele. Welche Rolle tatsächlich startbar ist und mit
//! welchen Rechten, entscheidet weiterhin `harw-registry-defaults`
//! (`embedded_agents::builtin_agent_definitions`, `roster::AgentRoster`).
//!
//! # Nebenläufigkeit
//! Der Baum wird höchstens einmal pro Prozess geparst (`OnceLock`); danach
//! reine, threadsichere Sicht.

use std::sync::OnceLock;

use harw_agent_dsl::bind::ContextProgramLibrary;
use harw_agent_dsl::diagnostics::SourceFile;
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::parse::parse_toml;
use harw_agent_dsl::raw::RawAgentDefinition;
use include_dir::{Dir, DirEntry, include_dir};

/// Der eingebettete Baum `harw-registry-defaults/agents/`.
static BUILTIN_AGENTS_DIR: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../harw-registry-defaults/agents");

/// Reservierte Wurzelverzeichnisse, die keine Agentendefinitionen tragen
/// (Spiegel von `harw_registry_defaults::embedded_agents::NON_ROLE_ROOT_DIRS`).
const NON_ROLE_ROOT_DIRS: &[&str] = &["family", "families", "organization", "context-programs"];

/// Cache für [`builtin_definition_layers`].
static BUILTIN_LAYERS: OnceLock<Vec<RawAgentDefinition>> = OnceLock::new();

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
    for entry in BUILTIN_AGENTS_DIR.entries() {
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
/// als [`harw_agent_dsl::layers::DefinitionLayer::BuiltIn`]-Schicht.
///
/// # Description
/// Eine Datei, die nicht parst, wird mit einer Warnung übersprungen: sie ist
/// ein Defekt der eingebauten Schicht, den
/// `harw_registry_defaults::embedded_agents` ohnehin hart meldet. Die
/// Discovery darf daran nicht scheitern, solange keine Nutzerdefinition sie
/// erweitert (dann meldet die Auflösung `MissingBase` mit dem Namen).
///
/// # Returns
/// Ein statischer Ausschnitt, nach Quellpfad sortiert.
///
/// # Concurrency
/// Höchstens ein Parse-Durchlauf pro Prozess.
#[must_use]
pub fn builtin_definition_layers() -> &'static [RawAgentDefinition] {
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

/// Reserviertes Unterverzeichnis der Kontextprogramm-Bibliothek (Spiegel
/// von `harw_registry_defaults::embedded_agents`).
pub const CONTEXT_PROGRAMS_DIR: &str = "context-programs";

/// Cache für [`builtin_source_files`].
static BUILTIN_SOURCE_FILES: OnceLock<Vec<(SourceFile, Option<String>)>> = OnceLock::new();

/// Die eingebauten Definitionen als Quelldateien für
/// `harw_agent_dsl::lower_v2::lower_v2` (#22 Welle 1B), jeweils mit der
/// deklarierten `id` (einmal je Prozess ermittelt).
///
/// # Description
/// Der Pfad ist `builtin/<relativer Pfad>` — ein Etikett für Spannen in
/// Diagnosen, kein Dateisystempfad. Alle Dateien liegen auf
/// [`DefinitionLayer::BuiltIn`].
#[must_use]
pub fn builtin_source_files() -> &'static [(SourceFile, Option<String>)] {
    BUILTIN_SOURCE_FILES
        .get_or_init(|| {
            builtin_agent_sources()
                .into_iter()
                .map(|(path, source)| {
                    let file =
                        SourceFile::new(DefinitionLayer::BuiltIn, format!("builtin/{path}"), source);
                    let id = file.declared_id();
                    (file, id)
                })
                .collect()
        })
        .as_slice()
}

/// Die eingebauten Kontextprogramme als `(Name, TOML)`: jede `.toml`-Datei
/// direkt unter `agents/context-programs/`, Name ist der Dateistamm, nach
/// Namen sortiert (wie `harw_registry_defaults::embedded_agents::builtin_context_program_toml`).
#[must_use]
pub fn builtin_context_program_sources() -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if let Some(dir) = BUILTIN_AGENTS_DIR.get_dir(CONTEXT_PROGRAMS_DIR) {
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

/// Die Kontextprogramm-Bibliothek der eingebauten Schicht (#22 Welle 1B):
/// alle eingebauten Programme auf [`DefinitionLayer::BuiltIn`], mit der
/// Wurzeldecke `harw_context::ceiling::ROOT_CONTEXT_SECTIONS`. Eigene
/// Programme der Layer fügt die Discovery hinzu.
///
/// # Errors
/// Die Meldung des DSL-Crates, wenn ein eingebautes Programm nicht parst
/// (ein Defekt der eingebetteten Dateien).
pub fn builtin_context_program_library() -> Result<ContextProgramLibrary, String> {
    ContextProgramLibrary::from_builtin_sources(&builtin_context_program_sources())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_layers_contain_both_bases_but_no_context_programs() {
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
    }

    #[test]
    fn builtin_sources_and_context_programs_are_embedded() -> Result<(), String> {
        let files = builtin_source_files();
        assert!(
            files
                .iter()
                .any(|(_, id)| id.as_deref() == Some("harwness.agent.worker-base@1"))
        );
        assert!(files.iter().all(|(file, _)| file.label().starts_with("builtin/")));
        let programs = builtin_context_program_sources();
        assert!(programs.iter().any(|(name, _)| *name == "explore"));
        let library = builtin_context_program_library()?;
        assert!(library.names().contains(&"explore"));
        Ok(())
    }
}
