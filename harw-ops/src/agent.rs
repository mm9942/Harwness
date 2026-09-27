//! `/agent` — Child-Agent-Management (list/stop/budget gegen `ManagedAgentSpawner`).
//!
//! # Verantwortungsbereich
//! Implementiert die `agent`-Operation gemäß Harwness Plan v2.
//! Exponiert einen **Command** `/agent` mit `channel_parity`-Sichtbarkeit.
//! Das Modell darf diese Operation **nicht** selbst aufrufen — kein
//! `model_tool`-Attribut, da sich das Modell nicht selbst manipulieren darf.
//!
//! # Schlüsseltypen
//! - [`AgentArgs`] — deserialisierbare Eingabe-Argumente mit typisierten Feldern
//!   für Action (`list`, `stop`, `budget`), optionalem Ziel-Agent-ID und
//!   optionalem Wert (z. B. Budget-Grenze).
//! - `AgentOperation` — vom `#[operation]`-Makro erzeugter Implementations-Struct.
//!
//! # Nebenläufigkeit
//! `AgentOperation` ist ein Unit-Struct ohne inneren Zustand → `Send + Sync`.
//!
//! # Fehler
//! - [`harw_operations::OpError::InvalidArguments`]: wenn `json_args` nicht in
//!   [`AgentArgs`] deserialisiert werden kann (wird vom Makro gehandhabt).
//!
//! # Verfügbarkeit
//! `list` und `stop` lesen den `Arc<ManagedAgentSpawner>`-Service aus
//! [`OpContext`], falls die TUI-Kompositionswurzel einen konfiguriert hat.
//! Ohne registrierten Spawner liefert die Operation weiterhin fail-closed
//! [`OpError::NotAvailable`]. `budget` zeigt die globalen [`harw_core::ChildLimits`],
//! die bei der Admission festgelegten [`harw_core::AgentBudget`]-Deckel je Kind
//! (Tokens, Tool-Aufrufe, Wanduhrzeit, Reasoning-Effort, Tiefe/Tiefendecke)
//! samt Live-Verbrauch aus `ChildRecord::live` sowie eine Aggregation je Rolle.
//! Eine Budget-*Anpassung* (`/agent budget <id> <wert>`) bietet der Controller
//! nicht an; sie bleibt fail-closed [`OpError::NotAvailable`].
//!
//! `use` braucht keinen Spawner: `/agent use <name>` prüft den Namen gegen
//! die eingebauten Rollen (`harw_registry_defaults::role_names::ALL`) und die
//! konfigurierten Agentendefinitionen und verankert ihn über
//! [`crate::config_util::SelectionPersistence::persist_active_agent`] als
//! `active_agent_definition` in der Profil-`config.toml`;
//! `/agent use --clear` entfernt den Eintrag. Beides gilt ab der nächsten
//! Sitzung.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::agent::AgentArgs;
//!
//! let args = AgentArgs {
//!     action: Some("stop".to_owned()),
//!     target: Some("abc-42".to_owned()),
//!     value: None,
//!     rest: None,
//! };
//! assert_eq!(args.action.as_deref(), Some("stop"));
//! assert_eq!(args.target.as_deref(), Some("abc-42"));
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

/// Eingabe-Argumente für die `agent`-Operation.
///
/// # Beschreibung
/// Trägt das optionale Sub-Kommando (`action`), das optionale Ziel-Agent-Objekt
/// (`target`, z. B. eine Agent-ID) sowie einen optionalen dritten Wert (`value`,
/// z. B. ein Budget-Limit).  Die Felder werden durch das [`harw_macros::FromRawArgs`]-Derive
/// direkt aus den tokenisierten TUI-Rohargumenten befüllt — jedes Feld erhält
/// genau das Token an der entsprechenden Position (0-basiert).
///
/// # Felder
/// - `action` (`Option<String>`): Token 0 — Sub-Kommando. Gültige Werte:
///   `"list"` (Standard), `"stop"`, `"budget"`, `"use"`. `None` wird intern als `"list"` behandelt.
/// - `target` (`Option<String>`): Token 1 — Ziel-Agent-ID (z. B. `"abc-42"`),
///   bei `use` der Agentenname oder `--clear`.
///   Relevant für `stop`, `budget` und `use`; bei `list` ignoriert.
/// - `value` (`Option<String>`): Token 2 — dritter Parameter (z. B. Budget-Grenze).
///   Relevant für `budget`; bei `list` und `stop` ignoriert.
///
/// # Verfügbarkeit
/// Die Sub-Kommandos werden gegen den in [`OpContext`] registrierten
/// `ManagedAgentSpawner` geroutet. Argumentwerte werden nie in
/// `NotAvailable`-Fehlermeldungen übernommen.
///
/// # Beispiel
/// ```rust
/// use harw_ops::agent::AgentArgs;
/// use harw_operations::FromRawArgs;
///
/// // "/agent stop abc-42" → tokens = ["stop", "abc-42"]
/// let args = AgentArgs::from_raw_args(&["stop".to_owned(), "abc-42".to_owned()]).unwrap();
/// assert_eq!(args.action.as_deref(), Some("stop"));
/// assert_eq!(args.target.as_deref(), Some("abc-42"));
/// assert!(args.value.is_none());
/// ```
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct AgentArgs {
    /// Sub-Kommando: `"list"` (Standard), `"stop"`, `"budget"`, `"use"`. Token 0.
    #[serde(default)]
    #[raw(first)]
    pub action: Option<String>,
    /// Ziel-Agent-ID (z. B. `"abc-42"`). Token 1. `None` wenn nicht angegeben.
    #[serde(default)]
    #[raw(nth = 1)]
    pub target: Option<String>,
    /// Dritter Parameter (z. B. Budget-Grenze). Token 2. `None` wenn nicht angegeben.
    #[serde(default)]
    #[raw(nth = 2)]
    pub value: Option<String>,
    /// Alle Token ab Token 1, mit Leerzeichen verbunden: die Argumente der
    /// Compiler-Befehle (`/agent check|build|inspect|graph|explain|new|fmt|
    /// diff|test|run|versions|clean|doctor …`, #22 Welle 2B).
    #[serde(default)]
    #[raw(join_from = 1)]
    pub rest: Option<String>,
}

