//! The content-addressed store.

use std::fs::{self, DirBuilder};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use harw_digest::ContentDigest;
use harw_fsutil::{AtomicWriteOptions, OpenMode, open_nofollow, write_atomic};
use harw_protocol::MediaRef;

use crate::error::MediaError;
use crate::inspect::inspect;
use crate::limits::MediaLimits;
use crate::sanitize::sanitize;

/// Where a provider adapter gets the bytes of a [`MediaRef`].
///
/// The trait is the seam between the store and the code that builds requests:
/// tests and remote setups can supply their own source.
pub trait MediaSource: Send + Sync {
    /// The bytes of `media`, verified against its reference.
    ///
    /// # Errors
    /// [`MediaError::NotFound`] when the source does not have the image,
    /// [`MediaError::Corrupt`] when what it has does not match.
    fn load(&self, media: &MediaRef) -> Result<Vec<u8>, MediaError>;
}

/// A directory of images, named by the digest of their (sanitized) bytes.
///
/// The directory and every file in it are private to the owner. Files are
/// written atomically and opened without following symlinks, and an image is
/// verified against its digest and size every time it is read, so a swapped
/// or damaged file is an error and never silently another picture.
#[derive(Debug, Clone)]
pub struct MediaStore {
    root: PathBuf,
    limits: MediaLimits,
}

impl MediaStore {
    /// Opens the store at `root`, creating it (mode `0700`) if it does not
    /// exist.
    ///
    /// # Errors
    /// [`MediaError::UnsafeStore`] when `root` is a symlink, not a directory,
    /// owned by someone else or open to group or others.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, MediaError> {
        Self::open_with(root, MediaLimits::default())
    }

    /// Like [`Self::open`], with explicit limits.
    ///
    /// # Errors
    /// See [`Self::open`].
    pub fn open_with(root: impl Into<PathBuf>, limits: MediaLimits) -> Result<Self, MediaError> {
        let root = root.into();
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        check_private_dir(&root)?;
        Ok(Self { root, limits })
    }

    /// The directory of this store.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The limits applied by [`Self::put`].
    #[must_use]
    pub fn limits(&self) -> MediaLimits {
        self.limits
    }

    fn path_of(&self, media: &MediaRef) -> PathBuf {
        let hex = media.digest().to_string();
        let fan = hex.get(..2).unwrap_or("00").to_owned();
        self.root
            .join(fan)
            .join(format!("{hex}.{}", media.format().extension()))
    }

    /// Ingests an image: checks it, sanitizes it, stores it, and returns the
    /// reference to put into a transcript.
    ///
    /// # Errors
    /// [`MediaError`]: empty, too large, not an accepted format, malformed,
    /// too many pixels, or a file system failure.
    pub fn put(&self, bytes: &[u8]) -> Result<MediaRef, MediaError> {
        let (media, clean) = ingest(bytes, self.limits)?;
        let path = self.path_of(&media);
        if let Some(parent) = path.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        match self.load(&media) {
            Ok(_) => return Ok(media), // already stored and intact
            Err(MediaError::NotFound | MediaError::Corrupt) => {}
            Err(other) => return Err(other),
        }
        write_atomic(&path, &clean, AtomicWriteOptions::private())?;
        Ok(media)
    }

    /// Whether the store holds `media`, intact.
    #[must_use]
    pub fn contains(&self, media: &MediaRef) -> bool {
        self.load(media).is_ok()
    }
}

impl MediaSource for MediaStore {
    fn load(&self, media: &MediaRef) -> Result<Vec<u8>, MediaError> {
        let path = self.path_of(media);
        let mut file = match open_nofollow(&path, OpenMode::read_only()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(MediaError::NotFound);
            }
            // A symlink in place of the file is not our file.
            Err(_) => return Err(MediaError::Corrupt),
        };
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() != media.bytes() || meta.mode() & 0o077 != 0 {
            return Err(MediaError::Corrupt);
        }
        let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
        // One byte more than expected, so a file that grew after the check
        // is noticed instead of truncated.
        let cap = media.bytes().saturating_add(1);
        file.by_ref().take(cap).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).ok() != Some(media.bytes())
            || &ContentDigest::of(&bytes) != media.digest()
        {
            return Err(MediaError::Corrupt);
        }
        Ok(bytes)
    }
}

