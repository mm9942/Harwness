//! Deterministic selection between configured model-provider backends.
//!
//! The router owns the backend registry and performs provider selection before
//! the request crosses a concrete HTTP-provider boundary.  It never rewrites
//! a [`ModelRequest`]: in particular, `model_id` remains available to the
//! selected backend.
//!
//! ## No silent fallback (G-048, W4a / A-OAI)
//! Routing never substitutes a different backend (and never an echo
//! provider): a request naming an unknown provider, or carrying an empty
//! provider id, fails with [`ModelError::RequestFailed`] before any backend
//! is called. Only a request *without* a provider id uses the configured
//! default. A backend whose construction failed stays registered as an
//! explicit error backend (see `build_provider`), so its requests fail
//! loudly instead of reaching another provider.
//!
//! ## Live-Neubau eines Backends
//! Die Backend-Tabelle liegt in einem geteilten [`RoutingBackends`]
//! ([`RoutingModelProvider::backends`]). Ein beim Start nicht baubares
//! Backend (z. B. fehlende Zugangsdaten) bleibt mit seinem Grund als
//! „nicht verfügbar" markiert und kann zur Laufzeit per
//! [`RoutingBackends::replace`] ersetzt werden — etwa wenn ein
//! Modellwechsel auf diesen Provider zielt. Ein laufender Request behält
//! das Backend, das er bei der Auswahl erhalten hat.

use crate::error::{HttpProviderError, HttpProviderResult};
use harw_core::{ModelError, ModelFuture, ModelProvider, ModelRequest};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// Ein registriertes Backend samt optionalem Nicht-verfügbar-Grund.
struct Backend {
    /// Der Provider, an den geroutet wird.
    provider: Arc<dyn ModelProvider>,
    /// `Some(grund)`, wenn der Bau scheiterte und `provider` nur den Fehler
    /// meldet.
    unavailable: Option<String>,
}

/// Geteilte, zur Laufzeit ersetzbare Backend-Tabelle eines
/// [`RoutingModelProvider`].
///
/// # Nebenläufigkeit
/// Die Tabelle liegt hinter einem `RwLock`; jeder Request hält die Sperre
/// nur für die Auswahl (Klon eines `Arc`), nie über den Modellaufruf.
pub struct RoutingBackends {
    entries: RwLock<BTreeMap<String, Backend>>,
}

impl RoutingBackends {
    /// `true`, wenn `provider_id` registriert ist (verfügbar oder nicht).
    #[must_use]
    pub fn contains(&self, provider_id: &str) -> bool {
        self.entries
            .read()
            .is_ok_and(|entries| entries.contains_key(provider_id))
    }

    /// Grund, warum das Backend von `provider_id` nicht nutzbar ist.
    ///
    /// # Returns
    /// `None` für ein nutzbares Backend, `Some(grund)` für ein beim Bau
    /// gescheitertes Backend oder eine vergiftete Sperre. Ein nicht
    /// registrierter Provider liefert ebenfalls `Some`.
    #[must_use]
    pub fn unavailable_reason(&self, provider_id: &str) -> Option<String> {
        let Ok(entries) = self.entries.read() else {
            return Some("provider routing table lock is poisoned".to_owned());
        };
        match entries.get(provider_id) {
            Some(backend) => backend.unavailable.clone(),
            None => Some(format!("provider '{provider_id}' is not configured")),
        }
    }

    /// Ersetzt (oder ergänzt) das Backend von `provider_id` durch ein
    /// nutzbares.
    ///
    /// # Description
    /// Wirkt nur auf Requests, die **danach** ausgewählt werden; ein
    /// laufender Request behält sein Backend.
    ///
    /// # Errors
    /// [`ModelError::RequestFailed`], wenn die Sperre vergiftet ist.
    pub fn replace(
        &self,
        provider_id: &str,
        provider: Box<dyn ModelProvider>,
    ) -> Result<(), ModelError> {
        let mut entries = self.entries.write().map_err(|_| {
            ModelError::RequestFailed("provider routing table lock is poisoned".to_owned())
        })?;
        entries.insert(
            provider_id.to_owned(),
            Backend {
                provider: Arc::from(provider),
                unavailable: None,
            },
        );
        Ok(())
    }

