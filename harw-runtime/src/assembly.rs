//! Die eine Montage aller `harw`-Einstiege (`RuntimeAssembly`).
//!
//! # Beschreibung
//! Neun Einstiegspfade bauten die Laufzeit bis Welle W2d-2 je selbst zusammen
//! (die TUI-Montage in `harw-tui/src/app.rs` und die Montagen in
//! `harw-cli/src/{chat,main,web,gateway,job_worker}.rs`; seither rufen die
//! Einstiege [`RuntimeAssembly::builder`], etwa `run_tui` in
//! `harw-tui/src/runtime_root.rs` und `job_assembly` in
//! `harw-cli/src/runtime_jobs.rs`). Sie unterschieden sich dabei in Dingen, die sich nicht
//! unterscheiden dürfen: Sandbox-Rechte wurden literal hingeschrieben statt
//! aus dem Einstieg abgeleitet, die Wurzeldecke gab es in zwei Fassungen, der
//! Wurzel-Trace entstand vier Mal innerhalb des Spawn-Kontexts (G-044), die
//! Config-Freigabepolitik hing an `[tools.plan].enabled` (G-010/F-154), und
//! die Projekterkennung lief je Kind erneut (G-071).
//!
//! [`RuntimeAssembly`] ist der eine Ort, der all das genau einmal tut. Die
//! Reihenfolge ist Teil des Vertrags, weil jeder Schritt den nächsten
//! begrenzt:
//!
//! 0. [`RuntimeAssemblyBuilder::narrowing`] — die Verengung des Aufrufers
//!    wird gegen die Profil-Whitelist geprüft, bevor etwas geladen wird.
//! 1. [`load_config`] — Konfiguration **mit** Vertrauensbericht.
//! 2. [`discover_project`] — **genau einmal**; jeder spätere Bedarf
//!    (Kind-Registries) benutzt das Ergebnis.
//! 3. [`root_sandbox`] — Rechte ausschließlich aus [`EntryKind::profile`],
//!    gegebenenfalls geschnitten mit [`RuntimeNarrowing::permissions`];
//!    gebunden an den erkannten Projekt-Root oder an den engeren
//!    [`RuntimeNarrowing::workspace_root`].
//! 4. [`root_ceiling`] — eine Wurzeldecke je [`CeilingPolicy`].
//! 5. [`new_root_trace`] + **ein** [`SpawnContext`], geklont für Sitzung und
//!    Spawner: Wurzel-Turn und Kinder hängen an derselben `trace_id`.
//! 6. [`ApprovalChain::for_root`] — Config-Politik ohne Nebenschalter, plus
//!    die [`crate::spec::AskResolution`] des Einstiegs.
//! 7. [`assemble_registry_for_project`] → [`ApprovalChain::install_over_default`].
//! 8. Operations-Registry nach [`OperationSurface`].
//! 9. Wurzel-Modell, dann Spawner nach [`SpawnerPolicy`].
//! 10. [`AssemblyContributor`]s in Registrierungsreihenfolge.
//! 11. [`RuntimeServices::new`].
//! 12. Modell-Tool-Fläche der Operationen (nur
//!     [`OperationSurface::AllWithModelTools`]), dann
//!     `ExtensionRegistryBuilder::build`.
//!
//! # Abweichung von der Auftragsreihenfolge (bewusst, begründet)
//! Der Auftrag nennt „`RuntimeServices::new(parts)` → Spawner → Contributors
//! → Modell". Das ist nicht baubar und wäre auch fachlich falsch:
//! [`RuntimeServicesParts::spawner`] ist ein Feld, das bei der Konstruktion
//! feststeht (G-061 verlangt den Spawner auf der Slash-Fläche), der
//! Kind-Fabrik-Konstruktor braucht den bereits gebauten Modellanbieter
//! ([`RuntimeChildRegistryFactory::with_definitions`] in `build_spawner`),
//! und ein Contributor, der nach der
//! Service-Montage liefe, könnte weder Operation noch Werkzeug beisteuern.
//! Modell → Spawner → Contributors → Services ist deshalb die einzige
//! Ordnung, in der jeder Schritt auf fertigen Eingaben steht.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harw_agent_dsl::ExecutableAgentIr;
use harw_agent_dsl::roles::AgentRoleId;
use harw_config::ResolvedConfig;
use harw_context::ContextCeiling;
use harw_core::{
    AgentSession, ChildRegistryFactory, ManagedAgentSpawner, ModelProvider, SessionActivation,
    SessionManager, SpawnContext, StateStore, ToolProfile,
};
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_extension_api::{ApprovalHandler, ExtensionRegistry, ExtensionRegistryBuilder, ToolName};
use harw_memory::Memory;
use harw_operations::adapter::ModelToolProvider;
use harw_operations::operation::{Operation, Surface};
use harw_operations::registry::OperationRegistry;
use harw_operations::{OpContext, SharedSessionController};
use harw_project_discovery::{DiscoveryConfig, ProjectContext, discover_project};
use harw_protocol::events::{SessionEvent, TurnEvent};
use harw_provider_http::SecretResolver;
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    IdentityOverrides, RegistryProfile, assemble_registry_for_project, role_names,
};
use harw_sandbox::{
    NetworkScope, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_session_store::{ApprovalStore, JobStore};
use harw_types::{AgentRole, Principal, SessionId, TenantId, TurnId, WorkspaceId};
use tokio::sync::mpsc::UnboundedSender;

use crate::approval::ApprovalChain;
use crate::budget::child_limits;
use crate::ceiling::root_ceiling;
use crate::children::RuntimeChildRegistryFactory;
use crate::config::{ConfigTrustReport, load_config};
use crate::contributors::{AssemblyContributor, AssemblyInputs, AssemblyParts};
use crate::error::{RuntimeError, RuntimeResult};
use crate::model::{ModelSource, build_root_model_with_resolver};
use crate::sandbox::root_sandbox;
use crate::services::{PlanServices, RuntimeServices, RuntimeServicesParts, ServiceSurface};
use crate::spec::{
    AskResolution, EntryKind, EntryProfile, OperationSurface, RootBudget, RuntimeSpec,
    SpawnerPolicy,
};
use crate::trace::new_root_trace;

/// Werkzeugaufrufe, die ein Turn höchstens je Modell-Runde auslösen darf.
///
/// # Beschreibung
/// Aus [`RootBudget::max_model_rounds`] allein folgt keine Obergrenze für
/// Werkzeugaufrufe: eine Runde darf mehrere Aufrufe parallel enthalten
/// (`harw-core/src/turn_loop.rs`, Parallelpfad). Der Faktor ist bewusst
/// konservativ und dokumentiert; die **Durchsetzung** folgt in Welle W4a.
const TOOL_CALLS_PER_ROUND: u32 = 8;

/// Obergrenze eines einzelnen gerenderten Werkzeugergebnisses in Bytes.
///
/// Übergangswert bis W4a (`ModelRequest.tool_result_max_bytes`, gefrorene
/// W3-Signatur): groß genug für eine gelesene Quelldatei, klein genug, dass
/// ein einzelnes Ergebnis das Kontextfenster nicht allein füllt.
const TOOL_RESULT_MAX_BYTES: usize = 64 * 1024;

/// Grenzwerte eines einzelnen Turns, abgeleitet aus dem [`RootBudget`].
///
/// # Beschreibung
/// `harw-core` kennt heute keinen solchen Typ (`grep TurnLimits` über den
/// Workspace ist leer), deshalb steht er hier. Er ist **reine Ableitung**:
/// jedes Feld folgt aus dem Budget des Laufs oder aus einer dokumentierten
/// Konstante dieses Moduls. Die Durchsetzung (Abbruch bei Überschreitung)
/// gehört in den Turn-Loop und folgt in Welle W4a; bis dahin ist dieser Typ
/// der eine Ort, an dem die Zahlen stehen, statt fünf verstreuter Literale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnLimits {
    /// Maximale Anzahl Modell-Runden eines Turns.
    pub max_model_rounds: u32,
    /// Maximale Anzahl Werkzeugaufrufe eines Turns.
    pub max_tool_calls: u32,
    /// Maximale Gesamtzahl erzeugter Ausgabe-Tokens.
    pub max_output_tokens_total: u64,
    /// Maximale Wanduhrzeit eines Turns.
    pub wall_time: Duration,
    /// Obergrenze eines einzelnen gerenderten Werkzeugergebnisses in Bytes.
    pub tool_result_max_bytes: usize,
}

impl TurnLimits {
    /// Leitet die Turn-Grenzwerte aus dem Budget des Laufs ab.
    ///
    /// # Argumente
    /// - `budget` (`&`[`RootBudget`]): das Budget des Wurzel-Agenten.
    ///
    /// # Rückgabe
    /// Grenzwerte, die das Budget **nie überschreiten**: Runden und Zeit sind
    /// identisch, die Tokengrenze ist dieselbe Zahl (ein Turn darf nicht mehr
    /// ausgeben als der ganze Lauf), die Aufrufgrenze ist das Produkt aus
    /// Runden und [`TOOL_CALLS_PER_ROUND`] mit Sättigung.
    #[must_use]
    pub const fn from_root_budget(budget: &RootBudget) -> Self {
        Self {
            max_model_rounds: budget.max_model_rounds,
            max_tool_calls: budget.max_model_rounds.saturating_mul(TOOL_CALLS_PER_ROUND),
            max_output_tokens_total: budget.max_total_tokens,
            wall_time: budget.max_wall,
            tool_result_max_bytes: TOOL_RESULT_MAX_BYTES,
        }
    }
}

/// Etwas, das vom Ende einer Sitzung erfahren muss.
///
/// # Beschreibung
/// Vertrag aus dem Plan-Abschnitt „Abgleich mit Teil A". Ein Lebenszyklus-Haken
/// räumt auf (Leases, Spool-Einträge, Transcript-Flush), er entscheidet nichts
/// und kann nichts verhindern — [`RuntimeAssembly::close_session`] ruft alle
/// Haken auf und ignoriert keine.
///
/// # Nebenläufigkeit
/// `Send + Sync`; `on_session_closed` bekommt `&self` und darf blockieren,
/// aber nicht `close_session` erneut aufrufen.
pub trait SessionLifecycleHook: Send + Sync {
    /// Meldet, dass die Sitzung `id` beendet ist.
    ///
    /// # Argumente
    /// - `id` (`&SessionId`): die beendete Sitzung.
    fn on_session_closed(&self, id: &SessionId);
}

/// Die durablen Speicher eines Laufs.
///
/// # Beschreibung
/// `state_store` ist Pflicht (ohne Verlaufsspeicher gibt es keinen Turn),
/// die beiden übrigen sind es nicht: ein `LocalEcho`-Lauf hat weder Jobs noch
/// durable Freigaben.
pub struct RuntimeStores {
    /// Verlaufsspeicher der Sitzung.
    pub state_store: Arc<dyn StateStore>,
    /// Job-Speicher; `None`, wenn der Einstieg keine durablen Jobs kennt.
    pub job_store: Option<Arc<JobStore>>,
    /// Speicher durabler Freigaben; `None` ohne durable Freigabefläche.
    pub approval_store: Option<Arc<ApprovalStore>>,
}

impl std::fmt::Debug for RuntimeStores {
    /// Zeigt nur, welche Speicher vorhanden sind — Pfade und Inhalte nicht.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeStores")
            .field("job_store", &self.job_store.is_some())
            .field("approval_store", &self.approval_store.is_some())
            .finish_non_exhaustive()
    }
}

/// Die fertig montierte Wurzelsitzung eines Laufs.
pub struct RootSession {
    /// Die Sitzung selbst; Modus, Agent-IR und Effort sind bereits gesetzt.
    pub session: AgentSession,
    /// Die Freigabe-Zelle **dieses** Laufs — der Schalter hinter
    /// `/permissions set`.
    pub approval_mode: ApprovalModeCell,
}

impl std::fmt::Debug for RootSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootSession")
            .field("session_id", self.session.id())
            .field("mode", &self.session.mode())
            .field("approval_mode", &self.approval_mode.get())
            .finish()
    }
}

