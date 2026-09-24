//! Gemeinsame Kontext-Helfer der Wissens-Ops (`/workbench`, `/kanban`,
//! `/diary`, `/palace`, `/dream`).
//!
//! # Beschreibung
//! - [`knowledge_store`] löst den Speicher aus dem [`OpContext`] auf.
//! - [`KnowledgeCaller`] bündelt, *wer* schreibt ([`caller_agent`]) und *mit
//!   welcher Sicht* gelesen wird ([`viewer_scope`]) — abgeleitet aus dem
//!   echten [`Principal`] im Kontext statt fest verdrahtet `OperatorOnly`.
//! - [`publish_knowledge`] meldet einen Schreibvorgang als
//!   `AgentEventKind::Knowledge` über den `AgentEventHub`, falls der Kontext
//!   einen trägt (wie der Matrix-Runner, `crate::matrix::runner::EventSink`).
//! - [`map_knowledge_error`] übersetzt [`KnowledgeError`] in [`OpError`].
//!
//! # Sichtregel
//! | Principal | Sicht |
//! |---|---|
//! | keiner im Kontext (Tests, Alt-Montagen) | `OperatorOnly` |
//! | `Human` (TUI, CLI, Web) | `OperatorOnly` |
//! | `Channel` (MCP-Client, Job im Auftrag eines Operators, Telegram) | `OperatorOnly` |
//! | `Model`, `Operation` (Agent, Kind-Agent, Gateway-Traum) | `SelfOnly` |
//!
//! Kanäle sind menschliche Operatoren hinter einer Transportfläche; ihre
//! Befugnis begrenzt bereits die `permission`-Stufe der Op (Telegram ist
//! `Observer` und erreicht keine Wissens-Op). Ein Agent sieht mit
//! `SelfOnly` fail-closed nichts, was `VisibilityScope::visible_to_caller`
//! nicht beweisen kann — Identitätsprüfungen für eigene Einträge ergänzen
//! die Lese-Werkzeuge der Folgewellen.

use std::sync::Arc;

use harw_core::agent_events::AgentEventHub;
use harw_knowledge::{AgentId, KnowledgeError, KnowledgeStore, VisibilityScope};
use harw_operations::{OpContext, OpError};
use harw_types::{Principal, PrincipalKind};

/// Fläche `/workbench` im Knowledge-Event.
pub const AREA_WORKBENCH: &str = "workbench";
/// Fläche `/kanban` im Knowledge-Event.
pub const AREA_KANBAN: &str = "kanban";
/// Fläche `/diary` im Knowledge-Event.
pub const AREA_DIARY: &str = "diary";
/// Fläche `/palace` im Knowledge-Event.
pub const AREA_PALACE: &str = "palace";
/// Fläche `/dream` im Knowledge-Event.
pub const AREA_DREAM: &str = "dream";

/// Löst `Arc<KnowledgeStore>` aus dem Kontext auf.
///
/// # Fehler
/// [`OpError::NotAvailable`], wenn die Fläche keinen Speicher trägt.
pub fn knowledge_store(ctx: &OpContext) -> Result<Arc<KnowledgeStore>, OpError> {
    ctx.service::<Arc<KnowledgeStore>>()
        .cloned()
        .ok_or_else(|| OpError::NotAvailable("kein Knowledge-Store im Kontext".to_owned()))
}

/// Aufrufer einer Wissens-Op: Autor-Identität plus Lesesicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeCaller {
    /// Identität, unter der geschrieben wird (Pfadkomponente-sicher).
    pub agent: AgentId,
    /// Sicht für `VisibilityScope::visible_to_caller`.
    pub viewer: VisibilityScope,
}

impl KnowledgeCaller {
    /// Ein Operator mit Autor `agent` (Sicht `OperatorOnly`).
    #[must_use]
    pub fn operator(agent: AgentId) -> Self {
        Self {
            agent,
            viewer: VisibilityScope::OperatorOnly,
        }
    }

    /// Leitet den Aufrufer aus dem Principal im Kontext ab (siehe Moduldoku).
    #[must_use]
    pub fn from_context(ctx: &OpContext) -> Self {
        let principal = ctx.service::<Principal>();
        Self {
            agent: agent_id_for(principal),
            viewer: viewer_scope(principal),
        }
    }

