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
//! - Schritt 7 (Session-Titel und Resume-Picker): `/resume` ohne Selektor
//!   öffnet den Session-Picker statt einer Textliste (`session_entries`
//!   reichert die vom Selektor gelieferten IDs über
//!   [`harw_session_store::meta::load_or_derive`] an), das Fortsetzen einer
//!   Sitzung aktualisiert ihren Metadaten-Sidecar
//!   ([`harw_session_store::meta::touch_opened`]), und [`build_root_runtime`]
//!   stellt den Kontext für die (noch in `crate::app` zu verdrahtende)
//!   Titel-Job-Anstoßung zusammen ([`TitleJobContext`]).
//!
//! Der Renderer selbst (`run_loop`, Zellen, Tastatur) bleibt in
//! [`crate::app`].
//!
//! ## Schlüsseltypen
//! - [`TuiSessionWiring`] — Controller + Sitzungs-Ereigniskanal einer Montage.
//! - [`TuiAssemblyFactory`] — baut für `/resume <id>` eine neue Montage.
//! - [`TuiResume`] — Auswahl dauerhafter Sitzungen plus Fabrik plus
//!   Session-Store-Wurzel (Schritt 7).
//! - [`TitleJobContext`] — Zutaten für die Titel-Job-Anstoßung (Schritt 7).
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
//! run_tui(assembly, TuiRunOptions { wiring, resume: None, verbose_tools: false })?;
//! # Ok(())
//! # }
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use ratatui::text::Line;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use time::OffsetDateTime;

use harw_core::{AgentSession, ModelProvider, StateStore};
use harw_extension_api::ApprovalHandler;
use harw_operations::SharedSessionController;
use harw_operations::adapter::CommandAdapter;
use harw_operations::session_control::UiaSelection;
use harw_protocol::events::{SessionEvent, TurnEvent};
use harw_runtime::{
    RootSession, RuntimeAssembly, RuntimeAssemblyBuilder, ServiceSurface,
};
use harw_session_store::meta::{self, SessionMeta};
use harw_types::{Clock, SessionId, SystemClock};

use crate::app::{
    ChatApp, ResumeSessionSelector, TerminalGuard, TuiError, TuiPlanServices, TuiRunOutcome,
    frame_scheduler, install_loaded_history, run_loop,
};
use crate::approval::{ApprovalDriver, ApprovalPromptReceiver, TuiApprovalHandler};
use crate::events::harw_event_channel;
use crate::frame_requester::frame_channel;
use crate::host_permit_dialog::HostPermitPromptReceiver;
use crate::input_reader::spawn_input_reader;
use crate::session_controller::TuiSessionController;
use crate::session_picker::SessionEntry;
use crate::tui_event::TuiEvent;

/// Erzeugt die einmalige Begrüßung einer TUI-Sitzung aus lokalem Kontext.
///
/// Formatiert Provider- und Modell-Info für die Begrüßungszeile.
///
/// Bevorzugt die effektive UIA-Auswahl und fällt je Achse auf
/// `default_model`/`default_provider` aus der Konfiguration zurück.
fn provider_model_info(config: &harw_config::ResolvedConfig) -> Option<String> {
    let selection = uia_selection_from_config(config);
    let model = selection.model().filter(|m| !m.is_empty());
    let provider = selection.provider().filter(|p| !p.is_empty());
    match (provider, model) {
        (Some(p), Some(m)) => Some(format!("Provider: {p} · Modell: {m}")),
        (Some(p), None) => Some(format!("Provider: {p}")),
        (None, Some(m)) => Some(format!("Modell: {m}")),
        (None, None) => None,
    }
}

/// Liest die effektive UIA-Provider-/Modell-Auswahl einer Assembly-Config.
///
/// Die UIA-spezifischen Persistenzwerte haben Vorrang; `default_*` dienen nur
/// als Achsen-Fallback und werden dabei nicht verändert.
fn uia_selection_from_config(config: &harw_config::ResolvedConfig) -> UiaSelection {
    UiaSelection::from_config(
        config.harness.uia_provider.as_deref(),
        config.harness.uia_model.as_deref(),
        config.harness.default_provider.as_deref(),
        config.harness.default_model.as_deref(),
    )
}

/// Sie ist reine Anzeige und wird nie als Nutzereingabe oder persistierte
/// Conversation-History behandelt. Die Uhrzeit wird explizit als UTC markiert,
/// damit die Ausgabe auch ohne verfügbare lokale Zeitzonendaten eindeutig bleibt.
fn tui_greeting(
    project_root: &str,
    uia_definition: Option<&str>,
    uia_user_name: Option<&str>,
    provider_info: Option<&str>,
) -> String {
    // Der in USER.md freiwillig hinterlegte Name gehört zum UIA-Kontext und
    // gewinnt deshalb vor dem technischen Login-Namen. Fehlt er, bleibt die
    // Begrüßung auch für ältere oder unpersonalisierte UIAs funktionsfähig.
    let user = uia_user_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("USER")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "da".to_owned());
    tui_greeting_at(project_root, uia_definition, &user, provider_info, OffsetDateTime::now_utc())
}

