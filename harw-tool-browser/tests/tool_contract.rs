use std::future::Future;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use async_trait::async_trait;
use harw_browser::action::{ActionOutcome, ActionRequest, BrowserAction};
use harw_browser::capability::{BrowserCapabilityProbe, CapabilityStatus};
use harw_browser::event::EventEnvelope;
use harw_browser::host::{BrowserHost, BrowserRuntime};
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
    BiDiRequirement, OpenBrowserRequest, OriginPolicy, ProfilePolicy, Viewport,
};
use harw_browser::selector::{Selector, Target};
use harw_browser::session::BrowserSessionHandle;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};
use harw_tool_browser::{
    ActRequest, ActResponse, BrowserOpenGrant, BrowserOpenPolicy, BrowserScopeAction,
    BrowserToolRequest, BrowserToolResponse, BrowserToolSet, CloseRequest, CloseResponse,
    EventsRequest, EventsResponse, FindRequest, FindResponse, ObserveRequest, ObserveResponse,
    OpenRequest, OpenResponse, WaitRequest, WaitResponse,
};

fn session_id() -> BrowserSessionId {
    BrowserSessionId::from_str("00000000-0000-4000-8000-000000000001")
        .expect("fixture session UUID is valid")
}

fn context_id() -> BrowserContextId {
    BrowserContextId::from_str("00000000-0000-4000-8000-000000000002")
        .expect("fixture context UUID is valid")
}

fn open_request() -> OpenRequest {
    OpenRequest {
        request: OpenBrowserRequest {
            start_url: url::Url::parse("https://erp.example.com/inbox")
                .expect("fixture URL is valid"),
            headless: true,
            profile: ProfilePolicy::Ephemeral,
            bidi: BiDiRequirement::Required,
            allowed_origins: OriginPolicy::new(
                vec![
                    "erp.example.com".to_owned(),
                    "assets.example.com".to_owned(),
                ],
                true,
            ),
            viewport: Some(Viewport {
                width: 1440,
                height: 900,
            }),
        },
    }
}

fn trusted_open_policy() -> BrowserOpenPolicy {
    BrowserOpenPolicy::grant(BrowserOpenGrant::ephemeral(
        OriginPolicy::new(
            vec![
                "erp.example.com".to_owned(),
                "assets.example.com".to_owned(),
            ],
            true,
        ),
        OriginPolicy::new(vec!["assets.example.com".to_owned()], true),
    ))
}

fn configured_tools(host: Arc<dyn BrowserHost>) -> BrowserToolSet {
    BrowserToolSet::with_open_policy(host, trusted_open_policy())
}

fn requests() -> Vec<BrowserToolRequest> {
    let revision = BrowserObservationRevision::initial().next();
    let target = Target::new(Selector::TestId("reply".to_owned()));

    vec![
        BrowserToolRequest::Open(open_request()),
        BrowserToolRequest::Observe(ObserveRequest {
            session_id: session_id(),
            context_id: context_id(),
            mode: ObservationMode::InteractiveElements,
        }),
        BrowserToolRequest::Find(FindRequest {
            session_id: session_id(),
            context_id: context_id(),
            target,
            revision,
        }),
        BrowserToolRequest::Act(ActRequest {
            session_id: session_id(),
            request: ActionRequest::new(
                context_id(),
                BrowserAction::Navigate {
                    url: url::Url::parse("https://erp.example.com/ticket/42")
                        .expect("fixture URL is valid"),
                },
            )
            .with_expected_revision(revision),
        }),
        BrowserToolRequest::Wait(WaitRequest {
            session_id: session_id(),
            context_id: context_id(),
            condition: WaitCondition::NavigationComplete,
            timeout: WaitTimeout::from_millis(2_000),
        }),
        BrowserToolRequest::Events(EventsRequest {
            session_id: session_id(),
            since: BrowserEventCursor::zero(),
        }),
        BrowserToolRequest::Close(CloseRequest {
            session_id: session_id(),
        }),
    ]
}

fn observation() -> BrowserObservation {
    BrowserObservation {
        session_id: session_id(),
        context_id: context_id(),
        revision: BrowserObservationRevision::initial(),
        url: url::Url::parse("https://erp.example.com/inbox").expect("fixture URL is valid"),
        title: "Mock inbox".to_owned(),
        document_identity: DocumentIdentity::new("mock-document"),
        elements: Vec::new(),
        artifacts: Vec::new(),
        event_cursor: BrowserEventCursor::zero(),
    }
}

