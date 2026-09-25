//! Lowering to Agent IR v2: `ResolvedAgentDefinition` + sources → [`AgentIr`]
//! (`docs/design/agent-ir-v1.md` §6.1, #22 wave 1).
//!
//! [`lower_v2`] lowers **every** top-level table of the resolved
//! configuration or reports it: a table this version lowers is typed into
//! its IR section; a table the DSL documents but this version does not lower
//! yet is a `HARW-SCHEMA-003` warning; anything else is a `HARW-SCHEMA-002`
//! error (with a spelling suggestion). Unknown keys inside a known table are
//! `HARW-SCHEMA-004`, wrongly typed values `HARW-PARSE-003`, negative or
//! overflowing integers `HARW-PARSE-004`. Nothing is dropped silently and no
//! value is invented from malformed input.
//!
//! [`compile_agent`] runs the whole front end over a set of
//! [`SourceFile`]s — parse (with spans), resolve, lower — and turns every
//! failure into [`Diagnostics`].
//!
//! # Lowered tables
//! | Table (alias) | IR section |
//! |---|---|
//! | `[tools]` (`[tool_surface]`) | [`ToolSurface`] |
//! | `[spawn]` (`[spawn_contract]`), `[spawn.budget]` | [`SpawnContract`], [`Budget`] |
//! | `[delegation]` | [`SpawnContract::delegation_targets`] |
//! | `[job]` (`[job_template]`) | [`Job`] |
//! | `[lifecycle]` (`[lifecycle_machine]`) | [`Lifecycle`] |
//! | `[context]` (`[context_program]`) | [`ContextProgram`] (with the bound program) |
//! | `[return]` (`[return_pipeline]`) | [`ReturnPipeline`] |
//! | `[models]` | [`Models`] |
//! | `[limits]` | [`Limits`] |
//! | `[work]` | [`Work`] |
//! | `[research]` | [`Research`] |
//! | `[verification]` | [`Verification`] |
//! | `[binary]` | [`Binary`] |
//! | `[network]` | [`Permissions::network`] (`hosts`) |
//! | `[authority]` | [`Authority`] (from the resolver) |
//! | `instructions_file` | [`Instructions`] |
//! | `skills` | [`Skills`] |
//!
//! Forward-compatible, documented but not lowered: `[compatibility]`,
//! `[binding]`, `[scope]`, `[requires]`, `[metadata]`.
//!
//! # Concurrency
//! Pure apart from the [`InstructionsLoader`] the caller supplies.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use time::OffsetDateTime;

use crate::bind::{BoundContextProgram, CONTEXT_PROGRAM_KEY, ContextProgramLibrary};
use crate::diagnostics::{
    Diagnostic, DiagnosticCode, Diagnostics, Severity, SourceFile, SourceIndex, codes, parse_source,
};
use crate::error::DslError;
use crate::ids::DefinitionId;
use crate::ir_v2::{
    AGENT_IR_SCHEMA, AgentIr, Authority, Binary, Budget, ContextProgram, ContextSection, Effort,
    FilesystemPermissions, Instructions, Interface, Job, Lifecycle, Limits, ModelRef, Models,
    NetworkMode, NetworkPermissions, Permissions, Research, ReturnContract, ReturnPipeline,
    SkillEntry, Skills, SpawnContract, SpawnPermissions, ToolSurface, Trace, TraceStep,
    Verification, WORKSPACE_WRITE_PATH, Work,
};
use crate::raw::RawAgentDefinition;
use crate::resolve::resolve_definition;
use crate::resolved::ResolvedAgentDefinition;
use crate::roles::{AgentRoleId, can_spawn};
use crate::skills::{SKILLS_CONFIG_KEY, skills_from_config_strict};

/// Top-level key naming the instruction file relative to the agent directory.
pub const INSTRUCTIONS_FILE_KEY: &str = "instructions_file";

/// Instruction file looked up next to the definition when
/// [`INSTRUCTIONS_FILE_KEY`] is absent.
pub const DEFAULT_INSTRUCTIONS_FILE: &str = "system.md";

/// Tables lowered into the IR (including aliases).
pub const LOWERED_TABLES: &[&str] = &[
    "tools",
    "tool_surface",
    "spawn",
    "spawn_contract",
    "delegation",
    "job",
    "job_template",
    "lifecycle",
    "lifecycle_machine",
    "context",
    "context_program",
    "return",
    "return_pipeline",
    "models",
    "limits",
    "work",
    "research",
    "verification",
    "binary",
    "network",
    "authority",
];

/// Tables the DSL documents but this version does not lower (warning).
pub const FORWARD_COMPATIBLE_TABLES: &[&str] =
    &["compatibility", "binding", "scope", "requires", "metadata"];

/// Loads instruction files for [`lower_v2`].
///
/// # Description
/// `agent_dir` is the directory of the definition file that names the
/// instruction file; `file` is the declared relative name. `Ok(None)` means
/// "no such file" (an error for an explicit `instructions_file`, silence for
/// the implicit `system.md`). Implementations must reject paths that leave
/// `agent_dir` ([`validate_relative_file`]).
pub trait InstructionsLoader {
    /// Loads `file` relative to `agent_dir`.
    ///
    /// # Errors
    /// A human-readable reason if the path is invalid or unreadable.
    fn load(&self, agent_dir: &Path, file: &str) -> Result<Option<String>, String>;
}

/// Checks that `file` is a non-empty relative path without `..` or a root.
///
/// # Errors
/// A reason naming the offending path.
pub fn validate_relative_file(file: &str) -> Result<PathBuf, String> {
    if file.trim().is_empty() {
        return Err("the instruction file name is empty".to_owned());
    }
    let path = Path::new(file);
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => {
                return Err(format!(
                    "'{file}' must be a relative path inside the agent directory (no '..', no absolute path)"
                ));
            }
        }
    }
    Ok(path.to_path_buf())
}

/// Loads instruction files from the filesystem.
///
/// # Description
/// Same rules as `harw_config::loader::load_agent_instructions`: relative
/// path only, symlinks are resolved and must stay inside the agent
/// directory, at most [`Self::DEFAULT_MAX_BYTES`] (configurable), UTF-8.
#[derive(Debug, Clone, Copy)]
pub struct FsInstructionsLoader {
    max_bytes: u64,
}

impl FsInstructionsLoader {
    /// Default size limit (64 KiB, as in `harw-config`).
    pub const DEFAULT_MAX_BYTES: u64 = 64 * 1024;

    /// A loader with the default size limit.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_bytes: Self::DEFAULT_MAX_BYTES,
        }
    }

    /// A loader with a custom size limit.
    #[must_use]
    pub fn with_max_bytes(max_bytes: u64) -> Self {
        Self { max_bytes }
    }
}

