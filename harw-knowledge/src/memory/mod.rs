//! Memory system: the three layers plus the bounded recall interface (§2).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §2. Promotion flows strictly
//! downward in freshness, upward in durability (§2.1):
//! `session recall -> diary -> topic memory -> (rarely) core memory`, and
//! `topic memory -> palace nodes` at the strictest gate.
//!
//! - [`core`]   — the single durable `MEMORY.md` (promotion-gated writes)
//! - [`topic`]  — one file per subject, freely agent-writable within scope
//! - [`palace`] — the linked `[[wikilink]]` long-term graph
//! - [`recall`] — the one bounded read path every surface uses (§2.3)

pub mod core;
pub mod palace;
pub mod recall;
pub mod topic;
