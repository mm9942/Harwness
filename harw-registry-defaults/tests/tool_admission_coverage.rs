//! Deckungstest: TOML-`admitted` gegen Rust-`RegistryProfile` (Befund 2).
//!
//! Spezifikationsquelle: Auftrag "Registry-TOML/Profil-Deckungstest" (dieser
//! Knoten). Motiviert durch die fehlende `lens.ask`-Zeile in
//! `agents/planner.toml`, die keiner der bestehenden Tests hätte sehen können.
//!
//! # Warum dieser Test nötig war
//! `harw-core/src/session.rs` baut die `SessionActivation` einer Rolle
//! deny-by-default aus genau der `[tools].admitted`-Liste ihrer TOML-Datei.
//! `RegistryProfile::tool_names()` (in `harw-registry-defaults/src/profile.rs`)
//! ist das, was die Rolle laut Registrierung *tatsächlich sehen und aufrufen*
//! dürfte. Bricht die Kette zwischen beiden — ein Werkzeug ist im Profil
//! registriert, aber die TOML admittiert es nicht —, sieht der Agent das
//! Werkzeug im System-Prompt-Inventar, darf es aber nicht aufrufen: ein
//! Fan-out hängt an einer Rückfrage fest, die ein Kind mit
//! `allow_pause = false` nie beantworten kann.
//!
//! # Was die bestehenden Tests nicht sehen konnten
//! `harw-registry-defaults/src/lib.rs::approval_allow_list_covers_every_builtin_role_tool`
//! prüft `RegistryProfile::tool_names()` gegen `AUTO_APPROVED_TOOLS` — beide
//! Werte kommen aus `profile.rs`. Der bestehende Verbotstest prüft `admitted`
//! nur gegen eine Verbotsliste (`fs.write`, `shell.exec`), nie gegen die vom
//! Profil bereitgestellte Positivliste. Keine bestehende Prüfung vergleicht
//! die TOML-Seite (`[tools].admitted`, geparst über die echte DSL-Pipeline)
//! gegen die Rust-Seite (`RegistryProfile::tool_names()`) — das ist genau die
//! Quellentrennung, die dieser Test herstellt: der Vergleichswert kommt aus
//! einer anderen Quelle als der geprüfte Wert.
//!
//! # Zwei Richtungen
//! 1. Jedes von einer Rolle `admitted` Werkzeug muss vom Profil der Rolle
//!    registriert sein (`tool_names()`, das beworbene Inventar inklusive der
//!    Composition-Root-Operationen `plan`/`goal`).
//! 2. Umgekehrt: jedes vom Profil registrierte Werkzeug einer Rolle muss die
//!    Rolle auch tatsächlich `admitted` haben — dieser Test wäre vor der
//!    `lens.ask`-Ergänzung in `planner.toml` für die Rolle `planner` rot
//!    gewesen.
//!
//! Eine dritte Prüfung macht zusätzlich sichtbar, welche von Rollen-Profilen
//! registrierten Werkzeuge keine der fünf eingebauten Rollen je `admitted`
//! (kein Fehler an sich, siehe die Begründung dort).
//!
//! # Determinismus
//! Keine Systemuhr in den Assertions selbst; `builtin_agent_definitions`
//! braucht intern einen Zeitstempel für Auflösungs-Traces
//! (`time::OffsetDateTime::now_utc()`), der aber nicht in das geprüfte
//! Ergebnis (`tool_surface().admitted()`) einfließt — zwei Läufe zu
//! verschiedenen Zeiten liefern dieselbe admittierte Liste.

use std::collections::{BTreeSet, HashMap};

use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    KANBAN_READ_TOOLS, KNOWLEDGE_READ_TOOLS, RegistryProfile, composition_tools_for_role,
    knowledge_tools_for_role, profile_for_role, role_names,
};
// Runde 5, Teil B: `host.sudo_exec` steuert ebenfalls die Composition-Root bei.
use harw_registry_defaults::profile::{SUDO_ROLES, SUDO_TOOLS, sudo_tools_for_role};
// Runde 5, Teil H: `agent.result` steuert ebenfalls die Composition-Root bei.
use harw_registry_defaults::profile::{CHILD_RESULT_TOOLS, child_result_tools_for_role};
// Runde 5, Teil M: `agent.message`/`parent.message` ebenso.
use harw_registry_defaults::profile::{
    CHILD_MESSAGE_TOOLS, PARENT_MESSAGE_TOOLS, child_message_tools_for_role,
    parent_message_tools_for_role,
};
// Runde 7, Teil M: die Matrix-Werkzeuge des Game Masters ebenso.
use harw_registry_defaults::profile::{MATRIX_GAME_MASTER_TOOLS, matrix_tools_for_role};
// Plan R9, Teil A: `skills.search`/`skills.load` ebenso.
use harw_registry_defaults::profile::{
    SKILL_CATALOG_EXCLUDED_ROLES, SKILL_CATALOG_TOOLS, skill_catalog_tools_for_role,
};

mod common;
use common::{TestError, TestResult, ctx};

/// Lädt die aufgelösten eingebauten Rollendefinitionen (TOML-Seite).
///
/// Keine lokalen Überschreibungen (`existing` bleibt leer) — dieser Test
/// prüft ausschließlich die eingebetteten Definitionen unter
/// `harw-registry-defaults/agents/`.
fn resolved_roles() -> TestResult<HashMap<String, harw_agent_dsl::ExecutableAgentIr>> {
    builtin_agent_definitions(&HashMap::new()).map_err(ctx(
        "eingebaute Rollendefinitionen müssen sich auflösen lassen",
    ))
}

