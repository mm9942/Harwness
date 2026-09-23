//! Rollen-Autorität auf Registry-Seite — welches Recht ein Werkzeug braucht,
//! welche Rechte-Obergrenze eine eingebaute Rolle bekommt und welche Werkzeuge
//! unter einem gewährten Rechtesatz überhaupt registriert werden.
//!
//! Spezifikationsquelle: Plan W5 RD (Befunde G-055, F-084), Plan K3
//! (`reduce_to_read_network`/`reduce_to_read_registry` in `harw-core-bridge`).
//!
//! # Verantwortlichkeit
//! Dieses Modul besitzt die **registry-seitige** Hälfte der Rechteverengung:
//! - [`tool_permission`]: das eine Recht, das der Prolog eines eingebauten
//!   Werkzeugs verlangt (belegt an den `#[harw_macros::tool(permission = …)]`-
//!   bzw. `require_permission`-Stellen der Werkzeug-Crates).
//! - [`AuthorityReducer`]: die Obergrenze, auf die ein Kind verengt wird —
//!   `reduce_to_read_only`, `reduce_to_read_registry`, `reduce_to_read_network`.
//! - [`authority_reducer_for_role`]: welche Obergrenze eine eingebaute Rolle
//!   bekommt.
//!
//! Die Sandbox-Hälfte (`SandboxSpec` des Kindes aus der des Elternteils) liegt
//! in `harw-core-bridge` (`agent_tool.rs`, A-BRIDGE); die Kennungen aus
//! [`AuthorityReducer::id`] sind dieselben Strings, damit beide Seiten über
//! denselben Namen sprechen.
//!
//! # Warum Werkzeuge ohne Recht gar nicht registriert werden
//! Ein Werkzeug, dessen Recht die Sandbox nicht trägt, scheitert bei jedem
//! Aufruf am Rechte-Prolog (`deps.source_*` ohne `ReadCargoRegistry`, `web.*`
//! ohne `NetworkAccess`, Register R6/F-084). Es im Inventar zu bewerben kostet
//! Budget und lädt zu Fehlversuchen ein.
//! [`crate::profile::assemble_registry_for_sandbox`] registriert deshalb nur,
//! was [`crate::profile::RegistryProfile::tool_names_for`] unter dem gewährten
//! Satz übrig lässt.
//!
//! # Schlüsseltypen
//! - [`AuthorityReducer`] — geschlossener Satz der Rechte-Obergrenzen.
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind rein und von jedem Thread aus sicher.
//!
//! # Fehler
//! Keine; unbekannte Werkzeug- oder Rollennamen liefern `None` (fail-closed).
//!
//! # Beispiele
//! ```rust
//! use harw_registry_defaults::authority::{
//!     AuthorityReducer, authority_reducer_for_role, reduce_to_read_registry,
//! };
//! use harw_registry_defaults::profile::role_names;
//! use harw_authority::{Permission, PermissionSet};
//!
//! let parent = PermissionSet::from_policy([
//!     Permission::ReadWorkspace,
//!     Permission::WriteWorkspace,
//!     Permission::ReadCargoRegistry,
//!     Permission::NetworkAccess,
//! ]);
//! let child = reduce_to_read_registry(&parent);
//! assert!(child.contains(Permission::ReadCargoRegistry));
//! assert!(!child.contains(Permission::WriteWorkspace));
//! assert!(!child.contains(Permission::NetworkAccess));
//! assert_eq!(
//!     authority_reducer_for_role(role_names::EXPLORER),
//!     Some(AuthorityReducer::ReadRegistry)
//! );
//! ```

use harw_authority::{Permission, PermissionSet};

use crate::profile::{
    AGENT_DEFINITION_LIST_TOOLS, AGENT_DEFINITION_READ_TOOLS, AGENT_DEFINITION_WRITE_TOOLS,
    BROWSER_TOOLS, DEPS_SOURCE_TOOLS, DEPS_WORKSPACE_TOOLS, DOC_TOOLS, EXPLORER_TOOLS,
    FS_READ_ONLY_TOOLS, LENS_TOOLS, PROCESS_TOOLS, SHELL_TOOLS, WEB_TOOLS, role_names,
};

