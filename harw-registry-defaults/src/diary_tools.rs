//! `DiaryToolProvider` — das Lese-Werkzeug `diary.read` (Plan D3,
//! `docs/design/knowledge-surfaces.md` §3).
//!
//! # Verantwortung
//! Ein Agent darf sein **eigenes** Tagebuch lesen: die automatischen Einträge
//! (Compaction, Sitzungsende, Traum-Reflexion) und Operator-Notizen in
//! `diary/<agent-id>/`. Die Agent-Id wird beim Bau des Providers von der
//! Montage gebunden (derselbe Agent, unter dem die Runtime die automatischen
//! Einträge schreibt) — nie aus einem Argument. Der
//! [`ToolExecutionContext`] trägt nur die Sitzungs-Id; anders als die
//! sitzungsgebundene Workbench (`workbench_tools.rs`, Autor
//! `model@<session>`) ist das Tagebuch agentengebunden und überdauert
//! Sitzungen, darum kommt die Identität aus der Montage.
//!
//! # Sichtbarkeit
//! Automatische Einträge tragen `OperatorOnly`, damit `/diary` sie dem
//! Operator zeigt; `VisibilityScope::visible_to_caller` ließe einen Agenten
//! (`SelfOnly`) nichts sehen. Das Werkzeug prüft deshalb die Identität selbst
//! (D0-Hinweis „eigene Einträge gegen die Agent-Id"): gelesen wird
//! ausschließlich das Verzeichnis des gebundenen Agenten, und nur Tage, deren
//! Halter ([`harw_knowledge::diary::DiaryDay::agent_id`]) dieser Agent ist.
//! Die reservierte Operator-Id `operator` wird nie gebunden.
//!
//! # Rechteklasse
//! Rein lesend, [`Permission::ReadWorkspace`]
//! ([`DiaryToolProvider::TOOL_PERMISSIONS`]).
//!
//! # Deckel
//! Höchstens [`MAX_RANGE_DAYS`] Tage je Aufruf, [`MAX_ENTRIES`] Einträge und
//! [`MAX_OUTPUT_BYTES`] Text; bei Überschreitung gewinnen die jüngsten
//! Einträge und `truncated` ist `true`.
//!
//! # Fehler
//! Kein Aufruf gibt `Err` zurück: ungültige Argumente und Speicherfehler
//! werden als [`ToolOutput::error`] gemeldet.

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_extension_api::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};
use harw_knowledge::diary::{self, DiaryTrigger};
use harw_knowledge::{AgentId, KnowledgeStore};
use harw_tools::{AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType, Permission};
use serde::Deserialize;

/// Name des Lese-Werkzeugs.
pub const DIARY_READ: &str = "diary.read";

/// Größter Datumsbereich eines Aufrufs in Tagen (inklusive beider Enden).
pub const MAX_RANGE_DAYS: i64 = 31;

/// Höchstzahl gelieferter Einträge je Aufruf.
pub const MAX_ENTRIES: usize = 50;

/// Obergrenze der gelieferten Eintragstexte in Bytes.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024;

/// Die reservierte Autor-Id des Operators (`harw_ops::knowledge_common`).
const OPERATOR_ID: &str = "operator";

/// Das Diary-Lese-Werkzeug über einem geteilten Wissensspeicher, gebunden an
/// einen Agenten.
#[derive(Debug, Clone)]
pub struct DiaryToolProvider {
    store: Arc<KnowledgeStore>,
    agent: AgentId,
}

impl DiaryToolProvider {
    /// Baut den Provider.
    ///
    /// # Argumente
    /// - `store` (`Arc<KnowledgeStore>`): derselbe Speicher wie
    ///   `RuntimeServices::knowledge_store`.
    /// - `agent` (`AgentId`): der Agent der Montage, dessen Tagebuch gelesen
    ///   werden darf (dieselbe Id, unter der die Runtime automatische
    ///   Einträge schreibt).
    #[must_use]
    pub fn new(store: Arc<KnowledgeStore>, agent: AgentId) -> Self {
        Self { store, agent }
    }
}

