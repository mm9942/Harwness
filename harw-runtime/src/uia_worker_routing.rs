//! Modellwahl der UIA-Worker-Rollen zur Laufzeit (Runde 5, Teil G).
//!
//! # Ursache des alten Fehlers
//! Die `uia-worker`-Familie bekam bis Runde 5 **ein** Modell, beim Start
//! einmal aus dem UIA-Client abgeleitet (`build_uia_worker_model`): ein
//! [`PinnedModelProvider`], der nur die Modell-Kennung `uia_worker_model`
//! pinnte, während der Provider stets der UIA-Provider des Starts blieb.
//! Wechselte die Nutzerin den UIA-Provider, passte der Worker-Pin nicht mehr
//! (Provider-/Modell-Mismatch, `/models set uia-worker` und
//! `/uia-worker-model switch` lehnten fremde Provider ab), und Worker ohne
//! Pin blieben beim UIA-Modell des Starts statt der neuen Wahl zu folgen.
//!
//! # Neue Regel
//! [`UiaWorkerRouting`] hält je Rolle eine eigene Wahl
//! ([`UiaWorkerModelChoice`]): eine feste Wahl pinnt Provider **und**
//! Modell; „wie UIA“ folgt der zuletzt an die UIA-Wurzel übergebenen
//! Live-Auswahl ([`UiaWorkerRouting::set_live_uia`]). Das Modell wird beim
//! **Start** eines Workers gebaut ([`UiaWorkerRouting::model_for_role`]) —
//! laufende Worker behalten es bis zum Ende ihres Laufs, neue Worker nehmen
//! die neue Wahl.
//!
//! # Nebenläufigkeit
//! Der Zustand liegt hinter einem `RwLock`; eine vergiftete Sperre gilt als
//! „keine Live-Auswahl“ (Rückfall auf den UIA-Client des Starts), nie als
//! Panik.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use harw_config::{ResolvedConfig, UiaWorkerModelChoice, catalog_provider_of};
use harw_core::{ModelProvider, PinnedModelProvider};
use harw_types::{ModelId, ProviderId};

/// Live-Auswahl der UIA-Wurzel (Provider optional, Modell gesetzt).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveUiaRoute {
    provider: Option<String>,
    model: String,
}

/// Veränderlicher Teil von [`UiaWorkerRouting`].
#[derive(Debug, Default)]
struct RoutingState {
    /// Zuletzt an die UIA-Wurzel übergebene Auswahl; `None` → der
    /// UIA-Client des Starts entscheidet (seine Standardroute).
    live_uia: Option<LiveUiaRoute>,
    /// Wirksame Wahl je Rolle; fehlende Rollen folgen der UIA.
    choices: HashMap<String, UiaWorkerModelChoice>,
}

/// Modellwahl der UIA-Worker-Rollen, geteilt zwischen Montage, Kind-Fabrik
/// und TUI.
pub struct UiaWorkerRouting {
    /// Der UIA-Client des Starts (Vorgabe-Router, ggf. mit
    /// UIA-Standardroute); jedes Worker-Modell wird darum gebaut.
    base: Arc<dyn ModelProvider>,
    /// Modell-Kennung → Provider laut Katalog, für eine Live-Auswahl ohne
    /// Provider.
    catalog: HashMap<String, String>,
    state: RwLock<RoutingState>,
}

impl std::fmt::Debug for UiaWorkerRouting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.read().ok();
        f.debug_struct("UiaWorkerRouting")
            .field(
                "live_uia",
                &state.as_ref().map(|state| state.live_uia.clone()),
            )
            .field("choices", &state.as_ref().map(|state| state.choices.len()))
            .finish_non_exhaustive()
    }
}

impl UiaWorkerRouting {
    /// Baut die Wahl aus der Konfiguration.
    ///
    /// # Beschreibung
    /// Jede Rolle aus [`harw_config::UIA_WORKER_ROLES`] wird über
    /// [`harw_config::resolve_uia_worker_models`] aufgelöst; ein Rückfall
    /// („Provider nicht angemeldet“) wird protokolliert.
    ///
    /// # Argumente
    /// - `config`: die aufgelöste Konfiguration des Laufs.
    /// - `base`: der UIA-Client des Starts.
    #[must_use]
    pub fn from_config(config: &ResolvedConfig, base: Arc<dyn ModelProvider>) -> Self {
        let mut choices = HashMap::new();
        for resolved in harw_config::resolve_uia_worker_models(config) {
            if let Some(notice) = &resolved.notice {
                tracing::warn!(role = %resolved.role, notice = %notice, "uia_worker.model_fallback");
            }
            choices.insert(resolved.role, resolved.choice);
        }
        let mut catalog = HashMap::new();
        for (key, entry) in &config.models {
            if let Some(provider) = catalog_provider_of(config, key) {
                catalog.insert(key.clone(), provider.clone());
                catalog.insert(entry.id.clone(), provider);
            }
        }
        Self {
            base,
            catalog,
            state: RwLock::new(RoutingState {
                live_uia: None,
                choices,
            }),
        }
    }

