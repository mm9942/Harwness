//! Wire adaptation for OpenAI's Sign in with ChatGPT plan usage preview.

use std::path::Path;
use std::time::Duration;

use harw_config::ProviderToml;
use harw_core::{ModelError, StreamSink};
use harw_oauth::chatgpt_plan::{ChatGptPlanError, ChatGptPlanStore};
use serde_json::{Value, json};

/// API-Name der ChatGPT-Plan-Route.
pub const API: &str = "openai-chatgpt-plan";
/// Feste Basis-URL der ChatGPT-Plan-Route.
pub const BASE_URL: &str = "https://api.openai.com/v1";
const TOOL_NAMESPACE: &str = "harw";

pub(crate) fn route(
    provider: &ProviderToml,
    home: Option<&Path>,
) -> crate::HttpProviderResult<Option<ChatGptPlanStore>> {
    if provider.api != API {
        return Ok(None);
    }
    if provider.base_url.trim_end_matches('/') != BASE_URL
        || provider.auth.is_some()
        || provider.api_key.is_some()
        || !matches!(provider.auth_header.as_deref(), None | Some("bearer"))
        || !provider.headers.is_empty()
        || provider.gateway_identity_headers
    {
        return Err(crate::HttpProviderError::Decode(
            "the ChatGPT plan provider requires the official OpenAI Responses endpoint and its own account connection".into(),
        ));
    }
    let home = home.ok_or_else(|| crate::HttpProviderError::MissingDefault {
        what: "Harw home for ChatGPT plan credentials".into(),
    })?;
    Ok(Some(ChatGptPlanStore::new(home)))
}

pub(crate) fn model_auth_error(error: ChatGptPlanError) -> ModelError {
    match error {
        ChatGptPlanError::OpenAiUnavailable => ModelError::Transient {
            status: None,
            retry_after_secs: None,
            message: error.to_string(),
        },
        _ => ModelError::Auth {
            message: error.to_string(),
        },
    }
}

pub(crate) fn prepare_body(body: &mut Value) {
    let Some(request) = body.as_object_mut() else {
        return;
    };
    request.insert("stream".into(), Value::Bool(true));
    request.insert("store".into(), Value::Bool(false));
    for key in [
        "background",
        "conversation",
        "max_output_tokens",
        "max_tool_calls",
        "metadata",
        "moderation",
        "multi_agent",
        "previous_response_id",
        "prompt",
        "prompt_cache_retention",
        "safety_identifier",
        "temperature",
        "top_logprobs",
        "top_p",
        "truncation",
        "user",
    ] {
        request.remove(key);
    }
    if let Some(Value::Array(tools)) = request.remove("tools")
        && !tools.is_empty()
    {
        request.insert(
            "tools".into(),
            json!([{
                "type": "namespace",
                "name": TOOL_NAMESPACE,
                "description": "Tools available in this Harw session",
                "tools": tools,
            }]),
        );
    }
    if let Some(input) = request.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            match item.get("type").and_then(Value::as_str) {
                Some("function_call" | "function_call_output") => {
                    item["namespace"] = Value::String(TOOL_NAMESPACE.into());
                }
                Some("message") if item.get("role").and_then(Value::as_str) == Some("system") => {
                    item["role"] = Value::String("developer".into());
                }
                _ => {}
            }
        }
    }
}

pub(crate) fn response_error(body: &Value) -> Option<ModelError> {
    let code = body
        .pointer("/response/error/code")
        .or_else(|| body.pointer("/error/code"))
        .or_else(|| body.get("code"))
        .and_then(Value::as_str)?;
    match code {
        "subscription_sharing_usage_limit_exceeded" => Some(ModelError::QuotaExceeded {
            message: "ChatGPT plan usage limit reached. Review usage at https://chatgpt.com/settings/usage".into(),
        }),
        "subscription_sharing_user_not_eligible" => Some(ModelError::Auth {
            message: "ChatGPT plan usage is unavailable for this account or workspace".into(),
        }),
        _ => None,
    }
}

