//! Proaktiver Rate-Limit-Pacer für HTTP-Provider (W-Token-Effizienz).
//!
//! ## Zweck
//! Anthropic (und kompatible Provider wie OpenAI/DashScope) melden ihr
//! aktuelles Rate-Limit-Budget über Antwort-Header. Statt erst auf einen
//! harten HTTP-429-Fehler zu reagieren, beobachtet [`ProviderRateLimiter`]
//! diese Header fortlaufend und lässt `harw` **vor** dem nächsten Request
//! kurz warten, sobald ein Kontingent knapp wird. Das entlastet reale
//! Anthropic-Rate-Limits, gegen die Nutzer sonst hart anlaufen.
//!
//! ## Beobachtete Header
//! - **Anthropic-Familie** (`anthropic-ratelimit-{requests,tokens,
//!   input-tokens,output-tokens}-{limit,remaining,reset}`): `reset` ist ein
//!   RFC3339-Zeitstempel, der relativ zu "jetzt" in ein [`std::time::Instant`]
//!   übersetzt wird.
//! - **OpenAI/DashScope-Familie** (`x-ratelimit-{limit,remaining}-
//!   {requests,tokens}`, `x-ratelimit-reset-{requests,tokens}`): `reset` ist
//!   eine Go-artige Dauer (`"1s"`, `"6m0s"`, `"250ms"`, `"1m30.5s"`).
//! - **`retry-after`** (Sekunden): globaler, providerunabhängiger
//!   Warte-Hinweis, unabhängig von einer bestimmten Dimension.
//!
//! Unparsbare Werte werden ignoriert (nur `tracing::debug!`), niemals als
//! Fehler nach außen gemeldet.
//!
//! ## Nebenläufigkeit
//! Der interne Zustand liegt in einem `std::sync::Mutex<State>`. Der Lock
//! wird ausschließlich in kurzen, synchronen Funktionen gehalten
//! ([`ProviderRateLimiter::observe_headers`],
//! [`ProviderRateLimiter::pending_wait`]) und **niemals** über ein `.await`
//! hinweg gehalten — [`ProviderRateLimiter::wait_for_slot`] liest die nötige
//! Wartezeit synchron aus, gibt den Lock frei und schläft danach mit
//! `tokio::time::sleep`.
//!
//! ## Schätzungsbewusste Zulassung (Welle 3)
//! [`ProviderRateLimiter::wait_for_slot_with_estimate`] berücksichtigt die
//! geschätzte Eingabegröße des nächsten Requests:
//! - **Fail-fast:** Übersteigt die Schätzung das *Limit* einer
//!   Token-Dimension (`input_tokens` bzw. `tokens`) selbst, kann der Request
//!   nie zugelassen werden — statt zu warten wird sofort
//!   [`RateBudgetError::RequestExceedsLimit`] geliefert.
//! - **Schätzungsbewusstes Warten:** Reicht `remaining` nicht für die
//!   Schätzung (oder liegt es bereits im Sicherheitsabstand), wird bis zum
//!   (geschätzten) Reset gewartet.
//! - **Optimistisches Dekrementieren:** Jede Zulassung zieht unter dem Lock
//!   sofort `1` Request und die geschätzten Tokens von `remaining` ab; die
//!   nächste Antwort überschreibt den Wert wieder mit dem autoritativen
//!   Header-Stand.
//! - **Kein Ansturm nach dem Reset:** Nach Ablauf von `reset_at` wird
//!   `remaining` auf `limit` aufgefüllt und ein neues Fenster
//!   (`jetzt + beobachtete Fensterlänge`) geschätzt. Wartende, die
//!   gleichzeitig aufwachen, werden nacheinander unter dem Lock geprüft und
//!   dekrementieren je Zulassung — wer nicht mehr hineinpasst, wartet auf das
//!   nächste geschätzte Fenster, statt dass alle gleichzeitig losschicken.
//! - Insgesamt wird pro Aufruf höchstens `MAX_WAIT` gewartet; danach wird der
//!   Request trotzdem zugelassen (der Provider antwortet notfalls mit 429).
//!
//! [`ProviderRateLimiter::wait_for_slot`] bleibt unverändert (ohne Schätzung,
//! ohne Dekrementieren); [`ProviderRateLimiter::record_request_estimate`]
//! erlaubt Aufrufern dieses Pfads, einen Request nachträglich optimistisch
//! zu verbuchen.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;

/// Obergrenze für eine einzelne Wartezeit pro `wait_for_slot`-Aufruf.
const MAX_WAIT: Duration = Duration::from_secs(120);

/// Obergrenze der gemeinsamen 429-Abkühlphase
/// ([`ProviderRateLimiter::note_rate_limit_cooldown`]).
const MAX_COOLDOWN: Duration = Duration::from_secs(10 * 60);

/// Angenommene Fensterlänge, wenn nach einem abgelaufenen Reset kein
/// beobachtetes Fenster bekannt ist (typische Limits sind "pro Minute").
const DEFAULT_WINDOW: Duration = Duration::from_secs(60);

/// Fehler der schätzungsbewussten Zulassung
/// ([`ProviderRateLimiter::wait_for_slot_with_estimate`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RateBudgetError {
    /// Die geschätzte Eingabe eines einzelnen Requests übersteigt das Limit
    /// einer Token-Dimension selbst — Warten hilft nie, der Aufrufer muss
    /// den Request verkleinern (z. B. Kontext kompaktieren).
    RequestExceedsLimit {
        /// Betroffene Dimension (`"input_tokens"` oder `"tokens"`).
        dimension: &'static str,
        /// Geschätzte Eingabe-Tokens des Requests.
        estimated_tokens: u64,
        /// Vom Provider gemeldetes Limit der Dimension.
        limit: u64,
    },
}

impl fmt::Display for RateBudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequestExceedsLimit {
                dimension,
                estimated_tokens,
                limit,
            } => write!(
                f,
                "Request übersteigt das Rate-Limit des Providers: geschätzt \
                 {estimated_tokens} Tokens, Limit der Dimension '{dimension}' ist \
                 {limit} Tokens; der Request muss verkleinert werden"
            ),
        }
    }
}

impl std::error::Error for RateBudgetError {}

/// Ergebnis einer einzelnen Zulassungsprüfung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    /// Zugelassen; Kontingent wurde bereits optimistisch dekrementiert.
    Admitted,
    /// Noch nicht zugelassen; `wait` bis zur nächsten Prüfung schlafen.
    Wait {
        dimension: &'static str,
        wait: Duration,
    },
}

/// Kontingent-Zustand einer einzelnen Rate-Limit-Dimension.
///
/// `limit`/`remaining` stammen aus den zuletzt beobachteten Headern,
/// `reset_at` ist der (relativ zu `Instant::now()` umgerechnete) Zeitpunkt,
/// zu dem das Kontingent laut Provider zurückgesetzt wird.
#[derive(Debug, Clone, Copy, Default)]
struct DimensionState {
    limit: Option<u64>,
    remaining: Option<u64>,
    reset_at: Option<Instant>,
    /// Zuletzt beobachtete Fensterlänge (`reset_at` minus
    /// Beobachtungszeitpunkt); dient nach einem abgelaufenen Reset zur
    /// Schätzung des nächsten Fensters.
    window: Option<Duration>,
}