/// Der Vorgabe-Freigabemodus eines Einstiegs.
///
/// # Beschreibung
/// Für **jeden** Einstieg [`ApprovalMode::Delegated`] — dieselbe Stufe, die
/// [`ApprovalModeCell::default`] liefert, und die einzige, die ohne
/// Zutun einer Person vertretbar ist:
/// - [`ApprovalMode::FullAccess`] ist nie eine Vorgabe. Er ist eine Aussage
///   einer anwesenden Person über einen Turn, den sie vor sich sieht.
/// - [`ApprovalMode::AlwaysAsk`] als Vorgabe wäre für die Einstiege ohne
///   Antwortfläche (`McpServe`, `JobPrompt`, `Gateway*`) kein strengerer,
///   sondern ein *funktionsloser* Zustand: jede Rückfrage liefe sofort in die
///   [`crate::spec::AskResolution`] des Einstiegs.
///
/// Die Funktion existiert trotz einheitlichem Ergebnis als **eine** benannte
/// Stelle: die Unterscheidung nach Einstieg ist damit vorbereitet, ohne dass
/// heute ein Einstieg eine Sonderregel bekommt, die niemand begründet hat.
///
/// # Argumente
/// - `entry` ([`EntryKind`]): der Einstieg.
///
/// # Rückgabe
/// [`ApprovalMode::Delegated`].
#[must_use]
pub const fn default_approval_mode(entry: EntryKind) -> ApprovalMode {
    match entry {
        EntryKind::Tui
        | EntryKind::OneShot
        | EntryKind::LocalEcho
        | EntryKind::Analyze
        | EntryKind::Doctor
        | EntryKind::Web
        | EntryKind::McpServe
        | EntryKind::JobPrompt
        | EntryKind::JobPlanNode
        | EntryKind::GatewayTelegram
        | EntryKind::GatewayDream => ApprovalMode::Delegated,
    }
}

/// Die organisatorische Rolle (§3-Spawn-Matrix) der Wurzel eines Einstiegs.
///
/// # Beschreibung
/// Fail-closed: nur ein Einstieg, der überhaupt spawnen darf, wird als
/// [`AgentRoleId::RootOrchestrator`] geführt. Alle übrigen laufen als
/// [`AgentRoleId::Worker`] — die Rolle, die nach
/// `harw_agent_dsl::roles::can_spawn` **kein** Ziel spawnen darf. Damit hängt
/// die Spawn-Fähigkeit nicht allein daran, dass kein Spawner montiert wurde,
/// sondern zusätzlich an der Matrix.
#[must_use]
const fn root_organizational_role(spawner: SpawnerPolicy) -> AgentRoleId {
    match spawner {
        SpawnerPolicy::BuiltinRoles => AgentRoleId::RootOrchestrator,
        SpawnerPolicy::None => AgentRoleId::Worker,
    }
}

/// Leitet die Basis-Aktivierung einer Sitzung aus ihrer Agent-IR ab.
///
/// # Beschreibung
/// Wortgleich zu [`AgentSession::with_executable_agent_ir`]
/// (`harw-core/src/session.rs:389-397`): Profil [`ToolProfile::Minimal`], dann
/// `admitted` einschalten, dann `forbidden` ausschalten. Die Montage braucht
/// diesen Wert **vor** der Sitzung, weil
/// [`ManagedAgentSpawner::with_external_root_parent`] die Eltern-Aktivierung
/// bei der Registrierung entgegennimmt (W2A-02) — die Sitzung entsteht aber
/// erst in [`RuntimeAssembly::new_root_session`].
///
/// Dass beide Wege dasselbe ergeben, ist kein Kommentar, sondern geprüft:
/// `root_activation_matches_the_session_base_activation` in
/// `harw-runtime/tests/rights_matrix.rs`.
///
/// # Argumente
/// - `ir` (`Option<&ExecutableAgentIr>`): die IR des Wurzel-Agenten.
///
/// # Rückgabe
/// Ohne IR [`SessionActivation::default`] (Profil `Full`) — genau der Wert,
/// den `AgentSession::new_with_id` setzt, also kein Schnitt.
fn root_activation(ir: Option<&ExecutableAgentIr>) -> SessionActivation {
    let Some(ir) = ir else {
        return SessionActivation::default();
    };
    let mut activation = SessionActivation::new(ToolProfile::Minimal);
    for name in ir.tool_surface().admitted() {
        activation.enable_tool(ToolName::new(name.clone()));
    }
    for name in ir.tool_surface().forbidden() {
        activation.disable_tool(ToolName::new(name.clone()));
    }
    activation
}

/// Eine Verengung, die der Aufrufer über die Rechte seines Einstiegs legt.
///
/// # Beschreibung
/// Manche Einstiege führen innerhalb ihrer Zeile der Reduktionstabelle
/// ([`EntryKind::profile`]) Arbeit aus, die **weniger** dürfen soll — etwa ein
/// Plan-Knoten-Job, dessen Knotenart nur lesend recherchiert (CONTRACTS-W2d2
/// §1.1, E10). Eine `RuntimeNarrowing` beschreibt diese Verengung auf vier
/// Achsen und kann Rechte **nie erweitern**:
///
/// - `registry_profile`: der Werkzeugsatz. Erlaubt ist nur, was die
///   Profil-Whitelist des Einstiegs zulässt (siehe
///   [`RuntimeAssemblyBuilder::narrowing`]); alles andere bricht den Bau ab.
/// - `identity`: ersetzt die Vorgabe-Identität der Registry vollständig;
///   ein gesetztes [`RuntimeSpec::active_agent`] behält Vorrang beim
///   Agentennamen.
/// - `permissions`: Obergrenze der Sandbox-Rechte; die Wurzel-Sandbox wird mit
///   ihr **geschnitten** ([`SandboxSpec::restrict`]), nie ersetzt.
/// - `workspace_root`: bindet die Wurzel-Sandbox an genau dieses Verzeichnis
///   statt an den erkannten Projekt-Root. Der kanonische Pfad muss gleich dem
///   kanonischen Projekt-Root oder ein Nachfahre davon sein (Vergleich über
///   Pfad-Komponenten); Projekterkennung, Konfiguration und Vertrauensbericht
///   bleiben am erkannten Projekt.
///
/// Aktivierung, Spawner-Politik und Freigabekette bleiben unberührt.
///
/// # Nebenläufigkeit
/// Reiner Wert (`Clone`), `Send + Sync`.
#[derive(Debug, Clone)]
pub struct RuntimeNarrowing {
    /// Gewünschter Werkzeugsatz; muss in der Whitelist des Einstiegsprofils liegen.
    pub registry_profile: RegistryProfile,
    /// Identität der Registry; ersetzt die Vorgabe des Einstiegs.
    pub identity: IdentityOverrides,
    /// Obergrenze der Sandbox-Rechte; wird mit den Profilrechten geschnitten.
    pub permissions: PermissionSet,
    /// Bindet die Root-Sandbox an genau dieses Verzeichnis statt an den erkannten Projekt-Root.
    /// Muss kanonisch gleich dem erkannten Root oder ein Nachfahre davon sein (nur enger), sonst
    /// [`RuntimeError::Sandbox`].
    pub workspace_root: Option<PathBuf>,
}

/// Mandant und Alias der reinen Enthaltenseins-Prüfung in [`sandbox_root`].
///
/// Die Registry dieser Prüfung lebt nur für die Dauer des Aufrufs; die
/// eigentliche Bindung entsteht danach über [`root_sandbox`] mit dem
/// Mandanten des Einstiegs.
const NARROWED_ROOT_TENANT: &str = "narrowing";
/// Workspace-Alias der Enthaltenseins-Prüfung (siehe [`NARROWED_ROOT_TENANT`]).
const NARROWED_ROOT_WORKSPACE: &str = "workspace-root";

/// Das Verzeichnis, an das die Wurzel-Sandbox gebunden wird.
///
/// # Beschreibung
/// Ohne [`RuntimeNarrowing::workspace_root`] ist das der erkannte
/// `project_root`. Mit ihm wird der verlangte Pfad über
/// [`WorkspaceRegistry::build`] mit `project_root` als Harness-Root
/// kanonisiert: dieselbe Mechanik, die jede Sandbox-Bindung benutzt
/// (einmal `canonicalize`, Verzeichnisprüfung, Komponenten-Vergleich
/// `starts_with` gegen den kanonischen Harness-Root). Es entsteht keine
/// eigene Symlink-Auflösung. Gleichheit mit dem Projekt-Root ist erlaubt,
/// jeder Pfad außerhalb wird abgelehnt.
///
/// Relative Pfade werden fail-closed abgelehnt: sie hätten sonst eine
/// stille Basis (den Projekt-Root), die der Aufrufer nicht benannt hat.
///
/// # Fehler
/// [`RuntimeError::Sandbox`], wenn der Pfad relativ ist, nicht existiert,
/// kein Verzeichnis ist oder außerhalb des erkannten Projekt-Roots liegt.
fn sandbox_root(
    project_root: &Path,
    narrowing: Option<&RuntimeNarrowing>,
) -> RuntimeResult<PathBuf> {
    let Some(requested) = narrowing.and_then(|narrowing| narrowing.workspace_root.as_deref())
    else {
        return Ok(project_root.to_path_buf());
    };
    if !requested.is_absolute() {
        return Err(RuntimeError::Sandbox {
            detail: format!(
                "refusing to bind the root sandbox to the relative workspace root {}: \
                 a narrowed workspace root must be absolute",
                requested.display()
            ),
        });
    }
    let tenant = TenantId::from_str(NARROWED_ROOT_TENANT);
    let workspace = WorkspaceId::from_str(NARROWED_ROOT_WORKSPACE);
    let registry = WorkspaceRegistry::build(
        project_root,
        [WorkspaceRegistration {
            tenant: tenant.clone(),
            workspace: workspace.clone(),
            root: requested.to_path_buf(),
        }],
    )
    .map_err(|error| RuntimeError::Sandbox {
        detail: format!(
            "refusing to bind the root sandbox to workspace root {}: it must be the discovered \
             project root {} or a directory below it: {error}",
            requested.display(),
            project_root.display()
        ),
    })?;
    let binding = registry
        .resolve(&tenant, &workspace)
        .map_err(|error| RuntimeError::Sandbox {
            detail: format!(
                "could not resolve the narrowed workspace root {}: {error}",
                requested.display()
            ),
        })?;
    Ok(binding.canonical_root().to_path_buf())
}

/// Stellt sicher, dass die gebaute Sandbox genau an `expected` gebunden ist.
///
/// # Beschreibung
/// [`root_sandbox`] kanonisiert den übergebenen, bereits kanonischen Pfad
/// erneut. Wurde zwischen beiden Schritten eine Pfadkomponente ausgetauscht,
/// wiche die Bindung ab; das wird hier fail-closed abgefangen.
///
/// # Fehler
/// [`RuntimeError::Sandbox`] bei jeder Abweichung.
fn ensure_bound_to(sandbox: &SandboxSpec, expected: &Path) -> RuntimeResult<()> {
    let bound = sandbox.workspace().canonical_root();
    if bound == expected {
        Ok(())
    } else {
        Err(RuntimeError::Sandbox {
            detail: format!(
                "the root sandbox is bound to {} but the narrowed workspace root is {}",
                bound.display(),
                expected.display()
            ),
        })
    }
}

/// Prüft eine gewünschte Werkzeugsatz-Verengung gegen das Einstiegsprofil.
///
/// # Beschreibung
/// Fail-closed Whitelist (CONTRACTS-W2d2 §1.1): Einstieg `Full` erlaubt
/// `Full | ReadOnlyExplore | NoTools`, Einstieg `NoTools` nur `NoTools`, jede
/// andere Kombination wird abgelehnt. Das `match` ist bewusst ohne
/// Auffang-Arm im ersten Tupelelement: eine neue [`RegistryProfile`]-Variante
/// bricht den Compiler, statt still zugelassen zu werden.
///
/// `ReadOnlyExplore` ist **keine** Werkzeug-Teilmenge von `Full`: es bringt
/// `deps.*` mit, das `Full` nicht registriert. Die Whitelist bleibt trotzdem
/// wie vertraglich festgelegt; abgesichert wird über die Sandbox, weil kein
/// Einstiegsprofil [`harw_sandbox::Permission::ReadCargoRegistry`] trägt und
/// die geschnittene Sandbox dieses Recht deshalb nie erhalten kann.
///
/// # Fehler
/// [`RuntimeError::Registry`] für jede nicht zugelassene Kombination.
fn narrowed_registry_profile(
    entry: EntryKind,
    entry_profile: RegistryProfile,
    requested: RegistryProfile,
) -> RuntimeResult<RegistryProfile> {
    use RegistryProfile::{Full, NoTools, Planning, ReadOnlyExplore, Research};

    match (entry_profile, requested) {
        (Full, Full | ReadOnlyExplore | NoTools) | (NoTools, NoTools) => Ok(requested),
        (Full, Research | Planning)
        | (NoTools, Full | ReadOnlyExplore | Research | Planning)
        | (ReadOnlyExplore | Research | Planning, _) => Err(RuntimeError::Registry {
            detail: format!(
                "refusing to narrow entry {entry:?} from registry profile {entry_profile:?} \
                 to {requested:?}: a narrowing may only reduce the tool set"
            ),
        }),
    }
}