/// Jedes von einer Rolle `admitted` Werkzeug muss vom Profil der Rolle
/// registriert/beworben sein — sonst admittiert eine TOML ein Werkzeug, das
/// der Agent laut Registrierung nie im Inventar sieht: toter, irreführender
/// Text in der `admitted`-Liste.
#[test]
fn every_role_admitted_tool_is_registered_by_its_profile() -> TestResult {
    let roles = resolved_roles()?;

    for role in role_names::ALL {
        let ir = roles.get(*role).ok_or(TestError::Unexpected(format!(
            "Rolle {role} fehlt in den aufgelösten Definitionen"
        )))?;
        let profile = profile_for_role(role).ok_or(TestError::Unexpected(format!(
            "eingebaute Rolle {role} braucht ein Profil"
        )))?;
        // Plan Punkt 1: Orchestratoren admittieren zusätzlich die Werkzeuge,
        // die die Composition-Root für sie beisteuert (`delegate_wave`) —
        // sie gehören zu keinem `RegistryProfile`, sind aber ebenso wenig
        // toter Text (siehe `profile::composition_tools_for_role`).
        // Plan Teil D: dasselbe gilt für die lesenden Wissenswerkzeuge, die
        // die Composition-Root bei vorhandenem Wissensspeicher anhängt
        // (`profile::knowledge_tools_for_role`).
        let advertised: BTreeSet<&str> = profile
            .tool_names()
            .into_iter()
            .chain(composition_tools_for_role(role).iter().copied())
            .chain(knowledge_tools_for_role(role).iter().copied())
            // Runde 5, Teil B: `host.sudo_exec` (nur TUI, nur Host-Shell-Worker).
            .chain(sudo_tools_for_role(role).iter().copied())
            // Runde 5, Teil H: `agent.result` (nur Rollen, die Kinder starten).
            .chain(child_result_tools_for_role(role).iter().copied())
            // Runde 5, Teil M: `agent.message` (Rollen, die Kinder starten)
            // und `parent.message` (Kind-Rollen mit Elternteil).
            .chain(child_message_tools_for_role(role).iter().copied())
            .chain(parent_message_tools_for_role(role).iter().copied())
            // Runde 7, Teil M: `matrix.*` (nur Game Master).
            .chain(matrix_tools_for_role(role).iter().copied())
            // Plan R9, Teil A: `skills.search`/`skills.load` (Skill-Katalog).
            .chain(skill_catalog_tools_for_role(role).iter().copied())
            // Plan R9, Teil F: Job-Kontrolle der Orchestratoren.
            .chain(
                harw_registry_defaults::profile::job_control_tools_for_role(role)
                    .iter()
                    .copied(),
            )
            .collect();

        for admitted in ir.tool_surface().admitted() {
            assert!(
                advertised.contains(admitted.as_str()),
                "Rolle {role} admittiert {admitted} in ihrer TOML, aber {profile:?} \
                 registriert/bewirbt es nicht (RegistryProfile::tool_names) — der \
                 Agent sieht das Werkzeug nie im System-Prompt und darf es \
                 folgerichtig auch nie aufrufen, dieser Eintrag ist toter Text"
            );
        }
    }
    Ok(())
}

/// Der Kernfall aus Befund 1: jedes vom Profil registrierte/beworbene
/// Werkzeug einer Rolle muss die Rolle auch tatsächlich `admitted` haben —
/// sonst sieht der Agent das Werkzeug im Inventar seines System-Prompts und
/// darf es nicht aufrufen (`harw-core/src/session.rs` baut die
/// `SessionActivation` deny-by-default ausschließlich aus `admitted`).
///
/// Dieser Test wäre vor der Ergänzung von `lens.ask` in `agents/planner.toml`
/// für die Rolle `planner` rot gewesen: `Planning` registriert `lens.ask`
/// (`RegistryProfile::tool_names`), aber `planner.toml` admittierte es nicht
/// — genau die Lücke aus Befund 1, gegen künftige Wiederholung abgesichert.
#[test]
fn every_profile_registered_tool_is_admitted_by_its_role() -> TestResult {
    let roles = resolved_roles()?;

    for role in role_names::ALL {
        let ir = roles.get(*role).ok_or(TestError::Unexpected(format!(
            "Rolle {role} fehlt in den aufgelösten Definitionen"
        )))?;
        let profile = profile_for_role(role).ok_or(TestError::Unexpected(format!(
            "eingebaute Rolle {role} braucht ein Profil"
        )))?;
        let admitted: BTreeSet<&str> = ir
            .tool_surface()
            .admitted()
            .iter()
            .map(String::as_str)
            .collect();

        let forbidden: BTreeSet<&str> = ir
            .tool_surface()
            .forbidden()
            .iter()
            .map(String::as_str)
            .collect();

        for tool in profile.tool_names() {
            // Einzige dokumentierte Ausnahme: das Explorer-Netz
            // (`web.fetch`/`web.search`, Nutzerentscheidung „der Explorer
            // durchsucht auch das Internet“) hängt am geteilten Profil
            // `ReadOnlyExplore`. `analyst`/`researcher-deps` teilen dieses
            // Profil, sollen aber kein Netz bekommen — sie müssen beide
            // Werkzeuge deshalb **ausdrücklich** verbieten (die Aktivierung
            // schaltet `forbidden` ab), statt sie nur stillschweigend
            // wegzulassen, und der `explorer` muss sie admittieren.
            let is_explorer_web_on_shared_profile = profile == RegistryProfile::ReadOnlyExplore
                && ["web.fetch", "web.search"].contains(&tool)
                && *role != role_names::EXPLORER;
            if is_explorer_web_on_shared_profile {
                assert!(
                    forbidden.contains(tool),
                    "Rolle {role} teilt ReadOnlyExplore mit dem explorer und muss \
                     {tool} ausdrücklich verbieten"
                );
                continue;
            }
            assert!(
                admitted.contains(tool),
                "Rolle {role} ({profile:?}) sieht {tool} in ihrem beworbenen \
                 Werkzeuginventar, aber agents/{role}.toml admittiert es nicht \
                 — die Rolle würde an einer Rückfrage für {tool} hängen \
                 bleiben, die sie mit allow_pause = false nie beantworten kann"
            );
        }
    }
    Ok(())
}