impl Default for FsInstructionsLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl InstructionsLoader for FsInstructionsLoader {
    fn load(&self, agent_dir: &Path, file: &str) -> Result<Option<String>, String> {
        let relative = validate_relative_file(file)?;
        let path = agent_dir.join(relative);
        let resolved = match std::fs::canonicalize(&path) {
            Ok(resolved) => resolved,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let root = std::fs::canonicalize(agent_dir)
            .map_err(|error| format!("{}: {error}", agent_dir.display()))?;
        if !resolved.starts_with(root.as_path()) {
            return Err(format!(
                "'{file}' resolves outside the agent directory {}",
                agent_dir.display()
            ));
        }
        let metadata =
            std::fs::metadata(&resolved).map_err(|error| format!("{}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("{} is not a regular file", path.display()));
        }
        if metadata.len() > self.max_bytes {
            return Err(format!(
                "{} is larger than {} bytes",
                path.display(),
                self.max_bytes
            ));
        }
        std::fs::read_to_string(resolved)
            .map(Some)
            .map_err(|error| format!("{}: {error}", path.display()))
    }
}

/// Serves instruction files from memory (embedded assets, tests).
///
/// # Description
/// Keys are `agent_dir.join(file)` exactly as [`lower_v2`] computes them
/// from the [`SourceFile::path`] of the definition.
#[derive(Debug, Clone, Default)]
pub struct MapInstructionsLoader {
    files: BTreeMap<PathBuf, String>,
}

impl MapInstructionsLoader {
    /// An empty loader.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `text` under `path` (agent directory joined with the file).
    pub fn insert(&mut self, path: impl Into<PathBuf>, text: impl Into<String>) {
        self.files.insert(path.into(), text.into());
    }
}

impl InstructionsLoader for MapInstructionsLoader {
    fn load(&self, agent_dir: &Path, file: &str) -> Result<Option<String>, String> {
        let relative = validate_relative_file(file)?;
        Ok(self.files.get(&agent_dir.join(relative)).cloned())
    }
}

/// Everything [`lower_v2`] needs besides the resolved definition.
///
/// # Description
/// - `files`: the TOML sources of all layers (spans, the target's own
///   tables, the directory of `instructions_file`).
/// - `instructions`: the instruction loader; without one an explicit
///   `instructions_file` is an error and no `system.md` is looked up.
/// - `context_programs`: the context-program library; without one a
///   `[context] program` is `HARW-CTX-001`.
#[derive(Clone, Copy)]
pub struct LowerSources<'a> {
    /// Source files of all layers.
    pub files: &'a [SourceFile],
    /// Instruction loader.
    pub instructions: Option<&'a dyn InstructionsLoader>,
    /// Context-program library.
    pub context_programs: Option<&'a ContextProgramLibrary>,
}

impl std::fmt::Debug for LowerSources<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LowerSources")
            .field("files", &self.files.len())
            .field("instructions", &self.instructions.is_some())
            .field("context_programs", &self.context_programs.is_some())
            .finish()
    }
}

impl<'a> LowerSources<'a> {
    /// Sources without loader and library.
    #[must_use]
    pub fn new(files: &'a [SourceFile]) -> Self {
        Self {
            files,
            instructions: None,
            context_programs: None,
        }
    }

    /// No files, no loader, no library.
    #[must_use]
    pub fn empty() -> LowerSources<'static> {
        LowerSources {
            files: &[],
            instructions: None,
            context_programs: None,
        }
    }

    /// Sets the instruction loader.
    #[must_use]
    pub fn with_instructions(mut self, loader: &'a dyn InstructionsLoader) -> Self {
        self.instructions = Some(loader);
        self
    }

    /// Sets the context-program library.
    #[must_use]
    pub fn with_context_programs(mut self, library: &'a ContextProgramLibrary) -> Self {
        self.context_programs = Some(library);
        self
    }
}

/// Maps a resolver error to a diagnostic with a span from `files`.
///
/// # Description
/// The span is looked up in the file of the definition the error is about
/// (the definition with the bad `schema` or skill list), else in the
/// target's files.
#[must_use]
pub fn diagnostic_for_error(
    error: &DslError,
    files: &[SourceFile],
    target: &DefinitionId,
) -> Diagnostic {
    let owner = match error {
        DslError::SchemaMismatch { of, .. } | DslError::InvalidSkill { of, .. } => of.to_string(),
        _ => target.to_string(),
    };
    let index = SourceIndex::new(files, Some(owner.as_str()));
    let diagnostic = Diagnostic::from_dsl_error(error);
    let span = diagnostic
        .path
        .as_deref()
        .and_then(|path| index.span_for(path));
    diagnostic.with_span(span)
}

/// Parses, resolves and lowers `target_id` from `sources.files`.
///
/// # Description
/// Every file is parsed with [`parse_source`] (syntax and structure errors
/// with spans), all files form the layer stack for
/// [`resolve_definition`] (errors mapped by [`diagnostic_for_error`]), and
/// the result is lowered by [`lower_v2`]. `now` stamps the resolution trace.
///
/// # Errors
/// [`Diagnostics`] containing at least one error.
pub fn compile_agent(
    target_id: &DefinitionId,
    sources: &LowerSources<'_>,
    now: OffsetDateTime,
) -> Result<AgentIr, Diagnostics> {
    let mut diagnostics = Diagnostics::new();
    let mut layers: Vec<(crate::layers::DefinitionLayer, RawAgentDefinition)> =
        Vec::with_capacity(sources.files.len());
    for file in sources.files {
        match parse_source(file) {
            Ok(raw) => layers.push((file.layer, raw)),
            Err(errors) => diagnostics.extend(errors),
        }
    }
    if diagnostics.has_errors() {
        return Err(diagnostics);
    }
    let resolved = resolve_definition(target_id, &layers, now).map_err(|error| {
        Diagnostics::from(diagnostic_for_error(&error, sources.files, target_id))
    })?;
    lower_v2(&resolved, sources)
}

/// Lowering context: span index plus collected diagnostics.
struct Cx<'s> {
    index: SourceIndex<'s>,
    diagnostics: Diagnostics,
}

