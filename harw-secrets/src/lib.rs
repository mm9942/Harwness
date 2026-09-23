//! `harw-secrets` — DEK/KEK envelope sealing plus an append-only, hash-chained,
//! ML-DSA-checkpointed audit log.
//!
//! Spec source: `docs/design/secrets-and-audit.md`. This crate owns three
//! concerns that share the root problem "prove nothing was read or changed
//! without authorization":
//!
//! - **Crate-encryption / envelope** ([`envelope`], [`record`], [`policy`]):
//!   per-secret DEK sealed under a deployment hybrid ML-KEM KEK via `crypt_guard`
//!   (reine ML-KEM-Stufen sind nur noch als `KemAlgo::Legacy*` lesbar, nicht öffenbar).
//! - **Key management** ([`kek`]): KEK provenance (key file / OS keyring /
//!   env seed) and the `0600` refuse-to-start permission check.
//! - **Audit** ([`audit`]): hash-chained [`audit::event::AuditEvent`] log with
//!   periodic ML-DSA-signed chain-head checkpoints, plus a second,
//!   hostexternen Beobachtungsweg ([`audit::mirror`], AW7-04) für den Fall
//!   einer Host-Übernahme.
//!
//! All cryptography is delegated to `crypt_guard`; this crate never depends on
//! `ml-kem`, `ml-dsa`, `hkdf`, or the raw AEAD crates directly (§2.1). In-memory
//! secret material uses `secrecy::SecretString` / `secrecy::SecretBox<[u8]>`.
//!
//! Status: envelope encryption, the hash-chained audit log and ML-DSA-65
//! signed checkpoints (`SecretStore::sign_checkpoint` /
//! `SecretStore::verify_checkpoints`) are implemented; the checkpoint signing
//! key is supplied by the caller and is distinct from the KEK.

pub mod audit;
pub mod envelope;
pub mod error;
pub mod id;
pub mod kek;
pub mod policy;
pub mod record;
pub mod store;
#[cfg(test)]
mod test_support;

pub use audit::chain::AuditLog;
pub use audit::checkpoint::{Checkpoint, CheckpointLog};
pub use audit::event::{Actor, AuditEvent, AuditEventId, SubjectRef};
pub use audit::mirror::{
    ChainAuditMirror, ChainBreakAlert, MirrorEntry, MirrorTransport, RecordingMirrorTransport,
};
pub use envelope::SealedSecret;
pub use error::{AuditError, AuditResult, MirrorError, MirrorResult, SecretsError, SecretsResult};
pub use id::{KeyVersion, SecretId};
pub use kek::{KekProvenance, derive_public_key, derive_secret_key, load_kek_material, load_seed};
pub use policy::{AeadAlgo, CryptoPolicy, KemAlgo};
pub use record::{SecretEnvelopeFormat, SecretMetadata, SecretRecord};
pub use store::{KekMaterial, SecretStore};
