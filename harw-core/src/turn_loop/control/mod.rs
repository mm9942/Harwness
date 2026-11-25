//! Turn control, cancellation and shared metering.

use super::*;

/// Grenzwerte eines einzelnen Turns (W4a A-LOOP).
///
/// # Description
/// Feldgleiche Kernfassung von `harw_runtime::TurnLimits`
/// (`harw-runtime/src/assembly.rs`): `harw-runtime` hängt an `harw-core`, der
/// Turn-Loop kann den Laufzeittyp deshalb nicht importieren. Die Laufzeit
/// überträgt ihre abgeleiteten Werte feldweise (Folgearbeit im Ledger
/// `W4a/A-LOOP.md`). Durchgesetzt wird in [`run_turn`] und den
/// `resume_*`-Einstiegen: vor jedem Modellaufruf und vor jeder
/// Werkzeugausführung.
///
/// # Concurrency
/// `Copy`-Datenhalter ohne innere Veränderlichkeit.
///
/// # Examples
/// ```rust
/// use harw_core::turn_loop::TurnLimits;
///
/// let limits = TurnLimits { max_model_rounds: 4, ..TurnLimits::unlimited() };
/// assert_eq!(limits.max_tool_calls, u32::MAX);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnLimits {
    /// Maximale Anzahl Modell-Runden eines Turns.
    pub max_model_rounds: u32,
    /// Maximale Anzahl Werkzeugaufrufe (inklusive Handoffs) eines Turns.
    pub max_tool_calls: u32,
    /// Maximale Gesamtzahl erzeugter Ausgabe-Tokens.
    pub max_output_tokens_total: u64,
    /// Maximale Wanduhrzeit eines Turns (monoton gemessen, ab Turn-Start).
    pub wall_time: Duration,
    /// Obergrenze eines einzelnen gerenderten Werkzeugergebnisses in Bytes;
    /// `usize::MAX` heißt „keine Kappung" und wird dem Provider nicht gemeldet.
    pub tool_result_max_bytes: usize,
}

impl TurnLimits {
    /// Returns limits that never trip (every field at its type maximum).
    ///
    /// # Returns
    /// A [`TurnLimits`] equal to [`TurnLimits::default`].
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            max_model_rounds: u32::MAX,
            max_tool_calls: u32::MAX,
            max_output_tokens_total: u64::MAX,
            wall_time: Duration::MAX,
            tool_result_max_bytes: usize::MAX,
        }
    }

    // Provider-Hinweis für `ModelRequest::tool_result_max_bytes`; unbegrenzt ⇒ `None`.
    pub(super) fn tool_result_max_bytes_hint(&self) -> Option<usize> {
        (self.tool_result_max_bytes != usize::MAX).then_some(self.tool_result_max_bytes)
    }
}

impl Default for TurnLimits {
    fn default() -> Self {
        Self::unlimited()
    }
}

// Zähler eines Turns, geteilt zwischen allen Klonen eines `TurnControl`.
#[derive(Debug, Default)]
struct TurnMeter {
    started: Option<Instant>,
    model_rounds: u32,
    tool_calls: u32,
    usage: TokenUsage,
    /// Ob die Abschluss-Anweisung des Token-Budgets bereits ausgelöst wurde
    /// (nur für das einmalige Log; die Anweisung selbst bleibt danach aktiv).
    wrap_up_announced: bool,
    /// Konkreter Grund eines `CancelReason::Budget`-Endes (welche Grenze mit
    /// Wert und Verbrauch bzw. welcher Turn-Wächter); der erste gewinnt.
    /// Siehe [`TurnControl::stop_detail`].
    stop_detail: Option<String>,
    /// Sitzungsweites Werkzeug-Budget eines Kindes als `(limit, used_before)`
    /// (siehe [`TurnControl::with_tool_call_budget`]); `None` = keins.
    tool_call_budget: Option<(u32, u32)>,
    /// Werkzeugaufrufe, die wegen des Werkzeug-Budgets nicht ausgeführt,
    /// sondern mit [`TOOL_BUDGET_SKIP_PREFIX`] beantwortet wurden.
    tool_budget_skipped: u32,
}

