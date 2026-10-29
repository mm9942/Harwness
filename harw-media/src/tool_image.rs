//! Typed descriptor emitted exclusively by the sandboxed `fs.view_image` tool.
use harw_protocol::{ImageDetail, MediaRef};
use serde::{Deserialize, Serialize};

/// Stored image for native multimodal input; contains no bytes or source path.
/// Consumers must also verify the originating tool call and resolve the media.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageToolResult {
    /// The image is ready for a vision-capable model request.
    ImageInspection {
        /// Verified, sanitized image in the media store.
        media: MediaRef,
        /// Requested provider detail level.
        detail: Option<ImageDetail>,
    },
}

impl ImageToolResult {
    /// The stored image and provider detail preference.
    #[must_use]
    pub fn image(&self) -> (&MediaRef, Option<ImageDetail>) {
        match self {
            Self::ImageInspection { media, detail } => (media, *detail),
        }
    }
}
