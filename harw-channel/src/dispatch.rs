//! The generic ingress -> admit -> session-key -> dispatch pipeline (spec §6.1).
//!
//! Transport-agnostic glue the harness core drives: it routes an inbound event
//! to a would-be `SessionKey`, runs the adapter's admission gate before that key
//! is trusted for anything beyond routing (§2.3), and classifies the outcome.
//! Rejected/deferred events never create session-store state.

use crate::adapter::ChannelAdapter;
use crate::error::{ChannelError, ChannelResult};
use crate::event::{Admission, DeferralReason, InboundEvent, RejectionReason};
use crate::ids::SessionKey;

/// The classified outcome of running one inbound event through the pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatch {
    /// Admitted: the resolved session key and the (moved) originating event.
    Admitted {
        /// The session address the event resolves to.
        key: SessionKey,
        /// The originating event, forwarded to the session runtime.
        /// Boxed to keep all variants close in size (InboundEvent is large).
        event: Box<InboundEvent>,
    },
    /// Rejected at admission with a loggable reason; no session touched.
    Rejected(RejectionReason),
    /// Deferred pending an out-of-band step (e.g. pairing onboarding).
    Deferred(DeferralReason),
}

/// Runs one inbound event through the admission pipeline for `adapter` (§2.3).
///
/// Computes a would-be `SessionKey` via the adapter's pure derivation, then
/// applies the admission gate; only an `Admitted` verdict yields a dispatchable
/// key. Binding errors surface as [`ChannelError`] via the `From<A::Error>`
/// bound the core requires at the pipeline edge (§6.3).
///
/// # Errors
/// Returns whatever [`ChannelError`] the adapter's `derive_session_key` maps to.
pub fn dispatch_inbound<A>(adapter: &A, event: InboundEvent) -> ChannelResult<Dispatch>
where
    A: ChannelAdapter,
    ChannelError: From<A::Error>,
{
    let key = adapter.derive_session_key(&event)?;
    match adapter.admit(&event, &key) {
        Admission::Admitted => Ok(Dispatch::Admitted {
            key,
            event: Box::new(event),
        }),
        Admission::Rejected(reason) => Ok(Dispatch::Rejected(reason)),
        Admission::Deferred(reason) => Ok(Dispatch::Deferred(reason)),
    }
}
