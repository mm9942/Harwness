// Spec: CONTRACT_harw_browser.md, section `session.rs`.

use std::sync::Arc;

use crate::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BrowserSessionState {
    Starting,
    Ready,
    Degraded,
    Reconnecting,
    Closing,
    Closed,
    Failed,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub id: BrowserSessionId,
    pub state: BrowserSessionState,
    pub browser_name: String,
    pub browser_version: Option<String>,
    pub driver_version: Option<String>,
}

// Opaque handle callers receive from BrowserHost::open/session. It hides the concrete
// WebDriver/BiDi implementation behind the BrowserRuntime trait object (plan.md 14.3).
#[derive(Clone)]
pub struct BrowserSessionHandle {
    runtime: Arc<dyn crate::host::BrowserRuntime>,
}

impl BrowserSessionHandle {
    pub fn new(runtime: Arc<dyn crate::host::BrowserRuntime>) -> Self {
        Self { runtime }
    }

    pub fn id(&self) -> BrowserSessionId {
        self.runtime.session_id()
    }

    /// Returns the browsing context created as this session's primary context.
    ///
    /// The returned identifier is stable for the lifetime of the session and
    /// lets callers begin browser work without guessing which tab the backend
    /// opened first.
    pub fn primary_context_id(&self) -> BrowserContextId {
        self.runtime.primary_context_id()
    }

    pub async fn observe(
        &self,
        context_id: &BrowserContextId,
        mode: crate::observation::ObservationMode,
    ) -> crate::error::Result<crate::observation::BrowserObservation> {
        self.runtime.observe(context_id, mode).await
    }

    pub async fn find(
        &self,
        context_id: &BrowserContextId,
        target: &crate::selector::Target,
        revision: BrowserObservationRevision,
    ) -> crate::error::Result<crate::observation::ObservedElement> {
        self.runtime.find(context_id, target, revision).await
    }

    pub async fn act(
        &self,
        request: crate::action::ActionRequest,
    ) -> crate::error::Result<crate::action::ActionOutcome> {
        self.runtime.act(request).await
    }

    pub async fn wait(
        &self,
        context_id: &BrowserContextId,
        condition: crate::wait::WaitCondition,
        timeout: crate::wait::WaitTimeout,
    ) -> crate::error::Result<crate::wait::WaitOutcome> {
        self.runtime.wait(context_id, condition, timeout).await
    }

    pub async fn events(
        &self,
        since: BrowserEventCursor,
    ) -> crate::error::Result<Vec<crate::event::EventEnvelope>> {
        self.runtime.events(since).await
    }

    pub async fn capability_probe(
        &self,
    ) -> crate::error::Result<crate::capability::BrowserCapabilityProbe> {
        self.runtime.capability_probe().await
    }

    pub async fn install_page_bridge(
        &self,
        request: crate::page_bridge::PageBridgeInstallRequest,
    ) -> crate::error::Result<crate::page_bridge::PageBridgeInstallationReceipt> {
        self.runtime.install_page_bridge(request).await
    }

