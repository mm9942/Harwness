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
use harw_tool_doc::DocToolProvider;
use harw_tool_explorer::ExplorerToolProvider;
use harw_tool_fs::FsToolProvider;
use harw_tool_lens::LensToolProvider;
use harw_tool_process::ProcessToolProvider;
use harw_tool_shell::{
    HostPermitPromptSender, HostPermitVariant, LatexToolProvider, ShellToolProvider,
};
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
use crate::skill_proposal_tools::{
    SKILL_PROPOSAL_DECIDE_TOOLS, SKILL_PROPOSAL_PROPOSE_TOOLS, SKILL_PROPOSAL_READ_TOOLS,
    SkillAuthorCeiling, SkillProposalToolProvider,
};
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
    /// Read-only Erkundung eines Verzeichnisbaums: Projekte, Dateien,
    /// Dokumente und ihre Beziehungen (sprach- und ökosystemneutral).
    pub const EXPLORER: &str = "explorer";
    /// Rust/Cargo-Spezialist der Dependency-Recherche: belegt Verhalten aus
    /// `Cargo.lock`, dem lokalen Registry-Quellcache, docs.rs und crates.io.
    /// Für jedes andere Ökosystem (npm, PyPI, Go, Maven …) ist
    /// [`DEPENDENCY_RESEARCHER`] zuständig.
    pub const RESEARCHER_DEPS: &str = "researcher-deps";
    /// Ökosystem-neutrale Dependency-Recherche (npm/pnpm/yarn, PyPI/uv/poetry,
    /// Go-Module, Maven/Gradle, Cargo): liest Manifeste und Lockfiles im
    /// Workspace und die offiziellen Paket-Registries/Dokus über das Netz —
    /// ohne die Cargo-spezifischen `deps.*`-Werkzeuge
    /// ([`crate::profile::RegistryProfile::ReadOnlyResearch`]).
    pub const DEPENDENCY_RESEARCHER: &str = "dependency-researcher";
    /// Allgemeine Recherche zu beliebigen Themen (Technik, Markt, Business,
    /// Dokumente): Web, Workspace-Dokumente und PDFs, mit analytischem
    /// Handwerk (Hypothesen, Schlüsselannahmen, Quellenbewertung,
    /// Wahrscheinlichkeit getrennt von Konfidenz) im `ResearchFinding`
    /// ([`crate::profile::RegistryProfile::ReadOnlyResearch`]).
    pub const RESEARCHER: &str = "researcher";
    /// Web-Recherche in Dokumentation, Release Notes und Paket-Metadaten über
    /// die Host-Allowlist der Sandbox — ausschließlich Netz, kein
    /// Workspace-Lesen (A5); für Rust zusätzlich `web.docs_rs`/`web.crates_io`.
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

    /// Spieler-Sitz des Matrix-Games (Runde 3, Welle E): formuliert aus der
    /// eigenen Sicht einen Zug. Liest nur die eigenen Unterlagen (ohne Netz,
    /// ohne Schreiben/Exec) — der Matrix-Runner übergibt die Sicht im Prompt
    /// (`agents/roles/matrix-player/matrix-player.toml`).
    pub const MATRIX_PLAYER: &str = "matrix-player";
    /// Schiedsrichter-Sitz des Matrix-Games: bewertet Züge und schreibt den
    /// Lagebericht. Liest nur Unterlagen (ohne Netz, ohne Schreiben/Exec)
    /// (`agents/roles/matrix-umpire/matrix-umpire.toml`).
    pub const MATRIX_UMPIRE: &str = "matrix-umpire";
    /// Markt-Sitz des Matrix-Games: schätzt Wahrscheinlichkeiten offener
    /// Entwicklungen. Liest nur Unterlagen (ohne Netz, ohne Schreiben/Exec)
    /// (`agents/roles/matrix-market/matrix-market.toml`).
    pub const MATRIX_MARKET: &str = "matrix-market";
    /// Red-Cell-Sitz des Matrix-Games: widerspricht aus der öffentlichen Lage
    /// den tragenden Annahmen der Argumente einer Runde. Liest nur Unterlagen
    /// (ohne Netz, ohne Schreiben/Exec)
    /// (`agents/roles/matrix-redcell/matrix-redcell.toml`).
    pub const MATRIX_REDCELL: &str = "matrix-redcell";
    /// Die vier Sitz-Rollen des Matrix-Games. Alle bekommen
    /// [`crate::profile::RegistryProfile::MatrixReader`] und den Reducer
    /// [`crate::authority::AuthorityReducer::ReadOnly`].
    pub const MATRIX_ROLES: [&str; 4] =
        [MATRIX_PLAYER, MATRIX_UMPIRE, MATRIX_MARKET, MATRIX_REDCELL];

    /// Game Master des Matrix-Games (Runde 7, Teil M): spezialisierter
    /// Orchestrator (`role = "root-orchestrator"`), den die UIA per
    /// `transfer_to_matrix-game-master` als Hintergrund-Kind startet. Er
    /// entwirft aus Freitext ein Szenario, lässt es freigeben, spielt es mit
    /// den vier Sitz-Rollen und liefert AAR plus `report.md`. Werkzeuge:
    /// lesende `fs.*`/`doc.read_pdf` ([`crate::profile::RegistryProfile::MatrixReader`],
    /// Reducer `ReadOnly`, kein Netz), `parent.message` und genau die
    /// Matrix-Werkzeuge aus [`crate::profile::MATRIX_GAME_MASTER_TOOLS`]
    /// (`agents/matrix-game-master.toml`).
    pub const MATRIX_GAME_MASTER: &str = "matrix-game-master";

    /// Führt Befehls- und Dateioperationen im Auftrag des Haupt-Agenten aus
    /// und liefert eine Zusammenfassung statt Rohausgaben (Slice B7). Die
    /// TUI delegiert damit Befehlsfolgen an einen eigenen Worker, statt sie
    /// als viele einzelne Tool-Aufrufe im Hauptfenster zu zeigen. Profil ist
    /// [`RegistryProfile::ShellExecution`] (ausschließlich `shell.exec`, kein
    /// Dateisystem-Werkzeug, nie `sudo`) — nicht `Full`; siehe
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

    /// LaTeX-Schreibspezialisierung der UIA (Runde 4, Teil E): dieselbe
    /// Begründung wie [`UIA_EXPLORER`]/[`UIA_WRITER`] — die Spawn-Matrix
    /// lässt der UIA nie den regulären `Worker`, deshalb trägt die Rolle
    /// `role = "uia-worker"`. Nutzerentscheidung: lesende `fs.*`,
    /// `fs.write`, `doc.read_pdf` und das typisierte `latex.build` (festes
    /// `latexmk`-argv in der Sandbox, kein freies `shell.exec`) — kein Netz,
    /// keine `deps.*`, kein `lens.ask`. LaTeX wird als installiert
    /// vorausgesetzt, nie nachinstalliert. Siehe
    /// `agents/uia-latex-writer.toml` und
    /// [`RegistryProfile::UiaLatexWriter`].
    pub const UIA_LATEX_WRITER: &str = "uia-latex-writer";

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

    /// Eingebauter Child-Orchestrator der Coding-Spur (`role =
    /// "child-orchestrator"`, Plan Punkt 1): Leader des Clans `coding`,
    /// delegiert Planung, Erkundung und Ausführung an Worker und schreibt
    /// selbst nicht. Profil wie [`ROOT_ORCHESTRATOR`]
    /// ([`crate::profile::RegistryProfile::Planning`]); siehe
    /// `agents/coding-orchestrator.toml`.
    pub const CODING_ORCHESTRATOR: &str = "coding-orchestrator";

    /// Eingebauter Child-Orchestrator der Recherche-Spur: Leader des Clans
    /// `research`, fächert auf read-only Rechercheure auf. Siehe
    /// `agents/research-orchestrator.toml`.
    pub const RESEARCH_ORCHESTRATOR: &str = "research-orchestrator";

    /// Eingebauter Child-Orchestrator der Verdichtungs-/Triage-Spur: Leader
    /// der Clans `synthesis` und `security`. Siehe
    /// `agents/analysis-orchestrator.toml`.
    pub const ANALYSIS_ORCHESTRATOR: &str = "analysis-orchestrator";

    /// Die eingebauten Child-Orchestratoren (`AgentRoleId::ChildOrchestrator`)
    /// — genau die Namen, die `agents/root-orchestrator.toml` in
    /// `[spawn].child_orchestrators` exakt freigibt.
    pub const CHILD_ORCHESTRATORS: &[&str] = &[
        CODING_ORCHESTRATOR,
        RESEARCH_ORCHESTRATOR,
        ANALYSIS_ORCHESTRATOR,
    ];

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
        DEPENDENCY_RESEARCHER,
        RESEARCHER,
        RESEARCHER_WEB,
        PLANNER,
        ANALYST,
        SECURITY_EGRESS_TRIAGE,
        SECURITY_BASELINE_TRIAGE,
        SECURITY_STRUCTURE_TRIAGE,
        SECURITY_ENDPOINT_TRIAGE,
        MATRIX_PLAYER,
        MATRIX_UMPIRE,
        MATRIX_MARKET,
        MATRIX_REDCELL,
        MATRIX_GAME_MASTER,
        EXECUTOR,
        MEMORY_STEWARD,
        UIA_WORKER,
        UIA_EXPLORER,
        UIA_WRITER,
        UIA_SHELL_WORKER,
        UIA_LATEX_WRITER,
        AGENT_STEWARD,
        CODING_ORCHESTRATOR,
        RESEARCH_ORCHESTRATOR,
        ANALYSIS_ORCHESTRATOR,
    ];
}

// ---------------------------------------------------------------------------
// Werkzeuge der Composition-Root für Orchestrator-Sitzungen
// ---------------------------------------------------------------------------

/// Die Werkzeuge, die eine Orchestrator-Sitzung über die Werkzeugoberfläche
/// ihres Registry-Profils hinaus admittiert: heute genau `delegate_wave`
/// (`harw-core-bridge::delegate_wave`, Plan Punkt 1).
///
/// # Warum nicht Teil eines [`RegistryProfile`]
/// `delegate_wave` braucht den `ManagedAgentSpawner` aus `harw-core`; dieses
/// Crate hängt bewusst nicht von `harw-core` ab. Die Composition-Root
/// (`harw-runtime`, Kind-Registry-Fabrik) hängt den Provider deshalb selbst
/// an die Registry einer Orchestrator-Rolle an. Ein eigenes
/// `RegistryProfile::Orchestration` hätte zudem jede erschöpfende
/// `match`-Stelle über `RegistryProfile` außerhalb dieses Crates gebrochen.
/// Die Deckungstests (`tests/tool_admission_coverage.rs`) vergleichen die
/// TOML-Seite deshalb gegen `profile.tool_names()` ∪
/// [`composition_tools_for_role`].
pub const ORCHESTRATION_TOOLS: &[&str] = &["delegate_wave"];

/// Liefert die Werkzeuge, die die Composition-Root für `role` zusätzlich zur
/// Profil-Registry beisteuert.
///
/// # Argumente
/// - `role` (`&str`): Rollenname, üblicherweise aus [`role_names`].
///
/// # Rückgabe
/// [`ORCHESTRATION_TOOLS`] für [`role_names::ROOT_ORCHESTRATOR`] und jeden
/// Eintrag aus [`role_names::CHILD_ORCHESTRATORS`], sonst eine leere Liste —
/// Worker bekommen nie eine Delegationsoberfläche.
///
/// # Nebenläufigkeit
/// Rein.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::{composition_tools_for_role, role_names};
///
/// assert_eq!(
///     composition_tools_for_role(role_names::CODING_ORCHESTRATOR),
///     &["delegate_wave"]
/// );
/// assert!(composition_tools_for_role(role_names::EXPLORER).is_empty());
/// ```
#[must_use]
pub fn composition_tools_for_role(role: &str) -> &'static [&'static str] {
    if is_orchestrator_role(role) {
        ORCHESTRATION_TOOLS
    } else {
        &[]
    }
}

// ── Runde 5, Teil H: `agent.result` ─────────────────────────────────────────

/// Das rein lesende Werkzeug `agent.result`
/// (`harw_core_bridge::AgentResultOperation`, Runde 5 Teil H): liefert den
/// ungekürzten Antworttext eines **eigenen**, abgeschlossenen Kind-Laufs aus
/// dem Ergebnisarchiv des Spawners, optional seitenweise.
///
/// # Warum nicht Teil eines [`RegistryProfile`]
/// Wie [`ORCHESTRATION_TOOLS`]: die Operation braucht den
/// `ManagedAgentSpawner` aus `harw-core`. Die Composition-Root hängt sie an
/// die Wurzel mit Spawner (`harw-runtime/src/assembly.rs`) und an die
/// Registry jeder Rolle aus [`child_result_tools_for_role`]
/// (`harw-runtime/src/children.rs`).
///
/// # Rechte
/// Rein lesend, ohne Sandbox-Rechteklasse (`tool_permission` liefert wie für
/// `delegate_wave` `None`): die Grenze zieht die Eltern-Kind-Bindung im
/// Spawner, nicht die Sandbox. Steht in [`crate::AUTO_APPROVED_TOOLS`].
pub const CHILD_RESULT_TOOLS: &[&str] = &["agent.result"];

/// Liefert [`CHILD_RESULT_TOOLS`] für jede Rolle, die Kinder starten darf
/// (heute die Orchestratoren, siehe [`is_orchestrator_role`]), sonst nichts.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::{child_result_tools_for_role, role_names};
///
/// assert_eq!(
///     child_result_tools_for_role(role_names::ROOT_ORCHESTRATOR),
///     &["agent.result"]
/// );
/// assert!(child_result_tools_for_role(role_names::EXPLORER).is_empty());
/// ```
#[must_use]
pub fn child_result_tools_for_role(role: &str) -> &'static [&'static str] {
    if is_orchestrator_role(role) {
        CHILD_RESULT_TOOLS
    } else {
        &[]
    }
}

// ── Plan R9, Teil A: `skills.search` / `skills.load` ────────────────────────

/// Die lesenden Skill-Katalog-Werkzeuge
/// ([`crate::skill_tools::SkillCatalogToolProvider`]): `skills.search` findet
/// Skills, `skills.load` lädt sie.
///
/// # Warum nicht Teil eines [`RegistryProfile`]
/// Der Provider braucht den bei der Montage gebauten
/// [`harw_catalog::SkillIndex`] (vertraute Config-Layer plus eingebettetes
/// Bündel) — Zustand der Composition-Root. Sie hängt ihn an die Wurzel und an
/// die Registry jeder Rolle aus [`skill_catalog_tools_for_role`]; die
/// `SessionActivation` des Kindes schaltet die Werkzeuge nur frei, wenn seine
/// Definition sie admittiert.
///
/// # Rechte
/// Keine Sandbox-Rechteklasse (`tool_permission` liefert `None`), keine
/// Freigabepflicht ([`crate::AUTO_APPROVED_TOOLS`], nie
/// [`crate::ALWAYS_ASK_TOOLS`]): Lesen im Host-Prozess, ohne Workspace, Netz
/// oder Prozess.
pub const SKILL_CATALOG_TOOLS: &[&str] = &["skills.search", "skills.load"];

/// Die eingebauten Rollen **ohne** Skill-Katalog: die vier
/// Security-Triage-Rollen (bewusst werkzeuglos, `admitted = []`) und die vier
/// Matrix-Sitze (feste, exakt geprüfte Leseflächen; ihre Anweisungen kommen
/// vom Matrix-Runner).
pub const SKILL_CATALOG_EXCLUDED_ROLES: &[&str] = &[
    role_names::SECURITY_EGRESS_TRIAGE,
    role_names::SECURITY_BASELINE_TRIAGE,
    role_names::SECURITY_STRUCTURE_TRIAGE,
    role_names::SECURITY_ENDPOINT_TRIAGE,
    role_names::MATRIX_PLAYER,
    role_names::MATRIX_UMPIRE,
    role_names::MATRIX_MARKET,
    role_names::MATRIX_REDCELL,
];

/// Liefert [`SKILL_CATALOG_TOOLS`] für jede Rolle außer
/// [`SKILL_CATALOG_EXCLUDED_ROLES`] — also für UIA-Helfer, Worker,
/// Orchestratoren, Stewards und den Game Master.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::{role_names, skill_catalog_tools_for_role};
///
/// assert_eq!(
///     skill_catalog_tools_for_role(role_names::EXPLORER),
///     &["skills.search", "skills.load"]
/// );
/// assert!(skill_catalog_tools_for_role(role_names::MATRIX_PLAYER).is_empty());
/// ```
#[must_use]
pub fn skill_catalog_tools_for_role(role: &str) -> &'static [&'static str] {
    if SKILL_CATALOG_EXCLUDED_ROLES.contains(&role) {
        &[]
    } else {
        SKILL_CATALOG_TOOLS
    }
}

// ── Runde 5, Teil M: `agent.message` / `parent.message` ─────────────────────

/// Das Werkzeug `agent.message` (`harw_core_bridge::AgentMessageOperation`,
/// Runde 5 Teil M): schickt einem **eigenen, laufenden** Kind eine
/// Textnachricht (Kurskorrektur oder Antwort auf seine Frage), die es an
/// seiner nächsten Runden-Grenze liest.
///
/// # Warum nicht Teil eines [`RegistryProfile`]
/// Wie [`CHILD_RESULT_TOOLS`]: die Operation braucht den
/// `ManagedAgentSpawner`. Die Composition-Root hängt sie an die Wurzel mit
/// Spawner und an die Registry jeder Rolle aus
/// [`child_message_tools_for_role`].
///
/// # Rechte
/// Keine Sandbox-Rechteklasse (`tool_permission` liefert `None`), keine
/// Freigabepflicht ([`crate::AUTO_APPROVED_TOOLS`], nie
/// [`crate::ALWAYS_ASK_TOOLS`]): das Werkzeug transportiert nur Text an das
/// eigene Kind — die Eltern-Kind-Bindung prüft der Spawner.
pub const CHILD_MESSAGE_TOOLS: &[&str] = &["agent.message"];

/// Liefert [`CHILD_MESSAGE_TOOLS`] für jede Rolle, die Kinder starten darf
/// (dieselbe Fläche wie [`child_result_tools_for_role`]), sonst nichts.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::{child_message_tools_for_role, role_names};
///
/// assert_eq!(
///     child_message_tools_for_role(role_names::ROOT_ORCHESTRATOR),
///     &["agent.message"]
/// );
/// assert!(child_message_tools_for_role(role_names::EXPLORER).is_empty());
/// ```
#[must_use]
pub fn child_message_tools_for_role(role: &str) -> &'static [&'static str] {
    if is_orchestrator_role(role) {
        CHILD_MESSAGE_TOOLS
    } else {
        &[]
    }
}

/// Das Werkzeug `parent.message` (`harw_core_bridge::ParentMessageOperation`,
/// Runde 5 Teil M): ein Kind meldet seinem **direkten** Elternteil einen
/// Zwischenstand (`info`, höchstens einmal je 30 s) oder stellt eine Frage
/// (`question`, wartet bis zu 10 min auf `agent.message`).
///
/// # Rechte
/// Wie [`CHILD_MESSAGE_TOOLS`]: keine Rechteklasse, keine Freigabepflicht,
/// Längendeckel 4 KiB, nur an den direkten Elternteil.
pub const PARENT_MESSAGE_TOOLS: &[&str] = &["parent.message"];

