//! Live-Modellwechsel in der laufenden Montage.
//!
//! # Ursache des alten Fehlers („nach dem Modellwechsel muss harw neu
//! starten")
//! `/model switch` schrieb die Wahl nur in den Sitzungs-Controller (die
//! Wurzelsitzung übernimmt sie an der nächsten Turn-Grenze) und als
//! Vorgabe in die Profil-Config. Alles, was die Montage beim Start daraus
//! abgeleitet hatte, blieb aber beim alten Stand:
//! - die Kind-Fabrik des Wurzel-Baums (`crate::children`) gab ungepinnten
//!   Kindern — also u. a. dem Orchestrator, der die eigentliche Arbeit
//!   macht — das beim Start ermittelte Vorgabemodell als `active_model`
//!   und keinen Provider (Router-Vorgabe des Starts);
//! - die Rollenwahl (`/models set …`) wurde nur gespeichert;
//! - ein Provider, dessen Client beim Start nicht gebaut werden konnte
//!   (z. B. `secrets:`-Schlüssel, deren Speicher nur für den
//!   Vorgabe-Provider geöffnet wird), blieb bis zum Neustart ein
//!   Fehler-Backend.
//!
//! # Neue Regel
//! [`LiveModelRouting`] hält die Live-Wahl der Montage:
//! - [`LiveModelRouting::set_live_main`] — die generische Auswahl des
//!   Controllers, von der TUI an jeder Turn-Grenze gemeldet. Neu gestartete
//!   ungepinnte Kinder sprechen sie über einen [`PinnedModelProvider`] an;
//!   laufende behalten ihr Modell.
//! - [`LiveModelControl::set_internal_model`] — die Rollenwahl für neu
//!   gestartete Kinder (Orchestrator, Worker, Explorer, Recherche, …).
//! - [`LiveModelControl::ensure_provider_ready`] — baut einen nicht
//!   verfügbaren Provider-Client über den regulären Provider-Bau
//!   ([`harw_provider_http::build_named_backend`]) neu und setzt ihn in den
//!   Router ein; scheitert das, lehnt die Wechsel-Operation ab und das alte
//!   Modell bleibt aktiv.
//!
//! # Nebenläufigkeit
//! Zustand hinter `RwLock`; ein Neubau ist durch eine eigene Sperre
//! serialisiert. Eine vergiftete Sperre gilt als „keine Live-Wahl"
//! (Rückfall auf den Stand des Starts), nie als Panik.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use harw_config::{
    InternalModelChoice, InternalModelPoint, InternalModelsToml, ResolvedConfig,
    ResolvedInternalModel, catalog_provider_of,
};
use harw_core::{ModelProvider, PinnedModelProvider};
use harw_ops::live_model::LiveModelControl;
use harw_provider_http::{RoutingBackends, SecretResolver};
use harw_types::{ModelId, ProviderId};

/// Geteilter `secrets:`-Resolver der Montage.
pub type SharedSecretResolver = Arc<dyn SecretResolver + Send + Sync>;

/// Öffnet bei Bedarf den versiegelten Speicher für einen Provider-Neubau
/// (von der Composition Root gesetzt, z. B. `harw-cli`).
pub type SecretResolverOpener =
    dyn Fn(&ResolvedConfig) -> Result<Option<SharedSecretResolver>, String> + Send + Sync;

/// Live-Auswahl des Hauptmodells (Provider optional, Modell gesetzt).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveMainRoute {
    provider: Option<String>,
    model: String,
}

/// Veränderlicher Teil von [`LiveModelRouting`].
#[derive(Debug)]
struct LiveState {
    /// Zuletzt gemeldete Controller-Auswahl; `None` → Stand des Starts.
    main: Option<LiveMainRoute>,
    /// Aktuelle `[internal_models]`-Wahl (Start + Live-Änderungen).
    internal_models: InternalModelsToml,
    /// Daraus aufgelöste Kind-Modellstellen
    /// ([`crate::children::resolve_internal_models_for_children`]).
    resolved: HashMap<InternalModelPoint, ResolvedInternalModel>,
}

