//! Registry-Profile — welche Werkzeuge und welche Identität eine Session bzw.
//! ein Kind-Agent bekommt.
//!
//! Spezifikationsquelle: AP W3-01..05.
//!
//! # Verantwortlichkeit
//! Dieses Modul besitzt die Zuordnung *Profil → Tool-Provider → beworbene
//! Werkzeugliste → Rollenbeschreibung*. Es ist die einzige Stelle, an der
//! entschieden wird, welche Werkzeuge ein Kind-Agent überhaupt sehen kann.
//! Die Freigabegrenze (`DefaultApprovalPolicy`) und der Zusammenbau der
//! `AssembledRegistry`-Struktur bleiben im Crate-Wurzelmodul.
//!
//! # Schlüsseltypen
//! - [`RegistryProfile`] — der geschlossene Satz eingebauter Profile.
//! - [`IdentityOverrides`] — Überschreibungen für den System-Prompt.
//! - [`RestrictedToolProvider`] — Sichtbarkeitsfilter über einem Provider.
//! - [`assemble_registry`] — erkennt das Projekt einmal und baut eine Registry.
//! - [`assemble_registry_for_project`] — baut eine Registry über einem bereits
//!   erkannten Projektkontext, ohne erneute Projekterkennung.
//! - [`assemble_registry_for_sandbox`] — wie oben, registriert aber nur die
//!   Werkzeuge, deren Recht der gewährte [`PermissionSet`] trägt (W5 RD).
//! - [`role_names`] — die Namen der eingebauten Rollen als Single Source of Truth.
//!
//! # Warum ein Filter statt eines zweiten Providers
//! `harw_tool_fs::FsToolProvider` liefert lesende *und* schreibende Werkzeuge
//! aus einem Provider. Ein read-only Profil registriert ihn deshalb hinter
//! [`RestrictedToolProvider`]: `fs.write` verschwindet dort aus `tools()` **und**
//! aus `executor()`. Ein Kind kann das Werkzeug damit weder sehen noch durch
//! Raten seines Namens aufrufen — die Beschränkung ist keine Prompt-Bitte.
//!
//! # Browser nur mit Grant (W5 RD, F-073) — Ausnahme `UiaQuickHelper`
//! Kein Profil registriert `browser.*` — mit **einer** dokumentierten
//! Ausnahme: [`RegistryProfile::UiaQuickHelper`] registriert und bewirbt
//! statisch `browser.open` (Nutzerentscheidung, siehe die Begründung bei
//! [`RegistryProfile::UiaQuickHelper`] und [`UIA_QUICK_HELPER_BROWSER_TOOLS`]).
//! Alle übrigen sechs `browser.*`-Werkzeuge bleiben für jedes Profil verboten.
//! Der tatsächliche Laufzeit-Provider entsteht weiterhin ausschließlich über
//! `browser_tool_provider` (Feature `browser`) mit einem ausdrücklichen
//! `harw_tool_browser::BrowserOpenGrant`; vorher hängte `Full` sie unter dem
//! Feature still und ohne Grant an.
//!
//! # Fehler
//! [`assemble_registry`] gibt [`crate::RegistryDefaultsError`] zurück:
//! `ProjectDiscovery`, wenn `cwd` kein auflösbares Projekt ist.
//! `browser_tool_provider` (Feature `browser`) liefert `BrowserHost`, wenn die
//! Host-Konfiguration ungültig ist.
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync`. [`assemble_registry`] ist synchron und
//! zustandslos.

use std::path::PathBuf;
use std::sync::Arc;

use harw_authority::PermissionSet;
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_extension_api::{
    ExtensionRegistryBuilder, ToolExecutor, ToolName, ToolProvider, ToolSpec,
};
use harw_instructions::{AgentIdentity, BaselineInstructionsProvider};
use harw_project_discovery::{
    DiscoveryConfig, ProjectContext, ProjectContextProvider, discover_project,
};
use harw_sandbox::{HostPermitSessionRegistry, ProcessPermitLedger, SandboxProfile};
use harw_tool_deps::DepsToolProvider;
use harw_tool_fs::FsToolProvider;
use harw_tool_lens::LensToolProvider;
use harw_tool_shell::{HostPermitPromptSender, HostPermitVariant, ShellToolProvider};
use harw_tool_web::WebToolProvider;

#[cfg(feature = "browser")]
use harw_browser::policy::{BrowserLimits, OriginPolicy};
#[cfg(feature = "browser")]
use harw_browser_thirtyfour::{config::FirefoxHostConfig, host::FirefoxHost};
#[cfg(feature = "browser")]
use harw_tool_browser::{
    BrowserOpenGrant, BrowserOpenPolicy, BrowserToolSet, HarwnessBrowserToolProvider,
};

use crate::agent_definition_tools::{DefinitionAuthorCeiling, DefinitionWriteMode};
use crate::authority::{permissions_of, tool_permission};
use crate::error::{RegistryDefaultsError, RegistryDefaultsResult};
use crate::{AssembledRegistry, DefaultApprovalPolicy};

/// Die Namen der eingebauten Rollen — Single Source of Truth.
///
/// # Beschreibung
/// Jede Stelle, die eine eingebaute Rolle benennt (Spawn-Operation, CLI-Flag,
/// eingebettete Agentendefinition, Test), verwendet diese Konstanten statt
/// eines eigenen String-Literals. Ein Tippfehler wird damit zum Compilefehler
/// statt zu einer stillschweigend unbekannten Rolle.
///
/// # Warum diese Liste noch existiert (Befund AW6-03-Nachfolge)
/// AW6-00 hat `harw_registry_defaults::embedded_agents` von einer festen
/// `include_str!`-Aufzählung auf eine Verzeichniskonvention umgestellt: eine
/// neue Rollendatei unter `agents/` wird seither ohne Änderung an einer
/// geteilten Datei *gefunden* (`builtin_agent_toml`). Diese Liste hier ist
/// eine andere Zusicherung: sie entscheidet, welche gefundenen Definitionen
/// [`crate::embedded_agents::builtin_agent_definitions`] tatsächlich zu
/// startbaren, gesenkten Rollen macht (`targets.contains(name)`-Filter dort)
/// — und genau das ist die Lücke, die vier bereits existierende, bereits
/// gefundene `security-*-triage`-Rollendateien am Verbotstest
/// (`test_every_builtin_role_forbids_write_and_shell_tools`) vorbeigeführt
/// hat, bis diese Änderung sie hier einträgt.
///
/// Eine rein verzeichnisbasierte Ableitung dieser Liste (dieselbe Bewegung
/// wie AW6-00) ist absichtlich **nicht** gewählt: `ALL` ist ein `pub const
/// &'static [&'static str]` und wird an jeder bekannten Aufrufstelle
/// außerhalb dieser Crate (`harw-ops`, `harw-cli`, `harw-tui`, die Tests in
/// `tests/tool_admission_coverage.rs`) direkt als Wert benutzt — `for role in
/// role_names::ALL`, `role_names::ALL.contains(...)`, `role_names::ALL.len()`
/// — nie über einen Funktionsaufruf. Eine `for`-Schleife über einen Platz
/// verlangt, dass dessen *Typ* `IntoIterator` implementiert; das trifft auf
/// die Referenz `&'static [&'static str]` zu (sie ist `Copy`, das Kopieren
/// aus dem `static`/`const` ist deshalb erlaubt), aber auf keinen Wrapper wie
/// `OnceLock<Vec<&str>>` oder `LazyLock<Vec<&str>>`, den eine zur Laufzeit aus
/// dem eingebetteten Verzeichnisbaum abgeleitete Liste bräuchte (`Vec<T>` ist
/// nicht `Copy`; ein `static` dieses Typs ließe sich an der Aufrufstelle nicht
/// mehr per Wertausdruck verschieben, „cannot move out of static item“).
/// Verzeichnisableitung würde also entweder den Typ dieser Konstante ändern
/// (bricht jede der genannten Aufrufstellen, die außerhalb des
/// Schreibbereichs dieser Änderung liegen) oder einen neuen Funktionsaufruf
/// `role_names::all()` erzwingen (dieselbe Bruchstelle). Das ist der Fall aus
/// der Auftragsbeschreibung: `ALL` steckt zwar in keinem Array fester Länge,
/// aber es wird an entfernten Aufrufstellen als unveränderlicher Wert
/// referenziert, nicht als Funktion aufgerufen — Weg 1 ist deshalb ohne
/// Änderungen außerhalb dieser Datei nicht erreichbar.
///
/// Stattdessen bleibt `ALL` eine handgepflegte Liste, aber der Test
/// `test_role_names_all_matches_discovered_role_files_minus_pending_exclusions`
/// (unten, `#[cfg(test)]`) prüft sie gegen eine **andere** Quelle — den
/// verzeichnisbasierten Fund von
/// [`crate::embedded_agents::builtin_agent_toml`] — statt gegen eine daneben
/// stehende Zahl. `context-steward`, `intel-scout`, `cargo-worker`,
/// `host-process-worker`, `sandbox-shell-worker` und `tmux-inspector-worker`
/// (`agents/roles/**`)
/// werden von derselben Verzeichnis-Sammlung ebenfalls gefunden, sind aber
/// bewusst nicht in `ALL`: ihre Aufnahme ist ein eigener, noch offener Befund
/// (siehe Abschlussbericht dieses Knotens) und außerhalb des Auftrags, der
/// ausschließlich die vier `security-*-triage`-Rollen betrifft. Der Test
/// führt sie deshalb explizit als bekannte, begründete Ausnahme, nicht als
/// stillschweigende Lücke.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::role_names;
///
/// assert!(role_names::ALL.contains(&role_names::EXPLORER));
/// assert!(role_names::ALL.contains(&role_names::SECURITY_BASELINE_TRIAGE));
/// // Keine feste Länge hier: `ALL` wächst mit jeder neuen eingebauten Rolle,
/// // und eine Zahl daneben wäre nur eine Wiederholung der Liste, die sie
/// // zählt — sie bräche beim nächsten Rollenzuwachs, ohne etwas Inhaltliches
/// // zu prüfen. Die vollständige Prüfung gegen eine unabhängige Quelle
/// // (Verzeichnis-Fund statt Zahl) übernimmt
/// // `test_role_names_all_matches_discovered_role_files_minus_pending_exclusions`
/// // im `#[cfg(test)]`-Modul dieser Datei.
/// ```
pub mod role_names {
    /// Startbarer Orchestrator, den die UIA als Wurzel eines Agentenbaums
    /// einsetzen kann.
    pub const ROOT_ORCHESTRATOR: &str = "root-orchestrator";
    /// Read-only Erkundung von Workspace und Dependency-Quellen.
    pub const EXPLORER: &str = "explorer";
    /// Dependency-Recherche aus `Cargo.lock` und dem lokalen Registry-Quellcache.
    pub const RESEARCHER_DEPS: &str = "researcher-deps";
    /// Web-Recherche über die Host-Allowlist der Sandbox.
    pub const RESEARCHER_WEB: &str = "researcher-web";
    /// Erzeugt Planvorschläge, ohne den autoritativen Plan zu mutieren.
    pub const PLANNER: &str = "planner";
    /// Orchestrator-naher, read-only arbeitender Verdichter.
    pub const ANALYST: &str = "analyst";

    /// Triagiert Netz-Befunde (`EgressFlow`/`ListenerOpened`) der
    /// `EgressFlowRule`; bekommt bereits ausgewertete `Finding`-Batches als
    /// Parameter, ruft selbst kein Werkzeug auf (`[tools].admitted = []` in
    /// `agents/roles/security-egress-triage/security-egress-triage.toml`).
    pub const SECURITY_EGRESS_TRIAGE: &str = "security-egress-triage";
    /// Triagiert Baseline-Abweichungs-Befunde (`HostSample` gegen
    /// `Baseline`) der `BaselineDeviationRule`; ebenfalls parametergetrieben,
    /// keine eigene Werkzeugoberfläche.
    pub const SECURITY_BASELINE_TRIAGE: &str = "security-baseline-triage";
    /// Triagiert Struktur-Abweichungs-Befunde (`StructureDrift`) der
    /// `StructureDriftRule`; parametergetrieben, keine eigene
    /// Werkzeugoberfläche.
    pub const SECURITY_STRUCTURE_TRIAGE: &str = "security-structure-triage";
    /// Bündelt die drei Ereignisarten ohne eigene verdichtende Regel
    /// (`ProcessExec`, `FileWrite`, `AuthEvent`); parametergetrieben, keine
    /// eigene Werkzeugoberfläche.
    pub const SECURITY_ENDPOINT_TRIAGE: &str = "security-endpoint-triage";

    /// Führt Befehls- und Dateioperationen im Auftrag des Haupt-Agenten aus
    /// und liefert eine Zusammenfassung statt Rohausgaben (Slice B7). Die
    /// TUI delegiert damit Befehlsfolgen an einen eigenen Worker, statt sie
    /// als viele einzelne Tool-Aufrufe im Hauptfenster zu zeigen. Einzige
    /// eingebaute Rolle mit [`RegistryProfile::Full`] — siehe
    /// [`profile_for_role`] und die Begründung in `agents/executor.toml`.
    pub const EXECUTOR: &str = "executor";

    /// Konsolidiert Projektgedächtnis-Fakten (Memory v3, §5.3): verschmilzt
    /// Duplikate, löst Widersprüche aus `facts/_conflicts.json` auf, senkt
    /// oder löscht veraltete Fakten, schreibt `MEMORY.md` neu. Bekommt
    /// bereits ausgewertete Bestände als Parameter, muss aber selbst
    /// schreiben können — die einzige eingebaute Rolle mit
    /// [`RegistryProfile::MemoryStewardship`] (siehe `agents/memory-steward.toml`
    /// und die Begründung bei [`RegistryProfile::MemoryStewardship`]).
    pub const MEMORY_STEWARD: &str = "memory-steward";

    /// Dedizierter Netz-Rechercheur der UIA (Addendum D+E, Agent REG-DE): die
    /// UIA spawnt selbst keine Worker außer diesem, wenn sie ohne Umweg über
    /// den Root-Orchestrator Netz-Recherche braucht. Gleiches Werkzeugprofil
    /// wie [`RESEARCHER_WEB`] (`RegistryProfile::Research`), siehe
    /// `agents/uia-worker.toml`.
    pub const UIA_WORKER: &str = "uia-worker";

    /// Read-only Erkundungsspezialisierung der UIA: die Spawn-Matrix
    /// (`harw-agent-dsl/src/roles.rs::can_spawn`) erlaubt der UIA
    /// (`AgentRoleId::UserInterface`) ausschließlich
    /// `RootOrchestrator`/`UiaWorker`/`AgentSteward` als Ziel — niemals den
    /// regulären `Worker`, unter dem `explorer`/`researcher-web` laufen. Die
    /// UIA-Werkzeuge `explore`/`research_web` spawnen intern aber genau diese
    /// beiden `role = "worker"`-Spezialisierungen und scheitern deshalb mit
    /// „no delegation capability is available for this request“, sobald die
    /// UIA sie aufruft. `uia-explorer` behebt das: dieselbe read-only
    /// Werkzeugoberfläche wie [`EXPLORER`]/[`RESEARCHER_WEB`] zusammen
    /// (`fs.read/list/search/glob/grep` plus `web.fetch`), aber unter der für
    /// die UIA bereits zugelassenen Organisationsrolle `role = "uia-worker"`
    /// (`AgentRoleId::UiaWorker`) statt `role = "worker"` — siehe
    /// `agents/uia-explorer.toml` und [`RegistryProfile::UiaExplorer`].
    pub const UIA_EXPLORER: &str = "uia-explorer";

    /// Schreibende Erkundungsspezialisierung der UIA: identische Begründung
    /// wie [`UIA_EXPLORER`], aber mit zusätzlichem `fs.write` — für
    /// UIA-Aufträge, die eine gefundene Datei auch tatsächlich ändern sollen,
    /// ohne dafür über `RootOrchestrator`/`AgentSteward` umzuleiten. Siehe
    /// `agents/uia-writer.toml` und [`RegistryProfile::UiaWriter`].
    pub const UIA_WRITER: &str = "uia-writer";

    /// Host-Shell-Spezialisierung der UIA: dieselbe Begründung wie
    /// [`UIA_EXPLORER`]/[`UIA_WRITER`] — die Spawn-Matrix lässt der UIA nie
    /// den regulären `Worker`, unter dem `host-process-worker` liefe
    /// (`role = "worker"`). Diese Rolle trägt deshalb `role = "uia-worker"`
    /// bei derselben Prozessoberfläche wie `host-process-worker`
    /// (`shell.exec`), ergänzt um den lesenden `fs.*`-Kern. Zweck: die UIA
    /// soll dem Nutzer bei Aufgaben helfen können, die er selbst nicht auf
    /// dem Host lösen kann — jede Ausführung bleibt dabei permitpflichtig
    /// und fail-closed (`SandboxProfile::Host`, siehe
    /// [`RegistryProfile::UiaShellWorker`] und `agents/uia-shell-worker.toml`).
    pub const UIA_SHELL_WORKER: &str = "uia-shell-worker";

    /// Setzt Agentendefinitionen um (Addendum K + Nachtrag K): die UIA und der
    /// Root-Orchestrator dürfen ihn spawnen (`AgentRoleId::AgentSteward`,
    /// `harw-agent-dsl/src/roles.rs`), er selbst spawnt nichts. Validiert
    /// und schreibt Agentendefinitionen (`agents.validate`,
    /// `agents.write_definition`) sowie neue UIA-Bündel
    /// (`agents.write_uia`, Nachtrag K) über
    /// `crate::agent_definition_tools::AgentDefinitionToolProvider` — die
    /// einzige eingebaute Rolle mit [`RegistryProfile::AgentStewardship`],
    /// siehe `agents/agent-steward.toml` und die Begründung dort.
    pub const AGENT_STEWARD: &str = "agent-steward";

    /// Alle bekannten eingebauten Rollen.
    ///
    /// Siehe die Moduldokumentation oben: `context-steward`, `intel-scout`,
    /// `cargo-worker`, `host-process-worker`, `sandbox-shell-worker` und
    /// `tmux-inspector-worker` sind absichtlich nicht enthalten (eigener,
    /// offener
    /// Befund), obwohl ihre Rollendateien bereits existieren und von
    /// [`crate::embedded_agents::builtin_agent_toml`] bereits gefunden werden.
    pub const ALL: &[&str] = &[
        ROOT_ORCHESTRATOR,
        EXPLORER,
        RESEARCHER_DEPS,
        RESEARCHER_WEB,
        PLANNER,
        ANALYST,
        SECURITY_EGRESS_TRIAGE,
        SECURITY_BASELINE_TRIAGE,
        SECURITY_STRUCTURE_TRIAGE,
        SECURITY_ENDPOINT_TRIAGE,
        EXECUTOR,
        MEMORY_STEWARD,
        UIA_WORKER,
        UIA_EXPLORER,
        UIA_WRITER,
        UIA_SHELL_WORKER,
        AGENT_STEWARD,
    ];
}

// ---------------------------------------------------------------------------
// Werkzeuglisten je Provider-Gruppe
// ---------------------------------------------------------------------------

/// Die lesenden Werkzeuge von `harw-tool-fs`.
pub(crate) const FS_READ_ONLY_TOOLS: &[&str] =
    &["fs.read", "fs.list", "fs.search", "fs.glob", "fs.grep"];

/// Die vollständige Werkzeugliste von `harw-tool-fs`, in Provider-Reihenfolge.
const FS_FULL_TOOLS: &[&str] = &[
    "fs.read",
    "fs.write",
    "fs.list",
    "fs.search",
    "fs.glob",
    "fs.grep",
];

/// Die Werkzeuge von [`RegistryProfile::MemoryStewardship`]: die fünf
/// lesenden `fs.*`-Werkzeuge plus `fs.write`, in Provider-Reihenfolge —
/// identisch zu [`FS_FULL_TOOLS`], aber bewusst als eigene Konstante
/// benannt, weil sie (anders als `FS_FULL_TOOLS`) nie mit `shell.exec`
/// zusammen registriert werden darf. Siehe `agents/memory-steward.toml`
/// und die Begründung bei [`RegistryProfile::MemoryStewardship`].
const MEMORY_STEWARDSHIP_TOOLS: &[&str] = FS_FULL_TOOLS;