/// Gesamtzustand des Pacers über alle bekannten Dimensionen.
#[derive(Debug, Clone, Default)]
struct State {
    requests: DimensionState,
    input_tokens: DimensionState,
    output_tokens: DimensionState,
    tokens: DimensionState,
    /// Globaler Warte-Hinweis aus einem `retry-after`-Header.
    retry_after_until: Option<Instant>,
}

/// Proaktiver Pacer, der HTTP-Rate-Limit-Header beobachtet und vor dem
/// nächsten Request wartet, sobald ein Kontingent knapp wird.
///
/// # Description
/// Trägt keine Provider-spezifische Logik; er kennt nur die zwei gängigen
/// Header-Familien (Anthropic, OpenAI/DashScope) und einen generischen
/// `retry-after`-Hinweis. Der Aufrufer (Provider-Implementierung) ruft nach
/// jeder HTTP-Antwort [`Self::observe_headers`] auf und vor jedem neuen
/// Request [`Self::wait_for_slot`].
pub struct ProviderRateLimiter {
    enabled: bool,
    safety_margin_pct: u8,
    state: Mutex<State>,
    /// Zähler beobachteter HTTP-429-Antworten dieses Providers (W6b —
    /// UIA-Sichtbarkeit auf Provider-Concurrency/Rate-Limit-Zustand). Läuft
    /// unabhängig von `enabled`/`safety_margin_pct`: auch ein deaktivierter
    /// Pacer soll melden können, dass der Provider tatsächlich 429
    /// zurückgegeben hat. Der Aufrufer (siehe `lib.rs::respond_once`) ruft
    /// [`Self::record_rate_limited`] genau dort auf, wo Status 429 erkannt
    /// wird — unabhängig davon, in welche [`harw_core::ModelError`]-Variante
    /// (`QuotaExceeded` oder `Transient{status: Some(429)}`) der Fehler
    /// anschließend übersetzt wird.
    rate_limited_count: AtomicU64,
    /// Gemeinsame Abkühlphase nach einem HTTP 429 (Ende der vom Provider
    /// verlangten Wartezeit). Läuft — wie der Zähler — unabhängig von
    /// `enabled`: alle Requests dieses Providers (UIA **und** Kinder teilen
    /// sich die Instanz) warten sie ab, statt parallel erneut in dasselbe
    /// Limit zu laufen. Sichtbar als `rate_limit_wait` im Load-Status.
    cooldown_until: Mutex<Option<Instant>>,
}

impl fmt::Debug for ProviderRateLimiter {
    /// Gibt Konfiguration, aber keinen Laufzeitzustand aus (kein Lock im
    /// `Debug`-Pfad nötig, da nur `enabled`/`safety_margin_pct` gedruckt
    /// werden).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRateLimiter")
            .field("enabled", &self.enabled)
            .field("safety_margin_pct", &self.safety_margin_pct)
            .field("rate_limited_count", &self.rate_limited_count())
            .finish_non_exhaustive()
    }
}

impl ProviderRateLimiter {
    /// Erstellt einen Pacer aus der optionalen TOML-Konfiguration.
    ///
    /// # Description
    /// Fehlt die Konfiguration (`None`), ist der Pacer deaktiviert
    /// (`enabled = false`) mit einem plausiblen Default-Sicherheitsabstand
    /// von 10 %, der ohnehin nie ausgewertet wird, solange der Pacer
    /// deaktiviert bleibt.
    ///
    /// # Arguments
    /// - `config` (`Option<harw_config::RateLimitToml>`): providerspezifische
    ///   Rate-Limit-Konfiguration, sofern in der `provider.toml` gesetzt.
    ///
    /// # Returns
    /// Ein neuer, leerer [`ProviderRateLimiter`] ohne beobachtete Header.
    pub fn new(config: Option<harw_config::RateLimitToml>) -> Self {
        let (enabled, safety_margin_pct) = match config {
            Some(cfg) => (cfg.enabled, cfg.safety_margin_pct),
            None => (false, 10),
        };
        Self {
            enabled,
            safety_margin_pct,
            state: Mutex::new(State::default()),
            rate_limited_count: AtomicU64::new(0),
            cooldown_until: Mutex::new(None),
        }
    }

    /// Meldet, ob der Pacer aktiv ist.
    ///
    /// # Returns
    /// `true`, wenn `observe_headers`/`wait_for_slot` tatsächlich wirken.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Zählt eine vom Aufrufer beobachtete HTTP-429-Antwort dieses Providers.
    ///
    /// # Description
    /// Reiner Beobachtungszähler (kein Pacing-Effekt) — läuft unabhängig von
    /// [`Self::is_enabled`], damit auch ein Provider ohne konfigurierten
    /// Rate-Limit-Pacer meldet, dass er tatsächlich 429 zurückgegeben hat.
    /// Der Aufrufer ruft dies genau dort auf, wo Status 429 aus einer
    /// HTTP-Antwort gelesen wird, unabhängig davon, in welche
    /// `harw_core::ModelError`-Variante der Fehler danach übersetzt wird.
    ///
    /// # Concurrency
    /// Lock-frei (`AtomicU64::fetch_add`); sicher von mehreren Threads/Tasks
    /// gleichzeitig aufrufbar.
    pub fn record_rate_limited(&self) {
        self.rate_limited_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Liefert die Gesamtzahl der seit Konstruktion beobachteten
    /// HTTP-429-Antworten dieses Providers.
    ///
    /// # Returns
    /// Monoton wachsender Zähler; setzt sich nie selbst zurück (im
    /// Unterschied zum internen Pacing-Zustand, den [`Self::wait_for_slot`]
    /// je Dimension zurücksetzt).
    ///
    /// # Concurrency
    /// Lock-frei; sicher von mehreren Threads/Tasks gleichzeitig aufrufbar.
    #[must_use]
    pub fn rate_limited_count(&self) -> u64 {
        self.rate_limited_count.load(Ordering::Relaxed)
    }

    /// Merkt nach einem HTTP 429 eine gemeinsame Abkühlphase von `wait` vor.
    ///
    /// # Description
    /// Unabhängig von [`Self::is_enabled`]. Eine bereits länger laufende
    /// Abkühlphase wird nie verkürzt. `wait` wird auf [`MAX_COOLDOWN`]
    /// gedeckelt (ein Stunden-Kontingent soll keine späteren Requests
    /// stundenlang still blockieren; die Retry-Hülle gibt bei solchen
    /// Hinweisen ohnehin sofort auf).
    ///
    /// # Concurrency
    /// Kurzer, synchroner Lock; nie über ein `.await` gehalten.
    pub fn note_rate_limit_cooldown(&self, wait: Duration) {
        if wait.is_zero() {
            return;
        }
        let until = Instant::now() + wait.min(MAX_COOLDOWN);
        let mut cooldown = match self.cooldown_until.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if cooldown.is_none_or(|current| current < until) {
            *cooldown = Some(until);
        }
    }

    /// Restdauer der gemeinsamen 429-Abkühlphase, falls eine läuft.
    #[must_use]
    pub fn cooldown_remaining(&self) -> Option<Duration> {
        let cooldown = match self.cooldown_until.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        };
        cooldown
            .map(|until| until.saturating_duration_since(Instant::now()))
            .filter(|remaining| !remaining.is_zero())
    }

