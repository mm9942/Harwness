//! Runde 7, Teil L: Tests für lokale Modell-Server (vLLM, LM Studio, Ollama)
//! gegen Fake-HTTP-Server auf Loopback.
//!
//! Abgedeckt: Auth ohne Schlüssel, Kompatibilitätsschalter im Chat-Body,
//! Streaming-Leerlauf-Zeitlimit statt Gesamtzeit, keine Timeout-Wiederholung
//! bei lokalen Providern, LAN-`http` nur mit Opt-in, werkzeuglose Modelle,
//! `message.reasoning` und die neuen Text-Tool-Call-Formate, LM-Studio-
//! Kontextfenster in der Discovery.

use super::*;
use crate::test_support::{TestError, TestResult, ctx};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// Eine vom Fake-Server beobachtete Anfrage (Kopfzeilen kleingeschrieben).
struct Observed {
    head: String,
    body: Option<Value>,
}

/// Antwort-Skript einer Verbindung: Teilstücke mit Wartezeit davor, danach
/// wird die Verbindung noch `hold` lang offen gehalten.
struct Script {
    parts: Vec<(Duration, Vec<u8>)>,
    hold: Duration,
}

impl Script {
    /// Vollständige JSON-Antwort ohne Verzögerung.
    fn json(body: &Value) -> Self {
        let body = body.to_string();
        let raw = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        Self {
            parts: vec![(Duration::ZERO, raw.into_bytes())],
            hold: Duration::ZERO,
        }
    }
}

type FakeServer = (
    String,
    Receiver<Observed>,
    thread::JoinHandle<TestResult<()>>,
);

/// Startet einen Fake-Server, der je Verbindung genau ein [`Script`] abspielt.
fn fake_server(scripts: Vec<Script>) -> TestResult<FakeServer> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind fake server"))?;
    let base_url = format!(
        "http://{}",
        listener.local_addr().map_err(ctx("fake server address"))?
    );
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || -> TestResult<()> {
        for script in scripts {
            let (mut stream, _) = listener.accept().map_err(ctx("accept fake request"))?;
            let observed = read_request(&mut stream)?;
            sender.send(observed).map_err(ctx("report fake request"))?;
            for (delay, bytes) in script.parts {
                thread::sleep(delay);
                // Ein abgebrochener Client (Timeout) ist hier erwartet.
                if stream.write_all(&bytes).is_err() || stream.flush().is_err() {
                    break;
                }
            }
            thread::sleep(script.hold);
        }
        Ok(())
    });
    Ok((base_url, receiver, handle))
}

