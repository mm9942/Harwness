//! Interne Modellstellen: pro Stelle wählbares Modell mit
//! OpenRouter/Nemotron-Standard.
//!
//! Nutzerregel (Addendum C, `CONTRACT.md`):
//! 1. Jede interne Stelle, die ein eigenes Modell nutzen kann (Sitzungstitel,
//!    Verdichtungs-Zusammenfassung, Gedächtnis-Konsolidierung,
//!    Traum-Reflexion, Explorer, Recherche), ist einzeln konfigurierbar über
//!    `[internal_models.<stelle>]` in der Harness-Config.
//! 2. Standard: Ist ein Provider `openrouter` konfiguriert (aktiviert +
//!    Auth vorhanden) und `use_openrouter_defaults = true` (Standard), nutzt
//!    die Stelle ihr NVIDIA-Nemotron-Standardmodell über OpenRouter.
//! 3. Sonst gilt unverändert das normale Modell der Session/des Nutzers
//!    (`source = MainModel`, `provider`/`model` bleiben `None` — der
//!    Aufrufer verwendet dann sein bereits aktives Modell).
//! 4. Eine explizite Wahl gewinnt immer, auch eine bewusst leere
//!    (`provider`/`model` beide `None`), die den Nutzer zwingend auf das
//!    Hauptmodell zurückfallen lässt. Onboarding empfiehlt OpenRouter beim
//!    Einrichten des ersten Providers.
//!
//! Auflösungsreihenfolge (siehe [`resolve_internal_model`]):
//! (a) `internal_models.<point>` mit gesetztem `model` → [`InternalModelSource::Explicit`];
//! (b) ein Eintrag mit `model = None` (auch bei gesetztem `provider`) erzwingt
//!     das Hauptmodell → [`InternalModelSource::MainModel`];
//! (c) nur für [`InternalModelPoint::SessionTitle`]: fehlt jeder Eintrag, aber
//!     das Legacy-Feld `session.title_model` ist gesetzt → `Explicit`;
//! (d) nur für Stellen mit [`InternalModelPoint::uses_openrouter_default`]:
//!     `use_openrouter_defaults && `[`openrouter_available`] → `OpenRouterDefault`
//!     (Provider `"openrouter"`, Modell aus [`InternalModelPoint::openrouter_default_model`]);
//! (e) sonst `MainModel` mit `provider`/`model` beide `None`.

use serde::{Deserialize, Serialize};

/// Interne Stelle, an der die Harness ein eigenes (Hilfs-)Modell einsetzen
/// kann statt des Hauptmodells der Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InternalModelPoint {
    /// Erzeugung des Sitzungstitels nach der ersten abgeschlossenen Runde.
    SessionTitle,
    /// Zusammenfassung des Verlaufs beim Verdichten (Compaction).
    CompactionSummary,
    /// Konsolidierung von Fakten/Lektionen beim Sitzungsende.
    MemoryConsolidation,
    /// Hintergrund-Reflexion über vergangene Sitzungen.
    DreamReflection,
    /// Code-/Projektstruktur-Erkundung zur Kontext-Anreicherung.
    Explorer,
    /// Recherche externer Quellen (Web/Doku) im Auftrag der Hauptsitzung.
    Research,
    /// Worker-Kind mit einfacher Aufgabe (Addendum D+E: `TaskComplexity::Simple`).
    WorkerSimple,
    /// Worker-Kind mit komplexer Aufgabe, auch wenn klein (Addendum D+E:
    /// `TaskComplexity::Complex` oder unbekannt).
    WorkerComplex,
    /// Wurzel-Orchestrator (oberste Orchestrator-Rolle). Ohne explizite Wahl
    /// gilt das Hauptmodell — kein OpenRouter-Standard.
    RootOrchestrator,
    /// Unter-Orchestrator (von einem Orchestrator gestarteter Orchestrator).
    /// Ohne explizite Wahl gilt das Hauptmodell — kein OpenRouter-Standard.
    SubOrchestrator,
    /// Runde 5, Teil E: Klassifizierer des Auto-Modus (Rolle
    /// `auto-classifier`). Ohne explizite Wahl kein OpenRouter-Standard,
    /// sondern das schnelle Modell des aktiven Providers
    /// ([`fast_model_for_active_provider`]).
    AutoClassifier,
}

