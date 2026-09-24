//! Agenten-übergreifender Live-Event-Bus.
//!
//! Jede [`AgentSession`](crate::AgentSession), der ein [`AgentEventHub`]
//! angehängt ist (Wurzel **und** alle Kinder, inklusive UIA-Worker), spiegelt
//! ihre [`TurnEvent`]s mit Absender-Kennung hierher. Dazu kommen
//! Orchestrierungs-Events des Spawners und Usage-Meldungen interner
//! Modell-Aufrufe (Kompaktierung, Titel, Verbindungstest), die sonst nirgends
//! gezählt würden.
//!
//! Der Hub ist ein `tokio::sync::broadcast`: beliebig viele Beobachter (TUI,
//! Web, Telemetrie), Senden blockiert nie, ein zu langsamer Beobachter
//! verliert die ältesten Events (`RecvError::Lagged`) statt den Turn-Loop
//! auszubremsen.

use harw_protocol::{AgentOrchestrationEvent, TurnEvent};
use harw_types::{SessionId, TokenUsage};
use tokio::sync::broadcast;

/// Standard-Kapazität des Broadcast-Puffers (Token-Deltas sind zahlreich).
pub const DEFAULT_AGENT_EVENT_CAPACITY: usize = 8192;

/// Nutzlast eines [`AgentEvent`].
#[derive(Debug, Clone)]
pub enum AgentEventKind {
    /// Ein Turn-Event der Absender-Session.
    Turn(TurnEvent),
    /// Ein Orchestrierungs-Event des Spawners (Admission, Status, Abschluss).
    Orchestration(AgentOrchestrationEvent),
    /// Token-Nutzung eines internen, nicht-Turn-Modellaufrufs.
    InternalUsage {
        /// Zweck, z. B. `compaction`, `title`, `connection_check`.
        purpose: String,
        usage: TokenUsage,
    },
}

/// Ein Event mit Absender-Kennung.
#[derive(Debug, Clone)]
pub struct AgentEvent {
    /// Session, die das Event erzeugt hat.
    pub agent: SessionId,
    /// Eltern-Session, falls `agent` ein Kind ist.
    pub parent: Option<SessionId>,
    /// Anzeigename der Rolle (z. B. `explorer`, `uia-worker`, `assistant`).
    pub role: String,
    pub kind: AgentEventKind,
}

/// Geteilter, billig klonbarer Event-Bus.
#[derive(Debug, Clone)]
pub struct AgentEventHub {
    tx: broadcast::Sender<AgentEvent>,
}

impl Default for AgentEventHub {
    fn default() -> Self {
        Self::new(DEFAULT_AGENT_EVENT_CAPACITY)
    }
}

impl AgentEventHub {
    /// Neuer Hub mit gegebener Puffer-Kapazität (mindestens 1).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Self { tx }
    }

    /// Neuer Beobachter; sieht nur Events ab jetzt.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.tx.subscribe()
    }

    /// Veröffentlicht ein Event (No-op ohne Beobachter).
    pub fn publish(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }

    /// Anzahl aktiver Beobachter.
    #[must_use]
    pub fn observer_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

/// [`crate::child_controller::OrchestrationObserver`], der Orchestrierungs-
/// Events live in einen [`AgentEventHub`] spiegelt und optional an einen
/// weiteren Observer (typisch: den persistierenden StateStore-Observer)
/// weiterreicht.
pub struct HubOrchestrationObserver {
    hub: AgentEventHub,
    next: Option<std::sync::Arc<dyn crate::child_controller::OrchestrationObserver>>,
}

impl HubOrchestrationObserver {
    #[must_use]
    pub fn new(
        hub: AgentEventHub,
        next: Option<std::sync::Arc<dyn crate::child_controller::OrchestrationObserver>>,
    ) -> Self {
        Self { hub, next }
    }
}

impl std::fmt::Debug for HubOrchestrationObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubOrchestrationObserver")
            .field("chained", &self.next.is_some())
            .finish()
    }
}

