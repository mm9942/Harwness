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
//!   `reduce_to_read_only`, `reduce_to_read_registry`, `reduce_to_read_network`,
//!   `reduce_to_read_explore`, `reduce_to_read_workspace_network`.
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
//!     authority_reducer_for_role(role_names::ANALYST),
//!     Some(AuthorityReducer::ReadRegistry)
//! );
//! assert_eq!(
//!     authority_reducer_for_role(role_names::EXPLORER),
//!     Some(AuthorityReducer::ReadExplore)
//! );
//! ```

use harw_authority::{Permission, PermissionSet};

use crate::capability_catalog::{
    WORK_DRIVER_ENQUEUE_TOOL, WORK_DRIVER_STATUS_TOOL, WORK_DRIVER_STOP_TOOL,
};
use crate::diary_tools::DiaryToolProvider;
use crate::kanban_tools::KanbanReadToolProvider;
use crate::memory_tools::{MEMORY_RECALL, MEMORY_RECORD};
use crate::palace_tools::PalaceToolProvider;
use crate::profile::{
    AGENT_DEFINITION_LIST_TOOLS, AGENT_DEFINITION_READ_TOOLS, AGENT_DEFINITION_WRITE_TOOLS,
    BROWSER_TOOLS, DEPS_SOURCE_TOOLS, DEPS_WORKSPACE_TOOLS, DOC_TOOLS, EXPLORER_TOOLS,
    FS_READ_ONLY_TOOLS, LATEX_TOOLS, LENS_TOOLS, PROCESS_TOOLS, SHELL_TOOLS, WEB_TOOLS, role_names,
};
use crate::skill_proposal_tools::{
    SKILL_PROPOSAL_DECIDE_TOOLS, SKILL_PROPOSAL_PROPOSE_TOOLS, SKILL_PROPOSAL_READ_TOOLS,
};
use crate::workbench_tools::{WorkbenchReadToolProvider, WorkbenchToolProvider};

/// Kennung des Reducers „nur Workspace lesen“.
pub const REDUCE_TO_READ_ONLY: &str = "reduce_to_read_only";
/// Kennung des Reducers „Workspace und Registry-Quellcache lesen“.
pub const REDUCE_TO_READ_REGISTRY: &str = "reduce_to_read_registry";
/// Kennung des Reducers „nur Netz, kein Workspace“.
pub const REDUCE_TO_READ_NETWORK: &str = "reduce_to_read_network";
/// Kennung des Reducers „Workspace, Registry-Quellcache und Netz lesen“
/// (Explorer mit Websuche).
pub const REDUCE_TO_READ_EXPLORE: &str = "reduce_to_read_explore";
/// Kennung des Reducers „Workspace und Netz lesen“ (UIA-Explorer mit
/// Websuche).
pub const REDUCE_TO_READ_WORKSPACE_NETWORK: &str = "reduce_to_read_workspace_network";

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
/// **Ausnahme Explorer-Netz** (Nutzerentscheidungen „der Explorer durchsucht
/// auch das Internet“ und „die UIA-Helfer recherchieren kurz online und
/// fügen manchmal Abhängigkeiten hinzu“): `ReadExplore` und
/// `ReadWorkspaceNetwork` tragen `NetworkAccess` gemeinsam mit
/// `ReadWorkspace` — ausschließlich für die Rollen, deren TOML
/// `web.fetch`/`web.search` admittiert und die das per
/// [`authority_reducer_for_role`] zugewiesen bekommen (`explorer`,
/// `uia-worker`, `uia-writer` → `ReadExplore`; `uia-explorer` →
/// `ReadWorkspaceNetwork`). Der Host-Scope des Kindes bleibt dabei an die
/// Egress-Policy gebunden: die Sandbox-Hälfte in `harw-core-bridge` reicht nur
/// den (egress-gebundenen) Scope des Elternteils als Obergrenze durch, genau
/// wie bei `ReadNetwork` (`researcher-web`); ein über den Handoff gestartetes
/// Kind erbt Sandbox und Host-Scope des Elternteils unverändert
/// (`ensure_child_of`), nie mehr. `ReadRegistry` (`analyst`,
/// `researcher-deps`, `planner` …) und `ReadOnly` (Triage-Rollen …) bleiben
/// ohne Netz.
///
/// # Varianten
/// - `ReadOnly` — `{ReadWorkspace}`.
/// - `ReadRegistry` — `{ReadWorkspace, ReadCargoRegistry}`.
/// - `ReadNetwork` — `{NetworkAccess}`.
/// - `ReadExplore` — `{ReadWorkspace, ReadCargoRegistry, NetworkAccess}`.
/// - `ReadWorkspaceNetwork` — `{ReadWorkspace, NetworkAccess}`.
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
    /// Workspace und Registry-Quellcache lesen plus ausgehendes Netz über die
    /// Egress-Policy (`explorer`, `uia-worker`, `uia-writer`).
    ReadExplore,
    /// Workspace lesen plus ausgehendes Netz über die Egress-Policy
    /// (`uia-explorer`, `dependency-researcher`, `researcher`).
    ReadWorkspaceNetwork,
}

