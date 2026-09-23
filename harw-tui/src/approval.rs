//! Freigabe- und Kind-Wiederaufnahme für die Harwness-TUI.
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt genau die Gegenseite zu den beiden Pausepunkten des
//! Kerns — [`TurnOutcome::AwaitingApproval`] und [`TurnOutcome::AwaitingChild`].
//! Es stellt die Frage an den Renderer, sammelt die Antwort ein und ruft
//! [`resume_after_approval`] bzw. [`resume_after_child`], bis der Turn
//! terminiert. Es besitzt **keinen** Renderer-Zustand, keine Terminal-I/O und
//! keine Session-Konstruktion; die TUI-Schleife übergibt Session, Modell und
//! Store bei jedem Aufruf.
//!
//! # Schlüsseltypen
//! - [`ApprovalPrompt`] — die Frage, die beim Renderer ankommt.
//! - [`ApprovalPromptReceiver`] — die Empfängerseite, die der Renderer pollt.
//! - [`TuiApprovalHandler`] — [`ApprovalHandler`]-Implementierung, die Fragen
//!   ausgibt und Antworten entgegennimmt.
//! - [`ApprovalScope`] — welche Werkzeuge dieser Handler von sich aus anhält.
//! - [`ChildTurnDriver`] — die Naht, über die ein Kind-Turn getrieben wird.
//! - [`ApprovalDriver`] — die **eine** Schleife über beide Pausearten.
//! - [`ApprovalDriverError`] — der Fehlertyp dieses Moduls.
//!
//! # Warum eine gemeinsame Schleife statt zweier getrennter
//! Die beiden Pausearten sind nicht disjunkt, sondern gegenseitig erreichbar:
//! ein freigegebener `transfer_to_*`-Aufruf erzeugt aus einer Freigabe ein
//! Kind ([`resume_after_approval`] spawnt es und liefert `AwaitingChild`), und
//! ein zurückgekehrtes Kind treibt den Eltern-Turn weiter, der unmittelbar
//! danach an einem `fs.write` erneut auf eine Freigabe warten kann. Zwei
//! getrennte Schleifen müssten sich deshalb gegenseitig aufrufen und hätten
//! zwei Abbruchgrenzen, zwei Fehlerpfade und zwei Stellen, an denen ein
//! `TurnOutcome` verloren gehen kann — genau der Fehler, den dieses Modul
//! behebt. [`ApprovalDriver::drive_to_completion`] hat deshalb **einen**
//! `match` über beide Ausgänge, **einen** Zähler und **einen** Rückweg.
//!
//! # Sicherheitsregel: Ablehnung ist der Default
//! Jeder Weg, auf dem keine ausdrückliche Freigabe eintrifft, ist eine
//! Ablehnung — geschlossener Fragekanal, fallengelassene Antwort, Zeitablauf,
//! vergifteter Mutex. Es gibt in diesem Modul keinen Pfad, auf dem eine
//! ausbleibende Antwort zu [`ApprovalResolution::Approve`] wird. Die Stellen,
//! die das entscheiden, sind [`TuiApprovalHandler::await_resolution`] und
//! [`TuiApprovalHandler::open_prompt`]; beide tragen den Hinweis erneut.
//!
//! # Vertrag: `review` ist seiteneffektfrei (CONTRACTS.md §extension, W0B-07)
//! [`ApprovalHandler::review`] **stellt keine Frage**. Es meldet nur, ob der
//! Aufruf eine Nutzerentscheidung braucht ([`ApprovalDecision::AskUser`]),
//! ob er ohne Entscheidung vorbeikommt (`Allow`) oder ob gar niemand mehr
//! fragen könnte (`Deny`, toter Fragekanal). Angezeigt wird ausschließlich
//! aus dem Pausenzustand heraus: [`ApprovalDriver::drive_to_completion`] liest
//! den vom Kern festgehaltenen [`PendingApproval`] und ruft
//! [`TuiApprovalHandler::open_prompt`] nach (W1-08, Register G-007).
//!
//! Warum das nötig ist: `harw_core::turn_loop::check_approval` befragt **jeden**
//! Handler und aggregiert (`Deny` > `AskUser` > `Allow`). Ein `review`, das
//! selbst einen Prompt öffnet, zeigte dem Nutzer deshalb auch dann eine Frage,
//! wenn ein späterer Handler den Aufruf verbietet, und vergab bei verworfener
//! `AskUser`-ID eine zweite, nie beantwortete Frage. Zusätzlich prüft
//! `preflight_approvals` mehrere Calls im Voraus — jede dort geöffnete Frage
//! wäre verfrüht.
//!
//! # Nebenläufigkeit
//! [`TuiApprovalHandler`] ist `Send + Sync` (innerer `std::sync::Mutex`) und
//! wird als `Arc` in die [`ExtensionRegistry`][harw_extension_api::ExtensionRegistry]
//! gelegt. Der Renderer-Thread wird **nie** blockiert: die Frage geht über
//! einen ungepufferten `mpsc`-Kanal; gewartet wird ausschließlich
//! `await`-basiert auf einem `tokio::sync::oneshot`. Die Futures dieses Moduls
//! werden auf dem `current_thread`-Runtime der TUI im selben Task gepollt wie
//! das Turn-Future.
//!
//! # Fehlertypen
//! [`ApprovalDriverError`] — Wiederaufnahme-Fehler des Kerns, fehlender
//! Pausezustand und die überschrittene Abbruchgrenze.
//!
//! # Spec
//! harw-tui AP W5-02 — Gegenseite zu `app.rs` „pausiert (noch nicht
//! unterstützt)".
//!
//! # Examples
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_tui::approval::{ApprovalDriver, ApprovalScope, TuiApprovalHandler};
//!
//! // Aufbau: Handler in die Registry, Empfänger an den Renderer.
//! let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::Deferred);
//! // Methodensyntax, nicht `Arc::clone(&handler)`: bei der UFCS-Form würde der
//! // Typparameter aus dem erwarteten Typ inferiert und bereits ein
//! // `&Arc<dyn ApprovalHandler>` verlangt — die Coercion käme zu spät.
//! let registered: Arc<dyn harw_extension_api::ApprovalHandler> = handler.clone();
//! let registry = harw_extension_api::ExtensionRegistry::builder()
//!     .approval_handler(registered)
//!     .build();
//! let driver = ApprovalDriver::new(Arc::clone(&handler));
//!
//! // Renderer-Seite: eine Frage beantworten.
//! if let Ok(prompt) = prompts.try_recv() {
//!     prompt.reject("nicht jetzt");
//! }
//! # let _ = (registry, driver);
//! ```

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harw_core::{
    AgentSession, ApprovalResolution, CoreError, ManagedAgentSpawner, ModelProvider,
    PendingApproval, StateStore, ToolCallResult, TurnInput, TurnOutcome, resume_after_approval,
    resume_after_child,
};
use harw_extension_api::{AgentSpawnError, ApprovalDecision, ApprovalHandler, ExtFuture, ToolCall};
use harw_types::{ItemId, SessionId, ToolCallId};
use tokio::sync::{mpsc, oneshot};

// ── Konstanten ───────────────────────────────────────────────────────────────

/// Obergrenze für **aufeinanderfolgende** Wiederaufnahmen innerhalb eines Turns.
///
/// # Beschreibung
/// Eine Schleife über [`TurnOutcome`] kann prinzipiell endlos laufen: ein Modell,
/// das nach jeder Ablehnung dasselbe Werkzeug erneut anfordert, erzeugt eine
/// unbegrenzte Folge von `AwaitingApproval`. Der Zähler wird pro Aufruf von
/// [`ApprovalDriver::drive_to_completion`] geführt, nicht global — ein neuer
/// Turn startet wieder bei null.
pub const MAX_CONSECUTIVE_RESUMES: usize = 32;

/// Voreingestellte Wartezeit auf **eine** Nutzerentscheidung
/// (Konstruktorvorgabe von [`TuiApprovalHandler::new`]/
/// [`TuiApprovalHandler::with_scope`]; [`TuiApprovalHandler::await_resolution`]
/// wartet genau diese Dauer, bevor sie [`ApprovalResolution::timed_out`]
/// liefert).
///
/// # Zwei Ebenen, eine Politik
/// [`ApprovalDriver::resolve`] prüft **zuerst** den kernseitigen
/// Ablaufzeitpunkt ([`harw_core::PendingApproval::timeout_at`], gesetzt von
/// [`harw_core::AgentSession::begin_approval`]) und lehnt sofort ab, wenn er
/// bereits erreicht ist, statt eine neue Wartezeit zu eröffnen — siehe dort.
/// Ist die Pause noch nicht abgelaufen, wartet [`TuiApprovalHandler::await_resolution`]
/// anschließend bis zu dieser Konstante auf die tatsächliche Antwort. Beide
/// Ebenen müssen auf dieselbe Frist eingestellt sein, sonst könnte der Kern
/// eine Pause für abgelaufen halten, während der Handler noch wartet (oder
/// umgekehrt); ein `assert_eq!` in den Modultests dieser Datei erzwingt die
/// Übereinstimmung mit [`harw_core::DEFAULT_APPROVAL_TIMEOUT`] (300 s).
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// Begründung, die eine wegen eines toten Fragekanals erzwungene Ablehnung trägt.
const REASON_PROMPT_UNDELIVERABLE: &str =
    "approval prompt could not be delivered to the terminal UI";

/// Begründung, die eine wegen einer fallengelassenen Antwort erzwungene
/// Ablehnung trägt.
const REASON_ANSWER_DROPPED: &str = "the terminal UI closed the approval prompt without answering";

/// Begründung, die eine wegen eines vergifteten Mutex erzwungene Ablehnung trägt.
const REASON_REGISTRY_POISONED: &str = "the approval prompt registry is unusable";

// ── ApprovalPrompt ───────────────────────────────────────────────────────────

/// Empfängerseite des Fragekanals — der Renderer pollt sie.
///
/// # Beschreibung
/// Ungepuffert (`unbounded`), damit [`TuiApprovalHandler::open_prompt`] eine
/// Frage stellen kann, ohne je zu blockieren. Der Renderer nimmt jede Frage
/// genau einmal entgegen und beantwortet sie über die Methoden von
/// [`ApprovalPrompt`].
pub type ApprovalPromptReceiver = mpsc::UnboundedReceiver<ApprovalPrompt>;

