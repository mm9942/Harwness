//! The personalized harw (#22): a native build of a user-interface agent is
//! a complete harw with that agent baked in as its fixed root.
//!
//! # What this module does
//! [`run_with_embedded_uia`] is the entry of such a build (the generated
//! `main.rs` calls it with the embedded artifact and the home name):
//! 1. verifies the artifact (hashes, header) and that it is a
//!    `user-interface` agent;
//! 2. with [`HomeChoice::Named`], switches the whole process to its own,
//!    independent home `~/.<name>` and project-local directory `.<name>`
//!    ([`harw_home::set_named_home`]) before any path is resolved
//!    (`HARW_HOME` still wins);
//! 3. on the first start of that home: scaffolds it and offers to import
//!    provider configuration and credentials from `~/.harw` (only on
//!    consent, never silently);
//! 4. on the first use in a project that has a `.harw` but no `.<name>`:
//!    offers once to copy the project configuration (never state);
//! 5. stores the agent ([`embedded_uia`]) and runs the normal harw
//!    ([`crate::main_entry`]).
//!
//! # Not yet wired (wave 3)
//! The runtime still selects the root through `active_uia_definition`. Wave
//! 3 makes the root selection (`harw-runtime`, `resolve_active_uia`) prefer
//! [`embedded_uia`] when it is set. Until then the embedded agent is
//! verified and available, and the independent home is fully in effect.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::OnceLock;

use harw_agent_artifact::Artifact;
use harw_agent_dsl::ir_v2::AgentIr;
use harw_agent_dsl::roles::AgentRoleId;

/// Which home a personalized harw uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeChoice {
    /// The normal `~/.harw` (or `HARW_HOME`).
    Default,
    /// Its own `~/.<name>` and `<project>/.<name>`.
    Named(&'static str),
}

/// The embedded user-interface agent.
#[derive(Debug, Clone)]
pub struct EmbeddedUia {
    /// The verified artifact.
    pub artifact: Artifact,
    /// Its IR.
    pub ir: AgentIr,
}

static EMBEDDED: OnceLock<EmbeddedUia> = OnceLock::new();

/// The embedded agent of a personalized harw, if this process is one.
#[must_use]
pub fn embedded_uia() -> Option<&'static EmbeddedUia> {
    EMBEDDED.get()
}

/// Verifies an embedded artifact and reads its IR.
///
/// # Errors
/// A message if the bytes are no valid artifact or no user-interface agent.
pub fn verify_embedded(bytes: &[u8]) -> Result<EmbeddedUia, String> {
    let artifact =
        Artifact::from_bytes(bytes).map_err(|error| format!("embedded agent artifact: {error}"))?;
    let bundle = harw_agent_artifact::Bundle::from_artifact(&artifact)
        .map_err(|error| format!("embedded agent artifact layout: {error}"))?;
    let ir: AgentIr = serde_json::from_value(bundle.header.ir)
        .map_err(|error| format!("embedded agent header: {error}"))?;
    if ir.role != AgentRoleId::UserInterface {
        return Err(format!(
            "the embedded agent {} is no user-interface agent",
            ir.id
        ));
    }
    Ok(EmbeddedUia { artifact, ir })
}

/// Entry of a personalized harw (see module docs).
#[must_use]
pub fn run_with_embedded_uia(artifact: &'static [u8], home: HomeChoice) -> ExitCode {
    let embedded = match verify_embedded(artifact) {
        Ok(embedded) => embedded,
        Err(message) => {
            eprintln!("harw: {message}");
            return ExitCode::from(2);
        }
    };
    if let HomeChoice::Named(name) = home {
        let path = match harw_home::set_named_home(name) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("harw: own home ~/.{name}: {error}");
                return ExitCode::from(2);
            }
        };
        let interactive = std::io::stdin().is_terminal();
        first_start(&path, interactive, &mut stdin_answer);
        if let Ok(cwd) = std::env::current_dir() {
            first_project_use(&path, &cwd, interactive, &mut stdin_answer);
        }
    }
    let _ = EMBEDDED.set(embedded);
    crate::main_entry()
}

