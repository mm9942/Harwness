//! `/diary` — das Tagebuch je Agent (`docs/design/knowledge-surfaces.md` §3,
//! `interaction-contract.md` §2.2).
//!
//! # Subcommands
//! - (bare) / `show [AgentRef] [--date=YYYY-MM-DD]` — rendert einen Tag;
//!   ohne Angaben das eigene Tagebuch von heute (UTC).
//! - `show [AgentRef] --from=YYYY-MM-DD [--to=YYYY-MM-DD]` — Bereichsansicht
//!   (höchstens [`MAX_RANGE_DAYS`] Tage; `--to` fehlt → heute, `--from`
//!   fehlt → `--to`). Nicht mit `--date` kombinierbar.
//! - `today` — Kurzform für das eigene Tagebuch von heute.
//! - `search <text> [--agent=<id>] [--from=…] [--to=…]` — Volltextsuche
//!   (ohne Groß-/Kleinschreibung) über die strukturierten Einträge aller
//!   sichtbaren Tagebücher bzw. eines Agenten; höchstens
//!   [`MAX_SEARCH_HITS`] Treffer, die jüngsten zuerst.
//!
//! `show`/`today` liefern zusätzlich `OpOutput::data` =
//! `{"agent","date","entries":[{"time","trigger","text"}]}` für die TUI,
//! die Bereichsansicht `{"agent","from","to","days":[{"date","entries":[…]}]}`
//! und `search` `{"query","truncated","hits":[{"agent","date","time",
//! "trigger","text"}]}`.
//! - `agents` — die Agenten mit eigenem Tagebuch (Verzeichnisse unter
//!   `diary/`), `data` = `{"agents":[…]}`; für die Agentenwahl des
//!   Wissensbrowsers. Ein Nicht-Operator sieht höchstens sich selbst.
//! - `note <text>` — außerplanmäßiger Eintrag (`DiaryTrigger::Manual`) im
//!   eigenen Tagebuch. Das `#`-Präfix der TUI ist Zucker für genau diesen
//!   Pfad (ein Codepfad, eine Audit-Spur).
//!
//! # Sichtbarkeit
//! `show` liest über `harw_knowledge::diary::read_day_entries` (strukturiert
//! aus der `.jsonl`-Seitendatei, Rückfall auf das Markdown) und rendert
//! eine Tagesdatei nur, wenn ihre Sichtbarkeit für den echten Aufrufer gilt
//! ([`crate::knowledge_common::KnowledgeCaller`]: Operator → `OperatorOnly`,
//! Agent → `SelfOnly`, fail-closed über
//! `VisibilityScope::visible_to_caller`) — dieselbe Regel wie der
//! Recall-Pfad. Eine unsichtbare Datei wird wie eine fehlende gemeldet (kein
//! Informationsleck). `note` schreibt mit `VisibilityScope::OperatorOnly`,
//! damit der eigene Eintrag des Operators wieder lesbar ist; nach dem
//! Schreiben geht `AgentEventKind::Knowledge { area: "diary",
//! id: "<agent>/<datum>" }` über den Hub.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Knowledge-Store im Kontext.
//! - [`OpError::InvalidArguments`] — Grammatik, ungültiges Datum/Agent, leerer Text.
//! - [`OpError::Execution`] — Ein-/Ausgabefehler des Speichers.

use harw_knowledge::diary::{self, DiaryDay, DiaryEntry, DiaryTrigger};
use harw_knowledge::{AgentId, KnowledgeStore, VisibilityScope};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

use crate::knowledge_args::{FlagSpec, KnowledgeArgs};
use crate::knowledge_common::{
    AREA_DIARY, KnowledgeCaller, knowledge_store, map_knowledge_error, publish_knowledge,
};

/// Flags von `show`.
const SHOW_FLAGS: &[FlagSpec] = &[
    FlagSpec::value("date"),
    FlagSpec::value("from"),
    FlagSpec::value("to"),
];