/// Eine einzelne Freigabefrage auf dem Weg zum Renderer.
///
/// # Description
/// Trägt die vom Kern erfasste, unveränderte [`ToolCall`]-Invokation und den
/// Rückkanal für die Antwort. Der Wert wird beim Beantworten **konsumiert** —
/// eine Frage kann daher weder doppelt beantwortet noch nach der Antwort noch
/// verändert werden.
///
/// Wird ein `ApprovalPrompt` fallengelassen, ohne beantwortet zu werden,
/// schließt der Rückkanal und der wartende Treiber liest das als **Ablehnung**
/// (siehe [`TuiApprovalHandler::await_resolution`]). Das ist der Normalfall,
/// wenn die TUI beendet wird, während eine Frage offen ist.
///
/// # Concurrency
/// `Send`; nicht `Sync` (der `oneshot::Sender` ist an einen Besitzer gebunden).
/// Der Wert wandert vom Turn-Future über einen `mpsc`-Kanal zum Renderer.
pub struct ApprovalPrompt {
    /// Korrelations-ID der Freigabeanfrage; identisch mit
    /// [`PendingApproval::request`] und mit dem `request`-Feld von
    /// [`TurnOutcome::AwaitingApproval`].
    request: ItemId,
    /// Der exakte, vom Kern festgehaltene Werkzeugaufruf.
    call: ToolCall,
    /// Rückkanal für genau eine Entscheidung.
    responder: oneshot::Sender<ApprovalResolution>,
}

impl ApprovalPrompt {
    /// Gibt die Korrelations-ID dieser Freigabeanfrage zurück.
    ///
    /// # Returns
    /// `&ItemId` — dieselbe ID, die [`TurnOutcome::AwaitingApproval`] meldet.
    #[must_use]
    pub fn request(&self) -> &ItemId {
        &self.request
    }

    /// Gibt den vollständigen, unveränderten Werkzeugaufruf zurück.
    ///
    /// # Returns
    /// `&ToolCall` — Name, Call-ID und JSON-Argumente, so wie der Kern sie
    /// festgehalten hat. Der Renderer darf ihn anzeigen, aber nicht ersetzen:
    /// [`resume_after_approval`] führt ausschließlich den vom Kern gespeicherten
    /// Aufruf aus.
    #[must_use]
    pub fn call(&self) -> &ToolCall {
        &self.call
    }

    /// Gibt die Call-ID des angefragten Werkzeugaufrufs zurück.
    ///
    /// # Returns
    /// `&ToolCallId` — nützlich, um die Frage einer bereits gerenderten
    /// Werkzeugzeile im Verlauf zuzuordnen.
    #[must_use]
    pub fn call_id(&self) -> &ToolCallId {
        &self.call.id
    }

    /// Gibt den Namen des angefragten Werkzeugs zurück.
    ///
    /// # Returns
    /// `&str`, z. B. `"fs.write"` oder `"shell.exec"`.
    #[must_use]
    pub fn tool_name(&self) -> &str {
        self.call.name.as_str()
    }

    /// Rendert die Argumente des Aufrufs als kompaktes JSON.
    ///
    /// # Description
    /// Bewusst eine eigene Methode statt eines `serde_json::Value`-Getters:
    /// `harw-tui` führt `serde_json` nicht als direkte Abhängigkeit, und der
    /// Renderer braucht ohnehin nur Text. Der Wert ist **ungefiltert** — die
    /// aufrufende Ansicht ist dafür zuständig, ihn zu kürzen, bevor sie ihn
    /// anzeigt.
    ///
    /// # Returns
    /// `String` mit der JSON-Serialisierung der Argumente.
    #[must_use]
    pub fn arguments_json(&self) -> String {
        self.call.arguments.to_string()
    }

    /// Beantwortet die Frage mit einer Freigabe.
    ///
    /// # Returns
    /// `true`, wenn die Antwort den wartenden Treiber erreicht hat; `false`,
    /// wenn dort niemand mehr wartet (der Turn wurde bereits abgebrochen).
    ///
    /// # Concurrency
    /// Nicht blockierend; sendet auf einen `oneshot`-Kanal.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # fn demo(prompt: harw_tui::approval::ApprovalPrompt) {
    /// let delivered = prompt.approve();
    /// assert!(delivered || !delivered); // beides ist ein gültiges Ergebnis
    /// # }
    /// ```
    pub fn approve(self) -> bool {
        self.answer(ApprovalResolution::Approve)
    }

    /// Beantwortet die Frage mit einer Ablehnung samt Begründung.
    ///
    /// # Arguments
    /// - `reason` (`impl Into<String>`): nutzersichtbare Begründung; sie landet
    ///   als Werkzeugergebnis im Modellverlauf.
    ///
    /// # Returns
    /// `true`, wenn die Antwort den wartenden Treiber erreicht hat.
    pub fn reject(self, reason: impl Into<String>) -> bool {
        self.answer(ApprovalResolution::Reject {
            reason: reason.into(),
        })
    }

    /// Beantwortet die Frage mit einer bereits gebildeten Entscheidung.
    ///
    /// # Arguments
    /// - `resolution` (`ApprovalResolution`): die Entscheidung des Nutzers.
    ///
    /// # Returns
    /// `true`, wenn die Antwort zugestellt wurde.
    pub fn answer(self, resolution: ApprovalResolution) -> bool {
        let request = self.request;
        let approved = matches!(resolution, ApprovalResolution::Approve);
        match self.responder.send(resolution) {
            Ok(()) => {
                tracing::debug!(
                    request = %request,
                    approved,
                    "tui.approval.answer_delivered"
                );
                true
            }
            Err(_) => {
                tracing::warn!(
                    request = %request,
                    approved,
                    "tui.approval.answer_undeliverable"
                );
                false
            }
        }
    }
}

/// Redigierte Darstellung: Argumente werden **nie** ausgegeben.
///
/// Folgt der Redaktionsregel von `harw-tools` (`call.arguments` wird niemals
/// geloggt, nur seine Größe).
impl fmt::Debug for ApprovalPrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApprovalPrompt")
            .field("request", &self.request.as_str())
            .field("call_id", &self.call.id.as_str())
            .field("tool_name", &self.call.name.as_str())
            .field("argument_bytes", &self.call.arguments.to_string().len())
            .finish()
    }
}

// ── ApprovalScope ────────────────────────────────────────────────────────────

/// Legt fest, welche Werkzeuge [`TuiApprovalHandler`] **von sich aus** anhält.
///
/// # Description
/// Die TUI-Registry trägt bereits eine Freigabepolitik (`DefaultApprovalPolicy`
/// aus `harw-registry-defaults`). Weil `harw_core::turn_loop::check_approval`
/// beim ersten Nicht-`Allow` abbricht, würde ein zweiter, ebenfalls
/// entscheidender Handler die bestehende Politik entweder verdoppeln oder
/// verdecken. [`ApprovalScope::Deferred`] — der Default — vermeidet das: der
/// Handler antwortet immer `Allow` und dient nur als Frage-/Antwortkanal. Der
/// [`ApprovalDriver`] stellt die Frage dann anhand des vom Kern festgehaltenen
/// [`PendingApproval`], egal welcher Handler die Pause ausgelöst hat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalScope {
    /// Hält nichts von sich aus an; beantwortet nur Pausen, die ein anderer
    /// registrierter Handler geöffnet hat. **Default.**
    Deferred,
    /// Hält genau die benannten Werkzeuge an, alles andere passiert.
    NamedTools(BTreeSet<String>),
    /// Hält jeden Werkzeugaufruf an. Sicherste, lauteste Variante.
    AllTools,
}

impl Default for ApprovalScope {
    /// Liefert [`ApprovalScope::Deferred`] — der Handler entscheidet nicht mit,
    /// solange nichts anderes verlangt wird.
    fn default() -> Self {
        Self::Deferred
    }
}

impl ApprovalScope {
    /// Baut einen Namens-Scope aus einer Aufzählung von Werkzeugnamen.
    ///
    /// # Arguments
    /// - `names` (`impl IntoIterator<Item = impl Into<String>>`): die exakt zu
    ///   bestätigenden Werkzeugnamen, z. B. `["fs.write", "shell.exec"]`.
    ///
    /// # Returns
    /// [`ApprovalScope::NamedTools`] mit den normalisierten Namen.
    ///
    /// # Examples
    /// ```rust
    /// use harw_tui::approval::ApprovalScope;
    ///
    /// let scope = ApprovalScope::named(["fs.write", "shell.exec"]);
    /// assert!(scope.requires_approval("fs.write"));
    /// assert!(!scope.requires_approval("fs.read"));
    /// ```
    #[must_use]
    pub fn named<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::NamedTools(names.into_iter().map(Into::into).collect())
    }

    /// Prüft, ob dieser Scope den benannten Werkzeugaufruf anhält.
    ///
    /// # Arguments
    /// - `tool_name` (`&str`): der Name aus [`ToolCall::name`].
    ///
    /// # Returns
    /// `true`, wenn der Handler eine Nutzerentscheidung anfordern soll.
    #[must_use]
    pub fn requires_approval(&self, tool_name: &str) -> bool {
        match self {
            Self::Deferred => false,
            Self::NamedTools(names) => names.contains(tool_name),
            Self::AllTools => true,
        }
    }
}

// ── TuiApprovalHandler ───────────────────────────────────────────────────────

/// [`ApprovalHandler`] der TUI: stellt Freigabefragen und nimmt Antworten an.
///
/// # Description
/// Der Handler hält zwei Hälften eines Frage-/Antwortpaars zusammen:
/// - die **Frage** geht über einen ungepufferten `mpsc`-Kanal an den Renderer,
/// - die **Antwort** kommt über einen `oneshot`-Kanal zurück, dessen
///   Empfängerseite bis zum Abholen in einer nach Anfrage-ID indizierten
///   Tabelle liegt.
///
/// [`ApprovalHandler::review`] wartet nie und **öffnet keine Frage**: es meldet
/// dem Kern nur [`ApprovalDecision::AskUser`]. Gefragt wird erst, wenn der Kern
/// den Turn sauber pausiert hat — [`ApprovalDriver`] ruft dann
/// [`Self::open_prompt`] und [`Self::await_resolution`].
///
/// # Concurrency
/// `Send + Sync`. Interne Veränderlichkeit über `std::sync::Mutex`; der Lock
/// wird nie über einen `await`-Punkt gehalten. Ein `Arc`-Klon teilt denselben
/// Zustand.
///
/// # Errors
/// Der Handler gibt keine `Result`s zurück — jeder Fehlerweg wird zu einer
/// Ablehnung, siehe Modul-Doku „Ablehnung ist der Default".
///
/// # Examples
/// ```rust
/// use harw_tui::approval::{ApprovalScope, TuiApprovalHandler};
///
/// let (handler, prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
/// assert_eq!(handler.pending_len(), 0);
/// drop(prompts);
/// ```
pub struct TuiApprovalHandler {
    /// Senderseite des Fragekanals zum Renderer.
    prompt_tx: mpsc::UnboundedSender<ApprovalPrompt>,
    /// Offene Rückkanäle, indiziert über die Anfrage-ID als String.
    pending: Mutex<HashMap<String, oneshot::Receiver<ApprovalResolution>>>,
    /// Welche Werkzeuge dieser Handler selbst anhält.
    scope: ApprovalScope,
    /// Maximale Wartezeit je Einzelfrage.
    timeout: Duration,
}

