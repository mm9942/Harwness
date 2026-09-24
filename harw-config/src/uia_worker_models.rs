//! Eigene Modellwahl je UIA-Worker-Rolle (Runde 5, Teil G).
//!
//! # Hintergrund
//! Bis Runde 5 hatte die ganze `uia-worker`-Rollenfamilie genau **einen**
//! Pin, `uia_worker_model`, und zwar nur als Modell-Kennung: der Provider
//! war zwingend der effektive UIA-Provider (Kopplungsregel). Ein Wechsel des
//! UIA-Providers machte den Pin damit ungültig — `/models set uia-worker`
//! und `/uia-worker-model switch` lehnten Modelle anderer Provider ab, und
//! die Runtime verwarf den Pin beim nächsten Start still.
//!
//! # Regel
//! Jede Rolle der Familie ([`UIA_WORKER_ROLES`]) bekommt eine eigene Wahl
//! unter `[uia_worker_models]`, als Zeichenkette je Rolle:
//!
//! | Wert | Bedeutung |
//! |---|---|
//! | `"uia"` | „wie UIA“: die Rolle folgt dem (auch live gewechselten) UIA-Modell |
//! | `"<provider>/<modell>"` | feste Wahl, getrennt am **ersten** `/` |
//!
//! Ohne Eintrag gilt der alte `uia_worker_model`-Pin als feste Wahl — jetzt
//! mit dem Provider, dem das Modell im Katalog gehört (nicht mehr der
//! UIA-Provider) —, sonst „wie UIA“. Eine feste Wahl bleibt nur, solange ihr
//! Provider angemeldet ist ([`provider_is_logged_in`]); sonst fällt die Rolle
//! mit einem Hinweis auf „wie UIA“ zurück. Der UIA-Wechsel selbst scheitert
//! dadurch nie an einer Worker-Bindung.
//!
//! # Nebenläufigkeit
//! Reine Funktionen über [`crate::ResolvedConfig`].

use serde::{Deserialize, Serialize};

/// Die Rollen der `uia-worker`-Familie (Organisationsrolle `uia-worker`),
/// in Anzeige-Reihenfolge.
pub const UIA_WORKER_ROLES: [&str; 5] = [
    "uia-worker",
    "uia-shell-worker",
    "uia-writer",
    "uia-latex-writer",
    "uia-explorer",
];

/// Konfigurationswert für „wie UIA“.
pub const FOLLOW_UIA_VALUE: &str = "uia";

/// `[uia_worker_models]` — eine Wahl je UIA-Worker-Rolle (siehe
/// Moduldoku). TOML-Schlüssel sind die Rollennamen mit `_` statt `-`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiaWorkerModelsToml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uia_worker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uia_shell_worker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uia_writer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uia_latex_writer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uia_explorer: Option<String>,
}

impl UiaWorkerModelsToml {
    /// TOML-Schlüssel einer Rolle (`uia-shell-worker` → `uia_shell_worker`),
    /// `None` für Rollen außerhalb von [`UIA_WORKER_ROLES`].
    #[must_use]
    pub fn toml_key(role: &str) -> Option<&'static str> {
        match role.trim().replace('_', "-").as_str() {
            "uia-worker" => Some("uia_worker"),
            "uia-shell-worker" => Some("uia_shell_worker"),
            "uia-writer" => Some("uia_writer"),
            "uia-latex-writer" => Some("uia_latex_writer"),
            "uia-explorer" => Some("uia_explorer"),
            _ => None,
        }
    }

    /// Gespeicherter Rohwert einer Rolle.
    #[must_use]
    pub fn get(&self, role: &str) -> Option<&str> {
        let slot = match Self::toml_key(role)? {
            "uia_worker" => &self.uia_worker,
            "uia_shell_worker" => &self.uia_shell_worker,
            "uia_writer" => &self.uia_writer,
            "uia_latex_writer" => &self.uia_latex_writer,
            _ => &self.uia_explorer,
        };
        slot.as_deref()
    }

    /// Setzt (oder entfernt, bei `None`) den Rohwert einer Rolle.
    ///
    /// # Rückgabe
    /// `false` für eine Rolle außerhalb von [`UIA_WORKER_ROLES`].
    pub fn set(&mut self, role: &str, value: Option<String>) -> bool {
        let slot = match Self::toml_key(role) {
            Some("uia_worker") => &mut self.uia_worker,
            Some("uia_shell_worker") => &mut self.uia_shell_worker,
            Some("uia_writer") => &mut self.uia_writer,
            Some("uia_latex_writer") => &mut self.uia_latex_writer,
            Some(_) => &mut self.uia_explorer,
            None => return false,
        };
        *slot = value;
        true
    }
}

