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

use crate::error::{HttpProviderError, HttpProviderResult};
use harw_core::{ModelError, ModelFuture, ModelProvider, ModelRequest};
use std::collections::BTreeMap;

/// Routes each model request to its selected configured provider backend.
///
/// A [`BTreeMap`] makes the registry's ownership and validation deterministic;
/// request-time lookup remains logarithmic and does not depend on insertion
/// order.
pub struct RoutingModelProvider {
    providers: BTreeMap<String, Box<dyn ModelProvider>>,
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
        if providers.is_empty() {
            return Err(HttpProviderError::EmptyProviderSet);
        }

        let default_provider_id = default_provider_id.into();
        if !providers.contains_key(&default_provider_id) {
            return Err(HttpProviderError::DefaultProviderNotFound {
                name: default_provider_id,
            });
        }

        Ok(Self {
            providers,
            default_provider_id,
        })
    }

    /// Returns the ids of all registered backends in deterministic order.
    pub fn provider_ids(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    /// Resolves the backend for `request` without any fallback.
    ///
    /// # Errors
    /// [`ModelError::RequestFailed`] for an empty provider id or an id that is
    /// not registered (G-048: never routed to the default instead).
    fn select(&self, request: &ModelRequest) -> Result<&dyn ModelProvider, ModelError> {
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
        self.providers
            .get(provider_id)
            .map(|provider| &**provider)
            .ok_or_else(|| {
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

impl ModelProvider for RoutingModelProvider {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        match self.select(&request) {
            Ok(provider) => provider.respond(request),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RoutingModelProvider;
    use harw_core::{
        ContextAssembly, ConversationHistory, ModelError, ModelFuture, ModelProvider, ModelRequest,
        ModelResponse,
    };
    use harw_types::{ModelId, ProviderId};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

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
                requests.lock().unwrap().push(request);
                Ok(ModelResponse::text(response))
            })
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
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
    async fn routes_to_default_and_preserves_model_id() {
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
        .unwrap();

        let response = router
            .respond(request().with_model_id(Some(ModelId::from("model-override"))))
            .await
            .unwrap();

        assert_eq!(response.message.as_deref(), Some("default"));
        let requests = default_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].model_id.as_ref().map(ModelId::as_str),
            Some("model-override")
        );
    }

    #[tokio::test]
    async fn routes_to_explicit_provider() {
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
        .unwrap();

        let response = router
            .respond(request().with_provider_id(Some(ProviderId::from("explicit"))))
            .await
            .unwrap();

        assert_eq!(response.message.as_deref(), Some("explicit"));
        assert_eq!(explicit_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rejects_unknown_provider_before_calling_any_backend() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let router = RoutingModelProvider::new(
            registry([(
                "default",
                Box::new(RecordingProvider::new("default", Arc::clone(&requests)))
                    as Box<dyn ModelProvider>,
            )]),
            "default",
        )
        .unwrap();

        let error = router
            .respond(request().with_provider_id(Some(ProviderId::from("unknown"))))
            .await
            .unwrap_err();

        assert!(matches!(error, ModelError::RequestFailed(_)));
        assert!(requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_respond_empty_provider_id_is_error_not_default_fallback() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let router = RoutingModelProvider::new(
            registry([(
                "default",
                Box::new(RecordingProvider::new("default", Arc::clone(&requests)))
                    as Box<dyn ModelProvider>,
            )]),
            "default",
        )
        .unwrap();

        let error = router
            .respond(request().with_provider_id(Some(ProviderId::from(""))))
            .await
            .unwrap_err();

        assert!(
            matches!(&error, ModelError::RequestFailed(message) if message.contains("empty")),
            "unexpected error: {error}"
        );
        assert!(requests.lock().unwrap().is_empty());
    }

    #[test]
    fn test_provider_ids_lists_registered_backends_in_order() {
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
        .unwrap();
        assert_eq!(
            router.provider_ids().collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
    }

    #[test]
    fn validates_empty_registry_and_missing_default_deterministically() {
        let Err(empty) = RoutingModelProvider::new(BTreeMap::new(), "default") else {
            panic!("an empty provider set must be rejected");
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
            panic!("a missing default provider must be rejected");
        };
        assert!(matches!(
            missing,
            crate::HttpProviderError::DefaultProviderNotFound { name }
                if name == "default"
        ));
    }
}