/// Liest den optionalen Anzeigenamen der aktiven UIA aus ihrer `USER.md`.
///
/// Die Runtime hat dieselbe Datei bereits als UIA-Personalisierung validiert.
/// Ein fehlender Name ist kein Fehler: Dann verwendet [`tui_greeting`] den
/// Login-Namen als Rückfall. Ein Leseproblem wird ebenfalls nicht in der
/// Anzeige eskaliert, weil eine Begrüßung die gestartete Sitzung nicht
/// unbenutzbar machen darf.
fn active_uia_user_name(assembly: &RuntimeAssembly) -> Option<String> {
    let config = assembly.config();
    let definition = config.harness.active_uia_definition.as_deref()?;
    let agent_dir = config.agent_definition_dirs.get(definition)?;
    harw_config::load_uia_user_name(agent_dir).ok().flatten()
}

/// Deterministischer Kern der TUI-Begrüßung; getrennt für die Tests.
fn tui_greeting_at(
    project_root: &str,
    uia_definition: Option<&str>,
    user: &str,
    provider_info: Option<&str>,
    now: OffsetDateTime,
) -> String {
    let salutation = match now.hour() {
        5..=11 => "Guten Morgen",
        12..=17 => "Guten Tag",
        18..=22 => "Guten Abend",
        _ => "Gute Nacht",
    };
    let workspace = Path::new(project_root)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("dieser Workspace");
    let uia = uia_definition
        .and_then(|definition| definition.rsplit('.').next())
        .filter(|name| !name.is_empty())
        .unwrap_or("Harw");
    let model_line = provider_info
        .filter(|info| !info.is_empty())
        .map(|info| format!(" · {info}"))
        .unwrap_or_default();
    format!(
        "{uia}: {salutation}, {user}! Bereit für »{workspace}«{model_line} — {} {:02}:{:02} UTC. Womit beginnen wir?",
        now.date(),
        now.hour(),
        now.minute(),
    )
}

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
///
/// # Annahme (Schritt 7)
/// [`ResumeSessionSelector`] (definiert in `crate::app`, gehört nicht zu
/// diesem Slice) liefert weiterhin nur `Vec<SessionId>` /
/// `Result<SessionId, String>` — die reichhaltigere Anzeige des Session-
/// Pickers (Titel, letzte Aktivität, Projekt, Turns) wird **hier lokal**
/// nachgerüstet, über [`harw_session_store::meta::load_or_derive`]
/// ([`session_entries`]). Dafür braucht dieser Typ zusätzlich die Wurzel des
/// Session-Stores, in der die `<id>.meta.json`-Sidecars liegen — dieselbe
/// Wurzel, die die Composition-Root (`harw-cli/src/chat.rs`) auch dem
/// Transcript-Speicher und `ProfileResumeSelector` übergibt. **Fremde
/// Anpassung nötig**: `harw-cli/src/chat.rs` muss beim Bau von `TuiResume`
/// zusätzlich `session_store_root: sessions_root.clone()` mitgeben (aktuell
/// wird `sessions_root` unverändert in `ProfileResumeSelector::new`
/// verschoben).
pub struct TuiResume {
    /// Listet und löst dauerhafte Sitzungen auf.
    pub selector: Box<dyn ResumeSessionSelector>,
    /// Montiert die gewählte Sitzung.
    pub factory: Box<dyn TuiAssemblyFactory>,
    /// Wurzelverzeichnis des Session-Stores (Transcripts und
    /// `.meta.json`-Sidecars), für [`session_entries`] und
    /// [`harw_session_store::meta::touch_opened`].
    pub session_store_root: PathBuf,
    /// Ob beim Start sofort der Session-Picker erscheinen soll.
    ///
    /// `harw -r` ohne Selektor setzt das: die CLI startet dann eine frische
    /// Sitzung, und die Auswahl übernimmt der Picker. Ohne dieses Feld bliebe
    /// `-r` wirkungslos, weil der Picker sonst nur über `/resume` in der
    /// laufenden Sitzung erscheint.
    pub open_picker_at_start: bool,
}

/// Handgeschriebenes `Debug`: die Trait-Objekte leiten kein `Debug` ab.
impl std::fmt::Debug for TuiResume {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TuiResume")
            .field("selector", &"<dyn ResumeSessionSelector>")
            .field("factory", &"<dyn TuiAssemblyFactory>")
            .field("session_store_root", &self.session_store_root)
            .field("open_picker_at_start", &self.open_picker_at_start)
            .finish()
    }
}

