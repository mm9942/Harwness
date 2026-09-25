//! The [`Compiler`] driver: input → front end → passes → finished unit and
//! artifact.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use harw_agent_artifact::Artifact;
use harw_agent_dsl::diagnostics::SourceFile;
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::ir_v2::Interface;
use harw_agent_dsl::lower_v2::{LowerSources, compile_agent};
use harw_agent_dsl::{Diagnostic, Diagnostics};
use harw_catalog::SkillIndex;
use time::OffsetDateTime;

use crate::artifact_out::{agent_input, build_artifact};
use crate::discovery::{AgentInput, BrokenDefinition, DefinitionEntry, SourceSet, Target};
use crate::env::CompilerEnv;
use crate::error::CompileError;
use crate::passes::{
    ChildClosure, ChildResolver, Pass, PruneUnusedTools, ReachableTools, ResolveModels,
    ResolveSkills, ResolvedChild, RightsCheck, ValidateRoles,
};
use crate::rights::{BuiltinCeilings, RightsSet};
use crate::unit::CompileUnit;

/// Options of one compiler instance.
#[derive(Debug, Clone)]
pub struct CompilerOptions {
    /// Timestamp of the resolution trace (never part of an artifact).
    pub now: OffsetDateTime,
    /// The author ceiling (`RightsCheck`), if the build has one.
    pub author_ceiling: Option<RightsSet>,
    /// Interfaces for the compiled root, overriding `[binary].interfaces`.
    pub interfaces: Option<Vec<Interface>>,
    /// Default interface for the compiled root (must be one of the
    /// interfaces; ignored otherwise).
    pub default_interface: Option<Interface>,
    /// Binary name for the compiled root, overriding `[binary].name`.
    pub binary_name: Option<String>,
}

impl Default for CompilerOptions {
    fn default() -> Self {
        Self {
            now: OffsetDateTime::now_utc(),
            author_ceiling: None,
            interfaces: None,
            default_interface: None,
            binary_name: None,
        }
    }
}

/// A finished compile.
#[derive(Debug, Clone)]
pub struct Compiled {
    /// The unit after all passes, snapshot recomputed.
    pub unit: CompileUnit,
    /// Warnings and notes (front end and passes).
    pub diagnostics: Diagnostics,
    /// The artifact.
    pub artifact: Artifact,
}

/// The agent compiler.
///
/// # Concurrency
/// `Send + Sync`; the child cache is behind a mutex.
pub struct Compiler {
    env: CompilerEnv,
    sources: SourceSet,
    files: Arc<Vec<SourceFile>>,
    skills: SkillIndex,
    ceilings: BuiltinCeilings,
    options: CompilerOptions,
    children: Mutex<BTreeMap<String, Result<ResolvedChild, String>>>,
}

impl std::fmt::Debug for Compiler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compiler")
            .field("home", &self.env.home)
            .field("layers", &self.env.layers)
            .field("definitions", &self.sources.entries.len())
            .finish_non_exhaustive()
    }
}

impl Compiler {
    /// Discovers definitions and skills of `env` and loads the built-in
    /// ceilings.
    ///
    /// # Errors
    /// [`CompileError::Other`] if a built-in definition is broken.
    pub fn new(env: CompilerEnv, options: CompilerOptions) -> Result<Self, CompileError> {
        let sources = SourceSet::discover(&env.layers)?;
        let skills = SkillIndex::build(&env.layers);
        let ceilings = BuiltinCeilings::load(options.now)?;
        Ok(Self {
            files: Arc::new(sources.files.clone()),
            sources,
            skills,
            ceilings,
            options,
            env,
            children: Mutex::new(BTreeMap::new()),
        })
    }

    /// The environment.
    #[must_use]
    pub fn env(&self) -> &CompilerEnv {
        &self.env
    }

    /// The discovered sources.
    #[must_use]
    pub fn sources(&self) -> &SourceSet {
        &self.sources
    }

    /// The built-in ceilings.
    #[must_use]
    pub fn ceilings(&self) -> &BuiltinCeilings {
        &self.ceilings
    }

    /// The options.
    #[must_use]
    pub fn options(&self) -> &CompilerOptions {
        &self.options
    }