impl crate::child_controller::OrchestrationObserver for HubOrchestrationObserver {
    fn on_orchestration_event(&self, event: AgentOrchestrationEvent) {
        if self.hub.observer_count() > 0 {
            self.hub.publish(AgentEvent {
                agent: event.child_session_id.clone(),
                parent: Some(event.parent_session_id.clone()),
                role: event.role.clone(),
                kind: AgentEventKind::Orchestration(event.clone()),
            });
        }
        if let Some(next) = &self.next {
            next.on_orchestration_event(event);
        }
    }
}

/// [`crate::ModelProvider`]-Hülle für interne Einmal-Aufrufe (Titel,
/// Verbindungstest …): meldet die Token-Nutzung jeder erfolgreichen Antwort
/// als [`AgentEventKind::InternalUsage`] auf dem Bus, damit auch diese
/// Aufrufe in der Live-Summe auftauchen.
pub struct UsageReportingProvider {
    inner: std::sync::Arc<dyn crate::ModelProvider>,
    hub: AgentEventHub,
    agent: SessionId,
    purpose: String,
}

impl UsageReportingProvider {
    #[must_use]
    pub fn new(
        inner: std::sync::Arc<dyn crate::ModelProvider>,
        hub: AgentEventHub,
        agent: SessionId,
        purpose: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            hub,
            agent,
            purpose: purpose.into(),
        }
    }
}

impl crate::ModelProvider for UsageReportingProvider {
    fn respond<'a>(&'a self, request: crate::ModelRequest) -> crate::ModelFuture<'a> {
        Box::pin(async move {
            let response = self.inner.respond(request).await?;
            self.hub.publish(AgentEvent {
                agent: self.agent.clone(),
                parent: None,
                role: "internal".into(),
                kind: AgentEventKind::InternalUsage {
                    purpose: self.purpose.clone(),
                    usage: response.usage.clone(),
                },
            });
            Ok(response)
        })
    }

    /// Reicht die gepinnte Modell-ID des umhüllten Providers durch.
    ///
    /// # Description
    /// Die Hülle meldet nur die Token-Nutzung und ändert das angesprochene
    /// Modell nicht; ein Pin des inneren Providers bleibt so sichtbar.
    ///
    /// # Returns
    /// `self.inner.pinned_model_id()`.
    fn pinned_model_id(&self) -> Option<String> {
        self.inner.pinned_model_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hub_delivers_to_every_subscriber() -> Result<(), Box<dyn std::error::Error>> {
        let hub = AgentEventHub::new(8);
        let mut a = hub.subscribe();
        let mut b = hub.subscribe();
        let agent = SessionId::try_from_str("s-1")?;
        hub.publish(AgentEvent {
            agent: agent.clone(),
            parent: None,
            role: "assistant".into(),
            kind: AgentEventKind::InternalUsage {
                purpose: "title".into(),
                usage: TokenUsage::default(),
            },
        });
        assert_eq!(a.recv().await?.agent, agent);
        assert_eq!(b.recv().await?.agent, agent);
        assert_eq!(hub.observer_count(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn usage_reporting_provider_publishes_internal_usage()
    -> Result<(), Box<dyn std::error::Error>> {
        let hub = AgentEventHub::new(4);
        let mut rx = hub.subscribe();
        let provider = UsageReportingProvider::new(
            std::sync::Arc::new(crate::model::EchoModelProvider::new("hi")),
            hub,
            SessionId::try_from_str("s-3")?,
            "title",
        );
        let request = crate::ModelRequest::new(
            harw_extension_api::LoadedInstructions {
                system_prompt: String::new(),
                fragments: Vec::new(),
            },
            Vec::new(),
            crate::ConversationHistory::new(),
            Vec::new(),
        );
        crate::ModelProvider::respond(&provider, request).await?;
        let event = rx.recv().await?;
        assert!(matches!(
            event.kind,
            AgentEventKind::InternalUsage { ref purpose, .. } if purpose == "title"
        ));
        Ok(())
    }

    #[test]
    fn publish_without_subscribers_is_a_noop() -> Result<(), Box<dyn std::error::Error>> {
        let hub = AgentEventHub::default();
        hub.publish(AgentEvent {
            agent: SessionId::try_from_str("s-2")?,
            parent: None,
            role: "x".into(),
            kind: AgentEventKind::InternalUsage {
                purpose: "p".into(),
                usage: TokenUsage::default(),
            },
        });
        Ok(())
    }
}