/// Die Rollen, die als Kind mit einem Elternteil laufen und
/// [`PARENT_MESSAGE_TOOLS`] bekommen.
///
/// # Bewusst nicht enthalten
/// - die vier Security-Triage-Rollen: parametergetrieben, ohne eigene
///   Werkzeugoberfläche (`[tools].admitted = []`);
/// - die vier Matrix-Sitze: der Matrix-Runner fährt sie, kein wartender
///   Eltern-Turn liest ihre Meldungen;
/// - `memory-steward`: ein Harness-interner Konsolidierungslauf ohne
///   Modell-Elternteil;
/// - `executor`: bewusst genau `shell.exec` (siehe `agents/executor.toml`
///   und `test_only_executor_gets_the_shell_execution_profile`).
pub const PARENT_MESSAGE_ROLES: &[&str] = &[
    role_names::ROOT_ORCHESTRATOR,
    role_names::CODING_ORCHESTRATOR,
    role_names::RESEARCH_ORCHESTRATOR,
    role_names::ANALYSIS_ORCHESTRATOR,
    role_names::EXPLORER,
    role_names::RESEARCHER_DEPS,
    role_names::DEPENDENCY_RESEARCHER,
    role_names::RESEARCHER,
    role_names::RESEARCHER_WEB,
    role_names::PLANNER,
    role_names::ANALYST,
    role_names::UIA_WORKER,
    role_names::UIA_EXPLORER,
    role_names::UIA_WRITER,
    role_names::UIA_SHELL_WORKER,
    role_names::UIA_LATEX_WRITER,
    role_names::AGENT_STEWARD,
    // Runde 7, Teil M: Zwischenstände und die bis zu drei Kernfragen an die UIA.
    role_names::MATRIX_GAME_MASTER,
];

/// Liefert [`PARENT_MESSAGE_TOOLS`] für die Rollen aus
/// [`PARENT_MESSAGE_ROLES`], sonst nichts.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::{parent_message_tools_for_role, role_names};
///
/// assert_eq!(
///     parent_message_tools_for_role(role_names::EXPLORER),
///     &["parent.message"]
/// );
/// assert!(parent_message_tools_for_role(role_names::MATRIX_PLAYER).is_empty());
/// ```
#[must_use]
pub fn parent_message_tools_for_role(role: &str) -> &'static [&'static str] {
    if PARENT_MESSAGE_ROLES.contains(&role) {
        PARENT_MESSAGE_TOOLS
    } else {
        &[]
    }
}

// ── Runde 5, Teil B: `host.sudo_exec` ───────────────────────────────────────

/// Das Root-Werkzeug `host.sudo_exec` (`harw_tool_shell::SudoToolProvider`,
/// Runde 5 Teil B).
///
/// # Warum nicht Teil eines [`RegistryProfile`]
/// Der Provider braucht die Sendeseite des sudo-Fragekanals, die es nur in
/// der interaktiven TUI gibt (`EntryKind::Tui`). Die Composition-Root
/// (`harw-runtime/src/children.rs`) hängt ihn deshalb nur an die Registry
/// der Rollen aus [`SUDO_ROLES`] und nur, wenn ein solcher Kanal existiert —
/// sonst fehlt das Werkzeug (fail-closed). Es steht in
/// [`crate::ALWAYS_ASK_TOOLS`] und braucht `Permission::ExecuteProcess`
/// ([`crate::authority::tool_permission`]).
///
/// # Deckung
/// Wie [`ORCHESTRATION_TOOLS`]: die Deckungstests vergleichen die TOML-Seite
/// gegen `profile.tool_names()` ∪ [`composition_tools_for_role`] ∪
/// [`knowledge_tools_for_role`] ∪ [`sudo_tools_for_role`].
pub const SUDO_TOOLS: &[&str] = &["host.sudo_exec"];

/// Die einzigen Rollen, die Root-Befehle anfragen dürfen
/// (Nutzerentscheidung Runde 5): die beiden Host-Shell-Worker.
/// `host-process-worker` steht bewusst nicht in [`role_names::ALL`] (siehe
/// dort), deshalb hier als Literal.
pub const SUDO_ROLES: &[&str] = &[role_names::UIA_SHELL_WORKER, "host-process-worker"];

/// Liefert [`SUDO_TOOLS`] für die Rollen aus [`SUDO_ROLES`], sonst nichts.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::{role_names, sudo_tools_for_role};
///
/// assert_eq!(sudo_tools_for_role(role_names::UIA_SHELL_WORKER), &["host.sudo_exec"]);
/// assert!(sudo_tools_for_role(role_names::EXECUTOR).is_empty());
/// ```
#[must_use]
pub fn sudo_tools_for_role(role: &str) -> &'static [&'static str] {
    if SUDO_ROLES.contains(&role) {
        SUDO_TOOLS
    } else {
        &[]
    }
}

// ── Runde 7, Teil M: Werkzeuge des Game Masters ─────────────────────────────

/// Die lesenden bzw. nur in den Matrix-Speicher schreibenden Werkzeuge des
/// Game Masters: `matrix.draft_scenario` (validiert ein Szenario und legt es
/// unter `<profil>/knowledge/matrix/scenarios/` ab — harness-eigener
/// Speicher, wie `plan.write` die Plan-Datei) und `matrix.status` (rein
/// lesend). Plan R9: dazu `matrix.add_fact` — trägt einen recherchierten
/// Fakt mit Belegen als öffentliche Lage ins Journal des eigenen Laufs ein
/// (nur Matrix-Speicher, kein Workspace, kein Netz). Alle drei deklarieren
/// `model_tool(approval = "none")` und stehen in
/// [`crate::AUTO_APPROVED_TOOLS`].
pub const MATRIX_GAME_MASTER_READ_TOOLS: &[&str] =
    &["matrix.draft_scenario", "matrix.status", "matrix.add_fact"];

/// Alle Werkzeuge des Game Masters in Provider-Reihenfolge
/// (`harw_ops::matrix::game_master::game_master_operations`).
/// `matrix.start`, `matrix.run` und `matrix.finish` deklarieren
/// `model_tool(approval = "always")` (Sitz-Agenten kosten Budget,
/// `matrix.finish` legt eine Berichtskopie im Workspace an) und stehen nie in
/// [`crate::AUTO_APPROVED_TOOLS`].
///
/// # Warum nicht Teil eines [`RegistryProfile`]
/// Wie [`ORCHESTRATION_TOOLS`]: die Operationen brauchen den
/// `ManagedAgentSpawner` (Sitz-Agenten) und leben in `harw-ops`. Die
/// Composition-Root (`harw-runtime/src/children.rs`) hängt sie über einen
/// rollengebundenen `ModelToolProvider` ausschließlich an die Rollen aus
/// [`matrix_tools_for_role`].
pub const MATRIX_GAME_MASTER_TOOLS: &[&str] = &[
    "matrix.draft_scenario",
    "matrix.status",
    "matrix.start",
    "matrix.run",
    "matrix.finish",
    // Plan R9: recherchierter Fakt mit Quellen als öffentliche Lage.
    "matrix.add_fact",
];

// ── R18: Gateway-Werkzeuge und WorkDriver-Bericht ───────────────────────────

/// Die lesenden `gateway.*`-Werkzeuge (R18-Vertrag §6, D-B): die fünf
/// port-gestützten Leseoperationen (`harw_ops::gateway_ops::GATEWAY_READ_OPS`)
/// und die vier Diagnoseoperationen ohne Port
/// (`harw_ops::gateway_ops::GATEWAY_DIAGNOSTICS_OPS`). Alle deklarieren
/// `model_tool(readonly, approval = "none")` und stehen in
/// [`crate::AUTO_APPROVED_TOOLS`]; der Abgleich mit der echten
/// Operations-Registry liegt in `harw-ops/tests/gateway_ops.rs` (dieses Crate
/// darf `harw-ops` nicht sehen).
///
/// # Warum nicht Teil eines [`RegistryProfile`]
/// Die Runtime hängt sie über ihre Gateway-Contributors ausschließlich an
/// eine UIA-Wurzel (`harw-runtime/src/contributors.rs`).
pub const GATEWAY_READ_TOOLS: &[&str] = &[
    "gateway.status",
    "gateway.connections.list",
    "gateway.sessions.list",
    "gateway.listeners.list",
    "gateway.tools.list",
    "gateway.channels.list",
    "gateway.channels.connect_info",
    "gateway.health",
    "gateway.logs",
];

/// Die mutierenden `gateway.*`-Werkzeuge (R18-Vertrag §6, D-B). Alle
/// deklarieren `model_tool(approval = "always")` und stehen in
/// [`crate::ALWAYS_ASK_TOOLS`]: unter `ask`/`auto` fragt jeder Aufruf, auch
/// gegen eine passende Allow-Regel. Die Freigabe erweitert nie die Rechte —
/// das Gateway prüft weiter `gateway_admin` und den Mandanten des Aufrufers.
pub const GATEWAY_MUTATION_TOOLS: &[&str] = &[
    "gateway.connections.revoke",
    "gateway.drain",
    "gateway.listeners.set",
    "gateway.tools.grant",
    "gateway.tools.narrow",
];

/// Das Rückgabewerkzeug der WorkDriver-Worker (R18-Vertrag §8, D-E,
/// `harw_plan_bridge::WORK_DRIVER_REPORT_TOOL`). Es schreibt nur den Bericht
/// in den Slot des eigenen Worker-Turns (kein Workspace, kein Prozess, kein
/// Netz) und steht in [`crate::AUTO_APPROVED_TOOLS`]: der Worker läuft
/// unbeaufsichtigt im Job, eine Rückfrage würde jede Runde als „blocked"
/// beenden. Registriert nur in der Worker-Montage von `harw-cli`
/// (`job_worker_work_driver.rs`), nie über ein [`RegistryProfile`].
pub const WORK_DRIVER_REPORT_TOOLS: &[&str] = &["work_driver.report"];

/// Liefert [`MATRIX_GAME_MASTER_TOOLS`] für [`role_names::MATRIX_GAME_MASTER`],
/// sonst nichts.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::{matrix_tools_for_role, role_names};
///
/// assert!(matrix_tools_for_role(role_names::MATRIX_GAME_MASTER).contains(&"matrix.run"));
/// assert!(matrix_tools_for_role(role_names::ROOT_ORCHESTRATOR).is_empty());
/// assert!(matrix_tools_for_role(role_names::MATRIX_PLAYER).is_empty());
/// ```
#[must_use]
pub fn matrix_tools_for_role(role: &str) -> &'static [&'static str] {
    if role == role_names::MATRIX_GAME_MASTER {
        MATRIX_GAME_MASTER_TOOLS
    } else {
        &[]
    }
}

/// Die lesenden Wissenswerkzeuge (Plan Teil D), die die Composition-Root
/// neben der Profil-Registry an Wurzel und ausgewählte Kind-Rollen hängt:
/// `workbench.show`, `diary.read`, `palace.search`, `palace.recall`.
/// Die Kanban-Lesewerkzeuge stehen getrennt in [`KANBAN_READ_TOOLS`].
///
/// # Warum nicht Teil eines [`RegistryProfile`]
/// Die Provider brauchen den `KnowledgeStore` des Profil-Homes (und für
/// `diary.read` die Agent-Id der Sitzung) — Zustand der Composition-Root,
/// nicht des Projekts. Ohne Wissensspeicher werden sie nie registriert.
/// Jedes Werkzeug ist rein lesend mit `Permission::ReadWorkspace` (siehe
/// [`crate::authority::tool_permission`]) und steht in
/// [`crate::AUTO_APPROVED_TOOLS`].
///
/// # Deckung
/// Wie bei [`ORCHESTRATION_TOOLS`] vergleichen die Deckungstests die
/// TOML-Seite gegen `profile.tool_names()` ∪ [`composition_tools_for_role`]
/// ∪ [`knowledge_tools_for_role`].
pub const KNOWLEDGE_READ_TOOLS: &[&str] = &[
    "workbench.show",
    "diary.read",
    "palace.search",
    "palace.recall",
];

/// Die lesenden Kanban-Werkzeuge (Plan D2): `kanban.list`, `kanban.show`.
///
/// # Beschreibung
/// Rein lesend (`Permission::ReadWorkspace`), aber auf Wunsch der Nutzerin
/// **nicht** in [`crate::AUTO_APPROVED_TOOLS`] — jeder Aufruf fragt — und nur
/// für die UIA-Wurzel (Montage der Composition-Root) und
/// [`role_names::ROOT_ORCHESTRATOR`]. Kanban wird nur auf ausdrücklichen
/// Wunsch der Nutzerin benutzt; kein Agent legt von selbst Aufgaben aufs
/// Board oder leitet sie daraus ab.
pub const KANBAN_READ_TOOLS: &[&str] = &["kanban.list", "kanban.show"];

/// [`KNOWLEDGE_READ_TOOLS`] ∪ [`KANBAN_READ_TOOLS`] — die Fläche des
/// Root-Orchestrators.
const ROOT_ORCHESTRATOR_KNOWLEDGE_TOOLS: &[&str] = &[
    "workbench.show",
    "diary.read",
    "palace.search",
    "palace.recall",
    "kanban.list",
    "kanban.show",
];

/// Die eingebauten Rollen, denen die Composition-Root die lesenden
/// Wissenswerkzeuge ([`KNOWLEDGE_READ_TOOLS`]) anbietet (Entscheidung der
/// Nutzerin: „Agenten dürfen Workbench, Palace und Diary lesen“; Kanban nur
/// der Root-Orchestrator, siehe [`KANBAN_READ_TOOLS`]).
///
/// # Beschreibung
/// Aufgenommen sind die Orchestratoren und die lesenden bzw. schreibenden
/// Rollen mit Workspace-Lesezugriff. Bewusst **nicht** aufgenommen:
/// - reine Exec-Rollen (`executor`, `uia-shell-worker`) — ihre Fläche ist
///   absichtlich nur der Prozesspfad (plus Lesen bei der Shell-UIA-Rolle);
/// - `researcher-web` — nur `web.*`, kein Workspace;
/// - die vier Security-Triage-Rollen und die Matrix-Sitze — feste,
///   exakt geprüfte Werkzeugmengen ohne Wissenszugriff;
/// - `memory-steward`, `agent-steward`, `uia-latex-writer` — eng
///   zugeschnittene Schreibrollen.
pub const KNOWLEDGE_READER_ROLES: &[&str] = &[
    role_names::ROOT_ORCHESTRATOR,
    role_names::CODING_ORCHESTRATOR,
    role_names::RESEARCH_ORCHESTRATOR,
    role_names::ANALYSIS_ORCHESTRATOR,
    role_names::PLANNER,
    role_names::ANALYST,
    role_names::EXPLORER,
    role_names::RESEARCHER,
    role_names::RESEARCHER_DEPS,
    role_names::DEPENDENCY_RESEARCHER,
    role_names::UIA_EXPLORER,
    role_names::UIA_WORKER,
    role_names::UIA_WRITER,
];

/// Liefert die lesenden Wissenswerkzeuge, die die Composition-Root für
/// `role` zusätzlich zur Profil-Registry beisteuern **darf**.
///
/// # Argumente
/// - `role` (`&str`): Rollenname, üblicherweise aus [`role_names`].
///
/// # Rückgabe
/// Für [`role_names::ROOT_ORCHESTRATOR`] [`KNOWLEDGE_READ_TOOLS`] und
/// [`KANBAN_READ_TOOLS`], für jede andere Rolle aus
/// [`KNOWLEDGE_READER_ROLES`] [`KNOWLEDGE_READ_TOOLS`], sonst eine leere
/// Liste. Die Kind-Registry-Fabrik montiert einen Provider
/// zusätzlich nur, wenn die Definition der Rolle mindestens eines seiner
/// Werkzeuge admittiert und ein Wissensspeicher vorliegt.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::{knowledge_tools_for_role, role_names};
///
/// assert!(knowledge_tools_for_role(role_names::EXPLORER).contains(&"palace.search"));
/// assert!(!knowledge_tools_for_role(role_names::EXPLORER).contains(&"kanban.list"));
/// assert!(knowledge_tools_for_role(role_names::ROOT_ORCHESTRATOR).contains(&"kanban.list"));
/// assert!(knowledge_tools_for_role(role_names::RESEARCHER_WEB).is_empty());
/// ```
#[must_use]
pub fn knowledge_tools_for_role(role: &str) -> &'static [&'static str] {
    if role == role_names::ROOT_ORCHESTRATOR {
        ROOT_ORCHESTRATOR_KNOWLEDGE_TOOLS
    } else if KNOWLEDGE_READER_ROLES.contains(&role) {
        KNOWLEDGE_READ_TOOLS
    } else {
        &[]
    }
}

/// Ob `role` eine eingebaute Orchestrator-Rolle ist (Root oder Child).
///
/// # Rückgabe
/// `true` für [`role_names::ROOT_ORCHESTRATOR`] und jeden Eintrag aus
/// [`role_names::CHILD_ORCHESTRATORS`].
#[must_use]
pub fn is_orchestrator_role(role: &str) -> bool {
    role == role_names::ROOT_ORCHESTRATOR || role_names::CHILD_ORCHESTRATORS.contains(&role)
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
    "fs.edit",
    "fs.list",
    "fs.search",
    "fs.glob",
    "fs.grep",
];

/// Die Werkzeuge von [`RegistryProfile::MemoryStewardship`]: die fünf
/// lesenden `fs.*`-Werkzeuge plus `fs.write`/`fs.edit`, in Provider-Reihenfolge —
/// identisch zu [`FS_FULL_TOOLS`], aber bewusst als eigene Konstante
/// benannt, weil sie (anders als `FS_FULL_TOOLS`) nie mit `shell.exec`
/// zusammen registriert werden darf. Siehe `agents/memory-steward.toml`
/// und die Begründung bei [`RegistryProfile::MemoryStewardship`].
const MEMORY_STEWARDSHIP_TOOLS: &[&str] = FS_FULL_TOOLS;

/// Das eine Werkzeug von `harw-tool-doc` (`DocToolProvider`, Anbindung W3):
/// liest eine PDF-Datei aus dem Workspace seitenweise als Text/Markdown —
/// Mistral OCR, wenn ein Mistral-Provider konfiguriert ist, sonst lokale
/// Extraktion über `oxidize-pdf`. `fs.read` liest PDFs nicht sinnvoll (Binär-
/// statt Textinhalt); `doc.read_pdf` ergänzt deshalb jedes Profil, das einen
/// lesenden `FsToolProvider` registriert — dieselbe Berechtigung
/// (`Permission::ReadWorkspace`) wie `fs.read`, siehe
/// [`crate::authority::tool_permission`].
pub(crate) const DOC_TOOLS: &[&str] = &["doc.read_pdf"];

/// Die Werkzeuge von `harw-tool-explorer`: projektunabhängiger Überblick
/// über den gesamten Baum ab der Workspace-Wurzel (Baum, Projekte,
/// Relationen, Suche). Rein lesend, `Permission::ReadWorkspace`.
pub(crate) const EXPLORER_TOOLS: &[&str] = &[
    "explore.tree",
    "explore.projects",
    "explore.relations",
    "explore.find",
];

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
pub(crate) const WEB_TOOLS: &[&str] = &["web.fetch", "web.docs_rs", "web.crates_io", "web.search"];