/// Die Werkzeuge von `harw-tool-deps`, in Provider-Reihenfolge.
const DEPS_TOOLS: &[&str] = &[
    "deps.graph",
    "deps.locked",
    "deps.source_read",
    "deps.source_search",
    "deps.source_list",
];

/// Die Deps-Werkzeuge, die nur den Workspace (`Cargo.lock`/Metadaten) lesen
/// (`ReadWorkspace`). Teilmenge von [`DEPS_TOOLS`].
pub(crate) const DEPS_WORKSPACE_TOOLS: &[&str] = &["deps.graph", "deps.locked"];

/// Die Deps-Werkzeuge über dem Registry-Quellcache (`ReadCargoRegistry`).
/// Teilmenge von [`DEPS_TOOLS`].
pub(crate) const DEPS_SOURCE_TOOLS: &[&str] =
    &["deps.source_read", "deps.source_search", "deps.source_list"];

/// Die Werkzeuge von `harw-tool-web`, in Provider-Reihenfolge.
pub(crate) const WEB_TOOLS: &[&str] = &["web.fetch", "web.docs_rs", "web.crates_io"];

/// Das einzige Netz-Werkzeug von [`RegistryProfile::UiaQuickHelper`]
/// (Addendum I): nur `web.fetch`, ohne `web.docs_rs`/`web.crates_io` — die
/// Schnellhelfer-Rolle braucht keinen automatischen Crate-/Doku-Index, nur
/// gezieltes Nachschlagen einer einzelnen URL.
///
/// Dieselbe Begründung gilt für [`RegistryProfile::UiaExplorer`] und
/// [`RegistryProfile::UiaWriter`] — beide teilen sich diese Konstante statt
/// eine eigene, wertgleiche Liste zu pflegen.
pub(crate) const UIA_QUICK_HELPER_WEB_TOOLS: &[&str] = &["web.fetch"];

/// Das einzige Browser-Werkzeug von [`RegistryProfile::UiaQuickHelper`]
/// (Nutzerentscheidung, ersetzt Addendum I in diesem Punkt): `uia-worker`
/// darf `browser.open` benutzen — `agents/uia-worker.toml` admittiert es
/// weiterhin. Statt eines expliziten Laufzeit-Grants
/// (`browser_tool_provider`, das sonst für **jedes** `browser.*`-Werkzeug der
/// einzige Weg ist, siehe die Moduldokumentation oben) registriert und
/// bewirbt `UiaQuickHelper` dieses eine Werkzeug statisch, im selben Muster
/// wie [`UIA_QUICK_HELPER_WEB_TOOLS`] es für `web.fetch` tut — genau ein
/// benanntes Werkzeug, nicht die ganze Werkzeugfläche seines Providers.
/// Bewusst **kein** weiteres Browser-Werkzeug: `browser.observe`/`find`/
/// `act`/`wait`/`events`/`close` bleiben für jedes Profil (inklusive
/// `UiaQuickHelper`) verboten, siehe [`BROWSER_TOOLS`] und
/// `harw-registry-defaults/tests/role_rights_matrix.rs`.
pub(crate) const UIA_QUICK_HELPER_BROWSER_TOOLS: &[&str] = &["browser.open"];

/// Das lesende Werkzeug von
/// `crate::agent_definition_tools::AgentDefinitionToolProvider` (K-C,
/// Addendum K): parst und senkt einen Agentendefinitions-Entwurf über
/// dieselbe DSL-Pipeline, ohne zu schreiben.
pub(crate) const AGENT_DEFINITION_READ_TOOLS: &[&str] = &["agents.validate"];

/// Die lesenden Abfragewerkzeuge desselben Providers (Nachtrag K2):
/// `agents.list_proposals` listet die im Vorschlagsmodus
/// (`DefinitionWriteMode::ProposalOnly`) unter `<profil>/agents/.proposals/`
/// abgelegten, noch nicht committeten Entwürfe (Status
/// `pending_uia_review`).
pub(crate) const AGENT_DEFINITION_LIST_TOOLS: &[&str] = &["agents.list_proposals"];

/// Die schreibenden Werkzeuge desselben Providers (Addendum K + Nachtrag K):
/// `agents.write_definition` legt eine validierte Agentendefinition ab
/// (Projekt- oder Profil-Scope; im Vorschlagsmodus als Vorschlag statt
/// direkt), `agents.write_uia` legt ein neues UIA-Bündel (`definition.toml`,
/// `agent.toml`, `Personality.md`, optional `USER.md`) ausschließlich im
/// Profil-Scope an und aktiviert es nie, `agents.commit_proposal`/
/// `agents.reject_proposal` übernehmen oder verwerfen einen zuvor
/// abgelegten Vorschlag — laut Nachtrag K2 registriert
/// `crate::agent_definition_tools::AgentDefinitionToolProvider` die beiden
/// letzten nur im Commit-Modus (Elternrolle der UIA); die hier gepflegte
/// Liste bleibt die **maximale** (Commit-Modus-)Werkzeugmenge, siehe die
/// Begründung bei [`RegistryProfile::AgentStewardship`] und
/// `harw-registry-defaults/tests/tool_admission_coverage.rs`.
pub(crate) const AGENT_DEFINITION_WRITE_TOOLS: &[&str] = &[
    "agents.write_definition",
    "agents.write_uia",
    "agents.commit_proposal",
    "agents.reject_proposal",
];

/// Alle Werkzeuge von [`RegistryProfile::AgentStewardship`], in
/// Registrierungsreihenfolge: erst die lesenden Werkzeuge
/// (`agents.validate`, `agents.list_proposals`), dann die schreibenden —
/// dieselbe Reihenfolge, in der
/// `crate::agent_definition_tools::AgentDefinitionToolProvider` sie im
/// Commit-Modus (der maximalen Werkzeugmenge) bewirbt.
const AGENT_DEFINITION_TOOLS: &[&str] = &[
    "agents.validate",
    "agents.list_proposals",
    "agents.write_definition",
    "agents.write_uia",
    "agents.commit_proposal",
    "agents.reject_proposal",
];

/// Die Werkzeuge von `harw-tool-shell`.
pub(crate) const SHELL_TOOLS: &[&str] = &["shell.exec"];

/// Das eine Werkzeug von `harw-tool-lens` (AW6-10): semantische Abfrage über
/// `docs.design` und `knowledge.palace`.
///
/// # Warum nur `RegistryProfile::Planning` diese Konstante anhängt
/// `harw-tool-lens` wurde gebaut, damit Lens überhaupt einen Konsumenten
/// bekommt (`harw_tool_lens`s eigener `//!`-Block: „Ohne diesen Knoten
/// erreicht Lens nie einen Agenten“) — aber ein neues Werkzeug bekommt keine
/// Rolle automatisch, nur weil es existiert. Von den eingebauten Rollen
/// stellt nur `planner` überhaupt eine Retrieval-Frage im Sinne dieses
/// Werkzeugs:
///
/// - **`planner`**: erzeugt Planvorschläge und braucht dafür genau das, was
///   `lens.ask` indiziert — `docs.design` (die Design-Dokumente, aus denen
///   ein Plan hervorgeht) und `knowledge.palace` (bereits abgelegtes
///   Kontextwissen). Ohne diesen Zugriff plant er blind gegenüber Dokumenten,
///   die er nur noch per `fs.grep`/`fs.search` erraten könnte.
/// - **`explorer`**: erkundet Workspace und Dependency-Quellen strukturell
///   (`fs.*`, `deps.*`) — sein Auftrag ist wörtliches Auffinden, keine
///   semantische Frage an einen Index. Kein Beleg im Rollentext, dass er
///   `docs.design`/`knowledge.palace` befragen müsste.
/// - **`researcher-deps`**: befragt `Cargo.lock` und den lokalen
///   Registry-Quellcache — eine andere Datenquelle als die von `lens.ask`
///   indizierten Design-/Wissensbestände.
/// - **`researcher-web`**: recherchiert außerhalb der Sandbox über die
///   Host-Allowlist; `lens.ask` befragt ausschließlich interne Indizes, nie
///   das Netz. Beide Werkzeugflächen adressieren disjunkte Fragestellungen.
/// - **`analyst`**: verdichtet bereits vorliegende Ergebnisse read-only, teilt
///   sich aber technisch [`RegistryProfile::ReadOnlyExplore`] mit `explorer`
///   und `researcher-deps`. Eine Aufnahme hier würde `lens.ask` an alle drei
///   Rollen zugleich vergeben, obwohl nur `analyst` einen plausiblen
///   Retrieval-Bedarf hätte — das ist genau die unbemerkte Rechteausweitung,
///   die dieser Knoten vermeiden soll. Ein eigenes Profil für `analyst`
///   allein wäre eine Rollenzuschnitt-Entscheidung, die außerhalb dieses
///   Knotens getroffen werden sollte (siehe Abschlussbericht).
/// - **`context-steward`**, **`intel-scout`** (`agents/roles/**`, außerhalb
///   des Schreibbereichs dieses Knotens): `context-steward` nimmt seine
///   `ContextProposal`/`ModelBehaviorProposal`-Bestände laut eigener
///   Rollendefinition bereits als Parameter entgegen und beansprucht
///   ausdrücklich keine Werkzeugoberfläche (`[tools].admitted = []`); ein
///   Lens-Zugriff bliebe ungenutzter Code. `intel-scout` korreliert
///   Advisories gegen `Cargo.lock` (`[tools].admitted = ["deps.locked"]`) —
///   keine Design-/Wissensfrage.
pub(crate) const LENS_TOOLS: &[&str] = &["lens.ask"];

/// Die Browser-Werkzeuge, in Provider-Reihenfolge.
///
/// Kein Profil registriert die vollständige Liste (W5 RD): sie entstehen nur
/// über `browser_tool_provider` (Feature `browser`) mit ausdrücklichem Grant.
/// Die Liste bleibt ohne Feature bestehen, damit
/// [`crate::authority::tool_permission`] und die Rechte-Matrix sie kennen.
///
/// Einzige Ausnahme: [`RegistryProfile::UiaQuickHelper`] registriert daraus
/// statisch das erste Element, `browser.open`, über
/// [`UIA_QUICK_HELPER_BROWSER_TOOLS`] (Nutzerentscheidung) — die übrigen
/// sechs bleiben für jedes Profil verboten.
pub(crate) const BROWSER_TOOLS: &[&str] = &[
    "browser.open",
    "browser.observe",
    "browser.find",
    "browser.act",
    "browser.wait",
    "browser.events",
    "browser.close",
];

// Früher stand hier `PLANNING_OPERATION_TOOLS = ["plan", "goal"]`, das
// `RegistryProfile::Planning::tool_names` zusätzlich bewarb. Entfernt in W1-05
// (Register F-014, G-003): Kind-Registries tragen keinen `ModelToolProvider`,
// der Planner sah `plan`/`goal` also nur als Phantom ohne Executor — und der
// Deckungstest in `lib.rs` zwang beide über `tool_names()` in
// `AUTO_APPROVED_TOOLS`, obwohl sie `model_tool(approval = "always")`
// deklarieren. Das war der Approval-Bypass im Root.

// ---------------------------------------------------------------------------
// RegistryProfile
// ---------------------------------------------------------------------------

/// Welche Werkzeuge und welche Identität eine Session bzw. ein Kind-Agent bekommt.
///
/// # Beschreibung
/// Ein Profil legt drei Dinge gemeinsam fest, damit sie nicht auseinanderlaufen
/// können: die registrierten Tool-Provider, die im System-Prompt beworbene
/// Werkzeugliste und die Rollenbeschreibung. Vorher wurde die Werkzeugliste von
/// Hand gepflegt und bewarb Werkzeuge, die das Kind gar nicht besaß.
///
/// # Varianten
/// - `Full` — voller Coding-Satz: `fs.*`, `shell.exec` (ohne Browser, W5 RD).
/// - `ShellExecution` — ausschließlich `shell.exec`; kein Dateisystem-Werkzeug.
/// - `ReadOnlyExplore` — ausschließlich lesend.
/// - `Research` — **nur** `web.*` (W5 RD, Annahme A5): kein `fs.*`, kein
///   `deps.*`, damit die einzige Rolle mit Netz keine Workspace-Daten lesen und
///   hinaustragen kann. Netz-Scope aus `[network].researcher_web_hosts`
///   ([`crate::research_web::researcher_web_policy`]).
/// - `Planning` — `ReadOnlyExplore` plus `lens.ask` (Plan-/Goal-Operationen
///   bewirbt es nicht: das Kind besitzt dafür keinen Executor).
/// - `MemoryStewardship` — genau `fs.*` (alle sechs Werkzeuge, inklusive
///   `fs.write`), aber **kein** `shell.exec` und kein `web.*`. Einzige
///   eingebaute Rolle: [`role_names::MEMORY_STEWARD`] (siehe deren
///   Begründung bei [`RegistryProfile::MemoryStewardship`] unten für den
///   Grund, warum `Full` dafür zu weit wäre).
/// - `NoTools` — registriert und bewirbt gar nichts.
///
/// # Warum `NoTools` und nicht `ReadOnlyExplore` für die Triage-Rollen
/// Die vier `security-*-triage`-Rollen (siehe [`role_names`]) haben
/// `[tools].admitted = []`: sie bekommen bereits ausgewertete `Finding`-Batches
/// als Parameter, statt selbst ein Werkzeug aufzurufen. `ReadOnlyExplore`
/// registriert trotzdem zehn Werkzeuge (`fs.*`, `deps.*`) und bewirbt sie im
/// System-Prompt-Inventar — der Deckungstest
/// (`harw-registry-defaults/tests/tool_admission_coverage.rs`,
/// `every_profile_registered_tool_is_admitted_by_its_role`) verlangt aber
/// ausdrücklich, dass jedes beworbene Werkzeug einer Rolle auch `admitted`
/// ist. Ein leeres `admitted` gegen ein zehn Werkzeuge bewerbendes Profil
/// wäre also nicht bloß überflüssig, sondern ein roter Test — und genau die
/// Verwirrung des Werkzeuginventars, an der `lens.ask` in `planner.toml`
/// schon einmal gescheitert ist (siehe [`LENS_TOOLS`]), nur diesmal in die
/// andere Richtung: mehr im Inventar, als die Rolle je aufrufen darf.
/// `NoTools` registriert nichts und bewirbt nichts — Inventar und
/// Aufrufrecht bleiben deckungsgleich leer.
///
/// # Warum es kein `Default` gibt (R4, G-071)
/// `RegistryProfile` hatte `#[derive(Default)]` mit `#[default] Full`. Damit
/// war `profile_for_role(role).unwrap_or_default()` in beiden Kind-Factories
/// (`harw-tui/src/app.rs`, `harw-cli/src/chat.rs`) eine stille Ausweitung: die
/// **unbekannte** Rolle bekam den vollen Coding-Satz inklusive `fs.write` und
/// `shell.exec` — genau das Gegenteil der fail-closed-Zusage von
/// [`profile_for_role`], das bewusst `None` statt `Full` liefert. Ein
/// „Standardprofil“ kann es nicht geben: welcher Werkzeugsatz richtig ist,
/// hängt am Aufrufkontext, nie am Typ. Wer kein Profil ermitteln kann, muss
/// scheitern, nicht raten.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::RegistryProfile;
///
/// assert!(RegistryProfile::ReadOnlyExplore.is_read_only());
/// assert!(!RegistryProfile::ReadOnlyExplore.tool_names().contains(&"fs.write"));
/// assert!(RegistryProfile::NoTools.tool_names().is_empty());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryProfile {
    /// Voller Coding-Satz: fs.*, shell.exec. Browser nur über expliziten Grant.
    Full,
    /// Dedizierter Prozessworker: ausschließlich `shell.exec`. Das Profil ist
    /// absichtlich kein Fallback und erhält weder `fs.write` noch lesende
    /// Workspace-Werkzeuge.
    ShellExecution,
    /// Ausschließlich lesend: fs.read/list/search/glob/grep + deps.*.
    ReadOnlyExplore,
    /// Nur web.* — kein Workspace-Lesen (A5); Netz nur über
    /// `[network].researcher_web_hosts`.
    Research,
    /// ReadOnlyExplore + `lens.ask`. Die Plan-/Goal-Operationen der
    /// Composition-Root gehören **nicht** dazu (siehe W1-05).
    Planning,
    /// Registriert und bewirbt keine Werkzeuge — für Rollen, die bereits
    /// ausgewertete Befund-Batches als Parameter bekommen (`[tools].admitted
    /// = []`), etwa die vier `security-*-triage`-Rollen (siehe
    /// [`role_names::SECURITY_EGRESS_TRIAGE`] u. a.).
    NoTools,
    /// Genau `fs.*` (alle sechs Werkzeuge, inklusive `fs.write`) — kein
    /// `shell.exec`, kein `web.*`, kein `deps.*`, kein `lens.ask`.
    ///
    /// # Warum dieses Profil existiert (Memory v3, §5.3)
    /// [`role_names::MEMORY_STEWARD`] konsolidiert Projektgedächtnis-Fakten
    /// und muss dafür Fakten, `MEMORY.md` und ggf. Löschungen tatsächlich
    /// schreiben — ein reiner Vorschlag ([`RegistryProfile::ReadOnlyExplore`]
    /// o. ä.) würde jede Konsolidierung wirkungslos machen. [`RegistryProfile::Full`]
    /// wäre dafür zu weit: es registriert zusammen mit `fs.write` immer auch
    /// `shell.exec` (siehe dessen Moduldokumentation), obwohl die Rolle laut
    /// `agents/memory-steward.toml` ausdrücklich keinen Subprozess und keinen
    /// eigenen `git commit` ausführen darf — die git-Baseline aus §5.3 setzt
    /// die Composition-Root, nicht der Agent. Ein eigenes, engeres Profil
    /// trennt „darf Dateien schreiben“ von „darf Prozesse starten“, statt sie
    /// wie `Full` untrennbar zu bündeln.
    MemoryStewardship,
    /// Schnellhelfer der UIA (Addendum I, korrigiert REG-DE): lesender
    /// Workspace-Zugriff (`FS_READ_ONLY_TOOLS`) plus `shell.exec`
    /// (`SHELL_TOOLS`, läuft wie überall über Sandbox+Freigabe) plus
    /// ausschließlich `web.fetch` plus ausschließlich `browser.open`
    /// (Nutzerentscheidung, siehe [`UIA_QUICK_HELPER_BROWSER_TOOLS`]) — kein
    /// `fs.write`, kein `deps.*`, kein `lens.ask`, keine der übrigen sechs
    /// `browser.*`-Werkzeuge.
    ///
    /// # Warum dieses Profil existiert (Addendum I)
    /// [`role_names::UIA_WORKER`] war zuvor auf [`RegistryProfile::Research`]
    /// abgebildet (reine Netz-Recherche der UIA). Die Nutzerentscheidung aus
    /// Addendum I korrigiert das: `uia-worker` ist der exklusive
    /// Schnellhelfer der UIA für kleine Schnelleingriffe — eine Frage mit
    /// einem Aufruf beantworten, schnell etwas in der Shell regeln, eine
    /// Datei lesen — nicht nur Netz-Recherche. `web.fetch` bleibt die
    /// einzige Netz-Oberfläche (kein `web.docs_rs`/`web.crates_io`, die für
    /// tiefere Recherche gedacht sind, siehe `RegistryProfile::Research`).
    ///
    /// # Warum `browser.open` (spätere Nutzerentscheidung)
    /// `uia-worker` darf `browser.open` benutzen — `agents/uia-worker.toml`
    /// admittiert es weiterhin. Damit die Rolle das Werkzeug tatsächlich im
    /// System-Prompt-Inventar sieht (sonst hinge sie an einer Rückfrage fest,
    /// die sie mit `allow_pause = false` nie beantworten könnte, siehe
    /// `harw-registry-defaults/tests/tool_admission_coverage.rs`), registriert
    /// und bewirbt `UiaQuickHelper` dieses eine Browser-Werkzeug statisch —
    /// die übrigen sechs (`browser.observe`/`find`/`act`/`wait`/`events`/
    /// `close`) bleiben verboten, siehe
    /// `harw-registry-defaults/tests/role_rights_matrix.rs`.
    UiaQuickHelper,
    /// Der lesende `fs.*`-Kern ([`FS_READ_ONLY_TOOLS`]) plus die
    /// Agentendefinitions-Werkzeuge von
    /// `crate::agent_definition_tools::AgentDefinitionToolProvider`
    /// ([`AGENT_DEFINITION_TOOLS`]) — kein `fs.write`, kein `shell.exec`,
    /// kein `web.*`, kein `deps.*`, kein `lens.ask`.
    ///
    /// # Warum dieses Profil existiert (Addendum K + Nachtrag K)
    /// [`role_names::AGENT_STEWARD`] setzt Agentendefinitionen um, die die
    /// UIA (beratend) oder der Root-Orchestrator spezifiziert haben: er
    /// validiert einen Entwurf (`agents.validate`, rein lesend) und schreibt
    /// ihn erst danach atomar ab (`agents.write_definition`,
    /// `agents.write_uia`). `RegistryProfile::Full` wäre zu weit (kein
    /// `shell.exec`, keine ungeprüfte Schreiboberfläche über `fs.write`
    /// hinaus — jede Schreibung läuft durch die validierende Pipeline des
    /// Providers, nie über rohen Dateizugriff). `RegistryProfile::
    /// MemoryStewardship` passt ebenfalls nicht: `fs.write` erlaubt
    /// beliebige Dateien, während `agent-steward` nur über die geprüften
    /// Agenten-Werkzeuge schreiben darf.
    AgentStewardship,
    /// Read-only Erkundungsspezialisierung der UIA: [`FS_READ_ONLY_TOOLS`]
    /// plus [`UIA_QUICK_HELPER_WEB_TOOLS`] (`web.fetch`) — kein `fs.write`,
    /// kein `shell.exec`, kein `deps.*`, kein `lens.ask`.
    ///
    /// # Warum dieses Profil existiert
    /// Die Spawn-Matrix (`harw-agent-dsl/src/roles.rs::can_spawn`) lässt die
    /// UIA (`AgentRoleId::UserInterface`) ausschließlich
    /// `RootOrchestrator`/`UiaWorker`/`AgentSteward` spawnen — nie den
    /// regulären `Worker`. Die UIA-Werkzeuge `explore`/`research_web` spawnen
    /// intern aber `explorer`/`researcher-web`, beide `role = "worker"`, und
    /// scheitern deshalb an dieser Sperre. [`role_names::UIA_EXPLORER`] trägt
    /// stattdessen die bereits zugelassene Organisationsrolle
    /// `role = "uia-worker"` (`AgentRoleId::UiaWorker`) bei derselben
    /// read-only Werkzeugoberfläche wie
    /// [`RegistryProfile::ReadOnlyExplore`] plus `web.fetch` (ohne `deps.*`,
    /// das dort zusätzlich registriert wäre) — siehe
    /// `agents/uia-explorer.toml`.
    UiaExplorer,
    /// Schreibende Erkundungsspezialisierung der UIA: [`UiaExplorer`] plus
    /// `fs.write` (alle sechs `fs.*`-Werkzeuge, [`FS_FULL_TOOLS`]) plus
    /// [`UIA_QUICK_HELPER_WEB_TOOLS`] — kein `shell.exec`, kein `deps.*`,
    /// kein `lens.ask`.
    ///
    /// # Warum dieses Profil existiert
    /// Dieselbe Begründung wie bei [`RegistryProfile::UiaExplorer`]: ein
    /// UIA-Auftrag, der eine gefundene Datei auch tatsächlich ändern soll,
    /// braucht `fs.write` unter der für die UIA zugelassenen
    /// Organisationsrolle `role = "uia-worker"` — siehe
    /// `agents/uia-writer.toml` und [`role_names::UIA_WRITER`].
    UiaWriter,
    /// Host-Shell-Spezialisierung der UIA: [`FS_READ_ONLY_TOOLS`] plus
    /// [`SHELL_TOOLS`] — kein `fs.write`, kein `web.*`, kein `deps.*`, kein
    /// `lens.ask`.
    ///
    /// # Warum dieses Profil existiert
    /// Dieselbe Begründung wie bei [`RegistryProfile::UiaExplorer`]/
    /// [`RegistryProfile::UiaWriter`]: die Spawn-Matrix
    /// (`harw-agent-dsl/src/roles.rs::can_spawn`) lässt die UIA nie den
    /// regulären `Worker`, unter dem `host-process-worker` liefe. Diese
    /// Rolle ist der fehlende Zwilling für Host-Ausführung unter
    /// `role = "uia-worker"` — die UIA soll dem Nutzer bei Aufgaben helfen
    /// können, die er selbst nicht auf dem Host lösen kann. Anders als
    /// [`RegistryProfile::UiaQuickHelper`] (das ebenfalls `shell.exec`
    /// registriert) hängt dieses Profil seinen `ShellToolProvider`
    /// ausdrücklich an [`harw_sandbox::SandboxProfile::Host`] statt an das
    /// von der Runtime übergebene Umgebungsprofil — jede Ausführung bleibt
    /// damit permitpflichtig und fail-closed, siehe
    /// `agents/uia-shell-worker.toml` und [`role_names::UIA_SHELL_WORKER`].
    UiaShellWorker,
}

