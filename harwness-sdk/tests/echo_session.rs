//! Ende-zu-Ende über die öffentliche Fläche: Offline-Echo, kein Netz.
//!
//! # Aufbau
//! Ein Tempverzeichnis je Test mit eigenem Root-Space (minimale, aktive UIA,
//! Layout wie `harw-runtime/tests/rights_matrix.rs`) und einem Projekt mit
//! `Cargo.toml`-Marker. Der Verlauf ist flüchtig (`ephemeral`).

use std::path::{Path, PathBuf};

use harwness_sdk::prelude::*;
use harwness_sdk::serde_json::{Value, json};
use harwness_sdk::{FinishStatus, Role};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

fn fixture(with_uia: bool) -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&project)?;
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n")?;
    if with_uia {
        write_fixture_uia(&home)?;
    }
    Ok(Fixture {
        _dir: dir,
        home,
        project,
    })
}

/// Minimale, gültige UIA im Standardprofil, aktiviert über
/// `active_uia_definition`.
fn write_fixture_uia(home: &Path) -> TestResult {
    let profile_dir = home.join("profiles").join("default");
    let agent_dir = profile_dir.join("agents").join("fixture-uia");
    std::fs::create_dir_all(&agent_dir)?;
    std::fs::write(
        agent_dir.join("definition.toml"),
        "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
    )?;
    std::fs::write(
        profile_dir.join("config.toml"),
        "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
    )?;
    Ok(())
}

fn builder(fixture: &Fixture) -> HarwnessBuilder {
    Harwness::builder()
        .home(&fixture.home)
        .cwd(&fixture.project)
        .scaffold_home(false)
        .ephemeral(true)
        .offline_echo("pong")
}

#[tokio::test]
async fn a_turn_completes_and_streams_a_root_finish() -> TestResult {
    let fixture = fixture(true)?;
    let harwness = builder(&fixture).build()?;
    let mut session = harwness.session()?;

    let mut events = session.events();
    let report = session.send("ping").await?;

    assert_eq!(report.status, TurnStatus::Completed);
    assert_eq!(report.text.as_deref(), Some("pong"));
    assert_eq!(&report.session_id, session.id());
    assert_eq!(report.approvals, 0);

    // Alle Ereignisse des Turns liegen bereits im Puffer des Stroms.
    let mut saw_finish = false;
    while let Ok(Some(event)) =
        tokio::time::timeout(std::time::Duration::from_millis(200), events.next()).await
    {
        if let SdkEvent::Finished { status, source, .. } = &event {
            assert!(source.is_root());
            assert_eq!(*status, FinishStatus::Completed);
            saw_finish = true;
            break;
        }
    }
    assert!(saw_finish, "the root turn must emit Finished");
    Ok(())
}

#[tokio::test]
async fn a_dropped_session_can_be_resumed_by_id() -> TestResult {
    let fixture = fixture(true)?;
    let harwness = builder(&fixture).build()?;

    let id = {
        let mut session = harwness.session()?;
        session.send("merk dir das").await?;
        session.id().clone()
    };

    let resumed = harwness.resume(&id).await?;
    assert_eq!(resumed.id(), &id);
    let history = resumed.history();
    assert!(
        history
            .iter()
            .any(|message| message.role == Role::User && message.text == "merk dir das"),
        "the resumed history must contain the earlier user message: {history:?}"
    );
    Ok(())
}

#[tokio::test]
async fn resuming_an_unknown_session_fails_closed() -> TestResult {
    let fixture = fixture(true)?;
    let harwness = builder(&fixture).build()?;
    let unknown = SessionId::new("never-stored")?;
    let result = harwness.resume(&unknown).await;
    assert!(
        matches!(result, Err(SdkError::SessionNotFound { .. })),
        "{result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn an_empty_prompt_is_rejected_before_any_turn() -> TestResult {
    let fixture = fixture(true)?;
    let harwness = builder(&fixture).build()?;
    let mut session = harwness.session()?;
    let result = session.send("   ").await;
    assert!(matches!(
        result,
        Err(SdkError::InvalidInput { field: "text", .. })
    ));
    assert!(!session.cancel(), "no turn is running");
    Ok(())
}

#[test]
fn build_without_an_active_uia_is_a_config_error() -> TestResult {
    let fixture = fixture(false)?;
    let result = builder(&fixture).build();
    assert!(matches!(result, Err(SdkError::Config { .. })), "{result:?}");
    Ok(())
}

#[test]
fn custom_tools_are_listed_after_build() -> TestResult {
    let fixture = fixture(true)?;
    let harwness = builder(&fixture)
        .tool(FnTool::new(
            "host.echo",
            "Gibt die Argumente zurück.",
            json!({"type": "object"}),
            |arguments: Value| async move { Ok::<_, ToolError>(arguments) },
        ))
        .build()?;
    assert_eq!(harwness.tool_names(), ["host.echo".to_owned()]);
    // Die Montage einer Sitzung registriert das Werkzeug ohne Fehler.
    let _session = harwness.session()?;
    Ok(())
}
