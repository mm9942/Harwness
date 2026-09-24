//! `/research`, `/research-deps` und `/research-web` — gebundene Recherche
//! durch read-only Kinder.
//!
//! # Verantwortungsbereich
//! Implementiert die Recherche-Operationen aus AP W4-03 plus die allgemeine
//! Recherche. Alle drei bauen aus denselben Argumenten eine
//! [`ResearchQuestion`](harw_research::ResearchQuestion), starten **einen**
//! Kind-Lauf und liefern das gegen den `ResearchFinding`-Vertrag validierte
//! Ergebnis. Sie unterscheiden sich in der Rolle des Kindes, den
//! Vorgabe-Quellklassen und dem erwarteten Ausgabeformat:
//!
//! | Operation | Kind-Rolle | Zweck |
//! |---|---|---|
//! | `/research` | `researcher` (UIA-Aufrufer: `uia-explorer`) | allgemeine Recherche zu beliebigen Themen |
//! | `/research-deps` | `researcher-deps` | Rust/Cargo-Dependency-Fakten (Vorgabe) |
//! | `/research-deps --generic` bzw. `ecosystem=<x>` | `dependency-researcher` | ökosystem-neutrale Dependency-Fakten |
//! | `/research-web` | `researcher-web` (UIA-Aufrufer: `uia-explorer`) | Doku-Web-Recherche |
//!
//! # Warum mehrere Operationen und nicht eine mit Parameter
//! `Surface::AgentTool::child_name` ist ein `&'static str`, der zur Compile-Zeit
//! aus der `#[operation]`-Deklaration stammt. Eine einzige Operation könnte die
//! Rolle also nicht zur Laufzeit umschalten, ohne ihre eigene Deklaration zu
//! widerlegen. Getrennte Operationen sind die ehrliche Form desselben Ablaufs.
//! `/research-deps --generic` ist die eine bewusste Laufzeit-Umschaltung: sie
//! bleibt innerhalb derselben Aufgabe (Dependency-Fakten) und wechselt nur vom
//! Rust-Spezialisten auf den ökosystem-neutralen Rechercheur — beide lesen,
//! keiner schreibt; die Obergrenze des Kindes bestimmt in jedem Fall
//! [`child_reducer`] aus der **tatsächlich gestarteten** Rolle.
//!
//! # Warum String-Literale statt `role_names`-Konstanten
//! Das `#[operation]`-Makro parst `agent_tool(child = …)` als `syn::LitStr` —
//! ein Konstanten-Pfad ist dort ein Compile-Fehler (siehe
//! `harw-macros/src/operation.rs`, `parse_operation_args`). Die Literale werden
//! deshalb in `RESEARCHER_CHILD`, `RESEARCHER_DEPS_CHILD` und
//! `RESEARCHER_WEB_CHILD` gespiegelt und in den Tests gegen
//! [`role_names`](harw_registry_defaults::profile::role_names) geprüft — ein
//! Auseinanderlaufen ist damit ein Testfehler, kein stiller Rollenfehler zur
//! Laufzeit.
//!
//! # UIA-Aufrufer
//! Die Spawn-Matrix (`harw_agent_dsl::roles::can_spawn`) lässt eine
//! `UserInterface`-Session keine `Worker`-Rolle starten; `researcher`,
//! `researcher-web`, `researcher-deps` und `dependency-researcher` sind
//! `Worker`. `/research` und `/research-web` weichen für UIA-Aufrufer deshalb
//! auf `uia-explorer` (`UiaWorker`, read-only mit egress-gebundenem Netz) aus.
//! `/research-deps` behält sein bisheriges Verhalten (kein Ausweichen).
//!
//! # Schlüsseltypen
//! - [`ResearchArgs`] — gemeinsamer Argument-Container aller Operationen.
//! - `ResearchOperation`, `ResearchDepsOperation`, `ResearchWebOperation` —
//!   vom `#[operation]`-Makro erzeugte Op-Structs.
//!
//! # Nebenläufigkeit
//! Alle Op-Structs sind zustandslose Unit-Structs → `Send + Sync`.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`]: keine Frage, eine unbekannte Quellklasse
//!   in `sources`, ein leeres `ecosystem=`, oder `--generic`/`ecosystem=` an
//!   einer anderen Operation als `/research-deps`.
//! - [`OpError::NotAvailable`]: kein Agent-Spawner/StateStore im Kontext.
//! - [`OpError::Execution`]: Kind-Lauf, Return-Contract oder Persistenz
//!   schlugen fehl.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::research::ResearchArgs;
//! use harw_operations::FromRawArgs;
//!
//! let args = ResearchArgs::from_raw_args(&[
//!     "--generic".to_owned(),
//!     "welche".to_owned(),
//!     "Lizenz".to_owned(),
//! ])
//! .expect("Argument-Parsing schlägt hier nicht fehl");
//! assert_eq!(args.question.as_deref(), Some("welche Lizenz"));
//! assert!(args.generic);
//! ```

use harw_agent_dsl::roles::AgentRoleId;
use harw_core::child_controller::JoinSemantics;
use harw_core_bridge::{ChildReturnContract, OpContextCoreExt, fanout_children, parse_budget_hint};
use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
use harw_registry_defaults::authority_reducer_for_role;
use harw_registry_defaults::profile::role_names;
use harw_research::{
    Freshness, QuestionId, QuestionScope, ResearchFinding, ResearchQuestion, SourceClass,
};

use crate::explore::{
    READ_ONLY_REDUCER, SINGLE_CHILD_BUDGET, child_payload, finding_from_value, finding_output,
    persist_finding, question_slug, reducer_for_role,
};

// ── Konstanten ───────────────────────────────────────────────────────────────

/// Das `child`-Literal der `research`-Deklaration.
///
/// Muss [`role_names::RESEARCHER`] entsprechen; siehe Modul-Dokumentation.
/// Nur im Test-Build vorhanden — sie hat keine Laufzeitaufgabe.
#[cfg(test)]
pub(crate) const RESEARCHER_CHILD: &str = "researcher";

/// Das `child`-Literal der `research_deps`-Deklaration.
///
/// Muss [`role_names::RESEARCHER_DEPS`] entsprechen; siehe Modul-Dokumentation.
/// Nur im Test-Build vorhanden — sie hat keine Laufzeitaufgabe.
#[cfg(test)]
pub(crate) const RESEARCHER_DEPS_CHILD: &str = "researcher-deps";

/// Das `child`-Literal der `research_web`-Deklaration.
///
/// Muss [`role_names::RESEARCHER_WEB`] entsprechen; siehe Modul-Dokumentation.
/// Nur im Test-Build vorhanden — sie hat keine Laufzeitaufgabe.
#[cfg(test)]
pub(crate) const RESEARCHER_WEB_CHILD: &str = "researcher-web";

