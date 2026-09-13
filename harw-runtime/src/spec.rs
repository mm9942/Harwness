//! Eingangsbeschreibung (`RuntimeSpec`) und die einzige Reduktionstabelle
//! (`EntryKind::profile`) für alle `harw`-Einstiege.
//!
//! # Beschreibung
//! Jeder Einstieg (TUI, One-Shot, Web, MCP, Job-Worker, Gateway, …) nennt nur
//! seine [`EntryKind`]; welche Sandbox-Rechte, welches Registry-Profil, welche
//! Operations-Fläche, welche Ask-Auflösung, welcher Spawner und welche
//! Kontext-Decke daraus folgen, legt ausschließlich [`EntryKind::profile`] fest
//! (Vertrag: `docs/remediation/CONTRACTS.md` §runtime-spec). Netzrechte
//! ([`Permission::NetworkAccess`]) vergibt bis Welle W5 (P1.7) **kein** Einstieg.

use std::path::PathBuf;
use std::time::Duration;

use harw_core::mode::InteractionMode;
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_registry_defaults::profile::RegistryProfile;
use harw_sandbox::{Permission, PermissionSet};
use harw_types::{ApprovalActor, Principal, ReasoningEffort};

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
}

impl EntryKind {
    /// Die **einzige** Reduktionstabelle Einstieg → Profil.
    ///
    /// # Tabelle (CONTRACTS.md §runtime-spec)
    /// | Entry | Rechte | Registry / Ops | Ask | Spawner | Decke |
    /// |---|---|---|---|---|---|
    /// | Tui | {R, W, X} | Full + AllWithModelTools | Interactive | BuiltinRoles | LocalRoot |
    /// | OneShot | {R, W, X} | Full + AllWithModelTools | RejectTurn | BuiltinRoles | LocalRoot |
    /// | LocalEcho | {R, W, X} | Full + None | Fail | None | LocalRoot |
    /// | Analyze | {R, W, X} | Full + CommandsOnly | Fail | BuiltinRoles | LocalRoot |
    /// | Doctor | {R, W, X} | Full + AllWithModelTools | Fail | None | LocalRoot |
    /// | Web | {R} | Full + CommandsOnly | Fail | None | Closed |
    /// | McpServe | {} | NoTools + None | BlockJob | None | Closed |
    /// | JobPrompt | {} | NoTools + None | BlockJob | None | Closed |
    /// | JobPlanNode | {R, W} | Full + None | Fail | None | LocalRoot |
    /// | GatewayTelegram / GatewayDream | {} | NoTools + None | Fail | None | Closed |
    ///
    /// `R` = [`Permission::ReadWorkspace`], `W` = [`Permission::WriteWorkspace`],
    /// `X` = [`Permission::ExecuteProcess`]. Für `Web` ist `{R}` die
    /// Spec-Obergrenze; die tier-abhängige Verengung liefert W2B-02
    /// (`permissions_for_tier`). Für `JobPlanNode` ist `{R, W}` die Obergrenze,
    /// die der Plan-Knoten-Vertrag weiter verengen darf.
    ///
    /// Kein Einstieg erhält Netz-, Secret-, Plugin- oder Registry-Leserechte.
    #[must_use]
    pub fn profile(self) -> EntryProfile {
        use Permission::{ExecuteProcess, ReadWorkspace, WriteWorkspace};

        let rwx = || PermissionSet::from_policy([ReadWorkspace, WriteWorkspace, ExecuteProcess]);

        match self {
            EntryKind::Tui => EntryProfile {
                permissions: rwx(),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::AllWithModelTools,
                ask: AskResolution::Interactive,
                spawner: SpawnerPolicy::BuiltinRoles,
                ceiling: CeilingPolicy::LocalRoot,
            },
            EntryKind::OneShot => EntryProfile {
                permissions: rwx(),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::AllWithModelTools,
                ask: AskResolution::RejectTurn,
                spawner: SpawnerPolicy::BuiltinRoles,
                ceiling: CeilingPolicy::LocalRoot,
            },
            EntryKind::LocalEcho => EntryProfile {
                permissions: rwx(),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::None,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::LocalRoot,
            },
            EntryKind::Analyze => EntryProfile {
                permissions: rwx(),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::CommandsOnly,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::BuiltinRoles,
                ceiling: CeilingPolicy::LocalRoot,
            },
            EntryKind::Doctor => EntryProfile {
                permissions: rwx(),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::AllWithModelTools,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::LocalRoot,
            },
            EntryKind::Web => EntryProfile {
                permissions: PermissionSet::from_policy([ReadWorkspace]),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::CommandsOnly,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::Closed,
            },
            EntryKind::McpServe | EntryKind::JobPrompt => EntryProfile {
                permissions: PermissionSet::empty(),
                registry_profile: RegistryProfile::NoTools,
                operations: OperationSurface::None,
                ask: AskResolution::BlockJob,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::Closed,
            },
            EntryKind::JobPlanNode => EntryProfile {
                permissions: PermissionSet::from_policy([ReadWorkspace, WriteWorkspace]),
                registry_profile: RegistryProfile::Full,
                operations: OperationSurface::None,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::LocalRoot,
            },
            EntryKind::GatewayTelegram | EntryKind::GatewayDream => EntryProfile {
                permissions: PermissionSet::empty(),
                registry_profile: RegistryProfile::NoTools,
                operations: OperationSurface::None,
                ask: AskResolution::Fail,
                spawner: SpawnerPolicy::None,
                ceiling: CeilingPolicy::Closed,
            },
        }
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
    /// Werkzeuge, die die Konfigurationspolitik ohne Rückfrage erlaubt.
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

    const ALL: [EntryKind; 11] = [
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
        use Permission::{ExecuteProcess as X, ReadWorkspace as R, WriteWorkspace as W};
        use RegistryProfile as P;
        use SpawnerPolicy as S;

        let expected: [Row; 11] = [
            (
                EntryKind::Tui,
                &[R, W, X],
                P::Full,
                O::AllWithModelTools,
                A::Interactive,
                S::BuiltinRoles,
                C::LocalRoot,
            ),
            (
                EntryKind::OneShot,
                &[R, W, X],
                P::Full,
                O::AllWithModelTools,
                A::RejectTurn,
                S::BuiltinRoles,
                C::LocalRoot,
            ),
            (
                EntryKind::LocalEcho,
                &[R, W, X],
                P::Full,
                O::None,
                A::Fail,
                S::None,
                C::LocalRoot,
            ),
            (
                EntryKind::Analyze,
                &[R, W, X],
                P::Full,
                O::CommandsOnly,
                A::Fail,
                S::BuiltinRoles,
                C::LocalRoot,
            ),
            (
                EntryKind::Doctor,
                &[R, W, X],
                P::Full,
                O::AllWithModelTools,
                A::Fail,
                S::None,
                C::LocalRoot,
            ),
            (
                EntryKind::Web,
                &[R],
                P::Full,
                O::CommandsOnly,
                A::Fail,
                S::None,
                C::Closed,
            ),
            (
                EntryKind::McpServe,
                &[],
                P::NoTools,
                O::None,
                A::BlockJob,
                S::None,
                C::Closed,
            ),
            (
                EntryKind::JobPrompt,
                &[],
                P::NoTools,
                O::None,
                A::BlockJob,
                S::None,
                C::Closed,
            ),
            (
                EntryKind::JobPlanNode,
                &[R, W],
                P::Full,
                O::None,
                A::Fail,
                S::None,
                C::LocalRoot,
            ),
            (
                EntryKind::GatewayTelegram,
                &[],
                P::NoTools,
                O::None,
                A::Fail,
                S::None,
                C::Closed,
            ),
            (
                EntryKind::GatewayDream,
                &[],
                P::NoTools,
                O::None,
                A::Fail,
                S::None,
                C::Closed,
            ),
        ];

        for (kind, perms, registry, ops, ask, spawner, ceiling) in expected {
            let want = EntryProfile {
                permissions: set(perms),
                registry_profile: registry,
                operations: ops,
                ask,
                spawner,
                ceiling,
            };
            assert_eq!(kind.profile(), want, "{kind:?}");
        }
    }

    #[test]
    fn no_entry_grants_network_or_other_elevated_permissions() {
        let forbidden = [
            Permission::NetworkAccess,
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
    fn job_gateway_and_mcp_have_empty_permissions_and_no_tools() {
        let closed = [
            EntryKind::McpServe,
            EntryKind::JobPrompt,
            EntryKind::GatewayTelegram,
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
    fn every_profile_is_subset_of_local_rwx() {
        let rwx = set(&[
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ]);
        for kind in ALL {
            let perms = kind.profile().permissions;
            assert!(perms.is_subset_of(&rwx), "{kind:?}");
        }
    }

    #[test]
    fn only_tui_asks_interactively() {
        for kind in ALL {
            let interactive = kind.profile().ask == AskResolution::Interactive;
            assert_eq!(interactive, kind == EntryKind::Tui, "{kind:?}");
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
        assert_eq!(doctor.permissions, tui.permissions);
        assert_eq!(doctor.registry_profile, tui.registry_profile);
        assert_eq!(doctor.operations, tui.operations);
        assert_eq!(doctor.ask, AskResolution::Fail);
        assert_eq!(doctor.spawner, SpawnerPolicy::None);
    }

    #[test]
    fn rights_snapshot_carries_principal_actor() {
        let principal = Principal::trusted_ingress(
            PrincipalKind::Human,
            "mia",
            IngressSurface::Tui,
            PermissionTier::Owner,
        );
        let spec = RuntimeSpec {
            entry: EntryKind::Tui,
            home: PathBuf::from("/home/mia/.harw"),
            cwd: PathBuf::from("/home/mia/projects/harwness"),
            principal,
            mode_override: Some(InteractionMode::Work),
            active_agent: None,
            reasoning_effort: Some(ReasoningEffort::High),
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
            ["ReadWorkspace", "WriteWorkspace", "ExecuteProcess"]
        );
        assert_eq!(snapshot.clone(), snapshot);
    }
}
