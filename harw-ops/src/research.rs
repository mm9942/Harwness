//! `/research-deps` und `/research-web` — gebundene Recherche durch read-only Kinder.
//!
//! # Verantwortungsbereich
//! Implementiert die beiden Recherche-Operationen aus AP W4-03. Beide bauen aus
//! denselben Argumenten eine [`ResearchQuestion`](harw_research::ResearchQuestion),
//! starten **einen** Kind-Lauf und liefern das gegen den `ResearchFinding`-Vertrag
//! validierte Ergebnis. Sie unterscheiden sich in genau zwei Punkten: der Rolle
//! des Kindes und den Vorgabe-Quellklassen.
//!
//! # Warum zwei Operationen und nicht eine mit Parameter
//! `Surface::AgentTool::child_name` ist ein `&'static str`, der zur Compile-Zeit
//! aus der `#[operation]`-Deklaration stammt. Eine einzige Operation könnte die
//! Rolle also nicht zur Laufzeit umschalten, ohne ihre eigene Deklaration zu
//! widerlegen. Zwei Operationen sind die ehrliche Form desselben Ablaufs.
//!
//! # Warum String-Literale statt `role_names`-Konstanten
//! Das `#[operation]`-Makro parst `agent_tool(child = …)` als `syn::LitStr` —
//! ein Konstanten-Pfad ist dort ein Compile-Fehler (siehe
//! `harw-macros/src/operation.rs`, `parse_operation_args`). Die Literale werden
//! deshalb in `RESEARCHER_DEPS_CHILD` und `RESEARCHER_WEB_CHILD` gespiegelt
//! und in den Tests gegen
//! [`role_names`](harw_registry_defaults::profile::role_names) geprüft — ein
//! Auseinanderlaufen ist damit ein Testfehler, kein stiller Rollenfehler zur
//! Laufzeit.
//!
//! # Schlüsseltypen
//! - [`ResearchArgs`] — gemeinsamer Argument-Container beider Operationen.
//! - `ResearchDepsOperation`, `ResearchWebOperation` — vom `#[operation]`-Makro
//!   erzeugte Op-Structs.
//!
//! # Nebenläufigkeit
//! Beide Op-Structs sind zustandslose Unit-Structs → `Send + Sync`.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`]: keine Frage, oder eine unbekannte
//!   Quellklasse in `sources`.
//! - [`OpError::NotAvailable`]: kein Agent-Spawner/StateStore im Kontext.
//! - [`OpError::Execution`]: Kind-Lauf, Return-Contract oder Persistenz
//!   schlugen fehl.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::research::ResearchArgs;
//! use harw_operations::FromRawArgs;
//!
//! let args = ResearchArgs::from_raw_args(&["welche".to_owned(), "MSRV".to_owned()])
//!     .expect("Argument-Parsing schlägt hier nicht fehl");
//! assert_eq!(args.question.as_deref(), Some("welche MSRV"));
//! ```

use harw_agent_dsl::roles::AgentRoleId;
use harw_core_bridge::OpContextCoreExt;
use harw_macros::operation;
use harw_operations::args::join_all_optional;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
use harw_registry_defaults::profile::role_names;
use harw_research::{Freshness, QuestionId, QuestionScope, ResearchQuestion, SourceClass};

use crate::explore::{
    child_payload, finding_output, persist_finding, question_slug, run_single_child,
};

// ── Konstanten ───────────────────────────────────────────────────────────────

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

/// Akteur-Kennung für Plan-Mutationen aus der Dependency-Recherche.
const ACTOR_RESEARCH_DEPS: &str = "op:research-deps";

/// Akteur-Kennung für Plan-Mutationen aus der Web-Recherche.
const ACTOR_RESEARCH_WEB: &str = "op:research-web";

