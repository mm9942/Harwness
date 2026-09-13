//! Ordered confidence level shared by the memory and model-catalog subsystems.
//!
//! # Responsibility
//!
//! This module owns [`Confidence`], a five-level ordinal scale describing how
//! well-supported a claim or measurement is. It carries no business logic of
//! its own — what a given level means for promotion, staleness, or trust
//! decisions is decided by the consuming crate (`harw-memory`'s
//! `PromotionScore`, `harw-model-catalog`'s `MetricEstimate`). This module's
//! only job is to own the single, shared definition of the scale and its
//! wire form.
//!
//! # `Confidence` types in the tree — and why only one was a duplicate
//!
//! Before this module existed (node AW0-03b), the workspace held four
//! independent types named `Confidence`:
//!
//! 1. `harw_research::Confidence` (`harw-research/src/types.rs`) — 4 levels,
//!    `Low/Medium/High/Verified`. Measures how strongly a research claim is
//!    backed by primary sources — verification depth, not evidence volume.
//!    Different semantics and a different arity than the type below. Not a
//!    duplicate; left untouched by this move.
//! 2. `harw_memory::epistemic::Confidence` (`harw-memory/src/epistemic.rs`) —
//!    5 levels, `VeryLow..VeryHigh`.
//! 3. `harw_model_catalog::provenance::Confidence`
//!    (`harw-model-catalog/src/provenance.rs`) — the same 5 levels, same
//!    order, same derives (`Debug, Clone, Copy, PartialEq, Eq, PartialOrd,
//!    Ord, Serialize, Deserialize`), same `#[serde(rename_all =
//!    "snake_case")]` as #2.
//! 4. `harw_knowledge::memory::palace::Confidence`
//!    (`harw-knowledge/src/memory/palace.rs`) — 3 levels, a lifecycle
//!    (`Established/Provisional/Superseded`), not an ordinal confidence
//!    measure. Being renamed to `PalaceStatus` in a parallel node — not the
//!    same vocabulary, never a duplicate of this type.
//!
//! A prior node (AW2-20) compared #2 and #3 line by line and found them
//! byte-identical in shape: same variants, same order, same derives, same
//! serde attribute. Under the "one symbol per vocabulary pair" rule that
//! makes them a duplicate, not an honestly separate pair — but that node
//! left both definitions in place rather than move one, because
//! `harw_types::Confidence` did not exist yet and a half-completed move is
//! worse than two honest copies.
//!
//! This module (AW0-03b) completes that move. [`Confidence`] is now defined
//! exactly once, here. `harw-memory` and `harw-model-catalog` each re-export
//! it under their historical path (`epistemic::Confidence`,
//! `provenance::Confidence`) so no call site changes. After this move the
//! tree holds three distinct `Confidence`-shaped types, not four — #2 and #3
//! above are the same type under two names, not two separate types:
//!
//! - `harw_research::Confidence` — untouched, genuinely different.
//! - [`Confidence`] (this type) — the single owner of the five-level scale.
//! - `harw_knowledge::memory::palace::Confidence` / `PalaceStatus` —
//!   untouched, genuinely different.
//!
//! Do not add a fifth `Confidence` type to the tree. If a new subsystem
//! needs an ordinal confidence scale, check whether this one fits before
//! defining another.
//!
//! # Concurrency
//!
//! [`Confidence`] is a `Copy` enum with no interior mutability; it is
//! `Send + Sync` and safe to share and compare across threads without any
//! synchronization.
//!
//! # Errors
//!
//! This module defines no error type of its own. Deserializing an invalid
//! JSON value (e.g. an unrecognized string) surfaces as a `serde_json::Error`
//! at the caller's deserialization site.
//!
//! # Examples
//!
//! ```rust
//! use harw_types::Confidence;
//!
//! assert!(Confidence::VeryLow < Confidence::VeryHigh);
//! let json = serde_json::to_string(&Confidence::High).unwrap();
//! assert_eq!(json, "\"high\"");
//! ```

use serde::{Deserialize, Serialize};