/// Flags von `search`.
const SEARCH_FLAGS: &[FlagSpec] = &[
    FlagSpec::value("agent"),
    FlagSpec::value("from"),
    FlagSpec::value("to"),
];

/// Größter Bereich von `show --from/--to` in Tagen (inklusive).
pub const MAX_RANGE_DAYS: i64 = 92;

/// Höchstzahl der Treffer von `search`.
pub const MAX_SEARCH_HITS: usize = 50;

/// Länge des Textausschnitts eines Suchtreffers in der Textausgabe (Zeichen).
const SNIPPET_CHARS: usize = 160;

/// Obergrenze eines `/diary note`-Texts in Bytes.
const MAX_DIARY_NOTE_BYTES: usize = 16 * 1024;

/// Argument-Container für `/diary`: rohe Tokens.
#[derive(Debug, Default, serde::Deserialize)]
pub struct DiaryArgs {
    /// Alle Tokens nach `/diary`.
    #[serde(default)]
    pub tokens: Vec<String>,
}

impl harw_operations::FromRawArgs for DiaryArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            tokens: tokens.to_vec(),
        })
    }
}

/// Führt `/diary` aus.
///
/// # Fehler
/// Siehe Moduldoku.
#[operation(
    name = "diary",
    summary = "Tagebuch: show [agent] [--date=YYYY-MM-DD | --from=… --to=…], search <text>, agents, note <text>.",
    domain = "knowledge",
    permission = "operator",
    command(
        path = "/diary",
        visibility = "channel_reduced",
        busy_subcommands = "-=immediate, show=immediate, today=immediate, search=immediate, agents=immediate"
    )
)]
async fn diary(ctx: &OpContext, args: DiaryArgs) -> Result<OpOutput, OpError> {
    let store = knowledge_store(ctx)?;
    let caller = KnowledgeCaller::from_context(ctx);
    let now = jiff::Timestamp::now();
    let output = run_diary_as(&store, &caller, &args.tokens, now)?;
    if args.tokens.first().map(String::as_str) == Some("note") {
        publish_knowledge(
            ctx,
            AREA_DIARY,
            Some(format!("{}/{}", caller.agent, now.strftime("%Y-%m-%d"))),
        );
    }
    Ok(output)
}

/// Der reine Kern von `/diary` aus Operator-Sicht (testbar ohne
/// `OpContext`); gleichbedeutend mit [`run_diary_as`] und
/// [`KnowledgeCaller::operator`].
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_diary(
    store: &KnowledgeStore,
    caller: &AgentId,
    tokens: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    run_diary_as(
        store,
        &KnowledgeCaller::operator(caller.clone()),
        tokens,
        now,
    )
}

/// Der reine Kern von `/diary` für einen beliebigen Aufrufer.
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_diary_as(
    store: &KnowledgeStore,
    caller: &KnowledgeCaller,
    tokens: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    // Ohne Flag-Spezifikation: der Subcommand ist das erste Token, auch wenn
    // es ein Flag ist (`/diary --date=…` = `show`), und `note`-Text bleibt
    // unverändert.
    let args = KnowledgeArgs::parse(tokens, &[])?;
    match args.subcommand() {
        None => show(store, caller, &[], now),
        Some("show") => show(store, caller, args.rest(), now),
        Some("today") => show(store, caller, &[], now),
        Some("note") => note(store, &caller.agent, &args.rest_text(), now),
        Some("search") => search(store, caller, args.rest(), now),
        Some("agents") => agents(store, caller),
        // `/diary <agent>` ist Kurzform von `/diary show <agent>`.
        Some(_) => show(store, caller, tokens, now),
    }
}

