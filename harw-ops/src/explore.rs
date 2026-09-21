//! `/explore` — eine gebundene Frage an ein read-only Kind (`explorer`).
//!
//! # Verantwortungsbereich
//! Implementiert die `explore`-Operation gemäß AP W4-02. Die Operation baut aus
//! den Argumenten eine [`ResearchQuestion`], hängt das erwartete Antwortformat
//! ([`finding_schema_prompt`]) an den Turn-Input des Kindes und fährt **genau
//! einen** Kind-Lauf über [`fanout_children`]. Das Ergebnis wird gegen den
//! `ResearchFinding`-Vertrag validiert und — sofern ein Finding-Store, ein
//! Plan-Store und ein Ziel-Knoten (`task`) vorhanden sind — als Artefakt
//! abgelegt und als [`EvidenceRef`](harw_plan::EvidenceRef) an den Plan-Knoten
//! gehängt.
//!
//! Dieses Modul ist außerdem der Wohnort der gemeinsamen Bausteine, die
//! [`crate::research`] und [`crate::analyze`] mitbenutzen: Slug-Bildung,
//! Kind-Nutzlast, Einzel-Kind-Lauf, Finding-Persistenz und Ausgabeform. Sie
//! liegen hier statt in einem eigenen Modul, weil `explore` die kanonische,
//! kleinste Form dieses Ablaufs ist.
//!
//! # Schlüsseltypen
//! - [`ExploreArgs`] — Argument-Container (Command-Fläche: die gesamte
//!   Token-Zeile ist die Frage).
//! - `ExploreOperation` — vom `#[operation]`-Makro erzeugter Op-Struct.
//!
//! # Flächen
//! - Command `/explore` (`channel_parity`)
//! - Model-Tool (readonly, `approval = none`)
//! - Agent-Tool (`child = "explorer"`, `authority = "reduce_to_read_only"`,
//!   `budget = "60k_tokens,40_tool_calls,180s"`)
//!
//! # Warum `FromRawArgs` von Hand geschrieben ist
//! `#[derive(harw_macros::FromRawArgs)]` verlangt im Struct-Pfad, dass **jedes**
//! Feld `Option<String>` ist und ein `#[raw(...)]`-Attribut trägt. [`ExploreArgs`]
//! trägt mit `scope: Vec<String>` bewusst ein Listenfeld, das nur über die
//! JSON-Flächen befüllt wird. Der Vertrag wird deshalb von Hand erfüllt — mit
//! exakt der Semantik, die `#[raw(join)]` am Feld dokumentiert: die gesamte
//! Token-Zeile ist die Frage.
//!
//! # Nebenläufigkeit
//! `ExploreOperation` ist ein zustandsloser Unit-Struct → `Send + Sync`. Der
//! Kind-Lauf selbst läuft über den `ManagedAgentSpawner`, der seine eigene
//! Nebenläufigkeitssicherheit mitbringt.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`]: keine (oder leere) Frage.
//! - [`OpError::NotAvailable`]: kein Agent-Spawner/StateStore im Kontext.
//! - [`OpError::Execution`]: das Kind lieferte kein vertragskonformes Finding,
//!   oder das Artefakt/die Evidenz konnte nicht abgelegt werden.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::explore::ExploreArgs;
//! use harw_operations::FromRawArgs;
//!
//! // "/explore wo wird die Sandbox gebaut" → die ganze Zeile ist die Frage.
//! let args = ExploreArgs::from_raw_args(&[
//!     "wo".to_owned(),
//!     "wird".to_owned(),
//!     "die".to_owned(),
//!     "Sandbox".to_owned(),
//!     "gebaut".to_owned(),
//! ])
//! .expect("Argument-Parsing schlägt hier nicht fehl");
//! assert_eq!(args.question.as_deref(), Some("wo wird die Sandbox gebaut"));
//! ```

use harw_agent_dsl::roles::AgentRoleId;
use harw_core::child_controller::JoinSemantics;
use harw_core_bridge::{ChildReturnContract, OpContextCoreExt, fanout_children, parse_budget_hint};
use harw_macros::operation;
use harw_operations::args::join_all_optional;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
use harw_plan::{PlanAction, TaskId};
use harw_plan_bridge::OpContextPlanExt;
use harw_registry_defaults::profile::role_names;
use harw_research::{
    Freshness, QuestionId, QuestionScope, ResearchFinding, ResearchQuestion, SourceClass,
    finding_schema_prompt,
};
use serde_json::{Value, json};