/// Verwaltet ausschließlich die vom aufrufenden Parent besessenen Kinder.
///
/// # Beschreibung
/// Der von der Runtime installierte [`harw_core::ManagedAgentSpawner`] ist die
/// einzige Lifecycle-Grenze. `list` zeigt den Teilbaum der aktuellen Sitzung;
/// `stop` akzeptiert einen Knoten aus deren Teilbaum und lässt die rekursive
/// Abbruchwirkung beim Controller. `budget` zeigt Limits, Budget-Deckel und
/// Live-Verbrauch für den gesamten Teilbaum oder — mit `target` — für genau
/// einen besessenen Knoten. Fehlt der Dienst, bleibt die Operation
/// fail-closed mit [`OpError::NotAvailable`]. `use` wählt ohne Spawner die
/// Wurzel-Agentendefinition für künftige Sitzungen (siehe [`agent_use`]).
///
/// **Command only**: Das Modell darf diese Operation nicht selbst aufrufen, da
/// sich das Modell nicht selbst manipulieren darf. Kein `model_tool`-Attribut.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Session-Kontext; liefert die aufrufende Session-ID
///   und den `Arc<ManagedAgentSpawner>`-Dienst.
/// - `args` (`AgentArgs`): Typisierte Sub-Kommando-Argumente.
///
/// # Rückgabe
/// Eine textuelle Liste, eine Abbruchbestätigung, einen Budget-Bericht oder
/// eine präzise [`OpError`]-Antwort für fehlende Dienste, ungültige Argumente
/// und Ziele außerhalb des besessenen Teilbaums.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: `json_args` nicht deserialisierbar (Makro),
///   unbekannte Action, `stop` ohne Ziel oder ein Ziel ohne aktiven Record.
/// - [`OpError::NotAvailable`]: kein Spawner registriert, Ziel außerhalb des
///   eigenen Teilbaums oder eine angefragte Budget-Anpassung (`value`).
///
/// # Nebenläufigkeit
/// Die Operation selbst ist zustandslos; der Spawner nimmt nur kurz seine
/// internen Locks und liefert Snapshots zurück.
///
/// # Beispiel
/// ```rust,no_run
/// // Wird indirekt über Operation::run aufgerufen.
/// ```
#[operation(
    name = "agent",
    summary = "Child-Agent-Management: list/stop/budget gegen den registrierten ManagedAgentSpawner; use wählt den Wurzel-Agenten ab der nächsten Sitzung.",
    domain = "agents",
    permission = "operator",
    command(path = "/agent", visibility = "channel_parity", busy = "immediate")
)]
async fn agent(ctx: &OpContext, args: AgentArgs) -> Result<OpOutput, OpError> {
    // #22 Welle 2B: `/agent use <name> <version|digest>` schaltet die
    // installierte Version eines kompilierten Agenten um; mit nur einem
    // Argument bleibt `use` die Wahl der Wurzel-Agentendefinition.
    if args.action.as_deref() == Some("use") && args.value.is_some() {
        let tokens = compiler_tokens(&args);
        let (name, version) = (tokens.get(1).cloned(), tokens.get(2).cloned());
        if let (Some(name), Some(version)) = (name, version) {
            return run_compiler(
                ctx,
                harw_agent_compiler::AgentCommand::Use { name, version },
            );
        }
    }
    if args.action.as_deref() == Some("use") {
        return agent_use(ctx, args.target.as_deref());
    }
    // #22 Welle 2B: die Compiler-Befehle (`/agent check|build|…`).
    if args
        .action
        .as_deref()
        .is_some_and(|action| harw_agent_compiler::commands::COMPILER_ACTIONS.contains(&action))
    {
        return agent_compiler(ctx, &args);
    }
    // Plan R9, Teil C: die startbaren Definitionen (Roster: eingebaut und
    // benutzerdefiniert) brauchen keinen Spawner.
    if matches!(args.action.as_deref(), Some("defs" | "definitions")) {
        return agent_definitions(ctx, args.target.as_deref());
    }
    let Some(spawner) = ctx.service::<std::sync::Arc<harw_core::ManagedAgentSpawner>>() else {
        return Err(OpError::NotAvailable(
            "child-agent management is not available".to_owned(),
        ));
    };

    match args.action.as_deref().unwrap_or("list") {
        "list" => {
            let children = spawner.list_descendants_for(ctx.session_id());
            if children.is_empty() {
                return Ok(OpOutput::from(
                    "Keine aktiven Child-Agents. Startbare Agenten: /agent defs".to_owned(),
                ));
            }
            let mut lines = vec![format!("{} aktive(r) Child-Agent(s):", children.len())];
            for record in &children {
                lines.push(format!(
                    "- {} (role={}, model={}, depth={}, lease_expires_at={})",
                    record.child,
                    record.role,
                    record.model_route().as_deref().unwrap_or("-"),
                    record.depth,
                    record.lease_expires_at
                ));
            }
            Ok(OpOutput::from(lines.join(
                "
",
            )))
        }
        "stop" => {
            let Some(target) = args.target.as_deref() else {
                return Err(OpError::InvalidArguments(
                    "action 'stop' requires a target agent ID".to_owned(),
                ));
            };
            let child_id = harw_types::SessionId::from_str(target.to_owned());
            if !spawner.owns_descendant(ctx.session_id(), &child_id) {
                return Err(OpError::NotAvailable(
                    "agent target is unavailable in this parent session".to_owned(),
                ));
            }
            if spawner.request_cancellation(&child_id) {
                Ok(OpOutput::from(format!(
                    "Cancellation für Child-Agent {target} angefordert."
                )))
            } else {
                Err(OpError::InvalidArguments(format!(
                    "no admitted child agent found for target '{target}'"
                )))
            }
        }
        "budget" => {
            if args.value.is_some() {
                return Err(OpError::NotAvailable(
                    "child-agent budget adjustment is not available".to_owned(),
                ));
            }
            let records = match args.target.as_deref() {
                None => spawner.list_descendants_for(ctx.session_id()),
                Some(target) => {
                    let child_id = harw_types::SessionId::from_str(target.to_owned());
                    if !spawner.owns_descendant(ctx.session_id(), &child_id) {
                        return Err(OpError::NotAvailable(
                            "agent target is unavailable in this parent session".to_owned(),
                        ));
                    }
                    let Some(record) = spawner.child_record(&child_id) else {
                        return Err(OpError::InvalidArguments(format!(
                            "no admitted child agent found for target '{target}'"
                        )));
                    };
                    vec![record]
                }
            };
            Ok(OpOutput::from(format_budget_report(
                &spawner.limits(),
                &records,
                jiff::Timestamp::now(),
            )))
        }
        unknown => Err(OpError::InvalidArguments(format!(
            "unknown /agent action '{unknown}'"
        ))),
    }
}

/// Die Token eines Compiler-Befehls: Action plus alle weiteren Token.
fn compiler_tokens(args: &AgentArgs) -> Vec<String> {
    let mut tokens: Vec<String> = args.action.iter().cloned().collect();
    tokens.extend(
        args.rest
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned),
    );
    tokens
}

/// Der Compiler-Kontext der Sitzung: Root-Space aus `harw_home::home_dir`,
/// Arbeitsverzeichnis = Workspace-Wurzel der Sitzung.
fn compiler_env(ctx: &OpContext) -> Result<harw_agent_compiler::CompilerEnv, OpError> {
    // Der Compiler kennt die eingebauten Vorgaben nur über seinen
    // Installations-Slot; idempotent, damit auch SDK-Einbettungen ohne
    // `harw-cli::main_entry` den `/agent`-Pfad nutzen können.
    harw_registry_defaults::compiler_defaults::install();
    let cwd = ctx.sandbox().workspace().canonical_root().to_path_buf();
    harw_agent_compiler::CompilerEnv::detect(None, cwd)
        .map_err(|error| OpError::Execution(error.to_string()))
}

