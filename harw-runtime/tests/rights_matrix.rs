//! Tabellengetriebener Rechte-Snapshot je [`EntryKind`].
//!
//! # Beschreibung
//! Der Vertrag `docs/remediation/CONTRACTS.md` §runtime-spec nennt für jeden
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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harw_core::{ChildRegistryFactory, EchoModelProvider, InMemoryStateStore, StateStore};
use harw_extension_api::allow_rules::AllowRuleSet;
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_extension_api::{ApprovalDecision, ApprovalHandler, ExtFuture, ToolCall};
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

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&project).expect("project");
    // Projekt-Marker, damit `discover_project` genau hier stehen bleibt.
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n").expect("marker");
    write_fixture_uia(&home);
    Fixture {
        _dir: dir,
        home,
        project,
    }
}

/// Legt eine minimale, gültige UIA (`role = "user-interface"`) im
/// Standardprofil des Test-`home` an und aktiviert sie über
/// `harness.active_uia_definition`.
///
/// # Beschreibung
/// `EntryKind::Tui` und `EntryKind::OneShot` montieren seit dem UIA-Vertrag
/// (`harw-runtime/src/assembly.rs::resolve_active_uia`,
/// `docs/session-transcript-2026-09-14.md`) nur noch mit einer konfigurierten
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
fn write_fixture_uia(home: &Path) {
    let profile_dir = home.join("profiles").join("default");
    let agent_dir = profile_dir.join("agents").join("fixture-uia");
    std::fs::create_dir_all(&agent_dir).expect("fixture uia dir");
    std::fs::write(
        agent_dir.join("definition.toml"),
        "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
    )
    .expect("fixture uia definition");
    std::fs::write(
        profile_dir.join("config.toml"),
        "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
    )
    .expect("fixture profile config");
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
    // Sortiert, wie `rights_snapshot` sie sortiert.
    const RWX: &[&str] = &["ExecuteProcess", "ReadWorkspace", "WriteWorkspace"];
    const R: &[&str] = &["ReadWorkspace"];
    const RW: &[&str] = &["ReadWorkspace", "WriteWorkspace"];
    const NONE: &[&str] = &[];
    // Ohne `[policy].require_approval_for` entsteht keine Config-Politik
    // (W2B-03: ein Handler, der alles durchwinkt, wäre nur Rauschen); ohne
    // Responder — den erst `new_root_session` anhängt — bleibt die
    // Default-Politik allein. Nur `Tui` löst Rückfragen interaktiv auf; jeder
    // andere Einstieg trägt zusätzlich seine `AskResolutionPolicy`
    // ([`ApprovalHandlerKind::Other`], Befund Z2c-02).
    const DEFAULT_ONLY: &[ApprovalHandlerKind] = &[ApprovalHandlerKind::DefaultPolicy];
    const DEFAULT_AND_ASK: &[ApprovalHandlerKind] = &[
        ApprovalHandlerKind::DefaultPolicy,
        ApprovalHandlerKind::Other,
    ];

    match entry {
        EntryKind::Tui => Expected {
            permissions: RWX,
            tools_empty: false,
            approval_chain: DEFAULT_ONLY,
            ceiling_empty: false,
            spawner_empty: false,
        },
        EntryKind::OneShot | EntryKind::Analyze => Expected {
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
        EntryKind::McpServe
        | EntryKind::JobPrompt
        | EntryKind::GatewayTelegram
        | EntryKind::GatewayDream => Expected {
            permissions: NONE,
            tools_empty: true,
            approval_chain: DEFAULT_AND_ASK,
            ceiling_empty: true,
            spawner_empty: true,
        },
    }
}

#[test]
fn every_entry_matches_its_row_of_the_rights_table() {
    for entry in ALL_ENTRIES {
        let fixture = fixture();
        let assembled = assemble(entry, &fixture)
            .unwrap_or_else(|error| panic!("{entry:?} muss montieren: {error}"));
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
}

#[test]
fn no_entry_carries_network() {
    for entry in ALL_ENTRIES {
        let fixture = fixture();
        let assembled = assemble(entry, &fixture).expect("montiert");
        let snapshot = assembled.assembly.rights_snapshot();
        assert!(
            !snapshot.permissions.iter().any(|p| p == "NetworkAccess"),
            "{entry:?} darf bis W5 kein Netzrecht tragen"
        );
        assert!(
            assembled.assembly.network_scope().is_empty(),
            "{entry:?} darf bis W5 keinen Netz-Scope tragen"
        );
        assert!(
            assembled
                .assembly
                .sandbox()
                .network_scope()
                .is_empty(),
            "{entry:?}: auch die Sandbox selbst trägt keinen Scope"
        );
    }
}

#[test]
fn spawner_roles_are_exactly_the_builtin_roles() {
    let mut want: Vec<String> = role_names::ALL.iter().map(|r| (*r).to_owned()).collect();
    want.sort();

    for entry in ALL_ENTRIES {
        let fixture = fixture();
        let assembled = assemble(entry, &fixture).expect("montiert");
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
}

#[test]
fn ceiling_sections_follow_the_ceiling_policy() {
    for entry in ALL_ENTRIES {
        let fixture = fixture();
        let assembled = assemble(entry, &fixture).expect("montiert");
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
}

#[test]
fn a_spawning_entry_needs_a_session_event_sender() {
    let fixture = fixture();
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
}

#[test]
fn the_child_factory_refuses_an_unknown_role() {
    let fixture = fixture();
    let assembled = assemble(EntryKind::Tui, &fixture).expect("montiert");
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
    .expect("Fabrik");

    let input = spawn_input();
    // Kein `.expect_err(...)`: das verlangte `Result::Ok`-Typ `Debug`, den
    // `ExtensionRegistry` absichtlich nicht trägt (Werkzeug-Registries gehören
    // nicht ins Log). Die Eigenschaft, um die es hier geht — die Kette scheitert
    // an der unbekannten Rolle —, prüfen wir direkt über den `Err`-Zweig.
    let error = match factory.build_registry("definitely-not-a-role", &input, None) {
        Ok(_) => panic!("unbekannte Rolle muss fehlschlagen"),
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
        .expect("eingebaute Rolle montiert");
}

#[test]
fn the_child_registry_inherits_the_config_approval_policy() {
    let fixture = fixture();
    let assembled = assemble(EntryKind::Tui, &fixture).expect("montiert");
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
    .expect("Fabrik");
    let registry = factory
        .build_registry(role_names::EXPLORER, &spawn_input(), None)
        .expect("montiert");

    // Die Kind-Registry führt die geerbte Kette (F-018) — und **genau** sie:
    // `install_over_default` hängt keine zweite `DefaultApprovalPolicy` neben
    // die, die `assemble_registry_for_project` schon registriert hat
    // (Befund Z2c-06).
    assert_eq!(
        registry.approval_handlers().len(),
        child_chain.len(),
        "die Kind-Registry bildet die Kind-Kette ab, ohne Dublette: {child_chain:?}"
    );
}

#[test]
fn discovery_runs_exactly_once_per_assembly() {
    let fixture = fixture();
    let assembled = assemble(EntryKind::Tui, &fixture).expect("montiert");

    // Das Projektverzeichnis verschwindet **nach** der Montage. Eine
    // verborgene zweite Erkennung könnte danach nicht mehr gelingen; der
    // Beweis hängt also nicht an einem Zähler, sondern an der Unmöglichkeit.
    std::fs::remove_dir_all(&fixture.project).expect("Projekt entfernen");
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
        .expect("die Wurzelsitzung entsteht ohne zweite Erkennung");
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
    .expect("Fabrik");
    factory
        .build_registry(role_names::EXPLORER, &spawn_input(), None)
        .expect("Kind-Registry ohne zweite Erkennung");
}

#[test]
fn the_root_session_is_handed_out_exactly_once() {
    let fixture = fixture();
    let assembled = assemble(EntryKind::Tui, &fixture).expect("montiert");
    let id = assembled.assembly.root_session_id().clone();

    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    assembled
        .assembly
        .new_root_session(id.clone(), assembled.events.clone(), turn_events, None)
        .expect("erste Wurzelsitzung");

    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    let second =
        assembled
            .assembly
            .new_root_session(id, assembled.events.clone(), turn_events, None);
    assert!(matches!(second, Err(RuntimeError::Registry { .. })));
}

#[test]
fn a_foreign_session_id_is_refused() {
    let fixture = fixture();
    let assembled = assemble(EntryKind::Tui, &fixture).expect("montiert");
    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    let refused = assembled.assembly.new_root_session(
        SessionId::new(),
        assembled.events.clone(),
        turn_events,
        None,
    );
    assert!(matches!(refused, Err(RuntimeError::Spawner { .. })));
}

#[test]
fn root_activation_matches_the_session_base_activation() {
    // `EntryKind::Tui` (und `OneShot`) montieren seit dem UIA-Vertrag
    // ausschließlich über `harness.active_uia_definition`
    // (`resolve_active_uia`, `harw-runtime/src/assembly.rs`) — dort ersetzt
    // die UIA jede `active_agent`-Auswahl vollständig (`agent_ir` bleibt
    // `None`, sobald `uia_ir` gesetzt ist). `EntryKind::Analyze` ist kein
    // UI-Einstieg: `resolve_active_uia` liefert für ihn immer `Ok(None)`,
    // also bestimmt `active_agent` hier weiterhin die Wurzelaktivierung.
    // Zugleich führt `Analyze` — wie `Tui`/`OneShot` — einen
    // `SpawnerPolicy::BuiltinRoles`-Spawner, den `new_root_session`
    // braucht, um überhaupt eine Sitzung zu eröffnen (sonst
    // `RuntimeError::Spawner`). Die Invariante W2A-02 bleibt unverändert:
    // die Spawner-Fläche, mit der die Wurzel registriert wurde, muss exakt
    // die Basis-Aktivierung der eröffneten Sitzung sein.
    let fixture = fixture();
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
        .expect("montiert");

    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEventAlias>();
    let root = assembly
        .new_root_session(assembly.root_session_id().clone(), events, turn_events, None)
        .expect("Wurzelsitzung");

    // Der Spawner wurde mit derselben Fläche registriert, die die Sitzung
    // danach als Basis führt — sonst würde ein Kind gegen eine andere
    // Aktivierung geschnitten als die, unter der die Wurzel läuft (W2A-02).
    let base = root.session.base_activation();
    assert_eq!(base.profile(), harw_core::ToolProfile::Minimal);
}

#[test]
fn an_unknown_active_agent_fails_closed() {
    // `active_agent` bestimmt die Wurzelaktivierung nur für Nicht-UI-
    // Einstiege (`resolve_active_uia` in `harw-runtime/src/assembly.rs`
    // gibt für alles außer `Tui`/`OneShot` `Ok(None)` zurück, also greift
    // dort `resolve_active_agent(spec.active_agent, ...)`). `Analyze` ist
    // ein solcher Nicht-UI-Einstieg. Fail-closed bei unbekannter Rolle
    // bleibt die geprüfte Absicht — nur der Einstieg wechselt.
    let fixture = fixture();
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
}

#[test]
fn tui_ignores_an_unknown_active_agent_because_the_uia_governs() {
    // In `Tui` (und `OneShot`) bestimmt ausschließlich die konfigurierte
    // UIA die Root-Aktivierung; `resolve_active_agent` wird für
    // `spec.active_agent` gar nicht erst aufgerufen, sobald `uia_ir`
    // aufgelöst ist (`agent_ir` bleibt `None`). Eine unbekannte
    // `active_agent`-Rolle darf die UIA daher weder ersetzen noch die
    // Montage zu Fall bringen — die Fixture-UIA aus `write_fixture_uia`
    // montiert unverändert.
    let fixture = fixture();
    let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventAlias>();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let mut spec = spec_for(EntryKind::Tui, &fixture);
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

    assert!(
        built.is_ok(),
        "die UIA muss eine unbekannte active_agent-Rolle in Tui überschatten: {built:?}"
    );
}

#[test]
fn turn_limits_never_exceed_the_root_budget() {
    for entry in ALL_ENTRIES {
        let fixture = fixture();
        let assembled = assemble(entry, &fixture).expect("montiert");
        let budget = *assembled.assembly.budget();
        let limits = *assembled.assembly.turn_limits();
        assert_eq!(limits.max_model_rounds, budget.max_model_rounds, "{entry:?}");
        assert_eq!(
            limits.max_output_tokens_total, budget.max_total_tokens,
            "{entry:?}"
        );
        assert_eq!(limits.wall_time, budget.max_wall, "{entry:?}");
        assert!(limits.tool_result_max_bytes > 0, "{entry:?}");
    }
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
fn closing_a_session_reaches_every_hook() {
    let fixture = fixture();
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
        .expect("montiert");

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
fn the_operation_surface_reaches_the_assembled_run() {
    for entry in ALL_ENTRIES {
        let fixture = fixture();
        let assembled = assemble(entry, &fixture).expect("montiert");
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
}

/// Die Namen der Plan-Werkzeuge, die `register_plan_tools`
/// (`harw-ops/src/lib.rs`) unter das volle Werkzeugprofil mischt:
/// `PlanOperation`, `GoalOperation`, `ExploreOperation`,
/// `ResearchDepsOperation`, `ResearchWebOperation`, `AnalyzeOperation`
/// (`OperationMeta::name` je Datei in `harw-ops/src/{plan,goal,explore,
/// research,analyze}.rs`).
const PLAN_TOOL_NAMES: &[&str] = &[
    "plan",
    "goal",
    "explore",
    "research_deps",
    "research_web",
    "analyze",
];

/// Z2c-01, zweite Hälfte: **nur** [`OperationSurface::AllWithModelTools`]
/// legt die Operationen dem Modell als Werkzeuge vor. Vor W2c bekamen
/// `Analyze` und `Web` — Vertrag „nur Commands" — dieselbe volle
/// Modell-Tool-Fläche wie die TUI.
#[test]
fn only_the_full_surface_offers_operations_to_the_model() {
    let fixture = fixture();
    let tui = assemble(EntryKind::Tui, &fixture).expect("montiert");
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
        let assembled = assemble(entry, &fixture).expect("montiert");
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
        let assembled = assemble(entry, &fixture).expect("montiert");
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
}

/// Z2c-02: Ein Responder ist nur dort zulässig, wo die Vertragstabelle eine
/// anwesende Person vorsieht; sonst montiert die Kette den deterministischen
/// Deny-Handler ihrer [`AskResolution`].
#[test]
fn only_an_interactive_entry_accepts_an_approval_responder() {
    for entry in ALL_ENTRIES {
        let fixture = fixture();
        let assembled = assemble(entry, &fixture).expect("montiert");
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
                let root = offered.unwrap_or_else(|error| {
                    panic!("{entry:?} löst interaktiv auf und nimmt einen Responder: {error}")
                });
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
                    .unwrap_or_else(|error| panic!("{entry:?} montiert ohne Responder: {error}"));

                let chain = assembled.assembly.rights_snapshot().approval_chain;
                assert!(
                    chain.contains(&(label, ApprovalHandlerKind::Other)),
                    "{entry:?}: '{label}' muss in der Kette stehen: {chain:?}"
                );
            }
        }
    }
}

/// Z2c-06: Die Kette trägt die eingebaute Standardpolitik **genau einmal**.
/// `assemble_registry_for_project` registriert sie selbst; eine zweite käme
/// aus der Montage und wäre eine Dublette.
#[test]
fn every_entry_carries_exactly_one_default_policy() {
    for entry in ALL_ENTRIES {
        let fixture = fixture();
        let assembled = assemble(entry, &fixture).expect("montiert");
        let chain = assembled.assembly.rights_snapshot().approval_chain;
        let defaults: Vec<&(&str, ApprovalHandlerKind)> = chain
            .iter()
            .filter(|(_, kind)| *kind == ApprovalHandlerKind::DefaultPolicy)
            .collect();
        assert_eq!(defaults.len(), 1, "{entry:?}: {chain:?}");
        assert_eq!(defaults[0].0, DEFAULT_POLICY_LABEL, "{entry:?}");
    }
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
fn the_fixture_project_is_a_directory() {
    let fixture = fixture();
    let project: &Path = fixture.project.as_path();
    assert!(project.is_dir());
    assert!(fixture.home.is_dir());
}