/// Sichtbarkeitsprobe für Befund 2, zweite Hälfte: welche von den
/// Rollen-Profilen registrierten Werkzeuge admittiert **keine** der fünf
/// eingebauten Rollen?
///
/// Kein Fehler an sich (siehe Auftrag) — aber eine stillschweigende neue
/// Lücke soll auffallen statt unbemerkt zu bleiben. Die Erwartungsmenge ist
/// deshalb explizit dokumentiert, nicht bloß geloggt: verändert sich die
/// Menge, muss diese Zeile bewusst angepasst werden.
///
/// Stand dieses Knotens: **keine** Lücke — die drei von Rollen genutzten
/// Profile (`ReadOnlyExplore`, `Research`, `Planning`) registrieren
/// zusammen genau die Vereinigung dessen, was `explorer`, `researcher-deps`,
/// `researcher-web`, `planner` und `analyst` admittieren (`plan`/`goal` sind
/// Composition-Root-Operationen, die `tool_names()` für `Planning`
/// zusätzlich bewirbt, siehe `PLANNING_OPERATION_TOOLS` in `profile.rs`, und
/// die `planner.toml` folgerichtig ebenfalls admittiert). `RegistryProfile::
/// Full` bleibt bewusst ausgenommen: keine der fünf eingebauten Rollen
/// bekommt `Full` (`profile_for_role` bildet nie darauf ab) — dass `fs.write`/
/// `shell.exec`/`browser.*` von keiner Rolle admittiert werden, ist der
/// gewollte Zustand aus AP W3-04, keine übersehene Lücke.
#[test]
fn profile_tools_unclaimed_by_any_role_match_the_documented_expectation() -> TestResult {
    let roles = resolved_roles()?;

    let mut admitted_anywhere: BTreeSet<String> = BTreeSet::new();
    for role in role_names::ALL {
        let ir = roles
            .get(*role)
            .ok_or(TestError::Missing("Rolle geladen"))?;
        admitted_anywhere.extend(ir.tool_surface().admitted().iter().cloned());
    }

    let role_profiles = [
        RegistryProfile::ReadOnlyExplore,
        RegistryProfile::Research,
        RegistryProfile::Planning,
    ];
    let mut registered_by_role_profiles: BTreeSet<String> = BTreeSet::new();
    for profile in role_profiles {
        registered_by_role_profiles.extend(profile.tool_names().into_iter().map(str::to_owned));
    }

    let unclaimed: BTreeSet<String> = registered_by_role_profiles
        .difference(&admitted_anywhere)
        .cloned()
        .collect();

    let expected: BTreeSet<String> = BTreeSet::new();
    assert_eq!(
        unclaimed, expected,
        "ein von einem Rollen-Profil registriertes Werkzeug hat keine \
         admittierende Rolle mehr — entweder eine neue Rolle braucht dieses \
         Werkzeug in ihrer admitted-Liste, oder diese Erwartungsmenge muss \
         bewusst um {unclaimed:?} erweitert werden"
    );
    Ok(())
}

/// `agent-steward` und `RegistryProfile::AgentStewardship` sind ein
/// Sonderfall der beiden Deckungstests oben (Nachtrag K2): zwei ihrer
/// Werkzeuge (`agents.commit_proposal`, `agents.reject_proposal`) registriert
/// `crate::agent_definition_tools::AgentDefinitionToolProvider` zur
/// **Laufzeit** nur im Commit-Modus (Elternrolle der UIA) — im
/// Vorschlagsmodus (Elternrolle Root-Orchestrator oder unbekannt,
/// fail-closed) bleiben sie ungenutzt. Die statische Deckung zwischen TOML
/// und `RegistryProfile` kann diesen Laufzeitzustand nicht ausdrücken; sie
/// bleibt deshalb bewusst bei der **maximalen** (Commit-Modus-)Werkzeugmenge
/// maßgeblich — `agent-steward.toml` admittiert alle sechs
/// `agents.*`-Werkzeuge, `AgentStewardship::tool_names()` bewirbt sie
/// ebenfalls alle sechs, und die beiden generischen Tests oben decken damit
/// unverändert beide Richtungen ab, ohne dass die statische Prüfung den
/// Modus kennen müsste. Dieser Test macht die Absicht explizit, statt sie
/// stillschweigend an den beiden generischen Tests hängen zu lassen.
#[test]
fn agent_steward_admits_the_commit_mode_tool_set_even_though_two_tools_are_mode_gated_at_runtime()
-> TestResult {
    let roles = resolved_roles()?;
    let ir = roles
        .get(role_names::AGENT_STEWARD)
        .ok_or(TestError::Missing("agent-steward ist eingebaut"))?;
    // Runde 5, Teil M: `parent.message` steuert die Composition-Root bei
    // (kein Profil-Werkzeug) — für den Profilvergleich ausgenommen.
    let admitted: BTreeSet<&str> = ir
        .tool_surface()
        .admitted()
        .iter()
        .map(String::as_str)
        .filter(|tool| !PARENT_MESSAGE_TOOLS.contains(tool))
        // Plan R9, Teil A: der Skill-Katalog wird unabhängig vom Profil montiert.
        .filter(|tool| !matches!(*tool, "skills.search" | "skills.load"))
        .collect();

    let profile = profile_for_role(role_names::AGENT_STEWARD)
        .ok_or(TestError::Missing("agent-steward braucht ein Profil"))?;
    assert_eq!(profile, RegistryProfile::AgentStewardship);
    let advertised: BTreeSet<&str> = profile.tool_names().into_iter().collect();

    assert_eq!(
        admitted, advertised,
        "agent-steward.toml muss exakt die Commit-Modus-Werkzeugmenge \
         admittieren, auch wenn zwei ihrer Werkzeuge im Vorschlagsmodus zur \
         Laufzeit nicht registriert werden"
    );
    for mode_gated in ["agents.commit_proposal", "agents.reject_proposal"] {
        assert!(
            admitted.contains(mode_gated),
            "{mode_gated} fehlt in admitted"
        );
    }
    for always_on in [
        "agents.validate",
        "agents.list_proposals",
        "agents.write_definition",
        "agents.write_uia",
    ] {
        assert!(
            admitted.contains(always_on),
            "{always_on} fehlt in admitted"
        );
    }
    Ok(())
}

