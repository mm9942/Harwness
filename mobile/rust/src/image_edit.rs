//! Explicit, device-local OpenAI image editing. This module does not mount UI,
//! import into a chat or vault, or use the existing transcript media resolver.
//!
//! The caller presents the one selected source and exact edit instruction, obtains
//! consent, and then calls `edit_confirmed_synchronous` with the authenticated configuration
//! snapshot and its route-scoped key resolver. No fallback, redirects, automatic
//! retries, or provider-returned URL fetches are performed.

use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::{Decompress, FlushDecompress, Status};
use reqwest::{Client, Request, header, multipart};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, de::IgnoredAny};
use thiserror::Error;

const OPENAI_EDIT_ENDPOINT: &str = "https://api.openai.com/v1/images/edits";
const OPENAI_EDIT_MODEL: &str = "gpt-image-1.5";
const OUTPUT_EDGE: u32 = 1024;
const MAX_INPUT_EDGE: u32 = 1536;
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 6 * 1024 * 1024;
const MAX_EDIT_INSTRUCTION_BYTES: usize = 4 * 1024;
const MAX_BASE64_BYTES: usize = MAX_OUTPUT_BYTES.div_ceil(3) * 4;
const MAX_EDIT_RESPONSE_BYTES: usize = MAX_BASE64_BYTES + 8192;
const MAX_PNG_CHUNKS: usize = 4_096;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Internal classification without identifiers, keys, request details or image bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ImageEditError {
    #[error("the selected image-edit provider or model is not configured")]
    RouteNotEligible,
    #[error("the confirmed image-edit route changed")]
    RouteChanged,
    #[error("the edit instruction is blank, too long, or contains unsupported control characters")]
    InvalidInstruction,
    #[error("the edited image is not a supported bounded still PNG")]
    InvalidImage,
    #[error("the image bytes ended before a complete PNG was received")]
    IncompleteImage,
    #[error("the selected provider credential is unavailable")]
    MissingCredential,
    #[error("the image-edit request could not be constructed")]
    BuildRequest,
    #[error("the provider did not return a successful image-edit response")]
    ProviderRefused,
    #[error("the image-edit transport failed; delivery may be unknown")]
    UnknownDelivery,
    #[error("the provider image-edit response ended before completion")]
    ResponseInterrupted,
    #[error("the provider reply is not application/json")]
    ResponseType,
    #[error("the provider reply exceeds the image-edit size limit")]
    ResponseTooLarge,
    #[error("the provider reply does not describe one inline edited image")]
    InvalidResponse,
}

/// Trusted adapter view of this device's existing provider config. `revision`
/// must change for route, model, or credential changes. It is not a user field.
#[derive(Clone, Copy)]
pub struct ConfiguredEditRoute<'a> {
    pub route_id: &'a str,
    pub api: &'a str,
    pub base_url: &'a str,
    pub available_models: &'a [String],
    pub enabled: bool,
    pub revision: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ImageEditRoute {
    route_id: String,
    revision: u64,
}

impl ImageEditRoute {
    /// Specifically allowlisted edit model; chat vision input alone does not grant editing.
    pub fn select(
        configured: &ConfiguredEditRoute<'_>,
        selected_route: &str,
        selected_model: &str,
    ) -> Result<Self, ImageEditError> {
        let valid_id = !selected_route.is_empty()
            && selected_route.len() <= 64
            && selected_route.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            });
        if !valid_id
            || configured.route_id != selected_route
            || !configured.enabled
            || configured.api != "openai-responses"
            || !matches!(
                configured.base_url,
                "https://api.openai.com/v1" | "https://api.openai.com/v1/"
            )
            || selected_model != OPENAI_EDIT_MODEL
            || !configured
                .available_models
                .iter()
                .any(|name| name == selected_model)
        {
            return Err(ImageEditError::RouteNotEligible);
        }
        Ok(Self {
            route_id: selected_route.to_owned(),
            revision: configured.revision,
        })
    }

    pub fn endpoint(&self) -> &'static str {
        OPENAI_EDIT_ENDPOINT
    }

    pub fn model(&self) -> &'static str {
        OPENAI_EDIT_MODEL
    }
}

