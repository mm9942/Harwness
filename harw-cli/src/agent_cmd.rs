//! `harw agent …`: Agenten, Skills und Plugins verwalten (`uia-new`, `list`,
//! `skills`, `plugins`) und der Agenten-Compiler (#22 Welle 2B: `check`,
//! `build`, `inspect`, `graph`, `explain`, `new`, `fmt`, `diff`, `test`,
//! `run`, `versions`, `use`, `clean`, `doctor`).
//!
//! `uia-new` startet den Einrichtungsdialog für einen weiteren
//! Benutzeroberflächen-Agenten; `skills` und `plugins` rufen dieselben
//! Operationen auf wie die Chat-Befehle `/skills` und `/plugins`; `list`
//! zeigt den Roster wie `/agent defs` samt Snapshot und Build-Zustand. Die
//! Compiler-Befehle laufen über dieselbe Befehlsschicht wie `/agent …` in der
//! TUI ([`harw_agent_compiler::commands`]); Fortschritt geht nach stderr,
//! das Ergebnis als Text oder (`--json`) als JSON nach stdout. Der Exit-Code
//! folgt dem Befehl (1 bei Fehlern, 69 für `run` ohne Runner).

use std::io::Write;

use harw_agent_compiler::commands::{AgentCommand, BuildArgs, CleanArgs, parse_interfaces};
use harw_agent_compiler::graph::{GraphFormat, GraphKind};
use harw_agent_compiler::scaffold::ScaffoldRole;
use harw_agent_compiler::{CommandContext, CompilerEnv, ProcessProbe, run_command};

use crate::cli::{AgentAction, GlobalArgs};
use crate::jobs_cmd::{run_and_print, working_dir};
use crate::output::Printer;

/// Führt `harw agent …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn `--json` für den interaktiven
/// `uia-new`-Dialog verlangt wird, der Dialog bzw. die Operation scheitert,
/// ein Compiler-Argument ungültig ist oder die Ausgabe fehlschlägt. Ein
/// Compiler-Befehl, der mit Diagnosen endet, beendet den Prozess mit seinem
/// eigenen Exit-Code, nachdem das Ergebnis ausgegeben wurde.
///
/// # Description
/// `install-record` trägt vorher über
/// `crate::lifecycle::ensure_dod_defaults_for_install` `[dod.approve]
/// tui_tty = true` in eine vorhandene `config.toml` des Homes nach (PL-100
/// O12; nie fatal, ein ausdrückliches `false` bleibt). Gehört das Home einem
/// anderen Benutzer als dem effektiven (z. B. `sudo -E make install`), wird
/// nichts nachgetragen.
pub fn run(g: &GlobalArgs, a: AgentAction) -> Result<(), String> {
    match a {
        AgentAction::UiaNew => {
            Printer::new(g.output()).require_text("harw agent uia-new")?;
            crate::uia_bootstrap::run_new_uia_command(g.home.clone())?;
            // #22: die neue UIA im Hintergrund kompilieren, falls möglich.
            crate::auto_build::on_start(g.home.clone());
            Ok(())
        }
        // Plan R9, Teil C: derselbe Roster wie `/agent defs` in der TUI.
        AgentAction::List { query } => {
            let mut args = vec!["defs".to_owned()];
            args.extend(query);
            run_and_print(g, "/agent", args)
        }
        AgentAction::Skills { args } => run_and_print(g, "/skills", args),
        AgentAction::Plugins { args } => run_and_print(g, "/plugins", args),
        AgentAction::AutoBuildUia { release_lock } => {
            let env = compiler_env(g)?;
            let result = run_compiler(g, env.clone(), AgentCommand::AutoBuildUia);
            if release_lock {
                crate::auto_build::release_lock(&env);
            }
            result
        }
        // PL-100 O12: `install-record` läuft bei `make install`, `install.sh`
        // und `harw update`; dort wird `[dod.approve] tui_tty` nachgetragen.
        action @ AgentAction::InstallRecord { .. } => {
            let command = compiler_command(action)?;
            let env = compiler_env(g)?;
            // Vor `run_compiler`, das den Prozess beenden kann.
            ensure_install_defaults(&env);
            run_compiler(g, env, command)
        }
        other => {
            let command = compiler_command(other)?;
            let env = compiler_env(g)?;
            run_compiler(g, env, command)
        }
    }
}