/// `/diary agents` — die Agenten mit eigenem Tagebuch.
///
/// # Beschreibung
/// Liest die Agentenverzeichnisse über `harw_knowledge::diary::list_agents`
/// (sortiert, nur gültige Agent-Ids). Ein Operator sieht alle; jeder andere
/// Aufrufer höchstens sich selbst — die Existenz fremder Tagebücher ist
/// selbst schon eine Auskunft.
fn agents(store: &KnowledgeStore, caller: &KnowledgeCaller) -> Result<OpOutput, OpError> {
    let operator = caller.viewer == VisibilityScope::OperatorOnly;
    let agents: Vec<String> = diary::list_agents(store)
        .map_err(map_knowledge_error)?
        .into_iter()
        .filter(|agent| operator || *agent == caller.agent)
        .map(|agent| agent.as_str().to_owned())
        .collect();
    let text = if agents.is_empty() {
        "Keine Tagebücher vorhanden.".to_owned()
    } else {
        format!("{} Tagebücher: {}", agents.len(), agents.join(", "))
    };
    Ok(OpOutput {
        text,
        data: Some(serde_json::json!({ "agents": agents })),
    })
}

fn show(
    store: &KnowledgeStore,
    caller: &KnowledgeCaller,
    tokens: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let args = KnowledgeArgs::parse(tokens, SHOW_FLAGS)?;
    if args.value("from").is_some() || args.value("to").is_some() {
        if args.value("date").is_some() {
            return Err(OpError::InvalidArguments(
                "--date und --from/--to schließen sich aus".to_owned(),
            ));
        }
        return show_range(store, caller, &args, now);
    }
    let date = match args.value("date") {
        Some(date) => validate_date(date)?,
        None => now.strftime("%Y-%m-%d").to_string(),
    };
    let rest = args.positionals();
    if rest.len() > 1 {
        return Err(OpError::InvalidArguments(
            "Aufruf: /diary show [agent] [--date=YYYY-MM-DD]".to_owned(),
        ));
    }
    let agent = match rest.first() {
        Some(raw) => AgentId::new(validate_agent(raw)?),
        None => caller.agent.clone(),
    };
    let day = diary::read_day(store, &agent, &date)
        .map_err(map_knowledge_error)?
        .filter(|artifact| caller.can_read(&artifact.frontmatter.visibility));
    let entries = match &day {
        Some(_) => diary::read_day_entries(store, &agent, &date)
            .map_err(map_knowledge_error)?
            .filter(|structured| caller.can_read(&structured.visibility)),
        None => None,
    };
    let (Some(artifact), Some(entries)) = (day, entries) else {
        return Ok(OpOutput {
            text: format!("Kein sichtbarer Tagebucheintrag für {agent} am {date}."),
            data: Some(day_data(&agent, &date, None)),
        });
    };
    Ok(OpOutput {
        text: format!("Tagebuch {agent} — {date}\n\n{}", artifact.body.trim_end()),
        data: Some(day_data(&agent, &date, Some(&entries))),
    })
}

/// Bereichsansicht von `show --from/--to`.
fn show_range(
    store: &KnowledgeStore,
    caller: &KnowledgeCaller,
    args: &KnowledgeArgs,
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let rest = args.positionals();
    if rest.len() > 1 {
        return Err(OpError::InvalidArguments(
            "Aufruf: /diary show [agent] --from=YYYY-MM-DD [--to=YYYY-MM-DD]".to_owned(),
        ));
    }
    let agent = match rest.first() {
        Some(raw) => AgentId::new(validate_agent(raw)?),
        None => caller.agent.clone(),
    };
    let (from, to) = date_range(args, now)?;
    let days: Vec<DiaryDay> = diary::read_range(store, &agent, &from, &to)
        .map_err(map_knowledge_error)?
        .into_iter()
        .filter(|day| caller.can_read(&day.visibility))
        .collect();
    let data = serde_json::json!({
        "agent": agent.as_str(),
        "from": from,
        "to": to,
        "days": days
            .iter()
            .map(|day| serde_json::json!({ "date": day.date, "entries": entries_data(day) }))
            .collect::<Vec<_>>(),
    });
    if days.is_empty() {
        return Ok(OpOutput {
            text: format!("Keine sichtbaren Tagebucheinträge für {agent} von {from} bis {to}."),
            data: Some(data),
        });
    }
    let mut text = format!("Tagebuch {agent} — {from} bis {to}");
    for day in &days {
        text.push_str(&format!("\n\n## {}\n", day.date));
        for entry in &day.entries {
            text.push_str(&format!(
                "\n### {} — {}\n\n{}\n",
                entry.recorded_at.strftime("%H:%M:%S"),
                entry.trigger.label(),
                entry.text
            ));
        }
    }
    Ok(OpOutput {
        text: text.trim_end().to_owned(),
        data: Some(data),
    })
}

