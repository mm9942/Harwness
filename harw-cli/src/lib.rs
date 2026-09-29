//! Standalone `harw` process entrypoint (library root; the `harw` binary in
//! `src/main.rs` calls [`main_entry`]).
//!
//! Ohne Subcommand (oder mit `chat`) startet `harw` den interaktiven
//! ratatui-Chat (`chat`), nachdem der Root-Space `~/.harw` sichergestellt und
//! — beim Erststart — der Onboarding-Wizard (`onboarding`) durchlaufen wurde;
//! `exec` beantwortet einen einzelnen Prompt ohne Oberfläche. Die übrigen
//! Subcommands decken Sitzungen, Konfiguration, Anbieter und Modelle,
//! Agenten und Wissen, Aufträge, Dienste (MCP-Listener, Web-Oberfläche,
//! Gateway) sowie Einrichtung und Diagnose ab. Ältere Befehlsnamen werden
//! weiterhin angenommen und mit einem Hinweis auf den neuen Namen umgeleitet.
//!
//! Vor dem Routing wendet [`apply_process_globals`] `-C/--cwd` und
//! `--profile` prozessweit an; [`dispatch`] lehnt Sitzungs-Flags außerhalb
//! von `chat`/`exec`/`analyze` und `--json` bei Befehlen ohne JSON-Ausgabe
//! ab. `harw serve` fährt bei SIGTERM/SIGINT
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

mod agent_cmd;
mod auth;
mod auth_migrate;
// #22: automatischer Artefakt-Build der aktiven UIA im Hintergrund.
mod auto_build;
mod chat;
mod cli;
mod completions;
mod connect;
mod doc_ocr;
// #22: die personalisierte harw mit eingebetteter UIA (`--native`).
pub mod embedded_uia;
mod gateway;
mod home;
mod job_worker;
mod jobs_cmd;
mod knowledge_cmd;
mod worker_cmd;
mod lens;
mod lifecycle;
mod mcp;
mod mcp_auth;
mod models;
mod observe;
mod onboarding;
mod op_bridge;
mod output;
mod pr_review;
mod project_trust;
mod provider_cmd;
mod resume;
mod runtime_entry;
mod runtime_gateway;
mod runtime_jobs;
mod runtime_web;
mod sandbox_cmd;
mod secret_store;
mod session_cmd;
mod settings;
mod tailscale_cmd;
mod telegram_launcher;
#[cfg(test)]
mod test_support;
mod uia_bootstrap;
mod update_cmd;
mod verify_sandbox;
mod web;
mod worker_cancellation;

pub use embedded_uia::{HomeChoice, run_with_embedded_uia};

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode};
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
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

use cli::{
    AgentAction, AnalyzeArgs, AnalyzeOrder, ChannelAction, ChatArgs, Cli, Command, DebugAction,
    GlobalArgs, KnowledgeAction, ModelsAction, SessionAction,
};
use output::Printer;

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

/// Öffnet (und rotiert bei Bedarf) das Datei-Log der TUI.
fn open_tui_log_file() -> Option<std::fs::File> {
    const MAX_BYTES: u64 = 10 * 1024 * 1024;
    let dir = harw_home::paths::logs_dir(&harw_home::home_dir().ok()?);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("tui.log");
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, dir.join("tui.log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// Filter-Vorgabe, wenn weder `--log`, `RUST_LOG` noch ein vom Default
/// abweichendes `[logging] level` gesetzt ist.
///
/// Standardmäßig auf `warn` reduziert, damit die interaktive TUI (Alternate
/// Screen) nicht durch Info-Spans überschrieben wird und Subcommands ruhig
/// bleiben. Ein explizites `--log info` (oder `RUST_LOG=info`) bleibt möglich.
const DEFAULT_LOG_FILTER: &str = "warn";

/// Default-Wert von `[logging] level` (siehe `harw_config::LoggingSection`).
///
/// Da das Feld kein `Option` ist, lässt sich ein explizit gesetztes
/// `level = "info"` nicht vom Default unterscheiden; beides fällt deshalb auf
/// [`DEFAULT_LOG_FILTER`] zurück.
const CONFIG_DEFAULT_LOG_LEVEL: &str = "info";

/// Wählt die wirksame Tracing-Filter-Direktive.
///
/// # Description
///
/// Präzedenz (erste gültige gewinnt):
/// 1. explizit auf der Kommandozeile übergebenes `--log` (beim Parsen bereits
///    über `LogFilterParser` validiert);
/// 2. die Umgebungsvariable `RUST_LOG`, sofern nicht leer und als
///    `EnvFilter`-Direktive gültig;
/// 3. `[logging] level` der effektiven Harness-Konfiguration, sofern gültig
///    und vom Default (`"info"`) verschieden;
/// 4. [`DEFAULT_LOG_FILTER`].
///
/// Ungültige Werte der Stufen 2 und 3 werden übersprungen, damit der Prozess
/// immer mit einem brauchbaren Subscriber startet.
///
/// # Arguments
///
/// - `cli_explicit` (`Option<&str>`): `--log`, nur wenn auf der Kommandozeile
///   gesetzt (nicht der clap-Default).
/// - `rust_log` (`Option<&str>`): Wert von `RUST_LOG`, falls gesetzt.
/// - `config_level` (`Option<&str>`): `[logging] level`, falls die
///   Konfiguration geladen werden konnte.
///
/// # Returns
///
/// Die Direktive, die an `EnvFilter::try_new` geht.
fn resolve_log_directive(
    cli_explicit: Option<&str>,
    rust_log: Option<&str>,
    config_level: Option<&str>,
) -> String {
    use tracing_subscriber::EnvFilter;
    if let Some(level) = cli_explicit {
        return level.to_owned();
    }
    let valid = |value: &str| !value.trim().is_empty() && EnvFilter::try_new(value).is_ok();
    if let Some(value) = rust_log.filter(|value| valid(value)) {
        return value.to_owned();
    }
    if let Some(value) = config_level.filter(|value| {
        !value.trim().eq_ignore_ascii_case(CONFIG_DEFAULT_LOG_LEVEL) && valid(value)
    }) {
        return value.to_owned();
    }
    DEFAULT_LOG_FILTER.to_owned()
}

/// Lädt best-effort den `[logging]`-Abschnitt der effektiven Konfiguration.
///
/// # Description
///
/// Läuft *vor* der Installation des Subscribers: Home auflösen
/// ([`home::resolve_home`]), vertraute Layer bestimmen
/// ([`harw_home::config_layers`]) und die Kette über
/// [`discover_config`] zusammenführen — dieselbe Kette wie `harw settings`.
/// Es wird nichts angelegt und nichts migriert: existiert das Home noch nicht
/// (Erststart, `harw init`), gelten stumm die Defaults. Andere Fehler werden
/// nur außerhalb der TUI auf `stderr` gemeldet (der Alternate Screen darf
/// nicht beschrieben werden); der eigentliche Befehl meldet eine kaputte
/// Konfiguration ohnehin selbst.
///
/// # Arguments
///
/// - `home_override` (`Option<PathBuf>`): `--home`.
/// - `tui_active` (`bool`): unterdrückt die `stderr`-Warnung.
///
/// # Returns
///
/// `Some(section)` bei erfolgreich geladener Konfiguration, sonst `None`.
fn load_logging_section(
    home_override: Option<PathBuf>,
    tui_active: bool,
) -> Option<harw_config::LoggingSection> {
    let home = home::resolve_home(home_override).ok()?;
    if !home.is_dir() {
        return None;
    }
    let loaded = harw_home::config_layers(&home)
        .map_err(|error| error.to_string())
        .and_then(|layers| discover_config(&layers).map_err(|error| error.to_string()));
    match loaded {
        Ok(config) => Some(config.harness.logging),
        Err(error) => {
            if !tui_active {
                eprintln!("harw: [logging] nicht geladen, verwende Defaults: {error}");
            }
            None
        }
    }
}

/// Wirksame Tracing-Einstellungen nach Auflösung von CLI, Umgebung und
/// Konfiguration.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TracingSettings {
    /// `EnvFilter`-Direktive (siehe [`resolve_log_directive`]).
    directive: String,
    /// `[logging] json`: Ereignisse als zeilenweises JSON formatieren.
    json: bool,
    /// `[logging] target_module_paths`: Modulpfad (`target`) je Ereignis
    /// anzeigen (entspricht `fmt().with_target(..)`).
    target_module_paths: bool,
    /// `--log-sensitive`.
    log_sensitive: bool,
}

/// Initializes the global `tracing` subscriber for the binary.
///
/// # Description
///
/// Must be called exactly once, as the very first statement after argument
/// parsing. Libraries must **never** call this function — subscriber
/// installation is the binary's exclusive responsibility.
///
/// The subscriber uses `tracing_subscriber::EnvFilter` with
/// `settings.directive` (already resolved by [`resolve_log_directive`]:
/// `--log` > `RUST_LOG` > `[logging] level` > `warn`). If the directive cannot
/// be parsed, the filter falls back to `"warn"` so the process always starts
/// with a usable subscriber.
///
/// Außerhalb der TUI geht die Ausgabe nach `stderr`; `[logging] json` schaltet
/// auf den JSON-Formatter, `[logging] target_module_paths` blendet den
/// Modulpfad ein. In der TUI wird nie ins Terminal geschrieben, sondern nur in
/// `<HARW_HOME>/logs/tui.log` (Target immer an, JSON gemäß Konfiguration).
///
/// When `log_sensitive` is `true` the global [`LOG_SENSITIVE`] flag is set and
/// a `warn!` event is emitted immediately after subscriber installation to
/// remind operators that sensitive data may appear in logs.
///
/// # Arguments
///
/// - `settings` (`&TracingSettings`): aufgelöste Einstellungen.
/// - `tui_active` (`bool`): interaktive TUI läuft (Alternate Screen).
///
/// # Panics
///
/// Panics if a global subscriber has already been installed (only possible if
/// this function is called twice, which is a programming error).
fn init_tracing(settings: &TracingSettings, tui_active: bool) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_new(&settings.directive)
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    // Im Alternate-Screen ist auch STDERR sichtbar. Ein `fmt`-Layer darf in
    // der TUI daher gar nicht installiert werden: Ein Sink-Writer schützt nur
    // diesen einen Layer, nicht spätere Writer/Layers. Die reine Registry
    // behält den Filter und die Trace-Metadaten für Instrumentierung, formatiert
    // aber *keine* Ereignisse in das Terminal.
    if tui_active {
        use tracing_subscriber::prelude::*;
        // Ins Terminal darf nichts, in eine Datei schon: `<HARW_HOME>/logs/tui.log`
        // (bei > 10 MiB beim Start nach `tui.log.1` rotiert). Ohne auflösbares
        // Home bleibt es bei der reinen Registry.
        match open_tui_log_file() {
            Some(file) => {
                let layer = tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .with_writer(std::sync::Mutex::new(file));
                if settings.json {
                    tracing_subscriber::registry()
                        .with(filter)
                        .with(layer.json())
                        .init();
                } else {
                    tracing_subscriber::registry()
                        .with(filter)
                        .with(layer)
                        .init();
                }
            }
            None => tracing_subscriber::registry().with(filter).init(),
        }
    } else if settings.json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_target(settings.target_module_paths)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(settings.target_module_paths)
            .with_writer(std::io::stderr)
            .init();
    }
    if settings.log_sensitive {
        LOG_SENSITIVE.store(true, Ordering::Relaxed);
        tracing::warn!(
            "--log-sensitive is enabled: prompts, tool-args and responses will be logged. \
             Do NOT use in production."
        );
    }
}

/// Parst die Kommandozeile und meldet, ob `--log` explizit gesetzt wurde.
///
/// Entspricht `Cli::parse()` (Fehler/`--help` beenden den Prozess über
/// `clap::Error::exit`), behält aber die `ArgMatches`, um den clap-Default
/// `info` von einem expliziten `--log info` zu unterscheiden. Geparst wird
/// gegen [`cli::command`], damit Hilfe und Fehler die deutschen Texte zeigen.
fn parse_cli() -> (Cli, bool) {
    use clap::FromArgMatches;
    let mut matches = cli::command().get_matches();
    let log_explicit = log_flag_explicit(&matches);
    match Cli::from_arg_matches_mut(&mut matches) {
        Ok(cli) => (cli, log_explicit),
        Err(error) => error.format(&mut cli::command()).exit(),
    }
}

/// `true`, wenn `--log` auf der Kommandozeile stand (auch hinter einem
/// Subcommand; clap propagiert globale Argumente zur Wurzel).
fn log_flag_explicit(matches: &clap::ArgMatches) -> bool {
    matches!(
        matches.value_source("log"),
        Some(clap::parser::ValueSource::CommandLine)
    )
}

/// Erkennt `harw kill …` direkt in den rohen Prozessargumenten.
///
/// Liefert die Argumente hinter `kill` unverändert, wenn `kill` das erste
/// Argument nach dem Programmnamen ist, sonst `None`. Läuft **vor** dem
/// clap-Parse von [`Cli`]: dessen globale Flags (`--log`, `--verbose`,
/// `--home`, …) würden killer-eigene gleichnamige Flags sonst abfangen.
fn kill_passthrough_args<I>(args: I) -> Option<Vec<OsString>>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let _program = args.next()?;
    let first = args.next()?;
    (first == "kill").then(|| args.collect())
}

/// Führt `harw kill` aus: reicht `args` unverändert an killer weiter.
///
/// killer parst selbst (inkl. `--help`), initialisiert sein eigenes Tracing
/// (`--log`) und startet den sudo-Helfer bei Bedarf als
/// `<harw> kill --helper …` neu. Deshalb läuft dieser Pfad vor
/// [`init_tracing`].
#[cfg(target_os = "linux")]
fn run_kill(args: Vec<OsString>) -> ExitCode {
    let argv = std::iter::once(OsString::from("harw kill")).chain(args);
    harw_killer::run_cli(
        argv,
        harw_killer::HelperInvocation::Subcommand(vec![OsString::from("kill")]),
    )
}

/// `harw kill` gibt es nur unter Linux (killer braucht pidfd).
#[cfg(not(target_os = "linux"))]
fn run_kill(_args: Vec<OsString>) -> ExitCode {
    eprintln!("harw: `harw kill` ist nur unter Linux verfügbar");
    ExitCode::from(2)
}

/// Der Einstieg des `harw`-Programms (`src/main.rs` ruft nur ihn auf).
///
/// # Beschreibung
/// Seit #22 ist `harw-cli` zusätzlich eine Bibliothek, damit eine nativ
/// gebaute, personalisierte harw ([`run_with_embedded_uia`]) denselben
/// Einstieg nutzen kann; das Verhalten des `harw`-Binaries ist unverändert.
pub fn main_entry() -> ExitCode {
    // The agent compiler reads the built-in definitions and the capability
    // catalog through an injected contract (`harw agent …`, `/agent …`, the
    // automatic UIA build); this process provides them.
    harw_registry_defaults::compiler_defaults::install();
    if let Some(args) = kill_passthrough_args(std::env::args_os()) {
        return run_kill(args);
    }
    let cli = match parse_cli() {
        // `harw <Root-Flags> kill …`: ebenfalls vor dem Tracing-Init an
        // killer übergeben.
        (
            Cli {
                command: Some(Command::Kill { args }),
                ..
            },
            _,
        ) => return run_kill(args),
        parsed => parsed,
    };
    let (cli, log_explicit) = cli;
    // `-C` und `--profile` gelten prozessweit und müssen vor dem ersten
    // Pfadzugriff (auch dem Laden von `[logging]`) wirken.
    if let Err(error) = apply_process_globals(&cli.global) {
        eprintln!("harw: {error}");
        return ExitCode::from(2);
    }
    let tui_active = starts_tui(&cli);
    let logging = load_logging_section(cli.global.home.clone(), tui_active).unwrap_or_default();
    let rust_log = std::env::var("RUST_LOG").ok();
    let settings = TracingSettings {
        directive: resolve_log_directive(
            log_explicit.then_some(cli.global.log.as_str()),
            rust_log.as_deref(),
            Some(logging.level.as_str()),
        ),
        json: logging.json,
        target_module_paths: logging.target_module_paths,
        log_sensitive: cli.global.log_sensitive,
    };
    init_tracing(&settings, tui_active);
    // #22: die aktive UIA bei Bedarf im Hintergrund kompilieren (blockiert
    // den Start nie; ein Fehlschlag erscheint einmal als Hinweis).
    if tui_active {
        auto_build::on_start(cli.global.home.clone());
        // Hinweis auf eine neuere Version aus `version.json`; bei Bedarf
        // prüft `harw update --check` losgelöst im Hintergrund.
        update_cmd::on_start(cli.global.home.clone());
    }
    let code = match dispatch(cli) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("harw: {error}");
            2
        }
    };
    std::process::exit(code);
}

