//! The backends and the build orchestration.
//!
//! - **Backend A (default)**: the artifact is appended to a copy of the
//!   prebuilt `harw-agent-runner` ([`runner`] finds it and checks its
//!   capabilities). With `--artifact-only` only the `.harwa` file is made.
//! - **Backend B (`--native`)**: [`native`] generates a crate and runs cargo.
//!
//! Every build is installed as a version under `~/.harw/bin`
//! ([`crate::bin_dir`]); `-o` additionally copies the result. After a
//! native build the cache collector runs ([`crate::cache`]).

pub mod native;
pub mod runner;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use harw_agent_artifact::{EmbeddedArtifact, append_to_executable, write_executable};
use serde::Serialize;

use crate::bin_dir::{BUILD_RECORD_SCHEMA, BinDir, BuildRecord, InstalledVersion, RemovedVersion};
use crate::cache::{
    AgentCompilerSettings, GcPolicy, GcReport, collect_garbage, stale_runner_dirs, unix_now,
};
use crate::compiler::Compiled;
use crate::env::{CompilerEnv, HARW_VERSION};
use crate::error::CompileError;
use native::{NativeBackend, NativeFlavor, NativeOutput};
use runner::{RunnerCandidate, RunnerProbe, locate_runner};

/// What `harw agent build` was asked to do (interfaces are a compiler
/// option: they change the IR before the passes).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildOptions {
    /// Backend B.
    pub native: bool,
    /// Only the `.harwa` artifact, no runner.
    pub artifact_only: bool,
    /// `--runner`.
    pub runner: Option<PathBuf>,
    /// `-o`.
    pub output: Option<PathBuf>,
    /// `--target`.
    pub target: Option<String>,
    /// `--harw-src`.
    pub harw_src: Option<PathBuf>,
    /// Native UIA builds: the personalized harw's own home name
    /// (`~/.<name>`).
    pub home_name: Option<String>,
}

/// The backend a build used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    /// Artifact appended to the prebuilt runner.
    Artifact,
    /// Only the artifact file.
    ArtifactOnly,
    /// Generated crate plus cargo.
    Native,
}

impl BackendKind {
    /// Label (`artifact`, `artifact-only`, `native`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Artifact => "artifact",
            Self::ArtifactOnly => "artifact-only",
            Self::Native => "native",
        }
    }
}

/// The result of a build.
#[derive(Debug, Clone, Serialize)]
pub struct BuildReport {
    /// Binary name.
    pub name: String,
    /// Definition ID.
    pub definition_id: String,
    /// Definition version.
    pub version: String,
    /// Backend.
    pub backend: BackendKind,
    /// Target triple.
    pub target: String,
    /// Artifact digest (hex).
    pub artifact_digest: String,
    /// v7 snapshot (hex).
    pub snapshot: String,
    /// Interfaces.
    pub interfaces: Vec<String>,
    /// Runner features the agent needs.
    pub features: Vec<String>,
    /// The installed version.
    pub installed: InstalledVersion,
    /// The `-o` copy.
    pub output: Option<PathBuf>,
    /// The runner used (backend A).
    pub runner: Option<RunnerCandidate>,
    /// The native build (backend B).
    pub native: Option<NativeOutput>,
    /// Cache collection after a native build.
    pub gc: Option<GcReport>,
    /// Versions removed by `[agent_compiler] keep_versions`.
    pub pruned_versions: Vec<RemovedVersion>,
    /// Notes (runner version mismatch, …).
    pub notes: Vec<String>,
}

/// The runner features an agent needs: its providers, `core` and its
/// interfaces, sorted.
#[must_use]
pub fn required_features(compiled: &Compiled) -> BTreeSet<String> {
    let mut features = compiled.unit.features.clone();
    for interface in &compiled.unit.ir.binary.interfaces {
        features.insert(interface.as_str().to_owned());
    }
    features
}

