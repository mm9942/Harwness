//! Aufgelöste Modelle je Rolle — gemeinsame Sicht für `/models` und die TUI.
//!
//! Führt die verstreuten Modellwahlen (UIA-Pin `uia_provider`/`uia_model`,
//! UIA-Worker-Pin `uia_worker_model`, Orchestrator-Stellen und die übrigen
//! [`InternalModelPoint`]s) in eine einheitliche Tabelle zusammen: pro
//! [`ModelRole`] genau eine [`RoleModelRow`] mit Provider, Modell, Herkunft
//! ([`RoleModelSource`]) und Reasoning-Effort.
//!
//! Auflösungsregeln:
//! - **UIA**: `uia_provider` + `uia_model` mit vorhandenem, aktiviertem
//!   Provider → [`RoleModelSource::UiaPin`]; sonst `default_model` →
//!   [`RoleModelSource::DefaultModel`]; sonst [`RoleModelSource::Unset`].
//!   (Spiegelt `harw-runtime` `resolve_uia_model`.)
//! - **UIA-Worker** (Rolle `uia-worker`, Runde 5 Teil G): feste Wahl aus
//!   `[uia_worker_models]` bzw. dem alten `uia_worker_model` mit eigenem
//!   Provider → [`RoleModelSource::UiaWorkerPin`], sonst Provider/Modell der
//!   UIA-Zeile → [`RoleModelSource::InheritsUia`] (siehe
//!   [`crate::resolve_uia_worker_model`]).
//! - **Orchestrator**: explizite Wahl `internal_models.root_orchestrator` →
//!   [`RoleModelSource::Explicit`], sonst Hauptmodell →
//!   [`RoleModelSource::DefaultModel`] (bzw. `Unset` ohne `default_model`).
//! - **Sub-Orchestrator**: explizite Wahl `internal_models.sub_orchestrator`,
//!   sonst Provider/Modell der Orchestrator-Zeile →
//!   [`RoleModelSource::InheritsOrchestrator`].
//! - **Übrige Rollen**: [`resolve_internal_model`] — `Explicit` →
//!   `Explicit`, `OpenRouterDefault` → `OpenRouterDefault`, `MainModel` →
//!   `DefaultModel` (bzw. `Unset` ohne `default_model`).
//!
//! Reasoning-Effort: zuerst das rollenspezifische `[reasoning]`-Feld
//! (`uia`, `root_orchestrator`, `sub_orchestrator`, `worker_simple`,
//! `worker_complex`), sonst `default_reasoning_effort` des aufgelösten
//! Modells, sonst des aufgelösten Providers, sonst `None`.

use crate::ResolvedConfig;
use crate::internal_models::{
    InternalModelPoint, InternalModelSource, fast_model_for_active_provider, resolve_internal_model,
};

/// Rolle, für die ein Modell gewählt bzw. angezeigt werden kann.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelRole {
    /// Interaktive Benutzeroberflächen-Sitzung (UIA).
    Uia,
    /// uia-worker-Rollenfamilie (`uia-worker`, `uia-explorer`, …).
    UiaWorker,
    /// Wurzel-Orchestrator.
    Orchestrator,
    /// Unter-Orchestrator.
    SubOrchestrator,
    /// Worker mit einfacher Aufgabe.
    WorkerSimple,
    /// Worker mit komplexer Aufgabe.
    WorkerComplex,
    /// Code-/Projekt-Erkundung.
    Explorer,
    /// Recherche externer Quellen.
    Research,
    /// Verlaufs-Zusammenfassung beim Verdichten.
    CompactionSummary,
    /// Sitzungstitel.
    SessionTitle,
    /// Gedächtnis-Konsolidierung.
    MemoryConsolidation,
    /// Traum-Reflexion.
    DreamReflection,
    /// Runde 5, Teil E: Klassifizierer des Auto-Modus.
    AutoClassifier,
    /// R14: interner Bewerter des work drivers (DEC-002).
    WorkDriverJudge,
}

impl ModelRole {
    /// Alle Rollen in stabiler Anzeige-Reihenfolge.
    pub const ALL: [ModelRole; 14] = [
        ModelRole::Uia,
        ModelRole::UiaWorker,
        ModelRole::Orchestrator,
        ModelRole::SubOrchestrator,
        ModelRole::WorkerSimple,
        ModelRole::WorkerComplex,
        ModelRole::Explorer,
        ModelRole::Research,
        ModelRole::CompactionSummary,
        ModelRole::SessionTitle,
        ModelRole::MemoryConsolidation,
        ModelRole::DreamReflection,
        ModelRole::AutoClassifier,
        ModelRole::WorkDriverJudge,
    ];

