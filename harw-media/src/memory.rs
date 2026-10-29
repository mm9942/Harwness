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
        let mut images = self.images.lock().map_err(|_| MediaError::Corrupt)?;
        // Hard resident-byte bound, including repeated model-initiated image reads.
        const MAX_RESIDENT_BYTES: usize = 64 * 1024 * 1024;
        let key = media.digest().to_string();
        if !images.contains_key(&key) {
            let mut resident: usize = images.values().map(Vec::len).sum();
            while resident.saturating_add(clean.len()) > MAX_RESIDENT_BYTES {
                let Some(old) = images.keys().next().cloned() else {
                    break;
                };
                if let Some(bytes) = images.remove(&old) {
                    resident = resident.saturating_sub(bytes.len());
                }
            }
            images.insert(key, clean);
        }
        Ok(media)
    }
}

impl MediaSource for MemorySource {
    fn load(&self, media: &MediaRef) -> Result<Vec<u8>, MediaError> {
        let images = self.images.lock().map_err(|_| MediaError::Corrupt)?;
        let bytes = images
            .get(&media.digest().to_string())
            .ok_or(MediaError::NotFound)?;
        if u64::try_from(bytes.len()).ok() != Some(media.bytes()) {
            return Err(MediaError::Corrupt);
        }
        let seen = crate::inspect::inspect(bytes)?;
        if seen.format != media.format()
            || seen.width != media.width()
            || seen.height != media.height()
        {
            return Err(MediaError::Corrupt);
        }
        Ok(bytes.clone())
    }
}
