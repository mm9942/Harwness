use std::future::Future;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use async_trait::async_trait;
use harw_browser::action::{ActionOutcome, ActionRequest, BrowserAction};
use harw_browser::capability::{BrowserCapabilityProbe, CapabilityStatus};
use harw_browser::event::{BrowserEvent, EventClass, EventEnvelope};
use harw_browser::host::{BrowserHost, BrowserRuntime};
use harw_browser::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId, EffectId,
    EventId,
};
use harw_browser::observation::{
    BrowserObservation, DocumentIdentity, ObservationMode, ObservedElement,
};
use harw_browser::page_bridge::{
    PageBridgeInstallRequest, PageBridgeInstallationId, PageBridgeInstallationReceipt,
};
use harw_browser::policy::{OpenBrowserRequest, OriginPolicy};
use harw_browser::selector::{Selector, Target};
use harw_browser::session::BrowserSessionHandle;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};
use harw_tool_browser::{
    ActRequest, BrowserOpenGrant, BrowserOpenPolicy, BrowserToolRequest, BrowserToolResponse,
    BrowserToolSet, EventsRequest, FindRequest, WaitRequest,
};

fn session_id() -> BrowserSessionId {
    BrowserSessionId::from_str("00000000-0000-4000-8000-000000000011")
        .expect("fixture session UUID is valid")
}

fn context_id() -> BrowserContextId {
    BrowserContextId::from_str("00000000-0000-4000-8000-000000000012")
        .expect("fixture context UUID is valid")
}

fn effect_id() -> EffectId {
    EffectId::from_str("00000000-0000-4000-8000-000000000013")
        .expect("fixture effect UUID is valid")
}

#[test]
fn dispatches_find_act_wait_and_events_with_preserved_arguments() {
    let journal = Arc::new(Mutex::new(Journal::default()));
    let host = Arc::new(MockHost {
        runtime: Arc::new(MockRuntime {
            journal: Arc::clone(&journal),
        }),
        journal: Arc::clone(&journal),
    });
    let tools = BrowserToolSet::with_open_policy(
        host as Arc<dyn BrowserHost>,
        BrowserOpenPolicy::grant(BrowserOpenGrant::ephemeral(
            OriginPolicy::new(vec!["example.com".to_owned()], true),
            OriginPolicy::new(Vec::new(), true),
        )),
    );
    let revision = BrowserObservationRevision::initial().next();
    let target =
        Target::new(Selector::TestId("save-ticket".to_owned())).with_fallback(Selector::Role {
            role: "button".to_owned(),
            name: Some("Save".to_owned()),
        });

    let found = dispatch(
        &tools,
        BrowserToolRequest::Find(FindRequest {
            session_id: session_id(),
            context_id: context_id(),
            target,
            revision,
        }),
    );
    match found {
        BrowserToolResponse::Find(response) => {
            assert_eq!(response.element.element_ref, "resolved-save-ticket");
            assert_eq!(response.element.tag, "button");
        }
        other => panic!("expected find response, got {other:?}"),
    }

    let action = ActionRequest::new(
        context_id(),
        BrowserAction::Click {
            target: Target::new(Selector::TestId("confirm".to_owned())),
        },
    )
    .with_expected_revision(revision);
    let acted = dispatch(
        &tools,
        BrowserToolRequest::Act(ActRequest {
            session_id: session_id(),
            request: action,
        }),
    );
    match acted {
        BrowserToolResponse::Act(response) => {
            assert_eq!(response.outcome.effect_id, effect_id());
            assert!(response.outcome.confirmed);
            assert_eq!(response.outcome.new_revision, Some(revision.next()));
        }
        other => panic!("expected act response, got {other:?}"),
    }

    let waited = dispatch(
        &tools,
        BrowserToolRequest::Wait(WaitRequest {
            session_id: session_id(),
            context_id: context_id(),
            condition: WaitCondition::NetworkQuiescence { idle_ms: 250 },
            timeout: WaitTimeout::from_millis(3_000),
        }),
    );
    match waited {
        BrowserToolResponse::Wait(response) => {
            assert!(response.outcome.satisfied);
            assert_eq!(response.outcome.elapsed_ms, 25);
            assert_eq!(
                response.outcome.condition,
                WaitCondition::NetworkQuiescence { idle_ms: 250 }
            );
        }
        other => panic!("expected wait response, got {other:?}"),
    }

    let events = dispatch(
        &tools,
        BrowserToolRequest::Events(EventsRequest {
            session_id: session_id(),
            since: BrowserEventCursor::zero().next(),
        }),
    );
    match events {
        BrowserToolResponse::Events(response) => {
            assert_eq!(response.events.len(), 1);
            assert_eq!(
                response.events[0].cursor,
                BrowserEventCursor::zero().next().next()
            );
            assert!(matches!(
                &response.events[0].event,
                BrowserEvent::DriverLog { level, message }
                    if level == "info" && message == "mock event"
            ));
        }
        other => panic!("expected events response, got {other:?}"),
    }

    let journal = journal.lock().expect("journal lock is healthy");
    assert_eq!(journal.session_lookups, vec![session_id(); 4]);
    assert_eq!(
        journal.find,
        Some((
            context_id(),
            Target::new(Selector::TestId("save-ticket".to_owned())).with_fallback(Selector::Role {
                role: "button".to_owned(),
                name: Some("Save".to_owned()),
            },),
            revision,
        ))
    );
    assert_eq!(
        journal.act,
        Some(
            ActionRequest::new(
                context_id(),
                BrowserAction::Click {
                    target: Target::new(Selector::TestId("confirm".to_owned())),
                },
            )
            .with_expected_revision(revision)
        )
    );
    assert_eq!(
        journal.wait,
        Some((
            context_id(),
            WaitCondition::NetworkQuiescence { idle_ms: 250 },
            3_000,
        ))
    );
    assert_eq!(
        journal.events_since,
        Some(BrowserEventCursor::zero().next())
    );
}