/// Wendet die prozessweiten globalen Flags `-C/--cwd` und `--profile` an.
///
/// # Description
/// `-C DIR` wechselt das Arbeitsverzeichnis des Prozesses, sodass jeder
/// Befehl so arbeitet, als wäre `harw` in `DIR` gestartet worden.
/// `--profile NAME` legt das aktive Profil prozessweit fest
/// ([`harw_home::paths::set_profile_override`]) und hat damit Vorrang vor
/// `HARW_PROFILE`. Beides muss vor dem ersten Pfadzugriff geschehen.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn das Verzeichnis nicht gewechselt werden
/// kann oder der Profilname ungültig ist.
fn apply_process_globals(global: &GlobalArgs) -> Result<(), String> {
    if let Some(dir) = global.cwd.as_deref() {
        std::env::set_current_dir(dir).map_err(|error| {
            format!(
                "Arbeitsverzeichnis {} kann nicht gewechselt werden (-C/--cwd): {error}",
                dir.display()
            )
        })?;
    }
    if let Some(profile) = global.profile.clone() {
        harw_home::paths::set_profile_override(profile.clone()).map_err(|error| {
            format!("Profil '{profile}' kann nicht verwendet werden (--profile): {error}")
        })?;
    }
    Ok(())
}

/// `true`, wenn der Aufruf die interaktive Oberfläche (Alternate Screen)
/// startet: `harw`/`harw chat` ohne ersten Prompt sowie `harw session resume`.
fn starts_tui(cli: &Cli) -> bool {
    match &cli.command {
        None => cli.chat.prompt.is_none(),
        Some(Command::Chat(args)) => args.prompt.is_none(),
        Some(Command::Session {
            action: SessionAction::Resume { .. },
        }) => true,
        Some(_) => false,
    }
}

/// Befehlsname, wie ihn die Person tippt (für Fehlermeldungen).
///
/// # Returns
/// `"harw"` ohne Subcommand, sonst `"harw <befehl>"`.
fn command_label(command: Option<&Command>) -> String {
    let name = match command {
        None => return "harw".to_owned(),
        Some(command) => match command {
            Command::Chat(_) => "chat",
            Command::Exec(_) => "exec",
            Command::Analyze(_) => "analyze",
            Command::Session { .. } => "session",
            Command::Config { .. } => "config",
            Command::Provider { .. } => "provider",
            Command::Model { .. } => "model",
            Command::Auth { .. } => "auth",
            Command::Project { .. } => "project",
            Command::Agent { .. } => "agent",
            Command::Knowledge { .. } => "knowledge",
            Command::Jobs { .. } => "jobs",
            Command::Worker { .. } => "worker",
            Command::PrReview(_) => "pr-review",
            Command::Gateway { .. } => "gateway",
            Command::Serve { .. } => "serve",
            Command::Web { .. } => "web",
            Command::Service { .. } => "service",
            Command::Mcp { .. } => "mcp",
            Command::Channel { .. } => "channel",
            Command::Init => "init",
            Command::Onboard => "onboard",
            Command::Doctor { .. } => "doctor",
            Command::Update { .. } => "update",
            Command::Tailscale { .. } => "tailscale",
            Command::Install { .. } => "install",
            Command::Uninstall { .. } => "uninstall",
            Command::Completions(_) => "completions",
            Command::BugReport { .. } => "bug-report",
            Command::Sandbox { .. } => "sandbox",
            Command::Debug { .. } => "debug",
            Command::Kill { .. } => "kill",
            Command::Connect { .. } => "connect",
            Command::Lens { .. } => "lens",
            Command::Uia { .. } => "uia",
            Command::Catalog { .. } => "catalog",
            Command::Run { .. } => "run",
            Command::Classify { .. } => "classify",
        },
    };
    format!("harw {name}")
}

/// Lehnt Sitzungs-Flags bei Befehlen ab, die keine Sitzung starten.
///
/// # Description
/// `--mode`, `--approval`, `--model`, `--goal`, `--agent` und `--add-dir` wirken nur
/// bei `harw`/`harw chat`, `harw exec` und `harw analyze`. Bei jedem anderen
/// Befehl würden sie still ignoriert; stattdessen bricht der Aufruf mit
/// einem Hinweis ab. `analyze` startet keine Sitzung mit zusätzlichen
/// Arbeitsverzeichnissen, deshalb gilt dort `--add-dir` ebenfalls als
/// Fehler.
///
/// # Errors
/// Ein deutscher Fehlertext, der die Flags, den Befehl und die zulässigen
/// Befehle nennt.
fn reject_misplaced_session_flags(
    command: Option<&Command>,
    global: &GlobalArgs,
) -> Result<(), String> {
    let used = global.session_flags_used();
    if used.is_empty() {
        return Ok(());
    }
    match command {
        None | Some(Command::Chat(_) | Command::Exec(_)) => Ok(()),
        Some(Command::Analyze(_)) => {
            if global.add_dir.is_empty() {
                Ok(())
            } else {
                Err("`--add-dir` wirkt nur bei `harw chat` und `harw exec`, \
                     nicht bei `harw analyze`"
                    .to_owned())
            }
        }
        Some(other) => {
            let flags = used
                .iter()
                .map(|flag| format!("`{flag}`"))
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "{flags} gilt nur für `harw chat`, `harw exec` und `harw analyze`, \
                 nicht für `{}`",
                command_label(Some(other))
            ))
        }
    }
}

/// Lehnt `--json` bei Befehlen ohne JSON-Ausgabe ab.
///
/// # Description
/// JSON liefern `harw session list|show`, `harw jobs …`,
/// `harw knowledge memory|proposals` und `harw agent skills|plugins`; diese
/// Befehle (und `provider`) prüfen ihre Unterfälle selbst. Jeder andere
/// Befehl bricht mit `--json` ab, statt stillschweigend Text auszugeben.
///
/// # Errors
/// Der Fehlertext von [`Printer::require_text`] mit dem Befehlsnamen.
fn reject_unsupported_json(command: Option<&Command>, global: &GlobalArgs) -> Result<(), String> {
    if !global.json {
        return Ok(());
    }
    let printer = Printer::new(global.output());
    match command {
        Some(Command::Session {
            action: SessionAction::Resume { .. },
        }) => printer.require_text("harw session resume"),
        Some(
            Command::Session { .. }
            | Command::Jobs { .. }
            | Command::Worker { .. }
            | Command::Knowledge { .. }
            | Command::Agent { .. }
            | Command::Provider { .. },
        ) => Ok(()),
        other => printer.require_text(&command_label(other)),
    }
}

/// Meldet auf `stderr`, dass ein älterer Befehlsname umgeleitet wird.
fn legacy_hint(old: &str, new: &str) {
    eprintln!("Hinweis: `harw {old}` heißt jetzt `harw {new}`.");
}

/// Startet den Chat (`harw`, `harw chat`, `harw exec`, `harw session resume`).
///
/// # Description
/// Modus und Ziel werden *vor* dem Chat aufgelöst: ein unbekannter
/// Modusname darf keine Session starten, und ein `--goal` muss im
/// Goal-Store stehen, bevor der erste Turn Kontext einsammelt. Die Spec
/// dient hier nur dem Config-Laden (Home, cwd, Repo-Trust); die eigentliche
/// Montage baut `chat::run_chat`. `--approval` und `--model` gehen als
/// Sitzungs-Overrides an die Montage.
///
/// # Errors
/// Ein `String` bei Home-, Config-, Modus- oder Ziel-Fehlern sowie aus dem
/// Chat selbst.
fn run_chat_entry(global: &GlobalArgs, chat_args: ChatArgs) -> Result<(), String> {
    let (entry, surface) = if chat_args.prompt.is_some() {
        (EntryKind::OneShot, IngressSurface::Cli)
    } else {
        (EntryKind::Tui, IngressSurface::Tui)
    };
    let spec = local_runtime_spec(global.home.clone(), entry, surface)?;
    let startup = prepare_planning_startup(&spec, global.mode.as_deref(), global.goal.as_deref())?;
    // Dieselben Store-Instanzen weiterreichen, nicht neue öffnen: zwei
    // Schreiber auf einem Plan-Verzeichnis wären stiller Datenverlust.
    // Die Freigabepolitik aus `[policy]` baut die Montage selbst.
    let chat_startup = chat::ChatStartup {
        mode: startup.mode,
        plan: startup.services.to_runtime(),
        goal_context: startup.goal_context,
        approval: global.approval,
        model: global.model.clone(),
        agent: global.agent.clone(),
    };
    chat::run_chat(
        global.home.clone(),
        chat_args.prompt,
        chat_args.resume,
        chat_startup,
        chat::ChatOptions {
            all_projects: chat_args.all,
            verbose: global.verbose,
            add_dirs: global.add_dir.clone(),
        },
    )
}

/// `harw debug echo`: ein Durchlauf gegen den Echo-Test-Anbieter.
fn run_debug_echo(home_override: Option<PathBuf>, input: &[String]) -> Result<(), String> {
    let home = home::resolve_home(home_override)?;
    home::ensure_home(&home).map_err(|error| error.to_string())?;
    println!("{}", run_local_echo(&input.join(" "), &home)?);
    Ok(())
}

/// `harw debug classify`: zeigt die Einordnung einer Eingabezeile.
fn run_debug_classify(input: &[String]) -> Result<(), String> {
    let text = input.join(" ");
    println!(
        "{:?}",
        classify_input(&text).map_err(|error| error.to_string())?
    );
    Ok(())
}

