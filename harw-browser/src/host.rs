//! # host
//!
//! ## Responsibility
//! This module owns the **two async trait contracts** a concrete browser
//! backend must implement: [`BrowserRuntime`] (the per-session operation
//! surface — observe, find, act, wait, events, capability probing, close) and
//! [`BrowserHost`] (opening, looking up, and closing sessions). It does not
//! own any implementation of these traits — that is the responsibility of
//! downstream backend crates such as `harw-browser-thirtyfour` — nor does it
//! own the caller-facing handle type (see
//! [`crate::session::BrowserSessionHandle`], which wraps a `dyn
//! BrowserRuntime`).
//!
//! ## Key types exported
//! - [`BrowserRuntime`] — the object-safe, `Send + Sync` trait a backend
//!   implements to expose one open session's operations.
//! - [`BrowserHost`] — the object-safe, `Send + Sync` trait a backend
//!   implements to manage the lifecycle of sessions.
//!
//! ## Concurrency
//! Both traits require `Send + Sync` and every method is `async`, so
//! conforming implementations must be safe to call concurrently from
//! multiple threads/tasks. This module itself performs no I/O and spawns no
//! threads; it only declares the contract.
//!
//! ## Errors
//! Every method returns [`crate::error::Result`]; see [`crate::error::Error`]
//! for the full set of variants a conforming implementation is expected to
//! produce.
//!
//! ## Examples
//! ```rust,no_run
//! use harw_browser::error::Result;
//! use harw_browser::host::BrowserHost;
//! use harw_browser::ids::BrowserSessionId;
//!
//! async fn look_up(host: &dyn BrowserHost, id: BrowserSessionId) -> Result<()> {
//!     let _handle = host.session(&id).await?;
//!     Ok(())
//! }
//! ```

// Spec: CONTRACT_harw_browser.md, section `host.rs`.

use crate::action::{ActionOutcome, ActionRequest};
use crate::capability::BrowserCapabilityProbe;
use crate::error::Result;
use crate::event::EventEnvelope;
use crate::ids::{
    BrowserContextId, BrowserEventCursor, BrowserObservationRevision, BrowserSessionId,
};
use crate::observation::{BrowserObservation, ObservationMode, ObservedElement};
use crate::page_bridge::{PageBridgeInstallRequest, PageBridgeInstallationReceipt};
use crate::policy::OpenBrowserRequest;
use crate::selector::Target;
use crate::session::BrowserSessionHandle;
use crate::wait::{WaitCondition, WaitOutcome, WaitTimeout};
use async_trait::async_trait;