/// Die Wahl einer UIA-Worker-Rolle.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UiaWorkerModelChoice {
    /// „wie UIA“ — folgt dem (auch live gewechselten) UIA-Modell.
    FollowUia,
    /// Feste Wahl.
    Fixed {
        /// Kanonischer Provider-Name.
        provider: String,
        /// Modell-Kennung.
        model: String,
    },
}

impl UiaWorkerModelChoice {
    /// Parst einen Konfigurationswert (`"uia"` oder `"<provider>/<modell>"`).
    ///
    /// # Rückgabe
    /// `None` für einen leeren oder unlesbaren Wert.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case(FOLLOW_UIA_VALUE) || value.eq_ignore_ascii_case("wie-uia") {
            return Some(Self::FollowUia);
        }
        let (provider, model) = value.split_once('/')?;
        let (provider, model) = (provider.trim(), model.trim());
        if provider.is_empty() || model.is_empty() {
            return None;
        }
        Some(Self::Fixed {
            provider: provider.to_owned(),
            model: model.to_owned(),
        })
    }

    /// Wert, wie er unter `[uia_worker_models]` gespeichert wird.
    #[must_use]
    pub fn to_config_value(&self) -> String {
        match self {
            Self::FollowUia => FOLLOW_UIA_VALUE.to_owned(),
            Self::Fixed { provider, model } => format!("{provider}/{model}"),
        }
    }

    /// Deutsches Anzeige-Label („wie UIA“ oder `provider/modell`).
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::FollowUia => "wie UIA".to_owned(),
            Self::Fixed { provider, model } => format!("{provider}/{model}"),
        }
    }

    /// `true` für „wie UIA“.
    #[must_use]
    pub fn follows_uia(&self) -> bool {
        matches!(self, Self::FollowUia)
    }
}

/// Herkunft der aufgelösten Wahl.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiaWorkerModelSource {
    /// Eintrag unter `[uia_worker_models]`.
    Explicit,
    /// Alter Pin `uia_worker_model` (Provider aus dem Katalog).
    Legacy,
    /// Keine Wahl — Vorgabe „wie UIA“.
    Default,
    /// Feste Wahl, deren Provider nicht (mehr) angemeldet ist oder die nicht
    /// lesbar war — Rückfall auf „wie UIA“.
    FellBack,
}

impl UiaWorkerModelSource {
    /// Deutsches Anzeige-Label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Explicit => "eigene Wahl",
            Self::Legacy => "uia_worker_model",
            Self::Default => "Vorgabe",
            Self::FellBack => "Rückfall",
        }
    }
}

/// Aufgelöste Wahl einer UIA-Worker-Rolle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUiaWorkerModel {
    /// Rollenname (z. B. `uia-writer`).
    pub role: String,
    /// Die wirksame Wahl.
    pub choice: UiaWorkerModelChoice,
    /// Woher sie stammt.
    pub source: UiaWorkerModelSource,
    /// Hinweis für die Nutzerin bei einem Rückfall auf „wie UIA“.
    pub notice: Option<String>,
}

/// Kanonischer Name des Providers `name` (Schlüssel oder `name`), falls
/// konfiguriert.
fn canonical_provider<'a>(config: &'a crate::ResolvedConfig, name: &str) -> Option<&'a str> {
    config
        .providers
        .iter()
        .find(|(key, provider)| key.as_str() == name || provider.name == name)
        .map(|(_, provider)| provider.name.as_str())
}