impl TuiApprovalHandler {
    /// Baut einen Handler mit [`ApprovalScope::Deferred`] und
    /// [`DEFAULT_APPROVAL_TIMEOUT`].
    ///
    /// # Returns
    /// `(Arc<Self>, ApprovalPromptReceiver)` — der `Arc` gehört in die
    /// `ExtensionRegistry` **und** in den [`ApprovalDriver`], der Empfänger in
    /// die Renderer-Schleife.
    ///
    /// # Concurrency
    /// Synchron und allokierend; keine Threads.
    #[must_use]
    pub fn new() -> (Arc<Self>, ApprovalPromptReceiver) {
        Self::with_scope_and_timeout(ApprovalScope::default(), DEFAULT_APPROVAL_TIMEOUT)
    }

    /// Baut einen Handler mit eigenem Scope und [`DEFAULT_APPROVAL_TIMEOUT`].
    ///
    /// # Arguments
    /// - `scope` ([`ApprovalScope`]): welche Werkzeuge selbst angehalten werden.
    ///
    /// # Returns
    /// `(Arc<Self>, ApprovalPromptReceiver)`.
    #[must_use]
    pub fn with_scope(scope: ApprovalScope) -> (Arc<Self>, ApprovalPromptReceiver) {
        Self::with_scope_and_timeout(scope, DEFAULT_APPROVAL_TIMEOUT)
    }

    /// Baut einen Handler mit eigenem Scope und eigener Wartezeit.
    ///
    /// # Arguments
    /// - `scope` ([`ApprovalScope`]): welche Werkzeuge selbst angehalten werden.
    /// - `timeout` (`Duration`): Wartezeit je Einzelfrage. Nach Ablauf gilt die
    ///   Frage als **abgelehnt**.
    ///
    /// # Returns
    /// `(Arc<Self>, ApprovalPromptReceiver)`.
    #[must_use]
    pub fn with_scope_and_timeout(
        scope: ApprovalScope,
        timeout: Duration,
    ) -> (Arc<Self>, ApprovalPromptReceiver) {
        let (prompt_tx, prompt_rx) = mpsc::unbounded_channel();
        let handler = Arc::new(Self {
            prompt_tx,
            pending: Mutex::new(HashMap::new()),
            scope,
            timeout,
        });
        (handler, prompt_rx)
    }

    /// Gibt den konfigurierten Scope zurück.
    ///
    /// # Returns
    /// `&ApprovalScope`.
    #[must_use]
    pub fn scope(&self) -> &ApprovalScope {
        &self.scope
    }

    /// Gibt die konfigurierte Wartezeit je Einzelfrage zurück.
    ///
    /// # Returns
    /// `Duration`.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Zählt die derzeit offenen, noch nicht abgeholten Rückkanäle.
    ///
    /// # Returns
    /// Anzahl offener Anfragen; `0`, wenn die Tabelle unbenutzbar ist.
    ///
    /// # Concurrency
    /// Nimmt kurz den internen Lock.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.lock().map_or(0, |pending| pending.len())
    }

    /// Prüft, ob für `request` bereits ein Rückkanal bereitliegt.
    ///
    /// # Arguments
    /// - `request` (`&ItemId`): Korrelations-ID der Freigabeanfrage.
    ///
    /// # Returns
    /// `true`, wenn dieser Handler die Frage selbst gestellt hat.
    #[must_use]
    pub fn has_pending(&self, request: &ItemId) -> bool {
        self.pending
            .lock()
            .is_ok_and(|pending| pending.contains_key(request.as_str()))
    }

    /// Stellt eine Freigabefrage für eine bereits vergebene Anfrage-ID.
    ///
    /// # Description
    /// Notwendig, weil die Anfrage-ID nicht zwingend von diesem Handler stammt:
    /// `DefaultApprovalPolicy` und `ConfigApprovalPolicy` vergeben eigene IDs
    /// und gewinnen die Vorprüfung, sobald sie vor diesem Handler registriert
    /// sind. Der [`ApprovalDriver`] holt sich dann den exakten Aufruf aus
    /// [`AgentSession::pending_approval`] und ruft diese Methode nach.
    ///
    /// Eine bereits offene Frage zur selben ID wird **ersetzt**; der alte
    /// Rückkanal schließt dabei und der zugehörige Wartende liest das als
    /// Ablehnung.
    ///
    /// # Arguments
    /// - `request` (`&ItemId`): Korrelations-ID aus [`PendingApproval::request`].
    /// - `call` (`&ToolCall`): der vom Kern festgehaltene Aufruf.
    ///
    /// # Returns
    /// `true`, wenn die Frage den Renderer erreicht hat. **`false` bedeutet
    /// Ablehnung** — es gibt niemanden mehr, der zustimmen könnte, und der
    /// Aufrufer darf daraus niemals eine Freigabe machen.
    ///
    /// # Concurrency
    /// Nimmt kurz den internen Lock und sendet danach ohne Lock.
    pub fn open_prompt(&self, request: &ItemId, call: &ToolCall) -> bool {
        let (responder, receiver) = oneshot::channel();
        {
            let Ok(mut pending) = self.pending.lock() else {
                tracing::error!(request = %request, "tui.approval.registry_poisoned");
                return false;
            };
            pending.insert(request.as_str().to_owned(), receiver);
        }

        let prompt = ApprovalPrompt {
            request: request.clone(),
            call: call.clone(),
            responder,
        };
        if self.prompt_tx.send(prompt).is_err() {
            // Der Renderer ist weg. Den Rückkanal wieder abräumen, damit ein
            // späterer `await_resolution` nicht auf einen Sender wartet, den
            // niemand mehr hält — und `false` melden, was der Aufrufer als
            // Ablehnung behandeln muss.
            self.discard(request);
            tracing::warn!(
                request = %request,
                tool = %call.name,
                "tui.approval.prompt_channel_closed"
            );
            return false;
        }

        tracing::info!(
            request = %request,
            call_id = %call.id,
            tool = %call.name,
            argument_bytes = call.arguments.to_string().len(),
            "tui.approval.prompt_issued"
        );
        true
    }

    /// Wartet auf genau eine Entscheidung zu `request`.
    ///
    /// # Description
    /// **Hier wird entschieden, dass Ablehnung der Default ist.** Alle vier
    /// Wege, auf denen keine ausdrückliche Antwort eintrifft, enden in
    /// [`ApprovalResolution::Reject`]:
    /// 1. kein Rückkanal vorhanden (Frage wurde nie gestellt oder schon abgeholt),
    /// 2. Rückkanal geschlossen, weil der Renderer die Frage fallen ließ oder
    ///    die TUI beendet wurde,
    /// 3. [`Self::timeout`] abgelaufen,
    /// 4. die Kanaltabelle war unbenutzbar.
    ///
    /// Eine Freigabe entsteht ausschließlich dadurch, dass jemand
    /// [`ApprovalPrompt::approve`] gerufen hat. Die Ablehnung ist zudem
    /// *vollständig*: sie führt über [`resume_after_approval`] zu einem
    /// regulären Werkzeugergebnis und damit zu einem sauberen Turn-Ende — der
    /// Turn bleibt nie halbfertig stehen.
    ///
    /// # Arguments
    /// - `request` (`&ItemId`): Korrelations-ID der Freigabeanfrage.
    ///
    /// # Returns
    /// [`ApprovalResolution::Approve`] nur bei ausdrücklicher Freigabe, sonst
    /// [`ApprovalResolution::Reject`] mit sprechender Begründung.
    ///
    /// # Concurrency
    /// `async`; der interne Lock wird vor dem `await` freigegeben. Blockiert
    /// den Renderer-Thread nicht.
    ///
    /// # Examples
    /// ```rust
    /// # use harw_tui::approval::{ApprovalScope, TuiApprovalHandler};
    /// # use harw_core::ApprovalResolution;
    /// # use harw_types::ItemId;
    /// # async fn demo() {
    /// let (handler, prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
    /// drop(prompts); // UI weg
    /// let decision = handler.await_resolution(&ItemId::new()).await;
    /// assert!(matches!(decision, ApprovalResolution::Reject { .. }));
    /// # }
    /// ```
    pub async fn await_resolution(&self, request: &ItemId) -> ApprovalResolution {
        let receiver = match self.pending.lock() {
            Ok(mut pending) => pending.remove(request.as_str()),
            Err(_) => {
                tracing::error!(request = %request, "tui.approval.registry_poisoned");
                return reject(REASON_REGISTRY_POISONED);
            }
        };

        let Some(receiver) = receiver else {
            tracing::warn!(request = %request, "tui.approval.no_open_prompt");
            return reject(REASON_PROMPT_UNDELIVERABLE);
        };

        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(resolution)) => {
                tracing::info!(
                    request = %request,
                    approved = matches!(resolution, ApprovalResolution::Approve),
                    "tui.approval.resolved"
                );
                resolution
            }
            // Der `oneshot::Sender` wurde fallengelassen: die Frage ist
            // verschwunden, ohne beantwortet zu werden. Kein Zustimmungspfad.
            Ok(Err(_recv_error)) => {
                tracing::warn!(request = %request, "tui.approval.channel_closed_rejects");
                reject(REASON_ANSWER_DROPPED)
            }
            // Zeitablauf: ebenfalls Ablehnung, damit der Turn regulär endet
            // statt in `WaitingForApproval` hängenzubleiben. Die Begründung
            // kommt aus `harw_core::ApprovalResolution::timed_out` — derselbe
            // Text, den auch der kernseitige `timeout_at`-Kurzschluss in
            // [`ApprovalDriver::resolve`] verwendet (§4.4 des
            // Interaktionsvertrags: eine Begründung, nicht eine je Front-End).
            Err(_elapsed) => {
                tracing::warn!(
                    request = %request,
                    timeout_ms = u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
                    "tui.approval.timed_out_rejects"
                );
                ApprovalResolution::timed_out()
            }
        }
    }

    /// Entfernt einen Rückkanal, ohne auf ihn zu warten (interner Aufräumpfad).
    fn discard(&self, request: &ItemId) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(request.as_str());
        }
    }
}

/// Redigierte Darstellung ohne Aufrufinhalte.
impl fmt::Debug for TuiApprovalHandler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TuiApprovalHandler")
            .field("scope", &self.scope)
            .field("timeout", &self.timeout)
            .field("pending", &self.pending_len())
            .finish()
    }
}