impl Cx<'_> {
    /// Records a diagnostic at `path` (span from the sources); identical
    /// diagnostics are recorded once.
    fn report(&mut self, code: &DiagnosticCode, path: &str, message: impl Into<String>) {
        let span = self.index.span_for(path);
        let diagnostic = Diagnostic::new(code, message)
            .with_path(path)
            .with_span(span);
        self.push(diagnostic);
    }

    /// Like [`Self::report`] with a specific help text.
    fn report_help(
        &mut self,
        code: &DiagnosticCode,
        path: &str,
        message: impl Into<String>,
        help: impl Into<String>,
    ) {
        let span = self.index.span_for(path);
        let diagnostic = Diagnostic::new(code, message)
            .with_help(help)
            .with_path(path)
            .with_span(span);
        self.push(diagnostic);
    }

    fn push(&mut self, diagnostic: Diagnostic) {
        let duplicate = self.diagnostics.iter().any(|existing| {
            existing.code == diagnostic.code
                && existing.path == diagnostic.path
                && existing.message == diagnostic.message
        });
        if !duplicate {
            self.diagnostics.push(diagnostic);
        }
    }

    /// Records a resolver-style error with a span.
    fn push_error(&mut self, error: &DslError) {
        let diagnostic = Diagnostic::from_dsl_error(error);
        let span = diagnostic
            .path
            .as_deref()
            .and_then(|path| self.index.span_for(path));
        self.push(diagnostic.with_span(span));
    }

    fn wrong_type(&mut self, path: &str, expected: &str, value: &toml::Value) {
        self.report(
            &codes::PARSE_WRONG_TYPE,
            path,
            format!("`{path}` must be {expected}, found {}", value.type_str()),
        );
    }

    /// Looks up a section under its name or alias.
    fn section<'c>(
        &mut self,
        config: &'c toml::Table,
        names: &[&'static str],
    ) -> Option<(&'c toml::Table, &'static str)> {
        let present: Vec<&'static str> = names
            .iter()
            .copied()
            .filter(|name| config.contains_key(*name))
            .collect();
        let first = *present.first()?;
        if let Some(second) = present.get(1) {
            self.report(
                &codes::SCHEMA_ALIAS_CONFLICT,
                second,
                format!("`[{first}]` and its alias `[{second}]` are both present"),
            );
        }
        match config.get(first) {
            Some(toml::Value::Table(table)) => Some((table, first)),
            Some(other) => {
                self.wrong_type(first, "a table", other);
                None
            }
            None => None,
        }
    }

    /// Reports every key of `table` not in `allowed`.
    fn check_keys(&mut self, table: &toml::Table, prefix: &str, allowed: &[&str]) {
        for key in table.keys() {
            if allowed.contains(&key.as_str()) {
                continue;
            }
            let path = join(prefix, key);
            let suggestion = suggest(key, allowed)
                .map(|known| format!("did you mean `{known}`? "))
                .unwrap_or_default();
            self.report_help(
                &codes::SCHEMA_UNKNOWN_KEY,
                &path,
                format!("unknown key `{key}` in `[{prefix}]`"),
                format!("{suggestion}known keys: {}", allowed.join(", ")),
            );
        }
    }

    fn opt_str(&mut self, table: &toml::Table, prefix: &str, key: &str) -> Option<String> {
        match table.get(key)? {
            toml::Value::String(value) => Some(value.clone()),
            other => {
                self.wrong_type(&join(prefix, key), "a string", other);
                None
            }
        }
    }

    fn opt_bool(&mut self, table: &toml::Table, prefix: &str, key: &str) -> Option<bool> {
        match table.get(key)? {
            toml::Value::Boolean(value) => Some(*value),
            other => {
                self.wrong_type(&join(prefix, key), "a boolean", other);
                None
            }
        }
    }

    fn opt_int(&mut self, table: &toml::Table, prefix: &str, key: &str) -> Option<i64> {
        match table.get(key)? {
            toml::Value::Integer(value) => Some(*value),
            other => {
                self.wrong_type(&join(prefix, key), "an integer", other);
                None
            }
        }
    }

    fn opt_u32(&mut self, table: &toml::Table, prefix: &str, key: &str) -> Option<u32> {
        let raw = self.opt_int(table, prefix, key)?;
        match u32::try_from(raw) {
            Ok(value) => Some(value),
            Err(_) => {
                let path = join(prefix, key);
                self.report(
                    &codes::PARSE_OUT_OF_RANGE,
                    &path,
                    format!("`{path}` = {raw} is outside 0..={}", u32::MAX),
                );
                None
            }
        }
    }

    fn opt_u64(&mut self, table: &toml::Table, prefix: &str, key: &str) -> Option<u64> {
        let raw = self.opt_int(table, prefix, key)?;
        match u64::try_from(raw) {
            Ok(value) => Some(value),
            Err(_) => {
                let path = join(prefix, key);
                self.report(
                    &codes::PARSE_OUT_OF_RANGE,
                    &path,
                    format!("`{path}` = {raw} is negative"),
                );
                None
            }
        }
    }

    /// A string list; wrongly typed elements are reported and skipped.
    fn opt_strings(&mut self, table: &toml::Table, prefix: &str, key: &str) -> Option<Vec<String>> {
        let path = join(prefix, key);
        let values = match table.get(key)? {
            toml::Value::Array(values) => values,
            other => {
                self.wrong_type(&path, "an array of strings", other);
                return None;
            }
        };
        let mut strings = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            match value {
                toml::Value::String(value) => strings.push(value.clone()),
                other => self.wrong_type(&format!("{path}[{index}]"), "a string", other),
            }
        }
        Some(strings)
    }

    /// Parses an effort label or reports `HARW-MODEL-001`.
    fn effort(&mut self, label: &str, path: &str) -> Option<Effort> {
        let effort = Effort::parse(label);
        if effort.is_none() {
            self.report(
                &codes::MODEL_UNKNOWN_EFFORT,
                path,
                format!("unknown effort `{label}` at `{path}`"),
            );
        }
        effort
    }
}

/// `prefix.key`, or `key` without a prefix.
fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Levenshtein distance (small inputs only).
fn edit_distance(left: &str, right: &str) -> usize {
    let right_chars: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right_chars.len()).collect();
    for (i, left_char) in left.chars().enumerate() {
        let mut current = Vec::with_capacity(right_chars.len() + 1);
        current.push(i + 1);
        for (j, right_char) in right_chars.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_char != *right_char);
            let insertion = current[j] + 1;
            let deletion = previous[j + 1] + 1;
            current.push(substitution.min(insertion).min(deletion));
        }
        previous = current;
    }
    previous[right_chars.len()]
}

/// The closest known name within distance 2, if any.
fn suggest<'k>(unknown: &str, known: &[&'k str]) -> Option<&'k str> {
    known
        .iter()
        .map(|candidate| (edit_distance(unknown, candidate), *candidate))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// Tool names are non-empty and contain no whitespace or control characters.
fn valid_label(label: &str) -> bool {
    !label.is_empty() && !label.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// `[A-Z_][A-Z0-9_]*`
fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_uppercase() || first == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// `[a-z0-9][a-z0-9._-]{0,63}`
fn valid_binary_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// A valid binary name derived from the specialization.
fn default_binary_name(specialization: &str) -> String {
    let mut name: String = specialization
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        name = format!("agent-{name}");
        name.truncate(64);
    }
    name
}

/// `true` if `table` has `[context] program`.
fn binds_program(table: &toml::Table) -> bool {
    table
        .get("context")
        .and_then(toml::Value::as_table)
        .is_some_and(|context| context.contains_key(CONTEXT_PROGRAM_KEY))
}

/// The target's own top-level tables (highest-priority target file first).
///
/// # Description
/// Used for the context-program binding (the resolver drops a child's
/// `[context]` when a base has one). Picks the highest-layer target file
/// that binds a program, else the highest-layer target file, else nothing.
fn own_tables(target_files: &[&SourceFile]) -> toml::Table {
    let parsed: Vec<toml::Table> = target_files
        .iter()
        .filter_map(|file| toml::from_str::<toml::Table>(&file.text).ok())
        .collect();
    parsed
        .iter()
        .find(|table| binds_program(table))
        .or_else(|| parsed.first())
        .cloned()
        .unwrap_or_default()
}

