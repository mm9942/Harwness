//! Default-Einstieg von `harw`: Root-Space sicherstellen, Onboarding-Stand
//! prüfen und den Chat über die gemeinsame Runtime-Montage starten.
//!
//! # Beschreibung
//! Fluss: Home auflösen → scaffolden → Konfiguration über
//! [`harw_runtime::load_config`] laden (mit Repo-Vertrauensprüfung) → bei
//! unvollständigem Onboarding den Wizard fahren und neu laden → die
//! Eingangsbeschreibung ([`RuntimeSpec`]) um Modus und aktiven Agenten
//! ergänzen (CONTRACTS-W2d2 E6: der Aufrufer löst beides auf) → Montage über
//! [`RuntimeAssembly::builder`].
//!
//! Dieses Modul montiert **nichts** mehr selbst: Registry, Sandbox,
//! Spawn-Kontext, Kind-Spawner, Freigabekette und Modell-Tool-Fläche entstehen
//! ausschließlich in `harw-runtime` aus dem Einstiegsprofil
//! ([`EntryKind::Tui`] bzw. [`EntryKind::OneShot`]). `chat.rs` liefert nur die
//! Zutaten, die allein die Composition-Root kennt (CONTRACTS-W2d2 §2 C1):
//!
//! - Speicher: durabler Transkript-Speicher und Job-Speicher des aktiven
//!   Profils, Gedächtnis ([`build_memory`]), `secrets:`-Resolver.
//! - Planungsdienste und Ziel-Kontext aus [`ChatStartup`] — von `main.rs`
//!   genau einmal geöffnet und hier nur durchgereicht; der Ziel-Kontext
//!   gelangt über den privaten `GoalContextContributor` in die Registry (E11).
//!
//! # Zweige
//! - **Interaktiv** (kein `initial_prompt`): `ChatTuiFactory` implementiert
//!   [`TuiAssemblyFactory`]; dieselbe Fabrik baut die Start-Montage und jede
//!   `/resume`-Montage (E2). Übergabe an [`harw_tui::run_tui`].
//! - **One-shot** (`initial_prompt`): [`EntryKind::OneShot`] mit Modus
//!   (Befund F-154), ein Turn, Antwort auf stdout. Rückfragen löst das Profil
//!   (`AskResolution::RejectTurn`) bereits in der Kette ab; der Zweig
//!   `AwaitingApproval` bleibt defensiv und lehnt ab.
//!
//! # Nebenläufigkeit
//! Synchroner Einstieg. Der One-shot-Zweig startet eine eigene
//! `current_thread`-Tokio-Runtime; der interaktive Zweig überlässt das
//! [`harw_tui::run_tui`]. Geteilte Speicher liegen als `Arc` in
//! `ChatRuntimeInputs` und werden je Montage nur per `Arc::clone` weitergegeben
//! — ein `/resume` öffnet nie einen zweiten Speicher auf demselben Verzeichnis.
//!
//! # Fehler
//! Alle Fehler sind menschenlesbare `String`s (Home, Scaffolding,
//! Konfiguration, Montage, TUI, Turn). Ein Provider-Fehler der Montage wird
//! auf den Onboarding-Hinweis abgebildet (`assembly_error`).
//!
//! # Beispiel
//! ```rust,ignore
//! let startup = ChatStartup { mode: InteractionMode::Chat, plan: None, goal_context: None };
//! chat::run_chat(None, Some("Hallo".to_owned()), None, startup)?;
//! ```

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use harw_config::ResolvedConfig;
use harw_core::{
    ApprovalResolution, InteractionMode, ModelMessage, StateStore, TurnInput, TurnOutcome,
    resume_after_approval, run_turn,
};
use harw_extension_api::ContextProvider;
use harw_protocol::{SessionEvent, TurnEvent};
use harw_runtime::{
    AssemblyContributor, AssemblyInputs, AssemblyParts, EntryKind, ModelSource, PlanServices,
    RootSession, RuntimeAssembly, RuntimeAssemblyBuilder, RuntimeError, RuntimeResult,
    RuntimeSpec, RuntimeStores,
};
use harw_session_store::JobStore;
use harw_tui::{TuiAssemblyFactory, TuiResume, TuiRunOptions, TuiSessionWiring};
use harw_types::{IngressSurface, SessionId, ThreadRef};
use tokio::{runtime::Builder, sync::mpsc::UnboundedSender};

