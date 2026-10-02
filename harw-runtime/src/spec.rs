//! Eingangsbeschreibung (`RuntimeSpec`) und die einzige Reduktionstabelle
//! (`EntryKind::profile`) für alle `harw`-Einstiege.
//!
//! # Beschreibung
//! Jeder Einstieg (TUI, One-Shot, Web, MCP, Job-Worker, Gateway, …) nennt nur
//! seine [`EntryKind`]; welche Sandbox-Rechte, welches Registry-Profil, welche
//! Operations-Fläche, welche Ask-Auflösung, welcher Spawner und welche
//! Kontext-Decke daraus folgen, legt ausschließlich [`EntryKind::profile`] fest
//! (Vertrag: `docs/design/runtime-contracts.md` §runtime-spec).
//!
//! # Netz
//! Nur die beiden Nutzeroberflächen [`EntryKind::Tui`] und
//! [`EntryKind::OneShot`] tragen [`Permission::NetworkAccess`] (Runde 3,
//! Welle A2: „UIA-Wurzel egress-gebunden“). Das Recht ist hier nur die
//! Obergrenze; welche Hosts es tatsächlich öffnet, bestimmt
//! [`crate::sandbox::root_network_scope`] aus der Egress-Allowlist der
//! Konfiguration. Ohne Allowlist bleibt der Scope leer und die Montage
//! streicht das Recht wieder (fail-closed). Die UIA-Wurzel selbst registriert
//! trotzdem kein `web.*` (`RegistryProfile::Full` enthält keine
//! Web-Werkzeuge); das Netz ist reine, egress-gebundene Durchreichung an ihre
//! Helfer, deren Netz nie breiter ist als das der Wurzel.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use harw_agent_dsl::roles::AgentRoleId;
use harw_authority::{Permission, PermissionSet};
use harw_core::child_backend::ChildBackend;
use harw_core::mode::InteractionMode;
use harw_extension_api::approval_mode::ApprovalMode;
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_protocol::session_wire::AgentRole;
use harw_registry_defaults::profile::RegistryProfile;
use harw_types::{ApprovalActor, Principal, ReasoningEffort};

use crate::embedded::{EffectiveRights, EmbeddedAgent};

/// Hält ein [`ChildBackend`] in [`RuntimeSpec`], das selbst `Clone`,
/// `Debug`, `PartialEq` und `Eq` bleiben muss (Tests vergleichen ganze
/// Specs). [`ChildBackend`] verlangt keinen dieser Bounds, deshalb bettet
/// dieser Wrapper das Objekt nur ein und vergleicht über Zeigergleichheit
/// (dieselbe `Arc`-Instanz), statt seinen Inhalt zu inspizieren (#22 Welle
/// 3C).
#[derive(Clone)]
pub struct ChildBackendHandle(pub Arc<dyn ChildBackend>);

impl std::fmt::Debug for ChildBackendHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ChildBackendHandle").finish_non_exhaustive()
    }
}

impl PartialEq for ChildBackendHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ChildBackendHandle {}

/// Art des Einstiegs in die Runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// Interaktive Terminal-Oberfläche.
    Tui,
    /// Nicht-interaktiver Einzelauftrag über die CLI.
    OneShot,
    /// Lokaler Echo-/Diagnose-Durchlauf ohne Operationen.
    LocalEcho,
    /// Analyse-Lauf mit Command-Operationen, ohne Rückfragen.
    Analyze,
    /// Diagnose (`harw doctor`): Rechte wie TUI, aber ohne Rückfragen und Spawner.
    Doctor,
    /// Lokale Web-Oberfläche.
    Web,
    /// MCP-Server.
    McpServe,
    /// Durabler Job mit Prompt-Eingabe.
    JobPrompt,
    /// Durabler Job für einen Plan-Knoten.
    JobPlanNode,
    /// Telegram-Gateway.
    GatewayTelegram,
    /// Dream-Gateway.
    GatewayDream,
    /// Ein kompilierter Agent (#22 Welle 3A): Konfiguration, Agent, Skills
    /// und Wissen kommen aus [`RuntimeSpec::embedded`] statt aus `~/.harw`.
    /// [`EntryKind::profile`] liefert dafür nur eine konservative
    /// Rückfallzeile (leere Rechte) — die tatsächliche Zeile entsteht aus dem
    /// Manifest über [`EntryProfile::for_embedded`], die die Montage
    /// (`assembly.rs`) an Stelle von `profile()` aufruft, sobald
    /// [`RuntimeSpec::embedded`] gesetzt ist.
    CompiledAgent,
}

/// Wie eine Rückfrage (`AskUser`) aufgelöst wird, wenn niemand interaktiv
/// antworten kann.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AskResolution {
    /// Eine anwesende Person beantwortet die Rückfrage.
    Interactive,
    /// Der aktuelle Turn wird abgelehnt.
    RejectTurn,
    /// Der Job wird blockiert, bis eine durable Freigabe vorliegt.
    BlockJob,
    /// Der Lauf schlägt fehl.
    Fail,
}

