//! `WorkbenchToolProvider` — die Modell-Werkzeuge der Workbench
//! (`docs/design/knowledge-surfaces.md` §5): `workbench.note` und
//! `workbench.hypothesis`.
//!
//! # Verantwortung
//! Das Modell darf seine eigene Arbeitsfläche beschreiben — Notizen und
//! Hypothesen, die über Turns hinweg sichtbar bleiben, ohne das
//! Themengedächtnis zu verschmutzen (§5.1). Beide Werkzeuge schreiben
//! ausschließlich in den **Sitzungs-Scope der aufrufenden Sitzung**
//! (`workbench/session:<session-id>/` unter
//! `harw_home::knowledge_dir(profile)`), abgeleitet aus
//! [`ToolExecutionContext::session_id`] — nie aus einem Argument. Es gibt
//! bewusst **kein** `workbench.pin`: Anheften löst Pfade auf und bleibt dem
//! Operator-Kommando `/workbench pin` vorbehalten.
//!
//! # Rechteklasse: „read-only-safe"
//! Die Werkzeuge verändern weder Workspace noch Host, starten nichts und
//! gehen nicht ins Netz; ihr einziger Effekt ist ein atomarer Schreibzugriff
//! in den harness-eigenen, sitzungsgebundenen Scratch-Bereich (flüchtig per
//! Design, §5.2 `session-lifetime`). Darum tragen sie
//! [`Permission::ReadWorkspace`] ([`WorkbenchToolProvider::TOOL_PERMISSIONS`]):
//! jede lesende Rolle darf sie nutzen, ohne dass ein Schreibrecht auf den
//! Workspace nötig wäre. Größenobergrenzen (`harw_knowledge::workbench::
//! MAX_NOTE_BYTES`/`MAX_LINE_BYTES`) begrenzen den Effekt.
//!
//! # Lesende Fläche (Runde 4, D1)
//! Getrennt davon, in eigenen Typen:
//! - [`WorkbenchReadToolProvider`] mit `workbench.show` — **rein lesend**,
//!   liefert Pins (nur Pfad + Notiz, nie Dateiinhalte), Hypothesen und das
//!   Notiz-Ende der aufrufenden Sitzung bzw. — mit `scope: "project"` — des
//!   bei der Montage festgelegten Projekts; gedeckelt auf
//!   [`WORKBENCH_SHOW_MAX_BYTES`]. Kein Argument wählt eine fremde Sitzung
//!   oder ein fremdes Projekt (SelfOnly-Semantik).
//! - [`WorkbenchContextProvider`] — Kontextbeitrag „Werkbank" (Pins + offene
//!   Hypothesen der Sitzung, gedeckelt auf [`WORKBENCH_CONTEXT_MAX_BYTES`]).
//!
//! # Nebenläufigkeit
//! `Send + Sync`; hält nur ein `Arc<KnowledgeStore>`. Die Read-Modify-Write-
//! Zyklen serialisiert `harw_knowledge::workbench` prozessweit.
//!
//! # Fehler
//! Kein Aufruf gibt `Err` zurück: ungültige Argumente und Speicherfehler
//! werden als [`ToolOutput::error`] gemeldet.

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_extension_api::contributors::{ContextProvider, ExtFuture, ToolProvider};
use harw_extension_api::types::{ContextFragment, TurnInputContext};
use harw_extension_api::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};
use harw_knowledge::workbench::{self, DigestOptions, WorkbenchScope};
use harw_knowledge::{AgentId, KnowledgeStore};
use harw_tools::args::parse_args;
use harw_tools::schema_helpers::{object_schema_all_required, string_property};
use harw_tools::{FunctionToolSpec, Permission};
use serde::Deserialize;

/// Name des Notiz-Werkzeugs.
pub const WORKBENCH_NOTE: &str = "workbench.note";

/// Name des Hypothesen-Werkzeugs.
pub const WORKBENCH_HYPOTHESIS: &str = "workbench.hypothesis";

/// Name des lesenden Werkzeugs.
pub const WORKBENCH_SHOW: &str = "workbench.show";

/// Obergrenze der Ausgabe von `workbench.show` in Bytes.
pub const WORKBENCH_SHOW_MAX_BYTES: usize = 4 * 1024;

