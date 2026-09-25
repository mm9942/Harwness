//! Discovery: which definition files exist, in which layer, and how a name
//! or a path on the command line maps to one of them.
//!
//! # Layers (ascending precedence)
//! 1. [`DefinitionLayer::BuiltIn`]: the embedded roles and bases
//!    (`harw_registry_defaults::embedded_agents::builtin_source_files`,
//!    labels `builtin/<name>.toml`).
//! 2. [`DefinitionLayer::InstalledPack`]: the definitions bundled with harw
//!    (`harw_home::bundled_files`, labels `bundled/agents/<name>/…`), but
//!    only for IDs no config layer defines (a bundled definition is usually
//!    also installed into `~/.harw/agents/`, and the same ID twice would be
//!    composed as an overlay).
//! 3. The config layers (`~/.harw`, the active profile, a trusted project
//!    `.harw`): `agents/<name>/definition.toml` and the older flat
//!    `agents/<name>.toml`, like `harw-config`. The last of several layers
//!    is [`DefinitionLayer::Project`], the others
//!    [`DefinitionLayer::UserGlobal`].
//! 4. An explicit path from the command line: [`DefinitionLayer::RunLocal`];
//!    every other file with the same ID is dropped, so the file is compiled
//!    exactly as written.
//!
//! A file that does not parse is kept out of the shared layer stack (one
//! broken definition must not break every other compile) and only compiled
//! when it is the target, where its parse diagnostics are the answer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use harw_agent_dsl::bind::ContextProgramLibrary;
use harw_agent_dsl::diagnostics::SourceFile;
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::lower_v2::{
    FsInstructionsLoader, InstructionsLoader, LowerSources, MapInstructionsLoader,
};
use harw_agent_dsl::parse::parse_toml;
use harw_agent_dsl::roles::AgentRoleId;

use crate::error::CompileError;

/// Label prefix of bundled definitions (virtual, never read from disk).
pub const BUNDLED_PREFIX: &str = "bundled";

/// Label prefix of built-in definitions (virtual).
pub const BUILTIN_PREFIX: &str = "builtin";

/// Where a definition comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Embedded built-in role or base.
    BuiltIn,
    /// Bundled with harw (`harw-home` assets).
    Bundled,
    /// A config layer directory.
    Layer(PathBuf),
    /// A path given on the command line.
    Explicit,
}

impl Origin {
    /// Short label (`builtin`, `bundled`, `layer`, `path`).
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::BuiltIn => "builtin",
            Self::Bundled => "bundled",
            Self::Layer(_) => "layer",
            Self::Explicit => "path",
        }
    }
}

/// One discovered definition file.
#[derive(Debug, Clone)]
pub struct DefinitionEntry {
    /// Lookup name: directory name, file stem or built-in role name.
    pub name: String,
    /// Declared ID.
    pub id: DefinitionId,
    /// Declared role.
    pub role: AgentRoleId,
    /// Declared specialization.
    pub specialization: String,
    /// Layer of the file.
    pub layer: DefinitionLayer,
    /// The source label (a real path for layer and explicit files).
    pub label: PathBuf,
    /// Origin.
    pub origin: Origin,
    /// Directory on disk that holds the definition (layer and explicit
    /// files only): `tests/`, `system.md`, `fmt`.
    pub dir: Option<PathBuf>,
}

/// A definition file that does not parse.
#[derive(Debug, Clone)]
pub struct BrokenDefinition {
    /// Lookup name.
    pub name: String,
    /// The file.
    pub file: SourceFile,
}

/// Instruction loader over bundled texts (in memory) and real files.
#[derive(Debug, Clone, Default)]
pub struct CompositeLoader {
    memory: MapInstructionsLoader,
    fs: FsInstructionsLoader,
}

impl CompositeLoader {
    /// Registers an in-memory file under `label_dir/file`.
    pub fn insert(&mut self, path: impl Into<PathBuf>, text: impl Into<String>) {
        self.memory.insert(path, text);
    }
}

impl InstructionsLoader for CompositeLoader {
    fn load(&self, agent_dir: &Path, file: &str) -> Result<Option<String>, String> {
        if let Some(text) = self.memory.load(agent_dir, file)? {
            return Ok(Some(text));
        }
        if agent_dir.starts_with(BUNDLED_PREFIX) || agent_dir.starts_with(BUILTIN_PREFIX) {
            return Ok(None);
        }
        self.fs.load(agent_dir, file)
    }
}

/// All definition sources the compiler knows.
#[derive(Debug, Clone)]
pub struct SourceSet {
    /// Every parseable file, all layers.
    pub files: Vec<SourceFile>,
    /// One entry per parseable file (same order as `files`).
    pub entries: Vec<DefinitionEntry>,
    /// Files that do not parse.
    pub broken: Vec<BrokenDefinition>,
    /// Instruction loader.
    pub loader: CompositeLoader,
    /// Context-program library (built-in plus layer programs).
    pub programs: ContextProgramLibrary,
}