/// Reports `HARW-RESOLVE-004` for own tables the resolver replaced by a
/// base's or mixin's table.
///
/// # Description
/// The resolver composes a definition's own tables under its base and
/// mixins with "existing key wins" (`resolve::merge_tables`), so an own
/// `[work]` is ignored when the base has one. This is only checked for a
/// single target file (with several layers, higher layers legitimately
/// override the lowest one). Arrays are merged, not replaced, and are
/// skipped; `[authority]` has its own monotonicity rules; a patched table
/// and a `[context]` whose program the binding already read are skipped.
fn report_shadowed_tables(
    cx: &mut Cx<'_>,
    target_files: &[&SourceFile],
    resolved_config: &toml::Table,
    program_bound: bool,
) {
    let [file] = target_files else {
        return;
    };
    let Ok(raw) = toml::from_str::<RawAgentDefinition>(&file.text) else {
        return;
    };
    if raw.extends.is_none() && raw.mixins.is_empty() {
        return;
    }
    for (key, own_value) in &raw.tables {
        if own_value.is_array() || key == "authority" || raw.patch.contains_key(key) {
            continue;
        }
        if key == "context" && program_bound {
            continue;
        }
        let shadowed = resolved_config
            .get(key)
            .is_some_and(|resolved_value| resolved_value != own_value);
        if shadowed {
            cx.report(
                &codes::RESOLVE_SHADOWED_TABLE,
                key,
                format!(
                    "`{key}` of this definition is shadowed by the value its base or a mixin sets"
                ),
            );
        }
    }
}

/// Keys allowed in `[context]`.
const CONTEXT_KEYS: &[&str] = &[
    CONTEXT_PROGRAM_KEY,
    "policy",
    "context_policy",
    "must_include",
    "exclude",
];

/// Checks `[context]` keys and the `program` type in the resolved config and
/// the own tables (before binding rewrites the table).
///
/// # Returns
/// `false` if `program` has the wrong type (binding is then skipped).
fn check_context_tables(cx: &mut Cx<'_>, resolved_config: &toml::Table, own: &toml::Table) -> bool {
    let mut program_ok = true;
    for source in [resolved_config, own] {
        for name in ["context", "context_program"] {
            let Some(toml::Value::Table(table)) = source.get(name) else {
                continue;
            };
            cx.check_keys(table, name, CONTEXT_KEYS);
            if let Some(value) = table
                .get(CONTEXT_PROGRAM_KEY)
                .filter(|value| !value.is_str())
            {
                cx.wrong_type(&join(name, CONTEXT_PROGRAM_KEY), "a string", value);
                program_ok = false;
            }
        }
    }
    program_ok
}

/// Binds the context program, reporting failures.
fn bind_program(
    cx: &mut Cx<'_>,
    own: &toml::Table,
    config: &mut toml::Table,
    library: Option<&ContextProgramLibrary>,
) -> Option<BoundContextProgram> {
    let name = crate::bind::context_program_name(own, config)
        .ok()
        .flatten()?;
    let Some(library) = library else {
        cx.report_help(
            &codes::CTX_UNKNOWN_PROGRAM,
            "context.program",
            format!(
                "context program `{name}` is bound, but no context-program library was supplied"
            ),
            "pass the context-program library in `LowerSources::with_context_programs`",
        );
        return None;
    };
    match crate::bind::bind_context_program(own, config, library, OffsetDateTime::UNIX_EPOCH) {
        Ok(bound) => bound,
        Err(error) => {
            cx.push_error(&error);
            None
        }
    }
}

