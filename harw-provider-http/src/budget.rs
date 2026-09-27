//! Client-seitige Request-/Token-Budgets (RPM/TPM) je Provider und Modell.
//!
//! # Description
//! Ergänzt das **reaktive** Header-Pacing ([`crate::rate_limiter`]) um
//! **proaktive**, lokal konfigurierte Obergrenzen aus `[rate_limit]` in
//! `providers/<name>.toml` bzw. `models/<name>.toml` (siehe
//! [`harw_config::RateLimitToml`]):
//!
//! - `requests_per_minute` (RPM), `tokens_per_minute` (TPM, Eingabe +
//!   Ausgabe), `input_tokens_per_minute`, `output_tokens_per_minute` —
//!   jeweils ein Token-Bucket mit Kapazität = Limit, der **kontinuierlich**
//!   mit `Limit / 60 s` aufgefüllt wird (kein festes Minutenfenster, damit es
//!   keinen Burst an der Fenstergrenze gibt);
//! - `max_concurrent` — ein Semaphore je Bucket.
//!
//! Ein Request reserviert vor dem Senden per [`ProviderBudgets::acquire`]
//! `1` Request, die geschätzten Eingabe-Tokens und die angeforderten
//! Ausgabe-Tokens (auf die Bucket-Kapazität gekappt); fehlt Kapazität, wartet
//! der Aufruf (ohne Busy-Loop), bis sie nachgelaufen ist. Nach der Antwort
//! gleicht [`BudgetPermit::reconcile`] die Reservierung mit der tatsächlichen
//! [`TokenUsage`] ab (Überschuss wird zurückgegeben, Mehrverbrauch
//! nachbelastet). Eine HTTP-429-Antwort sperrt über
//! [`ProviderBudgets::penalize`] alle Buckets des Requests für die
//! `Retry-After`-Dauer — auch für bereits wartende Aufrufer.
//!
//! # Schlüssel
//! Buckets liegen in einer [`ProviderBudgetRegistry`] unter `(provider_id,
//! None)` (Provider-weit) bzw. `(provider_id, Some(model_id))` (Modell-
//! Override). Ein Request für ein Modell mit Override belegt **beide**
//! Buckets. Die Registry ist prozessweit ([`ProviderBudgetRegistry::global`])
//! oder injizierbar, damit ein Neuaufbau der Provider (Config-Reload) bei
//! unveränderten Limits denselben Bucket-Zustand weiterverwendet.
//!
//! # Fail-fast
//! Übersteigt schon die **Eingabe-Schätzung** eines einzelnen Requests ein
//! Minutenlimit (`tokens_per_minute`/`input_tokens_per_minute`), kann er nie
//! zugelassen werden: [`BudgetError::ExceedsLimit`] statt ewigem Warten.
//!
//! # Concurrency
//! Alle Typen sind `Send + Sync`. Der Bucket-Zustand liegt hinter einem
//! kurzzeitig gehaltenen `std::sync::Mutex` (nie über ein `.await` hinweg);
//! Wartende schlafen über `tokio::time::timeout` auf einem
//! [`tokio::sync::Notify`], das bei Rückgaben und Sperren geweckt wird.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use harw_types::TokenUsage;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

/// Länge des Bezugsfensters aller Budgets in Sekunden (per Minute).
const WINDOW_SECS: f64 = 60.0;

/// Kürzeste gemeldete Wartezeit; verhindert ein Drehen mit `0`-Timeouts,
/// wenn Rundung eine Winzigkeit Kapazität fehlen lässt.
const MIN_WAIT: Duration = Duration::from_millis(1);

/// Längste einzelne Wartezeit, die ein Bucket meldet (tiefe negative
/// Füllstände nach großem Mehrverbrauch).
const MAX_REPORTED_WAIT: Duration = Duration::from_secs(3600);

/// Längster einzelner Schlaf, bevor ein Wartender den Zustand neu prüft.
const MAX_SLEEP: Duration = Duration::from_secs(60);

/// Sperrdauer nach einer 429-Antwort ohne verwertbares `Retry-After`.
pub const DEFAULT_PENALTY: Duration = Duration::from_secs(5);

/// Obergrenze einer einzelnen 429-Sperre.
const MAX_PENALTY: Duration = Duration::from_secs(300);

/// Grobe Faustregel für die Eingabe-Schätzung: Bytes je Token.
pub const BYTES_PER_TOKEN_ESTIMATE: u64 = 4;

/// Rechnet eine Byte-Zahl mit [`BYTES_PER_TOKEN_ESTIMATE`] in Tokens um
/// (aufgerundet).
#[must_use]
pub fn estimate_tokens_from_bytes(bytes: u64) -> u64 {
    bytes.div_ceil(BYTES_PER_TOKEN_ESTIMATE)
}

/// Schätzt die Eingabe-Tokens eines fertigen Wire-Bodys (JSON) über seine
/// serialisierte Länge.
///
/// # Description
/// Bewusst provider-neutral und eher zu hoch (JSON-Rahmen, Base64-Bilder
/// zählen mit): das Budget soll lieber etwas früher bremsen als einen 429
/// provozieren. [`BudgetPermit::reconcile`] korrigiert die Schätzung nach
/// der Antwort auf den tatsächlichen Verbrauch.
#[must_use]
pub fn estimate_wire_tokens(wire: &serde_json::Value) -> u64 {
    let mut counter = ByteCounter(0);
    match serde_json::to_writer(&mut counter, wire) {
        Ok(()) => estimate_tokens_from_bytes(counter.0),
        Err(_) => 0,
    }
}

/// Zählt geschriebene Bytes, ohne sie zu puffern.
struct ByteCounter(u64);

impl std::io::Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Zeitquelle der Buckets; injizierbar für deterministische Tests.
pub trait BudgetClock: Send + Sync + fmt::Debug {
    /// Aktueller Zeitpunkt.
    fn now(&self) -> Instant;
}

/// Produktions-Zeitquelle über [`Instant::now`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl BudgetClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Die wirksamen Budget-Grenzen eines Buckets (alle `None` = kein Budget).
///
/// `Some(0)` wird beim Bau eines [`ProviderBudget`] wie `None` behandelt
/// (die Konfiguration lehnt `0` bereits ab, siehe
/// [`harw_config::RateLimitToml::validate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BudgetLimits {
    /// Requests pro Minute.
    pub requests_per_minute: Option<u32>,
    /// Tokens (Eingabe + Ausgabe) pro Minute.
    pub tokens_per_minute: Option<u64>,
    /// Eingabe-Tokens pro Minute.
    pub input_tokens_per_minute: Option<u64>,
    /// Ausgabe-Tokens pro Minute.
    pub output_tokens_per_minute: Option<u64>,
    /// Gleichzeitig in Flug befindliche Requests.
    pub max_concurrent: Option<u32>,
}

