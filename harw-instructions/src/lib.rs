//! `harw-instructions` — Baseline system-prompt provider for the Harwness coding agent.
//!
//! Fills the previously-empty InstructionsProvider slot in the ExtensionRegistry.
//! Provides the model with essential grounding: role, working directory, tool
//! inventory, and behavioral constraints. Without this, the model has zero
//! context about being a coding agent.
//!
//! # Zwei-Block-Konvention (Knoten AW4-01)
//! [`trust_boundary`] trägt den kanonischen Wortlaut der Trennung von
//! Anweisung und Daten im gerenderten Kontext: [`trust_boundary::DATA_BLOCK_NOTICE`]
//! (auch von `harw_core::context_budget::render_trust_blocks` verwendet, das
//! die eigentliche Zwei-Block-Montage baut) und
//! [`trust_boundary::context_blocks_section`], den optionale Systemprompt-
//! Abschnitt, den [`AgentIdentity::with_trust_boundary_notice`] aktiviert.

#![forbid(unsafe_code)]
pub mod baseline;
pub mod trust_boundary;
pub use baseline::{AgentIdentity, BaselineInstructionsProvider};
pub use trust_boundary::{DATA_BLOCK_NOTICE, context_blocks_section};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
