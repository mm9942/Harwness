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
    /// Client-seitiges Rate-Limiting für diesen Provider; `None` = kein
    /// Override (deaktiviert, siehe [`RateLimitToml::default`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitToml>,
    /// Harte Obergrenze gleichzeitig in Flug befindlicher Requests an
    /// diesen Provider; `None` = unbegrenzt. Anders als `rate_limit`
    /// (reaktives Header-Pacing) ist dies ein rein client-seitiger
    /// Zähler, der zusätzliche Requests blockiert statt sie fehlschlagen
    /// zu lassen. `Some(0)` ist ungültig (siehe [`Self::validate`]) und
    /// würde jeden Request auf ewig blockieren.
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

fn default_true() -> bool {
    true
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
            ("cf-aig-token", "file:/home/mia/.harw/secrets/cf.token"),
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
}
