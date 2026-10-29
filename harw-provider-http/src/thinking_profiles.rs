//! Verified Chat Completions thinking options, scoped to official routes.
//!
//! Sources (2026-10-29): https://docs.z.ai/guides/llm/glm-5.3,
//! https://docs.z.ai/guides/capabilities/thinking-mode,
//! https://docs.z.ai/guides/tools/stream-tool, and
//! https://api-docs.deepseek.com/guides/thinking_mode.
//! A matching model name on a proxy does not establish its wire contract.

use harw_core::ModelError;
use harw_types::ReasoningEffort;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    Glm53,
    DeepSeek,
}

fn profile(base_url: &str, model: &str) -> Option<Profile> {
    let url = reqwest::Url::parse(base_url).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return None;
    }
    let path = url.path().trim_end_matches('/');
    match (url.host_str()?, path, model) {
        ("api.z.ai", "/api/paas/v4" | "/api/coding/paas/v4", "glm-5.3") => Some(Profile::Glm53),
        ("api.deepseek.com", "" | "/v1", "deepseek-flash" | "deepseek-v4-pro") => {
            Some(Profile::DeepSeek)
        }
        _ => None,
    }
}

/// Whether the official route documents replay of assistant reasoning content.
pub(crate) fn supports_reasoning_content(base_url: &str, model: &str) -> bool {
    profile(base_url, model).is_some()
}

