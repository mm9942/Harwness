//! The user-interface agent (UIA): name derivation, its bundle files, the
//! native "personalized harw" home, and the automatic artifact build of the
//! active UIA.
//!
//! # Automatic build (`auto_build_uia`)
//! The one agent harw builds on its own is the active UIA
//! (`active_uia_definition`), and only with the artifact backend:
//! - disabled with `[agent_compiler] auto_build_uia = false`;
//! - skipped silently without an active UIA or without an installed runner;
//! - no rebuild while the artifact digest, the harw version and the runner
//!   (size and modification time) are unchanged;
//! - the result goes to `~/.harw/bin/<name>` like any build (versioned),
//!   `<name>` = `[binary].name`, else `harw-uia-<specialization>`;
//!   interfaces = `[binary].interfaces`, else `tui`, `repl`, `cli`.
//!
//! The state of the last attempt is `~/.harw/bin/.auto-build-uia.json`;
//! [`take_auto_build_notice`] returns a failure once, for a one-time notice.

use std::path::{Path, PathBuf};

use harw_agent_dsl::diagnostics::SourceFile;
use harw_agent_dsl::ir_v2::Interface;
use harw_agent_dsl::roles::AgentRoleId;
use serde::{Deserialize, Serialize};

use crate::backend::runner::{RunnerProbe, locate_runner};
use crate::backend::{BuildOptions, build};
use crate::bin_dir::BinDir;
use crate::cache::AgentCompilerSettings;
use crate::compiler::{Compiler, CompilerOptions};
use crate::discovery::{DefinitionEntry, SourceSet};
use crate::env::{CompilerEnv, HARW_VERSION};
use crate::error::CompileError;
use crate::unit::CompileUnit;

/// Default interfaces of an automatically built UIA.
pub const UIA_DEFAULT_INTERFACES: &[Interface] = &[Interface::Tui, Interface::Repl, Interface::Cli];

/// State file of the automatic build, in the bin directory.
pub const AUTO_BUILD_STATE_FILE: &str = ".auto-build-uia.json";

/// Largest bundle file embedded with a UIA.
pub const MAX_BUNDLE_FILE_BYTES: u64 = 1024 * 1024;

/// What the definition's own `[binary]` table states (not inherited values).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExplicitBinary {
    /// `[binary].name`.
    pub name: Option<String>,
    /// `[binary].interfaces`.
    pub interfaces: Option<Vec<Interface>>,
    /// `[binary].default_interface`.
    pub default_interface: Option<Interface>,
}

/// Reads the explicit `[binary]` keys of a definition file.
#[must_use]
pub fn explicit_binary(file: Option<&SourceFile>) -> ExplicitBinary {
    let Some(table) = file.and_then(|file| toml::from_str::<toml::Table>(&file.text).ok()) else {
        return ExplicitBinary::default();
    };
    let Some(binary) = table.get("binary").and_then(toml::Value::as_table) else {
        return ExplicitBinary::default();
    };
    ExplicitBinary {
        name: binary
            .get("name")
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
        interfaces: binary
            .get("interfaces")
            .and_then(toml::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .filter_map(Interface::parse)
                    .collect()
            }),
        default_interface: binary
            .get("default_interface")
            .and_then(toml::Value::as_str)
            .and_then(Interface::parse),
    }
}

/// Makes a string a valid binary name (`[a-z0-9][a-z0-9._-]{0,63}`).
#[must_use]
pub fn sanitize_binary_name(raw: &str) -> String {
    let mapped: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed: String = mapped
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .trim_end_matches('-')
        .chars()
        .take(64)
        .collect();
    if trimmed.is_empty() {
        "agent".to_owned()
    } else {
        trimmed
    }
}

/// Binary name of the automatically built UIA: `[binary].name`, else
/// `harw-uia-<specialization>`.
#[must_use]
pub fn auto_binary_name(explicit_name: Option<&str>, specialization: &str) -> String {
    match explicit_name {
        Some(name) => sanitize_binary_name(name),
        None => sanitize_binary_name(&format!("harw-uia-{specialization}")),
    }
}

/// Binary name of a native UIA build (a personalized harw):
/// `[binary].name`, else `harw-<definition name>`.
#[must_use]
pub fn native_binary_name(explicit_name: Option<&str>, definition_name: &str) -> String {
    match explicit_name {
        Some(name) => sanitize_binary_name(name),
        None => sanitize_binary_name(&format!("harw-{definition_name}")),
    }
}

