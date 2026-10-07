//! Local (Unix socket) identity helpers.
//!
//! The kernel reports the peer's uid through `SO_PEERCRED`. A uid that is not
//! the configured one never gets a connection; the identity is built from
//! that fact alone, never from anything the peer sends.

use harw_session_host::{ClientIdentity, ConnectionId, caps_for_tier, gateway_caps_for_tier};
use harw_types::{
    ApprovalActor, AuthStrength, IngressSurface, PermissionTier, Principal, PrincipalKind,
    TrustZone,
};
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use crate::server::ComServer;

/// The identity of the local user `uid` at `tier`.
///
/// Capabilities are the tier's session ceiling **plus** its gateway ceiling
/// (`gateway_read` from maintainer, `gateway_admin` from owner). Without the
/// gateway part, `gateway.*` is unreachable for every local tier. `tool_call`
/// is never part of a human identity.
#[must_use]
pub fn local_identity(uid: u32, tier: PermissionTier) -> ClientIdentity {
    ClientIdentity {
        principal: Principal::trusted_ingress(
            PrincipalKind::Human,
            format!("uid:{uid}"),
            IngressSurface::Tui,
            tier,
        ),
        tenant: None,
        caps: caps_for_tier(tier).with(gateway_caps_for_tier(tier)),
        device: None,
        actor: ApprovalActor::Operator {
            id: format!("uid:{uid}"),
        },
        label: format!("local-{uid}"),
        zone: TrustZone::Local,
        strength: AuthStrength::PeerCredential,
        connection: ConnectionId::next(),
        agent: None,
    }
}

/// Reads the peer's kernel credentials right after `accept()` and returns the
/// local identity if the peer is `allowed_uid`.
///
/// `None` means: drop the connection before reading anything from it.
#[must_use]
pub fn peer_identity(
    stream: &UnixStream,
    allowed_uid: u32,
    tier: PermissionTier,
) -> Option<ClientIdentity> {
    let credentials = match stream.peer_cred() {
        Ok(credentials) => credentials,
        Err(error) => {
            tracing::warn!(%error, "session com: peer credentials unavailable");
            return None;
        }
    };
    if credentials.uid() != allowed_uid {
        tracing::warn!(uid = credentials.uid(), "session com: peer uid not allowed");
        return None;
    }
    Some(local_identity(credentials.uid(), tier))
}

/// Pause after a failed `accept`, so a persistent error does not spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Accepts connections on `listener` until `shutdown` flips to `true`.
///
/// Each peer's kernel credentials are read first; a uid other than
/// `allowed_uid` is dropped before a byte is read, a full server drops the
/// connection too. Binding the socket (private directory, mode, stale-socket
/// replacement) stays with the caller.
pub async fn serve_unix(
    server: &ComServer,
    listener: UnixListener,
    allowed_uid: u32,
    tier: PermissionTier,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let Some(identity) = peer_identity(&stream, allowed_uid, tier) else {
                        continue;
                    };
                    if let Err(error) = server.serve_io(stream, identity) {
                        tracing::warn!(%error, "session com: connection dropped");
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "session com: accept failed");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_caps_follow_the_tier_and_tool_call_never_appears() {
        let observer = local_identity(1000, PermissionTier::Observer).caps;
        let operator = local_identity(1000, PermissionTier::Operator).caps;
        let maintainer = local_identity(1000, PermissionTier::Maintainer).caps;
        let owner = local_identity(1000, PermissionTier::Owner).caps;
        assert!(observer.observe && !observer.gateway_read && !observer.gateway_admin);
        assert!(operator.steer && !operator.gateway_read);
        assert!(maintainer.gateway_read && !maintainer.gateway_admin);
        assert!(owner.gateway_read && owner.gateway_admin && owner.control);
        for caps in [observer, operator, maintainer, owner] {
            assert!(!caps.tool_call, "a human tier never carries tool_call");
        }
    }

    #[test]
    fn a_local_identity_is_structurally_valid_and_unique() {
        let a = local_identity(1000, PermissionTier::Owner);
        let b = local_identity(1000, PermissionTier::Owner);
        assert!(a.validate().is_ok());
        assert_ne!(a.connection, b.connection);
        assert_eq!(a.label, "local-1000");
    }
}