/// Kennung des Reducers „nur Workspace lesen“.
pub const REDUCE_TO_READ_ONLY: &str = "reduce_to_read_only";
/// Kennung des Reducers „Workspace und Registry-Quellcache lesen“.
pub const REDUCE_TO_READ_REGISTRY: &str = "reduce_to_read_registry";
/// Kennung des Reducers „nur Netz, kein Workspace“.
pub const REDUCE_TO_READ_NETWORK: &str = "reduce_to_read_network";

/// Die Rechte-Obergrenze, auf die ein Kind-Agent verengt wird.
///
/// # Beschreibung
/// Ein Reducer **gewährt** nichts: [`AuthorityReducer::reduce`] schneidet den
/// Satz des Elternteils mit [`AuthorityReducer::ceiling`]. Hat der Elternteil
/// ein Recht nicht, bekommt das Kind es auch über den großzügigsten Reducer
/// nicht (monoton, durch Schnittmenge der beiden Mengen).
///
/// Keine Obergrenze enthält `WriteWorkspace`, `ExecuteProcess`, `ReadSecrets`
/// oder `ManagePlugins`. `ReadNetwork` enthält bewusst **kein**
/// `ReadWorkspace` (Annahme A5): wer ins Netz darf, liest keine
/// Workspace-Daten, die er hinaustragen könnte.
///
/// # Varianten
/// - `ReadOnly` — `{ReadWorkspace}`.
/// - `ReadRegistry` — `{ReadWorkspace, ReadCargoRegistry}`.
/// - `ReadNetwork` — `{NetworkAccess}`.
///
/// # Nebenläufigkeit
/// `Copy`, rein; von jedem Thread aus sicher.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::authority::AuthorityReducer;
/// use harw_authority::Permission;
///
/// let ceiling = AuthorityReducer::ReadNetwork.ceiling();
/// assert!(ceiling.contains(Permission::NetworkAccess));
/// assert!(!ceiling.contains(Permission::ReadWorkspace));
/// assert_eq!(AuthorityReducer::ReadNetwork.id(), "reduce_to_read_network");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityReducer {
    /// Nur den Workspace lesen.
    ReadOnly,
    /// Workspace und den lokalen Registry-Quellcache lesen.
    ReadRegistry,
    /// Nur ausgehendes Netz über die Egress-Policy; kein Workspace.
    ReadNetwork,
}

impl AuthorityReducer {
    /// Alle Reducer in Deklarationsreihenfolge (für erschöpfende Tests).
    pub const ALL: &'static [AuthorityReducer] = &[
        AuthorityReducer::ReadOnly,
        AuthorityReducer::ReadRegistry,
        AuthorityReducer::ReadNetwork,
    ];

    /// Liefert die Kennung, unter der `harw-core-bridge` denselben Reducer führt.
    ///
    /// # Rückgabe
    /// [`REDUCE_TO_READ_ONLY`], [`REDUCE_TO_READ_REGISTRY`] oder
    /// [`REDUCE_TO_READ_NETWORK`].
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            AuthorityReducer::ReadOnly => REDUCE_TO_READ_ONLY,
            AuthorityReducer::ReadRegistry => REDUCE_TO_READ_REGISTRY,
            AuthorityReducer::ReadNetwork => REDUCE_TO_READ_NETWORK,
        }
    }

    /// Liefert die Rechte-Obergrenze dieses Reducers.
    ///
    /// # Rückgabe
    /// Ein neuer [`PermissionSet`] mit genau den Rechten der Variante.
    #[must_use]
    pub fn ceiling(self) -> PermissionSet {
        match self {
            AuthorityReducer::ReadOnly => PermissionSet::from_policy([Permission::ReadWorkspace]),
            AuthorityReducer::ReadRegistry => PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::ReadCargoRegistry,
            ]),
            AuthorityReducer::ReadNetwork => {
                PermissionSet::from_policy([Permission::NetworkAccess])
            }
        }
    }

    /// Verengt den Rechtesatz eines Elternteils auf diese Obergrenze.
    ///
    /// # Argumente
    /// - `granted` (`&PermissionSet`): die Rechte des Elternteils; nur geliehen.
    ///
    /// # Rückgabe
    /// `granted ∩ ceiling()` — nie mehr als `granted`, nie mehr als die Obergrenze.
    #[must_use]
    pub fn reduce(self, granted: &PermissionSet) -> PermissionSet {
        let ceiling = self.ceiling();
        PermissionSet::from_policy(
            granted
                .iter()
                .filter(|permission| ceiling.contains(*permission)),
        )
    }
}