    /// Kurzer, stabiler Schlüssel für Befehle (`/models set <rolle> …`).
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Uia => "uia",
            Self::UiaWorker => "uia-worker",
            Self::Orchestrator => "orchestrator",
            Self::SubOrchestrator => "sub-orchestrator",
            Self::WorkerSimple => "worker-simple",
            Self::WorkerComplex => "worker-complex",
            Self::Explorer => "explorer",
            Self::Research => "research",
            Self::CompactionSummary => "compaction",
            Self::SessionTitle => "title",
            Self::MemoryConsolidation => "memory",
            Self::DreamReflection => "dream",
            Self::AutoClassifier => "auto-classifier",
            Self::WorkDriverJudge => "work-driver-judge",
        }
    }

    /// Deutsches Anzeige-Label der Rolle.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Uia => "Benutzeroberfläche (UIA)",
            Self::UiaWorker => "UIA-Worker",
            Self::Orchestrator => "Orchestrator",
            Self::SubOrchestrator => "Sub-Orchestrator",
            Self::WorkerSimple => "Worker (einfach)",
            Self::WorkerComplex => "Worker (komplex)",
            Self::Explorer => "Explorer",
            Self::Research => "Recherche",
            Self::CompactionSummary => "Verdichtung",
            Self::SessionTitle => "Sitzungstitel",
            Self::MemoryConsolidation => "Gedächtnis-Konsolidierung",
            Self::DreamReflection => "Traum-Reflexion",
            Self::AutoClassifier => "Auto-Modus-Klassifizierer",
            Self::WorkDriverJudge => "work-driver-Bewerter",
        }
    }

    /// Parst einen Rollen-Schlüssel. Akzeptiert [`Self::key`] (Groß-/
    /// Kleinschreibung und `_`/`-` egal) sowie jeden Schlüssel einer
    /// [`InternalModelPoint`] (z. B. `session_title`, `root_orchestrator`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
        if normalized.is_empty() {
            return None;
        }
        if let Some(role) = Self::ALL.into_iter().find(|role| role.key() == normalized) {
            return Some(role);
        }
        let point = InternalModelPoint::parse(&normalized)?;
        Self::ALL
            .into_iter()
            .find(|role| role.internal_point() == Some(point))
    }

    /// Zugehörige interne Modellstelle; `None` für UIA und UIA-Worker (die
    /// über `uia_provider`/`uia_model`/`uia_worker_model` gepinnt werden).
    #[must_use]
    pub fn internal_point(self) -> Option<InternalModelPoint> {
        match self {
            Self::Uia | Self::UiaWorker => None,
            Self::Orchestrator => Some(InternalModelPoint::RootOrchestrator),
            Self::SubOrchestrator => Some(InternalModelPoint::SubOrchestrator),
            Self::WorkerSimple => Some(InternalModelPoint::WorkerSimple),
            Self::WorkerComplex => Some(InternalModelPoint::WorkerComplex),
            Self::Explorer => Some(InternalModelPoint::Explorer),
            Self::Research => Some(InternalModelPoint::Research),
            Self::CompactionSummary => Some(InternalModelPoint::CompactionSummary),
            Self::SessionTitle => Some(InternalModelPoint::SessionTitle),
            Self::MemoryConsolidation => Some(InternalModelPoint::MemoryConsolidation),
            Self::DreamReflection => Some(InternalModelPoint::DreamReflection),
            Self::AutoClassifier => Some(InternalModelPoint::AutoClassifier),
            Self::WorkDriverJudge => Some(InternalModelPoint::WorkDriverJudge),
        }
    }

    /// `true`, wenn eine Wahl dieser Rolle (`/models set|reset`) in der
    /// laufenden Sitzung sofort für **neu gestartete** Kind-Agenten gilt
    /// (Live-Modellwechsel: die Kind-Modellstellen des Wurzel-Baums). Alle
    /// übrigen Rollen werden beim Start einmal verdrahtet und wirken erst ab
    /// der nächsten Sitzung.
    #[must_use]
    pub fn applies_to_new_agents_live(self) -> bool {
        matches!(
            self,
            Self::Orchestrator
                | Self::SubOrchestrator
                | Self::WorkerSimple
                | Self::WorkerComplex
                | Self::Explorer
                | Self::Research
                | Self::MemoryConsolidation
        )
    }
}