/// Immutable single-image draft for preview/approval. Never derives Debug.
pub struct PreparedImageEdit {
    route: ImageEditRoute,
    source: Vec<u8>,
    instruction: String,
}

impl PreparedImageEdit {
    pub fn prepare(
        route: ImageEditRoute,
        source_png: &[u8],
        instruction: &str,
    ) -> Result<Self, ImageEditError> {
        if instruction.len() > MAX_EDIT_INSTRUCTION_BYTES
            || instruction.trim().is_empty()
            || instruction
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err(ImageEditError::InvalidInstruction);
        }
        let source = checked_png(source_png, PngProfile::Input)?;
        Ok(Self {
            route,
            source,
            instruction: instruction.trim().to_owned(),
        })
    }

    pub fn route(&self) -> &ImageEditRoute {
        &self.route
    }

    /// These are the exact sanitized bytes which may be sent after confirmation.
    pub fn source(&self) -> &[u8] {
        &self.source
    }

    pub fn instruction(&self) -> &str {
        &self.instruction
    }
}

/// A validated, caller-owned result. The digest is integrity metadata, not a
/// capability, URL or permission to access transcript media.
pub struct EditedImage {
    bytes: Vec<u8>,
    digest: blake3::Hash,
}

impl EditedImage {
    pub fn mime(&self) -> &'static str {
        "image/png"
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (OUTPUT_EDGE, OUTPUT_EDGE)
    }

    #[cfg(test)]
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn digest(&self) -> blake3::Hash {
        self.digest
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

fn build_request(
    approved: PreparedImageEdit,
    current: &ConfiguredEditRoute<'_>,
    resolve_key: impl FnOnce(&str) -> Result<SecretString, ImageEditError>,
) -> Result<(Client, Request), ImageEditError> {
    let selected =
        ImageEditRoute::select(current, &approved.route.route_id, approved.route.model())
            .map_err(|_| ImageEditError::RouteChanged)?;
    if selected != approved.route {
        return Err(ImageEditError::RouteChanged);
    }
    let key = resolve_key(&selected.route_id).map_err(|_| ImageEditError::MissingCredential)?;
    if key.expose_secret().trim().is_empty() || key.expose_secret().len() > 4096 {
        return Err(ImageEditError::MissingCredential);
    }
    let client = Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|_| ImageEditError::BuildRequest)?;
    let part = multipart::Part::bytes(approved.source)
        .file_name("selected.png")
        .mime_str("image/png")
        .map_err(|_| ImageEditError::BuildRequest)?;
    let form = multipart::Form::new()
        .text("model", selected.model())
        .text("prompt", approved.instruction)
        .text("n", "1")
        .text("size", "1024x1024")
        .text("output_format", "png")
        .text("stream", "false")
        .part("image", part);
    let request = client
        .post(selected.endpoint())
        .header(header::ACCEPT, "application/json")
        .bearer_auth(key.expose_secret())
        .multipart(form)
        .build()
        .map_err(|_| ImageEditError::BuildRequest)?;
    Ok((client, request))
}

