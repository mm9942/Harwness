//! Live-Stand der aufgelösten Konfiguration einer laufenden Montage.
//!
//! # Ursache des Fehlers „nach einer Änderung zeigt die Ansicht den alten
//! Wert“
//! Die Montage legte beim Start **einen** `Arc<ResolvedConfig>` in jede
//! `ServiceMap`. Operationen wie `/models set`, `/model switch`,
//! `/uia-model switch`, `/uia-effort`, `/mode default` oder
//! `/skills activate` schreiben ihre Wahl zwar in die Profil-`config.toml`,
//! der Schnappschuss im Speicher blieb aber beim Stand des Starts. Alles,
//! was danach aus ihm las — `/models show`, `/mode show`, `/status`, die
//! Vorauswahl der Picker und die Übersichten der TUI (F7, F8) — zeigte bis
//! zum Neustart die alten Werte.
//!
//! # Neue Regel
//! [`LiveConfig`] hält den aktuellen Schnappschuss. Die Montage legt bei
//! jedem Bau einer `ServiceMap` [`LiveConfig::current`] ab (statt des
//! Start-Standes) und registriert die Zelle selbst als
//! [`SharedLiveConfig`]. Jede gelungene Persistenz über
//! [`crate::config_util::SelectionPersistence`] wird danach im Speicher
//! gespiegelt ([`MirroringSelectionPersistence`], von
//! [`crate::config_util::selection_persistence`] automatisch vorgeschaltet),
//! ebenso der Skill-Schalter von `/skills activate|deactivate`. So sehen
//! alle Ansichten denselben Stand wie die Datei.
//!
//! Gespiegelt wird nur, was auch geschrieben wurde: schlägt die Persistenz
//! fehl (`Some(hinweis)`), bleibt der Schnappschuss unverändert.
//!
//! # Nebenläufigkeit
//! `RwLock` um einen `Arc`; ein Schreiber ersetzt den `Arc` (Copy-on-Write),
//! Leser behalten ihren alten Schnappschuss. Eine vergiftete Sperre wird
//! übernommen, nie in eine Panik verwandelt.

use std::sync::{Arc, PoisonError, RwLock};

use harw_config::{InternalModelChoice, InternalModelPoint, ResolvedConfig};
use harw_operations::OpContext;

use crate::config_util::SelectionPersistence;

/// Veränderlicher, geteilter Stand der aufgelösten Konfiguration.
#[derive(Debug)]
pub struct LiveConfig {
    current: RwLock<Arc<ResolvedConfig>>,
}

/// Geteilter Handle, unter dem die Montage die Zelle registriert.
pub type SharedLiveConfig = Arc<LiveConfig>;

impl LiveConfig {
    /// Startet mit dem Stand des Starts.
    #[must_use]
    pub fn new(config: Arc<ResolvedConfig>) -> Self {
        Self {
            current: RwLock::new(config),
        }
    }

    /// Der aktuelle Schnappschuss.
    #[must_use]
    pub fn current(&self) -> Arc<ResolvedConfig> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Wendet `edit` auf eine Kopie des aktuellen Stands an und macht sie
    /// zum neuen Schnappschuss.
    pub fn update(&self, edit: impl FnOnce(&mut ResolvedConfig)) {
        let mut guard = self.current.write().unwrap_or_else(PoisonError::into_inner);
        let mut next = (**guard).clone();
        edit(&mut next);
        *guard = Arc::new(next);
    }
}

/// Spiegelt `edit` in die registrierte [`SharedLiveConfig`] (ohne Dienst:
/// keine Wirkung).
pub(crate) fn mirror(ctx: &OpContext, edit: impl FnOnce(&mut ResolvedConfig)) {
    if let Some(live) = ctx.service::<SharedLiveConfig>() {
        live.update(edit);
    }
}

/// Setzt `slot` auf `value`, wenn `value` gesetzt ist („`None` = unverändert
/// lassen“, wie [`crate::config_util::persist_default_selection`]).
fn set_if_some(slot: &mut Option<String>, value: Option<&str>) {
    if let Some(value) = value {
        *slot = Some(value.to_owned());
    }
}

/// Schaltet vor einen [`SelectionPersistence`]-Dienst und spiegelt jede
/// gelungene Persistenz in eine [`LiveConfig`].
///
/// # Beschreibung
/// Je Methode dieselbe Semantik wie der Schreibkern in
/// [`crate::config_util`]: was dort in die Datei geht, geht hier in den
/// Schnappschuss. Ein Hinweis (`Some`) des inneren Dienstes heißt
/// „nicht gespeichert“ — dann bleibt der Schnappschuss unverändert.
pub struct MirroringSelectionPersistence {
    inner: Arc<dyn SelectionPersistence>,
    live: SharedLiveConfig,
}

impl MirroringSelectionPersistence {
    /// Baut den Spiegel um `inner`.
    #[must_use]
    pub fn new(inner: Arc<dyn SelectionPersistence>, live: SharedLiveConfig) -> Self {
        Self { inner, live }
    }

