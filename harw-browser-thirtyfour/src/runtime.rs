//! Per-session state owned by the Firefox adapter (B-ADAPT, F-009).
//!
//! # Description
//! Holds the authorized `OpenBrowserRequest` (origin policies and limits), the
//! per-session `ActionBudget`, the bounded event journal and the raw driver.
//! After every effect the runtime validates all window locations through
//! [`crate::location_guard`] and aborts the session on a breach.

use crate::driver::FirefoxDriver;
use crate::journal::{EventJournal, EventJournalPolicy};
use crate::location_guard::LocationProbe;
use async_trait::async_trait;
use harw_browser::action::ActionBudget;
use harw_browser::capability::CapabilityStatus;
use harw_browser::event::{BrowserEvent, EventClass, EventEnvelope};
use harw_browser::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId, EventId,
};
use harw_browser::policy::OpenBrowserRequest;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use thirtyfour::WindowHandle;
use tokio::sync::{Mutex, MutexGuard, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventCapabilityState {
    pub(crate) navigation: CapabilityStatus,
    pub(crate) network: CapabilityStatus,
    pub(crate) log: CapabilityStatus,
    pub(crate) script: CapabilityStatus,
}

impl EventCapabilityState {
    fn unavailable() -> Self {
        Self {
            navigation: CapabilityStatus::Unavailable,
            network: CapabilityStatus::Unavailable,
            log: CapabilityStatus::Unavailable,
            script: CapabilityStatus::Unavailable,
        }
    }
}

/// All mutable authority and normalized state for one Firefox session.
pub(crate) struct FirefoxRuntime {
    session_id: BrowserSessionId,
    primary_context_id: BrowserContextId,
    request: OpenBrowserRequest,
    action_budget: Mutex<ActionBudget>,
    bidi_status: CapabilityStatus,
    driver: Mutex<FirefoxDriver>,
    windows: RwLock<HashMap<BrowserContextId, WindowHandle>>,
    revisions: RwLock<HashMap<BrowserContextId, BrowserObservationRevision>>,
    bidi_contexts: RwLock<HashMap<String, BrowserContextId>>,
    bidi_realms: RwLock<HashMap<String, BrowserContextId>>,
    journal: Mutex<EventJournal>,
    next_event_cursor: Mutex<BrowserEventCursor>,
    event_capabilities: RwLock<EventCapabilityState>,
    closed: AtomicBool,
}

impl FirefoxRuntime {
    pub(crate) fn new(
        driver: FirefoxDriver,
        session_id: BrowserSessionId,
        request: OpenBrowserRequest,
        bidi_status: CapabilityStatus,
        primary_context_id: BrowserContextId,
        primary_window: WindowHandle,
        event_policy: EventJournalPolicy,
    ) -> Self {
        let mut windows = HashMap::new();
        windows.insert(primary_context_id, primary_window);
        let mut revisions = HashMap::new();
        revisions.insert(primary_context_id, BrowserObservationRevision::initial());
        Self {
            session_id,
            primary_context_id,
            action_budget: Mutex::new(ActionBudget::new(&request.limits)),
            request,
            bidi_status,
            driver: Mutex::new(driver),
            windows: RwLock::new(windows),
            revisions: RwLock::new(revisions),
            bidi_contexts: RwLock::new(HashMap::new()),
            bidi_realms: RwLock::new(HashMap::new()),
            journal: Mutex::new(EventJournal::new(event_policy)),
            next_event_cursor: Mutex::new(BrowserEventCursor::zero()),
            event_capabilities: RwLock::new(EventCapabilityState::unavailable()),
            closed: AtomicBool::new(false),
        }
    }

    pub(crate) fn session_id(&self) -> BrowserSessionId {
        self.session_id
    }

    pub(crate) fn primary_context_id(&self) -> BrowserContextId {
        self.primary_context_id
    }

    /// Returns the authorized open request (origin policies and limits).
    pub(crate) fn request(&self) -> &OpenBrowserRequest {
        &self.request
    }

    /// Consumes one action from the per-session budget.
    pub(crate) async fn consume_action_budget(&self) -> harw_browser::Result<()> {
        self.action_budget.lock().await.try_consume()
    }

    /// Validates every open window location; aborts the session on a breach (F-009).
    pub(crate) async fn enforce_location_policy(&self) -> harw_browser::Result<()> {
        crate::location_guard::enforce_location_policy(self, &self.request).await
    }

    pub(crate) fn bidi_status(&self) -> CapabilityStatus {
        self.bidi_status
    }

    pub(crate) async fn driver(&self) -> MutexGuard<'_, FirefoxDriver> {
        self.driver.lock().await
    }

    pub(crate) fn windows(&self) -> &RwLock<HashMap<BrowserContextId, WindowHandle>> {
        &self.windows
    }

    pub(crate) fn revisions(
        &self,
    ) -> &RwLock<HashMap<BrowserContextId, BrowserObservationRevision>> {
        &self.revisions
    }

    /// Returns retained events newer than `since`.
    pub(crate) async fn events_since(&self, since: BrowserEventCursor) -> Vec<EventEnvelope> {
        self.journal.lock().await.since(since)
    }

    /// Returns the cursor of the newest retained event, or zero.
    pub(crate) async fn last_event_cursor(&self) -> BrowserEventCursor {
        self.journal
            .lock()
            .await
            .last_cursor()
            .unwrap_or_else(BrowserEventCursor::zero)
    }

    pub(crate) async fn set_event_capabilities(
        &self,
        navigation: CapabilityStatus,
        network: CapabilityStatus,
        log: CapabilityStatus,
        script: CapabilityStatus,
    ) {
        *self.event_capabilities.write().await = EventCapabilityState {
            navigation,
            network,
            log,
            script,
        };
    }

    pub(crate) async fn event_capabilities(&self) -> EventCapabilityState {
        *self.event_capabilities.read().await
    }

    pub(crate) async fn register_bidi_context(
        &self,
        bidi_context_id: String,
        context_id: BrowserContextId,
    ) -> harw_browser::Result<()> {
        register_identity(
            &self.bidi_contexts,
            "BiDi browsing context",
            bidi_context_id,
            context_id,
        )
        .await
    }

    pub(crate) async fn resolve_bidi_context(
        &self,
        bidi_context_id: &str,
    ) -> harw_browser::Result<BrowserContextId> {
        resolve_identity(
            &self.bidi_contexts,
            "BiDi browsing context",
            bidi_context_id,
        )
        .await
    }

    pub(crate) async fn register_bidi_realm(
        &self,
        realm_id: String,
        context_id: BrowserContextId,
    ) -> harw_browser::Result<()> {
        register_identity(&self.bidi_realms, "BiDi script realm", realm_id, context_id).await
    }

    pub(crate) async fn resolve_bidi_realm(
        &self,
        realm_id: &str,
    ) -> harw_browser::Result<BrowserContextId> {
        resolve_identity(&self.bidi_realms, "BiDi script realm", realm_id).await
    }

    /// Removes the browser-context association after BiDi reports that a realm was destroyed.
    ///
    /// Realm destruction may be delivered more than once while a browsing context is torn down,
    /// so an already-absent nonblank identity is intentionally a successful no-op.
    pub(crate) async fn remove_bidi_realm(&self, realm_id: &str) -> harw_browser::Result<()> {
        remove_identity(&self.bidi_realms, "BiDi script realm", realm_id).await
    }

    pub(crate) async fn append_event(
        &self,
        class: EventClass,
        event: BrowserEvent,
    ) -> harw_browser::Result<BrowserEventCursor> {
        self.ensure_open()?;
        let mut next_cursor = self.next_event_cursor.lock().await;
        *next_cursor = next_cursor.next();
        let cursor = *next_cursor;
        let envelope = EventEnvelope::new(EventId::new(), cursor, self.session_id, class, event);
        let disposition = self.journal.lock().await.push(envelope);
        tracing::trace!(
            cursor = cursor.value(),
            ?disposition,
            "journaled browser event"
        );
        Ok(cursor)
    }

    pub(crate) fn ensure_open(&self) -> harw_browser::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(harw_browser::Error::SessionNotFound {
                session_id: self.session_id,
            });
        }
        Ok(())
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    #[tracing::instrument(level = "info", skip(self), fields(session_id = %self.session_id))]
    pub(crate) async fn close_runtime(&self) -> harw_browser::Result<()> {
        if self
            .closed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tracing::debug!("Firefox runtime was already closed");
            return Ok(());
        }

        let mut driver = self.driver.lock().await;
        driver.quit().await.map_err(harw_browser::Error::from)?;
        tracing::info!("Firefox runtime closed");
        Ok(())
    }
}

