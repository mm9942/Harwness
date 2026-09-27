//! Plan R9, Teil B: die mitgelieferten Agenten (`harw-home/assets/agents`)
//! werden über die reguläre Discovery gefunden, gesenkt und als
//! benutzerdefinierte Agenten in den Roster aufgenommen — unter ihrer
//! Rechtedecke.

mod common;

use std::collections::HashMap;

use common::{TestError, TestResult, ctx};
use harw_agent_dsl::roles::AgentRoleId;
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{RegistryProfile, role_names};
use harw_registry_defaults::{AgentRoster, RosterSource};

/// Schreibt alle mitgelieferten `agents/`-Dateien in ein Temp-Home und baut
/// daraus den Roster.
fn bundled_roster() -> TestResult<(tempfile::TempDir, AgentRoster, Vec<String>)> {
    let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
    let mut names = Vec::new();
    for file in harw_home::bundled_files()
        .iter()
        .filter(|file| file.relative_path.starts_with("agents/"))
    {
        let target = file.target_in(home.path());
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, file.contents)?;
        if let Some(name) = file
            .relative_path
            .strip_prefix("agents/")
            .and_then(|rest| rest.strip_suffix("/definition.toml"))
        {
            names.push(name.to_owned());
        }
    }
    let config = harw_config::discover_config(&[home.path().to_path_buf()])
        .map_err(ctx("Discovery der mitgelieferten Agenten"))?;
    let agents = harw_registry_defaults::ConfigAgents::from_config(&config)
        .map_err(ctx("mitgelieferte Agenten senken"))?;
    let builtin = builtin_agent_definitions(&HashMap::new()).map_err(ctx("eingebaute Rollen"))?;
    let roster = AgentRoster::from_config(&builtin, &agents).map_err(ctx("Roster"))?;
    Ok((home, roster, names))
}

#[test]
fn every_bundled_definition_becomes_a_spawnable_custom_agent() -> TestResult {
    let (_home, roster, names) = bundled_roster()?;
    assert!(names.len() >= 21, "mindestens die 21 migrierten Agenten");
    for name in &names {
        let entry = roster
            .entry(name)
            .ok_or_else(|| TestError::Unexpected(format!("{name} fehlt im Roster")))?;
        assert!(
            matches!(entry.source, RosterSource::Custom { .. }),
            "{name} ist benutzerdefiniert"
        );
        assert!(
            matches!(
                entry.role,
                AgentRoleId::Worker | AgentRoleId::UiaWorker | AgentRoleId::ChildOrchestrator
            ),
            "{name}"
        );
        assert!(
            !entry.tools.iter().any(|tool| tool == "shell.exec"),
            "{name}: kein mitgelieferter Agent bekommt eine freie Shell"
        );
        assert!(
            roster.instructions(name).is_some(),
            "{name}: system.md kommt über instructions_file an"
        );
    }
    // Die eingebauten Rollen bleiben vollständig und unverändert.
    for role in role_names::ALL {
        let entry = roster
            .entry(role)
            .ok_or(TestError::Missing("eingebaute Rolle"))?;
        assert_eq!(entry.source, RosterSource::BuiltIn, "{role}");
    }
    assert_eq!(roster.len(), role_names::ALL.len() + names.len());
    Ok(())
}

#[test]
fn bundled_writers_keep_fs_write_and_readers_stay_read_only() -> TestResult {
    let (_home, roster, _names) = bundled_roster()?;
    for writer in ["synthesis-writer", "business-author", "rust-implementer"] {
        let entry = roster.entry(writer).ok_or(TestError::Missing("writer"))?;
        assert!(
            entry.tools.iter().any(|tool| tool == "fs.write"),
            "{writer} behält fs.write"
        );
        assert_eq!(entry.profile, RegistryProfile::WorkspaceEdit, "{writer}");
        assert!(!entry.read_only, "{writer}");
    }
    for reader in ["debugger", "security-auditor", "evidence-collector"] {
        let entry = roster.entry(reader).ok_or(TestError::Missing("reader"))?;
        assert!(entry.read_only, "{reader}");
        assert!(
            !entry.tools.iter().any(|tool| tool.starts_with("web.")),
            "{reader}: kein Netz"
        );
    }
    let latex = roster
        .entry("latex-writer")
        .ok_or(TestError::Missing("latex-writer"))?;
    assert_eq!(latex.role, AgentRoleId::UiaWorker);
    assert_eq!(latex.base_role, role_names::UIA_LATEX_WRITER);
    for orchestrator in [
        "debug-orchestrator",
        "security-inspection-orchestrator",
        "wargaming-orchestrator",
    ] {
        let entry = roster
            .entry(orchestrator)
            .ok_or(TestError::Missing("orchestrator"))?;
        assert_eq!(entry.role, AgentRoleId::ChildOrchestrator, "{orchestrator}");
        assert_eq!(entry.max_depth, Some(1), "{orchestrator}");
        assert!(
            entry.tools.iter().any(|tool| tool == "delegate_wave"),
            "{orchestrator}"
        );
        assert!(
            roster.delegation_targets(orchestrator).is_some(),
            "{orchestrator}: eigene Delegationsziele"
        );
    }
    Ok(())
}