/// Verengt `granted` auf `{ReadWorkspace}`.
///
/// # Argumente
/// - `granted` (`&PermissionSet`): Rechte des Elternteils.
///
/// # Rückgabe
/// `granted ∩ {ReadWorkspace}`.
#[must_use]
pub fn reduce_to_read_only(granted: &PermissionSet) -> PermissionSet {
    AuthorityReducer::ReadOnly.reduce(granted)
}

/// Verengt `granted` auf `{ReadWorkspace, ReadCargoRegistry}` — der Reducer für
/// `explorer`, `analyst`, `researcher-deps` (und `planner`).
///
/// # Argumente
/// - `granted` (`&PermissionSet`): Rechte des Elternteils.
///
/// # Rückgabe
/// `granted ∩ {ReadWorkspace, ReadCargoRegistry}`; `deps.source_*` bleibt nur
/// sichtbar, wenn der Elternteil `ReadCargoRegistry` selbst trägt.
#[must_use]
pub fn reduce_to_read_registry(granted: &PermissionSet) -> PermissionSet {
    AuthorityReducer::ReadRegistry.reduce(granted)
}

/// Verengt `granted` auf `{NetworkAccess}` — der Reducer für `researcher-web`.
///
/// # Argumente
/// - `granted` (`&PermissionSet`): Rechte des Elternteils.
///
/// # Rückgabe
/// `granted ∩ {NetworkAccess}`; nie ein Lese- oder Schreibrecht am Workspace.
#[must_use]
pub fn reduce_to_read_network(granted: &PermissionSet) -> PermissionSet {
    AuthorityReducer::ReadNetwork.reduce(granted)
}

