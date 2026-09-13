use async_trait::async_trait;
use harw_browser::action::{ActionOutcome, ActionRequest};
use harw_browser::capability::{BrowserCapabilityProbe, CapabilityStatus};
use harw_browser::event::EventEnvelope;
use harw_browser::host::BrowserRuntime;
use harw_browser::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId, EffectId,
};
use harw_browser::observation::{
    BrowserObservation, DocumentIdentity, ObservationMode, ObservedElement,
};
use harw_browser::page_bridge::{
    PageBridgeContentTrust, PageBridgeInstallRequest, PageBridgeInstallationId,
    PageBridgeInstallationReceipt, PageBridgePolicy,
};
use harw_browser::selector::Target;
use harw_browser::session::BrowserSessionHandle;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

struct RecordingRuntime {
    session_id: BrowserSessionId,
    primary_context_id: BrowserContextId,
    requests: Mutex<Vec<PageBridgeInstallRequest>>,
    receipt: Mutex<Option<PageBridgeInstallationReceipt>>,
}

#[async_trait]
impl BrowserRuntime for RecordingRuntime {
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
    ) -> harw_browser::Result<BrowserObservation> {
        Ok(BrowserObservation {
            session_id: self.session_id,
            context_id: *context_id,
            revision: BrowserObservationRevision::initial(),
            url: url::Url::parse("about:blank")?,
            title: String::new(),
            document_identity: DocumentIdentity::new("mock"),
            elements: Vec::new(),
            artifacts: Vec::new(),
            event_cursor: BrowserEventCursor::zero(),
        })
    }

    async fn find(
        &self,
        _context_id: &BrowserContextId,
        _target: &Target,
        _revision: BrowserObservationRevision,
    ) -> harw_browser::Result<ObservedElement> {
        Ok(ObservedElement::new("mock", "div"))
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
            false,
            CapabilityStatus::Unavailable,
            CapabilityStatus::Unavailable,
            CapabilityStatus::Unavailable,
            CapabilityStatus::Unavailable,
            CapabilityStatus::Unavailable,
        ))
    }

    async fn install_page_bridge(
        &self,
        request: PageBridgeInstallRequest,
    ) -> harw_browser::Result<PageBridgeInstallationReceipt> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request);
        self.receipt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or_else(|| harw_browser::Error::CapabilityUnavailable {
                detail: "mock receipt already consumed".to_owned(),
            })
    }

    async fn close(&self) -> harw_browser::Result<()> {
        Ok(())
    }
}

#[test]
fn typed_bridge_policy_rejects_unbounded_configuration() {
    assert!(PageBridgePolicy::new(0, 10, Duration::from_secs(60)).is_err());
    assert!(PageBridgePolicy::new(4_096, 0, Duration::from_secs(60)).is_err());
    assert!(PageBridgePolicy::new(4_096, 10, Duration::ZERO).is_err());
}

#[test]
fn session_handle_forwards_typed_page_bridge_installation() {
    let session_id = BrowserSessionId::new();
    let context_id = BrowserContextId::new();
    let installation_id = PageBridgeInstallationId::new();
    let receipt = PageBridgeInstallationReceipt::new(
        installation_id,
        context_id,
        "generic-chat",
        "1.0.0",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    );
    let runtime = Arc::new(RecordingRuntime {
        session_id,
        primary_context_id: context_id,
        requests: Mutex::new(Vec::new()),
        receipt: Mutex::new(Some(receipt)),
    });
    let handle = BrowserSessionHandle::new(runtime.clone());
    assert_eq!(handle.primary_context_id(), context_id);
    let policy = PageBridgePolicy::new(4_096, 20, Duration::from_secs(60))
        .unwrap_or_else(|error| panic!("bounded bridge policy must be valid: {error}"));
    let request = PageBridgeInstallRequest::new(
        context_id,
        "generic-chat",
        "1.0.0",
        "window.__harwBridge = Object.freeze({ version: '1.0.0' });",
        policy,
    )
    .unwrap_or_else(|error| panic!("versioned bridge request must be valid: {error}"));

    assert_eq!(
        request.emitted_content_trust(),
        PageBridgeContentTrust::Untrusted
    );
    let returned = block_on(handle.install_page_bridge(request))
        .unwrap_or_else(|error| panic!("handle must forward bridge installation: {error}"));

    assert_eq!(returned.installation_id(), installation_id);
    assert_eq!(returned.context_id(), context_id);
    assert_eq!(returned.bridge_id(), "generic-chat");
    assert_eq!(returned.version(), "1.0.0");
    assert_eq!(returned.script_sha256().len(), 64);

    let requests = runtime
        .requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].context_id(), context_id);
    assert_eq!(requests[0].bridge_id(), "generic-chat");
    assert_eq!(requests[0].version(), "1.0.0");
    assert_eq!(requests[0].policy(), &policy);
    assert!(requests[0].script().contains("Object.freeze"));
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