/// Asks a yes/no question on the terminal (default no).
fn stdin_answer(question: &str) -> bool {
    eprint!("{question} [j/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "j" | "ja" | "y" | "yes")
}

/// First start of the own home: scaffold it and offer the provider import.
///
/// Returns the imported files (empty without consent).
pub fn first_start(home: &Path, interactive: bool, ask: &mut dyn FnMut(&str) -> bool) -> Vec<std::path::PathBuf> {
    if home.exists() {
        return Vec::new();
    }
    if let Err(error) = harw_home::ensure_home(home) {
        eprintln!("harw: {}: {error}", home.display());
        return Vec::new();
    }
    let Ok(standard) = harw_home::paths::named_home_dir("harw") else {
        return Vec::new();
    };
    if !interactive || !standard.is_dir() || standard == home {
        return Vec::new();
    }
    if !ask(&format!(
        "Provider und Zugangsdaten aus {} übernehmen?",
        standard.display()
    )) {
        return Vec::new();
    }
    match harw_home::import::import_providers_and_auth(&standard, home) {
        Ok(copied) => copied,
        Err(error) => {
            eprintln!("harw: import from {}: {error}", standard.display());
            Vec::new()
        }
    }
}

/// First use in a project with `.harw` but without the own project dir:
/// offer once to copy the project configuration. Returns the copied files.
pub fn first_project_use(
    home: &Path,
    cwd: &Path,
    interactive: bool,
    ask: &mut dyn FnMut(&str) -> bool,
) -> Vec<std::path::PathBuf> {
    let Ok(project) = harw_home::project::discover_project(cwd, &[]) else {
        return Vec::new();
    };
    let own = project.root.join(harw_home::project_dir_name());
    let standard = project.root.join(harw_home::paths::HOME_DIR_NAME);
    if own == standard || own.exists() || !standard.is_dir() || !interactive {
        return Vec::new();
    }
    let key = harw_home::project::project_key(&project.root);
    let mut decisions = harw_home::import::ImportDecisions::load(home);
    if decisions.get(&key).is_some() {
        return Vec::new();
    }
    let consent = ask(&format!(
        "Projektkonfiguration aus {} nach {} übernehmen (nur Konfiguration, kein Zustand)?",
        standard.display(),
        own.display()
    ));
    let copied = if consent {
        harw_home::import::copy_project_config(&standard, &own).unwrap_or_else(|error| {
            eprintln!("harw: copy project configuration: {error}");
            Vec::new()
        })
    } else {
        Vec::new()
    };
    let _ = decisions.record(&key, consent);
    copied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_rejects_garbage_and_non_uia_agents() {
        assert!(verify_embedded(b"not an artifact").is_err());
        let header = serde_json::json!({"schema": "harwness.agent-ir/v2"});
        let artifact = harw_agent_artifact::ArtifactBuilder::new(&header).build();
        let Ok(artifact) = artifact else {
            return;
        };
        assert!(verify_embedded(&artifact.to_bytes()).is_err(), "no AgentIr header");
    }

    #[test]
    fn test_first_start_imports_only_on_consent() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let home = root.path().join(".mia");
        let mut asked = 0;
        let copied = first_start(&home, false, &mut |_| {
            asked += 1;
            true
        });
        assert!(copied.is_empty(), "never without a terminal");
        assert_eq!(asked, 0);
        assert!(home.is_dir(), "the own home is scaffolded");
        Ok(())
    }

    #[test]
    fn test_project_offer_is_remembered() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let project = root.path().join("project");
        std::fs::create_dir_all(project.join(".git"))?;
        std::fs::create_dir_all(project.join(".harw"))?;
        std::fs::write(project.join(".harw").join("config.toml"), "x = 1\n")?;
        // Without the override the project dir is `.harw` itself: nothing to offer.
        let mut asked = 0;
        let copied = first_project_use(&home, &project, true, &mut |_| {
            asked += 1;
            false
        });
        assert!(copied.is_empty());
        assert_eq!(asked, 0, "the standard harw is never asked");
        Ok(())
    }
}
