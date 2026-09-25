//! `harw-agent-artifact`: the `agent-artifact-v1` container format.
//!
//! # Responsibility
//!
//! An agent artifact is the frozen, self-contained form of one compiled
//! agent: a header (the `AgentIr` as canonical JSON) plus every file the
//! agent reads at runtime, protected by BLAKE3 hashes. The compiler writes
//! it with [`ArtifactBuilder`]; the runner reads it with
//! [`Artifact::from_bytes`] or, appended to its own executable, with
//! [`EmbeddedArtifact::from_current_exe`].
//!
//! This is a leaf crate (only `blake3`, `serde`, `serde_json`). It treats
//! the header as opaque canonical JSON; typed access to `AgentIr` and the
//! header ↔ payload cross-reference checks belong to the layer that owns the
//! IR types. The normative description is `docs/design/agent-artifact-v1.md`;
//! where it and this crate disagree on a byte-level constant, this crate is
//! authoritative.
//!
//! # Byte layout
//!
//! All integers are unsigned little endian, all strings UTF-8 without a
//! terminator, all hashes raw 32-byte BLAKE3 (unkeyed).
//!
//! | Size | Field | Notes |
//! |---|---|---|
//! | 8 | `magic` | [`ARTIFACT_MAGIC`], ASCII `HARWAGNT` |
//! | 2 | `format_version` | [`FORMAT_VERSION`] (`1`) |
//! | 2 | `flags` | [`FORMAT_FLAGS`]; must be `0` in v1 |
//! | 4 | `header_len` | ≤ [`MAX_HEADER_LEN`] |
//! | `header_len` | `header` | [`canonical_json`] bytes, no floating-point numbers |
//! | 4 | `payload_count` | ≤ [`MAX_PAYLOAD_COUNT`] |
//! | variable | `payload_table` | `payload_count` entries, see below |
//! | Σ lengths | `payload_data` | payload bytes, concatenated in table order |
//! | 32 | `artifact_hash` | BLAKE3 over every preceding byte |
//!
//! Payload table entry:
//!
//! | Size | Field | Notes |
//! |---|---|---|
//! | 1 | `kind` | 1 instructions, 2 skill, 3 knowledge, 4 context program, 5 template, `0xFF` other |
//! | 2 + n | `kind_name` | only for `0xFF`: u16 length, then the name ([`PayloadKind::Other`]) |
//! | 2 | `path_len` | ≤ [`MAX_PATH_LEN`] |
//! | `path_len` | `path` | normalized relative path ([`normalize_path`]) |
//! | 8 | `length` | ≤ [`MAX_PAYLOAD_LEN`] |
//! | 32 | `blake3` | BLAKE3 of the payload bytes |
//!
//! Offsets are implicit: payload *n* starts where payload *n − 1* ends, the
//! first right after the table. Entries are strictly sorted by `(kind,
//! path)` (kind by wire tag, `Other` by name), so paths are unique per kind.
//! The whole artifact is at most [`MAX_ARTIFACT_LEN`] bytes; nothing
//! follows `artifact_hash`.
//!
//! # Determinism
//!
//! The bytes depend only on the header value and the set of payloads:
//! the header is canonical JSON, payloads are sorted, and nothing records a
//! time, host, user, absolute path or environment value. Payload bytes are
//! embedded as given. The same inputs in any insertion order give the same
//! bytes and the same [`ArtifactDigest`].
//!
//! # Verification order (fail-closed)
//!
//! [`Artifact::from_bytes`] checks the total size, magic, version and flags,
//! then recomputes `artifact_hash` **before** interpreting any other byte,
//! then the header (size, valid, canonical, no floats), the table (count,
//! kinds, normalized paths, sizes, strict order), each payload hash, and
//! that no bytes remain. The first failure is returned; there is no partial
//! result.
//!
//! # Bundles: shared payload pool and agent entries
//!
//! A compiled agent (with its whole delegation closure) is stored as a
//! [`bundle`]: every file once in a content-addressed pool
//! (`pool/<blake3>`), one entry per agent (`agents/<id>.json`, IR plus
//! references), and a header with the root and the agent index.
//! [`Bundle::from_artifact`] verifies the layout (refs resolve, no
//! unreferenced blob).
//!
//! # Embedding in an executable
//!
//! A built binary is `runner ‖ artifact ‖ footer`; see the [`embed`] module
//! for the 56-byte footer ending in [`FOOTER_MAGIC`] (`HARWAEN2`).
//! [`append_to_executable`] produces it, [`write_executable`] writes it with
//! mode `0o755`, and [`EmbeddedArtifact`] finds and verifies it again. No
//! footer is [`ArtifactError::NotEmbedded`]; any mismatch is
//! [`ArtifactError::Tampered`].
//!
//! # Example
//!
//! ```
//! use harw_agent_artifact::{Artifact, ArtifactBuilder, PayloadKind};
//! # fn main() -> Result<(), harw_agent_artifact::ArtifactError> {
//! let header = serde_json::json!({"schema": "harwness.agent-ir/v2", "name": "reviewer"});
//! let artifact = ArtifactBuilder::new(&header)
//!     .add_payload(PayloadKind::Instructions, "instructions/system.md", b"Be precise.".to_vec())
//!     .add_payload(PayloadKind::Skill, "skills/review/instructions.md", b"# Review".to_vec())
//!     .build()?;
//!
//! let parsed = Artifact::from_bytes(&artifact.to_bytes())?;
//! assert_eq!(parsed.digest(), artifact.digest());
//! let skill = parsed.payload(&PayloadKind::Skill, "skills/review/instructions.md");
//! assert_eq!(skill.map(|p| p.bytes()), Some(&b"# Review"[..]));
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod artifact;
pub mod bundle;
mod canonical;
mod digest;
pub mod embed;
mod error;
mod kind;
mod path;

pub use artifact::{
    ARTIFACT_MAGIC, Artifact, ArtifactBuilder, FORMAT_FLAGS, FORMAT_VERSION, MAX_ARTIFACT_LEN,
    MAX_HEADER_LEN, MAX_PAYLOAD_COUNT, MAX_PAYLOAD_LEN, Payload,
};
pub use bundle::{
    AgentEntry, AgentInput, Bundle, BundleBuilder, BundleError, BundleHeader, ChildLink,
    PayloadRef, PoolStats,
};
pub use canonical::canonical_json;
pub use digest::{ArtifactDigest, InvalidArtifactDigest};
pub use embed::{
    EmbeddedArtifact, FOOTER_LEN, FOOTER_MAGIC, append_to_executable, write_executable,
};
pub use error::{ArtifactError, Limit, PathRejection, TamperScope};
pub use kind::{MAX_KIND_NAME_LEN, PayloadKind};
pub use path::{MAX_PATH_LEN, normalize_path};

#[cfg(test)]
mod test_support;
