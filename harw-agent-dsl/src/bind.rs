//! Context-program binding: `[context] program = "<name>"` (Runde 3, Welle C2;
//! moved into the DSL crate for IR v2, #22 wave 1).
//!
//! A definition binds a program of a [`ContextProgramLibrary`] by name. The
//! library holds the built-in programs **passed in by the caller** (this crate
//! embeds no program files; `harw-registry-defaults` supplies its
//! `agents/context-programs/*.toml`) plus user programs from higher layers.
//!
//! # What binding does
//! [`bind_context_program`] resolves the named program and rewrites the
//! resolved `[context]` table the way the legacy lowering reads it (policy,
//! `must_include`, `exclude`), and additionally returns the full resolved
//! program ([`BoundContextProgram`]) so the IR v2 lowering keeps every
//! section's strength, detail mode and trust class — the legacy path lost
//! them.
//!
//! # The root ceiling
//! `harw_core::child_controller` rejects a child whose `must_include` names a
//! section outside the context ceiling. Bound programs require some sections
//! no root ceiling admits (`plan.current`, `child.returns`, …). Binding
//! therefore copies only must-include sections inside
//! [`ContextProgramLibrary::ceiling_sections`] into `must_include`; the rest
//! is reported as deferred (named by the program, not required at start).
//!
//! # Concurrency
//! All types are `Send + Sync`; binding is pure apart from the timestamp
//! passed in for the program's resolution trace.

use time::OffsetDateTime;

use crate::context_program::{
    RawContextProgramDefinition, ResolvedContextProgramDefinition, SectionStrength,
    resolve_context_program,
};
use crate::error::DslError;
use crate::ids::DefinitionId;
use crate::layers::DefinitionLayer;

/// Key of the program binding inside `[context]`.
pub const CONTEXT_PROGRAM_KEY: &str = "program";

/// A library of context programs, addressable by name.
///
/// # Description
/// A name is the file stem under which the caller registers a program
/// (`explore.toml` → `"explore"`). The same name may be registered in several
/// layers as long as every layer declares the same program ID — higher
/// layers then patch lower ones through `resolve_context_program`. All
/// programs are resolved against one shared layer stack, so `extends` between
/// programs resolves.
#[derive(Debug, Clone)]
pub struct ContextProgramLibrary {
    ids: Vec<(String, DefinitionId)>,
    layers: Vec<(DefinitionLayer, RawContextProgramDefinition)>,
    ceiling_sections: Vec<String>,
}

