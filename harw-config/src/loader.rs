use crate::error::{ConfigError, ConfigResult};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// Lädt den System-Prompt für einen Agenten aus seinem Dir.
pub fn load_system_prompt(agent_dir: &Path, system_file: Option<&str>) -> ConfigResult<String> {
    let path = configured_file_path(agent_dir, system_file.unwrap_or("system.md"), "system_file")?;
    read_optional_file(&path)
}

/// Lädt den freien Identitätstext einer UIA aus `identity.md` in ihrem
/// Agentenordner.
///
/// # Description
/// `identity.md` beantwortet *wer/was ist der Agent selbst* (Name, Herkunft,
/// Selbstverständnis, Abgrenzung zu anderen UIAs) — stabil, ändert sich
/// selten. Das unterscheidet sie von `Personality.md` (Ton/Antwortverhalten)
/// und von `USER.md` (Kontext über den Nutzer). Analog zu
/// [`load_system_prompt`]: reiner Volltext-Read über [`configured_file_path`]
/// und [`read_optional_file`], keine Zeilen-Interpretation (im Gegensatz zu
/// [`load_uia_user_name`]), weil `identity.md` freie Prosa ist.
///
/// # Arguments
/// - `agent_dir` (`&Path`): der Agentenordner der UIA.
///
/// # Returns
/// Den vollständigen Dateiinhalt, oder einen leeren `String`, wenn
/// `identity.md` fehlt ("fehlt" ist kein Fehler, siehe `read_optional_file`).
///
/// # Errors
/// - [`ConfigError::Invalid`]: wenn `identity.md` (konstant, kein
///   Aufrufer-Parameter) dennoch einen Pfad-Traversal-Versuch ergäbe (nur
///   über einen manipulierten Symlink im Agentenordner erreichbar).
/// - [`ConfigError::ReadFailed`]: bei jedem I/O-Fehler außer "nicht gefunden".
pub fn load_uia_identity(agent_dir: &Path) -> ConfigResult<String> {
    let path = configured_file_path(agent_dir, "identity.md", "identity.md")?;
    read_optional_file(&path)
}

/// Lädt die drei benutzerpflegbaren UIA-Kontextdateien aus deren Agentenordner.
///
/// `identity.md` beantwortet, wer der Agent selbst ist (siehe
/// [`load_uia_identity`]). `Personality.md` prägt ausschließlich Ton,
/// Persönlichkeit und Antwortverhalten. `USER.md` enthält persönlichen
/// Kontext über den Nutzer. Alle drei Dateien bleiben optional und werden als
/// getrennte, gekennzeichnete Modellkontext-Fragmente zurückgegeben, in der
/// Reihenfolge Identität → Ton → Nutzerkontext; technische Autorisierung
/// kommt weiterhin ausschließlich aus `definition.toml`, Runtime und Sandbox.
pub fn load_uia_personalization(agent_dir: &Path) -> ConfigResult<Vec<String>> {
    let identity = load_uia_identity(agent_dir)?;
    let personality = read_optional_file(&configured_file_path(
        agent_dir,
        "Personality.md",
        "Personality.md",
    )?)?;
    let user = read_optional_file(&configured_file_path(agent_dir, "USER.md", "USER.md")?)?;

    let mut fragments = Vec::new();
    if !identity.trim().is_empty() {
        fragments.push(format!("# UIA-Identität\n{identity}"));
    }
    if !personality.trim().is_empty() {
        fragments.push(format!(
            "# UIA-Persönlichkeit und Antwortverhalten\n{personality}"
        ));
    }
    if !user.trim().is_empty() {
        fragments.push(format!("# Nutzerkontext (USER.md)\n{user}"));
    }
    Ok(fragments)
}

/// Liest den freiwillig hinterlegten Anzeigenamen aus der `USER.md` einer UIA.
///
/// Nur eine eigene Zeile im Format `Name: Ada` (Groß-/Kleinschreibung des
/// Feldnamens ist unerheblich) gilt als Anzeigename. Sonstiger persönlicher
/// Kontext wird bewusst nicht geraten oder als Name ausgegeben.
pub fn load_uia_user_name(agent_dir: &Path) -> ConfigResult<Option<String>> {
    let user = read_optional_file(&configured_file_path(agent_dir, "USER.md", "USER.md")?)?;
    Ok(user.lines().find_map(user_name_from_line))
}

