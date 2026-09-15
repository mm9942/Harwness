//! Rechte-Matrix: alle eingebauten Rollen × alle Registry-Profile × alle
//! Rechtesätze (W5 RD, Befunde G-055, F-084, F-073, Annahme A5).
//!
//! # Was hier festgehalten wird
//! 1. **Rollentabelle**: jede Rolle aus `role_names::ALL` hat genau das
//!    erwartete Profil und den erwarteten Reducer.
//! 2. **Profil × Rechtesatz** (8 × 2⁷): `tool_names_for(granted)` registriert
//!    nie ein Werkzeug ohne gewährtes Recht; `fs.write` nur in `Full`/
//!    `MemoryStewardship`, `shell.exec` nur in `Full`/`ShellExecution`/
//!    `UiaQuickHelper` (Addendum I), jeweils nur mit dem passenden Recht;
//!    `deps.source_*` nur mit `ReadCargoRegistry`; `web.*` nur in `Research`
//!    (alle drei Werkzeuge) oder `UiaQuickHelper` (nur `web.fetch`), jeweils
//!    nur mit `NetworkAccess`; `browser.*` in keinem Profil.
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
        // Behoben (Agent F-FIX, Addendum F+G): `authority_reducer_for_role`
        // in `harw-registry-defaults/src/authority.rs` trägt jetzt Match-Arme
        // für `MEMORY_STEWARD`, `UIA_WORKER` und `EXECUTOR`; die Reducer
        // unten stimmen mit dort überein (siehe Doku bei
        // `authority_reducer_for_role`).
        (role_names::EXECUTOR, RegistryProfile::ShellExecution, AuthorityReducer::ReadOnly),
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
        (role_names::UIA_WORKER, RegistryProfile::UiaQuickHelper, AuthorityReducer::ReadOnly),
        // Addendum K: `agent-steward` ist die einzige Rolle mit
        // `RegistryProfile::AgentStewardship`, ebenfalls mit dem
        // `executor`-Muster als Reducer-Ausnahme (siehe
        // `authority_reducer_for_role`).
        (role_names::AGENT_STEWARD, RegistryProfile::AgentStewardship, AuthorityReducer::ReadOnly),
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
            // `fs.write` gehört zu `Full` und `MemoryStewardship`; `shell.exec`
            // zu `Full`, `ShellExecution` und `UiaQuickHelper` (Addendum I).
            let may_write =
                matches!(*profile, RegistryProfile::Full | RegistryProfile::MemoryStewardship);
            let may_exec = matches!(
                *profile,
                RegistryProfile::Full
                    | RegistryProfile::ShellExecution
                    | RegistryProfile::UiaQuickHelper
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
                assert!(granted.contains(Permission::ReadCargoRegistry), "{profile:?}");
            }
            if tools.iter().any(|tool| tool.starts_with("web.")) {
                // `Research` führt alle drei `web.*`-Werkzeuge, `UiaQuickHelper`
                // (Addendum I) ausschließlich `web.fetch`.
                assert!(matches!(
                    *profile,
                    RegistryProfile::Research | RegistryProfile::UiaQuickHelper
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
        let profile = profile_for_role(role).expect("eingebaute Rolle braucht ein Profil");
        let reducer =
            authority_reducer_for_role(role).expect("eingebaute Rolle braucht einen Reducer");
        let is_documented_gated_exception =
            !profile.required_permissions().is_subset_of(&reducer.ceiling());
        let admitted = ir.tool_surface().admitted();
        for tool in admitted {
            if is_documented_gated_exception && profile.registered_tool_names().contains(&tool.as_str())
            {
                // Erwartete, dokumentierte Ausnahme — siehe oben. `browser.*`
                // gehört zu keinem Profil (W5 RD), bleibt also für jede Rolle
                // verboten.
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

/// Die lesenden `fs.*`-Werkzeuge, wie `RegistryProfile::AgentStewardship`
/// sie registriert — dieselbe Liste wie `crate::profile::FS_READ_ONLY_TOOLS`
/// (`pub(crate)`, hier deshalb als eigene Kopie, wie schon in
/// `harw-registry-defaults/src/profile.rs::test_read_only_explore_exposes_exact_tool_set`).
const FS_READ_ONLY: &[&str] = &["fs.read", "fs.list", "fs.search", "fs.glob", "fs.grep"];

/// Erwartete beworbene/registrierte Werkzeuge unter `granted` — für jedes
/// Profil außer `AgentStewardship` unverändert `tool_names_for(granted)`
/// (die statische Vertrags-Obermenge).
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
fn expected_tool_names(profile: RegistryProfile, granted: &PermissionSet) -> Vec<String> {
    if profile == RegistryProfile::AgentStewardship {
        let raw: Vec<&'static str> = FS_READ_ONLY
            .iter()
            .chain(
                harw_registry_defaults::agent_definition_tool_names_for_access(None).iter(),
            )
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
    profile.tool_names_for(granted).iter().map(|name| (*name).to_owned()).collect()
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
            let expected = expected_tool_names(*profile, &granted);
            assert_eq!(registered, expected, "{profile:?} unter {granted:?}");
            assert_eq!(assembled.identity.tools_available, expected, "{profile:?}");
        }
    }
}
