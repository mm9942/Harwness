//! Rechte-Matrix: alle eingebauten Rollen × alle Registry-Profile × alle
//! Rechtesätze (W5 RD, Befunde G-055, F-084, F-073, Annahme A5).
//!
//! # Was hier festgehalten wird
//! 1. **Rollentabelle**: jede Rolle aus `role_names::ALL` hat genau das
//!    erwartete Profil und den erwarteten Reducer.
//! 2. **Profil × Rechtesatz** (`RegistryProfile::ALL.len()` × 2⁷):
//!    `tool_names_for(granted)` registriert nie ein Werkzeug ohne gewährtes
//!    Recht; `fs.write` nur in `Full`/`MemoryStewardship`/`UiaWriter`,
//!    `shell.exec` nur in `Full`/`ShellExecution`/`UiaQuickHelper`
//!    (Addendum I)/`UiaShellWorker`, jeweils nur mit dem passenden Recht;
//!    `deps.source_*` nur mit `ReadCargoRegistry`; `web.*` nur in `Research`
//!    (alle drei Werkzeuge), `UiaQuickHelper`, `UiaExplorer` oder `UiaWriter`
//!    (nur `web.fetch`), jeweils nur mit `NetworkAccess`; `browser.*` in
//!    keinem Profil — **außer** `browser.open` in `UiaQuickHelper`
//!    (Nutzerentscheidung, siehe `UIA_QUICK_HELPER_BROWSER_TOOLS`).
//! 3. **Rolle × Rechtesatz**: nach dem Rollen-Reducer sieht keine Rolle
//!    `fs.write`, `shell.exec` oder `browser.*` — auch `uia-worker` nicht:
//!    ihre `browser.open`-Ausnahme kommt aus der festen Profilzuweisung
//!    (`UiaQuickHelper`) bei der Registry-Montage, nie aus dem Reducer;
//!    `ReadOnly` (ihr Reducer) deckelt `NetworkAccess` ohnehin weg.
//!    `researcher-web` sieht nie `fs.*`, `deps.*` oder `lens.ask`.
//! 4. **TOML-Seite** (andere Quelle): keine Rolle admittiert `fs.write`,
//!    `shell.exec` oder `browser.*` — außer `uia-worker`, die einzige Rolle
//!    mit der einzigen `browser.*`-Ausnahme `browser.open`;
//!    `researcher-web` admittiert nur `web.*` und verbietet `fs.*`/`deps.*`
//!    ausdrücklich.
//! 5. **Montage**: die tatsächlich gebaute Registry entspricht Punkt 2 — mit
//!    der dokumentierten Ausnahme `UiaQuickHelper`/`browser.open`, das ohne
//!    einen tatsächlichen `BrowserOpenGrant` (Feature `browser`, hier nicht
//!    verdrahtet) nicht real registriert wird (siehe `expected_tool_names`).
//!
//! # Determinismus
//! Keine Netz- oder Prozessabhängigkeit; die Montage liest nur das
//! Crate-Verzeichnis (Projekterkennung wie in den bestehenden Tests).

use std::collections::{BTreeSet, HashMap};

use harw_authority::{Permission, PermissionSet};
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_project_discovery::{DiscoveryConfig, discover_project};
use harw_registry_defaults::authority::{AuthorityReducer, authority_reducer_for_role};
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    IdentityOverrides, RegistryProfile, assemble_registry_for_sandbox, profile_for_role, role_names,
};

mod common;
use common::{TestError, TestResult, ctx};

/// Alle sieben Rechte in fester Reihenfolge (Bitposition = Index).
const PERMISSIONS: [Permission; 7] = [
    Permission::ReadWorkspace,
    Permission::WriteWorkspace,
    Permission::ExecuteProcess,
    Permission::NetworkAccess,
    Permission::ReadSecrets,
    Permission::ManagePlugins,
    Permission::ReadCargoRegistry,
];