    /// Wartet eine laufende 429-Abkühlphase ab (höchstens [`MAX_WAIT`]).
    ///
    /// # Description
    /// Wird vor dem Belegen eines Nebenläufigkeits-Slots aufgerufen, damit
    /// ein wartender Request keinen Slot blockiert. Abbruchsicher: das
    /// Future darf jederzeit gedroppt werden.
    pub async fn wait_for_cooldown(&self) {
        let Some(remaining) = self.cooldown_remaining() else {
            return;
        };
        let wait = remaining.min(MAX_WAIT);
        tracing::info!(
            wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            "provider.rate_limit.cooldown_wait"
        );
        tokio::time::sleep(wait).await;
    }

    /// Aktuell fällige Wartezeit für die Statusanzeige: das Maximum aus
    /// Header-Pacing ([`Self::pending_wait`], nur wenn aktiv) und der
    /// gemeinsamen 429-Abkühlphase ([`Self::cooldown_remaining`]).
    #[must_use]
    pub fn status_wait(&self) -> Option<Duration> {
        match (self.pending_wait(), self.cooldown_remaining()) {
            (Some(pacing), Some(cooldown)) => Some(pacing.max(cooldown)),
            (pacing, cooldown) => pacing.or(cooldown),
        }
    }

    /// Wertet Rate-Limit-Header einer HTTP-Antwort aus und aktualisiert den
    /// internen Zustand je Dimension.
    ///
    /// # Description
    /// No-op, wenn der Pacer deaktiviert ist. Unparsbare Werte werden pro
    /// Header ignoriert und nur via `tracing::debug!` protokolliert; ein
    /// einzelner fehlerhafter Header verwirft nie die übrigen.
    ///
    /// # Arguments
    /// - `headers` (`&reqwest::header::HeaderMap`): Antwort-Header des
    ///   zuletzt ausgeführten Requests.
    ///
    /// # Concurrency
    /// Hält den internen `Mutex` nur für die Dauer dieses synchronen
    /// Aufrufs, niemals über ein `.await` hinweg.
    pub fn observe_headers(&self, headers: &HeaderMap) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        let mut state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        apply_anthropic_family(
            &mut state.requests,
            headers,
            "anthropic-ratelimit-requests",
            now,
        );
        apply_anthropic_family(
            &mut state.tokens,
            headers,
            "anthropic-ratelimit-tokens",
            now,
        );
        apply_anthropic_family(
            &mut state.input_tokens,
            headers,
            "anthropic-ratelimit-input-tokens",
            now,
        );
        apply_anthropic_family(
            &mut state.output_tokens,
            headers,
            "anthropic-ratelimit-output-tokens",
            now,
        );

        apply_openai_family(&mut state.requests, headers, "requests", now);
        apply_openai_family(&mut state.tokens, headers, "tokens", now);