/// Ob und womit Kind-Agenten gespawnt werden dürfen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpawnerPolicy {
    /// Kein Spawner.
    None,
    /// Nur die eingebauten Rollen.
    BuiltinRoles,
}

/// Obergrenze der Kontext-/Konfigurationsabschnitte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CeilingPolicy {
    /// Decke des lokalen Root-Space (`~/.harw`).
    LocalRoot,
    /// Geschlossene Decke: nichts aus lokalem Root-Space.
    Closed,
}

/// Welche Operationen der Einstieg exponiert.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OperationSurface {
    /// Commands und Modell-Tools.
    AllWithModelTools,
    /// Nur Commands.
    CommandsOnly,
    /// Keine Operationen.
    None,
}

/// Ergebnis der Reduktionstabelle für einen Einstieg.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryProfile {
    /// Sandbox-Rechte des Root-Agenten.
    pub permissions: PermissionSet,
    /// Registry-Profil (Werkzeugsatz).
    pub registry_profile: RegistryProfile,
    /// Exponierte Operations-Fläche.
    pub operations: OperationSurface,
    /// Auflösung von Rückfragen.
    pub ask: AskResolution,
    /// Spawner-Politik.
    pub spawner: SpawnerPolicy,
    /// Kontext-Decke.
    pub ceiling: CeilingPolicy,
    /// Ob der Projektkontext (Doku-Kaskade `HARW.md`/`AGENTS.md`/`CLAUDE.md`
    /// sowie `project_root`/`cwd` des Hosts) in den Modellkontext der
    /// Wurzel-Registry gelangt. `false` für Einstiege, deren Ergebnis ein
    /// entfernter oder nicht lokal vertrauenswürdiger Einreicher liest
    /// (Befund Z2d2-R1): die Montage übergibt dann einen Projektkontext ohne
    /// Doku und mit neutralem Platzhalter statt Host-Pfaden.
    pub project_context: bool,
}