impl ApprovalHandler for TuiApprovalHandler {
    /// Prüft einen Werkzeugaufruf **seiteneffektfrei** (CONTRACTS.md §extension).
    ///
    /// # Description
    /// Liegt der Aufruf außerhalb von [`Self::scope`], lautet die Antwort
    /// [`ApprovalDecision::Allow`] — andere registrierte Handler entscheiden
    /// dann weiter. Andernfalls wird eine Anfrage-ID vergeben und
    /// [`ApprovalDecision::AskUser`] gemeldet, was den Turn im Kern sauber
    /// pausiert. **Es wird keine Frage geöffnet, nichts gesendet und nichts
    /// in der Rückkanal-Tabelle abgelegt**; das erledigt der
    /// [`ApprovalDriver`] aus dem Pausenzustand heraus (siehe Modul-Doku).
    ///
    /// Ist der Fragekanal bereits geschlossen — der Renderer ist weg —, lautet
    /// die Antwort [`ApprovalDecision::Deny`]: eine Frage, die niemand sehen
    /// kann, ist keine Freigabe. Die Prüfung liest nur den Kanalzustand
    /// (`UnboundedSender::is_closed`) und verändert ihn nicht.
    ///
    /// # Concurrency
    /// Das zurückgegebene Future ist sofort fertig; es nimmt keinen Lock.
    fn review<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
        let decision = if !self.scope.requires_approval(call.name.as_str()) {
            ApprovalDecision::Allow
        } else if self.prompt_tx.is_closed() {
            ApprovalDecision::Deny(REASON_PROMPT_UNDELIVERABLE.to_owned())
        } else {
            ApprovalDecision::AskUser(ItemId::new())
        };
        Box::pin(async move { decision })
    }
}

/// Baut eine Ablehnung mit fester Begründung.
fn reject(reason: &str) -> ApprovalResolution {
    ApprovalResolution::Reject {
        reason: reason.to_owned(),
    }
}

// ── ChildTurnDriver ──────────────────────────────────────────────────────────

/// Boxed Future eines Kind-Turns.
///
/// `Send` bleibt gefordert, damit ein Kind-Future später ohne Änderung dieses
/// Kontrakts in einen `tokio::spawn`-Task wandern kann.
pub type ChildDriveFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ToolCallResult, AgentSpawnError>> + Send + 'a>>;

/// Treibt ein vom Eltern-Turn abgezweigtes Kind bis zu seinem Ergebnis.
///
/// # Description
/// Eigener Trait statt direkter Verwendung von [`ManagedAgentSpawner`], weil
/// die Registry nur ein `Arc<dyn AgentSpawner>` hält — und `AgentSpawner`
/// kennt kein `run_child`. Der Trait ist zugleich die Testnaht: eine Attrappe
/// ersetzt den echten Spawner, ohne dass ein Prozess entsteht.
///
/// # Concurrency
/// `Send + Sync`; die zurückgegebenen Futures sind `Send`.
pub trait ChildTurnDriver: Send + Sync {
    /// Führt den Turn des Kindes aus und verpackt sein Ergebnis.
    ///
    /// # Arguments
    /// - `child` (`&SessionId`): die vom Spawner vergebene Kind-Session.
    /// - `store` (`&dyn StateStore`): Persistenz für den Kind-Turn.
    ///
    /// # Returns
    /// Ein [`ToolCallResult`], das der Eltern-Turn als Werkzeugergebnis
    /// aufnimmt.
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn das Kind gar nicht ausgeführt werden konnte.
    /// Der [`ApprovalDriver`] wandelt auch das noch in ein Fehler-Ergebnis, um
    /// den Eltern-Turn sauber zu beenden.
    fn drive_child<'a>(
        &'a self,
        child: &'a SessionId,
        store: &'a dyn StateStore,
    ) -> ChildDriveFuture<'a>;
}

impl ChildTurnDriver for ManagedAgentSpawner {
    /// Führt das Kind über [`ManagedAgentSpawner::run_child`] aus.
    ///
    /// # Description
    /// Das Kind erhält [`TurnInput::default`]: sein Auftrag steckt bereits im
    /// `SpawnInput`, den der Kern beim Handoff übergeben hat; ein zusätzlicher
    /// Nutzer-Text würde den Verlauf verfälschen. Nach einem regulären Ende
    /// wird die letzte Assistant-Antwort des Kindes zum Werkzeugergebnis.
    ///
    /// Pausiert das Kind selbst, endet es hier: verschachtelte Pausen werden
    /// von der TUI **nicht** weitergetrieben, sondern als Fehler-Ergebnis an
    /// den Eltern-Turn gemeldet. Damit bleibt genau eine Instanz — der
    /// [`ApprovalDriver`] des Eltern-Turns — für Nutzerfragen zuständig.
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn das Kind nicht admittiert ist, sein Turn
    /// fehlschlägt oder es keinen Antworttext hinterlassen hat.
    fn drive_child<'a>(
        &'a self,
        child: &'a SessionId,
        store: &'a dyn StateStore,
    ) -> ChildDriveFuture<'a> {
        Box::pin(async move {
            let run = self.run_child(child, store, TurnInput::default()).await?;
            match run.outcome {
                TurnOutcome::Completed => {
                    let text = self.child_final_assistant_text(child)?;
                    tracing::info!(
                        child = %child,
                        response_bytes = text.len(),
                        "tui.child.completed"
                    );
                    Ok(ToolCallResult::success(text.into()))
                }
                TurnOutcome::AwaitingApproval { .. } => {
                    tracing::warn!(child = %child, "tui.child.paused_on_approval");
                    Ok(ToolCallResult::error(
                        "child agent paused for its own approval; nested approvals are not \
                         driven from the terminal UI",
                    ))
                }
                TurnOutcome::AwaitingChild { role, .. } => {
                    tracing::warn!(child = %child, role = %role, "tui.child.paused_on_handoff");
                    Ok(ToolCallResult::error(format!(
                        "child agent paused on its own handoff to '{role}'; nested handoffs are \
                         not driven from the terminal UI"
                    )))
                }
                // Terminale Ausgänge: kein Antworttext, aber auch keine Pause —
                // der Aufrufer bekommt den Grund als Werkzeugfehler zu sehen.
                TurnOutcome::Cancelled { reason } => {
                    tracing::warn!(child = %child, ?reason, "tui.child.cancelled");
                    Ok(ToolCallResult::error(format!(
                        "child agent was cancelled: {reason:?}"
                    )))
                }
                TurnOutcome::Truncated => {
                    tracing::warn!(child = %child, "tui.child.truncated");
                    Ok(ToolCallResult::error(
                        "child agent stopped because its model output was truncated",
                    ))
                }
                TurnOutcome::Refused { detail } => {
                    tracing::warn!(child = %child, ?detail, "tui.child.refused");
                    Ok(ToolCallResult::error(match detail {
                        Some(detail) => format!("child agent refused to answer: {detail}"),
                        None => "child agent refused to answer".to_owned(),
                    }))
                }
                TurnOutcome::Failed { reason } => {
                    tracing::warn!(child = %child, %reason, "tui.child.failed");
                    Ok(ToolCallResult::error(format!(
                        "child agent failed: {reason}"
                    )))
                }
            }
        })
    }
}

// ── Fehler ───────────────────────────────────────────────────────────────────

/// An welcher Wiederaufnahme ein Kern-Aufruf gescheitert ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeStage {
    /// [`resume_after_approval`].
    Approval,
    /// [`resume_after_child`].
    Child,
}

impl fmt::Display for ResumeStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Approval => formatter.write_str("approval resume"),
            Self::Child => formatter.write_str("child resume"),
        }
    }
}

/// Bequemer Ergebnistyp dieses Moduls.
pub type ApprovalDriverResult<T> = Result<T, ApprovalDriverError>;

/// Fehler, die beim Wiederaufnehmen eines pausierten Turns entstehen.
///
/// # Description
/// Handgeschrieben (kein `thiserror`/`anyhow`). Jede Variante trägt alles, was
/// zum Verstehen des Fehlers nötig ist, ohne in den Quelltext zu schauen.
///
/// Es gibt bewusst **kein** `From<CoreError>`: welcher der beiden
/// Wiederaufnahme-Pfade gescheitert ist, lässt sich dem [`CoreError`] nicht
/// ansehen, und ein blindes `?` würde diese Information verlieren. Die
/// Aufrufstellen benutzen deshalb `.map_err(|source| …)` mit explizitem
/// [`ResumeStage`].
pub enum ApprovalDriverError {
    /// Ein Wiederaufnahme-Aufruf des Kerns ist fehlgeschlagen.
    Resume {
        /// Welcher Wiederaufnahmepfad betroffen war.
        stage: ResumeStage,
        /// Der Fehler des Kerns.
        source: CoreError,
    },
    /// Der Turn meldete eine Freigabepause, die Session hielt aber keinen
    /// zugehörigen Aufruf fest. Ohne ihn gibt es nichts zu fragen und nichts
    /// auszuführen.
    MissingPendingApproval {
        /// ID der betroffenen Session.
        session: String,
    },
    /// Die Abbruchgrenze für aufeinanderfolgende Wiederaufnahmen wurde
    /// überschritten. Die Session wurde daraufhin ausdrücklich als
    /// fehlgeschlagen markiert, damit kein halbfertiger Pausezustand bleibt.
    ResumeLimitExceeded {
        /// Die überschrittene Grenze.
        limit: usize,
        /// Die Pauseart, die zuletzt gemeldet wurde.
        last_pause: &'static str,
    },
}

impl fmt::Display for ApprovalDriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resume { stage, source } => {
                write!(formatter, "{stage} failed: {source}")
            }
            Self::MissingPendingApproval { session } => write!(
                formatter,
                "session {session} reported an approval pause without a pending tool call"
            ),
            Self::ResumeLimitExceeded { limit, last_pause } => write!(
                formatter,
                "turn still paused ({last_pause}) after {limit} consecutive resumes; \
                 the session was failed instead of resuming again"
            ),
        }
    }
}

/// Debug delegiert an [`fmt::Display`], damit es nur eine Formatierung gibt.
impl fmt::Debug for ApprovalDriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for ApprovalDriverError {
    /// Liefert den eingebetteten [`CoreError`], falls vorhanden.
    ///
    /// # Returns
    /// - `Some(&CoreError)` für [`ApprovalDriverError::Resume`].
    /// - `None` für alle anderen Varianten.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resume { source, .. } => Some(source),
            Self::MissingPendingApproval { .. } | Self::ResumeLimitExceeded { .. } => None,
        }
    }
}

// ── ApprovalDriver ───────────────────────────────────────────────────────────