/// Classifies every top-level key of the (bound) config.
fn classify_top_level(cx: &mut Cx<'_>, config: &toml::Table) {
    let mut known: Vec<&str> = LOWERED_TABLES.to_vec();
    known.extend_from_slice(FORWARD_COMPATIBLE_TABLES);
    known.push(SKILLS_CONFIG_KEY);
    known.push(INSTRUCTIONS_FILE_KEY);
    for key in config.keys() {
        let key = key.as_str();
        if LOWERED_TABLES.contains(&key) || key == SKILLS_CONFIG_KEY || key == INSTRUCTIONS_FILE_KEY
        {
            continue;
        }
        if FORWARD_COMPATIBLE_TABLES.contains(&key) {
            cx.report(
                &codes::SCHEMA_UNLOWERED_TABLE,
                key,
                format!(
                    "`[{key}]` is documented but not lowered by this version; it has no effect"
                ),
            );
            continue;
        }
        let suggestion = suggest(key, &known)
            .map(|candidate| format!("did you mean `[{candidate}]`? "))
            .unwrap_or_default();
        cx.report_help(
            &codes::SCHEMA_UNKNOWN_TABLE,
            key,
            format!("unknown table or key `{key}`"),
            format!(
                "{suggestion}known tables: {}",
                LOWERED_TABLES
                    .iter()
                    .chain(FORWARD_COMPATIBLE_TABLES)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
}

fn lower_tools(cx: &mut Cx<'_>, config: &toml::Table) -> ToolSurface {
    let Some((table, name)) = cx.section(config, &["tools", "tool_surface"]) else {
        return ToolSurface::default();
    };
    cx.check_keys(table, name, &["admitted", "forbidden"]);
    let admitted = cx.opt_strings(table, name, "admitted").unwrap_or_default();
    let forbidden = cx.opt_strings(table, name, "forbidden").unwrap_or_default();
    for (list, key) in [(&admitted, "admitted"), (&forbidden, "forbidden")] {
        let mut seen = BTreeSet::new();
        for (index, tool) in list.iter().enumerate() {
            let path = format!("{name}.{key}[{index}]");
            if !valid_label(tool) {
                cx.report(
                    &codes::TOOL_INVALID_NAME,
                    &path,
                    format!("invalid tool name `{tool}`"),
                );
            } else if !seen.insert(tool.as_str()) {
                cx.report(
                    &codes::TOOL_DUPLICATE,
                    &path,
                    format!("tool `{tool}` is listed twice in `{name}.{key}`"),
                );
            }
        }
    }
    for (index, tool) in admitted.iter().enumerate() {
        if forbidden.contains(tool) {
            cx.report(
                &codes::TOOL_CONFLICT,
                &format!("{name}.admitted[{index}]"),
                format!("tool `{tool}` is both admitted and forbidden"),
            );
        }
    }
    ToolSurface {
        admitted,
        forbidden,
    }
}

fn lower_delegation(cx: &mut Cx<'_>, config: &toml::Table) -> Option<Vec<String>> {
    let (table, name) = cx.section(config, &["delegation"])?;
    cx.check_keys(table, name, &["targets"]);
    cx.opt_strings(table, name, "targets")
}

fn lower_spawn(
    cx: &mut Cx<'_>,
    config: &toml::Table,
    role: AgentRoleId,
    delegation_targets: Option<Vec<String>>,
) -> SpawnContract {
    let mut spawn = SpawnContract {
        delegation_targets,
        ..SpawnContract::default()
    };
    let Some((table, name)) = cx.section(config, &["spawn", "spawn_contract"]) else {
        return spawn;
    };
    cx.check_keys(
        table,
        name,
        &[
            "workspace_hint",
            "max_depth",
            "child_orchestrators",
            "budget",
        ],
    );
    spawn.workspace_hint = cx.opt_str(table, name, "workspace_hint");
    spawn.max_depth = cx.opt_u32(table, name, "max_depth");
    spawn.child_orchestrators = cx
        .opt_strings(table, name, "child_orchestrators")
        .unwrap_or_default();
    if !spawn.child_orchestrators.is_empty() && !can_spawn(role, AgentRoleId::ChildOrchestrator) {
        cx.report(
            &codes::ROLE_SPAWN_MATRIX,
            &format!("{name}.child_orchestrators"),
            format!("role `{role:?}` may not spawn child orchestrators, but lists some"),
        );
    }
    match table.get("budget") {
        None => {}
        Some(toml::Value::Table(budget)) => {
            let prefix = format!("{name}.budget");
            cx.check_keys(
                budget,
                &prefix,
                &[
                    "max_tokens",
                    "max_tool_calls",
                    "max_wall_secs",
                    "effort_cap",
                ],
            );
            let max_tokens = cx.opt_u64(budget, &prefix, "max_tokens");
            let max_tool_calls = cx.opt_u32(budget, &prefix, "max_tool_calls");
            let max_wall_secs = cx.opt_u64(budget, &prefix, "max_wall_secs");
            let effort_cap = cx
                .opt_str(budget, &prefix, "effort_cap")
                .and_then(|label| cx.effort(&label, &format!("{prefix}.effort_cap")));
            spawn.budget = Some(Budget {
                max_tokens,
                max_tool_calls,
                max_wall_secs,
                effort_cap,
            });
        }
        Some(other) => cx.wrong_type(&format!("{name}.budget"), "a table", other),
    }
    spawn
}

fn lower_job(cx: &mut Cx<'_>, config: &toml::Table) -> Job {
    let Some((table, name)) = cx.section(config, &["job", "job_template"]) else {
        return Job::default();
    };
    cx.check_keys(table, name, &["goal_kind"]);
    Job {
        goal_kind: cx.opt_str(table, name, "goal_kind"),
    }
}

fn lower_lifecycle(cx: &mut Cx<'_>, config: &toml::Table) -> Lifecycle {
    let Some((table, name)) = cx.section(config, &["lifecycle", "lifecycle_machine"]) else {
        return Lifecycle::default();
    };
    cx.check_keys(table, name, &["allow_pause", "allow_rerun", "max_attempts"]);
    Lifecycle {
        allow_pause: cx.opt_bool(table, name, "allow_pause").unwrap_or(false),
        allow_rerun: cx.opt_bool(table, name, "allow_rerun").unwrap_or(false),
        max_attempts: cx.opt_u32(table, name, "max_attempts"),
    }
}

fn lower_context(
    cx: &mut Cx<'_>,
    config: &toml::Table,
    bound: Option<&BoundContextProgram>,
) -> ContextProgram {
    let mut context = ContextProgram::default();
    if let Some(bound) = bound {
        context.program = Some(bound.name.clone());
        context.program_id = Some(bound.program.id.to_string());
        context.sections = bound
            .program
            .sections
            .iter()
            .map(|section| ContextSection {
                name: section.name.clone(),
                strength: section.strength,
                detail: section.detail,
                trust: section.trust,
            })
            .collect();
        context.deferred = bound.deferred.clone();
        if !bound.deferred.is_empty() {
            cx.report(
                &codes::CTX_DEFERRED_SECTIONS,
                "context.program",
                format!(
                    "program `{}` requires sections outside the context ceiling; deferred: {}",
                    bound.name,
                    bound.deferred.join(", ")
                ),
            );
        }
    }
    let Some((table, name)) = cx.section(config, &["context", "context_program"]) else {
        return context;
    };
    // Keys were checked before binding (`check_context_tables`).
    context.policy = cx
        .opt_str(table, name, "policy")
        .or_else(|| cx.opt_str(table, name, "context_policy"));
    context.must_include = cx
        .opt_strings(table, name, "must_include")
        .unwrap_or_default();
    context.exclude = cx.opt_strings(table, name, "exclude").unwrap_or_default();
    context
}

fn lower_return(cx: &mut Cx<'_>, config: &toml::Table) -> ReturnPipeline {
    let Some((table, name)) = cx.section(config, &["return", "return_pipeline"]) else {
        return ReturnPipeline::default();
    };
    cx.check_keys(table, name, &["contract", "validators"]);
    let contract = cx.opt_str(table, name, "contract").and_then(|label| {
        let parsed = ReturnContract::parse(&label);
        if parsed.is_none() {
            cx.report_help(
                &codes::RETURN_UNKNOWN_CONTRACT,
                &format!("{name}.contract"),
                format!("unknown return contract `{label}`"),
                format!(
                    "known contracts: {}",
                    ReturnContract::ALL
                        .iter()
                        .map(|contract| contract.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        parsed
    });
    let validators = cx
        .opt_strings(table, name, "validators")
        .unwrap_or_default();
    for (index, validator) in validators.iter().enumerate() {
        if !valid_label(validator) {
            cx.report(
                &codes::RETURN_INVALID_VALIDATOR,
                &format!("{name}.validators[{index}]"),
                format!("invalid validator label `{validator}`"),
            );
        }
    }
    ReturnPipeline {
        contract,
        validators,
    }
}

fn lower_models(cx: &mut Cx<'_>, config: &toml::Table) -> Option<Models> {
    let (table, name) = cx.section(config, &["models"])?;
    cx.check_keys(
        table,
        name,
        &["provider", "model", "effort", "fallbacks", "required_env"],
    );
    let provider = cx.opt_str(table, name, "provider");
    let model = cx.opt_str(table, name, "model");
    let effort = cx
        .opt_str(table, name, "effort")
        .and_then(|label| cx.effort(&label, "models.effort"));
    let mut fallbacks = Vec::new();
    for (index, entry) in cx
        .opt_strings(table, name, "fallbacks")
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        match entry.split_once('/') {
            Some((fallback_provider, fallback_model))
                if valid_label(fallback_provider)
                    && valid_label(fallback_model)
                    && !fallback_model.contains('/') =>
            {
                fallbacks.push(ModelRef {
                    provider: fallback_provider.to_owned(),
                    model: fallback_model.to_owned(),
                });
            }
            _ => cx.report(
                &codes::MODEL_INVALID_FALLBACK,
                &format!("models.fallbacks[{index}]"),
                format!("fallback `{entry}` is not of the form `provider/model`"),
            ),
        }
    }
    let required_env = cx
        .opt_strings(table, name, "required_env")
        .unwrap_or_default();
    for (index, variable) in required_env.iter().enumerate() {
        if !valid_env_name(variable) {
            cx.report(
                &codes::MODEL_INVALID_ENV,
                &format!("models.required_env[{index}]"),
                format!("`{variable}` is not a valid environment variable name"),
            );
        }
    }
    Some(Models {
        provider,
        model,
        effort,
        fallbacks,
        required_env,
    })
}

fn lower_limits(cx: &mut Cx<'_>, config: &toml::Table) -> Option<Limits> {
    let (table, name) = cx.section(config, &["limits"])?;
    cx.check_keys(
        table,
        name,
        &[
            "max_tool_calls",
            "max_agent_tool_calls",
            "max_wall_time_seconds",
            "max_context_tokens",
        ],
    );
    Some(Limits {
        max_tool_calls: cx.opt_u32(table, name, "max_tool_calls"),
        max_agent_tool_calls: cx.opt_u32(table, name, "max_agent_tool_calls"),
        max_wall_time_seconds: cx.opt_u64(table, name, "max_wall_time_seconds"),
        max_context_tokens: cx.opt_u64(table, name, "max_context_tokens"),
    })
}

fn lower_work(cx: &mut Cx<'_>, config: &toml::Table) -> Option<Work> {
    let (table, name) = cx.section(config, &["work"])?;
    cx.check_keys(
        table,
        name,
        &[
            "mode",
            "may_write_code",
            "may_research_web",
            "may_change_plan",
            "aggregates_child_returns",
        ],
    );
    Some(Work {
        mode: cx.opt_str(table, name, "mode"),
        may_write_code: cx.opt_bool(table, name, "may_write_code"),
        may_research_web: cx.opt_bool(table, name, "may_research_web"),
        may_change_plan: cx.opt_bool(table, name, "may_change_plan"),
        aggregates_child_returns: cx.opt_bool(table, name, "aggregates_child_returns"),
    })
}

fn lower_research(cx: &mut Cx<'_>, config: &toml::Table) -> Option<Research> {
    let (table, name) = cx.section(config, &["research"])?;
    cx.check_keys(
        table,
        name,
        &[
            "sources",
            "freshness",
            "primary_sources_required",
            "output",
            "ecosystems",
            "may_write_code",
            "tradecraft",
        ],
    );
    Some(Research {
        sources: cx.opt_strings(table, name, "sources").unwrap_or_default(),
        freshness: cx.opt_str(table, name, "freshness"),
        primary_sources_required: cx.opt_bool(table, name, "primary_sources_required"),
        output: cx.opt_str(table, name, "output"),
        ecosystems: cx
            .opt_strings(table, name, "ecosystems")
            .unwrap_or_default(),
        may_write_code: cx.opt_bool(table, name, "may_write_code"),
        tradecraft: cx
            .opt_strings(table, name, "tradecraft")
            .unwrap_or_default(),
    })
}

fn lower_verification(cx: &mut Cx<'_>, config: &toml::Table) -> Option<Verification> {
    let (table, name) = cx.section(config, &["verification"])?;
    cx.check_keys(table, name, &["profile", "commands"]);
    Some(Verification {
        profile: cx.opt_str(table, name, "profile"),
        commands: cx.opt_strings(table, name, "commands").unwrap_or_default(),
    })
}

fn lower_network_hosts(cx: &mut Cx<'_>, config: &toml::Table) -> Vec<String> {
    let Some((table, name)) = cx.section(config, &["network"]) else {
        return Vec::new();
    };
    cx.check_keys(table, name, &["hosts"]);
    let hosts = cx.opt_strings(table, name, "hosts").unwrap_or_default();
    for (index, host) in hosts.iter().enumerate() {
        if !valid_label(host) {
            cx.report(
                &codes::PARSE_WRONG_TYPE,
                &format!("network.hosts[{index}]"),
                format!("`network.hosts[{index}]` must be a host name, found `{host}`"),
            );
        }
    }
    hosts
}

fn lower_authority_table(cx: &mut Cx<'_>, config: &toml::Table) {
    if let Some((table, name)) = cx.section(config, &["authority"]) {
        cx.check_keys(table, name, &["capabilities"]);
        let _ = cx.opt_strings(table, name, "capabilities");
    }
}

fn lower_binary(cx: &mut Cx<'_>, config: &toml::Table, specialization: &str) -> Binary {
    let default_name = default_binary_name(specialization);
    let Some((table, name)) = cx.section(config, &["binary"]) else {
        return Binary {
            name: default_name,
            interfaces: vec![Interface::Cli],
            default_interface: Interface::Cli,
        };
    };
    cx.check_keys(table, name, &["name", "interfaces", "default_interface"]);
    let binary_name = match cx.opt_str(table, name, "name") {
        Some(binary_name) => {
            if !valid_binary_name(&binary_name) {
                cx.report(
                    &codes::BINARY_INVALID_NAME,
                    "binary.name",
                    format!("invalid binary name `{binary_name}`"),
                );
            }
            binary_name
        }
        None => default_name,
    };
    let mut interfaces: Vec<Interface> = Vec::new();
    match cx.opt_strings(table, name, "interfaces") {
        None => interfaces.push(Interface::Cli),
        Some(labels) => {
            if labels.is_empty() {
                cx.report(
                    &codes::BINARY_EMPTY_INTERFACES,
                    "binary.interfaces",
                    "`binary.interfaces` is empty",
                );
            }
            for (index, label) in labels.iter().enumerate() {
                let path = format!("binary.interfaces[{index}]");
                match Interface::parse(label) {
                    Some(interface) if interfaces.contains(&interface) => cx.report(
                        &codes::BINARY_DUPLICATE_INTERFACE,
                        &path,
                        format!("interface `{label}` is listed twice"),
                    ),
                    Some(interface) => interfaces.push(interface),
                    None => cx.report(
                        &codes::BINARY_UNKNOWN_INTERFACE,
                        &path,
                        format!("unknown interface `{label}`"),
                    ),
                }
            }
        }
    }
    let default_interface = match cx.opt_str(table, name, "default_interface") {
        None => interfaces.first().copied().unwrap_or(Interface::Cli),
        Some(label) => match Interface::parse(&label) {
            Some(interface) => {
                if !interfaces.contains(&interface) {
                    cx.report(
                        &codes::BINARY_DEFAULT_NOT_LISTED,
                        "binary.default_interface",
                        format!("default interface `{label}` is not one of `binary.interfaces`"),
                    );
                }
                interface
            }
            None => {
                cx.report(
                    &codes::BINARY_UNKNOWN_INTERFACE,
                    "binary.default_interface",
                    format!("unknown interface `{label}`"),
                );
                Interface::Cli
            }
        },
    };
    Binary {
        name: binary_name,
        interfaces,
        default_interface,
    }
}

/// `true` if `source` sets `instructions_file = file` at top level.
fn sets_instructions_file(source: &SourceFile, file: &str) -> bool {
    toml::from_str::<toml::Table>(&source.text)
        .ok()
        .and_then(|table| {
            table
                .get(INSTRUCTIONS_FILE_KEY)
                .and_then(toml::Value::as_str)
                .map(|value| value == file)
        })
        .unwrap_or(false)
}

/// Finds the source file that sets `instructions_file = file`: the target's
/// files first (highest layer first), then every other file.
fn instructions_owner<'f>(
    files: &'f [SourceFile],
    target_files: &[&'f SourceFile],
    file: &str,
) -> Option<&'f SourceFile> {
    if let Some(owner) = target_files
        .iter()
        .copied()
        .find(|source| sets_instructions_file(source, file))
    {
        return Some(owner);
    }
    let mut others: Vec<(usize, &'f SourceFile)> = files.iter().enumerate().collect();
    others.sort_by(|left, right| {
        right
            .1
            .layer
            .cmp(&left.1.layer)
            .then_with(|| right.0.cmp(&left.0))
    });
    others
        .into_iter()
        .map(|(_, source)| source)
        .find(|source| sets_instructions_file(source, file))
}

fn lower_instructions(
    cx: &mut Cx<'_>,
    config: &toml::Table,
    sources: &LowerSources<'_>,
    target_files: &[&SourceFile],
) -> Instructions {
    let declared = match config.get(INSTRUCTIONS_FILE_KEY) {
        None => None,
        Some(toml::Value::String(file)) => Some(file.clone()),
        Some(other) => {
            cx.wrong_type(INSTRUCTIONS_FILE_KEY, "a string", other);
            return Instructions::none();
        }
    };
    let Some(file) = declared else {
        // Implicit `system.md` next to the highest-priority target file.
        let (Some(loader), Some(owner)) = (sources.instructions, target_files.first()) else {
            return Instructions::none();
        };
        let dir = owner.path.parent().unwrap_or_else(|| Path::new(""));
        return match loader.load(dir, DEFAULT_INSTRUCTIONS_FILE) {
            Ok(Some(text)) => Instructions::new(text, Some(DEFAULT_INSTRUCTIONS_FILE.to_owned())),
            Ok(None) => Instructions::none(),
            Err(reason) => {
                cx.report(
                    &codes::SCHEMA_INSTRUCTIONS,
                    INSTRUCTIONS_FILE_KEY,
                    format!("`{DEFAULT_INSTRUCTIONS_FILE}` cannot be loaded: {reason}"),
                );
                Instructions::none()
            }
        };
    };
    let Some(owner) = instructions_owner(sources.files, target_files, &file) else {
        cx.report(
            &codes::SCHEMA_INSTRUCTIONS,
            INSTRUCTIONS_FILE_KEY,
            format!("no supplied source file sets `instructions_file = \"{file}\"`, so its directory is unknown"),
        );
        return Instructions::none();
    };
    let Some(loader) = sources.instructions else {
        cx.report_help(
            &codes::SCHEMA_INSTRUCTIONS,
            INSTRUCTIONS_FILE_KEY,
            format!(
                "`instructions_file = \"{file}\"` is set, but no instruction loader was supplied"
            ),
            "pass a loader in `LowerSources::with_instructions`",
        );
        return Instructions::none();
    };
    let dir = owner.path.parent().unwrap_or_else(|| Path::new(""));
    match loader.load(dir, &file) {
        Ok(Some(text)) => Instructions::new(text, Some(file)),
        Ok(None) => {
            cx.report(
                &codes::SCHEMA_INSTRUCTIONS,
                INSTRUCTIONS_FILE_KEY,
                format!(
                    "instruction file `{file}` does not exist next to {}",
                    owner.label()
                ),
            );
            Instructions::none()
        }
        Err(reason) => {
            cx.report(
                &codes::SCHEMA_INSTRUCTIONS,
                INSTRUCTIONS_FILE_KEY,
                format!("instruction file `{file}` cannot be loaded: {reason}"),
            );
            Instructions::none()
        }
    }
}

/// Tool-label prefixes that reach the network.
const NETWORK_TOOL_PREFIXES: &[&str] = &["web.", "browser."];
/// Tools that write into the workspace file tree.
const FS_WRITE_TOOLS: &[&str] = &["fs.write", "fs.edit"];
/// Known tools that write outside the workspace file tree.
const OTHER_WRITE_TOOLS: &[&str] = &[
    "agents.commit_proposal",
    "agents.write_definition",
    "agents.write_uia",
    "latex.build",
    "skills.commit_proposal",
    "skills.propose",
];
/// Tool-label prefixes that read the workspace.
const READ_TOOL_PREFIXES: &[&str] = &["fs.", "doc.", "deps.", "explore.", "lens."];
/// Tools that start processes.
const SHELL_TOOLS: &[&str] = &["shell.exec", "job.start"];
/// Tool-label prefixes that start processes.
const SHELL_TOOL_PREFIXES: &[&str] = &["process."];
/// Tool-label prefixes that act on the host outside the sandbox.
const HOST_TOOL_PREFIXES: &[&str] = &["host."];

/// `true` if `tool` starts with one of `prefixes`.
fn has_prefix(tool: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| tool.starts_with(prefix))
}

/// Sorted, de-duplicated copy.
fn sorted_unique<'i>(values: impl IntoIterator<Item = &'i String>) -> Vec<String> {
    values
        .into_iter()
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// Derives the rights manifest (§ [`Permissions`]).
fn derive_permissions(
    tools: &ToolSurface,
    authority: &Authority,
    spawn: &SpawnContract,
    models: Option<&Models>,
    hosts: &[String],
) -> Permissions {
    let effective = sorted_unique(
        tools
            .admitted
            .iter()
            .filter(|tool| !tools.forbidden.contains(tool)),
    );
    let network_tools: Vec<String> = effective
        .iter()
        .filter(|tool| has_prefix(tool.as_str(), NETWORK_TOOL_PREFIXES))
        .cloned()
        .collect();
    let write = effective
        .iter()
        .any(|tool| FS_WRITE_TOOLS.contains(&tool.as_str()));
    let read = effective.iter().any(|tool| {
        has_prefix(tool.as_str(), READ_TOOL_PREFIXES) && !FS_WRITE_TOOLS.contains(&tool.as_str())
    });
    let write_paths = if write {
        vec![
            spawn
                .workspace_hint
                .clone()
                .unwrap_or_else(|| WORKSPACE_WRITE_PATH.to_owned()),
        ]
    } else {
        Vec::new()
    };
    let other_write_tools: Vec<String> = effective
        .iter()
        .filter(|tool| OTHER_WRITE_TOOLS.contains(&tool.as_str()))
        .cloned()
        .collect();
    let shell = effective.iter().any(|tool| {
        SHELL_TOOLS.contains(&tool.as_str()) || has_prefix(tool.as_str(), SHELL_TOOL_PREFIXES)
    });
    let host = effective
        .iter()
        .any(|tool| has_prefix(tool.as_str(), HOST_TOOL_PREFIXES));
    let network = NetworkPermissions {
        mode: if network_tools.is_empty() {
            NetworkMode::Off
        } else {
            NetworkMode::Allowlist
        },
        tools: network_tools,
        hosts: sorted_unique(hosts),
    };
    Permissions {
        forbidden_tools: sorted_unique(&tools.forbidden),
        tools: effective,
        capabilities: sorted_unique(&authority.capabilities),
        filesystem: FilesystemPermissions {
            read,
            write,
            write_paths,
            other_write_tools,
        },
        network,
        shell,
        host,
        spawn: SpawnPermissions {
            max_depth: spawn.max_depth.unwrap_or(0),
            child_orchestrators: sorted_unique(&spawn.child_orchestrators),
            delegation_targets: spawn
                .delegation_targets
                .as_ref()
                .map(|targets| sorted_unique(targets.iter()))
                .unwrap_or_default(),
        },
        budget: spawn.budget.clone(),
        required_env: models
            .map(|models| sorted_unique(&models.required_env))
            .unwrap_or_default(),
    }
}

/// Lowers a resolved definition to [`AgentIr`] (§ module docs).
///
/// # Description
/// Binds the context program (from the target's own tables, see
/// [`crate::bind`]), lowers every table, loads the instructions, derives the
/// permission manifest and attaches the v7 snapshot. Warnings and notes are
/// kept in [`AgentIr::trace`]; any error fails the lowering.
///
/// # Errors
/// [`Diagnostics`] with at least one error.
pub fn lower_v2(
    resolved: &ResolvedAgentDefinition,
    sources: &LowerSources<'_>,
) -> Result<AgentIr, Diagnostics> {
    let target = resolved.id.to_string();
    let index = SourceIndex::new(sources.files, Some(target.as_str()));
    let target_files = index.target_files();
    let mut cx = Cx {
        index,
        diagnostics: Diagnostics::new(),
    };

    if resolved.specialization.trim().is_empty() {
        cx.report(
            &codes::SCHEMA_EMPTY_SPECIALIZATION,
            "specialization",
            format!("definition `{target}` has an empty specialization"),
        );
    }
    let reasoning_effort = resolved
        .reasoning_effort
        .as_deref()
        .and_then(|label| cx.effort(label, "reasoning_effort"));

    // Context-program binding (before the tables are read, as the legacy
    // path does in `harw-registry-defaults`).
    let own = own_tables(&target_files);
    let mut config = resolved.config.clone();
    let program_ok = check_context_tables(&mut cx, &resolved.config, &own);
    let bound = if program_ok {
        bind_program(&mut cx, &own, &mut config, sources.context_programs)
    } else {
        None
    };
    report_shadowed_tables(&mut cx, &target_files, &resolved.config, bound.is_some());
    classify_top_level(&mut cx, &config);

    let tools = lower_tools(&mut cx, &config);
    let delegation_targets = lower_delegation(&mut cx, &config);
    let spawn = lower_spawn(&mut cx, &config, resolved.role, delegation_targets);
    let job = lower_job(&mut cx, &config);
    let lifecycle = lower_lifecycle(&mut cx, &config);
    let context = lower_context(&mut cx, &config, bound.as_ref());
    let return_pipeline = lower_return(&mut cx, &config);
    let models = lower_models(&mut cx, &config);
    let limits = lower_limits(&mut cx, &config);
    let work = lower_work(&mut cx, &config);
    let research = lower_research(&mut cx, &config);
    let verification = lower_verification(&mut cx, &config);
    let hosts = lower_network_hosts(&mut cx, &config);
    lower_authority_table(&mut cx, &config);
    let binary = lower_binary(&mut cx, &config, &resolved.specialization);
    let skills = match skills_from_config_strict(&resolved.id, &config) {
        Ok(names) => names,
        Err(error) => {
            cx.push_error(&error);
            Vec::new()
        }
    };
    let instructions = lower_instructions(&mut cx, &config, sources, &target_files);
    let authority = Authority {
        capabilities: resolved.authority.capabilities.clone(),
    };
    let permissions = derive_permissions(&tools, &authority, &spawn, models.as_ref(), &hosts);

    if cx.diagnostics.has_errors() {
        return Err(cx.diagnostics);
    }
    let kept: Vec<Diagnostic> = cx
        .diagnostics
        .into_vec()
        .into_iter()
        .filter(|diagnostic| diagnostic.severity != Severity::Error)
        .collect();
    let ir = AgentIr {
        schema: AGENT_IR_SCHEMA.to_owned(),
        id: resolved.id.clone(),
        version: resolved.version.clone(),
        name: resolved.name.clone(),
        description: resolved.description.clone(),
        role: resolved.role,
        specialization: resolved.specialization.clone(),
        reasoning_effort,
        authority,
        instructions,
        tools,
        spawn,
        job,
        lifecycle,
        context,
        return_pipeline,
        models,
        limits,
        work,
        research,
        verification,
        skills: Skills {
            entries: skills
                .into_iter()
                .map(|name| SkillEntry { name, hash: None })
                .collect(),
        },
        binary,
        permissions,
        trace: Trace {
            steps: resolved
                .trace
                .steps
                .iter()
                .map(|step| TraceStep {
                    source: step.source.clone(),
                    kind: step.kind.clone(),
                    applied_at: step.applied_at,
                })
                .collect(),
            diagnostics: kept,
        },
        snapshot: None,
    };
    Ok(ir.with_snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edit_distance_and_suggestion() {
        assert_eq!(edit_distance("tols", "tools"), 1);
        assert_eq!(suggest("tols", LOWERED_TABLES), Some("tools"));
        assert_eq!(suggest("zzzzzz", LOWERED_TABLES), None);
    }

    #[test]
    fn test_name_validators() {
        assert!(valid_env_name("ANTHROPIC_API_KEY"));
        assert!(!valid_env_name("anthropic"));
        assert!(!valid_env_name("1KEY"));
        assert!(valid_binary_name("evidence-critic"));
        assert!(!valid_binary_name("Evidence Critic"));
        assert_eq!(default_binary_name("Evidence Critic"), "evidence-critic");
        assert_eq!(default_binary_name("-x"), "agent--x");
        assert!(valid_label("fs.read"));
        assert!(!valid_label("fs read"));
        assert!(!valid_label(""));
    }

    #[test]
    fn test_validate_relative_file_rejects_escapes() {
        assert!(validate_relative_file("system.md").is_ok());
        assert!(validate_relative_file("docs/system.md").is_ok());
        assert!(validate_relative_file("../system.md").is_err());
        assert!(validate_relative_file("/etc/passwd").is_err());
        assert!(validate_relative_file(" ").is_err());
    }

    #[test]
    fn test_map_loader_serves_by_joined_path() {
        let mut loader = MapInstructionsLoader::new();
        loader.insert("agents/a/system.md", "hi");
        assert_eq!(
            loader.load(Path::new("agents/a"), "system.md"),
            Ok(Some("hi".to_owned()))
        );
        assert_eq!(loader.load(Path::new("agents/b"), "system.md"), Ok(None));
        assert!(
            loader
                .load(Path::new("agents/a"), "../a/system.md")
                .is_err()
        );
    }

    #[test]
    fn test_permissions_are_derived_conservatively() {
        let tools = ToolSurface {
            admitted: vec![
                "web.fetch".to_owned(),
                "fs.read".to_owned(),
                "fs.write".to_owned(),
                "shell.exec".to_owned(),
                "host.sudo_exec".to_owned(),
                "skills.propose".to_owned(),
            ],
            forbidden: vec!["shell.exec".to_owned()],
        };
        let spawn = SpawnContract::default();
        let permissions = derive_permissions(
            &tools,
            &Authority::default(),
            &spawn,
            None,
            &["docs.rs".to_owned()],
        );
        assert_eq!(permissions.network.mode, NetworkMode::Allowlist);
        assert_eq!(permissions.network.tools, ["web.fetch"]);
        assert_eq!(permissions.network.hosts, ["docs.rs"]);
        assert!(permissions.filesystem.read && permissions.filesystem.write);
        assert_eq!(permissions.filesystem.write_paths, [WORKSPACE_WRITE_PATH]);
        assert_eq!(permissions.filesystem.other_write_tools, ["skills.propose"]);
        assert!(!permissions.shell, "forbidden tools never count");
        assert!(permissions.host);
        assert_eq!(permissions.spawn.max_depth, 0);
        assert!(!permissions.tools.contains(&"shell.exec".to_owned()));
    }
}