impl Default for ContextProgramLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextProgramLibrary {
    /// An empty library whose ceiling is
    /// [`harw_context::ceiling::ROOT_CONTEXT_SECTIONS`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            ids: Vec::new(),
            layers: Vec::new(),
            ceiling_sections: harw_context::ceiling::ROOT_CONTEXT_SECTIONS
                .iter()
                .map(|section| (*section).to_owned())
                .collect(),
        }
    }

    /// Replaces the ceiling sections used to filter `must_include`.
    #[must_use]
    pub fn with_ceiling_sections(mut self, sections: &[&str]) -> Self {
        self.ceiling_sections = sections.iter().map(|s| (*s).to_owned()).collect();
        self
    }

    /// Registers one parsed program under `name` in `layer`.
    ///
    /// # Errors
    /// [`DslError::Toml`] if `name` is already registered with a different
    /// program ID.
    pub fn insert(
        &mut self,
        name: &str,
        layer: DefinitionLayer,
        program: RawContextProgramDefinition,
    ) -> Result<(), DslError> {
        match self.ids.iter().find(|(known, _)| known == name) {
            Some((_, existing)) if existing != &program.id => {
                return Err(DslError::Toml(format!(
                    "context program name '{name}' is registered twice with different IDs ('{existing}' and '{}')",
                    program.id
                )));
            }
            Some(_) => {}
            None => self.ids.push((name.to_owned(), program.id.clone())),
        }
        self.layers.push((layer, program));
        Ok(())
    }

    /// Parses and registers `(name, toml)` sources in `layer`.
    ///
    /// # Errors
    /// [`DslError::Toml`] naming the program if a source does not parse as
    /// `harwness.context/v1` or a name clashes (see [`Self::insert`]).
    pub fn insert_sources(
        &mut self,
        layer: DefinitionLayer,
        sources: &[(&str, &str)],
    ) -> Result<(), DslError> {
        for (name, source) in sources {
            let raw = toml::from_str::<RawContextProgramDefinition>(source).map_err(|error| {
                DslError::Toml(format!("context program '{name}' does not parse: {error}"))
            })?;
            self.insert(name, layer, raw)?;
        }
        Ok(())
    }

    /// Builds a library from built-in `(name, toml)` sources.
    ///
    /// # Errors
    /// See [`Self::insert_sources`].
    pub fn from_builtin_sources(sources: &[(&str, &str)]) -> Result<Self, DslError> {
        let mut library = Self::new();
        library.insert_sources(DefinitionLayer::BuiltIn, sources)?;
        Ok(library)
    }

    /// Registered names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.ids.iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// The ceiling sections used to filter `must_include`.
    #[must_use]
    pub fn ceiling_sections(&self) -> &[String] {
        &self.ceiling_sections
    }

    /// Resolves the program registered under `name`.
    ///
    /// # Errors
    /// - [`DslError::UnknownContextProgram`]: no program has this name (the
    ///   error lists the known names).
    /// - [`DslError::ContextProgramResolution`]: the program does not resolve.
    pub fn resolve(
        &self,
        name: &str,
        now: OffsetDateTime,
    ) -> Result<ResolvedContextProgramDefinition, DslError> {
        let Some((_, id)) = self.ids.iter().find(|(known, _)| known == name) else {
            return Err(DslError::UnknownContextProgram {
                name: name.to_owned(),
                known: self.names().into_iter().map(str::to_owned).collect(),
            });
        };
        resolve_context_program(id, &self.layers, now).map_err(|error| {
            DslError::ContextProgramResolution {
                name: name.to_owned(),
                message: error.to_string(),
            }
        })
    }
}

/// The result of a successful binding.
#[derive(Debug, Clone)]
pub struct BoundContextProgram {
    /// The library name that was bound (`"explore"`).
    pub name: String,
    /// The fully resolved program, with every section's strength, detail
    /// mode and trust class.
    pub program: ResolvedContextProgramDefinition,
    /// Must-include sections of the program outside the ceiling, in
    /// declaration order; they are not copied into `must_include`.
    pub deferred: Vec<String>,
}

/// The `[context]` table of a TOML table, if present.
fn context_table(tables: &toml::Table) -> Option<&toml::Table> {
    tables.get("context").and_then(toml::Value::as_table)
}