impl SourceSet {
    /// Discovers built-in, bundled and layer definitions.
    ///
    /// # Errors
    /// [`CompileError::Other`] if the built-in context-program library is
    /// broken (a build defect, not a user error).
    pub fn discover(layers: &[PathBuf]) -> Result<Self, CompileError> {
        let mut programs =
            harw_registry_defaults::embedded_agents::builtin_context_program_library()
                .map_err(|error| CompileError::Other(format!("built-in context programs: {error}")))?;
        let mut set = Self {
            files: Vec::new(),
            entries: Vec::new(),
            broken: Vec::new(),
            loader: CompositeLoader::default(),
            programs: ContextProgramLibrary::new(),
        };
        for file in harw_registry_defaults::embedded_agents::builtin_source_files() {
            let name = file
                .path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_owned();
            set.push(name, file, Origin::BuiltIn, None);
        }

        let mut layer_files: Vec<(String, SourceFile, Origin, Option<PathBuf>)> = Vec::new();
        for (index, layer_dir) in layers.iter().enumerate() {
            let layer = if layers.len() > 1 && index + 1 == layers.len() {
                DefinitionLayer::Project
            } else {
                DefinitionLayer::UserGlobal
            };
            collect_layer(layer_dir, layer, &mut layer_files);
            collect_programs(layer_dir, layer, &mut programs);
        }
        let layer_ids: BTreeSet<String> = layer_files
            .iter()
            .filter_map(|(_, file, _, _)| file.declared_id())
            .collect();

        for bundled in harw_home::bundled_files() {
            let Some(rest) = bundled.relative_path.strip_prefix("agents/") else {
                continue;
            };
            let Some((name, file_name)) = rest.split_once('/') else {
                continue;
            };
            let label_dir = Path::new(BUNDLED_PREFIX).join("agents").join(name);
            if file_name == "definition.toml" {
                let file = SourceFile::new(
                    DefinitionLayer::InstalledPack,
                    label_dir.join("definition.toml"),
                    bundled.contents,
                );
                if file
                    .declared_id()
                    .is_some_and(|id| layer_ids.contains(&id))
                {
                    continue;
                }
                set.push(name.to_owned(), file, Origin::Bundled, None);
            } else if file_name.ends_with(".md") && !file_name.contains('/') {
                set.loader.insert(label_dir.join(file_name), bundled.contents);
            }
        }
        for (name, file, origin, dir) in layer_files {
            set.push(name, file, origin, dir);
        }
        set.programs = programs;
        Ok(set)
    }

    fn push(&mut self, name: String, file: SourceFile, origin: Origin, dir: Option<PathBuf>) {
        match parse_toml(&file.text) {
            Ok(raw) => {
                self.entries.push(DefinitionEntry {
                    name,
                    id: raw.id,
                    role: raw.role,
                    specialization: raw.specialization,
                    layer: file.layer,
                    label: file.path.clone(),
                    origin,
                    dir,
                });
                self.files.push(file);
            }
            Err(_) => self.broken.push(BrokenDefinition { name, file }),
        }
    }

    /// Adds an explicit definition file (a path from the command line) at
    /// [`DefinitionLayer::RunLocal`] and drops every other file with its ID.
    ///
    /// # Errors
    /// [`CompileError::NotADefinition`] if the path is neither a definition
    /// file nor a directory with `definition.toml`, or cannot be read.
    pub fn add_explicit(&mut self, path: &Path) -> Result<Target, CompileError> {
        let file_path = if path.is_dir() {
            let candidate = path.join("definition.toml");
            if !candidate.is_file() {
                return Err(CompileError::NotADefinition {
                    path: path.to_path_buf(),
                    reason: "the directory has no definition.toml".to_owned(),
                });
            }
            candidate
        } else {
            path.to_path_buf()
        };
        let text = std::fs::read_to_string(&file_path).map_err(|error| {
            CompileError::NotADefinition {
                path: file_path.clone(),
                reason: error.to_string(),
            }
        })?;
        let dir = file_path.parent().map(Path::to_path_buf);
        let name = name_for(&file_path);
        let file = SourceFile::new(DefinitionLayer::RunLocal, file_path.clone(), text);
        match parse_toml(&file.text) {
            Ok(raw) => {
                let id = raw.id.clone();
                let keep: Vec<bool> = self.entries.iter().map(|entry| entry.id != id).collect();
                let mut index = 0;
                self.files.retain(|_| {
                    let keep_it = keep[index];
                    index += 1;
                    keep_it
                });
                self.entries.retain(|entry| entry.id != id);
                let entry = DefinitionEntry {
                    name,
                    id: raw.id,
                    role: raw.role,
                    specialization: raw.specialization,
                    layer: DefinitionLayer::RunLocal,
                    label: file_path,
                    origin: Origin::Explicit,
                    dir,
                };
                self.entries.push(entry.clone());
                self.files.push(file);
                Ok(Target::Entry(entry))
            }
            Err(_) => Ok(Target::Broken(BrokenDefinition { name, file })),
        }
    }

