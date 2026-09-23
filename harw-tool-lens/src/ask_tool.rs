//! `lens.ask` — das eine Werkzeug, mit dem ein Agent Lens erreicht.
//!
//! # Warum genau ein Werkzeug, kein `lens.list_indices` daneben
//! Der Auftrag verlangt eine begründete Entscheidung, kein geratenes „ja,
//! mehrere". Die Alternative wäre: `lens.ask(index_name, question)` plus
//! `lens.list_indices()`, damit der Aufrufer vorher sehen kann, welche
//! `index_name`-Werte gültig sind. Diese Alternative würde aber genau die
//! Wahl wieder beim Aufrufer ablegen, die [`crate::scope`] ihm bewusst
//! entzieht: **welchen Index** er befragt, entscheidet implizit auch **welche
//! Sichtbarkeit** er befragt (ein Index ist physisch an genau eine
//! Sichtbarkeit gebunden). Ein `index_name`-Parameter wäre also ein zweiter,
//! informellerer Weg, denselben Entscheid zu treffen, den `scope` formal
//! bereits trifft -- zwei Wahrheiten über dieselbe Grenze.
//!
//! `lens.ask` nimmt deshalb **nur** `question` entgegen (plus ein optionales
//! `limit`). Intern befragt es über
//! [`harw_lens_federation::federated_query`] **alle** Indizes, die
//! [`crate::scope::selectors_in_scope`] für den aktuellen, aus dem
//! [`harw_tools::ToolExecutionContext`] abgeleiteten [`harw_lens::ReadScope`]
//! freigibt, und verschmilzt die Treffer über RRF. Eine Auflistung wäre nur
//! nützlich, wenn der Aufrufer anschließend selektiv einen bestimmten Index
//! ansteuern dürfte -- genau die Umgehung, die diese Fassade verhindern
//! soll. Was ein Agent stattdessen sinnvoll erfährt, ist nicht „welche Indizes
//! gibt es", sondern „welche habe ich tatsächlich befragt, und welche wurden
//! übersprungen" -- das liefert jede `lens.ask`-Antwort bereits mit (siehe
//! unten), ohne einen zweiten Werkzeugaufruf.
//!
//! # Kein schreibendes Werkzeug
//! Dieses Modul (und diese Crate insgesamt) besitzt **keinen** Aufruf von
//! [`harw_lens::build`]. Der Schreibpfad ist `harw-lens-source`, aufgerufen
//! von einer Bau-Pipeline außerhalb der Werkzeugoberfläche eines Agenten,
//! nicht von einem Modell auf Zuruf -- siehe
//! `test_provider_exposes_no_write_capable_tool` in `provider.rs` für den
//! Beleg, dass dieser Provider nur `lens.ask` bewirbt.
//!
//! # Warum `lens_ask` `ask_with_home` über `spawn_blocking` aufruft
//! [`ask_with_home`] ist vollständig synchron und ruft
//! [`crate::provenance::ask_embedder`] auf, das seit der Erweiterung der
//! `harw-lens`-Fassade um [`harw_lens::RemoteEmbedder`]/
//! [`harw_lens::HttpEmbedBackend`] bei konfiguriertem Endpunkt einen echten
//! `reqwest::blocking`-Aufruf tätigen kann (siehe `crate::provenance`s
//! `//!`-Block, Abschnitt „Die Blockier-Falle"). Ein direkter Aufruf aus der
//! `async fn lens_ask` heraus liefe auf dem Tokio-Runtime-Worker-Thread, auf
//! dem `lens_ask` selbst ausgeführt wird, und bräche in diesem Fall mit
//! "Cannot start a runtime from within a runtime" ab. `lens_ask` reicht den
//! Aufruf deshalb über `tokio::task::spawn_blocking` an den dafür
//! vorgesehenen Blocking-Thread-Pool weiter -- unabhängig davon, ob der
//! deterministische Platzhalter oder der entfernte Pfad gewählt wird, damit
//! ein Wechsel der Konfiguration keinen Wechsel dieses Aufrufmusters
//! erzwingt. [`ask_with_home`] selbst bleibt unverändert vollständig
//! synchron und ohne eigene Tokio-Abhängigkeit -- Tests rufen es weiterhin
//! direkt auf, ohne eine Runtime aufzubauen.
//!
//! # Übersprungene Indizes bleiben sichtbar
//! [`harw_lens_federation::FederatedOutcome`] trägt `queried` **und**
//! `skipped` (samt [`harw_lens_federation::SkipReason`]). Ein Werkzeugergebnis,
//! das nur `fused` zurückgäbe, ließe den Aufrufer eine Teilantwort für eine
//! vollständige halten (siehe die Moduldokumentation von `federated_query`).
//! [`outcome_to_json`] reicht deshalb beide Felder unverändert in die
//! JSON-Antwort durch -- kein Zusammenfassen zu einer einzelnen Zahl, kein
//! Weglassen bei leerem `skipped`.
use crate::provenance::{ask_descriptor, ask_embedder, ask_provenance};
use crate::scope::{derive_read_scope, selectors_in_scope};
use harw_lens::{CollapsePolicy, EdgeIndex, Ranked};
use harw_lens_federation::{FederatedOutcome, SkipReason, SkippedIndex, federated_query};
use harw_macros::Tool;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;