/// Reads a string list from `table[field]`; missing means empty.
fn table_strings(table: Option<&toml::Table>, field: &str) -> Vec<String> {
    table
        .and_then(|table| table.get(field))
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Reads the explicit policy (`policy`, then `context_policy`).
fn table_policy(table: Option<&toml::Table>) -> Option<String> {
    let table = table?;
    ["policy", "context_policy"]
        .iter()
        .find_map(|field| table.get(*field).and_then(toml::Value::as_str))
        .map(str::to_owned)
}

/// Appends `value` unless already present.
fn push_unique(list: &mut Vec<String>, value: &str) {
    if !list.iter().any(|existing| existing == value) {
        list.push(value.to_owned());
    }
}

/// The bound program name: first from the definition's own tables, else
/// from the resolved config (inherited, or an own section without a base).
///
/// # Description
/// The own tables matter because the resolver keeps a base's `[context]`
/// and drops the child's (see `worker-base.toml`); the binding lives in
/// exactly that dropped table.
///
/// # Errors
/// [`DslError::Toml`] if `program` is not a string.
pub fn context_program_name(
    own_tables: &toml::Table,
    resolved_config: &toml::Table,
) -> Result<Option<String>, DslError> {
    let value = context_table(own_tables)
        .and_then(|table| table.get(CONTEXT_PROGRAM_KEY))
        .or_else(|| {
            context_table(resolved_config).and_then(|table| table.get(CONTEXT_PROGRAM_KEY))
        });
    match value {
        None => Ok(None),
        Some(toml::Value::String(name)) => Ok(Some(name.clone())),
        Some(other) => Err(DslError::Toml(format!(
            "[context].{CONTEXT_PROGRAM_KEY} muss ein String sein, gefunden: {}",
            other.type_str()
        ))),
    }
}

/// Binds the program named by `[context] program = "<name>"` into the
/// resolved config.
///
/// # Description
/// Without a binding `resolved_config` stays untouched and the result is
/// `Ok(None)`. With a binding a new `[context]` table replaces the old one:
///
/// - `program`: the bound name.
/// - `policy`: an inline `policy`/`context_policy` wins (own tables first,
///   then inherited), else the program's canonical ID.
/// - `must_include`: first the program's must-include sections admitted by
///   the ceiling, in declaration order, then the inline selectors
///   (inherited, then own). Duplicates are removed.
/// - `exclude`: first the program's exclusions, then the inline ones. A
///   program exclusion the definition lists inline as `must_include` is
///   dropped: inline wins.
///
/// # Errors
/// - [`DslError::Toml`]: `program` is not a string.
/// - [`DslError::UnknownContextProgram`] / [`DslError::ContextProgramResolution`]:
///   see [`ContextProgramLibrary::resolve`]. An unknown name is a hard error,
///   never a silent fallback to the base.
pub fn bind_context_program(
    own_tables: &toml::Table,
    resolved_config: &mut toml::Table,
    library: &ContextProgramLibrary,
    now: OffsetDateTime,
) -> Result<Option<BoundContextProgram>, DslError> {
    let Some(name) = context_program_name(own_tables, resolved_config)? else {
        return Ok(None);
    };
    let program = library.resolve(&name, now)?;

    let inherited = context_table(resolved_config);
    let own = context_table(own_tables);

    let mut inline_must_include = table_strings(inherited, "must_include");
    for selector in table_strings(own, "must_include") {
        push_unique(&mut inline_must_include, &selector);
    }
    let mut inline_exclude = table_strings(inherited, "exclude");
    for selector in table_strings(own, "exclude") {
        push_unique(&mut inline_exclude, &selector);
    }
    let policy = table_policy(own)
        .or_else(|| table_policy(inherited))
        .unwrap_or_else(|| program.id.to_string());

    let ceiling = library.ceiling_sections();
    let mut must_include: Vec<String> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();
    for section in &program.sections {
        if section.strength != SectionStrength::MustInclude {
            continue;
        }
        if ceiling.iter().any(|admitted| admitted == &section.name) {
            push_unique(&mut must_include, &section.name);
        } else {
            push_unique(&mut deferred, &section.name);
        }
    }
    for selector in &inline_must_include {
        push_unique(&mut must_include, selector);
    }

    let mut exclude: Vec<String> = Vec::new();
    for selector in &program.exclude {
        if !inline_must_include.contains(selector) {
            push_unique(&mut exclude, selector);
        }
    }
    for selector in &inline_exclude {
        push_unique(&mut exclude, selector);
    }

    let to_array = |values: Vec<String>| {
        toml::Value::Array(values.into_iter().map(toml::Value::String).collect())
    };
    let mut context = toml::Table::new();
    context.insert(
        CONTEXT_PROGRAM_KEY.to_owned(),
        toml::Value::String(name.clone()),
    );
    context.insert("policy".to_owned(), toml::Value::String(policy));
    context.insert("must_include".to_owned(), to_array(must_include));
    context.insert("exclude".to_owned(), to_array(exclude));
    resolved_config.insert("context".to_owned(), toml::Value::Table(context));
    Ok(Some(BoundContextProgram {
        name,
        program,
        deferred,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    const BASE: &str = r#"
schema = "harwness.context/v1"
id = "harwness.context.base@1"
version = "1.0.0"
exclude = ["secret.*"]

[[sections]]
name = "task.objective"
strength = "must-include"
detail = "full"
trust = "instruction"
"#;

    const EXPLORE: &str = r#"
schema = "harwness.context/v1"
id = "harwness.context.explore@1"
version = "1.0.0"
extends = { id = "harwness.context.base@1" }
exclude = ["plan.*"]

[[sections]]
name = "plan.current"
strength = "must-include"
detail = "references"
trust = "evidence"

[[sections]]
name = "repo.tree"
detail = "summary"
trust = "evidence"
"#;

    fn library() -> TestResult<ContextProgramLibrary> {
        Ok(ContextProgramLibrary::from_builtin_sources(&[
            ("base", BASE),
            ("explore", EXPLORE),
        ])?)
    }

    fn tables(src: &str) -> TestResult<toml::Table> {
        Ok(toml::from_str::<toml::Table>(src)?)
    }

    #[test]
    fn test_binding_keeps_sections_with_detail_and_trust_and_defers_outside_ceiling() -> TestResult
    {
        let own = tables("[context]\nprogram = \"explore\"\nmust_include = [\"history.tail\"]")?;
        let mut config = own.clone();
        let bound =
            bind_context_program(&own, &mut config, &library()?, OffsetDateTime::UNIX_EPOCH)?
                .ok_or(TestError::Missing("binding"))?;
        assert_eq!(bound.name, "explore");
        assert_eq!(bound.deferred, ["plan.current"]);
        let names: Vec<&str> = bound
            .program
            .sections
            .iter()
            .map(|section| section.name.as_str())
            .collect();
        assert_eq!(names, ["task.objective", "plan.current", "repo.tree"]);
        assert_eq!(
            bound.program.sections[1].detail,
            harw_context::DetailMode::References
        );
        assert_eq!(
            bound.program.sections[1].trust,
            harw_context::TrustClass::Evidence
        );
        let context = config
            .get("context")
            .and_then(toml::Value::as_table)
            .ok_or(TestError::Missing("[context]"))?;
        assert_eq!(
            table_strings(Some(context), "must_include"),
            ["task.objective", "history.tail"]
        );
        assert_eq!(
            context.get("policy").and_then(toml::Value::as_str),
            Some("harwness.context.explore@1")
        );
        Ok(())
    }

    #[test]
    fn test_unknown_program_names_the_known_ones() -> TestResult {
        let own = tables("[context]\nprogram = \"nope\"")?;
        let mut config = own.clone();
        let result =
            bind_context_program(&own, &mut config, &library()?, OffsetDateTime::UNIX_EPOCH);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "unknown program must fail".to_owned(),
            ));
        };
        assert!(matches!(error, DslError::UnknownContextProgram { .. }));
        let message = error.to_string();
        assert!(
            message.contains("nope") && message.contains("explore"),
            "{message}"
        );
        Ok(())
    }

    #[test]
    fn test_no_program_leaves_the_config_untouched() -> TestResult {
        let own = tables("[context]\nmust_include = [\"task.objective\"]")?;
        let mut config = own.clone();
        let bound =
            bind_context_program(&own, &mut config, &library()?, OffsetDateTime::UNIX_EPOCH)?;
        assert!(bound.is_none());
        assert_eq!(config, own);
        Ok(())
    }

    #[test]
    fn test_same_name_with_different_id_is_rejected_and_user_layer_can_patch() -> TestResult {
        let mut library = library()?;
        let clash = EXPLORE.replace("harwness.context.explore@1", "acme.context.explore@1");
        assert!(
            library
                .insert_sources(DefinitionLayer::Project, &[("explore", clash.as_str())])
                .is_err()
        );
        let user = r#"
schema = "harwness.context/v1"
id = "acme.context.mine@1"
version = "1.0.0"

[[sections]]
name = "history.tail"
strength = "must-include"
"#;
        library.insert_sources(DefinitionLayer::UserGlobal, &[("mine", user)])?;
        assert!(library.names().contains(&"mine"));
        let resolved = library.resolve("mine", OffsetDateTime::UNIX_EPOCH)?;
        assert_eq!(resolved.sections.len(), 1);
        Ok(())
    }
}