#[async_trait]
impl LocationProbe for FirefoxRuntime {
    async fn open_locations(&self) -> harw_browser::Result<Vec<url::Url>> {
        let driver = self.driver.lock().await;
        let webdriver = driver.webdriver();
        let handles = webdriver
            .windows()
            .await
            .map_err(|error| location_error("list browser windows", &error))?;
        let mut locations = Vec::with_capacity(handles.len());
        for handle in handles {
            webdriver
                .switch_to_window(handle)
                .await
                .map_err(|error| location_error("switch to browser window", &error))?;
            locations.push(
                webdriver
                    .current_url()
                    .await
                    .map_err(|error| location_error("read browser window location", &error))?,
            );
        }
        Ok(locations)
    }

    async fn abort_session(&self, reason: &str) {
        tracing::warn!(session_id = %self.session_id, reason, "aborting Firefox session");
        if let Err(error) = self.close_runtime().await {
            tracing::warn!(%error, "Firefox session abort did not shut down cleanly");
        }
    }
}

fn location_error(
    operation: &str,
    error: &thirtyfour::error::WebDriverError,
) -> harw_browser::Error {
    harw_browser::Error::CapabilityUnavailable {
        detail: format!("could not {operation} for origin enforcement: {error}"),
    }
}

async fn register_identity(
    identities: &RwLock<HashMap<String, BrowserContextId>>,
    kind: &str,
    external_id: String,
    context_id: BrowserContextId,
) -> harw_browser::Result<()> {
    if external_id.trim().is_empty() {
        return Err(harw_browser::Error::InvalidArgument {
            detail: format!("{kind} identity must not be empty"),
        });
    }
    let mut identities = identities.write().await;
    if let Some(existing) = identities.get(&external_id) {
        if *existing != context_id {
            return Err(harw_browser::Error::InvalidArgument {
                detail: format!(
                    "{kind} '{external_id}' is already bound to another browser context"
                ),
            });
        }
        return Ok(());
    }
    identities.insert(external_id, context_id);
    Ok(())
}

async fn resolve_identity(
    identities: &RwLock<HashMap<String, BrowserContextId>>,
    kind: &str,
    external_id: &str,
) -> harw_browser::Result<BrowserContextId> {
    identities
        .read()
        .await
        .get(external_id)
        .copied()
        .ok_or_else(|| harw_browser::Error::InvalidArgument {
            detail: format!("{kind} '{external_id}' has no registered Harwness context"),
        })
}

async fn remove_identity(
    identities: &RwLock<HashMap<String, BrowserContextId>>,
    kind: &str,
    external_id: &str,
) -> harw_browser::Result<()> {
    if external_id.trim().is_empty() {
        return Err(harw_browser::Error::InvalidArgument {
            detail: format!("{kind} identity must not be empty"),
        });
    }

    identities.write().await.remove(external_id);
    Ok(())
}