/// Nachtrag K3 (Welle FANIN-K, Fan-in-Zusatzpunkte): schärft den Test oben um
/// den tatsächlichen Laufzeit-**Standard**zustand — ohne eine vom Aufrufer
/// gesetzte `AgentDefinitionAccess` (der fail-closed Standardpfad über
/// `assemble_registry_for_project`/`assemble_registry_for_sandbox` ohne
/// `..._with_definition_access`) registriert `AgentDefinitionToolProvider`
/// **nur** die beiden lesenden Werkzeuge — unabhängig davon, dass
/// `agent-steward.toml` und `RegistryProfile::AgentStewardship::tool_names()`
/// (Test oben) weiterhin die volle Vertrags-Obermenge admittieren/bewerben.
/// Mit einer gesetzten Decke und `DefinitionWriteMode::Commit` registriert
/// derselbe Provider dagegen genau diese Obermenge — siehe
/// `harw-registry-defaults/src/profile.rs::
/// test_agent_stewardship_registers_all_six_tools_with_a_commit_ceiling` für
/// den End-to-End-Beleg über die montierte Registry.
#[test]
fn agent_definition_tool_provider_without_access_registers_only_read_and_list_tools() {
    let names = harw_registry_defaults::agent_definition_tool_names_for_access(None);
    assert_eq!(
        names,
        vec![
            "agents.validate",
            "agents.list_proposals",
            "skills.validate",
            "skills.list_proposals",
        ]
    );
}

/// Nutzerentscheidung „die UIA-Helfer recherchieren kurz online und fügen
/// manchmal Abhängigkeiten hinzu“: `uia-worker` und `uia-writer` admittieren
/// alle vier `web.*`-Werkzeuge (`web.fetch`/`web.search` plus die
/// Crate-Werkzeuge `web.docs_rs`/`web.crates_io`, `UIA_HELPER_WEB_TOOLS`) und
/// die fünf lesenden `deps.*`-Werkzeuge — und ihr Profil (`UiaQuickHelper`
/// bzw. `UiaWriter`) registriert/bewirbt genau diese auch, sonst wäre der
/// TOML-Eintrag toter Text. `lens.ask` bleibt beiden verboten; das Netz
/// selbst bindet weiterhin die Egress-Policy des Elternteils.
#[test]
fn uia_helpers_admit_and_register_web_search_and_read_only_deps_tools() -> TestResult {
    let roles = resolved_roles()?;
    const RESEARCH_TOOLS: [&str; 9] = [
        "web.fetch",
        "web.docs_rs",
        "web.crates_io",
        "web.search",
        "deps.graph",
        "deps.locked",
        "deps.source_read",
        "deps.source_search",
        "deps.source_list",
    ];
    for (role, expected_profile) in [
        (role_names::UIA_WORKER, RegistryProfile::UiaQuickHelper),
        (role_names::UIA_WRITER, RegistryProfile::UiaWriter),
    ] {
        let ir = roles
            .get(role)
            .ok_or(TestError::Unexpected(format!("{role} fehlt")))?;
        let profile =
            profile_for_role(role).ok_or(TestError::Unexpected(format!("{role} ohne Profil")))?;
        assert_eq!(profile, expected_profile, "{role}");
        let advertised: BTreeSet<&str> = profile.tool_names().into_iter().collect();
        let admitted: BTreeSet<&str> = ir
            .tool_surface()
            .admitted()
            .iter()
            .map(String::as_str)
            .collect();
        let forbidden: BTreeSet<&str> = ir
            .tool_surface()
            .forbidden()
            .iter()
            .map(String::as_str)
            .collect();
        for tool in RESEARCH_TOOLS {
            assert!(admitted.contains(tool), "{role} muss {tool} admittieren");
            assert!(
                advertised.contains(tool),
                "{profile:?} muss {tool} registrieren"
            );
            assert!(!forbidden.contains(tool), "{role} verbietet {tool}");
        }
        assert!(!admitted.contains("lens.ask"), "{role} admittiert lens.ask");
        assert!(
            !advertised.contains("lens.ask"),
            "{profile:?} registriert lens.ask"
        );
        assert!(
            forbidden.contains("lens.ask"),
            "{role} muss lens.ask verbieten"
        );
    }
    Ok(())
}

/// Plan Punkt 1: jede Orchestrator-Rolle admittiert die Werkzeuge, die die
/// Composition-Root für sie beisteuert (`delegate_wave`) — sonst hängt die
/// Kind-Registry einen Provider an, dessen Werkzeug die `SessionActivation`
/// (deny-by-default aus `admitted`) nie freischaltet. Umgekehrt admittiert
/// kein Worker ein solches Werkzeug.
#[test]
fn orchestrators_admit_exactly_their_composition_tools() -> TestResult {
    let roles = resolved_roles()?;
    for role in role_names::ALL {
        let ir = roles
            .get(*role)
            .ok_or(TestError::Unexpected(format!("{role} fehlt")))?;
        let admitted: BTreeSet<&str> = ir
            .tool_surface()
            .admitted()
            .iter()
            .map(String::as_str)
            .collect();
        let granted = composition_tools_for_role(role);
        for tool in granted {
            assert!(admitted.contains(tool), "{role} muss {tool} admittieren");
        }
        for tool in harw_registry_defaults::profile::ORCHESTRATION_TOOLS {
            if !granted.contains(tool) {
                assert!(
                    !admitted.contains(tool),
                    "{role} ist kein Orchestrator und darf {tool} nicht admittieren"
                );
            }
        }
    }
    Ok(())
}