impl RegistryProfile {
    /// Alle Profile in Deklarationsreihenfolge.
    ///
    /// Nützlich für erschöpfende Tests und für CLI-Hilfetexte.
    pub const ALL: &'static [RegistryProfile] = &[
        RegistryProfile::Full,
        RegistryProfile::ShellExecution,
        RegistryProfile::ReadOnlyExplore,
        RegistryProfile::Research,
        RegistryProfile::Planning,
        RegistryProfile::NoTools,
        RegistryProfile::MemoryStewardship,
        RegistryProfile::UiaQuickHelper,
        RegistryProfile::AgentStewardship,
        RegistryProfile::UiaExplorer,
        RegistryProfile::UiaWriter,
        RegistryProfile::UiaShellWorker,
    ];

    /// Liefert die Rollenbeschreibung, die im System-Prompt erscheint.
    ///
    /// # Rückgabe
    /// Ein kurzes, menschenlesbares Label wie `"read-only exploration agent"`.
    /// [`IdentityOverrides::role_description`] kann es ersetzen.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_registry_defaults::profile::RegistryProfile;
    ///
    /// assert_eq!(RegistryProfile::Full.role_description(), "coding agent");
    /// ```
    #[must_use]
    pub const fn role_description(self) -> &'static str {
        match self {
            RegistryProfile::Full => "coding agent",
            RegistryProfile::ShellExecution => "sandboxed process execution agent",
            RegistryProfile::ReadOnlyExplore => "read-only exploration agent",
            RegistryProfile::Research => "research agent",
            RegistryProfile::Planning => "planning agent",
            RegistryProfile::NoTools => "parameter-only triage agent",
            RegistryProfile::MemoryStewardship => "memory consolidation agent",
            RegistryProfile::UiaQuickHelper => "quick helper of the user interface agent",
            RegistryProfile::AgentStewardship => "agent definition steward",
            RegistryProfile::UiaExplorer => {
                "read-only exploration specialization of the user interface agent"
            }
            RegistryProfile::UiaWriter => {
                "writing exploration specialization of the user interface agent"
            }
            RegistryProfile::UiaShellWorker => {
                "host shell execution specialization of the user interface agent"
            }
        }
    }

    /// Gibt an, ob dieses Profil ausschließlich lesende Werkzeuge registriert.
    ///
    /// # Rückgabe
    /// `false` für [`RegistryProfile::Full`], das reine Prozessprofil
    /// [`RegistryProfile::ShellExecution`],
    /// [`RegistryProfile::MemoryStewardship`] (registriert `fs.write`),
    /// [`RegistryProfile::UiaQuickHelper`] (registriert `shell.exec`),
    /// [`RegistryProfile::AgentStewardship`] (registriert die schreibenden
    /// Agentendefinitions-Werkzeuge `agents.write_definition`/
    /// `agents.write_uia`), [`RegistryProfile::UiaWriter`] (registriert
    /// `fs.write`) und [`RegistryProfile::UiaShellWorker`] (registriert
    /// `shell.exec`). Alle übrigen Profile — inklusive
    /// [`RegistryProfile::UiaExplorer`] — sind read-only und dürfen weder
    /// `fs.write` noch `shell.exec` sehen.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_registry_defaults::profile::RegistryProfile;
    ///
    /// assert!(!RegistryProfile::Full.is_read_only());
    /// assert!(RegistryProfile::Planning.is_read_only());
    /// assert!(!RegistryProfile::MemoryStewardship.is_read_only());
    /// ```
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        !matches!(
            self,
            RegistryProfile::Full
                | RegistryProfile::ShellExecution
                | RegistryProfile::MemoryStewardship
                | RegistryProfile::UiaQuickHelper
                | RegistryProfile::AgentStewardship
                | RegistryProfile::UiaWriter
                | RegistryProfile::UiaShellWorker
        )
    }

    /// Die Werkzeuge, die **dieses Crate** für das Profil registriert.
    ///
    /// # Beschreibung
    /// Die Reihenfolge entspricht exakt der Reihenfolge, in der
    /// [`assemble_registry`] die Provider registriert, und damit der Reihenfolge
    /// von `ExtensionRegistry::tool_providers().flat_map(ToolProvider::tools)`.
    /// `browser.*` gehört zu keinem Profil (W5 RD). `Research` registriert
    /// ausschließlich `web.*`. `Planning` hängt zusätzlich `lens.ask` an —
    /// siehe die Begründung bei [`LENS_TOOLS`], warum ausschließlich
    /// `planner` dieses Werkzeug bekommt.
    ///
    /// # Rückgabe
    /// Die Tool-Namen in Registrierungsreihenfolge.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_registry_defaults::profile::RegistryProfile;
    ///
    /// let tools = RegistryProfile::ReadOnlyExplore.registered_tool_names();
    /// assert_eq!(tools.first(), Some(&"fs.read"));
    /// assert!(!tools.contains(&"shell.exec"));
    /// ```
    #[must_use]
    pub fn registered_tool_names(self) -> Vec<&'static str> {
        match self {
            RegistryProfile::Full => FS_FULL_TOOLS
                .iter()
                .chain(SHELL_TOOLS.iter())
                .copied()
                .collect(),
            RegistryProfile::ShellExecution => SHELL_TOOLS.to_vec(),
            RegistryProfile::ReadOnlyExplore => FS_READ_ONLY_TOOLS
                .iter()
                .chain(DEPS_TOOLS.iter())
                .copied()
                .collect(),
            // `Planning` teilt den read-only Kern mit `ReadOnlyExplore`, hängt
            // aber zusätzlich `lens.ask` an — siehe die Begründung bei
            // `LENS_TOOLS`, warum ausschließlich `planner` dieses Werkzeug
            // bekommt.
            RegistryProfile::Planning => FS_READ_ONLY_TOOLS
                .iter()
                .chain(DEPS_TOOLS.iter())
                .chain(LENS_TOOLS.iter())
                .copied()
                .collect(),
            // Nur `web.*` (A5): die Rolle mit Netz liest keine Workspace-Daten.
            RegistryProfile::Research => WEB_TOOLS.to_vec(),
            // Siehe die Begründung bei `RegistryProfile::NoTools`: keine
            // Werkzeuge registriert, keine beworben.
            RegistryProfile::NoTools => Vec::new(),
            // Alle sechs `fs.*`-Werkzeuge (inklusive `fs.write`), aber kein
            // `shell.exec` — siehe die Begründung bei
            // `RegistryProfile::MemoryStewardship`.
            RegistryProfile::MemoryStewardship => MEMORY_STEWARDSHIP_TOOLS.to_vec(),
            // Schnellhelfer der UIA (Addendum I): lesender fs.*-Kern plus
            // `shell.exec` plus ausschließlich `web.fetch` plus
            // ausschließlich `browser.open` (Nutzerentscheidung) — siehe die
            // Begründung bei `RegistryProfile::UiaQuickHelper`.
            RegistryProfile::UiaQuickHelper => FS_READ_ONLY_TOOLS
                .iter()
                .chain(SHELL_TOOLS.iter())
                .chain(UIA_QUICK_HELPER_WEB_TOOLS.iter())
                .chain(UIA_QUICK_HELPER_BROWSER_TOOLS.iter())
                .copied()
                .collect(),
            // `agent-steward` (Addendum K + Nachtrag K2): lesender fs.*-Kern
            // plus die Agentendefinitions-Werkzeuge in ihrer maximalen
            // (Commit-Modus-)Ausprägung — siehe die Begründung bei
            // `RegistryProfile::AgentStewardship`. Bewusst weiterhin die
            // **maximale** Vertrags-Obermenge, unabhängig davon, dass der
            // tatsächlich montierte Provider ohne `AgentDefinitionAccess`
            // fail-closed nur zwei dieser Werkzeuge registriert (Nachtrag
            // K3) — siehe [`agent_definition_tool_names_for_access`] und
            // [`assemble_registry_for_sandbox_with_definition_access`] für
            // den tatsächlichen Laufzeitzustand.
            RegistryProfile::AgentStewardship => FS_READ_ONLY_TOOLS
                .iter()
                .chain(AGENT_DEFINITION_TOOLS.iter())
                .copied()
                .collect(),
            // Read-only Erkundungsspezialisierung der UIA (siehe die
            // Begründung bei `RegistryProfile::UiaExplorer`): lesender
            // fs.*-Kern plus ausschließlich `web.fetch` — kein `deps.*`.
            RegistryProfile::UiaExplorer => FS_READ_ONLY_TOOLS
                .iter()
                .chain(UIA_QUICK_HELPER_WEB_TOOLS.iter())
                .copied()
                .collect(),
            // Schreibende Erkundungsspezialisierung der UIA (siehe die
            // Begründung bei `RegistryProfile::UiaWriter`): alle sechs
            // fs.*-Werkzeuge (inklusive `fs.write`) plus ausschließlich
            // `web.fetch` — kein `deps.*`, kein `shell.exec`.
            RegistryProfile::UiaWriter => FS_FULL_TOOLS
                .iter()
                .chain(UIA_QUICK_HELPER_WEB_TOOLS.iter())
                .copied()
                .collect(),
            // Host-Shell-Spezialisierung der UIA (siehe die Begründung bei
            // `RegistryProfile::UiaShellWorker`): lesender fs.*-Kern plus
            // `shell.exec` — kein `web.*`, kein `deps.*`.
            RegistryProfile::UiaShellWorker => FS_READ_ONLY_TOOLS
                .iter()
                .chain(SHELL_TOOLS.iter())
                .copied()
                .collect(),
        }
    }

    /// Die vollständige Werkzeugoberfläche, die der System-Prompt bewirbt.
    ///
    /// # Beschreibung
    /// Für **alle** Profile identisch zu
    /// [`RegistryProfile::registered_tool_names`]: beworben wird nur, wofür die
    /// Registry des Profils auch einen Executor trägt. Früher hängte `Planning`
    /// zusätzlich `plan` und `goal` an — Operationen der Composition-Root, die
    /// eine Kind-Registry nie besitzt; über die Deckungstests erzwang das ihre
    /// Auto-Freigabe (W1-05, Register F-014/G-003).
    ///
    /// # Rückgabe
    /// Die beworbenen Tool-Namen in Prompt-Reihenfolge.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_registry_defaults::profile::RegistryProfile;
    ///
    /// let planning = RegistryProfile::Planning.tool_names();
    /// assert!(planning.contains(&"lens.ask"));
    /// assert!(!planning.contains(&"plan"));
    /// assert!(!planning.contains(&"goal"));
    /// assert!(!planning.contains(&"fs.write"));
    /// ```
    #[must_use]
    pub fn tool_names(self) -> Vec<&'static str> {
        self.registered_tool_names()
    }

    /// Die Rechte, die jedes Werkzeug dieses Profils zusammen verlangt.
    ///
    /// # Beschreibung
    /// Vereinigung von [`crate::authority::tool_permission`] über
    /// [`RegistryProfile::registered_tool_names`]. Unter genau diesem Satz
    /// registriert [`assemble_registry_for_sandbox`] das volle Profil.
    ///
    /// # Rückgabe
    /// Ein [`PermissionSet`]; leer für [`RegistryProfile::NoTools`].
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_registry_defaults::profile::RegistryProfile;
    /// use harw_authority::Permission;
    ///
    /// let research = RegistryProfile::Research.required_permissions();
    /// assert!(research.contains(Permission::NetworkAccess));
    /// assert!(!research.contains(Permission::ReadWorkspace));
    /// ```
    #[must_use]
    pub fn required_permissions(self) -> PermissionSet {
        permissions_of(&self.registered_tool_names())
    }

    /// Die Werkzeuge dieses Profils, deren Recht `granted` trägt.
    ///
    /// # Beschreibung
    /// Registry-seitiger Reducer: ein Werkzeug ohne gewährtes Recht (oder ohne
    /// bekanntes Recht) fällt heraus, statt beworben zu werden und am
    /// Rechte-Prolog zu scheitern. Beispiel `ReadOnlyExplore` ohne
    /// `ReadCargoRegistry`: `deps.source_*` fehlt (R0-Frage, F-084).
    ///
    /// # Argumente
    /// - `granted` (`&PermissionSet`): die Rechte der Ziel-Sandbox.
    ///
    /// # Rückgabe
    /// Teilmenge von [`RegistryProfile::registered_tool_names`] in
    /// Registrierungsreihenfolge.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_registry_defaults::profile::RegistryProfile;
    /// use harw_authority::{Permission, PermissionSet};
    ///
    /// let granted = PermissionSet::from_policy([Permission::ReadWorkspace]);
    /// let tools = RegistryProfile::ReadOnlyExplore.tool_names_for(&granted);
    /// assert!(tools.contains(&"deps.locked"));
    /// assert!(!tools.contains(&"deps.source_read"));
    /// ```
    #[must_use]
    pub fn tool_names_for(self, granted: &PermissionSet) -> Vec<&'static str> {
        self.registered_tool_names()
            .into_iter()
            .filter(|tool| tool_permission(tool).is_some_and(|needed| granted.contains(needed)))
            .collect()
    }
}

