//! Explicit, read-only status; never an alternative route for normal sessions.

#[cfg(feature = "onion")]
use super::*;
use crate::MobileOwnerStatus;
#[cfg(not(feature = "onion"))]
use super::{MobileCore, MobileError, ServerProfile};

#[uniffi::export]
impl MobileCore {
    /// Read coarse status once following an explicit user request. Require the
    /// exact current saved pair and the open Onion session before using Tor.
    /// No V1, direct, ordinary RPC, or background retry fallback.
    pub async fn fenced_owner_status(
        &self,
        expected: ServerProfile,
        session_id: String,
    ) -> Result<MobileOwnerStatus, MobileError> {
        #[cfg(feature = "onion")]
        {
            let profile = ValidatedProfile::new(expected)?;
            let session = parse_session_id(session_id)?;
            let hub = profile.owner_status_hub.clone().ok_or_else(not_configured)?;
            let owner = profile.onion.as_ref().ok_or_else(not_configured)?.clone();
            if &profile.expected_node == self.local.node_id()
                || &hub.node == self.local.node_id()
                || profile.profile.server_public_key_hex == self.enrollment.public_key_hex
                || profile.profile.owner_status_hub.as_ref().is_some_and(|pin|
                    pin.public_key_hex == self.enrollment.public_key_hex)
            {
                return Err(MobileError::InvalidProfile {
                    detail: "Owner status needs three distinct device, owner and Hub identities".into(),
                });
            }
            let (link, scope, attachment, attachment_stop, view_scope) = {
                let mut state = self.shared.state();
                let link = state.active()?;
                if !current(&state, &link, &profile, &session) {
                    return Err(MobileError::Disconnected);
                }
                let scope = state.owner_status_scope.clone();
                let view = state.session_mut(&session)?;
                let view_scope = view.owner_status_scope().ok_or(MobileError::Disconnected)?;
                let worker = view.worker.as_ref().ok_or_else(unopened)?;
                (link, scope, worker.task.id(), worker.stop.clone(), view_scope)
            };
            // Only one explicit status request per mobile core at a time.
            let slot = self.shared.owner_status_active
                .get_or_init(|| Arc::new(Semaphore::new(1))).clone()
                .try_acquire_owned().map_err(|_| MobileError::Denied {
                    detail: "There is already an owner status request".into(),
                })?;
            let local = self.local.clone();
            let shared = Arc::clone(&self.shared);
            let stop = link.stop.clone();
            let runtime = self.runtime.as_ref().ok_or_else(|| internal("Runtime stopped"))?;
            let work = runtime.spawn(async move {
                let _permit = slot;
                let still_current = || {
                    let state = shared.state();
                    !scope.is_cancelled() && !attachment_stop.is_cancelled() && !view_scope.is_cancelled()
                        && current(&state, &link, &profile, &session)
                        && state.session(&session).ok().and_then(|view| view.worker.as_ref())
                            .is_some_and(|worker| worker.task.id() == attachment)
                };
                tokio::select! {
                    biased;
                    () = stop.cancelled() => Err(MobileError::Disconnected),
                    () = scope.cancelled() => Err(MobileError::Disconnected),
                    () = attachment_stop.cancelled() => Err(MobileError::Disconnected),
                    () = view_scope.cancelled() => Err(MobileError::Disconnected),
                    result = async {
                        if !still_current() {
                            return Err(MobileError::Disconnected);
                        }
                        let device = harw_session_remote::OwnerStatusDevice::from_enrolled_node(&local)
                            .map_err(|_| unavailable())?;
                        let (owner_io, hub_io) = shared.dialer.dial_owner_status_pair(&owner, &hub.onion).await?;
                        if !still_current() {
                            return Err(MobileError::Disconnected);
                        }
                        let transport = route::onion_transport_options();
                        let mut options = ConnectOptions::new("Harw Android owner status");
                        options.limits.hello_timeout = options.limits.hello_timeout.max(route::ONION_HELLO_TIMEOUT);
                        let (mut owner_link, mut hub_link) = tokio::try_join!(
                            device.connect_owner_over(owner_io, &profile.expected_node, &profile.verifier, options, &transport),
                            device.connect_hub_over(hub_io, &hub.node, &hub.verifier, &transport),
                        ).map_err(|_| unavailable())?;
                        if !still_current() {
                            return Err(MobileError::Disconnected);
                        }
                        // Neither bootstrap nor the independent handshakes spend the
                        // short owner reservation, which is negotiated only once here.
                        let reservation = owner_link.reserve(session.clone()).await.map_err(|_| unavailable())?;
                        if !still_current() {
                            return Err(MobileError::Disconnected);
                        }
                        let status = reservation.read(&mut hub_link).await.map_err(|_| unavailable())?;
                        if !still_current() {
                            return Err(MobileError::Disconnected);
                        }
                        use harw_protocol::session_wire::OwnerObservationStatus as Wire;
                        Ok(match status {
                            Wire::Idle => MobileOwnerStatus::Idle,
                            Wire::Active => MobileOwnerStatus::Active,
                            Wire::Interrupted => MobileOwnerStatus::Interrupted,
                            Wire::Failed => MobileOwnerStatus::Failed,
                        })
                    } => result,
                }
            });
            tokio_util::task::AbortOnDropHandle::new(work).await
                .map_err(|_| internal("Owner status task stopped"))?
        }
        #[cfg(not(feature = "onion"))]
        {
            let _ = (expected, session_id);
            Err(MobileError::Denied { detail: "Owner status requires Tor support".into() })
        }
    }
}

#[cfg(feature = "onion")]
fn current(state: &CoreState, link: &Arc<Link>, profile: &ValidatedProfile, session: &SessionId) -> bool {
    state.phase == ConnectionPhase::Connected
        && state.same_link(link)
        && link.backend.kind() == BackendKind::Remote
        && link.backend.granted().observe
        && state.route == Some(ConnectionRoute::Onion)
        && state.profile.as_ref().is_some_and(|selected| selected.profile == profile.profile)
        && state.sessions.get(session).is_some_and(SessionState::can_read_owner_status)
        && !state.detaching.contains(session)
}

#[cfg(feature = "onion")]
fn not_configured() -> MobileError {
    MobileError::InvalidProfile { detail: "No independently pinned Owner Status Hub in this profile".into() }
}

#[cfg(feature = "onion")]
fn unavailable() -> MobileError {
    MobileError::Session { detail: "The pinned owner and Hub did not confirm owner status".into() }
}