/// Der Compiler-Kontext aus `--home` und dem Arbeitsverzeichnis.
fn compiler_env(g: &GlobalArgs) -> Result<CompilerEnv, String> {
    let cwd = working_dir(g)?;
    CompilerEnv::detect(g.home.clone(), cwd).map_err(|error| error.to_string())
}

/// Trägt die DoD-Vorgaben (`[dod.approve] tui_tty = true`, PL-100 O12) in die
/// globale `config.toml` des Compiler-Homes nach. Schlägt nie fehl; der
/// Rückgabewert sagt, ob geschrieben wurde. Unter Unix wird nichts
/// geschrieben, wenn das Home einem anderen Benutzer gehört als dem
/// effektiven (sonst würde `sudo -E` Konfiguration und Lock-Datei root
/// zuordnen).
fn ensure_install_defaults(env: &CompilerEnv) -> bool {
    if !home_owner_allows_backfill(&env.home) {
        tracing::debug!(reason = "home_foreign_owner", "dod.defaults.skipped");
        return false;
    }
    crate::lifecycle::ensure_dod_defaults_for_install(&env.home)
}

/// Entscheidung der Eigentümerprüfung: ein nicht ermittelbarer Eigentümer
/// (Home fehlt noch) erlaubt das Nachtragen, sonst muss er der effektiven UID
/// entsprechen.
fn owner_allows(home_uid: Option<u32>, euid: u32) -> bool {
    home_uid.is_none_or(|uid| uid == euid)
}

/// Prüft den Eigentümer des Homes (Symlinks werden nicht verfolgt).
#[cfg(unix)]
fn home_owner_allows_backfill(home: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::symlink_metadata(home)
        .ok()
        .map(|metadata| metadata.uid());
    owner_allows(uid, rustix::process::geteuid().as_raw())
}

/// Auf anderen Plattformen gibt es keine Eigentümerprüfung.
#[cfg(not(unix))]
fn home_owner_allows_backfill(_home: &std::path::Path) -> bool {
    true
}

/// Führt einen Compiler-Befehl aus, gibt das Ergebnis aus und beendet den
/// Prozess bei einem Exit-Code ungleich 0.
fn run_compiler(g: &GlobalArgs, env: CompilerEnv, command: AgentCommand) -> Result<(), String> {
    let mut progress = |line: &str| {
        // Fortschritt ist best effort: ein geschlossenes stderr bricht den Befehl nicht ab.
        let _ = writeln!(std::io::stderr(), "harw: {line}");
    };
    // `harw agent test` prüft Antworten über den installierten Runner.
    let runner_env = env.clone();
    let case_runner = harw_agent_compiler::testing::SubprocessCaseRunner::new(&runner_env);
    let mut ctx = CommandContext {
        env,
        probe: &ProcessProbe,
        case_runner: &case_runner,
        progress: &mut progress,
    };
    let output = run_command(&mut ctx, command);
    Printer::new(g.output()).value(&output.text, output.json)?;
    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
    Ok(())
}

