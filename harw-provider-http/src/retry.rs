//! Wiederholung vorübergehender Modell-Fehler mit exponentiellem Backoff.
//!
//! ## Verantwortung (W4a / A-OAI)
//! [`RetryingProvider`] umhüllt einen beliebigen [`ModelProvider`] und
//! wiederholt eine Anfrage **nur**, wenn der Fehler laut Vertrag
//! [`ModelError::is_retryable`] wiederholbar ist (`Transient` – 408/5xx/529,
//! Verbindungsfehler –, `Timeout` und `RateLimited` – 429). `QuotaExceeded`,
//! `Auth`, `ContextLength`, `RequestFailed` (z. B. 400) und alle übrigen
//! Varianten werden sofort zurückgegeben.
//!
//! ## Wartezeit
//! - Exponentielles Backoff `base · 2^n`, gedeckelt auf `max_delay`, mit
//!   „equal jitter“: die Hälfte fest, die andere Hälfte zufällig
//!   ([`RetryPolicy::backoff_delay`]).
//! - Meldet der Provider einen Wartehinweis (`Transient::retry_after_secs`
//!   oder `RateLimited::retry_after_secs`), wird **mindestens** so lange
//!   gewartet. Liegt der Hinweis über `max_retry_after`, wird nicht
//!   wiederholt (früher zu senden als der Provider erlaubt, würde nur
//!   denselben Fehler erneut auslösen und Versuche verbrauchen).
//!
//! ## Determinismus
//! Zufall ([`JitterSource`]) und Warten ([`RetrySleeper`]) sind injizierbar;
//! Tests verwenden feste Jitter-Werte und einen aufzeichnenden Sleeper ohne
//! echte Zeit.
//!
//! ## Abbruch
//! Vor jedem Versuch und während jeder Wartezeit wird der
//! [`CancelToken`] geprüft; ein Abbruch liefert [`ModelError::Cancelled`].
//! Ein laufender Versuch selbst wird nicht unterbrochen.
//!
//! ## Nebenläufigkeit
//! [`RetryingProvider`] ist `Send + Sync`, wenn `P` es ist. [`ThreadSleeper`]
//! braucht keine Async-Runtime: je Wartezeit ein kurzlebiger Thread, der beim
//! Drop des Futures sofort endet (Kanal-Trennung).
//!
//! ## Fehler
//! Der innere Fehler des letzten Versuchs; [`ModelError::Cancelled`] bei
//! Abbruch. Kann der Timer nicht gestartet werden
//! ([`HttpProviderError::TimerUnavailable`]), wird der letzte Fehler
//! unverändert zurückgegeben.
//!
//! # Examples
//! ```rust,no_run
//! use harw_core::cancel::CancelToken;
//! use harw_core::EchoModelProvider;
//! use harw_provider_http::{RetryPolicy, RetryingProvider};
//!
//! let provider = RetryingProvider::new(EchoModelProvider::default(), RetryPolicy::default())
//!     .with_cancel(CancelToken::new());
//! # let _ = provider;
//! ```

use crate::error::HttpProviderError;
use harw_core::cancel::CancelToken;
use harw_core::{ModelError, ModelFuture, ModelProvider, ModelRequest, ModelResponse};
use std::collections::hash_map::RandomState;
use std::future::Future;
use std::hash::{BuildHasher, Hasher};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

/// Future einer Backoff-Wartezeit.
pub type SleepFuture =
    Pin<Box<dyn Future<Output = Result<(), HttpProviderError>> + Send + 'static>>;

/// Parameter der Wiederholungsstrategie.
///
/// # Description
/// `max_attempts` zählt **alle** Versuche inklusive des ersten (Wert 0 wird
/// wie 1 behandelt: kein Retry).
///
/// # Concurrency
/// Plain `Copy`-Wert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Höchstzahl der Versuche inklusive des ersten.
    pub max_attempts: u32,
    /// Basis der exponentiellen Wartezeit (erste Wiederholung).
    pub base_delay: Duration,
    /// Obergrenze der exponentiellen Wartezeit.
    pub max_delay: Duration,
    /// Größter respektierter `retry_after`-Hinweis; darüber kein Retry.
    pub max_retry_after: Duration,
    /// Runde 7, Teil L4: ob [`ModelError::Timeout`] wiederholt wird.
    /// `false` für lokale Provider (siehe
    /// `harw_config::ProviderToml::effective_retry_timeouts`): ein
    /// überlasteter lokaler Server wird durch Wiederholen nur länger
    /// blockiert. Übrige wiederholbare Fehler bleiben unberührt.
    pub retry_timeouts: bool,
}

