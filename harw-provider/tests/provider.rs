//! Verhaltenstests für die Provider-Schicht.

mod common;

use common::{TestError, TestResult};
use harw_provider::{
    AgentProviderOverride, ApiKeyConfig, BearerAuth, HeaderMap, InvocationRequest,
    InvocationResponse, ModelBuilder, ProviderAuth, ProviderBuilder, ProviderError,
    ProviderInvoker, ProviderRegistry, ProviderResult, VecOp, VecProviderRegistry,
    invoke_with_failover, resolve_provider_chain_with_override,
};
use harw_types::{ModelId, ModelName, ProviderId, ProviderName};
use url::Url;

fn record(name: &str, primary: bool) -> TestResult<harw_provider::ProviderRecord> {
    let url = Url::parse("https://api.openai.com/v1")?;
    let builder = ProviderBuilder::new()
        .id(ProviderId::from(name))
        .name(ProviderName::from(name))
        .base_url(url)
        .auth(ProviderAuth::ApiKey(ApiKeyConfig {
            api_key: secrecy_string(),
        }));
    let built = if primary {
        builder.primary().build_record()?
    } else {
        builder.build_record()?
    };
    Ok(built)
}

fn secrecy_string() -> secrecy::SecretString {
    secrecy::SecretString::new("sk-test".to_owned())
}

#[test]
fn builder_requires_fields() -> TestResult {
    let Err(err) = ProviderBuilder::new().build_record() else {
        return Err(TestError::Unexpected(
            "build_record ohne Felder haette Err liefern muessen".to_owned(),
        ));
    };
    assert!(matches!(err, ProviderError::MissingProviderField("id")));
    Ok(())
}

#[test]
fn registry_register_and_resolve_chain() -> TestResult {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)?)?;
    reg.register(record("azure", false)?)?;
    reg.register(record("ollama", false)?)?;

    reg.validate()?;
    let chain = reg.resolve_execution_chain()?;
    assert_eq!(chain.primary().name.as_str(), "openai");
    assert_eq!(chain.secondaries().len(), 2);
    assert_eq!(chain.all().len(), 3);
    Ok(())
}

#[test]
fn duplicate_primary_is_rejected_by_validate() -> TestResult {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)?)?;
    reg.register(record("azure", true)?)?;
    assert!(matches!(
        reg.validate(),
        Err(ProviderError::DuplicatePrimaryProvider)
    ));
    Ok(())
}

#[test]
fn duplicate_name_is_rejected_on_register() -> TestResult {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)?)?;
    assert!(matches!(
        reg.register(record("openai", false)?),
        Err(ProviderError::ProviderAlreadyRegistered { .. })
    ));
    Ok(())
}

#[test]
fn vec_op_move_to_front_dedups() {
    let mut chain: Vec<ProviderName> = vec!["openai".into(), "azure".into(), "ollama".into()];
    VecOp::MoveToFront(ProviderName::from("azure")).apply(&mut chain);
    assert_eq!(chain[0].as_str(), "azure");
    assert_eq!(chain.len(), 3); // azure rückt nach vorn, altes Vorkommen fällt raus
}

#[test]
fn override_move_to_front_then_replace_tail() -> TestResult {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)?)?;
    reg.register(record("azure", false)?)?;
    reg.register(record("ollama", false)?)?;

    let ov = AgentProviderOverride {
        preferred_primary: Some(ProviderName::from("azure")),
        explicit_secondaries: Some(vec![ProviderName::from("ollama")]),
        ..Default::default()
    };
    let chain = resolve_provider_chain_with_override(&reg, &ov)?;
    assert_eq!(chain.primary().name.as_str(), "azure");
    assert_eq!(chain.secondaries().len(), 1);
    assert_eq!(chain.secondaries()[0].name.as_str(), "ollama");
    Ok(())
}

struct FlakyInvoker {
    succeed_on: &'static str,
}

impl ProviderInvoker for FlakyInvoker {
    fn invoke(&self, request: InvocationRequest) -> ProviderResult<InvocationResponse> {
        if request.provider.as_str() == self.succeed_on {
            Ok(InvocationResponse {
                provider: request.provider,
                model: request.model,
            })
        } else {
            Err(ProviderError::ProviderNotRegistered {
                name: request.provider,
            })
        }
    }
}

#[test]
fn failover_walks_chain_and_succeeds_on_secondary() -> TestResult {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)?)?;
    reg.register(record("ollama", false)?)?;
    let chain = reg.resolve_execution_chain()?;

    let invoker = FlakyInvoker {
        succeed_on: "ollama",
    };
    let resp = invoke_with_failover(&chain, &invoker, InvocationRequest::new("ignored".into()))?;
    assert_eq!(resp.provider.as_str(), "ollama");
    Ok(())
}

#[test]
fn failover_collects_all_failures() -> TestResult {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)?)?;
    reg.register(record("ollama", false)?)?;
    let chain = reg.resolve_execution_chain()?;

    let invoker = FlakyInvoker {
        succeed_on: "nobody",
    };
    let Err(err) = invoke_with_failover(&chain, &invoker, InvocationRequest::new("x".into()))
    else {
        return Err(TestError::Unexpected(
            "invoke_with_failover haette scheitern muessen".to_owned(),
        ));
    };
    match err {
        ProviderError::AllProvidersFailed { tried } => assert_eq!(tried.len(), 2),
        other => return Err(TestError::Unexpected(format!("unexpected error: {other}"))),
    }
    Ok(())
}

#[test]
fn bearer_auth_sets_authorization_header() {
    let auth = BearerAuth::from_static("tok-123");
    let mut headers = HeaderMap::new();
    use harw_provider::AuthProvider;
    auth.add_auth_headers(&mut headers);
    assert_eq!(headers.get("authorization"), Some("Bearer tok-123"));
}

#[test]
fn model_builder_capability_transition() -> TestResult {
    let rec = ModelBuilder::new()
        .id(ModelId::from("text-embedding-3-large"))
        .name(ModelName::from("text-embedding-3-large"))
        .embedding()
        .build_record()?;
    assert_eq!(rec.capability, harw_provider::ModelCapabilityTag::Embedding);
    Ok(())
}
