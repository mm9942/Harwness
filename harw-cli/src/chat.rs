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
//! let startup = ChatStartup {
//!     mode: InteractionMode::Chat,
//!     plan: None,
//!     goal_context: None,
//!     approval: None,
//!     model: None,
//! };
//! chat::run_chat(None, Some("Hallo".to_owned()), None, startup, chat::ChatOptions::default())?;
//! ```

use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
    sync::Arc,
};

use harw_agent_dsl::roles::AgentRoleId;
use harw_config::ResolvedConfig;
use harw_core::{
    ApprovalResolution, InteractionMode, ModelMessage, StateStore, TurnInput, TurnOutcome,
    resume_after_approval, run_turn,
};
use harw_extension_api::{ApprovalMode, ContextProvider};
use harw_memory::facts::{FactScope, FactStore};
use harw_protocol::{SessionEvent, TurnEvent};
use harw_runtime::{
    AssemblyContributor, AssemblyInputs, AssemblyParts, EntryKind, ModelSource, PlanServices,
    RootSession, RuntimeAssembly, RuntimeAssemblyBuilder, RuntimeError, RuntimeResult, RuntimeSpec,
    RuntimeStores,
};
use harw_session_store::JobStore;
use harw_tui::{TuiAssemblyFactory, TuiResume, TuiRunOptions, TuiSessionWiring};
use harw_types::{IngressSurface, SessionId, ThreadRef};
use tokio::{runtime::Builder, sync::mpsc::UnboundedSender};

use crate::home::resolve_home;
use crate::resume::{
    discover_sessions, prompt_for_session, resolve_session_selector, session_matches_project,
};
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
    /// Explizit gewählter Freigabemodus (`--approval`,
    /// `RuntimeSpec::approval_override`); `None` lässt die Konfiguration
    /// entscheiden.
    pub(crate) approval: Option<ApprovalMode>,
    /// Explizit gewähltes Modell (`--model`, `RuntimeSpec::model_override`)
    /// als Schlüssel, Modell-ID oder Alias; `None` lässt die Vorgabe stehen.
    pub(crate) model: Option<String>,
}