impl Default for RetryPolicy {
    /// 4 Versuche, 10 s Basis, 30 s Deckel, `retry_after` bis 60 s. Durch
    /// Equal-Jitter liegt die erste Wartezeit damit immer bei mindestens 5 s.
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(30),
            max_retry_after: Duration::from_secs(60),
            retry_timeouts: true,
        }
    }
}

impl RetryPolicy {
    /// Berechnet die Backoff-Wartezeit vor Wiederholung `retry_index` (0-basiert).
    ///
    /// # Arguments
    /// - `retry_index` (`u32`): 0 für die erste Wiederholung.
    /// - `unit` (`f64`): Zufallswert in `[0, 1]`; außerhalb wird geklemmt, `NaN` → 0.
    ///
    /// # Returns
    /// `cap/2 + cap/2 · unit` mit `cap = min(max_delay, base_delay · 2^retry_index)`.
    #[must_use]
    pub fn backoff_delay(&self, retry_index: u32, unit: f64) -> Duration {
        let factor = 1_u32.checked_shl(retry_index.min(31)).unwrap_or(u32::MAX);
        let cap = self
            .base_delay
            .checked_mul(factor)
            .unwrap_or(self.max_delay)
            .min(self.max_delay);
        let unit = if unit.is_nan() {
            0.0
        } else {
            unit.clamp(0.0, 1.0)
        };
        let half = cap / 2;
        half + half.mul_f64(unit)
    }
}

/// Ergebnis der Retry-Entscheidung für einen Fehler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Nach der angegebenen Wartezeit erneut versuchen.
    Retry(Duration),
    /// Fehler sofort zurückgeben.
    GiveUp,
}

/// Entscheidet rein funktional, ob und wann nach `error` wiederholt wird.
///
/// # Description
/// Siehe Moduldoku: nur [`ModelError::is_retryable`] (und
/// [`ModelError::Timeout`] nur bei `policy.retry_timeouts`); der vom Fehler
/// gemeldete Wartehinweis (`Transient::retry_after_secs` oder
/// `RateLimited::retry_after_secs`) ist Untergrenze, über
/// `policy.max_retry_after` → [`RetryDecision::GiveUp`].
/// Die Versuchsobergrenze prüft der Aufrufer.
///
/// # Arguments
/// - `policy` (`&RetryPolicy`): Strategie.
/// - `error` (`&ModelError`): Fehler des letzten Versuchs.
/// - `retry_index` (`u32`): Index der geplanten Wiederholung (0-basiert).
/// - `unit` (`f64`): Jitter-Wert in `[0, 1]`.
///
/// # Returns
/// Die [`RetryDecision`].
#[must_use]
pub fn retry_decision(
    policy: &RetryPolicy,
    error: &ModelError,
    retry_index: u32,
    unit: f64,
) -> RetryDecision {
    if !error.is_retryable() {
        return RetryDecision::GiveUp;
    }
    if !policy.retry_timeouts && matches!(error, ModelError::Timeout { .. }) {
        return RetryDecision::GiveUp;
    }
    let backoff = policy.backoff_delay(retry_index, unit);
    let hint = match error {
        ModelError::Transient {
            retry_after_secs: Some(secs),
            ..
        } => Some(Duration::from_secs(*secs)),
        // `RateLimited::retry_after_secs` ist – anders als bei `Transient` –
        // nicht optional (der Provider liefert immer einen Wert, notfalls
        // einen Fallback; siehe `anthropic.rs::parse_retry_after`).
        ModelError::RateLimited {
            retry_after_secs, ..
        } => Some(Duration::from_secs(*retry_after_secs)),
        _ => None,
    };
    match hint {
        Some(hint) if hint > policy.max_retry_after => RetryDecision::GiveUp,
        Some(hint) => RetryDecision::Retry(hint.max(backoff)),
        None => RetryDecision::Retry(backoff),
    }
}