/// Kontext für die (noch ausstehende) Titel-Job-Anstoßung nach dem ersten
/// abgeschlossenen Turn einer neuen Sitzung (Schritt 7).
///
/// # Annahme
/// `harw_runtime::session_title::spawn_title_job` existiert zum Zeitpunkt
/// dieses Slices noch nicht (paralleler Slice `harw-runtime/src/session_title.rs`,
/// Welle W1 „Schritt 7 Store/Titel"). Erwartete Signatur, dokumentiert für die
/// Gegenseite:
///
/// ```ignore
/// pub fn spawn_title_job(
///     provider: std::sync::Arc<dyn harw_core::ModelProvider>,
///     session_store_root: std::path::PathBuf,
///     session_id: harw_types::SessionId,
///     first_user_message: String,
///     assistant_reply_start: String,
///     title_model: Option<String>,
/// );
/// ```
///
/// Der eigentliche Aufruf gehört an die Stelle, an der
/// `SessionEvent::TurnCompleted` in `crate::app::run_loop` behandelt wird
/// (`harw-tui/src/app.rs:~1279`) — diese Datei gehört nicht zu diesem Slice
/// (B3b, nur `runtime_root.rs`). [`build_root_runtime`] stellt hier nur
/// zusammen, was für diesen (noch zu ergänzenden) Aufruf gebraucht wird, und
/// reicht es über die ebenfalls noch zu ergänzende
/// `ChatApp::with_title_job_context(TitleJobContext) -> ChatApp` an den
/// Renderer-Zustand weiter. `run_loop` müsste dort beim ersten
/// `SessionEvent::TurnCompleted` einer Sitzung ohne vorhandenen Titel (siehe
/// `harw_session_store::meta::SessionMeta::title_source`) `spawn_title_job`
/// mit der ersten Nutzernachricht und dem Anfang der ersten Modellantwort
/// aufrufen und den Kontext danach verwerfen (nur einmal je Sitzung).
///
/// Die Bedingung „nur wenn ein Provider verfügbar ist" ist in dieser
/// Laufzeit-Architektur strukturell immer erfüllt: [`RuntimeAssembly::model`]
/// liefert stets einen `Arc<dyn ModelProvider>` (auch der Offline-Echo-Modus
/// zählt als „verfügbar"). [`build_root_runtime`] gattert stattdessen nur auf
/// die Konfiguration (`title_generation`) und darauf, ob überhaupt eine
/// Session-Store-Wurzel bekannt ist (kein `TuiResume` → keine Wurzel → kein
/// Titel-Job, da nirgends ein Sidecar geschrieben werden könnte).
#[derive(Clone)]
pub struct TitleJobContext {
    /// Modell-Anbieter der (neuen) Wurzelsitzung.
    pub provider: Arc<dyn ModelProvider>,
    /// Wurzelverzeichnis des Session-Stores für den Sidecar-Schreibzugriff.
    pub session_store_root: PathBuf,
    /// Konfiguriertes Titel-Modell (`[session] title_model`), falls gesetzt.
    pub title_model: Option<String>,
    /// Aufgelöste Konfiguration der Montage, aus der `run_loop` die interne
    /// Modellstelle [`harw_config::InternalModelPoint::SessionTitle`] auflöst
    /// (Addendum C) und `harw_runtime::session_title::title_model_selection`
    /// aufruft, um `spawn_title_job` deren `pin`-Argument zu geben.
    pub config: Arc<harw_config::ResolvedConfig>,
}

/// Handgeschriebenes `Debug`: `provider` ist ein Trait-Objekt.
impl std::fmt::Debug for TitleJobContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TitleJobContext")
            .field("provider", &"<dyn ModelProvider>")
            .field("session_store_root", &self.session_store_root)
            .field("title_model", &self.title_model)
            .field("config", &"<ResolvedConfig>")
            .finish()
    }
}

/// Eingaben von [`run_tui`] neben der Montage.
pub struct TuiRunOptions {
    /// Die Verdrahtung, die in den Builder der übergebenen Montage gelegt wurde.
    pub wiring: TuiSessionWiring,
    /// `/resume`-Unterstützung; `None` lehnt `/resume` mit Hinweis ab.
    pub resume: Option<TuiResume>,
    /// Ausführliche Werkzeugzellen; wird von `harw chat --verbose` gesetzt
    /// und beim anschließenden Fortsetzen einer Sitzung beibehalten.
    pub verbose_tools: bool,
}