impl InternalModelPoint {
    /// Alle Stellen in stabiler Reihenfolge (u. a. für Iteration/Merge).
    pub const ALL: [InternalModelPoint; 11] = [
        InternalModelPoint::SessionTitle,
        InternalModelPoint::CompactionSummary,
        InternalModelPoint::MemoryConsolidation,
        InternalModelPoint::DreamReflection,
        InternalModelPoint::Explorer,
        InternalModelPoint::Research,
        InternalModelPoint::WorkerSimple,
        InternalModelPoint::WorkerComplex,
        InternalModelPoint::RootOrchestrator,
        InternalModelPoint::SubOrchestrator,
        InternalModelPoint::AutoClassifier,
    ];

    /// TOML-/Config-Schlüssel dieser Stelle, z. B. `"session_title"`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::SessionTitle => "session_title",
            Self::CompactionSummary => "compaction_summary",
            Self::MemoryConsolidation => "memory_consolidation",
            Self::DreamReflection => "dream_reflection",
            Self::Explorer => "explorer",
            Self::Research => "research",
            Self::WorkerSimple => "worker_simple",
            Self::WorkerComplex => "worker_complex",
            Self::RootOrchestrator => "root_orchestrator",
            Self::SubOrchestrator => "sub_orchestrator",
            Self::AutoClassifier => "auto_classifier",
        }
    }

    /// Parst einen Stellen-Schlüssel. Akzeptiert sowohl `snake_case`
    /// (`"session_title"`) als auch die Variante mit Bindestrichen
    /// (`"session-title"`, `"dream-reflection"`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().replace('-', "_");
        Self::ALL
            .into_iter()
            .find(|point| point.key() == normalized)
    }

    /// Einzeiliges deutsches Beschreibungslabel dieser Stelle.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::SessionTitle => {
                "Erzeugt den Sitzungstitel nach der ersten abgeschlossenen Runde."
            }
            Self::CompactionSummary => {
                "Fasst den Gesprächsverlauf beim Verdichten (Compaction) zusammen."
            }
            Self::MemoryConsolidation => {
                "Konsolidiert Fakten und Lektionen beim Abschluss der Sitzung."
            }
            Self::DreamReflection => "Reflektiert im Hintergrund über vergangene Sitzungen.",
            Self::Explorer => "Erkundet Code und Projektstruktur zur Kontext-Anreicherung.",
            Self::Research => "Recherchiert externe Quellen (Web/Dokumentation).",
            Self::WorkerSimple => "Worker-Kind mit einfacher Aufgabe.",
            Self::WorkerComplex => "Worker-Kind mit komplexer Aufgabe (auch wenn klein).",
            Self::RootOrchestrator => "Wurzel-Orchestrator, der die Arbeit plant und verteilt.",
            Self::SubOrchestrator => "Unter-Orchestrator, der Teilaufgaben weiter verteilt.",
            Self::AutoClassifier => {
                "Beurteilt im Auto-Modus Werkzeugaufrufe (erlauben, fragen, ablehnen)."
            }
        }
    }

    /// `true`, wenn diese Stelle ohne explizite Wahl auf den
    /// OpenRouter-Nemotron-Standard fallen darf (Schritt (d) der Auflösung).
    ///
    /// Für die Orchestrator-Stellen `false`: Orchestratoren nutzen ohne
    /// explizite Wahl immer das Hauptmodell.
    #[must_use]
    pub fn uses_openrouter_default(self) -> bool {
        !matches!(
            self,
            Self::RootOrchestrator | Self::SubOrchestrator | Self::AutoClassifier
        )
    }

    /// NVIDIA-Nemotron-Standardmodell dieser Stelle über OpenRouter.
    ///
    /// Live verifiziert (OpenRouter `/api/v1/models`, 2026-09-15):
    /// `nvidia/nemotron-3-super-120b-a12b` für rechercheintensive Stellen
    /// (Explorer, Research), sonst das leichtere
    /// `nvidia/nemotron-3.5-lightning`. Die Orchestrator-Stellen nutzen den
    /// Standard nie (siehe [`Self::uses_openrouter_default`]); für sie liefert
    /// diese Funktion der Vollständigkeit halber das Modell von
    /// [`Self::WorkerComplex`].
    #[must_use]
    pub fn openrouter_default_model(self) -> &'static str {
        match self {
            Self::Explorer
            | Self::Research
            | Self::WorkerComplex
            | Self::RootOrchestrator
            | Self::SubOrchestrator => "nvidia/nemotron-3-super-120b-a12b",
            Self::SessionTitle
            | Self::CompactionSummary
            | Self::MemoryConsolidation
            | Self::DreamReflection
            | Self::WorkerSimple
            | Self::AutoClassifier => "nvidia/nemotron-3.5-lightning",
        }
    }
}