/// Präfix des synthetischen Ergebnisses eines Werkzeugaufrufs, der wegen
/// eines erschöpften Werkzeug-Budgets nicht ausgeführt wurde.
pub const TOOL_BUDGET_SKIP_PREFIX: &str = "nicht ausgeführt: Werkzeug-Budget erschöpft";

/// Präfix des Ergebnisses eines Werkzeugaufrufs, den der [`ToolCallMeter`]
/// abgewiesen hat; danach folgt `": "` und der Grund des Meters.
pub const TOOL_CALL_REFUSED_PREFIX: &str = "nicht ausgeführt: Werkzeugaufruf abgewiesen";

/// Vorgabe-Schwelle (Prozent des Limits), ab der ein Turn mit
/// [`TurnTokenBudget`] die Abschluss-Anweisung
/// ([`TOKEN_BUDGET_WRAP_UP_INSTRUCTION`]) in jede weitere Anfrage legt.
pub const DEFAULT_TOKEN_BUDGET_WRAP_UP_PERCENT: u64 = 80;

/// Abschluss-Anweisung, sobald ein Token-Budget zu
/// [`TurnTokenBudget::wrap_up_percent`] verbraucht ist.
pub const TOKEN_BUDGET_WRAP_UP_INSTRUCTION: &str = "Dein Token-Budget für diesen Auftrag ist \
fast aufgebraucht. Beginne keine neuen Erkundungen und rufe nur noch Werkzeuge auf, wenn sie für \
den Abschluss unverzichtbar sind. Fasse jetzt zusammen, was du herausgefunden hast, und liefere \
dein Ergebnis im verlangten Format ab; offene Punkte nennst du ausdrücklich als offen.";

/// Token-Budget eines Turns, gemessen in **neuen** Tokens (Teil C).
///
/// # Beschreibung
/// Gezählt wird [`TokenUsage::fresh_tokens`] jeder Modellrunde: ungecachte
/// Eingabe plus Ausgabe. Ein erneut gesendeter, gecachter Verlauf zählt
/// damit nicht jede Runde erneut — das alte Maß
/// (`input_tokens + output_tokens` kumuliert) ließ ein Kind mit 60 000
/// Tokens Budget schon nach wenigen Runden bei „210 426 verbraucht"
/// scheitern. `used_before` trägt den Verbrauch früherer Turns derselben
/// Sitzung (das Budget gilt für die ganze Kindsitzung).
///
/// Geprüft wird vor **jeder** Modellrunde ([`TurnControl`]-Prüfpunkt):
/// - ab `wrap_up_percent` % legt der Turn die Abschluss-Anweisung
///   [`TOKEN_BUDGET_WRAP_UP_INSTRUCTION`] in jede weitere Anfrage;
/// - ab 100 % endet der Turn wie jede andere Grenze mit
///   `TurnOutcome::Cancelled { reason: CancelReason::Budget }`; der Aufrufer
///   (`ManagedAgentSpawner`) macht daraus ein Teilergebnis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnTokenBudget {
    /// Obergrenze der neuen Tokens (inklusive `used_before`).
    pub limit: u64,
    /// Bereits vor diesem Turn verbrauchte neue Tokens der Sitzung.
    pub used_before: u64,
    /// Schwelle der Abschluss-Anweisung in Prozent von `limit`.
    pub wrap_up_percent: u64,
}

impl TurnTokenBudget {
    /// Budget mit `limit`, Vorverbrauch `used_before` und der Vorgabe-Schwelle
    /// [`DEFAULT_TOKEN_BUDGET_WRAP_UP_PERCENT`].
    #[must_use]
    pub const fn new(limit: u64, used_before: u64) -> Self {
        Self {
            limit,
            used_before,
            wrap_up_percent: DEFAULT_TOKEN_BUDGET_WRAP_UP_PERCENT,
        }
    }