impl BudgetLimits {
    /// Liest die Budget-Felder aus einer `[rate_limit]`-Sektion.
    ///
    /// # Returns
    /// `None`, wenn die Sektion keine Budgets aktiviert (kein Budget-Feld,
    /// oder `mode = "header"`, siehe
    /// [`harw_config::RateLimitToml::budget_enabled`]).
    #[must_use]
    pub fn from_config(config: &harw_config::RateLimitToml) -> Option<Self> {
        if !config.budget_enabled() {
            return None;
        }
        let limits = Self {
            requests_per_minute: config.requests_per_minute,
            tokens_per_minute: config.tokens_per_minute,
            input_tokens_per_minute: config.input_tokens_per_minute,
            output_tokens_per_minute: config.output_tokens_per_minute,
            max_concurrent: config.max_concurrent,
        }
        .normalized();
        (!limits.is_empty()).then_some(limits)
    }

    /// `true`, wenn keine einzige Grenze gesetzt ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requests_per_minute.is_none()
            && self.tokens_per_minute.is_none()
            && self.input_tokens_per_minute.is_none()
            && self.output_tokens_per_minute.is_none()
            && self.max_concurrent.is_none()
    }

    /// Ersetzt `Some(0)` durch `None`.
    #[must_use]
    fn normalized(self) -> Self {
        Self {
            requests_per_minute: self.requests_per_minute.filter(|value| *value > 0),
            tokens_per_minute: self.tokens_per_minute.filter(|value| *value > 0),
            input_tokens_per_minute: self.input_tokens_per_minute.filter(|value| *value > 0),
            output_tokens_per_minute: self.output_tokens_per_minute.filter(|value| *value > 0),
            max_concurrent: self.max_concurrent.filter(|value| *value > 0),
        }
    }
}

/// Fehler beim Reservieren eines Budgets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetError {
    /// Schon die Eingabe-Schätzung eines einzelnen Requests übersteigt ein
    /// Minutenlimit; der Request könnte nie zugelassen werden.
    ExceedsLimit {
        /// Provider des Buckets.
        provider: String,
        /// Modell des Buckets (`None` = Provider-weit).
        model: Option<String>,
        /// Name des verletzten Felds, z. B. `tokens_per_minute`.
        dimension: &'static str,
        /// Angefragte Menge.
        requested: u64,
        /// Konfiguriertes Limit.
        limit: u64,
    },
    /// Das `max_concurrent`-Semaphore wurde geschlossen (interner Fehler).
    Closed,
}

impl fmt::Display for BudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExceedsLimit {
                provider,
                model,
                dimension,
                requested,
                limit,
            } => {
                write!(
                    f,
                    "request needs ~{requested} tokens but provider '{provider}'"
                )?;
                if let Some(model) = model {
                    write!(f, " model '{model}'")?;
                }
                write!(
                    f,
                    " allows only {limit} per minute (rate_limit.{dimension}); shrink the request or raise the budget"
                )
            }
            Self::Closed => write!(f, "internal error: provider budget semaphore was closed"),
        }
    }
}

impl std::error::Error for BudgetError {}

impl From<BudgetError> for harw_core::ModelError {
    fn from(error: BudgetError) -> Self {
        Self::RequestFailed(error.to_string())
    }
}

/// Ein kontinuierlich auffüllender Token-Bucket (Kapazität = Minutenlimit).
#[derive(Debug, Clone, Copy)]
struct TokenBucket {
    capacity: f64,
    /// Aktueller Füllstand; darf nach Nachbelastung negativ werden.
    level: f64,
}

impl TokenBucket {
    fn full(capacity: u64) -> Self {
        let capacity = capacity as f64;
        Self {
            capacity,
            level: capacity,
        }
    }

    fn per_second(&self) -> f64 {
        self.capacity / WINDOW_SECS
    }

    fn refill(&mut self, elapsed: Duration) {
        let refilled = self.level + self.per_second() * elapsed.as_secs_f64();
        self.level = refilled.min(self.capacity);
    }

    /// Wartezeit, bis `amount` verfügbar ist; `None` = sofort.
    fn wait_for(&self, amount: f64) -> Option<Duration> {
        if self.level >= amount {
            return None;
        }
        Some(duration_from_secs(
            (amount - self.level) / self.per_second(),
        ))
    }

    fn take(&mut self, amount: f64) {
        self.level -= amount;
    }

    /// Positiv = nachbelasten, negativ = zurückgeben (bis zur Kapazität).
    fn adjust(&mut self, delta: f64) {
        self.level = (self.level - delta).min(self.capacity);
    }

    fn available(&self) -> u64 {
        // `as` sättigt bei f64 → u64; der Wert ist hier >= 0.
        self.level.max(0.0).floor() as u64
    }
}

/// Wandelt Sekunden (f64) panikfrei in eine Wartezeit um.
fn duration_from_secs(secs: f64) -> Duration {
    if !secs.is_finite() || secs <= 0.0 {
        return MIN_WAIT;
    }
    Duration::try_from_secs_f64(secs)
        .unwrap_or(MAX_REPORTED_WAIT)
        .saturating_add(MIN_WAIT)
        .clamp(MIN_WAIT, MAX_REPORTED_WAIT)
}

/// Die Reservierung eines einzelnen Requests je Dimension.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Charge {
    input: f64,
    output: f64,
}

impl Charge {
    fn total(&self) -> f64 {
        self.input + self.output
    }
}

/// Veränderlicher Zustand eines Buckets.
#[derive(Debug, Clone, Copy)]
struct BudgetState {
    last_refill: Instant,
    requests: Option<TokenBucket>,
    tokens: Option<TokenBucket>,
    input_tokens: Option<TokenBucket>,
    output_tokens: Option<TokenBucket>,
    /// Bis wann nach einer 429-Antwort niemand zugelassen wird.
    blocked_until: Option<Instant>,
    admitted_total: u64,
    throttled_total: u64,
    penalties_total: u64,
}

impl BudgetState {
    fn new(limits: &BudgetLimits, now: Instant) -> Self {
        Self {
            last_refill: now,
            requests: limits
                .requests_per_minute
                .map(|limit| TokenBucket::full(u64::from(limit))),
            tokens: limits.tokens_per_minute.map(TokenBucket::full),
            input_tokens: limits.input_tokens_per_minute.map(TokenBucket::full),
            output_tokens: limits.output_tokens_per_minute.map(TokenBucket::full),
            blocked_until: None,
            admitted_total: 0,
            throttled_total: 0,
            penalties_total: 0,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill);
        if elapsed.is_zero() {
            return;
        }
        self.last_refill = now;
        for bucket in [
            self.requests.as_mut(),
            self.tokens.as_mut(),
            self.input_tokens.as_mut(),
            self.output_tokens.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            bucket.refill(elapsed);
        }
    }

    /// Längste fällige Wartezeit über Sperre und alle Buckets; `None` =
    /// sofort zulassbar.
    fn wait_needed(&self, charge: &Charge, now: Instant) -> Option<Duration> {
        self.longest_wait([1.0, charge.total(), charge.input, charge.output], now)
    }

    /// Wie [`Self::wait_needed`], aber mit je `1` Einheit in jedem Bucket
    /// (Vorschau für das Pacing, siehe [`ProviderBudget::preview_wait`]).
    fn unit_wait(&self, now: Instant) -> Option<Duration> {
        self.longest_wait([1.0; 4], now)
    }

