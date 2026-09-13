use crate::error::{ConfigError, ConfigResult};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// Lädt den System-Prompt für einen Agenten aus seinem Dir.
pub fn load_system_prompt(agent_dir: &Path, system_file: Option<&str>) -> ConfigResult<String> {
    let path = configured_file_path(agent_dir, system_file.unwrap_or("system.md"), "system_file")?;
    read_optional_file(&path)
}

/// Lädt Skill-Instructions.
pub fn load_skill_instructions(
    skill_dir: &Path,
    instructions_file: Option<&str>,
) -> ConfigResult<String> {
    let path = configured_file_path(
        skill_dir,
        instructions_file.unwrap_or("instructions.md"),
        "instructions_file",
    )?;
    read_optional_file(&path)
}

fn configured_file_path(
    base_dir: &Path,
    configured_file: &str,
    field: &str,
) -> ConfigResult<PathBuf> {
    let configured_path = Path::new(configured_file);
    if configured_path.is_absolute()
        || configured_path
            .components()
            .any(|component| matches!(component, Component::Prefix(_) | Component::ParentDir))
    {
        return Err(ConfigError::Invalid(format!(
            "{field} must stay within its base directory and contain no absolute or parent-traversal components: {configured_file:?}"
        )));
    }

    let canonical_base = base_dir
        .canonicalize()
        .map_err(|error| ConfigError::ReadFailed {
            path: base_dir.display().to_string(),
            reason: error.to_string(),
        })?;
    let candidate = canonical_base.join(configured_path);

    resolve_contained_path(&canonical_base, &candidate, field, configured_file)
}

/// Canonicalizes the configured file when it exists, otherwise its nearest
/// existing ancestor. This resolves symlinks before the containment check
/// while preserving the optional-file behavior for a missing final component.
fn resolve_contained_path(
    canonical_base: &Path,
    candidate: &Path,
    field: &str,
    configured_file: &str,
) -> ConfigResult<PathBuf> {
    match candidate.canonicalize() {
        Ok(canonical_file) => {
            ensure_within_base(canonical_base, &canonical_file, field, configured_file)?;
            Ok(canonical_file)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let canonical_ancestor = canonicalize_existing_ancestor(candidate)?;
            ensure_within_base(canonical_base, &canonical_ancestor, field, configured_file)?;
            Ok(candidate.to_path_buf())
        }
        Err(error) => Err(ConfigError::ReadFailed {
            path: candidate.display().to_string(),
            reason: error.to_string(),
        }),
    }
}

fn canonicalize_existing_ancestor(path: &Path) -> ConfigResult<PathBuf> {
    for ancestor in path.ancestors() {
        match ancestor.canonicalize() {
            Ok(canonical_path) => return Ok(canonical_path),
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(ConfigError::ReadFailed {
                    path: ancestor.display().to_string(),
                    reason: error.to_string(),
                });
            }
        }
    }

    Err(ConfigError::Invalid(format!(
        "{path:?} has no existing ancestor for containment validation"
    )))
}

fn ensure_within_base(
    canonical_base: &Path,
    canonical_path: &Path,
    field: &str,
    configured_file: &str,
) -> ConfigResult<()> {
    if canonical_path.starts_with(canonical_base) {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!(
            "{field} resolves outside its base directory: {configured_file:?}"
        )))
    }
}

fn read_optional_file(path: &Path) -> ConfigResult<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(ConfigError::ReadFailed {
            path: path.display().to_string(),
            reason: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    static NEXT_TEST_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    fn test_directory() -> PathBuf {
        let unique = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "harw-config-loader-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn system_prompt_rejects_absolute_configured_path() {
        let error = load_system_prompt(Path::new("agents/planner"), Some("/etc/passwd"))
            .expect_err("absolute configured system paths must be rejected");

        assert!(matches!(error, ConfigError::Invalid(message) if message.contains("system_file")));
    }

    #[test]
    fn skill_instructions_reject_parent_traversal() {
        let error = load_skill_instructions(Path::new("skills/review"), Some("../secret.md"))
            .expect_err("parent traversal in configured skill paths must be rejected");

        assert!(
            matches!(error, ConfigError::Invalid(message) if message.contains("instructions_file"))
        );
    }

    #[test]
    fn configured_file_path_rejects_nested_parent_traversal() {
        let error = configured_file_path(
            Path::new("agents/planner"),
            "prompts/../../secret.md",
            "system_file",
        )
        .expect_err("parent traversal must be rejected regardless of its position");

        assert!(matches!(error, ConfigError::Invalid(message) if message.contains("system_file")));
    }

    #[test]
    fn configured_file_path_allows_relative_nested_path() {
        let directory = test_directory();
        let prompts = directory.join("prompts");
        fs::create_dir(&prompts).unwrap();

        let path = configured_file_path(&directory, "prompts/system.md", "system_file")
            .expect("relative paths within the base directory should be accepted");

        assert_eq!(path, prompts.join("system.md"));
        fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn system_prompt_rejects_symlink_that_escapes_agent_directory() {
        let agent_directory = test_directory();
        let outside_directory = test_directory();
        fs::write(outside_directory.join("goal.md"), "outside agent root").unwrap();
        symlink(&outside_directory, agent_directory.join("escape")).unwrap();

        let error = load_system_prompt(&agent_directory, Some("escape/goal.md"))
            .expect_err("a system prompt symlink must not escape its agent directory");

        assert!(matches!(error, ConfigError::Invalid(message) if message.contains("system_file")));
        fs::remove_dir_all(&agent_directory).unwrap();
        fs::remove_dir_all(&outside_directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn skill_instructions_reject_symlinked_missing_file_outside_skill_directory() {
        let skill_directory = test_directory();
        let outside_directory = test_directory();
        symlink(&outside_directory, skill_directory.join("escape")).unwrap();

        let error = load_skill_instructions(&skill_directory, Some("escape/missing.md"))
            .expect_err("an optional file through an escaping symlink must be rejected");

        assert!(
            matches!(error, ConfigError::Invalid(message) if message.contains("instructions_file"))
        );
        fs::remove_dir_all(&skill_directory).unwrap();
        fs::remove_dir_all(&outside_directory).unwrap();
    }

    #[test]
    fn loader_allows_missing_optional_file() {
        let directory = test_directory();
        let content = load_system_prompt(&directory, Some("missing.md"))
            .expect("a missing optional system prompt should be empty");

        assert!(content.is_empty());
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn loader_reports_non_not_found_read_errors() {
        let directory = test_directory();
        let error = load_skill_instructions(&directory, Some("."))
            .expect_err("reading a directory must report the I/O error");

        assert!(matches!(error, ConfigError::ReadFailed { .. }));
        std::fs::remove_dir(&directory).unwrap();
    }
}
