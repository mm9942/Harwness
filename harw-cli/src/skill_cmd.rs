//! `harw skill list|show|preview|validate` (PL-97 B3): Skill-Katalog,
//! Trigger-Vorschau und Prüfung eines Skill-Verzeichnisses von der
//! Kommandozeile aus.
//!
//! `list`, `show` und `preview` rufen dieselbe Operation auf wie der
//! Chat-Befehl `/skills` (`/skills list`, `/skills show <name>`,
//! `/skills preview …`), damit Terminal und Chat identisch reagieren.
//! `preview` wählt dabei über denselben Injektor wie der Turn-Loop
//! (`harw_core::SkillInjector::preview`: dieselben `[skills]`-Einstellungen,
//! derselbe `SkillIndex` samt `TriggerIndex`) und ruft nie ein Modell auf.
//! `validate` braucht weder Konfiguration noch Laufzeit und prüft das
//! Verzeichnis direkt über `harw_ops::skills::validate_skill_dir` — denselben
//! Weg wie `/skills validate`.
//!
//! Exit-Codes: 0 bei Erfolg, 1 bei einem Fehlschlag (die Operation scheitert,
//! `validate` beanstandet etwas), 2 bei einer falschen Bedienung (clap bzw.
//! eine Vorschau ganz ohne Eingabe).

use std::path::PathBuf;

use crate::cli::{GlobalArgs, SkillAction};
use crate::jobs_cmd::working_dir;
use crate::op_bridge::{OpTarget, run_operation};
use crate::output::Printer;

/// Exit-Code eines Fehlschlags.
const FAILURE_EXIT: i32 = 1;

/// Was eine `skill`-Aktion ausführt.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SkillPlan {
    /// Die Operation `/skills` mit diesen Token.
    Operation(Vec<String>),
    /// Das Skill-Verzeichnis direkt prüfen.
    Validate(PathBuf),
}

/// Führt `harw skill …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext bei einer Vorschau ohne jede Eingabe, einem
/// nicht ermittelbaren Arbeits- bzw. Skill-Verzeichnis oder einer
/// fehlgeschlagenen Ausgabe. Scheitert die Operation oder beanstandet
/// `validate` etwas, endet der Prozess nach der Ausgabe mit Exit-Code 1.
pub fn run(g: &GlobalArgs, a: SkillAction) -> Result<(), String> {
    let printer = Printer::new(g.output());
    match plan(a)? {
        SkillPlan::Validate(dir) => {
            let dir = std::path::absolute(&dir).map_err(|error| {
                format!(
                    "Skill-Verzeichnis '{}' nicht auflösbar: {error}",
                    dir.display()
                )
            })?;
            let report = harw_ops::skills::validate_skill_dir(&dir);
            printer.value(&report.render(), report.to_json())?;
            if !report.is_valid() {
                std::process::exit(FAILURE_EXIT);
            }
            Ok(())
        }
        SkillPlan::Operation(tokens) => {
            let cwd = working_dir(g)?;
            let target = OpTarget {
                home: g.home.clone(),
                cwd: &cwd,
            };
            match run_operation(target, "/skills", tokens) {
                Ok(output) => printer.op_output(&output),
                Err(error) => {
                    eprintln!("harw: {error}");
                    std::process::exit(FAILURE_EXIT);
                }
            }
        }
    }
}

