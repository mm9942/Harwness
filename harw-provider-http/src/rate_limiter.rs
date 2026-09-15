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

use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;

/// Obergrenze für eine einzelne Wartezeit pro `wait_for_slot`-Aufruf.
const MAX_WAIT: Duration = Duration::from_secs(120);

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
}

/// Gesamtzustand des Pacers über alle bekannten Dimensionen.
#[derive(Debug, Default)]
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
}

impl fmt::Debug for ProviderRateLimiter {
    /// Gibt Konfiguration, aber keinen Laufzeitzustand aus (kein Lock im
    /// `Debug`-Pfad nötig, da nur `enabled`/`safety_margin_pct` gedruckt
    /// werden).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderRateLimiter")
            .field("enabled", &self.enabled)
            .field("safety_margin_pct", &self.safety_margin_pct)
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
        }
    }

    /// Meldet, ob der Pacer aktiv ist.
    ///
    /// # Returns
    /// `true`, wenn `observe_headers`/`wait_for_slot` tatsächlich wirken.
    pub fn is_enabled(&self) -> bool {
        self.enabled
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

        apply_anthropic_family(&mut state.requests, headers, "anthropic-ratelimit-requests", now);
        apply_anthropic_family(&mut state.tokens, headers, "anthropic-ratelimit-tokens", now);
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
                    tracing::debug!(header = "retry-after", value, "provider.rate_limit.header_parse_failed");
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
            if let (Some(limit), Some(remaining), Some(reset_at)) = (dim.limit, dim.remaining, dim.reset_at) {
                if reset_at > now
                    && remaining.saturating_mul(100) <= limit.saturating_mul(u64::from(self.safety_margin_pct))
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

/// Liest einen Header-Wert als `&str`, ignoriert nicht-ASCII/ungültige
/// Werte (`to_str()` schlägt fehl → `None`).
fn header_str<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Aktualisiert eine Dimension aus der Anthropic-Header-Familie
/// (`{prefix}-limit`, `{prefix}-remaining`, `{prefix}-reset`).
fn apply_anthropic_family(dim: &mut DimensionState, headers: &HeaderMap, prefix: &str, now: Instant) {
    let limit_header = format!("{prefix}-limit");
    if let Some(value) = header_str(headers, &limit_header) {
        match value.trim().parse::<u64>() {
            Ok(n) => dim.limit = Some(n),
            Err(_) => tracing::debug!(header = %limit_header, value, "provider.rate_limit.header_parse_failed"),
        }
    }

    let remaining_header = format!("{prefix}-remaining");
    if let Some(value) = header_str(headers, &remaining_header) {
        match value.trim().parse::<u64>() {
            Ok(n) => dim.remaining = Some(n),
            Err(_) => tracing::debug!(header = %remaining_header, value, "provider.rate_limit.header_parse_failed"),
        }
    }

    let reset_header = format!("{prefix}-reset");
    if let Some(value) = header_str(headers, &reset_header) {
        match parse_rfc3339_epoch_seconds(value).and_then(|epoch| instant_from_epoch_seconds(epoch, now)) {
            Some(instant) => dim.reset_at = Some(instant),
            None => tracing::debug!(header = %reset_header, value, "provider.rate_limit.header_parse_failed"),
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
            Err(_) => tracing::debug!(header = %limit_header, value, "provider.rate_limit.header_parse_failed"),
        }
    }

    let remaining_header = format!("x-ratelimit-remaining-{kind}");
    if let Some(value) = header_str(headers, &remaining_header) {
        match value.trim().parse::<u64>() {
            Ok(n) => dim.remaining = Some(n),
            Err(_) => tracing::debug!(header = %remaining_header, value, "provider.rate_limit.header_parse_failed"),
        }
    }

    let reset_header = format!("x-ratelimit-reset-{kind}");
    if let Some(value) = header_str(headers, &reset_header) {
        match parse_go_like_duration(value) {
            Some(duration) => dim.reset_at = Some(now + duration),
            None => tracing::debug!(header = %reset_header, value, "provider.rate_limit.header_parse_failed"),
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
fn parse_rfc3339_epoch_seconds(input: &str) -> Option<f64> {
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
    while sec_end < rest_bytes.len() && (rest_bytes[sec_end].is_ascii_digit() || rest_bytes[sec_end] == b'.') {
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
    let epoch = (days as f64) * 86400.0 + (hour as f64) * 3600.0 + (minute as f64) * 60.0 + seconds_frac
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
    let now_epoch = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs_f64();
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
    use reqwest::header::{HeaderMap, HeaderValue};

    fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
                HeaderValue::from_str(value).expect("valid header value"),
            );
        }
        headers
    }

    #[test]
    fn test_parse_go_like_duration_cases() {
        assert_eq!(parse_go_like_duration("1s"), Some(Duration::from_secs(1)));
        assert_eq!(parse_go_like_duration("250ms"), Some(Duration::from_millis(250)));
        assert_eq!(parse_go_like_duration("6m0s"), Some(Duration::from_secs(360)));
        assert_eq!(
            parse_go_like_duration("1m30.5s"),
            Some(Duration::from_secs_f64(90.5))
        );
        assert_eq!(parse_go_like_duration(""), None);
        assert_eq!(parse_go_like_duration("abc"), None);
    }

    #[test]
    fn test_disabled_limiter_never_waits() {
        let limiter = ProviderRateLimiter::new(None);
        assert!(!limiter.is_enabled());
        let headers = header_map(&[
            ("anthropic-ratelimit-requests-limit", "50"),
            ("anthropic-ratelimit-requests-remaining", "0"),
        ]);
        limiter.observe_headers(&headers);
        assert_eq!(limiter.pending_wait(), None);
    }

    #[test]
    fn test_observe_headers_anthropic_family_triggers_pending_wait() {
        let limiter = ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct: 20,
        }));
        let headers = header_map(&[
            ("anthropic-ratelimit-requests-limit", "100"),
            ("anthropic-ratelimit-requests-remaining", "5"),
            ("anthropic-ratelimit-requests-reset", "2999-01-01T00:00:00Z"),
        ]);
        limiter.observe_headers(&headers);
        let wait = limiter.pending_wait();
        assert!(wait.is_some(), "remaining 5%% <= safety margin 20%% muss warten ausloesen");
    }

    #[test]
    fn test_observe_headers_openai_family_above_margin_no_wait() {
        let limiter = ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct: 10,
        }));
        let headers = header_map(&[
            ("x-ratelimit-limit-requests", "100"),
            ("x-ratelimit-remaining-requests", "80"),
            ("x-ratelimit-reset-requests", "6m0s"),
        ]);
        limiter.observe_headers(&headers);
        assert_eq!(limiter.pending_wait(), None);
    }

    #[test]
    fn test_observe_headers_malformed_values_ignored() {
        let limiter = ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct: 50,
        }));
        let headers = header_map(&[
            ("anthropic-ratelimit-tokens-limit", "not-a-number"),
            ("anthropic-ratelimit-tokens-remaining", "also-bad"),
            ("anthropic-ratelimit-tokens-reset", "not-a-timestamp"),
        ]);
        limiter.observe_headers(&headers);
        assert_eq!(limiter.pending_wait(), None);
    }

    #[test]
    fn test_retry_after_header_triggers_pending_wait() {
        let limiter = ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct: 10,
        }));
        let headers = header_map(&[("retry-after", "5")]);
        limiter.observe_headers(&headers);
        let wait = limiter.pending_wait();
        assert!(wait.is_some());
        assert!(wait.expect("checked above") <= Duration::from_secs(5));
    }

    #[test]
    fn test_pending_wait_capped_at_max_wait() {
        let limiter = ProviderRateLimiter::new(Some(harw_config::RateLimitToml {
            enabled: true,
            safety_margin_pct: 100,
        }));
        let headers = header_map(&[
            ("anthropic-ratelimit-requests-limit", "10"),
            ("anthropic-ratelimit-requests-remaining", "1"),
            ("anthropic-ratelimit-requests-reset", "2999-01-01T00:00:00Z"),
        ]);
        limiter.observe_headers(&headers);
        let wait = limiter.pending_wait().expect("wait erwartet");
        assert!(wait <= MAX_WAIT);
    }

    #[test]
    fn test_parse_rfc3339_epoch_seconds_basic() {
        // 1970-01-01T00:00:01Z == 1 Sekunde seit Epoche.
        let epoch = parse_rfc3339_epoch_seconds("1970-01-01T00:00:01Z").expect("parsebar");
        assert!((epoch - 1.0).abs() < 1e-6);
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
}
