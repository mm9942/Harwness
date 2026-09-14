//! Standalone `harw` process entrypoint.
//!
//! Ohne Subcommand startet `harw` den interaktiven ratatui-Chat (`chat`),
//! nachdem der Root-Space `~/.harw` sichergestellt und — beim Erststart — der
//! Onboarding-Wizard (`onboarding`) durchlaufen wurde. Die Subcommands
//! (`init`, `onboard`, `doctor`, `serve`, `web`, `project`, `classify`, `run`)
//! decken Einrichtung, Validierung, den MCP-Listener, die Web-Oberfläche,
//! Projekt-Trust und Bootstrap-Pfade ab. `harw serve` fährt bei SIGTERM/SIGINT
//! geordnet herunter ([`serve_until`]); sein Job-Worker läuft auf einem eigenen
//! Thread mit eigener Tokio-Runtime, damit blockierende Store-I/O den
//! MCP-Listener nicht aushungert. Die
//! Argument-Grammatik lebt in `cli` (clap); Hilfe erscheint nur bei
//! `--help`/`-h`.

#![forbid(unsafe_code)]

// jemalloc nur mit Cargo-Feature `jemalloc` (Vorgabe aus, G-067): auf
// 16K-Seiten-Kerneln (RPi 5) ist ein 4K-gebautes jemalloc absturzgefährdet.
#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod auth;
mod chat;
mod cli;
mod completion;
mod gateway;
mod home;
mod job_worker;
mod lifecycle;
mod mcp_auth;
mod observe;
mod onboarding;
mod project_trust;
mod resume;
mod runtime_entry;
mod runtime_gateway;
mod runtime_jobs;
mod runtime_web;
mod secret_store;
mod settings;
mod web;
mod worker_cancellation;
mod uia_bootstrap;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use harw_config::{
    McpJobCapabilityToml, OriginAllowlistToml, PlanSection, ProviderToml, ResolvedConfig,
    SecretRef, discover_config,
};
use harw_core::{
    InteractionMode, JobExecutionRegistry, ModelMessage, ModelProvider, TurnInput, TurnOutcome,
    run_turn,
};
use harw_extension_api::ContextProvider;
use harw_mcp_server::{
    BoundMcpListener, DurableMcpSupervisor, McpAuthenticator, McpEventBus, McpJobCapability,
    McpListenerConfig, McpPrincipal, McpSupervisor, PrincipalRegistry, StaticBearerAuthenticator,
};
use harw_operations::OpInput;
use harw_plan::goal::{Goal, GoalAction, GoalId, GoalStatus, GoalStore};
use harw_plan::types::{Criterion, VerificationStep};
use harw_plan::{
    FileGoalStore, FilePlanStore, InMemoryGoalStore, InMemoryPlanStore, PlanAction, PlanId,
    PlanNodeKind, PlanStore, PlanToolConfig,
};
use harw_plan_bridge::{FindingStore, GoalContextProvider, offset_from_timestamp};
use harw_protocol::SessionEvent;
use harw_provider_http::SecretResolver;
use harw_runtime::{EntryKind, ModelSource, RuntimeSpec, RuntimeStores, ServiceSurface};
use harw_session_store::JobStore;
use harw_tui::classify_input;
use harw_types::{
    ApprovalActor, IngressSurface, PermissionTier, SessionId, TenantId, ThreadRef, TurnId,
    WorkspaceId,
};
use tokio::runtime::Builder;
use worker_cancellation::RegistryWorkerCancellationSink;

use cli::{AnalyzeArgs, Cli, Command};

use std::sync::atomic::{AtomicBool, Ordering};

/// Latest configuration revision understood by this CLI.
///
/// Revision 1 introduces the durable `config_version` marker.  It does not
/// require a TOML transformation beyond recording the marker, so the
/// installer runner receives no per-field migration steps yet.  Keeping the
/// revision here makes future schema migrations an explicit CLI startup
/// concern rather than allowing config consumers to interpret stale files.
const LATEST_CONFIG_VERSION: u32 = 1;

/// Global flag set by [`init_tracing`] when `--log-sensitive` is active.
///
/// `Relaxed` ordering is sufficient because the flag is written once before any
/// additional threads start (the Tokio runtime is created later) and is
/// thereafter only read.
static LOG_SENSITIVE: AtomicBool = AtomicBool::new(false);

/// Returns `true` when the current process was started with `--log-sensitive`.
///
/// # Description
///
/// Other modules that need to gate sensitive content (prompts, tool arguments,
/// model responses) behind the debug flag call this function instead of
/// reading `cli.log_sensitive` directly. The value is set once by
/// [`init_tracing`] before any async tasks start, so callers never observe
/// a stale `false`.
///
/// # Returns
///
/// `true` if `--log-sensitive` was passed on the command line, `false`
/// otherwise.
///
/// # Concurrency
///
/// Lock-free; safe to call from any thread or async task after
/// [`init_tracing`] has returned.
///
/// # Examples
///
/// ```rust,no_run
/// # fn example(user_text: &str) {
/// if harw_cli::log_sensitive_enabled() {
///     tracing::debug!(prompt = %user_text, "full prompt logged");
/// } else {
///     tracing::debug!(prompt_bytes = user_text.len(), "prompt content redacted");
/// }
/// # }
/// ```
pub fn log_sensitive_enabled() -> bool {
    LOG_SENSITIVE.load(Ordering::Relaxed)
}

/// Initializes the global `tracing` subscriber for the binary.
///
/// # Description
///
/// Must be called exactly once, as the very first statement after argument
/// parsing. Libraries must **never** call this function — subscriber
/// installation is the binary's exclusive responsibility.
///
/// The subscriber uses `tracing_subscriber::EnvFilter`, which accepts the same
/// directive syntax as `RUST_LOG` (e.g. `"debug"`,
/// `"harw_core=trace,info"`). If `level` cannot be parsed, the filter falls
/// back to `"info"` so the process always starts with a usable subscriber.
///
/// When `log_sensitive` is `true` the global [`LOG_SENSITIVE`] flag is set and
/// a `warn!` event is emitted immediately after subscriber installation to
/// remind operators that sensitive data may appear in logs.
///
/// # Arguments
///
/// - `level` (`&str`): tracing filter directive passed verbatim to
///   [`tracing_subscriber::EnvFilter::try_new`].
/// - `log_sensitive` (`bool`): when `true`, enables [`log_sensitive_enabled`]
///   and emits a startup warning.
///
/// # Panics
///
/// Panics if a global subscriber has already been installed (only possible if
/// this function is called twice, which is a programming error).
fn init_tracing(level: &str, log_sensitive: bool, tui_active: bool) {
    use tracing_subscriber::EnvFilter;
    // Standardmäßig auf `warn` reduzieren, damit die interaktive TUI (Alternate
    // Screen) nicht durch Info-Spans überschrieben wird. `--log info` bleibt
    // explizit möglich, wenn Nutzer:innen tiefere Traces wollen.
    let effective = if level.eq_ignore_ascii_case("info") {
        "warn"
    } else {
        level
    };
    let filter = EnvFilter::try_new(effective).unwrap_or_else(|_| EnvFilter::new("warn"));
    // Im Alternate-Screen ist auch STDERR sichtbar. Ein `fmt`-Layer darf in
    // der TUI daher gar nicht installiert werden: Ein Sink-Writer schützt nur
    // diesen einen Layer, nicht spätere Writer/Layers. Die reine Registry
    // behält den Filter und die Trace-Metadaten für Instrumentierung, formatiert
    // aber *keine* Ereignisse in das Terminal.
    if tui_active {
        use tracing_subscriber::prelude::*;
        tracing_subscriber::registry().with(filter).init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_writer(std::io::stderr)
            .init();
    }
    if log_sensitive {
        LOG_SENSITIVE.store(true, Ordering::Relaxed);
        tracing::warn!(
            "--log-sensitive is enabled: prompts, tool-args and responses will be logged. \
             Do NOT use in production."
        );
    }
}

fn main() {
    let cli = Cli::parse();
    let tui_active = cli.command.is_none() && cli.chat.prompt.is_none();
    init_tracing(&cli.chat.log, cli.chat.log_sensitive, tui_active);
    let code = match dispatch(cli) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("harw: {error}");
            2
        }
    };
    std::process::exit(code);
}