/// Zufallsquelle für den Backoff-Jitter.
pub trait JitterSource: Send + Sync {
    /// Liefert den nächsten Wert in `[0, 1)`.
    fn next_unit(&self) -> f64;
}

/// Std-only Jitter-Quelle (SipHash mit zufälligem Prozess-Schlüssel über
/// einem Zähler). Nicht kryptographisch; nur zur Entzerrung von Retries.
#[derive(Debug, Default)]
pub struct StdJitter {
    state: RandomState,
    counter: AtomicU64,
}

impl JitterSource for StdJitter {
    fn next_unit(&self) -> f64 {
        let mut hasher = self.state.build_hasher();
        hasher.write_u64(self.counter.fetch_add(1, Ordering::Relaxed));
        // 53 Bit Mantisse → gleichverteilt in [0, 1).
        (hasher.finish() >> 11) as f64 / (1_u64 << 53) as f64
    }
}

/// Wartet eine Backoff-Zeit ab (injizierbare Uhr).
pub trait RetrySleeper: Send + Sync {
    /// Startet eine Wartezeit von `duration`.
    ///
    /// # Errors
    /// [`HttpProviderError::TimerUnavailable`], wenn kein Timer gestartet
    /// werden konnte.
    fn sleep(&self, duration: Duration) -> SleepFuture;
}

/// Runtime-unabhängiger Sleeper: ein Thread je Wartezeit.
///
/// # Concurrency
/// Der Thread wartet mit `recv_timeout` auf einem Kanal; wird das Future vor
/// Ablauf gedroppt (z. B. Abbruch), trennt sich der Kanal und der Thread
/// endet sofort. Der `JoinHandle` wird bewusst nicht gehalten.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreadSleeper;

// Geteilter Zustand zwischen Timer-Thread und Future.
#[derive(Default)]
struct TimerState {
    done: bool,
    waker: Option<Waker>,
}

// Future über den Timer-Thread; `_stop` trennt beim Drop den Kanal.
struct ThreadSleep {
    shared: Arc<Mutex<TimerState>>,
    _stop: mpsc::Sender<()>,
}

impl Future for ThreadSleep {
    type Output = Result<(), HttpProviderError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if state.done {
            Poll::Ready(Ok(()))
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl RetrySleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) -> SleepFuture {
        let shared = Arc::new(Mutex::new(TimerState::default()));
        let (stop, stopped) = mpsc::channel::<()>();
        let thread_shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("harw-retry-sleep".to_owned())
            .spawn(move || {
                if let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(duration) {
                    let waker = {
                        let mut state =
                            thread_shared.lock().unwrap_or_else(PoisonError::into_inner);
                        state.done = true;
                        state.waker.take()
                    };
                    if let Some(waker) = waker {
                        waker.wake();
                    }
                }
            });
        match spawned {
            Ok(_detached) => Box::pin(ThreadSleep {
                shared,
                _stop: stop,
            }),
            Err(_) => Box::pin(std::future::ready(Err(
                HttpProviderError::TimerUnavailable {
                    reason: "backoff thread could not be spawned".to_owned(),
                },
            ))),
        }
    }
}

// Ergebnis des Wettlaufs zwischen Wartezeit und Abbruch.
enum Waited {
    Elapsed(Result<(), HttpProviderError>),
    Cancelled,
}

/// Provider-Hülle mit Wiederholung vorübergehender Fehler.
///
/// # Description
/// Siehe Moduldoku. Ohne [`Self::with_cancel`] hält die Hülle einen eigenen,
/// nie abgebrochenen Token.
///
/// # Concurrency
/// `Send + Sync`; die Anfrage wird je Versuch geklont (nur wenn noch ein
/// weiterer Versuch möglich ist).
pub struct RetryingProvider<P: ModelProvider> {
    inner: P,
    policy: RetryPolicy,
    sleeper: Arc<dyn RetrySleeper>,
    jitter: Arc<dyn JitterSource>,
    cancel: CancelToken,
}

impl<P: ModelProvider> RetryingProvider<P> {
    /// Umhüllt `inner` mit `policy`, [`ThreadSleeper`] und [`StdJitter`].
    ///
    /// # Arguments
    /// - `inner` (`P`): der eigentliche Provider (übernommen).
    /// - `policy` ([`RetryPolicy`]): Strategie.
    #[must_use]
    pub fn new(inner: P, policy: RetryPolicy) -> Self {
        Self {
            inner,
            policy,
            sleeper: Arc::new(ThreadSleeper),
            jitter: Arc::new(StdJitter::default()),
            cancel: CancelToken::new(),
        }
    }

