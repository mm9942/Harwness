//! Media by reference: what a transcript item carries instead of image bytes.
//!
//! An image is stored once, content-addressed, by the media store
//! (`harw-media`). Items, events and session frames carry only a
//! [`MediaRef`]: the digest of the stored bytes plus the facts a client or a
//! provider adapter needs without loading them (format, size, dimensions).
//! This keeps transcripts, replay and the compact phone stream small, and it
//! keeps image bytes out of every log and wire frame that shows items.
//!
//! # Validation
//! A [`MediaRef`] is validated when it is built **and when it is
//! deserialized** (`#[serde(try_from)]`), so a persisted or received value
//! can never carry a zero size, an absurd dimension or an unknown format.
//! The store applies stricter policy limits when it ingests bytes; the limits
//! here only bound what a value may claim.

use std::fmt;

use harw_types::ContentDigest;
use serde::{Deserialize, Serialize};

/// Largest size a [`MediaRef`] may claim (256 MiB). The store's policy limit
/// is far lower; this only rejects values that cannot be real.
pub const MAX_CLAIMED_BYTES: u64 = 256 * 1024 * 1024;

/// Largest width or height a [`MediaRef`] may claim, in pixels.
pub const MAX_CLAIMED_EDGE: u32 = 65_535;

/// The image formats the harness accepts and forwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF (providers use the first frame).
    Gif,
    /// WebP.
    Webp,
}

impl ImageFormat {
    /// The IANA media type (`image/png`, …).
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }

    /// The file extension without a dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }

    /// The format of an IANA media type, if it is one of ours.
    #[must_use]
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/png" => Some(Self::Png),
            "image/jpeg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/webp" => Some(Self::Webp),
            _ => None,
        }
    }
}

/// How much detail a provider should spend on an image (OpenAI's `detail`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    /// Cheap, low resolution.
    Low,
    /// Full resolution.
    High,
    /// The provider decides.
    Auto,
}

impl ImageDetail {
    /// The value providers expect on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
            Self::Auto => "auto",
        }
    }
}

/// Why a [`MediaRef`] was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaRefError {
    /// The size is zero or above [`MAX_CLAIMED_BYTES`].
    Bytes,
    /// A dimension is zero or above [`MAX_CLAIMED_EDGE`].
    Dimensions,
}

impl fmt::Display for MediaRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bytes => "media size must be between 1 byte and 256 MiB",
            Self::Dimensions => "media width and height must be between 1 and 65535",
        })
    }
}

impl std::error::Error for MediaRefError {}

/// A stored image, by content: what transcripts and frames carry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawMediaRef", into = "RawMediaRef")]
pub struct MediaRef {
    digest: ContentDigest,
    format: ImageFormat,
    bytes: u64,
    width: u32,
    height: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMediaRef {
    digest: ContentDigest,
    format: ImageFormat,
    bytes: u64,
    width: u32,
    height: u32,
}

impl TryFrom<RawMediaRef> for MediaRef {
    type Error = MediaRefError;

    fn try_from(raw: RawMediaRef) -> Result<Self, Self::Error> {
        Self::new(raw.digest, raw.format, raw.bytes, raw.width, raw.height)
    }
}

impl From<MediaRef> for RawMediaRef {
    fn from(media: MediaRef) -> Self {
        Self {
            digest: media.digest,
            format: media.format,
            bytes: media.bytes,
            width: media.width,
            height: media.height,
        }
    }
}

impl MediaRef {
    /// Builds a validated reference.
    ///
    /// # Errors
    /// [`MediaRefError`] for a size or dimension no real image has.
    pub fn new(
        digest: ContentDigest,
        format: ImageFormat,
        bytes: u64,
        width: u32,
        height: u32,
    ) -> Result<Self, MediaRefError> {
        if bytes == 0 || bytes > MAX_CLAIMED_BYTES {
            return Err(MediaRefError::Bytes);
        }
        if width == 0 || height == 0 || width > MAX_CLAIMED_EDGE || height > MAX_CLAIMED_EDGE {
            return Err(MediaRefError::Dimensions);
        }
        Ok(Self {
            digest,
            format,
            bytes,
            width,
            height,
        })
    }

    /// The digest of the stored bytes.
    #[must_use]
    pub fn digest(&self) -> &ContentDigest {
        &self.digest
    }

    /// The image format.
    #[must_use]
    pub fn format(&self) -> ImageFormat {
        self.format
    }

    /// The media type, for example `image/png`.
    #[must_use]
    pub fn mime(&self) -> &'static str {
        self.format.mime()
    }

    /// The size of the stored bytes.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// A short, safe description for places that cannot show the image:
    /// no bytes, no path, no URL.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "image {}x{} {} ({} bytes)",
            self.width,
            self.height,
            self.format.mime(),
            self.bytes
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn digest() -> ContentDigest {
        ContentDigest::of(b"pixels")
    }

    fn sample() -> TestResult<MediaRef> {
        MediaRef::new(digest(), ImageFormat::Png, 1234, 640, 480)
            .map_err(|e| TestError::Unexpected(e.to_string()))
    }

    #[test]
    fn a_reference_round_trips_through_json() -> TestResult {
        let media = sample()?;
        let json =
            serde_json::to_string(&media).map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(json.contains("\"format\":\"png\""), "{json}");
        let back: MediaRef =
            serde_json::from_str(&json).map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert_eq!(back, media);
        Ok(())
    }

    #[test]
    fn impossible_claims_are_refused_when_built() {
        assert_eq!(
            MediaRef::new(digest(), ImageFormat::Png, 0, 1, 1),
            Err(MediaRefError::Bytes)
        );
        assert_eq!(
            MediaRef::new(digest(), ImageFormat::Png, MAX_CLAIMED_BYTES + 1, 1, 1),
            Err(MediaRefError::Bytes)
        );
        assert_eq!(
            MediaRef::new(digest(), ImageFormat::Png, 1, 0, 1),
            Err(MediaRefError::Dimensions)
        );
        assert_eq!(
            MediaRef::new(digest(), ImageFormat::Png, 1, 1, MAX_CLAIMED_EDGE + 1),
            Err(MediaRefError::Dimensions)
        );
    }

    #[test]
    fn impossible_claims_are_refused_when_deserialized() -> TestResult {
        let media = sample()?;
        let json =
            serde_json::to_string(&media).map_err(|e| TestError::Unexpected(e.to_string()))?;
        for bad in [
            json.replace("\"bytes\":1234", "\"bytes\":0"),
            json.replace("\"width\":640", "\"width\":0"),
            json.replace("\"height\":480", "\"height\":9999999"),
            json.replace("\"format\":\"png\"", "\"format\":\"bmp\""),
            json.replace('}', ",\"path\":\"/etc/passwd\"}"),
        ] {
            assert!(serde_json::from_str::<MediaRef>(&bad).is_err(), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn formats_know_their_media_type_and_back() {
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::Webp,
        ] {
            assert_eq!(ImageFormat::from_mime(format.mime()), Some(format));
        }
        assert_eq!(ImageFormat::from_mime("image/svg+xml"), None);
    }

    #[test]
    fn the_description_has_no_bytes_path_or_url() -> TestResult {
        let text = sample()?.describe();
        assert_eq!(text, "image 640x480 image/png (1234 bytes)");
        Ok(())
    }
}