/// The per-session operation surface a concrete browser backend implements.
///
/// # Description
/// One `BrowserRuntime` implementation corresponds to one open browser
/// session (possibly spanning multiple browsing contexts/tabs identified by
/// [`BrowserContextId`]). [`crate::session::BrowserSessionHandle`] holds an
/// `Arc<dyn BrowserRuntime>` and forwards every method call to it, so callers
/// never need to know the concrete backend type.
///
/// # Concurrency
/// Requires `Send + Sync`: implementations must be safe to call from
/// multiple threads/tasks concurrently, since [`BrowserSessionHandle`]
/// clones share the same underlying runtime.
///
/// [`BrowserSessionHandle`]: crate::session::BrowserSessionHandle
// `async_trait` setzt auf jede erzeugte Methode ein `#[must_use]`, obwohl der
// erzeugte Rückgabetyp (`Pin<Box<dyn Future>>`) ohnehin schon als `must_use`
// gilt. Die Doppelung entsteht im Makro, nicht in diesem Code — deshalb hier
// eine benannte Ausnahme statt einer Änderung an den Methodensignaturen.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait BrowserRuntime: Send + Sync {
    /// Returns the identifier of the session this runtime backs.
    ///
    /// # Returns
    /// `BrowserSessionId` — the session's identifier.
    fn session_id(&self) -> BrowserSessionId;

    /// Returns the primary browsing context allocated when
    /// [`BrowserHost::open`] created this session.
    ///
    /// The returned identifier is a concrete context owned by this runtime,
    /// not a driver-native handle or a context inferred from a later
    /// observation. Callers can use it as the initial context for operations
    /// that require a [`BrowserContextId`].
    ///
    /// # Returns
    /// `BrowserContextId` — the context allocated for this session by
    /// [`BrowserHost::open`].
    fn primary_context_id(&self) -> BrowserContextId;

    /// Observes a browsing context according to the given [`ObservationMode`].
    ///
    /// # Arguments
    /// - `context_id` (`&BrowserContextId`): the browsing context to observe.
    ///   Borrowed.
    /// - `mode` (`ObservationMode`): what kind of observation to take (page
    ///   summary, interactive elements, DOM selection, etc.). Ownership
    ///   transferred.
    ///
    /// # Returns
    /// `Result<BrowserObservation>` — the observation, including the
    /// context's current revision and event cursor.
    ///
    /// # Errors
    /// - [`crate::error::Error::SessionNotFound`] — the session has been
    ///   closed or never existed.
    async fn observe(
        &self,
        context_id: &BrowserContextId,
        mode: ObservationMode,
    ) -> Result<BrowserObservation>;

    /// Resolves a [`Target`] to a concrete [`ObservedElement`] within a
    /// context, honoring [`crate::selector::Target::candidates`] fallback
    /// order.
    ///
    /// # Arguments
    /// - `context_id` (`&BrowserContextId`): the browsing context to search
    ///   within. Borrowed.
    /// - `target` (`&Target`): the primary selector plus fallbacks to try in
    ///   order. Borrowed.
    /// - `revision` (`BrowserObservationRevision`): the revision the caller
    ///   expects the context to still be at. Owned (`Copy`).
    ///
    /// # Returns
    /// `Result<ObservedElement>` — the first matching element found.
    ///
    /// # Errors
    /// - [`crate::error::Error::SelectorNotFound`] — no candidate selector
    ///   matched any element.
    /// - [`crate::error::Error::StaleRevision`] — the context has moved past
    ///   `revision`.
    async fn find(
        &self,
        context_id: &BrowserContextId,
        target: &Target,
        revision: BrowserObservationRevision,
    ) -> Result<ObservedElement>;

    /// Executes an [`ActionRequest`] against this session.
    ///
    /// # Arguments
    /// - `request` (`ActionRequest`): the action to perform, its target
    ///   context, and optional expected revision. Ownership transferred.
    ///
    /// # Returns
    /// `Result<ActionOutcome>` — the effect id, resulting revision (if
    /// known), confirmation state, and any diagnostics.
    ///
    /// # Errors
    /// - [`crate::error::Error::StaleRevision`] — `request.expected_revision`
    ///   no longer matches the context's current revision.
    /// - [`crate::error::Error::SelectorNotFound`] — the action's target
    ///   could not be resolved.
    async fn act(&self, request: ActionRequest) -> Result<ActionOutcome>;

    /// Blocks (asynchronously) until `condition` is satisfied or `timeout`
    /// elapses.
    ///
    /// # Arguments
    /// - `context_id` (`&BrowserContextId`): the browsing context the
    ///   condition applies to. Borrowed.
    /// - `condition` (`WaitCondition`): the condition to wait for. Ownership
    ///   transferred.
    /// - `timeout` (`WaitTimeout`): the maximum duration to wait. Owned
    ///   (`Copy`).
    ///
    /// # Returns
    /// `Result<WaitOutcome>` — whether the condition was satisfied and how
    /// long it took, even when the timeout elapses first (`satisfied: false`
    /// is not itself an error).
    ///
    /// # Errors
    /// - [`crate::error::Error::SessionNotFound`] — the session was closed
    ///   while waiting.
    async fn wait(
        &self,
        context_id: &BrowserContextId,
        condition: WaitCondition,
        timeout: WaitTimeout,
    ) -> Result<WaitOutcome>;

    /// Fetches events emitted since `since`.
    ///
    /// # Arguments
    /// - `since` (`BrowserEventCursor`): the cursor of the last event the
    ///   caller has already seen; pass [`BrowserEventCursor::zero`] to fetch
    ///   from the start. Owned (`Copy`).
    ///
    /// # Returns
    /// `Result<Vec<EventEnvelope>>` — events strictly after `since`, in
    /// cursor order. May be empty if nothing new has occurred.
    ///
    /// [`BrowserEventCursor::zero`]: crate::ids::BrowserEventCursor::zero
    async fn events(&self, since: BrowserEventCursor) -> Result<Vec<EventEnvelope>>;

    /// Probes what this session's backend actually supports at runtime.
    ///
    /// # Returns
    /// `Result<BrowserCapabilityProbe>` — the browser/driver identity and
    /// per-feature support levels.
    async fn capability_probe(&self) -> Result<BrowserCapabilityProbe>;

    /// Installs a bounded, versioned page bridge into one browsing context.
    ///
    /// The runtime must validate the request policy and context ownership
    /// before executing page-provided script, and returns a typed receipt
    /// identifying the exact installed bridge version and script digest.
    async fn install_page_bridge(
        &self,
        request: PageBridgeInstallRequest,
    ) -> Result<PageBridgeInstallationReceipt>;

    /// Closes this session and releases any underlying resources (browser
    /// process, driver connection, temporary profile).
    ///
    /// # Returns
    /// `Result<()>` — `Ok(())` once the session is fully closed.
    async fn close(&self) -> Result<()>;
}