    /// Längste Wartezeit über Sperre und die Buckets RPM, TPM, Eingabe,
    /// Ausgabe mit den Mengen `amounts` (in dieser Reihenfolge).
    fn longest_wait(&self, amounts: [f64; 4], now: Instant) -> Option<Duration> {
        let mut longest = self
            .blocked_until
            .map(|until| until.saturating_duration_since(now))
            .filter(|rest| !rest.is_zero());
        let buckets = [
            &self.requests,
            &self.tokens,
            &self.input_tokens,
            &self.output_tokens,
        ];
        for (bucket, amount) in buckets.into_iter().zip(amounts) {
            if let Some(wait) = bucket.as_ref().and_then(|bucket| bucket.wait_for(amount)) {
                longest = Some(longest.map_or(wait, |current| current.max(wait)));
            }
        }
        longest
    }

    fn take(&mut self, charge: &Charge) {
        let total = charge.total();
        if let Some(bucket) = self.requests.as_mut() {
            bucket.take(1.0);
        }
        if let Some(bucket) = self.tokens.as_mut() {
            bucket.take(total);
        }
        if let Some(bucket) = self.input_tokens.as_mut() {
            bucket.take(charge.input);
        }
        if let Some(bucket) = self.output_tokens.as_mut() {
            bucket.take(charge.output);
        }
    }
}

/// Zählt einen Aufrufer als wartend, solange er lebt (auch bei Abbruch des
/// wartenden Futures).
struct WaitingGuard<'a> {
    counter: &'a AtomicUsize,
}

impl<'a> WaitingGuard<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for WaitingGuard<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Ein einzelner Budget-Bucket für `(provider, Option<model>)`.
///
/// # Concurrency
/// `Send + Sync`; gedacht zur gemeinsamen Nutzung über `Arc`.
#[derive(Debug)]
pub struct ProviderBudget {
    provider_id: String,
    model_id: Option<String>,
    limits: BudgetLimits,
    state: Mutex<BudgetState>,
    notify: Notify,
    concurrency: Option<Arc<Semaphore>>,
    waiting: AtomicUsize,
    clock: Arc<dyn BudgetClock>,
}

impl ProviderBudget {
    /// Baut einen vollen Bucket mit der Systemuhr.
    #[must_use]
    pub fn new(provider_id: &str, model_id: Option<&str>, limits: BudgetLimits) -> Self {
        Self::with_clock(provider_id, model_id, limits, Arc::new(SystemClock))
    }

    /// Baut einen vollen Bucket mit injizierter Zeitquelle.
    #[must_use]
    pub fn with_clock(
        provider_id: &str,
        model_id: Option<&str>,
        limits: BudgetLimits,
        clock: Arc<dyn BudgetClock>,
    ) -> Self {
        let limits = limits.normalized();
        let now = clock.now();
        Self {
            provider_id: provider_id.to_owned(),
            model_id: model_id.map(str::to_owned),
            limits,
            state: Mutex::new(BudgetState::new(&limits, now)),
            notify: Notify::new(),
            concurrency: limits
                .max_concurrent
                .map(|permits| Arc::new(Semaphore::new(permits as usize))),
            waiting: AtomicUsize::new(0),
            clock,
        }
    }

    /// Provider dieses Buckets.
    #[must_use]
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    /// Modell dieses Buckets (`None` = Provider-weit).
    #[must_use]
    pub fn model_id(&self) -> Option<&str> {
        self.model_id.as_deref()
    }

    /// Die (normalisierten) Grenzen dieses Buckets.
    #[must_use]
    pub fn limits(&self) -> BudgetLimits {
        self.limits
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, BudgetState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn exceeds(&self, dimension: &'static str, requested: u64, limit: u64) -> BudgetError {
        BudgetError::ExceedsLimit {
            provider: self.provider_id.clone(),
            model: self.model_id.clone(),
            dimension,
            requested,
            limit,
        }
    }

    /// Berechnet die Reservierung und prüft Fail-fast.
    fn charge_for(&self, estimated_input: u64, max_output: u64) -> Result<Charge, BudgetError> {
        if let Some(limit) = self.limits.input_tokens_per_minute
            && estimated_input > limit
        {
            return Err(self.exceeds("input_tokens_per_minute", estimated_input, limit));
        }
        if let Some(limit) = self.limits.tokens_per_minute
            && estimated_input > limit
        {
            return Err(self.exceeds("tokens_per_minute", estimated_input, limit));
        }
        // Die Ausgabe-Reservierung ist eine Obergrenze, keine Zusage: sie wird
        // auf die Bucket-Kapazität gekappt, damit ein großes `max_tokens`
        // einen Request nicht dauerhaft blockiert. `reconcile` belastet einen
        // echten Mehrverbrauch nach.
        let mut output = max_output;
        if let Some(limit) = self.limits.output_tokens_per_minute {
            output = output.min(limit);
        }
        if let Some(limit) = self.limits.tokens_per_minute {
            output = output.min(limit.saturating_sub(estimated_input));
        }
        Ok(Charge {
            input: estimated_input as f64,
            output: output as f64,
        })
    }

    /// Versucht eine Zulassung; `None` = zugelassen und abgebucht, sonst die
    /// fällige Wartezeit (und der Aufrufer wird als wartend gezählt).
    fn try_admit<'a>(
        &'a self,
        charge: &Charge,
        waiting: &mut Option<WaitingGuard<'a>>,
    ) -> Option<Duration> {
        let now = self.clock.now();
        let mut state = self.lock_state();
        state.refill(now);
        match state.wait_needed(charge, now) {
            None => {
                state.take(charge);
                state.admitted_total = state.admitted_total.saturating_add(1);
                None
            }
            Some(wait) => {
                if waiting.is_none() {
                    state.throttled_total = state.throttled_total.saturating_add(1);
                    *waiting = Some(WaitingGuard::new(&self.waiting));
                }
                Some(wait)
            }
        }
    }

    /// Weckt alle Wartenden, damit sie ihren Bedarf neu prüfen.
    fn wake_waiters(&self) {
        self.notify.notify_waiters();
    }

    /// Reserviert Budget für einen Request und wartet, bis Kapazität frei ist.
    ///
    /// # Arguments
    /// - `estimated_input`: geschätzte Eingabe-Tokens (siehe
    ///   [`estimate_wire_tokens`]).
    /// - `max_output`: angeforderte Ausgabe-Tokens (`0`, wenn unbekannt);
    ///   wird auf die Bucket-Kapazität gekappt.
    ///
    /// # Errors
    /// - [`BudgetError::ExceedsLimit`]: die Eingabe-Schätzung allein
    ///   übersteigt ein Minutenlimit (sofort, ohne Warten).
    /// - [`BudgetError::Closed`]: internes Semaphore geschlossen.
    ///
    /// # Cancel safety
    /// Wird das Future vor der Zulassung fallen gelassen, ist nichts
    /// abgebucht. Nach der Zulassung hält der zurückgegebene
    /// [`BudgetPermit`] die Reservierung.
    pub async fn acquire(
        self: &Arc<Self>,
        estimated_input: u64,
        max_output: u64,
    ) -> Result<BudgetPermit, BudgetError> {
        let charge = self.charge_for(estimated_input, max_output)?;
        let mut waiting: Option<WaitingGuard<'_>> = None;
        loop {
            // Vor der Prüfung registrieren, damit kein Wecken zwischen
            // Prüfung und Schlaf verloren geht.
            let notified = self.notify.notified();
            let mut notified = std::pin::pin!(notified);
            let _registered = notified.as_mut().enable();
            let Some(wait) = self.try_admit(&charge, &mut waiting) else {
                break;
            };
            tracing::debug!(
                provider = %self.provider_id,
                model = self.model_id.as_deref().unwrap_or("*"),
                wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                "provider_budget.waiting"
            );
            // Timeout und Wecken bedeuten beide: Zustand neu prüfen.
            let _woken = tokio::time::timeout(wait.min(MAX_SLEEP), notified).await;
        }
        drop(waiting);
        let slot = match &self.concurrency {
            Some(semaphore) => Some(
                Arc::clone(semaphore)
                    .acquire_owned()
                    .await
                    .map_err(|_| BudgetError::Closed)?,
            ),
            None => None,
        };
        Ok(BudgetPermit {
            reservations: vec![Reservation {
                budget: Arc::clone(self),
                charge,
                _slot: slot,
            }],
        })
    }