/// Liest eine HTTP/1.1-Anfrage samt `content-length`-Body.
fn read_request(stream: &mut std::net::TcpStream) -> TestResult<Observed> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).map_err(ctx("read fake request"))?;
        if read == 0 {
            return Err(TestError::Unexpected(
                "client closed connection before headers completed".to_owned(),
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
    let content_length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut buffer).map_err(ctx("read fake body"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = if content_length == 0 {
        None
    } else {
        let end = (header_end + content_length).min(bytes.len());
        Some(serde_json::from_slice(&bytes[header_end..end]).map_err(ctx("fake request JSON"))?)
    };
    Ok(Observed { head, body })
}

fn join_error(_payload: Box<dyn std::any::Any + Send>) -> TestError {
    TestError::Unexpected("fake server thread panicked".to_owned())
}

/// Provider-Datei wie vom Katalog-Seed bzw. `harw provider add --no-auth`.
fn local_provider(
    name: &str,
    base_url: &str,
    extra: Value,
) -> TestResult<harw_config::ProviderToml> {
    let mut value = serde_json::json!({
        "name": name,
        "api": "openai-chat",
        "base_url": base_url,
        "models": ["local-model"],
    });
    if let (Some(target), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        for (key, field) in extra {
            target.insert(key.clone(), field.clone());
        }
    }
    serde_json::from_value(value).map_err(ctx("provider toml aus JSON"))
}

fn config_with(provider: harw_config::ProviderToml) -> harw_config::ResolvedConfig {
    let mut config = harw_config::ResolvedConfig::default();
    config.harness.default_provider = Some(provider.name.clone());
    config.harness.default_model = Some("local-model".to_owned());
    config.providers.insert(provider.name.clone(), provider);
    config
}

fn plain_request() -> ModelRequest {
    let mut history = harw_core::ConversationHistory::new();
    history.push_user_text("Hallo");
    ModelRequest {
        stream: None,
        system_prompt: String::new(),
        instruction_fragments: Vec::new(),
        context: Vec::new(),
        history,
        tools: Vec::new(),
        context_assembly: Default::default(),
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

fn tool(name: &str) -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "path".to_owned(),
        harw_tools::JsonSchema {
            schema_type: Some(harw_tools::JsonSchemaType::String),
            ..Default::default()
        },
    );
    ToolSpec::Function(harw_tools::FunctionToolSpec {
        name: ToolName::new(name),
        description: "Liest eine Datei".to_owned(),
        parameters: harw_tools::JsonSchema {
            schema_type: Some(harw_tools::JsonSchemaType::Object),
            properties: Some(properties),
            ..Default::default()
        },
        strict: true,
    })
}

fn chat_reply(content: &str) -> Value {
    serde_json::json!({"choices": [{"message": {"content": content}, "finish_reason": "stop"}]})
}

// ── L1: Auth ohne Schlüssel ────────────────────────────────────────────────

#[tokio::test]
async fn seeded_lmstudio_and_vllm_build_without_key_and_send_no_auth_header() -> TestResult {
    // lmstudio: wie geseedet (`auth_header = "none"`); vllm: ganz ohne
    // auth_header — die Loopback-Vorgabe greift.
    for (name, extra) in [
        ("lmstudio", serde_json::json!({"auth_header": "none"})),
        ("vllm", serde_json::json!({})),
    ] {
        let (base_url, requests, server) = fake_server(vec![Script::json(&chat_reply("ok"))])?;
        let provider = local_provider(name, &format!("{base_url}/v1"), extra)?;
        let config = config_with(provider);
        let backend =
            build_provider(&config).map_err(ctx("local provider must build without key"))?;
        backend
            .respond(plain_request())
            .await
            .map_err(ctx("local respond"))?;
        let observed = requests.recv().map_err(ctx("fake server saw a request"))?;
        assert!(
            !observed.head.contains("authorization:"),
            "{name}: no auth header expected, got {}",
            observed.head
        );
        server.join().map_err(join_error)??;
    }
    Ok(())
}

#[test]
fn local_provider_with_explicit_auth_keeps_bearer_default() -> TestResult {
    let provider = local_provider(
        "vllm",
        "http://localhost:8000/v1",
        serde_json::json!({"auth": "env:VLLM_API_KEY"}),
    )?;
    assert_eq!(default_auth_header("vllm", &provider), "bearer");
    let keyless = local_provider("vllm", "http://localhost:8000/v1", serde_json::json!({}))?;
    assert_eq!(default_auth_header("vllm", &keyless), "none");
    let cloud = local_provider(
        "gateway",
        "https://gateway.example/v1",
        serde_json::json!({}),
    )?;
    assert_eq!(default_auth_header("gateway", &cloud), "bearer");
    Ok(())
}

// ── L6: Kompatibilitätsschalter ────────────────────────────────────────────

#[tokio::test]
async fn local_chat_body_uses_max_tokens_without_strict_or_reasoning_effort() -> TestResult {
    let (base_url, requests, server) = fake_server(vec![Script::json(&chat_reply("ok"))])?;
    let provider = local_provider("vllm", &format!("{base_url}/v1"), serde_json::json!({}))?;
    let config = config_with(provider);
    let backend = build_provider(&config).map_err(ctx("build local provider"))?;
    let mut request = plain_request();
    request.tools = vec![tool("fs.read")];
    request.max_output_tokens = Some(512);
    request.reasoning_effort = Some(harw_types::ReasoningEffort::High);
    backend
        .respond(request)
        .await
        .map_err(ctx("local respond"))?;
    let body = requests
        .recv()
        .map_err(ctx("fake server saw a request"))?
        .body
        .ok_or(TestError::Missing("chat body"))?;
    assert_eq!(body["max_tokens"], 512);
    assert!(body.get("max_completion_tokens").is_none());
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["parallel_tool_calls"], false);
    assert!(body["tools"][0]["function"].get("strict").is_none());
    server.join().map_err(join_error)??;
    Ok(())
}

#[test]
fn explicit_compat_switches_shape_the_chat_body() -> TestResult {
    let provider = local_provider(
        "lmstudio",
        "http://localhost:1234/v1",
        serde_json::json!({
            "max_tokens_field": "both",
            "send_reasoning_effort": true,
            "strict_tools": true,
            "parallel_tool_calls": true,
        }),
    )?;
    let mut request = plain_request();
    request.tools = vec![tool("fs.read")];
    request.max_output_tokens = Some(64);
    request.reasoning_effort = Some(harw_types::ReasoningEffort::Low);
    let body = build_chat_body_with(&request, "m", ChatCompat::from_provider(&provider));
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["max_completion_tokens"], 64);
    assert_eq!(body["reasoning_effort"], "low");
    assert_eq!(body["parallel_tool_calls"], true);
    assert_eq!(body["tools"][0]["function"]["strict"], true);

    // Cloud-Vorgabe bleibt exakt wie bisher.
    let cloud = build_chat_body_with(&request, "m", ChatCompat::default());
    assert_eq!(cloud, build_chat_body(&request, "m"));
    assert!(cloud.get("max_tokens").is_none());
    assert!(cloud.get("parallel_tool_calls").is_none());
    Ok(())
}

// ── L4: Zeitlimits ─────────────────────────────────────────────────────────

const SSE_HEAD: &str =
    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";

fn sse_delta(text: &str) -> Vec<u8> {
    let chunk =
        serde_json::json!({"id": "c1", "choices": [{"index": 0, "delta": {"content": text}}]});
    format!("data: {chunk}\n\n").into_bytes()
}

fn streaming_provider(base_url: &str) -> TestResult<OpenAiResponsesProvider> {
    let provider = local_provider("vllm", &format!("{base_url}/v1"), serde_json::json!({}))?;
    let config = config_with(provider.clone());
    let sources = SecretSources {
        env_layer: &config.env_layer,
        resolver: None,
        home: None,
        endpoint: None,
    };
    OpenAiResponsesProvider::from_named_config("vllm", &provider, &config, "local-model", sources)
        .map_err(ctx("build streaming provider"))
}

fn streaming_request() -> (ModelRequest, std::sync::Arc<Mutex<String>>) {
    let seen = std::sync::Arc::new(Mutex::new(String::new()));
    let sink_seen = std::sync::Arc::clone(&seen);
    let mut request = plain_request();
    request.stream = Some(harw_core::StreamSink::new(move |event| {
        if let harw_core::ModelStreamEvent::TextDelta(text) = event
            && let Ok(mut guard) = sink_seen.lock()
        {
            guard.push_str(&text);
        }
    }));
    (request, seen)
}

#[tokio::test]
async fn stalled_stream_hits_idle_timeout() -> TestResult {
    let (base_url, _requests, server) = fake_server(vec![Script {
        parts: vec![
            (Duration::ZERO, SSE_HEAD.as_bytes().to_vec()),
            (Duration::ZERO, sse_delta("Hal")),
        ],
        hold: Duration::from_secs(2),
    }])?;
    let mut provider = streaming_provider(&base_url)?;
    provider.stream_idle_timeout = Duration::from_millis(300);
    let (request, seen) = streaming_request();
    let started = std::time::Instant::now();
    let Err(error) = provider.respond(request).await else {
        return Err(TestError::Unexpected(
            "stalled stream must time out".to_owned(),
        ));
    };
    assert!(matches!(error, ModelError::Timeout { .. }), "{error}");
    assert!(started.elapsed() < Duration::from_millis(1_800));
    assert_eq!(seen.lock().map(|g| g.clone()).unwrap_or_default(), "Hal");
    server.join().map_err(join_error)??;
    Ok(())
}

#[tokio::test]
async fn steady_stream_outlives_request_timeout() -> TestResult {
    let step = Duration::from_millis(150);
    let (base_url, _requests, server) = fake_server(vec![Script {
        parts: vec![
            (Duration::ZERO, SSE_HEAD.as_bytes().to_vec()),
            (step, sse_delta("a")),
            (step, sse_delta("b")),
            (step, sse_delta("c")),
            (step, sse_delta("d")),
            (step, b"data: [DONE]\n\n".to_vec()),
        ],
        hold: Duration::ZERO,
    }])?;
    let mut provider = streaming_provider(&base_url)?;
    // Gesamtdauer ~750 ms > request_timeout, aber nie 400 ms ohne Bytes.
    provider.request_timeout = Duration::from_millis(400);
    provider.stream_idle_timeout = Duration::from_millis(400);
    let (request, seen) = streaming_request();
    let response = provider
        .respond(request)
        .await
        .map_err(ctx("steady stream must complete"))?;
    assert_eq!(response.message.as_deref(), Some("abcd"));
    assert_eq!(seen.lock().map(|g| g.clone()).unwrap_or_default(), "abcd");
    server.join().map_err(join_error)??;
    Ok(())
}

#[tokio::test]
async fn local_provider_does_not_retry_timeouts() -> TestResult {
    // Der Server antwortet nie; mit Wiederholung käme nach >= 5 s Backoff
    // ein zweiter Versuch.
    let (base_url, _requests, server) = fake_server(vec![Script {
        parts: Vec::new(),
        hold: Duration::from_secs(2),
    }])?;
    let provider = local_provider(
        "vllm",
        &format!("{base_url}/v1"),
        serde_json::json!({"request_timeout_secs": 1}),
    )?;
    assert!(!provider.effective_retry_timeouts());
    let backend = build_provider(&config_with(provider)).map_err(ctx("build local provider"))?;
    let started = std::time::Instant::now();
    let Err(error) = backend.respond(plain_request()).await else {
        return Err(TestError::Unexpected(
            "silent server must time out".to_owned(),
        ));
    };
    assert!(matches!(error, ModelError::Timeout { .. }), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "timeout must not be retried for local providers"
    );
    server.join().map_err(join_error)??;
    Ok(())
}

#[test]
fn provider_timeouts_come_from_provider_toml() -> TestResult {
    let provider = streaming_provider("http://127.0.0.1:9")?;
    assert_eq!(provider.request_timeout, Duration::from_secs(600));
    assert_eq!(provider.stream_idle_timeout, Duration::from_secs(120));
    let policy = network_retry_policy_for(&local_provider(
        "cloud",
        "https://gateway.example/v1",
        serde_json::json!({}),
    )?);
    assert!(policy.retry_timeouts);
    Ok(())
}

// ── L8: LAN nur mit Opt-in ─────────────────────────────────────────────────

#[test]
fn lan_http_requires_explicit_opt_in() -> TestResult {
    let lan = "http://192.168.1.10:8000/v1";
    let Err(error) = validate_endpoint_with(lan, false) else {
        return Err(TestError::Unexpected(
            "LAN http without opt-in must fail".to_owned(),
        ));
    };
    assert!(error.to_string().contains("allow_insecure_lan"));
    assert!(!error.to_string().contains(lan));
    validate_endpoint_with(lan, true).map_err(ctx("LAN http with opt-in"))?;
    validate_endpoint_with("http://10.0.0.5/v1", true).map_err(ctx("10/8 with opt-in"))?;
    // Öffentliche Hosts bleiben auch mit Opt-in https-pflichtig.
    assert!(validate_endpoint_with("http://api.example.com/v1", true).is_err());
    assert!(validate_endpoint_with("http://gpu-box.lan:8000/v1", true).is_err());

    let provider = local_provider("gpu", lan, serde_json::json!({}))?;
    assert!(build_provider(&config_with(provider)).is_err());
    let provider = local_provider("gpu", lan, serde_json::json!({"allow_insecure_lan": true}))?;
    build_provider(&config_with(provider)).map_err(ctx("LAN provider with opt-in builds"))?;
    Ok(())
}

#[test]
fn loopback_client_builds_without_proxy() -> TestResult {
    http_client_for_endpoint("http://localhost:1234/v1").map_err(ctx("loopback client"))?;
    http_client_for_endpoint("https://api.example.com/v1").map_err(ctx("cloud client"))?;
    Ok(())
}

// ── L7: Werkzeuglose Modelle, Reasoning, Text-Tool-Calls ───────────────────

#[test]
fn toolless_model_is_offered_no_tools_but_a_notice() -> TestResult {
    let mut config = harw_config::ResolvedConfig::default();
    let model: harw_config::ModelToml = serde_json::from_value(serde_json::json!({
        "id": "tiny",
        "provider": "lmstudio",
        "aliases": ["tiny-alias"],
        "capabilities": {"tool_calling": false},
    }))
    .map_err(ctx("model toml aus JSON"))?;
    config.models.insert("tiny".to_owned(), model);
    let toolless = toolless_models("lmstudio", &config);
    assert!(toolless.contains("tiny") && toolless.contains("tiny-alias"));
    assert!(toolless_models("other", &config).is_empty());

    let mut request = plain_request();
    request.tools = vec![tool("fs.read")];
    assert!(strip_tools_for_toolless_model(
        &mut request,
        "tiny-alias",
        &toolless
    ));
    assert!(request.tools.is_empty());
    assert!(
        request
            .instruction_fragments
            .iter()
            .any(|fragment| fragment.contains("keine Werkzeugaufrufe"))
    );

    let mut request = plain_request();
    request.tools = vec![tool("fs.read")];
    assert!(!strip_tools_for_toolless_model(
        &mut request,
        "big",
        &toolless
    ));
    assert_eq!(request.tools.len(), 1);
    Ok(())
}

#[test]
fn interpret_chat_reads_non_streamed_message_reasoning() -> TestResult {
    let body = serde_json::json!({"choices": [{
        "message": {"content": "42", "reasoning": "rechne 6*7"},
        "finish_reason": "stop"
    }]});
    let response = interpret_chat(&body, "vllm", "m", &[]).map_err(ctx("interpret"))?;
    let reasoning = response.reasoning.ok_or(TestError::Missing("reasoning"))?;
    assert_eq!(reasoning.blocks[0]["text"], "rechne 6*7");
    Ok(())
}

#[test]
fn interpret_chat_parses_new_text_tool_call_formats_fail_closed() -> TestResult {
    let mistral = serde_json::json!({"choices": [{
        "message": {"content": "[TOOL_CALLS]fs.read[ARGS]{\"path\":\"a\"}"},
        "finish_reason": "stop"
    }]});
    let response = interpret_chat(&mistral, "vllm", "m", &["fs.read"]).map_err(ctx("mistral"))?;
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name.as_str(), "fs.read");
    assert!(matches!(response.stop, StopReason::ToolUse));
    assert_eq!(response.message, None);

    let fenced = serde_json::json!({"choices": [{
        "message": {"content": "```json\n{\"name\":\"fs.read\",\"arguments\":{\"path\":\"b\"}}\n```"},
        "finish_reason": "stop"
    }]});
    let response = interpret_chat(&fenced, "vllm", "m", &["fs.read"]).map_err(ctx("fenced"))?;
    assert_eq!(
        response.tool_calls[0].arguments,
        serde_json::json!({"path": "b"})
    );

    // Unbekanntes Werkzeug: Text bleibt Text.
    let unknown = serde_json::json!({"choices": [{
        "message": {"content": "<|python_tag|>{\"name\":\"shell.exec\",\"parameters\":{}}"},
        "finish_reason": "stop"
    }]});
    let response = interpret_chat(&unknown, "vllm", "m", &["fs.read"]).map_err(ctx("unknown"))?;
    assert!(response.tool_calls.is_empty());
    assert!(response.message.is_some());
    Ok(())
}