/// Liefert den Reducer, den eine eingebaute Rolle bekommt.
///
/// # Beschreibung
/// - `explorer`, `analyst`, `researcher-deps`, `planner` →
///   [`AuthorityReducer::ReadRegistry`]: ihre Profile registrieren
///   `deps.source_*`, deren Prolog `ReadCargoRegistry` verlangt.
/// - `researcher-web` → [`AuthorityReducer::ReadNetwork`].
/// - die vier `security-*-triage`-Rollen → [`AuthorityReducer::ReadOnly`]
///   (Profil `NoTools`, sie brauchen gar kein Recht; `ReadOnly` ist die engste
///   Kennung des Vokabulars).
/// - `memory-steward` → [`AuthorityReducer::ReadRegistry`] (Profil
///   `MemoryStewardship`; nächstliegender Reducer gleicher Lese-Reichweite).
/// - `executor` → [`AuthorityReducer::ReadOnly`] (Profil `ShellExecution`;
///   engste Kennung des Vokabulars, analog den Triage-Rollen).
/// - `uia-worker` → [`AuthorityReducer::ReadOnly`] (Profil
///   `UiaQuickHelper`, Addendum I; **Muster `executor`**, dieselbe
///   Ausnahme — die Rolle registriert `shell.exec`/`web.fetch`, kein
///   Reducer trägt je `ExecuteProcess` oder `NetworkAccess` gemeinsam mit
///   `ReadWorkspace` weiter, siehe die Ausnahme-Begründung unten).
/// - `agent-steward` → [`AuthorityReducer::ReadOnly`] (Profil
///   `AgentStewardship`, Addendum K; ebenfalls **Muster `executor`** — die
///   Rolle registriert die schreibenden Agentendefinitions-Werkzeuge, kein
///   Reducer trägt je `WriteWorkspace` weiter).
/// - `uia-explorer` → [`AuthorityReducer::ReadOnly`] (Profil
///   `UiaExplorer`; **Muster `uia-worker`**, dieselbe Ausnahme — die Rolle
///   registriert `fs.read/list/search/glob/grep` **und** `web.fetch`, kein
///   Reducer trägt je `NetworkAccess` gemeinsam mit `ReadWorkspace` weiter,
///   siehe die Ausnahme-Begründung unten).
/// - `uia-writer` → [`AuthorityReducer::ReadOnly`] (Profil `UiaWriter`;
///   ebenfalls **Muster `uia-worker`** — die Rolle registriert zusätzlich zu
///   `fs.write` (`WriteWorkspace`) auch `web.fetch` (`NetworkAccess`), kein
///   Reducer trägt je `WriteWorkspace` oder `NetworkAccess` gemeinsam mit
///   `ReadWorkspace` weiter).
/// - `uia-shell-worker` → [`AuthorityReducer::ReadOnly`] (Profil
///   `UiaShellWorker`; **Muster `uia-worker`**, dieselbe Ausnahme — die
///   Rolle registriert `fs.read/list/search/glob/grep` **und** `shell.exec`,
///   kein Reducer trägt je `ExecuteProcess` gemeinsam mit `ReadWorkspace`
///   weiter).
///
/// Invariante (Test): Für jede eingebaute Rolle gilt
/// `profile.required_permissions() ⊆ reducer.ceiling()` — keine Rolle bewirbt
/// ein Werkzeug, das ihre Obergrenze nie tragen kann. **Ausnahme:**
/// `memory-steward` (braucht `WriteWorkspace` für `fs.write`), `executor`
/// (braucht `ExecuteProcess` für `shell.exec`), `uia-worker` (braucht
/// zusätzlich zu `ReadWorkspace` auch `ExecuteProcess` für `shell.exec` und
/// `NetworkAccess` für `web.fetch`), `agent-steward` (Addendum K, braucht
/// `WriteWorkspace` für `agents.write_definition`/`agents.write_uia`/
/// `agents.commit_proposal`/`agents.reject_proposal`), `uia-explorer`
/// (braucht zusätzlich zu `ReadWorkspace` auch `NetworkAccess` für
/// `web.fetch`), `uia-writer` (braucht zusätzlich zu `ReadWorkspace` auch
/// `WriteWorkspace` für `fs.write` und `NetworkAccess` für `web.fetch`) und
/// `uia-shell-worker` (braucht zusätzlich zu `ReadWorkspace` auch
/// `ExecuteProcess` für `shell.exec`) — kein Reducer trägt je
/// `WriteWorkspace`, `ExecuteProcess` oder `NetworkAccess` gemeinsam mit
/// `ReadWorkspace` (siehe `test_reduce_never_exceeds_parent_or_ceiling`, das
/// ist Absicht: Delegation gibt nie Schreib-/Ausführungsrecht weiter und
/// `ReadNetwork` schließt `ReadWorkspace` bewusst aus, Annahme A5). Alle
/// sieben Rollen erhalten diese Rechte nicht über diesen
/// Reducer-Mechanismus, sondern über ihre feste Profilzuweisung bei der
/// Registry-Montage ([`crate::profile::assemble_registry_for_sandbox`]); der
/// hier vergebene Reducer bindet nur die verbleibende, weitergebbare
/// Lese-/Netz-Autorität.
///
/// # Argumente
/// - `role` (`&str`): Rollenname, üblicherweise aus [`role_names`].
///
/// # Rückgabe
/// `Some(reducer)` für eine eingebaute Rolle, sonst `None` (fail-closed).
///
/// # Nebenläufigkeit
/// Rein.
#[must_use]
pub fn authority_reducer_for_role(role: &str) -> Option<AuthorityReducer> {
    match role {
        role_names::ROOT_ORCHESTRATOR => Some(AuthorityReducer::ReadRegistry),
        role_names::EXPLORER
        | role_names::ANALYST
        | role_names::RESEARCHER_DEPS
        | role_names::PLANNER => Some(AuthorityReducer::ReadRegistry),
        role_names::RESEARCHER_WEB => Some(AuthorityReducer::ReadNetwork),
        role_names::SECURITY_EGRESS_TRIAGE
        | role_names::SECURITY_BASELINE_TRIAGE
        | role_names::SECURITY_STRUCTURE_TRIAGE
        | role_names::SECURITY_ENDPOINT_TRIAGE
        | role_names::EXECUTOR => Some(AuthorityReducer::ReadOnly),
        role_names::MEMORY_STEWARD => Some(AuthorityReducer::ReadRegistry),
        // Addendum I: dokumentierte Ausnahme nach dem Muster `executor` —
        // `UiaQuickHelper` registriert `shell.exec`/`web.fetch`, die kein
        // Reducer je zusammen mit `ReadWorkspace` weiterträgt.
        role_names::UIA_WORKER => Some(AuthorityReducer::ReadOnly),
        // Addendum K: dokumentierte Ausnahme nach dem Muster `executor`/
        // `uia-worker` — `AgentStewardship` registriert die schreibenden
        // Agentendefinitions-Werkzeuge (`WriteWorkspace`), die kein Reducer
        // je zusammen mit `ReadWorkspace` weiterträgt.
        role_names::AGENT_STEWARD => Some(AuthorityReducer::ReadOnly),
        // Muster `uia-worker`: `UiaExplorer` registriert
        // `fs.read/list/search/glob/grep` **und** `web.fetch`, kein Reducer
        // trägt je `NetworkAccess` zusammen mit `ReadWorkspace` weiter.
        role_names::UIA_EXPLORER => Some(AuthorityReducer::ReadOnly),
        // Muster `uia-worker`: `UiaWriter` registriert zusätzlich `fs.write`
        // (`WriteWorkspace`) neben `web.fetch` (`NetworkAccess`), kein
        // Reducer trägt je `WriteWorkspace` oder `NetworkAccess` zusammen
        // mit `ReadWorkspace` weiter.
        role_names::UIA_WRITER => Some(AuthorityReducer::ReadOnly),
        // Muster `uia-worker`: `UiaShellWorker` registriert
        // `fs.read/list/search/glob/grep` **und** `shell.exec`, kein
        // Reducer trägt je `ExecuteProcess` zusammen mit `ReadWorkspace`
        // weiter.
        role_names::UIA_SHELL_WORKER => Some(AuthorityReducer::ReadOnly),
        _ => None,
    }
}

