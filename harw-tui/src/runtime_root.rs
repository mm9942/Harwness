//! # runtime_root
//!
//! Einstieg der interaktiven TUI über die eine Laufzeit-Montage
//! ([`harw_runtime::RuntimeAssembly`]).
//!
//! ## Verantwortung
//! Seit Welle W2d-2 (CONTRACTS-W2d2 §1.2, §2 T1, E1/E2/E5) montiert die TUI
//! nichts mehr selbst: Registry, Sandbox, Spawn-Kontext, Freigabekette,
//! Operationen, Spawner und Dienste kommen fertig aus der
//! [`RuntimeAssembly`], die die Composition-Root (`harw-cli`) baut. Dieses
//! Modul besitzt nur noch
//! - die Verdrahtung, die **vor** dem Bau in den Builder muss
//!   ([`TuiSessionWiring`]: Sitzungs-Controller und Ereigniskanal),
//! - den Aufbau der Wurzelsitzung samt interaktiver Antwortfläche
//!   (`build_root_runtime`: [`TuiApprovalHandler`] + [`ApprovalDriver`]) und
//!   des Renderer-Zustands ([`ChatApp`]),
//! - den Terminal-Lebenszyklus und die Ereignisschleife mit `/resume`
//!   ([`run_tui`]); ein Wechsel der Sitzung montiert über
//!   [`TuiAssemblyFactory`] eine **neue** Laufzeit, denn eine Montage vergibt
//!   ihre Wurzel-Registry genau einmal.
//!
//! Der Renderer selbst (`run_loop`, Zellen, Tastatur) bleibt in
//! [`crate::app`].
//!
//! ## Schlüsseltypen
//! - [`TuiSessionWiring`] — Controller + Sitzungs-Ereigniskanal einer Montage.
//! - [`TuiAssemblyFactory`] — baut für `/resume <id>` eine neue Montage.
//! - [`TuiResume`] — Auswahl dauerhafter Sitzungen plus Fabrik.
//! - [`TuiRunOptions`] — Eingaben von [`run_tui`] neben der Montage.
//! - `ResumableGateway` — `ChatGateway` mit austauschbarer Sitzung (privat).
//!
//! ## Nebenläufigkeit
//! [`run_tui`] blockiert den aufrufenden Thread und treibt einen
//! `current_thread`-Tokio-Runtime. Ein OS-Thread liest die Tastatur, ein
//! Tokio-Task koalesziert Frame-Anforderungen; alle Kanäle sind unbounded
//! MPSC. Der `Arc<TuiApprovalHandler>` liegt zugleich in der Freigabekette der
//! Sitzung und im [`ApprovalDriver`] (AP W5-03, Bedingung 2).
//!
//! ## Fehler
//! Alle Fehler werden als [`TuiError`] ausgedrückt; Montagefehler der Laufzeit
//! ([`harw_runtime::RuntimeError`]) erscheinen als [`TuiError::Core`] mit
//! vollständigem Text.
//!
//! ## Beispiele
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_tui::{TuiRunOptions, TuiSessionWiring, run_tui};
//!
//! # fn demo(
//! #     builder: harw_runtime::RuntimeAssemblyBuilder,
//! # ) -> Result<(), Box<dyn std::error::Error>> {
//! let wiring = TuiSessionWiring::new();
//! let assembly = Arc::new(wiring.install(builder).build()?);
//! run_tui(assembly, TuiRunOptions { wiring, resume: None })?;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use ratatui::text::Line;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use harw_core::{AgentSession, ModelProvider, StateStore};
use harw_extension_api::ApprovalHandler;
use harw_operations::SharedSessionController;
use harw_operations::adapter::CommandAdapter;
use harw_protocol::events::{SessionEvent, TurnEvent};
use harw_runtime::{RootSession, RuntimeAssembly, RuntimeAssemblyBuilder};
use harw_types::SessionId;