/// Die Browser-Werkzeuge, wie `harw-tool-browser` sie benennt.
const BROWSER: &[&str] = &[
    "browser.open",
    "browser.observe",
    "browser.find",
    "browser.act",
    "browser.wait",
    "browser.events",
    "browser.close",
];

/// Alle 128 Teilmengen der sieben Rechte.
fn every_permission_subset() -> Vec<PermissionSet> {
    (0u8..128)
        .map(|mask| {
            PermissionSet::from_policy(
                PERMISSIONS
                    .iter()
                    .enumerate()
                    .filter(|(bit, _)| mask & (1u8 << *bit) != 0)
                    .map(|(_, permission)| *permission),
            )
        })
        .collect()
}

/// Erwartete Rollentabelle: Rolle → (Profil, Reducer).
fn expected_role_table() -> Vec<(&'static str, RegistryProfile, AuthorityReducer)> {
    vec![
        (
            role_names::ROOT_ORCHESTRATOR,
            RegistryProfile::Planning,
            AuthorityReducer::ReadRegistry,
        ),
        (
            role_names::EXPLORER,
            RegistryProfile::ReadOnlyExplore,
            AuthorityReducer::ReadRegistry,
        ),
        (
            role_names::RESEARCHER_DEPS,
            RegistryProfile::ReadOnlyExplore,
            AuthorityReducer::ReadRegistry,
        ),
        (
            role_names::ANALYST,
            RegistryProfile::ReadOnlyExplore,
            AuthorityReducer::ReadRegistry,
        ),
        (
            role_names::PLANNER,
            RegistryProfile::Planning,
            AuthorityReducer::ReadRegistry,
        ),
        (
            role_names::RESEARCHER_WEB,
            RegistryProfile::Research,
            AuthorityReducer::ReadNetwork,
        ),
        (
            role_names::SECURITY_EGRESS_TRIAGE,
            RegistryProfile::NoTools,
            AuthorityReducer::ReadOnly,
        ),
        (
            role_names::SECURITY_BASELINE_TRIAGE,
            RegistryProfile::NoTools,
            AuthorityReducer::ReadOnly,
        ),
        (
            role_names::SECURITY_STRUCTURE_TRIAGE,
            RegistryProfile::NoTools,
            AuthorityReducer::ReadOnly,
        ),
        (
            role_names::SECURITY_ENDPOINT_TRIAGE,
            RegistryProfile::NoTools,
            AuthorityReducer::ReadOnly,
        ),
        // Behoben (Agent F-FIX, Addendum F+G): `authority_reducer_for_role`
        // in `harw-registry-defaults/src/authority.rs` trägt jetzt Match-Arme
        // für `MEMORY_STEWARD`, `UIA_WORKER` und `EXECUTOR`; die Reducer
        // unten stimmen mit dort überein (siehe Doku bei
        // `authority_reducer_for_role`).
        (
            role_names::EXECUTOR,
            RegistryProfile::ShellExecution,
            AuthorityReducer::ReadOnly,
        ),
        (
            role_names::MEMORY_STEWARD,
            RegistryProfile::MemoryStewardship,
            AuthorityReducer::ReadRegistry,
        ),
        // Addendum I (korrigiert REG-DE): `uia-worker` ist der exklusive
        // Schnellhelfer der UIA — nicht mehr reine Netz-Recherche
        // (`RegistryProfile::Research`), sondern
        // `RegistryProfile::UiaQuickHelper` mit dem `executor`-Muster als
        // Reducer-Ausnahme (siehe `authority_reducer_for_role`).
        (
            role_names::UIA_WORKER,
            RegistryProfile::UiaQuickHelper,
            AuthorityReducer::ReadOnly,
        ),
        // Addendum K: `agent-steward` ist die einzige Rolle mit
        // `RegistryProfile::AgentStewardship`, ebenfalls mit dem
        // `executor`-Muster als Reducer-Ausnahme (siehe
        // `authority_reducer_for_role`).
        (
            role_names::AGENT_STEWARD,
            RegistryProfile::AgentStewardship,
            AuthorityReducer::ReadOnly,
        ),
        // Host-Shell-Spezialisierung der UIA: `RegistryProfile::UiaShellWorker`
        // mit dem `executor`-Muster als Reducer-Ausnahme (siehe
        // `authority_reducer_for_role`).
        (
            role_names::UIA_SHELL_WORKER,
            RegistryProfile::UiaShellWorker,
            AuthorityReducer::ReadOnly,
        ),
        // Read-only Erkundungsspezialisierung der UIA: `RegistryProfile::
        // UiaExplorer` mit dem `uia-worker`-Muster als Reducer-Ausnahme
        // (siehe `authority_reducer_for_role`).
        (
            role_names::UIA_EXPLORER,
            RegistryProfile::UiaExplorer,
            AuthorityReducer::ReadOnly,
        ),
        // Schreibende Erkundungsspezialisierung der UIA: `RegistryProfile::
        // UiaWriter` mit demselben Reducer-Muster.
        (
            role_names::UIA_WRITER,
            RegistryProfile::UiaWriter,
            AuthorityReducer::ReadOnly,
        ),
    ]
}