/// Live-Modellwahl einer Montage (siehe Moduldoku).
pub struct LiveModelRouting {
    /// Konfiguration des Starts (Katalog, Provider, Vorgaben).
    config: Arc<ResolvedConfig>,
    state: RwLock<LiveState>,
    /// Backend-Tabelle des Vorgabe-Routers; `None` ohne HTTP-Router
    /// (Echo/Override) — dann gibt es nichts neu zu bauen.
    backends: Option<Arc<RoutingBackends>>,
    /// harw-Home für `file:`-Referenzen beim Neubau.
    home: Option<PathBuf>,
    /// Beim Start geöffneter `secrets:`-Resolver (falls vorhanden).
    resolver: Option<SharedSecretResolver>,
    /// Öffnet den Resolver nachträglich, wenn beim Start keiner nötig war.
    resolver_opener: Option<Arc<SecretResolverOpener>>,
    /// Serialisiert Neubauten desselben oder verschiedener Provider.
    rebuild: Mutex<()>,
}

impl std::fmt::Debug for LiveModelRouting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let main = self.state.read().ok().and_then(|state| state.main.clone());
        f.debug_struct("LiveModelRouting")
            .field("main", &main)
            .field("backends", &self.backends.is_some())
            .field("resolver", &self.resolver.is_some())
            .finish_non_exhaustive()
    }
}

impl LiveModelRouting {
    /// Baut die Live-Wahl aus der Konfiguration des Starts.
    #[must_use]
    pub fn new(config: Arc<ResolvedConfig>) -> Self {
        let internal_models = config.harness.internal_models.clone();
        let resolved = crate::children::resolve_internal_models_for_children(&config);
        Self {
            config,
            state: RwLock::new(LiveState {
                main: None,
                internal_models,
                resolved,
            }),
            backends: None,
            home: None,
            resolver: None,
            resolver_opener: None,
            rebuild: Mutex::new(()),
        }
    }

    /// Ergänzt die Backend-Tabelle des Vorgabe-Routers und das harw-Home
    /// für einen Provider-Neubau.
    #[must_use]
    pub fn with_backends(mut self, backends: Option<Arc<RoutingBackends>>, home: PathBuf) -> Self {
        self.backends = backends;
        self.home = Some(home);
        self
    }

    /// Ergänzt den beim Start geöffneten Resolver und den nachträglichen
    /// Öffner.
    #[must_use]
    pub fn with_secret_resolvers(
        mut self,
        resolver: Option<SharedSecretResolver>,
        opener: Option<Arc<SecretResolverOpener>>,
    ) -> Self {
        self.resolver = resolver;
        self.resolver_opener = opener;
        self
    }

    /// Merkt sich die generische Controller-Auswahl.
    ///
    /// # Beschreibung
    /// Die TUI ruft das an jeder Turn-Grenze auf (ein laufender Turn behält
    /// sein Modell). Ohne Modell gilt wieder der Stand des Starts. Wirkt nur
    /// auf **neu** gestartete Kinder.
    pub fn set_live_main(&self, provider: Option<String>, model: Option<String>) {
        let route = model
            .filter(|model| !model.trim().is_empty())
            .map(|model| LiveMainRoute {
                provider: provider.filter(|provider| !provider.trim().is_empty()),
                model,
            });
        if let Ok(mut state) = self.state.write() {
            state.main = route;
        }
    }

    /// Provider/Modell, das ein jetzt gestartetes ungepinntes Kind anspricht;
    /// `None` → Stand des Starts.
    #[must_use]
    pub fn live_main(&self) -> Option<(Option<String>, String)> {
        let live = self
            .state
            .read()
            .ok()
            .and_then(|state| state.main.clone())?;
        let provider = live
            .provider
            .or_else(|| catalog_provider_of(&self.config, &live.model));
        Some((provider, live.model))
    }