/// Handgeschriebenes `Debug`: [`TuiSessionWiring`] enthält Kanäle.
impl std::fmt::Debug for TuiRunOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TuiRunOptions")
            .field("wiring", &self.wiring)
            .field("resume", &self.resume)
            .field("verbose_tools", &self.verbose_tools)
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
/// Systemzeile. Scheitert das Laden des Verlaufs der Startsitzung (Schritt 3)
/// oder das Betreten des Terminals (Schritt 4), ist die Wurzelsitzung bereits
/// erzeugt; sie wird vor der Rückgabe von `Err` ebenfalls per
/// [`RuntimeAssembly::close_session`] geschlossen. Beim Verlassen der
/// Schleife wird die aktive Sitzung geschlossen.
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
/// - [`TuiError::Core`]: `options.wiring` gehört nicht zu `assembly`,
///   Wurzelsitzung nicht montierbar, Verlauf der Startsitzung nicht ladbar
///   oder Fehler im Turn-Loop.
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
/// harw_tui::run_tui(assembly, harw_tui::TuiRunOptions { wiring, resume: None, verbose_tools: false })
/// # }
/// ```
pub fn run_tui(assembly: Arc<RuntimeAssembly>, options: TuiRunOptions) -> Result<(), TuiError> {
    let TuiRunOptions {
        wiring,
        resume,
        verbose_tools,
    } = options;
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
        mut host_permit_prompts,
    } = build_root_runtime(
        &assembly,
        wiring,
        resume.as_ref().map(|r| r.session_store_root.as_path()),
        verbose_tools,
    )?;
    let history = match runtime.block_on(assembly.state_store().load_history(session.id())) {
        Ok(history) => history,
        Err(error) => {
            // Die Wurzelsitzung wurde bereits erzeugt (`build_root_runtime`);
            // ihr Ende wird gemeldet, bevor der Fehler propagiert.
            assembly.close_session(assembly.root_session_id());
            return Err(TuiError::Core(format!("durable history load failed: {error}")));
        }
    };
    install_loaded_history(&mut session, &mut app, history);
    let uia_user_name = active_uia_user_name(&assembly);
    app.push_lines(vec![Line::from(tui_greeting(
        app.project_root(),
        assembly.config().harness.active_uia_definition.as_deref(),
        uia_user_name.as_deref(),
        provider_model_info(assembly.config()).as_deref(),
    ))]);
    let mut gateway = ResumableGateway::new(
        session,
        Arc::clone(assembly.state_store()),
        Arc::clone(assembly.model()),
    );
    let mut current = assembly;
    let mut guard = match TerminalGuard::enter() {
        Ok(guard) => guard,
        Err(error) => {
            // Die Wurzelsitzung wurde bereits erzeugt; ihr Ende wird gemeldet,
            // bevor der Terminal-Setup-Fehler propagiert.
            current.close_session(current.root_session_id());
            return Err(error);
        }
    };

    // `harw -r` ohne Selektor: die Auswahl gehört vor die erste Eingabe, nicht
    // hinter ein getipptes `/resume`. Schlägt das Auflisten fehl, startet die
    // frische Sitzung trotzdem — mit einer Meldung statt eines Abbruchs.
    if let Some(resume_options) = resume.as_ref()
        && resume_options.open_picker_at_start
    {
        match resume_options.selector.available_sessions() {
            Ok(ids) => {
                let entries = session_entries(&resume_options.session_store_root, ids);
                app.open_session_picker(entries);
            }
            Err(error) => push_system_text(
                &mut app,
                &format!("Could not list resumable sessions: {error}"),
            ),
        }
    }

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
                &mut host_permit_prompts,
            )
            .await?
            {
                TuiRunOutcome::Quit => return Ok(()),
                TuiRunOutcome::Resume { selector: None } => {
                    match resume.as_ref() {
                        Some(resume) => match resume.selector.available_sessions() {
                            Ok(ids) => {
                                let entries = session_entries(&resume.session_store_root, ids);
                                // `ChatApp::open_session_picker` ist eine noch
                                // zu ergänzende Erwartung an `crate::app`
                                // (paralleler Slice B-app); der Aufruf steht
                                // schon hier, damit die Verdrahtung feststeht.
                                app.open_session_picker(entries);
                            }
                            Err(error) => push_system_text(
                                &mut app,
                                &format!("Could not list resumable sessions: {error}"),
                            ),
                        },
                        None => push_system_text(&mut app, RESUME_NOT_CONFIGURED),
                    }
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
                    match resume_session(resume, &raw_selector, verbose_tools).await {
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
                            host_permit_prompts = next_runtime.host_permit_prompts;
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
    /// Empfängerseite des Host-Permit-Fragekanals dieser Montage (siehe
    /// [`harw_runtime::RuntimeAssembly::take_host_permit_prompts`]). Wer sie
    /// sendet ist `harw_tool_shell::exec::ShellExecutor::authorize_host_command`
    /// — muss ebenso gepollt werden wie `approvals`, sonst läuft jede
    /// Host-Permit-Frage in den Timeout und gilt als Ablehnung (fail-closed,
    /// keine Sonderbehandlung).
    host_permit_prompts: HostPermitPromptReceiver,
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
//
// `session_store_root` ist `None`, wenn `/resume` in dieser Laufzeit gar
// nicht konfiguriert ist (`TuiRunOptions::resume`); in dem Fall gibt es
// keine bekannte Sidecar-Wurzel und der Titel-Job-Kontext (Schritt 7,
// [`TitleJobContext`]) bleibt unbestückt.
fn build_root_runtime(
    assembly: &Arc<RuntimeAssembly>,
    wiring: TuiSessionWiring,
    session_store_root: Option<&Path>,
    verbose_tools: bool,
) -> Result<RootRuntime, TuiError> {
    let TuiSessionWiring {
        controller,
        events_tx,
        events_rx,
    } = wiring;
    // Die Verdrahtung muss aus derselben Montage stammen: `install` legte den
    // Controller als `SharedSessionController` in die Slash-`ServiceMap`, ehe
    // die Montage gebaut wurde (E1.2). Eine fremde Verdrahtung trüge einen
    // anderen `Arc`, dessen Ereignisse nirgends in dieser Montage ankommen.
    let slash_services = assembly.services().service_map(ServiceSurface::Slash);
    let belongs_to_assembly = slash_services
        .get::<SharedSessionController>()
        .is_some_and(|installed| {
            std::ptr::addr_eq(Arc::as_ptr(installed), Arc::as_ptr(&controller))
        });
    if !belongs_to_assembly {
        return Err(TuiError::Core(
            "wiring does not belong to this assembly".to_owned(),
        ));
    }
    // Die Assembly hat ihre Config beim Bau aus der Persistenz geladen. Die
    // UIA-Auswahl wird deshalb hier pro Root-Montage neu gesetzt — sowohl beim
    // frischen Start als auch bei `/resume` — ohne `default_*` umzudeuten.
    let uia_selection = uia_selection_from_config(assembly.config());
    controller
        .initialize_uia_selection(uia_selection)
        .map_err(|error| {
            TuiError::Core(format!(
                "could not initialize the UIA session selection: {error}"
            ))
        })?;
    // AP W5-03, Bedingung 2: genau ein Handler, dessen `Arc` gleichzeitig in
    // der Freigabekette der Sitzung und im `ApprovalDriver` liegt.
    let (approval_handler, approvals) = TuiApprovalHandler::new();
    let approval_driver = ApprovalDriver::new(Arc::clone(&approval_handler));
    // Die Empfängerseite gehört zu genau dieser Montage: der Produzent
    // (`harw_tool_shell::exec::ShellExecutor::authorize_host_command`) sendet
    // über `assembly.host_permit_prompt_sender()`; take-once liefert hier den
    // einzigen Empfänger dieses Laufs. Ein zweiter Aufruf auf derselben
    // Montage schlägt fehl statt still einen zweiten, nie gepollten Kanal zu
    // erzeugen (siehe [`RuntimeAssembly::take_host_permit_prompts`]).
    let host_permit_prompts = assembly.take_host_permit_prompts().map_err(|error| {
        TuiError::Core(format!(
            "could not take the host permit prompt receiver: {error}"
        ))
    })?;
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
    .with_verbose_tools(verbose_tools)
    .with_session_controller(controller)
    .with_project_root(assembly.project().project_root.display().to_string())
    .with_managed_spawner(assembly.spawner().cloned());
    if let Some(plan) = assembly.plan_services() {
        app = app.with_plan_services(TuiPlanServices::from(plan));
    }
    // Schritt 7: Titel-Job-Kontext nur bestücken, wenn eine Sidecar-Wurzel
    // bekannt ist (`/resume` konfiguriert) und die Konfiguration die
    // Titelerzeugung nicht abgeschaltet hat. `ChatApp::with_title_job_context`
    // ist eine noch zu ergänzende Erwartung an `crate::app` (siehe
    // [`TitleJobContext`]-Dokumentation).
    if let Some(store_root) = session_store_root {
        let session_config = &assembly.config().harness.session;
        if session_config.title_generation {
            app = app.with_title_job_context(TitleJobContext {
                provider: Arc::clone(assembly.model()),
                session_store_root: store_root.to_path_buf(),
                title_model: session_config.title_model.clone(),
                config: Arc::clone(assembly.config()),
            });
        }
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
        host_permit_prompts,
    })
}

// Löst `/resume <selector>` auf und montiert die gewählte Sitzung vollständig
// (Montage, Wurzelsitzung, Verlauf, Willkommenszeile). Jeder Fehler wird zu
// einer Systemzeile; die laufende Sitzung bleibt dann unberührt.
async fn resume_session(
    resume: &TuiResume,
    raw_selector: &str,
    verbose_tools: bool,
) -> Result<ResumedRuntime, String> {
    let selected = resume
        .selector
        .resolve_session(raw_selector)
        .map_err(|error| format!("Could not resolve session: {error}"))?;
    // Schritt 7: `last_opened_at` beim Fortsetzen aktualisieren. Ein Fehler
    // hier darf das Fortsetzen selbst nicht verhindern (best effort, nur
    // gemeldet) — die Sitzung bleibt auch ohne aktualisierten Sidecar nutzbar.
    if let Err(error) = meta::touch_opened(&resume.session_store_root, &selected, SystemClock.now())
    {
        tracing::warn!(
            session = %selected,
            error = %error,
            "tui.resume.touch_opened_failed"
        );
    }
    let (assembly, wiring) = resume
        .factory
        .assemble(Some(selected))
        .map_err(|error| format!("Could not assemble session: {error}"))?;
    let mut runtime =
        build_root_runtime(
            &assembly,
            wiring,
            Some(resume.session_store_root.as_path()),
            verbose_tools,
        )
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
    let uia_user_name = active_uia_user_name(&assembly);
    runtime.app.push_lines(vec![Line::from(tui_greeting(
        runtime.app.project_root(),
        assembly.config().harness.active_uia_definition.as_deref(),
        uia_user_name.as_deref(),
        provider_model_info(assembly.config()).as_deref(),
    ))]);
    Ok(ResumedRuntime { assembly, runtime })
}

// Baut Picker-Einträge aus Sitzungs-IDs (Schritt 7). Nutzt
// `harw_session_store::meta::load_or_derive`, um `ResumeSessionSelector`
// (siehe `TuiResume`-Dokumentation) nicht um Anzeigefelder erweitern zu
// müssen — dieser Slice besitzt `crate::app` nicht.
//
// Eine Sitzung, deren Sidecar weder geladen noch aus dem Transcript
// abgeleitet werden kann (z. B. defektes Transcript), wird übersprungen und
// nur mit `tracing::warn!` gemeldet: ein einzelner kaputter Eintrag darf den
// gesamten Picker nicht leeren. Eine leere `ids`-Liste ergibt eine leere
// Ergebnisliste — der Picker selbst zeigt dafür seinen Leerzustand
// ("Keine Sessions gefunden", `session_picker.rs`).
fn session_entries(session_store_root: &Path, ids: Vec<SessionId>) -> Vec<SessionEntry> {
    ids.into_iter()
        .filter_map(|id| match meta::load_or_derive(session_store_root, &id) {
            Ok(session_meta) => Some(session_entry_from_meta(id, &session_meta)),
            Err(error) => {
                tracing::warn!(
                    session = %id,
                    error = %error,
                    "tui.resume.session_meta_failed"
                );
                None
            }
        })
        .collect()
}

// Übersetzt einen geladenen/abgeleiteten `SessionMeta` in einen
// Picker-Eintrag.
//
// # Annahmen
// - `title`: [`SessionMeta::display_title`] ist nie leer (gesetzter Titel,
//   sonst aus der ersten Nutzernachricht abgeleitet, sonst Platzhalter) —
//   genau das erwartet [`SessionEntry::title`].
// - `project_label`: bevorzugt `project_root`, fällt auf `cwd` zurück, da
//   der Picker nur ein einzelnes Label anzeigt und beide Felder optional
//   sind (ältere, abgeleitete Sitzungen kennen keins von beidem).
// - `turns`: nur gesetzt, wenn mindestens ein Turn abgeschlossen wurde
//   (`meta.turns > 0`); eine druckfrische Sitzung zeigt sonst irreführend
//   "0 Turns" statt gar keine Turn-Angabe.
fn session_entry_from_meta(id: SessionId, session_meta: &SessionMeta) -> SessionEntry {
    let project_label = session_meta
        .project_root
        .as_deref()
        .or(session_meta.cwd.as_deref())
        .map(|path| path.display().to_string());
    SessionEntry {
        id: id.as_str().to_owned(),
        title: session_meta.display_title(),
        last_active: SystemTime::from(session_meta.last_opened_at),
        project_label,
        turns: (session_meta.turns > 0).then_some(session_meta.turns),
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
    use harw_operations::SessionController;
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
        write_fixture_uia(&home);
        Fixture {
            _dir: dir,
            home,
            project,
        }
    }

    /// Legt eine minimale, gültige UIA (`role = "user-interface"`) im
    /// Standardprofil des Test-`home` an und aktiviert sie über
    /// `harness.active_uia_definition`.
    ///
    /// # Beschreibung
    /// Seit dem UIA-Vertrag (siehe `resolve_active_uia` in
    /// `harw-runtime/src/assembly.rs`) montieren `EntryKind::Tui` und
    /// `EntryKind::OneShot` nur mit einer konfigurierten UIA (fail-closed,
    /// `RuntimeError::Registry`). `fixture()` erzeugt `home` hier ohne
    /// `harw_home::ensure_home` (kein vorbestehendes Profil-`config.toml`),
    /// daher genügt ein frisches `std::fs::write` — anders als in
    /// `harw-cli/src/chat.rs`, wo ein bereits gescaffoldetes
    /// Profil-`config.toml` mit offener `[mcp_listener]`-Tabelle nicht ans
    /// Ende angehängt werden darf. Layout spiegelt exakt
    /// `harw-runtime/src/assembly.rs`s eigenes `write_fixture_uia`.
    fn write_fixture_uia(home: &std::path::Path) {
        let profile_dir = home.join("profiles").join("default");
        let agent_dir = profile_dir.join("agents").join("fixture-uia");
        std::fs::create_dir_all(&agent_dir).expect("fixture uia dir");
        std::fs::write(
            agent_dir.join("definition.toml"),
            "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
        )
        .expect("fixture uia definition");
        std::fs::write(
            profile_dir.join("config.toml"),
            "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
        )
        .expect("fixture profile config");
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
        let controller = Arc::clone(&wiring.controller);
        let chain_before = assembly.rights_snapshot().approval_chain;

        let runtime = build_root_runtime(&assembly, wiring, None, false).expect("root runtime builds");

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
        assert_eq!(
            controller.uia_selection(),
            uia_selection_from_config(assembly.config()),
            "the root controller must receive the effective persisted UIA selection"
        );
    }

    #[test]
    fn uia_selection_from_config_prefers_uia_values_per_axis() {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("default-provider".to_owned());
        config.harness.default_model = Some("default-model".to_owned());
        config.harness.uia_provider = Some("uia-provider".to_owned());

        assert_eq!(
            uia_selection_from_config(&config),
            UiaSelection::new(
                Some("uia-provider".to_owned()),
                Some("default-model".to_owned())
            )
        );
    }

    #[test]
    fn test_build_root_runtime_applies_mode_override() {
        let fixture = fixture();
        let (assembly, wiring) = tui_assembly(&fixture, Some(InteractionMode::Plan));

        let runtime = build_root_runtime(&assembly, wiring, None, false).expect("root runtime builds");

        assert_eq!(runtime.session.mode(), InteractionMode::Plan);
        assert_eq!(runtime.app.active_mode(), InteractionMode::Plan);
    }

    #[test]
    fn test_build_root_runtime_rejects_foreign_wiring() {
        let fixture = fixture();
        let (assembly_a, _wiring_a) = tui_assembly(&fixture, None);
        let (_assembly_b, wiring_b) = tui_assembly(&fixture, None);

        // `wiring_b` wurde in die Slash-`ServiceMap` von `assembly_b` gelegt,
        // nicht in die von `assembly_a`; der Bau muss fail-closed ablehnen,
        // statt eine Verdrahtung zu verwenden, deren Ereignisse nirgends in
        // `assembly_a` ankommen.
        let result = build_root_runtime(&assembly_a, wiring_b, None, false);

        match result {
            Ok(_) => panic!("expected TuiError::Core for foreign wiring, got Ok"),
            Err(TuiError::Core(message)) => {
                assert!(
                    message.contains("does not belong"),
                    "unexpected error message: {message}"
                );
            }
            Err(other) => panic!("expected TuiError::Core for foreign wiring, got {other:?}"),
        }
    }

    use harw_session_store::TitleSource;

    fn meta_with(
        id: &str,
        title: Option<&str>,
        turns: u64,
        project_root: Option<&str>,
        cwd: Option<&str>,
    ) -> SessionMeta {
        SessionMeta {
            version: harw_session_store::SESSION_META_VERSION,
            session_id: SessionId::from_str(id),
            title: title.map(str::to_owned),
            title_source: if title.is_some() {
                TitleSource::Manual
            } else {
                TitleSource::None
            },
            created_at: SystemClock.now(),
            last_opened_at: SystemClock.now(),
            cwd: cwd.map(PathBuf::from),
            project_root: project_root.map(PathBuf::from),
            project_key: None,
            first_user_message: None,
            turns,
            usage_rounds: 0,
            total_usage: harw_types::TokenUsage::default(),
            drift_events: std::collections::BTreeMap::new(),
        }
    }

    /// [`session_entry_from_meta`] übernimmt Titel und `last_opened_at`,
    /// bevorzugt `project_root` vor `cwd` fürs Label und blendet `turns == 0`
    /// als `None` aus.
    #[test]
    fn test_session_entry_from_meta_maps_fields_and_hides_zero_turns() {
        let meta = meta_with(
            "session-a",
            Some("Mein Titel"),
            0,
            Some("/home/mia/projects/harwness"),
            Some("/home/mia/projects/harwness/sub"),
        );

        let entry = session_entry_from_meta(SessionId::from_str("session-a"), &meta);

        assert_eq!(entry.id, "session-a");
        assert_eq!(entry.title, "Mein Titel");
        assert_eq!(entry.last_active, SystemTime::from(meta.last_opened_at));
        assert_eq!(
            entry.project_label.as_deref(),
            Some("/home/mia/projects/harwness"),
            "project_root has priority over cwd"
        );
        assert_eq!(entry.turns, None, "zero completed turns must not be shown");
    }

    /// Fehlt `project_root`, fällt das Label auf `cwd` zurück; ein Titel aus
    /// `SessionMeta::display_title` (kein gesetzter Titel) wird übernommen.
    #[test]
    fn test_session_entry_from_meta_falls_back_to_cwd_and_derived_title() {
        let mut meta = meta_with("session-b", None, 3, None, Some("/tmp/project"));
        meta.first_user_message = Some("Erste Nachricht der Sitzung".to_owned());

        let entry = session_entry_from_meta(SessionId::from_str("session-b"), &meta);

        assert_eq!(entry.project_label.as_deref(), Some("/tmp/project"));
        assert_eq!(entry.title, "Erste Nachricht der Sitzung");
        assert_eq!(entry.turns, Some(3));
    }

    /// [`session_entries`] liest jeden Sidecar über `meta::load_or_derive` und
    /// baut daraus die Anzeige-Einträge des Pickers.
    #[test]
    fn test_session_entries_reads_saved_sidecars() {
        let temp = tempfile::tempdir().expect("tempdir");
        let meta_a = meta_with("session-a", Some("Alpha"), 5, None, None);
        let meta_b = meta_with("session-b", Some("Beta"), 0, None, None);
        meta::save(temp.path(), &meta_a).expect("save a");
        meta::save(temp.path(), &meta_b).expect("save b");

        let entries = session_entries(
            temp.path(),
            vec![SessionId::from_str("session-a"), SessionId::from_str("session-b")],
        );

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| entry.title == "Alpha" && entry.turns == Some(5)));
        assert!(entries.iter().any(|entry| entry.title == "Beta" && entry.turns.is_none()));
    }

    /// Ohne fortsetzbare Sitzungen liefert [`session_entries`] eine leere
    /// Liste (der Picker zeigt dafür selbst seinen Leerzustand) — Schritt 7,
    /// "Verhalten ohne Sessions".
    #[test]
    fn test_session_entries_of_empty_ids_is_empty() {
        let temp = tempfile::tempdir().expect("tempdir");

        let entries = session_entries(temp.path(), Vec::new());

        assert!(entries.is_empty());
    }

    /// Eine Sitzungs-ID, die nicht einmal adressierbar ist (z. B. durch einen
    /// Ableitungsfehler), wird übersprungen statt den gesamten Picker leer zu
    /// machen oder zu paniken.
    #[test]
    fn test_session_entries_skips_unaddressable_session_id() {
        let temp = tempfile::tempdir().expect("tempdir");

        let entries = session_entries(temp.path(), vec![SessionId::from_str("../escape")]);

        assert!(entries.is_empty());
    }
}