    pub async fn close(&self) -> crate::error::Result<()> {
        self.runtime.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    use crate::action::{ActionOutcome, ActionRequest};
    use crate::capability::{BrowserCapabilityProbe, CapabilityStatus};
    use crate::error::Result;
    use crate::event::EventEnvelope;
    use crate::host::BrowserRuntime;
    use crate::observation::{
        BrowserObservation, DocumentIdentity, ObservationMode, ObservedElement,
    };
    use crate::test_support::{TestResult, ctx};
    use crate::wait::{WaitCondition, WaitOutcome, WaitTimeout};

    // Polls `fut` to completion, assuming it resolves on the very first poll
    // (true for all mock futures below, since they contain no real await points).
    // `Waker::noop` and `pin!` replace the hand-rolled vtable this used to
    // carry, so the crate needs no `unsafe` and no async runtime dependency.
    fn block_on<F: Future>(fut: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        let mut fut = std::pin::pin!(fut);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => continue,
            }
        }
    }

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
                revision: BrowserObservationRevision::initial(),
                url: url::Url::parse("https://example.com")?,
                title: "mock".to_owned(),
                document_identity: DocumentIdentity::new("mock-doc"),
                elements: Vec::new(),
                artifacts: Vec::new(),
                event_cursor: BrowserEventCursor::zero(),
            })
        }

        async fn find(
            &self,
            _context_id: &BrowserContextId,
            _target: &crate::selector::Target,
            _revision: BrowserObservationRevision,
        ) -> Result<ObservedElement> {
            Ok(ObservedElement::new("mock-ref", "div"))
        }

        async fn act(&self, _request: ActionRequest) -> Result<ActionOutcome> {
            Ok(ActionOutcome::new(crate::ids::EffectId::new()))
        }

        async fn wait(
            &self,
            _context_id: &BrowserContextId,
            condition: WaitCondition,
            _timeout: WaitTimeout,
        ) -> Result<WaitOutcome> {
            Ok(WaitOutcome {
                satisfied: true,
                elapsed_ms: 0,
                condition,
            })
        }

        async fn events(&self, _since: BrowserEventCursor) -> Result<Vec<EventEnvelope>> {
            Ok(Vec::new())
        }

        async fn capability_probe(&self) -> Result<BrowserCapabilityProbe> {
            Ok(BrowserCapabilityProbe {
                browser_name: "mock-browser".to_owned(),
                browser_version: None,
                driver_version: None,
                bidi_connected: false,
                network_events: CapabilityStatus::Unavailable,
                console_events: CapabilityStatus::Unavailable,
                navigation_events: CapabilityStatus::Unavailable,
                script_messages: CapabilityStatus::Unavailable,
                driver_logs: CapabilityStatus::Unavailable,
            })
        }

        async fn install_page_bridge(
            &self,
            request: crate::page_bridge::PageBridgeInstallRequest,
        ) -> Result<crate::page_bridge::PageBridgeInstallationReceipt> {
            Ok(crate::page_bridge::PageBridgeInstallationReceipt::new(
                crate::page_bridge::PageBridgeInstallationId::new(),
                request.context_id(),
                request.bridge_id(),
                request.version(),
                "0000000000000000000000000000000000000000000000000000000000000",
            ))
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    fn mock_handle() -> (BrowserSessionId, BrowserContextId, BrowserSessionHandle) {
        let session_id = BrowserSessionId::new();
        let primary_context_id = BrowserContextId::new();
        let runtime: Arc<dyn BrowserRuntime> = Arc::new(MockRuntime {
            session_id,
            primary_context_id,
        });
        (
            session_id,
            primary_context_id,
            BrowserSessionHandle::new(runtime),
        )
    }

    #[test]
    fn test_browser_session_handle_id_matches_runtime() {
        let (session_id, _primary_context_id, handle) = mock_handle();
        assert_eq!(handle.id(), session_id);
    }

    #[test]
    fn test_browser_session_handle_primary_context_id_matches_runtime() {
        let (_session_id, primary_context_id, handle) = mock_handle();
        assert_eq!(handle.primary_context_id(), primary_context_id);
    }

    #[test]
    fn test_browser_session_handle_close_resolves_ok() {
        let (_session_id, _primary_context_id, handle) = mock_handle();
        let result = block_on(handle.close());
        assert!(result.is_ok());
    }

    #[test]
    fn test_browser_session_handle_events_resolves_empty() -> TestResult {
        let (_session_id, _primary_context_id, handle) = mock_handle();
        let result = block_on(handle.events(BrowserEventCursor::zero()));
        assert_eq!(
            result.map_err(ctx("mock events call succeeds"))?,
            Vec::new()
        );
        Ok(())
    }

    #[test]
    fn test_browser_session_handle_capability_probe_resolves() -> TestResult {
        let (_session_id, _primary_context_id, handle) = mock_handle();
        let result = block_on(handle.capability_probe());
        let probe = result.map_err(ctx("mock capability probe succeeds"))?;
        assert_eq!(probe.browser_name, "mock-browser");
        Ok(())
    }

    #[test]
    fn test_browser_session_handle_clone_shares_same_session_id() {
        let (session_id, _primary_context_id, handle) = mock_handle();
        let cloned = handle.clone();
        assert_eq!(cloned.id(), session_id);
    }

    #[test]
    fn test_browser_session_handle_observe_delegates_to_runtime() -> TestResult {
        let (session_id, _primary_context_id, handle) = mock_handle();
        let context_id = BrowserContextId::new();
        let observation = block_on(handle.observe(&context_id, ObservationMode::PageSummary))
            .map_err(ctx("mock observe always succeeds"))?;
        assert_eq!(observation.session_id, session_id);
        assert_eq!(observation.context_id, context_id);
        Ok(())
    }

    #[test]
    fn test_browser_session_handle_find_delegates_to_runtime() -> TestResult {
        let (_session_id, _primary_context_id, handle) = mock_handle();
        let context_id = BrowserContextId::new();
        let target = crate::selector::Target::new(crate::selector::Selector::Id("a".to_owned()));
        let element =
            block_on(handle.find(&context_id, &target, BrowserObservationRevision::initial()))
                .map_err(ctx("mock find always succeeds"))?;
        assert_eq!(element.element_ref, "mock-ref");
        Ok(())
    }

    #[test]
    fn test_browser_session_handle_act_delegates_to_runtime() -> TestResult {
        let (_session_id, _primary_context_id, handle) = mock_handle();
        let context_id = BrowserContextId::new();
        let request = ActionRequest::new(context_id, crate::action::BrowserAction::Reload);
        let outcome = block_on(handle.act(request)).map_err(ctx("mock act always succeeds"))?;
        assert!(!outcome.confirmed);
        Ok(())
    }

    #[test]
    fn test_browser_session_handle_wait_delegates_to_runtime() -> TestResult {
        let (_session_id, _primary_context_id, handle) = mock_handle();
        let context_id = BrowserContextId::new();
        let condition = WaitCondition::NavigationComplete;
        let outcome = block_on(handle.wait(
            &context_id,
            condition.clone(),
            WaitTimeout::from_millis(100),
        ))
        .map_err(ctx("mock wait always succeeds"))?;
        assert!(outcome.satisfied);
        assert_eq!(outcome.condition, condition);
        Ok(())
    }

    #[test]
    fn test_session_info_serde_json_round_trip() -> TestResult {
        let info = SessionInfo {
            id: BrowserSessionId::new(),
            state: BrowserSessionState::Ready,
            browser_name: "chrome".to_owned(),
            browser_version: Some("120.0".to_owned()),
            driver_version: None,
        };

        let json = serde_json::to_string(&info).map_err(ctx("session info serializes"))?;
        let decoded: SessionInfo =
            serde_json::from_str(&json).map_err(ctx("session info deserializes"))?;
        assert_eq!(decoded.id, info.id);
        assert_eq!(decoded.state, info.state);
        assert_eq!(decoded.browser_name, info.browser_name);
        assert_eq!(decoded.browser_version, info.browser_version);
        assert_eq!(decoded.driver_version, info.driver_version);
        Ok(())
    }

    #[test]
    fn test_browser_session_state_variants_serde_json_round_trip() -> TestResult {
        let states = [
            BrowserSessionState::Starting,
            BrowserSessionState::Ready,
            BrowserSessionState::Degraded,
            BrowserSessionState::Reconnecting,
            BrowserSessionState::Closing,
            BrowserSessionState::Closed,
            BrowserSessionState::Failed,
        ];
        for state in states {
            let json = serde_json::to_string(&state).map_err(ctx("state serializes"))?;
            let decoded: BrowserSessionState =
                serde_json::from_str(&json).map_err(ctx("state deserializes"))?;
            assert_eq!(decoded, state);
        }
        Ok(())
    }
}
