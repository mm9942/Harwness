//! Default-Einstieg von `harw`: Root-Space sicherstellen, Onboarding-Stand
//! prüfen, den echten Provider aufbauen und den interaktiven ratatui-Chat
//! starten.
//!
//! Fluss (spiegelt codex/hermes): Home auflösen → scaffolden → Config laden →
//! bei unvollständigem Onboarding den Wizard fahren → Provider aus der Config
//! bauen (OpenAI-kompatibel via `harw-provider-http`) → ratatui-Chat.
//!
//! Kann der echte Provider nicht aufgebaut werden (z. B. fehlender API-Key),
//! fällt der Chat auf den lokalen Echo-Bootstrap zurück, damit die Oberfläche
//! offline nutzbar bleibt.
//!
//! Für den interaktiven Pfad (kein `initial_prompt`) baut dieses Modul
//! zusätzlich die 16 `harw-ops`-Kern-Operationen (`harw_ops::register_all`)
//! und eine lokale TUI-Sandbox ([`build_tui_sandbox`]) und übergibt beides an
//! [`harw_tui::run_chat_tui`], sodass `/model list`, `/status`, `/ps`,
//! `/stop job-42` u. Ä. echt über die Operation-Adapter-Pipeline laufen statt
//! über einen hartkodierten Match.
//!
//! # Kind-Agenten und Planungsdienste im One-shot-Pfad (AP W5-06)
//! Der One-shot-Zweig ([`one_shot`]) bekommt über
//! [`build_one_shot_managed_spawner`] denselben `ManagedAgentSpawner`-Zugang
//! wie die TUI (`harw-tui/src/app.rs`, `build_tui_managed_spawner`), aber mit
//! genau den fünf eingebauten, read-only Rollen aus
//! [`harw_registry_defaults::profile::role_names`]. Jede Rolle bekommt über
//! [`OneShotChildRegistryFactory`] das Registry-Profil, das
//! [`harw_registry_defaults::profile::profile_for_role`] für sie vorsieht
//! (fail-closed: eine unbekannte Rolle fällt auf das volle
//! Standard-Profil zurück, nie umgekehrt), und ihre eingebaute Agent-IR aus
//! [`harw_registry_defaults::embedded_agents::builtin_agent_definitions`] für
//! die Tool-Aktivierung, das Budget und die Pause-Sperre. Damit werden
//! `explore`, `research_deps`, `research_web` und `analyze` auch ohne TUI
//! wirksam. Bereits vorhandene Planungsdienste (Plan-/Goal-/Finding-Store)
//! reicht [`run_chat`] optional als [`OneShotPlanServices`] durch, statt sie
//! selbst zu öffnen — ein zweiter Store auf demselben Verzeichnis wäre ein
//! Datenverlust-Risiko.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use harw_agent_dsl::roles::AgentRoleId;
use harw_config::{ResolvedConfig, discover_config};
use harw_core::{
    AgentSession, ApprovalResolution, ChildLimits, ChildRegistryFactory, ManagedAgentSpawner,
    ModelMessage, ModelProvider, SessionManager, SpawnContext, StateStore, TranscriptStateStore,
    TurnInput, TurnOutcome, resume_after_approval, run_turn,
};
use harw_observe::TraceContext;
use harw_operations::{
    OpContext, ServiceMap, adapter::ModelToolProvider, registry::OperationRegistry,
};
use harw_sandbox::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_session_store::TranscriptStore;
use harw_types::{AgentRole, ApprovalActor, SessionId, TenantId, ThreadRef, WorkspaceId};
use tokio::runtime::Builder;
use uuid::Uuid;

use crate::home::resolve_home;
use crate::resume::{discover_sessions, prompt_for_session, resolve_session_selector};

const ONE_SHOT_APPROVAL_DENIAL: &str = "one-shot mode is non-interactive; approval-gated write or shell calls are denied. \
     Re-run `harw chat` without an initial prompt to review and approve the request interactively.";
const PROMPT_RESUME_CONFLICT: &str = "cannot combine an initial prompt with --resume; use --resume without a prompt for interactive session recovery, or omit --resume for one-shot mode.";
const RESUME_SELECTION_CANCELLED: &str =
    "session resume cancelled; no session was selected, so a new session was not started.";

fn one_shot_approval_actor() -> ApprovalActor {
    ApprovalActor::Operator {
        id: "local-cli".to_owned(),
    }
}

fn one_shot_approval_rejection() -> ApprovalResolution {
    ApprovalResolution::Reject {
        reason: ONE_SHOT_APPROVAL_DENIAL.to_owned(),
    }
}

/// Bereits konstruierte Plan-/Ziel-/Recherche-Dienste für den One-shot-Pfad.
///
/// # Beschreibung
/// AP W5-06. Bündelt genau die vier Dienste, die
/// [`harw_plan_bridge::register_plan_services`] gemeinsam in eine
/// [`ServiceMap`] einträgt (`plan_store`, `goal_store`, `finding_store`,
/// `plan_config`). Die Composition-Root (`harw-cli/src/main.rs`) baut diese
/// Dienste **genau einmal** gegen das aktive Profil und reicht sie über
/// [`run_chat`] hier durch — `chat.rs` öffnet selbst **niemals** einen
/// zweiten Store auf demselben Verzeichnis, weil zwei nebenläufige
/// Store-Instanzen auf derselben Datei ein Datenverlust-Risiko wären.
///
/// `None` an der `run_chat`-Schnittstelle bedeutet: die Composition-Root hat
/// keine Planungsdienste konfiguriert (z. B. `[tools.plan] enabled = false`
/// oder kein aktives Profil) — `plan`/`goal`/`explore`/`research_*`/`analyze`
/// beantworten den One-shot-Turn dann mit `OpError::NotAvailable`, statt ihn
/// abzubrechen.
///
/// # Nebenläufigkeit
/// `Clone`: alle vier Felder sind `Arc`-Zeiger bzw. ein kleiner Werttyp: ein
/// Klon teilt denselben zugrundeliegenden Store, dupliziert ihn nie.
#[derive(Clone)]
pub struct OneShotPlanServices {
    /// Der Plan-Store der aktiven Profilsitzung.
    pub plan_store: Arc<dyn harw_plan::PlanStore>,
    /// Der Goal-Store der aktiven Profilsitzung.
    pub goal_store: Arc<dyn harw_plan::goal::GoalStore>,
    /// Die Ablage für Recherche-/Explorations-Artefakte (`/explore`,
    /// `/research_deps`, `/research_web`, `/analyze`).
    pub finding_store: Arc<harw_plan_bridge::FindingStore>,
    /// Die geltende Plan-Tool-Konfiguration (`[tools.plan]`).
    pub plan_config: harw_plan::PlanToolConfig,
    /// Kontext-Beitragende, die in **jeden** Turn einfließen — insbesondere der
    /// Ziel-Kontext aus `harw_plan_bridge::GoalContextProvider`.
    ///
    /// Als Trait-Objekt, damit `harw-tui` sie entgegennehmen kann, ohne
    /// `harw-plan-bridge` zu kennen. Ohne diesen Weg überlebt ein per `--goal`
    /// gesetztes Ziel zwar im Store, erreicht aber keinen Modell-Turn.
    pub context_providers: Vec<Arc<dyn harw_extension_api::ContextProvider>>,
    /// Zusätzliche Freigabe-Politiken (aus `[policy] require_approval_for`),
    /// die **hinter** die bestehenden gehängt werden. Anhängen kann nie
    /// lockern: die erste Nicht-`Allow`-Entscheidung gewinnt.
    pub approval_handlers: Vec<Arc<dyn harw_extension_api::ApprovalHandler>>,
    /// Startmodus aus `--mode` bzw. `[mode] default`; `None` behält den
    /// Session-Default.
    pub initial_mode: Option<harw_core::InteractionMode>,
}