    /// Ersetzt den Sleeper (z. B. aufzeichnender Test-Sleeper).
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Arc<dyn RetrySleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// Ersetzt die Jitter-Quelle (z. B. fester Wert im Test).
    #[must_use]
    pub fn with_jitter(mut self, jitter: Arc<dyn JitterSource>) -> Self {
        self.jitter = jitter;
        self
    }

    /// Setzt den Abbruch-Token, der zwischen Versuchen geprüft wird.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Liefert die konfigurierte Strategie.
    #[must_use]
    pub fn policy(&self) -> &RetryPolicy {
        &self.policy
    }

    // Wartet `delay` ab oder bricht bei Cancel früher ab.
    async fn wait(&self, delay: Duration) -> Waited {
        let mut sleep = self.sleeper.sleep(delay);
        let cancelled = self.cancel.cancelled();
        let mut cancelled = std::pin::pin!(cancelled);
        std::future::poll_fn(move |cx| {
            if cancelled.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Waited::Cancelled);
            }
            sleep.as_mut().poll(cx).map(Waited::Elapsed)
        })
        .await
    }

    // Racet einen einzelnen Versuch (`self.inner.respond(request)`) gegen
    // BEIDE Abbruchsignale: den Hüllen-Token (`self.cancel`, i. d. R. nie
    // gesetzt — siehe `with_cancel`) und den Request-eigenen Token
    // (`request.cancel`, W3/C-CANCEL). Bislang lief nur `wait()` zwischen
    // Versuchen gegen `self.cancel`; der eigentliche Modell-Aufruf racete
    // gegen nichts. Kein `tokio::select!`, weil dieses Crate `tokio` ohne
    // das Feature `macros` einbindet — manuelles `poll_fn`-Racing wie schon
    // bei `wait()`.
    async fn race_respond(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let request_cancel = request.cancel.clone();
        let mut respond = self.inner.respond(request);
        let self_cancelled = self.cancel.cancelled();
        let mut self_cancelled = std::pin::pin!(self_cancelled);
        let request_cancelled = async move {
            match request_cancel {
                Some(token) => token.cancelled().await,
                None => std::future::pending().await,
            }
        };
        let mut request_cancelled = std::pin::pin!(request_cancelled);
        std::future::poll_fn(move |cx| {
            if self_cancelled.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(ModelError::Cancelled));
            }
            if request_cancelled.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(ModelError::Cancelled));
            }
            respond.as_mut().poll(cx)
        })
        .await
    }
}

