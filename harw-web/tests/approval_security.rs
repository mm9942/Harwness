//! Integrationstests der öffentlichen Freigabe-API von `harw-web` (A-APPR).
//!
//! Belegt über die Crate-Wurzel-Re-Exporte:
//! - ohne Principal-Actor wird abgelehnt, der Speicher bleibt unberührt;
//! - Web-Principals brauchen einen bestätigten `SO_PEERCRED`-Peer;
//! - `resolved_at` stammt aus der übergebenen Serveruhr;
//! - abgelaufene Anfragen sind nicht auflösbar und nicht gelistet.

use harw_session_store::approval::{ApprovalRecord, ApprovalStore};
use harw_session_store::error::SessionStoreError;
use harw_types::{
    ApprovalActor, Clock, IngressSurface, ItemId, PermissionTier, Principal, PrincipalKind,
    ReviewDecision, SessionId, ToolCallId,
};
use harw_web::{
    ApprovalCaller, PeerCredentials, SecurityError, StaticUidApprovalActorMap,
    list_pending_approvals, resolve_approval,
};
use jiff::{SignedDuration, Timestamp};

// Feste Serveruhr für deterministische Zeitstempel.
struct FixedClock(Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

fn issued_at() -> Timestamp {
    Timestamp::constant(1_700_000_000, 0)
}

fn clock_after(minutes: i64) -> FixedClock {
    // Testhilfe: der Wertebereich wird nie verlassen.
    FixedClock(
        issued_at()
            .checked_add(SignedDuration::from_mins(minutes))
            .expect("timestamp in range"),
    )
}

fn issue(store: &ApprovalStore) -> (SessionId, ItemId) {
    let session = SessionId::from_str("session-1");
    let request = ItemId::from_str("approval-1");
    store
        .issue(&ApprovalRecord {
            request: request.clone(),
            session: session.clone(),
            call_id: ToolCallId::from_str("call-1"),
            actor: ApprovalActor::Operator {
                id: "owner".to_owned(),
            },
            issued_at: issued_at(),
        })
        .expect("issue");
    (session, request)
}

fn web_principal() -> Principal {
    Principal::trusted_ingress(
        PrincipalKind::Human,
        "uid:1000",
        IngressSurface::Web,
        PermissionTier::Owner,
    )
}

#[test]
fn test_resolve_approval_model_principal_is_rejected_and_store_untouched() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = ApprovalStore::new(temp.path());
    let (session, request) = issue(&store);
    let model = Principal::trusted_ingress(
        PrincipalKind::Model,
        "root",
        IngressSurface::Tui,
        PermissionTier::Owner,
    );

    let error = resolve_approval(
        &store,
        &ApprovalCaller::new(&model),
        &session,
        &request,
        ReviewDecision::Approved,
        None,
        &clock_after(1),
    )
    .expect_err("model has no approver actor");

    assert!(matches!(error, SecurityError::NoApproverActor { .. }));
    assert_eq!(store.resolution(&session, &request).expect("readable"), None);
}

#[test]
fn test_resolve_approval_web_principal_requires_confirmed_peer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = ApprovalStore::new(temp.path());
    let (session, request) = issue(&store);
    let principal = web_principal();

    let error = resolve_approval(
        &store,
        &ApprovalCaller::new(&principal),
        &session,
        &request,
        ReviewDecision::Approved,
        None,
        &clock_after(1),
    )
    .expect_err("no peer");
    assert!(matches!(error, SecurityError::UnauthenticatedWebPeer));

    let table = StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())]);
    let stranger = PeerCredentials::new(7, 4242, 4242);
    let error = resolve_approval(
        &store,
        &ApprovalCaller::new(&principal).with_web_peer(&stranger, &table),
        &session,
        &request,
        ReviewDecision::Approved,
        None,
        &clock_after(1),
    )
    .expect_err("unknown peer");
    assert!(matches!(error, SecurityError::UnknownApprover { uid: 4242 }));
    assert_eq!(store.resolution(&session, &request).expect("readable"), None);
}

#[test]
fn test_resolve_approval_resolved_at_comes_from_server_clock() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = ApprovalStore::new(temp.path());
    let (session, request) = issue(&store);
    let principal = web_principal();
    let table = StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())]);
    let peer = PeerCredentials::new(7, 1000, 1000);
    let clock = clock_after(12);

    let resolution = resolve_approval(
        &store,
        &ApprovalCaller::new(&principal).with_web_peer(&peer, &table),
        &session,
        &request,
        ReviewDecision::Rejected,
        None,
        &clock,
    )
    .expect("resolves");

    assert_eq!(resolution.resolved_at, clock.0);
    assert_eq!(
        resolution.actor,
        ApprovalActor::Operator {
            id: "owner".to_owned()
        }
    );
}

#[test]
fn test_expired_request_is_not_listed_and_not_resolvable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = ApprovalStore::new(temp.path());
    let (session, request) = issue(&store);
    let principal = web_principal();
    let table = StaticUidApprovalActorMap::new(vec![(1000, "owner".to_owned())]);
    let peer = PeerCredentials::new(7, 1000, 1000);
    let expired = clock_after(30);

    assert_eq!(list_pending_approvals(&store, 10, &clock_after(29)).expect("list").len(), 1);
    assert!(list_pending_approvals(&store, 10, &expired).expect("list").is_empty());

    let error = resolve_approval(
        &store,
        &ApprovalCaller::new(&principal).with_web_peer(&peer, &table),
        &session,
        &request,
        ReviewDecision::Approved,
        None,
        &expired,
    )
    .expect_err("expired");
    assert!(matches!(
        error,
        SecurityError::Store(SessionStoreError::ApprovalExpired { .. })
    ));
}