/// Die Netz-Werkzeuge der Erkundungsprofile [`RegistryProfile::ReadOnlyExplore`],
/// [`RegistryProfile::UiaExplorer`] und [`RegistryProfile::ReadOnlyResearch`]:
/// `web.fetch` und `web.search`, in Provider-Reihenfolge — kein
/// `web.docs_rs`/`web.crates_io` (die Crate-Werkzeuge bleiben bei
/// [`RegistryProfile::Research`] und den UIA-Helfern, siehe
/// [`UIA_HELPER_WEB_TOOLS`]).
///
/// # Warum diese Profile Netz bekommen (Nutzerentscheidungen)
/// „Der Explorer durchsucht alles … auch das Internet“: `explorer` und
/// `uia-explorer` admittieren beide Werkzeuge in ihrer TOML. Dasselbe gilt
/// für `uia-worker` und `uia-writer` (Nutzerentscheidung „die UIA-Helfer
/// recherchieren kurz online und fügen manchmal Abhängigkeiten hinzu“),
/// die dafür über [`UIA_HELPER_WEB_TOOLS`] zusätzlich die Crate-Werkzeuge
/// bekommen. Jeder Abruf braucht weiterhin `Permission::NetworkAccess` im
/// Sandbox-Scope der Rolle (Prolog, Host-Allowlist); ohne dieses Recht fallen
/// beide Werkzeuge über [`RegistryProfile::tool_names_for`] heraus. Die
/// übrigen Rollen auf `ReadOnlyExplore` (`analyst`, `researcher-deps`)
/// verbieten beide Werkzeuge ausdrücklich in ihrem `[tools].forbidden`.
/// [`RegistryProfile::ReadOnlyResearch`] (`researcher`,
/// `dependency-researcher`) führt dieselben zwei Werkzeuge.
pub(crate) const EXPLORER_WEB_TOOLS: &[&str] = &["web.fetch", "web.search"];

/// Die Netz-Werkzeuge der UIA-Helfer [`RegistryProfile::UiaQuickHelper`]
/// (`uia-worker`) und [`RegistryProfile::UiaWriter`] (`uia-writer`): alle
/// vier `web.*`-Werkzeuge in Provider-Reihenfolge — `web.fetch`,
/// `web.docs_rs`, `web.crates_io`, `web.search`.
///
/// # Warum eine eigene Konstante (Nutzerentscheidung)
/// Die UIA-Helfer prüfen vor dem Hinzufügen einer Abhängigkeit auch deren
/// crates.io-Metadaten und docs.rs-Dokumentation. Die beiden Crate-Werkzeuge
/// sind rein lesend und laufen wie `web.fetch`/`web.search` durch die
/// Egress-Policy und den Host-Scope des Elternteils. Der `explorer` (und
/// jedes andere Profil mit [`EXPLORER_WEB_TOOLS`]) bekommt sie ausdrücklich
/// **nicht** — deshalb keine Wiederverwendung von [`EXPLORER_WEB_TOOLS`].
pub(crate) const UIA_HELPER_WEB_TOOLS: &[&str] =
    &["web.fetch", "web.docs_rs", "web.crates_io", "web.search"];

/// Das einzige Browser-Werkzeug von [`RegistryProfile::UiaQuickHelper`]
/// (Nutzerentscheidung, ersetzt Addendum I in diesem Punkt): `uia-worker`
/// darf `browser.open` benutzen — `agents/uia-worker.toml` admittiert es
/// weiterhin. Statt eines expliziten Laufzeit-Grants
/// (`browser_tool_provider`, das sonst für **jedes** `browser.*`-Werkzeug der
/// einzige Weg ist, siehe die Moduldokumentation oben) registriert und
/// bewirbt `UiaQuickHelper` dieses eine Werkzeug statisch, im selben Muster
/// wie [`EXPLORER_WEB_TOOLS`] es für `web.fetch`/`web.search` tut — genau ein
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
///
/// Danach folgen die fünf Skill-Vorschlagswerkzeuge von
/// `crate::skill_proposal_tools::SkillProposalToolProvider` (neben dem
/// Agentendefinitions-Provider montiert), ebenfalls in ihrer
/// Commit-Modus-Ausprägung und Registrierungsreihenfolge.
const AGENT_DEFINITION_TOOLS: &[&str] = &[
    "agents.validate",
    "agents.list_proposals",
    "agents.write_definition",
    "agents.write_uia",
    "agents.commit_proposal",
    "agents.reject_proposal",
    "skills.validate",
    "skills.list_proposals",
    "skills.propose",
    "skills.commit_proposal",
    "skills.reject_proposal",
];

/// Die Werkzeuge von `harw-tool-shell`.
pub(crate) const SHELL_TOOLS: &[&str] = &["shell.exec"];

/// harw-tool-tunnel-v1: die vier Werkzeuge von `harw-tool-tunnel`
/// (`tunnel.start/status/stop/list`) in Registrierungsreihenfolge.
///
/// # Beschreibung
/// Statische Vertrags-Obermenge nach dem Muster von [`SHELL_TOOLS`] und
/// [`JOB_TOOLS`]: Loopback-only-Bindung, Ziel-Allowlist, Approval beim
/// Start, Keyfile-Referenz statt Key-Inhalt (Vertrag:
/// `docs/design/tunnel-policy-v1.md`). `tunnel.start` startet einen
/// verwalteten SSH-Prozess (daher später `ExecuteProcess` wie `job.start`),
/// die übrigen drei lesen bzw. beenden nur verwaltete Tunnels des
/// Aufrufers (später `ReadWorkspace` wie die Job-Control-Tools). Tatsächlich
/// montiert werden sie erst mit der Tunnel-Wiring der Plan-Knoten
/// `job-lifecycle`; bis dahin bleibt die Liste die statische Obermenge und
/// nichts wird beworben.
pub const TUNNEL_TOOLS: &[&str] = &[
    "tunnel.start",
    "tunnel.status",
    "tunnel.stop",
    "tunnel.list",
];

/// Plan R9, Teil F: die sechs Werkzeuge von `harw-tool-job`
/// (`job.start/status/logs/stop/list/wait`) in Registrierungsreihenfolge.
///
/// # Beschreibung
/// Stehen in jedem Profil direkt hinter [`SHELL_TOOLS`]: `job.start` läuft
/// über einen Klon desselben `ShellToolProvider` (Profil, Permit-Ledger,
/// Fragekanal, Host-PATH, Host-Lease), nur mit großzügigeren rlimits
/// (`ShellLimits::for_jobs`). Tatsächlich montiert werden sie nur mit einer
/// [`JobWiring`] (eine Job-Verwaltung je harw-Sitzung); ohne sie bleibt die
/// Liste die statische Vertrags-Obermenge und nichts wird beworben (siehe
/// [`assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits`]).
pub const JOB_TOOLS: &[&str] = &harw_tool_job::JOB_TOOL_NAMES;

/// Plan R9, Teil F: die Job-Werkzeuge der Orchestratoren (ohne Shell):
/// `job.status/logs/stop/list/wait` — kein `job.start`.
///
/// # Warum nicht Teil eines [`RegistryProfile`]
/// Wie [`SUDO_TOOLS`]: die Composition-Root hängt sie über
/// [`JobWiring::control_provider`] an die Registry der Rollen aus
/// [`job_control_tools_for_role`], sobald es eine Job-Verwaltung gibt.
pub const JOB_CONTROL_TOOLS: &[&str] = &harw_tool_job::JOB_CONTROL_TOOLS;

/// Liefert [`JOB_CONTROL_TOOLS`] für Orchestrator-Rollen
/// ([`is_orchestrator_role`]), sonst nichts. Shell-Rollen bekommen `job.*`
/// über ihr Profil ([`JOB_TOOLS`]).
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::{job_control_tools_for_role, role_names};
///
/// assert!(job_control_tools_for_role(role_names::ROOT_ORCHESTRATOR).contains(&"job.wait"));
/// assert!(!job_control_tools_for_role(role_names::CODING_ORCHESTRATOR).contains(&"job.start"));
/// assert!(job_control_tools_for_role(role_names::EXECUTOR).is_empty());
/// ```
#[must_use]
pub fn job_control_tools_for_role(role: &str) -> &'static [&'static str] {
    if is_orchestrator_role(role) {
        JOB_CONTROL_TOOLS
    } else {
        &[]
    }
}

/// Die typisierten LaTeX-Werkzeuge von `harw-tool-shell`
/// (`harw_tool_shell::LatexToolProvider`), in Registrierungsreihenfolge des
/// Providers:
/// - `latex.build` (Runde 4, Teil E): startet `latexmk` bzw. (Runde 7, Teil
///   T3) ohne latexmk die Engine direkt, mit festem argv in der
///   Bubblewrap-Sandbox, ohne Netz und ohne Shell-Escape; verlangt
///   `Permission::ExecuteProcess`.
/// - `latex.template` (Runde 7, Teil T2): kopiert `harw-report.sty` und das
///   Gerüst in den Workspace, überschreibt nie; verlangt
///   `Permission::WriteWorkspace` (siehe
///   [`crate::authority::tool_permission`]).
/// - `latex.check` (Runde 7, Teil T4): prüft Pakete, Schriften und Sprachen
///   per `kpsewhich`/`fc-list` in derselben Sandbox; verlangt
///   `Permission::ExecuteProcess`.
///
/// Keines steht in der Auto-Freigabe. Nur [`RegistryProfile::UiaLatexWriter`]
/// registriert sie.
pub(crate) const LATEX_TOOLS: &[&str] = &[
    harw_tool_shell::LATEX_BUILD_TOOL,
    LATEX_TEMPLATE_TOOL,
    "latex.check",
];

/// Name von `latex.template` (Runde 7, Teil T2) — das einzige LaTeX-Werkzeug
/// mit `WriteWorkspace` statt `ExecuteProcess`.
pub(crate) const LATEX_TEMPLATE_TOOL: &str = "latex.template";

/// Die Werkzeuge von `harw-tool-process`: `process.list` (Vorschau, sendet
/// nie ein Signal) und `process.kill` (destruktiv, immer freigabepflichtig
/// über [`crate::ALWAYS_ASK_TOOLS`]). Beide verlangen
/// `Permission::ExecuteProcess`; nur unter Linux wirksam.
pub(crate) const PROCESS_TOOLS: &[&str] = &["process.list", "process.kill"];

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

/// The Obsidian vault tools, in provider order.
///
/// The vault (`docs/planning` by default, overridable via
/// `HARW_OBSIDIAN_VAULT` relative to the workspace root) is the project's
/// long-form memory: code maps, architecture notes, decision records,
/// learning summaries. The four read tools give every profile that already
/// reads the workspace the same access to the notes; `obsidian.write` is
/// restricted to `Full` (coding agents) because it creates and replaces
/// files. All five resolve paths through the sandbox's workspace binding,
/// so they never exceed the caller's existing file permissions.
pub(crate) const OBSIDIAN_READ_TOOLS: &[&str] =
    &["obsidian.map", "obsidian.read", "obsidian.search", "obsidian.links"];