    /// Sperrt diesen Bucket nach einer HTTP-429-Antwort.
    ///
    /// # Description
    /// Niemand (auch kein bereits Wartender) wird vor `now + retry_after`
    /// zugelassen; eine bestehende längere Sperre bleibt. `None` nutzt
    /// [`DEFAULT_PENALTY`]; die Dauer ist auf 5 Minuten gekappt.
    pub fn penalize(&self, retry_after: Option<Duration>) {
        let pause = retry_after.unwrap_or(DEFAULT_PENALTY).min(MAX_PENALTY);
        let now = self.clock.now();
        let until = now.checked_add(pause).unwrap_or(now);
        {
            let mut state = self.lock_state();
            state.blocked_until = Some(
                state
                    .blocked_until
                    .map_or(until, |current| current.max(until)),
            );
            state.penalties_total = state.penalties_total.saturating_add(1);
        }
        tracing::info!(
            provider = %self.provider_id,
            model = self.model_id.as_deref().unwrap_or("*"),
            pause_ms = u64::try_from(pause.as_millis()).unwrap_or(u64::MAX),
            "provider_budget.penalized_after_429"
        );
        self.wake_waiters();
    }

    /// Gleicht eine Reservierung mit dem tatsächlichen Verbrauch ab.
    fn reconcile(&self, charge: &Charge, actual_input: u64, actual_output: u64) {
        let actual_input = actual_input as f64;
        let actual_output = actual_output as f64;
        let delta_input = actual_input - charge.input;
        let delta_output = actual_output - charge.output;
        let delta_total = (actual_input + actual_output) - charge.total();
        {
            let now = self.clock.now();
            let mut state = self.lock_state();
            state.refill(now);
            if let Some(bucket) = state.tokens.as_mut() {
                bucket.adjust(delta_total);
            }
            if let Some(bucket) = state.input_tokens.as_mut() {
                bucket.adjust(delta_input);
            }
            if let Some(bucket) = state.output_tokens.as_mut() {
                bucket.adjust(delta_output);
            }
        }
        if delta_total < 0.0 || delta_input < 0.0 || delta_output < 0.0 {
            self.wake_waiters();
        }
    }

    /// Wartezeit, bis dieser Bucket wieder mindestens eine Einheit je
    /// Dimension (1 Request, 1 Token) zulässt; berücksichtigt eine
    /// 429-Sperre.
    ///
    /// # Description
    /// Seiteneffektfrei: arbeitet auf einer Kopie des Zustands (der Lock wird
    /// nur für das Kopieren gehalten), bucht nichts ab und zählt niemanden
    /// als wartend. `max_concurrent` fließt nicht ein, weil das Freiwerden
    /// eines Platzes keine vorhersagbare Dauer hat.
    ///
    /// # Returns
    /// `None`, wenn sofort Kapazität frei ist.
    #[must_use]
    pub fn preview_wait(&self) -> Option<Duration> {
        let now = self.clock.now();
        let mut probe = *self.lock_state();
        probe.refill(now);
        probe.unit_wait(now)
    }

    /// Momentaufnahme für `/status` und das `provider-concurrency`-Tool.
    #[must_use]
    pub fn snapshot(&self) -> BudgetSnapshot {
        let now = self.clock.now();
        let mut state = self.lock_state();
        state.refill(now);
        BudgetSnapshot {
            provider: self.provider_id.clone(),
            model: self.model_id.clone(),
            limits: self.limits,
            requests_available: state.requests.map(|bucket| bucket.available()),
            tokens_available: state.tokens.map(|bucket| bucket.available()),
            input_tokens_available: state.input_tokens.map(|bucket| bucket.available()),
            output_tokens_available: state.output_tokens.map(|bucket| bucket.available()),
            concurrent_available: self
                .concurrency
                .as_ref()
                .map(|semaphore| semaphore.available_permits()),
            waiting: self.waiting.load(Ordering::Relaxed),
            blocked_for: state
                .blocked_until
                .map(|until| until.saturating_duration_since(now))
                .filter(|rest| !rest.is_zero()),
            admitted_total: state.admitted_total,
            throttled_total: state.throttled_total,
            penalties_total: state.penalties_total,
        }
    }
}

/// Momentaufnahme eines Buckets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetSnapshot {
    /// Provider des Buckets.
    pub provider: String,
    /// Modell des Buckets (`None` = Provider-weit).
    pub model: Option<String>,
    /// Konfigurierte Grenzen.
    pub limits: BudgetLimits,
    /// Aktuell verfügbare Requests (RPM-Bucket).
    pub requests_available: Option<u64>,
    /// Aktuell verfügbare Tokens (TPM-Bucket).
    pub tokens_available: Option<u64>,
    /// Aktuell verfügbare Eingabe-Tokens.
    pub input_tokens_available: Option<u64>,
    /// Aktuell verfügbare Ausgabe-Tokens.
    pub output_tokens_available: Option<u64>,
    /// Freie `max_concurrent`-Plätze.
    pub concurrent_available: Option<usize>,
    /// Aktuell auf Kapazität wartende Requests.
    pub waiting: usize,
    /// Restdauer einer 429-Sperre.
    pub blocked_for: Option<Duration>,
    /// Seit Bau zugelassene Requests.
    pub admitted_total: u64,
    /// Seit Bau zurückgehaltene (wartende) Requests.
    pub throttled_total: u64,
    /// Seit Bau verhängte 429-Sperren.
    pub penalties_total: u64,
}