/// Plan R9 (Matrix-Game mit Internet): `intel-web-researcher` wird ein
/// Roster-Eintrag unter der Decke von `researcher-web` — Profil `Research`
/// mit Netz-Werkzeugen, ohne jedes `fs.*` — und ist für den Game Master
/// (Organisationsrolle Root-Orchestrator) sichtbar und als
/// Delegationsziel deklariert, ebenso `evidence-collector` und
/// `evidence-critic`.
#[test]
fn intel_web_researcher_is_a_network_research_worker_the_game_master_can_delegate_to() -> TestResult
{
    let (_home, roster, _names) = bundled_roster()?;
    let researcher = roster
        .entry("intel-web-researcher")
        .ok_or(TestError::Missing("intel-web-researcher im Roster"))?;
    assert!(researcher.is_custom());
    assert_eq!(researcher.role, AgentRoleId::Worker);
    assert_eq!(researcher.base_role, role_names::RESEARCHER_WEB);
    assert_eq!(researcher.profile, RegistryProfile::Research);
    for tool in ["web.search", "web.fetch", "skills.load"] {
        assert!(
            researcher.tools.iter().any(|admitted| admitted == tool),
            "intel-web-researcher braucht {tool}: {:?}",
            researcher.tools
        );
    }
    assert!(
        !researcher.tools.iter().any(|tool| tool.starts_with("fs.")
            || tool.starts_with("deps.")
            || tool == "shell.exec"),
        "kein Workspace-Zugriff, keine Shell: {:?}",
        researcher.tools
    );
    assert!(researcher.read_only);
    assert_eq!(researcher.skills, ["evidence-quality-review"]);
    assert!(harw_registry_defaults::role_may_use_open_web(
        &researcher.base_role
    ));
    assert!(roster.instructions("intel-web-researcher").is_some());

    let master = roster
        .entry(role_names::MATRIX_GAME_MASTER)
        .ok_or(TestError::Missing("matrix-game-master"))?;
    assert_eq!(master.role, AgentRoleId::RootOrchestrator);
    let declared = roster
        .delegation_targets(role_names::MATRIX_GAME_MASTER)
        .ok_or(TestError::Missing("[delegation].targets des Game Masters"))?;
    let candidates: Vec<(String, AgentRoleId)> = roster
        .entries()
        .map(|entry| (entry.name.clone(), entry.role))
        .collect();
    let visible: Vec<String> = harw_core::delegation_visibility::visible_delegation_targets(
        master.role,
        &candidates,
        &[],
        1,
    )
    .into_iter()
    .map(|target| target.name)
    .collect();
    for target in [
        "intel-web-researcher",
        "evidence-collector",
        "evidence-critic",
    ] {
        assert!(
            declared.iter().any(|name| name == target),
            "Game Master deklariert {target} nicht: {declared:?}"
        );
        assert!(
            visible.iter().any(|name| name == target),
            "Game Master sieht {target} nicht"
        );
        let entry = roster.entry(target).ok_or(TestError::Missing("Ziel"))?;
        assert!(
            entry.read_only,
            "{target} ist ein lesendes Ziel (auch im Plan-Modus)"
        );
    }
    // Die Sitz-Rollen bleiben ohne Netz (der Runner startet sie mit einer
    // Lesesicht) und dürfen kein offenes Web lesen.
    for seat in role_names::MATRIX_ROLES {
        assert!(
            !harw_registry_defaults::role_may_use_open_web(seat),
            "{seat}"
        );
    }
    Ok(())
}