/// Herkunft des aufgelösten Modells einer Rolle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoleModelSource {
    /// Explizit per `[internal_models.<stelle>]` gewählt.
    Explicit,
    /// OpenRouter-Nemotron-Standard der internen Stelle.
    OpenRouterDefault,
    /// UIA-Pin `uia_provider`/`uia_model`.
    UiaPin,
    /// UIA-Worker-Pin `uia_worker_model`.
    UiaWorkerPin,
    /// Übernimmt das Modell der UIA.
    InheritsUia,
    /// Übernimmt das Modell des Orchestrators.
    InheritsOrchestrator,
    /// Standardmodell `default_provider`/`default_model`.
    DefaultModel,
    /// Kein Modell konfiguriert.
    Unset,
}

impl RoleModelSource {
    /// Deutsches Anzeige-Label der Herkunft.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Explicit => "explizit gewählt",
            Self::OpenRouterDefault => "OpenRouter-Standard",
            Self::UiaPin => "UIA-Pin",
            Self::UiaWorkerPin => "UIA-Worker-Pin",
            Self::InheritsUia => "erbt von UIA",
            Self::InheritsOrchestrator => "erbt vom Orchestrator",
            Self::DefaultModel => "Standardmodell",
            Self::Unset => "nicht gesetzt",
        }
    }
}

/// Eine Zeile der Rollen-Modell-Tabelle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleModelRow {
    pub role: ModelRole,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub source: RoleModelSource,
    /// Reasoning-Effort-Label (`"low"`, `"high"`, …), falls bestimmbar.
    pub reasoning_effort: Option<String>,
}

/// Löst Provider, Modell, Herkunft und Effort für `role` auf (siehe
/// Modul-Dokumentation).
#[must_use]
pub fn resolve_role_model(config: &ResolvedConfig, role: ModelRole) -> RoleModelRow {
    let (provider, model, source) = resolve_provider_model(config, role);
    let reasoning_effort = resolve_effort(config, role, provider.as_deref(), model.as_deref());
    RoleModelRow {
        role,
        provider,
        model,
        source,
        reasoning_effort,
    }
}

/// Löst alle Rollen in der Reihenfolge von [`ModelRole::ALL`] auf.
#[must_use]
pub fn resolve_role_models(config: &ResolvedConfig) -> Vec<RoleModelRow> {
    ModelRole::ALL
        .into_iter()
        .map(|role| resolve_role_model(config, role))
        .collect()
}

/// R18 F2: Ersatzmodell des Auto-Modus-Klassifizierers.
///
/// # Beschreibung
/// Liefert das Klassifizierer-Modell eine leere Antwort (auch nach einem
/// zweiten Versuch), fragt der Auto-Modus einmal dieses Ersatzmodell, bevor
/// er fail-closed die Person fragt. Ersatz ist das konfigurierte
/// Hauptmodell (`default_provider`/`default_model`) — aber nur, wenn es sich
/// vom aufgelösten Klassifizierer-Modell ([`ModelRole::AutoClassifier`])
/// unterscheidet: dasselbe Modell ein drittes Mal zu fragen bringt nichts.
///
/// # Arguments
/// - `config` (`&ResolvedConfig`): die Konfiguration.
///
/// # Returns
/// `Some((provider, modell))` des Ersatzmodells, sonst `None` (kein
/// Hauptmodell gesetzt oder identisch mit dem Klassifizierer-Modell).
#[must_use]
pub fn resolve_auto_classifier_fallback(
    config: &ResolvedConfig,
) -> Option<(Option<String>, String)> {
    let (provider, model, _) = default_model(config);
    let model = model?;
    let primary = resolve_role_model(config, ModelRole::AutoClassifier);
    let same_model = primary.model.as_deref() == Some(model.as_str());
    let same_provider = primary.provider.is_none()
        || provider.is_none()
        || primary.provider.as_deref() == provider.as_deref();
    if same_model && same_provider {
        return None;
    }
    Some((provider, model))
}

type ProviderModelSource = (Option<String>, Option<String>, RoleModelSource);