impl AuthorityReducer {
    /// Alle Reducer in Deklarationsreihenfolge (für erschöpfende Tests).
    pub const ALL: &'static [AuthorityReducer] = &[
        AuthorityReducer::ReadOnly,
        AuthorityReducer::ReadRegistry,
        AuthorityReducer::ReadNetwork,
        AuthorityReducer::ReadExplore,
        AuthorityReducer::ReadWorkspaceNetwork,
    ];

    /// Liefert die Kennung, unter der `harw-core-bridge` denselben Reducer führt.
    ///
    /// # Rückgabe
    /// [`REDUCE_TO_READ_ONLY`], [`REDUCE_TO_READ_REGISTRY`],
    /// [`REDUCE_TO_READ_NETWORK`], [`REDUCE_TO_READ_EXPLORE`] oder
    /// [`REDUCE_TO_READ_WORKSPACE_NETWORK`].
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            AuthorityReducer::ReadOnly => REDUCE_TO_READ_ONLY,
            AuthorityReducer::ReadRegistry => REDUCE_TO_READ_REGISTRY,
            AuthorityReducer::ReadNetwork => REDUCE_TO_READ_NETWORK,
            AuthorityReducer::ReadExplore => REDUCE_TO_READ_EXPLORE,
            AuthorityReducer::ReadWorkspaceNetwork => REDUCE_TO_READ_WORKSPACE_NETWORK,
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
            AuthorityReducer::ReadExplore => PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::ReadCargoRegistry,
                Permission::NetworkAccess,
            ]),
            AuthorityReducer::ReadWorkspaceNetwork => {
                PermissionSet::from_policy([Permission::ReadWorkspace, Permission::NetworkAccess])
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
/// `analyst`, `researcher-deps` (und `planner`); bewusst ohne Netz.
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

/// Verengt `granted` auf `{ReadWorkspace, ReadCargoRegistry, NetworkAccess}` —
/// der Reducer für `explorer` (Workspace, Dependency-Quellen und Websuche)
/// sowie für den weitergebbaren Lese-/Netzanteil von `uia-worker` und
/// `uia-writer`.
///
/// # Argumente
/// - `granted` (`&PermissionSet`): Rechte des Elternteils.
///
/// # Rückgabe
/// `granted ∩ {ReadWorkspace, ReadCargoRegistry, NetworkAccess}`;
/// `NetworkAccess` bleibt nur, wenn der Elternteil es selbst trägt.
#[must_use]
pub fn reduce_to_read_explore(granted: &PermissionSet) -> PermissionSet {
    AuthorityReducer::ReadExplore.reduce(granted)
}

/// Verengt `granted` auf `{ReadWorkspace, NetworkAccess}` — der Reducer für
/// `uia-explorer` (Workspace und Websuche, kein Registry-Quellcache).
///
/// # Argumente
/// - `granted` (`&PermissionSet`): Rechte des Elternteils.
///
/// # Rückgabe
/// `granted ∩ {ReadWorkspace, NetworkAccess}`.
#[must_use]
pub fn reduce_to_read_workspace_network(granted: &PermissionSet) -> PermissionSet {
    AuthorityReducer::ReadWorkspaceNetwork.reduce(granted)
}

/// Liefert den Reducer, den eine eingebaute Rolle bekommt.
///
/// # Beschreibung
/// - `explorer` → [`AuthorityReducer::ReadExplore`]: sein Profil
///   `ReadOnlyExplore` registriert `deps.source_*` (`ReadCargoRegistry`) und
///   seine TOML admittiert `web.fetch`/`web.search` (`NetworkAccess`).
/// - `analyst`, `researcher-deps`, `planner` →
///   [`AuthorityReducer::ReadRegistry`]: ihre Profile registrieren
///   `deps.source_*`, deren Prolog `ReadCargoRegistry` verlangt; ihre TOML
///   verbietet `web.*`, sie bekommen deshalb kein Netz.
/// - `researcher-web` → [`AuthorityReducer::ReadNetwork`].
/// - `dependency-researcher`, `researcher` →
///   [`AuthorityReducer::ReadWorkspaceNetwork`] (Profil `ReadOnlyResearch`:
///   `fs.read/list/search/glob/grep`, `doc.read_pdf`, `explore.*`,
///   `web.fetch`/`web.search` — die Obergrenze trägt genau `ReadWorkspace`
///   und `NetworkAccess`, kein `ReadCargoRegistry`).
/// - die vier `security-*-triage`-Rollen → [`AuthorityReducer::ReadOnly`]
///   (Profil `NoTools`, sie brauchen gar kein Recht; `ReadOnly` ist die engste
///   Kennung des Vokabulars).
/// - die vier Matrix-Game-Sitze `matrix-player`, `matrix-umpire`,
///   `matrix-market`, `matrix-redcell` → [`AuthorityReducer::ReadOnly`] (Profil
///   `MatrixReader`: lesende `fs.*` plus `doc.read_pdf`, braucht genau
///   `ReadWorkspace` — die Obergrenze von `ReadOnly`; nie Netz, nie
///   Schreiben, nie Exec).
/// - `memory-steward` → [`AuthorityReducer::ReadRegistry`] (Profil
///   `MemoryStewardship`; nächstliegender Reducer gleicher Lese-Reichweite).
/// - `executor` → [`AuthorityReducer::ReadOnly`] (Profil `ShellExecution`;
///   engste Kennung des Vokabulars, analog den Triage-Rollen).
/// - `uia-worker` → [`AuthorityReducer::ReadExplore`] (Profil
///   `UiaQuickHelper`, Addendum I + Nutzerentscheidung „kurz online
///   recherchieren, manchmal Abhängigkeiten hinzufügen“): die Obergrenze
///   trägt den Workspace-Lesezugriff, den Registry-Quellcache für
///   `deps.source_*` und `NetworkAccess` für `web.fetch`/`web.search`/
///   `browser.open`. `shell.exec` (`ExecuteProcess`) bleibt die
///   dokumentierte **Ausnahme nach Muster `executor`**, siehe unten.
/// - `agent-steward` → [`AuthorityReducer::ReadOnly`] (Profil
///   `AgentStewardship`, Addendum K; ebenfalls **Muster `executor`** — die
///   Rolle registriert die schreibenden Agentendefinitions-Werkzeuge, kein
///   Reducer trägt je `WriteWorkspace` weiter).
/// - `uia-explorer` → [`AuthorityReducer::ReadWorkspaceNetwork`] (Profil
///   `UiaExplorer`: `fs.read/list/search/glob/grep`, `doc.read_pdf`,
///   `explore.*` und `web.fetch`/`web.search` — die Obergrenze trägt genau
///   `ReadWorkspace` und `NetworkAccess`).
/// - `uia-writer` → [`AuthorityReducer::ReadExplore`] (Profil `UiaWriter`;
///   dieselbe Nutzerentscheidung wie `uia-worker`): Workspace lesen,
///   `deps.*` inklusive `deps.source_*` und `web.fetch`/`web.search`.
///   `fs.write` (`WriteWorkspace`) bleibt die dokumentierte **Ausnahme nach
///   Muster `executor`**, siehe unten.
/// - `uia-shell-worker` → [`AuthorityReducer::ReadOnly`] (Profil
///   `UiaShellWorker`; **Muster `uia-worker`**, dieselbe Ausnahme — die
///   Rolle registriert `fs.read/list/search/glob/grep` **und** `shell.exec`,
///   kein Reducer trägt je `ExecuteProcess` gemeinsam mit `ReadWorkspace`
///   weiter).
/// - `uia-latex-writer` → [`AuthorityReducer::ReadOnly`] (Profil
///   `UiaLatexWriter`, Runde 4 Teil E): Workspace lesen, nie Netz.
///   `fs.write` (`WriteWorkspace`) und `latex.build` (`ExecuteProcess`)
///   bleiben die dokumentierte **Ausnahme nach Muster `executor`**.
///
/// Invariante (Test): Für jede eingebaute Rolle gilt
/// `profile.required_permissions() ⊆ reducer.ceiling()` — keine Rolle bewirbt
/// ein Werkzeug, das ihre Obergrenze nie tragen kann. **Teilausnahme
/// Explorer-Netz:** `ReadOnlyExplore` wird auch von `analyst` und
/// `researcher-deps` genutzt und registriert `web.fetch`/`web.search`
/// (`crate::profile::EXPLORER_WEB_TOOLS`); beide Rollen verbieten sie in ihrer
/// TOML und bekommen mit `ReadRegistry` bewusst kein `NetworkAccess` — für
/// sie gilt die Invariante ohne genau diese zwei Werkzeuge. `explorer`
/// (`ReadExplore`) und `uia-explorer` (`ReadWorkspaceNetwork`) erfüllen sie
/// vollständig; `uia-worker`/`uia-writer` (`ReadExplore`) bis auf ihr eines
/// Schreib- bzw. Ausführungsrecht. **Ausnahme:**
/// `memory-steward` (braucht `WriteWorkspace` für `fs.write`), `executor`
/// (braucht `ExecuteProcess` für `shell.exec`), `uia-worker` (braucht
/// über `ReadExplore` hinaus nur noch `ExecuteProcess` für `shell.exec`),
/// `agent-steward` (Addendum K, braucht
/// `WriteWorkspace` für `agents.write_definition`/`agents.write_uia`/
/// `agents.commit_proposal`/`agents.reject_proposal`), `uia-writer` (braucht
/// über `ReadExplore` hinaus nur noch `WriteWorkspace` für `fs.write`) und
/// `uia-shell-worker` (braucht
/// zusätzlich zu `ReadWorkspace` auch `ExecuteProcess` für `shell.exec`)
/// und `uia-latex-writer` (braucht über `ReadOnly` hinaus nur
/// `WriteWorkspace` für `fs.write` und `ExecuteProcess` für `latex.build`) —
/// kein Reducer trägt je `WriteWorkspace` oder `ExecuteProcess` weiter
/// (siehe `test_reduce_never_exceeds_parent_or_ceiling`, das ist Absicht:
/// Delegation gibt nie Schreib-/Ausführungsrecht weiter), und
/// `NetworkAccess` gemeinsam mit `ReadWorkspace` tragen nur `ReadExplore`
/// und `ReadWorkspaceNetwork` (siehe
/// `test_only_web_admitting_roles_get_network_access`). Diese sieben Rollen
/// erhalten die fehlenden Rechte nicht über diesen Reducer-Mechanismus,
/// sondern über ihre feste Profilzuweisung bei der Registry-Montage
/// ([`crate::profile::assemble_registry_for_sandbox`]); der hier vergebene
/// Reducer bindet nur die verbleibende, weitergebbare Lese- und
/// (bei `ReadExplore`) egress-gebundene Netz-Autorität.
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
        // Child-Orchestratoren (Plan Punkt 1) teilen das Profil `Planning`
        // des Root-Orchestrators: read-only, `deps.source_*` braucht
        // `ReadCargoRegistry`, kein eigenes `web.*`.
        role_names::CODING_ORCHESTRATOR | role_names::ANALYSIS_ORCHESTRATOR => {
            Some(AuthorityReducer::ReadRegistry)
        }
        // Dokumentierte Durchreichung: `research-orchestrator` startet
        // `researcher-web`/`researcher`/`dependency-researcher`, deren Sandbox
        // stets eine Teilmenge SEINER Sandbox ist. Ohne `NetworkAccess` im
        // eigenen Satz bekämen seine Netz-Rechercheure nie Netz. Er selbst
        // registriert kein Netz-Werkzeug (`Planning`) und verbietet `web.*` in
        // seiner TOML — das Recht ist reine, egress-gebundene Durchreichung
        // (`test_research_orchestrator_passes_network_through_without_web_tools`).
        role_names::RESEARCH_ORCHESTRATOR => Some(AuthorityReducer::ReadExplore),
        // Explorer-Netz: die TOML admittiert `web.fetch`/`web.search`.
        role_names::EXPLORER => Some(AuthorityReducer::ReadExplore),
        role_names::ANALYST | role_names::RESEARCHER_DEPS | role_names::PLANNER => {
            Some(AuthorityReducer::ReadRegistry)
        }
        role_names::RESEARCHER_WEB => Some(AuthorityReducer::ReadNetwork),
        // Ökosystem-neutrale bzw. allgemeine Recherche
        // (`RegistryProfile::ReadOnlyResearch`): Workspace-Dokumente lesen
        // plus egress-gebundenes Netz, kein Registry-Quellcache (die Rollen
        // registrieren kein `deps.*`) — trägt ihr ganzes Profil.
        role_names::DEPENDENCY_RESEARCHER | role_names::RESEARCHER => {
            Some(AuthorityReducer::ReadWorkspaceNetwork)
        }
        role_names::SECURITY_EGRESS_TRIAGE
        | role_names::SECURITY_BASELINE_TRIAGE
        | role_names::SECURITY_STRUCTURE_TRIAGE
        | role_names::SECURITY_ENDPOINT_TRIAGE
        | role_names::EXECUTOR => Some(AuthorityReducer::ReadOnly),
        // Die vier Matrix-Game-Sitze (Profil `MatrixReader`): brauchen genau
        // `ReadWorkspace` für ihre Unterlagen; `ReadOnly` trägt genau das —
        // nie Netz, nie Schreiben, nie Exec.
        role_names::MATRIX_PLAYER
        | role_names::MATRIX_UMPIRE
        | role_names::MATRIX_MARKET
        | role_names::MATRIX_REDCELL => Some(AuthorityReducer::ReadOnly),
        role_names::MEMORY_STEWARD => Some(AuthorityReducer::ReadRegistry),
        // Nutzerentscheidung „kurz online recherchieren, manchmal
        // Abhängigkeiten hinzufügen“: `UiaQuickHelper` registriert
        // `deps.*` und `web.fetch`/`web.search`, die TOML admittiert sie —
        // Netz nur egress-gebunden und nie weiter als beim Elternteil.
        // `shell.exec` bleibt die dokumentierte Ausnahme nach dem Muster
        // `executor` (Addendum I): kein Reducer trägt `ExecuteProcess`.
        role_names::UIA_WORKER => Some(AuthorityReducer::ReadExplore),
        // Addendum K: dokumentierte Ausnahme nach dem Muster `executor`/
        // `uia-worker` — `AgentStewardship` registriert die schreibenden
        // Agentendefinitions-Werkzeuge (`WriteWorkspace`), die kein Reducer
        // je zusammen mit `ReadWorkspace` weiterträgt.
        role_names::AGENT_STEWARD => Some(AuthorityReducer::ReadOnly),
        // Explorer-Netz: `UiaExplorer` registriert
        // `fs.read/list/search/glob/grep`, `explore.*` **und**
        // `web.fetch`/`web.search`, die TOML admittiert beide.
        role_names::UIA_EXPLORER => Some(AuthorityReducer::ReadWorkspaceNetwork),
        // Dieselbe Nutzerentscheidung wie `uia-worker`: `UiaWriter`
        // registriert `deps.*` und `web.fetch`/`web.search`. `fs.write`
        // bleibt die dokumentierte Ausnahme nach dem Muster `executor`:
        // kein Reducer trägt `WriteWorkspace`.
        role_names::UIA_WRITER => Some(AuthorityReducer::ReadExplore),
        // Muster `uia-worker`: `UiaShellWorker` registriert
        // `fs.read/list/search/glob/grep` **und** `shell.exec`, kein
        // Reducer trägt je `ExecuteProcess` zusammen mit `ReadWorkspace`
        // weiter.
        role_names::UIA_SHELL_WORKER => Some(AuthorityReducer::ReadOnly),
        // Runde 4, Teil E: `UiaLatexWriter` registriert lesende `fs.*`,
        // `fs.write`, `doc.read_pdf` und `latex.build` — kein Netz.
        // `fs.write`/`latex.build` bleiben die dokumentierte Ausnahme nach dem
        // Muster `executor`: kein Reducer trägt `WriteWorkspace` oder
        // `ExecuteProcess` weiter.
        role_names::UIA_LATEX_WRITER => Some(AuthorityReducer::ReadOnly),
        // Runde 7, Teil M: der Game Master liest höchstens einen Brief im
        // Workspace (`MatrixReader`) — kein Netz, kein Schreiben, keine
        // Ausführung. Seine Sitz-Agenten bekommen ihre Unterlagen-Sicht vom
        // Runner (`SandboxSpec::harness_read_view`), nie mehr als er selbst.
        role_names::MATRIX_GAME_MASTER => Some(AuthorityReducer::ReadOnly),
        _ => None,
    }
}

