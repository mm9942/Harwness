//! Live-Modellwechsel in der TUI.
//!
//! # Beschreibung
//! - [`sync_live_main`] meldet an jeder Turn-Grenze die generische Auswahl
//!   des Controllers (`/model switch`) an die Montage
//!   ([`harw_runtime::live_model::LiveModelRouting::set_live_main`]): neu
//!   gestartete Kinder des Wurzel-Baums (Orchestrator, Worker, …) nehmen
//!   sie, laufende behalten ihr Modell. Vorher bekamen sie bis zum
//!   Neustart das Vorgabemodell des Starts.
//! - [`check_pending_route`] und [`report_rejected_route`] schützen die
//!   Wurzelsitzung an derselben Turn-Grenze: die Route des Controllers wird
//!   gegen den Provider-Lebenszyklus geprüft, **bevor** sie in die Sitzung
//!   geschrieben wird; eine abgeschaltete Route wird verworfen und gemeldet.
//! - [`resolved_root_route`] liefert Provider und Modell, die die
//!   Wurzelsitzung tatsächlich anspricht, als aufgelöste Kennungen für die
//!   Statuszeile (`<provider>/<modell>`, nie ein Alias wie `default`).
//! - [`route_label`] formatiert ein solches Paar einheitlich für Statuszeile,
//!   Agentenliste und Meldungen.

use harw_operations::SessionController as _;

use crate::session_controller::RejectedRoute;

use super::{ChatApp, Role};