    /// Finds a definition by name (see module docs for the order).
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&DefinitionEntry> {
        let by_precedence = || {
            let mut sorted: Vec<(usize, &DefinitionEntry)> =
                self.entries.iter().enumerate().collect();
            sorted.sort_by(|(ia, a), (ib, b)| b.layer.cmp(&a.layer).then(ib.cmp(ia)));
            sorted.into_iter().map(|(_, entry)| entry)
        };
        by_precedence()
            .find(|entry| entry.name == name)
            .or_else(|| by_precedence().find(|entry| entry.specialization == name))
            .or_else(|| {
                by_precedence().find(|entry| entry.id.to_string() == name || entry.id.name == name)
            })
    }

    /// Finds a definition by its exact ID.
    #[must_use]
    pub fn find_id(&self, id: &str) -> Option<&DefinitionEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.id.to_string() == id)
            .max_by_key(|entry| entry.layer)
    }

    /// A broken definition with this lookup name.
    #[must_use]
    pub fn find_broken(&self, name: &str) -> Option<&BrokenDefinition> {
        self.broken.iter().rev().find(|broken| broken.name == name)
    }

    /// Names similar to `name` (edit distance ≤ 3 or substring), sorted.
    #[must_use]
    pub fn suggestions(&self, name: &str) -> Vec<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        for entry in &self.entries {
            if matches!(entry.origin, Origin::BuiltIn)
                && harw_registry_defaults::embedded_agents::BASE_DEFINITION_NAMES
                    .contains(&entry.name.as_str())
            {
                continue;
            }
            if edit_distance(&entry.name, name) <= 3
                || (name.len() >= 3 && entry.name.contains(name))
            {
                out.insert(entry.name.clone());
            }
        }
        out.into_iter().take(5).collect()
    }

    /// Every agent a user can compile (no bases), unique by name, sorted.
    #[must_use]
    pub fn compilable_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| {
                !harw_registry_defaults::embedded_agents::BASE_DEFINITION_NAMES
                    .contains(&entry.name.as_str())
            })
            .map(|entry| entry.name.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The sources for [`harw_agent_dsl::lower_v2::compile_agent`].
    #[must_use]
    pub fn lower_sources(&self) -> LowerSources<'_> {
        LowerSources::new(&self.files)
            .with_instructions(&self.loader)
            .with_context_programs(&self.programs)
    }

    /// The source file with this label.
    #[must_use]
    pub fn file(&self, label: &Path) -> Option<&SourceFile> {
        self.files
            .iter()
            .chain(self.broken.iter().map(|broken| &broken.file))
            .find(|file| file.path == label)
    }
}

/// The result of resolving a command-line argument.
#[derive(Debug, Clone)]
pub enum Target {
    /// A parseable definition.
    Entry(DefinitionEntry),
    /// A definition file that does not parse.
    Broken(BrokenDefinition),
}

/// A command-line agent argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentInput {
    /// A definition name from the layers.
    Name(String),
    /// A definition directory or file.
    Path(PathBuf),
}

impl AgentInput {
    /// Interprets `raw`: a path if it contains a separator, ends in `.toml`
    /// or names an existing file or directory relative to `cwd`; otherwise a
    /// name.
    #[must_use]
    pub fn parse(raw: &str, cwd: &Path) -> Self {
        let looks_like_path = raw.contains('/')
            || raw.contains(std::path::MAIN_SEPARATOR)
            || raw.ends_with(".toml")
            || raw == "."
            || raw == "..";
        let candidate = Path::new(raw);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            cwd.join(candidate)
        };
        if looks_like_path || resolved.join("definition.toml").is_file() {
            Self::Path(resolved)
        } else {
            Self::Name(raw.to_owned())
        }
    }

    /// The argument as the user wrote it (for messages).
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Name(name) => name.clone(),
            Self::Path(path) => path.display().to_string(),
        }
    }
}

/// The lookup name of a definition file: its directory name for
/// `definition.toml`, else its file stem.
#[must_use]
pub fn name_for(file: &Path) -> String {
    let is_definition = file.file_name().and_then(|name| name.to_str()) == Some("definition.toml");
    let named = if is_definition {
        file.parent().and_then(Path::file_name)
    } else {
        file.file_stem()
    };
    named
        .and_then(|name| name.to_str())
        .unwrap_or("agent")
        .to_owned()
}