    /// Spiegelt `edit`, falls `note` keinen Fehler meldet; reicht `note`
    /// unverändert durch.
    fn on_success(
        &self,
        note: Option<String>,
        edit: impl FnOnce(&mut ResolvedConfig),
    ) -> Option<String> {
        if note.is_none() {
            self.live.update(edit);
        }
        note
    }
}

impl SelectionPersistence for MirroringSelectionPersistence {
    fn persist_default_selection(
        &self,
        default_provider: Option<&str>,
        default_model: Option<&str>,
    ) -> Option<String> {
        let note = self
            .inner
            .persist_default_selection(default_provider, default_model);
        self.on_success(note, |config| {
            set_if_some(&mut config.harness.default_provider, default_provider);
            set_if_some(&mut config.harness.default_model, default_model);
        })
    }

    fn persist_uia_selection(
        &self,
        uia_provider: Option<&str>,
        uia_model: Option<&str>,
    ) -> Option<String> {
        let note = self.inner.persist_uia_selection(uia_provider, uia_model);
        if uia_provider.is_none() && uia_model.is_none() {
            // Der Schreibkern tut in diesem Fall nichts.
            return note;
        }
        self.on_success(note, |config| {
            set_if_some(&mut config.harness.uia_provider, uia_provider);
            // `None` entfernt einen bestehenden UIA-Modell-Pin.
            config.harness.uia_model = uia_model.map(str::to_owned);
        })
    }

    fn persist_uia_worker_model(&self, model: Option<&str>) -> Option<String> {
        let note = self.inner.persist_uia_worker_model(model);
        self.on_success(note, |config| {
            config.harness.uia_worker_model = model.map(str::to_owned);
        })
    }

    fn persist_uia_reasoning_effort(&self, effort: Option<&str>) -> Option<String> {
        let note = self.inner.persist_uia_reasoning_effort(effort);
        self.on_success(note, |config| {
            config.harness.reasoning.uia = effort.map(str::to_owned);
        })
    }

    fn persist_role_reasoning_effort(
        &self,
        role_key: &str,
        effort: Option<&str>,
    ) -> Option<String> {
        let note = self.inner.persist_role_reasoning_effort(role_key, effort);
        self.on_success(note, |config| {
            let slot = match role_key {
                "uia" => &mut config.harness.reasoning.uia,
                "root-orchestrator" => &mut config.harness.reasoning.root_orchestrator,
                "sub-orchestrator" => &mut config.harness.reasoning.sub_orchestrator,
                "worker-simple" => &mut config.harness.reasoning.worker_simple,
                "worker-complex" => &mut config.harness.reasoning.worker_complex,
                _ => return,
            };
            *slot = effort.map(str::to_owned);
        })
    }

