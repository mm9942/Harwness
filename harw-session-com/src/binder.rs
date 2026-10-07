//! Binding an identity to the method tables a connection may use.
//!
//! The binder is the only place that turns a trusted [`ClientIdentity`] into
//! [`Ports`]. For the real host it calls [`SessionHost::connect`] **before**
//! the upgrade, so a revoked, denied or draining caller gets an HTTP status
//! instead of a socket that opens and silently closes.

use std::sync::Arc;

use harw_session_host::{ClientIdentity, SessionHost};
use harw_session_ws::Ports;

use crate::refusal::ComRefusal;

/// Binds an authenticated identity to the ports a connection serves.
pub trait ComBinder: Send + Sync + 'static {
    /// Binds `identity`, or refuses before the upgrade.
    ///
    /// # Errors
    /// A [`ComRefusal`], answered as an HTTP status.
    fn bind(&self, identity: ClientIdentity) -> Result<Ports, ComRefusal>;
}

impl<F> ComBinder for F
where
    F: Fn(ClientIdentity) -> Result<Ports, ComRefusal> + Send + Sync + 'static,
{
    fn bind(&self, identity: ClientIdentity) -> Result<Ports, ComRefusal> {
        self(identity)
    }
}

/// Which method tables a connection is offered. Offering a table does not
/// grant it: `tool.*` and `gateway.*` still need the caps in the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortOffer {
    /// `session.*`, `turn.*`, `approval.*` only; `tool.*` and `gateway.*`
    /// answer "method not found".
    SessionOnly,
    /// Also `tool.*` and `gateway.*` (R18); each is checked against the
    /// identity's `tool_call`, `gateway_read` and `gateway_admin` caps.
    All,
}

/// [`ComBinder`] for a [`SessionHost`].
#[derive(Clone)]
pub struct HostBinder {
    host: SessionHost,
    offer: PortOffer,
}

impl HostBinder {
    /// Binds connections to `host`, offering `offer`.
    #[must_use]
    pub fn new(host: SessionHost, offer: PortOffer) -> Self {
        Self { host, offer }
    }
}

impl ComBinder for HostBinder {
    fn bind(&self, identity: ClientIdentity) -> Result<Ports, ComRefusal> {
        if self.host.is_draining() {
            return Err(ComRefusal::Draining);
        }
        let connection = self
            .host
            .connect(identity)
            .map_err(|error| ComRefusal::from_host(&error))?;
        let connection = Arc::new(connection);
        Ok(match self.offer {
            PortOffer::SessionOnly => Ports::session_only(connection),
            PortOffer::All => Ports::all(connection),
        })
    }
}
