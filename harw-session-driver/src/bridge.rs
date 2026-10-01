//! The `TurnDriver` bridge over `harw-core` (S03).
//!
//! Skeleton: [`CoreTurnDriver::new`] answers
//! [`DriverBridgeError::NotImplemented`]; the trait impl below is the frozen
//! shape S03 fills in (bodies answer `HostError::NotImplemented`).

use std::path::PathBuf;
use std::sync::Arc;

use harw_session_host::HostError;
use harw_session_host::driver::{
    CancelSignal, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_types::SessionId;

use crate::DriverBridgeError;

const WHAT: &str = "CoreTurnDriver (S03)";

/// What the bridge needs to assemble a core runtime.
#[derive(Clone, Debug)]
pub struct CoreDriverConfig {
    /// Session store root (transcripts, meta, approvals).
    pub sessions_root: PathBuf,
}

impl CoreDriverConfig {
    /// Config for `sessions_root`.
    #[must_use]
    pub fn new(sessions_root: PathBuf) -> Self {
        Self { sessions_root }
    }
}

/// Production [`TurnDriver`] wrapping the `harw-core` durable turn loop.
#[derive(Debug)]
pub struct CoreTurnDriver {
    _config: CoreDriverConfig,
}

impl CoreTurnDriver {
    /// Build the driver from `config`.
    ///
    /// # Errors
    /// [`DriverBridgeError::NotImplemented`] in the skeleton.
    pub fn new(config: CoreDriverConfig) -> Result<Self, DriverBridgeError> {
        let _ = config;
        Err(DriverBridgeError::NotImplemented(WHAT))
    }
}

fn stub<T: Send + 'static>() -> DriverFuture<'static, T> {
    Box::pin(async { Err(HostError::NotImplemented(WHAT)) })
}

impl TurnDriver for CoreTurnDriver {
    fn create_session(&self, session_id: &SessionId, title: Option<&str>) -> DriverFuture<'_, ()> {
        let _ = (session_id, title);
        stub()
    }

    fn run_turn(
        &self,
        input: TurnInput,
        cancel: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        let _ = (input, cancel, sink);
        stub()
    }

    fn resume_after_approval(
        &self,
        session_id: &SessionId,
        cancel: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        let _ = (session_id, cancel, sink);
        stub()
    }

    fn apply_setting(&self, session_id: &SessionId, setting: Setting) -> DriverFuture<'_, ()> {
        let _ = (session_id, setting);
        stub()
    }

    fn model_name(&self, session_id: &SessionId) -> Option<String> {
        let _ = session_id;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_constructor_is_typed_not_implemented() {
        let result = CoreTurnDriver::new(CoreDriverConfig::new(PathBuf::from(".")));
        assert!(matches!(result, Err(DriverBridgeError::NotImplemented(_))));
    }
}
