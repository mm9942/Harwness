//! `harw-channel` — the transport-agnostic channel-ingress core.
//!
//! Spec source: `docs/design/channel-ingress-telegram.md` (§2, §6.1) plus the
//! Phase-1 shared contracts. This crate owns everything upstream of session
//! resolution that is *not* specific to one transport: the [`ChannelAdapter`]
//! behavior contract, the structural [`SessionKey`], the admission verdict
//! ([`Admission`]), channel [`ChannelCapabilities`], the shared pairing-code
//! primitive ([`PairingCode`]/[`PairingRegistry`], backed by
//! `harw-session-store`), the generic [`dispatch_inbound`] pipeline, and the
//! shared capability-driven [`chunk_text`] mechanism. Concrete bindings (e.g.
//! `harw-channel-telegram`) implement [`ChannelAdapter`] and own their own
//! transport, markdown escaping, and rate-limit numbers (§7).
//!
//! A channel is a *perimeter, not a trust boundary override* (§1): an adapter
//! can only reduce what a session may do, never grant capabilities.
//!
//! # Concurrency
//! [`ChannelAdapter`] is `Send + Sync` so a binding can be driven from a
//! dedicated ingress thread; the trait is synchronous by design (§6.2), so the
//! core imposes no async runtime. Token storage integrates with
//! `harw-secrets::SecretStore` (§1a) and pairing durability with
//! `harw-session-store` (§3.2).
//!
//! # Errors
//! All fallible core paths return [`ChannelError`] / [`ChannelResult`].
//!
//! Implemented: session-key canonicalization, pairing-code generation/expiry/
//! validation (durably via `pairing_store`), message chunking and admission
//! classification.

#![forbid(unsafe_code)]

pub mod adapter;
pub mod dispatch;
pub mod error;
pub mod event;
pub mod ids;
pub mod pairing;
pub mod pairing_store;

pub use adapter::{
    AttachmentSupport, ChannelAdapter, ChannelCapabilities, MarkdownSupport, ThreadSupport,
    chunk_text,
};
pub use dispatch::{Dispatch, dispatch_inbound};
pub use error::{ChannelError, ChannelResult};
pub use event::{
    Admission, ApprovalAction, ApprovalPrompt, AttachmentRef, ChannelSendOp, DeferralReason,
    InboundEvent, InlineAction, OutboundContent, RejectionReason, SenderRef,
};
pub use ids::{ChannelId, PeerId, SessionKey, TenantId, ThreadRef};
pub use pairing::{PairingCode, PairingRecord, PairingRegistry};
pub use pairing_store::PairingStore;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
