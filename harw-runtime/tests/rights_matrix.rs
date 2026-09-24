//! Tabellengetriebener Rechte-Snapshot je [`EntryKind`].
//!
//! # Beschreibung
//! Der Vertrag `docs/design/runtime-contracts.md` §runtime-spec nennt für jeden
//! der elf Einstiege genau eine Zeile: Rechte, Registry-Profil, Ask-Auflösung,
//! Spawner, Decke. `harw-runtime/src/spec.rs` prüft, dass
//! [`EntryKind::profile`] diese Tabelle wiedergibt — das ist die *Deklaration*.
//! Diese Datei prüft die *Wirkung*: Was hat ein **tatsächlich montierter**
//! Lauf am Ende in der Hand?
//!
//! Der Unterschied ist der eigentliche Befund der Welle. Vorher deklarierten
//! neun Einstiege ihre Rechte und montierten daneben etwas anderes — z. B.
//! `PermissionSet::from_policy([Read, Write, Execute])` literal für einen
//! Einstieg, dem der Vertrag `{}` zuspricht. Ein Test auf der Deklaration
//! hätte das nie bemerkt.
//!
//! # Aufbau
//! Ein Tempverzeichnis je Lauf (Projekt-Marker `Cargo.toml`, eigener
//! Root-Space), [`ModelSource::Echo`] als Modell — kein Netz, kein Anbieter,
//! kein Geheimnis.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::{TestError, TestResult, ctx};
use harw_agent_dsl::roles::AgentRoleId;
use harw_core::{ChildRegistryFactory, EchoModelProvider, InMemoryStateStore, StateStore};
use harw_extension_api::allow_rules::AllowRuleSet;
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_extension_api::{AgentSpawner, ApprovalDecision, ApprovalHandler, ExtFuture, ToolCall};
use harw_operations::operation::Surface;
use harw_registry_defaults::profile::role_names;
use harw_runtime::approval::{ApprovalChain, AskResolutionPolicy, DEFAULT_POLICY_LABEL};
use harw_runtime::assembly::{RuntimeAssembly, RuntimeStores, SessionLifecycleHook};
use harw_runtime::children::RuntimeChildRegistryFactory;
use harw_runtime::contributors::{AssemblyContributor, AssemblyInputs, AssemblyParts};
use harw_runtime::error::{RuntimeError, RuntimeResult};
use harw_runtime::model::ModelSource;
use harw_runtime::spec::{
    AskResolution, CeilingPolicy, EntryKind, OperationSurface, RuntimeSpec, SpawnerPolicy,
};
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind, SessionId, ToolCallId};
use tempfile::TempDir;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Alle elf Einstiege.
const ALL_ENTRIES: [EntryKind; 11] = [
    EntryKind::Tui,
    EntryKind::OneShot,
    EntryKind::LocalEcho,
    EntryKind::Analyze,
    EntryKind::Doctor,
    EntryKind::Web,
    EntryKind::McpServe,
    EntryKind::JobPrompt,
    EntryKind::JobPlanNode,
    EntryKind::GatewayTelegram,
    EntryKind::GatewayDream,
];

/// Ein leeres Projekt mit eigenem Root-Space.
struct Fixture {
    _dir: TempDir,
    home: PathBuf,
    project: PathBuf,
}

fn fixture() -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home).map_err(ctx("home"))?;
    std::fs::create_dir_all(&project).map_err(ctx("project"))?;
    // Projekt-Marker, damit `discover_project` genau hier stehen bleibt.
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n").map_err(ctx("marker"))?;
    write_fixture_uia(&home)?;
    Ok(Fixture {
        _dir: dir,
        home,
        project,
    })
}

/// Legt eine minimale, gültige UIA (`role = "user-interface"`) im
/// Standardprofil des Test-`home` an und aktiviert sie über
/// `harness.active_uia_definition`.
///
/// # Beschreibung
/// `EntryKind::Tui` und `EntryKind::OneShot` montieren seit dem UIA-Vertrag
/// (`harw-runtime/src/assembly.rs::resolve_active_uia`) nur noch mit einer konfigurierten
/// UIA — fail-closed, ohne stillen Full-Tool-Fallback. Diese Tabellen-Tests
/// prüfen genau diese acht (bzw. elf) Einstiege in einem leeren
/// Tempverzeichnis, das ohne diese Funktion keine UIA kennt. Layout und
/// Inhalt spiegeln exakt `harw-cli/src/uia_bootstrap.rs::write_generated_uia`:
/// `<home>/profiles/default/agents/fixture-uia/definition.toml` plus
/// `<home>/profiles/default/config.toml` mit
/// `active_uia_definition = "<id>"` — das aktive Profil ohne
/// `active_profile`-Datei ist `"default"` (`harw_home::active_profile_name`).
/// Nur `Tui`/`OneShot` lesen `active_uia_definition` überhaupt
/// (`resolve_active_uia`); alle anderen Einstiege bleiben unverändert.
fn write_fixture_uia(home: &Path) -> TestResult {
    let profile_dir = home.join("profiles").join("default");
    let agent_dir = profile_dir.join("agents").join("fixture-uia");
    std::fs::create_dir_all(&agent_dir).map_err(ctx("fixture uia dir"))?;
    std::fs::write(
        agent_dir.join("definition.toml"),
        "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
    )
    .map_err(ctx("fixture uia definition"))?;
    std::fs::write(
        profile_dir.join("config.toml"),
        "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
    )
    .map_err(ctx("fixture profile config"))
}

fn spec_for(entry: EntryKind, fixture: &Fixture) -> RuntimeSpec {
    RuntimeSpec {
        entry,
        home: fixture.home.clone(),
        cwd: fixture.project.clone(),
        principal: Principal::trusted_ingress(
            PrincipalKind::Human,
            "rights-matrix",
            IngressSurface::Tui,
            PermissionTier::Owner,
        ),
        mode_override: None,
        active_agent: None,
        reasoning_effort: None,
        approval_override: None,
        model_override: None,
    }
}

/// Ein montierter Lauf samt der Kanäle, die ihn am Leben halten.
struct Assembled {
    assembly: RuntimeAssembly,
    events: UnboundedSender<SessionEventAlias>,
    _event_rx: UnboundedReceiver<SessionEventAlias>,
}

type SessionEventAlias = harw_protocol::events::SessionEvent;
type TurnEventAlias = harw_protocol::events::TurnEvent;

