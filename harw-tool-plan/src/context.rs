//! Der angeheftete Plan als Kontextbeitrag (Runde 5, Teil F, Punkt 5).
//!
//! # Beschreibung
//! Nach der Freigabe über `plan.exit` heftet die TUI den Plan an
//! ([`crate::session::PinnedPlan`]). Dieser Provider liefert ihn in **jeder**
//! Modellanfrage als eigenes Kontextfragment. Kontextfragmente entstehen je
//! Anfrage neu (`harw_core::turn_loop::gather_context`) und liegen nicht im
//! Verlauf — die Verdichtung (`harw_core::compaction`) kann den Plan deshalb
//! nie entfernen oder zusammenfassen.
//!
//! # Nebenläufigkeit
//! `Send + Sync`; liest die geteilte Zelle bei jedem Beitrag frisch.

use harw_extension_api::{ContextFragment, ContextProvider, ExtFuture, TurnInputContext};

use crate::session::{PinnedPlan, PinnedPlanDoc};

/// Namensraum und Label des Fragments.
pub const PINNED_PLAN_NAMESPACE: &str = "plan.pinned";

/// Liefert den angehefteten Plan als Kontext.
#[derive(Debug, Clone)]
pub struct PinnedPlanContextProvider {
    pinned: PinnedPlan,
}

impl PinnedPlanContextProvider {
    /// Baut den Provider über der geteilten Zelle.
    #[must_use]
    pub fn new(pinned: PinnedPlan) -> Self {
        Self { pinned }
    }
}

/// Das Fragment zu einem angehefteten Plan (testbar ohne Laufzeit).
#[must_use]
pub fn pinned_plan_fragment(doc: &PinnedPlanDoc) -> ContextFragment {
    ContextFragment {
        label: PINNED_PLAN_NAMESPACE.to_owned(),
        content: format!(
            "Freigegebener Plan (angeheftet, gilt für die Umsetzung; Datei {}). \
             Setze ihn Schritt für Schritt um und weiche nur nach Rückfrage ab.\n\n{}",
            doc.display_path, doc.content
        ),
    }
}

impl ContextProvider for PinnedPlanContextProvider {
    fn contribute<'a>(&'a self, _ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>> {
        let fragment = self.pinned.get().map(|doc| pinned_plan_fragment(&doc));
        Box::pin(async move { fragment.into_iter().collect() })
    }

    fn namespace(&self) -> &'static str {
        PINNED_PLAN_NAMESPACE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use harw_types::{SessionId, TurnId};

    #[tokio::test]
    async fn contributes_only_while_a_plan_is_pinned() -> TestResult {
        let pinned = PinnedPlan::default();
        let provider = PinnedPlanContextProvider::new(pinned.clone());
        let ctx = TurnInputContext {
            session_id: SessionId::new(),
            turn_id: TurnId::new(),
            metadata: serde_json::Value::Null,
        };
        assert!(provider.contribute(&ctx).await.is_empty());
        pinned.pin("auth", "# Plan\n1. Schritt");
        let fragments = provider.contribute(&ctx).await;
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].label, PINNED_PLAN_NAMESPACE);
        assert!(fragments[0].content.contains("1. Schritt"));
        assert!(fragments[0].content.contains(".harw/plans/auth.md"));
        assert_eq!(provider.namespace(), PINNED_PLAN_NAMESPACE);
        Ok(())
    }
}