use crate::home::resolve_home;
use crate::resume::{discover_sessions, prompt_for_session, resolve_session_selector};
use crate::runtime_entry::{
    configured_secret_resolver, local_principal, profile_sessions_root, runtime_spec,
    transcript_state_store,
};

const ONE_SHOT_APPROVAL_DENIAL: &str = "one-shot mode is non-interactive; approval-gated write or shell calls are denied. \
     Re-run `harw chat` without an initial prompt to review and approve the request interactively.";
const PROMPT_RESUME_CONFLICT: &str = "cannot combine an initial prompt with --resume; use --resume without a prompt for interactive session recovery, or omit --resume for one-shot mode.";
const RESUME_SELECTION_CANCELLED: &str =
    "session resume cancelled; no session was selected, so a new session was not started.";

// Die fail-closed Auflösung einer defensiv abgefangenen One-shot-Rückfrage.
fn one_shot_approval_rejection() -> ApprovalResolution {
    ApprovalResolution::Reject {
        reason: ONE_SHOT_APPROVAL_DENIAL.to_owned(),
    }
}

/// Startausstattung des Chats, von der Composition-Root (`main.rs`) gebaut.
///
/// # Description
/// CONTRACTS-W2d2 §1.3. `main.rs` löst `--mode` bzw. `[mode] default` zu
/// `mode` auf, öffnet die Planungsdienste **genau einmal** gegen das aktive
/// Profil und baut optional den Ziel-Kontext
/// (`harw_plan_bridge::GoalContextProvider`). `chat.rs` öffnet keinen dieser
/// Dienste selbst — zwei Store-Instanzen auf derselben Datei wären ein
/// Datenverlust-Risiko.
///
/// # Concurrency
/// `Clone`: `mode` ist `Copy`, `plan` besteht aus `Arc`s, `goal_context` ist
/// ein `Arc` — ein Klon teilt dieselben Dienste.
#[derive(Clone)]
pub(crate) struct ChatStartup {
    /// Interaktionsmodus der Wurzelsitzung (`RuntimeSpec::mode_override`).
    pub(crate) mode: InteractionMode,
    /// Planungsdienste; `None` lässt die Plan-Operationen unregistriert.
    pub(crate) plan: Option<PlanServices>,
    /// Kontext-Beitrag für jeden Turn (Ziel-Kontext); `None` heißt keiner.
    pub(crate) goal_context: Option<Arc<dyn ContextProvider>>,
}