/// Montiert `entry` im Tempprojekt.
///
/// # Beschreibung
/// Ohne jeden Prozess-Zustand: das Arbeitsverzeichnis des Laufs steht in
/// [`RuntimeSpec::cwd`], und **kein** Montageschritt liest das des Prozesses.
/// `load_config` ruft `harw_home::config_layers_report_at(&spec.home,
/// &spec.cwd)` (`harw-runtime/src/config.rs`), die Erkennung bekommt
/// `&spec.cwd`, die Sandbox `&project.project_root`. Der frühere
/// `set_current_dir`/`cwd_lock`-Apparat war damit gegenstandslos und hat die
/// Tests nur serialisiert — schlimmer noch: ein Verzeichniswechsel wirkt auf
/// **alle** Threads desselben Testbinaries (Befund Z2c-04).
fn assemble(entry: EntryKind, fixture: &Fixture) -> Result<Assembled, RuntimeError> {
    let (events, event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventAlias>();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let built = RuntimeAssembly::builder(spec_for(entry, fixture))
        .model(ModelSource::Echo("echo: rights-matrix".to_owned()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .session_events(events.clone())
        .build();

    built.map(|assembly| Assembled {
        assembly,
        events,
        _event_rx: event_rx,
    })
}

/// Die Erwartung einer Tabellenzeile.
struct Expected {
    /// Sandbox-Rechte als sortierte `Debug`-Namen.
    permissions: &'static [&'static str],
    /// Ob die Werkzeugliste leer sein muss.
    tools_empty: bool,
    /// Die Kinds der Freigabekette in Auswertungsreihenfolge.
    approval_chain: &'static [ApprovalHandlerKind],
    /// Ob die Decke keine Sektion führen darf.
    ceiling_empty: bool,
    /// Ob der Spawner keine Rolle führen darf.
    spawner_empty: bool,
}

/// Die Erwartungstabelle. Das `match` ist erschöpfend: eine neue
/// [`EntryKind`]-Variante bricht diesen Test beim Kompilieren.
fn expected(entry: EntryKind) -> Expected {
    // Sortiert, wie `rights_snapshot` sie sortiert. `Tui`/`OneShot` tragen
    // Netz, weil die Vorgabe-Konfiguration eine nicht leere Egress-Allowlist
    // hat (`[research].network_allow_hosts`, Runde 3 Welle A2).
    const RWXN: &[&str] = &[
        "ExecuteProcess",
        "NetworkAccess",
        "ReadWorkspace",
        "WriteWorkspace",
    ];
    const RWX: &[&str] = &["ExecuteProcess", "ReadWorkspace", "WriteWorkspace"];
    const R: &[&str] = &["ReadWorkspace"];
    const RW: &[&str] = &["ReadWorkspace", "WriteWorkspace"];
    const NONE: &[&str] = &[];
    // Ohne `[policy].require_approval_for` entsteht keine Config-Politik
    // (W2B-03: ein Handler, der alles durchwinkt, wäre nur Rauschen); ohne
    // Responder — den erst `new_root_session` anhängt — bleibt die
    // Default-Politik allein. Nur `Tui` und `GatewayTelegram` (Freigabe-
    // Buttons) lösen Rückfragen interaktiv auf; jeder andere Einstieg trägt
    // zusätzlich seine `AskResolutionPolicy` ([`ApprovalHandlerKind::Other`],
    // Befund Z2c-02).
    const DEFAULT_ONLY: &[ApprovalHandlerKind] = &[ApprovalHandlerKind::DefaultPolicy];
    const DEFAULT_AND_ASK: &[ApprovalHandlerKind] = &[
        ApprovalHandlerKind::DefaultPolicy,
        ApprovalHandlerKind::Other,
    ];

    match entry {
        EntryKind::Tui => Expected {
            permissions: RWXN,
            tools_empty: false,
            approval_chain: DEFAULT_ONLY,
            ceiling_empty: false,
            spawner_empty: false,
        },
        EntryKind::OneShot => Expected {
            permissions: RWXN,
            tools_empty: false,
            approval_chain: DEFAULT_AND_ASK,
            ceiling_empty: false,
            spawner_empty: false,
        },
        EntryKind::Analyze => Expected {
            permissions: RWX,
            tools_empty: false,
            approval_chain: DEFAULT_AND_ASK,
            ceiling_empty: false,
            spawner_empty: false,
        },
        EntryKind::LocalEcho | EntryKind::Doctor => Expected {
            permissions: RWX,
            tools_empty: false,
            approval_chain: DEFAULT_AND_ASK,
            ceiling_empty: false,
            spawner_empty: true,
        },
        EntryKind::Web => Expected {
            permissions: R,
            tools_empty: false,
            approval_chain: DEFAULT_AND_ASK,
            ceiling_empty: true,
            spawner_empty: true,
        },
        EntryKind::JobPlanNode => Expected {
            permissions: RW,
            tools_empty: false,
            approval_chain: DEFAULT_AND_ASK,
            ceiling_empty: false,
            spawner_empty: true,
        },
        // Runde 3, Welle D: Telegram liest und schreibt im Workspace, ohne
        // Shell und ohne Netz; Rückfragen beantwortet die Person im Chat.
        EntryKind::GatewayTelegram => Expected {
            permissions: RW,
            tools_empty: false,
            approval_chain: DEFAULT_ONLY,
            ceiling_empty: true,
            spawner_empty: true,
        },
        EntryKind::McpServe | EntryKind::JobPrompt | EntryKind::GatewayDream => Expected {
            permissions: NONE,
            tools_empty: true,
            approval_chain: DEFAULT_AND_ASK,
            ceiling_empty: true,
            spawner_empty: true,
        },
    }
}

#[test]
fn every_entry_matches_its_row_of_the_rights_table() -> TestResult {
    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)
            .map_err(|error| TestError::Unexpected(format!("{entry:?} muss montieren: {error}")))?;
        let snapshot = assembled.assembly.rights_snapshot();
        let want = expected(entry);

        assert_eq!(snapshot.entry, entry);
        assert_eq!(snapshot.permissions, want.permissions, "Rechte {entry:?}");
        assert_eq!(
            snapshot.tools.is_empty(),
            want.tools_empty,
            "Werkzeuge {entry:?}: {:?}",
            snapshot.tools
        );
        let kinds: Vec<ApprovalHandlerKind> =
            snapshot.approval_chain.iter().map(|(_, k)| *k).collect();
        assert_eq!(kinds, want.approval_chain, "Freigabekette {entry:?}");
        assert_eq!(
            snapshot.ceiling_sections.is_empty(),
            want.ceiling_empty,
            "Decke {entry:?}: {:?}",
            snapshot.ceiling_sections
        );
        assert_eq!(
            snapshot.spawner_roles.is_empty(),
            want.spawner_empty,
            "Spawner {entry:?}: {:?}",
            snapshot.spawner_roles
        );
        assert!(
            snapshot.config_policy_tools.is_empty(),
            "ohne [policy].require_approval_for gibt es keine Config-Politik ({entry:?})"
        );
        assert!(
            snapshot.untrusted_repo.is_none(),
            "das Tempprojekt trägt kein .harw ({entry:?})"
        );
        assert_eq!(
            snapshot.approval_actor,
            assembled.assembly.principal().approval_actor()
        );
    }
    Ok(())
}

/// Runde 3, Welle A2: nur die Nutzeroberflächen (`Tui`, `OneShot`) tragen
/// Netz, und ihr Host-Scope ist genau die Egress-Allowlist der Konfiguration
/// ([`harw_runtime::sandbox::root_network_scope`]). Jeder andere Einstieg
/// bleibt ohne Netzrecht und ohne Hosts; Contributor-Scopes bleiben leer.
#[test]
fn only_the_user_interfaces_carry_egress_bound_network() -> TestResult {
    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)?;
        let snapshot = assembled.assembly.rights_snapshot();
        let sandbox = assembled.assembly.sandbox();
        let networked = matches!(entry, EntryKind::Tui | EntryKind::OneShot);
        assert_eq!(
            snapshot.permissions.iter().any(|p| p == "NetworkAccess"),
            networked,
            "{entry:?}: Netzrecht"
        );
        assert!(
            assembled.assembly.network_scope().is_empty(),
            "{entry:?}: kein Contributor-Scope"
        );
        let expected_scope =
            harw_runtime::sandbox::root_network_scope(entry, assembled.assembly.config());
        assert_eq!(sandbox.network_scope(), &expected_scope, "{entry:?}");
        if networked {
            // Vorgabe `[research].network_allow_hosts`.
            assert!(sandbox.network_scope().allows("docs.rs"), "{entry:?}");
            assert!(!sandbox.network_scope().allows("evil.example"), "{entry:?}");
        } else {
            assert!(sandbox.network_scope().is_empty(), "{entry:?}");
        }
    }
    Ok(())
}

/// Ohne Egress-Allowlist entsteht kein Host — auch nicht der Host des
/// Such-Backends —, und damit auch kein Netzrecht der Wurzel (fail-closed).
#[test]
fn without_an_allowlist_the_root_gets_no_hosts() -> TestResult {
    let mut config = harw_config::ResolvedConfig::default();
    config.network.allow_hosts.clear();
    config.network.researcher_web_hosts.clear();
    config.harness.research.network_allow_hosts.clear();
    let fixture = fixture()?;
    for entry in ALL_ENTRIES {
        let scope = harw_runtime::sandbox::root_network_scope(entry, &config);
        assert!(scope.is_empty(), "{entry:?}");
        assert_eq!(scope.hosts().count(), 0, "{entry:?}");
        let sandbox =
            harw_runtime::sandbox::root_sandbox_with_network(entry, &fixture.project, scope)
                .map_err(ctx("Sandbox bindet"))?;
        assert!(
            !sandbox
                .permissions()
                .contains(harw_authority::Permission::NetworkAccess),
            "{entry:?}: ohne Host kein Netzrecht"
        );
        assert!(sandbox.network_scope().is_empty(), "{entry:?}");
    }
    Ok(())
}

/// Runde 3, Welle D: ein montierter Telegram-Lauf liest und schreibt im
/// Workspace (`fs.write`), führt aber weder `shell.*`/`process.*` noch
/// `web.*`, trägt kein Netz, und sein Freigabemodus ist unabhängig von der
/// Konfiguration immer `ask`.
#[test]
fn telegram_reads_and_writes_without_shell_network_or_relaxed_approval() -> TestResult {
    use harw_extension_api::approval_mode::ApprovalMode;

    let fixture = fixture()?;
    let assembled = assemble(EntryKind::GatewayTelegram, &fixture)?;
    let snapshot = assembled.assembly.rights_snapshot();
    assert_eq!(snapshot.permissions, ["ReadWorkspace", "WriteWorkspace"]);
    assert!(assembled.assembly.sandbox().network_scope().is_empty());
    let tools = snapshot.tools;
    for expected in ["fs.read", "fs.write", "fs.edit"] {
        assert!(
            tools.iter().any(|tool| tool == expected),
            "{expected} fehlt: {tools:?}"
        );
    }
    for tool in &tools {
        assert!(
            !tool.starts_with("shell.")
                && !tool.starts_with("process.")
                && !tool.starts_with("web.")
                && tool != "lens.ask",
            "Telegram darf {tool} nicht führen: {tools:?}"
        );
    }
    // Lesen läuft ohne Button durch, jedes Schreiben fragt (erzwungen).
    assert_eq!(
        assembled.assembly.approval_mode().get(),
        ApprovalMode::Delegated
    );

    // Auch ein expliziter Aufrufer-Override lockert den Modus nicht.
    let mut spec = spec_for(EntryKind::GatewayTelegram, &fixture);
    spec.approval_override = Some(ApprovalMode::FullAccess);
    let relaxed = build_with_spec(spec).map_err(ctx("Telegram mit Override montiert"))?;
    assert_eq!(relaxed.approval_mode().get(), ApprovalMode::Delegated);
    Ok(())
}