// `diary.read` liest nur das eigene Tagebuch (`ReadWorkspace`); der Provider
// deklariert keine Parallelitäts-Zusage (Standard `none`).
harw_tools::tool_provider! {
    impl for DiaryToolProvider as provider {
        DIARY_READ => {
            spec: read_spec(),
            permission: Permission::ReadWorkspace,
            executor: DiaryReadExecutor {
                store: Arc::clone(&provider.store),
                agent: provider.agent.clone(),
            },
        },
    }
}

fn string_property(description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::String),
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

fn read_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "from".to_owned(),
        string_property("Erster Tag (YYYY-MM-DD, UTC). Ohne Angabe: heute bzw. `to`."),
    );
    props.insert(
        "to".to_owned(),
        string_property("Letzter Tag (YYYY-MM-DD, UTC). Ohne Angabe: heute."),
    );
    let mut trigger = string_property("Nur Einträge dieses Auslösers.");
    trigger.enum_values = Some(
        [
            DiaryTrigger::EndOfSession,
            DiaryTrigger::Compaction,
            DiaryTrigger::DreamReflection,
            DiaryTrigger::Manual,
        ]
        .into_iter()
        .map(|trigger| serde_json::json!(trigger.label()))
        .collect(),
    );
    props.insert("trigger".to_owned(), trigger);
    let parameters = JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(props),
        required: Some(Vec::new()),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
    .into_strict();
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(DIARY_READ),
        description: format!(
            "Liest DEIN eigenes Tagebuch (Zusammenfassungen nach Verdichtungen, \
             Sitzungsenden, Reflexionen, Operator-Notizen) für einen Datumsbereich; \
             ohne Angaben heute. Höchstens {MAX_RANGE_DAYS} Tage, {MAX_ENTRIES} Einträge \
             und {} KiB je Aufruf, die jüngsten gewinnen. Nur lesend.",
            MAX_OUTPUT_BYTES / 1024
        ),
        parameters,
        strict: true,
    })
}

struct DiaryReadExecutor {
    store: Arc<KnowledgeStore>,
    agent: AgentId,
}

impl ToolExecutor for DiaryReadExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let arguments = call.arguments.clone();
        Box::pin(async move {
            Ok(execute_read(
                &self.store,
                &self.agent,
                arguments,
                jiff::Timestamp::now(),
            ))
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
}

/// Ein gelieferter Eintrag.
struct Found {
    date: String,
    time: String,
    trigger: &'static str,
    text: String,
}

/// Kern von `diary.read` (testbar ohne Sandbox-Kontext).
fn execute_read(
    store: &KnowledgeStore,
    agent: &AgentId,
    arguments: serde_json::Value,
    now: jiff::Timestamp,
) -> ToolOutput {
    let fail = |detail: String| ToolOutput::error(format!("{DIARY_READ}: {detail}"));
    if agent.as_str() == OPERATOR_ID || diary::validate_agent_id(agent).is_err() {
        return fail("kein Agenten-Tagebuch für diese Sitzung".to_owned());
    }
    let args: ReadArgs = if arguments.is_null() {
        ReadArgs::default()
    } else {
        match serde_json::from_value(arguments) {
            Ok(args) => args,
            Err(error) => return fail(format!("ungültige Argumente: {error}")),
        }
    };
    let today = now.strftime("%Y-%m-%d").to_string();
    let to = args.to.unwrap_or_else(|| today.clone());
    let from = args.from.unwrap_or_else(|| to.clone());
    let (from_date, to_date) = match (parse_day(&from), parse_day(&to)) {
        (Some(from_date), Some(to_date)) => (from_date, to_date),
        _ => return fail(format!("ungültiges Datum '{from}'/'{to}' (YYYY-MM-DD)")),
    };
    if from_date > to_date {
        return fail(format!("'from' ({from}) liegt nach 'to' ({to})"));
    }
    let span = from_date.until(to_date).map(|span| span.get_days());
    if !matches!(span, Ok(days) if i64::from(days) < MAX_RANGE_DAYS) {
        return fail(format!("Bereich größer als {MAX_RANGE_DAYS} Tage"));
    }
    let trigger = match args.trigger.as_deref() {
        None => None,
        Some(label) => match DiaryTrigger::from_label(label) {
            Some(trigger) => Some(trigger),
            None => return fail(format!("unbekannter trigger '{label}'")),
        },
    };
    let days = match diary::read_range(store, agent, &from, &to) {
        Ok(days) => days,
        Err(error) => return fail(error.to_string()),
    };
    let mut found: Vec<Found> = days
        .into_iter()
        // Identitätsprüfung statt Sichtbarkeit (Moduldoku).
        .filter(|day| &day.agent_id == agent)
        .flat_map(|day| {
            let date = day.date;
            day.entries.into_iter().map(move |entry| Found {
                date: date.clone(),
                time: entry.recorded_at.strftime("%H:%M:%S").to_string(),
                trigger: entry.trigger.label(),
                text: entry.text,
            })
        })
        .filter(|entry| trigger.is_none_or(|wanted| entry.trigger == wanted.label()))
        .collect();
    let total = found.len();
    // Die jüngsten Einträge gewinnen: von hinten füllen, dann umdrehen.
    let mut kept: Vec<Found> = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    while let Some(mut entry) = found.pop() {
        if kept.len() >= MAX_ENTRIES || bytes >= MAX_OUTPUT_BYTES {
            truncated = true;
            break;
        }
        let budget = MAX_OUTPUT_BYTES - bytes;
        if entry.text.len() > budget {
            entry.text = diary::truncate_entry_text(&entry.text, budget);
            truncated = true;
        }
        bytes += entry.text.len();
        kept.push(entry);
    }
    kept.reverse();
    let entries: Vec<serde_json::Value> = kept
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "date": entry.date,
                "time": entry.time,
                "trigger": entry.trigger,
                "text": entry.text,
            })
        })
        .collect();
    ToolOutput::json(serde_json::json!({
        "agent": agent.as_str(),
        "from": from,
        "to": to,
        "total": total,
        "truncated": truncated,
        "entries": entries,
    }))
}