/// Provider-Name des in der Nutzerregel referenzierten OpenRouter-Providers.
pub const OPENROUTER_PROVIDER: &str = "openrouter";

/// Explizite Provider-/Modell-Wahl für eine [`InternalModelPoint`].
///
/// Beide Felder `None` bedeutet eine bewusst leere Wahl: die Stelle wird auf
/// das Hauptmodell gezwungen, unabhängig von `use_openrouter_defaults`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InternalModelChoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Default für [`InternalModelsToml::use_openrouter_defaults`].
fn default_true() -> bool {
    true
}

/// `[internal_models]` — Konfiguration der wählbaren internen Modellstellen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InternalModelsToml {
    /// Standard: `true`. Bei `false` nutzen alle Stellen ohne explizite Wahl
    /// stets das Hauptmodell, auch wenn OpenRouter verfügbar ist.
    #[serde(default = "default_true")]
    pub use_openrouter_defaults: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<InternalModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_summary: Option<InternalModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_consolidation: Option<InternalModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dream_reflection: Option<InternalModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explorer: Option<InternalModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research: Option<InternalModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_simple: Option<InternalModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_complex: Option<InternalModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_orchestrator: Option<InternalModelChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_orchestrator: Option<InternalModelChoice>,
    /// Runde 5, Teil E: `[internal_models.auto_classifier]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_classifier: Option<InternalModelChoice>,
}

impl Default for InternalModelsToml {
    fn default() -> Self {
        Self {
            use_openrouter_defaults: true,
            session_title: None,
            compaction_summary: None,
            memory_consolidation: None,
            dream_reflection: None,
            explorer: None,
            research: None,
            worker_simple: None,
            worker_complex: None,
            root_orchestrator: None,
            sub_orchestrator: None,
            auto_classifier: None,
        }
    }
}

impl InternalModelsToml {
    /// Liefert die konfigurierte Wahl für `point`, falls gesetzt.
    #[must_use]
    pub fn choice(&self, point: InternalModelPoint) -> Option<&InternalModelChoice> {
        match point {
            InternalModelPoint::SessionTitle => self.session_title.as_ref(),
            InternalModelPoint::CompactionSummary => self.compaction_summary.as_ref(),
            InternalModelPoint::MemoryConsolidation => self.memory_consolidation.as_ref(),
            InternalModelPoint::DreamReflection => self.dream_reflection.as_ref(),
            InternalModelPoint::Explorer => self.explorer.as_ref(),
            InternalModelPoint::Research => self.research.as_ref(),
            InternalModelPoint::WorkerSimple => self.worker_simple.as_ref(),
            InternalModelPoint::WorkerComplex => self.worker_complex.as_ref(),
            InternalModelPoint::RootOrchestrator => self.root_orchestrator.as_ref(),
            InternalModelPoint::SubOrchestrator => self.sub_orchestrator.as_ref(),
            InternalModelPoint::AutoClassifier => self.auto_classifier.as_ref(),
        }
    }