/// Home name of a native UIA build: `[binary].name`, else the
/// specialization, sanitized (`~/.<name>`, `<project>/.<name>`).
#[must_use]
pub fn native_home_name(explicit_name: Option<&str>, specialization: &str) -> String {
    harw_home::paths::sanitize_home_name(explicit_name.unwrap_or(specialization))
}

/// Embeds the Markdown bundle files next to a UIA definition
/// (`identity.md`, `Personality.md`, `USER.md`, …) as knowledge payloads
/// (`knowledge/<file>`), except the instruction file itself. A change to
/// any of them changes the artifact digest.
pub fn embed_uia_bundle(unit: &mut CompileUnit) {
    if unit.ir.role != AgentRoleId::UserInterface {
        return;
    }
    let Some(dir) = unit.dir.clone() else {
        return;
    };
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return;
    };
    let instructions = unit.ir.instructions.source.clone();
    let mut files: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .filter(|path| {
            std::fs::symlink_metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_BUNDLE_FILE_BYTES)
        })
        .collect();
    files.sort();
    for path in files {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if instructions.as_deref() == Some(name) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            unit.put_file("knowledge", format!("knowledge/{name}"), bytes);
        }
    }
}

/// The recorded state of the last automatic build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoBuildState {
    /// The UIA definition ID.
    pub uia: String,
    /// Binary name.
    pub name: String,
    /// `built` or `failed`.
    pub status: String,
    /// Artifact digest of the last successful build.
    pub artifact_digest: Option<String>,
    /// harw version of the attempt.
    pub harw_version: String,
    /// Runner fingerprint (size and mtime).
    pub runner: String,
    /// Failure message.
    pub message: Option<String>,
    /// Time of the attempt (Unix seconds).
    pub at_unix: u64,
    /// Whether a failure has been shown as a notice.
    pub notified: bool,
}

/// The outcome of [`auto_build_uia`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum AutoBuildOutcome {
    /// `[agent_compiler] auto_build_uia = false`.
    Disabled,
    /// No `active_uia_definition`.
    NoUia,
    /// No runner installed yet (silent).
    NoRunner,
    /// Nothing changed since the last build.
    Unchanged {
        /// Binary name.
        name: String,
    },
    /// A new version was built and made current.
    Built {
        /// Binary name.
        name: String,
        /// Installed file.
        path: PathBuf,
        /// Artifact digest.
        digest: String,
    },
    /// The build failed (logged, shown once).
    Failed {
        /// Why.
        message: String,
    },
}

fn state_path(env: &CompilerEnv) -> PathBuf {
    env.bin_dir().join(AUTO_BUILD_STATE_FILE)
}