/// Runde 5, Teil H: jede Rolle, die Kinder starten darf, admittiert
/// `agent.result` — sonst hängt die Kind-Registry einen Provider an, den die
/// `SessionActivation` nie freischaltet, und die Kürzungsmarke verwiese ins
/// Leere. Umgekehrt admittiert keine andere Rolle das Werkzeug.
#[test]
fn spawning_roles_admit_exactly_their_child_result_tools() -> TestResult {
    let roles = resolved_roles()?;
    for role in role_names::ALL {
        let ir = roles
            .get(*role)
            .ok_or(TestError::Unexpected(format!("{role} fehlt")))?;
        let admitted: BTreeSet<&str> = ir
            .tool_surface()
            .admitted()
            .iter()
            .map(String::as_str)
            .collect();
        let granted = child_result_tools_for_role(role);
        // Wer `delegate_wave` bekommt, startet Kinder und braucht auch
        // `agent.result`.
        assert_eq!(
            granted.is_empty(),
            composition_tools_for_role(role).is_empty(),
            "{role}: agent.result folgt der Delegationsfläche"
        );
        for tool in CHILD_RESULT_TOOLS {
            assert_eq!(
                admitted.contains(tool),
                granted.contains(tool),
                "{role}: admitted({tool}) muss child_result_tools_for_role entsprechen"
            );
        }
    }
    Ok(())
}

/// Runde 3, Welle E + Matrix-Unterlagen: die drei Matrix-Game-Sitze
/// admittieren genau die lesenden Unterlagen-Werkzeuge (fünf lesende `fs.*`
/// plus `doc.read_pdf`), und ihr Profil (`MatrixReader`) bewirbt genau
/// diese — Inventar und Aufrufrecht bleiben deckungsgleich, ohne Netz,
/// Schreiben oder Exec.
#[test]
fn matrix_roles_admit_and_advertise_exactly_the_read_tools() -> TestResult {
    let expected: BTreeSet<&str> = [
        "fs.read",
        "fs.list",
        "fs.search",
        "fs.glob",
        "fs.grep",
        "doc.read_pdf",
    ]
    .into();
    let roles = resolved_roles()?;
    for role in role_names::MATRIX_ROLES {
        let ir = roles
            .get(role)
            .ok_or(TestError::Unexpected(format!("{role} fehlt")))?;
        let profile =
            profile_for_role(role).ok_or(TestError::Unexpected(format!("{role} ohne Profil")))?;
        assert_eq!(profile, RegistryProfile::MatrixReader, "{role}");
        let advertised: BTreeSet<&str> = profile.tool_names().into_iter().collect();
        assert_eq!(advertised, expected, "{role}: {profile:?}");
        assert!(
            composition_tools_for_role(role).is_empty(),
            "{role} ist kein Orchestrator"
        );
        let admitted: BTreeSet<&str> = ir
            .tool_surface()
            .admitted()
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(admitted, expected, "{role} admittiert {admitted:?}");
        for tool in &admitted {
            assert!(
                *tool != "fs.write"
                    && *tool != "fs.edit"
                    && !tool.starts_with("shell.")
                    && !tool.starts_with("process.")
                    && !tool.starts_with("web.")
                    && !tool.starts_with("browser."),
                "{role} admittiert {tool}"
            );
        }
        assert_eq!(ir.spawn_contract().max_depth(), Some(0), "{role}");
    }
    Ok(())
}

/// Runde 3, Welle D: `RegistryProfile::WorkspaceEdit` (Telegram mit
/// Workspace) bewirbt Lese- und Schreibwerkzeuge, aber nie `shell.*`,
/// `process.*`, `web.*`, `browser.*` oder `lens.ask` — und verlangt genau
/// `{ReadWorkspace, WriteWorkspace}`. Keine eingebaute Rolle bekommt es.
#[test]
fn workspace_edit_profile_never_includes_shell_or_web_tools() {
    use harw_authority::{Permission, PermissionSet};

    let tools = RegistryProfile::WorkspaceEdit.tool_names();
    assert!(tools.contains(&"fs.write"), "{tools:?}");
    assert!(tools.contains(&"fs.edit"), "{tools:?}");
    assert!(tools.contains(&"fs.read"), "{tools:?}");
    for tool in &tools {
        assert!(
            !tool.starts_with("shell.")
                && !tool.starts_with("process.")
                && !tool.starts_with("web.")
                && !tool.starts_with("browser.")
                && *tool != "lens.ask",
            "WorkspaceEdit bewirbt {tool}"
        );
    }
    assert_eq!(
        RegistryProfile::WorkspaceEdit.required_permissions(),
        // Kein `ExecuteProcess`: das Recht für `cargo.test_one` vergibt nur
        // `granted_for_capabilities` (nur `test-engineer`).
        PermissionSet::from_policy([Permission::ReadWorkspace, Permission::WriteWorkspace])
    );
    let cargo: Vec<&&str> = tools.iter().filter(|t| t.starts_with("cargo.")).collect();
    assert_eq!(cargo, [&"cargo.test_one"], "{tools:?}");
    for role in role_names::ALL {
        assert_ne!(
            profile_for_role(role),
            Some(RegistryProfile::WorkspaceEdit),
            "{role} darf WorkspaceEdit nicht bekommen"
        );
    }
}

