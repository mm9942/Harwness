//! Images in a model request: from [`MediaRef`] to bytes.
//!
//! Transcripts carry only [`MediaRef`]s. A provider adapter needs bytes, and it
//! reaches them through [`ModelMessage::User::images`]: when the history is
//! projected for a request ([`ConversationHistory::to_model_messages`]), every
//! `Media` part is resolved through the **process media source** installed with
//! [`install_media_source`]. The runtime installs the media store once at
//! start-up; tests install a map.
//!
//! An image the source cannot supply (no source installed, entry missing or
//! damaged) is **not dropped**: it stays in the message with `data: None`, and
//! the adapter renders an explicit placeholder so the model and the user can
//! see that something was there and was not sent.
//!
//! Resolved images are cached up to [`CACHE_BYTES`], because a conversation
//! re-projects its whole history for every request and the same pictures would
//! otherwise be read and verified from disk on each turn.
//!
//! [`ModelMessage::User::images`]: crate::history::ModelMessage::User
//! [`ConversationHistory::to_model_messages`]: crate::history::ConversationHistory::to_model_messages

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use harw_media::MediaSource;
use harw_protocol::{ImageDetail, MediaRef};

/// Upper bound for the bytes the resolver keeps in memory.
pub const CACHE_BYTES: usize = 64 * 1024 * 1024;

/// An image of a user message, as a provider adapter sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelImage {
    /// What the transcript holds.
    pub media: MediaRef,
    /// The sender's detail preference, if any.
    pub detail: Option<ImageDetail>,
    /// The verified bytes, or `None` if the source could not supply them.
    pub data: Option<Arc<[u8]>>,
}

impl ModelImage {
    /// The bytes, if they are there.
    #[must_use]
    pub fn bytes(&self) -> Option<&[u8]> {
        self.data.as_deref()
    }

    /// What to show in place of an image that cannot be sent: it says what it
    /// was, never where it came from.
    #[must_use]
    pub fn placeholder(&self, reason: &str) -> String {
        format!("[{} not sent: {reason}]", self.media.describe())
    }
}

static SOURCE: RwLock<Option<Arc<dyn MediaSource>>> = RwLock::new(None);
static CACHE: Mutex<Cache> = Mutex::new(Cache::new());

struct Cache {
    entries: Option<HashMap<String, Arc<[u8]>>>,
    order: VecDeque<String>,
    bytes: usize,
}

impl Cache {
    const fn new() -> Self {
        Self {
            entries: None,
            order: VecDeque::new(),
            bytes: 0,
        }
    }

    fn get(&self, key: &str) -> Option<Arc<[u8]>> {
        self.entries.as_ref()?.get(key).cloned()
    }

    fn put(&mut self, key: String, data: Arc<[u8]>) {
        let size = data.len();
        if size > CACHE_BYTES {
            return;
        }
        let entries = self.entries.get_or_insert_with(HashMap::new);
        if entries.insert(key.clone(), data).is_none() {
            self.order.push_back(key);
            self.bytes = self.bytes.saturating_add(size);
        }
        while self.bytes > CACHE_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(gone) = entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(gone.len());
            }
        }
    }
}

/// Installs the process media source (replacing any earlier one) and forgets
/// what was cached from it.
pub fn install_media_source(source: Arc<dyn MediaSource>) {
    if let Ok(mut slot) = SOURCE.write() {
        *slot = Some(source);
    }
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Cache::new();
    }
}

fn current_source() -> Option<Arc<dyn MediaSource>> {
    SOURCE.read().ok().and_then(|slot| slot.clone())
}

/// Resolves one image through the process media source.
#[must_use]
pub fn resolve_media(media: &MediaRef, detail: Option<ImageDetail>) -> ModelImage {
    resolve_media_with(current_source().as_deref(), media, detail)
}

/// Resolves one image through `source` (no cache). `None` means no source.
#[must_use]
pub fn resolve_media_with(
    source: Option<&dyn MediaSource>,
    media: &MediaRef,
    detail: Option<ImageDetail>,
) -> ModelImage {
    let data = source
        .and_then(|source| source.load(media).ok())
        .map(Arc::<[u8]>::from);
    ModelImage {
        media: media.clone(),
        detail,
        data,
    }
}

pub(crate) fn resolve_cached(media: &MediaRef, detail: Option<ImageDetail>) -> ModelImage {
    let key = media.digest().to_string();
    if let Some(data) = CACHE.lock().ok().and_then(|cache| cache.get(&key)) {
        return ModelImage {
            media: media.clone(),
            detail,
            data: Some(data),
        };
    }
    let image = resolve_media(media, detail);
    if let (Some(data), Ok(mut cache)) = (&image.data, CACHE.lock()) {
        cache.put(key, Arc::clone(data));
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_media::MediaError;
    use harw_protocol::ImageFormat;

    use crate::test_support::{TestError, TestResult};

    struct Fixed(Vec<u8>);

    impl MediaSource for Fixed {
        fn load(&self, _: &MediaRef) -> Result<Vec<u8>, MediaError> {
            Ok(self.0.clone())
        }
    }

    struct Missing;

    impl MediaSource for Missing {
        fn load(&self, _: &MediaRef) -> Result<Vec<u8>, MediaError> {
            Err(MediaError::NotFound)
        }
    }

    fn media() -> TestResult<MediaRef> {
        MediaRef::new(
            harw_types::ContentDigest::of(b"x"),
            ImageFormat::Png,
            4,
            10,
            20,
        )
        .map_err(|e| TestError::Unexpected(e.to_string()))
    }

    #[test]
    fn an_image_the_source_has_is_resolved_with_its_bytes() -> TestResult {
        let image = resolve_media_with(Some(&Fixed(vec![1, 2, 3, 4])), &media()?, None);
        assert_eq!(image.bytes(), Some(&[1, 2, 3, 4][..]));
        Ok(())
    }

    #[test]
    fn an_image_the_source_cannot_supply_is_kept_without_bytes() -> TestResult {
        for source in [Some(&Missing as &dyn MediaSource), None] {
            let image = resolve_media_with(source, &media()?, Some(ImageDetail::Low));
            assert_eq!(image.bytes(), None);
            assert_eq!(image.detail, Some(ImageDetail::Low));
            let text = image.placeholder("the media store has no such image");
            assert!(text.contains("image 10x20"), "{text}");
            assert!(text.contains("not sent"), "{text}");
        }
        Ok(())
    }

    #[test]
    fn the_cache_keeps_recent_images_and_stays_bounded() {
        let mut cache = Cache::new();
        let big: Arc<[u8]> = Arc::from(vec![0u8; CACHE_BYTES / 2 + 1]);
        cache.put("a".to_owned(), Arc::clone(&big));
        cache.put("b".to_owned(), Arc::clone(&big));
        assert!(cache.get("a").is_none(), "the oldest was evicted");
        assert!(cache.get("b").is_some());
        assert!(cache.bytes <= CACHE_BYTES);
        let huge: Arc<[u8]> = Arc::from(vec![0u8; CACHE_BYTES + 1]);
        cache.put("c".to_owned(), huge);
        assert!(
            cache.get("c").is_none(),
            "an image over the bound is not cached"
        );
    }
}