/// Akteur-Kennung für Plan-Mutationen aus der allgemeinen Recherche.
const ACTOR_RESEARCH: &str = "op:research";

/// Akteur-Kennung für Plan-Mutationen aus der Dependency-Recherche.
const ACTOR_RESEARCH_DEPS: &str = "op:research-deps";

/// Akteur-Kennung für Plan-Mutationen aus der Web-Recherche.
const ACTOR_RESEARCH_WEB: &str = "op:research-web";

/// Vorgabe-Quellklassen der allgemeinen Recherche.
///
/// `researcher` liest Workspace-Dokumente (`fs.*`, `doc.read_pdf`) und das
/// Netz (`web.fetch`/`web.search`); die Vorgabe deckt beides ab.
const GENERAL_DEFAULT_SOURCES: &[SourceClass] = &[
    SourceClass::LocalSource,
    SourceClass::OfficialDocs,
    SourceClass::Standard,
    SourceClass::Repository,
    SourceClass::ReleaseNotes,
    SourceClass::Web,
];

/// Vorgabe-Quellklassen der Rust/Cargo-Dependency-Recherche.
const DEPS_DEFAULT_SOURCES: &[SourceClass] = &[
    SourceClass::CargoRegistrySource,
    SourceClass::OfficialDocs,
    SourceClass::ReleaseNotes,
];

/// Vorgabe-Quellklassen der ökosystem-neutralen Dependency-Recherche.
///
/// Manifeste und Lockfiles im Workspace (`LocalSource`), die offiziellen
/// Paket-Registries (`PackageRegistrySource`, für Cargo
/// `CargoRegistrySource`), Doku und Release-Notes.
const GENERIC_DEPS_DEFAULT_SOURCES: &[SourceClass] = &[
    SourceClass::LocalSource,
    SourceClass::PackageRegistrySource,
    SourceClass::CargoRegistrySource,
    SourceClass::OfficialDocs,
    SourceClass::ReleaseNotes,
];

/// Vorgabe-Quellklassen der Web-Recherche.
const WEB_DEFAULT_SOURCES: &[SourceClass] = &[
    SourceClass::OfficialDocs,
    SourceClass::Standard,
    SourceClass::Repository,
    SourceClass::Web,
];

/// Erwartetes Ausgabeformat der allgemeinen Recherche.
const GENERAL_EXPECTED_OUTPUT: &str = "Ein ResearchFinding mit belegter Schlussfolgerung: \
     konkurrierende Hypothesen (je mit konsistenten und inkonsistenten Belegen), tragende \
     Schlüsselannahmen mit Status, je Beleg Quelle, Abrufzeitpunkt bzw. Fundstelle, wörtlicher \
     Auszug sowie reliability (Quelle) und credibility (Aussage), dazu likelihood getrennt von \
     confidence samt Begründung. Aussagen ohne Beleg gehören in unresolved_questions.";

/// Erwartetes Ausgabeformat der Rust/Cargo-Dependency-Recherche.
const DEPS_EXPECTED_OUTPUT: &str = "Ein ResearchFinding mit verified_versions: je Crate die \
     geprüfte Version, die MSRV und die benutzten Features, jeweils belegt durch die Quelle, aus \
     der der Wert stammt (Lockfile-Eintrag, Registry-Manifest oder offizielle Doku).";

/// Erwartetes Ausgabeformat der ökosystem-neutralen Dependency-Recherche.
///
/// [`generic_deps_expected_output`] hängt ein genanntes Ökosystem an.
const GENERIC_DEPS_EXPECTED_OUTPUT: &str = "Ein ResearchFinding mit verified_versions: je \
     Paket ein Eintrag mit package und ecosystem (npm, pypi, go, maven, cargo …), der geprüften \
     Version sowie — soweit die Frage es verlangt — Lizenz, Laufzeit-/Sprachkompatibilität und \
     relevanten Optionen, jeweils belegt durch die Quelle, aus der der Wert stammt \
     (Manifest- oder Lockfile-Eintrag im Workspace, offizielle Paket-Registry oder offizielle \
     Doku).";

/// Erwartetes Ausgabeformat der Web-Recherche.
const WEB_EXPECTED_OUTPUT: &str = "Ein ResearchFinding, dessen Belege jeweils URL, Abrufzeitpunkt \
     und wörtlichen Auszug tragen. Aussagen ohne abrufbare Quelle gehören in \
     unresolved_questions, nicht in die Schlussfolgerung.";

/// Stop-Bedingung der allgemeinen Recherche.
const GENERAL_STOP_CONDITION: &str = "Die Frage ist aus den erlaubten Quellklassen belegt \
     beantwortet und die konkurrierenden Hypothesen sind gegeneinander geprüft — oder die \
     erlaubten Quellen enthalten die Antwort nachweislich nicht.";

/// Stop-Bedingung der Rust/Cargo-Dependency-Recherche.
const DEPS_STOP_CONDITION: &str = "Für jedes genannte Crate liegen Version, MSRV und relevante \
     Features belegt vor — oder es ist belegt, dass die Quelle sie nicht ausweist.";

/// Stop-Bedingung der ökosystem-neutralen Dependency-Recherche.
const GENERIC_DEPS_STOP_CONDITION: &str = "Für jedes genannte Paket liegen Ökosystem, Version \
     und die erfragten Eigenschaften belegt vor — oder es ist belegt, dass die Quelle sie nicht \
     ausweist.";

/// Stop-Bedingung der Web-Recherche.
const WEB_STOP_CONDITION: &str = "Die Frage ist aus den erlaubten Quellklassen belegt beantwortet, \
     oder die erlaubten Quellen enthalten die Antwort nachweislich nicht.";

/// Schreibweisen, die das Rust/Cargo-Ökosystem bezeichnen.
///
/// Ein `ecosystem=` mit einem dieser Werte behält den Rust-Spezialisten
/// `researcher-deps`, solange nicht zusätzlich `--generic` gesetzt ist.
const CARGO_ECOSYSTEM_ALIASES: &[&str] = &["cargo", "rust", "crates", "crates.io", "crates_io"];

// ── Argumente ────────────────────────────────────────────────────────────────