/// Die Read-only-Rollen (analyst, researcher-deps, planner, die vier
/// security-*-triage-Rollen) bekommen auch unter einer vernetzten
/// UIA-Wurzel kein Netz, kein Schreiben und keine Ausführung: ihr Reducer
/// schneidet die Rechte der Wurzel entsprechend.
#[test]
fn read_only_roles_stay_network_free_under_a_networked_root() -> TestResult {
    use harw_authority::Permission;
    use harw_registry_defaults::authority::authority_reducer_for_role;

    let fixture = fixture()?;
    let assembled = assemble(EntryKind::Tui, &fixture)?;
    let root = assembled.assembly.sandbox().permissions().clone();
    assert!(
        root.contains(Permission::NetworkAccess),
        "precondition: die UIA-Wurzel trägt Netz"
    );
    for role in [
        role_names::ANALYST,
        role_names::RESEARCHER_DEPS,
        role_names::PLANNER,
        role_names::SECURITY_EGRESS_TRIAGE,
        role_names::SECURITY_BASELINE_TRIAGE,
        role_names::SECURITY_STRUCTURE_TRIAGE,
        role_names::SECURITY_ENDPOINT_TRIAGE,
    ] {
        let reducer = authority_reducer_for_role(role).ok_or(TestError::Missing("reducer"))?;
        let child = reducer.reduce(&root);
        for forbidden in [
            Permission::NetworkAccess,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ] {
            assert!(!child.contains(forbidden), "{role}: {forbidden:?}");
        }
    }
    Ok(())
}

#[test]
fn spawner_roles_are_exactly_the_builtin_roles() -> TestResult {
    let mut want: Vec<String> = role_names::ALL.iter().map(|r| (*r).to_owned()).collect();
    want.sort();

    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)?;
        let snapshot = assembled.assembly.rights_snapshot();
        match entry.profile().spawner {
            SpawnerPolicy::BuiltinRoles => {
                assert_eq!(snapshot.spawner_roles, want, "{entry:?}");
                assert!(assembled.assembly.spawner().is_some(), "{entry:?}");
            }
            SpawnerPolicy::None => {
                assert!(snapshot.spawner_roles.is_empty(), "{entry:?}");
                assert!(assembled.assembly.spawner().is_none(), "{entry:?}");
            }
        }
    }
    Ok(())
}