    /// Wählt das Backend unter der Lesesperre und klont seinen `Arc`.
    fn get(&self, provider_id: &str) -> Result<Option<Arc<dyn ModelProvider>>, ModelError> {
        let entries = self.entries.read().map_err(|_| {
            ModelError::RequestFailed("provider routing table lock is poisoned".to_owned())
        })?;
        Ok(entries
            .get(provider_id)
            .map(|backend| Arc::clone(&backend.provider)))
    }
}

/// Routes each model request to its selected configured provider backend.
///
/// A [`BTreeMap`] makes the registry's ownership and validation deterministic;
/// request-time lookup remains logarithmic and does not depend on insertion
/// order.
pub struct RoutingModelProvider {
    backends: Arc<RoutingBackends>,
    default_provider_id: String,
}

impl RoutingModelProvider {
    /// Creates a router after validating that the registry is usable.
    ///
    /// # Errors
    ///
    /// Returns [`HttpProviderError::EmptyProviderSet`] when no backends were
    /// configured, or [`HttpProviderError::DefaultProviderNotFound`] when the
    /// configured default does not name a backend in the registry.
    pub fn new(
        providers: BTreeMap<String, Box<dyn ModelProvider>>,
        default_provider_id: impl Into<String>,
    ) -> HttpProviderResult<Self> {
        Self::with_unavailable(providers, BTreeMap::new(), default_provider_id)
    }

    /// Wie [`Self::new`], zusätzlich mit beim Bau gescheiterten Backends.
    ///
    /// # Description
    /// Jeder Eintrag in `unavailable` (Provider → Grund) wird als
    /// Fehler-Backend registriert, das jeden Request mit dem Grund ablehnt
    /// (kein stiller Rückfall), und bleibt über
    /// [`RoutingBackends::unavailable_reason`] abfragbar, bis
    /// [`RoutingBackends::replace`] es ersetzt. Ein Name in beiden Tabellen
    /// gilt als nutzbar.
    ///
    /// # Errors
    /// Wie [`Self::new`]; ein nicht baubarer Vorgabe-Provider zählt als
    /// vorhanden (der Aufrufer entscheidet vorher, ob das fatal ist).
    pub fn with_unavailable(
        providers: BTreeMap<String, Box<dyn ModelProvider>>,
        unavailable: BTreeMap<String, String>,
        default_provider_id: impl Into<String>,
    ) -> HttpProviderResult<Self> {
        if providers.is_empty() && unavailable.is_empty() {
            return Err(HttpProviderError::EmptyProviderSet);
        }

        let default_provider_id = default_provider_id.into();
        if !providers.contains_key(&default_provider_id)
            && !unavailable.contains_key(&default_provider_id)
        {
            return Err(HttpProviderError::DefaultProviderNotFound {
                name: default_provider_id,
            });
        }

        let mut entries: BTreeMap<String, Backend> = unavailable
            .into_iter()
            .map(|(name, reason)| {
                let provider: Arc<dyn ModelProvider> =
                    Arc::new(UnavailableProvider(reason.clone()));
                (
                    name,
                    Backend {
                        provider,
                        unavailable: Some(reason),
                    },
                )
            })
            .collect();
        for (name, provider) in providers {
            entries.insert(
                name,
                Backend {
                    provider: Arc::from(provider),
                    unavailable: None,
                },
            );
        }

        Ok(Self {
            backends: Arc::new(RoutingBackends {
                entries: RwLock::new(entries),
            }),
            default_provider_id,
        })
    }