/// Schneidet die Wurzel-Sandbox mit der Rechte-Obergrenze einer Verengung.
///
/// # Beschreibung
/// Ohne Verengung bleibt `sandbox` unverändert. Mit Verengung entsteht
/// `sandbox.restrict(&narrowing.permissions)`; das Ergebnis wird zusätzlich
/// gegen die Profilrechte geprüft, damit auch eine künftige Änderung an
/// [`root_sandbox`] oder [`SandboxSpec::restrict`] nie still mehr Rechte
/// ergibt, als die Tabelle dem Einstieg zuspricht.
///
/// # Fehler
/// [`RuntimeError::Sandbox`], wenn das Ergebnis die Profilrechte überschreitet.
fn narrowed_sandbox(
    sandbox: SandboxSpec,
    profile: &EntryProfile,
    narrowing: Option<&RuntimeNarrowing>,
) -> RuntimeResult<SandboxSpec> {
    let sandbox = match narrowing {
        Some(narrowing) => sandbox.restrict(&narrowing.permissions),
        None => sandbox,
    };
    if sandbox.permissions().is_subset_of(&profile.permissions) {
        Ok(sandbox)
    } else {
        Err(RuntimeError::Sandbox {
            detail: format!(
                "the root sandbox permissions {:?} exceed the entry profile permissions {:?}",
                sandbox.permissions(),
                profile.permissions
            ),
        })
    }
}

/// Die Registry-Identität des Wurzel-Agenten.
///
/// # Beschreibung
/// Ohne Verengung: Vorgabe-Identität mit `agent_name = spec.active_agent`.
/// Mit Verengung: deren `identity` ersetzt die Vorgabe; ein gesetztes
/// `active_agent` hat beim Agentennamen Vorrang.
fn root_identity(spec: &RuntimeSpec, narrowing: Option<&RuntimeNarrowing>) -> IdentityOverrides {
    let mut identity = narrowing
        .map(|narrowing| narrowing.identity.clone())
        .unwrap_or_default();
    if spec.active_agent.is_some() || narrowing.is_none() {
        identity.agent_name.clone_from(&spec.active_agent);
    }
    identity
}

/// Baut eine [`RuntimeAssembly`] Schritt für Schritt.
///
/// # Beschreibung
/// Entsteht ausschließlich über [`RuntimeAssembly::builder`]. Pflicht sind
/// [`Self::model`] und [`Self::stores`]; alles Übrige ist optional und fehlt
/// dann bewusst, statt durch einen erfundenen Vorgabewert ersetzt zu werden.
pub struct RuntimeAssemblyBuilder {
    spec: RuntimeSpec,
    model: Option<ModelSource>,
    stores: Option<RuntimeStores>,
    plan_services: Option<PlanServices>,
    memory: Option<Arc<dyn Memory>>,
    session_controller: Option<SharedSessionController>,
    session_events: Option<UnboundedSender<SessionEvent>>,
    contributors: Vec<Arc<dyn AssemblyContributor>>,
    root_session_id: Option<SessionId>,
    /// Löst `secrets:`-Referenzen beim Bau von [`ModelSource::Configured`]
    /// auf; ohne ihn schlägt jedes `auth = "secrets:…"` fehl (Befund C2a).
    secret_resolver: Option<Arc<dyn SecretResolver + Send + Sync>>,
    /// Verengung von Werkzeugsatz, Identität und Sandbox-Rechten durch den
    /// Aufrufer (CONTRACTS-W2d2 §1.1); `None` heißt „Profil unverändert".
    narrowing: Option<RuntimeNarrowing>,
}

impl std::fmt::Debug for RuntimeAssemblyBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeAssemblyBuilder")
            .field("entry", &self.spec.entry)
            .field("model", &self.model)
            .field("stores", &self.stores)
            .field("plan_services", &self.plan_services.is_some())
            .field("memory", &self.memory.is_some())
            .field("session_controller", &self.session_controller.is_some())
            .field("session_events", &self.session_events.is_some())
            .field("contributors", &self.contributors.len())
            .field("secret_resolver", &self.secret_resolver.is_some())
            .field(
                "narrowing",
                &self
                    .narrowing
                    .as_ref()
                    .map(|narrowing| narrowing.registry_profile),
            )
            .finish_non_exhaustive()
    }
}

impl RuntimeAssemblyBuilder {
    /// Wählt die Quelle des Wurzel-Modells. **Pflicht.**
    #[must_use]
    pub fn model(mut self, model: ModelSource) -> Self {
        self.model = Some(model);
        self
    }

    /// Übergibt die durablen Speicher. **Pflicht.**
    #[must_use]
    pub fn stores(mut self, stores: RuntimeStores) -> Self {
        self.stores = Some(stores);
        self
    }

    /// Übergibt die Plan-Dienste; ohne sie registriert die Montage keine
    /// Plan-Operationen (`harw_ops::register_plan_tools` bleibt ungerufen).
    #[must_use]
    pub fn plan_services(mut self, plan: PlanServices) -> Self {
        self.plan_services = Some(plan);
        self
    }

    /// Übergibt das Gedächtnis des Laufs.
    #[must_use]
    pub fn memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Übergibt den Sitzungs-Controller (`/model`, `/effort`, `/mode`).
    #[must_use]
    pub fn session_controller(mut self, controller: SharedSessionController) -> Self {
        self.session_controller = Some(controller);
        self
    }

    /// Übergibt den Ereigniskanal der Sitzungen.
    ///
    /// # Beschreibung
    /// **Erforderlich für [`SpawnerPolicy::BuiltinRoles`]:** der
    /// [`SessionManager`] hinter [`ManagedAgentSpawner`] nimmt den Sender bei
    /// seiner Konstruktion entgegen (`harw-core/src/session_manager.rs:19`),
    /// also lange bevor [`RuntimeAssembly::new_root_session`] gerufen wird.
    /// Der Aufrufer übergibt denselben Sender, den er später an
    /// `new_root_session` gibt — im TUI-Einstieg etwa legt
    /// `TuiSessionWiring::install` (`harw-tui/src/runtime_root.rs`) den Sender
    /// in den Builder, und `run_tui` reicht einen Klon desselben Senders an
    /// [`RuntimeAssembly::new_root_session`].
    #[must_use]
    pub fn session_events(mut self, events: UnboundedSender<SessionEvent>) -> Self {
        self.session_events = Some(events);
        self
    }

    /// Übergibt den `secrets:`-Resolver für [`ModelSource::Configured`].
    ///
    /// # Beschreibung
    /// Ohne diesen Aufruf baut [`Self::build`] das Wurzel-Modell mit
    /// `resolver: None` — jede `auth = "secrets:…"`-Referenz eines
    /// aktivierten Providers schlägt dann fehl
    /// ([`harw_provider_http::build_provider_with_home`]). Der Aufrufer öffnet
    /// den versiegelten Speicher genau wie bisher
    /// (`harw-cli/src/secret_store.rs` `open_configured_secret_resolver`) und
    /// reicht das Ergebnis hier herein; diese Crate öffnet ihn nicht selbst
    /// (siehe `crate::model`, Abschnitt „Geheimnisse").
    #[must_use]
    pub fn secret_resolver(mut self, resolver: Arc<dyn SecretResolver + Send + Sync>) -> Self {
        self.secret_resolver = Some(resolver);
        self
    }

    /// Hängt einen [`AssemblyContributor`] an. Die Reihenfolge der Aufrufe ist
    /// die Ausführungsreihenfolge.
    #[must_use]
    pub fn contributor(mut self, contributor: Arc<dyn AssemblyContributor>) -> Self {
        self.contributors.push(contributor);
        self
    }

    /// Legt die Kennung der Wurzelsitzung fest.
    ///
    /// # Beschreibung
    /// Ohne diesen Aufruf prägt [`Self::build`] eine frische
    /// [`SessionId`]. Der Wert ist danach über
    /// [`RuntimeAssembly::root_session_id`] lesbar und muss
    /// [`RuntimeAssembly::new_root_session`] übergeben werden: der Spawner hat
    /// genau diese Kennung als vertrauenswürdige Wurzel registriert
    /// (`ManagedAgentSpawner::with_external_root_parent`), und eine zweite
    /// Registrierung ist dort ausgeschlossen.
    #[must_use]
    pub fn root_session_id(mut self, id: SessionId) -> Self {
        self.root_session_id = Some(id);
        self
    }

    /// Legt eine Verengung über die Rechte des Einstiegs.
    ///
    /// # Beschreibung
    /// Fail-closed Bau-Semantik (CONTRACTS-W2d2 §1.1), durchgesetzt in
    /// [`Self::build`] **vor** jedem anderen Montageschritt:
    ///
    /// | Einstiegsprofil | zulässige `registry_profile` |
    /// |---|---|
    /// | `Full` | `Full`, `ReadOnlyExplore`, `NoTools` |
    /// | `NoTools` | `NoTools` |
    /// | jedes andere | keine |
    ///
    /// Die Sandbox wird zu `root_sandbox(entry, root).restrict(&permissions)`
    /// und liegt damit immer innerhalb der Profilrechte; dieselbe Sandbox
    /// steht im [`SpawnContext`], Kinder erben also nie mehr. `root` ist der
    /// erkannte Projekt-Root oder, falls gesetzt, der kanonische
    /// [`RuntimeNarrowing::workspace_root`] (gleich dem Projekt-Root oder ein
    /// Nachfahre davon, sonst [`RuntimeError::Sandbox`]).
    /// `identity` ersetzt die Vorgabe-Identität, [`RuntimeSpec::active_agent`]
    /// behält Vorrang beim Agentennamen. Aktivierung, Spawner-Politik,
    /// Freigabekette und [`RuntimeAssembly::profile`] (die unveränderte
    /// Tabellenzeile) bleiben unberührt.
    ///
    /// # Argumente
    /// - `narrowing` ([`RuntimeNarrowing`]): die Verengung; ein zweiter Aufruf
    ///   ersetzt den ersten.
    #[must_use]
    pub fn narrowing(mut self, narrowing: RuntimeNarrowing) -> Self {
        self.narrowing = Some(narrowing);
        self
    }