/// Startet den Default-Chat-Pfad.
///
/// # Arguments
/// - `home_override` (`Option<PathBuf>`): expliziter Root-Space (`--home`).
/// - `initial_prompt` (`Option<String>`): einmaliger Eröffnungs-Prompt; ohne
///   diesen wird der interaktive ratatui-Chat betreten.
/// - `resume_selection` (`Option<Option<String>>`): vorhandene Session
///   fortsetzen; ohne Selector wird sie interaktiv ausgewählt.
/// - `plan_services` (`Option<OneShotPlanServices>`): bereits von der
///   Composition-Root gebaute Planungsdienste (AP W5-06). Nur der
///   One-shot-Zweig (`initial_prompt.is_some()`) verwendet sie; der
///   interaktive Zweig baut seine eigene Ausstattung unverändert selbst.
///   `None` lässt `plan`/`goal`/`explore`/`research_*`/`analyze` im
///   One-shot-Turn mit `OpError::NotAvailable` antworten.
///
/// # Errors
/// Ein `String` mit menschenlesbarer Ursache bei Home-Auflösung, Scaffolding,
/// Config-Ladefehler, TUI- oder Turn-Ausführung.
pub fn run_chat(
    home_override: Option<PathBuf>,
    initial_prompt: Option<String>,
    resume_selection: Option<Option<String>>,
    plan_services: Option<OneShotPlanServices>,
) -> Result<(), String> {
    validate_chat_mode(initial_prompt.as_deref(), resume_selection.as_ref())?;

    let home = resolve_home(home_override)?;
    harw_home::ensure_home(&home).map_err(|error| error.to_string())?;

    let layers = harw_home::config_layers(&home).map_err(|error| error.to_string())?;
    let mut config = discover_config(&layers).map_err(|error| error.to_string())?;

    // Erststart-Fluss: nur wenn der Home-Workspace noch nicht einsatzbereit ist,
    // führt der Wizard durch die Einrichtung. „Einsatzbereit" heißt: die
    // `onboarding.seen`-Flags sind vollständig ODER es sind bereits ein
    // Default-Provider **und** ein Default-Modell gesetzt (robust gegen
    // driftende Flags — sobald das Home ordentlich steht, startet direkt der
    // Chat, ohne erneute Provider/Modell-Abfrage).
    let workspace_ready = config.harness.onboarding.seen.is_complete()
        || config.harness.default_provider.is_some()
        || !config.providers.is_empty();
    if !workspace_ready {
        crate::onboarding::run_wizard(&home)?;
        config = discover_config(&layers).map_err(|error| error.to_string())?;
    }

    config.validate().map_err(|error| error.to_string())?;
    let runtime_config = Arc::new(config);
    let executable_agent = selected_executable_agent(runtime_config.as_ref())?;
    let model = build_model(&home, runtime_config.as_ref())?;

    match initial_prompt {
        Some(prompt) => one_shot(model, &home, &prompt, executable_agent, plan_services),
        None => {
            let sessions_root = active_profile_sessions_root(&home)?;
            let existing_session_id =
                resolve_startup_resume_selection(&sessions_root, resume_selection)?;
            let resume_selector = ProfileResumeSelector::new(sessions_root);
            let mut operations = OperationRegistry::new();
            harw_ops::register_all(&mut operations);
            // Die sechs Planungs-Operationen gehören in **jede** Oberfläche,
            // nicht nur in `harw analyze`. Ohne diesen Aufruf liegen Plan-,
            // Goal- und Finding-Store zwar in der ServiceMap, aber keine
            // Operation kann sie erreichen — die Dienste hätten keinen
            // Aufrufer. Das Gate steckt in der Konfiguration selbst:
            // `register_plan_tools` ist ein No-op, solange
            // `[tools.plan] enabled = false`.
            if let Some(services) = plan_services.as_ref() {
                let registered =
                    harw_ops::register_plan_tools(&mut operations, &services.plan_config);
                tracing::info!(registered, "chat.plan_tools.registered");
            }
            let cwd = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;
            let assembled = harw_registry_defaults::assemble_default_registry(cwd)
                .map_err(|error| error.to_string())?;
            let sandbox = build_tui_sandbox(&assembled.project.project_root)?;
            let state_store: Box<dyn harw_core::StateStore> =
                Box::new(build_cli_state_store(&home)?);
            let job_store_root = active_profile_job_store_root(&home)?;
            let job_store = Arc::new(harw_session_store::JobStore::new(&job_store_root));
            let memory = build_memory(&home);
            // Die Plan-Dienste an die TUI durchreichen — **dieselben**
            // `Arc`-Instanzen, die auch die ServiceMap trägt. Ohne diesen
            // Schritt blieben `PlanGraphCell` und `GoalCell` toter Code, und
            // Ziel-Kontext wie Freigabe-Politik erreichten keinen Turn.
            let tui_plan_services =
                plan_services
                    .as_ref()
                    .map(|services| harw_tui::app::TuiPlanServices {
                        plan_store: Arc::clone(&services.plan_store),
                        goal_store: Arc::clone(&services.goal_store),
                        context_providers: services.context_providers.clone(),
                        approval_handlers: services.approval_handlers.clone(),
                        initial_mode: services.initial_mode,
                    });
            harw_tui::app::run_chat_tui_resumable_with_plan(
                model,
                state_store,
                job_store,
                Arc::clone(&runtime_config),
                executable_agent.cloned(),
                operations,
                sandbox,
                memory,
                existing_session_id,
                Some(&resume_selector),
                tui_plan_services,
            )
            .map_err(|error| error.to_string())
        }
    }
}

/// Resolves the configured executable agent policy without silently selecting
/// another definition when the requested ID is absent.
fn selected_executable_agent(
    config: &ResolvedConfig,
) -> Result<Option<&harw_agent_dsl::ExecutableAgentIr>, String> {
    let Some(definition_id) = config.harness.active_agent_definition.as_deref() else {
        return Ok(None);
    };

    config
        .executable_agents
        .get(definition_id)
        .ok_or_else(|| {
            format!(
                "active agent definition `{definition_id}` was not found among discovered executable agents"
            )
        })
        .map(Some)
}

/// Prevents one-shot invocations from silently discarding a requested resume.
///
/// A prompt selects the non-interactive one-shot path, while `--resume`
/// requires the interactive TUI. Keeping this check before home/config work
/// makes the failure deterministic and avoids any session-side effect.
fn validate_chat_mode(
    initial_prompt: Option<&str>,
    resume_selection: Option<&Option<String>>,
) -> Result<(), String> {
    if initial_prompt.is_some() && resume_selection.is_some() {
        return Err(PROMPT_RESUME_CONFLICT.to_owned());
    }

    Ok(())
}

/// CLI-owned bridge from durable profile transcripts to the TUI `/resume`
/// runtime boundary.
struct ProfileResumeSelector {
    sessions_root: PathBuf,
}

impl ProfileResumeSelector {
    fn new(sessions_root: PathBuf) -> Self {
        Self { sessions_root }
    }

    fn discover(&self) -> Result<Vec<crate::resume::DiscoveredSession>, String> {
        discover_sessions(&self.sessions_root).map_err(|error| error.to_string())
    }
}

impl harw_tui::app::ResumeSessionSelector for ProfileResumeSelector {
    fn available_sessions(&self) -> Result<Vec<SessionId>, String> {
        self.discover()
            .map(|sessions| sessions.into_iter().map(|session| session.id).collect())
    }

    fn resolve_session(&self, selector: &str) -> Result<SessionId, String> {
        let sessions = self.discover()?;
        resolve_session_selector(&sessions, selector).map_err(|error| error.to_string())
    }
}

/// Resolves startup `--resume` input without allowing a selector to fall back
/// to a freshly-created session.
fn resolve_startup_resume_selection(
    sessions_root: &Path,
    resume_selection: Option<Option<String>>,
) -> Result<Option<SessionId>, String> {
    let Some(selector) = resume_selection else {
        return Ok(None);
    };

    let sessions = discover_sessions(sessions_root).map_err(|error| error.to_string())?;
    match selector {
        Some(selector) => resolve_session_selector(&sessions, &selector)
            .map(Some)
            .map_err(|error| error.to_string()),
        None => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            prompt_for_session(&sessions, &mut stdin.lock(), &mut stdout.lock())
                .map_err(|error| error.to_string())?
                .map(Some)
                .ok_or_else(|| RESUME_SELECTION_CANCELLED.to_owned())
        }
    }
}

/// Baut das persistente Memory-Backend für die TUI-Chat-Session.
///
/// # Beschreibung
/// Öffnet einen [`harw_memory::FileMemoryStore`] unter
/// `<home>/profiles/<active-profile>/memories/` und gibt ihn als
/// `Arc<dyn harw_memory::Memory>` zurück. Schlägt die Profilauflösung oder das
/// Öffnen fehl (Rechte, ungültiger Pfad), fällt die Funktion auf `None` zurück
/// — die `/memory`-Op meldet dann `NotAvailable`, die restliche TUI läuft
/// weiter.
fn build_memory(home: &Path) -> Option<std::sync::Arc<dyn harw_memory::Memory>> {
    let root = match active_profile_memories_root(home) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(%error, "harw-memory: konnte Profilverzeichnis nicht auflösen");
            return None;
        }
    };

    match harw_memory::FileMemoryStore::open(&root) {
        Ok(store) => Some(std::sync::Arc::new(store)),
        Err(error) => {
            tracing::warn!(path = %root.display(), %error, "harw-memory: konnte Backend nicht öffnen");
            None
        }
    }
}

/// Baut die Sandbox-Authority für die interaktive TUI-Chat-Session.
///
/// # Beschreibung
/// Registriert genau einen Tenant/Workspace-Alias (`tui`/`project`) gegen den
/// kanonischen Root des von der Default-Registry entdeckten Projekts und erteilt
/// Lese-, Schreib- und Ausführungsrechte für diesen Workspace. Kein
/// Netzwerkzugriff — die 16 `harw-ops`-Kern-Operationen benötigen ihn für das
/// MVP nicht.
///
/// # Argumente
/// - `project_root` (`&Path`): kanonischer Root des Projekts, das die
///   Default-Registry und die Basis-Instruktionen beschreiben.
///
/// # Errors
/// Ein `String` mit menschenlesbarer Ursache, wenn die
/// Workspace-Registrierung/-Auflösung fehlschlägt (siehe
/// [`harw_sandbox::SandboxError`]).
fn build_tui_sandbox(project_root: &Path) -> Result<SandboxSpec, String> {
    let tenant = TenantId::from_str("tui");
    let workspace = WorkspaceId::from_str("project");
    let registry = WorkspaceRegistry::build(
        project_root,
        [WorkspaceRegistration {
            tenant: tenant.clone(),
            workspace: workspace.clone(),
            root: PathBuf::from("."),
        }],
    )
    .map_err(|error| error.to_string())?;
    let binding = registry
        .resolve(&tenant, &workspace)
        .map_err(|error| error.to_string())?;

    Ok(SandboxSpec::from_resolved(
        binding,
        PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ]),
    ))
}