    /// Returns the ids of all registered backends in deterministic order.
    #[must_use]
    pub fn provider_ids(&self) -> Vec<String> {
        self.backends
            .entries
            .read()
            .map(|entries| entries.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Geteilter Handle auf die Backend-Tabelle (für einen Live-Neubau nach
    /// einem Provider-Wechsel, siehe Moduldoku).
    #[must_use]
    pub fn backends(&self) -> Arc<RoutingBackends> {
        Arc::clone(&self.backends)
    }

    /// Resolves the backend for `request` without any fallback.
    ///
    /// # Errors
    /// [`ModelError::RequestFailed`] for an empty provider id or an id that is
    /// not registered (G-048: never routed to the default instead).
    fn select(&self, request: &ModelRequest) -> Result<Arc<dyn ModelProvider>, ModelError> {
        let provider_id = match request.provider_id.as_ref() {
            None => self.default_provider_id.as_str(),
            Some(provider_id) if provider_id.as_str().trim().is_empty() => {
                return Err(ModelError::RequestFailed(
                    "requested model provider id is empty; refusing to fall back to the default provider"
                        .to_owned(),
                ));
            }
            Some(provider_id) => provider_id.as_str(),
        };
        self.backends.get(provider_id)?.ok_or_else(|| {
            tracing::warn!(
                provider = provider_id,
                "model request for unconfigured provider"
            );
            ModelError::RequestFailed(format!(
                "requested model provider '{provider_id}' is not configured"
            ))
        })
    }
}

/// Fehler-Backend eines beim Bau gescheiterten Providers: lehnt jeden
/// Request mit dem Baufehler ab.
struct UnavailableProvider(String);

impl ModelProvider for UnavailableProvider {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move { Err(ModelError::RequestFailed(self.0.clone())) })
    }
}

impl ModelProvider for RoutingModelProvider {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        match self.select(&request) {
            Ok(provider) => Box::pin(async move { provider.respond(request).await }),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }

    /// Meldet nie eine gepinnte Modell-ID.
    ///
    /// # Description
    /// Der Router wählt das Backend pro Request anhand von
    /// `request.provider_id`/`request.model_id`; es gibt kein einzelnes,
    /// fest angesprochenes Modell. Die Pins einzelner Backends gelten nur für
    /// Requests, die dorthin geroutet werden, und werden daher bewusst nicht
    /// durchgereicht.
    ///
    /// # Returns
    /// Immer `None`.
    fn pinned_model_id(&self) -> Option<String> {
        None
    }

    /// Wartezeit des Vorgabe-Backends.
    ///
    /// # Description
    /// Anders als die Pins wird die Drosselung durchgereicht: Requests ohne
    /// `provider_id` landen beim Vorgabe-Backend, dessen Limits sind also die
    /// für ungeroutete Arbeit maßgeblichen (konservativ). Wartezeiten anderer
    /// Backends fließen bewusst nicht ein.
    ///
    /// # Returns
    /// Die Wartezeit des Vorgabe-Backends; `None` bei vergifteter Sperre
    /// (der Request selbst scheitert dann ohnehin bei der Auswahl).
    fn pacing_wait(&self) -> Option<std::time::Duration> {
        self.backends
            .get(&self.default_provider_id)
            .ok()
            .flatten()
            .and_then(|provider| provider.pacing_wait())
    }
}

#[cfg(test)]
mod tests {
    use super::RoutingModelProvider;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_core::{
        ContextAssembly, ConversationHistory, ModelError, ModelFuture, ModelProvider, ModelRequest,
        ModelResponse,
    };
    use harw_types::{ModelId, ProviderId};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct RecordingProvider {
        response: &'static str,
        requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    impl RecordingProvider {
        fn new(response: &'static str, requests: Arc<Mutex<Vec<ModelRequest>>>) -> Self {
            Self { response, requests }
        }
    }

    impl ModelProvider for RecordingProvider {
        fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
            let response = self.response;
            let requests = Arc::clone(&self.requests);
            Box::pin(async move {
                requests
                    .lock()
                    .map_err(|poison| {
                        ModelError::RequestFailed(format!(
                            "mock provider requests lock poisoned: {poison}"
                        ))
                    })?
                    .push(request);
                Ok(ModelResponse::text(response))
            })
        }
    }

