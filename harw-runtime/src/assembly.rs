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
//! 7. [`assemble_registry_for_project`] → [`ApprovalChain::install_over_default`];
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
use harw_config::{PermissionsSection, PlanSection, ResolvedConfig, discover_config};
use harw_context::ContextCeiling;
use harw_core::{
    AgentSession, ChildRegistryFactory, ManagedAgentSpawner, ModelProvider, SessionActivation,
    SessionManager, SpawnContext, StateStore, ToolProfile,
};
use harw_extension_api::allow_rules::{AllowRuleSet, ApprovalRule, RuleDecision, RuleScope};
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_extension_api::{ApprovalHandler, ExtensionRegistry, ExtensionRegistryBuilder, ToolName};
use harw_home::paths::active_profile_name;
use harw_home::project::{
    ProjectHome, ProjectRoot, discover_project as discover_home_project, project_key,
    project_settings_dir,
};
use harw_memory::Memory;
use harw_operations::adapter::ModelToolProvider;
use harw_operations::operation::{Operation, Surface};
use harw_operations::registry::OperationRegistry;
use harw_operations::{OpContext, SharedSessionController};
use harw_plan::{InMemoryGoalStore, InMemoryPlanStore, PlanNodeKind, PlanToolConfig};
use harw_plan_bridge::FindingStore;
use harw_project_discovery::{DiscoveryConfig, ProjectContext, discover_project};
use harw_protocol::events::{SessionEvent, TurnEvent};
use harw_provider_http::SecretResolver;
use harw_registry_defaults::embedded_agents::builtin_agent_definitions;
use harw_registry_defaults::profile::{
    IdentityOverrides, RegistryProfile, assemble_registry_for_project, role_names,
};
use harw_sandbox::{
    ExtraRootsCell, NetworkScope, PermissionSet, SandboxSpec, WorkspaceRegistration,
    WorkspaceRegistry,
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

/// Vorgabe-Timeout einer offenen Freigabeanfrage in Sekunden, wenn weder
/// Projekt- noch globale Konfiguration `[permissions] approval_timeout_secs`
/// setzen (Plan Schritt 3: 1800 s statt der bisherigen 300 s).
const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 1800;

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

/// Das Home-Verzeichnis des Betriebssystem-Nutzers, roh aus `$HOME` gelesen.
///
/// # Beschreibung
/// Wird ausschließlich als Ausschlussregel für `/add-workdir`-Kandidaten
/// gebraucht ([`harw_sandbox::validate_extra_root`]) — dieselbe Quelle wie
/// `harw_home::project::refuse_unsupported_root` (privat dort, deshalb hier
/// dupliziert statt importiert). Ein leerer oder fehlender Wert liefert
/// `None`; der Aufrufer überspringt die Home-Prüfung dann statt sie
/// abzulehnen.
fn os_user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Lädt die projekt-scoped `[permissions]`-Sektion (Contract §2, Zeile A2).
///
/// # Beschreibung
/// Der autoritätsgewährende Speicherort eines Projekts ist
/// `~/.harw/profiles/<profil>/projects/<project-key>` — außerhalb jedes
/// Repos, damit ein geklontes Projekt sich keine Rechte selbst geben kann.
/// Dieses Verzeichnis wird wie jeder andere Config-Layer über
/// [`harw_config::discover_config`] gelesen (eine `config.toml` darunter);
/// fehlt das Verzeichnis oder die Datei, liefert `discover_config` bereits
/// eine leere [`ResolvedConfig`] — das ist der normale „noch nichts
/// gemerkt“-Zustand eines Projekts, kein Fehler.
///
/// Jeder andere Fehler (ungültiger Profilname, kaputtes TOML) wird
/// **nicht** weitergereicht: die Wurzel-Montage darf an einer beschädigten
/// Projekt-Einstellungsdatei nicht scheitern. Es bleibt bei einem `warn!`
/// und der leeren Sektion.
///
/// # Arguments
/// - `home` (`&Path`): aufgelöster Root-Space.
/// - `profile` (`&str`): aktives Profil ([`active_profile_name`]).
/// - `key` (`&str`): Projekt-Schlüssel ([`project_key`]).
///
/// # Returns
/// Die geladene [`PermissionsSection`]; leer, wenn nichts gemerkt wurde oder
/// das Lesen fehlschlug.
fn load_project_permissions(home: &Path, profile: &str, key: &str) -> PermissionsSection {
    let dir = match project_settings_dir(home, profile, key) {
        Ok(dir) => dir,
        Err(error) => {
            tracing::warn!(
                profile,
                key,
                error = %error,
                "runtime.project_settings.path_invalid"
            );
            return PermissionsSection::default();
        }
    };
    match discover_config(std::slice::from_ref(&dir)) {
        Ok(config) => config.harness.permissions,
        Err(error) => {
            tracing::warn!(
                dir = %dir.display(),
                error = %error,
                "runtime.project_settings.load_failed"
            );
            PermissionsSection::default()
        }
    }
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
/// - `global` (`&PermissionsSection`): `[permissions]` aus der globalen
///   Konfiguration (`config.harness.permissions`).
/// - `project` (`&PermissionsSection`): `[permissions]` aus der
///   Projekt-Einstellungsdatei ([`load_project_permissions`]).
///
/// # Returns
/// Den effektiven [`ApprovalMode`].
fn effective_approval_mode(
    entry: EntryKind,
    global: &PermissionsSection,
    project: &PermissionsSection,
) -> ApprovalMode {
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
fn effective_approval_timeout(global: &PermissionsSection, project: &PermissionsSection) -> Duration {
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

/// Baut die eingebauten Plan-Dienste, mit denen die TUI ohne jede
/// Konfiguration startet (Plan Schritt 1, `[tools.plan] enabled` Default
/// `true` für interaktive TUI-Einstiege).
///
/// # Beschreibung
/// `plan`/`goal` sind reine In-Memory-Speicher — bewusst nicht persistent,
/// solange niemand `[tools.plan] persist = true` setzt (derselbe Vorgabewert
/// wie [`PlanToolConfig::enabled_defaults`]). Der [`FindingStore`] wurzelt
/// auf [`ProjectHome::plans_dir`]: er legt sein Verzeichnis erst beim ersten
/// Schreiben an, ein unbenutztes Projekt bleibt also ohne Spur auf der
/// Platte.
///
/// # Arguments
/// - `project_home` (`&ProjectHome`): das Projekt-Home dieses Laufs.
///
/// # Returns
/// [`PlanServices`] mit aktivierter [`PlanToolConfig`].
fn default_tui_plan_services(project_home: &ProjectHome) -> PlanServices {
    PlanServices {
        plan: Arc::new(InMemoryPlanStore::new()),
        goal: Arc::new(InMemoryGoalStore::new()),
        findings: Arc::new(FindingStore::new(project_home.plans_dir())),
        plan_config: PlanToolConfig::enabled_defaults(),
    }
}

/// Ob der Aufrufer `[tools.plan]` in keiner Konfigurationsebene angefasst hat.
///
/// # Beschreibung
/// `PlanSection::enabled` ist ein einfaches `bool` (kein `Option<bool>`) und
/// kann „nie gesetzt“ nicht von „ausdrücklich auf `false` gesetzt“
/// unterscheiden (Kopplung an `harw-config`, außerhalb dieser Welle). Als
/// Näherung gilt die Sektion nur dann als unangetastet, wenn sie **exakt**
/// [`PlanSection::default`] entspricht — jede andere Abweichung (auch nur
/// `persist = true` bei weiterhin `enabled = false`) wird als bewusste
/// Entscheidung gewertet und nicht überschrieben.
///
/// # Returns
/// `true`, wenn `section == PlanSection::default()`.
fn plan_section_is_untouched(section: &PlanSection) -> bool {
    *section == PlanSection::default()
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
        .map(|name| name.parse::<PlanNodeKind>().map_err(|error| error.to_string()))
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
/// 3. **Unangetastete Sektion** ([`plan_section_is_untouched`]): gilt als
///    „noch nie entschieden“ und wird zu [`default_tui_plan_services`] —
///    `/plan` und `/goal` funktionieren damit ohne jeden Konfigurationseintrag.
/// 4. **Berührte Sektion, `enabled = false`**: bleibt geschlossen — eine
///    bewusste Abschaltung wird nie überschrieben.
/// 5. **Berührte Sektion, `enabled = true`**: [`plan_tool_config_from_section`]
///    übersetzt die volle Konfiguration (Knotenlimits,
///    `require_exploration_for` etc.). Schlägt die Übersetzung fehl, bleibt
///    die Fläche geschlossen (`warn!`, fail-soft wie
///    [`load_project_permissions`]). Die Speicher bleiben, wie im eingebauten
///    Vorgabefall, In-Memory — das Umschalten auf `FilePlanStore`/
///    `FileGoalStore` bei `persist = true` ist nicht Teil dieser Welle.
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
) -> Option<PlanServices> {
    if explicit.is_some() {
        return explicit;
    }
    if !matches!(entry, EntryKind::Tui) {
        return None;
    }
    if plan_section_is_untouched(section) {
        return Some(default_tui_plan_services(project_home));
    }
    if !section.enabled {
        return None;
    }
    match plan_tool_config_from_section(section) {
        Ok(plan_config) => Some(PlanServices {
            plan: Arc::new(InMemoryPlanStore::new()),
            goal: Arc::new(InMemoryGoalStore::new()),
            findings: Arc::new(FindingStore::new(project_home.plans_dir())),
            plan_config,
        }),
        Err(error) => {
            tracing::warn!(
                entry = ?entry,
                error = %error,
                "runtime.plan_tool_config.invalid"
            );
            None
        }
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

/// Neutraler Platzhalter für `project_root`/`cwd` im Modellkontext von
/// Einstiegen ohne [`EntryProfile::project_context`] (Befund Z2d2-R1).
const PROJECT_CONTEXT_PLACEHOLDER: &str = "<workspace>";

/// Der Projektkontext, den die Wurzel-Registry dem Modell zeigt.
///
/// # Beschreibung
/// Die Registry ([`assemble_registry_for_project`]) reicht den Kontext an
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
    ///   benannter Agent nicht aufgelöst werden kann, eine
    ///   [`Self::narrowing`] einen nicht zugelassenen Werkzeugsatz verlangt
    ///   oder einen Werkzeugsatz außer `Full` an einem Einstieg mit
    ///   [`OperationSurface::AllWithModelTools`] verlangt.
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
        ensure_narrowing_fits_operations(spec.entry, &profile, narrowing.as_ref())?;

        // 1. Konfiguration mit Vertrauensbericht.
        let (config, trust_report) = load_config(&spec)?;
        let config = Arc::new(config);

        // 2. Projekterkennung — genau einmal je Lauf.
        let project = discover_project(&spec.cwd, &DiscoveryConfig::default()).map_err(|error| {
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
        let home_project_root =
            discover_home_project(&spec.cwd, &markers).map_err(|error| RuntimeError::Discovery {
                detail: format!("could not discover the project home below the cwd: {error}"),
            })?;
        let home_project = ProjectHome::at(&home_project_root);
        if let Err(error) = home_project.ensure() {
            tracing::warn!(
                root = %home_project_root.root.display(),
                error = %error,
                "runtime.project_home.ensure_failed"
            );
        }

        // Freigaben-Konfiguration: Projekt schlägt Global schlägt eingebaute
        // Vorgabe (Contract §2). Die Projekt-Einstellungsdatei liegt
        // autoritätsgewährend außerhalb des Repos.
        let profile_name = active_profile_name(&spec.home);
        let project_settings_key = project_key(&home_project_root.root);
        let project_permissions =
            load_project_permissions(&spec.home, &profile_name, &project_settings_key);
        let global_permissions = config.harness.permissions.clone();

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
        let sandbox = sandbox.with_extra_roots(extra_roots.clone());

        // Agent-Definitionen werden einmal gesenkt. Ein interaktiver Einstieg
        // besitzt zwingend eine konfigurierte UIA; fehlende oder falsch gerollte
        // Auswahl ist ein Startfehler, nie ein stiller Full-Tool-Fallback.
        let needs_definitions = spec.active_agent.is_some()
            || config.harness.active_uia_definition.is_some()
            || matches!(profile.spawner, SpawnerPolicy::BuiltinRoles);
        let agent_definitions = if needs_definitions {
            lower_agent_definitions(&config)?
        } else {
            HashMap::new()
        };
        let uia_ir = resolve_active_uia(spec.entry, &config, &agent_definitions)?;
        // Ein UI-Einstieg hat genau einen Root: die UIA. Die frühere
        // `active_agent`-Auswahl bleibt für nicht-interaktive Einstiege
        // erhalten, darf aber die UIA weder ersetzen noch ihre Tool-Decke
        // überlagern.
        let agent_ir = if uia_ir.is_some() {
            None
        } else {
            resolve_active_agent(spec.active_agent.as_deref(), &config, &agent_definitions)?
        };
        let activation = root_activation(uia_ir.as_ref().or(agent_ir.as_ref()));

        // 5. Ein Trace, ein Spawn-Kontext.
        let trace = new_root_trace(spec.entry);
        let spawn_context = SpawnContext {
            sandbox: sandbox.clone(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: spec.principal.approval_actor(),
            organizational_role: uia_ir
                .as_ref()
                .map_or_else(|| root_organizational_role(profile.spawner), ExecutableAgentIr::role),
            // The root can delegate a child orchestrator only when its frozen
            // active definition lists that exact role. No active definition
            // means no such delegation grant.
            allowed_child_orchestrators: uia_ir
                .as_ref()
                .map(|ir| ir.spawn_contract().child_orchestrators().to_vec())
                .unwrap_or_default(),
            trace: Some(trace),
            ceiling: Some(ceiling.clone()),
        };

        // 6. Freigabekette. Der Responder kommt erst mit `new_root_session`:
        //    er gehört zur Oberfläche, nicht zur Montage.
        let approval_mode = ApprovalModeCell::new(effective_approval_mode(
            spec.entry,
            &global_permissions,
            &project_permissions,
        ));
        let allow_rules = seed_allow_rule_set(&global_permissions, &project_permissions);
        let approval_timeout = effective_approval_timeout(&global_permissions, &project_permissions);
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

        // 7. Registry: ein Projektkontext, eine Kette. Der Modellkontext folgt
        //    `profile.project_context` und einem gebundenen `workspace_root`.
        let mut overrides = root_identity(&spec, narrowing.as_ref());
        if let Some(uia) = uia_ir.as_ref() {
            let definition_id = uia.id().to_string();
            if let Some(agent_dir) = config.agent_definition_dirs.get(&definition_id) {
                let fragments = harw_config::load_uia_personalization(agent_dir).map_err(|error| {
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
        let narrowed_root = narrowing
            .as_ref()
            .and_then(|narrowing| narrowing.workspace_root.as_ref())
            .map(|_| bound_root.as_path());
        let assembled = assemble_registry_for_project(
            registry_profile,
            &registry_project_context(&project, &profile, narrowed_root),
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

        // 7b. Plan-Dienste: expliziter Builder-Wert gewinnt; sonst eingebaute
        //     Vorgabe für interaktive TUI-Einstiege, sofern `[tools.plan]`
        //     nicht ausdrücklich abweicht (Plan Schritt 1, G-010/F-154,
        //     G-024/G-098).
        let plan_services = resolve_plan_services(
            spec.entry,
            plan_services,
            &config.harness.tools.plan,
            &home_project,
        );

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
            allow_rules: allow_rules.clone(),
            extra_roots: extra_roots.clone(),
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

/// Löst die obligatorische UIA eines interaktiven Einstiegs auf.
///
/// TUI und One-shot sind Nutzeroberflächen und benötigen zwingend eine UIA.
/// Ein Telegram-Gateway verwendet dieselbe UIA, wenn sie konfiguriert ist, damit
/// lokale und Telegram-Gespräche dieselbe Persönlichkeit und den zugehörigen
/// Kontext verwenden. Unkonfigurierte Gateways bleiben aus Kompatibilitätsgründen
/// bei ihrem bisherigen, agentlosen Root.
fn resolve_active_uia(
    entry: EntryKind,
    config: &ResolvedConfig,
    builtin: &HashMap<String, ExecutableAgentIr>,
) -> RuntimeResult<Option<ExecutableAgentIr>> {
    let required = matches!(entry, EntryKind::Tui | EntryKind::OneShot);
    let eligible = required || matches!(entry, EntryKind::GatewayTelegram);
    if !eligible {
        return Ok(None);
    }
    let Some(name) = config.harness.active_uia_definition.as_deref() else {
        return if required {
            Err(RuntimeError::Registry {
                detail: "no active UIA is configured; set harness.active_uia_definition to a user-interface agent definition".to_owned(),
            })
        } else {
            Ok(None)
        };
    };
    let ir = resolve_active_agent(Some(name), config, builtin)?.ok_or_else(|| RuntimeError::Registry {
        detail: format!("UIA '{name}' did not resolve"),
    })?;
    if ir.role() != AgentRoleId::UserInterface {
        return Err(RuntimeError::Registry {
            detail: format!("configured UIA '{name}' has role {:?}, expected user-interface", ir.role()),
        });
    }
    Ok(Some(ir))
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
            // The registered target's sealed role comes from its frozen
            // definition. Unknown/missing definitions fail closed as workers,
            // which cannot spawn further agents.
            definitions
                .get(*role)
                .map_or(AgentRoleId::Worker, ExecutableAgentIr::role),
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
    fn interactive_entries_require_a_configured_user_interface_agent() {
        let (config, definitions) = builtin();
        let error = resolve_active_uia(EntryKind::Tui, &config, &definitions).unwrap_err();
        assert!(matches!(error, RuntimeError::Registry { .. }));

        let explorer = definitions.get(role_names::EXPLORER).expect("explorer").clone();
        let mut config = config;
        config.harness.active_uia_definition = Some("not-a-uia".to_owned());
        config.executable_agents.insert("not-a-uia".to_owned(), explorer);
        let error = resolve_active_uia(EntryKind::Tui, &config, &definitions).unwrap_err();
        assert!(matches!(error, RuntimeError::Registry { .. }));
    }

    #[test]
    fn non_interactive_entries_do_not_require_a_uia() {
        let (config, definitions) = builtin();
        assert!(resolve_active_uia(EntryKind::Doctor, &config, &definitions)
            .expect("doctor has no UIA requirement")
            .is_none());
        assert!(resolve_active_uia(EntryKind::GatewayTelegram, &config, &definitions)
            .expect("unconfigured gateway remains compatible")
            .is_none());
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

    // ── Projektkontext im Modellkontext (Befunde Z2d2-R1/R2/R8) ─────────────

    /// Liest ein [`harw_extension_api::ExtFuture`] synchron aus. Die Provider
    /// der Registry (`ProjectContextProvider`, `BaselineInstructionsProvider`)
    /// haben keinen `.await`-Punkt und sind beim ersten `poll` fertig.
    fn ready<T>(mut future: harw_extension_api::ExtFuture<'_, T>) -> T {
        use std::task::{Context, Poll, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("der Provider wurde beim ersten poll nicht fertig"),
        }
    }

    /// Alles, was die Wurzel-Registry dem Modell als Kontext zeigt:
    /// Systemprompt und Fragmente der Instruktions-Provider sowie jedes
    /// Kontextfragment als `label\ncontent`. Nimmt die Registry aus der
    /// Montage (danach ist sie für `new_root_session` verbraucht).
    fn registry_model_context(assembly: &RuntimeAssembly) -> Vec<String> {
        use harw_extension_api::{ContextProvider as _, InstructionsProvider as _};
        let registry = assembly
            .registry
            .lock()
            .expect("registry lock")
            .take()
            .expect("die Registry wurde noch nicht herausgegeben");
        let turn = harw_extension_api::TurnInputContext::default();
        let mut out = Vec::new();
        for provider in registry.instructions_providers() {
            let loaded = ready(provider.load());
            out.push(loaded.system_prompt);
            out.extend(loaded.fragments);
        }
        for provider in registry.context_providers() {
            for fragment in ready(provider.contribute(&turn)) {
                out.push(format!("{}\n{}", fragment.label, fragment.content));
            }
        }
        out
    }

    const AGENTS_MARKER: &str = "R1-MARKER-agents-doc-must-not-leak";

    #[test]
    fn test_job_prompt_assembly_has_no_project_docs() {
        let fixture = build_fixture();
        std::fs::write(
            fixture.project.join("AGENTS.md"),
            format!("# Geheim\n{AGENTS_MARKER}\n"),
        )
        .expect("AGENTS.md");
        let canonical_project = fixture.project.canonicalize().expect("canonical project");

        for entry in [
            EntryKind::JobPrompt,
            EntryKind::McpServe,
            EntryKind::GatewayTelegram,
            EntryKind::GatewayDream,
            EntryKind::Web,
        ] {
            assert!(!entry.profile().project_context, "{entry:?}");
            let assembly = fixture_builder(entry, &fixture)
                .build()
                .unwrap_or_else(|error| panic!("{entry:?} montiert nicht: {error}"));
            assert!(
                assembly
                    .project()
                    .docs
                    .iter()
                    .any(|doc| doc.content.contains(AGENTS_MARKER)),
                "{entry:?}: die Erkennung selbst hat AGENTS.md gefunden (Test ist aussagekräftig)"
            );

            let context = registry_model_context(&assembly);
            assert!(!context.is_empty(), "{entry:?}");
            for text in &context {
                assert!(!text.contains(AGENTS_MARKER), "{entry:?}: AGENTS.md im Kontext: {text}");
                assert!(!text.contains("project.doc:"), "{entry:?}: Doku-Fragment: {text}");
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
                context.iter().any(|text| *text == expected_root),
                "{entry:?}: neutraler Platzhalter statt Host-Pfad: {context:?}"
            );
        }
    }

    #[test]
    fn test_tui_assembly_keeps_project_docs() {
        let fixture = build_fixture();
        std::fs::write(
            fixture.project.join("AGENTS.md"),
            format!("# Projekt\n{AGENTS_MARKER}\n"),
        )
        .expect("AGENTS.md");
        let canonical_project = fixture.project.canonicalize().expect("canonical project");
        assert!(EntryKind::Tui.profile().project_context);

        // `Tui` spawnt (`BuiltinRoles`) und braucht deshalb einen Ereigniskanal.
        let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let assembly = fixture_builder(EntryKind::Tui, &fixture)
            .session_events(events)
            .build()
            .expect("Tui montiert");

        let context = registry_model_context(&assembly);
        let shows_agents_doc = |text: &String| {
            text.starts_with("project.doc:AGENTS.md\n") && text.contains(AGENTS_MARKER)
        };
        assert!(context.iter().any(shows_agents_doc), "Tui zeigt AGENTS.md: {context:?}");
        let expected_root = format!(
            "project.root\nproject_root={}\ncwd={}",
            canonical_project.display(),
            canonical_project.display()
        );
        assert!(
            context.iter().any(|text| *text == expected_root),
            "Tui zeigt den erkannten Projekt-Root: {context:?}"
        );
    }

    #[test]
    fn test_narrowing_workspace_root_filters_docs_above_root() {
        const ABOVE: &str = "R2-MARKER-doc-above-bound-root";
        const INSIDE: &str = "R2-MARKER-doc-inside-bound-root";

        let fixture = build_fixture();
        let workspace = fixture.project.join("nested").join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(fixture.project.join("AGENTS.md"), ABOVE).expect("oberes AGENTS.md");
        std::fs::write(workspace.join("AGENTS.md"), INSIDE).expect("inneres AGENTS.md");
        let canonical_workspace = workspace.canonicalize().expect("canonical workspace");

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
            .expect("Plan-Knoten mit gebundenem Workspace montiert");
        assert_eq!(
            assembly.project().docs.len(),
            2,
            "die Erkennung findet beide Dateien; gefiltert wird nur der Modellkontext"
        );

        let context = registry_model_context(&assembly);
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
            context.iter().any(|text| *text == expected_root),
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
        let narrowed =
            registry_project_context(&project, &profile, Some(Path::new("/work/space")));
        let kept: Vec<&str> = narrowed.docs.iter().map(|doc| doc.content.as_str()).collect();
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
        assert_eq!(redacted.project_root, PathBuf::from(PROJECT_CONTEXT_PLACEHOLDER));
        assert_eq!(redacted.cwd, PathBuf::from(PROJECT_CONTEXT_PLACEHOLDER));
    }

    #[test]
    fn test_narrowing_rejects_restricted_profile_with_model_tool_operations() {
        let fixture = build_fixture();
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
            let error = fixture_builder(EntryKind::Doctor, &fixture)
                .narrowing(narrowing_to(requested))
                .build()
                .expect_err("Werkzeugverengung an AllWithModelTools muss abgelehnt werden");
            assert!(
                matches!(error, RuntimeError::Registry { .. }),
                "{requested:?}: {error}"
            );
        }

        // `Full` bleibt zugelassen, ebenso eine Verengung ohne Modell-Tool-Fläche.
        fixture_builder(EntryKind::Doctor, &fixture)
            .narrowing(narrowing_to(RegistryProfile::Full))
            .build()
            .expect("Doctor mit Full montiert");
        fixture_builder(EntryKind::LocalEcho, &fixture)
            .narrowing(narrowing_to(RegistryProfile::ReadOnlyExplore))
            .build()
            .expect("LocalEcho (OperationSurface::None) mit ReadOnlyExplore montiert");

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
        assert_eq!(effective_approval_timeout(&global, &project), Duration::from_secs(60));

        project.approval_timeout_secs = Some(30);
        assert_eq!(effective_approval_timeout(&global, &project), Duration::from_secs(30));
    }

    /// Eine ungültige Extra-Root (hier: nicht existent) wird übersprungen,
    /// nicht abgelehnt — nur der gültige Eintrag landet in der Zelle.
    #[test]
    fn test_seed_extra_roots_skips_invalid_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let primary = dir.path().join("primary");
        std::fs::create_dir_all(&primary).expect("primary");
        let valid = primary.join("valid");
        std::fs::create_dir_all(&valid).expect("valid");
        let missing = primary.join("does-not-exist");

        let mut global = PermissionsSection::default();
        global.extra_roots = vec![valid, missing];

        let cell = seed_extra_roots(&global, &PermissionsSection::default(), &primary, None);
        let snapshot = cell.snapshot();
        assert_eq!(snapshot.len(), 1, "nur der gültige Eintrag bleibt: {snapshot:?}");
    }

    // ── Plan-Dienste: Default an, außer ausdrücklich abgeschaltet (G-024) ────

    /// Ein interaktiver TUI-Lauf ohne jeden Konfigurationseintrag bekommt die
    /// eingebaute Plan-Vorgabe — `/plan` und `/goal` funktionieren ohne
    /// `[tools.plan]` (G-024/G-098). Andere Einstiege bleiben unverändert
    /// ohne eingebaute Vorgabe.
    #[test]
    fn test_resolve_plan_services_defaults_on_for_tui_when_untouched() {
        let fixture = build_fixture();
        let home_project_root =
            discover_home_project(&fixture.project, &[]).expect("Projekt-Home erkannt");
        let project_home = ProjectHome::at(&home_project_root);

        let resolved =
            resolve_plan_services(EntryKind::Tui, None, &PlanSection::default(), &project_home);
        let Some(plan) = resolved else {
            panic!("Tui ohne Config-Eintrag muss die eingebaute Plan-Vorgabe bekommen");
        };
        assert!(plan.plan_config.enabled);

        assert!(
            resolve_plan_services(
                EntryKind::OneShot,
                None,
                &PlanSection::default(),
                &project_home
            )
            .is_none(),
            "die Gate-Semantik anderer Einstiege bleibt unverändert"
        );
    }

    /// Eine berührte Sektion mit `enabled = false` bleibt geschlossen — eine
    /// bewusste Abschaltung wird nie überschrieben. Ein expliziter
    /// Builder-Wert gewinnt dagegen immer, unabhängig von der Konfiguration.
    #[test]
    fn test_resolve_plan_services_stays_off_when_touched_and_disabled() {
        let fixture = build_fixture();
        let home_project_root =
            discover_home_project(&fixture.project, &[]).expect("Projekt-Home erkannt");
        let project_home = ProjectHome::at(&home_project_root);

        let mut section = PlanSection::default();
        section.enabled = false;
        assert!(!plan_section_is_untouched(&section));

        assert!(
            resolve_plan_services(EntryKind::Tui, None, &section, &project_home).is_none(),
            "eine berührte, weiterhin `enabled = false`-Sektion bleibt geschlossen"
        );

        let explicit = default_tui_plan_services(&project_home);
        let findings = Arc::clone(&explicit.findings);
        let resolved = resolve_plan_services(EntryKind::Tui, Some(explicit), &section, &project_home)
            .expect("ein expliziter Builder-Wert bleibt erhalten");
        assert!(Arc::ptr_eq(&resolved.findings, &findings));
    }

    /// Eine berührte, ausdrücklich aktivierte Sektion wird vollständig
    /// übersetzt (Knotenlimits, Exploration-Vorgaben).
    #[test]
    fn test_plan_tool_config_from_section_translates_fields() {
        let mut section = PlanSection::default();
        section.enabled = true;
        section.max_nodes = 12;
        section.require_exploration_for = vec!["coding".to_owned()];

        let config = plan_tool_config_from_section(&section).expect("gültige Sektion übersetzt");
        assert!(config.enabled);
        assert_eq!(config.max_nodes, 12);
        assert_eq!(config.require_exploration_for, vec![PlanNodeKind::Coding]);
    }

    /// Eine ungültige Sektion (`max_nodes = 0`) übersetzt nicht — dieselbe
    /// Prüfung, die [`resolve_plan_services`] fail-soft in `None` auflöst.
    #[test]
    fn test_plan_tool_config_from_section_rejects_zero_max_nodes() {
        let mut section = PlanSection::default();
        section.enabled = true;
        section.max_nodes = 0;
        assert!(plan_tool_config_from_section(&section).is_err());

        let fixture = build_fixture();
        let home_project_root =
            discover_home_project(&fixture.project, &[]).expect("Projekt-Home erkannt");
        let project_home = ProjectHome::at(&home_project_root);
        assert!(
            resolve_plan_services(EntryKind::Tui, None, &section, &project_home).is_none(),
            "eine ungültige, berührte Sektion bleibt fail-soft geschlossen"
        );
    }
}