#[test]
fn ceiling_sections_follow_the_ceiling_policy() -> TestResult {
    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)?;
        let sections = assembled.assembly.rights_snapshot().ceiling_sections;
        match entry.profile().ceiling {
            CeilingPolicy::Closed => assert!(sections.is_empty(), "{entry:?}"),
            CeilingPolicy::LocalRoot => {
                assert!(
                    sections.contains(&harw_core::HISTORY_TAIL_SECTION.to_owned()),
                    "{entry:?} muss den Verlaufsschwanz führen: {sections:?}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn a_spawning_entry_needs_a_session_event_sender() -> TestResult {
    let fixture = fixture()?;
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let built = RuntimeAssembly::builder(spec_for(EntryKind::Tui, &fixture))
        .model(ModelSource::Echo("echo".to_owned()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .build();

    assert!(matches!(built, Err(RuntimeError::Spawner { .. })));
    Ok(())
}

#[test]
fn the_child_factory_refuses_an_unknown_role() -> TestResult {
    let fixture = fixture()?;
    let assembled = assemble(EntryKind::Tui, &fixture)?;
    let factory = RuntimeChildRegistryFactory::new(
        assembled.assembly.project().clone(),
        Arc::new(EchoModelProvider::new("echo")),
        ApprovalChain::for_root(
            assembled.assembly.config(),
            AskResolution::Interactive,
            ApprovalModeCell::default(),
            None,
            AllowRuleSet::new(),
        ),
    )
    .map_err(ctx("Fabrik"))?;

    let input = spawn_input();
    // Kein `.expect_err(...)`: das verlangte `Result::Ok`-Typ `Debug`, den
    // `ExtensionRegistry` absichtlich nicht trägt (Werkzeug-Registries gehören
    // nicht ins Log). Die Eigenschaft, um die es hier geht — die Kette scheitert
    // an der unbekannten Rolle —, prüfen wir direkt über den `Err`-Zweig.
    let error = match factory.build_registry("definitely-not-a-role", &input, None) {
        Ok(_) => {
            return Err(TestError::Unexpected(
                "unbekannte Rolle muss fehlschlagen".into(),
            ));
        }
        Err(error) => error,
    };
    assert!(
        error.message.contains("unknown role"),
        "die Meldung nennt den Grund: {}",
        error.message
    );

    // Gegenprobe: eine eingebaute Rolle montiert.
    factory
        .build_registry(role_names::EXPLORER, &input, None)
        .map_err(ctx("eingebaute Rolle montiert"))?;
    Ok(())
}

#[test]
fn the_child_registry_inherits_the_config_approval_policy() -> TestResult {
    let fixture = fixture()?;
    let assembled = assemble(EntryKind::Tui, &fixture)?;
    let mut config = harw_config::ResolvedConfig::default();
    config.harness.policy.require_approval_for = vec!["fs.write".to_owned()];
    let chain = ApprovalChain::for_root(
        &config,
        AskResolution::Interactive,
        ApprovalModeCell::default(),
        None,
        AllowRuleSet::new(),
    );
    // Die Kette, die das Kind führen muss: dieselbe wie die des Elternteils,
    // nur über der gelösten Modus-Zelle des Kindes.
    let child_chain = chain.for_child().snapshot();
    assert!(
        child_chain
            .iter()
            .any(|(_, kind)| *kind == ApprovalHandlerKind::ConfigPolicy),
        "die Vorbedingung des Tests: das Elternteil führt eine Config-Politik"
    );

    let factory = RuntimeChildRegistryFactory::new(
        assembled.assembly.project().clone(),
        Arc::new(EchoModelProvider::new("echo")),
        chain,
    )
    .map_err(ctx("Fabrik"))?;
    let registry = factory
        .build_registry(role_names::EXPLORER, &spawn_input(), None)
        .map_err(ctx("montiert"))?;

    // Die Kind-Registry führt die geerbte Kette (F-018) — und **genau** sie:
    // `install_over_default` hängt keine zweite `DefaultApprovalPolicy` neben
    // die, die `assemble_registry_for_project` schon registriert hat
    // (Befund Z2c-06).
    assert_eq!(
        registry.approval_handlers().len(),
        child_chain.len(),
        "die Kind-Registry bildet die Kind-Kette ab, ohne Dublette: {child_chain:?}"
    );
    Ok(())
}

#[test]
fn discovery_runs_exactly_once_per_assembly() -> TestResult {
    let fixture = fixture()?;
    let assembled = assemble(EntryKind::Tui, &fixture)?;

    // Das Projektverzeichnis verschwindet **nach** der Montage. Eine
    // verborgene zweite Erkennung könnte danach nicht mehr gelingen; der
    // Beweis hängt also nicht an einem Zähler, sondern an der Unmöglichkeit.
    // Umbenennen statt `remove_dir_all`: Die Montage darf im Hintergrund
    // noch in das Projekt schreiben (Journal, Caches); ein Löschen liefe dann
    // in `ENOTEMPTY`. Das Umbenennen ist atomar, der Pfad ist sofort weg.
    let parked = fixture.project.with_file_name("project-entfernt");
    std::fs::rename(&fixture.project, &parked).map_err(ctx("Projekt entfernen"))?;
    assert!(
        harw_project_discovery::discover_project(
            &fixture.project,
            &harw_project_discovery::DiscoveryConfig::default(),
        )
        .is_err(),
        "die Erkennung muss ab hier scheitern"
    );

    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    let root = assembled
        .assembly
        .new_root_session(
            assembled.assembly.root_session_id().clone(),
            assembled.events.clone(),
            turn_events,
            None,
        )
        .map_err(ctx("die Wurzelsitzung entsteht ohne zweite Erkennung"))?;
    assert_eq!(root.session.id(), assembled.assembly.root_session_id());

    // Auch eine Kind-Registry kommt ohne Verzeichnis aus.
    let factory = RuntimeChildRegistryFactory::new(
        assembled.assembly.project().clone(),
        Arc::new(EchoModelProvider::new("echo")),
        ApprovalChain::for_root(
            assembled.assembly.config(),
            AskResolution::Interactive,
            ApprovalModeCell::default(),
            None,
            AllowRuleSet::new(),
        ),
    )
    .map_err(ctx("Fabrik"))?;
    factory
        .build_registry(role_names::EXPLORER, &spawn_input(), None)
        .map_err(ctx("Kind-Registry ohne zweite Erkennung"))?;
    Ok(())
}

#[test]
fn the_root_session_is_handed_out_exactly_once() -> TestResult {
    let fixture = fixture()?;
    let assembled = assemble(EntryKind::Tui, &fixture)?;
    let id = assembled.assembly.root_session_id().clone();

    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    assembled
        .assembly
        .new_root_session(id.clone(), assembled.events.clone(), turn_events, None)
        .map_err(ctx("erste Wurzelsitzung"))?;

    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    let second =
        assembled
            .assembly
            .new_root_session(id, assembled.events.clone(), turn_events, None);
    assert!(matches!(second, Err(RuntimeError::Registry { .. })));
    Ok(())
}

#[test]
fn a_foreign_session_id_is_refused() -> TestResult {
    let fixture = fixture()?;
    let assembled = assemble(EntryKind::Tui, &fixture)?;
    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    let refused = assembled.assembly.new_root_session(
        SessionId::new(),
        assembled.events.clone(),
        turn_events,
        None,
    );
    assert!(matches!(refused, Err(RuntimeError::Spawner { .. })));
    Ok(())
}

#[test]
fn root_activation_matches_the_session_base_activation() -> TestResult {
    // `active_agent` bestimmt die Wurzelaktivierung (seit Runde 3, Welle C1
    // auch in `Tui`/`OneShot`, wo ein expliziter Agent die UIA ersetzt;
    // siehe `an_explicit_root_agent_makes_the_uia_optional`).
    // `EntryKind::Analyze` ist kein UI-Einstieg und kennt keine UIA-Pflicht.
    // Zugleich führt `Analyze` — wie `Tui`/`OneShot` — einen
    // `SpawnerPolicy::BuiltinRoles`-Spawner, den `new_root_session`
    // braucht, um überhaupt eine Sitzung zu eröffnen (sonst
    // `RuntimeError::Spawner`). Die Invariante W2A-02 bleibt unverändert:
    // die Spawner-Fläche, mit der die Wurzel registriert wurde, muss exakt
    // die Basis-Aktivierung der eröffneten Sitzung sein.
    let fixture = fixture()?;
    let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventAlias>();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let mut spec = spec_for(EntryKind::Analyze, &fixture);
    spec.active_agent = Some(role_names::EXPLORER.to_owned());
    let assembly = RuntimeAssembly::builder(spec)
        .model(ModelSource::Echo("echo".to_owned()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .session_events(events.clone())
        .build()
        .map_err(ctx("montiert"))?;

    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    let root = assembly
        .new_root_session(
            assembly.root_session_id().clone(),
            events,
            turn_events,
            None,
        )
        .map_err(ctx("Wurzelsitzung"))?;

    // Der Spawner wurde mit derselben Fläche registriert, die die Sitzung
    // danach als Basis führt — sonst würde ein Kind gegen eine andere
    // Aktivierung geschnitten als die, unter der die Wurzel läuft (W2A-02).
    let base = root.session.base_activation();
    assert_eq!(base.profile(), harw_core::ToolProfile::Minimal);
    Ok(())
}

#[test]
fn an_unknown_active_agent_fails_closed() -> TestResult {
    // `active_agent` bestimmt die Wurzelaktivierung jedes Einstiegs
    // (`resolve_explicit_root_agent` in `harw-runtime/src/assembly.rs`).
    // Fail-closed bei unbekannter Rolle bleibt die geprüfte Absicht; den
    // `Tui`-Fall prüft `tui_fails_closed_on_an_unknown_explicit_root_agent`.
    let fixture = fixture()?;
    let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventAlias>();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let mut spec = spec_for(EntryKind::Analyze, &fixture);
    spec.active_agent = Some("definitely-not-a-role".to_owned());
    let built = RuntimeAssembly::builder(spec)
        .model(ModelSource::Echo("echo".to_owned()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .session_events(events)
        .build();

    assert!(matches!(built, Err(RuntimeError::Registry { .. })));
    Ok(())
}

/// Runde 3, Welle C1: ein explizit gewählter Wurzel-Agent gewinnt über die
/// UIA — also scheitert ein unbekannter Name jetzt auch in `Tui`
/// fail-closed, statt von der UIA überschattet zu werden.
#[test]
fn tui_fails_closed_on_an_unknown_explicit_root_agent() -> TestResult {
    let fixture = fixture()?;
    let mut spec = spec_for(EntryKind::Tui, &fixture);
    spec.active_agent = Some("definitely-not-a-role".to_owned());
    let built = build_with_spec(spec);
    assert!(
        matches!(built, Err(RuntimeError::Registry { .. })),
        "ein unbekannter --agent muss scheitern: {:?}",
        built.err()
    );
    Ok(())
}

/// Montiert eine beliebige Eingangsbeschreibung mit Echo-Modell.
fn build_with_spec(spec: RuntimeSpec) -> Result<RuntimeAssembly, RuntimeError> {
    let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventAlias>();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    RuntimeAssembly::builder(spec)
        .model(ModelSource::Echo("echo".to_owned()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .session_events(events)
        .build()
}

/// Ein Projekt, dessen Root-Space **keine** UIA konfiguriert.
fn fixture_without_uia() -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home).map_err(ctx("home"))?;
    std::fs::create_dir_all(&project).map_err(ctx("project"))?;
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n").map_err(ctx("marker"))?;
    Ok(Fixture {
        _dir: dir,
        home,
        project,
    })
}

/// Runde 3, Welle C1: mit explizitem Wurzel-Agenten ist die UIA nicht Pflicht.
/// `Tui` und `OneShot` montieren ohne `active_uia_definition`, die Wurzel
/// trägt die Organisationsrolle des gewählten Agenten (`explorer` →
/// `Worker`), und ohne Agent bleibt die UIA-Pflicht bestehen.
#[test]
fn an_explicit_root_agent_makes_the_uia_optional() -> TestResult {
    for entry in [EntryKind::Tui, EntryKind::OneShot] {
        let fixture = fixture_without_uia()?;

        let without_agent = build_with_spec(spec_for(entry, &fixture));
        assert!(
            matches!(without_agent, Err(RuntimeError::Registry { .. })),
            "{entry:?}: ohne Agent bleibt die UIA Pflicht"
        );

        let mut spec = spec_for(entry, &fixture);
        spec.active_agent = Some(role_names::EXPLORER.to_owned());
        let assembly = build_with_spec(spec).map_err(|error| {
            TestError::Unexpected(format!("{entry:?} mit --agent explorer: {error}"))
        })?;
        assert_eq!(
            assembly.spawn_context().organizational_role,
            AgentRoleId::Worker,
            "{entry:?}"
        );
        // Die Wurzelaktivierung folgt der Definition des Agenten
        // (`Minimal` + `admitted` − `forbidden`), nicht der Vorgabe.
        assert_eq!(
            assembly.root_activation().profile(),
            harw_core::ToolProfile::Minimal,
            "{entry:?}"
        );
    }
    Ok(())
}

/// Runde 3, Welle C1: als Wurzel sind nur Root-/Child-Orchestrator und
/// Worker zulässig. Eine UIA (`user-interface`) oder ein UIA-Helfer als
/// `--agent` ist ein Konfigurationsfehler — auch wenn eine UIA konfiguriert
/// ist, gewinnt der explizite Agent und wird geprüft.
#[test]
fn an_explicit_root_agent_with_a_foreign_role_is_a_config_error() -> TestResult {
    let fixture = fixture()?;
    for agent in [
        "harwness.agent.fixture-uia@1".to_owned(),
        role_names::UIA_WORKER.to_owned(),
    ] {
        let mut spec = spec_for(EntryKind::Tui, &fixture);
        spec.active_agent = Some(agent.clone());
        let built = build_with_spec(spec);
        assert!(
            matches!(built, Err(RuntimeError::Config { .. })),
            "{agent}: {:?}",
            built.err()
        );
    }
    Ok(())
}

#[test]
fn turn_limits_never_exceed_the_root_budget() -> TestResult {
    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)?;
        let budget = *assembled.assembly.budget();
        let limits = *assembled.assembly.turn_limits();
        assert_eq!(
            limits.max_model_rounds, budget.max_model_rounds,
            "{entry:?}"
        );
        assert_eq!(
            limits.max_output_tokens_total, budget.max_total_tokens,
            "{entry:?}"
        );
        assert_eq!(limits.wall_time, budget.max_wall, "{entry:?}");
        assert!(limits.tool_result_max_bytes > 0, "{entry:?}");
    }
    Ok(())
}

/// Ein Haken-Doppel: merkt sich jede gemeldete Sitzung.
#[derive(Debug, Default)]
struct CountingHook {
    closed: Mutex<Vec<SessionId>>,
}

impl SessionLifecycleHook for CountingHook {
    fn on_session_closed(&self, id: &SessionId) {
        self.closed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(id.clone());
    }
}

/// Ein Contributor-Doppel: hängt genau einen Haken ein.
#[derive(Debug, Default)]
struct HookContributor {
    hook: Arc<CountingHook>,
}

impl AssemblyContributor for HookContributor {
    fn contribute(
        &self,
        _inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        parts
            .lifecycle_hooks
            .push(Arc::clone(&self.hook) as Arc<dyn SessionLifecycleHook>);
        Ok(())
    }
}

/// Z2c-11(c): Der Erweiterungspunkt war ungetestet, weil
/// `default_contributors()` leer ist. Ein Test-Contributor hängt einen Haken
/// ein — damit ist beides geprüft: dass ein Beitrag die Montage erreicht und
/// dass `close_session` jeden Haken ruft.
#[test]
fn closing_a_session_reaches_every_hook() -> TestResult {
    let fixture = fixture()?;
    let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventAlias>();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let contributor = Arc::new(HookContributor::default());
    let assembly = RuntimeAssembly::builder(spec_for(EntryKind::Tui, &fixture))
        .model(ModelSource::Echo("echo".to_owned()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .session_events(events)
        .contributor(Arc::clone(&contributor) as Arc<dyn AssemblyContributor>)
        .build()
        .map_err(ctx("montiert"))?;

    // Wurzel und ein Kind: `close_session` unterscheidet sie nicht.
    let child = SessionId::new();
    assembly.close_session(assembly.root_session_id());
    assembly.close_session(&child);

    let seen = contributor
        .hook
        .closed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(seen, vec![assembly.root_session_id().clone(), child]);
    Ok(())
}

/// Ein Responder-Doppel: entscheidet nichts, meldet aber Art und Namen,
/// damit [`RuntimeAssembly::rights_snapshot`] geprüft werden kann.
#[derive(Debug)]
struct TestResponder;

impl ApprovalHandler for TestResponder {
    fn review<'a>(&'a self, _call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
        Box::pin(async { ApprovalDecision::Allow })
    }

    fn kind(&self) -> ApprovalHandlerKind {
        ApprovalHandlerKind::Interactive
    }

    fn label(&self) -> &'static str {
        "test-responder"
    }
}

/// Die Namen der Operationen, die in `assembly` eine Modell-Tool-Fläche
/// deklarieren — also genau die, die ein Einstieg mit
/// [`OperationSurface::AllWithModelTools`] dem Modell anbietet
/// (`ModelToolAdapter::tool_name` ist `OperationMeta::name`).
fn operation_model_tool_names(assembly: &RuntimeAssembly) -> Vec<String> {
    assembly
        .operations()
        .by_surface(|surface| matches!(surface, Surface::ModelTool { .. }))
        .iter()
        .map(|operation| operation.meta().name.to_owned())
        .collect()
}

/// Z2c-01, erste Hälfte: die Operations-Registry eines Laufs ist die seines
/// Einstiegs — [`OperationSurface::None`] heißt wirklich „keine".
#[test]
fn the_operation_surface_reaches_the_assembled_run() -> TestResult {
    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)?;
        let operations = assembled.assembly.operations();
        match entry.profile().operations {
            OperationSurface::None => assert!(
                operations.is_empty(),
                "{entry:?} darf keine Operation führen, führt aber {}",
                operations.len()
            ),
            OperationSurface::CommandsOnly | OperationSurface::AllWithModelTools => {
                assert!(!operations.is_empty(), "{entry:?} führt Operationen");
            }
        }
    }
    Ok(())
}

/// Die Namen der Plan-Werkzeuge, die `register_plan_tools`
/// (`harw-ops/src/lib.rs`) unter das volle Werkzeugprofil mischt:
/// `PlanOperation`, `GoalOperation`, `ExploreOperation`,
/// `ResearchDepsOperation`, `ResearchWebOperation`, `AnalyzeOperation` und
/// die allgemeine Recherche `research` (Runde 3, Welle B)
/// (`OperationMeta::name` je Datei in `harw-ops/src/{plan,goal,explore,
/// research,analyze}.rs`).
const PLAN_TOOL_NAMES: &[&str] = &[
    "plan",
    "goal",
    "explore",
    "research_deps",
    "research_web",
    "analyze",
    "research",
];

/// Runde 5, Teil H: `agent.result` steht an der Wurzel genau dann, wenn sie
/// Kinder starten kann (Spawner) und dem Modell Werkzeuge anbietet.
#[test]
fn agent_result_is_offered_exactly_where_the_root_can_spawn_children() -> TestResult {
    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)
            .map_err(|error| TestError::Unexpected(format!("{entry:?} muss montieren: {error}")))?;
        let profile = entry.profile();
        let spawns = profile.spawner != SpawnerPolicy::None
            && profile.operations == OperationSurface::AllWithModelTools;
        let tools = assembled.assembly.rights_snapshot().tools;
        assert_eq!(
            tools.iter().any(|tool| tool == "agent.result"),
            spawns,
            "{entry:?}: agent.result folgt Spawner + Modell-Werkzeugfläche: {tools:?}"
        );
    }
    Ok(())
}

/// Runde 5, Teil M: `agent.message` steht an der Wurzel genau dort, wo
/// `agent.result` steht (Spawner + Modell-Werkzeugfläche); `parent.message`
/// nie — die Wurzel hat keinen Elternteil.
#[test]
fn agent_message_follows_agent_result_and_the_root_never_has_parent_message() -> TestResult {
    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)
            .map_err(|error| TestError::Unexpected(format!("{entry:?} muss montieren: {error}")))?;
        let tools = assembled.assembly.rights_snapshot().tools;
        assert_eq!(
            tools.iter().any(|tool| tool == "agent.message"),
            tools.iter().any(|tool| tool == "agent.result"),
            "{entry:?}: agent.message folgt agent.result: {tools:?}"
        );
        assert!(
            !tools.iter().any(|tool| tool == "parent.message"),
            "{entry:?}: die Wurzel führt nie parent.message: {tools:?}"
        );
    }
    Ok(())
}