/// Liest `--from`/`--to` (Default: `to` = heute, `from` = `to`) und prüft
/// Reihenfolge und [`MAX_RANGE_DAYS`].
fn date_range(args: &KnowledgeArgs, now: jiff::Timestamp) -> Result<(String, String), OpError> {
    let to = match args.value("to") {
        Some(raw) => validate_date(raw)?,
        None => now.strftime("%Y-%m-%d").to_string(),
    };
    let from = match args.value("from") {
        Some(raw) => validate_date(raw)?,
        None => to.clone(),
    };
    let parse = |raw: &str| {
        raw.parse::<jiff::civil::Date>()
            .map_err(|_| OpError::InvalidArguments(format!("ungültiges Datum '{raw}'")))
    };
    let (start, end) = (parse(&from)?, parse(&to)?);
    if start > end {
        return Err(OpError::InvalidArguments(format!(
            "--from ({from}) liegt nach --to ({to})"
        )));
    }
    let span_days = start
        .until(end)
        .map(|span| i64::from(span.get_days()))
        .unwrap_or(i64::MAX);
    if span_days >= MAX_RANGE_DAYS {
        return Err(OpError::InvalidArguments(format!(
            "Bereich größer als {MAX_RANGE_DAYS} Tage"
        )));
    }
    Ok((from, to))
}

/// `/diary search <text> [--agent=…] [--from=…] [--to=…]`.
fn search(
    store: &KnowledgeStore,
    caller: &KnowledgeCaller,
    tokens: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let args = KnowledgeArgs::parse(tokens, SEARCH_FLAGS)?;
    let query = args.positionals().join(" ");
    let query = query.trim();
    if query.is_empty() {
        return Err(OpError::InvalidArguments(
            "Aufruf: /diary search <text> [--agent=<id>] [--from=YYYY-MM-DD] [--to=YYYY-MM-DD]"
                .to_owned(),
        ));
    }
    let needle = query.to_lowercase();
    let bounded = args.value("from").is_some() || args.value("to").is_some();
    let range = if bounded {
        Some(date_range(&args, now)?)
    } else {
        None
    };
    let agents = match args.value("agent") {
        Some(raw) => vec![AgentId::new(validate_agent(raw)?)],
        None => diary::list_agents(store).map_err(map_knowledge_error)?,
    };

    // (Datum, Zeit, Agent, Trigger, Text) aller Treffer.
    let mut hits: Vec<(String, String, String, &'static str, String)> = Vec::new();
    for agent in &agents {
        for date in diary::list_days(store, agent).map_err(map_knowledge_error)? {
            if let Some((from, to)) = &range
                && (date < *from || date > *to)
            {
                continue;
            }
            let Some(day) =
                diary::read_day_entries(store, agent, &date).map_err(map_knowledge_error)?
            else {
                continue;
            };
            if !caller.can_read(&day.visibility) {
                continue;
            }
            for entry in day.entries {
                if entry.text.to_lowercase().contains(&needle) {
                    hits.push((
                        date.clone(),
                        entry.recorded_at.strftime("%H:%M:%S").to_string(),
                        agent.as_str().to_owned(),
                        entry.trigger.label(),
                        entry.text,
                    ));
                }
            }
        }
    }
    // Jüngste zuerst, stabil nach Agent.
    hits.sort_by(|a, b| (&b.0, &b.1, &a.2).cmp(&(&a.0, &a.1, &b.2)));
    let truncated = hits.len() > MAX_SEARCH_HITS;
    hits.truncate(MAX_SEARCH_HITS);

    let data = serde_json::json!({
        "query": query,
        "truncated": truncated,
        "hits": hits
            .iter()
            .map(|(date, time, agent, trigger, text)| serde_json::json!({
                "agent": agent,
                "date": date,
                "time": time,
                "trigger": trigger,
                "text": text,
            }))
            .collect::<Vec<_>>(),
    });
    if hits.is_empty() {
        return Ok(OpOutput {
            text: format!("Keine sichtbaren Tagebucheinträge mit „{query}“."),
            data: Some(data),
        });
    }
    let mut text = format!("{} Treffer für „{query}“", hits.len());
    if truncated {
        text.push_str(&format!(" (auf {MAX_SEARCH_HITS} gekürzt)"));
    }
    text.push(':');
    for (date, time, agent, trigger, entry) in &hits {
        text.push_str(&format!(
            "\n- {agent} {date} {time} [{trigger}] {}",
            snippet(entry)
        ));
    }
    Ok(OpOutput {
        text,
        data: Some(data),
    })
}

/// Erste nicht leere Zeile, auf [`SNIPPET_CHARS`] Zeichen gekürzt.
fn snippet(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().count() <= SNIPPET_CHARS {
        return line.to_owned();
    }
    let mut short: String = line.chars().take(SNIPPET_CHARS).collect();
    short.push('…');
    short
}

/// Einträge eines Tages als JSON (`{"time","trigger","text"}`).
fn entries_data(day: &DiaryDay) -> Vec<serde_json::Value> {
    day.entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "time": entry.recorded_at.strftime("%H:%M:%S").to_string(),
                "trigger": entry.trigger.label(),
                "text": entry.text,
            })
        })
        .collect()
}