/// `true`, wenn der Provider `name` konfiguriert, aktiviert und angemeldet
/// ist: Auth-Verweis oder (veraltet) Klartext-Schlüssel vorhanden, oder der
/// Provider verlangt ausdrücklich keine Auth (`auth_header = "none"`, etwa
/// ein lokaler Endpunkt).
#[must_use]
pub fn provider_is_logged_in(config: &crate::ResolvedConfig, name: &str) -> bool {
    config
        .providers
        .iter()
        .find(|(key, provider)| key.as_str() == name || provider.name == name)
        .is_some_and(|(_, provider)| {
            provider.enabled
                && (provider.auth.is_some()
                    || provider.has_plaintext_secret()
                    || provider
                        .auth_header
                        .as_deref()
                        .is_some_and(|header| header.eq_ignore_ascii_case("none")))
        })
}

/// Provider, dem `model` im Katalog gehört (Schlüssel, `id` oder Alias),
/// kanonisiert.
#[must_use]
pub fn catalog_provider_of(config: &crate::ResolvedConfig, model: &str) -> Option<String> {
    let entry = config.models.get(model).or_else(|| {
        config
            .models
            .values()
            .find(|entry| entry.id == model || entry.aliases.iter().any(|alias| alias == model))
    })?;
    Some(
        canonical_provider(config, &entry.provider)
            .unwrap_or(entry.provider.as_str())
            .to_owned(),
    )
}

/// Rückfall auf „wie UIA“ mit Hinweis.
fn fell_back(role: &str, notice: String) -> ResolvedUiaWorkerModel {
    ResolvedUiaWorkerModel {
        role: role.to_owned(),
        choice: UiaWorkerModelChoice::FollowUia,
        source: UiaWorkerModelSource::FellBack,
        notice: Some(notice),
    }
}

/// Prüft eine feste Wahl gegen die angemeldeten Provider.
fn checked_fixed(
    config: &crate::ResolvedConfig,
    role: &str,
    provider: &str,
    model: &str,
    source: UiaWorkerModelSource,
) -> ResolvedUiaWorkerModel {
    if !provider_is_logged_in(config, provider) {
        return fell_back(
            role,
            format!(
                "{role}: Provider „{provider}“ ist nicht angemeldet — die Rolle folgt wieder \
                 der UIA (wie UIA)."
            ),
        );
    }
    ResolvedUiaWorkerModel {
        role: role.to_owned(),
        choice: UiaWorkerModelChoice::Fixed {
            provider: canonical_provider(config, provider)
                .unwrap_or(provider)
                .to_owned(),
            model: model.to_owned(),
        },
        source,
        notice: None,
    }
}

/// Löst die wirksame Wahl einer UIA-Worker-Rolle auf (siehe Moduldoku).
///
/// # Argumente
/// - `config`: die aufgelöste Konfiguration.
/// - `role`: Rollenname; Rollen außerhalb von [`UIA_WORKER_ROLES`] (etwa
///   repo-lokale Rollen mit Organisationsrolle `uia-worker`) folgen immer
///   der UIA, außer der alte `uia_worker_model`-Pin greift.
#[must_use]
pub fn resolve_uia_worker_model(
    config: &crate::ResolvedConfig,
    role: &str,
) -> ResolvedUiaWorkerModel {
    if let Some(raw) = config.harness.uia_worker_models.get(role) {
        return match UiaWorkerModelChoice::parse(raw) {
            Some(UiaWorkerModelChoice::FollowUia) => ResolvedUiaWorkerModel {
                role: role.to_owned(),
                choice: UiaWorkerModelChoice::FollowUia,
                source: UiaWorkerModelSource::Explicit,
                notice: None,
            },
            Some(UiaWorkerModelChoice::Fixed { provider, model }) => checked_fixed(
                config,
                role,
                &provider,
                &model,
                UiaWorkerModelSource::Explicit,
            ),
            None => fell_back(
                role,
                format!(
                    "{role}: Modellwahl „{raw}“ ist unlesbar (erwartet „uia“ oder \
                     „provider/modell“) — die Rolle folgt der UIA."
                ),
            ),
        };
    }
    if let Some(legacy) = config
        .harness
        .uia_worker_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return match catalog_provider_of(config, legacy) {
            Some(provider) => checked_fixed(
                config,
                role,
                &provider,
                legacy,
                UiaWorkerModelSource::Legacy,
            ),
            None => fell_back(
                role,
                format!(
                    "{role}: uia_worker_model „{legacy}“ steht nicht im Modellkatalog — die \
                     Rolle folgt der UIA."
                ),
            ),
        };
    }
    ResolvedUiaWorkerModel {
        role: role.to_owned(),
        choice: UiaWorkerModelChoice::FollowUia,
        source: UiaWorkerModelSource::Default,
        notice: None,
    }
}

