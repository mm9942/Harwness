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
//! 3. [`root_sandbox_with_network`] — Rechte ausschließlich aus
//!    [`EntryKind::profile`], Netz-Hosts nur aus der Egress-Allowlist
//!    ([`root_network_scope`]), gegebenenfalls geschnitten mit
//!    [`RuntimeNarrowing::permissions`];
//!    gebunden an den erkannten Projekt-Root oder an den engeren
//!    [`RuntimeNarrowing::workspace_root`].
//! 4. [`root_ceiling`] — eine Wurzeldecke je [`CeilingPolicy`].
//! 5. [`new_root_trace`] + **ein** [`SpawnContext`], geklont für Sitzung und
//!    Spawner: Wurzel-Turn und Kinder hängen an derselben `trace_id`.
//! 6. [`ApprovalChain::for_root`] — Config-Politik ohne Nebenschalter, plus
//!    die [`crate::spec::AskResolution`] des Einstiegs.
//! 7. [`assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits`](harw_registry_defaults::profile::assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits) → [`ApprovalChain::install_over_default`];
//!    der Projektkontext der Registry folgt [`EntryProfile::project_context`]
//!    (ohne ihn: keine Doku, Platzhalter statt Host-Pfaden) und bei
//!    [`RuntimeNarrowing::workspace_root`] nur Doku unterhalb des gebundenen Roots.
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

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harw_agent_dsl::ExecutableAgentIr;
use harw_agent_dsl::roles::AgentRoleId;
use harw_authority::{
    NetworkScope, PermissionRequest, PermissionSet, SandboxSpec, WorkspaceRegistration,
    WorkspaceRegistry,
};
use harw_config::{PermissionsSection, PlanSection, ResolvedConfig};
use harw_context::ContextCeiling;
use harw_core::{
    AgentSession, ChildRegistryFactory, ContextBudget, DriftObserver, GuardPolicy, InteractionMode,
    ManagedAgentSpawner, ModelProvider, OrchestrationObserver, PitfallAdvisor, RoleEffortWeights,
    SessionActivation, SessionManager, SpawnContext, StateStore, ToolProfile,
};
use harw_extension_api::allow_rules::{AllowRuleSet, ApprovalRule, RuleDecision, RuleScope};
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use harw_extension_api::contributors::{ApprovalHandlerKind, ContextProvider, ExtFuture};
use harw_extension_api::types::{ContextFragment, TurnInputContext};
use harw_extension_api::{
    ApprovalHandler, ExtensionRegistry, ExtensionRegistryBuilder, ToolName,
    capabilities::AgentSpawner,
};
use harw_home::paths::{active_profile_name, profile_dir};
use harw_home::project::{ProjectHome, ProjectRoot, discover_project as discover_home_project};
use harw_memory::Memory;
use harw_operations::adapter::ModelToolProvider;
use harw_operations::operation::{Operation, Surface};
use harw_operations::registry::OperationRegistry;
use harw_operations::{OpContext, SharedSessionController};
use harw_plan::{
    FileGoalStore, FilePlanStore, GoalStore, InMemoryGoalStore, InMemoryPlanStore, PlanNodeKind,
    PlanStore, PlanToolConfig,
};
use harw_plan_bridge::FindingStore;
use harw_project_discovery::{DiscoveryConfig, ProjectContext, discover_project};
use harw_protocol::{
    AgentOrchestrationEvent,
    events::{SessionEvent, TurnEvent},
};
use harw_provider_http::{ProviderLoadRegistry, SecretResolver};
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    HostPermitWiring, IdentityOverrides, RegistryProfile, role_names,
};
use harw_sandbox::{ExtraRootsCell, HostPermitSessionRegistry, ProcessPermitLedger};
use harw_session_store::{ApprovalStore, JobStore};
use harw_tool_shell::host_permit_prompt::{
    HostPermitHandles, HostPermitPromptReceiver, HostPermitPromptSender, HostPermitVariant,
    host_permit_prompt_channel,
};
use harw_types::{
    AgentRole, ModelId, Principal, ProviderId, SessionId, TenantId, TurnId, WorkspaceId,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::approval::ApprovalChain;
use crate::budget::child_limits;
use crate::ceiling::root_ceiling;
use crate::children::RuntimeChildRegistryFactory;
use crate::config::{ConfigTrustReport, load_config};
use crate::contributors::{AssemblyContributor, AssemblyInputs, AssemblyParts};
use crate::error::{RuntimeError, RuntimeResult};
use crate::model::ModelSource;
use crate::sandbox::{root_network_scope, root_sandbox_with_network};
use crate::services::{PlanServices, RuntimeServices, RuntimeServicesParts, ServiceSurface};
use crate::spec::{
    AskResolution, ChildBackendHandle, EntryKind, EntryProfile, OperationSurface, RootBudget,
    RuntimeSpec, SpawnerPolicy,
};
use crate::trace::new_root_trace;

/// Werkzeugaufrufe, die ein Turn höchstens je Modell-Runde auslösen darf.
///
/// # Beschreibung
/// Aus [`RootBudget::max_model_rounds`] allein folgt keine Obergrenze für
/// Werkzeugaufrufe: eine Runde darf mehrere Aufrufe parallel enthalten
/// (`harw-core/src/turn_loop.rs`, Parallelpfad). Der Faktor ist bewusst
/// konservativ und dokumentiert; durchgesetzt wird er im Turn-Loop über
/// [`TurnLimits::to_core`].
const TOOL_CALLS_PER_ROUND: u32 = 8;

/// Obergrenze eines einzelnen gerenderten Werkzeugergebnisses in Bytes.
///
/// Übergangswert bis W4a (`ModelRequest.tool_result_max_bytes`, gefrorene
/// W3-Signatur): groß genug für eine gelesene Quelldatei, klein genug, dass
/// ein einzelnes Ergebnis das Kontextfenster nicht allein füllt.
const TOOL_RESULT_MAX_BYTES: usize = 64 * 1024;

/// Vorgabe-Timeout einer offenen Freigabeanfrage in Sekunden, wenn weder
/// Projekt- noch globale Konfiguration `[permissions] approval_timeout_secs`
/// setzen (Plan Schritt 3: 1800 s statt der bisherigen 300 s).
const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 1800;

/// Rückfall-Kontextfenster (Token), wenn weder das konfigurierte Modell
/// (`config.models[id].context_window`) noch der eingebaute Modellkatalog
/// eine Größe nennt (Welle 3). 32k statt früher 200k: ein unbekanntes
/// (z. B. lokales) Modell hat oft ein kleines Fenster, und ein zu großer
/// Rückfallwert ließe [`harw_core::AutoCompactPolicy::for_context_window`]
/// erst verdichten, nachdem der Provider die Anfrage bereits abgelehnt hat.
/// Jeder Rückfall wird mit `tracing::warn!` gemeldet ([`model_limits_for`]).
pub const UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS: u64 = 32_768;

/// Kontextfenster (Token) des eingebauten Offline-Echos
/// ([`ModelSource::Echo`]), wenn weder Konfiguration noch Katalog ein Fenster
/// für das (optionale) Wurzelmodell nennen.
///
/// Der Echo ist kein echtes Modell: er hat kein Fenster, das eine Anfrage
/// überschreiten könnte, und ruft keinen Provider. Das konservative
/// [`UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS`] schützt ein *unbekanntes echtes*
/// Modell vor einer Ablehnung durch den Provider; beim Echo würde es nur den
/// Offline-Pfad (`harw run`, SDK `offline_echo`, Tests) an die zufällige Größe
/// von System-Prompt und Werkzeugschemata koppeln. 200k entspricht dem
/// verbreiteten Fenster großer Cloud-Modelle (und dem früheren Rückfallwert);
/// die Verdichtungs-Obergrenze (`[compaction] absolute_ceiling_tokens`) gilt
/// unverändert. Ein konfiguriertes/katalogisiertes Fenster hat weiterhin
/// Vorrang (Tests, die mit Echo bewusst ein kleines Fenster setzen, bleiben
/// gültig). [`ModelSource::Override`] bekommt diesen Wert **nicht**: dahinter
/// steht in Produktion ein echter Provider (Jobs, Telegram).
pub const OFFLINE_ECHO_CONTEXT_WINDOW_TOKENS: u64 = 200_000;

/// Fester Token-Aufschlag für System-Prompt, Werkzeugschemata und
/// Kontextfragmente, den die Auto-Verdichtung zusätzlich zur Ausgabereserve
/// vom Fenster abzieht ([`harw_core::AutoCompactPolicy::with_fixed_overhead`]).
const FIXED_CONTEXT_OVERHEAD_TOKENS: u64 = 4096;

/// Grenzwerte eines einzelnen Turns, abgeleitet aus dem [`RootBudget`].
///
/// # Beschreibung
/// `harw-core` kennt heute keinen solchen Typ (`grep TurnLimits` über den
/// Workspace ist leer), deshalb steht er hier. Er ist **reine Ableitung**:
/// jedes Feld folgt aus dem Budget des Laufs oder aus einer dokumentierten
/// Konstante dieses Moduls. Durchgesetzt werden die Werte im Turn-Loop von
/// `harw-core` ([`Self::to_core`], als Vorgabe-Grenzen der Wurzelsitzung).
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
    /// Die durchsetzbare Form für den Turn-Loop von `harw-core`.
    #[must_use]
    pub const fn to_core(&self) -> harw_core::turn_loop::TurnLimits {
        harw_core::turn_loop::TurnLimits {
            max_model_rounds: self.max_model_rounds,
            max_tool_calls: self.max_tool_calls,
            max_output_tokens_total: self.max_output_tokens_total,
            wall_time: self.wall_time,
            tool_result_max_bytes: self.tool_result_max_bytes,
        }
    }

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
/// Besonders ist [`EntryKind::GatewayTelegram`] (Runde 3, Welle D): der Chat
/// liest und schreibt im gebundenen Workspace, und die Person beantwortet jede
/// Rückfrage über Freigabe-Buttons. Lesende Werkzeuge laufen durch, jedes
/// Schreiben (`fs.write`/`fs.edit`) fragt. Der Modus ist dort
/// [`ApprovalMode::Delegated`] — und zwar nicht nur als Vorgabe, sondern
/// **erzwungen**: [`forced_approval_mode`] übersteuert Konfiguration und
/// Aufrufer-Override, damit kein `full` das Schreiben freischaltet.
///
/// # Argumente
/// - `entry` ([`EntryKind`]): der Einstieg.
///
/// # Rückgabe
/// Immer [`ApprovalMode::Delegated`]; für `GatewayTelegram` zusätzlich
/// erzwungen (siehe [`forced_approval_mode`]).
#[must_use]
pub const fn default_approval_mode(entry: EntryKind) -> ApprovalMode {
    match entry {
        EntryKind::GatewayTelegram
        | EntryKind::Tui
        | EntryKind::OneShot
        | EntryKind::LocalEcho
        | EntryKind::Analyze
        | EntryKind::Doctor
        | EntryKind::Web
        | EntryKind::McpServe
        | EntryKind::JobPrompt
        | EntryKind::JobPlanNode
        | EntryKind::GatewayDream
        | EntryKind::CompiledAgent => ApprovalMode::Delegated,
    }
}

/// Der Freigabemodus, den ein Einstieg unabhängig von Konfiguration und
/// Aufrufer-Override führen muss.
///
/// # Beschreibung
/// [`EntryKind::GatewayTelegram`] schreibt im Workspace eines entfernten
/// Chats; `fs.write`/`fs.edit` (und jeder andere nicht lesende Aufruf)
/// stehen dort immer unter Freigabe, auch wenn `[permissions].default_mode`
/// oder ein `--approval` etwas anderes sagt (Runde 3, Welle D). Lesende
/// Werkzeuge laufen ohne Button durch. Alle anderen Einstiege haben keinen
/// erzwungenen Modus.
///
/// # Argumente
/// - `entry` ([`EntryKind`]): der Einstieg.
///
/// # Rückgabe
/// `Some(ApprovalMode::Delegated)` für `GatewayTelegram`, sonst `None`.
#[must_use]
pub const fn forced_approval_mode(entry: EntryKind) -> Option<ApprovalMode> {
    match entry {
        EntryKind::GatewayTelegram => Some(ApprovalMode::Delegated),
        EntryKind::Tui
        | EntryKind::OneShot
        | EntryKind::LocalEcho
        | EntryKind::Analyze
        | EntryKind::Doctor
        | EntryKind::Web
        | EntryKind::McpServe
        | EntryKind::JobPrompt
        | EntryKind::JobPlanNode
        | EntryKind::GatewayDream
        | EntryKind::CompiledAgent => None,
    }
}

/// Das Home-Verzeichnis des Betriebssystem-Nutzers, roh aus `$HOME` gelesen.
///
/// # Beschreibung
/// Wird ausschließlich als Ausschlussregel für `/add-workdir`-Kandidaten
/// gebraucht ([`harw_sandbox::validate_extra_root`]) — dieselbe Quelle wie
/// `harw_home::project::classify_project_home_root` (privat dort, deshalb hier
/// dupliziert statt importiert). Ein leerer oder fehlender Wert liefert
/// `None`; der Aufrufer überspringt die Home-Prüfung dann statt sie
/// abzulehnen.
fn os_user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Baut ein [`SandboxProfile`] aus der vertrauenswürdigen
/// `[sandbox]`-Konfiguration.
///
/// # Beschreibung
/// Standard ist [`SandboxProfile::Strict`]: hermetische Bubblewrap-Sandbox.
/// Ein konfiguriertes Cargo-Modul wird aktiviert, wenn die Validierung
/// durchgeht; ein Fehler wird geloggt und das Modul fällt auf Strict zurück
/// (fail-safe, nicht fail-closed — die Sandbox bleibt hermetisch).
/// Ein konfiguriertes tmux-Modul verhält sich analog.
///
/// Diese Funktion ist der einzige Ort, an dem ein Sandbox-Profil aus
/// Konfiguration entsteht. Das Profil ist ein vertrauenswürdiger
/// Runtime-Input und wird an die Registry weitergegeben, nie an ein Tool.
fn sandbox_profile_from_config(
    section: &harw_config::SandboxSection,
) -> harw_sandbox::SandboxProfile {
    use harw_config::{CargoSandboxModeToml, TmuxOperationModeToml};
    use harw_sandbox::{CargoExecutionMode, SandboxProfile};

    // Cargo-Modul
    if let Some(cargo) = &section.cargo {
        let mode = match cargo.mode {
            CargoSandboxModeToml::Inspect => CargoExecutionMode::Inspect,
            CargoSandboxModeToml::BuildOffline => CargoExecutionMode::BuildOffline,
            CargoSandboxModeToml::Fetch => CargoExecutionMode::Fetch,
        };
        match harw_sandbox::CargoSandboxProfile::new(
            mode,
            &cargo.cargo_bin,
            &cargo.rustup_home,
            &cargo.cargo_home,
        ) {
            Ok(profile) => return SandboxProfile::Cargo(profile),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "runtime.sandbox.cargo_profile_invalid"
                );
            }
        }
    }

    // tmux-Modul
    if let Some(tmux) = &section.tmux {
        let mode = match tmux.mode {
            TmuxOperationModeToml::Inspect => harw_sandbox::TmuxOperationMode::Inspect,
            TmuxOperationModeToml::Write => harw_sandbox::TmuxOperationMode::Write,
        };
        match harw_sandbox::TmuxSandboxProfile::new(mode, &tmux.socket_path) {
            Ok(profile) => return SandboxProfile::Tmux(profile),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "runtime.sandbox.tmux_profile_invalid"
                );
            }
        }
    }

    SandboxProfile::Strict
}

/// Der effektive Vorgabe-Freigabemodus eines Laufs (Contract §2).
///
/// # Beschreibung
/// Präzedenz Projekt > Global > [`default_approval_mode`]: die Projekt-Ebene
/// gewinnt gegen die globale, diese gegen die eingebaute Vorgabe des
/// Einstiegs. Ein Sitzungs-Override liegt außerhalb dieser Funktion — er
/// lebt ausschließlich in der zurückgegebenen [`ApprovalModeCell`] und
/// überschreibt beide Ebenen zur Laufzeit (`/permissions set` bzw.
/// Shift+Tab), ohne dass die Montage erneut liefe.
///
/// Ein nicht auflösbarer Modus-String (weder `ask`, `auto` noch `full`) wird
/// wie „nicht gesetzt“ behandelt und übersprungen — die Montage lehnt eine
/// ungültige Konfiguration hier nicht ab, das übernimmt
/// [`PermissionsSection::validate`] vor dem Speichern.
///
/// # Arguments
/// - `entry` ([`EntryKind`]): der Einstieg, dessen eingebaute Vorgabe die
///   unterste Stufe bildet.
/// - `global` (`&PermissionsSection`): die bereits vollständig gemergte
///   `[permissions]`-Sektion aus `config.harness.permissions`; sie enthält
///   gegebenenfalls die Projekt-Einstellungen als höchste Präzedenz.
/// - `project` (`&PermissionsSection`): zusätzlicher, nur für explizite
///   Einbettungen gelieferter Projekt-Override. Die normale Montage übergibt
///   ihn leer, weil `load_config` diesen Layer bereits eingemergt hat.
///
/// # Returns
/// Den effektiven [`ApprovalMode`]; für einen Einstieg mit
/// [`forced_approval_mode`] immer dessen Modus, unabhängig von der
/// Konfiguration.
fn effective_approval_mode(
    entry: EntryKind,
    global: &PermissionsSection,
    project: &PermissionsSection,
) -> ApprovalMode {
    if let Some(forced) = forced_approval_mode(entry) {
        return forced;
    }
    project
        .default_mode
        .as_deref()
        .and_then(ApprovalMode::parse)
        .or_else(|| global.default_mode.as_deref().and_then(ApprovalMode::parse))
        .unwrap_or_else(|| default_approval_mode(entry))
}

