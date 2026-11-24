//! Explicit standalone image descriptions. This path never creates a chat
//! turn or resolves an external provider.

use crate::PromptImage;

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LocalVisionChoice {
    pub model_id: String,
    pub effective_parallelism: u8,
    /// Opaque identity of the loaded engine and projector, not persisted.
    pub loaded_revision: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct LocalVisionImage {
    pub id: u64,
    pub image: PromptImage,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LocalVisionOutcome {
    Caption { text: String },
    OutputLimit { text: String },
    ContextLimit { text: String },
    InvalidInput,
    Failed,
}

#[uniffi::export(with_foreign)]
pub trait LocalVisionProgress: Send + Sync {
    fn on_result(&self, id: u64, outcome: LocalVisionOutcome);
}

#[cfg(feature = "local-host")]
use crate::MobileError;
#[cfg(feature = "local-host")]
use harw_types::cancel::{CancelReason, CancelToken};
#[cfg(feature = "local-host")]
use std::sync::Arc;

#[cfg(any(feature = "local-host", test))]
pub(crate) const MAX_LOCAL_VISION_CONCURRENCY: usize = 5;
#[cfg(any(feature = "local-host", test))]
pub(crate) const DEFAULT_LOCAL_VISION_CONCURRENCY: usize = 3;

#[cfg(feature = "local-host")]
const MAX_IMAGES: usize = 50;
#[cfg(feature = "local-host")]
const MAX_B64: usize = 240_000;
#[cfg(feature = "local-host")]
const MAX_JPEG: usize = 180_000;
#[cfg(feature = "local-host")]
const MAX_EDGE: u32 = 1280;

#[cfg(feature = "local-host")]
fn session(detail: &str) -> MobileError {
    MobileError::Session {
        detail: detail.to_owned(),
    }
}

#[cfg(feature = "local-host")]
fn normalize_image(image: &PromptImage) -> Option<Arc<[u8]>> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use harw_protocol::ImageFormat;

    if image.mime != "image/jpeg" || image.data_b64.len() > MAX_B64 {
        return None;
    }
    let bytes = STANDARD.decode(&image.data_b64).ok()?;
    if bytes.len() > MAX_JPEG {
        return None;
    }
    let seen = harw_media::inspect(&bytes).ok()?;
    if seen.format != ImageFormat::Jpeg || seen.width > MAX_EDGE || seen.height > MAX_EDGE {
        return None;
    }
    let clean = harw_media::sanitize(&bytes, seen.format).ok()?;
    Some(clean.into())
}

#[cfg(feature = "local-host")]
fn prepare_images(
    images: Vec<LocalVisionImage>,
    cancel: &CancelToken,
) -> Result<Vec<(u64, Arc<[u8]>)>, MobileError> {
    let total = images.iter().try_fold(0usize, |sum, image| {
        sum.checked_add(image.image.data_b64.len())
    });
    if images.is_empty()
        || images.len() > MAX_IMAGES
        || total.is_none_or(|size| size > MAX_IMAGES * MAX_B64)
    {
        return Err(session(
            "Die lokale Bildanalyse ist auf 1–50 Bilder und 12 MB Eingabedaten begrenzt.",
        ));
    }
    let mut ids = std::collections::HashSet::with_capacity(images.len());
    if images.iter().any(|image| !ids.insert(image.id)) {
        return Err(session(
            "Bild-Kennungen müssen in der Auswahl eindeutig sein.",
        ));
    }

    images
        .into_iter()
        .map(|image| {
            if cancel.is_cancelled() {
                return Err(session("Lokale Bildanalyse gestoppt."));
            }
            // The provider gives this ID its own InvalidInput result and still
            // processes all the other entries. Nothing uninspected reaches MTMD.
            Ok((image.id, normalize_image(&image.image).unwrap_or_default()))
        })
        .collect()
}

#[cfg(feature = "local-host")]
impl From<harw_llama::LocalImageDescriptionOutcome> for LocalVisionOutcome {
    fn from(outcome: harw_llama::LocalImageDescriptionOutcome) -> Self {
        use harw_llama::LocalImageDescriptionOutcome as Source;

        match outcome {
            Source::Caption { text } => Self::Caption { text },
            Source::OutputLimit { text } => Self::OutputLimit { text },
            Source::ContextLimit { text } => Self::ContextLimit { text },
            Source::InvalidInput => Self::InvalidInput,
            Source::Failed => Self::Failed,
        }
    }
}

/// Dropping the UniFFI wait cancels cooperatively, but never aborts the
/// supervisor: that task keeps the cache exclusion until native work ends.
#[cfg(feature = "local-host")]
struct CancelOnDrop(CancelToken);

#[cfg(feature = "local-host")]
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel(CancelReason::User);
    }
}

