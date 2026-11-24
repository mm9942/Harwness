//! Negotiated Cloud Library. No request carries a tenant, profile, session or storage path.

use harw_types::SessionId;
use serde::{Deserialize, Serialize};

/// First wire version that can negotiate this opt-in extension.
pub const CLOUD_LIBRARY_WIRE_MINOR: u32 = 5;
/// One bounded phone-safe JPEG. Base64 adds 4/3 bytes on the wire.
pub const CLOUD_LIBRARY_MAX_JPEG_BYTES: usize = 180_000;
/// The private in-profile catalogue never grows beyond this number of images.
pub const CLOUD_LIBRARY_MAX_ASSETS: usize = 16;

/// User-selected Library workflow; its identity comes from the authenticated transport.
#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudLibraryCommandV1 {
    /// List names and optional descriptions. Never returns media.
    List,
    /// Read one exact Library-owned image, not a session/tool/media reference.
    Preview { expected_revision: u64, asset_id: SessionId },
    /// Explicit upload of exactly one JPEG.
    Import { expected_revision: u64, name: String, data_b64: String },
    /// Update only the name of an exact current asset.
    Rename { expected_revision: u64, asset_id: SessionId, name: String },
    /// Update only the description of an exact current asset (null clears it).
    Describe { expected_revision: u64, asset_id: SessionId, description: Option<String> },
    /// Remove one exact current asset from this owner's catalogue.
    Remove { expected_revision: u64, asset_id: SessionId },
}

impl CloudLibraryCommandV1 {
    /// Never silently retry a mutation after an uncertain transport outcome.
    #[must_use]
    pub const fn mutates(&self) -> bool {
        !matches!(self, Self::List | Self::Preview { .. })
    }
}

/// Current asset metadata from an authorised Cloud Library snapshot.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudLibraryAssetV1 {
    /// Server-generated Library id. Not an authority or a session identifier.
    pub asset_id: SessionId,
    /// User-visible name.
    pub name: String,
    /// User-visible description.
    pub description: Option<String>,
}

/// Typed bounded reply to one Cloud Library command.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudLibraryReplyV1 {
    /// Catalogue at an exact revision, no image bytes.
    Snapshot { revision: u64, assets: Vec<CloudLibraryAssetV1> },
    /// Scoped bytes from the verified revision.
    Preview { asset_id: SessionId, data_b64: String },
    /// Committed update at a new exact revision.
    Updated { revision: u64 },
}