/// Runde 5, Teil K: `agent.status`/`agent.cancel` gibt es nur an der
/// TUI-Wurzel — nur dort laufen Orchestratoren im Hintergrund. Jeder andere
/// Einstieg (One-Shot, Telegram, serve, Jobs) bleibt synchron und ohne diese
/// Werkzeuge.
#[test]
fn background_agent_tools_are_offered_only_at_the_tui_root() -> TestResult {
    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)
            .map_err(|error| TestError::Unexpected(format!("{entry:?} muss montieren: {error}")))?;
        let tools = assembled.assembly.rights_snapshot().tools;
        let expected = entry == EntryKind::Tui;
        for tool in ["agent.status", "agent.cancel"] {
            assert_eq!(
                tools.iter().any(|name| name == tool),
                expected,
                "{entry:?}: {tool} nur an der TUI-Wurzel: {tools:?}"
            );
        }
    }
    Ok(())
}

/// Z2c-01, zweite Hälfte: **nur** [`OperationSurface::AllWithModelTools`]
/// legt die Operationen dem Modell als Werkzeuge vor. Vor W2c bekamen
/// `Analyze` und `Web` — Vertrag „nur Commands" — dieselbe volle
/// Modell-Tool-Fläche wie die TUI.
#[test]
fn only_the_full_surface_offers_operations_to_the_model() -> TestResult {
    let fixture = fixture()?;
    let tui = assemble(EntryKind::Tui, &fixture)?;
    let exposed = operation_model_tool_names(&tui.assembly);
    assert!(
        !exposed.is_empty(),
        "die Vorbedingung des Tests: es gibt Modell-Tool-Operationen"
    );

    // Bewusste, benannte Ausnahme (siehe `resolve_plan_services` in
    // `harw-runtime/src/assembly.rs`): Seit dieser Welle bekommt **nur**
    // `Tui` die Plan-Werkzeuge (`PLAN_TOOL_NAMES`) automatisch dazu, sobald
    // `[tools.plan]` unangetastet ist (`default_tui_plan_services`).
    // `OneShot` bringt (wie zuvor, z. B. `harw-cli/src/chat.rs`) keinen
    // eigenen Plan-Speicher über den Builder mit und bleibt ohne eingebaute
    // Vorgabe geschlossen — Präzedenzregel 2 in `resolve_plan_services`:
    // „Nicht-`Tui`-Einstiege ohne Builder-Wert bleiben ohne eingebaute
    // Vorgabe geschlossen". Das weicht die eigentliche Zusicherung dieses
    // Tests nicht auf: Die volle Modell-Tool-Fläche
    // (`OperationSurface::AllWithModelTools`) bleibt die einzige Fläche, die
    // dem Modell überhaupt Operationen anbietet — `Analyze`/`Web`
    // (`OperationSurface::CommandsOnly`) bieten weiterhin keine einzige an
    // (siehe unten). Nur *innerhalb* der vollen Fläche unterscheiden sich
    // `Tui` und `OneShot` jetzt um genau die dokumentierten Plan-Werkzeuge.
    for entry in [EntryKind::Tui, EntryKind::OneShot] {
        let assembled = assemble(entry, &fixture)?;
        let tools = assembled.assembly.rights_snapshot().tools;
        for name in &exposed {
            let is_plan_tool = PLAN_TOOL_NAMES.contains(&name.as_str());
            if is_plan_tool && entry != EntryKind::Tui {
                assert!(
                    !tools.contains(name),
                    "{entry:?} darf das Plan-Werkzeug '{name}' nicht anbieten \
                     (dokumentierte Ausnahme, nur `Tui` bekommt die \
                     Plan-Werkzeuge standardmäßig): {tools:?}"
                );
                continue;
            }
            assert!(
                tools.contains(name),
                "{entry:?} muss '{name}' dem Modell anbieten: {tools:?}"
            );
        }
    }

    for entry in [EntryKind::Analyze, EntryKind::Web] {
        let assembled = assemble(entry, &fixture)?;
        let tools = assembled.assembly.rights_snapshot().tools;
        for name in &exposed {
            assert!(
                !tools.contains(name),
                "{entry:?} darf '{name}' nicht als Modell-Werkzeug führen: {tools:?}"
            );
        }
        assert!(
            !assembled.assembly.operations().is_empty(),
            "{entry:?} behält seine Command-Operationen"
        );
    }
    Ok(())
}

