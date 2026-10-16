use crate::serde_defaults::default_true;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use harw_types::ReasoningEffort;

use crate::auth_toml::SecretRef;
use crate::error::{ConfigError, ConfigResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderToml {
    pub name: String,
    pub api: String,
    pub base_url: String,
    /// Bevorzugtes Feld: eine `SecretRef` (`env:`/`file:`/`keyring:`/`secrets:`),
    /// niemals ein literaler Schlüssel.
    #[serde(default)]
    pub auth: Option<SecretRef>,
    /// Credential transport: bearer, api-key, x-api-key or none.
    /// Omitted preserves the transport's compatible default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    /// DEPRECATED: literaler API-Key im Klartext. Wird weiterhin geparst
    /// (Rückwärtskompatibilität), aber `harw doctor` markiert jedes
    /// nicht-leere Vorkommen als `ConfigError::PlaintextSecret`.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub origin_allowlist: OriginAllowlistToml,
    /// Client-seitiges Rate-Limiting für diesen Provider: reaktives
    /// Header-Pacing und/oder proaktive RPM/TPM-Budgets (siehe
    /// [`RateLimitToml`]); `None` = beides aus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitToml>,
    /// Harte Obergrenze gleichzeitig in Flug befindlicher Requests an
    /// diesen Provider; `None` = unbegrenzt. Anders als `rate_limit`
    /// (reaktives Header-Pacing) ist dies ein rein client-seitiger
    /// Zähler, der zusätzliche Requests blockiert statt sie fehlschlagen
    /// zu lassen. `Some(0)` ist ungültig (siehe [`Self::validate`]) und
    /// würde jeden Request auf ewig blockieren.
    ///
    /// Runde 7, Teil L5: Für lokale Modell-Server (ein Modell, eine GPU) ist
    /// `max_concurrency = 1` die empfohlene Einstellung; `harw provider add`
    /// und der Katalog-Seed setzen sie für lokale Provider. Weitere Requests
    /// (z. B. parallele Kinder) warten dann in der Warteschlange; die
    /// Wartezeit zählt nicht zum Request-Zeitlimit. Verhältnis zu
    /// `rate_limit.max_concurrent`: beide Grenzen gelten gleichzeitig, die
    /// kleinere gewinnt. `max_concurrency` ist die Provider-weite, zur
    /// Laufzeit verstellbare Grenze und der empfohlene Ort;
    /// `rate_limit.max_concurrent` ist nur für Budget-Buckets je Modell
    /// gedacht.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
    /// Optionaler Wert für den `originator`-HTTP-Header, den `harw` auf der
    /// Codex-/ChatGPT-Route (`harw-provider-http::codex`) an OpenAI sendet.
    /// `None` behält den harw-eigenen Default `"harw"` bei.
    ///
    /// **Wichtig:** Dieser Wert dient bei OpenAI ausschließlich der
    /// Client-Identifikation und wird bei jedem Request im Klartext
    /// mitgeschickt. Ihn auf den Wert des offiziellen Codex-CLI-Clients zu
    /// setzen, um wie dieser Client zu erscheinen, ist eine bewusste
    /// Entscheidung der Nutzerin/des Nutzers — sie kann im Widerspruch zu
    /// OpenAIs Nutzungsbedingungen stehen. `harw` erzwingt hier keine
    /// bestimmte Wahl, validiert den Wert aber (siehe [`Self::validate`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    /// Token-Streaming (SSE) für alle Modelle dieses Providers. `None` =
    /// Default: an für native Anthropic-/OpenAI-APIs. Ein Modell kann das
    /// per `ModelToml::stream` übersteuern. Ohne Streaming meldet der
    /// Turn-Loop Text und Usage pro Modell-Runde.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Standard-Reasoning-Effort für Sessions/Kinder, die über diesen
    /// Provider laufen, sofern nicht durch eine spezifischere Ebene
    /// überschrieben (Modell, Agenten-Definition). `None` = keine
    /// Provider-seitige Vorgabe.
    ///
    /// `ReasoningEffort` ist selbst serde-fähig (`FromStr`/`Display`/
    /// `Serialize`/`Deserialize`, `rename_all = "snake_case"`, siehe
    /// `harw-types/src/reasoning.rs`), daher wird hier direkt der Enum-Typ
    /// verwendet statt eines undurchsichtigen `String`: ein unbekanntes
    /// Label (z. B. `"medum"`) scheitert bereits beim Deserialisieren der
    /// TOML-Datei, nicht erst bei einer späteren Auflösung. Die **Rangfolge**
    /// gegenüber Modell-/Agenten-Ebene ist NICHT Teil dieser Änderung — das
    /// ist Aufgabe einer späteren Welle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<ReasoningEffort>,
    /// Opt-in: sendet pro Request `x-harw-session`, `x-harw-agent`, `x-harw-role`
    /// an eigene Cloudflare-Worker/AI-Gateway-Endpunkte. Standard: aus.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gateway_identity_headers: bool,
    /// Runde 7, Teil L4: Gesamt-Zeitlimit eines nicht gestreamten Requests
    /// bzw. Wartezeit bis zu den Antwort-Headern eines gestreamten Requests,
    /// in Sekunden. `None` = Vorgabe (120 s, lokale Provider 600 s, siehe
    /// [`Self::effective_request_timeout_secs`]). `0` ist ungültig.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<u64>,
    /// Runde 7, Teil L4: Leerlauf-Zeitlimit beim Streaming — so viele
    /// Sekunden ohne ein einziges empfangenes Byte brechen den Stream ab.
    /// Ein laufender Stream hat **kein** Gesamt-Zeitlimit mehr. `None` =
    /// Vorgabe (lokal 120 s, sonst gleich dem Request-Zeitlimit, siehe
    /// [`Self::effective_stream_idle_timeout_secs`]). `0` ist ungültig.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_secs: Option<u64>,
    /// Runde 7, Teil L4: ob ein Zeitlimit-Fehler automatisch wiederholt
    /// wird. `None` = Vorgabe (Cloud: ja, lokale Provider: nein — ein
    /// überlasteter lokaler Server wird durch Wiederholen nur noch
    /// langsamer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_timeouts: Option<bool>,
    /// Runde 7, Teil L6 (nur `openai-chat`): welches Feld die
    /// Ausgabe-Obergrenze trägt. `None` = Vorgabe (Cloud:
    /// `max_completion_tokens`, lokal: `max_tokens`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
    /// Runde 7, Teil L6 (nur `openai-chat`): ob `reasoning_effort` gesendet
    /// wird. `None` = Vorgabe (Cloud: ja, lokal: nein — viele lokale Server
    /// lehnen unbekannte Felder ab).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_reasoning_effort: Option<bool>,
    /// Runde 7, Teil L6 (nur `openai-chat`): ob Werkzeug-Schemas mit
    /// `"strict"` gesendet werden. `None` = Vorgabe (Cloud: ja, lokal: nein).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_tools: Option<bool>,
    /// Runde 7, Teil L6 (nur `openai-chat`): Wert für `parallel_tool_calls`,
    /// sobald Werkzeuge angeboten werden. `None` = Vorgabe (Cloud: Feld
    /// weglassen, lokal: `false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// Runde 7, Teil L8: erlaubt unverschlüsseltes `http` zu einer privaten
    /// LAN-Adresse (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`,
    /// IPv6-ULA `fc00::/7`). Standard `false`: `http` nur für Loopback.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_insecure_lan: bool,
}

/// Runde 7, Teil L6: Name des Felds für die Ausgabe-Obergrenze im
/// `chat/completions`-Body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    /// Nur `max_tokens` (ältere bzw. lokale OpenAI-kompatible Server).
    MaxTokens,
    /// Nur `max_completion_tokens` (aktuelle OpenAI-API).
    MaxCompletionTokens,
    /// Beide Felder mit demselben Wert.
    Both,
}