/// Führt einen Compiler-Befehl synchron aus (Fortschritt landet vor dem
/// Ergebnis im Text).
fn run_compiler(
    ctx: &OpContext,
    command: harw_agent_compiler::AgentCommand,
) -> Result<OpOutput, OpError> {
    let env = compiler_env(ctx)?;
    let mut progress_lines: Vec<String> = Vec::new();
    let mut progress = |line: &str| progress_lines.push(line.to_owned());
    // `/agent test` prüft Antworten über den installierten Runner.
    let runner_env = env.clone();
    let case_runner = harw_agent_compiler::testing::SubprocessCaseRunner::new(&runner_env);
    let mut command_ctx = harw_agent_compiler::CommandContext {
        env,
        probe: &harw_agent_compiler::ProcessProbe,
        case_runner: &case_runner,
        progress: &mut progress,
    };
    let output = harw_agent_compiler::run_command(&mut command_ctx, command);
    let mut text = progress_lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&output.text);
    if output.exit_code == 0 {
        Ok(OpOutput {
            text,
            data: Some(output.json),
        })
    } else {
        Err(OpError::Execution(text))
    }
}

/// `/agent check|build|inspect|graph|explain|new|fmt|diff|test|run|versions|
/// clean|doctor …` (#22 Welle 2B).
///
/// # Beschreibung
/// Dieselben Befehle wie `harw agent …` im Terminal
/// ([`harw_agent_compiler::commands`]). `build` läuft, wenn die Sitzung
/// eine Job-Verwaltung hat, als Job (`harw agent build … --json` als eigener
/// Prozess; Fortschritt und Ergebnis im Job-Log, `/jobs`); ohne
/// Job-Verwaltung synchron — mit [`FOREGROUND_BUILD_NOTE`] als erster Zeile.
fn agent_compiler(ctx: &OpContext, args: &AgentArgs) -> Result<OpOutput, OpError> {
    let tokens = compiler_tokens(args);
    let command = harw_agent_compiler::parse_tokens(&tokens)
        .map_err(OpError::InvalidArguments)?
        .ok_or_else(|| {
            OpError::InvalidArguments(format!("unknown /agent action '{}'", tokens.join(" ")))
        })?;
    let is_build = matches!(command, harw_agent_compiler::AgentCommand::Build(_));
    if !is_build {
        return run_compiler(ctx, command);
    }
    if let Some(manager) = ctx.service::<std::sync::Arc<harw_tool_job::JobManager>>() {
        return start_build_job(ctx, manager, &tokens);
    }
    tracing::info!("{FOREGROUND_BUILD_NOTE}");
    with_foreground_note(run_compiler(ctx, command))
}

/// Erste Zeile eines `/agent build`, der mangels Job-Verwaltung synchron
/// läuft (etwa außerhalb der TUI).
const FOREGROUND_BUILD_NOTE: &str = "no job system in this session, building in the foreground";

/// Stellt [`FOREGROUND_BUILD_NOTE`] vor den Text eines synchronen Builds —
/// im Erfolg wie im Fehler.
fn with_foreground_note(result: Result<OpOutput, OpError>) -> Result<OpOutput, OpError> {
    let prefix = |text: String| format!("{FOREGROUND_BUILD_NOTE}\n{text}");
    match result {
        Ok(output) => Ok(OpOutput {
            text: prefix(output.text),
            data: output.data,
        }),
        Err(OpError::InvalidArguments(text)) => Err(OpError::InvalidArguments(prefix(text))),
        Err(OpError::Execution(text)) => Err(OpError::Execution(prefix(text))),
        Err(OpError::NotAvailable(text)) => Err(OpError::NotAvailable(prefix(text))),
    }
}

/// Startet `harw agent build … --json` als Job der Sitzung.
fn start_build_job(
    ctx: &OpContext,
    manager: &std::sync::Arc<harw_tool_job::JobManager>,
    tokens: &[String],
) -> Result<OpOutput, OpError> {
    let exe = std::env::current_exe()
        .map_err(|error| OpError::Execution(format!("harw executable unknown: {error}")))?;
    let cwd = ctx.sandbox().workspace().canonical_root().to_path_buf();
    let mut argv: Vec<String> = vec!["agent".to_owned()];
    argv.extend(tokens.iter().cloned());
    argv.push("--json".to_owned());
    let mut command = tokio::process::Command::new(&exe);
    command
        .args(&argv)
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(false);
    #[cfg(unix)]
    command.process_group(0);
    let display = format!("harw {}", argv.join(" "));
    let request = harw_tool_job::StartRequest {
        name: format!("agent build {}", tokens.get(1).map_or("", String::as_str)),
        command: display.clone(),
        cwd: Some(cwd),
        env_keys: Vec::new(),
        notify_every: manager.config().effective_notify_every(None),
        owner: harw_tool_job::JobOwner::new(ctx.session_id().as_str(), Vec::new()),
    };
    let prepared = harw_tool_job::PreparedJob {
        command,
        executed_on_host: true,
    };
    let status = manager
        .start(request, prepared)
        .map_err(|error| OpError::Execution(format!("build job could not start: {error}")))?;
    Ok(OpOutput::from(format!(
        "Build läuft als Job {} ({display}); Fortschritt und Ergebnis: /jobs show {}",
        status.meta.job_id.as_str(),
        status.meta.job_id.as_str()
    )))
}

/// `/agent defs [suchbegriff]`: die startbaren Agentendefinitionen des
/// Laufs (Plan R9, Teil C).
///
/// # Beschreibung
/// Liest denselben Roster wie die Laufzeit
/// ([`harw_registry_defaults::AgentRoster::from_config`]): die eingebauten
/// Rollen und jede gültige benutzerdefinierte Definition aus Profil und
/// vertrautem Projekt — auch die aus `agent.toml` migrierten Agenten, die die
/// alte Liste über `config.agents` nicht mehr zeigte. Je Agent: Name,
/// Organisationsrolle, Herkunft, lesend/schreibend und die Beschreibung.
/// `query` filtert (Groß-/Kleinschreibung egal) nach Name und Beschreibung.
///
/// # Fehler
/// [`OpError::Execution`], wenn die eingebauten Definitionen oder der Roster
/// nicht gebaut werden können.
fn agent_definitions(ctx: &OpContext, query: Option<&str>) -> Result<OpOutput, OpError> {
    let config = crate::provider::resolved_config(ctx)?;
    let agents = config_agents(&config)?;
    let builtin = harw_registry_defaults::embedded_agents::builtin_agent_definitions(
        &agents.executable_agents,
    )
    .map_err(|error| {
        OpError::Execution(format!(
            "eingebaute Agentendefinitionen nicht ladbar: {error}"
        ))
    })?;
    let roster = harw_registry_defaults::AgentRoster::from_config(&builtin, &agents)
        .map_err(|error| OpError::Execution(format!("Agenten-Roster nicht baubar: {error}")))?;
    let compiled = compiled_info(ctx, &roster);
    Ok(OpOutput::from(format_definitions(
        &roster, query, &compiled,
    )))
}