/// Das effektive Freigabe-Timeout eines Laufs (Plan Schritt 3), dieselbe
/// Präzedenz wie [`effective_approval_mode`]: Projekt > Global >
/// [`DEFAULT_APPROVAL_TIMEOUT_SECS`].
///
/// # Returns
/// Eine [`Duration`] in Sekunden; die TUI liest sie über
/// [`RuntimeAssembly::approval_timeout`] für den Countdown im Freigabe-Panel.
fn effective_approval_timeout(
    global: &PermissionsSection,
    project: &PermissionsSection,
) -> Duration {
    let secs = project
        .approval_timeout_secs
        .or(global.approval_timeout_secs)
        .unwrap_or(DEFAULT_APPROVAL_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Übersetzt eine geladene `[[permissions.allow]]`/`[[permissions.deny]]`-Liste
/// in [`ApprovalRule`]n eines festen [`RuleScope`].
fn rules_from_section(section: &PermissionsSection, scope: RuleScope, out: &mut Vec<ApprovalRule>) {
    for rule in &section.allow {
        out.push(ApprovalRule {
            tool: rule.tool.clone(),
            pattern: rule.pattern.clone(),
            decision: RuleDecision::Allow,
            scope,
        });
    }
    for rule in &section.deny {
        out.push(ApprovalRule {
            tool: rule.tool.clone(),
            pattern: rule.pattern.clone(),
            decision: RuleDecision::Deny,
            scope,
        });
    }
}

/// Sät die geteilte [`AllowRuleSet`] eines Laufs aus globaler und
/// Projekt-Konfiguration (Contract §2/§4).
///
/// # Beschreibung
/// Global-Regeln zuerst, dann Projekt-Regeln — die Reihenfolge ist reine
/// Diagnose (`AllowRuleSet::evaluate` gewichtet keinen Scope höher als
/// einen anderen). `deny`-Einträge werden **immer** übernommen, ohne
/// Sonderbehandlung: es gibt keinen Schalter, der eine Deny-Regel
/// unterdrücken könnte — genau das macht „Deny gewinnt über alle Scopes
/// hinweg“ aus.
///
/// # Returns
/// Eine neue [`AllowRuleSet`] mit allen geladenen Regeln.
fn seed_allow_rule_set(global: &PermissionsSection, project: &PermissionsSection) -> AllowRuleSet {
    let mut rules = Vec::with_capacity(
        global.allow.len() + global.deny.len() + project.allow.len() + project.deny.len(),
    );
    rules_from_section(global, RuleScope::Global, &mut rules);
    rules_from_section(project, RuleScope::Project, &mut rules);
    AllowRuleSet::from_rules(rules)
}

/// Sät die geteilte [`ExtraRootsCell`] eines Laufs aus globaler und
/// Projekt-Konfiguration (Contract §5, Slice A8, Plan Schritt 6).
///
/// # Beschreibung
/// Jeder Eintrag wird über [`harw_sandbox::validate_extra_root`] gegen die primäre
/// Workspace-Wurzel geprüft. Ein ungültiger Eintrag (nicht existent, `/`,
/// `$HOME`, Vorfahre oder bereits Teil der primären Wurzel) wird **nicht**
/// abgelehnt, sondern mit `warn!` übersprungen — eine kaputte oder veraltete
/// Konfigurationszeile darf die Montage nicht zu Fall bringen. Alle geladenen
/// Einträge gelten als bereits dauerhaft gemerkt (`persisted = true`), weil
/// sie aus einer gespeicherten Konfigurationsdatei stammen.
///
/// # Arguments
/// - `global` / `project` (`&PermissionsSection`): siehe
///   [`seed_allow_rule_set`].
/// - `primary_root` (`&Path`): die bereits kanonische primäre Workspace-Wurzel
///   dieses Laufs (siehe [`sandbox_root`]).
/// - `user_home` (`Option<&Path>`): siehe [`harw_sandbox::validate_extra_root`].
///
/// # Returns
/// Eine neue [`ExtraRootsCell`] mit allen gültigen Einträgen.
fn seed_extra_roots(
    global: &PermissionsSection,
    project: &PermissionsSection,
    primary_root: &Path,
    user_home: Option<&Path>,
) -> ExtraRootsCell {
    let cell = ExtraRootsCell::new();
    for candidate in global.extra_roots.iter().chain(project.extra_roots.iter()) {
        if let Err(error) = cell.add(candidate, true, primary_root, user_home) {
            tracing::warn!(
                path = %candidate.display(),
                error = %error,
                "runtime.extra_roots.seed_skipped"
            );
        }
    }
    cell
}

/// Übersetzt eine deklarative `[tools.plan]`-Sektion in eine [`PlanToolConfig`].
///
/// # Beschreibung
/// Spiegelt `harw_cli::main::plan_tool_config_from_section` (dort
/// `pub(crate)`, deshalb hier dupliziert statt importiert — `harw-runtime`
/// darf keine Abhängigkeit auf `harw-cli` eingehen): [`PlanSection::validate`]
/// läuft zuerst, damit Tippfehler in `max_nodes`, `max_expand_depth` und
/// `require_exploration_for` vor jedem Store-Bau auffallen; danach wird jede
/// Zeichenkette aus `require_exploration_for` in ein [`PlanNodeKind`] geparst.
/// Ein Parse-Fehler wäre hier ein Widerspruch zu [`PlanSection::validate`]
/// (das dieselbe Liste bereits gegen bekannte Namen prüft) und wird darum
/// ebenfalls als `Err` gemeldet, statt verschluckt zu werden.
///
/// # Arguments
/// - `section` (`&PlanSection`): die geladene `[tools.plan]`-Sektion, geliehen.
///
/// # Returns
/// Die [`PlanToolConfig`], die Stores **und** Operationen gemeinsam regiert.
///
/// # Errors
/// Ein `String` mit der Begründung aus [`PlanSection::validate`] oder aus dem
/// gescheiterten `PlanNodeKind`-Parse.
fn plan_tool_config_from_section(section: &PlanSection) -> Result<PlanToolConfig, String> {
    section.validate()?;
    let require_exploration_for = section
        .require_exploration_for
        .iter()
        .map(|name| {
            name.parse::<PlanNodeKind>()
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<PlanNodeKind>, String>>()?;

    Ok(PlanToolConfig {
        enabled: section.enabled,
        persist: section.persist,
        require_for_complex_work: section.require_for_complex_work,
        validate_dependency_cycles: section.validate_dependency_cycles,
        validate_write_conflicts: section.validate_write_conflicts,
        max_nodes: section.max_nodes,
        require_exploration_for,
        exploration_ttl_secs: section.exploration_ttl_secs,
        max_expand_depth: section.max_expand_depth,
    })
}

/// Öffnet die Planungsfläche des Wurzel-Laufs — außer der Aufrufer hat bereits
/// eine über [`RuntimeAssemblyBuilder::plan_services`] mitgebracht.
///
/// # Beschreibung
/// Schließt G-024/G-098: `harw-tui/src/runtime_root.rs` ruft
/// `RuntimeAssemblyBuilder::plan_services` nie auf (es liest nur
/// [`RuntimeAssembly::plan_services`] nach dem Bau) — die TUI bekam die
/// Planungsfläche bislang **nie**, unabhängig von `[tools.plan]`. Präzedenz:
///
/// 1. **Builder-Wert** (`explicit`): hat immer Vorrang. Ein Aufrufer, der
///    eigene Speicher mitbringt (z. B. `harw-cli/src/chat.rs` für `OneShot`),
///    wird nie überschrieben — die Gate-Semantik dieser Einstiege ändert sich
///    durch diese Funktion nicht.
/// 2. **Nicht-`Tui`-Einstiege ohne Builder-Wert**: bleiben ohne eingebaute
///    Vorgabe geschlossen.
/// 3. **`enabled = false`**: bleibt geschlossen — eine
///    bewusste Abschaltung wird nie überschrieben.
/// 4. **`enabled = true`** (auch der deklarative Default): [`plan_tool_config_from_section`]
///    übersetzt die volle Konfiguration (Knotenlimits,
///    `require_exploration_for` etc.). Ein Fehler beendet die Montage, statt
///    einen unbemerkten In-Memory-Ersatz zu verwenden. Bei `persist = true`
///    werden Plan und Goal als [`FilePlanStore`] bzw. [`FileGoalStore`] unter
///    dem bereits aufgelösten Projekt-Home geöffnet.
///
/// # Arguments
/// - `entry` ([`EntryKind`]): der Einstieg des Laufs.
/// - `explicit` (`Option<PlanServices>`): der Builder-Wert.
/// - `section` (`&PlanSection`): `config.harness.tools.plan` des Laufs.
/// - `project_home` (`&ProjectHome`): Wurzel des [`FindingStore`] der Vorgabe.
///
/// # Returns
/// Die zu benutzende Planungsfläche, oder `None`.
fn resolve_plan_services(
    entry: EntryKind,
    explicit: Option<PlanServices>,
    section: &PlanSection,
    project_home: &ProjectHome,
) -> RuntimeResult<Option<PlanServices>> {
    if explicit.is_some() {
        return Ok(explicit);
    }
    if !matches!(entry, EntryKind::Tui) {
        return Ok(None);
    }
    if !section.enabled {
        return Ok(None);
    }
    let plan_config =
        plan_tool_config_from_section(section).map_err(|detail| RuntimeError::Config { detail })?;
    let (plan, goal): (Arc<dyn PlanStore>, Arc<dyn GoalStore>) = if plan_config.persist {
        let plan = FilePlanStore::with_config(
            project_home.plans_dir().join("default"),
            plan_config.clone(),
        )
        .map_err(|error| RuntimeError::Config {
            detail: format!("cannot open persistent plan store: {error}"),
        })?;
        let goal =
            FileGoalStore::new(project_home.goals_dir().join("default")).map_err(|error| {
                RuntimeError::Config {
                    detail: format!("cannot open persistent goal store: {error}"),
                }
            })?;
        (Arc::new(plan), Arc::new(goal))
    } else {
        (
            Arc::new(InMemoryPlanStore::new()),
            Arc::new(InMemoryGoalStore::new()),
        )
    };
    Ok(Some(PlanServices {
        plan,
        goal,
        findings: Arc::new(FindingStore::new(project_home.plans_dir())),
        plan_config,
    }))
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
    // Plan R9, Teil A: wortgleich zu `with_executable_agent_ir`.
    for name in harw_core::activation::ALWAYS_AVAILABLE_TOOLS {
        activation.enable_tool(ToolName::new(*name));
    }
    for name in ir.tool_surface().forbidden() {
        activation.disable_tool(ToolName::new(name.clone()));
    }
    activation
}

/// Das Kontextprogramm der Wurzel-IR, geprüft gegen die Wurzeldecke
/// (#22 Welle 1B).
///
/// # Beschreibung
/// Dieselbe Regel wie bei der Kind-Admission
/// (`ManagedAgentSpawner::describe_context_program_ceiling_violation`): jede
/// Sektion aus `must_include` und `section_detail` muss in der Decke liegen.
/// Ein leeres Programm (`ContextProgram::default()`, keine Deklaration)
/// ergibt `None` — die Wurzel rendert dann genau wie bisher. Unter einer
/// geschlossenen Decke (keine Sektion, Einstiege ohne Spawner) gilt kein
/// Programm; die Wurzel läuft ohne und das wird geloggt.
///
/// # Fehler
/// [`RuntimeError::Registry`], wenn das Programm eine Sektion außerhalb der
/// Wurzeldecke verlangt: fail-closed, die Wurzel startet nicht mit einem
/// Programm, das sie nicht einhalten kann.
fn root_context_program(
    ir: Option<&ExecutableAgentIr>,
    ceiling: &ContextCeiling,
) -> RuntimeResult<Option<harw_agent_dsl::executable::ContextProgram>> {
    let Some(ir) = ir else {
        return Ok(None);
    };
    let program = ir.context_program();
    if program == &harw_agent_dsl::executable::ContextProgram::default() {
        return Ok(None);
    }
    if ceiling.sections.is_empty() {
        // Geschlossene Decke (`CeilingPolicy::Closed`: Web, MCP, Jobs,
        // Gateways — Einstiege ohne Spawner): dort kann kein Programm
        // gelten, und auch kein Kind mit Programm würde zugelassen. Die
        // Wurzel läuft ohne Programm; das wird sichtbar gemeldet.
        tracing::warn!(
            agent = ir.specialization(),
            policy = program.context_policy().unwrap_or("<none>"),
            "runtime.root_context_program.skipped_under_closed_ceiling"
        );
        return Ok(None);
    }
    let outside = program
        .must_include()
        .iter()
        .map(String::as_str)
        .chain(
            program
                .section_detail()
                .iter()
                .map(harw_agent_dsl::executable::SectionDetail::name),
        )
        .find(|name| {
            !harw_context::SectionName::try_new(*name)
                .is_ok_and(|section| ceiling.sections.contains(&section))
        });
    if let Some(name) = outside {
        return Err(RuntimeError::Registry {
            detail: format!(
                "root agent '{}' declares context section '{name}' outside the root context \
                 ceiling; refusing to start with a context program it cannot honour",
                ir.specialization()
            ),
        });
    }
    Ok(Some(program.clone()))
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
/// eigentliche Bindung entsteht danach über [`crate::sandbox::root_sandbox`] mit dem
/// Mandanten des Einstiegs.
const NARROWED_ROOT_TENANT: &str = "narrowing";
/// Workspace-Alias der Enthaltenseins-Prüfung (siehe [`NARROWED_ROOT_TENANT`]).
const NARROWED_ROOT_WORKSPACE: &str = "workspace-root";

/// Neutraler Platzhalter für `project_root`/`cwd` im Modellkontext von
/// Einstiegen ohne [`EntryProfile::project_context`] (Befund Z2d2-R1).
const PROJECT_CONTEXT_PLACEHOLDER: &str = "<workspace>";

/// Der Projektkontext, den die Wurzel-Registry dem Modell zeigt.
///
/// # Beschreibung
/// Die Registry ([`assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits`](harw_registry_defaults::profile::assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits)) reicht den Kontext an
/// zwei Stellen in den Modellkontext: `ProjectContextProvider` (Fragmente
/// `project.root` mit `project_root=`/`cwd=` und je Doku-Datei
/// `project.doc:<name>`) und die Baseline-Identität (Systemprompt mit
/// `cwd`/`project_root`). Beide lesen ausschließlich die öffentlichen Felder
/// von [`ProjectContext`]; deshalb genügt hier eine Kopie:
///
/// - `profile.project_context == false` (z. B. `JobPrompt`, `McpServe`,
///   Gateways, `Web`): keine Doku, `project_root`/`cwd` =
///   [`PROJECT_CONTEXT_PLACEHOLDER`]. Ein entfernter Einreicher kann so weder
///   `HARW.md`/`AGENTS.md`/`CLAUDE.md` noch Host-Pfade über das Ergebnis
///   abziehen (Befund Z2d2-R1).
/// - `narrowed_root = Some(root)` (gesetzter
///   [`RuntimeNarrowing::workspace_root`], bereits kanonisch): nur Doku, deren
///   Pfad unterhalb von `root` liegt ([`Path::starts_with`] vergleicht
///   Komponenten, kein String-Präfix); `project_root` = `root`, `cwd` bleibt,
///   falls es unter `root` liegt, sonst `root` (Befund Z2d2-R2).
/// - sonst: der erkannte Kontext unverändert, ohne Kopie.
///
/// [`RuntimeAssembly::project`], Spawner und Contributors sehen weiterhin den
/// erkannten Kontext; nur der Modellkontext der Wurzel-Registry wird verengt.
fn registry_project_context<'a>(
    project: &'a ProjectContext,
    profile: &EntryProfile,
    narrowed_root: Option<&Path>,
) -> Cow<'a, ProjectContext> {
    if !profile.project_context {
        return Cow::Owned(ProjectContext {
            cwd: PathBuf::from(PROJECT_CONTEXT_PLACEHOLDER),
            project_root: PathBuf::from(PROJECT_CONTEXT_PLACEHOLDER),
            docs: Vec::new(),
        });
    }
    let Some(root) = narrowed_root else {
        return Cow::Borrowed(project);
    };
    let cwd = if project.cwd.starts_with(root) {
        project.cwd.clone()
    } else {
        root.to_path_buf()
    };
    Cow::Owned(ProjectContext {
        cwd,
        project_root: root.to_path_buf(),
        docs: project
            .docs
            .iter()
            .filter(|doc| doc.path.starts_with(root))
            .cloned()
            .collect(),
    })
}

/// Lehnt eine Werkzeugverengung an Einstiegen mit Modell-Tool-Fläche ab.
///
/// # Beschreibung
/// Die Operations-Modell-Tools ([`OperationSurface::AllWithModelTools`])
/// entstehen unabhängig vom Registry-Profil (Schritt 12). Eine Verengung auf
/// einen kleineren Werkzeugsatz ließe sie dort sichtbar und wäre damit nur
/// scheinbar wirksam (Befund Z2d2-R8). Deshalb fail-closed: bei gesetzter
/// Verengung mit `registry_profile != Full` und dieser Fläche bricht der Bau
/// ab. Ohne Verengung oder mit `Full` ändert sich nichts.
///
/// # Fehler
/// [`RuntimeError::Registry`] für die abgelehnte Kombination.
fn ensure_narrowing_fits_operations(
    entry: EntryKind,
    profile: &EntryProfile,
    narrowing: Option<&RuntimeNarrowing>,
) -> RuntimeResult<()> {
    match narrowing {
        Some(narrowing)
            if narrowing.registry_profile != RegistryProfile::Full
                && profile.operations == OperationSurface::AllWithModelTools =>
        {
            Err(RuntimeError::Registry {
                detail: format!(
                    "refusing to narrow entry {entry:?} to registry profile {:?}: its operation \
                     surface {:?} exposes operation model tools the narrowing cannot remove",
                    narrowing.registry_profile, profile.operations
                ),
            })
        }
        _ => Ok(()),
    }
}

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
/// [`crate::sandbox::root_sandbox`] kanonisiert den übergebenen, bereits kanonischen Pfad
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
/// `Full | ShellExecution | ReadOnlyExplore | NoTools | MemoryStewardship |
/// WorkspaceEdit` (dazu die UIA-Spezialisierungen), Einstieg `WorkspaceEdit`
/// nur `WorkspaceEdit | NoTools`, Einstieg `NoTools` nur `NoTools`, Einstieg `MemoryStewardship` nur
/// `MemoryStewardship` (identisch, kein Aufweiten), jede andere Kombination
/// wird abgelehnt. Das `match` ist bewusst ohne Auffang-Arm im ersten
/// Tupelelement: eine neue [`RegistryProfile`]-Variante bricht den Compiler,
/// statt still zugelassen zu werden.
///
/// `ReadOnlyExplore` ist **keine** Werkzeug-Teilmenge von `Full`: es bringt
/// `deps.*` mit, das `Full` nicht registriert. Die Whitelist bleibt trotzdem
/// wie vertraglich festgelegt; abgesichert wird über die Sandbox, weil kein
/// Einstiegsprofil [`harw_authority::Permission::ReadCargoRegistry`] trägt und
/// die geschnittene Sandbox dieses Recht deshalb nie erhalten kann. Dasselbe
/// gilt für `UiaQuickHelper`/`UiaWriter`, die seit der Nutzerentscheidung
/// „die UIA-Helfer recherchieren kurz online und fügen manchmal
/// Abhängigkeiten hinzu“ ebenfalls `deps.*` und `web.fetch`/`web.search`
/// registrieren: kein Einstiegsprofil trägt `ReadCargoRegistry`, und die
/// einzigen Einstiege mit [`harw_authority::Permission::NetworkAccess`]
/// (`Tui`, `OneShot`) führen Modell-Tool-Operationen und dürfen deshalb nur
/// auf `Full` verengen ([`ensure_narrowing_fits_operations`]); die Werkzeuge
/// fallen bei einer Verengung also über `tool_names_for(granted)` bzw. am
/// Rechte-Prolog heraus.
///
/// # Fehler
/// [`RuntimeError::Registry`] für jede nicht zugelassene Kombination.
fn narrowed_registry_profile(
    entry: EntryKind,
    entry_profile: RegistryProfile,
    requested: RegistryProfile,
) -> RuntimeResult<RegistryProfile> {
    use RegistryProfile::{
        AgentStewardship, Full, MatrixReader, MemoryStewardship, NoTools, Planning,
        ReadOnlyExplore, ReadOnlyResearch, Research, ShellExecution, UiaExplorer, UiaLatexWriter,
        UiaQuickHelper, UiaShellWorker, UiaWriter, WorkspaceEdit,
    };

    match (entry_profile, requested) {
        // Full darf auf ShellExecution, MemoryStewardship, UiaQuickHelper,
        // AgentStewardship, UiaExplorer, UiaWriter oder UiaShellWorker
        // verengt werden (alle sieben Werkzeugsätze ⊆ Full oder — wie
        // ReadOnlyExplore — über die Sandbox abgesichert, siehe unten);
        // ShellExecution, MemoryStewardship, UiaQuickHelper,
        // AgentStewardship, UiaExplorer, UiaWriter und UiaShellWorker
        // selbst dürfen nur identisch bleiben (kein Aufweiten auf Full,
        // kein Mischen mit ReadOnly-/Planungs-Profilen — und,
        // sicherheitsrelevant, kein Wechsel zwischen UiaExplorer und
        // UiaWriter: `UiaWriter` trägt zusätzlich `fs.write`, das ein
        // UiaExplorer-Einstieg nie besaß, ein Wechsel wäre also ein
        // Aufweiten trotz gleicher Organisationsrolle).
        // `AgentStewardship` wird dabei wie `MemoryStewardship`/
        // `UiaQuickHelper` behandelt (Addendum K): nur Full darf zu ihm
        // verengen, und als Einstieg narrowt er ausschließlich auf sich
        // selbst. `UiaExplorer`/`UiaWriter` (Addendum D+E, REG-DE) sind
        // Spezialisierungen derselben Organisationsrolle
        // (`AgentRoleId::UiaWorker`) wie `UiaQuickHelper` und folgen
        // deshalb demselben Muster: nur Full darf zu ihnen verengen, und
        // sie selbst narrowen ausschließlich auf sich selbst. `UiaShellWorker`
        // (uia-shell-worker.toml) ist derselbe Zwilling für `shell.exec` statt
        // Schreibzugriff — dieselbe Organisationsrolle, dieselbe Spawn-Matrix
        // (`AgentRoleId::UserInterface` kann `UiaWorker`-Rollen spawnen),
        // deshalb exakt dasselbe Muster: nur Full darf zu ihm verengen, er
        // narrowt ausschließlich auf sich selbst, und kein Wechsel zwischen
        // ihm und den anderen UIA-Geschwistern (er trägt `shell.exec`, das
        // die anderen nie besaßen). Erreichbarkeit bleibt damit identisch zu
        // UiaExplorer/UiaWriter: ein Einstieg, der diese nicht erreichen
        // konnte, kann auch UiaShellWorker nicht erreichen; die
        // Host-Sandbox-Bindung selbst erzwingt ausschließlich der
        // ProcessPermitLedger, nicht dieses Match.
        //
        // `WorkspaceEdit` (Runde 3, Welle D; Einstiegsprofil von
        // `GatewayTelegram`) folgt demselben Muster: Full darf zu ihm
        // verengen (seine `deps.*` sind wie bei ReadOnlyExplore über die
        // Sandbox abgesichert), und als Einstieg narrowt er nur auf sich
        // selbst oder auf `NoTools` — eine reine Reduktion.
        (
            Full,
            Full | ShellExecution | ReadOnlyExplore | NoTools | MemoryStewardship | UiaQuickHelper
            | AgentStewardship | UiaExplorer | UiaWriter | UiaShellWorker | WorkspaceEdit,
        )
        | (WorkspaceEdit, WorkspaceEdit | NoTools)
        | (ShellExecution, ShellExecution)
        | (UiaQuickHelper, UiaQuickHelper)
        | (NoTools, NoTools)
        | (MemoryStewardship, MemoryStewardship)
        | (AgentStewardship, AgentStewardship)
        | (UiaExplorer, UiaExplorer)
        | (UiaWriter, UiaWriter)
        | (UiaShellWorker, UiaShellWorker) => Ok(requested),
        (Full, Research | Planning)
        | (
            ShellExecution,
            Full | ReadOnlyExplore | Research | Planning | NoTools | MemoryStewardship
            | UiaQuickHelper | AgentStewardship | UiaExplorer | UiaWriter | UiaShellWorker
            | WorkspaceEdit,
        )
        | (
            UiaQuickHelper,
            Full | ShellExecution | ReadOnlyExplore | Research | Planning | NoTools
            | MemoryStewardship | AgentStewardship | UiaExplorer | UiaWriter | UiaShellWorker
            | WorkspaceEdit,
        )
        | (
            NoTools,
            Full | ShellExecution | ReadOnlyExplore | Research | Planning | MemoryStewardship
            | UiaQuickHelper | AgentStewardship | UiaExplorer | UiaWriter | UiaShellWorker
            | WorkspaceEdit,
        )
        | (
            MemoryStewardship,
            Full | ShellExecution | ReadOnlyExplore | Research | Planning | NoTools
            | UiaQuickHelper | AgentStewardship | UiaExplorer | UiaWriter | UiaShellWorker
            | WorkspaceEdit,
        )
        | (
            AgentStewardship,
            Full | ShellExecution | ReadOnlyExplore | Research | Planning | NoTools
            | MemoryStewardship | UiaQuickHelper | UiaExplorer | UiaWriter | UiaShellWorker
            | WorkspaceEdit,
        )
        | (
            UiaExplorer,
            Full | ShellExecution | ReadOnlyExplore | Research | Planning | NoTools
            | MemoryStewardship | UiaQuickHelper | AgentStewardship | UiaWriter | UiaShellWorker
            | WorkspaceEdit,
        )
        | (
            UiaWriter,
            Full | ShellExecution | ReadOnlyExplore | Research | Planning | NoTools
            | MemoryStewardship | UiaQuickHelper | AgentStewardship | UiaExplorer | UiaShellWorker
            | WorkspaceEdit,
        )
        | (
            UiaShellWorker,
            Full | ShellExecution | ReadOnlyExplore | Research | Planning | NoTools
            | MemoryStewardship | UiaQuickHelper | AgentStewardship | UiaExplorer | UiaWriter
            | WorkspaceEdit,
        )
        | (
            WorkspaceEdit,
            Full | ShellExecution | ReadOnlyExplore | Research | Planning | MemoryStewardship
            | UiaQuickHelper | AgentStewardship | UiaExplorer | UiaWriter | UiaShellWorker,
        )
        // `ReadOnlyResearch` (researcher / dependency-researcher) ist wie
        // `Research` ein reines Kind-Profil mit Netz: kein Einstieg darf
        // auf es verengen, und es selbst verengt nie. `MatrixReader`
        // (Matrix-Sitze) ist ebenso ein reines Kind-Profil, genauso
        // `UiaLatexWriter` (Runde 4, Teil E: `latex.build` gibt es nur als
        // gespawntes UIA-Kind, nie als Einstiegsverengung).
        | (
            ReadOnlyExplore | Research | ReadOnlyResearch | Planning | MatrixReader
            | UiaLatexWriter,
            _,
        )
        | (_, ReadOnlyResearch | MatrixReader | UiaLatexWriter) => Err(RuntimeError::Registry {
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
/// [`crate::sandbox::root_sandbox`] oder [`SandboxSpec::restrict`] nie still mehr Rechte
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
        Some(narrowing) => {
            // Das Netz (Hosts) bleibt nur erhalten, wenn die Verengung
            // `NetworkAccess` selbst weiterträgt — so entsteht nie ein
            // Netzrecht ohne Hosts und nie Hosts ohne Netzrecht.
            let keeps_network = narrowing
                .permissions
                .contains(harw_authority::Permission::NetworkAccess);
            let scope = if keeps_network {
                sandbox.network_scope().clone()
            } else {
                NetworkScope::empty()
            };
            let narrowed = sandbox.restrict(
                &PermissionRequest::from_permissions(narrowing.permissions.iter())
                    .with_network_scope(scope),
            );
            if narrowed.network_scope().is_empty()
                && narrowed
                    .permissions()
                    .contains(harw_authority::Permission::NetworkAccess)
            {
                narrowed.restrict(&PermissionRequest::from_permissions(
                    narrowed.permissions().iter().filter(|permission| {
                        *permission != harw_authority::Permission::NetworkAccess
                    }),
                ))
            } else {
                narrowed
            }
        }
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
    /// Öffnet den `secrets:`-Resolver nachträglich, wenn ein Live-
    /// Modellwechsel einen beim Start nicht gebauten Provider braucht
    /// ([`Self::secret_resolver_opener`]).
    secret_resolver_opener: Option<Arc<crate::live_model::SecretResolverOpener>>,
    /// Verengung von Werkzeugsatz, Identität und Sandbox-Rechten durch den
    /// Aufrufer (CONTRACTS-W2d2 §1.1); `None` heißt „Profil unverändert".
    narrowing: Option<RuntimeNarrowing>,
    /// Projekt-Fakten-Wurzel für den Gedächtnis-Recall (Addendum B, §2/§4);
    /// ohne expliziten Aufruf öffnet [`Self::build`] die Vorgabe unter
    /// `home_project.memories_dir()` best-effort selbst.
    project_facts: Option<Arc<harw_memory::FactStore>>,
    /// Globale Fakten-Wurzel, analog zu [`Self::project_facts`]; ohne
    /// expliziten Aufruf bleibt sie `None` — anders als die Projekt-Wurzel
    /// kennt die Montage keinen Vorgabepfad für sie (das Profilverzeichnis
    /// liegt außerhalb von `RuntimeSpec`).
    global_facts: Option<Arc<harw_memory::FactStore>>,
    /// Zusätzliche Lebenszyklus-Haken des Aufrufers (Addendum B,
    /// [`crate::memory_wiring::MemoryConsolidationHook`] u. ä.); werden in
    /// [`Self::build`] mit den Beiträgen der [`AssemblyContributor`]s
    /// zusammengeführt.
    extra_lifecycle_hooks: Vec<Arc<dyn SessionLifecycleHook>>,
    /// Agenten-übergreifender Live-Bus; ohne expliziten Aufruf legt
    /// [`Self::build`] einen frischen an.
    agent_events: Option<harw_core::AgentEventHub>,
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
            .field("project_facts", &self.project_facts.is_some())
            .field("global_facts", &self.global_facts.is_some())
            .field("extra_lifecycle_hooks", &self.extra_lifecycle_hooks.len())
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

    /// Übergibt einen Öffner für den `secrets:`-Resolver (Live-Modellwechsel).
    ///
    /// # Beschreibung
    /// Der Aufrufer öffnet beim Start nur dann einen Resolver, wenn der
    /// Vorgabe-Provider ihn braucht. Wechselt die Sitzung später auf einen
    /// Provider mit `secrets:`-Referenz, öffnet
    /// [`crate::live_model::LiveModelRouting`] den Speicher über diesen
    /// Öffner und baut den Provider-Client neu — ohne Neustart. Ohne Aufruf
    /// scheitert ein solcher Wechsel mit einer klaren Meldung.
    #[must_use]
    pub fn secret_resolver_opener(
        mut self,
        opener: Arc<crate::live_model::SecretResolverOpener>,
    ) -> Self {
        self.secret_resolver_opener = Some(opener);
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
    pub fn agent_events(mut self, hub: harw_core::AgentEventHub) -> Self {
        self.agent_events = Some(hub);
        self
    }

    /// Setzt die Kennung der Wurzelsitzung (siehe unten).
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

    /// Übergibt die Fakten-Stores des Gedächtnis-Recalls (Addendum B, §2/§4).
    ///
    /// # Beschreibung
    /// Beide Argumente sind optional und unabhängig voneinander: fehlt
    /// `project`, öffnet [`Self::build`] die Projekt-Vorgabe unter
    /// `home_project.memories_dir()` selbst (best-effort, `warn!` bei
    /// Fehlschlag); fehlt `global`, bleibt der globale Anteil des Recalls
    /// leer — dafür gibt es keine eingebaute Vorgabe. Ein Aufrufer, der
    /// bereits Stores geöffnet hat (etwa `harw-cli/src/chat.rs`s
    /// `open_fact_stores`), reicht sie hier durch, statt sie doppelt zu
    /// öffnen.
    ///
    /// # Argumente
    /// - `project` (`Option<Arc<harw_memory::FactStore>>`): Projekt-Fakten-
    ///   Wurzel, Projekt geht laut Design §4 im Lesepfad vor.
    /// - `global` (`Option<Arc<harw_memory::FactStore>>`): globale
    ///   Fakten-Wurzel.
    #[must_use]
    pub fn fact_stores(
        mut self,
        project: Option<Arc<harw_memory::FactStore>>,
        global: Option<Arc<harw_memory::FactStore>>,
    ) -> Self {
        self.project_facts = project;
        self.global_facts = global;
        self
    }

    /// Hängt einen zusätzlichen [`SessionLifecycleHook`] an.
    ///
    /// # Beschreibung
    /// Ergänzt, nicht ersetzt: [`Self::build`] führt alle hier angehängten
    /// Haken mit denen zusammen, die [`AssemblyContributor`]s über
    /// [`AssemblyParts::lifecycle_hooks`] beisteuern (Addendum B braucht
    /// diesen Weg für [`crate::memory_wiring::MemoryConsolidationHook`], ohne
    /// dass `harw-memory` selbst ein `AssemblyContributor` werden müsste).
    ///
    /// # Argumente
    /// - `hook` (`Arc<dyn SessionLifecycleHook>`): der anzuhängende Haken.
    #[must_use]
    pub fn lifecycle_hook(mut self, hook: Arc<dyn SessionLifecycleHook>) -> Self {
        self.extra_lifecycle_hooks.push(hook);
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
    /// - [`RuntimeError::Sandbox`] aus [`crate::sandbox::root_sandbox`], oder wenn
    ///   [`RuntimeNarrowing::workspace_root`] relativ ist, nicht existiert oder
    ///   außerhalb des erkannten Projekt-Roots liegt.
    /// - [`RuntimeError::Registry`], wenn die Registry nicht montiert, ein
    ///   benannter Agent nicht aufgelöst werden kann, eine
    ///   [`Self::narrowing`] einen nicht zugelassenen Werkzeugsatz verlangt
    ///   oder einen Werkzeugsatz außer `Full` an einem Einstieg mit
    ///   [`OperationSurface::AllWithModelTools`] verlangt.
    /// - [`RuntimeError::Sandbox`] auch, wenn die verengte Sandbox die
    ///   Profilrechte überschritte (konstruktionsbedingt unerreichbar, aber
    ///   geprüft).
    /// - [`RuntimeError::Spawner`], wenn ein Einstieg mit
    ///   [`SpawnerPolicy::BuiltinRoles`] ohne [`Self::session_events`] gebaut
    ///   wird, die Wurzelregistrierung scheitert, oder der Wurzel-Trace aus
    ///   [`new_root_trace`] nicht erzeugt werden kann (per Konstruktion
    ///   unerreichbar, siehe dort).
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
            secret_resolver_opener,
            narrowing,
            project_facts,
            global_facts,
            extra_lifecycle_hooks,
            agent_events,
        } = self;
        let agent_events = agent_events.unwrap_or_default();

        let model_source = model.ok_or_else(|| RuntimeError::Provider {
            detail: "no model source was given to the runtime builder".to_owned(),
        })?;
        let stores = stores.ok_or_else(|| RuntimeError::Store {
            detail: "no stores were given to the runtime builder".to_owned(),
        })?;

        // #22 Welle 3A: `EntryKind::CompiledAgent` hat keine Tabellenzeile —
        // ihr Profil entsteht aus den Manifest-Rechten des eingebetteten
        // Agenten, nicht aus `EntryKind::profile()` (dessen Zeile für diese
        // Art nur die engste Rückfallzeile ist, siehe `spec.rs`).
        let profile = match spec.embedded.as_ref() {
            Some(embedded) => crate::spec::EntryProfile::for_embedded(embedded.rights()),
            None => spec.entry.profile(),
        };

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
        ensure_narrowing_fits_operations(spec.entry, &profile, narrowing.as_ref())?;

        // 1. Konfiguration mit Vertrauensbericht. Ein eingebetteter Lauf
        //    liest sie vollständig aus dem Speicher (`load_config_embedded`)
        //    und trägt deshalb keinen Vertrauensbericht über `~/.harw`-Layer;
        //    `trust_report` bleibt für ihn leer (kein Layer, kein
        //    nicht-vertrautes Repo).
        let (config, trust_report) = match spec.embedded.as_ref() {
            Some(_) => (
                crate::config::load_config_embedded(&spec)?,
                ConfigTrustReport {
                    layers: Vec::new(),
                    untrusted_repo: None,
                    trust_status: None,
                },
            ),
            None => load_config(&spec)?,
        };
        let config = Arc::new(config);
        // Netz-Werkzeuge (web.fetch/web.search/…) einmal je Prozess mit der
        // Egress-Policy aus `[network]`/`[research]` und dem Such-Backend aus
        // `[web.search]` einrichten — ohne das scheitert jeder Abruf mit
        // `NotConfigured`. Ein ungültiger Host-Eintrag deaktiviert nur das
        // Netz (Warnung), nicht die ganze Sitzung.
        if let Ok(home) = harw_home::home_dir()
            && let Err(error) = harw_registry_defaults::install_web_tools(
                &config,
                &harw_home::paths::cache_dir(&home),
            )
        {
            tracing::warn!(%error, "runtime.web_tools_not_configured");
        }

        // 2. Projekterkennung — genau einmal je Lauf. Profil-eigene
        //    `project_root_markers` ersetzen die Standardmarker.
        let discovery_config = match config.harness.project_root_markers.as_ref() {
            Some(markers) if !markers.is_empty() => {
                DiscoveryConfig::default().with_root_markers(markers.clone())
            }
            _ => DiscoveryConfig::default(),
        };
        let project = discover_project(&spec.cwd, &discovery_config).map_err(|error| {
            RuntimeError::Discovery {
                detail: format!("could not discover the project below the cwd: {error}"),
            }
        })?;

        // 2b. Projekt-Home nach Contract §3 (`harw_home::project`) —
        //     eigenständig von der Projekterkennung oben: jene speist den
        //     Modellkontext, diese die Scope-Architektur (Trust-Anker,
        //     `.harw`, Projekt-Einstellungsdatei). Ein Fehler beim Anlegen
        //     der Home-Verzeichnisse ist nie fatal (z. B. `$HOME` selbst).
        let markers = config
            .harness
            .project_root_markers
            .clone()
            .unwrap_or_default();
        let home_project_root = discover_home_project(&spec.cwd, &markers).map_err(|error| {
            RuntimeError::Discovery {
                detail: format!("could not discover the project home below the cwd: {error}"),
            }
        })?;
        let home_project = ProjectHome::at(&home_project_root);
        if let Err(error) = home_project.ensure() {
            tracing::warn!(
                root = %home_project_root.root.display(),
                error = %error,
                "runtime.project_home.ensure_failed"
            );
        }

        // Addendum B: die Projekt-Erfassungsfläche (`<home_project>/memories`)
        // best-effort öffnen — ein Fehlschlag (kaputtes Verzeichnis, fehlende
        // Rechte) darf die Montage nie zu Fall bringen, nur den Recall/die
        // Erfassung dieses Laufs abschalten.
        let memory_capture =
            match harw_memory::capture::ProjectMemoryCapture::open(&home_project.memories_dir()) {
                Ok(capture) => Some(Arc::new(capture)),
                Err(error) => {
                    tracing::warn!(
                        root = %home_project_root.root.display(),
                        error = %error,
                        "runtime.memory_capture.open_failed"
                    );
                    None
                }
            };

        // Vorgabe der Projekt-Fakten-Wurzel, falls der Aufrufer keine über
        // `RuntimeAssemblyBuilder::fact_stores` mitgebracht hat (siehe dessen
        // Doku). Dieselbe Wurzel wie `memory_capture`, unabhängig davon, ob
        // deren Öffnen gelang — `FactStore::open` legt `facts/` bei Bedarf
        // selbst an.
        let project_facts = project_facts.or_else(|| {
            match harw_memory::FactStore::open(
                home_project.memories_dir(),
                harw_memory::FactScope::Project,
            ) {
                Ok(store) => Some(Arc::new(store)),
                Err(error) => {
                    tracing::warn!(
                        root = %home_project_root.root.display(),
                        error = %error,
                        "runtime.memory_facts.project_default_open_failed"
                    );
                    None
                }
            }
        });

        // Addendum F+G: Wächter-Schwellen, Rollen-Reasoning-Gewichtung und
        // Pitfall-Berater dieses Laufs — alle drei einmalig hier aufgelöst,
        // damit [`Self::new_root_session`] sie unverändert wiederverwendet
        // statt bei jedem Aufruf neu zu bauen (`build_registry` ruft
        // `new_root_session` genau einmal, aber `role_effort_weights` wird
        // auch von [`build_spawner`] gebraucht).
        let guard_policy = crate::guard_wiring::guard_policy_from_config(&config);
        let role_effort_weights = crate::guard_wiring::role_effort_weights_from_config(&config);
        let pitfall_advisor: Option<Arc<dyn PitfallAdvisor>> = project_facts.clone().map(|store| {
            Arc::new(crate::guard_wiring::MemoryPitfallAdvisor::new(store))
                as Arc<dyn PitfallAdvisor>
        });

        // Die komplette Konfigurationskette, einschließlich der
        // autoritätsgewährenden Projekt-Einstellungen außerhalb des Repos,
        // wurde in `load_config` genau einmal gemergt. Ein zweites,
        // permissions-spezifisches Einlesen würde Allow-/Deny-Regeln doppelt
        // registrieren. Die effektive Sektion enthält daher bereits die
        // Projekt-Präzedenz.
        let profile_name = active_profile_name(&spec.home);
        let global_permissions = config.harness.permissions.clone();
        let project_permissions = PermissionsSection::default();
        // Welle FANIN-K/FANIN-RT: das Agentendefinitions-Verzeichnis des
        // aktiven Profils (`<profil>/agents`) — gebraucht sowohl für die
        // Kind-Fabrik (Schritt 9, `agent-steward`-Kinder) als auch für die
        // UIA-Wurzelregistrierung selbst (Schritt 7, unten). Ein nicht
        // auflösbares Profil (`HomeError::InvalidProfileName`) bleibt `None`
        // (fail-closed).
        let profile_agents_dir = profile_dir(&spec.home, &profile_name)
            .ok()
            .map(|dir| dir.join("agents"));

        // 3./4. Sandbox und Decke aus dem Einstiegsprofil.
        //      Eine Verengung schneidet die Sandbox, sie ersetzt sie nie; ein
        //      `workspace_root` bindet sie enger (nie außerhalb des Projekts).
        //      Netz (nur `Tui`/`OneShot`): Hosts ausschließlich aus der
        //      Egress-Allowlist; ohne Allowlist kein Host und kein Netzrecht.
        let bound_root = sandbox_root(&project.project_root, narrowing.as_ref())?;
        let unrestricted = root_sandbox_with_network(
            spec.entry,
            &bound_root,
            root_network_scope(spec.entry, &config),
        )?;
        if narrowing
            .as_ref()
            .is_some_and(|narrowing| narrowing.workspace_root.is_some())
        {
            ensure_bound_to(&unrestricted, &bound_root)?;
        }
        let sandbox = narrowed_sandbox(unrestricted, &profile, narrowing.as_ref())?;
        let ceiling = root_ceiling(profile.ceiling);

        // Zusätzliche Workspace-Wurzeln (`/add-workdir`, Contract §5) aus
        // Global- und Projekt-Konfiguration, gegen die bereits gebundene
        // primäre Wurzel validiert; ungültige Einträge werden übersprungen,
        // nicht abgelehnt.
        let extra_roots = seed_extra_roots(
            &global_permissions,
            &project_permissions,
            &bound_root,
            os_user_home().as_deref(),
        );
        // `SandboxSpec` carries the canonical primary workspace only. The
        // session cell remains available to `/add-workdir` and the operation
        // surfaces, but is not a mutable authority side channel for a frozen
        // sandbox specification.

        // Agent-Definitionen werden einmal gesenkt. Ein interaktiver Einstieg
        // ohne explizit gewählten Wurzel-Agenten besitzt zwingend eine
        // konfigurierte UIA; fehlende oder falsch gerollte Auswahl ist ein
        // Startfehler, nie ein stiller Full-Tool-Fallback.
        let needs_definitions = spec.active_agent.is_some()
            || config.harness.active_uia_definition.is_some()
            || matches!(profile.spawner, SpawnerPolicy::BuiltinRoles);
        let agent_definitions = if needs_definitions {
            lower_agent_definitions(&config)?
        } else {
            HashMap::new()
        };
        // Plan R9, Teil B: der Roster aller startbaren Agenten — eingebaute
        // Rollen plus die benutzerdefinierten Agenten aus Profil und
        // vertrautem Projekt (geklemmt unter ihre Basisrolle). Der Spawner
        // registriert genau diese Namen; Welle 2 (Katalog, `agents.delegate`)
        // liest ihn über `RuntimeAssembly::agent_roster`.
        let agent_roster = Arc::new(if needs_definitions {
            harw_registry_defaults::AgentRoster::from_config(&agent_definitions, &config).map_err(
                |error| RuntimeError::Registry {
                    detail: format!("could not build the agent roster: {error}"),
                },
            )?
        } else {
            harw_registry_defaults::AgentRoster::default()
        });
        // Runde 3, Welle C1: ein explizit gewählter Wurzel-Agent
        // (`--agent`, `harness.active_agent_definition`) gewinnt über die UIA;
        // die UIA ist dann nicht Pflicht. Als Wurzel zugelassen sind nur
        // `root-orchestrator`, `child-orchestrator` und `worker`
        // ([`resolve_explicit_root_agent`]). Ohne expliziten Agenten hat ein
        // UI-Einstieg genau einen Root: die UIA. Die Werkzeuge bleiben in
        // jedem Fall durch Einstiegsprofil und Sandbox gedeckelt.
        let agent_ir =
            resolve_explicit_root_agent(spec.active_agent.as_deref(), &config, &agent_definitions)?;
        // #22 Welle 3: eine native, personalisierte harw bringt ihre UIA
        // bereits eingebettet mit (`spec.embedded`, von `harw-cli` aus
        // `embedded_uia()` gesetzt); die gewinnt dann über
        // `harness.active_uia_definition`. Nur für `Tui`/`OneShot` — dieselbe
        // Gattung wie [`resolve_active_uia`] selbst prüft; `spec.embedded`
        // gehört sonst (Welle 3A) `EntryKind::CompiledAgent`, dessen
        // eingebetteter Wurzel-Agent typischerweise keine UIA ist und dessen
        // Rolle diese Funktion deshalb nicht abfragen darf.
        let uia_ir = if agent_ir.is_some() {
            None
        } else if matches!(spec.entry, EntryKind::Tui | EntryKind::OneShot)
            && let Some(embedded) = spec.embedded.as_deref()
        {
            Some(resolve_embedded_uia(embedded.root_ir())?)
        } else {
            resolve_active_uia(spec.entry, &config, &agent_definitions)?
        };
        // Die Kind-Decke ist die *tatsächliche* Aktivierung der Root-Session:
        // `new_root_session` wendet ausschließlich `agent_ir` an (bei aktiver
        // UIA `None` → `SessionActivation::default()`), nie `uia_ir`. Früher
        // stand hier `uia_ir.or(agent_ir)`; eine UIA-Definition ohne
        // `[tools]`-Abschnitt ergab damit eine leere `Minimal`-Decke, gegen
        // die jedes Kind auf null Werkzeuge geschnitten wurde, während die
        // Wurzel selbst weiter lesen konnte (Explore-Kinder schrieben daraufhin
        // Tool-Calls als Text). Enkel ⊆ Kind ⊆ Wurzel gilt so gegen die echte
        // Wurzel.
        let activation = root_activation(agent_ir.as_ref());
        // #22 Welle 1B: die Wurzel (explizit gewählter Agent, sonst die UIA)
        // bringt ihr Kontextprogramm mit wie jedes Kind — geprüft gegen die
        // Wurzeldecke mit derselben Regel wie die Kind-Admission.
        let root_context_program =
            root_context_program(agent_ir.as_ref().or(uia_ir.as_ref()), &ceiling)?;
        // Welle 8: Provider-/Modell-/Agenten-Reasoning-Effort-Vorgaben der
        // Wurzel-UIA, einmalig hier bestimmt (nicht in
        // [`Self::new_root_session`], das keinen Zugriff auf `uia_ir` selbst
        // hat — nur auf die daraus geklonten Werte).
        let root_uia_reasoning_effort_defaults =
            resolve_root_uia_reasoning_effort_defaults(&config, uia_ir.as_ref());
        // Welle 3: das Modell, dessen Fenster die Wurzel budgetiert.
        let root_model_id = effective_root_model_id(&config, uia_ir.is_some());

        // 5. Ein Trace, ein Spawn-Kontext.
        let trace = new_root_trace(spec.entry).map_err(|error| RuntimeError::Spawner {
            detail: error.to_string(),
        })?;
        let spawn_context = SpawnContext {
            sandbox: sandbox.clone(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: spec.principal.approval_actor(),
            // Die Rolle der Wurzel ist die der aktiven UIA bzw. des explizit
            // gewählten Wurzel-Agenten; ohne beide entscheidet die
            // Spawner-Politik.
            organizational_role: uia_ir.as_ref().or(agent_ir.as_ref()).map_or_else(
                || root_organizational_role(profile.spawner),
                ExecutableAgentIr::role,
            ),
            // The root can delegate a child orchestrator only when its frozen
            // active definition lists that exact role. No active definition
            // means no such delegation grant.
            allowed_child_orchestrators: uia_ir
                .as_ref()
                .or(agent_ir.as_ref())
                .map(|ir| ir.spawn_contract().child_orchestrators().to_vec())
                .unwrap_or_default(),
            trace: Some(trace),
            ceiling: Some(ceiling.clone()),
        };

        // 6. Freigabekette. Der Responder kommt erst mit `new_root_session`:
        //    er gehört zur Oberfläche, nicht zur Montage.
        // Ein erzwungener Modus (`GatewayTelegram`: immer `ask`) schlägt auch
        // einen expliziten Aufrufer-Override.
        let approval_mode = ApprovalModeCell::new(
            forced_approval_mode(spec.entry)
                .or(spec.approval_override)
                .unwrap_or_else(|| {
                    effective_approval_mode(spec.entry, &global_permissions, &project_permissions)
                }),
        );
        let allow_rules = seed_allow_rule_set(&global_permissions, &project_permissions);
        let approval_timeout =
            effective_approval_timeout(&global_permissions, &project_permissions);
        tracing::info!(
            rules = allow_rules.snapshot().len(),
            roots = extra_roots.snapshot().len(),
            mode = %approval_mode.get(),
            project = %home_project_root.root.display(),
            "Freigaben geladen"
        );
        // `profile.ask` ist ab hier durchgesetzt, nicht nur deklariert: alles
        // außer `Interactive` hängt eine `AskResolutionPolicy` in die Kette
        // (Befund Z2c-02).
        let chain = ApprovalChain::for_root(
            &config,
            profile.ask,
            approval_mode.clone(),
            None,
            allow_rules.clone(),
        );
        // Runde 5, Teil E: ausgebauter Auto-Modus (Vorfilter, Klassifizierer,
        // Protokoll, Deckel) nur für Einstiege mit anwesender Person; die
        // Modell-Anbindung folgt nach dem Bau des Wurzelmodells.
        let chain = if profile.ask == AskResolution::Interactive {
            chain.with_auto_mode(crate::auto_classifier::AutoModeHandle::new(
                approval_mode.clone(),
                crate::auto_classifier::PrefilterContext::new(
                    bound_root.clone(),
                    extra_roots.clone(),
                    sandbox.network_scope().clone(),
                    os_user_home(),
                ),
            ))
        } else {
            chain
        };

        // 7. Registry: ein Projektkontext, eine Kette. Der Modellkontext folgt
        //    `profile.project_context` und einem gebundenen `workspace_root`.
        let mut overrides = root_identity(&spec, narrowing.as_ref());
        // Addendum F+G: die organisatorische Rolle der Wurzel-Identität folgt
        // exakt derselben Regel wie `spawn_context.organizational_role` oben
        // (Rolle der aktiven UIA-IR bzw. `root_organizational_role(...)`) —
        // beide dürfen nie auseinanderlaufen.
        overrides.organizational_role = Some(spawn_context.organizational_role);
        // Trägt das Agent-Verzeichnis der aktiven UIA, damit
        // `uia_self.update_document` (siehe `harw_registry_defaults::
        // agent_definition_tools::UiaSelfDocumentToolProvider`) nach dem Bau
        // der Registry an genau dieses Verzeichnis gebunden werden kann.
        // `None`, solange kein `uia_ir` aktiv ist oder ihr Verzeichnis nicht
        // auflösbar ist — dann bleibt das Werkzeug ungeregistriert (kein
        // eigenes `identity.md`/`USER.md`/`Personality.md` ohne UIA-Wurzel).
        let mut uia_agent_dir_for_self_document: Option<PathBuf> = None;
        if let Some(uia) = uia_ir.as_ref() {
            let definition_id = uia.id().to_string();
            if let Some(agent_dir) = config.agent_definition_dirs.get(&definition_id) {
                uia_agent_dir_for_self_document = Some(agent_dir.clone());
                let fragments =
                    harw_config::load_uia_personalization(agent_dir).map_err(|error| {
                        RuntimeError::Registry {
                            detail: format!(
                                "could not load UIA personalization from {}: {error}",
                                agent_dir.display()
                            ),
                        }
                    })?;
                overrides.extra_context.extend(fragments);
            }
        }
        // Welle 4 (Skill-Proposals) / Plan R9, Teil A: die fest gebundenen
        // Skills der Wurzel als Instruktionsfragmente mit SHA-256-Provenienz
        // — derselbe Weg wie für Kinder
        // (`RuntimeChildRegistryFactory::with_skill_catalog`). Quelle ist das
        // `skills`-Feld der Definition der Wurzel (aktive UIA bzw.
        // `--agent`-IR), vereinigt mit einem gleichnamigen Legacy-
        // `agents/<agent>/agent.toml`. Früher wurde nur
        // `config.agents[<Definitions-Id>]` gesucht, ein Schlüssel, den es nie
        // gibt — die UIA bekam deshalb nie Skills. Namen ohne Layer kommen aus
        // dem eingebetteten Bündel (`SkillIndex`). Ein aktivierter, aber nicht
        // ladbarer Skill lässt die Montage scheitern (fail-closed).
        // `root_skill_agent` bleibt die Agent-Id des Tagebuchs (Diary, 12a).
        let root_skill_agent = uia_ir
            .as_ref()
            .map(|ir| ir.id().to_string())
            .or_else(|| overrides.agent_name.clone());
        // #22 Welle 3A: ein eingebetteter Lauf (`spec.embedded`) liest keine
        // Skills aus `~/.harw`-Layern — sie kommen ausschließlich aus dem
        // Blob-Pool des Artefakts (`EmbeddedAgent::skill_bundle`),
        // `build_with_bundle` nimmt dieses in-Memory-Bündel bereits ohne
        // Änderung an `harw-catalog` entgegen.
        let skill_index = Arc::new(match spec.embedded.as_ref() {
            Some(embedded) => {
                let bundle = embedded.skill_bundle();
                let refs: Vec<(&str, &str)> = bundle
                    .iter()
                    .map(|(path, content)| (path.as_str(), content.as_str()))
                    .collect();
                harw_catalog::SkillIndex::build_with_bundle(&[], &refs)
            }
            None => harw_catalog::SkillIndex::build(&trust_report.layers),
        });
        let legacy_skill_agent = match uia_ir.as_ref() {
            Some(uia) => Some(uia.specialization().to_owned()),
            None => overrides.agent_name.clone(),
        };
        overrides
            .extra_context
            .extend(crate::children::root_skill_fragments(
                &config,
                &trust_report.layers,
                &skill_index,
                uia_ir.as_ref().or(agent_ir.as_ref()),
                legacy_skill_agent.as_deref(),
            )?);
        // Plan R9, Teil A: `skills.search`/`skills.load` an der Wurzel — nicht
        // für werkzeuglose Einstiege (`NoTools`) und nicht für den entfernten
        // Telegram-Chat (`WorkspaceEdit`, wie Workbench/Palace); eine
        // `--agent`-Wurzel nur, wenn ihre Definition beide admittiert. Dazu
        // genau eine Kontextzeile mit der Anzahl verfügbarer Skills.
        let skill_catalog_at_root = !matches!(
            registry_profile,
            RegistryProfile::NoTools | RegistryProfile::WorkspaceEdit
        ) && (agent_ir.is_none()
            || harw_registry_defaults::profile::SKILL_CATALOG_TOOLS
                .iter()
                .all(|tool| activation.is_tool_enabled(&ToolName::new((*tool).to_owned()))));
        if skill_catalog_at_root {
            overrides
                .extra_context
                .push(harw_registry_defaults::skill_catalog_hint(
                    skill_index.len(),
                ));
        }
        let narrowed_root = narrowing
            .as_ref()
            .and_then(|narrowing| narrowing.workspace_root.as_ref())
            .map(|_| bound_root.as_path());
        // Welle FANIN-K/FANIN-RT: nur die UIA-Wurzel bekommt die
        // Bauplan-Prüfwerkzeuge (`agents.validate`/`agents.list_proposals`/
        // `agents.commit_proposal`/`agents.reject_proposal`) — nie ein
        // nicht-UIA-Root-Einstieg. Die Decke ist die UIA aus ihren eigenen
        // effektiven Rechten: `tools`/`permissions` aus dem gebundenen
        // Wurzel-`sandbox` dieses Laufs, `max_depth` aus den für das
        // Vorgabemodell abgeleiteten Kindlimits, `budget_tokens` aus
        // [`RootBudget`], `effort_cap` aus
        // [`RoleEffortWeights::uia`](role_effort_weights). `mode` ist immer
        // `Commit`: nur die UIA selbst darf Vorschläge committen/verwerfen
        // (Nachtrag K2).
        // Sandbox-Profil aus vertrauenswürdiger Host-Konfiguration.
        // Standard: Strict (hermetisch). Cargo/tmux werden nur aktiviert,
        // wenn die Konfiguration sie liefert und die Validierung durchgeht.
        let sandbox_profile = sandbox_profile_from_config(&config.harness.sandbox);
        // Einmal je Montage instanziiert (siehe `host_permit_session.rs`-Doku
        // in `harw-sandbox`): trägt jede über die lokale UI bestätigte
        // Host-Freigabe dieses Laufs. Für Strict/Cargo/Tmux ohne
        // `SandboxProfile::Host` bleibt beides ungenutzt — `profile_tool_providers`
        // hängt Ledger/Registry nur an einen tatsächlich Host-profilierten
        // `ShellToolProvider`.
        let host_permit_ledger = Arc::new(ProcessPermitLedger::default());
        let host_permit_registry = Arc::new(HostPermitSessionRegistry::default());
        // Runde 6, Teil A2: der Auto-Modus liest den Status der
        // Host-Arbeitsphase aus derselben Registry (Vorfilter und
        // Klassifizierer-Prompt); die Sitzungs-Id setzt die TUI.
        if let Some(auto) = chain.auto_mode() {
            auto.install_host_lease(Arc::clone(&host_permit_registry));
        }
        // Derselbe Fragekanal-Vertrag wie `harw_tool_shell::exec::ShellExecutor`
        // ihn sendet (siehe [`harw_tool_shell::host_permit_prompt`]): die Sende-
        // seite gehört, sobald ein `ShellToolProvider` mit `SandboxProfile::Host`
        // gebaut wird, über `with_host_permit_prompts` an genau diesen Provider;
        // die Empfängerseite hält [`RuntimeAssembly::take_host_permit_prompts`]
        // bis zur ersten Abholung durch den Renderer (z. B. `harw-tui`) fest.
        let (host_permit_prompt_sender, host_permit_prompt_receiver) = host_permit_prompt_channel();
        // Verdrahtung für `assemble_registry_for_sandbox_with_definition_access_
        // and_sandbox_profile_and_permits`: bündelt Ledger, Sitzungs-Registry und
        // Fragekanal-Sender. Vorauswahl `SessionLease` nur für den Shell-Modus
        // (`InteractionMode::Shell` — laufende Sitzungsphase); jeder andere
        // Modus (und `None`, wenn der Lauf keinen Modus-Override trägt) bleibt
        // beim Default `SingleExecution` aus [`HostPermitWiring::new`].
        let host_permit_wiring = HostPermitWiring::new(
            Arc::clone(&host_permit_ledger),
            Arc::clone(&host_permit_registry),
            host_permit_prompt_sender.clone(),
        );
        let host_permit_wiring = match spec.mode_override {
            Some(InteractionMode::Shell) => {
                host_permit_wiring.with_preselected_variant(HostPermitVariant::SessionLease)
            }
            _ => host_permit_wiring,
        };
        // Runde 5, Teil B: der sudo-Fragekanal (`host.sudo_exec`) entsteht
        // ausschließlich für die interaktive TUI. Jeder andere Einstieg hat
        // keinen Kanal — `RuntimeChildRegistryFactory::with_sudo_exec` baut
        // dann kein Werkzeug (fail-closed). Der Sender reist mit der
        // Host-Permit-Verdrahtung zu den Kind-Fabriken; den Empfänger holt
        // die TUI einmalig über `RuntimeAssembly::take_sudo_prompts`.
        let (sudo_prompt_sender, sudo_prompt_receiver) = if spec.entry == EntryKind::Tui {
            let (sender, receiver) = harw_tool_shell::sudo_prompt_channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let host_permit_wiring = host_permit_wiring.with_sudo_prompts(sudo_prompt_sender);
        // Runde 5, Teil N: Host-Mode-Anfragen (`shell.exec` mit
        // `request_host`) nur in der TUI; reist wie der sudo-Kanal mit der
        // Verdrahtung zu Wurzel und Kind-Fabriken. Die Wurzel-Beschriftung
        // für den Baum-Pfad wird unten beim Kennen der Wurzel-ID eingetragen.
        let host_permit_wiring = host_permit_wiring
            .with_host_escalation(crate::host_escalation_wiring::escalation_for_entry(
                spec.entry,
            ))
            // Runde 5, Teil N: `[shell] max_timeout_secs` für jeden
            // Shell-Provider (Wurzel und Kinder).
            .with_shell_max_timeout_secs(config.harness.shell.effective_max_timeout_secs());
        // Plan R9, Teil F: eine Job-Verwaltung je interaktiver TUI-Sitzung
        // (`<projekt>/.harw/state/jobs/`). Sie reist mit der Host-Permit-
        // Verdrahtung zu Wurzel und Kind-Fabriken: `job.*` neben jedem
        // `shell.exec` (Klon desselben Shell-Providers), Kontrollwerkzeuge
        // für Orchestratoren. Andere Einstiege enden mit ihrem Prozess und
        // bekommen keine Jobs.
        let (session_jobs, job_notifications) = if spec.entry == EntryKind::Tui {
            match crate::job_wiring::SessionJobs::open(&home_project.state_dir()) {
                Ok((jobs, receiver)) => (Some(jobs), Some(receiver)),
                Err(error) => {
                    tracing::warn!(error = %error, "runtime.jobs.open_failed");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        let host_permit_wiring = host_permit_wiring.with_jobs(
            session_jobs
                .as_ref()
                .map(crate::job_wiring::SessionJobs::wiring),
        );
        let host_root_label: Option<String> = if uia_ir.is_some() {
            Some(crate::host_escalation_wiring::UIA_ROOT_LABEL.to_owned())
        } else {
            overrides.agent_name.clone()
        };
        // Teil B3/B4: `host_permit_wiring` wird unten von
        // `assemble_registry_for_sandbox_with_definition_access_and_sandbox_
        // profile_and_permits` (Root-Registry) per Wert konsumiert. Kind-
        // Registries (`build_spawner`s `factory`/`uia_worker_factory`)
        // brauchen dieselbe Verdrahtung aber erst deutlich später — deshalb
        // hier ein Klon (`HostPermitWiring` ist `#[derive(Clone)]`: Ledger
        // und Sitzungs-Registry sind `Arc`, der Fragekanal-Sender ein
        // `mpsc::UnboundedSender`-Klon), als `Option` verpackt, weil
        // [`RuntimeChildRegistryFactory::with_host_permits`] (und damit
        // [`SpawnerInputs::host_permit_wiring`]) generell `Option<HostPermitWiring>`
        // erwartet — auf diesem Pfad ist sie immer `Some`, die Root-Montage
        // baut `host_permit_wiring` bedingungslos.
        let host_permit_wiring_for_children = Some(host_permit_wiring.clone());

        // Skill-Proposals: die Urheber-Decke der UIA für `skills.*` — dieselben
        // Werkzeuge wie ihre `DefinitionAuthorCeiling` unten, dazu die
        // aktivierten MCPs der Konfiguration. `None` ohne UIA-Wurzel; dann
        // wird der Provider gar nicht montiert.
        let uia_skill_ceiling =
            uia_ir
                .as_ref()
                .map(|_| harw_registry_defaults::SkillAuthorCeiling {
                    role: AgentRoleId::UserInterface,
                    tools: registry_profile
                        .tool_names_for(sandbox.permissions())
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    mcps: config
                        .mcps
                        .values()
                        .filter(|mcp| mcp.enabled)
                        .map(|mcp| mcp.name.clone())
                        .collect(),
                });
        let assembled = if uia_ir.is_some() {
            let default_model_id = config.harness.default_model.as_deref().unwrap_or_default();
            let ceiling = harw_registry_defaults::agent_definition_tools::DefinitionAuthorCeiling {
                role: AgentRoleId::UserInterface,
                // Plan Teil D: die lesenden Wissenswerkzeuge trägt die UIA
                // selbst (Schritt 12a–12a'''), also darf sie sie auch in
                // Definitionen vergeben — nur mit `ReadWorkspace` in der
                // Sandbox. Kanban bleibt außen vor (nur auf Nutzerwunsch).
                tools: registry_profile
                    .tool_names_for(sandbox.permissions())
                    .into_iter()
                    .chain(
                        harw_registry_defaults::profile::KNOWLEDGE_READ_TOOLS
                            .iter()
                            .copied()
                            .filter(|_| {
                                sandbox
                                    .permissions()
                                    .contains(harw_authority::Permission::ReadWorkspace)
                            }),
                    )
                    // Plan R9, Teil A: den Skill-Katalog trägt jede Sitzung.
                    .chain(
                        harw_registry_defaults::profile::SKILL_CATALOG_TOOLS
                            .iter()
                            .copied(),
                    )
                    .map(str::to_owned)
                    .collect(),
                permissions: sandbox.permissions().clone(),
                max_depth: child_limits(&config, default_model_id).max_depth,
                budget_tokens: RootBudget::from_config(&config, spec.entry).max_total_tokens,
                effort_cap: Some(role_effort_weights.uia.to_string()),
            };
            let access = harw_registry_defaults::profile::AgentDefinitionAccess {
                project_agents_dir: Some(
                    project
                        .project_root
                        .join(harw_home::project_dir_name())
                        .join("agents"),
                ),
                profile_agents_dir: profile_agents_dir.clone(),
                mode: harw_registry_defaults::agent_definition_tools::DefinitionWriteMode::Commit,
                ceiling: Some(ceiling),
            };
            {
                // Fuer die UIA-Wurzel: Profil weiterreichen.
                let project_ctx = registry_project_context(&project, &profile, narrowed_root);
                let granted = registry_profile.required_permissions();
                harw_registry_defaults::profile::assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
                    registry_profile,
                    &project_ctx,
                    overrides,
                    chain.mode().clone(),
                    &granted,
                    Some(access),
                    &sandbox_profile,
                    Some(host_permit_wiring),
                )
            }
        } else {
            {
                let project_ctx = registry_project_context(&project, &profile, narrowed_root);
                let granted = registry_profile.required_permissions();
                harw_registry_defaults::profile::assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
                    registry_profile,
                    &project_ctx,
                    overrides,
                    chain.mode().clone(),
                    &granted,
                    None,
                    &sandbox_profile,
                    Some(host_permit_wiring),
                )
            }
        }
        .map_err(|error| RuntimeError::Registry {
            detail: format!("could not assemble the root registry: {error}"),
        })?;
        // `install_over_default`: `assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits` hat die
        // `DefaultApprovalPolicy` über `chain.mode()` gerade selbst registriert
        // (harw-registry-defaults/src/profile.rs:921-922) — eine zweite wäre
        // eine Dublette (Befund Z2c-06).
        let mut registry_builder = chain.install_over_default(assembled.registry);
        // Nur die UIA-Wurzel bekommt `uia_self.update_document` — ein
        // Root-Orchestrator-/Worker-Einstieg (kein `uia_agent_dir_for_self_document`)
        // hat kein eigenes `identity.md`/`USER.md`/`Personality.md`, das dieses
        // Werkzeug pflegen könnte (Structure Plan §4: nur `role =
        // "user-interface"`).
        if let Some(agent_dir) = uia_agent_dir_for_self_document {
            registry_builder = registry_builder.tool_provider(Arc::new(
                harw_registry_defaults::agent_definition_tools::UiaSelfDocumentToolProvider::new(
                    agent_dir,
                    AgentRoleId::UserInterface,
                ),
            ));
        }
        // Plan R9, Teil A: der lesende Skill-Katalog (siehe oben).
        if skill_catalog_at_root {
            registry_builder = registry_builder.tool_provider(Arc::new(
                harw_registry_defaults::SkillCatalogToolProvider::new(Arc::clone(&skill_index)),
            ));
        }
        // Skill-Proposals: nur die UIA-Wurzel prüft und committet
        // Skill-Vorschläge sofort (`DefinitionWriteMode::Commit`, analog
        // Nachtrag K2). Ziel ist `<profil>/skills`; ein nicht auflösbares
        // Profil lässt den Provider fail-closed ohne Ablage.
        if let Some(ceiling) = uia_skill_ceiling {
            registry_builder = registry_builder.tool_provider(Arc::new(
                harw_registry_defaults::SkillProposalToolProvider::new(
                    profile_dir(&spec.home, &profile_name)
                        .ok()
                        .map(|dir| dir.join("skills")),
                    harw_registry_defaults::DefinitionWriteMode::Commit,
                    Some(ceiling),
                ),
            ));
        }

        // 7b. Plan-Dienste: expliziter Builder-Wert gewinnt; sonst eingebaute
        //     Vorgabe für interaktive TUI-Einstiege, sofern `[tools.plan]`
        //     nicht ausdrücklich abweicht (Plan Schritt 1, G-010/F-154,
        //     G-024/G-098).
        let plan_services = resolve_plan_services(
            spec.entry,
            plan_services,
            &config.harness.tools.plan,
            &home_project,
        )?;

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
        //
        //    Welle 3a: `source_is_configured` wird **vor** dem Verbrauch von
        //    `model_source` ermittelt ([`ModelSource`] wird by-value
        //    konsumiert) — [`split_root_and_uia_worker_models`] braucht ihn,
        //    um zu entscheiden, ob die UIA-Sitzung überhaupt einen eigenen
        //    HTTP-Client bauen darf (siehe dessen Doku und
        //    [`crate::model::build_uia_model_with_resolver`]).
        let source_is_configured = matches!(&model_source, ModelSource::Configured);
        // Der Offline-Echo ist kein echtes Modell: Wurzel und Kinder bekommen
        // ohne bekanntes Fenster [`OFFLINE_ECHO_CONTEXT_WINDOW_TOKENS`] statt
        // des 32k-Rückfalls für unbekannte echte Modelle
        // ([`session_model_limits`]).
        let offline_echo = matches!(&model_source, ModelSource::Echo(_));
        let (default_tree_model, root_load_registry, root_backends) =
            crate::model::build_root_model_with_backends(
                &spec,
                &config,
                model_source,
                secret_resolver.as_deref().map(|r| r as &dyn SecretResolver),
            )?;
        // Live-Modellwechsel: generische Auswahl und Rollenwahl für neu
        // gestartete Kinder, Neubau eines beim Start nicht baubaren
        // Provider-Clients (siehe `crate::live_model`).
        let live_models = Arc::new(
            crate::live_model::LiveModelRouting::new(Arc::clone(&config))
                .with_backends(root_backends, spec.home.clone())
                .with_secret_resolvers(secret_resolver.clone(), secret_resolver_opener),
        );
        // Welle 3a: bei aktiver UIA bekommt sie ihr eigenes Provider-Modell
        // (`uia_client`), aus dem sich zusätzlich das Modell der gesamten
        // `uia-worker`-Rollenfamilie ableitet (`uia_worker_model`) — beide
        // unabhängig vom `default_tree_model`, das weiterhin jede andere
        // Kind-Rolle bedient. Ohne aktive UIA sind beide Rückgabewerte
        // unverändert `Arc::clone(&default_tree_model)` (bit-identisch zum
        // bisherigen Verhalten).
        //
        // `uia_load_registry` trägt zusätzlich zur [`ProviderLoadControl`]-
        // Registry des Wurzel-Baums nur die Handles, die ein **eigenständiger**
        // UIA-Client gebaut hat (leer, solange die UIA denselben
        // `default_tree_model` weiterverwendet — siehe
        // [`crate::model::build_uia_model_with_registry_and_resolver`]).
        let (model, uia_worker_model, uia_load_registry) = split_root_and_uia_worker_models(
            &spec,
            &config,
            source_is_configured,
            &default_tree_model,
            uia_ir.as_ref(),
            secret_resolver.as_deref().map(|r| r as &dyn SecretResolver),
        )?;
        // Runde 5, Teil E: Klassifizierer-Modell (Rolle `auto-classifier`,
        // Vorgabe: schnelles Modell des aktiven Providers) an den Auto-Modus.
        // Nur bei konfiguriertem Provider: ein eingespieltes Modell (Echo,
        // Tests) bekommt keinen Klassifizierer — dort gilt „ask" wie bisher,
        // und kein Klassifizierer-Aufruf verbraucht eine Skript-Antwort.
        if let Some(auto) = chain.auto_mode().filter(|_| source_is_configured) {
            // Der Baum-Provider bedient wie bei den Kind-Rollen jede interne
            // Modellstelle (Pin über Provider-Id + Modell).
            if let Some(backend) = crate::auto_classifier::ModelClassifierBackend::from_config(
                &config,
                Arc::clone(&default_tree_model),
                root_model_id.as_deref(),
            ) {
                auto.install_backend(Arc::new(backend));
            }
        }
        // Runde 5, Teil G: eigene Modellwahl je UIA-Worker-Rolle, gebaut um
        // den UIA-Client (`model`); die TUI hält sie über
        // `RuntimeAssembly::uia_worker_routing` aktuell.
        let uia_worker_routing = Arc::new(
            crate::uia_worker_routing::UiaWorkerRouting::from_config(&config, Arc::clone(&model)),
        );
        // Merge: derselbe Provider-Name in beiden Registries → der
        // Wurzel-Baum-Eintrag gewinnt (er bedient die meisten Kind-Rollen und
        // ist damit der repräsentative Handle für `/status`/`/provider`; ein
        // zweiter, gleichnamiger Handle aus der UIA-Registry wäre für dieselbe
        // HTTP-Verbindung ohnehin redundant, siehe
        // `crate::model::build_uia_model_with_registry_and_resolver`-Doku).
        let mut provider_load_registry: ProviderLoadRegistry = uia_load_registry;
        provider_load_registry.extend(root_load_registry);
        let root_session_id = root_session_id.unwrap_or_else(SessionId::new);
        // Runde 5, Teil N: Wurzel des Baum-Pfads für Host-Mode-Anfragen.
        crate::host_escalation_wiring::register_root(
            host_permit_wiring_for_children.as_ref(),
            &root_session_id,
            host_root_label.as_deref(),
        );
        // Plan Teil D: der Wissensspeicher des Profils entsteht **vor** dem
        // Spawner, damit Wurzel (Schritt 11/12a) und Kind-Registries
        // dieselbe Instanz teilen. Er legt nichts an; ohne auflösbares
        // Profilverzeichnis bleibt er `None` (die Dienste bauen ihn dann wie
        // bisher aus dem Home-Kontext, und Kinder bekommen keine
        // Wissenswerkzeuge).
        let knowledge_store = profile_dir(&spec.home, &profile_name).ok().map(|dir| {
            Arc::new(harw_knowledge::KnowledgeStore::new(
                &harw_home::knowledge_dir(&dir),
            ))
        });
        let child_knowledge = knowledge_store.as_ref().map(|store| {
            (
                Arc::clone(store),
                crate::services::kanban_transitions(stores.job_store.as_ref(), &spec.principal),
            )
        });
        let (spawner, spawner_roles) = build_spawner(
            profile.spawner,
            SpawnerInputs {
                config: &config,
                project: &project,
                chain: &chain,
                model: &default_tree_model,
                uia_worker_model: &uia_worker_model,
                // Runde 5, Teil G.
                uia_worker_routing: Arc::clone(&uia_worker_routing),
                live_models: Arc::clone(&live_models),
                root_session_id: &root_session_id,
                root_model_id: root_model_id.clone(),
                offline_echo,
                spawn_context: &spawn_context,
                reasoning_effort: spec.reasoning_effort,
                activation: &activation,
                roster: &agent_roster,
                guard_policy,
                pitfall_advisor: pitfall_advisor.clone(),
                profile_agents_dir: profile_agents_dir.clone(),
                // Welle 8: dieselbe aufgelöste Config, aus der auch
                // `resolve_internal_models_for_children` gespeist wird — ohne
                // diesen Aufruf liefern beide Kind-Fabriken für jede Rolle
                // `(None, None)` an `reasoning_effort_defaults_for_role_task`
                // (Kompatibilitäts-Default, siehe
                // `RuntimeChildRegistryFactory::with_reasoning_effort_config`).
                reasoning_effort_config: Arc::clone(&config),
                // Teil B4: dasselbe Sandbox-Profil und dieselbe (geklonte)
                // Host-Permit-Verdrahtung wie die Root-Registry — reicht
                // `build_spawner` an `RuntimeChildRegistryFactory::with_host_permits`
                // für `factory` **und** `uia_worker_factory` durch (`uia-shell-worker`,
                // `host-process-worker`).
                sandbox_profile: &sandbox_profile,
                host_permit_wiring: &host_permit_wiring_for_children,
                state_store: Arc::clone(&stores.state_store),
                agent_events: agent_events.clone(),
                // Welle 4: die vertrauten Config-Layer, aus denen die
                // Kind-Fabriken die Skill-Verzeichnisse auflösen.
                skill_roots: trust_report.layers.clone(),
                // Plan Teil D: lesende Wissenswerkzeuge der Kinder.
                knowledge: child_knowledge,
                // Welle 3C: reicht `RuntimeSpec::child_backend` an den
                // gebauten `ManagedAgentSpawner` durch.
                child_backend: spec
                    .child_backend
                    .as_ref()
                    .map(|handle| Arc::clone(&handle.0)),
            },
            session_events,
        )?;
        // Die Root-Registry erhält den echten ManagedAgentSpawner. Nur
        // Kind-Registries verwenden den schwachen Deferred-Adapter; dadurch
        // kann ein Root-Orchestrator weiter delegieren, ohne einen
        // Referenzzyklus zwischen Registry, Spawner und Factory zu erzeugen.
        if let Some(spawner) = spawner.as_ref() {
            let managed_spawner = Arc::clone(spawner);
            let spawner: Arc<dyn AgentSpawner> = managed_spawner;
            registry_builder = registry_builder.spawner(spawner);
        }
        // Addendum F+G ("Zombies"): der periodische Kind-Reaper braucht eine
        // laufende Tokio-Runtime — geprüft **hier**, nicht in
        // `guard_wiring::spawn_child_reaper` selbst (dessen `tokio::task::spawn`
        // würde ohne Runtime panicken statt sauber zu degradieren). Ohne
        // Spawner (`SpawnerPolicy::None`) gibt es nichts zu räumen.
        if let Some(spawner) = spawner.as_ref() {
            match tokio::runtime::Handle::try_current() {
                Ok(_handle) => {
                    let _reaper = crate::guard_wiring::spawn_child_reaper(
                        Arc::clone(spawner),
                        Duration::from_secs(30),
                    );
                }
                Err(error) => {
                    tracing::debug!(
                        error = %error,
                        "runtime.child_reaper.no_tokio_runtime_skipping"
                    );
                }
            }
        }

        // Plan R9, Teil F: Zustellung der Job-Ereignisse — Kinder über die
        // Journale des Spawners, sonst die Wurzel; dazu das Signal für die
        // Jobs-Gruppe des Agenten-Panels. Ein Wurzel-Profil ohne Shell (etwa
        // eine Orchestrator-Wurzel) verfolgt und stoppt Jobs seiner Kinder
        // über die Kontrollwerkzeuge.
        if let Some(jobs) = session_jobs.as_ref() {
            jobs.router.bind(
                spawner
                    .as_ref()
                    .map(|spawner| Arc::clone(spawner.child_comms())),
                root_session_id.clone(),
            );
            jobs.router.attach_agent_events(agent_events.clone());
            if let Some(receiver) = job_notifications {
                crate::job_wiring::spawn_forwarder(Arc::clone(&jobs.router), receiver);
            }
            let root_has_shell = registry_profile
                .registered_tool_names()
                .contains(&"shell.exec");
            if !root_has_shell && registry_profile != RegistryProfile::NoTools {
                registry_builder = registry_builder.tool_provider(jobs.wiring().control_provider());
            }
        }

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

        // Addendum B: Lebenszyklus-Haken des Aufrufers ([`Self::lifecycle_hook`])
        // und — falls die Erfassungsfläche geöffnet werden konnte — der
        // Konsolidierungs-Haken werden mit den Beiträgen der Contributors
        // zusammengeführt. Reihenfolge ist reine Diagnose ([`Self::close_session`]
        // ruft jeden Haken, keiner kann einen anderen verhindern).
        let mut lifecycle_hooks = lifecycle_hooks;
        lifecycle_hooks.extend(extra_lifecycle_hooks);
        if let Some(capture) = memory_capture.as_ref() {
            lifecycle_hooks.push(Arc::new(crate::memory_wiring::MemoryConsolidationHook::new(
                Arc::clone(capture),
            )) as Arc<dyn SessionLifecycleHook>);
        }

        let operations = Arc::new(operations);

        // 11. Dienste. Sie entstehen **vor** dem Bau der Registry, weil die
        //     Modell-Tool-Fläche der Operationen ihre Service-Map braucht.
        let home_context = Arc::new(
            harw_home::ResolvedHomeContext::new(
                &spec.home,
                profile_name.clone(),
                home_project_root.clone(),
            )
            .map_err(|error| RuntimeError::Config {
                detail: error.to_string(),
            })?,
        );
        let services = RuntimeServices::new(RuntimeServicesParts {
            operations: Arc::clone(&operations),
            state_store: Arc::clone(&stores.state_store),
            job_store: stores.job_store.clone(),
            spawner: spawner.clone(),
            memory,
            config: Arc::clone(&config),
            plan: plan_services,
            approval_mode: approval_mode.clone(),
            allow_rules: allow_rules.clone(),
            extra_roots: extra_roots.clone(),
            principal: spec.principal.clone(),
            session_controller,
            provider_load_registry: provider_load_registry.clone(),
            // Teil B3: derselbe Wurzel-Ledger/-Registry/-Sender wie
            // `Self::host_permit_ledger`/`host_permit_session_registry`/
            // `host_permit_prompt_sender` (siehe deren Accessoren unten) —
            // nur `Arc::clone`/Sender-Klon, kein zweiter Ledger. Der
            // `RuntimeAssembly`-Literal am Ende dieser Funktion bewegt die
            // ungeklonten Originale, darum wird hier geklont statt bewegt.
            host_permit_handles: Some(Arc::new(HostPermitHandles {
                ledger: Arc::clone(&host_permit_ledger),
                registry: Arc::clone(&host_permit_registry),
                prompts: Some(host_permit_prompt_sender.clone()),
            })),
        });
        // Plan Teil D: dieselbe Speicher-Instanz wie die Kind-Registries;
        // `with_home_context` legt nur dann einen eigenen an, wenn hier
        // keiner gesetzt wurde.
        let services = match &knowledge_store {
            Some(store) => services.with_knowledge_store(Arc::clone(store)),
            None => services,
        }
        .with_home_context(Arc::clone(&home_context))
        .with_agent_events(Arc::new(agent_events.clone()));
        // Live-Modellwechsel: `/model switch` & Co. bauen darüber einen
        // Provider-Client neu bzw. übernehmen die Rollenwahl.
        // Plan R9, Teil F: `/jobs` und die Jobs-Gruppe der TUI (Slash-Fläche).
        let services = match session_jobs.as_ref() {
            Some(jobs) => services.with_job_manager(Arc::clone(&jobs.manager)),
            None => services,
        };
        let services = services.with_live_model_control(
            Arc::clone(&live_models) as harw_ops::live_model::SharedLiveModelControl
        );
        // Live-Stand der Konfiguration: gespeicherte Änderungen (`/models
        // set`, `/model switch`, `/mode default`, …) sind sofort in jeder
        // neu gebauten `ServiceMap` sichtbar — Ansichten zeigen nie mehr den
        // Stand des Starts.
        let services = services.with_live_config(Arc::new(harw_ops::live_config::LiveConfig::new(
            Arc::clone(&config),
        )));
        // Runde 5, Teil E: `/permissions log` liest das Auto-Modus-Protokoll.
        let services = match chain.auto_mode() {
            Some(auto) => services.with_auto_decision_log(auto.log().clone()),
            None => services,
        };
        // 11a. Traum-Starter für `/dream run` (Plan D5) — nur für die
        //      interaktiven Einstiege (TUI, One-Shot). Der Gateway-Traum
        //      (`EntryKind::GatewayDream`) hat seinen eigenen Scheduler; alle
        //      übrigen Einstiege bekommen keinen Starter, `/dream run` meldet
        //      dort sauber „nicht verfügbar“. Ohne `KnowledgeStore` gibt es
        //      nichts zu träumen.
        let services = match (
            matches!(spec.entry, EntryKind::Tui | EntryKind::OneShot),
            services.knowledge_store().cloned(),
        ) {
            (true, Some(knowledge)) => {
                services.with_dream_launcher(Arc::new(crate::dream_run::RuntimeDreamLauncher::new(
                    Arc::clone(&model),
                    knowledge,
                    stores.job_store.clone(),
                    crate::dream_run::transcript_root_for_profile(
                        &home_context.profile_dir,
                        &config,
                    ),
                    Arc::clone(&config),
                )))
            }
            _ => services,
        };
        // Runde 5, Teil P: der Plan-Fragekanal der TUI entsteht schon hier
        // (vorher erst in 12e), damit die `plan`-Operation ihn als
        // `PlanConfirmChannel` in ihrer Service-Map findet. Schritt 12e
        // nutzt denselben Sender/Empfänger.
        let (plan_ui_sender, plan_ui_receiver) = if spec.entry == EntryKind::Tui {
            let (sender, receiver) = harw_tool_plan::plan_ui_channel();
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let services = match &plan_ui_sender {
            Some(sender) => {
                services.with_plan_confirm(harw_tool_plan::PlanConfirmChannel::new(sender.clone()))
            }
            None => services,
        };
        let services = Arc::new(services);

        // 12. Modell-Tool-Fläche der Operationen — **nur** für
        //     `OperationSurface::AllWithModelTools`.
        let registry_builder = install_operation_model_tools(
            registry_builder,
            profile.operations,
            &operations,
            &services,
        );
        // Runde 5, Teil H: `agent.result` an der Wurzel — nur mit Spawner und
        // Modell-Werkzeugfläche; ein expliziter Wurzel-Agent nur, wenn seine
        // Definition es admittiert (siehe `agent_result_wiring`).
        let agent_result_root = spawner.as_ref().filter(|_| {
            profile.operations == OperationSurface::AllWithModelTools
                && match agent_ir.as_ref() {
                    Some(_) => activation.is_tool_enabled(&ToolName::new(
                        harw_core_bridge::AGENT_RESULT_TOOL.to_owned(),
                    )),
                    None => true,
                }
        });
        let registry_builder = match agent_result_root {
            Some(spawner) => {
                let spawner = Arc::clone(spawner);
                registry_builder.tool_provider(Arc::new(
                    crate::agent_result_wiring::agent_result_provider(move || {
                        Some(Arc::clone(&spawner))
                    }),
                ))
            }
            None => registry_builder,
        };
        // Runde 5, Teil M: `agent.message` an der Wurzel — dieselbe Bedingung
        // wie `agent.result` (Spawner, Modell-Werkzeugfläche, bei explizitem
        // Wurzel-Agenten nur, wenn seine Definition es admittiert). Die
        // Wurzel hat keinen Elternteil, also kein `parent.message`.
        let agent_message_root = spawner.as_ref().filter(|_| {
            profile.operations == OperationSurface::AllWithModelTools
                && match agent_ir.as_ref() {
                    Some(_) => activation.is_tool_enabled(&ToolName::new(
                        harw_core_bridge::AGENT_MESSAGE_TOOL.to_owned(),
                    )),
                    None => true,
                }
        });
        let registry_builder = match agent_message_root {
            Some(spawner) => {
                let spawner = Arc::clone(spawner);
                registry_builder.tool_provider(Arc::new(
                    crate::agent_messaging_wiring::agent_message_provider(move || {
                        Some(Arc::clone(&spawner))
                    }),
                ))
            }
            None => registry_builder,
        };
        // Runde 5, Teil K: `agent.status`/`agent.cancel` nur an der
        // TUI-Wurzel (Hintergrund-Agenten gibt es nur dort) — mit Spawner und
        // Modell-Werkzeugfläche; ein expliziter Wurzel-Agent nur, wenn seine
        // Definition die Werkzeuge admittiert.
        let registry_builder = match spawner.as_ref().filter(|_| {
            spec.entry == EntryKind::Tui
                && profile.operations == OperationSurface::AllWithModelTools
        }) {
            Some(spawner) => {
                let spawner = Arc::clone(spawner);
                let admitted: Vec<&'static str> = harw_core_bridge::AGENT_BACKGROUND_TOOLS
                    .iter()
                    .copied()
                    .filter(|tool| match agent_ir.as_ref() {
                        Some(_) => activation.is_tool_enabled(&ToolName::new((*tool).to_owned())),
                        None => true,
                    })
                    .collect();
                crate::agent_background_wiring::install_agent_background_tools(
                    registry_builder,
                    &admitted,
                    move || Some(Arc::clone(&spawner)),
                )
            }
            None => registry_builder,
        };
        // 12a. Workbench-Werkzeuge der Wurzelsitzung: nur, wenn die Dienste
        //      einen `KnowledgeStore` tragen (`RuntimeServices::with_home_context`
        //      baut ihn aus dem aufgelösten Home-Kontext) **und** die Sitzung
        //      überhaupt Werkzeuge führen darf: ein `NoTools`-Profil
        //      (`McpServe`, `JobPrompt`, Gateways) bleibt werkzeuglos, und die
        //      (ggf. verengte) Sandbox muss jede Rechteklasse der Werkzeuge
        //      (`WorkbenchToolProvider::TOOL_PERMISSIONS`, `ReadWorkspace`)
        //      tragen — sonst wäre die Rechte-Tabelle der Einstiege gebrochen.
        //      `WorkspaceEdit` (`GatewayTelegram`) bleibt ebenfalls ohne
        //      Workbench: ein entfernter Chat bekommt genau die Lese- und
        //      Schreibwerkzeuge seines Profils, keine Notizablage im
        //      Profil-Home.
        let workbench_allowed = !matches!(
            registry_profile,
            RegistryProfile::NoTools | RegistryProfile::WorkspaceEdit
        )
            && harw_registry_defaults::WorkbenchToolProvider::TOOL_PERMISSIONS
                .iter()
                .flatten()
                .all(|needed| sandbox.permissions().contains(*needed));
        //      Neben den beiden Schreib-Notizwerkzeugen bekommt die Wurzel
        //      das lesende `workbench.show` (auf das gebundene Projekt
        //      gedeckelt) und den Workbench-Kontext (Plan D1).
        let registry_builder = match services.knowledge_store() {
            Some(store) if workbench_allowed => registry_builder
                .tool_provider(Arc::new(
                    harw_registry_defaults::WorkbenchToolProvider::new(Arc::clone(store))
                        // Runde 5, Teil C: Live-Update der `/workbench`-Ansicht
                        // nach `workbench.note`/`workbench.hypothesis`.
                        .with_change_notifier({
                            let hub = agent_events.clone();
                            Arc::new(move |session: &str, scope: &str| {
                                hub.publish_knowledge(
                                    SessionId::from_str(session),
                                    "workbench",
                                    Some(scope.to_owned()),
                                );
                            })
                        }),
                ))
                .tool_provider(Arc::new(
                    harw_registry_defaults::WorkbenchReadToolProvider::new(Arc::clone(store))
                        .with_project(harw_knowledge::workbench::WorkbenchScope::project_for_path(
                            &bound_root,
                        )),
                ))
                .context_provider(Arc::new(
                    harw_registry_defaults::WorkbenchContextProvider::new(Arc::clone(store)),
                ))
                .map_err(|error| RuntimeError::Registry {
                    detail: format!("could not register the workbench context provider: {error}"),
                })?,
            _ => registry_builder,
        };

        // 12a'. Palace-Lesewerkzeuge (Plan D4): `palace.search`/`palace.recall`
        //       zeigen nur `established`-Knoten mit gedeckelten Hops. Dieselbe
        //       Zulassung wie die Workbench (Rechteklasse `ReadWorkspace`,
        //       kein `NoTools`, kein Telegram-`WorkspaceEdit` — ein entfernter
        //       Chat liest kein Palace).
        let palace_allowed = !matches!(
            registry_profile,
            RegistryProfile::NoTools | RegistryProfile::WorkspaceEdit
        ) && harw_registry_defaults::PalaceToolProvider::TOOL_PERMISSIONS
            .iter()
            .flatten()
            .all(|needed| sandbox.permissions().contains(*needed));
        let registry_builder = match services.knowledge_store() {
            Some(store) if palace_allowed => registry_builder.tool_provider(Arc::new(
                harw_registry_defaults::PalaceToolProvider::new(Arc::clone(store)),
            )),
            _ => registry_builder,
        };

        // 12a''. Kanban-Lesewerkzeuge (Plan D2): `kanban.list`/`kanban.show`
        //        lesen Boards und Karten; der Kartenzustand kommt nur lesend
        //        aus dem Job-Ledger (`JobTransitions::snapshot`). Entscheidung
        //        der Nutzerin: Kanban nur auf ihren ausdrücklichen Wunsch —
        //        deshalb nur für die interaktive TUI-Wurzel (UIA) oder einen
        //        explizit gewählten Wurzel-Agenten, dessen Definition die
        //        Werkzeuge admittiert (heute nur `root-orchestrator`), und nie
        //        auto-freigegeben (jeder Aufruf fragt). Sonst wie die
        //        Workbench (kein Telegram, `ReadWorkspace`).
        let kanban_root = match agent_ir.as_ref() {
            Some(_) => harw_registry_defaults::KanbanReadToolProvider::TOOL_NAMES
                .iter()
                .all(|tool| activation.is_tool_enabled(&ToolName::new((*tool).to_owned()))),
            None => spec.entry == EntryKind::Tui,
        };
        let kanban_allowed = kanban_root
            && !matches!(
                registry_profile,
                RegistryProfile::NoTools | RegistryProfile::WorkspaceEdit
            )
            && harw_registry_defaults::KanbanReadToolProvider::TOOL_PERMISSIONS
                .iter()
                .flatten()
                .all(|needed| sandbox.permissions().contains(*needed));
        let registry_builder = match services.knowledge_store() {
            Some(store) if kanban_allowed => registry_builder.tool_provider(Arc::new(
                harw_registry_defaults::KanbanReadToolProvider::new(Arc::clone(store))
                    .with_ledger(services.kanban_ledger(ServiceSurface::ModelTool)),
            )),
            _ => registry_builder,
        };

        // 12a'''. Diary (Plan D3): automatische Einträge über den
        //         `DiaryRecorder` (Sitzungsende, Verdichtung) und das lesende
        //         `diary.read`, das nur die Einträge des Wurzel-Agenten zeigt —
        //         dieselbe Agent-Id bekommen Recorder und Werkzeug. Der
        //         Recorder schreibt unabhängig von der Werkzeugfläche, sobald
        //         ein `KnowledgeStore` existiert; das Werkzeug folgt der
        //         Workbench-Zulassung (gleiche Rechteklasse, kein Telegram).
        let diary_agent = harw_knowledge::AgentId::new(
            root_skill_agent
                .clone()
                .unwrap_or_else(|| "root".to_owned()),
        );
        let diary_recorder = services.knowledge_store().map(|store| {
            Arc::new(
                crate::diary_wiring::DiaryRecorder::new(Arc::clone(store), diary_agent.clone())
                    .with_agent_events(agent_events.clone()),
            )
        });
        let diary_allowed = !matches!(
            registry_profile,
            RegistryProfile::NoTools | RegistryProfile::WorkspaceEdit
        ) && harw_registry_defaults::DiaryToolProvider::TOOL_PERMISSIONS
            .iter()
            .flatten()
            .all(|needed| sandbox.permissions().contains(*needed));
        let registry_builder = match services.knowledge_store() {
            Some(store) if diary_allowed => registry_builder.tool_provider(Arc::new(
                harw_registry_defaults::DiaryToolProvider::new(Arc::clone(store), diary_agent),
            )),
            _ => registry_builder,
        };
        if let Some(recorder) = &diary_recorder {
            lifecycle_hooks.push(Arc::clone(recorder) as Arc<dyn SessionLifecycleHook>);
        }

        // 12b. Handoff-Kontext: liest, falls vorhanden,
        //     `<home_project>/.harw/handoff.json` (siehe
        //     `crate::handoff::handoff_path`) in den Modellkontext der
        //     Wurzelsitzung ein — dieselbe Registrierungsart wie jeder andere
        //     Provider (`ExtensionRegistryBuilder::context_provider`).
        //     `home_project` wird unten in den `RuntimeAssembly`-Literal
        //     verschoben, deshalb hier geklont statt geliehen.
        let registry_builder = registry_builder
            .context_provider(Arc::new(crate::handoff::HandoffContextProvider::new(
                home_project.clone(),
            )))
            .map_err(|error| RuntimeError::Registry {
                detail: format!("could not register the handoff context provider: {error}"),
            })?;

        // 12c. Gedächtnis-Fakten-Recall (Addendum B, §2/§4). Registriert nur,
        //      wenn mindestens eine Quelle etwas beitragen könnte (siehe
        //      `MemoryFactsContextProvider`-Doku für die Begründung, warum
        //      dies **nicht** über
        //      `harw_memory::context_provider::MemoryContextProvider`
        //      läuft).
        let registry_builder = if project_facts.is_some() || global_facts.is_some() {
            let file_index =
                harw_memory::file_index::FileKnowledgeIndex::open(&home_project.memories_dir())
                    .map(Arc::new)
                    .map_err(|error| {
                        tracing::warn!(
                            error = %error,
                            "runtime.memory_file_index.open_failed"
                        );
                    })
                    .ok();
            registry_builder
                .context_provider(Arc::new(MemoryFactsContextProvider::new(
                    project_facts.clone(),
                    global_facts.clone(),
                    file_index,
                )))
                .map_err(|error| RuntimeError::Registry {
                    detail: format!(
                        "could not register the memory facts context provider: {error}"
                    ),
                })?
        } else {
            registry_builder
        };

        // 12d. Repository-Überblick (`repo.tree`) aus dem verzeichnis-
        //      gebundenen Explorer — nur für Einstiege mit Projektkontext,
        //      sonst würden Host-Pfade durchsickern.
        let registry_builder = if profile.project_context {
            registry_builder
                .context_provider(Arc::new(crate::task_context::RepoTreeContextProvider::new(
                    bound_root.clone(),
                )))
                .map_err(|error| RuntimeError::Registry {
                    detail: format!("could not register the repo tree context provider: {error}"),
                })?
        } else {
            registry_builder
        };

        // 12e. Runde 5, Teil F: Plan-Modus der Wurzel. `plan.write`,
        //      `plan.exit`, `plan.enter`, `ask_user` hängen NUR an dieser
        //      Wurzel-Registry (Kind-Fabriken bekommen sie nie) und prüfen
        //      zusätzlich die hier gebundene Wurzel-Sitzung. Den Fragekanal
        //      gibt es nur für `EntryKind::Tui`; jeder andere Einstieg
        //      antwortet fail-closed. Die `PlanModeGate` sperrt im Plan-Modus
        //      sofort (auch mitten im Turn) alles außerhalb der
        //      Plan-Positivliste; der angeheftete Plan ist ein Kontextbeitrag
        //      je Anfrage und übersteht so jede Verdichtung.
        let plan_session = harw_tool_plan::PlanSession::new(
            harw_tool_plan::PlanDir::new(home_project.plans_dir()),
            spec.mode_override == Some(InteractionMode::Plan),
        );
        plan_session.bind_root(root_session_id.as_str());
        // Runde 5, Teil P: `plan_ui_sender`/`plan_ui_receiver` stammen aus
        // Schritt 11 (vor `Arc::new(services)`).
        // Nur Einstiege mit der vollen Modell-Werkzeugfläche (TUI, One-Shot,
        // Doctor); Jobs, Gateways und reine Befehlsflächen bleiben unverändert.
        let registry_builder = if matches!(
            registry_profile,
            RegistryProfile::NoTools | RegistryProfile::WorkspaceEdit
        ) || profile.operations != OperationSurface::AllWithModelTools
        {
            registry_builder
        } else {
            registry_builder
                .tool_provider(Arc::new(harw_tool_plan::PlanToolProvider::new(
                    plan_session.clone(),
                    plan_ui_sender,
                )))
                .approval_handler(Arc::new(harw_tool_plan::PlanModeGate::new(
                    plan_session.lock().clone(),
                    InteractionMode::Plan
                        .allowed_tools()
                        .unwrap_or_default()
                        .iter()
                        .copied(),
                )))
                .context_provider(Arc::new(harw_tool_plan::PinnedPlanContextProvider::new(
                    plan_session.pinned().clone(),
                )))
                .map_err(|error| RuntimeError::Registry {
                    detail: format!("could not register the pinned plan context provider: {error}"),
                })?
        };

        // Addendum B, „Präzisierung Konsolidierungszeitpunkt": holt beim
        // Aufbau der Wurzelsitzung liegengebliebene `_incoming`-Kandidaten
        // eines abgestürzten vorherigen Laufs nach. Nur für Einstiege, deren
        // Arbeit nicht so kurzlebig/intern ist, dass ein zusätzlicher
        // Hintergrund-Thread reine Verschwendung wäre (siehe
        // `entry_wants_startup_sweep`); harmlos best-effort, kein Fehler
        // dieses Schritts bricht die Montage ab.
        if let Some(capture) = memory_capture.as_ref() {
            if entry_wants_startup_sweep(spec.entry) {
                crate::memory_wiring::spawn_startup_sweep(Arc::clone(capture));
            }
        }

        let registry = registry_builder.build();
        let tools = registered_tool_names(&registry);

        Ok(RuntimeAssembly {
            spec,
            profile,
            config,
            trust_report,
            project,
            home_project_root,
            home_project,
            sandbox,
            ceiling,
            spawn_context,
            budget,
            turn_limits,
            network_scope,
            chain,
            approval_mode,
            allow_rules,
            extra_roots,
            approval_timeout,
            agent_ir,
            root_context_program,
            activation,
            operations,
            services,
            model,
            stores,
            spawner,
            spawner_roles,
            agent_roster,
            agent_events,
            lifecycle_hooks,
            tools,
            root_session_id,
            memory_capture,
            diary_recorder,
            guard_policy,
            role_effort_weights,
            root_uia_reasoning_effort_defaults,
            pitfall_advisor,
            root_model_id,
            offline_echo,
            registry: Mutex::new(Some(registry)),
            responder: Mutex::new(None),
            host_permit_ledger,
            host_permit_registry,
            host_permit_prompt_sender,
            host_permit_prompts: Mutex::new(Some(host_permit_prompt_receiver)),
            sudo_prompts: Mutex::new(sudo_prompt_receiver),
            // Runde 5, Teil F.
            plan_session,
            plan_ui_requests: Mutex::new(plan_ui_receiver),
            // Runde 5, Teil G.
            uia_worker_routing,
            live_models,
            // Plan R9, Teil F.
            session_jobs,
        })
    }
}

/// Senkt die eingebauten Rollen **einmal** je Montage.
///
/// # Beschreibung
/// `existing` ist `config.executable_agents` — die gesenkten lokalen
/// DSL-Definitionen, nach `DefinitionId`. Seit Plan R9 (Teil B) ersetzt keine
/// davon eine eingebaute Rolle: [`builtin_agent_definitions`] meldet eine
/// Kollision nur und behält die eingebaute Rolle. Lokale Agenten werden unter
/// ihrem eigenen Namen startbar (`harw_registry_defaults::AgentRoster`).
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

/// Löst die UIA eines interaktiven Einstiegs auf.
///
/// TUI und One-shot sind Nutzeroberflächen. Ohne explizit gewählten
/// Wurzel-Agenten ([`resolve_explicit_root_agent`]) starten sie
/// ausschließlich mit `harness.active_uia_definition`, deren gesenkte
/// DSL-Rolle `user-interface` sein muss. Die Montage ruft diese Funktion nur
/// auf, wenn kein expliziter Wurzel-Agent gesetzt ist — dann (und nur dann)
/// ist die UIA Pflicht. Andere Einstiege haben keine UIA-Pflicht.
fn resolve_active_uia(
    entry: EntryKind,
    config: &ResolvedConfig,
    builtin: &HashMap<String, ExecutableAgentIr>,
) -> RuntimeResult<Option<ExecutableAgentIr>> {
    if !matches!(entry, EntryKind::Tui | EntryKind::OneShot) {
        return Ok(None);
    }
    let name = config.harness.active_uia_definition.as_deref().ok_or_else(|| RuntimeError::Registry {
        detail: "no active UIA is configured; set harness.active_uia_definition to a user-interface agent definition".to_owned(),
    })?;
    let ir = resolve_active_agent(Some(name), config, builtin)?.ok_or_else(|| {
        RuntimeError::Registry {
            detail: format!("UIA '{name}' did not resolve"),
        }
    })?;
    if ir.role() != AgentRoleId::UserInterface {
        return Err(RuntimeError::Registry {
            detail: format!(
                "configured UIA '{name}' has role {:?}, expected user-interface",
                ir.role()
            ),
        });
    }
    Ok(Some(ir))
}

/// Senkt die eingebettete UIA einer personalisierten harw (#22) und prüft
/// ihre Rolle, wie [`resolve_active_uia`] es für `harness.active_uia_definition`
/// tut.
///
/// # Beschreibung
/// Aufgerufen anstelle von [`resolve_active_uia`] für `Tui`/`OneShot`, wenn
/// `spec.embedded` ([`crate::embedded::EmbeddedAgent`], Welle 3A) gesetzt ist
/// — die native, personalisierte harw trägt ihre UIA bereits als
/// eingebettetes Artefakt (`harw-cli::embedded_uia` reicht sie über
/// [`crate::spec::RuntimeSpec::embedded`] durch); sie gewinnt über eine
/// konfigurierte `harness.active_uia_definition`, muss sich aber denselben
/// Rollen- und Senkungs-Regeln stellen wie diese. `harw-runtime` kennt
/// `harw-cli::embedded_uia` nicht selbst.
///
/// # Errors
/// [`RuntimeError::Registry`], wenn die eingebettete Definition nicht die
/// Rolle `user-interface` trägt (etwa `EmbeddedAgent::root_ir` eines
/// `EntryKind::CompiledAgent`-Artefakts, das keine UIA ist — der Aufrufer
/// gattet deshalb bereits auf `Tui`/`OneShot`, bevor er hierher kommt).
fn resolve_embedded_uia(
    embedded: &harw_agent_dsl::ir_v2::AgentIr,
) -> RuntimeResult<ExecutableAgentIr> {
    let ir = ExecutableAgentIr::from(embedded);
    if ir.role() != AgentRoleId::UserInterface {
        return Err(RuntimeError::Registry {
            detail: format!(
                "embedded UIA '{}' has role {:?}, expected user-interface",
                embedded.id,
                ir.role()
            ),
        });
    }
    Ok(ir)
}

/// Löst einen explizit gewählten Wurzel-Agenten auf und prüft seine Rolle.
///
/// # Beschreibung
/// Runde 3, Welle C1: `spec.active_agent` (`--agent`, bzw. die persistierte
/// Auswahl `harness.active_agent_definition`) gewinnt über die UIA. Als
/// Wurzel zugelassen sind nur die Organisationsrollen
/// [`AgentRoleId::RootOrchestrator`], [`AgentRoleId::ChildOrchestrator`] und
/// [`AgentRoleId::Worker`]. Eine UIA gehört nach
/// `harness.active_uia_definition`, UIA-Helfer und der Agent-Steward sind
/// keine eigenständigen Wurzeln.
///
/// # Argumente
/// - `name` (`Option<&str>`): der explizit gewählte Agent; `None` heißt
///   „keiner“.
/// - `config` / `builtin`: wie [`resolve_active_agent`].
///
/// # Rückgabe
/// `Ok(None)` ohne Namen, sonst `Ok(Some(ir))`.
///
/// # Fehler
/// - [`RuntimeError::Registry`], wenn der Name unbekannt ist (siehe
///   [`resolve_active_agent`]).
/// - [`RuntimeError::Config`], wenn der Agent eine nicht zugelassene Rolle
///   trägt.
fn resolve_explicit_root_agent(
    name: Option<&str>,
    config: &ResolvedConfig,
    builtin: &HashMap<String, ExecutableAgentIr>,
) -> RuntimeResult<Option<ExecutableAgentIr>> {
    let Some(ir) = resolve_active_agent(name, config, builtin)? else {
        return Ok(None);
    };
    match ir.role() {
        AgentRoleId::RootOrchestrator | AgentRoleId::ChildOrchestrator | AgentRoleId::Worker => {
            Ok(Some(ir))
        }
        other => Err(RuntimeError::Config {
            detail: format!(
                "Der Agent „{}“ hat die Rolle {other:?} und kann nicht als Wurzel starten; \
                 zulässig sind nur Root-Orchestrator, Child-Orchestrator und Worker.",
                name.unwrap_or_default()
            ),
        }),
    }
}

/// Löst Provider-/Modell-/Agenten-Reasoning-Effort-Vorgaben der Wurzel-UIA
/// auf (Welle 8: Rangfolge Provider > Modell > Agent > Rolle).
///
/// # Description
/// Spiegelt [`crate::model::resolve_uia_model`] (dort privat, deshalb hier
/// dupliziert statt importiert): `uia_provider`/`uia_model`, falls beide
/// gesetzt sind und `uia_provider` einen vorhandenen, aktivierten Provider
/// bezeichnet, sonst `default_provider`/`default_model`. Aus der so
/// bestimmten Provider-/Modell-ID werden anschließend
/// `ProviderToml::default_reasoning_effort`/`ModelToml::default_reasoning_effort`
/// gelesen (`None` bei fehlendem Eintrag oder fehlender ID). Das dritte
/// Ergebnisfeld ist [`ExecutableAgentIr::reasoning_effort`] der übergebenen
/// UIA-Definition, als eigenständiger `String` geklont, damit
/// [`RuntimeAssembly`] ihn unabhängig von der Lebensdauer der IR selbst
/// halten kann.
///
/// # Arguments
/// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration des Laufs.
/// - `uia_ir` (`Option<&ExecutableAgentIr>`): die über
///   [`resolve_active_uia`] aufgelöste UIA-Definition, falls dieser Einstieg
///   eine hat.
///
/// # Returns
/// `(provider_default, model_default, agent_default)`, zum Aufruf von
/// [`crate::guard_wiring::resolve_default_reasoning_effort`] gedacht;
/// `(None, None, None)`, solange `uia_ir` `None` ist.
fn resolve_root_uia_reasoning_effort_defaults(
    config: &ResolvedConfig,
    uia_ir: Option<&ExecutableAgentIr>,
) -> (
    Option<harw_types::ReasoningEffort>,
    Option<harw_types::ReasoningEffort>,
    Option<String>,
) {
    let Some(uia_ir) = uia_ir else {
        return (None, None, None);
    };
    let uia_provider_usable = config
        .harness
        .uia_provider
        .as_deref()
        .is_some_and(|provider_id| {
            config
                .providers
                .get(provider_id)
                .is_some_and(|provider| provider.enabled)
        });
    let (provider_id, model_id) = if uia_provider_usable && config.harness.uia_model.is_some() {
        (
            config.harness.uia_provider.as_deref(),
            config.harness.uia_model.as_deref(),
        )
    } else {
        (
            config.harness.default_provider.as_deref(),
            config.harness.default_model.as_deref(),
        )
    };
    let provider_default = provider_id
        .and_then(|id| config.providers.get(id))
        .and_then(|provider| provider.default_reasoning_effort);
    let model_default = model_id
        .and_then(|id| config.models.get(id))
        .and_then(|model| model.default_reasoning_effort);
    let agent_default = uia_ir.reasoning_effort().map(str::to_owned);
    (provider_default, model_default, agent_default)
}

/// Baut das Root-Modell und das (ggf. abweichende) Modell der
/// `uia-worker`-Rollenfamilie eines Laufs (Welle 3a, Teil A).
///
/// # Description
/// Reiner, direkt getesteter Baustein von [`RuntimeAssemblyBuilder::build`]
/// (Schritt 9): ohne aktive UIA (`uia_ir` ist `None`) bleibt alles
/// bit-identisch zum bisherigen Verhalten — beide Rückgabewerte sind
/// `Arc::clone(default_tree_model)`. Mit aktiver UIA baut diese Funktion
/// zuerst über [`crate::model::build_uia_model_with_resolver`] das
/// eigenständige Modell der UIA-Sitzung selbst (`uia_client`, verwendet den
/// `default_tree_model` unverändert, solange `source_is_configured` `false`
/// ist oder kein abweichender `uia_provider`/`uia_model` konfiguriert ist),
/// und leitet daraus über [`crate::model::build_uia_worker_model`] das
/// Modell der gesamten `uia-worker`-Rollenfamilie ab (`uia-worker`,
/// `uia-explorer`, `uia-writer`, `uia-shell-worker`, `uia-latex-writer`) —
/// dieselbe Ableitung, die `build_spawner`s `uia_worker_factory` (Teil A,
/// Schritt 4) anschließend registriert.
///
/// # Arguments
/// - `spec` / `config` / `resolver`: wie
///   [`crate::model::build_uia_model_with_resolver`].
/// - `source_is_configured` (`bool`): vom Aufrufer **vor** dem Verbrauch des
///   [`ModelSource`] ermittelt (`matches!(&model_source, ModelSource::Configured)`).
/// - `default_tree_model` (`&Arc<dyn ModelProvider>`): das bereits gebaute
///   Vorgabe-Modell des Laufs; Rückgabewert für beide Positionen, solange
///   `uia_ir` `None` ist.
/// - `uia_ir` (`Option<&ExecutableAgentIr>`): das Ergebnis von
///   [`resolve_active_uia`]; nur `Some`/`None` ist relevant, der Inhalt der
///   IR selbst wird hier nicht gelesen.
///
/// # Returns
/// `(model, uia_worker_model, uia_load_registry)` — `model` ist der Wert, der
/// den Root-Turn dieses Laufs treibt (bei aktiver UIA der UIA-Client, sonst
/// unverändert `default_tree_model`); `uia_worker_model` ist das Modell, das
/// `build_spawner` an die `uia_worker_factory` reicht; `uia_load_registry`
/// ist die [`ProviderLoadRegistry`] des eigenständig gebauten UIA-Clients
/// (leer ohne aktive UIA oder solange die UIA `default_tree_model`
/// weiterverwendet — siehe
/// [`crate::model::build_uia_model_with_registry_and_resolver`]).
///
/// # Errors
/// Wie [`crate::model::build_uia_model_with_resolver`]:
/// [`RuntimeError::Provider`], wenn der konfigurierte UIA-Provider nicht
/// gebaut werden kann.
/// Rückgabe von [`split_root_and_uia_worker_models`]: `(model,
/// uia_worker_model, uia_load_registry)`, siehe dortige `# Returns`-Sektion.
type SplitRootAndUiaWorkerModels = (
    Arc<dyn ModelProvider>,
    Arc<dyn ModelProvider>,
    ProviderLoadRegistry,
);

fn split_root_and_uia_worker_models(
    spec: &RuntimeSpec,
    config: &ResolvedConfig,
    source_is_configured: bool,
    default_tree_model: &Arc<dyn ModelProvider>,
    uia_ir: Option<&ExecutableAgentIr>,
    resolver: Option<&dyn SecretResolver>,
) -> RuntimeResult<SplitRootAndUiaWorkerModels> {
    if uia_ir.is_none() {
        return Ok((
            Arc::clone(default_tree_model),
            Arc::clone(default_tree_model),
            ProviderLoadRegistry::new(),
        ));
    }
    let (uia_client, uia_load_registry) = crate::model::build_uia_model_with_registry_and_resolver(
        spec,
        config,
        source_is_configured,
        default_tree_model,
        resolver,
    )?;
    let uia_worker_model = crate::model::build_uia_worker_model(config, &uia_client);
    Ok((uia_client, uia_worker_model, uia_load_registry))
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
/// Aus demselben `ToolExecutionContext` übernimmt die Fabrik auch den
/// `CancelToken` des Turns (`ToolExecutionContext::cancel`) in den gebauten
/// [`OpContext`] (`OpContext::with_cancel_token`) — ist keiner gesetzt
/// (z. B. in einem Test-Fixture ohne `TurnControl`), bleibt
/// `OpContext::cancel_token` `None`.
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
                let ctx = OpContext::new(
                    execution_context.session_id().clone(),
                    execution_context.turn_id().clone(),
                    execution_context.sandbox().clone(),
                    services.service_map(ServiceSurface::ModelTool),
                );
                // Der Turn-Loop hängt seinen `CancelToken` an jeden
                // `ToolExecutionContext` (`control.cancel_token()`, siehe
                // `harw-core::turn_loop`); ohne diesen Schritt bekäme jede
                // Operation — u. a. `AgentToolAdapter::invoke` und
                // `fanout_children` in `harw-core-bridge` — nie den echten
                // Turn-Abbruch zu sehen und müsste immer auf einen frischen,
                // nie abgebrochenen Token zurückfallen.
                match execution_context.cancel() {
                    Some(cancel) => ctx.with_cancel_token(cancel.clone()),
                    None => ctx,
                }
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

/// Ob ein Einstieg den Gedächtnis-Startup-Sweep bekommt (Addendum B,
/// „Präzisierung Konsolidierungszeitpunkt").
///
/// # Beschreibung
/// [`crate::memory_wiring::spawn_startup_sweep`] öffnet einen eigenen
/// Hintergrund-Thread; das lohnt sich für Einstiege, deren Lauf lange genug
/// lebt, dass ein liegengebliebener `_incoming`-Kandidat eines vorherigen
/// Absturzes während dieses Laufs überhaupt sichtbar würde
/// (`Tui`/`OneShot`, die beiden Gateways, `Web`). Kurzlebige, oft
/// wiederholte oder rein interne Einstiege (`Doctor`, `LocalEcho`,
/// `McpServe`, die beiden Job-Varianten) bekämen bei jedem Aufruf einen
/// zusätzlichen Thread, ohne dass ein Mensch je den Nachholeffekt sähe —
/// die Montage überspringt den Sweep dort. Der Sweep selbst bliebe in jedem
/// Fall harmlos (best-effort, siehe seine eigene Doku); diese Funktion ist
/// reine Sparsamkeit, keine Korrektheitsanforderung.
///
/// # Argumente
/// - `entry` ([`EntryKind`]): der Einstieg des Laufs.
///
/// # Rückgabe
/// `true` für `Tui`, `OneShot`, `Web`, `GatewayTelegram`, `GatewayDream`;
/// sonst `false`.
#[must_use]
const fn entry_wants_startup_sweep(entry: EntryKind) -> bool {
    matches!(
        entry,
        EntryKind::Tui
            | EntryKind::OneShot
            | EntryKind::Web
            | EntryKind::GatewayTelegram
            | EntryKind::GatewayDream
    )
}

/// Höchstzahl Präferenz-/Pitfall-Fakten, die [`MemoryFactsContextProvider`]
/// insgesamt (Projekt + Global) ausliefert (Addendum B: „immer laden …, max.
/// 10").
const MEMORY_FACTS_MAX_DELIVERED: usize = 10;

/// Höchstzahl Dateiwissen-Einträge, die [`MemoryFactsContextProvider`] im
/// Abschnitt „Bekannte Dateien" ausliefert (Addendum B: „12").
const MEMORY_FILES_MAX_DELIVERED: usize = 12;

/// Gesamtobergrenze der Faktenzeilen (Präferenzen/Fallen **plus**
/// Stichwort-Treffer), die [`MemoryFactsContextProvider`] insgesamt
/// ausliefert (Ticket „Recall-Stichwörter", Pending-Integration-Punkt 4:
/// „total fact lines ≤ 15"). [`MEMORY_FACTS_MAX_DELIVERED`] bleibt die
/// Obergrenze für den unveränderten Präferenzen/Fallen-Anteil; der Rest bis
/// hierhin steht Stichwort-Treffern zur Verfügung.
const MEMORY_FACTS_TOTAL_MAX_DELIVERED: usize = 15;

/// Mindestwortlänge für aus dem Turn-Eingang abgeleitete Suchstichwörter.
const MEMORY_KEYWORD_MIN_CHARS: usize = 4;

/// Höchstzahl Stichwörter, die [`derive_memory_search_keywords`] aus dem
/// letzten Nutzertext ableitet (wie `harw_memory::context_provider`s
/// `MAX_FACT_SEARCH_KEYWORDS`).
const MEMORY_KEYWORDS_MAX: usize = 12;

/// Kleine, undogmatische deutsch/englische Stopwortliste für
/// [`derive_memory_search_keywords`].
///
/// # Beschreibung
/// `harw_memory::context_provider::MemoryContextProvider::search_keywords`
/// ist privat und kennt selbst **keine** Stopwortliste (nur Wortlänge ≥ 3);
/// dieser Adapter repliziert deshalb eine eigene, bewusst kleine Liste statt
/// die private Funktion zu duplizieren oder sie öffentlich zu machen (außerhalb
/// dieses Vertrags).
const MEMORY_KEYWORD_STOPWORDS: &[&str] = &[
    "dass", "eine", "einen", "einem", "einer", "eines", "sich", "sind", "wird", "werden", "wurde",
    "wurden", "haben", "hatte", "hatten", "kann", "könnte", "muss", "müssen", "auch", "aber",
    "oder", "nicht", "noch", "schon", "wenn", "dann", "diese", "dieser", "dieses", "dabei",
    "damit", "durch", "über", "unter", "immer", "mehr", "sehr", "nach", "vor", "bei", "bitte",
    "danke", "bereits", "dafür", "davon", "diesem", "diesen", "that", "this", "these", "those",
    "with", "from", "have", "has", "had", "will", "would", "could", "should", "please", "about",
    "what", "when", "where", "which", "your", "the", "and", "for", "are", "was", "were", "been",
    "being", "into", "onto", "than", "then", "there", "their", "them", "they", "some", "such",
    "just", "like", "want", "need", "make", "does", "doing", "done", "here", "also", "only",
    "very",
];

/// Liest den jüngsten Nutzertext aus `ctx.metadata`, falls vorhanden.
///
/// # Beschreibung
/// `harw_extension_api::TurnInputContext` trägt selbst keinen eigenen
/// Freitext-Nutzertext, nur `session_id`, `turn_id` und `metadata`
/// (`serde_json::Value`) — siehe `harw_memory::context_provider.rs`, das
/// dieselbe Lücke dokumentiert und stattdessen sein eigenes STM liest,
/// worauf dieser Adapter keinen Zugriff hat. `TurnInput::metadata` ist am
/// einzigen produktiven Aufrufort (`harw-core/src/turn_loop.rs`) heute immer
/// `Null` — diese Funktion liest deshalb best-effort einen von mehreren
/// gebräuchlichen Feldnamen (oder `metadata` selbst als String), damit ein
/// künftiger Aufrufer, der Text mitgibt, ohne Änderung an dieser Stelle
/// greift; heute liefert sie strukturbedingt `None` und die Aufrufer fallen
/// auf den bisherigen Pfad ohne Stichwortsuche zurück (keine Regression).
fn latest_user_text_from_metadata(ctx: &TurnInputContext) -> Option<String> {
    match &ctx.metadata {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Object(map) => {
            const CANDIDATE_KEYS: &[&str] = &[
                "user_text",
                "latest_user_message",
                "text",
                "message",
                "input",
            ];
            CANDIDATE_KEYS.iter().find_map(|key| match map.get(*key) {
                Some(serde_json::Value::String(text)) => Some(text.clone()),
                _ => None,
            })
        }
        _ => None,
    }
}

/// Leitet Suchstichwörter aus dem Turn-Eingang ab (Pending-Integration-Punkt
/// 4).
///
/// # Beschreibung
/// Zerlegt den über [`latest_user_text_from_metadata`] gefundenen Text an
/// nicht-alphanumerischen Zeichen, senkt auf Kleinschreibung, verwirft Wörter
/// unter [`MEMORY_KEYWORD_MIN_CHARS`] Zeichen sowie [`MEMORY_KEYWORD_STOPWORDS`]
/// und dedupliziert, bis höchstens [`MEMORY_KEYWORDS_MAX`] Stichwörter übrig
/// sind — dieselbe Grundform wie `harw_memory::context_provider`s
/// `search_keywords` (dort: Länge ≥ 3, keine Stopwortliste, keine
/// Deduplizierung), hier bewusst etwas strenger für eine gezieltere Suche.
///
/// # Returns
/// Eine leere Liste, wenn kein Nutzertext verfügbar ist oder keine
/// hinreichend langen Wörter übrig bleiben — die Aufrufer fallen dann auf
/// ihre bisherigen Recency-Pfade zurück.
fn derive_memory_search_keywords(ctx: &TurnInputContext) -> Vec<String> {
    let Some(text) = latest_user_text_from_metadata(ctx) else {
        return Vec::new();
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.chars().count() >= MEMORY_KEYWORD_MIN_CHARS)
        .filter(|word| !MEMORY_KEYWORD_STOPWORDS.contains(&word.as_str()))
        .filter(|word| seen.insert(word.clone()))
        .take(MEMORY_KEYWORDS_MAX)
        .collect()
}

/// Facts-only-Gedächtnis-Recall für die Wurzel-Registry (Addendum B, §2/§4).
///
/// # Beschreibung
/// `harw_memory::context_provider::MemoryContextProvider<M>` verlangt
/// `M: Memory` **ohne** `?Sized` — der implizite `Sized`-Bound jedes
/// Typarameters gilt dort unverändert (siehe `harw-memory/src/context_provider.rs`,
/// `pub struct MemoryContextProvider<M: Memory> { store: Arc<M>, .. }`).
/// [`RuntimeAssemblyBuilder::memory`] hält aber nur `Arc<dyn Memory>` — ein
/// unsized Trait-Objekt, für das `MemoryContextProvider<dyn Memory>` deshalb
/// **nicht** instanziierbar ist (`the trait Sized is not implemented for
/// dyn Memory`). Diese Inkompatibilität besteht unabhängig davon, ob
/// überhaupt ein `Arc<dyn Memory>` übergeben wurde — sie ist strukturell.
///
/// Dieser Typ ist der nächstliegende gangbare Pfad (Addendum B, Vertrag für
/// diesen Knoten: „sonst BLOCKED-Detail und Fakten-only-Recall auf dem
/// nächstliegenden gangbaren Pfad implementieren"): er umgeht
/// `MemoryContextProvider<M>` vollständig und liefert eigenständig zwei
/// Abschnitte aus den bereits geöffneten [`harw_memory::FactStore`]s und dem
/// [`harw_memory::file_index::FileKnowledgeIndex`] — beide sind
/// M-generic-frei (`FactStore`/`FileKnowledgeIndex` sind konkrete Typen,
/// kein Trait-Objekt-Problem):
///
/// 1. „Präferenzen & Fallen" — alle [`harw_memory::FactType::Preference`]-
///    und [`harw_memory::FactType::Pitfall`]-Fakten, Projekt vor Global, bis
///    [`MEMORY_FACTS_MAX_DELIVERED`] insgesamt.
/// 2. „Bekannte Dateien" — bis zu [`MEMORY_FILES_MAX_DELIVERED`]
///    Dateiwissen-Einträge, nach `last_seen` absteigend sortiert (ohne
///    Stichwortsuche: dieser Adapter hat keinen Zugriff auf den STM-Puffer,
///    den `MemoryContextProvider::search_keywords` dafür liest — der lebt
///    ausschließlich innerhalb des M-generischen Providers).
///
/// HOT/STM/WARM (der eigentliche v2-`Memory`-Store) bleibt damit außerhalb
/// des Modellkontexts dieses Laufs; das ist keine Verschlechterung
/// gegenüber dem Zustand vor diesem Knoten — vor ihm erreichte **gar kein**
/// Gedächtnis-Fragment die Registry (siehe Bericht: `MemoryContextProvider`
/// wurde nirgends instanziiert).
///
/// # Nebenläufigkeit
/// `Send + Sync`: hält nur `Arc`s (`FactStore`, `FileKnowledgeIndex`), keine
/// eigene innere Veränderlichkeit.
///
/// # Fehler
/// Kein eigener Fehlertyp: ein Lesefehler eines Stores wird nur
/// `tracing::warn!`, der betroffene Abschnitt liefert dann schlicht nichts —
/// derselbe Grundsatz wie `harw_memory::context_provider::MemoryContextProvider`.
struct MemoryFactsContextProvider {
    /// Projekt-Fakten-Wurzel; Projekt geht laut Design §4 im Lesepfad vor.
    project_facts: Option<Arc<harw_memory::FactStore>>,
    /// Globale Fakten-Wurzel.
    global_facts: Option<Arc<harw_memory::FactStore>>,
    /// Dateiwissen-Index der Projekt-Wurzel, falls er geöffnet werden konnte.
    file_index: Option<Arc<harw_memory::file_index::FileKnowledgeIndex>>,
}

impl MemoryFactsContextProvider {
    /// Baut den Provider aus bereits geöffneten Quellen.
    #[must_use]
    fn new(
        project_facts: Option<Arc<harw_memory::FactStore>>,
        global_facts: Option<Arc<harw_memory::FactStore>>,
        file_index: Option<Arc<harw_memory::file_index::FileKnowledgeIndex>>,
    ) -> Self {
        Self {
            project_facts,
            global_facts,
            file_index,
        }
    }

    /// Rendert den Abschnitt „Präferenzen & Fallen" (Addendum B).
    ///
    /// # Beschreibung
    /// Liest [`harw_memory::FactType::Preference`]- und
    /// [`harw_memory::FactType::Pitfall`]-Fakten aus Projekt- (zuerst) und
    /// Global-Store, bis [`MEMORY_FACTS_MAX_DELIVERED`] insgesamt erreicht
    /// sind. Ein Lesefehler eines Stores wird nur geloggt; der andere Store
    /// trägt trotzdem weiter bei.
    ///
    /// # Arguments
    /// - `keywords` (`&[String]`): über [`derive_memory_search_keywords`]
    ///   aus dem Turn-Eingang abgeleitete Stichwörter; leer, wenn keiner
    ///   verfügbar war. Zusätzlich zu den unverändert immer geladenen
    ///   Präferenz-/Fallen-Fakten liefert ein nicht-leeres `keywords` weitere,
    ///   über [`harw_memory::FactStore::search`] gefundene Treffer beliebigen
    ///   Fakttyps, dedupliziert gegen bereits ausgelieferte Fakten, bis
    ///   insgesamt [`MEMORY_FACTS_TOTAL_MAX_DELIVERED`] Zeilen erreicht sind.
    fn preferences_and_pitfalls(&self, keywords: &[String]) -> Vec<ContextFragment> {
        let mut lines: Vec<String> = Vec::new();
        let mut seen_facts: std::collections::HashSet<String> = std::collections::HashSet::new();
        let stores = [
            ("project", &self.project_facts),
            ("global", &self.global_facts),
        ];
        for (scope_name, store) in stores {
            let Some(store) = store else { continue };
            if lines.len() >= MEMORY_FACTS_MAX_DELIVERED {
                break;
            }
            match store.list() {
                Ok(facts) => {
                    for fact in facts.into_iter().filter(|fact| {
                        matches!(
                            fact.fact_type,
                            harw_memory::FactType::Preference | harw_memory::FactType::Pitfall
                        )
                    }) {
                        if lines.len() >= MEMORY_FACTS_MAX_DELIVERED {
                            break;
                        }
                        seen_facts.insert(format!("{scope_name}:{}", fact.name));
                        lines.push(format!(
                            "- ({scope_name}, {}) {}",
                            fact.fact_type, fact.description
                        ));
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        scope = scope_name,
                        error = %error,
                        "runtime.memory_facts.list_failed"
                    );
                }
            }
        }
        if !keywords.is_empty() {
            let keyword_refs: Vec<&str> = keywords.iter().map(String::as_str).collect();
            for (scope_name, store) in stores {
                let Some(store) = store else { continue };
                if lines.len() >= MEMORY_FACTS_TOTAL_MAX_DELIVERED {
                    break;
                }
                let remaining = MEMORY_FACTS_TOTAL_MAX_DELIVERED - lines.len();
                match store.search(&keyword_refs, remaining) {
                    Ok(facts) => {
                        for fact in facts {
                            if lines.len() >= MEMORY_FACTS_TOTAL_MAX_DELIVERED {
                                break;
                            }
                            if !seen_facts.insert(format!("{scope_name}:{}", fact.name)) {
                                continue;
                            }
                            lines.push(format!(
                                "- ({scope_name}, {} · Stichwort) {}",
                                fact.fact_type, fact.description
                            ));
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            scope = scope_name,
                            error = %error,
                            "runtime.memory_facts.search_failed"
                        );
                    }
                }
            }
        }
        if lines.is_empty() {
            return Vec::new();
        }
        vec![ContextFragment {
            label: "memory.facts.preferences".to_owned(),
            content: format!("Präferenzen & bekannte Fallen:\n{}", lines.join("\n")),
        }]
    }

    /// Rendert den Abschnitt „Bekannte Dateien" (Addendum B).
    ///
    /// # Beschreibung
    /// Ist `keywords` nicht leer, liefert
    /// [`harw_memory::file_index::FileKnowledgeIndex::search`] bis zu
    /// [`MEMORY_FILES_MAX_DELIVERED`] stichwort-passende Einträge. Sonst (kein
    /// Stichwort aus dem Turn-Eingang ableitbar) fällt diese Funktion auf die
    /// bisherige Recency-Liste zurück: bis zu [`MEMORY_FILES_MAX_DELIVERED`]
    /// Einträge des Dateiwissen-Index, nach `last_seen` absteigend sortiert
    /// (jüngstes zuerst). Ein Lesefehler des Index wird nur geloggt.
    fn known_files(&self, keywords: &[String]) -> Vec<ContextFragment> {
        let Some(index) = self.file_index.as_ref() else {
            return Vec::new();
        };
        let entries = if keywords.is_empty() {
            let mut entries = match index.list() {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(error = %error, "runtime.memory_files.list_failed");
                    return Vec::new();
                }
            };
            entries.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
            entries.truncate(MEMORY_FILES_MAX_DELIVERED);
            entries
        } else {
            match index.search(keywords, MEMORY_FILES_MAX_DELIVERED) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(error = %error, "runtime.memory_files.search_failed");
                    return Vec::new();
                }
            }
        };
        if entries.is_empty() {
            return Vec::new();
        }

        let lines: Vec<String> = entries
            .iter()
            .map(|entry| {
                let summary = entry.summary.as_deref().unwrap_or("");
                if entry.symbols.is_empty() {
                    format!("- {} — {summary}", entry.path)
                } else {
                    format!(
                        "- {} — {summary} [{}]",
                        entry.path,
                        entry.symbols.join(", ")
                    )
                }
            })
            .collect();
        vec![ContextFragment {
            label: "memory.files.known".to_owned(),
            content: format!("Bekannte Dateien (bereits gelesen):\n{}", lines.join("\n")),
        }]
    }
}

impl ContextProvider for MemoryFactsContextProvider {
    /// Liefert die beiden Fakten-/Dateiwissen-Abschnitte dieses Laufs.
    ///
    /// # Beschreibung
    /// Synchron gebaut (keine `.await`-Stelle nötig — beide Quellen sind
    /// dateibasiert und werden ohne eigenen Async-Layer gelesen). `ctx` wird
    /// über [`derive_memory_search_keywords`] gelesen: dieser Adapter hat —
    /// anders als `harw_memory::context_provider::MemoryContextProvider` —
    /// kein STM, aus dem er sonst eine Stichwortsuche ableiten könnte, liest
    /// deshalb `ctx.metadata` best-effort (siehe dort für die strukturelle
    /// Einschränkung); ohne Treffer fallen beide Abschnitte unverändert auf
    /// ihre bisherigen Recency-Pfade zurück.
    fn contribute<'a>(&'a self, ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>> {
        Box::pin(async move {
            let keywords = derive_memory_search_keywords(ctx);
            let mut fragments = self.preferences_and_pitfalls(&keywords);
            fragments.extend(self.known_files(&keywords));
            fragments
        })
    }
}

/// Die Kennung des Modells, das die Wurzelsitzung dieses Laufs tatsächlich
/// treibt.
///
/// # Beschreibung
/// Spiegelt [`crate::model::resolve_uia_model`] (dort privat): ist die Wurzel
/// eine UIA (`uia_root`) und sind `uia_provider`/`uia_model` beide gesetzt,
/// mit einem vorhandenen, aktivierten `uia_provider`, dann ist `uia_model`
/// das effektive Modell; sonst `default_model`. Dieselbe Rangfolge nutzt
/// [`resolve_root_uia_reasoning_effort_defaults`].
///
/// # Argumente
/// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration dieses Laufs.
/// - `uia_root` (`bool`): ob die Wurzel dieses Laufs eine UIA ist.
///
/// # Rückgabe
/// Die Modellkennung oder `None`, wenn weder UIA- noch Vorgabemodell gesetzt
/// ist.
fn effective_root_model_id(config: &ResolvedConfig, uia_root: bool) -> Option<String> {
    if uia_root {
        let uia_provider_usable =
            config
                .harness
                .uia_provider
                .as_deref()
                .is_some_and(|provider_id| {
                    config
                        .providers
                        .get(provider_id)
                        .is_some_and(|provider| provider.enabled)
                });
        if uia_provider_usable {
            if let Some(model) = config.harness.uia_model.as_deref() {
                return Some(model.to_owned());
            }
        }
    }
    config.harness.default_model.clone()
}

/// Der Provider des Modells, das die Wurzelsitzung dieses Laufs treibt —
/// Gegenstück zu [`effective_root_model_id`] (dieselbe UIA-Rangfolge).
///
/// # Rückgabe
/// `uia_provider` bei nutzbarer UIA-Auswahl einer UIA-Wurzel, sonst
/// `default_provider`.
fn effective_root_provider_id(config: &ResolvedConfig, uia_root: bool) -> Option<String> {
    if uia_root
        && config.harness.uia_model.is_some()
        && let Some(provider) = config.harness.uia_provider.as_deref()
        && config
            .providers
            .get(provider)
            .is_some_and(|provider| provider.enabled)
    {
        return Some(provider.to_owned());
    }
    config.harness.default_provider.clone()
}

/// Die für das Kontext-/Ausgabebudget relevanten Grenzen eines Modells.
///
/// Ergebnis von [`model_limits_for`]; alle Token-Angaben in Tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelLimits {
    /// Effektives Kontextfenster (nie 0; unbekannt →
    /// [`UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS`], beim Offline-Echo
    /// [`OFFLINE_ECHO_CONTEXT_WINDOW_TOKENS`]).
    pub context_window: u64,
    /// `[models.<id>].max_tokens`, falls konfiguriert.
    pub configured_max_output: Option<u64>,
    /// Maximale Ausgabe laut eingebautem Katalog, falls deklariert.
    pub catalog_max_output: Option<u64>,
    /// Ob das Modell einen Reasoning-/Thinking-Modus deklariert
    /// (`[models.<id>].reasoning` oder Katalog-`ReasoningSupport` ≠ `None`).
    pub thinking: bool,
    /// `false`, wenn weder Konfiguration noch Katalog ein Fenster nennen und
    /// der Rückfallwert greift.
    pub known: bool,
}

impl ModelLimits {
    /// Die Ausgabereserve dieses Modells nach
    /// [`harw_core::context_budget::output_reserve_tokens`].
    #[must_use]
    pub fn output_reserve_tokens(&self) -> u64 {
        harw_core::context_budget::output_reserve_tokens(
            self.context_window,
            self.configured_max_output,
            self.catalog_max_output,
            self.thinking,
        )
    }
}

/// Katalogwerte eines eingebauten Modells (aus `harw-model-catalog`).
#[derive(Clone, Copy, Debug)]
struct CatalogLimits {
    context_window: u64,
    max_output: Option<u64>,
    thinking: bool,
}

/// Der einmal je Prozess aufgebaute Katalog, Schlüssel = Katalog-Modell-ID.
fn builtin_catalog() -> &'static HashMap<String, CatalogLimits> {
    static CATALOG: std::sync::OnceLock<HashMap<String, CatalogLimits>> =
        std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        harw_model_catalog::descriptor::bootstrap_descriptors()
            .into_iter()
            .map(|d| {
                (
                    d.model.as_str().to_owned(),
                    CatalogLimits {
                        context_window: u64::from(d.context_window),
                        max_output: d.max_output_tokens.map(u64::from),
                        thinking: !matches!(
                            d.capabilities.reasoning,
                            harw_model_catalog::descriptor::ReasoningSupport::None
                        ),
                    },
                )
            })
            .collect()
    })
}

/// Normalisiert eine Modellkennung für den Katalogvergleich.
///
/// # Beschreibung
/// Kleinschreibung; ein Provider-Präfix bis zum letzten `/` entfällt
/// (`anthropic/claude-x` → `claude-x`); danach wiederholt ein Datums-/
/// Versionssuffix: `-latest`, `@YYYYMMDD`, `-YYYYMMDD`, `-YYYY-MM-DD` und ein
/// vierstelliges `-YYMM` (z. B. `mistral-large-2411`).
fn normalize_model_id(model_id: &str) -> String {
    let lowered = model_id.trim().to_ascii_lowercase();
    let mut id = lowered
        .rsplit_once('/')
        .map_or(lowered.as_str(), |(_, tail)| tail)
        .to_owned();
    loop {
        let before = id.len();
        if let Some(stripped) = id.strip_suffix("-latest") {
            id = stripped.to_owned();
        }
        // `@YYYYMMDD` (Vertex-Schreibweise) bzw. jede `@`-Version.
        if let Some((head, _)) = id.rsplit_once('@') {
            if !head.is_empty() {
                id = head.to_owned();
            }
        }
        // `-YYYY-MM-DD`: nur ASCII-Bytes geprüft, `truncate` trifft deshalb
        // immer eine Zeichengrenze.
        let bytes = id.as_bytes();
        let len = bytes.len();
        if len > 11 {
            let is_iso_date =
                bytes[len - 11..]
                    .iter()
                    .enumerate()
                    .all(|(index, byte)| match index {
                        0 | 5 | 8 => *byte == b'-',
                        _ => byte.is_ascii_digit(),
                    });
            if is_iso_date {
                id.truncate(len - 11);
            }
        }
        // `-YYYYMMDD` bzw. `-YYMM`
        let strip_numeric = id.rsplit_once('-').is_some_and(|(head, tail)| {
            !head.is_empty()
                && (tail.len() == 8 || tail.len() == 4)
                && tail.bytes().all(|byte| byte.is_ascii_digit())
        });
        if strip_numeric {
            if let Some((head, _)) = id.rsplit_once('-') {
                id = head.to_owned();
            }
        }
        if id.len() == before {
            return id;
        }
    }
}

/// Findet den passenden Katalogschlüssel zu einer Modellkennung.
///
/// # Beschreibung
/// Reine Funktion (direkt getestet). Rangfolge:
/// 1. exakter Treffer;
/// 2. Treffer nach [`normalize_model_id`] auf beiden Seiten (Provider-Präfix,
///    Datums-/`-latest`-Suffix, Groß-/Kleinschreibung);
/// 3. Treffer, wenn beide Seiten nach [`version_dots_to_dashes`] gleich sind
///    (Teil C: OpenRouter-Schreibweise `anthropic/claude-sonnet-4.5` →
///    `claude-sonnet-4-5-20250929`);
/// 4. längster Katalogschlüssel, dessen normalisierte Form ein Präfix der
///    normalisierten Kennung ist und an einer Trennstelle (`-`, `.`, `:`,
///    `@`, `_`, `[`) endet (`gpt-4o-mini-high` → `gpt-4o-mini`); verglichen
///    wird dabei ebenfalls in der Form mit `-` statt Versionspunkt.
///
/// Bei gleich langen Präfix-Treffern gewinnt der lexikografisch kleinste
/// Schlüssel, damit das Ergebnis nicht von der `HashMap`-Reihenfolge abhängt.
///
/// # Rückgabe
/// Der Katalogschlüssel oder `None`.
fn match_catalog_key<'a, I>(keys: I, model_id: &str) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let keys: Vec<&'a str> = keys.into_iter().collect();
    if let Some(exact) = keys.iter().copied().find(|key| *key == model_id) {
        return Some(exact);
    }
    let wanted = normalize_model_id(model_id);
    if wanted.is_empty() {
        return None;
    }
    let wanted_dashed = version_dots_to_dashes(&wanted);
    let mut normalized_hit: Option<&'a str> = None;
    let mut dashed_hit: Option<&'a str> = None;
    let mut prefix_hit: Option<(usize, &'a str)> = None;
    for key in keys {
        let normalized = normalize_model_id(key);
        if normalized.is_empty() {
            continue;
        }
        if normalized == wanted {
            if normalized_hit.is_none_or(|current| key < current) {
                normalized_hit = Some(key);
            }
            continue;
        }
        let dashed = version_dots_to_dashes(&normalized);
        if dashed == wanted_dashed {
            if dashed_hit.is_none_or(|current| key < current) {
                dashed_hit = Some(key);
            }
            continue;
        }
        let at_boundary = wanted_dashed
            .strip_prefix(dashed.as_str())
            .is_some_and(|rest| rest.starts_with(['-', '.', ':', '@', '_', '[']));
        if at_boundary {
            let better = match prefix_hit {
                None => true,
                Some((len, current)) => {
                    normalized.len() > len || (normalized.len() == len && key < current)
                }
            };
            if better {
                prefix_hit = Some((normalized.len(), key));
            }
        }
    }
    normalized_hit
        .or(dashed_hit)
        .or(prefix_hit.map(|(_, key)| key))
}

/// Ersetzt jeden Punkt zwischen zwei Ziffern durch `-` (`claude-sonnet-4.5`
/// → `claude-sonnet-4-5`, `gpt-5.4` → `gpt-5-4`). Anbieter schreiben
/// Versionsnummern mal mit Punkt, mal mit Bindestrich; der Katalogabgleich
/// ([`match_catalog_key`]) vergleicht deshalb zusätzlich in dieser Form.
fn version_dots_to_dashes(id: &str) -> String {
    let bytes = id.as_bytes();
    id.char_indices()
        .map(|(index, character)| {
            let between_digits = character == '.'
                && index > 0
                && bytes.get(index - 1).is_some_and(u8::is_ascii_digit)
                && bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
            if between_digits { '-' } else { character }
        })
        .collect()
}

/// Kontext-/Ausgabegrenzen eines Modells.
///
/// # Beschreibung
/// Rangfolge für das Fenster: `[models.<id>].context_window` (per Schlüssel,
/// `id` oder Alias) → eingebauter Modellkatalog (`harw-model-catalog`,
/// verifizierte Vendor-Angaben; Alias-/Präfix-Abgleich über
/// [`match_catalog_key`], zuerst mit der konfigurierten `id`, dann mit der
/// übergebenen Kennung) → [`UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS`] mit
/// `tracing::warn!`. `max_tokens`/`reasoning` stammen aus dem
/// Konfigurationseintrag, Katalog-`max_output_tokens`/Reasoning aus dem
/// Katalogtreffer.
///
/// # Argumente
/// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration.
/// - `model_id` (`&str`): Modellkennung, Schlüssel oder Alias.
///
/// # Rückgabe
/// Die [`ModelLimits`] des Modells.
#[must_use]
pub fn model_limits_for(config: &ResolvedConfig, model_id: &str) -> ModelLimits {
    let (limits, local) = lookup_model_limits(config, model_id);
    if !limits.known && local {
        tracing::warn!(
            model = model_id,
            fallback_tokens = UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS,
            "runtime.context_window.unknown_local_model: lokales Modell ohne \
             [models.<id>].context_window; `harw provider scan` liest das Fenster vom \
             Server, sonst gilt das konservative Rückfallfenster"
        );
    } else if !limits.known {
        tracing::warn!(
            model = model_id,
            fallback_tokens = UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS,
            "runtime.context_window.unknown_model: weder [models.<id>].context_window noch \
             der Modellkatalog kennen dieses Modell; konservatives Rückfallfenster aktiv"
        );
    }
    limits
}

/// Die Nachschlage-Logik von [`model_limits_for`] ohne Warnung.
///
/// # Rückgabe
/// `(limits, local)` — `local` ist `true`, wenn das Modell über einen lokalen
/// Provider läuft ([`model_runs_locally`]).
fn lookup_model_limits(config: &ResolvedConfig, model_id: &str) -> (ModelLimits, bool) {
    let configured = config.models.get(model_id).or_else(|| {
        config.models.values().find(|model| {
            model.id == model_id || model.aliases.iter().any(|alias| alias.as_str() == model_id)
        })
    });
    let catalog = builtin_catalog();
    // Runde 7, Teil L3: Ein lokal betriebenes Modell (vLLM/LM Studio/Ollama)
    // hat das Fenster, mit dem der Server gestartet wurde — nicht das des
    // Herstellers. Deshalb für lokale Provider kein Katalog-Präfix-Match.
    let local = model_runs_locally(config, configured, model_id);
    let catalog_entry = if local {
        None
    } else {
        configured
            .and_then(|model| match_catalog_key(catalog.keys().map(String::as_str), &model.id))
            .or_else(|| match_catalog_key(catalog.keys().map(String::as_str), model_id))
            .and_then(|key| catalog.get(key))
    };

    let configured_window = configured
        .and_then(|model| model.context_window)
        .filter(|window| *window > 0);
    let catalog_window = catalog_entry
        .map(|entry| entry.context_window)
        .filter(|window| *window > 0);
    let known = configured_window.is_some() || catalog_window.is_some();
    let context_window = configured_window
        .or(catalog_window)
        .unwrap_or(UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS);
    let limits = ModelLimits {
        context_window,
        configured_max_output: configured.and_then(|model| model.max_tokens),
        catalog_max_output: catalog_entry.and_then(|entry| entry.max_output),
        thinking: configured.is_some_and(|model| model.reasoning)
            || catalog_entry.is_some_and(|entry| entry.thinking),
        known,
    };
    (limits, local)
}

/// Runde 7, Teil L3: `true`, wenn `model_id` über einen lokalen Provider
/// läuft ([`harw_config::ProviderToml::is_local`]: Ollama, Loopback, LAN mit
/// `allow_insecure_lan`). Maßgeblich ist der Provider des konfigurierten
/// Modells; ohne Modelleintrag der Standard-Provider, wenn `model_id` das
/// Standardmodell ist.
fn model_runs_locally(
    config: &ResolvedConfig,
    configured: Option<&harw_config::ModelToml>,
    model_id: &str,
) -> bool {
    let provider = match configured {
        Some(model) => Some(model.provider.as_str()),
        None if config.harness.default_model.as_deref() == Some(model_id) => {
            config.harness.default_provider.as_deref()
        }
        None => None,
    };
    provider
        .and_then(|name| config.providers.get(name))
        .is_some_and(harw_config::ProviderToml::is_local)
}

/// Grenzen des Modells `model` oder, ohne Modell, des Rückfallmodells
/// `fallback`; ohne beide das konservative Rückfallfenster (mit Warnung).
fn model_limits_or_fallback(
    config: &ResolvedConfig,
    model: Option<&str>,
    fallback: Option<&str>,
) -> ModelLimits {
    match model.or(fallback) {
        Some(model) => model_limits_for(config, model),
        None => {
            tracing::warn!(
                fallback_tokens = UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS,
                "runtime.context_window.no_model: kein Modell konfiguriert; konservatives \
                 Rückfallfenster aktiv"
            );
            ModelLimits {
                context_window: UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS,
                configured_max_output: None,
                catalog_max_output: None,
                thinking: false,
                known: false,
            }
        }
    }
}

/// Grenzen für Wurzel und Kinder einer Montage mit Modellquelle
/// `offline_echo` ([`ModelSource::Echo`]) bzw. einem echten Provider.
///
/// # Beschreibung
/// Ohne Echo exakt [`model_limits_or_fallback`] (unbekanntes echtes Modell →
/// [`UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS`] mit Warnung). Mit Echo gilt ein
/// konfiguriertes oder katalogisiertes Fenster weiterhin; nur der Rückfall
/// ist [`OFFLINE_ECHO_CONTEXT_WINDOW_TOKENS`] — ohne Warnung, denn der Echo
/// hat kein Fenster, das überschritten werden könnte.
///
/// # Argumente
/// - `config` (`&ResolvedConfig`): die aufgelöste Konfiguration.
/// - `offline_echo` (`bool`): `true`, wenn die Montage den Offline-Echo fährt.
/// - `model` / `fallback` (`Option<&str>`): wie bei [`model_limits_or_fallback`].
///
/// # Rückgabe
/// Die [`ModelLimits`] für Fenster und Ausgabereserve.
fn session_model_limits(
    config: &ResolvedConfig,
    offline_echo: bool,
    model: Option<&str>,
    fallback: Option<&str>,
) -> ModelLimits {
    if !offline_echo {
        return model_limits_or_fallback(config, model, fallback);
    }
    model
        .or(fallback)
        .map(|model| lookup_model_limits(config, model).0)
        .filter(|limits| limits.known)
        .unwrap_or(ModelLimits {
            context_window: OFFLINE_ECHO_CONTEXT_WINDOW_TOKENS,
            configured_max_output: None,
            catalog_max_output: None,
            thinking: false,
            known: false,
        })
}

/// Kontextfenster eines Modells in Tokens.
///
/// Kurzform von [`model_limits_for`]`(..).context_window` — Rangfolge
/// `[models.<id>].context_window` (per ID oder Alias) → eingebauter
/// Modellkatalog (Alias-/Präfix-Abgleich) →
/// [`UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS`] (mit Warnung).
#[must_use]
pub fn context_window_for_model(config: &ResolvedConfig, model_id: &str) -> u64 {
    model_limits_for(config, model_id).context_window
}

/// Die feste Verdichtungs-Obergrenze dieses Laufs in Tokens:
/// `[compaction] absolute_ceiling_tokens`, sonst
/// [`harw_core::DEFAULT_ABSOLUTE_CEILING_TOKENS`]. Gilt für Wurzel und Kinder.
fn configured_compaction_ceiling(config: &ResolvedConfig) -> u64 {
    config
        .harness
        .compaction
        .absolute_ceiling_tokens
        .unwrap_or(harw_core::DEFAULT_ABSOLUTE_CEILING_TOKENS)
}

/// Byte-Budget der Request-Montage aus dem Kontextfenster abgeleitet: der
/// Verlauf darf ~3 Bytes je Token belegen (konservativ, Text ≈ 4 B/Token),
/// damit Auto-Compaction (70 % des Fensters) greift, **bevor** die harte
/// Byte-Kappung still Verlauf verwirft. `[context].max_history_bytes`
/// übersteuert.
#[must_use]
pub fn context_budget_for_window(config: &ResolvedConfig, window_tokens: u64) -> ContextBudget {
    let conservative = ContextBudget::conservative();
    let derived = usize::try_from(window_tokens.saturating_mul(3)).unwrap_or(usize::MAX);
    ContextBudget {
        max_context_bytes: conservative.max_context_bytes,
        max_history_bytes: config
            .harness
            .compaction
            .max_history_bytes
            .unwrap_or_else(|| derived.max(conservative.max_history_bytes)),
    }
}

/// Bridges controller lifecycle snapshots into the runtime's existing durable
/// session store. The observer never blocks controller locks: it schedules a
/// write on the current Tokio runtime after the controller has released its
/// own registry locks. A controller can also be driven from a synchronous
/// caller (notably command setup and embedding hosts); in that case a short
/// lived current-thread runtime synchronously owns the write instead of
/// dropping or detaching it. Controller locks have already been released at
/// this boundary, so this bridge cannot stall controller scheduling.
struct StateStoreOrchestrationObserver {
    state_store: Arc<dyn StateStore>,
}

impl OrchestrationObserver for StateStoreOrchestrationObserver {
    fn on_orchestration_event(&self, event: AgentOrchestrationEvent) {
        let store = Arc::clone(&self.state_store);
        let session = event.root_session_id.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Err(error) = store.record_agent_orchestration(&session, &event).await {
                        tracing::warn!(%error, session = %session, "orchestration_event.persist_failed");
                    }
                });
            }
            Err(_) => {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::warn!(%error, session = %session, "orchestration_event.bridge_runtime_failed");
                        return;
                    }
                };
                runtime.block_on(async move {
                    if let Err(error) = store.record_agent_orchestration(&session, &event).await {
                        tracing::warn!(%error, session = %session, "orchestration_event.persist_failed");
                    }
                });
            }
        }
    }
}