    /// Montiert den Lauf.
    ///
    /// # Rückgabe
    /// Eine [`RuntimeAssembly`], deren Rechte vollständig aus
    /// [`EntryKind::profile`] folgen, gegebenenfalls verengt durch
    /// [`Self::narrowing`].
    ///
    /// # Fehler
    /// - [`RuntimeError::Provider`], wenn [`Self::model`] fehlt oder das
    ///   Modell nicht gebaut werden kann.
    /// - [`RuntimeError::Store`], wenn [`Self::stores`] fehlt.
    /// - [`RuntimeError::Config`] / [`RuntimeError::Trust`] aus
    ///   [`load_config`].
    /// - [`RuntimeError::Discovery`], wenn die Projekterkennung scheitert.
    /// - [`RuntimeError::Sandbox`] aus [`root_sandbox`], oder wenn
    ///   [`RuntimeNarrowing::workspace_root`] relativ ist, nicht existiert oder
    ///   außerhalb des erkannten Projekt-Roots liegt.
    /// - [`RuntimeError::Registry`], wenn die Registry nicht montiert, ein
    ///   benannter Agent nicht aufgelöst werden kann oder eine
    ///   [`Self::narrowing`] einen nicht zugelassenen Werkzeugsatz verlangt.
    /// - [`RuntimeError::Sandbox`] auch, wenn die verengte Sandbox die
    ///   Profilrechte überschritte (konstruktionsbedingt unerreichbar, aber
    ///   geprüft).
    /// - [`RuntimeError::Spawner`], wenn ein Einstieg mit
    ///   [`SpawnerPolicy::BuiltinRoles`] ohne [`Self::session_events`] gebaut
    ///   wird oder die Wurzelregistrierung scheitert.
    pub fn build(self) -> RuntimeResult<RuntimeAssembly> {
        let Self {
            spec,
            model,
            stores,
            plan_services,
            memory,
            session_controller,
            session_events,
            contributors,
            root_session_id,
            secret_resolver,
            narrowing,
        } = self;

        let model_source = model.ok_or_else(|| RuntimeError::Provider {
            detail: "no model source was given to the runtime builder".to_owned(),
        })?;
        let stores = stores.ok_or_else(|| RuntimeError::Store {
            detail: "no stores were given to the runtime builder".to_owned(),
        })?;

        let profile = spec.entry.profile();

        // 0. Verengung des Aufrufers — fail-closed, bevor irgendetwas gelesen
        //    oder gebaut wird (CONTRACTS-W2d2 §1.1).
        let registry_profile = match narrowing.as_ref() {
            Some(narrowing) => narrowed_registry_profile(
                spec.entry,
                profile.registry_profile,
                narrowing.registry_profile,
            )?,
            None => profile.registry_profile,
        };

        // 1. Konfiguration mit Vertrauensbericht.
        let (config, trust_report) = load_config(&spec)?;
        let config = Arc::new(config);

        // 2. Projekterkennung — genau einmal je Lauf.
        let project = discover_project(&spec.cwd, &DiscoveryConfig::default()).map_err(|error| {
            RuntimeError::Discovery {
                detail: format!("could not discover the project below the cwd: {error}"),
            }
        })?;

        // 3./4. Sandbox und Decke aus dem Einstiegsprofil.
        //      Eine Verengung schneidet die Sandbox, sie ersetzt sie nie; ein
        //      `workspace_root` bindet sie enger (nie außerhalb des Projekts).
        let bound_root = sandbox_root(&project.project_root, narrowing.as_ref())?;
        let unrestricted = root_sandbox(spec.entry, &bound_root)?;
        if narrowing
            .as_ref()
            .is_some_and(|narrowing| narrowing.workspace_root.is_some())
        {
            ensure_bound_to(&unrestricted, &bound_root)?;
        }
        let sandbox = narrowed_sandbox(unrestricted, &profile, narrowing.as_ref())?;
        let ceiling = root_ceiling(profile.ceiling);

        // 5. Ein Trace, ein Spawn-Kontext.
        let trace = new_root_trace(spec.entry);
        let spawn_context = SpawnContext {
            sandbox: sandbox.clone(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: spec.principal.approval_actor(),
            organizational_role: root_organizational_role(profile.spawner),
            trace: Some(trace),
            ceiling: Some(ceiling.clone()),
        };

        // 6. Freigabekette. Der Responder kommt erst mit `new_root_session`:
        //    er gehört zur Oberfläche, nicht zur Montage.
        let approval_mode = ApprovalModeCell::new(default_approval_mode(spec.entry));
        // `profile.ask` ist ab hier durchgesetzt, nicht nur deklariert: alles
        // außer `Interactive` hängt eine `AskResolutionPolicy` in die Kette
        // (Befund Z2c-02).
        let chain = ApprovalChain::for_root(&config, profile.ask, approval_mode.clone(), None);

        // Die eingebauten Rollen werden **einmal** gesenkt und danach sowohl
        // für `--agent` als auch für die Kind-Fabrik benutzt (Befund Z2c-07).
        // Ohne beides bleibt die Arbeit ganz aus.
        let needs_definitions =
            spec.active_agent.is_some() || matches!(profile.spawner, SpawnerPolicy::BuiltinRoles);
        let agent_definitions = if needs_definitions {
            lower_agent_definitions(&config)?
        } else {
            HashMap::new()
        };

        // Agent-IR des Wurzel-Agenten (falls einer benannt ist).
        let agent_ir =
            resolve_active_agent(spec.active_agent.as_deref(), &config, &agent_definitions)?;
        let activation = root_activation(agent_ir.as_ref());

        // 7. Registry: ein Projektkontext, eine Kette.
        let overrides = root_identity(&spec, narrowing.as_ref());
        let assembled = assemble_registry_for_project(
            registry_profile,
            &project,
            overrides,
            chain.mode().clone(),
        )
        .map_err(|error| RuntimeError::Registry {
            detail: format!("could not assemble the root registry: {error}"),
        })?;
        // `install_over_default`: `assemble_registry_for_project` hat die
        // `DefaultApprovalPolicy` über `chain.mode()` gerade selbst registriert
        // (harw-registry-defaults/src/profile.rs:921-922) — eine zweite wäre
        // eine Dublette (Befund Z2c-06).
        let registry_builder = chain.install_over_default(assembled.registry);

        // 8. Operationen nach der Fläche des Einstiegs.
        let operations = build_operations(profile.operations, plan_services.as_ref());

        let budget = RootBudget::from_config(&config, spec.entry);
        let turn_limits = TurnLimits::from_root_budget(&budget);

        // 9. Modell, dann Spawner (die Kind-Fabrik braucht den Anbieter).
        //    `secret_resolver` löst `secrets:`-Referenzen für
        //    `ModelSource::Configured` auf (Befund C2a); der Upcast wirft nur
        //    die Auto-Traits ab, dieselbe `SecretResolver`-Vtable bleibt
        //    gültig, deshalb reicht eine gewöhnliche Unsize-Coercion ohne
        //    Trait-Upcasting-Feature.
        let model = build_root_model_with_resolver(
            &spec,
            &config,
            model_source,
            secret_resolver.as_deref().map(|r| r as &dyn SecretResolver),
        )?;
        let root_session_id = root_session_id.unwrap_or_else(SessionId::new);
        let (spawner, spawner_roles) = build_spawner(
            profile.spawner,
            SpawnerInputs {
                config: &config,
                project: &project,
                chain: &chain,
                model: &model,
                root_session_id: &root_session_id,
                spawn_context: &spawn_context,
                reasoning_effort: spec.reasoning_effort,
                activation: &activation,
                definitions: &agent_definitions,
            },
            session_events,
        )?;

        // 10. Contributors.
        let mut parts = AssemblyParts {
            registry: registry_builder,
            operations,
            lifecycle_hooks: Vec::new(),
            network_scope: NetworkScope::empty(),
        };
        {
            let inputs = AssemblyInputs {
                spec: &spec,
                profile: &profile,
                config: config.as_ref(),
                trust_report: &trust_report,
                project: &project,
                spawn_context: &spawn_context,
                budget: &budget,
                turn_limits: &turn_limits,
                approval_mode: &approval_mode,
            };
            for contributor in &contributors {
                contributor.contribute(&inputs, &mut parts)?;
            }
        }
        let AssemblyParts {
            registry: registry_builder,
            operations,
            lifecycle_hooks,
            network_scope,
        } = parts;

        let operations = Arc::new(operations);

        // 11. Dienste. Sie entstehen **vor** dem Bau der Registry, weil die
        //     Modell-Tool-Fläche der Operationen ihre Service-Map braucht.
        let services = Arc::new(RuntimeServices::new(RuntimeServicesParts {
            operations: Arc::clone(&operations),
            state_store: Arc::clone(&stores.state_store),
            job_store: stores.job_store.clone(),
            spawner: spawner.clone(),
            memory,
            config: Arc::clone(&config),
            plan: plan_services,
            approval_mode: approval_mode.clone(),
            principal: spec.principal.clone(),
            session_controller,
        }));

        // 12. Modell-Tool-Fläche der Operationen — **nur** für
        //     `OperationSurface::AllWithModelTools`.
        let registry_builder = install_operation_model_tools(
            registry_builder,
            profile.operations,
            &operations,
            &services,
        );

        let registry = registry_builder.build();
        let tools = registered_tool_names(&registry);

        Ok(RuntimeAssembly {
            spec,
            profile,
            config,
            trust_report,
            project,
            sandbox,
            ceiling,
            spawn_context,
            budget,
            turn_limits,
            network_scope,
            chain,
            approval_mode,
            agent_ir,
            activation,
            operations,
            services,
            model,
            stores,
            spawner,
            spawner_roles,
            lifecycle_hooks,
            tools,
            root_session_id,
            registry: Mutex::new(Some(registry)),
            responder: Mutex::new(None),
        })
    }
}

/// Senkt die eingebauten Rollen **einmal** je Montage.
///
/// # Beschreibung
/// `existing` ist `config.executable_agents` — die aus `[agents]` gelowerten
/// Rollen. [`builtin_agent_definitions`] überspringt jede eingebaute Rolle,
/// deren Namen eine lokale Definition bereits belegt
/// (`harw-registry-defaults/src/embedded_agents.rs:645-647`); die lokale Rolle
/// gewinnt damit, ohne dass hier etwas zusammengeführt werden müsste.
///
/// # Fehler
/// [`RuntimeError::Registry`], wenn eine eingebettete Definition nicht senkt.
fn lower_agent_definitions(
    config: &ResolvedConfig,
) -> RuntimeResult<HashMap<String, ExecutableAgentIr>> {
    builtin_agent_definitions(&config.executable_agents).map_err(|error| RuntimeError::Registry {
        detail: format!("could not lower builtin agent definitions: {error}"),
    })
}

/// Löst den benannten Wurzel-Agenten zu seiner gesenkten IR auf.
///
/// # Argumente
/// - `name` (`Option<&str>`): der Wert von `--agent`; `None` heißt „keiner".
/// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration. Ihre
///   `executable_agents` haben **Vorrang** vor den eingebauten Rollen: wer
///   eine Rolle in `[agents]` konfiguriert, startet sonst nicht (Befund
///   Z2c-05). Die Vorrangrichtung ist dieselbe, die
///   [`builtin_agent_definitions`] selbst anwendet.
/// - `builtin` (`&HashMap<String, ExecutableAgentIr>`): das Ergebnis von
///   [`lower_agent_definitions`].
///
/// # Fehler
/// [`RuntimeError::Registry`], wenn der Name weder konfiguriert noch eingebaut
/// ist. **Fail-closed:** ein unbekannter `--agent`-Name startet nicht mit dem
/// vollen Werkzeugsatz, er startet gar nicht.
fn resolve_active_agent(
    name: Option<&str>,
    config: &ResolvedConfig,
    builtin: &HashMap<String, ExecutableAgentIr>,
) -> RuntimeResult<Option<ExecutableAgentIr>> {
    let Some(name) = name else {
        return Ok(None);
    };
    if let Some(ir) = config.executable_agents.get(name) {
        return Ok(Some(ir.clone()));
    }
    builtin
        .get(name)
        .cloned()
        .map(Some)
        .ok_or_else(|| RuntimeError::Registry {
            detail: format!("no agent definition is registered under the name '{name}'"),
        })
}

/// Baut die Operations-Registry eines Laufs nach seiner [`OperationSurface`].
///
/// # Beschreibung
/// Die drei Flächen sind **wirklich** drei (Befund Z2c-01):
///
/// | Fläche | Operations-Registry | Modell-Tool-Provider |
/// |---|---|---|
/// | [`OperationSurface::AllWithModelTools`] | alle | ja |
/// | [`OperationSurface::CommandsOnly`] | alle außer den reinen Modell-Tools | **nein** |
/// | [`OperationSurface::None`] | leer | nein |
///
/// `CommandsOnly` lässt jede Operation weg, deren **einzige** deklarierte
/// Fläche [`Surface::ModelTool`] ist: sie wäre in dieser Registry über keinen
/// Weg mehr erreichbar. Operationen mit Command- *oder* Web-Fläche bleiben
/// vollständig — die Web-Routen von `Analyze`/`Web` hängen an
/// [`Surface::Web`], nicht an der Command-Fläche, und dürfen nicht
/// versehentlich mitverschwinden.
///
/// Den zweiten Teil der Trennung — den [`ModelToolProvider`] — montiert
/// [`install_operation_model_tools`]; er braucht die fertigen Dienste.
///
/// # Argumente
/// - `surface` ([`OperationSurface`]): die Fläche des Einstiegs.
/// - `plan` (`Option<&PlanServices>`): die geöffnete Planungsfläche; nur dann
///   werden die Plan-Operationen registriert.
fn build_operations(surface: OperationSurface, plan: Option<&PlanServices>) -> OperationRegistry {
    /// Registriert den vollen Satz: Kern-Operationen plus, falls die
    /// Planungsfläche offen ist, die Plan-Operationen.
    fn register_full(plan: Option<&PlanServices>) -> OperationRegistry {
        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        if let Some(plan) = plan {
            let _registered = harw_ops::register_plan_tools(&mut registry, &plan.plan_config);
        }
        registry
    }

    match surface {
        OperationSurface::None => OperationRegistry::new(),
        OperationSurface::AllWithModelTools => register_full(plan),
        OperationSurface::CommandsOnly => {
            let all = register_full(plan);
            let mut commands = OperationRegistry::new();
            for operation in all.iter().filter(|operation| {
                operation
                    .meta()
                    .surfaces
                    .iter()
                    .any(|declared| !matches!(declared, Surface::ModelTool { .. }))
            }) {
                commands.register(Arc::clone(operation));
            }
            commands
        }
    }
}

/// Hängt die Modell-Tool-Fläche der Operationen an die Registry — oder nicht.
///
/// # Beschreibung
/// Der zweite und eigentliche Teil von Z2c-01. Bis W2c legte die Montage jeder
/// Fläche dieselbe volle Operations-Registry in die `ServiceMap`, und ob eine
/// Operation dem **Modell** als Werkzeug angeboten wurde, entschied allein der
/// Einstieg außerhalb der Montage (die inzwischen entfernten Modell-Tool-
/// Montagen der TUI in `harw-tui/src/app.rs` und des One-Shot-Pfads in
/// `harw-cli/src/chat.rs`). `Analyze` und `Web`, denen die
/// Vertragstabelle „nur Commands" zuspricht, bekamen damit faktisch die volle
/// Modell-Tool-Fläche.
///
/// Nur [`OperationSurface::AllWithModelTools`] registriert deshalb einen
/// [`ModelToolProvider`]. Er filtert selbst auf Operationen mit
/// [`Surface::ModelTool`] (`ModelToolProvider::new` →
/// `ModelToolAdapter::from_operation`), und seine Werkzeugnamen tauchen danach
/// in [`RuntimeAssembly::rights_snapshot`]`.tools` auf — dort ist die Trennung
/// prüfbar.
///
/// # Argumente
/// - `builder` ([`ExtensionRegistryBuilder`]): der Registry-Bauer des Laufs.
/// - `surface` ([`OperationSurface`]): die Fläche des Einstiegs.
/// - `operations` (`&Arc<OperationRegistry>`): die fertige Registry **nach**
///   den Contributors.
/// - `services` (`&Arc<RuntimeServices>`): die Dienstfabrik des Laufs; jeder
///   Aufruf baut daraus die Map der Fläche [`ServiceSurface::ModelTool`].
///
/// # Autorität
/// Die Sandbox kommt aus dem `ToolExecutionContext`, den der Turn-Loop stellt
/// — **nie** aus Modell-Argumenten. Die Fabrik bekommt nur diesen Kontext zu
/// sehen (`OpContextFactory`, `harw-operations/src/adapter/model_tool.rs:52`).
fn install_operation_model_tools(
    builder: ExtensionRegistryBuilder,
    surface: OperationSurface,
    operations: &Arc<OperationRegistry>,
    services: &Arc<RuntimeServices>,
) -> ExtensionRegistryBuilder {
    match surface {
        OperationSurface::CommandsOnly | OperationSurface::None => builder,
        OperationSurface::AllWithModelTools => {
            let exposed: Vec<Arc<dyn Operation>> = operations.iter().map(Arc::clone).collect();
            let services = Arc::clone(services);
            let provider = ModelToolProvider::new(exposed, move |execution_context| {
                OpContext::new(
                    execution_context.session_id().clone(),
                    execution_context.turn_id().clone(),
                    execution_context.sandbox().clone(),
                    services.service_map(ServiceSurface::ModelTool),
                )
            });
            builder.tool_provider(Arc::new(provider))
        }
    }
}

/// Die Namen aller in `registry` sichtbaren Werkzeuge, sortiert und
/// dublettenfrei.
fn registered_tool_names(registry: &ExtensionRegistry) -> Vec<String> {
    let mut names: Vec<String> = registry
        .tool_providers()
        .iter()
        .flat_map(|provider| provider.tools())
        .map(|spec| spec.name().to_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Die Leihgaben, aus denen [`build_spawner`] den Spawner baut.
///
/// # Beschreibung
/// Die neun Werte stammen aus verschiedenen, voneinander unabhängigen
/// Montageschritten (Konfiguration, Projekt, Freigabekette, Modell,
/// Wurzelidentität, Spawn-Kontext, Effort, Aktivierung, gesenkte Rollen). Sie
/// stehen hier in **einem** Typ, weil elf Positionsparameter an der einen
/// Aufrufstelle nicht mehr lesbar wären — und weil ein unterdrückter
/// `clippy::too_many_arguments` in dieser Welle ausgeschlossen ist. Die
/// benannten Felder sagen an der Aufrufstelle, woher jeder Wert kommt.
///
/// Alle Felder sind Leihgaben der Montage; geklont wird erst dort, wo
/// `ManagedAgentSpawner` Eigentum verlangt.
struct SpawnerInputs<'a> {
    /// Die aufgelöste Konfiguration; gelesen werden nur das Vorgabemodell und
    /// die daraus abgeleiteten Kindlimits.
    config: &'a ResolvedConfig,
    /// Der **einmal** erkannte Projektkontext; die Kind-Fabrik hält ihn.
    project: &'a ProjectContext,
    /// Die Wurzelkette; jedes Kind leitet daraus [`ApprovalChain::for_child`] ab.
    chain: &'a ApprovalChain,
    /// Der Modellanbieter des Wurzel-Turns; jedes Kind benutzt denselben.
    model: &'a Arc<dyn ModelProvider>,
    /// Die Kennung der Wurzelsitzung, unter der der Spawner sie registriert.
    root_session_id: &'a SessionId,
    /// Der eine Spawn-Kontext des Laufs (Sandbox, Trace, Decke).
    spawn_context: &'a SpawnContext,
    /// Der gewählte Reasoning-Effort, falls einer gesetzt ist.
    reasoning_effort: Option<harw_types::ReasoningEffort>,
    /// Die Basis-Aktivierung der Wurzel; Kinder werden dagegen geschnitten.
    activation: &'a SessionActivation,
    /// Die **einmal** gesenkten eingebauten Rollen.
    definitions: &'a HashMap<String, ExecutableAgentIr>,
}

/// Montiert den Spawner eines Laufs nach seiner [`SpawnerPolicy`].
///
/// # Rückgabe
/// `(Option<Arc<ManagedAgentSpawner>>, Vec<String>)` — bei
/// [`SpawnerPolicy::None`] `(None, vec![])`, sonst der Spawner und die
/// registrierten Rollennamen.
fn build_spawner(
    policy: SpawnerPolicy,
    inputs: SpawnerInputs<'_>,
    session_events: Option<UnboundedSender<SessionEvent>>,
) -> RuntimeResult<(Option<Arc<ManagedAgentSpawner>>, Vec<String>)> {
    // Erschöpfend statt `if policy == …`: eine künftige Variante (etwa
    // `ConfiguredRoles`) fiele sonst still in den `BuiltinRoles`-Zweig,
    // statt den Compiler zu brechen (Befund Z2c-03).
    match policy {
        SpawnerPolicy::None => return Ok((None, Vec::new())),
        SpawnerPolicy::BuiltinRoles => {}
    }

    let SpawnerInputs {
        config,
        project,
        chain,
        model,
        root_session_id,
        spawn_context,
        reasoning_effort,
        activation,
        definitions,
    } = inputs;

    let events = session_events.ok_or_else(|| RuntimeError::Spawner {
        detail: "an entry with a child spawner needs a session event sender \
                 (RuntimeAssemblyBuilder::session_events)"
            .to_owned(),
    })?;

    let factory: Arc<dyn ChildRegistryFactory> =
        Arc::new(RuntimeChildRegistryFactory::with_definitions(
            project.clone(),
            Arc::clone(model),
            chain.clone(),
            definitions.clone(),
        ));

    let model_id = config.harness.default_model.as_deref().unwrap_or_default();
    let manager = Arc::new(std::sync::Mutex::new(SessionManager::new(events)));
    let mut spawner = ManagedAgentSpawner::new(manager, child_limits(config, model_id));
    let mut roles: Vec<String> = Vec::with_capacity(role_names::ALL.len());
    for role in role_names::ALL {
        spawner = spawner.with_role(
            (*role).to_owned(),
            AgentRole::Agent {
                name: (*role).to_owned(),
            },
            AgentRoleId::Worker,
            Arc::clone(&factory),
        );
        roles.push((*role).to_owned());
    }
    roles.sort();

    let spawner = spawner
        .with_external_root_parent(
            root_session_id.clone(),
            spawn_context.clone(),
            reasoning_effort,
            activation.clone(),
        )
        .map_err(|error| RuntimeError::Spawner {
            detail: format!("could not register the trusted runtime root parent: {error}"),
        })?;

    Ok((Some(Arc::new(spawner)), roles))
}

/// Die fertige Montage eines Laufs.
///
/// # Nebenläufigkeit
/// `Send + Sync`. Die beiden `Mutex`-Felder halten das, was **genau einmal**
/// vergeben wird (die Wurzel-Registry) bzw. nachträglich gesetzt wird (der
/// Freigabe-Responder); alles andere ist nach [`RuntimeAssemblyBuilder::build`]
/// unveränderlich.
pub struct RuntimeAssembly {
    spec: RuntimeSpec,
    profile: EntryProfile,
    config: Arc<ResolvedConfig>,
    trust_report: ConfigTrustReport,
    project: ProjectContext,
    sandbox: SandboxSpec,
    ceiling: ContextCeiling,
    spawn_context: SpawnContext,
    budget: RootBudget,
    turn_limits: TurnLimits,
    network_scope: NetworkScope,
    chain: ApprovalChain,
    approval_mode: ApprovalModeCell,
    agent_ir: Option<ExecutableAgentIr>,
    activation: SessionActivation,
    operations: Arc<OperationRegistry>,
    /// Hinter einem `Arc`, weil die Modell-Tool-Fläche der Operationen
    /// dieselbe Fabrik in ihrer Kontext-Closure hält
    /// ([`install_operation_model_tools`]).
    services: Arc<RuntimeServices>,
    model: Arc<dyn ModelProvider>,
    stores: RuntimeStores,
    spawner: Option<Arc<ManagedAgentSpawner>>,
    spawner_roles: Vec<String>,
    lifecycle_hooks: Vec<Arc<dyn SessionLifecycleHook>>,
    tools: Vec<String>,
    root_session_id: SessionId,
    registry: Mutex<Option<ExtensionRegistry>>,
    responder: Mutex<Option<Arc<dyn ApprovalHandler>>>,
}

impl std::fmt::Debug for RuntimeAssembly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeAssembly")
            .field("entry", &self.spec.entry)
            .field("project_root", &self.project.project_root)
            .field("root_session_id", &self.root_session_id)
            .field("tools", &self.tools.len())
            .field("spawner_roles", &self.spawner_roles)
            .finish_non_exhaustive()
    }
}