    /// Das Modell für ein neu gestartetes Kind auf dem Hauptmodell.
    ///
    /// # Rückgabe
    /// Ohne Live-Wahl `base` unverändert, sonst ein [`PinnedModelProvider`]
    /// um `base`, der Provider und Modell der Live-Wahl fest anspricht.
    #[must_use]
    pub fn main_model_provider(&self, base: &Arc<dyn ModelProvider>) -> Arc<dyn ModelProvider> {
        match self.live_main() {
            Some((provider, model)) => Arc::new(PinnedModelProvider::new(
                Arc::clone(base),
                provider.map(ProviderId::from),
                Some(ModelId::from(model)),
            )),
            None => Arc::clone(base),
        }
    }

    /// Aktuelle Auflösung einer Kind-Modellstelle.
    #[must_use]
    pub fn resolved_internal_model(
        &self,
        point: InternalModelPoint,
    ) -> Option<ResolvedInternalModel> {
        self.state
            .read()
            .ok()
            .and_then(|state| state.resolved.get(&point).cloned())
    }

    /// Konfiguration mit dem Default-Modell, das der Router tatsächlich
    /// nutzt (Rückfall wie beim Start).
    fn build_config(&self) -> std::borrow::Cow<'_, ResolvedConfig> {
        if self.config.harness.default_model.is_some() {
            return std::borrow::Cow::Borrowed(self.config.as_ref());
        }
        let mut config = (*self.config).clone();
        config.harness.default_model = crate::model::effective_default_model_id(&config);
        std::borrow::Cow::Owned(config)
    }

    /// Baut das Backend von `provider` über den regulären Provider-Bau.
    fn build_backend(
        &self,
        provider: &str,
        resolver: Option<&SharedSecretResolver>,
    ) -> Result<Box<dyn ModelProvider>, String> {
        let config = self.build_config();
        harw_provider_http::build_named_backend(
            &config,
            provider,
            self.home.as_deref(),
            resolver.map(|resolver| &**resolver as &dyn SecretResolver),
        )
        .map(|(backend, _load_control)| backend)
        .map_err(|error| error.to_string())
    }
}

impl LiveModelControl for LiveModelRouting {
    fn ensure_provider_ready(&self, provider: &str) -> Result<(), String> {
        let Some(backends) = &self.backends else {
            return Ok(());
        };
        if backends.unavailable_reason(provider).is_none() {
            return Ok(());
        }
        let _guard = self
            .rebuild
            .lock()
            .map_err(|_| "provider rebuild lock is poisoned".to_owned())?;
        // Ein paralleler Wechsel kann das Backend inzwischen gebaut haben.
        if backends.unavailable_reason(provider).is_none() {
            return Ok(());
        }
        let first = match self.build_backend(provider, self.resolver.as_ref()) {
            Ok(backend) => {
                return install(backends, provider, backend);
            }
            Err(error) => error,
        };
        // Beim Start war kein `secrets:`-Resolver nötig (er wird nur für den
        // Vorgabe-Provider geöffnet) — jetzt nachträglich öffnen und erneut
        // bauen.
        let opened = match (&self.resolver, &self.resolver_opener) {
            (None, Some(opener)) => opener(&self.config)?,
            _ => None,
        };
        let Some(resolver) = opened else {
            return Err(first);
        };
        let backend = self.build_backend(provider, Some(&resolver))?;
        install(backends, provider, backend)
    }

    fn set_internal_model(
        &self,
        point: InternalModelPoint,
        choice: Option<InternalModelChoice>,
    ) -> bool {
        let Ok(mut state) = self.state.write() else {
            return false;
        };
        state.internal_models.set_choice(point, choice);
        let mut config = (*self.config).clone();
        config.harness.internal_models = state.internal_models.clone();
        state.resolved = crate::children::resolve_internal_models_for_children(&config);
        let live = state.resolved.contains_key(&point);
        tracing::info!(
            point = point.key(),
            live,
            "runtime.live_model.internal_model"
        );
        live
    }
}