#[test]
fn test_role_table_matches_profile_and_reducer_for_every_builtin_role() {
    let table = expected_role_table();
    let listed: BTreeSet<&str> = table.iter().map(|(role, _, _)| *role).collect();
    let all: BTreeSet<&str> = role_names::ALL.iter().copied().collect();
    assert_eq!(listed, all, "die Matrix muss jede eingebaute Rolle führen");

    for (role, profile, reducer) in table {
        assert_eq!(profile_for_role(role), Some(profile), "{role}: Profil");
        assert_eq!(
            authority_reducer_for_role(role),
            Some(reducer),
            "{role}: Reducer"
        );
    }
}

#[test]
fn test_profile_by_permission_matrix_never_registers_ungranted_tools() -> TestResult {
    for profile in RegistryProfile::ALL {
        for granted in every_permission_subset() {
            let tools = profile.tool_names_for(&granted);
            for tool in &tools {
                let needed = harw_registry_defaults::tool_permission(tool).ok_or(
                    TestError::Unexpected(format!("{profile:?}: {tool} ohne bekanntes Recht")),
                )?;
                assert!(
                    granted.contains(needed),
                    "{profile:?}: {tool} ohne {needed:?}"
                );
                // Nutzerentscheidung: einzige Ausnahme ist `browser.open`
                // unter `UiaQuickHelper` — alle übrigen sechs
                // `browser.*`-Werkzeuge bleiben für jedes Profil ohne Grant.
                let is_the_documented_exception =
                    *tool == "browser.open" && *profile == RegistryProfile::UiaQuickHelper;
                assert!(
                    is_the_documented_exception || !BROWSER.contains(tool),
                    "{profile:?}: {tool} ohne Grant"
                );
            }
            let has = |name: &str| tools.contains(&name);
            // `fs.write` gehört zu `Full`, `MemoryStewardship` und
            // `UiaWriter` (schreibende Erkundungsspezialisierung der UIA,
            // siehe `agents/uia-writer.toml` und die Begründung bei
            // `RegistryProfile::UiaWriter` in `harw-registry-defaults/src/
            // profile.rs`); `shell.exec` zu `Full`, `ShellExecution`,
            // `UiaQuickHelper` (Addendum I) und `UiaShellWorker`.
            let may_write = matches!(
                *profile,
                RegistryProfile::Full
                    | RegistryProfile::MemoryStewardship
                    | RegistryProfile::UiaWriter
            );
            let may_exec = matches!(
                *profile,
                RegistryProfile::Full
                    | RegistryProfile::ShellExecution
                    | RegistryProfile::UiaQuickHelper
                    | RegistryProfile::UiaShellWorker
            );
            assert_eq!(
                has("fs.write"),
                may_write && granted.contains(Permission::WriteWorkspace),
                "{profile:?}: fs.write"
            );
            assert_eq!(
                has("shell.exec"),
                may_exec && granted.contains(Permission::ExecuteProcess),
                "{profile:?}: shell.exec"
            );
            if tools.iter().any(|tool| tool.starts_with("deps.source_")) {
                assert!(
                    granted.contains(Permission::ReadCargoRegistry),
                    "{profile:?}"
                );
            }
            if tools.iter().any(|tool| tool.starts_with("web.")) {
                // `Research` führt alle drei `web.*`-Werkzeuge; `UiaQuickHelper`
                // (Addendum I), `UiaExplorer` und `UiaWriter` ausschließlich
                // `web.fetch` (`UIA_QUICK_HELPER_WEB_TOOLS`).
                assert!(matches!(
                    *profile,
                    RegistryProfile::Research
                        | RegistryProfile::UiaQuickHelper
                        | RegistryProfile::UiaExplorer
                        | RegistryProfile::UiaWriter
                ));
                assert!(granted.contains(Permission::NetworkAccess));
            }
            if *profile == RegistryProfile::Research {
                assert!(
                    tools.iter().all(|tool| tool.starts_with("web.")),
                    "Research darf nur web.* führen: {tools:?}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn test_role_by_permission_matrix_after_reducer() {
    for (role, profile, reducer) in expected_role_table() {
        for parent in every_permission_subset() {
            let child = reducer.reduce(&parent);
            assert!(child.is_subset_of(&parent), "{role}: Reducer gewährt Neues");
            assert!(
                child.is_subset_of(&reducer.ceiling()),
                "{role}: über der Obergrenze"
            );
            for forbidden in [
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
                Permission::ReadSecrets,
                Permission::ManagePlugins,
            ] {
                assert!(!child.contains(forbidden), "{role}: {forbidden:?}");
            }

            let tools = profile.tool_names_for(&child);
            for tool in &tools {
                assert!(
                    *tool != "fs.write" && *tool != "shell.exec" && !BROWSER.contains(tool),
                    "{role}: {tool} darf nie sichtbar sein"
                );
            }
            if role == role_names::RESEARCHER_WEB {
                assert!(!child.contains(Permission::ReadWorkspace));
                assert!(!child.contains(Permission::ReadCargoRegistry));
                for tool in &tools {
                    assert!(
                        !tool.starts_with("fs.")
                            && !tool.starts_with("deps.")
                            && *tool != "lens.ask",
                        "researcher-web sieht {tool}"
                    );
                }
            }
            let sees_source = tools.iter().any(|tool| tool.starts_with("deps.source_"));
            assert_eq!(
                sees_source,
                parent.contains(Permission::ReadCargoRegistry)
                    && reducer == AuthorityReducer::ReadRegistry
                    && profile
                        .registered_tool_names()
                        .contains(&"deps.source_read"),
                "{role}: deps.source_* genau dann, wenn der Elternteil ReadCargoRegistry trägt"
            );
        }
    }
}

#[test]
fn test_role_tomls_never_admit_write_shell_or_browser_and_researcher_web_is_web_only() -> TestResult
{
    let roles: HashMap<String, harw_agent_dsl::ExecutableAgentIr> =
        builtin_agent_definitions(&HashMap::new()).map_err(ctx(
            "eingebaute Rollendefinitionen müssen sich auflösen lassen",
        ))?;

    for role in role_names::ALL {
        let ir = roles.get(*role).ok_or(TestError::Unexpected(format!(
            "Rolle {role} fehlt in den aufgelösten Definitionen"
        )))?;
        // Dieselbe berechnete Bedingung wie
        // `harw_registry_defaults::authority::tests::
        // test_authority_reducer_for_role_covers_every_role_and_bounds_its_profile`
        // (dort `exempt_from_subset_bound`) statt einer zweiten,
        // handgepflegten Rollenliste: `executor`, `memory-steward`,
        // `uia-worker` und `agent-steward` sind dokumentierte Ausnahmen —
        // ihr Profil braucht ein Recht (`WriteWorkspace`/`ExecuteProcess`),
        // das der `AuthorityReducer` ihrer Rolle nie trägt, weil sie es über
        // ihre feste Profilzuweisung bei der Registry-Montage bekommen (siehe
        // `authority_reducer_for_role`), nicht über den Reducer.
        let profile = profile_for_role(role)
            .ok_or(TestError::Missing("eingebaute Rolle braucht ein Profil"))?;
        let reducer = authority_reducer_for_role(role)
            .ok_or(TestError::Missing("eingebaute Rolle braucht einen Reducer"))?;
        let is_documented_gated_exception = !profile
            .required_permissions()
            .is_subset_of(&reducer.ceiling());
        let admitted = ir.tool_surface().admitted();
        for tool in admitted {
            // Nutzerentscheidung: `uia-worker` darf `browser.open` benutzen —
            // die einzige zulässige `browser.*`-Ausnahme, für genau diese
            // eine Rolle und genau dieses eine Werkzeug (siehe
            // `RegistryProfile::UiaQuickHelper` und
            // `UIA_QUICK_HELPER_BROWSER_TOOLS` in
            // `harw-registry-defaults/src/profile.rs`).
            if tool == "browser.open" && *role == role_names::UIA_WORKER {
                continue;
            }
            if is_documented_gated_exception
                && profile.registered_tool_names().contains(&tool.as_str())
            {
                // Erwartete, dokumentierte Ausnahme — siehe oben. Jedes
                // übrige `browser.*`-Werkzeug gehört zu keinem Profil (W5 RD),
                // bleibt also für jede Rolle verboten.
                assert!(!tool.starts_with("browser."), "{role} admittiert {tool}");
                continue;
            }
            assert!(
                tool != "fs.write" && tool != "shell.exec" && !tool.starts_with("browser."),
                "{role} admittiert {tool}"
            );
        }
    }

    let web = roles
        .get(role_names::RESEARCHER_WEB)
        .ok_or(TestError::Missing("researcher-web ist eingebaut"))?;
    let admitted: BTreeSet<&str> = web
        .tool_surface()
        .admitted()
        .iter()
        .map(String::as_str)
        .collect();
    let expected: BTreeSet<&str> = ["web.fetch", "web.docs_rs", "web.crates_io"].into();
    assert_eq!(admitted, expected, "researcher-web admittiert nur web.*");

    let forbidden: BTreeSet<&str> = web
        .tool_surface()
        .forbidden()
        .iter()
        .map(String::as_str)
        .collect();
    for tool in [
        "fs.read",
        "fs.list",
        "fs.search",
        "fs.glob",
        "fs.grep",
        "fs.write",
        "shell.exec",
        "deps.graph",
        "deps.locked",
        "deps.source_read",
        "deps.source_search",
        "deps.source_list",
    ] {
        assert!(
            forbidden.contains(tool),
            "researcher-web muss {tool} ausdrücklich verbieten"
        );
    }
    Ok(())
}

/// Die lesenden `fs.*`-Werkzeuge, wie `RegistryProfile::AgentStewardship`
/// sie registriert — dieselbe Liste wie `crate::profile::FS_READ_ONLY_TOOLS`
/// (`pub(crate)`, hier deshalb als eigene Kopie, wie schon in
/// `harw-registry-defaults/src/profile.rs::test_read_only_explore_exposes_exact_tool_set`).
const FS_READ_ONLY: &[&str] = &["fs.read", "fs.list", "fs.search", "fs.glob", "fs.grep"];

/// Das lesende Doc-Werkzeug, wie `RegistryProfile::AgentStewardship` es
/// registriert — dieselbe Liste wie `crate::profile::DOC_TOOLS` (`pub(crate)`,
/// hier deshalb als eigene Kopie, wie schon bei [`FS_READ_ONLY`]).
const DOC_READ_ONLY: &[&str] = &["doc.read_pdf"];

/// Erwartete beworbene/registrierte Werkzeuge unter `granted` — für jedes
/// Profil außer `AgentStewardship` und `UiaQuickHelper` unverändert
/// `tool_names_for(granted)` (die statische Vertrags-Obermenge).
///
/// # Nachtrag K3 (`AgentStewardship`)
/// Ohne `AgentDefinitionAccess` (der Standardpfad über
/// `assemble_registry_for_sandbox`, den dieser Test verwendet) registriert
/// `AgentDefinitionToolProvider` fail-closed nur `agents.validate`/
/// `agents.list_proposals` — `RegistryProfile::tool_names_for` bleibt
/// dagegen bewusst bei der maximalen Vertrags-Obermenge (siehe
/// `RegistryProfile::registered_tool_names` und
/// `tests/tool_admission_coverage.rs`), taugt hier also nicht als
/// Erwartungswert für die tatsächlich montierte Registry.
///
/// # Nutzerentscheidung (`UiaQuickHelper`)
/// `RegistryProfile::UiaQuickHelper::tool_names_for` bewirbt `browser.open`
/// statisch (siehe `harw-registry-defaults/src/profile.rs::
/// UIA_QUICK_HELPER_BROWSER_TOOLS`). Der tatsächliche Laufzeit-Provider
/// braucht dafür weiterhin einen `harw_tool_browser::BrowserOpenGrant` über
/// `profile::browser_tool_provider` (Feature `browser`, standardmäßig aus),
/// den `assemble_registry_for_sandbox` (der Standardpfad, den dieser Test
/// verwendet) nicht baut — die tatsächlich montierte Registry bleibt also
/// ohne `browser.open`, unabhängig vom gewährten `NetworkAccess`.
fn expected_tool_names(profile: RegistryProfile, granted: &PermissionSet) -> Vec<String> {
    if profile == RegistryProfile::AgentStewardship {
        let raw: Vec<&'static str> = FS_READ_ONLY
            .iter()
            .chain(DOC_READ_ONLY.iter())
            .chain(harw_registry_defaults::agent_definition_tool_names_for_access(None).iter())
            .copied()
            .collect();
        return raw
            .into_iter()
            .filter(|tool| {
                harw_registry_defaults::tool_permission(tool)
                    .is_some_and(|needed| granted.contains(needed))
            })
            .map(|tool| tool.to_owned())
            .collect();
    }
    if profile == RegistryProfile::UiaQuickHelper {
        return profile
            .tool_names_for(granted)
            .into_iter()
            .filter(|tool| *tool != "browser.open")
            .map(|tool| tool.to_owned())
            .collect();
    }
    profile
        .tool_names_for(granted)
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

#[test]
fn test_assembled_registry_matches_the_matrix_for_every_profile_and_permission_set() -> TestResult {
    let cwd = std::env::current_dir().map_err(ctx("cwd"))?;
    let project = discover_project(&cwd, &DiscoveryConfig::default())
        .map_err(ctx("Discovery im Crate-Verzeichnis"))?;

    for profile in RegistryProfile::ALL {
        for granted in every_permission_subset() {
            let assembled = assemble_registry_for_sandbox(
                *profile,
                &project,
                IdentityOverrides::default(),
                ApprovalModeCell::default(),
                &granted,
            )
            .map_err(ctx("assemble"))?;
            let registered: Vec<String> = assembled
                .registry
                .tool_providers()
                .iter()
                .flat_map(|provider| provider.tools())
                .map(|spec| spec.name().to_owned())
                .collect();
            let expected = expected_tool_names(*profile, &granted);
            assert_eq!(registered, expected, "{profile:?} unter {granted:?}");
            assert_eq!(assembled.identity.tools_available, expected, "{profile:?}");
        }
    }
    Ok(())
}