/// Obergrenze des Kontextbeitrags „Werkbank" in Bytes.
pub const WORKBENCH_CONTEXT_MAX_BYTES: usize = 1536;

/// Namensraum und Fragment-Label des Kontextbeitrags.
pub const WORKBENCH_CONTEXT_NAMESPACE: &str = "workbench";

/// Notizzeilen, die `workbench.show` aus dem Tail zeigt.
const SHOW_NOTES_LINES: usize = 12;

/// Meldet einen erfolgreichen Schreibvorgang der Modell-Werkzeuge
/// (Runde 5, Teil C): `(session_id, scope)`, wobei `scope` die
/// Pfadkomponente des Workbench-Scopes ist (`session:<id>`).
///
/// # Beschreibung
/// Die Montage hängt hier `AgentEventHub::publish_knowledge(session,
/// "workbench", Some(scope))` ein, damit eine offene `/workbench`-Ansicht
/// sofort neu lädt. Als Rückruf statt als `AgentEventHub`, weil
/// `harw-registry-defaults` nicht von `harw-core` abhängt.
pub type WorkbenchChangeNotifier = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// Die Workbench-Modell-Werkzeuge über einem geteilten Wissensspeicher.
#[derive(Clone)]
pub struct WorkbenchToolProvider {
    store: Arc<KnowledgeStore>,
    notifier: Option<WorkbenchChangeNotifier>,
}

impl std::fmt::Debug for WorkbenchToolProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkbenchToolProvider")
            .field("store", &self.store.root())
            .field("notifier", &self.notifier.is_some())
            .finish()
    }
}

impl WorkbenchToolProvider {
    /// Die Werkzeugnamen in Provider-Reihenfolge.
    pub const TOOL_NAMES: &'static [&'static str] = &[WORKBENCH_NOTE, WORKBENCH_HYPOTHESIS];

    /// Die Rechteklasse je Werkzeug, parallel zu [`Self::TOOL_NAMES`]
    /// (siehe Moduldoku „read-only-safe").
    pub const TOOL_PERMISSIONS: &'static [Option<Permission>] = &[
        Some(Permission::ReadWorkspace),
        Some(Permission::ReadWorkspace),
    ];

    /// Baut den Provider über dem Speicher der Montage.
    ///
    /// # Argumente
    /// - `store` (`Arc<KnowledgeStore>`): derselbe Speicher, den
    ///   `RuntimeServices::knowledge_store` auf Slash/Modell-Werkzeug legt.
    #[must_use]
    pub fn new(store: Arc<KnowledgeStore>) -> Self {
        Self {
            store,
            notifier: None,
        }
    }

    /// Hängt den Live-Update-Rückruf an (Runde 5, Teil C): nach jedem
    /// erfolgreichen `workbench.note`/`workbench.hypothesis` wird er mit
    /// Sitzungs-Id und Scope aufgerufen; fehlgeschlagene Aufrufe melden
    /// nichts.
    #[must_use]
    pub fn with_change_notifier(mut self, notifier: WorkbenchChangeNotifier) -> Self {
        self.notifier = Some(notifier);
        self
    }
}

impl ToolProvider for WorkbenchToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![note_spec(), hypothesis_spec()]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        let tool = match name.as_str() {
            WORKBENCH_NOTE => WorkbenchTool::Note,
            WORKBENCH_HYPOTHESIS => WorkbenchTool::Hypothesis,
            _ => return None,
        };
        Some(Arc::new(WorkbenchExecutor {
            store: Arc::clone(&self.store),
            tool,
            notifier: self.notifier.clone(),
        }))
    }
}

fn note_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "text".to_owned(),
        string_property("Notiztext (Markdown, höchstens 16 KiB)."),
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(WORKBENCH_NOTE),
        description: "Hängt eine Notiz an die Workbench DIESER Sitzung an (NOTES.md): \
             Zwischenstände, Beobachtungen, nächste Schritte. Flüchtig per Design, \
             wird nicht ins Gedächtnis übernommen. Ändert keine Workspace-Datei."
            .to_owned(),
        parameters: object_schema_all_required(props),
        strict: true,
    })
}

