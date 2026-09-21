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
//! (d) `use_openrouter_defaults && `[`openrouter_available`] → `OpenRouterDefault`
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
}

impl InternalModelPoint {
    /// Alle Stellen in stabiler Reihenfolge (u. a. für Iteration/Merge).
    pub const ALL: [InternalModelPoint; 8] = [
        InternalModelPoint::SessionTitle,
        InternalModelPoint::CompactionSummary,
        InternalModelPoint::MemoryConsolidation,
        InternalModelPoint::DreamReflection,
        InternalModelPoint::Explorer,
        InternalModelPoint::Research,
        InternalModelPoint::WorkerSimple,
        InternalModelPoint::WorkerComplex,
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
        }
    }

    /// Parst einen Stellen-Schlüssel. Akzeptiert sowohl `snake_case`
    /// (`"session_title"`) als auch die Variante mit Bindestrichen
    /// (`"session-title"`, `"dream-reflection"`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().replace('-', "_");
        Self::ALL.into_iter().find(|point| point.key() == normalized)
    }

    /// Einzeiliges deutsches Beschreibungslabel dieser Stelle.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::SessionTitle => "Erzeugt den Sitzungstitel nach der ersten abgeschlossenen Runde.",
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
        }
    }

    /// NVIDIA-Nemotron-Standardmodell dieser Stelle über OpenRouter.
    ///
    /// Live verifiziert (OpenRouter `/api/v1/models`, 2026-09-15):
    /// `nvidia/nemotron-3-super-120b-a12b` für rechercheintensive Stellen
    /// (Explorer, Research), sonst das leichtere
    /// `nvidia/nemotron-3.5-lightning`.
    #[must_use]
    pub fn openrouter_default_model(self) -> &'static str {
        match self {
            Self::Explorer | Self::Research | Self::WorkerComplex => {
                "nvidia/nemotron-3-super-120b-a12b"
            }
            Self::SessionTitle
            | Self::CompactionSummary
            | Self::MemoryConsolidation
            | Self::DreamReflection
            | Self::WorkerSimple => "nvidia/nemotron-3.5-lightning",
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
    config.providers.get(OPENROUTER_PROVIDER).is_some_and(|provider| {
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

    if internal_models.use_openrouter_defaults && openrouter_available(config) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_toml::SecretRef;
    use crate::harness_config::HarnessConfig;
    use crate::provider_toml::ProviderToml;
    use crate::ResolvedConfig;
    use std::collections::HashMap;
    use std::str::FromStr;

    fn openrouter_provider(enabled: bool, with_auth: bool) -> ProviderToml {
        ProviderToml {
            name: OPENROUTER_PROVIDER.to_owned(),
            api: "openrouter-chat".to_owned(),
            base_url: "https://openrouter.ai/api/v1".to_owned(),
            auth: if with_auth {
                Some(SecretRef::from_str("env:OPENROUTER_API_KEY").expect("valid secret ref"))
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
        }
    }

    fn config_with_openrouter(enabled: bool, with_auth: bool) -> ResolvedConfig {
        let mut providers = HashMap::new();
        providers.insert(
            OPENROUTER_PROVIDER.to_owned(),
            openrouter_provider(enabled, with_auth),
        );
        ResolvedConfig {
            providers,
            ..Default::default()
        }
    }

    #[test]
    fn test_explicit_choice_wins_over_openrouter_default() {
        let mut config = config_with_openrouter(true, true);
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
    }

    #[test]
    fn test_empty_choice_forces_main_model() {
        let mut config = config_with_openrouter(true, true);
        config.harness.internal_models.set_choice(
            InternalModelPoint::CompactionSummary,
            Some(InternalModelChoice::default()),
        );

        let resolved = resolve_internal_model(&config, InternalModelPoint::CompactionSummary);
        assert_eq!(resolved.source, InternalModelSource::MainModel);
        assert!(resolved.is_main_model());
        assert!(resolved.provider.is_none());
        assert!(resolved.model.is_none());
    }

    #[test]
    fn test_openrouter_default_used_when_available() {
        let config = config_with_openrouter(true, true);
        let resolved = resolve_internal_model(&config, InternalModelPoint::Explorer);
        assert_eq!(resolved.source, InternalModelSource::OpenRouterDefault);
        assert_eq!(resolved.provider.as_deref(), Some(OPENROUTER_PROVIDER));
        assert_eq!(
            resolved.model.as_deref(),
            Some("nvidia/nemotron-3-super-120b-a12b")
        );
    }

    #[test]
    fn test_openrouter_disabled_falls_back_to_main_model() {
        let config = config_with_openrouter(false, true);
        let resolved = resolve_internal_model(&config, InternalModelPoint::SessionTitle);
        assert_eq!(resolved.source, InternalModelSource::MainModel);
        assert!(resolved.is_main_model());
    }

    #[test]
    fn test_openrouter_without_auth_falls_back_to_main_model() {
        let config = config_with_openrouter(true, false);
        assert!(!openrouter_available(&config));
        let resolved = resolve_internal_model(&config, InternalModelPoint::MemoryConsolidation);
        assert_eq!(resolved.source, InternalModelSource::MainModel);
    }

    #[test]
    fn test_use_openrouter_defaults_false_forces_main_model() {
        let mut config = config_with_openrouter(true, true);
        config.harness.internal_models.use_openrouter_defaults = false;
        let resolved = resolve_internal_model(&config, InternalModelPoint::DreamReflection);
        assert_eq!(resolved.source, InternalModelSource::MainModel);
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

    #[test]
    fn test_internal_models_toml_default_has_no_choices() {
        let defaults = HarnessConfig::default().internal_models;
        assert!(defaults.use_openrouter_defaults);
        for point in InternalModelPoint::ALL {
            assert!(defaults.choice(point).is_none());
        }
    }

    #[test]
    fn test_internal_models_toml_parses_deny_unknown_fields() {
        let src = r#"
            use_openrouter_defaults = false

            [session_title]
            provider = "anthropic"
            model = "claude-haiku"
        "#;
        let parsed: InternalModelsToml = toml::from_str(src).expect("valid internal_models table");
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
    }
}
