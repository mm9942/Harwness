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

/// Führt einen Compiler-Befehl aus, gibt das Ergebnis aus und beendet den
/// Prozess bei einem Exit-Code ungleich 0.
fn run_compiler(g: &GlobalArgs, env: CompilerEnv, command: AgentCommand) -> Result<(), String> {
    let mut progress = |line: &str| eprintln!("harw: {line}");
    let mut ctx = CommandContext {
        env,
        probe: &ProcessProbe,
        case_runner: &harw_agent_compiler::testing::EchoStub,
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