/// Collects `agents/<name>/definition.toml` and `agents/<name>.toml` of one
/// layer (sorted; a directory definition shadows a flat file of the same
/// ID, as in `harw-config`).
fn collect_layer(
    layer_dir: &Path,
    layer: DefinitionLayer,
    out: &mut Vec<(String, SourceFile, Origin, Option<PathBuf>)>,
) {
    let agents = layer_dir.join("agents");
    let Ok(read_dir) = std::fs::read_dir(&agents) else {
        return;
    };
    let mut paths: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'))
        })
        .collect();
    paths.sort();
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    let mut flat: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_dir() {
            let definition = path.join("definition.toml");
            if let Ok(text) = std::fs::read_to_string(&definition) {
                let file = SourceFile::new(layer, definition.clone(), text);
                if let Some(id) = file.declared_id() {
                    seen_ids.insert(id);
                }
                out.push((
                    name_for(&definition),
                    file,
                    Origin::Layer(layer_dir.to_path_buf()),
                    Some(path.clone()),
                ));
            }
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
            flat.push(path);
        }
    }
    for path in flat {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let file = SourceFile::new(layer, path.clone(), text);
        if file.declared_id().is_some_and(|id| seen_ids.contains(&id)) {
            continue;
        }
        out.push((
            name_for(&path),
            file,
            Origin::Layer(layer_dir.to_path_buf()),
            path.parent().map(Path::to_path_buf),
        ));
    }
}

/// Registers `<layer>/context-programs/*.toml` (unparseable programs are
/// skipped; the lowering then reports `HARW-CTX-001` for a binding to them).
fn collect_programs(layer_dir: &Path, layer: DefinitionLayer, library: &mut ContextProgramLibrary) {
    let Ok(read_dir) = std::fs::read_dir(layer_dir.join("context-programs")) else {
        return;
    };
    let mut paths: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .collect();
    paths.sort();
    for path in paths {
        let (Some(name), Ok(text)) = (
            path.file_stem().and_then(|stem| stem.to_str()),
            std::fs::read_to_string(&path),
        ) else {
            continue;
        };
        if let Err(error) = library.insert_sources(layer, &[(name, text.as_str())]) {
            // A broken user program must not break unrelated compiles.
            let _ = error;
        }
    }
}

/// Levenshtein distance (small inputs only).
#[must_use]
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            let value = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
            current.push(value);
        }
        previous = current;
    }
    previous[b_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_parse_distinguishes_names_and_paths() {
        let cwd = Path::new("/nonexistent-cwd");
        assert_eq!(
            AgentInput::parse("evidence-critic", cwd),
            AgentInput::Name("evidence-critic".to_owned())
        );
        assert_eq!(
            AgentInput::parse("./agents/x", cwd),
            AgentInput::Path(PathBuf::from("/nonexistent-cwd/./agents/x"))
        );
        assert_eq!(
            AgentInput::parse("/abs/definition.toml", cwd),
            AgentInput::Path(PathBuf::from("/abs/definition.toml"))
        );
    }

    #[test]
    fn test_name_for_definition_and_flat_files() {
        assert_eq!(name_for(Path::new("/a/agents/critic/definition.toml")), "critic");
        assert_eq!(name_for(Path::new("/a/agents/flat.toml")), "flat");
    }

    #[test]
    fn test_builtin_and_bundled_definitions_are_found() -> Result<(), CompileError> {
        let set = SourceSet::discover(&[])?;
        assert!(set.find("explorer").is_some(), "built-in role");
        let critic = set.find("evidence-critic");
        assert_eq!(
            critic.map(|entry| entry.origin.clone()),
            Some(Origin::Bundled),
            "bundled definition"
        );
        assert!(set.find("no-such-agent").is_none());
        assert!(set.suggestions("evidence-critc").contains(&"evidence-critic".to_owned()));
        Ok(())
    }

    #[test]
    fn test_layer_definition_shadows_the_bundled_copy() -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        let dir = home.path().join("agents").join("evidence-critic");
        std::fs::create_dir_all(&dir)?;
        let bundled = harw_home::bundled_files()
            .iter()
            .find(|file| file.relative_path == "agents/evidence-critic/definition.toml")
            .map(|file| file.contents)
            .ok_or("bundled evidence-critic")?;
        std::fs::write(dir.join("definition.toml"), bundled)?;
        let set = SourceSet::discover(&[home.path().to_path_buf()])?;
        let found = set.find("evidence-critic").ok_or("found")?;
        assert!(matches!(found.origin, Origin::Layer(_)));
        let copies = set
            .entries
            .iter()
            .filter(|entry| entry.id.to_string() == "harwness.agent.evidence-critic@1")
            .count();
        assert_eq!(copies, 1, "no overlay of identical copies");
        Ok(())
    }

    #[test]
    fn test_edit_distance() {
        assert_eq!(edit_distance("critic", "critc"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
    }
}
