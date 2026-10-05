//! Shared `#[serde(default = "...")]` helpers for the TOML config structs.

/// `true`; for boolean fields that default to enabled.
#[must_use]
pub(crate) const fn default_true() -> bool {
    true
}