impl EntryKind {
    /// Die **einzige** Reduktionstabelle Einstieg → Profil.
    ///
    /// # Tabelle (docs/design/runtime-contracts.md §runtime-spec)
    /// | Entry | Rechte | Registry / Ops | Ask | Spawner | Decke | Projektkontext |
    /// |---|---|---|---|---|---|---|
    /// | Tui | {R, W, X, N} | Full + AllWithModelTools | Interactive | BuiltinRoles | LocalRoot | ja |
    /// | OneShot | {R, W, X, N} | Full + AllWithModelTools | RejectTurn | BuiltinRoles | LocalRoot | ja |
    /// | LocalEcho | {R, W, X} | Full + None | Fail | None | LocalRoot | ja |
    /// | Analyze | {R, W, X} | Full + CommandsOnly | Fail | BuiltinRoles | LocalRoot | ja |
    /// | Doctor | {R, W, X} | Full + AllWithModelTools | Fail | None | LocalRoot | ja |
    /// | Web | {R} | Full + CommandsOnly | Fail | None | Closed | nein |
    /// | McpServe | {} | NoTools + None | BlockJob | None | Closed | nein |
    /// | JobPrompt | {} | NoTools + None | BlockJob | None | Closed | nein |
    /// | JobPlanNode | {R, W} | Full + None | Fail | None | LocalRoot | ja |
    /// | GatewayTelegram | {R, W} | WorkspaceEdit + None | Interactive | None | Closed | nein |
    /// | GatewayDream | {} | NoTools + None | Fail | None | Closed | nein |
    ///
    /// `R` = [`Permission::ReadWorkspace`], `W` = [`Permission::WriteWorkspace`],
    /// `X` = [`Permission::ExecuteProcess`], `N` = [`Permission::NetworkAccess`]
    /// (Hosts nur aus der Egress-Allowlist, siehe Moduldoku „Netz“). Für `Web` ist `{R}` die
    /// Spec-Obergrenze; die tier-abhängige Verengung liefert W2B-02
    /// (`permissions_for_tier`). Für `JobPlanNode` ist `{R, W}` die Obergrenze,
    /// die der Plan-Knoten-Vertrag weiter verengen darf.
    ///
    /// `GatewayTelegram` (Runde 3, Welle D) liest und schreibt im gebundenen
    /// Workspace des Chats, aber ohne Shell und ohne Netz; jede Rückfrage
    /// beantwortet die Person über Freigabe-Buttons im Chat (`Interactive`),
    /// und der Freigabemodus ist für diesen Einstieg unabhängig von der
    /// Konfiguration immer `ask` (`crate::assembly::effective_approval_mode`).
    ///
    /// Kein Einstieg erhält Secret-, Plugin- oder Registry-Leserechte; Netz
    /// tragen nur `Tui` und `OneShot`, und zwar egress-gebunden.
    #[must_use]
    pub fn profile(self) -> EntryProfile {
        use Permission::{ExecuteProcess, NetworkAccess, ReadWorkspace, WriteWorkspace};

        let rwx = || PermissionSet::from_policy([ReadWorkspace, WriteWorkspace, ExecuteProcess]);
        let rwxn = || {
            PermissionSet::from_policy([
                ReadWorkspace,
                WriteWorkspace,
                ExecuteProcess,
                NetworkAccess,
            ])
        };

        match self {
            EntryKind::Tui => EntryProfile {
                permissions: rwxn(),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::AllWithModelTools,
                ask: AskResolution::Interactive,
                spawner: SpawnerPolicy::BuiltinRoles,
                ceiling: CeilingPolicy::LocalRoot,
                project_context: true,
            },
            EntryKind::OneShot => EntryProfile {
                permissions: rwxn(),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::AllWithModelTools,
                ask: AskResolution::RejectTurn,
                spawner: SpawnerPolicy::BuiltinRoles,
                ceiling: CeilingPolicy::LocalRoot,
                project_context: true,
            },
            EntryKind::LocalEcho => EntryProfile {
                permissions: rwx(),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::None,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::LocalRoot,
                project_context: true,
            },
            EntryKind::Analyze => EntryProfile {
                permissions: rwx(),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::CommandsOnly,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::BuiltinRoles,
                ceiling: CeilingPolicy::LocalRoot,
                project_context: true,
            },
            EntryKind::Doctor => EntryProfile {
                permissions: rwx(),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::AllWithModelTools,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::LocalRoot,
                project_context: true,
            },
            EntryKind::Web => EntryProfile {
                permissions: PermissionSet::from_policy([ReadWorkspace]),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::CommandsOnly,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::Closed,
                project_context: false,
            },
            EntryKind::McpServe | EntryKind::JobPrompt => EntryProfile {
                permissions: PermissionSet::empty(),
                registry_profile: RegistryProfile::NoTools,
                operations: OperationSurface::None,
                ask: AskResolution::BlockJob,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::Closed,
                project_context: false,
            },
            EntryKind::JobPlanNode => EntryProfile {
                permissions: PermissionSet::from_policy([ReadWorkspace, WriteWorkspace]),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::None,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::LocalRoot,
                project_context: true,
            },
            EntryKind::GatewayTelegram => EntryProfile {
                permissions: PermissionSet::from_policy([ReadWorkspace, WriteWorkspace]),
                registry_profile: RegistryProfile::WorkspaceEdit,
                operations: OperationSurface::None,
                ask: AskResolution::Interactive,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::Closed,
                project_context: false,
            },
            EntryKind::GatewayDream => EntryProfile {
                permissions: PermissionSet::empty(),
                registry_profile: RegistryProfile::NoTools,
                operations: OperationSurface::None,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::Closed,
                project_context: false,
            },
            // Keine Zeile der Tabelle: das Manifest bestimmt die Rechte
            // (siehe [`EntryProfile::for_embedded`]), nicht ein fester
            // Eintrag. Diese Rückfallzeile gilt nur, falls `profile()` ohne
            // die Rechte aus [`RuntimeSpec::embedded`] aufgerufen wird — sie
            // ist absichtlich die engste Zeile der Tabelle (leere Rechte,
            // kein Werkzeug), nie ein stiller Vollzugriff.
            EntryKind::CompiledAgent => EntryProfile {
                permissions: PermissionSet::empty(),
                registry_profile: RegistryProfile::NoTools,
                operations: OperationSurface::None,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::Closed,
                project_context: false,
            },
        }
    }
}

impl EntryProfile {
    /// Die tatsächliche Zeile für [`EntryKind::CompiledAgent`]: aus den
    /// Manifest-Rechten abgeleitet statt aus einer Tabellenzeile (#22 Welle
    /// 3A).
    ///
    /// # Beschreibung
    /// Bildet [`EffectiveRights`] auf die engste bestehende
    /// [`RegistryProfile`]-Zeile ab, die sie noch abdeckt:
    /// - keine Werkzeuge, kein Schreiben, keine Shell ⇒
    ///   [`RegistryProfile::NoTools`] (leere Rechte, wie [`EntryKind::McpServe`]);
    /// - eine Shell ist admittiert ⇒ [`RegistryProfile::Full`] (die einzige
    ///   bestehende Zeile mit `shell.exec`);
    /// - sonst (mindestens ein Werkzeug oder Schreiben, keine Shell) ⇒
    ///   [`RegistryProfile::WorkspaceEdit`] (lesen/schreiben, keine Shell,
    ///   wie [`EntryKind::GatewayTelegram`]).
    ///
    /// Die Sandbox-Rechte ([`PermissionSet`]) entstehen unabhängig aus den
    /// booleschen Feldern von `rights` selbst, nicht aus der gewählten
    /// `RegistryProfile`-Zeile — das ist die eigentliche „Verengung auf
    /// `rights.tools`": welches einzelne Werkzeug einer (notwendig
    /// gröberen) `RegistryProfile`-Zeile am Ende sichtbar bleibt, entscheidet
    /// die Aktivierung der Sitzung (`SessionActivation`) anhand der IR des
    /// Agenten selbst (`ir.tools.admitted`, bereits identisch mit
    /// `rights.tools`) — hier wird nur die tragende Grobzeile gewählt, nie
    /// eine Rechteklasse erweitert, die `rights` nicht trägt.
    #[must_use]
    pub fn for_embedded(rights: &EffectiveRights) -> EntryProfile {
        use Permission::{ExecuteProcess, NetworkAccess, ReadWorkspace, WriteWorkspace};

        let mut policy = Vec::new();
        if !rights.tools.is_empty() || rights.write || rights.shell {
            policy.push(ReadWorkspace);
        }
        if rights.write {
            policy.push(WriteWorkspace);
        }
        if rights.shell {
            policy.push(ExecuteProcess);
        }
        if rights.network_open && !rights.network_hosts.is_empty() {
            policy.push(NetworkAccess);
        }
        let permissions = PermissionSet::from_policy(policy);

        let registry_profile = if rights.tools.is_empty() && !rights.write && !rights.shell {
            RegistryProfile::NoTools
        } else if rights.shell {
            RegistryProfile::Full
        } else {
            RegistryProfile::WorkspaceEdit
        };

        EntryProfile {
            permissions,
            registry_profile,
            operations: OperationSurface::None,
            ask: AskResolution::Interactive,
            spawner: SpawnerPolicy::BuiltinRoles,
            ceiling: CeilingPolicy::LocalRoot,
            project_context: true,
        }
    }
}