    /// Merkt sich die Live-Auswahl der UIA-Wurzel.
    ///
    /// # Beschreibung
    /// Die TUI ruft das an jeder Turn-Grenze nach dem Übernehmen der
    /// Controller-Auswahl auf. Ohne Modell bleibt es beim UIA-Client des
    /// Starts. Wirkt nur auf **neu** gestartete Worker.
    pub fn set_live_uia(&self, provider: Option<String>, model: Option<String>) {
        let route = model
            .filter(|model| !model.trim().is_empty())
            .map(|model| LiveUiaRoute {
                provider: provider.filter(|provider| !provider.trim().is_empty()),
                model,
            });
        if let Ok(mut state) = self.state.write() {
            state.live_uia = route;
        }
    }

    /// Setzt die Wahl einer Rolle für künftige Worker-Starts.
    pub fn set_choice(&self, role: &str, choice: UiaWorkerModelChoice) {
        if let Ok(mut state) = self.state.write() {
            state.choices.insert(role.to_owned(), choice);
        }
    }

    /// Setzt alle bekannten Rollen auf „wie UIA“.
    pub fn follow_uia_everywhere(&self) {
        if let Ok(mut state) = self.state.write() {
            for choice in state.choices.values_mut() {
                *choice = UiaWorkerModelChoice::FollowUia;
            }
            for role in harw_config::UIA_WORKER_ROLES {
                state
                    .choices
                    .insert(role.to_owned(), UiaWorkerModelChoice::FollowUia);
            }
        }
    }

    /// Wirksame Wahl einer Rolle (unbekannte Rollen folgen der UIA).
    #[must_use]
    pub fn choice_for(&self, role: &str) -> UiaWorkerModelChoice {
        self.state
            .read()
            .ok()
            .and_then(|state| state.choices.get(role).cloned())
            .unwrap_or(UiaWorkerModelChoice::FollowUia)
    }

    /// Provider/Modell, das ein jetzt gestarteter Worker der Rolle ansprechen
    /// würde; `None` → der UIA-Client des Starts entscheidet.
    #[must_use]
    pub fn route_for_role(&self, role: &str) -> Option<(Option<String>, String)> {
        match self.choice_for(role) {
            UiaWorkerModelChoice::Fixed { provider, model } => Some((Some(provider), model)),
            UiaWorkerModelChoice::FollowUia => {
                let live = self
                    .state
                    .read()
                    .ok()
                    .and_then(|state| state.live_uia.clone())?;
                let provider = live
                    .provider
                    .or_else(|| self.catalog.get(&live.model).cloned());
                Some((provider, live.model))
            }
        }
    }