/// Snapshot und Build-Zustand je Agent sowie die kompilierten Agenten in
/// `~/.harw/bin` (#22 Welle 2B), für `/agent defs` und `harw agent list`.
#[derive(Debug, Clone, Default)]
struct CompiledInfo {
    /// Name → Zusatz hinter der Zeile (Snapshot, installierte Version).
    annotations: std::collections::BTreeMap<String, String>,
    /// Zeilen der kompilierten Agenten in `~/.harw/bin`.
    installed: Vec<String>,
}

/// Sammelt [`CompiledInfo`]; ein Fehler (kein Home, defekte Quellen) lässt
/// die Liste ohne Zusätze.
fn compiled_info(ctx: &OpContext, roster: &harw_registry_defaults::AgentRoster) -> CompiledInfo {
    let Ok(env) = compiler_env(ctx) else {
        return CompiledInfo::default();
    };
    let names: Vec<String> = roster.names().map(str::to_owned).collect();
    let snapshots = harw_agent_compiler::commands::definition_snapshots(&env, &names);
    let installed = harw_agent_compiler::commands::installed_agents(&env);
    let mut info = CompiledInfo::default();
    for name in &names {
        let Some((id, snapshot)) = snapshots.get(name) else {
            continue;
        };
        let build = installed
            .iter()
            .find(|agent| agent.definition_id.as_deref() == Some(id.as_str()));
        let state = match build {
            None => "nicht kompiliert".to_owned(),
            Some(agent) if agent.source_snapshot.as_deref() == Some(snapshot.as_str()) => {
                format!(
                    "kompiliert {} (aktuell)",
                    agent.current.as_deref().unwrap_or("-")
                )
            }
            Some(agent) => format!(
                "kompiliert {} (veraltet: Definition geändert)",
                agent.current.as_deref().unwrap_or("-")
            ),
        };
        info.annotations.insert(
            name.clone(),
            format!(
                "snapshot {}; {state}",
                snapshot.chars().take(12).collect::<String>()
            ),
        );
    }
    let bin = env.bin_dir();
    for agent in installed {
        info.installed.push(format!(
            "- {} [kompiliert ({}); {}{}]",
            agent.name,
            bin.display(),
            agent.current.as_deref().unwrap_or("keine aktuelle Version"),
            if agent.auto { "; auto-kompiliert" } else { "" }
        ));
    }
    info
}

/// Die Textform von `/agent defs` (rein, testbar).
///
/// Drei Arten: `eingebaut` (in harw eingebettet), `eigene Definition`
/// (Profil/Projekt) und `kompiliert (~/.harw/bin)`; ein kompilierter Build
/// ersetzt nie die eingebaute Rolle oder die Definition, aus der er stammt.
fn format_definitions(
    roster: &harw_registry_defaults::AgentRoster,
    query: Option<&str>,
    compiled: &CompiledInfo,
) -> String {
    let query = query
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_lowercase);
    let entries: Vec<&harw_registry_defaults::RosterEntry> = roster
        .entries()
        .filter(|entry| {
            query.as_deref().is_none_or(|query| {
                entry.name.to_lowercase().contains(query)
                    || entry
                        .description
                        .as_deref()
                        .is_some_and(|text| text.to_lowercase().contains(query))
            })
        })
        .collect();
    if entries.is_empty() {
        return "Keine passenden Agentendefinitionen.".to_owned();
    }
    let custom = entries.iter().filter(|entry| entry.is_custom()).count();
    let mut lines = vec![format!(
        "{} Agenten ({} eingebaut, {custom} eigene Definition(en)):",
        entries.len(),
        entries.len() - custom
    )];
    for entry in entries {
        let source = match &entry.source {
            harw_registry_defaults::RosterSource::BuiltIn => "eingebaut".to_owned(),
            harw_registry_defaults::RosterSource::Custom { layer, .. } => match layer {
                Some(layer) => format!("eigene Definition, {layer:?}"),
                None => "eigene Definition".to_owned(),
            },
        };
        let description = entry
            .description
            .as_deref()
            .and_then(|text| text.lines().next())
            .unwrap_or("-");
        let annotation = compiled
            .annotations
            .get(&entry.name)
            .map(|annotation| format!(" · {annotation}"))
            .unwrap_or_default();
        lines.push(format!(
            "- {} [{}; {source}; {}] — {description}{annotation}",
            entry.name,
            harw_core::delegation_visibility::role_label(entry.role),
            if entry.read_only {
                "lesend"
            } else {
                "schreibend"
            },
        ));
    }
    if !compiled.installed.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "{} kompilierte Agenten (Kopien; die Quelle bleibt die Definition):",
            compiled.installed.len()
        ));
        lines.extend(compiled.installed.iter().cloned());
    }
    lines.join("\n")
}

/// Rollen, die als Wurzel einer Sitzung starten dürfen (`--agent`,
/// `active_agent_definition`).
const ROOT_CAPABLE_ROLES: [harw_agent_dsl::roles::AgentRoleId; 3] = [
    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
    harw_agent_dsl::roles::AgentRoleId::ChildOrchestrator,
    harw_agent_dsl::roles::AgentRoleId::Worker,
];

/// Aufrufhilfe für `/agent use`.
const AGENT_USE_USAGE: &str = "/agent use <name> | /agent use --clear";

/// `/agent use <name>` bzw. `/agent use --clear`.
///
/// # Beschreibung
/// Mit `--clear` wird `active_agent_definition` aus der Profil-`config.toml`
/// entfernt. Sonst wird `name` gegen die konfigurierten Agentendefinitionen
/// (`ConfigAgents::executable_agents`, Vorrang) und die eingebauten Rollen
/// geprüft ([`validate_root_agent`]) und erst danach gespeichert. Beides
/// wirkt ab der nächsten Sitzung; die laufende Sitzung bleibt unverändert.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: Name fehlt, ist unbekannt oder hat eine
///   Rolle, die nicht als Wurzel starten darf.
/// - [`OpError::Execution`]: Config-Discovery oder das Senken der
///   eingebauten Definitionen schlägt fehl.
fn agent_use(ctx: &OpContext, target: Option<&str>) -> Result<OpOutput, OpError> {
    let target = target.map(str::trim).filter(|target| !target.is_empty());
    let Some(target) = target else {
        return Err(OpError::InvalidArguments(format!(
            "Agentenname fehlt. Aufruf: {AGENT_USE_USAGE}."
        )));
    };
    let persistence = crate::config_util::selection_persistence(ctx);
    let (mut text, note) = if target == "--clear" {
        (
            "Aktiver Agent entfernt – gilt ab nächster Sitzung.".to_owned(),
            persistence.persist_active_agent(None),
        )
    } else {
        let config = crate::provider::resolved_config(ctx)?;
        let agents = config_agents(&config)?;
        let builtin = harw_registry_defaults::embedded_agents::builtin_agent_definitions(
            &agents.executable_agents,
        )
        .map_err(|error| {
            OpError::Execution(format!(
                "eingebaute Agentendefinitionen nicht ladbar: {error}"
            ))
        })?;
        validate_root_agent(target, &agents.executable_agents, &builtin)?;
        (
            format!("Aktiver Agent: {target} – gilt ab nächster Sitzung."),
            persistence.persist_active_agent(Some(target)),
        )
    };
    if let Some(note) = note {
        text.push('\n');
        text.push_str(&note);
    }
    Ok(OpOutput::from(text))
}