/// Nutzlast von `show`/`today` für die TUI:
/// `{"agent","date","entries":[{"time","trigger","text"}]}`.
fn day_data(agent: &AgentId, date: &str, day: Option<&DiaryDay>) -> serde_json::Value {
    let entries = day.map(entries_data).unwrap_or_default();
    serde_json::json!({ "agent": agent.as_str(), "date": date, "entries": entries })
}

fn note(
    store: &KnowledgeStore,
    caller: &AgentId,
    text: &str,
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(OpError::InvalidArguments(
            "Aufruf: /diary note <text>".to_owned(),
        ));
    }
    if text.len() > MAX_DIARY_NOTE_BYTES {
        return Err(OpError::InvalidArguments(format!(
            "Tagebucheintrag ist länger als {MAX_DIARY_NOTE_BYTES} Bytes"
        )));
    }
    validate_agent(caller.as_str())?;
    let entry = DiaryEntry {
        agent_id: caller.clone(),
        recorded_at: now,
        trigger: DiaryTrigger::Manual,
        visibility: VisibilityScope::OperatorOnly,
        body: text.to_owned(),
    };
    let artifact = diary::append(store, &entry).map_err(map_knowledge_error)?;
    let count = artifact
        .frontmatter
        .extra
        .get("entry_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Ok(OpOutput::from(format!(
        "Tagebucheintrag für {caller} am {} gespeichert ({count} Einträge heute).",
        now.strftime("%Y-%m-%d")
    )))
}

/// Verlangt `YYYY-MM-DD` aus Ziffern (verhindert zugleich Pfad-Escapes).
fn validate_date(raw: &str) -> Result<String, OpError> {
    let well_formed = raw.len() == 10
        && raw.char_indices().all(|(index, c)| match index {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        });
    if !well_formed {
        return Err(OpError::InvalidArguments(format!(
            "ungültiges Datum '{raw}' (YYYY-MM-DD)"
        )));
    }
    Ok(raw.to_owned())
}