/// Z2c-02: Ein Responder ist nur dort zulässig, wo die Vertragstabelle eine
/// anwesende Person vorsieht; sonst montiert die Kette den deterministischen
/// Deny-Handler ihrer [`AskResolution`].
#[test]
fn only_an_interactive_entry_accepts_an_approval_responder() -> TestResult {
    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)?;
        let ask = entry.profile().ask;
        let label = AskResolutionPolicy::label_for(ask);
        let id = assembled.assembly.root_session_id().clone();

        let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
        let offered = assembled.assembly.new_root_session(
            id.clone(),
            assembled.events.clone(),
            turn_events,
            Some(Arc::new(TestResponder)),
        );

        match ask {
            AskResolution::Interactive => {
                let root = offered.map_err(|error| {
                    TestError::Unexpected(format!(
                        "{entry:?} löst interaktiv auf und nimmt einen Responder: {error}"
                    ))
                })?;
                assert_eq!(root.session.id(), &id);
                let chain = assembled.assembly.rights_snapshot().approval_chain;
                assert!(
                    chain.contains(&("test-responder", ApprovalHandlerKind::Interactive)),
                    "{entry:?}: der Responder steht im Snapshot: {chain:?}"
                );
                assert!(
                    chain.iter().all(|(name, _)| *name != label),
                    "{entry:?} braucht keinen Deny-Handler: {chain:?}"
                );
            }
            AskResolution::RejectTurn | AskResolution::BlockJob | AskResolution::Fail => {
                assert!(
                    matches!(offered, Err(RuntimeError::Registry { .. })),
                    "{entry:?} darf keinen Responder annehmen"
                );

                // Die Registry ist dabei **nicht** verbraucht worden: die
                // Prüfung steht vor der Vergabe. Ohne Responder montiert
                // derselbe Einstieg.
                let (turn_events, _turn_rx) =
                    tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
                assembled
                    .assembly
                    .new_root_session(id, assembled.events.clone(), turn_events, None)
                    .map_err(|error| {
                        TestError::Unexpected(format!("{entry:?} montiert ohne Responder: {error}"))
                    })?;

                let chain = assembled.assembly.rights_snapshot().approval_chain;
                assert!(
                    chain.contains(&(label, ApprovalHandlerKind::Other)),
                    "{entry:?}: '{label}' muss in der Kette stehen: {chain:?}"
                );
            }
        }
    }
    Ok(())
}

/// Z2c-06: Die Kette trägt die eingebaute Standardpolitik **genau einmal**.
/// `assemble_registry_for_project` registriert sie selbst; eine zweite käme
/// aus der Montage und wäre eine Dublette.
#[test]
fn every_entry_carries_exactly_one_default_policy() -> TestResult {
    for entry in ALL_ENTRIES {
        let fixture = fixture()?;
        let assembled = assemble(entry, &fixture)?;
        let chain = assembled.assembly.rights_snapshot().approval_chain;
        let defaults: Vec<&(&str, ApprovalHandlerKind)> = chain
            .iter()
            .filter(|(_, kind)| *kind == ApprovalHandlerKind::DefaultPolicy)
            .collect();
        assert_eq!(defaults.len(), 1, "{entry:?}: {chain:?}");
        assert_eq!(defaults[0].0, DEFAULT_POLICY_LABEL, "{entry:?}");
    }
    Ok(())
}

/// Ein minimaler, vertrauenswürdiger Handoff für die Fabrik-Tests.
fn spawn_input() -> harw_extension_api::SpawnInput {
    harw_extension_api::SpawnInput {
        parent_session_id: SessionId::new(),
        handoff_call_id: ToolCallId::new(),
        instructions: None,
        context: serde_json::Value::Null,
        ceiling: None,
    }
}

/// Hält `Path` im Gebrauch: die Fixture-Pfade werden als `&Path` geprüft.
#[test]
fn the_fixture_project_is_a_directory() -> TestResult {
    let fixture = fixture()?;
    let project: &Path = fixture.project.as_path();
    assert!(project.is_dir());
    assert!(fixture.home.is_dir());
    Ok(())
}

// The next two tests close the end-to-end gap left by
// `harw-core/tests/child_controller.rs` (a hand-built spawner, never a real
// assembled run) and by this crate's own
// `harw-registry-defaults/tests/uia_spawn_authority.rs` (registry roles,
// never a mounted `ManagedAgentSpawner`): a UIA chat session — the real
// production shape of `EntryKind::Tui` with an active UIA definition
// (`write_fixture_uia`, `resolve_active_uia` in
// `harw-runtime/src/assembly.rs`) — drives the **same** `ManagedAgentSpawner`
// the assembled run registers as its child spawner (`RuntimeAssembly::spawner`,
// built in `build_spawner`). This is the exact object `/explore` and
// `/research-web` (`harw-ops/src/{explore,research}.rs::run_single_child`)
// hand their role name to via `OpContext`/`fanout_children`.

/// Builds `assembled`'s root session (precondition-verifying it as the
/// `UserInterface` organizational role, `write_fixture_uia` sets
/// `role = "user-interface"`) and returns the real `ManagedAgentSpawner`
/// `/explore` / `/research-web` would use, plus a ready `SandboxSpec` and the
/// root's own session id for building a `SpawnInput`.
async fn uia_spawner_fixture(
    assembled: &Assembled,
) -> TestResult<(
    Arc<harw_core::ManagedAgentSpawner>,
    harw_authority::SandboxSpec,
    SessionId,
)> {
    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    let root = assembled
        .assembly
        .new_root_session(
            assembled.assembly.root_session_id().clone(),
            assembled.events.clone(),
            turn_events,
            None,
        )
        .map_err(ctx("Wurzelsitzung"))?;
    let organizational_role = root
        .session
        .spawn_context()
        .ok_or(TestError::Missing(
            "root session carries a trusted spawn context",
        ))?
        .organizational_role;
    assert_eq!(
        organizational_role,
        AgentRoleId::UserInterface,
        "precondition: the fixture UIA governs this root as UserInterface"
    );

    let spawner = Arc::clone(assembled.assembly.spawner().ok_or(TestError::Missing(
        "EntryKind::Tui mounts a BuiltinRoles spawner",
    ))?);
    let sandbox = assembled.assembly.sandbox().clone();
    let parent = assembled.assembly.root_session_id().clone();
    Ok((spawner, sandbox, parent))
}

/// Builds a minimal, trusted `SpawnInput` for the given parent session.
fn uia_child_input(parent_session_id: SessionId) -> harw_extension_api::SpawnInput {
    harw_extension_api::SpawnInput {
        parent_session_id,
        handoff_call_id: ToolCallId::new(),
        instructions: None,
        context: serde_json::json!({"task": "uia rights-matrix probe"}),
        ceiling: None,
    }
}

/// Case 1 (regression, already true today): the plain
/// `role_names::EXPLORER` / `role_names::RESEARCHER_WEB` roles that
/// `/explore` / `/research-web`
/// (`harw-ops/src/{explore,research}.rs::run_single_child`) pass today
/// resolve to organizational role `Worker` (`build_spawner`'s
/// `definitions.get(role).map_or(Worker, role())`), which
/// `harw_agent_dsl::roles::can_spawn` never lets a `UserInterface` caller
/// spawn. A UIA session calling `/explore` or `/research-web` fails here —
/// this is the design conflict as it stands before the sibling fix, proven
/// against the same `ManagedAgentSpawner` a mounted `EntryKind::Tui` run
/// actually registers, not a hand-built stand-in.
#[tokio::test]
async fn uia_root_session_is_denied_the_plain_explore_and_research_roles() -> TestResult {
    let fixture = fixture()?;
    let assembled = assemble(EntryKind::Tui, &fixture)?;
    let (spawner, sandbox, parent) = uia_spawner_fixture(&assembled).await?;

    for plain_role in [role_names::EXPLORER, role_names::RESEARCHER_WEB] {
        let Err(rejected) = spawner
            .spawn_child(
                plain_role,
                uia_child_input(parent.clone()),
                sandbox.clone(),
                None,
            )
            .await
        else {
            return Err(TestError::Unexpected(
                "a UIA root session must never spawn a plain Worker-role child".into(),
            ));
        };
        assert_eq!(
            rejected.message, "no delegation capability is available for this request",
            "role '{plain_role}'"
        );
    }
    Ok(())
}

