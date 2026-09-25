//! Imports into a fresh, independent root space (#22: the native
//! personalized harw lives in `~/.<name>` and `<project>/.<name>`).
//!
//! Nothing here runs without the user's consent; the caller asks first.
//!
//! - [`import_providers_and_auth`]: provider configuration and credentials
//!   from `~/.harw` (`config.toml`, `auth.toml`, `providers/`).
//! - [`copy_project_config`]: a project's configuration from `.harw` to
//!   `.<name>` (`config.toml`, `agents/`, `skills/`, `context-programs/`),
//!   never state, sessions, plans or logs.
//! - [`ImportDecisions`]: the answers, remembered per project in the new
//!   root space, so the offer comes once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{HomeError, HomeResult};

/// Top-level files and directories [`import_providers_and_auth`] copies.
pub const PROVIDER_IMPORT_ENTRIES: &[&str] = &["config.toml", "auth.toml", "providers"];

/// Project entries [`copy_project_config`] copies (configuration only).
pub const PROJECT_CONFIG_ENTRIES: &[&str] =
    &["config.toml", "agents", "skills", "context-programs"];

/// File in the new root space that remembers the per-project answers.
pub const IMPORT_DECISIONS_FILE: &str = "project-imports.toml";

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> HomeError + '_ {
    move |source| HomeError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Copies a file or a directory tree (symlinks are skipped, never
/// followed). Returns the copied files.
fn copy_tree(from: &Path, to: &Path, copied: &mut Vec<PathBuf>) -> HomeResult<()> {
    let metadata = std::fs::symlink_metadata(from).map_err(io(from))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        std::fs::create_dir_all(to).map_err(io(to))?;
        let mut entries: Vec<PathBuf> = std::fs::read_dir(from)
            .map_err(io(from))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for entry in entries {
            if let Some(name) = entry.file_name() {
                copy_tree(&entry, &to.join(name), copied)?;
            }
        }
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(io(parent))?;
    }
    std::fs::copy(from, to).map_err(io(to))?;
    copied.push(to.to_path_buf());
    Ok(())
}

/// Copies the provider configuration and credentials of `source` (usually
/// `~/.harw`) into `target` (the new root space), overwriting the scaffolded
/// defaults. `auth.toml` keeps mode `0600`.
///
/// # Returns
/// The files written, sorted.
///
/// # Errors
/// [`HomeError::Io`] on a copy failure.
pub fn import_providers_and_auth(source: &Path, target: &Path) -> HomeResult<Vec<PathBuf>> {
    let mut copied = Vec::new();
    for entry in PROVIDER_IMPORT_ENTRIES {
        let from = source.join(entry);
        if from.exists() {
            copy_tree(&from, &target.join(entry), &mut copied)?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let auth = target.join("auth.toml");
        if auth.is_file() {
            std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o600))
                .map_err(io(&auth))?;
        }
    }
    copied.sort();
    Ok(copied)
}

/// Copies a project's configuration from `<project>/<from_dir>` to
/// `<project>/<to_dir>` (see [`PROJECT_CONFIG_ENTRIES`]).
///
/// # Errors
/// [`HomeError::Io`] on a copy failure.
pub fn copy_project_config(from: &Path, to: &Path) -> HomeResult<Vec<PathBuf>> {
    let mut copied = Vec::new();
    for entry in PROJECT_CONFIG_ENTRIES {
        let source = from.join(entry);
        if source.exists() {
            copy_tree(&source, &to.join(entry), &mut copied)?;
        }
    }
    copied.sort();
    Ok(copied)
}

/// Remembered answers to the project-config offer, keyed by project key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportDecisions {
    path: PathBuf,
    decisions: BTreeMap<String, bool>,
}

impl ImportDecisions {
    /// Loads `<home>/project-imports.toml` (missing or unreadable: empty).
    #[must_use]
    pub fn load(home: &Path) -> Self {
        let path = home.join(IMPORT_DECISIONS_FILE);
        let decisions = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str::<BTreeMap<String, bool>>(&text).ok())
            .unwrap_or_default();
        Self { path, decisions }
    }

    /// The remembered answer for `project_key`.
    #[must_use]
    pub fn get(&self, project_key: &str) -> Option<bool> {
        self.decisions.get(project_key).copied()
    }

    /// Records an answer and saves the file.
    ///
    /// # Errors
    /// [`HomeError::Io`] on a write failure.
    pub fn record(&mut self, project_key: &str, copied: bool) -> HomeResult<()> {
        self.decisions.insert(project_key.to_owned(), copied);
        let text = toml::to_string(&self.decisions).map_err(|error| HomeError::Io {
            path: self.path.clone(),
            source: std::io::Error::other(error.to_string()),
        })?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(io(parent))?;
        }
        std::fs::write(&self.path, text).map_err(io(&self.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn test_provider_import_copies_config_and_auth_only() -> TestResult {
        let source = tempfile::tempdir()?;
        let target = tempfile::tempdir()?;
        std::fs::write(
            source.path().join("config.toml"),
            "default_provider = \"x\"\n",
        )?;
        std::fs::write(source.path().join("auth.toml"), "[x]\nkey = \"env:X\"\n")?;
        std::fs::create_dir_all(source.path().join("providers"))?;
        std::fs::write(
            source.path().join("providers").join("x.toml"),
            "name = \"x\"\n",
        )?;
        std::fs::create_dir_all(source.path().join("sessions"))?;
        std::fs::write(source.path().join("sessions").join("s.json"), "{}")?;
        std::fs::write(target.path().join("config.toml"), "# scaffolded\n")?;

        let copied = import_providers_and_auth(source.path(), target.path())?;
        assert_eq!(copied.len(), 3);
        assert_eq!(
            std::fs::read_to_string(target.path().join("config.toml"))?,
            "default_provider = \"x\"\n",
            "the scaffolded default is replaced"
        );
        assert!(target.path().join("providers").join("x.toml").is_file());
        assert!(
            !target.path().join("sessions").exists(),
            "no state is copied"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(target.path().join("auth.toml"))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        Ok(())
    }

    #[test]
    fn test_project_copy_takes_config_not_state() -> TestResult {
        let project = tempfile::tempdir()?;
        let from = project.path().join(".harw");
        let to = project.path().join(".mia");
        std::fs::create_dir_all(from.join("agents").join("a"))?;
        std::fs::write(from.join("config.toml"), "x = 1\n")?;
        std::fs::write(
            from.join("agents").join("a").join("definition.toml"),
            "id = 1\n",
        )?;
        for state in ["state", "plans", "memories", "logs"] {
            std::fs::create_dir_all(from.join(state))?;
            std::fs::write(from.join(state).join("f"), "state")?;
        }
        std::fs::write(from.join("handoff.json"), "{}")?;
        let copied = copy_project_config(&from, &to)?;
        assert_eq!(copied.len(), 2);
        assert!(
            to.join("agents")
                .join("a")
                .join("definition.toml")
                .is_file()
        );
        for state in ["state", "plans", "memories", "logs", "handoff.json"] {
            assert!(!to.join(state).exists(), "{state} is not copied");
        }
        Ok(())
    }

    #[test]
    fn test_decisions_are_remembered() -> TestResult {
        let home = tempfile::tempdir()?;
        let mut decisions = ImportDecisions::load(home.path());
        assert_eq!(decisions.get("p1"), None);
        decisions.record("p1", false)?;
        decisions.record("p2", true)?;
        let again = ImportDecisions::load(home.path());
        assert_eq!(again.get("p1"), Some(false));
        assert_eq!(again.get("p2"), Some(true));
        Ok(())
    }
}