/// Vorgabe des Request-Zeitlimits für Cloud-Provider (Sekunden).
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 120;
/// Vorgabe des Request-Zeitlimits für lokale Provider (Sekunden).
pub const DEFAULT_LOCAL_REQUEST_TIMEOUT_SECS: u64 = 600;
/// Vorgabe des Streaming-Leerlauf-Zeitlimits für lokale Provider (Sekunden).
pub const DEFAULT_LOCAL_STREAM_IDLE_TIMEOUT_SECS: u64 = 120;

/// Runde 7, Teil L1/L8: Host-Anteil einer Basis-URL (ohne Schema,
/// Userinfo, Port und IPv6-Klammern), kleingeschrieben.
///
/// # Arguments
/// - `base_url`: z. B. `http://[::1]:8000/v1`.
///
/// # Returns
/// `Some(host)` oder `None`, wenn kein Host erkennbar ist.
fn url_host(base_url: &str) -> Option<String> {
    let rest = base_url.trim();
    let rest = rest.split_once("://").map_or(rest, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

/// Runde 7, Teil L1: `true`, wenn `host` die eigene Maschine bezeichnet
/// (`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`).
fn host_is_loopback(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_loopback(),
        Ok(std::net::IpAddr::V6(v6)) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Err(_) => false,
    }
}

/// Runde 7, Teil L8: `true`, wenn `host` eine private LAN-IP ist
/// (`10/8`, `172.16/12`, `192.168/16`, IPv6-ULA `fc00::/7`). Hostnamen
/// zählen bewusst nicht (DNS kann überallhin zeigen).
#[must_use]
pub fn host_is_private_lan(host: &str) -> bool {
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_private(),
        Ok(std::net::IpAddr::V6(v6)) => {
            (v6.segments()[0] & 0xfe00) == 0xfc00
                || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_private())
        }
        Err(_) => false,
    }
}

/// Höchstlänge des `originator`-Felds (siehe [`ProviderToml::validate`]).
const MAX_ORIGINATOR_CHARS: usize = 64;

/// `true`, wenn `value` nicht leer ist und ausschließlich druckbare ASCII-
/// Zeichen (0x20–0x7E) enthält — also ohne Steuerzeichen (Tab, Zeilenumbruch, …)
/// und ohne Nicht-ASCII-Zeichen.
fn is_printable_ascii(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii() && !c.is_ascii_control())
}

impl ProviderToml {
    /// `true`, wenn dieser Provider noch den veralteten `api_key`-Klartext
    /// verwendet und daher von `harw doctor` als Verstoß markiert werden muss.
    #[must_use]
    pub fn has_plaintext_secret(&self) -> bool {
        self.api_key.as_ref().is_some_and(|k| !k.is_empty())
    }

    /// Runde 7, Teil L1: `true`, wenn dieser Provider ein lokales
    /// Modell-Backend anspricht — Transport `ollama`, eine Loopback-Basis-URL
    /// oder eine private LAN-IP mit `allow_insecure_lan = true`.
    ///
    /// Lokale Provider bekommen andere Vorgaben (kein Auth-Header, längere
    /// Zeitlimits, keine Timeout-Wiederholung, schlanker Chat-Body,
    /// `max_concurrency = 1` beim Anlegen) und keinen Hersteller-Präfix-Match
    /// für das Kontextfenster.
    #[must_use]
    pub fn is_local(&self) -> bool {
        if self.api == "ollama" {
            return true;
        }
        url_host(&self.base_url).is_some_and(|host| {
            host_is_loopback(&host) || (self.allow_insecure_lan && host_is_private_lan(&host))
        })
    }

    /// Runde 7, Teil L4: wirksames Request-Zeitlimit in Sekunden
    /// (`request_timeout_secs`, sonst 600 lokal bzw. 120).
    #[must_use]
    pub fn effective_request_timeout_secs(&self) -> u64 {
        self.request_timeout_secs.unwrap_or(if self.is_local() {
            DEFAULT_LOCAL_REQUEST_TIMEOUT_SECS
        } else {
            DEFAULT_REQUEST_TIMEOUT_SECS
        })
    }