impl EntryProfile {
    /// Die Zeile eines an ein Gateway angebundenen Laufs (R18 D-A, Vertrag
    /// `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §1).
    ///
    /// # Beschreibung
    /// Werkzeuge laufen nur im Gateway; der Agentenprozess selbst führt
    /// nichts aus. Deshalb, ausgehend von der Zeile des Einstiegs:
    /// - [`RegistryProfile::NoTools`] — kein lokaler Werkzeugsatz; die
    ///   Montage registriert daneben nur den entfernten Proxy
    ///   (`crate::assembly::RuntimeAssemblyBuilder::remote_tools`);
    /// - [`OperationSurface::AllWithModelTools`] wird zu
    ///   [`OperationSurface::CommandsOnly`] — Operationen bleiben als
    ///   Befehle erreichbar, aber kein Operations-Modell-Werkzeug läuft
    ///   lokal;
    /// - [`SpawnerPolicy::None`] — Unteragenten bekommen Werkzeuge nur über
    ///   die Delegation der UIA im Gateway, nie über eine lokale
    ///   Kind-Registry.
    ///
    /// Rechte, Rückfrage-Auflösung, Decke und Projektkontext bleiben die des
    /// Einstiegs; ohne lokales Werkzeug nutzt sie kein Ausführer.
    #[must_use]
    pub fn for_tool_gateway(self) -> EntryProfile {
        let operations = match self.operations {
            OperationSurface::AllWithModelTools => OperationSurface::CommandsOnly,
            other => other,
        };
        EntryProfile {
            registry_profile: RegistryProfile::NoTools,
            operations,
            spawner: SpawnerPolicy::None,
            ..self
        }
    }
}

/// Die Wire-Rolle (`harw_protocol::session_wire::AgentRole`) einer
/// Organisationsrolle (R18 §2.2).
///
/// # Beschreibung
/// `harw-protocol` kann nicht von `harw-agent-dsl` abhängen; die Abbildung
/// lebt deshalb an der Composition-Root und ist Wert für Wert gepinnt
/// (`tests/remote_tools.rs`, RP-T5).
#[must_use]
pub const fn wire_agent_role(role: AgentRoleId) -> AgentRole {
    match role {
        AgentRoleId::UserInterface => AgentRole::UserInterface,
        AgentRoleId::RootOrchestrator => AgentRole::RootOrchestrator,
        AgentRoleId::ChildOrchestrator => AgentRole::ChildOrchestrator,
        AgentRoleId::Worker => AgentRole::Worker,
        AgentRoleId::UiaWorker => AgentRole::UiaWorker,
        AgentRoleId::AgentSteward => AgentRole::AgentSteward,
    }
}

/// Die Organisationsrolle einer Wire-Rolle; `None` für
/// [`AgentRole::Unknown`] (fail closed: eine unbekannte Rolle wird nie als
/// bekannte behandelt).
#[must_use]
pub const fn agent_role_from_wire(role: AgentRole) -> Option<AgentRoleId> {
    match role {
        AgentRole::UserInterface => Some(AgentRoleId::UserInterface),
        AgentRole::RootOrchestrator => Some(AgentRoleId::RootOrchestrator),
        AgentRole::ChildOrchestrator => Some(AgentRoleId::ChildOrchestrator),
        AgentRole::Worker => Some(AgentRoleId::Worker),
        AgentRole::UiaWorker => Some(AgentRoleId::UiaWorker),
        AgentRole::AgentSteward => Some(AgentRoleId::AgentSteward),
        AgentRole::Unknown => None,
    }
}