fn hypothesis_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    let mut action = string_property(
        "\"add\" legt eine offene Hypothese an; \"confirm\"/\"reject\" entscheidet eine offene.",
    );
    action.enum_values = Some(vec![
        serde_json::json!("add"),
        serde_json::json!("confirm"),
        serde_json::json!("reject"),
    ]);
    props.insert("action".to_owned(), action);
    props.insert(
        "text".to_owned(),
        string_property(
            "Bei add: der einzeilige Hypothesentext. Bei confirm/reject: exakt der \
             Hypothesentext oder ihre Nummer als \"#<n>\".",
        ),
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(WORKBENCH_HYPOTHESIS),
        description: "Pflegt die Hypothesenliste der Workbench DIESER Sitzung \
             (hypotheses.md): offene Vermutungen festhalten und später bestätigen oder \
             verwerfen. Eine entschiedene Hypothese kann nicht erneut entschieden werden."
            .to_owned(),
        parameters: object_schema_all_required(props),
        strict: true,
    })
}

/// Welches der beiden Werkzeuge ein Executor bedient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkbenchTool {
    Note,
    Hypothesis,
}

struct WorkbenchExecutor {
    store: Arc<KnowledgeStore>,
    tool: WorkbenchTool,
    notifier: Option<WorkbenchChangeNotifier>,
}

impl ToolExecutor for WorkbenchExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let session_id = context.session_id().as_str().to_owned();
        let arguments = call.arguments.clone();
        Box::pin(async move {
            let now = jiff::Timestamp::now();
            let output = match self.tool {
                WorkbenchTool::Note => execute_note(&self.store, &session_id, arguments, now),
                WorkbenchTool::Hypothesis => {
                    execute_hypothesis(&self.store, &session_id, arguments, now)
                }
            };
            // Runde 5, Teil C: Live-Update nur nach erfolgreichem Schreiben.
            notify_change(self.notifier.as_ref(), &session_id, &output);
            Ok(output)
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteArgs {
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HypothesisArgs {
    action: String,
    text: String,
}

/// Ruft `notifier` nach einem erfolgreichen Werkzeugaufruf mit dem
/// Sitzungs-Scope auf (Runde 5, Teil C); Fehler melden nichts.
fn notify_change(
    notifier: Option<&WorkbenchChangeNotifier>,
    session_id: &str,
    output: &ToolOutput,
) {
    let Some(notifier) = notifier else {
        return;
    };
    if matches!(output, ToolOutput::Error { .. }) {
        return;
    }
    let scope = WorkbenchScope::Session(session_id.to_owned());
    notifier(session_id, &scope.path_component());
}

/// Der Autor, unter dem das Modell in seine Sitzungs-Workbench schreibt.
fn model_author(session_id: &str) -> AgentId {
    AgentId::new(format!("model@{session_id}"))
}

/// Kern von `workbench.note` (testbar ohne Sandbox-Kontext).
fn execute_note(
    store: &KnowledgeStore,
    session_id: &str,
    arguments: serde_json::Value,
    now: jiff::Timestamp,
) -> ToolOutput {
    let args: NoteArgs = match parse_args(WORKBENCH_NOTE, &arguments) {
        Ok(args) => args,
        Err(out) => return out,
    };
    let scope = WorkbenchScope::Session(session_id.to_owned());
    match workbench::append_note(store, &scope, &model_author(session_id), &args.text, now) {
        Ok(()) => ToolOutput::json(serde_json::json!({
            "ok": true,
            "scope": scope.path_component(),
        })),
        Err(error) => ToolOutput::error(format!("{WORKBENCH_NOTE}: {error}")),
    }
}

/// Kern von `workbench.hypothesis` (testbar ohne Sandbox-Kontext).
fn execute_hypothesis(
    store: &KnowledgeStore,
    session_id: &str,
    arguments: serde_json::Value,
    now: jiff::Timestamp,
) -> ToolOutput {
    let args: HypothesisArgs = match parse_args(WORKBENCH_HYPOTHESIS, &arguments) {
        Ok(args) => args,
        Err(out) => return out,
    };
    let scope = WorkbenchScope::Session(session_id.to_owned());
    let author = model_author(session_id);
    let result = match args.action.as_str() {
        "add" => workbench::add_hypothesis(store, &scope, &author, &args.text, now).map(|number| {
            serde_json::json!({ "ok": true, "id": format!("#{number}"), "status": "testing" })
        }),
        "confirm" | "reject" => {
            let decided = if args.action == "confirm" {
                workbench::confirm(store, &scope, &author, &args.text, now)
            } else {
                workbench::reject(store, &scope, &author, &args.text, now)
            };
            decided.map(|hypothesis| {
                serde_json::json!({
                    "ok": true,
                    "text": hypothesis.text,
                    "status": hypothesis.status.label(),
                })
            })
        }
        other => {
            return ToolOutput::error(format!(
                "{WORKBENCH_HYPOTHESIS}: unbekannte action '{other}' (add, confirm, reject)"
            ));
        }
    };
    match result {
        Ok(value) => ToolOutput::json(value),
        Err(error) => ToolOutput::error(format!("{WORKBENCH_HYPOTHESIS}: {error}")),
    }
}

// --- Lesende Fläche -----------------------------------------------------------

/// Das rein lesende Werkzeug `workbench.show`.
///
/// # Rechteklasse
/// [`Permission::ReadWorkspace`]; liest nur harness-eigene Workbench-Dateien
/// des eigenen Scopes, nie angeheftete Dateien selbst. Darum für
/// `AUTO_APPROVED_TOOLS` geeignet.
#[derive(Debug, Clone)]
pub struct WorkbenchReadToolProvider {
    store: Arc<KnowledgeStore>,
    project: Option<WorkbenchScope>,
}

impl WorkbenchReadToolProvider {
    /// Die Werkzeugnamen in Provider-Reihenfolge.
    pub const TOOL_NAMES: &'static [&'static str] = &[WORKBENCH_SHOW];

    /// Die Rechteklasse je Werkzeug, parallel zu [`Self::TOOL_NAMES`].
    pub const TOOL_PERMISSIONS: &'static [Option<Permission>] = &[Some(Permission::ReadWorkspace)];

    /// Baut den Provider; ohne [`Self::with_project`] kennt er nur die Sitzung.
    #[must_use]
    pub fn new(store: Arc<KnowledgeStore>) -> Self {
        Self {
            store,
            project: None,
        }
    }

    /// Legt das Projekt der Sitzung fest (z. B.
    /// `WorkbenchScope::project_for_path(<Projektwurzel>)`); nur ein
    /// [`WorkbenchScope::Project`] wird übernommen.
    #[must_use]
    pub fn with_project(mut self, project: Option<WorkbenchScope>) -> Self {
        self.project = project.filter(|scope| !scope.is_session());
        self
    }
}

impl ToolProvider for WorkbenchReadToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![show_spec()]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        (name.as_str() == WORKBENCH_SHOW).then(|| {
            Arc::new(WorkbenchShowExecutor {
                store: Arc::clone(&self.store),
                project: self.project.clone(),
            }) as Arc<dyn ToolExecutor>
        })
    }

    fn parallel_safe(&self, name: &ToolName) -> bool {
        name.as_str() == WORKBENCH_SHOW
    }
}

