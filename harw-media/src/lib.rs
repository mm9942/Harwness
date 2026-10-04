//! `harw-media`: the media store behind model input.
//!
//! An image a user attaches, or a tool produces, is **ingested once**:
//! recognised by its magic bytes (never by a file name), measured from its
//! headers, checked against [`MediaLimits`], stripped of metadata and of any
//! data after the image, and stored content-addressed with owner-only
//! permissions. What travels through transcripts and frames is only the
//! [`harw_protocol::MediaRef`]; the bytes come back out through
//! [`MediaSource`] when a provider adapter builds its request.
//!
//! No image decoder is involved: the harness copies pixels, it never renders
//! them. SVG is refused (it can carry script) and so is anything that is not
//! a PNG, JPEG, GIF or WebP.

#![forbid(unsafe_code)]

mod error;
#[cfg(test)]
mod fixtures;
mod inspect;
mod limits;
mod sanitize;
mod store;

pub use error::MediaError;
pub use inspect::{Inspected, inspect, sniff};
pub use limits::MediaLimits;
pub use sanitize::sanitize;
pub use store::{MediaSource, MediaStore};
