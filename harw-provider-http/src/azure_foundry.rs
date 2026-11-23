//! Token limits for the Foundry Chat preset's verified deployment names.

use harw_sandbox::egress::EgressUrl;

/// Published token limits for one Azure AI Foundry Chat deployment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AzureFoundryChatTokenLimits {
    /// Total context window in tokens.
    pub context_window: u32,
    /// Maximum generated tokens, including any reasoning tokens.
    pub max_output_tokens: u32,
}

/// Looks up verified limits for the canonical Azure AI Foundry Chat preset.
///
/// A deployment is not a global model alias: require the exact route ID and
/// the preset's HTTPS host/path, and match the full deployment name (ignoring
/// ASCII case only). Custom deployments keep the usual configured limits.
#[must_use]
pub fn azure_foundry_chat_token_limits(
    route_id: &str,
    base_url: &str,
    deployment: &str,
) -> Option<AzureFoundryChatTokenLimits> {
    if route_id != "azure-foundry-chat" {
        return None;
    }
    let endpoint = EgressUrl::parse(base_url).ok()?;
    let url = endpoint.as_url();
    if !endpoint.is_https()
        || !endpoint.host_str().ends_with(".services.ai.azure.com")
        || url.path().trim_end_matches('/') != "/openai/v1"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }

    const DEPLOYMENTS: &[(&str, u32, u32)] = &[
        ("grok-4.6", 200_000, 128_000),
        ("grok-4-20-non-reasoning", 262_000, 8_192),
        ("mistral-large-4-latest", 128_000, 4_096),
        ("DeepSeek-V4.1-Flash-1017", 1_000_000, 128_000),
        ("Kimi-K3.1-Pro", 1_048_576, 262_144),
        ("Kimi-K2.7-Code", 262_144, 262_144),
        ("grok-4.3", 200_000, 8_192),
    ];
    DEPLOYMENTS
        .iter()
        .find(|(id, _, _)| deployment.eq_ignore_ascii_case(id))
        .map(
            |&(_, context_window, max_output_tokens)| AzureFoundryChatTokenLimits {
                context_window,
                max_output_tokens,
            },
        )
}