    /// Baut das Modell für einen **neuen** Worker der Rolle.
    ///
    /// # Rückgabe
    /// Ein [`PinnedModelProvider`] um den UIA-Client, der Provider und Modell
    /// der Wahl fest anspricht, oder — für „wie UIA“ ohne Live-Auswahl — den
    /// UIA-Client des Starts unverändert.
    #[must_use]
    pub fn model_for_role(&self, role: &str) -> Arc<dyn ModelProvider> {
        match self.route_for_role(role) {
            Some((provider, model)) => {
                tracing::debug!(
                    role,
                    provider = provider.as_deref().unwrap_or(""),
                    model = %model,
                    "uia_worker.model"
                );
                Arc::new(PinnedModelProvider::new(
                    Arc::clone(&self.base),
                    provider.map(ProviderId::from),
                    Some(ModelId::from(model)),
                ))
            }
            None => Arc::clone(&self.base),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_core::{ModelError, ModelFuture, ModelRequest};
    use harw_extension_api::types::LoadedInstructions;
    use std::sync::Mutex;

    /// Merkt sich Provider/Modell jeder Anfrage und scheitert dann.
    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<(Option<String>, Option<String>)>>,
    }

    impl ModelProvider for Recorder {
        fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push((
                    request.provider_id.map(|id| id.as_str().to_owned()),
                    request.model_id.map(|id| id.as_str().to_owned()),
                ));
            }
            Box::pin(async { Err(ModelError::RequestFailed("recorder".to_owned())) })
        }
    }

    fn ask(provider: &dyn ModelProvider) -> TestResult {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        let request = ModelRequest::new(
            LoadedInstructions::default(),
            Vec::new(),
            harw_core::ConversationHistory::new(),
            Vec::new(),
        );
        let _ = runtime.block_on(provider.respond(request));
        Ok(())
    }

    fn last(recorder: &Recorder) -> Option<(Option<String>, Option<String>)> {
        recorder
            .seen
            .lock()
            .ok()
            .and_then(|seen| seen.last().cloned())
    }

    fn routing(config: &ResolvedConfig) -> (Arc<Recorder>, UiaWorkerRouting) {
        let recorder = Arc::new(Recorder::default());
        let base: Arc<dyn ModelProvider> = recorder.clone();
        (recorder, UiaWorkerRouting::from_config(config, base))
    }

    /// „wie UIA“ ohne Live-Auswahl reicht den UIA-Client unverändert durch;
    /// nach einem UIA-Providerwechsel zieht ein neuer Worker mit.
    #[test]
    fn follow_uia_workers_pick_up_a_uia_provider_switch() -> TestResult {
        let (recorder, routing) = routing(&ResolvedConfig::default());
        ask(routing.model_for_role("uia-writer").as_ref())?;
        assert_eq!(last(&recorder), Some((None, None)));

        routing.set_live_uia(Some("openai".to_owned()), Some("gpt-5".to_owned()));
        ask(routing.model_for_role("uia-writer").as_ref())?;
        assert_eq!(
            last(&recorder),
            Some((Some("openai".to_owned()), Some("gpt-5".to_owned())))
        );
        Ok(())
    }

    /// Ein laufender Worker behält sein Modell; erst ein neuer Worker nimmt
    /// die neue UIA-Wahl.
    #[test]
    fn running_workers_keep_their_model_after_a_switch() -> TestResult {
        let (recorder, routing) = routing(&ResolvedConfig::default());
        routing.set_live_uia(
            Some("anthropic".to_owned()),
            Some("claude-opus-5-5".to_owned()),
        );
        let running = routing.model_for_role("uia-worker");
        routing.set_live_uia(Some("openai".to_owned()), Some("gpt-5".to_owned()));

        ask(running.as_ref())?;
        assert_eq!(
            last(&recorder),
            Some((
                Some("anthropic".to_owned()),
                Some("claude-opus-5-5".to_owned())
            ))
        );
        ask(routing.model_for_role("uia-worker").as_ref())?;
        assert_eq!(
            last(&recorder),
            Some((Some("openai".to_owned()), Some("gpt-5".to_owned())))
        );
        Ok(())
    }

    /// Eine feste Wahl bleibt beim UIA-Providerwechsel erhalten und scheitert
    /// nicht an ihm; „alle wie UIA“ löst sie wieder.
    #[test]
    fn fixed_choice_survives_a_uia_switch_until_reset() -> TestResult {
        let (recorder, routing) = routing(&ResolvedConfig::default());
        routing.set_choice(
            "uia-explorer",
            UiaWorkerModelChoice::Fixed {
                provider: "openrouter".to_owned(),
                model: "nvidia/nemotron-x".to_owned(),
            },
        );
        routing.set_live_uia(Some("openai".to_owned()), Some("gpt-5".to_owned()));
        ask(routing.model_for_role("uia-explorer").as_ref())?;
        assert_eq!(
            last(&recorder),
            Some((
                Some("openrouter".to_owned()),
                Some("nvidia/nemotron-x".to_owned())
            ))
        );

        routing.follow_uia_everywhere();
        ask(routing.model_for_role("uia-explorer").as_ref())?;
        assert_eq!(
            last(&recorder),
            Some((Some("openai".to_owned()), Some("gpt-5".to_owned())))
        );
        Ok(())
    }

    /// Eine Live-Auswahl ohne Provider bekommt den Provider aus dem Katalog.
    #[test]
    fn live_model_without_provider_uses_the_catalog_provider() -> TestResult {
        let mut config = ResolvedConfig::default();
        config.models.insert(
            "gpt-5".to_owned(),
            toml::from_str("id = \"gpt-5\"\nprovider = \"openai\"\n")
                .map_err(|error| TestError::Unexpected(error.to_string()))?,
        );
        let (_recorder, routing) = routing(&config);
        routing.set_live_uia(None, Some("gpt-5".to_owned()));
        assert_eq!(
            routing.route_for_role("uia-shell-worker"),
            Some((Some("openai".to_owned()), "gpt-5".to_owned()))
        );
        Ok(())
    }
}