/// Runde 4, Teil E: `uia-latex-writer` admittiert genau lesende `fs.*`,
/// `fs.write`/`fs.edit`, `doc.read_pdf` und das typisierte `latex.build`
/// (Runde 7, Teil T5: dazu `latex.template` und `latex.check`) — und sein
/// Profil (`UiaLatexWriter`) registriert/bewirbt genau diese Menge. Freie
/// Shell, Prozesswerkzeuge, Netz, `deps.*` und `lens.ask` sind ausdrücklich
/// verboten; Rückgabevertrag `execution-summary`, keine Spawn-Tiefe.
#[test]
fn uia_latex_writer_admits_exactly_files_pdf_and_latex_build() -> TestResult {
    let roles = resolved_roles()?;
    let role = role_names::UIA_LATEX_WRITER;
    let ir = roles
        .get(role)
        .ok_or(TestError::Unexpected(format!("{role} fehlt")))?;
    let profile =
        profile_for_role(role).ok_or(TestError::Unexpected(format!("{role} ohne Profil")))?;
    assert_eq!(profile, RegistryProfile::UiaLatexWriter);
    let expected: BTreeSet<&str> = [
        "fs.read",
        "fs.write",
        "fs.edit",
        "fs.list",
        "fs.search",
        "fs.glob",
        "fs.grep",
        "doc.read_pdf",
        "latex.build",
        "latex.template",
        "latex.check",
    ]
    .into();
    let admitted: BTreeSet<&str> = ir
        .tool_surface()
        .admitted()
        .iter()
        .map(String::as_str)
        .collect();
    let advertised: BTreeSet<&str> = profile.tool_names().into_iter().collect();
    // Runde 5, Teil M: zusätzlich `parent.message` aus der Composition-Root
    // (reiner Text an den Elternteil, keine Rechteklasse).
    let mut expected_admitted = expected.clone();
    expected_admitted.extend(PARENT_MESSAGE_TOOLS.iter().copied());
    // Plan R9, Teil A: dazu der Skill-Katalog (ohne Rechteklasse).
    expected_admitted.extend(SKILL_CATALOG_TOOLS.iter().copied());
    assert_eq!(admitted, expected_admitted, "{role}: admitted");
    assert_eq!(advertised, expected, "{profile:?}: beworben");
    let forbidden: BTreeSet<&str> = ir
        .tool_surface()
        .forbidden()
        .iter()
        .map(String::as_str)
        .collect();
    for tool in [
        "shell.exec",
        "process.kill",
        "web.fetch",
        "web.search",
        "web.docs_rs",
        "web.crates_io",
        "deps.graph",
        "deps.source_read",
        "lens.ask",
    ] {
        assert!(forbidden.contains(tool), "{role} muss {tool} verbieten");
    }
    assert_eq!(ir.spawn_contract().max_depth(), Some(0), "{role}");
    assert_eq!(
        ir.return_pipeline().contract(),
        Some("harwness.return.execution-summary@1"),
        "{role}"
    );
    // Runde 7, Teil T5 / Plan R9: nur `latex-report` (Vorlage, Werkzeuge,
    // Übergabe) ist fest gebunden; Handwerk, Build und Pyramide lädt der
    // Writer bei Bedarf über `skills.load`. Das Budget ist realistisch.
    assert_eq!(ir.skills(), ["latex-report"], "{role}: skills");
    let budget = ir
        .spawn_contract()
        .budget()
        .ok_or(TestError::Unexpected(format!("{role} ohne [spawn.budget]")))?;
    assert_eq!(budget.max_tokens(), Some(150_000), "{role}");
    assert_eq!(budget.max_wall_secs(), Some(600), "{role}");
    assert_eq!(budget.max_tool_calls(), Some(60), "{role}");
    Ok(())
}

/// Plan Teil D („Agenten dürfen Workbench, Palace und Diary lesen“): jede
/// Rolle admittiert genau die lesenden Wissenswerkzeuge, die
/// `profile::knowledge_tools_for_role` ihr anbietet — sonst montiert die
/// Kind-Registry einen Provider, dessen Werkzeug die deny-by-default-
/// Aktivierung nie freischaltet —, und keines davon ist verboten. Kanban
/// (`kanban.list`/`kanban.show`, nur auf ausdrücklichen Wunsch der Nutzerin)
/// admittiert ausschließlich der Root-Orchestrator.
#[test]
fn roles_admit_exactly_their_offered_knowledge_read_tools() -> TestResult {
    let roles = resolved_roles()?;
    for role in role_names::ALL {
        let ir = roles
            .get(*role)
            .ok_or(TestError::Unexpected(format!("{role} fehlt")))?;
        let admitted: BTreeSet<&str> = ir
            .tool_surface()
            .admitted()
            .iter()
            .map(String::as_str)
            .collect();
        let forbidden: BTreeSet<&str> = ir
            .tool_surface()
            .forbidden()
            .iter()
            .map(String::as_str)
            .collect();
        let offered = knowledge_tools_for_role(role);
        for tool in KNOWLEDGE_READ_TOOLS.iter().chain(KANBAN_READ_TOOLS) {
            assert_eq!(
                admitted.contains(tool),
                offered.contains(tool),
                "{role}: {tool} admittiert = {}, angeboten = {}",
                admitted.contains(tool),
                offered.contains(tool)
            );
            assert!(!forbidden.contains(tool), "{role} verbietet {tool}");
        }
        for tool in KANBAN_READ_TOOLS {
            assert_eq!(
                admitted.contains(tool),
                *role == role_names::ROOT_ORCHESTRATOR,
                "{role}: {tool} nur für den Root-Orchestrator"
            );
        }
    }
    Ok(())
}