/// Checks, measures and sanitizes `bytes`; returns the reference and the
/// sanitized bytes it names. Shared by every [`MediaSource`] that ingests.
pub(crate) fn ingest(bytes: &[u8], limits: MediaLimits) -> Result<(MediaRef, Vec<u8>), MediaError> {
    if bytes.is_empty() {
        return Err(MediaError::Empty);
    }
    let limit = limits.max_bytes;
    if u64::try_from(bytes.len()).map_or(true, |len| len > limit) {
        return Err(MediaError::TooLarge { limit });
    }
    let seen = inspect(bytes)?;
    let clean = sanitize(bytes, seen.format)?;
    // The sanitized copy must still be the same image.
    let after = inspect(&clean)?;
    if after != seen {
        return Err(MediaError::Malformed("sanitizing changed the image"));
    }
    let pixels = u64::from(seen.width) * u64::from(seen.height);
    if seen.width > limits.max_edge || seen.height > limits.max_edge || pixels > limits.max_pixels {
        return Err(MediaError::DimensionsTooLarge {
            width: seen.width,
            height: seen.height,
        });
    }
    let size = u64::try_from(clean.len()).map_err(|_| MediaError::TooLarge { limit })?;
    let media = MediaRef::new(
        ContentDigest::of(&clean),
        seen.format,
        size,
        seen.width,
        seen.height,
    )
    .map_err(|_| MediaError::Malformed("implausible size or dimensions"))?;
    Ok((media, clean))
}