/// The Obsidian vault tools, in provider order.
///
/// The vault (`docs/planning` by default, overridable via
/// `HARW_OBSIDIAN_VAULT` relative to the workspace root) is the project's
/// long-form memory: code maps, architecture notes, decision records,
/// learning summaries. The four read tools give every profile that already
/// reads the workspace the same access to the notes; `obsidian.write` is
/// restricted to `Full` (coding agents) because it creates and replaces
/// files. All five resolve paths through the sandbox's workspace binding,
/// so they never exceed the caller's existing file permissions.
pub(crate) const OBSIDIAN_TOOLS: &[&str] = &[
    "obsidian.map",
    "obsidian.read",
    "obsidian.search",
    "obsidian.links",
    "obsidian.write",
];

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
/// - `Full` — voller Coding-Satz: `fs.*`, `doc.read_pdf`, `shell.exec` (ohne
///   Browser, W5 RD).
/// - `ShellExecution` — ausschließlich `shell.exec`; kein Dateisystem-Werkzeug.
/// - `ReadOnlyExplore` — ausschließlich lesend.
/// - `Research` — **nur** `web.*` (W5 RD, Annahme A5): kein `fs.*`, kein
///   `doc.read_pdf`, kein `deps.*`, damit die einzige Rolle mit Netz keine
///   Workspace-Daten lesen und hinaustragen kann. Netz-Scope aus
///   `[network].researcher_web_hosts`
///   ([`crate::research_web::researcher_web_policy`]).
/// - `Planning` — `ReadOnlyExplore` plus `lens.ask` (Plan-/Goal-Operationen
///   bewirbt es nicht: das Kind besitzt dafür keinen Executor).
/// - `MemoryStewardship` — genau `fs.*` (alle sieben Werkzeuge, inklusive
///   `fs.write`) plus `doc.read_pdf`, aber **kein** `shell.exec` und kein
///   `web.*`. Einzige eingebaute Rolle: [`role_names::MEMORY_STEWARD`]
///   (siehe deren Begründung bei [`RegistryProfile::MemoryStewardship`]
///   unten für den Grund, warum `Full` dafür zu weit wäre).
/// - `NoTools` — registriert und bewirbt gar nichts.
/// - `MatrixReader` — nur die lesenden `fs.*` plus `doc.read_pdf`; Rechte
///   genau `{ReadWorkspace}` (Matrix-Game-Sitze, siehe
///   [`RegistryProfile::MatrixReader`]).
/// - `WorkspaceEdit` — Workspace lesen und schreiben (`fs.*` inklusive
///   `fs.write`), aber kein `shell.*`, kein `web.*`, kein `lens.ask`; Rechte
///   genau `{ReadWorkspace, WriteWorkspace}` (siehe
///   [`RegistryProfile::WorkspaceEdit`]).
/// - `UiaLatexWriter` — lesende `fs.*` plus `fs.write` plus `doc.read_pdf`
///   plus `latex.build` unter der UIA-Organisationsrolle; kein `shell.*`,
///   kein `process.*`, kein `web.*`, kein `deps.*`, kein `explore.*`, kein
///   `lens.ask` (siehe [`RegistryProfile::UiaLatexWriter`]).
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
    /// Voller Coding-Satz: fs.*, doc.read_pdf, explore.*, shell.exec sowie
    /// `process.list`/`process.kill` (`ExecuteProcess`; `process.kill` fragt
    /// immer, siehe [`crate::ALWAYS_ASK_TOOLS`]). Browser nur über expliziten
    /// Grant.
    Full,
    /// Dedizierter Prozessworker: ausschließlich `shell.exec`. Das Profil ist
    /// absichtlich kein Fallback und erhält weder `fs.write` noch lesende
    /// Workspace-Werkzeuge.
    ShellExecution,
    /// Ausschließlich lesend: fs.read/list/search/glob/grep + doc.read_pdf +
    /// explore.* + deps.* + `web.fetch`/`web.search` ([`EXPLORER_WEB_TOOLS`],
    /// nur mit `NetworkAccess`; Nutzerentscheidung „der Explorer durchsucht
    /// auch das Internet“). Kein Schreib- und kein Ausführungswerkzeug.
    ReadOnlyExplore,
    /// Nur web.* — kein Workspace-Lesen (A5); Netz nur über
    /// `[network].researcher_web_hosts`.
    Research,
    /// ReadOnlyExplore ohne `web.*` + `lens.ask` (root-orchestrator und
    /// planner verbieten Netz-Werkzeuge). Die Plan-/Goal-Operationen der
    /// Composition-Root gehören **nicht** dazu (siehe W1-05).
    Planning,
    /// Registriert und bewirbt keine Werkzeuge — für Rollen, die bereits
    /// ausgewertete Befund-Batches als Parameter bekommen (`[tools].admitted
    /// = []`), etwa die vier `security-*-triage`-Rollen (siehe
    /// [`role_names::SECURITY_EGRESS_TRIAGE`] u. a.).
    ///
    /// Zugleich die lokale Grundlage eines an ein Gateway angebundenen
    /// Agenten (R18 D-A): dort montiert die Runtime `NoTools` plus
    /// ausschließlich den entfernten Proxy aus `harw-tool-remote`
    /// (`harw_runtime::assembly::RuntimeAssemblyBuilder::remote_tools`);
    /// kein lokaler Ausführer steht daneben.
    NoTools,
    /// Genau `fs.*` (alle sieben Werkzeuge, inklusive `fs.write`) plus
    /// `doc.read_pdf` — kein `shell.exec`, kein `web.*`, kein `deps.*`, kein
    /// `lens.ask`.
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
    /// Workspace-Zugriff (`FS_READ_ONLY_TOOLS`) plus `doc.read_pdf`
    /// (`DOC_TOOLS`) plus `shell.exec` (`SHELL_TOOLS`, läuft wie überall über
    /// Sandbox+Freigabe) plus die fünf lesenden `deps.*`-Werkzeuge
    /// ([`DEPS_TOOLS`]) plus alle vier `web.*`-Werkzeuge
    /// ([`UIA_HELPER_WEB_TOOLS`]) plus ausschließlich `browser.open`
    /// (Nutzerentscheidung, siehe [`UIA_QUICK_HELPER_BROWSER_TOOLS`]) — kein
    /// `fs.write`, kein `lens.ask`, keine der übrigen sechs
    /// `browser.*`-Werkzeuge.
    ///
    /// # Warum dieses Profil existiert (Addendum I)
    /// [`role_names::UIA_WORKER`] war zuvor auf [`RegistryProfile::Research`]
    /// abgebildet (reine Netz-Recherche der UIA). Die Nutzerentscheidung aus
    /// Addendum I korrigiert das: `uia-worker` ist der exklusive
    /// Schnellhelfer der UIA für kleine Schnelleingriffe — eine Frage mit
    /// einem Aufruf beantworten, schnell etwas in der Shell regeln, eine
    /// Datei lesen — nicht nur Netz-Recherche.
    ///
    /// # Warum `web.search` und `deps.*` (spätere Nutzerentscheidung)
    /// `uia-worker` recherchiert kurz online und fügt manchmal eine
    /// Abhängigkeit hinzu. Dafür bekommt das Profil neben `web.fetch` auch
    /// `web.search` ([`EXPLORER_WEB_TOOLS`], wie der Explorer) und die rein
    /// lesenden Dependency-Werkzeuge ([`DEPS_TOOLS`]: `deps.graph`/
    /// `deps.locked` über `ReadWorkspace`, `deps.source_*` über
    /// `ReadCargoRegistry`), um vorhandene Versionen und Quellen zu prüfen,
    /// bevor eine Abhängigkeit dazukommt. Seit einer weiteren
    /// Nutzerentscheidung kommen `web.docs_rs`/`web.crates_io` hinzu
    /// ([`UIA_HELPER_WEB_TOOLS`]): rein lesend, egress-gebunden wie
    /// `web.fetch`/`web.search`. Das Netz ist dabei nie weiter als das
    /// des Elternteils: der Reducer der Rolle ist
    /// [`crate::authority::AuthorityReducer::ReadExplore`], die Kind-Sandbox
    /// erbt Recht und Host-Scope des Elternteils, und jeder Abruf läuft
    /// zusätzlich durch die prozessweite Egress-Policy.
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
    /// Der lesende `fs.*`-Kern ([`FS_READ_ONLY_TOOLS`]) plus `doc.read_pdf`
    /// ([`DOC_TOOLS`]) plus die Agentendefinitions-Werkzeuge von
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
    /// plus [`DOC_TOOLS`] (`doc.read_pdf`) plus [`EXPLORER_TOOLS`]
    /// (`explore.*`) plus [`EXPLORER_WEB_TOOLS`] (`web.fetch`, `web.search`)
    /// — kein `fs.write`, kein `shell.exec`, kein `deps.*`, kein `lens.ask`.
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
    /// [`RegistryProfile::ReadOnlyExplore`] (inklusive `explore.*` und
    /// `web.fetch`/`web.search`), aber ohne `deps.*` — siehe
    /// `agents/uia-explorer.toml`.
    UiaExplorer,
    /// Schreibende Erkundungsspezialisierung der UIA: alle sieben
    /// `fs.*`-Werkzeuge inklusive `fs.write` ([`FS_FULL_TOOLS`]) plus
    /// [`DOC_TOOLS`] (`doc.read_pdf`) plus die fünf lesenden
    /// `deps.*`-Werkzeuge ([`DEPS_TOOLS`]) plus alle vier `web.*`-Werkzeuge
    /// ([`UIA_HELPER_WEB_TOOLS`]) — kein `shell.exec`, kein `lens.ask`, kein
    /// `explore.*`.
    ///
    /// # Warum dieses Profil existiert
    /// Dieselbe Begründung wie bei [`RegistryProfile::UiaExplorer`]: ein
    /// UIA-Auftrag, der eine gefundene Datei auch tatsächlich ändern soll,
    /// braucht `fs.write` unter der für die UIA zugelassenen
    /// Organisationsrolle `role = "uia-worker"` — siehe
    /// `agents/uia-writer.toml` und [`role_names::UIA_WRITER`].
    ///
    /// # Warum `web.search` und `deps.*` (Nutzerentscheidung)
    /// `uia-writer` fügt manchmal eine Abhängigkeit hinzu und recherchiert
    /// dafür kurz online: `web.search`/`web.fetch` für die Recherche,
    /// `web.docs_rs`/`web.crates_io` für Crate-Metadaten und -Dokumentation,
    /// `deps.*` (rein lesend), um `Cargo.lock` und vorhandene Quellen zu
    /// prüfen, bevor `fs.write` das Manifest ändert. Netz bleibt an den
    /// Elternteil gebunden — siehe dieselbe Begründung bei
    /// [`RegistryProfile::UiaQuickHelper`].
    UiaWriter,
    /// Host-Shell-Spezialisierung der UIA: [`FS_READ_ONLY_TOOLS`] plus
    /// [`DOC_TOOLS`] (`doc.read_pdf`) plus [`SHELL_TOOLS`] — kein `fs.write`,
    /// kein `web.*`, kein `deps.*`, kein `lens.ask`.
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
    /// Lesende Recherche mit Netz: [`FS_READ_ONLY_TOOLS`] plus [`DOC_TOOLS`]
    /// (`doc.read_pdf`) plus [`EXPLORER_TOOLS`] (`explore.*`) plus
    /// [`EXPLORER_WEB_TOOLS`] (`web.fetch`, `web.search`) — kein `fs.write`,
    /// kein `shell.exec`, kein `deps.*`, kein `lens.ask`,
    /// kein `web.docs_rs`/`web.crates_io`.
    ///
    /// # Warum dieses Profil existiert
    /// Harwness ist nicht Rust-zentriert: Recherche zu npm-, PyPI-, Go- oder
    /// Maven-Abhängigkeiten, zu Märkten, Wettbewerbern oder Fachdokumenten
    /// braucht Workspace-Dokumente (Manifeste, Lockfiles, PDFs) **und**
    /// offizielle Quellen im Netz — aber keine Cargo-spezifischen
    /// `deps.*`-Werkzeuge (die nur `Cargo.lock` und den Cargo-Registry-Cache
    /// kennen). `ReadOnlyExplore` bringt genau diese `deps.*` mit, `Research`
    /// liest keinen Workspace (A5), `UiaExplorer` hat dieselbe
    /// Werkzeugmenge, gehört aber zur Organisationsrolle der UIA und trägt
    /// deren Rollenbeschreibung. Rollen: [`role_names::DEPENDENCY_RESEARCHER`]
    /// und [`role_names::RESEARCHER`], beide mit dem Reducer
    /// [`crate::authority::AuthorityReducer::ReadWorkspaceNetwork`] — Netz
    /// nur, wenn der Elternteil es trägt, und nur zu dessen
    /// (egress-gebundenen) Hosts.
    ReadOnlyResearch,
    /// Workspace lesen und schreiben, aber weder ausführen noch ins Netz:
    /// alle sieben `fs.*`-Werkzeuge (inklusive `fs.write`) plus
    /// `doc.read_pdf` plus `explore.*` plus die workspace-lesenden
    /// `deps.graph`/`deps.locked` — kein `shell.*`, kein `process.*`, kein
    /// `web.*`, kein `lens.ask`, kein `deps.source_*`.
    ///
    /// # Warum dieses Profil existiert (Runde 3, Welle D)
    /// Ein Telegram-Chat mit gebundenem Workspace soll Dateien lesen und
    /// (mit Freigabe) ändern können, aber keine Shell bekommen. `Full` wäre
    /// zu weit (`shell.exec`, `process.*`), `ReadOnlyExplore` kann nicht
    /// schreiben und bringt Netz-Werkzeuge mit, `UiaWriter` bringt `web.*`
    /// und den Registry-Quellcache mit. Die Rechte bleiben deshalb exakt
    /// `{ReadWorkspace, WriteWorkspace}`: `deps.source_*`
    /// (`ReadCargoRegistry`) ist bewusst nicht dabei. `fs.edit` (Runde 5,
    /// Teil D) gehört mit `fs.write` dazu — gleiches Recht, Freigabe je Aufruf.
    /// Keine eingebaute Rolle bekommt dieses Profil — es ist ein reines
    /// Einstiegsprofil.
    WorkspaceEdit,
    /// Nur lesende Unterlagen: [`FS_READ_ONLY_TOOLS`] (`fs.read`, `fs.list`,
    /// `fs.search`, `fs.glob`, `fs.grep`) plus [`DOC_TOOLS`]
    /// (`doc.read_pdf`) — kein `fs.write`, kein `shell.*`, kein
    /// `process.*`, kein `web.*`, kein `explore.*`, kein `deps.*`, kein
    /// `lens.ask`. Rechte genau `{ReadWorkspace}`.
    ///
    /// # Warum dieses Profil existiert (Runde 3, Matrix-Unterlagen)
    /// Nutzerwunsch: die Sitze des Matrix-Games ([`role_names::MATRIX_ROLES`])
    /// sollen Dateien aus einem Unterlagen-Ordner lesen können. Der
    /// Matrix-Runner kopiert je Sitz nur die für ihn sichtbaren Unterlagen in
    /// einen eigenen Ordner und setzt ihn als Sandbox-Wurzel des Kindes; das
    /// Profil liefert genau die Werkzeuge, um diesen Ordner zu lesen.
    /// `ReadOnlyExplore` wäre zu weit (`explore.*` sieht den ganzen Baum,
    /// `deps.*` den Registry-Cache, dazu `web.*`), `NoTools` zu eng. Der
    /// Reducer der Rollen bleibt
    /// [`crate::authority::AuthorityReducer::ReadOnly`] — kein Netz.
    MatrixReader,
    /// LaTeX-Schreibspezialisierung der UIA: alle sieben `fs.*`-Werkzeuge
    /// inklusive `fs.write` ([`FS_FULL_TOOLS`]) plus [`DOC_TOOLS`]
    /// (`doc.read_pdf`) plus [`LATEX_TOOLS`] (`latex.build`, `latex.template`,
    /// `latex.check`) — kein
    /// `shell.*`, kein `process.*`, kein `web.*`, kein `deps.*`, kein
    /// `explore.*`, kein `lens.ask`. Rechte genau
    /// `{ReadWorkspace, WriteWorkspace, ExecuteProcess}`.
    ///
    /// # Warum dieses Profil existiert (Runde 4, Teil E)
    /// Nutzerentscheidung: der mitgelieferte LaTeX-Worker der UIA liest
    /// Quellen, legt `.tex`/`.bib` an und ändert sie, liest PDFs als Vorlage
    /// und darf einen Build **starten** — aber nur über das typisierte
    /// `latex.build` (festes `latexmk`-argv, Sandbox ohne Netz, kein
    /// Shell-Escape, Freigabe je Aufruf), nie über eine freie Shell. LaTeX
    /// wird als installiert vorausgesetzt; fehlt es, meldet `latex.build`
    /// `not_installed` samt Hinweis für die Nutzerin.
    /// Kein bestehendes Profil passt genau: [`RegistryProfile::WorkspaceEdit`]
    /// bringt `explore.*` und `deps.graph`/`deps.locked` mit,
    /// [`RegistryProfile::UiaWriter`] `web.*` und `deps.*`, und
    /// [`RegistryProfile::MemoryStewardship`] hat zwar dieselbe
    /// Werkzeugmenge, ist aber der Gedächtnis-Konsolidierung vorbehalten und
    /// trägt deren Rollenbeschreibung; `UiaShellWorker` bringt die freie
    /// Shell mit. `fs.edit` (Runde 5, Teil D) ist wie `fs.write` zugelassen.
    /// Rolle: [`role_names::UIA_LATEX_WRITER`],
    /// Reducer [`crate::authority::AuthorityReducer::ReadOnly`] — ohne Netz;
    /// `fs.write` (`WriteWorkspace`) und `latex.build` (`ExecuteProcess`)
    /// sind wie bei `uia-writer`/`uia-shell-worker` die dokumentierte
    /// Ausnahme nach Muster `executor` (kein Reducer trägt Schreib- oder
    /// Ausführungsrecht weiter).
    UiaLatexWriter,
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
        RegistryProfile::ReadOnlyResearch,
        RegistryProfile::WorkspaceEdit,
        RegistryProfile::MatrixReader,
        RegistryProfile::UiaLatexWriter,
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
            RegistryProfile::ReadOnlyResearch => "read-only research agent",
            RegistryProfile::WorkspaceEdit => "workspace editing agent without shell",
            RegistryProfile::MatrixReader => "matrix game seat reading its materials",
            RegistryProfile::UiaLatexWriter => {
                "LaTeX writing specialization of the user interface agent"
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
    /// `fs.write`), [`RegistryProfile::UiaShellWorker`] (registriert
    /// `shell.exec`), [`RegistryProfile::WorkspaceEdit`] (registriert
    /// `fs.write`) und [`RegistryProfile::UiaLatexWriter`] (registriert
    /// `fs.write` und `latex.build`). Alle übrigen Profile — inklusive
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
                | RegistryProfile::WorkspaceEdit
                | RegistryProfile::UiaLatexWriter
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
                .chain(DOC_TOOLS.iter())
                .chain(EXPLORER_TOOLS.iter())
                .chain(SHELL_TOOLS.iter())
                .chain(JOB_TOOLS.iter())
                .chain(PROCESS_TOOLS.iter())
                .chain(OBSIDIAN_TOOLS.iter())
                .copied()
                .collect(),
            RegistryProfile::ShellExecution => SHELL_TOOLS
                .iter()
                .chain(JOB_TOOLS.iter())
                .copied()
                .collect(),
            RegistryProfile::ReadOnlyExplore => FS_READ_ONLY_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .chain(EXPLORER_TOOLS.iter())
                .chain(DEPS_TOOLS.iter())
                // Der Explorer darf auch das Netz durchsuchen (`web.fetch`,
                // `web.search`, siehe `EXPLORER_WEB_TOOLS`); `analyst` und
                // `researcher-deps` verbieten beide in ihrer TOML, und jeder
                // Abruf braucht weiterhin `NetworkAccess` im Sandbox-Scope.
                .chain(EXPLORER_WEB_TOOLS.iter())
                .copied()
                .collect(),
            // `Planning` teilt den read-only Kern mit `ReadOnlyExplore`, hängt
            // aber zusätzlich `lens.ask` an — siehe die Begründung bei
            // `LENS_TOOLS`, warum ausschließlich `planner` dieses Werkzeug
            // bekommt.
            RegistryProfile::Planning => FS_READ_ONLY_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .chain(EXPLORER_TOOLS.iter())
                .chain(DEPS_TOOLS.iter())
                .chain(OBSIDIAN_READ_TOOLS.iter())
                .chain(LENS_TOOLS.iter())
                .copied()
                .collect(),
            // Nur `web.*` (A5): die Rolle mit Netz liest keine Workspace-Daten.
            RegistryProfile::Research => WEB_TOOLS.to_vec(),
            // Siehe die Begründung bei `RegistryProfile::NoTools`: keine
            // Werkzeuge registriert, keine beworben.
            RegistryProfile::NoTools => Vec::new(),
            // Alle sieben `fs.*`-Werkzeuge (inklusive `fs.write`) plus
            // `doc.read_pdf`, aber kein `shell.exec` — siehe die Begründung
            // bei `RegistryProfile::MemoryStewardship`.
            RegistryProfile::MemoryStewardship => MEMORY_STEWARDSHIP_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .copied()
                .collect(),
            // Schnellhelfer der UIA (Addendum I): lesender fs.*-Kern plus
            // `doc.read_pdf` plus `shell.exec` plus die lesenden `deps.*`
            // plus `web.fetch`/`web.search` plus ausschließlich
            // `browser.open` (Nutzerentscheidungen) — siehe die Begründung
            // bei `RegistryProfile::UiaQuickHelper`.
            RegistryProfile::UiaQuickHelper => FS_READ_ONLY_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .chain(SHELL_TOOLS.iter())
                .chain(JOB_TOOLS.iter())
                .chain(DEPS_TOOLS.iter())
                .chain(UIA_HELPER_WEB_TOOLS.iter())
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
                .chain(DOC_TOOLS.iter())
                .chain(AGENT_DEFINITION_TOOLS.iter())
                .copied()
                .collect(),
            // Read-only Erkundungsspezialisierung der UIA (siehe die
            // Begründung bei `RegistryProfile::UiaExplorer`): lesender
            // fs.*-Kern plus `doc.read_pdf` plus `explore.*` plus
            // `web.fetch`/`web.search` — kein `deps.*`.
            RegistryProfile::UiaExplorer => FS_READ_ONLY_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .chain(EXPLORER_TOOLS.iter())
                .chain(EXPLORER_WEB_TOOLS.iter())
                .copied()
                .collect(),
            // Schreibende Erkundungsspezialisierung der UIA (siehe die
            // Begründung bei `RegistryProfile::UiaWriter`): alle sieben
            // fs.*-Werkzeuge (inklusive `fs.write`) plus `doc.read_pdf` plus
            // die lesenden `deps.*` plus `web.fetch`/`web.search` — kein
            // `shell.exec`.
            RegistryProfile::UiaWriter => FS_FULL_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .chain(DEPS_TOOLS.iter())
                .chain(UIA_HELPER_WEB_TOOLS.iter())
                .copied()
                .collect(),
            // Host-Shell-Spezialisierung der UIA (siehe die Begründung bei
            // `RegistryProfile::UiaShellWorker`): lesender fs.*-Kern plus
            // `doc.read_pdf` plus `shell.exec` — kein `web.*`, kein `deps.*`.
            RegistryProfile::UiaShellWorker => FS_READ_ONLY_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .chain(SHELL_TOOLS.iter())
                .chain(JOB_TOOLS.iter())
                .copied()
                .collect(),
            // Lesende Recherche mit Netz (siehe die Begründung bei
            // `RegistryProfile::ReadOnlyResearch`): lesender fs.*-Kern plus
            // `doc.read_pdf` plus `explore.*` plus `web.fetch`/`web.search` —
            // kein `deps.*` (Cargo-spezifisch).
            RegistryProfile::ReadOnlyResearch => FS_READ_ONLY_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .chain(EXPLORER_TOOLS.iter())
                .chain(EXPLORER_WEB_TOOLS.iter())
                .copied()
                .collect(),
            // Workspace lesen und schreiben ohne Shell und ohne Netz (siehe
            // die Begründung bei `RegistryProfile::WorkspaceEdit`): alle
            // sieben `fs.*` plus `doc.read_pdf` plus `explore.*` plus die
            // workspace-lesenden `deps.graph`/`deps.locked`.
            RegistryProfile::WorkspaceEdit => FS_FULL_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .chain(EXPLORER_TOOLS.iter())
                .chain(DEPS_WORKSPACE_TOOLS.iter())
                .copied()
                .collect(),
            // Nur lesende Unterlagen der Matrix-Sitze (siehe die Begründung
            // bei `RegistryProfile::MatrixReader`): lesender fs.*-Kern plus
            // `doc.read_pdf` — kein Netz, kein Schreiben, keine Ausführung.
            RegistryProfile::MatrixReader => FS_READ_ONLY_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .copied()
                .collect(),
            // LaTeX-Schreibspezialisierung der UIA (siehe die Begründung bei
            // `RegistryProfile::UiaLatexWriter`): alle sieben `fs.*`
            // (inklusive `fs.write`) plus `doc.read_pdf` plus `latex.build`
            // — kein Netz, keine freie Shell, keine `deps.*`, kein
            // `explore.*`.
            RegistryProfile::UiaLatexWriter => FS_FULL_TOOLS
                .iter()
                .chain(DOC_TOOLS.iter())
                .chain(LATEX_TOOLS.iter())
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
        // Orchestratoren (Root und die drei Child-Orchestratoren) teilen die
        // read-only Planungsoberfläche ohne Netz; ihr Fan-out-Werkzeug
        // `delegate_wave` steuert die Composition-Root bei (siehe
        // `composition_tools_for_role`).
        role_names::ROOT_ORCHESTRATOR
        | role_names::CODING_ORCHESTRATOR
        | role_names::RESEARCH_ORCHESTRATOR
        | role_names::ANALYSIS_ORCHESTRATOR => Some(RegistryProfile::Planning),
        role_names::EXPLORER | role_names::RESEARCHER_DEPS | role_names::ANALYST => {
            Some(RegistryProfile::ReadOnlyExplore)
        }
        role_names::RESEARCHER_WEB => Some(RegistryProfile::Research),
        // Ökosystem-neutrale und allgemeine Recherche: Workspace-Dokumente
        // plus Netz, ohne die Cargo-spezifischen `deps.*` — siehe die
        // Begründung bei `RegistryProfile::ReadOnlyResearch`.
        role_names::DEPENDENCY_RESEARCHER | role_names::RESEARCHER => {
            Some(RegistryProfile::ReadOnlyResearch)
        }
        role_names::PLANNER => Some(RegistryProfile::Planning),
        // Die vier Triage-Rollen admittieren kein Werkzeug (`[tools].admitted
        // = []`) — sie bekommen bereits ausgewertete Befund-Batches als
        // Parameter. Siehe die Begründung bei `RegistryProfile::NoTools`.
        role_names::SECURITY_EGRESS_TRIAGE
        | role_names::SECURITY_BASELINE_TRIAGE
        | role_names::SECURITY_STRUCTURE_TRIAGE
        | role_names::SECURITY_ENDPOINT_TRIAGE => Some(RegistryProfile::NoTools),
        // Die vier Matrix-Game-Sitze lesen nur ihre Unterlagen (lesende
        // `fs.*` plus `doc.read_pdf`, ohne Netz, Schreiben oder Exec); der
        // Matrix-Runner übergibt jedem Sitz seine Sicht im Prompt und liest
        // die Antwort als Text (`agents/roles/matrix-*/matrix-*.toml`). Siehe
        // die Begründung bei `RegistryProfile::MatrixReader`.
        role_names::MATRIX_PLAYER
        | role_names::MATRIX_UMPIRE
        | role_names::MATRIX_MARKET
        | role_names::MATRIX_REDCELL => Some(RegistryProfile::MatrixReader),
        // Runde 7, Teil M: der Game Master liest höchstens einen Brief im
        // Workspace (dieselbe lesende Fläche ohne Netz, Schreiben oder Exec);
        // seine Matrix-Werkzeuge steuert die Composition-Root bei (siehe
        // `matrix_tools_for_role`).
        role_names::MATRIX_GAME_MASTER => Some(RegistryProfile::MatrixReader),
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
        // plus `shell.exec` plus lesende `deps.*` plus `web.fetch`/
        // `web.search`, siehe `agents/uia-worker.toml`
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
        // LaTeX-Schreibspezialisierung der UIA („nur schreiben“) — siehe die
        // Begründung bei `RegistryProfile::UiaLatexWriter` und
        // `agents/uia-latex-writer.toml`.
        role_names::UIA_LATEX_WRITER => Some(RegistryProfile::UiaLatexWriter),
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
    /// `agents.validate`/`agents.list_proposals` (und der daneben montierte
    /// `SkillProposalToolProvider` nur `skills.validate`/
    /// `skills.list_proposals`).
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
    // Danach die Werkzeuge des daneben montierten
    // `SkillProposalToolProvider` — dieselbe Staffelung: lesend immer, mit
    // Decke `skills.propose`, im Commit-Modus zusätzlich Commit/Reject.
    tools.extend_from_slice(SKILL_PROPOSAL_READ_TOOLS);
    if let Some(access) = access {
        if access.ceiling.is_some() {
            tools.extend_from_slice(SKILL_PROPOSAL_PROPOSE_TOOLS);
            if access.mode == DefinitionWriteMode::Commit {
                tools.extend_from_slice(SKILL_PROPOSAL_DECIDE_TOOLS);
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
    /// Runde 5, Teil B: Sendeseite des sudo-Fragekanals (`host.sudo_exec`).
    /// Nur die TUI-Montage setzt ihn; `None` heißt: kein
    /// `harw_tool_shell::SudoToolProvider` wird gebaut (fail-closed). Reist
    /// mit dieser Verdrahtung, damit er die Kind-Fabriken ohne eigene
    /// Verdrahtungsebene erreicht.
    pub sudo_prompts: Option<harw_tool_shell::SudoPromptSender>,
    /// Runde 5, Teil N: Host-Mode-Anfragen aus dem Agentenbaum
    /// (`shell.exec` mit `request_host`). Nur die TUI-Montage setzt sie;
    /// `None` heißt: jede Anfrage endet fail-closed mit
    /// `harw_tool_shell::HOST_MODE_REQUIRES_TUI_MSG`. Reist wie
    /// `sudo_prompts` mit dieser Verdrahtung zu jeder Kind-Fabrik.
    pub host_escalation: Option<harw_tool_shell::HostEscalation>,
    /// Runde 5, Teil N: Obergrenze für `shell.exec`-Zeitlimits aus
    /// `[shell] max_timeout_secs` (bereits geklemmt). `None`: Vorgabe
    /// `harw_tool_shell::DEFAULT_MAX_TIMEOUT_SECS`. Reist mit dieser
    /// Verdrahtung, weil sie jeden Shell-Provider (Wurzel und Kinder) erreicht.
    pub shell_max_timeout_secs: Option<u64>,
    /// Plan R9, Teil F: die Job-Verwaltung der harw-Sitzung samt
    /// Elternkette. `Some` montiert `job.*` neben jedem `shell.exec`
    /// ([`JOB_TOOLS`]) und die Kontrollwerkzeuge der Orchestratoren
    /// ([`JOB_CONTROL_TOOLS`]); reist wie `sudo_prompts` zu jeder
    /// Kind-Fabrik. `None`: keine Job-Werkzeuge.
    pub jobs: Option<JobWiring>,
}

/// Plan R9, Teil F: Job-Verwaltung und Elternkette für `job.*`.
///
/// # Beschreibung
/// Eine [`harw_tool_job::JobManager`] je harw-Sitzung (Wurzel und alle
/// Kinder teilen sie); die Elternkette ([`harw_tool_job::JobLineage`])
/// liefert der Agentenbaum der Montage. Besitz eines Jobs: Erzeuger plus
/// Vorfahren (die aufrufende Sitzung kommt aus dem Ausführungskontext).
#[derive(Clone)]
pub struct JobWiring {
    /// Die Job-Verwaltung der Sitzung.
    pub manager: Arc<harw_tool_job::JobManager>,
    /// Elternkette einer Agenten-Sitzung (Besitz und Weiterleitung).
    pub lineage: Arc<dyn harw_tool_job::JobLineage>,
}

impl std::fmt::Debug for JobWiring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobWiring")
            .field("manager", &self.manager)
            .finish_non_exhaustive()
    }
}

impl JobWiring {
    /// Bündelt Verwaltung und Elternkette.
    #[must_use]
    pub fn new(
        manager: Arc<harw_tool_job::JobManager>,
        lineage: Arc<dyn harw_tool_job::JobLineage>,
    ) -> Self {
        Self { manager, lineage }
    }

    /// Die sechs `job.*`-Werkzeuge über einem Klon von `shell`.
    ///
    /// # Beschreibung
    /// `job.start` geht durch
    /// `ShellToolProvider::prepare_background_launch` desselben (geklonten)
    /// Providers — Profil, Permit-Ledger, Fragekanal, Host-PATH und
    /// Host-Lease verhalten sich wie bei `shell.exec`. Nur die rlimits sind
    /// die großzügigen Job-Grenzen ([`harw_tool_shell::ShellLimits::for_jobs`]:
    /// kein Adressraum- und kein Dateigrößen-Deckel; die CPU-Grenze wächst
    /// mit dem CPU-Budget der Verwaltung wie bei `shell.exec`).
    #[must_use]
    pub fn tool_provider(&self, shell: &ShellToolProvider) -> harw_tool_job::JobToolProvider {
        let mut launcher_shell = shell.clone();
        launcher_shell.limits = harw_tool_shell::ShellLimits::for_jobs();
        harw_tool_job::job_tools(
            Arc::clone(&self.manager),
            Arc::new(harw_tool_job::ShellJobLauncher::new(launcher_shell)),
            Arc::clone(&self.lineage),
        )
    }

    /// Nur [`JOB_CONTROL_TOOLS`] (lesen, warten, stoppen — kein
    /// `job.start`) für Orchestratoren ohne Shell. Der Startweg ist ein
    /// Sandbox-Standard-Shell-Provider, aber über den Filter nie erreichbar.
    #[must_use]
    pub fn control_provider(&self) -> Arc<dyn ToolProvider> {
        let provider: Arc<dyn ToolProvider> =
            Arc::new(self.tool_provider(&ShellToolProvider::default()));
        Arc::new(RestrictedToolProvider::new(provider, JOB_CONTROL_TOOLS))
    }
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
            sudo_prompts: None,
            host_escalation: None,
            shell_max_timeout_secs: None,
            jobs: None,
        }
    }

    /// Plan R9, Teil F: hängt die Job-Verwaltung an (`job.*` neben jedem
    /// `shell.exec`, Kontrollwerkzeuge für Orchestratoren).
    #[must_use]
    pub fn with_jobs(mut self, jobs: Option<JobWiring>) -> Self {
        self.jobs = jobs;
        self
    }

    /// Runde 5, Teil N: hängt die Verdrahtung für Host-Mode-Anfragen an
    /// (`shell.exec` mit `request_host`; nur TUI).
    #[must_use]
    pub fn with_host_escalation(
        mut self,
        escalation: Option<harw_tool_shell::HostEscalation>,
    ) -> Self {
        // Runde 5, Teil N: nur die TUI-Montage übergibt `Some`.
        self.host_escalation = escalation;
        self
    }

    /// Runde 5, Teil N: setzt die Obergrenze für `shell.exec`-Zeitlimits
    /// (`[shell] max_timeout_secs`, bereits geklemmt).
    #[must_use]
    pub fn with_shell_max_timeout_secs(mut self, max_timeout_secs: u64) -> Self {
        self.shell_max_timeout_secs = Some(max_timeout_secs);
        self
    }

    /// Hängt die Sendeseite des sudo-Fragekanals an (nur TUI, Runde 5 Teil B).
    #[must_use]
    pub fn with_sudo_prompts(mut self, sender: Option<harw_tool_shell::SudoPromptSender>) -> Self {
        self.sudo_prompts = sender;
        self
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
    // Reicht die einmal je Lauf instanziierte `HostPermitWiring` (Ledger +
    // Sitzungs-Registry + Fragekanal-Sender) durch, damit jeder gebaute
    // `ShellToolProvider` sowohl `run_command`s tatsächliche
    // `authorize()`-Prüfung (Host-Profil) als auch die sitzungs-/prozessweite
    // Freigabeprüfung über die Registry (jedes andere Profil, siehe
    // `harw_tool_shell::exec::ShellExecutor::determine_effective_host`)
    // erreichen kann. `None` verhält sich exakt wie vor dieser Ergänzung
    // (Host-Ausführung bleibt dann fail-closed ohne Ledger; ein
    // `sandbox-lease` auf einem Strict/Cargo/Tmux-Profil bleibt wirkungslos,
    // weil keine Registry angehängt ist, die die Freigabe sehen könnte).
    host_permits: Option<&HostPermitWiring>,
    // Die laufzeit-lebendige Freigabemodus-Zelle (Android-Anbindung): jeder
    // gebaute `ShellToolProvider` bekommt denselben Klon angehängt, damit
    // `harw_tool_shell` unter Android bei `ApprovalMode::FullAccess` ohne
    // Rückfrage auf dem Host ausführen kann. Unter Linux liest der Shell-
    // Provider die Zelle nicht (Verhalten unverändert), siehe
    // `harw_tool_shell::ShellToolProvider::with_approval_mode`.
    approval_mode: &ApprovalModeCell,
) -> Vec<Arc<dyn ToolProvider>> {
    // Baut einen `ShellToolProvider` für `sandbox_profile` und hängt, sobald
    // `host_permits` übergeben wurde, Ledger, Sitzungs-Registry,
    // Fragekanal-Sender und vorausgewählte Variante an — unabhängig davon,
    // ob `sandbox_profile.is_host()` gilt. Ein `sandbox-lease` soll
    // prozessweit wirken (Root-Session **und** jede Kind-Session, siehe
    // `HostPermitSessionRegistry::mark_global_approval`), also muss jeder
    // gebaute `ShellToolProvider` dieselbe Registry sehen, nicht nur der des
    // Host-Profil-Workers. Das ändert das Sicherheitsverhalten von
    // Strict/Cargo/Tmux **nicht**: `determine_effective_host`
    // (`harw-tool-shell/src/exec.rs`) prüft für ein nicht-Host-Profil
    // weiterhin ausschließlich, ob die angehängte Registry eine aktive
    // Sitzungs- oder Einmalfreigabe für die aufrufende Session meldet — ohne
    // eine solche Freigabe läuft der Aufruf unverändert in bwrap.
    let build_shell = |sandbox_profile: &SandboxProfile| -> ShellToolProvider {
        let mut provider = ShellToolProvider::default()
            .with_sandbox_profile(sandbox_profile.clone())
            .with_approval_mode(approval_mode.clone());
        if let Some(wiring) = host_permits {
            provider = provider
                .with_permit_ledger(Arc::clone(&wiring.ledger))
                .with_host_permit_registry(Arc::clone(&wiring.registry))
                .with_host_permit_prompts(wiring.prompt_sender.clone())
                .with_preselected_permit_variant(wiring.preselected_variant);
            // Runde 5, Teil N: Host-Mode-Anfrage nur mit TUI-Verdrahtung.
            if let Some(escalation) = &wiring.host_escalation {
                provider = provider.with_host_escalation(escalation.clone());
            }
            // Runde 5, Teil N: Obergrenze `[shell] max_timeout_secs`.
            if let Some(max_timeout_secs) = wiring.shell_max_timeout_secs {
                provider = provider.with_max_timeout_secs(max_timeout_secs);
            }
        }
        provider
    };
    // Plan R9, Teil F: `shell.exec` plus — mit Job-Verdrahtung — direkt
    // dahinter `job.*` über einem Klon **desselben** Shell-Providers
    // ([`JobWiring::tool_provider`]); Reihenfolge wie [`JOB_TOOLS`] hinter
    // [`SHELL_TOOLS`] in [`RegistryProfile::registered_tool_names`].
    let build_shell_providers = |sandbox_profile: &SandboxProfile| -> Vec<Arc<dyn ToolProvider>> {
        let shell = build_shell(sandbox_profile);
        let jobs = host_permits
            .and_then(|wiring| wiring.jobs.as_ref())
            .map(|jobs| Arc::new(jobs.tool_provider(&shell)) as Arc<dyn ToolProvider>);
        // R18 D-D: `shell.exec` startet immer in der Sandbox; eine genehmigte
        // Host-Eskalation meldet es selbst über `"executed_on": "host"`.
        // Die Hülle trägt genau diese Metadaten zur Fertigmeldung
        // (`ToolCallCompleted::placement`), damit die TUI `sandbox`/`host`
        // statt der Befehlszeile zeigen kann.
        let mut providers: Vec<Arc<dyn ToolProvider>> =
            vec![Arc::new(SandboxPlacedToolProvider::new(Arc::new(shell)))];
        providers.extend(jobs);
        providers
    };
    // Der read-only Anteil ist für drei Profile identisch: der gefilterte
    // FS-Provider plus der lesende Doc-Provider (`doc.read_pdf`, Anbindung
    // W3 — dieselbe Berechtigung wie `fs.read`, siehe `DOC_TOOLS`) plus der
    // vollständig lesende Deps-Provider.
    fn read_only_base() -> Vec<Arc<dyn ToolProvider>> {
        let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
            Arc::new(FsToolProvider::default()),
            FS_READ_ONLY_TOOLS,
        ));
        let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
        let explorer: Arc<dyn ToolProvider> = Arc::new(ExplorerToolProvider::new());
        let dependencies: Arc<dyn ToolProvider> = Arc::new(DepsToolProvider::new());
        vec![filesystem, doc, explorer, dependencies]
    }

    match profile {
        RegistryProfile::Full => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(FsToolProvider::default());
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            let explorer: Arc<dyn ToolProvider> = Arc::new(ExplorerToolProvider::new());
            let shell = build_shell_providers(sandbox_profile);
            let process: Arc<dyn ToolProvider> = Arc::new(ProcessToolProvider::new());
            let mut providers = vec![filesystem, doc, explorer];
            providers.extend(shell);
            providers.push(process);
            // Obsidian vault tools (read + write): coding agents maintain
            // the project's long-form memory alongside the code.
            providers.push(Arc::new(harw_tool_obsidian::ObsidianToolProvider::new()));
            providers
        }
        RegistryProfile::ShellExecution => build_shell_providers(sandbox_profile),
        // Der Web-Provider wird auf `EXPLORER_WEB_TOOLS` gefiltert, damit
        // `web.docs_rs`/`web.crates_io` nicht per Namensraten erreichbar sind.
        RegistryProfile::ReadOnlyExplore => {
            let mut providers = read_only_base();
            providers.push(Arc::new(RestrictedToolProvider::new(
                Arc::new(WebToolProvider::new()),
                EXPLORER_WEB_TOOLS,
            )));
            providers
        }
        // `LensToolProvider::new()` ist zustandslos (keine Bau-, Home- oder
        // Indexpfad-Konfiguration nötig): `derive_read_scope` leitet den
        // `ReadScope` beim Aufruf aus dem `ToolExecutionContext` ab, nie aus
        // einem Konstruktorargument. `profile.rs` muss ihm deshalb nichts
        // zusätzlich mitgeben.
        RegistryProfile::Planning => {
            let mut providers = read_only_base();
            // Obsidian vault reads: the planning agent maps the project's
            // long-form memory (`docs/planning` vault) the same way it reads
            // the workspace. Write stays out — planning is read-only.
            providers.push(Arc::new(harw_tool_obsidian::ObsidianToolProvider::new()));
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
        // Der volle, ungefilterte `FsToolProvider` (alle sieben `fs.*`, inklusive
        // `fs.write`) plus der lesende Doc-Provider — aber kein
        // `ShellToolProvider`. Siehe die Begründung bei
        // `RegistryProfile::MemoryStewardship`.
        RegistryProfile::MemoryStewardship => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(FsToolProvider::default());
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            vec![filesystem, doc]
        }
        // Schnellhelfer der UIA (Addendum I): gefilterter, lesender
        // FS-Provider + lesender Doc-Provider + voller Shell-Provider
        // (Sandbox+Freigabe greifen wie überall) + vollständig lesender
        // Deps-Provider + auf `web.fetch`/`web.search` gefilterter
        // Web-Provider.
        RegistryProfile::UiaQuickHelper => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(FsToolProvider::default()),
                FS_READ_ONLY_TOOLS,
            ));
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            let shell = build_shell_providers(sandbox_profile);
            let dependencies: Arc<dyn ToolProvider> = Arc::new(DepsToolProvider::new());
            let web: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(WebToolProvider::new()),
                UIA_HELPER_WEB_TOOLS,
            ));
            let mut providers = vec![filesystem, doc];
            providers.extend(shell);
            providers.push(dependencies);
            providers.push(web);
            providers
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
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            // Skill-Vorschläge liegen neben `<profil>/agents` unter
            // `<profil>/skills`; die Decke übernimmt Rolle und Werkzeuge der
            // Agentendefinitions-Decke (MCPs kennt dieser Pfad nicht —
            // fail-closed leer).
            let skill_proposals: Arc<dyn ToolProvider> = Arc::new(SkillProposalToolProvider::new(
                agent_definition_access
                    .profile_agents_dir
                    .as_ref()
                    .and_then(|dir| dir.parent())
                    .map(|profile| profile.join("skills")),
                agent_definition_access.mode,
                agent_definition_access
                    .ceiling
                    .as_ref()
                    .map(|ceiling| SkillAuthorCeiling {
                        role: ceiling.role,
                        tools: ceiling.tools.clone(),
                        mcps: Default::default(),
                    }),
            ));
            let agent_definitions: Arc<dyn ToolProvider> = Arc::new(
                crate::agent_definition_tools::AgentDefinitionToolProvider::new(
                    agent_definition_access.project_agents_dir,
                    agent_definition_access.profile_agents_dir,
                    agent_definition_access.mode,
                    agent_definition_access.ceiling,
                ),
            );
            vec![filesystem, doc, agent_definitions, skill_proposals]
        }
        // Read-only Erkundungsspezialisierung der UIA: gefilterter, lesender
        // FS-Provider + lesender Doc-Provider + Explorer-Provider + auf
        // `web.fetch`/`web.search` gefilterter Web-Provider — siehe die
        // Begründung bei
        // `RegistryProfile::UiaExplorer`.
        RegistryProfile::UiaExplorer => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(FsToolProvider::default()),
                FS_READ_ONLY_TOOLS,
            ));
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            let explorer: Arc<dyn ToolProvider> = Arc::new(ExplorerToolProvider::new());
            let web: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(WebToolProvider::new()),
                EXPLORER_WEB_TOOLS,
            ));
            vec![filesystem, doc, explorer, web]
        }
        // Schreibende Erkundungsspezialisierung der UIA: voller, ungefilterter
        // FS-Provider (alle sieben `fs.*`, inklusive `fs.write`) + lesender
        // Doc-Provider + vollständig lesender Deps-Provider + auf
        // `web.fetch`/`web.search` gefilterter Web-Provider — kein
        // `ShellToolProvider`. Siehe die Begründung bei
        // `RegistryProfile::UiaWriter`.
        RegistryProfile::UiaWriter => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(FsToolProvider::default());
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            let dependencies: Arc<dyn ToolProvider> = Arc::new(DepsToolProvider::new());
            let web: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(WebToolProvider::new()),
                UIA_HELPER_WEB_TOOLS,
            ));
            vec![filesystem, doc, dependencies, web]
        }
        // Host-Shell-Spezialisierung der UIA: gefilterter, lesender
        // FS-Provider + lesender Doc-Provider + Shell-Provider — anders als
        // `build_shell_provider` in den übrigen Zweigen hängt dieser Zweig
        // ausdrücklich `SandboxProfile::Host` an, nicht das von der Runtime
        // übergebene `sandbox_profile` — siehe die Begründung bei
        // `RegistryProfile::UiaShellWorker`. Ledger und Sitzungs-Registry
        // hängt `build_shell_provider` immer an, sobald `host_permits`
        // übergeben wurde (unabhängig vom Profil); für dieses `Host`-Profil
        // greift zusätzlich `authorize_host_command` bei jedem Aufruf.
        RegistryProfile::UiaShellWorker => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(FsToolProvider::default()),
                FS_READ_ONLY_TOOLS,
            ));
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            let mut providers = vec![filesystem, doc];
            providers.extend(build_shell_providers(&SandboxProfile::Host));
            providers
        }
        // Lesende Recherche mit Netz: gefilterter, lesender FS-Provider +
        // lesender Doc-Provider + Explorer-Provider + auf
        // `web.fetch`/`web.search` gefilterter Web-Provider — siehe die
        // Begründung bei `RegistryProfile::ReadOnlyResearch`.
        RegistryProfile::ReadOnlyResearch => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(FsToolProvider::default()),
                FS_READ_ONLY_TOOLS,
            ));
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            let explorer: Arc<dyn ToolProvider> = Arc::new(ExplorerToolProvider::new());
            let web: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(WebToolProvider::new()),
                EXPLORER_WEB_TOOLS,
            ));
            vec![filesystem, doc, explorer, web]
        }
        // Workspace lesen und schreiben ohne Shell und ohne Netz: voller,
        // ungefilterter FS-Provider (alle sieben `fs.*`, inklusive
        // `fs.write`) + lesender Doc-Provider + Explorer-Provider + auf
        // `deps.graph`/`deps.locked` gefilterter Deps-Provider — kein
        // `ShellToolProvider`, kein `WebToolProvider`. Siehe die Begründung
        // bei `RegistryProfile::WorkspaceEdit`.
        RegistryProfile::WorkspaceEdit => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(FsToolProvider::default());
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            let explorer: Arc<dyn ToolProvider> = Arc::new(ExplorerToolProvider::new());
            let dependencies: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(DepsToolProvider::new()),
                DEPS_WORKSPACE_TOOLS,
            ));
            vec![filesystem, doc, explorer, dependencies]
        }
        // Nur lesende Unterlagen: gefilterter, lesender FS-Provider +
        // lesender Doc-Provider — kein Explorer-, Deps-, Web- oder
        // Shell-Provider. Siehe die Begründung bei
        // `RegistryProfile::MatrixReader`.
        RegistryProfile::MatrixReader => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(RestrictedToolProvider::new(
                Arc::new(FsToolProvider::default()),
                FS_READ_ONLY_TOOLS,
            ));
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            vec![filesystem, doc]
        }
        // LaTeX-Schreibspezialisierung der UIA: voller, ungefilterter
        // FS-Provider (alle sieben `fs.*`, inklusive `fs.write`) + lesender
        // Doc-Provider + `LatexToolProvider` (`latex.build`, festes argv in
        // der Bubblewrap-Sandbox, nie Host) — kein Shell-, Web-, Deps- oder
        // Explorer-Provider. Siehe die Begründung bei
        // `RegistryProfile::UiaLatexWriter`.
        RegistryProfile::UiaLatexWriter => {
            let filesystem: Arc<dyn ToolProvider> = Arc::new(FsToolProvider::default());
            let doc: Arc<dyn ToolProvider> = Arc::new(DocToolProvider);
            let latex: Arc<dyn ToolProvider> = Arc::new(LatexToolProvider::new());
            vec![filesystem, doc, latex]
        }
    }
}