// ── Gemeinsame Konstanten ────────────────────────────────────────────────────

/// Kennung des Authority-Reducers, der die Parent-Sandbox auf read-only senkt.
///
/// Alle Kind-Flächen dieses APs (`explore`, `research_*`, `analyze`) benutzen
/// denselben Reducer: keine dieser Operationen darf ein Kind erzeugen, das mehr
/// als lesen kann.
pub(crate) const READ_ONLY_REDUCER: &str = "reduce_to_read_only";

/// Budget-Label für einen einzelnen Explorations-/Recherche-Lauf.
///
/// Grammatik siehe [`parse_budget_hint`]: `<n>[k|m]_tokens`, `<n>_tool_calls`,
/// `<n>s|ms`, `effort=<level>`, kommagetrennt. Der Wert ist identisch mit dem
/// `budget`-Schlüssel der `agent_tool`-Deklaration — ein Auseinanderlaufen
/// würde die Deklaration zur Lüge machen.
pub(crate) const SINGLE_CHILD_BUDGET: &str = "60k_tokens,40_tool_calls,180s";

/// Der `child`-Wert der `agent_tool`-Deklaration dieser Operation.
///
/// Das `#[operation]`-Makro akzeptiert an dieser Stelle **nur** String-Literale
/// (`syn::LitStr`), keinen Konstanten-Pfad. Die Konstante existiert deshalb
/// ausschließlich, um in einem Test gegen
/// [`role_names::EXPLORER`](harw_registry_defaults::profile::role_names::EXPLORER)
/// geprüft zu werden — deshalb existiert sie nur im Test-Build.
#[cfg(test)]
pub(crate) const EXPLORER_CHILD: &str = "explorer";

/// Akteur-Kennung für Plan-Mutationen aus dieser Operation.
const ACTOR_EXPLORE: &str = "op:explore";

/// Maximale Zeichenzahl des aus einer Frage abgeleiteten Slugs.
const SLUG_MAX_CHARS: usize = 48;

/// Standard-Beschreibung des erwarteten Ausgabeformats.
const DEFAULT_EXPECTED_OUTPUT: &str = "Ein ResearchFinding: Schlussfolgerung in Prosa, jede \
     Behauptung mit mindestens einem Beleg (Datei + Zeilenbereich oder Symbolname), offene Fragen \
     ausdrücklich benannt.";

/// Standard-Stop-Bedingung einer gebundenen Exploration.
const DEFAULT_STOP_CONDITION: &str = "Die Frage ist mit Belegen aus dem erlaubten Scope \
     beantwortet, oder der Scope enthält die Antwort nachweislich nicht — dann wird das als \
     offene Frage zurückgemeldet.";

// ── Argumente ────────────────────────────────────────────────────────────────

/// Eingabe-Argumente der `explore`-Operation.
///
/// # Beschreibung
/// Auf der Command-Fläche ist die gesamte Token-Zeile die Frage
/// (`#[raw(join)]`-Semantik, siehe [`FromRawArgs`]-Impl weiter unten). Die
/// übrigen Felder sind ausschließlich über die JSON-Flächen (Model-Tool,
/// Agent-Tool) erreichbar.
///
/// # Felder
/// - `question` (`Option<String>`): die gebundene Frage.
/// - `scope` (`Vec<String>`): Pfade oder Globs, auf die die Exploration
///   begrenzt ist. Leer = kein Pfadfilter.
/// - `expected_output` (`Option<String>`): erwartete Form der Antwort.
/// - `task` (`Option<String>`): zugehöriger Plan-Knoten.
///
/// # Spec-Referenz
/// AP W4-02 — `/explore`.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct ExploreArgs {
    /// Die gebundene Frage, die das Kind beantworten soll.
    #[serde(default)]
    #[raw(join)]
    pub question: Option<String>,
    /// Pfade oder Globs, auf die die Exploration begrenzt ist.
    #[serde(default)]
    #[tool(default = [])]
    pub scope: Vec<String>,
    /// Erwartete Form der Antwort (Freitext für das Kind).
    #[serde(default)]
    pub expected_output: Option<String>,
    /// Zugehöriger Plan-Knoten, an den der Nachweis gehängt wird.
    #[serde(default)]
    pub task: Option<String>,
}

