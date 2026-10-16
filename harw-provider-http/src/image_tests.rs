//! Images through the real provider code: body shape per wire format, the
//! gate for models without image input, and a full round trip against a fake
//! vLLM-style server.

use super::*;
use crate::local_model_tests::{
    Script, chat_reply, config_with, fake_server, join_error, local_provider,
};
use crate::test_support::{TestError, TestResult, ctx};
use base64::Engine as _;
use harw_media::MemorySource;
use harw_protocol::{ImageDetail, MediaRef};
use std::sync::{Arc, OnceLock};

/// One media source for the whole test binary (it is process-wide in
/// production, too); images are content-addressed, so tests do not disturb
/// each other.
fn source() -> &'static Arc<MemorySource> {
    static SOURCE: OnceLock<Arc<MemorySource>> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let source = Arc::new(MemorySource::new());
        harw_core::install_media_source(source.clone());
        source
    })
}

fn png(width: u32, height: u32, marker: &[u8]) -> Vec<u8> {
    let chunk = |kind: &[u8; 4], data: &[u8]| {
        let mut out = u32::try_from(data.len())
            .unwrap_or(0)
            .to_be_bytes()
            .to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    };
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", marker));
    out.extend_from_slice(&chunk(b"IEND", b""));
    out
}

fn stored(marker: &[u8]) -> TestResult<(MediaRef, Vec<u8>)> {
    let media = source()
        .put(&png(40, 30, marker))
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    let bytes = harw_media::MediaSource::load(source().as_ref(), &media)
        .map_err(|e| TestError::Unexpected(e.to_string()))?;
    Ok((media, bytes))
}

fn request_with(text: &str, images: Vec<MediaRef>) -> ModelRequest {
    let mut history = harw_core::ConversationHistory::new();
    history.push_user_message(text, images);
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

fn decode(url: &str) -> TestResult<Vec<u8>> {
    let encoded = url
        .strip_prefix("data:image/png;base64,")
        .ok_or_else(|| TestError::Unexpected(format!("not a png data url: {url:.40}")))?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(ctx("base64"))
}

// --- Chat Completions ------------------------------------------------------

#[test]
fn chat_sends_the_image_as_a_data_url_before_the_text() -> TestResult {
    let (media, bytes) = stored(b"chat-marker")?;
    let request = request_with("what is this?", vec![media]);
    let body = build_chat_body(&request, "gpt-4o");
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0]["type"], "image_url");
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "what is this?");
    let url = content[0]["image_url"]["url"]
        .as_str()
        .ok_or(TestError::Missing("image url"))?;
    assert_eq!(decode(url)?, bytes, "the sanitized bytes, exactly");
    assert!(content[0]["image_url"].get("detail").is_none());
    Ok(())
}

#[test]
fn chat_passes_a_detail_preference() -> TestResult {
    let (media, _) = stored(b"detail-marker")?;
    let mut history = harw_core::ConversationHistory::new();
    history.push(harw_protocol::TurnItem::UserMessage(
        harw_protocol::UserMessageItem {
            id: harw_types::ItemId::new(),
            content: vec![
                harw_protocol::ContentPart::Media {
                    media,
                    detail: Some(ImageDetail::Low),
                },
                harw_protocol::ContentPart::Text {
                    text: "x".to_owned(),
                },
            ],
        },
    ));
    let mut request = request_with("", vec![]);
    request.history = history;
    let body = build_chat_body(&request, "gpt-4o");
    assert_eq!(
        body["messages"][0]["content"][0]["image_url"]["detail"],
        "low"
    );
    Ok(())
}

#[test]
fn a_message_without_images_keeps_its_old_shape() {
    let request = request_with("just text", vec![]);
    let body = build_chat_body(&request, "gpt-4o");
    assert_eq!(body["messages"][0]["content"], "just text");
}

#[test]
fn an_image_only_message_has_no_empty_text_part() -> TestResult {
    let (media, _) = stored(b"only-marker")?;
    let body = build_chat_body(&request_with("", vec![media]), "gpt-4o");
    let content = body["messages"][0]["content"]
        .as_array()
        .ok_or(TestError::Missing("content array"))?;
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "image_url");
    Ok(())
}