#[cfg(test)]
mod greeting_tests {
    use super::tui_greeting_at;
    use time::{Date, Month, PrimitiveDateTime, Time};

    #[test]
    fn greeting_uses_workspace_user_date_and_time() {
        let now = PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 14).unwrap(),
            Time::from_hms(19, 5, 0).unwrap(),
        )
        .assume_utc();
        let greeting = tui_greeting_at("/work/Harwness", Some("harwness.agent.emily-ui"), "Mia", None, now);
        assert!(greeting.starts_with("emily-ui: Guten Abend, Mia!"));
        assert!(greeting.contains("»Harwness«"));
        assert!(greeting.contains("2026-09-14 19:05 UTC"));
    }

    #[test]
    fn greeting_prefers_the_explicit_user_profile_name() {
        let now = PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 14).unwrap(),
            Time::from_hms(11, 4, 0).unwrap(),
        )
        .assume_utc();

        let greeting = tui_greeting_at(
            "/work/Harwness",
            Some("harwness.agent.terminal-ui@1"),
            "Mia",
            None,
            now,
        );

        assert!(greeting.starts_with("terminal-ui@1: Guten Morgen, Mia!"));
    }

    #[test]
    fn greeting_includes_provider_and_model_when_available() {
        let now = PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 14).unwrap(),
            Time::from_hms(10, 0, 0).unwrap(),
        )
        .assume_utc();
        let greeting = tui_greeting_at(
            "/work/Harwness",
            Some("harwness.agent.emily-ui"),
            "Mia",
            Some("Provider: anthropic · Modell: claude-sonnet"),
            now,
        );
        assert!(greeting.contains("Provider: anthropic · Modell: claude-sonnet"));
    }

    #[test]
    fn greeting_omits_provider_info_when_none() {
        let now = PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 14).unwrap(),
            Time::from_hms(10, 0, 0).unwrap(),
        )
        .assume_utc();
        let greeting = tui_greeting_at(
            "/work/Harwness",
            Some("harwness.agent.emily-ui"),
            "Mia",
            None,
            now,
        );
        assert!(!greeting.contains("Provider:"));
        assert!(!greeting.contains("Modell:"));
    }
}