/// Das Registry-Profil, das eine eingebaute Rolle erhält.
///
/// # Beschreibung
/// Bildet die Rollennamen aus [`role_names`] auf ihr Profil ab. Ein unbekannter
/// Name liefert `None` — der Aufrufer muss den Fall behandeln (fail-closed),
/// statt auf [`RegistryProfile::Full`] zurückzufallen und einem unbekannten
/// Kind versehentlich Schreibrechte zu geben.
///
/// # Argumente
/// - `role` (`&str`): der Rollenname, üblicherweise eine Konstante aus [`role_names`].
///
/// # Rückgabe
/// `Some(profile)` für eine bekannte eingebaute Rolle, sonst `None`.
///
/// # Nebenläufigkeit
/// Rein; von jedem Thread aus sicher.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::{profile_for_role, role_names, RegistryProfile};
///
/// assert_eq!(
///     profile_for_role(role_names::RESEARCHER_WEB),
///     Some(RegistryProfile::Research)
/// );
/// assert_eq!(
///     profile_for_role(role_names::SECURITY_EGRESS_TRIAGE),
///     Some(RegistryProfile::NoTools)
/// );
/// assert_eq!(profile_for_role("unbekannt"), None);
/// ```
#[must_use]
pub fn profile_for_role(role: &str) -> Option<RegistryProfile> {
    match role {
        role_names::ROOT_ORCHESTRATOR => Some(RegistryProfile::Planning),
        role_names::EXPLORER | role_names::RESEARCHER_DEPS | role_names::ANALYST => {
            Some(RegistryProfile::ReadOnlyExplore)
        }
        role_names::RESEARCHER_WEB => Some(RegistryProfile::Research),
        role_names::PLANNER => Some(RegistryProfile::Planning),
        // Die vier Triage-Rollen admittieren kein Werkzeug (`[tools].admitted
        // = []`) — sie bekommen bereits ausgewertete Befund-Batches als
        // Parameter. Siehe die Begründung bei `RegistryProfile::NoTools`.
        role_names::SECURITY_EGRESS_TRIAGE
        | role_names::SECURITY_BASELINE_TRIAGE
        | role_names::SECURITY_STRUCTURE_TRIAGE
        | role_names::SECURITY_ENDPOINT_TRIAGE => Some(RegistryProfile::NoTools),
        // Einzige eingebaute Rolle mit der Prozessoberfläche: sie führt nur
        // beauftragte Sandbox-Prozesse aus. Das ist eine ausdrückliche,
        // dokumentierte Ausnahme (siehe `agents/executor.toml` und den Test
        // `test_only_executor_gets_the_shell_execution_profile` in
        // `embedded_agents.rs`), kein Fallback: jede andere unbekannte Rolle
        // fällt weiterhin auf `None`.
        role_names::EXECUTOR => Some(RegistryProfile::ShellExecution),
        // Einzige eingebaute Rolle mit der schmalen fs.*-Schreiboberfläche
        // ohne shell.exec — siehe die Begründung bei
        // `RegistryProfile::MemoryStewardship` und `agents/memory-steward.toml`.
        role_names::MEMORY_STEWARD => Some(RegistryProfile::MemoryStewardship),
        // `uia-worker` ist der exklusive Schnellhelfer der UIA (Addendum I,
        // korrigiert REG-DE) — nicht mehr nur Netz-Recherche
        // (`RegistryProfile::Research`), sondern lesender Workspace-Zugriff
        // plus `shell.exec` plus `web.fetch`, siehe `agents/uia-worker.toml`
        // und die Begründung bei `RegistryProfile::UiaQuickHelper`.
        role_names::UIA_WORKER => Some(RegistryProfile::UiaQuickHelper),
        // Read-only Erkundungsspezialisierung der UIA — siehe die Begründung
        // bei `RegistryProfile::UiaExplorer` und `agents/uia-explorer.toml`.
        role_names::UIA_EXPLORER => Some(RegistryProfile::UiaExplorer),
        // Schreibende Erkundungsspezialisierung der UIA — siehe die
        // Begründung bei `RegistryProfile::UiaWriter` und
        // `agents/uia-writer.toml`.
        role_names::UIA_WRITER => Some(RegistryProfile::UiaWriter),
        // Host-Shell-Spezialisierung der UIA — siehe die Begründung bei
        // `RegistryProfile::UiaShellWorker` und `agents/uia-shell-worker.toml`.
        role_names::UIA_SHELL_WORKER => Some(RegistryProfile::UiaShellWorker),
        // Einzige eingebaute Rolle mit den Agentendefinitions-Werkzeugen —
        // siehe die Begründung bei `RegistryProfile::AgentStewardship` und
        // `agents/agent-steward.toml`.
        role_names::AGENT_STEWARD => Some(RegistryProfile::AgentStewardship),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// IdentityOverrides
// ---------------------------------------------------------------------------

/// Überschreibungen für die Agenten-Identität im System-Prompt.
///
/// # Beschreibung
/// Ein Kind-Agent trägt denselben Werkzeugsatz wie sein Profil, aber einen
/// eigenen Namen, eine eigene Rollenbeschreibung und zusätzliche
/// Kontextfragmente (etwa seinen Auftrag oder seinen Rückgabevertrag).
/// Nicht gesetzte Felder behalten den Profil-Default.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::IdentityOverrides;
///
/// let overrides = IdentityOverrides {
///     agent_name: Some("explorer-3".to_owned()),
///     ..IdentityOverrides::default()
/// };
/// assert_eq!(overrides.agent_name.as_deref(), Some("explorer-3"));
/// assert!(overrides.extra_context.is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct IdentityOverrides {
    /// Name des Agenten; `None` behält `"harw"`.
    pub agent_name: Option<String>,
    /// Rollenbeschreibung; `None` behält [`RegistryProfile::role_description`].
    pub role_description: Option<String>,
    /// Zusätzliche Kontextfragmente für [`harw_instructions::AgentIdentity`].
    pub extra_context: Vec<String>,
    /// Organisatorische Rolle dieses Agenten im Agenten-Baum (Addendum F+G,
    /// Agent F-FIX; seit Addendum K über
    /// [`crate::embedded_agents::builtin_organization_knowledge`], das für
    /// die UIA, `agent-steward`, Root- und Sub-Orchestrator zusätzlich das
    /// Organisations- und Bauplan-Wissen voranstellt). `Some(role)` hängt den
    /// zusammengesetzten Text für genau diese Rolle an den System-Prompt an;
    /// `None` hängt kein Regelwerk an — anders
    /// als zuvor wird nicht mehr bedingungslos das Worker-Regelwerk vergeben,
    /// da über diesen Weg auch Nicht-Worker-Knoten (z. B. `Composite`-Knoten
    /// im Plan-Baum) zusammengestellt werden können.
    pub organizational_role: Option<harw_agent_dsl::roles::AgentRoleId>,
}

// ---------------------------------------------------------------------------
// RestrictedToolProvider
// ---------------------------------------------------------------------------

/// Blendet aus einem Tool-Provider alle nicht ausdrücklich erlaubten Werkzeuge aus.
///
/// # Beschreibung
/// Ein Provider bündelt Werkzeuge, die nicht immer gemeinsam vergeben werden
/// dürfen: `harw_tool_fs::FsToolProvider` liefert `fs.write` zusammen mit den
/// fünf lesenden Werkzeugen. Dieser Wrapper filtert `tools()` **und**
/// `executor()`, sodass ein ausgeblendetes Werkzeug weder im Inventar erscheint
/// noch durch Raten seines Namens ausführbar ist.
///
/// Unbekannte oder ausgeblendete Namen liefern `None` bzw. `false` — fail closed.
///
/// # Nebenläufigkeit
/// `Send + Sync`; nach der Konstruktion unveränderlich.
///
/// # Beispiele
/// ```rust
/// use std::sync::Arc;
/// use harw_extension_api::{ToolName, ToolProvider};
/// use harw_registry_defaults::profile::RestrictedToolProvider;
/// use harw_tool_fs::FsToolProvider;
///
/// let provider = RestrictedToolProvider::new(
///     Arc::new(FsToolProvider::default()),
///     &["fs.read"],
/// );
/// assert_eq!(provider.tools().len(), 1);
/// assert!(provider.executor(&ToolName::new("fs.write")).is_none());
/// ```
pub struct RestrictedToolProvider {
    /// Der gefilterte Provider.
    inner: Arc<dyn ToolProvider>,
    /// Die erlaubten Tool-Namen; alles andere ist unsichtbar.
    allowed: Vec<String>,
}

impl RestrictedToolProvider {
    /// Erzeugt einen Filter über `inner`, der nur `allowed` durchlässt.
    ///
    /// # Argumente
    /// - `inner` (`Arc<dyn ToolProvider>`): der zu filternde Provider; der
    ///   `Arc` wird übernommen (nur der Zeiger, nicht der Provider, wird geteilt).
    /// - `allowed` (`&[&str]`): die sichtbaren Tool-Namen.
    ///
    /// # Rückgabe
    /// Ein `RestrictedToolProvider`, der ausschließlich `allowed` freigibt.
    ///
    /// # Nebenläufigkeit
    /// Thread-sicher; kein veränderlicher Zustand.
    #[must_use]
    pub fn new(inner: Arc<dyn ToolProvider>, allowed: &[&str]) -> Self {
        Self {
            inner,
            allowed: allowed.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    /// Gibt an, ob `name` durch den Filter darf.
    fn allows(&self, name: &str) -> bool {
        self.allowed.iter().any(|allowed| allowed == name)
    }
}

impl ToolProvider for RestrictedToolProvider {
    /// Gibt nur die Spezifikationen der erlaubten Werkzeuge zurück.
    ///
    /// # Rückgabe
    /// Die Teilmenge von `inner.tools()`, deren Name freigegeben ist — in der
    /// Reihenfolge des inneren Providers.
    fn tools(&self) -> Vec<ToolSpec> {
        self.inner
            .tools()
            .into_iter()
            .filter(|spec| self.allows(spec.name()))
            .collect()
    }

    /// Löst nur erlaubte Namen in einen Executor auf.
    ///
    /// # Rückgabe
    /// `None` für ausgeblendete und unbekannte Namen — ein Modell kann ein
    /// gefiltertes Werkzeug damit nicht durch Raten erreichen.
    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        if !self.allows(name.as_str()) {
            return None;
        }
        self.inner.executor(name)
    }

    /// Gibt die Parallelitätszusage des inneren Providers weiter.
    ///
    /// # Rückgabe
    /// `false` für ausgeblendete Namen, sonst `inner.parallel_safe(name)`.
    fn parallel_safe(&self, name: &ToolName) -> bool {
        self.allows(name.as_str()) && self.inner.parallel_safe(name)
    }
}

// ---------------------------------------------------------------------------
// Zusammenbau
// ---------------------------------------------------------------------------

/// Der geteilte Vertrag für `crate::agent_definition_tools::
/// AgentDefinitionToolProvider` (Welle FANIN-K, Nachtrag K3): Verzeichnisse,
/// Schreibmodus und Urheber-Decke in einem Wert.
///
/// # Description
/// Ersetzt das frühere, private `AgentDefinitionDirs` (nur Verzeichnisse) —
/// mit der Urheber-Decke aus Nachtrag K3 reicht ein reiner Pfadwert nicht
/// mehr aus, der Aufrufer muss auch Schreibmodus und Rechte-Obergrenze des
/// Eltern-Aufrufers mitgeben. `project_agents_dir`/`profile_agents_dir`
/// spiegeln die gleichnamigen Konstruktorargumente von
/// `AgentDefinitionToolProvider::new`; `mode`/`ceiling` ebenso.
///
/// # Bekannte Lücke (ABWEICHUNG, siehe Abschlussbericht dieses Knotens)
/// Diese Crate hängt nicht von `harw-home` ab (keine `Cargo.toml`-Änderung
/// in diesem Auftrag) und kennt deshalb kein aktives Profil oder dessen
/// Home-Verzeichnis — der `None`-Standardpfad
/// ([`assemble_registry_for_sandbox_with_definition_access`] ohne `access`)
/// lässt `profile_agents_dir` deshalb `None`. Ebenso kennt dieser Standardpfad
/// weder Elternrolle noch Elternrechte und setzt `mode =
/// DefinitionWriteMode::ProposalOnly`, `ceiling = None` (fail-closed,
/// Nachtrag K2/K3) — die Elternrollen-/Elternrechte-abhängige Wahl ist Sache
/// des Fan-in an der Montagestelle (`harw-runtime/src/children.rs`), die
/// diese Werte über `access` explizit übergibt.
#[derive(Debug, Clone)]
pub struct AgentDefinitionAccess {
    /// Ziel für `scope = "project"`/`"run"`; `None`, wenn kein
    /// Projektkontext bekannt ist.
    pub project_agents_dir: Option<PathBuf>,
    /// Ziel für `scope = "profile"`, jedes `agents.write_uia`-Bundle und
    /// `.proposals/`; `None`, wenn kein Profil bekannt ist.
    pub profile_agents_dir: Option<PathBuf>,
    /// Ob diese Instanz `agents.commit_proposal`/`agents.reject_proposal`
    /// registriert (nur zusammen mit einer gesetzten `ceiling`).
    pub mode: DefinitionWriteMode,
    /// Die Urheber-Decke des Eltern-Aufrufers des Stewards (Nachtrag K3);
    /// `None` ⇒ fail-closed — der Provider registriert dann ausschließlich
    /// `agents.validate`/`agents.list_proposals`.
    pub ceiling: Option<DefinitionAuthorCeiling>,
}

/// Die Agentendefinitions-Werkzeuge, die `AgentDefinitionToolProvider`
/// tatsächlich registriert, für eine gegebene [`AgentDefinitionAccess`] (oder
/// den fail-closed Standard `None`) — Nachtrag K3.
///
/// # Description
/// Spiegelt exakt `AgentDefinitionToolProvider::tools()`: erst die beiden
/// immer registrierten lesenden Werkzeuge, dann — nur wenn `access` gesetzt
/// ist **und** eine `ceiling` trägt — die beiden Schreib-Werkzeuge, dann —
/// nur zusätzlich im [`DefinitionWriteMode::Commit`] — die beiden
/// Freigabe-Werkzeuge. `None` (kein Zugriff konfiguriert) liefert nur die
/// ersten beiden — der fail-closed Standard, den
/// [`assemble_registry_for_sandbox_with_definition_access`] ohne `access`
/// verwendet.
///
/// Diese Funktion ist die Quelle für den tatsächlich zur Laufzeit
/// registrierten Werkzeugsatz von [`RegistryProfile::AgentStewardship`] —
/// anders als [`RegistryProfile::registered_tool_names`], die für dieses eine
/// Profil bewusst bei der **maximalen** Vertrags-Obermenge bleibt (siehe
/// deren Dokumentation und `harw-registry-defaults/tests/
/// tool_admission_coverage.rs`).
///
/// # Arguments
/// - `access` (`Option<&`[`AgentDefinitionAccess`]`>`): die zu prüfende
///   Konfiguration; nur `mode` und `ceiling` fließen ein.
///
/// # Returns
/// Die Tool-Namen in Registrierungsreihenfolge.
#[must_use]
pub fn agent_definition_tool_names_for_access(
    access: Option<&AgentDefinitionAccess>,
) -> Vec<&'static str> {
    let mut tools: Vec<&'static str> = AGENT_DEFINITION_READ_TOOLS
        .iter()
        .chain(AGENT_DEFINITION_LIST_TOOLS.iter())
        .copied()
        .collect();
    if let Some(access) = access {
        if access.ceiling.is_some() {
            tools.extend_from_slice(&AGENT_DEFINITION_WRITE_TOOLS[..2]);
            if access.mode == DefinitionWriteMode::Commit {
                tools.extend_from_slice(&AGENT_DEFINITION_WRITE_TOOLS[2..]);
            }
        }
    }
    tools
}

/// Die Verdrahtung für Host-Profil-Permit-Prüfung, die
/// [`profile_tool_providers`] an jeden tatsächlich mit
/// [`SandboxProfile::Host`] gebauten [`ShellToolProvider`] hängt.
///
/// # Beschreibung
/// Bündelt die drei Werte, die zusammen die Permit-Kette eines
/// Host-Profil-`shell.exec`-Aufrufs bilden: den einmal je Lauf
/// instanziierten [`ProcessPermitLedger`], die dazugehörige
/// [`HostPermitSessionRegistry`] und die Sendeseite des
/// Host-Permit-Fragekanals ([`HostPermitPromptSender`],
/// `harw_tool_shell::host_permit_prompt`). Ohne angehängten Sender lehnt
/// `harw_tool_shell::exec::ShellExecutor::authorize_host_command` jede
/// Anfrage ohne bereits gemerkten Permit oder laufende Sitzungsphase sofort
/// ab — vor dieser Ergänzung erreichte der Sender, den
/// `harw_runtime::assembly::RuntimeAssembly` bereits baute, nie einen
/// gebauten `ShellToolProvider`, sodass ein Host-Profil-Shell-Worker nie eine
/// Rückfrage stellen konnte. Dieser Typ schließt genau diese Lücke, indem er
/// den Sender zusammen mit Ledger und Registry an [`profile_tool_providers`]
/// transportiert.
///
/// # Vorausgewählte Variante
/// `preselected_variant` ist reine Anzeige-Vorauswahl für eine offene Frage
/// (siehe [`HostPermitVariant`]) und ändert nie, was tatsächlich genehmigt
/// wird. Die Vorgabe über [`Self::new`] ist [`HostPermitVariant::default`]
/// (`SingleExecution`); [`Self::with_preselected_variant`] wählt bewusst eine
/// andere Variante.
#[derive(Debug, Clone)]
pub struct HostPermitWiring {
    /// Der einmal je Lauf instanziierte Permit-Ledger.
    pub ledger: Arc<ProcessPermitLedger>,
    /// Die dazugehörige Sitzungs-Registry.
    pub registry: Arc<HostPermitSessionRegistry>,
    /// Sendeseite des Host-Permit-Fragekanals; ohne sie bleibt Host-Ausführung
    /// ohne bereits gemerkten Permit oder laufende Sitzungsphase fail-closed.
    pub prompt_sender: HostPermitPromptSender,
    /// Anzeige-Vorauswahl für eine neu geöffnete Frage; ändert nie, was
    /// tatsächlich genehmigt wird.
    pub preselected_variant: HostPermitVariant,
}

impl HostPermitWiring {
    /// Baut die Verdrahtung mit [`HostPermitVariant::default`]
    /// (`SingleExecution`) als Vorauswahl.
    #[must_use]
    pub fn new(
        ledger: Arc<ProcessPermitLedger>,
        registry: Arc<HostPermitSessionRegistry>,
        prompt_sender: HostPermitPromptSender,
    ) -> Self {
        Self {
            ledger,
            registry,
            prompt_sender,
            preselected_variant: HostPermitVariant::default(),
        }
    }

    /// Setzt die Anzeige-Vorauswahl einer neu geöffneten Frage.
    #[must_use]
    pub fn with_preselected_variant(mut self, variant: HostPermitVariant) -> Self {
        self.preselected_variant = variant;
        self
    }
}

/// Erzeugt die Tool-Provider eines Profils in Registrierungsreihenfolge.
///
/// # Argumente
/// - `profile` ([`RegistryProfile`]): das zu bauende Profil.
/// - `agent_definition_access` ([`AgentDefinitionAccess`]): nur für
///   [`RegistryProfile::AgentStewardship`] relevant; alle anderen Zweige
///   ignorieren den Parameter.
///
/// Infallibel: seit W5 RD baut kein Profil mehr einen Browser-Host.
fn profile_tool_providers(
    profile: RegistryProfile,
    agent_definition_access: AgentDefinitionAccess,
    sandbox_profile: &SandboxProfile,
    // Nur für Host-Profil-Worker relevant (siehe `host-process-worker.toml`):
    // die Runtime-Montage reicht hier die einmal instanziierte
    // `HostPermitWiring` (Ledger + Sitzungs-Registry + Fragekanal-Sender)
    // durch, damit jeder gebaute `ShellToolProvider` sowohl
    // `run_command`s tatsächliche `authorize()`-Prüfung als auch eine
    // Rückfrage über den Sender erreichen kann. `None` verhält sich exakt wie
    // vor dieser Ergänzung (Host-Ausführung bleibt dann fail-closed ohne
    // Ledger).
    host_permits: Option<&HostPermitWiring>,
) -> Vec<Arc<dyn ToolProvider>> {
    // Baut einen `ShellToolProvider` für `sandbox_profile` und hängt bei
    // Host-Profil (falls übergeben) Ledger, Sitzungs-Registry, Fragekanal-
    // Sender und vorausgewählte Variante an.
    let build_shell_provider = |sandbox_profile: &SandboxProfile| -> Arc<dyn ToolProvider> {
        let mut provider =
            ShellToolProvider::default().with_sandbox_profile(sandbox_profile.clone());
        if sandbox_profile.is_host() {
            if let Some(wiring) = host_permits {
                provider = provider
                    .with_permit_ledger(Arc::clone(&wiring.ledger))
                    .with_host_permit_registry(Arc::clone(&wiring.registry))
                    .with_host_permit_prompts(wiring.prompt_sender.clone())
                    .with_preselected_permit_variant(wiring.preselected_variant);
            }
        }
        Arc::new(provider)
    };
    // Der read-only Anteil ist für drei Profile identisch: der gefilterte
    // FS-Provider plus der vollständig lesende Deps-Provider.
    fn read_only_base() -> Vec<Arc<dyn ToolProvider>> {
        let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
            Arc::new(FsToolProvider::default()),
            FS_READ_ONLY_TOOLS,
        ));
        let dependencies: Arc<dyn ToolProvider> = Arc::new(DepsToolProvider::new());
        vec![filesystem, dependencies]
    }

    match profile {
        RegistryProfile::Full => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(FsToolProvider::default());
            let shell = build_shell_provider(sandbox_profile);
            vec![filesystem, shell]
        }
        RegistryProfile::ShellExecution => vec![build_shell_provider(sandbox_profile)],
        RegistryProfile::ReadOnlyExplore => read_only_base(),
        // `LensToolProvider::new()` ist zustandslos (keine Bau-, Home- oder
        // Indexpfad-Konfiguration nötig): `derive_read_scope` leitet den
        // `ReadScope` beim Aufruf aus dem `ToolExecutionContext` ab, nie aus
        // einem Konstruktorargument. `profile.rs` muss ihm deshalb nichts
        // zusätzlich mitgeben.
        RegistryProfile::Planning => {
            let mut providers = read_only_base();
            providers.push(Arc::new(LensToolProvider::new()));
            providers
        }
        // Kein `read_only_base` (A5): weder `fs.*` noch `deps.*`. Die Policy
        // aus `[network].researcher_web_hosts` reicht W6 I-CONTRIB an die
        // Web-Werkzeuge durch, sobald `harw-tool-web` (N-WEB) sie annimmt.
        RegistryProfile::Research => {
            let web: Arc<dyn ToolProvider> = Arc::new(WebToolProvider::new());
            vec![web]
        }
        // Keine Provider: siehe die Begründung bei `RegistryProfile::NoTools`.
        RegistryProfile::NoTools => Vec::new(),
        // Der volle, ungefilterte `FsToolProvider` (alle sechs `fs.*`, inklusive
        // `fs.write`) — aber kein `ShellToolProvider`. Siehe die Begründung bei
        // `RegistryProfile::MemoryStewardship`.
        RegistryProfile::MemoryStewardship => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(FsToolProvider::default());
            vec![filesystem]
        }
        // Schnellhelfer der UIA (Addendum I): gefilterter, lesender
        // FS-Provider + voller Shell-Provider (Sandbox+Freigabe greifen wie
        // überall) + auf `web.fetch` gefilterter Web-Provider.
        RegistryProfile::UiaQuickHelper => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(FsToolProvider::default()),
                FS_READ_ONLY_TOOLS,
            ));
            let shell = build_shell_provider(sandbox_profile);
            let web: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(WebToolProvider::new()),
                UIA_QUICK_HELPER_WEB_TOOLS,
            ));
            vec![filesystem, shell, web]
        }
        // `agent-steward` (Addendum K + Nachtrag K/K2/K3): gefilterter,
        // lesender FS-Provider + der Agentendefinitions-Provider, dessen
        // Verzeichnisse, Schreibmodus und Urheber-Decke vollständig aus
        // `agent_definition_access` kommen. Diese Funktion trifft selbst
        // keine Modus-/Decke-Entscheidung mehr — das ist Sache des Aufrufers
        // ([`assemble_registry_for_sandbox_with_definition_access`]): ohne
        // `access` (der Standardpfad über [`assemble_registry_for_sandbox`])
        // bleibt es beim fail-closed Vorschlagsmodus ohne Decke (Nachtrag K2:
        // „unbekannt ⇒ ProposalOnly“); die elternrollen-/elternrechte-
        // abhängige Wahl (`Commit` nur für einen von der UIA gestarteten
        // Steward, Decke aus den effektiven Elternrechten) ist Sache des
        // Fan-in an der Montagestelle (`harw-runtime/src/children.rs::
        // definition_write_mode_for_parent_role`), das seine Wahl über
        // `access` an [`assemble_registry_for_project_with_definition_access`]
        // durchreicht (ABWEICHUNG: diese Verdrahtung selbst ist nicht Teil
        // dieses Knotens, siehe Abschlussbericht).
        RegistryProfile::AgentStewardship => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(FsToolProvider::default()),
                FS_READ_ONLY_TOOLS,
            ));
            let agent_definitions: Arc<dyn ToolProvider> = Arc::new(
                crate::agent_definition_tools::AgentDefinitionToolProvider::new(
                    agent_definition_access.project_agents_dir,
                    agent_definition_access.profile_agents_dir,
                    agent_definition_access.mode,
                    agent_definition_access.ceiling,
                ),
            );
            vec![filesystem, agent_definitions]
        }
        // Read-only Erkundungsspezialisierung der UIA: gefilterter, lesender
        // FS-Provider + auf `web.fetch` gefilterter Web-Provider — siehe die
        // Begründung bei `RegistryProfile::UiaExplorer`.
        RegistryProfile::UiaExplorer => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(FsToolProvider::default()),
                FS_READ_ONLY_TOOLS,
            ));
            let web: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(WebToolProvider::new()),
                UIA_QUICK_HELPER_WEB_TOOLS,
            ));
            vec![filesystem, web]
        }
        // Schreibende Erkundungsspezialisierung der UIA: voller, ungefilterter
        // FS-Provider (alle sechs `fs.*`, inklusive `fs.write`) + auf
        // `web.fetch` gefilterter Web-Provider — kein `ShellToolProvider`.
        // Siehe die Begründung bei `RegistryProfile::UiaWriter`.
        RegistryProfile::UiaWriter => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(FsToolProvider::default());
            let web: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(WebToolProvider::new()),
                UIA_QUICK_HELPER_WEB_TOOLS,
            ));
            vec![filesystem, web]
        }
        // Host-Shell-Spezialisierung der UIA: gefilterter, lesender
        // FS-Provider + Shell-Provider — anders als `build_shell_provider`
        // in den übrigen Zweigen hängt dieser Zweig ausdrücklich
        // `SandboxProfile::Host` an, nicht das von der Runtime übergebene
        // `sandbox_profile` — siehe die Begründung bei
        // `RegistryProfile::UiaShellWorker`. Ledger und Sitzungs-Registry
        // hängt `build_shell_provider` bereits automatisch an, sobald
        // `is_host()` gilt und `host_permits` übergeben wurde.
        RegistryProfile::UiaShellWorker => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(FsToolProvider::default()),
                FS_READ_ONLY_TOOLS,
            ));
            let shell = build_shell_provider(&SandboxProfile::Host);
            vec![filesystem, shell]
        }
    }
}