impl FromRawArgs for ExploreArgs {
    /// Nimmt die gesamte Token-Zeile als Frage (`#[raw(join)]`-Semantik).
    ///
    /// # Argumente
    /// - `tokens` (`&[String]`): die rohen Command-Argumente.
    ///
    /// # Rückgabe
    /// `Ok(ExploreArgs)` — `scope`, `expected_output` und `task` bleiben auf
    /// ihren Defaults; sie sind nur über die JSON-Flächen setzbar.
    ///
    /// # Fehler
    /// Keine — diese Implementierung schlägt nie fehl. Die leere Frage wird
    /// erst im Op-Rumpf abgewiesen, damit die Fehlermeldung den Aufrufkontext
    /// nennen kann.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            question: join_all_optional(tokens),
            scope: Vec::new(),
            expected_output: None,
            task: None,
        })
    }
}

// ── Gemeinsame Bausteine ─────────────────────────────────────────────────────

/// Bildet aus einer Frage einen stabilen, dateisystemtauglichen Bezeichner.
///
/// # Beschreibung
/// ASCII-Buchstaben und -Ziffern werden kleingeschrieben übernommen, jede
/// andere Zeichenfolge wird zu genau einem `-`. Nicht-ASCII-Buchstaben (`ä`,
/// `ü`, `ß`) fallen damit ebenfalls auf `-` — der Slug ist eine Kennung, keine
/// Wiedergabe der Frage. Der Rumpf wird auf [`SLUG_MAX_CHARS`] gekürzt; eine
/// Frage ohne jedes ASCII-Alphanumerikum ergibt `<prefix>-frage`.
///
/// # Argumente
/// - `prefix` (`&str`): Namensraum des Bezeichners, z. B. `"explore"`.
/// - `question` (`&str`): die Frage, aus der abgeleitet wird.
///
/// # Rückgabe
/// `"<prefix>-<slug>"`.
///
/// # Nebenläufigkeit
/// Reine Funktion ohne geteilten Zustand.
///
/// # Beispiel
/// ```rust
/// # // interner Helfer — hier nur als Verhaltensbeschreibung.
/// // question_slug("explore", "Wo wird die Sandbox gebaut?") == "explore-wo-wird-die-sandbox-gebaut"
/// ```
pub(crate) fn question_slug(prefix: &str, question: &str) -> String {
    let mut body = String::with_capacity(SLUG_MAX_CHARS);
    let mut pending_dash = false;
    for ch in question.chars() {
        if !ch.is_ascii_alphanumeric() {
            pending_dash = true;
            continue;
        }
        // Ein ausstehender Trenner zählt zur Länge: sonst könnte der Rumpf die
        // Obergrenze um genau ein Zeichen überschreiten.
        let separator = usize::from(pending_dash && !body.is_empty());
        if body.len() + separator + 1 > SLUG_MAX_CHARS {
            break;
        }
        if separator == 1 {
            body.push('-');
        }
        pending_dash = false;
        body.push(ch.to_ascii_lowercase());
    }
    if body.is_empty() {
        body.push_str("frage");
    }
    format!("{prefix}-{body}")
}

/// Baut die JSON-Nutzlast, mit der ein Recherche-Kind gestartet wird.
///
/// # Beschreibung
/// Die Nutzlast trägt zwei Felder: die serialisierte [`ResearchQuestion`] und
/// das Antwortformat aus [`finding_schema_prompt`]. `fanout_children` reicht
/// diesen Wert sowohl als `SpawnInput::context` als auch — als kompakter
/// JSON-String — als Turn-Input an das Kind weiter; das Kind sieht das Schema
/// damit ohne zusätzlichen Kanal.
///
/// # Argumente
/// - `question` (`&ResearchQuestion`): die gebundene Frage.
///
/// # Rückgabe
/// Ein JSON-Objekt `{ "question": …, "response_format": … }`.
///
/// # Fehler
/// - [`OpError::Execution`]: die Frage ist nicht serialisierbar (praktisch
///   unerreichbar; wird nicht stillschweigend verschluckt).
///
/// # Nebenläufigkeit
/// Reine Funktion.
pub(crate) fn child_payload(question: &ResearchQuestion) -> Result<Value, OpError> {
    let encoded = serde_json::to_value(question).map_err(|error| {
        OpError::Execution(format!("Recherche-Frage nicht serialisierbar: {error}"))
    })?;
    Ok(json!({
        "question": encoded,
        "response_format": finding_schema_prompt(),
    }))
}