/// Runs the chosen backend for a compiled agent and installs the result.
///
/// # Errors
/// Backend errors ([`CompileError::RunnerNotFound`],
/// [`CompileError::RunnerIncompatible`], [`CompileError::ToolchainMissing`],
/// [`CompileError::SourcesMissing`], [`CompileError::CargoFailed`]) and I/O.
pub fn build(
    env: &CompilerEnv,
    compiled: &Compiled,
    options: &BuildOptions,
    probe: &dyn RunnerProbe,
    progress: &mut dyn FnMut(&str),
) -> Result<BuildReport, CompileError> {
    let ir = &compiled.unit.ir;
    let target = options
        .target
        .clone()
        .unwrap_or_else(|| env.host_target.clone());
    let name = ir.binary.name.clone();
    let digest = compiled.artifact.digest().to_hex();
    let interfaces: Vec<String> = ir
        .binary
        .interfaces
        .iter()
        .map(|interface| interface.as_str().to_owned())
        .collect();
    let features = required_features(compiled);
    let backend = if options.artifact_only {
        BackendKind::ArtifactOnly
    } else if options.native {
        BackendKind::Native
    } else {
        BackendKind::Artifact
    };
    let now = time::OffsetDateTime::now_utc();
    let record = BuildRecord {
        schema: BUILD_RECORD_SCHEMA.to_owned(),
        name: name.clone(),
        definition_id: ir.id.to_string(),
        version: ir.version.0.to_string(),
        artifact_digest: digest.clone(),
        snapshot: ir
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.digest.clone())
            .unwrap_or_default(),
        source_snapshot: compiled.unit.source_snapshot.clone(),
        interfaces: interfaces.clone(),
        harw_version: HARW_VERSION.to_owned(),
        built_at: now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        built_at_unix: now.unix_timestamp(),
        sequence: 0,
        backend: backend.as_str().to_owned(),
        target: target.clone(),
        file: if backend == BackendKind::ArtifactOnly {
            format!("{name}.harwa")
        } else {
            name.clone()
        },
    };
    let bin = BinDir::new(env.bin_dir());
    let mut report = BuildReport {
        name: name.clone(),
        definition_id: record.definition_id.clone(),
        version: record.version.clone(),
        backend,
        target: target.clone(),
        artifact_digest: digest,
        snapshot: record.snapshot.clone(),
        interfaces,
        features: features.iter().cloned().collect(),
        installed: InstalledVersion {
            dir_name: String::new(),
            dir: PathBuf::new(),
            file: PathBuf::new(),
            record: record.clone(),
            current: false,
        },
        output: None,
        runner: None,
        native: None,
        gc: None,
        pruned_versions: Vec::new(),
        notes: Vec::new(),
    };

    let (bytes, executable) = match backend {
        BackendKind::ArtifactOnly => (compiled.artifact.to_bytes(), false),
        BackendKind::Artifact => {
            let runner = locate_runner(env, options.runner.as_deref(), &target)?;
            progress(&format!(
                "runner: {} ({:?})",
                runner.path.display(),
                runner.source
            ));
            let capabilities = probe.capabilities(&runner.path)?;
            let feature_list: Vec<String> = features.iter().cloned().collect();
            let problems = capabilities.problems(&report.interfaces, &feature_list);
            if !problems.is_empty() {
                return Err(CompileError::RunnerIncompatible {
                    runner: runner.path.clone(),
                    reason: problems.join("; "),
                });
            }
            report.notes.extend(capabilities.version_note());
            let runner_bytes = std::fs::read(&runner.path)
                .map_err(CompileError::io(format!("read {}", runner.path.display())))?;
            let binary = append_to_executable(&runner_bytes, &compiled.artifact);
            // The same check the runner does at startup.
            EmbeddedArtifact::from_executable_bytes(&binary)?;
            report.runner = Some(runner);
            (binary, true)
        }
        BackendKind::Native => {
            let native = NativeBackend {
                env,
                harw_src: options.harw_src.as_deref(),
                target: &target,
            };
            let flavor = if ir.role == harw_agent_dsl::roles::AgentRoleId::UserInterface {
                NativeFlavor::Harw
            } else {
                NativeFlavor::Runner
            };
            progress(&format!("native flavor: {flavor:?}"));
            if let Some(home) = options
                .home_name
                .as_deref()
                .filter(|_| flavor == NativeFlavor::Harw)
            {
                let path = harw_home::paths::named_home_dir(home)
                    .map_or_else(|_| format!("~/.{home}"), |path| path.display().to_string());
                progress(&format!(
                    "the personalized harw will use its own home {path}"
                ));
                report
                    .notes
                    .push(format!("home of the personalized harw: {path}"));
            }
            let output = native.build(
                flavor,
                options.home_name.as_deref(),
                &name,
                &record.definition_id,
                &record.version,
                &compiled.artifact,
                &features,
                progress,
            )?;
            let bytes = std::fs::read(&output.binary).map_err(CompileError::io(format!(
                "read {}",
                output.binary.display()
            )))?;
            report.native = Some(output);
            (bytes, true)
        }
    };

    std::fs::create_dir_all(bin.root())
        .map_err(CompileError::io(format!("create {}", bin.root().display())))?;
    report.installed = bin.install(&record, &bytes, executable, executable)?;
    progress(&format!("installed {}", report.installed.file.display()));

    if let Some(output) = &options.output {
        let path = output_path(env, output, &record.file);
        if executable {
            write_executable(&path, &bytes)?;
        } else {
            std::fs::write(&path, &bytes)
                .map_err(CompileError::io(format!("write {}", path.display())))?;
        }
        progress(&format!("copied to {}", path.display()));
        report.output = Some(path);
    }

    if let Some(native) = &report.native {
        let settings = AgentCompilerSettings::load(env);
        let gc = collect_garbage(
            &env.build_cache_dir(),
            &GcPolicy::automatic(settings.max_bytes()),
            std::slice::from_ref(&native.crate_dir),
            std::slice::from_ref(&native.binary),
            unix_now(),
        )?;
        for stale in stale_runner_dirs(env) {
            let _ = std::fs::remove_dir_all(&stale);
            report.notes.push(format!(
                "removed runner of another harw version: {}",
                stale.display()
            ));
        }
        if let Some(keep) = settings.keep_versions {
            report.pruned_versions = bin.prune_versions(&name, keep, false)?;
        }
        report.gc = Some(gc);
    }
    Ok(report)
}

/// `-o`: a directory gets the file name appended; relative paths resolve
/// against the working directory.
#[must_use]
pub fn output_path(env: &CompilerEnv, output: &Path, file_name: &str) -> PathBuf {
    let path = env.resolve(output);
    if path.is_dir() {
        path.join(file_name)
    } else {
        path
    }
}