    /// Runde 7, Teil L4: wirksames Streaming-Leerlauf-Zeitlimit in Sekunden
    /// (`stream_idle_timeout_secs`, sonst 120 lokal bzw. das
    /// Request-Zeitlimit).
    #[must_use]
    pub fn effective_stream_idle_timeout_secs(&self) -> u64 {
        self.stream_idle_timeout_secs.unwrap_or(if self.is_local() {
            DEFAULT_LOCAL_STREAM_IDLE_TIMEOUT_SECS
        } else {
            self.effective_request_timeout_secs()
        })
    }

    /// Runde 7, Teil L4: ob Zeitlimit-Fehler wiederholt werden
    /// (`retry_timeouts`, sonst nur bei Cloud-Providern).
    #[must_use]
    pub fn effective_retry_timeouts(&self) -> bool {
        self.retry_timeouts.unwrap_or(!self.is_local())
    }

    /// Runde 7, Teil L6: wirksames Feld für die Ausgabe-Obergrenze.
    #[must_use]
    pub fn effective_max_tokens_field(&self) -> MaxTokensField {
        self.max_tokens_field.unwrap_or(if self.is_local() {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        })
    }

    /// Runde 7, Teil L6: ob `reasoning_effort` gesendet wird.
    #[must_use]
    pub fn effective_send_reasoning_effort(&self) -> bool {
        self.send_reasoning_effort.unwrap_or(!self.is_local())
    }

    /// Runde 7, Teil L6: ob Werkzeug-Schemas `"strict"` tragen.
    #[must_use]
    pub fn effective_strict_tools(&self) -> bool {
        self.strict_tools.unwrap_or(!self.is_local())
    }

    /// Runde 7, Teil L6: Wert für `parallel_tool_calls` (`None` = Feld
    /// weglassen; lokal `Some(false)`).
    #[must_use]
    pub fn effective_parallel_tool_calls(&self) -> Option<bool> {
        self.parallel_tool_calls
            .or_else(|| self.is_local().then_some(false))
    }

    /// `true`, wenn ein Header dieses Namens Credentials trägt und daher nur
    /// als [`SecretRef`] konfiguriert werden darf.
    ///
    /// Regel (ASCII-case-insensitiv): der Name enthält `authorization` (deckt
    /// `authorization`, `proxy-authorization`, `cf-aig-authorization` ab),
    /// endet auf `-key` (`x-api-key`, `api-key`) oder enthält `token`.
    /// `harw-provider-http` nutzt dieselbe Regel, um solche Header aufzulösen
    /// und als sensitiv zu markieren.
    #[must_use]
    pub fn is_sensitive_header_name(name: &str) -> bool {
        let name = name.trim().to_ascii_lowercase();
        name.contains("authorization") || name.ends_with("-key") || name.contains("token")
    }

    /// Prüft Invarianten, die die TOML-Deserialisierung nicht ausdrücken kann.
    ///
    /// Derzeit: Header mit Credential-Namen (siehe
    /// [`Self::is_sensitive_header_name`]) müssen eine gültige [`SecretRef`]
    /// (`env:`/`file:`/`file-json:`/`keyring:`/`secrets:`) sein, nie Klartext.
    ///
    /// # Errors
    /// - [`ConfigError::PlaintextSecret`]: Header mit Credential-Namen ist
    ///   Klartext statt einer [`SecretRef`]; der Wert erscheint nie im
    ///   Fehler. Bei mehreren Verstößen wird der lexikographisch kleinste
    ///   Header-Name gemeldet (deterministisch trotz `HashMap`).
    /// - [`ConfigError::Invalid`]: `max_concurrency` ist auf `Some(0)`
    ///   gesetzt, was jeden Request an diesen Provider für immer blockieren
    ///   würde (fast sicher ein Tippfehler statt beabsichtigtes Verhalten).
    /// - [`ConfigError::Invalid`]: `originator` ist gesetzt, aber leer, länger
    ///   als [`MAX_ORIGINATOR_CHARS`] Zeichen oder enthält Nicht-ASCII-/
    ///   Steuerzeichen (siehe [`is_printable_ascii`]).
    /// - [`ConfigError::Invalid`]: `request_timeout_secs` oder
    ///   `stream_idle_timeout_secs` ist `0` (Runde 7, Teil L4).
    /// - [`ConfigError::Invalid`]: `[rate_limit]` verletzt
    ///   [`RateLimitToml::validate`] (Budget-Feld `0`, Marge > 100,
    ///   `mode = "budget"` ohne Budget).
    pub fn validate(&self) -> ConfigResult<()> {
        let mut headers: Vec<(&String, &String)> = self.headers.iter().collect();
        headers.sort_by(|left, right| left.0.cmp(right.0));
        for (name, value) in headers {
            if Self::is_sensitive_header_name(name) && value.parse::<SecretRef>().is_err() {
                return Err(ConfigError::PlaintextSecret {
                    file: format!("providers/{}.toml", self.name),
                    field: format!("headers.{name}"),
                });
            }
        }
        if self.max_concurrency == Some(0) {
            return Err(ConfigError::Invalid(format!(
                "provider '{}': max_concurrency = 0 would block every request forever; omit the field for unlimited concurrency or set it to a positive value",
                self.name
            )));
        }
        if let Some(rate_limit) = &self.rate_limit {
            rate_limit.validate(&format!("provider '{}'", self.name))?;
        }
        for (field, value) in [
            ("request_timeout_secs", self.request_timeout_secs),
            ("stream_idle_timeout_secs", self.stream_idle_timeout_secs),
        ] {
            if value == Some(0) {
                return Err(ConfigError::Invalid(format!(
                    "provider '{}': {field} = 0 would fail every request immediately; omit the field for the default or set a positive number of seconds",
                    self.name
                )));
            }
        }
        if let Some(originator) = &self.originator {
            if originator.len() > MAX_ORIGINATOR_CHARS || !is_printable_ascii(originator) {
                return Err(ConfigError::Invalid(format!(
                    "provider '{}': originator must be non-empty, printable ASCII (no control characters) and at most {MAX_ORIGINATOR_CHARS} characters",
                    self.name
                )));
            }
        }
        Ok(())
    }
}

