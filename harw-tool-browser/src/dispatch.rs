use crate::tool_set::BrowserToolSet;
use crate::types::{
    ActResponse, BrowserToolRequest, BrowserToolResponse, CloseResponse, EventsResponse,
    FindResponse, ObserveResponse, OpenResponse, PreparedBrowserCall, WaitResponse,
};

impl BrowserToolSet {
    /// Consumes one prepared call and routes it through the authoritative browser host.
    pub async fn dispatch(
        &self,
        prepared: PreparedBrowserCall,
    ) -> harw_browser::Result<BrowserToolResponse> {
        tracing::debug!(
            operation = ?prepared.scope.action,
            session_id = ?prepared.scope.session_id,
            "dispatching prepared browser tool call"
        );

        match prepared.into_request() {
            BrowserToolRequest::Open(open) => {
                let handle = self.host().open(open.request).await?;
                Ok(BrowserToolResponse::Open(OpenResponse {
                    session_id: handle.id(),
                    primary_context_id: handle.primary_context_id(),
                }))
            }
            BrowserToolRequest::Observe(observe) => {
                let handle = self.host().session(&observe.session_id).await?;
                let observation = handle.observe(&observe.context_id, observe.mode).await?;
                Ok(BrowserToolResponse::Observe(ObserveResponse {
                    observation,
                }))
            }
            BrowserToolRequest::Find(find) => {
                let handle = self.host().session(&find.session_id).await?;
                let element = handle
                    .find(&find.context_id, &find.target, find.revision)
                    .await?;
                Ok(BrowserToolResponse::Find(FindResponse { element }))
            }
            BrowserToolRequest::Act(act) => {
                let handle = self.host().session(&act.session_id).await?;
                let outcome = handle.act(act.request).await?;
                Ok(BrowserToolResponse::Act(ActResponse { outcome }))
            }
            BrowserToolRequest::Wait(wait) => {
                let handle = self.host().session(&wait.session_id).await?;
                let outcome = handle
                    .wait(&wait.context_id, wait.condition, wait.timeout)
                    .await?;
                Ok(BrowserToolResponse::Wait(WaitResponse { outcome }))
            }
            BrowserToolRequest::Events(events) => {
                let handle = self.host().session(&events.session_id).await?;
                let events = handle.events(events.since).await?;
                Ok(BrowserToolResponse::Events(EventsResponse { events }))
            }
            BrowserToolRequest::Close(close) => {
                self.host().close(&close.session_id).await?;
                Ok(BrowserToolResponse::Close(CloseResponse {
                    session_id: close.session_id,
                }))
            }
        }
    }
}
