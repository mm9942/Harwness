//! # artifact
//!
//! ## Responsibility
//! This module owns **references to captured evidence** — screenshots, raw
//! bytes, extracted text, and HTML snapshots — produced while observing or
//! acting on a browser context. It owns only the reference/metadata type
//! ([`ArtifactRef`]) and its kind tag ([`ArtifactKind`]); it does not own the
//! artifact bytes themselves (those live behind whatever URI or storage the
//! backend chooses) or the retention/storage policy for evidence.
//!
//! ## Key types exported
//! - [`ArtifactKind`] — the kind of captured evidence (screenshot, raw bytes,
//!   text, HTML).
//! - [`ArtifactRef`] — a lightweight, serializable reference to a captured
//!   artifact: id, kind, MIME type, size, and an optional storage URI.
//!
//! ## Concurrency
//! Single-threaded, pure data types; `Send + Sync` follows automatically from
//! the fields. No locking or shared state.
//!
//! ## Errors
//! This module defines no fallible operations; it is referenced from
//! [`crate::diagnostic`] and [`crate::observation`] as evidence attached to
//! failures and observations.
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::artifact::{ArtifactKind, ArtifactRef};
//!
//! let artifact = ArtifactRef::new(ArtifactKind::Screenshot, "image/png", 4096)
//!     .with_uri("file:///tmp/shot.png");
//! assert_eq!(artifact.kind, ArtifactKind::Screenshot);
//! ```

use crate::ids::ArtifactId;

/// The kind of captured evidence an [`ArtifactRef`] points to.
///
/// # Concurrency
/// `Copy`, plain data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ArtifactKind {
    Screenshot,
    Bytes,
    Text,
    Html,
}

/// A lightweight, serializable reference to a captured artifact.
///
/// # Description
/// Carries enough metadata (kind, MIME type, size) for a caller to decide
/// whether to fetch the artifact, plus a unique [`crate::ids::ArtifactId`]
/// and an optional `uri` pointing at where the actual bytes are stored. The
/// artifact bytes are never embedded in this type — only a reference to
/// them.
///
/// # Concurrency
/// Plain data, `Send + Sync`, no interior mutability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactRef {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub mime_type: String,
    pub size_bytes: u64,
    pub uri: Option<String>,
}

impl ArtifactRef {
    /// Builds a new artifact reference with a freshly generated
    /// [`crate::ids::ArtifactId`] and no storage URI.
    ///
    /// # Arguments
    /// - `kind` (`ArtifactKind`): the kind of evidence captured. Owned
    ///   (`Copy`).
    /// - `mime_type` (`impl Into<String>`): the MIME type of the captured
    ///   data (for example `"image/png"`). Converted to an owned `String`.
    /// - `size_bytes` (`u64`): the size of the captured data in bytes.
    ///
    /// # Returns
    /// `ArtifactRef` — with `id` freshly generated and `uri: None`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::artifact::{ArtifactKind, ArtifactRef};
    ///
    /// let artifact = ArtifactRef::new(ArtifactKind::Text, "text/plain", 128);
    /// assert_eq!(artifact.uri, None);
    /// ```
    pub fn new(kind: ArtifactKind, mime_type: impl Into<String>, size_bytes: u64) -> Self {
        Self {
            id: ArtifactId::new(),
            kind,
            mime_type: mime_type.into(),
            size_bytes,
            uri: None,
        }
    }

    /// Attaches a storage URI pointing at the artifact's bytes.
    ///
    /// # Description
    /// Consumes and returns `self` so calls can be chained onto
    /// [`ArtifactRef::new`].
    ///
    /// # Arguments
    /// - `uri` (`impl Into<String>`): the location the artifact bytes can be
    ///   fetched from (for example a `file://` or `https://` URI). Converted
    ///   to an owned `String`.
    ///
    /// # Returns
    /// `ArtifactRef` — `self` with `uri` set to `Some(uri)`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_browser::artifact::{ArtifactKind, ArtifactRef};
    ///
    /// let artifact = ArtifactRef::new(ArtifactKind::Bytes, "application/octet-stream", 42)
    ///     .with_uri("file:///tmp/artifact.bin");
    /// assert_eq!(artifact.uri.as_deref(), Some("file:///tmp/artifact.bin"));
    /// ```
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_sets_fields_and_no_uri() {
        let artifact = ArtifactRef::new(ArtifactKind::Screenshot, "image/png", 1024);
        assert_eq!(artifact.kind, ArtifactKind::Screenshot);
        assert_eq!(artifact.mime_type, "image/png");
        assert_eq!(artifact.size_bytes, 1024);
        assert_eq!(artifact.uri, None);
    }

    #[test]
    fn test_with_uri_sets_uri() {
        let artifact = ArtifactRef::new(ArtifactKind::Bytes, "application/octet-stream", 42)
            .with_uri("file:///tmp/artifact.bin");
        assert_eq!(artifact.uri, Some("file:///tmp/artifact.bin".to_owned()));
    }

    #[test]
    fn test_new_generates_distinct_ids() {
        let a = ArtifactRef::new(ArtifactKind::Text, "text/plain", 0);
        let b = ArtifactRef::new(ArtifactKind::Text, "text/plain", 0);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn test_artifact_ref_serde_json_round_trip_without_uri() {
        let artifact = ArtifactRef::new(ArtifactKind::Html, "text/html", 2048);
        let json = serde_json::to_string(&artifact).expect("artifact serializes");
        let decoded: ArtifactRef = serde_json::from_str(&json).expect("artifact deserializes");
        assert_eq!(decoded, artifact);
    }

    #[test]
    fn test_artifact_ref_serde_json_round_trip_with_uri() {
        let artifact = ArtifactRef::new(ArtifactKind::Screenshot, "image/png", 4096)
            .with_uri("file:///tmp/shot.png");
        let json = serde_json::to_string(&artifact).expect("artifact serializes");
        let decoded: ArtifactRef = serde_json::from_str(&json).expect("artifact deserializes");
        assert_eq!(decoded, artifact);
    }
}
