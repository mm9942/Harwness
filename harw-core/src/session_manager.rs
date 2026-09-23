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
    /// Live-Bus, den jede hier angelegte oder wieder eingesetzte Session
    /// bekommt (Wurzel, Kinder, UIA-Worker).
    agent_events: Option<crate::agent_events::AgentEventHub>,
}

impl SessionManager {
    pub fn new(event_tx: mpsc::UnboundedSender<SessionEvent>) -> Self {
        Self {
            sessions: HashMap::new(),
            event_tx,
            agent_events: None,
        }
    }

    /// Hängt jeder von diesem Manager verwalteten Session den Live-Bus an.
    #[must_use]
    pub fn with_agent_events(mut self, hub: crate::agent_events::AgentEventHub) -> Self {
        self.agent_events = Some(hub);
        self
    }

    /// Der Live-Bus dieses Managers, falls gesetzt.
    #[must_use]
    pub fn agent_events(&self) -> Option<&crate::agent_events::AgentEventHub> {
        self.agent_events.as_ref()
    }

    fn attach_hub(&self, session: &mut AgentSession) {
        if session.agent_events().is_none() {
            session.set_agent_events(self.agent_events.clone());
        }
    }

    pub fn create_session(
        &mut self,
        role: AgentRole,
        parent: Option<SessionId>,
        registry: ExtensionRegistry,
    ) -> SessionId {
        let mut session = AgentSession::new(role, parent, registry, self.event_tx.clone());
        self.attach_hub(&mut session);
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
        let mut session = AgentSession::new(role, parent, registry, self.event_tx.clone())
            .with_spawn_context(spawn_context);
        self.attach_hub(&mut session);
        let id = session.id().clone();
        self.sessions.insert(id.as_str().to_owned(), session);
        id
    }

    /// Liefert eine Kopie des Event-Senders, mit dem dieser Manager Sessions anlegt.
    ///
    /// # Description
    /// A-CHILD (F-183). Der Kind-Controller baut eine neue Kind-Session samt
    /// Registry **außerhalb** des Manager-Locks und legt sie danach über
    /// [`Self::restore`] ein. Dafür braucht er denselben Sender, den
    /// [`Self::create_governed_session`] intern verwendet — sonst liefen die
    /// Events des Kindes in einen anderen Kanal.
    ///
    /// # Returns
    /// Einen geklonten `mpsc::UnboundedSender<SessionEvent>` (billig, teilt den Kanal).
    ///
    /// # Concurrency
    /// Braucht nur `&self`; der Aufrufer hält den Manager-Lock nur für diesen Aufruf.
    #[must_use]
    pub fn event_sender(&self) -> mpsc::UnboundedSender<SessionEvent> {
        self.event_tx.clone()
    }

    /// Prüft, ob eine Session mit dieser ID gerade im Manager liegt.
    ///
    /// # Description
    /// A-CHILD. Eine während eines Kind-Turns entnommene Session (`remove`)
    /// ist hier bis zum `restore` **nicht** sichtbar; der Reaper nutzt genau
    /// das, um laufende von ruhenden Kindern zu unterscheiden.
    ///
    /// # Returns
    /// `true`, wenn `id` registriert ist.
    #[must_use]
    pub fn contains(&self, id: &SessionId) -> bool {
        self.sessions.contains_key(id.as_str())
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
    pub fn restore(&mut self, mut session: AgentSession) -> CoreResult<()> {
        self.attach_hub(&mut session);
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