/// Senkt die Agentendefinitionen der aufgelösten Konfiguration.
///
/// # Beschreibung
/// `harw-config` reicht die DSL-Definitionen ungeparst weiter
/// (`ResolvedConfig::agent_sources`); gesenkt wird hier, beim Konsumenten
/// ([`harw_registry_defaults::ConfigAgents::from_config`]).
///
/// # Fehler
/// [`OpError::Execution`], wenn eine Definition nicht parst, auflöst oder
/// senkt.
fn config_agents(
    config: &harw_config::ResolvedConfig,
) -> Result<harw_registry_defaults::ConfigAgents, OpError> {
    harw_registry_defaults::ConfigAgents::from_config(config)
        .map_err(|error| OpError::Execution(format!("Agentendefinitionen nicht ladbar: {error}")))
}

/// Prüft, ob `name` eine bekannte, als Wurzel startbare Agentendefinition ist.
///
/// # Beschreibung
/// Konfigurierte Definitionen (`configured`) haben Vorrang vor den
/// eingebauten (`builtin`) — dieselbe Reihenfolge wie
/// `harw-runtime::assembly::resolve_active_agent`. Erlaubt sind nur die
/// Rollen aus [`ROOT_CAPABLE_ROLES`].
///
/// # Fehler
/// [`OpError::InvalidArguments`] mit deutschem Text und der Liste der
/// zulässigen Namen, wenn der Name unbekannt ist oder seine Rolle nicht als
/// Wurzel starten darf.
fn validate_root_agent(
    name: &str,
    configured: &std::collections::HashMap<String, harw_agent_dsl::ExecutableAgentIr>,
    builtin: &std::collections::HashMap<String, harw_agent_dsl::ExecutableAgentIr>,
) -> Result<(), OpError> {
    let known = configured.get(name).or_else(|| builtin.get(name));
    match known {
        Some(ir) if ROOT_CAPABLE_ROLES.contains(&ir.role()) => Ok(()),
        Some(ir) => Err(OpError::InvalidArguments(format!(
            "Agent '{name}' hat die Rolle {:?} und kann nicht als Wurzel starten. \
             Zulässig: {}.",
            ir.role(),
            root_capable_names(configured, builtin)
        ))),
        None => Err(OpError::InvalidArguments(format!(
            "Unbekannter Agent '{name}'. Zulässig: {}.",
            root_capable_names(configured, builtin)
        ))),
    }
}