/// Filtert einen Provider auf die Werkzeuge in `allowed`.
///
/// Gibt den Provider unverändert zurück, wenn nichts herausfällt, und `None`,
/// wenn nichts übrig bleibt (ein leerer Provider wird nicht registriert).
fn restrict_provider(
    provider: Arc<dyn ToolProvider>,
    allowed: &[&'static str],
) -> Option<Arc<dyn ToolProvider>> {
    let offered: Vec<ToolSpec> = provider.tools();
    let kept = offered
        .iter()
        .filter(|spec| allowed.iter().any(|name| *name == spec.name()))
        .count();
    if kept == 0 {
        None
    } else if kept == offered.len() {
        Some(provider)
    } else {
        Some(Arc::new(RestrictedToolProvider::new(provider, allowed)))
    }
}

/// Baut die Browser-Werkzeuge mit einem **ausdrücklichen** Öffnungs-Grant.
///
/// # Beschreibung
/// Einziger Weg zu `browser.*` (W5 RD, F-073): kein Profil registriert sie
/// mehr. Der Grant (`harw_tool_browser::BrowserOpenGrant`) legt Origins,
/// Profilbindung und Limits host-seitig fest; das Modell kann ihn nicht
/// erweitern. Ohne Grant gibt es keinen Provider. Der Aufrufer (W6 I-CONTRIB)
/// baut den Grant aus `[browser]` und registriert den Provider nur, wenn
/// `[browser].enabled = true`.
///
/// `FirefoxHost::new` führt keine I/O aus und startet keinen Prozess; ein
/// fehlender WebDriver fällt erst beim ersten `browser.open` auf.
///
/// # Argumente
/// - `grant` (`BrowserOpenGrant`): die host-eigene Öffnungs-Autorität; Eigentum
///   geht über.
///
/// # Rückgabe
/// `Ok(Arc<dyn ToolProvider>)` mit den sieben `browser.*`-Werkzeugen.
///
/// # Fehler
/// - [`RegistryDefaultsError::BrowserHost`]: die Firefox-Host-Konfiguration ist
///   ungültig.
///
/// # Nebenläufigkeit
/// Synchron; der Provider ist `Send + Sync`.
#[cfg(feature = "browser")]
pub fn browser_tool_provider(
    grant: BrowserOpenGrant,
) -> RegistryDefaultsResult<Arc<dyn ToolProvider>> {
    let host: Arc<dyn harw_browser::host::BrowserHost> = Arc::new(
        FirefoxHost::new(FirefoxHostConfig::default())
            .map_err(|error| RegistryDefaultsError::BrowserHost(error.to_string()))?,
    );
    let tool_set = BrowserToolSet::with_open_policy(host, BrowserOpenPolicy::grant(grant));
    Ok(Arc::new(HarwnessBrowserToolProvider::new(tool_set)))
}

/// Baut den Browser-Provider aus der vertrauenswürdigen `[browser]`-Sektion.
///
/// `browser.enabled = false` entfernt die Browser-Oberfläche vollständig.
/// Bei aktivierter Konfiguration werden die Origins als Navigation-Allowlist
/// mit privatem Netzwerk-Veto, eine leere Authentifizierungs-Allowlist und das
/// konfigurierte Aktionslimit in den host-eigenen Grant übernommen. Die
/// Geckodriver-Pinning-Felder gehören zur Backend-Konfiguration und werden von
/// dieser Registry-Verdrahtung nicht erweitert.
///
/// Jede ungültige Konfiguration bleibt fail-closed und wird als
/// [`RegistryDefaultsError::BrowserHost`] zurückgegeben.
#[cfg(feature = "browser")]
pub fn browser_tool_provider_for_config(
    section: &harw_config::BrowserSection,
) -> RegistryDefaultsResult<Option<Arc<dyn ToolProvider>>> {
    if !section.enabled {
        return Ok(None);
    }

    section
        .validate()
        .map_err(RegistryDefaultsError::BrowserHost)?;
    let allowed_origins =
        OriginPolicy::from_origins(section.allowed_origins.iter().map(String::as_str), true)
            .map_err(|error| RegistryDefaultsError::BrowserHost(error.to_string()))?;
    let grant = BrowserOpenGrant::ephemeral(allowed_origins, OriginPolicy::default())
        .with_limits(BrowserLimits::default().with_max_actions_per_session(section.max_actions));

    browser_tool_provider(grant).map(Some)
}

/// Baut eine Registry für das gewünschte Profil.
///
/// # Beschreibung
/// Entdeckt zuerst den Projektkontext unter `cwd`, registriert dann genau die
/// Tool-Provider des Profils und baut daraus eine [`AssembledRegistry`]. Die
/// [`harw_instructions::AgentIdentity`] bewirbt **genau** die Werkzeuge des
/// Profils ([`RegistryProfile::tool_names`]) und trägt dessen
/// Rollenbeschreibung, sofern `overrides` sie nicht ersetzt.
///
/// Die Projekterkennung läuft **genau einmal**; der Rest der Montage liegt in
/// [`assemble_registry_for_project`], an das diese Funktion delegiert. Wer
/// mehrere Registries über demselben Projekt baut (Sitzung plus Kinder), ruft
/// diese Funktion einmal und danach nur noch
/// [`assemble_registry_for_project`] mit dem erhaltenen
/// [`AssembledRegistry::project`].
///
/// Die Freigabegrenze ist für alle Profile dieselbe
/// [`crate::DefaultApprovalPolicy`]: sie lässt ausschließlich read-only
/// Werkzeuge ohne Rückfrage durch.
///
/// # Freigabemodus
/// Diese Signatur nimmt keine [`ApprovalModeCell`] entgegen und baut deshalb
/// eine **eigene** Zelle auf [`harw_extension_api::ApprovalMode::Delegated`]
/// ([`ApprovalModeCell::default`]) — die engste Stufe ohne Einstellung, aber
/// eben auch eine Zelle, die niemand außerhalb der erzeugten Registry umlegen
/// kann. Eine Sitzung, deren Modus zur Laufzeit umschaltbar sein muss
/// (`/permissions set`), montiert über [`assemble_registry_for_project`] und
/// gibt einen Klon ihrer eigenen Zelle mit. Das Durchreichen der
/// Sitzungs-Zelle aus den Composition-Roots (`harw-tui`, `harw-cli`) ist
/// W2c-Folgearbeit.
///
/// # Argumente
/// - `profile` ([`RegistryProfile`]): das gewünschte Werkzeug-/Identitätsprofil.
/// - `cwd` (`PathBuf`): Startpunkt der Projekterkennung; Eigentum geht über.
/// - `overrides` ([`IdentityOverrides`]): Überschreibungen für den System-Prompt.
///
/// # Rückgabe
/// `Ok(AssembledRegistry)` mit Registry, erkanntem Projektkontext und Identität.
///
/// # Fehler
/// - [`RegistryDefaultsError::ProjectDiscovery`][]: `cwd` ist kein auflösbares Projekt.
/// - [`RegistryDefaultsError::ContextProviderRegistration`][]: siehe
///   [`assemble_registry_for_project`].
///
/// # Nebenläufigkeit
/// Synchron; alle erzeugten Provider sind `Send + Sync`.
///
/// # Beispiele
/// ```rust,no_run
/// use std::path::PathBuf;
/// use harw_registry_defaults::profile::{assemble_registry, IdentityOverrides, RegistryProfile};
///
/// let assembled = assemble_registry(
///     RegistryProfile::ReadOnlyExplore,
///     PathBuf::from("/workspace"),
///     IdentityOverrides::default(),
/// )?;
/// assert_eq!(assembled.identity.role_description, "read-only exploration agent");
/// # Ok::<(), harw_registry_defaults::RegistryDefaultsError>(())
/// ```
pub fn assemble_registry(
    profile: RegistryProfile,
    cwd: PathBuf,
    overrides: IdentityOverrides,
) -> RegistryDefaultsResult<AssembledRegistry> {
    let discovery_config = DiscoveryConfig::default();
    let project = discover_project(&cwd, &discovery_config).map_err(RegistryDefaultsError::from)?;

    assemble_registry_for_project(profile, &project, overrides, ApprovalModeCell::default())
}

/// Baut eine Registry für das gewünschte Profil über einem **bereits
/// erkannten** Projektkontext.
///
/// # Beschreibung
/// Identisch zu [`assemble_registry`], nur ohne dessen ersten Schritt: Diese
/// Funktion nimmt keinen Pfad entgegen, sie kann also gar keine
/// Projekterkennung auslösen. Wer eine Sitzung und ihre Kind-Registries
/// montiert, entdeckt einmal und übergibt denselben [`ProjectContext`] an jeden
/// Aufruf.
///
/// # Warum das die eigentliche Änderung ist (R5, G-071)
/// Jede Kind-Registry und jede Registry-Kopie lief bisher durch
/// [`assemble_registry`] und damit durch eine eigene
/// [`harw_project_discovery::discover_project`]-Runde: Start der TUI dreimal,
/// jeder Fan-out einmal je Kind. Das ist nicht nur Arbeit, es ist eine
/// Fehlerquelle — ein `AGENTS.md`, das zwischen zwei Runden unlesbar wird,
/// ließe Elternteil und Kind auf verschiedene Projektwurzeln blicken, und ein
/// einziges nicht-UTF-8-Dokument verhinderte jeden Kind-Spawn. Ein einmal
/// erkannter Kontext kann beides nicht.
///
/// # Argumente
/// - `profile` ([`RegistryProfile`]): das gewünschte Werkzeug-/Identitätsprofil.
/// - `project` (`&ProjectContext`): der bereits erkannte Projektkontext; wird
///   für die Registry und das Ergebnis geklont, nie neu ermittelt.
/// - `overrides` ([`IdentityOverrides`]): Überschreibungen für den System-Prompt.
/// - `approval_mode` ([`ApprovalModeCell`]): die Zelle, aus der die
///   [`crate::DefaultApprovalPolicy`] dieser Registry ihren Freigabemodus
///   liest. Ein Klon der Sitzungs-Zelle lässt Kind und Elternteil denselben
///   Modus sehen, [`ApprovalModeCell::detached`] trennt sie.
///
/// # Rückgabe
/// `Ok(AssembledRegistry)` mit Registry, dem **übergebenen** Projektkontext und
/// der Identität.
///
/// # Fehler
/// - [`RegistryDefaultsError::ContextProviderRegistration`]: der Namensraum des
///   [`ProjectContextProvider`] ist bereits belegt.
///
/// Ein Discovery-Fehler ist hier **nicht** möglich: Die Funktion bekommt keinen
/// Pfad.
///
/// # Nebenläufigkeit
/// Synchron; alle erzeugten Provider sind `Send + Sync`.
///
/// # Beispiele
/// ```rust,no_run
/// use std::path::Path;
/// use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
/// use harw_project_discovery::{DiscoveryConfig, discover_project};
/// use harw_registry_defaults::profile::{
///     IdentityOverrides, RegistryProfile, assemble_registry_for_project,
/// };
///
/// // Einmal entdecken …
/// let project = discover_project(Path::new("/workspace"), &DiscoveryConfig::default())?;
/// let mode = ApprovalModeCell::new(ApprovalMode::Delegated);
///
/// // … und beliebig oft montieren, ohne erneute Projekterkennung.
/// for _ in 0..2 {
///     let assembled = assemble_registry_for_project(
///         RegistryProfile::ReadOnlyExplore,
///         &project,
///         IdentityOverrides::default(),
///         mode.clone(),
///     )?;
///     assert_eq!(assembled.identity.role_description, "read-only exploration agent");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn assemble_registry_for_project(
    profile: RegistryProfile,
    project: &ProjectContext,
    overrides: IdentityOverrides,
    approval_mode: ApprovalModeCell,
) -> RegistryDefaultsResult<AssembledRegistry> {
    assemble_registry_for_project_with_definition_access(
        profile,
        project,
        overrides,
        approval_mode,
        None,
    )
}

/// Wie [`assemble_registry_for_project`], nimmt aber zusätzlich eine
/// [`AgentDefinitionAccess`] entgegen (Welle FANIN-K, Nachtrag K3).
///
/// # Beschreibung
/// [`assemble_registry_for_project`] ist genau `..._with_definition_access(..,
/// None)`: ohne `access` bleibt `crate::agent_definition_tools::
/// AgentDefinitionToolProvider` fail-closed bei
/// `DefinitionWriteMode::ProposalOnly` ohne Decke — registriert also nur
/// `agents.validate`/`agents.list_proposals`, unabhängig vom Profil. Wer eine
/// Kind-Registry für `RegistryProfile::AgentStewardship` montiert und dem
/// Steward tatsächlich Schreibrechte geben will, übergibt `Some(access)` mit
/// den Verzeichnissen, dem gewählten Modus (`Commit` nur, wenn der Eltern-
/// Aufrufer die UIA ist, siehe Nachtrag K2) und der Urheber-Decke des
/// Eltern-Aufrufers (Nachtrag K3).
///
/// # Argumente
/// - `profile` ([`RegistryProfile`]): das gewünschte Werkzeug-/Identitätsprofil.
/// - `project` (`&ProjectContext`): bereits erkannter Projektkontext.
/// - `overrides` ([`IdentityOverrides`]): Überschreibungen für den System-Prompt.
/// - `approval_mode` ([`ApprovalModeCell`]): Freigabemodus-Zelle der Politik.
/// - `access` (`Option<`[`AgentDefinitionAccess`]`>`): nur für
///   [`RegistryProfile::AgentStewardship`] relevant; `None` ist der
///   fail-closed Standard.
///
/// # Rückgabe
/// `Ok(AssembledRegistry)` mit Registry, dem **übergebenen** Projektkontext und
/// der Identität.
///
/// # Fehler
/// - [`RegistryDefaultsError::ContextProviderRegistration`]: der Namensraum des
///   [`ProjectContextProvider`] ist bereits belegt.
///
/// # Nebenläufigkeit
/// Synchron; alle erzeugten Provider sind `Send + Sync`.
pub fn assemble_registry_for_project_with_definition_access(
    profile: RegistryProfile,
    project: &ProjectContext,
    overrides: IdentityOverrides,
    approval_mode: ApprovalModeCell,
    access: Option<AgentDefinitionAccess>,
) -> RegistryDefaultsResult<AssembledRegistry> {
    assemble_registry_for_sandbox_with_definition_access(
        profile,
        project,
        overrides,
        approval_mode,
        &profile.required_permissions(),
        access,
    )
}