/// Zusätzliche Chat-Flags, die `harw-cli/src/cli.rs::ChatArgs` heute noch
/// nicht alle trägt (Scope-Contract §5 Zeile B5: `--all`, `--verbose`,
/// `--add-dir`; diese Datei besitzt nur `resume.rs`/`chat.rs`, nicht
/// `cli.rs`). `Default` bildet exakt das heutige Verhalten ab (kein
/// Projektfilter-Override, keine ausführlichere TUI-Darstellung, keine
/// zusätzlichen Arbeitsverzeichnis-Wurzeln), damit `main.rs` bis zur
/// Ergänzung der fehlenden `ChatArgs`-Felder unverändert
/// `ChatOptions::default()` an [`run_chat`] übergeben kann — sobald `cli.rs`
/// die Felder ergänzt, genügt in `main.rs` je eine Zeile
/// (`all_projects: cli.chat.all`, `verbose: cli.chat.verbose`,
/// `add_dirs: cli.chat.add_dir`) an derselben Konstruktionsstelle.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChatOptions {
    /// `--all`: `harw -r` ohne Selektor zeigt Sessions aller Projekte statt
    /// nur des aktuellen (Contract §4: "`harw -r` zeigt standardmäßig die
    /// Sessions des aktuellen Projekts, `harw -r --all` alle").
    pub(crate) all_projects: bool,
    /// `--verbose`: ausführlichere TUI-Darstellung beim Start (Plan Schritt 2,
    /// Ctrl+O-Äquivalent). Landet als `TuiRunOptions::verbose_tools` in der TUI;
    /// siehe dort für den erwarteten Abnehmer.
    pub(crate) verbose: bool,
    /// `--add-dir`: zusätzliche, für diese Sitzung freigegebene
    /// Arbeitsverzeichnis-Wurzeln, registriert über
    /// `harw_sandbox::ExtraRootsCell` ([`apply_extra_dirs`]).
    pub(crate) add_dirs: Vec<PathBuf>,
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
/// - `options` ([`ChatOptions`]): `--all`/`--verbose`/`--add-dir` (Schritt 7 /
///   Contract §5 Zeile B5), bis `cli.rs` die zugehörigen `ChatArgs`-Felder
///   ergänzt per [`ChatOptions::default`] aufrufbar.
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
/// run_chat(None, None, Some(None), startup, ChatOptions::default())?; // interaktive Session-Auswahl
/// ```
pub fn run_chat(
    home_override: Option<PathBuf>,
    initial_prompt: Option<String>,
    resume_selection: Option<Option<String>>,
    startup: ChatStartup,
    options: ChatOptions,
) -> Result<(), String> {
    validate_chat_mode(initial_prompt.as_deref(), resume_selection.as_ref())?;

    let home = resolve_home(home_override)?;
    crate::home::ensure_home(&home).map_err(|error| error.to_string())?;
    let cwd = std::env::current_dir().map_err(|error| format!("cwd: {error}"))?;
    let ChatOptions {
        all_projects,
        verbose,
        add_dirs,
    } = options;

    let (entry, surface) = if initial_prompt.is_some() {
        (EntryKind::OneShot, IngressSurface::Cli)
    } else {
        (EntryKind::Tui, IngressSurface::Tui)
    };
    let spec = runtime_spec(entry, &home, &cwd, local_principal(surface));
    let mut config = load_chat_config(&spec)?;

    // Ein lokaler Chat braucht immer eine UIA. Fehlt die Auswahl, verwenden
    // wir eine vorhandene UIA oder erzeugen eine minimale lokale Definition;
    // danach wird die Konfiguration neu geladen, damit die Runtime exakt den
    // persistenten Profilwert validiert und montiert.
    if crate::uia_bootstrap::ensure_active_uia(&home, &config)?.is_some() {
        config = load_chat_config(&spec)?;
    }

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

    match initial_prompt {
        Some(prompt) => {
            // One-shot path: reads the real `default_provider`/`default_model`
            // unmodified — the UIA model pin never applies here.
            let inputs = ChatRuntimeInputs::new(spec, &config, startup, verbose)?;
            run_one_shot(&inputs, &prompt, &add_dirs)
        }
        None => {
            // The interactive root receives its UIA provider/model selection
            // through `TuiSessionController`; keep the assembly config's
            // generic defaults untouched so one-shot and worker paths retain
            // their intentional defaults.
            let inputs = ChatRuntimeInputs::new(spec, &config, startup, verbose)?;

            let sessions_root = profile_sessions_root(&home)?;
            let project_key = current_project_key(&cwd);
            // `-r` ohne Selektor am Terminal: die Auflösung liefert bewusst
            // keine Sitzung, die Auswahl übernimmt der Picker der TUI. Ohne
            // Terminal hat `prompt_for_session` bereits entschieden, dann
            // bleibt der Picker aus.
            let bare_resume = matches!(resume_selection, Some(None));
            let existing_session_id = resolve_startup_resume_selection(
                &sessions_root,
                resume_selection,
                project_key.as_deref(),
                all_projects,
            )?;
            let open_picker_at_start = bare_resume && existing_session_id.is_none();
            let factory = ChatTuiFactory::new(inputs, configured_model_source, add_dirs);
            let (assembly, wiring) = factory.assemble(existing_session_id)?;
            harw_tui::run_tui(
                assembly,
                TuiRunOptions {
                    wiring,
                    keybindings_path: harw_home::profile_dir(
                        &home,
                        &harw_home::active_profile_name(&home),
                    )
                    .ok()
                    .map(|dir| dir.join(&config.harness.tui.keybindings_file)),
                    verbose_tools: factory.verbose_tools(),
                    resume: Some(TuiResume {
                        selector: Box::new(ProfileResumeSelector::new(
                            sessions_root.clone(),
                            cwd.clone(),
                            all_projects,
                        )),
                        // Wurzel der Transcript-/Meta-Sidecars für den Picker
                        // (Nachbar-Slice, `harw-tui/src/runtime_root.rs`); dieselbe
                        // Wurzel, die `ProfileResumeSelector`/`discover_sessions`
                        // schon für dieses Profil verwenden.
                        session_store_root: sessions_root,
                        open_picker_at_start,
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
    // Projekt-Fakten-Wurzel (Memory v3, `docs/design/memory-v3-ltm.md` §2/§4),
    // `<projekt>/.harw/memories`, sofern Projekterkennung und `ensure()`
    // gelingen. Über `chat_builder` → `RuntimeAssemblyBuilder::fact_stores`
    // an die Montage gebunden (Addendum B, Agent MEM-RUNTIME); die Montage
    // öffnet ohne diesen Wert selbst eine Projekt-Vorgabe unter
    // `home_project.memories_dir()`, dieser bereits geöffnete Store spart ihr
    // das doppelte Öffnen.
    project_facts: Option<Arc<FactStore>>,
    // Globale Fakten-Wurzel (`<home>/profiles/<profil>/memories`), analog zu
    // `project_facts`; Projekt geht laut Design §4 im Lesepfad vor.
    global_facts: Option<Arc<FactStore>>,
    // `--verbose` (Contract §5 Zeile B5, Plan Schritt 2 Ctrl+O-Äquivalent):
    // wird über `ChatTuiFactory::verbose_tools` als
    // `TuiRunOptions::verbose_tools` an die TUI gereicht.
    verbose: bool,
}

impl ChatRuntimeInputs {
    // Ergänzt `spec` um Modus und aktiven Agenten (E6), öffnet die
    // profilgebundenen Speicher genau einmal und hält `verbose` sowie die
    // Projekt-/Global-Fakten-Wurzeln für die Montage bereit.
    fn new(
        mut spec: RuntimeSpec,
        config: &ResolvedConfig,
        startup: ChatStartup,
        verbose: bool,
    ) -> Result<Self, String> {
        spec.mode_override = Some(startup.mode);
        spec.approval_override = startup.approval;
        spec.model_override = startup.model.clone();
        if startup.approval == Some(ApprovalMode::FullAccess) {
            eprintln!(
                "Warnung: Freigabemodus „full“ aktiv – Schreib- und Shell-Aktionen \
                 laufen in dieser Sitzung ohne Rückfrage."
            );
        }
        spec.active_agent = config.harness.active_agent_definition.clone();

        let home = spec.home.clone();
        let state_store =
            transcript_state_store(&profile_sessions_root(&home)?, cli_thread_for_session);
        let job_store = Arc::new(JobStore::new(&active_profile_job_store_root(&home)?));
        let memory = build_memory(&home, config);
        let secret_resolver = configured_secret_resolver(&home, config)?;
        let (project_facts, global_facts) = open_fact_stores(&home, &spec.cwd);

        Ok(Self {
            spec,
            startup,
            state_store,
            job_store,
            memory,
            secret_resolver,
            project_facts,
            global_facts,
            verbose,
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
    builder = builder.fact_stores(inputs.project_facts.clone(), inputs.global_facts.clone());
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
    // `--add-dir`-Wurzeln (Contract §5 Zeile B5); jede Montage bekommt eine
    // frische `harw_sandbox::ExtraRootsCell` (siehe `RuntimeServices`), daher
    // registriert [`Self::assemble`] sie bei jedem Aufruf erneut — sonst
    // gingen sie bei jedem `/resume` verloren.
    add_dirs: Vec<PathBuf>,
}

impl ChatTuiFactory {
    /// Returns the CLI verbosity selected for every runtime assembled by this factory.
    fn verbose_tools(&self) -> bool {
        self.inputs.verbose
    }

    // Übernimmt die Zutaten, die Modellquelle und die `--add-dir`-Wurzeln.
    fn new(inputs: ChatRuntimeInputs, model: fn() -> ModelSource, add_dirs: Vec<PathBuf>) -> Self {
        Self {
            inputs,
            model,
            add_dirs,
        }
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
        apply_extra_dirs(&assembly, &self.add_dirs);
        tag_session_project(
            &self.inputs.spec.home,
            &self.inputs.spec.cwd,
            assembly.root_session_id(),
        );
        tracing::info!(
            session_id = %assembly.root_session_id(),
            "chat.tui.assembled"
        );
        Ok((Arc::new(assembly), wiring))
    }
}

/// CLI-owned bridge from durable profile transcripts to the TUI `/resume`
/// runtime boundary.
///
/// # Projektfilter (Schritt 7, Contract §5 Zeile B5)
/// `runtime_root.rs` (Nachbar-Slice B3) baut `SessionEntry`s bereits selbst
/// aus `available_sessions()` (unverändert, dieser Trait-Methode) plus
/// `TuiResume::session_store_root` (siehe dessen `session_entries`-Helfer
/// und `app.push_line`-Aufrufer `app.open_session_picker`) — dieser Typ
/// liefert also **keinen** eigenen `Vec<SessionEntry>`-Baustein, sondern
/// wendet den Projektfilter direkt auf die von `available_sessions()`
/// gelieferte ID-Liste an. Ein späteres `Ctrl+A` (`SessionPicker::show_all`)
/// kann diesen Filter zur Laufzeit noch nicht umschalten, da
/// `ResumeSessionSelector::available_sessions` keinen `all`-Parameter
/// entgegennimmt; `self.all` gilt bis dahin nur für den Startwert aus
/// `--all`.
struct ProfileResumeSelector {
    sessions_root: PathBuf,
    // Arbeitsverzeichnis des Prozesses; nötig, um den Projekt-Schlüssel für
    // den Filter in [`ResumeSessionSelector::available_sessions`] zu
    // ermitteln (Schritt 7 Projektfilter).
    cwd: PathBuf,
    // `--all` (Contract §5 Zeile B5): hebt den Projektfilter auf.
    all: bool,
}

impl ProfileResumeSelector {
    fn new(sessions_root: PathBuf, cwd: PathBuf, all: bool) -> Self {
        Self {
            sessions_root,
            cwd,
            all,
        }
    }

    fn discover(&self) -> Result<Vec<crate::resume::DiscoveredSession>, String> {
        discover_sessions(&self.sessions_root).map_err(|error| error.to_string())
    }
}

impl harw_tui::app::ResumeSessionSelector for ProfileResumeSelector {
    fn available_sessions(&self) -> Result<Vec<SessionId>, String> {
        let project_key = current_project_key(&self.cwd);
        Ok(self
            .discover()?
            .into_iter()
            .filter(|session| self.all || session_matches_project(session, project_key.as_deref()))
            .map(|session| session.id)
            .collect())
    }

    fn resolve_session(&self, selector: &str) -> Result<SessionId, String> {
        let sessions = self.discover()?;
        resolve_session_selector(&sessions, selector).map_err(|error| error.to_string())
    }
}

/// Ermittelt den Projekt-Schlüssel des aktuellen Arbeitsverzeichnisses für
/// den `-r`-Projektfilter (Contract §3/§4).
///
/// # Returns
/// `None` bei Erkennungsfehlern (z. B. `cwd` nicht kanonisierbar) — der
/// Filter behandelt das dann symmetrisch zu Sessions ohne `project_key`
/// (siehe [`crate::resume::session_matches_project`]).
fn current_project_key(cwd: &Path) -> Option<String> {
    match harw_home::project::discover_project(cwd, &[]) {
        Ok(project) => Some(harw_home::project::project_key(&project.root)),
        Err(error) => {
            tracing::warn!(%error, "resume: konnte Projekt-Schlüssel nicht ermitteln");
            None
        }
    }
}

// Ordnet eine Session einmalig dem Projekt des Arbeitsverzeichnisses zu, damit
// der `-r`-Projektfilter sie wiederfindet. Eine bereits zugeordnete Session
// behält ihr Projekt (ein `--all`-Resume aus einem anderen Projekt hängt sie
// nicht um). Best-effort: Fehler werden geloggt und blockieren den Start nie.
fn tag_session_project(home: &Path, cwd: &Path, session_id: &SessionId) {
    let project = match harw_home::project::discover_project(cwd, &[]) {
        Ok(project) => project,
        Err(error) => {
            tracing::warn!(%error, "resume: Projekt für Session-Zuordnung nicht erkannt");
            return;
        }
    };
    let sessions_root = match profile_sessions_root(home) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(%error, "resume: Session-Verzeichnis nicht auflösbar");
            return;
        }
    };
    match harw_session_store::meta::load_or_derive(&sessions_root, session_id) {
        Ok(meta) if meta.project_key.is_some() => {}
        Ok(_) => {
            let key = harw_home::project::project_key(&project.root);
            if let Err(error) = harw_session_store::meta::set_project(
                &sessions_root,
                session_id,
                Some(cwd),
                Some(&project.root),
                Some(&key),
            ) {
                tracing::warn!(session = %session_id, %error, "resume: Projekt-Zuordnung nicht gespeichert");
            }
        }
        Err(error) => {
            tracing::warn!(session = %session_id, %error, "resume: Session-Meta nicht lesbar");
        }
    }
}

/// Registriert `--add-dir`-Wurzeln sitzungsweit über
/// `assembly.services().extra_roots()` (`harw_sandbox::ExtraRootsCell`,
/// bereits von A8/B1 bereitgestellt).
///
/// # Description
/// Ein abgelehnter Kandidat (z. B. `TooMany`, `AncestorOfPrimary`) wird nur
/// mit `tracing::warn!` gemeldet — ein ungültiges `--add-dir` darf den
/// Chatstart nicht verhindern, analog zu [`build_memory`].
fn apply_extra_dirs(assembly: &RuntimeAssembly, add_dirs: &[PathBuf]) {
    if add_dirs.is_empty() {
        return;
    }
    let primary_root = assembly.spec().cwd.clone();
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let extra_roots = assembly.services().extra_roots();
    for dir in add_dirs {
        if let Err(error) = extra_roots.add(dir, false, &primary_root, user_home.as_deref()) {
            tracing::warn!(
                path = %dir.display(),
                %error,
                "--add-dir: Wurzel wurde nicht registriert"
            );
        }
    }
}

/// Resolves startup `--resume` input without allowing a selector to fall back
/// to a freshly-created session.
///
/// # Description
/// `Some(None)` (bare `-r`) unterscheidet zwei Fälle: an einem Terminal gibt
/// diese Funktion `Ok(None)` zurück (frische Sitzung; die eigentliche
/// Auswahl übernimmt der TUI-Picker über `TuiResume::selector`, dessen
/// `available_sessions()` denselben Projektfilter anwendet, siehe
/// [`ProfileResumeSelector`]). Ohne Terminal (Pipes, nicht-interaktive Tests)
/// bleibt [`prompt_for_session`] der einzig mögliche Weg und respektiert
/// denselben Projektfilter wie der Picker.
fn resolve_startup_resume_selection(
    sessions_root: &Path,
    resume_selection: Option<Option<String>>,
    current_project_key: Option<&str>,
    all_projects: bool,
) -> Result<Option<SessionId>, String> {
    let Some(selector) = resume_selection else {
        return Ok(None);
    };

    let sessions = discover_sessions(sessions_root).map_err(|error| error.to_string())?;
    match selector {
        Some(selector) => resolve_session_selector(&sessions, &selector)
            .map(Some)
            .map_err(|error| error.to_string()),
        None if std::io::stdin().is_terminal() => {
            // Ein Terminal bekommt den Vollbild-Picker der TUI statt eines
            // blockierenden stdin-Prompts vor dem eigentlichen Start (siehe
            // Funktions- und `prompt_for_session`-Doku).
            Ok(None)
        }
        None => {
            let filtered: Vec<crate::resume::DiscoveredSession> = sessions
                .into_iter()
                .filter(|session| {
                    all_projects || session_matches_project(session, current_project_key)
                })
                .collect();
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            prompt_for_session(&filtered, &mut stdin.lock(), &mut stdout.lock())
                .map_err(|error| error.to_string())?
                .map(Some)
                .ok_or_else(|| RESUME_SELECTION_CANCELLED.to_owned())
        }
    }
}

/// Baut das persistente Memory-Backend für die Chat-Session.
///
/// # Beschreibung
/// Ist eine aktive UIA konfiguriert (`config.harness.active_uia_definition`
/// löst über `config.agent_definition_dirs`/`config.executable_agents` auf
/// eine Definition mit `role = "user-interface"` auf), öffnet diese Funktion
/// einen [`harw_memory::FileMemoryStore`] unter `<agent_dir>/memory/` — dem
/// **eigenen** Gedächtnis dieser UIA (Structure Plan §3: "jede UIA-Identität
/// hat ihr eigenes Gedächtnis"). Sonst (kein aktiver Agent, Agent nicht
/// auflösbar, oder eine andere Rolle) fällt sie auf die bisherige globale
/// Wurzel `<home>/profiles/<active-profile>/memories/` zurück
/// ([`active_profile_memories_root`]) — unverändertes Verhalten für
/// Root-Orchestrator-/Worker-Einstiege.
///
/// Gibt in beiden Fällen einen `Arc<dyn harw_memory::Memory>` zurück.
/// Schlägt die Verzeichnisauflösung oder das Öffnen fehl (Rechte, ungültiger
/// Pfad), fällt die Funktion auf `None` zurück — die `/memory`-Op meldet dann
/// `NotAvailable`, der restliche Chat läuft weiter.
fn build_memory(home: &Path, config: &ResolvedConfig) -> Option<Arc<dyn harw_memory::Memory>> {
    let root = match uia_agent_memory_root(config) {
        Some(root) => root,
        None => match active_profile_memories_root(home) {
            Ok(root) => root,
            Err(error) => {
                tracing::warn!(%error, "harw-memory: konnte Profilverzeichnis nicht auflösen");
                return None;
            }
        },
    };

    match harw_memory::FileMemoryStore::open(&root) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            tracing::warn!(path = %root.display(), %error, "harw-memory: konnte Backend nicht öffnen");
            None
        }
    }
}

/// Liefert `<agent_dir>/memory/`, wenn eine aktive UIA konfiguriert ist,
/// sonst `None`.
///
/// # Beschreibung
/// Löst `config.harness.active_uia_definition` (dieselbe Konfiguration, die
/// `harw-runtime`s `resolve_active_uia` für die Registry-Montage verwendet)
/// über `config.agent_definition_dirs` auf den Agentenordner auf. Zur
/// Vorsicht wird zusätzlich über `config.executable_agents` die Rolle
/// geprüft: nur `role = AgentRoleId::UserInterface` liefert einen Pfad — ein
/// Root-Orchestrator- oder Worker-Einstieg (keine `active_uia_definition`,
/// oder eine unerwartet andere Rolle) behält die globale Profil-Root.
fn uia_agent_memory_root(config: &ResolvedConfig) -> Option<PathBuf> {
    let definition_id = config.harness.active_uia_definition.as_deref()?;
    let is_uia = config
        .executable_agents
        .get(definition_id)
        .map(|ir| ir.role() == AgentRoleId::UserInterface)
        .unwrap_or(false);
    if !is_uia {
        return None;
    }
    let agent_dir = config.agent_definition_dirs.get(definition_id)?;
    Some(agent_dir.join("memory"))
}

/// Öffnet die projekt- und profilweiten Fakten-Wurzeln (Memory v3,
/// `docs/design/memory-v3-ltm.md` §2/§4: "Projekt-Treffer zuerst").
///
/// # Beschreibung
/// Projekt zuerst: `<projekt>/.harw/memories` über
/// [`harw_home::project::ProjectHome::memories_dir`] (Root wird best-effort
/// über `ProjectHome::ensure` angelegt); danach global unter
/// `<home>/profiles/<profil>/memories`. Jeder Fehlschlag (Projekterkennung,
/// `ensure`, `FactStore::open`) liefert für diese Rolle `None` und wird nur
/// mit `tracing::warn!` gemeldet — wie [`build_memory`] darf ein
/// Gedächtnisproblem den Chatstart nie verhindern.
///
/// # Bindung an die Montage (Addendum B, Agent MEM-RUNTIME)
/// Beide Stores werden über [`ChatRuntimeInputs::project_facts`]/
/// [`ChatRuntimeInputs::global_facts`] gehalten und von `chat_builder` per
/// [`harw_runtime::RuntimeAssemblyBuilder::fact_stores`] an die Montage
/// gebunden — der Gedächtnis-Recall (§4) sieht sie damit für jeden Turn der
/// Wurzelsitzung. [`harw_memory::context_provider::MemoryContextProvider`]
/// selbst kennt weiterhin nur eine HOT/STM/WARM-Wurzel über
/// `M: harw_memory::Memory`; `FactStore` implementiert dieses Trait nicht —
/// der Fakten-Anteil hängt deshalb zusätzlich an [`ChatRuntimeInputs::memory`]
/// (siehe `RuntimeAssembly::build`, Schritt 12c: ohne v2-`Memory`-Store
/// bleibt der Recall aus, unabhängig davon, ob hier Fakten-Stores geöffnet
/// werden konnten).
fn open_fact_stores(home: &Path, cwd: &Path) -> (Option<Arc<FactStore>>, Option<Arc<FactStore>>) {
    let project = project_memories_root(cwd).and_then(|root| {
        match FactStore::open(&root, FactScope::Project) {
            Ok(store) => Some(Arc::new(store)),
            Err(error) => {
                tracing::warn!(
                    path = %root.display(),
                    %error,
                    "harw-memory: konnte Projekt-Fakten-Wurzel nicht öffnen"
                );
                None
            }
        }
    });

    let global = match active_profile_memories_root(home) {
        Ok(root) => match FactStore::open(&root, FactScope::Global) {
            Ok(store) => Some(Arc::new(store)),
            Err(error) => {
                tracing::warn!(
                    path = %root.display(),
                    %error,
                    "harw-memory: konnte globale Fakten-Wurzel nicht öffnen"
                );
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, "harw-memory: konnte Profilverzeichnis für Fakten nicht auflösen");
            None
        }
    };

    (project, global)
}

/// Ermittelt und stellt die projekt-lokale Fakten-Wurzel sicher.
///
/// `None` bei jedem Fehlschlag der Projekterkennung; ein Fehlschlag von
/// `ProjectHome::ensure` wird nur gewarnt — `FactStore::open` legt `facts/`
/// bei Bedarf ohnehin selbst an, `.harw/plans`/`.harw/goals` fehlen dann nur
/// vorübergehend.
fn project_memories_root(cwd: &Path) -> Option<PathBuf> {
    let project = match harw_home::project::discover_project(cwd, &[]) {
        Ok(project) => project,
        Err(error) => {
            tracing::warn!(%error, "harw-memory: konnte Projekt-Root nicht ermitteln");
            return None;
        }
    };
    let project_home = harw_home::project::ProjectHome::at(&project);
    if let Err(error) = project_home.ensure() {
        tracing::warn!(%error, "harw-memory: konnte Projekt-Home nicht anlegen");
    }
    Some(project_home.memories_dir())
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
fn run_one_shot(
    inputs: &ChatRuntimeInputs,
    prompt: &str,
    add_dirs: &[PathBuf],
) -> Result<(), String> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start local runtime: {error}"))?;

    // Kein Renderer konsumiert die Ereignisse; die Empfänger bleiben bis zum
    // Ende gebunden, damit Sendungen nicht an einem geschlossenen Kanal enden.
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
    let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();

    let assembly = one_shot_assembly(inputs, ModelSource::Configured, event_tx.clone())?;
    apply_extra_dirs(&assembly, add_dirs);
    let root_id = assembly.root_session_id().clone();
    tag_session_project(&inputs.spec.home, &inputs.spec.cwd, &root_id);
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
            // Terminale Ausgänge: der Lauf endet ohne Antwort, und der Grund
            // gehört in die Meldung statt in ein stilles „fertig".
            TurnOutcome::Cancelled { reason } => {
                return Err(format!("turn was cancelled: {reason:?}"));
            }
            TurnOutcome::Truncated => {
                return Err("model output was truncated".to_owned());
            }
            TurnOutcome::Refused { detail } => {
                return Err(match detail {
                    Some(detail) => format!("model refused to answer: {detail}"),
                    None => "model refused to answer".to_owned(),
                });
            }
            TurnOutcome::Failed { reason } => {
                return Err(format!("turn failed: {reason}"));
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
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_core::{
        ModelError, ModelFuture, ModelProvider, ModelRequest, ModelResponse, ToolCallResult,
    };
    use harw_extension_api::{ContextFragment, ExtFuture, ToolCall, ToolName, TurnInputContext};
    use harw_protocol::TurnItem;
    use harw_session_store::TranscriptStore;
    use harw_types::ToolCallId;
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

    fn chat_fixture() -> TestResult<ChatFixture> {
        let dir = tempfile::tempdir().map_err(ctx("create fixture directory"))?;
        let home = dir.path().join("home");
        let cwd = dir.path().join("project");
        harw_home::ensure_home(&home).map_err(ctx("scaffold home"))?;
        std::fs::create_dir_all(&cwd).map_err(ctx("create project directory"))?;
        // Projekt-Marker, damit die Projekterkennung genau hier stehen bleibt.
        std::fs::write(cwd.join("Cargo.toml"), "[workspace]\n")
            .map_err(ctx("write project marker"))?;
        write_fixture_uia(&home)?;
        Ok(ChatFixture {
            _dir: dir,
            home,
            cwd,
        })
    }

    /// Legt eine minimale, gültige UIA (`role = "user-interface"`) im
    /// Standardprofil des Test-`home` an und aktiviert sie über
    /// `harness.active_uia_definition`.
    ///
    /// # Beschreibung
    /// Seit dem UIA-Vertrag (siehe `resolve_active_uia` in
    /// `harw-runtime/src/assembly.rs`) montieren `EntryKind::Tui` und
    /// `EntryKind::OneShot` nur mit einer konfigurierten UIA (fail-closed,
    /// `RuntimeError::Registry`). `chat_fixture` deckt beide Einstiege ab und
    /// muss deshalb selbst eine bereitstellen, statt implizit auf einen
    /// Bootstrap außerhalb dieser Crate (`harw-cli/src/uia_bootstrap.rs`) zu
    /// vertrauen. Layout und Inhalt spiegeln exakt `write_generated_uia` dort
    /// sowie `harw-runtime`s eigene Test-Fixtures (`assembly.rs`,
    /// `tests/rights_matrix.rs`):
    /// `<home>/profiles/default/agents/fixture-uia/definition.toml` plus eine
    /// Zeile `active_uia_definition = "<id>"`, dem von `ensure_home`
    /// geschriebenen Profil-`config.toml` vorangestellt (statt sie ans
    /// Dateiende anzuhängen), damit dessen restlicher Inhalt erhalten bleibt
    /// — das aktive Profil ohne `active_profile`-Datei ist `"default"`
    /// (`harw_home::active_profile_name`).
    ///
    /// `PROFILE_CONFIG_TEMPLATE` (`harw-home/src/scaffold.rs`) endet mit
    /// einer offenen `[mcp_listener]`-Tabelle. In TOML gehört ein
    /// schlüssellos vorangestelltes `key = value` nach einem
    /// Tabellenkopf zur zuletzt geöffneten Tabelle — ein Anhängen ans
    /// Dateiende hätte `active_uia_definition` also fälschlich in
    /// `[mcp_listener]` platziert und (mit `deny_unknown_fields`) einen
    /// Parse-Fehler ausgelöst. Voranstellen hält den Schlüssel auf
    /// Root-Ebene, wo `HarnessConfig::active_uia_definition` ihn erwartet.
    fn write_fixture_uia(home: &Path) -> TestResult {
        let profile_dir = home.join("profiles").join("default");
        let agent_dir = profile_dir.join("agents").join("fixture-uia");
        std::fs::create_dir_all(&agent_dir).map_err(ctx("fixture uia dir"))?;
        std::fs::write(
            agent_dir.join("definition.toml"),
            "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
        )
        .map_err(ctx("fixture uia definition"))?;
        let config_path = profile_dir.join("config.toml");
        let existing = std::fs::read_to_string(&config_path)
            .map_err(ctx("read profile config for the fixture UIA"))?;
        let updated =
            format!("active_uia_definition = \"harwness.agent.fixture-uia@1\"\n\n{existing}");
        std::fs::write(&config_path, updated)
            .map_err(ctx("prepend active_uia_definition to profile config"))?;
        Ok(())
    }

    fn fixture_inputs(
        fixture: &ChatFixture,
        entry: EntryKind,
        surface: IngressSurface,
        startup: ChatStartup,
    ) -> TestResult<ChatRuntimeInputs> {
        let spec = runtime_spec(entry, &fixture.home, &fixture.cwd, local_principal(surface));
        let config = load_chat_config(&spec).map_err(ctx("load fixture config"))?;
        ChatRuntimeInputs::new(spec, &config, startup, false).map_err(ctx("build chat inputs"))
    }

    fn startup(mode: InteractionMode) -> ChatStartup {
        ChatStartup {
            mode,
            plan: None,
            goal_context: None,
            approval: None,
            model: None,
        }
    }

    fn echo_model_source() -> ModelSource {
        ModelSource::Echo("chat test reply".to_owned())
    }

    fn write_transcript(sessions_root: &Path, session_id: &str) -> TestResult {
        std::fs::create_dir_all(sessions_root).map_err(ctx("create sessions directory"))?;
        std::fs::write(sessions_root.join(format!("{session_id}.jsonl")), "{}\n")
            .map_err(ctx("write transcript"))?;
        Ok(())
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
    fn active_profile_memories_root_uses_the_active_profile_memories_directory() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create home directory"))?;
        harw_home::ensure_home(home.path()).map_err(ctx("scaffold home"))?;
        std::fs::write(harw_home::active_profile_path(home.path()), "analysis\n")
            .map_err(ctx("select analysis profile"))?;

        let memories_root =
            active_profile_memories_root(home.path()).map_err(ctx("resolve memories root"))?;
        let expected = home
            .path()
            .join("profiles")
            .join("analysis")
            .join("memories");

        assert_eq!(memories_root, expected);
        Ok(())
    }

    #[test]
    fn chat_job_store_uses_the_active_profile_jobs_directory() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create home directory"))?;
        harw_home::ensure_home(home.path()).map_err(ctx("scaffold home"))?;
        std::fs::write(harw_home::active_profile_path(home.path()), "analysis\n")
            .map_err(ctx("select analysis profile"))?;

        let store_root =
            active_profile_job_store_root(home.path()).map_err(ctx("resolve job root"))?;
        let store = JobStore::new(&store_root);

        assert_eq!(
            store.root(),
            home.path().join("profiles").join("analysis").join("jobs")
        );
        Ok(())
    }

    #[test]
    fn prompt_and_resume_are_mutually_exclusive() -> TestResult {
        let result = validate_chat_mode(Some("continue this"), Some(&None));
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "prompt plus interactive resume must be rejected".into(),
            ));
        };

        assert_eq!(error, PROMPT_RESUME_CONFLICT);
        Ok(())
    }

    #[test]
    fn one_shot_and_interactive_resume_modes_are_individually_allowed() {
        assert!(validate_chat_mode(Some("one shot"), None).is_ok());
        assert!(validate_chat_mode(None, Some(&None)).is_ok());
        assert!(validate_chat_mode(None, Some(&Some("session-42".to_owned()))).is_ok());
    }

    #[test]
    fn explicit_startup_resume_uses_the_discovered_session_id() -> TestResult {
        let sessions = tempfile::tempdir().map_err(ctx("create sessions directory"))?;
        write_transcript(sessions.path(), "session-42")?;

        let selected = resolve_startup_resume_selection(
            sessions.path(),
            Some(Some("session-42".to_owned())),
            None,
            false,
        )
        .map_err(ctx("resolve explicit session"))?;

        assert_eq!(selected, Some(SessionId::from_str("session-42")));
        Ok(())
    }

    #[test]
    fn unknown_startup_resume_selector_fails_closed() -> TestResult {
        let sessions = tempfile::tempdir().map_err(ctx("create sessions directory"))?;
        write_transcript(sessions.path(), "session-42")?;

        let result = resolve_startup_resume_selection(
            sessions.path(),
            Some(Some("missing".to_owned())),
            None,
            false,
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "unknown selector must not create a new session".into(),
            ));
        };

        assert!(error.contains("unbekannte Session-Auswahl"));
        Ok(())
    }

    #[test]
    fn no_resume_selection_preserves_new_session_startup() -> TestResult {
        let sessions = tempfile::tempdir().map_err(ctx("create sessions directory"))?;

        assert_eq!(
            resolve_startup_resume_selection(sessions.path(), None, None, false)
                .map_err(ctx("no resume request is valid"))?,
            None
        );
        Ok(())
    }

    // Ein `-r` ohne Wert an einem Terminal darf nicht mehr blockierend über
    // stdin auflösen — das übernimmt seit Schritt 7 der TUI-Picker über
    // `ProfileResumeSelector::available_sessions`. Da `cargo test` selbst
    // typischerweise ohne TTY läuft, dokumentiert dieser Test nur den
    // Nicht-TTY-Zweig: er respektiert denselben Projektfilter wie der
    // Picker, statt alle Sessions unbesehen anzuzeigen.
    #[test]
    fn bare_resume_without_a_tty_prompts_only_over_sessions_matching_the_project_filter()
    -> TestResult {
        let sessions_dir = tempfile::tempdir().map_err(ctx("create sessions directory"))?;
        // Leere Datei (kein `{}`-Inhalt wie `write_transcript`): ein leeres
        // Transcript lässt `meta::load_or_derive` einen frischen Sidecar
        // ableiten, statt an einem nicht-parsbaren Datensatz zu scheitern.
        std::fs::File::create(sessions_dir.path().join("session-other-project.jsonl"))
            .map_err(ctx("create empty transcript for meta derivation"))?;
        harw_session_store::meta::set_project(
            sessions_dir.path(),
            &SessionId::from_str("session-other-project"),
            None,
            None,
            Some("other-project-key"),
        )
        .map_err(ctx("tag session with a foreign project key"))?;

        assert!(
            !std::io::stdin().is_terminal(),
            "test runners are expected to run without a TTY; \
             this test only covers the non-TTY fallback branch"
        );

        // Der Projektfilter greift bereits vor `prompt_for_session`: die
        // einzige Session gehört zu einem anderen Projekt, die gefilterte
        // Liste ist leer, und `prompt_for_session` lehnt eine leere Liste
        // mit `ResumeError::NoSessions` ab, statt stdin überhaupt zu lesen.
        let result = resolve_startup_resume_selection(
            sessions_dir.path(),
            Some(None),
            Some("current-project-key"),
            false,
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "no session matches the current project".into(),
            ));
        };

        assert!(
            error.contains("keine dauerhaften Sessions gefunden"),
            "{error}"
        );
        Ok(())
    }

    // `ProfileResumeSelector::available_sessions` ist der reale Abnehmer des
    // Projektfilters: `runtime_root.rs` (B3) baut `SessionEntry`s direkt aus
    // dieser Liste plus `TuiResume::session_store_root`, ohne einen eigenen
    // `Vec<SessionEntry>`-Baustein von hier entgegenzunehmen.
    #[test]
    fn profile_resume_selector_filters_available_sessions_by_project_unless_all() -> TestResult {
        use harw_tui::app::ResumeSessionSelector;

        let project_dir = tempfile::tempdir().map_err(ctx("create project directory"))?;
        let sessions_dir = tempfile::tempdir().map_err(ctx("create sessions directory"))?;
        std::fs::File::create(sessions_dir.path().join("in-project.jsonl"))
            .map_err(ctx("create empty transcript for meta derivation"))?;
        std::fs::File::create(sessions_dir.path().join("other-project.jsonl"))
            .map_err(ctx("create empty transcript for meta derivation"))?;

        let current_key = current_project_key(project_dir.path()).ok_or(TestError::Missing(
            "a real directory always yields a project key",
        ))?;
        harw_session_store::meta::set_project(
            sessions_dir.path(),
            &SessionId::from_str("in-project"),
            None,
            None,
            Some(current_key.as_str()),
        )
        .map_err(ctx("tag in-project session with the current project key"))?;
        harw_session_store::meta::set_project(
            sessions_dir.path(),
            &SessionId::from_str("other-project"),
            None,
            None,
            Some("some-other-project-key"),
        )
        .map_err(ctx("tag other-project session with a foreign project key"))?;

        let filtered = ProfileResumeSelector::new(
            sessions_dir.path().to_path_buf(),
            project_dir.path().to_path_buf(),
            false,
        );
        assert_eq!(
            filtered
                .available_sessions()
                .map_err(ctx("list filtered sessions"))?,
            vec![SessionId::from_str("in-project")]
        );

        let unfiltered = ProfileResumeSelector::new(
            sessions_dir.path().to_path_buf(),
            project_dir.path().to_path_buf(),
            true,
        );
        let mut all_ids = unfiltered
            .available_sessions()
            .map_err(ctx("list all sessions with --all"))?;
        all_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        assert_eq!(
            all_ids,
            vec![
                SessionId::from_str("in-project"),
                SessionId::from_str("other-project"),
            ]
        );
        Ok(())
    }

    #[test]
    fn test_tag_session_project_tags_an_untagged_session() -> TestResult {
        let fixture = chat_fixture()?;
        let sessions_root =
            profile_sessions_root(&fixture.home).map_err(ctx("resolve sessions root"))?;
        std::fs::create_dir_all(&sessions_root).map_err(ctx("create sessions directory"))?;
        std::fs::File::create(sessions_root.join("sess-untagged.jsonl"))
            .map_err(ctx("create empty transcript for meta derivation"))?;
        let session_id = SessionId::from_str("sess-untagged");

        tag_session_project(&fixture.home, &fixture.cwd, &session_id);

        let meta = harw_session_store::meta::load_or_derive(&sessions_root, &session_id)
            .map_err(ctx("load meta after tagging"))?;
        let expected_key = current_project_key(&fixture.cwd).ok_or(TestError::Missing(
            "fixture cwd always yields a project key",
        ))?;
        assert_eq!(meta.project_key.as_deref(), Some(expected_key.as_str()));
        assert_eq!(meta.cwd.as_deref(), Some(fixture.cwd.as_path()));
        assert!(meta.project_root.is_some());
        Ok(())
    }

    #[test]
    fn test_tag_session_project_does_not_overwrite_an_existing_different_key() -> TestResult {
        let fixture = chat_fixture()?;
        let sessions_root =
            profile_sessions_root(&fixture.home).map_err(ctx("resolve sessions root"))?;
        std::fs::create_dir_all(&sessions_root).map_err(ctx("create sessions directory"))?;
        std::fs::File::create(sessions_root.join("sess-already-tagged.jsonl"))
            .map_err(ctx("create empty transcript for meta derivation"))?;
        let session_id = SessionId::from_str("sess-already-tagged");
        harw_session_store::meta::set_project(
            &sessions_root,
            &session_id,
            None,
            None,
            Some("pre-existing-key"),
        )
        .map_err(ctx("pre-tag session with a foreign project key"))?;

        tag_session_project(&fixture.home, &fixture.cwd, &session_id);

        let meta = harw_session_store::meta::load_or_derive(&sessions_root, &session_id)
            .map_err(ctx("load meta after tagging attempt"))?;
        assert_eq!(meta.project_key.as_deref(), Some("pre-existing-key"));
        Ok(())
    }

    #[test]
    fn test_tag_session_project_with_nonexistent_cwd_leaves_meta_untouched() -> TestResult {
        let fixture = chat_fixture()?;
        let sessions_root =
            profile_sessions_root(&fixture.home).map_err(ctx("resolve sessions root"))?;
        std::fs::create_dir_all(&sessions_root).map_err(ctx("create sessions directory"))?;
        std::fs::File::create(sessions_root.join("sess-missing-cwd.jsonl"))
            .map_err(ctx("create empty transcript for meta derivation"))?;
        let session_id = SessionId::from_str("sess-missing-cwd");
        let missing_cwd = fixture.cwd.join("does-not-exist");

        tag_session_project(&fixture.home, &missing_cwd, &session_id);

        let meta = harw_session_store::meta::load_or_derive(&sessions_root, &session_id)
            .map_err(ctx("load meta after failed tagging attempt"))?;
        assert_eq!(meta.project_key, None);
        assert_eq!(meta.cwd, None);
        assert_eq!(meta.project_root, None);
        Ok(())
    }

    // Bugfix: `harw -r` ohne `--all` zeigte zuvor auch Alt-Sessions ohne
    // `project_key` in jedem Projekt (der Filter behandelte ein fehlendes
    // `project_key` bisher als Universal-Treffer). Eine untagged Alt-Session,
    // deren Transcript keinen per Backfill auflösbaren Projekt-Root enthält
    // (hier: leeres Transcript), bleibt jetzt nur noch über `--all`
    // erreichbar — analog zu einer Session mit einem fremden `project_key`.
    #[test]
    fn test_profile_resume_selector_available_sessions_excludes_untagged_legacy_session_without_all()
    -> TestResult {
        use harw_tui::app::ResumeSessionSelector;

        let project_dir = tempfile::tempdir().map_err(ctx("create project directory"))?;
        let sessions_dir = tempfile::tempdir().map_err(ctx("create sessions directory"))?;
        std::fs::File::create(sessions_dir.path().join("in-project.jsonl"))
            .map_err(ctx("create empty transcript for meta derivation"))?;
        std::fs::File::create(sessions_dir.path().join("legacy-untagged.jsonl"))
            .map_err(ctx("create empty transcript for meta derivation"))?;
        std::fs::File::create(sessions_dir.path().join("other-project.jsonl"))
            .map_err(ctx("create empty transcript for meta derivation"))?;

        let current_key = current_project_key(project_dir.path()).ok_or(TestError::Missing(
            "a real directory always yields a project key",
        ))?;
        harw_session_store::meta::set_project(
            sessions_dir.path(),
            &SessionId::from_str("in-project"),
            None,
            None,
            Some(current_key.as_str()),
        )
        .map_err(ctx("tag in-project session with the current project key"))?;
        harw_session_store::meta::set_project(
            sessions_dir.path(),
            &SessionId::from_str("other-project"),
            None,
            None,
            Some("some-other-project-key"),
        )
        .map_err(ctx("tag other-project session with a foreign project key"))?;
        // `legacy-untagged` gets no `set_project` call and an empty
        // transcript: `meta::load_or_derive` leaves `project_key` as `None`,
        // and `crate::resume::backfill_project_key` finds no path candidate
        // to backfill from, matching a real legacy session whose transcript
        // predates any absolute-path tool usage.

        let filtered = ProfileResumeSelector::new(
            sessions_dir.path().to_path_buf(),
            project_dir.path().to_path_buf(),
            false,
        );
        let visible_ids = filtered
            .available_sessions()
            .map_err(ctx("list filtered sessions"))?;
        assert_eq!(visible_ids, vec![SessionId::from_str("in-project")]);

        let all = ProfileResumeSelector::new(
            sessions_dir.path().to_path_buf(),
            project_dir.path().to_path_buf(),
            true,
        );
        let mut all_ids = all
            .available_sessions()
            .map_err(ctx("list all sessions with --all"))?;
        all_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        assert_eq!(
            all_ids,
            vec![
                SessionId::from_str("in-project"),
                SessionId::from_str("legacy-untagged"),
                SessionId::from_str("other-project"),
            ]
        );
        Ok(())
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
    fn test_transcript_state_store_persists_cli_turns_in_the_active_profile_sessions_root()
    -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("create home directory"))?;
        harw_home::ensure_home(home.path()).map_err(ctx("scaffold home"))?;
        let sessions_root =
            profile_sessions_root(home.path()).map_err(ctx("resolve sessions root"))?;
        let store = transcript_state_store(&sessions_root, cli_thread_for_session);
        let session = SessionId::from_str("session-123");
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("persist this turn");
        let item = history
            .items()
            .first()
            .ok_or(TestError::Missing("history has a turn item"))?;
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ctx("build test runtime"))?;

        runtime
            .block_on(store.save_turn(&session, item))
            .map_err(ctx("persist turn through transcript adapter"))?;

        let records = TranscriptStore::new(&sessions_root)
            .reader(&session)
            .map_err(ctx("open CLI transcript"))?
            .collect::<harw_session_store::SessionStoreResult<Vec<_>>>()
            .map_err(ctx("read CLI transcript"))?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].thread, cli_thread_for_session(&session));
        Ok(())
    }

    #[test]
    fn test_goal_context_contributor_registers_provider() -> TestResult {
        let fixture = chat_fixture()?;
        let goal_context: Arc<dyn ContextProvider> = Arc::new(TestGoalContext);
        let with_goal = ChatStartup {
            goal_context: Some(goal_context),
            ..startup(InteractionMode::Chat)
        };
        let inputs = fixture_inputs(&fixture, EntryKind::OneShot, IngressSurface::Cli, with_goal)?;
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();

        let assembly = one_shot_assembly(&inputs, echo_model_source(), event_tx.clone())
            .map_err(ctx("assemble one-shot runtime with goal context"))?;
        let root = assembly
            .new_root_session(assembly.root_session_id().clone(), event_tx, turn_tx, None)
            .map_err(ctx("create root session"))?;

        let registered = root
            .session
            .registry()
            .context_providers()
            .iter()
            .filter(|provider| provider.namespace() == TEST_GOAL_NAMESPACE)
            .count();
        assert_eq!(
            registered, 1,
            "the goal context must be registered exactly once"
        );

        let without_goal = fixture_inputs(
            &fixture,
            EntryKind::OneShot,
            IngressSurface::Cli,
            startup(InteractionMode::Chat),
        )?;
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
        let baseline = one_shot_assembly(&without_goal, echo_model_source(), event_tx.clone())
            .map_err(ctx("assemble one-shot runtime without goal context"))?;
        let baseline_root = baseline
            .new_root_session(baseline.root_session_id().clone(), event_tx, turn_tx, None)
            .map_err(ctx("create baseline root session"))?;
        assert!(
            !baseline_root
                .session
                .registry()
                .context_providers()
                .iter()
                .any(|provider| provider.namespace() == TEST_GOAL_NAMESPACE),
            "without a goal context no such provider may appear"
        );
        Ok(())
    }

    #[test]
    fn test_one_shot_assembly_applies_mode_and_config_policy() -> TestResult {
        let fixture = chat_fixture()?;
        // `[policy]` gehört ins **Profil**-`config.toml`, nicht ins
        // Root-`config.toml` von `fixture.home`: `discover_config_with_restricted`
        // (`harw-config/src/discovery.rs`) läuft `layers` (Root, dann aktives
        // Profil) in aufsteigender Präzedenz durch und ersetzt bei jedem
        // Layer, das ein `config.toml` hat, `resolved.harness` **vollständig**
        // durch die frisch aus diesem Layer geparste `HarnessConfig`
        // (`resolved.harness = cfg;`). Nur eine explizite Handvoll Felder
        // (`default_provider`, `default_model`, `active_uia_definition`,
        // `onboarding`, `internal_models`) wird dabei vom vorherigen Layer
        // fortgeschrieben — `policy` gehört nicht dazu. Das von
        // `harw_home::ensure_home` gescaffoldete Profil-`config.toml`
        // (`PROFILE_CONFIG_TEMPLATE`) kennt kein `[policy]`, deserialisiert es
        // also als leer, und dieser leere Wert überschreibt beim Profil-Layer
        // (dem letzten Layer hier, da `chat_fixture()` kein `.harw` im Projekt
        // anlegt) das `require_approval_for`, das dieser Test zuvor nur ins
        // Root-`config.toml` geschrieben hatte — die Kette sah darum nie etwas
        // davon (`snapshot.config_policy_tools` blieb `[]`). Ein Schreiben ins
        // Root-`config.toml` bliebe also wirkungslos, solange ein Profil-Layer
        // danach folgt; das Profil-`config.toml` ist die Datei, die
        // `one_shot_assembly` für `[policy]` tatsächlich liest.
        let profile_config_path = fixture
            .home
            .join("profiles")
            .join("default")
            .join("config.toml");
        let mut config_file = std::fs::OpenOptions::new()
            .append(true)
            .open(&profile_config_path)
            .map_err(ctx("open profile config"))?;
        config_file
            .write_all(b"\n[policy]\nrequire_approval_for = [\"fs.write\"]\n")
            .map_err(ctx("append approval policy"))?;
        drop(config_file);
        let inputs = fixture_inputs(
            &fixture,
            EntryKind::OneShot,
            IngressSurface::Cli,
            startup(InteractionMode::Explore),
        )?;
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();

        let assembly = one_shot_assembly(&inputs, echo_model_source(), event_tx.clone())
            .map_err(ctx("assemble one-shot runtime"))?;
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
        assert_eq!(
            assembly.spec().mode_override,
            Some(InteractionMode::Explore)
        );

        let root = assembly
            .new_root_session(assembly.root_session_id().clone(), event_tx, turn_tx, None)
            .map_err(ctx("create root session"))?;
        assert_eq!(root.session.mode(), InteractionMode::Explore);
        Ok(())
    }

    #[test]
    fn test_chat_tui_factory_uses_selected_root_session_id() -> TestResult {
        let fixture = chat_fixture()?;
        let inputs = fixture_inputs(
            &fixture,
            EntryKind::Tui,
            IngressSurface::Tui,
            startup(InteractionMode::Chat),
        )?;
        let factory = ChatTuiFactory::new(inputs, echo_model_source, Vec::new());
        let selected = SessionId::from_str("session-42");

        let (resumed, _wiring) = factory
            .assemble(Some(selected.clone()))
            .map_err(ctx("assemble with a selected root session"))?;
        assert_eq!(resumed.root_session_id(), &selected);
        assert_eq!(resumed.spec().entry, EntryKind::Tui);

        let (fresh, _fresh_wiring) = factory
            .assemble(None)
            .map_err(ctx("assemble a fresh session"))?;
        assert_ne!(fresh.root_session_id(), &selected);
        assert!(
            Arc::ptr_eq(resumed.state_store(), fresh.state_store()),
            "every assembly of one factory must share the same transcript store"
        );
        Ok(())
    }

    /// Liefert eine vorprogrammierte Folge von Model-Antworten, eine pro Aufruf
    /// (Muster aus `harw-core/tests/turn_loop.rs::ScriptedModel`).
    struct ScriptedModel {
        responses: std::sync::Mutex<std::collections::VecDeque<ModelResponse>>,
    }

    impl ScriptedModel {
        fn new(responses: Vec<ModelResponse>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into_iter().collect()),
            }
        }
    }

    impl ModelProvider for ScriptedModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            // Test-Double: ein vergifteter Mutex kann hier nur entstehen, wenn
            // ein früherer Aufruf paniken würde — das gibt es in diesem
            // Modul nicht mehr (Bible R087/R165). `into_inner` gewinnt den
            // Guard trotzdem zurück, statt zu paniken.
            let next = self
                .responses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front();
            Box::pin(async move { next.ok_or(ModelError::EmptyResponse) })
        }
    }

    // Befund C8: ein one-shot-Turn ohne interaktiven Responder darf einen
    // per `[policy] require_approval_for` gesperrten Tool-Call nicht in
    // `AwaitingApproval` pausieren lassen — `AskResolution::RejectTurn`
    // (harw-runtime/src/spec.rs:114-115) hängt für `EntryKind::OneShot` eine
    // `AskResolutionPolicy` in die Freigabekette, die jede Rückfrage ohne
    // Responder sofort ablehnt (harw-runtime/src/approval.rs ~:201-211:
    // `would_ask` → `Deny`, nie `AskUser`, wenn niemand antworten kann).
    #[test]
    fn test_one_shot_policy_gated_tool_call_is_denied_without_pausing() -> TestResult {
        let fixture = chat_fixture()?;
        let mut config_file = std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.home.join("config.toml"))
            .map_err(ctx("open home config"))?;
        config_file
            .write_all(b"\n[policy]\nrequire_approval_for = [\"fs.write\"]\n")
            .map_err(ctx("append approval policy"))?;
        drop(config_file);

        let inputs = fixture_inputs(
            &fixture,
            EntryKind::OneShot,
            IngressSurface::Cli,
            startup(InteractionMode::Chat),
        )?;
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_tx, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();

        let gated_call_id = ToolCallId::new();
        let model: Arc<dyn ModelProvider> = Arc::new(ScriptedModel::new(vec![
            ModelResponse {
                message: None,
                tool_calls: vec![ToolCall {
                    id: gated_call_id.clone(),
                    name: ToolName::new("fs.write"),
                    arguments: serde_json::json!({"path": "note.txt", "content": "hi"}),
                }],
                ..Default::default()
            },
            ModelResponse::text("the write request was not carried out"),
        ]));

        let assembly = one_shot_assembly(&inputs, ModelSource::Override(model), event_tx.clone())
            .map_err(ctx(
            "assemble one-shot runtime with a scripted, policy-gated tool call",
        ))?;
        let mut root = assembly
            .new_root_session(assembly.root_session_id().clone(), event_tx, turn_tx, None)
            .map_err(ctx("create root session"))?;

        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ctx("build test runtime"))?;
        let outcome = runtime
            .block_on(run_turn(
                &mut root.session,
                assembly.model().as_ref(),
                assembly.state_store().as_ref(),
                TurnInput::user("please write the file"),
            ))
            .map_err(ctx(
                "a rejected turn still runs to completion, it never errors out",
            ))?;

        assert!(
            matches!(outcome, TurnOutcome::Completed),
            "one-shot's AskResolution::RejectTurn must deny the gated call outright \
             instead of pausing into AwaitingApproval without a responder: {outcome:?}"
        );

        let denial = root
            .session
            .history()
            .items()
            .iter()
            .find_map(|item| match item {
                TurnItem::ToolResult(result) if result.call_id == gated_call_id => {
                    Some(result.result.clone())
                }
                _ => None,
            })
            .ok_or(TestError::Missing(
                "the gated tool call must have produced a tool result in history",
            ))?;
        match denial {
            ToolCallResult::Error { message } => assert!(
                message.contains("denied"),
                "the tool result must record the denial: {message}"
            ),
            ToolCallResult::Success { .. } => {
                return Err(TestError::Unexpected(
                    "a policy-gated fs.write must never be dispatched without an interactive responder".into(),
                ));
            }
        }
        Ok(())
    }
}