    /// `true`, wenn der Aufrufer ein Artefakt mit Sichtbarkeit `scope` lesen
    /// darf.
    #[must_use]
    pub fn can_read(&self, scope: &VisibilityScope) -> bool {
        scope.visible_to_caller(&self.viewer)
    }
}

/// Die Lesesicht eines Principals (Tabelle in der Moduldoku).
#[must_use]
pub fn viewer_scope(principal: Option<&Principal>) -> VisibilityScope {
    match principal.map(Principal::kind) {
        None | Some(PrincipalKind::Human | PrincipalKind::Channel) => VisibilityScope::OperatorOnly,
        Some(PrincipalKind::Model | PrincipalKind::Operation) => VisibilityScope::SelfOnly,
    }
}

/// Die Identität, unter der eine Wissens-Op schreibt: die Principal-Id,
/// sonst `operator`.
///
/// Pfadtrenner und Steuerzeichen werden durch `-` ersetzt, weil die Id als
/// Pfadkomponente dient (`diary/<agent-id>/…`).
#[must_use]
pub fn caller_agent(ctx: &OpContext) -> AgentId {
    agent_id_for(ctx.service::<Principal>())
}

fn agent_id_for(principal: Option<&Principal>) -> AgentId {
    let raw = principal
        .map(|principal| principal.id().to_owned())
        .filter(|id| !id.trim().is_empty() && id != "." && id != "..")
        .unwrap_or_else(|| "operator".to_owned());
    let safe: String = raw
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    AgentId::new(safe)
}

/// Meldet einen Schreibvorgang auf einer Wissensfläche über den
/// `AgentEventHub` im Kontext; ohne Hub ein No-op.
///
/// # Argumente
/// - `area` — eine der `AREA_*`-Konstanten.
/// - `id` — betroffene Einheit, falls bekannt.
pub fn publish_knowledge(ctx: &OpContext, area: &str, id: Option<String>) {
    if let Some(hub) = ctx.service::<Arc<AgentEventHub>>() {
        hub.publish_knowledge(ctx.session_id().clone(), area, id);
    }
}

/// Übersetzt [`KnowledgeError`] in [`OpError`]: Eingabe-/Zustandsfehler
/// werden [`OpError::InvalidArguments`], alles andere [`OpError::Execution`].
#[must_use]
pub fn map_knowledge_error(error: KnowledgeError) -> OpError {
    let caller_error = match &error {
        KnowledgeError::Io(io) => io.kind() == std::io::ErrorKind::InvalidInput,
        KnowledgeError::ArtifactNotFound(_)
        | KnowledgeError::IllegalTransition { .. }
        | KnowledgeError::ArchiveBlockedByChildren { .. }
        | KnowledgeError::ClaimRequiresApproval { .. }
        | KnowledgeError::PromotionNotReviewed { .. }
        | KnowledgeError::VisibilityDenied { .. }
        | KnowledgeError::RecallBoundExceeded { .. } => true,
        _ => false,
    };
    if caller_error {
        OpError::InvalidArguments(error.to_string())
    } else {
        OpError::Execution(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{KnowledgeCaller, viewer_scope};
    use harw_knowledge::{AgentId, VisibilityScope};
    use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};

    fn principal(kind: PrincipalKind) -> Principal {
        Principal::trusted_ingress(kind, "p", IngressSurface::Tui, PermissionTier::Operator)
    }

    #[test]
    fn humans_and_channels_read_as_operator_agents_fail_closed() {
        assert_eq!(viewer_scope(None), VisibilityScope::OperatorOnly);
        for kind in [PrincipalKind::Human, PrincipalKind::Channel] {
            assert_eq!(
                viewer_scope(Some(&principal(kind))),
                VisibilityScope::OperatorOnly
            );
        }
        for kind in [PrincipalKind::Model, PrincipalKind::Operation] {
            assert_eq!(
                viewer_scope(Some(&principal(kind))),
                VisibilityScope::SelfOnly
            );
        }
    }

    #[test]
    fn an_agent_caller_cannot_read_operator_only_artifacts() {
        let agent = KnowledgeCaller {
            agent: AgentId::new("root-explorer"),
            viewer: VisibilityScope::SelfOnly,
        };
        assert!(!agent.can_read(&VisibilityScope::OperatorOnly));
        let operator = KnowledgeCaller::operator(AgentId::new("operator"));
        assert!(operator.can_read(&VisibilityScope::OperatorOnly));
    }
}