/// Prüft die Route, die der Controller an der Turn-Grenze in die
/// Wurzelsitzung schreiben will, gegen den Provider-Lebenszyklus.
///
/// # Beschreibung
/// Wird von `ChatApp::apply_pending_controller_state` **vor** dem Schreiben
/// aufgerufen. Provider und Modell werden wie bei der Live-Wahl aufgelöst
/// ([`harw_runtime::live_model::check_explicit_selection`]: genannter
/// Provider, sonst Katalog, sonst Vorgabe-Provider; Alias → Modell-ID) gegen
/// die Live-Konfiguration der Montage; ohne Montage (Tests) gegen eine leere
/// Konfiguration, die nur ausdrücklich genannte Provider prüft. Ohne Modell
/// gibt es nichts zu prüfen. Ein abgekündigtes (deprecated) Modell gilt und
/// wird nur protokolliert.
///
/// # Rückgabe
/// `Ok(())`, wenn die Route übernommen werden darf; `Err(grund)` mit dem
/// Text der Ablehnung (inklusive vom Provider empfohlenem Ersatz), wenn das
/// Modell abgeschaltet ist.
pub(super) fn check_pending_route(
    app: &ChatApp,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(), String> {
    let Some(model) = model.filter(|model| !model.trim().is_empty()) else {
        return Ok(());
    };
    let config = app.resolved_config().unwrap_or_default();
    match harw_runtime::live_model::check_explicit_selection(&config, provider, model) {
        Ok(Some(warning)) => {
            tracing::warn!(%warning, "tui.live_model.deprecated_route");
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// Meldet eine von [`check_pending_route`] abgelehnte Route: Protokoll plus
/// sichtbare Systemzeile im Verlauf.
///
/// # Beschreibung
/// Der Controller wurde bereits auf die Route der Sitzung zurückgesetzt
/// ([`crate::session_controller::TuiSessionController::apply_to_session_checked`]);
/// die Zeile sagt, dass die Sitzung beim bisherigen Modell bleibt, und nennt
/// den Grund samt empfohlenem Ersatz.
pub(super) fn report_rejected_route(app: &mut ChatApp, rejected: &RejectedRoute) {
    tracing::warn!(
        reason = %rejected.reason,
        provider = ?rejected.provider,
        model = ?rejected.model,
        "tui.live_model.route_refused"
    );
    let label = route_label(rejected.provider.as_deref(), rejected.model.as_deref())
        .unwrap_or_else(|| "(keine Angabe)".to_owned());
    app.push_line(
        Role::System,
        format!(
            "Modellwechsel auf {label} abgelehnt: {reason}. \
             Die Sitzung bleibt beim bisherigen Modell.",
            reason = rejected.reason
        ),
    );
}

/// Meldet die generische Controller-Auswahl an die Live-Modellwahl der
/// Montage (nur an der Turn-Grenze aufrufen).
///
/// # Beschreibung
/// Wählt der Controller ein abgeschaltetes (retired) Modell, lehnt die
/// Montage die Wahl ab (fail-closed): neu gestartete Kinder behalten dann
/// die bisherige Live-Wahl bzw. den Stand des Starts. Die Wurzelsitzung ist
/// an derselben Stelle schon durch [`check_pending_route`] geschützt (die
/// Prüfung läuft vor dem Schreiben in die Sitzung und setzt den Controller
/// zurück); diese Ablehnung sichert nur noch die Kind-Route. Sie wird nur
/// protokolliert — die Sitzungszeilen gehören der Wechsel-Operation bzw.
/// [`report_rejected_route`], und diese Funktion läuft an jeder Turn-Grenze,
/// würde also dieselbe Zeile wiederholen.
pub(super) fn sync_live_main(app: &ChatApp) {
    let Some(runtime) = app.runtime() else {
        return;
    };
    let snap = app.session_controller.snapshot();
    if let Err(error) = runtime
        .live_models()
        .set_live_main(snap.active_provider, snap.active_model)
    {
        tracing::warn!(%error, "tui.live_model.sync_refused");
    }
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
    use super::super::ChatApp;
    use super::super::tests::test_chat_app;
    use super::{resolved_root_route, route_label};
    use crate::export::ExportEntry;
    use crate::runtime_root::tests::runtime_backed_app;
    use crate::test_support::{TestResult, ctx};
    use harw_core::AgentSession;
    use harw_extension_api::ExtensionRegistryBuilder;
    use harw_operations::SessionController as _;
    use harw_types::{AgentRole, ModelId, ProviderId};

    /// Ein in der eingebetteten Lebenszyklus-Tabelle abgeschaltetes Angebot
    /// (Mistral OCR 4.0, retired 2026-09-30 — für die echte Uhr sicher in der
    /// Vergangenheit; Ersatz `mistral-ocr-4-1`).
    const RETIRED_PROVIDER: &str = "mistral";
    const RETIRED_MODEL: &str = "mistral-ocr-4-0";

    /// Wurzelsitzung ohne Spawn-Kontext (generische Route).
    fn bare_session() -> AgentSession {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        AgentSession::new(
            AgentRole::Assistant,
            None,
            ExtensionRegistryBuilder::default().build(),
            event_tx,
        )
    }

    /// Alle Systemzeilen des Verlaufs (sichtbare Meldungen der App).
    fn system_notices(app: &ChatApp) -> Vec<String> {
        app.export_entries
            .iter()
            .filter_map(|entry| match entry {
                ExportEntry::System(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Setzt Provider und Modell direkt im Controller (an der Operation
    /// vorbei, wie ein fremder Schreiber).
    fn select(app: &ChatApp, provider: &str, model: &str) -> TestResult {
        app.session_controller
            .set_active_provider(provider.to_owned())
            .map_err(ctx("set_active_provider"))?;
        app.session_controller
            .set_active_model(model.to_owned())
            .map_err(ctx("set_active_model"))?;
        Ok(())
    }

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

    /// Erreicht eine abgeschaltete Route den Controller an der Operation
    /// vorbei, schreibt die Turn-Grenze sie weder in die Wurzelsitzung noch in
    /// die Kind-Route: Sitzung und `live_main()` bleiben unverändert, der
    /// Controller wird auf die Sitzungsroute zurückgesetzt, eine Systemzeile
    /// nennt Grund und Ersatz — und an der nächsten Grenze kommt sie nicht
    /// noch einmal.
    #[test]
    fn retired_controller_route_never_reaches_session_or_child_route() -> TestResult {
        let mut env = runtime_backed_app()?;
        let mut session = bare_session();

        // Zulässige Wahl: Sitzung und Kind-Route übernehmen sie.
        select(&env.app, "openai", "gpt-5")?;
        assert!(env.app.apply_pending_controller_state(&mut session));
        assert_eq!(session.active_model(), Some(&ModelId::from("gpt-5")));
        let live_before = env.assembly.live_models().live_main();
        assert_eq!(
            live_before,
            Some((Some("openai".to_owned()), "gpt-5".to_owned()))
        );

        // Abgeschaltete Wahl im Controller: nicht angewendet.
        select(&env.app, RETIRED_PROVIDER, RETIRED_MODEL)?;
        assert!(
            !env.app.apply_pending_controller_state(&mut session),
            "allein eine abgelehnte Route verändert die Sitzung nicht"
        );
        assert_eq!(session.active_model(), Some(&ModelId::from("gpt-5")));
        assert_eq!(session.active_provider(), Some(&ProviderId::from("openai")));
        assert_eq!(env.assembly.live_models().live_main(), live_before);
        let snap = env.app.session_controller.snapshot();
        assert_eq!(snap.active_model.as_deref(), Some("gpt-5"));
        assert_eq!(snap.active_provider.as_deref(), Some("openai"));
        let notices = system_notices(&env.app);
        assert!(
            notices
                .iter()
                .any(|text| text.contains("abgelehnt") && text.contains("mistral-ocr-4-1")),
            "die Ablehnung ist sichtbar und nennt den Ersatz: {notices:?}"
        );

        // Nächste Turn-Grenze: nichts mehr ausstehend, keine zweite Meldung.
        assert!(!env.app.apply_pending_controller_state(&mut session));
        assert_eq!(system_notices(&env.app).len(), notices.len());
        Ok(())
    }

    /// Auch ohne Montage (Tests, Einbettung) wird eine ausdrücklich genannte
    /// abgeschaltete Route nicht in die Sitzung geschrieben.
    #[test]
    fn retired_route_is_refused_without_a_runtime_too() -> TestResult {
        let mut app = test_chat_app()?;
        let mut session = bare_session();
        let notices_before = system_notices(&app).len();
        select(&app, RETIRED_PROVIDER, RETIRED_MODEL)?;

        assert!(!app.apply_pending_controller_state(&mut session));
        assert!(session.active_model().is_none());
        assert!(session.active_provider().is_none());
        assert!(app.session_controller.snapshot().active_model.is_none());
        assert_eq!(system_notices(&app).len(), notices_before + 1);
        Ok(())
    }

    /// Ein abgekündigtes (deprecated) Modell wird übernommen, ohne Ablehnung.
    #[test]
    fn deprecated_route_is_applied_without_a_notice() -> TestResult {
        let mut app = test_chat_app()?;
        let mut session = bare_session();
        let notices_before = system_notices(&app).len();
        select(&app, "anthropic", "claude-mythos-preview")?;

        assert!(app.apply_pending_controller_state(&mut session));
        assert_eq!(
            session.active_model(),
            Some(&ModelId::from("claude-mythos-preview"))
        );
        assert_eq!(
            session.active_provider(),
            Some(&ProviderId::from("anthropic"))
        );
        assert_eq!(system_notices(&app).len(), notices_before);
        Ok(())
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