/// Case 2 (the fix; needs the sibling change to compile):
/// `role_names::UIA_EXPLORER` / `role_names::UIA_WRITER` resolve to
/// organizational role `UiaWorker`, which `can_spawn(UserInterface,
/// UiaWorker)` already permits at the matrix level
/// (`harw-agent-dsl/src/roles.rs::test_can_spawn_uia_to_uia_worker_ok`).
/// `build_spawner` needs **no** change of its own to admit them once
/// `role_names::ALL` includes both names and an embedded TOML backs each
/// with `role = "uia-worker"` — it iterates `role_names::ALL`
/// unconditionally (`harw-runtime/src/assembly.rs::build_spawner`). This
/// test is the proof that registration is sufficient, once
/// `harw-ops/src/{explore,research}.rs` redirects a UIA caller to these
/// role names.
#[tokio::test]
async fn uia_root_session_is_admitted_its_uia_explorer_and_uia_writer_specializations() -> TestResult
{
    let fixture = fixture()?;
    let assembled = assemble(EntryKind::Tui, &fixture)?;
    let (spawner, sandbox, parent) = uia_spawner_fixture(&assembled).await?;

    for uia_role in [role_names::UIA_EXPLORER, role_names::UIA_WRITER] {
        spawner
            .spawn_child(
                uia_role,
                uia_child_input(parent.clone()),
                sandbox.clone(),
                None,
            )
            .await
            .map_err(|error| {
                TestError::Unexpected(format!(
                    "a UIA root session must admit its '{uia_role}' specialization: {error:?}"
                ))
            })?;
    }
    Ok(())
}

/// Nutzerentscheidung „die UIA-Helfer recherchieren kurz online und fügen
/// manchmal Abhängigkeiten hinzu“: `uia-worker` und `uia-writer` werden von
/// der UIA über denselben `ManagedAgentSpawner` admittiert, den ein
/// montierter `EntryKind::Tui`-Lauf registriert, und ihre Kind-Registry
/// (dieselbe [`RuntimeChildRegistryFactory::build_registry`]-Kette) trägt
/// alle vier `web.*`-Werkzeuge (`web.fetch`/`web.search` plus die
/// Crate-Werkzeuge `web.docs_rs`/`web.crates_io`, `UIA_HELPER_WEB_TOOLS`)
/// und die fünf lesenden `deps.*`-Werkzeuge — nie `lens.ask`.
///
/// Runde 3, Welle A2: die UIA-Wurzel eines `EntryKind::Tui`-Laufs trägt
/// `NetworkAccess` mit genau den Hosts der Egress-Allowlist, registriert
/// selbst aber **kein** `web.*`-Werkzeug — das Netz ist reine Durchreichung
/// an ihre Helfer. Das Netz eines Helfers ist nie breiter als das des
/// Elternteils: das Kind erbt über den Handoff die Sandbox des Elternteils
/// (`ManagedAgentSpawner::admit` prüft `ensure_child_of`), und jede
/// Verengung ist eine Schnittmenge — ein fremder Host kommt nie hinzu.
#[tokio::test]
async fn uia_helpers_get_web_search_and_deps_tools_but_never_more_network_than_the_parent()
-> TestResult {
    let fixture = fixture()?;
    let assembled = assemble(EntryKind::Tui, &fixture)?;
    let (spawner, sandbox, parent) = uia_spawner_fixture(&assembled).await?;
    assert!(
        sandbox
            .permissions()
            .contains(harw_authority::Permission::NetworkAccess),
        "precondition: the UIA root carries egress-bound network"
    );
    assert!(!sandbox.network_scope().is_empty());
    assert!(!sandbox.network_scope().allows("evil.example"));

    // Die UIA-Wurzel selbst registriert kein einziges `web.*`-Werkzeug.
    let root_tools = assembled.assembly.rights_snapshot().tools;
    assert!(
        !root_tools.iter().any(|tool| tool.starts_with("web.")),
        "die UIA-Wurzel darf kein web.* registrieren: {root_tools:?}"
    );

    // Eine Verengung um einen fremden Host verbreitert das Netz nie: die
    // Schnittmenge behält nur Hosts, die schon die Wurzel trägt.
    let widened = sandbox.restrict(
        &harw_authority::PermissionRequest::from_permissions(sandbox.permissions().iter())
            .with_network_scope(harw_authority::NetworkScope::from_hosts([
                "docs.rs".to_owned(),
                "evil.example".to_owned(),
            ])),
    );
    widened
        .ensure_child_of(&sandbox)
        .map_err(ctx("jede Verengung ist ein Kind der Wurzel"))?;
    assert!(!widened.network_scope().allows("evil.example"));
    assert!(
        widened
            .network_scope()
            .is_subset_of(sandbox.network_scope())
    );

    let factory = RuntimeChildRegistryFactory::new(
        assembled.assembly.project().clone(),
        Arc::new(EchoModelProvider::new("echo")),
        ApprovalChain::for_root(
            assembled.assembly.config(),
            AskResolution::Interactive,
            ApprovalModeCell::default(),
            None,
            AllowRuleSet::new(),
        ),
    )
    .map_err(ctx("Fabrik"))?;

    for uia_role in [role_names::UIA_WORKER, role_names::UIA_WRITER] {
        let registry = factory
            .build_registry(uia_role, &spawn_input(), None)
            .map_err(ctx("UIA-Helfer-Registry montiert"))?;
        let tools: Vec<String> = registry
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|spec| spec.name().to_owned())
            .collect();
        for expected in [
            "web.fetch",
            "web.docs_rs",
            "web.crates_io",
            "web.search",
            "deps.graph",
            "deps.locked",
            "deps.source_read",
            "deps.source_search",
            "deps.source_list",
        ] {
            assert!(
                tools.iter().any(|tool| tool == expected),
                "{uia_role}: {expected} fehlt in {tools:?}"
            );
        }
        assert!(
            !tools.iter().any(|tool| tool == "lens.ask"),
            "{uia_role}: lens.ask darf nicht registriert sein"
        );

        spawner
            .spawn_child(
                uia_role,
                uia_child_input(parent.clone()),
                sandbox.clone(),
                None,
            )
            .await
            .map_err(|error| {
                TestError::Unexpected(format!(
                    "a UIA root session must admit '{uia_role}': {error:?}"
                ))
            })?;
    }
    Ok(())
}

// ── Welle 3a, Teil A: die uia-worker-Rollenfamilie bekommt die UIA nicht die
//    Vorgabe-Provider (`harw-runtime/src/{children,assembly}.rs`) ──────────

/// Konfiguration mit zwei baubaren, netzlosen Loopback-Providern
/// (`"local-a"` als Vorgabe, `"local-b"` als abweichender UIA-Provider) —
/// dasselbe Muster wie `harw_runtime::model`s private Testfixtur, hier
/// dupliziert statt importiert (jene ist privat zu diesem Modul).
fn two_provider_config() -> harw_config::ResolvedConfig {
    fn loopback_provider(name: &str) -> harw_config::ProviderToml {
        harw_config::ProviderToml {
            stream: None,
            name: name.to_owned(),
            api: "openai-chat".to_owned(),
            base_url: "http://127.0.0.1:11434/v1".to_owned(),
            auth: None,
            auth_header: Some("none".to_owned()),
            api_key: None,
            headers: std::collections::HashMap::new(),
            models: Vec::new(),
            enabled: true,
            origin_allowlist: harw_config::OriginAllowlistToml::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
            default_reasoning_effort: None,
            gateway_identity_headers: false,
            request_timeout_secs: None,
            stream_idle_timeout_secs: None,
            retry_timeouts: None,
            max_tokens_field: None,
            send_reasoning_effort: None,
            strict_tools: None,
            parallel_tool_calls: None,
            allow_insecure_lan: false,
        }
    }

    let mut config = harw_config::ResolvedConfig {
        harness: harw_config::HarnessConfig {
            default_provider: Some("local-a".to_owned()),
            default_model: Some("local-a-model".to_owned()),
            ..harw_config::HarnessConfig::default()
        },
        ..harw_config::ResolvedConfig::default()
    };
    config
        .providers
        .insert("local-a".to_owned(), loopback_provider("local-a"));
    config
        .providers
        .insert("local-b".to_owned(), loopback_provider("local-b"));
    config
}

/// Ein Config-Layer mit allen mitgelieferten Skills
/// (`harw_home::bundled_files`, Präfix `skills/`) samt der daraus entdeckten
/// Konfiguration — dieselben Dateien, die `harw init` nach `~/.harw/skills`
/// schreibt (Runde 7, Teil T5).
fn bundled_skill_layer() -> TestResult<(TempDir, harw_config::ResolvedConfig)> {
    let layer = TempDir::new().map_err(ctx("Skill-Layer"))?;
    for file in harw_home::bundled_files()
        .iter()
        .filter(|file| file.relative_path.starts_with("skills/"))
    {
        let target = file.target_in(layer.path());
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(ctx("Skill-Verzeichnis"))?;
        }
        std::fs::write(&target, file.contents).map_err(ctx("Skill-Datei"))?;
    }
    let config = harw_config::discover_config(&[layer.path().to_path_buf()])
        .map_err(ctx("Skills entdecken"))?;
    Ok((layer, config))
}