fn check_private_dir(root: &Path) -> Result<(), MediaError> {
    let meta = fs::symlink_metadata(root)?;
    if meta.file_type().is_symlink() {
        return Err(MediaError::UnsafeStore("the store path is a symlink"));
    }
    if !meta.is_dir() {
        return Err(MediaError::UnsafeStore("the store path is not a directory"));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(MediaError::UnsafeStore(
            "the store is open to group or others",
        ));
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(MediaError::UnsafeStore("the store belongs to another user"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{SECRET, TestError, TestResult, contains, gif, jpeg, png, webp_extended};
    use std::os::unix::fs::symlink;

    fn store() -> TestResult<(tempfile::TempDir, MediaStore)> {
        let dir = tempfile::tempdir().map_err(|e| TestError::Unexpected(e.to_string()))?;
        let store = MediaStore::open(dir.path().join("media"))
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        Ok((dir, store))
    }

    fn put(store: &MediaStore, bytes: &[u8]) -> TestResult<MediaRef> {
        store
            .put(bytes)
            .map_err(|e| TestError::Unexpected(e.to_string()))
    }

    fn file_of(store: &MediaStore, media: &MediaRef) -> PathBuf {
        store.path_of(media)
    }

    #[test]
    fn an_image_goes_in_sanitized_and_comes_out_verified() -> TestResult {
        let (_dir, store) = store()?;
        let input = jpeg(640, 480, b"appended payload");
        let media = put(&store, &input)?;
        assert_eq!((media.width(), media.height()), (640, 480));
        assert_eq!(media.mime(), "image/jpeg");
        let out = store
            .load(&media)
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(!contains(&out, SECRET));
        assert!(!contains(&out, b"appended payload"));
        assert_eq!(u64::try_from(out.len()).ok(), Some(media.bytes()));
        assert!(store.contains(&media));
        Ok(())
    }

    #[test]
    fn the_same_image_is_stored_once() -> TestResult {
        let (_dir, store) = store()?;
        let a = put(&store, &png(32, 32, &[], &[]))?;
        let b = put(&store, &png(32, 32, &[(b"tEXt", SECRET)], b"tail"))?;
        assert_eq!(
            a, b,
            "metadata and trailing data do not change the identity"
        );
        let files = fs::read_dir(
            file_of(&store, &a)
                .parent()
                .ok_or(TestError::Missing("fan-out directory"))?,
        )
        .map_err(|e| TestError::Unexpected(e.to_string()))?
        .count();
        assert_eq!(files, 1);
        Ok(())
    }

    #[test]
    fn the_store_and_its_files_are_private() -> TestResult {
        let (_dir, store) = store()?;
        let media = put(&store, &gif(8, 8, &[]))?;
        let dir_mode = fs::metadata(store.root())
            .map_err(|e| TestError::Unexpected(e.to_string()))?
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o077, 0);
        let file_mode = fs::metadata(file_of(&store, &media))
            .map_err(|e| TestError::Unexpected(e.to_string()))?
            .permissions()
            .mode();
        assert_eq!(file_mode & 0o777, 0o600);
        Ok(())
    }

    #[test]
    fn policy_limits_are_enforced() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| TestError::Unexpected(e.to_string()))?;
        let limits = MediaLimits {
            max_bytes: 4096,
            max_edge: 1000,
            max_pixels: 500_000,
        };
        let store = MediaStore::open_with(dir.path().join("m"), limits)
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(matches!(store.put(&[]), Err(MediaError::Empty)));
        assert!(matches!(
            store.put(&vec![0u8; 5000]),
            Err(MediaError::TooLarge { limit: 4096 })
        ));
        assert!(matches!(
            store.put(&png(1001, 10, &[], &[])),
            Err(MediaError::DimensionsTooLarge { .. })
        ));
        assert!(matches!(
            store.put(&png(900, 900, &[], &[])),
            Err(MediaError::DimensionsTooLarge { .. })
        ));
        assert!(matches!(
            store.put(b"<svg onload='x'/>"),
            Err(MediaError::UnknownFormat)
        ));
        assert!(store.put(&png(700, 700, &[], &[])).is_ok());
        Ok(())
    }

    #[test]
    fn a_damaged_or_swapped_file_is_an_error_never_another_picture() -> TestResult {
        let (_dir, store) = store()?;
        let media = put(&store, &webp_extended(64, 64))?;
        let path = file_of(&store, &media);
        let original = fs::read(&path).map_err(|e| TestError::Unexpected(e.to_string()))?;

        // Same length, different bytes.
        let mut flipped = original.clone();
        if let Some(last) = flipped.last_mut() {
            *last ^= 0xFF;
        }
        fs::write(&path, &flipped).map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(matches!(store.load(&media), Err(MediaError::Corrupt)));

        // Different length.
        fs::write(&path, &original[..original.len() - 2])
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(matches!(store.load(&media), Err(MediaError::Corrupt)));

        // A symlink to another stored image.
        let other = put(&store, &png(16, 16, &[], &[]))?;
        fs::remove_file(&path).map_err(|e| TestError::Unexpected(e.to_string()))?;
        symlink(file_of(&store, &other), &path)
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(matches!(store.load(&media), Err(MediaError::Corrupt)));

        // Gone.
        fs::remove_file(&path).map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(matches!(store.load(&media), Err(MediaError::NotFound)));

        // A put repairs a damaged entry.
        let again = put(&store, &webp_extended(64, 64))?;
        assert_eq!(again, media);
        assert!(store.contains(&media));
        Ok(())
    }

    #[test]
    fn an_unsafe_store_directory_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| TestError::Unexpected(e.to_string()))?;
        let open_dir = dir.path().join("open");
        DirBuilder::new()
            .mode(0o755)
            .create(&open_dir)
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(matches!(
            MediaStore::open(&open_dir),
            Err(MediaError::UnsafeStore(_))
        ));

        let real = dir.path().join("real");
        DirBuilder::new()
            .mode(0o700)
            .create(&real)
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        let link = dir.path().join("link");
        symlink(&real, &link).map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(matches!(
            MediaStore::open(&link),
            Err(MediaError::UnsafeStore(_))
        ));

        let file = dir.path().join("file");
        fs::write(&file, b"x").map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(MediaStore::open(&file).is_err());
        Ok(())
    }
}