/// Routet den geparsten Command; ohne Subcommand startet der Chat.
fn dispatch(cli: Cli) -> Result<(), String> {
    let home_override = cli.chat.home.clone();
    // `--mode`/`--goal` sind Root-Flags: sie gelten für den Chat-Einstieg *und*
    // für `analyze`. Vor dem `match` kopiert, damit der Teil-Move von
    // `cli.command` und `cli.chat` sie nicht unerreichbar macht.
    let requested_mode = cli.mode.clone();
    let requested_goal = cli.goal.clone();
    run_startup_migrations(&cli.command, home_override.clone())?;

    match cli.command {
        None => {
            // Modus und Ziel werden *vor* dem Chat aufgelöst: ein unbekannter
            // Modusname darf keine Session starten, und ein `--goal` muss im
            // Goal-Store stehen, bevor der erste Turn Kontext einsammelt.
            // Die Spec dient hier nur dem Config-Laden (Home, cwd, Repo-Trust);
            // die eigentliche Montage baut `chat::run_chat`.
            let (entry, surface) = if cli.chat.prompt.is_some() {
                (EntryKind::OneShot, IngressSurface::Cli)
            } else {
                (EntryKind::Tui, IngressSurface::Tui)
            };
            let spec = local_runtime_spec(home_override.clone(), entry, surface)?;
            let startup = prepare_planning_startup(
                &spec,
                requested_mode.as_deref(),
                requested_goal.as_deref(),
            )?;
            // Dieselben Store-Instanzen weiterreichen, nicht neue öffnen: zwei
            // Schreiber auf einem Plan-Verzeichnis wären stiller Datenverlust.
            // Die Freigabepolitik aus `[policy]` baut die Montage selbst.
            let chat_startup = chat::ChatStartup {
                mode: startup.mode,
                plan: startup.services.to_runtime(),
                goal_context: startup.goal_context,
            };
            chat::run_chat(
                home_override,
                cli.chat.prompt,
                cli.chat.resume,
                chat_startup,
                chat::ChatOptions {
                    all_projects: cli.chat.all,
                    verbose: cli.chat.verbose,
                    add_dirs: cli.chat.add_dir,
                },
            )
        }
        Some(Command::Init) => cmd_init(home_override),
        Some(Command::Onboard) => {
            let home = home::resolve_home(home_override)?;
            harw_home::ensure_home(&home).map_err(|error| error.to_string())?;
            onboarding::run_wizard(&home)
        }
        Some(Command::Doctor { config_dir }) => {
            let layers = resolve_layers(home_override.clone(), config_dir.clone())?;
            doctor(layers, home_override.clone(), config_dir)?;
            lifecycle::health(home_override)
        }
        Some(Command::Gateway { telemetry }) => gateway::run(home_override, telemetry),
        Some(Command::Serve { config_dir }) => {
            let (layers, storage_root, home) = resolve_serve_paths(home_override, config_dir)?;
            serve_mcp(layers, storage_root, home)
        }
        Some(Command::Web { socket }) => {
            // `harw web` kennt kein `--config-dir` mehr: der Root-Space kommt
            // ausschließlich aus `--home` bzw. `HARW_HOME` (siehe `crate::web`).
            let home = web_home(home::resolve_home(home_override))?;
            web::serve_web(Some(home), socket)
        }
        Some(Command::Project { action }) => project_trust::run(home_override, action),
        Some(Command::Settings { action }) => settings::run(home_override, action),
        Some(Command::Classify { input }) => {
            let text = input.join(" ");
            println!(
                "{:?}",
                classify_input(&text).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        Some(Command::Run { input }) => {
            let home = home::resolve_home(home_override)?;
            harw_home::ensure_home(&home).map_err(|error| error.to_string())?;
            println!("{}", run_local_echo(&input.join(" "), &home)?);
            Ok(())
        }
        Some(Command::Auth { action }) => auth::run(home_override, action),
        Some(Command::Completion { shell }) => completion::print_completion(shell),
        Some(Command::Update { check }) => lifecycle::update(home_override, check),
        Some(Command::Service { action }) => lifecycle::service(home_override, action),
        Some(Command::Catalog { refresh }) => lifecycle::catalog(home_override, refresh),
        Some(Command::Uninstall {
            scope,
            dry_run,
            yes,
        }) => lifecycle::uninstall(home_override, &scope, dry_run, yes),
        Some(Command::Analyze(args)) => cmd_analyze(
            home_override,
            requested_mode.as_deref(),
            requested_goal.as_deref(),
            &args,
        ),
    }
}

/// Applies configuration migrations before commands which load configuration.
///
/// Clap handles `--help` and `--version` before [`dispatch`] is reached.
/// Completion output and commands that do not load configuration deliberately
/// skip this function, so those paths remain free of migration writes.
fn run_startup_migrations(
    command: &Option<Command>,
    home_override: Option<PathBuf>,
) -> Result<(), String> {
    let layers = match command {
        // `analyze` liest `[tools.plan]`, `[mode]` und `[policy]` — es gehört
        // damit zu den Pfaden, die vor dem Lesen migrieren müssen.
        None
        | Some(Command::Onboard)
        | Some(Command::Gateway { .. })
        | Some(Command::Settings { .. })
        | Some(Command::Analyze(_)) => {
            let home = home::resolve_home(home_override)?;
            harw_home::ensure_home(&home).map_err(|error| error.to_string())?;
            harw_home::config_layers(&home).map_err(|error| error.to_string())?
        }
        Some(Command::Web { .. }) => {
            let home = web_home(home::resolve_home(home_override))?;
            harw_home::ensure_home(&home).map_err(|error| error.to_string())?;
            harw_home::config_layers(&home).map_err(|error| error.to_string())?
        }
        Some(Command::Doctor { config_dir }) | Some(Command::Serve { config_dir }) => {
            match config_dir {
                Some(dir) => vec![dir.clone()],
                None => {
                    let home = home::resolve_home(home_override)?;
                    harw_home::ensure_home(&home).map_err(|error| error.to_string())?;
                    harw_home::config_layers(&home).map_err(|error| error.to_string())?
                }
            }
        }
        Some(
            Command::Init
            | Command::Classify { .. }
            | Command::Run { .. }
            | Command::Completion { .. }
            | Command::Update { .. }
            | Command::Service { .. }
            | Command::Catalog { .. }
            | Command::Auth { .. }
            | Command::Project { .. }
            | Command::Uninstall { .. },
        ) => return Ok(()),
    };

    let config_paths = layers
        .iter()
        .map(|layer| layer.join("config.toml"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    migrate_config_paths(&config_paths)
}

/// Resolves the mandatory HARW home of `harw web` and names the flag on failure.
///
/// `harw web` has no `--config-dir` fallback (CONTRACTS-W2d2 §1.3): without a
/// resolvable home there is neither a trust report nor an approval store. The
/// resolution result is passed in (instead of calling
/// [`home::resolve_home`] here) so the error path is testable without
/// touching process environment variables.
///
/// # Errors
/// Returns the resolver's error prefixed with a hint to `--home` / `HARW_HOME`.
fn web_home(resolved: Result<PathBuf, String>) -> Result<PathBuf, String> {
    resolved.map_err(|error| {
        format!("harw web requires a HARW home (--home or HARW_HOME): {error}")
    })
}

/// Runs the installer-owned migration runner for each discovered config layer.
///
/// The runner owns idempotence and backup numbering.  A failure is surfaced
/// with the affected config path so startup never continues with a stale or
/// only partially migrated configuration.
fn migrate_config_paths(config_paths: &[PathBuf]) -> Result<(), String> {
    let runner = harw_install::MigrationRunner::new(
        Vec::<Box<dyn harw_install::ConfigMigration>>::new(),
        LATEST_CONFIG_VERSION,
    );
    for config_path in config_paths {
        runner.run(config_path).map_err(|error| {
            format!(
                "configuration migration failed for {}: {error}",
                config_path.display()
            )
        })?;
    }
    Ok(())
}

/// Legt den Root-Space an (idempotent) und meldet, was neu erstellt wurde.
fn cmd_init(home_override: Option<PathBuf>) -> Result<(), String> {
    let home = home::resolve_home(home_override)?;
    let report = harw_home::ensure_home(&home).map_err(|error| error.to_string())?;
    if report.created_home {
        println!("Root-Space angelegt: {}", report.home.display());
    } else {
        println!("Root-Space vorhanden: {}", report.home.display());
    }
    println!("aktives Profil: {}", report.profile_dir.display());
    println!("neu geschriebene Dateien: {}", report.written_files.len());

    let cwd = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;
    let project = harw_home::project::discover_project(&cwd, &[])
        .map_err(|error| error.to_string())?;
    if !project.root.join(".git").exists() {
        let status = ProcessCommand::new("git")
            .arg("init")
            .current_dir(&project.root)
            .status()
            .map_err(|error| format!("could not run git init in {}: {error}", project.root.display()))?;
        if !status.success() {
            return Err(format!("git init failed in {} with status {status}", project.root.display()));
        }
        println!("Git-Repository angelegt: {}", project.root.display());
    }
    let project_home = harw_home::project::ProjectHome::at(&project);
    project_home.ensure().map_err(|error| error.to_string())?;
    println!("Harw-Projektzustand angelegt: {}", project_home.dir.display());
    Ok(())
}

/// Baut die Config-Layer für `doctor`: expliziter `--config-dir` gewinnt, sonst
/// die Home-Layer (Root-Space wird dabei idempotent sichergestellt).
fn resolve_layers(
    home_override: Option<PathBuf>,
    config_dir: Option<PathBuf>,
) -> Result<Vec<PathBuf>, String> {
    if let Some(dir) = config_dir {
        return Ok(vec![dir]);
    }
    let home = home::resolve_home(home_override)?;
    harw_home::ensure_home(&home).map_err(|error| error.to_string())?;
    harw_home::config_layers(&home).map_err(|error| error.to_string())
}

/// Wie [`resolve_layers`], liefert zusätzlich den Storage-Root für `serve`.
/// [`JobStore`] hängt sein `jobs`-Verzeichnis selbst an diesen Root an. Bei
/// normalem Harw-Home ist der Root das aktive Profil, damit Job-Records und
/// Transkripte gemeinsam unter `profiles/<active>/jobs` liegen. Bei externem
/// Config-Verzeichnis ist es dieses Verzeichnis selbst. Ein explizites `--home`
/// bleibt dort ausschließlich der Root für den sealed Secret Store.
pub(crate) fn resolve_serve_paths(
    home_override: Option<PathBuf>,
    config_dir: Option<PathBuf>,
) -> Result<(Vec<PathBuf>, PathBuf, Option<PathBuf>), String> {
    let layers = resolve_layers(home_override.clone(), config_dir.clone())?;
    if let Some(dir) = config_dir {
        return Ok((layers, dir, home_override));
    }
    let home = home::resolve_home(home_override)?;
    let active_profile = harw_home::paths::active_profile_name(&home);
    let storage_root =
        harw_home::paths::profile_dir(&home, &active_profile).map_err(|error| error.to_string())?;
    Ok((layers, storage_root, Some(home)))
}

fn serve_mcp(
    layers: Vec<PathBuf>,
    storage_root: PathBuf,
    home: Option<PathBuf>,
) -> Result<(), String> {
    let config = discover_config(&layers).map_err(|error| error.to_string())?;
    config.validate().map_err(|error| error.to_string())?;

    if !config.harness.mcp_listener.enabled {
        return Err(
            "mcp_listener.enabled is false in configuration; not starting the MCP endpoint"
                .to_owned(),
        );
    }

    require_home_for_sealed_refs(&config, home.as_deref())?;
    let secret_resolver = open_serve_secret_resolver(&config, home.as_deref())?;
    let authenticator = build_authenticator(
        &config,
        secret_resolver
            .as_ref()
            .map(|resolver| resolver as &dyn SecretResolver),
    )?;
    let principals = build_principal_registry(&config)?;
    // Prompt-Jobs laufen nur für aktuell konfigurierte MCP-Principals
    // (`job_worker::check_prompt_claim_scope`).
    let configured_submitters = Arc::new(crate::runtime_jobs::configured_principal_ids(&config));
    let address = config
        .harness
        .mcp_listener
        .listen_addr
        .parse()
        .map_err(|error| {
            format!(
                "invalid mcp_listener.listen_addr '{}': {error}",
                config.harness.mcp_listener.listen_addr
            )
        })?;
    let store = Arc::new(JobStore::new(&storage_root));
    let executions = Arc::new(JobExecutionRegistry::new());
    let provider: Arc<dyn ModelProvider> =
        build_serve_provider(&config, home.as_deref(), secret_resolver.as_ref())?.into();
    let transcript_root = storage_root.join("jobs").join("transcripts");
    std::fs::create_dir_all(&transcript_root)
        .map_err(|error| format!("could not create job transcript root: {error}"))?;
    let supervisor: Arc<dyn McpSupervisor> =
        Arc::new(DurableMcpSupervisor::with_worker_cancellation_sink(
            Arc::clone(&store),
            Arc::new(RegistryWorkerCancellationSink::new(Arc::clone(&executions))),
        ));

    // Plan-Dienste für `plan-node`-Jobs. Ohne sie bliebe die Kette
    // `admit_ready_nodes → Job → on_job_completed` wirkungslos. Der Projekt-Root
    // ist das Arbeitsverzeichnis des Dienstes — dagegen löst der Worker die
    // Pfade des Mutationsvertrags auf.
    let plan_node_services = match home.as_deref() {
        Some(home_path) => {
            let plan_config = plan_tool_config_from_section(&config.harness.tools.plan)?;
            let project_root =
                std::env::current_dir().map_err(|error| error.to_string())?;
            build_plan_node_services(home_path, &plan_config, &project_root)?
        }
        // Ohne HARW-Home gibt es kein `plans/`-Verzeichnis. `plan-node`-Jobs
        // enden dann sichtbar als blockiert, statt still übersprungen zu werden.
        None => None,
    };
    tracing::info!(
        plan_node_services = plan_node_services.is_some(),
        "serve.job_worker.plan_services"
    );

    // Jeder Job montiert seine eigene `RuntimeAssembly` unter diesem Home und
    // dem Arbeitsverzeichnis des Dienstes. Ohne Home enden Jobs sichtbar als
    // blockiert (E3, `job_worker::JobWorkerContext::runtime_root`).
    let runtime_root = match home.as_deref() {
        Some(home_path) => Some(job_worker::JobRuntimeRoot {
            home: home_path.to_path_buf(),
            cwd: std::env::current_dir().map_err(|error| error.to_string())?,
        }),
        None => None,
    };
    let worker_context = Arc::new(job_worker::JobWorkerContext {
        transcript_root,
        configured_submitters,
        runtime_root,
    });

    // Zwei Runtimes (G-054): der Listener behält seine `current_thread`-Runtime
    // auf dem Hauptthread; der Job-Worker bekommt eine eigene auf einem eigenen
    // Thread. Beide werden vor dem Binden gebaut, damit ein Runtime-Fehler den
    // Start abbricht, statt einen Listener ohne Worker laufen zu lassen.
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let worker_runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not build job worker runtime: {error}"))?;

    let (listener, signals) = runtime.block_on(async {
        // Signale vor dem Binden registrieren: ab dem ersten Accept beendet
        // SIGTERM den Prozess nicht mehr hart (G-022).
        let signals = lifecycle::ShutdownSignals::install()?;
        let event_bus = Arc::new(McpEventBus::with_default_capacity());
        let listener = BoundMcpListener::bind_with_supervisor_and_events(
            McpListenerConfig {
                address,
                max_sessions: 128,
                session_ttl: jiff::SignedDuration::from_secs(900),
            },
            authenticator,
            principals,
            Some(supervisor),
            Some(event_bus),
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok::<_, String>((listener, signals))
    })?;
    eprintln!(
        "harw MCP listening on http://{}{}",
        listener.local_addr().map_err(|error| error.to_string())?,
        config.harness.mcp_listener.path
    );

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let worker = spawn_job_worker_thread(worker_runtime, move || {
        job_worker::run_job_worker(
            store,
            executions,
            provider,
            plan_node_services,
            shutdown_rx,
            worker_context,
        )
    })?;

    let ServeOutcome {
        listener: listener_result,
        worker: worker_stop,
    } = runtime.block_on(serve_until(
        |listener_shutdown| listener.serve_until(listener_shutdown),
        async {
            let reason = signals.wait().await;
            tracing::info!(signal = reason.signal_name(), "serve.shutdown.requested");
        },
        &shutdown_tx,
        worker.done,
        WORKER_SHUTDOWN_GRACE,
    ));

    let worker_result = match worker_stop {
        WorkerStop::Finished | WorkerStop::Vanished => worker
            .handle
            .join()
            .map_err(|_| "job worker thread panicked".to_owned()),
        WorkerStop::TimedOut => {
            // Der Thread wird nicht gejoint: `main` beendet den Prozess gleich
            // mit `std::process::exit`. Laufende Jobs bleiben `Running`, bis
            // ihr Lease abläuft.
            tracing::warn!(
                grace_secs = WORKER_SHUTDOWN_GRACE.as_secs(),
                "serve.shutdown.worker_timed_out"
            );
            Err(format!(
                "job worker did not stop within {}s after shutdown",
                WORKER_SHUTDOWN_GRACE.as_secs()
            ))
        }
    };
    tracing::info!(worker = ?worker_stop, "serve.shutdown.complete");

    listener_result?;
    worker_result
}

/// Upper bound for the job worker to observe the shutdown flag after the MCP
/// listener has stopped (G-022). Below systemd's default `TimeoutStopSec=90s`.
const WORKER_SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Job worker running on its own OS thread and Tokio runtime (G-054).
struct JobWorkerThread {
    /// Joined once `done` resolved; left detached on timeout.
    handle: std::thread::JoinHandle<()>,
    /// Fires when the worker future returned; dropped unsent on panic.
    done: tokio::sync::oneshot::Receiver<()>,
}

/// Spawns `make_worker()` on a dedicated thread driven by `runtime`.
///
/// # Description
/// Blocking work inside the job worker (fs4 locks, fsync, store listing) then
/// only stalls this thread, never the MCP listener's runtime (G-054). The
/// runtime is built by the caller so its failure aborts startup instead of
/// silently running a listener without a worker.
///
/// # Errors
/// When the OS refuses to spawn the thread.
///
/// # Concurrency
/// The closure and its captures move to the new thread (`Send + 'static`); the
/// worker future itself is created and polled only there.
fn spawn_job_worker_thread<F, Fut>(
    runtime: tokio::runtime::Runtime,
    make_worker: F,
) -> Result<JobWorkerThread, String>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()>,
{
    let (done_tx, done) = tokio::sync::oneshot::channel();
    let handle = std::thread::Builder::new()
        .name("harw-job-worker".to_owned())
        .spawn(move || {
            runtime.block_on(make_worker());
            // `Err` only means `serve_until` already gave up waiting (timeout);
            // there is nobody left to notify.
            if done_tx.send(()).is_err() {
                tracing::debug!("serve.job_worker.finished_after_grace");
            }
        })
        .map_err(|error| format!("could not spawn job worker thread: {error}"))?;
    Ok(JobWorkerThread { handle, done })
}

/// How the job worker ended during [`serve_until`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerStop {
    /// The worker future returned.
    Finished,
    /// The completion sender was dropped without sending (worker panicked).
    Vanished,
    /// The worker did not finish within the grace period.
    TimedOut,
}

/// Result of [`serve_until`]: listener outcome plus how the worker stopped.
#[derive(Debug)]
struct ServeOutcome {
    /// `Err` when the listener failed or the worker died while serving.
    listener: Result<(), String>,
    /// Worker termination observed after the listener stopped.
    worker: WorkerStop,
}

/// Serves until `shutdown` resolves, then stops listener and worker in order.
///
/// # Description
/// 1. Starts `listener` with a receiver of `shutdown_tx`.
/// 2. Waits for the first of: the listener ending on its own (error), the
///    external `shutdown` future (signal), or the worker ending unexpectedly.
/// 3. Sets the shared watch flag to `true` — the listener stops accepting and
///    aborts its connections, the worker leaves its poll loop — and awaits the
///    listener.
/// 4. Waits at most `grace` for `worker_done`.
///
/// # Arguments
/// - `listener`: builds the serving future from a shutdown receiver
///   (production: `BoundMcpListener::serve_until`).
/// - `shutdown`: resolves when shutdown is requested (production: signals;
///   tests: a oneshot).
/// - `shutdown_tx` (`&watch::Sender<bool>`): the channel the worker already
///   watches.
/// - `worker_done`: completion notification of the worker thread.
/// - `grace` (`Duration`): cap on waiting for the worker.
///
/// # Returns
/// [`ServeOutcome`]; never waits unbounded for the worker.
///
/// # Concurrency
/// Runs on the listener runtime; the worker runs elsewhere and is only
/// observed through the watch/oneshot channels.
async fn serve_until<L, LFut, S>(
    listener: L,
    shutdown: S,
    shutdown_tx: &tokio::sync::watch::Sender<bool>,
    mut worker_done: tokio::sync::oneshot::Receiver<()>,
    grace: Duration,
) -> ServeOutcome
where
    L: FnOnce(tokio::sync::watch::Receiver<bool>) -> LFut,
    LFut: Future<Output = std::io::Result<()>>,
    S: Future<Output = ()>,
{
    let serving = listener(shutdown_tx.subscribe());
    tokio::pin!(serving);
    tokio::pin!(shutdown);

    let mut worker_early = None;
    let listener_result = tokio::select! {
        result = &mut serving => result.map_err(|error| error.to_string()),
        () = &mut shutdown => {
            shutdown_tx.send_replace(true);
            serving.await.map_err(|error| error.to_string())
        }
        early = &mut worker_done => {
            tracing::error!("serve.job_worker.stopped_while_serving");
            worker_early = Some(match early {
                Ok(()) => WorkerStop::Finished,
                Err(_) => WorkerStop::Vanished,
            });
            shutdown_tx.send_replace(true);
            match serving.await {
                Ok(()) => Err("job worker stopped unexpectedly while serving".to_owned()),
                Err(error) => Err(error.to_string()),
            }
        }
    };
    // Idempotent: covers the listener ending on its own.
    shutdown_tx.send_replace(true);

    let worker = match worker_early {
        Some(stop) => stop,
        None => match tokio::time::timeout(grace, worker_done).await {
            Ok(Ok(())) => WorkerStop::Finished,
            Ok(Err(_)) => WorkerStop::Vanished,
            Err(_) => WorkerStop::TimedOut,
        },
    };
    ServeOutcome {
        listener: listener_result,
        worker,
    }
}

/// Baut den Provider für `harw serve`.
///
/// # Arguments
/// - `home` (`Option<&Path>`): aktives harw-Home. Liegt es vor, werden
///   `file:`/`file-json:`-Credentials (von `harw onboard` und `harw-oauth`
///   erzeugt) unterhalb von `<home>/secrets/` aufgelöst; ohne Home schlagen
///   sie fail-closed fehl (`build_provider`).
fn build_serve_provider(
    config: &ResolvedConfig,
    home: Option<&Path>,
    resolver: Option<&secret_store::ConfiguredSecretResolver>,
) -> Result<Box<dyn ModelProvider>, String> {
    let resolver = resolver.map(|resolver| resolver as &dyn SecretResolver);
    match home {
        Some(home) => harw_provider_http::build_provider_with_home(config, home, resolver),
        None => match resolver {
            Some(resolver) => harw_provider_http::build_provider_with_resolver(config, resolver),
            None => harw_provider_http::build_provider(config),
        },
    }
    .map_err(|error| format!("could not construct configured model provider: {error}"))
}

fn require_home_for_sealed_refs(
    config: &ResolvedConfig,
    home: Option<&Path>,
) -> Result<(), String> {
    if home.is_none() && configured_serve_uses_sealed_secret(config) {
        return Err(
            "enabled provider or MCP principal using sealed secrets requires HARW_HOME; --config-dir does not identify a sealed-secret store"
                .to_owned(),
        );
    }
    Ok(())
}

fn configured_serve_uses_sealed_secret(config: &ResolvedConfig) -> bool {
    config
        .providers
        .values()
        .any(|provider| provider.enabled && matches!(&provider.auth, Some(SecretRef::Secrets(_))))
        || config
            .harness
            .mcp_listener
            .principals
            .iter()
            .any(|principal| matches!(&principal.credential_ref, SecretRef::Secrets(_)))
}

/// Opens the single sealed-secret resolver shared by provider and MCP auth.
/// The secret-store module intentionally keys opening off providers; a
/// synthetic provider in a separate config view extends that decision to
/// MCP-only `secrets:` credentials without changing the loaded configuration.
fn open_serve_secret_resolver(
    config: &ResolvedConfig,
    home: Option<&Path>,
) -> Result<Option<secret_store::ConfiguredSecretResolver>, String> {
    let Some(home) = home else {
        return Ok(None);
    };
    if config
        .providers
        .values()
        .any(|provider| provider.enabled && matches!(&provider.auth, Some(SecretRef::Secrets(_))))
    {
        return secret_store::open_configured_secret_resolver(home, config);
    }
    if !config
        .harness
        .mcp_listener
        .principals
        .iter()
        .any(|principal| matches!(&principal.credential_ref, SecretRef::Secrets(_)))
    {
        return Ok(None);
    }

    let mut resolver_config = ResolvedConfig {
        auth: config.auth.clone(),
        ..Default::default()
    };
    resolver_config.providers.insert(
        "__mcp_secret_resolver__".to_owned(),
        ProviderToml {
            name: "__mcp_secret_resolver__".to_owned(),
            api: "openai-compatible".to_owned(),
            base_url: "https://invalid.local".to_owned(),
            auth: Some(SecretRef::Secrets("__mcp_secret_resolver__".to_owned())),
            auth_header: None,
            api_key: None,
            headers: std::collections::HashMap::new(),
            models: Vec::new(),
            enabled: true,
            origin_allowlist: OriginAllowlistToml::default(),
        },
    );
    secret_store::open_configured_secret_resolver(home, &resolver_config)
}

/// Resolves each configured principal into the workspace authority the
/// transport enforces. `principal_key` is the authenticator-issued identity
/// (the principal's configured `id`); the submitter actor granted `*Own`
/// capabilities is the same `id`, matching the local single-operator model.
///
/// # Errors
/// Fails closed when a principal `id` is configured more than once
/// ([`PrincipalRegistry::try_insert`]); startup must abort instead of serving
/// with an ambiguous identity.
fn build_principal_registry(config: &ResolvedConfig) -> Result<PrincipalRegistry, String> {
    let mut registry = PrincipalRegistry::new();
    for principal in &config.harness.mcp_listener.principals {
        let capabilities = principal
            .job_capabilities
            .iter()
            .map(|capability| match capability {
                McpJobCapabilityToml::SubmitOwn => McpJobCapability::SubmitOwn,
                McpJobCapabilityToml::ReadOwn => McpJobCapability::ReadOwn,
                McpJobCapabilityToml::ReadWorkspace => McpJobCapability::ReadWorkspace,
                McpJobCapabilityToml::CancelOwn => McpJobCapability::CancelOwn,
                McpJobCapabilityToml::CancelWorkspace => McpJobCapability::CancelWorkspace,
            })
            .collect::<Vec<_>>();
        registry
            .try_insert(
                principal.id.clone(),
                McpPrincipal::from_trusted_ingress(
                    ApprovalActor::Operator {
                        id: principal.id.clone(),
                    },
                    TenantId::from_str(principal.tenant.clone()),
                    WorkspaceId::from_str(principal.workspace.clone()),
                    capabilities,
                ),
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(registry)
}

/// Resolves every configured principal's credential and builds the
/// server's bearer authenticator. Fails closed: any principal whose
/// credential cannot be resolved aborts startup rather than serving with a
/// partial or silently-excluded principal set.
fn build_authenticator(
    config: &ResolvedConfig,
    resolver: Option<&dyn SecretResolver>,
) -> Result<Arc<dyn McpAuthenticator>, String> {
    let mut entries = Vec::with_capacity(config.harness.mcp_listener.principals.len());
    for principal in &config.harness.mcp_listener.principals {
        let credential =
            mcp_auth::resolve_mcp_credential_with_resolver(&principal.credential_ref, resolver)
                .map_err(|error| format!("principal '{}': {error}", principal.id))?;
        entries.push((principal.id.clone(), credential));
    }
    Ok(Arc::new(StaticBearerAuthenticator::new(entries)))
}

/// Runs one complete local harness turn with the intentionally non-networked
/// bootstrap provider. This makes the CLI exercise the real runtime assembly
/// (`EntryKind::LocalEcho`), session FSM, history persistence seam, and turn
/// loop without silently claiming to be a production model integration.
///
/// # Errors
/// Returns a `String` when the working directory is unreadable, the profile
/// session storage cannot be prepared, the runtime assembly or root session
/// cannot be built, the turn fails or pauses, or no assistant text is produced.
fn run_local_echo(input: &str, home: &Path) -> Result<String, String> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start local runtime: {error}"))?;

    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let sessions_root = runtime_entry::profile_sessions_root(home)?;
    let state_store = runtime_entry::transcript_state_store(&sessions_root, run_thread_for_session);
    let spec = runtime_entry::runtime_spec(
        EntryKind::LocalEcho,
        home,
        &cwd,
        runtime_entry::local_principal(IngressSurface::Cli),
    );
    let assembly = runtime_entry::build_assembly(
        spec,
        ModelSource::Echo(format!("echo: {input}")),
        RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        },
        None,
    )?;

    runtime.block_on(async {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel();
        let root_session_id = assembly.root_session_id().clone();
        let mut session = assembly
            .new_root_session(root_session_id.clone(), event_tx, turn_tx, None)
            .map_err(|error| format!("could not create the local root session: {error}"))?
            .session;

        let outcome = run_turn(
            &mut session,
            assembly.model().as_ref(),
            assembly.state_store().as_ref(),
            TurnInput::user(input),
        )
        .await;
        assembly.close_session(&root_session_id);
        match outcome.map_err(|error| error.to_string())? {
            TurnOutcome::Completed => {}
            TurnOutcome::AwaitingChild { .. } | TurnOutcome::AwaitingApproval { .. } => {
                return Err("local echo provider unexpectedly paused a turn".to_owned());
            }
            // Terminale Ausgänge: beim Echo-Provider ebenso unerwartet wie eine
            // Pause, aber mit eigenem Grund in der Meldung.
            TurnOutcome::Cancelled { reason } => {
                return Err(format!("local echo turn was cancelled: {reason:?}"));
            }
            TurnOutcome::Truncated => {
                return Err("local echo turn was truncated".to_owned());
            }
            TurnOutcome::Refused { detail } => {
                return Err(format!("local echo turn was refused: {detail:?}"));
            }
            TurnOutcome::Failed { reason } => {
                return Err(format!("local echo turn failed: {reason}"));
            }
        }

        session
            .history()
            .to_model_messages()
            .into_iter()
            .rev()
            .find_map(|message| match message {
                ModelMessage::Assistant { text } => Some(text),
                _ => None,
            })
            .ok_or_else(|| "local echo provider produced no assistant response".to_owned())
    })
}

/// Maps every local `run` session to a stable, ingress-specific transcript
/// thread. The session ID is generated once by the core and remains unchanged
/// for the lifetime of the durable transcript.
fn run_thread_for_session(session_id: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("cli-run-session:{}", session_id.as_str()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Planungsfläche — Composition-Root (AP W5-08)
//
// Bis hierher existierten Plan-Store, Goal-Store, `FindingStore`,
// `GoalContextProvider` und die sechs Planungs-Operationen unabhängig
// voneinander. Dieser Abschnitt ist die einzige Stelle, die sie zusammensetzt:
// eine `PlanToolConfig` speist **gleichzeitig** die Plan-/Goal-/Finding-Stores
// und die `OperationRegistry` (Ops), sodass registrierte Operationen und
// vorhandene Dienste nicht auseinanderlaufen können.
// ─────────────────────────────────────────────────────────────────────────────

/// Verzeichnisname des voreingestellten Plan-Space unterhalb von
/// [`harw_home::paths::plans_dir`].
pub(crate) const DEFAULT_PLAN_SPACE: &str = "default";

/// Verzeichnisname des voreingestellten Goal-Space unterhalb von
/// [`harw_home::paths::goals_dir`].
pub(crate) const DEFAULT_GOAL_SPACE: &str = "default";

/// Bezeichner des Ziels, das `--goal` beim Start anlegt.
const STARTUP_GOAL_ID: &str = "cli-startup";

/// Bezeichner des Plans, an den `--goal` bindet, falls noch keiner existiert.
const STARTUP_PLAN_ID: &str = "plan-cli";

/// Akteur-Bezeichner für Mutationen, die von der Kommandozeile ausgehen.
///
/// Das Präfix `human:` ist bindend: `harw_plan::goal::validate_goal_action`
/// weist jeden Akteur mit `model:`-Präfix von `SetStatus{Achieved|Abandoned}`
/// zurück. `harw_ops::plan::CallSurface::Command` bildet auf genau dasselbe
/// Präfix ab (`actor_prefix` → `"human"`), deshalb bleibt ein später über
/// `/goal achieve` gestellter Antrag auf demselben Ziel zulässig.
const CLI_ACTOR: &str = "human:cli";

/// Akteur, unter dem der Job-Worker Plan-Mutationen einträgt.
///
/// Bewusst **weder** `human:` **noch** `model:`: ein Job ist keins von beidem.
/// Er ist eine Maschine, die einen bereits genehmigten Knoten abarbeitet. Das
/// `model:`-Präfix wäre irreführend (kein Modell hat den Job angefordert) und
/// `human:` wäre eine Rechteanmaßung — es würde dem Worker erlauben, ein Ziel
/// auf `Achieved` zu setzen. Ein eigenes Präfix fällt in beide Sperren nicht
/// hinein und bleibt in der Plan-Historie als das erkennbar, was es ist.
const PLAN_NODE_JOB_ACTOR: &str = "job:plan-node-worker";

/// Aufzählung der gültigen Interaktionsmodi für Fehlermeldungen.
const VALID_MODE_NAMES: &str = "chat, plan, explore, work";

/// Löst den Interaktionsmodus einer neuen Session auf.
///
/// # Description
///
/// `--mode` gewinnt vor `[mode] default`. Ein unbekannter Name ist in **beiden**
/// Fällen ein Fehler und niemals ein stiller Rückfall auf `chat`: ein Tippfehler
/// würde sonst eine Session mit deutlich weiteren Rechten starten, als der
/// Aufrufer verlangt hat. [`InteractionMode::parse`] normalisiert Groß-/
/// Kleinschreibung, Bindestriche und umgebende Leerzeichen.
///
/// # Arguments
///
/// - `requested` (`Option<&str>`): Wert von `--mode`, geliehen.
/// - `configured` (`&str`): Wert von `[mode] default`, geliehen.
///
/// # Returns
///
/// Den typisierten [`InteractionMode`].
///
/// # Errors
///
/// Ein `String`, der den abgelehnten Namen und die vollständige Liste der
/// gültigen Modi nennt.
///
/// # Examples
///
/// `resolve_startup_mode(Some("explore"), "chat")` ergibt
/// [`InteractionMode::Explore`]; `resolve_startup_mode(None, "plan")` ergibt
/// [`InteractionMode::Plan`]; `resolve_startup_mode(Some("wörk"), "chat")`
/// ergibt einen Fehler, der `chat, plan, explore, work` nennt.
fn resolve_startup_mode(
    requested: Option<&str>,
    configured: &str,
) -> Result<InteractionMode, String> {
    match requested {
        Some(raw) => InteractionMode::parse(raw).ok_or_else(|| {
            format!("unbekannter Interaktionsmodus '{raw}'; gültig sind: {VALID_MODE_NAMES}")
        }),
        None => InteractionMode::parse(configured).ok_or_else(|| {
            format!(
                "[mode] default = '{configured}' ist kein bekannter Interaktionsmodus; \
                 gültig sind: {VALID_MODE_NAMES}"
            )
        }),
    }
}

/// Übersetzt einen Knotenart-Namen aus `[tools.plan]` in [`PlanNodeKind`].
///
/// # Description
///
/// `harw-config` darf laut Modul-Doku von `plan_toml.rs` nicht auf `harw-plan`
/// zeigen und hält die Knotenarten deshalb als `String`. Die Übersetzung ist
/// ausdrücklich Aufgabe des Konsumenten, der beide Crates kennt — also dieser
/// Composition-Root. Die Namen sind die `serde`-Repräsentation von
/// [`PlanNodeKind`] (`snake_case`).
///
/// # Arguments
///
/// - `name` (`&str`): der konfigurierte Knotenart-Name, geliehen.
///
/// # Returns
///
/// Die passende [`PlanNodeKind`].
///
/// # Errors
///
/// Ein `String` mit dem abgelehnten Namen und der vollständigen Werteliste.
fn parse_plan_node_kind(name: &str) -> Result<PlanNodeKind, String> {
    // Weder Zuordnung noch Liste stehen hier noch von Hand: `PlanNodeKind`
    // leitet `KebabEnum` ab und liefert damit `FromStr` und `ALL`. Vorher stand
    // die Liste an dieser Stelle **zweimal** — einmal als `match` und einmal als
    // Fließtext in der Fehlermeldung darunter. Eine neue Variante hätte an
    // beiden Stellen unbemerkt gefehlt.
    name.parse::<PlanNodeKind>().map_err(|_| {
        let allowed = PlanNodeKind::ALL
            .iter()
            .map(PlanNodeKind::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "tools.plan.require_exploration_for enthält unbekannte Knotenart \
             '{name}'; erlaubt sind: {allowed}"
        )
    })
}

/// Übersetzt die deklarative `[tools.plan]`-Sektion in eine [`PlanToolConfig`].
///
/// # Description
///
/// Genau **eine** so gebaute Konfiguration geht anschließend sowohl in die
/// Plan-, Goal- und Finding-Stores ([`build_plan_services`]) als auch in die
/// `OperationRegistry` (über [`harw_ops::register_plan_tools`]). Beide aus
/// derselben Quelle zu speisen ist der Kern dieses Moduls: eine registrierte
/// Operation ohne passenden Store — oder ein Store ohne Operationen — wäre der
/// schwerste Fehler dieser Datei.
///
/// [`PlanSection::validate`] läuft zuerst, damit Tippfehler in `max_nodes`,
/// `max_expand_depth` und `require_exploration_for` vor jeder Store-Erzeugung
/// auffallen.
///
/// # Arguments
///
/// - `section` (`&PlanSection`): die geladene `[tools.plan]`-Sektion, geliehen.
///
/// # Returns
///
/// Die [`PlanToolConfig`], die Stores **und** Operationen gemeinsam regiert.
///
/// # Errors
///
/// Ein `String` mit der Begründung aus [`PlanSection::validate`] oder aus
/// [`parse_plan_node_kind`].
pub(crate) fn plan_tool_config_from_section(section: &PlanSection) -> Result<PlanToolConfig, String> {
    section.validate()?;
    let require_exploration_for = section
        .require_exploration_for
        .iter()
        .map(|name| parse_plan_node_kind(name.as_str()))
        .collect::<Result<Vec<PlanNodeKind>, String>>()?;

    Ok(PlanToolConfig {
        enabled: section.enabled,
        persist: section.persist,
        require_for_complex_work: section.require_for_complex_work,
        validate_dependency_cycles: section.validate_dependency_cycles,
        validate_write_conflicts: section.validate_write_conflicts,
        max_nodes: section.max_nodes,
        require_exploration_for,
        exploration_ttl_secs: section.exploration_ttl_secs,
        max_expand_depth: section.max_expand_depth,
    })
}

/// Die vollständige Plan-Ausstattung einer Laufzeit.
///
/// # Description
///
/// `plan`/`goal`/`findings` sind bei abgeschalteter Planungsfläche alle `None`
/// — nie nur ein Teil. Ein Aufrufer, der `plan`/`goal` mit `None` sieht, weiß
/// damit auch, dass [`PlanServices::to_runtime`] `None` liefert.
///
/// # Concurrency
///
/// Die Stores sind `Arc<dyn …>` über `Send + Sync`-Implementierungen und
/// werden nur geklont bzw. verschoben.
pub(crate) struct PlanServices {
    /// Derselbe Plan-Store, den die Montage über [`PlanServices::to_runtime`]
    /// bekommt — für Startup-Mutationen.
    ///
    /// `pub(crate)`, weil `crate::web` dieselben Stores braucht, um pro
    /// Web-Aufruf eine frische [`harw_runtime::PlanServices`] zu bauen — siehe
    /// `crate::web`-Moduldoku.
    pub(crate) plan: Option<Arc<dyn PlanStore>>,
    /// Derselbe Goal-Store — für Startup-Mutationen.
    pub(crate) goal: Option<Arc<dyn GoalStore>>,
    /// Derselbe Finding-Store.
    ///
    /// Wird gehalten, damit [`PlanServices::to_runtime`] der Montage
    /// **dieselben** Instanzen gibt, statt zweite Stores auf demselben
    /// Verzeichnis zu öffnen — zwei Schreiber auf einem Plan-Verzeichnis wären
    /// stiller Datenverlust.
    pub(crate) findings: Option<Arc<FindingStore>>,
    /// Dieselbe Konfiguration, die auch die Operationen registriert hat.
    ///
    /// Sie hier mitzuführen ist der Grund, warum Stores und Operationen nicht
    /// auseinanderlaufen können: es gibt genau eine `PlanToolConfig` pro Start.
    pub(crate) config: PlanToolConfig,
}

impl PlanServices {
    /// Reicht die Stores als [`harw_runtime::PlanServices`] an eine Montage weiter.
    ///
    /// # Description
    /// Alle drei Stores oder keiner: eine halb geöffnete Planungsfläche wird nie
    /// an [`harw_runtime::RuntimeAssemblyBuilder::plan_services`] gereicht. Es
    /// werden **keine** neuen Stores erzeugt — die `Arc`s zeigen auf dieselben
    /// Instanzen wie `self.plan`/`self.goal`/`self.findings`; zwei Schreiber
    /// auf einem Plan-Verzeichnis wären stiller Datenverlust.
    ///
    /// # Returns
    /// `Some(_)`, wenn Plan-, Goal- **und** Finding-Store vorliegen; sonst `None`.
    ///
    /// # Concurrency
    /// Klont nur `Arc`-Zeiger und die Konfiguration.
    #[must_use]
    pub(crate) fn to_runtime(&self) -> Option<harw_runtime::PlanServices> {
        Some(harw_runtime::PlanServices {
            plan: Arc::clone(self.plan.as_ref()?),
            goal: Arc::clone(self.goal.as_ref()?),
            findings: Arc::clone(self.findings.as_ref()?),
            plan_config: self.config.clone(),
        })
    }
}

/// Baut die Plan-Dienste einer Laufzeit.
///
/// # Description
///
/// Ist `[tools.plan] enabled = false`, entsteht **nichts**: keine Stores, keine
/// Dienste, und ausdrücklich auch kein leeres Verzeichnis auf der Platte. Ein
/// abgeschaltetes Werkzeug darf keine Spur hinterlassen, sonst wirkt es beim
/// nächsten Blick ins Dateisystem, als sei es benutzt worden.
///
/// Ist die Fläche aktiv, entscheidet `persist` über die Bauart:
/// - `persist = true` → [`FilePlanStore`] unter `plans_dir(home)/<plan_space>`
///   und [`FileGoalStore`] unter `goals_dir(home)/<goal_space>`. Plans und Goals
///   liegen getrennt, weil ein Ziel Plan-Revisionen überlebt und unabhängig
///   löschbar bleiben muss.
/// - `persist = false` → [`InMemoryPlanStore`] und [`InMemoryGoalStore`]; auch
///   dieser Pfad legt kein Verzeichnis an.
///
/// Der [`FindingStore`] wurzelt immer auf `plans_dir(home)` (nicht auf dem
/// Plan-Space): er schlüsselt seine Artefakte selbst nach Plan-ID
/// (`<root>/<plan_id>/research/<question_id>.md`) und legt das Verzeichnis erst
/// beim ersten Schreiben an.
///
/// Alle drei Stores gehen als `Some` oder alle drei als `None` in das
/// zurückgegebene [`PlanServices`] — es gibt keinen beobachtbaren
/// Zwischenzustand mit halber Ausstattung.
///
/// # Arguments
///
/// - `home` (`&Path`): aufgelöster Root-Space, geliehen.
/// - `config` (`&PlanToolConfig`): die Konfiguration, die auch die Operationen
///   regiert; geliehen und für den Store geklont.
/// - `plan_space` (`&str`): Verzeichnisname des Plan-Space unterhalb von
///   `plans_dir(home)`.
/// - `goal_space` (`&str`): Verzeichnisname des Goal-Space unterhalb von
///   `goals_dir(home)`.
///
/// # Returns
///
/// [`PlanServices`] — bei abgeschalteter Fläche mit `None`-Stores.
///
/// # Errors
///
/// Ein `String`, wenn ein persistenter Store sein Wurzelverzeichnis nicht
/// öffnen kann oder einen vorhandenen Stand nicht gegen die Konfiguration
/// validieren konnte.
///
/// # Concurrency
///
/// Rein synchron; benötigt keinen exklusiven Zugriff auf gemeinsame Daten.
pub(crate) fn build_plan_services(
    project_home: &harw_home::project::ProjectHome,
    config: &PlanToolConfig,
    plan_space: &str,
    goal_space: &str,
) -> Result<PlanServices, String> {
    if !config.is_enabled() {
        tracing::info!(
            reason = "tools.plan.enabled = false",
            "plan.services.skipped"
        );
        return Ok(PlanServices {
            plan: None,
            goal: None,
            findings: None,
            config: config.clone(),
        });
    }

    project_home.ensure().map_err(|error| error.to_string())?;
    let plan_root = project_home.plans_dir().join(plan_space);
    let goal_root = project_home.goals_dir().join(goal_space);

    let (plan, goal): (Arc<dyn PlanStore>, Arc<dyn GoalStore>) = if config.persist {
        let plan_store =
            FilePlanStore::with_config(&plan_root, config.clone()).map_err(|error| {
                format!(
                    "Plan-Store unter {} konnte nicht geöffnet werden: {error}",
                    plan_root.display()
                )
            })?;
        let goal_store = FileGoalStore::new(&goal_root).map_err(|error| {
            format!(
                "Goal-Store unter {} konnte nicht geöffnet werden: {error}",
                goal_root.display()
            )
        })?;
        (Arc::new(plan_store), Arc::new(goal_store))
    } else {
        (
            Arc::new(InMemoryPlanStore::new()),
            Arc::new(InMemoryGoalStore::new()),
        )
    };

    let findings = Arc::new(FindingStore::new(project_home.plans_dir()));

    tracing::info!(
        persist = config.persist,
        plan_root = %plan_root.display(),
        goal_root = %goal_root.display(),
        max_nodes = config.max_nodes,
        "plan.services.registered"
    );

    Ok(PlanServices {
        plan: Some(plan),
        goal: Some(goal),
        findings: Some(findings),
        config: config.clone(),
    })
}

/// Registriert Kern- und Planungs-Operationen in einer frischen Registry.
///
/// # Description
/// Nur noch Testhilfe: Produktionspfade finden Operationen über
/// [`harw_runtime::RuntimeAssembly::operations`]. Dieselbe [`PlanToolConfig`],
/// die [`build_plan_services`] bekommen hat, gated hier die sechs
/// Planungs-Operationen. Ist sie abgeschaltet, erscheinen `plan`, `goal`,
/// `explore`, `research_deps`, `research_web` und `analyze` gar nicht erst in
/// der Werkzeugliste.
///
/// # Arguments
/// - `config` (`&PlanToolConfig`): das Gate; geliehen.
///
/// # Returns
/// Die gefüllte `OperationRegistry` und die Anzahl registrierter
/// Planungs-Operationen (`0` oder [`harw_ops::PLAN_TOOL_COUNT`]).
#[cfg(test)]
pub(crate) fn build_operation_registry(
    config: &PlanToolConfig,
) -> (harw_operations::registry::OperationRegistry, usize) {
    let mut registry = harw_operations::registry::OperationRegistry::new();
    harw_ops::register_all(&mut registry);
    let plan_tools = harw_ops::register_plan_tools(&mut registry, config);
    (registry, plan_tools)
}

/// Das Ergebnis der Startup-Komposition der Planungsfläche.
///
/// # Description
/// Trägt, was ein Einstieg vor der Montage selbst auflöst (E6): den
/// Startmodus, die eine [`PlanToolConfig`], den Ziel-Kontext und die
/// Plan-Dienste. Der Chat-Einstieg reicht daraus `chat::ChatStartup` weiter,
/// `harw analyze` montiert damit [`EntryKind::Analyze`]. Freigabepolitik und
/// Extension-Registry baut die [`harw_runtime::RuntimeAssembly`].
///
/// # Concurrency
/// Alle Felder sind `Send + Sync` oder werden verschoben; der Typ selbst wird
/// nicht geteilt.
struct PlanningStartup {
    /// Aufgelöster Interaktionsmodus dieser Session.
    mode: InteractionMode,
    /// Die eine Konfiguration hinter Stores *und* Operationen.
    plan_config: PlanToolConfig,
    /// Ziel-Kontext-Beitragender; `None`, wenn die Planungsfläche aus ist.
    goal_context: Option<Arc<dyn ContextProvider>>,
    /// Plan-Dienste (Plan-, Goal- und Finding-Store, falls aktiv).
    services: PlanServices,
}

/// Legt das Startziel an und bindet es an den Plan.
///
/// # Description
/// Reihenfolge: Ziel setzen, Plan sicherstellen, Ziel an dessen tatsächliche
/// Revision binden. Eine erfundene Revision wäre schlimmer als gar keine,
/// deshalb wird sie aus dem Store gelesen und nicht geraten.
///
/// Das Ziel bekommt **ein** manuelles Akzeptanzkriterium, dessen Beschreibung
/// der Zielsatz selbst ist. Ohne mindestens ein Kriterium gilt ein Ziel laut
/// `harw_plan::goal::validate_goal_action` niemals als erreichbar — ein Ziel,
/// das nie geschlossen werden kann, wäre ein schlechter Startwert. Der
/// [`VerificationStep::Manual`] mit demselben Text macht es über
/// `plan evidence … manual <text>` belegbar (siehe
/// `harw_plan::goal::verification_satisfied_by`).
///
/// Akteur ist [`CLI_ACTOR`] — ein Start über die Kommandozeile ist ein
/// menschlicher Akteur, sonst wäre `/goal achieve` auf demselben Ziel später
/// unzulässig.
///
/// # Arguments
/// - `services` (`&PlanServices`): die gebauten Plan-Dienste, geliehen.
/// - `statement` (`&str`): der Zielsatz aus `--goal`, geliehen.
/// - `goal_id` (`&str`): Bezeichner des anzulegenden Ziels.
///
/// # Returns
/// Eine für Menschen lesbare Zusammenfassung dessen, was angelegt wurde.
///
/// # Errors
/// - Ein `String`, wenn `statement` leer ist.
/// - Ein `String`, wenn die Planungsfläche abgeschaltet ist (nennt den
///   Konfigurationsschlüssel).
/// - Ein `String`, wenn der Goal- oder Plan-Store die Mutation ablehnt.
///
/// # Concurrency
/// Die Serialisierung liegt bei den Stores; diese Funktion hält keine Locks.
fn seed_startup_goal(
    services: &PlanServices,
    statement: &str,
    goal_id: &str,
) -> Result<String, String> {
    let statement = statement.trim();
    if statement.is_empty() {
        return Err("--goal erwartet einen nicht-leeren Zielsatz".to_owned());
    }

    let Some(goal_store) = services.goal.as_ref() else {
        return Err(
            "--goal braucht die Planungsfläche; aktiviere sie über `[tools.plan] enabled = true`"
                .to_owned(),
        );
    };

    let now = offset_from_timestamp(jiff::Timestamp::now());
    let goal = Goal {
        id: GoalId::new(goal_id),
        revision: 0,
        statement: statement.to_owned(),
        non_goals: Vec::new(),
        invariants: Vec::new(),
        acceptance_criteria: vec![Criterion {
            description: statement.to_owned(),
            verification: vec![VerificationStep::Manual {
                note: statement.to_owned(),
            }],
        }],
        constraints: Vec::new(),
        open_questions: Vec::new(),
        status: GoalStatus::Active,
        plan_id: None,
        plan_revision: None,
        evidence: Vec::new(),
        created_at: now,
        updated_at: now,
    };

    let set_event = goal_store
        .apply(GoalAction::Set { goal }, CLI_ACTOR)
        .map_err(|error| format!("Goal-Store lehnt `set` ab: {error}"))?;

    let Some(plan_store) = services.plan.as_ref() else {
        tracing::warn!(goal_id, "startup.goal.unbound: kein Plan-Store verfügbar");
        return Ok(format!(
            "Ziel '{goal_id}' gesetzt (Revision {}); ohne Plan-Store nicht gebunden.",
            set_event.revision
        ));
    };

    if plan_store.current().is_err() {
        plan_store
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::new(STARTUP_PLAN_ID),
                    goal: statement.to_owned(),
                },
                CLI_ACTOR,
            )
            .map_err(|error| format!("Plan konnte nicht angelegt werden: {error}"))?;
    }

    let plan = plan_store
        .current()
        .map_err(|error| format!("Plan nicht lesbar: {error}"))?;
    let plan_id = plan.id.clone();
    let revision = plan.revision;
    let bind_event = goal_store
        .apply(
            GoalAction::BindPlan {
                plan_id: plan_id.clone(),
                revision,
            },
            CLI_ACTOR,
        )
        .map_err(|error| format!("Goal-Store lehnt `bind` ab: {error}"))?;

    Ok(format!(
        "Ziel '{goal_id}' gesetzt und an Plan '{plan_id}' @ Revision {revision} gebunden \
         (Goal-Revision {}).",
        bind_event.revision
    ))
}

/// Baut die Plan-Dienste, mit denen der Job-Worker `plan-node`-Jobs ausführt.
///
/// # Beschreibung
///
/// Ohne diese Dienste ignoriert der Worker keinen `plan-node`-Job mehr, aber er
/// blockiert jeden — die Kette `admit_ready_nodes → Job → on_job_completed`
/// bliebe wirkungslos, und ein Coding-Knoten stünde für immer auf `InProgress`.
///
/// Die übergebene Sandbox ist die **Obergrenze**, nicht die Arbeits-Sandbox: der
/// Worker leitet für jeden Knoten aus dessen Mutationsvertrag eine engere ab und
/// weist alles ab, was darüber hinausginge. Sie stammt aus
/// [`harw_runtime::root_sandbox`] für [`EntryKind::JobPlanNode`] und trägt damit
/// genau die Profilrechte dieses Einstiegs — die zwei Dateisystem-Berechtigungen,
/// aus denen ein Vertrag überhaupt etwas ableiten kann, bewusst **ohne**
/// `ExecuteProcess` und `NetworkAccess`: der Mutationsvertrag kennt heute kein
/// Feld, das solche Autorität begründen könnte, und was nicht begründbar ist,
/// wird nicht gewährt.
///
/// # Argumente
/// - `home` (`&Path`): HARW-Home; darunter liegen `plans/` und `goals/`.
/// - `config` (`&PlanToolConfig`): Gate und Grenzen der Planungsfläche.
/// - `project_root` (`&Path`): Wurzel des Arbeitsbereichs, gegen die die Pfade
///   des Mutationsvertrags aufgelöst werden.
///
/// # Rückgabe
/// `Some(_)`, wenn die Planungsfläche aktiv ist und einen Plan-Store hat;
/// `None`, wenn `[tools.plan] enabled = false` — dann meldet der Worker jeden
/// `plan-node`-Job sichtbar als blockiert.
///
/// # Fehler
/// - `Err(String)`: die Plan-Dienste ließen sich nicht aufbauen oder
///   `project_root` ließ sich nicht als Workspace binden
///   ([`harw_runtime::RuntimeError::Sandbox`]).
fn build_plan_node_services(
    home: &Path,
    config: &PlanToolConfig,
    project_root: &Path,
) -> Result<Option<Arc<job_worker::PlanNodeServices>>, String> {
    let project = harw_home::project::discover_project(project_root, &[])
        .map_err(|error| error.to_string())?;
    let project_home = harw_home::project::ProjectHome::at(&project);
    let services = build_plan_services(&project_home, config, DEFAULT_PLAN_SPACE, DEFAULT_GOAL_SPACE)?;
    let Some(plan) = services.plan else {
        return Ok(None);
    };

    let ceiling = harw_runtime::root_sandbox(EntryKind::JobPlanNode, project_root)
        .map_err(|error| error.to_string())?;

    Ok(Some(Arc::new(job_worker::PlanNodeServices::new(
        plan,
        ceiling,
        PLAN_NODE_JOB_ACTOR.to_owned(),
    ))))
}

/// Baut die [`RuntimeSpec`] eines lokalen Einstiegs.
///
/// # Description
/// Löst den Root-Space (`--home` vor `HARW_HOME`) und das Arbeitsverzeichnis
/// auf und setzt den lokalen Principal der Eingangsfläche
/// ([`runtime_entry::local_principal`]). Modus und Agent bleiben `None`; der
/// Aufrufer setzt sie nach [`prepare_planning_startup`] selbst (E6).
///
/// # Arguments
/// - `home_override` (`Option<PathBuf>`): expliziter Root-Space (`--home`).
/// - `entry` ([`EntryKind`]): der Einstieg.
/// - `surface` ([`IngressSurface`]): `Tui` oder `Cli`.
///
/// # Errors
/// Ein `String`, wenn weder Home noch Arbeitsverzeichnis auflösbar sind.
fn local_runtime_spec(
    home_override: Option<PathBuf>,
    entry: EntryKind,
    surface: IngressSurface,
) -> Result<RuntimeSpec, String> {
    let home = home::resolve_home(home_override)?;
    let cwd = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;
    Ok(runtime_entry::runtime_spec(
        entry,
        &home,
        &cwd,
        runtime_entry::local_principal(surface),
    ))
}

/// Setzt die Planungsfläche für einen Prozessstart zusammen.
///
/// # Description
/// Der eigentliche Composition-Root. Reihenfolge ist Absicht:
/// 1. Root-Space sicherstellen, Konfiguration über [`harw_runtime::load_config`]
///    laden (Layer, Repo-Trust, Validierung — dieselbe Quelle wie die Montage).
/// 2. `--mode` bzw. `[mode] default` typisieren — ein unbekannter Name bricht
///    hier ab, bevor irgendein Store entsteht. Der Modus ist ein Rückgabewert,
///    kein Prozesszustand.
/// 3. `[tools.plan]` in **eine** [`PlanToolConfig`] übersetzen.
/// 4. Mit genau dieser Konfiguration die Dienste bauen
///    ([`build_plan_services`]).
/// 5. Den [`GoalContextProvider`] vorbereiten.
/// 6. Ein `--goal` anlegen und binden.
///
/// Die Freigabepolitik aus `[policy] require_approval_for` baut die
/// [`harw_runtime::RuntimeAssembly`] selbst; sie ist deshalb nicht Teil des
/// Ergebnisses.
///
/// # Arguments
/// - `spec` (`&RuntimeSpec`): Spec des Einstiegs; `home` und `cwd` bestimmen
///   die Config-Layer.
/// - `requested_mode` (`Option<&str>`): Wert von `--mode`, geliehen.
/// - `requested_goal` (`Option<&str>`): Wert von `--goal`, geliehen.
///
/// # Returns
/// Den [`PlanningStartup`] mit Modus, Konfiguration, Ziel-Kontext und
/// Plan-Diensten.
///
/// # Errors
/// Ein `String` bei Home-, Config- oder Trust-Ladefehler, unbekanntem Modus,
/// ungültiger `[tools.plan]`-Sektion, nicht öffenbarem Store oder abgelehnter
/// Ziel-Mutation.
///
/// # Concurrency
/// Rein synchron; kein globaler Zustand.
fn prepare_planning_startup(
    spec: &RuntimeSpec,
    requested_mode: Option<&str>,
    requested_goal: Option<&str>,
) -> Result<PlanningStartup, String> {
    harw_home::ensure_home(&spec.home).map_err(|error| error.to_string())?;
    let (config, _trust) = harw_runtime::load_config(spec).map_err(|error| error.to_string())?;

    let mode = resolve_startup_mode(requested_mode, &config.harness.mode.default)?;
    tracing::info!(
        mode = mode.as_str(),
        explicit = requested_mode.is_some(),
        "startup.mode.resolved"
    );

    let plan_config = plan_tool_config_from_section(&config.harness.tools.plan)?;
    let project = harw_home::project::discover_project(&spec.cwd, &[])
        .map_err(|error| error.to_string())?;
    let project_home = harw_home::project::ProjectHome::at(&project);
    let services = build_plan_services(
        &project_home,
        &plan_config,
        DEFAULT_PLAN_SPACE,
        DEFAULT_GOAL_SPACE,
    )?;

    let goal_context = match (services.goal.as_ref(), services.plan.as_ref()) {
        (Some(goal), Some(plan)) => {
            let provider: Arc<dyn ContextProvider> = Arc::new(GoalContextProvider::new(
                Arc::clone(goal),
                Arc::clone(plan),
            ));
            Some(provider)
        }
        _ => None,
    };

    if let Some(statement) = requested_goal {
        let summary = seed_startup_goal(&services, statement, STARTUP_GOAL_ID)?;
        tracing::info!(goal_id = STARTUP_GOAL_ID, "startup.goal.seeded");
        println!("{summary}");
    }

    Ok(PlanningStartup {
        mode,
        plan_config,
        goal_context,
        services,
    })
}

/// Übersetzt die CLI-Flags von `harw analyze` in die Command-Tokens der Operation.
///
/// # Description
/// Die Flag-Grammatik von `/analyze` lebt in `harw_ops::analyze::AnalyzeArgs`
/// (`FromRawArgs`). Diese Funktion baut deshalb Tokens statt einen zweiten
/// Parser: so gibt es genau eine Stelle, die entscheidet, was `--top-down` oder
/// `--max-parallel` bedeuten. Nur die Widersprüche, die erst die clap-Form
/// erzeugt (zwei Bool-Flags für eine Richtung, `--workspace` neben einem
/// Crate-Namen), werden hier abgefangen.
///
/// # Arguments
/// - `args` (`&AnalyzeArgs`): die geparsten CLI-Flags, geliehen.
///
/// # Returns
/// Die Token-Liste für `OpInput::command("/analyze", …)`.
///
/// # Errors
/// Ein `String`, wenn `--bottom-up` und `--top-down` zusammen stehen,
/// `--workspace` neben einem Crate-Namen steht oder `--max-parallel 0` verlangt
/// wird (eine Welle ohne Kind ist keine Welle).
fn analyze_tokens(args: &AnalyzeArgs) -> Result<Vec<String>, String> {
    if args.bottom_up && args.top_down {
        return Err(
            "--bottom-up und --top-down schließen einander aus; wähle genau eine Richtung"
                .to_owned(),
        );
    }
    if let Some(name) = args.crate_name.as_deref().filter(|_| args.workspace) {
        return Err(format!(
            "--workspace analysiert den gesamten Workspace; der Crate-Name '{name}' ist damit \
             unvereinbar"
        ));
    }

    let mut tokens: Vec<String> = Vec::new();
    if args.dry_run {
        tokens.push("--dry-run".to_owned());
    }
    if args.bottom_up {
        tokens.push("--bottom-up".to_owned());
    }
    if args.top_down {
        tokens.push("--top-down".to_owned());
    }
    if let Some(max_parallel) = args.max_parallel {
        if max_parallel == 0 {
            return Err("--max-parallel muss mindestens 1 sein".to_owned());
        }
        tokens.push("--max-parallel".to_owned());
        tokens.push(max_parallel.to_string());
    }
    if let Some(name) = args.crate_name.as_deref() {
        tokens.push(name.to_owned());
    }
    Ok(tokens)
}

/// Fehlermeldung, wenn `harw analyze` ohne Planungsfläche aufgerufen wird.
///
/// Eine Stelle für beide Fälle — abgeschaltetes Gate und fehlende Operation —,
/// damit der Hinweis auf den Konfigurationsschlüssel nie auseinanderläuft.
fn analyze_plan_surface_disabled() -> String {
    "die Planungsfläche ist abgeschaltet; `analyze` ist damit nicht registriert. \
     Setze `[tools.plan] enabled = true` in der Konfiguration."
        .to_owned()
}

/// Montiert die Laufzeit für `harw analyze`.
///
/// # Description
/// Baut eine [`harw_runtime::RuntimeAssembly`] für [`EntryKind::Analyze`]
/// (`OperationSurface::CommandsOnly`, `SpawnerPolicy::BuiltinRoles`) aus einer
/// bereits vorbereiteten [`RuntimeSpec`], den Plan-Diensten
/// ([`PlanServices::to_runtime`]), der Modellquelle und einem optionalen
/// Secret-Resolver. Herausgelöst aus [`cmd_analyze`], damit die Montage selbst
/// — ohne CLI-Parsing, `println!` oder Operationsaufruf — isoliert testbar
/// ist.
///
/// Der Sitzungs-Ereigniskanal ist Pflicht für `SpawnerPolicy::BuiltinRoles`;
/// sein Empfänger geht an den Aufrufer zurück und muss bis zum Ende von dessen
/// Nutzung der Montage gebunden bleiben, damit Sendungen nicht an einem
/// geschlossenen Kanal enden. Eine Wurzelsitzung entsteht nicht: eine spätere
/// Operation läuft direkt unter
/// [`harw_runtime::RuntimeAssembly::root_session_id`], der beim Bau als
/// Spawner-Wurzel registrierten Kennung.
///
/// # Arguments
/// - `spec` (`RuntimeSpec`): vorbereitete Spec (inkl. `mode_override`),
///   verbraucht (der Builder nimmt sie entgegen).
/// - `plan` (`harw_runtime::PlanServices`): vollständige Plan-Dienste.
/// - `model` (`ModelSource`): `Echo` für `--dry-run`, sonst `Configured`.
/// - `resolver` (`Option<Arc<dyn SecretResolver + Send + Sync>>`): versiegelter
///   Secret-Resolver; `None` im `--dry-run`-Pfad.
///
/// # Returns
/// Die gebaute [`harw_runtime::RuntimeAssembly`] und den Empfänger des
/// Sitzungs-Ereigniskanals.
///
/// # Errors
/// Ein `String`, wenn der Montagebau scheitert.
///
/// # Concurrency
/// Rein synchron; baut keine eigene Runtime.
fn analyze_assembly(
    spec: RuntimeSpec,
    plan: harw_runtime::PlanServices,
    model: ModelSource,
    resolver: Option<Arc<dyn SecretResolver + Send + Sync>>,
) -> Result<
    (
        harw_runtime::RuntimeAssembly,
        tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
    ),
    String,
> {
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut builder = harw_runtime::RuntimeAssembly::builder(spec)
        .model(model)
        .stores(RuntimeStores {
            state_store: Arc::new(harw_core::InMemoryStateStore::new()),
            job_store: None,
            approval_store: None,
        })
        .plan_services(plan)
        .session_events(event_tx);
    if let Some(resolver) = resolver {
        builder = builder.secret_resolver(resolver);
    }
    let assembly = builder.build().map_err(|error| error.to_string())?;
    Ok((assembly, event_rx))
}

/// Führt `harw analyze` gegen die `analyze`-Operation aus.
///
/// # Description
/// Baut denselben Planungs-Startup wie der Chat-Einstieg
/// ([`prepare_planning_startup`]) und montiert dann über [`analyze_assembly`]
/// die Laufzeit. `/analyze` wird in
/// [`harw_runtime::RuntimeAssembly::operations`] gesucht und über die
/// Slash-Fläche ([`ServiceSurface::Slash`]) mit der Sandbox der Montage
/// ausgeführt. Ist die Planungsfläche abgeschaltet, ist `/analyze` gar nicht
/// registriert — die Fehlermeldung nennt dann den Konfigurationsschlüssel.
///
/// Modell (E7): `--dry-run` montiert ein nie aufgerufenes
/// [`ModelSource::Echo`]; sonst [`ModelSource::Configured`] samt versiegeltem
/// Secret-Resolver ([`runtime_entry::configured_secret_resolver`]), weil ein
/// echter Fan-out Kind-Agenten über den Spawner der Montage startet.
///
/// Vor der Ausführung prüft diese Funktion die Mindest-Berechtigungsstufe der
/// gefundenen Operation ([`harw_operations::operation::OperationMeta::permission`])
/// gegen die Stufe des montierten Principals
/// ([`harw_runtime::RuntimeAssembly::principal`]) — dieselbe Regel wie
/// `harw-tui`s `CommandRegistry::dispatch` (`harw-tui/src/registry.rs`): eine
/// zu niedrige Stufe bricht ab, statt die Operation trotzdem laufen zu lassen.
///
/// Der Sitzungs-Ereigniskanal aus [`analyze_assembly`] bleibt bis zum Ende
/// dieser Funktion gebunden, damit Sendungen nicht an einem geschlossenen
/// Kanal enden. Eine Wurzelsitzung entsteht nicht: die Operation läuft direkt
/// unter [`harw_runtime::RuntimeAssembly::root_session_id`], der beim Bau als
/// Spawner-Wurzel registrierten Kennung.
///
/// # Arguments
/// - `home_override` (`Option<PathBuf>`): expliziter Root-Space (`--home`).
/// - `requested_mode` (`Option<&str>`): Wert von `--mode`, geliehen.
/// - `requested_goal` (`Option<&str>`): Wert von `--goal`, geliehen.
/// - `args` (`&AnalyzeArgs`): die geparsten `analyze`-Flags, geliehen.
///
/// # Returns
/// `Ok(())`, nachdem der Bericht der Operation ausgegeben wurde.
///
/// # Errors
/// Ein `String` bei widersprüchlichen Flags, abgeschalteter Planungsfläche,
/// Config-/Trust-/Montagefehlern (inkl. fehlender Provider-Einrichtung ohne
/// `--dry-run`), zu niedriger Berechtigungsstufe oder abgelehnter Operation.
///
/// # Concurrency
/// Baut eine eigene Single-Thread-Tokio-Runtime für den einen Operationsaufruf.
fn cmd_analyze(
    home_override: Option<PathBuf>,
    requested_mode: Option<&str>,
    requested_goal: Option<&str>,
    args: &AnalyzeArgs,
) -> Result<(), String> {
    // Flag-Widersprüche vor jeder Datei- oder Netzarbeit melden.
    let tokens = analyze_tokens(args)?;
    let mut spec = local_runtime_spec(home_override, EntryKind::Analyze, IngressSurface::Cli)?;
    let startup = prepare_planning_startup(&spec, requested_mode, requested_goal)?;
    if !startup.plan_config.is_enabled() {
        return Err(analyze_plan_surface_disabled());
    }
    let plan = startup
        .services
        .to_runtime()
        .ok_or_else(analyze_plan_surface_disabled)?;
    spec.mode_override = Some(startup.mode);

    let (model, secret_resolver) = if args.dry_run {
        (ModelSource::Echo("harw analyze --dry-run".to_owned()), None)
    } else {
        let (config, _trust) =
            harw_runtime::load_config(&spec).map_err(|error| error.to_string())?;
        (
            ModelSource::Configured,
            runtime_entry::configured_secret_resolver(&spec.home, &config)?,
        )
    };

    let (assembly, _event_rx) = analyze_assembly(spec, plan, model, secret_resolver)?;

    let operation = assembly
        .operations()
        .find_by_command("/analyze")
        .map(Arc::clone)
        .ok_or_else(analyze_plan_surface_disabled)?;

    // C4: eine zu niedrige Berechtigungsstufe darf die Operation nicht
    // erreichen — dieselbe Prüfung wie `harw-tui`s `CommandRegistry::dispatch`
    // (`harw-tui/src/registry.rs`: `context.caller_tier < spec.permission`).
    let required: PermissionTier = operation.meta().permission;
    let actual: PermissionTier = assembly.principal().tier();
    if actual < required {
        return Err(format!(
            "`analyze` erfordert mindestens Berechtigungsstufe {required:?}, \
             der aufrufende Principal hat aber nur {actual:?}"
        ));
    }

    // Goal-Kontext wirkt bei analyze nur über Store-Seeding (--goal legt das
    // Ziel bereits in prepare_planning_startup an); Kind-Registry-Anbindung
    // (GoalContextContributor für /analyze) folgt in W4a.
    tracing::info!(
        mode = startup.mode.as_str(),
        operations = assembly.operations().len(),
        cwd = %assembly.spec().cwd.display(),
        "analyze.runtime.assembled"
    );

    let ctx = assembly.op_context(
        ServiceSurface::Slash,
        assembly.root_session_id().clone(),
        TurnId::new(),
        assembly.sandbox().clone(),
    );

    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start analyze runtime: {error}"))?;
    let output = runtime
        .block_on(async {
            operation
                .run(&ctx, OpInput::command("/analyze", tokens))
                .await
        })
        .map_err(|error| error.to_string())?;
    println!("{}", output.text);
    Ok(())
}

/// Prüft die Konfiguration für `harw doctor` und gibt eine Zusammenfassung aus.
///
/// # Description
/// Validiert die Config-Layer wie bisher und druckt deren Kennzahlen. Für die
/// Laufzeit-Rechte gibt es drei Fälle ([`doctor_home_resolution`]):
/// - `--config-dir` gesetzt: Runtime-Montage übersprungen, eine Zeile
///   `runtime_warning=skipped: --config-dir` (C6 — ein externes `--config-dir`
///   ist kein HARW-Home; ein Montageversuch dagegen wäre irreführend).
/// - kein `--config-dir`, aber [`home::resolve_home`] scheitert: eine Zeile
///   `runtime_warning=<text>` — der Fehler wird gemeldet, nicht still
///   verworfen.
/// - sonst: [`print_runtime_rights`] montiert [`EntryKind::Doctor`] und
///   druckt die effektiven Rechte.
///
/// # Arguments
/// - `layers` (`Vec<PathBuf>`): die Config-Layer.
/// - `home_override` (`Option<PathBuf>`): expliziter `--home`-Wert.
/// - `config_dir` (`Option<PathBuf>`): expliziter `--config-dir`-Wert.
///
/// # Errors
/// Ein `String`, wenn die Config nicht geladen oder validiert werden kann.
/// Ein Montagefehler oder ein nicht auflösbares Home ist **kein** Fehler
/// dieses Befehls: beides erscheint als Warnzeile, der Exit-Code bleibt der
/// der Config-Prüfung.
fn doctor(
    layers: Vec<PathBuf>,
    home_override: Option<PathBuf>,
    config_dir: Option<PathBuf>,
) -> Result<(), String> {
    let config = discover_config(&layers).map_err(|error| error.to_string())?;
    config.validate().map_err(|error| error.to_string())?;
    println!("Harwness configuration is valid.");
    println!(
        "layers={}",
        layers
            .iter()
            .map(|layer| layer.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("agents={}", config.agents.len());
    println!("providers={}", config.providers.len());
    println!("models={}", config.models.len());
    println!("skills={}", config.skills.len());
    println!("channels={}", config.channels.len());
    println!(
        "mcp_listener_enabled={}",
        config.harness.mcp_listener.enabled
    );
    println!(
        "mcp_listener_addr={}",
        config.harness.mcp_listener.listen_addr
    );
    println!("mcp_listener_path={}", config.harness.mcp_listener.path);
    match doctor_home_resolution(config_dir.as_deref(), home_override) {
        Ok(home) => print_runtime_rights(&home),
        Err(reason) => println!("runtime_warning={reason}"),
    }
    Ok(())
}

/// Entscheidet, ob und wie `doctor` den Root-Space für die Laufzeit-Rechte auflöst.
///
/// # Description
/// Reine Auflösungslogik ohne Seiteneffekt, herausgelöst aus [`doctor`], damit
/// C6 (kein stilles Verwerfen eines Auflösungsfehlers, kein Montageversuch
/// gegen ein externes `--config-dir`) ohne `println!`-Erfassung testbar ist.
///
/// # Arguments
/// - `config_dir` (`Option<&Path>`): expliziter `--config-dir`-Wert, geliehen.
/// - `home_override` (`Option<PathBuf>`): expliziter `--home`-Wert.
///
/// # Returns
/// Den aufgelösten Root-Space.
///
/// # Errors
/// - `"skipped: --config-dir"`, wenn `config_dir` gesetzt ist — ein externes
///   `--config-dir` ist kein HARW-Home, also wird die Runtime-Montage
///   übersprungen statt gegen den falschen Pfad zu scheitern.
/// - der Fehlertext von [`home::resolve_home`], wenn kein `--config-dir`
///   gesetzt ist, aber auch kein Root-Space auflösbar ist.
fn doctor_home_resolution(
    config_dir: Option<&Path>,
    home_override: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if config_dir.is_some() {
        return Err("skipped: --config-dir".to_owned());
    }
    home::resolve_home(home_override)
}

/// Druckt die effektiven Rechte der Doctor-Montage oder eine Warnzeile.
///
/// # Description
/// Montiert [`EntryKind::Doctor`] unter `home` und dem Arbeitsverzeichnis und
/// gibt aus [`harw_runtime::RuntimeAssembly::rights_snapshot`] Einstieg,
/// Rechte, Werkzeuganzahl, Approval-Kette und ein etwaiges nicht
/// vertrauenswürdiges Repository aus. Scheitert Arbeitsverzeichnis oder
/// Montage, erscheint genau eine `runtime_warning=`-Zeile.
///
/// # Arguments
/// - `home` (`&Path`): aufgelöster Root-Space.
fn print_runtime_rights(home: &Path) {
    let assembly = std::env::current_dir()
        .map_err(|error| format!("cwd: {error}"))
        .and_then(|cwd| runtime_entry::doctor_assembly(home, &cwd));
    let assembly = match assembly {
        Ok(assembly) => assembly,
        Err(error) => {
            println!("runtime_warning=runtime assembly failed: {error}");
            return;
        }
    };
    let rights = assembly.rights_snapshot();
    println!("runtime_entry={:?}", rights.entry);
    println!("runtime_permissions={}", rights.permissions.join(", "));
    println!("runtime_tools={}", rights.tools.len());
    println!(
        "runtime_approval_chain={}",
        rights
            .approval_chain
            .iter()
            .map(|(label, kind)| format!("{label}:{kind:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    match rights.untrusted_repo {
        Some(repo) => println!("runtime_untrusted_repo={}", repo.display()),
        None => println!("runtime_untrusted_repo=none"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_bare_invocation_as_chat() {
        let cli = Cli::try_parse_from(["harw"]).expect("bare harw parses");
        assert!(cli.command.is_none());
    }

    #[test]
    fn cli_parses_doctor_with_config_dir() {
        let cli = Cli::try_parse_from(["harw", "doctor", "--config-dir", "/tmp/x"])
            .expect("doctor parses");
        assert!(matches!(cli.command, Some(Command::Doctor { .. })));
    }

    #[test]
    fn cli_treats_unknown_token_as_chat_prompt() {
        // Wie `codex "prompt"`: ein freistehendes Token ist der Chat-Prompt,
        // kein unbekannter Subcommand.
        let cli = Cli::try_parse_from(["harw", "launch"]).expect("free token parses as prompt");
        assert!(cli.command.is_none());
        assert_eq!(cli.chat.prompt.as_deref(), Some("launch"));
    }

    #[test]
    fn local_run_executes_a_completed_core_turn() {
        let home = unique_temp_dir("local-run-response");
        harw_home::ensure_home(&home).expect("home scaffolds");
        assert_eq!(
            run_local_echo("hello harness", &home).unwrap(),
            "echo: hello harness"
        );
        std::fs::remove_dir_all(home).expect("remove temporary home");
    }

    #[test]
    fn local_run_persists_transcript_under_isolated_home() {
        let home = unique_temp_dir("local-run-transcript");
        harw_home::ensure_home(&home).expect("home scaffolds");

        assert_eq!(
            run_local_echo("persist me", &home).unwrap(),
            "echo: persist me"
        );

        let profile_name = harw_home::active_profile_name(&home);
        let sessions_root = home.join("profiles").join(profile_name).join("sessions");
        let transcripts = std::fs::read_dir(&sessions_root)
            .expect("sessions directory exists")
            .map(|entry| entry.expect("transcript directory entry").path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "jsonl")
            })
            .collect::<Vec<_>>();
        assert_eq!(transcripts.len(), 1, "run creates one durable transcript");
        let transcript = std::fs::read_to_string(&transcripts[0]).expect("read transcript");
        assert!(transcript.contains("persist me"), "user input is durable");
        assert!(
            transcript.contains("echo: persist me"),
            "assistant response is durable"
        );

        std::fs::remove_dir_all(home).expect("remove temporary home");
    }

    #[test]
    fn run_subcommand_requires_input() {
        assert!(Cli::try_parse_from(["harw", "run"]).is_err());
    }

    #[test]
    fn startup_migrations_write_one_backup_and_are_idempotent() {
        let dir = tempfile::tempdir().expect("create temporary config directory");
        let config_path = dir.path().join("config.toml");
        let original = "# existing user configuration\n[logging]\nlevel = \"info\"\n";
        std::fs::write(&config_path, original).expect("write old config");

        migrate_config_paths(std::slice::from_ref(&config_path))
            .expect("startup migration succeeds");

        let migrated = std::fs::read_to_string(&config_path).expect("read migrated config");
        assert!(migrated.contains("config_version = 1"), "{migrated}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml.bak.0"))
                .expect("read migration backup"),
            original
        );

        migrate_config_paths(std::slice::from_ref(&config_path))
            .expect("current config is a no-op");
        assert_eq!(
            std::fs::read_to_string(&config_path).expect("read config after second run"),
            migrated
        );
        assert!(
            !dir.path().join("config.toml.bak.1").exists(),
            "idempotent migration must not create another backup"
        );
    }

    #[test]
    fn startup_migrations_report_the_affected_config_path() {
        let dir = tempfile::tempdir().expect("create temporary config directory");
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "config_version = 2\n").expect("write future config");

        let error = migrate_config_paths(std::slice::from_ref(&config_path))
            .expect_err("future config version must stop startup");

        assert!(
            error.contains("configuration migration failed for"),
            "{error}"
        );
        assert!(
            error.contains(&config_path.display().to_string()),
            "{error}"
        );
        assert!(error.contains("kein Migrationspfad"), "{error}");
    }

    #[test]
    fn completion_path_skips_startup_migrations() {
        let home = tempfile::tempdir()
            .expect("create temporary parent")
            .path()
            .join("absent-harw-home");
        let command = Some(Command::Completion {
            shell: clap_complete::Shell::Zsh,
        });

        run_startup_migrations(&command, Some(home.clone())).expect("completion skips migrations");

        assert!(
            !home.exists(),
            "completion must not scaffold a home or write migration state"
        );
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("harw-cli-test-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        dir
    }

    /// Z1-R2-03: `harw serve` muss `file:`-Credentials auflösen, die
    /// `harw onboard` (`onboarding.rs` `write_secret_file`) und `harw-oauth`
    /// unterhalb von `<home>/secrets/` anlegen. Ohne Home bleibt der Pfad
    /// bewusst fail-closed
    /// (`harw_provider_http::FILE_CREDENTIAL_NO_HOME_REASON`).
    #[test]
    fn build_serve_provider_resolves_file_credentials_only_with_a_home() {
        use std::os::unix::fs::PermissionsExt as _;

        let home = unique_temp_dir("serve-provider-file-credential");
        let secrets = home.join("secrets");
        std::fs::create_dir_all(&secrets).expect("secrets dir creates");
        let token = secrets.join("gateway.key");
        std::fs::write(&token, "gateway-file-key").expect("secret writes");
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))
            .expect("secret is private");

        let provider = ProviderToml {
            name: "gateway".to_owned(),
            api: "openai-chat".to_owned(),
            base_url: "https://gateway.example/v1".to_owned(),
            auth: Some(SecretRef::File(
                token.to_str().expect("UTF-8 fixture path").to_owned(),
            )),
            auth_header: Some("bearer".to_owned()),
            api_key: None,
            headers: std::collections::HashMap::new(),
            models: vec!["model".to_owned()],
            enabled: true,
            origin_allowlist: OriginAllowlistToml::default(),
        };
        let mut config = ResolvedConfig::default();
        config.harness.default_provider = Some("gateway".to_owned());
        config.harness.default_model = Some("model".to_owned());
        config.providers.insert("gateway".to_owned(), provider);

        build_serve_provider(&config, Some(home.as_path()), None)
            .expect("file credential below <home>/secrets resolves for serve");
        // `Box<dyn ModelProvider>` implementiert kein `Debug` (Trait-Objekt
        // ohne Debug-Bound) — `.expect_err(..)` würde das für den Ok-Zweig
        // verlangen. Daher hier von Hand matchen und im unerwarteten
        // Ok-Fall mit einer eigenen, sprechenden Meldung abbrechen statt
        // den Provider selbst zu formatieren.
        let error = match build_serve_provider(&config, None, None) {
            Err(error) => error,
            Ok(_) => panic!(
                "file credential must stay fail-closed without a home, got a provider instead"
            ),
        };
        assert!(!error.contains("gateway-file-key"), "leaked secret: {error}");
        assert!(!error.contains("gateway.key"), "leaked path: {error}");

        std::fs::remove_dir_all(&home).expect("remove temporary home");
    }

    // ── Planungsfläche: Composition-Root (AP W5-08) ───────────────────────────

    /// Baut die Plan-Dienste über einem frischen Home und gibt beides zurück.
    fn plan_services_over_temp_home(
        config: &PlanToolConfig,
    ) -> (tempfile::TempDir, super::PlanServices) {
        let home = tempfile::tempdir().expect("create temporary home");
        let root = harw_home::project::ProjectRoot {
            root: home.path().to_path_buf(),
            trust_key: home.path().to_path_buf(),
            kind: harw_home::project::ProjectKind::Directory,
        };
        let project_home = harw_home::project::ProjectHome::at(&root);
        let services = build_plan_services(
            &project_home,
            config,
            DEFAULT_PLAN_SPACE,
            DEFAULT_GOAL_SPACE,
        )
        .expect("plan services build");
        (home, services)
    }

    #[test]
    fn disabled_plan_surface_registers_nothing_and_creates_no_directories() {
        // `persist = true` bei `enabled = false`: gerade dann darf nichts auf
        // der Platte entstehen — sonst sähe ein abgeschaltetes Werkzeug beim
        // nächsten Blick ins Dateisystem benutzt aus.
        let config = PlanToolConfig {
            enabled: false,
            persist: true,
            ..PlanToolConfig::default()
        };
        assert!(!config.is_enabled(), "explizit deaktivierte Konfiguration bleibt aus");

        let (home, services) = plan_services_over_temp_home(&config);

        assert!(services.plan.is_none());
        assert!(services.goal.is_none());
        assert!(services.findings.is_none());
        assert!(!services.config.is_enabled());
        assert!(services.to_runtime().is_none());

        assert!(
            !harw_home::paths::plans_dir(home.path()).exists(),
            "ein abgeschaltetes Plan-Werkzeug darf kein plans/-Verzeichnis anlegen"
        );
        assert!(
            !harw_home::paths::goals_dir(home.path()).exists(),
            "ein abgeschaltetes Plan-Werkzeug darf kein goals/-Verzeichnis anlegen"
        );

        let (registry, plan_tools) = build_operation_registry(&config);
        assert_eq!(plan_tools, 0, "geschlossenes Gate darf nichts registrieren");
        for path in [
            "/plan",
            "/goal",
            "/explore",
            "/research-deps",
            "/research-web",
            "/analyze",
        ] {
            assert!(
                registry.find_by_command(path).is_none(),
                "{path} darf bei geschlossenem Gate nicht auffindbar sein"
            );
        }
    }

    #[test]
    fn enabled_plan_surface_registers_six_operations_and_four_services() {
        let config = PlanToolConfig::enabled_defaults();
        let (_home, services) = plan_services_over_temp_home(&config);

        assert!(services.plan.is_some());
        assert!(services.goal.is_some());
        assert!(services.findings.is_some());
        assert!(services.to_runtime().is_some());

        let (registry, plan_tools) = build_operation_registry(&config);
        assert_eq!(plan_tools, harw_ops::PLAN_TOOL_COUNT);
        for name in [
            "plan",
            "goal",
            "explore",
            "research_deps",
            "research_web",
            "analyze",
        ] {
            assert!(
                registry.find_by_name(name).is_some(),
                "{name} wurde nicht registriert"
            );
        }
    }

    #[test]
    fn persisting_plan_surface_uses_separate_plan_and_goal_roots() {
        let config = PlanToolConfig {
            persist: true,
            ..PlanToolConfig::enabled_defaults()
        };
        let (home, services) = plan_services_over_temp_home(&config);

        assert!(services.plan.is_some());
        assert!(services.goal.is_some());

        let plans = home.path().join(".harw/plans").join(DEFAULT_PLAN_SPACE);
        let goals = home.path().join(".harw/goals").join(DEFAULT_GOAL_SPACE);
        assert!(plans.is_dir(), "{} fehlt", plans.display());
        assert!(goals.is_dir(), "{} fehlt", goals.display());
        assert_ne!(
            plans, goals,
            "ein Ziel überlebt Plan-Revisionen und braucht einen eigenen Speicherort"
        );
    }

    #[test]
    fn unknown_mode_is_rejected_with_the_list_of_valid_modes() {
        let error = resolve_startup_mode(Some("voelliger-unsinn"), "chat")
            .expect_err("ein unbekannter Modus darf nicht still auf chat fallen");

        assert!(error.contains("voelliger-unsinn"), "{error}");
        for mode in ["chat", "plan", "explore", "work"] {
            assert!(error.contains(mode), "{error} nennt '{mode}' nicht");
        }
    }

    #[test]
    fn explicit_mode_is_typed_and_beats_the_configured_default() {
        assert_eq!(
            resolve_startup_mode(Some("explore"), "work").expect("explore parses"),
            InteractionMode::Explore
        );
        assert_eq!(
            resolve_startup_mode(Some(" WORK "), "chat").expect("normalisierter Name parst"),
            InteractionMode::Work
        );
        assert_eq!(
            resolve_startup_mode(None, "plan").expect("[mode] default gilt ohne Flag"),
            InteractionMode::Plan
        );
    }

    #[test]
    fn invalid_configured_mode_is_an_error_not_a_silent_chat_fallback() {
        let error = resolve_startup_mode(None, "wörk")
            .expect_err("auch ein Konfigurationsfehler darf nicht still werden");
        assert!(error.contains("[mode] default"), "{error}");
        assert!(error.contains("work"), "{error}");
    }

    /// `to_runtime` reicht eine Planungsfläche nur vollständig weiter — und
    /// zwar mit **denselben** Store-Instanzen, nicht mit neu geöffneten.
    #[test]
    fn test_plan_services_to_runtime_requires_all_three_stores() {
        let disabled = super::PlanServices {
            plan: None,
            goal: None,
            findings: None,
            config: PlanToolConfig::default(),
        };
        assert!(disabled.to_runtime().is_none());

        let partial = super::PlanServices {
            plan: Some(Arc::new(InMemoryPlanStore::new())),
            goal: Some(Arc::new(InMemoryGoalStore::new())),
            findings: None,
            config: PlanToolConfig::default(),
        };
        assert!(
            partial.to_runtime().is_none(),
            "eine halb geöffnete Planungsfläche darf nie an die Montage gehen"
        );

        let config = PlanToolConfig::enabled_defaults();
        let (_home, complete) = plan_services_over_temp_home(&config);
        let Some(runtime) = complete.to_runtime() else {
            panic!("bei aktiver Planungsfläche müssen alle drei Stores vorliegen");
        };
        let (Some(plan), Some(goal), Some(findings)) = (
            complete.plan.as_ref(),
            complete.goal.as_ref(),
            complete.findings.as_ref(),
        ) else {
            panic!("die Fixture hat alle drei Stores");
        };
        assert!(Arc::ptr_eq(&runtime.plan, plan), "derselbe Plan-Store");
        assert!(Arc::ptr_eq(&runtime.goal, goal), "derselbe Goal-Store");
        assert!(Arc::ptr_eq(&runtime.findings, findings), "derselbe Finding-Store");
        assert!(runtime.plan_config.is_enabled(), "die Konfiguration reist mit");
        assert_eq!(runtime.plan_config.max_nodes, complete.config.max_nodes);
    }

    /// Der Startmodus ist ein Rückgabewert, kein Prozesszustand: zwei
    /// Auflösungen hintereinander liefern je ihren eigenen Modus.
    #[test]
    fn test_prepare_planning_startup_resolves_mode_without_global_state() {
        let home = tempfile::tempdir().expect("create temporary home");
        let cwd = tempfile::tempdir().expect("create temporary cwd");
        let spec = runtime_entry::runtime_spec(
            EntryKind::Tui,
            home.path(),
            cwd.path(),
            runtime_entry::local_principal(IngressSurface::Tui),
        );

        let explore = prepare_planning_startup(&spec, Some("explore"), None)
            .expect("explicit explore mode resolves");
        let explore_mode = explore.mode;
        assert_eq!(
            explore.services.to_runtime().is_some(),
            explore.plan_config.is_enabled(),
            "Plan-Dienste für die Montage genau dann, wenn [tools.plan] aktiv ist"
        );
        drop(explore);

        let work = prepare_planning_startup(&spec, Some("work"), None)
            .expect("explicit work mode resolves");
        assert_eq!(explore_mode, InteractionMode::Explore);
        assert_eq!(work.mode, InteractionMode::Work);
        drop(work);

        let Err(error) = prepare_planning_startup(&spec, Some("voelliger-unsinn"), None) else {
            panic!("ein unbekannter Modus darf keinen Start ergeben");
        };
        assert!(error.contains("voelliger-unsinn"), "{error}");
    }

    #[test]
    fn startup_goal_is_readable_and_created_by_a_human_actor() {
        let config = PlanToolConfig::enabled_defaults();
        let (_home, services) = plan_services_over_temp_home(&config);

        let summary = seed_startup_goal(&services, "  Die Planungsfläche steht  ", STARTUP_GOAL_ID)
            .expect("das Startziel muss anlegbar sein");
        assert!(summary.contains(STARTUP_GOAL_ID), "{summary}");

        let store = services.goal.as_ref().expect("Goal-Store vorhanden");
        let goal = store.current().expect("Ziel ist über den Store lesbar");
        assert_eq!(goal.id.as_str(), STARTUP_GOAL_ID);
        assert_eq!(goal.statement, "Die Planungsfläche steht");
        assert_eq!(
            goal.plan_id.as_ref().map(harw_plan::PlanId::as_str),
            Some(STARTUP_PLAN_ID)
        );
        assert!(
            goal.plan_revision.is_some(),
            "das Ziel muss an eine echte Plan-Revision gebunden sein"
        );

        // Der Akteur-Nachweis: derselbe Akteur dürfte das Ziel abschließen.
        let achieve = GoalAction::SetStatus {
            status: GoalStatus::Achieved,
            reason: Some("alle Kriterien belegt".to_owned()),
        };
        harw_plan::goal::validate_goal_action(Some(&goal), &achieve, CLI_ACTOR)
            .expect("ein menschlicher Akteur darf das Ziel für erreicht erklären");
        match harw_plan::goal::validate_goal_action(Some(&goal), &achieve, "model:test") {
            Err(harw_plan::PlanError::ActorNotAuthorized { .. }) => {}
            other => panic!("ein Modell-Akteur muss abgewiesen werden, war: {other:?}"),
        }
    }

    #[test]
    fn startup_goal_without_the_plan_surface_names_the_config_key() {
        let config = PlanToolConfig::default();
        let (_home, services) = plan_services_over_temp_home(&config);

        let error = seed_startup_goal(&services, "irgendein Ziel", STARTUP_GOAL_ID)
            .expect_err("ohne Goal-Store darf --goal nicht stillschweigend verpuffen");
        assert!(error.contains("tools.plan"), "{error}");
    }

    #[test]
    fn startup_goal_rejects_an_empty_statement() {
        let config = PlanToolConfig::enabled_defaults();
        let (_home, services) = plan_services_over_temp_home(&config);

        assert!(seed_startup_goal(&services, "   ", STARTUP_GOAL_ID).is_err());
    }

    #[test]
    fn plan_section_translates_into_one_shared_tool_config() {
        let section: PlanSection = toml::from_str(
            "enabled = true\npersist = true\nmax_nodes = 128\n\
             require_exploration_for = [\"coding\", \"docs\"]\n",
        )
        .expect("gültige [tools.plan]-Sektion");

        let config = plan_tool_config_from_section(&section).expect("Sektion ist übersetzbar");
        assert!(config.is_enabled());
        assert!(config.persist);
        assert_eq!(config.max_nodes, 128);
        assert_eq!(
            config.require_exploration_for,
            vec![PlanNodeKind::Coding, PlanNodeKind::Docs]
        );

        // Genau diese Konfiguration regiert auch die Operationen.
        let (registry, plan_tools) = build_operation_registry(&config);
        assert_eq!(plan_tools, harw_ops::PLAN_TOOL_COUNT);
        assert!(registry.find_by_command("/analyze").is_some());
    }

    /// Die Knotenart-Liste in `harw-config` bleibt mit `PlanNodeKind::ALL`
    /// deckungsgleich.
    ///
    /// `harw-config` kennt `harw-plan` bewusst **nicht** — deshalb hält
    /// `PlanSection::require_exploration_for` rohe Strings und validiert sie
    /// gegen eine eigene Liste. Diese Schichtung ist richtig, erzeugt aber eine
    /// zweite Quelle für dieselbe Aufzählung.
    ///
    /// Dieser Test ist die Klammer: er lebt in `harw-cli`, das **beide** Crates
    /// kennt, und schlägt fehl, sobald eine neue `PlanNodeKind`-Variante in der
    /// Config-Liste fehlt. Damit ist die Doppelung gekoppelt, ohne die
    /// Abhängigkeitsrichtung zu verletzen.
    #[test]
    fn config_node_kind_list_covers_every_plan_node_kind() {
        for kind in PlanNodeKind::ALL {
            let section = PlanSection {
                require_exploration_for: vec![kind.as_str().to_owned()],
                ..PlanSection::default()
            };
            assert!(
                section.validate().is_ok(),
                "harw-config kennt die Knotenart '{}' nicht — \
                 KNOWN_NODE_KINDS in plan_toml.rs muss ergänzt werden",
                kind.as_str()
            );
        }
    }

    #[test]
    fn plan_section_with_an_unknown_node_kind_is_rejected() {
        let section = PlanSection {
            require_exploration_for: vec!["schreiben".to_owned()],
            ..PlanSection::default()
        };

        let error = plan_tool_config_from_section(&section)
            .expect_err("ein Tippfehler in require_exploration_for muss auffallen");
        assert!(error.contains("schreiben"), "{error}");
    }

    #[test]
    fn analyze_flags_map_onto_the_operation_command_grammar() {
        let args = AnalyzeArgs {
            crate_name: Some("harw-core".to_owned()),
            workspace: false,
            bottom_up: false,
            top_down: true,
            dry_run: true,
            max_parallel: Some(3),
        };

        assert_eq!(
            analyze_tokens(&args).expect("Flags sind übersetzbar"),
            vec![
                "--dry-run".to_owned(),
                "--top-down".to_owned(),
                "--max-parallel".to_owned(),
                "3".to_owned(),
                "harw-core".to_owned(),
            ]
        );
    }

    #[test]
    fn analyze_tokens_are_accepted_by_the_operation_argument_parser() {
        use harw_operations::FromRawArgs;

        let args = AnalyzeArgs {
            crate_name: Some("harw-core".to_owned()),
            workspace: false,
            bottom_up: false,
            top_down: true,
            dry_run: true,
            max_parallel: Some(3),
        };
        let tokens = analyze_tokens(&args).expect("Flags sind übersetzbar");

        let parsed = harw_ops::analyze::AnalyzeArgs::from_raw_args(&tokens)
            .expect("die Operation muss ihre eigene Grammatik akzeptieren");
        assert_eq!(parsed.crate_name.as_deref(), Some("harw-core"));
        assert_eq!(parsed.bottom_up, Some(false));
        assert_eq!(parsed.dry_run, Some(true));
        assert_eq!(parsed.max_parallel, Some(3));
    }

    #[test]
    fn analyze_rejects_contradictory_direction_and_scope_flags() {
        let both_directions = AnalyzeArgs {
            crate_name: None,
            workspace: false,
            bottom_up: true,
            top_down: true,
            dry_run: false,
            max_parallel: None,
        };
        assert!(analyze_tokens(&both_directions).is_err());

        let workspace_and_crate = AnalyzeArgs {
            crate_name: Some("harw-core".to_owned()),
            workspace: true,
            bottom_up: false,
            top_down: false,
            dry_run: false,
            max_parallel: None,
        };
        let error = analyze_tokens(&workspace_and_crate)
            .expect_err("--workspace und ein Crate-Name schließen einander aus");
        assert!(error.contains("harw-core"), "{error}");

        let zero_parallel = AnalyzeArgs {
            crate_name: None,
            workspace: true,
            bottom_up: false,
            top_down: false,
            dry_run: false,
            max_parallel: Some(0),
        };
        assert!(analyze_tokens(&zero_parallel).is_err());
    }

    /// C3: bei aktiver Planungsfläche montiert [`analyze_assembly`] eine
    /// Laufzeit, in der `/analyze` über
    /// `assembly.operations().find_by_command` auffindbar ist.
    #[test]
    fn test_analyze_assembly_registers_analyze_operation_when_plan_enabled() {
        let home = tempfile::tempdir().expect("create temporary home");
        harw_home::ensure_home(home.path()).expect("home scaffolds");
        std::fs::write(
            home.path().join("config.toml"),
            "[tools.plan]\nenabled = true\n",
        )
        .expect("write plan-enabled config");
        let cwd = tempfile::tempdir().expect("create temporary cwd");

        let spec = runtime_entry::runtime_spec(
            EntryKind::Analyze,
            home.path(),
            cwd.path(),
            runtime_entry::local_principal(IngressSurface::Cli),
        );
        let startup = prepare_planning_startup(&spec, None, None)
            .expect("planning startup resolves over an enabled plan surface");
        assert!(
            startup.plan_config.is_enabled(),
            "config.toml muss [tools.plan] aktivieren"
        );
        let plan = startup
            .services
            .to_runtime()
            .expect("bei aktiver Fläche liegen alle drei Plan-Stores vor");

        let (assembly, _event_rx) =
            analyze_assembly(spec, plan, ModelSource::Echo("test".to_owned()), None)
                .expect("analyze assembly builds over an enabled plan surface");

        assert!(
            assembly.operations().find_by_command("/analyze").is_some(),
            "/analyze muss bei aktiver Planungsfläche registriert sein"
        );
    }

    /// C3: eine abgeschaltete Planungsfläche lässt `harw analyze` mit der
    /// zentralen Fehlermeldung ([`analyze_plan_surface_disabled`]) scheitern.
    #[test]
    fn test_cmd_analyze_disabled_plan_surface_names_config_key() {
        let home = tempfile::tempdir().expect("create temporary home");
        let args = AnalyzeArgs {
            crate_name: None,
            workspace: false,
            bottom_up: false,
            top_down: false,
            dry_run: true,
            max_parallel: None,
        };

        let error = cmd_analyze(Some(home.path().to_path_buf()), None, None, &args)
            .expect_err("harw analyze muss ohne `[tools.plan] enabled = true` scheitern");
        assert_eq!(error, analyze_plan_surface_disabled());
    }

    #[test]
    fn cli_exposes_mode_and_goal_as_root_flags() {
        let cli = Cli::try_parse_from(["harw", "--mode", "explore", "--goal", "Bridge fertig"])
            .expect("root flags parse");

        assert_eq!(cli.mode.as_deref(), Some("explore"));
        assert_eq!(cli.goal.as_deref(), Some("Bridge fertig"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn external_serve_config_uses_explicit_home_for_sealed_secret_store() {
        let config_dir = PathBuf::from("/tmp/harw-external-config");
        let home = PathBuf::from("/tmp/harw-sealed-secret-store");

        let (layers, storage_root, secret_store_home) =
            resolve_serve_paths(Some(home.clone()), Some(config_dir.clone()))
                .expect("external config with explicit home resolves");

        assert_eq!(layers, vec![config_dir.clone()]);
        assert_eq!(storage_root, config_dir);
        assert_eq!(secret_store_home, Some(home));
    }

    #[test]
    fn normal_serve_config_uses_active_profile_as_storage_root() {
        let home = unique_temp_dir("serve-active-profile-root");
        harw_home::ensure_home(&home).expect("home scaffolds");
        std::fs::write(harw_home::paths::active_profile_path(&home), "mcp-worker\n")
            .expect("active profile writes");

        let (_, storage_root, secret_store_home) =
            resolve_serve_paths(Some(home.clone()), None).expect("normal serve paths resolve");

        assert_eq!(storage_root, home.join("profiles").join("mcp-worker"));
        assert_eq!(
            storage_root.join("jobs"),
            home.join("profiles/mcp-worker/jobs")
        );
        assert_eq!(secret_store_home, Some(home.clone()));
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn normal_serve_config_rejects_an_invalid_active_profile() {
        let home = unique_temp_dir("serve-invalid-active-profile");
        std::fs::write(harw_home::paths::active_profile_path(&home), "../outside\n")
            .expect("invalid active profile writes");

        let error = resolve_serve_paths(Some(home.clone()), None)
            .expect_err("invalid active profile must fail path resolution");

        assert!(error.contains("invalid profile name"), "{error}");
        assert!(error.contains("../outside"), "{error}");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn external_serve_config_rejects_sealed_refs_without_explicit_home() {
        let config_dir = PathBuf::from("/tmp/harw-external-config");
        let (_, _, secret_store_home) = resolve_serve_paths(None, Some(config_dir))
            .expect("external config without home still resolves its config paths");
        let mut config = ResolvedConfig::default();
        config.providers.insert(
            "sealed".to_owned(),
            toml::from_str(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .expect("valid sealed provider config"),
        );

        let error = require_home_for_sealed_refs(&config, secret_store_home.as_deref())
            .expect_err("external config must not guess a sealed-secret root");
        assert!(error.contains("requires HARW_HOME"), "{error}");
        assert!(error.contains("--config-dir"), "{error}");
    }

    #[test]
    fn serve_fails_closed_when_listener_is_disabled() {
        let dir = unique_temp_dir("serve-disabled");
        std::fs::write(dir.join("config.toml"), "").expect("empty config writes");
        let error = serve_mcp(vec![dir.clone()], dir.clone(), None)
            .expect_err("serve must refuse a disabled listener");
        assert!(error.contains("mcp_listener.enabled is false"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn external_config_dir_rejects_sealed_provider_without_harw_home() {
        let mut config = ResolvedConfig::default();
        config.providers.insert(
            "sealed".to_owned(),
            toml::from_str(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .expect("valid sealed provider config"),
        );

        let error = require_home_for_sealed_refs(&config, None)
            .expect_err("external config must not guess a sealed-secret root");
        assert!(error.contains("requires HARW_HOME"), "{error}");
        assert!(error.contains("--config-dir"), "{error}");

        require_home_for_sealed_refs(&config, Some(Path::new("/tmp/harw-sealed-secret-store")))
            .expect("explicit home enables the sealed provider secret store");
    }

    #[test]
    fn ordinary_provider_refs_remain_valid_without_harw_home() {
        let config = ResolvedConfig::default();

        require_home_for_sealed_refs(&config, None)
            .expect("ordinary provider references do not require HARW_HOME");
    }

    #[test]
    fn external_config_dir_rejects_sealed_mcp_principal_without_harw_home() {
        let mut config = ResolvedConfig::default();
        config
            .harness
            .mcp_listener
            .principals
            .push(harw_config::McpPrincipalToml {
                id: "sealed-mcp".to_owned(),
                credential_ref: "secrets:mcp-token".parse().expect("valid secret ref"),
                tenant: "mia".to_owned(),
                workspace: "harwness".to_owned(),
                job_capabilities: Vec::new(),
            });

        let error = require_home_for_sealed_refs(&config, None)
            .expect_err("external config must not guess a sealed-secret root");
        assert!(error.contains("requires HARW_HOME"), "{error}");
        assert!(error.contains("MCP principal"), "{error}");

        require_home_for_sealed_refs(&config, Some(Path::new("/tmp/harw-sealed-secret-store")))
            .expect("explicit home enables the sealed MCP principal secret store");
    }

    #[test]
    fn build_authenticator_resolves_file_credentials_for_every_principal() {
        let dir = unique_temp_dir("build-authenticator-ok");
        let credential_path = dir.join("token");
        std::fs::write(&credential_path, "super-secret-token").expect("credential file writes");

        let mut config = ResolvedConfig::default();
        config
            .harness
            .mcp_listener
            .principals
            .push(harw_config::McpPrincipalToml {
                id: "mia-local".to_owned(),
                credential_ref: format!("file:{}", credential_path.display())
                    .parse()
                    .expect("valid secret ref"),
                tenant: "mia".to_owned(),
                workspace: "harwness".to_owned(),
                job_capabilities: Vec::new(),
            });

        let authenticator = build_authenticator(&config, None).expect("credential resolves");
        let principal = authenticator
            .authenticate(Some("Bearer super-secret-token"))
            .expect("token authenticates");
        assert_eq!(principal.principal_key, "mia-local");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_principal_registry_maps_capabilities_and_tenant_scope() {
        let mut config = ResolvedConfig::default();
        config
            .harness
            .mcp_listener
            .principals
            .push(harw_config::McpPrincipalToml {
                id: "mia-local".to_owned(),
                credential_ref: "env:HARW_TEST_UNUSED".parse().expect("valid secret ref"),
                tenant: "mia".to_owned(),
                workspace: "harwness".to_owned(),
                job_capabilities: vec![
                    McpJobCapabilityToml::ReadOwn,
                    McpJobCapabilityToml::CancelOwn,
                ],
            });

        let registry =
            build_principal_registry(&config).expect("distinct principal ids build a registry");
        let principal = registry
            .get("mia-local")
            .expect("configured principal is present in the registry");
        assert!(
            principal
                .capabilities()
                .contains(&McpJobCapability::ReadOwn)
        );
        assert!(
            principal
                .capabilities()
                .contains(&McpJobCapability::CancelOwn)
        );
        assert!(
            !principal
                .capabilities()
                .contains(&McpJobCapability::ReadWorkspace)
        );
    }

    #[test]
    fn build_principal_registry_grants_submit_own_only_when_configured() {
        let mut config = ResolvedConfig::default();
        for (id, job_capabilities) in [
            ("submitter", vec![McpJobCapabilityToml::SubmitOwn]),
            ("reader", Vec::new()),
        ] {
            config
                .harness
                .mcp_listener
                .principals
                .push(harw_config::McpPrincipalToml {
                    id: id.to_owned(),
                    credential_ref: "env:HARW_TEST_UNUSED".parse().expect("valid secret ref"),
                    tenant: "mia".to_owned(),
                    workspace: "harwness".to_owned(),
                    job_capabilities,
                });
        }

        let registry =
            build_principal_registry(&config).expect("distinct principal ids build a registry");
        assert!(
            registry
                .get("submitter")
                .expect("configured submitter is present")
                .capabilities()
                .contains(&McpJobCapability::SubmitOwn)
        );
        assert!(
            !registry
                .get("reader")
                .expect("configured reader is present")
                .capabilities()
                .contains(&McpJobCapability::SubmitOwn)
        );
    }

    #[test]
    fn test_build_principal_registry_rejects_duplicate_id() {
        let mut config = ResolvedConfig::default();
        for workspace in ["first", "second"] {
            config
                .harness
                .mcp_listener
                .principals
                .push(harw_config::McpPrincipalToml {
                    id: "twice".to_owned(),
                    credential_ref: "env:HARW_TEST_UNUSED".parse().expect("valid secret ref"),
                    tenant: "mia".to_owned(),
                    workspace: workspace.to_owned(),
                    job_capabilities: vec![McpJobCapabilityToml::SubmitOwn],
                });
        }

        match build_principal_registry(&config) {
            Ok(_) => panic!("a duplicated principal id must abort registry construction"),
            Err(error) => {
                assert!(error.contains("'twice'"), "{error}");
                assert!(error.contains("more than once"), "{error}");
            }
        }
    }

    #[test]
    fn test_dispatch_web_without_home_names_home_flag() {
        let error = web_home(Err("could not determine the home directory".to_owned()))
            .expect_err("an unresolvable home must stop harw web");
        assert!(error.contains("--home"), "{error}");
        assert!(error.contains("HARW_HOME"), "{error}");
        assert!(
            error.contains("could not determine the home directory"),
            "{error}"
        );

        let home = PathBuf::from("/tmp/harw-web-home");
        assert_eq!(web_home(Ok(home.clone())), Ok(home));
    }

    /// C6: ein externes `--config-dir` ist kein HARW-Home — `doctor` darf
    /// keinen Montageversuch dagegen unternehmen, sondern überspringt die
    /// Laufzeit-Rechte ausdrücklich.
    #[test]
    fn test_doctor_home_resolution_skips_runtime_rights_with_config_dir() {
        let config_dir = PathBuf::from("/tmp/harw-doctor-config-dir");
        let error = doctor_home_resolution(Some(config_dir.as_path()), None)
            .expect_err("--config-dir must skip the runtime rights assembly");
        assert_eq!(error, "skipped: --config-dir");

        // Auch mit einem zusätzlich gesetzten `--home` bleibt es beim Skip:
        // `--config-dir` gewinnt, das Home wird nicht stillschweigend benutzt.
        let error = doctor_home_resolution(
            Some(config_dir.as_path()),
            Some(PathBuf::from("/tmp/harw-doctor-home")),
        )
        .expect_err("--config-dir must win over a coincidentally set --home");
        assert_eq!(error, "skipped: --config-dir");
    }

    /// C6: ohne `--config-dir` löst `doctor` den Root-Space normal auf.
    #[test]
    fn test_doctor_home_resolution_resolves_explicit_home_without_config_dir() {
        let home = PathBuf::from("/tmp/harw-doctor-explicit-home");
        let resolved = doctor_home_resolution(None, Some(home.clone()))
            .expect("an explicit --home resolves without --config-dir");
        assert_eq!(resolved, home);
    }

    #[test]
    fn test_run_startup_migrations_project_skips_home_resolution() {
        let parent = tempfile::tempdir().expect("create temporary parent");
        let missing_home = parent.path().join("absent-harw-home");
        let command = Cli::try_parse_from(["harw", "project", "status"])
            .expect("project status parses")
            .command;

        run_startup_migrations(&command, Some(missing_home.clone()))
            .expect("project commands do not migrate configuration");
        assert!(
            !missing_home.exists(),
            "project commands must not scaffold a home during startup migrations"
        );
    }

    #[test]
    fn build_authenticator_fails_closed_on_unresolvable_credential() {
        let mut config = ResolvedConfig::default();
        config
            .harness
            .mcp_listener
            .principals
            .push(harw_config::McpPrincipalToml {
                id: "mia-local".to_owned(),
                credential_ref: "env:HARW_TEST_MCP_TOKEN_UNSET_FOR_SURE"
                    .parse()
                    .expect("valid secret ref"),
                tenant: "mia".to_owned(),
                workspace: "harwness".to_owned(),
                job_capabilities: Vec::new(),
            });

        match build_authenticator(&config, None) {
            Ok(_) => panic!("missing environment credential must fail closed"),
            Err(error) => assert!(error.contains("mia-local"), "{error}"),
        }
    }

    /// Fake listener: serves until the shared watch flag turns `true`, like
    /// `BoundMcpListener::serve_until`, and reports that it saw the flag.
    async fn fake_listener(
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        stopped: tokio::sync::oneshot::Sender<()>,
    ) -> std::io::Result<()> {
        while !*shutdown.borrow() {
            if shutdown.changed().await.is_err() {
                break;
            }
        }
        stopped
            .send(())
            .expect("test observer outlives the fake listener");
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_until_shutdown_stops_listener_and_worker() {
        let (shutdown_tx, worker_rx) = tokio::sync::watch::channel(false);
        let (trigger_tx, trigger_rx) = tokio::sync::oneshot::channel::<()>();
        let (listener_stopped_tx, mut listener_stopped_rx) = tokio::sync::oneshot::channel();
        let (worker_done_tx, worker_done_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let mut worker_rx = worker_rx;
            while !*worker_rx.borrow() {
                if worker_rx.changed().await.is_err() {
                    break;
                }
            }
            worker_done_tx.send(()).expect("serve_until awaits the worker");
        });

        let serve = serve_until(
            |rx| fake_listener(rx, listener_stopped_tx),
            async {
                trigger_rx.await.expect("trigger sender kept alive");
            },
            &shutdown_tx,
            worker_done_rx,
            Duration::from_secs(5),
        );
        tokio::pin!(serve);
        // Without a shutdown request the future keeps serving.
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut serve)
                .await
                .is_err(),
            "serve_until must not finish before shutdown"
        );
        assert!(listener_stopped_rx.try_recv().is_err());

        trigger_tx.send(()).expect("serve_until holds the trigger");
        let outcome = tokio::time::timeout(Duration::from_secs(5), serve)
            .await
            .expect("shutdown must end serve_until");

        assert_eq!(outcome.listener, Ok(()));
        assert_eq!(outcome.worker, WorkerStop::Finished);
        assert_eq!(listener_stopped_rx.try_recv(), Ok(()));
        assert!(*shutdown_tx.borrow());
        worker.await.expect("worker task joins");
    }

    #[tokio::test]
    async fn test_serve_until_caps_wait_for_stuck_worker() {
        let (shutdown_tx, _worker_rx) = tokio::sync::watch::channel(false);
        let (listener_stopped_tx, _listener_stopped_rx) = tokio::sync::oneshot::channel();
        // The sender is kept alive and never used: a worker that ignores shutdown.
        let (_worker_done_tx, worker_done_rx) = tokio::sync::oneshot::channel::<()>();

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            serve_until(
                |rx| fake_listener(rx, listener_stopped_tx),
                std::future::ready(()),
                &shutdown_tx,
                worker_done_rx,
                Duration::from_millis(20),
            ),
        )
        .await
        .expect("grace period must bound serve_until");

        assert_eq!(outcome.listener, Ok(()));
        assert_eq!(outcome.worker, WorkerStop::TimedOut);
    }

    #[tokio::test]
    async fn test_serve_until_worker_panic_while_serving_is_error() {
        let (shutdown_tx, _worker_rx) = tokio::sync::watch::channel(false);
        let (listener_stopped_tx, mut listener_stopped_rx) = tokio::sync::oneshot::channel();
        let (worker_done_tx, worker_done_rx) = tokio::sync::oneshot::channel::<()>();
        drop(worker_done_tx);

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            serve_until(
                |rx| fake_listener(rx, listener_stopped_tx),
                std::future::pending::<()>(),
                &shutdown_tx,
                worker_done_rx,
                Duration::from_secs(5),
            ),
        )
        .await
        .expect("a vanished worker must end serve_until");

        assert_eq!(outcome.worker, WorkerStop::Vanished);
        let error = outcome.listener.expect_err("dead worker is reported");
        assert!(error.contains("job worker stopped unexpectedly"), "{error}");
        assert_eq!(listener_stopped_rx.try_recv(), Ok(()));
    }

    #[tokio::test]
    async fn test_serve_until_listener_error_signals_worker_shutdown() {
        let (shutdown_tx, worker_rx) = tokio::sync::watch::channel(false);
        let (worker_done_tx, worker_done_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let mut worker_rx = worker_rx;
            while !*worker_rx.borrow() {
                if worker_rx.changed().await.is_err() {
                    break;
                }
            }
            worker_done_tx.send(()).expect("serve_until awaits the worker");
        });

        let outcome = serve_until(
            |_rx| async { Err(std::io::Error::other("accept failed")) },
            std::future::pending::<()>(),
            &shutdown_tx,
            worker_done_rx,
            Duration::from_secs(5),
        )
        .await;

        assert_eq!(outcome.listener, Err("accept failed".to_owned()));
        assert_eq!(outcome.worker, WorkerStop::Finished);
        worker.await.expect("worker task joins");
    }

    #[test]
    fn test_spawn_job_worker_thread_reports_completion() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build worker runtime for test");
        let (ran_tx, ran_rx) = std::sync::mpsc::channel();
        let worker = spawn_job_worker_thread(runtime, move || async move {
            ran_tx
                .send(std::thread::current().name().map(str::to_owned))
                .expect("test receiver alive");
        })
        .expect("spawn worker thread");

        worker.handle.join().expect("worker thread joins");
        assert_eq!(
            ran_rx.recv().expect("worker ran"),
            Some("harw-job-worker".to_owned())
        );
        let mut done = worker.done;
        assert_eq!(done.try_recv(), Ok(()));
    }

    #[test]
    fn test_jemalloc_allocator_is_feature_gated() {
        let source = include_str!("main.rs");
        let allocator = source
            .find("#[global_allocator]")
            .expect("global allocator declaration present");
        let gate = source
            .find("#[cfg(feature = \"jemalloc\")]")
            .expect("jemalloc cfg gate present");
        // The gate must directly precede the allocator attribute.
        assert!(gate < allocator);
        assert_eq!(
            source[gate..allocator].trim(),
            "#[cfg(feature = \"jemalloc\")]"
        );
        assert_eq!(
            source
                .lines()
                .filter(|line| line.trim() == "#[global_allocator]")
                .count(),
            1
        );

        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest.contains("tikv-jemallocator = { version = \"0.7.0\", optional = true }"),
            "jemalloc dependency must be optional"
        );
        assert!(manifest.contains("jemalloc = [\"dep:tikv-jemallocator\"]"));
        // Opt-in: the default feature set must not pull jemalloc in.
        assert!(manifest.contains("default = []"));
    }
}
