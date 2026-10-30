//! Gemeinsame Kontext-Helfer der Wissens-Ops (`/workbench`, `/kanban`,
//! `/diary`, `/palace`, `/dream`).
//!
//! # Beschreibung
//! - [`knowledge_store`] löst den Speicher aus dem [`OpContext`] auf.
//! - [`KnowledgeCaller`] bündelt, *wer* schreibt ([`caller_agent`]), *mit
//!   welcher Sicht* gelesen wird ([`viewer_scope`]) und — falls die Montage
//!   sie vorgibt — die vertrauenswürdige [`CallerIdentity`]; abgeleitet aus
//!   dem echten [`Principal`] im Kontext statt fest verdrahtet `OperatorOnly`.
//! - [`KnowledgeCaller::query`] baut die [`RecallQuery`] des Aufrufers: mit
//!   Sicht und, wenn vorhanden, gebundener Identität. Lese-Ops bauen ihre
//!   Anfragen ausschließlich darüber.
//! - [`publish_knowledge`] meldet einen Schreibvorgang als
//!   `AgentEventKind::Knowledge` über den `AgentEventHub`, falls der Kontext
//!   einen trägt (wie der Matrix-Runner, `crate::matrix::runner::EventSink`).
//! - [`map_knowledge_error`] übersetzt [`KnowledgeError`] in [`OpError`].
//!
//! # Sichtregel
//! | Principal | Sicht | Identität |
//! |---|---|---|
//! | keiner im Kontext (Tests, Alt-Montagen) | `OperatorOnly` | Dienst `Arc<CallerIdentity>`, sonst keine |
//! | `Human` (TUI, CLI, Web) | `OperatorOnly` | Dienst `Arc<CallerIdentity>`, sonst keine |
//! | `Channel` (MCP-Client, Job im Auftrag eines Operators, Telegram) | `OperatorOnly` | Dienst `Arc<CallerIdentity>`, sonst keine |
//! | `Model`, `Operation` (Agent, Kind-Agent, Gateway-Traum) | `SelfOnly` | Dienst `Arc<CallerIdentity>`, sonst keine |
//! | [`KnowledgeCaller::operator`] (reine Kerne, Tests) | `OperatorOnly` | Operator-Identität des Autors |
//!
//! Die Identität kommt ausschließlich aus dem Dienst, den die Montage
//! (Admission) in den Kontext legt — nie aus Agent-Eingaben. Fehlt sie, bleibt
//! es bei der Skopus-Heuristik von `VisibilityScope::visible_to_caller`.
//!
//! Kanäle sind menschliche Operatoren hinter einer Transportfläche; ihre
//! Befugnis begrenzt bereits die `permission`-Stufe der Op (Telegram ist
//! `Observer` und erreicht keine Wissens-Op). Ein Agent sieht mit
//! `SelfOnly` ohne Identität fail-closed nichts, was
//! `VisibilityScope::visible_to_caller` nicht beweisen kann; mit gebundener
//! Identität entscheidet `harw_knowledge::artifact::artifact_visible` (eigene
//! Einträge, Nachfahren-Bäume, erteilte Rollen) — Geschwister sehen einander
//! nie.

use std::sync::Arc;

use harw_core::agent_events::AgentEventHub;
use harw_knowledge::artifact::RecallQuery;
use harw_knowledge::caller_identity::CallerIdentity;
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

/// Aufrufer einer Wissens-Op: Autor-Identität, Lesesicht und optional die
/// vertrauenswürdige [`CallerIdentity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeCaller {
    /// Identität, unter der geschrieben wird (Pfadkomponente-sicher).
    pub agent: AgentId,
    /// Sicht für `VisibilityScope::visible_to_caller`.
    pub viewer: VisibilityScope,
    /// Von der Montage vorgegebene Identität (Rolle, Vorfahren, Operator-Flag)
    /// für identitätsgebundenes Recall; `None` = Skopus-Heuristik wie bisher.
    pub identity: Option<CallerIdentity>,
}