/// Vorgabe-Quellklassen der Dependency-Recherche.
const DEPS_DEFAULT_SOURCES: &[SourceClass] = &[
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

/// Erwartetes Ausgabeformat der Dependency-Recherche.
const DEPS_EXPECTED_OUTPUT: &str = "Ein ResearchFinding mit verified_versions: je Crate die \
     geprüfte Version, die MSRV und die benutzten Features, jeweils belegt durch die Quelle, aus \
     der der Wert stammt (Lockfile-Eintrag, Registry-Manifest oder offizielle Doku).";

/// Erwartetes Ausgabeformat der Web-Recherche.
const WEB_EXPECTED_OUTPUT: &str = "Ein ResearchFinding, dessen Belege jeweils URL, Abrufzeitpunkt \
     und wörtlichen Auszug tragen. Aussagen ohne abrufbare Quelle gehören in \
     unresolved_questions, nicht in die Schlussfolgerung.";

/// Stop-Bedingung der Dependency-Recherche.
const DEPS_STOP_CONDITION: &str = "Für jedes genannte Crate liegen Version, MSRV und relevante \
     Features belegt vor — oder es ist belegt, dass die Quelle sie nicht ausweist.";

/// Stop-Bedingung der Web-Recherche.
const WEB_STOP_CONDITION: &str = "Die Frage ist aus den erlaubten Quellklassen belegt beantwortet, \
     oder die erlaubten Quellen enthalten die Antwort nachweislich nicht.";

// ── Argumente ────────────────────────────────────────────────────────────────

/// Eingabe-Argumente beider Recherche-Operationen.
///
/// # Beschreibung
/// Auf der Command-Fläche ist die gesamte Token-Zeile die Frage; die Listen
/// (`crates`, `urls`, `sources`) sind über die JSON-Flächen erreichbar. Das ist
/// dieselbe Aufteilung wie bei [`crate::explore::ExploreArgs`] und aus demselben
/// Grund von Hand implementiert (siehe dort).
///
/// # Felder
/// - `question` (`Option<String>`): die gebundene Frage.
/// - `crates` (`Vec<String>`): Crate-Namen, auf die die Recherche begrenzt ist.
/// - `urls` (`Vec<String>`): URLs oder URL-Präfixe im erlaubten Scope.
/// - `sources` (`Vec<String>`): Quellklassen als Text; siehe
///   `parse_source_class`. Leer = die Vorgabe der jeweiligen Operation.
/// - `task` (`Option<String>`): zugehöriger Plan-Knoten.
///
/// # Spec-Referenz
/// AP W4-03 — `/research-deps`, `/research-web`.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct ResearchArgs {
    /// Die gebundene Frage, die das Kind beantworten soll.
    #[serde(default)]
    #[raw(join)]
    pub question: Option<String>,
    /// Crate-Namen, auf die die Recherche begrenzt ist.
    #[serde(default)]
    #[tool(default = [])]
    pub crates: Vec<String>,
    /// URLs oder URL-Präfixe, auf die die Recherche begrenzt ist.
    #[serde(default)]
    #[tool(default = [])]
    pub urls: Vec<String>,
    /// Erlaubte Quellklassen (`local_source`, `cargo_registry_source`,
    /// `official_docs`, `repository`, `release_notes`, `standard`, `web`).
    #[serde(default)]
    #[tool(default = [])]
    pub sources: Vec<String>,
    /// Zugehöriger Plan-Knoten, an den der Nachweis gehängt wird.
    #[serde(default)]
    pub task: Option<String>,
}

impl FromRawArgs for ResearchArgs {
    /// Nimmt die gesamte Token-Zeile als Frage (`#[raw(join)]`-Semantik).
    ///
    /// # Argumente
    /// - `tokens` (`&[String]`): die rohen Command-Argumente.
    ///
    /// # Rückgabe
    /// `Ok(ResearchArgs)` — die Listenfelder bleiben leer; sie sind nur über
    /// die JSON-Flächen setzbar.
    ///
    /// # Fehler
    /// Keine — diese Implementierung schlägt nie fehl.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            question: join_all_optional(tokens),
            crates: Vec::new(),
            urls: Vec::new(),
            sources: Vec::new(),
            task: None,
        })
    }
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

// ── Gemeinsamer Ablauf ───────────────────────────────────────────────────────

/// Die Teile, in denen sich die beiden Recherche-Operationen unterscheiden.
struct ResearchProfile {
    /// Rollenname des Kindes.
    role: &'static str,
    /// Namensraum des abgeleiteten Frage-Slugs.
    slug_prefix: &'static str,
    /// Akteur-Kennung für Plan-Mutationen.
    actor: &'static str,
    /// Beschreibung des erwarteten Ausgabeformats.
    expected_output: &'static str,
    /// Stop-Bedingung der Recherche.
    stop_condition: &'static str,
    /// Vorgabe-Quellklassen, wenn der Aufrufer keine nennt.
    default_sources: &'static [SourceClass],
}

/// Führt den gemeinsamen Recherche-Ablauf beider Operationen aus.
///
/// # Beschreibung
/// Prüft die Frage, baut die [`ResearchQuestion`] mit dem Scope aus `crates`,
/// `urls` und den aufgelösten Quellklassen, fährt einen Kind-Lauf und legt das
/// Ergebnis ab, sofern `task`, Finding-Store und Plan vorliegen.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext.
/// - `args` ([`ResearchArgs`]): die Argumente; werden konsumiert.
/// - `profile` (`&ResearchProfile`): die operationsspezifischen Teile.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit dem validierten Finding als JSON.
///
/// # Fehler
/// Siehe Modul-Dokumentation.
async fn run_research(
    ctx: &OpContext,
    args: ResearchArgs,
    profile: &ResearchProfile,
) -> Result<OpOutput, OpError> {
    let ResearchArgs {
        question,
        crates,
        urls,
        sources,
        task,
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
            crates,
            urls,
            sources,
        },
        expected_output: profile.expected_output.to_owned(),
        freshness: Freshness::AnyTime,
        stop_condition: profile.stop_condition.to_owned(),
        owner_task: task.clone(),
    };

    let payload = child_payload(&research_question)?;
    let finding = run_single_child(ctx, profile.role, payload).await?;
    let locator = persist_finding(ctx, task.as_deref(), &finding, profile.actor)?;
    finding_output(&finding, locator.as_deref())
}