    /// Setzt (oder entfernt, bei `None`) die Wahl für `point`.
    pub fn set_choice(&mut self, point: InternalModelPoint, choice: Option<InternalModelChoice>) {
        match point {
            InternalModelPoint::SessionTitle => self.session_title = choice,
            InternalModelPoint::CompactionSummary => self.compaction_summary = choice,
            InternalModelPoint::MemoryConsolidation => self.memory_consolidation = choice,
            InternalModelPoint::DreamReflection => self.dream_reflection = choice,
            InternalModelPoint::Explorer => self.explorer = choice,
            InternalModelPoint::Research => self.research = choice,
            InternalModelPoint::WorkerSimple => self.worker_simple = choice,
            InternalModelPoint::WorkerComplex => self.worker_complex = choice,
            InternalModelPoint::RootOrchestrator => self.root_orchestrator = choice,
            InternalModelPoint::SubOrchestrator => self.sub_orchestrator = choice,
            InternalModelPoint::AutoClassifier => self.auto_classifier = choice,
        }
    }
}

/// Herkunft der aufgelösten Provider-/Modell-Wahl einer Stelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalModelSource {
    /// Explizit vom Nutzer gewählt (per-Stelle-Eintrag oder Legacy
    /// `title_model`).
    Explicit,
    /// OpenRouter-Nemotron-Standard, weil kein expliziter Eintrag vorliegt
    /// und die Voraussetzungen der Nutzerregel erfüllt sind.
    OpenRouterDefault,
    /// Kein Sonderfall: die Stelle nutzt das normale Modell der
    /// Session/des Nutzers.
    MainModel,
}

/// Ergebnis der Auflösung einer [`InternalModelPoint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInternalModel {
    pub point: InternalModelPoint,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub source: InternalModelSource,
}

impl ResolvedInternalModel {
    /// `true`, wenn diese Stelle keine eigene Provider-/Modell-Wahl trägt und
    /// der Aufrufer folglich das aktive Hauptmodell der Session nutzen muss.
    #[must_use]
    pub fn is_main_model(&self) -> bool {
        matches!(self.source, InternalModelSource::MainModel)
    }
}

/// `true`, wenn ein Provider namens [`OPENROUTER_PROVIDER`] konfiguriert,
/// aktiviert ist und Auth (SecretRef oder — veraltet — Klartext-`api_key`)
/// vorhanden ist.
///
/// Auth-Erkennung folgt derselben Regel wie
/// `ProviderToml::has_plaintext_secret` (siehe `harw-ops/src/provider.rs`
/// `configured_auth_is_present`): `provider.auth.is_some() ||
/// provider.has_plaintext_secret()`.
#[must_use]
pub fn openrouter_available(config: &crate::ResolvedConfig) -> bool {
    config
        .providers
        .get(OPENROUTER_PROVIDER)
        .is_some_and(|provider| {
            provider.enabled && (provider.auth.is_some() || provider.has_plaintext_secret())
        })
}

