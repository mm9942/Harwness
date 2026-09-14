//! Rechte-Matrix: alle eingebauten Rollen × alle Registry-Profile × alle
//! Rechtesätze (W5 RD, Befunde G-055, F-084, F-073, Annahme A5).
//!
//! # Was hier festgehalten wird
//! 1. **Rollentabelle**: jede Rolle aus `role_names::ALL` hat genau das
//!    erwartete Profil und den erwarteten Reducer.
//! 2. **Profil × Rechtesatz** (5 × 2⁷): `tool_names_for(granted)` registriert
//!    nie ein Werkzeug ohne gewährtes Recht; `fs.write`/`shell.exec` nur im
//!    Profil `Full` und nur mit dem jeweiligen Recht; `deps.source_*` nur mit
//!    `ReadCargoRegistry`; `web.*` nur im Profil `Research` und nur mit
//!    `NetworkAccess`; `browser.*` in keinem Profil.
//! 3. **Rolle × Rechtesatz**: nach dem Rollen-Reducer sieht keine Rolle
//!    `fs.write`, `shell.exec` oder `browser.*`; `researcher-web` sieht nie
//!    `fs.*`, `deps.*` oder `lens.ask`.
//! 4. **TOML-Seite** (andere Quelle): keine Rolle admittiert `fs.write`,
//!    `shell.exec` oder `browser.*`; `researcher-web` admittiert nur `web.*`
//!    und verbietet `fs.*`/`deps.*` ausdrücklich.
//! 5. **Montage**: die tatsächlich gebaute Registry entspricht Punkt 2.
//!
//! # Determinismus
//! Keine Netz- oder Prozessabhängigkeit; die Montage liest nur das
//! Crate-Verzeichnis (Projekterkennung wie in den bestehenden Tests).

use std::collections::{BTreeSet, HashMap};

use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_project_discovery::{DiscoveryConfig, discover_project};
use harw_registry_defaults::authority::{AuthorityReducer, authority_reducer_for_role};
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    IdentityOverrides, RegistryProfile, assemble_registry_for_sandbox, profile_for_role,
    role_names,
};
use harw_sandbox::{Permission, PermissionSet};

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
        (role_names::EXPLORER, RegistryProfile::ReadOnlyExplore, AuthorityReducer::ReadRegistry),
        (
            role_names::RESEARCHER_DEPS,
            RegistryProfile::ReadOnlyExplore,
            AuthorityReducer::ReadRegistry,
        ),
        (role_names::ANALYST, RegistryProfile::ReadOnlyExplore, AuthorityReducer::ReadRegistry),
        (role_names::PLANNER, RegistryProfile::Planning, AuthorityReducer::ReadRegistry),
        (role_names::RESEARCHER_WEB, RegistryProfile::Research, AuthorityReducer::ReadNetwork),
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
        assert_eq!(authority_reducer_for_role(role), Some(reducer), "{role}: Reducer");
    }
}

#[test]
fn test_profile_by_permission_matrix_never_registers_ungranted_tools() {
    for profile in RegistryProfile::ALL {
        for granted in every_permission_subset() {
            let tools = profile.tool_names_for(&granted);
            for tool in &tools {
                let needed = harw_registry_defaults::tool_permission(tool)
                    .unwrap_or_else(|| panic!("{profile:?}: {tool} ohne bekanntes Recht"));
                assert!(granted.contains(needed), "{profile:?}: {tool} ohne {needed:?}");
                assert!(!BROWSER.contains(tool), "{profile:?}: {tool} ohne Grant");
            }
            let has = |name: &str| tools.iter().any(|tool| *tool == name);
            let full = *profile == RegistryProfile::Full;
            assert_eq!(
                has("fs.write"),
                full && granted.contains(Permission::WriteWorkspace),
                "{profile:?}: fs.write"
            );
            assert_eq!(
                has("shell.exec"),
                full && granted.contains(Permission::ExecuteProcess),
                "{profile:?}: shell.exec"
            );
            if tools.iter().any(|tool| tool.starts_with("deps.source_")) {
                assert!(granted.contains(Permission::ReadCargoRegistry), "{profile:?}");
            }
            if tools.iter().any(|tool| tool.starts_with("web.")) {
                assert_eq!(*profile, RegistryProfile::Research);
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
}

#[test]
fn test_role_by_permission_matrix_after_reducer() {
    for (role, profile, reducer) in expected_role_table() {
        for parent in every_permission_subset() {
            let child = reducer.reduce(&parent);
            assert!(child.is_subset_of(&parent), "{role}: Reducer gewährt Neues");
            assert!(child.is_subset_of(&reducer.ceiling()), "{role}: über der Obergrenze");
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
                    && profile.registered_tool_names().contains(&"deps.source_read"),
                "{role}: deps.source_* genau dann, wenn der Elternteil ReadCargoRegistry trägt"
            );
        }
    }
}

#[test]
fn test_role_tomls_never_admit_write_shell_or_browser_and_researcher_web_is_web_only() {
    let roles: HashMap<String, harw_agent_dsl::ExecutableAgentIr> =
        builtin_agent_definitions(&HashMap::new())
            .expect("eingebaute Rollendefinitionen müssen sich auflösen lassen");

    for role in role_names::ALL {
        let ir = roles
            .get(*role)
            .unwrap_or_else(|| panic!("Rolle {role} fehlt in den aufgelösten Definitionen"));
        let admitted = ir.tool_surface().admitted();
        for tool in admitted {
            assert!(
                tool != "fs.write" && tool != "shell.exec" && !tool.starts_with("browser."),
                "{role} admittiert {tool}"
            );
        }
    }

    let web = roles
        .get(role_names::RESEARCHER_WEB)
        .expect("researcher-web ist eingebaut");
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
        assert!(forbidden.contains(tool), "researcher-web muss {tool} ausdrücklich verbieten");
    }
}

#[test]
fn test_assembled_registry_matches_the_matrix_for_every_profile_and_permission_set() {
    let cwd = std::env::current_dir().expect("cwd");
    let project = discover_project(&cwd, &DiscoveryConfig::default())
        .expect("Discovery im Crate-Verzeichnis");

    for profile in RegistryProfile::ALL {
        for granted in every_permission_subset() {
            let assembled = assemble_registry_for_sandbox(
                *profile,
                &project,
                IdentityOverrides::default(),
                ApprovalModeCell::default(),
                &granted,
            )
            .expect("assemble");
            let registered: Vec<String> = assembled
                .registry
                .tool_providers()
                .iter()
                .flat_map(|provider| provider.tools())
                .map(|spec| spec.name().to_owned())
                .collect();
            let expected: Vec<String> = profile
                .tool_names_for(&granted)
                .iter()
                .map(|name| (*name).to_owned())
                .collect();
            assert_eq!(registered, expected, "{profile:?} unter {granted:?}");
            assert_eq!(assembled.identity.tools_available, expected, "{profile:?}");
        }
    }
}