/// Budget des Root-Agenten eines Laufs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootBudget {
    /// Maximale Anzahl Modell-Runden.
    pub max_model_rounds: u32,
    /// Maximale Gesamtzahl Tokens.
    pub max_total_tokens: u64,
    /// Maximale Wanduhrzeit.
    pub max_wall: Duration,
}

/// Vollständige Eingangsbeschreibung eines Runtime-Laufs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSpec {
    /// Einstiegsart; bestimmt über [`EntryKind::profile`] alle Rechte.
    pub entry: EntryKind,
    /// Aufgelöster Root-Space (`~/.harw` bzw. `HARW_HOME`).
    pub home: PathBuf,
    /// Arbeitsverzeichnis des Laufs.
    pub cwd: PathBuf,
    /// Vertrauenswürdig ermittelter Aufrufer.
    pub principal: Principal,
    /// Interaktionsmodus der Wurzelsitzung. Die Montage liest **keine**
    /// Konfigurationsvorgabe: der Aufrufer löst Flag und Konfiguration selbst
    /// auf und setzt das Ergebnis hier (CONTRACTS-W2d2 E6); `None` lässt den
    /// Vorgabemodus der Sitzung unverändert.
    pub mode_override: Option<InteractionMode>,
    /// Wurzel-Agent. Wie beim Modus löst der Aufrufer `--agent` und eine
    /// etwaige Konfigurationsvorgabe selbst auf (E6); `None` heißt „kein
    /// benannter Agent", die Montage ergänzt keinen.
    pub active_agent: Option<String>,
    /// Explizit gewählter Reasoning-Effort.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Explizit gewählter Freigabemodus (z. B. `--approval`). `Some` hat
    /// Vorrang vor `[permissions].default_mode` aus der Konfiguration und der
    /// Vorgabe der Einstiegsart; `None` lässt die Konfiguration entscheiden.
    pub approval_override: Option<ApprovalMode>,
    /// Explizit gewähltes Modell (z. B. `--model`) als Schlüssel, Modell-ID
    /// oder Alias aus der Modellkonfiguration. `load_config` prüft den Wert
    /// und setzt daraus Vorgabemodell und -anbieter des Laufs; ein unbekanntes
    /// Modell ist ein Konfigurationsfehler. `None` lässt die Vorgabe stehen.
    pub model_override: Option<String>,
    /// Ein eingebettetes Agenten-Artefakt (#22 Welle 3A). Gesetzt heißt:
    /// Konfiguration, Agent, Skills und Wissen kommen ausschließlich aus
    /// diesem Artefakt ([`crate::config::load_config_embedded`]) statt aus
    /// `~/.harw`; nur ein optionales Zustandsverzeichnis wird noch für
    /// Sitzungen und Protokolle genutzt. `None` ist der bestehende Pfad
    /// (jeder andere [`EntryKind`]) unverändert.
    pub embedded: Option<Arc<EmbeddedAgent>>,
    /// Ein [`ChildBackend`], über das jedes von
    /// [`crate::children::ChildRegistryFactory`] gebaute
    /// `ManagedAgentSpawner` seine Kinder statt in-process laufen lässt (#22
    /// Welle 3C, z. B. `harw-agent-runner`s `JobChildBackend`). `None` ist
    /// der bestehende Pfad (jeder Kind-Lauf bleibt in-process).
    pub child_backend: Option<ChildBackendHandle>,
}