fn dispatch(tools: &BrowserToolSet, request: BrowserToolRequest) -> BrowserToolResponse {
    let prepared = tools.prepare(request).expect("request prepares");
    block_on(tools.dispatch(prepared)).expect("dispatch succeeds")
}

#[derive(Default)]
struct Journal {
    session_lookups: Vec<BrowserSessionId>,
    find: Option<(BrowserContextId, Target, BrowserObservationRevision)>,
    act: Option<ActionRequest>,
    wait: Option<(BrowserContextId, WaitCondition, u64)>,
    events_since: Option<BrowserEventCursor>,
}

struct MockRuntime {
    journal: Arc<Mutex<Journal>>,
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
        context_id: &BrowserContextId,
        _mode: ObservationMode,
    ) -> harw_browser::Result<BrowserObservation> {
        Ok(BrowserObservation {
            session_id: session_id(),
            context_id: *context_id,
            revision: BrowserObservationRevision::initial(),
            url: url::Url::parse("https://example.com").expect("fixture URL is valid"),
            title: String::new(),
            document_identity: DocumentIdentity::new("unused"),
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
        self.journal.lock().expect("journal lock is healthy").find =
            Some((*context_id, target.clone(), revision));
        Ok(ObservedElement::new("resolved-save-ticket", "button"))
    }

    async fn act(&self, request: ActionRequest) -> harw_browser::Result<ActionOutcome> {
        self.journal.lock().expect("journal lock is healthy").act = Some(request);
        let mut outcome = ActionOutcome::new(effect_id());
        outcome.confirmed = true;
        outcome.new_revision = Some(BrowserObservationRevision::initial().next().next());
        Ok(outcome)
    }

    async fn wait(
        &self,
        context_id: &BrowserContextId,
        condition: WaitCondition,
        timeout: WaitTimeout,
    ) -> harw_browser::Result<WaitOutcome> {
        self.journal.lock().expect("journal lock is healthy").wait = Some((
            *context_id,
            condition.clone(),
            timeout.duration().as_millis() as u64,
        ));
        Ok(WaitOutcome::new(true, 25, condition))
    }

    async fn events(&self, since: BrowserEventCursor) -> harw_browser::Result<Vec<EventEnvelope>> {
        self.journal
            .lock()
            .expect("journal lock is healthy")
            .events_since = Some(since);
        Ok(vec![EventEnvelope::new(
            EventId::from_str("00000000-0000-4000-8000-000000000014")
                .expect("fixture event UUID is valid"),
            since.next(),
            session_id(),
            EventClass::Critical,
            BrowserEvent::DriverLog {
                level: "info".to_owned(),
                message: "mock event".to_owned(),
            },
        )])
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
    journal: Arc<Mutex<Journal>>,
}

#[async_trait]
impl BrowserHost for MockHost {
    async fn open(
        &self,
        _request: OpenBrowserRequest,
    ) -> harw_browser::Result<BrowserSessionHandle> {
        Ok(BrowserSessionHandle::new(Arc::clone(&self.runtime)))
    }

    async fn session(
        &self,
        requested_session: &BrowserSessionId,
    ) -> harw_browser::Result<BrowserSessionHandle> {
        self.journal
            .lock()
            .expect("journal lock is healthy")
            .session_lookups
            .push(*requested_session);
        Ok(BrowserSessionHandle::new(Arc::clone(&self.runtime)))
    }

    async fn close(&self, _id: &BrowserSessionId) -> harw_browser::Result<()> {
        Ok(())
    }
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
