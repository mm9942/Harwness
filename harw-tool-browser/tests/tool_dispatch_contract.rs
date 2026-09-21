//! Dispatch contract of `BrowserToolSet` against a multi-session fake host:
//! ownership, budgets, re-validation, observed-location post-check and
//! session-end cleanup (W5 B-TOOL).

use std::collections::HashMap;
use std::future::Future;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;
use harw_browser::action::{ActionOutcome, ActionRequest, BrowserAction};
use harw_browser::capability::{BrowserCapabilityProbe, CapabilityStatus};
use harw_browser::error::Error;
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
use harw_browser::policy::{BiDiRequirement, BrowserLimits, OpenBrowserRequest, OriginPolicy};
use harw_browser::selector::{Selector, Target};
use harw_browser::session::BrowserSessionHandle;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};
use harw_tool_browser::{
    ActRequest, BrowserOpenGrant, BrowserOpenPolicy, BrowserToolRequest, BrowserToolResponse,
    BrowserToolSet, CloseRequest, EventsRequest, FindRequest, ObserveRequest, OpenRequest,
    WaitRequest, prepare_browser_call,
};

const OWNER_A: &str = "harness-session-a";
const OWNER_B: &str = "harness-session-b";

// ── Fixtures (tests may use expect: a failure is a fixture bug) ─────────────

fn url(text: &str) -> url::Url {
    url::Url::parse(text).expect("fixture URL is valid")
}

fn numbered<T: FromStr>(n: u64) -> T
where
    T::Err: std::fmt::Debug,
{
    T::from_str(&format!("00000000-0000-4000-8000-{n:012}")).expect("fixture UUID is valid")
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("fixture lock is healthy")
}

fn tools_with(limits: BrowserLimits) -> (Arc<FakeHost>, BrowserToolSet) {
    let host = Arc::new(FakeHost::default());
    let grant = BrowserOpenGrant::ephemeral(
        OriginPolicy::from_origins(["https://erp.example.com"], true).expect("valid policy"),
        OriginPolicy::from_origins(["https://sso.example.org"], true).expect("valid policy"),
    )
    .with_limits(limits);
    let tools = BrowserToolSet::with_open_policy(
        Arc::clone(&host) as Arc<dyn BrowserHost>,
        BrowserOpenPolicy::grant(grant),
    );
    (host, tools)
}

fn tools() -> (Arc<FakeHost>, BrowserToolSet) {
    tools_with(BrowserLimits::default())
}

fn run(tools: &BrowserToolSet, owner: &str, request: BrowserToolRequest) -> harw_browser::Result<BrowserToolResponse> {
    let prepared = tools.prepare(request)?;
    block_on(tools.dispatch(owner, prepared))
}

fn open(tools: &BrowserToolSet, owner: &str) -> (BrowserSessionId, BrowserContextId) {
    let response = run(
        tools,
        owner,
        BrowserToolRequest::Open(OpenRequest {
            start_url: url("https://erp.example.com/inbox"),
            headless: true,
            bidi: BiDiRequirement::Preferred,
            viewport: None,
        }),
    )
    .expect("open succeeds");
    match response {
        BrowserToolResponse::Open(open) => (open.session_id, open.primary_context_id),
        other => panic!("expected open response, got {other:?}"),
    }
}

fn observe(session_id: BrowserSessionId, context_id: BrowserContextId) -> BrowserToolRequest {
    BrowserToolRequest::Observe(ObserveRequest {
        session_id,
        context_id,
        mode: ObservationMode::PageSummary,
    })
}

fn act(session_id: BrowserSessionId, context_id: BrowserContextId, action: BrowserAction) -> BrowserToolRequest {
    BrowserToolRequest::Act(ActRequest {
        session_id,
        request: ActionRequest::new(context_id, action)
            .with_expected_revision(BrowserObservationRevision::initial()),
    })
}