/// Sortierte, kommagetrennte Liste aller als Wurzel startbaren Namen.
fn root_capable_names(
    configured: &std::collections::HashMap<String, harw_agent_dsl::ExecutableAgentIr>,
    builtin: &std::collections::HashMap<String, harw_agent_dsl::ExecutableAgentIr>,
) -> String {
    let mut names: Vec<&str> = configured
        .iter()
        .chain(
            builtin
                .iter()
                .filter(|(name, _)| !configured.contains_key(*name)),
        )
        .filter(|(_, ir)| ROOT_CAPABLE_ROLES.contains(&ir.role()))
        .map(|(name, _)| name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    names.join(", ")
}

/// Formatiert den Budget-Bericht für `/agent budget`.
///
/// # Beschreibung
/// Reine Darstellungsfunktion ohne Spawner-Zugriff, damit sie ohne
/// admittierte Kinder testbar ist. Der Bericht besteht aus
/// 1. den globalen [`harw_core::ChildLimits`] des Controllers,
/// 2. einer Aggregation je Rolle (Anzahl, summierte Live-Tokens und
///    Tool-Aufrufe) und
/// 3. je Kind dem Budget-Deckel aus `ChildRecord::budget` gegenüber dem
///    Live-Verbrauch aus `ChildRecord::live` (Tokens = Input + Output wie bei
///    der Controller-eigenen Überbudget-Erkennung) und der seit `admitted_at`
///    vergangenen Wanduhrzeit, plus Tiefe gegen die geerbte Tiefendecke.
///
/// # Argumente
/// - `limits` (`&ChildLimits`): globale Admission-Limits des Spawners.
/// - `records` (`&[ChildRecord]`): die anzuzeigenden, besessenen Kinder.
/// - `now` (`jiff::Timestamp`): Referenzzeitpunkt für die Wanduhrzeit.
///
/// # Rückgabe
/// Mehrzeiliger Text; bei leerem `records` nur die Limits und der Hinweis
/// „Keine aktiven Child-Agents.“.
///
/// # Nebenläufigkeit
/// Reine Funktion; kein geteilter Zustand.
fn format_budget_report(
    limits: &harw_core::ChildLimits,
    records: &[harw_core::ChildRecord],
    now: jiff::Timestamp,
) -> String {
    let mut lines = vec![format!(
        "Child-Agent-Limits: max_depth={}, max_active_children_per_parent={}, lease_seconds={}",
        limits.max_depth, limits.max_active_children_per_parent, limits.lease_seconds
    )];
    if records.is_empty() {
        lines.push("Keine aktiven Child-Agents.".to_owned());
        return lines.join("\n");
    }

    let mut per_role: std::collections::BTreeMap<&str, (usize, u64, u64)> =
        std::collections::BTreeMap::new();
    for record in records {
        let entry = per_role.entry(record.role.as_str()).or_insert((0, 0, 0));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(record.live.usage.total());
        entry.2 = entry.2.saturating_add(u64::from(record.live.tool_calls));
    }
    lines.push("Je Rolle:".to_owned());
    for (role, (count, tokens, tool_calls)) in &per_role {
        lines.push(format!(
            "- {role}: {count} Kind(er), tokens={tokens}, tool_calls={tool_calls}"
        ));
    }

    lines.push(format!("{} Child-Agent(s):", records.len()));
    for record in records {
        let elapsed_ms = u64::try_from(now.duration_since(record.admitted_at).as_millis().max(0))
            .unwrap_or(u64::MAX);
        let effort = record.budget.reasoning_effort.map_or_else(
            || "provider-default".to_owned(),
            |effort| effort.to_string(),
        );
        lines.push(format!(
            "- {} (role={}, status={}, depth={}/{})",
            record.child,
            record.role,
            record.status.as_str(),
            record.depth,
            record.depth_ceiling
        ));
        lines.push(format!(
            "  tokens: {}",
            usage_against_limit(record.live.usage.total(), record.budget.max_tokens, "")
        ));
        lines.push(format!(
            "  tool_calls: {}",
            usage_against_limit(
                u64::from(record.live.tool_calls),
                record.budget.max_tool_calls.map(u64::from),
                ""
            )
        ));
        lines.push(format!(
            "  wall_time: {}",
            usage_against_limit(elapsed_ms, record.budget.max_wall_time_ms, "ms")
        ));
        lines.push(format!("  reasoning_effort: {effort}"));
    }
    lines.join("\n")
}

/// Formatiert `used` gegen ein optionales Limit als `used / limit (p%)`.
///
/// # Argumente
/// - `used` (`u64`): bisheriger Verbrauch.
/// - `limit` (`Option<u64>`): Deckel; `None` = unbegrenzt.
/// - `unit` (`&str`): Einheitensuffix für beide Werte (z. B. `"ms"`).
///
/// # Rückgabe
/// `"<used><unit> / unbegrenzt"` ohne Deckel, sonst
/// `"<used><unit> / <limit><unit> (<p>%)"`, bei Überschreitung mit Zusatz
/// `" ÜBERSCHRITTEN"`. Ein Deckel `0` liefert keinen Prozentwert.
fn usage_against_limit(used: u64, limit: Option<u64>, unit: &str) -> String {
    let Some(limit) = limit else {
        return format!("{used}{unit} / unbegrenzt");
    };
    let mut text = if limit == 0 {
        format!("{used}{unit} / {limit}{unit}")
    } else {
        let percent = u128::from(used).saturating_mul(100) / u128::from(limit);
        format!("{used}{unit} / {limit}{unit} ({percent}%)")
    };
    if used > limit {
        text.push_str(" ÜBERSCHRITTEN");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::AgentArgs;
    use crate::config_util::RecordedSelectionPersistCall;
    use crate::test_support::ctx as ctx_err;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_registry_defaults::role_names::{ROOT_ORCHESTRATOR, UIA_WORKER};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_context() -> TestResult<(OpContext, std::path::PathBuf)> {
        test_context_with(ServiceMap::new())
    }

    fn test_context_with(services: ServiceMap) -> TestResult<(OpContext, std::path::PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-agent-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
        ))
    }

    #[test]
    fn test_agent_args_from_raw_args_sets_action() -> TestResult {
        let args = AgentArgs::from_raw_args(&toks(&["list"]));
        match args {
            Ok(a) => {
                assert_eq!(a.action.as_deref(), Some("list"));
                assert!(a.target.is_none());
                assert!(a.value.is_none());
            }
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[test]
    fn test_agent_args_from_raw_args_stop_preserves_target() -> TestResult {
        let args = AgentArgs::from_raw_args(&toks(&["stop", "abc-42"]));
        match args {
            Ok(a) => {
                assert_eq!(a.action.as_deref(), Some("stop"));
                assert_eq!(a.target.as_deref(), Some("abc-42"));
                assert!(a.value.is_none());
            }
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[test]
    fn test_agent_args_from_raw_args_budget_preserves_target_and_value() -> TestResult {
        let args = AgentArgs::from_raw_args(&toks(&["budget", "abc-42", "8k"]));
        match args {
            Ok(a) => {
                assert_eq!(a.action.as_deref(), Some("budget"));
                assert_eq!(a.target.as_deref(), Some("abc-42"));
                assert_eq!(a.value.as_deref(), Some("8k"));
            }
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[test]
    fn test_agent_args_from_raw_args_empty_tokens_sets_action_none() -> TestResult {
        let args = AgentArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => {
                assert!(a.action.is_none());
                assert!(a.target.is_none());
                assert!(a.value.is_none());
            }
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn agent_default_and_list_return_not_available() -> TestResult {
        let (ctx, root) = test_context()?;
        let expected = "child-agent management is not available";
        assert!(matches!(
            super::agent(&ctx, AgentArgs::default()).await,
            Err(OpError::NotAvailable(message)) if message == expected
        ));
        assert!(matches!(
            super::agent(
                &ctx,
                AgentArgs {
                    action: Some("list".to_owned()),
                    target: None,
                    value: None,
                    rest: None,
                },
            )
            .await,
            Err(OpError::NotAvailable(message)) if message == expected
        ));
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;
        Ok(())
    }

    #[tokio::test]
    async fn agent_budget_does_not_leak_target_or_value() -> TestResult {
        let (ctx, root) = test_context()?;
        let action = "budget";
        let target = "sensitive-agent-id";
        let value = "secret-budget";
        let result = super::agent(
            &ctx,
            AgentArgs {
                action: Some(action.to_owned()),
                target: Some(target.to_owned()),
                value: Some(value.to_owned()),
                rest: None,
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert_eq!(message, "child-agent management is not available");
                assert!(!message.contains(action));
                assert!(!message.contains(target));
                assert!(!message.contains(value));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn agent_list_with_spawner_service_reports_no_active_children() -> TestResult {
        use harw_core::{ChildLimits, ManagedAgentSpawner, SessionManager};
        use harw_operations::context::ServiceMap;
        use std::sync::{Arc, Mutex};

        let (ctx, root) = test_context()?;
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(event_tx)));
        let spawner = Arc::new(ManagedAgentSpawner::new(
            manager,
            ChildLimits::conservative(),
        ));

        let mut services = ServiceMap::new();
        services.insert(spawner);
        let ctx = OpContext::new(
            ctx.session_id().clone(),
            ctx.turn_id().clone(),
            ctx.sandbox().clone(),
            services,
        );

        let result = super::agent(&ctx, AgentArgs::default()).await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        match result {
            Ok(output) => assert!(output.text.contains("Keine aktive")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Ok empty listing, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn agent_stop_with_spawner_service_and_unknown_target_is_not_available() -> TestResult {
        use harw_core::{ChildLimits, ManagedAgentSpawner, SessionManager};
        use harw_operations::context::ServiceMap;
        use std::sync::{Arc, Mutex};

        let (ctx, root) = test_context()?;
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(event_tx)));
        let spawner = Arc::new(ManagedAgentSpawner::new(
            manager,
            ChildLimits::conservative(),
        ));

        let mut services = ServiceMap::new();
        services.insert(spawner);
        let ctx = OpContext::new(
            ctx.session_id().clone(),
            ctx.turn_id().clone(),
            ctx.sandbox().clone(),
            services,
        );

        let result = super::agent(
            &ctx,
            AgentArgs {
                action: Some("stop".to_owned()),
                target: Some("unknown-child".to_owned()),
                value: None,
                rest: None,
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        assert!(matches!(result, Err(OpError::NotAvailable(_))));
        Ok(())
    }

    fn context_with_spawner() -> TestResult<(OpContext, std::path::PathBuf)> {
        use harw_core::{ChildLimits, ManagedAgentSpawner, SessionManager};
        use std::sync::{Arc, Mutex};

        let (ctx, root) = test_context()?;
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(event_tx)));
        let spawner = Arc::new(ManagedAgentSpawner::new(
            manager,
            ChildLimits::conservative(),
        ));
        let mut services = ServiceMap::new();
        services.insert(spawner);
        Ok((
            OpContext::new(
                ctx.session_id().clone(),
                ctx.turn_id().clone(),
                ctx.sandbox().clone(),
                services,
            ),
            root,
        ))
    }

    fn record(
        child: &str,
        role: &str,
        budget: harw_core::AgentBudget,
        tokens: (u64, u64),
        tool_calls: u32,
        admitted_at: jiff::Timestamp,
    ) -> harw_core::ChildRecord {
        harw_core::ChildRecord {
            child: SessionId::from_str(child),
            parent: SessionId::from_str("parent-1"),
            handoff_call_id: harw_types::ToolCallId::from_str(format!("call-{child}")),
            role: role.to_owned(),
            depth: 1,
            admitted_at,
            lease_expires_at: admitted_at,
            budget,
            allow_pause: false,
            depth_ceiling: 3,
            trace: None,
            status: harw_core::child_controller::ChildStatus::Running,
            task_complexity: None,
            model: None,
            provider: None,
            consumed: harw_core::child_controller::ChildUsage::default(),
            charged_to_parent: harw_core::child_controller::ChildUsage::default(),
            live: harw_core::child_controller::ChildLiveStats {
                usage: harw_types::TokenUsage {
                    input_tokens: tokens.0,
                    output_tokens: tokens.1,
                    ..harw_types::TokenUsage::default()
                },
                tool_calls,
            },
        }
    }

    #[test]
    fn usage_against_limit_formats_unlimited_percent_and_exceeded() -> TestResult {
        assert_eq!(super::usage_against_limit(5, None, ""), "5 / unbegrenzt");
        assert_eq!(
            super::usage_against_limit(25, Some(100), ""),
            "25 / 100 (25%)"
        );
        assert_eq!(
            super::usage_against_limit(1500, Some(1000), "ms"),
            "1500ms / 1000ms (150%) ÜBERSCHRITTEN"
        );
        assert_eq!(
            super::usage_against_limit(1, Some(0), ""),
            "1 / 0 ÜBERSCHRITTEN"
        );
        Ok(())
    }

    #[test]
    fn format_budget_report_without_children_shows_limits_only() -> TestResult {
        let report = super::format_budget_report(
            &harw_core::ChildLimits::conservative(),
            &[],
            jiff::Timestamp::UNIX_EPOCH,
        );
        assert!(
            report.contains("max_depth=4, max_active_children_per_parent=8, lease_seconds=900")
        );
        assert!(report.contains("Keine aktiven Child-Agents."));
        assert!(!report.contains("Je Rolle"));
        Ok(())
    }

    #[test]
    fn format_budget_report_shows_per_child_budget_live_usage_and_role_totals() -> TestResult {
        let admitted = jiff::Timestamp::from_second(1_000).map_err(ctx("admitted timestamp"))?;
        let now = jiff::Timestamp::from_second(1_012).map_err(ctx("now timestamp"))?;
        let capped = harw_core::AgentBudget {
            max_tokens: Some(1_000),
            max_tool_calls: Some(4),
            max_wall_time_ms: Some(10_000),
            reasoning_effort: Some(harw_types::ReasoningEffort::High),
        };
        let records = vec![
            record("child-a", "explorer", capped, (200, 50), 2, admitted),
            record(
                "child-b",
                "explorer",
                harw_core::AgentBudget::default(),
                (100, 0),
                7,
                admitted,
            ),
            record(
                "child-c",
                "worker",
                harw_core::AgentBudget::default(),
                (0, 0),
                0,
                admitted,
            ),
        ];
        let report =
            super::format_budget_report(&harw_core::ChildLimits::conservative(), &records, now);

        assert!(report.contains("- explorer: 2 Kind(er), tokens=350, tool_calls=9"));
        assert!(report.contains("- worker: 1 Kind(er), tokens=0, tool_calls=0"));
        assert!(report.contains("3 Child-Agent(s):"));
        assert!(report.contains("- child-a (role=explorer, status=running, depth=1/3)"));
        assert!(report.contains("  tokens: 250 / 1000 (25%)"));
        assert!(report.contains("  tool_calls: 2 / 4 (50%)"));
        assert!(report.contains("  wall_time: 12000ms / 10000ms (120%) ÜBERSCHRITTEN"));
        assert!(report.contains("  reasoning_effort: high"));
        assert!(report.contains("  tokens: 100 / unbegrenzt"));
        assert!(report.contains("  tool_calls: 7 / unbegrenzt"));
        assert!(report.contains("  reasoning_effort: provider-default"));
        Ok(())
    }

    #[tokio::test]
    async fn agent_budget_with_spawner_and_no_children_reports_limits() -> TestResult {
        let (ctx, root) = context_with_spawner()?;
        let result = super::agent(
            &ctx,
            AgentArgs {
                action: Some("budget".to_owned()),
                target: None,
                value: None,
                rest: None,
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;
        match result {
            Ok(output) => {
                assert!(output.text.contains("Child-Agent-Limits: max_depth=4"));
                assert!(output.text.contains("Keine aktiven Child-Agents."));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Ok budget report, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn agent_budget_adjustment_with_spawner_is_not_available_and_does_not_leak() -> TestResult
    {
        let (ctx, root) = context_with_spawner()?;
        let result = super::agent(
            &ctx,
            AgentArgs {
                action: Some("budget".to_owned()),
                target: Some("sensitive-agent-id".to_owned()),
                value: Some("secret-budget".to_owned()),
                rest: None,
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert_eq!(message, "child-agent budget adjustment is not available");
                assert!(!message.contains("sensitive-agent-id"));
                assert!(!message.contains("secret-budget"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn agent_budget_with_unowned_target_is_not_available() -> TestResult {
        let (ctx, root) = context_with_spawner()?;
        let result = super::agent(
            &ctx,
            AgentArgs {
                action: Some("budget".to_owned()),
                target: Some("unknown-child".to_owned()),
                value: None,
                rest: None,
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert_eq!(
                    message,
                    "agent target is unavailable in this parent session"
                );
                assert!(!message.contains("unknown-child"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── /agent use ──────────────────────────────────────────────────────────

    type Recorder = std::sync::Arc<crate::config_util::RecordingSelectionPersistence>;

    /// Kontext mit leerer Konfiguration und aufzeichnender Persistenz, damit
    /// `/agent use` weder `HARW_HOME` liest noch schreibt.
    fn use_context() -> TestResult<(OpContext, std::path::PathBuf, Recorder)> {
        let recorder: Recorder =
            std::sync::Arc::new(crate::config_util::RecordingSelectionPersistence::new());
        let persistence: std::sync::Arc<dyn crate::config_util::SelectionPersistence> =
            recorder.clone();
        let mut services = ServiceMap::new();
        services.insert(persistence);
        services.insert(std::sync::Arc::new(harw_config::ResolvedConfig::default()));
        let (ctx, root) = test_context_with(services)?;
        Ok((ctx, root, recorder))
    }

    fn use_args(target: Option<&str>) -> AgentArgs {
        AgentArgs {
            action: Some("use".to_owned()),
            target: target.map(str::to_owned),
            value: None,
            rest: None,
        }
    }

    #[test]
    fn test_agent_args_from_raw_args_use_clear() -> TestResult {
        let args = AgentArgs::from_raw_args(&toks(&["use", "--clear"]))
            .map_err(|e| TestError::Unexpected(format!("Unerwarteter Fehler: {e}")))?;
        assert_eq!(args.action.as_deref(), Some("use"));
        assert_eq!(args.target.as_deref(), Some("--clear"));
        Ok(())
    }

    #[tokio::test]
    async fn agent_use_persists_builtin_root_agent_without_spawner() -> TestResult {
        let (ctx, root, recorder) = use_context()?;
        let result = super::agent(&ctx, use_args(Some(ROOT_ORCHESTRATOR))).await;
        std::fs::remove_dir_all(root).map_err(ctx_err("remove test workspace"))?;
        let output = result.map_err(|e| TestError::Unexpected(format!("{e:?}")))?;
        let text = output.text;
        assert!(text.contains("gilt ab nächster Sitzung"), "{text}");
        assert_eq!(
            recorder.calls(),
            vec![RecordedSelectionPersistCall::ActiveAgent {
                name: Some(ROOT_ORCHESTRATOR.to_owned()),
            }]
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_use_clear_removes_the_selection() -> TestResult {
        let (ctx, root, recorder) = use_context()?;
        let result = super::agent(&ctx, use_args(Some("--clear"))).await;
        std::fs::remove_dir_all(root).map_err(ctx_err("remove test workspace"))?;
        let output = result.map_err(|e| TestError::Unexpected(format!("{e:?}")))?;
        assert!(
            output.text.contains("gilt ab nächster Sitzung"),
            "{}",
            output.text
        );
        assert_eq!(
            recorder.calls(),
            vec![RecordedSelectionPersistCall::ActiveAgent { name: None }]
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_use_rejects_unknown_missing_and_non_root_agents() -> TestResult {
        let (ctx, root, recorder) = use_context()?;
        let unknown = super::agent(&ctx, use_args(Some("gibt-es-nicht"))).await;
        let missing = super::agent(&ctx, use_args(None)).await;
        let non_root = super::agent(&ctx, use_args(Some(UIA_WORKER))).await;
        std::fs::remove_dir_all(root).map_err(ctx_err("remove test workspace"))?;

        assert!(
            matches!(&unknown, Err(OpError::InvalidArguments(message))
                if message.contains("Unbekannter Agent") && message.contains(ROOT_ORCHESTRATOR)),
            "{unknown:?}"
        );
        assert!(
            matches!(&missing, Err(OpError::InvalidArguments(message))
                if message.contains("/agent use")),
            "{missing:?}"
        );
        assert!(
            matches!(&non_root, Err(OpError::InvalidArguments(message))
                if message.contains("nicht als Wurzel")),
            "{non_root:?}"
        );
        assert!(
            recorder.calls().is_empty(),
            "ungültige Namen dürfen nichts speichern"
        );
        Ok(())
    }

    /// Plan R9, Teil C: `/agent defs` zeigt den Roster — eingebaute und
    /// benutzerdefinierte Agenten mit Rolle, Herkunft und Beschreibung (die
    /// alte Liste über `config.agents` kannte die migrierten nicht).
    #[test]
    fn agent_defs_lists_builtin_and_custom_agents() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx_err("tempdir"))?;
        let dir = home.path().join("agents").join("zettel-sammler");
        std::fs::create_dir_all(&dir).map_err(ctx_err("Agentenordner"))?;
        std::fs::write(
            dir.join("definition.toml"),
            r#"schema = "harwness.agent/v1"
id = "user.agent.zettel-sammler@1"
version = "1.0.0"
extends = { id = "harwness.agent.worker-base@1" }
role = "worker"
specialization = "zettel-sammler"
description = "Sammelt Zettelkasten-Einträge"

[tools]
admitted = ["fs.read"]
"#,
        )
        .map_err(ctx_err("definition.toml"))?;
        let config = harw_config::discover_config(&[home.path().to_path_buf()])
            .map_err(ctx_err("Discovery"))?;
        let agents = super::config_agents(&config).map_err(ctx_err("Agenten senken"))?;
        let builtin = harw_registry_defaults::embedded_agents::builtin_agent_definitions(
            &agents.executable_agents,
        )
        .map_err(ctx_err("eingebaute Rollen"))?;
        let roster = harw_registry_defaults::AgentRoster::from_config(&builtin, &agents)
            .map_err(ctx_err("Roster"))?;

        let text = super::format_definitions(&roster, None, &super::CompiledInfo::default());
        assert!(
            text.contains("- explorer [worker; eingebaut; lesend] — "),
            "{text}"
        );
        assert!(
            text.contains("- zettel-sammler [worker; eigene Definition"),
            "{text}"
        );
        assert!(text.contains("Sammelt Zettelkasten-Einträge"), "{text}");

        let filtered =
            super::format_definitions(&roster, Some("ZETTEL"), &super::CompiledInfo::default());
        assert!(
            filtered.starts_with("1 Agenten (0 eingebaut, 1 eigene Definition(en)):"),
            "{filtered}"
        );
        assert_eq!(
            super::format_definitions(
                &roster,
                Some("gibt-es-nicht-xyz"),
                &super::CompiledInfo::default()
            ),
            "Keine passenden Agentendefinitionen."
        );

        // #22: Snapshot, Build-Zustand und die kompilierten Agenten.
        let mut compiled = super::CompiledInfo::default();
        compiled.annotations.insert(
            "zettel-sammler".to_owned(),
            "snapshot 0123456789ab; kompiliert 1.0.0-abc (aktuell)".to_owned(),
        );
        compiled.installed.push(
            "- harw-uia-terminal-ui [kompiliert (/h/bin); 1.0.0-x; auto-kompiliert]".to_owned(),
        );
        let annotated = super::format_definitions(&roster, Some("zettel"), &compiled);
        assert!(
            annotated.contains("Zettelkasten-Einträge · snapshot 0123456789ab; kompiliert"),
            "{annotated}"
        );
        assert!(annotated.contains("1 kompilierte Agenten"), "{annotated}");
        assert!(annotated.contains("auto-kompiliert"), "{annotated}");
        Ok(())
    }

    /// `/agent build` ohne Job-Verwaltung läuft weiter synchron, aber die
    /// erste Zeile sagt das — im Erfolg wie im Fehler.
    #[test]
    fn test_foreground_build_output_starts_with_the_note() -> TestResult {
        let ok = super::with_foreground_note(Ok(harw_operations::OpOutput {
            text: "built".to_owned(),
            data: None,
        }))
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
        let mut lines = ok.text.lines();
        assert_eq!(lines.next(), Some(super::FOREGROUND_BUILD_NOTE));
        assert_eq!(lines.next(), Some("built"));
        assert!(
            super::FOREGROUND_BUILD_NOTE
                .contains("no job system in this session, building in the foreground")
        );
        match super::with_foreground_note(Err(OpError::Execution("failed".to_owned()))) {
            Err(OpError::Execution(text)) => {
                assert!(text.starts_with(super::FOREGROUND_BUILD_NOTE), "{text}");
                assert!(text.ends_with("failed"), "{text}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an execution error, got {other:?}"
                )));
            }
        }
        Ok(())
    }
}