use crate::app::{
    ChatApp, ResumeSessionSelector, TerminalGuard, TuiError, TuiPlanServices, TuiRunOutcome,
    WELCOME, frame_scheduler, install_loaded_history, run_loop,
};
use crate::approval::{ApprovalDriver, ApprovalPromptReceiver, TuiApprovalHandler};
use crate::events::harw_event_channel;
use crate::frame_requester::frame_channel;
use crate::input_reader::spawn_input_reader;
use crate::session_controller::TuiSessionController;
use crate::tui_event::TuiEvent;

/// Hinweis, wenn `/resume` ohne konfigurierte [`TuiResume`] eintrifft.
const RESUME_NOT_CONFIGURED: &str = "Session resume is not configured.";

/// Verdrahtung einer TUI-Montage, die vor [`RuntimeAssemblyBuilder::build`]
/// feststehen muss.
///
/// # Beschreibung
/// Der [`TuiSessionController`] ist derselbe `Arc`, den die Slash- und
/// Modell-Tool-Dienste der Montage tragen und den der Renderer an der
/// Turn-Grenze auf die Sitzung anwendet. Der Ereigniskanal wird bei
/// [`harw_runtime::SpawnerPolicy::BuiltinRoles`] (Einstieg `Tui`) schon beim
/// Bau vom Spawner gebraucht; [`run_tui`] reicht denselben Sender an
/// [`RuntimeAssembly::new_root_session`] und liest den Empfänger im Loop.
///
/// Jede Montage bekommt ihre **eigene** Verdrahtung; eine per `/resume`
/// gebaute Montage liefert sie über [`TuiAssemblyFactory::assemble`] mit.
///
/// # Nebenläufigkeit
/// Nicht `Clone`: der Empfänger existiert genau einmal. Controller und Sender
/// sind `Send + Sync`-fähig geteilt (`Arc`, `UnboundedSender`).
pub struct TuiSessionWiring {
    /// Sitzungs-Controller für `/model`, `/effort` und `/mode`.
    controller: Arc<TuiSessionController>,
    /// Senderseite der Sitzungsereignisse (Builder und Wurzelsitzung).
    events_tx: UnboundedSender<SessionEvent>,
    /// Empfängerseite, gelesen von der Ereignisschleife.
    events_rx: UnboundedReceiver<SessionEvent>,
}

impl TuiSessionWiring {
    /// Erzeugt einen frischen Controller und einen neuen Ereigniskanal.
    ///
    /// # Rückgabe
    /// Eine unbenutzte [`TuiSessionWiring`].
    ///
    /// # Nebenläufigkeit
    /// Synchron, allokiert nur.
    ///
    /// # Beispiele
    /// ```rust,no_run
    /// let wiring = harw_tui::TuiSessionWiring::new();
    /// # drop(wiring);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            controller: Arc::new(TuiSessionController::new()),
            events_tx,
            events_rx,
        }
    }

    /// Legt Controller und Ereignissender in einen Montage-Builder.
    ///
    /// # Beschreibung
    /// Ruft [`RuntimeAssemblyBuilder::session_controller`] mit einem
    /// `Arc`-Zeiger auf **denselben** Controller und
    /// [`RuntimeAssemblyBuilder::session_events`] mit einem Klon des Senders.
    ///
    /// # Argumente
    /// - `builder` ([`RuntimeAssemblyBuilder`]): der Builder der Composition-Root.
    ///
    /// # Rückgabe
    /// Der ergänzte Builder.
    ///
    /// # Nebenläufigkeit
    /// Klont nur Zeiger/Sender.
    ///
    /// # Beispiele
    /// ```rust,no_run
    /// # fn demo(builder: harw_runtime::RuntimeAssemblyBuilder) {
    /// let wiring = harw_tui::TuiSessionWiring::new();
    /// let builder = wiring.install(builder);
    /// # drop(builder);
    /// # }
    /// ```
    #[must_use]
    pub fn install(&self, builder: RuntimeAssemblyBuilder) -> RuntimeAssemblyBuilder {
        // Typparameter explizit, damit die Unsized-Coercion zu
        // `Arc<dyn SessionController>` erst beim Binden geschieht.
        let controller: SharedSessionController =
            Arc::<TuiSessionController>::clone(&self.controller);
        builder
            .session_controller(controller)
            .session_events(self.events_tx.clone())
    }
}