    /// Ersetzt die Schwelle der Abschluss-Anweisung (Prozent, auf 100 gekappt).
    #[must_use]
    pub const fn with_wrap_up_percent(mut self, percent: u64) -> Self {
        self.wrap_up_percent = if percent > 100 { 100 } else { percent };
        self
    }

    /// `true`, wenn `used` die Abschluss-Schwelle erreicht.
    #[must_use]
    pub fn wrap_up_reached(&self, used: u64) -> bool {
        u128::from(used) * 100 >= u128::from(self.limit) * u128::from(self.wrap_up_percent)
    }

    /// `true`, wenn `used` das Limit erreicht.
    #[must_use]
    pub const fn exhausted(&self, used: u64) -> bool {
        used >= self.limit
    }
}

/// Steuerblock eines Turns: Abbruch, Grenzwerte, Serveruhr und Zähler.
///
/// # Description
/// Ein `TurnControl` gehört zu **genau einem** Turn. Klone teilen denselben
/// [`CancelToken`]-Knoten und denselben Zähler; wer einen pausierten Turn mit
/// `resume_*` fortsetzt, übergibt deshalb einen Klon des Blocks, mit dem der
/// Turn gestartet wurde — nur dann zählen Runden, Aufrufe, Nutzung und
/// Wanduhr über die Pause hinweg weiter. Ein frischer Block setzt die Zähler
/// zurück.
///
/// Die Uhr liefert Zeitstempel für dauerhafte Freigaben (`issued_at`,
/// `ApprovalStore::resolve`) und Kind-Abschlüsse; die Wanduhrgrenze misst
/// dagegen monoton über [`Instant`].
///
/// # Concurrency
/// `Send + Sync`; der Zähler liegt hinter einem `Mutex`, der nie über einen
/// `.await` gehalten wird.
///
/// # Examples
/// ```rust
/// use harw_core::cancel::{CancelReason, CancelToken};
/// use harw_core::turn_loop::{TurnControl, TurnInput, TurnLimits};
///
/// let token = CancelToken::new();
/// let control = TurnControl::new()
///     .with_cancel(token.clone())
///     .with_limits(TurnLimits { max_model_rounds: 8, ..TurnLimits::unlimited() });
/// let input = TurnInput::user("hallo").with_control(control.clone());
/// token.cancel(CancelReason::User);
/// assert_eq!(input.control.cancel_token().reason(), Some(CancelReason::User));
/// ```
#[derive(Clone)]
pub struct TurnControl {
    cancel: CancelToken,
    limits: TurnLimits,
    clock: Arc<dyn Clock>,
    meter: Arc<Mutex<TurnMeter>>,
    token_budget: Option<TurnTokenBudget>,
    // Bucht jeden Werkzeugaufruf vor seiner Ausführung (siehe
    // `crate::tool_call_meter`); Klone teilen denselben Meter.
    tool_call_meter: Option<Arc<dyn ToolCallMeter>>,
    // The host receives a signal only after the complete rewind frame is durable.
    history_replaced: Option<Arc<dyn Fn() + Send + Sync>>,
    rewind_media: Option<Arc<RewindMediaCheck>>,
    rewrite_replacement_id: Option<harw_types::ItemId>,
    // Only the trusted hosted driver sets source provenance; not model metadata.
    title_seed_human: Option<bool>,
    title_seed_pending: bool,
    // PL-100 Phase 4 (B22): DoD-Taint dieses Turns (siehe
    // `crate::dod_taint`); Klone teilen denselben Zustand.
    dod_taint: crate::dod_taint::DodTaint,
}

/// Trusted hosted source check; never receives a client-supplied reference.
pub type RewindMediaCheck = dyn Fn(&harw_protocol::MediaRef) -> bool + Send + Sync;

impl std::fmt::Debug for TurnControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnControl")
            .field("cancel", &self.cancel)
            .field("limits", &self.limits)
            .field("token_budget", &self.token_budget)
            .field("tool_call_meter", &self.tool_call_meter.is_some())
            .field("dod_taint", &self.dod_taint)
            .finish_non_exhaustive()
    }
}

