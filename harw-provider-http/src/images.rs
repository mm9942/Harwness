//! Images in the three wire formats we speak.
//!
//! Every adapter gets a user message as `(text, images)` from
//! `ConversationHistory::to_model_messages` and has to turn it into its own
//! shape:
//!
//! - **Chat Completions** (OpenAI, Mistral, vLLM, Ollama's compatible API,
//!   most gateways): `content` is a list of `{"type":"text"}` and
//!   `{"type":"image_url","image_url":{"url":"data:…","detail":…}}` parts.
//! - **Responses API**: `{"type":"input_image","image_url":"data:…"}`.
//! - **Anthropic Messages**: `{"type":"image","source":{"type":"base64",…}}`.
//!
//! Rules that hold for all of them:
//! - Only bytes the harness holds are sent, as a `data:` URL or base64. A URL
//!   a third party would have to fetch is never produced.
//! - Images go **before** the text (Anthropic measures better that way; the
//!   others do not care).
//! - A message without images keeps its old shape (a plain string), so
//!   requests that do not use images are byte-identical to before.
//! - An image whose bytes are unavailable is **not dropped**: a text part
//!   names it and says it was not sent.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use harw_core::ModelImage;
use serde_json::{Value, json};

/// Why an image the harness holds could not be loaded, for the placeholder.
const UNAVAILABLE: &str = "the image could not be loaded from the media store";

/// `data:image/png;base64,…` for a loaded image, `None` if it has no bytes.
pub(crate) fn data_url(image: &ModelImage) -> Option<String> {
    let bytes = image.bytes()?;
    Some(format!(
        "data:{};base64,{}",
        image.media.mime(),
        STANDARD.encode(bytes)
    ))
}

/// The `content` of a Chat Completions user message.
pub(crate) fn chat_user_content(text: String, images: &[ModelImage]) -> Value {
    if images.is_empty() {
        return Value::String(text);
    }
    let mut parts: Vec<Value> = images
        .iter()
        .map(|image| match data_url(image) {
            Some(url) => {
                let mut part = json!({ "url": url });
                if let Some(detail) = image.detail {
                    part["detail"] = Value::from(detail.as_str());
                }
                json!({ "type": "image_url", "image_url": part })
            }
            None => json!({ "type": "text", "text": image.placeholder(UNAVAILABLE) }),
        })
        .collect();
    if !text.is_empty() {
        parts.push(json!({ "type": "text", "text": text }));
    }
    Value::Array(parts)
}

/// The content parts of a Responses API user message.
pub(crate) fn responses_user_content(
    text: String,
    images: &[ModelImage],
) -> Vec<harw_provider::openai::ContentPart> {
    use harw_provider::openai::ContentPart;
    let mut parts: Vec<ContentPart> = images
        .iter()
        .map(|image| match data_url(image) {
            Some(image_url) => ContentPart::InputImage {
                image_url,
                detail: image.detail.map(|d| d.as_str().to_owned()),
            },
            None => ContentPart::InputText {
                text: image.placeholder(UNAVAILABLE),
            },
        })
        .collect();
    if !text.is_empty() || parts.is_empty() {
        parts.push(ContentPart::InputText { text });
    }
    parts
}

/// The `content` of an Anthropic user message.
pub(crate) fn anthropic_user_content(text: String, images: &[ModelImage]) -> Value {
    if images.is_empty() {
        return Value::String(text);
    }
    let mut blocks: Vec<Value> = images
        .iter()
        .map(|image| match image.bytes() {
            Some(bytes) => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.media.mime(),
                    "data": STANDARD.encode(bytes),
                },
            }),
            None => json!({ "type": "text", "text": image.placeholder(UNAVAILABLE) }),
        })
        .collect();
    if !text.is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    Value::Array(blocks)
}

/// The reason given in the text that replaces an image for a model that does
/// not accept images.
pub(crate) const IMAGELESS_REASON: &str = "this model does not accept images";