/// Runde 5, Teil B: `host.sudo_exec` admittieren genau die beiden
/// Host-Shell-Worker (`uia-shell-worker`, `host-process-worker`); keine
/// andere eingebaute Rolle admittiert es.
#[test]
fn only_the_host_shell_workers_admit_host_sudo_exec() -> TestResult {
    let roles = resolved_roles()?;
    // `host-process-worker` ist (noch) keine spawnbare Rolle und fehlt deshalb
    // in `resolved_roles()`; geprüft wird jede Sudo-Rolle, die aufgelöst wird
    // — mindestens aber `uia-shell-worker`.
    assert!(
        roles.contains_key("uia-shell-worker"),
        "uia-shell-worker fehlt"
    );
    for role in SUDO_ROLES {
        let Some(ir) = roles.get(*role) else {
            continue;
        };
        for tool in SUDO_TOOLS {
            assert!(
                ir.tool_surface().admitted().iter().any(|name| name == tool),
                "{role} muss {tool} admittieren"
            );
        }
    }
    for (role, ir) in &roles {
        if SUDO_ROLES.contains(&role.as_str()) {
            continue;
        }
        for tool in SUDO_TOOLS {
            assert!(
                !ir.tool_surface().admitted().iter().any(|name| name == tool),
                "{role} darf {tool} nicht admittieren"
            );
        }
    }
    Ok(())
}

/// Runde 5, Teil F: `plan.write`, `plan.exit`, `plan.enter` und `ask_user`
/// gehören ausschließlich der Wurzel (Composition-Root in `harw-runtime`,
/// nur mit TUI-Kanal wirksam). Keine eingebaute Rolle admittiert oder
/// bewirbt sie — ein Kind kann sie nie aufrufen.
#[test]
fn no_builtin_role_admits_or_advertises_the_root_only_plan_tools() -> TestResult {
    let roles = resolved_roles()?;
    for (role, ir) in &roles {
        for tool in harw_tool_plan::PlanToolProvider::TOOL_NAMES {
            assert!(
                !ir.tool_surface().admitted().iter().any(|name| name == tool),
                "{role} darf {tool} nicht admittieren (nur Wurzel)"
            );
            assert!(
                !composition_tools_for_role(role).contains(tool)
                    && !knowledge_tools_for_role(role).contains(tool)
                    && !sudo_tools_for_role(role).contains(tool),
                "{role} darf {tool} nicht über die Composition-Root bekommen"
            );
            if let Some(profile) = profile_for_role(role) {
                assert!(
                    !profile.tool_names().contains(tool),
                    "{profile:?} ({role}) darf {tool} nicht registrieren"
                );
            }
        }
    }
    Ok(())
}

/// Runde 5, Teil F, Punkt 4: im Plan-Modus startet der Agent nur
/// schreibgeschützte Kinder (`explore`/`research*` → Explorer- und
/// Researcher-Rollen). Weder ihr Profil noch ihre TOML enthält ein
/// schreibendes oder ausführendes Werkzeug.
#[test]
fn plan_mode_child_roles_are_read_only() -> TestResult {
    use harw_authority::Permission;
    use harw_registry_defaults::authority::tool_permission;

    let roles = resolved_roles()?;
    let is_mutating = |tool: &str| {
        matches!(
            tool_permission(tool),
            Some(Permission::WriteWorkspace | Permission::ExecuteProcess)
        ) || matches!(
            tool,
            "fs.write" | "fs.edit" | "shell.exec" | "host.sudo_exec" | "plan.write"
        )
    };
    for role in [
        role_names::EXPLORER,
        role_names::UIA_EXPLORER,
        role_names::RESEARCHER,
        role_names::RESEARCHER_DEPS,
        role_names::RESEARCHER_WEB,
        role_names::DEPENDENCY_RESEARCHER,
    ] {
        if let Some(profile) = profile_for_role(role) {
            for tool in profile.tool_names() {
                assert!(!is_mutating(tool), "{role}: Profil registriert {tool}");
            }
        }
        if let Some(ir) = roles.get(role) {
            for tool in ir.tool_surface().admitted() {
                assert!(
                    !is_mutating(tool.as_str()),
                    "{role}: TOML admittiert {}",
                    tool.as_str()
                );
            }
        }
    }
    Ok(())
}

/// Runde 5, Teil M: jede Rolle, die Kinder starten darf, admittiert
/// `agent.message`; jede Kind-Rolle aus `PARENT_MESSAGE_ROLES` admittiert
/// `parent.message` — sonst hängt die Kind-Registry einen Provider an, den
/// die `SessionActivation` nie freischaltet. Umgekehrt admittiert keine
/// andere Rolle die Werkzeuge.
#[test]
fn roles_admit_exactly_their_messaging_tools() -> TestResult {
    let roles = resolved_roles()?;
    for role in role_names::ALL {
        let ir = roles
            .get(*role)
            .ok_or(TestError::Unexpected(format!("{role} fehlt")))?;
        let admitted: BTreeSet<&str> = ir
            .tool_surface()
            .admitted()
            .iter()
            .map(String::as_str)
            .collect();
        for tool in CHILD_MESSAGE_TOOLS {
            assert_eq!(
                admitted.contains(tool),
                child_message_tools_for_role(role).contains(tool),
                "{role}: admitted({tool}) muss child_message_tools_for_role entsprechen"
            );
        }
        for tool in PARENT_MESSAGE_TOOLS {
            assert_eq!(
                admitted.contains(tool),
                parent_message_tools_for_role(role).contains(tool),
                "{role}: admitted({tool}) muss parent_message_tools_for_role entsprechen"
            );
        }
    }
    Ok(())
}