/// Übersetzt eine `skill`-Aktion in die Token von `/skills` bzw. die
/// direkte Prüfung.
///
/// Die Token entsprechen der Chat-Grammatik: das Verb zuerst; bei `preview`
/// jede Option mit genau einem Wert und der Aufgabentext hinter `--`, damit
/// ein Text mit führendem `--` nie als Option gilt.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn `preview` weder Aufgabentext noch eine
/// Option erhält.
fn plan(a: SkillAction) -> Result<SkillPlan, String> {
    let tokens = match a {
        SkillAction::List => vec!["list".to_owned()],
        SkillAction::Show { name } => vec!["show".to_owned(), name],
        SkillAction::Preview {
            task,
            tools,
            errors,
            paths,
            commands,
            budget,
        } => {
            let task = task.join(" ");
            let has_input = !task.trim().is_empty()
                || [&tools, &errors, &paths, &commands]
                    .iter()
                    .any(|values| values.iter().any(|value| !value.trim().is_empty()));
            if !has_input {
                return Err(
                    "`harw skill preview` braucht einen Aufgabentext oder mindestens \
                     eine der Angaben --tool, --error, --path, --command"
                        .to_owned(),
                );
            }
            let mut tokens = vec!["preview".to_owned()];
            for (option, values) in [
                ("--tool", tools),
                ("--error", errors),
                ("--path", paths),
                ("--command", commands),
            ] {
                for value in values {
                    tokens.push(option.to_owned());
                    tokens.push(value);
                }
            }
            if let Some(budget) = budget {
                tokens.push("--budget".to_owned());
                tokens.push(budget.to_string());
            }
            if !task.trim().is_empty() {
                tokens.push("--".to_owned());
                tokens.push(task);
            }
            tokens
        }
        SkillAction::Validate { dir } => return Ok(SkillPlan::Validate(dir)),
    };
    Ok(SkillPlan::Operation(tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_operations::FromRawArgs;

    fn preview(task: &[&str]) -> SkillAction {
        SkillAction::Preview {
            task: task.iter().map(|word| (*word).to_owned()).collect(),
            tools: Vec::new(),
            errors: Vec::new(),
            paths: Vec::new(),
            commands: Vec::new(),
            budget: None,
        }
    }

    fn operation_tokens(action: SkillAction) -> TestResult<Vec<String>> {
        match plan(action).map_err(TestError::Unexpected)? {
            SkillPlan::Operation(tokens) => Ok(tokens),
            other => Err(TestError::Unexpected(format!(
                "erwartete eine Operation, bekam {other:?}"
            ))),
        }
    }

    #[test]
    fn skill_list_and_show_map_to_the_skills_operation() -> TestResult {
        assert_eq!(operation_tokens(SkillAction::List)?, vec!["list"]);
        assert_eq!(
            operation_tokens(SkillAction::Show {
                name: "rust-borrow".to_owned()
            })?,
            vec!["show", "rust-borrow"]
        );
        Ok(())
    }

    #[test]
    fn skill_preview_passes_every_option_and_the_task_after_a_separator() -> TestResult {
        let tokens = operation_tokens(SkillAction::Preview {
            task: vec!["--weird".to_owned(), "cargo test fails".to_owned()],
            tools: vec!["sys.ps".to_owned()],
            errors: vec!["E0505".to_owned()],
            paths: vec!["src/lib.rs".to_owned()],
            commands: vec!["cargo test".to_owned()],
            budget: Some(512),
        })?;
        assert_eq!(
            tokens,
            vec![
                "preview",
                "--tool",
                "sys.ps",
                "--error",
                "E0505",
                "--path",
                "src/lib.rs",
                "--command",
                "cargo test",
                "--budget",
                "512",
                "--",
                "--weird cargo test fails",
            ]
        );
        // Die Operation liest dieselben Eingaben zurück.
        let args = harw_ops::skills::SkillsArgs::from_raw_args(&tokens)
            .map_err(ctx("SkillsArgs::from_raw_args"))?;
        let parsed =
            harw_ops::skills::PreviewArgs::parse(&args.rest).map_err(ctx("PreviewArgs::parse"))?;
        assert_eq!(parsed.input.task, "--weird cargo test fails");
        assert_eq!(parsed.input.tools, vec!["sys.ps"]);
        assert_eq!(parsed.input.tool_output, vec!["E0505"]);
        assert_eq!(parsed.input.paths, vec!["src/lib.rs"]);
        assert_eq!(parsed.input.commands, vec!["cargo test"]);
        assert_eq!(parsed.budget, Some(512));
        Ok(())
    }

    #[test]
    fn skill_preview_without_any_input_is_a_usage_error() {
        assert!(plan(preview(&[])).is_err());
        assert!(plan(preview(&["  "])).is_err());
        assert!(plan(preview(&["cargo", "test"])).is_ok());
    }

    #[test]
    fn skill_validate_checks_the_directory_directly() -> TestResult {
        let dir = PathBuf::from("skills/probe");
        assert_eq!(
            plan(SkillAction::Validate { dir: dir.clone() }).map_err(TestError::Unexpected)?,
            SkillPlan::Validate(dir)
        );
        Ok(())
    }

    /// `harw skill validate` meldet bei 17 Schlüsselwörtern genau die Warnung
    /// aus `harw-catalog/src/skill_triggers.rs` und beanstandet das
    /// Verzeichnis (Exit-Code 1).
    #[test]
    fn skill_validate_reports_the_catalog_warning_for_seventeen_keywords() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = temp.path().join("probe");
        std::fs::create_dir_all(&dir).map_err(ctx("create skill dir"))?;
        std::fs::write(
            dir.join("skill.toml"),
            "name = \"probe\"\ndescription = \"Use when probing.\"\n",
        )
        .map_err(ctx("write skill.toml"))?;
        std::fs::write(dir.join("instructions.md"), "# Probe\n")
            .map_err(ctx("write instructions.md"))?;
        let keywords: Vec<String> = (1..=17).map(|n| format!("\"keyword{n:02}\"")).collect();
        std::fs::write(
            dir.join("triggers.toml"),
            format!("[triggers]\nkeywords = [{}]\n", keywords.join(", ")),
        )
        .map_err(ctx("write triggers.toml"))?;

        let report = harw_ops::skills::validate_skill_dir(&dir);
        assert!(!report.is_valid());
        assert_eq!(
            report.warnings,
            vec!["triggers.keywords: 17 Einträge, nur die ersten 16 behalten".to_owned()]
        );
        assert!(
            report
                .render()
                .contains("Warnung: triggers.keywords: 17 Einträge, nur die ersten 16 behalten")
        );
        assert_eq!(report.to_json()["valid"], false);
        Ok(())
    }
}
