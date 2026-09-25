//! The one error type of this crate.
//!
//! Every failure is fail-closed: a reader that sees anything it does not
//! fully understand returns an error and never a partially parsed artifact.

use std::fmt;
use std::io;

use crate::kind::PayloadKind;

/// Which size limit an artifact or payload exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// [`crate::MAX_HEADER_LEN`]: bytes of canonical header JSON.
    HeaderLen,
    /// [`crate::MAX_PAYLOAD_COUNT`]: entries in the payload table.
    PayloadCount,
    /// [`crate::MAX_PAYLOAD_LEN`]: bytes of one payload.
    PayloadLen,
    /// [`crate::MAX_ARTIFACT_LEN`]: bytes of the whole encoded artifact.
    ArtifactLen,
    /// [`crate::MAX_PATH_LEN`]: bytes of one payload path.
    PathLen,
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::HeaderLen => "header length",
            Self::PayloadCount => "payload count",
            Self::PayloadLen => "payload length",
            Self::ArtifactLen => "artifact length",
            Self::PathLen => "path length",
        })
    }
}

/// Why a payload path was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRejection {
    /// Nothing is left after dropping empty and `.` segments.
    Empty,
    /// Starts with `/` (or is a Windows drive / UNC path).
    Absolute,
    /// Contains a `..` segment.
    ParentSegment,
    /// Contains a backslash; only `/` is a separator.
    Backslash,
    /// Contains a NUL or another control character.
    ControlCharacter,
    /// Stored path in an artifact is not valid UTF-8.
    NotUtf8,
    /// Stored path in an artifact is not in normalized form.
    NotNormalized,
}

impl fmt::Display for PathRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "path is empty",
            Self::Absolute => "path is absolute",
            Self::ParentSegment => "path contains a `..` segment",
            Self::Backslash => "path contains a backslash",
            Self::ControlCharacter => "path contains a control character",
            Self::NotUtf8 => "stored path is not valid UTF-8",
            Self::NotNormalized => "stored path is not normalized",
        })
    }
}

/// What a failed hash check covered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TamperScope {
    /// The artifact trailer: BLAKE3 over every byte before it.
    Artifact,
    /// One payload's recorded BLAKE3 does not match its bytes.
    Payload {
        /// Kind of the mismatching payload.
        kind: PayloadKind,
        /// Path of the mismatching payload.
        path: String,
    },
    /// The executable footer: offset/length do not describe the bytes in
    /// front of it, or its hash copy does not match the embedded artifact.
    Footer,
}

impl fmt::Display for TamperScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Artifact => f.write_str("artifact trailer hash mismatch"),
            Self::Payload { kind, path } => {
                write!(f, "payload hash mismatch for {kind}:{path}")
            }
            Self::Footer => f.write_str("executable footer does not match embedded artifact"),
        }
    }
}

/// Errors from building, parsing, embedding and extracting artifacts.
#[derive(Debug)]
pub enum ArtifactError {
    /// The bytes do not start with `HARWAGNT`.
    BadMagic,
    /// The artifact declares a format version this crate does not read.
    UnsupportedVersion {
        /// Version found in the bytes.
        found: u16,
    },
    /// The artifact sets flag bits this crate does not understand (v1: any).
    UnsupportedFlags {
        /// Flags found in the bytes.
        flags: u16,
    },
    /// The input ends before a declared field or section.
    Truncated {
        /// What was being read.
        what: &'static str,
    },
    /// Bytes remain after the trailer.
    TrailingBytes {
        /// Number of unexpected bytes.
        extra: usize,
    },
    /// A size limit was exceeded.
    LimitExceeded {
        /// Which limit.
        limit: Limit,
        /// The declared or actual value.
        actual: u64,
        /// The maximum allowed value.
        max: u64,
    },
    /// A payload path was rejected.
    InvalidPath {
        /// The path as given (lossy for non-UTF-8 input).
        path: String,
        /// Why.
        reason: PathRejection,
    },
    /// An `Other` payload kind name, or an unknown kind tag.
    InvalidKind {
        /// The rejected name or tag.
        detail: String,
    },
    /// Two payloads share the same `(kind, path)`.
    DuplicatePayload {
        /// Kind of the duplicate.
        kind: PayloadKind,
        /// Path of the duplicate.
        path: String,
    },
    /// The payload table is not strictly sorted by `(kind, path)`.
    UnsortedPayloads,
    /// The header is not valid UTF-8 JSON.
    InvalidHeader {
        /// Rendered parser error.
        reason: String,
    },
    /// The header is valid JSON but not in canonical form.
    NonCanonicalHeader,
    /// A hash check failed.
    Tampered(TamperScope),
    /// The executable carries no `HARWAEND` footer.
    NotEmbedded,
    /// Reading or writing a file failed.
    Io {
        /// What was being done.
        context: &'static str,
        /// The underlying error.
        source: io::Error,
    },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => f.write_str("not an agent artifact (bad magic)"),
            Self::UnsupportedVersion { found } => {
                write!(f, "unsupported agent artifact format version {found}")
            }
            Self::UnsupportedFlags { flags } => {
                write!(f, "unsupported agent artifact flags {flags:#06x}")
            }
            Self::Truncated { what } => write!(f, "agent artifact truncated while reading {what}"),
            Self::TrailingBytes { extra } => {
                write!(f, "{extra} unexpected bytes after the artifact trailer")
            }
            Self::LimitExceeded { limit, actual, max } => {
                write!(f, "{limit} {actual} exceeds the limit of {max}")
            }
            Self::InvalidPath { path, reason } => {
                write!(f, "invalid payload path {path:?}: {reason}")
            }
            Self::InvalidKind { detail } => write!(f, "invalid payload kind: {detail}"),
            Self::DuplicatePayload { kind, path } => {
                write!(f, "duplicate payload {kind}:{path}")
            }
            Self::UnsortedPayloads => f.write_str("payload table is not sorted by (kind, path)"),
            Self::InvalidHeader { reason } => write!(f, "invalid artifact header: {reason}"),
            Self::NonCanonicalHeader => f.write_str("artifact header is not canonical JSON"),
            Self::Tampered(scope) => write!(f, "agent artifact tampered: {scope}"),
            Self::NotEmbedded => f.write_str("executable carries no embedded agent artifact"),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for ArtifactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