/// Parst `YYYY-MM-DD` streng (Ziffernform plus gültiges Kalenderdatum).
fn parse_day(raw: &str) -> Option<jiff::civil::Date> {
    if !diary::is_day_key(raw) {
        return None;
    }
    raw.parse::<jiff::civil::Date>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_extension_api::contributors::ToolProvider;

    fn temporary_store(label: &str) -> TestResult<Arc<KnowledgeStore>> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-registry-diary-{label}-{}-{nonce}",
            std::process::id()
        ));
        Ok(Arc::new(KnowledgeStore::new(&root)))
    }

    fn noon() -> TestResult<jiff::Timestamp> {
        // 2025-09-24T12:00:00Z
        jiff::Timestamp::from_second(1_758_715_200).map_err(ctx("valid timestamp"))
    }

    fn days_before(now: jiff::Timestamp, days: i64) -> TestResult<jiff::Timestamp> {
        now.checked_sub(jiff::SignedDuration::from_secs(days * 86_400))
            .map_err(ctx("timestamp in range"))
    }

    fn json(output: ToolOutput) -> TestResult<serde_json::Value> {
        match output {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("expected json: {other:?}"))),
        }
    }

    fn texts(content: &serde_json::Value) -> Vec<String> {
        content["entries"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry["text"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn provider_lists_exactly_one_read_only_tool() {
        let provider = DiaryToolProvider::new(
            Arc::new(KnowledgeStore::new(std::path::Path::new("/x"))),
            AgentId::new("explorer"),
        );
        let names: Vec<String> = provider
            .tools()
            .iter()
            .map(|spec| {
                let ToolSpec::Function(function) = spec;
                function.name.as_str().to_owned()
            })
            .collect();
        assert_eq!(names, vec![DIARY_READ.to_owned()]);
        assert!(provider.executor(&ToolName::new(DIARY_READ)).is_some());
        assert!(provider.executor(&ToolName::new("diary.note")).is_none());
        assert_eq!(
            DiaryToolProvider::TOOL_PERMISSIONS,
            &[Some(Permission::ReadWorkspace)]
        );
    }

    #[test]
    fn defaults_to_today_and_filters_by_trigger() -> TestResult {
        let store = temporary_store("today")?;
        let me = AgentId::new("explorer");
        let now = noon()?;
        diary::record(&store, &me, DiaryTrigger::Compaction, "verdichtet", now)
            .map_err(ctx("compaction entry"))?;
        diary::record(&store, &me, DiaryTrigger::EndOfSession, "Ende", now)
            .map_err(ctx("end entry"))?;
        diary::record(
            &store,
            &me,
            DiaryTrigger::Manual,
            "gestern",
            days_before(now, 1)?,
        )
        .map_err(ctx("yesterday entry"))?;

        let content = json(execute_read(&store, &me, serde_json::Value::Null, now))?;
        assert_eq!(texts(&content), vec!["verdichtet", "Ende"]);
        assert_eq!(content["from"], "2025-09-24");

        let filtered = json(execute_read(
            &store,
            &me,
            serde_json::json!({ "trigger": "compaction", "from": null, "to": null }),
            now,
        ))?;
        assert_eq!(texts(&filtered), vec!["verdichtet"]);

        let range = json(execute_read(
            &store,
            &me,
            serde_json::json!({ "from": "2025-09-23", "to": "2025-09-24" }),
            now,
        ))?;
        assert_eq!(texts(&range), vec!["gestern", "verdichtet", "Ende"]);
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn foreign_agent_entries_are_invisible() -> TestResult {
        let store = temporary_store("foreign")?;
        let now = noon()?;
        let me = AgentId::new("explorer");
        diary::record(
            &store,
            &AgentId::new("writer"),
            DiaryTrigger::Manual,
            "fremd",
            now,
        )
        .map_err(ctx("foreign entry"))?;
        diary::record(&store, &me, DiaryTrigger::Manual, "eigen", now).map_err(ctx("own entry"))?;

        let content = json(execute_read(&store, &me, serde_json::json!({}), now))?;
        assert_eq!(texts(&content), vec!["eigen"]);

        // Kein Argument wählt einen anderen Agenten.
        let smuggled = execute_read(&store, &me, serde_json::json!({ "agent": "writer" }), now);
        assert!(matches!(smuggled, ToolOutput::Error { .. }));

        // Das Operator-Tagebuch ist nie bindbar.
        diary::record(
            &store,
            &AgentId::new("operator"),
            DiaryTrigger::Manual,
            "op",
            now,
        )
        .map_err(ctx("operator entry"))?;
        let operator = execute_read(
            &store,
            &AgentId::new("operator"),
            serde_json::json!({}),
            now,
        );
        assert!(matches!(operator, ToolOutput::Error { .. }));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn output_is_capped_and_keeps_the_newest_entries() -> TestResult {
        let store = temporary_store("cap")?;
        let me = AgentId::new("explorer");
        let now = noon()?;
        for index in 0..(MAX_ENTRIES + 5) {
            let at = now
                .checked_add(jiff::SignedDuration::from_secs(
                    i64::try_from(index).unwrap_or(0),
                ))
                .map_err(ctx("timestamp in range"))?;
            diary::record(&store, &me, DiaryTrigger::Manual, &format!("e{index}"), at)
                .map_err(ctx("entry"))?;
        }
        let content = json(execute_read(&store, &me, serde_json::json!({}), now))?;
        let found = texts(&content);
        assert_eq!(found.len(), MAX_ENTRIES);
        assert_eq!(content["truncated"], true);
        assert_eq!(content["total"], MAX_ENTRIES + 5);
        assert_eq!(found.last().map(String::as_str), Some("e54"));
        assert_eq!(found.first().map(String::as_str), Some("e5"));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn invalid_ranges_and_triggers_are_errors() -> TestResult {
        let store = temporary_store("invalid")?;
        let me = AgentId::new("explorer");
        let now = noon()?;
        for arguments in [
            serde_json::json!({ "from": "gestern" }),
            serde_json::json!({ "from": "2025-02-30" }),
            serde_json::json!({ "from": "2025-09-25", "to": "2025-09-24" }),
            serde_json::json!({ "from": "2025-01-01", "to": "2025-09-24" }),
            serde_json::json!({ "trigger": "unknown" }),
        ] {
            let output = execute_read(&store, &me, arguments.clone(), now);
            assert!(
                matches!(output, ToolOutput::Error { .. }),
                "{arguments} → {output:?}"
            );
        }
        Ok(())
    }
}