/// Runde 7, Teil M: der Game Master admittiert genau die lesenden
/// Unterlagen-Werkzeuge, seine fünf Matrix-Werkzeuge und `parent.message` —
/// kein Schreiben, keine Ausführung, kein Netz, keine Delegation; keine
/// andere Rolle admittiert ein `matrix.*`-Werkzeug.
#[test]
fn game_master_admits_exactly_its_matrix_tools() -> TestResult {
    let roles = resolved_roles()?;
    let role = role_names::MATRIX_GAME_MASTER;
    let ir = roles
        .get(role)
        .ok_or(TestError::Unexpected(format!("{role} fehlt")))?;
    let admitted: BTreeSet<&str> = ir
        .tool_surface()
        .admitted()
        .iter()
        .map(String::as_str)
        .collect();
    let expected: BTreeSet<&str> = [
        "fs.read",
        "fs.list",
        "fs.search",
        "fs.glob",
        "fs.grep",
        "doc.read_pdf",
        "parent.message",
    ]
    .into_iter()
    .chain(MATRIX_GAME_MASTER_TOOLS.iter().copied())
    // Plan R9, Teil A: Skill-Katalog (ohne Rechteklasse).
    .chain(SKILL_CATALOG_TOOLS.iter().copied())
    .collect();
    assert_eq!(admitted, expected, "{role}");
    assert_eq!(profile_for_role(role), Some(RegistryProfile::MatrixReader));
    assert!(
        composition_tools_for_role(role).is_empty(),
        "kein delegate_wave"
    );
    let forbidden: BTreeSet<&str> = ir
        .tool_surface()
        .forbidden()
        .iter()
        .map(String::as_str)
        .collect();
    for tool in [
        "fs.write",
        "shell.exec",
        "web.fetch",
        "web.search",
        "transfer_to_executor",
    ] {
        assert!(forbidden.contains(tool), "{role} muss {tool} verbieten");
    }
    assert_eq!(ir.spawn_contract().max_depth(), Some(1), "{role}");
    for other in role_names::ALL.iter().filter(|r| **r != role) {
        let ir = roles
            .get(*other)
            .ok_or(TestError::Unexpected(format!("{other} fehlt")))?;
        assert!(
            !ir.tool_surface()
                .admitted()
                .iter()
                .any(|t| t.starts_with("matrix.")),
            "{other} darf kein matrix.*-Werkzeug admittieren"
        );
        assert!(matrix_tools_for_role(other).is_empty(), "{other}");
    }
    Ok(())
}

/// Plan R9, Teil A: jede eingebaute Rolle außer den dokumentierten
/// Ausnahmen (werkzeuglose Security-Triage, Matrix-Sitze) admittiert
/// `skills.search` und `skills.load` und verbietet keines davon — sonst hängt
/// die Kind-Registry den Katalog-Provider an, den die deny-by-default-
/// Aktivierung nie freischaltet, und der Agent könnte Skills nur raten. Die
/// Ausnahmen admittieren keines. Beide Werkzeuge gehören zu keinem
/// `RegistryProfile` und tragen keine Sandbox-Rechteklasse.
#[test]
fn every_role_admits_the_skill_catalog_tools_except_the_documented_ones() -> TestResult {
    let roles = resolved_roles()?;
    for role in role_names::ALL {
        let ir = roles
            .get(*role)
            .ok_or(TestError::Unexpected(format!("{role} fehlt")))?;
        let excluded = SKILL_CATALOG_EXCLUDED_ROLES.contains(role);
        assert_eq!(
            skill_catalog_tools_for_role(role).is_empty(),
            excluded,
            "{role}: Angebot"
        );
        for tool in SKILL_CATALOG_TOOLS {
            assert_eq!(
                ir.tool_surface().admitted().iter().any(|name| name == tool),
                !excluded,
                "{role}: {tool} admittiert"
            );
            assert!(
                !ir.tool_surface()
                    .forbidden()
                    .iter()
                    .any(|name| name == tool),
                "{role} verbietet {tool}"
            );
            assert_eq!(
                harw_registry_defaults::tool_permission(tool),
                None,
                "{tool} trägt keine Rechteklasse"
            );
            if let Some(profile) = profile_for_role(role) {
                assert!(
                    !profile.tool_names().contains(tool),
                    "{profile:?} ({role}) darf {tool} nicht selbst registrieren"
                );
            }
        }
    }
    for role in [
        role_names::UIA_WORKER,
        role_names::UIA_WRITER,
        role_names::UIA_EXPLORER,
        role_names::UIA_SHELL_WORKER,
        role_names::UIA_LATEX_WRITER,
        role_names::ROOT_ORCHESTRATOR,
        role_names::CODING_ORCHESTRATOR,
        role_names::RESEARCH_ORCHESTRATOR,
        role_names::ANALYSIS_ORCHESTRATOR,
        role_names::AGENT_STEWARD,
        role_names::MEMORY_STEWARD,
        role_names::EXPLORER,
        role_names::EXECUTOR,
        role_names::MATRIX_GAME_MASTER,
    ] {
        assert_eq!(
            skill_catalog_tools_for_role(role),
            SKILL_CATALOG_TOOLS,
            "{role}"
        );
    }
    Ok(())
}

/// Plan R9, Teil A/E5: jede Organisationsrolle erfährt aus ihrem Regelwerk,
/// dass Skills nur über `skills.search` gefunden und mit `skills.load`
/// geladen werden — nie über das Dateisystem, nie „gibt es nicht“ ohne Suche.
#[test]
fn every_organizational_role_knows_how_to_find_and_load_skills() {
    use harw_agent_dsl::roles::AgentRoleId;
    use harw_registry_defaults::embedded_agents::builtin_organization_knowledge;

    for role in [
        AgentRoleId::UserInterface,
        AgentRoleId::RootOrchestrator,
        AgentRoleId::ChildOrchestrator,
        AgentRoleId::Worker,
        AgentRoleId::UiaWorker,
        AgentRoleId::AgentSteward,
    ] {
        let text = builtin_organization_knowledge(role);
        for needle in [
            "`skills.search`",
            "`skills.load`",
            "nie im Dateisystem suchen",
        ] {
            assert!(text.contains(needle), "{role:?}: fehlt {needle}");
        }
    }
}