/// Übersetzt die clap-Grammatik in einen Compiler-Befehl.
///
/// # Errors
/// Ein Fehlertext für ungültige Werte (Schnittstelle, Format, Rolle).
pub(crate) fn compiler_command(action: AgentAction) -> Result<AgentCommand, String> {
    Ok(match action {
        AgentAction::Check { targets } => AgentCommand::Check { targets },
        AgentAction::Build {
            agent,
            interface,
            native,
            artifact_only,
            runner,
            output,
            target_triple,
            harw_src,
        } => AgentCommand::Build(BuildArgs {
            target: agent,
            interfaces: if interface.is_empty() {
                None
            } else {
                Some(parse_interfaces(&interface.join(","))?)
            },
            native,
            artifact_only,
            runner,
            output,
            target_triple,
            harw_src,
        }),
        AgentAction::Inspect { target } => AgentCommand::Inspect { target },
        AgentAction::Graph {
            agent,
            all,
            format,
            kind,
        } => AgentCommand::Graph {
            target: agent,
            all,
            format: GraphFormat::parse(&format).ok_or_else(|| {
                format!("unbekanntes Format `{format}` (text, dot, mermaid, json)")
            })?,
            kind: GraphKind::parse(&kind).ok_or_else(|| {
                format!("unbekannte Art `{kind}` (delegation, resolution, rights, all)")
            })?,
        },
        AgentAction::Explain { target, field } => AgentCommand::Explain { target, field },
        AgentAction::New {
            name,
            role,
            extends,
            dir,
        } => AgentCommand::New {
            name,
            role: ScaffoldRole::parse(&role)
                .ok_or_else(|| format!("unbekannte Rolle `{role}` (worker, child-orchestrator)"))?,
            extends,
            dir,
        },
        AgentAction::Fmt { paths, check } => AgentCommand::Fmt { paths, check },
        AgentAction::Diff { left, right } => AgentCommand::Diff { left, right },
        AgentAction::Test { agent } => AgentCommand::Test { target: agent },
        AgentAction::Run { target, prompt } => AgentCommand::Run {
            target,
            prompt: (!prompt.is_empty()).then(|| prompt.join(" ")),
        },
        AgentAction::Versions { name } => AgentCommand::Versions { name },
        AgentAction::Use { name, version } => AgentCommand::Use { name, version },
        AgentAction::Clean {
            all,
            older_than,
            keep,
            dry_run,
        } => AgentCommand::Clean(CleanArgs {
            all,
            older_than_days: older_than,
            keep,
            dry_run,
        }),
        AgentAction::Doctor => AgentCommand::Doctor,
        AgentAction::InstallRecord { source_dir, bindir } => {
            AgentCommand::InstallRecord { source_dir, bindir }
        }
        AgentAction::AutoBuildUia { .. } => AgentCommand::AutoBuildUia,
        AgentAction::UiaNew
        | AgentAction::List { .. }
        | AgentAction::Skills { .. }
        | AgentAction::Plugins { .. } => {
            return Err("kein Compiler-Befehl".to_owned());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn install_record_action_maps_to_install_record_command() -> TestResult {
        let command = compiler_command(AgentAction::InstallRecord {
            source_dir: None,
            bindir: None,
        })
        .map_err(TestError::Unexpected)?;
        let AgentCommand::InstallRecord { source_dir, bindir } = command else {
            return Err(TestError::Unexpected("install-record".to_owned()));
        };
        assert!(source_dir.is_none() && bindir.is_none());
        Ok(())
    }

    /// `run(install-record)` gegen ein temporäres Home.
    fn run_install_record(home: &std::path::Path, cwd: &std::path::Path) -> TestResult {
        let g = GlobalArgs {
            home: Some(home.to_path_buf()),
            cwd: Some(cwd.to_path_buf()),
            ..GlobalArgs::default()
        };
        run(
            &g,
            AgentAction::InstallRecord {
                source_dir: None,
                bindir: None,
            },
        )
        .map_err(TestError::Unexpected)
    }

    #[test]
    fn run_install_record_backfills_tui_tty_once() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temporary HARW home"))?;
        let cwd = tempfile::tempdir().map_err(ctx("create temporary cwd"))?;
        let path = home.path().join("config.toml");
        std::fs::write(&path, "# scaffolded\n").map_err(ctx("seed config"))?;
        run_install_record(home.path(), cwd.path())?;
        assert!(home.path().join("install.toml").is_file());
        let first = std::fs::read_to_string(&path).map_err(ctx("read config"))?;
        assert!(first.contains("tui_tty = true"), "{first}");
        assert!(first.contains("# scaffolded"), "{first}");
        run_install_record(home.path(), cwd.path())?;
        let second = std::fs::read_to_string(&path).map_err(ctx("read config again"))?;
        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn run_install_record_keeps_explicit_false() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temporary HARW home"))?;
        let cwd = tempfile::tempdir().map_err(ctx("create temporary cwd"))?;
        let path = home.path().join("config.toml");
        let seed = "[dod.approve]\ntui_tty = false\n";
        std::fs::write(&path, seed).map_err(ctx("seed config"))?;
        run_install_record(home.path(), cwd.path())?;
        let text = std::fs::read_to_string(&path).map_err(ctx("read config"))?;
        assert_eq!(text, seed);
        Ok(())
    }

    #[test]
    fn owner_decision_requires_matching_uid() {
        assert!(owner_allows(None, 1000));
        assert!(owner_allows(Some(1000), 1000));
        assert!(!owner_allows(Some(1000), 1001));
    }

    #[test]
    fn own_home_passes_the_owner_check() -> TestResult {
        // Das temporäre Home gehört dem aktuellen Benutzer: erlaubt.
        let home = tempfile::tempdir().map_err(ctx("create temporary HARW home"))?;
        assert!(home_owner_allows_backfill(home.path()));
        Ok(())
    }
}
