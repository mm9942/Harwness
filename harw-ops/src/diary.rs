//! `/diary` — das Tagebuch je Agent (`docs/design/knowledge-surfaces.md` §3,
//! `interaction-contract.md` §2.2).
//!
//! # Subcommands
//! - (bare) / `show [AgentRef] [--date=YYYY-MM-DD]` — rendert einen Tag;
//!   ohne Angaben das eigene Tagebuch von heute (UTC).
//! - `today` — Kurzform für das eigene Tagebuch von heute.
//!
//! `show`/`today` liefern zusätzlich `OpOutput::data` =
//! `{"agent","date","entries":[{"time","trigger","text"}]}` für die TUI.
//! - `note <text>` — außerplanmäßiger Eintrag (`DiaryTrigger::Manual`) im
//!   eigenen Tagebuch. Das `#`-Präfix der TUI ist Zucker für genau diesen
//!   Pfad (ein Codepfad, eine Audit-Spur).
//!
//! # Sichtbarkeit
//! `show` liest über `harw_knowledge::diary::read_day` und rendert eine
//! Tagesdatei nur, wenn ihre Sichtbarkeit für einen `OperatorOnly`-Aufrufer
//! gilt (`VisibilityScope::visible_to_caller`) — dieselbe fail-closed-Regel
//! wie der Recall-Pfad. Eine unsichtbare Datei wird wie eine fehlende
//! gemeldet (kein Informationsleck). `note` schreibt mit
//! `VisibilityScope::OperatorOnly`, damit der eigene Eintrag wieder lesbar ist.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Knowledge-Store im Kontext.
//! - [`OpError::InvalidArguments`] — Grammatik, ungültiges Datum/Agent, leerer Text.
//! - [`OpError::Execution`] — Ein-/Ausgabefehler des Speichers.

use harw_knowledge::diary::{self, DiaryEntry, DiaryTrigger};
use harw_knowledge::{AgentId, KnowledgeStore, VisibilityScope};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

use crate::workbench::{caller_agent, knowledge_store, map_knowledge_error, split_flag};

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
    summary = "Tagebuch: show [agent] [--date=YYYY-MM-DD], note <text>.",
    domain = "knowledge",
    permission = "operator",
    command(path = "/diary", visibility = "channel_reduced")
)]
async fn diary(ctx: &OpContext, args: DiaryArgs) -> Result<OpOutput, OpError> {
    let store = knowledge_store(ctx)?;
    run_diary(
        &store,
        &caller_agent(ctx),
        &args.tokens,
        jiff::Timestamp::now(),
    )
}

/// Der reine Kern von `/diary` (testbar ohne `OpContext`).
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_diary(
    store: &KnowledgeStore,
    caller: &AgentId,
    tokens: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    match tokens.first().map(String::as_str) {
        None => show(store, caller, &[], now),
        Some("show") => show(store, caller, tokens.get(1..).unwrap_or_default(), now),
        Some("today") => show(store, caller, &[], now),
        Some("note") => note(
            store,
            caller,
            &tokens.get(1..).unwrap_or_default().join(" "),
            now,
        ),
        // `/diary <agent>` ist Kurzform von `/diary show <agent>`.
        Some(_) => show(store, caller, tokens, now),
    }
}

fn show(
    store: &KnowledgeStore,
    caller: &AgentId,
    tokens: &[String],
    now: jiff::Timestamp,
) -> Result<OpOutput, OpError> {
    let (date_flag, rest) = split_flag(tokens, "--date=");
    let date = match date_flag {
        Some(date) => validate_date(&date)?,
        None => now.strftime("%Y-%m-%d").to_string(),
    };
    if rest.len() > 1 {
        return Err(OpError::InvalidArguments(
            "Aufruf: /diary show [agent] [--date=YYYY-MM-DD]".to_owned(),
        ));
    }
    let agent = match rest.first() {
        Some(raw) => AgentId::new(validate_agent(raw)?),
        None => caller.clone(),
    };
    let day = diary::read_day(store, &agent, &date).map_err(map_knowledge_error)?;
    let visible = day.filter(|artifact| {
        artifact
            .frontmatter
            .visibility
            .visible_to_caller(&VisibilityScope::OperatorOnly)
    });
    let Some(artifact) = visible else {
        return Ok(OpOutput {
            text: format!("Kein sichtbarer Tagebucheintrag für {agent} am {date}."),
            data: Some(day_data(&agent, &date, "")),
        });
    };
    Ok(OpOutput {
        text: format!("Tagebuch {agent} — {date}\n\n{}", artifact.body.trim_end()),
        data: Some(day_data(&agent, &date, &artifact.body)),
    })
}

/// Zerlegt einen Tages-Body in `### HH:MM:SS — <trigger>`-Einträge.
///
/// # Rückgabe
/// `(time, trigger, text)` je Eintrag in Dateireihenfolge.
fn parse_entries(body: &str) -> Vec<(String, String, String)> {
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for line in body.lines() {
        let header = line
            .strip_prefix("### ")
            .and_then(|rest| rest.split_once(" — "));
        match header {
            Some((time, trigger)) => {
                entries.push((
                    time.trim().to_owned(),
                    trigger.trim().to_owned(),
                    String::new(),
                ));
            }
            None => {
                if let Some((_, _, text)) = entries.last_mut() {
                    if !text.is_empty() || !line.trim().is_empty() {
                        text.push_str(line);
                        text.push('\n');
                    }
                }
            }
        }
    }
    for (_, _, text) in &mut entries {
        let trimmed = text.trim_end().to_owned();
        *text = trimmed;
    }
    entries
}

/// Nutzlast von `show`/`today` für die TUI.
fn day_data(agent: &AgentId, date: &str, body: &str) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = parse_entries(body)
        .into_iter()
        .map(|(time, trigger, text)| {
            serde_json::json!({ "time": time, "trigger": trigger, "text": text })
        })
        .collect();
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
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_knowledge::{AgentId, KnowledgeStore};
    use harw_operations::operation::Surface;
    use harw_operations::{OpError, Operation};

    fn temporary_store(label: &str) -> TestResult<KnowledgeStore> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-ops-diary-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("create temporary knowledge root"))?;
        Ok(KnowledgeStore::new(&root))
    }

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
}