/// Fährt genau einen Kind-Lauf und liefert das validierte Finding.
///
/// # Beschreibung
/// Benutzt [`fanout_children`] mit einer einelementigen Fragenliste und
/// `max_parallel = 1` — derselbe Pfad wie beim Fan-out, nur mit Kardinalität
/// eins. Der Return-Contract ist [`ChildReturnContract::ResearchFinding`]; ein
/// Kind, das ihn bricht, erzeugt hier einen `Err`-Eintrag statt eines
/// halbgaren Findings.
///
/// # Argumente
/// - `ctx` (`&OpContext`): liefert Spawner, StateStore und Parent-Sandbox.
/// - `role` (`&str`): Rollenname des Kindes (aus
///   [`role_names`](harw_registry_defaults::profile::role_names)).
/// - `payload` (`Value`): die Nutzlast aus [`child_payload`].
///
/// # Rückgabe
/// Das validierte [`ResearchFinding`] des Kindes.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein Agent-Spawner/StateStore registriert.
/// - [`OpError::InvalidArguments`]: [`SINGLE_CHILD_BUDGET`] ist syntaktisch
///   ungültig (Programmierfehler in dieser Datei, nie eine Nutzereingabe).
/// - [`OpError::Execution`]: der Kind-Lauf schlug fehl oder das Ergebnis ist
///   kein deserialisierbares [`ResearchFinding`].
///
/// # Nebenläufigkeit
/// `async`; hält keinen Lock über ein `await`.
pub(crate) async fn run_single_child(
    ctx: &OpContext,
    role: &str,
    payload: Value,
) -> Result<ResearchFinding, OpError> {
    let budget = parse_budget_hint(SINGLE_CHILD_BUDGET)?;
    let mut results = fanout_children(
        ctx,
        role,
        std::slice::from_ref(&payload),
        READ_ONLY_REDUCER,
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

/// Deserialisiert ein vom Vertrag bereits validiertes Kind-Ergebnis.
///
/// # Argumente
/// - `value` (`Value`): das JSON, das [`ChildReturnContract::ResearchFinding`]
///   durchgelassen hat.
///
/// # Rückgabe
/// Das typisierte [`ResearchFinding`].
///
/// # Fehler
/// - [`OpError::Execution`]: das JSON passt nicht auf [`ResearchFinding`].
pub(crate) fn finding_from_value(value: Value) -> Result<ResearchFinding, OpError> {
    serde_json::from_value::<ResearchFinding>(value).map_err(|error| {
        OpError::Execution(format!(
            "Kind-Ergebnis ist kein gültiges ResearchFinding: {error}"
        ))
    })
}

/// Legt ein Finding ab und hängt den Nachweis an einen Plan-Knoten.
///
/// # Beschreibung
/// Persistiert nur, wenn **alle drei** Voraussetzungen erfüllt sind: ein
/// `task`, ein registrierter [`FindingStore`](harw_plan_bridge::FindingStore)
/// und ein Plan-Store mit einem angelegten Plan. Fehlt eine davon, ist das
/// keine Störung, sondern der reguläre „nur zurückgeben"-Fall: die Operation
/// liefert das Finding trotzdem.
///
/// # Argumente
/// - `ctx` (`&OpContext`): liefert Finding-Store und Plan-Store.
/// - `task` (`Option<&str>`): der Plan-Knoten, an den der Nachweis gehört.
/// - `finding` (`&ResearchFinding`): das abzulegende Ergebnis.
/// - `actor` (`&str`): Akteur der Plan-Mutation (runtime-gesetzt).
///
/// # Rückgabe
/// `Some(Pfad)` des geschriebenen Artefakts, oder `None`, wenn nicht
/// persistiert wurde.
///
/// # Fehler
/// - [`OpError::Execution`]: das Artefakt konnte nicht geschrieben oder die
///   Evidenz nicht angehängt werden.
///
/// # Nebenläufigkeit
/// Synchron; der Schreibvorgang des Finding-Stores ist atomar (`rename`).
pub(crate) fn persist_finding(
    ctx: &OpContext,
    task: Option<&str>,
    finding: &ResearchFinding,
    actor: &str,
) -> Result<Option<String>, OpError> {
    let (Some(task), Some(store), Some(plan)) = (task, ctx.finding_store(), ctx.plan_store())
    else {
        return Ok(None);
    };
    let Ok(snapshot) = plan.current() else {
        return Ok(None);
    };
    let plan_id = snapshot.id.as_str();

    let path = store.write(plan_id, finding).map_err(|error| {
        OpError::Execution(format!(
            "Finding-Artefakt konnte nicht abgelegt werden: {error}"
        ))
    })?;
    let evidence = store.evidence_for(plan_id, finding, actor);
    plan.apply(
        PlanAction::AttachEvidence {
            id: TaskId::new(task),
            evidence,
        },
        actor,
    )
    .map_err(|error| {
        OpError::Execution(format!(
            "Evidenz konnte nicht an Knoten '{task}' gehängt werden: {error}"
        ))
    })?;

    Ok(Some(path.display().to_string()))
}

/// Serialisiert ein Finding als Operationsausgabe.
///
/// # Beschreibung
/// Das Feld `artifact` nennt den Ablageort des Artefakts oder ist `null`, wenn
/// nicht persistiert wurde — der Aufrufer soll nicht raten müssen, ob sein
/// Ergebnis den Turn überlebt.
///
/// # Argumente
/// - `finding` (`&ResearchFinding`): das auszugebende Ergebnis.
/// - `locator` (`Option<&str>`): Pfad des Artefakts, falls abgelegt.
///
/// # Rückgabe
/// [`OpOutput`] mit eingerücktem JSON.
///
/// # Fehler
/// - [`OpError::Execution`]: das Finding ist nicht serialisierbar.
pub(crate) fn finding_output(
    finding: &ResearchFinding,
    locator: Option<&str>,
) -> Result<OpOutput, OpError> {
    let mut payload = serde_json::to_value(finding)
        .map_err(|error| OpError::Execution(format!("Finding nicht serialisierbar: {error}")))?;
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "artifact".to_owned(),
            locator.map_or(Value::Null, |path| Value::String(path.to_owned())),
        );
    }
    let text = serde_json::to_string_pretty(&payload)
        .map_err(|error| OpError::Execution(format!("Ausgabe nicht serialisierbar: {error}")))?;
    Ok(OpOutput::from(text))
}