/// Treibt einen pausierten Turn bis zum Ende — eine Schleife für beide Pausen.
///
/// # Description
/// Nimmt das [`TurnOutcome`] entgegen, das `run_turn` (oder ein vorheriges
/// `resume_*`) geliefert hat, und arbeitet es ab, bis
/// [`TurnOutcome::Completed`] erreicht ist oder die Abbruchgrenze greift.
/// Begründung für die **eine** Schleife statt zweier: siehe Modul-Doku.
///
/// # Concurrency
/// Alle Methoden sind `async` und werden im selben Task gepollt wie das
/// Turn-Future der TUI. Der Treiber hält keine Locks über `await`-Punkte.
///
/// # Examples
/// ```rust,no_run
/// # use std::sync::Arc;
/// # use harw_tui::approval::{ApprovalDriver, TuiApprovalHandler};
/// # use harw_core::{AgentSession, ModelProvider, StateStore, TurnOutcome};
/// # async fn demo(
/// #     session: &mut AgentSession,
/// #     model: &dyn ModelProvider,
/// #     store: &dyn StateStore,
/// #     outcome: TurnOutcome,
/// # ) {
/// let (handler, _prompts) = TuiApprovalHandler::new();
/// let driver = ApprovalDriver::new(Arc::clone(&handler));
/// let finished = driver
///     .drive_to_completion(session, model, store, None, outcome)
///     .await;
/// assert!(finished.is_ok() || finished.is_err());
/// # }
/// ```
pub struct ApprovalDriver {
    /// Frage-/Antwortkanal; derselbe `Arc`, der in der Registry liegt.
    handler: Arc<TuiApprovalHandler>,
    /// Abbruchgrenze für aufeinanderfolgende Wiederaufnahmen.
    max_resumes: usize,
}

impl ApprovalDriver {
    /// Baut einen Treiber mit [`MAX_CONSECUTIVE_RESUMES`].
    ///
    /// # Arguments
    /// - `handler` (`Arc<TuiApprovalHandler>`): derselbe Handler, der in der
    ///   `ExtensionRegistry` registriert ist. Ein anderer Handler würde die
    ///   Rückkanäle nicht finden und jede Frage als Ablehnung auflösen.
    ///
    /// # Returns
    /// Einen einsatzbereiten `ApprovalDriver`.
    #[must_use]
    pub fn new(handler: Arc<TuiApprovalHandler>) -> Self {
        Self {
            handler,
            max_resumes: MAX_CONSECUTIVE_RESUMES,
        }
    }

    /// Baut einen Treiber mit eigener Abbruchgrenze.
    ///
    /// # Arguments
    /// - `handler` (`Arc<TuiApprovalHandler>`): siehe [`Self::new`].
    /// - `max_resumes` (`usize`): Obergrenze aufeinanderfolgender
    ///   Wiederaufnahmen. `0` bedeutet: keine einzige Wiederaufnahme, jede
    ///   Pause schlägt sofort fehl.
    ///
    /// # Returns
    /// Einen `ApprovalDriver` mit der angegebenen Grenze.
    #[must_use]
    pub fn with_max_resumes(handler: Arc<TuiApprovalHandler>, max_resumes: usize) -> Self {
        Self {
            handler,
            max_resumes,
        }
    }

    /// Gibt den geteilten Handler zurück.
    ///
    /// # Returns
    /// `&Arc<TuiApprovalHandler>` — für `Arc::clone` beim Registrieren.
    #[must_use]
    pub fn handler(&self) -> &Arc<TuiApprovalHandler> {
        &self.handler
    }

    /// Gibt die konfigurierte Abbruchgrenze zurück.
    ///
    /// # Returns
    /// `usize`.
    #[must_use]
    pub fn max_resumes(&self) -> usize {
        self.max_resumes
    }

    /// Arbeitet alle Pausen eines Turns ab, bis er terminiert.
    ///
    /// # Description
    /// Eine Schleife über beide Ausgänge:
    /// - [`TurnOutcome::AwaitingApproval`] — der exakte Aufruf wird aus
    ///   [`AgentSession::pending_approval`] gelesen (nicht aus dem Outcome, das
    ///   nur IDs trägt), die Frage gestellt, die Antwort abgewartet und
    ///   [`resume_after_approval`] gerufen. Der [`ApprovalActor`][harw_types::ApprovalActor]
    ///   stammt ebenfalls aus dem festgehaltenen Pausezustand, damit die
    ///   Identitätsprüfung des Kerns nicht an einer TUI-seitigen Kopie scheitert.
    /// - [`TurnOutcome::AwaitingChild`] — das Kind wird über `children`
    ///   getrieben und sein Ergebnis über [`resume_after_child`] eingespielt.
    ///   Ohne Treiber oder bei einem Spawn-Fehler wird ein **Fehler-Ergebnis**
    ///   eingespielt statt abzubrechen: der Eltern-Turn endet dadurch regulär,
    ///   statt in `WaitingForChild` hängenzubleiben.
    ///
    /// Jeder Durchgang ersetzt `outcome` durch das Ergebnis der Wiederaufnahme
    /// — ein Turn darf beliebig oft hintereinander pausieren, und ein Kind darf
    /// eine Freigabe auslösen wie eine Freigabe ein Kind.
    ///
    /// # Arguments
    /// - `session` (`&mut AgentSession`): die pausierte Session.
    /// - `model` (`&dyn ModelProvider`): derselbe Provider wie im Ausgangsturn.
    /// - `store` (`&dyn StateStore`): derselbe Store wie im Ausgangsturn.
    /// - `children` (`Option<&dyn ChildTurnDriver>`): Kind-Treiber; `None`
    ///   beantwortet jeden Handoff mit einem Fehler-Ergebnis.
    /// - `outcome` ([`TurnOutcome`]): das Ergebnis des Ausgangsturns.
    ///
    /// # Returns
    /// [`TurnOutcome::Completed`]. Ein anderer Wert wird nie zurückgegeben —
    /// jede Pause wird entweder abgearbeitet oder zum Fehler.
    ///
    /// # Errors
    /// - [`ApprovalDriverError::Resume`]: der Kern hat eine Wiederaufnahme
    ///   abgelehnt (falscher Zustand, fehlender Actor, Modellfehler …).
    /// - [`ApprovalDriverError::MissingPendingApproval`]: die Session meldete
    ///   eine Freigabepause ohne festgehaltenen Aufruf.
    /// - [`ApprovalDriverError::ResumeLimitExceeded`]: die in
    ///   [`Self::max_resumes`] konfigurierte Grenze wurde überschritten; die
    ///   Session wurde als fehlgeschlagen markiert.
    ///
    /// # Concurrency
    /// `async`, single-task. Wartet ausschließlich auf `oneshot`-Antworten,
    /// Kind-Futures und Kern-Futures; blockiert nie einen Thread.
    pub async fn drive_to_completion(
        &self,
        session: &mut AgentSession,
        model: &dyn ModelProvider,
        store: &dyn StateStore,
        children: Option<&dyn ChildTurnDriver>,
        outcome: TurnOutcome,
    ) -> ApprovalDriverResult<TurnOutcome> {
        let mut outcome = outcome;

        for step in 0..self.max_resumes {
            match outcome {
                TurnOutcome::Completed => return Ok(TurnOutcome::Completed),

                // Terminale Ausgänge sind keine Pause: sie werden unverändert
                // durchgereicht, damit der Aufrufer den echten Grund sieht,
                // statt ihn als „abgeschlossen" zu lesen.
                terminal @ (TurnOutcome::Cancelled { .. }
                | TurnOutcome::Truncated
                | TurnOutcome::Refused { .. }
                | TurnOutcome::Failed { .. }) => return Ok(terminal),

                TurnOutcome::AwaitingApproval { call_id, request } => {
                    let Some(pending) = session.pending_approval().cloned() else {
                        tracing::error!(
                            request = %request,
                            call_id = %call_id,
                            "tui.resume.missing_pending_approval"
                        );
                        return Err(ApprovalDriverError::MissingPendingApproval {
                            session: session.id().to_string(),
                        });
                    };

                    tracing::info!(
                        step,
                        request = %pending.request,
                        call_id = %pending.call.id,
                        tool = %pending.call.name,
                        "tui.resume.approval_pause"
                    );

                    let resolution = self.resolve(&pending).await;
                    let actor = pending.actor.clone();
                    outcome = resume_after_approval(session, model, store, actor, resolution)
                        .await
                        .map_err(|source| ApprovalDriverError::Resume {
                            stage: ResumeStage::Approval,
                            source,
                        })?;
                }

                TurnOutcome::AwaitingChild {
                    child,
                    call_id,
                    role,
                } => {
                    tracing::info!(
                        step,
                        child = %child,
                        call_id = %call_id,
                        role = %role,
                        "tui.resume.child_pause"
                    );

                    let result = Self::child_result(children, &child, &role, store).await;
                    outcome = resume_after_child(session, model, store, child, call_id, result)
                        .await
                        .map_err(|source| ApprovalDriverError::Resume {
                            stage: ResumeStage::Child,
                            source,
                        })?;
                }
            }
        }

        // Grenze erreicht. Nicht weiterdrehen: der Turn wird ausdrücklich als
        // fehlgeschlagen markiert, damit die Session nicht dauerhaft in
        // `WaitingForApproval`/`WaitingForChild` stehenbleibt (dort würde jeder
        // weitere Turn mit `NotIdle` scheitern, ohne dass jemand erfährt, warum).
        let last_pause = pause_label(&outcome);
        tracing::error!(
            limit = self.max_resumes,
            last_pause,
            session = %session.id(),
            "tui.resume.limit_exceeded"
        );
        session.fail(format!(
            "turn still paused ({last_pause}) after {} consecutive resumes",
            self.max_resumes
        ));
        Err(ApprovalDriverError::ResumeLimitExceeded {
            limit: self.max_resumes,
            last_pause,
        })
    }

    /// Beschafft die Nutzerentscheidung zu einer festgehaltenen Pause.
    ///
    /// # Description
    /// Prüft zuerst [`PendingApproval::timeout_at`] gegen die Wanduhr — der
    /// Kern legt diesen Zeitpunkt bereits bei
    /// [`harw_core::AgentSession::begin_approval`] fest (Interaktionsvertrag
    /// §4.4). Ist er bereits erreicht (z. B. weil die TUI neu gestartet
    /// wurde, während der Turn pausierte, oder weil ein vorheriger
    /// `resume_after_approval`-Versuch fehlschlug und dieselbe Pause erneut
    /// vorliegt), wird sofort [`ApprovalResolution::timed_out`] geliefert,
    /// **ohne** eine neue, volle Wartezeit zu eröffnen — ein bereits
    /// abgelaufenes Fenster bekommt keine zweite Chance.
    ///
    /// Andernfalls stellt sie die Frage nach, wenn die Anfrage-ID von einem
    /// anderen [`ApprovalHandler`] stammt (z. B. `DefaultApprovalPolicy`).
    /// Gelingt das nicht, ist das Ergebnis eine Ablehnung — nie eine
    /// Freigabe.
    async fn resolve(&self, pending: &PendingApproval) -> ApprovalResolution {
        if pending.is_timed_out(jiff::Timestamp::now()) {
            tracing::warn!(
                request = %pending.request,
                timeout_at = %pending.timeout_at,
                "tui.approval.already_timed_out"
            );
            return ApprovalResolution::timed_out();
        }
        if !self.handler.has_pending(&pending.request)
            && !self.handler.open_prompt(&pending.request, &pending.call)
        {
            return reject(REASON_PROMPT_UNDELIVERABLE);
        }
        self.handler.await_resolution(&pending.request).await
    }