    /// Resolves a command-line argument to a definition. A path is added to
    /// the sources (see [`SourceSet::add_explicit`]).
    ///
    /// # Errors
    /// [`CompileError::UnknownAgent`] for an unknown name,
    /// [`CompileError::NotADefinition`] for a bad path.
    pub fn resolve(&mut self, input: &AgentInput) -> Result<Target, CompileError> {
        match input {
            AgentInput::Path(path) => {
                let target = self.sources.add_explicit(path)?;
                self.files = Arc::new(self.sources.files.clone());
                self.clear_child_cache();
                Ok(target)
            }
            AgentInput::Name(name) => {
                if let Some(entry) = self.sources.find(name) {
                    return Ok(Target::Entry(entry.clone()));
                }
                if let Some(broken) = self.sources.find_broken(name) {
                    return Ok(Target::Broken(broken.clone()));
                }
                Err(CompileError::UnknownAgent {
                    name: name.clone(),
                    suggestions: self.sources.suggestions(name),
                })
            }
        }
    }

    fn clear_child_cache(&self) {
        if let Ok(mut cache) = self.children.lock() {
            cache.clear();
        }
    }

    /// Parse, resolve and lower `entry` into a fresh unit.
    ///
    /// # Errors
    /// The front-end diagnostics (at least one error).
    pub fn front_end(&self, entry: &DefinitionEntry) -> Result<CompileUnit, Diagnostics> {
        let sources = self.sources.lower_sources();
        let ir = compile_agent(&entry.id, &sources, self.options.now)?;
        Ok(CompileUnit::new(
            entry.name.clone(),
            ir,
            entry.dir.clone(),
            Arc::clone(&self.files),
        ))
    }

    /// The diagnostics of a definition file that does not parse.
    #[must_use]
    pub fn broken_diagnostics(&self, broken: &BrokenDefinition) -> Diagnostics {
        let files = vec![broken.file.clone()];
        let sources = LowerSources::new(&files);
        let placeholder = DefinitionId::parse("harwness.agent.unparsed@1");
        match placeholder {
            Ok(id) => match compile_agent(&id, &sources, self.options.now) {
                Err(diagnostics) => diagnostics,
                Ok(_) => Diagnostics::new(),
            },
            Err(error) => Diagnostics::from(Diagnostic::from_dsl_error(&error)),
        }
    }

    /// The passes in pipeline order for a unit at `stack.len()` depth.
    fn passes<'a>(&'a self, stack: &[String]) -> Vec<Box<dyn Pass + 'a>> {
        vec![
            Box::new(ValidateRoles),
            Box::new(RightsCheck {
                ceilings: &self.ceilings,
                author: self.options.author_ceiling.as_ref(),
            }),
            Box::new(ResolveSkills {
                index: &self.skills,
            }),
            Box::new(ReachableTools),
            Box::new(PruneUnusedTools),
            Box::new(ResolveModels),
            Box::new(ChildClosure {
                resolver: self,
                stack: stack.to_vec(),
            }),
        ]
    }

    /// Runs every pass over `unit`, stopping after the first pass that
    /// reports an error. Returns all diagnostics.
    pub fn run_passes(&self, unit: &mut CompileUnit, stack: &[String]) -> Diagnostics {
        let mut diagnostics = Diagnostics::new();
        for pass in self.passes(stack) {
            let reported = pass.run(unit);
            let failed = reported.has_errors();
            diagnostics.extend(reported);
            if failed {
                break;
            }
        }
        diagnostics
    }

    /// Compiles `entry` completely: front end, passes, snapshot, artifact.
    ///
    /// # Errors
    /// [`CompileError::Diagnostics`] with every diagnostic (at least one
    /// error), or an artifact error.
    pub fn compile_entry(&self, entry: &DefinitionEntry) -> Result<Compiled, CompileError> {
        self.compile_at(entry, &[], true)
    }

    fn compile_at(
        &self,
        entry: &DefinitionEntry,
        stack: &[String],
        root: bool,
    ) -> Result<Compiled, CompileError> {
        let (unit, diagnostics) = self.compile_unit(entry, stack, root)?;
        let artifact = build_artifact(&unit)?;
        Ok(Compiled {
            unit,
            diagnostics,
            artifact,
        })
    }

    /// Front end, passes and the final snapshot, without the artifact.
    fn compile_unit(
        &self,
        entry: &DefinitionEntry,
        stack: &[String],
        root: bool,
    ) -> Result<(CompileUnit, Diagnostics), CompileError> {
        let mut unit = self.front_end(entry)?;
        unit.depth = u32::try_from(stack.len()).unwrap_or(u32::MAX);
        crate::uia::embed_uia_bundle(&mut unit);
        if root {
            apply_root_options(&mut unit, &self.options);
        }
        let mut diagnostics = Diagnostics::from(unit.ir.trace.diagnostics.clone());
        diagnostics.extend(self.run_passes(&mut unit, stack));
        if diagnostics.has_errors() {
            return Err(CompileError::Diagnostics(diagnostics));
        }
        unit.ir = unit.ir.with_snapshot();
        Ok((unit, diagnostics))
    }

    /// Resolves and compiles a command-line argument.
    ///
    /// # Errors
    /// See [`Self::resolve`] and [`Self::compile_entry`]; a file that does
    /// not parse is [`CompileError::Diagnostics`].
    pub fn compile_input(&mut self, input: &AgentInput) -> Result<Compiled, CompileError> {
        match self.resolve(input)? {
            Target::Entry(entry) => self.compile_entry(&entry),
            Target::Broken(broken) => {
                Err(CompileError::Diagnostics(self.broken_diagnostics(&broken)))
            }
        }
    }

    /// Front end only (no passes): the lowered IR of an argument.
    ///
    /// # Errors
    /// As [`Self::compile_input`].
    pub fn lower_input(&mut self, input: &AgentInput) -> Result<CompileUnit, CompileError> {
        match self.resolve(input)? {
            Target::Entry(entry) => Ok(self.front_end(&entry)?),
            Target::Broken(broken) => {
                Err(CompileError::Diagnostics(self.broken_diagnostics(&broken)))
            }
        }
    }
}

