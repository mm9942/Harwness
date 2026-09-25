//! Ansichten zeigen nach einer Änderung sofort den Live-Stand.
//!
//! Jeder Test ändert einen Wert über denselben Weg wie ein Nutzer (Picker →
//! synthetisierte Slash-Zeile → echte Operation auf der echten Montage),
//! öffnet das Element danach neu und prüft, dass es den neuen Wert zeigt
//! bzw. vorauswählt. Geschrieben wird nie in eine echte `config.toml`: die
//! Operationen bekommen eine aufzeichnende Persistenz, der Live-Stand der
//! Montage wird darüber gespiegelt (`harw_ops::live_config`).

use std::sync::Arc;

use harw_ops::model::{RecordingSelectionPersistence, SelectionPersistence};
use harw_tools::serde_json::{self, Value, json};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::*;
use crate::events::harw_event_channel;
use crate::runtime_root::tests::{RuntimeBackedApp, runtime_backed_app};
use crate::test_support::{TestError, TestResult, ctx};

/// Ein aktivierter Provider mit Klartext-Schlüssel (für `/model switch`)
/// und zwei Katalogmodelle, direkt in den Live-Stand der Montage gelegt.
fn seed_catalog(env: &RuntimeBackedApp) -> TestResult {
    let provider: harw_config::ProviderToml = serde_json::from_value(json!({
        "name": "provider-a",
        "api": "openai-chat",
        "base_url": "https://provider-a.example.test",
        "api_key": "test-key",
        "enabled": true,
    }))
    .map_err(ctx("provider fixture"))?;
    let model_a: harw_config::ModelToml = serde_json::from_value(json!({
        "id": "model-a-2026", "provider": "provider-a", "aliases": ["model-a"],
    }))
    .map_err(ctx("model-a fixture"))?;
    let model_b: harw_config::ModelToml = serde_json::from_value(json!({
        "id": "model-b-2026", "provider": "provider-a", "aliases": ["model-b"],
    }))
    .map_err(ctx("model-b fixture"))?;
    let live = env
        .assembly
        .services()
        .live_config()
        .ok_or(TestError::Missing("the assembly binds a live config"))?;
    live.update(|config| {
        config.providers.insert("provider-a".to_owned(), provider);
        config.models.insert("model-a".to_owned(), model_a);
        config.models.insert("model-b".to_owned(), model_b);
    });
    Ok(())
}

/// Führt `raw` wie die TUI über die echte Slash-Fläche der Montage aus —
/// nur die Persistenz ist aufzeichnend statt dateibasiert.
async fn run_op(
    env: &RuntimeBackedApp,
    recorder: &Arc<RecordingSelectionPersistence>,
    raw: &str,
) -> TestResult<Option<Value>> {
    let rt = Arc::clone(&env.assembly);
    let persistence: Arc<dyn SelectionPersistence> = recorder.clone();
    let output = command_data::execute_command_with_data(
        env.app.adapters(),
        env.app.sandbox(),
        env.app.session_id(),
        runtime_commands::caller_tier(rt.principal()),
        raw,
        || {
            let mut services = runtime_commands::slash_service_map(rt.services());
            services.insert(persistence);
            services
        },
    )
    .await
    .map_err(|error| TestError::Unexpected(format!("{raw}: {error}")))?;
    Ok(output.data)
}

/// Drückt `code` im App-Zustand (wie der echte Tastaturpfad).
fn press(app: &mut ChatApp, code: KeyCode, bus: &HarwEventSender) {
    handle_key(app, KeyEvent::new(code, KeyModifiers::NONE), bus);
}

/// Die nächste synthetisierte Slash-Zeile.
fn next_command(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<HarwEvent>,
) -> TestResult<String> {
    match receiver.try_recv() {
        Ok(HarwEvent::Command(command)) => Ok(command),
        other => Err(TestError::Unexpected(format!(
            "expected a synthesized command, got {other:?}"
        ))),
    }
}

/// Text einer generischen Ansicht.
fn view_text(view: &dyn OverlayView) -> String {
    let area = Rect::new(0, 0, 160, 30);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf, crate::style::Theme::Dark);
    crate::overlay_view::buffer_text(&buf)
}