    /// Backend, das nur eine feste Drosselungs-Wartezeit meldet.
    struct PacedProvider(Option<Duration>);

    impl ModelProvider for PacedProvider {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            Box::pin(async move { Ok(ModelResponse::text("paced")) })
        }

        fn pacing_wait(&self) -> Option<Duration> {
            self.0
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            stream: None,
            system_prompt: "system".to_owned(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history: ConversationHistory::new(),
            tools: Vec::new(),
            context_assembly: ContextAssembly::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        }
    }

    fn registry(
        entries: impl IntoIterator<Item = (&'static str, Box<dyn ModelProvider>)>,
    ) -> BTreeMap<String, Box<dyn ModelProvider>> {
        entries
            .into_iter()
            .map(|(provider_id, provider)| (provider_id.to_owned(), provider))
            .collect()
    }

    #[tokio::test]
    async fn routes_to_default_and_preserves_model_id() -> TestResult {
        let default_requests = Arc::new(Mutex::new(Vec::new()));
        let explicit_requests = Arc::new(Mutex::new(Vec::new()));
        let router = RoutingModelProvider::new(
            registry([
                (
                    "default",
                    Box::new(RecordingProvider::new(
                        "default",
                        Arc::clone(&default_requests),
                    )) as Box<dyn ModelProvider>,
                ),
                (
                    "explicit",
                    Box::new(RecordingProvider::new("explicit", explicit_requests))
                        as Box<dyn ModelProvider>,
                ),
            ]),
            "default",
        )
        .map_err(ctx("router construction"))?;

        let response = router
            .respond(request().with_model_id(Some(ModelId::from("model-override"))))
            .await
            .map_err(ctx("respond"))?;

        assert_eq!(response.message.as_deref(), Some("default"));
        let requests = default_requests.lock().map_err(ctx("requests lock"))?;
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].model_id.as_ref().map(ModelId::as_str),
            Some("model-override")
        );
        Ok(())
    }

