//! Why media was refused or could not be stored or read.

use std::fmt;
use std::io;

/// Error of every media operation.
#[derive(Debug)]
#[non_exhaustive]
pub enum MediaError {
    /// No bytes.
    Empty,
    /// More bytes than the policy allows.
    TooLarge {
        /// The limit in bytes.
        limit: u64,
    },
    /// Not a PNG, JPEG, GIF or WebP.
    UnknownFormat,
    /// The bytes claim a format but are not well formed.
    Malformed(&'static str),
    /// The image is larger than the policy allows.
    DimensionsTooLarge {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
    /// The store has no such image.
    NotFound,
    /// The stored bytes do not match their reference (size or digest).
    Corrupt,
    /// The store directory is not private or not a plain directory.
    UnsafeStore(&'static str),
    /// A file system failure.
    Io(io::Error),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the image is empty"),
            Self::TooLarge { limit } => write!(f, "the image is larger than {limit} bytes"),
            Self::UnknownFormat => f.write_str("not a PNG, JPEG, GIF or WebP image"),
            Self::Malformed(why) => write!(f, "malformed image: {why}"),
            Self::DimensionsTooLarge { width, height } => {
                write!(f, "the image is too large ({width}x{height} pixels)")
            }
            Self::NotFound => f.write_str("the image is not in the media store"),
            Self::Corrupt => f.write_str("the stored image does not match its reference"),
            Self::UnsafeStore(why) => write!(f, "unsafe media store: {why}"),
            Self::Io(error) => write!(f, "media store i/o error: {error}"),
        }
    }
}

impl std::error::Error for MediaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for MediaError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