impl Default for TuiSessionWiring {
    /// Wie [`TuiSessionWiring::new`].
    fn default() -> Self {
        Self::new()
    }
}

/// Handgeschriebenes `Debug`: Kanäle leiten kein `Debug` ab.
impl std::fmt::Debug for TuiSessionWiring {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TuiSessionWiring")
            .field("controller", &self.controller)
            .field("events_closed", &self.events_tx.is_closed())
            .finish_non_exhaustive()
    }
}

/// Montiert für `/resume <selector>` eine neue Laufzeit.
///
/// # Beschreibung
/// CONTRACTS-W2d2 E2. Eine [`RuntimeAssembly`] vergibt ihre Wurzel-Registry
/// genau einmal und registriert beim Spawner genau eine Wurzelkennung; ein
/// Sitzungswechsel braucht deshalb eine vollständig neue Montage. Die
/// Composition-Root kennt Config, Home, Modell und Speicher und baut sie hier.
pub trait TuiAssemblyFactory {
    /// Baut eine Montage samt eigener Verdrahtung.
    ///
    /// # Argumente
    /// - `root_session_id` (`Option<SessionId>`): die fortzusetzende Sitzung;
    ///   `None` prägt eine frische Kennung (E5).
    ///
    /// # Rückgabe
    /// Die Montage und die [`TuiSessionWiring`], die bei ihrem Bau per
    /// [`TuiSessionWiring::install`] eingelegt wurde.
    ///
    /// # Fehler
    /// Menschenlesbarer Text, wenn die Montage scheitert; [`run_tui`] zeigt ihn
    /// an und bleibt in der aktuellen Sitzung.
    fn assemble(
        &self,
        root_session_id: Option<SessionId>,
    ) -> Result<(Arc<RuntimeAssembly>, TuiSessionWiring), String>;
}

/// Alles, was `/resume` braucht.
pub struct TuiResume {
    /// Listet und löst dauerhafte Sitzungen auf.
    pub selector: Box<dyn ResumeSessionSelector>,
    /// Montiert die gewählte Sitzung.
    pub factory: Box<dyn TuiAssemblyFactory>,
}

/// Handgeschriebenes `Debug`: die Trait-Objekte leiten kein `Debug` ab.
impl std::fmt::Debug for TuiResume {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TuiResume")
            .field("selector", &"<dyn ResumeSessionSelector>")
            .field("factory", &"<dyn TuiAssemblyFactory>")
            .finish()
    }
}

/// Eingaben von [`run_tui`] neben der Montage.
pub struct TuiRunOptions {
    /// Die Verdrahtung, die in den Builder der übergebenen Montage gelegt wurde.
    pub wiring: TuiSessionWiring,
    /// `/resume`-Unterstützung; `None` lehnt `/resume` mit Hinweis ab.
    pub resume: Option<TuiResume>,
}

/// Handgeschriebenes `Debug`: [`TuiSessionWiring`] enthält Kanäle.
impl std::fmt::Debug for TuiRunOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TuiRunOptions")
            .field("wiring", &self.wiring)
            .field("resume", &self.resume)
            .finish()
    }
}

