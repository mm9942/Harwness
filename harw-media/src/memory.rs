//! An in-memory media source, for tests and for embedders without a disk.

use std::collections::HashMap;
use std::sync::Mutex;

use harw_protocol::MediaRef;

use crate::error::MediaError;
use crate::limits::MediaLimits;
use crate::store::{MediaSource, ingest};

/// Images held in memory. It ingests exactly like the disk store (format
/// check, limits, sanitizing), so what a test gets is what production gets.
#[derive(Debug, Default)]
pub struct MemorySource {
    images: Mutex<HashMap<String, Vec<u8>>>,
    limits: MediaLimits,
}

impl MemorySource {
    /// An empty source with the default limits.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingests an image and returns its reference.
    ///
    /// # Errors
    /// See [`crate::MediaStore::put`].
    pub fn put(&self, bytes: &[u8]) -> Result<MediaRef, MediaError> {
        let (media, clean) = ingest(bytes, self.limits)?;
        if let Ok(mut images) = self.images.lock() {
            images.insert(media.digest().to_string(), clean);
        }
        Ok(media)
    }
}

impl MediaSource for MemorySource {
    fn load(&self, media: &MediaRef) -> Result<Vec<u8>, MediaError> {
        let images = self.images.lock().map_err(|_| MediaError::Corrupt)?;
        images
            .get(&media.digest().to_string())
            .cloned()
            .ok_or(MediaError::NotFound)
    }
}