/// Text des ganzen Bildschirms (Statuszeile inklusive).
fn screen_text(app: &ChatApp) -> TestResult<String> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 24))
        .map_err(ctx("test terminal"))?;
    terminal
        .draw(|frame| render_viewport(frame, app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let buffer = terminal.backend().buffer();
    Ok((0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// F8 / Modell-Picker: eine über den Picker geänderte Rollenwahl steht beim
/// erneuten Öffnen in der Übersicht und ist im Picker vorausgewählt — auch
/// nach dem Nachladen über `/models show` und über das getippte `/models`.
#[tokio::test]
async fn role_model_change_is_shown_and_preselected_after_reopening() -> TestResult {
    let mut env = runtime_backed_app()?;
    seed_catalog(&env)?;
    let recorder = Arc::new(RecordingSelectionPersistence::new());
    let (bus, mut receiver) = harw_event_channel();
    let explorer = PickerTarget::Role {
        role: harw_config::ModelRole::Explorer,
    };

    // Wahl über den Picker: Provider bestätigen, zweites Modell wählen.
    env.app.open_model_switch_picker(explorer.clone());
    press(&mut env.app, KeyCode::Enter, &bus);
    press(&mut env.app, KeyCode::Down, &bus);
    press(&mut env.app, KeyCode::Enter, &bus);
    let command = next_command(&mut receiver)?;
    assert_eq!(command, "/models set explorer provider-a/model-b-2026");
    run_op(&env, &recorder, &command).await?;

    // F8 neu öffnen: die Übersicht zeigt die neue Wahl …
    let view = env.app.model_roles_view();
    let text = view_text(&view);
    assert!(text.contains("model-b-2026"), "{text}");

    // … auch nachdem `/models show` (ihr Nachladebefehl) die Daten liefert.
    let mut refreshed = env.app.model_roles_view();
    let data = run_op(&env, &recorder, crate::model_roles_view::REFRESH_COMMAND)
        .await?
        .ok_or(TestError::Missing("/models show data"))?;
    refreshed.apply_data(&data);
    let text = view_text(&refreshed);
    assert!(text.contains("model-b-2026"), "{text}");

    // Das getippte `/models` öffnet dieselbe Live-Übersicht.
    let intercepted = local_intercept_for(&env.app, "/models")
        .ok_or(TestError::Missing("/models is intercepted locally"))?;
    apply_local_intercept(&mut env.app, intercepted, &bus);
    match env.app.overlay.as_ref() {
        Some(Overlay::View(view)) => {
            let text = view_text(view.as_ref());
            assert!(text.contains("model-b-2026"), "{text}");
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "/models must open the overview, got {other:?}"
            )));
        }
    }
    env.app.overlay = None;

    // Picker neu öffnen: zweimal Enter übernimmt die Vorauswahl — die neue Wahl.
    env.app.open_model_switch_picker(explorer);
    press(&mut env.app, KeyCode::Enter, &bus);
    press(&mut env.app, KeyCode::Enter, &bus);
    assert_eq!(
        next_command(&mut receiver)?,
        "/models set explorer provider-a/model-b-2026",
        "the reopened picker must preselect the current choice"
    );
    Ok(())
}

/// Statuszeile und Live-Zeile von F8: ein Wechsel des Wurzelmodells ist
/// sofort sichtbar; `Enter` auf der Live-Zeile öffnet den Picker, der die
/// Wurzel wirklich wechselt.
#[tokio::test]
async fn root_model_switch_updates_status_line_and_live_row() -> TestResult {
    let env = runtime_backed_app()?;
    seed_catalog(&env)?;
    let recorder = Arc::new(RecordingSelectionPersistence::new());

    run_op(&env, &recorder, "/model switch model-b-2026").await?;
    let screen = screen_text(&env.app)?;
    assert!(screen.contains("provider-a/model-b-2026"), "{screen}");

    let mut view = env.app.model_roles_view();
    // Nachladen darf die aufgelöste Live-Zeile nicht zurücksetzen.
    let data = run_op(&env, &recorder, crate::model_roles_view::REFRESH_COMMAND)
        .await?
        .ok_or(TestError::Missing("/models show data"))?;
    view.apply_data(&data);
    let text = view_text(&view);
    assert!(text.contains("model-b-2026"), "{text}");

    let expected = if env.assembly.root_is_uia() {
        "/uia-model"
    } else {
        "/model"
    };
    assert_eq!(
        view.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        OverlayOutcome::RunAndClose(expected.to_owned())
    );

    if env.assembly.root_is_uia() {
        // Bei einer UIA-Wurzel wechselt `/uia-model` die Wurzel.
        run_op(&env, &recorder, "/uia-model switch model-a-2026").await?;
        let screen = screen_text(&env.app)?;
        assert!(screen.contains("provider-a/model-a-2026"), "{screen}");
        let text = view_text(&env.app.model_roles_view());
        assert!(text.contains("model-a-2026"), "{text}");
    }
    Ok(())
}

/// F7: `/mode default work` ist beim erneuten Öffnen der Modus-Auswahl als
/// Standard sichtbar (vorher: Stand des Starts bis zum Neustart).
#[tokio::test]
async fn mode_default_change_is_shown_after_reopening_the_mode_picker() -> TestResult {
    let env = runtime_backed_app()?;
    let recorder = Arc::new(RecordingSelectionPersistence::new());
    let before = view_text(&env.app.mode_picker_view());
    assert!(!before.contains("Standard: work"), "{before}");

    run_op(&env, &recorder, "/mode default work").await?;

    let after = view_text(&env.app.mode_picker_view());
    assert!(after.contains("Standard: work"), "{after}");
    Ok(())
}

/// UIA-Effort-Auswahl: der gespeicherte Wert ist beim erneuten Öffnen
/// vorausgewählt.
#[tokio::test]
async fn uia_effort_change_is_preselected_after_reopening() -> TestResult {
    let mut env = runtime_backed_app()?;
    let recorder = Arc::new(RecordingSelectionPersistence::new());
    let (bus, mut receiver) = harw_event_channel();

    run_op(&env, &recorder, "/uia-effort low").await?;

    env.app.open_effort_choice(EffortTarget::Uia);
    press(&mut env.app, KeyCode::Enter, &bus);
    assert_eq!(next_command(&mut receiver)?, "/uia-effort low");
    Ok(())
}

/// Bereich „UIA-Worker-Modelle“ (`/models worker`): eine gespeicherte
/// Worker-Wahl steht beim erneuten Öffnen in der Tabelle.
#[tokio::test]
async fn uia_worker_model_change_is_shown_after_reopening() -> TestResult {
    let mut env = runtime_backed_app()?;
    seed_catalog(&env)?;
    let recorder = Arc::new(RecordingSelectionPersistence::new());

    run_op(
        &env,
        &recorder,
        "/models worker uia-writer provider-a/model-b-2026",
    )
    .await?;

    env.app.open_uia_worker_models_view();
    match env.app.overlay.as_ref() {
        Some(Overlay::View(view)) => {
            let text = view_text(view.as_ref());
            assert!(text.contains("model-b-2026"), "{text}");
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "expected the UIA worker overview, got {other:?}"
            )));
        }
    }
    Ok(())
}
