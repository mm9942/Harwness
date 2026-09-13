//! `SessionManager` — verwaltet mehrere `AgentSession`s
//! (Orchestrator + Workers).

use crate::error::{CoreError, CoreResult};
use crate::session::{AgentSession, SpawnContext};
use harw_extension_api::ExtensionRegistry;
use harw_protocol::events::SessionEvent;
use harw_types::{AgentRole, SessionId};
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Verwaltet mehrere `AgentSession`s (Orchestrator + Workers).
pub struct SessionManager {
    sessions: HashMap<String, AgentSession>,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
}

impl SessionManager {
    pub fn new(event_tx: mpsc::UnboundedSender<SessionEvent>) -> Self {
        Self {
            sessions: HashMap::new(),
            event_tx,
        }
    }

    pub fn create_session(
        &mut self,
        role: AgentRole,
        parent: Option<SessionId>,
        registry: ExtensionRegistry,
    ) -> SessionId {
        let session = AgentSession::new(role, parent, registry, self.event_tx.clone());
        let id = session.id().clone();
        self.sessions.insert(id.as_str().to_owned(), session);
        id
    }

    /// Create a child/root session only after its sandbox and advisory catalog
    /// snapshot have been resolved by trusted orchestration code.
    pub fn create_governed_session(
        &mut self,
        role: AgentRole,
        parent: Option<SessionId>,
        registry: ExtensionRegistry,
        spawn_context: SpawnContext,
    ) -> SessionId {
        let session = AgentSession::new(role, parent, registry, self.event_tx.clone())
            .with_spawn_context(spawn_context);
        let id = session.id().clone();
        self.sessions.insert(id.as_str().to_owned(), session);
        id
    }

    pub fn get(&self, id: &SessionId) -> CoreResult<&AgentSession> {
        self.sessions
            .get(id.as_str())
            .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))
    }

    pub fn get_mut(&mut self, id: &SessionId) -> CoreResult<&mut AgentSession> {
        self.sessions
            .get_mut(id.as_str())
            .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))
    }

    pub fn remove(&mut self, id: &SessionId) -> Option<AgentSession> {
        self.sessions.remove(id.as_str())
    }

    /// Restores a session temporarily removed for asynchronous execution. A
    /// duplicate id is rejected so a runner cannot overwrite a concurrently
    /// managed session.
    pub fn restore(&mut self, session: AgentSession) -> CoreResult<()> {
        let id = session.id().clone();
        if self.sessions.contains_key(id.as_str()) {
            return Err(CoreError::TurnRejected(format!(
                "cannot restore duplicate session {id}"
            )));
        }
        self.sessions.insert(id.as_str().to_owned(), session);
        Ok(())
    }

    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}