/// Welche Agenten/Channels über diesen Provider routen dürfen. Leer/fehlend
/// bedeutet "keine Einschränkung".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginAllowlistToml {
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
}

/// Default-Sicherheitsmarge für client-seitiges Rate-Limiting in Prozent.
fn default_safety_margin_pct() -> u8 {
    10
}

/// Welche Rate-Limit-Mechanismen für einen Provider bzw. ein Modell wirken.
///
/// - `header`: nur das reaktive Header-Pacing (`x-ratelimit-*`/
///   `anthropic-ratelimit-*`, siehe `harw-provider-http::rate_limiter`),
///   gesteuert über [`RateLimitToml::enabled`]; konfigurierte Budgets werden
///   ignoriert.
/// - `budget`: nur die client-seitigen Token-Buckets (RPM/TPM, siehe
///   `harw-provider-http::budget`); das Header-Pacing bleibt aus, auch wenn
///   `enabled = true` gesetzt ist.
/// - `both`: beides (Default, sobald mindestens ein Budget-Feld gesetzt ist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitMode {
    /// Nur reaktives Header-Pacing.
    Header,
    /// Nur client-seitige Budgets.
    Budget,
    /// Header-Pacing und Budgets gemeinsam.
    Both,
}

impl RateLimitMode {
    /// `true`, wenn dieser Modus das Header-Pacing einschließt.
    #[must_use]
    pub fn uses_header(self) -> bool {
        matches!(self, Self::Header | Self::Both)
    }

    /// `true`, wenn dieser Modus die client-seitigen Budgets einschließt.
    #[must_use]
    pub fn uses_budget(self) -> bool {
        matches!(self, Self::Budget | Self::Both)
    }
}

/// Client-seitiges Rate-Limiting-Konfiguration für einen Provider (Sektion
/// `[rate_limit]` in `providers/<name>.toml`) oder als Override für ein
/// einzelnes Modell (Sektion `[rate_limit]` in `models/<name>.toml`).
///
/// # Description
/// Zwei unabhängige Mechanismen, gewählt über [`Self::mode`]:
/// - **Header-Pacing** (reaktiv): `enabled` + `safety_margin_pct`; wartet,
///   wenn die vom Provider gemeldeten Rest-Kontingente knapp werden.
/// - **Budgets** (proaktiv): `requests_per_minute`, `tokens_per_minute`,
///   `input_tokens_per_minute`, `output_tokens_per_minute`, `max_concurrent`;
///   kontinuierlich auffüllende Token-Buckets, die Requests vor dem Senden
///   zurückhalten, bis Kapazität frei ist. Budgets wirken unabhängig von
///   `enabled` (das nur das Header-Pacing schaltet).
///
/// Als Modell-Override (`ModelToml::rate_limit`) bilden die Budget-Felder
/// einen eigenen Bucket je (Provider, Modell), der **zusätzlich** zum
/// Provider-Bucket gilt; `enabled`/`safety_margin_pct` sind dort ohne
/// Wirkung.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitToml {
    /// `true` aktiviert das reaktive Header-Pacing/Throttling.
    #[serde(default)]
    pub enabled: bool,
    /// Sicherheitsmarge (Prozent) unterhalb des vom Provider gemeldeten
    /// Limits, die eingehalten wird, bevor gewartet wird.
    #[serde(default = "default_safety_margin_pct")]
    pub safety_margin_pct: u8,
    /// Höchstzahl Requests pro Minute (RPM-Budget); `None` = unbegrenzt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<u32>,
    /// Höchstzahl Tokens (Eingabe + Ausgabe) pro Minute (TPM-Budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_minute: Option<u64>,
    /// Höchstzahl Eingabe-Tokens pro Minute (ITPM-Budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens_per_minute: Option<u64>,
    /// Höchstzahl Ausgabe-Tokens pro Minute (OTPM-Budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_per_minute: Option<u64>,
    /// Höchstzahl gleichzeitig in Flug befindlicher Requests dieses Buckets.
    /// Anders als `ProviderToml::max_concurrency` (Provider-weit, zur
    /// Laufzeit verstellbar) gilt dies je Budget-Bucket und damit auch je
    /// Modell-Override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
    /// Welche Mechanismen wirken; `None` = [`Self::effective_mode`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RateLimitMode>,
}

impl Default for RateLimitToml {
    fn default() -> Self {
        Self {
            enabled: false,
            safety_margin_pct: default_safety_margin_pct(),
            requests_per_minute: None,
            tokens_per_minute: None,
            input_tokens_per_minute: None,
            output_tokens_per_minute: None,
            max_concurrent: None,
            mode: None,
        }
    }
}

impl RateLimitToml {
    /// `true`, wenn mindestens ein Budget-Feld gesetzt ist.
    #[must_use]
    pub fn has_budget(&self) -> bool {
        self.requests_per_minute.is_some()
            || self.tokens_per_minute.is_some()
            || self.input_tokens_per_minute.is_some()
            || self.output_tokens_per_minute.is_some()
            || self.max_concurrent.is_some()
    }

    /// Wirksamer Modus: explizites `mode`, sonst `both`, sobald ein Budget
    /// gesetzt ist, sonst `header` (bisheriges Verhalten).
    #[must_use]
    pub fn effective_mode(&self) -> RateLimitMode {
        self.mode.unwrap_or(if self.has_budget() {
            RateLimitMode::Both
        } else {
            RateLimitMode::Header
        })
    }

    /// `true`, wenn das reaktive Header-Pacing laufen soll (`enabled` und
    /// ein Modus mit Header-Anteil).
    #[must_use]
    pub fn header_pacing_enabled(&self) -> bool {
        self.enabled && self.effective_mode().uses_header()
    }

