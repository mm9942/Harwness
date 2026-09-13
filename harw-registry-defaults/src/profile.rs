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
//! - [`assemble_registry`] — baut eine Registry für ein Profil.
//! - [`role_names`] — die Namen der eingebauten Rollen als Single Source of Truth.
//!
//! # Warum ein Filter statt eines zweiten Providers
//! `harw_tool_fs::FsToolProvider` liefert lesende *und* schreibende Werkzeuge
//! aus einem Provider. Ein read-only Profil registriert ihn deshalb hinter
//! [`RestrictedToolProvider`]: `fs.write` verschwindet dort aus `tools()` **und**
//! aus `executor()`. Ein Kind kann das Werkzeug damit weder sehen noch durch
//! Raten seines Namens aufrufen — die Beschränkung ist keine Prompt-Bitte.
//!
//! # Fehler
//! [`assemble_registry`] gibt [`crate::RegistryDefaultsError`] zurück:
//! `ProjectDiscovery`, wenn `cwd` kein auflösbares Projekt ist, und
//! `BrowserHost` (nur unter dem Feature `browser`), wenn die Host-Konfiguration
//! ungültig ist.
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync`. [`assemble_registry`] ist synchron und
//! zustandslos.

use std::path::PathBuf;
use std::sync::Arc;

use harw_extension_api::{
    ExtensionRegistryBuilder, ToolExecutor, ToolName, ToolProvider, ToolSpec,
};
use harw_instructions::{AgentIdentity, BaselineInstructionsProvider};
use harw_project_discovery::{DiscoveryConfig, ProjectContextProvider, discover_project};
use harw_tool_deps::DepsToolProvider;
use harw_tool_fs::FsToolProvider;
use harw_tool_lens::LensToolProvider;
use harw_tool_shell::ShellToolProvider;
use harw_tool_web::WebToolProvider;

#[cfg(feature = "browser")]
use harw_browser_thirtyfour::{config::FirefoxHostConfig, host::FirefoxHost};
#[cfg(feature = "browser")]
use harw_tool_browser::{BrowserToolSet, HarwnessBrowserToolProvider};

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
/// stehende Zahl. `context-steward` und `intel-scout` (`agents/roles/**`)
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

    /// Alle bekannten eingebauten Rollen.
    ///
    /// Siehe die Moduldokumentation oben: `context-steward` und
    /// `intel-scout` sind absichtlich nicht enthalten (eigener, offener
    /// Befund), obwohl ihre Rollendateien bereits existieren und von
    /// [`crate::embedded_agents::builtin_agent_toml`] bereits gefunden werden.
    pub const ALL: &[&str] = &[
        EXPLORER,
        RESEARCHER_DEPS,
        RESEARCHER_WEB,
        PLANNER,
        ANALYST,
        SECURITY_EGRESS_TRIAGE,
        SECURITY_BASELINE_TRIAGE,
        SECURITY_STRUCTURE_TRIAGE,
        SECURITY_ENDPOINT_TRIAGE,
    ];
}

// ---------------------------------------------------------------------------
// Werkzeuglisten je Provider-Gruppe
// ---------------------------------------------------------------------------

/// Die lesenden Werkzeuge von `harw-tool-fs`.
const FS_READ_ONLY_TOOLS: &[&str] = &["fs.read", "fs.list", "fs.search", "fs.glob", "fs.grep"];

/// Die vollständige Werkzeugliste von `harw-tool-fs`, in Provider-Reihenfolge.
const FS_FULL_TOOLS: &[&str] = &[
    "fs.read",
    "fs.write",
    "fs.list",
    "fs.search",
    "fs.glob",
    "fs.grep",
];

/// Die Werkzeuge von `harw-tool-deps`, in Provider-Reihenfolge.
const DEPS_TOOLS: &[&str] = &[
    "deps.graph",
    "deps.locked",
    "deps.source_read",
    "deps.source_search",
    "deps.source_list",
];

/// Die Werkzeuge von `harw-tool-web`, in Provider-Reihenfolge.
const WEB_TOOLS: &[&str] = &["web.fetch", "web.docs_rs", "web.crates_io"];

/// Die Werkzeuge von `harw-tool-shell`.
const SHELL_TOOLS: &[&str] = &["shell.exec"];

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
const LENS_TOOLS: &[&str] = &["lens.ask"];

/// Die Browser-Werkzeuge, in Provider-Reihenfolge (nur unter Feature `browser`).
#[cfg(feature = "browser")]
const BROWSER_TOOLS: &[&str] = &[
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
/// - `Full` — voller Coding-Satz (heutiges Verhalten).
/// - `ReadOnlyExplore` — ausschließlich lesend.
/// - `Research` — `ReadOnlyExplore` plus Netzzugang über die Sandbox-Allowlist.
/// - `Planning` — `ReadOnlyExplore` plus `lens.ask` (Plan-/Goal-Operationen
///   bewirbt es nicht: das Kind besitzt dafür keinen Executor).
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
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::profile::RegistryProfile;
///
/// assert_eq!(RegistryProfile::default(), RegistryProfile::Full);
/// assert!(RegistryProfile::ReadOnlyExplore.is_read_only());
/// assert!(!RegistryProfile::ReadOnlyExplore.tool_names().contains(&"fs.write"));
/// assert!(RegistryProfile::NoTools.tool_names().is_empty());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegistryProfile {
    /// Voller Coding-Satz (heutiges Verhalten): fs.*, shell.exec, Browser (Feature).
    #[default]
    Full,
    /// Ausschließlich lesend: fs.read/list/search/glob/grep + deps.*.
    ReadOnlyExplore,
    /// ReadOnlyExplore + web.* (Netz nur über die Host-Allowlist der Sandbox).
    Research,
    /// ReadOnlyExplore + `lens.ask`. Die Plan-/Goal-Operationen der
    /// Composition-Root gehören **nicht** dazu (siehe W1-05).
    Planning,
    /// Registriert und bewirbt keine Werkzeuge — für Rollen, die bereits
    /// ausgewertete Befund-Batches als Parameter bekommen (`[tools].admitted
    /// = []`), etwa die vier `security-*-triage`-Rollen (siehe
    /// [`role_names::SECURITY_EGRESS_TRIAGE`] u. a.).
    NoTools,
}

impl RegistryProfile {
    /// Alle Profile in Deklarationsreihenfolge.
    ///
    /// Nützlich für erschöpfende Tests und für CLI-Hilfetexte.
    pub const ALL: &'static [RegistryProfile] = &[
        RegistryProfile::Full,
        RegistryProfile::ReadOnlyExplore,
        RegistryProfile::Research,
        RegistryProfile::Planning,
        RegistryProfile::NoTools,
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
            RegistryProfile::ReadOnlyExplore => "read-only exploration agent",
            RegistryProfile::Research => "research agent",
            RegistryProfile::Planning => "planning agent",
            RegistryProfile::NoTools => "parameter-only triage agent",
        }
    }

    /// Gibt an, ob dieses Profil ausschließlich lesende Werkzeuge registriert.
    ///
    /// # Rückgabe
    /// `false` nur für [`RegistryProfile::Full`]; alle anderen Profile sind
    /// read-only und dürfen weder `fs.write` noch `shell.exec` sehen.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_registry_defaults::profile::RegistryProfile;
    ///
    /// assert!(!RegistryProfile::Full.is_read_only());
    /// assert!(RegistryProfile::Planning.is_read_only());
    /// ```
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        !matches!(self, RegistryProfile::Full)
    }

    /// Die Werkzeuge, die **dieses Crate** für das Profil registriert.
    ///
    /// # Beschreibung
    /// Die Reihenfolge entspricht exakt der Reihenfolge, in der
    /// [`assemble_registry`] die Provider registriert, und damit der Reihenfolge
    /// von `ExtensionRegistry::tool_providers().flat_map(ToolProvider::tools)`.
    /// Unter dem Feature `browser` hängt `Full` zusätzlich die sieben
    /// `browser.*`-Werkzeuge an. `Planning` hängt zusätzlich `lens.ask` an —
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
            RegistryProfile::Full => {
                // `mut` wird nur unter dem Feature `browser` gebraucht.
                #[cfg_attr(not(feature = "browser"), allow(unused_mut))]
                let mut names: Vec<&'static str> = FS_FULL_TOOLS
                    .iter()
                    .chain(SHELL_TOOLS.iter())
                    .copied()
                    .collect();
                #[cfg(feature = "browser")]
                names.extend(BROWSER_TOOLS.iter().copied());
                names
            }
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
            RegistryProfile::Research => FS_READ_ONLY_TOOLS
                .iter()
                .chain(DEPS_TOOLS.iter())
                .chain(WEB_TOOLS.iter())
                .copied()
                .collect(),
            // Siehe die Begründung bei `RegistryProfile::NoTools`: keine
            // Werkzeuge registriert, keine beworben.
            RegistryProfile::NoTools => Vec::new(),
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

/// Erzeugt die Tool-Provider eines Profils in Registrierungsreihenfolge.
///
/// # Fehler
/// - [`RegistryDefaultsError::BrowserHost`]: nur unter dem Feature `browser`,
///   wenn die Firefox-Host-Konfiguration ungültig ist.
fn profile_tool_providers(
    profile: RegistryProfile,
) -> RegistryDefaultsResult<Vec<Arc<dyn ToolProvider>>> {
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
            let shell: Arc<dyn ToolProvider> = Arc::new(ShellToolProvider::default());
            // `mut` wird nur unter dem Feature `browser` gebraucht.
            #[cfg_attr(not(feature = "browser"), allow(unused_mut))]
            let mut providers: Vec<Arc<dyn ToolProvider>> = vec![filesystem, shell];
            #[cfg(feature = "browser")]
            {
                // `FirefoxHost::new` führt keine I/O aus und startet keinen
                // Prozess; ein fehlender WebDriver fällt erst beim ersten
                // `browser.open` auf und bricht den Start nicht.
                let host: Arc<dyn harw_browser::host::BrowserHost> =
                    Arc::new(FirefoxHost::new(FirefoxHostConfig::default()).map_err(|error| {
                        RegistryDefaultsError::BrowserHost(error.to_string())
                    })?);
                providers.push(Arc::new(HarwnessBrowserToolProvider::new(
                    BrowserToolSet::new(host),
                )));
            }
            Ok(providers)
        }
        RegistryProfile::ReadOnlyExplore => Ok(read_only_base()),
        // `LensToolProvider::new()` ist zustandslos (keine Bau-, Home- oder
        // Indexpfad-Konfiguration nötig): `derive_read_scope` leitet den
        // `ReadScope` beim Aufruf aus dem `ToolExecutionContext` ab, nie aus
        // einem Konstruktorargument. `profile.rs` muss ihm deshalb nichts
        // zusätzlich mitgeben.
        RegistryProfile::Planning => {
            let mut providers = read_only_base();
            providers.push(Arc::new(LensToolProvider::new()));
            Ok(providers)
        }
        RegistryProfile::Research => {
            let mut providers = read_only_base();
            providers.push(Arc::new(WebToolProvider::new()));
            Ok(providers)
        }
        // Keine Provider: siehe die Begründung bei `RegistryProfile::NoTools`.
        RegistryProfile::NoTools => Ok(Vec::new()),
    }
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
/// Die Freigabegrenze ist für alle Profile dieselbe
/// [`crate::DefaultApprovalPolicy`]: sie lässt ausschließlich read-only
/// Werkzeuge ohne Rückfrage durch.
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
/// - [`RegistryDefaultsError::ProjectDiscovery`]: `cwd` ist kein auflösbares Projekt.
/// - [`RegistryDefaultsError::BrowserHost`]: nur unter dem Feature `browser` und
///   nur für [`RegistryProfile::Full`], wenn die Host-Konfiguration ungültig ist.
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

    let providers = profile_tool_providers(profile)?;

    let advertised_tools: Vec<String> = profile
        .tool_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();

    let IdentityOverrides {
        agent_name,
        role_description,
        extra_context,
    } = overrides;

    let identity = AgentIdentity::new(
        agent_name.unwrap_or_else(|| "harw".to_owned()),
        project.cwd.display().to_string(),
    )
    .with_role(role_description.unwrap_or_else(|| profile.role_description().to_owned()))
    .with_project_root(project.project_root.display().to_string())
    .with_tools(advertised_tools)
    .with_extra_context(extra_context);

    let mut builder = ExtensionRegistryBuilder::default()
        .approval_handler(Arc::new(DefaultApprovalPolicy))
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
        project,
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

    #[test]
    fn test_research_profile_adds_exactly_the_web_tools() {
        let assembled = assemble(RegistryProfile::Research);
        let names = registered_names(&assembled);
        for web_tool in WEB_TOOLS {
            assert!(names.contains(&(*web_tool).to_owned()), "fehlt: {web_tool}");
        }
        assert_eq!(
            names.len(),
            RegistryProfile::ReadOnlyExplore.registered_tool_names().len() + WEB_TOOLS.len()
        );
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
        assert!(!assembled.identity.tools_available.contains(&"plan".to_owned()));
        assert!(!assembled.identity.tools_available.contains(&"goal".to_owned()));
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
                    !assembled.identity.tools_available.contains(&forbidden.to_owned()),
                    "{profile:?} darf {forbidden} nicht bewerben"
                );
            }
        }
    }

    #[test]
    fn test_identity_advertises_exactly_the_profile_tool_names() {
        for profile in RegistryProfile::ALL {
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
            assert_eq!(assembled.identity.role_description, profile.role_description());
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
            },
        )
        .expect("assemble");

        assert_eq!(assembled.identity.agent_name, "explorer-3");
        assert_eq!(assembled.identity.role_description, "focused explorer");
        assert_eq!(assembled.identity.extra_context, vec!["Antworte nur mit JSON."]);
    }

    #[test]
    fn test_restricted_provider_hides_filtered_executor() {
        let provider = RestrictedToolProvider::new(
            Arc::new(FsToolProvider::default()),
            FS_READ_ONLY_TOOLS,
        );
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

    #[test]
    fn test_default_profile_is_full() {
        assert_eq!(RegistryProfile::default(), RegistryProfile::Full);
        assert!(!RegistryProfile::Full.is_read_only());
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
    /// `context-steward` und `intel-scout` werden von derselben
    /// Verzeichnis-Sammlung ebenfalls gefunden, sind aber bewusst nicht in
    /// `ALL` (siehe Moduldokumentation von [`role_names`]) — ein eigener,
    /// noch offener Befund außerhalb dieses Knotens. Sie sind deshalb eine
    /// dokumentierte, keine stillschweigende Ausnahme.
    #[test]
    fn test_role_names_all_matches_discovered_role_files_minus_pending_exclusions() {
        use std::collections::BTreeSet;

        /// Rollendateien, die die Verzeichnis-Sammlung bereits findet, deren
        /// Aufnahme in `role_names::ALL` aber ein eigener, noch offener
        /// Befund ist (siehe Moduldokumentation von `role_names`).
        const PENDING_EXCLUSIONS: &[&str] = &["context-steward", "intel-scout"];

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
}