/// Liefert die in `[delegation].targets` einer eingebauten Agentendefinition
/// zugelassenen Delegationsziele (Plan Punkt 1, `delegate_wave`).
///
/// # Beschreibung
/// Die Liste ist eine **zusätzliche** Schnittmenge zur Laufzeit-Sichtbarkeit
/// (`harw_core::delegation_visibility`, `docs/design/
/// delegation-capabilities.md`: „DefinitionDeclaredTargets ∩
/// RoleMatrixTargets ∩ …“) — sie erweitert nie, sie verengt nur. Gelesen wird
/// ausschließlich die eingebaute Schicht (`agents/**/*.toml`, über
/// [`crate::embedded_agents::builtin_agent_toml`]); das Ergebnis wird einmal
/// pro Prozess gecacht.
///
/// # Argumente
/// - `role` (`&str`): Rollenname der delegierenden Sitzung.
///
/// # Rückgabe
/// `Some(targets)`, wenn die eingebaute Definition von `role` eine
/// `[delegation]`-Tabelle mit `targets` trägt (auch eine leere Liste — dann
/// ist KEIN Ziel zugelassen); `None`, wenn es keine eingebaute Definition oder
/// keine solche Tabelle gibt. Ein Aufrufer, der daraus eine Admission ableitet,
/// behandelt `None` fail-closed (kein Ziel).
///
/// # Fehler
/// Keine: eine Definition, die nicht parst, trägt schlicht keine Liste — die
/// Parse-Prüfung selbst gehört `builtin_agent_definitions`.
///
/// # Nebenläufigkeit
/// Die Tabelle entsteht höchstens einmal (`OnceLock`); danach reine Lesesicht.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::authority::delegation_targets_for_role;
/// use harw_registry_defaults::profile::role_names;
///
/// let targets = delegation_targets_for_role(role_names::CODING_ORCHESTRATOR)
///     .unwrap_or_default();
/// assert!(targets.iter().any(|target| target == role_names::EXECUTOR));
/// assert!(delegation_targets_for_role(role_names::EXPLORER).is_none());
/// ```
#[must_use]
pub fn delegation_targets_for_role(role: &str) -> Option<Vec<String>> {
    builtin_delegation_targets().get(role).cloned()
}