#[test]
fn compact_surface_has_exactly_the_seven_plan_tools() {
    let tools = BrowserToolSet::new(Arc::new(MockHost::new()));
    let descriptors = tools.descriptors();
    let names: Vec<&str> = descriptors
        .iter()
        .map(|descriptor| descriptor.name.as_str())
        .collect();
    let capabilities: Vec<&str> = descriptors
        .iter()
        .map(|descriptor| descriptor.capability.as_str())
        .collect();

    assert_eq!(
        names,
        [
            "browser.open",
            "browser.observe",
            "browser.find",
            "browser.act",
            "browser.wait",
            "browser.events",
            "browser.close",
        ]
    );
    assert_eq!(
        capabilities,
        [
            "harwness.browser.open@1",
            "harwness.browser.observe@1",
            "harwness.browser.find@1",
            "harwness.browser.act@1",
            "harwness.browser.wait@1",
            "harwness.browser.events@1",
            "harwness.browser.close@1",
        ]
    );
    assert!(
        descriptors
            .iter()
            .all(|descriptor| descriptor.input_schema.is_object())
    );
}

#[test]
fn every_request_envelope_round_trips_through_json() {
    for request in requests() {
        let json = serde_json::to_value(&request).expect("typed request serializes");
        let decoded: BrowserToolRequest =
            serde_json::from_value(json).expect("typed request deserializes");
        assert_eq!(decoded, request);
    }
}

#[test]
fn every_response_envelope_round_trips_through_json() {
    let responses = vec![
        BrowserToolResponse::Open(OpenResponse {
            session_id: session_id(),
            primary_context_id: context_id(),
        }),
        BrowserToolResponse::Observe(ObserveResponse {
            observation: observation(),
        }),
        BrowserToolResponse::Find(FindResponse {
            element: ObservedElement::new("reply", "button"),
        }),
        BrowserToolResponse::Act(ActResponse {
            outcome: ActionOutcome::new(EffectId::new()),
        }),
        BrowserToolResponse::Wait(WaitResponse {
            outcome: WaitOutcome::new(true, 0, WaitCondition::NavigationComplete),
        }),
        BrowserToolResponse::Events(EventsResponse { events: Vec::new() }),
        BrowserToolResponse::Close(CloseResponse {
            session_id: session_id(),
        }),
    ];

    for response in responses {
        let json = serde_json::to_value(&response).expect("typed response serializes");
        let decoded: BrowserToolResponse =
            serde_json::from_value(json).expect("typed response deserializes");
        assert_eq!(decoded, response);
    }
}

#[test]
fn preparation_derives_open_origin_and_action_scope() {
    let tools = configured_tools(Arc::new(MockHost::new()));
    let prepared = tools
        .prepare(BrowserToolRequest::Open(open_request()))
        .expect("valid open request prepares");

    assert_eq!(prepared.scope.session_id, None);
    assert_eq!(prepared.scope.context_id, None);
    assert_eq!(prepared.scope.action, BrowserScopeAction::Open);
    assert_eq!(
        prepared.scope.origins,
        [
            "erp.example.com".to_owned(),
            "assets.example.com".to_owned()
        ]
    );
    assert_eq!(prepared.scope.expected_revision, None);
}

#[test]
fn unconfigured_tool_set_rejects_browser_open_fail_closed() {
    let tools = BrowserToolSet::new(Arc::new(MockHost::new()));

    let error = tools
        .prepare(BrowserToolRequest::Open(open_request()))
        .expect_err("default tool set must not grant browser.open authority");

    assert!(matches!(
        error,
        harw_browser::error::Error::OriginNotAllowed { .. }
    ));
}