impl Default for TurnControl {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnControl {
    pub(super) fn rewind_media(&self) -> Option<Arc<RewindMediaCheck>> {
        self.rewind_media.clone()
    }

    pub(super) fn rewrite_replacement_id(&self) -> Option<harw_types::ItemId> {
        self.rewrite_replacement_id.clone()
    }

    pub(super) fn notify_history_replaced(&self) {
        if let Some(notify) = &self.history_replaced {
            notify();
        }
    }

    pub(super) fn title_seed_human(&self) -> Option<bool> {
        self.title_seed_human
    }

    pub(super) fn title_seed_pending(&self) -> bool {
        self.title_seed_pending
    }

    pub(super) fn set_title_seed_pending(&mut self, pending: bool) {
        self.title_seed_pending = pending;
    }

    pub(super) fn tool_call_meter(&self) -> Option<&Arc<dyn ToolCallMeter>> {
        self.tool_call_meter.as_ref()
    }

    /// Creates a control block with a fresh token, unlimited limits and the system clock.
    ///
    /// # Returns
    /// A new [`TurnControl`] whose counters are all zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancel: CancelToken::new(),
            limits: TurnLimits::unlimited(),
            clock: Arc::new(SystemClock),
            meter: Arc::new(Mutex::new(TurnMeter::default())),
            token_budget: None,
            tool_call_meter: None,
            history_replaced: None,
            rewind_media: None,
            rewrite_replacement_id: None,
            title_seed_human: None,
            title_seed_pending: false,
            dod_taint: crate::dod_taint::DodTaint::new(),
        }
    }

    /// Mark the origin of an actual hosted user input. The durable store still
    /// refuses legacy, imported, forked, and otherwise uninitialized sessions.
    #[must_use]
    pub fn with_hosted_title_source(mut self, human: bool) -> Self {
        self.title_seed_human = Some(human);
        self
    }

    /// Installs the hosted driver's post-commit notification for an exact history edit.
    #[must_use]
    pub fn with_history_replaced(mut self, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.history_replaced = Some(notify);
        self
    }

    /// Uses the canonical replacement identity derived by the hosted driver.
    #[must_use]
    pub fn with_rewrite_replacement_id(mut self, id: Option<harw_types::ItemId>) -> Self {
        self.rewrite_replacement_id = id;
        self
    }

    /// Verifies target-owned stored prompt media before replacing history.
    #[must_use]
    pub fn with_rewind_media_check(mut self, check: Arc<RewindMediaCheck>) -> Self {
        self.rewind_media = Some(check);
        self
    }

    /// Der DoD-Taint dieses Turns (PL-100 Phase 4, Bedrohung B22).
    ///
    /// # Beschreibung
    /// Der Turn-Loop vermerkt darin jeden abgefertigten `dod.*`-Aufruf;
    /// `check_approval` liest ihn für die
    /// [`crate::policy::DodTaintPolicy`]. Eine Oberfläche, die den Block
    /// selbst gebaut hat, kann darüber den Grund einer Rückfrage anzeigen.
    ///
    /// # Returns
    /// Den geteilten [`crate::dod_taint::DodTaint`]; Klone dieses Blocks sehen
    /// denselben Zustand.
    #[must_use]
    pub fn dod_taint(&self) -> &crate::dod_taint::DodTaint {
        &self.dod_taint
    }

    /// Registriert den [`ToolCallMeter`], der jeden Werkzeugaufruf bucht und
    /// zulässt, bevor er ausgeführt wird.
    ///
    /// # Beschreibung
    /// Der Meter gilt für alle Ausführungswege des Turns (sequenziell,
    /// parallel und die Fortsetzung nach einer Rückfrage) und überlebt eine
    /// Pause mit dem Steuerblock. Den [`ToolCallPermit`] einer Zulassung
    /// hält der Turn-Loop, bis das Ergebnis des Aufrufs feststeht. Eine
    /// Abweisung beendet den Turn mit `CancelReason::Budget`; die
    /// Einzelheiten stehen in [`crate::tool_call_meter`].
    ///
    /// # Arguments
    /// - `meter` (`Arc<dyn ToolCallMeter>`): geteilter Meter des Aufrufers.
    #[must_use]
    pub fn with_tool_call_meter(mut self, meter: Arc<dyn ToolCallMeter>) -> Self {
        self.tool_call_meter = Some(meter);
        self
    }

    /// Setzt ein Token-Budget über neue, ungecachte Tokens
    /// ([`TurnTokenBudget`]), das vor jeder Modellrunde geprüft wird.
    ///
    /// # Arguments
    /// - `budget` (`TurnTokenBudget`): Limit, Vorverbrauch und
    ///   Abschluss-Schwelle.
    #[must_use]
    pub fn with_token_budget(mut self, budget: TurnTokenBudget) -> Self {
        self.token_budget = Some(budget);
        self
    }

    /// Das gesetzte Token-Budget, falls vorhanden.
    #[must_use]
    pub fn token_budget(&self) -> Option<TurnTokenBudget> {
        self.token_budget
    }

    /// Neue Tokens dieses Turns plus Vorverbrauch des Budgets
    /// ([`TurnTokenBudget::used_before`], `0` ohne Budget).
    #[must_use]
    pub fn fresh_tokens_used(&self) -> u64 {
        let own = self.meter().usage.fresh_tokens();
        self.token_budget
            .map_or(own, |budget| budget.used_before.saturating_add(own))
    }

    /// `true`, wenn ein gesetztes Token-Budget erschöpft ist.
    #[must_use]
    pub fn token_budget_exhausted(&self) -> bool {
        self.token_budget
            .is_some_and(|budget| budget.exhausted(self.fresh_tokens_used()))
    }

    /// `true`, sobald ein gesetztes Token-Budget die Abschluss-Schwelle
    /// erreicht hat; die erste Feststellung wird einmal geloggt.
    #[must_use]
    pub fn token_budget_wrap_up_due(&self) -> bool {
        let Some(budget) = self.token_budget else {
            return false;
        };
        let used = self.fresh_tokens_used();
        if !budget.wrap_up_reached(used) {
            return false;
        }
        let mut meter = self.meter();
        if !meter.wrap_up_announced {
            meter.wrap_up_announced = true;
            tracing::info!(
                limit = budget.limit,
                used,
                percent = budget.wrap_up_percent,
                "turn_loop.token_budget_wrap_up"
            );
        }
        true
    }

    /// Replaces the cancellation token (e.g. a child of the caller's token).
    ///
    /// # Arguments
    /// - `cancel` (`CancelToken`): token observed at every checkpoint.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Replaces the turn limits.
    ///
    /// # Arguments
    /// - `limits` (`TurnLimits`): enforced before model calls and tool executions.
    #[must_use]
    pub fn with_limits(mut self, limits: TurnLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Replaces the server clock used for approval and child timestamps.
    ///
    /// # Arguments
    /// - `clock` (`Arc<dyn Clock>`): shared clock; tests pass a fixed clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Returns the cancellation token observed by this turn.
    #[must_use]
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Returns the enforced limits.
    #[must_use]
    pub fn limits(&self) -> &TurnLimits {
        &self.limits
    }

    /// Returns the server clock.
    #[must_use]
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Returns the token usage accumulated over all model rounds so far.
    ///
    /// # Concurrency
    /// Briefly locks the shared counter.
    #[must_use]
    pub fn usage(&self) -> TokenUsage {
        self.meter().usage.clone()
    }

    /// Returns the number of model rounds started so far.
    #[must_use]
    pub fn model_rounds(&self) -> u32 {
        self.meter().model_rounds
    }

    /// Returns the number of tool calls (including handoffs) dispatched so far.
    #[must_use]
    pub fn tool_calls(&self) -> u32 {
        self.meter().tool_calls
    }

    // Ein vergifteter Mutex hält nur Zähler: der letzte Stand bleibt gültig.
    fn meter(&self) -> MutexGuard<'_, TurnMeter> {
        self.meter.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // Startet die Wanduhr beim ersten Aufruf; weitere Aufrufe ändern nichts.
    pub(super) fn start(&self) {
        let mut meter = self.meter();
        if meter.started.is_none() {
            meter.started = Some(Instant::now());
        }
    }

    // Addiert die Nutzung einer Modellrunde.
    pub(super) fn record_usage(&self, usage: &TokenUsage) {
        self.meter().usage.add(usage);
    }

    // Zählt eine begonnene Modellrunde.
    pub(super) fn record_model_round(&self) {
        let mut meter = self.meter();
        meter.model_rounds = meter.model_rounds.saturating_add(1);
    }

    /// Setzt ein sitzungsweites Werkzeug-Budget (Kind-`max_tool_calls`):
    /// `limit` Aufrufe insgesamt, davon `used_before` vor diesem Turn.
    ///
    /// # Beschreibung
    /// Anders als [`TurnLimits::max_tool_calls`] (verwirft eine zu große
    /// Antwort ganz) wird eine Antwort mit mehr Aufrufen als dem Rest
    /// **vor** dem Dispatch geteilt: höchstens der Rest wird ausgeführt, die
    /// übrigen bekommen ein Ergebnis mit [`TOOL_BUDGET_SKIP_PREFIX`], und der
    /// Turn endet nach dieser Runde mit `CancelReason::Budget`. Der Zähler
    /// liegt im geteilten Meter, gilt also für alle Klone.
    #[must_use]
    pub fn with_tool_call_budget(self, limit: u32, used_before: u32) -> Self {
        self.meter().tool_call_budget = Some((limit, used_before));
        self
    }

    /// Anzahl der wegen des Werkzeug-Budgets nicht ausgeführten Aufrufe.
    #[must_use]
    pub fn tool_budget_skipped(&self) -> u32 {
        self.meter().tool_budget_skipped
    }

    // Teilt `calls` am Rest des Werkzeug-Budgets: behält die ersten
    // `rest` Aufrufe und liefert die übrigen samt Ergebnistext.
    pub(super) fn split_over_tool_budget(&self, calls: &mut Vec<ToolCall>) -> Option<(Vec<ToolCall>, String)> {
        let mut meter = self.meter();
        let (limit, used_before) = meter.tool_call_budget?;
        let used = used_before.saturating_add(meter.tool_calls);
        let rest = usize::try_from(limit.saturating_sub(used)).unwrap_or(usize::MAX);
        if calls.len() <= rest {
            return None;
        }
        let skipped = calls.split_off(rest);
        let count = u32::try_from(skipped.len()).unwrap_or(u32::MAX);
        meter.tool_budget_skipped = meter.tool_budget_skipped.saturating_add(count);
        let executed = used.saturating_add(u32::try_from(rest).unwrap_or(u32::MAX));
        meter.stop_detail.get_or_insert_with(|| {
            format!("Werkzeug-Budget erschöpft ({executed} von {limit} Aufrufen)")
        });
        tracing::warn!(
            limit,
            executed,
            skipped = count,
            "turn_loop.tool_budget_split"
        );
        Some((
            skipped,
            format!("{TOOL_BUDGET_SKIP_PREFIX} ({limit} von {limit})"),
        ))
    }

    // Zählt `count` ausgelöste Werkzeugaufrufe.
    pub(super) fn record_tool_calls(&self, count: usize) {
        let mut meter = self.meter();
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        meter.tool_calls = meter.tool_calls.saturating_add(count);
    }

    // `true`, wenn die Wanduhrgrenze erreicht ist.
    fn wall_time_exceeded(&self, meter: &TurnMeter) -> bool {
        meter
            .started
            .is_some_and(|started| started.elapsed() >= self.limits.wall_time)
    }

    // Prüfpunkt vor einem Modellaufruf: Abbruch, Runden, Ausgabe-Tokens, Wanduhr.
    pub(super) fn model_checkpoint(&self) -> Option<CancelReason> {
        if let Some(reason) = self.cancel.reason() {
            return Some(reason);
        }
        let mut meter = self.meter();
        let fresh_used = self.token_budget.map(|budget| {
            budget
                .used_before
                .saturating_add(meter.usage.fresh_tokens())
        });
        let fresh_exhausted = self
            .token_budget
            .zip(fresh_used)
            .is_some_and(|(budget, used)| budget.exhausted(used));
        let detail = if meter.model_rounds >= self.limits.max_model_rounds {
            Some(format!(
                "Rundenlimit des Turns erreicht ({}/{} Modellrunden)",
                meter.model_rounds, self.limits.max_model_rounds
            ))
        } else if meter.usage.output_tokens >= self.limits.max_output_tokens_total {
            Some(format!(
                "Ausgabe-Token-Limit des Turns erreicht ({}/{} Tokens)",
                meter.usage.output_tokens, self.limits.max_output_tokens_total
            ))
        } else if fresh_exhausted {
            Some(format!(
                "{TOKEN_BUDGET_STOP_PREFIX} ({} von {} neuen Tokens; Cache-Lesungen zählen nicht)",
                fresh_used.unwrap_or(0),
                self.token_budget.map_or(0, |budget| budget.limit)
            ))
        } else if self.wall_time_exceeded(&meter) {
            Some(self.wall_time_detail())
        } else {
            None
        };
        if fresh_exhausted {
            tracing::warn!(
                limit = self.token_budget.map_or(0, |budget| budget.limit),
                "turn_loop.token_budget_exhausted"
            );
        }
        let detail = detail?;
        meter.stop_detail.get_or_insert(detail);
        Some(CancelReason::Budget)
    }

    // Prüfpunkt vor dem Auslösen von `count` Werkzeugaufrufen.
    pub(super) fn tool_checkpoint(&self, count: usize) -> Option<CancelReason> {
        if let Some(reason) = self.cancel.reason() {
            return Some(reason);
        }
        let mut meter = self.meter();
        let requested =
            u64::from(meter.tool_calls).saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        let detail = if requested > u64::from(self.limits.max_tool_calls) {
            Some(format!(
                "Werkzeug-Limit des Turns erreicht ({requested}/{} Aufrufe)",
                self.limits.max_tool_calls
            ))
        } else if self.wall_time_exceeded(&meter) {
            Some(self.wall_time_detail())
        } else {
            None
        };
        let detail = detail?;
        meter.stop_detail.get_or_insert(detail);
        Some(CancelReason::Budget)
    }

    // Beschreibung eines Wanduhr-Endes mit Grenze.
    fn wall_time_detail(&self) -> String {
        format!(
            "Zeitlimit des Turns erreicht ({} s)",
            self.limits.wall_time.as_secs()
        )
    }

    /// Vermerkt den konkreten Grund eines `CancelReason::Budget`-Endes (z. B.
    /// einen Abbruch durch einen Turn-Wächter). Ein bereits vermerkter Grund
    /// bleibt stehen.
    pub(crate) fn note_stop_detail(&self, detail: impl Into<String>) {
        self.meter()
            .stop_detail
            .get_or_insert_with(|| detail.into());
    }

    /// Der konkrete Grund, aus dem dieser Turn mit `CancelReason::Budget`
    /// endete: welche Grenze (mit Wert und Verbrauch) oder welcher
    /// Turn-Wächter. `None`, solange kein solches Ende eintrat.
    ///
    /// # Concurrency
    /// Klone teilen den Zähler; der Aufrufer (z. B. der Kind-Spawner) liest
    /// den Grund über seinen eigenen Klon.
    #[must_use]
    pub fn stop_detail(&self) -> Option<String> {
        self.meter().stop_detail.clone()
    }
}
