//! Correlation selectors and fixed direct request for an exact owner
//! placement read. Selectors do not grant authority: independent device and
//! owner channel authentication and current authorization are mandatory.

use std::num::NonZeroU64;

use harw_types::SessionId;

/// Fixed, non-upgrading Hub-to-owner observation proof route.
pub const OWNER_PROOF_PATH: &str = "/v1/owner/observe-proof";

/// A one-use, nonzero Hub-issued selector. Do not log or persist it.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct OwnerPlacementLookupId([u8; 32]);

impl OwnerPlacementLookupId {
    /// Decodes a selector; only the Hub can authorize it for the operation.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Option<Self> {
        (bytes != [0; 32]).then_some(Self(bytes))
    }

    /// Bytes for the closed, authenticated lookup protocol only.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Ordered one-use selectors for a single separately authorized local read.
/// An accepting Hub must enforce that the second succeeds only after the
/// first; this wrapper's ordering does not replace that remote check.
pub struct OwnerPlacementLookupPair {
    before: OwnerPlacementLookupId,
    after: OwnerPlacementLookupId,
}

impl OwnerPlacementLookupPair {
    #[must_use]
    pub fn new(before: OwnerPlacementLookupId, after: OwnerPlacementLookupId) -> Option<Self> {
        (before != after).then_some(Self { before, after })
    }

    #[must_use]
    pub fn into_ids(self) -> (OwnerPlacementLookupId, OwnerPlacementLookupId) {
        (self.before, self.after)
    }
}

/// The phase authenticated in the Hub's one-use response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerPlacementLookupPhase {
    Before,
    After,
}

/// Short-lived selector minted on one live owner Session WebSocket. This is
/// not an observation grant; the owner compares the original connection, the
/// independently authenticated Hub scope and current local authorization.
#[derive(PartialEq, Eq)]
pub struct OwnerObservationReservationId([u8; 32]);

impl OwnerObservationReservationId {
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Option<Self> {
        (bytes != [0; 32]).then_some(Self(bytes))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Returned only to the original authorized device on its owner connection.
/// Expiration is enforced by the owner's monotonic clock, never by the wire.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerObservationReservationReceipt {
    reservation_id: [u8; 32],
}

impl OwnerObservationReservationReceipt {
    #[must_use]
    pub fn new(id: OwnerObservationReservationId) -> Self {
        Self { reservation_id: *id.as_bytes() }
    }

    #[must_use]
    pub fn into_id(self) -> Option<OwnerObservationReservationId> {
        OwnerObservationReservationId::from_bytes(self.reservation_id)
    }
}

/// The Hub's expected incarnation, to be compared and never used as
/// authority. The owner extracts all identities from its live connection.
pub struct DirectOwnerStatusExpectation {
    pub session_id: SessionId,
    pub owner_instance: String,
    pub placement_generation: NonZeroU64,
    pub owner_epoch: NonZeroU64,
}

/// Parameters of the optional, separately negotiated fenced direct status
/// read. These selectors are sent only on the selected owner's already
/// authenticated Session WebSocket. It deliberately accepts no identity or
/// address fields and is not safe to log or cache.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FencedDirectOwnerStatus {
    session_id: SessionId,
    owner_instance: String,
    placement_generation: NonZeroU64,
    owner_epoch: NonZeroU64,
    reservation_id: [u8; 32],
    before_id: [u8; 32],
    after_id: [u8; 32],
}

impl FencedDirectOwnerStatus {
    #[must_use]
    pub fn new(
        expected: DirectOwnerStatusExpectation,
        pair: OwnerPlacementLookupPair,
        reservation: OwnerObservationReservationId,
    ) -> Option<Self> {
        if !valid_instance(&expected.owner_instance) {
            return None;
        }
        let (before, after) = pair.into_ids();
        Some(Self {
            session_id: expected.session_id,
            owner_instance: expected.owner_instance,
            placement_generation: expected.placement_generation,
            owner_epoch: expected.owner_epoch,
            reservation_id: *reservation.as_bytes(),
            before_id: *before.as_bytes(),
            after_id: *after.as_bytes(),
        })
    }

    #[must_use]
    pub fn into_selection(self) -> Option<(
        DirectOwnerStatusExpectation, OwnerPlacementLookupPair, OwnerObservationReservationId,
    )> {
        if !valid_instance(&self.owner_instance) {
            return None;
        }
        let pair = OwnerPlacementLookupPair::new(
            OwnerPlacementLookupId::from_bytes(self.before_id)?,
            OwnerPlacementLookupId::from_bytes(self.after_id)?,
        )?;
        Some((DirectOwnerStatusExpectation {
            session_id: self.session_id,
            owner_instance: self.owner_instance,
            placement_generation: self.placement_generation,
            owner_epoch: self.owner_epoch,
        }, pair, OwnerObservationReservationId::from_bytes(self.reservation_id)?))
    }
}

fn valid_instance(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b':'))
}