/// Die Leihgaben, aus denen [`build_spawner`] den Spawner baut.
///
/// # Beschreibung
/// Die zehn Werte (seit Welle 3a: zwei Modelle statt eines) stammen aus
/// verschiedenen, voneinander unabhängigen Montageschritten (Konfiguration,
/// Projekt, Freigabekette, Wurzel-Baum-Modell, UIA-Worker-Modell,
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
    /// Der Modellanbieter des Wurzel-**Baums** (`default_tree_model`); jede
    /// "normale" Kind-Rolle benutzt ihn — **unabhängig** davon, ob der
    /// laufende Einstieg gerade eine UIA ist ([`Self::uia_worker_model`]
    /// trägt deren ggf. abweichendes Modell separat).
    model: &'a Arc<dyn ModelProvider>,
    /// Das Modell der `uia-worker`-Rollenfamilie (Welle 3a, Teil A):
    /// `uia-worker`, `uia-explorer`, `uia-writer`, `uia-shell-worker`,
    /// `uia-latex-writer`. Ohne
    /// aktive UIA identisch zu [`Self::model`] (`Arc::clone`); mit aktiver
    /// UIA das über [`crate::model::build_uia_worker_model`] abgeleitete
    /// Modell (siehe [`split_root_and_uia_worker_models`]).
    uia_worker_model: &'a Arc<dyn ModelProvider>,
    /// Runde 5, Teil G: eigene Modellwahl je UIA-Worker-Rolle; die
    /// UIA-Worker-Fabrik baut das Modell jedes Kindes beim Start daraus.
    uia_worker_routing: Arc<crate::uia_worker_routing::UiaWorkerRouting>,
    /// Live-Modellwahl der Montage; die Fabrik des Wurzel-Baums gibt neu
    /// gestarteten Kindern daraus Hauptmodell und Rollenwahl.
    live_models: Arc<crate::live_model::LiveModelRouting>,
    /// Die Kennung der Wurzelsitzung, unter der der Spawner sie registriert.
    root_session_id: &'a SessionId,
    /// Das Modell, das die Wurzelsitzung treibt ([`effective_root_model_id`]);
    /// Teil C: Rückfall für das Kind-Modell und Hauptmodell der
    /// UIA-Worker-Fabrik.
    root_model_id: Option<String>,
    /// `true` bei [`ModelSource::Echo`]: Kind-Fenster ohne bekanntes Modell
    /// sind dann [`OFFLINE_ECHO_CONTEXT_WINDOW_TOKENS`] ([`session_model_limits`]).
    offline_echo: bool,
    /// Der eine Spawn-Kontext des Laufs (Sandbox, Trace, Decke).
    spawn_context: &'a SpawnContext,
    /// Der gewählte Reasoning-Effort, falls einer gesetzt ist.
    reasoning_effort: Option<harw_types::ReasoningEffort>,
    /// Die Basis-Aktivierung der Wurzel; Kinder werden dagegen geschnitten.
    activation: &'a SessionActivation,
    /// Der Roster aller startbaren Agenten (Plan R9, Teil B): eingebaute
    /// Rollen und geklemmte benutzerdefinierte Agenten, je mit ihrer IR.
    roster: &'a harw_registry_defaults::AgentRoster,
    /// Wächter-Schwellen dieses Laufs (Addendum F+G); jedes über diesen
    /// Spawner admittierte Kind bekommt dieselbe Politik wie die Wurzel
    /// (`children.rs`-Brief: "Kind-Sessions bekommen dieselbe GuardPolicy +
    /// DriftTracer + PitfallAdvisor, falls die Factory die Session baut" —
    /// hier: falls [`ManagedAgentSpawner`] die Session baut).
    guard_policy: GuardPolicy,
    /// Pitfall-Berater dieses Laufs (Addendum F+G), `None` ohne geöffnete
    /// Projekt-Fakten-Wurzel; wie `guard_policy` an jedes Kind weitergereicht.
    pitfall_advisor: Option<Arc<dyn PitfallAdvisor>>,
    /// Agentendefinitions-Verzeichnis des aktiven Profils (`<profil>/agents`,
    /// Welle FANIN-K/FANIN-RT, Fan-in-Zusatzpunkt "`profile_agents_dir` …
    /// im Runtime-Pfad setzen"). `None`, wenn kein Profil ermittelbar war.
    profile_agents_dir: Option<PathBuf>,
    /// Die aufgelöste Config des Laufs, für
    /// [`RuntimeChildRegistryFactory::with_reasoning_effort_config`] (Welle
    /// 8: Rangfolge Provider > Modell > Agent > Rolle). Jede über diesen
    /// Spawner gebaute Kind-Fabrik (`factory` **und** `uia_worker_factory`)
    /// bekommt sie — ohne diesen Aufruf lieferten die neuen Kind-Effort-
    /// Vorgaben unverändert `(None, None)`.
    reasoning_effort_config: Arc<ResolvedConfig>,
    /// Das Sandbox-Profil der Wurzel (Teil B4), reicht über
    /// [`RuntimeChildRegistryFactory::with_host_permits`] an jede über diesen
    /// Spawner gebaute Kind-Fabrik (`factory` **und** `uia_worker_factory`)
    /// durch — dieselbe Instanz, mit der auch die Root-Registry montiert
    /// wurde ([`sandbox_profile_from_config`]).
    sandbox_profile: &'a harw_sandbox::SandboxProfile,
    /// Die Host-Permit-Verdrahtung der Wurzel (Ledger, Sitzungs-Registry,
    /// Fragekanal-Sender; Teil B3/B4), `None` ohne Verdrahtung. Wie
    /// [`Self::sandbox_profile`] an `factory` **und** `uia_worker_factory`
    /// durchgereicht — damit erreichen auch `uia-shell-worker` und
    /// `host-process-worker` dieselbe Freigabekette wie die Wurzel.
    host_permit_wiring: &'a Option<HostPermitWiring>,
    /// Shared transcript/state store used by lifecycle observer records.
    state_store: Arc<dyn StateStore>,
    /// Live-Bus des Laufs; jedes Kind bekommt ihn über den SessionManager.
    agent_events: harw_core::AgentEventHub,
    /// Die vertrauten Config-Layer in aufsteigender Präzedenz
    /// (`ConfigTrustReport::layers`); reicht über
    /// [`RuntimeChildRegistryFactory::with_skill_catalog`] an `factory`
    /// **und** `uia_worker_factory` durch (Welle 4, Skills erreichen Kinder).
    skill_roots: Vec<PathBuf>,
    /// Wissensspeicher und Kanban-Ledger für die lesenden Wissenswerkzeuge
    /// der Kinder (Plan Teil D); reicht über
    /// [`RuntimeChildRegistryFactory::with_knowledge`] an `factory` **und**
    /// `uia_worker_factory`. `None` → kein Kind bekommt sie.
    knowledge: Option<crate::children::ChildKnowledgeSource>,
    /// [`RuntimeSpec::child_backend`] des Laufs (#22 Welle 3C); `Some` lässt
    /// den gebauten `ManagedAgentSpawner` jedes Kind über dieses
    /// [`harw_core::child_backend::ChildBackend`] statt in-process laufen.
    child_backend: Option<Arc<dyn harw_core::child_backend::ChildBackend>>,
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
        uia_worker_model,
        uia_worker_routing,
        live_models,
        root_session_id,
        root_model_id,
        offline_echo,
        spawn_context,
        reasoning_effort,
        activation,
        roster,
        guard_policy,
        pitfall_advisor,
        profile_agents_dir,
        reasoning_effort_config,
        sandbox_profile,
        host_permit_wiring,
        state_store,
        agent_events,
        skill_roots,
        knowledge,
        child_backend,
    } = inputs;

    let events = session_events.ok_or_else(|| RuntimeError::Spawner {
        detail: "an entry with a child spawner needs a session event sender \
                 (RuntimeAssemblyBuilder::session_events)"
            .to_owned(),
    })?;

    let spawner_slot = Arc::new(std::sync::OnceLock::new());
    // Runde 5, Teil C: ein gemeinsamer Diary-Recorder für die Kinder beider
    // Fabriken (Agent-Id = Rollenname, Einträge bei Verdichtung und
    // Kind-Freigabe); nur mit Wissensspeicher.
    let child_diary = knowledge.as_ref().map(|(store, _)| {
        Arc::new(
            crate::diary_wiring::DiaryRecorder::new(
                Arc::clone(store),
                harw_knowledge::AgentId::new("child".to_owned()),
            )
            .with_agent_events(agent_events.clone()),
        )
    });
    // Teil C: die Modelle, die ungepinnte Kinder tatsächlich rufen. Der
    // Wurzel-Baum spricht das effektive Vorgabemodell an (inklusive Rückfall
    // auf das erste nutzbare Katalogmodell); die Wurzel selbst ggf. das
    // UIA-Modell. Ist das Wurzelmodell nur das rohe Vorgabemodell, gilt
    // dessen effektive Auflösung.
    let default_model_id = crate::model::effective_default_model_id(config);
    let root_model_id = match root_model_id {
        Some(model) if Some(&model) != config.harness.default_model.as_ref() => Some(model),
        _ => default_model_id.clone(),
    };
    let factory: Arc<dyn ChildRegistryFactory> = Arc::new(
        RuntimeChildRegistryFactory::with_definitions(
            project.clone(),
            Arc::clone(model),
            chain.clone(),
            roster.definitions().clone(),
        )
        // Plan R9, Teil B: Basisrolle, Profil und Instruktionen der
        // benutzerdefinierten Agenten.
        .with_custom_agents(roster.custom_wiring())
        .with_internal_models(crate::children::resolve_internal_models_for_children(
            config,
        ))
        // Live-Modellwechsel: neue Kinder folgen `/model switch` und
        // `/models set` ohne Neustart.
        .with_live_models(live_models)
        .with_profile_agents_dir(profile_agents_dir.clone())
        .with_browser_config(config.browser.clone())
        .with_spawner_slot(Arc::clone(&spawner_slot))
        .with_reasoning_effort_config(Arc::clone(&reasoning_effort_config))
        // Teil B4: dasselbe Sandbox-Profil und dieselbe Host-Permit-
        // Verdrahtung wie die Root-Registry — ohne diesen Aufruf bliebe jede
        // Kind-Registry bei `SandboxProfile::Strict`/`None` (Strict-Fallback
        // in `RuntimeChildRegistryFactory::with_definitions`), unabhängig
        // vom konfigurierten Root-Profil.
        .with_host_permits(sandbox_profile.clone(), host_permit_wiring.clone())
        // Teil D: schließt die Effort-Lücke für Kinder auf dem Hauptmodell
        // (`resolved.is_main_model()` in `reasoning_effort_defaults_for_point`).
        .with_main_model_selection(
            config.harness.default_provider.clone(),
            config.harness.default_model.clone(),
        )
        // Teil C: das Modell, das ein ungepinntes Kind dieser Fabrik ruft.
        .with_effective_main_model(default_model_id.clone())
        // B: `delegate_wave` der Orchestrator-Kinder braucht denselben
        // StateStore wie die Wurzel (den Spawner löst die Fabrik selbst über
        // den Spawner-Slot auf).
        .with_delegate_wave_store(Arc::clone(&state_store))
        .with_skill_catalog(config, skill_roots.clone())?
        .with_optional_knowledge(knowledge.clone())
        // Runde 5, Teil C: Diary der Kind-Agenten.
        .with_child_diary(child_diary.clone()),
    );
    // Welle 3a, Teil A: eine zweite Fabrik-Instanz, ausschließlich für die
    // `uia-worker`-Rollenfamilie (`AgentRoleId::UiaWorker`). Gleiches Projekt,
    // gleiche Kette, gleiche gesenkten Definitionen wie `factory` — der
    // einzige Unterschied ist `model`: hier bereits exakt das
    // UIA-Worker-Modell (`uia_worker_model`), deshalb **ohne**
    // `.with_internal_models(...)` — `internal_point_for_role` liefert für
    // diese vier Rollen ohnehin `None` (Welle 3a, Teil A, Schritt 1), eine
    // interne Modellstelle könnte hier nichts mehr überschreiben.
    // Teil D: derselbe „`uia_provider` mit Fallback auf `default_provider`"-
    // Vorrang wie an anderer Stelle der Montage
    // (`resolve_root_uia_reasoning_effort_defaults`); das Modell folgt der
    // in `with_main_model_selection`s Doku festgelegten Rangfolge
    // `uia_worker_model` > `uia_model` > `default_model` — dieselben
    // Konfigurationsfelder, aus denen auch [`crate::model::build_uia_worker_model`]
    // (dort privat, deshalb hier dupliziert statt importiert) das tatsächliche
    // Modell dieser Rollenfamilie ableitet.
    let uia_worker_provider = config
        .harness
        .uia_provider
        .clone()
        .or_else(|| config.harness.default_provider.clone());
    let uia_worker_model_id = config
        .harness
        .uia_worker_model
        .clone()
        .or_else(|| config.harness.uia_model.clone())
        .or_else(|| config.harness.default_model.clone());
    let uia_worker_factory: Arc<dyn ChildRegistryFactory> = Arc::new(
        RuntimeChildRegistryFactory::with_definitions(
            project.clone(),
            Arc::clone(uia_worker_model),
            chain.clone(),
            roster.definitions().clone(),
        )
        // Plan R9, Teil B: Basisrolle, Profil und Instruktionen der
        // benutzerdefinierten Agenten.
        .with_custom_agents(roster.custom_wiring())
        .with_profile_agents_dir(profile_agents_dir)
        .with_browser_config(config.browser.clone())
        .with_spawner_slot(Arc::clone(&spawner_slot))
        .with_reasoning_effort_config(Arc::clone(&reasoning_effort_config))
        // Teil B4: dieselbe Verdrahtung wie `factory` — erreicht damit auch
        // `uia-shell-worker` und `host-process-worker` (beide Rollen der
        // `uia-worker`-Familie).
        .with_host_permits(sandbox_profile.clone(), host_permit_wiring.clone())
        .with_main_model_selection(uia_worker_provider, uia_worker_model_id)
        // Runde 5, Teil G: Modell je Rolle (eigene Wahl oder „wie UIA“).
        .with_uia_worker_routing(uia_worker_routing)
        // Teil C: ohne gültigen `uia_worker_model`-Pin ruft die Familie das
        // Modell der UIA-Sitzung (ohne aktive UIA: das Vorgabemodell).
        .with_effective_main_model(root_model_id.clone())
        .with_skill_catalog(config, skill_roots.clone())?
        .with_optional_knowledge(knowledge)
        // Runde 5, Teil C: Diary der Kind-Agenten.
        .with_child_diary(child_diary),
    );

    let model_id = config.harness.default_model.as_deref().unwrap_or_default();
    let manager = Arc::new(std::sync::Mutex::new(
        SessionManager::new(events).with_agent_events(agent_events.clone()),
    ));
    let window_config = Arc::new(config.clone());
    let probe_config = Arc::clone(&window_config);
    let window_default = default_model_id.clone();
    let probe_default = default_model_id.clone();
    let mut spawner = ManagedAgentSpawner::new(manager, child_limits(config, model_id))
        // Jedes Kind bekommt das Kontextfenster seines tatsächlichen Modells
        // (Teil C: ohne Modell das effektive Vorgabemodell des Wurzel-Baums,
        // nicht das rohe `default_model`).
        .with_context_window_resolver(Arc::new(move |model: Option<&str>| {
            session_model_limits(
                &window_config,
                offline_echo,
                model,
                window_default.as_deref(),
            )
            .context_window
        }))
        // Teil C: ein unbekanntes Kind-Modell (Rückfallfenster) meldet die
        // Admission laut im Trace und im Agent-Panel.
        .with_model_known_probe(Arc::new(move |model: Option<&str>| {
            model
                .or(probe_default.as_deref())
                .is_some_and(|model| model_limits_for(&probe_config, model).known)
        }))
        // Teil C: Rückfall für das Kind-Modell, wenn weder Rollen-Pin noch
        // Fabrik-Hauptmodell bekannt sind.
        .with_root_model(root_model_id.clone())
        // Anzeige `<provider>/<modell>`: Provider der Wurzel als Rückfall.
        .with_root_provider(effective_root_provider_id(
            config,
            spawn_context.organizational_role == AgentRoleId::UserInterface,
        ))
        // Welle 3: dieselbe feste Verdichtungs-Obergrenze wie die Wurzel
        // (`[compaction] absolute_ceiling_tokens`, sonst Kern-Vorgabe).
        .with_compaction_ceiling(Some(configured_compaction_ceiling(config)))
        // Runde 5, Teil K: `[agents]` — Orchestrierungsgrenzen (geklemmt,
        // konsistent); die allgemeine Tiefe steckt in `child_limits`.
        .with_orchestration_limits(crate::agent_background_wiring::orchestration_limits(config))
        // Orchestrierungs-Events gehen live auf den Bus und danach in den
        // persistierenden StateStore-Observer.
        .with_orchestration_observer(Arc::new(harw_core::HubOrchestrationObserver::new(
            agent_events,
            Some(Arc::new(StateStoreOrchestrationObserver { state_store })),
        )))
        // Addendum F+G: Rollen-Reasoning-Gewichtung und Drift-Beobachter
        // gelten für jedes über diesen Spawner admittierte Kind.
        .with_role_effort_weights(Some(crate::guard_wiring::role_effort_weights_from_config(
            config,
        )))
        .with_drift_observer(Some(
            Arc::new(crate::guard_wiring::DriftTracer) as Arc<dyn DriftObserver>
        ))
        // Welle FANIN-K/FANIN-RT: dieselben Wächter-Schwellen und derselbe
        // Pitfall-Berater wie die Wurzelsitzung (`Self::new_root_session`,
        // `with_guard_policy`/`with_pitfall_advisor`) gelten für jedes über
        // diesen Spawner gebaute Kind — die Kind-Fabrik baut keine eigene
        // Session, [`ManagedAgentSpawner`] tut das.
        .with_guard_policy(guard_policy)
        .with_pitfall_advisor(pitfall_advisor.clone())
        // Runde 3, Welle E: die werkzeug- und netzlosen Matrix-Sitzrollen
        // dürfen auch aus UIA-Sitzungen gestartet werden.
        .with_uia_spawnable_roles(
            role_names::MATRIX_ROLES
                .iter()
                .map(|role| (*role).to_owned()),
        )
        // Plan R9, Teil C/E1: Katalogdaten des Rosters — Beschreibung,
        // Skills, Rechte, Budget, Herkunft und die Lese-Eigenschaft, nach der
        // im Plan-Modus nur lesende Ziele delegierbar bleiben.
        .with_delegation_catalog(roster.entries().map(delegation_target_info));
    // Welle 3C: ein gesetztes `RuntimeSpec::child_backend` lässt jedes über
    // diesen Spawner admittierte Kind über dieses `ChildBackend` laufen
    // (z. B. `harw-agent-runner`s `JobChildBackend`) statt in-process.
    if let Some(backend) = child_backend {
        spawner = spawner.with_child_backend(backend);
    }
    // Plan R9, Teil B: registriert wird der ganze Roster — die eingebauten
    // Rollen (`role_names::ALL`) und daneben jeder benutzerdefinierte Agent.
    let mut roles: Vec<String> = Vec::with_capacity(roster.len());
    for role in roster.names() {
        // Welle 3a, Teil A, Schritt 5: dieselbe Organisationsrolle, die
        // `RuntimeChildRegistryFactory::build_registry` für `role` gleich
        // noch einmal liest — trifft automatisch alle vier
        // UIA-Spezialisierungen, ohne eine Namensliste zu pflegen.
        let organizational_role = roster
            .definitions()
            .get(role)
            .map_or(AgentRoleId::Worker, ExecutableAgentIr::role);
        let role_factory = if organizational_role == AgentRoleId::UiaWorker {
            Arc::clone(&uia_worker_factory)
        } else {
            Arc::clone(&factory)
        };
        spawner = spawner.with_role(
            role.to_owned(),
            AgentRole::Agent {
                name: role.to_owned(),
            },
            // The registered target's sealed role comes from its frozen
            // definition. Unknown/missing definitions fail closed as workers,
            // which cannot spawn further agents.
            organizational_role,
            role_factory,
        );
        roles.push(role.to_owned());
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

    let spawner = Arc::new(spawner);
    spawner_slot
        .set(Arc::downgrade(&spawner))
        .map_err(|_| RuntimeError::Spawner {
            detail: "managed child spawner slot was initialized twice".to_owned(),
        })?;

    Ok((Some(spawner), roles))
}