/// Liefert das Recht, das der Prolog eines eingebauten Werkzeugs verlangt.
///
/// # Beschreibung
/// Belegt an den Werkzeug-Crates (Stand W5 RD):
/// - `fs.read/list/search/glob/grep` → `ReadWorkspace`
///   (`harw-tool-fs/src/{read.rs:97,list.rs:107,search.rs:191,glob.rs:114,grep.rs:256}`),
///   `fs.write` → `WriteWorkspace` (`write.rs:115`).
/// - `doc.read_pdf` → `ReadWorkspace` (`harw-tool-doc/src/tool.rs`, dieselbe
///   Berechtigung wie `fs.read`: Datei muss innerhalb des Workspace liegen).
/// - `shell.exec` → `ExecuteProcess` (`harw-tool-shell/src/exec.rs:574`).
/// - `deps.graph`, `deps.locked` → `ReadWorkspace`, `deps.source_*` →
///   `ReadCargoRegistry` (`harw-tool-deps/src/{graph_tool.rs:195,
///   locked_tool.rs:209, source_tool.rs:803,855,918}`).
/// - `lens.ask` → `ReadWorkspace` (`harw-tool-lens/src/ask_tool.rs:242`).
/// - `web.*` → `NetworkAccess` (`harw-tool-web/src/{fetch.rs:1467,
///   docs_rs.rs:302, crates_io.rs:261}`).
/// - `browser.*` → `NetworkAccess` (`harw-tool-browser/src/harness_provider.rs:239`).
/// - `agents.validate`, `agents.list_proposals` → `ReadWorkspace`;
///   `agents.write_definition`, `agents.write_uia`, `agents.commit_proposal`,
///   `agents.reject_proposal` → `WriteWorkspace` (Addendum K + Nachtrag K/K2,
///   `crate::agent_definition_tools::AgentDefinitionToolProvider`, K-C).
///
/// Die Deps- und Lens-Zuordnung prüft ein Test zusätzlich gegen die
/// `TOOL_PERMISSIONS`-Konstanten der Provider (andere Quelle als diese Tabelle).
///
/// # Argumente
/// - `tool` (`&str`): Werkzeugname.
///
/// # Rückgabe
/// `Some(permission)` für ein eingebautes Werkzeug, sonst `None` — ein
/// unbekanntes Werkzeug gilt als nicht gewährbar und wird nie registriert.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::authority::tool_permission;
/// use harw_authority::Permission;
///
/// assert_eq!(tool_permission("deps.source_read"), Some(Permission::ReadCargoRegistry));
/// assert_eq!(tool_permission("web.fetch"), Some(Permission::NetworkAccess));
/// assert_eq!(tool_permission("plugin.unknown"), None);
/// ```
#[must_use]
pub fn tool_permission(tool: &str) -> Option<Permission> {
    let listed = |list: &[&str]| list.contains(&tool);
    if tool == "fs.write" || listed(AGENT_DEFINITION_WRITE_TOOLS) {
        Some(Permission::WriteWorkspace)
    } else if listed(SHELL_TOOLS) || listed(PROCESS_TOOLS) {
        Some(Permission::ExecuteProcess)
    } else if listed(FS_READ_ONLY_TOOLS)
        || listed(DOC_TOOLS)
        || listed(EXPLORER_TOOLS)
        || listed(DEPS_WORKSPACE_TOOLS)
        || listed(LENS_TOOLS)
        || listed(AGENT_DEFINITION_READ_TOOLS)
        || listed(AGENT_DEFINITION_LIST_TOOLS)
    {
        Some(Permission::ReadWorkspace)
    } else if listed(DEPS_SOURCE_TOOLS) {
        Some(Permission::ReadCargoRegistry)
    } else if listed(WEB_TOOLS) || listed(BROWSER_TOOLS) {
        Some(Permission::NetworkAccess)
    } else {
        None
    }
}