impl RuntimeAssembly {
    /// Beginnt eine Montage.
    ///
    /// # Argumente
    /// - `spec` ([`RuntimeSpec`]): die Eingangsbeschreibung des Laufs.
    ///
    /// # Rückgabe
    /// Einen [`RuntimeAssemblyBuilder`] ohne Modell und ohne Speicher; beides
    /// ist Pflicht.
    #[must_use]
    pub fn builder(spec: RuntimeSpec) -> RuntimeAssemblyBuilder {
        RuntimeAssemblyBuilder {
            spec,
            model: None,
            stores: None,
            plan_services: None,
            memory: None,
            session_controller: None,
            session_events: None,
            contributors: Vec::new(),
            root_session_id: None,
            secret_resolver: None,
            narrowing: None,
        }
    }

    /// Die Eingangsbeschreibung dieses Laufs.
    #[must_use]
    pub const fn spec(&self) -> &RuntimeSpec {
        &self.spec
    }

    /// Das Profil, aus dem **alle** Rechte dieses Laufs folgen.
    #[must_use]
    pub const fn profile(&self) -> &EntryProfile {
        &self.profile
    }

    /// Die aufgelöste Konfiguration.
    #[must_use]
    pub fn config(&self) -> &Arc<ResolvedConfig> {
        &self.config
    }

    /// Der Vertrauensbericht der Konfigurationsschichten.
    #[must_use]
    pub const fn trust_report(&self) -> &ConfigTrustReport {
        &self.trust_report
    }

    /// Der **einmalig** ermittelte Projektkontext.
    #[must_use]
    pub const fn project(&self) -> &ProjectContext {
        &self.project
    }

    /// Der vertrauenswürdig ermittelte Aufrufer.
    #[must_use]
    pub const fn principal(&self) -> &Principal {
        &self.spec.principal
    }

    /// Der eine Spawn-Kontext dieses Laufs (Sandbox, Decke, Trace, Akteur).
    #[must_use]
    pub const fn spawn_context(&self) -> &SpawnContext {
        &self.spawn_context
    }

    /// Die Wurzel-Sandbox — dieselbe, die im [`SpawnContext`] steht.
    #[must_use]
    pub const fn sandbox(&self) -> &SandboxSpec {
        &self.sandbox
    }

    /// Die Kontext-Decke dieses Laufs.
    #[must_use]
    pub const fn ceiling(&self) -> &ContextCeiling {
        &self.ceiling
    }

