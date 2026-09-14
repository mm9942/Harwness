//! Effectful dispatch of prepared browser calls.
//!
//! # Responsibility
//! Routes a sealed [`PreparedBrowserCall`] to the browser host after the
//! ownership/limit/budget admission in [`BrowserToolSet`], and enforces the
//! observed-location post-check: after open, observe, find, act, wait and
//! events the current URL must satisfy
//! `OpenBrowserRequest::check_observed_location`. A breach revokes and closes
//! the browser session and returns `Error::OriginNotAllowed` without the
//! response payload.
//!
//! The location is read through the existing `BrowserRuntime::observe`
//! contract (`ObservationMode::PageSummary`, field `url`); a cheaper dedicated
//! runtime method is proposed in ledger W5/B-TOOL.
//!
//! # Concurrency
//! Futures are `Send`; no registry lock is held across `await`.
//!
//! # Errors
//! Host/runtime errors are propagated unchanged; see [`BrowserToolSet`] for
//! admission errors.

use std::sync::Arc;

use harw_browser::error::{Error, Result};
use harw_browser::ids::{BrowserContextId, BrowserSessionId};
use harw_browser::observation::ObservationMode;
use harw_browser::policy::OpenBrowserRequest;
use harw_browser::session::BrowserSessionHandle;

use crate::tool_set::BrowserToolSet;
use crate::types::{
    ActResponse, BrowserToolResponse, CloseResponse, EventsResponse, FindResponse,
    ObserveResponse, OpenResponse, PreparedBrowserCall, PreparedBrowserRequest, WaitResponse,
};

impl BrowserToolSet {
    /// Consumes one prepared call and routes it through the authoritative host.
    ///
    /// # Arguments
    /// - `owner` (`&str`): the harness session issuing the call
    ///   (`ToolExecutionContext::session_id().as_str()`); an opened browser
    ///   session is bound to it, later calls must present the same owner.
    /// - `prepared` (`PreparedBrowserCall`): sealed call from [`Self::prepare`].
    ///
    /// # Returns
    /// The typed response; always untrusted page-derived data.
    ///
    /// # Errors
    /// - `Error::SessionNotFound`: unknown or foreign session.
    /// - `Error::InvalidArgument`: limits violated or action budget exhausted.
    /// - `Error::OriginNotAllowed`: navigation target outside the grant, or the
    ///   observed location left the grant (session closed).
    /// - Any host/runtime error.
    ///
    /// # Concurrency
    /// Safe to call concurrently; budget accounting is atomic per call.
    pub async fn dispatch(&self, owner: &str, prepared: PreparedBrowserCall) -> Result<BrowserToolResponse> {
        tracing::debug!(
            operation = ?prepared.scope().action,
            session_id = ?prepared.scope().session_id,
            "dispatching prepared browser tool call"
        );

        let request = prepared.into_request();
        // Admission (ownership, limits, navigation target, budget) happens on a
        // borrow before the request is consumed; Open and Close are admitted
        // by their own paths.
        let authority = match session_bound(&request) {
            Some(session_id) => Some(self.admit(owner, session_id, &request)?),
            None => None,
        };

        match request {
            PreparedBrowserRequest::Open(open) => self.dispatch_open(owner, open).await,
            PreparedBrowserRequest::Observe(observe) => {
                let authority = authority.ok_or_else(missing_admission)?;
                let session_id = observe.session_id;
                let handle = self.session_handle(session_id).await?;
                let observation = handle.observe(&observe.context_id, observe.mode).await?;
                self.enforce_url(session_id, &observation.url, &authority).await?;
                Ok(BrowserToolResponse::Observe(ObserveResponse { observation }))
            }
            PreparedBrowserRequest::Find(find) => {
                let authority = authority.ok_or_else(missing_admission)?;
                let session_id = find.session_id;
                let handle = self.session_handle(session_id).await?;
                let found = handle.find(&find.context_id, &find.target, find.revision).await;
                let element = self
                    .settle(session_id, &handle, &find.context_id, &authority, found)
                    .await?;
                Ok(BrowserToolResponse::Find(FindResponse { element }))
            }
            PreparedBrowserRequest::Act(act) => {
                let authority = authority.ok_or_else(missing_admission)?;
                let session_id = act.session_id;
                let context_id = act.request.context_id;
                let handle = self.session_handle(session_id).await?;
                let acted = handle.act(act.request).await;
                let outcome = self
                    .settle(session_id, &handle, &context_id, &authority, acted)
                    .await?;
                Ok(BrowserToolResponse::Act(ActResponse { outcome }))
            }
            PreparedBrowserRequest::Wait(wait) => {
                let authority = authority.ok_or_else(missing_admission)?;
                let session_id = wait.session_id;
                let handle = self.session_handle(session_id).await?;
                let waited = handle.wait(&wait.context_id, wait.condition, wait.timeout).await;
                let outcome = self
                    .settle(session_id, &handle, &wait.context_id, &authority, waited)
                    .await?;
                Ok(BrowserToolResponse::Wait(WaitResponse { outcome }))
            }
            PreparedBrowserRequest::Events(events) => {
                let authority = authority.ok_or_else(missing_admission)?;
                let session_id = events.session_id;
                let handle = self.session_handle(session_id).await?;
                let context_id = handle.primary_context_id();
                let fetched = handle.events(events.since).await;
                let events = self
                    .settle(session_id, &handle, &context_id, &authority, fetched)
                    .await?;
                Ok(BrowserToolResponse::Events(EventsResponse { events }))
            }
            PreparedBrowserRequest::Close(close) => {
                self.unregister_owned(owner, close.session_id)?;
                self.host().close(&close.session_id).await?;
                Ok(BrowserToolResponse::Close(CloseResponse {
                    session_id: close.session_id,
                }))
            }
        }
    }