/// Erzeugt einen frischen Wurzel-Trace für den einmaligen CLI-Lauf.
///
/// Diese Stelle ist eine Wurzel: ein einmaliger `harw chat <prompt>`-Lauf hat
/// kein Elternteil, dessen Trace er erben könnte — hier entsteht die
/// `trace_id`, die die gesamte Arbeit dieses Prozesses verbindet. Folgt
/// demselben Zufallsmuster wie `harw-core`s `new_span_id`
/// (`harw-core/src/child_controller.rs`) statt ein zweites zu erfinden:
/// `uuid::Uuid::new_v4` liefert die vollen 32 Hexzeichen für `trace_id`, ein
/// zweiter, unabhängiger Wurf liefert die ersten 16 für `span_id`. Beide sind
/// per Konstruktion bereits gültiges Kleinbuchstaben-Hex der richtigen Länge,
/// [`TraceContext::new`] validiert trotzdem statt Felder direkt zu setzen.
fn new_one_shot_root_trace() -> Result<TraceContext, String> {
    let trace_id = Uuid::new_v4().simple().to_string();
    let span_id = Uuid::new_v4().simple().to_string()[..16].to_owned();
    TraceContext::new(trace_id, span_id)
        .map_err(|error| format!("could not build one-shot root trace context: {error}"))
}

/// Baut den vertrauenswürdigen Tool-Kontext für einen einmaligen Coding-Turn.
///
/// Der Default-Registry stellt Datei- und Shell-Tools für das entdeckte
/// Projekt bereit. Die Sandbox muss daher an genau dessen kanonischen Root
/// gebunden sein; ohne diesen Kontext lehnt `harw-core` Tool-Aufrufe
/// fail-closed ab.
fn build_one_shot_spawn_context(project_root: &Path) -> Result<SpawnContext, String> {
    let tenant = TenantId::from_str("cli");
    let workspace = WorkspaceId::from_str("project");
    let registry = WorkspaceRegistry::build(
        project_root,
        [WorkspaceRegistration {
            tenant: tenant.clone(),
            workspace: workspace.clone(),
            root: PathBuf::from("."),
        }],
    )
    .map_err(|error| error.to_string())?;
    let binding = registry
        .resolve(&tenant, &workspace)
        .map_err(|error| error.to_string())?;
    let organizational_role = serde_json::from_str("\"root-orchestrator\"")
        .map_err(|error| format!("could not resolve one-shot organizational role: {error}"))?;
    let trace = new_one_shot_root_trace()?;

    Ok(SpawnContext {
        sandbox: SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
            ]),
        ),
        suggestions: None,
        capability_snapshot: None,
        // The default coding-tool policy pauses write and shell calls. Bind
        // that pause to the local CLI operator so a resolution cannot be
        // injected by an unrelated actor.
        approval_actor: Some(one_shot_approval_actor()),
        organizational_role,
        // Wurzel: kein Elternteil existiert, dessen Trace geerbt werden
        // könnte — siehe `new_one_shot_root_trace`.
        trace: Some(trace),
        // Wurzel: kein Elternteil existiert, dessen bereits geschnittene
        // Decke geerbt werden könnte, also entsteht sie hier, einmal — siehe
        // `crate::root_context::local_root_context_ceiling`.
        ceiling: Some(crate::root_context::local_root_context_ceiling()),
    })
}

/// Trusted, role-specific child registry builder for the one-shot path.
///
/// # Beschreibung
/// AP W5-06. Mirrors `harw-tui/src/app.rs`'s `TuiChildRegistryFactory`
/// pattern, but for the five built-in read-only roles from
/// [`harw_registry_defaults::profile::role_names`] instead of a
/// user-configured role set. It never reconstructs the parent's own
/// (write-capable) one-shot registry: every role is bound to the registry
/// profile [`harw_registry_defaults::profile::profile_for_role`] selects for
/// it, and its tool-activation IR comes from
/// [`harw_registry_defaults::embedded_agents::builtin_agent_definitions`].
///
/// A local override under `.harw/agents/` is **not** merged in here: that
/// loader keys its definitions by full `DefinitionId`
/// (`harw_config::ResolvedConfig::executable_agents`), while the built-in
/// merge rule in `builtin_agent_definitions` expects the bare role name
/// (`specialization`) as its key. Reconciling the two addressing schemes is
/// outside this AP's scope, so this factory always resolves the five roles
/// to their embedded definitions; see the worker report for this
/// simplification.
///
/// # Concurrency
/// `Send + Sync`: `discovery_cwd` is an owned `PathBuf`, `model` is an
/// `Arc<dyn ModelProvider>`, and `builtin_definitions` is populated once at
/// construction and never mutated afterward.
struct OneShotChildRegistryFactory {
    /// Startpunkt der Projekterkennung für jede Kind-Registry.
    discovery_cwd: PathBuf,
    /// Der Modellanbieter, den jedes Kind für seinen Turn wiederverwendet.
    model: Arc<dyn ModelProvider>,
    /// Die fünf eingebauten Rollen, bereits zu [`harw_agent_dsl::ExecutableAgentIr`]
    /// gesenkt, geschlüsselt nach ihrem Rollennamen.
    builtin_definitions: HashMap<String, harw_agent_dsl::ExecutableAgentIr>,
}

impl OneShotChildRegistryFactory {
    /// Lowert die eingebauten Rollen genau einmal und hält sie für die
    /// Lebensdauer des Spawners.
    ///
    /// # Arguments
    /// - `discovery_cwd` (`PathBuf`): Startpunkt der Projekterkennung, die an
    ///   jede Kind-Registry weitergereicht wird.
    /// - `model` (`Arc<dyn ModelProvider>`): der für jedes Kind wiederverwendete
    ///   Modellanbieter (derselbe wie der des One-shot-Root-Turns).
    ///
    /// # Returns
    /// `Ok(Self)` mit allen fünf eingebauten Rollen gesenkt.
    ///
    /// # Errors
    /// Ein `String`, wenn eine eingebettete Agentendefinition nicht senkt
    /// (praktisch unerreichbar: die TOML-Quellen sind zur Bauzeit eingebettet
    /// und werden von `harw-registry-defaults` selbst getestet).
    ///
    /// # Concurrency
    /// Reine Konstruktion; kein geteilter Zustand.
    fn new(discovery_cwd: PathBuf, model: Arc<dyn ModelProvider>) -> Result<Self, String> {
        let builtin_definitions =
            harw_registry_defaults::embedded_agents::builtin_agent_definitions(&HashMap::new())
                .map_err(|error| {
                    format!("could not lower builtin agent definitions for one-shot child spawning: {error}")
                })?;
        Ok(Self {
            discovery_cwd,
            model,
            builtin_definitions,
        })
    }
}

impl ChildRegistryFactory for OneShotChildRegistryFactory {
    /// Baut die Registry eines Kindes nach dem Profil seiner Rolle.
    ///
    /// # Arguments
    /// - `role` (`&str`): der registrierte Rollenname.
    /// - `_input` (`&harw_extension_api::SpawnInput`): ungenutzt — die
    ///   Registry hängt nur von `role` ab, nie von Modell-JSON.
    /// - `_suggestions` (`Option<&harw_catalog::AgentSuggestions>`): ungenutzt;
    ///   der Default-Vertrag dieses Traits lässt Vorschläge unberücksichtigt,
    ///   solange `build_registry_with_capabilities` nicht überschrieben wird.
    ///
    /// # Returns
    /// `Ok(ExtensionRegistry)` mit genau den Tool-Providern des Profils, das
    /// [`harw_registry_defaults::profile::profile_for_role`] für `role`
    /// liefert. Eine unbekannte Rolle fällt auf
    /// [`harw_registry_defaults::profile::RegistryProfile::default`] zurück
    /// (den vollen Coding-Satz, den auch gewöhnliche Kinder bekommen) statt zu
    /// scheitern.
    ///
    /// # Errors
    /// [`harw_extension_api::AgentSpawnError`], wenn die Projekterkennung unter
    /// `discovery_cwd` fehlschlägt.
    ///
    /// # Concurrency
    /// Zustandslos außer Lesezugriff auf `self`; sicher aus mehreren Threads
    /// aufrufbar.
    fn build_registry(
        &self,
        role: &str,
        _input: &harw_extension_api::SpawnInput,
        _suggestions: Option<&harw_catalog::AgentSuggestions>,
    ) -> Result<harw_extension_api::ExtensionRegistry, harw_extension_api::AgentSpawnError> {
        let profile = harw_registry_defaults::profile::profile_for_role(role).unwrap_or_default();
        let overrides = harw_registry_defaults::profile::IdentityOverrides {
            agent_name: Some(role.to_owned()),
            ..harw_registry_defaults::profile::IdentityOverrides::default()
        };
        let assembled = harw_registry_defaults::profile::assemble_registry(
            profile,
            self.discovery_cwd.clone(),
            overrides,
        )
        .map_err(|error| harw_extension_api::AgentSpawnError {
            message: format!(
                "could not assemble one-shot child registry for role '{role}': {error}"
            ),
        })?;
        Ok(assembled.registry)
    }