impl fmt::Display for BudgetSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}:",
            self.provider,
            self.model.as_deref().unwrap_or("*")
        )?;
        if let (Some(available), Some(limit)) =
            (self.requests_available, self.limits.requests_per_minute)
        {
            write!(f, " rpm {available}/{limit}")?;
        }
        if let (Some(available), Some(limit)) =
            (self.tokens_available, self.limits.tokens_per_minute)
        {
            write!(f, " tpm {available}/{limit}")?;
        }
        if let (Some(available), Some(limit)) = (
            self.input_tokens_available,
            self.limits.input_tokens_per_minute,
        ) {
            write!(f, " itpm {available}/{limit}")?;
        }
        if let (Some(available), Some(limit)) = (
            self.output_tokens_available,
            self.limits.output_tokens_per_minute,
        ) {
            write!(f, " otpm {available}/{limit}")?;
        }
        if let (Some(available), Some(limit)) =
            (self.concurrent_available, self.limits.max_concurrent)
        {
            write!(f, " concurrent {available}/{limit} free")?;
        }
        write!(f, " waiting {}", self.waiting)?;
        if let Some(blocked) = self.blocked_for {
            write!(f, " blocked {}s", blocked.as_secs().max(1))?;
        }
        Ok(())
    }
}

/// Eine Reservierung in einem einzelnen Bucket.
#[derive(Debug)]
struct Reservation {
    budget: Arc<ProviderBudget>,
    charge: Charge,
    _slot: Option<OwnedSemaphorePermit>,
}

/// Zulassung eines Requests über alle betroffenen Buckets.
///
/// # Description
/// Hält die Reservierungen und `max_concurrent`-Plätze, bis der Permit
/// fällt. [`Self::reconcile`] gleicht mit dem tatsächlichen Verbrauch ab;
/// ohne Abgleich (Fehler, Abbruch) bleibt die Schätzung abgebucht — das ist
/// bewusst konservativ.
#[derive(Debug, Default)]
#[must_use = "dropping the permit releases the concurrency slot immediately"]
pub struct BudgetPermit {
    reservations: Vec<Reservation>,
}

impl BudgetPermit {
    /// Ein Permit ohne Reservierungen (kein Budget konfiguriert).
    pub fn empty() -> Self {
        Self::default()
    }

    /// `true`, wenn kein Bucket betroffen ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reservations.is_empty()
    }

    fn merge(&mut self, other: Self) {
        self.reservations.extend(other.reservations);
    }

    /// Gleicht die Reservierung mit dem tatsächlichen Verbrauch ab und gibt
    /// die `max_concurrent`-Plätze frei.
    ///
    /// # Description
    /// Eingabe = `input_tokens` plus — bei getrennt gemeldeten Cache-Tokens
    /// (Anthropic) — `cache_write_tokens`; Cache-Lesezugriffe zählen nicht.
    /// Ausgabe = `output_tokens`. Meldet der Provider gar keine Nutzung
    /// (beides `0`), bleibt die Schätzung stehen.
    pub fn reconcile(self, usage: &TokenUsage) {
        let cache_write = if usage.cache_separate {
            usage.cache_write_tokens.unwrap_or(0)
        } else {
            0
        };
        let actual_input = usage.input_tokens.saturating_add(cache_write);
        let actual_output = usage.output_tokens;
        if actual_input == 0 && actual_output == 0 {
            return;
        }
        for reservation in &self.reservations {
            reservation
                .budget
                .reconcile(&reservation.charge, actual_input, actual_output);
        }
    }
}

/// Die Buckets, die ein konkreter Provider-Backend nutzt: Provider-weit plus
/// Modell-Overrides (unter Modell-ID **und** Aliasen).
///
/// # Concurrency
/// Billig klonbar (nur `Arc`s); `Send + Sync`.
#[derive(Debug, Clone, Default)]
pub struct ProviderBudgets {
    provider: Option<Arc<ProviderBudget>>,
    models: HashMap<String, Arc<ProviderBudget>>,
    /// Jeder Modell-Bucket genau einmal (für Snapshots).
    model_budgets: Vec<Arc<ProviderBudget>>,
}

impl ProviderBudgets {
    /// Nur ein Provider-weiter Bucket.
    #[must_use]
    pub fn for_provider(budget: Arc<ProviderBudget>) -> Self {
        Self {
            provider: Some(budget),
            ..Self::default()
        }
    }

    /// Ergänzt einen Modell-Bucket unter `model` und `aliases`.
    #[must_use]
    pub fn with_model(
        mut self,
        model: &str,
        aliases: &[String],
        budget: Arc<ProviderBudget>,
    ) -> Self {
        self.models.insert(model.to_owned(), Arc::clone(&budget));
        for alias in aliases {
            self.models
                .entry(alias.clone())
                .or_insert_with(|| Arc::clone(&budget));
        }
        self.model_budgets.push(budget);
        self
    }

    /// `true`, wenn weder ein Provider- noch ein Modell-Bucket existiert.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model_budgets.is_empty()
    }

    /// Die für `model` zuständigen Buckets (Provider zuerst).
    fn budgets_for<'a>(&'a self, model: &str) -> impl Iterator<Item = &'a Arc<ProviderBudget>> {
        self.provider.iter().chain(self.models.get(model))
    }

    /// Reserviert Budget in allen für `model` zuständigen Buckets.
    ///
    /// # Description
    /// Prüft zuerst Fail-fast gegen **alle** Buckets, dann wird nacheinander
    /// (Provider, dann Modell) reserviert und gewartet. Ohne Buckets kehrt
    /// der Aufruf sofort mit [`BudgetPermit::empty`] zurück.
    ///
    /// # Errors
    /// Siehe [`ProviderBudget::acquire`].
    pub async fn acquire(
        &self,
        model: &str,
        estimated_input: u64,
        max_output: u64,
    ) -> Result<BudgetPermit, BudgetError> {
        for budget in self.budgets_for(model) {
            budget.charge_for(estimated_input, max_output)?;
        }
        let mut permit = BudgetPermit::empty();
        for budget in self.budgets_for(model) {
            permit.merge(budget.acquire(estimated_input, max_output).await?);
        }
        Ok(permit)
    }

    /// Sperrt alle für `model` zuständigen Buckets nach einer 429-Antwort
    /// (siehe [`ProviderBudget::penalize`]).
    pub fn penalize(&self, model: &str, retry_after: Option<Duration>) {
        for budget in self.budgets_for(model) {
            budget.penalize(retry_after);
        }
    }

    /// Längste Wartezeit über **alle** Buckets (Provider und jedes Modell),
    /// bis jeder wieder mindestens eine Einheit zulässt (siehe
    /// [`ProviderBudget::preview_wait`]).
    ///
    /// # Description
    /// Seiteneffektfrei; gedacht für das Pacing vor einem Request, dessen
    /// Modell noch nicht feststeht (daher bewusst konservativ über alle
    /// Modell-Buckets). Für ein bekanntes Modell siehe
    /// [`Self::preview_wait_for`].
    ///
    /// # Returns
    /// `None` ohne konfigurierte Buckets oder wenn keiner warten müsste.
    #[must_use]
    pub fn preview_wait(&self) -> Option<Duration> {
        self.provider
            .iter()
            .chain(self.model_budgets.iter())
            .filter_map(|budget| budget.preview_wait())
            .max()
    }

    /// Wie [`Self::preview_wait`], aber nur über die für `model` zuständigen
    /// Buckets (Provider-weit plus ggf. Modell-Override).
    #[must_use]
    pub fn preview_wait_for(&self, model: &str) -> Option<Duration> {
        self.budgets_for(model)
            .filter_map(|budget| budget.preview_wait())
            .max()
    }

    /// Momentaufnahmen aller Buckets (Provider zuerst, dann Modelle).
    #[must_use]
    pub fn snapshot(&self) -> Vec<BudgetSnapshot> {
        self.provider
            .iter()
            .chain(self.model_budgets.iter())
            .map(|budget| budget.snapshot())
            .collect()
    }
}