/// Die gecachte Tabelle `Rolle → [delegation].targets` der eingebauten Schicht.
fn builtin_delegation_targets() -> &'static std::collections::HashMap<String, Vec<String>> {
    static TABLE: std::sync::OnceLock<std::collections::HashMap<String, Vec<String>>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = std::collections::HashMap::new();
        for (name, source) in crate::embedded_agents::builtin_agent_toml() {
            let Ok(raw) = harw_agent_dsl::parse::parse_toml(source) else {
                continue;
            };
            if let Some(targets) = delegation_targets_of(&raw.tables) {
                table.insert((*name).to_owned(), targets);
            }
        }
        table
    })
}

/// Liest `[delegation].targets` aus den freien Tabellen einer Definition.
///
/// # Rückgabe
/// `Some(liste)` nur, wenn `targets` ein Array ist; Nicht-String-Einträge
/// werden verworfen (sie können kein Rollenname sein).
fn delegation_targets_of(tables: &toml::Table) -> Option<Vec<String>> {
    let targets = tables
        .get("delegation")?
        .as_table()?
        .get("targets")?
        .as_array()?;
    Some(
        targets
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect(),
    )
}

/// Liefert das Recht, das der Prolog eines eingebauten Werkzeugs verlangt.
///
/// # Beschreibung
/// Belegt an den Werkzeug-Crates (Stand W5 RD):
/// - `fs.read/list/search/glob/grep` → `ReadWorkspace`
///   (`harw-tool-fs/src/{read.rs:97,list.rs:107,search.rs:191,glob.rs:114,grep.rs:256}`),
///   `fs.write` → `WriteWorkspace` (`write.rs:115`); ebenso `fs.edit`
///   (`edit.rs`, Runde 5, Teil D).
/// - `doc.read_pdf` → `ReadWorkspace` (`harw-tool-doc/src/tool.rs`, dieselbe
///   Berechtigung wie `fs.read`: Datei muss innerhalb des Workspace liegen).
/// - `shell.exec` → `ExecuteProcess` (`harw-tool-shell/src/exec.rs:574`).
/// - `job.start` → `ExecuteProcess` (derselbe Startweg wie `shell.exec`);
///   `job.status/logs/stop/list/wait` → `ReadWorkspace` (Plan R9, Teil F:
///   kein Prozessstart, nur eigene Jobs bzw. die der Nachfahren).
/// - `tunnel.start` → `ExecuteProcess` (harw-tool-tunnel-v1, geplant:
///   startet einen verwalteten SSH-Prozess über denselben Startweg wie
///   `job.start`/`shell.exec`); `tunnel.status/stop/list` →
///   `ReadWorkspace` (starten nichts, lesen bzw. beenden nur verwaltete
///   Tunnels des Aufrufers — Wiring folgt im Plan-Knoten `job-lifecycle`;
///   bis dahin ist `TUNNEL_TOOLS` nur die statische Vertrags-Obermenge).
/// - `work_driver.enqueue` → `ExecuteProcess` (R14, derselbe Startweg wie
///   `job.start`/`shell.exec`: startet einen dauerhaften Hintergrund-Job, der
///   die `[work_driver] verify`-Kommandos ausführt und Worker-Agenten
///   antreibt); `work_driver.status/stop` → `ReadWorkspace` (liest nur den
///   Job bzw. beendet nur einen eigenen Job über den Job-Cancel-Pfad, wie
///   `job.status`/`job.stop`). Die von `work_driver.enqueue` gestarteten
///   Worker bekommen `WriteWorkspace` zusätzlich, aber nur auf ihre eigenen
///   Scope-Pfade beschränkt — das regelt der Worker-Spawner, nicht dieser
///   Rechtefilter, der pro Werkzeug nur ein Recht liefert.
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
/// - `skills.validate`, `skills.list_proposals` → `ReadWorkspace`;
///   `skills.propose`, `skills.commit_proposal`, `skills.reject_proposal` →
///   `WriteWorkspace` (`crate::skill_proposal_tools::SkillProposalToolProvider`).
/// - `workbench.note`, `workbench.hypothesis` → `ReadWorkspace`
///   (`crate::workbench_tools::WorkbenchToolProvider::TOOL_PERMISSIONS`).
/// - Lesende Wissenswerkzeuge (Plan Teil D) → `ReadWorkspace`:
///   `workbench.show` (`crate::workbench_tools::WorkbenchReadToolProvider`),
///   `diary.read` (`crate::diary_tools::DiaryToolProvider`, nur eigene
///   Einträge), `palace.search`/`palace.recall`
///   (`crate::palace_tools::PalaceToolProvider`, nur `established`),
///   `kanban.list`/`kanban.show` (`crate::kanban_tools::KanbanReadToolProvider`,
///   nur Kartendaten, kein Übergang). Damit
///   dürfen auch read-only Rollen ohne Netz sie tragen — keines schreibt.
/// - `delegate_wave` (und die übrigen Operationen der Composition-Root wie
///   `plan`/`goal`/`explore`) → `None`: keine Provider-Werkzeuge, sondern
///   `harw-ops`-/`harw-core-bridge`-Operationen mit eigenem
///   `PermissionTier`; ihre Zulassung regelt
///   [`crate::profile::composition_tools_for_role`] bzw. die
///   Operations-Registry, nicht dieser Rechtefilter. Jedes von
///   `delegate_wave` gestartete Kind bekommt seine Rechte über den
///   [`AuthorityReducer`] seiner eigenen Rolle.
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
    if tool == "fs.write"
        || tool == "fs.edit"
        // Runde 7, Teil T2: `latex.template` schreibt Vorlage und Gerüst in
        // den Workspace (kein Prozess) — vor dem `LATEX_TOOLS`-Zweig geprüft.
        || tool == crate::profile::LATEX_TEMPLATE_TOOL
        // `memory.record` schreibt Fakten (Projekt oder global); nie auto-freigegeben.
        || tool == MEMORY_RECORD
        || listed(AGENT_DEFINITION_WRITE_TOOLS)
        || listed(SKILL_PROPOSAL_PROPOSE_TOOLS)
        || listed(SKILL_PROPOSAL_DECIDE_TOOLS)
    {
        Some(Permission::WriteWorkspace)
    } else if listed(SHELL_TOOLS)
        // Plan R9, Teil F: `job.start` startet einen Prozess wie `shell.exec`.
        || tool == harw_tool_job::JOB_START_TOOL
        // R14: `work_driver.enqueue` startet einen dauerhaften Hintergrund-Job
        // über denselben Startweg wie `job.start`/`shell.exec` (führt die
        // `[work_driver] verify`-Kommandos aus und treibt Worker-Agenten an)
        // und trägt daher dasselbe Prozessrecht. Die Worker, die es startet,
        // bekommen `WriteWorkspace` zusätzlich, aber nur auf ihre eigenen
        // Scope-Pfade beschränkt — das regelt der Worker-Spawner, nicht
        // dieser Rechtefilter (der pro Werkzeug nur ein Recht liefert).
        || tool == WORK_DRIVER_ENQUEUE_TOOL
        || listed(PROCESS_TOOLS)
        || listed(LATEX_TOOLS)
        || listed(crate::profile::SUDO_TOOLS)
    {
        Some(Permission::ExecuteProcess)
    } else if listed(FS_READ_ONLY_TOOLS)
        || listed(DOC_TOOLS)
        || listed(EXPLORER_TOOLS)
        || listed(DEPS_WORKSPACE_TOOLS)
        || listed(LENS_TOOLS)
        || listed(AGENT_DEFINITION_READ_TOOLS)
        || listed(AGENT_DEFINITION_LIST_TOOLS)
        || listed(SKILL_PROPOSAL_READ_TOOLS)
        || listed(WorkbenchToolProvider::TOOL_NAMES)
        || listed(WorkbenchReadToolProvider::TOOL_NAMES)
        || listed(DiaryToolProvider::TOOL_NAMES)
        || listed(PalaceToolProvider::TOOL_NAMES)
        || listed(KanbanReadToolProvider::TOOL_NAMES)
        || tool == MEMORY_RECALL
        // Runde 5, Teil F: `plan.write` schreibt ausschließlich das
        // Harness-Artefakt `.harw/plans/<slug>.md` und muss unter der
        // Plan-Decke (ohne `WriteWorkspace`) laufen; die übrigen drei lesen
        // nur die Antwort der Nutzerin. Registriert nur für die Wurzel.
        || listed(harw_tool_plan::PlanToolProvider::TOOL_NAMES)
        // Plan R9, Teil F: `job.status/logs/stop/list/wait` starten nichts;
        // sie lesen bzw. beenden nur Jobs des Aufrufers und seiner
        // Nachfahren (Besitzprüfung in `harw-tool-job`).
        || listed(&harw_tool_job::JOB_CONTROL_TOOLS)
        // R14: `work_driver.status` liest nur den Job und seine
        // Zustands-Sidecar; `work_driver.stop` beendet nur einen eigenen Job
        // über den Job-Cancel-Pfad — beide starten nichts, wie
        // `job.status`/`job.stop`.
        || tool == WORK_DRIVER_STATUS_TOOL
        || tool == WORK_DRIVER_STOP_TOOL
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