    /// Liefert den für den One-shot-Root-Turn gewählten Modellanbieter.
    ///
    /// # Returns
    /// `Ok(Arc<dyn ModelProvider>)` — immer derselbe geteilte Zeiger, nie ein
    /// neu aufgebauter Provider.
    ///
    /// # Errors
    /// Nie: der Modellanbieter ist bereits zur Konstruktionszeit aufgelöst.
    fn model_for(
        &self,
        _role: &str,
    ) -> Result<Arc<dyn ModelProvider>, harw_extension_api::AgentSpawnError> {
        Ok(Arc::clone(&self.model))
    }

    /// Liefert die eingebaute Agent-IR einer der fünf Rollen.
    ///
    /// # Returns
    /// `Some(&ExecutableAgentIr)` für jede Rolle aus
    /// [`harw_registry_defaults::profile::role_names::ALL`]; `None` für jede
    /// andere Rolle.
    fn executable_agent_ir(&self, role: &str) -> Option<&harw_agent_dsl::ExecutableAgentIr> {
        self.builtin_definitions.get(role)
    }
}

/// Builds the one-shot path's child-spawn authority for the five built-in
/// read-only roles.
///
/// # Beschreibung
/// AP W5-06. Mirrors `harw-tui/src/app.rs`'s `build_tui_managed_spawner`:
/// a dedicated [`SessionManager`] backs the spawner (child sessions never
/// mirror into the one-shot root's own manager, because the root itself has
/// none), and [`ManagedAgentSpawner::with_external_root_parent`] admits
/// `session_id` as the one trusted root — the same session the one-shot turn
/// runs under. Every role name comes from
/// [`harw_registry_defaults::profile::role_names::ALL`], never a literal, so
/// a rename there cannot silently desynchronize this registration from the
/// DSL TOMLs or the `agent_tool(child = "...")` declarations in `harw-ops`.
///
/// Every registered role is granted the organizational role
/// [`AgentRoleId::Worker`] — matching the `role = "worker"` every built-in
/// agent definition declares (see `harw-registry-defaults/agents/*.toml`) —
/// so [`ManagedAgentSpawner::admit`] enforces the same closed spawn matrix
/// the TUI relies on. `spawn_context.organizational_role` (root-orchestrator
/// for the one-shot root) must already permit spawning workers; this
/// function does not widen that check.
///
/// # Arguments
/// - `session_id` (`SessionId`): the one-shot root session's own ID. The
///   caller must construct the root `AgentSession` with this same ID
///   (`AgentSession::new_with_id`) so admission finds a matching parent.
/// - `spawn_context` (`SpawnContext`): the root's own trusted spawn context;
///   every admitted child's sandbox is checked against
///   `spawn_context.sandbox` and can never escalate past it.
/// - `model` (`Arc<dyn ModelProvider>`): the model every admitted child
///   reuses for its own turn.
/// - `discovery_cwd` (`PathBuf`): project-discovery root passed to every
///   child registry.
///
/// # Returns
/// `Ok(Arc<ManagedAgentSpawner>)` with all five built-in roles registered.
///
/// # Errors
/// A `String` when the built-in agent definitions cannot be lowered, or when
/// `with_external_root_parent` rejects the registration (unreachable in
/// practice: the manager backing this spawner is always freshly created).
///
/// # Concurrency
/// Creates its own dedicated child [`SessionManager`] and event channel —
/// distinct from the one-shot root session's own channel, since one-shot
/// mode already discards its root session events (no interactive renderer
/// consumes them).
fn build_one_shot_managed_spawner(
    session_id: SessionId,
    spawn_context: SpawnContext,
    model: Arc<dyn ModelProvider>,
    discovery_cwd: PathBuf,
) -> Result<Arc<ManagedAgentSpawner>, String> {
    let factory: Arc<dyn ChildRegistryFactory> =
        Arc::new(OneShotChildRegistryFactory::new(discovery_cwd, model)?);

    let (child_event_tx, _child_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(child_event_tx)));
    let mut spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative());
    for role in harw_registry_defaults::profile::role_names::ALL {
        spawner = spawner.with_role(
            (*role).to_owned(),
            AgentRole::Agent {
                name: (*role).to_owned(),
            },
            AgentRoleId::Worker,
            Arc::clone(&factory),
        );
    }

    spawner
        .with_external_root_parent(session_id, spawn_context, None)
        .map(Arc::new)
        .map_err(|error| {
            format!("could not register trusted one-shot root for child spawning: {error}")
        })
}

/// Builds the operation context for a model-initiated one-shot tool call.
///
/// Per-call authority comes exclusively from the harness-created execution
/// context. The operation registry is trusted composition-root data needed by
/// operation adapters such as `help`; one-shot mode deliberately supplies no
/// session controller or job store, because neither runtime boundary exists
/// for a single non-interactive turn.
///
/// # Arguments
/// - `execution_context` (`&harw_extension_api::ToolExecutionContext`): the
///   per-call session/turn/sandbox authority.
/// - `operations` (`&[Arc<dyn harw_operations::Operation>]`): the assembled
///   one-shot operation set.
/// - `managed_spawner` (`Option<&Arc<ManagedAgentSpawner>>`): AP W5-06.
///   `None` means [`build_one_shot_managed_spawner`] could not be built for
///   this invocation — `explore`/`research_*`/`analyze`/`/agent` then degrade
///   to `OpError::NotAvailable` instead of aborting the turn. `Some(spawner)`
///   registers it under its concrete type, matching
///   `harw_core_bridge::OpContextCoreExt`'s registration contract.
/// - `state_store` (`&Arc<dyn StateStore>`): AP W5-06. Registered under its
///   concrete type so `harw-core-bridge`'s `fanout_children` can durably run
///   admitted children; this is the exact store the root turn itself uses,
///   never a second instance.
/// - `plan_services` (`Option<&OneShotPlanServices>`): AP W5-06. `None`
///   leaves `plan`/`goal`/`explore`/`research_*`/`analyze` answering with
///   `OpError::NotAvailable`; `Some(services)` registers all four plan
///   services under one exclusive `ServiceMap` borrow via
///   [`harw_plan_bridge::register_plan_services`].
///
/// # Returns
/// The fully assembled `OpContext` for exactly one model tool call.
///
/// # Concurrency
/// Synchronous; every inserted service is an `Arc` clone (or a small `Clone`
/// value for `plan_config`), never a fresh allocation of the underlying
/// store.
fn one_shot_model_tool_context(
    execution_context: &harw_extension_api::ToolExecutionContext,
    operations: &[Arc<dyn harw_operations::Operation>],
    managed_spawner: Option<&Arc<ManagedAgentSpawner>>,
    state_store: &Arc<dyn StateStore>,
    plan_services: Option<&OneShotPlanServices>,
) -> OpContext {
    let mut operation_registry = OperationRegistry::new();
    for operation in operations {
        operation_registry.register(Arc::clone(operation));
    }

    let mut services = ServiceMap::new();
    services.insert(operation_registry);
    if let Some(managed_spawner) = managed_spawner {
        services.insert(Arc::clone(managed_spawner));
    }
    services.insert(Arc::clone(state_store));
    if let Some(plan_services) = plan_services {
        harw_plan_bridge::register_plan_services(
            &mut services,
            Arc::clone(&plan_services.plan_store),
            Arc::clone(&plan_services.goal_store),
            Arc::clone(&plan_services.finding_store),
            plan_services.plan_config.clone(),
        );
    }

    OpContext::new(
        execution_context.session_id().clone(),
        execution_context.turn_id().clone(),
        execution_context.sandbox().clone(),
        services,
    )
}

/// Adds the operation-backed model-tool surface to the assembled one-shot
/// registry without replacing its default approval handlers.
///
/// # Arguments
/// See [`one_shot_model_tool_context`] — every optional argument here is
/// forwarded unchanged into every context built for this provider.
///
/// # Concurrency
/// Captured services are cloned once per call into the returned closure;
/// each invocation of that closure only clones `Arc` pointers.
fn add_one_shot_model_tool_provider(
    extension_registry: &mut harw_extension_api::ExtensionRegistry,
    operations: &[Arc<dyn harw_operations::Operation>],
    managed_spawner: Option<&Arc<ManagedAgentSpawner>>,
    state_store: &Arc<dyn StateStore>,
    plan_services: Option<&OneShotPlanServices>,
) {
    let provider_operations = operations.to_vec();
    let context_operations = operations.to_vec();
    let managed_spawner = managed_spawner.cloned();
    let state_store = Arc::clone(state_store);
    let plan_services = plan_services.cloned();
    let provider = ModelToolProvider::new(provider_operations, move |execution_context| {
        one_shot_model_tool_context(
            execution_context,
            &context_operations,
            managed_spawner.as_ref(),
            &state_store,
            plan_services.as_ref(),
        )
    });
    extension_registry.add_tool_provider(Arc::new(provider));
}