/// Schlüssel eines Buckets: `(provider_id, Option<model_id>)`.
pub type BudgetKey = (String, Option<String>);

/// Registry aller Budget-Buckets.
///
/// # Description
/// Hält Buckets über einen Neuaufbau der Provider hinweg: ein erneutes
/// [`Self::configure`] mit **unveränderten** Grenzen liefert denselben
/// `Arc`, geänderte Grenzen ersetzen den Bucket (voll aufgefüllt).
///
/// # Concurrency
/// `Send + Sync`; der Map-Lock wird nur kurz gehalten.
#[derive(Debug)]
pub struct ProviderBudgetRegistry {
    budgets: Mutex<HashMap<BudgetKey, Arc<ProviderBudget>>>,
    clock: Arc<dyn BudgetClock>,
}

impl Default for ProviderBudgetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderBudgetRegistry {
    /// Leere Registry mit Systemuhr.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    /// Leere Registry mit injizierter Zeitquelle (Tests).
    #[must_use]
    pub fn with_clock(clock: Arc<dyn BudgetClock>) -> Self {
        Self {
            budgets: Mutex::new(HashMap::new()),
            clock,
        }
    }

    /// Prozessweite Registry (Fallback, wenn keine injiziert wurde).
    #[must_use]
    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<ProviderBudgetRegistry>> = OnceLock::new();
        Arc::clone(GLOBAL.get_or_init(|| Arc::new(Self::new())))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<BudgetKey, Arc<ProviderBudget>>> {
        self.budgets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Legt den Bucket für `(provider_id, model_id)` an, ersetzt ihn oder
    /// entfernt ihn.
    ///
    /// # Returns
    /// `None` (und der Bucket ist entfernt), wenn `limits` leer ist; sonst
    /// der bestehende Bucket bei identischen Grenzen oder ein neuer, voller.
    pub fn configure(
        &self,
        provider_id: &str,
        model_id: Option<&str>,
        limits: BudgetLimits,
    ) -> Option<Arc<ProviderBudget>> {
        let limits = limits.normalized();
        let key: BudgetKey = (provider_id.to_owned(), model_id.map(str::to_owned));
        let mut budgets = self.lock();
        if limits.is_empty() {
            budgets.remove(&key);
            return None;
        }
        if let Some(existing) = budgets.get(&key)
            && existing.limits() == limits
        {
            return Some(Arc::clone(existing));
        }
        let budget = Arc::new(ProviderBudget::with_clock(
            provider_id,
            model_id,
            limits,
            Arc::clone(&self.clock),
        ));
        budgets.insert(key, Arc::clone(&budget));
        Some(budget)
    }

    /// Liefert den Bucket für `(provider_id, model_id)`, falls vorhanden.
    #[must_use]
    pub fn get(&self, provider_id: &str, model_id: Option<&str>) -> Option<Arc<ProviderBudget>> {
        let key: BudgetKey = (provider_id.to_owned(), model_id.map(str::to_owned));
        self.lock().get(&key).map(Arc::clone)
    }

    /// Baut die Buckets eines Providers aus seiner Konfiguration.
    ///
    /// # Description
    /// Provider-Bucket aus `provider.rate_limit`, Modell-Buckets aus
    /// `rate_limit` jedes `config.models`-Eintrags mit `provider ==
    /// provider_id` (unter ID und Aliasen). Modell-Buckets dieses Providers,
    /// die nicht mehr konfiguriert sind, werden aus der Registry entfernt.
    pub fn configure_provider(
        &self,
        provider_id: &str,
        provider: &harw_config::ProviderToml,
        config: &harw_config::ResolvedConfig,
    ) -> ProviderBudgets {
        let provider_limits = provider
            .rate_limit
            .as_ref()
            .and_then(BudgetLimits::from_config)
            .unwrap_or_default();
        let mut budgets = ProviderBudgets {
            provider: self.configure(provider_id, None, provider_limits),
            ..ProviderBudgets::default()
        };
        let mut models: Vec<&harw_config::ModelToml> = config
            .models
            .values()
            .filter(|model| model.provider == provider_id)
            .collect();
        models.sort_by(|left, right| left.id.cmp(&right.id));
        let mut configured: BTreeSet<String> = BTreeSet::new();
        for model in models {
            let limits = model
                .rate_limit
                .as_ref()
                .and_then(BudgetLimits::from_config)
                .unwrap_or_default();
            if let Some(budget) = self.configure(provider_id, Some(model.id.as_str()), limits) {
                configured.insert(model.id.clone());
                budgets = budgets.with_model(&model.id, &model.aliases, budget);
            }
        }
        self.lock().retain(|(provider, model), _| {
            provider != provider_id
                || model
                    .as_ref()
                    .is_none_or(|model| configured.contains(model))
        });
        budgets
    }

    /// Momentaufnahmen aller Buckets, sortiert nach Schlüssel.
    #[must_use]
    pub fn snapshots(&self) -> Vec<BudgetSnapshot> {
        let mut budgets: Vec<(BudgetKey, Arc<ProviderBudget>)> = self
            .lock()
            .iter()
            .map(|(key, budget)| (key.clone(), Arc::clone(budget)))
            .collect();
        budgets.sort_by(|left, right| left.0.cmp(&right.0));
        budgets
            .into_iter()
            .map(|(_, budget)| budget.snapshot())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    /// Manuell vorgestellte Uhr für deterministische Tests.
    #[derive(Debug)]
    struct ManualClock {
        base: Instant,
        offset: Mutex<Duration>,
    }

    impl ManualClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: Instant::now(),
                offset: Mutex::new(Duration::ZERO),
            })
        }

        fn advance(&self, by: Duration) {
            let mut offset = self.offset.lock().unwrap_or_else(PoisonError::into_inner);
            *offset += by;
        }
    }

    impl BudgetClock for ManualClock {
        fn now(&self) -> Instant {
            let offset = *self.offset.lock().unwrap_or_else(PoisonError::into_inner);
            self.base + offset
        }
    }

    fn budget(clock: &Arc<ManualClock>, limits: BudgetLimits) -> Arc<ProviderBudget> {
        let clock: Arc<dyn BudgetClock> = Arc::clone(clock) as Arc<dyn BudgetClock>;
        Arc::new(ProviderBudget::with_clock("openai", None, limits, clock))
    }

    fn usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            ..TokenUsage::default()
        }
    }

    /// Lässt andere Tasks laufen, bis `budget` `expected` Wartende zählt.
    async fn wait_for_waiters(budget: &ProviderBudget, expected: usize) -> TestResult {
        for _ in 0..1000 {
            if budget.snapshot().waiting == expected {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
        Err(format!("expected {expected} waiting callers").into())
    }

    /// Gibt geweckten Tasks Gelegenheit, ihren Zustand neu zu prüfen.
    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    const SHORT: Duration = Duration::from_millis(50);
    const LONG: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn acquire_within_budget_is_immediate_and_debits_buckets() -> TestResult {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                requests_per_minute: Some(2),
                tokens_per_minute: Some(1_000),
                ..BudgetLimits::default()
            },
        );
        let first = tokio::time::timeout(SHORT, budget.acquire(100, 50)).await??;
        let second = tokio::time::timeout(SHORT, budget.acquire(100, 50)).await??;
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.requests_available, Some(0));
        assert_eq!(snapshot.tokens_available, Some(700));
        assert_eq!(snapshot.admitted_total, 2);
        assert_eq!(snapshot.throttled_total, 0);
        drop((first, second));
        Ok(())
    }

    #[tokio::test]
    async fn acquire_waits_until_bucket_refills() -> TestResult {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                requests_per_minute: Some(1),
                ..BudgetLimits::default()
            },
        );
        let _first = budget.acquire(0, 0).await?;
        let waiter = {
            let budget = Arc::clone(&budget);
            tokio::spawn(async move { budget.acquire(0, 0).await })
        };
        wait_for_waiters(&budget, 1).await?;
        assert!(!waiter.is_finished(), "second request must wait for refill");

        clock.advance(Duration::from_secs(30));
        budget.wake_waiters();
        settle().await;
        wait_for_waiters(&budget, 1).await?;
        assert!(
            !waiter.is_finished(),
            "half a minute refills only half a request"
        );

        clock.advance(Duration::from_secs(30));
        budget.wake_waiters();
        let _second = tokio::time::timeout(LONG, waiter).await???;
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.waiting, 0);
        assert_eq!(snapshot.throttled_total, 1);
        assert_eq!(snapshot.admitted_total, 2);
        Ok(())
    }

    #[test]
    fn buckets_refill_continuously() {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                requests_per_minute: Some(60),
                ..BudgetLimits::default()
            },
        );
        let charge = Charge {
            input: 0.0,
            output: 0.0,
        };
        let mut waiting = None;
        for _ in 0..60 {
            assert!(budget.try_admit(&charge, &mut waiting).is_none());
        }
        assert!(budget.try_admit(&charge, &mut waiting).is_some());
        drop(waiting);
        clock.advance(Duration::from_secs(1));
        assert_eq!(budget.snapshot().requests_available, Some(1));
        clock.advance(Duration::from_secs(600));
        assert_eq!(
            budget.snapshot().requests_available,
            Some(60),
            "refill is capped at the per-minute capacity"
        );
    }

    #[tokio::test]
    async fn single_request_over_the_limit_fails_fast() -> TestResult {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                tokens_per_minute: Some(1_000),
                ..BudgetLimits::default()
            },
        );
        let result = tokio::time::timeout(SHORT, budget.acquire(2_000, 0)).await?;
        let Err(BudgetError::ExceedsLimit {
            dimension,
            requested,
            limit,
            ..
        }) = result
        else {
            return Err("oversized request must fail fast".into());
        };
        assert_eq!(dimension, "tokens_per_minute");
        assert_eq!(requested, 2_000);
        assert_eq!(limit, 1_000);
        Ok(())
    }

    #[tokio::test]
    async fn max_output_is_clamped_to_remaining_capacity() -> TestResult {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                tokens_per_minute: Some(1_000),
                output_tokens_per_minute: Some(500),
                ..BudgetLimits::default()
            },
        );
        let _permit = tokio::time::timeout(SHORT, budget.acquire(100, 100_000)).await??;
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.output_tokens_available, Some(0));
        assert_eq!(snapshot.tokens_available, Some(400));
        Ok(())
    }

    #[tokio::test]
    async fn reconcile_returns_unused_and_charges_overuse() -> TestResult {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                tokens_per_minute: Some(1_000),
                ..BudgetLimits::default()
            },
        );
        let permit = budget.acquire(400, 400).await?;
        assert_eq!(budget.snapshot().tokens_available, Some(200));
        permit.reconcile(&usage(100, 50));
        assert_eq!(budget.snapshot().tokens_available, Some(850));

        let permit = budget.acquire(100, 0).await?;
        permit.reconcile(&usage(1_500, 0));
        assert_eq!(budget.snapshot().tokens_available, Some(0));
        let blocked = tokio::time::timeout(SHORT, budget.acquire(100, 0)).await;
        assert!(blocked.is_err(), "overuse must hold back the next request");
        Ok(())
    }

    #[tokio::test]
    async fn reconcile_without_reported_usage_keeps_estimate() -> TestResult {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                tokens_per_minute: Some(1_000),
                ..BudgetLimits::default()
            },
        );
        let permit = budget.acquire(300, 0).await?;
        permit.reconcile(&TokenUsage::default());
        assert_eq!(budget.snapshot().tokens_available, Some(700));
        Ok(())
    }

    #[tokio::test]
    async fn penalize_blocks_waiters_until_retry_after() -> TestResult {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                requests_per_minute: Some(100),
                ..BudgetLimits::default()
            },
        );
        budget.penalize(Some(Duration::from_secs(30)));
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.blocked_for, Some(Duration::from_secs(30)));
        assert_eq!(snapshot.penalties_total, 1);

        let waiter = {
            let budget = Arc::clone(&budget);
            tokio::spawn(async move { budget.acquire(0, 0).await })
        };
        wait_for_waiters(&budget, 1).await?;
        clock.advance(Duration::from_secs(29));
        budget.wake_waiters();
        settle().await;
        wait_for_waiters(&budget, 1).await?;
        assert!(!waiter.is_finished(), "still inside the 429 back-off");

        clock.advance(Duration::from_secs(1));
        budget.wake_waiters();
        let _permit = tokio::time::timeout(LONG, waiter).await???;
        assert_eq!(budget.snapshot().blocked_for, None);
        Ok(())
    }

    #[tokio::test]
    async fn max_concurrent_limits_in_flight_permits() -> TestResult {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                max_concurrent: Some(1),
                ..BudgetLimits::default()
            },
        );
        let first = budget.acquire(0, 0).await?;
        assert_eq!(budget.snapshot().concurrent_available, Some(0));
        assert!(
            tokio::time::timeout(SHORT, budget.acquire(0, 0))
                .await
                .is_err()
        );
        drop(first);
        let _second = tokio::time::timeout(LONG, budget.acquire(0, 0)).await??;
        Ok(())
    }

    #[test]
    fn registry_reuses_unchanged_buckets_and_replaces_changed_ones() -> TestResult {
        let registry = ProviderBudgetRegistry::new();
        let limits = BudgetLimits {
            requests_per_minute: Some(10),
            ..BudgetLimits::default()
        };
        let first = registry
            .configure("openai", None, limits)
            .ok_or("budget configured")?;
        let same = registry
            .configure("openai", None, limits)
            .ok_or("budget configured")?;
        assert!(Arc::ptr_eq(&first, &same));

        let changed = registry
            .configure(
                "openai",
                None,
                BudgetLimits {
                    requests_per_minute: Some(20),
                    ..BudgetLimits::default()
                },
            )
            .ok_or("budget configured")?;
        assert!(!Arc::ptr_eq(&first, &changed));

        assert!(
            registry
                .configure("openai", None, BudgetLimits::default())
                .is_none()
        );
        assert!(registry.get("openai", None).is_none());
        Ok(())
    }

    fn provider_toml(
        rate_limit: Option<harw_config::RateLimitToml>,
    ) -> TestResult<harw_config::ProviderToml> {
        let mut provider: harw_config::ProviderToml = toml_like_provider()?;
        provider.rate_limit = rate_limit;
        Ok(provider)
    }

    fn toml_like_provider() -> TestResult<harw_config::ProviderToml> {
        let value = serde_json::json!({
            "name": "openai",
            "api": "openai-chat",
            "base_url": "https://api.openai.com/v1",
        });
        Ok(serde_json::from_value(value)?)
    }

    fn model_toml(
        id: &str,
        aliases: &[&str],
        rate_limit: Option<harw_config::RateLimitToml>,
    ) -> TestResult<harw_config::ModelToml> {
        let value = serde_json::json!({
            "id": id,
            "provider": "openai",
            "aliases": aliases,
        });
        let mut model: harw_config::ModelToml = serde_json::from_value(value)?;
        model.rate_limit = rate_limit;
        Ok(model)
    }

    #[tokio::test]
    async fn configure_provider_builds_provider_and_model_buckets() -> TestResult {
        let registry = ProviderBudgetRegistry::new();
        let provider = provider_toml(Some(harw_config::RateLimitToml {
            tokens_per_minute: Some(10_000),
            ..harw_config::RateLimitToml::default()
        }))?;
        let mut config = harw_config::ResolvedConfig::default();
        config.models.insert(
            "gpt-5".to_owned(),
            model_toml(
                "gpt-5",
                &["flagship"],
                Some(harw_config::RateLimitToml {
                    tokens_per_minute: Some(1_000),
                    ..harw_config::RateLimitToml::default()
                }),
            )?,
        );
        config
            .models
            .insert("gpt-mini".to_owned(), model_toml("gpt-mini", &[], None)?);

        let budgets = registry.configure_provider("openai", &provider, &config);
        assert!(!budgets.is_empty());
        assert_eq!(budgets.snapshot().len(), 2);

        // Alias trifft denselben Modell-Bucket; beide Buckets werden belastet.
        let permit = budgets.acquire("flagship", 400, 0).await?;
        let snapshots = budgets.snapshot();
        assert_eq!(snapshots[0].model, None);
        assert_eq!(snapshots[0].tokens_available, Some(9_600));
        assert_eq!(snapshots[1].model.as_deref(), Some("gpt-5"));
        assert_eq!(snapshots[1].tokens_available, Some(600));
        drop(permit);

        // Ein Modell ohne Override belastet nur den Provider-Bucket.
        let _permit = budgets.acquire("gpt-mini", 100, 0).await?;
        assert_eq!(budgets.snapshot()[1].tokens_available, Some(600));

        // Fail-fast greift auch, wenn nur der Modell-Bucket zu klein ist.
        assert!(matches!(
            budgets.acquire("gpt-5", 5_000, 0).await,
            Err(BudgetError::ExceedsLimit { .. })
        ));

        // Entfällt der Override, verschwindet der Modell-Bucket aus der Registry.
        config
            .models
            .insert("gpt-5".to_owned(), model_toml("gpt-5", &[], None)?);
        let budgets = registry.configure_provider("openai", &provider, &config);
        assert_eq!(budgets.snapshot().len(), 1);
        assert!(registry.get("openai", Some("gpt-5")).is_none());
        assert_eq!(registry.snapshots().len(), 1);
        Ok(())
    }

    #[test]
    fn header_mode_builds_no_budget() {
        let config = harw_config::RateLimitToml {
            enabled: true,
            requests_per_minute: Some(10),
            mode: Some(harw_config::RateLimitMode::Header),
            ..harw_config::RateLimitToml::default()
        };
        assert!(BudgetLimits::from_config(&config).is_none());
    }

    #[test]
    fn wire_estimate_counts_serialized_bytes() {
        let wire = serde_json::json!({ "input": "abcdefgh" });
        // `{"input":"abcdefgh"}` = 20 Bytes → 5 Tokens.
        assert_eq!(estimate_wire_tokens(&wire), 5);
        assert_eq!(estimate_tokens_from_bytes(0), 0);
        assert_eq!(estimate_tokens_from_bytes(5), 2);
    }

    #[test]
    fn preview_wait_without_budgets_is_none() {
        let budgets = ProviderBudgets::default();
        assert_eq!(budgets.preview_wait(), None);
        assert_eq!(budgets.preview_wait_for("gpt-5"), None);
    }

    #[tokio::test]
    async fn preview_wait_reports_exhausted_tpm_bucket() -> TestResult {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                tokens_per_minute: Some(1_000),
                ..BudgetLimits::default()
            },
        );
        let budgets = ProviderBudgets::for_provider(Arc::clone(&budget));
        assert_eq!(budgets.preview_wait(), None, "a full bucket needs no wait");

        let _permit = tokio::time::timeout(SHORT, budgets.acquire("gpt-5", 1_000, 0)).await??;
        let wait = budgets
            .preview_wait()
            .ok_or("exhausted bucket must report a wait")?;
        assert!(wait > Duration::ZERO);
        assert!(
            wait <= Duration::from_secs(60),
            "wait {wait:?} exceeds the window"
        );
        assert_eq!(budgets.preview_wait_for("gpt-5"), Some(wait));

        // Nach dem Nachlaufen einer Einheit ist keine Wartezeit mehr fällig.
        clock.advance(wait);
        assert_eq!(budgets.preview_wait(), None);
        Ok(())
    }

    #[tokio::test]
    async fn preview_wait_does_not_consume_capacity() -> TestResult {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                requests_per_minute: Some(1),
                tokens_per_minute: Some(1_000),
                ..BudgetLimits::default()
            },
        );
        let budgets = ProviderBudgets::for_provider(Arc::clone(&budget));
        let before = budget.snapshot();
        for _ in 0..10 {
            assert_eq!(budgets.preview_wait(), None);
        }
        assert_eq!(
            budget.snapshot(),
            before,
            "preview must not change the state"
        );

        let _permit = tokio::time::timeout(SHORT, budgets.acquire("gpt-5", 100, 0)).await??;
        let admitted = budget.snapshot();
        assert_eq!(admitted.requests_available, Some(0));
        assert_eq!(admitted.tokens_available, Some(900));
        assert_eq!(admitted.admitted_total, 1);
        assert_eq!(admitted.throttled_total, 0);

        assert!(budgets.preview_wait().is_some(), "rpm bucket is empty now");
        let after = budget.snapshot();
        assert_eq!(
            after, admitted,
            "a waiting preview is not counted as throttled"
        );
        assert_eq!(after.waiting, 0);
        Ok(())
    }

    #[test]
    fn snapshot_display_is_compact() {
        let clock = ManualClock::new();
        let budget = budget(
            &clock,
            BudgetLimits {
                requests_per_minute: Some(60),
                tokens_per_minute: Some(1_000),
                ..BudgetLimits::default()
            },
        );
        assert_eq!(
            budget.snapshot().to_string(),
            "openai/*: rpm 60/60 tpm 1000/1000 waiting 0"
        );
    }
}