/// Startet die interaktive TUI über eine fertige Montage.
///
/// # Beschreibung
/// 1. Baut einen `current_thread`-Tokio-Runtime.
/// 2. Baut die Wurzelsitzung und den Renderer-Zustand (`build_root_runtime`).
/// 3. Lädt den dauerhaften Verlauf der Sitzung und hängt die Willkommenszeile an.
/// 4. Betritt Raw-Mode/Alternate-Screen hinter `TerminalGuard` und treibt
///    `run_loop`, bis `Quit` kommt oder ein Fehler auftritt.
///
/// `/resume` ohne Selektor listet die Sitzungen aus [`TuiResume::selector`];
/// `/resume <selector>` löst auf, montiert über [`TuiResume::factory`] neu,
/// tauscht Sitzung, Speicher, Modell, Renderer-Zustand, Kanäle und
/// Freigabetreiber gemeinsam aus und schließt danach die alte Sitzung
/// ([`RuntimeAssembly::close_session`]). Scheitert einer dieser Schritte,
/// bleibt die bisherige Sitzung aktiv und der Fehler erscheint als
/// Systemzeile. Beim Verlassen der Schleife wird die aktive Sitzung
/// geschlossen.
///
/// # Argumente
/// - `assembly` (`Arc<RuntimeAssembly>`): Montage mit Einstieg `Tui`, gebaut
///   mit `options.wiring` ([`TuiSessionWiring::install`]).
/// - `options` ([`TuiRunOptions`]): Verdrahtung und optionales `/resume`.
///
/// # Rückgabe
/// `Ok(())` bei sauberem Verlassen.
///
/// # Fehler
/// - [`TuiError::Io`]: Runtime-, Terminal- oder Zeichenfehler.
/// - [`TuiError::Core`]: Wurzelsitzung nicht montierbar, Verlauf der
///   Startsitzung nicht ladbar oder Fehler im Turn-Loop.
///
/// # Nebenläufigkeit
/// Blockiert den aufrufenden Thread; startet einen Eingabe-Reader-Thread und
/// einen Frame-Scheduler-Task.
///
/// # Beispiele
/// ```rust,no_run
/// # use std::sync::Arc;
/// # fn demo(
/// #     assembly: Arc<harw_runtime::RuntimeAssembly>,
/// #     wiring: harw_tui::TuiSessionWiring,
/// # ) -> Result<(), harw_tui::TuiError> {
/// harw_tui::run_tui(assembly, harw_tui::TuiRunOptions { wiring, resume: None })
/// # }
/// ```
pub fn run_tui(assembly: Arc<RuntimeAssembly>, options: TuiRunOptions) -> Result<(), TuiError> {
    let TuiRunOptions { wiring, resume } = options;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(TuiError::from)?;

    let RootRuntime {
        mut session,
        mut app,
        mut event_rx,
        mut turn_event_rx,
        mut approval_driver,
        mut approvals,
    } = build_root_runtime(&assembly, wiring)?;
    let history = runtime
        .block_on(assembly.state_store().load_history(session.id()))
        .map_err(|error| TuiError::Core(format!("durable history load failed: {error}")))?;
    install_loaded_history(&mut session, &mut app, history);
    app.push_lines(vec![Line::from(WELCOME)]);
    let mut gateway = ResumableGateway::new(
        session,
        Arc::clone(assembly.state_store()),
        Arc::clone(assembly.model()),
    );
    let mut current = assembly;
    let mut guard = TerminalGuard::enter()?;

    let result = runtime.block_on(async {
        let (tui_tx, mut tui_rx) = tokio::sync::mpsc::unbounded_channel::<TuiEvent>();
        let (harw_tx, mut harw_rx) = harw_event_channel();
        let (frame_req, frame_rx) = frame_channel();
        tokio::spawn(frame_scheduler(frame_rx, tui_tx.clone()));
        let _reader = spawn_input_reader(tui_tx);
        frame_req.schedule_frame();

        loop {
            match run_loop(
                &mut guard,
                &mut app,
                &mut gateway,
                &mut event_rx,
                &mut turn_event_rx,
                &mut tui_rx,
                &harw_tx,
                &mut harw_rx,
                &frame_req,
                &approval_driver,
                &mut approvals,
            )
            .await?
            {
                TuiRunOutcome::Quit => return Ok(()),
                TuiRunOutcome::Resume { selector: None } => {
                    let message = match resume.as_ref() {
                        Some(resume) => resumable_sessions_message(resume.selector.as_ref()),
                        None => RESUME_NOT_CONFIGURED.to_owned(),
                    };
                    push_system_text(&mut app, &message);
                    frame_req.schedule_frame();
                }
                TuiRunOutcome::Resume {
                    selector: Some(raw_selector),
                } => {
                    let Some(resume) = resume.as_ref() else {
                        push_system_text(&mut app, RESUME_NOT_CONFIGURED);
                        frame_req.schedule_frame();
                        continue;
                    };
                    match resume_session(resume, &raw_selector).await {
                        Ok(next) => {
                            let ResumedRuntime {
                                assembly: next_assembly,
                                runtime: next_runtime,
                            } = next;
                            gateway.replace(
                                next_runtime.session,
                                Arc::clone(next_assembly.state_store()),
                                Arc::clone(next_assembly.model()),
                            );
                            app = next_runtime.app;
                            event_rx = next_runtime.event_rx;
                            turn_event_rx = next_runtime.turn_event_rx;
                            // Treiber und Fragekanal gehören zum Handler der
                            // **neuen** Wurzelsitzung; sie werden gemeinsam mit
                            // ihr ersetzt, sonst fände der Treiber die
                            // Rückkanäle nicht mehr.
                            approval_driver = next_runtime.approval_driver;
                            approvals = next_runtime.approvals;
                            let previous = std::mem::replace(&mut current, next_assembly);
                            previous.close_session(previous.root_session_id());
                            tracing::info!(
                                previous = %previous.root_session_id(),
                                session_id = %current.root_session_id(),
                                "tui.session.resumed"
                            );
                        }
                        Err(message) => push_system_text(&mut app, &message),
                    }
                    frame_req.schedule_frame();
                }
            }
        }
    });
    // `guard` stellt das Terminal zurück, auch im Fehlerfall.
    drop(guard);
    current.close_session(current.root_session_id());
    result
}