/// Startet den Default-Chat-Pfad.
///
/// # Description
/// Validiert die Kombination aus Prompt und `--resume`, bereitet Home und
/// Konfiguration vor und verzweigt in den One-shot- bzw. interaktiven Pfad
/// (siehe Modul-Doku).
///
/// # Arguments
/// - `home_override` (`Option<PathBuf>`): expliziter Root-Space (`--home`).
/// - `initial_prompt` (`Option<String>`): einmaliger Prompt; ohne ihn wird der
///   interaktive Chat betreten.
/// - `resume_selection` (`Option<Option<String>>`): vorhandene Session
///   fortsetzen; ohne Selector wird sie interaktiv ausgewählt.
/// - `startup` (`ChatStartup`): Modus, Planungsdienste, Ziel-Kontext.
///
/// # Returns
/// `Ok(())`, sobald der Turn ausgegeben bzw. die TUI beendet wurde.
///
/// # Errors
/// Ein `String` bei Konflikt Prompt/`--resume`, Home-Auflösung,
/// Scaffolding, Konfiguration, Onboarding, Montage, TUI oder Turn.
///
/// # Concurrency
/// Blockiert den aufrufenden Thread bis zum Ende des Chats.
///
/// # Examples
/// ```rust,ignore
/// run_chat(None, None, Some(None), startup)?; // interaktive Session-Auswahl
/// ```
pub fn run_chat(
    home_override: Option<PathBuf>,
    initial_prompt: Option<String>,
    resume_selection: Option<Option<String>>,
    startup: ChatStartup,
) -> Result<(), String> {
    validate_chat_mode(initial_prompt.as_deref(), resume_selection.as_ref())?;

    let home = resolve_home(home_override)?;
    harw_home::ensure_home(&home).map_err(|error| error.to_string())?;
    let cwd = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;

    let (entry, surface) = if initial_prompt.is_some() {
        (EntryKind::OneShot, IngressSurface::Cli)
    } else {
        (EntryKind::Tui, IngressSurface::Tui)
    };
    let spec = runtime_spec(entry, &home, &cwd, local_principal(surface));
    let mut config = load_chat_config(&spec)?;

    // Erststart-Fluss: nur wenn der Home-Workspace noch nicht einsatzbereit ist,
    // führt der Wizard durch die Einrichtung. „Einsatzbereit" heißt: die
    // `onboarding.seen`-Flags sind vollständig ODER es sind bereits ein
    // Default-Provider bzw. Provider gesetzt (robust gegen driftende Flags).
    let workspace_ready = config.harness.onboarding.seen.is_complete()
        || config.harness.default_provider.is_some()
        || !config.providers.is_empty();
    if !workspace_ready {
        crate::onboarding::run_wizard(&home)?;
        config = load_chat_config(&spec)?;
    }

    let inputs = ChatRuntimeInputs::new(spec, &config, startup)?;

    match initial_prompt {
        Some(prompt) => run_one_shot(&inputs, &prompt),
        None => {
            let sessions_root = profile_sessions_root(&home)?;
            let existing_session_id =
                resolve_startup_resume_selection(&sessions_root, resume_selection)?;
            let factory = ChatTuiFactory::new(inputs, configured_model_source);
            let (assembly, wiring) = factory.assemble(existing_session_id)?;
            harw_tui::run_tui(
                assembly,
                TuiRunOptions {
                    wiring,
                    resume: Some(TuiResume {
                        selector: Box::new(ProfileResumeSelector::new(sessions_root)),
                        factory: Box::new(factory),
                    }),
                },
            )
            .map_err(|error| error.to_string())
        }
    }
}

// Lädt die Konfiguration mit Vertrauensbericht; der Bericht selbst wird von
// der Montage erneut erzeugt und dort ausgewertet.
fn load_chat_config(spec: &RuntimeSpec) -> Result<ResolvedConfig, String> {
    harw_runtime::load_config(spec)
        .map(|(config, _trust_report)| config)
        .map_err(|error| error.to_string())
}