/// Baut eine Registry über einem erkannten Projektkontext und registriert nur
/// die Werkzeuge, deren Recht die Ziel-Sandbox trägt.
///
/// # Beschreibung
/// Wie [`assemble_registry_for_project`], aber jede Provider-Liste wird auf
/// [`RegistryProfile::tool_names_for`]`(granted)` gefiltert — Registrierung
/// **und** Identität (beworbenes Inventar). Ein Werkzeug ohne gewährtes Recht
/// ist damit weder sichtbar noch per Namensraten ausführbar
/// ([`RestrictedToolProvider`]); ein Provider ohne verbleibendes Werkzeug wird
/// gar nicht registriert.
///
/// Das beantwortet die R0-Frage „`deps.*` nur mit `ReadCargoRegistry`“ hart:
/// `ReadOnlyExplore` unter `{ReadWorkspace}` registriert `deps.graph` und
/// `deps.locked`, aber kein `deps.source_*`.
///
/// Composition-Roots geben den Rechtesatz der Sandbox, in der die Session läuft
/// (Kind: `AuthorityReducer::reduce` über den Elternsatz, siehe
/// [`crate::authority::authority_reducer_for_role`]). Der Aufruf mit
/// [`RegistryProfile::required_permissions`] registriert das volle Profil —
/// genau das tut [`assemble_registry_for_project`].
///
/// # Argumente
/// - `profile` ([`RegistryProfile`]): Werkzeug-/Identitätsprofil.
/// - `project` (`&ProjectContext`): bereits erkannter Projektkontext.
/// - `overrides` ([`IdentityOverrides`]): Überschreibungen für den System-Prompt.
/// - `approval_mode` ([`ApprovalModeCell`]): Freigabemodus-Zelle der Politik.
/// - `granted` (`&PermissionSet`): Rechte der Ziel-Sandbox; nur geliehen.
///
/// # Rückgabe
/// `Ok(AssembledRegistry)`; `identity.tools_available` ist exakt die Menge der
/// registrierten Werkzeuge.
///
/// # Fehler
/// - [`RegistryDefaultsError::ContextProviderRegistration`]: der Namensraum des
///   [`ProjectContextProvider`] ist bereits belegt.
///
/// # Nebenläufigkeit
/// Synchron; alle erzeugten Provider sind `Send + Sync`.
///
/// # Beispiele
/// ```rust,no_run
/// use std::path::Path;
/// use harw_extension_api::approval_mode::ApprovalModeCell;
/// use harw_project_discovery::{DiscoveryConfig, discover_project};
/// use harw_registry_defaults::authority::reduce_to_read_registry;
/// use harw_registry_defaults::profile::{
///     IdentityOverrides, RegistryProfile, assemble_registry_for_sandbox,
/// };
/// use harw_authority::{Permission, PermissionSet};
///
/// let project = discover_project(Path::new("/workspace"), &DiscoveryConfig::default())?;
/// let parent = PermissionSet::from_policy([Permission::ReadWorkspace]);
/// let assembled = assemble_registry_for_sandbox(
///     RegistryProfile::ReadOnlyExplore,
///     &project,
///     IdentityOverrides::default(),
///     ApprovalModeCell::default(),
///     &reduce_to_read_registry(&parent),
/// )?;
/// assert!(!assembled.identity.tools_available.contains(&"deps.source_read".to_owned()));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn assemble_registry_for_sandbox(
    profile: RegistryProfile,
    project: &ProjectContext,
    overrides: IdentityOverrides,
    approval_mode: ApprovalModeCell,
    granted: &PermissionSet,
) -> RegistryDefaultsResult<AssembledRegistry> {
    assemble_registry_for_sandbox_with_definition_access(
        profile,
        project,
        overrides,
        approval_mode,
        granted,
        None,
    )
}

/// Wie [`assemble_registry_for_sandbox`], nimmt aber zusätzlich eine
/// [`AgentDefinitionAccess`] entgegen (Welle FANIN-K, Nachtrag K3).
///
/// # Beschreibung
/// [`assemble_registry_for_sandbox`] ist genau `..._with_definition_access(..,
/// None)`. Ohne `access` leitet sich `project_agents_dir` weiterhin aus dem
/// bereits erkannten Projektkontext ab (`profile_agents_dir` bleibt `None`,
/// `mode = DefinitionWriteMode::ProposalOnly`, `ceiling = None` — fail-closed,
/// siehe [`AgentDefinitionAccess`]).
///
/// Für [`RegistryProfile::AgentStewardship`] weicht die beworbene und
/// registrierte Werkzeugmenge dabei bewusst von
/// [`RegistryProfile::tool_names_for`] ab: `tool_names_for` bleibt die
/// statische **maximale** Vertrags-Obergrenze (siehe
/// [`RegistryProfile::registered_tool_names`]), während diese Funktion über
/// [`agent_definition_tool_names_for_access`] genau die Werkzeuge bewirbt, die
/// `AgentDefinitionToolProvider` unter der übergebenen `access` **tatsächlich**
/// registriert — sonst würde die Identität Werkzeuge ankündigen, an denen ein
/// Kind mit `allow_pause = false` folgenlos hängen bliebe. Alle anderen
/// Profile verhalten sich unverändert zu [`assemble_registry_for_sandbox`].
///
/// # Argumente
/// - `profile` ([`RegistryProfile`]): Werkzeug-/Identitätsprofil.
/// - `project` (`&ProjectContext`): bereits erkannter Projektkontext.
/// - `overrides` ([`IdentityOverrides`]): Überschreibungen für den System-Prompt.
/// - `approval_mode` ([`ApprovalModeCell`]): Freigabemodus-Zelle der Politik.
/// - `granted` (`&PermissionSet`): Rechte der Ziel-Sandbox; nur geliehen.
/// - `access` (`Option<`[`AgentDefinitionAccess`]`>`): nur für
///   [`RegistryProfile::AgentStewardship`] relevant; `None` ist der
///   fail-closed Standard.
///
/// # Rückgabe
/// `Ok(AssembledRegistry)`; `identity.tools_available` ist exakt die Menge der
/// registrierten Werkzeuge.
///
/// # Fehler
/// - [`RegistryDefaultsError::ContextProviderRegistration`]: der Namensraum des
///   [`ProjectContextProvider`] ist bereits belegt.
///
/// # Nebenläufigkeit
/// Synchron; alle erzeugten Provider sind `Send + Sync`.
pub fn assemble_registry_for_sandbox_with_definition_access(
    profile: RegistryProfile,
    project: &ProjectContext,
    overrides: IdentityOverrides,
    approval_mode: ApprovalModeCell,
    granted: &PermissionSet,
    access: Option<AgentDefinitionAccess>,
) -> RegistryDefaultsResult<AssembledRegistry> {
    assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile(
        profile,
        project,
        overrides,
        approval_mode,
        granted,
        access,
        &SandboxProfile::Strict,
    )
}

/// Wie [`assemble_registry_for_sandbox_with_definition_access`], nimmt aber
/// zusätzlich ein [`SandboxProfile`] entgegen, das an jeden konstruierten
/// [`ShellToolProvider`] weitergegeben wird. Das Profil ist ein
/// vertrauenswürdiger Runtime-Input: nur der Runtime-Aufbau (nicht ein
/// Tool-Aufruf) wählt es.
pub fn assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile(
    profile: RegistryProfile,
    project: &ProjectContext,
    overrides: IdentityOverrides,
    approval_mode: ApprovalModeCell,
    granted: &PermissionSet,
    access: Option<AgentDefinitionAccess>,
    sandbox_profile: &SandboxProfile,
) -> RegistryDefaultsResult<AssembledRegistry> {
    assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
        profile,
        project,
        overrides,
        approval_mode,
        granted,
        access,
        sandbox_profile,
        None,
    )
}