/// Klont einen [`TuiApprovalHandler`]-Zeiger als Trait-Objekt.
///
/// # Beschreibung
/// Die Umwandlung `Arc<TuiApprovalHandler>` → `Arc<dyn ApprovalHandler>` ist eine
/// Unsized-Coercion und braucht eine eigene Stelle, an der sie stattfindet.
/// `let x: Arc<dyn ApprovalHandler> = Arc::clone(&handler)` **compiliert nicht**:
/// bei der UFCS-Form `Arc::clone` wird der Typparameter aus dem *erwarteten* Typ
/// inferiert, Rust wählt also `T = dyn ApprovalHandler` und verlangt bereits ein
/// `&Arc<dyn ApprovalHandler>` als Argument — die Coercion käme zu spät.
///
/// Bei der Methodensyntax `handler.clone()` steht `T` dagegen durch den Receiver
/// fest; die Coercion geschieht beim Zurückgeben. Deshalb genau diese Form.
///
/// # Argumente
/// - `handler` (`&Arc<TuiApprovalHandler>`): der geteilte Handler.
///
/// # Rückgabe
/// Derselbe Handler als `Arc<dyn ApprovalHandler>` — **kein** zweiter Handler,
/// nur ein zweiter Zeiger. Das ist Bedingung 2 aus `approval.rs`: Kette und
/// Treiber müssen denselben Handler sehen, sonst fände der Treiber die
/// Rückkanäle nicht und löste jede Frage als Ablehnung auf.
pub(crate) fn as_dyn_approval_handler(
    handler: &Arc<TuiApprovalHandler>,
) -> Arc<dyn ApprovalHandler> {
    handler.clone()
}

/// Alles, was eine frisch gebaute Wurzelsitzung an die Ereignisschleife gibt.
///
/// Ein eigener Typ statt eines Tupels: [`ApprovalDriver`] und
/// [`ApprovalPromptReceiver`] gehören zum selben `Arc<TuiApprovalHandler>`,
/// der zugleich in der Freigabekette der Sitzung liegt (AP W5-03).
struct RootRuntime {
    /// Die Wurzelsitzung der Montage.
    session: AgentSession,
    /// Der zugehörige Renderer-Zustand.
    app: ChatApp,
    /// Sitzungsereignisse (Token-Summary, Kind-Sitzungen).
    event_rx: UnboundedReceiver<SessionEvent>,
    /// Turn-Ereignisse (Werkzeuge, Kinder, Plan, Modus).
    turn_event_rx: UnboundedReceiver<TurnEvent>,
    /// Treiber über beide Pausearten; hält denselben Handler wie die Kette.
    approval_driver: ApprovalDriver,
    /// Fragekanal zum Renderer; muss gepollt werden, sonst läuft jede Frage in
    /// den Timeout und gilt als Ablehnung.
    approvals: ApprovalPromptReceiver,
}