// Die Modellquelle des produktiven Pfads: der konfigurierte HTTP-Provider.
fn configured_model_source() -> ModelSource {
    ModelSource::Configured
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

/// Alle Zutaten einer Chat-Montage, die über mehrere Montagen geteilt werden.
///
/// # Description
/// Entsteht einmal je Prozess. Speicher, Gedächtnis und Resolver sind hier
/// bereits geöffnet, damit Start- und `/resume`-Montagen dieselben Instanzen
/// teilen (`Arc::clone`), statt Verzeichnisse erneut zu öffnen.
///
/// # Concurrency
/// Nur `Arc`s und Werttypen; wird von `ChatTuiFactory` besessen und
/// ausschließlich gelesen.
struct ChatRuntimeInputs {
    // Eingangsbeschreibung inkl. aufgelöstem Modus und aktivem Agenten.
    spec: RuntimeSpec,
    // Planungsdienste und Ziel-Kontext aus der Composition-Root.
    startup: ChatStartup,
    // Durabler Transkript-Speicher des aktiven Profils.
    state_store: Arc<dyn StateStore>,
    // Job-Speicher des aktiven Profils.
    job_store: Arc<JobStore>,
    // Gedächtnis des aktiven Profils, falls es geöffnet werden konnte.
    memory: Option<Arc<dyn harw_memory::Memory>>,
    // `secrets:`-Resolver, falls die Konfiguration einen verlangt.
    secret_resolver: Option<Arc<dyn harw_provider_http::SecretResolver + Send + Sync>>,
}

impl ChatRuntimeInputs {
    // Ergänzt `spec` um Modus und aktiven Agenten (E6) und öffnet die
    // profilgebundenen Speicher genau einmal.
    fn new(
        mut spec: RuntimeSpec,
        config: &ResolvedConfig,
        startup: ChatStartup,
    ) -> Result<Self, String> {
        spec.mode_override = Some(startup.mode);
        spec.active_agent = config.harness.active_agent_definition.clone();

        let home = spec.home.clone();
        let state_store =
            transcript_state_store(&profile_sessions_root(&home)?, cli_thread_for_session);
        let job_store = Arc::new(JobStore::new(&active_profile_job_store_root(&home)?));
        let memory = build_memory(&home);
        let secret_resolver = configured_secret_resolver(&home, config)?;

        Ok(Self {
            spec,
            startup,
            state_store,
            job_store,
            memory,
            secret_resolver,
        })
    }
}

// Setzt den Builder einer Chat-Montage zusammen: Modell, Resolver, Speicher,
// Planungsdienste, Gedächtnis und Ziel-Kontext. Ereigniskanal und
// Wurzelkennung ergänzt der jeweilige Zweig.
fn chat_builder(inputs: &ChatRuntimeInputs, model: ModelSource) -> RuntimeAssemblyBuilder {
    let mut builder = RuntimeAssembly::builder(inputs.spec.clone())
        .model(model)
        .stores(RuntimeStores {
            state_store: Arc::clone(&inputs.state_store),
            job_store: Some(Arc::clone(&inputs.job_store)),
            approval_store: None,
        });
    if let Some(resolver) = inputs.secret_resolver.as_ref() {
        builder = builder.secret_resolver(Arc::clone(resolver));
    }
    if let Some(plan) = inputs.startup.plan.as_ref() {
        builder = builder.plan_services(plan.clone());
    }
    if let Some(memory) = inputs.memory.as_ref() {
        builder = builder.memory(Arc::clone(memory));
    }
    if let Some(provider) = inputs.startup.goal_context.as_ref() {
        builder = builder.contributor(Arc::new(GoalContextContributor {
            provider: Arc::clone(provider),
        }));
    }
    builder
}

// Bildet einen Montagefehler auf eine menschenlesbare Meldung ab; ein
// Provider-Fehler verweist wie bisher auf das Onboarding.
fn assembly_error(error: RuntimeError) -> String {
    match error {
        RuntimeError::Provider { detail } => {
            format!("Provider-Einrichtung unvollständig: {detail}. Prüfe harw onboard.")
        }
        other => other.to_string(),
    }
}

/// Hängt den Ziel-Kontext der Composition-Root in die Wurzel-Registry.
///
/// # Description
/// CONTRACTS-W2d2 E11. Additiv und rechteneutral: registriert genau einen
/// [`ContextProvider`] über `ExtensionRegistryBuilder::context_provider`
/// (inkl. Namensraum-Kollisionsprüfung). Scheitert die Registrierung, bricht
/// die Montage ab; der dabei geleerte Registry-Bauer wird nie verwendet.
///
/// # Concurrency
/// `Send + Sync`: hält nur ein `Arc<dyn ContextProvider>`.
struct GoalContextContributor {
    // Der von `main.rs` gebaute Kontext-Provider.
    provider: Arc<dyn ContextProvider>,
}

impl AssemblyContributor for GoalContextContributor {
    fn contribute(
        &self,
        _inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        let registry = std::mem::take(&mut parts.registry);
        parts.registry = registry
            .context_provider(Arc::clone(&self.provider))
            .map_err(|error| RuntimeError::Registry {
                detail: format!("could not register the goal context provider: {error}"),
            })?;
        Ok(())
    }
}

/// Baut Montagen für den interaktiven Chat — beim Start und bei `/resume`.
///
/// # Description
/// CONTRACTS-W2d2 E2. Jede Montage bekommt ein frisches
/// [`TuiSessionWiring`] (Controller + Ereigniskanal), eine optional gewählte
/// Wurzelkennung und dieselben geteilten Speicher aus `ChatRuntimeInputs`.
///
/// # Concurrency
/// Zustandslos außer Lesezugriff auf `self`.
struct ChatTuiFactory {
    // Geteilte Zutaten aller Montagen dieses Prozesses.
    inputs: ChatRuntimeInputs,
    // Quelle des Wurzel-Modells je Montage (produktiv: `Configured`).
    model: fn() -> ModelSource,
}

impl ChatTuiFactory {
    // Übernimmt die Zutaten und die Modellquelle.
    fn new(inputs: ChatRuntimeInputs, model: fn() -> ModelSource) -> Self {
        Self { inputs, model }
    }
}

impl TuiAssemblyFactory for ChatTuiFactory {
    fn assemble(
        &self,
        root_session_id: Option<SessionId>,
    ) -> Result<(Arc<RuntimeAssembly>, TuiSessionWiring), String> {
        let wiring = TuiSessionWiring::new();
        let mut builder = wiring.install(chat_builder(&self.inputs, (self.model)()));
        if let Some(id) = root_session_id {
            builder = builder.root_session_id(id);
        }
        let assembly = builder.build().map_err(assembly_error)?;
        tracing::info!(
            session_id = %assembly.root_session_id(),
            "chat.tui.assembled"
        );
        Ok((Arc::new(assembly), wiring))
    }
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

/// Baut das persistente Memory-Backend für die Chat-Session.
///
/// # Beschreibung
/// Öffnet einen [`harw_memory::FileMemoryStore`] unter
/// `<home>/profiles/<active-profile>/memories/` und gibt ihn als
/// `Arc<dyn harw_memory::Memory>` zurück. Schlägt die Profilauflösung oder das
/// Öffnen fehl (Rechte, ungültiger Pfad), fällt die Funktion auf `None` zurück
/// — die `/memory`-Op meldet dann `NotAvailable`, der restliche Chat läuft
/// weiter.
fn build_memory(home: &Path) -> Option<Arc<dyn harw_memory::Memory>> {
    let root = match active_profile_memories_root(home) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(%error, "harw-memory: konnte Profilverzeichnis nicht auflösen");
            return None;
        }
    };

    match harw_memory::FileMemoryStore::open(&root) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            tracing::warn!(path = %root.display(), %error, "harw-memory: konnte Backend nicht öffnen");
            None
        }
    }
}