/// Meldet für jedes Werkzeug eines lokal sandboxierten Providers die
/// Platzierung `sandbox` (R18 D-D, Vertrag
/// `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §5).
///
/// # Beschreibung
/// Jeder Ausführer des inneren Providers wird in
/// [`harw_tools::executor::PlacedToolExecutor`] gehüllt; eine genehmigte
/// Host-Eskalation (`"executed_on": "host"`) wird daraus beim Melden zu
/// `host` ([`harw_tools::executor::ExecutionPlacement::resolve_local`]).
/// Werkzeugliste und Parallelitätszusage bleiben unverändert.
///
/// # Nebenläufigkeit
/// Thread-sicher; kein veränderlicher Zustand.
pub struct SandboxPlacedToolProvider {
    /// Der sandboxierte Provider (heute `ShellToolProvider`).
    inner: Arc<dyn ToolProvider>,
}

impl SandboxPlacedToolProvider {
    /// Hüllt `inner`.
    #[must_use]
    pub fn new(inner: Arc<dyn ToolProvider>) -> Self {
        Self { inner }
    }
}

impl ToolProvider for SandboxPlacedToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        self.inner.tools()
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        self.inner.executor(name).map(|executor| {
            Arc::new(harw_tools::executor::PlacedToolExecutor::new(
                executor,
                harw_tools::executor::ExecutionPlacement::Sandbox,
            )) as Arc<dyn ToolExecutor>
        })
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        self.inner.parallel_safe(name)
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
/// `agents.validate`/`agents.list_proposals` (plus `skills.validate`/
/// `skills.list_proposals`), unabhängig vom Profil. Wer eine
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
/// Alle drei Werte werden an **jeden** gebauten [`ShellToolProvider`]
/// gehängt, unabhängig davon, ob dessen `sandbox_profile` tatsächlich
/// [`SandboxProfile::Host`] ist — ein `sandbox-lease` soll prozessweit gelten
/// (Root-Session und jede Kind-Session, siehe
/// [`harw_sandbox::HostPermitSessionRegistry::mark_global_approval`]), also
/// muss jeder Shell-Provider dieselbe Registry sehen können, um eine aktive
/// Freigabe zu erkennen. Für Strict/Cargo/Tmux ändert das die
/// Sicherheitsgrenze **nicht**: ohne eine aktive Sitzungs- oder
/// Einmalfreigabe in der Registry bleibt die Sandbox weiterhin die Grenze
/// (`ShellExecutor::determine_effective_host`, `harw-tool-shell/src/exec.rs`,
/// prüft für ein nicht-Host-Profil ausschließlich die Registry, nie den
/// Permit-Ledger selbst). Die Runtime muss über beide Aufrufstellen (Host-
/// und Nicht-Host-Zweig, siehe `harw-runtime/src/assembly.rs`) **dieselbe**
/// `Arc`-Instanz von Ledger und Registry sowie denselben Sender-Klon
/// durchreichen: ein zweiter, unabhängig instanziierter Ledger hätte keine
/// Kenntnis von den bereits gemerkten Sitzungszustimmungen und würde jede
/// Host-Ausführung erneut ablehnen; ein anderer Sender ließe die Frage nie
/// beim Empfänger ankommen, den die Runtime tatsächlich pollt.
///
/// `None` verhält sich exakt wie
/// [`assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile`]:
/// Host-Ausführung bleibt dann fail-closed, weil kein Ledger konfiguriert ist,
/// und kein Shell-Provider kann eine `sandbox-lease`-Freigabe überhaupt sehen.
///
/// # Argumente
/// - `profile` ([`RegistryProfile`]): Werkzeug-/Identitätsprofil.
/// - `project` (`&ProjectContext`): bereits erkannter Projektkontext.
/// - `overrides` ([`IdentityOverrides`]): Überschreibungen für den System-Prompt.
/// - `approval_mode` ([`ApprovalModeCell`]): Freigabemodus-Zelle der Politik;
///   wird zusätzlich (als Klon) an jeden gebauten [`ShellToolProvider`]
///   gereicht ([`ShellToolProvider::with_approval_mode`]), damit dieser unter
///   Android bei `ApprovalMode::FullAccess` ohne Rückfrage auf dem Host
///   ausführen kann — unter Linux ändert das das Verhalten nicht.
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
/// entsteht, an den es gehängt werden könnte. Für jedes Profil, das einen
/// [`ShellToolProvider`] baut, hängt `host_permits` (sobald übergeben) immer
/// an — `UiaShellWorker` hängt seinen `ShellToolProvider` zusätzlich immer
/// an `SandboxProfile::Host` (unabhängig vom übergebenen `sandbox_profile`),
/// sodass dessen `authorize_host_command`-Pfad zusätzlich zur Registry
/// **immer** greift, sobald `host_permits` übergeben wird.
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
        project_agents_dir: Some(
            project
                .project_root
                .join(harw_home::project_dir_name())
                .join("agents"),
        ),
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
            .chain(DOC_TOOLS.iter())
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
        &approval_mode,
    )
    .into_iter()
    .filter_map(|provider| restrict_provider(provider, &allowed))
    .collect();

    // Beworben wird nur, wofür tatsächlich ein Provider-Werkzeug registriert
    // ist: `allowed` ist die statische Vertrags-Obermenge (etwa
    // `browser.open` bei `UiaQuickHelper`, das ohne `BrowserOpenGrant` keinen
    // Provider hat). Ein beworbenes Werkzeug ohne Executor schickte das
    // Modell in einen sicheren Fehlschlag.
    let registered: Vec<ToolSpec> = providers
        .iter()
        .flat_map(|provider| provider.tools())
        .collect();
    let advertised_tools: Vec<String> = allowed
        .iter()
        .filter(|name| registered.iter().any(|spec| spec.name() == **name))
        .map(|name| (*name).to_owned())
        .collect();

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
    use crate::test_support::{TestError, TestResult, ctx};

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

    fn assemble(profile: RegistryProfile) -> TestResult<AssembledRegistry> {
        let cwd = std::env::current_dir().map_err(ctx("cwd"))?;
        assemble_registry(profile, cwd, IdentityOverrides::default()).map_err(ctx("assemble"))
    }

    /// R18 D-D (RP-T6): `shell.exec` trägt die Platzierungs-Metadaten
    /// `sandbox` (die Host-Eskalation leitet `resolve_local` daraus ab);
    /// ein Werkzeug ohne Metadaten bleibt „unbekannt“ statt `host`.
    #[test]
    fn test_shell_exec_carries_sandbox_placement_metadata() -> TestResult {
        use harw_tools::executor::ExecutionPlacement;

        for profile in [RegistryProfile::Full, RegistryProfile::ShellExecution] {
            let assembled = assemble(profile)?;
            let executor_of = |name: &str| {
                assembled
                    .registry
                    .tool_providers()
                    .iter()
                    .find_map(|provider| provider.executor(&ToolName::new(name)))
            };
            let shell = executor_of("shell.exec").ok_or(TestError::Missing("shell.exec"))?;
            assert_eq!(
                shell.placement(),
                Some(ExecutionPlacement::Sandbox),
                "{profile:?}"
            );
            if profile == RegistryProfile::Full {
                let read = executor_of("fs.read").ok_or(TestError::Missing("fs.read"))?;
                assert_eq!(read.placement(), None);
            }
        }
        Ok(())
    }

    #[test]
    fn test_registered_tool_names_matches_actually_registered_tools_for_every_profile() -> TestResult
    {
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
            let assembled = assemble(*profile)?;
            // Plan R9, Teil F: `job.*` montiert nur eine `JobWiring`;
            // `assemble()` hat keine (siehe
            // `test_job_tools_follow_shell_exec_with_job_wiring`).
            let expected: Vec<String> = profile
                .registered_tool_names()
                .iter()
                .filter(|name| !JOB_TOOLS.contains(name))
                .map(|name| (*name).to_owned())
                .collect();
            assert_eq!(
                registered_names(&assembled),
                expected,
                "{profile:?}: die statische Liste muss der Registry entsprechen"
            );
        }
        Ok(())
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
    fn test_uia_quick_helper_does_not_yet_build_a_runtime_browser_provider() -> TestResult {
        let assembled = assemble(RegistryProfile::UiaQuickHelper)?;
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
        Ok(())
    }

    /// Nachtrag K3: ohne eine gesetzte [`AgentDefinitionAccess`] registriert
    /// `AgentDefinitionToolProvider` fail-closed nur `agents.validate`/
    /// `agents.list_proposals` — unabhängig davon, dass
    /// `RegistryProfile::AgentStewardship::registered_tool_names()` weiterhin
    /// die volle Vertrags-Obermenge zurückgibt (siehe der Test oben).
    #[test]
    fn test_agent_stewardship_registers_only_read_and_list_without_access() -> TestResult {
        let assembled = assemble(RegistryProfile::AgentStewardship)?;
        let mut expected: Vec<String> = FS_READ_ONLY_TOOLS
            .iter()
            .chain(DOC_TOOLS.iter())
            .map(|name| (*name).to_owned())
            .collect();
        expected.push("agents.validate".to_owned());
        expected.push("agents.list_proposals".to_owned());
        expected.push("skills.validate".to_owned());
        expected.push("skills.list_proposals".to_owned());
        assert_eq!(registered_names(&assembled), expected);
        assert_eq!(assembled.identity.tools_available, expected);
        Ok(())
    }

    /// Nachtrag K3: mit einer gesetzten Decke und `DefinitionWriteMode::Commit`
    /// registriert derselbe Provider die volle Vertrags-Obermenge — genau die
    /// Werkzeugmenge, die `agent-steward.toml` admittiert (siehe
    /// `tests/tool_admission_coverage.rs`).
    #[test]
    fn test_agent_stewardship_registers_all_six_tools_with_a_commit_ceiling() -> TestResult {
        use crate::agent_definition_tools::DefinitionAuthorCeiling;
        use harw_agent_dsl::roles::AgentRoleId;

        let cwd = std::env::current_dir().map_err(ctx("cwd"))?;
        let project = discover_project(&cwd, &DiscoveryConfig::default())
            .map_err(ctx("Discovery im Workspace"))?;
        let access = AgentDefinitionAccess {
            project_agents_dir: Some(
                project
                    .project_root
                    .join(harw_home::project_dir_name())
                    .join("agents"),
            ),
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
        .map_err(ctx("assemble"))?;

        let mut expected: Vec<String> = FS_READ_ONLY_TOOLS
            .iter()
            .chain(DOC_TOOLS.iter())
            .map(|name| (*name).to_owned())
            .collect();
        expected.extend(AGENT_DEFINITION_TOOLS.iter().map(|name| (*name).to_owned()));
        assert_eq!(registered_names(&assembled), expected);
        assert_eq!(assembled.identity.tools_available, expected);
        Ok(())
    }

    #[test]
    fn test_read_only_explore_exposes_exact_tool_set() -> TestResult {
        let assembled = assemble(RegistryProfile::ReadOnlyExplore)?;
        assert_eq!(
            registered_names(&assembled),
            vec![
                "fs.read",
                "fs.list",
                "fs.search",
                "fs.glob",
                "fs.grep",
                "doc.read_pdf",
                "explore.tree",
                "explore.projects",
                "explore.relations",
                "explore.find",
                "deps.graph",
                "deps.locked",
                "deps.source_read",
                "deps.source_search",
                "deps.source_list",
                // Nutzerentscheidung: der Explorer darf auch das Netz
                // durchsuchen — nur diese zwei, nie `web.docs_rs`/`web.crates_io`.
                "web.fetch",
                "web.search",
            ]
        );
        Ok(())
    }

    /// Nutzerentscheidung „UIA-Helfer recherchieren kurz online und fügen
    /// manchmal Abhängigkeiten hinzu“: `UiaQuickHelper` (`uia-worker`) und
    /// `UiaWriter` (`uia-writer`) registrieren tatsächlich die fünf lesenden
    /// `deps.*`-Werkzeuge und alle vier `web.*`-Werkzeuge (seit der
    /// Nutzerentscheidung auch `web.docs_rs`/`web.crates_io`) — nie
    /// `lens.ask` — und brauchen dafür genau `ReadCargoRegistry` und
    /// `NetworkAccess` zusätzlich.
    #[test]
    fn test_uia_helpers_register_web_search_and_read_only_deps_tools() -> TestResult {
        use harw_authority::Permission;

        const DEPS_AND_WEB: [&str; 9] = [
            "deps.graph",
            "deps.locked",
            "deps.source_read",
            "deps.source_search",
            "deps.source_list",
            "web.fetch",
            "web.docs_rs",
            "web.crates_io",
            "web.search",
        ];
        let quick = registered_names(&assemble(RegistryProfile::UiaQuickHelper)?);
        // Ohne `JobWiring` (`assemble`) montiert kein Profil `job.*`.
        let mut expected_quick: Vec<&str> = FS_READ_ONLY_TOOLS
            .iter()
            .chain(DOC_TOOLS.iter())
            .chain(SHELL_TOOLS.iter())
            .copied()
            .collect();
        expected_quick.extend(DEPS_AND_WEB);
        assert_eq!(quick, expected_quick);

        let writer = registered_names(&assemble(RegistryProfile::UiaWriter)?);
        let mut expected_writer: Vec<&str> = FS_FULL_TOOLS
            .iter()
            .chain(DOC_TOOLS.iter())
            .copied()
            .collect();
        expected_writer.extend(DEPS_AND_WEB);
        assert_eq!(writer, expected_writer);

        for profile in [RegistryProfile::UiaQuickHelper, RegistryProfile::UiaWriter] {
            let names = profile.registered_tool_names();
            assert!(!names.contains(&"lens.ask"), "{profile:?}: lens.ask");
            let required = profile.required_permissions();
            assert!(required.contains(Permission::NetworkAccess), "{profile:?}");
            assert!(
                required.contains(Permission::ReadCargoRegistry),
                "{profile:?}"
            );
        }
        Ok(())
    }

    /// A5: Die einzige Rolle mit Netz registriert **nur** `web.*` — kein
    /// `fs.*`, kein `deps.*`, also nichts, womit sie Workspace-Daten lesen und
    /// über `web.fetch` hinaustragen könnte.
    #[test]
    fn test_research_profile_registers_only_the_web_tools() -> TestResult {
        let assembled = assemble(RegistryProfile::Research)?;
        let names = registered_names(&assembled);
        let expected: Vec<String> = WEB_TOOLS.iter().map(|tool| (*tool).to_owned()).collect();
        assert_eq!(names, expected);
        assert!(!names.iter().any(|name| name.starts_with("fs.")));
        assert!(!names.iter().any(|name| name.starts_with("deps.")));
        assert_eq!(assembled.identity.tools_available, expected);
        Ok(())
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
            set(&[
                Permission::ReadWorkspace,
                Permission::ReadCargoRegistry,
                Permission::NetworkAccess
            ])
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
        assert_eq!(
            RegistryProfile::WorkspaceEdit.required_permissions(),
            set(&[Permission::ReadWorkspace, Permission::WriteWorkspace])
        );
        assert_eq!(
            RegistryProfile::MatrixReader.required_permissions(),
            set(&[Permission::ReadWorkspace])
        );
    }

    /// `MatrixReader` (Runde 3, Matrix-Unterlagen): exakt die lesenden
    /// `fs.*` plus `doc.read_pdf`, read-only, registriert wie beworben.
    #[test]
    fn test_matrix_reader_profile_exact_tool_surface() -> TestResult {
        let expected = vec![
            "fs.read",
            "fs.list",
            "fs.search",
            "fs.glob",
            "fs.grep",
            "doc.read_pdf",
        ];
        assert_eq!(RegistryProfile::MatrixReader.tool_names(), expected);
        assert!(RegistryProfile::MatrixReader.is_read_only());
        let assembled = assemble(RegistryProfile::MatrixReader)?;
        assert_eq!(registered_names(&assembled), expected);
        Ok(())
    }

    /// `WorkspaceEdit` (Runde 3, Welle D): exakt `fs.*` inklusive
    /// `fs.write`, `doc.read_pdf`, `explore.*` und `deps.graph`/
    /// `deps.locked` — nie `shell.*`, `process.*`, `web.*`, `lens.ask`,
    /// `browser.*` oder `deps.source_*`; registriert wie beworben.
    #[test]
    fn test_workspace_edit_profile_exact_tool_surface() -> TestResult {
        let expected = vec![
            "fs.read",
            "fs.write",
            "fs.edit",
            "fs.list",
            "fs.search",
            "fs.glob",
            "fs.grep",
            "doc.read_pdf",
            "explore.tree",
            "explore.projects",
            "explore.relations",
            "explore.find",
            "deps.graph",
            "deps.locked",
        ];
        assert_eq!(RegistryProfile::WorkspaceEdit.tool_names(), expected);
        for tool in RegistryProfile::WorkspaceEdit.tool_names() {
            assert!(
                !tool.starts_with("shell.")
                    && !tool.starts_with("process.")
                    && !tool.starts_with("web.")
                    && !tool.starts_with("browser.")
                    && !tool.starts_with("deps.source_")
                    && tool != "lens.ask",
                "WorkspaceEdit darf {tool} nicht registrieren"
            );
        }
        let assembled = assemble(RegistryProfile::WorkspaceEdit)?;
        let expected_owned: Vec<String> = expected.iter().map(|t| (*t).to_owned()).collect();
        assert_eq!(registered_names(&assembled), expected_owned);
        assert_eq!(assembled.identity.tools_available, expected_owned);
        Ok(())
    }

    /// `UiaLatexWriter` (Runde 4, Teil E; Runde 7, Teil T5): exakt `fs.*`
    /// inklusive `fs.write`, `doc.read_pdf`, `latex.build`, `latex.template`
    /// und `latex.check` — nie `shell.*`, `process.*`, `web.*`, `deps.*`,
    /// `explore.*`, `lens.ask`; die montierte Registry trägt tatsächlich
    /// einen Executor für jedes LaTeX-Werkzeug.
    #[test]
    fn test_uia_latex_writer_profile_exact_tool_surface() -> TestResult {
        let expected = vec![
            "fs.read",
            "fs.write",
            "fs.edit",
            "fs.list",
            "fs.search",
            "fs.glob",
            "fs.grep",
            "doc.read_pdf",
            "latex.build",
            "latex.template",
            "latex.check",
        ];
        assert_eq!(RegistryProfile::UiaLatexWriter.tool_names(), expected);
        assert_eq!(
            profile_for_role(role_names::UIA_LATEX_WRITER),
            Some(RegistryProfile::UiaLatexWriter)
        );
        let assembled = assemble(RegistryProfile::UiaLatexWriter)?;
        let expected_owned: Vec<String> = expected.iter().map(|t| (*t).to_owned()).collect();
        assert_eq!(registered_names(&assembled), expected_owned);
        for tool in ["latex.build", "latex.template", "latex.check"] {
            assert!(
                assembled
                    .registry
                    .tool_providers()
                    .iter()
                    .any(|provider| provider.executor(&ToolName::new(tool)).is_some()),
                "{tool} braucht einen Executor"
            );
        }
        Ok(())
    }

    /// Die Wurzel der UIA-Sitzung (Einstieg `Tui` → `RegistryProfile::Full`)
    /// registriert nie ein `web.*`-Werkzeug — Netz-Recherche bleibt den
    /// UIA-Helfern vorbehalten, auch wenn die Wurzel egress-gebundenes Netz
    /// trägt (Runde 3, Welle A).
    #[test]
    fn test_uia_root_profile_full_registers_no_web_tools() -> TestResult {
        let advertised = RegistryProfile::Full.tool_names();
        assert!(
            !advertised.iter().any(|tool| tool.starts_with("web.")),
            "Full bewirbt web.*: {advertised:?}"
        );
        let assembled = assemble(RegistryProfile::Full)?;
        assert!(
            !registered_names(&assembled)
                .iter()
                .any(|tool| tool.starts_with("web.")),
            "Full registriert web.*"
        );
        Ok(())
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
                "doc.read_pdf",
                "explore.tree",
                "explore.projects",
                "explore.relations",
                "explore.find",
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
    fn test_assemble_registry_for_sandbox_registers_and_advertises_only_granted_tools() -> TestResult
    {
        use harw_authority::Permission;

        let cwd = std::env::current_dir().map_err(ctx("cwd"))?;
        let project = discover_project(&cwd, &DiscoveryConfig::default())
            .map_err(ctx("Discovery im Workspace"))?;
        let granted = PermissionSet::from_policy([Permission::ReadWorkspace]);
        let assembled = assemble_registry_for_sandbox(
            RegistryProfile::ReadOnlyExplore,
            &project,
            IdentityOverrides::default(),
            ApprovalModeCell::default(),
            &granted,
        )
        .map_err(ctx("assemble"))?;

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
        .map_err(ctx("assemble"))?;
        assert!(research.registry.tool_providers().is_empty());
        assert!(research.identity.tools_available.is_empty());
        Ok(())
    }

    #[test]
    fn test_planning_profile_registers_read_only_tools_and_does_not_advertise_plan_or_goal()
    -> TestResult {
        let assembled = assemble(RegistryProfile::Planning)?;
        // Registriert wird der read-only Kern von `ReadOnlyExplore` (fs.*,
        // doc.read_pdf, explore.*, deps.*) **ohne** dessen Explorer-Netz
        // (`web.fetch`/`web.search`, nur für `explorer`), plus `lens.ask`
        // (siehe `LENS_TOOLS`) plus die lesenden Obsidian-Werkzeuge (siehe
        // `OBSIDIAN_READ_TOOLS`).
        let expected: Vec<String> = FS_READ_ONLY_TOOLS
            .iter()
            .chain(DOC_TOOLS.iter())
            .chain(EXPLORER_TOOLS.iter())
            .chain(DEPS_TOOLS.iter())
            .chain(OBSIDIAN_READ_TOOLS.iter())
            .chain(LENS_TOOLS.iter())
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(registered_names(&assembled), expected);
        assert!(
            !expected.iter().any(|name| name.starts_with("web.")),
            "Planning (root-orchestrator, planner) führt kein Netz-Werkzeug"
        );
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
        Ok(())
    }

    #[test]
    fn test_only_planner_profile_registers_lens_ask() -> TestResult {
        // `LensToolProvider` erscheint in der zusammengestellten Registry —
        // der Beleg, dass Lens jetzt einen echten Konsumenten hat.
        let planning = assemble(RegistryProfile::Planning)?;
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
            let assembled = assemble(*profile)?;
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
        Ok(())
    }

    #[test]
    fn test_read_only_profiles_never_expose_write_or_shell_tools() -> TestResult {
        for profile in RegistryProfile::ALL.iter().filter(|p| p.is_read_only()) {
            let assembled = assemble(*profile)?;
            let names = registered_names(&assembled);
            for forbidden in ["fs.write", "fs.edit", "shell.exec"] {
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
        Ok(())
    }

    #[test]
    fn test_identity_advertises_exactly_the_profile_tool_names() -> TestResult {
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
            let assembled = assemble(*profile)?;
            // Plan R9, Teil F: ohne `JobWiring` wird `job.*` nicht beworben.
            let expected: Vec<String> = profile
                .tool_names()
                .iter()
                .filter(|name| !JOB_TOOLS.contains(name))
                .map(|name| (*name).to_owned())
                .collect();
            assert_eq!(
                assembled.identity.tools_available, expected,
                "{profile:?}: der Prompt darf nur Profil-Werkzeuge bewerben"
            );
        }
        Ok(())
    }

    #[test]
    fn test_identity_uses_profile_role_description_by_default() -> TestResult {
        for profile in RegistryProfile::ALL {
            let assembled = assemble(*profile)?;
            assert_eq!(
                assembled.identity.role_description,
                profile.role_description()
            );
        }
        Ok(())
    }

    #[test]
    fn test_identity_overrides_replace_name_role_and_context() -> TestResult {
        let cwd = std::env::current_dir().map_err(ctx("cwd"))?;
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
        .map_err(ctx("assemble"))?;

        assert_eq!(assembled.identity.agent_name, "explorer-3");
        assert_eq!(assembled.identity.role_description, "focused explorer");
        assert_eq!(
            assembled.identity.extra_context,
            vec!["Antworte nur mit JSON."]
        );
        Ok(())
    }

    /// Legt ein leeres Projektverzeichnis unter `std::env::temp_dir()` an und
    /// setzt einen `Cargo.toml`-Marker hinein, damit `discover_project` genau
    /// dieses Verzeichnis als Projektwurzel erkennt.
    ///
    /// Der Name ist über Prozess-ID und Nanosekunden eindeutig — kein
    /// gemeinsamer Zähler, kein anderer geteilter Zustand, damit parallel
    /// laufende Tests einander nicht sehen.
    fn make_temp_project(tag: &str) -> TestResult<PathBuf> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("Systemzeit liegt vor der Unix-Epoche"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-registry-defaults-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("Projektverzeichnis anlegen"))?;
        std::fs::write(root.join("Cargo.toml"), b"[package]\nname = \"tmp\"\n")
            .map_err(ctx("Projektmarker schreiben"))?;
        Ok(root)
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
    fn test_assemble_registry_for_project_never_runs_discovery_again() -> TestResult {
        let root = make_temp_project("no-rediscovery")?;

        let project = discover_project(&root, &DiscoveryConfig::default())
            .map_err(ctx("Discovery im Tempdir"))?;
        assert_eq!(
            project.project_root, project.cwd,
            "der Marker liegt im Wurzelverzeichnis selbst"
        );

        std::fs::remove_dir_all(&root).map_err(ctx("Projektverzeichnis entfernen"))?;
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
        .map_err(ctx("erste Montage kommt ohne Discovery aus"))?;
        let second = assemble_registry_for_project(
            RegistryProfile::ReadOnlyExplore,
            &project,
            IdentityOverrides::default(),
            mode.clone(),
        )
        .map_err(ctx("zweite Montage kommt ohne Discovery aus"))?;

        assert_eq!(first.project.project_root, project.project_root);
        assert_eq!(second.project.project_root, project.project_root);
        assert_eq!(first.identity.cwd, second.identity.cwd);
        assert_eq!(registered_names(&first), registered_names(&second));
        assert_eq!(first.registry.approval_handlers().len(), 1);
        assert_eq!(second.registry.approval_handlers().len(), 1);
        Ok(())
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
        assert!(provider.executor(&ToolName::new("fs.edit")).is_none());
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
        for role in role_names::MATRIX_ROLES {
            assert!(role_names::ALL.contains(&role), "{role} fehlt in ALL");
            assert_eq!(
                profile_for_role(role),
                Some(RegistryProfile::MatrixReader),
                "Matrix-Rolle {role} muss MatrixReader bekommen"
            );
        }
        for role in role_names::ALL {
            assert!(profile_for_role(role).is_some(), "unbekannt: {role}");
        }
        // Plan Punkt 1: Root- und Child-Orchestratoren teilen die read-only
        // Planungsoberfläche ohne Netz.
        for role in [role_names::ROOT_ORCHESTRATOR]
            .iter()
            .chain(role_names::CHILD_ORCHESTRATORS.iter())
        {
            assert_eq!(
                profile_for_role(role),
                Some(RegistryProfile::Planning),
                "Orchestrator {role} muss Planning bekommen"
            );
        }
    }

    /// `delegate_wave` steuert die Composition-Root bei — ausschließlich für
    /// Orchestratoren, nie für Worker oder UIA-Rollen, und nie als Teil eines
    /// Registry-Profils (sonst bewürbe `Planning` es auch dem `planner`).
    #[test]
    fn test_composition_tools_are_granted_only_to_orchestrators() {
        let orchestrators: Vec<&str> = std::iter::once(role_names::ROOT_ORCHESTRATOR)
            .chain(role_names::CHILD_ORCHESTRATORS.iter().copied())
            .collect();
        for role in role_names::ALL {
            let expected: &[&str] = if orchestrators.contains(role) {
                ORCHESTRATION_TOOLS
            } else {
                &[]
            };
            assert_eq!(composition_tools_for_role(role), expected, "{role}");
            assert_eq!(is_orchestrator_role(role), orchestrators.contains(role));
        }
        for profile in RegistryProfile::ALL {
            for tool in ORCHESTRATION_TOOLS {
                assert!(
                    !profile.registered_tool_names().contains(tool),
                    "{profile:?} darf {tool} nicht selbst registrieren"
                );
            }
        }
        assert!(composition_tools_for_role("unbekannt").is_empty());
    }

    /// Runde 5, Teil B: `host.sudo_exec` bekommen ausschließlich die beiden
    /// Host-Shell-Worker; kein Profil registriert es selbst, es braucht
    /// `ExecuteProcess` und fragt immer (`ALWAYS_ASK_TOOLS`), und nie ist es
    /// automatisch freigegeben.
    #[test]
    fn test_sudo_tools_are_offered_only_to_the_two_host_shell_workers() {
        for role in role_names::ALL {
            let expected: &[&str] = if *role == role_names::UIA_SHELL_WORKER {
                SUDO_TOOLS
            } else {
                &[]
            };
            assert_eq!(sudo_tools_for_role(role), expected, "{role}");
        }
        assert_eq!(sudo_tools_for_role("host-process-worker"), SUDO_TOOLS);
        assert!(sudo_tools_for_role("sandbox-shell-worker").is_empty());
        assert!(sudo_tools_for_role("unbekannt").is_empty());
        for profile in RegistryProfile::ALL {
            for tool in SUDO_TOOLS {
                assert!(
                    !profile.registered_tool_names().contains(tool),
                    "{profile:?} darf {tool} nicht selbst registrieren"
                );
            }
        }
        for tool in SUDO_TOOLS {
            assert_eq!(
                crate::authority::tool_permission(tool),
                Some(harw_authority::Permission::ExecuteProcess)
            );
            assert!(crate::ALWAYS_ASK_TOOLS.contains(tool), "{tool}");
            assert!(!crate::AUTO_APPROVED_TOOLS.contains(tool), "{tool}");
        }
    }

    /// Ohne ausdrücklichen Aufruf trägt die Host-Permit-Verdrahtung keinen
    /// sudo-Kanal (fail-closed für jeden Nicht-TUI-Einstieg).
    #[test]
    fn test_host_permit_wiring_has_no_sudo_channel_by_default() {
        let (sender, _receiver) = harw_tool_shell::host_permit_prompt_channel();
        let wiring = HostPermitWiring::new(
            Arc::new(ProcessPermitLedger::default()),
            Arc::new(HostPermitSessionRegistry::default()),
            sender,
        );
        assert!(wiring.sudo_prompts.is_none());
        let (sudo, _sudo_receiver) = harw_tool_shell::sudo_prompt_channel();
        assert!(wiring.with_sudo_prompts(Some(sudo)).sudo_prompts.is_some());
    }

    /// Plan R9, Teil F: jedes Profil mit `shell.exec` bewirbt direkt dahinter
    /// die sechs `job.*`-Werkzeuge; kein Profil ohne Shell tut das.
    #[test]
    fn test_shell_profiles_carry_job_tools_right_after_shell_exec() {
        for profile in RegistryProfile::ALL {
            let names = profile.registered_tool_names();
            match names.iter().position(|name| *name == "shell.exec") {
                Some(index) => assert_eq!(
                    &names[index + 1..index + 1 + JOB_TOOLS.len()],
                    JOB_TOOLS,
                    "{profile:?}"
                ),
                None => {
                    for tool in JOB_TOOLS {
                        assert!(!names.contains(tool), "{profile:?}: {tool} ohne Shell");
                    }
                }
            }
        }
        // Orchestratoren (ohne Shell) bekommen nur die Kontrollwerkzeuge.
        for role in role_names::ALL {
            let expected: &[&str] = if is_orchestrator_role(role) {
                JOB_CONTROL_TOOLS
            } else {
                &[]
            };
            assert_eq!(job_control_tools_for_role(role), expected, "{role}");
        }
        assert!(!JOB_CONTROL_TOOLS.contains(&"job.start"));
    }

    /// Plan R9, Teil F: mit `JobWiring` montiert jedes Shell-Profil `job.*`
    /// direkt hinter `shell.exec` — die statische Liste stimmt dann exakt
    /// mit der Registry überein.
    #[test]
    fn test_job_tools_follow_shell_exec_with_job_wiring() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let manager = harw_tool_job::JobManager::new(
            harw_tool_job::JobManagerConfig::new(dir.path()),
            Arc::new(harw_tool_job::NoopNotifier),
        )
        .map_err(ctx("job manager"))?;
        let (sender, _receiver) = harw_tool_shell::host_permit_prompt_channel();
        let wiring = HostPermitWiring::new(
            Arc::new(ProcessPermitLedger::default()),
            Arc::new(HostPermitSessionRegistry::default()),
            sender,
        )
        .with_jobs(Some(JobWiring::new(
            manager,
            Arc::new(harw_tool_job::NoLineage),
        )));
        let cwd = std::env::current_dir().map_err(ctx("cwd"))?;
        let project = harw_project_discovery::discover_project(
            &cwd,
            &harw_project_discovery::DiscoveryConfig::default(),
        )
        .map_err(ctx("discover"))?;
        for profile in [
            RegistryProfile::Full,
            RegistryProfile::ShellExecution,
            RegistryProfile::UiaShellWorker,
            RegistryProfile::ReadOnlyExplore,
        ] {
            let granted = profile.required_permissions();
            let assembled =
                assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
                    profile,
                    &project,
                    IdentityOverrides::default(),
                    ApprovalModeCell::default(),
                    &granted,
                    None,
                    &SandboxProfile::default(),
                    Some(wiring.clone()),
                )
                .map_err(ctx("assemble with job wiring"))?;
            let expected: Vec<String> = profile
                .registered_tool_names()
                .iter()
                .map(|name| (*name).to_owned())
                .collect();
            assert_eq!(registered_names(&assembled), expected, "{profile:?}");
            assert_eq!(assembled.identity.tools_available, expected, "{profile:?}");
        }
        // Orchestratoren: nur lesen, warten, stoppen.
        let jobs = wiring
            .jobs
            .as_ref()
            .ok_or(TestError::Missing("job wiring"))?;
        let control: Vec<String> = jobs
            .control_provider()
            .tools()
            .iter()
            .map(|spec| spec.name().to_owned())
            .collect();
        let mut expected_control: Vec<String> = JOB_CONTROL_TOOLS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        expected_control.sort();
        let mut control_sorted = control.clone();
        control_sorted.sort();
        assert_eq!(control_sorted, expected_control);
        assert!(
            jobs.control_provider()
                .executor(&ToolName::new("job.start"))
                .is_none()
        );
        Ok(())
    }

    /// Plan Teil D: die lesenden Wissenswerkzeuge sind ausnahmslos
    /// `ReadWorkspace`, von keinem Profil selbst registriert und nur für die
    /// dokumentierten Leserollen angeboten; Workbench/Diary/Palace sind
    /// auto-freigegeben, Kanban nie und nur für den Root-Orchestrator.
    #[test]
    fn test_knowledge_tools_are_read_only_and_offered_only_to_reader_roles() {
        let all: Vec<&str> = [KNOWLEDGE_READ_TOOLS, KANBAN_READ_TOOLS].concat();
        assert_eq!(ROOT_ORCHESTRATOR_KNOWLEDGE_TOOLS, all.as_slice());
        for tool in &all {
            assert_eq!(
                crate::authority::tool_permission(tool),
                Some(harw_authority::Permission::ReadWorkspace),
                "{tool}"
            );
            assert_eq!(
                crate::AUTO_APPROVED_TOOLS.contains(tool),
                !KANBAN_READ_TOOLS.contains(tool),
                "{tool}"
            );
            for profile in RegistryProfile::ALL {
                assert!(
                    !profile.registered_tool_names().contains(tool),
                    "{profile:?} darf {tool} nicht selbst registrieren"
                );
            }
        }
        for role in role_names::ALL {
            let expected: &[&str] = if *role == role_names::ROOT_ORCHESTRATOR {
                ROOT_ORCHESTRATOR_KNOWLEDGE_TOOLS
            } else if KNOWLEDGE_READER_ROLES.contains(role) {
                KNOWLEDGE_READ_TOOLS
            } else {
                &[]
            };
            assert_eq!(knowledge_tools_for_role(role), expected, "{role}");
        }
        for role in KNOWLEDGE_READER_ROLES {
            assert!(role_names::ALL.contains(role), "{role}");
        }
        assert!(knowledge_tools_for_role("unbekannt").is_empty());
    }

    #[test]
    fn test_child_orchestrators_are_listed_in_all() {
        for role in role_names::CHILD_ORCHESTRATORS {
            assert!(role_names::ALL.contains(role), "{role} fehlt in ALL");
        }
        assert_eq!(role_names::CHILD_ORCHESTRATORS.len(), 3);
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
        let advertised: BTreeSet<&str> = RegistryProfile::UiaShellWorker
            .tool_names()
            .into_iter()
            .collect();
        let expected: BTreeSet<&str> = [
            "shell.exec",
            "fs.read",
            "fs.list",
            "fs.search",
            "fs.glob",
            "fs.grep",
            "doc.read_pdf",
        ]
        .into_iter()
        // Plan R9, Teil F: `job.*` gehört zu jedem `shell.exec`.
        .chain(JOB_TOOLS.iter().copied())
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
        assert!(!RegistryProfile::WorkspaceEdit.is_read_only());
        assert!(!RegistryProfile::UiaLatexWriter.is_read_only());
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
                    | RegistryProfile::WorkspaceEdit
                    | RegistryProfile::UiaLatexWriter
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
            // `WorkspaceEdit` ist ein reines Einstiegsprofil (Telegram mit
            // Workspace): keine eingebaute Rolle bekommt es.
            assert_ne!(
                profile_for_role(role),
                Some(RegistryProfile::WorkspaceEdit),
                "{role}"
            );
        }
        assert_eq!(profile_for_role("unbekannt"), None);
    }

    #[test]
    fn test_no_tools_profile_registers_and_advertises_nothing() -> TestResult {
        // Der eigentliche Zweck von `NoTools` (siehe Begründung bei
        // `RegistryProfile::NoTools`): eine Rolle mit `[tools].admitted = []`
        // darf kein Werkzeug im System-Prompt-Inventar sehen, das sie nicht
        // aufrufen darf.
        let assembled = assemble(RegistryProfile::NoTools)?;
        assert!(registered_names(&assembled).is_empty());
        assert!(assembled.identity.tools_available.is_empty());
        assert!(RegistryProfile::NoTools.is_read_only());
        Ok(())
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
            .filter(|name| !crate::embedded_agents::BASE_DEFINITION_NAMES.contains(name))
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
    // Funktion muss `host_permits` tatsächlich an jeden gebauten
    // `ShellToolProvider` durchreichen (statt es nur entgegenzunehmen) —
    // unabhängig vom `sandbox_profile` (siehe „volle Sandbox-Deaktivierung“,
    // Nutzerwunsch 2026-09-21: eine `sandbox-lease`-Freigabe muss auch
    // `SandboxProfile::Strict`-Provider erreichen, nicht nur `Host`). Sie
    // führen den echten `shell.exec`-Executor aus (wie
    // `harw-tool-shell::exec::tests`), weil nur die Ausführung selbst
    // beweist, dass Ledger bzw. Registry tatsächlich erreicht werden — ein
    // Blick auf `registered_tool_names()` allein würde die Verdrahtung nicht
    // zeigen.

    mod permits_wiring {
        use super::*;
        // Aliasiert, weil jeder Test unten seine `ToolExecutionContext` lokal
        // `ctx` nennt (siehe `make_ctx`) — das würde die Hilfsfunktion
        // `crate::test_support::ctx` sonst nach dem `let ctx = …` verdecken.
        use crate::test_support::ctx as with_ctx;
        use harw_authority::{Permission, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
        use harw_tools::{ToolCall, ToolExecutionContext, ToolOutput};
        use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};

        /// Baut eine eigenständige Workspace-Sandbox (unabhängig vom
        /// `ProjectContext`, den `discover_project` liefert) mit genau dem
        /// übergebenen Rechtesatz.
        fn make_sandbox(
            dir: &std::path::Path,
            permissions: Vec<Permission>,
        ) -> TestResult<SandboxSpec> {
            let ws_subdir = dir.join("project");
            std::fs::create_dir_all(&ws_subdir).map_err(ctx("project subdir must be created"))?;
            let registry = WorkspaceRegistry::build(
                dir,
                [WorkspaceRegistration {
                    tenant: TenantId::from_str("test-tenant"),
                    workspace: WorkspaceId::from_str("project"),
                    root: ws_subdir,
                }],
            )
            .map_err(ctx("registry build must succeed"))?;
            let binding = registry
                .resolve(
                    &TenantId::from_str("test-tenant"),
                    &WorkspaceId::from_str("project"),
                )
                .map_err(ctx("resolve must succeed"))?;
            Ok(SandboxSpec::from_resolved(
                binding,
                PermissionSet::from_policy(permissions),
            ))
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
        ) -> TestResult<Arc<dyn ToolExecutor>> {
            let project = discover_project(project_root, &DiscoveryConfig::default())
                .map_err(ctx("Discovery im Tempdir"))?;
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
            .map_err(ctx("assemble must succeed"))?;
            assembled
                .registry
                .tool_providers()
                .iter()
                .find_map(|provider| provider.executor(&ToolName::new("shell.exec")))
                .ok_or(TestError::Missing(
                    "shell.exec executor must be registered for ShellExecution",
                ))
        }

        /// Wie [`shell_executor_for`], nimmt aber zusätzlich eine
        /// explizite [`ApprovalModeCell`] entgegen (Android-Anbindung): dient
        /// dem Nachweis, dass eine `FullAccess`-Zelle auf der hier getesteten
        /// (Linux-)Plattform das Sandbox-Verhalten eines Nicht-Host-Profils
        /// unverändert lässt.
        fn shell_executor_for_with_approval_mode(
            sandbox_profile: &SandboxProfile,
            approval_mode: ApprovalModeCell,
            project_root: &std::path::Path,
        ) -> TestResult<Arc<dyn ToolExecutor>> {
            let project = discover_project(project_root, &DiscoveryConfig::default())
                .map_err(ctx("Discovery im Tempdir"))?;
            let granted = PermissionSet::from_policy([Permission::ExecuteProcess]);
            let assembled = assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile_and_permits(
                RegistryProfile::ShellExecution,
                &project,
                IdentityOverrides::default(),
                approval_mode,
                &granted,
                None,
                sandbox_profile,
                None,
            )
            .map_err(ctx("assemble must succeed"))?;
            assembled
                .registry
                .tool_providers()
                .iter()
                .find_map(|provider| provider.executor(&ToolName::new("shell.exec")))
                .ok_or(TestError::Missing(
                    "shell.exec executor must be registered for ShellExecution",
                ))
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
        ) -> TestResult<Arc<dyn ToolExecutor>> {
            let project = discover_project(project_root, &DiscoveryConfig::default())
                .map_err(ctx("Discovery im Tempdir"))?;
            let granted =
                PermissionSet::from_policy([Permission::ReadWorkspace, Permission::ExecuteProcess]);
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
            .map_err(ctx("assemble must succeed"))?;
            assembled
                .registry
                .tool_providers()
                .iter()
                .find_map(|provider| provider.executor(&ToolName::new("shell.exec")))
                .ok_or(TestError::Missing(
                    "shell.exec executor must be registered for UiaShellWorker",
                ))
        }

        /// `RegistryProfile::UiaShellWorker` trägt `SandboxProfile::Host` immer
        /// — auch wenn die aufrufende Montage `SandboxProfile::Strict`
        /// übergibt — und lehnt deshalb ohne Ledger jede Ausführung ab
        /// (fail-closed).
        #[tokio::test]
        async fn test_uia_shell_worker_always_carries_host_profile_and_denies_without_ledger()
        -> TestResult {
            let project_root = make_temp_project("uia-shell-worker-no-ledger")?;
            let executor = uia_shell_worker_executor_for(None, &project_root)?;
            let tmp = tempfile::tempdir().map_err(with_ctx("tempdir"))?;
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess])?);
            let call = make_call("echo must_not_run");

            let output = executor
                .execute(&ctx, &call)
                .await
                .map_err(with_ctx("execute must not return Err"))?;

            match output {
                ToolOutput::Error { message } => {
                    assert!(
                        message.contains("host execution requires a process permit"),
                        "uia-shell-worker must fail closed without a ledger even though \
                         sandbox_profile passed to assembly was Strict, got: {message:?}"
                    );
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected Error output without permits, got: {other:?}"
                    )));
                }
            }
            Ok(())
        }

        /// Mit Ledger, aber ohne Sitzungszustimmung bleibt `uia-shell-worker`
        /// ebenfalls fail-closed.
        #[tokio::test]
        async fn test_uia_shell_worker_denies_without_session_approval() -> TestResult {
            let project_root = make_temp_project("uia-shell-worker-no-approval")?;
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let executor = uia_shell_worker_executor_for(
                Some(wiring_with_closed_channel(ledger, registry)),
                &project_root,
            )?;
            let tmp = tempfile::tempdir().map_err(with_ctx("tempdir"))?;
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess])?);
            let call = make_call("echo must_not_run");

            let output = executor
                .execute(&ctx, &call)
                .await
                .map_err(with_ctx("execute must not return Err"))?;

            match output {
                ToolOutput::Error { message } => {
                    assert!(
                        message.contains("requires local UI approval"),
                        "with a ledger but no session approval, denial must name the \
                         missing approval, got: {message:?}"
                    );
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected Error output without session approval, got: {other:?}"
                    )));
                }
            }
            Ok(())
        }

        #[tokio::test]
        async fn test_and_permits_none_denies_host_execution() -> TestResult {
            let project_root = make_temp_project("permits-none")?;
            let executor = shell_executor_for(&SandboxProfile::Host, None, &project_root)?;
            let tmp = tempfile::tempdir().map_err(with_ctx("tempdir"))?;
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess])?);
            let call = make_call("echo must_not_run");

            let output = executor
                .execute(&ctx, &call)
                .await
                .map_err(with_ctx("execute must not return Err"))?;

            match output {
                ToolOutput::Error { message } => {
                    assert!(
                        message.contains("host execution requires a process permit"),
                        "without host_permits the Host profile must fail closed, got: {message:?}"
                    );
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected Error output without permits, got: {other:?}"
                    )));
                }
            }
            Ok(())
        }

        #[tokio::test]
        async fn test_and_permits_configured_but_session_not_approved_denies() -> TestResult {
            let project_root = make_temp_project("permits-no-approval")?;
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let executor = shell_executor_for(
                &SandboxProfile::Host,
                Some(wiring_with_closed_channel(ledger, registry)),
                &project_root,
            )?;
            let tmp = tempfile::tempdir().map_err(with_ctx("tempdir"))?;
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess])?);
            let call = make_call("echo must_not_run");

            let output = executor
                .execute(&ctx, &call)
                .await
                .map_err(with_ctx("execute must not return Err"))?;

            match output {
                ToolOutput::Error { message } => {
                    assert!(
                        message.contains("requires local UI approval"),
                        "with a ledger but no session approval, denial must name the \
                         missing approval, got: {message:?}"
                    );
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected Error output without session approval, got: {other:?}"
                    )));
                }
            }
            Ok(())
        }

        #[tokio::test]
        async fn test_and_permits_configured_and_session_approved_passes_permit_boundary()
        -> TestResult {
            let project_root = make_temp_project("permits-approved")?;
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let executor = shell_executor_for(
                &SandboxProfile::Host,
                Some(wiring_with_closed_channel(
                    Arc::clone(&ledger),
                    Arc::clone(&registry),
                )),
                &project_root,
            )?;
            let tmp = tempfile::tempdir().map_err(with_ctx("tempdir"))?;
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess])?);
            registry.mark_session_approved(ctx.session_id().as_str());
            let call = make_call("echo host_ok");

            let output = executor
                .execute(&ctx, &call)
                .await
                .map_err(with_ctx("execute must not return Err"))?;

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
            Ok(())
        }

        #[tokio::test]
        async fn test_and_permits_without_active_approval_runs_in_sandbox_for_non_host_profile()
        -> TestResult {
            // Der Registry-Eintrag wird jetzt auch an ein
            // `SandboxProfile::Strict`-Provider gehängt (Nutzerwunsch „volle
            // Sandbox-Deaktivierung“ — siehe `build_shell_provider`), aber
            // ohne eine aktive Sitzungs- oder Einmalfreigabe in der Registry
            // bleibt die Sandbox unverändert die Grenze: die Ausführung darf
            // deshalb nie mit einer Permit-Fehlermeldung scheitern (der
            // Ledger wird für Strict nie befragt, nur die Registry — und die
            // meldet hier keine Freigabe).
            let project_root = make_temp_project("permits-strict-no-approval")?;
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let executor = shell_executor_for(
                &SandboxProfile::Strict,
                Some(wiring_with_closed_channel(ledger, registry)),
                &project_root,
            )?;
            let tmp = tempfile::tempdir().map_err(with_ctx("tempdir"))?;
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess])?);
            let call = make_call("echo strict_mode_ok");

            let output = executor
                .execute(&ctx, &call)
                .await
                .map_err(with_ctx("execute must not return Err"))?;

            match &output {
                ToolOutput::Error { message } => {
                    assert!(
                        !message.contains("host execution requires a process permit")
                            && !message.contains("requires local UI approval"),
                        "Strict profile without an active approval must never trigger a \
                         permit denial, got: {message:?}"
                    );
                }
                ToolOutput::Json { content } => {
                    assert!(
                        content.get("executed_on").is_none(),
                        "without an active approval the Strict profile must not run on \
                         the host: {content}"
                    );
                }
                ToolOutput::Text { .. } => {}
            }
            Ok(())
        }

        /// Android-Anbindung: eine `FullAccess`-Zelle wird jetzt an jeden
        /// gebauten `ShellToolProvider` gereicht
        /// ([`ShellToolProvider::with_approval_mode`]), aber auf der hier
        /// getesteten (Linux-)Plattform liest der Shell-Provider die Zelle
        /// nicht — ein Nicht-Host-Profil (`Strict`) muss deshalb trotz
        /// `FullAccess` unverändert im Sandbox-Pfad bleiben, statt ohne
        /// Rückfrage auf dem Host zu laufen.
        #[tokio::test]
        async fn test_full_access_approval_mode_does_not_bypass_sandbox_on_default_platform()
        -> TestResult {
            let project_root = make_temp_project("full-access-non-host-profile")?;
            let approval_mode = ApprovalModeCell::new(harw_extension_api::ApprovalMode::FullAccess);
            let executor = shell_executor_for_with_approval_mode(
                &SandboxProfile::Strict,
                approval_mode,
                &project_root,
            )?;
            let tmp = tempfile::tempdir().map_err(with_ctx("tempdir"))?;
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess])?);
            let call = make_call("echo full_access_still_sandboxed");

            let output = executor
                .execute(&ctx, &call)
                .await
                .map_err(with_ctx("execute must not return Err"))?;

            match &output {
                ToolOutput::Error { message } => {
                    assert!(
                        !message.contains("host execution requires a process permit")
                            && !message.contains("requires local UI approval"),
                        "a FullAccess cell without host-permit wiring must never trigger a \
                         permit denial on the sandbox path, got: {message:?}"
                    );
                }
                ToolOutput::Json { content } => {
                    assert!(
                        content.get("executed_on").is_none(),
                        "on the default (non-Android) platform, FullAccess must not move a \
                         Strict-profile shell.exec call onto the host: {content}"
                    );
                }
                ToolOutput::Text { .. } => {}
            }
            Ok(())
        }

        /// Beweist die eigentliche Behebung dieses Auftrags: ein
        /// `/sandbox-lease` auf der prozessweit geteilten Registry muss auch
        /// `shell.exec`-Aufrufe erreichen, deren `ShellToolProvider` mit
        /// `SandboxProfile::Strict` gebaut wurde (die Root-Session ist immer
        /// Strict) — vor dieser Behebung hängte `build_shell_provider` Ledger
        /// und Registry nur an einen `SandboxProfile::Host`-Provider, sodass
        /// `determine_effective_host` für Strict immer `false` lieferte, egal
        /// welche Freigabe in der Registry stand.
        #[tokio::test]
        async fn test_and_permits_with_active_session_approval_runs_on_host_for_strict_profile()
        -> TestResult {
            let project_root = make_temp_project("permits-strict-approved")?;
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let executor = shell_executor_for(
                &SandboxProfile::Strict,
                Some(wiring_with_closed_channel(
                    Arc::clone(&ledger),
                    Arc::clone(&registry),
                )),
                &project_root,
            )?;
            let tmp = tempfile::tempdir().map_err(with_ctx("tempdir"))?;
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess])?);
            registry.mark_session_approved(ctx.session_id().as_str());
            let call = make_call("echo strict_lease_ok");

            let output = executor
                .execute(&ctx, &call)
                .await
                .map_err(with_ctx("execute must not return Err"))?;

            match &output {
                ToolOutput::Json { content } => {
                    assert_eq!(
                        content["executed_on"], "host",
                        "an active sandbox-lease approval must run a Strict-profile \
                         shell.exec call on the host, got: {content}"
                    );
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected a successful host-executed JSON output, got: {other:?}"
                    )));
                }
            }
            Ok(())
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
        -> TestResult {
            let project_root = make_temp_project("permits-prompt-sender-wired")?;
            let ledger = Arc::new(ProcessPermitLedger::default());
            let registry = Arc::new(HostPermitSessionRegistry::default());
            let (sender, mut receiver) = harw_tool_shell::host_permit_prompt_channel();
            let wiring = HostPermitWiring::new(ledger, registry, sender)
                .with_preselected_variant(HostPermitVariant::SingleExecution);
            let executor = shell_executor_for(&SandboxProfile::Host, Some(wiring), &project_root)?;

            let approver: tokio::task::JoinHandle<TestResult> = tokio::spawn(async move {
                let prompt = receiver
                    .recv()
                    .await
                    .ok_or(TestError::Missing("prompt must arrive at the receiver"))?;
                assert_eq!(
                    prompt.preselected_variant(),
                    HostPermitVariant::SingleExecution,
                    "the wiring's preselected variant must reach the prompt unchanged"
                );
                assert!(
                    prompt.approve(HostPermitVariant::SingleExecution),
                    "the approval must reach the waiting ShellExecutor"
                );
                Ok(())
            });

            let tmp = tempfile::tempdir().map_err(with_ctx("tempdir"))?;
            let ctx = make_ctx(make_sandbox(tmp.path(), vec![Permission::ExecuteProcess])?);
            let call = make_call("echo prompt_sender_wired");

            let output = executor
                .execute(&ctx, &call)
                .await
                .map_err(with_ctx("execute must not return Err"))?;
            approver
                .await
                .map_err(with_ctx("approver task must not panic"))??;

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
            Ok(())
        }
    }
}
