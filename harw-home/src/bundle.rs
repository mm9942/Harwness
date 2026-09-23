//! Mit dem Binary ausgelieferte Agenten- und Skill-Definitionen.
//!
//! # Verantwortung
//! Dieses Modul besitzt die eingebettete Startausstattung des Root-Space: die
//! Delegationshierarchie (ein Root-Orchestrator, vier Child-Orchestratoren,
//! zwölf Worker) und die Skills, auf die deren `agent.toml` verweist. Es
//! besitzt **nicht** das Schreiben — das leistet [`crate::scaffold::ensure_home`].
//!
//! # Schlüsseltypen
//! - [`BundledFile`] — ein eingebetteter Dateiinhalt samt Zielpfad.
//!
//! # Abgrenzung
//! Die UIA ist bewusst **nicht** Teil des Bundles: sie entsteht pro Nutzer über
//! `harw uia new`. Das Bundle liefert nur, was unterhalb der UIA delegiert wird.
//!
//! # Nebenläufigkeit
//! Ausschließlich `&'static`-Daten ohne innere Veränderlichkeit: `Send + Sync`,
//! beliebig teilbar, keine Sperren.
//!
//! # Fehler
//! Dieses Modul erzeugt keine Fehler; es liefert nur Konstanten.
//!
//! # Beispiele
//! ```rust
//! let agents = harw_home::bundled_files()
//!     .iter()
//!     .filter(|file| file.relative_path.starts_with("agents/"))
//!     .count();
//! assert!(agents > 0);
//! ```

use std::path::PathBuf;

/// Eine eingebettete Bundle-Datei samt ihrem Ziel im Root-Space.
///
/// # Description
/// `relative_path` ist stets mit `/` getrennt, unabhängig von der Plattform;
/// [`Self::target_in`] setzt daraus einen plattformgerechten Pfad zusammen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundledFile {
    /// Pfad relativ zum Root-Space, z. B. `"agents/debugger/agent.toml"`.
    pub relative_path: &'static str,
    /// Vollständiger Dateiinhalt, zur Compile-Zeit eingebettet.
    pub contents: &'static str,
}

impl BundledFile {
    /// Setzt den Zielpfad dieser Datei unterhalb von `home` zusammen.
    ///
    /// # Arguments
    /// - `home` (`&std::path::Path`): Root-Space, üblicherweise `~/.harw`.
    ///
    /// # Returns
    /// Der plattformgerechte Zielpfad.
    ///
    /// # Concurrency
    /// Reine Berechnung, aus jedem Thread aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use std::path::Path;
    ///
    /// let file = harw_home::bundled_files()[0];
    /// assert!(file.target_in(Path::new("/tmp/harw")).starts_with("/tmp/harw"));
    /// ```
    #[must_use]
    pub fn target_in(&self, home: &std::path::Path) -> PathBuf {
        let mut target = home.to_path_buf();
        for segment in self.relative_path.split('/') {
            target.push(segment);
        }
        target
    }
}

/// Liefert alle eingebetteten Agenten- und Skill-Dateien.
///
/// # Description
/// Die Reihenfolge ist stabil (Pfad-sortiert) und Teil des beobachtbaren
/// Verhaltens, damit ein Scaffold-Bericht deterministisch bleibt.
///
/// # Returns
/// Ein `&'static`-Slice aller Bundle-Dateien.
///
/// # Concurrency
/// Gibt nur statische Daten zurück; aus jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// assert!(
///     harw_home::bundled_files()
///         .iter()
///         .any(|file| file.relative_path == "agents/coding-orchestrator/agent.toml")
/// );
/// ```
#[must_use]
pub fn bundled_files() -> &'static [BundledFile] {
    BUNDLED_FILES
}

