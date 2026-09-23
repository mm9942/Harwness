//! `KnowledgeError` — the crate-wide error surface, built with the
//! `harw_macros::HarwError` derive per the shared error contract.
//!
//! The derive emits `Display`, `std::error::Error` (with `source()` wired
//! through `#[from]` variants) and the `KnowledgeResult<T>` alias (the enum
//! name ends in `Error`). No `anyhow`/`thiserror`. Foreign errors that
//! implemented flows actually propagate via `?` get a `#[from]` variant
//! (io, YAML frontmatter, index-cache JSON, TOML, job runtime, time).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §8.1 (verbatim message variants),
//! extended with `MalformedFrontmatter`, `IllegalTransition` and `Json` for the
//! pure-logic bodies this skeleton implements (frontmatter split, §6.3 card
//! transitions, index cache), and with `ClaimRequiresApproval`/`TomlEncode`/
//! `TomlDecode` for the §6.4 approval gate and `board.toml` persistence.

use harw_macros::HarwError;

/// Central error type for `harw-knowledge` (emits `KnowledgeResult<T>`).
#[derive(Debug, HarwError)]
pub enum KnowledgeError {
    /// No artifact exists for the requested id.
    #[msg("artifact {0} not found")]
    ArtifactNotFound(String),

    /// A write was refused because the caller's scope does not cover the target.
    #[msg("write to {surface} rejected: caller scope {caller:?} does not cover {required:?}")]
    VisibilityDenied {
        surface: String,
        caller: String,
        required: String,
    },

    /// A promotion that requires review was attempted with no recorded review.
    #[msg("promotion from {from} to {to} requires review but none was recorded")]
    PromotionNotReviewed { from: String, to: String },

    /// A card cannot be archived while it still has unresolved child cards.
    #[msg("card {card_id} cannot archive: {blocking_children} unresolved child card(s)")]
    ArchiveBlockedByChildren {
        card_id: String,
        blocking_children: usize,
    },

    /// A recall query requested a bound larger than the hard maximum.
    #[msg("recall query exceeded bound: {field} = {value} > max {max}")]
    RecallBoundExceeded {
        field: String,
        value: usize,
        max: usize,
    },

    /// A stored markdown artifact lacked a well-formed frontmatter fence.
    #[msg("malformed frontmatter: {detail}")]
    MalformedFrontmatter { detail: String },

    /// A kanban card lifecycle transition was not permitted from its state (§6.3).
    #[msg("illegal card transition from {from} to {to}")]
    IllegalTransition { from: String, to: String },

    /// A `claim` (Ready -> Running, §6.3) on a worker lane whose bound role
    /// carries `RiskLevel::High` or above was attempted without a passed-in
    /// `ApprovalProof` resolving to a positive `ReviewDecision` (§6.4). This
    /// crate never contacts an approval service itself — the caller must
    /// already hold the proof before calling `claim`.
    #[msg("card {card_id} cannot claim on a lane with risk level {risk_level}: a positive approval proof is required")]
    ClaimRequiresApproval { card_id: String, risk_level: String },

    /// A `ContextProposal` was read with a kind other than `ContextProposal`
    /// (AW5-09) — `from_artifact` refuses to guess a mismatched payload.
    #[msg("artifact {id} has kind {actual}, expected {expected}")]
    ArtifactKindMismatch {
        id: String,
        expected: String,
        actual: String,
    },

    /// `ContextProposal::accept`/`reject` was called on a proposal that is
    /// not `Pending` (AW5-09) — a decided proposal is not decided twice.
    #[msg("context proposal {id} is not pending (current status: {status})")]
    ProposalNotPending { id: String, status: String },

    /// `ModelBehaviorProposal::accept`/`reject` was called on a proposal that
    /// is not `Pending` (AW6-06) — a decided proposal is not decided twice.
    #[msg("model behavior proposal {id} is not pending (current status: {status})")]
    ModelProposalNotPending { id: String, status: String },

    /// A `context_steward::StewardWindow` was constructed with `since` after
    /// `until`, or a trailing span could not be subtracted from `as_of`
    /// without overflowing (Context Steward, AW6-08). The window is always
    /// caller-supplied (no system clock), so this is a caller error, not a
    /// transient one.
    #[msg("invalid steward observation window: {detail}")]
    StewardWindowInvalid { detail: String },

    /// Filesystem I/O failure; defers `Display`/`source()` to the inner error.
    #[from]
    Io(std::io::Error),

    /// YAML frontmatter (de)serialization failure (serde_norway).
    #[from]
    Frontmatter(serde_norway::Error),

    /// Index-cache JSON (de)serialization failure.
    #[from]
    Json(serde_json::Error),

    /// `kanban/boards/<board-id>/board.toml` (de)serialization failure —
    /// encode side (§1.2, §6 persistence).
    #[from]
    TomlEncode(toml::ser::Error),

    /// `kanban/boards/<board-id>/board.toml` (de)serialization failure —
    /// decode side (§1.2, §6 persistence).
    #[from]
    TomlDecode(toml::de::Error),

    /// A governed-work operation (claim/charge) failed in `harw-job-runtime`.
    #[from]
    Job(harw_job_runtime::JobError),

    /// Zeitarithmetik hat den von `jiff` darstellbaren Bereich überschritten
    /// (z. B. beim Berechnen des Diary-Rollup-Cutoffs aus `retention_days`).
    #[from]
    Time(jiff::Error),
}