// ── L3: LM-Studio-Kontextfenster in der Discovery ──────────────────────────

#[tokio::test]
async fn discovery_merges_lmstudio_context_lengths() -> TestResult {
    let (base_url, requests, server) = fake_server(vec![
        Script::json(
            &serde_json::json!({"data": [{"id": "qwen3-8b"}, {"id": "vllm-style", "max_model_len": 40960}]}),
        ),
        Script::json(
            &serde_json::json!({"data": [{"id": "qwen3-8b", "max_context_length": 32768}]}),
        ),
    ])?;
    let provider = local_provider("lmstudio", &format!("{base_url}/v1"), serde_json::json!({}))?;
    let models = discovery::list_models("lmstudio", &provider, None)
        .await
        .map_err(ctx("list local models"))?;
    assert_eq!(models[0].context_length, Some(32768));
    assert_eq!(models[1].context_length, Some(40960));
    let first = requests.recv().map_err(ctx("models request"))?;
    assert!(first.head.starts_with("get /v1/models"));
    let second = requests.recv().map_err(ctx("lmstudio request"))?;
    assert!(second.head.starts_with("get /api/v0/models"));
    server.join().map_err(join_error)??;
    Ok(())
}

// ── L5: Warten auf einen Modell-Slot ───────────────────────────────────────

#[tokio::test]
async fn limiter_reports_callers_waiting_for_a_slot() -> TestResult {
    let limiter = std::sync::Arc::new(DynamicConcurrencyLimiter::new(Some(1)));
    let first = limiter.acquire().await.map_err(ctx("first permit"))?;
    assert_eq!(limiter.waiting(), 0);
    let waiter_limiter = std::sync::Arc::clone(&limiter);
    let waiter = tokio::spawn(async move { waiter_limiter.acquire().await.map(|_permit| ()) });
    for _ in 0..100 {
        if limiter.waiting() == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(limiter.waiting(), 1, "second caller must wait for the slot");
    drop(first);
    waiter
        .await
        .map_err(ctx("join waiter"))?
        .map_err(ctx("second permit"))?;
    assert_eq!(limiter.waiting(), 0);
    Ok(())
}
