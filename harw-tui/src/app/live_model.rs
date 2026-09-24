//! Live-Modellwechsel in der TUI.
//!
//! # Beschreibung
//! - [`sync_live_main`] meldet an jeder Turn-Grenze die generische Auswahl
//!   des Controllers (`/model switch`) an die Montage
//!   ([`harw_runtime::live_model::LiveModelRouting::set_live_main`]): neu
//!   gestartete Kinder des Wurzel-Baums (Orchestrator, Worker, …) nehmen
//!   sie, laufende behalten ihr Modell. Vorher bekamen sie bis zum
//!   Neustart das Vorgabemodell des Starts.
//! - [`resolved_root_route`] liefert Provider und Modell, die die
//!   Wurzelsitzung tatsächlich anspricht, als aufgelöste Kennungen für die
//!   Statuszeile (`<provider>/<modell>`, nie ein Alias wie `default`).
//! - [`route_label`] formatiert ein solches Paar einheitlich für Statuszeile,
//!   Agentenliste und Meldungen.

use harw_operations::SessionController as _;

use super::ChatApp;

/// Meldet die generische Controller-Auswahl an die Live-Modellwahl der
/// Montage (nur an der Turn-Grenze aufrufen).
pub(super) fn sync_live_main(app: &ChatApp) {
    let Some(runtime) = app.runtime() else {
        return;
    };
    let snap = app.session_controller.snapshot();
    runtime
        .live_models()
        .set_live_main(snap.active_provider, snap.active_model);
}

/// Provider und Modell, die die Wurzelsitzung jetzt (bzw. ab dem nächsten
/// Turn) anspricht.
///
/// # Beschreibung
/// Dieselbe Rangfolge wie `TuiSessionController::apply_to_session`: bei
/// einer UIA-Wurzel gilt eine gesetzte UIA-Auswahl als Ganzes, sonst die
/// generische Auswahl. Ohne eigene Wahl löst die Montage das effektive
/// Wurzelmodell des Starts samt Provider auf
/// ([`harw_runtime::RuntimeAssembly::resolved_root_route`]). Ohne Montage
/// (Tests) bleibt es beim Controller-Stand; ein Alias wie `default` wird nie
/// als Modell gemeldet.
pub(super) fn resolved_root_route(app: &ChatApp) -> (Option<String>, Option<String>) {
    let snap = app.session_controller.snapshot();
    let Some(runtime) = app.runtime() else {
        let model = snap
            .active_model
            .clone()
            .or_else(|| app.export_session_model.clone())
            .filter(|model| model != "default");
        return (snap.active_provider, model);
    };
    let uia = &snap.uia_selection;
    let (provider, model) =
        if runtime.root_is_uia() && (uia.model().is_some() || uia.provider().is_some()) {
            (uia.provider(), uia.model())
        } else {
            (
                snap.active_provider.as_deref(),
                snap.active_model.as_deref(),
            )
        };
    runtime.resolved_root_route(provider, model)
}

/// Einheitliche Anzeige eines Provider/Modell-Paars: `provider/modell`, ohne
/// Provider nur das Modell, ohne Modell nur der Provider.
#[must_use]
pub(crate) fn route_label(provider: Option<&str>, model: Option<&str>) -> Option<String> {
    let provider = provider.filter(|provider| !provider.trim().is_empty());
    let model = model.filter(|model| !model.trim().is_empty());
    match (provider, model) {
        (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
        (None, Some(model)) => Some(model.to_owned()),
        (Some(provider), None) => Some(provider.to_owned()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_chat_app;
    use super::{resolved_root_route, route_label};
    use crate::test_support::{TestResult, ctx};
    use harw_operations::SessionController as _;

    #[test]
    fn route_label_joins_provider_and_model() {
        assert_eq!(
            route_label(Some("anthropic"), Some("claude-opus-5-5")).as_deref(),
            Some("anthropic/claude-opus-5-5")
        );
        assert_eq!(
            route_label(Some("openrouter"), Some("nvidia/nemotron-x")).as_deref(),
            Some("openrouter/nvidia/nemotron-x")
        );
        assert_eq!(route_label(None, Some("m")).as_deref(), Some("m"));
        assert_eq!(route_label(Some("p"), Some(" ")).as_deref(), Some("p"));
        assert_eq!(route_label(None, None), None);
    }

    /// Ohne Montage zeigt die Statuszeile den Controller-Stand, nie das
    /// Platzhalter-Modell `default` aus `SessionConfigured`.
    #[test]
    fn root_route_without_runtime_never_reports_the_default_alias() -> TestResult {
        let mut app = test_chat_app()?;
        app.export_session_model = Some("default".to_owned());
        assert_eq!(resolved_root_route(&app), (None, None));

        app.session_controller
            .set_active_provider("openai".to_owned())
            .map_err(ctx("set_active_provider"))?;
        app.session_controller
            .set_active_model("gpt-5".to_owned())
            .map_err(ctx("set_active_model"))?;
        assert_eq!(
            resolved_root_route(&app),
            (Some("openai".to_owned()), Some("gpt-5".to_owned()))
        );
        Ok(())
    }
}