fn resolve_provider_model(config: &ResolvedConfig, role: ModelRole) -> ProviderModelSource {
    match role {
        ModelRole::Uia => resolve_uia(config),
        // Runde 5, Teil G: die Zeile zeigt die Rolle `uia-worker`; eine feste
        // Wahl trägt ihren eigenen Provider (keine Kopplung an die UIA mehr).
        ModelRole::UiaWorker => {
            match crate::uia_worker_models::resolve_uia_worker_model(config, "uia-worker").choice {
                crate::uia_worker_models::UiaWorkerModelChoice::Fixed { provider, model } => {
                    (Some(provider), Some(model), RoleModelSource::UiaWorkerPin)
                }
                crate::uia_worker_models::UiaWorkerModelChoice::FollowUia => {
                    let (provider, uia_model, _) = resolve_uia(config);
                    (provider, uia_model, RoleModelSource::InheritsUia)
                }
            }
        }
        ModelRole::Orchestrator => {
            let resolved = resolve_internal_model(config, InternalModelPoint::RootOrchestrator);
            match resolved.source {
                InternalModelSource::Explicit => {
                    (resolved.provider, resolved.model, RoleModelSource::Explicit)
                }
                // Orchestratoren nutzen nie den OpenRouter-Standard; beide
                // übrigen Fälle bedeuten „Hauptmodell“.
                InternalModelSource::OpenRouterDefault | InternalModelSource::MainModel => {
                    default_model(config)
                }
            }
        }
        ModelRole::SubOrchestrator => {
            let resolved = resolve_internal_model(config, InternalModelPoint::SubOrchestrator);
            match resolved.source {
                InternalModelSource::Explicit => {
                    (resolved.provider, resolved.model, RoleModelSource::Explicit)
                }
                InternalModelSource::OpenRouterDefault | InternalModelSource::MainModel => {
                    let (provider, model, _) =
                        resolve_provider_model(config, ModelRole::Orchestrator);
                    (provider, model, RoleModelSource::InheritsOrchestrator)
                }
            }
        }
        ModelRole::WorkerSimple
        | ModelRole::WorkerComplex
        | ModelRole::Explorer
        | ModelRole::Research
        | ModelRole::CompactionSummary
        | ModelRole::SessionTitle
        | ModelRole::MemoryConsolidation
        | ModelRole::DreamReflection => {
            let Some(point) = role.internal_point() else {
                return default_model(config);
            };
            let resolved = resolve_internal_model(config, point);
            match resolved.source {
                InternalModelSource::Explicit => {
                    (resolved.provider, resolved.model, RoleModelSource::Explicit)
                }
                InternalModelSource::OpenRouterDefault => (
                    resolved.provider,
                    resolved.model,
                    RoleModelSource::OpenRouterDefault,
                ),
                InternalModelSource::MainModel => default_model(config),
            }
        }
        // Runde 5, Teil E: ohne explizite Wahl das schnelle Modell des
        // aktiven Providers (claude-haiku-4-5 bzw. das kleinste), sonst das
        // Hauptmodell.
        // R14: der work-driver-Bewerter folgt derselben Regel (DEC-002):
        // explizite Wahl, sonst das schnelle Modell des aktiven Providers.
        ModelRole::AutoClassifier | ModelRole::WorkDriverJudge => {
            let point = role
                .internal_point()
                .unwrap_or(InternalModelPoint::AutoClassifier);
            let resolved = resolve_internal_model(config, point);
            match resolved.source {
                InternalModelSource::Explicit => {
                    (resolved.provider, resolved.model, RoleModelSource::Explicit)
                }
                InternalModelSource::OpenRouterDefault | InternalModelSource::MainModel => {
                    match fast_model_for_active_provider(config) {
                        Some((provider, model)) => {
                            (provider, Some(model), RoleModelSource::DefaultModel)
                        }
                        None => default_model(config),
                    }
                }
            }
        }
    }
}

/// UIA: Pin nur mit vorhandenem, aktiviertem Provider und gesetztem Modell
/// (wie `harw-runtime` `resolve_uia_model`), sonst Standardmodell.
fn resolve_uia(config: &ResolvedConfig) -> ProviderModelSource {
    let pinned_provider = non_empty(config.harness.uia_provider.as_deref());
    let pinned_model = non_empty(config.harness.uia_model.as_deref());
    match (pinned_provider, pinned_model) {
        (Some(provider), Some(model)) if provider_is_usable(config, &provider) => {
            (Some(provider), Some(model), RoleModelSource::UiaPin)
        }
        _ => default_model(config),
    }
}

