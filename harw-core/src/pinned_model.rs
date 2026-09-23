//! `PinnedModelProvider` — feste Modell-/Provider-Zuordnung für interne
//! Modellstellen (Addendum C).
//!
//! Interne Aufrufe des Cores — Session-Titel, Compaction-Zusammenfassung,
//! Dream-Reflexion, Explorer-/Research-Kinder — sollen wahlweise ein eigenes
//! Modell/Provider nutzen, **ohne** das Hauptmodell der Session anzufassen.
//! `PinnedModelProvider` ist die dafür vorgesehene Naht: er umhüllt einen
//! beliebigen [`crate::model::ModelProvider`] und überschreibt vor jedem
//! Aufruf `ModelRequest::model_id`/`ModelRequest::provider_id`, sofern beim
//! Bau des Wrappers ein Wert dafür angegeben wurde. Ist an einer Stelle
//! `None` gepinnt, bleibt der vom Aufrufer gesetzte Wert (i. d. R. das
//! Hauptmodell der Session) unverändert erhalten.
//!
//! # Nebenläufigkeit
//! `PinnedModelProvider` ist `Send + Sync`, solange der innere
//! `Arc<dyn ModelProvider>` es ist (per Trait-Bound erzwungen). Es hält
//! keinen eigenen Zustand außer den beiden optionalen IDs und dem `Arc` —
//! kein Lock nötig.
//!
//! # Fehler
//! Gibt jeden Fehler des inneren Providers unverändert weiter
//! ([`crate::model::ModelError`]).

use std::sync::Arc;

use crate::model::{ModelFuture, ModelProvider, ModelRequest};
use harw_types::{ModelId, ProviderId};

/// Umhüllt einen [`ModelProvider`] und pinnt Modell-/Provider-ID fest, sofern
/// gesetzt.
///
/// # Description
/// Wird an internen Modellstellen (Addendum C: `InternalModelPoint`)
/// verwendet, damit deren Requests den Session-Provider mit einer anderen
/// Modell-/Provider-Wahl durchlaufen, ohne dass `respond`-Aufrufer diese
/// Stellen selbst kennen müssen.
pub struct PinnedModelProvider {
    /// Der eigentliche Provider, an den nach dem Pinnen delegiert wird.
    inner: Arc<dyn ModelProvider>,
    /// Fest zugeordnete Provider-ID. `None`: `request.provider_id` bleibt
    /// unangetastet.
    provider_id: Option<ProviderId>,
    /// Fest zugeordnete Modell-ID. `None`: `request.model_id` bleibt
    /// unangetastet.
    model_id: Option<ModelId>,
}

impl PinnedModelProvider {
    /// Baut einen neuen `PinnedModelProvider` um `inner`.
    ///
    /// # Arguments
    /// - `inner` (`Arc<dyn ModelProvider>`): der umhüllte Provider, an den
    ///   `respond` nach dem optionalen Pinnen delegiert.
    /// - `provider_id` (`Option<ProviderId>`): feste Provider-ID, oder `None`
    ///   für „Request-Wert unverändert lassen".
    /// - `model_id` (`Option<ModelId>`): feste Modell-ID, oder `None` für
    ///   „Request-Wert unverändert lassen".
    ///
    /// # Returns
    /// Einen fertig konfigurierten `PinnedModelProvider`.
    #[must_use]
    pub fn new(
        inner: Arc<dyn ModelProvider>,
        provider_id: Option<ProviderId>,
        model_id: Option<ModelId>,
    ) -> Self {
        Self {
            inner,
            provider_id,
            model_id,
        }
    }

    /// `true`, wenn mindestens eine der beiden IDs gepinnt ist.
    ///
    /// # Returns
    /// `self.provider_id.is_some() || self.model_id.is_some()`.
    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.provider_id.is_some() || self.model_id.is_some()
    }
}

impl ModelProvider for PinnedModelProvider {
    /// Überschreibt `request.model_id`/`request.provider_id` mit den
    /// gepinnten Werten (sofern gesetzt) und delegiert dann an `inner`.
    ///
    /// # Description
    /// Der Trait [`ModelProvider`] hat aktuell genau eine Methode
    /// (`respond`) und keine weiteren Default-Methoden — sie ist die
    /// einzige, an die delegiert werden muss.
    ///
    /// # Arguments
    /// - `request` (`ModelRequest`): der Request, dessen `model_id`/
    ///   `provider_id` bei Bedarf ersetzt werden, bevor er an `inner`
    ///   weitergereicht wird.
    ///
    /// # Returns
    /// Das [`ModelFuture`] des inneren Providers, unverändert durchgereicht.
    ///
    /// # Concurrency
    /// Hält keine Locks; das zurückgegebene Future läuft vollständig im
    /// inneren Provider.
    fn respond<'a>(&'a self, mut request: ModelRequest) -> ModelFuture<'a> {
        if let Some(model_id) = self.model_id.clone() {
            request.model_id = Some(model_id);
        }
        if let Some(provider_id) = self.provider_id.clone() {
            request.provider_id = Some(provider_id);
        }
        self.inner.respond(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::ConversationHistory;
    use crate::test_support::{TestError, TestResult};
    use crate::testing::RecordingModelProvider;
    use harw_extension_api::LoadedInstructions;

    fn make_request() -> ModelRequest {
        let instructions = LoadedInstructions {
            system_prompt: "system".to_owned(),
            fragments: vec![],
        };
        ModelRequest::new(instructions, vec![], ConversationHistory::default(), vec![])
    }

    #[tokio::test]
    async fn test_pinned_model_provider_sets_pinned_ids() -> TestResult {
        let recorder = RecordingModelProvider::new();
        let inner: Arc<dyn ModelProvider> = Arc::new(recorder.clone());
        let pinned_provider = ProviderId::from("openrouter");
        let pinned_model = ModelId::from("nvidia/nemotron-3.5-lightning");
        let provider = PinnedModelProvider::new(
            inner,
            Some(pinned_provider.clone()),
            Some(pinned_model.clone()),
        );
        assert!(provider.is_pinned());

        let request = make_request();
        let _ = provider.respond(request).await?;

        let recorded = recorder
            .last()
            .ok_or(TestError::Missing("request must have been forwarded"))?;
        assert_eq!(recorded.model_id, Some(pinned_model));
        assert_eq!(recorded.provider_id, Some(pinned_provider));
        Ok(())
    }

    #[tokio::test]
    async fn test_pinned_model_provider_unpinned_passes_through_unchanged() -> TestResult {
        let recorder = RecordingModelProvider::new();
        let inner: Arc<dyn ModelProvider> = Arc::new(recorder.clone());
        let provider = PinnedModelProvider::new(inner, None, None);
        assert!(!provider.is_pinned());

        let request = make_request();
        assert!(request.model_id.is_none());
        assert!(request.provider_id.is_none());

        let _ = provider.respond(request).await?;

        let recorded = recorder
            .last()
            .ok_or(TestError::Missing("request must have been forwarded"))?;
        assert!(recorded.model_id.is_none());
        assert!(recorded.provider_id.is_none());
        Ok(())
    }
}