/// Plan R9, Teil C: die Katalogdaten eines Roster-Eintrags für
/// `agents.catalog`, `transfer_to_*`-Beschreibungen und die Plan-Modus-Regel
/// ([`harw_core::delegation_visibility::delegable_in_mode`]).
fn delegation_target_info(
    entry: &harw_registry_defaults::RosterEntry,
) -> harw_extension_api::DelegationTargetInfo {
    harw_extension_api::DelegationTargetInfo {
        name: entry.name.clone(),
        role: harw_core::delegation_visibility::role_label(entry.role).to_owned(),
        description: entry.description.clone(),
        skills: entry.skills.clone(),
        profile_summary: entry.profile_summary(),
        read_only: entry.read_only,
        budget_tokens: entry.budget_tokens,
        custom: entry.is_custom(),
    }
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
    /// Projekt-Root und Trust-Anker nach Contract §3 (`harw_home::project`) —
    /// eigenständig von [`Self::project`] (Modellkontext): dieser hier trägt
    /// die Scope-Architektur (Projekt-Home, Projekt-Einstellungsdatei).
    home_project_root: ProjectRoot,
    /// Projekt-lokaler Zustand (`<root>/.harw`) desselben Projekts.
    home_project: ProjectHome,
    sandbox: SandboxSpec,
    ceiling: ContextCeiling,
    spawn_context: SpawnContext,
    budget: RootBudget,
    turn_limits: TurnLimits,
    network_scope: NetworkScope,
    chain: ApprovalChain,
    approval_mode: ApprovalModeCell,
    /// Geteilte Freigaberegeln dieses Laufs (Contract §2/§4); dieselbe Zelle
    /// wie in [`ApprovalChain`] und in jeder [`crate::services::ServiceMap`].
    allow_rules: AllowRuleSet,
    /// Zusätzliche Workspace-Wurzeln dieses Laufs (`/add-workdir`); dieselbe
    /// Zelle wie in [`Self::sandbox`] (via `SandboxSpec::with_extra_roots`).
    extra_roots: ExtraRootsCell,
    /// Effektives Freigabe-Timeout (Projekt > Global > Vorgabe), für die TUI.
    approval_timeout: Duration,
    agent_ir: Option<ExecutableAgentIr>,
    /// #22 Welle 1B: das Kontextprogramm der Wurzel (explizit gewählter
    /// Agent, sonst UIA), bereits gegen die Wurzeldecke geprüft; `None` ohne
    /// deklariertes Programm.
    root_context_program: Option<harw_agent_dsl::executable::ContextProgram>,
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
    /// Plan R9, Teil B: der Roster aller startbaren Agenten dieses Laufs
    /// (leer ohne Spawner-Politik bzw. ohne Definitionen).
    agent_roster: Arc<harw_registry_defaults::AgentRoster>,
    /// Agenten-übergreifender Live-Bus (Wurzel + alle Kinder).
    agent_events: harw_core::AgentEventHub,
    lifecycle_hooks: Vec<Arc<dyn SessionLifecycleHook>>,
    tools: Vec<String>,
    root_session_id: SessionId,
    /// Die Projekt-Erfassungsfläche des Gedächtnisses (Addendum B), `None`
    /// wenn [`RuntimeAssemblyBuilder::build`] sie nicht öffnen konnte.
    /// [`Self::new_root_session`] hängt daraus, falls gesetzt, einen
    /// [`crate::memory_wiring::MemoryCaptureObserver`] an die Wurzelsitzung.
    memory_capture: Option<Arc<harw_memory::capture::ProjectMemoryCapture>>,
    /// Automatische Diary-Einträge der Wurzelsitzung (Plan D3), `None` ohne
    /// `KnowledgeStore`. Steht zusätzlich in [`Self::lifecycle_hooks`]
    /// (Sitzungsende); [`Self::new_root_session`] kettet daraus die
    /// Verdichtungs- und Turn-Beobachter und meldet den Sitzungsstart.
    diary_recorder: Option<Arc<crate::diary_wiring::DiaryRecorder>>,
    /// Wächter-Schwellen dieses Laufs (Addendum F+G), aus `[guards]`
    /// aufgelöst über [`crate::guard_wiring::guard_policy_from_config`].
    /// [`Self::new_root_session`] hängt sie über `with_guard_policy` an die
    /// Wurzelsitzung.
    guard_policy: GuardPolicy,
    /// Rollen-Reasoning-Effort-Gewichtung dieses Laufs (Addendum F+G), aus
    /// `[reasoning]` aufgelöst über
    /// [`crate::guard_wiring::role_effort_weights_from_config`].
    /// [`Self::new_root_session`] nutzt `weights.uia`, falls die Wurzel eine
    /// UIA ist und [`RuntimeSpec::reasoning_effort`] `None` bleibt.
    role_effort_weights: RoleEffortWeights,
    /// Provider-/Modell-/Agenten-Reasoning-Effort-Vorgaben der Wurzel-UIA
    /// (Welle 8: Rangfolge Provider > Modell > Agent > Rolle), aus
    /// [`resolve_root_uia_reasoning_effort_defaults`]. `(None, None, None)`,
    /// solange der Einstieg keine UIA-Wurzel ist.
    /// [`Self::new_root_session`] reicht sie, zusammen mit
    /// `role_effort_weights.uia` als unterster Ebene, an
    /// [`crate::guard_wiring::resolve_default_reasoning_effort`] weiter.
    root_uia_reasoning_effort_defaults: (
        Option<harw_types::ReasoningEffort>,
        Option<harw_types::ReasoningEffort>,
        Option<String>,
    ),
    /// Pitfall-Berater über die Projekt-Fakten-Wurzel (Addendum F+G,
    /// `PitfallMatch`), `None` ohne geöffnete Projekt-Fakten-Wurzel.
    /// [`Self::new_root_session`] hängt ihn, falls gesetzt, über
    /// `with_pitfall_advisor` an die Wurzelsitzung.
    pitfall_advisor: Option<Arc<dyn PitfallAdvisor>>,
    /// Das effektive Modell der Wurzelsitzung (Welle 3): bei UIA-Wurzel das
    /// UIA-Modell, sonst `default_model` ([`effective_root_model_id`]).
    /// [`Self::new_root_session`] leitet daraus Kontextfenster und
    /// Ausgabereserve ab.
    root_model_id: Option<String>,
    /// `true`, wenn die Montage den Offline-Echo fährt ([`ModelSource::Echo`]);
    /// ohne bekanntes Wurzelmodell gilt dann
    /// [`OFFLINE_ECHO_CONTEXT_WINDOW_TOKENS`] ([`session_model_limits`]).
    offline_echo: bool,
    registry: Mutex<Option<ExtensionRegistry>>,
    responder: Mutex<Option<Arc<dyn ApprovalHandler>>>,
    /// Einmal je Montage instanziierter Permit-Ledger für Host-Profil-Worker
    /// (siehe [`harw_sandbox::ProcessPermitLedger`], `host-process-worker.toml`).
    host_permit_ledger: Arc<ProcessPermitLedger>,
    /// Sitzungsseitige Zuordnung von UI-Zustimmungen zu bereits ausgestellten
    /// Host-Permits dieses Laufs (siehe [`harw_sandbox::HostPermitSessionRegistry`]).
    host_permit_registry: Arc<HostPermitSessionRegistry>,
    /// Sendeseite des Host-Permit-Fragekanals (siehe
    /// [`harw_tool_shell::host_permit_prompt`]); an jeden `ShellToolProvider`
    /// weiterzugeben, dessen `sandbox_profile` tatsächlich
    /// [`harw_sandbox::SandboxProfile::Host`] ist (`with_host_permit_prompts`)
    /// — derselbe Kanal, dessen Empfängerseite [`Self::take_host_permit_prompts`]
    /// liefert.
    host_permit_prompt_sender: HostPermitPromptSender,
    /// Empfängerseite desselben Kanals, bis zur ersten Abholung gehalten
    /// (Take-once, wie die Wurzel-Registry) — genau ein Renderer (z. B.
    /// `harw-tui`) darf ihn übernehmen; ein zweiter Aufruf bekommt `Err`.
    host_permit_prompts: Mutex<Option<HostPermitPromptReceiver>>,
    /// Runde 5, Teil B: Empfängerseite des sudo-Fragekanals
    /// (`host.sudo_exec`); nur bei `EntryKind::Tui` gesetzt, take-once über
    /// [`Self::take_sudo_prompts`].
    sudo_prompts: Mutex<Option<harw_tool_shell::SudoPromptReceiver>>,
    /// Runde 5, Teil F: geteilter Plan-Zustand der Wurzel (Sperre, aktueller
    /// Slug, angehefteter Plan) — siehe [`Self::plan_session`].
    plan_session: harw_tool_plan::PlanSession,
    /// Runde 5, Teil F: Empfängerseite des Plan-Fragekanals (`plan.exit`,
    /// `plan.enter`, `ask_user`); nur `EntryKind::Tui`, einmal abholbar über
    /// [`Self::take_plan_ui_requests`].
    plan_ui_requests: Mutex<Option<harw_tool_plan::PlanUiReceiver>>,
    /// Runde 5, Teil G: Modellwahl der UIA-Worker-Rollen (siehe
    /// [`Self::uia_worker_routing`]).
    uia_worker_routing: Arc<crate::uia_worker_routing::UiaWorkerRouting>,
    /// Live-Modellwahl der Montage (siehe [`Self::live_models`]).
    live_models: Arc<crate::live_model::LiveModelRouting>,
    /// Plan R9, Teil F: Job-Verwaltung und Zustellung (nur TUI), siehe
    /// [`Self::session_jobs`].
    session_jobs: Option<crate::job_wiring::SessionJobs>,
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
            contributors: crate::contributors::default_contributors(),
            root_session_id: None,
            secret_resolver: None,
            secret_resolver_opener: None,
            narrowing: None,
            project_facts: None,
            global_facts: None,
            extra_lifecycle_hooks: Vec::new(),
            agent_events: None,
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

    /// Plan R9, Teil F: Job-Verwaltung und Ereignis-Zustellung dieser
    /// Sitzung — `Some` nur für die interaktive TUI.
    ///
    /// # Beschreibung
    /// Die TUI liest darüber die Jobs-Gruppe des Agenten-Panels
    /// (`manager.list(Caller::Operator)`), holt die Notizen an die Wurzel
    /// (`router.take_root_notes`) und Oberflächen-Ereignisse
    /// (`router.take_ui_events`) ab und fragt beim Beenden, ob laufende Jobs
    /// gestoppt oder abgelöst werden (`stop_all`/`detach_all`).
    #[must_use]
    pub fn session_jobs(&self) -> Option<&crate::job_wiring::SessionJobs> {
        self.session_jobs.as_ref()
    }

    /// Die aufgelöste Konfiguration **des Starts**.
    ///
    /// Für Anzeigen, die gespeicherte Änderungen dieses Laufs zeigen müssen
    /// (Modellwahl je Rolle, Standardmodus, …), gilt
    /// [`Self::current_config`].
    #[must_use]
    pub fn config(&self) -> &Arc<ResolvedConfig> {
        &self.config
    }

    /// Der aktuelle Stand der Konfiguration: Start-Stand plus alle seither
    /// gespeicherten Änderungen ([`RuntimeServices::current_config`]).
    #[must_use]
    pub fn current_config(&self) -> Arc<ResolvedConfig> {
        self.services.current_config()
    }

    /// Der einmal je Montage instanziierte Permit-Ledger für Host-Profil-
    /// Worker (siehe [`harw_sandbox::ProcessPermitLedger`]).
    ///
    /// # Beschreibung
    /// Wird von [`harw_registry_defaults::profile::assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits`]
    /// an jeden `ShellToolProvider` gehängt, dessen `sandbox_profile`
    /// tatsächlich [`harw_sandbox::SandboxProfile::Host`] ist. Konsumenten,
    /// die außerhalb dieser Montage selbst einen Host-Profil-Worker bauen
    /// (z. B. eine künftige Sub-Worker-Fanin-Stelle), müssen dieselbe Arc
    /// verwenden — ein zweiter, unabhängig instanziierter Ledger hätte keine
    /// Kenntnis von den über [`Self::host_permit_session_registry`]
    /// gemerkten Sitzungszustimmungen.
    ///
    /// # Returns
    /// Eine Referenz auf den geteilten `Arc<`[`harw_sandbox::ProcessPermitLedger`]`>`
    /// dieses Laufs — derselbe Zeiger für die gesamte Lebensdauer der
    /// Montage, nie neu instanziiert.
    ///
    /// # Concurrency
    /// `Send + Sync`; der Ledger selbst kapselt seine eigene Synchronisierung.
    /// Ein `Arc::clone` dieser Referenz ist die vorgesehene Art, den Ledger an
    /// einen neu gebauten `ShellToolProvider` weiterzugeben.
    #[must_use]
    pub fn host_permit_ledger(&self) -> &Arc<ProcessPermitLedger> {
        &self.host_permit_ledger
    }

    /// Die sitzungsseitige Zuordnung von UI-Zustimmungen zu bereits
    /// ausgestellten Host-Permits dieses Laufs (siehe
    /// [`harw_sandbox::HostPermitSessionRegistry`]).
    ///
    /// # Beschreibung
    /// Die lokale Bestätigungs-UI (`harw-tui`) ruft
    /// [`harw_sandbox::HostPermitSessionRegistry::mark_session_approved`] auf
    /// dieser Instanz auf, sobald ein Nutzer einer Host-Profil-Anfrage
    /// zustimmt; `harw_tool_shell::ShellExecutor` liest dieselbe Instanz
    /// über `with_host_permit_registry`, um weitere Befehle derselben Sitzung
    /// ohne erneute Rückfrage zu autorisieren.
    ///
    /// # Returns
    /// Eine Referenz auf den geteilten `Arc<`[`harw_sandbox::HostPermitSessionRegistry`]`>`
    /// dieses Laufs — derselbe Zeiger für die gesamte Lebensdauer der
    /// Montage. Wie der gesamte Permit-Ledger lebt dieser Zustand
    /// ausschließlich im Speicher dieses Prozesses (kein anderer `harw`-Prozess
    /// sieht ihn, siehe `harw_sandbox::HostPermitSessionRegistry`-Moduldoku).
    ///
    /// # Concurrency
    /// `Send + Sync`; die Registry kapselt ihre eigene Synchronisierung
    /// (ein `Mutex` je Registry, siehe `harw_sandbox::HostPermitSessionRegistry`).
    /// Ein `Arc::clone` dieser Referenz ist die vorgesehene Art, sie an einen
    /// neu gebauten `ShellToolProvider` oder den Bestätigungsdialog
    /// weiterzugeben.
    #[must_use]
    pub fn host_permit_session_registry(&self) -> &Arc<HostPermitSessionRegistry> {
        &self.host_permit_registry
    }

    /// Die Sendeseite des Host-Permit-Fragekanals dieses Laufs (siehe
    /// [`harw_tool_shell::host_permit_prompt`]).
    ///
    /// # Beschreibung
    /// Wer künftig einen `ShellToolProvider` mit
    /// [`harw_sandbox::SandboxProfile::Host`] baut (heute
    /// `harw_registry_defaults::profile`, außerhalb dieser Montage), muss
    /// diesen Sender über
    /// `ShellToolProvider::with_host_permit_prompts` anhängen — sonst bleibt
    /// jede Host-Anfrage ohne bereits gemerkten Permit oder laufende
    /// Sitzungsphase fail-closed abgelehnt, ohne dass je eine Frage entsteht.
    ///
    /// # Returns
    /// Eine Referenz auf den geteilten Sender; `Clone` liefert einen weiteren
    /// Zeiger auf denselben Kanal (ein `mpsc::UnboundedSender` ist `Clone`).
    #[must_use]
    pub fn host_permit_prompt_sender(&self) -> &HostPermitPromptSender {
        &self.host_permit_prompt_sender
    }

    /// Händigt die Empfängerseite des Host-Permit-Fragekanals einmalig aus.
    ///
    /// # Beschreibung
    /// Take-once, wie es die Wurzel-Registry dieses Laufs auch tut: der
    /// erste Aufruf liefert den [`HostPermitPromptReceiver`], den
    /// `RuntimeAssemblyBuilder::build` beim Aufbau dieses Laufs zusammen mit
    /// [`Self::host_permit_prompt_sender`] erzeugt hat
    /// ([`harw_tool_shell::host_permit_prompt::host_permit_prompt_channel`]).
    /// Ein Renderer (z. B. `harw-tui`) muss ihn ebenso pollen wie den
    /// normalen Freigabekanal — sonst läuft jede Host-Permit-Frage in den
    /// Timeout und gilt als Ablehnung (fail-closed, keine Sonderbehandlung).
    ///
    /// # Returns
    /// `Ok(receiver)` beim ersten Aufruf.
    ///
    /// # Errors
    /// [`RuntimeError::Registry`], wenn der Empfänger bereits ausgehändigt
    /// wurde oder die interne Sperre vergiftet ist.
    pub fn take_host_permit_prompts(&self) -> RuntimeResult<HostPermitPromptReceiver> {
        let mut slot = self
            .host_permit_prompts
            .lock()
            .map_err(|_| RuntimeError::Registry {
                detail: "the host permit prompt lock is poisoned".to_owned(),
            })?;
        slot.take().ok_or_else(|| RuntimeError::Registry {
            detail: "this runtime has already handed out its host permit prompt receiver"
                .to_owned(),
        })
    }

    /// Händigt die Empfängerseite des sudo-Fragekanals (`host.sudo_exec`,
    /// Runde 5 Teil B) einmalig aus.
    ///
    /// # Beschreibung
    /// Nur eine `EntryKind::Tui`-Montage hat einen Kanal; jeder andere
    /// Einstieg liefert `None`, ebenso ein zweiter Aufruf oder eine
    /// vergiftete Sperre. Ohne abgeholten Empfänger läuft jede sudo-Frage in
    /// den Zeitablauf und gilt als Ablehnung (fail-closed).
    ///
    /// # Returns
    /// `Some(receiver)` beim ersten Aufruf einer TUI-Montage, sonst `None`.
    #[must_use]
    pub fn take_sudo_prompts(&self) -> Option<harw_tool_shell::SudoPromptReceiver> {
        self.sudo_prompts
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    /// Runde 5, Teil F: der geteilte Plan-Zustand der Wurzel.
    ///
    /// # Beschreibung
    /// Die TUI setzt darüber die sofort wirkende Plan-Sperre (Shift+Tab,
    /// `/plan`, Freigabe über `plan.exit`), liest den aktuellen Plan
    /// (`/plan show|edit|open`) und heftet den freigegebenen Plan an.
    #[must_use]
    pub fn plan_session(&self) -> &harw_tool_plan::PlanSession {
        &self.plan_session
    }

    /// Runde 5, Teil F: händigt die Empfängerseite des Plan-Fragekanals
    /// (`plan.exit`, `plan.enter`, `ask_user`) einmalig aus.
    ///
    /// # Returns
    /// `Some(receiver)` beim ersten Aufruf einer TUI-Montage, sonst `None`
    /// (jeder andere Einstieg, zweiter Aufruf, vergiftete Sperre). Ohne
    /// abgeholten Empfänger endet jede Frage ohne Antwort — nie als
    /// Zustimmung.
    #[must_use]
    pub fn take_plan_ui_requests(&self) -> Option<harw_tool_plan::PlanUiReceiver> {
        self.plan_ui_requests
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    /// Runde 5, Teil G: die Modellwahl der UIA-Worker-Rollen dieses Laufs.
    ///
    /// # Beschreibung
    /// Die TUI meldet hierüber die Live-Auswahl der UIA
    /// ([`crate::uia_worker_routing::UiaWorkerRouting::set_live_uia`]) und
    /// neue Rollenwahlen; neu gestartete UIA-Worker nehmen sie, laufende
    /// behalten ihr Modell.
    #[must_use]
    pub fn uia_worker_routing(&self) -> &Arc<crate::uia_worker_routing::UiaWorkerRouting> {
        &self.uia_worker_routing
    }

    /// Die Live-Modellwahl dieses Laufs (Live-Modellwechsel).
    ///
    /// # Beschreibung
    /// Die TUI meldet hierüber an jeder Turn-Grenze die generische Auswahl
    /// des Controllers
    /// ([`crate::live_model::LiveModelRouting::set_live_main`]); neu
    /// gestartete Kinder des Wurzel-Baums nehmen sie, laufende behalten ihr
    /// Modell.
    #[must_use]
    pub fn live_models(&self) -> &Arc<crate::live_model::LiveModelRouting> {
        &self.live_models
    }

    /// Provider/Modell, das die Wurzelsitzung mit `active_model`/
    /// `active_provider` tatsächlich anspricht, als aufgelöste Kennungen.
    ///
    /// # Beschreibung
    /// Ohne eigene Wahl gilt das effektive Wurzelmodell des Starts
    /// (UIA-Modell bei UIA-Wurzel, sonst Vorgabemodell) und dessen
    /// Provider; ein Alias wird über den Modellkatalog zur Modell-ID
    /// aufgelöst. Grundlage der Modellanzeige (`<provider>/<modell>`).
    #[must_use]
    pub fn resolved_root_route(
        &self,
        active_provider: Option<&str>,
        active_model: Option<&str>,
    ) -> (Option<String>, Option<String>) {
        let model = active_model
            .map(str::to_owned)
            .or_else(|| self.root_model_id.clone());
        let provider = active_provider.map(str::to_owned).or_else(|| {
            match active_model {
                // Eigene Modellwahl ohne Provider: Provider laut Katalog.
                Some(model) => harw_config::catalog_provider_of(&self.config, model),
                None => self.root_provider_id(),
            }
        });
        let model = model.map(|model| crate::live_model::resolve_model_id(&self.config, &model));
        (provider, model)
    }

    /// `true`, wenn die Wurzelsitzung dieses Laufs eine UIA ist (dann gilt
    /// für sie die UIA-Auswahl des Controllers vor der generischen).
    #[must_use]
    pub fn root_is_uia(&self) -> bool {
        self.spawn_context.organizational_role == AgentRoleId::UserInterface
    }

    /// Provider des effektiven Wurzelmodells des Starts.
    fn root_provider_id(&self) -> Option<String> {
        effective_root_provider_id(&self.config, self.root_is_uia())
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

    /// Der über `harw_home::project` erkannte Projekt-Root samt Trust-Anker
    /// (Contract §3), eigenständig von [`Self::project`] (Modellkontext):
    /// dieser hier trägt die Scope-Architektur (Trust-Anker, `.harw`,
    /// Projekt-Einstellungsdatei). Marker sind `config.project_root_markers`,
    /// leer bedeutet `[".git"]` ([`harw_home::project::discover_project`]).
    #[must_use]
    pub const fn home_project_root(&self) -> &ProjectRoot {
        &self.home_project_root
    }

    /// Das Projekt-lokale Home (`<root>/.harw`) dieses Laufs (Contract §3).
    ///
    /// # Beschreibung
    /// [`RuntimeAssemblyBuilder::build`] hat [`ProjectHome::ensure`] bereits
    /// best-effort aufgerufen (ein Fehler dort bleibt ein `warn!`, ohne den
    /// Bau abzubrechen); dieser Zugriff liefert dasselbe Objekt zum
    /// wiederholten Gebrauch, etwa für weitere `.harw`-Unterverzeichnisse.
    #[must_use]
    pub const fn home_project(&self) -> &ProjectHome {
        &self.home_project
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
    /// (W2A-02). Er entspricht der Basis-Aktivierung, die
    /// [`Self::new_root_session`] aus derselben `agent_ir` ableitet (bei
    /// aktiver UIA: `SessionActivation::default()`). Er ist hier lesbar, weil die Anzeige den
    /// Unterschied „vom Modus verengt" gegen „von der Agent-Definition
    /// verboten" braucht — `harw-tui`s `/tools` (W2A-01, „noch kein
    /// Aufrufer") ist der benannte Verbraucher in Welle W2d.
    #[must_use]
    pub const fn root_activation(&self) -> &SessionActivation {
        &self.activation
    }

    /// #22 Welle 1B: das Kontextprogramm, das die Wurzelsitzung trägt
    /// (explizit gewählter Agent, sonst UIA), bereits gegen die Wurzeldecke
    /// geprüft; `None` ohne deklariertes Programm.
    #[must_use]
    pub const fn root_context_program(
        &self,
    ) -> Option<&harw_agent_dsl::executable::ContextProgram> {
        self.root_context_program.as_ref()
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

    /// Die geteilten Freigaberegeln dieses Laufs (Contract §2/§4).
    ///
    /// # Beschreibung
    /// Dieselbe Zelle, die in der Freigabekette dieses Laufs
    /// ([`ApprovalChain::rules`]) und in jeder [`crate::services::ServiceMap`]
    /// liegt (über [`RuntimeServices::allow_rules`]) — eine über
    /// `/permissions` angelegte Regel gilt damit sofort überall.
    #[must_use]
    pub const fn allow_rules(&self) -> &AllowRuleSet {
        &self.allow_rules
    }

    /// Runde 5, Teil E: der geteilte Auto-Modus-Zustand dieses Laufs
    /// (Protokoll für `/permissions log` und die Werkzeugzellen, Sitzungsziel
    /// für den Klassifizierer, Lern-Zähler für den Freigabedialog).
    ///
    /// # Rückgabe
    /// `Some` genau für Einstiege mit anwesender Person
    /// ([`AskResolution::Interactive`]).
    #[must_use]
    pub fn auto_mode(&self) -> Option<&crate::auto_classifier::AutoModeHandle> {
        self.chain.auto_mode()
    }

    /// Die zusätzlichen Workspace-Wurzeln dieses Laufs (`/add-workdir`,
    /// Contract §5 Slice A8).
    ///
    /// # Beschreibung
    /// Dieselbe Zelle wie in [`Self::sandbox`] (via
    /// `SandboxSpec::with_extra_roots`) und in jeder Service-Map (über
    /// [`RuntimeServices::extra_roots`]).
    #[must_use]
    pub const fn extra_roots(&self) -> &ExtraRootsCell {
        &self.extra_roots
    }

    /// Das effektive Freigabe-Timeout dieses Laufs (Präzedenz Projekt >
    /// Global > [`DEFAULT_APPROVAL_TIMEOUT_SECS`], siehe
    /// [`effective_approval_timeout`]).
    ///
    /// # Beschreibung
    /// Für die TUI, deren Freigabe-Panel damit einen Countdown zeigen kann,
    /// bevor eine offene Rückfrage automatisch abgelehnt wird.
    #[must_use]
    pub const fn approval_timeout(&self) -> Duration {
        self.approval_timeout
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

    /// Der Roster aller startbaren Agenten dieses Laufs (Plan R9, Teil B):
    /// eingebaute Rollen plus benutzerdefinierte Agenten aus Profil und
    /// vertrautem Projekt, je mit Rolle, Beschreibung, Skills und
    /// Profilzusammenfassung ([`harw_registry_defaults::AgentRoster::entries`]).
    /// Leer, wenn der Lauf keinen Spawner hat.
    #[must_use]
    pub fn agent_roster(&self) -> &Arc<harw_registry_defaults::AgentRoster> {
        &self.agent_roster
    }

    /// Der agenten-übergreifende Live-Bus dieses Laufs. Beobachter (TUI,
    /// Web, Telemetrie) abonnieren ihn über
    /// [`harw_core::AgentEventHub::subscribe`].
    #[must_use]
    pub fn agent_events(&self) -> &harw_core::AgentEventHub {
        &self.agent_events
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

        // Auto-Verdichtung: Schwellen aus dem Kontextfenster des
        // effektiven Wurzelmodells (UIA-Modell bei UIA-Wurzel, sonst
        // Vorgabemodell; [`effective_root_model_id`]), nie deaktiviert — es
        // gibt (noch) keinen `harw_config::HarnessConfig`-Schalter, der eine
        // Abwahl erlaubte, und einen neuen zu erfinden ist außerhalb dieses
        // Vertrags. Die Ausgabereserve (`[models.<id>].max_tokens` bzw.
        // Katalog-Maximum, begrenzt durch das Fenster) und ein fester
        // Aufschlag für System-Prompt/Werkzeuge werden vom Fenster abgezogen.
        // Der Handoff-Beobachter schreibt bei jeder Verdichtung
        // `<project>/.harw/handoff.json` (Contract §"harw-runtime/src/handoff.rs").
        let offline_echo = self.offline_echo;
        let root_limits = session_model_limits(
            &self.config,
            offline_echo,
            self.root_model_id.as_deref(),
            None,
        );
        let context_window = root_limits.context_window;
        let output_reserve = root_limits.output_reserve_tokens();
        // Addendum F+G / Welle 8: eine UIA-Wurzel ohne expliziten Effort
        // (`spec.reasoning_effort`, die einzige Live-Einstellung, die über
        // dieser gesamten Rangfolge steht) erbt nicht mehr unverändert
        // `role_effort_weights.uia`, sondern die volle Rangfolge
        // Provider > Modell > Agent > Rolle
        // ([`crate::guard_wiring::resolve_default_reasoning_effort`]), mit
        // `role_effort_weights.uia` als Boden. Jeder andere Einstieg (auch
        // ein `active_agent`-Root ohne UIA) bleibt unverändert bei
        // `spec.reasoning_effort`.
        let reasoning_effort =
            if self.spawn_context.organizational_role == AgentRoleId::UserInterface {
                let (provider_default, model_default, agent_default) =
                    &self.root_uia_reasoning_effort_defaults;
                self.spec.reasoning_effort.or_else(|| {
                    crate::guard_wiring::resolve_default_reasoning_effort(
                        *provider_default,
                        *model_default,
                        agent_default.as_deref(),
                        Some(self.role_effort_weights.uia),
                    )
                })
            } else {
                self.spec.reasoning_effort
            };
        if let Some(recorder) = &self.diary_recorder {
            recorder.session_started(&id, None);
        }
        let mut session =
            AgentSession::new_with_id(id, AgentRole::Assistant, None, registry, events)
                .with_spawn_context(self.spawn_context.clone())
                .with_reasoning_effort(reasoning_effort)
                .with_turn_event_sink(turn_events)
                .with_agent_events(self.agent_events.clone())
                .with_context_budget(context_budget_for_window(&self.config, context_window))
                // Eine konfigurierte Verlaufsgrenze übersteht Modellwechsel.
                .with_configured_max_history_bytes(self.config.harness.compaction.max_history_bytes)
                // Die aus dem Budget abgeleiteten Turn-Grenzen werden jetzt
                // tatsächlich im Turn-Loop durchgesetzt.
                .with_default_turn_limits(self.turn_limits.to_core())
                .with_context_window_resolver({
                    let config = Arc::clone(&self.config);
                    let root_model_id = self.root_model_id.clone();
                    Arc::new(move |model: Option<&str>| {
                        session_model_limits(&config, offline_echo, model, root_model_id.as_deref())
                            .context_window
                    })
                })
                // Live-Modellwechsel: die Ausgabereserve folgt dem neuen
                // Modell (sonst bliebe die Reserve des Start-Modells stehen).
                .with_output_reserve_resolver({
                    let config = Arc::clone(&self.config);
                    let root_model_id = self.root_model_id.clone();
                    Arc::new(move |model: Option<&str>| {
                        session_model_limits(&config, offline_echo, model, root_model_id.as_deref())
                            .output_reserve_tokens()
                    })
                })
                .with_auto_compact(Some(
                    harw_core::AutoCompactPolicy::for_context_window(context_window)
                        .with_absolute_ceiling(Some(configured_compaction_ceiling(&self.config)))
                        .with_output_reserve(output_reserve)
                        .with_fixed_overhead(FIXED_CONTEXT_OVERHEAD_TOKENS),
                ))
                // Plan D3: der Diary-Recorder hängt sich hinter Handoff bzw.
                // Gedächtnis-Erfassung in dieselben Slots (Kette, kein
                // Ersatz) und zählt Verdichtungen und Runden mit.
                .with_compaction_observer({
                    let handoff: Arc<dyn harw_core::compaction::CompactionObserver> = Arc::new(
                        crate::handoff::HandoffWriter::new(self.home_project.clone()),
                    );
                    Some(match &self.diary_recorder {
                        Some(recorder) => recorder.compaction_observer(Some(handoff)),
                        None => handoff,
                    })
                })
                .with_tool_outcome_observer({
                    let memory = self.memory_capture.clone().map(|capture| {
                        Arc::new(crate::memory_wiring::MemoryCaptureObserver::new(capture))
                            as Arc<dyn harw_core::capture::ToolOutcomeObserver>
                    });
                    match &self.diary_recorder {
                        Some(recorder) => Some(recorder.tool_outcome_observer(memory)),
                        None => memory,
                    }
                })
                // Addendum F+G: Wächter-Verdrahtung der Wurzel-(UIA-)Sitzung.
                .with_guard_policy(Some(self.guard_policy))
                .with_drift_observer(Some(
                    Arc::new(crate::guard_wiring::DriftTracer) as Arc<dyn DriftObserver>
                ))
                .with_pitfall_advisor(self.pitfall_advisor.clone());
        // Addendum C: die Verdichtungs-Zusammenfassung nutzt die interne
        // Modellstelle `CompactionSummary`, sofern sie nicht auf das
        // Hauptmodell aufgelöst hat (dann bleibt `AgentSession` unverändert
        // beim Vorgabemodell der Sitzung).
        let summary = harw_config::resolve_internal_model(
            &self.config,
            harw_config::InternalModelPoint::CompactionSummary,
        );
        if !summary.is_main_model() {
            session = session.with_compaction_summary_model(
                summary.provider.map(ProviderId::from),
                summary.model.map(ModelId::from),
            );
        }
        if let Some(ir) = self.agent_ir.as_ref() {
            session = session.with_executable_agent_ir(ir);
        }
        // #22 Welle 1B: wie ein Kind (`ManagedAgentSpawner::admit`) trägt die
        // Wurzel das Programm ihrer Definition; ohne Programm bleibt die
        // Kontextmontage unverändert.
        if let Some(program) = self.root_context_program.clone() {
            session = session.with_context_program(program);
        }
        // Welle 3: jede Modellanfrage der Wurzel fordert höchstens die
        // Ausgabereserve an, die die Auto-Verdichtung vom Fenster abzieht.
        session.set_max_output_tokens(Some(output_reserve));
        if let Some(mode) = self.spec.mode_override {
            session.set_mode(mode);
            // Runde 9, E6: Kinder dieses Baums starten im Modus der Wurzel
            // und folgen späteren Wechseln (`ManagedAgentSpawner::live_mode`).
            if let Some(spawner) = self.spawner.as_ref() {
                spawner.live_mode().publish(mode);
            }
        }

        tracing::info!(
            entry = ?self.spec.entry,
            session_id = %self.root_session_id,
            mode = session.mode().as_str(),
            approval_mode = ?self.approval_mode.get(),
            root_model = self.root_model_id.as_deref().unwrap_or("<none>"),
            context_window = context_window,
            context_window_known = root_limits.known,
            output_reserve = output_reserve,
            "runtime.root_session.created"
        );
        // `SessionConfigured` an alle Beobachter (TUI übernimmt das Modell
        // für den Export).
        session.announce_configured();

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
    use crate::test_support::{TestError, TestResult, ctx};

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
        assert_eq!(
            TurnLimits::from_root_budget(&budget).max_tool_calls,
            u32::MAX
        );
    }

    /// Der Rustdoc von [`default_approval_mode`] sagt „immer `Delegated`“ —
    /// auch Telegram, dort zusätzlich erzwungen (Befund Z2c-12, Runde 3
    /// Welle D: Lesen läuft durch, Schreiben fragt).
    #[test]
    fn every_entry_starts_delegated() {
        for entry in ALL_ENTRIES {
            assert_eq!(
                default_approval_mode(entry),
                ApprovalMode::Delegated,
                "{entry:?}"
            );
        }
    }

    /// Telegram fragt bei jedem Schreiben — auch wenn Global- und
    /// Projekt-Konfiguration `full` verlangen. Kein anderer Einstieg hat einen
    /// erzwungenen Modus.
    #[test]
    fn telegram_approval_mode_is_forced_to_delegated_regardless_of_config() {
        let global = PermissionsSection {
            default_mode: Some("full".to_owned()),
            ..PermissionsSection::default()
        };
        let project = PermissionsSection {
            default_mode: Some("full".to_owned()),
            ..PermissionsSection::default()
        };
        assert_eq!(
            effective_approval_mode(EntryKind::GatewayTelegram, &global, &project),
            ApprovalMode::Delegated
        );
        for entry in ALL_ENTRIES {
            let expected = (entry == EntryKind::GatewayTelegram).then_some(ApprovalMode::Delegated);
            assert_eq!(forced_approval_mode(entry), expected, "{entry:?}");
        }
    }

    #[test]
    fn an_explicit_root_agent_must_be_an_orchestrator_or_worker() -> TestResult {
        let (config, definitions) = builtin()?;
        assert!(resolve_explicit_root_agent(None, &config, &definitions)?.is_none());

        let explorer =
            resolve_explicit_root_agent(Some(role_names::EXPLORER), &config, &definitions)?
                .ok_or(TestError::Missing("explorer"))?;
        assert_eq!(explorer.role(), AgentRoleId::Worker);

        let Err(unknown) =
            resolve_explicit_root_agent(Some("definitely-not-a-role"), &config, &definitions)
        else {
            return Err(TestError::Unexpected(
                "unbekannter Agent muss scheitern".into(),
            ));
        };
        assert!(
            matches!(unknown, RuntimeError::Registry { .. }),
            "{unknown}"
        );

        // Jede eingebaute Definition mit einer anderen Rolle als
        // Root-/Child-Orchestrator oder Worker wird mit einem
        // Konfigurationsfehler abgelehnt.
        let mut rejected = 0_usize;
        for (name, ir) in &definitions {
            let allowed = matches!(
                ir.role(),
                AgentRoleId::RootOrchestrator
                    | AgentRoleId::ChildOrchestrator
                    | AgentRoleId::Worker
            );
            let result = resolve_explicit_root_agent(Some(name), &config, &definitions);
            if allowed {
                assert!(result.is_ok(), "{name}");
            } else {
                rejected += 1;
                assert!(
                    matches!(result, Err(RuntimeError::Config { .. })),
                    "{name}: {:?}",
                    result.err()
                );
            }
        }
        assert!(
            rejected > 0,
            "mindestens die UIA-Helfer tragen eine andere Rolle"
        );
        Ok(())
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
    fn builtin() -> TestResult<(ResolvedConfig, HashMap<String, ExecutableAgentIr>)> {
        let config = ResolvedConfig::default();
        let definitions = lower_agent_definitions(&config).map_err(ctx("Rollen senken"))?;
        Ok((config, definitions))
    }

    #[test]
    fn interactive_entries_require_a_configured_user_interface_agent() -> TestResult {
        let (config, definitions) = builtin()?;
        let Err(error) = resolve_active_uia(EntryKind::Tui, &config, &definitions) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, RuntimeError::Registry { .. }));

        let explorer = definitions
            .get(role_names::EXPLORER)
            .ok_or(TestError::Missing("explorer"))?
            .clone();
        let mut config = config;
        config.harness.active_uia_definition = Some("not-a-uia".to_owned());
        config
            .executable_agents
            .insert("not-a-uia".to_owned(), explorer);
        let Err(error) = resolve_active_uia(EntryKind::Tui, &config, &definitions) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, RuntimeError::Registry { .. }));
        Ok(())
    }

    #[test]
    fn non_interactive_entries_do_not_require_a_uia() -> TestResult {
        let (config, definitions) = builtin()?;
        assert!(
            resolve_active_uia(EntryKind::Doctor, &config, &definitions)
                .map_err(ctx("doctor has no UIA requirement"))?
                .is_none()
        );
        Ok(())
    }

    /// Senkt eine minimale Agentendefinition zur `AgentIr`, wie
    /// [`crate::embedded::EmbeddedAgent::root_ir`] sie liefert — derselbe
    /// Weg wie `crate::embedded::tests::compile`.
    fn compile_ir(source: &str, target: &str) -> TestResult<harw_agent_dsl::ir_v2::AgentIr> {
        use harw_agent_dsl::diagnostics::SourceFile;
        use harw_agent_dsl::ids::DefinitionId;
        use harw_agent_dsl::layers::DefinitionLayer;
        use harw_agent_dsl::lower_v2::{LowerSources, compile_agent};
        let files = vec![SourceFile::new(
            DefinitionLayer::UserGlobal,
            "agents/embedded/definition.toml",
            source,
        )];
        let sources = LowerSources::new(&files);
        let target = DefinitionId::parse(target).map_err(ctx("target id"))?;
        compile_agent(&target, &sources, time::OffsetDateTime::UNIX_EPOCH)
            .map_err(|diagnostics| TestError::Unexpected(format!("compile: {diagnostics}")))
    }

    /// #22 Welle 3: die eingebettete UIA einer personalisierten harw
    /// (`spec.embedded`, `EntryKind::Tui`/`OneShot`) wird genau wie eine
    /// über `harness.active_uia_definition` konfigurierte UIA auf die Rolle
    /// `user-interface` geprüft — [`resolve_embedded_uia`] ist der Kern
    /// dieser Prüfung, den die Montage anstelle von [`resolve_active_uia`]
    /// aufruft, sobald `spec.embedded` gesetzt ist (siehe Aufrufstelle in
    /// [`RuntimeAssembly::builder`]).
    #[test]
    fn resolve_embedded_uia_accepts_user_interface_role_and_rejects_others() -> TestResult {
        let uia_ir = compile_ir(
            "schema = \"harwness.agent/v1\"\n\
             id = \"acme.agent.embedded-uia@1\"\n\
             version = \"1.0.0\"\n\
             role = \"user-interface\"\n\
             specialization = \"terminal-ui\"\n",
            "acme.agent.embedded-uia@1",
        )?;
        let worker_ir = compile_ir(
            "schema = \"harwness.agent/v1\"\n\
             id = \"acme.agent.embedded-worker@1\"\n\
             version = \"1.0.0\"\n\
             role = \"worker\"\n\
             specialization = \"embedded-worker\"\n",
            "acme.agent.embedded-worker@1",
        )?;

        let resolved =
            resolve_embedded_uia(&uia_ir).map_err(ctx("embedded uia should resolve"))?;
        assert_eq!(resolved.role(), AgentRoleId::UserInterface);

        let rejected = resolve_embedded_uia(&worker_ir);
        assert!(
            matches!(rejected, Err(RuntimeError::Registry { .. })),
            "{rejected:?}"
        );
        Ok(())
    }

    // ── Kontextfenster je effektivem Modell (Welle 3) ───────────────────

    fn model_toml(
        id: &str,
        context_window: Option<u64>,
        max_tokens: Option<u64>,
    ) -> harw_config::ModelToml {
        harw_config::ModelToml {
            id: id.to_owned(),
            name: None,
            provider: "local-a".to_owned(),
            aliases: Vec::new(),
            context_window,
            max_tokens,
            prompt_caching: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: harw_config::ModelCapabilitiesToml::default(),
            default_reasoning_effort: None,
            stream: None,
            rate_limit: None,
        }
    }

    #[test]
    fn normalize_model_id_strips_provider_prefix_and_date_suffixes() {
        assert_eq!(
            normalize_model_id("anthropic/Claude-Opus-4-8"),
            "claude-opus-4-8"
        );
        assert_eq!(
            normalize_model_id("claude-haiku-4-5-20251001"),
            "claude-haiku-4-5"
        );
        assert_eq!(normalize_model_id("gpt-4o-2024-08-06"), "gpt-4o");
        assert_eq!(normalize_model_id("mistral-large-2411"), "mistral-large");
        assert_eq!(normalize_model_id("mistral-large-latest"), "mistral-large");
        assert_eq!(
            normalize_model_id("claude-sonnet-5@20260101"),
            "claude-sonnet-5"
        );
        assert_eq!(normalize_model_id("glm-4.6"), "glm-4.6");
        assert_eq!(
            normalize_model_id("llama-3.3-70b-versatile"),
            "llama-3.3-70b-versatile"
        );
    }

    /// Runde 5, Teil H: der echte Aufrufpfad (`model_limits_for` →
    /// `match_catalog_key` über den eingebauten Katalog) kennt Opus 5.5 in
    /// beiden Schreibweisen und liefert dessen Fenster, nicht das
    /// Rückfallfenster und nicht das des Legacy-Modells `claude-opus-5`.
    #[test]
    fn opus_5_5_resolves_through_the_catalog_call_path() -> TestResult {
        let catalog = builtin_catalog();
        let expected = catalog
            .get("claude-opus-5-5")
            .map(|entry| entry.context_window)
            .ok_or(TestError::Missing("Katalogeintrag claude-opus-5-5"))?;
        let config = ResolvedConfig::default();
        for id in ["claude-opus-5-5", "anthropic/claude-opus-5.5"] {
            let key = match_catalog_key(catalog.keys().map(String::as_str), id)
                .ok_or(TestError::Missing("Katalogtreffer für Opus 5.5"))?;
            assert_eq!(
                version_dots_to_dashes(&normalize_model_id(key)),
                "claude-opus-5-5",
                "{id} → {key}"
            );
            let limits = model_limits_for(&config, id);
            assert!(limits.known, "{id} muss bekannt sein");
            assert_eq!(limits.context_window, expected, "{id}");
        }
        Ok(())
    }

    #[test]
    fn match_catalog_key_prefers_exact_then_normalized_then_longest_prefix() {
        // Runde 5, Teil H: Opus 5.5 (Standardmodell) in beiden Schreibweisen
        // gegen die Schlüsselliste — nie der Legacy-Eintrag `claude-opus-5`.
        let opus_keys = ["claude-opus-5", "claude-opus-5-5", "claude-sonnet-5"];
        for id in ["claude-opus-5-5", "anthropic/claude-opus-5.5"] {
            assert_eq!(
                match_catalog_key(opus_keys, id),
                Some("claude-opus-5-5"),
                "{id}"
            );
        }
        let keys = [
            "gpt-4o",
            "gpt-4o-mini",
            "claude-haiku-4-5-20251001",
            "mistral-large-2411",
        ];
        assert_eq!(match_catalog_key(keys, "gpt-4o"), Some("gpt-4o"));
        assert_eq!(
            match_catalog_key(keys, "openai/gpt-4o-mini"),
            Some("gpt-4o-mini")
        );
        assert_eq!(
            match_catalog_key(keys, "claude-haiku-4-5"),
            Some("claude-haiku-4-5-20251001")
        );
        assert_eq!(
            match_catalog_key(keys, "mistral-large-latest"),
            Some("mistral-large-2411")
        );
        // Längster Präfix an einer Trennstelle gewinnt.
        assert_eq!(
            match_catalog_key(keys, "gpt-4o-mini-high"),
            Some("gpt-4o-mini")
        );
        assert_eq!(match_catalog_key(keys, "gpt-4o:thinking"), Some("gpt-4o"));
        // Teil C: Versionspunkt statt Bindestrich (OpenRouter-Schreibweise).
        assert_eq!(
            match_catalog_key(keys, "anthropic/claude-haiku-4.5"),
            Some("claude-haiku-4-5-20251001")
        );
        assert_eq!(
            match_catalog_key(keys, "anthropic/claude-haiku-4.5:beta"),
            Some("claude-haiku-4-5-20251001")
        );
        // Kein Präfix ohne Trennstelle, kein Treffer für Fremdes.
        assert_eq!(match_catalog_key(keys, "gpt-4ox"), None);
        assert_eq!(match_catalog_key(keys, "my-local-model"), None);
        assert_eq!(match_catalog_key(keys, ""), None);
    }

    #[test]
    fn version_dots_become_dashes_only_between_digits() {
        assert_eq!(
            version_dots_to_dashes("claude-sonnet-4.5"),
            "claude-sonnet-4-5"
        );
        assert_eq!(version_dots_to_dashes("gpt-5.4-mini"), "gpt-5-4-mini");
        assert_eq!(version_dots_to_dashes("a.b-1."), "a.b-1.");
    }

    #[test]
    fn dotted_openrouter_ids_resolve_to_the_catalog_window() {
        let config = ResolvedConfig::default();
        let dashed = model_limits_for(&config, "claude-haiku-4-5-20251001");
        let dotted = model_limits_for(&config, "anthropic/claude-haiku-4.5");
        assert!(
            dotted.known,
            "OpenRouter-Schreibweise muss den Katalog treffen"
        );
        assert_eq!(dotted.context_window, dashed.context_window);
    }

    #[test]
    fn unknown_models_fall_back_to_a_conservative_window() {
        let config = ResolvedConfig::default();
        let limits = model_limits_for(&config, "my-local-model");
        assert_eq!(limits.context_window, UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS);
        assert_eq!(limits.context_window, 32_768);
        assert!(!limits.known);
        assert_eq!(
            context_window_for_model(&config, "my-local-model"),
            UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS
        );
        assert_eq!(
            model_limits_or_fallback(&config, None, None).context_window,
            UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS
        );
    }

    /// Der Offline-Echo ist kein echtes Modell: ohne bekanntes Fenster gilt
    /// [`OFFLINE_ECHO_CONTEXT_WINDOW_TOKENS`], während ein unbekanntes Modell
    /// hinter einem echten Provider beim konservativen 32k-Rückfall bleibt.
    #[test]
    fn offline_echo_gets_a_large_window_while_unknown_real_models_keep_32k() {
        const _: () =
            assert!(OFFLINE_ECHO_CONTEXT_WINDOW_TOKENS > UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS);
        let config = ResolvedConfig::default();

        // Echo ohne Modell (SDK `offline_echo`) und mit unbekanntem Modell.
        for (model, fallback) in [(None, None), (Some("my-local-model"), None)] {
            let echo = session_model_limits(&config, true, model, fallback);
            assert_eq!(echo.context_window, OFFLINE_ECHO_CONTEXT_WINDOW_TOKENS);
            assert!(!echo.known);
            assert!(
                echo.output_reserve_tokens() < echo.context_window / 2,
                "Reserve muss deutlich unter dem Fenster bleiben"
            );
        }

        // Echter Provider: unverändert der konservative Rückfall.
        for (model, fallback) in [(None, None), (Some("my-local-model"), None)] {
            let real = session_model_limits(&config, false, model, fallback);
            assert_eq!(real.context_window, UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS);
            assert_eq!(real.context_window, 32_768);
            assert!(!real.known);
        }

        // Ein bekanntes Fenster gewinnt auch beim Echo (Verdichtungstests
        // setzen bewusst kleine Fenster).
        let mut small = ResolvedConfig::default();
        small.models.insert(
            "tiny".to_owned(),
            model_toml("tiny-model", Some(8_000), None),
        );
        let echo_small = session_model_limits(&small, true, None, Some("tiny"));
        assert!(echo_small.known);
        assert_eq!(echo_small.context_window, 8_000);
        let echo_catalog = session_model_limits(&config, true, Some("claude-haiku-4-5"), None);
        assert_eq!(
            echo_catalog.context_window,
            model_limits_for(&config, "claude-haiku-4-5").context_window
        );
    }

    #[test]
    fn catalog_models_resolve_through_provider_prefix_and_date_suffix() -> TestResult {
        let config = ResolvedConfig::default();
        let exact = model_limits_for(&config, "claude-haiku-4-5-20251001");
        assert!(exact.known, "Katalogmodell muss bekannt sein");
        let aliased = model_limits_for(&config, "anthropic/claude-haiku-4-5");
        assert!(aliased.known);
        assert_eq!(aliased.context_window, exact.context_window);
        assert_eq!(aliased.catalog_max_output, exact.catalog_max_output);
        let descriptor = harw_model_catalog::descriptor::bootstrap_descriptors()
            .into_iter()
            .find(|d| d.model.as_str() == "claude-haiku-4-5-20251001")
            .ok_or(TestError::Missing("claude-haiku im Katalog"))?;
        assert_eq!(exact.context_window, u64::from(descriptor.context_window));
        Ok(())
    }

    #[test]
    fn configured_model_entries_win_and_carry_max_tokens() {
        let mut config = ResolvedConfig::default();
        let mut entry = model_toml("local-model", Some(100_000), Some(8_000));
        entry.aliases.push("lm".to_owned());
        entry.reasoning = true;
        config.models.insert("local".to_owned(), entry);
        for id in ["local", "local-model", "lm"] {
            let limits = model_limits_for(&config, id);
            assert_eq!(limits.context_window, 100_000, "{id}");
            assert_eq!(limits.configured_max_output, Some(8_000), "{id}");
            assert!(limits.thinking, "{id}");
            assert!(limits.known, "{id}");
        }

        // Konfigurationseintrag ohne Fenster, dessen `id` ein Katalogmodell
        // ist: das Katalogfenster greift, `max_tokens` bleibt konfiguriert.
        config.models.insert(
            "fast".to_owned(),
            model_toml("claude-haiku-4-5", None, Some(4_000)),
        );
        let limits = model_limits_for(&config, "fast");
        assert!(limits.known);
        assert_eq!(
            limits.context_window,
            model_limits_for(&config, "claude-haiku-4-5-20251001").context_window
        );
        assert_eq!(limits.configured_max_output, Some(4_000));
    }

    /// Runde 7, Teil L3: Ein Modell eines lokalen Providers bekommt nie das
    /// Hersteller-Fenster aus dem Katalog — nur das konfigurierte bzw.
    /// gescannte Fenster, sonst das Rückfallfenster.
    #[test]
    fn local_models_get_no_vendor_catalog_window() {
        let mut config = two_provider_config();
        config.models.insert(
            "haiku-local".to_owned(),
            model_toml("claude-haiku-4-5", None, None),
        );
        let limits = model_limits_for(&config, "haiku-local");
        assert!(
            !limits.known,
            "lokales Modell darf den Katalog nicht treffen"
        );
        assert_eq!(limits.context_window, UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS);
        assert_eq!(limits.catalog_max_output, None);

        // Das gescannte Fenster gilt.
        config.models.insert(
            "haiku-local".to_owned(),
            model_toml("claude-haiku-4-5", Some(16_384), None),
        );
        let limits = model_limits_for(&config, "haiku-local");
        assert!(limits.known);
        assert_eq!(limits.context_window, 16_384);

        // Dasselbe Modell ohne lokalen Provider trifft den Katalog weiterhin.
        let cloud = model_limits_for(&ResolvedConfig::default(), "claude-haiku-4-5");
        assert!(cloud.known);
        assert_ne!(cloud.context_window, UNKNOWN_MODEL_CONTEXT_WINDOW_TOKENS);
    }

    #[test]
    fn effective_root_model_follows_the_uia_pair_only_for_uia_roots() -> TestResult {
        let mut config = two_provider_config();
        assert_eq!(
            effective_root_model_id(&config, true).as_deref(),
            Some("local-a-model"),
            "ohne UIA-Paar gilt das Vorgabemodell"
        );
        config.harness.uia_provider = Some("local-b".to_owned());
        config.harness.uia_model = Some("local-b-model".to_owned());
        assert_eq!(
            effective_root_model_id(&config, true).as_deref(),
            Some("local-b-model")
        );
        assert_eq!(
            effective_root_model_id(&config, false).as_deref(),
            Some("local-a-model"),
            "eine Nicht-UIA-Wurzel ignoriert das UIA-Paar"
        );
        config
            .providers
            .get_mut("local-b")
            .ok_or(TestError::Missing("local-b"))?
            .enabled = false;
        assert_eq!(
            effective_root_model_id(&config, true).as_deref(),
            Some("local-a-model"),
            "deaktivierter UIA-Provider fällt auf das Vorgabemodell zurück"
        );
        Ok(())
    }

    #[test]
    fn compaction_ceiling_defaults_to_the_core_constant() {
        let mut config = ResolvedConfig::default();
        assert_eq!(
            configured_compaction_ceiling(&config),
            harw_core::DEFAULT_ABSOLUTE_CEILING_TOKENS
        );
        config.harness.compaction.absolute_ceiling_tokens = Some(123_456);
        assert_eq!(configured_compaction_ceiling(&config), 123_456);
    }

    // ── split_root_and_uia_worker_models (Welle 3a, Teil A) ─────────────

    /// Ein minimaler [`RuntimeSpec`], nur für den Modell-Bau relevant — kein
    /// echtes Home/Cwd wird je gelesen, solange `ModelSource::Configured`
    /// nicht mit einem tatsächlich konfigurierten Provider zusammentrifft.
    fn model_spec() -> RuntimeSpec {
        RuntimeSpec {
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
            approval_override: None,
            model_override: None,
            embedded: None,
            child_backend: None,
        }
    }

    /// Konfiguration mit zwei baubaren, netzlosen Loopback-Providern
    /// (`"local-a"` als Vorgabe, `"local-b"` als abweichender UIA-Provider) —
    /// dasselbe Muster wie `model::tests::two_provider_config`, hier
    /// dupliziert statt importiert (jenes ist privat zu `model.rs`).
    fn two_provider_config() -> ResolvedConfig {
        fn loopback_provider(name: &str) -> harw_config::ProviderToml {
            harw_config::ProviderToml {
                stream: None,
                name: name.to_owned(),
                api: "openai-chat".to_owned(),
                base_url: "http://127.0.0.1:11434/v1".to_owned(),
                auth: None,
                auth_header: Some("none".to_owned()),
                api_key: None,
                headers: HashMap::new(),
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

        let mut config = ResolvedConfig {
            harness: harw_config::HarnessConfig {
                default_provider: Some("local-a".to_owned()),
                default_model: Some("local-a-model".to_owned()),
                ..harw_config::HarnessConfig::default()
            },
            ..ResolvedConfig::default()
        };
        config
            .providers
            .insert("local-a".to_owned(), loopback_provider("local-a"));
        config
            .providers
            .insert("local-b".to_owned(), loopback_provider("local-b"));
        config
    }

    // ── resolve_root_uia_reasoning_effort_defaults (Welle 8) ────────────

    #[test]
    fn resolve_root_uia_reasoning_effort_defaults_is_none_triple_without_a_uia() {
        let config = two_provider_config();
        assert_eq!(
            resolve_root_uia_reasoning_effort_defaults(&config, None),
            (None, None, None),
            "ohne UIA gibt es keine Wurzel-UIA-Effort-Vorgabe aufzulösen"
        );
    }

    #[test]
    fn resolve_root_uia_reasoning_effort_defaults_prefers_uia_pair_when_usable() -> TestResult {
        let mut config = two_provider_config();
        // `local-a` (der Vorgabe-Provider) trägt einen anderen Effort als
        // `local-b` (der abweichende UIA-Provider) — nur `local-b` darf
        // gewinnen, wenn `uia_provider`/`uia_model` beide gesetzt und
        // `uia_provider` nutzbar ist.
        config
            .providers
            .get_mut("local-a")
            .ok_or(TestError::Missing("local-a"))?
            .default_reasoning_effort = Some(harw_types::ReasoningEffort::Low);
        config
            .providers
            .get_mut("local-b")
            .ok_or(TestError::Missing("local-b"))?
            .default_reasoning_effort = Some(harw_types::ReasoningEffort::Xhigh);
        config.harness.uia_provider = Some("local-b".to_owned());
        config.harness.uia_model = Some("local-b-model".to_owned());

        let (_config_for_definitions, definitions) = builtin()?;
        let uia_ir = definitions
            .get(role_names::EXPLORER)
            .ok_or(TestError::Missing("builtin explorer definition"))?;

        let (provider_default, model_default, _agent_default) =
            resolve_root_uia_reasoning_effort_defaults(&config, Some(uia_ir));
        assert_eq!(provider_default, Some(harw_types::ReasoningEffort::Xhigh));
        assert_eq!(model_default, None, "local-b-model ist nicht katalogisiert");
        Ok(())
    }

    #[test]
    fn resolve_root_uia_reasoning_effort_defaults_falls_back_to_default_pair_without_a_usable_uia_pair()
    -> TestResult {
        let mut config = two_provider_config();
        config
            .providers
            .get_mut("local-a")
            .ok_or(TestError::Missing("local-a"))?
            .default_reasoning_effort = Some(harw_types::ReasoningEffort::High);
        // Kein `uia_provider`/`uia_model` gesetzt: die Auflösung muss auf
        // `default_provider` (`local-a`) zurückfallen.
        let (_config_for_definitions, definitions) = builtin()?;
        let uia_ir = definitions
            .get(role_names::EXPLORER)
            .ok_or(TestError::Missing("builtin explorer definition"))?;

        let (provider_default, _model_default, _agent_default) =
            resolve_root_uia_reasoning_effort_defaults(&config, Some(uia_ir));
        assert_eq!(provider_default, Some(harw_types::ReasoningEffort::High));
        Ok(())
    }

    #[test]
    fn resolve_root_uia_reasoning_effort_defaults_carries_the_agent_label() -> TestResult {
        let config = two_provider_config();
        let (_config_for_definitions, definitions) = builtin()?;
        let uia_ir = definitions
            .get(role_names::EXPLORER)
            .ok_or(TestError::Missing("builtin explorer definition"))?;
        let (_provider_default, _model_default, agent_default) =
            resolve_root_uia_reasoning_effort_defaults(&config, Some(uia_ir));
        assert_eq!(
            agent_default.as_deref(),
            uia_ir.reasoning_effort(),
            "das dritte Feld ist ExecutableAgentIr::reasoning_effort() der UIA, als \
             eigenständiger String geklont"
        );
        Ok(())
    }

    #[test]
    fn builder_splits_model_and_uia_worker_model_only_when_a_uia_is_active() -> TestResult {
        let spec = model_spec();
        let config = ResolvedConfig::default();
        let default_tree_model: Arc<dyn ModelProvider> =
            Arc::new(harw_core::EchoModelProvider::new("echo: root"));

        let (model, uia_worker_model, uia_load_registry) = split_root_and_uia_worker_models(
            &spec,
            &config,
            false,
            &default_tree_model,
            None,
            None,
        )
        .map_err(ctx("without an active uia the split never fails"))?;
        assert!(
            uia_load_registry.is_empty(),
            "without an active uia there is no dedicated client, so no dedicated registry"
        );

        assert!(
            Arc::ptr_eq(&model, &default_tree_model),
            "without an active uia, the root model must stay the default tree model"
        );
        assert!(
            Arc::ptr_eq(&uia_worker_model, &default_tree_model),
            "without an active uia, the uia-worker family must also fall back to the \
             default tree model"
        );
        Ok(())
    }

    #[test]
    fn builder_gives_the_uia_session_its_own_provider_client_when_configured() -> TestResult {
        let spec = model_spec();
        let mut config = two_provider_config();
        config.harness.uia_provider = Some("local-b".to_owned());
        config.harness.uia_model = Some("local-b-model".to_owned());
        let default_tree_model: Arc<dyn ModelProvider> =
            crate::model::build_root_model(&spec, &config, ModelSource::Configured)
                .map_err(ctx("default provider (local-a) must build"))?;

        // Any `Some` is enough to flip the branch — the split function only
        // checks presence, never the IR's own content.
        let (_config_for_definitions, definitions) = builtin()?;
        let uia_ir = definitions
            .get(role_names::EXPLORER)
            .ok_or(TestError::Missing("builtin explorer definition"))?;

        let (model, uia_worker_model, _uia_load_registry) = split_root_and_uia_worker_models(
            &spec,
            &config,
            true,
            &default_tree_model,
            Some(uia_ir),
            None,
        )
        .map_err(ctx(
            "uia provider (local-b) must build as a dedicated client",
        ))?;

        assert!(
            !Arc::ptr_eq(&model, &default_tree_model),
            "an active uia with its own configured provider must get a dedicated client, \
             not the default tree model"
        );
        assert!(
            !Arc::ptr_eq(&uia_worker_model, &default_tree_model),
            "the uia-worker family must be derived from the uia client, not the default \
             tree model, once the uia provider differs"
        );
        Ok(())
    }

    #[test]
    fn an_unknown_agent_name_fails_closed() -> TestResult {
        let (config, definitions) = builtin()?;
        let Err(error) = resolve_active_agent(Some("definitely-not-a-role"), &config, &definitions)
        else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, RuntimeError::Registry { .. }));
        Ok(())
    }

    #[test]
    fn a_known_agent_name_resolves_and_narrows_the_activation() -> TestResult {
        let (config, definitions) = builtin()?;
        let ir = resolve_active_agent(Some(role_names::EXPLORER), &config, &definitions)
            .map_err(ctx("explorer resolves"))?
            .ok_or(TestError::Missing("explorer exists"))?;
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
        Ok(())
    }

    #[test]
    fn without_an_agent_the_activation_cuts_nothing() -> TestResult {
        let (config, definitions) = builtin()?;
        assert!(
            resolve_active_agent(None, &config, &definitions)
                .map_err(ctx("none"))?
                .is_none()
        );
        assert_eq!(root_activation(None).profile(), ToolProfile::default());
        Ok(())
    }

    /// Z2c-05: Eine in `[agents]` konfigurierte Rolle löst auf — und geht der
    /// gleichnamigen eingebauten vor.
    #[test]
    fn a_configured_agent_wins_over_the_builtin_one() -> TestResult {
        let (mut config, definitions) = builtin()?;
        let explorer = definitions
            .get(role_names::EXPLORER)
            .ok_or(TestError::Missing("explorer exists"))?
            .clone();
        config
            .executable_agents
            .insert("hausrolle".to_owned(), explorer);

        let expected = definitions
            .get(role_names::EXPLORER)
            .ok_or(TestError::Missing("explorer"))?
            .tool_surface()
            .admitted()
            .to_vec();
        let ir = resolve_active_agent(Some("hausrolle"), &config, &definitions)
            .map_err(ctx("konfigurierte Rolle löst auf"))?
            .ok_or(TestError::Missing("sie existiert"))?;
        assert_eq!(ir.tool_surface().admitted(), expected.as_slice());
        Ok(())
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
            approval_override: None,
            model_override: None,
            embedded: None,
            child_backend: None,
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

    fn build_fixture() -> TestResult<BuildFixture> {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&home).map_err(ctx("home"))?;
        std::fs::create_dir_all(&project).map_err(ctx("project"))?;
        // Projekt-Marker, damit `discover_project` genau hier stehen bleibt.
        std::fs::write(project.join("Cargo.toml"), "[workspace]\n").map_err(ctx("marker"))?;
        write_fixture_uia(&home)?;
        Ok(BuildFixture {
            _dir: dir,
            home,
            project,
        })
    }

    /// Wie [`build_fixture`], mit [`write_fixture_uia_with_default_provider_effort`]
    /// statt [`write_fixture_uia`].
    fn build_fixture_with_default_provider_effort(
        provider_effort: &str,
    ) -> TestResult<BuildFixture> {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("home");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&home).map_err(ctx("home"))?;
        std::fs::create_dir_all(&project).map_err(ctx("project"))?;
        std::fs::write(project.join("Cargo.toml"), "[workspace]\n").map_err(ctx("marker"))?;
        write_fixture_uia_with_default_provider_effort(&home, provider_effort)?;
        Ok(BuildFixture {
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
    /// Seit dem UIA-Vertrag (siehe `resolve_active_uia`) montieren `EntryKind::Tui`
    /// und `EntryKind::OneShot` nur mit einer konfigurierten UIA
    /// (fail-closed, `RuntimeError::Registry`). Test-Fixtures müssen deshalb
    /// selbst eine bereitstellen, statt implizit auf einen Bootstrap
    /// außerhalb dieser Crate (`harw-cli/src/uia_bootstrap.rs`) zu vertrauen.
    /// Layout und Inhalt spiegeln exakt `write_generated_uia` dort:
    /// `<home>/profiles/default/agents/fixture-uia/definition.toml` plus
    /// `<home>/profiles/default/config.toml` mit
    /// `active_uia_definition = "<id>"` — das aktive Profil ohne
    /// `active_profile`-Datei ist `"default"`
    /// (`harw_home::active_profile_name`).
    fn write_fixture_uia(home: &std::path::Path) -> TestResult {
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

    /// Wie [`write_fixture_uia`], zusätzlich mit einem `default_provider`/
    /// `default_model`-Paar, dessen Provider-Datei einen
    /// `default_reasoning_effort` trägt (Welle 8: Rangfolge Provider &gt;
    /// Modell &gt; Agent &gt; Rolle) — für Tests von
    /// [`resolve_root_uia_reasoning_effort_defaults`] über den vollen
    /// [`RuntimeAssembly::new_root_session`]-Pfad, nicht nur die reine
    /// Funktion.
    fn write_fixture_uia_with_default_provider_effort(
        home: &std::path::Path,
        provider_effort: &str,
    ) -> TestResult {
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
            "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n\
             default_provider = \"fixture-provider\"\n\
             default_model = \"fixture-model\"\n",
        )
        .map_err(ctx("fixture profile config"))?;
        let providers_dir = profile_dir.join("providers");
        std::fs::create_dir_all(&providers_dir).map_err(ctx("providers dir"))?;
        std::fs::write(
            providers_dir.join("fixture-provider.toml"),
            format!(
                "name = \"fixture-provider\"\n\
                 api = \"openai-chat\"\n\
                 base_url = \"http://127.0.0.1:11434/v1\"\n\
                 auth_header = \"none\"\n\
                 default_reasoning_effort = \"{provider_effort}\"\n"
            ),
        )
        .map_err(ctx("fixture provider toml"))?;
        let models_dir = profile_dir.join("models");
        std::fs::create_dir_all(&models_dir).map_err(ctx("models dir"))?;
        std::fs::write(
            models_dir.join("fixture-model.toml"),
            "id = \"fixture-model\"\nprovider = \"fixture-provider\"\n",
        )
        .map_err(ctx("fixture model toml"))
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
            approval_override: None,
            model_override: None,
            embedded: None,
            child_backend: None,
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

    /// Jede [`harw_authority::Permission`] — die weiteste denkbare Obergrenze.
    fn every_permission() -> PermissionSet {
        use harw_authority::Permission;
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
    fn test_plan_services_accessor_returns_builder_input() -> TestResult {
        let fixture = build_fixture()?;
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
            .map_err(ctx("LocalEcho montiert"))?;

        let Some(returned) = assembly.plan_services() else {
            return Err(TestError::Missing(
                "plan_services() muss die Builder-Eingabe liefern",
            ));
        };
        assert!(
            Arc::ptr_eq(&returned.findings, &findings),
            "plan_services() liefert genau den übergebenen Wert"
        );
        let Some(via_services) = assembly.services().plan() else {
            return Err(TestError::Missing(
                "RuntimeServices::plan() muss dieselbe Planungsfläche liefern",
            ));
        };
        assert!(Arc::ptr_eq(&via_services.findings, &findings));
        Ok(())
    }

    #[test]
    fn test_memory_accessor_none_without_memory() -> TestResult {
        let fixture = build_fixture()?;
        let assembly = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .map_err(ctx("LocalEcho montiert"))?;
        assert!(assembly.memory().is_none());
        assert!(assembly.services().memory().is_none());
        assert!(assembly.plan_services().is_none());
        Ok(())
    }

    /// Plan Teil B3: die Root-`ServiceMap` trägt `Arc<HostPermitHandles>` mit
    /// demselben Ledger wie [`RuntimeAssembly::host_permit_ledger`] — kein
    /// zweiter, unabhängig instanziierter Ledger.
    #[test]
    fn test_root_service_map_shares_the_host_permit_ledger_with_the_assembly() -> TestResult {
        let fixture = build_fixture()?;
        let assembly = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .map_err(ctx("LocalEcho montiert"))?;

        let map = assembly.services().service_map(ServiceSurface::Slash);
        let Some(handles) = map.get::<Arc<HostPermitHandles>>() else {
            return Err(TestError::Missing(
                "Root-ServiceMap muss HostPermitHandles tragen (Plan Teil B3)",
            ));
        };
        assert!(
            Arc::ptr_eq(&handles.ledger, assembly.host_permit_ledger()),
            "HostPermitHandles.ledger muss derselbe Arc wie assembly.host_permit_ledger() sein"
        );
        assert!(
            Arc::ptr_eq(&handles.registry, assembly.host_permit_session_registry()),
            "HostPermitHandles.registry muss derselbe Arc wie \
             assembly.host_permit_session_registry() sein"
        );
        Ok(())
    }

    /// Addendum B: eine Montage in einem frischen Tempdir-Projekt öffnet die
    /// Erfassungsfläche best-effort und hängt daraus einen
    /// [`crate::memory_wiring::MemoryCaptureObserver`] an die Wurzelsitzung.
    #[test]
    fn test_root_session_gets_a_memory_capture_observer_when_capture_opens() -> TestResult {
        let fixture = build_fixture()?;
        let assembly = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .map_err(ctx("LocalEcho montiert"))?;

        let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
        let root = assembly
            .new_root_session(
                assembly.root_session_id().clone(),
                events,
                turn_events,
                None,
            )
            .map_err(ctx("Wurzelsitzung entsteht"))?;

        assert!(
            root.session.tool_outcome_observer().is_some(),
            "eine erfolgreich geöffnete Erfassungsfläche muss die Wurzelsitzung \
             mit einem ToolOutcomeObserver verdrahten"
        );
        Ok(())
    }

    /// Welle 8: eine UIA-Wurzel ohne explizites `spec.reasoning_effort`
    /// übernimmt `default_provider`s `default_reasoning_effort`, statt
    /// unverändert `role_effort_weights.uia` (`High`) zu bleiben — die
    /// Provider-Ebene steht in der Rangfolge Provider > Modell > Agent > Rolle
    /// über der Rollen-Ebene.
    #[test]
    fn test_root_uia_session_reasoning_effort_prefers_provider_default_over_role_weight()
    -> TestResult {
        let fixture = build_fixture_with_default_provider_effort("xhigh")?;
        let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let assembly = fixture_builder(EntryKind::Tui, &fixture)
            .session_events(events)
            .build()
            .map_err(ctx("Tui montiert"))?;

        let (session_events, _session_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
        let root = assembly
            .new_root_session(
                assembly.root_session_id().clone(),
                session_events,
                turn_events,
                None,
            )
            .map_err(ctx("UIA-Wurzelsitzung entsteht"))?;

        assert_eq!(
            root.session.reasoning_effort(),
            Some(harw_types::ReasoningEffort::Xhigh),
            "der `default_provider`-Vorgabewert (\"xhigh\") muss vor \
             `role_effort_weights.uia` (High) gewinnen"
        );
        Ok(())
    }

    /// Runde 5, Teil B: nur eine TUI-Montage hat einen sudo-Fragekanal
    /// (take-once); jeder andere Einstieg hat keinen — dort wird
    /// `host.sudo_exec` nie registriert (fail-closed).
    #[test]
    fn test_sudo_prompt_channel_exists_only_for_the_tui_entry() -> TestResult {
        let fixture = build_fixture()?;
        let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let tui = fixture_builder(EntryKind::Tui, &fixture)
            .session_events(events)
            .build()
            .map_err(ctx("Tui montiert"))?;
        assert!(tui.take_sudo_prompts().is_some(), "TUI bekommt den Kanal");
        assert!(tui.take_sudo_prompts().is_none(), "nur einmal abholbar");

        let echo = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .map_err(ctx("LocalEcho montiert"))?;
        assert!(
            echo.take_sudo_prompts().is_none(),
            "Nicht-TUI-Einstiege haben keinen sudo-Kanal"
        );
        Ok(())
    }

    /// #22 Welle 3C: ein gesetztes `RuntimeSpec::child_backend` erreicht den
    /// gebauten `ManagedAgentSpawner`. `ManagedAgentSpawner::child_backend()`
    /// ist `pub(crate)` zu `harw-core` und von hier nicht lesbar, deshalb
    /// prüft dieser Test über die Referenzzählung des `Arc`s: hält der
    /// Spawner eine eigene Kopie, bleibt `strong_count` über den ursprünglich
    /// gebauten Wert hinaus erhöht, solange `assembly` (und damit sein
    /// Spawner) lebt.
    #[test]
    fn test_child_backend_reaches_the_built_spawner() -> TestResult {
        struct FakeChildBackend;
        impl harw_core::child_backend::ChildBackend for FakeChildBackend {
            fn run<'a>(
                &'a self,
                _spec: harw_core::child_backend::ChildRunSpec,
                _io: &'a dyn harw_core::child_backend::ChildIo,
            ) -> harw_core::child_backend::ChildBackendFuture<'a> {
                Box::pin(async {
                    harw_core::child_backend::ChildRunOutcome {
                        status: harw_core::child_backend::ChildRunStatus::Completed,
                        text: None,
                        usage: harw_core::ChildUsage::default(),
                        continuation: None,
                    }
                })
            }
        }

        let fixture = build_fixture()?;
        let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let backend: Arc<dyn harw_core::child_backend::ChildBackend> = Arc::new(FakeChildBackend);
        let mut builder = fixture_builder(EntryKind::Tui, &fixture).session_events(events);
        builder.spec.child_backend = Some(ChildBackendHandle(Arc::clone(&backend)));
        let strong_before = Arc::strong_count(&backend);

        let assembly = builder
            .build()
            .map_err(ctx("Tui mit ChildBackend montiert"))?;

        assert!(
            assembly.spawner().is_some(),
            "EntryKind::Tui montiert einen Spawner (SpawnerPolicy::BuiltinRoles)"
        );
        assert!(
            Arc::strong_count(&backend) > strong_before,
            "der gebaute ManagedAgentSpawner muss eine eigene Arc-Referenz auf \
             das ChildBackend halten"
        );
        Ok(())
    }

    /// Runde 5, Teil F: die Plan-Werkzeuge hängen an der Wurzel der vollen
    /// Modell-Werkzeugfläche, die Wurzel ist gebunden (Kinder nie), und nur
    /// die TUI hat einen Plan-Fragekanal (take-once). Ohne Kanal antworten
    /// `plan.exit`/`ask_user` fail-closed (siehe `harw-tool-plan`).
    #[test]
    fn test_plan_tools_are_root_only_and_the_ui_channel_exists_only_for_the_tui() -> TestResult {
        let fixture = build_fixture()?;
        let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let tui = fixture_builder(EntryKind::Tui, &fixture)
            .session_events(events)
            .build()
            .map_err(ctx("Tui montiert"))?;
        let tools = tui.rights_snapshot().tools;
        for name in harw_tool_plan::PlanToolProvider::TOOL_NAMES {
            assert!(
                tools.iter().any(|tool| tool == name),
                "TUI-Wurzel muss {name} führen: {tools:?}"
            );
        }
        assert!(
            tui.plan_session().is_root(tui.root_session_id().as_str()),
            "die Wurzel ist gebunden"
        );
        assert!(
            !tui.plan_session().is_root(SessionId::new().as_str()),
            "jede andere Sitzung (Kind) ist es nicht"
        );
        assert!(!tui.plan_session().lock().is_locked());
        assert!(
            tui.take_plan_ui_requests().is_some(),
            "TUI bekommt den Kanal"
        );
        assert!(tui.take_plan_ui_requests().is_none(), "nur einmal abholbar");

        let echo = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .map_err(ctx("LocalEcho montiert"))?;
        assert!(echo.take_plan_ui_requests().is_none());
        Ok(())
    }

    /// Plan R9, Teil F: nur die TUI bekommt eine Job-Verwaltung; ihre
    /// Wurzel führt `job.*` neben `shell.exec`, die Slash-Fläche trägt die
    /// Verwaltung für `/jobs`, und der Router ist an die Wurzel gebunden.
    #[test]
    fn test_jobs_exist_only_for_the_tui_and_follow_shell_exec() -> TestResult {
        let fixture = build_fixture()?;
        let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let tui = fixture_builder(EntryKind::Tui, &fixture)
            .session_events(events)
            .build()
            .map_err(ctx("Tui montiert"))?;
        let jobs = tui
            .session_jobs()
            .ok_or(TestError::Missing("TUI hat eine Job-Verwaltung"))?;
        assert_eq!(jobs.router.root(), Some(tui.root_session_id()));
        let tools = tui.rights_snapshot().tools;
        if tools.iter().any(|tool| tool == "shell.exec") {
            for name in harw_tool_job::JOB_TOOL_NAMES {
                assert!(
                    tools.iter().any(|tool| tool == name),
                    "TUI-Wurzel muss {name} führen: {tools:?}"
                );
            }
        }
        assert!(
            tui.services()
                .service_map(ServiceSurface::Slash)
                .get::<Arc<harw_tool_job::JobManager>>()
                .is_some(),
            "/jobs findet die Verwaltung"
        );

        let echo = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .map_err(ctx("LocalEcho montiert"))?;
        assert!(echo.session_jobs().is_none());
        assert!(
            !echo
                .rights_snapshot()
                .tools
                .iter()
                .any(|tool| tool.starts_with("job."))
        );
        Ok(())
    }

    /// Regressionstest: ohne konfigurierten Provider-/Modell-/Agenten-
    /// Standard bleibt die UIA-Wurzel unverändert bei `role_effort_weights.uia`
    /// (Addendum F+G, bisheriges Verhalten).
    #[test]
    fn test_root_uia_session_reasoning_effort_falls_back_to_role_weight_without_any_default()
    -> TestResult {
        let fixture = build_fixture()?;
        let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let assembly = fixture_builder(EntryKind::Tui, &fixture)
            .session_events(events)
            .build()
            .map_err(ctx("Tui montiert"))?;

        let (session_events, _session_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
        let root = assembly
            .new_root_session(
                assembly.root_session_id().clone(),
                session_events,
                turn_events,
                None,
            )
            .map_err(ctx("UIA-Wurzelsitzung entsteht"))?;

        assert_eq!(
            root.session.reasoning_effort(),
            Some(RoleEffortWeights::default().uia),
            "ohne jede konfigurierte Ebene bleibt die UIA-Wurzel beim Rollengewicht"
        );
        Ok(())
    }

    /// Eine explizite Live-Einstellung (`RuntimeSpec::reasoning_effort`)
    /// steht laut Nutzerentscheidung über der gesamten Rangfolge Provider >
    /// Modell > Agent > Rolle — auch über einem konfigurierten
    /// `default_provider`.
    #[test]
    fn test_root_uia_session_reasoning_effort_explicit_spec_wins_over_provider_default()
    -> TestResult {
        let fixture = build_fixture_with_default_provider_effort("xhigh")?;
        let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let mut builder = fixture_builder(EntryKind::Tui, &fixture).session_events(events);
        builder.spec.reasoning_effort = Some(harw_types::ReasoningEffort::Low);
        let assembly = builder.build().map_err(ctx("Tui montiert"))?;

        let (session_events, _session_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel::<TurnEvent>();
        let root = assembly
            .new_root_session(
                assembly.root_session_id().clone(),
                session_events,
                turn_events,
                None,
            )
            .map_err(ctx("UIA-Wurzelsitzung entsteht"))?;

        assert_eq!(
            root.session.reasoning_effort(),
            Some(harw_types::ReasoningEffort::Low),
            "eine explizite Live-Einstellung steht über der gesamten Rangfolge"
        );
        Ok(())
    }

    #[test]
    fn test_approval_mode_accessor_shares_the_services_cell() -> TestResult {
        let fixture = build_fixture()?;
        let assembly = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .map_err(ctx("LocalEcho montiert"))?;
        assert_eq!(assembly.approval_mode().get(), ApprovalMode::Delegated);
        assembly.approval_mode().set(ApprovalMode::AlwaysAsk);
        assert_eq!(
            assembly.services().approval_mode().get(),
            ApprovalMode::AlwaysAsk,
            "approval_mode() ist dieselbe Zelle wie in den Service-Maps"
        );
        Ok(())
    }

    #[test]
    fn test_narrowing_readonly_on_full_entry_drops_write_tools() -> TestResult {
        let fixture = build_fixture()?;
        let baseline = fixture_builder(EntryKind::LocalEcho, &fixture)
            .build()
            .map_err(ctx("LocalEcho montiert"))?
            .rights_snapshot();
        assert!(baseline.tools.iter().any(|tool| tool == "fs.write"));
        // Runde 5, Teil H: `fs.edit` (Teil D) steht überall, wo `fs.write` steht.
        assert!(baseline.tools.iter().any(|tool| tool == "fs.edit"));
        assert!(baseline.tools.iter().any(|tool| tool == "shell.exec"));

        let narrowed = fixture_builder(EntryKind::LocalEcho, &fixture)
            .narrowing(RuntimeNarrowing {
                registry_profile: RegistryProfile::ReadOnlyExplore,
                identity: IdentityOverrides::default(),
                permissions: PermissionSet::from_policy([
                    harw_authority::Permission::ReadWorkspace,
                ]),
                workspace_root: None,
            })
            .build()
            .map_err(ctx("Full → ReadOnlyExplore ist zugelassen"))?;
        let snapshot = narrowed.rights_snapshot();

        assert!(!snapshot.tools.iter().any(|tool| tool == "fs.write"));
        // Runde 5, Teil H: die Verengung auf Lesen entfernt auch `fs.edit`.
        assert!(!snapshot.tools.iter().any(|tool| tool == "fs.edit"));
        assert!(!snapshot.tools.iter().any(|tool| tool == "shell.exec"));
        assert!(snapshot.tools.iter().any(|tool| tool == "fs.read"));
        // Neben dem Profil darf die Wurzel nur die sitzungsgebundenen
        // Wissenswerkzeuge führen (Schritt 12a–12a''': Workbench, Palace,
        // Kanban, Diary; Rechteklasse `ReadWorkspace`, keine
        // Workspace-Schreibrechte).
        let read_only = RegistryProfile::ReadOnlyExplore.registered_tool_names();
        let knowledge: Vec<&str> = [
            harw_registry_defaults::WorkbenchToolProvider::TOOL_NAMES,
            harw_registry_defaults::WorkbenchReadToolProvider::TOOL_NAMES,
            harw_registry_defaults::PalaceToolProvider::TOOL_NAMES,
            harw_registry_defaults::KanbanReadToolProvider::TOOL_NAMES,
            harw_registry_defaults::DiaryToolProvider::TOOL_NAMES,
            // Plan R9, Teil A: der lesende Skill-Katalog (keine Rechteklasse).
            harw_registry_defaults::SkillCatalogToolProvider::TOOL_NAMES,
        ]
        .concat();
        assert!(
            snapshot.tools.iter().all(|tool| {
                read_only.contains(&tool.as_str()) || knowledge.contains(&tool.as_str())
            }),
            "nur Werkzeuge des Profils ReadOnlyExplore (plus Wissenswerkzeuge): {:?}",
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
        Ok(())
    }

    #[test]
    fn test_narrowing_rejects_full_on_notools_entry() -> TestResult {
        let fixture = build_fixture()?;
        let Err(error) = fixture_builder(EntryKind::JobPrompt, &fixture)
            .narrowing(RuntimeNarrowing {
                registry_profile: RegistryProfile::Full,
                identity: IdentityOverrides::default(),
                permissions: every_permission(),
                workspace_root: None,
            })
            .build()
        else {
            return Err(TestError::Unexpected(
                "NoTools → Full muss abgelehnt werden".into(),
            ));
        };
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
        for requested in [
            Full,
            ReadOnlyExplore,
            NoTools,
            RegistryProfile::WorkspaceEdit,
        ] {
            assert_eq!(
                narrowed_registry_profile(entry, Full, requested).ok(),
                Some(requested)
            );
        }
        // `WorkspaceEdit` (Telegram) narrowt nur auf sich selbst oder auf
        // `NoTools`; kein anderes Profil darf zu ihm aufweiten.
        let telegram = EntryKind::GatewayTelegram;
        for requested in RegistryProfile::ALL {
            let allowed = matches!(requested, RegistryProfile::WorkspaceEdit | NoTools);
            assert_eq!(
                narrowed_registry_profile(telegram, RegistryProfile::WorkspaceEdit, *requested)
                    .is_ok(),
                allowed,
                "WorkspaceEdit → {requested:?}"
            );
        }
        for entry_profile in RegistryProfile::ALL {
            if matches!(entry_profile, Full | RegistryProfile::WorkspaceEdit) {
                continue;
            }
            assert!(
                narrowed_registry_profile(entry, *entry_profile, RegistryProfile::WorkspaceEdit)
                    .is_err(),
                "{entry_profile:?} → WorkspaceEdit"
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
            .map_err(ctx("NoTools → NoTools ist zugelassen"))?;
        assert!(same.rights_snapshot().tools.is_empty());
        Ok(())
    }

    #[test]
    fn test_narrowing_permissions_never_exceed_profile() -> TestResult {
        let fixture = build_fixture()?;
        for entry in [
            EntryKind::LocalEcho,
            EntryKind::Doctor,
            EntryKind::Web,
            EntryKind::McpServe,
            EntryKind::JobPlanNode,
            EntryKind::GatewayTelegram,
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
                .map_err(|error| {
                    TestError::Unexpected(format!("{entry:?} montiert nicht: {error}"))
                })?;
            let granted = assembly.sandbox().permissions();
            assert!(granted.is_subset_of(&profile.permissions), "{entry:?}");
            assert_eq!(
                granted, &profile.permissions,
                "{entry:?}: die weiteste Obergrenze ergibt genau die Profilrechte"
            );
            assert_eq!(assembly.spawn_context().sandbox.permissions(), granted);
            assert!(!granted.contains(harw_authority::Permission::NetworkAccess));
            assert!(!granted.contains(harw_authority::Permission::ReadCargoRegistry));
        }

        // Eine engere Obergrenze schneidet; ein fremdes Recht kommt nie hinzu.
        let assembly = fixture_builder(EntryKind::LocalEcho, &fixture)
            .narrowing(RuntimeNarrowing {
                registry_profile: RegistryProfile::Full,
                identity: IdentityOverrides::default(),
                permissions: PermissionSet::from_policy([
                    harw_authority::Permission::ReadWorkspace,
                    harw_authority::Permission::NetworkAccess,
                ]),
                workspace_root: None,
            })
            .build()
            .map_err(ctx("LocalEcho montiert"))?;
        assert_eq!(
            assembly.sandbox().permissions(),
            &PermissionSet::from_policy([harw_authority::Permission::ReadWorkspace])
        );
        Ok(())
    }

    #[test]
    fn test_root_identity_active_agent_wins_over_narrowing() -> TestResult {
        let fixture = build_fixture()?;
        let mut spec = fixture_builder(EntryKind::LocalEcho, &fixture).spec;
        let narrowing = RuntimeNarrowing {
            registry_profile: RegistryProfile::ReadOnlyExplore,
            identity: IdentityOverrides {
                agent_name: Some("plan-node".to_owned()),
                role_description: Some("research node".to_owned()),
                extra_context: vec!["node 7".to_owned()],
                organizational_role: None,
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
        assert_eq!(
            with_agent.role_description.as_deref(),
            Some("research node")
        );

        let default = root_identity(&spec, None);
        assert_eq!(default.agent_name.as_deref(), Some("explorer"));
        assert!(default.role_description.is_none());
        Ok(())
    }

    #[test]
    fn test_narrowing_workspace_root_descendant_binds_sandbox_to_it() -> TestResult {
        let fixture = build_fixture()?;
        let workspace = fixture.project.join("nested").join("workspace");
        std::fs::create_dir_all(&workspace).map_err(ctx("workspace"))?;
        let canonical_workspace = workspace
            .canonicalize()
            .map_err(ctx("canonical workspace"))?;
        let canonical_project = fixture
            .project
            .canonicalize()
            .map_err(ctx("canonical project"))?;

        // Projekterkennung ab dem Unterordner endet am markierten Elternprojekt.
        let mut builder = fixture_builder(EntryKind::JobPlanNode, &fixture);
        builder.spec.cwd.clone_from(&workspace);
        let assembly = builder
            .narrowing(RuntimeNarrowing {
                registry_profile: RegistryProfile::ReadOnlyExplore,
                identity: IdentityOverrides::default(),
                permissions: PermissionSet::from_policy([
                    harw_authority::Permission::ReadWorkspace,
                ]),
                workspace_root: Some(workspace.clone()),
            })
            .build()
            .map_err(ctx("ein Nachfahre des Projekt-Roots ist zugelassen"))?;

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
            assembly
                .spawn_context()
                .sandbox
                .workspace()
                .canonical_root(),
            canonical_workspace.as_path(),
            "der Spawn-Kontext trägt dieselbe engere Bindung"
        );
        assert_eq!(
            assembly.sandbox().permissions(),
            &PermissionSet::from_policy([harw_authority::Permission::ReadWorkspace])
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
            .map_err(ctx("der Projekt-Root selbst ist zugelassen"))?;
        assert_eq!(
            same.sandbox().workspace().canonical_root(),
            canonical_project.as_path()
        );
        Ok(())
    }

    #[test]
    fn test_narrowing_workspace_root_outside_project_is_rejected() -> TestResult {
        let fixture = build_fixture()?;
        let Some(parent) = fixture.project.parent().map(Path::to_path_buf) else {
            return Err(TestError::Missing(
                "das Fixture-Projekt hat ein Elternverzeichnis",
            ));
        };
        let sibling = parent.join("sibling");
        std::fs::create_dir_all(&sibling).map_err(ctx("sibling"))?;

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
                std::fs::create_dir_all(&evil).map_err(ctx("prefix sibling"))?;
                evil
            }),
            (
                "Ausbruch über ..",
                fixture.project.join("..").join("sibling"),
            ),
            ("fehlendes Verzeichnis", fixture.project.join("missing")),
            ("relativer Pfad", PathBuf::from("nested")),
        ] {
            let Err(error) = fixture_builder(EntryKind::LocalEcho, &fixture)
                .narrowing(narrowing_to(root))
                .build()
            else {
                return Err(TestError::Unexpected(label.to_owned()));
            };
            assert!(
                matches!(error, RuntimeError::Sandbox { .. }),
                "{label}: {error}"
            );
        }

        // Ein Symlink im Projekt, der hinausführt, wird nach der Kanonisierung abgelehnt.
        #[cfg(unix)]
        {
            let link = fixture.project.join("escape-link");
            std::os::unix::fs::symlink(&sibling, &link).map_err(ctx("symlink"))?;
            let Err(error) = fixture_builder(EntryKind::LocalEcho, &fixture)
                .narrowing(narrowing_to(link))
                .build()
            else {
                return Err(TestError::Unexpected("Symlink nach außen".into()));
            };
            assert!(matches!(error, RuntimeError::Sandbox { .. }), "{error}");
        }
        Ok(())
    }

    // ── Projektkontext im Modellkontext (Befunde Z2d2-R1/R2/R8) ─────────────

    /// Liest ein [`harw_extension_api::ExtFuture`] synchron aus. Die Provider
    /// der Registry (`ProjectContextProvider`, `BaselineInstructionsProvider`)
    /// haben keinen `.await`-Punkt und sind beim ersten `poll` fertig.
    fn ready<T>(mut future: harw_extension_api::ExtFuture<'_, T>) -> TestResult<T> {
        use std::task::{Context, Poll, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => Ok(value),
            Poll::Pending => Err(TestError::Unexpected(
                "der Provider wurde beim ersten poll nicht fertig".into(),
            )),
        }
    }

    /// Alles, was die Wurzel-Registry dem Modell als Kontext zeigt:
    /// Systemprompt und Fragmente der Instruktions-Provider sowie jedes
    /// Kontextfragment als `label\ncontent`. Nimmt die Registry aus der
    /// Montage (danach ist sie für `new_root_session` verbraucht).
    fn registry_model_context(assembly: &RuntimeAssembly) -> TestResult<Vec<String>> {
        let registry = assembly
            .registry
            .lock()
            .map_err(|_| TestError::Unexpected("registry lock poisoned".into()))?
            .take()
            .ok_or(TestError::Missing(
                "die Registry wurde noch nicht herausgegeben",
            ))?;
        let turn = harw_extension_api::TurnInputContext::default();
        let mut out = Vec::new();
        for provider in registry.instructions_providers() {
            let loaded = ready(provider.load())?;
            out.push(loaded.system_prompt);
            out.extend(loaded.fragments);
        }
        for provider in registry.context_providers() {
            for fragment in ready(provider.contribute(&turn))? {
                out.push(format!("{}\n{}", fragment.label, fragment.content));
            }
        }
        Ok(out)
    }

    const AGENTS_MARKER: &str = "R1-MARKER-agents-doc-must-not-leak";

    #[test]
    fn test_job_prompt_assembly_has_no_project_docs() -> TestResult {
        let fixture = build_fixture()?;
        std::fs::write(
            fixture.project.join("AGENTS.md"),
            format!("# Geheim\n{AGENTS_MARKER}\n"),
        )
        .map_err(ctx("AGENTS.md"))?;
        let canonical_project = fixture
            .project
            .canonicalize()
            .map_err(ctx("canonical project"))?;

        for entry in [
            EntryKind::JobPrompt,
            EntryKind::McpServe,
            EntryKind::GatewayTelegram,
            EntryKind::GatewayDream,
            EntryKind::Web,
        ] {
            assert!(!entry.profile().project_context, "{entry:?}");
            let assembly = fixture_builder(entry, &fixture).build().map_err(|error| {
                TestError::Unexpected(format!("{entry:?} montiert nicht: {error}"))
            })?;
            assert!(
                assembly
                    .project()
                    .docs
                    .iter()
                    .any(|doc| doc.content.contains(AGENTS_MARKER)),
                "{entry:?}: die Erkennung selbst hat AGENTS.md gefunden (Test ist aussagekräftig)"
            );

            let context = registry_model_context(&assembly)?;
            assert!(!context.is_empty(), "{entry:?}");
            for text in &context {
                assert!(
                    !text.contains(AGENTS_MARKER),
                    "{entry:?}: AGENTS.md im Kontext: {text}"
                );
                assert!(
                    !text.contains("project.doc:"),
                    "{entry:?}: Doku-Fragment: {text}"
                );
                assert!(
                    !text.contains(canonical_project.to_string_lossy().as_ref()),
                    "{entry:?}: Host-Pfad im Kontext: {text}"
                );
                assert!(
                    !text.contains(fixture.project.to_string_lossy().as_ref()),
                    "{entry:?}: Host-Pfad im Kontext: {text}"
                );
            }
            let placeholder = PROJECT_CONTEXT_PLACEHOLDER;
            let expected_root =
                format!("project.root\nproject_root={placeholder}\ncwd={placeholder}");
            assert!(
                context.contains(&expected_root),
                "{entry:?}: neutraler Platzhalter statt Host-Pfad: {context:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_tui_assembly_keeps_project_docs() -> TestResult {
        let fixture = build_fixture()?;
        std::fs::write(
            fixture.project.join("AGENTS.md"),
            format!("# Projekt\n{AGENTS_MARKER}\n"),
        )
        .map_err(ctx("AGENTS.md"))?;
        let canonical_project = fixture
            .project
            .canonicalize()
            .map_err(ctx("canonical project"))?;
        assert!(EntryKind::Tui.profile().project_context);

        // `Tui` spawnt (`BuiltinRoles`) und braucht deshalb einen Ereigniskanal.
        let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let assembly = fixture_builder(EntryKind::Tui, &fixture)
            .session_events(events)
            .build()
            .map_err(ctx("Tui montiert"))?;

        let context = registry_model_context(&assembly)?;
        let shows_agents_doc = |text: &String| {
            text.starts_with("project.doc:AGENTS.md\n") && text.contains(AGENTS_MARKER)
        };
        assert!(
            context.iter().any(shows_agents_doc),
            "Tui zeigt AGENTS.md: {context:?}"
        );
        let expected_root = format!(
            "project.root\nproject_root={}\ncwd={}",
            canonical_project.display(),
            canonical_project.display()
        );
        assert!(
            context.contains(&expected_root),
            "Tui zeigt den erkannten Projekt-Root: {context:?}"
        );
        Ok(())
    }

    #[test]
    fn test_narrowing_workspace_root_filters_docs_above_root() -> TestResult {
        const ABOVE: &str = "R2-MARKER-doc-above-bound-root";
        const INSIDE: &str = "R2-MARKER-doc-inside-bound-root";

        let fixture = build_fixture()?;
        let workspace = fixture.project.join("nested").join("workspace");
        std::fs::create_dir_all(&workspace).map_err(ctx("workspace"))?;
        std::fs::write(fixture.project.join("AGENTS.md"), ABOVE)
            .map_err(ctx("oberes AGENTS.md"))?;
        std::fs::write(workspace.join("AGENTS.md"), INSIDE).map_err(ctx("inneres AGENTS.md"))?;
        let canonical_workspace = workspace
            .canonicalize()
            .map_err(ctx("canonical workspace"))?;

        let mut builder = fixture_builder(EntryKind::JobPlanNode, &fixture);
        builder.spec.cwd.clone_from(&workspace);
        let assembly = builder
            .narrowing(RuntimeNarrowing {
                registry_profile: RegistryProfile::ReadOnlyExplore,
                identity: IdentityOverrides::default(),
                permissions: PermissionSet::from_policy([
                    harw_authority::Permission::ReadWorkspace,
                ]),
                workspace_root: Some(workspace.clone()),
            })
            .build()
            .map_err(ctx("Plan-Knoten mit gebundenem Workspace montiert"))?;
        assert_eq!(
            assembly.project().docs.len(),
            2,
            "die Erkennung findet beide Dateien; gefiltert wird nur der Modellkontext"
        );

        let context = registry_model_context(&assembly)?;
        assert!(
            context.iter().all(|text| !text.contains(ABOVE)),
            "Doku oberhalb des gebundenen Roots darf nicht erscheinen: {context:?}"
        );
        assert!(
            context.iter().any(|text| text.contains(INSIDE)),
            "Doku im gebundenen Root bleibt: {context:?}"
        );
        let expected_root = format!(
            "project.root\nproject_root={}\ncwd={}",
            canonical_workspace.display(),
            canonical_workspace.display()
        );
        assert!(
            context.contains(&expected_root),
            "project_root im Kontext ist der gebundene Root: {context:?}"
        );

        // Komponenten- statt String-Vergleich, an der reinen Entscheidung.
        let doc = |path: &str| harw_project_discovery::DiscoveredDoc {
            path: PathBuf::from(path),
            filename: "AGENTS.md".to_owned(),
            content: path.to_owned(),
        };
        let project = ProjectContext {
            cwd: PathBuf::from("/work/space/sub"),
            project_root: PathBuf::from("/work"),
            docs: vec![
                doc("/work/AGENTS.md"),
                doc("/work/space-evil/AGENTS.md"),
                doc("/work/space/AGENTS.md"),
                doc("/work/space/sub/AGENTS.md"),
            ],
        };
        let profile = EntryKind::JobPlanNode.profile();
        let narrowed = registry_project_context(&project, &profile, Some(Path::new("/work/space")));
        let kept: Vec<&str> = narrowed
            .docs
            .iter()
            .map(|doc| doc.content.as_str())
            .collect();
        assert_eq!(kept, ["/work/space/AGENTS.md", "/work/space/sub/AGENTS.md"]);
        assert_eq!(narrowed.project_root, PathBuf::from("/work/space"));
        assert_eq!(narrowed.cwd, PathBuf::from("/work/space/sub"));

        // Ohne gebundenen Root bleibt der erkannte Kontext unverändert (keine Kopie).
        let unchanged = registry_project_context(&project, &profile, None);
        assert!(matches!(unchanged, Cow::Borrowed(_)));

        // Ohne Projektkontext gewinnt die Schwärzung auch gegen einen gebundenen Root.
        let redacted = registry_project_context(
            &project,
            &EntryKind::JobPrompt.profile(),
            Some(Path::new("/work/space")),
        );
        assert!(redacted.docs.is_empty());
        assert_eq!(
            redacted.project_root,
            PathBuf::from(PROJECT_CONTEXT_PLACEHOLDER)
        );
        assert_eq!(redacted.cwd, PathBuf::from(PROJECT_CONTEXT_PLACEHOLDER));
        Ok(())
    }

    #[test]
    fn test_narrowing_rejects_restricted_profile_with_model_tool_operations() -> TestResult {
        let fixture = build_fixture()?;
        assert_eq!(
            EntryKind::Doctor.profile().operations,
            OperationSurface::AllWithModelTools
        );
        let narrowing_to = |registry_profile: RegistryProfile| RuntimeNarrowing {
            registry_profile,
            identity: IdentityOverrides::default(),
            permissions: every_permission(),
            workspace_root: None,
        };

        for requested in [RegistryProfile::ReadOnlyExplore, RegistryProfile::NoTools] {
            let Err(error) = fixture_builder(EntryKind::Doctor, &fixture)
                .narrowing(narrowing_to(requested))
                .build()
            else {
                return Err(TestError::Unexpected(
                    "Werkzeugverengung an AllWithModelTools muss abgelehnt werden".into(),
                ));
            };
            assert!(
                matches!(error, RuntimeError::Registry { .. }),
                "{requested:?}: {error}"
            );
        }

        // `Full` bleibt zugelassen, ebenso eine Verengung ohne Modell-Tool-Fläche.
        fixture_builder(EntryKind::Doctor, &fixture)
            .narrowing(narrowing_to(RegistryProfile::Full))
            .build()
            .map_err(ctx("Doctor mit Full montiert"))?;
        fixture_builder(EntryKind::LocalEcho, &fixture)
            .narrowing(narrowing_to(RegistryProfile::ReadOnlyExplore))
            .build()
            .map_err(ctx(
                "LocalEcho (OperationSurface::None) mit ReadOnlyExplore montiert",
            ))?;

        // Reine Entscheidung für alle Einstiege mit Modell-Tool-Fläche.
        for entry in [EntryKind::Tui, EntryKind::OneShot, EntryKind::Doctor] {
            let profile = entry.profile();
            assert!(ensure_narrowing_fits_operations(entry, &profile, None).is_ok());
            assert!(
                ensure_narrowing_fits_operations(
                    entry,
                    &profile,
                    Some(&narrowing_to(RegistryProfile::Full))
                )
                .is_ok()
            );
            for requested in [RegistryProfile::ReadOnlyExplore, RegistryProfile::NoTools] {
                assert!(
                    matches!(
                        ensure_narrowing_fits_operations(
                            entry,
                            &profile,
                            Some(&narrowing_to(requested))
                        ),
                        Err(RuntimeError::Registry { .. })
                    ),
                    "{entry:?} → {requested:?}"
                );
            }
        }
        Ok(())
    }

    // ── Freigaben: Präzedenz und Fail-Soft (Contract §2/§4/§5) ───────────────

    /// Präzedenz Projekt > Global > eingebaute Vorgabe des Einstiegs.
    #[test]
    fn test_effective_approval_mode_precedence_project_over_global_over_default() {
        let mut global = PermissionsSection::default();
        let mut project = PermissionsSection::default();

        // Keine Ebene gesetzt: die eingebaute Vorgabe des Einstiegs gewinnt.
        assert_eq!(
            effective_approval_mode(EntryKind::Tui, &global, &project),
            default_approval_mode(EntryKind::Tui)
        );

        // Nur global gesetzt: global schlägt die eingebaute Vorgabe.
        global.default_mode = Some("full".to_owned());
        assert_eq!(
            effective_approval_mode(EntryKind::Tui, &global, &project),
            ApprovalMode::FullAccess
        );

        // Projekt zusätzlich gesetzt: Projekt schlägt global.
        project.default_mode = Some("ask".to_owned());
        assert_eq!(
            effective_approval_mode(EntryKind::Tui, &global, &project),
            ApprovalMode::AlwaysAsk
        );
    }

    /// Dieselbe Präzedenz gilt für das Freigabe-Timeout (Plan Schritt 3).
    #[test]
    fn test_effective_approval_timeout_precedence_project_over_global_over_default() {
        let mut global = PermissionsSection::default();
        let mut project = PermissionsSection::default();

        assert_eq!(
            effective_approval_timeout(&global, &project),
            Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS)
        );

        global.approval_timeout_secs = Some(60);
        assert_eq!(
            effective_approval_timeout(&global, &project),
            Duration::from_secs(60)
        );

        project.approval_timeout_secs = Some(30);
        assert_eq!(
            effective_approval_timeout(&global, &project),
            Duration::from_secs(30)
        );
    }

    /// Eine ungültige Extra-Root (hier: nicht existent) wird übersprungen,
    /// nicht abgelehnt — nur der gültige Eintrag landet in der Zelle.
    ///
    /// Der gültige Kandidat muss außerhalb von `primary` liegen: eine
    /// zusätzliche Wurzel *innerhalb* der primären Wurzel würde deren Rechte
    /// nicht erweitern und wird von [`validate_extra_root`]
    /// (`harw-sandbox/src/extra_roots.rs`) als `AlreadyContained` verworfen
    /// (die vorherige Fassung dieses Tests platzierte `valid` fälschlich
    /// unter `primary` und scheiterte deshalb mit `0` statt `1`).
    #[test]
    fn test_seed_extra_roots_skips_invalid_entries() -> TestResult {
        let dir = tempfile::tempdir()?;
        let primary = dir.path().join("primary");
        std::fs::create_dir_all(&primary).map_err(ctx("primary"))?;
        let valid = dir.path().join("valid");
        std::fs::create_dir_all(&valid).map_err(ctx("valid"))?;
        let missing = primary.join("does-not-exist");

        let global = PermissionsSection {
            extra_roots: vec![valid, missing],
            ..PermissionsSection::default()
        };

        let cell = seed_extra_roots(&global, &PermissionsSection::default(), &primary, None);
        let snapshot = cell.snapshot();
        assert_eq!(
            snapshot.len(),
            1,
            "nur der gültige Eintrag bleibt: {snapshot:?}"
        );
        Ok(())
    }

    // ── Plan-Dienste: Default persistent an, außer ausdrücklich abgeschaltet ─

    /// Ein interaktiver TUI-Lauf ohne jeden Konfigurationseintrag bekommt die
    /// persistente Plan-Vorgabe — `/plan` und `/goal` funktionieren ohne
    /// `[tools.plan]` (G-024/G-098), und die Stores liegen unter dem
    /// Projekt-Home. Andere Einstiege bleiben unverändert ohne Planfläche.
    #[test]
    fn test_resolve_plan_services_defaults_on_for_tui_when_untouched() -> TestResult {
        let fixture = build_fixture()?;
        let home_project_root =
            discover_home_project(&fixture.project, &[]).map_err(ctx("Projekt-Home erkannt"))?;
        let project_home = ProjectHome::at(&home_project_root);

        let resolved =
            resolve_plan_services(EntryKind::Tui, None, &PlanSection::default(), &project_home)
                .map_err(ctx("plan stores"))?;
        let Some(plan) = resolved else {
            return Err(TestError::Missing(
                "Tui ohne Config-Eintrag muss die eingebaute Plan-Vorgabe bekommen",
            ));
        };
        assert!(plan.plan_config.enabled);
        assert!(plan.plan_config.persist);

        assert!(
            resolve_plan_services(
                EntryKind::OneShot,
                None,
                &PlanSection::default(),
                &project_home
            )
            .map_err(ctx("plan configuration"))?
            .is_none(),
            "die Gate-Semantik anderer Einstiege bleibt unverändert"
        );
        Ok(())
    }

    /// Eine berührte Sektion mit `enabled = false` bleibt geschlossen — eine
    /// bewusste Abschaltung wird nie überschrieben. Ein expliziter
    /// Builder-Wert gewinnt dagegen immer, unabhängig von der Konfiguration.
    #[test]
    fn test_resolve_plan_services_stays_off_when_touched_and_disabled() -> TestResult {
        let fixture = build_fixture()?;
        let home_project_root =
            discover_home_project(&fixture.project, &[]).map_err(ctx("Projekt-Home erkannt"))?;
        let project_home = ProjectHome::at(&home_project_root);

        let section = PlanSection {
            enabled: false,
            ..PlanSection::default()
        };
        assert!(
            resolve_plan_services(EntryKind::Tui, None, &section, &project_home)
                .map_err(ctx("disabled plan"))?
                .is_none(),
            "eine berührte, weiterhin `enabled = false`-Sektion bleibt geschlossen"
        );

        let explicit = PlanServices {
            plan: Arc::new(InMemoryPlanStore::new()),
            goal: Arc::new(InMemoryGoalStore::new()),
            findings: Arc::new(FindingStore::new(project_home.plans_dir())),
            plan_config: PlanToolConfig::enabled_defaults(),
        };
        let findings = Arc::clone(&explicit.findings);
        let resolved =
            resolve_plan_services(EntryKind::Tui, Some(explicit), &section, &project_home)
                .map_err(ctx("plan configuration"))?
                .ok_or(TestError::Missing(
                    "ein expliziter Builder-Wert bleibt erhalten",
                ))?;
        assert!(Arc::ptr_eq(&resolved.findings, &findings));
        Ok(())
    }

    /// Eine berührte, ausdrücklich aktivierte Sektion wird vollständig
    /// übersetzt (Knotenlimits, Exploration-Vorgaben).
    #[test]
    fn test_plan_tool_config_from_section_translates_fields() -> TestResult {
        let section = PlanSection {
            enabled: true,
            max_nodes: 12,
            require_exploration_for: vec!["coding".to_owned()],
            ..PlanSection::default()
        };

        let config =
            plan_tool_config_from_section(&section).map_err(ctx("gültige Sektion übersetzt"))?;
        assert!(config.enabled);
        assert_eq!(config.max_nodes, 12);
        assert_eq!(config.require_exploration_for, vec![PlanNodeKind::Coding]);
        Ok(())
    }

    /// Eine ungültige Sektion (`max_nodes = 0`) übersetzt nicht und beendet
    /// die Runtime-Montage, statt einen flüchtigen Ersatzstore zu öffnen.
    #[test]
    fn test_plan_tool_config_from_section_rejects_zero_max_nodes() -> TestResult {
        let section = PlanSection {
            enabled: true,
            max_nodes: 0,
            ..PlanSection::default()
        };
        assert!(plan_tool_config_from_section(&section).is_err());

        let fixture = build_fixture()?;
        let home_project_root =
            discover_home_project(&fixture.project, &[]).map_err(ctx("Projekt-Home erkannt"))?;
        let project_home = ProjectHome::at(&home_project_root);
        assert!(
            resolve_plan_services(EntryKind::Tui, None, &section, &project_home).is_err(),
            "eine ungültige Plan-Konfiguration muss die Montage ablehnen"
        );
        Ok(())
    }
}
