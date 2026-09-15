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
use harw_registry_defaults::profile::{RegistryProfile, profile_for_role, role_names};

/// Lädt die aufgelösten eingebauten Rollendefinitionen (TOML-Seite).
///
/// Keine lokalen Überschreibungen (`existing` bleibt leer) — dieser Test
/// prüft ausschließlich die eingebetteten Definitionen unter
/// `harw-registry-defaults/agents/`.
fn resolved_roles() -> HashMap<String, harw_agent_dsl::ExecutableAgentIr> {
    builtin_agent_definitions(&HashMap::new())
        .expect("eingebaute Rollendefinitionen müssen sich auflösen lassen")
}

/// Jedes von einer Rolle `admitted` Werkzeug muss vom Profil der Rolle
/// registriert/beworben sein — sonst admittiert eine TOML ein Werkzeug, das
/// der Agent laut Registrierung nie im Inventar sieht: toter, irreführender
/// Text in der `admitted`-Liste.
#[test]
fn every_role_admitted_tool_is_registered_by_its_profile() {
    let roles = resolved_roles();

    for role in role_names::ALL {
        let ir = roles
            .get(*role)
            .unwrap_or_else(|| panic!("Rolle {role} fehlt in den aufgelösten Definitionen"));
        let profile = profile_for_role(role)
            .unwrap_or_else(|| panic!("eingebaute Rolle {role} braucht ein Profil"));
        let advertised: BTreeSet<&str> = profile.tool_names().into_iter().collect();

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
fn every_profile_registered_tool_is_admitted_by_its_role() {
    let roles = resolved_roles();

    for role in role_names::ALL {
        let ir = roles
            .get(*role)
            .unwrap_or_else(|| panic!("Rolle {role} fehlt in den aufgelösten Definitionen"));
        let profile = profile_for_role(role)
            .unwrap_or_else(|| panic!("eingebaute Rolle {role} braucht ein Profil"));
        let admitted: BTreeSet<&str> =
            ir.tool_surface().admitted().iter().map(String::as_str).collect();

        for tool in profile.tool_names() {
            assert!(
                admitted.contains(tool),
                "Rolle {role} ({profile:?}) sieht {tool} in ihrem beworbenen \
                 Werkzeuginventar, aber agents/{role}.toml admittiert es nicht \
                 — die Rolle würde an einer Rückfrage für {tool} hängen \
                 bleiben, die sie mit allow_pause = false nie beantworten kann"
            );
        }
    }
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
fn profile_tools_unclaimed_by_any_role_match_the_documented_expectation() {
    let roles = resolved_roles();

    let mut admitted_anywhere: BTreeSet<String> = BTreeSet::new();
    for role in role_names::ALL {
        let ir = roles.get(*role).expect("Rolle geladen");
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
{
    let roles = resolved_roles();
    let ir = roles
        .get(role_names::AGENT_STEWARD)
        .expect("agent-steward ist eingebaut");
    let admitted: BTreeSet<&str> =
        ir.tool_surface().admitted().iter().map(String::as_str).collect();

    let profile = profile_for_role(role_names::AGENT_STEWARD)
        .expect("agent-steward braucht ein Profil");
    assert_eq!(profile, RegistryProfile::AgentStewardship);
    let advertised: BTreeSet<&str> = profile.tool_names().into_iter().collect();

    assert_eq!(
        admitted, advertised,
        "agent-steward.toml muss exakt die Commit-Modus-Werkzeugmenge \
         admittieren, auch wenn zwei ihrer Werkzeuge im Vorschlagsmodus zur \
         Laufzeit nicht registriert werden"
    );
    for mode_gated in ["agents.commit_proposal", "agents.reject_proposal"] {
        assert!(admitted.contains(mode_gated), "{mode_gated} fehlt in admitted");
    }
    for always_on in ["agents.validate", "agents.list_proposals", "agents.write_definition", "agents.write_uia"] {
        assert!(admitted.contains(always_on), "{always_on} fehlt in admitted");
    }
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
    assert_eq!(names, vec!["agents.validate", "agents.list_proposals"]);
}