/// Opens, looks up, and closes browser sessions.
///
/// # Description
/// A `BrowserHost` is the entry point a caller uses to obtain
/// [`crate::session::BrowserSessionHandle`]s; the handles themselves forward
/// per-session operations to a [`BrowserRuntime`] rather than to the host.
///
/// # Concurrency
/// Requires `Send + Sync`: implementations must be safe to call from
/// multiple threads/tasks concurrently, since multiple callers may open,
/// look up, or close sessions at the same time.
// Dieselbe Makro-Doppelung wie bei `BrowserRuntime` weiter oben.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait BrowserHost: Send + Sync {
    /// Opens a new browser session according to `request`.
    ///
    /// # Arguments
    /// - `request` (`OpenBrowserRequest`): the start URL, headless flag,
    ///   profile policy, BiDi requirement, allowed-origins policy, and
    ///   optional viewport for the new session. Ownership transferred.
    ///
    /// # Returns
    /// `Result<BrowserSessionHandle>` — a handle to the newly opened
    /// session.
    ///
    /// # Errors
    /// - [`crate::error::Error::OriginNotAllowed`] — `request.start_url`'s
    ///   origin is rejected by `request.allowed_origins`.
    /// - [`crate::error::Error::CapabilityUnavailable`] — `request.bidi` is
    ///   [`crate::policy::BiDiRequirement::Required`] but no BiDi connection
    ///   could be established.
    async fn open(&self, request: OpenBrowserRequest) -> Result<BrowserSessionHandle>;

    /// Looks up a handle to an already-open session.
    ///
    /// # Arguments
    /// - `id` (`&BrowserSessionId`): the session to look up. Borrowed.
    ///
    /// # Returns
    /// `Result<BrowserSessionHandle>` — a handle to the session identified by
    /// `id`.
    ///
    /// # Errors
    /// - [`crate::error::Error::SessionNotFound`] — no open session matches
    ///   `id`.
    async fn session(&self, id: &BrowserSessionId) -> Result<BrowserSessionHandle>;

    /// Closes the session identified by `id`.
    ///
    /// # Arguments
    /// - `id` (`&BrowserSessionId`): the session to close. Borrowed.
    ///
    /// # Returns
    /// `Result<()>` — `Ok(())` once the session is fully closed.
    ///
    /// # Errors
    /// - [`crate::error::Error::SessionNotFound`] — no open session matches
    ///   `id`.
    async fn close(&self, id: &BrowserSessionId) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::CapabilityStatus;
    use crate::ids::EffectId;
    use crate::observation::DocumentIdentity;
    use crate::page_bridge::PageBridgeInstallationId;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};

    // Minimal mock proving `BrowserRuntime` is object-safe: every method has a
    // trivial, immediately-ready body so the block-on helper below resolves on
    // the first poll.
    struct MockRuntime {
        id: BrowserSessionId,
        primary_context_id: BrowserContextId,
    }

    #[async_trait]
    impl BrowserRuntime for MockRuntime {
        fn session_id(&self) -> BrowserSessionId {
            self.id
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
                session_id: self.id,
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
        ) -> Result<ObservedElement> {
            Ok(ObservedElement::new("mock-ref", "div"))
        }

        async fn act(&self, _request: ActionRequest) -> Result<ActionOutcome> {
            Ok(ActionOutcome::new(EffectId::new()))
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
                browser_name: "mock".to_owned(),
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
            request: PageBridgeInstallRequest,
        ) -> Result<PageBridgeInstallationReceipt> {
            Ok(PageBridgeInstallationReceipt::new(
                PageBridgeInstallationId::new(),
                request.context_id(),
                request.bridge_id(),
                request.version(),
                "0000000000000000000000000000000000000000000000000000000000000000",
            ))
        }

        async fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    // Polls a future to completion assuming it is always `Poll::Ready` on the
    // first poll (true for every mock method above, which contain no `.await`
    // on a pending source). `Waker::noop` replaces the hand-rolled no-op
    // vtable this used to carry, so no `unsafe` is needed.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        let mut fut = std::pin::pin!(fut);
        loop {
            if let Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
                return value;
            }
        }
    }

    #[test]
    fn test_mock_runtime_is_object_safe_and_closes() {
        let expected_id = BrowserSessionId::new();
        let expected_context_id = BrowserContextId::new();
        let runtime: Arc<dyn BrowserRuntime> = Arc::new(MockRuntime {
            id: expected_id,
            primary_context_id: expected_context_id,
        });
        assert_eq!(runtime.session_id(), expected_id);
        assert_eq!(runtime.primary_context_id(), expected_context_id);
        let result = block_on(runtime.close());
        assert!(result.is_ok());
    }

    #[test]
    fn test_mock_runtime_capability_probe_resolves() {
        let runtime: Arc<dyn BrowserRuntime> = Arc::new(MockRuntime {
            id: BrowserSessionId::new(),
            primary_context_id: BrowserContextId::new(),
        });
        let probe = block_on(runtime.capability_probe()).expect("mock probe always succeeds");
        assert_eq!(probe.browser_name, "mock");
    }
}