fn click() -> BrowserAction {
    BrowserAction::Click {
        target: Target::new(Selector::TestId("confirm".to_owned())),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn test_dispatch_open_binds_session_and_routes_all_operations() {
    let (host, tools) = tools();
    let (session_id, context_id) = open(&tools, OWNER_A);
    assert_eq!(tools.owned_sessions(OWNER_A), [session_id]);

    let revision = BrowserObservationRevision::initial().next();
    let target = Target::new(Selector::TestId("save".to_owned())).with_fallback(Selector::Role {
        role: "button".to_owned(),
        name: Some("Save".to_owned()),
    });

    match run(&tools, OWNER_A, observe(session_id, context_id)).expect("observe succeeds") {
        BrowserToolResponse::Observe(response) => {
            assert_eq!(response.observation.url, url("https://erp.example.com/inbox"));
        }
        other => panic!("expected observe response, got {other:?}"),
    }

    match run(
        &tools,
        OWNER_A,
        BrowserToolRequest::Find(FindRequest {
            session_id,
            context_id,
            target: target.clone(),
            revision,
        }),
    )
    .expect("find succeeds")
    {
        BrowserToolResponse::Find(response) => assert_eq!(response.element.tag, "button"),
        other => panic!("expected find response, got {other:?}"),
    }

    match run(&tools, OWNER_A, act(session_id, context_id, click())).expect("act succeeds") {
        BrowserToolResponse::Act(response) => assert!(response.outcome.confirmed),
        other => panic!("expected act response, got {other:?}"),
    }

    match run(
        &tools,
        OWNER_A,
        BrowserToolRequest::Wait(WaitRequest {
            session_id,
            context_id,
            condition: WaitCondition::NetworkQuiescence { idle_ms: 250 },
            timeout: WaitTimeout::from_millis(3_000),
        }),
    )
    .expect("wait succeeds")
    {
        BrowserToolResponse::Wait(response) => assert!(response.outcome.satisfied),
        other => panic!("expected wait response, got {other:?}"),
    }

    match run(
        &tools,
        OWNER_A,
        BrowserToolRequest::Events(EventsRequest {
            session_id,
            since: BrowserEventCursor::zero().next(),
        }),
    )
    .expect("events succeed")
    {
        BrowserToolResponse::Events(response) => assert!(response.events.is_empty()),
        other => panic!("expected events response, got {other:?}"),
    }

    match run(&tools, OWNER_A, BrowserToolRequest::Close(CloseRequest { session_id })).expect("close succeeds") {
        BrowserToolResponse::Close(response) => assert_eq!(response.session_id, session_id),
        other => panic!("expected close response, got {other:?}"),
    }

    let shared = &host.shared;
    assert_eq!(*lock(&shared.finds), [(context_id, target, revision)]);
    assert_eq!(lock(&shared.acts).len(), 1);
    assert_eq!(
        *lock(&shared.waits),
        [(context_id, WaitCondition::NetworkQuiescence { idle_ms: 250 }, 3_000)]
    );
    assert_eq!(*lock(&shared.events_since), [BrowserEventCursor::zero().next()]);
    assert_eq!(*lock(&shared.closed), [session_id]);
    assert!(tools.owned_sessions(OWNER_A).is_empty());
}

#[test]
fn test_dispatch_rejects_foreign_session_without_host_lookup() {
    let (host, tools) = tools();
    let (session_id, context_id) = open(&tools, OWNER_A);

    for request in [
        observe(session_id, context_id),
        act(session_id, context_id, click()),
        BrowserToolRequest::Events(EventsRequest {
            session_id,
            since: BrowserEventCursor::zero(),
        }),
    ] {
        let error = run(&tools, OWNER_B, request).expect_err("foreign owner must be rejected");
        assert!(
            matches!(error, Error::SessionNotFound { session_id: rejected } if rejected == session_id),
            "{error}"
        );
    }

    assert!(lock(&host.shared.session_lookups).is_empty());
    assert!(lock(&host.shared.acts).is_empty());
}

#[test]
fn test_dispatch_close_by_foreign_owner_is_rejected_and_session_stays_open() {
    let (host, tools) = tools();
    let (session_id, context_id) = open(&tools, OWNER_A);

    let error = run(&tools, OWNER_B, BrowserToolRequest::Close(CloseRequest { session_id }))
        .expect_err("foreign close must be rejected");

    assert!(matches!(error, Error::SessionNotFound { .. }));
    assert!(lock(&host.shared.closed).is_empty());
    assert!(run(&tools, OWNER_A, observe(session_id, context_id)).is_ok());
}

#[test]
fn test_dispatch_act_budget_exhausted() {
    let (host, tools) = tools_with(BrowserLimits::default().with_max_actions_per_session(2));
    let (session_id, context_id) = open(&tools, OWNER_A);

    assert!(run(&tools, OWNER_A, act(session_id, context_id, BrowserAction::Reload)).is_ok());
    assert!(run(&tools, OWNER_A, act(session_id, context_id, BrowserAction::Reload)).is_ok());
    let error = run(&tools, OWNER_A, act(session_id, context_id, BrowserAction::Reload))
        .expect_err("third action exceeds the budget");

    assert!(
        matches!(&error, Error::InvalidArgument { detail } if detail.contains("budget")),
        "{error}"
    );
    assert_eq!(lock(&host.shared.acts).len(), 2);
}

#[test]
fn test_dispatch_act_navigation_outside_grant_rejected_without_consuming_budget() {
    let (host, tools) = tools_with(BrowserLimits::default().with_max_actions_per_session(1));
    let (session_id, context_id) = open(&tools, OWNER_A);

    let error = run(
        &tools,
        OWNER_A,
        act(
            session_id,
            context_id,
            BrowserAction::Navigate {
                url: url("https://evil.example.net/"),
            },
        ),
    )
    .expect_err("navigation outside the grant is rejected");
    assert!(matches!(error, Error::OriginNotAllowed { .. }), "{error}");

    let error = run(
        &tools,
        OWNER_A,
        act(
            session_id,
            context_id,
            BrowserAction::Navigate {
                url: url("https://sso.example.org/login"),
            },
        ),
    )
    .expect_err("authentication origins are never navigation targets");
    assert!(matches!(error, Error::OriginNotAllowed { .. }), "{error}");

    assert!(run(&tools, OWNER_A, act(session_id, context_id, BrowserAction::Reload)).is_ok());
    assert_eq!(lock(&host.shared.acts).len(), 1);
}

#[test]
fn test_dispatch_act_observed_location_breach_closes_session() {
    let (host, tools) = tools();
    let (session_id, context_id) = open(&tools, OWNER_A);
    *lock(&host.shared.redirect_on_act) = Some(url("https://evil.example.net/phish"));

    let error = run(&tools, OWNER_A, act(session_id, context_id, click()))
        .expect_err("leaving the granted origins is a breach");

    assert!(matches!(error, Error::OriginNotAllowed { .. }), "{error}");
    assert_eq!(*lock(&host.shared.closed), [session_id]);
    assert!(tools.owned_sessions(OWNER_A).is_empty());
    let error = run(&tools, OWNER_A, observe(session_id, context_id))
        .expect_err("a breached session is gone");
    assert!(matches!(error, Error::SessionNotFound { .. }));
}

#[test]
fn test_dispatch_act_accepts_authentication_origin_as_observed_location() {
    let (host, tools) = tools();
    let (session_id, context_id) = open(&tools, OWNER_A);
    *lock(&host.shared.redirect_on_act) = Some(url("https://sso.example.org/login"));

    assert!(run(&tools, OWNER_A, act(session_id, context_id, click())).is_ok());
    assert!(lock(&host.shared.closed).is_empty());
}

#[test]
fn test_dispatch_open_rejects_start_location_outside_grant() {
    let (host, tools) = tools();
    *lock(&host.shared.open_redirect) = Some(url("https://evil.example.net/"));

    let error = run(
        &tools,
        OWNER_A,
        BrowserToolRequest::Open(OpenRequest {
            start_url: url("https://erp.example.com/"),
            headless: true,
            bidi: BiDiRequirement::NotRequired,
            viewport: None,
        }),
    )
    .expect_err("redirected start location is a breach");

    assert!(matches!(error, Error::OriginNotAllowed { .. }), "{error}");
    assert_eq!(lock(&host.shared.closed).len(), 1);
    assert!(tools.owned_sessions(OWNER_A).is_empty());
}

#[test]
fn test_dispatch_revalidates_with_session_limits() {
    let (host, tools) = tools_with(BrowserLimits::default().with_max_text_bytes(8));
    let (session_id, context_id) = open(&tools, OWNER_A);
    let request = act(
        session_id,
        context_id,
        BrowserAction::Type {
            target: Target::new(Selector::Css("textarea".to_owned())),
            text: "0123456789abcdef".to_owned(),
        },
    );

    let direct = tools.prepare(request.clone()).expect_err("tool set prepares with grant limits");
    assert!(matches!(direct, Error::InvalidArgument { .. }));

    let loose = prepare_browser_call(request).expect("default limits admit 16 bytes");
    let error = block_on(tools.dispatch(OWNER_A, loose)).expect_err("session limits apply at dispatch");
    assert!(matches!(error, Error::InvalidArgument { .. }), "{error}");
    assert!(lock(&host.shared.acts).is_empty());
}

#[test]
fn test_close_sessions_owned_by_closes_only_that_owners_sessions() {
    let (host, tools) = tools();
    let (a1, _) = open(&tools, OWNER_A);
    let (a2, _) = open(&tools, OWNER_A);
    let (b1, b1_context) = open(&tools, OWNER_B);

    let closed = block_on(tools.close_sessions_owned_by(OWNER_A)).expect("closes succeed");

    assert_eq!(closed, 2);
    let mut closed_ids = lock(&host.shared.closed).clone();
    closed_ids.sort_by_key(ToString::to_string);
    let mut expected = vec![a1, a2];
    expected.sort_by_key(ToString::to_string);
    assert_eq!(closed_ids, expected);
    assert!(tools.owned_sessions(OWNER_A).is_empty());
    assert_eq!(tools.owned_sessions(OWNER_B), [b1]);
    assert!(run(&tools, OWNER_B, observe(b1, b1_context)).is_ok());
}

#[test]
fn test_revoke_sessions_owned_by_blocks_further_use() {
    let (host, tools) = tools();
    let (session_id, context_id) = open(&tools, OWNER_A);

    assert_eq!(tools.revoke_sessions_owned_by(OWNER_A), [session_id]);

    let error = run(&tools, OWNER_A, observe(session_id, context_id)).expect_err("revoked");
    assert!(matches!(error, Error::SessionNotFound { .. }));
    assert!(lock(&host.shared.closed).is_empty());
}

// ── Fake host / runtime ─────────────────────────────────────────────────────

#[derive(Default)]
struct Shared {
    location: Mutex<Option<url::Url>>,
    open_redirect: Mutex<Option<url::Url>>,
    redirect_on_act: Mutex<Option<url::Url>>,
    session_lookups: Mutex<Vec<BrowserSessionId>>,
    finds: Mutex<Vec<(BrowserContextId, Target, BrowserObservationRevision)>>,
    acts: Mutex<Vec<ActionRequest>>,
    waits: Mutex<Vec<(BrowserContextId, WaitCondition, u64)>>,
    events_since: Mutex<Vec<BrowserEventCursor>>,
    closed: Mutex<Vec<BrowserSessionId>>,
}

#[derive(Default)]
struct FakeHost {
    shared: Arc<Shared>,
    next: AtomicU64,
    runtimes: Mutex<HashMap<BrowserSessionId, Arc<FakeRuntime>>>,
}

struct FakeRuntime {
    id: BrowserSessionId,
    context: BrowserContextId,
    shared: Arc<Shared>,
}

#[async_trait]
impl BrowserHost for FakeHost {
    async fn open(&self, request: OpenBrowserRequest) -> harw_browser::Result<BrowserSessionHandle> {
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let runtime = Arc::new(FakeRuntime {
            id: numbered(n),
            context: numbered(1_000 + n),
            shared: Arc::clone(&self.shared),
        });
        let landed = lock(&self.shared.open_redirect)
            .clone()
            .unwrap_or(request.start_url);
        *lock(&self.shared.location) = Some(landed);
        lock(&self.runtimes).insert(runtime.id, Arc::clone(&runtime));
        Ok(BrowserSessionHandle::new(runtime))
    }

    async fn session(&self, id: &BrowserSessionId) -> harw_browser::Result<BrowserSessionHandle> {
        lock(&self.shared.session_lookups).push(*id);
        let runtime = lock(&self.runtimes)
            .get(id)
            .cloned()
            .ok_or(Error::SessionNotFound { session_id: *id })?;
        Ok(BrowserSessionHandle::new(runtime))
    }

    async fn close(&self, id: &BrowserSessionId) -> harw_browser::Result<()> {
        lock(&self.shared.closed).push(*id);
        lock(&self.runtimes).remove(id);
        Ok(())
    }
}

#[async_trait]
impl BrowserRuntime for FakeRuntime {
    fn session_id(&self) -> BrowserSessionId {
        self.id
    }

    fn primary_context_id(&self) -> BrowserContextId {
        self.context
    }

    async fn observe(
        &self,
        context_id: &BrowserContextId,
        _mode: ObservationMode,
    ) -> harw_browser::Result<BrowserObservation> {
        let location = lock(&self.shared.location)
            .clone()
            .unwrap_or_else(|| url("about:blank"));
        Ok(BrowserObservation {
            session_id: self.id,
            context_id: *context_id,
            revision: BrowserObservationRevision::initial(),
            url: location,
            title: "Fake page".to_owned(),
            document_identity: DocumentIdentity::new("fake"),
            elements: Vec::new(),
            artifacts: Vec::new(),
            event_cursor: BrowserEventCursor::zero(),
        })
    }

    async fn find(
        &self,
        context_id: &BrowserContextId,
        target: &Target,
        revision: BrowserObservationRevision,
    ) -> harw_browser::Result<ObservedElement> {
        lock(&self.shared.finds).push((*context_id, target.clone(), revision));
        Ok(ObservedElement::new("fake-element", "button"))
    }

    async fn act(&self, request: ActionRequest) -> harw_browser::Result<ActionOutcome> {
        lock(&self.shared.acts).push(request);
        if let Some(target) = lock(&self.shared.redirect_on_act).clone() {
            *lock(&self.shared.location) = Some(target);
        }
        let mut outcome = ActionOutcome::new(EffectId::new());
        outcome.confirmed = true;
        Ok(outcome)
    }

    async fn wait(
        &self,
        context_id: &BrowserContextId,
        condition: WaitCondition,
        timeout: WaitTimeout,
    ) -> harw_browser::Result<WaitOutcome> {
        lock(&self.shared.waits).push((*context_id, condition.clone(), timeout.millis()));
        Ok(WaitOutcome::new(true, 25, condition))
    }

    async fn events(&self, since: BrowserEventCursor) -> harw_browser::Result<Vec<EventEnvelope>> {
        lock(&self.shared.events_since).push(since);
        Ok(Vec::new())
    }

    async fn capability_probe(&self) -> harw_browser::Result<BrowserCapabilityProbe> {
        Ok(BrowserCapabilityProbe::new(
            "fake".to_owned(),
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
        Ok(PageBridgeInstallationReceipt::new(
            PageBridgeInstallationId::new(),
            request.context_id(),
            request.bridge_id(),
            request.version(),
            "0".repeat(64),
        ))
    }

    async fn close(&self) -> harw_browser::Result<()> {
        Ok(())
    }
}

// Fake futures resolve without real I/O; polling to completion is enough.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