#[test]
fn an_image_the_store_lost_is_named_not_dropped() -> TestResult {
    let unknown = MediaRef::new(
        harw_types::ContentDigest::of(b"never ingested"),
        harw_protocol::ImageFormat::Png,
        10,
        4,
        4,
    )
    .map_err(|e| TestError::Unexpected(e.to_string()))?;
    let _ = source();
    let body = build_chat_body(&request_with("look", vec![unknown]), "gpt-4o");
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0]["type"], "text");
    let text = content[0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("image 4x4"), "{text}");
    assert!(text.contains("not sent"), "{text}");
    assert!(!body.to_string().contains("image_url"));
    Ok(())
}

#[test]
fn explicit_cache_markers_cope_with_a_content_array() -> TestResult {
    let (media, _) = stored(b"cache-marker")?;
    let mut body = build_chat_body(&request_with("describe", vec![media]), "m");
    cache_strategy::apply_chat_cache_control(
        &mut body,
        cache_strategy::CacheStrategy::ExplicitEphemeral { max_breakpoints: 4 },
    );
    let content = &body["messages"][0]["content"];
    assert_eq!(
        content[0]["type"], "image_url",
        "the image part is untouched"
    );
    assert!(content[0].get("cache_control").is_none());
    Ok(())
}

// --- Responses API ---------------------------------------------------------

#[test]
fn responses_sends_input_image_before_input_text() -> TestResult {
    let (media, bytes) = stored(b"responses-marker")?;
    let request = request_with("what is this?", vec![media]);
    let wire = serde_json::to_value(build_request("gpt-5", &request))
        .map_err(ctx("responses request json"))?;
    let content = &wire["input"][0]["content"];
    assert_eq!(content[0]["type"], "input_image");
    assert_eq!(content[1]["type"], "input_text");
    assert_eq!(
        decode(content[0]["image_url"].as_str().unwrap_or_default())?,
        bytes
    );
    assert!(content[0].get("detail").is_none());
    Ok(())
}

// --- Anthropic Messages ----------------------------------------------------

#[test]
fn anthropic_sends_a_base64_image_block_before_the_text() -> TestResult {
    let (media, bytes) = stored(b"anthropic-marker")?;
    let request = request_with("what is this?", vec![media]);
    let body = anthropic::build_messages_body("claude-sonnet-5-5", 1024, &request);
    let blocks = &body["messages"][0]["content"];
    assert_eq!(blocks[0]["type"], "image");
    assert_eq!(blocks[0]["source"]["type"], "base64");
    assert_eq!(blocks[0]["source"]["media_type"], "image/png");
    let data = blocks[0]["source"]["data"].as_str().unwrap_or_default();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(ctx("base64"))?,
        bytes
    );
    assert_eq!(
        blocks[1],
        serde_json::json!({"type": "text", "text": "what is this?"})
    );
    Ok(())
}

#[test]
fn anthropic_without_images_keeps_its_old_shape() {
    let body =
        anthropic::build_messages_body("claude-sonnet-5-5", 1024, &request_with("hi", vec![]));
    assert_eq!(body["messages"][0]["content"], "hi");
}

// --- The gate ---------------------------------------------------------------

fn config_with_model(imageless: bool) -> TestResult<harw_config::ResolvedConfig> {
    let mut config = harw_config::ResolvedConfig::default();
    let capabilities = if imageless {
        serde_json::json!({"image_input": false})
    } else {
        serde_json::json!({})
    };
    let model: harw_config::ModelToml = serde_json::from_value(serde_json::json!({
        "id": "text-only",
        "provider": "vllm",
        "aliases": ["text-only-alias"],
        "capabilities": capabilities,
    }))
    .map_err(ctx("model toml aus JSON"))?;
    config.models.insert(model.id.clone(), model);
    Ok(config)
}