    /// Die Basis-Aktivierung der Wurzel — die Werkzeugfläche, gegen die jedes
    /// Kind geschnitten wird.
    ///
    /// # Beschreibung
    /// Genau der Wert, mit dem
    /// [`ManagedAgentSpawner::with_external_root_parent`] registriert wurde
    /// (W2A-02) und den [`Self::new_root_session`] der Sitzung als
    /// `base_activation` gibt. Er ist hier lesbar, weil die Anzeige den
    /// Unterschied „vom Modus verengt" gegen „von der Agent-Definition
    /// verboten" braucht — `harw-tui`s `/tools` (W2A-01, „noch kein
    /// Aufrufer") ist der benannte Verbraucher in Welle W2d.
    #[must_use]
    pub const fn root_activation(&self) -> &SessionActivation {
        &self.activation
    }

    /// Das Budget des Wurzel-Agenten.
    #[must_use]
    pub const fn budget(&self) -> &RootBudget {
        &self.budget
    }

    /// Die daraus abgeleiteten Turn-Grenzwerte (Durchsetzung: W4a).
    #[must_use]
    pub const fn turn_limits(&self) -> &TurnLimits {
        &self.turn_limits
    }

    /// Der Netz-Scope dieses Laufs. Leer, solange kein
    /// [`AssemblyContributor`] ihn füllt (bis Welle W5/P1.7: immer).
    #[must_use]
    pub const fn network_scope(&self) -> &NetworkScope {
        &self.network_scope
    }

    /// Die Operations-Registry (Slash-Befehle und Modell-Tools).
    #[must_use]
    pub fn operations(&self) -> &Arc<OperationRegistry> {
        &self.operations
    }

    /// Die Dienst-Fabrik aller vier Flächen.
    #[must_use]
    pub fn services(&self) -> &RuntimeServices {
        &self.services
    }

    /// Die Plan-Dienste, die dem Builder übergeben wurden.
    ///
    /// # Beschreibung
    /// Delegiert an [`RuntimeServices::plan`]; es gibt genau eine
    /// Planungsfläche je Lauf (CONTRACTS-W2d2 §1.1).
    ///
    /// # Rückgabe
    /// `Some(&PlanServices)` nach [`RuntimeAssemblyBuilder::plan_services`],
    /// sonst `None`.
    #[must_use]
    pub fn plan_services(&self) -> Option<&PlanServices> {
        self.services.plan()
    }

    /// Das Gedächtnis, das dem Builder übergeben wurde.
    ///
    /// # Beschreibung
    /// Delegiert an [`RuntimeServices::memory`] (CONTRACTS-W2d2 §1.1).
    ///
    /// # Rückgabe
    /// `Some(&Arc<dyn Memory>)` nach [`RuntimeAssemblyBuilder::memory`],
    /// sonst `None`.
    #[must_use]
    pub fn memory(&self) -> Option<&Arc<dyn Memory>> {
        self.services.memory()
    }

    /// Die Freigabemodus-Zelle dieses Laufs.
    ///
    /// # Beschreibung
    /// Dieselbe Zelle, die in jeder Service-Map liegt, die die Freigabekette
    /// liest und die [`Self::new_root_session`] als
    /// [`RootSession::approval_mode`] ausgibt; Klone teilen ihren Zustand.
    #[must_use]
    pub const fn approval_mode(&self) -> &ApprovalModeCell {
        &self.approval_mode
    }

    /// Der Wurzel-Modellanbieter.
    #[must_use]
    pub fn model(&self) -> &Arc<dyn ModelProvider> {
        &self.model
    }

    /// Der Verlaufsspeicher.
    #[must_use]
    pub fn state_store(&self) -> &Arc<dyn StateStore> {
        &self.stores.state_store
    }

    /// Der Job-Speicher, falls der Einstieg einen hat.
    #[must_use]
    pub fn job_store(&self) -> Option<&Arc<JobStore>> {
        self.stores.job_store.as_ref()
    }

    /// Der Speicher durabler Freigaben, falls der Einstieg einen hat.
    #[must_use]
    pub fn approval_store(&self) -> Option<&Arc<ApprovalStore>> {
        self.stores.approval_store.as_ref()
    }

    /// Der Kind-Spawner, falls der Einstieg einen hat.
    #[must_use]
    pub fn spawner(&self) -> Option<&Arc<ManagedAgentSpawner>> {
        self.spawner.as_ref()
    }

    /// Die Kennung der Wurzelsitzung.
    ///
    /// # Beschreibung
    /// Beim Bauen geprägt (oder über
    /// [`RuntimeAssemblyBuilder::root_session_id`] vorgegeben) und beim
    /// Spawner als vertrauenswürdige Wurzel registriert.
    /// [`Self::new_root_session`] nimmt genau diese Kennung entgegen.
    #[must_use]
    pub const fn root_session_id(&self) -> &SessionId {
        &self.root_session_id
    }

    /// Baut einen [`OpContext`] für eine Fläche.
    ///
    /// # Argumente
    /// - `surface` ([`ServiceSurface`]): die Fläche, deren Dienste sichtbar
    ///   sein sollen.
    /// - `session` ([`SessionId`]) / `turn` ([`TurnId`]): die laufende Arbeit.
    /// - `sandbox` ([`SandboxSpec`]): die **bereits verengte** Sandbox dieses
    ///   Aufrufs. Bewusst ein Parameter und kein Griff nach
    ///   [`Self::sandbox`]: der Web-Einstieg schneidet sie je Verbindung nach
    ///   Tier (`permissions_for_tier`, F-045), und diese Verengung darf die
    ///   Montage nicht versehentlich umgehen.
    #[must_use]
    pub fn op_context(
        &self,
        surface: ServiceSurface,
        session: SessionId,
        turn: TurnId,
        sandbox: SandboxSpec,
    ) -> OpContext {
        OpContext::new(session, turn, sandbox, self.services.service_map(surface))
    }

    /// Erzeugt die **eine** Wurzelsitzung dieses Laufs.
    ///
    /// # Argumente
    /// - `id` ([`SessionId`]): muss [`Self::root_session_id`] entsprechen.
    /// - `events` (`UnboundedSender<SessionEvent>`): Ereigniskanal der Sitzung.
    /// - `turn_events` (`UnboundedSender<TurnEvent>`): Kanal der Turn-Ereignisse.
    /// - `responder` (`Option<Arc<dyn ApprovalHandler>>`): die Antwortfläche
    ///   für Rückfragen. Sie wird **hier** an die Kette gehängt, nicht bei der
    ///   Montage: nur ein Einstieg mit
    ///   [`crate::spec::AskResolution::Interactive`] bringt einen mit, und
    ///   Kind-Sitzungen erben ihn nie ([`ApprovalChain::for_child`]).
    ///
    /// # Rückgabe
    /// Eine [`RootSession`] mit gesetztem Spawn-Kontext, Agent-IR
    /// (`spec.active_agent`), Reasoning-Effort und — falls
    /// `spec.mode_override` gesetzt ist — Interaktionsmodus.
    ///
    /// # Fehler
    /// - [`RuntimeError::Spawner`], wenn `id` nicht der registrierten
    ///   Wurzelkennung entspricht.
    /// - [`RuntimeError::Registry`], wenn ein Responder übergeben wird, obwohl
    ///   die [`crate::spec::AskResolution`] des Einstiegs **nicht**
    ///   [`AskResolution::Interactive`] ist; wenn bereits eine Wurzelsitzung
    ///   erzeugt wurde (die Registry wird genau einmal vergeben); oder wenn
    ///   der Mutex vergiftet ist.
    pub fn new_root_session(
        &self,
        id: SessionId,
        events: UnboundedSender<SessionEvent>,
        turn_events: UnboundedSender<TurnEvent>,
        responder: Option<Arc<dyn ApprovalHandler>>,
    ) -> RuntimeResult<RootSession> {
        if id != self.root_session_id {
            return Err(RuntimeError::Spawner {
                detail: format!(
                    "refusing to build a root session under '{id}': the runtime registered \
                     '{}' as its trusted root",
                    self.root_session_id
                ),
            });
        }

        // Fail-closed gegen die Vertragstabelle (Befund Z2c-02): nur ein
        // Einstieg mit `AskResolution::Interactive` hat jemanden, der antwortet.
        // Ein Responder an einem anderen Einstieg montierte eine
        // Rückfragefläche, die die Tabelle ihm verweigert — und der Snapshot
        // wiese sie danach als `Interactive`/`Channel` aus.
        if responder.is_some() && self.profile.ask != AskResolution::Interactive {
            return Err(RuntimeError::Registry {
                detail: format!(
                    "refusing an approval responder for entry {:?}: its ask resolution is {:?}, \
                     not Interactive — nobody may be asked here",
                    self.spec.entry, self.profile.ask
                ),
            });
        }

        let mut slot = self.registry.lock().map_err(|_| RuntimeError::Registry {
            detail: "the runtime registry lock is poisoned".to_owned(),
        })?;
        let registry = slot.take().ok_or_else(|| RuntimeError::Registry {
            detail: "this runtime has already handed out its root registry".to_owned(),
        })?;
        drop(slot);

        let registry = match responder.clone() {
            Some(handler) => registry.into_builder().approval_handler(handler).build(),
            None => registry,
        };
        if let Some(handler) = responder {
            let mut stored = self.responder.lock().map_err(|_| RuntimeError::Registry {
                detail: "the runtime responder lock is poisoned".to_owned(),
            })?;
            *stored = Some(handler);
        }

        let mut session =
            AgentSession::new_with_id(id, AgentRole::Assistant, None, registry, events)
                .with_spawn_context(self.spawn_context.clone())
                .with_reasoning_effort(self.spec.reasoning_effort)
                .with_turn_event_sink(turn_events);
        if let Some(ir) = self.agent_ir.as_ref() {
            session = session.with_executable_agent_ir(ir);
        }
        if let Some(mode) = self.spec.mode_override {
            session.set_mode(mode);
        }

        tracing::info!(
            entry = ?self.spec.entry,
            session_id = %self.root_session_id,
            mode = session.mode().as_str(),
            approval_mode = ?self.approval_mode.get(),
            "runtime.root_session.created"
        );

        Ok(RootSession {
            session,
            approval_mode: self.approval_mode.clone(),
        })
    }

    /// Meldet allen [`SessionLifecycleHook`]s das Ende einer Sitzung.
    ///
    /// # Argumente
    /// - `id` (`&SessionId`): die beendete Sitzung (Wurzel oder Kind).
    ///
    /// # Beschreibung
    /// Ruft **jeden** Haken, auch wenn ein früherer lange braucht; ein Haken
    /// kann nichts verhindern und nichts überspringen.
    pub fn close_session(&self, id: &SessionId) {
        for hook in &self.lifecycle_hooks {
            hook.on_session_closed(id);
        }
        tracing::debug!(
            session_id = %id,
            hooks = self.lifecycle_hooks.len(),
            "runtime.session.closed"
        );
    }

