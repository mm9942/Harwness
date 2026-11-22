use std::any::Any;
use std::sync::Arc;

use harw_protocol::SessionPort;
use harw_protocol::session_wire::{ClientCaps, HelloAck};
use harw_session_remote::{RemoteConnection, RemotePort};

/// Which kind of host a [`Backend`] talks to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackendKind {
    Remote,
    #[cfg_attr(not(feature = "local-host"), allow(dead_code))]
    Local,
}

/// A connected session host: the port used for requests plus the handshake
/// result. Field order is drop order: the port drops before the guard, so the
/// guard (for example an on-device host handle) outlives every user of the port.
pub(crate) struct Backend {
    port: Arc<dyn SessionPort>,
    ack: HelloAck,
    kind: BackendKind,
    _guard: Option<Box<dyn Any + Send + Sync>>,
}

impl Backend {
    /// Wraps an established remote connection.
    pub(crate) fn remote(connection: RemoteConnection) -> Self {
        let ack = connection.hello_ack().clone();
        Self {
            port: Arc::new(RemotePort::new(connection)),
            ack,
            kind: BackendKind::Remote,
            _guard: None,
        }
    }

    /// Wraps an on-device host port. `guard` keeps the host alive until the
    /// backend drops.
    #[cfg_attr(not(feature = "local-host"), allow(dead_code))]
    pub(crate) fn local(
        port: Arc<dyn SessionPort>,
        ack: HelloAck,
        guard: Box<dyn Any + Send + Sync>,
    ) -> Self {
        Self {
            port,
            ack,
            kind: BackendKind::Local,
            _guard: Some(guard),
        }
    }

    pub(crate) fn port(&self) -> &dyn SessionPort {
        self.port.as_ref()
    }

    #[allow(dead_code)]
    pub(crate) fn ack(&self) -> &HelloAck {
        &self.ack
    }

    pub(crate) fn granted(&self) -> ClientCaps {
        self.ack.granted
    }

    pub(crate) fn host_epoch(&self) -> u64 {
        self.ack.host_epoch
    }

    pub(crate) fn features(&self) -> &[String] {
        &self.ack.features
    }

    pub(crate) fn kind(&self) -> BackendKind {
        self.kind
    }
}