/// Die eingebettete Startausstattung. Neue Dateien unter `assets/` müssen hier
/// eingetragen werden; `test_every_asset_file_is_embedded` erzwingt das.
static BUNDLED_FILES: &[BundledFile] = &[
    BundledFile {
        relative_path: "agents/coding-orchestrator/agent.toml",
        contents: include_str!("../assets/agents/coding-orchestrator/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/coding-orchestrator/system.md",
        contents: include_str!("../assets/agents/coding-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/debugger/agent.toml",
        contents: include_str!("../assets/agents/debugger/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/debugger/system.md",
        contents: include_str!("../assets/agents/debugger/system.md"),
    },
    BundledFile {
        relative_path: "agents/debug-orchestrator/agent.toml",
        contents: include_str!("../assets/agents/debug-orchestrator/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/debug-orchestrator/system.md",
        contents: include_str!("../assets/agents/debug-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/dependency-analyst/agent.toml",
        contents: include_str!("../assets/agents/dependency-analyst/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/dependency-analyst/system.md",
        contents: include_str!("../assets/agents/dependency-analyst/system.md"),
    },
    BundledFile {
        relative_path: "agents/dependency-research-orchestrator/agent.toml",
        contents: include_str!("../assets/agents/dependency-research-orchestrator/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/dependency-research-orchestrator/system.md",
        contents: include_str!("../assets/agents/dependency-research-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/dependency-security-reviewer/agent.toml",
        contents: include_str!("../assets/agents/dependency-security-reviewer/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/dependency-security-reviewer/system.md",
        contents: include_str!("../assets/agents/dependency-security-reviewer/system.md"),
    },
    BundledFile {
        relative_path: "agents/implementation-orchestrator/agent.toml",
        contents: include_str!("../assets/agents/implementation-orchestrator/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/implementation-orchestrator/system.md",
        contents: include_str!("../assets/agents/implementation-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/incident-triager/agent.toml",
        contents: include_str!("../assets/agents/incident-triager/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/incident-triager/system.md",
        contents: include_str!("../assets/agents/incident-triager/system.md"),
    },
    BundledFile {
        relative_path: "agents/log-analyst/agent.toml",
        contents: include_str!("../assets/agents/log-analyst/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/log-analyst/system.md",
        contents: include_str!("../assets/agents/log-analyst/system.md"),
    },
    BundledFile {
        relative_path: "agents/refactorer/agent.toml",
        contents: include_str!("../assets/agents/refactorer/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/refactorer/system.md",
        contents: include_str!("../assets/agents/refactorer/system.md"),
    },
    BundledFile {
        relative_path: "agents/rust-implementer/agent.toml",
        contents: include_str!("../assets/agents/rust-implementer/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/rust-implementer/system.md",
        contents: include_str!("../assets/agents/rust-implementer/system.md"),
    },
    BundledFile {
        relative_path: "agents/secret-scanner/agent.toml",
        contents: include_str!("../assets/agents/secret-scanner/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/secret-scanner/system.md",
        contents: include_str!("../assets/agents/secret-scanner/system.md"),
    },
    BundledFile {
        relative_path: "agents/security-auditor/agent.toml",
        contents: include_str!("../assets/agents/security-auditor/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/security-auditor/system.md",
        contents: include_str!("../assets/agents/security-auditor/system.md"),
    },
    BundledFile {
        relative_path: "agents/security-inspection-orchestrator/agent.toml",
        contents: include_str!("../assets/agents/security-inspection-orchestrator/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/security-inspection-orchestrator/system.md",
        contents: include_str!("../assets/agents/security-inspection-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/source-researcher/agent.toml",
        contents: include_str!("../assets/agents/source-researcher/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/source-researcher/system.md",
        contents: include_str!("../assets/agents/source-researcher/system.md"),
    },
    BundledFile {
        relative_path: "agents/system-observer/agent.toml",
        contents: include_str!("../assets/agents/system-observer/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/system-observer/system.md",
        contents: include_str!("../assets/agents/system-observer/system.md"),
    },
    BundledFile {
        relative_path: "agents/test-engineer/agent.toml",
        contents: include_str!("../assets/agents/test-engineer/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/test-engineer/system.md",
        contents: include_str!("../assets/agents/test-engineer/system.md"),
    },
    BundledFile {
        relative_path: "skills/debugging/skill.toml",
        contents: include_str!("../assets/skills/debugging/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/dependency-research/skill.toml",
        contents: include_str!("../assets/skills/dependency-research/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/implementation/skill.toml",
        contents: include_str!("../assets/skills/implementation/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/incident-response/skill.toml",
        contents: include_str!("../assets/skills/incident-response/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/planning/skill.toml",
        contents: include_str!("../assets/skills/planning/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/security-inspection/skill.toml",
        contents: include_str!("../assets/skills/security-inspection/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/server-operations/skill.toml",
        contents: include_str!("../assets/skills/server-operations/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/verification/skill.toml",
        contents: include_str!("../assets/skills/verification/skill.toml"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    /// Jede Datei unter `assets/` muss im Bundle stehen — sonst läge sie im
    /// Repo, käme aber nie beim Nutzer an.
    #[test]
    fn test_every_asset_file_is_embedded() -> TestResult {
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let mut on_disk = Vec::new();
        collect_files(&assets, &assets, &mut on_disk)?;
        on_disk.sort();

        let mut embedded: Vec<String> = bundled_files()
            .iter()
            .map(|file| file.relative_path.to_owned())
            .collect();
        embedded.sort();

        assert_eq!(
            on_disk, embedded,
            "assets/ und BUNDLED_FILES laufen auseinander"
        );
        Ok(())
    }

    fn collect_files(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut Vec<String>,
    ) -> TestResult {
        let entries = std::fs::read_dir(dir).map_err(ctx("assets-Verzeichnis lesbar"))?;
        for entry in entries {
            let path = entry.map_err(ctx("Verzeichniseintrag lesbar"))?.path();
            if path.is_dir() {
                collect_files(root, &path, out)?;
            } else {
                let relative = path
                    .strip_prefix(root)
                    .map_err(ctx("Pfad liegt unter assets"))?;
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
        Ok(())
    }

    #[test]
    fn test_no_bundled_file_is_empty() {
        for file in bundled_files() {
            assert!(
                !file.contents.trim().is_empty(),
                "{} ist leer",
                file.relative_path
            );
        }
    }

    #[test]
    fn test_bundled_agent_toml_parses_and_declares_no_uia_role() -> TestResult {
        let mut agents = 0;
        for file in bundled_files() {
            if !file.relative_path.ends_with("/agent.toml") {
                continue;
            }
            agents += 1;
            let agent: harw_config::AgentToml = toml::from_str(file.contents).map_err(|error| {
                crate::test_support::TestError::Unexpected(format!(
                    "{}: {error}",
                    file.relative_path
                ))
            })?;
            assert_ne!(
                agent.role, "user-interface",
                "{} darf keine UIA sein — die entsteht pro Nutzer über `harw uia new`",
                file.relative_path
            );
        }
        assert_eq!(agents, 17, "das Bundle liefert 17 Agentendefinitionen");
        Ok(())
    }

    #[test]
    fn test_bundled_skill_toml_parses() -> TestResult {
        let mut skills = 0;
        for file in bundled_files() {
            if !file.relative_path.ends_with("/skill.toml") {
                continue;
            }
            skills += 1;
            toml::from_str::<harw_config::SkillToml>(file.contents).map_err(|error| {
                crate::test_support::TestError::Unexpected(format!(
                    "{}: {error}",
                    file.relative_path
                ))
            })?;
        }
        assert_eq!(skills, 8, "das Bundle liefert 8 Skills");
        Ok(())
    }

    /// Ein Agent, der auf einen nicht mitgelieferten Skill verweist, wäre beim
    /// Nutzer sofort kaputt.
    #[test]
    fn test_every_referenced_skill_ships_with_the_bundle() -> TestResult {
        let available: Vec<String> = bundled_files()
            .iter()
            .filter_map(|file| {
                file.relative_path
                    .strip_prefix("skills/")
                    .and_then(|rest| rest.strip_suffix("/skill.toml"))
                    .map(str::to_owned)
            })
            .collect();

        for file in bundled_files() {
            if !file.relative_path.ends_with("/agent.toml") {
                continue;
            }
            let agent: harw_config::AgentToml =
                toml::from_str(file.contents).map_err(ctx("agent.toml parst"))?;
            for skill in &agent.skills {
                assert!(
                    available.contains(skill),
                    "{} verweist auf den fehlenden Skill '{skill}'",
                    file.relative_path
                );
            }
        }
        Ok(())
    }

    #[test]
    fn test_target_in_joins_segments_platform_correctly() {
        let file = BundledFile {
            relative_path: "agents/debugger/agent.toml",
            contents: "",
        };
        let target = file.target_in(std::path::Path::new("/tmp/harw"));
        assert_eq!(
            target,
            std::path::Path::new("/tmp/harw")
                .join("agents")
                .join("debugger")
                .join("agent.toml")
        );
    }
}