impl<P: ModelProvider> ModelProvider for RetryingProvider<P> {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move {
            let max_attempts = self.policy.max_attempts.max(1);
            let mut pending = Some(request);
            let mut attempt: u32 = 1;
            loop {
                if self.cancel.is_cancelled() {
                    return Err(ModelError::Cancelled);
                }
                let current = if attempt >= max_attempts {
                    pending.take()
                } else {
                    pending.clone()
                };
                let Some(current) = current else {
                    return Err(ModelError::RequestFailed(
                        "retry state lost the pending request".to_owned(),
                    ));
                };
                let error = match self.race_respond(current).await {
                    Ok(response) => return Ok(response),
                    Err(error) => error,
                };
                if attempt >= max_attempts {
                    return Err(error);
                }
                let decision =
                    retry_decision(&self.policy, &error, attempt - 1, self.jitter.next_unit());
                let RetryDecision::Retry(delay) = decision else {
                    return Err(error);
                };
                tracing::warn!(
                    attempt,
                    max_attempts,
                    delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    error = %error,
                    "retrying transient model error"
                );
                match self.wait(delay).await {
                    Waited::Cancelled => return Err(ModelError::Cancelled),
                    Waited::Elapsed(Err(timer_error)) => {
                        tracing::warn!(error = %timer_error, "retry backoff unavailable");
                        return Err(error);
                    }
                    Waited::Elapsed(Ok(())) => {}
                }
                attempt += 1;
            }
        })
    }

    /// Reicht die gepinnte Modell-ID des umhüllten Providers durch.
    ///
    /// # Description
    /// Wiederholungen ändern das angesprochene Modell nicht; ohne dieses
    /// Durchreichen ginge ein Pin des inneren Providers (z. B.
    /// [`harw_core::PinnedModelProvider`]) hinter der Retry-Hülle verloren.
    ///
    /// # Returns
    /// `self.inner.pinned_model_id()`.
    fn pinned_model_id(&self) -> Option<String> {
        self.inner.pinned_model_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_core::cancel::CancelReason;

    fn policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 4,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
            max_retry_after: Duration::from_secs(60),
            retry_timeouts: true,
        }
    }

    fn transient(retry_after_secs: Option<u64>) -> ModelError {
        ModelError::Transient {
            status: Some(503),
            retry_after_secs,
            message: "busy".to_owned(),
        }
    }

    #[test]
    fn test_backoff_delay_grows_exponentially_and_is_capped() {
        let policy = policy();
        assert_eq!(policy.backoff_delay(0, 0.0), Duration::from_millis(50));
        assert_eq!(policy.backoff_delay(0, 1.0), Duration::from_millis(100));
        assert_eq!(policy.backoff_delay(2, 1.0), Duration::from_millis(400));
        assert_eq!(policy.backoff_delay(10, 1.0), Duration::from_secs(1));
        assert_eq!(
            policy.backoff_delay(u32::MAX, 0.0),
            Duration::from_millis(500)
        );
        assert_eq!(policy.backoff_delay(0, f64::NAN), Duration::from_millis(50));
    }

    /// Runde 7, Teil L4: lokale Provider wiederholen Zeitlimit-Fehler nicht,
    /// wohl aber andere vorübergehende Fehler.
    #[test]
    fn test_retry_decision_skips_timeouts_when_disabled() {
        let policy = RetryPolicy {
            retry_timeouts: false,
            ..policy()
        };
        let timeout = ModelError::Timeout {
            message: "stream idle".to_owned(),
        };
        assert_eq!(
            retry_decision(&policy, &timeout, 0, 0.0),
            RetryDecision::GiveUp
        );
        assert!(matches!(
            retry_decision(&policy, &transient(None), 0, 0.0),
            RetryDecision::Retry(_)
        ));
    }

    #[test]
    fn test_retry_decision_table() {
        let policy = policy();
        assert_eq!(
            retry_decision(&policy, &transient(None), 0, 1.0),
            RetryDecision::Retry(Duration::from_millis(100))
        );
        assert_eq!(
            retry_decision(&policy, &transient(Some(5)), 0, 1.0),
            RetryDecision::Retry(Duration::from_secs(5))
        );
        assert_eq!(
            retry_decision(&policy, &transient(Some(61)), 0, 1.0),
            RetryDecision::GiveUp
        );
        let rate_limited = ModelError::RateLimited {
            retry_after_secs: 5,
            message: "429".to_owned(),
        };
        assert_eq!(
            retry_decision(&policy, &rate_limited, 0, 1.0),
            RetryDecision::Retry(Duration::from_secs(5))
        );
        let rate_limited_over_cap = ModelError::RateLimited {
            retry_after_secs: 61,
            message: "429".to_owned(),
        };
        assert_eq!(
            retry_decision(&policy, &rate_limited_over_cap, 0, 1.0),
            RetryDecision::GiveUp
        );
        let timeout = ModelError::Timeout {
            message: "t".to_owned(),
        };
        assert!(matches!(
            retry_decision(&policy, &timeout, 0, 0.0),
            RetryDecision::Retry(_)
        ));
        for error in [
            ModelError::QuotaExceeded {
                message: "q".to_owned(),
            },
            ModelError::Auth {
                message: "a".to_owned(),
            },
            ModelError::ContextLength {
                message: "c".to_owned(),
            },
            ModelError::RequestFailed("400".to_owned()),
            ModelError::Cancelled,
        ] {
            assert_eq!(
                retry_decision(&policy, &error, 0, 0.5),
                RetryDecision::GiveUp
            );
        }
    }

    #[test]
    fn test_std_jitter_next_unit_in_range_and_varies() {
        let jitter = StdJitter::default();
        let values: Vec<f64> = (0..16).map(|_| jitter.next_unit()).collect();
        assert!(values.iter().all(|value| (0.0..1.0).contains(value)));
        assert!(values.windows(2).any(|pair| pair[0] != pair[1]));
    }

    #[tokio::test]
    async fn test_thread_sleeper_sleep_completes() -> TestResult {
        let started = std::time::Instant::now();
        ThreadSleeper
            .sleep(Duration::from_millis(20))
            .await
            .map_err(ctx("timer thread available"))?;
        assert!(started.elapsed() >= Duration::from_millis(20));
        Ok(())
    }

    #[test]
    fn test_policy_accessor_returns_configured_policy() {
        let provider = RetryingProvider::new(harw_core::EchoModelProvider::default(), policy());
        assert_eq!(provider.policy(), &policy());
    }

    #[test]
    fn default_policy_has_at_least_five_seconds_first_backoff() {
        let policy = RetryPolicy::default();
        assert!(policy.backoff_delay(0, 0.0) >= Duration::from_secs(5));
    }

    #[tokio::test]
    async fn test_respond_cancelled_before_first_attempt() {
        let cancel = CancelToken::new();
        cancel.cancel(CancelReason::User);
        let provider = RetryingProvider::new(harw_core::EchoModelProvider::default(), policy())
            .with_cancel(cancel);
        let request = harw_core::ModelRequest::new(
            Default::default(),
            Vec::new(),
            harw_core::ConversationHistory::new(),
            Vec::new(),
        );
        assert!(matches!(
            provider.respond(request).await,
            Err(ModelError::Cancelled)
        ));
    }

    #[test]
    fn test_pinned_model_id_is_forwarded_to_the_inner_provider() {
        let echo: Arc<dyn ModelProvider> = Arc::new(harw_core::EchoModelProvider::default());
        let unpinned = RetryingProvider::new(
            harw_core::PinnedModelProvider::new(Arc::clone(&echo), None, None),
            policy(),
        );
        assert_eq!(unpinned.pinned_model_id(), None);

        let pinned = RetryingProvider::new(
            harw_core::PinnedModelProvider::new(
                echo,
                None,
                Some(harw_types::ModelId::from("pinned-model")),
            ),
            policy(),
        );
        assert_eq!(pinned.pinned_model_id().as_deref(), Some("pinned-model"));
    }

    // Provider, der `delay` lang "arbeitet" bevor er erfolgreich antwortet —
    // nur für den Race-Test unten: simuliert einen laufenden
    // `inner.respond`-Aufruf, gegen den ein Cancel racen muss.
    struct SlowProvider {
        delay: Duration,
    }

    impl ModelProvider for SlowProvider {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            let delay = self.delay;
            Box::pin(async move {
                ThreadSleeper
                    .sleep(delay)
                    .await
                    .map_err(|error| ModelError::RequestFailed(error.to_string()))?;
                Ok(ModelResponse::text("slow-done"))
            })
        }
    }

    // Deckt den bislang toten Pfad ab: das Cancel-Signal kommt über
    // `ModelRequest.cancel` (nicht über `RetryingProvider::with_cancel`) und
    // schlägt während der laufende `inner.respond(...)`-Aufruf noch läuft
    // ein (Provider braucht 150ms, Cancel nach 20ms) — muss den Aufruf mit
    // `Err(ModelError::Cancelled)` abbrechen statt die vollen 150ms
    // abzuwarten.
    #[tokio::test]
    async fn test_respond_cancelled_via_request_cancel_token_during_inner_call() {
        let request_cancel = CancelToken::new();
        let mut request = harw_core::ModelRequest::new(
            Default::default(),
            Vec::new(),
            harw_core::ConversationHistory::new(),
            Vec::new(),
        );
        request.cancel = Some(request_cancel.clone());

        let provider = RetryingProvider::new(
            SlowProvider {
                delay: Duration::from_millis(150),
            },
            policy(),
        );

        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            request_cancel.cancel(CancelReason::User);
        });

        let started = std::time::Instant::now();
        assert!(matches!(
            provider.respond(request).await,
            Err(ModelError::Cancelled)
        ));
        // Muss deutlich vor Ablauf der vollen 150ms zurückkommen — sonst hat
        // das Cancel nicht gegen den `inner.respond`-Aufruf gerennt, sondern
        // ist wirkungslos verpufft.
        assert!(started.elapsed() < Duration::from_millis(150));
    }
}