    /// `true`, wenn client-seitige Budgets greifen sollen (mindestens ein
    /// Budget-Feld und ein Modus mit Budget-Anteil).
    #[must_use]
    pub fn budget_enabled(&self) -> bool {
        self.has_budget() && self.effective_mode().uses_budget()
    }

    /// Kopie für den Header-Pacer: `enabled` spiegelt
    /// [`Self::header_pacing_enabled`], damit `mode = "budget"` das
    /// Header-Pacing abschaltet, ohne dass der Pacer den Modus kennen muss.
    #[must_use]
    pub fn header_pacer_config(&self) -> Self {
        Self {
            enabled: self.header_pacing_enabled(),
            ..self.clone()
        }
    }

    /// Prüft die Budget-Felder.
    ///
    /// # Arguments
    /// - `owner`: Bezeichner für Fehlermeldungen, z. B. `provider 'openai'`.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`], wenn ein Budget-Feld `0` ist (würde jeden
    /// Request für immer blockieren), `safety_margin_pct` über 100 liegt oder
    /// `mode = "budget"` ohne ein einziges Budget-Feld gesetzt ist.
    pub fn validate(&self, owner: &str) -> ConfigResult<()> {
        let zero_fields = [
            (
                "requests_per_minute",
                self.requests_per_minute.map(u64::from),
            ),
            ("tokens_per_minute", self.tokens_per_minute),
            ("input_tokens_per_minute", self.input_tokens_per_minute),
            ("output_tokens_per_minute", self.output_tokens_per_minute),
            ("max_concurrent", self.max_concurrent.map(u64::from)),
        ];
        for (field, value) in zero_fields {
            if value == Some(0) {
                return Err(ConfigError::Invalid(format!(
                    "{owner}: rate_limit.{field} = 0 would block every request forever; omit the field for no limit or set it to a positive value"
                )));
            }
        }
        if self.safety_margin_pct > 100 {
            return Err(ConfigError::Invalid(format!(
                "{owner}: rate_limit.safety_margin_pct must be between 0 and 100"
            )));
        }
        if self.mode == Some(RateLimitMode::Budget) && !self.has_budget() {
            return Err(ConfigError::Invalid(format!(
                "{owner}: rate_limit.mode = \"budget\" requires at least one of requests_per_minute, tokens_per_minute, input_tokens_per_minute, output_tokens_per_minute or max_concurrent"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_provider_with_auth_secretref() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
            auth = "env:OPENAI_API_KEY"
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert!(!provider.has_plaintext_secret());
        Ok(())
    }

    #[test]
    fn test_provider_legacy_api_key_flagged() -> TestResult {
        let src = r#"
            name = "old-style"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
            api_key = "sk-literal-value-here"
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert!(provider.has_plaintext_secret());
        Ok(())
    }

    #[test]
    fn test_provider_rejects_misspelled_auth_field() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
            auht = "env:OPENAI_API_KEY"
        "#;

        let Err(error) = toml::from_str::<ProviderToml>(src) else {
            return Err(TestError::Unexpected(
                "misspelled auth field should fail to parse".into(),
            ));
        };
        assert!(error.to_string().contains("unknown field `auht`"));
        Ok(())
    }

    fn provider_with_headers(headers: &[(&str, &str)]) -> TestResult<ProviderToml> {
        let src = r#"
            name = "gateway"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            auth = "env:GATEWAY_KEY"
        "#;
        let mut provider: ProviderToml =
            toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        provider.headers = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        Ok(provider)
    }

    #[test]
    fn test_validate_rejects_plaintext_credential_headers() -> TestResult {
        for name in [
            "authorization",
            "Authorization",
            "cf-aig-authorization",
            "x-api-key",
            "API-KEY",
            "x-auth-token",
            "X-Session-Token-Id",
        ] {
            let provider = provider_with_headers(&[(name, "Bearer plaintext-header-secret")])?;
            let Err(error) = provider.validate() else {
                return Err(TestError::Unexpected(format!(
                    "{name}: erwartete validate()-Ablehnung eines Klartext-Headers"
                )));
            };
            assert!(
                matches!(&error, ConfigError::PlaintextSecret { field, .. }
                    if *field == format!("headers.{name}")),
                "{name}: {error}"
            );
            assert!(!error.to_string().contains("plaintext-header-secret"));
        }
        Ok(())
    }

    #[test]
    fn test_validate_accepts_secret_ref_credential_headers_and_plain_other_headers() -> TestResult {
        let provider = provider_with_headers(&[
            ("authorization", "env:GATEWAY_BEARER"),
            ("x-api-key", "secrets:gateway/api-key"),
            ("cf-aig-token", "file:/home/user/.harw/secrets/cf.token"),
            ("x-provider-marker", "plain-value"),
            ("keyboard", "not-a-credential"),
        ])?;
        provider
            .validate()
            .map_err(ctx("secret refs and ordinary headers are valid"))?;
        Ok(())
    }

    #[test]
    fn test_validate_rejects_malformed_secret_ref_and_reports_smallest_name() -> TestResult {
        let provider =
            provider_with_headers(&[("x-api-key", "env:"), ("authorization", "unknown:value")])?;
        let Err(error) = provider.validate() else {
            return Err(TestError::Unexpected(
                "malformed secret ref should fail validation".into(),
            ));
        };
        assert!(matches!(
            error,
            ConfigError::PlaintextSecret { ref field, .. } if field == "headers.authorization"
        ));
        Ok(())
    }

    #[test]
    fn test_provider_rejects_misspelled_api_field() -> TestResult {
        let src = r#"
            name = "openai"
            ap = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#;

        let Err(error) = toml::from_str::<ProviderToml>(src) else {
            return Err(TestError::Unexpected(
                "misspelled api field should fail to parse".into(),
            ));
        };
        assert!(error.to_string().contains("unknown field `ap`"));
        Ok(())
    }

    #[test]
    fn test_provider_with_rate_limit_section_parses_and_defaults_margin() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"

            [rate_limit]
            enabled = true
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        let rate_limit = provider
            .rate_limit
            .ok_or(TestError::Missing("rate_limit section present"))?;
        assert!(rate_limit.enabled);
        assert_eq!(rate_limit.safety_margin_pct, 10);
        Ok(())
    }

    #[test]
    fn test_provider_without_rate_limit_section_is_none() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert!(provider.rate_limit.is_none());
        Ok(())
    }

    #[test]
    fn test_provider_with_max_concurrency_round_trips() -> TestResult {
        let src = r#"
            name = "workers-ai"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            max_concurrency = 3
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert_eq!(provider.max_concurrency, Some(3));
        provider
            .validate()
            .map_err(ctx("max_concurrency = 3 is valid"))?;
        Ok(())
    }

    #[test]
    fn test_provider_without_max_concurrency_is_none() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert!(provider.max_concurrency.is_none());
        provider
            .validate()
            .map_err(ctx("absent max_concurrency is valid (unbounded)"))?;
        Ok(())
    }