impl KnowledgeCaller {
    /// Ein Operator mit Autor `agent` (Sicht `OperatorOnly`, Operator-Identität).
    #[must_use]
    pub fn operator(agent: AgentId) -> Self {
        let identity = CallerIdentity::new(agent.clone()).as_operator();
        Self {
            agent,
            viewer: VisibilityScope::OperatorOnly,
            identity: Some(identity),
        }
    }

    /// Leitet den Aufrufer aus dem Principal im Kontext ab (siehe Moduldoku).
    ///
    /// Die Identität stammt aus dem Dienst `Arc<CallerIdentity>`; ohne diesen
    /// Dienst bleibt sie `None`.
    #[must_use]
    pub fn from_context(ctx: &OpContext) -> Self {
        let principal = ctx.service::<Principal>();
        Self {
            agent: agent_id_for(principal),
            viewer: viewer_scope(principal),
            identity: ctx
                .service::<Arc<CallerIdentity>>()
                .map(|identity| (**identity).clone()),
        }
    }

    /// `true`, wenn der Aufrufer ein Artefakt mit Sichtbarkeit `scope` lesen
    /// darf.
    #[must_use]
    pub fn can_read(&self, scope: &VisibilityScope) -> bool {
        scope.visible_to_caller(&self.viewer)
    }

    /// Baut die [`RecallQuery`] dieses Aufrufers: Text und Lesesicht, plus die
    /// gebundene Identität, falls vorhanden (identitätsbasierte Sichtbarkeit
    /// statt Skopus-Heuristik). Alle übrigen Schranken bleiben die Standardwerte
    /// von [`RecallQuery::new`].
    #[must_use]
    pub fn query(&self, text: impl Into<String>) -> RecallQuery {
        let query = RecallQuery::new(text, self.viewer.clone());
        match &self.identity {
            Some(identity) => query.with_caller_identity(identity.clone()),
            None => query,
        }
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
    use crate::test_support::{TestError, TestResult};
    use harw_knowledge::caller_identity::CallerIdentity;
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
            identity: None,
        };
        assert!(!agent.can_read(&VisibilityScope::OperatorOnly));
        let operator = KnowledgeCaller::operator(AgentId::new("operator"));
        assert!(operator.can_read(&VisibilityScope::OperatorOnly));
    }

    #[test]
    fn caller_with_identity_builds_identity_bound_query() -> TestResult {
        // Agent mit gebundener Identität: Anfrage trägt Sicht und Identität.
        let identity = CallerIdentity::new(AgentId::new("explorer"))
            .with_ancestors(vec![AgentId::new("orchestrator")]);
        let agent = KnowledgeCaller {
            agent: AgentId::new("explorer"),
            viewer: VisibilityScope::SelfOnly,
            identity: Some(identity.clone()),
        };
        let query = agent.query("canary");
        assert_eq!(query.text, "canary");
        assert_eq!(query.caller_scope, VisibilityScope::SelfOnly);
        assert_eq!(query.caller_identity, Some(identity));

        // Operator: Operator-Identität des Autors, Sicht `OperatorOnly`.
        let operator = KnowledgeCaller::operator(AgentId::new("operator"));
        let operator_query = operator.query(String::new());
        assert_eq!(operator_query.caller_scope, VisibilityScope::OperatorOnly);
        let operator_identity = operator_query
            .caller_identity
            .ok_or(TestError::Missing("operator identity"))?;
        assert!(operator_identity.operator);
        assert_eq!(operator_identity.agent_id, AgentId::new("operator"));

        // Ohne Identität bleibt es bei der Skopus-Heuristik.
        let legacy = KnowledgeCaller {
            agent: AgentId::new("operator"),
            viewer: VisibilityScope::OperatorOnly,
            identity: None,
        };
        assert!(legacy.query("x").caller_identity.is_none());
        Ok(())
    }
}