/// Ordered confidence level for a claim or measurement.
///
/// # Description
///
/// A five-level ordinal scale: `VeryLow < Low < Medium < High < VeryHigh`.
/// Implements `PartialOrd`/`Ord`, so comparisons like
/// `Confidence::Low < Confidence::High` are meaningful. Two independent
/// subsystems share this scale: `harw-memory`'s epistemic layer uses it to
/// gate promotion (low confidence raises the `uncertainty_penalty` in a
/// `PromotionScore`); `harw-model-catalog`'s provenance layer uses it to
/// annotate how well-supported a `MetricEstimate` is. See the module-level
/// documentation for the full inventory of `Confidence`-named types in the
/// tree and why this is the one shared definition.
///
/// # Wire format
///
/// Serializes as a bare snake_case string per variant: `"very_low"`,
/// `"low"`, `"medium"`, `"high"`, `"very_high"`. This form is persisted by
/// both consuming subsystems; changing it breaks existing files in both at
/// once. This module's test suite pins the JSON literal for each variant
/// rather than relying on a roundtrip alone, since a roundtrip stays green
/// even if both sides of a (de)serialization change together.
///
/// # Concurrency
///
/// `Copy` enum; `Send + Sync`.
///
/// # Examples
///
/// ```rust
/// use harw_types::Confidence;
/// assert!(Confidence::VeryLow < Confidence::VeryHigh);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Very low confidence; almost no evidence.
    VeryLow,
    /// Low confidence; a few isolated hints.
    Low,
    /// Medium confidence; several consistent hints.
    Medium,
    /// High confidence; solid evidence.
    High,
    /// Very high confidence; broadly and consistently confirmed.
    VeryHigh,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_confidence_json_form_matches_literal_per_variant() {
        // Literal comparison, not a roundtrip: a roundtrip stays green even
        // if serialization and deserialization drift together.
        assert_eq!(
            serde_json::to_string(&Confidence::VeryLow).unwrap(),
            "\"very_low\""
        );
        assert_eq!(
            serde_json::to_string(&Confidence::Low).unwrap(),
            "\"low\""
        );
        assert_eq!(
            serde_json::to_string(&Confidence::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(
            serde_json::to_string(&Confidence::High).unwrap(),
            "\"high\""
        );
        assert_eq!(
            serde_json::to_string(&Confidence::VeryHigh).unwrap(),
            "\"very_high\""
        );
    }

    #[test]
    fn test_confidence_deserializes_from_literal_per_variant() {
        assert_eq!(
            serde_json::from_str::<Confidence>("\"very_low\"").unwrap(),
            Confidence::VeryLow
        );
        assert_eq!(
            serde_json::from_str::<Confidence>("\"low\"").unwrap(),
            Confidence::Low
        );
        assert_eq!(
            serde_json::from_str::<Confidence>("\"medium\"").unwrap(),
            Confidence::Medium
        );
        assert_eq!(
            serde_json::from_str::<Confidence>("\"high\"").unwrap(),
            Confidence::High
        );
        assert_eq!(
            serde_json::from_str::<Confidence>("\"very_high\"").unwrap(),
            Confidence::VeryHigh
        );
    }

    #[test]
    fn test_confidence_ordering_matches_meaning() {
        assert!(Confidence::VeryLow < Confidence::Low);
        assert!(Confidence::Low < Confidence::Medium);
        assert!(Confidence::Medium < Confidence::High);
        assert!(Confidence::High < Confidence::VeryHigh);
        assert!(Confidence::VeryLow < Confidence::VeryHigh);
    }

    #[test]
    fn test_confidence_roundtrip_all_variants() {
        for c in [
            Confidence::VeryLow,
            Confidence::Low,
            Confidence::Medium,
            Confidence::High,
            Confidence::VeryHigh,
        ] {
            let json = serde_json::to_string(&c).unwrap();
            let recovered: Confidence = serde_json::from_str(&json).unwrap();
            assert_eq!(c, recovered);
        }
    }
}
