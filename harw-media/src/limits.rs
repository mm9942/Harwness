//! Policy limits for ingesting an image.

/// What the store accepts. The defaults sit inside the strictest provider
/// limits we send to: Anthropic takes 10 MB of base64 per image (7 MiB of raw
/// bytes is 9.8 MB encoded) and 8000 pixels per side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaLimits {
    /// Largest accepted input, in bytes.
    pub max_bytes: u64,
    /// Largest accepted width or height, in pixels.
    pub max_edge: u32,
    /// Largest accepted pixel count (width times height); bounds what a
    /// provider's decoder has to allocate.
    pub max_pixels: u64,
}

impl Default for MediaLimits {
    fn default() -> Self {
        Self {
            max_bytes: 7 * 1024 * 1024,
            max_edge: 8000,
            max_pixels: 40_000_000,
        }
    }
}