#[test]
fn host_open_policy_replaces_model_supplied_origins_and_profile() {
    let tools = configured_tools(Arc::new(MockHost::new()));
    let mut request = open_request();
    request.request.allowed_origins = OriginPolicy::new(vec!["evil.example".to_owned()], false);
    request.request.profile = ProfilePolicy::Persistent {
        binding: "model-selected-profile".to_owned(),
    };

    let prepared = tools
        .prepare(BrowserToolRequest::Open(request))
        .expect("host grant authorizes the start URL");

    assert_eq!(
        prepared.scope.origins,
        [
            "erp.example.com".to_owned(),
            "assets.example.com".to_owned(),
        ]
    );
    let BrowserToolRequest::Open(open) = prepared.request() else {
        panic!("prepared request remains browser.open");
    };
    assert_eq!(open.request.profile, ProfilePolicy::Ephemeral);
    assert_eq!(
        open.request.allowed_origins,
        OriginPolicy::new(
            vec![
                "erp.example.com".to_owned(),
                "assets.example.com".to_owned(),
            ],
            true,
        )
    );
}

#[test]
fn preparation_preserves_act_session_context_origin_and_revision_authority() {
    let tools = BrowserToolSet::new(Arc::new(MockHost::new()));
    let revision = BrowserObservationRevision::initial().next();
    let request = BrowserToolRequest::Act(ActRequest {
        session_id: session_id(),
        request: ActionRequest::new(
            context_id(),
            BrowserAction::Navigate {
                url: url::Url::parse("https://erp.example.com/ticket/42")
                    .expect("fixture URL is valid"),
            },
        )
        .with_expected_revision(revision),
    });
    let prepared = tools.prepare(request).expect("valid action prepares");

    assert_eq!(prepared.scope.session_id, Some(session_id()));
    assert_eq!(prepared.scope.context_id, Some(context_id()));
    assert_eq!(prepared.scope.action, BrowserScopeAction::Act);
    assert_eq!(
        prepared.scope.origins,
        ["https://erp.example.com".to_owned()]
    );
    assert_eq!(prepared.scope.expected_revision, Some(revision));
}

#[test]
fn dispatch_routes_open_observe_and_close_through_the_host_contract() {
    let host = Arc::new(MockHost::new());
    let tools = configured_tools(Arc::clone(&host) as Arc<dyn BrowserHost>);

    let opened = block_on(
        tools.dispatch(
            tools
                .prepare(BrowserToolRequest::Open(open_request()))
                .expect("open prepares"),
        ),
    )
    .expect("open dispatch succeeds");
    match opened {
        BrowserToolResponse::Open(response) => {
            assert_eq!(response.session_id, session_id());
            assert_eq!(response.primary_context_id, context_id());
        }
        other => panic!("expected open response, got {other:?}"),
    }

    let observed = block_on(
        tools.dispatch(
            tools
                .prepare(BrowserToolRequest::Observe(ObserveRequest {
                    session_id: session_id(),
                    context_id: context_id(),
                    mode: ObservationMode::PageSummary,
                }))
                .expect("observe prepares"),
        ),
    )
    .expect("observe dispatch succeeds");
    match observed {
        BrowserToolResponse::Observe(response) => {
            assert_eq!(response.observation.session_id, session_id());
            assert_eq!(response.observation.context_id, context_id());
            assert_eq!(response.observation.title, "Mock inbox");
        }
        other => panic!("expected observe response, got {other:?}"),
    }

    let closed = block_on(
        tools.dispatch(
            tools
                .prepare(BrowserToolRequest::Close(CloseRequest {
                    session_id: session_id(),
                }))
                .expect("close prepares"),
        ),
    )
    .expect("close dispatch succeeds");
    match closed {
        BrowserToolResponse::Close(response) => assert_eq!(response.session_id, session_id()),
        other => panic!("expected close response, got {other:?}"),
    }

    assert_eq!(host.calls(), ["open", "session", "close"]);
    assert_eq!(host.observation_count(), 1);
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

struct MockRuntime {
    observations: Arc<AtomicUsize>,
}

#[async_trait]
impl BrowserRuntime for MockRuntime {
    fn session_id(&self) -> BrowserSessionId {
        session_id()
    }

    fn primary_context_id(&self) -> BrowserContextId {
        context_id()
    }

    async fn observe(
        &self,
        requested_context: &BrowserContextId,
        _mode: ObservationMode,
    ) -> harw_browser::Result<BrowserObservation> {
        assert_eq!(*requested_context, context_id());
        self.observations.fetch_add(1, Ordering::Relaxed);
        Ok(observation())
    }

    async fn find(
        &self,
        _context_id: &BrowserContextId,
        _target: &Target,
        _revision: BrowserObservationRevision,
    ) -> harw_browser::Result<ObservedElement> {
        Ok(ObservedElement::new("mock-element", "button"))
    }

    async fn act(&self, _request: ActionRequest) -> harw_browser::Result<ActionOutcome> {
        Ok(ActionOutcome::new(EffectId::new()))
    }

    async fn wait(
        &self,
        _context_id: &BrowserContextId,
        condition: WaitCondition,
        _timeout: WaitTimeout,
    ) -> harw_browser::Result<WaitOutcome> {
        Ok(WaitOutcome::new(true, 0, condition))
    }

    async fn events(&self, _since: BrowserEventCursor) -> harw_browser::Result<Vec<EventEnvelope>> {
        Ok(Vec::new())
    }

    async fn capability_probe(&self) -> harw_browser::Result<BrowserCapabilityProbe> {
        Ok(BrowserCapabilityProbe::new(
            "mock".to_owned(),
            None,
            None,
            true,
            CapabilityStatus::Native,
            CapabilityStatus::Native,
            CapabilityStatus::Native,
            CapabilityStatus::Native,
            CapabilityStatus::Native,
        ))
    }

    async fn install_page_bridge(
        &self,
        request: PageBridgeInstallRequest,
    ) -> harw_browser::Result<PageBridgeInstallationReceipt> {
        Ok(mock_page_bridge_receipt(request))
    }

    async fn close(&self) -> harw_browser::Result<()> {
        Ok(())
    }
}

fn mock_page_bridge_receipt(request: PageBridgeInstallRequest) -> PageBridgeInstallationReceipt {
    let provenance = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        request.context_id(),
        request.bridge_id(),
        request.version(),
        request.script(),
        request.policy().max_payload_bytes(),
        request.policy().max_messages(),
        request.policy().window().as_nanos(),
    );
    PageBridgeInstallationReceipt::new(
        PageBridgeInstallationId::new(),
        request.context_id(),
        request.bridge_id(),
        request.version(),
        deterministic_provenance_digest(&provenance),
    )
}