/// Verlangt eine sichere einzelne Pfadkomponente als Agent-Id.
fn validate_agent(raw: &str) -> Result<&str, OpError> {
    let unsafe_id = raw.is_empty()
        || raw == "."
        || raw == ".."
        || raw.chars().any(|c| c == '/' || c == '\\' || c.is_control());
    if unsafe_id {
        return Err(OpError::InvalidArguments(format!(
            "ungültige Agent-Id '{raw}'"
        )));
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::run_diary;
    use crate::knowledge_test_support::temporary_store;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_knowledge::AgentId;
    use harw_operations::operation::Surface;
    use harw_operations::{OpError, Operation};

    fn noon() -> TestResult<jiff::Timestamp> {
        jiff::Timestamp::from_second(1_758_715_200).map_err(ctx("valid timestamp"))
    }

    #[test]
    fn diary_is_command_only() {
        let surfaces = &super::DiaryOperation.meta().surfaces;
        assert!(
            !surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
    }

    #[test]
    fn note_then_show_round_trips_for_the_caller() -> TestResult {
        let store = temporary_store("round-trip")?;
        let me = AgentId::new("operator");
        let now = noon()?;
        let written = run_diary(&store, &me, &toks(&["note", "Build", "grün"]), now)
            .map_err(ctx("note succeeds"))?;
        assert!(written.text.contains("1 Einträge"), "{}", written.text);

        let shown = run_diary(&store, &me, &toks(&[]), now).map_err(ctx("show succeeds"))?;
        assert!(shown.text.contains("Build grün"), "{}", shown.text);
        assert!(shown.text.contains("manual"), "{}", shown.text);

        let date_flag = format!("--date={}", now.strftime("%Y-%m-%d"));
        let explicit = run_diary(
            &store,
            &AgentId::new("someone-else"),
            &toks(&["show", "operator", date_flag.as_str()]),
            now,
        )
        .map_err(ctx("explicit show succeeds"))?;
        assert!(explicit.text.contains("Build grün"));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn today_carries_structured_entries_for_the_tui() -> TestResult {
        let store = temporary_store("today")?;
        let me = AgentId::new("operator");
        let now = noon()?;
        run_diary(&store, &me, &toks(&["note", "erster"]), now).map_err(ctx("note 1"))?;
        run_diary(&store, &me, &toks(&["note", "zweiter"]), now).map_err(ctx("note 2"))?;
        let output = run_diary(&store, &me, &toks(&["today"]), now).map_err(ctx("today"))?;
        let data = output.data.ok_or(TestError::Missing("today data"))?;
        assert_eq!(data["entries"][0]["trigger"], "manual");
        assert_eq!(data["entries"][0]["text"], "erster");
        assert_eq!(data["entries"][1]["text"], "zweiter");
        assert_eq!(data["entries"][0]["time"], "12:00:00");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn a_missing_day_is_reported_not_an_error() -> TestResult {
        let store = temporary_store("missing")?;
        let output = run_diary(
            &store,
            &AgentId::new("operator"),
            &toks(&["--date=2020-01-01"]),
            noon()?,
        )
        .map_err(ctx("show of a missing day succeeds"))?;
        assert!(output.text.starts_with("Kein sichtbarer Tagebucheintrag"));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn path_escapes_and_bad_dates_are_refused() -> TestResult {
        let store = temporary_store("escape")?;
        let me = AgentId::new("operator");
        for tokens in [
            vec!["show", "../etc"],
            vec!["show", "--date=../../x"],
            vec!["show", "--date=2020-1-01"],
            vec!["note"],
        ] {
            match run_diary(&store, &me, &toks(&tokens), noon()?) {
                Err(OpError::InvalidArguments(_)) => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?} must be InvalidArguments, got {other:?}"
                    )));
                }
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    /// Ein Agenten-Aufrufer (Sicht `SelfOnly`) sieht den `OperatorOnly`-Tag
    /// des Operators nicht — fail-closed, wie eine fehlende Datei.
    #[test]
    fn an_agent_viewer_does_not_see_the_operator_diary() -> TestResult {
        use crate::knowledge_common::KnowledgeCaller;
        use harw_knowledge::VisibilityScope;

        let store = temporary_store("diary-viewer")?;
        let now = noon()?;
        run_diary(
            &store,
            &AgentId::new("operator"),
            &toks(&["note", "geheim"]),
            now,
        )
        .map_err(ctx("note"))?;
        let agent = KnowledgeCaller {
            agent: AgentId::new("root-explorer"),
            viewer: VisibilityScope::SelfOnly,
        };
        let shown = super::run_diary_as(&store, &agent, &toks(&["show", "operator"]), now)
            .map_err(ctx("show as agent"))?;
        assert!(shown.text.starts_with("Kein sichtbarer"), "{}", shown.text);
        assert!(!shown.text.contains("geheim"));
        let data = shown.data.ok_or(TestError::Missing("data"))?;
        assert_eq!(data["entries"].as_array().map(Vec::len), Some(0));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn agents_lists_diaries_for_the_operator_and_only_self_for_agents() -> TestResult {
        use crate::knowledge_common::KnowledgeCaller;
        use harw_knowledge::VisibilityScope;

        let store = temporary_store("diary-agents")?;
        let now = noon()?;
        let empty = run_diary(&store, &AgentId::new("operator"), &toks(&["agents"]), now)
            .map_err(ctx("agents on empty store"))?;
        let data = empty.data.ok_or(TestError::Missing("empty agents data"))?;
        assert_eq!(data["agents"].as_array().map(Vec::len), Some(0));

        for agent in ["operator", "explorer"] {
            run_diary(&store, &AgentId::new(agent), &toks(&["note", "hallo"]), now)
                .map_err(ctx("note"))?;
        }
        let listed = run_diary(&store, &AgentId::new("operator"), &toks(&["agents"]), now)
            .map_err(ctx("agents as operator"))?;
        let data = listed.data.ok_or(TestError::Missing("agents data"))?;
        assert_eq!(data["agents"], serde_json::json!(["explorer", "operator"]));

        let agent = KnowledgeCaller {
            agent: AgentId::new("explorer"),
            viewer: VisibilityScope::SelfOnly,
        };
        let own = super::run_diary_as(&store, &agent, &toks(&["agents"]), now)
            .map_err(ctx("agents as agent"))?;
        let data = own.data.ok_or(TestError::Missing("own agents data"))?;
        assert_eq!(data["agents"], serde_json::json!(["explorer"]));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    fn days_before(now: jiff::Timestamp, days: i64) -> TestResult<jiff::Timestamp> {
        now.checked_sub(jiff::SignedDuration::from_secs(days * 86_400))
            .map_err(ctx("timestamp in range"))
    }

    #[test]
    fn show_range_lists_every_day_in_the_window() -> TestResult {
        let store = temporary_store("range")?;
        let me = AgentId::new("operator");
        let now = noon()?;
        for (days, text) in [(3, "vor drei"), (1, "gestern"), (0, "heute")] {
            run_diary(&store, &me, &toks(&["note", text]), days_before(now, days)?)
                .map_err(ctx("note"))?;
        }
        let output = run_diary(
            &store,
            &me,
            &toks(&["show", "--from", "2025-09-22", "--to=2025-09-24"]),
            now,
        )
        .map_err(ctx("range"))?;
        assert!(output.text.contains("gestern"), "{}", output.text);
        assert!(output.text.contains("heute"));
        assert!(!output.text.contains("vor drei"));
        let data = output.data.ok_or(TestError::Missing("range data"))?;
        assert_eq!(data["days"].as_array().map(Vec::len), Some(2));
        assert_eq!(data["days"][0]["date"], "2025-09-23");
        assert_eq!(data["days"][1]["entries"][0]["text"], "heute");

        // `--from` allein: bis heute.
        let open_end =
            run_diary(&store, &me, &toks(&["--from=2025-09-21"]), now).map_err(ctx("open end"))?;
        assert!(open_end.text.contains("vor drei"));

        for tokens in [
            vec!["show", "--date=2025-09-24", "--from=2025-09-23"],
            vec!["show", "--from=2025-09-25", "--to=2025-09-24"],
            vec!["show", "--from=2024-01-01", "--to=2025-09-24"],
            vec!["show", "--from=2025-02-30"],
        ] {
            match run_diary(&store, &me, &toks(&tokens), now) {
                Err(OpError::InvalidArguments(_)) => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{tokens:?} must be InvalidArguments, got {other:?}"
                    )));
                }
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn search_finds_visible_entries_across_agents_newest_first() -> TestResult {
        use crate::knowledge_common::KnowledgeCaller;
        use harw_knowledge::VisibilityScope;
        use harw_knowledge::diary::{self, DiaryTrigger};

        let store = temporary_store("search")?;
        let now = noon()?;
        run_diary(
            &store,
            &AgentId::new("operator"),
            &toks(&["note", "Parser", "kaputt"]),
            days_before(now, 2)?,
        )
        .map_err(ctx("note"))?;
        diary::record(
            &store,
            &AgentId::new("explorer"),
            DiaryTrigger::Compaction,
            "Der PARSER läuft wieder.",
            now,
        )
        .map_err(ctx("record"))?;
        diary::record(
            &store,
            &AgentId::new("explorer"),
            DiaryTrigger::Manual,
            "anderes Thema",
            now,
        )
        .map_err(ctx("record other"))?;

        let found = run_diary(
            &store,
            &AgentId::new("operator"),
            &toks(&["search", "parser"]),
            now,
        )
        .map_err(ctx("search"))?;
        let data = found.data.ok_or(TestError::Missing("search data"))?;
        let hits = data["hits"].as_array().ok_or(TestError::Missing("hits"))?;
        assert_eq!(hits.len(), 2, "{}", found.text);
        assert_eq!(hits[0]["agent"], "explorer");
        assert_eq!(hits[0]["trigger"], "compaction");
        assert_eq!(hits[1]["agent"], "operator");

        let scoped = run_diary(
            &store,
            &AgentId::new("operator"),
            &toks(&["search", "parser", "--agent=operator"]),
            now,
        )
        .map_err(ctx("scoped search"))?;
        assert!(scoped.text.contains("1 Treffer"), "{}", scoped.text);

        let windowed = run_diary(
            &store,
            &AgentId::new("operator"),
            &toks(&["search", "parser", "--from=2025-09-24"]),
            now,
        )
        .map_err(ctx("windowed search"))?;
        assert!(windowed.text.contains("1 Treffer"), "{}", windowed.text);

        // Ein Agent sieht die OperatorOnly-Tage fail-closed nicht.
        let agent = KnowledgeCaller {
            agent: AgentId::new("explorer"),
            viewer: VisibilityScope::SelfOnly,
        };
        let hidden = super::run_diary_as(&store, &agent, &toks(&["search", "parser"]), now)
            .map_err(ctx("agent search"))?;
        assert!(
            hidden.text.starts_with("Keine sichtbaren"),
            "{}",
            hidden.text
        );

        match run_diary(&store, &AgentId::new("operator"), &toks(&["search"]), now) {
            Err(OpError::InvalidArguments(_)) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "empty search must fail: {other:?}"
                )));
            }
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn search_is_capped() -> TestResult {
        use harw_knowledge::diary::{self, DiaryTrigger};

        let store = temporary_store("search-cap")?;
        let now = noon()?;
        let me = AgentId::new("explorer");
        for index in 0..(super::MAX_SEARCH_HITS + 3) {
            let at = now
                .checked_add(jiff::SignedDuration::from_secs(
                    i64::try_from(index).unwrap_or(0),
                ))
                .map_err(ctx("timestamp in range"))?;
            diary::record(
                &store,
                &me,
                DiaryTrigger::Manual,
                &format!("treffer {index}"),
                at,
            )
            .map_err(ctx("record"))?;
        }
        let output = run_diary(
            &store,
            &AgentId::new("operator"),
            &toks(&["search", "treffer"]),
            now,
        )
        .map_err(ctx("search"))?;
        let data = output.data.ok_or(TestError::Missing("data"))?;
        assert_eq!(data["truncated"], true);
        assert_eq!(
            data["hits"].as_array().map(Vec::len),
            Some(super::MAX_SEARCH_HITS)
        );
        assert_eq!(data["hits"][0]["text"], "treffer 52");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }
}
