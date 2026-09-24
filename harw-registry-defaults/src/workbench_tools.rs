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
//! # Nebenläufigkeit
//! `Send + Sync`; hält nur ein `Arc<KnowledgeStore>`. Die Read-Modify-Write-
//! Zyklen serialisiert `harw_knowledge::workbench` prozessweit.
//!
//! # Fehler
//! Kein Aufruf gibt `Err` zurück: ungültige Argumente und Speicherfehler
//! werden als [`ToolOutput::error`] gemeldet.

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_extension_api::contributors::ToolProvider;
use harw_extension_api::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};
use harw_knowledge::workbench::{self, WorkbenchScope};
use harw_knowledge::{AgentId, KnowledgeStore};
use harw_tools::{AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType, Permission};
use serde::Deserialize;

/// Name des Notiz-Werkzeugs.
pub const WORKBENCH_NOTE: &str = "workbench.note";

/// Name des Hypothesen-Werkzeugs.
pub const WORKBENCH_HYPOTHESIS: &str = "workbench.hypothesis";

/// Die Workbench-Modell-Werkzeuge über einem geteilten Wissensspeicher.
#[derive(Debug, Clone)]
pub struct WorkbenchToolProvider {
    store: Arc<KnowledgeStore>,
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
        Self { store }
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
        }))
    }
}

fn string_property(description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::String),
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

fn object_schema(props: BTreeMap<String, JsonSchema>) -> JsonSchema {
    let required = props.keys().cloned().collect();
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(props),
        required: Some(required),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
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
        parameters: object_schema(props),
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
        parameters: object_schema(props),
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
            Ok(match self.tool {
                WorkbenchTool::Note => execute_note(&self.store, &session_id, arguments, now),
                WorkbenchTool::Hypothesis => {
                    execute_hypothesis(&self.store, &session_id, arguments, now)
                }
            })
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
    let args: NoteArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(error) => {
            return ToolOutput::error(format!("{WORKBENCH_NOTE}: ungültige Argumente: {error}"));
        }
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
    let args: HypothesisArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(error) => {
            return ToolOutput::error(format!(
                "{WORKBENCH_HYPOTHESIS}: ungültige Argumente: {error}"
            ));
        }
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
