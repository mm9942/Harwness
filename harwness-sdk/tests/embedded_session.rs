//! `.embedded(..)` plus Offline-Echo, kein Netz (#22 Welle 3A).
//!
//! Spiegelt `tests/echo_session.rs`, aber ohne jede `~/.harw`-Konfiguration:
//! die Wurzel-IR kommt ausschließlich aus einem im Test gebauten
//! Agenten-Artefakt (`harw_agent_artifact` + `harw_agent_dsl`).

use std::path::PathBuf;
use std::sync::Arc;

use harw_agent_artifact::bundle::{AgentInput, BundleBuilder};
use harw_agent_dsl::diagnostics::SourceFile;
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::ir_v2::AgentIr;
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::lower_v2::{LowerSources, compile_agent};
use harw_runtime::EmbeddedAgent;
use harwness_sdk::prelude::*;
use time::OffsetDateTime;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const ROOT_DEF: &str = r#"
schema = "harwness.agent/v1"
id = "acme.agent.sdk-embedded@1"
version = "1.0.0"
role = "worker"
specialization = "sdk-embedded"

[tools]
admitted = ["fs.read"]
"#;

fn compile(source: &str, target: &str) -> TestResult<AgentIr> {
    let files = vec![SourceFile::new(
        DefinitionLayer::UserGlobal,
        "agents/sdk-embedded/definition.toml",
        source,
    )];
    let sources = LowerSources::new(&files);
    let target = DefinitionId::parse(target)?;
    compile_agent(&target, &sources, OffsetDateTime::UNIX_EPOCH)
        .map_err(|diagnostics| -> Box<dyn std::error::Error> { diagnostics.to_string().into() })
}

fn embedded_agent() -> TestResult<EmbeddedAgent> {
    let ir = compile(ROOT_DEF, "acme.agent.sdk-embedded@1")?;
    let root = AgentInput {
        id: "root".to_owned(),
        name: "root".to_owned(),
        ir: serde_json::to_value(&ir)?,
        files: Vec::new(),
        children: Vec::new(),
    };
    let artifact = BundleBuilder::new(root).build()?;
    let bundle = harw_agent_artifact::Bundle::from_artifact(&artifact)?;
    Ok(EmbeddedAgent::from_bundle(bundle, &artifact)?)
}

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

fn fixture() -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&project)?;
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n")?;
    // Ein widersprüchlicher, gleichnamiger Agent im Root-Space — ein
    // eingebetteter Lauf darf ihn nie sehen (siehe
    // `harw-runtime/src/config.rs`, `load_config_embedded_never_reads_a_conflicting_agent_from_home`).
    let agent_dir = home.join("agents").join("sdk-embedded");
    std::fs::create_dir_all(&agent_dir)?;
    std::fs::write(
        agent_dir.join("definition.toml"),
        "schema = \"harwness.agent/v1\"\nid = \"acme.agent.sdk-embedded@1\"\nversion = \"9.9.9\"\nrole = \"worker\"\nspecialization = \"from-home-not-bundle\"\n",
    )?;
    Ok(Fixture {
        _dir: dir,
        home,
        project,
    })
}

fn builder(fixture: &Fixture, agent: EmbeddedAgent) -> TestResult<HarwnessBuilder> {
    Ok(Harwness::builder()
        .home(&fixture.home)
        .cwd(&fixture.project)
        .scaffold_home(false)
        .ephemeral(true)
        .offline_echo("pong")
        .embedded(Arc::new(agent)))
}

#[tokio::test]
async fn an_embedded_run_completes_a_turn_without_reading_harw_home() -> TestResult {
    let fixture = fixture()?;
    let agent = embedded_agent()?;
    let harwness = builder(&fixture, agent)?.build()?;
    let mut session = harwness.session()?;

    let report = session.send("ping").await?;

    assert_eq!(report.status, TurnStatus::Completed);
    assert_eq!(report.text.as_deref(), Some("pong"));
    Ok(())
}

#[test]
fn embedded_needs_neither_an_active_uia_nor_a_default_provider() -> TestResult {
    let fixture = fixture()?;
    let agent = embedded_agent()?;
    // Kein `.provider(..)`, keine `active_uia_definition` im (ohnehin
    // ignorierten) Root-Space — `.embedded(..)` allein muss genügen.
    let harwness = builder(&fixture, agent)?.build()?;
    let _session = harwness.session()?;
    Ok(())
}