    /// Treibt ein Kind und verpackt jeden Ausgang als [`ToolCallResult`].
    ///
    /// Auch ein Spawn-Fehler und ein fehlender Treiber werden zu einem
    /// Fehler-Ergebnis: der Eltern-Turn muss das Kind-Werkzeug beantwortet
    /// bekommen, sonst bliebe er in `WaitingForChild` stehen.
    async fn child_result(
        children: Option<&dyn ChildTurnDriver>,
        child: &SessionId,
        role: &str,
        store: &dyn StateStore,
    ) -> ToolCallResult {
        let Some(driver) = children else {
            tracing::warn!(child = %child, role = %role, "tui.child.no_driver");
            return ToolCallResult::error(format!(
                "child agent '{role}' could not be run: this terminal session has no child driver"
            ));
        };

        match driver.drive_child(child, store).await {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(
                    child = %child,
                    role = %role,
                    error = %error,
                    "tui.child.drive_failed"
                );
                ToolCallResult::error(format!("child agent '{role}' failed: {error}"))
            }
        }
    }
}

/// Kurzname einer Pauseart für Fehlermeldungen und Log-Felder.
fn pause_label(outcome: &TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Completed => "completed",
        TurnOutcome::AwaitingApproval { .. } => "awaiting approval",
        TurnOutcome::AwaitingChild { .. } => "awaiting child",
        TurnOutcome::Cancelled { .. } => "cancelled",
        TurnOutcome::Truncated => "truncated",
        TurnOutcome::Refused { .. } => "refused",
        TurnOutcome::Failed { .. } => "failed",
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use harw_agent_dsl::roles::AgentRoleId;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_core::{
        InMemoryStateStore, ModelFuture, ModelRequest, ModelResponse, SpawnContext, run_turn,
    };
    use harw_extension_api::{
        ExtensionRegistry, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName,
        ToolOutput, ToolProvider, ToolSpec,
    };
    use harw_tools::serde_json::{Value, json};
    use harw_tools::{FunctionToolSpec, JsonSchema};
    use harw_types::{AgentRole, ApprovalActor, TenantId, WorkspaceId};

    const WRITE_TOOL: &str = "fs.write";

    // ── Testdoubles ──────────────────────────────────────────────────────────

    /// Modell, das eine vorher festgelegte Folge von Antworten liefert.
    struct ScriptedModel {
        responses: Mutex<VecDeque<ModelResponse>>,
    }

    impl ScriptedModel {
        fn new(responses: Vec<ModelResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
            }
        }

        /// Baut eine Antwort, die genau einen Werkzeugaufruf anfordert.
        fn tool_response(name: &str, arguments: Value) -> ModelResponse {
            ModelResponse {
                message: None,
                tool_calls: vec![ToolCall {
                    id: ToolCallId::new(),
                    name: ToolName::new(name),
                    arguments,
                }],
                usage: harw_types::TokenUsage::default(),
                // `reasoning`/`stop` spielen für die Freigabe-Tests keine
                // Rolle — Default liefert `None` bzw. `StopReason::EndTurn`.
                ..Default::default()
            }
        }
    }

    impl ModelProvider for ScriptedModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            let next = match self.responses.lock() {
                Ok(mut responses) => responses.pop_front(),
                Err(_) => None,
            };
            Box::pin(async move {
                Ok(next.unwrap_or_else(|| ModelResponse::text("scripted model exhausted")))
            })
        }
    }

    /// Modell, das **immer** denselben Werkzeugaufruf anfordert. Für den Test
    /// der Abbruchgrenze.
    struct LoopingToolModel;

    impl ModelProvider for LoopingToolModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            Box::pin(async move {
                Ok(ScriptedModel::tool_response(
                    WRITE_TOOL,
                    json!({"path": "loop.txt"}),
                ))
            })
        }
    }

    /// Werkzeug, das nur zählt, wie oft es tatsächlich ausgeführt wurde.
    struct CountingTool {
        executions: Arc<AtomicUsize>,
    }

    impl ToolExecutor for CountingTool {
        fn execute<'a>(
            &'a self,
            _context: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            let executions = Arc::clone(&self.executions);
            Box::pin(async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("written"))
            })
        }
    }

    struct CountingToolProvider {
        executions: Arc<AtomicUsize>,
    }

    impl ToolProvider for CountingToolProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new(WRITE_TOOL),
                description: "test-only write tool".to_owned(),
                parameters: JsonSchema::default(),
                strict: false,
            })]
        }

        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            if name.as_str() == WRITE_TOOL {
                Some(Arc::new(CountingTool {
                    executions: Arc::clone(&self.executions),
                }))
            } else {
                None
            }
        }
    }

    // ── Fixtures ─────────────────────────────────────────────────────────────

    /// Sandbox auf einem eindeutigen Temp-Verzeichnis (Muster aus `app.rs`).
    fn test_sandbox() -> TestResult<SandboxSpec> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-tui-approval-test-{}-{}",
            std::process::id(),
            id
        ));
        let created = std::fs::create_dir_all(root.join("workspace"));
        assert!(
            created.is_ok(),
            "temp workspace directory must be creatable"
        );

        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tui-approval-test"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("workspace registry must build for the approval tests"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tui-approval-test"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("test workspace must resolve"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace, Permission::WriteWorkspace]),
        ))
    }

    fn test_spawn_context() -> TestResult<SpawnContext> {
        Ok(SpawnContext {
            sandbox: test_sandbox()?,
            suggestions: None,
            capability_snapshot: None,
            approval_actor: Some(ApprovalActor::Operator {
                id: "tui-approval-test".to_owned(),
            }),
            organizational_role: AgentRoleId::RootOrchestrator,
            allowed_child_orchestrators: Vec::new(),
            // Generic fixture — not exercising trace propagation.
            trace: None,
            // Generic fixture — not exercising context-ceiling propagation.
            ceiling: None,
        })
    }

    /// Baut eine Session mit dem Zähl-Werkzeug und dem übergebenen Handler.
    fn test_session(
        handler: &Arc<TuiApprovalHandler>,
        executions: &Arc<AtomicUsize>,
    ) -> TestResult<AgentSession> {
        // Methodensyntax, nicht `Arc::clone(handler)`: bei der UFCS-Form würde
        // der Typparameter aus dem erwarteten Typ inferiert (`T = dyn
        // ApprovalHandler`) und bereits ein `&Arc<dyn ApprovalHandler>`
        // verlangt — die Unsized-Coercion käme zu spät. Über den Receiver steht
        // `T` fest, und die Coercion greift bei der Zuweisung.
        let registered: Arc<dyn ApprovalHandler> = handler.clone();
        let registry: ExtensionRegistry = ExtensionRegistry::builder()
            .tool_provider(Arc::new(CountingToolProvider {
                executions: Arc::clone(executions),
            }))
            .approval_handler(registered)
            .build();
        // Der Session-Event-Empfänger wird hier fallengelassen; die Session
        // sendet best-effort (`let _ = tx.send(..)`), ein geschlossener Kanal
        // ist damit unschädlich und für diese Tests bedeutungslos.
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        Ok(
            AgentSession::new(AgentRole::Assistant, None, registry, event_tx)
                .with_spawn_context(test_spawn_context()?),
        )
    }

    fn write_call(path: &str) -> Value {
        json!({ "path": path, "contents": "hello" })
    }

    /// Baut eine [`PendingApproval`] mit einem weit in der Zukunft liegenden
    /// `timeout_at` — für Tests, die den Treiber unabhängig von der
    /// kernseitigen Ablauffrist beobachten wollen.
    fn test_pending_approval(path: &str) -> TestResult<PendingApproval> {
        let requested_at = jiff::Timestamp::now();
        Ok(PendingApproval {
            call: ToolCall {
                id: ToolCallId::new(),
                name: ToolName::new(WRITE_TOOL),
                arguments: write_call(path),
            },
            request: ItemId::new(),
            actor: ApprovalActor::Operator {
                id: "tui-approval-test".to_owned(),
            },
            requested_at,
            timeout_at: requested_at
                .checked_add(jiff::SignedDuration::from_secs(3600))
                .map_err(ctx("one hour from now stays in range"))?,
        })
    }

    /// Treibt den Turn und beantwortet dabei jede eintreffende Frage.
    ///
    /// `answers` wird der Reihe nach verbraucht; ist die Liste erschöpft, wird
    /// die Frage **fallengelassen** statt beantwortet — das ist der Fall
    /// „UI geschlossen".
    async fn drive_answering(
        driver: &ApprovalDriver,
        session: &mut AgentSession,
        model: &dyn ModelProvider,
        store: &dyn StateStore,
        prompts: &mut ApprovalPromptReceiver,
        answers: Vec<Option<ApprovalResolution>>,
        outcome: TurnOutcome,
    ) -> (ApprovalDriverResult<TurnOutcome>, usize) {
        let mut answers = answers.into_iter();
        let mut asked = 0usize;
        let mut channel_open = true;

        let drive = driver.drive_to_completion(session, model, store, None, outcome);
        tokio::pin!(drive);

        loop {
            tokio::select! {
                result = &mut drive => return (result, asked),
                prompt = prompts.recv(), if channel_open => match prompt {
                    Some(prompt) => {
                        asked += 1;
                        match answers.next().flatten() {
                            Some(resolution) => { prompt.answer(resolution); }
                            // Kein Wert mehr: Frage fallenlassen ⇒ Ablehnung.
                            None => drop(prompt),
                        }
                    }
                    None => channel_open = false,
                },
            }
        }
    }

    // ── ApprovalScope ────────────────────────────────────────────────────────

    #[test]
    fn test_approval_scope_deferred_never_asks() {
        let scope = ApprovalScope::default();
        assert_eq!(scope, ApprovalScope::Deferred);
        assert!(!scope.requires_approval(WRITE_TOOL));
    }

    #[test]
    fn test_approval_scope_named_asks_only_listed_tools() {
        let scope = ApprovalScope::named([WRITE_TOOL]);
        assert!(scope.requires_approval(WRITE_TOOL));
        assert!(!scope.requires_approval("fs.read"));
    }

    #[test]
    fn test_approval_scope_all_tools_asks_everything() {
        assert!(ApprovalScope::AllTools.requires_approval("anything"));
    }

    // ── Handler ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_review_out_of_scope_call_allows_without_prompting() {
        let (handler, mut prompts) =
            TuiApprovalHandler::with_scope(ApprovalScope::named(["other"]));
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(WRITE_TOOL),
            arguments: Value::Null,
        };

        let decision = handler.review(&call).await;

        assert!(matches!(decision, ApprovalDecision::Allow));
        assert!(prompts.try_recv().is_err(), "no prompt must be issued");
        assert_eq!(handler.pending_len(), 0);
    }

    /// W1-08: `review` fragt nach einer Entscheidung, hat dabei aber **keinen**
    /// Seiteneffekt — kein Prompt, kein Eintrag in der Rückkanal-Tabelle.
    #[tokio::test]
    async fn test_review_in_scope_call_asks_user_without_side_effects() -> TestResult {
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(WRITE_TOOL),
            arguments: write_call("a.txt"),
        };

        let decision = handler.review(&call).await;

        let ApprovalDecision::AskUser(request) = decision else {
            return Err(TestError::Unexpected(
                "an in-scope call must ask the user".into(),
            ));
        };
        assert!(
            prompts.try_recv().is_err(),
            "review darf keine Frage senden — das tut erst der Treiber"
        );
        assert!(!handler.has_pending(&request));
        assert_eq!(handler.pending_len(), 0);
        Ok(())
    }

    /// Mehrfaches `review` (Handler-Aggregation im Kern, Vorprüfung mehrerer
    /// Calls) hinterlässt weiterhin nichts.
    #[tokio::test]
    async fn test_repeated_review_never_accumulates_prompts() {
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        for index in 0..5 {
            let call = ToolCall {
                id: ToolCallId::new(),
                name: ToolName::new(WRITE_TOOL),
                arguments: write_call(&format!("{index}.txt")),
            };
            let decision = handler.review(&call).await;
            assert!(matches!(decision, ApprovalDecision::AskUser(_)));
        }

        assert!(prompts.try_recv().is_err(), "kein Prompt darf entstehen");
        assert_eq!(handler.pending_len(), 0);
    }

    /// Der Treiber stellt die Frage aus dem Pausenzustand nach — genau einmal,
    /// auch wenn `review` sie nicht geöffnet hat.
    #[tokio::test]
    async fn test_driver_opens_the_prompt_that_review_no_longer_opens() -> TestResult {
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let driver = ApprovalDriver::new(Arc::clone(&handler));
        let pending = test_pending_approval("a.txt")?;

        // Der erste Poll öffnet die Frage; danach wartet der Treiber auf die
        // Antwort (der Zeitablauf hier ist nur das Poll-Signal, nicht der
        // Freigabe-Timeout des Handlers).
        let mut resolve = Box::pin(driver.resolve(&pending));
        let polled = tokio::time::timeout(Duration::from_millis(50), &mut resolve).await;
        assert!(
            polled.is_err(),
            "ohne Antwort darf der Treiber nicht fertig werden"
        );

        let Ok(prompt) = prompts.try_recv() else {
            return Err(TestError::Unexpected(
                "der Treiber muss die Frage nachstellen".into(),
            ));
        };
        assert_eq!(prompt.tool_name(), WRITE_TOOL);
        assert!(prompt.arguments_json().contains("a.txt"));
        assert!(prompt.approve());

        let resolution = resolve.await;
        assert!(matches!(resolution, ApprovalResolution::Approve));
        assert!(prompts.try_recv().is_err(), "genau eine Frage, nicht zwei");
        Ok(())
    }

    /// Eine bereits zu `timeout_at` abgelaufene Pause wird sofort als
    /// Zeitablauf abgelehnt — ohne eine neue Frage zu öffnen und ohne eine
    /// frische Wartezeit zu eröffnen (Interaktionsvertrag §4.4).
    #[tokio::test]
    async fn test_resolve_denies_immediately_when_the_core_deadline_already_passed() -> TestResult {
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let driver = ApprovalDriver::new(Arc::clone(&handler));
        let mut pending = test_pending_approval("a.txt")?;
        // `timeout_at` liegt bereits in der Vergangenheit.
        pending.timeout_at = pending
            .requested_at
            .checked_sub(jiff::SignedDuration::from_secs(1))
            .map_err(ctx("one second before requested_at stays in range"))?;

        let resolution = driver.resolve(&pending).await;

        assert_eq!(resolution, ApprovalResolution::timed_out());
        assert!(
            prompts.try_recv().is_err(),
            "eine bereits abgelaufene Pause darf keine neue Frage öffnen"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_review_denies_when_the_prompt_channel_is_already_closed() {
        let (handler, prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        drop(prompts); // Renderer weg — niemand kann mehr zustimmen.

        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(WRITE_TOOL),
            arguments: Value::Null,
        };
        let decision = handler.review(&call).await;

        assert!(
            matches!(decision, ApprovalDecision::Deny(_)),
            "a closed prompt channel must deny, never allow"
        );
        assert_eq!(
            handler.pending_len(),
            0,
            "the unusable responder must be cleaned up"
        );
    }

    #[tokio::test]
    async fn test_await_resolution_without_open_prompt_rejects() {
        let (handler, _prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);

        let resolution = handler.await_resolution(&ItemId::new()).await;

        assert!(matches!(resolution, ApprovalResolution::Reject { .. }));
    }

    #[tokio::test]
    async fn test_await_resolution_treats_a_dropped_prompt_as_rejection() -> TestResult {
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let request = ItemId::new();
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(WRITE_TOOL),
            arguments: Value::Null,
        };
        assert!(handler.open_prompt(&request, &call));

        let Ok(prompt) = prompts.try_recv() else {
            return Err(TestError::Unexpected("the prompt must be queued".into()));
        };
        drop(prompt); // UI schließt die Frage ohne Antwort.

        let resolution = handler.await_resolution(&request).await;

        let ApprovalResolution::Reject { reason } = resolution else {
            return Err(TestError::Unexpected(
                "a dropped prompt must never approve".into(),
            ));
        };
        assert_eq!(reason, REASON_ANSWER_DROPPED);
        Ok(())
    }

    #[tokio::test]
    async fn test_await_resolution_rejects_after_the_timeout_elapses() -> TestResult {
        let (handler, mut prompts) = TuiApprovalHandler::with_scope_and_timeout(
            ApprovalScope::AllTools,
            Duration::from_millis(20),
        );
        let request = ItemId::new();
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(WRITE_TOOL),
            arguments: Value::Null,
        };
        assert!(handler.open_prompt(&request, &call));
        // Die Frage bleibt im Kanal liegen — niemand antwortet.
        let queued = prompts.try_recv();
        assert!(queued.is_ok());

        let resolution = handler.await_resolution(&request).await;

        let ApprovalResolution::Reject { reason } = resolution else {
            return Err(TestError::Unexpected(
                "a timed-out prompt must never approve".into(),
            ));
        };
        assert_eq!(reason, harw_core::APPROVAL_TIMEOUT_REASON);
        Ok(())
    }

    /// `DEFAULT_APPROVAL_TIMEOUT` (TUI-Fallback) muss mit der kernseitigen
    /// Politik übereinstimmen — sonst driften zwei „Vorgabe"-Fristen
    /// auseinander, obwohl der Kern längst die einzige Quelle sein soll.
    #[test]
    fn test_default_approval_timeout_matches_the_core_policy() -> TestResult {
        let tui_default_secs =
            i64::try_from(DEFAULT_APPROVAL_TIMEOUT.as_secs()).map_err(ctx("300 fits in i64"))?;
        assert_eq!(
            jiff::SignedDuration::from_secs(tui_default_secs),
            harw_core::DEFAULT_APPROVAL_TIMEOUT
        );
        Ok(())
    }

    // ── Treiber: Freigabe / Ablehnung ────────────────────────────────────────

    #[tokio::test]
    async fn test_approved_call_runs_the_tool_and_completes_the_turn() -> TestResult {
        let executions = Arc::new(AtomicUsize::new(0));
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let mut session = test_session(&handler, &executions)?;
        let store = InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            ScriptedModel::tool_response(WRITE_TOOL, write_call("a.txt")),
            ModelResponse::text("file written"),
        ]);
        let driver = ApprovalDriver::new(Arc::clone(&handler));

        let Ok(outcome) = run_turn(&mut session, &model, &store, TurnInput::user("write a")).await
        else {
            return Err(TestError::Unexpected(
                "the first turn leg must reach the approval pause".into(),
            ));
        };
        assert!(matches!(outcome, TurnOutcome::AwaitingApproval { .. }));

        let (result, asked) = drive_answering(
            &driver,
            &mut session,
            &model,
            &store,
            &mut prompts,
            vec![Some(ApprovalResolution::Approve)],
            outcome,
        )
        .await;

        let Ok(final_outcome) = result else {
            return Err(TestError::Unexpected("an approved turn must finish".into()));
        };
        assert!(matches!(final_outcome, TurnOutcome::Completed));
        assert_eq!(asked, 1);
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "an approved tool must run exactly once"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_rejected_call_ends_the_turn_without_running_the_tool() -> TestResult {
        let executions = Arc::new(AtomicUsize::new(0));
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let mut session = test_session(&handler, &executions)?;
        let store = InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            ScriptedModel::tool_response(WRITE_TOOL, write_call("a.txt")),
            ModelResponse::text("understood, nothing written"),
        ]);
        let driver = ApprovalDriver::new(Arc::clone(&handler));

        let Ok(outcome) = run_turn(&mut session, &model, &store, TurnInput::user("write a")).await
        else {
            return Err(TestError::Unexpected(
                "the first turn leg must reach the approval pause".into(),
            ));
        };

        let (result, asked) = drive_answering(
            &driver,
            &mut session,
            &model,
            &store,
            &mut prompts,
            vec![Some(ApprovalResolution::Reject {
                reason: "not this time".to_owned(),
            })],
            outcome,
        )
        .await;

        let Ok(final_outcome) = result else {
            return Err(TestError::Unexpected(
                "a rejected turn must still finish cleanly".into(),
            ));
        };
        assert!(matches!(final_outcome, TurnOutcome::Completed));
        assert_eq!(asked, 1);
        assert_eq!(
            executions.load(Ordering::SeqCst),
            0,
            "a rejected tool must never run"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_closed_prompt_channel_counts_as_rejection_and_still_ends_the_turn() -> TestResult
    {
        // Der wichtigste Test dieser Datei: keine UI ⇒ keine Freigabe.
        let executions = Arc::new(AtomicUsize::new(0));
        let (handler, prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let mut session = test_session(&handler, &executions)?;
        let store = InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            ScriptedModel::tool_response(WRITE_TOOL, write_call("a.txt")),
            ModelResponse::text("denied, nothing written"),
        ]);
        let _driver = ApprovalDriver::new(Arc::clone(&handler));

        drop(prompts); // Renderer beendet, bevor der Turn startet.

        let Ok(outcome) = run_turn(&mut session, &model, &store, TurnInput::user("write a")).await
        else {
            return Err(TestError::Unexpected(
                "the turn must survive a dead prompt channel".into(),
            ));
        };

        // Der Handler hat inline abgelehnt (`Deny`), der Turn ist deshalb gar
        // nicht erst pausiert — und das Werkzeug lief nicht.
        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(
            executions.load(Ordering::SeqCst),
            0,
            "a closed prompt channel must never lead to execution"
        );

        // Und auch der Treiber selbst hätte eine Pause abgelehnt:
        let resolution = handler.await_resolution(&ItemId::new()).await;
        assert!(matches!(resolution, ApprovalResolution::Reject { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn test_prompt_dropped_mid_turn_rejects_and_ends_the_turn() -> TestResult {
        let executions = Arc::new(AtomicUsize::new(0));
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let mut session = test_session(&handler, &executions)?;
        let store = InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            ScriptedModel::tool_response(WRITE_TOOL, write_call("a.txt")),
            ModelResponse::text("nothing written"),
        ]);
        let driver = ApprovalDriver::new(Arc::clone(&handler));

        let Ok(outcome) = run_turn(&mut session, &model, &store, TurnInput::user("write a")).await
        else {
            return Err(TestError::Unexpected(
                "the first turn leg must reach the approval pause".into(),
            ));
        };

        // `answers` ist leer ⇒ die Frage wird fallengelassen.
        let (result, asked) = drive_answering(
            &driver,
            &mut session,
            &model,
            &store,
            &mut prompts,
            vec![None],
            outcome,
        )
        .await;

        let Ok(final_outcome) = result else {
            return Err(TestError::Unexpected(
                "a dropped prompt must still end the turn".into(),
            ));
        };
        assert!(matches!(final_outcome, TurnOutcome::Completed));
        assert_eq!(asked, 1);
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        Ok(())
    }

    // ── Treiber: Schleife ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_two_approvals_in_one_turn_both_run_through_the_loop() -> TestResult {
        let executions = Arc::new(AtomicUsize::new(0));
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let mut session = test_session(&handler, &executions)?;
        let store = InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            ScriptedModel::tool_response(WRITE_TOOL, write_call("first.txt")),
            ScriptedModel::tool_response(WRITE_TOOL, write_call("second.txt")),
            ModelResponse::text("both files written"),
        ]);
        let driver = ApprovalDriver::new(Arc::clone(&handler));

        let Ok(outcome) =
            run_turn(&mut session, &model, &store, TurnInput::user("write both")).await
        else {
            return Err(TestError::Unexpected(
                "the first turn leg must reach the approval pause".into(),
            ));
        };

        let (result, asked) = drive_answering(
            &driver,
            &mut session,
            &model,
            &store,
            &mut prompts,
            vec![
                Some(ApprovalResolution::Approve),
                Some(ApprovalResolution::Approve),
            ],
            outcome,
        )
        .await;

        let Ok(final_outcome) = result else {
            return Err(TestError::Unexpected(
                "both approvals must drive the turn to completion".into(),
            ));
        };
        assert!(matches!(final_outcome, TurnOutcome::Completed));
        assert_eq!(asked, 2, "the loop must ask a second time, not just once");
        assert_eq!(
            executions.load(Ordering::SeqCst),
            2,
            "both approved writes must run"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_resume_limit_trips_and_reports_a_clear_error() -> TestResult {
        let executions = Arc::new(AtomicUsize::new(0));
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let mut session = test_session(&handler, &executions)?;
        let store = InMemoryStateStore::new();
        let model = LoopingToolModel;
        let driver = ApprovalDriver::with_max_resumes(Arc::clone(&handler), 3);

        let Ok(outcome) = run_turn(&mut session, &model, &store, TurnInput::user("loop")).await
        else {
            return Err(TestError::Unexpected(
                "the first turn leg must reach the approval pause".into(),
            ));
        };

        // Jede Frage wird abgelehnt; das Modell fragt trotzdem erneut.
        let rejections = vec![
            Some(ApprovalResolution::Reject {
                reason: "no".to_owned(),
            });
            8
        ];
        let (result, asked) = drive_answering(
            &driver,
            &mut session,
            &model,
            &store,
            &mut prompts,
            rejections,
            outcome,
        )
        .await;

        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "an endlessly pausing turn must not spin forever".into(),
            ));
        };
        let ApprovalDriverError::ResumeLimitExceeded { limit, last_pause } = error else {
            return Err(TestError::Unexpected(
                "the limit must be reported as its own error variant".into(),
            ));
        };
        assert_eq!(limit, 3);
        assert_eq!(last_pause, "awaiting approval");
        // Drei Fragen werden beantwortet; die vierte kann noch im Kanal liegen,
        // wenn `select!` sie vor dem Fehlerergebnis abholt. Mehr als vier wäre
        // ein Zeichen, dass die Grenze nicht greift.
        assert!(
            (3..=4).contains(&asked),
            "the loop must ask once per resume and then stop, got {asked}"
        );
        assert_eq!(
            executions.load(Ordering::SeqCst),
            0,
            "no rejected tool may run"
        );
        Ok(())
    }

    // ── Fehlerdarstellung ────────────────────────────────────────────────────

    #[test]
    fn test_resume_limit_error_message_names_limit_and_pause() {
        let error = ApprovalDriverError::ResumeLimitExceeded {
            limit: 7,
            last_pause: "awaiting child",
        };
        let rendered = error.to_string();
        assert!(rendered.contains('7'));
        assert!(rendered.contains("awaiting child"));
        assert_eq!(format!("{error:?}"), rendered, "Debug delegates to Display");
    }

    #[test]
    fn test_missing_pending_approval_error_names_the_session() {
        let error = ApprovalDriverError::MissingPendingApproval {
            session: "session-42".to_owned(),
        };
        assert!(error.to_string().contains("session-42"));
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn test_resume_error_exposes_the_core_error_as_source() {
        let error = ApprovalDriverError::Resume {
            stage: ResumeStage::Child,
            source: CoreError::TurnRejected("nope".to_owned()),
        };
        assert!(error.to_string().contains("child resume"));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn test_pause_label_names_every_outcome() {
        assert_eq!(pause_label(&TurnOutcome::Completed), "completed");
        assert_eq!(
            pause_label(&TurnOutcome::AwaitingApproval {
                call_id: ToolCallId::new(),
                request: ItemId::new(),
            }),
            "awaiting approval"
        );
        assert_eq!(
            pause_label(&TurnOutcome::AwaitingChild {
                child: SessionId::new(),
                call_id: ToolCallId::new(),
                role: "worker".to_owned(),
            }),
            "awaiting child"
        );
    }

    // ── Kind-Treiber ─────────────────────────────────────────────────────────

    /// Attrappe: liefert ein festes Kind-Ergebnis, ohne einen Spawner.
    struct StubChildDriver {
        calls: AtomicUsize,
    }

    impl ChildTurnDriver for StubChildDriver {
        fn drive_child<'a>(
            &'a self,
            _child: &'a SessionId,
            _store: &'a dyn StateStore,
        ) -> ChildDriveFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(ToolCallResult::success(json!({"child": "done"}))) })
        }
    }

    #[tokio::test]
    async fn test_child_result_without_a_driver_is_an_error_result_not_a_hang() -> TestResult {
        let store = InMemoryStateStore::new();
        let result =
            ApprovalDriver::child_result(None, &SessionId::new(), "reviewer", &store).await;

        let ToolCallResult::Error { message } = result else {
            return Err(TestError::Unexpected(
                "a missing child driver must produce an error result".into(),
            ));
        };
        assert!(message.contains("reviewer"));
        Ok(())
    }

    #[tokio::test]
    async fn test_child_result_uses_the_configured_driver() {
        let store = InMemoryStateStore::new();
        let driver = StubChildDriver {
            calls: AtomicUsize::new(0),
        };

        let result =
            ApprovalDriver::child_result(Some(&driver), &SessionId::new(), "reviewer", &store)
                .await;

        assert!(result.is_success());
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_child_pause_resumes_the_parent_turn_through_the_same_loop() -> TestResult {
        let executions = Arc::new(AtomicUsize::new(0));
        let (handler, _prompts) = TuiApprovalHandler::new();
        let mut session = test_session(&handler, &executions)?;
        let store = InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![ModelResponse::text("parent turn resumed")]);
        let driver = ApprovalDriver::new(Arc::clone(&handler));
        let children = StubChildDriver {
            calls: AtomicUsize::new(0),
        };

        // Session künstlich in eine Handoff-Pause versetzen (wie es der Kern
        // nach einem `transfer_to_*`-Aufruf täte).
        let started = session.try_start_turn();
        assert!(started.is_ok(), "a fresh session must start a turn");
        let child = SessionId::new();
        let call_id = ToolCallId::new();
        let handoff = session.begin_handoff(child.clone(), call_id.clone(), "reviewer".to_owned());
        assert!(handoff.is_ok(), "the handoff must pause the session");

        let outcome = TurnOutcome::AwaitingChild {
            child,
            call_id,
            role: "reviewer".to_owned(),
        };
        let result = driver
            .drive_to_completion(&mut session, &model, &store, Some(&children), outcome)
            .await;

        let Ok(final_outcome) = result else {
            return Err(TestError::Unexpected(
                "a returning child must drive the parent turn to completion".into(),
            ));
        };
        assert!(matches!(final_outcome, TurnOutcome::Completed));
        assert_eq!(children.calls.load(Ordering::SeqCst), 1);
        Ok(())
    }

    // ── Konstruktion ─────────────────────────────────────────────────────────

    #[test]
    fn test_driver_defaults_to_the_named_resume_limit() {
        let (handler, _prompts) = TuiApprovalHandler::new();
        let driver = ApprovalDriver::new(Arc::clone(&handler));
        assert_eq!(driver.max_resumes(), MAX_CONSECUTIVE_RESUMES);
        assert_eq!(driver.handler().timeout(), DEFAULT_APPROVAL_TIMEOUT);
    }

    #[test]
    fn test_handler_debug_output_never_contains_arguments() -> TestResult {
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        let request = ItemId::new();
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(WRITE_TOOL),
            arguments: json!({"secret": "swordfish"}),
        };
        assert!(handler.open_prompt(&request, &call));

        let Ok(prompt) = prompts.try_recv() else {
            return Err(TestError::Unexpected("the prompt must be queued".into()));
        };
        let rendered = format!("{prompt:?}");
        assert!(
            !rendered.contains("swordfish"),
            "arguments must be redacted"
        );
        assert!(rendered.contains(WRITE_TOOL));
        assert!(format!("{handler:?}").contains("AllTools"));
        Ok(())
    }
}