/// Hauptmodell `default_provider`/`default_model`; ohne `default_model`
/// [`RoleModelSource::Unset`].
fn default_model(config: &ResolvedConfig) -> ProviderModelSource {
    let provider = non_empty(config.harness.default_provider.as_deref());
    match non_empty(config.harness.default_model.as_deref()) {
        Some(model) => (provider, Some(model), RoleModelSource::DefaultModel),
        None => (provider, None, RoleModelSource::Unset),
    }
}

fn provider_is_usable(config: &ResolvedConfig, provider: &str) -> bool {
    config
        .providers
        .get(provider)
        .is_some_and(|entry| entry.enabled)
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Effort: rollenspezifisches `[reasoning]`-Feld, sonst Modell-, sonst
/// Provider-`default_reasoning_effort`.
fn resolve_effort(
    config: &ResolvedConfig,
    role: ModelRole,
    provider: Option<&str>,
    model: Option<&str>,
) -> Option<String> {
    let weights = &config.harness.reasoning;
    let role_weight = match role {
        ModelRole::Uia => weights.uia.as_deref(),
        ModelRole::Orchestrator => weights.root_orchestrator.as_deref(),
        ModelRole::SubOrchestrator => weights.sub_orchestrator.as_deref(),
        ModelRole::WorkerSimple => weights.worker_simple.as_deref(),
        ModelRole::WorkerComplex => weights.worker_complex.as_deref(),
        ModelRole::UiaWorker
        | ModelRole::Explorer
        | ModelRole::Research
        | ModelRole::CompactionSummary
        | ModelRole::SessionTitle
        | ModelRole::MemoryConsolidation
        | ModelRole::DreamReflection
        | ModelRole::AutoClassifier
        | ModelRole::WorkDriverJudge => None,
    };
    if let Some(effort) = non_empty(role_weight) {
        return Some(effort);
    }
    if let Some(model_id) = model {
        let model_effort = config
            .models
            .values()
            .filter(|entry| provider.is_none_or(|p| entry.provider == p))
            .find(|entry| entry.id == model_id || entry.aliases.iter().any(|a| a == model_id))
            .and_then(|entry| entry.default_reasoning_effort);
        if let Some(effort) = model_effort {
            return Some(effort.to_string());
        }
    }
    provider
        .and_then(|p| config.providers.get(p))
        .and_then(|entry| entry.default_reasoning_effort)
        .map(|effort| effort.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_toml::SecretRef;
    use crate::internal_models::{InternalModelChoice, OPENROUTER_PROVIDER};
    use crate::model_toml::ModelToml;
    use crate::provider_toml::ProviderToml;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::collections::HashMap;
    use std::str::FromStr;

    fn provider(name: &str, enabled: bool) -> TestResult<ProviderToml> {
        Ok(ProviderToml {
            stream: None,
            name: name.to_owned(),
            api: "openai-chat".to_owned(),
            base_url: "https://example.invalid/v1".to_owned(),
            auth: Some(SecretRef::from_str("env:TEST_API_KEY").map_err(ctx("secret ref"))?),
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

    /// `default_provider = "main"`, `default_model = "main-model"`, Provider
    /// `main` und `pin` aktiviert.
    fn base_config() -> TestResult<ResolvedConfig> {
        let mut providers = HashMap::new();
        providers.insert("main".to_owned(), provider("main", true)?);
        providers.insert("pin".to_owned(), provider("pin", true)?);
        let mut config = ResolvedConfig {
            providers,
            ..Default::default()
        };
        config.harness.default_provider = Some("main".to_owned());
        config.harness.default_model = Some("main-model".to_owned());
        Ok(config)
    }

    fn with_openrouter(mut config: ResolvedConfig) -> TestResult<ResolvedConfig> {
        config.providers.insert(
            OPENROUTER_PROVIDER.to_owned(),
            provider(OPENROUTER_PROVIDER, true)?,
        );
        Ok(config)
    }

    fn explicit(provider: &str, model: &str) -> Option<InternalModelChoice> {
        Some(InternalModelChoice {
            provider: Some(provider.to_owned()),
            model: Some(model.to_owned()),
        })
    }

    #[test]
    fn test_all_roles_have_unique_keys_and_parse_roundtrip() -> TestResult {
        for role in ModelRole::ALL {
            let parsed = ModelRole::parse(role.key()).ok_or(TestError::Missing("rolle"))?;
            assert_eq!(parsed, role);
            assert!(!role.label().is_empty());
        }
        let mut keys: Vec<&str> = ModelRole::ALL.iter().map(|role| role.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ModelRole::ALL.len());
        assert_eq!(ModelRole::parse("UIA_Worker"), Some(ModelRole::UiaWorker));
        assert_eq!(
            ModelRole::parse("session_title"),
            Some(ModelRole::SessionTitle)
        );
        assert_eq!(
            ModelRole::parse("root_orchestrator"),
            Some(ModelRole::Orchestrator)
        );
        assert_eq!(ModelRole::parse(""), None);
        assert_eq!(ModelRole::parse("unbekannt"), None);
        Ok(())
    }

    #[test]
    fn test_internal_point_mapping() {
        assert_eq!(ModelRole::Uia.internal_point(), None);
        assert_eq!(ModelRole::UiaWorker.internal_point(), None);
        assert_eq!(
            ModelRole::Orchestrator.internal_point(),
            Some(InternalModelPoint::RootOrchestrator)
        );
        assert_eq!(
            ModelRole::SubOrchestrator.internal_point(),
            Some(InternalModelPoint::SubOrchestrator)
        );
        // Jede interne Stelle ist genau einer Rolle zugeordnet.
        for point in InternalModelPoint::ALL {
            let count = ModelRole::ALL
                .iter()
                .filter(|role| role.internal_point() == Some(point))
                .count();
            assert_eq!(count, 1, "{point:?}");
        }
    }

    #[test]
    fn test_uia_pin_with_enabled_provider() -> TestResult {
        let mut config = base_config()?;
        config.harness.uia_provider = Some("pin".to_owned());
        config.harness.uia_model = Some("pin-model".to_owned());
        let row = resolve_role_model(&config, ModelRole::Uia);
        assert_eq!(row.source, RoleModelSource::UiaPin);
        assert_eq!(row.provider.as_deref(), Some("pin"));
        assert_eq!(row.model.as_deref(), Some("pin-model"));
        Ok(())
    }

    #[test]
    fn test_uia_pin_with_disabled_or_unknown_provider_falls_back() -> TestResult {
        let mut config = base_config()?;
        config
            .providers
            .insert("off".to_owned(), provider("off", false)?);
        config.harness.uia_provider = Some("off".to_owned());
        config.harness.uia_model = Some("pin-model".to_owned());
        let row = resolve_role_model(&config, ModelRole::Uia);
        assert_eq!(row.source, RoleModelSource::DefaultModel);
        assert_eq!(row.provider.as_deref(), Some("main"));
        assert_eq!(row.model.as_deref(), Some("main-model"));

        config.harness.uia_provider = Some("gibt-es-nicht".to_owned());
        let row = resolve_role_model(&config, ModelRole::Uia);
        assert_eq!(row.source, RoleModelSource::DefaultModel);
        Ok(())
    }

    #[test]
    fn test_uia_unset_without_default_model() {
        let config = ResolvedConfig::default();
        let row = resolve_role_model(&config, ModelRole::Uia);
        assert_eq!(row.source, RoleModelSource::Unset);
        assert!(row.model.is_none());
    }

    /// Runde 5, Teil G: der alte Pin trägt den Provider aus dem Katalog,
    /// nicht mehr den UIA-Provider.
    #[test]
    fn test_uia_worker_pin_uses_catalog_provider() -> TestResult {
        let mut config = base_config()?;
        config.harness.uia_provider = Some("pin".to_owned());
        config.harness.uia_model = Some("pin-model".to_owned());
        config.harness.uia_worker_model = Some("worker-model".to_owned());
        config.models.insert(
            "worker-model".to_owned(),
            toml::from_str("id = \"worker-model\"\nprovider = \"main\"\n")
                .map_err(ctx("model toml"))?,
        );
        let row = resolve_role_model(&config, ModelRole::UiaWorker);
        assert_eq!(row.source, RoleModelSource::UiaWorkerPin);
        assert_eq!(row.provider.as_deref(), Some("main"));
        assert_eq!(row.model.as_deref(), Some("worker-model"));
        Ok(())
    }

    /// Runde 5, Teil G: eine eigene Rollenwahl mit anderem Provider als die
    /// UIA wird angezeigt; „uia“ zeigt wieder die UIA-Zeile.
    #[test]
    fn test_uia_worker_own_choice_is_independent_of_uia_provider() -> TestResult {
        let mut config = base_config()?;
        config.harness.uia_provider = Some("pin".to_owned());
        config.harness.uia_model = Some("pin-model".to_owned());
        config
            .harness
            .uia_worker_models
            .set("uia-worker", Some("main/fast-model".to_owned()));
        let row = resolve_role_model(&config, ModelRole::UiaWorker);
        assert_eq!(row.source, RoleModelSource::UiaWorkerPin);
        assert_eq!(row.provider.as_deref(), Some("main"));
        assert_eq!(row.model.as_deref(), Some("fast-model"));

        config
            .harness
            .uia_worker_models
            .set("uia-worker", Some("uia".to_owned()));
        let row = resolve_role_model(&config, ModelRole::UiaWorker);
        assert_eq!(row.source, RoleModelSource::InheritsUia);
        assert_eq!(row.provider.as_deref(), Some("pin"));
        assert_eq!(row.model.as_deref(), Some("pin-model"));
        Ok(())
    }

    #[test]
    fn test_uia_worker_inherits_uia() -> TestResult {
        let mut config = base_config()?;
        config.harness.uia_provider = Some("pin".to_owned());
        config.harness.uia_model = Some("pin-model".to_owned());
        let row = resolve_role_model(&config, ModelRole::UiaWorker);
        assert_eq!(row.source, RoleModelSource::InheritsUia);
        assert_eq!(row.provider.as_deref(), Some("pin"));
        assert_eq!(row.model.as_deref(), Some("pin-model"));
        Ok(())
    }

    #[test]
    fn test_orchestrator_default_model_even_with_openrouter() -> TestResult {
        let config = with_openrouter(base_config()?)?;
        let row = resolve_role_model(&config, ModelRole::Orchestrator);
        assert_eq!(row.source, RoleModelSource::DefaultModel);
        assert_eq!(row.provider.as_deref(), Some("main"));
        assert_eq!(row.model.as_deref(), Some("main-model"));
        Ok(())
    }

    #[test]
    fn test_orchestrator_explicit() -> TestResult {
        let mut config = base_config()?;
        config.harness.internal_models.set_choice(
            InternalModelPoint::RootOrchestrator,
            explicit("pin", "orch"),
        );
        let row = resolve_role_model(&config, ModelRole::Orchestrator);
        assert_eq!(row.source, RoleModelSource::Explicit);
        assert_eq!(row.provider.as_deref(), Some("pin"));
        assert_eq!(row.model.as_deref(), Some("orch"));
        Ok(())
    }

    #[test]
    fn test_sub_orchestrator_inherits_orchestrator() -> TestResult {
        let mut config = with_openrouter(base_config()?)?;
        config.harness.internal_models.set_choice(
            InternalModelPoint::RootOrchestrator,
            explicit("pin", "orch"),
        );
        let row = resolve_role_model(&config, ModelRole::SubOrchestrator);
        assert_eq!(row.source, RoleModelSource::InheritsOrchestrator);
        assert_eq!(row.provider.as_deref(), Some("pin"));
        assert_eq!(row.model.as_deref(), Some("orch"));
        Ok(())
    }

    #[test]
    fn test_sub_orchestrator_explicit() -> TestResult {
        let mut config = base_config()?;
        config
            .harness
            .internal_models
            .set_choice(InternalModelPoint::SubOrchestrator, explicit("pin", "sub"));
        let row = resolve_role_model(&config, ModelRole::SubOrchestrator);
        assert_eq!(row.source, RoleModelSource::Explicit);
        assert_eq!(row.model.as_deref(), Some("sub"));
        Ok(())
    }

    #[test]
    fn test_other_roles_map_internal_sources() -> TestResult {
        // Ohne OpenRouter: Hauptmodell → DefaultModel.
        let config = base_config()?;
        let row = resolve_role_model(&config, ModelRole::Explorer);
        assert_eq!(row.source, RoleModelSource::DefaultModel);
        assert_eq!(row.model.as_deref(), Some("main-model"));

        // Mit OpenRouter: OpenRouter-Standard.
        let mut config = with_openrouter(base_config()?)?;
        let row = resolve_role_model(&config, ModelRole::SessionTitle);
        assert_eq!(row.source, RoleModelSource::OpenRouterDefault);
        assert_eq!(row.provider.as_deref(), Some(OPENROUTER_PROVIDER));

        // Explizite Wahl gewinnt.
        config
            .harness
            .internal_models
            .set_choice(InternalModelPoint::Research, explicit("pin", "res"));
        let row = resolve_role_model(&config, ModelRole::Research);
        assert_eq!(row.source, RoleModelSource::Explicit);
        assert_eq!(row.model.as_deref(), Some("res"));
        Ok(())
    }

    #[test]
    fn test_other_role_unset_without_default_model() {
        let config = ResolvedConfig::default();
        let row = resolve_role_model(&config, ModelRole::MemoryConsolidation);
        assert_eq!(row.source, RoleModelSource::Unset);
    }

    #[test]
    fn test_effort_role_weight_then_model_then_provider() -> TestResult {
        let mut config = base_config()?;
        config.harness.reasoning.uia = Some("xhigh".to_owned());
        assert_eq!(
            resolve_role_model(&config, ModelRole::Uia)
                .reasoning_effort
                .as_deref(),
            Some("xhigh")
        );

        // Kein Rollenfeld: Modell-Default.
        let model: ModelToml = toml::from_str(
            r#"
                id = "main-model"
                provider = "main"
                default_reasoning_effort = "low"
            "#,
        )
        .map_err(ctx("model-toml parsen"))?;
        config.models.insert("main-model".to_owned(), model);
        assert_eq!(
            resolve_role_model(&config, ModelRole::Explorer)
                .reasoning_effort
                .as_deref(),
            Some("low")
        );

        // Kein Modell-Default: Provider-Default.
        config.models.clear();
        let main = config
            .providers
            .get_mut("main")
            .ok_or(TestError::Missing("provider main"))?;
        main.default_reasoning_effort = Some(harw_types::ReasoningEffort::High);
        assert_eq!(
            resolve_role_model(&config, ModelRole::Explorer)
                .reasoning_effort
                .as_deref(),
            Some("high")
        );
        Ok(())
    }

    #[test]
    fn test_resolve_role_models_covers_all_roles_in_order() -> TestResult {
        let config = base_config()?;
        let rows = resolve_role_models(&config);
        let roles: Vec<ModelRole> = rows.iter().map(|row| row.role).collect();
        assert_eq!(roles, ModelRole::ALL.to_vec());
        Ok(())
    }

    /// Runde 5, Teil E: `auto-classifier` bekommt ohne Wahl das schnelle
    /// Modell des aktiven Providers, mit Wahl die explizite.
    #[test]
    fn test_auto_classifier_role_resolves_fast_model_or_explicit_choice() -> TestResult {
        let mut config = base_config()?;
        config.harness.default_provider = Some("anthropic".to_owned());
        let row = resolve_role_model(&config, ModelRole::AutoClassifier);
        assert_eq!(row.model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(
            ModelRole::parse("auto-classifier"),
            Some(ModelRole::AutoClassifier)
        );

        config
            .harness
            .internal_models
            .set_choice(InternalModelPoint::AutoClassifier, explicit("pin", "tiny"));
        let row = resolve_role_model(&config, ModelRole::AutoClassifier);
        assert_eq!(row.source, RoleModelSource::Explicit);
        assert_eq!(row.model.as_deref(), Some("tiny"));
        Ok(())
    }

    /// R18 F2 (EX-01): das Ersatzmodell des Klassifizierers ist das
    /// Hauptmodell, aber nie dasselbe Modell wie der Klassifizierer selbst.
    #[test]
    fn test_auto_classifier_fallback_is_the_main_model_when_distinct() -> TestResult {
        let mut config = base_config()?;
        config.harness.default_provider = Some("anthropic".to_owned());
        // Klassifizierer: claude-haiku-4-5, Hauptmodell: main-model.
        assert_eq!(
            resolve_auto_classifier_fallback(&config),
            Some((Some("anthropic".to_owned()), "main-model".to_owned()))
        );

        // Explizite Klassifizierer-Wahl = Hauptmodell → kein Ersatz.
        config.harness.internal_models.set_choice(
            InternalModelPoint::AutoClassifier,
            explicit("anthropic", "main-model"),
        );
        assert_eq!(resolve_auto_classifier_fallback(&config), None);

        // Ohne Hauptmodell gibt es keinen Ersatz.
        let config = ResolvedConfig::default();
        assert_eq!(resolve_auto_classifier_fallback(&config), None);
        Ok(())
    }

    #[test]
    fn test_source_labels_are_german_and_distinct() {
        let sources = [
            RoleModelSource::Explicit,
            RoleModelSource::OpenRouterDefault,
            RoleModelSource::UiaPin,
            RoleModelSource::UiaWorkerPin,
            RoleModelSource::InheritsUia,
            RoleModelSource::InheritsOrchestrator,
            RoleModelSource::DefaultModel,
            RoleModelSource::Unset,
        ];
        let mut labels: Vec<&str> = sources.iter().map(|source| source.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), sources.len());
    }
}