/// Wie [`assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile`],
/// nimmt aber zusätzlich die einmal von der Runtime instanziierte
/// [`HostPermitWiring`] (Permit-Ledger, Sitzungs-Registry und die Sendeseite
/// des Host-Permit-Fragekanals) entgegen (siehe
/// `harw_runtime::assembly::RuntimeAssembly`).
///
/// # Beschreibung
/// Alle drei Werte werden nur an einen [`ShellToolProvider`] gehängt, dessen
/// `sandbox_profile` tatsächlich [`SandboxProfile::Host`] ist —
/// Strict/Cargo/Tmux bleiben unverändert, weil für sie die Sandbox selbst die
/// Grenze ist, nicht der Permit. Die Runtime muss über beide Aufrufstellen
/// (Host- und Nicht-Host-Zweig, siehe `harw-runtime/src/assembly.rs`)
/// **dieselbe** `Arc`-Instanz von Ledger und Registry sowie denselben
/// Sender-Klon durchreichen: ein zweiter, unabhängig instanziierter Ledger
/// hätte keine Kenntnis von den bereits gemerkten Sitzungszustimmungen und
/// würde jede Host-Ausführung erneut ablehnen; ein anderer Sender ließe die
/// Frage nie beim Empfänger ankommen, den die Runtime tatsächlich pollt.
///
/// `None` verhält sich exakt wie
/// [`assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile`]:
/// Host-Ausführung bleibt dann fail-closed, weil kein Ledger konfiguriert ist.
///
/// # Argumente
/// - `profile` ([`RegistryProfile`]): Werkzeug-/Identitätsprofil.
/// - `project` (`&ProjectContext`): bereits erkannter Projektkontext.
/// - `overrides` ([`IdentityOverrides`]): Überschreibungen für den System-Prompt.
/// - `approval_mode` ([`ApprovalModeCell`]): Freigabemodus-Zelle der Politik.
/// - `granted` (`&PermissionSet`): Rechte der Ziel-Sandbox; nur geliehen.
/// - `access` (`Option<`[`AgentDefinitionAccess`]`>`): nur für
///   [`RegistryProfile::AgentStewardship`] relevant; `None` ist der
///   fail-closed Standard.
/// - `sandbox_profile` (`&SandboxProfile`): vertrauenswürdiges, von der
///   Runtime gewähltes Sandbox-Profil; wird an jeden konstruierten
///   [`ShellToolProvider`] weitergegeben.
/// - `host_permits` (`Option<`[`HostPermitWiring`]`>`): der einmal je Lauf
///   instanziierte Permit-Ledger, die zugehörige Sitzungs-Registry und die
///   Sendeseite des Host-Permit-Fragekanals; Eigentum geht über (nur Zeiger
///   und Sender-Handle werden geteilt, nicht der Zustand kopiert). `None`,
///   wenn diese Montage keine Permit-geschützte Host-Ausführung anbietet.
///
/// # Rückgabe
/// `Ok(AssembledRegistry)`; `identity.tools_available` ist exakt die Menge der
/// registrierten Werkzeuge. Für ein Profil ohne `shell.exec`
/// (`ShellExecution`/`Full`/`UiaQuickHelper`/`UiaShellWorker` ausgenommen)
/// bleibt `host_permits` wirkungslos, weil kein [`ShellToolProvider`]
/// entsteht, an den es gehängt werden könnte. `UiaShellWorker` hängt seinen
/// `ShellToolProvider` zudem immer an `SandboxProfile::Host` (unabhängig vom
/// übergebenen `sandbox_profile`), sodass `host_permits` für diese Rolle
/// **immer** greift, sobald es übergeben wird.
///
/// # Fehler
/// - [`RegistryDefaultsError::ContextProviderRegistration`]: der Namensraum des
///   [`ProjectContextProvider`] ist bereits belegt.
///
/// # Nebenläufigkeit
/// Synchron; alle erzeugten Provider sind `Send + Sync`. `host_permits`
/// bringt bereits `Arc`-geteilten, intern gesperrten Zustand sowie einen
/// `Clone + Send + Sync` Sender mit ([`ProcessPermitLedger`]/
/// [`HostPermitSessionRegistry`] kapseln ihre eigene Synchronisierung, der
/// Fragekanal ist ein `tokio::sync::mpsc::UnboundedSender`); diese Funktion
/// selbst hält keine Sperre.
///
/// # Examples
/// ```rust,no_run
/// use std::sync::Arc;
/// use harw_extension_api::approval_mode::ApprovalModeCell;
/// use harw_project_discovery::ProjectContext;
/// use harw_registry_defaults::profile::{
///     HostPermitWiring, IdentityOverrides, RegistryProfile,
///     assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits,
/// };
/// use harw_authority::PermissionSet;
/// use harw_sandbox::{HostPermitSessionRegistry, ProcessPermitLedger, SandboxProfile};
/// use harw_tool_shell::host_permit_prompt_channel;
///
/// # fn demo(project: &ProjectContext) -> Result<(), Box<dyn std::error::Error>> {
/// let ledger = Arc::new(ProcessPermitLedger::default());
/// let registry = Arc::new(HostPermitSessionRegistry::default());
/// let (prompt_sender, _prompt_receiver) = host_permit_prompt_channel();
/// let registry_out = assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
///     RegistryProfile::ShellExecution,
///     project,
///     IdentityOverrides::default(),
///     ApprovalModeCell::default(),
///     &PermissionSet::from_policy([harw_authority::Permission::ExecuteProcess]),
///     None,
///     &SandboxProfile::Strict,
///     Some(HostPermitWiring::new(ledger, registry, prompt_sender)),
/// )?;
/// let _ = registry_out;
/// # Ok(())
/// # }
/// ```
// clippy::too_many_arguments: jeder Parameter ist ein eigenständiger,
// unabhängig optionaler Konstruktionsbaustein (Profil, Projektkontext,
// Identität, Freigabe-Zustand, Rechte, Definitionszugriff, Sandbox-Profil,
// Host-Permit-Verdrahtung); ein Bündel-Struct würde die aussagekräftigen
// Parameternamen an den bestehenden Aufrufstellen (`harw-runtime/src/
// assembly.rs`, `harw-cli/src/sandbox_cmd.rs`) verschleiern, ohne die
// Kopplung zu verringern.
#[allow(clippy::too_many_arguments)]
pub fn assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
    profile: RegistryProfile,
    project: &ProjectContext,
    overrides: IdentityOverrides,
    approval_mode: ApprovalModeCell,
    granted: &PermissionSet,
    access: Option<AgentDefinitionAccess>,
    sandbox_profile: &SandboxProfile,
    host_permits: Option<HostPermitWiring>,
) -> RegistryDefaultsResult<AssembledRegistry> {
    let agent_definition_access = access.unwrap_or_else(|| AgentDefinitionAccess {
        project_agents_dir: Some(project.project_root.join(".harw").join("agents")),
        profile_agents_dir: None,
        mode: DefinitionWriteMode::ProposalOnly,
        ceiling: None,
    });
    // `AgentStewardship` bewirbt/registriert genau die Werkzeuge, die
    // `AgentDefinitionToolProvider` unter `agent_definition_access`
    // tatsächlich trägt (Nachtrag K3) — für jedes andere Profil bleibt es
    // bei der bisherigen, rein statischen Filterung.
    let allowed: Vec<&'static str> = if profile == RegistryProfile::AgentStewardship {
        FS_READ_ONLY_TOOLS
            .iter()
            .chain(agent_definition_tool_names_for_access(Some(&agent_definition_access)).iter())
            .copied()
            .filter(|tool| tool_permission(tool).is_some_and(|needed| granted.contains(needed)))
            .collect()
    } else {
        profile.tool_names_for(granted)
    };
    let providers: Vec<Arc<dyn ToolProvider>> = profile_tool_providers(
        profile,
        agent_definition_access,
        sandbox_profile,
        host_permits.as_ref(),
    )
    .into_iter()
    .filter_map(|provider| restrict_provider(provider, &allowed))
    .collect();

    let advertised_tools: Vec<String> = allowed.iter().map(|name| (*name).to_owned()).collect();

    let IdentityOverrides {
        agent_name,
        role_description,
        extra_context,
        organizational_role,
    } = overrides;

    let mut identity = AgentIdentity::new(
        agent_name.unwrap_or_else(|| "harw".to_owned()),
        project.cwd.display().to_string(),
    )
    .with_role(role_description.unwrap_or_else(|| profile.role_description().to_owned()))
    .with_project_root(project.project_root.display().to_string())
    .with_tools(advertised_tools)
    .with_extra_context(extra_context);
    // Das Regelwerk wird nur angehängt, wenn der Aufrufer eine organisatorische
    // Rolle nennt (Addendum F+G, Agent F-FIX). Früher hing hier
    // bedingungslos das Worker-Regelwerk (`AgentRoleId::Worker`) — das war
    // falsch für Aufrufer, die über diesen Weg auch Nicht-Worker-Knoten
    // zusammenstellen (z. B. `PlanNodeKind::Composite` in
    // `harw-cli/src/job_worker.rs`). `None` hängt bewusst kein Regelwerk an,
    // statt eines geratenen Rollentexts.
    if let Some(role) = organizational_role {
        identity = identity.with_organization_knowledge(
            crate::embedded_agents::builtin_organization_knowledge(role),
        );
    }

    let mut builder = ExtensionRegistryBuilder::default()
        .approval_handler(Arc::new(DefaultApprovalPolicy::new(approval_mode)))
        .instructions_provider(Arc::new(BaselineInstructionsProvider::new(
            identity.clone(),
        )))
        // `context_provider` gibt seit der Trait-Erweiterung ein `Result`:
        // Namensraum und Vertrauensklasse werden an jedem Registrierungsweg
        // geprüft, und ein doppelt beanspruchter Namensraum ist ein Fehler,
        // kein stiller Vorrang.
        .context_provider(Arc::new(ProjectContextProvider::new(project.clone())))?;
    for provider in providers {
        builder = builder.tool_provider(provider);
    }

    Ok(AssembledRegistry {
        registry: builder.build(),
        project: project.clone(),
        identity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sammelt die tatsächlich registrierten Tool-Namen einer Registry.
    fn registered_names(assembled: &AssembledRegistry) -> Vec<String> {
        assembled
            .registry
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|spec| spec.name().to_owned())
            .collect()
    }

    fn assemble(profile: RegistryProfile) -> AssembledRegistry {
        let cwd = std::env::current_dir().expect("cwd");
        assemble_registry(profile, cwd, IdentityOverrides::default()).expect("assemble")
    }

    #[test]
    fn test_registered_tool_names_matches_actually_registered_tools_for_every_profile() {
        for profile in RegistryProfile::ALL {
            // `AgentStewardship` ist die einzige Ausnahme (Nachtrag K3): ohne
            // `AgentDefinitionAccess` (der Standardpfad über `assemble()`
            // hier) registriert `AgentDefinitionToolProvider` fail-closed nur
            // die beiden lesenden Werkzeuge, während `registered_tool_names()`
            // für dieses eine Profil bewusst die maximale Vertrags-Obermenge
            // bleibt (siehe deren Dokumentation und
            // `tests/tool_admission_coverage.rs`). Siehe die dedizierten
            // Tests unten für den tatsächlichen Laufzeitzustand mit und ohne
            // Decke.
            if *profile == RegistryProfile::AgentStewardship {
                continue;
            }
            // `UiaQuickHelper` ist eine zweite, ebenso dokumentierte Ausnahme
            // (Nutzerentscheidung): `browser.open` braucht laut Moduldokumentation
            // ("Browser nur mit Grant") weiterhin einen tatsächlichen
            // `harw_tool_browser::BrowserOpenGrant` über `browser_tool_provider`
            // (Feature `browser`, standardmäßig **aus**), den `profile_tool_providers`
            // für dieses Profil (noch) nicht baut. `registered_tool_names()`
            // bleibt bewusst die vollständige, statisch beworbene Vertrags-
            // Obermenge (siehe `tests/tool_admission_coverage.rs`); der
            // dedizierte Test unten prüft den tatsächlichen Laufzeitzustand.
            if *profile == RegistryProfile::UiaQuickHelper {
                continue;
            }
            let assembled = assemble(*profile);
            let expected: Vec<String> = profile
                .registered_tool_names()
                .iter()
                .map(|name| (*name).to_owned())
                .collect();
            assert_eq!(
                registered_names(&assembled),
                expected,
                "{profile:?}: die statische Liste muss der Registry entsprechen"
            );
        }
    }

    /// Nutzerentscheidung: `RegistryProfile::UiaQuickHelper::registered_tool_names()`
    /// bewirbt `browser.open` statisch (siehe
    /// [`UIA_QUICK_HELPER_BROWSER_TOOLS`]), aber `profile_tool_providers` baut
    /// dafür (noch) keinen tatsächlichen Browser-Provider — der bräuchte einen
    /// `harw_tool_browser::BrowserOpenGrant`, den nur `browser_tool_provider`
    /// (Feature `browser`, standardmäßig aus) liefert. Ohne diese Erweiterung
    /// bleibt die tatsächlich montierte Registry also exakt die vorherige
    /// Werkzeugmenge ohne `browser.open` — dieser Test macht das explizit,
    /// statt es stillschweigend an der Ausnahme oben hängen zu lassen.
    #[test]
    fn test_uia_quick_helper_does_not_yet_build_a_runtime_browser_provider() {
        let assembled = assemble(RegistryProfile::UiaQuickHelper);
        let registered = registered_names(&assembled);
        assert!(
            !registered.iter().any(|tool| tool == "browser.open"),
            "profile_tool_providers baut noch keinen Browser-Provider für \
             UiaQuickHelper; sobald ein Grant durchgereicht wird, muss dieser \
             Test zusammen mit der Ausnahme oben entfernt werden"
        );
        assert!(
            RegistryProfile::UiaQuickHelper
                .registered_tool_names()
                .contains(&"browser.open"),
            "die statische Liste muss browser.open weiterhin bewerben"
        );
    }

    /// Nachtrag K3: ohne eine gesetzte [`AgentDefinitionAccess`] registriert
    /// `AgentDefinitionToolProvider` fail-closed nur `agents.validate`/
    /// `agents.list_proposals` — unabhängig davon, dass
    /// `RegistryProfile::AgentStewardship::registered_tool_names()` weiterhin
    /// die volle Vertrags-Obermenge zurückgibt (siehe der Test oben).
    #[test]
    fn test_agent_stewardship_registers_only_read_and_list_without_access() {
        let assembled = assemble(RegistryProfile::AgentStewardship);
        let mut expected: Vec<String> = FS_READ_ONLY_TOOLS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        expected.push("agents.validate".to_owned());
        expected.push("agents.list_proposals".to_owned());
        assert_eq!(registered_names(&assembled), expected);
        assert_eq!(assembled.identity.tools_available, expected);
    }

    /// Nachtrag K3: mit einer gesetzten Decke und `DefinitionWriteMode::Commit`
    /// registriert derselbe Provider die volle Vertrags-Obermenge — genau die
    /// Werkzeugmenge, die `agent-steward.toml` admittiert (siehe
    /// `tests/tool_admission_coverage.rs`).
    #[test]
    fn test_agent_stewardship_registers_all_six_tools_with_a_commit_ceiling() {
        use crate::agent_definition_tools::DefinitionAuthorCeiling;
        use harw_agent_dsl::roles::AgentRoleId;

        let cwd = std::env::current_dir().expect("cwd");
        let project =
            discover_project(&cwd, &DiscoveryConfig::default()).expect("Discovery im Workspace");
        let access = AgentDefinitionAccess {
            project_agents_dir: Some(project.project_root.join(".harw").join("agents")),
            profile_agents_dir: None,
            mode: DefinitionWriteMode::Commit,
            ceiling: Some(DefinitionAuthorCeiling {
                role: AgentRoleId::UserInterface,
                tools: std::collections::BTreeSet::new(),
                permissions: PermissionSet::empty(),
                max_depth: 0,
                budget_tokens: 0,
                effort_cap: None,
            }),
        };
        let assembled = assemble_registry_for_project_with_definition_access(
            RegistryProfile::AgentStewardship,
            &project,
            IdentityOverrides::default(),
            ApprovalModeCell::default(),
            Some(access),
        )
        .expect("assemble");

        let mut expected: Vec<String> = FS_READ_ONLY_TOOLS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        expected.extend(AGENT_DEFINITION_TOOLS.iter().map(|name| (*name).to_owned()));
        assert_eq!(registered_names(&assembled), expected);
        assert_eq!(assembled.identity.tools_available, expected);
    }

    #[test]
    fn test_read_only_explore_exposes_exact_tool_set() {
        let assembled = assemble(RegistryProfile::ReadOnlyExplore);
        assert_eq!(
            registered_names(&assembled),
            vec![
                "fs.read",
                "fs.list",
                "fs.search",
                "fs.glob",
                "fs.grep",
                "deps.graph",
                "deps.locked",
                "deps.source_read",
                "deps.source_search",
                "deps.source_list",
            ]
        );
    }

    /// A5: Die einzige Rolle mit Netz registriert **nur** `web.*` — kein
    /// `fs.*`, kein `deps.*`, also nichts, womit sie Workspace-Daten lesen und
    /// über `web.fetch` hinaustragen könnte.
    #[test]
    fn test_research_profile_registers_only_the_web_tools() {
        let assembled = assemble(RegistryProfile::Research);
        let names = registered_names(&assembled);
        let expected: Vec<String> = WEB_TOOLS.iter().map(|tool| (*tool).to_owned()).collect();
        assert_eq!(names, expected);
        assert!(!names.iter().any(|name| name.starts_with("fs.")));
        assert!(!names.iter().any(|name| name.starts_with("deps.")));
        assert_eq!(assembled.identity.tools_available, expected);
    }

    #[test]
    fn test_no_profile_registers_browser_tools_without_a_grant() {
        for profile in RegistryProfile::ALL {
            let names = profile.registered_tool_names();
            for tool in BROWSER_TOOLS {
                // Einzige dokumentierte Ausnahme (Nutzerentscheidung): nur
                // `UiaQuickHelper` registriert `browser.open` — statisch, ohne
                // Laufzeit-Grant. Die übrigen sechs Browser-Werkzeuge bleiben
                // für jedes Profil, `UiaQuickHelper` eingeschlossen, verboten.
                let is_the_documented_exception =
                    *profile == RegistryProfile::UiaQuickHelper && *tool == "browser.open";
                assert!(
                    is_the_documented_exception || !names.contains(tool),
                    "{profile:?} registriert {tool} ohne Grant"
                );
            }
        }
    }

    #[test]
    fn test_deps_tool_lists_partition_the_provider_order() {
        let joined: Vec<&str> = DEPS_WORKSPACE_TOOLS
            .iter()
            .chain(DEPS_SOURCE_TOOLS.iter())
            .copied()
            .collect();
        assert_eq!(joined, DEPS_TOOLS);
        assert_eq!(harw_tool_deps::DepsToolProvider::TOOL_NAMES, DEPS_TOOLS);
    }

    #[test]
    fn test_required_permissions_per_profile() {
        use harw_authority::Permission;

        let set = |permissions: &[Permission]| PermissionSet::from_policy(permissions.to_vec());
        assert_eq!(
            RegistryProfile::Full.required_permissions(),
            set(&[
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
                Permission::ExecuteProcess
            ])
        );
        assert_eq!(
            RegistryProfile::ReadOnlyExplore.required_permissions(),
            set(&[Permission::ReadWorkspace, Permission::ReadCargoRegistry])
        );
        assert_eq!(
            RegistryProfile::Planning.required_permissions(),
            set(&[Permission::ReadWorkspace, Permission::ReadCargoRegistry])
        );
        assert_eq!(
            RegistryProfile::Research.required_permissions(),
            set(&[Permission::NetworkAccess])
        );
        assert_eq!(
            RegistryProfile::NoTools.required_permissions(),
            PermissionSet::empty()
        );
    }

    #[test]
    fn test_tool_names_for_hides_registry_tools_without_read_cargo_registry() {
        use harw_authority::Permission;

        let workspace_only = PermissionSet::from_policy([Permission::ReadWorkspace]);
        let tools = RegistryProfile::ReadOnlyExplore.tool_names_for(&workspace_only);
        assert_eq!(
            tools,
            vec![
                "fs.read",
                "fs.list",
                "fs.search",
                "fs.glob",
                "fs.grep",
                "deps.graph",
                "deps.locked"
            ]
        );
        for profile in RegistryProfile::ALL {
            assert_eq!(
                profile.tool_names_for(&profile.required_permissions()),
                profile.registered_tool_names(),
                "{profile:?}: unter den eigenen Rechten fällt nichts heraus"
            );
            assert!(
                profile.tool_names_for(&PermissionSet::empty()).is_empty(),
                "{profile:?}: ohne Rechte bleibt nichts"
            );
        }
    }

    #[test]
    fn test_assemble_registry_for_sandbox_registers_and_advertises_only_granted_tools() {
        use harw_authority::Permission;

        let cwd = std::env::current_dir().expect("cwd");
        let project =
            discover_project(&cwd, &DiscoveryConfig::default()).expect("Discovery im Workspace");
        let granted = PermissionSet::from_policy([Permission::ReadWorkspace]);
        let assembled = assemble_registry_for_sandbox(
            RegistryProfile::ReadOnlyExplore,
            &project,
            IdentityOverrides::default(),
            ApprovalModeCell::default(),
            &granted,
        )
        .expect("assemble");

        let expected: Vec<String> = RegistryProfile::ReadOnlyExplore
            .tool_names_for(&granted)
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(registered_names(&assembled), expected);
        assert_eq!(assembled.identity.tools_available, expected);

        let deps_source = ToolName::new("deps.source_read");
        for provider in assembled.registry.tool_providers() {
            assert!(
                provider.executor(&deps_source).is_none(),
                "deps.source_read darf ohne ReadCargoRegistry nicht per Namensraten laufen"
            );
        }

        // Research ohne NetworkAccess: kein einziger Provider bleibt übrig.
        let research = assemble_registry_for_sandbox(
            RegistryProfile::Research,
            &project,
            IdentityOverrides::default(),
            ApprovalModeCell::default(),
            &granted,
        )
        .expect("assemble");
        assert!(research.registry.tool_providers().is_empty());
        assert!(research.identity.tools_available.is_empty());
    }

    #[test]
    fn test_planning_profile_registers_read_only_tools_and_does_not_advertise_plan_or_goal() {
        let assembled = assemble(RegistryProfile::Planning);
        // Registriert werden die read-only Provider von `ReadOnlyExplore`
        // plus `lens.ask` — der einzige Grund, warum `Planning` sich
        // überhaupt vom read-only Kern unterscheidet (siehe `LENS_TOOLS`).
        let mut expected: Vec<String> = RegistryProfile::ReadOnlyExplore
            .registered_tool_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        expected.push("lens.ask".to_owned());
        assert_eq!(registered_names(&assembled), expected);
        // … und beworben wird genau das Registrierte: `plan`/`goal` sind
        // Composition-Root-Operationen ohne Executor im Kind (W1-05).
        assert_eq!(assembled.identity.tools_available, expected);
        assert!(
            !assembled
                .identity
                .tools_available
                .contains(&"plan".to_owned())
        );
        assert!(
            !assembled
                .identity
                .tools_available
                .contains(&"goal".to_owned())
        );
    }

    #[test]
    fn test_only_planner_profile_registers_lens_ask() {
        // `LensToolProvider` erscheint in der zusammengestellten Registry —
        // der Beleg, dass Lens jetzt einen echten Konsumenten hat.
        let planning = assemble(RegistryProfile::Planning);
        assert!(registered_names(&planning).contains(&"lens.ask".to_owned()));
        assert!(
            planning
                .identity
                .tools_available
                .contains(&"lens.ask".to_owned())
        );

        // Alle anderen Profile — insbesondere `ReadOnlyExplore`, das sich
        // `explorer`, `researcher-deps` und `analyst` teilen — bekommen
        // `lens.ask` ausdrücklich nicht.
        for profile in RegistryProfile::ALL
            .iter()
            .filter(|p| **p != RegistryProfile::Planning)
        {
            let assembled = assemble(*profile);
            assert!(
                !registered_names(&assembled).contains(&"lens.ask".to_owned()),
                "{profile:?} darf lens.ask nicht registrieren"
            );
            assert!(
                !assembled
                    .identity
                    .tools_available
                    .contains(&"lens.ask".to_owned()),
                "{profile:?} darf lens.ask nicht bewerben"
            );
        }
    }

    #[test]
    fn test_read_only_profiles_never_expose_write_or_shell_tools() {
        for profile in RegistryProfile::ALL.iter().filter(|p| p.is_read_only()) {
            let assembled = assemble(*profile);
            let names = registered_names(&assembled);
            for forbidden in ["fs.write", "shell.exec"] {
                assert!(
                    !names.contains(&forbidden.to_owned()),
                    "{profile:?} darf {forbidden} nicht registrieren"
                );
                assert!(
                    !assembled
                        .identity
                        .tools_available
                        .contains(&forbidden.to_owned()),
                    "{profile:?} darf {forbidden} nicht bewerben"
                );
            }
        }
    }

    #[test]
    fn test_identity_advertises_exactly_the_profile_tool_names() {
        for profile in RegistryProfile::ALL {
            // `AgentStewardship` ist auch hier die einzige Ausnahme (Nachtrag
            // K3, wie bei `test_registered_tool_names_matches_actually_
            // registered_tools_for_every_profile` oben): der beworbene Satz
            // folgt dem access-abhängigen Laufzeitsatz aus
            // `agent_definition_tool_names_for_access` — ohne `AgentDefinition
            // Access` (der Standardpfad über `assemble()` hier) ist das fail-
            // closed nur `agents.validate`/`agents.list_proposals`, während
            // `RegistryProfile::tool_names()` bewusst die maximale
            // Commit-Modus-Obermenge bleibt (siehe deren Dokumentation). Der
            // dedizierte Test `test_agent_stewardship_registers_only_read_
            // and_list_without_access` prüft diesen Fall bereits exakt.
            if *profile == RegistryProfile::AgentStewardship {
                continue;
            }
            // `UiaQuickHelper` ist auch hier die zweite Ausnahme
            // (Nutzerentscheidung, siehe
            // `test_uia_quick_helper_does_not_yet_build_a_runtime_browser_provider`
            // oben): `tool_names()` bewirbt `browser.open` statisch, aber ohne
            // einen `harw_tool_browser::BrowserOpenGrant` (Feature `browser`,
            // standardmäßig aus) baut `profile_tool_providers` dafür keinen
            // Provider — der tatsächlich beworbene Prompt-Satz bleibt also
            // ohne `browser.open`.
            if *profile == RegistryProfile::UiaQuickHelper {
                continue;
            }
            let assembled = assemble(*profile);
            let expected: Vec<String> = profile
                .tool_names()
                .iter()
                .map(|name| (*name).to_owned())
                .collect();
            assert_eq!(
                assembled.identity.tools_available, expected,
                "{profile:?}: der Prompt darf nur Profil-Werkzeuge bewerben"
            );
        }
    }

    #[test]
    fn test_identity_uses_profile_role_description_by_default() {
        for profile in RegistryProfile::ALL {
            let assembled = assemble(*profile);
            assert_eq!(
                assembled.identity.role_description,
                profile.role_description()
            );
        }
    }

    #[test]
    fn test_identity_overrides_replace_name_role_and_context() {
        let cwd = std::env::current_dir().expect("cwd");
        let assembled = assemble_registry(
            RegistryProfile::ReadOnlyExplore,
            cwd,
            IdentityOverrides {
                agent_name: Some("explorer-3".to_owned()),
                role_description: Some("focused explorer".to_owned()),
                extra_context: vec!["Antworte nur mit JSON.".to_owned()],
                organizational_role: None,
            },
        )
        .expect("assemble");

        assert_eq!(assembled.identity.agent_name, "explorer-3");
        assert_eq!(assembled.identity.role_description, "focused explorer");
        assert_eq!(
            assembled.identity.extra_context,
            vec!["Antworte nur mit JSON."]
        );
    }

    /// Legt ein leeres Projektverzeichnis unter `std::env::temp_dir()` an und
    /// setzt einen `Cargo.toml`-Marker hinein, damit `discover_project` genau
    /// dieses Verzeichnis als Projektwurzel erkennt.
    ///
    /// Der Name ist über Prozess-ID und Nanosekunden eindeutig — kein
    /// gemeinsamer Zähler, kein anderer geteilter Zustand, damit parallel
    /// laufende Tests einander nicht sehen.
    fn make_temp_project(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("Systemzeit liegt vor der Unix-Epoche")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-registry-defaults-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("Projektverzeichnis anlegen");
        std::fs::write(root.join("Cargo.toml"), b"[package]\nname = \"tmp\"\n")
            .expect("Projektmarker schreiben");
        root
    }

    /// Der Kern von G-071: Zwei Montagen über demselben [`ProjectContext`]
    /// lösen keine zweite Projekterkennung aus.
    ///
    /// Bewiesen wird das nicht über einen Zähler, sondern über die
    /// Unmöglichkeit: Nach der einen Discovery wird das Projektverzeichnis
    /// gelöscht. Ab da scheitert `discover_project` (der Test prüft das
    /// ausdrücklich) — beide Montagen gelingen trotzdem und liefern denselben
    /// Kontext. Eine verborgene Re-Discovery könnte das nicht.
    #[test]
    fn test_assemble_registry_for_project_never_runs_discovery_again() {
        let root = make_temp_project("no-rediscovery");

        let project =
            discover_project(&root, &DiscoveryConfig::default()).expect("Discovery im Tempdir");
        assert_eq!(
            project.project_root, project.cwd,
            "der Marker liegt im Wurzelverzeichnis selbst"
        );

        std::fs::remove_dir_all(&root).expect("Projektverzeichnis entfernen");
        assert!(
            discover_project(&root, &DiscoveryConfig::default()).is_err(),
            "nach dem Löschen muss jede erneute Discovery scheitern — sonst \
             beweist dieser Test nichts"
        );

        let mode = ApprovalModeCell::new(harw_extension_api::ApprovalMode::AlwaysAsk);
        let first = assemble_registry_for_project(
            RegistryProfile::ReadOnlyExplore,
            &project,
            IdentityOverrides::default(),
            mode.clone(),
        )
        .expect("erste Montage kommt ohne Discovery aus");
        let second = assemble_registry_for_project(
            RegistryProfile::ReadOnlyExplore,
            &project,
            IdentityOverrides::default(),
            mode.clone(),
        )
        .expect("zweite Montage kommt ohne Discovery aus");

        assert_eq!(first.project.project_root, project.project_root);
        assert_eq!(second.project.project_root, project.project_root);
        assert_eq!(first.identity.cwd, second.identity.cwd);
        assert_eq!(registered_names(&first), registered_names(&second));
        assert_eq!(first.registry.approval_handlers().len(), 1);
        assert_eq!(second.registry.approval_handlers().len(), 1);
    }

    #[test]
    fn test_restricted_provider_hides_filtered_executor() {
        let provider =
            RestrictedToolProvider::new(Arc::new(FsToolProvider::default()), FS_READ_ONLY_TOOLS);
        assert_eq!(provider.tools().len(), FS_READ_ONLY_TOOLS.len());
        assert!(provider.executor(&ToolName::new("fs.read")).is_some());
        assert!(
            provider.executor(&ToolName::new("fs.write")).is_none(),
            "gefiltertes Werkzeug darf nicht per Namensraten erreichbar sein"
        );
        assert!(provider.executor(&ToolName::new("unbekannt")).is_none());
        assert!(!provider.parallel_safe(&ToolName::new("fs.write")));
        assert!(provider.parallel_safe(&ToolName::new("fs.read")));
    }

    #[test]
    fn test_profile_for_role_maps_every_builtin_role() {
        assert_eq!(
            profile_for_role(role_names::EXPLORER),
            Some(RegistryProfile::ReadOnlyExplore)
        );
        assert_eq!(
            profile_for_role(role_names::RESEARCHER_DEPS),
            Some(RegistryProfile::ReadOnlyExplore)
        );
        assert_eq!(
            profile_for_role(role_names::RESEARCHER_WEB),
            Some(RegistryProfile::Research)
        );
        assert_eq!(
            profile_for_role(role_names::PLANNER),
            Some(RegistryProfile::Planning)
        );
        assert_eq!(
            profile_for_role(role_names::ANALYST),
            Some(RegistryProfile::ReadOnlyExplore)
        );
        assert_eq!(
            profile_for_role(role_names::EXECUTOR),
            Some(RegistryProfile::ShellExecution),
            "executor ist die einzige eingebaute Rolle mit der schmalen \
             Prozessoberfläche"
        );
        assert_eq!(
            profile_for_role(role_names::UIA_SHELL_WORKER),
            Some(RegistryProfile::UiaShellWorker),
            "uia-shell-worker ist die Host-Shell-Spezialisierung der UIA"
        );
        for role in [
            role_names::SECURITY_EGRESS_TRIAGE,
            role_names::SECURITY_BASELINE_TRIAGE,
            role_names::SECURITY_STRUCTURE_TRIAGE,
            role_names::SECURITY_ENDPOINT_TRIAGE,
        ] {
            assert_eq!(
                profile_for_role(role),
                Some(RegistryProfile::NoTools),
                "Triage-Rolle {role} muss NoTools bekommen"
            );
        }
        for role in role_names::ALL {
            assert!(profile_for_role(role).is_some(), "unbekannt: {role}");
        }
    }

    #[test]
    fn test_profile_for_unknown_role_is_none_not_full() {
        assert_eq!(profile_for_role("coder"), None);
        assert_eq!(profile_for_role(""), None);
    }

    /// `uia-shell-worker` bekommt `RegistryProfile::UiaShellWorker`, und dessen
    /// beworbene/registrierte Werkzeugoberfläche ist exakt `shell.exec` plus
    /// die fünf lesenden `fs.*`-Werkzeuge — nicht mehr, nicht weniger.
    #[test]
    fn test_uia_shell_worker_profile_and_tool_surface() {
        use std::collections::BTreeSet;

        assert_eq!(
            profile_for_role(role_names::UIA_SHELL_WORKER),
            Some(RegistryProfile::UiaShellWorker)
        );
        let advertised: BTreeSet<&str> =
            RegistryProfile::UiaShellWorker.tool_names().into_iter().collect();
        let expected: BTreeSet<&str> = [
            "shell.exec",
            "fs.read",
            "fs.list",
            "fs.search",
            "fs.glob",
            "fs.grep",
        ]
        .into_iter()
        .collect();
        assert_eq!(advertised, expected);
        assert!(!RegistryProfile::UiaShellWorker.is_read_only());
    }

    /// Ein unbekannter Rollenname darf nie auf einen Prozess- oder
    /// Schreibzugriff zurückfallen. Das schmale Prozessprofil ist ausschließlich
    /// für den dokumentierten Executor erreichbar.
    #[test]
    fn test_privileged_profiles_are_never_unnamed_fallbacks() {
        assert!(!RegistryProfile::Full.is_read_only());
        assert!(!RegistryProfile::ShellExecution.is_read_only());
        assert!(!RegistryProfile::MemoryStewardship.is_read_only());
        assert!(!RegistryProfile::UiaQuickHelper.is_read_only());
        assert!(!RegistryProfile::AgentStewardship.is_read_only());
        assert!(!RegistryProfile::UiaWriter.is_read_only());
        assert!(!RegistryProfile::UiaShellWorker.is_read_only());
        for profile in RegistryProfile::ALL.iter().filter(|profile| {
            !matches!(
                **profile,
                RegistryProfile::Full
                    | RegistryProfile::ShellExecution
                    | RegistryProfile::MemoryStewardship
                    | RegistryProfile::UiaQuickHelper
                    | RegistryProfile::AgentStewardship
                    | RegistryProfile::UiaWriter
                    | RegistryProfile::UiaShellWorker
            )
        }) {
            assert!(profile.is_read_only(), "{profile:?} muss read-only sein");
        }
        for role in role_names::ALL {
            if role == &role_names::EXECUTOR {
                assert_eq!(
                    profile_for_role(role),
                    Some(RegistryProfile::ShellExecution)
                );
            } else if role == &role_names::MEMORY_STEWARD {
                assert_eq!(
                    profile_for_role(role),
                    Some(RegistryProfile::MemoryStewardship)
                );
            } else if role == &role_names::UIA_WORKER {
                // Addendum I: `uia-worker` ist die einzige Rolle mit
                // `RegistryProfile::UiaQuickHelper` — nicht mehr `Research`.
                assert_eq!(
                    profile_for_role(role),
                    Some(RegistryProfile::UiaQuickHelper)
                );
            } else if role == &role_names::AGENT_STEWARD {
                // Addendum K: `agent-steward` ist die einzige Rolle mit
                // `RegistryProfile::AgentStewardship`.
                assert_eq!(
                    profile_for_role(role),
                    Some(RegistryProfile::AgentStewardship)
                );
            } else {
                assert_ne!(profile_for_role(role), Some(RegistryProfile::Full));
                assert_ne!(
                    profile_for_role(role),
                    Some(RegistryProfile::ShellExecution)
                );
                assert_ne!(
                    profile_for_role(role),
                    Some(RegistryProfile::MemoryStewardship)
                );
                assert_ne!(
                    profile_for_role(role),
                    Some(RegistryProfile::UiaQuickHelper)
                );
                assert_ne!(
                    profile_for_role(role),
                    Some(RegistryProfile::AgentStewardship)
                );
            }
        }
        assert_eq!(profile_for_role("unbekannt"), None);
    }

    #[test]
    fn test_no_tools_profile_registers_and_advertises_nothing() {
        // Der eigentliche Zweck von `NoTools` (siehe Begründung bei
        // `RegistryProfile::NoTools`): eine Rolle mit `[tools].admitted = []`
        // darf kein Werkzeug im System-Prompt-Inventar sehen, das sie nicht
        // aufrufen darf.
        let assembled = assemble(RegistryProfile::NoTools);
        assert!(registered_names(&assembled).is_empty());
        assert!(assembled.identity.tools_available.is_empty());
        assert!(RegistryProfile::NoTools.is_read_only());
    }

    /// Fängt genau den Befund dieses Knotens ab, falls er sich wiederholt:
    /// eine gefundene, aber nicht in [`role_names::ALL`] eingetragene
    /// Rollendatei. Der Vergleichswert kommt bewusst aus einer *anderen*
    /// Quelle als `ALL` selbst — der Verzeichnis-Sammlung
    /// [`crate::embedded_agents::builtin_agent_toml`] — statt aus einer
    /// daneben stehenden Zahl (siehe die Kritik an den vier Fundstellen in
    /// `agents/families/security/security.toml`, die genau das getan haben).
    ///
    /// `context-steward`, `intel-scout`, `cargo-worker`, `host-process-worker`,
    /// `sandbox-shell-worker` und `tmux-inspector-worker` werden von derselben
    /// Verzeichnis-Sammlung ebenfalls gefunden, sind aber bewusst nicht in
    /// `ALL` (siehe Moduldokumentation von [`role_names`]) — eigene, noch
    /// offene Befunde außerhalb dieses Knotens. Sie sind deshalb dokumentierte,
    /// keine stillschweigenden Ausnahmen.
    #[test]
    fn test_role_names_all_matches_discovered_role_files_minus_pending_exclusions() {
        use std::collections::BTreeSet;

        /// Rollendateien, die die Verzeichnis-Sammlung bereits findet, deren
        /// Aufnahme in `role_names::ALL` aber ein eigener, noch offener
        /// Befund ist (siehe Moduldokumentation von `role_names`).
        /// `memory-steward` (Memory v3, §5.3) stand hier bis zur Einführung
        /// von `RegistryProfile::MemoryStewardship`: die Konsolidierung
        /// braucht genau `fs.*` **ohne** `shell.exec`, und ein solches
        /// Profil gab es vorher nicht. Jetzt hat sie ein Profil und einen
        /// Eintrag in `role_names::ALL` — keine Ausnahme mehr.
        const PENDING_EXCLUSIONS: &[&str] = &[
            "context-steward",
            "intel-scout",
            "cargo-worker",
            "host-process-worker",
            "sandbox-shell-worker",
            "tmux-inspector-worker",
        ];

        let discovered: BTreeSet<&str> = crate::embedded_agents::builtin_agent_toml()
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| *name != crate::embedded_agents::WORKER_BASE_NAME)
            .filter(|name| !PENDING_EXCLUSIONS.contains(name))
            .collect();

        let listed: BTreeSet<&str> = role_names::ALL.iter().copied().collect();

        assert_eq!(
            discovered, listed,
            "role_names::ALL ist gegenüber den tatsächlich gefundenen \
             Rollendateien veraltet (abzüglich der dokumentierten Ausnahmen \
             {PENDING_EXCLUSIONS:?}) — eine neue Rollendatei unter agents/ \
             wird zwar gefunden, aber ohne einen Eintrag hier nie gesenkt"
        );
    }

    // ── `assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits` ──
    //
    // Diese Tests decken die eigentliche Ergänzung dieses Auftrags ab: die
    // Funktion muss `host_permits` tatsächlich an den gebauten
    // `ShellToolProvider` durchreichen (statt es nur entgegenzunehmen), und
    // zwar ausschließlich für `SandboxProfile::Host`. Sie führen den echten
    // `shell.exec`-Executor aus (wie `harw-tool-shell::exec::tests`), weil nur
    // die Ausführung selbst beweist, dass der Ledger tatsächlich erreicht
    // wird — ein Blick auf `registered_tool_names()` allein würde die
    // Verdrahtung nicht zeigen.

    mod permits_wiring {
        use super::*;
        use harw_authority::{Permission, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
        use harw_tools::{ToolCall, ToolExecutionContext, ToolOutput};
        use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
        use std::time::Duration;

        /// Baut eine eigenständige Workspace-Sandbox (unabhängig vom
        /// `ProjectContext`, den `discover_project` liefert) mit genau dem
        /// übergebenen Rechtesatz.
        fn make_sandbox(dir: &std::path::Path, permissions: Vec<Permission>) -> SandboxSpec {
            let ws_subdir = dir.join("project");
            std::fs::create_dir_all(&ws_subdir).expect("project subdir must be created");
            let registry = WorkspaceRegistry::build(
                dir,
                [WorkspaceRegistration {
                    tenant: TenantId::from_str("test-tenant"),
                    workspace: WorkspaceId::from_str("project"),
                    root: ws_subdir,
                }],
            )
            .expect("registry build must succeed");
            let binding = registry
                .resolve(
                    &TenantId::from_str("test-tenant"),
                    &WorkspaceId::from_str("project"),
                )
                .expect("resolve must succeed");
            SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions))
        }

        fn make_ctx(sandbox: SandboxSpec) -> ToolExecutionContext {
            ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox)
        }

        fn make_call(command: &str) -> ToolCall {
            ToolCall {
                id: ToolCallId::new(),
                name: ToolName::new("shell.exec"),
                arguments: serde_json::json!({ "command": command }),
            }
        }

        /// Baut eine [`HostPermitWiring`] aus Ledger und Registry, deren
        /// Fragekanal-Empfänger sofort verworfen wird: `sender.send(..)`
        /// scheitert dadurch synchron (der Kanal ist bereits geschlossen),
        /// statt bis `host_permit_timeout` auf eine nie kommende Antwort zu
        /// warten. Repliziert exakt das alte Verhalten „kein Fragekanal
        /// angehängt" (fail-closed, `requires local UI approval`) für Tests,
        /// die keine tatsächliche Rückfrage beantworten wollen.
        fn wiring_with_closed_channel(
            ledger: Arc<ProcessPermitLedger>,
            registry: Arc<HostPermitSessionRegistry>,
        ) -> HostPermitWiring {
            let (sender, receiver) = harw_tool_shell::host_permit_prompt_channel();
            drop(receiver);
            HostPermitWiring::new(ledger, registry, sender)
        }

        /// Baut eine Registry über `profile_tool_providers` (über den
        /// öffentlichen Einstiegspunkt) mit `sandbox_profile` und
        /// `host_permits` und liefert deren `shell.exec`-Executor.
        fn shell_executor_for(
            sandbox_profile: &SandboxProfile,
            host_permits: Option<HostPermitWiring>,
            project_root: &std::path::Path,
        ) -> Arc<dyn ToolExecutor> {
            let project = discover_project(project_root, &DiscoveryConfig::default())
                .expect("Discovery im Tempdir");
            let granted = PermissionSet::from_policy([Permission::ExecuteProcess]);
            let assembled = assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
                RegistryProfile::ShellExecution,
                &project,
                IdentityOverrides::default(),
                ApprovalModeCell::default(),
                &granted,
                None,
                sandbox_profile,
                host_permits,
            )
            .expect("assemble must succeed");
            assembled
                .registry
                .tool_providers()
                .iter()
                .find_map(|provider| provider.executor(&ToolName::new("shell.exec")))
                .expect("shell.exec executor must be registered for ShellExecution")
        }

        /// Wie [`shell_executor_for`], aber für
        /// `RegistryProfile::UiaShellWorker` — dessen Shell-Provider hängt sich
        /// ausdrücklich immer an `SandboxProfile::Host`, unabhängig vom
        /// übergebenen `sandbox_profile` (siehe die Begründung bei
        /// `RegistryProfile::UiaShellWorker`). Das übergebene
        /// `sandbox_profile` bleibt hier deshalb bewusst `SandboxProfile::Strict`
        /// — der Test beweist damit gerade, dass der Zweig trotzdem den
        /// Host-Provider baut.
        fn uia_shell_worker_executor_for(
            host_permits: Option<HostPermitWiring>,
            project_root: &std::path::Path,
        ) -> Arc<dyn ToolExecutor> {
            let project = discover_project(project_root, &DiscoveryConfig::default())
                .expect("Discovery im Tempdir");
            let granted = PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::ExecuteProcess,
            ]);
            let assembled = assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
                RegistryProfile::UiaShellWorker,
                &project,
                IdentityOverrides::default(),
                ApprovalModeCell::default(),
                &granted,
                None,
                &SandboxProfile::Strict,
                host_permits,
            )
            .expect("assemble must succeed");
            assembled
                .registry
                .tool_providers()
                .iter()
                .find_map(|provider| provider.executor(&ToolName::new("shell.exec")))
                .expect("shell.exec executor must be registered for UiaShellWorker")
        }

        /// `RegistryProfile::UiaShellWorker` trägt `SandboxProfile::Host` immer
        /// — auch wenn die aufrufende Montage `SandboxProfile::Strict`
        /// übergibt — und lehnt deshalb ohne Ledger jede Ausführung ab
        /// (fail-closed).
        #[tokio::test]
        async fn test_uia_shell_worker_always_carries_host_profile_and_denies_without_ledger() {
            let project_root = make_temp_project("uia-shell-worker-no-ledger");
            let executor = uia_shell_worker_executor_for(None, &project_root);
            let tmp = tempfile::tempdir().expect("tempdir");
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess]));
            let call = make_call("echo must_not_run");

            let output = executor
                .execute(&ctx, &call)
                .await
                .expect("execute must not return Err");

            match output {
                ToolOutput::Error { message } => {
                    assert!(
                        message.contains("host execution requires a process permit"),
                        "uia-shell-worker must fail closed without a ledger even though \
                         sandbox_profile passed to assembly was Strict, got: {message:?}"
                    );
                }
                other => panic!("expected Error output without permits, got: {other:?}"),
            }
        }

        /// Mit Ledger, aber ohne Sitzungszustimmung bleibt `uia-shell-worker`
        /// ebenfalls fail-closed.
        #[tokio::test]
        async fn test_uia_shell_worker_denies_without_session_approval() {
            let project_root = make_temp_project("uia-shell-worker-no-approval");
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let executor = uia_shell_worker_executor_for(
                Some(wiring_with_closed_channel(ledger, registry)),
                &project_root,
            );
            let tmp = tempfile::tempdir().expect("tempdir");
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess]));
            let call = make_call("echo must_not_run");

            let output = executor
                .execute(&ctx, &call)
                .await
                .expect("execute must not return Err");

            match output {
                ToolOutput::Error { message } => {
                    assert!(
                        message.contains("requires local UI approval"),
                        "with a ledger but no session approval, denial must name the \
                         missing approval, got: {message:?}"
                    );
                }
                other => panic!("expected Error output without session approval, got: {other:?}"),
            }
        }

        #[tokio::test]
        async fn test_and_permits_none_denies_host_execution() {
            let project_root = make_temp_project("permits-none");
            let executor = shell_executor_for(&SandboxProfile::Host, None, &project_root);
            let tmp = tempfile::tempdir().expect("tempdir");
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess]));
            let call = make_call("echo must_not_run");

            let output = executor
                .execute(&ctx, &call)
                .await
                .expect("execute must not return Err");

            match output {
                ToolOutput::Error { message } => {
                    assert!(
                        message.contains("host execution requires a process permit"),
                        "without host_permits the Host profile must fail closed, got: {message:?}"
                    );
                }
                other => panic!("expected Error output without permits, got: {other:?}"),
            }
        }

        #[tokio::test]
        async fn test_and_permits_configured_but_session_not_approved_denies() {
            let project_root = make_temp_project("permits-no-approval");
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let executor = shell_executor_for(
                &SandboxProfile::Host,
                Some(wiring_with_closed_channel(ledger, registry)),
                &project_root,
            );
            let tmp = tempfile::tempdir().expect("tempdir");
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess]));
            let call = make_call("echo must_not_run");

            let output = executor
                .execute(&ctx, &call)
                .await
                .expect("execute must not return Err");

            match output {
                ToolOutput::Error { message } => {
                    assert!(
                        message.contains("requires local UI approval"),
                        "with a ledger but no session approval, denial must name the \
                         missing approval, got: {message:?}"
                    );
                }
                other => panic!("expected Error output without session approval, got: {other:?}"),
            }
        }

        #[tokio::test]
        async fn test_and_permits_configured_and_session_approved_passes_permit_boundary() {
            let project_root = make_temp_project("permits-approved");
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let executor = shell_executor_for(
                &SandboxProfile::Host,
                Some(wiring_with_closed_channel(
                    Arc::clone(&ledger),
                    Arc::clone(&registry),
                )),
                &project_root,
            );
            let tmp = tempfile::tempdir().expect("tempdir");
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess]));
            registry.mark_session_approved(ctx.session_id().as_str(), Duration::from_secs(60));
            let call = make_call("echo host_ok");

            let output = executor
                .execute(&ctx, &call)
                .await
                .expect("execute must not return Err");

            // Der Permit-Grenzfehler darf nach der Zustimmung nicht mehr
            // auftreten; ein verbleibender Fehler darf nur noch von der
            // fehlenden Bubblewrap-Sandbox in der Testumgebung stammen.
            match &output {
                ToolOutput::Error { message } => {
                    assert!(
                        !message.contains("process permit") && !message.contains("UI approval"),
                        "a session-approved host command must pass the permit boundary, \
                         got: {message:?}"
                    );
                }
                ToolOutput::Json { .. } | ToolOutput::Text { .. } => {}
            }
        }

        #[tokio::test]
        async fn test_and_permits_is_ignored_for_non_host_sandbox_profile() {
            // Selbst wenn Ledger+Registry übergeben werden, dürfen sie nur an
            // ein `SandboxProfile::Host`-Provider gehängt werden: für Strict
            // ist die Sandbox die Grenze, nicht der Permit — die Ausführung
            // darf deshalb nie mit einer Permit-Fehlermeldung scheitern.
            let project_root = make_temp_project("permits-strict-ignored");
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let executor = shell_executor_for(
                &SandboxProfile::Strict,
                Some(wiring_with_closed_channel(ledger, registry)),
                &project_root,
            );
            let tmp = tempfile::tempdir().expect("tempdir");
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess]));
            let call = make_call("echo strict_mode_ok");

            let output = executor
                .execute(&ctx, &call)
                .await
                .expect("execute must not return Err");

            match &output {
                ToolOutput::Error { message } => {
                    assert!(
                        !message.contains("host execution requires a process permit")
                            && !message.contains("requires local UI approval"),
                        "Strict profile must never trigger a permit denial, got: {message:?}"
                    );
                }
                ToolOutput::Json { .. } | ToolOutput::Text { .. } => {}
            }
        }

        /// Beweist die eigentliche Ergänzung dieses Auftrags: `HostPermitWiring`
        /// hängt ihren `prompt_sender` tatsächlich an den gebauten
        /// `ShellToolProvider`, sodass eine Sitzung ohne bereits gemerkten
        /// Permit und ohne laufende Sitzungsphase eine echte Rückfrage über
        /// den Fragekanal erhält — statt (wie vor dieser Ergänzung, in der der
        /// Sender nie an einen Provider gehängt wurde) sofort mit
        /// `requires local UI approval` fail-closed abzulehnen. Ein
        /// Hintergrund-Task pollt den Empfänger und stimmt zu; erst danach darf
        /// die Ausführung die Permit-Grenze passieren.
        #[tokio::test]
        async fn test_prompt_sender_reaches_shell_provider_and_a_live_approval_passes_the_permit_boundary()
        {
            let project_root = make_temp_project("permits-prompt-sender-wired");
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let (sender, mut receiver) = harw_tool_shell::host_permit_prompt_channel();
            let wiring = HostPermitWiring::new(ledger, registry, sender)
                .with_preselected_variant(HostPermitVariant::SingleExecution);
            let executor = shell_executor_for(&SandboxProfile::Host, Some(wiring), &project_root);

            let approver = tokio::spawn(async move {
                let prompt = receiver.recv().await.expect("prompt must arrive at the receiver");
                assert_eq!(
                    prompt.preselected_variant(),
                    HostPermitVariant::SingleExecution,
                    "the wiring's preselected variant must reach the prompt unchanged"
                );
                assert!(
                    prompt.approve(HostPermitVariant::SingleExecution),
                    "the approval must reach the waiting ShellExecutor"
                );
            });

            let tmp = tempfile::tempdir().expect("tempdir");
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess]));
            let call = make_call("echo prompt_sender_wired");

            let output = executor
                .execute(&ctx, &call)
                .await
                .expect("execute must not return Err");
            approver.await.expect("approver task must not panic");

            match &output {
                ToolOutput::Error { message } => {
                    assert!(
                        !message.contains("host execution requires a process permit")
                            && !message.contains("requires local UI approval"),
                        "a live-approved prompt must pass the permit boundary, got: {message:?}"
                    );
                }
                ToolOutput::Json { .. } | ToolOutput::Text { .. } => {}
            }
        }
    }
}