fn deterministic_provenance_digest(provenance: &str) -> String {
    let mut digest = String::with_capacity(64);
    for seed in [
        0xcbf2_9ce4_8422_2325_u64,
        0x9e37_79b9_7f4a_7c15_u64,
        0xd6e8_feb8_6659_fd93_u64,
        0xa076_1d64_78bd_642f_u64,
    ] {
        let hash = provenance.bytes().fold(seed, |state, byte| {
            (state ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        use std::fmt::Write as _;
        let _ = write!(digest, "{hash:016x}");
    }
    digest
}

struct MockHost {
    runtime: Arc<dyn BrowserRuntime>,
    observations: Arc<AtomicUsize>,
    calls: Mutex<Vec<&'static str>>,
}

impl MockHost {
    fn new() -> Self {
        let observations = Arc::new(AtomicUsize::new(0));
        Self {
            runtime: Arc::new(MockRuntime {
                observations: Arc::clone(&observations),
            }),
            observations,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls
            .lock()
            .expect("call journal lock is healthy")
            .to_vec()
    }

    fn record(&self, call: &'static str) {
        self.calls
            .lock()
            .expect("call journal lock is healthy")
            .push(call);
    }

    fn observation_count(&self) -> usize {
        self.observations.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl BrowserHost for MockHost {
    async fn open(
        &self,
        _request: OpenBrowserRequest,
    ) -> harw_browser::Result<BrowserSessionHandle> {
        self.record("open");
        Ok(BrowserSessionHandle::new(Arc::clone(&self.runtime)))
    }

    async fn session(
        &self,
        requested_session: &BrowserSessionId,
    ) -> harw_browser::Result<BrowserSessionHandle> {
        self.record("session");
        assert_eq!(*requested_session, session_id());
        Ok(BrowserSessionHandle::new(Arc::clone(&self.runtime)))
    }

    async fn close(&self, requested_session: &BrowserSessionId) -> harw_browser::Result<()> {
        self.record("close");
        assert_eq!(*requested_session, session_id());
        Ok(())
    }
}