    /// Die seiteneffektfreie Momentaufnahme der effektiven Rechte.
    ///
    /// # Beschreibung
    /// Liest ausschließlich bereits montierten Zustand: sie baut nichts, öffnet
    /// nichts und fragt kein Modell. Gedacht für `harw doctor`, die
    /// Rechte-Matrix-Tests und jede Diagnose, die wissen muss, was ein Einstieg
    /// *tatsächlich* darf.
    ///
    /// # Rückgabe
    /// Eine [`crate::spec::RightsSnapshot`]; `permissions`, `tools`,
    /// `ceiling_sections`, `config_policy_tools` und `spawner_roles` sind
    /// sortiert und dublettenfrei, `approval_chain` steht in
    /// Auswertungsreihenfolge.
    #[must_use]
    pub fn rights_snapshot(&self) -> crate::spec::RightsSnapshot {
        let mut permissions: Vec<String> = self
            .sandbox
            .permissions()
            .iter()
            .map(|permission| format!("{permission:?}"))
            .collect();
        permissions.sort();

        let mut approval_chain: Vec<(&'static str, ApprovalHandlerKind)> = self.chain.snapshot();
        // Bewusst keine verschachtelten `if let`: `clippy::collapsible_if`
        // greift seit Clippy 1.88 auch auf `if let`-Ketten, und let-chains
        // stehen unter MSRV 1.85 noch nicht zur Verfügung (Befund Z2c-09).
        approval_chain.extend(self.responder.lock().ok().and_then(|responder| {
            responder
                .as_ref()
                .map(|handler| (handler.label(), handler.kind()))
        }));

        let mut ceiling_sections: Vec<String> = self
            .ceiling
            .sections
            .iter()
            .map(|section| section.as_str().to_owned())
            .collect();
        ceiling_sections.sort();

        crate::spec::RightsSnapshot {
            entry: self.spec.entry,
            principal: self.spec.principal.clone(),
            approval_actor: self.spec.principal.approval_actor(),
            permissions,
            tools: self.tools.clone(),
            approval_chain,
            config_policy_tools: self.chain.config_policy_tools(),
            ceiling_sections,
            spawner_roles: self.spawner_roles.clone(),
            budget: self.budget,
            untrusted_repo: self.trust_report.untrusted_repo.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_limits_never_exceed_the_root_budget() {
        let budget = RootBudget {
            max_model_rounds: 64,
            max_total_tokens: 2_000_000,
            max_wall: Duration::from_secs(3600),
        };
        let limits = TurnLimits::from_root_budget(&budget);
        assert_eq!(limits.max_model_rounds, budget.max_model_rounds);
        assert_eq!(limits.max_output_tokens_total, budget.max_total_tokens);
        assert_eq!(limits.wall_time, budget.max_wall);
        assert_eq!(limits.max_tool_calls, 64 * TOOL_CALLS_PER_ROUND);
        assert_eq!(limits.tool_result_max_bytes, TOOL_RESULT_MAX_BYTES);
    }

    #[test]
    fn turn_limits_saturate_instead_of_wrapping() {
        let budget = RootBudget {
            max_model_rounds: u32::MAX,
            max_total_tokens: 1,
            max_wall: Duration::from_secs(1),
        };
        assert_eq!(TurnLimits::from_root_budget(&budget).max_tool_calls, u32::MAX);
    }

    /// Der Rustdoc von [`default_approval_mode`] sagt „immer `Delegated`" —
    /// dann ist das auch die Zusage, die der Test prüft (Befund Z2c-12).
    #[test]
    fn every_entry_starts_delegated() {
        for entry in ALL_ENTRIES {
            assert_eq!(default_approval_mode(entry), ApprovalMode::Delegated, "{entry:?}");
        }
    }

    #[test]
    fn only_spawning_entries_are_root_orchestrators() {
        for entry in ALL_ENTRIES {
            let profile = entry.profile();
            let role = root_organizational_role(profile.spawner);
            match profile.spawner {
                SpawnerPolicy::BuiltinRoles => assert_eq!(role, AgentRoleId::RootOrchestrator),
                SpawnerPolicy::None => assert_eq!(role, AgentRoleId::Worker),
            }
        }
    }

    /// Die gesenkten eingebauten Rollen einer leeren Konfiguration.
    fn builtin() -> (ResolvedConfig, HashMap<String, ExecutableAgentIr>) {
        let config = ResolvedConfig::default();
        let definitions = lower_agent_definitions(&config).expect("Rollen senken");
        (config, definitions)
    }

    #[test]
    fn an_unknown_agent_name_fails_closed() {
        let (config, definitions) = builtin();
        let error = resolve_active_agent(Some("definitely-not-a-role"), &config, &definitions)
            .unwrap_err();
        assert!(matches!(error, RuntimeError::Registry { .. }));
    }

    #[test]
    fn a_known_agent_name_resolves_and_narrows_the_activation() {
        let (config, definitions) = builtin();
        let ir = resolve_active_agent(Some(role_names::EXPLORER), &config, &definitions)
            .expect("explorer resolves")
            .expect("explorer exists");
        let activation = root_activation(Some(&ir));
        // `Minimal` ist deny-by-default: nur ausdrücklich admittierte
        // Werkzeuge sind sichtbar. `SessionActivation` hat kein `PartialEq`,
        // deshalb wird die Fläche verhaltensbasiert geprüft.
        assert_eq!(activation.profile(), ToolProfile::Minimal);
        assert!(
            ir.tool_surface()
                .admitted()
                .iter()
                .all(|name| activation.is_tool_enabled(&ToolName::new(name.clone()))),
            "jede admittierte Fähigkeit der IR ist aktiviert"
        );
        assert!(
            ir.tool_surface()
                .forbidden()
                .iter()
                .all(|name| !activation.is_tool_enabled(&ToolName::new(name.clone()))),
            "keine verbotene Fähigkeit der IR ist aktiviert"
        );
    }

    #[test]
    fn without_an_agent_the_activation_cuts_nothing() {
        let (config, definitions) = builtin();
        assert!(
            resolve_active_agent(None, &config, &definitions)
                .expect("none")
                .is_none()
        );
        assert_eq!(root_activation(None).profile(), ToolProfile::default());
    }

    /// Z2c-05: Eine in `[agents]` konfigurierte Rolle löst auf — und geht der
    /// gleichnamigen eingebauten vor.
    #[test]
    fn a_configured_agent_wins_over_the_builtin_one() {
        let (mut config, definitions) = builtin();
        let explorer = definitions
            .get(role_names::EXPLORER)
            .expect("explorer exists")
            .clone();
        config
            .executable_agents
            .insert("hausrolle".to_owned(), explorer);

        let expected = definitions
            .get(role_names::EXPLORER)
            .expect("explorer")
            .tool_surface()
            .admitted()
            .to_vec();
        let ir = resolve_active_agent(Some("hausrolle"), &config, &definitions)
            .expect("konfigurierte Rolle löst auf")
            .expect("sie existiert");
        assert_eq!(ir.tool_surface().admitted(), expected.as_slice());
    }

    /// Z2c-01: Die drei Operations-Flächen sind wirklich drei.
    #[test]
    fn the_operation_surface_decides_what_is_registered() {
        let none = build_operations(OperationSurface::None, None);
        assert!(none.is_empty());

        let all = build_operations(OperationSurface::AllWithModelTools, None);
        assert!(!all.is_empty(), "harw-ops registriert Operationen");
        let model_tools = all
            .by_surface(|surface| matches!(surface, Surface::ModelTool { .. }))
            .len();
        assert!(model_tools > 0, "es gibt überhaupt Modell-Tool-Operationen");

        let commands = build_operations(OperationSurface::CommandsOnly, None);
        // Keine Operation, deren einzige Fläche `ModelTool` ist.
        for operation in commands.iter() {
            assert!(
                operation
                    .meta()
                    .surfaces
                    .iter()
                    .any(|surface| !matches!(surface, Surface::ModelTool { .. })),
                "{} hätte in CommandsOnly keine erreichbare Fläche",
                operation.meta().name
            );
        }
    }

    /// Alle Einstiege; das `match` in `crate::spec` hält die Liste vollständig.
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

    #[test]
    fn ceiling_policy_is_reflected_in_the_spawn_context() {
        for entry in ALL_ENTRIES {
            let profile = entry.profile();
            let ceiling = root_ceiling(profile.ceiling);
            if profile.ceiling == crate::spec::CeilingPolicy::Closed {
                assert!(ceiling.sections.is_empty(), "{entry:?}");
            } else {
                assert!(!ceiling.sections.is_empty(), "{entry:?}");
            }
        }
    }

    /// Befund C2a: `build()` reichte `secrets:`-Referenzen bis heute nicht an
    /// [`build_root_model_with_resolver`] durch, weil der Builder keinen
    /// Resolver kannte. Ein Test, der [`RuntimeAssemblyBuilder::build`]
    /// tatsächlich durchläuft, bräuchte zusätzlich [`RuntimeStores`] (ein
    /// durabler `StateStore`) und — je nach [`SpawnerPolicy`] des Einstiegs —
    /// einen `session_events`-Sender samt `SessionManager`; beide Bauteile
    /// liegen außerhalb der Read-list dieses Agenten (`harw-session-store`,
    /// `harw-core::SessionManager`) und außerhalb der owned files. Geprüft
    /// wird deshalb der unstrittige Teil: [`RuntimeAssemblyBuilder::secret_resolver`]
    /// setzt genau das Feld, das `build()` (siehe die Bau-Stelle oben) per
    /// `secret_resolver.as_deref().map(|r| r as &dyn SecretResolver)` an
    /// [`build_root_model_with_resolver`] weiterreicht.
    ///
    /// Befund W7 (Z2d-1-Review): der ursprüngliche Testname versprach, dass der
    /// Resolver tatsächlich für ein konfiguriertes Modell *verwendet* wird —
    /// geprüft wird aber nur, dass der Builder das Feld setzt. Umbenannt, um
    /// Testname und Assertion in Deckung zu bringen; die Assertion selbst ist
    /// unverändert.
    #[test]
    fn test_builder_secret_resolver_sets_field() {
        struct FakeResolver;
        impl SecretResolver for FakeResolver {
            fn resolve(&self, _reference: &str) -> Result<secrecy::SecretString, String> {
                Ok(secrecy::SecretString::from("fake-secret".to_owned()))
            }
        }

        let spec = RuntimeSpec {
            entry: EntryKind::OneShot,
            home: std::path::PathBuf::from("/nonexistent-home"),
            cwd: std::path::PathBuf::from("/nonexistent-cwd"),
            principal: harw_types::Principal::trusted_ingress(
                harw_types::PrincipalKind::Human,
                "test",
                harw_types::IngressSurface::Tui,
                harw_types::PermissionTier::Owner,
            ),
            mode_override: None,
            active_agent: None,
            reasoning_effort: None,
        };

        let builder = RuntimeAssembly::builder(spec).secret_resolver(Arc::new(FakeResolver));

        assert!(
            builder.secret_resolver.is_some(),
            "secret_resolver() muss das Feld setzen, das build() beim Bau des \
             Wurzel-Modells an build_root_model_with_resolver reicht"
        );
    }

    /// Befund W8 (Z2d-1-Review): `web.rs` ist die erste Aufrufstelle, die
    /// `RuntimeAssembly` über einen `Send + Sync`-Grenze (Achsum-Handler)
    /// trägt; bis dahin gab es keinen Compile-Zeit-Beleg, dass der Typ diese
    /// Auto-Traits tatsächlich hält. Reiner Compile-Zeit-Test: schlägt beim
    /// Kompilieren fehl, falls `RuntimeAssembly` künftig ein `!Send`- oder
    /// `!Sync`-Feld bekommt.
    #[test]
    fn test_runtime_assembly_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RuntimeAssembly>();
    }

    // ── Accessoren und Verengung (CONTRACTS-W2d2 §1.1, R0) ──────────────────

    /// Ein leeres Projekt mit eigenem Root-Space — dasselbe Muster wie
    /// `fixture()` in `harw-runtime/tests/rights_matrix.rs`, ohne
    /// Prozess-Zustand (kein `set_current_dir`, kein `set_var`).
    struct BuildFixture {
        _dir: tempfile::TempDir,
        home: std::path::PathBuf,
        project: std::path::PathBuf,
    }

    fn build_fixture() -> BuildFixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        // Projekt-Marker, damit `discover_project` genau hier stehen bleibt.
        std::fs::write(project.join("Cargo.toml"), "[workspace]\n").expect("marker");
        BuildFixture {
            _dir: dir,
            home,
            project,
        }
    }

    /// Ein Builder mit Echo-Modell und In-Memory-Verlauf. Nur für Einstiege
    /// mit [`SpawnerPolicy::None`] baubar (kein `session_events` gesetzt).
    fn fixture_builder(entry: EntryKind, fixture: &BuildFixture) -> RuntimeAssemblyBuilder {
        let spec = RuntimeSpec {
            entry,
            home: fixture.home.clone(),
            cwd: fixture.project.clone(),
            principal: harw_types::Principal::trusted_ingress(
                harw_types::PrincipalKind::Human,
                "r0",
                harw_types::IngressSurface::Tui,
                harw_types::PermissionTier::Owner,
            ),
            mode_override: None,
            active_agent: None,
            reasoning_effort: None,
        };
        let state_store: Arc<dyn StateStore> = Arc::new(harw_core::InMemoryStateStore::new());
        RuntimeAssembly::builder(spec)
            .model(ModelSource::Echo("echo: r0".to_owned()))
            .stores(RuntimeStores {
                state_store,
                job_store: None,
                approval_store: None,
            })
    }

    /// Jede [`harw_sandbox::Permission`] — die weiteste denkbare Obergrenze.
    fn every_permission() -> PermissionSet {
        use harw_sandbox::Permission;
        PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
            Permission::ReadSecrets,
            Permission::ManagePlugins,
            Permission::ReadCargoRegistry,
        ])
    }

    #[test]
    fn test_plan_services_accessor_returns_builder_input() {
        let fixture = build_fixture();
        let findings = Arc::new(harw_plan_bridge::FindingStore::new("/nonexistent/r0/plans"));
        let plan = PlanServices {
            plan: Arc::new(harw_plan::InMemoryPlanStore::new()),
            goal: Arc::new(harw_plan::InMemoryGoalStore::new()),
            findings: Arc::clone(&findings),
            plan_config: harw_plan::PlanToolConfig::enabled_defaults(),
        };
        let assembly = fixture_builder(EntryKind::LocalEcho, &fixture)
            .plan_services(plan)
            .build()
            .expect("LocalEcho montiert");

        let Some(returned) = assembly.plan_services() else {
            panic!("plan_services() muss die Builder-Eingabe liefern");
        };
        assert!(
            Arc::ptr_eq(&returned.findings, &findings),
            "plan_services() liefert genau den übergebenen Wert"
        );
        let Some(via_services) = assembly.services().plan() else {
            panic!("RuntimeServices::plan() muss dieselbe Planungsfläche liefern");
        };
        assert!(Arc::ptr_eq(&via_services.findings, &findings));
    }

    #[test]
    fn test_memory_accessor_none_without_memory() {
        let fixture = build_fixture();
        let assembly = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .expect("LocalEcho montiert");
        assert!(assembly.memory().is_none());
        assert!(assembly.services().memory().is_none());
        assert!(assembly.plan_services().is_none());
    }

    #[test]
    fn test_approval_mode_accessor_shares_the_services_cell() {
        let fixture = build_fixture();
        let assembly = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .expect("LocalEcho montiert");
        assert_eq!(assembly.approval_mode().get(), ApprovalMode::Delegated);
        assembly.approval_mode().set(ApprovalMode::AlwaysAsk);
        assert_eq!(
            assembly.services().approval_mode().get(),
            ApprovalMode::AlwaysAsk,
            "approval_mode() ist dieselbe Zelle wie in den Service-Maps"
        );
    }

    #[test]
    fn test_narrowing_readonly_on_full_entry_drops_write_tools() {
        let fixture = build_fixture();
        let baseline = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .expect("LocalEcho montiert")
            .rights_snapshot();
        assert!(baseline.tools.iter().any(|tool| tool == "fs.write"));
        assert!(baseline.tools.iter().any(|tool| tool == "shell.exec"));

        let narrowed = fixture_builder(EntryKind::LocalEcho, &fixture)
            .narrowing(RuntimeNarrowing {
                registry_profile: RegistryProfile::ReadOnlyExplore,
                identity: IdentityOverrides::default(),
                permissions: PermissionSet::from_policy([harw_sandbox::Permission::ReadWorkspace]),
                workspace_root: None,
            })
            .build()
            .expect("Full → ReadOnlyExplore ist zugelassen");
        let snapshot = narrowed.rights_snapshot();

        assert!(!snapshot.tools.iter().any(|tool| tool == "fs.write"));
        assert!(!snapshot.tools.iter().any(|tool| tool == "shell.exec"));
        assert!(snapshot.tools.iter().any(|tool| tool == "fs.read"));
        let read_only = RegistryProfile::ReadOnlyExplore.registered_tool_names();
        assert!(
            snapshot
                .tools
                .iter()
                .all(|tool| read_only.contains(&tool.as_str())),
            "nur Werkzeuge des Profils ReadOnlyExplore: {:?}",
            snapshot.tools
        );
        assert_eq!(snapshot.permissions, vec!["ReadWorkspace".to_owned()]);
        assert_eq!(
            narrowed.spawn_context().sandbox.permissions(),
            narrowed.sandbox().permissions(),
            "der Spawn-Kontext trägt dieselbe verengte Sandbox"
        );
        assert_eq!(
            narrowed.profile().registry_profile,
            RegistryProfile::Full,
            "die Tabellenzeile selbst bleibt unverändert"
        );
    }

    #[test]
    fn test_narrowing_rejects_full_on_notools_entry() {
        let fixture = build_fixture();
        let error = fixture_builder(EntryKind::JobPrompt, &fixture)
            .narrowing(RuntimeNarrowing {
                registry_profile: RegistryProfile::Full,
                identity: IdentityOverrides::default(),
                permissions: every_permission(),
                workspace_root: None,
            })
            .build()
            .expect_err("NoTools → Full muss abgelehnt werden");
        assert!(matches!(error, RuntimeError::Registry { .. }), "{error}");

        // Die übrige Whitelist, geprüft an der reinen Entscheidung.
        use RegistryProfile::{Full, NoTools, Planning, ReadOnlyExplore, Research};
        let entry = EntryKind::JobPrompt;
        for requested in [Full, ReadOnlyExplore, Research, Planning] {
            assert!(
                narrowed_registry_profile(entry, NoTools, requested).is_err(),
                "NoTools → {requested:?}"
            );
        }
        for requested in [Research, Planning] {
            assert!(
                narrowed_registry_profile(entry, Full, requested).is_err(),
                "Full → {requested:?}"
            );
        }
        for entry_profile in [ReadOnlyExplore, Research, Planning] {
            for requested in RegistryProfile::ALL {
                assert!(
                    narrowed_registry_profile(entry, entry_profile, *requested).is_err(),
                    "{entry_profile:?} → {requested:?}"
                );
            }
        }
        for requested in [Full, ReadOnlyExplore, NoTools] {
            assert_eq!(
                narrowed_registry_profile(entry, Full, requested).ok(),
                Some(requested)
            );
        }

        // NoTools → NoTools montiert und bleibt werkzeuglos.
        let same = fixture_builder(EntryKind::JobPrompt, &fixture)
            .narrowing(RuntimeNarrowing {
                registry_profile: NoTools,
                identity: IdentityOverrides::default(),
                permissions: every_permission(),
                workspace_root: None,
            })
            .build()
            .expect("NoTools → NoTools ist zugelassen");
        assert!(same.rights_snapshot().tools.is_empty());
    }

    #[test]
    fn test_narrowing_permissions_never_exceed_profile() {
        let fixture = build_fixture();
        for entry in [
            EntryKind::LocalEcho,
            EntryKind::Doctor,
            EntryKind::Web,
            EntryKind::McpServe,
            EntryKind::JobPlanNode,
        ] {
            let profile = entry.profile();
            let assembly = fixture_builder(entry, &fixture)
                .narrowing(RuntimeNarrowing {
                    registry_profile: profile.registry_profile,
                    identity: IdentityOverrides::default(),
                    permissions: every_permission(),
                    workspace_root: None,
                })
                .build()
                .unwrap_or_else(|error| panic!("{entry:?} montiert nicht: {error}"));
            let granted = assembly.sandbox().permissions();
            assert!(granted.is_subset_of(&profile.permissions), "{entry:?}");
            assert_eq!(
                granted,
                &profile.permissions,
                "{entry:?}: die weiteste Obergrenze ergibt genau die Profilrechte"
            );
            assert_eq!(assembly.spawn_context().sandbox.permissions(), granted);
            assert!(!granted.contains(harw_sandbox::Permission::NetworkAccess));
            assert!(!granted.contains(harw_sandbox::Permission::ReadCargoRegistry));
        }

        // Eine engere Obergrenze schneidet; ein fremdes Recht kommt nie hinzu.
        let assembly = fixture_builder(EntryKind::LocalEcho, &fixture)
            .narrowing(RuntimeNarrowing {
                registry_profile: RegistryProfile::Full,
                identity: IdentityOverrides::default(),
                permissions: PermissionSet::from_policy([
                    harw_sandbox::Permission::ReadWorkspace,
                    harw_sandbox::Permission::NetworkAccess,
                ]),
                workspace_root: None,
            })
            .build()
            .expect("LocalEcho montiert");
        assert_eq!(
            assembly.sandbox().permissions(),
            &PermissionSet::from_policy([harw_sandbox::Permission::ReadWorkspace])
        );
    }

    #[test]
    fn test_root_identity_active_agent_wins_over_narrowing() {
        let fixture = build_fixture();
        let mut spec = fixture_builder(EntryKind::LocalEcho, &fixture).spec;
        let narrowing = RuntimeNarrowing {
            registry_profile: RegistryProfile::ReadOnlyExplore,
            identity: IdentityOverrides {
                agent_name: Some("plan-node".to_owned()),
                role_description: Some("research node".to_owned()),
                extra_context: vec!["node 7".to_owned()],
            },
            permissions: PermissionSet::empty(),
            workspace_root: None,
        };

        let replaced = root_identity(&spec, Some(&narrowing));
        assert_eq!(replaced.agent_name.as_deref(), Some("plan-node"));
        assert_eq!(replaced.role_description.as_deref(), Some("research node"));
        assert_eq!(replaced.extra_context, vec!["node 7".to_owned()]);

        spec.active_agent = Some("explorer".to_owned());
        let with_agent = root_identity(&spec, Some(&narrowing));
        assert_eq!(with_agent.agent_name.as_deref(), Some("explorer"));
        assert_eq!(with_agent.role_description.as_deref(), Some("research node"));

        let default = root_identity(&spec, None);
        assert_eq!(default.agent_name.as_deref(), Some("explorer"));
        assert!(default.role_description.is_none());
    }

    #[test]
    fn test_narrowing_workspace_root_descendant_binds_sandbox_to_it() {
        let fixture = build_fixture();
        let workspace = fixture.project.join("nested").join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let canonical_workspace = workspace.canonicalize().expect("canonical workspace");
        let canonical_project = fixture.project.canonicalize().expect("canonical project");

        // Projekterkennung ab dem Unterordner endet am markierten Elternprojekt.
        let mut builder = fixture_builder(EntryKind::JobPlanNode, &fixture);
        builder.spec.cwd.clone_from(&workspace);
        let assembly = builder
            .narrowing(RuntimeNarrowing {
                registry_profile: RegistryProfile::ReadOnlyExplore,
                identity: IdentityOverrides::default(),
                permissions: PermissionSet::from_policy([harw_sandbox::Permission::ReadWorkspace]),
                workspace_root: Some(workspace.clone()),
            })
            .build()
            .expect("ein Nachfahre des Projekt-Roots ist zugelassen");

        assert_eq!(
            assembly.project().project_root,
            canonical_project,
            "Projekterkennung bleibt am markierten Projekt"
        );
        assert_eq!(
            assembly.sandbox().workspace().canonical_root(),
            canonical_workspace.as_path(),
            "die Sandbox ist an den Workspace-Unterordner gebunden"
        );
        assert_eq!(
            assembly.spawn_context().sandbox.workspace().canonical_root(),
            canonical_workspace.as_path(),
            "der Spawn-Kontext trägt dieselbe engere Bindung"
        );
        assert_eq!(
            assembly.sandbox().permissions(),
            &PermissionSet::from_policy([harw_sandbox::Permission::ReadWorkspace])
        );

        // Gleichheit mit dem Projekt-Root ist ebenfalls zugelassen.
        let same = fixture_builder(EntryKind::LocalEcho, &fixture)
            .narrowing(RuntimeNarrowing {
                registry_profile: RegistryProfile::Full,
                identity: IdentityOverrides::default(),
                permissions: every_permission(),
                workspace_root: Some(fixture.project.clone()),
            })
            .build()
            .expect("der Projekt-Root selbst ist zugelassen");
        assert_eq!(
            same.sandbox().workspace().canonical_root(),
            canonical_project.as_path()
        );
    }

    #[test]
    fn test_narrowing_workspace_root_outside_project_is_rejected() {
        let fixture = build_fixture();
        let Some(parent) = fixture.project.parent().map(Path::to_path_buf) else {
            panic!("das Fixture-Projekt hat ein Elternverzeichnis");
        };
        let sibling = parent.join("sibling");
        std::fs::create_dir_all(&sibling).expect("sibling");

        let narrowing_to = |root: PathBuf| RuntimeNarrowing {
            registry_profile: RegistryProfile::Full,
            identity: IdentityOverrides::default(),
            permissions: every_permission(),
            workspace_root: Some(root),
        };

        for (label, root) in [
            ("Elternverzeichnis", parent.clone()),
            ("Geschwisterverzeichnis", sibling.clone()),
            // String-Präfix, aber kein Pfad-Nachfahre: `<tmp>/project` vs. `<tmp>/project-evil`.
            ("Präfix-Geschwister", {
                let evil = parent.join("project-evil");
                std::fs::create_dir_all(&evil).expect("prefix sibling");
                evil
            }),
            ("Ausbruch über ..", fixture.project.join("..").join("sibling")),
            ("fehlendes Verzeichnis", fixture.project.join("missing")),
            ("relativer Pfad", PathBuf::from("nested")),
        ] {
            let error = fixture_builder(EntryKind::LocalEcho, &fixture)
                .narrowing(narrowing_to(root))
                .build()
                .expect_err(label);
            assert!(
                matches!(error, RuntimeError::Sandbox { .. }),
                "{label}: {error}"
            );
        }

        // Ein Symlink im Projekt, der hinausführt, wird nach der Kanonisierung abgelehnt.
        #[cfg(unix)]
        {
            let link = fixture.project.join("escape-link");
            std::os::unix::fs::symlink(&sibling, &link).expect("symlink");
            let error = fixture_builder(EntryKind::LocalEcho, &fixture)
                .narrowing(narrowing_to(link))
                .build()
                .expect_err("Symlink nach außen");
            assert!(matches!(error, RuntimeError::Sandbox { .. }), "{error}");
        }
    }
}