/// Die Rechte, die eine Rolle bräuchte, um jedes Werkzeug ihres Profils nutzen
/// zu können — Hilfsfunktion für
/// [`crate::profile::RegistryProfile::required_permissions`].
pub(crate) fn permissions_of(tools: &[&str]) -> PermissionSet {
    PermissionSet::from_policy(tools.iter().copied().filter_map(tool_permission))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::RegistryProfile;

    fn every_permission() -> PermissionSet {
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
    fn test_tool_permission_matches_deps_provider_declarations() {
        let names = harw_tool_deps::DepsToolProvider::TOOL_NAMES;
        let declared = harw_tool_deps::DepsToolProvider::TOOL_PERMISSIONS;
        assert_eq!(names.len(), declared.len());
        for (name, permission) in names.iter().zip(declared.iter()) {
            assert_eq!(tool_permission(name), *permission, "{name}");
        }
    }

    #[test]
    fn test_tool_permission_matches_process_provider_declarations() {
        let names = harw_tool_process::ProcessToolProvider::TOOL_NAMES;
        let declared = harw_tool_process::ProcessToolProvider::TOOL_PERMISSIONS;
        assert_eq!(names.len(), declared.len());
        for (name, permission) in names.iter().zip(declared.iter()) {
            assert_eq!(tool_permission(name), *permission, "{name}");
        }
    }

    #[test]
    fn test_tool_permission_matches_lens_provider_declarations() {
        let names = harw_tool_lens::LensToolProvider::TOOL_NAMES;
        let declared = harw_tool_lens::LensToolProvider::TOOL_PERMISSIONS;
        for (name, permission) in names.iter().zip(declared.iter()) {
            assert_eq!(tool_permission(name), *permission, "{name}");
        }
    }

    #[test]
    fn test_tool_permission_covers_every_tool_of_every_profile() {
        for profile in RegistryProfile::ALL {
            for tool in profile.registered_tool_names() {
                assert!(
                    tool_permission(tool).is_some(),
                    "{profile:?}: {tool} ohne Recht"
                );
            }
        }
        for tool in BROWSER_TOOLS {
            assert_eq!(tool_permission(tool), Some(Permission::NetworkAccess));
        }
        assert_eq!(
            tool_permission("fs.write"),
            Some(Permission::WriteWorkspace)
        );
        assert_eq!(
            tool_permission("shell.exec"),
            Some(Permission::ExecuteProcess)
        );
        assert_eq!(tool_permission("plan"), None);
    }

    #[test]
    fn test_reduce_never_exceeds_parent_or_ceiling() {
        let all = every_permission();
        let none = PermissionSet::empty();
        for reducer in AuthorityReducer::ALL {
            let reduced = reducer.reduce(&all);
            assert_eq!(reduced, reducer.ceiling(), "{reducer:?}");
            assert!(reducer.reduce(&none).iter().next().is_none(), "{reducer:?}");
            for forbidden in [
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
                Permission::ReadSecrets,
                Permission::ManagePlugins,
            ] {
                assert!(!reduced.contains(forbidden), "{reducer:?}: {forbidden:?}");
            }
        }
    }

    #[test]
    fn test_reduce_to_read_registry_keeps_registry_only_if_parent_has_it() {
        let without = PermissionSet::from_policy([Permission::ReadWorkspace]);
        assert!(!reduce_to_read_registry(&without).contains(Permission::ReadCargoRegistry));
        let with = PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::ReadCargoRegistry,
            Permission::NetworkAccess,
        ]);
        let reduced = reduce_to_read_registry(&with);
        assert!(reduced.contains(Permission::ReadCargoRegistry));
        assert!(!reduced.contains(Permission::NetworkAccess));
    }

    #[test]
    fn test_reduce_to_read_network_never_reads_the_workspace() {
        let reduced = reduce_to_read_network(&every_permission());
        assert_eq!(
            reduced.iter().collect::<Vec<_>>(),
            vec![Permission::NetworkAccess]
        );
    }

    #[test]
    fn test_reduce_to_read_only_is_read_workspace_at_most() {
        let reduced = reduce_to_read_only(&every_permission());
        assert_eq!(
            reduced.iter().collect::<Vec<_>>(),
            vec![Permission::ReadWorkspace]
        );
    }

    #[test]
    fn test_authority_reducer_id_is_distinct_and_named() {
        assert_eq!(AuthorityReducer::ReadOnly.id(), REDUCE_TO_READ_ONLY);
        assert_eq!(AuthorityReducer::ReadRegistry.id(), REDUCE_TO_READ_REGISTRY);
        assert_eq!(AuthorityReducer::ReadNetwork.id(), REDUCE_TO_READ_NETWORK);
    }

    #[test]
    fn test_authority_reducer_for_role_covers_every_role_and_bounds_its_profile()
    -> crate::test_support::TestResult {
        // `memory-steward` (WriteWorkspace), `executor` (ExecuteProcess),
        // `uia-worker` (ExecuteProcess + NetworkAccess, Addendum I),
        // `agent-steward` (WriteWorkspace, Addendum K), `uia-explorer`
        // (NetworkAccess), `uia-writer` (WriteWorkspace + NetworkAccess) und
        // `uia-shell-worker` (ExecuteProcess) sind dokumentierte Ausnahmen
        // von der Untermengen-Invariante: kein Reducer trägt je Schreib-,
        // Ausführungs- oder (zusammen mit ReadWorkspace) Netzrecht weiter
        // (siehe `test_reduce_never_exceeds_parent_or_ceiling`); alle sieben
        // Rollen bekommen diese Rechte über ihre feste Profilzuweisung,
        // nicht über diesen Reducer (siehe Doku bei
        // `authority_reducer_for_role`).
        let exempt_from_subset_bound = [
            role_names::MEMORY_STEWARD,
            role_names::EXECUTOR,
            role_names::UIA_WORKER,
            role_names::AGENT_STEWARD,
            role_names::UIA_EXPLORER,
            role_names::UIA_WRITER,
            role_names::UIA_SHELL_WORKER,
        ];
        for role in role_names::ALL {
            let reducer = authority_reducer_for_role(role).ok_or(
                crate::test_support::TestError::Unexpected(format!(
                    "eingebaute Rolle {role} ohne Reducer"
                )),
            )?;
            let profile = crate::profile::profile_for_role(role).ok_or(
                crate::test_support::TestError::Unexpected(format!(
                    "eingebaute Rolle {role} ohne Profil"
                )),
            )?;
            if exempt_from_subset_bound.contains(role) {
                continue;
            }
            assert!(
                profile
                    .required_permissions()
                    .is_subset_of(&reducer.ceiling()),
                "{role}: {profile:?} braucht mehr, als {reducer:?} je trägt"
            );
        }
        assert_eq!(authority_reducer_for_role("coder"), None);
        assert_eq!(
            authority_reducer_for_role(role_names::RESEARCHER_WEB),
            Some(AuthorityReducer::ReadNetwork)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::MEMORY_STEWARD),
            Some(AuthorityReducer::ReadRegistry)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::UIA_WORKER),
            Some(AuthorityReducer::ReadOnly)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::EXECUTOR),
            Some(AuthorityReducer::ReadOnly)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::AGENT_STEWARD),
            Some(AuthorityReducer::ReadOnly)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::UIA_EXPLORER),
            Some(AuthorityReducer::ReadOnly)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::UIA_WRITER),
            Some(AuthorityReducer::ReadOnly)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::UIA_SHELL_WORKER),
            Some(AuthorityReducer::ReadOnly)
        );
        Ok(())
    }
}