/// Löst alle Rollen aus [`UIA_WORKER_ROLES`] in Anzeige-Reihenfolge auf.
#[must_use]
pub fn resolve_uia_worker_models(config: &crate::ResolvedConfig) -> Vec<ResolvedUiaWorkerModel> {
    UIA_WORKER_ROLES
        .iter()
        .map(|role| resolve_uia_worker_model(config, role))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResolvedConfig;
    use crate::auth_toml::SecretRef;
    use crate::model_toml::ModelToml;
    use crate::provider_toml::ProviderToml;
    use crate::test_support::{TestResult, ctx};
    use std::collections::HashMap;
    use std::str::FromStr;

    fn provider(name: &str, enabled: bool, with_auth: bool) -> TestResult<ProviderToml> {
        Ok(ProviderToml {
            stream: None,
            name: name.to_owned(),
            api: "openai-chat".to_owned(),
            base_url: "https://example.invalid/v1".to_owned(),
            auth: if with_auth {
                Some(SecretRef::from_str("env:TEST_API_KEY").map_err(ctx("secret ref"))?)
            } else {
                None
            },
            auth_header: None,
            api_key: None,
            headers: HashMap::new(),
            models: Vec::new(),
            enabled,
            origin_allowlist: Default::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
            default_reasoning_effort: None,
            gateway_identity_headers: false,
            request_timeout_secs: None,
            stream_idle_timeout_secs: None,
            retry_timeouts: None,
            max_tokens_field: None,
            send_reasoning_effort: None,
            strict_tools: None,
            parallel_tool_calls: None,
            allow_insecure_lan: false,
        })
    }

    fn model(id: &str, provider: &str) -> TestResult<ModelToml> {
        toml::from_str(&format!("id = \"{id}\"\nprovider = \"{provider}\"\n"))
            .map_err(ctx("model toml"))
    }

    /// Zwei angemeldete Provider (`anthropic`, `openai`), ein abgemeldeter
    /// (`local`), je ein Katalogmodell.
    fn config() -> TestResult<ResolvedConfig> {
        let mut config = ResolvedConfig::default();
        for (name, auth) in [("anthropic", true), ("openai", true), ("local", false)] {
            config
                .providers
                .insert(name.to_owned(), provider(name, true, auth)?);
        }
        config.models.insert(
            "claude-opus-5-5".to_owned(),
            model("claude-opus-5-5", "anthropic")?,
        );
        config
            .models
            .insert("gpt-5".to_owned(), model("gpt-5", "openai")?);
        config.harness.uia_provider = Some("anthropic".to_owned());
        config.harness.uia_model = Some("claude-opus-5-5".to_owned());
        Ok(config)
    }

    #[test]
    fn choice_parses_follow_and_fixed_values() {
        assert_eq!(
            UiaWorkerModelChoice::parse(" UIA "),
            Some(UiaWorkerModelChoice::FollowUia)
        );
        assert_eq!(
            UiaWorkerModelChoice::parse("openrouter/nvidia/nemotron-x"),
            Some(UiaWorkerModelChoice::Fixed {
                provider: "openrouter".to_owned(),
                model: "nvidia/nemotron-x".to_owned(),
            })
        );
        assert_eq!(UiaWorkerModelChoice::parse("nur-modell"), None);
        assert_eq!(UiaWorkerModelChoice::parse("/modell"), None);
        let fixed = UiaWorkerModelChoice::Fixed {
            provider: "openai".to_owned(),
            model: "gpt-5".to_owned(),
        };
        assert_eq!(
            UiaWorkerModelChoice::parse(&fixed.to_config_value()),
            Some(fixed)
        );
    }

    #[test]
    fn toml_keys_cover_every_role_and_set_get_round_trip() {
        let mut table = UiaWorkerModelsToml::default();
        for role in UIA_WORKER_ROLES {
            assert!(UiaWorkerModelsToml::toml_key(role).is_some(), "{role}");
            assert!(table.set(role, Some(format!("p/{role}"))));
            assert_eq!(table.get(role), Some(format!("p/{role}").as_str()));
        }
        assert!(!table.set("host-process-worker", Some("uia".to_owned())));
        assert_eq!(table.get("host-process-worker"), None);
    }

    #[test]
    fn roles_default_to_following_the_uia() -> TestResult {
        let config = config()?;
        for resolved in resolve_uia_worker_models(&config) {
            assert_eq!(resolved.choice, UiaWorkerModelChoice::FollowUia);
            assert_eq!(resolved.source, UiaWorkerModelSource::Default);
            assert_eq!(resolved.notice, None);
        }
        Ok(())
    }

    /// Eine feste Wahl bei einem **anderen** Provider als der UIA ist
    /// erlaubt (keine Kopplung mehr).
    #[test]
    fn fixed_choice_on_another_provider_is_kept() -> TestResult {
        let mut config = config()?;
        config
            .harness
            .uia_worker_models
            .set("uia-writer", Some("openai/gpt-5".to_owned()));
        let resolved = resolve_uia_worker_model(&config, "uia-writer");
        assert_eq!(
            resolved.choice,
            UiaWorkerModelChoice::Fixed {
                provider: "openai".to_owned(),
                model: "gpt-5".to_owned(),
            }
        );
        assert_eq!(resolved.source, UiaWorkerModelSource::Explicit);
        // Die übrigen Rollen folgen weiter der UIA.
        assert!(
            resolve_uia_worker_model(&config, "uia-worker")
                .choice
                .follows_uia()
        );
        Ok(())
    }

    /// Ist der Provider einer festen Wahl nicht angemeldet, fällt die Rolle
    /// mit Hinweis auf „wie UIA“ zurück.
    #[test]
    fn fixed_choice_without_logged_in_provider_falls_back_with_notice() -> TestResult {
        let mut config = config()?;
        config
            .harness
            .uia_worker_models
            .set("uia-explorer", Some("local/tiny".to_owned()));
        config
            .harness
            .uia_worker_models
            .set("uia-writer", Some("gone/model".to_owned()));
        for role in ["uia-explorer", "uia-writer"] {
            let resolved = resolve_uia_worker_model(&config, role);
            assert!(resolved.choice.follows_uia(), "{role}");
            assert_eq!(resolved.source, UiaWorkerModelSource::FellBack);
            assert!(
                resolved
                    .notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("nicht angemeldet")),
                "{role}: {resolved:?}"
            );
        }
        Ok(())
    }

    /// Der alte Pin bekommt den Provider aus dem Katalog — auch wenn die UIA
    /// inzwischen einen anderen Provider nutzt.
    #[test]
    fn legacy_pin_uses_catalog_provider_not_uia_provider() -> TestResult {
        let mut config = config()?;
        config.harness.uia_worker_model = Some("gpt-5".to_owned());
        let resolved = resolve_uia_worker_model(&config, "uia-shell-worker");
        assert_eq!(
            resolved.choice,
            UiaWorkerModelChoice::Fixed {
                provider: "openai".to_owned(),
                model: "gpt-5".to_owned(),
            }
        );
        assert_eq!(resolved.source, UiaWorkerModelSource::Legacy);

        // Eine eigene Rollenwahl („wie UIA“) schlägt den alten Pin.
        config
            .harness
            .uia_worker_models
            .set("uia-shell-worker", Some(FOLLOW_UIA_VALUE.to_owned()));
        let resolved = resolve_uia_worker_model(&config, "uia-shell-worker");
        assert!(resolved.choice.follows_uia());
        assert_eq!(resolved.source, UiaWorkerModelSource::Explicit);
        Ok(())
    }

    #[test]
    fn provider_login_requires_enabled_provider_with_auth_or_explicit_none() -> TestResult {
        let mut config = config()?;
        assert!(provider_is_logged_in(&config, "openai"));
        assert!(!provider_is_logged_in(&config, "local"));
        assert!(!provider_is_logged_in(&config, "unknown"));
        if let Some(local) = config.providers.get_mut("local") {
            local.auth_header = Some("none".to_owned());
        }
        assert!(provider_is_logged_in(&config, "local"));
        if let Some(openai) = config.providers.get_mut("openai") {
            openai.enabled = false;
        }
        assert!(!provider_is_logged_in(&config, "openai"));
        Ok(())
    }
}