#[test]
fn only_an_explicit_image_input_false_marks_a_model_imageless() -> TestResult {
    let marked = imageless_models("vllm", &config_with_model(true)?);
    assert!(marked.contains("text-only") && marked.contains("text-only-alias"));
    assert!(imageless_models("vllm", &config_with_model(false)?).is_empty());
    assert!(imageless_models("other", &config_with_model(true)?).is_empty());
    Ok(())
}

#[test]
fn an_imageless_model_gets_a_notice_with_the_size_not_the_bytes() -> TestResult {
    let (media, _) = stored(b"gate-marker")?;
    let mut request = request_with("what is this?", vec![media]);
    let imageless = imageless_models("vllm", &config_with_model(true)?);
    assert!(replace_images_for_imageless_model(
        &mut request,
        "text-only",
        &imageless
    ));
    let body = build_chat_body(&request, "text-only");
    let text = body["messages"][0]["content"].as_str().unwrap_or_default();
    assert!(text.contains("image 40x30"), "{text}");
    assert!(text.contains("this model does not accept images"), "{text}");
    assert!(!body.to_string().contains("image_url"));

    // Another model keeps its images.
    let (media, _) = stored(b"gate-marker-2")?;
    let mut other = request_with("x", vec![media]);
    assert!(!replace_images_for_imageless_model(
        &mut other,
        "vision-model",
        &imageless
    ));
    assert!(other.history.has_images());
    Ok(())
}

// --- Whole round trip -------------------------------------------------------

#[tokio::test]
async fn a_local_vision_model_receives_the_image_over_http() -> TestResult {
    let (media, bytes) = stored(b"roundtrip-marker")?;
    let (base_url, requests, server) =
        fake_server(vec![Script::json(&chat_reply("a small red square"))])?;
    let provider = local_provider("vllm", &format!("{base_url}/v1"), serde_json::json!({}))?;
    let backend = build_provider(&config_with(provider)).map_err(ctx("build provider"))?;
    let response = backend
        .respond(request_with("what do you see?", vec![media]))
        .await
        .map_err(ctx("respond"))?;
    assert_eq!(response.message.as_deref(), Some("a small red square"));
    let body = requests
        .recv()
        .map_err(ctx("fake server saw a request"))?
        .body
        .ok_or(TestError::Missing("chat body"))?;
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0]["type"], "image_url");
    let url = content[0]["image_url"]["url"].as_str().unwrap_or_default();
    assert_eq!(decode(url)?, bytes);
    assert_eq!(content[1]["text"], "what do you see?");
    server.join().map_err(join_error)??;
    Ok(())
}

#[tokio::test]
async fn a_model_without_image_input_is_sent_the_notice_over_http() -> TestResult {
    let (media, _) = stored(b"http-gate-marker")?;
    let (base_url, requests, server) =
        fake_server(vec![Script::json(&chat_reply("I cannot see images"))])?;
    let provider = local_provider("vllm", &format!("{base_url}/v1"), serde_json::json!({}))?;
    let mut config = config_with(provider);
    let model: harw_config::ModelToml = serde_json::from_value(serde_json::json!({
        "id": "local-model",
        "provider": "vllm",
        "capabilities": {"image_input": false},
    }))
    .map_err(ctx("model toml aus JSON"))?;
    config.models.insert(model.id.clone(), model);
    let backend = build_provider(&config).map_err(ctx("build provider"))?;
    backend
        .respond(request_with("what do you see?", vec![media]))
        .await
        .map_err(ctx("respond"))?;
    let body = requests
        .recv()
        .map_err(ctx("fake server saw a request"))?
        .body
        .ok_or(TestError::Missing("chat body"))?;
    assert!(!body.to_string().contains("image_url"), "{body}");
    let text = body["messages"][0]["content"].as_str().unwrap_or_default();
    assert!(text.contains("image 40x30"), "{text}");
    assert!(text.contains("this model does not accept images"), "{text}");
    assert!(text.contains("what do you see?"), "{text}");
    server.join().map_err(join_error)??;
    Ok(())
}
