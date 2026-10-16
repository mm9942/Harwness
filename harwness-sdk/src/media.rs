//! Bilder für die Eingabe: [`Image`] und der Medienspeicher unter dem Home.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harw_media::{MediaLimits, MediaStore};
use harw_protocol::MediaRef;

use crate::error::SdkError;

/// Wie viele Bilder eine Nachricht höchstens trägt. Strenger als die meisten
/// Anbieter (Anthropic 100, OpenAI 1500), passend zu Mistral Small
/// (`--limit-mm-per-prompt image=10`) und lokalen vLLM-Servern.
pub const MAX_IMAGES_PER_MESSAGE: usize = 10;

/// Ein Bild, das einer Nachricht beigefügt wird.
///
/// # Beschreibung
/// Angenommen werden PNG, JPEG, GIF und WebP; das Format kommt aus den
/// ersten Bytes, nie aus einem Dateinamen. Beim Senden prüft die SDK das
/// Bild, entfernt Metadaten (EXIF, XMP, Kommentare) und alles, was nach dem
/// Bild angehängt wurde, und legt es im Medienspeicher unter dem Home ab
/// (`<home>/media`, nur für den Besitzer lesbar). Im Verlauf steht nur eine
/// Referenz, nie das Bild selbst.
///
/// # Beispiel
/// ```rust,no_run
/// use harwness_sdk::{Harwness, Image};
///
/// # async fn demo() -> Result<(), harwness_sdk::SdkError> {
/// let harwness = Harwness::builder().build()?;
/// let mut session = harwness.session()?;
/// let report = session
///     .send_with_images("Was zeigt dieses Foto?", vec![Image::from_file("foto.jpg")?])
///     .await?;
/// println!("{}", report.text.unwrap_or_default());
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Image {
    bytes: Vec<u8>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nie die Bytes: nur die Größe.
        f.debug_struct("Image")
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl Image {
    /// Ein Bild aus Bytes im Speicher.
    #[must_use]
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

    /// Ein Bild aus einer Datei. Gelesen wird höchstens das Limit des
    /// Medienspeichers plus ein Byte; eine größere Datei wird nicht erst
    /// vollständig geladen.
    ///
    /// # Fehler
    /// [`SdkError::InvalidInput`], wenn die Datei fehlt, keine reguläre Datei
    /// ist oder das Limit überschreitet.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, SdkError> {
        use std::io::Read;
        let path = path.as_ref();
        let limit = MediaLimits::default().max_bytes;
        let file = std::fs::File::open(path)
            .map_err(|error| SdkError::invalid("image", format!("{}: {error}", path.display())))?;
        let meta = file
            .metadata()
            .map_err(|error| SdkError::invalid("image", error.to_string()))?;
        if !meta.is_file() {
            return Err(SdkError::invalid("image", "not a regular file"));
        }
        let mut bytes = Vec::new();
        file.take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| SdkError::invalid("image", error.to_string()))?;
        if u64::try_from(bytes.len()).map_or(true, |len| len > limit) {
            return Err(SdkError::invalid(
                "image",
                format!("larger than {limit} bytes"),
            ));
        }
        Ok(Self { bytes })
    }
}

/// Der Medienspeicher eines Homes, einmal geöffnet und einmal installiert.
#[derive(Debug)]
pub(crate) struct MediaHandle {
    root: PathBuf,
    store: Mutex<Option<Arc<MediaStore>>>,
}

impl MediaHandle {
    pub(crate) fn new(home: &Path) -> Self {
        Self {
            root: home.join("media"),
            store: Mutex::new(None),
        }
    }

    /// Hängt einen vorhandenen Speicher ein (ohne etwas anzulegen), damit eine
    /// fortgesetzte Sitzung ihre früheren Bilder wieder senden kann.
    pub(crate) fn attach_existing(&self) {
        let Ok(mut slot) = self.store.lock() else {
            return;
        };
        if slot.is_some() {
            return;
        }
        match MediaStore::open_existing(&self.root) {
            Ok(Some(store)) => {
                let store = Arc::new(store);
                harw_core::install_media_source(Arc::clone(&store) as _);
                *slot = Some(store);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "media store not attached");
            }
        }
    }

    /// Legt die Bilder ab (öffnet und installiert den Speicher bei Bedarf).
    pub(crate) fn ingest(&self, images: &[Image]) -> Result<Vec<MediaRef>, SdkError> {
        if images.len() > MAX_IMAGES_PER_MESSAGE {
            return Err(SdkError::invalid(
                "images",
                format!("at most {MAX_IMAGES_PER_MESSAGE} images per message"),
            ));
        }
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let store = {
            let mut slot = self
                .store
                .lock()
                .map_err(|_| SdkError::invalid("images", "media store lock poisoned"))?;
            if let Some(store) = slot.as_ref() {
                Arc::clone(store)
            } else {
                let store = Arc::new(
                    MediaStore::open(&self.root)
                        .map_err(|error| SdkError::invalid("images", error.to_string()))?,
                );
                harw_core::install_media_source(Arc::clone(&store) as _);
                *slot = Some(Arc::clone(&store));
                store
            }
        };
        images
            .iter()
            .map(|image| {
                store
                    .put(&image.bytes)
                    .map_err(|error| SdkError::invalid("image", error.to_string()))
            })
            .collect()
    }
}