/// Execute exactly one user-confirmed, previously previewed edit, explicitly
/// using a synchronous result. No streaming-first caller should automatically
/// fall back to this route. The caller's resolver must resolve the already configured **API-key slot** by route ID;
/// neither cookies nor general ChatGPT credentials are accepted by this contract.
/// A transport error is not consent to retry, even on the same route.
pub async fn edit_confirmed_synchronous(
    approved: PreparedImageEdit,
    current: &ConfiguredEditRoute<'_>,
    resolve_key: impl FnOnce(&str) -> Result<SecretString, ImageEditError>,
) -> Result<EditedImage, ImageEditError> {
    let (client, request) = build_request(approved, current, resolve_key)?;
    let mut response = client
        .execute(request)
        .await
        .map_err(|_| ImageEditError::UnknownDelivery)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(ImageEditError::ProviderRefused);
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|content| content.to_str().ok())
        .filter(|content| response_is_json(content))
        .ok_or(ImageEditError::ResponseType)?
        .to_owned();
    let mut body = Vec::new();
    if response
        .content_length()
        .is_some_and(|bytes| bytes > MAX_EDIT_RESPONSE_BYTES as u64)
    {
        return Err(ImageEditError::ResponseTooLarge);
    }
    while let Some(part) = response
        .chunk()
        .await
        .map_err(|_| ImageEditError::ResponseInterrupted)?
    {
        append_response_chunk(&mut body, &part)?;
    }
    decode_response(&content_type, &body)
}

fn append_response_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), ImageEditError> {
    if chunk.len() > MAX_EDIT_RESPONSE_BYTES.saturating_sub(body.len()) {
        return Err(ImageEditError::ResponseTooLarge);
    }
    body.extend_from_slice(chunk);
    Ok(())
}

#[derive(Deserialize)]
struct ImageResponse<'a> {
    #[serde(borrow)]
    data: [ImageEntry<'a>; 1],
    error: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct ImageEntry<'a> {
    #[serde(borrow)]
    b64_json: &'a str,
    url: Option<IgnoredAny>,
}

fn response_is_json(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
}

fn decode_response(content_type: &str, body: &[u8]) -> Result<EditedImage, ImageEditError> {
    if !response_is_json(content_type) {
        return Err(ImageEditError::ResponseType);
    }
    if body.len() > MAX_EDIT_RESPONSE_BYTES {
        return Err(ImageEditError::ResponseTooLarge);
    }
    let image: ImageResponse<'_> = serde_json::from_slice(body).map_err(|error| {
        if error.is_eof() {
            ImageEditError::ResponseInterrupted
        } else {
            ImageEditError::InvalidResponse
        }
    })?;
    let [image_entry] = image.data;
    if image.error.is_some() || image_entry.url.is_some() {
        return Err(ImageEditError::InvalidResponse);
    }
    if image_entry.b64_json.len() > MAX_BASE64_BYTES {
        return Err(ImageEditError::ResponseTooLarge);
    }
    let decoded = STANDARD
        .decode(image_entry.b64_json)
        .map_err(|_| ImageEditError::InvalidResponse)?;
    let bytes = checked_png(&decoded, PngProfile::Output)?;
    Ok(EditedImage {
        digest: blake3::hash(&bytes),
        bytes,
    })
}

#[derive(Clone, Copy)]
enum PngProfile {
    Input,
    Output,
}