/// Eingabe-Argumente aller Recherche-Operationen.
///
/// # Beschreibung
/// Auf der Command-Fläche ist die Token-Zeile die Frage; ausgenommen sind die
/// Routing-Angaben `--generic` und `ecosystem=<x>` (auch `--ecosystem=<x>`
/// oder `--ecosystem <x>`), die nur `/research-deps` auswertet. Die Listen
/// (`packages`, `urls`, `sources`) sind über die JSON-Flächen erreichbar. Das
/// ist dieselbe Aufteilung wie bei [`crate::explore::ExploreArgs`] und aus
/// demselben Grund von Hand implementiert (siehe dort).
///
/// # Felder
/// - `question` (`Option<String>`): die gebundene Frage.
/// - `packages` (`Vec<String>`): Paket-/Crate-Namen, auf die die Recherche
///   begrenzt ist. Der frühere Feldname `crates` wird als Alias weiter
///   akzeptiert.
/// - `urls` (`Vec<String>`): URLs oder URL-Präfixe im erlaubten Scope.
/// - `sources` (`Vec<String>`): Quellklassen als Text; siehe
///   `parse_source_class`. Leer = die Vorgabe der jeweiligen Operation.
/// - `task` (`Option<String>`): zugehöriger Plan-Knoten.
/// - `generic` (`bool`): nur `/research-deps` — ökosystem-neutraler
///   `dependency-researcher` statt des Rust-Spezialisten.
/// - `ecosystem` (`Option<String>`): nur `/research-deps` — Paket-Ökosystem
///   (`npm`, `pypi`, `go`, `maven`, `cargo` …); jedes Nicht-Cargo-Ökosystem
///   wählt den `dependency-researcher`.
///
/// # Spec-Referenz
/// AP W4-03 — `/research-deps`, `/research-web`; allgemeine Recherche
/// `/research`.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct ResearchArgs {
    /// Die gebundene Frage, die das Kind beantworten soll.
    #[serde(default)]
    #[raw(join)]
    pub question: Option<String>,
    /// Paket- bzw. Crate-Namen, auf die die Recherche begrenzt ist (Alias:
    /// `crates`).
    #[serde(default, alias = "crates")]
    #[tool(default = [])]
    pub packages: Vec<String>,
    /// URLs oder URL-Präfixe, auf die die Recherche begrenzt ist.
    #[serde(default)]
    #[tool(default = [])]
    pub urls: Vec<String>,
    /// Erlaubte Quellklassen (`local_source`, `cargo_registry_source`,
    /// `package_registry_source`, `official_docs`, `repository`,
    /// `release_notes`, `standard`, `web`).
    #[serde(default)]
    #[tool(default = [])]
    pub sources: Vec<String>,
    /// Zugehöriger Plan-Knoten, an den der Nachweis gehängt wird.
    #[serde(default)]
    pub task: Option<String>,
    /// Nur `/research-deps`: ökosystem-neutralen `dependency-researcher`
    /// statt des Rust/Cargo-Spezialisten `researcher-deps` starten.
    #[serde(default)]
    #[tool(default = false)]
    pub generic: bool,
    /// Nur `/research-deps`: Paket-Ökosystem (`npm`, `pypi`, `go`, `maven`,
    /// `cargo` …). Jedes Nicht-Cargo-Ökosystem startet den
    /// `dependency-researcher`.
    #[serde(default)]
    pub ecosystem: Option<String>,
}

impl FromRawArgs for ResearchArgs {
    /// Trennt die Routing-Angaben ab und nimmt den Rest als Frage.
    ///
    /// # Beschreibung
    /// `--generic` setzt `generic`; `ecosystem=<x>`, `--ecosystem=<x>` und
    /// `--ecosystem <x>` setzen `ecosystem`. Alle übrigen Tokens werden mit
    /// einem Leerzeichen zur Frage verbunden (`#[raw(join)]`-Semantik).
    ///
    /// # Argumente
    /// - `tokens` (`&[String]`): die rohen Command-Argumente.
    ///
    /// # Rückgabe
    /// `Ok(ResearchArgs)` — die Listenfelder bleiben leer; sie sind nur über
    /// die JSON-Flächen setzbar.
    ///
    /// # Fehler
    /// - [`OpError::InvalidArguments`]: `ecosystem` ohne Wert.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        let mut generic = false;
        let mut ecosystem: Option<String> = None;
        let mut words: Vec<&str> = Vec::with_capacity(tokens.len());
        let mut iter = tokens.iter();
        while let Some(token) = iter.next() {
            let token = token.as_str();
            if token == "--generic" {
                generic = true;
            } else if token == "--ecosystem" {
                let value = iter.next().map(String::as_str).unwrap_or_default();
                ecosystem = Some(non_empty_ecosystem(value)?);
            } else if let Some(value) = token
                .strip_prefix("--ecosystem=")
                .or_else(|| token.strip_prefix("ecosystem="))
            {
                ecosystem = Some(non_empty_ecosystem(value)?);
            } else {
                words.push(token);
            }
        }
        let question = if words.is_empty() {
            None
        } else {
            Some(words.join(" "))
        };
        Ok(Self {
            question,
            packages: Vec::new(),
            urls: Vec::new(),
            sources: Vec::new(),
            task: None,
            generic,
            ecosystem,
        })
    }
}

/// Prüft eine `ecosystem`-Angabe auf Inhalt und normalisiert sie.
///
/// # Argumente
/// - `raw` (`&str`): der Wert hinter `ecosystem=`.
///
/// # Rückgabe
/// Der getrimmte, kleingeschriebene Wert.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: der Wert ist leer.
fn non_empty_ecosystem(raw: &str) -> Result<String, OpError> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(OpError::InvalidArguments(
            "ecosystem braucht einen Wert, z. B. ecosystem=npm".to_owned(),
        ));
    }
    Ok(normalized)
}

// ── Quellklassen ─────────────────────────────────────────────────────────────

/// Übersetzt eine Quellklassen-Angabe in eine [`SourceClass`].
///
/// # Beschreibung
/// Akzeptiert die kanonischen `snake_case`-Namen der Serde-Form sowie die
/// gebräuchlichen Kurzformen (`docs`, `registry`, `repo`, `changelog`, `spec`).
/// Bindestriche gelten als Unterstriche, Groß-/Kleinschreibung ist egal. Eine
/// unbekannte Angabe ist ein **Fehler** und keine stille Vorgabe: sonst würde
/// ein Tippfehler die gedachte Quellgrenze lautlos aufheben.
///
/// # Argumente
/// - `raw` (`&str`): die Angabe aus den Argumenten.
///
/// # Rückgabe
/// Die passende [`SourceClass`].
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: unbekannte Angabe; die Meldung nennt alle
///   erlaubten Werte.
///
/// # Nebenläufigkeit
/// Reine Funktion.
pub(crate) fn parse_source_class(raw: &str) -> Result<SourceClass, OpError> {
    let normalized = raw.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "local_source" | "local" => Ok(SourceClass::LocalSource),
        "cargo_registry_source" | "registry" | "crates_io" => Ok(SourceClass::CargoRegistrySource),
        "package_registry_source" | "package_registry" | "npm" | "pypi" | "maven" => {
            Ok(SourceClass::PackageRegistrySource)
        }
        "official_docs" | "docs" => Ok(SourceClass::OfficialDocs),
        "repository" | "repo" => Ok(SourceClass::Repository),
        "release_notes" | "changelog" => Ok(SourceClass::ReleaseNotes),
        "standard" | "spec" => Ok(SourceClass::Standard),
        "web" => Ok(SourceClass::Web),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannte Quellklasse '{other}'; erlaubt sind: local_source, \
             cargo_registry_source, package_registry_source, official_docs, repository, \
             release_notes, standard, web"
        ))),
    }
}