/// The recorded state, if any.
#[must_use]
pub fn auto_build_state(env: &CompilerEnv) -> Option<AutoBuildState> {
    let bytes = std::fs::read(state_path(env)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_state(env: &CompilerEnv, state: &AutoBuildState) {
    let path = state_path(env);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(state) {
        let _ = std::fs::write(path, bytes);
    }
}

/// Returns a failed auto-build's message once (then marks it shown).
#[must_use]
pub fn take_auto_build_notice(env: &CompilerEnv) -> Option<String> {
    let mut state = auto_build_state(env)?;
    if state.status != "failed" || state.notified {
        return None;
    }
    state.notified = true;
    save_state(env, &state);
    state.message.map(|message| {
        format!(
            "automatic build of the UIA `{}` failed: {message}",
            state.name
        )
    })
}

/// Size and modification time of a runner (a cheap change detector).
#[must_use]
pub fn runner_fingerprint(path: &Path) -> String {
    let Ok(metadata) = std::fs::metadata(path) else {
        return String::new();
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    format!("{}-{modified}", metadata.len())
}

/// Resolves the active UIA's definition entry.
fn find_uia(sources: &SourceSet, uia: &str) -> Option<DefinitionEntry> {
    sources
        .find_id(uia)
        .or_else(|| sources.find(uia))
        .filter(|entry| entry.role == AgentRoleId::UserInterface)
        .cloned()
}

/// Builds the active UIA if needed (see module docs). Never panics and
/// never returns an error: every problem is an outcome.
#[must_use]
pub fn auto_build_uia(env: &CompilerEnv, probe: &dyn RunnerProbe) -> AutoBuildOutcome {
    let settings = AgentCompilerSettings::load(env);
    if !settings.auto_build_uia() {
        return AutoBuildOutcome::Disabled;
    }
    let Some(uia) = settings.active_uia_definition.clone() else {
        return AutoBuildOutcome::NoUia;
    };
    let Ok(runner) = locate_runner(env, None, &env.host_target) else {
        return AutoBuildOutcome::NoRunner;
    };
    let fingerprint = runner_fingerprint(&runner.path);
    let now = crate::cache::unix_now();
    let fail = |name: &str, message: String| {
        save_state(
            env,
            &AutoBuildState {
                uia: uia.clone(),
                name: name.to_owned(),
                status: "failed".to_owned(),
                artifact_digest: None,
                harw_version: HARW_VERSION.to_owned(),
                runner: fingerprint.clone(),
                message: Some(message.clone()),
                at_unix: now,
                notified: false,
            },
        );
        AutoBuildOutcome::Failed { message }
    };
    let sources = match SourceSet::discover(&env.layers) {
        Ok(sources) => sources,
        Err(error) => return fail("", error.to_string()),
    };
    let Some(entry) = find_uia(&sources, &uia) else {
        return fail(
            "",
            format!("the active UIA `{uia}` is no user-interface definition in any layer"),
        );
    };
    let explicit = explicit_binary(sources.file(&entry.label));
    let name = auto_binary_name(explicit.name.as_deref(), &entry.specialization);
    let options = CompilerOptions {
        interfaces: Some(
            explicit
                .interfaces
                .clone()
                .unwrap_or_else(|| UIA_DEFAULT_INTERFACES.to_vec()),
        ),
        default_interface: explicit
            .default_interface
            .or_else(|| explicit.interfaces.is_none().then_some(Interface::Tui)),
        binary_name: Some(name.clone()),
        ..CompilerOptions::default()
    };
    let compiler = match Compiler::new(env.clone(), options) {
        Ok(compiler) => compiler,
        Err(error) => return fail(&name, error.to_string()),
    };
    let compiled = match compiler.compile_entry(&entry) {
        Ok(compiled) => compiled,
        Err(CompileError::Diagnostics(diagnostics)) => {
            return fail(&name, diagnostics.to_string());
        }
        Err(error) => return fail(&name, error.to_string()),
    };
    let digest = compiled.artifact.digest().to_hex();
    let unchanged = auto_build_state(env).is_some_and(|state| {
        state.status == "built"
            && state.uia == uia
            && state.name == name
            && state.artifact_digest.as_deref() == Some(digest.as_str())
            && state.harw_version == HARW_VERSION
            && state.runner == fingerprint
    }) && BinDir::new(env.bin_dir()).current(&name).is_some();
    if unchanged {
        return AutoBuildOutcome::Unchanged { name };
    }
    let options = BuildOptions {
        runner: Some(runner.path.clone()),
        ..BuildOptions::default()
    };
    match build(env, &compiled, &options, probe, &mut |_| {}) {
        Ok(report) => {
            save_state(
                env,
                &AutoBuildState {
                    uia: uia.clone(),
                    name: name.clone(),
                    status: "built".to_owned(),
                    artifact_digest: Some(digest.clone()),
                    harw_version: HARW_VERSION.to_owned(),
                    runner: fingerprint.clone(),
                    message: None,
                    at_unix: now,
                    notified: false,
                },
            );
            AutoBuildOutcome::Built {
                name,
                path: report.installed.file,
                digest,
            }
        }
        Err(error) => fail(&name, error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::runner::{CAPABILITIES_SCHEMA, RunnerCapabilities};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct FullRunner;
    impl RunnerProbe for FullRunner {
        fn capabilities(&self, _runner: &Path) -> Result<RunnerCapabilities, CompileError> {
            Ok(RunnerCapabilities {
                schema: CAPABILITIES_SCHEMA.to_owned(),
                runner_version: HARW_VERSION.to_owned(),
                target: crate::env::host_target(),
                artifact_formats: vec![harw_agent_artifact::FORMAT_VERSION],
                ir_schema: harw_agent_dsl::AGENT_IR_SCHEMA.to_owned(),
                interfaces: Interface::ALL
                    .iter()
                    .map(|i| i.as_str().to_owned())
                    .collect(),
                features: harw_registry_defaults::capability_catalog::PROVIDER_FEATURES
                    .iter()
                    .map(|feature| (*feature).to_owned())
                    .chain(Interface::ALL.iter().map(|i| i.as_str().to_owned()))
                    .collect(),
                child_protocol: None,
            })
        }
    }

    const UIA: &str = "schema = \"harwness.agent/v1\"\nid = \"user.agent.mia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\nname = \"Mia\"\n";

    fn setup(config: &str) -> Result<(tempfile::TempDir, CompilerEnv), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let agent = home.join("agents").join("mia");
        std::fs::create_dir_all(&agent)?;
        std::fs::write(agent.join("definition.toml"), UIA)?;
        std::fs::write(agent.join("identity.md"), "I am Mia.\n")?;
        std::fs::write(home.join("config.toml"), config)?;
        let env = CompilerEnv::isolated(home, root.path().to_path_buf());
        Ok((root, env))
    }

    fn install_runner(env: &CompilerEnv) -> Result<(), Box<dyn std::error::Error>> {
        let path = env
            .home_runner_dir(&env.host_target)
            .join(crate::env::RUNNER_BINARY);
        std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
        std::fs::write(path, b"fake runner")?;
        Ok(())
    }

    #[test]
    fn test_disabled_and_missing_uia_and_missing_runner() -> TestResult {
        let (_root, env) = setup(
            "active_uia_definition = \"user.agent.mia@1\"\n[agent_compiler]\nauto_build_uia = false\n",
        )?;
        install_runner(&env)?;
        assert_eq!(
            auto_build_uia(&env, &FullRunner),
            AutoBuildOutcome::Disabled
        );

        let (_root, env) = setup("")?;
        install_runner(&env)?;
        assert_eq!(auto_build_uia(&env, &FullRunner), AutoBuildOutcome::NoUia);

        let (_root, env) = setup("active_uia_definition = \"user.agent.mia@1\"\n")?;
        assert_eq!(
            auto_build_uia(&env, &FullRunner),
            AutoBuildOutcome::NoRunner
        );
        assert!(auto_build_state(&env).is_none(), "a skip leaves no state");
        Ok(())
    }

    #[test]
    fn test_builds_once_then_only_on_change() -> TestResult {
        let (_root, env) = setup("active_uia_definition = \"user.agent.mia@1\"\n")?;
        install_runner(&env)?;
        let AutoBuildOutcome::Built { name, path, digest } = auto_build_uia(&env, &FullRunner)
        else {
            return Err("first run builds".into());
        };
        assert_eq!(name, "harw-uia-terminal-ui");
        assert!(path.is_file());
        assert!(env.bin_dir().join(&name).exists(), "current link");
        assert_eq!(
            auto_build_uia(&env, &FullRunner),
            AutoBuildOutcome::Unchanged { name: name.clone() }
        );
        // A bundle file changes: the digest changes, it rebuilds.
        std::fs::write(
            env.home.join("agents").join("mia").join("identity.md"),
            "I am Mia, now with more detail.\n",
        )?;
        let AutoBuildOutcome::Built { digest: second, .. } = auto_build_uia(&env, &FullRunner)
        else {
            return Err("a changed bundle rebuilds".into());
        };
        assert_ne!(digest, second);
        assert_eq!(BinDir::new(env.bin_dir()).versions(&name)?.len(), 2);
        Ok(())
    }

    #[test]
    fn test_name_derivation() {
        assert_eq!(
            auto_binary_name(None, "terminal-ui"),
            "harw-uia-terminal-ui"
        );
        assert_eq!(auto_binary_name(Some("Mia Bot"), "x"), "mia-bot");
        assert_eq!(native_binary_name(None, "mia"), "harw-mia");
        assert_eq!(native_binary_name(Some("my-harw"), "mia"), "my-harw");
        assert_eq!(native_home_name(None, "terminal-ui"), "terminal-ui");
        assert_eq!(native_home_name(Some("Mia!"), "x"), "mia");
        assert_eq!(sanitize_binary_name("--Weird Name--"), "weird-name");
        assert_eq!(sanitize_binary_name("???"), "agent");
    }

    #[test]
    fn test_explicit_binary_reads_only_own_keys() {
        let file = SourceFile::new(
            harw_agent_dsl::layers::DefinitionLayer::UserGlobal,
            "x/definition.toml",
            "[binary]\nname = \"mia\"\ninterfaces = [\"cli\", \"mcp\"]\n",
        );
        let explicit = explicit_binary(Some(&file));
        assert_eq!(explicit.name.as_deref(), Some("mia"));
        assert_eq!(
            explicit.interfaces,
            Some(vec![Interface::Cli, Interface::Mcp])
        );
        assert_eq!(explicit.default_interface, None);
        assert_eq!(explicit_binary(None), ExplicitBinary::default());
    }
}