// ── Operationen ──────────────────────────────────────────────────────────────

/// Recherchiert Dependency-Fakten über den `researcher-deps`-Kindagenten.
///
/// # Beschreibung
/// Das Kind arbeitet gegen `Cargo.lock` und den lokalen Registry-Quellcache;
/// die Vorgabe-Quellklassen sind entsprechend
/// [`SourceClass::CargoRegistrySource`], [`SourceClass::OfficialDocs`] und
/// [`SourceClass::ReleaseNotes`]. Der Aufrufer kann sie über `sources`
/// überschreiben.
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
/// - [`OpError::InvalidArguments`]: keine Frage oder unbekannte Quellklasse.
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
    summary = "Recherchiert Dependency-Fakten (Versionen, MSRV, Features) über einen read-only Kindagenten.",
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
    run_research(
        ctx,
        args,
        &ResearchProfile {
            role: role_names::RESEARCHER_DEPS,
            slug_prefix: "research-deps",
            actor: ACTOR_RESEARCH_DEPS,
            expected_output: DEPS_EXPECTED_OUTPUT,
            stop_condition: DEPS_STOP_CONDITION,
            default_sources: DEPS_DEFAULT_SOURCES,
        },
    )
    .await
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
/// - [`OpError::InvalidArguments`]: keine Frage oder unbekannte Quellklasse.
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
    // UIA-Chat-Sessions (`organizational_role == AgentRoleId::UserInterface`)
    // dürfen keine `Worker`-Rolle spawnen — `researcher-web` trägt
    // `organizational_role = AgentRoleId::Worker`, den die Spawn-Matrix für
    // UIA-Aufrufer nicht zulässt. Für diese Aufrufer weicht der Kind-Lauf
    // deshalb auf `UIA_EXPLORER` aus (`organizational_role =
    // AgentRoleId::UiaWorker`, eigenes read-only Tool-Profil inklusive
    // `web.fetch`, deckt denselben Bedarf ab). Kann die Rolle der
    // aufrufenden Session nicht ermittelt werden, bleibt das bisherige
    // Verhalten unverändert.
    let role = match ctx
        .managed_spawner()
        .and_then(|spawner| spawner.session_organizational_role(ctx.session_id()))
    {
        Some(AgentRoleId::UserInterface) => role_names::UIA_EXPLORER,
        _ => role_names::RESEARCHER_WEB,
    };
    run_research(
        ctx,
        args,
        &ResearchProfile {
            role,
            slug_prefix: "research-web",
            actor: ACTOR_RESEARCH_WEB,
            expected_output: WEB_EXPECTED_OUTPUT,
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
        RESEARCHER_DEPS_CHILD, RESEARCHER_WEB_CHILD, ResearchArgs, ResearchDepsOperation,
        ResearchWebOperation, parse_source_class, resolve_sources,
    };
    use crate::test_support::{TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, OpContext, OpError, Operation, Surface};
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

    /// Liest den `child_name` der `AgentTool`-Fläche einer Operation.
    fn declared_child(meta: &harw_operations::OperationMeta) -> Option<&'static str> {
        meta.surfaces.iter().find_map(|surface| match surface {
            Surface::AgentTool { child_name, .. } => Some(*child_name),
            _ => None,
        })
    }

    #[test]
    fn test_research_args_from_raw_args_joins_tokens_into_question() -> TestResult {
        let args = ResearchArgs::from_raw_args(&toks(&["welche", "MSRV", "hat", "serde"]))
            .map_err(ctx("ResearchArgs::from_raw_args"))?;
        assert_eq!(args.question.as_deref(), Some("welche MSRV hat serde"));
        assert!(args.crates.is_empty());
        assert!(args.urls.is_empty());
        assert!(args.sources.is_empty());
        assert!(args.task.is_none());
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
    fn test_both_research_roles_are_known_builtin_roles() {
        assert!(role_names::ALL.contains(&RESEARCHER_DEPS_CHILD));
        assert!(role_names::ALL.contains(&RESEARCHER_WEB_CHILD));
    }

    #[test]
    fn test_research_operations_declare_distinct_command_paths() {
        let deps = ResearchDepsOperation.meta();
        let web = ResearchWebOperation.meta();
        assert_eq!(deps.name, "research_deps");
        assert_eq!(web.name, "research_web");
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
