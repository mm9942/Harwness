//! Test-only model provider that records every [`ModelRequest`] for later assertion.
//!
//! # Purpose
//! Provides a [`RecordingModelProvider`] for integration tests that need to inspect
//! what the turn-loop passed to the model: reasoning effort, tool specs, history length,
//! etc. The provider is fully thread-safe and `Clone`-able so it can be shared across
//! tasks while keeping a single shared recording buffer.
//!
//! # Design (spec § testing / 0.2.0 milestone)
//! - Records every [`ModelRequest`] in call order inside an `Arc<Mutex<Vec<ModelRequest>>>`.
//! - Returns a configurable canned [`ModelResponse`] (defaults to a short fixed string).
//! - Exposes the recorded requests via [`RecordingModelProvider::recorded`],
//!   [`RecordingModelProvider::last`], and [`RecordingModelProvider::clear`].
//!
//! # Concurrency
//! `RecordingModelProvider` is `Send + Sync` (required by [`ModelProvider`]).
//! All interior mutation goes through a `Mutex`; `Arc` clone shares the same buffer.
//!
//! # Usage
//! ```rust,no_run
//! use harw_core::testing::RecordingModelProvider;
//! let provider = RecordingModelProvider::new();
//! // … drive a turn-loop with &provider …
//! let calls = provider.recorded();
//! assert_eq!(calls.len(), 1);
//! ```

use std::sync::{Arc, Mutex};

use crate::model::{ModelFuture, ModelProvider, ModelRequest, ModelResponse};

/// A [`ModelProvider`] that records every [`ModelRequest`] it receives and returns
/// a configurable canned response.
///
/// # Description
/// Intended exclusively for tests. The inner `Arc<Mutex<Vec<ModelRequest>>>` ensures the
/// recording buffer is shared across all clones of the provider, so a single instance
/// handed to the turn-loop and kept in the test can both contribute to and read the
/// same record.
///
/// # Concurrency
/// Thread-safe. Locking is bounded to the duration of the push/read operations.
#[derive(Clone)]
pub struct RecordingModelProvider {
    /// Shared recording buffer — all clones point to the same allocation.
    inner: Arc<Mutex<Vec<ModelRequest>>>,
    /// Text returned for every `respond` call.
    canned_response: Arc<String>,
}

impl RecordingModelProvider {
    /// Creates a new provider with the default canned response `"(recording-provider: ok)"`.
    ///
    /// # Returns
    /// A fresh `RecordingModelProvider` with an empty recording buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::with_response("(recording-provider: ok)")
    }

    /// Creates a provider that returns `response` for every model call.
    ///
    /// # Arguments
    /// - `response` (`impl Into<String>`): the canned text to return as the assistant message.
    ///
    /// # Returns
    /// A fresh `RecordingModelProvider` with an empty recording buffer.
    #[must_use]
    pub fn with_response(response: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            canned_response: Arc::new(response.into()),
        }
    }

    /// Returns a clone of all recorded requests in call order.
    ///
    /// # Returns
    /// A `Vec<ModelRequest>` snapshot; the buffer is not cleared.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned (only possible if a thread panicked
    /// while holding the lock — should not occur in normal test usage).
    #[must_use]
    pub fn recorded(&self) -> Vec<ModelRequest> {
        self.inner.lock().unwrap().clone()
    }

    /// Returns the last recorded request, or `None` if no calls have been made yet.
    ///
    /// # Returns
    /// `Some(ModelRequest)` clone of the last entry, or `None`.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn last(&self) -> Option<ModelRequest> {
        self.inner.lock().unwrap().last().cloned()
    }

    /// Clears the recording buffer.
    ///
    /// Useful when a single provider instance is reused across multiple test phases.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}

impl Default for RecordingModelProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelProvider for RecordingModelProvider {
    /// Records `request` and immediately resolves with the canned response.
    ///
    /// # Description
    /// The request is pushed into the shared buffer while holding the mutex, then
    /// the mutex is released. The returned future is immediately ready (no network I/O).
    ///
    /// # Returns
    /// `Ok(ModelResponse)` containing the canned text as the assistant message.
    ///
    /// # Errors
    /// This implementation never returns `Err`; the signature is `Result<ModelResponse, ModelError>`
    /// only because the trait requires it.
    ///
    /// # Concurrency
    /// The mutex is held only for the duration of the push. The future itself does not
    /// hold any lock.
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        self.inner.lock().unwrap().push(request);
        let response = ModelResponse::text(self.canned_response.as_str());
        Box::pin(async move { Ok(response) })
    }
}

#[cfg(test)]
mod tests {
    use harw_types::ReasoningEffort;

    use super::*;
    use crate::history::ConversationHistory;
    use harw_extension_api::LoadedInstructions;

    fn make_request(effort: Option<ReasoningEffort>) -> ModelRequest {
        let instructions = LoadedInstructions {
            system_prompt: "system".to_owned(),
            fragments: vec![],
        };
        ModelRequest::new(instructions, vec![], ConversationHistory::default(), vec![])
            .with_reasoning_effort(effort)
    }

    #[tokio::test]
    async fn test_recording_captures_both_calls_in_order() {
        let provider = RecordingModelProvider::new();

        // First call — no reasoning effort specified.
        let req1 = make_request(None);
        let _ = provider.respond(req1).await.unwrap();

        // Second call — high reasoning effort.
        let req2 = make_request(Some(ReasoningEffort::High));
        let _ = provider.respond(req2).await.unwrap();

        let recorded = provider.recorded();
        assert_eq!(recorded.len(), 2, "expected exactly two recorded requests");

        // First request should have no reasoning effort.
        assert_eq!(
            recorded[0].reasoning_effort, None,
            "first request reasoning_effort should be None"
        );

        // Second request should preserve the high effort level.
        assert_eq!(
            recorded[1].reasoning_effort,
            Some(ReasoningEffort::High),
            "second request reasoning_effort should be High"
        );
    }

    #[tokio::test]
    async fn test_recording_returns_canned_response() {
        let provider = RecordingModelProvider::with_response("hello from test");
        let req = make_request(None);
        let response = provider.respond(req).await.unwrap();
        assert_eq!(
            response.message.as_deref(),
            Some("hello from test"),
            "canned response text should match"
        );
    }

    #[tokio::test]
    async fn test_clear_empties_buffer() {
        let provider = RecordingModelProvider::new();
        let _ = provider.respond(make_request(None)).await.unwrap();
        assert_eq!(provider.recorded().len(), 1);
        provider.clear();
        assert_eq!(
            provider.recorded().len(),
            0,
            "buffer should be empty after clear"
        );
    }

    #[tokio::test]
    async fn test_last_returns_most_recent() {
        let provider = RecordingModelProvider::new();
        assert!(
            provider.last().is_none(),
            "last() should be None when empty"
        );

        let _ = provider.respond(make_request(None)).await.unwrap();
        let _ = provider
            .respond(make_request(Some(ReasoningEffort::Low)))
            .await
            .unwrap();

        let last = provider.last().expect("last() should be Some after calls");
        assert_eq!(last.reasoning_effort, Some(ReasoningEffort::Low));
    }

    #[tokio::test]
    async fn test_clone_shares_buffer() {
        let provider = RecordingModelProvider::new();
        let clone = provider.clone();

        // Call via the clone.
        let _ = clone.respond(make_request(None)).await.unwrap();

        // Original should see the recording made through the clone.
        assert_eq!(
            provider.recorded().len(),
            1,
            "clone and original must share the same recording buffer"
        );
    }
}
