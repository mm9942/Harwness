//! Verhaltenstests für die Provider-Schicht.

use harw_provider::{
    AgentProviderOverride, ApiKeyConfig, BearerAuth, HeaderMap, InvocationRequest,
    InvocationResponse, ModelBuilder, ProviderBuilder, ProviderError, ProviderInvoker,
    ProviderRegistry, ProviderResult, SghAuth, VecOp, VecProviderRegistry, invoke_with_failover,
    resolve_provider_chain_with_override,
};
use harw_types::{ModelId, ModelName, ProviderId, ProviderName};
use url::Url;

fn record(name: &str, primary: bool) -> harw_provider::ProviderRecord {
    let url = Url::parse("https://api.openai.com/v1").unwrap();
    let builder = ProviderBuilder::new()
        .id(ProviderId::from(name))
        .name(ProviderName::from(name))
        .base_url(url)
        .auth(SghAuth::ApiKey(ApiKeyConfig {
            api_key: secrecy_string(),
        }));
    if primary {
        builder.primary().build_record().unwrap()
    } else {
        builder.build_record().unwrap()
    }
}

fn secrecy_string() -> secrecy::SecretString {
    secrecy::SecretString::new("sk-test".to_owned())
}

#[test]
fn builder_requires_fields() {
    let err = ProviderBuilder::new().build_record().unwrap_err();
    assert!(matches!(err, ProviderError::MissingProviderField("id")));
}

#[test]
fn registry_register_and_resolve_chain() {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)).unwrap();
    reg.register(record("azure", false)).unwrap();
    reg.register(record("ollama", false)).unwrap();

    reg.validate().unwrap();
    let chain = reg.resolve_execution_chain().unwrap();
    assert_eq!(chain.primary().name.as_str(), "openai");
    assert_eq!(chain.secondaries().len(), 2);
    assert_eq!(chain.all().len(), 3);
}

#[test]
fn duplicate_primary_is_rejected_by_validate() {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)).unwrap();
    reg.register(record("azure", true)).unwrap();
    assert!(matches!(
        reg.validate(),
        Err(ProviderError::DuplicatePrimaryProvider)
    ));
}

#[test]
fn duplicate_name_is_rejected_on_register() {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)).unwrap();
    assert!(matches!(
        reg.register(record("openai", false)),
        Err(ProviderError::ProviderAlreadyRegistered { .. })
    ));
}

#[test]
fn vec_op_move_to_front_dedups() {
    let mut chain: Vec<ProviderName> = vec!["openai".into(), "azure".into(), "ollama".into()];
    VecOp::MoveToFront(ProviderName::from("azure")).apply(&mut chain);
    assert_eq!(chain[0].as_str(), "azure");
    assert_eq!(chain.len(), 3); // azure rückt nach vorn, altes Vorkommen fällt raus
}

#[test]
fn override_move_to_front_then_replace_tail() {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)).unwrap();
    reg.register(record("azure", false)).unwrap();
    reg.register(record("ollama", false)).unwrap();

    let ov = AgentProviderOverride {
        preferred_primary: Some(ProviderName::from("azure")),
        explicit_secondaries: Some(vec![ProviderName::from("ollama")]),
        ..Default::default()
    };
    let chain = resolve_provider_chain_with_override(&reg, &ov).unwrap();
    assert_eq!(chain.primary().name.as_str(), "azure");
    assert_eq!(chain.secondaries().len(), 1);
    assert_eq!(chain.secondaries()[0].name.as_str(), "ollama");
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
fn failover_walks_chain_and_succeeds_on_secondary() {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)).unwrap();
    reg.register(record("ollama", false)).unwrap();
    let chain = reg.resolve_execution_chain().unwrap();

    let invoker = FlakyInvoker {
        succeed_on: "ollama",
    };
    let resp =
        invoke_with_failover(&chain, &invoker, InvocationRequest::new("ignored".into())).unwrap();
    assert_eq!(resp.provider.as_str(), "ollama");
}

#[test]
fn failover_collects_all_failures() {
    let mut reg = VecProviderRegistry::default();
    reg.register(record("openai", true)).unwrap();
    reg.register(record("ollama", false)).unwrap();
    let chain = reg.resolve_execution_chain().unwrap();

    let invoker = FlakyInvoker {
        succeed_on: "nobody",
    };
    let err =
        invoke_with_failover(&chain, &invoker, InvocationRequest::new("x".into())).unwrap_err();
    match err {
        ProviderError::AllProvidersFailed { tried } => assert_eq!(tried.len(), 2),
        other => panic!("unexpected error: {other}"),
    }
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
fn model_builder_capability_transition() {
    let rec = ModelBuilder::new()
        .id(ModelId::from("text-embedding-3-large"))
        .name(ModelName::from("text-embedding-3-large"))
        .embedding()
        .build_record()
        .unwrap();
    assert_eq!(rec.capability, harw_provider::ModelCapabilityTag::Embedding);
}