fn user_name_from_line(line: &str) -> Option<String> {
    let line = line.trim();
    let line = line.strip_prefix('-').map_or(line, str::trim_start);
    let (label, value) = line.split_once(':')?;
    // USER.md ist Markdown: neben `Name: Mia` ist daher auch der übliche
    // Listenpunkt `- **Name:** Mia` ein explizites Namensfeld.
    let label = label.trim().trim_matches('*').trim();
    if !label.eq_ignore_ascii_case("name") {
        return None;
    }

    // Der Wert kann das schließende `**` der Markdown-Hervorhebung tragen
    // (z. B. `- **Name:** Mia` → Wert `** Mia`, da der erste `:` bereits
    // innerhalb der Hervorhebung liegt) — deshalb dieselbe `*`-Bereinigung
    // wie beim Label.
    let value = value
        .trim()
        .trim_matches(['"', '\''])
        .trim()
        .trim_matches('*')
        .trim();
    (!value.is_empty() && !value.chars().any(char::is_control)).then(|| value.to_owned())
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
    fn load_uia_identity_missing_file_returns_empty() {
        let directory = test_directory();
        assert!(load_uia_identity(&directory).unwrap().is_empty());
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn load_uia_identity_present_returns_content() {
        let directory = test_directory();
        fs::write(directory.join("identity.md"), "Ich bin Emily.").unwrap();
        assert_eq!(load_uia_identity(&directory).unwrap(), "Ich bin Emily.");
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn load_uia_identity_rejects_path_traversal_via_escaping_symlink() {
        let agent_directory = test_directory();
        let outside_directory = test_directory();
        fs::write(outside_directory.join("identity.md"), "outside agent root").unwrap();
        symlink(&outside_directory, agent_directory.join("identity.md")).unwrap();

        let error = load_uia_identity(&agent_directory)
            .expect_err("an identity.md symlink must not escape its agent directory");

        assert!(matches!(error, ConfigError::Invalid(message) if message.contains("identity.md")));
        fs::remove_dir_all(&agent_directory).unwrap();
        fs::remove_dir_all(&outside_directory).unwrap();
    }

    #[test]
    fn uia_personalization_includes_identity_fragment_when_present() {
        let directory = test_directory();
        fs::write(directory.join("identity.md"), "Ich bin Emily.").unwrap();

        let fragments = load_uia_personalization(&directory).expect("load UIA files");
        assert_eq!(fragments.len(), 1);
        assert!(fragments[0].contains("UIA-Identität"));
        assert!(fragments[0].contains("Ich bin Emily."));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn uia_personalization_omits_identity_fragment_when_absent() {
        let directory = test_directory();
        fs::write(directory.join("Personality.md"), "warm and concise").unwrap();

        let fragments = load_uia_personalization(&directory).expect("load UIA files");
        assert_eq!(fragments.len(), 1);
        assert!(!fragments.iter().any(|fragment| fragment.contains("UIA-Identität")));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn uia_personalization_keeps_personality_and_user_context_separate() {
        let directory = test_directory();
        fs::write(directory.join("Personality.md"), "warm and concise").unwrap();
        fs::write(directory.join("USER.md"), "prefers German").unwrap();

        let fragments = load_uia_personalization(&directory).expect("load UIA files");
        assert_eq!(fragments.len(), 2);
        assert!(fragments[0].contains("UIA-Persönlichkeit"));
        assert!(fragments[0].contains("warm and concise"));
        assert!(fragments[1].contains("Nutzerkontext"));
        assert!(fragments[1].contains("prefers German"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn uia_user_name_reads_only_an_explicit_name_field() {
        let directory = test_directory();
        fs::write(directory.join("USER.md"), "# Nutzerkontext\n- Name: Mia\nprefers German").unwrap();

        assert_eq!(load_uia_user_name(&directory).unwrap().as_deref(), Some("Mia"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn uia_user_name_reads_a_markdown_emphasized_name_field() {
        let directory = test_directory();
        fs::write(directory.join("USER.md"), "# Nutzerprofil: Mia\n\n- **Name:** Mia").unwrap();

        assert_eq!(load_uia_user_name(&directory).unwrap().as_deref(), Some("Mia"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn uia_user_name_ignores_unstructured_context() {
        let directory = test_directory();
        fs::write(directory.join("USER.md"), "Mia prefers German").unwrap();

        assert_eq!(load_uia_user_name(&directory).unwrap(), None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn uia_personalization_allows_missing_files() {
        let directory = test_directory();
        assert!(load_uia_personalization(&directory).unwrap().is_empty());
        fs::remove_dir(directory).unwrap();
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