/// Löst die Quellklassen einer Anfrage auf — Angabe oder Vorgabe.
///
/// # Argumente
/// - `requested` (`&[String]`): die angeforderten Klassen; leer = Vorgabe.
/// - `fallback` (`&[SourceClass]`): die Vorgabe der jeweiligen Operation.
///
/// # Rückgabe
/// Die aufgelösten Klassen in Eingabereihenfolge.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: eine der Angaben ist unbekannt.
fn resolve_sources(
    requested: &[String],
    fallback: &[SourceClass],
) -> Result<Vec<SourceClass>, OpError> {
    if requested.is_empty() {
        return Ok(fallback.to_vec());
    }
    requested
        .iter()
        .map(|raw| parse_source_class(raw))
        .collect()
}

// ── Rollenwahl ───────────────────────────────────────────────────────────────

/// `true`, wenn `ecosystem` das Rust/Cargo-Ökosystem bezeichnet.
fn is_cargo_ecosystem(ecosystem: &str) -> bool {
    CARGO_ECOSYSTEM_ALIASES.contains(&ecosystem.trim().to_ascii_lowercase().as_str())
}

/// Wählt die Kind-Rolle von `/research-deps`.
///
/// # Beschreibung
/// - `generic == true` → [`role_names::DEPENDENCY_RESEARCHER`].
/// - ein Nicht-Cargo-`ecosystem` → [`role_names::DEPENDENCY_RESEARCHER`].
/// - sonst (auch `ecosystem=cargo`) → [`role_names::RESEARCHER_DEPS`], der
///   Rust-Spezialist bleibt die Vorgabe.
///
/// # Nebenläufigkeit
/// Reine Funktion.
fn deps_child_role(generic: bool, ecosystem: Option<&str>) -> &'static str {
    let foreign_ecosystem = ecosystem.is_some_and(|value| !is_cargo_ecosystem(value));
    if generic || foreign_ecosystem {
        role_names::DEPENDENCY_RESEARCHER
    } else {
        role_names::RESEARCHER_DEPS
    }
}

/// Erwartetes Ausgabeformat der ökosystem-neutralen Dependency-Recherche.
///
/// # Argumente
/// - `ecosystem` (`Option<&str>`): ein genanntes Ökosystem; wird angehängt.
fn generic_deps_expected_output(ecosystem: Option<&str>) -> String {
    match ecosystem {
        Some(value) => format!(
            "{GENERIC_DEPS_EXPECTED_OUTPUT} Ökosystem der Anfrage: {value} — ecosystem je \
             Eintrag entsprechend setzen."
        ),
        None => GENERIC_DEPS_EXPECTED_OUTPUT.to_owned(),
    }
}

/// Wählt die Rolle eines Rechercheurs abhängig von der aufrufenden Session.
///
/// # Beschreibung
/// UIA-Sessions (`organizational_role == AgentRoleId::UserInterface`) dürfen
/// keine `Worker`-Rolle spawnen. Für sie weicht der Kind-Lauf auf
/// [`role_names::UIA_EXPLORER`] aus (`AgentRoleId::UiaWorker`, eigenes
/// read-only Tool-Profil inklusive `web.fetch`). Kann die Rolle der
/// aufrufenden Session nicht ermittelt werden, bleibt es bei `worker_role`.
fn role_for_caller(ctx: &OpContext, worker_role: &'static str) -> &'static str {
    match ctx
        .managed_spawner()
        .and_then(|spawner| spawner.session_organizational_role(ctx.session_id()))
    {
        Some(AgentRoleId::UserInterface) => role_names::UIA_EXPLORER,
        _ => worker_role,
    }
}

/// Liefert die Authority-Reducer-Kennung für den Kind-Lauf einer Rolle.
///
/// # Beschreibung
/// - `researcher` und `dependency-researcher` bekommen die Obergrenze ihres
///   Registry-Profils `ReadOnlyResearch` aus
///   [`authority_reducer_for_role`] (`reduce_to_read_workspace_network`:
///   Workspace lesen plus egress-gebundenes Netz, nie breiter als das des
///   Elternteils). Ohne diese Zuordnung liefe ihr Kern-Werkzeug
///   `web.fetch`/`web.search` stets ins Leere. Fehlt die Zuordnung, fällt die
///   Wahl fail-closed auf `reduce_to_read_only`.
/// - Alle anderen Rollen behalten exakt die bisherige Wahl aus
///   [`crate::explore::reducer_for_role`] — `/research-deps` (Rust) und
///   `/research-web` ändern ihr Verhalten nicht.
///
/// # Nebenläufigkeit
/// Reine Funktion.
fn child_reducer(role: &str) -> &'static str {
    if role == role_names::RESEARCHER || role == role_names::DEPENDENCY_RESEARCHER {
        authority_reducer_for_role(role).map_or(READ_ONLY_REDUCER, |reducer| reducer.id())
    } else {
        reducer_for_role(role)
    }
}

/// Startet genau einen Recherche-Kind-Lauf und validiert das Finding.
///
/// # Beschreibung
/// Wie [`crate::explore::run_single_child`], aber mit dem Reducer aus
/// [`child_reducer`]. Budget ([`SINGLE_CHILD_BUDGET`]), Join-Semantik und
/// Return-Contract (`ResearchFinding`) sind identisch.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein Agent-Spawner im Kontext.
/// - [`OpError::Execution`]: Kind-Lauf oder Vertrag schlugen fehl.
async fn run_research_child(
    ctx: &OpContext,
    role: &str,
    payload: serde_json::Value,
) -> Result<ResearchFinding, OpError> {
    let budget = parse_budget_hint(SINGLE_CHILD_BUDGET)?;
    let mut results = fanout_children(
        ctx,
        role,
        std::slice::from_ref(&payload),
        child_reducer(role),
        budget,
        1,
        JoinSemantics::AllTerminal,
        ChildReturnContract::ResearchFinding,
    )
    .await?;

    let Some(outcome) = results.pop() else {
        return Err(OpError::Execution(
            "der Fan-out lieferte für den Einzel-Lauf kein Ergebnis".to_owned(),
        ));
    };
    let value = outcome.map_err(OpError::Execution)?;
    finding_from_value(value)
}