/// Ordnet jede lokale CLI-Session stabil ihrem Transcript-Thread zu.
///
/// Der Präfix trennt die lokale CLI-Provenienz von anderen Ingressen;
/// der unveränderte `SessionId`-Anteil macht die Zuordnung nach einem Prozess-
/// Neustart reproduzierbar.
fn cli_thread_for_session(session_id: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("cli-session:{}", session_id.as_str()))
}

/// Baut den langlebigen Zustandsspeicher der aktiven CLI-Profil-Session.
///
/// Die Transcript-Dateien gehören zum aktiven Profil und nicht zum Projekt oder
/// globalen Home, damit Profilwechsel keine Chat-Verläufe vermischen. Dieser
/// Adapter ist der Standard für interaktive und einmalige CLI-Turns; ein
/// flüchtiger Ersatz ist bewusst nicht vorgesehen, damit Fehler beim
/// Profilpfad nicht stillschweigend Session-Zustand verlieren.
fn build_cli_state_store(home: &Path) -> Result<TranscriptStateStore, String> {
    let sessions_root = active_profile_sessions_root(home)?;

    Ok(TranscriptStateStore::new(
        TranscriptStore::new(&sessions_root),
        cli_thread_for_session,
    ))
}

/// Resolves the transcript directory owned by the active CLI profile.
///
/// Both one-shot persistence and interactive session recovery use this one
/// root, so a profile switch cannot cross session boundaries.
fn active_profile_sessions_root(home: &Path) -> Result<PathBuf, String> {
    let profile_name = harw_home::active_profile_name(home);
    let profile = harw_home::profile_dir(home, &profile_name).map_err(|error| {
        format!("could not resolve active profile storage for one-shot chat: {error}")
    })?;
    Ok(profile.join("sessions"))
}

/// Resolves the canonical memory directory owned by the active CLI profile.
///
/// Keeping this alongside profile-scoped transcripts prevents a profile switch
/// from exposing one profile's durable memories to another.
fn active_profile_memories_root(home: &Path) -> Result<PathBuf, String> {
    let profile_name = harw_home::active_profile_name(home);
    let profile = harw_home::profile_dir(home, &profile_name).map_err(|error| {
        format!("could not resolve active profile storage for chat memory: {error}")
    })?;
    Ok(profile.join("memories"))
}

/// Resolves the parent directory for the active profile's durable jobs.
///
/// [`harw_session_store::JobStore`] appends its own `jobs` component, so this
/// helper intentionally returns `<home>/profiles/<active-profile>` rather than
/// a path ending in `jobs`.
fn active_profile_job_store_root(home: &Path) -> Result<PathBuf, String> {
    let profile_name = harw_home::active_profile_name(home);
    harw_home::profile_dir(home, &profile_name)
        .map_err(|error| format!("could not resolve active profile storage for chat jobs: {error}"))
}

/// Baut den Chat-Provider aus der Config; fällt auf den Echo-Bootstrap zurück,
/// wenn kein echter Provider aufgelöst werden kann.
///
/// Wenn die Konfiguration `secrets:`-Referenzen verwendet, wird der
/// konfigurierte Resolver aus dem aktiven Home geöffnet und injiziert. Ohne
/// konfigurierten Resolver bleibt der bisherige direkte Provider-Aufbau
/// erhalten.
///
/// Nutzt [`harw_provider_http::build_provider_with_home`], das anhand des
/// `api`-Felds des Default-Providers zwischen dem nativen
/// Anthropic-Messages-Transport (`anthropic-messages`, inkl.
/// Azure-Foundry-Gateway) und dem OpenAI-kompatiblen Transport wählt und
/// `file:`/`file-json:`-Referenzen unterhalb von `<home>/secrets/` auflöst.
fn build_model(home: &Path, config: &ResolvedConfig) -> Result<Box<dyn ModelProvider>, String> {
    let resolver = crate::secret_store::open_configured_secret_resolver(home, config)?;
    let result = match resolver {
        Some(resolver) => harw_provider_http::build_provider_with_home(
            config,
            home,
            Some(&resolver as &dyn harw_provider_http::SecretResolver),
        ),
        None => harw_provider_http::build_provider_with_home(config, home, None),
    };
    result.map_err(|error| {
        format!("Provider-Einrichtung unvollständig: {error}. Prüfe harw onboard.")
    })
}

