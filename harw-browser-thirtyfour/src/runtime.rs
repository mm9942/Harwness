//! Per-session state owned by the Firefox adapter.

use crate::driver::FirefoxDriver;
use crate::journal::{BackpressureDisposition, BidiEventClass, EventJournalPolicy};
use harw_browser::capability::CapabilityStatus;
use harw_browser::event::{BackpressureStats, BrowserEvent, EventClass, EventEnvelope};
use harw_browser::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId, EventId,
};
use harw_browser::policy::OriginPolicy;
use std::collections::{HashMap, VecDeque};
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
    origin_policy: OriginPolicy,
    bidi_status: CapabilityStatus,
    driver: Mutex<FirefoxDriver>,
    windows: RwLock<HashMap<BrowserContextId, WindowHandle>>,
    revisions: RwLock<HashMap<BrowserContextId, BrowserObservationRevision>>,
    bidi_contexts: RwLock<HashMap<String, BrowserContextId>>,
    bidi_realms: RwLock<HashMap<String, BrowserContextId>>,
    events: Mutex<VecDeque<EventEnvelope>>,
    next_event_cursor: Mutex<BrowserEventCursor>,
    backpressure_stats: Mutex<BackpressureStats>,
    event_policy: EventJournalPolicy,
    event_capabilities: RwLock<EventCapabilityState>,
    closed: AtomicBool,
}

impl FirefoxRuntime {
    pub(crate) fn new(
        driver: FirefoxDriver,
        session_id: BrowserSessionId,
        origin_policy: OriginPolicy,
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
            origin_policy,
            bidi_status,
            driver: Mutex::new(driver),
            windows: RwLock::new(windows),
            revisions: RwLock::new(revisions),
            bidi_contexts: RwLock::new(HashMap::new()),
            bidi_realms: RwLock::new(HashMap::new()),
            events: Mutex::new(VecDeque::with_capacity(event_policy.capacity())),
            next_event_cursor: Mutex::new(BrowserEventCursor::zero()),
            backpressure_stats: Mutex::new(BackpressureStats::default()),
            event_policy,
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

    pub(crate) fn origin_policy(&self) -> &OriginPolicy {
        &self.origin_policy
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

    pub(crate) fn events(&self) -> &Mutex<VecDeque<EventEnvelope>> {
        &self.events
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
        let mut journal = self.events.lock().await;
        if journal.len() < self.event_policy.capacity() {
            journal.push_back(envelope);
            return Ok(cursor);
        }

        if class == EventClass::Critical {
            if let Some(position) = journal
                .iter()
                .position(|entry| entry.class != EventClass::Critical)
            {
                journal.remove(position);
                self.backpressure_stats.lock().await.record_drop();
            }
            journal.push_back(envelope);
            tracing::warn!(
                cursor = cursor.value(),
                "retained critical browser event under journal pressure"
            );
            return Ok(cursor);
        }

        let disposition = self.event_policy.disposition(match class {
            EventClass::Critical => BidiEventClass::CriticalControl,
            EventClass::RequestLifecycle => BidiEventClass::RequestLifecycle,
            EventClass::ConsoleRepetition => BidiEventClass::Console,
            EventClass::StaticAsset => BidiEventClass::StaticAsset,
            EventClass::Artifact => BidiEventClass::ArtifactPayload,
        });
        let mut stats = self.backpressure_stats.lock().await;
        match disposition {
            BackpressureDisposition::Retain => journal.push_back(envelope),
            BackpressureDisposition::AggregateWhenFull => stats.record_aggregate(),
            BackpressureDisposition::Deduplicate => stats.record_deduplicate(),
            BackpressureDisposition::Sample | BackpressureDisposition::StoreExternally => {
                stats.record_drop()
            }
        }
        tracing::debug!(
            cursor = cursor.value(),
            ?disposition,
            "applied browser event backpressure"
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

        let driver = self.driver.lock().await;
        driver.quit().await.map_err(harw_browser::Error::from)?;
        tracing::info!("Firefox runtime closed");
        Ok(())
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