/// Löst eine Modellkennung (Katalog-Schlüssel, `id` oder Alias) zur
/// Modell-ID des Katalogs auf; eine unbekannte Kennung bleibt unverändert.
///
/// # Beschreibung
/// Grundlage der Modellanzeige (`<provider>/<modell>`): angezeigt wird die
/// Modell-ID, die tatsächlich angefragt wird, nie ein Alias.
#[must_use]
pub fn resolve_model_id(config: &ResolvedConfig, model: &str) -> String {
    config
        .models
        .get(model)
        .or_else(|| {
            config
                .models
                .values()
                .find(|entry| entry.id == model || entry.aliases.iter().any(|alias| alias == model))
        })
        .map_or_else(|| model.to_owned(), |entry| entry.id.clone())
}

/// Setzt ein neu gebautes Backend in den Router ein.
fn install(
    backends: &RoutingBackends,
    provider: &str,
    backend: Box<dyn ModelProvider>,
) -> Result<(), String> {
    backends
        .replace(provider, backend)
        .map_err(|error| error.to_string())?;
    tracing::info!(provider, "runtime.live_model.provider_rebuilt");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_config::{HarnessConfig, OriginAllowlistToml, ProviderToml, SecretRef};
    use harw_core::{ModelError, ModelFuture, ModelRequest};
    use harw_extension_api::types::LoadedInstructions;
    use harw_provider_http::RoutingModelProvider;
    use std::collections::BTreeMap;

    /// Merkt sich Provider/Modell jeder Anfrage und scheitert dann.
    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<(Option<String>, Option<String>)>>,
    }

    impl ModelProvider for Recorder {
        fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push((
                    request.provider_id.map(|id| id.as_str().to_owned()),
                    request.model_id.map(|id| id.as_str().to_owned()),
                ));
            }
            Box::pin(async { Err(ModelError::RequestFailed("recorder".to_owned())) })
        }
    }

    fn request() -> ModelRequest {
        ModelRequest::new(
            LoadedInstructions::default(),
            Vec::new(),
            harw_core::ConversationHistory::new(),
            Vec::new(),
        )
    }

    fn ask(provider: &dyn ModelProvider) -> TestResult<Result<(), ModelError>> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        Ok(runtime.block_on(provider.respond(request())).map(|_| ()))
    }

    fn last(recorder: &Recorder) -> Option<(Option<String>, Option<String>)> {
        recorder
            .seen
            .lock()
            .ok()
            .and_then(|seen| seen.last().cloned())
    }

    fn loopback_provider(name: &str, auth: Option<SecretRef>) -> ProviderToml {
        ProviderToml {
            stream: None,
            name: name.to_owned(),
            api: "openai-chat".to_owned(),
            base_url: "http://127.0.0.1:11434/v1".to_owned(),
            auth_header: if auth.is_some() {
                None
            } else {
                Some("none".to_owned())
            },
            auth,
            api_key: None,
            headers: std::collections::HashMap::new(),
            models: vec![format!("{name}-model")],
            enabled: true,
            origin_allowlist: OriginAllowlistToml::default(),
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
        }
    }

    /// Vorgabe `local` ohne Zugangsdaten, zweiter Provider `sealed` mit
    /// `secrets:`-Referenz (beim Start ohne Resolver nicht baubar).
    fn sealed_config() -> ResolvedConfig {
        let mut config = ResolvedConfig {
            harness: HarnessConfig {
                default_provider: Some("local".to_owned()),
                default_model: Some("local-model".to_owned()),
                ..HarnessConfig::default()
            },
            ..ResolvedConfig::default()
        };
        config
            .providers
            .insert("local".to_owned(), loopback_provider("local", None));
        config.providers.insert(
            "sealed".to_owned(),
            loopback_provider("sealed", Some(SecretRef::Secrets("sealed-key".to_owned()))),
        );
        config
    }

    struct FakeResolver;

    impl SecretResolver for FakeResolver {
        fn resolve(&self, _reference: &str) -> Result<secrecy::SecretString, String> {
            Ok(secrecy::SecretString::from("sealed-secret".to_owned()))
        }
    }

    /// Ohne Live-Wahl bleibt ein neues Kind beim Start-Client; nach einem
    /// Wechsel auf einen anderen Provider spricht ein **neues** Kind Provider
    /// und Modell der Wahl an, ein vorher gestartetes behält seins.
    #[test]
    fn new_children_follow_a_live_switch_running_ones_keep_their_model() -> TestResult {
        let recorder = Arc::new(Recorder::default());
        let base: Arc<dyn ModelProvider> = recorder.clone();
        let routing = LiveModelRouting::new(Arc::new(ResolvedConfig::default()));

        let before = routing.main_model_provider(&base);
        assert!(Arc::ptr_eq(&before, &base), "ohne Wahl kein Wrapper");

        routing.set_live_main(Some("openai".to_owned()), Some("gpt-5".to_owned()));
        let after = routing.main_model_provider(&base);
        let _ = ask(after.as_ref())?;
        assert_eq!(
            last(&recorder),
            Some((Some("openai".to_owned()), Some("gpt-5".to_owned())))
        );
        assert_eq!(after.pinned_model_id().as_deref(), Some("gpt-5"));

        routing.set_live_main(Some("anthropic".to_owned()), Some("claude-x".to_owned()));
        let _ = ask(after.as_ref())?;
        assert_eq!(
            last(&recorder),
            Some((Some("openai".to_owned()), Some("gpt-5".to_owned()))),
            "ein laufendes Kind behält sein Modell"
        );

        routing.set_live_main(None, None);
        assert!(routing.live_main().is_none());
        Ok(())
    }

    /// Eine Live-Wahl ohne Provider übernimmt den Provider aus dem Katalog.
    #[test]
    fn live_main_without_provider_uses_the_catalog_provider() {
        let mut config = ResolvedConfig::default();
        config.models.insert(
            "gpt-5".to_owned(),
            harw_config::ModelToml {
                stream: None,
                rate_limit: None,
                id: "gpt-5".to_owned(),
                name: None,
                provider: "openai".to_owned(),
                aliases: Vec::new(),
                context_window: None,
                max_tokens: None,
                prompt_caching: None,
                reasoning: false,
                input_types: Vec::new(),
                capabilities: harw_config::ModelCapabilitiesToml::default(),
                default_reasoning_effort: None,
            },
        );
        let routing = LiveModelRouting::new(Arc::new(config));
        routing.set_live_main(None, Some("gpt-5".to_owned()));
        assert_eq!(
            routing.live_main(),
            Some((Some("openai".to_owned()), "gpt-5".to_owned()))
        );
    }

    /// `/models set orchestrator …` wirkt für neue Kinder sofort; eine Stelle
    /// außerhalb des Kind-Baums (Sitzungstitel) meldet `false`.
    #[test]
    fn role_choice_applies_live_for_child_points_only() {
        let routing = LiveModelRouting::new(Arc::new(ResolvedConfig::default()));
        let live = routing.set_internal_model(
            InternalModelPoint::RootOrchestrator,
            Some(InternalModelChoice {
                provider: Some("openai".to_owned()),
                model: Some("gpt-5".to_owned()),
            }),
        );
        assert!(live);
        let resolved = routing.resolved_internal_model(InternalModelPoint::RootOrchestrator);
        assert_eq!(
            resolved
                .as_ref()
                .and_then(|resolved| resolved.model.as_deref()),
            Some("gpt-5")
        );
        assert_eq!(
            resolved
                .as_ref()
                .and_then(|resolved| resolved.provider.as_deref()),
            Some("openai")
        );

        assert!(routing.set_internal_model(InternalModelPoint::RootOrchestrator, None));
        assert!(
            routing
                .resolved_internal_model(InternalModelPoint::RootOrchestrator)
                .is_some_and(|resolved| resolved.is_main_model())
        );
        assert!(!routing.set_internal_model(
            InternalModelPoint::SessionTitle,
            Some(InternalModelChoice {
                provider: None,
                model: Some("m".to_owned()),
            }),
        ));
    }

    /// Ein beim Start nicht baubarer Provider (kein `secrets:`-Resolver)
    /// wird beim Wechsel mit dem nachträglich geöffneten Resolver gebaut und
    /// ersetzt das Fehler-Backend; die nächste Anfrage erreicht ihn.
    #[test]
    fn switch_rebuilds_a_provider_that_was_unavailable_at_startup() -> TestResult {
        let config = sealed_config();
        let (router, _) = harw_provider_http::build_routing_provider_with_load_registry_and_home(
            &config,
            std::path::Path::new("/nonexistent-home"),
            None,
        )
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
        let backends = router.backends();
        assert!(backends.unavailable_reason("sealed").is_some());
        assert!(backends.unavailable_reason("local").is_none());

        let opener: Arc<SecretResolverOpener> = Arc::new(
            |_config: &ResolvedConfig| -> Result<Option<SharedSecretResolver>, String> {
                let resolver: SharedSecretResolver = Arc::new(FakeResolver);
                Ok(Some(resolver))
            },
        );
        let routing = LiveModelRouting::new(Arc::new(config))
            .with_backends(Some(backends.clone()), PathBuf::from("/nonexistent-home"))
            .with_secret_resolvers(None, Some(opener));
        routing
            .ensure_provider_ready("sealed")
            .map_err(TestError::Unexpected)?;
        assert!(backends.unavailable_reason("sealed").is_none());
        // Bereits nutzbar: kein erneuter Bau, kein Fehler.
        routing
            .ensure_provider_ready("local")
            .map_err(TestError::Unexpected)?;
        Ok(())
    }

    /// Scheitert der Neubau, meldet der Dienst den Grund und das
    /// Fehler-Backend bleibt stehen (die Operation lehnt dann ab).
    #[test]
    fn failed_rebuild_reports_the_reason_and_keeps_the_error_backend() -> TestResult {
        let config = sealed_config();
        let (router, _) = harw_provider_http::build_routing_provider_with_load_registry_and_home(
            &config,
            std::path::Path::new("/nonexistent-home"),
            None,
        )
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
        let backends = router.backends();
        let routing = LiveModelRouting::new(Arc::new(config))
            .with_backends(Some(backends.clone()), PathBuf::from("/nonexistent-home"));
        let Err(reason) = routing.ensure_provider_ready("sealed") else {
            return Err(TestError::Unexpected(
                "ohne Resolver darf der Neubau nicht gelingen".to_owned(),
            ));
        };
        assert!(!reason.is_empty());
        assert!(backends.unavailable_reason("sealed").is_some());
        assert!(routing.ensure_provider_ready("unknown").is_err());
        Ok(())
    }

    /// Router-Ebene: vor dem Einsetzen lehnt das Fehler-Backend mit seinem
    /// Grund ab, danach erreicht die nächste Anfrage das neue Backend.
    #[test]
    fn replaced_backend_serves_the_next_request() -> TestResult {
        let recorder = Arc::new(Recorder::default());
        let mut providers: BTreeMap<String, Box<dyn ModelProvider>> = BTreeMap::new();
        providers.insert(
            "a".to_owned(),
            Box::new(PinnedModelProvider::new(recorder.clone(), None, None)),
        );
        let mut unavailable = BTreeMap::new();
        unavailable.insert("b".to_owned(), "missing credential".to_owned());
        let router = Arc::new(
            RoutingModelProvider::with_unavailable(providers, unavailable, "a")
                .map_err(|error| TestError::Unexpected(error.to_string()))?,
        );
        let backends = router.backends();
        let router: Arc<dyn ModelProvider> = router;
        let to_b = PinnedModelProvider::new(
            router,
            Some(ProviderId::from("b")),
            Some(ModelId::from("b-model")),
        );
        let Err(ModelError::RequestFailed(reason)) = ask(&to_b)? else {
            return Err(TestError::Unexpected(
                "das Fehler-Backend muss ablehnen".to_owned(),
            ));
        };
        assert!(reason.contains("missing credential"));
        assert!(last(&recorder).is_none());

        backends
            .replace(
                "b",
                Box::new(PinnedModelProvider::new(recorder.clone(), None, None)),
            )
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert!(backends.unavailable_reason("b").is_none());
        let _ = ask(&to_b)?;
        assert_eq!(
            last(&recorder),
            Some((Some("b".to_owned()), Some("b-model".to_owned())))
        );
        Ok(())
    }
}