/// Löst die Provider-/Modell-Wahl für `point` gemäß der Nutzerregel auf
/// (siehe Modul-Dokumentation für die Reihenfolge (a)–(e)).
#[must_use]
pub fn resolve_internal_model(
    config: &crate::ResolvedConfig,
    point: InternalModelPoint,
) -> ResolvedInternalModel {
    let internal_models = &config.harness.internal_models;

    if let Some(choice) = internal_models.choice(point) {
        if let Some(model) = choice.model.clone() {
            let provider = choice
                .provider
                .clone()
                .or_else(|| config.harness.default_provider.clone());
            return ResolvedInternalModel {
                point,
                provider,
                model: Some(model),
                source: InternalModelSource::Explicit,
            };
        }
        // Ein Eintrag ohne Modell erzwingt das Hauptmodell (Nutzerwille).
        return ResolvedInternalModel {
            point,
            provider: None,
            model: None,
            source: InternalModelSource::MainModel,
        };
    }

    if point == InternalModelPoint::SessionTitle {
        if let Some(model) = config.harness.session.title_model.clone() {
            return ResolvedInternalModel {
                point,
                provider: config.harness.default_provider.clone(),
                model: Some(model),
                source: InternalModelSource::Explicit,
            };
        }
    }

    if point.uses_openrouter_default()
        && internal_models.use_openrouter_defaults
        && openrouter_available(config)
    {
        return ResolvedInternalModel {
            point,
            provider: Some(OPENROUTER_PROVIDER.to_owned()),
            model: Some(point.openrouter_default_model().to_owned()),
            source: InternalModelSource::OpenRouterDefault,
        };
    }

    ResolvedInternalModel {
        point,
        provider: None,
        model: None,
        source: InternalModelSource::MainModel,
    }
}

/// Runde 5, Teil E: schnelles Modell für Anthropic-Provider (Klassifizierer
/// des Auto-Modus ohne explizite Wahl).
pub const ANTHROPIC_FAST_MODEL: &str = "claude-haiku-4-5";

/// Namensbestandteile, die ein Modell als klein/schnell ausweisen, in
/// absteigender Bevorzugung.
const FAST_MODEL_HINTS: &[&str] = &[
    "haiku",
    "nano",
    "mini",
    "flash-lite",
    "lite",
    "flash",
    "small",
    "instant",
    "8b",
];