// ── Operation ────────────────────────────────────────────────────────────────

/// Startet eine gebundene, read-only Exploration und liefert das Finding.
///
/// # Beschreibung
/// Ablauf in vier Schritten:
/// 1. Die Frage wird geprüft — eine leere Frage ist keine Frage.
/// 2. Aus den Argumenten entsteht eine [`ResearchQuestion`]; die ID ist
///    `task`, falls angegeben, sonst ein aus der Frage abgeleiteter Slug.
/// 3. Ein einzelner `explorer`-Lauf beantwortet sie; das Antwortformat hängt
///    als `response_format` an der Nutzlast.
/// 4. Liegen `task`, Finding-Store und Plan vor, wird das Finding abgelegt und
///    der Nachweis an den Knoten gehängt; sonst wird es nur zurückgegeben.
///
/// Der Scope wird als `paths` in die [`QuestionScope`] übernommen; die
/// Quellklasse ist [`SourceClass::LocalSource`] — `/explore` ist ausdrücklich
/// die *lokale* Erkundung, Web- und Dependency-Recherche haben eigene Ops
/// (siehe [`crate::research`]).
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext mit Spawner, Plan- und
///   Finding-Store.
/// - `args` ([`ExploreArgs`]): die Argumente; werden konsumiert.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit dem validierten Finding als eingerücktem JSON, ergänzt um
/// das Feld `artifact`.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: keine oder leere Frage.
/// - [`OpError::NotAvailable`]: kein Agent-Spawner/StateStore im Kontext.
/// - [`OpError::Execution`]: Kind-Lauf, Vertrag oder Persistenz schlugen fehl.
///
/// # Nebenläufigkeit
/// Zustandslos; der Kind-Lauf serialisiert sich über den `ManagedAgentSpawner`.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run() — direkte Verwendung nur im Test.
/// ```
#[operation(
    name = "explore",
    summary = "Beantwortet eine gebundene Frage durch einen read-only Explorer-Kindagenten.",
    domain = "agents",
    permission = "operator",
    command(path = "/explore", visibility = "channel_parity"),
    model_tool(readonly, approval = "none"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: der Kindagent
    // läuft mit `reduce_to_read_only`-Autorität, die Operation selbst
    // schreibt nichts.
    web(path = "/api/explore", method = "get", approval = "none"),
    agent_tool(
        child = "explorer",
        authority = "reduce_to_read_only",
        budget = "60k_tokens,40_tool_calls,180s"
    )
)]
async fn explore(ctx: &OpContext, args: ExploreArgs) -> Result<OpOutput, OpError> {
    let ExploreArgs {
        question,
        scope,
        expected_output,
        task,
    } = args;

    let question = question
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            OpError::InvalidArguments("eine Frage ist erforderlich: /explore <frage>".to_owned())
        })?
        .to_owned();

    let id = match task.as_deref() {
        Some(node) => node.to_owned(),
        None => question_slug("explore", &question),
    };

    let research_question = ResearchQuestion {
        id: QuestionId::new(id),
        question,
        scope: QuestionScope {
            paths: scope,
            crates: Vec::new(),
            urls: Vec::new(),
            sources: vec![SourceClass::LocalSource],
        },
        expected_output: expected_output.unwrap_or_else(|| DEFAULT_EXPECTED_OUTPUT.to_owned()),
        freshness: Freshness::AnyTime,
        stop_condition: DEFAULT_STOP_CONDITION.to_owned(),
        owner_task: task.clone(),
    };

    let payload = child_payload(&research_question)?;
    // UIA-Chat-Sessions (`organizational_role == AgentRoleId::UserInterface`)
    // dürfen keine `Worker`-Rolle spawnen — die Spawn-Matrix lässt für sie
    // nur `AgentRoleId::RootOrchestrator`/`AgentRoleId::UiaWorker` zu, und
    // `explorer` trägt `organizational_role = AgentRoleId::Worker`. Für
    // diese Aufrufer weicht der Kind-Lauf deshalb auf `UIA_EXPLORER` aus
    // (`organizational_role = AgentRoleId::UiaWorker`, deckt denselben
    // read-only Bedarf ab). Kann die Rolle der aufrufenden Session nicht
    // ermittelt werden, bleibt das bisherige Verhalten unverändert.
    let role = match ctx
        .managed_spawner()
        .and_then(|spawner| spawner.session_organizational_role(ctx.session_id()))
    {
        Some(AgentRoleId::UserInterface) => role_names::UIA_EXPLORER,
        _ => role_names::EXPLORER,
    };
    let finding = run_single_child(ctx, role, payload).await?;
    let locator = persist_finding(ctx, task.as_deref(), &finding, ACTOR_EXPLORE)?;
    finding_output(&finding, locator.as_deref())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{EXPLORER_CHILD, ExploreArgs, ExploreOperation, question_slug};
    use crate::testutil::toks;
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, OpContext, OpError, Operation, Surface};
    use harw_registry_defaults::profile::role_names;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Baut einen minimalen [`OpContext`] mit leerer [`ServiceMap`].
    ///
    /// Der zweite Rückgabewert ist das Wurzelverzeichnis, das der Test wieder
    /// entfernen muss.
    fn test_context() -> (OpContext, std::path::PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-explore-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).expect("Test-Workspace anlegen");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .expect("Workspace-Registry bauen");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("Workspace-Binding auflösen");
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        )
    }

    #[test]
    fn test_explore_args_from_raw_args_joins_all_tokens_into_question() {
        match ExploreArgs::from_raw_args(&toks(&["wo", "liegt", "der", "Spawner"])) {
            Ok(args) => {
                assert_eq!(args.question.as_deref(), Some("wo liegt der Spawner"));
                assert!(args.scope.is_empty());
                assert!(args.expected_output.is_none());
                assert!(args.task.is_none());
            }
            Err(error) => panic!("unerwarteter Fehler: {error}"),
        }
    }

    #[test]
    fn test_explore_args_from_raw_args_empty_tokens_yields_no_question() {
        match ExploreArgs::from_raw_args(&toks(&[])) {
            Ok(args) => assert!(args.question.is_none()),
            Err(error) => panic!("unerwarteter Fehler: {error}"),
        }
    }

    #[test]
    fn test_explore_args_defaults_are_empty() {
        let args = ExploreArgs::default();
        assert!(args.question.is_none());
        assert!(args.scope.is_empty());
        assert!(args.expected_output.is_none());
        assert!(args.task.is_none());
    }

    #[test]
    fn test_question_slug_lowercases_and_collapses_separators() {
        assert_eq!(
            question_slug("explore", "Wo wird die Sandbox gebaut?"),
            "explore-wo-wird-die-sandbox-gebaut"
        );
    }

    #[test]
    fn test_question_slug_without_ascii_alphanumerics_falls_back() {
        assert_eq!(question_slug("explore", "??? !!!"), "explore-frage");
    }

    #[test]
    fn test_question_slug_is_bounded_in_length() {
        let long = "a".repeat(500);
        let slug = question_slug("explore", &long);
        assert!(
            slug.len() <= "explore-".len() + super::SLUG_MAX_CHARS,
            "Slug wurde nicht gekürzt: {slug}"
        );
    }

    #[test]
    fn test_explore_agent_tool_child_matches_role_names_constant() {
        assert_eq!(EXPLORER_CHILD, role_names::EXPLORER);

        let meta = ExploreOperation.meta();
        let declared = meta.surfaces.iter().find_map(|surface| match surface {
            Surface::AgentTool { child_name, .. } => Some(*child_name),
            _ => None,
        });
        assert_eq!(
            declared,
            Some(role_names::EXPLORER),
            "die agent_tool-Deklaration muss auf role_names::EXPLORER zeigen"
        );
    }

    #[test]
    fn test_explore_declares_command_model_tool_and_agent_tool() {
        let meta = ExploreOperation.meta();
        assert_eq!(meta.name, "explore");
        assert!(meta.surfaces.iter().any(
            |surface| matches!(surface, Surface::Command { path, .. } if *path == "/explore")
        ));
        assert!(
            meta.surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { readonly, .. } if *readonly))
        );
        assert!(
            meta.surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::AgentTool { .. }))
        );
    }

    #[test]
    fn test_explore_budget_hint_is_parsable_and_matches_declaration() {
        let budget = match harw_core_bridge::parse_budget_hint(super::SINGLE_CHILD_BUDGET) {
            Ok(budget) => budget,
            Err(error) => panic!("Budget-Label ist ungültig: {error}"),
        };
        assert_eq!(budget.max_tokens, Some(60_000));
        assert_eq!(budget.max_tool_calls, Some(40));
        assert_eq!(budget.max_wall_time_ms, Some(180_000));

        let meta = ExploreOperation.meta();
        let declared = meta.surfaces.iter().find_map(|surface| match surface {
            Surface::AgentTool { budget_hint, .. } => Some(*budget_hint),
            _ => None,
        });
        assert_eq!(declared, Some(super::SINGLE_CHILD_BUDGET));
    }

    #[tokio::test]
    async fn test_explore_without_spawner_is_not_available() {
        let (ctx, root) = test_context();
        let result = super::explore(
            &ctx,
            ExploreArgs {
                question: Some("wo liegt der Spawner".to_owned()),
                ..ExploreArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "ohne Agent-Spawner muss /explore fail-closed sein, war: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_explore_without_question_is_invalid_arguments() {
        let (ctx, root) = test_context();
        let result = super::explore(&ctx, ExploreArgs::default()).await;
        std::fs::remove_dir_all(root).ok();

        assert!(
            matches!(result, Err(OpError::InvalidArguments(_))),
            "eine leere Frage muss abgewiesen werden, war: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_explore_with_blank_question_is_invalid_arguments() {
        let (ctx, root) = test_context();
        let result = super::explore(
            &ctx,
            ExploreArgs {
                question: Some("   ".to_owned()),
                ..ExploreArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
    }
}