/// Das Recht, das ein Capability-Label aus `[authority] capabilities`
/// benennt (#22 Welle 1B).
///
/// # Beschreibung
/// Capability-Labels sind offen (`security.advisory.correlate` benennt eine
/// fachliche Fähigkeit, kein Recht). Nur Labels aus dem folgenden
/// Rechte-Vokabular (das Label selbst oder mit weiterem `.`-Suffix, etwa
/// `filesystem.write.scoped` aus der DSL-Spezifikation §7) bilden auf ein
/// [`Permission`] ab:
///
/// | Label-Präfix | Recht |
/// |---|---|
/// | `filesystem.read`, `workspace.read` | `ReadWorkspace` |
/// | `filesystem.write`, `workspace.write` | `WriteWorkspace` |
/// | `process`, `shell` | `ExecuteProcess` |
/// | `network`, `web` | `NetworkAccess` |
/// | `secrets.read` | `ReadSecrets` |
/// | `plugins.manage` | `ManagePlugins` |
/// | `cargo.registry.read` | `ReadCargoRegistry` |
///
/// # Rückgabe
/// `None` für ein fachliches Label ohne Rechtebezug.
#[must_use]
pub fn capability_permission(label: &str) -> Option<Permission> {
    let matches = |prefix: &str| {
        label == prefix
            || label
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.'))
    };
    if matches("filesystem.read") || matches("workspace.read") {
        Some(Permission::ReadWorkspace)
    } else if matches("filesystem.write") || matches("workspace.write") {
        Some(Permission::WriteWorkspace)
    } else if matches("process") || matches("shell") {
        Some(Permission::ExecuteProcess)
    } else if matches("network") || matches("web") {
        Some(Permission::NetworkAccess)
    } else if matches("secrets.read") {
        Some(Permission::ReadSecrets)
    } else if matches("plugins.manage") {
        Some(Permission::ManagePlugins)
    } else if matches("cargo.registry.read") {
        Some(Permission::ReadCargoRegistry)
    } else {
        None
    }
}

/// Die Rechte, die `[authority] capabilities` einer IR zulassen.
///
/// # Rückgabe
/// `None`, wenn kein Label ein Recht benennt — dann trifft die Autorität
/// keine Aussage über Rechte (nur fachliche Labels oder gar keine), und der
/// Schnitt in [`granted_for_ir`] lässt die Profilrechte unverändert.
#[must_use]
pub fn authority_permissions(capabilities: &[String]) -> Option<PermissionSet> {
    let rights: Vec<Permission> = capabilities
        .iter()
        .filter_map(|label| capability_permission(label))
        .collect();
    (!rights.is_empty()).then(|| PermissionSet::from_policy(rights))
}

/// Die Rechte, mit denen die Kind-Registry eines Agenten montiert wird
/// (#22 Welle 1B): die Rechte seines Registry-Profils, geschnitten mit
///
/// 1. den Rechten seiner `[authority] capabilities` ([`authority_permissions`]),
///    sofern die Autorität überhaupt ein Recht benennt, und
/// 2. mit `narrow_to_manifest` zusätzlich den Rechten, die die Werkzeuge
///    seines Rechte-Manifests brauchen (für benutzerdefinierte Agenten,
///    deren Profil von einer Basisrolle geerbt ist).
///
/// Ein reiner Schnitt: das Ergebnis ist immer eine Teilmenge von
/// `profile_rights` — die Autorität einer Definition kann Rechte nur
/// entziehen, nie hinzufügen.
#[must_use]
pub fn granted_for_ir(
    profile_rights: &PermissionSet,
    ir: &harw_agent_dsl::AgentIr,
    narrow_to_manifest: bool,
) -> PermissionSet {
    granted_for_capabilities(
        profile_rights,
        &ir.authority.capabilities,
        narrow_to_manifest.then_some(ir.permissions.tools.as_slice()),
    )
}