    // Opens a session, checks its initial location, then binds it to `owner`.
    async fn dispatch_open(&self, owner: &str, open: OpenBrowserRequest) -> Result<BrowserToolResponse> {
        let handle = self.host().open(open.clone()).await?;
        let authority = Arc::new(open);
        let session_id = handle.id();
        let primary_context_id = handle.primary_context_id();
        if self.is_registered(session_id) {
            // A host reusing a live id must not let this open touch (or, via
            // the breach path, close) the session registered under that id.
            return Err(Error::InvalidArgument {
                detail: format!("browser host returned already registered session '{session_id}'"),
            });
        }

        let observation = match handle.observe(&primary_context_id, ObservationMode::PageSummary).await {
            Ok(observation) => observation,
            Err(error) => {
                // Unverifiable start location: never hand out the session.
                self.abort_session(session_id).await;
                return Err(error);
            }
        };
        self.enforce_url(session_id, &observation.url, &authority).await?;
        self.register_session(owner, session_id, authority)?;
        tracing::info!(%session_id, "browser session opened and bound to its owner");

        Ok(BrowserToolResponse::Open(OpenResponse {
            session_id,
            primary_context_id,
        }))
    }

    // Looks up a handle and rejects a host answering with a different session.
    async fn session_handle(&self, session_id: BrowserSessionId) -> Result<BrowserSessionHandle> {
        let handle = self.host().session(&session_id).await?;
        if handle.id() != session_id {
            return Err(Error::SessionNotFound { session_id });
        }
        Ok(handle)
    }

    // Runs the location post-check after an operation, whatever its result:
    // a breach wins over the operation's own outcome; if the operation failed
    // its error is returned; an unverifiable location withholds the payload.
    async fn settle<T>(
        &self,
        session_id: BrowserSessionId,
        handle: &BrowserSessionHandle,
        context_id: &BrowserContextId,
        authority: &OpenBrowserRequest,
        operation: Result<T>,
    ) -> Result<T> {
        let observed = handle.observe(context_id, ObservationMode::PageSummary).await;
        match (operation, observed) {
            (operation, Ok(observation)) => {
                self.enforce_url(session_id, &observation.url, authority).await?;
                operation
            }
            (Err(error), Err(_)) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    // Checks one observed URL; on breach revokes and closes the session.
    async fn enforce_url(
        &self,
        session_id: BrowserSessionId,
        url: &url::Url,
        authority: &OpenBrowserRequest,
    ) -> Result<()> {
        match authority.check_observed_location(url) {
            Ok(()) => Ok(()),
            Err(breach) => {
                tracing::warn!(%session_id, origin = %url.origin().ascii_serialization(), "browser left its granted origins; closing session");
                self.abort_session(session_id).await;
                Err(breach)
            }
        }
    }

    // Revokes and closes a session after a policy breach; close errors are logged.
    async fn abort_session(&self, session_id: BrowserSessionId) {
        self.unregister(session_id);
        if let Err(error) = self.host().close(&session_id).await {
            tracing::error!(%session_id, %error, "closing browser session after policy breach failed");
        }
    }
}

// Session that must be admitted through the registry before dispatch.
// `Open` creates a session and `Close` is admitted by `unregister_owned`.
fn session_bound(request: &PreparedBrowserRequest) -> Option<BrowserSessionId> {
    match request {
        PreparedBrowserRequest::Open(_) | PreparedBrowserRequest::Close(_) => None,
        PreparedBrowserRequest::Observe(observe) => Some(observe.session_id),
        PreparedBrowserRequest::Find(find) => Some(find.session_id),
        PreparedBrowserRequest::Act(act) => Some(act.session_id),
        PreparedBrowserRequest::Wait(wait) => Some(wait.session_id),
        PreparedBrowserRequest::Events(events) => Some(events.session_id),
    }
}

// `session_bound` and the dispatch match agree by construction; this error
// only replaces a panic path.
fn missing_admission() -> Error {
    Error::InvalidArgument {
        detail: "browser request reached dispatch without session admission".to_owned(),
    }
}
