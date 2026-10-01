//! Offline model/provider-switch tests (public API only).
//!
//! Verifies that provider and model selection flows through the
//! `ProviderLike`/`ModelLike`/`ProviderInvoker` trait layer plus a
//! `ProviderRegistry`, with no network access, no keys, and no hard-coded
//! model name. A recording mock invoker stands in for the HTTP transport.

use std::sync::{Arc, Mutex};

use harw_provider::{
    InvocationRequest, InvocationResponse, ModelBuilder, ProviderBuilder, ProviderError,
    ProviderInvoker, ProviderRegistry, ProviderResult, VecProviderRegistry, invoke_primary,
};
use harw_types::{ModelId, ModelName, ProviderId, ProviderName};
use url::Url;

/// Test error type mirroring the crate's test-support shape.
type TestResult<T> = Result<T, String>;

/// Mock invoker: records every request and answers with a fixed response
/// echoing the requested provider/model — controlled, offline, key-free.
#[derive(Clone, Debug, Default)]
struct RecordingInvoker {
    seen: Arc<Mutex<Vec<InvocationRequest>>>,
}

impl RecordingInvoker {
    fn seen(&self) -> Vec<InvocationRequest> {
        self.seen.lock().map(|guard| guard.clone()).unwrap_or_default()
    }
}

impl ProviderInvoker for RecordingInvoker {
    fn invoke(&self, request: InvocationRequest) -> ProviderResult<InvocationResponse> {
        if let Ok(mut guard) = self.seen.lock() {
            guard.push(request.clone());
        }
        Ok(InvocationResponse {
            provider: request.provider,
            model: request.model,
        })
    }
}

/// Builds a provider record with the given id/name/role, no credentials.
fn record(
    id: &str,
    name: &str,
    primary: bool,
    model_id: &str,
    model_name: &str,
) -> TestResult<harw_provider::ProviderRecord> {
    let _ = name; // names are informational; ids carry the invocation path
    let url = Url::parse("http://127.0.0.1:1/v1").map_err(|e| format!("url: {e}"))?;
    let model = ModelBuilder::new()
        .id(ModelId::from(model_id))
        .name(ModelName::from(model_name))
        .build()
        .map_err(|e| format!("model build: {e}"))?;
    let builder = ProviderBuilder::new()
        .id(ProviderId::from(id))
        .name(ProviderName::from(id))
        .base_url(url)
        .auth(ProviderAuth::None);
    let builder = if primary { builder.primary() } else { builder };
    let mut record = builder
        .build_record()
        .map_err(|e| format!("provider build: {e}"))?;
    record.models.push(model.as_record());
    Ok(record)
}

#[test]
fn model_switch_reaches_the_selected_provider_and_model() -> TestResult {
    let mut registry = VecProviderRegistry::default();
    let primary = record("local-vllm", "Local vLLM", true, "model-a", "Model A")?;
    registry
        .register(primary)
        .map_err(|e| format!("register: {e}"))?;
    registry.validate().map_err(|e| format!("validate: {e}"))?;

    let invoker = RecordingInvoker::default();
    // The caller selects provider + model explicitly; nothing is hard-coded
    // inside the invocation path itself.
    let request = InvocationRequest::new(ProviderName::from("local-vllm"))
        .with_model(ModelName::from("model-a"));
    let response =
        invoke_primary(&registry, &invoker, request).map_err(|e| format!("invoke_primary: {e}"))?;

    assert_eq!(response.provider.as_str(), "local-vllm");
    assert_eq!(response.model.as_ref().map(|m| m.as_str()), Some("model-a"));
    let seen = invoker.seen();
    assert_eq!(seen.len(), 1, "exactly one recorded invocation");
    assert_eq!(seen[0].provider.as_str(), "local-vllm");
    assert_eq!(seen[0].model.as_ref().map(|m| m.as_str()), Some("model-a"));
    Ok(())
}

#[test]
fn switching_the_model_changes_only_the_request_not_the_provider() -> TestResult {
    let mut registry = VecProviderRegistry::default();
    registry
        .register(record("p1", "Provider One", true, "model-b", "Model B")?)
        .map_err(|e| format!("register: {e}"))?;
    registry.validate().map_err(|e| format!("validate: {e}"))?;

    let invoker = RecordingInvoker::default();
    for model in ["model-b", "model-c"] {
        let request =
            InvocationRequest::new(ProviderName::from("p1")).with_model(ModelName::from(model));
        let response =
            invoke_primary(&registry, &invoker, request).map_err(|e| format!("invoke {model}: {e}"))?;
        assert_eq!(response.model.as_ref().map(|m| m.as_str()), Some(model));
    }
    let seen = invoker.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].provider.as_str(), "p1");
    Ok(())
}

#[test]
fn unknown_provider_is_rejected_without_a_call() -> TestResult {
    let mut registry = VecProviderRegistry::default();
    registry
        .register(record("p1", "Provider One", true, "model-x", "Model X")?)
        .map_err(|e| format!("register: {e}"))?;
    registry.validate().map_err(|e| format!("validate: {e}"))?;

    let invoker = RecordingInvoker::default();
    let request = InvocationRequest::new(ProviderName::from("does-not-exist"));
    let outcome = invoke_primary(&registry, &invoker, request);
    assert!(outcome.is_err(), "unknown provider must fail");
    assert_eq!(
        invoker.seen().len(),
        0,
        "failed resolution must not reach the invoker"
    );
    let error = outcome.unwrap_err();
    match &error {
        ProviderError::ProviderNotRegistered { name } => {
            assert_eq!(name.as_str(), "does-not-exist");
        }
        other => return Err(format!("unexpected error: {other:?}")),
    }
    let _ = error;
    Ok(())
}

#[test]
fn model_record_flows_through_the_trait_layer() -> TestResult {
    use harw_provider::{ModelLike, ProviderLike, ProviderRecord};

    let provider_record = record("p2", "Provider Two", true, "model-y", "Model Y")?;
    assert_eq!(provider_record.models.len(), 1, "model attached");
    // The record round-trips through the erased trait view.
    let erased: &ProviderRecord = &provider_record;
    assert_eq!(
        <ProviderRecord as ProviderLike>::id(erased).as_str(),
        "p2"
    );
    let models = <ProviderRecord as ProviderLike>::models(erased);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id.as_str(), "model-y");
    assert_eq!(models[0].name.as_str(), "Model Y");
    Ok(())
}
