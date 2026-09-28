//! The shared `harw agent …` command surface.
//!
//! `harw-cli` parses its clap grammar into an [`AgentCommand`];
//! the `/agent` op in `harw-ops` parses its raw tokens with
//! [`parse_tokens`]. Both call [`run_command`] and print the
//! [`CommandOutput`] as text or JSON, so the CLI and the TUI behave the
//! same.

use std::path::{Path, PathBuf};

use harw_agent_dsl::diagnostics::Diagnostic;
use harw_agent_dsl::ir_v2::{AgentIr, Interface};
use harw_agent_dsl::roles::AgentRoleId;
use serde::Serialize;
use serde_json::{Value, json};

use crate::backend::native::NativeFlavor;
use crate::backend::runner::{RunnerProbe, locate_runner};
use crate::backend::{BuildOptions, build};
use crate::bin_dir::BinDir;
use crate::cache::{
    AgentCompilerSettings, DEFAULT_KEEP_VERSIONS, GcPolicy, collect_garbage, dir_size,
    stale_runner_dirs, unix_now,
};
use crate::compiler::{Compiled, Compiler, CompilerOptions};
use crate::diff::diff_irs;
use crate::discovery::{AgentInput, DefinitionEntry, Target};
use crate::doctor::{human_bytes, render_doctor, run_doctor};
use crate::env::{CompilerEnv, InstallRecord};
use crate::error::CompileError;
use crate::explain::{DEFAULT_FIELDS, explain_code, explain_field, render_fields};
use crate::fmt::{definition_files, format_files};
use crate::graph::{
    GraphFormat, GraphKind, delegation_graph, render, resolution_graph, rights_graph,
};
use crate::inspect::{inspect_path, read_artifact, resolve_target};
use crate::render::render_diagnostics;
use crate::scaffold::{ScaffoldRole, scaffold};
use crate::testing::{CaseRunner, case_files, render_cases, run_cases};
use crate::uia::{auto_build_uia, explicit_binary, native_binary_name, native_home_name};

/// Exit code of `harw agent run` when no `harw-agent-runner` can be located
/// at all for a bare artifact target (EX_UNAVAILABLE); see [`run_agent`].
pub const EXIT_RUNNER_UNAVAILABLE: i32 = 69;

/// Arguments of `build`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildArgs {
    /// Name or path.
    pub target: String,
    /// `--interface`.
    pub interfaces: Option<Vec<Interface>>,
    /// `--native`.
    pub native: bool,
    /// `--artifact-only`.
    pub artifact_only: bool,
    /// `--runner`.
    pub runner: Option<PathBuf>,
    /// `-o`.
    pub output: Option<PathBuf>,
    /// `--target`.
    pub target_triple: Option<String>,
    /// `--harw-src`.
    pub harw_src: Option<PathBuf>,
}

/// Arguments of `clean`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanArgs {
    /// Remove the whole cache and all old versions.
    pub all: bool,
    /// Remove cache entries unused for this many days.
    pub older_than_days: Option<u64>,
    /// Old versions kept per agent (besides the current one).
    pub keep: Option<usize>,
    /// Only report.
    pub dry_run: bool,
}

/// One `harw agent` command of the compiler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    /// `check [name|path]…`.
    Check {
        /// Targets; empty: every user definition.
        targets: Vec<String>,
    },
    /// `build`.
    Build(BuildArgs),
    /// `inspect <binary|artifact|installed name>`.
    Inspect {
        /// Target.
        target: String,
    },
    /// `graph`.
    Graph {
        /// A name or path; `None` with `all`.
        target: Option<String>,
        /// Every user definition.
        all: bool,
        /// Output format.
        format: GraphFormat,
        /// Which graph.
        kind: GraphKind,
    },
    /// `explain <name|code> [field]`.
    Explain {
        /// A definition or a diagnostic code.
        target: String,
        /// A field path.
        field: Option<String>,
    },
    /// `new <name>`.
    New {
        /// Agent name.
        name: String,
        /// Role.
        role: ScaffoldRole,
        /// `--extends`.
        extends: Option<String>,
        /// `--dir`.
        dir: Option<PathBuf>,
    },
    /// `fmt [paths] [--check]`.
    Fmt {
        /// Files or directories; empty: every layer's `agents/`.
        paths: Vec<PathBuf>,
        /// Only check.
        check: bool,
    },
    /// `diff <a> <b>`.
    Diff {
        /// Left side.
        left: String,
        /// Right side.
        right: String,
    },
    /// `test [name]`.
    Test {
        /// A name or path; `None`: every user definition with `tests/`.
        target: Option<String>,
    },
    /// `run <artifact|name> [prompt]` (wave 3).
    Run {
        /// Target.
        target: String,
        /// Prompt.
        prompt: Option<String>,
    },
    /// `versions <name>`.
    Versions {
        /// Installed agent name.
        name: String,
    },
    /// `use <name> <version|digest>`.
    Use {
        /// Installed agent name.
        name: String,
        /// Version, digest prefix or directory name.
        version: String,
    },
    /// `clean`.
    Clean(CleanArgs),
    /// `doctor`.
    Doctor,
    /// `install-record` (hidden; `make install`).
    InstallRecord {
        /// The source checkout.
        source_dir: Option<PathBuf>,
        /// Where harw was installed.
        bindir: Option<PathBuf>,
    },
    /// `auto-build-uia` (hidden; startup and `make install`).
    AutoBuildUia,
}

/// The subcommand names [`parse_tokens`] understands.
pub const COMPILER_ACTIONS: &[&str] = &[
    "check", "build", "inspect", "graph", "explain", "new", "fmt", "diff", "test", "run",
    "versions", "clean", "doctor",
];

/// What a command produced.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutput {
    /// Text for the terminal.
    pub text: String,
    /// The same as JSON (`--json`).
    pub json: Value,
    /// Process exit code (0 success, 1 errors, 69 needs the runner).
    pub exit_code: i32,
}

impl CommandOutput {
    fn ok(text: String, json: Value) -> Self {
        Self {
            text,
            json,
            exit_code: 0,
        }
    }

    fn failed(text: String, json: Value) -> Self {
        Self {
            text,
            json,
            exit_code: 1,
        }
    }

