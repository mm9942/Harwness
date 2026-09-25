//! `harw-agent-compiler`: from an agent definition to a compiled agent
//! (#22, ADR 0001).
//!
//! # Pipeline
//! 1. **Discovery** ([`discovery`]): built-in, bundled and layer definitions;
//!    a name or a path on the command line.
//! 2. **Front end** (`harw-agent-dsl`): parse, resolve, `lower_v2` into an
//!    `AgentIr` v2.
//! 3. **Passes** ([`passes`]): `ValidateRoles`, `RightsCheck`,
//!    `ResolveSkills`, `ReachableTools`, `PruneUnusedTools`,
//!    `ResolveModels`, `ChildClosure`. The v7 snapshot is recomputed after
//!    the last pass.
//! 4. **Artifact** ([`artifact_out`]): the IR (without trace) as header, plus
//!    instructions, skills, bundle files and nested child artifacts.
//! 5. **Backend** ([`backend`]): the artifact appended to the prebuilt
//!    `harw-agent-runner` (default), only the artifact (`--artifact-only`),
//!    or a generated crate built with cargo (`--native`; for a
//!    user-interface agent a complete, personalized harw).
//! 6. **Install** ([`bin_dir`]): every build is a version under
//!    `~/.harw/bin/.versions/<name>/`, `~/.harw/bin/<name>` points at the
//!    current one; `-o` copies it additionally.
//!
//! The compiler is opt-in: nothing is built unless someone runs `harw agent
//! build`, except the automatic artifact build of the active UIA ([`uia`]).
//! Built-in roles stay embedded in harw; building one only makes a copy.
//!
//! # Command surface
//! [`commands`] holds every `harw agent` subcommand of the compiler (check,
//! build, inspect, graph, explain, new, fmt, diff, test, run, versions,
//! use, clean, doctor) as data plus [`commands::run_command`]; the CLI and
//! the `/agent` op both call it.
//!
//! # Reproducibility
//! The same definition, sources and compiler version give the same artifact
//! bytes: the header is canonical JSON without the trace, payloads are
//! sorted, nothing records time, host or absolute paths.

#![forbid(unsafe_code)]

pub mod artifact_out;
pub mod backend;
pub mod bin_dir;
pub mod cache;
pub mod codes;
pub mod commands;
pub mod compiler;
pub mod diff;
pub mod discovery;
pub mod doctor;
pub mod env;
pub mod error;
pub mod explain;
pub mod fmt;
pub mod graph;
pub mod inspect;
pub mod passes;
pub mod render;
pub mod rights;
pub mod scaffold;
pub mod testing;
pub mod uia;
pub mod unit;

pub use backend::runner::{ProcessProbe, RunnerCapabilities, RunnerProbe};
pub use backend::{BuildOptions, BuildReport, build};
pub use commands::{AgentCommand, CommandContext, CommandOutput, parse_tokens, run_command};
pub use compiler::{Compiled, Compiler, CompilerOptions};
pub use discovery::AgentInput;
pub use env::{CompilerEnv, HARW_VERSION, InstallRecord};
pub use error::CompileError;
pub use unit::CompileUnit;