pub(crate) async fn read_response(
    response: reqwest::Response,
    sink: Option<&StreamSink>,
    idle_timeout: Duration,
) -> Result<Value, ModelError> {
    let mut terminal = None;
    crate::sse::read_sse(response, Some(idle_timeout), |frame| {
        if frame.data.trim().is_empty() || frame.data.trim() == "[DONE]" {
            return Ok(false);
        }
        let event: Value = serde_json::from_str(&frame.data)?;
        let kind = event.get("type").and_then(Value::as_str);
        match kind {
            Some("response.failed" | "error") => {
                return Err(response_error(&event).unwrap_or_else(|| {
                    ModelError::RequestFailed(
                        "ChatGPT plan response stream reported a failure".into(),
                    )
                }));
            }
            Some("response.cancelled") => return Err(ModelError::Cancelled),
            _ => {}
        }
        let value = crate::sse::responses_event(&event, sink)?;
        if let Some(value) = value {
            if value
                .get("output")
                .and_then(Value::as_array)
                .is_some_and(|output| {
                    output.iter().any(|item| {
                        item.get("type").and_then(Value::as_str) == Some("function_call")
                            && item
                                .get("namespace")
                                .and_then(Value::as_str)
                                .is_some_and(|namespace| namespace != TOOL_NAMESPACE)
                    })
                })
            {
                return Err(ModelError::RequestFailed(
                    "ChatGPT plan returned an unexpected tool namespace".into(),
                ));
            }
            terminal = Some(value);
        }
        Ok(terminal.is_some())
    })
    .await?;
    terminal.ok_or_else(|| ModelError::Truncated {
        message: "ChatGPT plan response stream ended before its terminal event".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, BufRead, BufReader, Write as _};
    use std::net::TcpListener;

    #[test]
    fn prepares_a_stateless_stream_with_namespaced_tools_and_replayed_calls() {
        let mut body = json!({
            "model": "gpt-test",
            "input": [
                {"type": "message", "role": "system", "content": "Instructions"},
                {"type": "function_call", "name": "read", "call_id": "call_1", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "result"},
            ],
            "tools": [{"type": "function", "name": "read", "parameters": {"type": "object"}}],
            "previous_response_id": "resp_test",
            "store": true,
            "stream": false,
            "max_output_tokens": 500,
        });
        prepare_body(&mut body);
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["input"][0]["role"], "developer");
        assert_eq!(body["input"][1]["namespace"], "harw");
        assert_eq!(body["input"][2]["namespace"], "harw");
        assert_eq!(body["tools"][0]["type"], "namespace");
        assert_eq!(body["tools"][0]["tools"][0]["name"], "read");
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("previous_response_id").is_none());
    }

    #[test]
    fn classifies_subscription_exhaustion_as_nonretryable() {
        let error = response_error(&json!({
            "type": "response.failed",
            "response": {"error": {"code": "subscription_sharing_usage_limit_exceeded"}},
        }));
        assert!(matches!(error, Some(ModelError::QuotaExceeded { .. })));
    }

    #[tokio::test]
    async fn reads_the_terminal_response_without_a_live_stream_sink()
    -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = format!("http://{}/responses", listener.local_addr()?);
        let server = std::thread::spawn(move || -> io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut reader = BufReader::new(&mut stream);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line)?;
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let body = concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[",
                "{\"type\":\"function_call\",\"namespace\":\"harw\",\"name\":\"read\",\"call_id\":\"call_1\",\"arguments\":\"{}\"}]}}\n\n",
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        });
        let response = crate::http_client_for_endpoint(&endpoint)?
            .get(endpoint)
            .send()
            .await?;
        let response = read_response(response, None, Duration::from_secs(3)).await?;
        assert_eq!(response["status"], "completed");
        assert_eq!(response["output"][0]["call_id"], "call_1");
        server
            .join()
            .map_err(|_| io::Error::other("test stream server panicked"))??;
        Ok(())
    }
}
