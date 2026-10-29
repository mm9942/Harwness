//! Shared media sources for tool execution and model request projection.
use crate::{MediaError, MediaSource};
use harw_protocol::MediaRef;
use std::sync::{Arc, RwLock};

static SOURCES: RwLock<Vec<Arc<dyn MediaSource>>> = RwLock::new(Vec::new());

/// Registers a verified source, retaining at most sixteen process sources.
pub fn install_media_source(source: Arc<dyn MediaSource>) {
    if let Ok(mut sources) = SOURCES.write() {
        if sources.iter().any(|known| Arc::ptr_eq(known, &source)) {
            return;
        }
        if sources.len() >= 16 {
            sources.remove(0);
        }
        sources.push(source);
    }
}

/// Loads through registered sources. References never grant filesystem access.
///
/// # Errors
/// Returns the final source error when no source can supply verified bytes.
pub fn load_installed_media(media: &MediaRef) -> Result<Vec<u8>, MediaError> {
    let sources = SOURCES.read().map_err(|_| MediaError::Corrupt)?.clone();
    let mut last = MediaError::NotFound;
    for source in sources {
        match source.load(media) {
            Ok(bytes) => return Ok(bytes),
            Err(error) => last = error,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_the_same_source_does_not_duplicate_it() {
        let source: Arc<dyn MediaSource> = Arc::new(crate::MemorySource::new());
        install_media_source(Arc::clone(&source));
        install_media_source(Arc::clone(&source));
        let count = SOURCES.read().map_or(0, |sources| {
            sources
                .iter()
                .filter(|known| Arc::ptr_eq(known, &source))
                .count()
        });
        assert_eq!(count, 1);
    }
}
