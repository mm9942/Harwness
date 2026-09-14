//! `BrowserRuntime` implementation: request validation, action budget and
//! post-effect origin enforcement around the family-specific helpers (F-009).

use crate::runtime::FirefoxRuntime;
use async_trait::async_trait;
use harw_browser::action::{
    ActionOutcome, ActionRequest, BrowserAction, validate_selector, validate_target,
};
use harw_browser::capability::{BrowserCapabilityProbe, CapabilityStatus};
use harw_browser::event::EventEnvelope;
use harw_browser::host::BrowserRuntime;
use harw_browser::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId,
};
use harw_browser::observation::{BrowserObservation, ObservationMode, ObservedElement};
use harw_browser::page_bridge::{PageBridgeInstallRequest, PageBridgeInstallationReceipt};
use harw_browser::selector::Target;
use harw_browser::wait::{WaitCondition, WaitOutcome, WaitTimeout};

#[async_trait]
impl BrowserRuntime for FirefoxRuntime {
    fn session_id(&self) -> BrowserSessionId {
        self.session_id()
    }

    fn primary_context_id(&self) -> BrowserContextId {
        self.primary_context_id()
    }

    async fn observe(
        &self,
        context_id: &BrowserContextId,
        mode: ObservationMode,
    ) -> harw_browser::Result<BrowserObservation> {
        self.ensure_open()?;
        if let ObservationMode::DomSelection { selector } = &mode {
            validate_selector(selector, &self.request().limits)?;
        }
        let observation = crate::observe::observe(self, context_id, mode).await;
        self.enforce_location_policy().await?;
        observation
    }

    async fn find(
        &self,
        context_id: &BrowserContextId,
        target: &Target,
        revision: BrowserObservationRevision,
    ) -> harw_browser::Result<ObservedElement> {
        self.ensure_open()?;
        validate_target(target, &self.request().limits)?;
        let found = crate::observe::find(self, context_id, target, revision).await;
        self.enforce_location_policy().await?;
        found
    }

    async fn act(&self, request: ActionRequest) -> harw_browser::Result<ActionOutcome> {
        self.ensure_open()?;
        request.validate(&self.request().limits)?;
        if let Some(target) = request.action.navigation_target() {
            self.request().check_navigation_target(target)?;
        }
        self.consume_action_budget().await?;
        let outcome = match &request.action {
            BrowserAction::Navigate { .. }
            | BrowserAction::Back
            | BrowserAction::Forward
            | BrowserAction::Reload => self.execute_navigation(request).await,
            BrowserAction::Click { .. }
            | BrowserAction::Type { .. }
            | BrowserAction::Clear { .. }
            | BrowserAction::Focus { .. }
            | BrowserAction::Select { .. } => self.execute_element_action(request).await,
            BrowserAction::Hover { .. }
            | BrowserAction::Scroll { .. }
            | BrowserAction::KeyPress { .. }
            | BrowserAction::Drag { .. } => {
                crate::gesture_actions::execute_gesture_action(self, request).await
            }
            BrowserAction::Submit { .. } => self.execute_submit_action(request).await,
        };
        // Runs even when the action failed: a partially applied effect may
        // still have navigated.
        self.enforce_location_policy().await?;
        outcome
    }

    async fn wait(
        &self,
        context_id: &BrowserContextId,
        condition: WaitCondition,
        timeout: WaitTimeout,
    ) -> harw_browser::Result<WaitOutcome> {
        self.ensure_open()?;
        let limits = &self.request().limits;
        condition.validate(limits)?;
        timeout.validate(limits)?;
        let outcome = self.wait_condition(context_id, condition, timeout).await;
        self.enforce_location_policy().await?;
        outcome
    }

    async fn events(&self, since: BrowserEventCursor) -> harw_browser::Result<Vec<EventEnvelope>> {
        self.ensure_open()?;
        Ok(self.events_since(since).await)
    }

    async fn capability_probe(&self) -> harw_browser::Result<BrowserCapabilityProbe> {
        self.ensure_open()?;
        let events = self.event_capabilities().await;
        Ok(BrowserCapabilityProbe::new(
            "firefox".to_owned(),
            None,
            None,
            self.bidi_status() != CapabilityStatus::Unavailable,
            events.network,
            events.log,
            events.navigation,
            events.script,
            CapabilityStatus::Unavailable,
        ))
    }

    async fn install_page_bridge(
        &self,
        request: PageBridgeInstallRequest,
    ) -> harw_browser::Result<PageBridgeInstallationReceipt> {
        self.ensure_open()?;
        self.install_preload_bridge(request).await
    }

    async fn close(&self) -> harw_browser::Result<()> {
        self.close_runtime().await
    }
}