/// Weist die `/research-deps`-Routing-Angaben an anderen Operationen ab.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: `generic` oder `ecosystem` ist gesetzt —
///   ein stilles Ignorieren würde eine gemeinte Rollenwahl verschlucken.
fn reject_deps_routing(args: &ResearchArgs, command: &str) -> Result<(), OpError> {
    if args.generic || args.ecosystem.is_some() {
        return Err(OpError::InvalidArguments(format!(
            "--generic und ecosystem=<x> gelten nur für /research-deps, nicht für {command}"
        )));
    }
    Ok(())
}

// ── Gemeinsamer Ablauf ───────────────────────────────────────────────────────

/// Die Teile, in denen sich die Recherche-Operationen unterscheiden.
struct ResearchProfile {
    /// Rollenname des Kindes.
    role: &'static str,
    /// Namensraum des abgeleiteten Frage-Slugs.
    slug_prefix: &'static str,
    /// Akteur-Kennung für Plan-Mutationen.
    actor: &'static str,
    /// Beschreibung des erwarteten Ausgabeformats.
    expected_output: String,
    /// Stop-Bedingung der Recherche.
    stop_condition: &'static str,
    /// Vorgabe-Quellklassen, wenn der Aufrufer keine nennt.
    default_sources: &'static [SourceClass],
}

/// Führt den gemeinsamen Recherche-Ablauf aller Operationen aus.
///
/// # Beschreibung
/// Prüft die Frage, baut die [`ResearchQuestion`] mit dem Scope aus
/// `packages`, `urls` und den aufgelösten Quellklassen, fährt einen Kind-Lauf
/// und legt das Ergebnis ab, sofern `task`, Finding-Store und Plan vorliegen.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext.
/// - `args` ([`ResearchArgs`]): die Argumente; werden konsumiert.
/// - `profile` (`ResearchProfile`): die operationsspezifischen Teile.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit dem validierten Finding als JSON.
///
/// # Fehler
/// Siehe Modul-Dokumentation.
async fn run_research(
    ctx: &OpContext,
    args: ResearchArgs,
    profile: ResearchProfile,
) -> Result<OpOutput, OpError> {
    let ResearchArgs {
        question,
        packages,
        urls,
        sources,
        task,
        generic: _,
        ecosystem: _,
    } = args;

    let question = question
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            OpError::InvalidArguments("eine Frage ist erforderlich: <command> <frage>".to_owned())
        })?
        .to_owned();

    let sources = resolve_sources(&sources, profile.default_sources)?;

    let id = match task.as_deref() {
        Some(node) => node.to_owned(),
        None => question_slug(profile.slug_prefix, &question),
    };

    let research_question = ResearchQuestion {
        id: QuestionId::new(id),
        question,
        scope: QuestionScope {
            paths: Vec::new(),
            crates: packages,
            urls,
            sources,
        },
        expected_output: profile.expected_output,
        freshness: Freshness::AnyTime,
        stop_condition: profile.stop_condition.to_owned(),
        owner_task: task.clone(),
    };

    let payload = child_payload(&research_question)?;
    let finding = run_research_child(ctx, profile.role, payload).await?;
    let locator = persist_finding(ctx, task.as_deref(), &finding, profile.actor)?;
    finding_output(&finding, locator.as_deref())
}

// ── Operationen ──────────────────────────────────────────────────────────────

/// Recherchiert eine allgemeine Frage über den `researcher`-Kindagenten.
///
/// # Beschreibung
/// Allgemeine Recherche zu beliebigen Themen (Technik, Standards, Markt,
/// Business, Fachdokumente). Das Kind liest Workspace-Dokumente und PDFs und
/// recherchiert egress-gebunden im Netz; sein Finding trägt das analytische
/// Handwerk der Rolle (Hypothesen, Schlüsselannahmen, Quellenbewertung,
/// likelihood getrennt von confidence). Vorgabe-Quellklassen:
/// [`SourceClass::LocalSource`], [`SourceClass::OfficialDocs`],
/// [`SourceClass::Standard`], [`SourceClass::Repository`],
/// [`SourceClass::ReleaseNotes`], [`SourceClass::Web`].
///
/// UIA-Aufrufer dürfen keinen `Worker` spawnen; für sie läuft die Recherche
/// über `uia-explorer` (siehe Modul-Dokumentation).
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext mit Spawner, Plan- und
///   Finding-Store.
/// - `args` ([`ResearchArgs`]): die Argumente; werden konsumiert.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit dem validierten Finding als eingerücktem JSON.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: keine Frage, unbekannte Quellklasse oder
///   `--generic`/`ecosystem=` angegeben.
/// - [`OpError::NotAvailable`]: kein Agent-Spawner/StateStore im Kontext.
/// - [`OpError::Execution`]: Kind-Lauf, Vertrag oder Persistenz schlugen fehl.
///
/// # Nebenläufigkeit
/// Zustandslos; der Kind-Lauf serialisiert sich über den `ManagedAgentSpawner`.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run().
/// ```
#[operation(
    name = "research",
    summary = "Recherchiert eine allgemeine Frage (Technik, Markt, Dokumente) über einen read-only Kindagenten.",
    domain = "agents",
    permission = "operator",
    command(path = "/research", visibility = "channel_parity"),
    model_tool(readonly, approval = "none"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: read-only
    // Kindagent, keine Mutation.
    web(path = "/api/research", method = "get", approval = "none"),
    agent_tool(
        child = "researcher",
        authority = "reduce_to_read_workspace_network",
        budget = "60k_tokens,40_tool_calls,180s"
    )
)]
async fn research(ctx: &OpContext, args: ResearchArgs) -> Result<OpOutput, OpError> {
    reject_deps_routing(&args, "/research")?;
    run_research(
        ctx,
        args,
        ResearchProfile {
            role: role_for_caller(ctx, role_names::RESEARCHER),
            slug_prefix: "research",
            actor: ACTOR_RESEARCH,
            expected_output: GENERAL_EXPECTED_OUTPUT.to_owned(),
            stop_condition: GENERAL_STOP_CONDITION,
            default_sources: GENERAL_DEFAULT_SOURCES,
        },
    )
    .await
}