/// Seiteneffektfreie Momentaufnahme der effektiven Rechte eines montierten
/// Laufs (Diagnose, `doctor`, Tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RightsSnapshot {
    /// Einstiegsart.
    pub entry: EntryKind,
    /// Aufrufer.
    pub principal: Principal,
    /// Freigabe-Akteur (aus [`Principal::approval_actor`]).
    pub approval_actor: Option<ApprovalActor>,
    /// Effektive Sandbox-Rechte, als stabile Namen.
    pub permissions: Vec<String>,
    /// Sichtbare Werkzeugnamen.
    pub tools: Vec<String>,
    /// Approval-Kette in Auswertungsreihenfolge: `(label, kind)`.
    pub approval_chain: Vec<(&'static str, ApprovalHandlerKind)>,
    /// Werkzeuge, für die die Konfigurationspolitik eine Freigabe verlangt
    /// (`[policy].require_approval_for`, sortiert und dublettenfrei; siehe
    /// [`crate::approval::ApprovalChain::config_policy_tools`]).
    pub config_policy_tools: Vec<String>,
    /// Aktive Abschnitte der Kontext-Decke.
    pub ceiling_sections: Vec<String>,
    /// Rollen, die der Spawner erzeugen darf.
    pub spawner_roles: Vec<String>,
    /// Root-Budget.
    pub budget: RootBudget,
    /// Nicht vertrauenswürdiges Repository im Arbeitsverzeichnis, falls erkannt.
    pub untrusted_repo: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::{IngressSurface, PermissionTier, PrincipalKind};

    const ALL: [EntryKind; 12] = [
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
        EntryKind::CompiledAgent,
    ];

    const ALL_PERMISSIONS: [Permission; 7] = [
        Permission::ReadWorkspace,
        Permission::WriteWorkspace,
        Permission::ExecuteProcess,
        Permission::NetworkAccess,
        Permission::ReadSecrets,
        Permission::ManagePlugins,
        Permission::ReadCargoRegistry,
    ];

    /// Erzwingt beim Kompilieren, dass `ALL` jede Variante kennt: eine neue
    /// Variante macht dieses `match` nicht-erschöpfend.
    fn index(kind: EntryKind) -> usize {
        match kind {
            EntryKind::Tui => 0,
            EntryKind::OneShot => 1,
            EntryKind::LocalEcho => 2,
            EntryKind::Analyze => 3,
            EntryKind::Doctor => 4,
            EntryKind::Web => 5,
            EntryKind::McpServe => 6,
            EntryKind::JobPrompt => 7,
            EntryKind::JobPlanNode => 8,
            EntryKind::GatewayTelegram => 9,
            EntryKind::GatewayDream => 10,
            EntryKind::CompiledAgent => 11,
        }
    }

    fn set(perms: &[Permission]) -> PermissionSet {
        PermissionSet::from_policy(perms.iter().copied())
    }

    /// Eine Zeile der Erwartungstabelle in `profile_matches_contract_table`
    /// (vermeidet `clippy::type_complexity` auf dem rohen Tupel-Array-Typ).
    type Row = (
        EntryKind,
        &'static [Permission],
        RegistryProfile,
        OperationSurface,
        AskResolution,
        SpawnerPolicy,
        CeilingPolicy,
        bool,
    );

    #[test]
    fn all_list_is_complete_and_ordered() {
        for (i, kind) in ALL.iter().enumerate() {
            assert_eq!(index(*kind), i);
        }
    }

    #[test]
    fn profile_matches_contract_table() {
        use AskResolution as A;
        use CeilingPolicy as C;
        use OperationSurface as O;
        use Permission::{
            ExecuteProcess as X, NetworkAccess as N, ReadWorkspace as R, WriteWorkspace as W,
        };
        use RegistryProfile as P;
        use SpawnerPolicy as S;

        let expected: [Row; 12] = [
            (
                EntryKind::Tui,
                &[R, W, X, N],
                P::Full,
                O::AllWithModelTools,
                A::Interactive,
                S::BuiltinRoles,
                C::LocalRoot,
                true,
            ),
            (
                EntryKind::OneShot,
                &[R, W, X, N],
                P::Full,
                O::AllWithModelTools,
                A::RejectTurn,
                S::BuiltinRoles,
                C::LocalRoot,
                true,
            ),
            (
                EntryKind::LocalEcho,
                &[R, W, X],
                P::Full,
                O::None,
                A::Fail,
                S::None,
                C::LocalRoot,
                true,
            ),
            (
                EntryKind::Analyze,
                &[R, W, X],
                P::Full,
                O::CommandsOnly,
                A::Fail,
                S::BuiltinRoles,
                C::LocalRoot,
                true,
            ),
            (
                EntryKind::Doctor,
                &[R, W, X],
                P::Full,
                O::AllWithModelTools,
                A::Fail,
                S::None,
                C::LocalRoot,
                true,
            ),
            (
                EntryKind::Web,
                &[R],
                P::Full,
                O::CommandsOnly,
                A::Fail,
                S::None,
                C::Closed,
                false,
            ),
            (
                EntryKind::McpServe,
                &[],
                P::NoTools,
                O::None,
                A::BlockJob,
                S::None,
                C::Closed,
                false,
            ),
            (
                EntryKind::JobPrompt,
                &[],
                P::NoTools,
                O::None,
                A::BlockJob,
                S::None,
                C::Closed,
                false,
            ),
            (
                EntryKind::JobPlanNode,
                &[R, W],
                P::Full,
                O::None,
                A::Fail,
                S::None,
                C::LocalRoot,
                true,
            ),
            (
                EntryKind::GatewayTelegram,
                &[R, W],
                P::WorkspaceEdit,
                O::None,
                A::Interactive,
                S::None,
                C::Closed,
                false,
            ),
            (
                EntryKind::GatewayDream,
                &[],
                P::NoTools,
                O::None,
                A::Fail,
                S::None,
                C::Closed,
                false,
            ),
            (
                EntryKind::CompiledAgent,
                &[],
                P::NoTools,
                O::None,
                A::Fail,
                S::None,
                C::Closed,
                false,
            ),
        ];

        for (kind, perms, registry, ops, ask, spawner, ceiling, project_context) in expected {
            let want = EntryProfile {
                permissions: set(perms),
                registry_profile: registry,
                operations: ops,
                ask,
                spawner,
                ceiling,
                project_context,
            };
            assert_eq!(kind.profile(), want, "{kind:?}");
        }
    }

    #[test]
    fn no_entry_grants_secrets_plugins_or_registry() {
        let forbidden = [
            Permission::ReadSecrets,
            Permission::ManagePlugins,
            Permission::ReadCargoRegistry,
        ];
        for kind in ALL {
            let perms = kind.profile().permissions;
            for p in forbidden {
                assert!(!perms.contains(p), "{kind:?} {p:?}");
            }
        }
    }

    #[test]
    fn only_the_user_interfaces_carry_network() {
        for kind in ALL {
            let networked = kind
                .profile()
                .permissions
                .contains(Permission::NetworkAccess);
            assert_eq!(
                networked,
                matches!(kind, EntryKind::Tui | EntryKind::OneShot),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn telegram_reads_and_writes_but_never_executes_or_networks() {
        let profile = EntryKind::GatewayTelegram.profile();
        assert_eq!(
            profile.permissions,
            set(&[Permission::ReadWorkspace, Permission::WriteWorkspace])
        );
        assert!(!profile.permissions.contains(Permission::ExecuteProcess));
        assert!(!profile.permissions.contains(Permission::NetworkAccess));
        assert_eq!(profile.registry_profile, RegistryProfile::WorkspaceEdit);
        assert_eq!(profile.operations, OperationSurface::None);
        assert_eq!(profile.ask, AskResolution::Interactive);
        assert_eq!(profile.spawner, SpawnerPolicy::None);
        assert_eq!(profile.ceiling, CeilingPolicy::Closed);
        assert!(!profile.project_context);
    }

    #[test]
    fn job_dream_and_mcp_have_empty_permissions_and_no_tools() {
        let closed = [
            EntryKind::McpServe,
            EntryKind::JobPrompt,
            EntryKind::GatewayDream,
        ];
        for kind in closed {
            let profile = kind.profile();
            assert_eq!(profile.permissions, PermissionSet::empty());
            for p in ALL_PERMISSIONS {
                assert!(!profile.permissions.contains(p), "{p:?}");
            }
            assert_eq!(profile.registry_profile, RegistryProfile::NoTools);
            assert_eq!(profile.operations, OperationSurface::None);
            assert_eq!(profile.spawner, SpawnerPolicy::None);
            assert_eq!(profile.ceiling, CeilingPolicy::Closed);
        }
    }

    #[test]
    fn test_profile_project_context_only_for_local_project_entries() {
        let local = [
            EntryKind::Tui,
            EntryKind::OneShot,
            EntryKind::LocalEcho,
            EntryKind::Analyze,
            EntryKind::Doctor,
            EntryKind::JobPlanNode,
        ];
        for kind in ALL {
            assert_eq!(
                kind.profile().project_context,
                local.contains(&kind),
                "{kind:?}"
            );
        }
        // Jeder Einstieg mit geschlossener Decke bekommt keinen Projektkontext.
        for kind in ALL {
            let profile = kind.profile();
            if profile.ceiling == CeilingPolicy::Closed {
                assert!(!profile.project_context, "{kind:?}");
            }
        }
    }

    #[test]
    fn every_profile_is_subset_of_the_tui_profile() {
        let tui = EntryKind::Tui.profile().permissions;
        assert_eq!(
            tui,
            set(&[
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
                Permission::NetworkAccess,
            ])
        );
        for kind in ALL {
            let perms = kind.profile().permissions;
            assert!(perms.is_subset_of(&tui), "{kind:?}");
        }
    }

    #[test]
    fn only_tui_and_telegram_ask_interactively() {
        for kind in ALL {
            let interactive = kind.profile().ask == AskResolution::Interactive;
            assert_eq!(
                interactive,
                matches!(kind, EntryKind::Tui | EntryKind::GatewayTelegram),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn closed_ceiling_never_spawns() {
        for kind in ALL {
            let profile = kind.profile();
            if profile.ceiling == CeilingPolicy::Closed {
                assert_eq!(profile.spawner, SpawnerPolicy::None, "{kind:?}");
            }
        }
    }

    #[test]
    fn doctor_shares_tui_rights_but_not_ask_or_spawner() {
        let tui = EntryKind::Tui.profile();
        let doctor = EntryKind::Doctor.profile();
        // Runde 3: die TUI-Wurzel hat egress-gebundenes Netz, die Diagnose
        // braucht keins — sonst sind die Rechte gleich.
        let tui_without_network = PermissionSet::from_policy(
            tui.permissions
                .iter()
                .filter(|p| *p != Permission::NetworkAccess),
        );
        assert_eq!(doctor.permissions, tui_without_network);
        assert!(!doctor.permissions.contains(Permission::NetworkAccess));
        assert_eq!(doctor.registry_profile, tui.registry_profile);
        assert_eq!(doctor.operations, tui.operations);
        assert_eq!(doctor.ask, AskResolution::Fail);
        assert_eq!(doctor.spawner, SpawnerPolicy::None);
    }

    #[test]
    fn rights_snapshot_carries_principal_actor() {
        let principal = Principal::trusted_ingress(
            PrincipalKind::Human,
            "alice",
            IngressSurface::Tui,
            PermissionTier::Owner,
        );
        let spec = RuntimeSpec {
            entry: EntryKind::Tui,
            home: PathBuf::from("/home/user/.harw"),
            cwd: PathBuf::from("/home/user/projects/harwness"),
            principal,
            mode_override: Some(InteractionMode::Work),
            active_agent: None,
            reasoning_effort: Some(ReasoningEffort::High),
            approval_override: None,
            model_override: None,
            embedded: None,
            child_backend: None,
        };
        let perms = spec.entry.profile().permissions;
        let names: Vec<String> = perms.iter().map(|p| format!("{p:?}")).collect();
        let snapshot = RightsSnapshot {
            entry: spec.entry,
            principal: spec.principal.clone(),
            approval_actor: spec.principal.approval_actor(),
            permissions: names,
            tools: Vec::new(),
            approval_chain: vec![("config-policy", ApprovalHandlerKind::ConfigPolicy)],
            config_policy_tools: Vec::new(),
            ceiling_sections: Vec::new(),
            spawner_roles: Vec::new(),
            budget: RootBudget {
                max_model_rounds: 8,
                max_total_tokens: 100_000,
                max_wall: Duration::from_secs(60),
            },
            untrusted_repo: None,
        };
        let expected_actor = ApprovalActor::Operator {
            id: "local-tui".to_owned(),
        };
        assert_eq!(snapshot.approval_actor, Some(expected_actor));
        assert_eq!(
            snapshot.permissions,
            [
                "ReadWorkspace",
                "WriteWorkspace",
                "ExecuteProcess",
                "NetworkAccess"
            ]
        );
        assert_eq!(snapshot.clone(), snapshot);
    }

    fn rights(
        tools: &[&str],
        network_hosts: &[&str],
        network_open: bool,
        write: bool,
        shell: bool,
    ) -> EffectiveRights {
        EffectiveRights {
            tools: tools.iter().map(|t| (*t).to_owned()).collect(),
            network_hosts: network_hosts.iter().map(|h| (*h).to_owned()).collect(),
            network_open,
            write,
            shell,
            host: false,
            full_access: false,
            budget: None,
        }
    }

    #[test]
    fn for_embedded_picks_no_tools_without_any_right() {
        let profile = EntryProfile::for_embedded(&rights(&[], &[], false, false, false));
        assert_eq!(profile.permissions, PermissionSet::empty());
        assert_eq!(profile.registry_profile, RegistryProfile::NoTools);
    }

    #[test]
    fn for_embedded_picks_workspace_edit_for_read_write_without_shell() {
        let profile =
            EntryProfile::for_embedded(&rights(&["fs.read", "fs.write"], &[], false, true, false));
        assert_eq!(
            profile.permissions,
            set(&[Permission::ReadWorkspace, Permission::WriteWorkspace])
        );
        assert!(!profile.permissions.contains(Permission::ExecuteProcess));
        assert_eq!(profile.registry_profile, RegistryProfile::WorkspaceEdit);
    }

    #[test]
    fn for_embedded_picks_full_only_when_shell_is_admitted() {
        let profile = EntryProfile::for_embedded(&rights(&["shell.exec"], &[], false, false, true));
        assert!(profile.permissions.contains(Permission::ExecuteProcess));
        assert_eq!(profile.registry_profile, RegistryProfile::Full);
    }

    #[test]
    fn for_embedded_grants_network_only_with_open_hosts() {
        let with_hosts = EntryProfile::for_embedded(&rights(
            &["web.fetch"],
            &["example.com"],
            true,
            false,
            false,
        ));
        assert!(with_hosts.permissions.contains(Permission::NetworkAccess));

        let without_hosts =
            EntryProfile::for_embedded(&rights(&["web.fetch"], &[], true, false, false));
        assert!(
            !without_hosts
                .permissions
                .contains(Permission::NetworkAccess)
        );
    }

    #[test]
    fn for_embedded_never_exceeds_the_tui_profile() {
        let tui = EntryKind::Tui.profile().permissions;
        let full_rights = rights(
            &["fs.read", "fs.write", "shell.exec", "web.fetch"],
            &["example.com"],
            true,
            true,
            true,
        );
        let profile = EntryProfile::for_embedded(&full_rights);
        assert!(profile.permissions.is_subset_of(&tui));
    }

    #[test]
    fn for_tool_gateway_removes_every_local_tool_path() {
        for kind in ALL {
            let local = kind.profile();
            let gateway = local.clone().for_tool_gateway();
            assert_eq!(
                gateway.registry_profile,
                RegistryProfile::NoTools,
                "{kind:?}"
            );
            assert_ne!(
                gateway.operations,
                OperationSurface::AllWithModelTools,
                "{kind:?}"
            );
            assert_eq!(gateway.spawner, SpawnerPolicy::None, "{kind:?}");
            assert_eq!(gateway.permissions, local.permissions, "{kind:?}");
            assert_eq!(gateway.ask, local.ask, "{kind:?}");
            assert_eq!(gateway.ceiling, local.ceiling, "{kind:?}");
            assert_eq!(gateway.project_context, local.project_context, "{kind:?}");
        }
        assert_eq!(
            EntryKind::Tui.profile().for_tool_gateway().operations,
            OperationSurface::CommandsOnly
        );
    }
}