        if let Some(value) = header_str(headers, "retry-after") {
            match value.trim().parse::<u64>() {
                Ok(secs) => state.retry_after_until = Some(now + Duration::from_secs(secs)),
                Err(_) => {
                    tracing::debug!(
                        header = "retry-after",
                        value,
                        "provider.rate_limit.header_parse_failed"
                    );
                }
            }
        }
    }

    /// Wartet asynchron, bis wieder ausreichend Kontingent vorhanden ist.
    ///
    /// # Description
    /// No-op, wenn deaktiviert oder kein Kontingent knapp ist. Andernfalls
    /// wird einmal (maximal `MAX_WAIT` = 120 s) geschlafen und danach der
    /// `remaining`-Wert der auslösenden Dimension zurückgesetzt, da er bis
    /// zur nächsten Antwort unbekannt ist.
    ///
    /// # Concurrency
    /// Der `Mutex`-Lock wird nur innerhalb von [`Self::pending_wait_dimension`]
    /// gehalten (synchron) und vor dem `.await` wieder freigegeben.
    pub async fn wait_for_slot(&self) {
        if !self.enabled {
            return;
        }
        let Some((dimension, wait)) = self.pending_wait_dimension() else {
            return;
        };
        tracing::info!(
            dimension,
            wait_ms = wait.as_millis() as u64,
            "provider.rate_limit.pacing"
        );
        tokio::time::sleep(wait).await;
        self.clear_dimension(dimension);
    }

    /// Wartet schätzungsbewusst auf ein Kontingent für einen Request mit
    /// `estimated_input_tokens` Eingabe-Tokens und verbucht ihn optimistisch.
    ///
    /// # Description
    /// No-op (`Ok(())`), wenn der Pacer deaktiviert ist. Sonst wird in einer
    /// Schleife unter dem Lock geprüft (Lock nie über `.await` gehalten):
    /// 1. Übersteigt die Schätzung das Limit von `input_tokens` oder
    ///    `tokens` → sofort [`RateBudgetError::RequestExceedsLimit`].
    /// 2. Abgelaufene Fenster werden auf `limit` aufgefüllt und ein neues
    ///    Fenster geschätzt.
    /// 3. Reicht `remaining` nicht (Requests: 1, Token-Dimensionen:
    ///    Schätzung) oder liegt es im Sicherheitsabstand, wird bis zum
    ///    Reset geschlafen und erneut geprüft.
    /// 4. Sonst wird zugelassen und `remaining` sofort dekrementiert.
    ///
    /// Nach insgesamt `MAX_WAIT` wird der Request ohne weiteres Warten
    /// zugelassen (und trotzdem verbucht).
    ///
    /// # Arguments
    /// - `estimated_input_tokens` (`u64`): geschätzte Eingabe-Tokens des
    ///   nächsten Requests.
    ///
    /// # Errors
    /// [`RateBudgetError::RequestExceedsLimit`], wenn der einzelne Request
    /// nie in das gemeldete Limit passt.
    pub async fn wait_for_slot_with_estimate(
        &self,
        estimated_input_tokens: u64,
    ) -> Result<(), RateBudgetError> {
        if !self.enabled {
            return Ok(());
        }
        let deadline = Instant::now() + MAX_WAIT;
        loop {
            let now = Instant::now();
            let force = now >= deadline;
            match self.try_admit(estimated_input_tokens, now, force)? {
                Admission::Admitted => return Ok(()),
                Admission::Wait { dimension, wait } => {
                    let wait = wait.min(deadline.saturating_duration_since(now));
                    tracing::info!(
                        dimension,
                        estimated_input_tokens,
                        wait_ms = wait.as_millis() as u64,
                        "provider.rate_limit.pacing"
                    );
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    /// Verbucht einen Request optimistisch, ohne zu warten.
    ///
    /// # Description
    /// Für Aufrufer, die [`Self::wait_for_slot`] (ohne Schätzung) nutzen:
    /// zieht `1` vom Request-Kontingent und `estimated_input_tokens` von
    /// `input_tokens`/`tokens` ab (sättigend, nur wo `remaining` bekannt
    /// ist). Abgelaufene Fenster werden vorher aufgefüllt. No-op, wenn der
    /// Pacer deaktiviert ist. Die nächste Antwort überschreibt die Werte
    /// wieder mit dem Header-Stand.
    ///
    /// # Arguments
    /// - `estimated_input_tokens` (`u64`): geschätzte Eingabe-Tokens des
    ///   gerade gesendeten Requests.
    pub fn record_request_estimate(&self, estimated_input_tokens: u64) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        let mut state = self.lock_state();
        refill_expired(&mut state, now);
        commit_admission(&mut state, estimated_input_tokens);
    }

    /// Prüft synchron, ob ein Request dieser Größe überhaupt jemals in die
    /// gemeldeten Limits passt (ohne zu warten oder zu verbuchen).
    ///
    /// # Errors
    /// [`RateBudgetError::RequestExceedsLimit`], wenn die Schätzung das
    /// Limit von `input_tokens` oder `tokens` übersteigt. Deaktiviert immer
    /// `Ok(())`.
    pub fn check_request_budget(&self, estimated_input_tokens: u64) -> Result<(), RateBudgetError> {
        if !self.enabled {
            return Ok(());
        }
        let state = self.lock_state();
        check_fits_limit(&state, estimated_input_tokens)
    }

    /// Vorschau auf [`Self::wait_for_slot_with_estimate`] ohne
    /// Seiteneffekt (auch für Tests/Statusanzeigen): rechnet auf einer Kopie des Zustands und liefert die
    /// fällige Wartezeit (`None` = würde sofort zugelassen).
    ///
    /// # Errors
    /// Wie [`Self::check_request_budget`].
    pub fn pending_wait_for_estimate(
        &self,
        estimated_input_tokens: u64,
    ) -> Result<Option<Duration>, RateBudgetError> {
        if !self.enabled {
            return Ok(None);
        }
        let mut snapshot = self.lock_state().clone();
        let decision = evaluate_admission(
            &mut snapshot,
            estimated_input_tokens,
            self.safety_margin_pct,
            Instant::now(),
            false,
        )?;
        Ok(match decision {
            Admission::Admitted => None,
            Admission::Wait { wait, .. } => Some(wait),
        })
    }

    /// Eine Zulassungsprüfung unter dem Lock (synchron, kein `.await`).
    fn try_admit(
        &self,
        estimated_input_tokens: u64,
        now: Instant,
        force: bool,
    ) -> Result<Admission, RateBudgetError> {
        let mut state = self.lock_state();
        evaluate_admission(
            &mut state,
            estimated_input_tokens,
            self.safety_margin_pct,
            now,
            force,
        )
    }

    /// Sperrt den Zustand; ein vergifteter Lock wird übernommen, da der
    /// Zustand nur aus Hinweisen besteht.
    fn lock_state(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Liefert die aktuell fällige Wartezeit, ohne zu schlafen.
    ///
    /// # Description
    /// Testbarer Kern von [`Self::wait_for_slot`]: berechnet dieselbe
    /// Entscheidung synchron und gibt nur die Dauer zurück (kein
    /// Dimensionslabel), damit Tests ohne `tokio::time::sleep` auskommen.
    ///
    /// # Returns
    /// `None`, wenn kein Kontingent knapp ist oder der Pacer deaktiviert
    /// ist; sonst die auf `MAX_WAIT` gekappte Wartezeit.
    pub(crate) fn pending_wait(&self) -> Option<Duration> {
        if !self.enabled {
            return None;
        }
        self.pending_wait_dimension().map(|(_, wait)| wait)
    }

    /// Ermittelt die Dimension mit dem größten aktuell fälligen
    /// Wartebedarf, falls vorhanden.
    fn pending_wait_dimension(&self) -> Option<(&'static str, Duration)> {
        let now = Instant::now();
        let state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let mut candidates: Vec<(&'static str, Duration)> = Vec::new();

        if let Some(reset) = state.retry_after_until {
            if reset > now {
                candidates.push(("retry_after", (reset - now).min(MAX_WAIT)));
            }
        }

        for (label, dim) in [
            ("requests", &state.requests),
            ("input_tokens", &state.input_tokens),
            ("output_tokens", &state.output_tokens),
            ("tokens", &state.tokens),
        ] {
            if let (Some(limit), Some(remaining), Some(reset_at)) =
                (dim.limit, dim.remaining, dim.reset_at)
            {
                if reset_at > now
                    && remaining.saturating_mul(100)
                        <= limit.saturating_mul(u64::from(self.safety_margin_pct))
                {
                    candidates.push((label, (reset_at - now).min(MAX_WAIT)));
                }
            }
        }

        candidates.into_iter().max_by_key(|(_, wait)| *wait)
    }

    /// Setzt `remaining` (bzw. den globalen Hinweis) der übergebenen
    /// Dimension zurück, nachdem darauf gewartet wurde.
    fn clear_dimension(&self, dimension: &str) {
        let mut state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        match dimension {
            "requests" => state.requests.remaining = None,
            "input_tokens" => state.input_tokens.remaining = None,
            "output_tokens" => state.output_tokens.remaining = None,
            "tokens" => state.tokens.remaining = None,
            "retry_after" => state.retry_after_until = None,
            _ => {}
        }
    }
}

/// Fail-fast-Prüfung: passt ein Request dieser Größe überhaupt jemals in
/// die Token-Limits?
fn check_fits_limit(state: &State, estimate: u64) -> Result<(), RateBudgetError> {
    for (dimension, dim) in [
        ("input_tokens", &state.input_tokens),
        ("tokens", &state.tokens),
    ] {
        if let Some(limit) = dim.limit {
            if limit > 0 && estimate > limit {
                return Err(RateBudgetError::RequestExceedsLimit {
                    dimension,
                    estimated_tokens: estimate,
                    limit,
                });
            }
        }
    }
    Ok(())
}

/// Füllt Dimensionen, deren Reset abgelaufen ist, auf `limit` auf und
/// schätzt das nächste Fenster (`now + window`); ein abgelaufener
/// `retry-after`-Hinweis wird verworfen.
fn refill_expired(state: &mut State, now: Instant) {
    for dim in [
        &mut state.requests,
        &mut state.input_tokens,
        &mut state.output_tokens,
        &mut state.tokens,
    ] {
        if let (Some(limit), Some(reset_at)) = (dim.limit, dim.reset_at) {
            if reset_at <= now {
                dim.remaining = Some(limit);
                dim.reset_at = Some(now + dim.window.unwrap_or(DEFAULT_WINDOW));
            }
        }
    }
    if state.retry_after_until.is_some_and(|until| until <= now) {
        state.retry_after_until = None;
    }
}

/// Wartebedarf einer Dimension für einen Request, der `needed` Einheiten
/// verbraucht: gewartet wird, wenn `remaining` nicht reicht oder bereits im
/// Sicherheitsabstand liegt und ein künftiger Reset bekannt ist.
fn dimension_wait(
    dim: &DimensionState,
    needed: u64,
    safety_margin_pct: u8,
    now: Instant,
) -> Option<Duration> {
    let (Some(limit), Some(remaining), Some(reset_at)) = (dim.limit, dim.remaining, dim.reset_at)
    else {
        return None;
    };
    if reset_at <= now {
        return None;
    }
    let short = remaining < needed;
    let below_margin =
        remaining.saturating_mul(100) <= limit.saturating_mul(u64::from(safety_margin_pct));
    (short || below_margin).then(|| (reset_at - now).min(MAX_WAIT))
}

/// Zieht einen zugelassenen Request optimistisch vom Kontingent ab.
fn commit_admission(state: &mut State, estimate: u64) {
    if let Some(remaining) = state.requests.remaining.as_mut() {
        *remaining = remaining.saturating_sub(1);
    }
    for dim in [&mut state.input_tokens, &mut state.tokens] {
        if let Some(remaining) = dim.remaining.as_mut() {
            *remaining = remaining.saturating_sub(estimate);
        }
    }
}

/// Kern der schätzungsbewussten Zulassung (auf einem gesperrten oder
/// kopierten Zustand): Fail-fast, Auffüllen abgelaufener Fenster, dann
/// entweder Zulassung mit optimistischem Dekrement oder die längste fällige
/// Wartezeit. `force` lässt ohne Warten zu (nach Ablauf von `MAX_WAIT`).
fn evaluate_admission(
    state: &mut State,
    estimate: u64,
    safety_margin_pct: u8,
    now: Instant,
    force: bool,
) -> Result<Admission, RateBudgetError> {
    check_fits_limit(state, estimate)?;
    refill_expired(state, now);

    if !force {
        let mut longest: Option<(&'static str, Duration)> = state
            .retry_after_until
            .filter(|until| *until > now)
            .map(|until| ("retry_after", (until - now).min(MAX_WAIT)));
        for (label, dim, needed) in [
            ("requests", &state.requests, 1),
            ("input_tokens", &state.input_tokens, estimate),
            ("output_tokens", &state.output_tokens, 0),
            ("tokens", &state.tokens, estimate),
        ] {
            if let Some(wait) = dimension_wait(dim, needed, safety_margin_pct, now) {
                if longest.is_none_or(|(_, current)| wait > current) {
                    longest = Some((label, wait));
                }
            }
        }
        if let Some((dimension, wait)) = longest {
            return Ok(Admission::Wait { dimension, wait });
        }
    }

    commit_admission(state, estimate);
    Ok(Admission::Admitted)
}

/// Positive Fensterlänge zwischen Beobachtung und Reset, sonst `None`.
fn observed_window(reset_at: Instant, now: Instant) -> Option<Duration> {
    let window = reset_at.saturating_duration_since(now);
    (!window.is_zero()).then_some(window)
}

/// Liest einen Header-Wert als `&str`, ignoriert nicht-ASCII/ungültige
/// Werte (`to_str()` schlägt fehl → `None`).
fn header_str<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Aktualisiert eine Dimension aus der Anthropic-Header-Familie
/// (`{prefix}-limit`, `{prefix}-remaining`, `{prefix}-reset`).
fn apply_anthropic_family(
    dim: &mut DimensionState,
    headers: &HeaderMap,
    prefix: &str,
    now: Instant,
) {
    let limit_header = format!("{prefix}-limit");
    if let Some(value) = header_str(headers, &limit_header) {
        match value.trim().parse::<u64>() {
            Ok(n) => dim.limit = Some(n),
            Err(_) => {
                tracing::debug!(header = %limit_header, value, "provider.rate_limit.header_parse_failed")
            }
        }
    }

    let remaining_header = format!("{prefix}-remaining");
    if let Some(value) = header_str(headers, &remaining_header) {
        match value.trim().parse::<u64>() {
            Ok(n) => dim.remaining = Some(n),
            Err(_) => {
                tracing::debug!(header = %remaining_header, value, "provider.rate_limit.header_parse_failed")
            }
        }
    }

    let reset_header = format!("{prefix}-reset");
    if let Some(value) = header_str(headers, &reset_header) {
        match parse_rfc3339_epoch_seconds(value)
            .and_then(|epoch| instant_from_epoch_seconds(epoch, now))
        {
            Some(instant) => {
                dim.reset_at = Some(instant);
                if let Some(window) = observed_window(instant, now) {
                    dim.window = Some(window);
                }
            }
            None => {
                tracing::debug!(header = %reset_header, value, "provider.rate_limit.header_parse_failed")
            }
        }
    }
}

/// Aktualisiert eine Dimension aus der OpenAI/DashScope-Header-Familie
/// (`x-ratelimit-limit-{kind}`, `x-ratelimit-remaining-{kind}`,
/// `x-ratelimit-reset-{kind}`).
fn apply_openai_family(dim: &mut DimensionState, headers: &HeaderMap, kind: &str, now: Instant) {
    let limit_header = format!("x-ratelimit-limit-{kind}");
    if let Some(value) = header_str(headers, &limit_header) {
        match value.trim().parse::<u64>() {
            Ok(n) => dim.limit = Some(n),
            Err(_) => {
                tracing::debug!(header = %limit_header, value, "provider.rate_limit.header_parse_failed")
            }
        }
    }

    let remaining_header = format!("x-ratelimit-remaining-{kind}");
    if let Some(value) = header_str(headers, &remaining_header) {
        match value.trim().parse::<u64>() {
            Ok(n) => dim.remaining = Some(n),
            Err(_) => {
                tracing::debug!(header = %remaining_header, value, "provider.rate_limit.header_parse_failed")
            }
        }
    }

    let reset_header = format!("x-ratelimit-reset-{kind}");
    if let Some(value) = header_str(headers, &reset_header) {
        match parse_go_like_duration(value) {
            Some(duration) => {
                dim.reset_at = Some(now + duration);
                if !duration.is_zero() {
                    dim.window = Some(duration);
                }
            }
            None => {
                tracing::debug!(header = %reset_header, value, "provider.rate_limit.header_parse_failed")
            }
        }
    }
}

/// Parst eine Go-artige Dauer wie `"1s"`, `"6m0s"`, `"250ms"` oder
/// `"1m30.5s"` in eine [`Duration`].
///
/// Unterstützte Einheiten: `ms`, `s`, `m`, `h`. Mehrere Segmente werden
/// aufsummiert. Gibt `None` bei leerem oder syntaktisch ungültigem Input
/// zurück.
fn parse_go_like_duration(input: &str) -> Option<Duration> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let mut idx = 0usize;
    let mut total = Duration::ZERO;
    let mut parsed_any = false;

    while idx < chars.len() {
        let num_start = idx;
        while idx < chars.len() && (chars[idx].is_ascii_digit() || chars[idx] == '.') {
            idx += 1;
        }
        if idx == num_start {
            return None;
        }
        let num_str: String = chars[num_start..idx].iter().collect();
        let value: f64 = num_str.parse().ok()?;

        if idx < chars.len() && chars[idx] == 'm' && chars.get(idx + 1) == Some(&'s') {
            idx += 2;
            total += Duration::from_secs_f64((value / 1000.0).max(0.0));
        } else if idx < chars.len() && chars[idx] == 's' {
            idx += 1;
            total += Duration::from_secs_f64(value.max(0.0));
        } else if idx < chars.len() && chars[idx] == 'm' {
            idx += 1;
            total += Duration::from_secs_f64((value * 60.0).max(0.0));
        } else if idx < chars.len() && chars[idx] == 'h' {
            idx += 1;
            total += Duration::from_secs_f64((value * 3600.0).max(0.0));
        } else {
            return None;
        }
        parsed_any = true;
    }

    parsed_any.then_some(total)
}

/// Parst einen RFC3339-Zeitstempel (`YYYY-MM-DDTHH:MM:SS(.fff)?(Z|±HH:MM)`)
/// in Sekunden seit der Unix-Epoche.
///
/// Minimalimplementierung ohne Fremd-Crate (keine `chrono`/`jiff`-Abhängigkeit
/// in dieser Crate verfügbar); nutzt Howard Hinnants `days_from_civil` zur
/// Umrechnung Kalenderdatum → Tage seit Epoche.
pub(crate) fn parse_rfc3339_epoch_seconds(input: &str) -> Option<f64> {
    let s = input.trim();
    if s.len() < 20 {
        return None;
    }
    let bytes = s.as_bytes();
    let year: i64 = s.get(0..4)?.parse().ok()?;
    if bytes.get(4) != Some(&b'-') {
        return None;
    }
    let month: u32 = s.get(5..7)?.parse().ok()?;
    if bytes.get(7) != Some(&b'-') {
        return None;
    }
    let day: u32 = s.get(8..10)?.parse().ok()?;
    match bytes.get(10) {
        Some(b'T') | Some(b't') => {}
        _ => return None,
    }
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    if bytes.get(13) != Some(&b':') {
        return None;
    }
    let minute: u32 = s.get(14..16)?.parse().ok()?;
    if bytes.get(16) != Some(&b':') {
        return None;
    }

    let rest = &s[17..];
    let rest_bytes = rest.as_bytes();
    let mut sec_end = 0usize;
    while sec_end < rest_bytes.len()
        && (rest_bytes[sec_end].is_ascii_digit() || rest_bytes[sec_end] == b'.')
    {
        sec_end += 1;
    }
    if sec_end == 0 {
        return None;
    }
    let seconds_frac: f64 = rest[..sec_end].parse().ok()?;
    let offset_part = &rest[sec_end..];

    let offset_seconds: i64 = if offset_part.starts_with('Z') || offset_part.starts_with('z') {
        0
    } else {
        let mut chars = offset_part.chars();
        let sign = chars.next()?;
        let sign_mult: i64 = match sign {
            '+' => 1,
            '-' => -1,
            _ => return None,
        };
        let off = &offset_part[1..];
        if off.len() < 5 {
            return None;
        }
        let oh: i64 = off.get(0..2)?.parse().ok()?;
        let om: i64 = off.get(3..5)?.parse().ok()?;
        sign_mult * (oh * 3600 + om * 60)
    };

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let epoch =
        (days as f64) * 86400.0 + (hour as f64) * 3600.0 + (minute as f64) * 60.0 + seconds_frac
            - offset_seconds as f64;
    Some(epoch)
}

/// Howard Hinnants `days_from_civil`: rechnet ein (proleptisch-gregorianisches)
/// Kalenderdatum in Tage seit der Unix-Epoche (1970-01-01) um.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Übersetzt einen absoluten Unix-Epochensekunden-Zeitpunkt relativ zu
/// `now` (Wall-Clock, via [`SystemTime`]) in ein [`Instant`], das an `now`
/// (Monotonic-Clock-Referenzpunkt) verankert ist.
///
/// Liegt der Zeitpunkt in der Vergangenheit, wird `now` selbst
/// zurückgegeben (keine Wartezeit).
fn instant_from_epoch_seconds(epoch: f64, now: Instant) -> Option<Instant> {
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    let delta = epoch - now_epoch;
    if delta > 0.0 {
        Some(now + Duration::from_secs_f64(delta))
    } else {
        Some(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use reqwest::header::{HeaderMap, HeaderValue};

    fn header_map(pairs: &[(&str, &str)]) -> TestResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(ctx("valid header name"))?,
                HeaderValue::from_str(value).map_err(ctx("valid header value"))?,
            );
        }
        Ok(headers)
    }

    #[test]
    fn test_parse_go_like_duration_cases() {
        assert_eq!(parse_go_like_duration("1s"), Some(Duration::from_secs(1)));
        assert_eq!(
            parse_go_like_duration("250ms"),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            parse_go_like_duration("6m0s"),
            Some(Duration::from_secs(360))
        );
        assert_eq!(
            parse_go_like_duration("1m30.5s"),
            Some(Duration::from_secs_f64(90.5))
        );
        assert_eq!(parse_go_like_duration(""), None);
        assert_eq!(parse_go_like_duration("abc"), None);
    }

    #[test]
    fn test_disabled_limiter_never_waits() -> TestResult {
        let limiter = ProviderRateLimiter::new(None);
        assert!(!limiter.is_enabled());
        let headers = header_map(&[
            ("anthropic-ratelimit-requests-limit", "50"),
            ("anthropic-ratelimit-requests-remaining", "0"),
        ])?;
        limiter.observe_headers(&headers);
        assert_eq!(limiter.pending_wait(), None);
        Ok(())
    }

    /// Export 429: nach einem 429 teilen sich alle Requests des Providers
    /// eine Abkühlphase — auch bei deaktiviertem Header-Pacer — und der
    /// Load-Status zeigt sie als Wartezeit.
    #[test]
    fn test_rate_limit_cooldown_is_shared_and_visible_even_when_disabled() {
        let limiter = ProviderRateLimiter::new(None);
        assert!(!limiter.is_enabled());
        assert_eq!(limiter.status_wait(), None);
        limiter.note_rate_limit_cooldown(Duration::from_secs(30));
        let wait = limiter.status_wait().unwrap_or_default();
        assert!(wait > Duration::from_secs(25) && wait <= Duration::from_secs(30));
        // Eine kürzere Meldung verkürzt die laufende Abkühlphase nicht.
        limiter.note_rate_limit_cooldown(Duration::from_secs(1));
        assert!(limiter.cooldown_remaining().unwrap_or_default() > Duration::from_secs(25));
        // Gedeckelt auf MAX_COOLDOWN.
        limiter.note_rate_limit_cooldown(Duration::from_secs(24 * 3600));
        assert!(limiter.cooldown_remaining().unwrap_or_default() <= MAX_COOLDOWN);
        // Pacing-Zustand bleibt deaktiviert.
        assert_eq!(limiter.pending_wait(), None);
    }

    #[test]
    fn test_observe_headers_anthropic_family_triggers_pending_wait() -> TestResult {
        let limiter = ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct: 20,
            ..harw_config::RateLimitToml::default()
        }));
        let headers = header_map(&[
            ("anthropic-ratelimit-requests-limit", "100"),
            ("anthropic-ratelimit-requests-remaining", "5"),
            ("anthropic-ratelimit-requests-reset", "2999-01-01T00:00:00Z"),
        ])?;
        limiter.observe_headers(&headers);
        let wait = limiter.pending_wait();
        assert!(
            wait.is_some(),
            "remaining 5%% <= safety margin 20%% muss warten ausloesen"
        );
        Ok(())
    }

    #[test]
    fn test_observe_headers_openai_family_above_margin_no_wait() -> TestResult {
        let limiter = ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct: 10,
            ..harw_config::RateLimitToml::default()
        }));
        let headers = header_map(&[
            ("x-ratelimit-limit-requests", "100"),
            ("x-ratelimit-remaining-requests", "80"),
            ("x-ratelimit-reset-requests", "6m0s"),
        ])?;
        limiter.observe_headers(&headers);
        assert_eq!(limiter.pending_wait(), None);
        Ok(())
    }

    #[test]
    fn test_observe_headers_malformed_values_ignored() -> TestResult {
        let limiter = ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct: 50,
            ..harw_config::RateLimitToml::default()
        }));
        let headers = header_map(&[
            ("anthropic-ratelimit-tokens-limit", "not-a-number"),
            ("anthropic-ratelimit-tokens-remaining", "also-bad"),
            ("anthropic-ratelimit-tokens-reset", "not-a-timestamp"),
        ])?;
        limiter.observe_headers(&headers);
        assert_eq!(limiter.pending_wait(), None);
        Ok(())
    }

    #[test]
    fn test_retry_after_header_triggers_pending_wait() -> TestResult {
        let limiter = ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct: 10,
            ..harw_config::RateLimitToml::default()
        }));
        let headers = header_map(&[("retry-after", "5")])?;
        limiter.observe_headers(&headers);
        let wait = limiter.pending_wait();
        assert!(wait.is_some());
        assert!(wait.ok_or(TestError::Missing("checked above"))? <= Duration::from_secs(5));
        Ok(())
    }

    #[test]
    fn test_pending_wait_capped_at_max_wait() -> TestResult {
        let limiter = ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct: 100,
            ..harw_config::RateLimitToml::default()
        }));
        let headers = header_map(&[
            ("anthropic-ratelimit-requests-limit", "10"),
            ("anthropic-ratelimit-requests-remaining", "1"),
            ("anthropic-ratelimit-requests-reset", "2999-01-01T00:00:00Z"),
        ])?;
        limiter.observe_headers(&headers);
        let wait = limiter
            .pending_wait()
            .ok_or(TestError::Missing("wait erwartet"))?;
        assert!(wait <= MAX_WAIT);
        Ok(())
    }

    #[test]
    fn test_parse_rfc3339_epoch_seconds_basic() -> TestResult {
        // 1970-01-01T00:00:01Z == 1 Sekunde seit Epoche.
        let epoch = parse_rfc3339_epoch_seconds("1970-01-01T00:00:01Z")
            .ok_or(TestError::Missing("parsebar"))?;
        assert!((epoch - 1.0).abs() < 1e-6);
        Ok(())
    }

    #[test]
    fn test_parse_rfc3339_epoch_seconds_rejects_garbage() {
        assert_eq!(parse_rfc3339_epoch_seconds("not-a-timestamp"), None);
    }

    #[tokio::test]
    async fn test_wait_for_slot_disabled_returns_immediately() {
        let limiter = ProviderRateLimiter::new(None);
        // Darf nicht blockieren; da deaktiviert, gibt es sofort zurueck.
        limiter.wait_for_slot().await;
    }

    #[test]
    fn test_rate_limited_count_starts_at_zero() {
        let limiter = ProviderRateLimiter::new(None);
        assert_eq!(limiter.rate_limited_count(), 0);
    }

    #[test]
    fn test_record_rate_limited_increments_count() {
        let limiter = ProviderRateLimiter::new(None);
        limiter.record_rate_limited();
        limiter.record_rate_limited();
        limiter.record_rate_limited();
        assert_eq!(limiter.rate_limited_count(), 3);
    }

    fn enabled_limiter(safety_margin_pct: u8) -> ProviderRateLimiter {
        ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct,
            ..harw_config::RateLimitToml::default()
        }))
    }

    /// Setzt eine Token-Dimension direkt (ohne Header-Umweg).
    fn set_input_tokens(
        limiter: &ProviderRateLimiter,
        limit: u64,
        remaining: u64,
        reset_at: Option<Instant>,
        window: Option<Duration>,
    ) {
        let mut state = limiter.lock_state();
        state.input_tokens = DimensionState {
            limit: Some(limit),
            remaining: Some(remaining),
            reset_at,
            window,
        };
    }

    fn input_remaining(limiter: &ProviderRateLimiter) -> Option<u64> {
        limiter.lock_state().input_tokens.remaining
    }

    #[test]
    fn test_estimate_exceeding_limit_fails_fast() -> TestResult {
        let limiter = enabled_limiter(10);
        let headers = header_map(&[
            ("anthropic-ratelimit-input-tokens-limit", "30000"),
            ("anthropic-ratelimit-input-tokens-remaining", "30000"),
            (
                "anthropic-ratelimit-input-tokens-reset",
                "2999-01-01T00:00:00Z",
            ),
        ])?;
        limiter.observe_headers(&headers);
        let expected = RateBudgetError::RequestExceedsLimit {
            dimension: "input_tokens",
            estimated_tokens: 40_000,
            limit: 30_000,
        };
        assert_eq!(limiter.check_request_budget(40_000), Err(expected.clone()));
        assert_eq!(limiter.pending_wait_for_estimate(40_000), Err(expected));
        assert_eq!(limiter.check_request_budget(30_000), Ok(()));
        Ok(())
    }

    #[test]
    fn test_openai_tokens_limit_fails_fast() -> TestResult {
        let limiter = enabled_limiter(10);
        let headers = header_map(&[
            ("x-ratelimit-limit-tokens", "1000"),
            ("x-ratelimit-remaining-tokens", "1000"),
            ("x-ratelimit-reset-tokens", "1m0s"),
        ])?;
        limiter.observe_headers(&headers);
        match limiter.check_request_budget(1001) {
            Err(RateBudgetError::RequestExceedsLimit { dimension, .. }) => {
                assert_eq!(dimension, "tokens");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_estimate_larger_than_remaining_waits() -> TestResult {
        let limiter = enabled_limiter(10);
        let reset = Instant::now() + Duration::from_secs(30);
        set_input_tokens(&limiter, 10_000, 5_000, Some(reset), None);
        let wait = limiter
            .pending_wait_for_estimate(6_000)
            .map_err(ctx("Vorschau"))?
            .ok_or(TestError::Missing("Wartezeit erwartet"))?;
        assert!(wait <= Duration::from_secs(30));
        // Eine kleinere Schätzung passt sofort.
        assert_eq!(
            limiter
                .pending_wait_for_estimate(1_000)
                .map_err(ctx("Vorschau"))?,
            None
        );
        // Die Vorschau verbucht nichts.
        assert_eq!(input_remaining(&limiter), Some(5_000));
        Ok(())
    }

    #[test]
    fn test_admission_decrements_optimistically() -> TestResult {
        let limiter = enabled_limiter(10);
        let reset = Instant::now() + Duration::from_secs(30);
        set_input_tokens(&limiter, 10_000, 10_000, Some(reset), None);
        {
            let mut state = limiter.lock_state();
            state.requests = DimensionState {
                limit: Some(50),
                remaining: Some(50),
                reset_at: Some(reset),
                window: None,
            };
        }
        let now = Instant::now();
        assert_eq!(
            limiter
                .try_admit(4_000, now, false)
                .map_err(ctx("Zulassung"))?,
            Admission::Admitted
        );
        assert_eq!(input_remaining(&limiter), Some(6_000));
        assert_eq!(limiter.lock_state().requests.remaining, Some(49));
        assert_eq!(
            limiter
                .try_admit(4_000, now, false)
                .map_err(ctx("Zulassung"))?,
            Admission::Admitted
        );
        assert_eq!(input_remaining(&limiter), Some(2_000));
        // Dritter Request passt nicht mehr → warten, nichts verbuchen.
        match limiter
            .try_admit(4_000, now, false)
            .map_err(ctx("Zulassung"))?
        {
            Admission::Wait { dimension, .. } => assert_eq!(dimension, "input_tokens"),
            Admission::Admitted => return Err(TestError::Unexpected("zugelassen".into())),
        }
        assert_eq!(input_remaining(&limiter), Some(2_000));
        Ok(())
    }

    #[test]
    fn test_no_stampede_after_reset() -> TestResult {
        let limiter = enabled_limiter(10);
        let window = Duration::from_secs(60);
        let past = Instant::now();
        set_input_tokens(&limiter, 100, 0, Some(past), Some(window));
        let now = past + Duration::from_millis(1);
        // Nach dem Reset wird aufgefüllt: der erste Wartende passt hinein …
        assert_eq!(
            limiter
                .try_admit(60, now, false)
                .map_err(ctx("Zulassung"))?,
            Admission::Admitted
        );
        assert_eq!(input_remaining(&limiter), Some(40));
        // … der zweite nicht mehr; er wartet auf das nächste geschätzte
        // Fenster statt gleichzeitig loszuschicken.
        match limiter
            .try_admit(60, now, false)
            .map_err(ctx("Zulassung"))?
        {
            Admission::Wait { dimension, wait } => {
                assert_eq!(dimension, "input_tokens");
                assert_eq!(wait, window);
            }
            Admission::Admitted => return Err(TestError::Unexpected("Ansturm".into())),
        }
        Ok(())
    }

    #[test]
    fn test_refill_without_window_uses_default() -> TestResult {
        let limiter = enabled_limiter(10);
        let past = Instant::now();
        set_input_tokens(&limiter, 100, 0, Some(past), None);
        let now = past + Duration::from_millis(1);
        assert_eq!(
            limiter
                .try_admit(100, now, false)
                .map_err(ctx("Zulassung"))?,
            Admission::Admitted
        );
        let reset = limiter
            .lock_state()
            .input_tokens
            .reset_at
            .ok_or(TestError::Missing("neues Fenster"))?;
        assert_eq!(reset, now + DEFAULT_WINDOW);
        Ok(())
    }

    #[test]
    fn test_force_admits_despite_shortage() -> TestResult {
        let limiter = enabled_limiter(10);
        let reset = Instant::now() + Duration::from_secs(30);
        set_input_tokens(&limiter, 10_000, 100, Some(reset), None);
        assert_eq!(
            limiter
                .try_admit(5_000, Instant::now(), true)
                .map_err(ctx("Zulassung"))?,
            Admission::Admitted
        );
        assert_eq!(input_remaining(&limiter), Some(0));
        Ok(())
    }

    #[test]
    fn test_record_request_estimate_decrements() -> TestResult {
        let limiter = enabled_limiter(10);
        let headers = header_map(&[
            ("x-ratelimit-limit-requests", "100"),
            ("x-ratelimit-remaining-requests", "80"),
            ("x-ratelimit-reset-requests", "6m0s"),
            ("x-ratelimit-limit-tokens", "10000"),
            ("x-ratelimit-remaining-tokens", "9000"),
            ("x-ratelimit-reset-tokens", "1m0s"),
        ])?;
        limiter.observe_headers(&headers);
        limiter.record_request_estimate(1_500);
        let state = limiter.lock_state();
        assert_eq!(state.requests.remaining, Some(79));
        assert_eq!(state.tokens.remaining, Some(7_500));
        assert_eq!(state.tokens.window, Some(Duration::from_secs(60)));
        assert_eq!(state.requests.window, Some(Duration::from_secs(360)));
        Ok(())
    }

    #[test]
    fn test_record_request_estimate_disabled_is_noop() {
        let limiter = ProviderRateLimiter::new(None);
        limiter.record_request_estimate(1_000);
        assert_eq!(input_remaining(&limiter), None);
    }

    #[test]
    fn test_expired_retry_after_is_cleared_on_admission() -> TestResult {
        let limiter = enabled_limiter(10);
        let past = Instant::now();
        limiter.lock_state().retry_after_until = Some(past);
        let now = past + Duration::from_millis(1);
        assert_eq!(
            limiter
                .try_admit(10, now, false)
                .map_err(ctx("Zulassung"))?,
            Admission::Admitted
        );
        assert_eq!(limiter.lock_state().retry_after_until, None);
        Ok(())
    }

    #[test]
    fn test_safety_margin_still_applies_with_estimate() -> TestResult {
        let limiter = enabled_limiter(20);
        let reset = Instant::now() + Duration::from_secs(10);
        // 15 % übrig, Sicherheitsabstand 20 % → warten, obwohl 1 Token passt.
        set_input_tokens(&limiter, 1_000, 150, Some(reset), None);
        assert!(
            limiter
                .pending_wait_for_estimate(1)
                .map_err(ctx("Vorschau"))?
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn test_rate_budget_error_display_is_german() {
        let err = RateBudgetError::RequestExceedsLimit {
            dimension: "input_tokens",
            estimated_tokens: 5,
            limit: 3,
        };
        let text = err.to_string();
        assert!(text.contains("übersteigt"));
        assert!(text.contains("input_tokens"));
    }

    #[tokio::test]
    async fn test_wait_for_slot_with_estimate_disabled_ok() -> TestResult {
        let limiter = ProviderRateLimiter::new(None);
        limiter
            .wait_for_slot_with_estimate(u64::MAX)
            .await
            .map_err(ctx("deaktiviert muss zulassen"))?;
        Ok(())
    }

    #[tokio::test]
    async fn test_wait_for_slot_with_estimate_fails_fast() {
        let limiter = enabled_limiter(10);
        let reset = Instant::now() + Duration::from_secs(60);
        set_input_tokens(&limiter, 1_000, 0, Some(reset), None);
        // Würde ohne Fail-fast 60 s warten; muss sofort scheitern.
        let result = limiter.wait_for_slot_with_estimate(2_000).await;
        assert!(matches!(
            result,
            Err(RateBudgetError::RequestExceedsLimit { limit: 1_000, .. })
        ));
    }

    #[tokio::test]
    async fn test_wait_for_slot_with_estimate_admits_and_decrements() -> TestResult {
        let limiter = enabled_limiter(10);
        let reset = Instant::now() + Duration::from_secs(60);
        set_input_tokens(&limiter, 1_000, 900, Some(reset), None);
        limiter
            .wait_for_slot_with_estimate(300)
            .await
            .map_err(ctx("muss sofort zulassen"))?;
        assert_eq!(input_remaining(&limiter), Some(600));
        Ok(())
    }

    #[test]
    fn test_record_rate_limited_counts_even_when_pacer_disabled() {
        // Der 429-Zähler ist ein reiner Beobachtungszähler, unabhängig vom
        // Pacing-Zustand (`enabled == false` heißt nur: kein proaktives
        // Warten, nicht "keine Beobachtung").
        let limiter = ProviderRateLimiter::new(None);
        assert!(!limiter.is_enabled());
        limiter.record_rate_limited();
        assert_eq!(limiter.rate_limited_count(), 1);
    }
}