/// Ende-zu-Ende-Beleg für Welle 3a, Teil A: die gesamte
/// `uia-worker`-Rollenfamilie (`uia-worker`, `uia-explorer`, `uia-writer`,
/// `uia-shell-worker`) muss das über `uia_provider`/`uia_model` konfigurierte
/// UIA-Modell benutzen, **nicht** `default_provider`/`default_model`, sobald
/// beide voneinander abweichen.
///
/// # Aufbau
/// `harw_core::ManagedAgentSpawner` selbst hat keine öffentliche Methode, um
/// das je Rolle registrierte Modell nachträglich zu inspizieren (es wird erst
/// bei einem tatsächlichen Kind-Turn über `ChildRegistryFactory::model_for_task`
/// gezogen, `harw-core/src/child_controller.rs::run_child_with_approvals`).
/// Dieser Test geht deshalb über [`RuntimeChildRegistryFactory`] direkt — **derselbe**
/// Fabriktyp, den `harw-runtime/src/assembly.rs::build_spawner` als
/// `uia_worker_factory` für genau diese vier Rollen registriert (Welle 3a,
/// Teil A, Schritt 5) — und über dieselben Produktionsfunktionen
/// (`harw_runtime::model::{build_root_model, build_uia_model, build_uia_worker_model}`),
/// die `RuntimeAssemblyBuilder::build` (`split_root_and_uia_worker_models`)
/// beim Bau des echten Laufs aufruft.
///
/// Drei Beweisschritte:
/// 1. **Spawn gelingt**: `factory.build_registry(role, ...)` — genau das, was
///    ein Spawn bei der Registry-Montage jeder Rolle verlangt
///    (`ChildRegistryFactory::build_registry`).
/// 2. **Routing**: `factory.model_for(role)` liefert exakt das aus
///    `uia_provider`/`uia_model` abgeleitete `uia_worker_model` — nicht das
///    `default_tree_model` (`local-a`). Das ist die direkte Wirkung von
///    Welle 3a, Teil A, Schritt 1: `internal_point_for_role` mappt die
///    gesamte Familie nicht mehr, `model_for` reicht also unverändert
///    `self.model` durch, und `self.model` ist bei dieser Fabrik bereits das
///    UIA-abgeleitete Modell.
/// 3. **Provider-Identität**: ein Request ohne eigene `provider_id` landet
///    trotzdem beim UIA-Provider (`local-b`) — die uia-worker-Rollenfamilie
///    setzt selbst keine `provider_id` (siehe
///    `harw_runtime::model::build_uia_worker_model`: pinnt nur `model_id`),
///    also muss die UIA-Standardroute (`UiaDefaultRouteProvider` in
///    `harw-runtime/src/model.rs`) sie auffüllen. Ein Request mit explizit
///    gesetzter `provider_id = "local-b"` darf ebenfalls nicht scheitern —
///    dasselbe Beweismuster wie `harw_runtime::model`s eigene Tests
///    `build_uia_model_explicit_branch_routes_default_requests_to_the_uia_provider`
///    und `build_uia_model_explicit_branch_respects_an_already_set_provider_id`.
///    Seit der Vereinheitlichung mit dem Vorgabe-Router (siehe
///    `harw-runtime/src/model.rs` `UiaModelResolution::Explicit`) gibt es
///    dafür keinen zweiten, unabhängigen HTTP-Client mehr — `local-b` ist
///    bereits im Vorgabe-Router registriert, der Loopback-Provider ohne
///    Netzzugriff bleibt (keine echte Verbindung nötig, siehe unten).
#[tokio::test]
async fn uia_worker_and_its_siblings_use_the_uia_provider_not_the_default_provider_when_they_differ()
-> TestResult {
    let fixture = fixture()?;
    let assembled = assemble(EntryKind::Tui, &fixture)?;

    let mut config = two_provider_config();
    config.harness.uia_provider = Some("local-b".to_owned());
    config.harness.uia_model = Some("local-b-model".to_owned());

    let spec = spec_for(EntryKind::Tui, &fixture);
    let default_tree_model: Arc<dyn harw_core::ModelProvider> =
        harw_runtime::model::build_root_model(&spec, &config, ModelSource::Configured)
            .map_err(ctx("default provider (local-a) must build"))?;
    let uia_client =
        harw_runtime::model::build_uia_model(&spec, &config, true, &default_tree_model).map_err(
            ctx(
                "uia provider (local-b) must resolve to a UiaDefaultRouteProvider wrapping the \
             default router — no second HTTP client is built",
            ),
        )?;
    let uia_worker_model = harw_runtime::model::build_uia_worker_model(&config, &uia_client);

    assert!(
        !Arc::ptr_eq(&default_tree_model, &uia_worker_model),
        "the uia-worker family must not resolve to the same Arc as the default provider's \
         client — it must go through the UiaDefaultRouteProvider/PinnedModelProvider wrapping"
    );

    let factory = RuntimeChildRegistryFactory::new(
        assembled.assembly.project().clone(),
        Arc::clone(&uia_worker_model),
        ApprovalChain::for_root(
            &config,
            AskResolution::Interactive,
            ApprovalModeCell::default(),
            None,
            AllowRuleSet::new(),
        ),
    )
    .map_err(ctx("Fabrik"))?;
    // Runde 7, Teil T5: `uia-latex-writer` verlangt die Skills seiner
    // Definition und startet ohne Skill-Katalog fail-closed nicht — die
    // Fabrik bekommt deshalb wie im echten Lauf die mitgelieferten Skills.
    let (skill_layer, skill_config) = bundled_skill_layer()?;
    let factory = factory
        .with_skill_catalog(&skill_config, vec![skill_layer.path().to_path_buf()])
        .map_err(ctx("Skill-Katalog"))?;

    for role in [
        role_names::UIA_WORKER,
        role_names::UIA_EXPLORER,
        role_names::UIA_WRITER,
        role_names::UIA_SHELL_WORKER,
        role_names::UIA_LATEX_WRITER,
    ] {
        // Beweisschritt 1: die Rolle spawnt tatsächlich.
        factory
            .build_registry(role, &spawn_input(), None)
            .map_err(|error| {
                TestError::Unexpected(format!("role '{role}' must build a registry: {error:?}"))
            })?;

        // Beweisschritt 2: exakt das uia-abgeleitete Modell, nicht das
        // Vorgabe-Modell.
        let model = factory.model_for(role).map_err(|error| {
            TestError::Unexpected(format!("role '{role}' must resolve a model: {error:?}"))
        })?;
        assert!(
            Arc::ptr_eq(&model, &uia_worker_model),
            "role '{role}' must receive exactly the uia-derived model, not the default \
             provider's client"
        );
    }

    // Beweisschritt 3a: eine uia-worker-Anfrage OHNE eigene `provider_id`
    // (wie `build_uia_worker_model` sie tatsächlich stellt — es pinnt nur
    // `model_id`) muss trotzdem beim UIA-Provider ('local-b') landen, nicht
    // beim Vorgabe-Provider ('local-a') und nicht bei einem unbekannten
    // Provider. Keine echte Netzwerkverbindung nötig — der Router
    // (`harw-provider-http/src/routing.rs` `RoutingModelProvider::select`)
    // entscheidet vor jedem tatsächlichen Aufruf. Direkt `.await`en (kein
    // zweiter, verschachtelter Tokio-Runtime-Bau): diese Testfunktion läuft
    // bereits unter `#[tokio::test]` — `Builder::new_current_thread().block_on(...)`
    // hier würde mit "Cannot start a runtime from within a runtime" abstürzen.
    let request_without_provider_id = harw_core::ModelRequest::new(
        harw_extension_api::types::LoadedInstructions::default(),
        Vec::new(),
        harw_core::ConversationHistory::new(),
        Vec::new(),
    );
    let result = uia_worker_model.respond(request_without_provider_id).await;
    if let Err(error) = result {
        let message = error.to_string();
        assert!(
            !message.contains("is not configured"),
            "a uia-worker request without its own provider_id must fall back to the uia default \
             route ('local-b'), which is registered in the default router: {message}"
        );
    }

    // Beweisschritt 3b: ein Request mit explizit gesetzter `provider_id =
    // "local-b"` darf ebenfalls nicht scheitern.
    let request_with_provider_id = harw_core::ModelRequest::new(
        harw_extension_api::types::LoadedInstructions::default(),
        Vec::new(),
        harw_core::ConversationHistory::new(),
        Vec::new(),
    )
    .with_provider_id(Some(harw_types::ProviderId::from("local-b")));
    let result = uia_worker_model.respond(request_with_provider_id).await;
    if let Err(error) = result {
        let message = error.to_string();
        assert!(
            !message.contains("is not configured"),
            "the uia-worker model must accept an explicit 'local-b' request too — 'local-b' is \
             registered in the default router: {message}"
        );
    }
    Ok(())
}