    #[tokio::test]
    async fn routes_to_explicit_provider() -> TestResult {
        let default_requests = Arc::new(Mutex::new(Vec::new()));
        let explicit_requests = Arc::new(Mutex::new(Vec::new()));
        let router = RoutingModelProvider::new(
            registry([
                (
                    "default",
                    Box::new(RecordingProvider::new("default", default_requests))
                        as Box<dyn ModelProvider>,
                ),
                (
                    "explicit",
                    Box::new(RecordingProvider::new(
                        "explicit",
                        Arc::clone(&explicit_requests),
                    )) as Box<dyn ModelProvider>,
                ),
            ]),
            "default",
        )
        .map_err(ctx("router construction"))?;

        let response = router
            .respond(request().with_provider_id(Some(ProviderId::from("explicit"))))
            .await
            .map_err(ctx("respond"))?;

        assert_eq!(response.message.as_deref(), Some("explicit"));
        assert_eq!(
            explicit_requests
                .lock()
                .map_err(ctx("requests lock"))?
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn rejects_unknown_provider_before_calling_any_backend() -> TestResult {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let router = RoutingModelProvider::new(
            registry([(
                "default",
                Box::new(RecordingProvider::new("default", Arc::clone(&requests)))
                    as Box<dyn ModelProvider>,
            )]),
            "default",
        )
        .map_err(ctx("router construction"))?;

        let Err(error) = router
            .respond(request().with_provider_id(Some(ProviderId::from("unknown"))))
            .await
        else {
            return Err(TestError::Unexpected(
                "unknown provider must be rejected".to_owned(),
            ));
        };

        assert!(matches!(error, ModelError::RequestFailed(_)));
        assert!(requests.lock().map_err(ctx("requests lock"))?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_respond_empty_provider_id_is_error_not_default_fallback() -> TestResult {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let router = RoutingModelProvider::new(
            registry([(
                "default",
                Box::new(RecordingProvider::new("default", Arc::clone(&requests)))
                    as Box<dyn ModelProvider>,
            )]),
            "default",
        )
        .map_err(ctx("router construction"))?;

        let Err(error) = router
            .respond(request().with_provider_id(Some(ProviderId::from(""))))
            .await
        else {
            return Err(TestError::Unexpected(
                "empty provider id must be rejected".to_owned(),
            ));
        };

        assert!(
            matches!(&error, ModelError::RequestFailed(message) if message.contains("empty")),
            "unexpected error: {error}"
        );
        assert!(requests.lock().map_err(ctx("requests lock"))?.is_empty());
        Ok(())
    }

    #[test]
    fn test_provider_ids_lists_registered_backends_in_order() -> TestResult {
        let router = RoutingModelProvider::new(
            registry([
                (
                    "zeta",
                    Box::new(RecordingProvider::new(
                        "z",
                        Arc::new(Mutex::new(Vec::new())),
                    )) as Box<dyn ModelProvider>,
                ),
                (
                    "alpha",
                    Box::new(RecordingProvider::new(
                        "a",
                        Arc::new(Mutex::new(Vec::new())),
                    )) as Box<dyn ModelProvider>,
                ),
            ]),
            "zeta",
        )
        .map_err(ctx("router construction"))?;
        assert_eq!(router.provider_ids(), vec!["alpha", "zeta"]);
        Ok(())
    }

    #[test]
    fn test_pacing_wait_reports_default_backend_only() -> TestResult {
        let wait = Duration::from_secs(7);
        let router = RoutingModelProvider::new(
            registry([
                (
                    "default",
                    Box::new(PacedProvider(Some(wait))) as Box<dyn ModelProvider>,
                ),
                (
                    "other",
                    Box::new(PacedProvider(Some(Duration::from_secs(60))))
                        as Box<dyn ModelProvider>,
                ),
            ]),
            "default",
        )
        .map_err(ctx("router construction"))?;
        assert_eq!(router.pacing_wait(), Some(wait));

        // Die Wartezeit eines Nicht-Vorgabe-Backends sickert nicht durch.
        let quiet = RoutingModelProvider::new(
            registry([
                (
                    "default",
                    Box::new(PacedProvider(None)) as Box<dyn ModelProvider>,
                ),
                (
                    "other",
                    Box::new(PacedProvider(Some(Duration::from_secs(60))))
                        as Box<dyn ModelProvider>,
                ),
            ]),
            "default",
        )
        .map_err(ctx("router construction"))?;
        assert_eq!(quiet.pacing_wait(), None);
        Ok(())
    }

    #[test]
    fn validates_empty_registry_and_missing_default_deterministically() -> TestResult {
        let Err(empty) = RoutingModelProvider::new(BTreeMap::new(), "default") else {
            return Err(TestError::Unexpected(
                "an empty provider set must be rejected".to_owned(),
            ));
        };
        assert!(matches!(empty, crate::HttpProviderError::EmptyProviderSet));

        let providers = registry([(
            "configured",
            Box::new(RecordingProvider::new(
                "configured",
                Arc::new(Mutex::new(Vec::new())),
            )) as Box<dyn ModelProvider>,
        )]);
        let Err(missing) = RoutingModelProvider::new(providers, "default") else {
            return Err(TestError::Unexpected(
                "a missing default provider must be rejected".to_owned(),
            ));
        };
        assert!(matches!(
            missing,
            crate::HttpProviderError::DefaultProviderNotFound { name }
                if name == "default"
        ));
        Ok(())
    }
}