/// Eine per `/resume` montierte Laufzeit samt fertig hydrierter Sitzung.
struct ResumedRuntime {
    /// Die neue Montage.
    assembly: Arc<RuntimeAssembly>,
    /// Sitzung, Renderer-Zustand und Kanäle der neuen Montage.
    runtime: RootRuntime,
}

// Baut Wurzelsitzung, Freigabetreiber und Renderer-Zustand aus einer Montage
// (CONTRACTS-W2d2 §2 T1). Die Montage entscheidet Registry, Sandbox, Modus
// (`spec.mode_override`) und Agent; hier wird nur verdrahtet.
fn build_root_runtime(
    assembly: &Arc<RuntimeAssembly>,
    wiring: TuiSessionWiring,
) -> Result<RootRuntime, TuiError> {
    let TuiSessionWiring {
        controller,
        events_tx,
        events_rx,
    } = wiring;
    // AP W5-03, Bedingung 2: genau ein Handler, dessen `Arc` gleichzeitig in
    // der Freigabekette der Sitzung und im `ApprovalDriver` liegt.
    let (approval_handler, approvals) = TuiApprovalHandler::new();
    let approval_driver = ApprovalDriver::new(Arc::clone(&approval_handler));
    let (turn_event_tx, turn_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let root_session_id = assembly.root_session_id().clone();
    let RootSession { session, .. } = assembly
        .new_root_session(
            root_session_id.clone(),
            events_tx,
            turn_event_tx,
            Some(as_dyn_approval_handler(&approval_handler)),
        )
        .map_err(|error| {
            tracing::error!(error = %error, "tui.root_session.failed");
            TuiError::Core(format!("could not build the root session: {error}"))
        })?;

    let adapters: Vec<CommandAdapter> = assembly
        .operations()
        .iter()
        .flat_map(|operation| CommandAdapter::from_operation(Arc::clone(operation)))
        .collect();
    let mut app = ChatApp::with_memory(
        adapters,
        assembly.sandbox().clone(),
        root_session_id,
        assembly.memory().cloned(),
    )
    .with_runtime(Arc::clone(assembly))
    .with_session_controller(controller)
    .with_project_root(assembly.project().project_root.display().to_string())
    .with_managed_spawner(assembly.spawner().cloned());
    if let Some(plan) = assembly.plan_services() {
        app = app.with_plan_services(TuiPlanServices::from(plan));
    }
    app.set_active_mode(session.mode());
    tracing::info!(
        session_id = %session.id(),
        mode = session.mode().as_str(),
        adapters = assembly.operations().iter().count(),
        "tui.root_runtime.built"
    );
    Ok(RootRuntime {
        session,
        app,
        event_rx: events_rx,
        turn_event_rx,
        approval_driver,
        approvals,
    })
}

// Löst `/resume <selector>` auf und montiert die gewählte Sitzung vollständig
// (Montage, Wurzelsitzung, Verlauf, Willkommenszeile). Jeder Fehler wird zu
// einer Systemzeile; die laufende Sitzung bleibt dann unberührt.
async fn resume_session(
    resume: &TuiResume,
    raw_selector: &str,
) -> Result<ResumedRuntime, String> {
    let selected = resume
        .selector
        .resolve_session(raw_selector)
        .map_err(|error| format!("Could not resolve session: {error}"))?;
    let (assembly, wiring) = resume
        .factory
        .assemble(Some(selected))
        .map_err(|error| format!("Could not assemble session: {error}"))?;
    let mut runtime = build_root_runtime(&assembly, wiring)
        .map_err(|error| format!("Could not start session: {error}"))?;
    let history = match assembly.state_store().load_history(runtime.session.id()).await {
        Ok(history) => history,
        Err(error) => {
            // Die Sitzung wurde bereits erzeugt; ihr Ende wird gemeldet, bevor
            // sie verworfen wird.
            assembly.close_session(assembly.root_session_id());
            return Err(format!("durable history load failed: {error}"));
        }
    };
    install_loaded_history(&mut runtime.session, &mut runtime.app, history);
    runtime.app.push_lines(vec![Line::from(WELCOME)]);
    Ok(ResumedRuntime { assembly, runtime })
}

// Baut die Systemzeile für `/resume` ohne Selektor.
fn resumable_sessions_message(selector: &dyn ResumeSessionSelector) -> String {
    match selector.available_sessions() {
        Ok(ids) if ids.is_empty() => "No resumable sessions available.".to_owned(),
        Ok(ids) => format!(
            "Select a session with /resume <selector>:\n{}",
            ids.iter()
                .map(SessionId::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        ),
        Err(error) => format!("Could not list resumable sessions: {error}"),
    }
}

// Hängt mehrzeiligen Systemtext als eine Zelle mit einer Zeile je Textzeile an.
fn push_system_text(app: &mut ChatApp, text: &str) {
    let lines = text.lines().map(|line| Line::from(line.to_owned())).collect();
    app.push_lines(lines);
}

/// Lokaler Gateway mit austauschbarer Sitzung für den `/resume`-Pfad.
///
/// `crate::gateway::LocalGateway` hält seine Felder privat und bietet keinen
/// Austausch; dieser Adapter besitzt genau diese eine zusätzliche
/// Lebenszyklus-Operation, ohne den geteilten Trait zu verbreitern. Speicher
/// und Modell wechseln mit, weil eine neue Montage eigene mitbringen darf.
struct ResumableGateway {
    /// Die aktive Sitzung.
    session: AgentSession,
    /// Verlaufsspeicher der aktiven Montage.
    store: Arc<dyn StateStore>,
    /// Modellanbieter der aktiven Montage.
    model: Arc<dyn ModelProvider>,
}

impl ResumableGateway {
    // Baut den Gateway für die Startsitzung.
    fn new(
        session: AgentSession,
        store: Arc<dyn StateStore>,
        model: Arc<dyn ModelProvider>,
    ) -> Self {
        Self {
            session,
            store,
            model,
        }
    }

    // Ersetzt Sitzung, Speicher und Modell gemeinsam (neue Montage).
    fn replace(
        &mut self,
        session: AgentSession,
        store: Arc<dyn StateStore>,
        model: Arc<dyn ModelProvider>,
    ) {
        self.session = session;
        self.store = store;
        self.model = model;
    }
}

impl crate::gateway::ChatGateway for ResumableGateway {
    fn session_mut(&mut self) -> &mut AgentSession {
        &mut self.session
    }

    fn store(&self) -> &dyn StateStore {
        self.store.as_ref()
    }

    fn model(&self) -> &dyn ModelProvider {
        self.model.as_ref()
    }

    fn borrow_turn_ctx(&mut self) -> (&mut AgentSession, &dyn StateStore, &dyn ModelProvider) {
        (&mut self.session, self.store.as_ref(), self.model.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use harw_core::InteractionMode;
    use harw_runtime::{
        EntryKind, ModelSource, RuntimeError, RuntimeSpec, RuntimeStores, ServiceSurface,
    };

    /// Temp-Home und Temp-Projekt ohne Prozess-Zustand (kein `set_var`,
    /// kein `set_current_dir`) — Muster aus `harw-runtime/src/assembly.rs`.
    struct Fixture {
        _dir: tempfile::TempDir,
        home: std::path::PathBuf,
        project: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        // Projekt-Marker, damit die Projekterkennung genau hier stehen bleibt.
        std::fs::write(project.join("Cargo.toml"), "[workspace]\n").expect("marker");
        Fixture {
            _dir: dir,
            home,
            project,
        }
    }

    /// Builder einer Echo-Montage mit Einstieg `Tui`.
    fn tui_builder(
        fixture: &Fixture,
        mode_override: Option<InteractionMode>,
    ) -> RuntimeAssemblyBuilder {
        let spec = RuntimeSpec {
            entry: EntryKind::Tui,
            home: fixture.home.clone(),
            cwd: fixture.project.clone(),
            principal: harw_types::Principal::trusted_ingress(
                harw_types::PrincipalKind::Human,
                "t1",
                harw_types::IngressSurface::Tui,
                harw_types::PermissionTier::Operator,
            ),
            mode_override,
            active_agent: None,
            reasoning_effort: None,
        };
        let state_store: Arc<dyn StateStore> = Arc::new(harw_core::InMemoryStateStore::new());
        RuntimeAssembly::builder(spec)
            .model(ModelSource::Echo("echo: t1".to_owned()))
            .stores(RuntimeStores {
                state_store,
                job_store: None,
                approval_store: None,
            })
    }

    fn tui_assembly(
        fixture: &Fixture,
        mode_override: Option<InteractionMode>,
    ) -> (Arc<RuntimeAssembly>, TuiSessionWiring) {
        let wiring = TuiSessionWiring::new();
        let assembly = wiring
            .install(tui_builder(fixture, mode_override))
            .build()
            .expect("tui echo assembly builds with wiring installed");
        (Arc::new(assembly), wiring)
    }

    #[test]
    fn test_tui_session_wiring_install_sets_controller_and_events() {
        let fixture = fixture();

        // Ohne Verdrahtung fehlt der Ereigniskanal, den `BuiltinRoles` braucht.
        let bare = tui_builder(&fixture, None).build();
        assert!(
            matches!(bare, Err(RuntimeError::Spawner { .. })),
            "a Tui assembly without session events must fail closed"
        );

        let (assembly, wiring) = tui_assembly(&fixture, None);
        let services = assembly.services().service_map(ServiceSurface::Slash);
        let installed = services
            .get::<SharedSessionController>()
            .expect("the slash surface carries the installed controller");
        assert!(std::ptr::addr_eq(
            Arc::as_ptr(installed),
            Arc::as_ptr(&wiring.controller)
        ));
        assert!(!wiring.events_tx.is_closed());
    }

    #[test]
    fn test_build_root_runtime_mounts_responder_in_chain() {
        let fixture = fixture();
        let (assembly, wiring) = tui_assembly(&fixture, None);
        let chain_before = assembly.rights_snapshot().approval_chain;

        let runtime = build_root_runtime(&assembly, wiring).expect("root runtime builds");

        let chain_after = assembly.rights_snapshot().approval_chain;
        assert_eq!(chain_after.len(), chain_before.len() + 1);
        let handler = runtime.approval_driver.handler();
        assert_eq!(chain_after.last(), Some(&(handler.label(), handler.kind())));
        // Bedingung 2: derselbe Handler in Sitzungs-Registry und Treiber.
        let mounted = runtime
            .session
            .registry()
            .approval_handlers()
            .iter()
            .any(|registered| std::ptr::addr_eq(Arc::as_ptr(registered), Arc::as_ptr(handler)));
        assert!(
            mounted,
            "the driver's handler must be mounted in the session registry"
        );
        assert_eq!(runtime.session.id(), assembly.root_session_id());
        assert_eq!(runtime.app.session_id(), assembly.root_session_id());
    }

    #[test]
    fn test_build_root_runtime_applies_mode_override() {
        let fixture = fixture();
        let (assembly, wiring) = tui_assembly(&fixture, Some(InteractionMode::Plan));

        let runtime = build_root_runtime(&assembly, wiring).expect("root runtime builds");

        assert_eq!(runtime.session.mode(), InteractionMode::Plan);
        assert_eq!(runtime.app.active_mode(), InteractionMode::Plan);
    }
}