    fn from_error(error: &CompileError, files: &[harw_agent_dsl::diagnostics::SourceFile]) -> Self {
        match error {
            CompileError::Diagnostics(diagnostics) => Self::failed(
                render_diagnostics(diagnostics.as_slice(), files),
                json!({"error": error.kind(), "diagnostics": diagnostics}),
            ),
            other => Self::failed(
                format!("error: {other}"),
                json!({"error": other.kind(), "message": other.to_string()}),
            ),
        }
    }
}

/// Everything a command needs besides its arguments.
pub struct CommandContext<'a> {
    /// The environment.
    pub env: CompilerEnv,
    /// Asks runners for their capabilities.
    pub probe: &'a dyn RunnerProbe,
    /// Runs test cases (the stub until wave 3).
    pub case_runner: &'a dyn CaseRunner,
    /// Progress lines (stderr in the CLI, job log in the TUI).
    pub progress: &'a mut dyn FnMut(&str),
}

fn to_json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Runs a command.
#[must_use]
pub fn run_command(ctx: &mut CommandContext<'_>, command: AgentCommand) -> CommandOutput {
    match command {
        AgentCommand::Check { targets } => check(ctx, &targets),
        AgentCommand::Build(args) => build_command(ctx, &args),
        AgentCommand::Inspect { target } => inspect(ctx, &target),
        AgentCommand::Graph {
            target,
            all,
            format,
            kind,
        } => graph(ctx, target.as_deref(), all, format, kind),
        AgentCommand::Explain { target, field } => explain(ctx, &target, field.as_deref()),
        AgentCommand::New {
            name,
            role,
            extends,
            dir,
        } => new(ctx, &name, role, extends.as_deref(), dir.as_deref()),
        AgentCommand::Fmt { paths, check } => fmt(ctx, &paths, check),
        AgentCommand::Diff { left, right } => diff(ctx, &left, &right),
        AgentCommand::Test { target } => test(ctx, target.as_deref()),
        AgentCommand::Run { target, prompt } => run_agent(ctx, &target, prompt.as_deref()),
        AgentCommand::Versions { name } => versions(ctx, &name),
        AgentCommand::Use { name, version } => use_version(ctx, &name, &version),
        AgentCommand::Clean(args) => clean(ctx, &args),
        AgentCommand::Doctor => {
            let checks = run_doctor(&ctx.env, ctx.probe);
            CommandOutput::ok(render_doctor(&checks), json!({"checks": checks}))
        }
        AgentCommand::InstallRecord { source_dir, bindir } => {
            let record = InstallRecord::new(
                source_dir.map(|dir| ctx.env.resolve(&dir)),
                bindir.map(|dir| ctx.env.resolve(&dir)),
            );
            match record.write(&ctx.env.home) {
                Ok(path) => CommandOutput::ok(
                    format!("install record written: {}", path.display()),
                    json!({"path": path, "record": record}),
                ),
                Err(error) => CommandOutput::from_error(&error, &[]),
            }
        }
        AgentCommand::AutoBuildUia => {
            let outcome = auto_build_uia(&ctx.env, ctx.probe);
            CommandOutput::ok(format!("{outcome:?}"), to_json(&outcome))
        }
    }
}

fn compiler(env: &CompilerEnv, options: CompilerOptions) -> Result<Compiler, CompileError> {
    Compiler::new(env.clone(), options)
}