    #[test]
    fn test_provider_with_max_concurrency_one_round_trips() -> TestResult {
        let src = r#"
            name = "workers-ai"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            max_concurrency = 1
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert_eq!(provider.max_concurrency, Some(1));
        provider.validate().map_err(ctx(
            "max_concurrency = 1 is the strictest valid value (fully serialized)",
        ))?;
        Ok(())
    }

    #[test]
    fn test_provider_with_max_concurrency_and_rate_limit_both_set_no_interaction() -> TestResult {
        let src = r#"
            name = "workers-ai"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            max_concurrency = 4

            [rate_limit]
            enabled = true
            safety_margin_pct = 20
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert_eq!(provider.max_concurrency, Some(4));
        let rate_limit = provider.rate_limit.clone().ok_or(TestError::Missing(
            "rate_limit section present alongside max_concurrency",
        ))?;
        assert!(rate_limit.enabled);
        assert_eq!(rate_limit.safety_margin_pct, 20);
        provider.validate().map_err(ctx(
            "max_concurrency and rate_limit are independent and both valid together",
        ))?;
        Ok(())
    }

    #[test]
    fn test_provider_without_originator_is_none_and_omitted_on_serialize() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-responses"
            base_url = "https://api.openai.com/v1"
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert!(provider.originator.is_none());
        provider
            .validate()
            .map_err(ctx("absent originator is valid (keeps the harw default)"))?;
        assert!(
            !toml::to_string(&provider)
                .map_err(ctx("provider toml serialisieren"))?
                .contains("originator")
        );
        Ok(())
    }

    #[test]
    fn test_provider_with_originator_round_trips() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-responses"
            base_url = "https://chatgpt.com/backend-api/codex"
            originator = "codex_cli_rs"
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert_eq!(provider.originator.as_deref(), Some("codex_cli_rs"));
        provider
            .validate()
            .map_err(ctx("printable ASCII originator is valid"))?;
        Ok(())
    }

    #[test]
    fn test_validate_rejects_empty_originator() -> TestResult {
        let mut provider = provider_with_headers(&[])?;
        provider.originator = Some(String::new());
        let Err(error) = provider.validate() else {
            return Err(TestError::Unexpected(
                "empty originator should fail validation".into(),
            ));
        };
        assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains("originator")));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_originator_with_control_characters() -> TestResult {
        let mut provider = provider_with_headers(&[])?;
        provider.originator = Some("bad\nvalue".to_owned());
        let Err(error) = provider.validate() else {
            return Err(TestError::Unexpected(
                "control characters in originator should fail validation".into(),
            ));
        };
        assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains("originator")));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_originator_with_non_ascii() -> TestResult {
        let mut provider = provider_with_headers(&[])?;
        provider.originator = Some("härw".to_owned());
        let Err(error) = provider.validate() else {
            return Err(TestError::Unexpected(
                "non-ASCII originator should fail validation".into(),
            ));
        };
        assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains("originator")));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_originator_over_max_length() -> TestResult {
        let mut provider = provider_with_headers(&[])?;
        provider.originator = Some("a".repeat(65));
        let Err(error) = provider.validate() else {
            return Err(TestError::Unexpected(
                "originator over the length cap should fail validation".into(),
            ));
        };
        assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains("originator")));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_originator_at_max_length() -> TestResult {
        let mut provider = provider_with_headers(&[])?;
        provider.originator = Some("a".repeat(64));
        provider
            .validate()
            .map_err(ctx("originator at exactly the length cap is valid"))?;
        Ok(())
    }

    #[test]
    fn test_provider_without_default_reasoning_effort_is_none() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert!(provider.default_reasoning_effort.is_none());
        assert!(
            !toml::to_string(&provider)
                .map_err(ctx("provider toml serialisieren"))?
                .contains("default_reasoning_effort")
        );
        Ok(())
    }

    #[test]
    fn test_provider_with_default_reasoning_effort_round_trips() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
            default_reasoning_effort = "high"
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert_eq!(
            provider.default_reasoning_effort,
            Some(harw_types::ReasoningEffort::High)
        );
        Ok(())
    }

    #[test]
    fn test_provider_rejects_unknown_default_reasoning_effort_value() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
            default_reasoning_effort = "extreme"
        "#;
        let Err(error) = toml::from_str::<ProviderToml>(src) else {
            return Err(TestError::Unexpected(
                "unknown default_reasoning_effort value should fail to parse".into(),
            ));
        };
        assert!(
            error.to_string().contains("extreme") || error.to_string().contains("unknown variant")
        );
        Ok(())
    }

    #[test]
    fn test_provider_without_gateway_identity_headers_defaults_to_false() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert!(!provider.gateway_identity_headers);
        Ok(())
    }

    #[test]
    fn test_provider_with_gateway_identity_headers_true_parses() -> TestResult {
        let src = r#"
            name = "workers-ai"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            gateway_identity_headers = true
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert!(provider.gateway_identity_headers);
        Ok(())
    }

    #[test]
    fn test_serializing_default_gateway_identity_headers_omits_the_key() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        assert!(!provider.gateway_identity_headers);
        assert!(
            !toml::to_string(&provider)
                .map_err(ctx("provider toml serialisieren"))?
                .contains("gateway_identity_headers")
        );
        Ok(())
    }

    #[test]
    fn test_serializing_true_gateway_identity_headers_round_trips() -> TestResult {
        let src = r#"
            name = "workers-ai"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            gateway_identity_headers = true
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        let serialized = toml::to_string(&provider).map_err(ctx("provider toml serialisieren"))?;
        assert!(serialized.contains("gateway_identity_headers = true"));
        let round_tripped: ProviderToml = toml::from_str(&serialized)
            .map_err(ctx("serialisiertes provider toml erneut parsen"))?;
        assert!(round_tripped.gateway_identity_headers);
        Ok(())
    }

    #[test]
    fn test_validate_rejects_max_concurrency_zero() -> TestResult {
        let src = r#"
            name = "workers-ai"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            max_concurrency = 0
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        let Err(error) = provider.validate() else {
            return Err(TestError::Unexpected(
                "max_concurrency = 0 should fail validation".into(),
            ));
        };
        assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains("max_concurrency")));
        Ok(())
    }

    #[test]
    fn test_rate_limit_budget_fields_parse_and_default_mode_is_both() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"

            [rate_limit]
            requests_per_minute = 500
            tokens_per_minute = 30000
            input_tokens_per_minute = 20000
            output_tokens_per_minute = 8000
            max_concurrent = 4
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        provider
            .validate()
            .map_err(ctx("positive budgets are valid"))?;
        let rate_limit = provider
            .rate_limit
            .ok_or(TestError::Missing("rate_limit section present"))?;
        assert_eq!(rate_limit.requests_per_minute, Some(500));
        assert_eq!(rate_limit.tokens_per_minute, Some(30_000));
        assert_eq!(rate_limit.input_tokens_per_minute, Some(20_000));
        assert_eq!(rate_limit.output_tokens_per_minute, Some(8_000));
        assert_eq!(rate_limit.max_concurrent, Some(4));
        assert_eq!(rate_limit.mode, None);
        assert_eq!(rate_limit.effective_mode(), RateLimitMode::Both);
        assert!(rate_limit.budget_enabled());
        assert!(
            !rate_limit.header_pacing_enabled(),
            "header pacing still requires enabled = true"
        );
        Ok(())
    }

    #[test]
    fn test_rate_limit_without_budget_keeps_header_mode() -> TestResult {
        let rate_limit: RateLimitToml =
            toml::from_str("enabled = true").map_err(ctx("rate_limit parsen"))?;
        assert_eq!(rate_limit.effective_mode(), RateLimitMode::Header);
        assert!(rate_limit.header_pacing_enabled());
        assert!(!rate_limit.budget_enabled());
        assert!(rate_limit.header_pacer_config().enabled);
        Ok(())
    }

    #[test]
    fn test_rate_limit_budget_mode_disables_header_pacing() -> TestResult {
        let rate_limit: RateLimitToml = toml::from_str(
            r#"
                enabled = true
                mode = "budget"
                tokens_per_minute = 1000
            "#,
        )
        .map_err(ctx("rate_limit parsen"))?;
        assert!(!rate_limit.header_pacing_enabled());
        assert!(!rate_limit.header_pacer_config().enabled);
        assert!(rate_limit.budget_enabled());
        Ok(())
    }

    #[test]
    fn test_rate_limit_header_mode_ignores_budgets() -> TestResult {
        let rate_limit: RateLimitToml = toml::from_str(
            r#"
                enabled = true
                mode = "header"
                requests_per_minute = 10
            "#,
        )
        .map_err(ctx("rate_limit parsen"))?;
        assert!(rate_limit.header_pacing_enabled());
        assert!(!rate_limit.budget_enabled());
        Ok(())
    }

    #[test]
    fn test_rate_limit_rejects_unknown_mode_and_field() {
        assert!(toml::from_str::<RateLimitToml>(r#"mode = "sometimes""#).is_err());
        assert!(toml::from_str::<RateLimitToml>("tokens_per_minut = 5").is_err());
    }

    #[test]
    fn test_rate_limit_validate_rejects_zero_budgets() -> TestResult {
        for field in [
            "requests_per_minute",
            "tokens_per_minute",
            "input_tokens_per_minute",
            "output_tokens_per_minute",
            "max_concurrent",
        ] {
            let rate_limit: RateLimitToml =
                toml::from_str(&format!("{field} = 0")).map_err(ctx("rate_limit parsen"))?;
            let Err(error) = rate_limit.validate("provider 'x'") else {
                return Err(TestError::Unexpected(format!(
                    "{field} = 0 should fail validation"
                )));
            };
            assert!(
                matches!(&error, ConfigError::Invalid(msg) if msg.contains(field)),
                "{field}: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_rate_limit_validate_rejects_budget_mode_without_budget() -> TestResult {
        let rate_limit: RateLimitToml =
            toml::from_str(r#"mode = "budget""#).map_err(ctx("rate_limit parsen"))?;
        assert!(rate_limit.validate("provider 'x'").is_err());
        Ok(())
    }

    #[test]
    fn test_provider_validate_propagates_rate_limit_errors() -> TestResult {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"

            [rate_limit]
            tokens_per_minute = 0
        "#;
        let provider: ProviderToml = toml::from_str(src).map_err(ctx("provider toml parsen"))?;
        let Err(error) = provider.validate() else {
            return Err(TestError::Unexpected(
                "tokens_per_minute = 0 should fail provider validation".into(),
            ));
        };
        assert!(
            matches!(error, ConfigError::Invalid(ref msg) if msg.contains("provider 'openai'") && msg.contains("tokens_per_minute"))
        );
        Ok(())
    }

    #[test]
    fn test_rate_limit_budget_fields_round_trip() -> TestResult {
        let rate_limit = RateLimitToml {
            requests_per_minute: Some(60),
            tokens_per_minute: Some(90_000),
            mode: Some(RateLimitMode::Both),
            ..RateLimitToml::default()
        };
        let encoded = toml::to_string(&rate_limit).map_err(ctx("rate_limit serialisieren"))?;
        assert!(encoded.contains("requests_per_minute = 60"));
        assert!(encoded.contains(r#"mode = "both""#));
        assert!(!encoded.contains("input_tokens_per_minute"));
        let decoded: RateLimitToml =
            toml::from_str(&encoded).map_err(ctx("rate_limit erneut parsen"))?;
        assert_eq!(decoded, rate_limit);
        Ok(())
    }

    // Runde 7, Teil L: lokale Vorgaben und neue Kompatibilitätsfelder.

    fn provider_from(src: &str) -> TestResult<ProviderToml> {
        toml::from_str(src).map_err(ctx("provider toml parsen"))
    }

    #[test]
    fn test_loopback_provider_is_local_and_gets_local_defaults() -> TestResult {
        let provider = provider_from(
            r#"
            name = "vllm"
            api = "openai-chat"
            base_url = "http://localhost:8000/v1"
        "#,
        )?;
        assert!(provider.is_local());
        assert_eq!(provider.effective_request_timeout_secs(), 600);
        assert_eq!(provider.effective_stream_idle_timeout_secs(), 120);
        assert!(!provider.effective_retry_timeouts());
        assert_eq!(
            provider.effective_max_tokens_field(),
            MaxTokensField::MaxTokens
        );
        assert!(!provider.effective_send_reasoning_effort());
        assert!(!provider.effective_strict_tools());
        assert_eq!(provider.effective_parallel_tool_calls(), Some(false));
        Ok(())
    }

    #[test]
    fn test_cloud_provider_keeps_previous_defaults() -> TestResult {
        let provider = provider_from(
            r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#,
        )?;
        assert!(!provider.is_local());
        assert_eq!(provider.effective_request_timeout_secs(), 120);
        assert_eq!(provider.effective_stream_idle_timeout_secs(), 120);
        assert!(provider.effective_retry_timeouts());
        assert_eq!(
            provider.effective_max_tokens_field(),
            MaxTokensField::MaxCompletionTokens
        );
        assert!(provider.effective_send_reasoning_effort());
        assert!(provider.effective_strict_tools());
        assert_eq!(provider.effective_parallel_tool_calls(), None);
        Ok(())
    }

    #[test]
    fn test_explicit_compat_fields_override_local_defaults() -> TestResult {
        let provider = provider_from(
            r#"
            name = "lmstudio"
            api = "openai-chat"
            base_url = "http://127.0.0.1:1234/v1"
            request_timeout_secs = 900
            stream_idle_timeout_secs = 45
            retry_timeouts = true
            max_tokens_field = "both"
            send_reasoning_effort = true
            strict_tools = true
            parallel_tool_calls = true
        "#,
        )?;
        assert_eq!(provider.effective_request_timeout_secs(), 900);
        assert_eq!(provider.effective_stream_idle_timeout_secs(), 45);
        assert!(provider.effective_retry_timeouts());
        assert_eq!(provider.effective_max_tokens_field(), MaxTokensField::Both);
        assert!(provider.effective_send_reasoning_effort());
        assert!(provider.effective_strict_tools());
        assert_eq!(provider.effective_parallel_tool_calls(), Some(true));
        Ok(())
    }

    #[test]
    fn test_lan_provider_is_local_only_with_opt_in() -> TestResult {
        let mut provider = provider_from(
            r#"
            name = "gpu-box"
            api = "openai-chat"
            base_url = "http://192.168.1.20:8000/v1"
        "#,
        )?;
        assert!(!provider.is_local());
        provider.allow_insecure_lan = true;
        assert!(provider.is_local());
        assert!(host_is_private_lan("10.1.2.3"));
        assert!(host_is_private_lan("172.16.0.1"));
        assert!(host_is_private_lan("fd00::1"));
        assert!(!host_is_private_lan("8.8.8.8"));
        assert!(!host_is_private_lan("gpu-box.lan"));
        Ok(())
    }

    #[test]
    fn test_url_host_handles_ipv6_userinfo_and_ports() {
        assert_eq!(url_host("http://[::1]:8000/v1").as_deref(), Some("::1"));
        assert_eq!(
            url_host("http://user@LocalHost:1234/v1").as_deref(),
            Some("localhost")
        );
        assert_eq!(
            url_host("https://api.example.com").as_deref(),
            Some("api.example.com")
        );
        assert!(host_is_loopback("127.0.0.2"));
        assert!(host_is_loopback("::ffff:127.0.0.1"));
        assert!(!host_is_loopback("localhost.example.com"));
    }

    #[test]
    fn test_validate_rejects_zero_timeouts() -> TestResult {
        for field in ["request_timeout_secs", "stream_idle_timeout_secs"] {
            let provider = provider_from(&format!(
                "name = \"local\"\napi = \"openai-chat\"\nbase_url = \"http://localhost:8000/v1\"\n{field} = 0\n"
            ))?;
            let Err(error) = provider.validate() else {
                return Err(TestError::Unexpected(format!(
                    "{field} = 0 must be rejected"
                )));
            };
            assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains(field)));
        }
        Ok(())
    }

    #[test]
    fn test_new_compat_fields_are_omitted_when_unset() -> TestResult {
        let provider = provider_from(
            r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#,
        )?;
        let encoded = toml::to_string(&provider).map_err(ctx("provider toml serialisieren"))?;
        for field in [
            "request_timeout_secs",
            "stream_idle_timeout_secs",
            "retry_timeouts",
            "max_tokens_field",
            "send_reasoning_effort",
            "strict_tools",
            "parallel_tool_calls",
            "allow_insecure_lan",
        ] {
            assert!(!encoded.contains(field), "{field} sollte fehlen: {encoded}");
        }
        Ok(())
    }
}