/// Apply documented options after generic Chat Completions serialization.
///
/// Unrecognized routes are untouched. GLM's documented wire levels are
/// low/high/max. The adapter rounds generic intermediate levels upward:
/// minimal -> low, medium -> high, xhigh -> max. This local normalization
/// preserves the generic ordering without changing the configured effort.
/// DeepSeek instead uses the aliases explicitly documented by its API.
/// An absent effort preserves the provider's default (GLM max, DeepSeek high).
pub(crate) fn apply_options(
    body: &mut Value,
    base_url: &str,
    model: &str,
    effort: Option<ReasoningEffort>,
    stream: bool,
    has_tools: bool,
) -> Result<(), ModelError> {
    let Some(profile) = profile(base_url, model) else {
        return Ok(());
    };
    let wire_effort = match (profile, effort) {
        (_, None) => None,
        (_, Some(ReasoningEffort::Low)) => Some("low"),
        (_, Some(ReasoningEffort::High)) => Some("high"),
        (_, Some(ReasoningEffort::Max)) => Some("max"),
        (_, Some(ReasoningEffort::Minimal)) => Some("low"),
        (_, Some(ReasoningEffort::Medium)) => Some("high"),
        (Profile::DeepSeek, Some(ReasoningEffort::Xhigh)) => Some("high"),
        (Profile::Glm53, Some(ReasoningEffort::Xhigh)) => Some("max"),
    };
    let object = body.as_object_mut().ok_or_else(|| {
        ModelError::RequestFailed("thinking options require a JSON request object".to_owned())
    })?;
    let thinking = object.entry("thinking").or_insert_with(|| json!({}));
    let thinking = thinking.as_object_mut().ok_or_else(|| {
        ModelError::RequestFailed("thinking options require a JSON thinking object".to_owned())
    })?;
    thinking.insert("type".to_owned(), Value::from("enabled"));
    if let Some(effort) = wire_effort {
        object.insert("reasoning_effort".to_owned(), Value::from(effort));
    } else {
        object.remove("reasoning_effort");
    }
    match profile {
        Profile::Glm53 => {
            if stream && has_tools {
                object.insert("tool_stream".to_owned(), Value::Bool(true));
            }
        }
        Profile::DeepSeek => {
            // The official API accepts but ignores these in thinking mode.
            for field in ["temperature", "presence_penalty", "frequency_penalty"] {
                object.remove(field);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_routes_only() {
        for endpoint in [
            "https://proxy.example/api/paas/v4",
            "https://api.z.ai.proxy.example/api/paas/v4",
            "http://api.z.ai/api/paas/v4",
            "https://api.z.ai:8443/api/paas/v4",
            "https://api.z.ai/other",
            "https://api.z.ai/api/v1",
            "https://api.z.ai/api/paas/v4?route=other",
            "https://user@api.z.ai/api/paas/v4",
        ] {
            assert_eq!(profile(endpoint, "glm-5.3"), None, "{endpoint}");
        }
        assert_eq!(
            profile("https://api.z.ai/api/coding/paas/v4/", "glm-5.3"),
            Some(Profile::Glm53)
        );
        assert_eq!(profile("https://api.z.ai/api/paas/v4", "glm-5.2"), None);
        assert_eq!(
            profile("https://api.deepseek.com/v1", "deepseek-flash"),
            Some(Profile::DeepSeek)
        );
    }

    #[test]
    fn glm53_preserves_exact_supported_effort_and_streams_tools() -> Result<(), ModelError> {
        for effort in [
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ] {
            let mut body = json!({});
            apply_options(
                &mut body,
                "https://api.z.ai/api/paas/v4",
                "glm-5.3",
                Some(effort),
                true,
                true,
            )?;
            assert_eq!(body["thinking"]["type"], "enabled");
            assert_eq!(body["reasoning_effort"], effort.to_string());
            assert_eq!(body["tool_stream"], true);
        }
        Ok(())
    }

    #[test]
    fn glm53_rounds_generic_intermediate_efforts_upward() -> Result<(), ModelError> {
        for (effort, expected) in [
            (ReasoningEffort::Minimal, "low"),
            (ReasoningEffort::Medium, "high"),
            (ReasoningEffort::Xhigh, "max"),
        ] {
            let mut body = json!({"model": "glm-5.3"});
            apply_options(
                &mut body,
                "https://api.z.ai/api/paas/v4",
                "glm-5.3",
                Some(effort),
                false,
                false,
            )?;
            assert_eq!(body["reasoning_effort"], expected);
        }
        Ok(())
    }

    #[test]
    fn absent_effort_leaves_vendor_default_and_tool_stream_requires_streaming_tools()
    -> Result<(), ModelError> {
        for (stream, tools) in [(false, false), (false, true), (true, false)] {
            let mut body = json!({});
            apply_options(
                &mut body,
                "https://api.z.ai/api/paas/v4",
                "glm-5.3",
                None,
                stream,
                tools,
            )?;
            assert!(body.get("reasoning_effort").is_none());
            assert!(body.get("tool_stream").is_none());
        }
        Ok(())
    }

    #[test]
    fn deepseek_uses_documented_aliases_and_removes_ignored_sampling() -> Result<(), ModelError> {
        for (effort, expected) in [
            (ReasoningEffort::Minimal, "low"),
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "high"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::Xhigh, "high"),
            (ReasoningEffort::Max, "max"),
        ] {
            let mut body =
                json!({"temperature": 0.2, "presence_penalty": 1, "frequency_penalty": 1});
            apply_options(
                &mut body,
                "https://api.deepseek.com",
                "deepseek-flash",
                Some(effort),
                true,
                true,
            )?;
            assert_eq!(body["reasoning_effort"], expected);
            assert!(body.get("temperature").is_none());
            assert!(body.get("presence_penalty").is_none());
            assert!(body.get("frequency_penalty").is_none());
            assert!(body.get("tool_stream").is_none());
        }
        Ok(())
    }

    #[test]
    fn unknown_route_preserves_request_exactly() -> Result<(), ModelError> {
        let mut body = json!({"reasoning_effort": "medium", "temperature": 0.2});
        let original = body.clone();
        apply_options(
            &mut body,
            "https://proxy.example/v1",
            "deepseek-flash",
            Some(ReasoningEffort::High),
            true,
            true,
        )?;
        assert_eq!(body, original);
        Ok(())
    }
}