/// Runde 5, Teil E: das schnelle Standardmodell des aktiven Providers.
///
/// # Beschreibung
/// Vorgabe für [`InternalModelPoint::AutoClassifier`], wenn keine explizite
/// Wahl vorliegt. Der aktive Provider ist `default_provider`, sonst der
/// Provider des `default_model`-Eintrags in `[models]`.
/// - Provider mit `api = "anthropic-messages"` (oder Name `anthropic`) →
///   [`ANTHROPIC_FAST_MODEL`].
/// - Sonst das erste bekannte Modell dieses Providers (aus `[models]` und
///   der Modellliste des Providers), dessen Id einen der Hinweise aus
///   `FAST_MODEL_HINTS` trägt (in deren Reihenfolge), sonst das mit dem
///   kleinsten bekannten `max_tokens`/`context_window`.
///
/// # Arguments
/// - `config` (`&crate::ResolvedConfig`): die aufgelöste Konfiguration.
///
/// # Returns
/// `Some((provider, model))`, oder `None`, wenn kein Provider/Modell
/// bestimmbar ist — der Aufrufer nutzt dann das Hauptmodell.
#[must_use]
pub fn fast_model_for_active_provider(
    config: &crate::ResolvedConfig,
) -> Option<(Option<String>, String)> {
    let default_model = config.harness.default_model.clone();
    let provider_name = config
        .harness
        .default_provider
        .clone()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            default_model
                .as_deref()
                .and_then(|model| config.models.get(model))
                .map(|entry| entry.provider.clone())
        })?;
    let provider = config.providers.get(&provider_name);
    let is_anthropic = provider_name.eq_ignore_ascii_case("anthropic")
        || provider.is_some_and(|entry| entry.api.starts_with("anthropic"));
    if is_anthropic {
        return Some((Some(provider_name), ANTHROPIC_FAST_MODEL.to_owned()));
    }

    let mut candidates: Vec<(String, Option<u64>)> = config
        .models
        .values()
        .filter(|entry| entry.provider == provider_name)
        .map(|entry| (entry.id.clone(), entry.max_tokens.or(entry.context_window)))
        .collect();
    if let Some(entry) = provider {
        for id in &entry.models {
            if !candidates.iter().any(|(known, _)| known == id) {
                candidates.push((id.clone(), None));
            }
        }
    }
    candidates.sort();

    for hint in FAST_MODEL_HINTS {
        if let Some((id, _)) = candidates
            .iter()
            .find(|(id, _)| id.to_ascii_lowercase().contains(hint))
        {
            return Some((Some(provider_name), id.clone()));
        }
    }
    candidates
        .iter()
        .filter_map(|(id, size)| size.map(|size| (size, id)))
        .min()
        .map(|(_, id)| (Some(provider_name), id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResolvedConfig;
    use crate::auth_toml::SecretRef;
    use crate::harness_config::HarnessConfig;
    use crate::provider_toml::ProviderToml;
    use crate::test_support::{TestResult, ctx};
    use std::collections::HashMap;
    use std::str::FromStr;

    fn openrouter_provider(enabled: bool, with_auth: bool) -> TestResult<ProviderToml> {
        Ok(ProviderToml {
            stream: None,
            name: OPENROUTER_PROVIDER.to_owned(),
            api: "openrouter-chat".to_owned(),
            base_url: "https://openrouter.ai/api/v1".to_owned(),
            auth: if with_auth {
                Some(
                    SecretRef::from_str("env:OPENROUTER_API_KEY")
                        .map_err(ctx("valid secret ref"))?,
                )
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

    fn config_with_openrouter(enabled: bool, with_auth: bool) -> TestResult<ResolvedConfig> {
        let mut providers = HashMap::new();
        providers.insert(
            OPENROUTER_PROVIDER.to_owned(),
            openrouter_provider(enabled, with_auth)?,
        );
        Ok(ResolvedConfig {
            providers,
            ..Default::default()
        })
    }

    #[test]
    fn test_explicit_choice_wins_over_openrouter_default() -> TestResult {
        let mut config = config_with_openrouter(true, true)?;
        config.harness.internal_models.set_choice(
            InternalModelPoint::Research,
            Some(InternalModelChoice {
                provider: Some("anthropic".to_owned()),
                model: Some("claude-haiku".to_owned()),
            }),
        );

        let resolved = resolve_internal_model(&config, InternalModelPoint::Research);
        assert_eq!(resolved.source, InternalModelSource::Explicit);
        assert_eq!(resolved.provider.as_deref(), Some("anthropic"));
        assert_eq!(resolved.model.as_deref(), Some("claude-haiku"));
        Ok(())
    }

    #[test]
    fn test_empty_choice_forces_main_model() -> TestResult {
        let mut config = config_with_openrouter(true, true)?;
        config.harness.internal_models.set_choice(
            InternalModelPoint::CompactionSummary,
            Some(InternalModelChoice::default()),
        );

        let resolved = resolve_internal_model(&config, InternalModelPoint::CompactionSummary);
        assert_eq!(resolved.source, InternalModelSource::MainModel);
        assert!(resolved.is_main_model());
        assert!(resolved.provider.is_none());
        assert!(resolved.model.is_none());
        Ok(())
    }

    #[test]
    fn test_openrouter_default_used_when_available() -> TestResult {
        let config = config_with_openrouter(true, true)?;
        let resolved = resolve_internal_model(&config, InternalModelPoint::Explorer);
        assert_eq!(resolved.source, InternalModelSource::OpenRouterDefault);
        assert_eq!(resolved.provider.as_deref(), Some(OPENROUTER_PROVIDER));
        assert_eq!(
            resolved.model.as_deref(),
            Some("nvidia/nemotron-3-super-120b-a12b")
        );
        Ok(())
    }

    #[test]
    fn test_openrouter_disabled_falls_back_to_main_model() -> TestResult {
        let config = config_with_openrouter(false, true)?;
        let resolved = resolve_internal_model(&config, InternalModelPoint::SessionTitle);
        assert_eq!(resolved.source, InternalModelSource::MainModel);
        assert!(resolved.is_main_model());
        Ok(())
    }

    #[test]
    fn test_openrouter_without_auth_falls_back_to_main_model() -> TestResult {
        let config = config_with_openrouter(true, false)?;
        assert!(!openrouter_available(&config));
        let resolved = resolve_internal_model(&config, InternalModelPoint::MemoryConsolidation);
        assert_eq!(resolved.source, InternalModelSource::MainModel);
        Ok(())
    }

    #[test]
    fn test_use_openrouter_defaults_false_forces_main_model() -> TestResult {
        let mut config = config_with_openrouter(true, true)?;
        config.harness.internal_models.use_openrouter_defaults = false;
        let resolved = resolve_internal_model(&config, InternalModelPoint::DreamReflection);
        assert_eq!(resolved.source, InternalModelSource::MainModel);
        Ok(())
    }

    #[test]
    fn test_legacy_title_model_resolves_as_explicit() {
        let mut config = ResolvedConfig::default();
        config.harness.default_provider = Some("anthropic".to_owned());
        config.harness.session.title_model = Some("claude-haiku".to_owned());

        let resolved = resolve_internal_model(&config, InternalModelPoint::SessionTitle);
        assert_eq!(resolved.source, InternalModelSource::Explicit);
        assert_eq!(resolved.provider.as_deref(), Some("anthropic"));
        assert_eq!(resolved.model.as_deref(), Some("claude-haiku"));
    }

    #[test]
    fn test_legacy_title_model_ignored_for_other_points() {
        let mut config = ResolvedConfig::default();
        config.harness.session.title_model = Some("claude-haiku".to_owned());
        // Kein OpenRouter konfiguriert -> MainModel, title_model gilt nur
        // für SessionTitle.
        let resolved = resolve_internal_model(&config, InternalModelPoint::Research);
        assert_eq!(resolved.source, InternalModelSource::MainModel);
    }

    #[test]
    fn test_orchestrator_points_use_main_model_even_with_openrouter() -> TestResult {
        let config = config_with_openrouter(true, true)?;
        assert!(openrouter_available(&config));
        for point in [
            InternalModelPoint::RootOrchestrator,
            InternalModelPoint::SubOrchestrator,
        ] {
            assert!(!point.uses_openrouter_default());
            let resolved = resolve_internal_model(&config, point);
            assert_eq!(resolved.source, InternalModelSource::MainModel);
            assert!(resolved.provider.is_none());
            assert!(resolved.model.is_none());
        }
        Ok(())
    }

    #[test]
    fn test_orchestrator_explicit_choice_wins() -> TestResult {
        let mut config = config_with_openrouter(true, true)?;
        config.harness.internal_models.set_choice(
            InternalModelPoint::SubOrchestrator,
            Some(InternalModelChoice {
                provider: Some("anthropic".to_owned()),
                model: Some("claude-sonnet".to_owned()),
            }),
        );
        let resolved = resolve_internal_model(&config, InternalModelPoint::SubOrchestrator);
        assert_eq!(resolved.source, InternalModelSource::Explicit);
        assert_eq!(resolved.provider.as_deref(), Some("anthropic"));
        assert_eq!(resolved.model.as_deref(), Some("claude-sonnet"));
        // Die andere Orchestrator-Stelle bleibt unberührt.
        let root = resolve_internal_model(&config, InternalModelPoint::RootOrchestrator);
        assert_eq!(root.source, InternalModelSource::MainModel);
        Ok(())
    }

    #[test]
    fn test_all_points_have_unique_keys_and_roundtrip() {
        // Runde 5, Teil E: +1 für `auto_classifier`.
        assert_eq!(InternalModelPoint::ALL.len(), 11);
        for point in InternalModelPoint::ALL {
            assert_eq!(InternalModelPoint::parse(point.key()), Some(point));
            let mut toml = InternalModelsToml::default();
            let choice = InternalModelChoice {
                provider: None,
                model: Some(point.key().to_owned()),
            };
            toml.set_choice(point, Some(choice.clone()));
            assert_eq!(toml.choice(point), Some(&choice));
        }
        assert_eq!(
            InternalModelPoint::parse("root-orchestrator"),
            Some(InternalModelPoint::RootOrchestrator)
        );
        assert_eq!(
            InternalModelPoint::parse("sub_orchestrator"),
            Some(InternalModelPoint::SubOrchestrator)
        );
    }

    #[test]
    fn test_parse_accepts_key_and_dashed_variant() {
        assert_eq!(
            InternalModelPoint::parse("dream_reflection"),
            Some(InternalModelPoint::DreamReflection)
        );
        assert_eq!(
            InternalModelPoint::parse("dream-reflection"),
            Some(InternalModelPoint::DreamReflection)
        );
        assert_eq!(InternalModelPoint::parse("unknown-point"), None);
    }

    /// Runde 5, Teil E: der Klassifizierer fällt nie auf den
    /// OpenRouter-Standard, sondern auf das schnelle Modell des aktiven
    /// Providers.
    #[test]
    fn test_auto_classifier_never_uses_openrouter_default() -> TestResult {
        let config = config_with_openrouter(true, true)?;
        let resolved = resolve_internal_model(&config, InternalModelPoint::AutoClassifier);
        assert_eq!(resolved.source, InternalModelSource::MainModel);
        assert_eq!(
            InternalModelPoint::parse("auto-classifier"),
            Some(InternalModelPoint::AutoClassifier)
        );
        Ok(())
    }

    #[test]
    fn test_fast_model_for_anthropic_is_haiku() {
        let mut config = ResolvedConfig::default();
        config.harness.default_provider = Some("anthropic".to_owned());
        assert_eq!(
            fast_model_for_active_provider(&config),
            Some((
                Some("anthropic".to_owned()),
                ANTHROPIC_FAST_MODEL.to_owned()
            ))
        );
    }

    #[test]
    fn test_fast_model_prefers_small_model_names_of_the_active_provider() -> TestResult {
        let mut config = config_with_openrouter(true, true)?;
        config.harness.default_provider = Some(OPENROUTER_PROVIDER.to_owned());
        if let Some(provider) = config.providers.get_mut(OPENROUTER_PROVIDER) {
            provider.models = vec!["vendor/big-model".to_owned(), "vendor/gpt-mini".to_owned()];
        }
        assert_eq!(
            fast_model_for_active_provider(&config),
            Some((
                Some(OPENROUTER_PROVIDER.to_owned()),
                "vendor/gpt-mini".to_owned()
            ))
        );
        Ok(())
    }

    #[test]
    fn test_fast_model_without_active_provider_is_none() {
        assert_eq!(
            fast_model_for_active_provider(&ResolvedConfig::default()),
            None
        );
    }

    #[test]
    fn test_internal_models_toml_default_has_no_choices() {
        let defaults = HarnessConfig::default().internal_models;
        assert!(defaults.use_openrouter_defaults);
        for point in InternalModelPoint::ALL {
            assert!(defaults.choice(point).is_none());
        }
    }

    #[test]
    fn test_internal_models_toml_parses_deny_unknown_fields() -> TestResult {
        let src = r#"
            use_openrouter_defaults = false

            [session_title]
            provider = "anthropic"
            model = "claude-haiku"
        "#;
        let parsed: InternalModelsToml =
            toml::from_str(src).map_err(ctx("valid internal_models table"))?;
        assert!(!parsed.use_openrouter_defaults);
        assert_eq!(
            parsed.choice(InternalModelPoint::SessionTitle),
            Some(&InternalModelChoice {
                provider: Some("anthropic".to_owned()),
                model: Some("claude-haiku".to_owned()),
            })
        );

        let bad = r#"
            use_openrouter_defualts = false
        "#;
        assert!(toml::from_str::<InternalModelsToml>(bad).is_err());
        Ok(())
    }
}