/// Führt genau einen Turn mit dem gewählten Modell aus und druckt die Antwort.
///
/// # Arguments
/// - `plan_services` (`Option<OneShotPlanServices>`): AP W5-06. Weitergereicht
///   von [`run_chat`]; siehe dort und [`one_shot_model_tool_context`].
fn one_shot(
    model: Box<dyn ModelProvider>,
    home: &Path,
    prompt: &str,
    executable_agent: Option<&harw_agent_dsl::ExecutableAgentIr>,
    plan_services: Option<OneShotPlanServices>,
) -> Result<(), String> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start local runtime: {error}"))?;

    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let assembled = harw_registry_defaults::assemble_default_registry(cwd.clone())
        .map_err(|error| error.to_string())?;
    let spawn_context = build_one_shot_spawn_context(&assembled.project.project_root)?;
    let mut operations = OperationRegistry::new();
    harw_ops::register_all(&mut operations);
    // Wie im interaktiven Zweig: ohne diesen Aufruf lägen die Plan-Dienste zwar
    // in der ServiceMap jedes Tool-Aufrufs, aber keine Operation könnte sie
    // erreichen. `register_plan_tools` ist ein No-op, solange
    // `[tools.plan] enabled = false`.
    if let Some(services) = plan_services.as_ref() {
        let registered = harw_ops::register_plan_tools(&mut operations, &services.plan_config);
        tracing::info!(registered, "one_shot.plan_tools.registered");
    }
    let operations: Vec<Arc<dyn harw_operations::Operation>> =
        operations.iter().map(Arc::clone).collect();
    let mut registry = assembled.registry;

    // AP W5-06: der One-shot-Root bekommt eine feste `SessionId`, damit sie
    // sowohl der Kind-Spawner (als vertrauter externer Root-Parent) als auch
    // die gleich darauf gebaute `AgentSession` (`new_with_id`) übereinstimmend
    // kennen — ohne diese Vorab-Vergabe entstünde ein Henne-Ei-Problem.
    let session_id = SessionId::new();
    let model: Arc<dyn ModelProvider> = Arc::from(model);
    let managed_spawner = match build_one_shot_managed_spawner(
        session_id.clone(),
        spawn_context.clone(),
        Arc::clone(&model),
        cwd,
    ) {
        Ok(spawner) => Some(spawner),
        Err(error) => {
            // Kein Kind-Spawner ist keine gescheiterte Ein-Turn-Ausführung —
            // `explore`/`research_*`/`analyze` degradieren dann auf
            // `OpError::NotAvailable`, wie es die TUI bei einem leeren
            // konfigurierten Kind-Rollen-Satz ebenfalls tut.
            tracing::warn!(%error, "one_shot.child_spawner_unavailable");
            None
        }
    };
    let state_store: Arc<dyn StateStore> = Arc::new(build_cli_state_store(home)?);

    add_one_shot_model_tool_provider(
        &mut registry,
        &operations,
        managed_spawner.as_ref(),
        &state_store,
        plan_services.as_ref(),
    );

    runtime.block_on(async {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session =
            AgentSession::new_with_id(session_id, AgentRole::Assistant, None, registry, event_tx)
                .with_spawn_context(spawn_context);
        if let Some(executable_agent) = executable_agent {
            session = session.with_executable_agent_ir(executable_agent);
        }

        match run_turn(
            &mut session,
            model.as_ref(),
            state_store.as_ref(),
            TurnInput::user(prompt),
        )
        .await
        .map_err(|error| error.to_string())?
        {
            TurnOutcome::Completed => {}
            TurnOutcome::AwaitingChild { .. } => {
                return Err("provider unexpectedly paused a turn".to_owned());
            }
            TurnOutcome::AwaitingApproval { .. } => {
                // One-shot invocations have no approval UI. Resolve the
                // captured request as a rejection rather than approving it
                // implicitly; `resume_after_approval` also verifies the
                // actor against the pending request before continuing.
                let _ = resume_after_approval(
                    &mut session,
                    model.as_ref(),
                    state_store.as_ref(),
                    one_shot_approval_actor(),
                    one_shot_approval_rejection(),
                )
                .await
                .map_err(|error| error.to_string())?;
                return Err(ONE_SHOT_APPROVAL_DENIAL.to_owned());
            }
        }

        let reply = session
            .history()
            .to_model_messages()
            .into_iter()
            .rev()
            .find_map(|message| match message {
                ModelMessage::Assistant { text } => Some(text),
                _ => None,
            })
            .ok_or_else(|| "provider produced no assistant response".to_owned())?;
        println!("{reply}");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_agent_dsl::{
        ExecutableAgentIr, ids::DefinitionId, layers::DefinitionLayer, lower, parse::parse_toml,
        resolve::resolve_definition,
    };
    use harw_core::EchoModelProvider;
    use harw_extension_api::{ApprovalDecision, ToolCall, ToolName};
    use harw_types::ToolCallId;

    fn test_executable_agent() -> (String, ExecutableAgentIr) {
        const DEFINITION_ID: &str = "harwness.agent.chat-test@1";
        const DEFINITION_TOML: &str = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.chat-test@1"
version = "1.0.0"
role = "worker"
specialization = "chat-test"
"#;

        let raw = parse_toml(DEFINITION_TOML).expect("parse test agent definition");
        let id = DefinitionId::parse(DEFINITION_ID).expect("parse test definition ID");
        let resolved = resolve_definition(
            &id,
            &[(DefinitionLayer::BuiltIn, raw)],
            std::time::SystemTime::now().into(),
        )
        .expect("resolve test agent definition");
        let executable = lower(&resolved).expect("lower test agent definition");
        (DEFINITION_ID.to_owned(), executable)
    }

    #[test]
    fn selected_executable_agent_is_none_without_active_selection() {
        let config = ResolvedConfig::default();

        assert!(
            selected_executable_agent(&config)
                .expect("missing selection is valid")
                .is_none()
        );
    }

    #[test]
    fn selected_executable_agent_returns_the_configured_ir_snapshot() {
        let (definition_id, executable) = test_executable_agent();
        let expected_snapshot = executable.snapshot_id().to_string();
        let mut config = ResolvedConfig::default();
        config
            .executable_agents
            .insert(definition_id.clone(), executable);
        config.harness.active_agent_definition = Some(definition_id.clone());

        let selected = selected_executable_agent(&config)
            .expect("configured selection should resolve")
            .expect("active selection should return an IR");

        assert_eq!(selected.snapshot_id().to_string(), expected_snapshot);
        assert!(std::ptr::eq(
            selected,
            config
                .executable_agents
                .get(&definition_id)
                .expect("configured executable agent")
        ));
    }

    #[test]
    fn selected_executable_agent_errors_for_a_missing_configured_id() {
        let mut config = ResolvedConfig::default();
        config.harness.active_agent_definition = Some("harwness.agent.missing@1".to_owned());

        let error = selected_executable_agent(&config)
            .expect_err("missing configured definition must fail closed");

        assert!(error.contains("harwness.agent.missing@1"));
        assert!(error.contains("executable agents"));
    }

    fn write_transcript(sessions_root: &Path, session_id: &str) {
        std::fs::create_dir_all(sessions_root).expect("create sessions directory");
        std::fs::write(sessions_root.join(format!("{session_id}.jsonl")), "{}\n")
            .expect("write transcript");
    }

    #[test]
    fn tui_sandbox_binds_interactive_authority_to_the_discovered_project_root() {
        let project = tempfile::tempdir().expect("create project directory");
        let sandbox = build_tui_sandbox(project.path()).expect("build TUI sandbox");
        let expected_root = project
            .path()
            .canonicalize()
            .expect("canonical project root");

        assert_eq!(
            sandbox.workspace().canonical_root(),
            expected_root.as_path()
        );
        assert_eq!(sandbox.workspace().tenant().as_str(), "tui");
        assert_eq!(sandbox.workspace().workspace().as_str(), "project");
        assert!(sandbox.permissions().contains(Permission::ReadWorkspace));
        assert!(sandbox.permissions().contains(Permission::WriteWorkspace));
        assert!(sandbox.permissions().contains(Permission::ExecuteProcess));
        assert!(!sandbox.permissions().contains(Permission::NetworkAccess));
    }

    #[test]
    fn one_shot_context_binds_default_tools_to_the_project_root() {
        let project = tempfile::tempdir().expect("create project directory");
        let context = build_one_shot_spawn_context(project.path()).expect("build spawn context");
        let expected_root = project
            .path()
            .canonicalize()
            .expect("canonical project root");

        assert_eq!(
            context.sandbox.workspace().canonical_root(),
            expected_root.as_path()
        );
        assert_eq!(context.sandbox.workspace().tenant().as_str(), "cli");
        assert_eq!(context.sandbox.workspace().workspace().as_str(), "project");
        assert!(
            context
                .sandbox
                .permissions()
                .contains(Permission::ReadWorkspace)
        );
        assert!(
            context
                .sandbox
                .permissions()
                .contains(Permission::WriteWorkspace)
        );
        assert!(
            context
                .sandbox
                .permissions()
                .contains(Permission::ExecuteProcess)
        );
        assert!(
            !context
                .sandbox
                .permissions()
                .contains(Permission::NetworkAccess)
        );
        assert_eq!(context.approval_actor, Some(one_shot_approval_actor()));
    }

    /// AW1-01c: the one-shot spawn context is a root — it carries a
    /// freshly-generated trace with the right hex shapes and no parent span.
    #[test]
    fn one_shot_context_carries_a_freshly_generated_root_trace() {
        let project = tempfile::tempdir().expect("create project directory");
        let context = build_one_shot_spawn_context(project.path()).expect("build spawn context");

        let trace = context.trace.expect("one-shot root must carry a trace");
        assert_eq!(trace.trace_id.len(), 32);
        assert!(trace.trace_id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(trace.trace_id, trace.trace_id.to_lowercase());
        assert_eq!(trace.span_id.len(), 16);
        assert!(trace.span_id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(trace.span_id, trace.span_id.to_lowercase());
        assert!(
            trace.parent_span_id.is_none(),
            "a root trace must not carry a parent span"
        );
    }

    /// AW1-01c: two one-shot runs must not look like the same run — the
    /// random source must not be broken/constant.
    #[test]
    fn one_shot_context_root_traces_differ_across_two_calls() {
        let project = tempfile::tempdir().expect("create project directory");
        let first = build_one_shot_spawn_context(project.path())
            .expect("build first spawn context")
            .trace
            .expect("first call must carry a trace");
        let second = build_one_shot_spawn_context(project.path())
            .expect("build second spawn context")
            .trace
            .expect("second call must carry a trace");

        assert_ne!(first.trace_id, second.trace_id);
    }

    #[test]
    fn one_shot_approval_denial_is_actionable_and_fail_closed() {
        assert_eq!(
            one_shot_approval_rejection(),
            ApprovalResolution::Reject {
                reason: ONE_SHOT_APPROVAL_DENIAL.to_owned(),
            }
        );
        assert!(ONE_SHOT_APPROVAL_DENIAL.contains("harw chat"));
    }

    #[test]
    fn one_shot_registry_advertises_operation_model_tools_and_keeps_approval_classification() {
        let cwd = std::env::current_dir().expect("resolve test working directory");
        let assembled = harw_registry_defaults::assemble_default_registry(cwd)
            .expect("assemble default one-shot registry");
        let initial_approval_handler_count = assembled.registry.approval_handlers().len();
        let mut registry = assembled.registry;
        let mut operation_registry = OperationRegistry::new();
        harw_ops::register_all(&mut operation_registry);
        let operations: Vec<Arc<dyn harw_operations::Operation>> =
            operation_registry.iter().map(Arc::clone).collect();
        let state_store: Arc<dyn StateStore> = Arc::new(harw_core::InMemoryStateStore::new());

        add_one_shot_model_tool_provider(&mut registry, &operations, None, &state_store, None);

        let advertised_tools: Vec<String> = registry
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|tool| tool.name().to_owned())
            .collect();
        for operation_name in ["status", "ps", "diff", "stop"] {
            assert!(
                advertised_tools.iter().any(|name| name == operation_name),
                "one-shot registry must advertise operation model tool `{operation_name}`"
            );
        }
        assert_eq!(
            registry.approval_handlers().len(),
            initial_approval_handler_count,
            "adding operation tools must preserve the assembled fail-closed approval policy"
        );

        let tool_call = |name: &str| ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(name),
            arguments: serde_json::Value::Null,
        };
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build approval test runtime");
        runtime.block_on(async {
            let handler = registry
                .approval_handlers()
                .first()
                .expect("assembled registry must retain its default approval handler");
            assert!(matches!(
                handler.review(&tool_call("status")).await,
                ApprovalDecision::Allow
            ));
            assert!(matches!(
                handler.review(&tool_call("stop")).await,
                ApprovalDecision::AskUser(_)
            ));
        });
    }

    #[test]
    fn one_shot_model_tool_context_uses_execution_authority_and_only_operation_registry() {
        let project = tempfile::tempdir().expect("create project directory");
        let spawn_context =
            build_one_shot_spawn_context(project.path()).expect("build one-shot spawn context");
        let mut operation_registry = OperationRegistry::new();
        harw_ops::register_all(&mut operation_registry);
        let operations: Vec<Arc<dyn harw_operations::Operation>> =
            operation_registry.iter().map(Arc::clone).collect();
        let execution_context = harw_extension_api::ToolExecutionContext::new(
            SessionId::new(),
            harw_types::TurnId::new(),
            spawn_context.sandbox,
        );
        let state_store: Arc<dyn StateStore> = Arc::new(harw_core::InMemoryStateStore::new());

        let context =
            one_shot_model_tool_context(&execution_context, &operations, None, &state_store, None);

        assert_eq!(context.session_id(), execution_context.session_id());
        assert_eq!(context.turn_id(), execution_context.turn_id());
        assert_eq!(context.sandbox(), execution_context.sandbox());
        assert_eq!(
            context
                .service::<OperationRegistry>()
                .expect("operation registry service")
                .len(),
            operations.len()
        );
        assert!(
            context
                .service::<Arc<harw_session_store::JobStore>>()
                .is_none()
        );
        assert!(
            context
                .service::<harw_operations::SharedSessionController>()
                .is_none()
        );
        assert!(
            context.service::<Arc<ManagedAgentSpawner>>().is_none(),
            "no managed spawner was passed in, so none must be discoverable"
        );
        assert!(
            context.service::<Arc<dyn StateStore>>().is_some(),
            "the one-shot state store must always be discoverable for fan-out children"
        );
        assert!(
            context.service::<harw_plan::PlanToolConfig>().is_none(),
            "no plan services were passed in, so none must be discoverable"
        );
    }

    #[test]
    fn active_profile_sessions_root_uses_the_active_profile_sessions_directory() {
        let home = tempfile::tempdir().expect("create home directory");
        harw_home::ensure_home(home.path()).expect("scaffold home");

        let sessions_root =
            active_profile_sessions_root(home.path()).expect("resolve sessions root");
        let profile_name = harw_home::active_profile_name(home.path());
        let expected = home
            .path()
            .join("profiles")
            .join(profile_name)
            .join("sessions");

        assert_eq!(sessions_root, expected);
    }

    #[test]
    fn active_profile_memories_root_uses_the_active_profile_memories_directory() {
        let home = tempfile::tempdir().expect("create home directory");
        harw_home::ensure_home(home.path()).expect("scaffold home");
        std::fs::write(harw_home::active_profile_path(home.path()), "analysis\n")
            .expect("select analysis profile");

        let memories_root =
            active_profile_memories_root(home.path()).expect("resolve memories root");
        let expected = home
            .path()
            .join("profiles")
            .join("analysis")
            .join("memories");

        assert_eq!(memories_root, expected);
    }

    #[test]
    fn chat_job_store_uses_the_active_profile_jobs_directory() {
        let home = tempfile::tempdir().expect("create home directory");
        harw_home::ensure_home(home.path()).expect("scaffold home");
        std::fs::write(harw_home::active_profile_path(home.path()), "analysis\n")
            .expect("select analysis profile");

        let store_root = active_profile_job_store_root(home.path()).expect("resolve job root");
        let store = harw_session_store::JobStore::new(&store_root);

        assert_eq!(
            store.root(),
            home.path().join("profiles").join("analysis").join("jobs")
        );
    }

    #[test]
    fn prompt_and_resume_are_mutually_exclusive() {
        let error = validate_chat_mode(Some("continue this"), Some(&None))
            .expect_err("prompt plus interactive resume must be rejected");

        assert_eq!(error, PROMPT_RESUME_CONFLICT);
    }

    #[test]
    fn one_shot_and_interactive_resume_modes_are_individually_allowed() {
        assert!(validate_chat_mode(Some("one shot"), None).is_ok());
        assert!(validate_chat_mode(None, Some(&None)).is_ok());
        assert!(validate_chat_mode(None, Some(&Some("session-42".to_owned()))).is_ok());
    }

    #[test]
    fn explicit_startup_resume_uses_the_discovered_session_id() {
        let sessions = tempfile::tempdir().expect("create sessions directory");
        write_transcript(sessions.path(), "session-42");

        let selected =
            resolve_startup_resume_selection(sessions.path(), Some(Some("session-42".to_owned())))
                .expect("resolve explicit session");

        assert_eq!(selected, Some(SessionId::from_str("session-42")));
    }

    #[test]
    fn unknown_startup_resume_selector_fails_closed() {
        let sessions = tempfile::tempdir().expect("create sessions directory");
        write_transcript(sessions.path(), "session-42");

        let error =
            resolve_startup_resume_selection(sessions.path(), Some(Some("missing".to_owned())))
                .expect_err("unknown selector must not create a new session");

        assert!(error.contains("unbekannte Session-Auswahl"));
    }

    #[test]
    fn no_resume_selection_preserves_new_session_startup() {
        let sessions = tempfile::tempdir().expect("create sessions directory");

        assert_eq!(
            resolve_startup_resume_selection(sessions.path(), None)
                .expect("no resume request is valid"),
            None
        );
    }

    #[test]
    fn cli_thread_mapping_is_deterministic_and_scoped_to_cli() {
        let session = SessionId::from_str("session-123");

        assert_eq!(
            cli_thread_for_session(&session),
            ThreadRef::from_str("cli-session:session-123")
        );
        assert_eq!(
            cli_thread_for_session(&session),
            cli_thread_for_session(&session)
        );
    }

    #[test]
    fn cli_state_store_persists_turns_in_the_active_profile_sessions_root() {
        let home = tempfile::tempdir().expect("create home directory");
        harw_home::ensure_home(home.path()).expect("scaffold home");
        let sessions_root =
            active_profile_sessions_root(home.path()).expect("resolve sessions root");
        let store = build_cli_state_store(home.path()).expect("build durable CLI state store");
        let session = SessionId::from_str("session-123");
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("persist this turn");
        let item = history.items().first().expect("history has a turn item");
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build test runtime");

        runtime
            .block_on(harw_core::StateStore::save_turn(&store, &session, item))
            .expect("persist turn through transcript adapter");

        let records = TranscriptStore::new(&sessions_root)
            .reader(&session)
            .expect("open CLI transcript")
            .collect::<harw_session_store::SessionStoreResult<Vec<_>>>()
            .expect("read CLI transcript");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].thread, cli_thread_for_session(&session));
    }

    // ── AP W5-06: one-shot child spawning and plan services ────────────────

    /// Collects every tool name a registry actually registers, across all
    /// its providers, in provider order.
    fn registered_tool_names(registry: &harw_extension_api::ExtensionRegistry) -> Vec<String> {
        registry
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|spec| spec.name().to_owned())
            .collect()
    }

    fn test_spawn_input() -> harw_extension_api::SpawnInput {
        harw_extension_api::SpawnInput {
            parent_session_id: SessionId::new(),
            handoff_call_id: ToolCallId::new(),
            instructions: None,
            context: serde_json::Value::Null,
            // This fixture requests no ceiling of its own: the child simply
            // inherits whatever ceiling its parent already enforces.
            ceiling: None,
        }
    }

    #[test]
    fn one_shot_child_registry_factory_denies_write_and_shell_for_the_explorer_role() {
        let cwd = std::env::current_dir().expect("resolve test working directory");
        let factory = OneShotChildRegistryFactory::new(
            cwd,
            Arc::new(EchoModelProvider::new("explorer reply")),
        )
        .expect("build one-shot child registry factory");

        let registry = factory
            .build_registry(
                harw_registry_defaults::profile::role_names::EXPLORER,
                &test_spawn_input(),
                None,
            )
            .expect("explorer registry must assemble");
        let names = registered_tool_names(&registry);

        assert!(
            names.contains(&"fs.read".to_owned()),
            "explorer must still see read-only tools: {names:?}"
        );
        assert!(
            !names.contains(&"fs.write".to_owned()),
            "explorer must never see fs.write: {names:?}"
        );
        assert!(
            !names.contains(&"shell.exec".to_owned()),
            "explorer must never see shell.exec: {names:?}"
        );
    }

    #[test]
    fn one_shot_child_registry_factory_falls_back_to_the_default_profile_for_an_unknown_role() {
        let cwd = std::env::current_dir().expect("resolve test working directory");
        let factory =
            OneShotChildRegistryFactory::new(cwd, Arc::new(EchoModelProvider::new("reply")))
                .expect("build one-shot child registry factory");

        let registry = factory
            .build_registry("not-a-builtin-role", &test_spawn_input(), None)
            .expect("an unknown role must fall back to the default profile, not fail");
        let names = registered_tool_names(&registry);

        assert_eq!(
            names,
            harw_registry_defaults::profile::RegistryProfile::default().registered_tool_names(),
            "an unknown role must get exactly the profile ordinary children use"
        );
        assert!(names.contains(&"fs.write".to_owned()));
        assert!(names.contains(&"shell.exec".to_owned()));
    }

    #[test]
    fn one_shot_child_registry_factory_executable_agent_ir_covers_every_builtin_role() {
        let cwd = std::env::current_dir().expect("resolve test working directory");
        let factory =
            OneShotChildRegistryFactory::new(cwd, Arc::new(EchoModelProvider::new("reply")))
                .expect("build one-shot child registry factory");

        for role in harw_registry_defaults::profile::role_names::ALL {
            assert!(
                factory.executable_agent_ir(role).is_some(),
                "builtin role '{role}' must have a lowered agent IR"
            );
        }
        assert!(
            factory.executable_agent_ir("not-a-builtin-role").is_none(),
            "an unknown role must not fabricate an agent IR"
        );
    }

    /// Was `spawn.max_depth` bedeutet, und welche Frage `role_names::ALL`
    /// beantwortet.
    ///
    /// Belegstelle für die Bedeutung: `harw-agent-dsl/src/executable.rs`,
    /// `SpawnContract` — „`max_depth` begrenzt, wie viele Ebenen an
    /// Kindagenten unterhalb dieses Agenten noch entstehen dürfen“ — und
    /// `SpawnContract::max_depth` — „`Some(0)` = dieser Agent darf keine
    /// Kinder erzeugen“. Die eingebauten Definitionen lesen den Wert genauso:
    /// `agents/analyst.toml` begründet `max_depth = 2` mit „der Analyst darf
    /// read-only Kinder starten und deren Rückgaben zusammenführen“, die vier
    /// `security-*-triage`-Rollen begründen `max_depth = 0` mit „ein reiner
    /// Sichtungs-/Vorschlags-Job braucht keine Kind-Agenten“. Der Wert
    /// beschreibt also die Tiefe **unterhalb** der Rolle, nicht die Tiefe, auf
    /// der die Rolle selbst sitzen darf.
    ///
    /// `role_names::ALL` beantwortet genau **eine** der beiden Fragen, die
    /// bisher an dieser Liste hingen: *welche Rollen gibt es*. Nur was dort
    /// steht, senkt `builtin_agent_definitions` zu einer startbaren Rolle, und
    /// nur das erreicht der Verbotstest für `fs.write`/`shell.exec` — genau
    /// deshalb stehen die vier Triage-Rollen darin, und deshalb bleiben sie
    /// darin. Die zweite Frage — *welche Rolle wird als Worker-Kind
    /// zugelassen* — liest dieser Test nicht mehr aus derselben Liste ab und
    /// auch nicht aus einer zweiten handgepflegten Liste (das wäre derselbe
    /// Fehler noch einmal), sondern aus der Definition der Rolle selbst:
    /// `executable_agent_ir(role).spawn_contract().max_depth()`.
    ///
    /// Behoben in `harw-core/src/child_controller.rs::admit`: die Admission
    /// vergleicht `depth` nicht mehr gegen `max_depth` der **gespawnten**
    /// Rolle, sondern gegen die geerbte `depth_ceiling` des **Elternteils**
    /// (`ChildRecord::depth_ceiling`; ein Kind erbt die Decke seines Elternteils
    /// und verschärft sie für seine eigenen Kinder um seinen eigenen
    /// `max_depth`-Wert). `max_depth = 0` heißt damit korrekt „diese Rolle
    /// erzeugt selbst keine Kinder“ und nicht mehr „diese Rolle darf kein Kind
    /// sein“ — eine Rolle mit `max_depth = 0` ist also als Kind zulässig, kann
    /// aber selbst keine Enkel erzeugen. Root-Spawns wie in diesem Test (ohne
    /// eigene `ceiling`) erben die Decke des Wurzelkontrollers und werden
    /// dadurch unabhängig vom eigenen `max_depth` jeder Rolle admittiert; die
    /// frühere Fallunterscheidung über `declared_depth_below` ist damit
    /// entbehrlich geworden und wurde entfernt. Die verbleibende, jetzt
    /// schärfere Aussage: **jede** eingebaute Rolle wird als Worker-Kind
    /// zugelassen — das war vor den beiden vorangegangenen Korrekturen keine
    /// Trivialität (erst scheiterte `explorer` an der Wurzeldecke, dann
    /// `security-egress-triage` an der falsch gelesenen Tiefenprüfung). Die
    /// zweite Hälfte der Bedeutung — dass eine Rolle mit `max_depth = 0` zwar
    /// als Kind läuft, aber keinen Enkel erzeugen kann — prüft dieser Test
    /// nicht: von `harw-cli` aus ist über `AgentSpawner` kein zweiter
    /// Spawn-Schritt aus dem Kind heraus erreichbar. Der zugehörige Beleg
    /// liegt in `harw-core/src/child_controller.rs`,
    /// `a_role_that_forbids_grandchildren_is_still_admissible_as_a_child`.
    #[test]
    fn build_one_shot_managed_spawner_admits_the_roles_their_spawn_contract_allows() {
        // Dieser Test prüft eine Eigenschaft JE ROLLE — „jede eingebaute
        // Rolle ist einzeln als Worker-Kind zulässig” —, nicht die ganz
        // andere Aussage „alle Rollen dürfen gleichzeitig als Kinder desselben
        // Elternteils laufen”. Die zweite Aussage würde das aktive
        // Kind-Limit prüfen, das eine echte, unabhängige Grenze ist und mit
        // dem Gegenstand dieses Tests nichts zu tun hat. Deshalb bekommt
        // jede Rolle ihren eigenen frischen Spawner (samt eigener
        // Session-Id): so bleibt die Prüfung pro Rolle vollständig
        // unabhängig von der Anzahl und Reihenfolge der übrigen Rollen, und
        // das Wachsen von `role_names::ALL` stößt nie wieder an eine
        // Obergrenze, die für den geprüften Sachverhalt irrelevant ist.
        let project = tempfile::tempdir().expect("create project directory");
        let spawn_context =
            build_one_shot_spawn_context(project.path()).expect("build one-shot spawn context");
        let model: Arc<dyn ModelProvider> = Arc::new(EchoModelProvider::new("child reply"));

        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build test runtime");
        for role in harw_registry_defaults::profile::role_names::ALL {
            let session_id = SessionId::new();
            let spawner = build_one_shot_managed_spawner(
                session_id.clone(),
                spawn_context.clone(),
                Arc::clone(&model),
                project.path().to_path_buf(),
            )
            .expect("build one-shot managed spawner");
            let spawn_input = harw_extension_api::SpawnInput {
                parent_session_id: session_id.clone(),
                handoff_call_id: ToolCallId::new(),
                instructions: None,
                context: serde_json::Value::Null,
                // No test requests a ceiling of its own here: each child
                // must be admitted purely by inheriting the root's ceiling.
                ceiling: None,
            };
            let child = runtime.block_on(harw_extension_api::AgentSpawner::spawn_child(
                spawner.as_ref(),
                role,
                spawn_input,
                spawn_context.sandbox.clone(),
                None,
            ));
            // `max_depth` beschreibt, wie viele Ebenen unterhalb der Rolle
            // noch entstehen dürfen — `Some(0)` heißt „diese Rolle erzeugt
            // selbst keine Kinder”, nicht „diese Rolle darf kein Kind sein”.
            // Als Wurzel-Spawn ohne eigene Decke erbt jedes Kind die Decke
            // des Wurzelkontrollers, unabhängig vom eigenen `max_depth`.
            assert!(
                child.is_ok(),
                "role '{role}' must be admitted as a worker child: {child:?}"
            );
        }
    }

    fn test_plan_services() -> (OneShotPlanServices, tempfile::TempDir) {
        let plans_dir = tempfile::tempdir().expect("create plans directory");
        let services = OneShotPlanServices {
            plan_store: Arc::new(harw_plan::InMemoryPlanStore::new()),
            goal_store: Arc::new(harw_plan::InMemoryGoalStore::new()),
            finding_store: Arc::new(harw_plan_bridge::FindingStore::new(plans_dir.path())),
            plan_config: harw_plan::PlanToolConfig::enabled_defaults(),
            // Die Laufzeit-Beiträge bleiben im Fixture leer: die Tests hier
            // prüfen die Store-Weitergabe, nicht die Registry-Komposition.
            // Dass die Beiträge tatsächlich ankommen, prüfen die Tests in
            // `main.rs` (`as_chat_runtime`) und `harw-tui` (Registry).
            context_providers: Vec::new(),
            approval_handlers: Vec::new(),
            initial_mode: None,
        };
        (services, plans_dir)
    }

    #[test]
    fn one_shot_model_tool_context_exposes_plan_services_when_configured() {
        let project = tempfile::tempdir().expect("create project directory");
        let spawn_context =
            build_one_shot_spawn_context(project.path()).expect("build one-shot spawn context");
        let execution_context = harw_extension_api::ToolExecutionContext::new(
            SessionId::new(),
            harw_types::TurnId::new(),
            spawn_context.sandbox,
        );
        let state_store: Arc<dyn StateStore> = Arc::new(harw_core::InMemoryStateStore::new());
        let (plan_services, _plans_dir) = test_plan_services();

        let context = one_shot_model_tool_context(
            &execution_context,
            &[],
            None,
            &state_store,
            Some(&plan_services),
        );

        assert!(
            context.service::<Arc<dyn harw_plan::PlanStore>>().is_some(),
            "the plan store must be discoverable through the context"
        );
        assert!(
            context
                .service::<Arc<dyn harw_plan::goal::GoalStore>>()
                .is_some(),
            "the goal store must be discoverable through the context"
        );
        assert!(
            context
                .service::<Arc<harw_plan_bridge::FindingStore>>()
                .is_some(),
            "the finding store must be discoverable through the context"
        );
        assert!(
            context.service::<harw_plan::PlanToolConfig>().is_some(),
            "the plan config must be discoverable through the context"
        );
    }

    #[test]
    fn one_shot_model_tool_context_omits_plan_services_when_not_configured() {
        let project = tempfile::tempdir().expect("create project directory");
        let spawn_context =
            build_one_shot_spawn_context(project.path()).expect("build one-shot spawn context");
        let execution_context = harw_extension_api::ToolExecutionContext::new(
            SessionId::new(),
            harw_types::TurnId::new(),
            spawn_context.sandbox,
        );
        let state_store: Arc<dyn StateStore> = Arc::new(harw_core::InMemoryStateStore::new());

        let context =
            one_shot_model_tool_context(&execution_context, &[], None, &state_store, None);

        assert!(context.service::<Arc<dyn harw_plan::PlanStore>>().is_none());
        assert!(
            context
                .service::<Arc<dyn harw_plan::goal::GoalStore>>()
                .is_none()
        );
        assert!(
            context
                .service::<Arc<harw_plan_bridge::FindingStore>>()
                .is_none()
        );
        assert!(context.service::<harw_plan::PlanToolConfig>().is_none());
    }
}