/// Applies the root-only options (interfaces, default interface, binary
/// name) to a freshly lowered unit.
pub fn apply_root_options(unit: &mut CompileUnit, options: &CompilerOptions) {
    if let Some(interfaces) = &options.interfaces {
        apply_interfaces(unit, interfaces);
    }
    if let Some(default) = options
        .default_interface
        .filter(|default| unit.ir.binary.interfaces.contains(default))
    {
        unit.ir.binary.default_interface = default;
    }
    if let Some(name) = &options.binary_name {
        unit.ir.binary.name.clone_from(name);
    }
}

/// Replaces the root's interfaces; keeps the default interface if it is in
/// the new list, else the first listed one becomes the default.
pub fn apply_interfaces(unit: &mut CompileUnit, interfaces: &[Interface]) {
    let mut list: Vec<Interface> = Vec::new();
    for interface in interfaces {
        if !list.contains(interface) {
            list.push(*interface);
        }
    }
    let Some(first) = list.first().copied() else {
        return;
    };
    if !list.contains(&unit.ir.binary.default_interface) {
        unit.ir.binary.default_interface = first;
    }
    unit.ir.binary.interfaces = list;
}

impl ChildResolver for Compiler {
    fn compile_child(
        &self,
        name: &str,
        _depth: u32,
        stack: &[String],
    ) -> Result<ResolvedChild, String> {
        if let Some(cached) = self
            .children
            .lock()
            .ok()
            .and_then(|cache| cache.get(name).cloned())
        {
            return cached;
        }
        let result = match self.sources.find(name) {
            None => Err(format!(
                "no definition named `{name}` in any layer{}",
                suggestion_suffix(&self.sources.suggestions(name))
            )),
            Some(entry) => {
                let entry = entry.clone();
                match self.compile_unit(&entry, stack, false) {
                    Ok((unit, _)) => match agent_input(&unit) {
                        Ok(own) => {
                            let mut entries = vec![own];
                            entries.extend(unit.agents.iter().cloned());
                            Ok(ResolvedChild {
                                snapshot: unit
                                    .ir
                                    .snapshot
                                    .as_ref()
                                    .map(|snapshot| snapshot.digest.clone())
                                    .unwrap_or_default(),
                                entries,
                                children: unit.children.clone(),
                                ir: unit.ir,
                            })
                        }
                        Err(error) => Err(error.to_string()),
                    },
                    Err(CompileError::Diagnostics(diagnostics)) => Err(format!(
                        "it does not compile:\n{}",
                        indent(&diagnostics.to_string())
                    )),
                    Err(other) => Err(other.to_string()),
                }
            }
        };
        if let Ok(mut cache) = self.children.lock() {
            cache.insert(name.to_owned(), result.clone());
        }
        result
    }
}

fn suggestion_suffix(suggestions: &[String]) -> String {
    if suggestions.is_empty() {
        String::new()
    } else {
        format!(" (did you mean: {})", suggestions.join(", "))
    }
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