/// Recherchiert Dependency-Fakten — Rust-Spezialist oder ökosystem-neutral.
///
/// # Beschreibung
/// Vorgabe ist der Rust/Cargo-Spezialist `researcher-deps`: er arbeitet gegen
/// `Cargo.lock` und den lokalen Registry-Quellcache; die Vorgabe-Quellklassen
/// sind [`SourceClass::CargoRegistrySource`], [`SourceClass::OfficialDocs`]
/// und [`SourceClass::ReleaseNotes`].
///
/// Mit `--generic` (bzw. `generic: true`) oder einem Nicht-Cargo-`ecosystem`
/// startet stattdessen der ökosystem-neutrale `dependency-researcher`
/// (Manifeste/Lockfiles im Workspace plus offizielle Paket-Registries); die
/// Vorgabe-Quellklassen sind dann [`SourceClass::LocalSource`],
/// [`SourceClass::PackageRegistrySource`],
/// [`SourceClass::CargoRegistrySource`], [`SourceClass::OfficialDocs`] und
/// [`SourceClass::ReleaseNotes`], das erwartete Ausgabeformat ist
/// sprachneutral (`package`/`ecosystem` je Eintrag). Der Aufrufer kann die
/// Quellklassen in beiden Fällen über `sources` überschreiben.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext mit Spawner, Plan- und
///   Finding-Store.
/// - `args` ([`ResearchArgs`]): die Argumente; werden konsumiert.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit dem validierten Finding als eingerücktem JSON.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: keine Frage, unbekannte Quellklasse oder
///   leeres `ecosystem`.
/// - [`OpError::NotAvailable`]: kein Agent-Spawner/StateStore im Kontext.
/// - [`OpError::Execution`]: Kind-Lauf, Vertrag oder Persistenz schlugen fehl.
///
/// # Nebenläufigkeit
/// Zustandslos; der Kind-Lauf serialisiert sich über den `ManagedAgentSpawner`.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run().
/// ```
#[operation(
    name = "research_deps",
    summary = "Recherchiert Dependency-Fakten (Versionen, MSRV, Features) über einen read-only Kindagenten; --generic bzw. ecosystem=<x> für andere Ökosysteme.",
    domain = "agents",
    permission = "operator",
    command(path = "/research-deps", visibility = "channel_parity"),
    model_tool(readonly, approval = "none"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: read-only
    // Kindagent, keine Mutation.
    web(path = "/api/research-deps", method = "get", approval = "none"),
    agent_tool(
        child = "researcher-deps",
        authority = "reduce_to_read_only",
        budget = "60k_tokens,40_tool_calls,180s"
    )
)]
async fn research_deps(ctx: &OpContext, args: ResearchArgs) -> Result<OpOutput, OpError> {
    let ecosystem = match args.ecosystem.as_deref() {
        Some(raw) => Some(non_empty_ecosystem(raw)?),
        None => None,
    };
    let role = deps_child_role(args.generic, ecosystem.as_deref());
    let profile = if role == role_names::DEPENDENCY_RESEARCHER {
        ResearchProfile {
            role,
            slug_prefix: "research-deps",
            actor: ACTOR_RESEARCH_DEPS,
            expected_output: generic_deps_expected_output(ecosystem.as_deref()),
            stop_condition: GENERIC_DEPS_STOP_CONDITION,
            default_sources: GENERIC_DEPS_DEFAULT_SOURCES,
        }
    } else {
        ResearchProfile {
            role,
            slug_prefix: "research-deps",
            actor: ACTOR_RESEARCH_DEPS,
            expected_output: DEPS_EXPECTED_OUTPUT.to_owned(),
            stop_condition: DEPS_STOP_CONDITION,
            default_sources: DEPS_DEFAULT_SOURCES,
        }
    };
    run_research(ctx, args, profile).await
}

