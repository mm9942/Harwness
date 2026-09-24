//! Content-addressed digests for evidence and manifest files.
//!
//! [`ContentDigest`] lives in the leaf crate `harw-digest` (blake3 + serde
//! only) so that crates which must stay free of time, randomness and I/O —
//! `harw-lens-types` inside the hull of `harw-lens-rank` — can use it without
//! inheriting this crate's `jiff`, `uuid` and `tokio-util` dependencies. This
//! module re-exports it so `harw_types::ContentDigest` and
//! `harw_types::digest::ContentDigest` keep working unchanged.

pub use harw_digest::ContentDigest;
