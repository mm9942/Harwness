//! Integration test exercising the crate's public API end-to-end: a policy
//! check gates an `OpenBrowserRequest`, a mock `BrowserRuntime` is wrapped in a
//! `BrowserSessionHandle`, and a full act -> observe -> events -> wait cycle is
//! driven through the handle. Every value that would cross a real
//! protocol/tool boundary is round-tripped through `serde_json` to confirm the
//! whole chain of types (action, artifact, diagnostic, event, host,
//! observation, policy, selector, session, wait) stays consistent together,
//! not just in isolation.

use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;

use harw_browser::action::{ActionOutcome, ActionRequest, BrowserAction};
use harw_browser::artifact::{ArtifactKind, ArtifactRef};
use harw_browser::capability::{BrowserCapabilityProbe, CapabilityStatus};
use harw_browser::diagnostic::{BrowserDiagnostic, DiagnosticCategory, DiagnosticSource, Severity};
use harw_browser::error::Result;
use harw_browser::event::{BackpressureStats, BrowserEvent, EventClass, EventEnvelope};
use harw_browser::host::BrowserRuntime;
use harw_browser::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId, EffectId,
};
use harw_browser::observation::{
    BrowserObservation, DocumentIdentity, ObservationMode, ObservedElement,
};
use harw_browser::page_bridge::{
    PageBridgeInstallRequest, PageBridgeInstallationId, PageBridgeInstallationReceipt,
};
use harw_browser::policy::{
    BiDiRequirement, BrowserLimits, OpenBrowserRequest, OriginPolicy, ProfilePolicy,
};
use harw_browser::selector::{Selector, Target};
use harw_browser::session::BrowserSessionHandle;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};

// Every future in this test resolves on the first poll, since the mock runtime
// below contains no real `.await` suspension points, so the standard-library
// no-op waker is sufficient and no hand-rolled vtable is needed.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = std::pin::pin!(fut);
    loop {
        if let Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
            return value;
        }
    }
}

// A mock runtime standing in for a real WebDriver/BiDi backend, driven purely
// through the crate's public `BrowserRuntime` trait.
struct MockRuntime {
    session_id: BrowserSessionId,
    primary_context_id: BrowserContextId,
}

#[async_trait]
impl BrowserRuntime for MockRuntime {
    fn session_id(&self) -> BrowserSessionId {
        self.session_id
    }

    fn primary_context_id(&self) -> BrowserContextId {
        self.primary_context_id
    }

    async fn observe(
        &self,
        context_id: &BrowserContextId,
        _mode: ObservationMode,
    ) -> Result<BrowserObservation> {
        Ok(BrowserObservation {
            session_id: self.session_id,
            context_id: *context_id,
            revision: BrowserObservationRevision::initial().next(),
            url: url::Url::parse("https://erp.example.com/dashboard").expect("valid static url"),
            title: "Dashboard".to_owned(),
            document_identity: DocumentIdentity::new("doc-lifecycle"),
            elements: vec![ObservedElement::new("ref-submit", "button")],
            artifacts: vec![ArtifactRef::new(ArtifactKind::Screenshot, "image/png", 256)],
            event_cursor: BrowserEventCursor::zero().next(),
        })
    }

    async fn find(
        &self,
        _context_id: &BrowserContextId,
        _target: &Target,
        _revision: BrowserObservationRevision,
    ) -> Result<ObservedElement> {
        Ok(ObservedElement::new("ref-submit", "button"))
    }

    async fn act(&self, request: ActionRequest) -> Result<ActionOutcome> {
        let mut outcome = ActionOutcome::new(EffectId::new());
        outcome.new_revision = Some(BrowserObservationRevision::initial().next());
        outcome.confirmed = true;
        outcome.diagnostics.push(
            BrowserDiagnostic::new(
                Severity::Low,
                DiagnosticSource::Automation,
                DiagnosticCategory::UnconfirmedAction,
                format!("processed action for context {}", request.context_id),
            )
            .with_evidence(ArtifactRef::new(ArtifactKind::Text, "text/plain", 4))
            .with_effect(outcome.effect_id),
        );
        Ok(outcome)
    }

    async fn wait(
        &self,
        _context_id: &BrowserContextId,
        condition: WaitCondition,
        _timeout: WaitTimeout,
    ) -> Result<WaitOutcome> {
        Ok(WaitOutcome::new(true, 42, condition))
    }

    async fn events(&self, since: BrowserEventCursor) -> Result<Vec<EventEnvelope>> {
        let mut stats = BackpressureStats::default();
        stats.record_drop();
        assert_eq!(stats.dropped, 1);

        Ok(vec![EventEnvelope::new(
            harw_browser::ids::EventId::new(),
            since.next(),
            self.session_id,
            EventClass::Critical,
            BrowserEvent::Navigation {
                context_id: BrowserContextId::new(),
                url: "https://erp.example.com/dashboard".to_owned(),
                title: Some("Dashboard".to_owned()),
            },
        )])
    }