/// Recherchiert im Web über den `researcher-web`-Kindagenten.
///
/// # Beschreibung
/// Das Kind arbeitet gegen die Host-Allowlist der Sandbox; die
/// Vorgabe-Quellklassen sind [`SourceClass::OfficialDocs`],
/// [`SourceClass::Standard`], [`SourceClass::Repository`] und
/// [`SourceClass::Web`]. Der Aufrufer kann sie über `sources` überschreiben und
/// den Suchraum über `urls` einengen.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext mit Spawner, Plan- und
///   Finding-Store.
/// - `args` ([`ResearchArgs`]): die Argumente; werden konsumiert.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit dem validierten Finding als eingerücktem JSON.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: keine Frage, unbekannte Quellklasse oder
///   `--generic`/`ecosystem=` angegeben.
/// - [`OpError::NotAvailable`]: kein Agent-Spawner/StateStore im Kontext.
/// - [`OpError::Execution`]: Kind-Lauf, Vertrag oder Persistenz schlugen fehl.
///
/// # Nebenläufigkeit
/// Zustandslos; der Kind-Lauf serialisiert sich über den `ManagedAgentSpawner`.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run().
/// ```
#[operation(
    name = "research_web",
    summary = "Recherchiert eine gebundene Frage im Web über einen read-only Kindagenten.",
    domain = "agents",
    permission = "operator",
    command(path = "/research-web", visibility = "channel_parity"),
    model_tool(readonly, approval = "none"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: read-only
    // Kindagent, keine Mutation.
    web(path = "/api/research-web", method = "get", approval = "none"),
    agent_tool(
        child = "researcher-web",
        authority = "reduce_to_read_only",
        budget = "60k_tokens,40_tool_calls,180s"
    )
)]
async fn research_web(ctx: &OpContext, args: ResearchArgs) -> Result<OpOutput, OpError> {
    reject_deps_routing(&args, "/research-web")?;
    // UIA-Chat-Sessions (`organizational_role == AgentRoleId::UserInterface`)
    // dürfen keine `Worker`-Rolle spawnen — `researcher-web` trägt
    // `organizational_role = AgentRoleId::Worker`. Siehe `role_for_caller`.
    run_research(
        ctx,
        args,
        ResearchProfile {
            role: role_for_caller(ctx, role_names::RESEARCHER_WEB),
            slug_prefix: "research-web",
            actor: ACTOR_RESEARCH_WEB,
            expected_output: WEB_EXPECTED_OUTPUT.to_owned(),
            stop_condition: WEB_STOP_CONDITION,
            default_sources: WEB_DEFAULT_SOURCES,
        },
    )
    .await
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        GENERIC_DEPS_EXPECTED_OUTPUT, RESEARCHER_CHILD, RESEARCHER_DEPS_CHILD,
        RESEARCHER_WEB_CHILD, ResearchArgs, ResearchDepsOperation, ResearchOperation,
        ResearchWebOperation, child_reducer, deps_child_role, generic_deps_expected_output,
        parse_source_class, resolve_sources,
    };
    use crate::explore::reducer_for_role;
    use crate::test_support::{TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, OpContext, OpError, Operation, Surface};
    use harw_registry_defaults::authority_reducer_for_role;
    use harw_registry_defaults::profile::role_names;
    use harw_research::SourceClass;
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Baut einen minimalen [`OpContext`] mit leerer [`ServiceMap`].
    fn test_context() -> TestResult<(OpContext, std::path::PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-research-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).map_err(ctx("Test-Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace-Binding auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        ))
    }

    /// Liest `child_name` und `authority_reducer` der `AgentTool`-Fläche.
    fn declared_agent_tool(
        meta: &harw_operations::OperationMeta,
    ) -> Option<(&'static str, &'static str)> {
        meta.surfaces.iter().find_map(|surface| match surface {
            Surface::AgentTool {
                child_name,
                authority_reducer,
                ..
            } => Some((*child_name, *authority_reducer)),
            _ => None,
        })
    }

    /// Liest den `child_name` der `AgentTool`-Fläche einer Operation.
    fn declared_child(meta: &harw_operations::OperationMeta) -> Option<&'static str> {
        declared_agent_tool(meta).map(|(child, _)| child)
    }

    #[test]
    fn test_research_args_from_raw_args_joins_tokens_into_question() -> TestResult {
        let args = ResearchArgs::from_raw_args(&toks(&["welche", "MSRV", "hat", "serde"]))
            .map_err(ctx("ResearchArgs::from_raw_args"))?;
        assert_eq!(args.question.as_deref(), Some("welche MSRV hat serde"));
        assert!(args.packages.is_empty());
        assert!(args.urls.is_empty());
        assert!(args.sources.is_empty());
        assert!(args.task.is_none());
        assert!(!args.generic);
        assert!(args.ecosystem.is_none());
        Ok(())
    }

    #[test]
    fn test_research_args_from_raw_args_empty_tokens_yields_no_question() -> TestResult {
        let args =
            ResearchArgs::from_raw_args(&toks(&[])).map_err(ctx("ResearchArgs::from_raw_args"))?;
        assert!(args.question.is_none());
        Ok(())
    }

    #[test]
    fn test_research_args_from_raw_args_extracts_generic_flag() -> TestResult {
        let args = ResearchArgs::from_raw_args(&toks(&["--generic", "welche", "Lizenz"]))
            .map_err(ctx("ResearchArgs::from_raw_args"))?;
        assert!(args.generic);
        assert_eq!(args.question.as_deref(), Some("welche Lizenz"));
        Ok(())
    }

    #[test]
    fn test_research_args_from_raw_args_extracts_ecosystem_in_all_spellings() -> TestResult {
        for tokens in [
            toks(&["ecosystem=NPM", "welche", "Version"]),
            toks(&["welche", "--ecosystem=npm", "Version"]),
            toks(&["welche", "Version", "--ecosystem", "npm"]),
        ] {
            let args =
                ResearchArgs::from_raw_args(&tokens).map_err(ctx("ResearchArgs::from_raw_args"))?;
            assert_eq!(args.ecosystem.as_deref(), Some("npm"), "{tokens:?}");
            assert_eq!(
                args.question.as_deref(),
                Some("welche Version"),
                "{tokens:?}"
            );
            assert!(!args.generic);
        }
        Ok(())
    }

    #[test]
    fn test_research_args_from_raw_args_rejects_empty_ecosystem() {
        for tokens in [
            toks(&["ecosystem=", "frage"]),
            toks(&["frage", "--ecosystem"]),
        ] {
            assert!(
                matches!(
                    ResearchArgs::from_raw_args(&tokens),
                    Err(OpError::InvalidArguments(_))
                ),
                "{tokens:?}"
            );
        }
    }

    #[test]
    fn test_research_args_accepts_packages_and_legacy_crates_alias() -> TestResult {
        let new: ResearchArgs =
            serde_json::from_value(serde_json::json!({"question": "q", "packages": ["left-pad"]}))
                .map_err(ctx("packages deserialisieren"))?;
        assert_eq!(new.packages, vec!["left-pad".to_owned()]);

        let legacy: ResearchArgs =
            serde_json::from_value(serde_json::json!({"question": "q", "crates": ["serde"]}))
                .map_err(ctx("crates-Alias deserialisieren"))?;
        assert_eq!(legacy.packages, vec!["serde".to_owned()]);
        assert!(!legacy.generic);
        assert!(legacy.ecosystem.is_none());
        Ok(())
    }

    #[test]
    fn test_parse_source_class_accepts_canonical_and_short_forms() {
        assert_eq!(
            parse_source_class("cargo_registry_source"),
            Ok(SourceClass::CargoRegistrySource)
        );
        assert_eq!(
            parse_source_class("Official-Docs"),
            Ok(SourceClass::OfficialDocs)
        );
        assert_eq!(parse_source_class(" repo "), Ok(SourceClass::Repository));
        assert_eq!(parse_source_class("web"), Ok(SourceClass::Web));
    }

    #[test]
    fn test_parse_source_class_rejects_unknown_value() {
        assert!(matches!(
            parse_source_class("bananas"),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn test_resolve_sources_falls_back_when_none_requested() -> TestResult {
        let sources =
            resolve_sources(&[], super::DEPS_DEFAULT_SOURCES).map_err(ctx("resolve_sources"))?;
        assert_eq!(sources, super::DEPS_DEFAULT_SOURCES.to_vec());
        Ok(())
    }

    #[test]
    fn test_resolve_sources_uses_requested_values_in_order() -> TestResult {
        let requested = toks(&["web", "docs"]);
        let sources = resolve_sources(&requested, super::DEPS_DEFAULT_SOURCES)
            .map_err(ctx("resolve_sources"))?;
        assert_eq!(sources, vec![SourceClass::Web, SourceClass::OfficialDocs]);
        Ok(())
    }

    #[test]
    fn test_generic_deps_default_sources_are_language_neutral() {
        assert!(super::GENERIC_DEPS_DEFAULT_SOURCES.contains(&SourceClass::PackageRegistrySource));
        assert!(super::GENERIC_DEPS_DEFAULT_SOURCES.contains(&SourceClass::LocalSource));
    }

    #[test]
    fn test_deps_child_role_defaults_to_rust_specialist() {
        assert_eq!(deps_child_role(false, None), role_names::RESEARCHER_DEPS);
        assert_eq!(
            deps_child_role(false, Some("cargo")),
            role_names::RESEARCHER_DEPS
        );
        assert_eq!(
            deps_child_role(false, Some("Crates.io")),
            role_names::RESEARCHER_DEPS
        );
    }

    #[test]
    fn test_deps_child_role_generic_or_foreign_ecosystem_uses_dependency_researcher() {
        assert_eq!(
            deps_child_role(true, None),
            role_names::DEPENDENCY_RESEARCHER
        );
        assert_eq!(
            deps_child_role(true, Some("cargo")),
            role_names::DEPENDENCY_RESEARCHER
        );
        for ecosystem in ["npm", "pypi", "go", "maven"] {
            assert_eq!(
                deps_child_role(false, Some(ecosystem)),
                role_names::DEPENDENCY_RESEARCHER,
                "{ecosystem}"
            );
        }
    }

    #[test]
    fn test_generic_deps_expected_output_is_language_neutral_and_names_ecosystem() {
        assert_eq!(
            generic_deps_expected_output(None),
            GENERIC_DEPS_EXPECTED_OUTPUT
        );
        assert!(GENERIC_DEPS_EXPECTED_OUTPUT.contains("package"));
        assert!(GENERIC_DEPS_EXPECTED_OUTPUT.contains("ecosystem"));
        assert!(!GENERIC_DEPS_EXPECTED_OUTPUT.contains("MSRV"));
        assert!(generic_deps_expected_output(Some("npm")).contains("npm"));
    }

    #[test]
    fn test_child_reducer_uses_registry_ceiling_for_research_profile_roles() {
        for role in [role_names::RESEARCHER, role_names::DEPENDENCY_RESEARCHER] {
            let expected = authority_reducer_for_role(role).map(|reducer| reducer.id());
            assert_eq!(Some(child_reducer(role)), expected, "{role}");
            assert_eq!(child_reducer(role), "reduce_to_read_workspace_network");
        }
    }

    #[test]
    fn test_child_reducer_keeps_existing_choice_for_other_roles() {
        for role in [
            role_names::RESEARCHER_DEPS,
            role_names::RESEARCHER_WEB,
            role_names::UIA_EXPLORER,
        ] {
            assert_eq!(child_reducer(role), reducer_for_role(role), "{role}");
        }
    }

    #[test]
    fn test_research_child_literal_matches_role_names() {
        assert_eq!(RESEARCHER_CHILD, role_names::RESEARCHER);
        assert_eq!(
            declared_child(ResearchOperation.meta()),
            Some(role_names::RESEARCHER),
            "die agent_tool-Deklaration muss auf role_names::RESEARCHER zeigen"
        );
    }

    #[test]
    fn test_research_declared_authority_matches_role_ceiling() {
        let declared = declared_agent_tool(ResearchOperation.meta()).map(|(_, reducer)| reducer);
        assert_eq!(
            declared,
            authority_reducer_for_role(role_names::RESEARCHER).map(|reducer| reducer.id())
        );
    }

    #[test]
    fn test_research_deps_child_literal_matches_role_names() {
        assert_eq!(RESEARCHER_DEPS_CHILD, role_names::RESEARCHER_DEPS);
        assert_eq!(
            declared_child(ResearchDepsOperation.meta()),
            Some(role_names::RESEARCHER_DEPS),
            "die agent_tool-Deklaration muss auf role_names::RESEARCHER_DEPS zeigen"
        );
    }

    #[test]
    fn test_research_web_child_literal_matches_role_names() {
        assert_eq!(RESEARCHER_WEB_CHILD, role_names::RESEARCHER_WEB);
        assert_eq!(
            declared_child(ResearchWebOperation.meta()),
            Some(role_names::RESEARCHER_WEB),
            "die agent_tool-Deklaration muss auf role_names::RESEARCHER_WEB zeigen"
        );
    }

    #[test]
    fn test_all_research_roles_are_known_builtin_roles() {
        assert!(role_names::ALL.contains(&RESEARCHER_CHILD));
        assert!(role_names::ALL.contains(&RESEARCHER_DEPS_CHILD));
        assert!(role_names::ALL.contains(&RESEARCHER_WEB_CHILD));
        assert!(role_names::ALL.contains(&role_names::DEPENDENCY_RESEARCHER));
        assert!(role_names::ALL.contains(&role_names::UIA_EXPLORER));
    }

    #[test]
    fn test_research_operations_declare_distinct_command_paths() {
        let general = ResearchOperation.meta();
        let deps = ResearchDepsOperation.meta();
        let web = ResearchWebOperation.meta();
        assert_eq!(general.name, "research");
        assert_eq!(deps.name, "research_deps");
        assert_eq!(web.name, "research_web");
        assert!(general.surfaces.iter().any(
            |surface| matches!(surface, Surface::Command { path, .. } if *path == "/research")
        ));
        assert!(deps.surfaces.iter().any(
            |surface| matches!(surface, Surface::Command { path, .. } if *path == "/research-deps")
        ));
        assert!(web.surfaces.iter().any(
            |surface| matches!(surface, Surface::Command { path, .. } if *path == "/research-web")
        ));
    }

    #[tokio::test]
    async fn test_research_deps_without_spawner_is_not_available() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let result = super::research_deps(
            &op_ctx,
            ResearchArgs {
                question: Some("welche MSRV hat serde".to_owned()),
                ..ResearchArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "ohne Agent-Spawner muss /research-deps fail-closed sein, war: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_research_deps_generic_without_spawner_is_not_available() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let result = super::research_deps(
            &op_ctx,
            ResearchArgs {
                question: Some("welche Lizenz hat left-pad".to_owned()),
                generic: true,
                ecosystem: Some("npm".to_owned()),
                ..ResearchArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "ohne Agent-Spawner muss /research-deps --generic fail-closed sein, war: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_research_deps_with_blank_ecosystem_is_invalid_arguments() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let result = super::research_deps(
            &op_ctx,
            ResearchArgs {
                question: Some("welche Version".to_owned()),
                ecosystem: Some("  ".to_owned()),
                ..ResearchArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn test_research_without_spawner_is_not_available() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let result = super::research(
            &op_ctx,
            ResearchArgs {
                question: Some("wie groß ist der Markt für Edge-Runtimes".to_owned()),
                ..ResearchArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "ohne Agent-Spawner muss /research fail-closed sein, war: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_research_without_question_is_invalid_arguments() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let result = super::research(&op_ctx, ResearchArgs::default()).await;
        std::fs::remove_dir_all(root).ok();

        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn test_research_and_research_web_reject_deps_routing_flags() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let generic = ResearchArgs {
            question: Some("frage".to_owned()),
            generic: true,
            ..ResearchArgs::default()
        };
        let ecosystem = ResearchArgs {
            question: Some("frage".to_owned()),
            ecosystem: Some("npm".to_owned()),
            ..ResearchArgs::default()
        };
        let general = super::research(&op_ctx, generic).await;
        let web = super::research_web(&op_ctx, ecosystem).await;
        std::fs::remove_dir_all(root).ok();

        assert!(
            matches!(general, Err(OpError::InvalidArguments(_))),
            "{general:?}"
        );
        assert!(matches!(web, Err(OpError::InvalidArguments(_))), "{web:?}");
        Ok(())
    }

    #[tokio::test]
    async fn test_research_web_without_question_is_invalid_arguments() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let result = super::research_web(&op_ctx, ResearchArgs::default()).await;
        std::fs::remove_dir_all(root).ok();

        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn test_research_web_with_unknown_source_is_invalid_arguments() -> TestResult {
        let (op_ctx, root) = test_context()?;
        let result = super::research_web(
            &op_ctx,
            ResearchArgs {
                question: Some("was ist neu in Rust 2024".to_owned()),
                sources: toks(&["bananas"]),
                ..ResearchArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        assert!(
            matches!(result, Err(OpError::InvalidArguments(_))),
            "eine unbekannte Quellklasse darf nicht still zur Vorgabe werden, war: {result:?}"
        );
        Ok(())
    }
}