/// The supervisor outlives a MobileCore object. Its runtime must not be
/// torn down while a native worker from the old link still holds its model.
#[cfg(feature = "local-host")]
pub(crate) fn supervisor_runtime() -> Result<&'static tokio::runtime::Runtime, MobileError> {
    static RUNTIME: std::sync::OnceLock<Option<tokio::runtime::Runtime>> =
        std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                // One shared inference gate fences batches; allow up to five
                // admitted native caption workers instead of silently queueing one.
                .max_blocking_threads(MAX_LOCAL_VISION_CONCURRENCY)
                .thread_name("harw-local-vision")
                .build()
                .ok()
        })
        .as_ref()
        .ok_or_else(|| session("Lokale Bildanalyse konnte nicht gestartet werden."))
}

#[cfg(feature = "local-host")]
pub(crate) async fn describe(
    model: Arc<crate::local::SwitchingModel>,
    stop: tokio_util::sync::CancellationToken,
    choice: LocalVisionChoice,
    images: Vec<LocalVisionImage>,
    progress: Arc<dyn LocalVisionProgress>,
) -> Result<(), MobileError> {
    let cancel = CancelToken::new();
    let _guard = CancelOnDrop(cancel.clone());
    let worker = supervisor_runtime()?.spawn(async move {
        let prepare_cancel = cancel.clone();
        let report_cancel = cancel.clone();
        let operation = async {
            let images =
                tokio::task::spawn_blocking(move || prepare_images(images, &prepare_cancel))
                    .await
                    .map_err(|_| session("Bildvorbereitung fehlgeschlagen."))??;
            model
                .describe_pinned_local_vision(&choice, images, &cancel, &move |result| {
                    if !report_cancel.is_cancelled() {
                        progress.on_result(result.id, result.outcome.into());
                    }
                })
                .await
        };
        tokio::pin!(operation);
        tokio::select! {
            biased;
            () = stop.cancelled() => {
                cancel.cancel(CancelReason::Shutdown);
                // Do not drop the future: it owns the live model fence.
                let _ = operation.await;
                Err(MobileError::Disconnected)
            },
            result = &mut operation => result,
        }
    });
    worker
        .await
        .map_err(|_| session("Lokale Bildanalyse konnte nicht ausgeführt werden."))?
}

#[cfg(all(feature = "local-host", test))]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};

    fn jpeg(width: u16, height: u16) -> PromptImage {
        let mut bytes = vec![
            0xFF, 0xD8, 0xFF, 0xE1, 0, 5, b'G', b'P', b'S', 0xFF, 0xC0, 0, 11, 8,
        ];
        bytes.extend(height.to_be_bytes());
        bytes.extend(width.to_be_bytes());
        bytes.extend([1, 1, 0x11, 0, 0xFF, 0xD9, b'T', b'A', b'I', b'L']);
        PromptImage {
            mime: "image/jpeg".into(),
            data_b64: STANDARD.encode(bytes),
        }
    }

    #[test]
    fn strips_metadata_and_rejects_oversized_dimensions_or_mime() {
        let jpeg = jpeg(1280, 720);
        let bytes = normalize_image(&jpeg).expect("bounded JPEG");
        assert!(bytes.ends_with(&[0xFF, 0xD9]));
        assert!(!bytes.windows(3).any(|window| window == b"GPS"));
        assert!(normalize_image(&self::jpeg(1281, 720)).is_none());
        assert!(
            normalize_image(&PromptImage {
                mime: "image/png".into(),
                ..jpeg
            })
            .is_none()
        );
    }

    #[test]
    fn validates_shape_and_distinguishes_an_invalid_item() {
        let cancel = harw_types::cancel::CancelToken::new();
        let image = LocalVisionImage {
            id: 7,
            image: jpeg(1, 1),
        };
        assert!(prepare_images(vec![], &cancel).is_err());
        assert!(prepare_images(vec![image.clone(); 51], &cancel).is_err());
        assert!(prepare_images(vec![image.clone(), image.clone()], &cancel).is_err());
        let invalid = LocalVisionImage {
            id: 42,
            image: PromptImage {
                mime: "image/jpeg".into(),
                data_b64: "A".repeat(240_001),
            },
        };
        let out =
            prepare_images(vec![invalid, image], &cancel).expect("batch with one malformed item");
        assert_eq!(out[0].0, 42);
        assert!(out[0].1.is_empty());
        assert_eq!(out[1].0, 7);
        assert!(!out[1].1.is_empty());
    }
}