/// The first slice deliberately implements a narrow still-PNG profile: strict
/// framing, CRC and one complete zlib stream, 8-bit RGB/RGBA, no interlacing,
/// palette, APNG, non-alpha transparency, external data, or trailing payload.
fn checked_png(bytes: &[u8], profile: PngProfile) -> Result<Vec<u8>, ImageEditError> {
    let fail = ImageEditError::InvalidImage;
    let byte_cap = match profile {
        PngProfile::Input => MAX_SOURCE_BYTES,
        PngProfile::Output => MAX_OUTPUT_BYTES,
    };
    if bytes.len() > byte_cap || !bytes.starts_with(PNG_MAGIC) {
        return Err(fail);
    }
    let mut keep = PNG_MAGIC.to_vec();
    let mut at = PNG_MAGIC.len();
    let mut filtered_stride = 0usize;
    let mut rows = 0usize;
    let mut compressed = Vec::new();
    let mut data_seen = false;
    let mut data_ended = false;
    for chunk_index in 0..MAX_PNG_CHUNKS {
        let header = bytes
            .get(at..at + 8)
            .ok_or(ImageEditError::IncompleteImage)?;
        let size = u32::from_be_bytes(header[..4].try_into().map_err(|_| fail)?) as usize;
        let kind: &[u8; 4] = header[4..].try_into().map_err(|_| fail)?;
        if !kind.iter().all(u8::is_ascii_alphabetic) || !kind[2].is_ascii_uppercase() {
            return Err(fail);
        }
        let data_end = at
            .checked_add(8)
            .and_then(|n| n.checked_add(size))
            .ok_or(fail)?;
        let end = data_end.checked_add(4).ok_or(fail)?;
        let current = bytes.get(at..end).ok_or(ImageEditError::IncompleteImage)?;
        let data = &bytes[at + 8..data_end];
        let crc = u32::from_be_bytes(bytes[data_end..end].try_into().map_err(|_| fail)?);
        if crc32fast::hash(&bytes[at + 4..data_end]) != crc {
            return Err(fail);
        }
        if chunk_index == 0 && kind != b"IHDR" {
            return Err(fail);
        }
        match kind {
            b"IHDR" if chunk_index == 0 && size == 13 => {
                let width = u32::from_be_bytes(data[..4].try_into().map_err(|_| fail)?);
                let height = u32::from_be_bytes(data[4..8].try_into().map_err(|_| fail)?);
                let channels = match data[8..] {
                    [8, 2, 0, 0, 0] => 3,
                    [8, 6, 0, 0, 0] => 4,
                    _ => return Err(fail),
                };
                let dimensions_allowed = match profile {
                    PngProfile::Input => {
                        (1..=MAX_INPUT_EDGE).contains(&width)
                            && (1..=MAX_INPUT_EDGE).contains(&height)
                    }
                    PngProfile::Output => width == OUTPUT_EDGE && height == OUTPUT_EDGE,
                };
                if !dimensions_allowed {
                    return Err(fail);
                }
                filtered_stride = width as usize * channels + 1;
                rows = height as usize;
                keep.extend_from_slice(current);
            }
            b"IDAT" if !data_ended && filtered_stride > 0 => {
                data_seen = true;
                compressed.extend_from_slice(data);
                keep.extend_from_slice(current);
            }
            b"IEND" if size == 0 && end == bytes.len() && data_seen && !compressed.is_empty() => {
                // An extra byte gives a clear signal when inflated data exceeds the expected rows.
                let raw_bytes = filtered_stride * rows;
                let mut filtered = vec![0; raw_bytes + 1];
                let mut decoder = Decompress::new(true);
                let status = decoder
                    .decompress(&compressed, &mut filtered, FlushDecompress::Finish)
                    .map_err(|_| fail)?;
                if status != Status::StreamEnd && decoder.total_out() <= raw_bytes as u64 {
                    return Err(ImageEditError::IncompleteImage);
                }
                if status != Status::StreamEnd
                    || decoder.total_in() != compressed.len() as u64
                    || decoder.total_out() != raw_bytes as u64
                    || filtered[..raw_bytes]
                        .chunks_exact(filtered_stride)
                        .any(|row| row[0] > 4)
                {
                    return Err(fail);
                }
                keep.extend_from_slice(current);
                return Ok(keep);
            }
            b"acTL" | b"fcTL" | b"fdAT" | b"tRNS" => return Err(fail),
            _ if kind[0].is_ascii_lowercase() => {
                // Drop ancillary metadata and prevent a later noncontiguous IDAT.
                data_ended |= data_seen;
            }
            _ => return Err(fail),
        }
        at = end;
    }
    Err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(body);
        let mut crc = crc32fast::Hasher::new();
        crc.update(tag);
        crc.update(body);
        out.extend_from_slice(&crc.finalize().to_be_bytes());
    }

    fn png(w: u32, h: u32, filter: u8, ancillary: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // RGBA8, non-interlaced
        chunk(&mut out, b"IHDR", &ihdr);
        if let Some((tag, data)) = ancillary {
            chunk(&mut out, tag, data);
        }
        let mut row = vec![127; 1 + w as usize * 4];
        row[0] = filter;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        for _ in 0..h {
            encoder.write_all(&row).unwrap();
        }
        chunk(&mut out, b"IDAT", &encoder.finish().unwrap());
        chunk(&mut out, b"IEND", &[]);
        out
    }

    fn body(png: &[u8]) -> Vec<u8> {
        serde_json::to_vec(
            &serde_json::json!({"created": 123,"data":[{"b64_json": STANDARD.encode(png)}]}),
        )
        .unwrap()
    }

    fn config<'a>(models: &'a [String]) -> ConfiguredEditRoute<'a> {
        ConfiguredEditRoute {
            route_id: "configured-route",
            api: "openai-responses",
            base_url: "https://api.openai.com/v1/",
            available_models: models,
            enabled: true,
            revision: 9,
        }
    }

    #[test]
    fn only_the_explicitly_configured_official_route_and_model_can_be_selected() {
        let models = vec!["gpt-image-1.5".into(), "gpt-5".into()];
        let current = config(&models);
        assert!(ImageEditRoute::select(&current, "configured-route", "gpt-image-1.5").is_ok());
        assert_eq!(
            ImageEditRoute::select(&current, "other", "gpt-image-1.5").err(),
            Some(ImageEditError::RouteNotEligible)
        );
        assert_eq!(
            ImageEditRoute::select(&current, "configured-route", "gpt-5").err(),
            Some(ImageEditError::RouteNotEligible)
        );
        for invalid in [
            ConfiguredEditRoute {
                base_url: "https://api.openai.com.example.test/v1",
                ..current
            },
            ConfiguredEditRoute {
                api: "anthropic-messages",
                ..current
            },
            ConfiguredEditRoute {
                enabled: false,
                ..current
            },
            ConfiguredEditRoute {
                available_models: &[],
                ..current
            },
        ] {
            assert_eq!(
                ImageEditRoute::select(&invalid, "configured-route", "gpt-image-1.5").err(),
                Some(ImageEditError::RouteNotEligible)
            );
        }
    }

    #[test]
    fn request_has_explicit_post_destination_and_rejects_a_changed_approval_before_transport() {
        let models = vec!["gpt-image-1.5".into()];
        let current = config(&models);
        let route = ImageEditRoute::select(&current, "configured-route", "gpt-image-1.5").unwrap();
        let source = png(3, 2, 0, Some((b"tEXt", b"user\0not-for-provider")));
        let make =
            || PreparedImageEdit::prepare(route.clone(), &source, "Make the shapes blue").unwrap();
        let prepared = make();
        assert!(
            !prepared
                .source
                .windows(16)
                .any(|w| w == b"not-for-provider")
        );
        let key = SecretString::from("fixture-not-a-secret".to_owned());
        let (client, request) = build_request(prepared, &current, |id| {
            assert_eq!(id, "configured-route");
            Ok(key)
        })
        .unwrap();
        drop(client);
        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(
            request.url().as_str(),
            "https://api.openai.com/v1/images/edits"
        );
        assert!(
            request.headers()[reqwest::header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("multipart/form-data; boundary=")
        );
        assert!(request.headers()[reqwest::header::AUTHORIZATION].is_sensitive());
        let changed = ConfiguredEditRoute {
            revision: 10,
            ..current
        };
        let err = build_request(make(), &changed, |_| {
            panic!("stale selection must not resolve a key")
        })
        .err();
        assert_eq!(err, Some(ImageEditError::RouteChanged));
    }

    #[test]
    fn malformed_edit_inputs_fail_before_building_a_request() {
        let models = vec!["gpt-image-1.5".into()];
        let route =
            ImageEditRoute::select(&config(&models), "configured-route", "gpt-image-1.5").unwrap();
        let src = png(3, 2, 0, None);
        for prompt in ["", "   ", "private\0control"] {
            assert_eq!(
                PreparedImageEdit::prepare(route.clone(), &src, prompt).err(),
                Some(ImageEditError::InvalidInstruction)
            );
        }
        assert_eq!(
            PreparedImageEdit::prepare(route.clone(), b"<svg/>", "edit").err(),
            Some(ImageEditError::InvalidImage)
        );
        assert_eq!(
            PreparedImageEdit::prepare(route, &src, &"x".repeat(MAX_EDIT_INSTRUCTION_BYTES + 1))
                .err(),
            Some(ImageEditError::InvalidInstruction)
        );
    }

    #[test]
    fn real_raster_bytes_are_returned_and_metadata_is_not_kept() {
        let input = png(1024, 1024, 0, Some((b"tEXt", b"owner\0private-metadata")));
        let edited = decode_response("application/json; charset=utf-8", &body(&input)).unwrap();
        assert_eq!(edited.mime(), "image/png");
        assert_eq!(edited.dimensions(), (1024, 1024));
        let expected = png(1024, 1024, 0, None);
        assert_eq!(edited.bytes(), expected);
        assert_eq!(edited.digest(), blake3::hash(&expected));
    }

    #[test]
    fn reply_never_accepts_text_a_url_multiple_images_or_the_wrong_content_type() {
        for bad in [
            &br#"{"data":[]}"#[..],
            br#"{"data":[{"url":"https://download.example.test/image.png"}]}"#,
            br#"{"data":[{"b64_json":"aGVsbG8="},{"b64_json":"aGVsbG8="}]}"#,
            br#"{"data":[{"b64_json":"aGVsbG8=","url":"https://other.invalid"}]}"#,
            br#"{"data":[{"b64_json":"not-base-64!"}]}"#,
            br#"{"data":[{"b64_json":"aGVsbG8="}]}"#,
        ] {
            assert!(decode_response("application/json", bad).is_err());
        }
        assert_eq!(
            decode_response("text/event-stream", b"").err(),
            Some(ImageEditError::ResponseType)
        );
        assert_eq!(
            decode_response("application/json", &vec![b' '; MAX_EDIT_RESPONSE_BYTES + 1]).err(),
            Some(ImageEditError::ResponseTooLarge)
        );
    }

    #[test]
    fn incremental_http_body_read_rejects_a_crossing_chunk_before_appending() {
        let mut body = vec![0; MAX_EDIT_RESPONSE_BYTES - 1];
        assert_eq!(
            append_response_chunk(&mut body, &[6, 7]),
            Err(ImageEditError::ResponseTooLarge)
        );
        assert_eq!(body.len(), MAX_EDIT_RESPONSE_BYTES - 1);
        assert_eq!(append_response_chunk(&mut body, &[6]), Ok(()));
        assert_eq!(body.len(), MAX_EDIT_RESPONSE_BYTES);
    }

    #[test]
    fn incomplete_json_and_incomplete_png_are_not_classified_as_generic_errors() {
        assert_eq!(
            decode_response("application/json", br#"{"data":[{"b64_json":"#).err(),
            Some(ImageEditError::ResponseInterrupted),
        );
        let good = png(1024, 1024, 0, None);
        assert_eq!(
            decode_response("application/json", &body(&good[..good.len() - 1])).err(),
            Some(ImageEditError::IncompleteImage),
        );
    }

    #[test]
    fn valid_dimensions_do_not_make_a_corrupt_animated_or_incomplete_image_renderable() {
        let good = png(1024, 1024, 0, None);
        let mut crc = good.clone();
        crc[30] ^= 1;
        let mut trailing = good.clone();
        trailing.push(0);
        for bad in [
            png(200, 200, 0, None),   // does not match fixed requested output
            png(1024, 1024, 5, None), // invalid filter after successful zlib inflation
            png(1024, 1024, 0, Some((b"acTL", &[0; 8]))),
            crc,
            trailing,
        ] {
            assert_eq!(
                decode_response("application/json", &body(&bad)).err(),
                Some(ImageEditError::InvalidImage)
            );
        }
    }
}