/// Every user definition (layers, not built-ins or bundled), unique names.
fn user_definitions(compiler: &Compiler) -> Vec<String> {
    let mut names: Vec<String> = compiler
        .sources()
        .entries
        .iter()
        .filter(|entry| matches!(entry.origin, crate::discovery::Origin::Layer(_)))
        .map(|entry| entry.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn check(ctx: &mut CommandContext<'_>, targets: &[String]) -> CommandOutput {
    let targets: Vec<String> = if targets.is_empty() {
        match compiler(&ctx.env, CompilerOptions::default()) {
            Ok(compiler) => user_definitions(&compiler),
            Err(error) => return CommandOutput::from_error(&error, &[]),
        }
    } else {
        targets.to_vec()
    };
    if targets.is_empty() {
        return CommandOutput::ok(
            "no user definitions to check (name one, or create one with `harw agent new`)"
                .to_owned(),
            json!({"targets": []}),
        );
    }
    let mut text = Vec::new();
    let mut results = Vec::new();
    let mut errors = 0;
    for target in &targets {
        let mut compiler = match compiler(&ctx.env, CompilerOptions::default()) {
            Ok(compiler) => compiler,
            Err(error) => return CommandOutput::from_error(&error, &[]),
        };
        let input = AgentInput::parse(target, &ctx.env.cwd);
        let result = compiler.compile_input(&input);
        let files = compiler.sources().files.clone();
        match result {
            Ok(compiled) => {
                let diagnostics: Vec<Diagnostic> = compiled.diagnostics.into_vec();
                let rendered = render_diagnostics(&diagnostics, &files);
                text.push(format!(
                    "{target}: ok{}",
                    if rendered.is_empty() {
                        String::new()
                    } else {
                        format!("\n{rendered}")
                    }
                ));
                results.push(json!({"target": target, "ok": true, "diagnostics": diagnostics}));
            }
            Err(error) => {
                errors += 1;
                let mut files = files;
                if let Some(broken) = compiler.sources().find_broken(target) {
                    files.push(broken.file.clone());
                }
                let output = CommandOutput::from_error(&error, &files);
                text.push(format!("{target}:\n{}", output.text));
                results.push(json!({"target": target, "ok": false, "result": output.json}));
            }
        }
    }
    let json = json!({"targets": results, "errors": errors});
    if errors > 0 {
        CommandOutput::failed(text.join("\n\n"), json)
    } else {
        CommandOutput::ok(text.join("\n\n"), json)
    }
}

fn entry_for(
    compiler: &mut Compiler,
    raw: &str,
    cwd: &Path,
) -> Result<DefinitionEntry, CommandOutput> {
    let input = AgentInput::parse(raw, cwd);
    match compiler.resolve(&input) {
        Ok(Target::Entry(entry)) => Ok(entry),
        Ok(Target::Broken(broken)) => {
            let diagnostics = compiler.broken_diagnostics(&broken);
            Err(CommandOutput::failed(
                render_diagnostics(diagnostics.as_slice(), std::slice::from_ref(&broken.file)),
                json!({"error": "diagnostics", "diagnostics": diagnostics}),
            ))
        }
        Err(error) => Err(CommandOutput::from_error(&error, &[])),
    }
}

fn build_command(ctx: &mut CommandContext<'_>, args: &BuildArgs) -> CommandOutput {
    let mut probe_compiler = match compiler(&ctx.env, CompilerOptions::default()) {
        Ok(compiler) => compiler,
        Err(error) => return CommandOutput::from_error(&error, &[]),
    };
    let entry = match entry_for(&mut probe_compiler, &args.target, &ctx.env.cwd) {
        Ok(entry) => entry,
        Err(output) => return output,
    };
    let mut options = CompilerOptions {
        interfaces: args.interfaces.clone(),
        target: args.target_triple.clone(),
        ..CompilerOptions::default()
    };
    let mut build_options = BuildOptions {
        native: args.native,
        artifact_only: args.artifact_only,
        runner: args.runner.clone(),
        output: args.output.clone(),
        target: args.target_triple.clone(),
        harw_src: args.harw_src.clone(),
        home_name: None,
    };
    if entry.role == AgentRoleId::UserInterface && args.native {
        let explicit = explicit_binary(probe_compiler.sources().file(&entry.label));
        options.binary_name = Some(native_binary_name(explicit.name.as_deref(), &entry.id.name));
        build_options.home_name = Some(native_home_name(
            explicit.name.as_deref(),
            &entry.specialization,
        ));
    }
    let mut compiler = match compiler(&ctx.env, options) {
        Ok(compiler) => compiler,
        Err(error) => return CommandOutput::from_error(&error, &[]),
    };
    let compiled = match compiler.compile_input(&AgentInput::parse(&args.target, &ctx.env.cwd)) {
        Ok(compiled) => compiled,
        Err(error) => return CommandOutput::from_error(&error, &compiler.sources().files),
    };
    let builtin = matches!(entry.origin, crate::discovery::Origin::BuiltIn);
    (ctx.progress)(&format!(
        "compiled {} ({} diagnostics), artifact {}",
        compiled.unit.name,
        compiled.diagnostics.len(),
        compiled.artifact.digest()
    ));
    match build(&ctx.env, &compiled, &build_options, ctx.probe, ctx.progress) {
        Ok(report) => {
            let mut lines = vec![format!(
                "built {} {} ({}) → {}",
                report.name,
                report.version,
                report.backend.as_str(),
                report.installed.file.display()
            )];
            if report.installed.current {
                lines.push(format!(
                    "current: {}",
                    ctx.env.bin_dir().join(&report.name).display()
                ));
            }
            if let Some(output) = &report.output {
                lines.push(format!("copied to {}", output.display()));
            }
            lines.push(format!("artifact {}", report.artifact_digest));
            if builtin {
                lines.push(
                    "note: a built-in role is only copied; harw keeps running its embedded built-in".to_owned(),
                );
            }
            if report
                .native
                .as_ref()
                .is_some_and(|native| native.flavor == NativeFlavor::Harw)
            {
                lines.push(
                    "a complete, personalized harw with this UIA as its fixed root".to_owned(),
                );
            }
            lines.extend(report.notes.iter().map(|note| format!("note: {note}")));
            let rendered =
                render_diagnostics(compiled.diagnostics.as_slice(), &compiler.sources().files);
            if !rendered.is_empty() {
                lines.push(rendered);
            }
            CommandOutput::ok(
                lines.join("\n"),
                json!({"build": report, "diagnostics": compiled.diagnostics}),
            )
        }
        Err(error) => CommandOutput::from_error(&error, &compiler.sources().files),
    }
}

fn inspect(ctx: &mut CommandContext<'_>, target: &str) -> CommandOutput {
    let result =
        resolve_target(&ctx.env, target).and_then(|(path, record)| inspect_path(&path, record));
    match result {
        Ok(report) => CommandOutput::ok(report.text(), to_json(&report)),
        Err(error) => CommandOutput::from_error(&error, &[]),
    }
}

/// `harw agent run <artifact|name> [prompt]`.
///
/// # Decision (wave 3B): the runner always runs out of process
/// `harw-agent-compiler` has no dependency on `harw-agent-runner`, and
/// through it none on `harw-runtime` or `harwness-sdk`. That is not new
/// here: this crate's `Cargo.toml` already states "compiling never starts
/// an agent" as a standing invariant, and `harw-agent-runner`'s manifest
/// only *dev*-depends on the compiler (for its `--capabilities` fixture),
/// deliberately keeping that a one-way, test-only edge with "no cycle:
/// `harw-agent-compiler` does not depend on us" spelled out in a comment.
/// So `run` never links the runner in-process; it always execs it as a
/// subprocess, the same way `harw agent build`'s default backend locates
/// one ([`locate_runner`]):
///
/// - A target that is already a built binary (an installed agent name, or
///   a path to one) is executed directly: it already *is*
///   `harw-agent-runner` fused with that agent's artifact.
/// - A bare `.harwa` artifact file has no runner of its own: a matching
///   runner is located, the artifact is appended to a copy of it in memory
///   (the exact [`harw_agent_artifact::append_to_executable`] the artifact
///   backend uses at build time), and the combined bytes are written to a
///   throwaway executable that is exec'ed and then removed.
///
/// The prompt, if given, is passed as the runner's one positional argument
/// (its one-shot mode; see `iface::cli`); without one the runner reads
/// stdin itself. Stdio is inherited, so streaming output and the terminal
/// approval handler behave exactly as a direct invocation would. The
/// runner's own exit code (0 completed, 1 failed, 2 cancelled, 3 approval
/// denied) passes through unchanged; a runner that cannot be found at all
/// keeps reporting [`EXIT_RUNNER_UNAVAILABLE`], as `run` always has.
fn run_agent(ctx: &mut CommandContext<'_>, target: &str, prompt: Option<&str>) -> CommandOutput {
    let (path, _record) = match resolve_target(&ctx.env, target) {
        Ok(resolved) => resolved,
        Err(error) => return CommandOutput::from_error(&error, &[]),
    };
    let (artifact, container, _runner_bytes) = match read_artifact(&path) {
        Ok(read) => read,
        Err(error) => return CommandOutput::from_error(&error, &[]),
    };
    let mut cleanup: Option<PathBuf> = None;
    let executable = if container == "binary" {
        path
    } else {
        let runner = match locate_runner(&ctx.env, None, &ctx.env.host_target) {
            Ok(runner) => runner,
            Err(error @ CompileError::RunnerNotFound { .. }) => {
                return CommandOutput {
                    text: format!("error: {error}"),
                    json: json!({"error": error.kind(), "message": error.to_string()}),
                    exit_code: EXIT_RUNNER_UNAVAILABLE,
                };
            }
            Err(error) => return CommandOutput::from_error(&error, &[]),
        };
        (ctx.progress)(&format!(
            "runner: {} ({:?})",
            runner.path.display(),
            runner.source
        ));
        let runner_bytes = match std::fs::read(&runner.path) {
            Ok(bytes) => bytes,
            Err(source) => {
                return CommandOutput::from_error(
                    &CompileError::Io {
                        context: format!("read {}", runner.path.display()),
                        source,
                    },
                    &[],
                );
            }
        };
        let binary = harw_agent_artifact::append_to_executable(&runner_bytes, &artifact);
        let temp = std::env::temp_dir().join(format!(
            "harw-agent-run-{}-{}",
            std::process::id(),
            artifact.digest()
        ));
        if let Err(source) = harw_agent_artifact::write_executable(&temp, &binary) {
            return CommandOutput::from_error(&CompileError::Artifact(source), &[]);
        }
        cleanup = Some(temp.clone());
        temp
    };
    let mut command = std::process::Command::new(&executable);
    if let Some(prompt) = prompt {
        command.arg(prompt);
    }
    let status = command.status();
    if let Some(temp) = &cleanup {
        let _ = std::fs::remove_file(temp);
    }
    match status {
        Ok(status) => {
            let exit_code = status.code().unwrap_or(1);
            CommandOutput {
                text: format!("`{}` exited with {exit_code}", executable.display()),
                json: json!({"executable": executable, "exit_code": exit_code}),
                exit_code,
            }
        }
        Err(source) => CommandOutput::from_error(
            &CompileError::Io {
                context: format!("run {}", executable.display()),
                source,
            },
            &[],
        ),
    }
}

fn compile_named(
    ctx: &CommandContext<'_>,
    target: &str,
) -> Result<(Compiler, Compiled), (CompileError, Vec<harw_agent_dsl::diagnostics::SourceFile>)> {
    let mut compiler =
        compiler(&ctx.env, CompilerOptions::default()).map_err(|error| (error, Vec::new()))?;
    let result = compiler.compile_input(&AgentInput::parse(target, &ctx.env.cwd));
    match result {
        Ok(compiled) => Ok((compiler, compiled)),
        Err(error) => {
            let files = compiler.sources().files.clone();
            Err((error, files))
        }
    }
}

fn graph(
    ctx: &mut CommandContext<'_>,
    target: Option<&str>,
    all: bool,
    format: GraphFormat,
    kind: GraphKind,
) -> CommandOutput {
    let targets: Vec<String> = match (target, all) {
        (Some(target), _) => vec![target.to_owned()],
        (None, true) => match compiler(&ctx.env, CompilerOptions::default()) {
            Ok(compiler) => user_definitions(&compiler),
            Err(error) => return CommandOutput::from_error(&error, &[]),
        },
        (None, false) => {
            return CommandOutput::failed(
                "name an agent or pass --all".to_owned(),
                json!({"error": "usage"}),
            );
        }
    };
    let mut compiled_all = Vec::new();
    let mut skipped = Vec::new();
    for target in &targets {
        match compile_named(ctx, target) {
            Ok(pair) => compiled_all.push(pair),
            Err((error, _)) => skipped.push(format!("{target}: {error}")),
        }
    }
    let mut graphs = Vec::new();
    if matches!(kind, GraphKind::Delegation | GraphKind::All) {
        let refs: Vec<&Compiled> = compiled_all.iter().map(|(_, compiled)| compiled).collect();
        graphs.push(delegation_graph(&refs));
    }
    for (compiler, compiled) in &compiled_all {
        if matches!(kind, GraphKind::Resolution | GraphKind::All) {
            graphs.push(resolution_graph(compiled, compiler.sources()));
        }
        if matches!(kind, GraphKind::Rights | GraphKind::All) {
            graphs.push(rights_graph(compiled));
        }
    }
    let mut text = render(&graphs, format);
    if !skipped.is_empty() && format == GraphFormat::Text {
        text.push_str(&format!(
            "\nskipped (does not compile):\n  {}\n",
            skipped.join("\n  ")
        ));
    }
    let json = json!({"graphs": graphs, "skipped": skipped});
    if compiled_all.is_empty() && !skipped.is_empty() {
        CommandOutput::failed(text, json)
    } else {
        CommandOutput::ok(text, json)
    }
}

fn explain(ctx: &mut CommandContext<'_>, target: &str, field: Option<&str>) -> CommandOutput {
    if crate::codes::looks_like_code(target) {
        return match explain_code(target) {
            Some(explanation) => CommandOutput::ok(explanation.text(), to_json(&explanation)),
            None => CommandOutput::failed(
                format!("unknown diagnostic code `{target}`"),
                json!({"error": "unknown-code", "code": target}),
            ),
        };
    }
    let (compiler, compiled) = match compile_named(ctx, target) {
        Ok(pair) => pair,
        Err((error, files)) => return CommandOutput::from_error(&error, &files),
    };
    let fields: Vec<String> = match field {
        Some(field) => vec![field.to_owned()],
        None => DEFAULT_FIELDS
            .iter()
            .map(|field| (*field).to_owned())
            .collect(),
    };
    let explanations: Vec<_> = fields
        .iter()
        .map(|field| explain_field(&compiled, compiler.sources(), field))
        .collect();
    CommandOutput::ok(
        render_fields(&compiled.unit.name, &explanations),
        json!({"agent": compiled.unit.name, "fields": explanations}),
    )
}

fn new(
    ctx: &mut CommandContext<'_>,
    name: &str,
    role: ScaffoldRole,
    extends: Option<&str>,
    dir: Option<&Path>,
) -> CommandOutput {
    let dir = dir.map_or_else(
        || ctx.env.home.join("agents").join(name),
        |dir| ctx.env.resolve(dir),
    );
    match scaffold(&dir, name, role, extends) {
        Ok(scaffolded) => CommandOutput::ok(
            format!(
                "created {}\n{}\ncheck it with: harw agent check {}",
                scaffolded.id,
                scaffolded
                    .files
                    .iter()
                    .map(|file| format!("  {}", file.display()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                dir.display()
            ),
            to_json(&scaffolded),
        ),
        Err(error) => CommandOutput::from_error(&error, &[]),
    }
}

fn fmt(ctx: &mut CommandContext<'_>, paths: &[PathBuf], check: bool) -> CommandOutput {
    let roots: Vec<PathBuf> = if paths.is_empty() {
        ctx.env
            .layers
            .iter()
            .map(|layer| layer.join("agents"))
            .filter(|dir| dir.is_dir())
            .collect()
    } else {
        paths.iter().map(|path| ctx.env.resolve(path)).collect()
    };
    let mut files = Vec::new();
    for root in &roots {
        match definition_files(root) {
            Ok(found) => files.extend(found),
            Err(error) => return CommandOutput::from_error(&error, &[]),
        }
    }
    let results = format_files(&files, check);
    let changed = results.iter().filter(|result| result.changed).count();
    let failed = results
        .iter()
        .filter(|result| result.error.is_some())
        .count();
    let mut lines: Vec<String> = results
        .iter()
        .filter(|result| result.changed || result.error.is_some())
        .map(|result| match &result.error {
            Some(error) => format!("error {}: {error}", result.path.display()),
            None if check => format!("would reformat {}", result.path.display()),
            None => format!("formatted {}", result.path.display()),
        })
        .collect();
    lines.push(format!(
        "{} file(s), {changed} {}, {failed} error(s)",
        results.len(),
        if check {
            "not formatted"
        } else {
            "reformatted"
        }
    ));
    let json = json!({"files": results, "changed": changed, "errors": failed, "check": check});
    if failed > 0 || (check && changed > 0) {
        CommandOutput::failed(lines.join("\n"), json)
    } else {
        CommandOutput::ok(lines.join("\n"), json)
    }
}

/// Loads one side of a diff: an artifact or binary file, an installed
/// version (`name@version`), or a definition (name or path).
fn diff_side(ctx: &CommandContext<'_>, raw: &str) -> Result<AgentIr, CommandOutput> {
    let path = ctx.env.resolve(Path::new(raw));
    let is_definition_file = path.extension().and_then(|ext| ext.to_str()) == Some("toml");
    if path.is_file() && !is_definition_file {
        return read_artifact(&path)
            .and_then(|(artifact, _, _)| crate::artifact_out::ir_from_artifact(&artifact))
            .map_err(|error| CommandOutput::from_error(&error, &[]));
    }
    if let Some((name, selector)) = raw.rsplit_once('@') {
        let bin = BinDir::new(ctx.env.bin_dir());
        let chosen = bin.versions(name).ok().and_then(|versions| {
            versions.into_iter().find(|version| {
                version.dir_name == selector
                    || version.record.version == selector
                    || (selector.len() >= 4 && version.record.artifact_digest.starts_with(selector))
            })
        });
        if let Some(version) = chosen {
            return read_artifact(&version.file)
                .and_then(|(artifact, _, _)| crate::artifact_out::ir_from_artifact(&artifact))
                .map_err(|error| CommandOutput::from_error(&error, &[]));
        }
    }
    match compile_named(ctx, raw) {
        Ok((_, compiled)) => Ok(compiled.unit.ir),
        Err((error, files)) => Err(CommandOutput::from_error(&error, &files)),
    }
}

fn diff(ctx: &mut CommandContext<'_>, left: &str, right: &str) -> CommandOutput {
    let left_ir = match diff_side(ctx, left) {
        Ok(ir) => ir,
        Err(output) => return output,
    };
    let right_ir = match diff_side(ctx, right) {
        Ok(ir) => ir,
        Err(output) => return output,
    };
    let diff = diff_irs(left, &left_ir, right, &right_ir);
    CommandOutput::ok(diff.text(), to_json(&diff))
}

fn test(ctx: &mut CommandContext<'_>, target: Option<&str>) -> CommandOutput {
    let targets: Vec<String> = match target {
        Some(target) => vec![target.to_owned()],
        None => match compiler(&ctx.env, CompilerOptions::default()) {
            Ok(compiler) => compiler
                .sources()
                .entries
                .iter()
                .filter(|entry| {
                    entry
                        .dir
                        .as_ref()
                        .is_some_and(|dir| dir.join("tests").is_dir())
                })
                .map(|entry| entry.name.clone())
                .collect(),
            Err(error) => return CommandOutput::from_error(&error, &[]),
        },
    };
    if targets.is_empty() {
        return CommandOutput::ok(
            "no definition with a tests/ folder".to_owned(),
            json!({"targets": []}),
        );
    }
    let mut text = Vec::new();
    let mut json_results = Vec::new();
    let mut failed = false;
    for target in &targets {
        let (_, compiled) = match compile_named(ctx, target) {
            Ok(pair) => pair,
            Err((error, files)) => {
                failed = true;
                let output = CommandOutput::from_error(&error, &files);
                text.push(format!("{target}: does not compile\n{}", output.text));
                json_results.push(json!({"target": target, "ok": false, "result": output.json}));
                continue;
            }
        };
        let files = compiled
            .unit
            .dir
            .as_deref()
            .map(case_files)
            .unwrap_or_default();
        match run_cases(
            &compiled.unit.ir,
            &compiled.artifact,
            &files,
            ctx.case_runner,
        ) {
            Ok(results) => {
                let ok = results.iter().all(crate::testing::CaseResult::ok);
                failed |= !ok;
                text.push(format!(
                    "{target}: valid, artifact {} built in memory, {} case(s)\n{}",
                    compiled.artifact.digest(),
                    results.len(),
                    render_cases(&results, ctx.case_runner.is_real())
                ));
                json_results.push(json!({
                    "target": target,
                    "ok": ok,
                    "artifact_digest": compiled.artifact.digest().to_hex(),
                    "runner": if ctx.case_runner.is_real() { "real" } else { "stub" },
                    "cases": results,
                }));
            }
            Err(error) => {
                failed = true;
                text.push(format!("{target}: {error}"));
            }
        }
    }
    let json = json!({"targets": json_results});
    if failed {
        CommandOutput::failed(text.join("\n"), json)
    } else {
        CommandOutput::ok(text.join("\n"), json)
    }
}

fn versions(ctx: &mut CommandContext<'_>, name: &str) -> CommandOutput {
    let bin = BinDir::new(ctx.env.bin_dir());
    match bin.versions(name) {
        Ok(versions) if versions.is_empty() => CommandOutput::failed(
            format!(
                "`{name}` has no installed versions in {}",
                ctx.env.bin_dir().display()
            ),
            json!({"name": name, "versions": []}),
        ),
        Ok(versions) => {
            let lines: Vec<String> = versions
                .iter()
                .map(|version| {
                    format!(
                        "{} {}  {}  {}  {}",
                        if version.current { "*" } else { " " },
                        version.dir_name,
                        version.record.backend,
                        version.record.built_at,
                        version.record.interfaces.join(",")
                    )
                })
                .collect();
            CommandOutput::ok(
                lines.join("\n"),
                json!({"name": name, "versions": versions}),
            )
        }
        Err(error) => CommandOutput::from_error(&error, &[]),
    }
}

fn use_version(ctx: &mut CommandContext<'_>, name: &str, version: &str) -> CommandOutput {
    match BinDir::new(ctx.env.bin_dir()).use_version(name, version) {
        Ok(chosen) => CommandOutput::ok(
            format!(
                "{} → {}",
                ctx.env.bin_dir().join(name).display(),
                chosen.dir_name
            ),
            to_json(&chosen),
        ),
        Err(error) => CommandOutput::from_error(&error, &[]),
    }
}

fn clean(ctx: &mut CommandContext<'_>, args: &CleanArgs) -> CommandOutput {
    let settings = AgentCompilerSettings::load(&ctx.env);
    let cache = ctx.env.build_cache_dir();
    let bin = BinDir::new(ctx.env.bin_dir());
    let bin_before = dir_size(bin.root());
    let policy = GcPolicy {
        max_bytes: settings.max_bytes(),
        keep: None,
        older_than_secs: args.older_than_days.map(|days| days.saturating_mul(86_400)),
        all: args.all,
        dry_run: args.dry_run,
    };
    let gc = match collect_garbage(&cache, &policy, &[], &[], unix_now()) {
        Ok(gc) => gc,
        Err(error) => return CommandOutput::from_error(&error, &[]),
    };
    let keep = if args.all {
        0
    } else {
        args.keep
            .or(settings.keep_versions)
            .unwrap_or(DEFAULT_KEEP_VERSIONS)
    };
    let mut removed_versions = Vec::new();
    for name in bin.names() {
        match bin.prune_versions(&name, keep, args.dry_run) {
            Ok(removed) => removed_versions.extend(removed),
            Err(error) => return CommandOutput::from_error(&error, &[]),
        }
    }
    let stale = stale_runner_dirs(&ctx.env);
    if !args.dry_run {
        for dir in &stale {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    let bin_after = if args.dry_run {
        bin_before.saturating_sub(removed_versions.iter().map(|version| version.bytes).sum())
    } else {
        dir_size(bin.root())
    };
    let verb = if args.dry_run {
        "would remove"
    } else {
        "removed"
    };
    let mut lines = vec![
        format!(
            "build cache {}: {} → {}",
            cache.display(),
            human_bytes(gc.before_bytes),
            human_bytes(gc.after_bytes)
        ),
        format!(
            "versions {}: {} → {} (current + {keep} kept per agent)",
            bin.root().display(),
            human_bytes(bin_before),
            human_bytes(bin_after)
        ),
    ];
    for removal in &gc.removed {
        lines.push(format!(
            "  {verb} {} ({}, {})",
            removal.path.display(),
            removal.reason,
            human_bytes(removal.bytes)
        ));
    }
    for version in &removed_versions {
        lines.push(format!("  {verb} {} {}", version.name, version.dir_name));
    }
    for dir in &stale {
        lines.push(format!(
            "  {verb} runner of another harw version {}",
            dir.display()
        ));
    }
    CommandOutput::ok(
        lines.join("\n"),
        json!({
            "dry_run": args.dry_run,
            "cache": gc,
            "versions": {"before_bytes": bin_before, "after_bytes": bin_after, "removed": removed_versions, "keep": keep},
            "stale_runners": stale,
        }),
    )
}

/// Parses the tokens after `/agent` (TUI op) into a compiler command;
/// `Ok(None)` if the first token is no compiler action.
///
/// # Errors
/// A usage message.
pub fn parse_tokens(tokens: &[String]) -> Result<Option<AgentCommand>, String> {
    let Some((action, rest)) = tokens.split_first() else {
        return Ok(None);
    };
    if !COMPILER_ACTIONS.contains(&action.as_str()) {
        return Ok(None);
    }
    let mut positional: Vec<String> = Vec::new();
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let takes_value = [
        "--interface",
        "--runner",
        "-o",
        "--output",
        "--target",
        "--harw-src",
        "--format",
        "--kind",
        "--role",
        "--extends",
        "--dir",
        "--older-than",
        "--keep",
    ];
    let mut iter = rest.iter();
    while let Some(token) = iter.next() {
        if let Some((flag, value)) = token
            .split_once('=')
            .filter(|(flag, _)| flag.starts_with("--"))
        {
            flags.push((flag.to_owned(), Some(value.to_owned())));
        } else if takes_value.contains(&token.as_str()) {
            let value = iter
                .next()
                .ok_or_else(|| format!("{token} needs a value"))?;
            flags.push((token.clone(), Some(value.clone())));
        } else if token.starts_with('-') && token.len() > 1 {
            flags.push((token.clone(), None));
        } else {
            positional.push(token.clone());
        }
    }
    let flag = |name: &str| flags.iter().any(|(flag, _)| flag == name);
    let value = |names: &[&str]| {
        flags
            .iter()
            .rev()
            .find(|(flag, _)| names.contains(&flag.as_str()))
            .and_then(|(_, value)| value.clone())
    };
    let first = |what: &str| {
        positional
            .first()
            .cloned()
            .ok_or_else(|| format!("/agent {action} needs {what}"))
    };
    let command = match action.as_str() {
        "check" => AgentCommand::Check {
            targets: positional.clone(),
        },
        "build" => AgentCommand::Build(BuildArgs {
            target: first("an agent name or path")?,
            interfaces: value(&["--interface"])
                .map(|raw| parse_interfaces(&raw))
                .transpose()?,
            native: flag("--native"),
            artifact_only: flag("--artifact-only"),
            runner: value(&["--runner"]).map(PathBuf::from),
            output: value(&["-o", "--output"]).map(PathBuf::from),
            target_triple: value(&["--target"]),
            harw_src: value(&["--harw-src"]).map(PathBuf::from),
        }),
        "inspect" => AgentCommand::Inspect {
            target: first("a binary, artifact or name")?,
        },
        "graph" => AgentCommand::Graph {
            target: positional.first().cloned(),
            all: flag("--all"),
            format: match value(&["--format"]) {
                Some(raw) => GraphFormat::parse(&raw)
                    .ok_or_else(|| format!("unknown format `{raw}` (text, dot, mermaid, json)"))?,
                None => GraphFormat::Text,
            },
            kind: match value(&["--kind"]) {
                Some(raw) => GraphKind::parse(&raw).ok_or_else(|| {
                    format!("unknown kind `{raw}` (delegation, resolution, rights, all)")
                })?,
                None => GraphKind::All,
            },
        },
        "explain" => AgentCommand::Explain {
            target: first("an agent name or a diagnostic code")?,
            field: positional.get(1).cloned(),
        },
        "new" => AgentCommand::New {
            name: first("a name")?,
            role: match value(&["--role"]) {
                Some(raw) => ScaffoldRole::parse(&raw)
                    .ok_or_else(|| format!("unknown role `{raw}` (worker, child-orchestrator)"))?,
                None => ScaffoldRole::Worker,
            },
            extends: value(&["--extends"]),
            dir: value(&["--dir"]).map(PathBuf::from),
        },
        "fmt" => AgentCommand::Fmt {
            paths: positional.iter().map(PathBuf::from).collect(),
            check: flag("--check"),
        },
        "diff" => AgentCommand::Diff {
            left: first("two sides")?,
            right: positional
                .get(1)
                .cloned()
                .ok_or_else(|| "/agent diff needs two sides".to_owned())?,
        },
        "test" => AgentCommand::Test {
            target: positional.first().cloned(),
        },
        "run" => AgentCommand::Run {
            target: first("an artifact or name")?,
            prompt: (positional.len() > 1).then(|| positional[1..].join(" ")),
        },
        "versions" => AgentCommand::Versions {
            name: first("a name")?,
        },
        "clean" => AgentCommand::Clean(CleanArgs {
            all: flag("--all"),
            older_than_days: value(&["--older-than"])
                .map(|raw| {
                    raw.parse::<u64>()
                        .map_err(|_| format!("--older-than needs days, not `{raw}`"))
                })
                .transpose()?,
            keep: value(&["--keep"])
                .map(|raw| {
                    raw.parse::<usize>()
                        .map_err(|_| format!("--keep needs a number, not `{raw}`"))
                })
                .transpose()?,
            dry_run: flag("--dry-run"),
        }),
        "doctor" => AgentCommand::Doctor,
        _ => return Ok(None),
    };
    Ok(Some(command))
}

/// Parses `cli,mcp` into interfaces.
///
/// # Errors
/// A message naming the unknown interface.
pub fn parse_interfaces(raw: &str) -> Result<Vec<Interface>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            Interface::parse(part)
                .ok_or_else(|| format!("unknown interface `{part}` (cli, repl, mcp, http, tui)"))
        })
        .collect()
}

/// One installed agent for `harw agent list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledSummary {
    /// Binary name.
    pub name: String,
    /// Current version directory.
    pub current: Option<String>,
    /// Definition ID of the current version.
    pub definition_id: Option<String>,
    /// Source snapshot of the current version.
    pub source_snapshot: Option<String>,
    /// Built automatically (the active UIA).
    pub auto: bool,
}

/// Every agent installed in `~/.harw/bin`.
#[must_use]
pub fn installed_agents(env: &CompilerEnv) -> Vec<InstalledSummary> {
    let bin = BinDir::new(env.bin_dir());
    let auto_name = crate::uia::auto_build_state(env).map(|state| state.name);
    bin.names()
        .into_iter()
        .map(|name| {
            let current = bin.current(&name);
            InstalledSummary {
                auto: auto_name.as_deref() == Some(name.as_str()),
                current: current.as_ref().map(|version| version.dir_name.clone()),
                definition_id: current
                    .as_ref()
                    .map(|version| version.record.definition_id.clone()),
                source_snapshot: current
                    .as_ref()
                    .map(|version| version.record.source_snapshot.clone()),
                name,
            }
        })
        .collect()
}

/// The lowered snapshot of every definition name (front end only), for the
/// build state in `harw agent list`; names that do not lower are missing.
#[must_use]
pub fn definition_snapshots(
    env: &CompilerEnv,
    names: &[String],
) -> std::collections::BTreeMap<String, (String, String)> {
    let mut out = std::collections::BTreeMap::new();
    let Ok(compiler) = Compiler::new(env.clone(), CompilerOptions::default()) else {
        return out;
    };
    for name in names {
        let Some(entry) = compiler.sources().find(name) else {
            continue;
        };
        if let Ok(unit) = compiler.front_end(entry) {
            out.insert(name.clone(), (unit.ir.id.to_string(), unit.source_snapshot));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(raw: &str) -> Vec<String> {
        raw.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn test_parse_tokens_for_every_action() -> Result<(), String> {
        let parsed = parse_tokens(&tokens(
            "build ec --interface cli,mcp --native -o out --target x",
        ))?;
        let Some(AgentCommand::Build(args)) = parsed else {
            return Err(format!("expected build, got {parsed:?}"));
        };
        assert_eq!(args.target, "ec");
        assert_eq!(args.interfaces, Some(vec![Interface::Cli, Interface::Mcp]));
        assert!(args.native && !args.artifact_only);
        assert_eq!(args.output, Some(PathBuf::from("out")));
        assert_eq!(args.target_triple.as_deref(), Some("x"));

        assert!(
            matches!(parse_tokens(&tokens("check a b"))?, Some(AgentCommand::Check { targets }) if targets.len() == 2)
        );
        assert!(matches!(
            parse_tokens(&tokens("graph --all --format=dot --kind rights"))?,
            Some(AgentCommand::Graph {
                all: true,
                format: GraphFormat::Dot,
                kind: GraphKind::Rights,
                ..
            })
        ));
        assert!(matches!(
            parse_tokens(&tokens("explain HARW-PATCH-003"))?,
            Some(AgentCommand::Explain { field: None, .. })
        ));
        assert!(matches!(
            parse_tokens(&tokens("new x --role child-orchestrator"))?,
            Some(AgentCommand::New {
                role: ScaffoldRole::ChildOrchestrator,
                ..
            })
        ));
        assert!(matches!(
            parse_tokens(&tokens("fmt --check"))?,
            Some(AgentCommand::Fmt { check: true, .. })
        ));
        assert!(matches!(
            parse_tokens(&tokens("diff a b"))?,
            Some(AgentCommand::Diff { .. })
        ));
        assert!(matches!(
            parse_tokens(&tokens("test"))?,
            Some(AgentCommand::Test { target: None })
        ));
        assert!(
            matches!(parse_tokens(&tokens("run x hello world"))?, Some(AgentCommand::Run { prompt: Some(p), .. }) if p == "hello world")
        );
        assert!(matches!(
            parse_tokens(&tokens("versions ec"))?,
            Some(AgentCommand::Versions { .. })
        ));
        assert!(matches!(
            parse_tokens(&tokens("clean --dry-run --keep 2 --older-than 7"))?,
            Some(AgentCommand::Clean(CleanArgs {
                dry_run: true,
                keep: Some(2),
                older_than_days: Some(7),
                all: false
            }))
        ));
        assert!(matches!(
            parse_tokens(&tokens("doctor"))?,
            Some(AgentCommand::Doctor)
        ));
        assert!(matches!(
            parse_tokens(&tokens("inspect ./ec"))?,
            Some(AgentCommand::Inspect { .. })
        ));
        assert_eq!(
            parse_tokens(&tokens("list"))?,
            None,
            "not a compiler action"
        );
        assert_eq!(parse_tokens(&[])?, None);
        assert!(parse_tokens(&tokens("build")).is_err());
        assert!(parse_tokens(&tokens("build x --interface grpc")).is_err());
        Ok(())
    }

    struct NoProbe;
    impl RunnerProbe for NoProbe {
        fn capabilities(
            &self,
            runner: &Path,
        ) -> Result<crate::backend::runner::RunnerCapabilities, CompileError> {
            Err(CompileError::RunnerIncompatible {
                runner: runner.to_path_buf(),
                reason: "test".to_owned(),
            })
        }
    }

    #[test]
    fn test_run_reports_a_target_that_is_neither_a_file_nor_installed() {
        let mut progress = |_: &str| {};
        let mut ctx = CommandContext {
            env: CompilerEnv::isolated(PathBuf::from("/nonexistent"), PathBuf::from("/")),
            probe: &NoProbe,
            case_runner: &crate::testing::EchoStub,
            progress: &mut progress,
        };
        let output = run_command(
            &mut ctx,
            AgentCommand::Run {
                target: "x".to_owned(),
                prompt: None,
            },
        );
        assert_eq!(output.exit_code, 1);
        assert!(
            output
                .text
                .contains("neither a file nor an agent installed"),
            "{}",
            output.text
        );
    }

    #[test]
    fn test_run_reports_the_missing_runner_for_a_bare_artifact()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let artifact =
            harw_agent_artifact::ArtifactBuilder::new(&json!({"name": "demo"})).build()?;
        let artifact_path = dir.path().join("demo.harwa");
        std::fs::write(&artifact_path, artifact.to_bytes())?;
        let env = CompilerEnv::isolated(dir.path().join("home"), dir.path().to_path_buf());
        let mut progress = |_: &str| {};
        let mut ctx = CommandContext {
            env,
            probe: &NoProbe,
            case_runner: &crate::testing::EchoStub,
            progress: &mut progress,
        };
        let output = run_command(
            &mut ctx,
            AgentCommand::Run {
                target: artifact_path.display().to_string(),
                prompt: None,
            },
        );
        assert_eq!(output.exit_code, EXIT_RUNNER_UNAVAILABLE);
        assert_eq!(output.json["error"], "runner-not-found");
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn test_run_execs_a_bare_artifact_through_a_located_runner()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let artifact =
            harw_agent_artifact::ArtifactBuilder::new(&json!({"name": "demo"})).build()?;
        let artifact_path = dir.path().join("demo.harwa");
        std::fs::write(&artifact_path, artifact.to_bytes())?;
        let home = dir.path().join("home");
        let env = CompilerEnv::isolated(home.clone(), dir.path().to_path_buf());
        // A fake runner that exits 3 (approval denied) before reading the
        // artifact bytes appended after it — a real runner exits the same
        // way for a denied approval, and this exercises exit-code passthrough
        // without needing a real `harw-agent-runner` binary.
        let runner_path = env
            .home_runner_dir(&env.host_target)
            .join("harw-agent-runner");
        std::fs::create_dir_all(runner_path.parent().ok_or("parent")?)?;
        harw_agent_artifact::write_executable(&runner_path, b"#!/bin/sh\nexit 3\n")?;
        let mut progress = |_: &str| {};
        let mut ctx = CommandContext {
            env,
            probe: &NoProbe,
            case_runner: &crate::testing::EchoStub,
            progress: &mut progress,
        };
        let output = run_command(
            &mut ctx,
            AgentCommand::Run {
                target: artifact_path.display().to_string(),
                prompt: Some("hi".to_owned()),
            },
        );
        assert_eq!(output.exit_code, 3);
        Ok(())
    }
}