fn show_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    let mut scope = string_property(
        "\"session\": die Workbench DIESER Sitzung; \"project\": die Projekt-Workbench \
         des aktuellen Projekts (falls bekannt).",
    );
    scope.enum_values = Some(vec![
        serde_json::json!("session"),
        serde_json::json!("project"),
    ]);
    props.insert("scope".to_owned(), scope);
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(WORKBENCH_SHOW),
        description: "Liest die Workbench (nur lesend): angeheftete Dateien (Pfad + Begründung, \
             kein Dateiinhalt), Hypothesen mit Nummer #<n> und das Ende der Notizen. \
             Gekürzt auf 4 KiB. Nur die eigene Sitzung bzw. das eigene Projekt."
            .to_owned(),
        parameters: object_schema_all_required(props),
        strict: true,
    })
}

struct WorkbenchShowExecutor {
    store: Arc<KnowledgeStore>,
    project: Option<WorkbenchScope>,
}

impl ToolExecutor for WorkbenchShowExecutor {
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let session_id = context.session_id().as_str().to_owned();
        let arguments = call.arguments.clone();
        Box::pin(async move {
            Ok(execute_show(
                &self.store,
                &session_id,
                self.project.as_ref(),
                arguments,
            ))
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowArgs {
    scope: String,
}

/// Kern von `workbench.show` (testbar ohne Sandbox-Kontext).
fn execute_show(
    store: &KnowledgeStore,
    session_id: &str,
    project: Option<&WorkbenchScope>,
    arguments: serde_json::Value,
) -> ToolOutput {
    let args: ShowArgs = match parse_args(WORKBENCH_SHOW, &arguments) {
        Ok(args) => args,
        Err(out) => return out,
    };
    let scope = match args.scope.as_str() {
        "session" => WorkbenchScope::Session(session_id.to_owned()),
        "project" => match project {
            Some(project) => project.clone(),
            None => {
                return ToolOutput::error(format!(
                    "{WORKBENCH_SHOW}: für diese Sitzung ist kein Projekt bekannt"
                ));
            }
        },
        other => {
            return ToolOutput::error(format!(
                "{WORKBENCH_SHOW}: unbekannter scope '{other}' (session, project)"
            ));
        }
    };
    match workbench::load(store, &scope) {
        Ok(bench) => {
            let body = workbench::digest(
                &bench,
                DigestOptions {
                    max_bytes: WORKBENCH_SHOW_MAX_BYTES,
                    notes_lines: SHOW_NOTES_LINES,
                    include_decided: true,
                },
            );
            let scope_name = scope.path_component();
            if body.is_empty() {
                ToolOutput::text(format!("Workbench {scope_name} ist leer."))
            } else {
                ToolOutput::text(format!("Workbench {scope_name}\n{body}"))
            }
        }
        Err(error) => ToolOutput::error(format!("{WORKBENCH_SHOW}: {error}")),
    }
}

/// Kontextbeitrag „Werkbank": Pins und offene Hypothesen der Sitzung.
///
/// # Beschreibung
/// Liest je Turn den Sitzungs-Scope von
/// [`TurnInputContext::session_id`] und liefert höchstens ein Fragment
/// (Label [`WORKBENCH_CONTEXT_NAMESPACE`]), gedeckelt auf
/// [`WORKBENCH_CONTEXT_MAX_BYTES`]; Notizen bleiben draußen (die liest das
/// Modell bei Bedarf über `workbench.show`). Leerer Scope oder Lesefehler →
/// kein Fragment (Fehler nur als `tracing::debug`). Vertrauensklasse ist die
/// Vorgabe `TrustClass::Data`: Pins und Hypothesen sind Nutzer-/Modelltext.
#[derive(Debug, Clone)]
pub struct WorkbenchContextProvider {
    store: Arc<KnowledgeStore>,
}

impl WorkbenchContextProvider {
    /// Baut den Provider über dem Speicher der Montage.
    #[must_use]
    pub fn new(store: Arc<KnowledgeStore>) -> Self {
        Self { store }
    }
}

impl ContextProvider for WorkbenchContextProvider {
    fn contribute<'a>(&'a self, ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>> {
        let session_id = ctx.session_id.as_str().to_owned();
        Box::pin(async move {
            workbench_context_fragment(&self.store, &session_id)
                .into_iter()
                .collect()
        })
    }

    fn namespace(&self) -> &'static str {
        WORKBENCH_CONTEXT_NAMESPACE
    }
}

/// Kern von [`WorkbenchContextProvider`] (testbar ohne Laufzeit).
fn workbench_context_fragment(store: &KnowledgeStore, session_id: &str) -> Option<ContextFragment> {
    let scope = WorkbenchScope::Session(session_id.to_owned());
    let bench = match workbench::load(store, &scope) {
        Ok(bench) => bench,
        Err(error) => {
            tracing::debug!(%error, "workbench.context.load_failed");
            return None;
        }
    };
    const HEADER: &str = "Werkbank dieser Sitzung (workbench.show für Details):\n";
    let body = workbench::digest(
        &bench,
        DigestOptions {
            max_bytes: WORKBENCH_CONTEXT_MAX_BYTES.saturating_sub(HEADER.len()),
            notes_lines: 0,
            include_decided: false,
        },
    );
    (!body.is_empty()).then(|| ContextFragment {
        label: WORKBENCH_CONTEXT_NAMESPACE.to_owned(),
        content: format!("{HEADER}{body}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn temporary_store(label: &str) -> TestResult<Arc<KnowledgeStore>> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-registry-workbench-{label}-{}-{nonce}",
            std::process::id()
        ));
        Ok(Arc::new(KnowledgeStore::new(&root)))
    }

    fn is_error(output: &ToolOutput) -> bool {
        matches!(output, ToolOutput::Error { .. })
    }

    /// Runde 5, Teil C: nur erfolgreiche Schreibvorgänge melden sich mit
    /// Sitzung und Scope; ohne Rückruf passiert nichts.
    #[test]
    fn change_notifier_fires_only_after_successful_writes() -> TestResult {
        let store = temporary_store("notify")?;
        let seen: Arc<std::sync::Mutex<Vec<(String, String)>>> = Arc::default();
        let sink = Arc::clone(&seen);
        let notifier: WorkbenchChangeNotifier = Arc::new(move |session: &str, scope: &str| {
            if let Ok(mut seen) = sink.lock() {
                seen.push((session.to_owned(), scope.to_owned()));
            }
        });
        let provider = WorkbenchToolProvider::new(Arc::clone(&store))
            .with_change_notifier(Arc::clone(&notifier));
        assert!(format!("{provider:?}").contains("notifier: true"));

        let now = jiff::Timestamp::now();
        let ok = execute_note(
            &store,
            "s-7",
            serde_json::json!({"text": "Zwischenstand"}),
            now,
        );
        assert!(!is_error(&ok));
        notify_change(Some(&notifier), "s-7", &ok);
        let failed = execute_note(&store, "s-7", serde_json::json!({"nope": 1}), now);
        assert!(is_error(&failed));
        notify_change(Some(&notifier), "s-7", &failed);
        let added = execute_hypothesis(
            &store,
            "s-7",
            serde_json::json!({"action": "add", "text": "Cache ist schuld"}),
            now,
        );
        notify_change(Some(&notifier), "s-7", &added);
        notify_change(None, "s-7", &added);

        let scope = WorkbenchScope::Session("s-7".to_owned()).path_component();
        let seen = seen
            .lock()
            .map_err(|_| TestError::Unexpected("notifier lock poisoned".to_owned()))?
            .clone();
        assert_eq!(
            seen,
            vec![("s-7".to_owned(), scope.clone()), ("s-7".to_owned(), scope),]
        );
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn provider_lists_exactly_the_two_tools_with_executors() {
        let provider =
            WorkbenchToolProvider::new(Arc::new(KnowledgeStore::new(std::path::Path::new("/x"))));
        let names: Vec<String> = provider
            .tools()
            .iter()
            .map(|spec| {
                let ToolSpec::Function(function) = spec;
                function.name.as_str().to_owned()
            })
            .collect();
        assert_eq!(
            names,
            vec![WORKBENCH_NOTE.to_owned(), WORKBENCH_HYPOTHESIS.to_owned()]
        );
        for name in WorkbenchToolProvider::TOOL_NAMES {
            assert!(provider.executor(&ToolName::new(*name)).is_some(), "{name}");
        }
        assert!(provider.executor(&ToolName::new("workbench.pin")).is_none());
        assert!(!provider.parallel_safe(&ToolName::new(WORKBENCH_NOTE)));
    }

    #[test]
    fn every_tool_is_read_only_safe() {
        assert_eq!(
            WorkbenchToolProvider::TOOL_NAMES.len(),
            WorkbenchToolProvider::TOOL_PERMISSIONS.len()
        );
        for permission in WorkbenchToolProvider::TOOL_PERMISSIONS {
            assert_eq!(*permission, Some(Permission::ReadWorkspace));
        }
    }

    #[test]
    fn note_writes_into_the_calling_sessions_scope_only() -> TestResult {
        let store = temporary_store("note")?;
        let output = execute_note(
            &store,
            "s-1",
            serde_json::json!({ "text": "Zwischenstand" }),
            jiff::Timestamp::now(),
        );
        assert!(!is_error(&output), "{output:?}");
        let own = workbench::load(&store, &WorkbenchScope::Session("s-1".to_owned()))
            .map_err(ctx("load own scope"))?;
        assert!(own.notes.contains("Zwischenstand"));
        let other = workbench::load(&store, &WorkbenchScope::Session("s-2".to_owned()))
            .map_err(ctx("load other scope"))?;
        assert!(other.is_empty());
        Ok(())
    }

    #[test]
    fn note_rejects_unknown_fields_and_empty_text() -> TestResult {
        let store = temporary_store("note-invalid")?;
        let now = jiff::Timestamp::now();
        assert!(is_error(&execute_note(
            &store,
            "s-1",
            serde_json::json!({ "text": "x", "scope": "project:other" }),
            now
        )));
        assert!(is_error(&execute_note(
            &store,
            "s-1",
            serde_json::json!({ "text": "   " }),
            now
        )));
        Ok(())
    }

    fn show_text(output: ToolOutput) -> TestResult<String> {
        match output {
            ToolOutput::Text { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn read_provider_lists_only_show_and_is_read_only() {
        let provider = WorkbenchReadToolProvider::new(Arc::new(KnowledgeStore::new(
            std::path::Path::new("/x"),
        )));
        let names: Vec<String> = provider
            .tools()
            .iter()
            .map(|spec| {
                let ToolSpec::Function(function) = spec;
                function.name.as_str().to_owned()
            })
            .collect();
        assert_eq!(names, vec![WORKBENCH_SHOW.to_owned()]);
        assert!(provider.executor(&ToolName::new(WORKBENCH_SHOW)).is_some());
        assert!(provider.executor(&ToolName::new(WORKBENCH_NOTE)).is_none());
        assert!(provider.parallel_safe(&ToolName::new(WORKBENCH_SHOW)));
        assert_eq!(
            WorkbenchReadToolProvider::TOOL_PERMISSIONS,
            &[Some(Permission::ReadWorkspace)]
        );
    }

    #[test]
    fn show_reads_only_the_callers_session_and_own_project() -> TestResult {
        let store = temporary_store("show")?;
        let now = jiff::Timestamp::now();
        let author = AgentId::new("operator");
        let own = WorkbenchScope::Session("s-1".to_owned());
        let foreign = WorkbenchScope::Session("s-2".to_owned());
        let project = WorkbenchScope::Project("harw".to_owned());
        workbench::pin(&store, &own, &author, "/abs/eigen.rs", "Einstieg", now)
            .map_err(ctx("pin"))?;
        workbench::add_hypothesis(&store, &own, &author, "Cache kalt", now)
            .map_err(ctx("hypothesis"))?;
        workbench::append_note(&store, &own, &author, "eigene Notiz", now).map_err(ctx("note"))?;
        workbench::append_note(&store, &foreign, &author, "fremd geheim", now)
            .map_err(ctx("foreign"))?;
        workbench::append_note(&store, &project, &author, "Projektnotiz", now)
            .map_err(ctx("project"))?;

        let text = show_text(execute_show(
            &store,
            "s-1",
            Some(&project),
            serde_json::json!({ "scope": "session" }),
        ))?;
        assert!(text.contains("/abs/eigen.rs — Einstieg"), "{text}");
        assert!(text.contains("#1 Cache kalt"), "{text}");
        assert!(text.contains("eigene Notiz"), "{text}");
        assert!(!text.contains("fremd geheim"), "{text}");

        let project_text = show_text(execute_show(
            &store,
            "s-1",
            Some(&project),
            serde_json::json!({ "scope": "project" }),
        ))?;
        assert!(project_text.contains("Projektnotiz"), "{project_text}");

        // Ohne Projekt, fremde Felder oder Scope-Werte: Fehler statt Fremdzugriff.
        for (project, args) in [
            (None, serde_json::json!({ "scope": "project" })),
            (
                Some(&project),
                serde_json::json!({ "scope": "session:s-2" }),
            ),
            (
                Some(&project),
                serde_json::json!({ "scope": "session", "session": "s-2" }),
            ),
        ] {
            assert!(is_error(&execute_show(&store, "s-1", project, args)));
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn show_output_is_capped() -> TestResult {
        let store = temporary_store("show-cap")?;
        let now = jiff::Timestamp::now();
        let own = WorkbenchScope::Session("s-1".to_owned());
        for index in 0..40 {
            workbench::add_hypothesis(
                &store,
                &own,
                &AgentId::new("operator"),
                &format!("Hypothese {index} {}", "x".repeat(200)),
                now,
            )
            .map_err(ctx("hypothesis"))?;
        }
        let text = show_text(execute_show(
            &store,
            "s-1",
            None,
            serde_json::json!({ "scope": "session" }),
        ))?;
        assert!(
            text.len() <= WORKBENCH_SHOW_MAX_BYTES + 64,
            "{}",
            text.len()
        );
        assert!(text.ends_with("(gekürzt)"));
        let empty = show_text(execute_show(
            &store,
            "leer",
            None,
            serde_json::json!({ "scope": "session" }),
        ))?;
        assert!(empty.ends_with("ist leer."));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn context_fragment_carries_pins_and_open_hypotheses_only() -> TestResult {
        let store = temporary_store("context")?;
        let now = jiff::Timestamp::now();
        let own = WorkbenchScope::Session("s-1".to_owned());
        let author = AgentId::new("operator");
        assert!(workbench_context_fragment(&store, "s-1").is_none());
        workbench::pin(&store, &own, &author, "/abs/k.rs", "", now).map_err(ctx("pin"))?;
        workbench::add_hypothesis(&store, &own, &author, "offen", now).map_err(ctx("h1"))?;
        workbench::add_hypothesis(&store, &own, &author, "verworfen", now).map_err(ctx("h2"))?;
        workbench::reject(&store, &own, &author, "#2", now).map_err(ctx("reject"))?;
        workbench::append_note(&store, &own, &author, "nicht im Kontext", now)
            .map_err(ctx("note"))?;
        let fragment =
            workbench_context_fragment(&store, "s-1").ok_or(TestError::Missing("fragment"))?;
        assert_eq!(fragment.label, WORKBENCH_CONTEXT_NAMESPACE);
        assert!(fragment.content.contains("/abs/k.rs"));
        assert!(fragment.content.contains("#1 offen"));
        assert!(!fragment.content.contains("verworfen"));
        assert!(!fragment.content.contains("nicht im Kontext"));
        assert!(fragment.content.len() <= WORKBENCH_CONTEXT_MAX_BYTES);
        assert!(workbench_context_fragment(&store, "s-2").is_none());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[tokio::test]
    async fn context_provider_reads_the_turns_session() -> TestResult {
        let store = temporary_store("context-provider")?;
        let session = harw_types::SessionId::new();
        let own = WorkbenchScope::Session(session.as_str().to_owned());
        workbench::add_hypothesis(
            &store,
            &own,
            &AgentId::new("operator"),
            "im Turn",
            jiff::Timestamp::now(),
        )
        .map_err(ctx("hypothesis"))?;
        let provider = WorkbenchContextProvider::new(Arc::clone(&store));
        assert_eq!(provider.namespace(), WORKBENCH_CONTEXT_NAMESPACE);
        let turn = TurnInputContext {
            session_id: session,
            turn_id: harw_types::TurnId::new(),
            metadata: serde_json::Value::Null,
        };
        let fragments = provider.contribute(&turn).await;
        assert_eq!(fragments.len(), 1);
        assert!(fragments[0].content.contains("im Turn"));
        let other = TurnInputContext {
            session_id: harw_types::SessionId::new(),
            turn_id: harw_types::TurnId::new(),
            metadata: serde_json::Value::Null,
        };
        assert!(provider.contribute(&other).await.is_empty());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn hypothesis_add_confirm_and_refuse_a_second_decision() -> TestResult {
        let store = temporary_store("hypothesis")?;
        let now = jiff::Timestamp::now();
        let added = execute_hypothesis(
            &store,
            "s-1",
            serde_json::json!({ "action": "add", "text": "Cache kalt" }),
            now,
        );
        let ToolOutput::Json { content } = added else {
            return Err(TestError::Unexpected(format!(
                "add must succeed: {added:?}"
            )));
        };
        assert_eq!(content["id"], "#1");

        let confirmed = execute_hypothesis(
            &store,
            "s-1",
            serde_json::json!({ "action": "confirm", "text": "#1" }),
            now,
        );
        assert!(!is_error(&confirmed), "{confirmed:?}");
        let again = execute_hypothesis(
            &store,
            "s-1",
            serde_json::json!({ "action": "reject", "text": "Cache kalt" }),
            now,
        );
        assert!(is_error(&again));
        let unknown = execute_hypothesis(
            &store,
            "s-1",
            serde_json::json!({ "action": "maybe", "text": "x" }),
            now,
        );
        assert!(is_error(&unknown));
        Ok(())
    }
}