    async fn capability_probe(&self) -> Result<BrowserCapabilityProbe> {
        Ok(BrowserCapabilityProbe::new(
            "chrome".to_owned(),
            Some("120.0".to_owned()),
            Some("120.0.1".to_owned()),
            true,
            CapabilityStatus::Native,
            CapabilityStatus::Native,
            CapabilityStatus::Native,
            CapabilityStatus::Fallback,
            CapabilityStatus::Unavailable,
        ))
    }

    async fn install_page_bridge(
        &self,
        request: PageBridgeInstallRequest,
    ) -> Result<PageBridgeInstallationReceipt> {
        Ok(PageBridgeInstallationReceipt::new(
            PageBridgeInstallationId::new(),
            request.context_id(),
            request.bridge_id(),
            request.version(),
            "mock-page-bridge-script-sha256",
        ))
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

#[test]
fn test_full_session_lifecycle_round_trips_across_modules() {
    // 1. Build an open request gated by an origin policy, and confirm the
    //    policy allows the intended start URL before "opening" the session.
    let allowed_origins = OriginPolicy::from_origins(["https://erp.example.com"], true)
        .expect("valid allowed-origins policy");
    let open_request = OpenBrowserRequest {
        start_url: url::Url::parse("https://erp.example.com/login").expect("valid url"),
        headless: true,
        profile: ProfilePolicy::Persistent {
            binding: "sales-team".to_owned(),
        },
        bidi: BiDiRequirement::Required,
        allowed_origins,
        authentication_origins: OriginPolicy::default(),
        viewport: None,
        limits: BrowserLimits::default(),
    };
    assert!(
        open_request
            .allowed_origins
            .is_allowed(&open_request.start_url)
    );

    // The request itself is a protocol-boundary type; confirm it survives a
    // JSON round trip before it would be handed to a real host implementation.
    let open_request_json = serde_json::to_string(&open_request).expect("open request serializes");
    let decoded_open_request: OpenBrowserRequest =
        serde_json::from_str(&open_request_json).expect("open request deserializes");
    assert_eq!(decoded_open_request, open_request);

    // 2. Stand up a mock runtime wrapped in the public session handle, as a
    //    real `BrowserHost::open` implementation would.
    let session_id = BrowserSessionId::new();
    let primary_context_id = BrowserContextId::new();
    let runtime: Arc<dyn BrowserRuntime> = Arc::new(MockRuntime {
        session_id,
        primary_context_id,
    });
    let handle = BrowserSessionHandle::new(runtime);
    assert_eq!(handle.id(), session_id);
    assert_eq!(handle.primary_context_id(), primary_context_id);

    let context_id = BrowserContextId::new();

    // 3. Act on the page and confirm the outcome (including its nested
    //    diagnostic and evidence) round-trips through JSON exactly.
    let action_request = ActionRequest::new(
        context_id,
        BrowserAction::Click {
            target: Target::new(Selector::TestId("submit".to_owned())),
        },
    )
    .with_expected_revision(BrowserObservationRevision::initial());

    let outcome = block_on(handle.act(action_request)).expect("mock act always succeeds");
    assert!(outcome.confirmed);
    assert_eq!(outcome.diagnostics.len(), 1);
    assert_eq!(outcome.diagnostics[0].effect_id, Some(outcome.effect_id));

    let outcome_json = serde_json::to_string(&outcome).expect("outcome serializes");
    let decoded_outcome: ActionOutcome =
        serde_json::from_str(&outcome_json).expect("outcome deserializes");
    assert_eq!(decoded_outcome, outcome);

    // 4. Observe the resulting page state and confirm the observation
    //    round-trips through JSON, matching the revision the action advanced to.
    let observation = block_on(handle.observe(&context_id, ObservationMode::PageSummary))
        .expect("mock observe always succeeds");
    assert_eq!(
        observation.revision,
        outcome.new_revision.expect("act set a new revision")
    );

    let observation_json = serde_json::to_string(&observation).expect("observation serializes");
    let decoded_observation: BrowserObservation =
        serde_json::from_str(&observation_json).expect("observation deserializes");
    assert_eq!(decoded_observation, observation);

    // 5. Drain events since the observation's cursor and confirm the envelope
    //    (carrying a real timestamp) round-trips through JSON.
    let events = block_on(handle.events(observation.event_cursor)).expect("mock events succeeds");
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].event, BrowserEvent::Navigation { .. }));

    let events_json = serde_json::to_string(&events).expect("events serialize");
    let decoded_events: Vec<EventEnvelope> =
        serde_json::from_str(&events_json).expect("events deserialize");
    assert_eq!(decoded_events, events);

    // 6. Wait for a follow-up condition and close the session.
    let wait_outcome = block_on(handle.wait(
        &context_id,
        WaitCondition::NavigationComplete,
        WaitTimeout::from_millis(1000),
    ))
    .expect("mock wait always succeeds");
    assert!(wait_outcome.satisfied);

    assert!(block_on(handle.close()).is_ok());
}