/// Ordnet jede lokale CLI-Session stabil ihrem Transcript-Thread zu.
///
/// Der Präfix trennt die lokale CLI-Provenienz von anderen Ingressen;
/// der unveränderte `SessionId`-Anteil macht die Zuordnung nach einem Prozess-
/// Neustart reproduzierbar.
fn cli_thread_for_session(session_id: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("cli-session:{}", session_id.as_str()))
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

// Montiert den One-shot-Lauf. `events` geht an den Builder (der Kind-Spawner
// von `SpawnerPolicy::BuiltinRoles` braucht ihn beim Bau) und muss danach
// derselbe Sender sein, der an `new_root_session` geht.
fn one_shot_assembly(
    inputs: &ChatRuntimeInputs,
    model: ModelSource,
    events: UnboundedSender<SessionEvent>,
) -> Result<RuntimeAssembly, String> {
    chat_builder(inputs, model)
        .session_events(events)
        .build()
        .map_err(assembly_error)
}

// Führt genau einen Turn über die One-shot-Montage aus und druckt die Antwort.
fn run_one_shot(inputs: &ChatRuntimeInputs, prompt: &str) -> Result<(), String> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start local runtime: {error}"))?;

    // Kein Renderer konsumiert die Ereignisse; die Empfänger bleiben bis zum
    // Ende gebunden, damit Sendungen nicht an einem geschlossenen Kanal enden.
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
    let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();

    let assembly = one_shot_assembly(inputs, ModelSource::Configured, event_tx.clone())?;
    let root_id = assembly.root_session_id().clone();
    let RootSession { mut session, .. } = assembly
        .new_root_session(root_id.clone(), event_tx, turn_tx, None)
        .map_err(assembly_error)?;

    let result = runtime.block_on(async {
        let model = assembly.model().as_ref();
        let state_store = assembly.state_store().as_ref();
        match run_turn(&mut session, model, state_store, TurnInput::user(prompt))
            .await
            .map_err(|error| error.to_string())?
        {
            TurnOutcome::Completed => {}
            TurnOutcome::AwaitingChild { .. } => {
                return Err("provider unexpectedly paused a turn".to_owned());
            }
            TurnOutcome::AwaitingApproval { .. } => {
                // Defensiv: `AskResolution::RejectTurn` lehnt Rückfragen schon
                // in der Kette ab. Kommt dennoch eine an, wird sie abgelehnt,
                // nie implizit freigegeben; `resume_after_approval` prüft den
                // Akteur gegen die offene Anfrage.
                let Some(actor) = assembly.principal().approval_actor() else {
                    return Err(ONE_SHOT_APPROVAL_DENIAL.to_owned());
                };
                // Das Ergebnis des abgelehnten Turns ist unerheblich: der Lauf
                // endet ohnehin mit der Ablehnungsmeldung; Fehler propagieren.
                let _rejected_outcome = resume_after_approval(
                    &mut session,
                    model,
                    state_store,
                    actor,
                    one_shot_approval_rejection(),
                )
                .await
                .map_err(|error| error.to_string())?;
                return Err(ONE_SHOT_APPROVAL_DENIAL.to_owned());
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
            .ok_or_else(|| "provider produced no assistant response".to_owned())
    });
    assembly.close_session(&root_id);

    let reply = result?;
    println!("{reply}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_extension_api::{ContextFragment, ExtFuture, TurnInputContext};
    use harw_session_store::TranscriptStore;
    use std::io::Write as _;

    const TEST_GOAL_NAMESPACE: &str = "chat-test.goal";

    /// Kontext-Provider ohne Beitrag, erkennbar an seinem Namensraum.
    struct TestGoalContext;

    impl ContextProvider for TestGoalContext {
        fn contribute<'a>(
            &'a self,
            _ctx: &'a TurnInputContext,
        ) -> ExtFuture<'a, Vec<ContextFragment>> {
            Box::pin(async { Vec::new() })
        }

        fn namespace(&self) -> &'static str {
            TEST_GOAL_NAMESPACE
        }
    }

    /// Temp-Home (gescaffoldet) und Temp-Projekt mit Marker.
    struct ChatFixture {
        _dir: tempfile::TempDir,
        home: PathBuf,
        cwd: PathBuf,
    }

    fn chat_fixture() -> ChatFixture {
        let dir = tempfile::tempdir().expect("create fixture directory");
        let home = dir.path().join("home");
        let cwd = dir.path().join("project");
        harw_home::ensure_home(&home).expect("scaffold home");
        std::fs::create_dir_all(&cwd).expect("create project directory");
        // Projekt-Marker, damit die Projekterkennung genau hier stehen bleibt.
        std::fs::write(cwd.join("Cargo.toml"), "[workspace]\n").expect("write project marker");
        ChatFixture {
            _dir: dir,
            home,
            cwd,
        }
    }

    fn fixture_inputs(
        fixture: &ChatFixture,
        entry: EntryKind,
        surface: IngressSurface,
        startup: ChatStartup,
    ) -> ChatRuntimeInputs {
        let spec = runtime_spec(entry, &fixture.home, &fixture.cwd, local_principal(surface));
        let config = load_chat_config(&spec).expect("load fixture config");
        ChatRuntimeInputs::new(spec, &config, startup).expect("build chat inputs")
    }

    fn startup(mode: InteractionMode) -> ChatStartup {
        ChatStartup {
            mode,
            plan: None,
            goal_context: None,
        }
    }

    fn echo_model_source() -> ModelSource {
        ModelSource::Echo("chat test reply".to_owned())
    }

    fn write_transcript(sessions_root: &Path, session_id: &str) {
        std::fs::create_dir_all(sessions_root).expect("create sessions directory");
        std::fs::write(sessions_root.join(format!("{session_id}.jsonl")), "{}\n")
            .expect("write transcript");
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
        let store = JobStore::new(&store_root);

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
    fn test_transcript_state_store_persists_cli_turns_in_the_active_profile_sessions_root() {
        let home = tempfile::tempdir().expect("create home directory");
        harw_home::ensure_home(home.path()).expect("scaffold home");
        let sessions_root = profile_sessions_root(home.path()).expect("resolve sessions root");
        let store = transcript_state_store(&sessions_root, cli_thread_for_session);
        let session = SessionId::from_str("session-123");
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("persist this turn");
        let item = history.items().first().expect("history has a turn item");
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build test runtime");

        runtime
            .block_on(store.save_turn(&session, item))
            .expect("persist turn through transcript adapter");

        let records = TranscriptStore::new(&sessions_root)
            .reader(&session)
            .expect("open CLI transcript")
            .collect::<harw_session_store::SessionStoreResult<Vec<_>>>()
            .expect("read CLI transcript");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].thread, cli_thread_for_session(&session));
    }

    #[test]
    fn test_goal_context_contributor_registers_provider() {
        let fixture = chat_fixture();
        let goal_context: Arc<dyn ContextProvider> = Arc::new(TestGoalContext);
        let with_goal = ChatStartup {
            goal_context: Some(goal_context),
            ..startup(InteractionMode::Chat)
        };
        let inputs = fixture_inputs(&fixture, EntryKind::OneShot, IngressSurface::Cli, with_goal);
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();

        let assembly = one_shot_assembly(&inputs, echo_model_source(), event_tx.clone())
            .expect("assemble one-shot runtime with goal context");
        let root = assembly
            .new_root_session(assembly.root_session_id().clone(), event_tx, turn_tx, None)
            .expect("create root session");

        let registered = root
            .session
            .registry()
            .context_providers()
            .iter()
            .filter(|provider| provider.namespace() == TEST_GOAL_NAMESPACE)
            .count();
        assert_eq!(registered, 1, "the goal context must be registered exactly once");

        let without_goal = fixture_inputs(
            &fixture,
            EntryKind::OneShot,
            IngressSurface::Cli,
            startup(InteractionMode::Chat),
        );
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
        let baseline = one_shot_assembly(&without_goal, echo_model_source(), event_tx.clone())
            .expect("assemble one-shot runtime without goal context");
        let baseline_root = baseline
            .new_root_session(baseline.root_session_id().clone(), event_tx, turn_tx, None)
            .expect("create baseline root session");
        assert!(
            !baseline_root
                .session
                .registry()
                .context_providers()
                .iter()
                .any(|provider| provider.namespace() == TEST_GOAL_NAMESPACE),
            "without a goal context no such provider may appear"
        );
    }

    #[test]
    fn test_one_shot_assembly_applies_mode_and_config_policy() {
        let fixture = chat_fixture();
        let mut config_file = std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.home.join("config.toml"))
            .expect("open home config");
        config_file
            .write_all(b"\n[policy]\nrequire_approval_for = [\"fs.write\"]\n")
            .expect("append approval policy");
        drop(config_file);
        let inputs = fixture_inputs(
            &fixture,
            EntryKind::OneShot,
            IngressSurface::Cli,
            startup(InteractionMode::Explore),
        );
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();

        let assembly = one_shot_assembly(&inputs, echo_model_source(), event_tx.clone())
            .expect("assemble one-shot runtime");
        let snapshot = assembly.rights_snapshot();
        assert_eq!(snapshot.entry, EntryKind::OneShot);
        assert!(
            snapshot
                .config_policy_tools
                .iter()
                .any(|tool| tool == "fs.write"),
            "[policy] require_approval_for must reach the one-shot chain: {:?}",
            snapshot.config_policy_tools
        );
        assert_eq!(assembly.spec().mode_override, Some(InteractionMode::Explore));

        let root = assembly
            .new_root_session(assembly.root_session_id().clone(), event_tx, turn_tx, None)
            .expect("create root session");
        assert_eq!(root.session.mode(), InteractionMode::Explore);
    }

    #[test]
    fn test_chat_tui_factory_uses_selected_root_session_id() {
        let fixture = chat_fixture();
        let inputs = fixture_inputs(
            &fixture,
            EntryKind::Tui,
            IngressSurface::Tui,
            startup(InteractionMode::Chat),
        );
        let factory = ChatTuiFactory::new(inputs, echo_model_source);
        let selected = SessionId::from_str("session-42");

        let (resumed, _wiring) = factory
            .assemble(Some(selected.clone()))
            .expect("assemble with a selected root session");
        assert_eq!(resumed.root_session_id(), &selected);
        assert_eq!(resumed.spec().entry, EntryKind::Tui);

        let (fresh, _fresh_wiring) = factory.assemble(None).expect("assemble a fresh session");
        assert_ne!(fresh.root_session_id(), &selected);
        assert!(
            Arc::ptr_eq(resumed.state_store(), fresh.state_store()),
            "every assembly of one factory must share the same transcript store"
        );
    }
}