/// Voreinstellung für `limit`, wenn der Aufrufer keinen Wert angibt.
pub const DEFAULT_LIMIT_PER_INDEX: usize = 10;

/// Harte Obergrenze für `limit`, unabhängig vom angeforderten Wert.
pub const MAX_LIMIT_PER_INDEX: usize = 50;

/// Die RRF-Konstante, mit der `lens.ask` mehrere Indizes verschmilzt.
///
/// Derselbe Wert, den `harw-lens-federation`s eigene Beispiele verwenden;
/// keine eigene, hier neu erfundene Kalibrierung.
pub const ASK_RRF_K: f32 = 60.0;

/// Deserialisierte Argumente für `lens.ask`.
///
/// # Description
/// Trägt bewusst **weder** `scope`/`visibility`/`read_scope` **noch**
/// `index_name` -- siehe den `//!`-Block dieses Moduls und
/// [`crate::scope`] für die Begründung. `#[serde(deny_unknown_fields)]`
/// (K19) weist jedes zusätzliche Feld ab, einschließlich eines versuchten
/// `scope`-Feldes (siehe
/// `test_ask_args_reject_caller_supplied_scope_field` unten).
#[derive(Debug, Tool, Deserialize)]
#[serde(deny_unknown_fields)]
#[tool(
    name = "lens.ask",
    description = "Stellt eine Frage an alle für diesen Aufrufer sichtbaren Lens-Indizes und liefert verschmolzene Treffer samt übersprungener Indizes."
)]
pub struct LensAskArgs {
    /// Die Frage im Klartext, unpräfixiert.
    pub question: String,
    /// Obergrenze der Treffer je Index vor der Fusion (Default
    /// [`DEFAULT_LIMIT_PER_INDEX`], hart begrenzt auf
    /// [`MAX_LIMIT_PER_INDEX`]).
    #[tool(default = 10)]
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Baut den JSON-Wert eines einzelnen [`Ranked`]-Treffers.
///
/// # Description
/// [`Ranked`] leitet selbst kein `Serialize` ab (siehe `harw-lens-types`s
/// `rank.rs`); dieser Baustein serialisiert deshalb sein `chunk`-Feld
/// (das `Serialize` ableitet) und sein `score`-Feld einzeln.
fn ranked_to_json(ranked: &Ranked) -> serde_json::Value {
    serde_json::json!({
        "chunk": ranked.chunk,
        "score": ranked.score,
    })
}

/// Baut den JSON-Wert eines übersprungenen Selektors samt Begründung.
///
/// # Description
/// [`SkippedIndex`]/[`SkipReason`] leiten selbst kein `Serialize` ab; dieser
/// Baustein übersetzt sie manuell, damit `skipped` in der Werkzeugantwort
/// erscheint (siehe den `//!`-Block dieses Moduls, Abschnitt „Übersprungene
/// Indizes bleiben sichtbar").
fn skipped_to_json(skipped: &SkippedIndex) -> serde_json::Value {
    let reason = match &skipped.reason {
        SkipReason::IncompatibleManifest { field } => serde_json::json!({
            "kind": "incompatible_manifest",
            "field": field,
        }),
    };
    serde_json::json!({
        "index_name": skipped.selector.index_name,
        "visibility": skipped.selector.visibility,
        "reason": reason,
    })
}

/// Baut die vollständige JSON-Antwort aus einem [`FederatedOutcome`].
///
/// # Description
/// Reicht `queried` und `skipped` unverändert durch, zusätzlich zur
/// verschmolzenen Rangliste `fused` -- siehe den `//!`-Block dieses Moduls.
fn outcome_to_json(outcome: &FederatedOutcome) -> serde_json::Value {
    let hits: Vec<serde_json::Value> = outcome.fused.iter().map(ranked_to_json).collect();
    let queried: Vec<serde_json::Value> = outcome
        .queried
        .iter()
        .map(|selector| {
            serde_json::json!({
                "index_name": selector.index_name,
                "visibility": selector.visibility,
            })
        })
        .collect();
    let skipped: Vec<serde_json::Value> = outcome.skipped.iter().map(skipped_to_json).collect();

    serde_json::json!({
        "hits": hits,
        "queried": queried,
        "skipped": skipped,
    })
}

/// Der eigentliche Ablauf von `lens.ask`, unabhängig von der
/// Root-Space-Auflösung.
///
/// # Description
/// Von [`lens_ask`] nach der ambienten Auflösung von `home` aufgerufen
/// (Muster: `harw-tool-deps`s `RegistryAccess::from_env`/`from_cargo_home`
/// -- ambiente Auflösung und Kernlogik bewusst getrennt, damit Tests einen
/// eigenen `home`-Pfad übergeben können, statt den Prozess-Zustand
/// `HARW_HOME` zu mutieren, was seit Rust 1.82 ohnehin ein `unsafe fn`-Aufruf
/// wäre).
///
/// # Errors
/// Liefert nie `Err`; Föderationsfehler werden als `Ok(ToolOutput::error(...))`
/// zurückgegeben.
fn ask_with_home(
    home: &std::path::Path,
    context: &ToolExecutionContext,
    args: &LensAskArgs,
) -> Result<ToolOutput, ToolsError> {
    if args.question.trim().is_empty() {
        return Ok(ToolOutput::error(
            "lens.ask: 'question' darf nicht leer sein",
        ));
    }

    let scope = derive_read_scope(context);
    let selectors = selectors_in_scope(&scope);
    if selectors.is_empty() {
        return Ok(ToolOutput::error(
            "lens.ask: kein Index im Lesebereich dieses Aufrufers sichtbar",
        ));
    }

    let limit = args
        .limit
        .unwrap_or(DEFAULT_LIMIT_PER_INDEX)
        .clamp(1, MAX_LIMIT_PER_INDEX);

    let embedder = ask_embedder();
    let descriptor = ask_descriptor();
    let provenance = ask_provenance();

    let outcome = federated_query(
        home,
        &selectors,
        &scope,
        &args.question,
        embedder.as_ref(),
        &descriptor,
        &provenance,
        &EdgeIndex::default(),
        CollapsePolicy::ByDigest,
        limit,
        ASK_RRF_K,
    );

    match outcome {
        Ok(outcome) => Ok(ToolOutput::json(outcome_to_json(&outcome))),
        Err(err) => Ok(ToolOutput::error(format!("lens.ask: {err}"))),
    }
}

/// Führt `lens.ask` aus: fragt alle sichtbaren Indizes und verschmilzt die
/// Treffer.
///
/// Berechtigungsprüfung (siehe den `//!`-Block von `crate` für den Befund,
/// warum das eine Näherung mit `ReadWorkspace` ist) und JSON-Deserialisierung
/// laufen im von `#[harw_macros::tool]` generierten Prolog von
/// [`LensAskTool`], bevor diese Funktion aufgerufen wird.
///
/// # Errors
/// Liefert nie `Err`; Root-Space- und Föderationsfehler werden als
/// `Ok(ToolOutput::error(...))` zurückgegeben.
#[harw_macros::tool(
    name = "lens.ask",
    description = "Stellt eine Frage an alle für diesen Aufrufer sichtbaren Lens-Indizes und liefert verschmolzene Treffer samt übersprungener Indizes.",
    permission = "read_workspace"
)]
async fn lens_ask(
    context: &ToolExecutionContext,
    args: LensAskArgs,
) -> Result<ToolOutput, ToolsError> {
    let home = match harw_home::paths::home_dir() {
        Ok(home) => home,
        Err(err) => {
            return Ok(ToolOutput::error(format!(
                "lens.ask: Root-Space nicht auflösbar: {err}"
            )));
        }
    };
    // `ask_with_home` ruft `ask_embedder()` auf, das seit
    // `crate::provenance` bei konfiguriertem Endpunkt ein
    // `harw_lens::HttpEmbedBackend` (`reqwest::blocking`) verwendet -- ein
    // direkter Aufruf auf diesem Tokio-Runtime-Worker-Thread bräche mit
    // "Cannot start a runtime from within a runtime" ab (siehe
    // `crate::provenance`s `//!`-Block, Abschnitt „Die Blockier-Falle").
    // `context` wird geklont, weil `ToolExecutionContext: Clone` (siehe
    // `harw_tools::executor`), aber keine `'static`-Referenz durch die
    // Grenze von `spawn_blocking` laufen kann.
    let context = context.clone();
    match tokio::task::spawn_blocking(move || ask_with_home(&home, &context, &args)).await {
        Ok(result) => result,
        Err(join_error) => Ok(ToolOutput::error(format!(
            "lens.ask: interner Ausführungsfehler: {join_error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_lens::{
        DEFAULT_VISIBILITY, DOCS_DESIGN_INDEX, OPERATOR_ONLY_VISIBILITY, collect_design_docs,
    };
    use harw_lens_query::{IndexSelector, QueryError, ReadScope, resolve_index};
    use harw_tools::{ToolCall, ToolExecutor as _, ToolName};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::path::{Path, PathBuf};

    fn scratch_dir(label: &str) -> TestResult<PathBuf> {
        let dir = std::env::temp_dir().join(format!(
            "harw-tool-lens-ask-tests-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(ctx("Scratch-Verzeichnis anlegen"))?;
        Ok(dir)
    }

    fn make_context(
        harness: &Path,
        permissions: Vec<Permission>,
    ) -> TestResult<ToolExecutionContext> {
        let workspace_dir = harness.join("ws");
        std::fs::create_dir_all(&workspace_dir).map_err(ctx("Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            harness,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Registry bauen"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .map_err(ctx("Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));
        Ok(ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            sandbox,
        ))
    }

    fn block_on<F: std::future::Future>(future: F) -> TestResult<F::Output> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(ctx("Tokio-Runtime bauen"))?;
        Ok(runtime.block_on(future))
    }

    /// **Der Beleg, dass Lens jetzt einen Konsumenten hat.** Ein per
    /// `HARW_HOME` gesetzter Root-Space bekommt über `harw_lens::build`
    /// einen echten Index; `lens.ask` (der Werkzeug-Executor, kein direkter
    /// Funktionsaufruf) findet darüber tatsächlich einen Treffer.
    #[test]
    fn test_agent_reaches_lens_hits_through_the_tool() -> TestResult {
        let harness = scratch_dir("reaches-lens")?;
        let home = harness.join("home");
        std::fs::create_dir_all(&home).map_err(ctx("Root-Space anlegen"))?;

        let docs_dir = harness.join("docs");
        std::fs::create_dir_all(&docs_dir).map_err(ctx("Docs-Verzeichnis anlegen"))?;
        std::fs::write(
            docs_dir.join("intro.md"),
            "# Einfuehrung\n\nLens bindet zehn Crates unter einer Fassade.\n",
        )
        .map_err(ctx("Design-Dokument schreiben"))?;
        let documents =
            collect_design_docs(&docs_dir).map_err(ctx("Design-Dokumente einsammeln"))?;

        let embedder = ask_embedder();
        let descriptor = ask_descriptor();
        harw_lens::build(
            &home,
            DOCS_DESIGN_INDEX,
            &documents,
            ASK_EMBEDDING_MODEL_FOR_TEST,
            harw_lens::Locality::Local,
            harw_lens::Metric::Cosine,
            embedder.as_ref(),
            &descriptor,
        )
        .map_err(ctx("Index bauen"))?;

        // `selectors_in_scope` befragt bei DEFAULT_VISIBILITY sowohl
        // `docs.design` als auch `knowledge.palace` (siehe `scope.rs`).
        // `federated_query` bricht die GESAMTE Anfrage ab, sobald
        // `resolve_index` fuer irgendeinen Selektor scheitert -- nicht nur
        // bei einem Sichtbarkeitsverstoss, sondern auch, wenn ein Index
        // schlicht noch nicht gebaut wurde (siehe den Befund in `crate`s
        // `//!`-Block). Dieser Test baut deshalb beide bekannten
        // DEFAULT_VISIBILITY-Indizes, damit der Erfolgspfad ueberhaupt
        // erreichbar ist.
        let palace_documents = vec![harw_lens::RawDocument {
            source: harw_lens::SourceRef::Artifact {
                id: "palace-node-1".to_owned(),
            },
            text: "Lens bindet zehn Crates unter einer Fassade.".to_owned(),
            visibility: DEFAULT_VISIBILITY.to_owned(),
        }];
        harw_lens::build(
            &home,
            harw_lens::KNOWLEDGE_PALACE_INDEX,
            &palace_documents,
            ASK_EMBEDDING_MODEL_FOR_TEST,
            harw_lens::Locality::Local,
            harw_lens::Metric::Cosine,
            embedder.as_ref(),
            &descriptor,
        )
        .map_err(ctx("Palace-Index bauen"))?;

        let context = make_context(&harness, vec![Permission::ReadWorkspace])?;
        let args = LensAskArgs {
            question: "Wie bindet Lens Crates?".to_owned(),
            limit: None,
        };

        let output =
            ask_with_home(&home, &context, &args).map_err(ctx("Tool-Funktion liefert nie Err"))?;

        match output {
            ToolOutput::Json { content } => {
                let hits = content["hits"]
                    .as_array()
                    .ok_or(TestError::Missing("hits ist ein Array"))?;
                assert!(
                    !hits.is_empty(),
                    "erwartet mindestens einen Treffer: {content}"
                );
                assert!(
                    content["queried"]
                        .as_array()
                        .ok_or(TestError::Missing("queried ist ein Array"))?
                        .iter()
                        .any(|s| s["index_name"] == DOCS_DESIGN_INDEX),
                    "docs.design haette befragt werden muessen: {content}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet ToolOutput::Json, war: {other:?}"
                )));
            }
        }

        std::fs::remove_dir_all(&harness).ok();
        Ok(())
    }

    /// Modellname, mit dem der Test-Index gebaut wird -- muss
    /// [`crate::provenance::ASK_EMBEDDING_MODEL`] entsprechen, weil sonst
    /// [`crate::provenance::ask_provenance`] das Manifest als inkompatibel
    /// ablehnte.
    const ASK_EMBEDDING_MODEL_FOR_TEST: &str = crate::provenance::ASK_EMBEDDING_MODEL;

    /// Ein Selektor außerhalb des Lesebereichs erzeugt über
    /// `resolve_index`/`federated_query` einen Fehler, **nie** eine leere
    /// Trefferliste -- die zweite Verteidigungslinie hinter
    /// [`crate::scope::selectors_in_scope`] (siehe dessen Moduldokumentation).
    #[test]
    fn test_federated_query_rejects_selector_outside_scope_even_though_ask_tool_never_sends_one()
    -> TestResult {
        let harness = scratch_dir("outside-scope")?;
        let home = harness.join("home");
        std::fs::create_dir_all(&home).map_err(ctx("Root-Space anlegen"))?;

        let scope = ReadScope::single(DEFAULT_VISIBILITY);
        let outside_selector = IndexSelector::new(DOCS_DESIGN_INDEX, OPERATOR_ONLY_VISIBILITY);

        let Err(err) = resolve_index(&home, &outside_selector, &scope) else {
            return Err(TestError::Unexpected(
                "ein Selektor ausserhalb des Scopes muss fehlschlagen".to_owned(),
            ));
        };
        assert!(matches!(err, QueryError::IndexNotVisible { .. }));

        std::fs::remove_dir_all(&harness).ok();
        Ok(())
    }

    /// Der Aufrufer kann seinen eigenen `ReadScope` nicht wählen: ein
    /// versuchtes `scope`-Feld in `call.arguments` wird von
    /// `#[serde(deny_unknown_fields)]` abgewiesen, bevor es je gelesen wird.
    #[test]
    fn test_ask_args_reject_caller_supplied_scope_field() -> TestResult {
        let harness = scratch_dir("reject-scope-field")?;
        let context = make_context(&harness, vec![Permission::ReadWorkspace])?;
        let tool = LensAskTool;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("lens.ask"),
            arguments: serde_json::json!({
                "question": "irrelevant",
                "scope": ["operator-only"],
            }),
        };

        let result = block_on(tool.execute(&context, &call))?;
        assert!(
            result.is_err(),
            "ein zusaetzliches 'scope'-Feld muss die Deserialisierung scheitern lassen"
        );

        std::fs::remove_dir_all(&harness).ok();
        Ok(())
    }

    /// Ein beliebiges unbekanntes Feld wird ebenso abgewiesen (K19), nicht
    /// nur `scope` speziell.
    #[test]
    fn test_ask_args_reject_any_unknown_field() -> TestResult {
        let harness = scratch_dir("reject-unknown-field")?;
        let context = make_context(&harness, vec![Permission::ReadWorkspace])?;
        let tool = LensAskTool;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("lens.ask"),
            arguments: serde_json::json!({
                "question": "irrelevant",
                "index_name": "docs.design",
            }),
        };

        let result = block_on(tool.execute(&context, &call))?;
        assert!(
            result.is_err(),
            "ein zusaetzliches 'index_name'-Feld muss die Deserialisierung scheitern lassen"
        );

        std::fs::remove_dir_all(&harness).ok();
        Ok(())
    }

    /// Fehlende `ReadWorkspace`-Permission verweigert den Dienst, bevor
    /// irgendein Root-Space beruehrt wird.
    #[test]
    fn test_ask_denied_without_read_workspace_permission() -> TestResult {
        let harness = scratch_dir("denied-no-permission")?;
        let context = make_context(&harness, vec![])?;
        let tool = LensAskTool;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("lens.ask"),
            arguments: serde_json::json!({ "question": "irrelevant" }),
        };

        let output = block_on(tool.execute(&context, &call))?.map_err(ctx("Tool laeuft"))?;
        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("ReadWorkspace"), "war: {message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet ToolOutput::Error, war: {other:?}"
                )));
            }
        }

        std::fs::remove_dir_all(&harness).ok();
        Ok(())
    }

    /// Eine leere Frage wird ohne Root-Space-/Föderationsaufruf abgelehnt.
    #[test]
    fn test_ask_rejects_blank_question() -> TestResult {
        let harness = scratch_dir("blank-question")?;
        let context = make_context(&harness, vec![Permission::ReadWorkspace])?;

        let output = block_on(lens_ask(
            &context,
            LensAskArgs {
                question: "   ".to_owned(),
                limit: None,
            },
        ))?
        .map_err(ctx("Tool-Funktion liefert nie Err"))?;

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("question"), "war: {message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet ToolOutput::Error, war: {other:?}"
                )));
            }
        }

        std::fs::remove_dir_all(&harness).ok();
        Ok(())
    }
}