/// Wie [`granted_for_ir`], mit den Capability-Labels und (optional) den
/// Manifest-Werkzeugen direkt — für Aufrufer, die nur die Laufzeitsicht
/// (`ExecutableAgentIr::authority`) einer eingebauten Rolle halten.
#[must_use]
pub fn granted_for_capabilities(
    profile_rights: &PermissionSet,
    capabilities: &[String],
    manifest_tools: Option<&[String]>,
) -> PermissionSet {
    let mut granted: Vec<Permission> = profile_rights.iter().collect();
    if let Some(authority) = authority_permissions(capabilities) {
        granted.retain(|permission| authority.contains(*permission));
    }
    if let Some(tools) = manifest_tools {
        let tools: Vec<&str> = tools.iter().map(String::as_str).collect();
        let manifest = permissions_of(&tools);
        granted.retain(|permission| manifest.contains(*permission));
    }
    PermissionSet::from_policy(granted)
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
    use crate::profile::{EXPLORER_WEB_TOOLS, RegistryProfile};

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
        assert_eq!(tool_permission("fs.edit"), Some(Permission::WriteWorkspace));
        assert_eq!(
            tool_permission("shell.exec"),
            Some(Permission::ExecuteProcess)
        );
        assert_eq!(
            tool_permission("job.start"),
            Some(Permission::ExecuteProcess)
        );
        for tool in harw_tool_job::JOB_CONTROL_TOOLS {
            assert_eq!(
                tool_permission(tool),
                Some(Permission::ReadWorkspace),
                "{tool}"
            );
        }
        assert_eq!(tool_permission("plan"), None);
        assert_eq!(tool_permission("delegate_wave"), None);
        // Runde 5, Teil H: `agent.result` liest nur eigene Kind-Ergebnisse —
        // die Grenze zieht der Spawner (Eltern-Kind-Bindung), keine
        // Sandbox-Rechteklasse.
        assert_eq!(tool_permission("agent.result"), None);
        // Runde 5, Teil K: `agent.status`/`agent.cancel` wirken nur auf
        // eigene Hintergrund-Agenten; die Grenze zieht der Spawner.
        assert_eq!(tool_permission("agent.status"), None);
        assert_eq!(tool_permission("agent.cancel"), None);
    }

    /// R14: `work_driver.enqueue` startet einen dauerhaften Hintergrund-Job
    /// über denselben Startweg wie `job.start` und trägt daher dasselbe
    /// Recht (`ExecuteProcess`); `work_driver.status`/`work_driver.stop`
    /// starten nichts — sie lesen bzw. beenden nur einen eigenen Job, wie
    /// `job.status`/`job.stop`, und tragen daher `ReadWorkspace`.
    #[test]
    fn test_tool_permission_maps_work_driver_tools_like_job_tools() {
        assert_eq!(
            tool_permission(WORK_DRIVER_ENQUEUE_TOOL),
            tool_permission(harw_tool_job::JOB_START_TOOL),
            "work_driver.enqueue muss wie job.start abgebildet werden"
        );
        assert_eq!(
            tool_permission(WORK_DRIVER_ENQUEUE_TOOL),
            Some(Permission::ExecuteProcess)
        );
        for tool in [WORK_DRIVER_STATUS_TOOL, WORK_DRIVER_STOP_TOOL] {
            assert_eq!(
                tool_permission(tool),
                Some(Permission::ReadWorkspace),
                "{tool}"
            );
        }
    }

    #[test]
    fn test_tool_permission_matches_workbench_provider_declarations() {
        let names = WorkbenchToolProvider::TOOL_NAMES;
        let declared = WorkbenchToolProvider::TOOL_PERMISSIONS;
        assert_eq!(names.len(), declared.len());
        for (name, permission) in names.iter().zip(declared.iter()) {
            assert_eq!(tool_permission(name), *permission, "{name}");
        }
    }

    /// Runde 5, Teil F: die Plan-Werkzeuge tragen `ReadWorkspace` — weder
    /// `WriteWorkspace` noch `ExecuteProcess`, sonst liefen sie unter der
    /// Plan-Decke nicht bzw. wären mehr als „nur `.harw/plans/`“.
    #[test]
    fn test_tool_permission_matches_plan_tool_provider_declarations() {
        let names = harw_tool_plan::PlanToolProvider::TOOL_NAMES;
        let declared = harw_tool_plan::PlanToolProvider::TOOL_PERMISSIONS;
        assert_eq!(names.len(), declared.len());
        for (name, permission) in names.iter().zip(declared.iter()) {
            assert_eq!(tool_permission(name), *permission, "{name}");
            assert_eq!(
                tool_permission(name),
                Some(Permission::ReadWorkspace),
                "{name}"
            );
        }
    }

    #[test]
    fn test_tool_permission_matches_knowledge_read_provider_declarations() {
        let providers: [(&[&str], &[Option<Permission>]); 4] = [
            (
                WorkbenchReadToolProvider::TOOL_NAMES,
                WorkbenchReadToolProvider::TOOL_PERMISSIONS,
            ),
            (
                DiaryToolProvider::TOOL_NAMES,
                DiaryToolProvider::TOOL_PERMISSIONS,
            ),
            (
                PalaceToolProvider::TOOL_NAMES,
                PalaceToolProvider::TOOL_PERMISSIONS,
            ),
            (
                KanbanReadToolProvider::TOOL_NAMES,
                KanbanReadToolProvider::TOOL_PERMISSIONS,
            ),
        ];
        for (names, declared) in providers {
            assert_eq!(names.len(), declared.len());
            for (name, permission) in names.iter().zip(declared.iter()) {
                assert_eq!(tool_permission(name), *permission, "{name}");
                assert_eq!(*permission, Some(Permission::ReadWorkspace), "{name}");
            }
        }
    }

    #[test]
    fn test_tool_permission_covers_every_skill_proposal_tool() {
        for tool in SKILL_PROPOSAL_READ_TOOLS {
            assert_eq!(
                tool_permission(tool),
                Some(Permission::ReadWorkspace),
                "{tool}"
            );
        }
        for tool in SKILL_PROPOSAL_PROPOSE_TOOLS
            .iter()
            .chain(SKILL_PROPOSAL_DECIDE_TOOLS.iter())
        {
            assert_eq!(
                tool_permission(tool),
                Some(Permission::WriteWorkspace),
                "{tool}"
            );
        }
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
    fn test_reduce_to_read_explore_carries_network_only_if_parent_has_it() {
        let without =
            PermissionSet::from_policy([Permission::ReadWorkspace, Permission::ReadCargoRegistry]);
        assert!(!reduce_to_read_explore(&without).contains(Permission::NetworkAccess));
        let reduced = reduce_to_read_explore(&every_permission());
        assert_eq!(
            reduced,
            PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::ReadCargoRegistry,
                Permission::NetworkAccess,
            ])
        );
    }

    #[test]
    fn test_reduce_to_read_workspace_network_has_no_registry() {
        let reduced = reduce_to_read_workspace_network(&every_permission());
        assert_eq!(
            reduced,
            PermissionSet::from_policy([Permission::ReadWorkspace, Permission::NetworkAccess])
        );
        assert!(!reduced.contains(Permission::ReadCargoRegistry));
    }

    #[test]
    fn test_only_web_admitting_roles_get_network_access() {
        // Sicherheitsmatrix Netz: `NetworkAccess` bekommen über den Reducer
        // genau die Rollen, deren TOML `web.*` admittiert und die dafür
        // freigegeben sind (seit der Nutzerentscheidung auch `uia-worker`
        // und `uia-writer`). `analyst`/`researcher-deps` teilen das Profil
        // des Explorers, verbieten `web.*` aber und bleiben ohne Netz;
        // `planner`, `root-orchestrator`, die Triage-Rollen, `executor`,
        // `memory-steward`, `agent-steward` und `uia-shell-worker` ebenso.
        let networked = [
            role_names::EXPLORER,
            role_names::UIA_EXPLORER,
            role_names::UIA_WORKER,
            role_names::UIA_WRITER,
            role_names::RESEARCHER_WEB,
            role_names::DEPENDENCY_RESEARCHER,
            role_names::RESEARCHER,
            // Dokumentierte Durchreichung (Plan Punkt 1): kein eigenes
            // `web.*`, siehe
            // `test_research_orchestrator_passes_network_through_without_web_tools`.
            role_names::RESEARCH_ORCHESTRATOR,
        ];
        for role in role_names::ALL {
            let carries_network = authority_reducer_for_role(role)
                .is_some_and(|reducer| reducer.ceiling().contains(Permission::NetworkAccess));
            assert_eq!(
                carries_network,
                networked.contains(role),
                "{role}: Netzrecht über den Reducer entgegen der Sicherheitsmatrix"
            );
        }
        for role in [
            role_names::ANALYST,
            role_names::RESEARCHER_DEPS,
            role_names::PLANNER,
        ] {
            assert_eq!(
                authority_reducer_for_role(role),
                Some(AuthorityReducer::ReadRegistry),
                "{role}"
            );
        }
        for role in [
            role_names::SECURITY_EGRESS_TRIAGE,
            role_names::SECURITY_BASELINE_TRIAGE,
            role_names::SECURITY_STRUCTURE_TRIAGE,
            role_names::SECURITY_ENDPOINT_TRIAGE,
        ]
        .into_iter()
        .chain(role_names::MATRIX_ROLES)
        {
            assert_eq!(
                authority_reducer_for_role(role),
                Some(AuthorityReducer::ReadOnly),
                "{role}"
            );
        }
        // Netz zusammen mit Workspace-Lesen tragen nur die zwei
        // Explorer-Reducer; `ReadNetwork` schließt den Workspace weiter aus.
        for reducer in AuthorityReducer::ALL {
            let ceiling = reducer.ceiling();
            let both = ceiling.contains(Permission::NetworkAccess)
                && ceiling.contains(Permission::ReadWorkspace);
            assert_eq!(
                both,
                matches!(
                    reducer,
                    AuthorityReducer::ReadExplore | AuthorityReducer::ReadWorkspaceNetwork
                ),
                "{reducer:?}"
            );
        }
    }

    #[test]
    fn test_uia_helpers_exceed_their_reducer_only_by_their_one_gated_permission()
    -> crate::test_support::TestResult {
        // `uia-worker`/`uia-writer` bleiben Ausnahmen von der
        // Untermengen-Invariante — aber nur um genau ein Recht, das kein
        // Reducer je weiterträgt (`ExecuteProcess` bzw. `WriteWorkspace`).
        // Netz (`web.fetch`/`web.search`/`browser.open`) und
        // Registry-Quellcache (`deps.source_*`) deckt `ReadExplore` ab.
        for (role, gated) in [
            (role_names::UIA_WORKER, Permission::ExecuteProcess),
            (role_names::UIA_WRITER, Permission::WriteWorkspace),
        ] {
            let reducer = authority_reducer_for_role(role).ok_or(
                crate::test_support::TestError::Unexpected(format!("{role} ohne Reducer")),
            )?;
            assert_eq!(reducer, AuthorityReducer::ReadExplore, "{role}");
            let profile = crate::profile::profile_for_role(role).ok_or(
                crate::test_support::TestError::Unexpected(format!("{role} ohne Profil")),
            )?;
            let required = profile.required_permissions();
            assert!(required.contains(gated), "{role}: {gated:?}");
            let rest = PermissionSet::from_policy(
                required.iter().filter(|permission| *permission != gated),
            );
            assert!(
                rest.is_subset_of(&reducer.ceiling()),
                "{role}: {profile:?} braucht über {gated:?} hinaus mehr als {reducer:?}"
            );
            assert!(
                reducer.ceiling().contains(Permission::NetworkAccess),
                "{role}"
            );
        }
        Ok(())
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
        assert_eq!(AuthorityReducer::ReadExplore.id(), REDUCE_TO_READ_EXPLORE);
        assert_eq!(
            AuthorityReducer::ReadWorkspaceNetwork.id(),
            REDUCE_TO_READ_WORKSPACE_NETWORK
        );
        let mut ids: Vec<&str> = AuthorityReducer::ALL.iter().map(|r| r.id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), AuthorityReducer::ALL.len());
    }

    #[test]
    fn test_authority_reducer_for_role_covers_every_role_and_bounds_its_profile()
    -> crate::test_support::TestResult {
        // `memory-steward` (WriteWorkspace), `executor` (ExecuteProcess),
        // `uia-worker` (ExecuteProcess, Addendum I; Netz trägt seit der
        // Nutzerentscheidung `ReadExplore`), `agent-steward`
        // (WriteWorkspace, Addendum K), `uia-writer` (WriteWorkspace; Netz
        // ebenfalls über `ReadExplore`) und `uia-shell-worker`
        // (ExecuteProcess) und `uia-latex-writer` (WriteWorkspace und
        // ExecuteProcess, ohne Netz) sind dokumentierte Ausnahmen von der
        // Untermengen-Invariante: kein Reducer trägt je Schreib- oder
        // Ausführungsrecht weiter (siehe
        // `test_reduce_never_exceeds_parent_or_ceiling`); alle sieben Rollen
        // bekommen diese Rechte über ihre feste Profilzuweisung, nicht über
        // diesen Reducer (siehe Doku bei `authority_reducer_for_role`).
        // `uia-explorer` ist keine Ausnahme mehr: `ReadWorkspaceNetwork`
        // trägt sein ganzes Profil.
        let exempt_from_subset_bound = [
            role_names::MEMORY_STEWARD,
            role_names::EXECUTOR,
            role_names::UIA_WORKER,
            role_names::AGENT_STEWARD,
            role_names::UIA_WRITER,
            role_names::UIA_SHELL_WORKER,
            role_names::UIA_LATEX_WRITER,
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
            // Explorer-Netz: `ReadOnlyExplore` registriert
            // `web.fetch`/`web.search` (`EXPLORER_WEB_TOOLS`, `NetworkAccess`)
            // auch für `analyst`/`researcher-deps`, deren TOML sie verbietet
            // und deren `ReadRegistry` kein Netz trägt. Nur für diese
            // Kombination (Profil ohne Netz-Obergrenze) sind genau diese zwei
            // Werkzeuge ausgenommen; `explorer` (`ReadExplore`) muss sein
            // Profil vollständig unter die Obergrenze bringen.
            let ceiling = reducer.ceiling();
            let web_exempt = profile == RegistryProfile::ReadOnlyExplore
                && !ceiling.contains(Permission::NetworkAccess);
            let bounded = PermissionSet::from_policy(
                profile
                    .registered_tool_names()
                    .into_iter()
                    .filter(|tool| !(web_exempt && EXPLORER_WEB_TOOLS.contains(tool)))
                    .filter_map(tool_permission),
            );
            assert!(
                bounded.is_subset_of(&ceiling),
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
            Some(AuthorityReducer::ReadExplore)
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
            Some(AuthorityReducer::ReadWorkspaceNetwork)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::EXPLORER),
            Some(AuthorityReducer::ReadExplore)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::UIA_WRITER),
            Some(AuthorityReducer::ReadExplore)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::UIA_SHELL_WORKER),
            Some(AuthorityReducer::ReadOnly)
        );
        assert_eq!(
            authority_reducer_for_role(role_names::UIA_LATEX_WRITER),
            Some(AuthorityReducer::ReadOnly)
        );
        // `uia-latex-writer` überschreitet `ReadOnly` genau um
        // `WriteWorkspace` (`fs.write`) und `ExecuteProcess`
        // (`latex.build`) — nie Netz, nie Registry-Quellcache.
        let latex = crate::profile::RegistryProfile::UiaLatexWriter.required_permissions();
        assert_eq!(
            latex,
            PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
            ])
        );
        assert_eq!(
            tool_permission("latex.build"),
            Some(Permission::ExecuteProcess)
        );
        // Runde 7, Teil T2/T4: `latex.template` schreibt nur (WriteWorkspace),
        // `latex.check` startet `kpsewhich`/`fc-list` (ExecuteProcess).
        assert_eq!(
            tool_permission("latex.template"),
            Some(Permission::WriteWorkspace)
        );
        assert_eq!(
            tool_permission("latex.check"),
            Some(Permission::ExecuteProcess)
        );
        for role in [
            role_names::ROOT_ORCHESTRATOR,
            role_names::CODING_ORCHESTRATOR,
            role_names::ANALYSIS_ORCHESTRATOR,
        ] {
            assert_eq!(
                authority_reducer_for_role(role),
                Some(AuthorityReducer::ReadRegistry),
                "{role}"
            );
        }
        assert_eq!(
            authority_reducer_for_role(role_names::RESEARCH_ORCHESTRATOR),
            Some(AuthorityReducer::ReadExplore)
        );
        Ok(())
    }

    /// Die Netz-Durchreichung des `research-orchestrator` ist KEIN eigenes
    /// Netz-Werkzeug: sein Profil registriert unter vollem Netz kein `web.*`,
    /// und der Reducer gibt nie Schreib- oder Ausführungsrecht weiter.
    #[test]
    fn test_research_orchestrator_passes_network_through_without_web_tools()
    -> crate::test_support::TestResult {
        let role = role_names::RESEARCH_ORCHESTRATOR;
        let reducer = authority_reducer_for_role(role).ok_or(
            crate::test_support::TestError::Missing("research-orchestrator braucht einen Reducer"),
        )?;
        let profile = crate::profile::profile_for_role(role).ok_or(
            crate::test_support::TestError::Missing("research-orchestrator braucht ein Profil"),
        )?;
        let child = reducer.reduce(&every_permission());
        assert!(child.contains(Permission::NetworkAccess));
        assert!(!child.contains(Permission::WriteWorkspace));
        assert!(!child.contains(Permission::ExecuteProcess));
        assert!(
            !profile
                .tool_names_for(&child)
                .iter()
                .any(|tool| tool.starts_with("web.") || tool.starts_with("browser.")),
            "{role}: {profile:?} darf trotz Netzrecht kein Netz-Werkzeug registrieren"
        );
        Ok(())
    }

    /// `[delegation].targets` der Orchestratoren: jede genannte Rolle ist eine
    /// eingebaute Rolle; Child-Orchestratoren nennen ausschließlich Worker
    /// (keinen Orchestrator, keinen `agent-steward`, keine UIA-Rolle), und
    /// Worker tragen gar keine Liste.
    #[test]
    fn test_delegation_targets_are_builtin_and_child_orchestrators_only_name_workers()
    -> crate::test_support::TestResult {
        let forbidden_for_children: Vec<&str> = std::iter::once(role_names::ROOT_ORCHESTRATOR)
            .chain(role_names::CHILD_ORCHESTRATORS.iter().copied())
            .chain([
                role_names::AGENT_STEWARD,
                role_names::UIA_WORKER,
                role_names::UIA_EXPLORER,
                role_names::UIA_WRITER,
                role_names::UIA_SHELL_WORKER,
                role_names::UIA_LATEX_WRITER,
            ])
            .collect();
        for role in role_names::CHILD_ORCHESTRATORS {
            let targets = delegation_targets_for_role(role).ok_or(
                crate::test_support::TestError::Unexpected(format!(
                    "{role} braucht [delegation].targets"
                )),
            )?;
            assert!(!targets.is_empty(), "{role}: leere Zielliste");
            for target in &targets {
                assert!(
                    role_names::ALL.contains(&target.as_str()),
                    "{role}: unbekanntes Ziel {target}"
                );
                assert!(
                    !forbidden_for_children.contains(&target.as_str()),
                    "{role}: {target} ist kein Worker-Ziel eines Child-Orchestrators"
                );
            }
        }
        let root = delegation_targets_for_role(role_names::ROOT_ORCHESTRATOR).ok_or(
            crate::test_support::TestError::Missing("root-orchestrator braucht [delegation]"),
        )?;
        for child in role_names::CHILD_ORCHESTRATORS {
            assert!(
                root.iter().any(|target| target == child),
                "root-orchestrator muss {child} als Ziel führen"
            );
        }
        for target in &root {
            assert!(role_names::ALL.contains(&target.as_str()), "{target}");
        }
        for worker in [
            role_names::EXPLORER,
            role_names::PLANNER,
            role_names::ANALYST,
            role_names::EXECUTOR,
        ] {
            assert!(
                delegation_targets_for_role(worker).is_none(),
                "{worker}: Worker tragen keine Delegationsliste"
            );
        }
        assert!(delegation_targets_for_role("unbekannt").is_none());
        Ok(())
    }

    #[test]
    fn test_delegation_targets_of_reads_only_string_arrays() -> crate::test_support::TestResult {
        let tables: toml::Table =
            toml::from_str("[delegation]\ntargets = [\"explorer\", 3, \"planner\"]\n")
                .map_err(crate::test_support::ctx("Test-TOML muss parsen"))?;
        assert_eq!(
            delegation_targets_of(&tables),
            Some(vec!["explorer".to_owned(), "planner".to_owned()])
        );
        let without: toml::Table = toml::from_str("[delegation]\ntargets = \"explorer\"\n")
            .map_err(crate::test_support::ctx("Test-TOML muss parsen"))?;
        assert_eq!(delegation_targets_of(&without), None);
        assert_eq!(delegation_targets_of(&toml::Table::new()), None);
        Ok(())
    }

    #[test]
    fn test_capability_permission_maps_only_the_rights_vocabulary() {
        assert_eq!(
            capability_permission("filesystem.read"),
            Some(Permission::ReadWorkspace)
        );
        assert_eq!(
            capability_permission("filesystem.write.scoped"),
            Some(Permission::WriteWorkspace)
        );
        assert_eq!(
            capability_permission("process.spawn.sandboxed"),
            Some(Permission::ExecuteProcess)
        );
        assert_eq!(
            capability_permission("network"),
            Some(Permission::NetworkAccess)
        );
        // Fachliche Labels und Beinahe-Treffer benennen kein Recht.
        assert_eq!(capability_permission("security.advisory.correlate"), None);
        assert_eq!(capability_permission("filesystem.reader"), None);
        assert_eq!(capability_permission("networking"), None);
        assert!(authority_permissions(&["security.context.read".to_owned()]).is_none());
        assert!(authority_permissions(&[]).is_none());
    }

    /// #22 Welle 1B: `[authority] capabilities` schneidet die Profilrechte —
    /// für jede Kombination aus Profil und Autorität ist das Ergebnis eine
    /// Teilmenge des Profils, nie mehr.
    #[test]
    fn test_granted_for_ir_never_widens_the_profile_rights() -> crate::test_support::TestResult {
        let irs = crate::embedded_agents::builtin_agent_irs(time::OffsetDateTime::UNIX_EPOCH)
            .map_err(crate::test_support::ctx("eingebaute Rollen senken"))?;
        let explorer = irs
            .get(role_names::EXPLORER)
            .ok_or(crate::test_support::TestError::Missing("explorer"))?;
        let labels = [
            "filesystem.read",
            "filesystem.write",
            "process.spawn",
            "network",
            "secrets.read",
            "security.verdict.propose",
        ];
        let profiles = [
            RegistryProfile::ReadOnlyExplore,
            RegistryProfile::ShellExecution,
            RegistryProfile::Research,
            RegistryProfile::NoTools,
            RegistryProfile::WorkspaceEdit,
        ];
        for profile in profiles {
            let rights = profile.required_permissions();
            // Jede Teilmenge der Labels als Autorität.
            for mask in 0_u32..(1 << labels.len()) {
                let mut ir = explorer.clone();
                ir.authority.capabilities = labels
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| mask & (1 << index) != 0)
                    .map(|(_, label)| (*label).to_owned())
                    .collect();
                for narrow in [false, true] {
                    let granted = granted_for_ir(&rights, &ir, narrow);
                    assert!(granted.is_subset_of(&rights), "{profile:?} {mask} {narrow}");
                    if let Some(authority) = authority_permissions(&ir.authority.capabilities) {
                        assert!(granted.is_subset_of(&authority), "{profile:?} {mask}");
                    }
                }
            }
        }
        // Ohne Rechte-Label bleibt das Profil unverändert.
        let mut domain_only = explorer.clone();
        domain_only.authority.capabilities = vec!["security.context.read".to_owned()];
        let rights = RegistryProfile::ReadOnlyExplore.required_permissions();
        assert_eq!(granted_for_ir(&rights, &domain_only, false), rights);
        // Eine Autorität nur mit Lesen entzieht einem Schreibprofil das Schreiben.
        let mut read_only = explorer.clone();
        read_only.authority.capabilities = vec!["filesystem.read".to_owned()];
        let edit = RegistryProfile::WorkspaceEdit.required_permissions();
        let granted = granted_for_ir(&edit, &read_only, false);
        assert!(granted.contains(Permission::ReadWorkspace));
        assert!(!granted.contains(Permission::WriteWorkspace));
        Ok(())
    }
}