/// Routet den geparsten Command; ohne Subcommand startet der Chat.
fn dispatch(cli: Cli) -> Result<(), String> {
    let Cli {
        global,
        chat: root_chat,
        command,
    } = cli;
    reject_misplaced_session_flags(command.as_ref(), &global)?;
    reject_unsupported_json(command.as_ref(), &global)?;
    let home_override = global.home.clone();
    run_startup_migrations(&command, home_override.clone())?;

    match command {
        None => run_chat_entry(&global, root_chat),
        Some(Command::Chat(args)) => run_chat_entry(&global, args),
        Some(Command::Exec(args)) => run_chat_entry(
            &global,
            ChatArgs {
                prompt: Some(args.prompt.join(" ")),
                resume: None,
                all: false,
            },
        ),
        Some(Command::Session { action }) => match session_cmd::run(&global, action)? {
            Some(id) => run_chat_entry(
                &global,
                ChatArgs {
                    prompt: None,
                    resume: Some(Some(id)),
                    all: false,
                },
            ),
            None => Ok(()),
        },
        Some(Command::Init) => cmd_init(home_override),
        Some(Command::Onboard) => {
            let home = home::resolve_home(home_override)?;
            home::ensure_home(&home).map_err(|error| error.to_string())?;
            onboarding::run_wizard(&home)
        }
        Some(Command::Channel {
            action: ChannelAction::Connect { channel, pair },
        }) => {
            let home = home::resolve_home(home_override)?;
            connect::run(&home, channel.as_str(), pair.as_deref())
        }
        Some(Command::Connect { channel, pair }) => {
            legacy_hint("connect", "channel connect");
            let home = home::resolve_home(home_override)?;
            connect::run(&home, channel.as_str(), pair.as_deref())
        }
        Some(Command::Doctor { config_dir }) => {
            let layers = resolve_layers(home_override.clone(), config_dir.clone())?;
            doctor(layers, home_override.clone(), config_dir)?;
            lifecycle::health(home_override)
        }
        Some(Command::Gateway {
            action: Some(action),
            ..
        }) => lifecycle::gateway_service(home_override, action),
        Some(Command::Gateway {
            action: None,
            telemetry,
        }) => gateway::run(home_override, telemetry),
        Some(Command::Serve { config_dir }) => {
            let (layers, storage_root, home) = resolve_serve_paths(home_override, config_dir)?;
            serve_mcp(layers, storage_root, home)
        }
        Some(Command::Mcp { action }) => mcp::run(home_override, action),
        Some(Command::Web {
            socket,
            system,
            systemd_socket,
            socket_group,
            tailnet,
            tailnet_port,
        }) => {
            // `harw web` kennt kein `--config-dir` mehr: der Root-Space kommt
            // ausschließlich aus `--home` bzw. `HARW_HOME` (siehe `crate::web`).
            let home = web_home(home::resolve_home(home_override))?;
            web::serve_web(
                Some(home),
                web::WebListenOptions {
                    socket,
                    system,
                    systemd_socket,
                    socket_group,
                },
                tailnet.then_some(tailnet_port),
            )
        }
        Some(Command::Project { action }) => project_trust::run(home_override, action),
        Some(Command::Config { action }) => settings::run(home_override, action),
        Some(Command::Provider { action }) => provider_cmd::run(&global, action),
        Some(Command::Model { action }) => models::run(home_override, action),
        Some(Command::Agent { action }) => agent_cmd::run(&global, action),
        Some(Command::Knowledge { action }) => knowledge_cmd::run(&global, action),
        Some(Command::Jobs { action }) => jobs_cmd::run(&global, action),
        Some(Command::Worker { action }) => worker_cmd::run(&global, action),
        Some(Command::PrReview(args)) => {
            if let Err(message) = crate::pr_review::run(&args.into()) {
                eprintln!("{message}");
                return Err(message);
            }
            Ok(())
        }
        Some(Command::Debug {
            action: DebugAction::Classify { input },
        }) => run_debug_classify(&input),
        Some(Command::Debug {
            action: DebugAction::Echo { input },
        }) => run_debug_echo(home_override, &input),
        Some(Command::Classify { input }) => {
            legacy_hint("classify", "debug classify");
            run_debug_classify(&input)
        }
        Some(Command::Run { input }) => {
            legacy_hint("run", "debug echo");
            run_debug_echo(home_override, &input)
        }
        Some(Command::Auth { action }) => auth::run(home_override, action),
        Some(Command::Completions(command)) => completions::run(command),
        Some(Command::Tailscale { action }) => tailscale_cmd::run(action),
        Some(Command::Update {
            check,
            yes,
            dismiss,
        }) => update_cmd::run(
            home_override,
            update_cmd::UpdateArgs {
                check,
                yes,
                dismiss,
            },
        ),
        Some(Command::Install { print_systemd }) => lifecycle::install(print_systemd),
        Some(Command::Service { action }) => lifecycle::service(home_override, action),
        Some(Command::Catalog { refresh }) => {
            legacy_hint("catalog", "model catalog");
            models::run(home_override, Some(ModelsAction::Catalog { refresh }))
        }
        Some(Command::Sandbox { action }) => sandbox_cmd::run(action),
        Some(Command::BugReport {
            report_type,
            title,
            area,
            failure_mode,
            task_category,
            what_happened,
            what_user_said,
            repro,
            evidence,
        }) => cmd_bug_report(
            home_override,
            report_type,
            title,
            area,
            failure_mode,
            task_category,
            what_happened,
            what_user_said,
            repro,
            evidence,
        ),
        Some(Command::Uninstall {
            scope,
            dry_run,
            yes,
        }) => {
            let scope: Vec<String> = scope.iter().map(|s| s.as_str().to_owned()).collect();
            lifecycle::uninstall(home_override, &scope, dry_run, yes)
        }
        Some(Command::Uia { action }) => match action {
            cli::UiaAction::New => {
                legacy_hint("uia new", "agent uia-new");
                agent_cmd::run(&global, AgentAction::UiaNew)
            }
        },
        Some(Command::Analyze(args)) => cmd_analyze(&global, &args),
        Some(Command::Lens { action }) => {
            legacy_hint("lens", "knowledge index");
            knowledge_cmd::run(&global, KnowledgeAction::Index { action })
        }
        // `main` leitet `harw kill` vor dem Tracing-Init an killer weiter.
        Some(Command::Kill { .. }) => {
            Err("interner Fehler: `harw kill` erreichte dispatch".to_owned())
        }
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
        // Der Modell-Katalog und der Wissensindex lesen keine Konfiguration.
        Some(
            Command::Model {
                action: Some(ModelsAction::Catalog { .. }),
            }
            | Command::Knowledge {
                action: KnowledgeAction::Index { .. },
            }
            | Command::Session {
                action: SessionAction::List { .. } | SessionAction::Show { .. },
            },
        ) => return Ok(()),
        // `analyze` liest `[tools.plan]`, `[mode]` und `[policy]` — es gehört
        // damit zu den Pfaden, die vor dem Lesen migrieren müssen. Die
        // Operationsbefehle (`jobs`, `knowledge memory|proposals`,
        // `agent skills|plugins`) montieren eine Laufzeit über die
        // Konfiguration; `session resume` startet den Chat.
        None
        | Some(
            Command::Chat(_)
            | Command::Exec(_)
            | Command::Session { .. }
            | Command::Onboard
            | Command::Connect { .. }
            | Command::Channel { .. }
            | Command::Gateway { .. }
            | Command::Config { .. }
            | Command::Provider { .. }
            | Command::Model { .. }
            | Command::Agent { .. }
            | Command::Knowledge { .. }
            | Command::Jobs { .. }
            | Command::Worker { .. }
            | Command::PrReview(_)
            | Command::Uia { .. }
            | Command::Analyze(_),
        ) => {
            let home = home::resolve_home(home_override)?;
            home::ensure_home(&home).map_err(|error| error.to_string())?;
            harw_home::config_layers(&home).map_err(|error| error.to_string())?
        }
        Some(Command::Web { .. }) => {
            let home = web_home(home::resolve_home(home_override))?;
            home::ensure_home(&home).map_err(|error| error.to_string())?;
            harw_home::config_layers(&home).map_err(|error| error.to_string())?
        }
        Some(Command::Doctor { config_dir }) | Some(Command::Serve { config_dir }) => {
            match config_dir {
                Some(dir) => vec![dir.clone()],
                None => {
                    let home = home::resolve_home(home_override)?;
                    home::ensure_home(&home).map_err(|error| error.to_string())?;
                    harw_home::config_layers(&home).map_err(|error| error.to_string())?
                }
            }
        }
        Some(
            Command::Init
            | Command::Classify { .. }
            | Command::Run { .. }
            | Command::Debug { .. }
            | Command::Completions(_)
            | Command::Tailscale { .. }
            | Command::Update { .. }
            | Command::Install { .. }
            | Command::Service { .. }
            | Command::Catalog { .. }
            | Command::Auth { .. }
            | Command::Project { .. }
            | Command::Uninstall { .. }
            | Command::Sandbox { .. }
            | Command::Mcp { .. }
            | Command::BugReport { .. }
            | Command::Lens { .. }
            | Command::Kill { .. },
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
    resolved
        .map_err(|error| format!("harw web requires a HARW home (--home or HARW_HOME): {error}"))
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
    let report = home::ensure_home(&home).map_err(|error| error.to_string())?;
    if report.created_home {
        println!("Root-Space angelegt: {}", report.home.display());
    } else {
        println!("Root-Space vorhanden: {}", report.home.display());
    }
    println!("aktives Profil: {}", report.profile_dir.display());
    println!("neu geschriebene Dateien: {}", report.written_files.len());

    let cwd = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;
    let project =
        harw_home::project::discover_project(&cwd, &[]).map_err(|error| error.to_string())?;
    if !project.root.join(".git").exists() {
        let status = ProcessCommand::new("git")
            .arg("init")
            .current_dir(&project.root)
            .status()
            .map_err(|error| {
                format!(
                    "could not run git init in {}: {error}",
                    project.root.display()
                )
            })?;
        if !status.success() {
            return Err(format!(
                "git init failed in {} with status {status}",
                project.root.display()
            ));
        }
        println!("Git-Repository angelegt: {}", project.root.display());
    }
    let project_home = harw_home::project::ProjectHome::at(&project);
    project_home.ensure().map_err(|error| error.to_string())?;
    println!(
        "Harw-Projektzustand angelegt: {}",
        project_home.dir.display()
    );
    Ok(())
}

/// Fragt eine Pflichtangabe für `harw bug-report` interaktiv ab, falls sie
/// nicht bereits per Flag übergeben wurde.
///
/// Spiegelt den `prompt_line`-Stil aus `crate::uia_bootstrap`: schreibt das
/// Label auf `stderr` (damit `stdout` für das Ergebnis frei bleibt), liest
/// eine Zeile von `stdin` und behandelt EOF als Abbruch statt als leere
/// Eingabe.
fn prompt_bug_report_field(label: &str) -> Result<String, String> {
    use std::io::Write as _;
    eprint!("{label}: ");
    std::io::stderr()
        .flush()
        .map_err(|error| format!("Bug-Report-Eingabe ausgeben: {error}"))?;
    let mut answer = String::new();
    let bytes_read = std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("Bug-Report-Eingabe lesen: {error}"))?;
    if bytes_read == 0 {
        return Err("Bug-Report-Eingabe abgebrochen (Eingabe beendet)".to_owned());
    }
    Ok(answer.trim().to_owned())
}

/// Speichert einen lokalen Bug-Report unter `<home>/bug-report/` (siehe
/// [`harw_ops::bug_report`]). Fehlende Pflichtfelder (`report_type`, `title`,
/// `area`, `failure_mode`, `what_happened`) werden interaktiv nachgefragt;
/// optionale Felder bleiben `None`, wenn nicht per Flag gesetzt.
#[allow(clippy::too_many_arguments)]
fn cmd_bug_report(
    home_override: Option<PathBuf>,
    report_type: Option<String>,
    title: Option<String>,
    area: Option<String>,
    failure_mode: Option<String>,
    task_category: Option<String>,
    what_happened: Option<String>,
    what_user_said: Option<String>,
    repro: Option<String>,
    evidence: Option<String>,
) -> Result<(), String> {
    let home = home::resolve_home(home_override)?;

    let report_type = match report_type {
        Some(value) => value,
        None => prompt_bug_report_field("Art")?,
    };
    let title = match title {
        Some(value) => value,
        None => prompt_bug_report_field("Titel")?,
    };
    let area = match area {
        Some(value) => value,
        None => prompt_bug_report_field("Bereich")?,
    };
    let failure_mode = match failure_mode {
        Some(value) => value,
        None => prompt_bug_report_field("Fehlermodus")?,
    };
    let what_happened = match what_happened {
        Some(value) => value,
        None => prompt_bug_report_field("Was ist passiert")?,
    };

    let report = harw_ops::bug_report::BugReport {
        id: harw_ops::bug_report::new_report_id(),
        report_type,
        title,
        area,
        failure_mode,
        task_category,
        what_happened,
        what_user_said,
        repro,
        evidence,
    };

    let path = harw_ops::bug_report::write_bug_report(&home, &report)
        .map_err(|error| error.to_string())?;
    println!("Fehlerbericht gespeichert: {}", path.display());
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
    home::ensure_home(&home).map_err(|error| error.to_string())?;
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
    // `harw-config` reicht Agentendefinitionen ungeparst weiter; ihr Senken
    // und die Prüfung der Auswahl gehören zur Validierung wie zuvor.
    harw_registry_defaults::ConfigAgents::from_config_validated(&config)
        .map_err(|error| error.to_string())?;

    if !config.harness.mcp_listener.enabled {
        return Err(
            "mcp_listener.enabled is false in configuration; not starting the MCP endpoint"
                .to_owned(),
        );
    }

    require_home_for_sealed_refs(&config, home.as_deref())?;
    let secret_resolver = open_serve_secret_resolver(&config, home.as_deref())?;
    // `doc.read_pdf` (docs/design/doc_read_pdf_design.md §W4): nutzt Mistral
    // OCR, wenn `serve`s Provider-Universum einen erreichbaren Mistral-Provider
    // enthält; sonst bleibt sie beim lokalen `oxidize-pdf`-Rückfall (nie fatal).
    crate::doc_ocr::install_doc_ocr(
        &config,
        home.as_deref(),
        secret_resolver
            .as_ref()
            .map(|resolver| resolver as &dyn SecretResolver),
    );
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
        // Nur das Vorhandensein eines HARW-Home entscheidet (`plans/` gibt es
        // nur unter Home). Der Home-Pfad selbst fließt hier nicht ein: der
        // Projekt-Root von `build_plan_node_services` ist laut Kommentar
        // oben bewusst das Arbeitsverzeichnis des Dienstes
        // (`std::env::current_dir()`), nicht `home_path` — `runtime_root`
        // unten ist die Stelle, die `home_path` tatsächlich verwendet.
        Some(_) => {
            let plan_config = plan_tool_config_from_section(&config.harness.tools.plan)?;
            let project_root = std::env::current_dir().map_err(|error| error.to_string())?;
            build_plan_node_services(&plan_config, &project_root)?
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
    // Verify-Befehle der Work-Driver-Wellen laufen gesandboxt unter demselben
    // Home und Arbeitsverzeichnis wie `runtime_root`. Nicht fatal: ohne Home
    // oder ohne baubaren Runner greift der bisherige Fallback-Verifier.
    let verify_runner = runtime_root
        .as_ref()
        .and_then(|root| verify_sandbox::build(&root.home, &root.cwd));
    // Kanban-Karten (Plan D2) liegen im Wissensspeicher des Profils, dessen
    // Job-Speicher dieser Dienst bedient (`<storage_root>/knowledge`). Ohne
    // HARW-Home bleiben `kanban_card`-Jobs unberührt.
    let knowledge = home.as_deref().map(|_| {
        Arc::new(harw_knowledge::KnowledgeStore::new(
            &harw_home::knowledge_dir(&storage_root),
        ))
    });
    let worker_context = Arc::new(job_worker::JobWorkerContext {
        transcript_root,
        configured_submitters,
        runtime_root,
        knowledge,
        verify_runner,
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
    // R18 P4: WorkDriver-Spur nach `[harness.jobs]` (höchstens so viele
    // Läufe gleichzeitig wie Jobs laufen dürfen, mindestens einer).
    let worker_options = job_worker::JobWorkerOptions::from_config(&config);
    let worker = spawn_job_worker_thread(worker_runtime, move || {
        job_worker::run_job_worker_with_options(
            store,
            executions,
            provider,
            plan_node_services,
            shutdown_rx,
            worker_context,
            worker_options,
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

/// Prüft, ob der Provider, den `serve` tatsächlich verwendet
/// (`config.harness.default_provider`), eine `secrets:`-Referenz trägt.
/// Ein `secrets:`-Eintrag im `credential_pool` dieses Providers zählt
/// ebenfalls (siehe [`secret_store::provider_uses_sealed_secret`]).
///
/// Anders als ein Scan über alle aktivierten Provider betrachtet diese
/// Funktion ausschließlich den Provider, den `serve_mcp` über
/// [`build_serve_provider`] tatsächlich lädt (`ModelSource`/`default_provider`,
/// siehe `harw_provider_http::build_provider_with_home`). Ein zweiter,
/// aktivierter, aber von diesem Lauf nie angesprochener `secrets:`-Provider
/// verlangt hier kein KEK.
fn active_serve_provider_uses_sealed_secret(config: &ResolvedConfig) -> bool {
    config
        .harness
        .default_provider
        .as_deref()
        .and_then(|provider_id| {
            config
                .providers
                .get(provider_id)
                .map(|provider| (provider_id, provider))
        })
        .is_some_and(|(provider_id, provider)| {
            secret_store::provider_uses_sealed_secret(provider_id, provider, &config.auth)
        })
}

/// Prüft, ob `serve` ein KEK braucht: entweder weil der tatsächlich genutzte
/// Provider (`default_provider`) `secrets:` referenziert, oder weil
/// mindestens ein konfigurierter MCP-Principal ein `secrets:`-Credential
/// referenziert.
///
/// Der Principal-Teil bleibt bewusst ein Scan über **alle** konfigurierten
/// Principals, nicht nur einen: `serve_mcp` authentifiziert jeden
/// konfigurierten Principal gleichzeitig (`build_authenticator` löst jedes
/// `credential_ref` auf, bevor der Listener bindet) — welcher Principal sich
/// später verbindet, steht beim Start nicht fest. Dieser Teil ist also
/// bereits korrekt auf „die Menge, die dieser Lauf tatsächlich verwendet"
/// verengt, nur ist diese Menge hier eben „alle Principals", nicht „ein
/// Provider".
fn configured_serve_uses_sealed_secret(config: &ResolvedConfig) -> bool {
    active_serve_provider_uses_sealed_secret(config)
        || config
            .harness
            .mcp_listener
            .principals
            .iter()
            .any(|principal| matches!(&principal.credential_ref, SecretRef::Secrets(_)))
}

/// Öffnet den einen geteilten Secret-Resolver für Provider- und MCP-Auth von
/// `serve`, verengt auf genau die Provider/Principals, die dieser Lauf
/// tatsächlich verwendet.
///
/// # Description
/// Baut eine eigene, minimale [`ResolvedConfig`]-Sicht (`resolver_config`)
/// statt der vollständigen `config` an
/// [`crate::secret_store::open_configured_secret_resolver`] weiterzureichen
/// — dieser prüft die KEK-Pflicht über **alle** in der übergebenen
/// Konfiguration enthaltenen aktivierten Provider, ein ungenutzter zweiter
/// `secrets:`-Provider ohne KEK dürfte `serve` also nicht blockieren
/// (dasselbe Muster wie
/// [`crate::secret_store::open_configured_secret_resolver_for_active_provider`]
/// für `runtime_entry`). `resolver_config` enthält deshalb höchstens zwei
/// Einträge:
/// - den tatsächlich genutzten Provider (`config.harness.default_provider`),
///   nur wenn er aktiviert ist und `secrets:` referenziert — als `auth` oder
///   als Eintrag in seinem `credential_pool`;
/// - einen synthetischen `__mcp_secret_resolver__`-Provider, nur wenn
///   mindestens ein konfigurierter MCP-Principal ein `secrets:`-Credential
///   referenziert (siehe [`configured_serve_uses_sealed_secret`] für die
///   Begründung, warum hier weiterhin alle Principals gelten).
///
/// Referenziert weder der genutzte Provider noch ein Principal `secrets:`,
/// wird nichts geöffnet — auch dann nicht, wenn ein anderer, ungenutzter
/// Provider `secrets:` referenziert. Referenziert einer von beiden es aber,
/// bleibt das Fail-Closed-Verhalten von
/// [`crate::secret_store::open_configured_secret_resolver`] unverändert: ohne
/// konfiguriertes KEK schlägt der Aufruf fehl, es gibt keinen
/// Klartext-Fallback. `[infrastructure]` wird in `resolver_config`
/// übernommen, damit AuthHub-V3-Datensätze öffnen.
fn open_serve_secret_resolver(
    config: &ResolvedConfig,
    home: Option<&Path>,
) -> Result<Option<secret_store::ConfiguredSecretResolver>, String> {
    let Some(home) = home else {
        return Ok(None);
    };

    let mut resolver_config = ResolvedConfig {
        auth: config.auth.clone(),
        infrastructure: config.infrastructure.clone(),
        ..Default::default()
    };
    let mut needs_resolver = false;

    if let Some(provider_id) = config.harness.default_provider.as_deref() {
        if let Some(provider) = config.providers.get(provider_id) {
            if secret_store::provider_uses_sealed_secret(provider_id, provider, &config.auth) {
                resolver_config
                    .providers
                    .insert(provider_id.to_owned(), provider.clone());
                needs_resolver = true;
            }
        }
    }

    if config
        .harness
        .mcp_listener
        .principals
        .iter()
        .any(|principal| matches!(&principal.credential_ref, SecretRef::Secrets(_)))
    {
        resolver_config.providers.insert(
            "__mcp_secret_resolver__".to_owned(),
            ProviderToml {
                stream: None,
                name: "__mcp_secret_resolver__".to_owned(),
                api: "openai-compatible".to_owned(),
                base_url: "https://invalid.local".to_owned(),
                auth: Some(SecretRef::Secrets("__mcp_secret_resolver__".to_owned())),
                auth_header: None,
                api_key: None,
                originator: None,
                headers: std::collections::HashMap::new(),
                models: Vec::new(),
                enabled: true,
                origin_allowlist: OriginAllowlistToml::default(),
                rate_limit: None,
                max_concurrency: None,
                default_reasoning_effort: None,
                gateway_identity_headers: false,
                request_timeout_secs: None,
                stream_idle_timeout_secs: None,
                retry_timeouts: None,
                max_tokens_field: None,
                send_reasoning_effort: None,
                strict_tools: None,
                parallel_tool_calls: None,
                allow_insecure_lan: false,
            },
        );
        needs_resolver = true;
    }

    if !needs_resolver {
        return Ok(None);
    }

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
///
/// Kommt aus [`InteractionMode::names`], damit Liste und Parser nie
/// auseinanderlaufen.
fn valid_mode_names() -> String {
    InteractionMode::names().collect::<Vec<_>>().join(", ")
}

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
/// ergibt einen Fehler, der `chat, plan, explore, work, shell` nennt.
fn resolve_startup_mode(
    requested: Option<&str>,
    configured: &str,
) -> Result<InteractionMode, String> {
    match requested {
        Some(raw) => InteractionMode::parse(raw).ok_or_else(|| {
            format!(
                "unbekannter Interaktionsmodus '{raw}'; gültig sind: {}",
                valid_mode_names()
            )
        }),
        None => InteractionMode::parse(configured).ok_or_else(|| {
            format!(
                "[mode] default = '{configured}' ist kein bekannter Interaktionsmodus; \
                 gültig sind: {}",
                valid_mode_names()
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
/// `OperationRegistry` (über [`harw_ops::register_plan_tools`] und, hinter
/// demselben Gate, [`harw_ops::register_work_driver_tools`]). Beide aus
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
pub(crate) fn plan_tool_config_from_section(
    section: &PlanSection,
) -> Result<PlanToolConfig, String> {
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

/// Registriert Kern-, Planungs- und WorkDriver-Operationen in einer frischen
/// Registry.
///
/// # Description
/// Nur noch Testhilfe: Produktionspfade finden Operationen über
/// [`harw_runtime::RuntimeAssembly::operations`]. Dieselbe [`PlanToolConfig`],
/// die [`build_plan_services`] bekommen hat, gated hier sowohl die sieben
/// Planungs-Operationen als auch die drei WorkDriver-Operationen — dasselbe
/// Gate, zwei getrennt gezählte Flächen. Ist es abgeschaltet, erscheinen
/// `plan`, `goal`, `explore`, `research`, `research_deps`, `research_web` und
/// `analyze` ebenso wenig in der Werkzeugliste wie die WorkDriver-Ops.
///
/// # Arguments
/// - `config` (`&PlanToolConfig`): das Gate; geliehen.
///
/// # Returns
/// Die gefüllte `OperationRegistry`, die Anzahl registrierter
/// Planungs-Operationen (`0` oder [`harw_ops::PLAN_TOOL_COUNT`]) und die
/// Anzahl registrierter WorkDriver-Operationen (`0` oder
/// [`harw_ops::WORK_DRIVER_TOOL_COUNT`]).
#[cfg(test)]
pub(crate) fn build_operation_registry(
    config: &PlanToolConfig,
) -> (harw_operations::registry::OperationRegistry, usize, usize) {
    let mut registry = harw_operations::registry::OperationRegistry::new();
    harw_ops::register_all(&mut registry);
    let plan_tools = harw_ops::register_plan_tools(&mut registry, config);
    let work_driver_tools = harw_ops::register_work_driver_tools(&mut registry, config);
    (registry, plan_tools, work_driver_tools)
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
        tenant: None,
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
        let startup_id = PlanId::new(STARTUP_PLAN_ID);
        // Runde 5: Nach `plan archive plan-cli` existiert der Start-Plan noch
        // im Katalog — dann wieder aktivieren statt an `PlanExists` zu scheitern.
        if plan_store.plan_by_id(&startup_id).is_ok() {
            plan_store
                .switch_plan(&startup_id, CLI_ACTOR)
                .map_err(|error| format!("Plan konnte nicht aktiviert werden: {error}"))?;
        } else {
            plan_store
                .apply(
                    PlanAction::Create {
                        plan_id: startup_id,
                        goal: statement.to_owned(),
                    },
                    CLI_ACTOR,
                )
                .map_err(|error| format!("Plan konnte nicht angelegt werden: {error}"))?;
        }
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
    config: &PlanToolConfig,
    project_root: &Path,
) -> Result<Option<Arc<job_worker::PlanNodeServices>>, String> {
    let project = harw_home::project::discover_project(project_root, &[])
        .map_err(|error| error.to_string())?;
    let project_home = harw_home::project::ProjectHome::at(&project);
    let services = build_plan_services(
        &project_home,
        config,
        DEFAULT_PLAN_SPACE,
        DEFAULT_GOAL_SPACE,
    )?;
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
    home::ensure_home(&spec.home).map_err(|error| error.to_string())?;
    let (config, _trust) = harw_runtime::load_config(spec).map_err(|error| error.to_string())?;

    let mode = resolve_startup_mode(requested_mode, &config.harness.mode.default)?;
    tracing::info!(
        mode = mode.as_str(),
        explicit = requested_mode.is_some(),
        "startup.mode.resolved"
    );

    let plan_config = plan_tool_config_from_section(&config.harness.tools.plan)?;
    let project =
        harw_home::project::discover_project(&spec.cwd, &[]).map_err(|error| error.to_string())?;
    let project_home = harw_home::project::ProjectHome::at(&project);
    let services = build_plan_services(
        &project_home,
        &plan_config,
        DEFAULT_PLAN_SPACE,
        DEFAULT_GOAL_SPACE,
    )?;

    let goal_context = match (services.goal.as_ref(), services.plan.as_ref()) {
        (Some(goal), Some(plan)) => {
            let provider: Arc<dyn ContextProvider> =
                Arc::new(GoalContextProvider::new(Arc::clone(goal), Arc::clone(plan)));
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
/// `--max-parallel` bedeuten. Die Richtung kommt aus
/// [`AnalyzeArgs::effective_order`] (`--order` bzw. die versteckten
/// Alt-Flags, deren Widersprüche clap bereits ablehnt); nur `top-down` wird
/// als Token weitergegeben, `bottom-up` ist die Vorgabe der Operation. Hier
/// abgefangen wird nur `--workspace` neben einem Crate-Namen.
///
/// # Arguments
/// - `args` (`&AnalyzeArgs`): die geparsten CLI-Flags, geliehen.
///
/// # Returns
/// Die Token-Liste für `OpInput::command("/analyze", …)`.
///
/// # Errors
/// Ein `String`, wenn `--workspace` neben einem Crate-Namen steht oder
/// `--max-parallel 0` verlangt wird (eine Welle ohne Kind ist keine Welle).
fn analyze_tokens(args: &AnalyzeArgs) -> Result<Vec<String>, String> {
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
    if matches!(args.effective_order(), AnalyzeOrder::TopDown) {
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
/// `--mode`, `--approval` und `--model` gehen als Overrides in die
/// [`RuntimeSpec`] (`mode_override`, `approval_override`, `model_override`);
/// `--agent` setzt `RuntimeSpec::active_agent`.
///
/// # Arguments
/// - `global` (`&GlobalArgs`): globale Flags (`--home`, `--mode`, `--goal`,
///   `--approval`, `--model`), geliehen.
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
fn cmd_analyze(global: &GlobalArgs, args: &AnalyzeArgs) -> Result<(), String> {
    // Flag-Widersprüche vor jeder Datei- oder Netzarbeit melden.
    let tokens = analyze_tokens(args)?;
    let mut spec =
        local_runtime_spec(global.home.clone(), EntryKind::Analyze, IngressSurface::Cli)?;
    let startup = prepare_planning_startup(&spec, global.mode.as_deref(), global.goal.as_deref())?;
    if !startup.plan_config.is_enabled() {
        return Err(analyze_plan_surface_disabled());
    }
    let plan = startup
        .services
        .to_runtime()
        .ok_or_else(analyze_plan_surface_disabled)?;
    spec.mode_override = Some(startup.mode);
    spec.approval_override = global.approval;
    spec.model_override = global.model.clone();
    if let Some(agent) = &global.agent {
        spec.active_agent = Some(agent.clone());
    }

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
/// Laufzeit-Rechte und die Diagnose-Checks gibt es drei Fälle
/// ([`doctor_home_resolution`]):
/// - `--config-dir` gesetzt: Runtime-Montage und Checks übersprungen, eine
///   Zeile `runtime_warning=skipped: --config-dir` (C6 — ein externes
///   `--config-dir` ist kein HARW-Home; ein Montageversuch dagegen wäre
///   irreführend).
/// - kein `--config-dir`, aber [`home::resolve_home`] scheitert: eine Zeile
///   `runtime_warning=<text>` — der Fehler wird gemeldet, nicht still
///   verworfen.
/// - sonst: [`print_runtime_rights`] montiert [`EntryKind::Doctor`] und
///   druckt die effektiven Rechte, anschließend führt [`run_doctor_checks`]
///   die vollständige Diagnose-Check-Liste aus (System/Sandbox/Runtime-
///   Grenzen/Service-Manager/Home aus [`harw_install::doctor::default_checks`]
///   plus die Audit-Integritäts- und KEK-Berechtigungs-Checks aus
///   `harw-secrets`) und druckt für jeden Check eine
///   `check <id>: <PASS|WARN|FAIL> — <message>`-Zeile.
///
/// # Arguments
/// - `layers` (`Vec<PathBuf>`): die Config-Layer.
/// - `home_override` (`Option<PathBuf>`): expliziter `--home`-Wert.
/// - `config_dir` (`Option<PathBuf>`): expliziter `--config-dir`-Wert.
///
/// # Errors
/// Ein `String`, wenn die Config nicht geladen oder validiert werden kann,
/// oder wenn mindestens einer der Diagnose-Checks
/// [`harw_install::doctor::CheckOutcome::Fail`] meldet — in beiden Fällen
/// beendet sich `harw doctor` mit einem von Null verschiedenen Exit-Code
/// (siehe `main`). Ein Montagefehler oder ein nicht auflösbares Home ist
/// **kein** Fehler dieses Befehls: beides erscheint als Warnzeile und
/// überspringt die Checks, ohne den Exit-Code zu beeinflussen.
fn doctor(
    layers: Vec<PathBuf>,
    home_override: Option<PathBuf>,
    config_dir: Option<PathBuf>,
) -> Result<(), String> {
    let config = discover_config(&layers).map_err(|error| error.to_string())?;
    config.validate().map_err(|error| error.to_string())?;
    // `harw-config` reicht Agentendefinitionen ungeparst weiter; ihr Senken
    // und die Prüfung der Auswahl gehören zur Validierung wie zuvor.
    harw_registry_defaults::ConfigAgents::from_config_validated(&config)
        .map_err(|error| error.to_string())?;
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

    let mut any_check_failed = false;
    match doctor_home_resolution(config_dir.as_deref(), home_override) {
        Ok(home) => {
            print_runtime_rights(&home);
            any_check_failed = run_doctor_checks(&home, &config);
        }
        Err(reason) => println!("runtime_warning={reason}"),
    }

    if any_check_failed {
        return Err("harw doctor: mindestens ein Check ist fehlgeschlagen".to_owned());
    }
    Ok(())
}

/// Führt die vollständige Doctor-Check-Liste aus und druckt für jeden Check
/// genau eine Zeile.
///
/// # Description
/// Baut die Standard-Checks über [`harw_install::doctor::default_checks`]
/// (System/Sandbox/Runtime-Tool-Grenzen/Service-Manager/Home-Existenz/Home-
/// Berechtigungen) und hängt zwei weitere an: die Audit-Integritätsprüfung
/// (§4.3 in `docs/design/secrets-and-audit.md`, Evidenz aus
/// [`audit_integrity_evidence`]) und die KEK-Schlüsseldatei-Berechtigungs-
/// prüfung (Evidenz aus [`kek_file_perms_evidence`]). Jeder Check erscheint
/// als eine Zeile `check <id>: <PASS|WARN|FAIL> — <message>`.
///
/// # Arguments
/// - `home` (`&Path`): aufgelöster Root-Space, unter dem die Home- und
///   Secrets-Checks laufen.
/// - `config` (`&ResolvedConfig`): geladene Konfiguration, für die
///   KEK-Provenienz des Berechtigungs-Checks.
///
/// # Returns
/// `true`, wenn mindestens ein Check [`harw_install::doctor::CheckOutcome::Fail`]
/// meldet — der Aufrufer beendet `harw doctor` in diesem Fall mit einem von
/// Null verschiedenen Exit-Code.
fn run_doctor_checks(home: &Path, config: &ResolvedConfig) -> bool {
    let mut checks = harw_install::doctor::default_checks(home);
    checks.push(Box::new(harw_install::doctor::AuditIntegrityCheck::new(
        audit_integrity_evidence(home),
    )));
    checks.push(Box::new(harw_install::doctor::KekFilePermsCheck::new(
        kek_file_perms_evidence(config),
    )));

    let mut any_failed = false;
    for (id, outcome) in harw_install::doctor::run_all(&checks) {
        let (label, message) = match &outcome {
            harw_install::doctor::CheckOutcome::Ok(message) => ("PASS", message.as_str()),
            harw_install::doctor::CheckOutcome::Warn(message) => ("WARN", message.as_str()),
            harw_install::doctor::CheckOutcome::Fail(message) => {
                any_failed = true;
                ("FAIL", message.as_str())
            }
        };
        println!("check {id}: {label} — {message}");
    }
    any_failed
}

/// Ermittelt die Evidenz für den Audit-Integritäts-Check (§4.3).
///
/// # Description
/// Öffnet — ohne KEK-Material zu laden, das die reine Pfadverifikation nicht
/// braucht — einen [`harw_secrets::SecretStore`] am dokumentierten
/// versiegelten-Speicher-Root `<home>/sealed-secrets` (siehe
/// `crate::secret_store::open_configured_secret_resolver`s Moduldoku für
/// diese Konvention) und ruft nacheinander
/// [`harw_secrets::SecretStore::verify_persisted_audit_chain`] und
/// [`harw_secrets::SecretStore::verify_persisted_checkpoints`] auf. Ein noch
/// nicht existierendes `audit.log`/`checkpoints.log` ist **kein**
/// Fehlschlag: ein Root-Space, der noch nie einen versiegelten
/// Geheimnisspeicher benutzt hat, sieht genauso aus
/// ([`harw_install::doctor::AuditIntegrityEvidence::Absent`]).
///
/// Ein ML-DSA-Verifikationsschlüssel ist derzeit nirgends konfigurierbar
/// (siehe `docs/design/secrets-and-audit.md` §4.2: „distinct from the KEK,
/// … should be rotatable independently" — noch ohne eigene Provenienz-
/// Konfiguration): die Signaturprüfung wird deshalb mit `None` ausdrücklich
/// übersprungen statt stillschweigend als bestanden gemeldet.
///
/// # Arguments
/// - `home` (`&Path`): aufgelöster Root-Space (`~/.harw`).
///
/// # Returns
/// Die klassifizierte Evidenz für [`harw_install::doctor::AuditIntegrityCheck`].
fn audit_integrity_evidence(home: &Path) -> harw_install::doctor::AuditIntegrityEvidence {
    use harw_install::doctor::AuditIntegrityEvidence as Evidence;
    use harw_secrets::AuditError;
    use harw_secrets::audit::chain::PersistedChainStatus;
    use harw_secrets::audit::checkpoint::PersistedCheckpointStatus;

    let store = harw_secrets::SecretStore::new(
        home.join("sealed-secrets"),
        harw_secrets::CryptoPolicy::strongest(),
        harw_secrets::KekProvenance::EnvSeed {
            var: "HARW_DOCTOR_UNUSED_KEK_SEED".to_owned(),
        },
        harw_secrets::KeyVersion::initial(),
    );

    let chain_result = store.verify_persisted_audit_chain();
    let (chain_event_count, chain_problem): (u64, Option<String>) = match &chain_result {
        Ok(PersistedChainStatus::Absent) => (0, None),
        Ok(PersistedChainStatus::Intact { event_count, .. }) => (*event_count, None),
        Err(AuditError::ChainBroken { index, .. }) => (
            u64::MAX,
            Some(format!("Audit-Kette gebrochen bei Ereignis {index}")),
        ),
        Err(other) => (u64::MAX, Some(format!("Audit-Kette unlesbar: {other}"))),
    };

    // Ohne vertrauenswürdige Ereigniszahl (Kette selbst unlesbar/gebrochen)
    // wird die Reichweitenprüfung bewusst zu einem No-op (`u64::MAX`) statt
    // gegen eine erfundene `0` zu vergleichen — siehe
    // `verify_persisted_checkpoints`s Doku.
    let checkpoint_result = store.verify_persisted_checkpoints(chain_event_count, None);
    let (checkpoint_count, signatures_checked, checkpoint_problem): (u64, bool, Option<String>) =
        match &checkpoint_result {
            Ok(PersistedCheckpointStatus::Absent) => (0, false, None),
            Ok(PersistedCheckpointStatus::Intact {
                checkpoint_count,
                signatures_checked,
            }) => (*checkpoint_count, *signatures_checked, None),
            Err(AuditError::ChainBroken { index, .. }) => (
                0,
                false,
                Some(format!("Checkpoint-Kette gebrochen bei Index {index}")),
            ),
            Err(AuditError::NonMonotonicCheckpoint { index }) => (
                0,
                false,
                Some(format!(
                    "Checkpoints nicht monoton aufsteigend bei Index {index}"
                )),
            ),
            Err(AuditError::CheckpointBeyondLog { referenced, actual }) => (
                0,
                false,
                Some(format!(
                    "Checkpoint referenziert Ereigniszahl {referenced}, Log enthält nur {actual} Ereignisse"
                )),
            ),
            Err(AuditError::InvalidCheckpointSignature { event_count }) => (
                0,
                false,
                Some(format!(
                    "ungültige Checkpoint-Signatur bei Ereigniszahl {event_count}"
                )),
            ),
            Err(other) => (0, false, Some(format!("Checkpoints unlesbar: {other}"))),
        };

    let chain_broken = matches!(chain_result, Err(AuditError::ChainBroken { .. }));
    let checkpoint_broken = matches!(
        checkpoint_result,
        Err(AuditError::ChainBroken { .. })
            | Err(AuditError::NonMonotonicCheckpoint { .. })
            | Err(AuditError::CheckpointBeyondLog { .. })
            | Err(AuditError::InvalidCheckpointSignature { .. })
    );

    // Sicherheitsereignis (tatsächlicher Bruch) geht vor bloßem Lesefehler,
    // Lesefehler geht vor „nichts da" — nie umgekehrt zusammengefasst.
    if chain_broken || checkpoint_broken {
        let reason = [chain_problem, checkpoint_problem]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
        return Evidence::Broken(reason);
    }

    if chain_problem.is_some() || checkpoint_problem.is_some() {
        let reason = [chain_problem, checkpoint_problem]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
        return Evidence::Unreadable(reason);
    }

    if chain_event_count == 0 && checkpoint_count == 0 {
        return Evidence::Absent;
    }

    Evidence::Intact {
        event_count: chain_event_count,
        checkpoint_count,
        signatures_checked,
    }
}

/// Ermittelt die Evidenz für den KEK-Schlüsseldatei-Berechtigungs-Check.
///
/// # Description
/// Ist kein KEK konfiguriert, oder ist die konfigurierte Provenienz nicht
/// `key_file` (also `keyring`/`env_seed`), ist diese Prüfung nicht
/// anwendbar — das ist bewusst kein Fehlschlag: nur die dateibasierte
/// Provenienz hat eine Dateiberechtigung, die geprüft werden könnte. Sonst
/// wird der konfigurierte Pfad (mit derselben `~/`-Expansion wie
/// `crate::secret_store::expand_leading_home`) an
/// [`harw_secrets::kek::check_key_file_permissions`] übergeben — dieselbe
/// Prüfung, die den Prozessstart beim Laden des echten KEK-Materials schon
/// heute erzwingt (siehe dortige Moduldoku).
///
/// # Arguments
/// - `config` (`&ResolvedConfig`): geladene Konfiguration.
///
/// # Returns
/// Die klassifizierte Evidenz für [`harw_install::doctor::KekFilePermsCheck`].
fn kek_file_perms_evidence(config: &ResolvedConfig) -> harw_install::doctor::KekFilePermsEvidence {
    use harw_install::doctor::KekFilePermsEvidence as Evidence;

    let Some(kek) = config.auth.kek.as_ref() else {
        return Evidence::NotApplicable;
    };
    if !matches!(&kek.provenance, harw_config::KekProvenance::KeyFile) {
        return Evidence::NotApplicable;
    }
    let Some(raw_path) = kek.key_file_path.as_deref().filter(|path| !path.is_empty()) else {
        return Evidence::Unsafe {
            path: "<nicht konfiguriert>".to_owned(),
            reason: "key_file_path fehlt trotz provenance = key_file".to_owned(),
        };
    };

    let path = expand_leading_home_for_kek_check(raw_path);
    match harw_secrets::kek::check_key_file_permissions(&path) {
        Ok(()) => Evidence::Ok {
            path: path.display().to_string(),
        },
        Err(error) => Evidence::Unsafe {
            path: path.display().to_string(),
            reason: error.to_string(),
        },
    }
}

/// Expandiert ein führendes `~/` im konfigurierten KEK-Schlüsseldateipfad
/// gegen `$HOME`.
///
/// Bildet dieselbe Konvention wie das private
/// `crate::secret_store::expand_leading_home` eigenständig nach: `harw
/// doctor` darf laut Aufgabenbereich nur die `doctor`-Funktion und ihre
/// Helfer in dieser Datei ändern, `crate::secret_store` bleibt unangetastet.
///
/// # Arguments
/// - `path` (`&str`): der konfigurierte, ggf. `~/`-präfigierte Pfad.
///
/// # Returns
/// Den expandierten Pfad, oder `path` unverändert, wenn kein `~/`-Präfix
/// vorliegt oder `$HOME` nicht gesetzt/leer ist.
fn expand_leading_home_for_kek_check(path: &str) -> PathBuf {
    let Some(remainder) = path.strip_prefix("~/") else {
        return PathBuf::from(path);
    };
    match std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        Some(home) => PathBuf::from(home).join(remainder),
        None => PathBuf::from(path),
    }
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
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn log_directive_prefers_explicit_cli_flag() {
        assert_eq!(
            resolve_log_directive(Some("info"), Some("trace"), Some("debug")),
            "info"
        );
    }

    #[test]
    fn log_directive_prefers_rust_log_over_config() {
        assert_eq!(
            resolve_log_directive(None, Some("harw_core=trace"), Some("debug")),
            "harw_core=trace"
        );
    }

    #[test]
    fn log_directive_skips_empty_or_invalid_rust_log() {
        assert_eq!(
            resolve_log_directive(None, Some("  "), Some("debug")),
            "debug"
        );
        assert_eq!(
            resolve_log_directive(None, Some("harw_core=notalevel"), Some("debug")),
            "debug"
        );
    }

    #[test]
    fn log_directive_uses_config_level_unless_default() {
        assert_eq!(resolve_log_directive(None, None, Some("error")), "error");
        assert_eq!(
            resolve_log_directive(None, None, Some("info")),
            DEFAULT_LOG_FILTER
        );
        assert_eq!(resolve_log_directive(None, None, None), DEFAULT_LOG_FILTER);
    }

    #[test]
    fn log_flag_source_distinguishes_default_from_explicit() -> TestResult {
        let default = cli::command()
            .try_get_matches_from(["harw"])
            .map_err(ctx("bare harw parses"))?;
        assert!(!log_flag_explicit(&default));
        let explicit = cli::command()
            .try_get_matches_from(["harw", "--log", "info"])
            .map_err(ctx("--log parses"))?;
        assert!(log_flag_explicit(&explicit));
        let after_subcommand = cli::command()
            .try_get_matches_from(["harw", "doctor", "--log", "debug"])
            .map_err(ctx("--log after subcommand parses"))?;
        assert!(log_flag_explicit(&after_subcommand));
        Ok(())
    }

    #[test]
    fn kill_passthrough_forwards_everything_after_kill_verbatim() {
        let os = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            kill_passthrough_args(os(&["harw", "kill", "--log", "debug", "-p", "x", "--help"])),
            Some(os(&["--log", "debug", "-p", "x", "--help"]))
        );
        assert_eq!(
            kill_passthrough_args(os(&["harw", "kill"])),
            Some(Vec::new())
        );
        assert_eq!(kill_passthrough_args(os(&["harw", "doctor", "kill"])), None);
        assert_eq!(kill_passthrough_args(os(&["harw"])), None);
    }

    #[test]
    fn cli_parses_kill_with_hyphen_args_and_help_verbatim() -> TestResult {
        let cli = Cli::try_parse_from(["harw", "kill", "-p", "sleep", "--dry-run", "--help"])
            .map_err(ctx("harw kill parses"))?;
        let Some(Command::Kill { args }) = cli.command else {
            return Err(TestError::Unexpected(format!(
                "erwartete Command::Kill, bekam {:?}",
                cli.command
            )));
        };
        let expected: Vec<OsString> = ["-p", "sleep", "--dry-run", "--help"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(args, expected);
        Ok(())
    }

    #[test]
    fn cli_parses_bare_invocation_as_chat() -> TestResult {
        let cli = Cli::try_parse_from(["harw"]).map_err(ctx("bare harw parses"))?;
        assert!(cli.command.is_none());
        Ok(())
    }

    #[test]
    fn cli_parses_doctor_with_config_dir() -> TestResult {
        let cli = Cli::try_parse_from(["harw", "doctor", "--config-dir", "/tmp/x"])
            .map_err(ctx("doctor parses"))?;
        assert!(matches!(cli.command, Some(Command::Doctor { .. })));
        Ok(())
    }

    #[test]
    fn cli_treats_unknown_token_as_chat_prompt() -> TestResult {
        // Wie `codex "prompt"`: ein freistehendes Token ist der Chat-Prompt,
        // kein unbekannter Subcommand.
        let cli =
            Cli::try_parse_from(["harw", "launch"]).map_err(ctx("free token parses as prompt"))?;
        assert!(cli.command.is_none());
        assert_eq!(cli.chat.prompt.as_deref(), Some("launch"));
        Ok(())
    }

    #[test]
    fn local_run_executes_a_completed_core_turn() -> TestResult {
        let home = unique_temp_dir("local-run-response")?;
        harw_home::ensure_home(&home).map_err(ctx("home scaffolds"))?;
        assert_eq!(
            run_local_echo("hello harness", &home).map_err(ctx("run local echo"))?,
            "echo: hello harness"
        );
        std::fs::remove_dir_all(home).map_err(ctx("remove temporary home"))?;
        Ok(())
    }

    #[test]
    fn local_run_persists_transcript_under_isolated_home() -> TestResult {
        let home = unique_temp_dir("local-run-transcript")?;
        harw_home::ensure_home(&home).map_err(ctx("home scaffolds"))?;

        assert_eq!(
            run_local_echo("persist me", &home).map_err(ctx("run local echo"))?,
            "echo: persist me"
        );

        let profile_name = harw_home::active_profile_name(&home);
        let sessions_root = home.join("profiles").join(profile_name).join("sessions");
        let transcripts = std::fs::read_dir(&sessions_root)
            .map_err(ctx("sessions directory exists"))?
            .map(|entry| {
                entry
                    .map_err(ctx("transcript directory entry"))
                    .map(|entry| entry.path())
            })
            .collect::<TestResult<Vec<_>>>()?
            .into_iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "jsonl")
            })
            .collect::<Vec<_>>();
        assert_eq!(transcripts.len(), 1, "run creates one durable transcript");
        let transcript =
            std::fs::read_to_string(&transcripts[0]).map_err(ctx("read transcript"))?;
        assert!(transcript.contains("persist me"), "user input is durable");
        assert!(
            transcript.contains("echo: persist me"),
            "assistant response is durable"
        );

        std::fs::remove_dir_all(home).map_err(ctx("remove temporary home"))?;
        Ok(())
    }

    #[test]
    fn run_subcommand_requires_input() {
        assert!(Cli::try_parse_from(["harw", "run"]).is_err());
    }

    #[test]
    fn startup_migrations_write_one_backup_and_are_idempotent() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("create temporary config directory"))?;
        let config_path = dir.path().join("config.toml");
        let original = "# existing user configuration\n[logging]\nlevel = \"info\"\n";
        std::fs::write(&config_path, original).map_err(ctx("write old config"))?;

        migrate_config_paths(std::slice::from_ref(&config_path))
            .map_err(ctx("startup migration succeeds"))?;

        let migrated =
            std::fs::read_to_string(&config_path).map_err(ctx("read migrated config"))?;
        assert!(migrated.contains("config_version = 1"), "{migrated}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml.bak.0"))
                .map_err(ctx("read migration backup"))?,
            original
        );

        migrate_config_paths(std::slice::from_ref(&config_path))
            .map_err(ctx("current config is a no-op"))?;
        assert_eq!(
            std::fs::read_to_string(&config_path).map_err(ctx("read config after second run"))?,
            migrated
        );
        assert!(
            !dir.path().join("config.toml.bak.1").exists(),
            "idempotent migration must not create another backup"
        );
        Ok(())
    }

    #[test]
    fn startup_migrations_report_the_affected_config_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("create temporary config directory"))?;
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "config_version = 2\n").map_err(ctx("write future config"))?;

        let result = migrate_config_paths(std::slice::from_ref(&config_path));
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "future config version must stop startup".into(),
            ));
        };

        assert!(
            error.contains("configuration migration failed for"),
            "{error}"
        );
        assert!(
            error.contains(&config_path.display().to_string()),
            "{error}"
        );
        assert!(error.contains("kein Migrationspfad"), "{error}");
        Ok(())
    }

    #[test]
    fn completion_path_skips_startup_migrations() -> TestResult {
        let home = tempfile::tempdir()
            .map_err(ctx("create temporary parent"))?
            .path()
            .join("absent-harw-home");
        let command = Some(Command::Completions(cli::CompletionsCommand {
            args: harw_completions::CompletionsArgs {
                shell: Some(harw_completions::Shell::Zsh),
                install: false,
                uninstall: false,
                dry_run: false,
            },
            all_binaries: false,
        }));

        run_startup_migrations(&command, Some(home.clone()))
            .map_err(ctx("completion skips migrations"))?;

        assert!(
            !home.exists(),
            "completion must not scaffold a home or write migration state"
        );
        Ok(())
    }

    fn unique_temp_dir(label: &str) -> TestResult<PathBuf> {
        let dir =
            std::env::temp_dir().join(format!("harw-cli-test-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(ctx("temp dir creates"))?;
        Ok(dir)
    }

    /// Z1-R2-03: `harw serve` muss `file:`-Credentials auflösen, die
    /// ältere `harw onboard`-Versionen (heute versiegelt über
    /// `onboarding.rs` `store_provider_key`) und `harw-oauth` unterhalb von
    /// `<home>/secrets/` angelegt haben. Ohne Home bleibt der Pfad
    /// bewusst fail-closed
    /// (`harw_provider_http::FILE_CREDENTIAL_NO_HOME_REASON`).
    #[test]
    fn build_serve_provider_resolves_file_credentials_only_with_a_home() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let home = unique_temp_dir("serve-provider-file-credential")?;
        let secrets = home.join("secrets");
        std::fs::create_dir_all(&secrets).map_err(ctx("secrets dir creates"))?;
        let token = secrets.join("gateway.key");
        std::fs::write(&token, "gateway-file-key").map_err(ctx("secret writes"))?;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))
            .map_err(ctx("secret is private"))?;

        let provider = ProviderToml {
            stream: None,
            name: "gateway".to_owned(),
            api: "openai-chat".to_owned(),
            base_url: "https://gateway.example/v1".to_owned(),
            auth: Some(SecretRef::File(
                token
                    .to_str()
                    .ok_or(TestError::Missing("UTF-8 fixture path"))?
                    .to_owned(),
            )),
            auth_header: Some("bearer".to_owned()),
            api_key: None,
            originator: None,
            headers: std::collections::HashMap::new(),
            models: vec!["model".to_owned()],
            enabled: true,
            origin_allowlist: OriginAllowlistToml::default(),
            rate_limit: None,
            max_concurrency: None,
            default_reasoning_effort: None,
            gateway_identity_headers: false,
            request_timeout_secs: None,
            stream_idle_timeout_secs: None,
            retry_timeouts: None,
            max_tokens_field: None,
            send_reasoning_effort: None,
            strict_tools: None,
            parallel_tool_calls: None,
            allow_insecure_lan: false,
        };
        let mut config = ResolvedConfig::default();
        config.harness.default_provider = Some("gateway".to_owned());
        config.harness.default_model = Some("model".to_owned());
        config.providers.insert("gateway".to_owned(), provider);

        build_serve_provider(&config, Some(home.as_path()), None).map_err(ctx(
            "file credential below <home>/secrets resolves for serve",
        ))?;
        // `Box<dyn ModelProvider>` implementiert kein `Debug` (Trait-Objekt
        // ohne Debug-Bound) — `.expect_err(..)` würde das für den Ok-Zweig
        // verlangen. Daher hier von Hand matchen und im unerwarteten
        // Ok-Fall mit einer eigenen, sprechenden Meldung abbrechen statt
        // den Provider selbst zu formatieren.
        let result = build_serve_provider(&config, None, None);
        let error = match result {
            Err(error) => error,
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "file credential must stay fail-closed without a home, got a provider instead"
                        .into(),
                ));
            }
        };
        assert!(
            !error.contains("gateway-file-key"),
            "leaked secret: {error}"
        );
        assert!(!error.contains("gateway.key"), "leaked path: {error}");

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary home"))?;
        Ok(())
    }

    // ── Planungsfläche: Composition-Root (AP W5-08) ───────────────────────────

    /// Baut die Plan-Dienste über einem frischen Home und gibt beides zurück.
    fn plan_services_over_temp_home(
        config: &PlanToolConfig,
    ) -> TestResult<(tempfile::TempDir, super::PlanServices)> {
        let home = tempfile::tempdir().map_err(ctx("create temporary home"))?;
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
        .map_err(ctx("plan services build"))?;
        Ok((home, services))
    }

    #[test]
    fn disabled_plan_surface_registers_nothing_and_creates_no_directories() -> TestResult {
        // `persist = true` bei `enabled = false`: gerade dann darf nichts auf
        // der Platte entstehen — sonst sähe ein abgeschaltetes Werkzeug beim
        // nächsten Blick ins Dateisystem benutzt aus.
        let config = PlanToolConfig {
            enabled: false,
            persist: true,
            ..PlanToolConfig::default()
        };
        assert!(
            !config.is_enabled(),
            "explizit deaktivierte Konfiguration bleibt aus"
        );

        let (home, services) = plan_services_over_temp_home(&config)?;

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

        let (registry, plan_tools, work_driver_tools) = build_operation_registry(&config);
        assert_eq!(plan_tools, 0, "geschlossenes Gate darf nichts registrieren");
        assert_eq!(
            work_driver_tools, 0,
            "geschlossenes Gate darf auch die WorkDriver-Ops nicht registrieren"
        );
        for path in [
            "/plan",
            "/goal",
            "/explore",
            "/research",
            "/research-deps",
            "/research-web",
            "/analyze",
        ] {
            assert!(
                registry.find_by_command(path).is_none(),
                "{path} darf bei geschlossenem Gate nicht auffindbar sein"
            );
        }
        Ok(())
    }

    #[test]
    fn enabled_plan_surface_registers_seven_operations_and_four_services() -> TestResult {
        let config = PlanToolConfig::enabled_defaults();
        let (_home, services) = plan_services_over_temp_home(&config)?;

        assert!(services.plan.is_some());
        assert!(services.goal.is_some());
        assert!(services.findings.is_some());
        assert!(services.to_runtime().is_some());

        let (registry, plan_tools, work_driver_tools) = build_operation_registry(&config);
        assert_eq!(plan_tools, harw_ops::PLAN_TOOL_COUNT);
        assert_eq!(work_driver_tools, harw_ops::WORK_DRIVER_TOOL_COUNT);
        for name in [
            "plan",
            "goal",
            "explore",
            "research",
            "research_deps",
            "research_web",
            "analyze",
        ] {
            assert!(
                registry.find_by_name(name).is_some(),
                "{name} wurde nicht registriert"
            );
        }
        Ok(())
    }

    #[test]
    fn persisting_plan_surface_uses_separate_plan_and_goal_roots() -> TestResult {
        let config = PlanToolConfig {
            persist: true,
            ..PlanToolConfig::enabled_defaults()
        };
        let (home, services) = plan_services_over_temp_home(&config)?;

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
        Ok(())
    }

    #[test]
    fn unknown_mode_is_rejected_with_the_list_of_valid_modes() -> TestResult {
        let result = resolve_startup_mode(Some("voelliger-unsinn"), "chat");
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "ein unbekannter Modus darf nicht still auf chat fallen".into(),
            ));
        };

        assert!(error.contains("voelliger-unsinn"), "{error}");
        for mode in ["chat", "plan", "explore", "work", "shell"] {
            assert!(error.contains(mode), "{error} nennt '{mode}' nicht");
        }
        Ok(())
    }

    #[test]
    fn explicit_mode_is_typed_and_beats_the_configured_default() -> TestResult {
        assert_eq!(
            resolve_startup_mode(Some("explore"), "work").map_err(ctx("explore parses"))?,
            InteractionMode::Explore
        );
        assert_eq!(
            resolve_startup_mode(Some(" WORK "), "chat")
                .map_err(ctx("normalisierter Name parst"))?,
            InteractionMode::Work
        );
        assert_eq!(
            resolve_startup_mode(None, "plan").map_err(ctx("[mode] default gilt ohne Flag"))?,
            InteractionMode::Plan
        );
        Ok(())
    }

    #[test]
    fn invalid_configured_mode_is_an_error_not_a_silent_chat_fallback() -> TestResult {
        let result = resolve_startup_mode(None, "wörk");
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "auch ein Konfigurationsfehler darf nicht still werden".into(),
            ));
        };
        assert!(error.contains("[mode] default"), "{error}");
        assert!(error.contains("work"), "{error}");
        Ok(())
    }

    /// `to_runtime` reicht eine Planungsfläche nur vollständig weiter — und
    /// zwar mit **denselben** Store-Instanzen, nicht mit neu geöffneten.
    #[test]
    fn test_plan_services_to_runtime_requires_all_three_stores() -> TestResult {
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
        let (_home, complete) = plan_services_over_temp_home(&config)?;
        let Some(runtime) = complete.to_runtime() else {
            return Err(TestError::Unexpected(
                "bei aktiver Planungsfläche müssen alle drei Stores vorliegen".into(),
            ));
        };
        let (Some(plan), Some(goal), Some(findings)) = (
            complete.plan.as_ref(),
            complete.goal.as_ref(),
            complete.findings.as_ref(),
        ) else {
            return Err(TestError::Unexpected(
                "die Fixture hat alle drei Stores".into(),
            ));
        };
        assert!(Arc::ptr_eq(&runtime.plan, plan), "derselbe Plan-Store");
        assert!(Arc::ptr_eq(&runtime.goal, goal), "derselbe Goal-Store");
        assert!(
            Arc::ptr_eq(&runtime.findings, findings),
            "derselbe Finding-Store"
        );
        assert!(
            runtime.plan_config.is_enabled(),
            "die Konfiguration reist mit"
        );
        assert_eq!(runtime.plan_config.max_nodes, complete.config.max_nodes);
        Ok(())
    }

    /// Der Startmodus ist ein Rückgabewert, kein Prozesszustand: zwei
    /// Auflösungen hintereinander liefern je ihren eigenen Modus.
    #[test]
    fn test_prepare_planning_startup_resolves_mode_without_global_state() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temporary home"))?;
        let cwd = tempfile::tempdir().map_err(ctx("create temporary cwd"))?;
        let spec = runtime_entry::runtime_spec(
            EntryKind::Tui,
            home.path(),
            cwd.path(),
            runtime_entry::local_principal(IngressSurface::Tui),
        );

        let explore = prepare_planning_startup(&spec, Some("explore"), None)
            .map_err(ctx("explicit explore mode resolves"))?;
        let explore_mode = explore.mode;
        assert_eq!(
            explore.services.to_runtime().is_some(),
            explore.plan_config.is_enabled(),
            "Plan-Dienste für die Montage genau dann, wenn [tools.plan] aktiv ist"
        );
        drop(explore);

        let work = prepare_planning_startup(&spec, Some("work"), None)
            .map_err(ctx("explicit work mode resolves"))?;
        assert_eq!(explore_mode, InteractionMode::Explore);
        assert_eq!(work.mode, InteractionMode::Work);
        drop(work);

        let Err(error) = prepare_planning_startup(&spec, Some("voelliger-unsinn"), None) else {
            return Err(TestError::Unexpected(
                "ein unbekannter Modus darf keinen Start ergeben".into(),
            ));
        };
        assert!(error.contains("voelliger-unsinn"), "{error}");
        Ok(())
    }

    #[test]
    fn startup_goal_is_readable_and_created_by_a_human_actor() -> TestResult {
        let config = PlanToolConfig::enabled_defaults();
        let (_home, services) = plan_services_over_temp_home(&config)?;

        let summary = seed_startup_goal(&services, "  Die Planungsfläche steht  ", STARTUP_GOAL_ID)
            .map_err(ctx("das Startziel muss anlegbar sein"))?;
        assert!(summary.contains(STARTUP_GOAL_ID), "{summary}");

        let store = services
            .goal
            .as_ref()
            .ok_or(TestError::Missing("Goal-Store vorhanden"))?;
        let goal = store
            .current()
            .map_err(ctx("Ziel ist über den Store lesbar"))?;
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
        harw_plan::goal::validate_goal_action(Some(&goal), &achieve, CLI_ACTOR).map_err(ctx(
            "ein menschlicher Akteur darf das Ziel für erreicht erklären",
        ))?;
        match harw_plan::goal::validate_goal_action(Some(&goal), &achieve, "model:test") {
            Err(harw_plan::PlanError::ActorNotAuthorized { .. }) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "ein Modell-Akteur muss abgewiesen werden, war: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn startup_goal_without_the_plan_surface_names_the_config_key() -> TestResult {
        let config = PlanToolConfig::default();
        let (_home, services) = plan_services_over_temp_home(&config)?;

        let result = seed_startup_goal(&services, "irgendein Ziel", STARTUP_GOAL_ID);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "ohne Goal-Store darf --goal nicht stillschweigend verpuffen".into(),
            ));
        };
        assert!(error.contains("tools.plan"), "{error}");
        Ok(())
    }

    #[test]
    fn startup_goal_rejects_an_empty_statement() -> TestResult {
        let config = PlanToolConfig::enabled_defaults();
        let (_home, services) = plan_services_over_temp_home(&config)?;

        assert!(seed_startup_goal(&services, "   ", STARTUP_GOAL_ID).is_err());
        Ok(())
    }

    #[test]
    fn plan_section_translates_into_one_shared_tool_config() -> TestResult {
        let section: PlanSection = toml::from_str(
            "enabled = true\npersist = true\nmax_nodes = 128\n\
             require_exploration_for = [\"coding\", \"docs\"]\n",
        )
        .map_err(ctx("gültige [tools.plan]-Sektion"))?;

        let config =
            plan_tool_config_from_section(&section).map_err(ctx("Sektion ist übersetzbar"))?;
        assert!(config.is_enabled());
        assert!(config.persist);
        assert_eq!(config.max_nodes, 128);
        assert_eq!(
            config.require_exploration_for,
            vec![PlanNodeKind::Coding, PlanNodeKind::Docs]
        );

        // Genau diese Konfiguration regiert auch die Operationen.
        let (registry, plan_tools, work_driver_tools) = build_operation_registry(&config);
        assert_eq!(plan_tools, harw_ops::PLAN_TOOL_COUNT);
        assert_eq!(work_driver_tools, harw_ops::WORK_DRIVER_TOOL_COUNT);
        assert!(registry.find_by_command("/analyze").is_some());
        Ok(())
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
    fn plan_section_with_an_unknown_node_kind_is_rejected() -> TestResult {
        let section = PlanSection {
            require_exploration_for: vec!["schreiben".to_owned()],
            ..PlanSection::default()
        };

        let result = plan_tool_config_from_section(&section);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "ein Tippfehler in require_exploration_for muss auffallen".into(),
            ));
        };
        assert!(error.contains("schreiben"), "{error}");
        Ok(())
    }

    #[test]
    fn analyze_flags_map_onto_the_operation_command_grammar() -> TestResult {
        let args = AnalyzeArgs {
            crate_name: Some("harw-core".to_owned()),
            workspace: false,
            order: AnalyzeOrder::TopDown,
            bottom_up: false,
            top_down: false,
            dry_run: true,
            max_parallel: Some(3),
        };

        assert_eq!(
            analyze_tokens(&args).map_err(ctx("Flags sind übersetzbar"))?,
            vec![
                "--dry-run".to_owned(),
                "--top-down".to_owned(),
                "--max-parallel".to_owned(),
                "3".to_owned(),
                "harw-core".to_owned(),
            ]
        );
        Ok(())
    }

    #[test]
    fn analyze_tokens_are_accepted_by_the_operation_argument_parser() -> TestResult {
        use harw_operations::FromRawArgs;

        let args = AnalyzeArgs {
            crate_name: Some("harw-core".to_owned()),
            workspace: false,
            order: AnalyzeOrder::TopDown,
            bottom_up: false,
            top_down: false,
            dry_run: true,
            max_parallel: Some(3),
        };
        let tokens = analyze_tokens(&args).map_err(ctx("Flags sind übersetzbar"))?;

        let parsed = harw_ops::analyze::AnalyzeArgs::from_raw_args(&tokens)
            .map_err(ctx("die Operation muss ihre eigene Grammatik akzeptieren"))?;
        assert_eq!(parsed.crate_name.as_deref(), Some("harw-core"));
        assert_eq!(parsed.bottom_up, Some(false));
        assert_eq!(parsed.dry_run, Some(true));
        assert_eq!(parsed.max_parallel, Some(3));
        Ok(())
    }

    #[test]
    fn analyze_legacy_top_down_flag_maps_onto_top_down_token() -> TestResult {
        let args = AnalyzeArgs {
            crate_name: None,
            workspace: false,
            order: AnalyzeOrder::BottomUp,
            bottom_up: false,
            top_down: true,
            dry_run: false,
            max_parallel: None,
        };
        assert_eq!(
            analyze_tokens(&args).map_err(ctx("Alt-Flag ist übersetzbar"))?,
            vec!["--top-down".to_owned()]
        );
        let default_order = AnalyzeArgs {
            top_down: false,
            ..args
        };
        assert!(
            analyze_tokens(&default_order)
                .map_err(ctx("Vorgabe ist übersetzbar"))?
                .is_empty(),
            "bottom-up ist die Vorgabe der Operation und braucht kein Token"
        );
        Ok(())
    }

    #[test]
    fn analyze_rejects_contradictory_scope_flags() -> TestResult {
        let workspace_and_crate = AnalyzeArgs {
            crate_name: Some("harw-core".to_owned()),
            workspace: true,
            order: AnalyzeOrder::BottomUp,
            bottom_up: false,
            top_down: false,
            dry_run: false,
            max_parallel: None,
        };
        let result = analyze_tokens(&workspace_and_crate);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "--workspace und ein Crate-Name schließen einander aus".into(),
            ));
        };
        assert!(error.contains("harw-core"), "{error}");

        let zero_parallel = AnalyzeArgs {
            crate_name: None,
            workspace: true,
            order: AnalyzeOrder::BottomUp,
            bottom_up: false,
            top_down: false,
            dry_run: false,
            max_parallel: Some(0),
        };
        assert!(analyze_tokens(&zero_parallel).is_err());
        Ok(())
    }

    /// C3: bei aktiver Planungsfläche montiert [`analyze_assembly`] eine
    /// Laufzeit, in der `/analyze` über
    /// `assembly.operations().find_by_command` auffindbar ist.
    #[test]
    fn test_analyze_assembly_registers_analyze_operation_when_plan_enabled() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temporary home"))?;
        harw_home::ensure_home(home.path()).map_err(ctx("home scaffolds"))?;
        std::fs::write(
            home.path().join("config.toml"),
            "[tools.plan]\nenabled = true\n",
        )
        .map_err(ctx("write plan-enabled config"))?;
        let cwd = tempfile::tempdir().map_err(ctx("create temporary cwd"))?;

        let spec = runtime_entry::runtime_spec(
            EntryKind::Analyze,
            home.path(),
            cwd.path(),
            runtime_entry::local_principal(IngressSurface::Cli),
        );
        let startup = prepare_planning_startup(&spec, None, None).map_err(ctx(
            "planning startup resolves over an enabled plan surface",
        ))?;
        assert!(
            startup.plan_config.is_enabled(),
            "config.toml muss [tools.plan] aktivieren"
        );
        let plan = startup.services.to_runtime().ok_or(TestError::Missing(
            "bei aktiver Fläche liegen alle drei Plan-Stores vor",
        ))?;

        let (assembly, _event_rx) =
            analyze_assembly(spec, plan, ModelSource::Echo("test".to_owned()), None)
                .map_err(ctx("analyze assembly builds over an enabled plan surface"))?;

        assert!(
            assembly.operations().find_by_command("/analyze").is_some(),
            "/analyze muss bei aktiver Planungsfläche registriert sein"
        );
        Ok(())
    }

    /// C3: eine abgeschaltete Planungsfläche lässt `harw analyze` mit der
    /// zentralen Fehlermeldung ([`analyze_plan_surface_disabled`]) scheitern.
    #[test]
    fn test_cmd_analyze_disabled_plan_surface_names_config_key() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create temporary home"))?;
        // `[tools.plan].enabled` defaults to `true` (siehe
        // `harw-config/src/plan_toml.rs::PlanSection::default`), also muss
        // die Fläche hier explizit abgeschaltet werden — sonst durchläuft
        // `cmd_analyze` die `is_enabled()`-Prüfung und versucht die Operation
        // tatsächlich auszuführen, was hier (ohne echten Workspace-Root im
        // `cwd` des Testprozesses) an einem unrelated Workspace-Graph-Fehler
        // scheitert statt an der hier geprüften Fehlermeldung.
        //
        // `[tools.plan]` muss ins **Profil**-`config.toml` geschrieben werden,
        // nicht ins Root-`config.toml` von `home`: `cmd_analyze` lädt seine
        // Konfiguration über `prepare_planning_startup` →
        // `home::ensure_home` + `harw_runtime::load_config`, und
        // `discover_config_with_restricted` (`harw-config/src/discovery.rs`)
        // läuft die vertrauten Layer (Root, dann aktives Profil) in
        // aufsteigender Präzedenz durch und ersetzt bei jedem Layer mit
        // eigenem `config.toml` `resolved.harness` **vollständig** durch die
        // frisch aus diesem Layer geparste `HarnessConfig`
        // (`resolved.harness = cfg;`). Nur eine explizite Handvoll Felder
        // (`default_provider`, `default_model`, `active_uia_definition`,
        // `onboarding`, `internal_models`) wird dabei fortgeschrieben —
        // `tools.plan` gehört nicht dazu. Das von `home::ensure_home`
        // gescaffoldete Profil-`config.toml` kennt kein `[tools.plan]`,
        // deserialisiert es also mit dem Default `enabled = true`, und dieser
        // Wert überschreibt beim Profil-Layer (dem letzten Layer hier) das
        // `enabled = false`, das nur im Root-`config.toml` stünde — die
        // Schwester `test_analyze_assembly_registers_analyze_operation_when_plan_enabled`
        // schreibt zwar ebenfalls nur ins Root-`config.toml`, besteht aber nur
        // zufällig, weil `enabled = true` dort mit dem Profil-Default
        // übereinstimmt. Root-Space zuerst scaffolden (idempotent — der
        // Aufruf in `prepare_planning_startup` überschreibt danach keine
        // vorhandene Datei mehr), dann `[tools.plan] enabled = false` in die
        // Profil-`config.toml` schreiben, die Datei, die `cmd_analyze`
        // tatsächlich zuletzt liest.
        harw_home::ensure_home(home.path()).map_err(ctx("home scaffolds"))?;
        std::fs::write(
            home.path()
                .join("profiles")
                .join("default")
                .join("config.toml"),
            "[tools.plan]\nenabled = false\n",
        )
        .map_err(ctx("write plan-disabled config"))?;
        let args = AnalyzeArgs {
            crate_name: None,
            workspace: false,
            order: AnalyzeOrder::BottomUp,
            bottom_up: false,
            top_down: false,
            dry_run: true,
            max_parallel: None,
        };
        let global = GlobalArgs {
            home: Some(home.path().to_path_buf()),
            ..GlobalArgs::default()
        };

        let result = cmd_analyze(&global, &args);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "harw analyze muss ohne `[tools.plan] enabled = true` scheitern".into(),
            ));
        };
        assert_eq!(error, analyze_plan_surface_disabled());
        Ok(())
    }

    #[test]
    fn cli_exposes_mode_and_goal_as_root_flags() -> TestResult {
        let cli = Cli::try_parse_from(["harw", "--mode", "explore", "--goal", "Bridge fertig"])
            .map_err(ctx("root flags parse"))?;

        assert_eq!(cli.global.mode.as_deref(), Some("explore"));
        assert_eq!(cli.global.goal.as_deref(), Some("Bridge fertig"));
        assert!(cli.command.is_none());
        Ok(())
    }

    #[test]
    fn valid_mode_names_lists_every_interaction_mode() {
        assert_eq!(valid_mode_names(), "chat, plan, explore, work, shell");
    }

    #[test]
    fn session_flags_are_accepted_for_chat_exec_and_analyze() -> TestResult {
        for argv in [
            vec!["harw", "--mode", "explore"],
            vec!["harw", "chat", "--model", "m1"],
            vec!["harw", "exec", "--approval", "ask", "hallo", "welt"],
            vec!["harw", "analyze", "--goal", "Ziel"],
            vec!["harw", "analyze", "--agent", "uia"],
            vec!["harw", "--agent", "planner"],
            vec!["harw", "--add-dir", "/tmp"],
        ] {
            let cli = Cli::try_parse_from(argv).map_err(ctx("session flags parse"))?;
            reject_misplaced_session_flags(cli.command.as_ref(), &cli.global)
                .map_err(ctx("session flags are allowed here"))?;
        }
        Ok(())
    }

    #[test]
    fn session_flags_are_rejected_for_other_commands() -> TestResult {
        let cli = Cli::try_parse_from(["harw", "doctor", "--mode", "explore"])
            .map_err(ctx("doctor with --mode parses"))?;
        let result = reject_misplaced_session_flags(cli.command.as_ref(), &cli.global);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "--mode bei doctor darf nicht still ignoriert werden".into(),
            ));
        };
        assert!(error.contains("--mode"), "{error}");
        assert!(error.contains("harw doctor"), "{error}");
        for allowed in ["harw chat", "harw exec", "harw analyze"] {
            assert!(error.contains(allowed), "{error} nennt '{allowed}' nicht");
        }

        let analyze = Cli::try_parse_from(["harw", "analyze", "--add-dir", "/tmp"])
            .map_err(ctx("analyze with --add-dir parses"))?;
        assert!(reject_misplaced_session_flags(analyze.command.as_ref(), &analyze.global).is_err());

        let doctor_agent = Cli::try_parse_from(["harw", "doctor", "--agent", "uia"])
            .map_err(ctx("doctor with --agent parses"))?;
        let result =
            reject_misplaced_session_flags(doctor_agent.command.as_ref(), &doctor_agent.global);
        assert!(
            matches!(&result, Err(error) if error.contains("--agent")),
            "--agent bei doctor muss abgelehnt werden, bekam {result:?}"
        );
        Ok(())
    }

    #[test]
    fn json_is_rejected_for_commands_without_json_output() -> TestResult {
        let doctor = Cli::try_parse_from(["harw", "doctor", "--json"])
            .map_err(ctx("doctor with --json parses"))?;
        let result = reject_unsupported_json(doctor.command.as_ref(), &doctor.global);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "--json bei doctor muss abgelehnt werden".into(),
            ));
        };
        assert!(error.contains("harw doctor"), "{error}");

        let root = Cli::try_parse_from(["harw", "--json"]).map_err(ctx("root --json parses"))?;
        assert!(reject_unsupported_json(root.command.as_ref(), &root.global).is_err());

        let resume = Cli::try_parse_from(["harw", "session", "resume", "abc", "--json"])
            .map_err(ctx("session resume --json parses"))?;
        assert!(reject_unsupported_json(resume.command.as_ref(), &resume.global).is_err());

        for argv in [
            vec!["harw", "session", "list", "--json"],
            vec!["harw", "jobs", "list", "--json"],
            // Vor dem Befehl, weil `memory`/`skills` alle folgenden
            // Argumente unverändert weiterreichen.
            vec!["harw", "--json", "knowledge", "memory"],
            vec!["harw", "--json", "agent", "skills"],
            vec!["harw", "doctor"],
        ] {
            let cli = Cli::try_parse_from(argv).map_err(ctx("json-capable command parses"))?;
            reject_unsupported_json(cli.command.as_ref(), &cli.global)
                .map_err(ctx("json-capable command accepts --json"))?;
        }
        Ok(())
    }

    #[test]
    fn exec_joins_prompt_words_and_is_not_a_tui_start() -> TestResult {
        let cli =
            Cli::try_parse_from(["harw", "exec", "sag", "hallo"]).map_err(ctx("exec parses"))?;
        assert!(!starts_tui(&cli));
        let Some(Command::Exec(args)) = cli.command else {
            return Err(TestError::Unexpected("erwartete Command::Exec".into()));
        };
        assert_eq!(args.prompt.join(" "), "sag hallo");

        let bare = Cli::try_parse_from(["harw"]).map_err(ctx("bare harw parses"))?;
        assert!(starts_tui(&bare));
        let resume = Cli::try_parse_from(["harw", "session", "resume", "abc"])
            .map_err(ctx("session resume parses"))?;
        assert!(starts_tui(&resume));
        Ok(())
    }

    #[test]
    fn command_label_names_new_and_legacy_commands() -> TestResult {
        assert_eq!(command_label(None), "harw");
        let bug = Cli::try_parse_from(["harw", "bug-report", "--title", "x"])
            .map_err(ctx("bug-report parses"))?;
        assert_eq!(command_label(bug.command.as_ref()), "harw bug-report");
        let legacy = Cli::try_parse_from(["harw", "catalog"]).map_err(ctx("catalog parses"))?;
        assert_eq!(command_label(legacy.command.as_ref()), "harw catalog");
        Ok(())
    }

    #[test]
    fn external_serve_config_uses_explicit_home_for_sealed_secret_store() -> TestResult {
        let config_dir = PathBuf::from("/tmp/harw-external-config");
        let home = PathBuf::from("/tmp/harw-sealed-secret-store");

        let (layers, storage_root, secret_store_home) =
            resolve_serve_paths(Some(home.clone()), Some(config_dir.clone()))
                .map_err(ctx("external config with explicit home resolves"))?;

        assert_eq!(layers, vec![config_dir.clone()]);
        assert_eq!(storage_root, config_dir);
        assert_eq!(secret_store_home, Some(home));
        Ok(())
    }

    #[test]
    fn normal_serve_config_uses_active_profile_as_storage_root() -> TestResult {
        let home = unique_temp_dir("serve-active-profile-root")?;
        harw_home::ensure_home(&home).map_err(ctx("home scaffolds"))?;
        std::fs::write(harw_home::paths::active_profile_path(&home), "mcp-worker\n")
            .map_err(ctx("active profile writes"))?;

        let (_, storage_root, secret_store_home) = resolve_serve_paths(Some(home.clone()), None)
            .map_err(ctx("normal serve paths resolve"))?;

        assert_eq!(storage_root, home.join("profiles").join("mcp-worker"));
        assert_eq!(
            storage_root.join("jobs"),
            home.join("profiles/mcp-worker/jobs")
        );
        assert_eq!(secret_store_home, Some(home.clone()));
        std::fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn normal_serve_config_rejects_an_invalid_active_profile() -> TestResult {
        let home = unique_temp_dir("serve-invalid-active-profile")?;
        std::fs::write(harw_home::paths::active_profile_path(&home), "../outside\n")
            .map_err(ctx("invalid active profile writes"))?;

        let result = resolve_serve_paths(Some(home.clone()), None);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "invalid active profile must fail path resolution".into(),
            ));
        };

        assert!(error.contains("invalid profile name"), "{error}");
        assert!(error.contains("../outside"), "{error}");
        std::fs::remove_dir_all(&home).ok();
        Ok(())
    }

    #[test]
    fn external_serve_config_rejects_sealed_refs_without_explicit_home() -> TestResult {
        let config_dir = PathBuf::from("/tmp/harw-external-config");
        let (_, _, secret_store_home) = resolve_serve_paths(None, Some(config_dir)).map_err(
            ctx("external config without home still resolves its config paths"),
        )?;
        let mut config = ResolvedConfig::default();
        config.providers.insert(
            "sealed".to_owned(),
            toml::from_str(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .map_err(ctx("valid sealed provider config"))?,
        );
        // `serve` must actually select this provider for the gate to fire —
        // the provider-scoped check only looks at `default_provider`.
        config.harness.default_provider = Some("sealed".to_owned());

        let result = require_home_for_sealed_refs(&config, secret_store_home.as_deref());
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "external config must not guess a sealed-secret root".into(),
            ));
        };
        assert!(error.contains("requires HARW_HOME"), "{error}");
        assert!(error.contains("--config-dir"), "{error}");
        Ok(())
    }

    /// Ein aktivierter `secrets:`-Provider, den `serve` gar nicht auswählt
    /// (kein `default_provider`), darf den Start nicht blockieren — ein
    /// fehlkonfigurierter Provider darf unbeteiligte Einträge nicht
    /// mitreißen.
    #[test]
    fn external_serve_config_ignores_unused_sealed_provider_without_home() -> TestResult {
        let mut config = ResolvedConfig::default();
        config.providers.insert(
            "sealed".to_owned(),
            toml::from_str(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .map_err(ctx("valid sealed provider config"))?,
        );
        config.harness.default_provider = None;

        require_home_for_sealed_refs(&config, None).map_err(ctx(
            "an enabled but unselected sealed provider must not require HARW_HOME",
        ))?;
        Ok(())
    }

    #[test]
    fn serve_fails_closed_when_listener_is_disabled() -> TestResult {
        let dir = unique_temp_dir("serve-disabled")?;
        std::fs::write(dir.join("config.toml"), "").map_err(ctx("empty config writes"))?;
        let result = serve_mcp(vec![dir.clone()], dir.clone(), None);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "serve must refuse a disabled listener".into(),
            ));
        };
        assert!(error.contains("mcp_listener.enabled is false"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn external_config_dir_rejects_sealed_provider_without_harw_home() -> TestResult {
        let mut config = ResolvedConfig::default();
        config.providers.insert(
            "sealed".to_owned(),
            toml::from_str(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .map_err(ctx("valid sealed provider config"))?,
        );
        // `serve` must actually select this provider for the gate to fire —
        // the provider-scoped check only looks at `default_provider`.
        config.harness.default_provider = Some("sealed".to_owned());

        let result = require_home_for_sealed_refs(&config, None);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "external config must not guess a sealed-secret root".into(),
            ));
        };
        assert!(error.contains("requires HARW_HOME"), "{error}");
        assert!(error.contains("--config-dir"), "{error}");

        require_home_for_sealed_refs(&config, Some(Path::new("/tmp/harw-sealed-secret-store")))
            .map_err(ctx(
                "explicit home enables the sealed provider secret store",
            ))?;
        Ok(())
    }

    #[test]
    fn ordinary_provider_refs_remain_valid_without_harw_home() -> TestResult {
        let config = ResolvedConfig::default();

        require_home_for_sealed_refs(&config, None)
            .map_err(ctx("ordinary provider references do not require HARW_HOME"))?;
        Ok(())
    }

    #[test]
    fn external_config_dir_rejects_sealed_mcp_principal_without_harw_home() -> TestResult {
        let mut config = ResolvedConfig::default();
        config
            .harness
            .mcp_listener
            .principals
            .push(harw_config::McpPrincipalToml {
                id: "sealed-mcp".to_owned(),
                credential_ref: "secrets:mcp-token"
                    .parse()
                    .map_err(ctx("valid secret ref"))?,
                tenant: "alice".to_owned(),
                workspace: "harwness".to_owned(),
                job_capabilities: Vec::new(),
            });

        let result = require_home_for_sealed_refs(&config, None);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "external config must not guess a sealed-secret root".into(),
            ));
        };
        assert!(error.contains("requires HARW_HOME"), "{error}");
        assert!(error.contains("MCP principal"), "{error}");

        require_home_for_sealed_refs(&config, Some(Path::new("/tmp/harw-sealed-secret-store")))
            .map_err(ctx(
                "explicit home enables the sealed MCP principal secret store",
            ))?;
        Ok(())
    }

    /// Baut eine Config mit zwei Providern: `"sealed"` referenziert
    /// `secrets:`, wird aber nicht ausgewählt; `"plain"` referenziert `env:`
    /// und ist `default_provider`. Für den provider-verengten Serve-Pfad darf
    /// so ein ungenutzter `sealed`-Provider den Start nicht blockieren.
    fn serve_config_with_unused_sealed_provider() -> TestResult<ResolvedConfig> {
        let mut config = ResolvedConfig::default();
        config.providers.insert(
            "sealed".to_owned(),
            toml::from_str(
                "name = \"sealed\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"secrets:provider-token\"\n",
            )
            .map_err(ctx("valid sealed provider config"))?,
        );
        config.providers.insert(
            "plain".to_owned(),
            toml::from_str(
                "name = \"plain\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"env:PLAIN_TOKEN\"\n",
            )
            .map_err(ctx("valid plain provider config"))?,
        );
        config.harness.default_provider = Some("plain".to_owned());
        Ok(config)
    }

    #[test]
    fn active_serve_provider_uses_sealed_secret_ignores_an_unselected_sealed_provider() -> TestResult
    {
        let config = serve_config_with_unused_sealed_provider()?;
        assert!(!active_serve_provider_uses_sealed_secret(&config));
        Ok(())
    }

    #[test]
    fn active_serve_provider_uses_sealed_secret_true_for_the_selected_provider() -> TestResult {
        let mut config = serve_config_with_unused_sealed_provider()?;
        config.harness.default_provider = Some("sealed".to_owned());
        assert!(active_serve_provider_uses_sealed_secret(&config));
        Ok(())
    }

    /// Kernverhalten dieses Reports: ein aktivierter, aber von `serve` nicht
    /// ausgewählter `secrets:`-Provider ohne KEK darf den Start nicht
    /// blockieren — `open_serve_secret_resolver` muss `Ok(None)` liefern,
    /// nicht fehlschlagen.
    #[test]
    fn open_serve_secret_resolver_ignores_unused_sealed_provider_without_a_kek() -> TestResult {
        let config = serve_config_with_unused_sealed_provider()?;
        let home = tempfile::tempdir().map_err(ctx("temporary home"))?;

        let resolver = open_serve_secret_resolver(&config, Some(home.path()))
            .map_err(ctx("an unused sealed provider must not require a KEK"))?;
        assert!(resolver.is_none());
        Ok(())
    }

    #[test]
    fn open_serve_secret_resolver_fails_closed_for_the_selected_sealed_provider() -> TestResult {
        let mut config = serve_config_with_unused_sealed_provider()?;
        config.harness.default_provider = Some("sealed".to_owned());
        let home = tempfile::tempdir().map_err(ctx("temporary home"))?;

        let result = open_serve_secret_resolver(&config, Some(home.path()));
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "the actually selected sealed provider must still require a KEK".into(),
            ));
        };
        assert!(error.contains("requires a configured KEK"), "{error}");
        Ok(())
    }

    /// Auch wenn `default_provider` selbst kein `secrets:` referenziert,
    /// bleibt ein `secrets:`-MCP-Principal fail-closed: jeder konfigurierte
    /// Principal ist ein Authentifizierungsziel, das `serve` beim Start
    /// tatsächlich verwendet (`build_authenticator`).
    #[test]
    fn open_serve_secret_resolver_fails_closed_for_a_sealed_mcp_principal_with_plain_provider()
    -> TestResult {
        let mut config = serve_config_with_unused_sealed_provider()?;
        config
            .harness
            .mcp_listener
            .principals
            .push(harw_config::McpPrincipalToml {
                id: "sealed-mcp".to_owned(),
                credential_ref: "secrets:mcp-token"
                    .parse()
                    .map_err(ctx("valid secret ref"))?,
                tenant: "alice".to_owned(),
                workspace: "harwness".to_owned(),
                job_capabilities: Vec::new(),
            });
        let home = tempfile::tempdir().map_err(ctx("temporary home"))?;

        let result = open_serve_secret_resolver(&config, Some(home.path()));
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "a configured sealed MCP principal must still require a KEK".into(),
            ));
        };
        assert!(error.contains("requires a configured KEK"), "{error}");
        Ok(())
    }

    #[test]
    fn open_serve_secret_resolver_returns_none_without_home_even_for_a_selected_sealed_provider()
    -> TestResult {
        let mut config = serve_config_with_unused_sealed_provider()?;
        config.harness.default_provider = Some("sealed".to_owned());

        let resolver = open_serve_secret_resolver(&config, None).map_err(ctx(
            "without --home there is nowhere to open the sealed store",
        ))?;
        assert!(resolver.is_none());
        Ok(())
    }

    /// Ein `secrets:`-Eintrag im `credential_pool` des ausgewählten Providers
    /// verlangt ein KEK, auch wenn dessen `auth` selbst `env:` ist.
    #[test]
    fn open_serve_secret_resolver_counts_a_sealed_pool_entry_of_the_selected_provider() -> TestResult
    {
        let mut config = serve_config_with_unused_sealed_provider()?;
        config.auth =
            toml::from_str("[[credential_pool.plain]]\nsecret = \"secrets:pool-token\"\n")
                .map_err(ctx("valid credential pool"))?;
        assert!(active_serve_provider_uses_sealed_secret(&config));
        let home = tempfile::tempdir().map_err(ctx("temporary home"))?;

        let result = open_serve_secret_resolver(&config, Some(home.path()));
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "a sealed pool entry of the selected provider must require a KEK".into(),
            ));
        };
        assert!(error.contains("requires a configured KEK"), "{error}");
        Ok(())
    }

    /// `[infrastructure]` erreicht den Resolver: mit einem (nicht
    /// existierenden) AuthHub-Socket öffnet `serve` den Store trotzdem, weil
    /// das Anhängen des V3-Wrappers keine Verbindung aufbaut, und ein
    /// V2-Datensatz unter dem lokalen KEK löst weiter auf.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn open_serve_secret_resolver_keeps_infrastructure() -> TestResult {
        use secrecy::ExposeSecret as _;
        use std::os::unix::fs::PermissionsExt as _;

        let home = tempfile::tempdir().map_err(ctx("temporary home"))?;
        let key_dir = home.path().join("keys");
        std::fs::create_dir_all(&key_dir).map_err(ctx("create key directory"))?;
        std::fs::set_permissions(&key_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(ctx("restrict key directory"))?;
        let key_path = key_dir.join("secrets.kek");
        std::fs::write(&key_path, b"01234567890123456789012345678901")
            .map_err(ctx("write test KEK"))?;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .map_err(ctx("restrict test KEK"))?;

        let mut config = serve_config_with_unused_sealed_provider()?;
        config.auth.kek = Some(harw_config::KekConfig {
            provenance: harw_config::KekProvenance::KeyFile,
            key_file_path: Some(key_path.display().to_string()),
            keyring_entry: None,
            env_seed_var: None,
        });
        let reference = secret_store::store_secret(
            home.path(),
            &config,
            "provider-token",
            "test",
            &secrecy::SecretString::from("v2-token".to_owned()),
        )
        .map_err(ctx("store a local V2 record"))?;
        let SecretRef::Secrets(id) = &reference else {
            return Err(TestError::Unexpected(
                "store_secret must return a secrets: reference".into(),
            ));
        };

        config.harness.default_provider = Some("sealed".to_owned());
        config.infrastructure = Some(harw_config::InfrastructureSection {
            auth_socket: Some(home.path().join("missing.sock")),
            ..harw_config::InfrastructureSection::default()
        });

        let resolver = open_serve_secret_resolver(&config, Some(home.path()))
            .map_err(ctx("a configured hub must not be dialled while opening"))?;
        let Some(resolver) = resolver else {
            return Err(TestError::Unexpected(
                "the selected sealed provider must open the store".into(),
            ));
        };
        let value = resolver
            .resolve(id)
            .map_err(ctx("a V2 record resolves with the hub configured"))?;
        assert_eq!(value.expose_secret(), "v2-token");
        Ok(())
    }

    #[test]
    fn build_authenticator_resolves_file_credentials_for_every_principal() -> TestResult {
        let dir = unique_temp_dir("build-authenticator-ok")?;
        let credential_path = dir.join("token");
        std::fs::write(&credential_path, "super-secret-token")
            .map_err(ctx("credential file writes"))?;

        let mut config = ResolvedConfig::default();
        config
            .harness
            .mcp_listener
            .principals
            .push(harw_config::McpPrincipalToml {
                id: "alice-local".to_owned(),
                credential_ref: format!("file:{}", credential_path.display())
                    .parse()
                    .map_err(ctx("valid secret ref"))?,
                tenant: "alice".to_owned(),
                workspace: "harwness".to_owned(),
                job_capabilities: Vec::new(),
            });

        let authenticator =
            build_authenticator(&config, None).map_err(ctx("credential resolves"))?;
        let principal = authenticator
            .authenticate(Some("Bearer super-secret-token"))
            .map_err(ctx("token authenticates"))?;
        assert_eq!(principal.principal_key, "alice-local");
        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn build_principal_registry_maps_capabilities_and_tenant_scope() -> TestResult {
        let mut config = ResolvedConfig::default();
        config
            .harness
            .mcp_listener
            .principals
            .push(harw_config::McpPrincipalToml {
                id: "alice-local".to_owned(),
                credential_ref: "env:HARW_TEST_UNUSED"
                    .parse()
                    .map_err(ctx("valid secret ref"))?,
                tenant: "alice".to_owned(),
                workspace: "harwness".to_owned(),
                job_capabilities: vec![
                    McpJobCapabilityToml::ReadOwn,
                    McpJobCapabilityToml::CancelOwn,
                ],
            });

        let registry = build_principal_registry(&config)
            .map_err(ctx("distinct principal ids build a registry"))?;
        let principal = registry.get("alice-local").ok_or(TestError::Missing(
            "configured principal is present in the registry",
        ))?;
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
        Ok(())
    }

    #[test]
    fn build_principal_registry_grants_submit_own_only_when_configured() -> TestResult {
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
                    credential_ref: "env:HARW_TEST_UNUSED"
                        .parse()
                        .map_err(ctx("valid secret ref"))?,
                    tenant: "alice".to_owned(),
                    workspace: "harwness".to_owned(),
                    job_capabilities,
                });
        }

        let registry = build_principal_registry(&config)
            .map_err(ctx("distinct principal ids build a registry"))?;
        assert!(
            registry
                .get("submitter")
                .ok_or(TestError::Missing("configured submitter is present"))?
                .capabilities()
                .contains(&McpJobCapability::SubmitOwn)
        );
        assert!(
            !registry
                .get("reader")
                .ok_or(TestError::Missing("configured reader is present"))?
                .capabilities()
                .contains(&McpJobCapability::SubmitOwn)
        );
        Ok(())
    }

    #[test]
    fn test_build_principal_registry_rejects_duplicate_id() -> TestResult {
        let mut config = ResolvedConfig::default();
        for workspace in ["first", "second"] {
            config
                .harness
                .mcp_listener
                .principals
                .push(harw_config::McpPrincipalToml {
                    id: "twice".to_owned(),
                    credential_ref: "env:HARW_TEST_UNUSED"
                        .parse()
                        .map_err(ctx("valid secret ref"))?,
                    tenant: "alice".to_owned(),
                    workspace: workspace.to_owned(),
                    job_capabilities: vec![McpJobCapabilityToml::SubmitOwn],
                });
        }

        match build_principal_registry(&config) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "a duplicated principal id must abort registry construction".into(),
                ));
            }
            Err(error) => {
                assert!(error.contains("'twice'"), "{error}");
                assert!(error.contains("more than once"), "{error}");
            }
        }
        Ok(())
    }

    #[test]
    fn test_dispatch_web_without_home_names_home_flag() -> TestResult {
        let result = web_home(Err("could not determine the home directory".to_owned()));
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "an unresolvable home must stop harw web".into(),
            ));
        };
        assert!(error.contains("--home"), "{error}");
        assert!(error.contains("HARW_HOME"), "{error}");
        assert!(
            error.contains("could not determine the home directory"),
            "{error}"
        );

        let home = PathBuf::from("/tmp/harw-web-home");
        assert_eq!(web_home(Ok(home.clone())), Ok(home));
        Ok(())
    }

    /// C6: ein externes `--config-dir` ist kein HARW-Home — `doctor` darf
    /// keinen Montageversuch dagegen unternehmen, sondern überspringt die
    /// Laufzeit-Rechte ausdrücklich.
    #[test]
    fn test_doctor_home_resolution_skips_runtime_rights_with_config_dir() -> TestResult {
        let config_dir = PathBuf::from("/tmp/harw-doctor-config-dir");
        let result = doctor_home_resolution(Some(config_dir.as_path()), None);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "--config-dir must skip the runtime rights assembly".into(),
            ));
        };
        assert_eq!(error, "skipped: --config-dir");

        // Auch mit einem zusätzlich gesetzten `--home` bleibt es beim Skip:
        // `--config-dir` gewinnt, das Home wird nicht stillschweigend benutzt.
        let result = doctor_home_resolution(
            Some(config_dir.as_path()),
            Some(PathBuf::from("/tmp/harw-doctor-home")),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "--config-dir must win over a coincidentally set --home".into(),
            ));
        };
        assert_eq!(error, "skipped: --config-dir");
        Ok(())
    }

    /// C6: ohne `--config-dir` löst `doctor` den Root-Space normal auf.
    #[test]
    fn test_doctor_home_resolution_resolves_explicit_home_without_config_dir() -> TestResult {
        let home = PathBuf::from("/tmp/harw-doctor-explicit-home");
        let resolved = doctor_home_resolution(None, Some(home.clone()))
            .map_err(ctx("an explicit --home resolves without --config-dir"))?;
        assert_eq!(resolved, home);
        Ok(())
    }

    #[test]
    fn test_run_startup_migrations_project_skips_home_resolution() -> TestResult {
        let parent = tempfile::tempdir().map_err(ctx("create temporary parent"))?;
        let missing_home = parent.path().join("absent-harw-home");
        let command = Cli::try_parse_from(["harw", "project", "status"])
            .map_err(ctx("project status parses"))?
            .command;

        run_startup_migrations(&command, Some(missing_home.clone()))
            .map_err(ctx("project commands do not migrate configuration"))?;
        assert!(
            !missing_home.exists(),
            "project commands must not scaffold a home during startup migrations"
        );
        Ok(())
    }

    #[test]
    fn build_authenticator_fails_closed_on_unresolvable_credential() -> TestResult {
        let mut config = ResolvedConfig::default();
        config
            .harness
            .mcp_listener
            .principals
            .push(harw_config::McpPrincipalToml {
                id: "alice-local".to_owned(),
                credential_ref: "env:HARW_TEST_MCP_TOKEN_UNSET_FOR_SURE"
                    .parse()
                    .map_err(ctx("valid secret ref"))?,
                tenant: "alice".to_owned(),
                workspace: "harwness".to_owned(),
                job_capabilities: Vec::new(),
            });

        match build_authenticator(&config, None) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "missing environment credential must fail closed".into(),
                ));
            }
            Err(error) => assert!(error.contains("alice-local"), "{error}"),
        }
        Ok(())
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
            .map_err(|()| std::io::Error::other("test observer outlives the fake listener"))?;
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_until_shutdown_stops_listener_and_worker() -> TestResult {
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
            worker_done_tx
                .send(())
                .map_err(|()| TestError::Unexpected("serve_until awaits the worker".into()))
        });

        let serve = serve_until(
            |rx| fake_listener(rx, listener_stopped_tx),
            async {
                let _ = trigger_rx.await;
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

        trigger_tx
            .send(())
            .map_err(|()| TestError::Unexpected("serve_until holds the trigger".into()))?;
        let outcome = tokio::time::timeout(Duration::from_secs(5), serve)
            .await
            .map_err(ctx("shutdown must end serve_until"))?;

        assert_eq!(outcome.listener, Ok(()));
        assert_eq!(outcome.worker, WorkerStop::Finished);
        assert_eq!(listener_stopped_rx.try_recv(), Ok(()));
        assert!(*shutdown_tx.borrow());
        worker.await.map_err(ctx("worker task joins"))??;
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_until_caps_wait_for_stuck_worker() -> TestResult {
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
        .map_err(ctx("grace period must bound serve_until"))?;

        assert_eq!(outcome.listener, Ok(()));
        assert_eq!(outcome.worker, WorkerStop::TimedOut);
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_until_worker_panic_while_serving_is_error() -> TestResult {
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
        .map_err(ctx("a vanished worker must end serve_until"))?;

        assert_eq!(outcome.worker, WorkerStop::Vanished);
        let Err(error) = outcome.listener else {
            return Err(TestError::Unexpected("dead worker is reported".into()));
        };
        assert!(error.contains("job worker stopped unexpectedly"), "{error}");
        assert_eq!(listener_stopped_rx.try_recv(), Ok(()));
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_until_listener_error_signals_worker_shutdown() -> TestResult {
        let (shutdown_tx, worker_rx) = tokio::sync::watch::channel(false);
        let (worker_done_tx, worker_done_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let mut worker_rx = worker_rx;
            while !*worker_rx.borrow() {
                if worker_rx.changed().await.is_err() {
                    break;
                }
            }
            worker_done_tx
                .send(())
                .map_err(|()| TestError::Unexpected("serve_until awaits the worker".into()))
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
        worker.await.map_err(ctx("worker task joins"))??;
        Ok(())
    }

    #[test]
    fn test_spawn_job_worker_thread_reports_completion() -> TestResult {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ctx("build worker runtime for test"))?;
        let (ran_tx, ran_rx) = std::sync::mpsc::channel();
        // `make_worker`'s `Fut: Future<Output = ()>` bound fixes the return
        // type to `()`: a failed send here can't propagate through `?` and
        // instead is logged, mirroring the same race's handling on
        // `done_tx.send` a few lines above in production code.
        let worker = spawn_job_worker_thread(runtime, move || async move {
            if ran_tx
                .send(std::thread::current().name().map(str::to_owned))
                .is_err()
            {
                tracing::debug!("test: ran_rx dropped before the worker reported");
            }
        })
        .map_err(ctx("spawn worker thread"))?;

        worker
            .handle
            .join()
            .map_err(|_| TestError::Unexpected("worker thread joins".into()))?;
        assert_eq!(
            ran_rx.recv().map_err(ctx("worker ran"))?,
            Some("harw-job-worker".to_owned())
        );
        let mut done = worker.done;
        assert_eq!(done.try_recv(), Ok(()));
        Ok(())
    }

    #[test]
    fn test_jemalloc_allocator_is_feature_gated() -> TestResult {
        // Seit #22 lebt der Allokator in der Bibliothek (`main.rs` ruft nur
        // `main_entry`), damit auch eine native, personalisierte harw ihn erbt.
        let source = include_str!("lib.rs");
        let allocator = source
            .find("#[global_allocator]")
            .ok_or(TestError::Missing("global allocator declaration present"))?;
        let gate = source
            .find("#[cfg(feature = \"jemalloc\")]")
            .ok_or(TestError::Missing("jemalloc cfg gate present"))?;
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
        Ok(())
    }

    fn unique_doctor_temp_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "harw-cli-doctor-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn expand_leading_home_for_kek_check_leaves_paths_without_a_tilde_unchanged() {
        assert_eq!(
            expand_leading_home_for_kek_check("/etc/harw/kek.seed"),
            PathBuf::from("/etc/harw/kek.seed")
        );
    }

    #[test]
    fn expand_leading_home_for_kek_check_expands_a_leading_tilde_against_home() {
        let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) else {
            // No $HOME in this environment: nothing to expand against, skip
            // rather than assert on an environment this test does not control.
            return;
        };
        let expected = PathBuf::from(home).join(".harw/kek.seed");
        assert_eq!(
            expand_leading_home_for_kek_check("~/.harw/kek.seed"),
            expected
        );
    }

    #[test]
    fn audit_integrity_evidence_reports_absent_for_a_fresh_home() -> TestResult {
        let home = unique_doctor_temp_dir("audit-absent");
        std::fs::create_dir_all(&home).map_err(ctx("create fresh temp home"))?;

        assert!(matches!(
            audit_integrity_evidence(&home),
            harw_install::doctor::AuditIntegrityEvidence::Absent
        ));

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn kek_file_perms_evidence_not_applicable_without_a_configured_kek() {
        let config = ResolvedConfig::default();
        assert!(matches!(
            kek_file_perms_evidence(&config),
            harw_install::doctor::KekFilePermsEvidence::NotApplicable
        ));
    }

    #[test]
    fn kek_file_perms_evidence_not_applicable_for_non_key_file_provenance() {
        let config = ResolvedConfig {
            auth: harw_config::AuthConfig {
                kek: Some(harw_config::KekConfig {
                    provenance: harw_config::KekProvenance::EnvSeed,
                    key_file_path: None,
                    keyring_entry: None,
                    env_seed_var: Some("HARW_TEST_SEED".to_owned()),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            kek_file_perms_evidence(&config),
            harw_install::doctor::KekFilePermsEvidence::NotApplicable
        ));
    }

    #[test]
    fn kek_file_perms_evidence_is_unsafe_when_key_file_path_is_missing() {
        let config = ResolvedConfig {
            auth: harw_config::AuthConfig {
                kek: Some(harw_config::KekConfig {
                    provenance: harw_config::KekProvenance::KeyFile,
                    key_file_path: None,
                    keyring_entry: None,
                    env_seed_var: None,
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            kek_file_perms_evidence(&config),
            harw_install::doctor::KekFilePermsEvidence::Unsafe { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn kek_file_perms_evidence_ok_for_a_0600_key_file() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let path = unique_doctor_temp_dir("kek-ok");
        std::fs::write(&path, [0u8; 32]).map_err(ctx("write test key file"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(ctx("set 0600 permissions"))?;

        let config = ResolvedConfig {
            auth: harw_config::AuthConfig {
                kek: Some(harw_config::KekConfig {
                    provenance: harw_config::KekProvenance::KeyFile,
                    key_file_path: Some(path.display().to_string()),
                    keyring_entry: None,
                    env_seed_var: None,
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(matches!(
            kek_file_perms_evidence(&config),
            harw_install::doctor::KekFilePermsEvidence::Ok { .. }
        ));

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn kek_file_perms_evidence_unsafe_for_a_group_readable_key_file() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let path = unique_doctor_temp_dir("kek-unsafe");
        std::fs::write(&path, [0u8; 32]).map_err(ctx("write test key file"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))
            .map_err(ctx("set unsafe permissions"))?;

        let config = ResolvedConfig {
            auth: harw_config::AuthConfig {
                kek: Some(harw_config::KekConfig {
                    provenance: harw_config::KekProvenance::KeyFile,
                    key_file_path: Some(path.display().to_string()),
                    keyring_entry: None,
                    env_seed_var: None,
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(matches!(
            kek_file_perms_evidence(&config),
            harw_install::doctor::KekFilePermsEvidence::Unsafe { .. }
        ));

        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn run_doctor_checks_returns_false_when_nothing_fails() -> TestResult {
        let home = unique_doctor_temp_dir("run-checks-ok");
        std::fs::create_dir_all(&home).map_err(ctx("create fresh temp home"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
                .map_err(ctx("set 0700 home permissions"))?;
        }
        let config = ResolvedConfig::default();

        assert!(!run_doctor_checks(&home, &config));

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }
}
