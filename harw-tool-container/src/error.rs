//! Errors of the container policy core.
//!
//! Every variant carries a static reason, never a long copy of caller input,
//! so an error can be shown to the model or logged without leaking or
//! amplifying what it sent.

use std::fmt;

/// Why a container specification was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainerPolicyError {
    /// The image reference is not `name@sha256:<64 lowercase hex>`.
    InvalidImage(&'static str),
    /// An image alias is malformed.
    InvalidAlias(&'static str),
    /// The alias is not in the image catalog.
    UnknownAlias,
    /// The catalog lists the same alias twice.
    DuplicateAlias,
    /// A mount source, destination or volume name is not acceptable.
    InvalidMount(&'static str),
    /// The text is not the container id that `create` prints.
    InvalidContainerId(&'static str),
    /// A name (run id, owner, connection, cache volume, label value) is invalid.
    InvalidName {
        /// Which field was rejected.
        field: &'static str,
        /// Why.
        reason: &'static str,
    },
    /// The command vector is empty, too large, or contains NUL.
    InvalidCommand(&'static str),
    /// The working directory leaves `/workspace` or is malformed.
    InvalidWorkdir(&'static str),
    /// An environment entry is malformed.
    InvalidEnv(&'static str),
    /// The environment key is not on the allowlist.
    EnvKeyNotAllowed,
    /// A path is not absolute, not clean, or contains forbidden characters.
    InvalidPath(&'static str),
}

impl fmt::Display for ContainerPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidImage(r) => write!(f, "invalid image reference: {r}"),
            Self::InvalidAlias(r) => write!(f, "invalid image alias: {r}"),
            Self::UnknownAlias => f.write_str("image alias is not in the catalog"),
            Self::DuplicateAlias => f.write_str("image catalog lists an alias twice"),
            Self::InvalidMount(r) => write!(f, "invalid mount: {r}"),
            Self::InvalidContainerId(r) => write!(f, "invalid container id: {r}"),
            Self::InvalidName { field, reason } => write!(f, "invalid {field}: {reason}"),
            Self::InvalidCommand(r) => write!(f, "invalid command: {r}"),
            Self::InvalidWorkdir(r) => write!(f, "invalid working directory: {r}"),
            Self::InvalidEnv(r) => write!(f, "invalid environment entry: {r}"),
            Self::EnvKeyNotAllowed => f.write_str("environment key is not allowed"),
            Self::InvalidPath(r) => write!(f, "invalid path: {r}"),
        }
    }
}

impl std::error::Error for ContainerPolicyError {}