    fn persist_internal_model(
        &self,
        point: InternalModelPoint,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Option<String> {
        let note = self.inner.persist_internal_model(point, provider, model);
        self.on_success(note, |config| {
            let choice = (provider.is_some() || model.is_some()).then(|| InternalModelChoice {
                provider: provider.map(str::to_owned),
                model: model.map(str::to_owned),
            });
            config.harness.internal_models.set_choice(point, choice);
        })
    }

    fn clear_uia_selection(&self) -> Option<String> {
        let note = self.inner.clear_uia_selection();
        self.on_success(note, |config| {
            config.harness.uia_provider = None;
            config.harness.uia_model = None;
        })
    }

    fn persist_active_agent(&self, name: Option<&str>) -> Option<String> {
        let note = self.inner.persist_active_agent(name);
        self.on_success(note, |config| {
            config.harness.active_agent_definition = name.map(str::to_owned);
        })
    }

    fn persist_uia_worker_role_model(&self, role: &str, value: Option<&str>) -> Option<String> {
        let note = self.inner.persist_uia_worker_role_model(role, value);
        self.on_success(note, |config| {
            config
                .harness
                .uia_worker_models
                .set(role, value.map(str::to_owned));
        })
    }

    fn persist_default_interaction_mode(&self, mode: &str) -> Option<String> {
        let note = self.inner.persist_default_interaction_mode(mode);
        self.on_success(note, |config| {
            mode.clone_into(&mut config.harness.mode.default);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{LiveConfig, MirroringSelectionPersistence, SharedLiveConfig};
    use crate::config_util::{RecordingSelectionPersistence, SelectionPersistence};
    use harw_config::{InternalModelPoint, ResolvedConfig};
    use std::sync::Arc;

    /// Innerer Dienst, der jede Persistenz als fehlgeschlagen meldet.
    struct FailingPersistence;

    impl SelectionPersistence for FailingPersistence {
        fn persist_default_selection(&self, _: Option<&str>, _: Option<&str>) -> Option<String> {
            Some("kaputt".to_owned())
        }
        fn persist_uia_selection(&self, _: Option<&str>, _: Option<&str>) -> Option<String> {
            Some("kaputt".to_owned())
        }
        fn persist_uia_worker_model(&self, _: Option<&str>) -> Option<String> {
            Some("kaputt".to_owned())
        }
        fn persist_uia_reasoning_effort(&self, _: Option<&str>) -> Option<String> {
            Some("kaputt".to_owned())
        }
        fn persist_role_reasoning_effort(&self, _: &str, _: Option<&str>) -> Option<String> {
            Some("kaputt".to_owned())
        }
        fn persist_internal_model(
            &self,
            _: InternalModelPoint,
            _: Option<&str>,
            _: Option<&str>,
        ) -> Option<String> {
            Some("kaputt".to_owned())
        }
        fn clear_uia_selection(&self) -> Option<String> {
            Some("kaputt".to_owned())
        }
        fn persist_active_agent(&self, _: Option<&str>) -> Option<String> {
            Some("kaputt".to_owned())
        }
        fn persist_uia_worker_role_model(&self, _: &str, _: Option<&str>) -> Option<String> {
            Some("kaputt".to_owned())
        }
        fn persist_default_interaction_mode(&self, _: &str) -> Option<String> {
            Some("kaputt".to_owned())
        }
    }

    fn mirrored() -> (SharedLiveConfig, MirroringSelectionPersistence) {
        let live = Arc::new(LiveConfig::new(Arc::new(ResolvedConfig::default())));
        let inner: Arc<dyn SelectionPersistence> = Arc::new(RecordingSelectionPersistence::new());
        let mirror = MirroringSelectionPersistence::new(inner, Arc::clone(&live));
        (live, mirror)
    }

    #[test]
    fn update_replaces_the_snapshot_but_keeps_old_readers_stable() {
        let live = LiveConfig::new(Arc::new(ResolvedConfig::default()));
        let before = live.current();
        live.update(|config| config.harness.default_model = Some("neu".to_owned()));
        assert_eq!(before.harness.default_model, None);
        assert_eq!(live.current().harness.default_model.as_deref(), Some("neu"));
    }

    #[test]
    fn every_selection_is_mirrored_after_successful_persistence() {
        let (live, mirror) = mirrored();
        assert_eq!(mirror.persist_default_selection(Some("p"), Some("m")), None);
        assert_eq!(mirror.persist_uia_selection(Some("up"), Some("um")), None);
        assert_eq!(mirror.persist_uia_worker_model(Some("wm")), None);
        assert_eq!(mirror.persist_uia_reasoning_effort(Some("high")), None);
        assert_eq!(
            mirror.persist_internal_model(InternalModelPoint::Explorer, Some("ep"), Some("em")),
            None
        );
        assert_eq!(
            mirror.persist_uia_worker_role_model("uia-writer", Some("wp/wmod")),
            None
        );
        assert_eq!(mirror.persist_active_agent(Some("agent-x")), None);
        assert_eq!(mirror.persist_default_interaction_mode("work"), None);

        let config = live.current();
        let harness = &config.harness;
        assert_eq!(harness.default_provider.as_deref(), Some("p"));
        assert_eq!(harness.default_model.as_deref(), Some("m"));
        assert_eq!(harness.uia_provider.as_deref(), Some("up"));
        assert_eq!(harness.uia_model.as_deref(), Some("um"));
        assert_eq!(harness.uia_worker_model.as_deref(), Some("wm"));
        assert_eq!(harness.reasoning.uia.as_deref(), Some("high"));
        let explorer = harness
            .internal_models
            .choice(InternalModelPoint::Explorer)
            .cloned();
        assert_eq!(
            explorer.and_then(|choice| choice.model),
            Some("em".to_owned())
        );
        assert_eq!(harness.uia_worker_models.get("uia-writer"), Some("wp/wmod"));
        assert_eq!(harness.active_agent_definition.as_deref(), Some("agent-x"));
        assert_eq!(harness.mode.default, "work");
    }

    #[test]
    fn resets_are_mirrored_too() {
        let (live, mirror) = mirrored();
        mirror.persist_uia_selection(Some("up"), Some("um"));
        mirror.persist_internal_model(InternalModelPoint::Explorer, Some("ep"), Some("em"));
        assert_eq!(mirror.clear_uia_selection(), None);
        assert_eq!(
            mirror.persist_internal_model(InternalModelPoint::Explorer, None, None),
            None
        );
        let config = live.current();
        assert_eq!(config.harness.uia_provider, None);
        assert_eq!(config.harness.uia_model, None);
        assert!(
            config
                .harness
                .internal_models
                .choice(InternalModelPoint::Explorer)
                .is_none()
        );
    }

    #[test]
    fn failed_persistence_leaves_the_snapshot_untouched() {
        let live = Arc::new(LiveConfig::new(Arc::new(ResolvedConfig::default())));
        let inner: Arc<dyn SelectionPersistence> = Arc::new(FailingPersistence);
        let mirror = MirroringSelectionPersistence::new(inner, Arc::clone(&live));
        assert!(
            mirror
                .persist_default_selection(Some("p"), Some("m"))
                .is_some()
        );
        assert!(mirror.persist_default_interaction_mode("work").is_some());
        let config = live.current();
        assert_eq!(config.harness.default_model, None);
        assert_ne!(config.harness.mode.default, "work");
    }
}
