//! `/dream` — die Traumberichte des Wissensspeichers
//! (`docs/design/knowledge-surfaces.md` §4, `interaction-contract.md` §2.2).
//!
//! # Subcommands
//! - (bare) / `list` — alle sichtbaren Traumberichte, neueste zuerst;
//!   `OpOutput::data` = `{"reports":[{"id","date","proposals"}]}`.
//!   `proposals` ist die Liste der Vorschläge des Berichts
//!   (`[{"section","target","summary"}]`); die TUI zeigt ihre Anzahl.
//! - `show <id>` — ein Bericht; `OpOutput::data` =
//!   `{"report":{"id","date","body","proposals"}}`. `<id>` darf die volle
//!   Artefakt-Id (`dream/<YYYY-MM-DD>/<work-id>`), `<YYYY-MM-DD>/<work-id>`
//!   oder die bloße Work-Id sein (muss dann eindeutig sein).
//!
//! Ein `run` gibt es bewusst nicht: `harw-knowledge` bietet keinen sicheren
//! Auslöser für einen Traum-Job (der Job lebt in `harw-job-runtime`, sein
//! Bericht wird nur über `KnowledgeStore::write_dream_report` geschrieben).
//!
//! # Lesepfad und Sichtbarkeit
//! `harw-knowledge` hat keine eigene Listen-API für Traumberichte; gelesen
//! wird wie bei `/palace` über `KnowledgeIndex::rebuild` (der die
//! `dreams/`-Fläche als `ArtifactKind::DreamReport` mit Id
//! `dream/<datum>/<work-id>` einliest) und
//! `harw_knowledge::memory::recall::search_with` mit
//! `VisibilityScope::OperatorOnly` — dieselbe fail-closed-Disziplin wie
//! `/palace`. Kein direkter Dateizugriff am Store vorbei.
//!
//! # Fläche
//! Nur `Surface::Command` (`channel_reduced`), kein Modell-Werkzeug:
//! Traumberichte sind review-gated und für den Operator bestimmt.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Knowledge-Store im Kontext.
//! - [`OpError::InvalidArguments`] — Grammatik, unbekannte/mehrdeutige Id.
//! - [`OpError::Execution`] — Index-/Ein-/Ausgabefehler.

use harw_knowledge::artifact::{ArtifactKind, KnowledgeArtifact, RecallQuery};
use harw_knowledge::index::KnowledgeIndex;
use harw_knowledge::memory::recall::{ListAllRanker, search_with};
use harw_knowledge::{KnowledgeStore, VisibilityScope};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

use crate::workbench::{knowledge_store, map_knowledge_error};

/// Id-Präfix, unter dem der Index Traumberichte führt.
const DREAM_ID_PREFIX: &str = "dream/";

/// Überschriften-Präfix der Vorschlagsabschnitte im Berichts-Body
/// (`harw_knowledge::dream::DreamReport::render_body`).
const PROPOSAL_HEADING_PREFIX: &str = "## Vorgeschlagene ";

/// Argument-Container für `/dream`: rohe Tokens.
#[derive(Debug, Default, serde::Deserialize)]
pub struct DreamArgs {
    /// Alle Tokens nach `/dream`.
    #[serde(default)]
    pub tokens: Vec<String>,
}

impl harw_operations::FromRawArgs for DreamArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            tokens: tokens.to_vec(),
        })
    }
}

/// Führt `/dream` aus.
///
/// # Fehler
/// Siehe Moduldoku.
#[operation(
    name = "dream",
    summary = "Traumberichte: list, show <id>.",
    domain = "knowledge",
    permission = "operator",
    command(path = "/dream", visibility = "channel_reduced")
)]
async fn dream(ctx: &OpContext, args: DreamArgs) -> Result<OpOutput, OpError> {
    let store = knowledge_store(ctx)?;
    run_dream(&store, &args.tokens)
}

/// Der reine Kern von `/dream` (testbar ohne `OpContext`).
///
/// # Fehler
/// Siehe Moduldoku.
pub fn run_dream(store: &KnowledgeStore, tokens: &[String]) -> Result<OpOutput, OpError> {
    let tail = tokens.get(1..).unwrap_or_default();
    match tokens.first().map(String::as_str) {
        None | Some("list") => list(store),
        Some("show") => show(store, tail),
        Some(other) => Err(OpError::InvalidArguments(format!(
            "unbekannter /dream-Subcommand: {other} (list, show)"
        ))),
    }
}

/// Alle für einen Operator sichtbaren Traumberichte, neueste zuerst.
///
/// # Rückgabe
/// `(berichte, gekürzt)`; `gekürzt` ist `true`, wenn die Recall-Obergrenze
/// gegriffen hat.
fn visible_reports(store: &KnowledgeStore) -> Result<(Vec<KnowledgeArtifact>, bool), OpError> {
    let index = KnowledgeIndex::rebuild(store).map_err(|error| {
        OpError::Execution(format!("Knowledge-Index-Aufbau fehlgeschlagen: {error}"))
    })?;
    let mut query = RecallQuery::new(String::new(), VisibilityScope::OperatorOnly);
    query.kinds = vec![ArtifactKind::DreamReport];
    query.max_hops = 0;
    let result = search_with(&index, &query, &ListAllRanker).map_err(map_knowledge_error)?;
    let mut reports: Vec<KnowledgeArtifact> = result
        .hits
        .iter()
        .filter_map(|hit| index.get(&hit.artifact.id).cloned())
        .collect();
    reports.sort_by(|left, right| {
        report_date(right)
            .cmp(&report_date(left))
            .then_with(|| right.id.as_str().cmp(left.id.as_str()))
    });
    Ok((reports, result.truncated))
}

/// Datum eines Berichts: das Datumssegment der Id, sonst `created_at` (UTC).
fn report_date(report: &KnowledgeArtifact) -> String {
    report
        .id
        .as_str()
        .strip_prefix(DREAM_ID_PREFIX)
        .and_then(|rest| rest.split_once('/'))
        .map(|(date, _)| date.to_owned())
        .unwrap_or_else(|| {
            report
                .frontmatter
                .created_at
                .strftime("%Y-%m-%d")
                .to_string()
        })
}

/// Die Vorschläge eines Berichts aus seinen `## Vorgeschlagene …`-Abschnitten.
///
/// # Rückgabe
/// `[{"section","target","summary"}]` in Body-Reihenfolge; `section` ist der
/// Rest der Überschrift (etwa `Topic-Updates`).
fn proposals(body: &str) -> Vec<serde_json::Value> {
    let mut section: Option<&str> = None;
    let mut found = Vec::new();
    for line in body.lines() {
        if line.starts_with("## ") {
            // Jede Abschnittsüberschrift beendet den vorigen Abschnitt; nur
            // Vorschlagsabschnitte öffnen einen neuen.
            section = line.strip_prefix(PROPOSAL_HEADING_PREFIX).map(str::trim);
            continue;
        }
        let Some(current) = section else {
            continue;
        };
        let Some(entry) = line.strip_prefix("- `") else {
            continue;
        };
        let (target, summary) = match entry.split_once("`:") {
            Some((target, summary)) => (target.trim(), summary.trim()),
            None => (entry.trim_end_matches('`').trim(), ""),
        };
        found.push(serde_json::json!({
            "section": current,
            "target": target,
            "summary": summary,
        }));
    }
    found
}

/// Kopf-Nutzlast eines Berichts (ohne Body).
fn report_summary(report: &KnowledgeArtifact) -> serde_json::Value {
    serde_json::json!({
        "id": report.id.as_str(),
        "date": report_date(report),
        "proposals": proposals(&report.body),
    })
}

fn list(store: &KnowledgeStore) -> Result<OpOutput, OpError> {
    let (reports, truncated) = visible_reports(store)?;
    let summaries: Vec<serde_json::Value> = reports.iter().map(report_summary).collect();
    let data = serde_json::json!({ "reports": summaries });
    if reports.is_empty() {
        return Ok(OpOutput {
            text: "Keine sichtbaren Traumberichte.".to_owned(),
            data: Some(data),
        });
    }
    let mut out = format!("{} Traumberichte:\n", reports.len());
    for report in &reports {
        out.push_str(&format!(
            "· {} — {} ({} Vorschläge)\n",
            report_date(report),
            report.id,
            proposals(&report.body).len()
        ));
    }
    if truncated {
        out.push_str("(gekürzt — ältere Berichte nicht aufgeführt)\n");
    }
    Ok(OpOutput {
        text: out,
        data: Some(data),
    })
}

/// Löst eine `show`-Referenz gegen die sichtbaren Berichte auf.
///
/// # Fehler
/// [`OpError::InvalidArguments`] bei unbekannter oder mehrdeutiger Id.
fn resolve(reports: Vec<KnowledgeArtifact>, raw: &str) -> Result<KnowledgeArtifact, OpError> {
    let raw = raw.trim();
    let exact = if raw.starts_with(DREAM_ID_PREFIX) {
        Some(raw.to_owned())
    } else if raw.contains('/') {
        Some(format!("{DREAM_ID_PREFIX}{raw}"))
    } else {
        None
    };
    let suffix = format!("/{raw}");
    let mut matches: Vec<KnowledgeArtifact> = reports
        .into_iter()
        .filter(|report| match &exact {
            Some(id) => report.id.as_str() == id,
            None => report.id.as_str().ends_with(&suffix),
        })
        .collect();
    match matches.len() {
        0 => Err(OpError::InvalidArguments(format!(
            "kein sichtbarer Traumbericht '{raw}'"
        ))),
        1 => matches.pop().ok_or_else(|| {
            OpError::InvalidArguments(format!("kein sichtbarer Traumbericht '{raw}'"))
        }),
        _ => {
            let ids: Vec<&str> = matches.iter().map(|report| report.id.as_str()).collect();
            Err(OpError::InvalidArguments(format!(
                "Traumbericht '{raw}' ist mehrdeutig: {}",
                ids.join(", ")
            )))
        }
    }
}

fn show(store: &KnowledgeStore, tail: &[String]) -> Result<OpOutput, OpError> {
    let raw = tail
        .first()
        .ok_or_else(|| OpError::InvalidArguments("Aufruf: /dream show <id>".to_owned()))?;
    let (reports, _) = visible_reports(store)?;
    let report = resolve(reports, raw)?;
    let date = report_date(&report);
    let body = report.body.trim();
    let mut data = report_summary(&report);
    if let Some(fields) = data.as_object_mut() {
        fields.insert("body".to_owned(), serde_json::json!(body));
    }
    Ok(OpOutput {
        text: format!("Traumbericht {date} — {}\n\n{body}\n", report.id),
        data: Some(serde_json::json!({ "report": data })),
    })
}

#[cfg(test)]
mod tests {
    use super::{DreamArgs, proposals, run_dream};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_job_runtime::WorkId;
    use harw_knowledge::KnowledgeStore;
    use harw_knowledge::artifact::ArtifactId;
    use harw_knowledge::dream::{DreamProposal, DreamReport};
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, OpError, Operation};

    fn temporary_store(label: &str) -> TestResult<KnowledgeStore> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-ops-dream-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("create temporary knowledge root"))?;
        Ok(KnowledgeStore::new(&root))
    }

    fn report(work_id: &str, second: i64, topics: usize) -> TestResult<DreamReport> {
        let created_at = jiff::Timestamp::from_second(second).map_err(ctx("valid timestamp"))?;
        Ok(DreamReport {
            work_id: WorkId::from_str(work_id),
            created_at,
            summary: "Zusammenfassung".to_owned(),
            proposed_topic_updates: (0..topics)
                .map(|index| DreamProposal {
                    artifact_id: ArtifactId::new(format!("topic/t{index}")),
                    summary: format!("Thema {index} ergänzen"),
                })
                .collect(),
            proposed_palace_promotions: vec![DreamProposal {
                artifact_id: ArtifactId::new("palace/p"),
                summary: "Knoten anlegen".to_owned(),
            }],
            follow_ups: vec!["- `kein/vorschlag`: nur ein Faden".to_owned()],
        })
    }

    fn run(store: &KnowledgeStore, tokens: &[&str]) -> Result<harw_operations::OpOutput, OpError> {
        run_dream(store, &toks(tokens))
    }

    #[test]
    fn dream_is_a_command_without_a_model_tool_surface() {
        let meta = super::DreamOperation.meta();
        assert_eq!(meta.name, "dream");
        assert_eq!(meta.permission, harw_operations::PermissionTier::Operator);
        assert!(
            !meta
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
        assert!(meta.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::Command {
                path: "/dream",
                visibility: CommandVisibility::ChannelReduced,
            }
        )));
    }

    #[test]
    fn from_raw_args_keeps_every_token() -> TestResult {
        let args = DreamArgs::from_raw_args(&toks(&["show", "x"])).map_err(ctx("parse"))?;
        assert_eq!(args.tokens, toks(&["show", "x"]));
        Ok(())
    }

    #[test]
    fn an_empty_store_lists_no_reports() -> TestResult {
        let store = temporary_store("empty")?;
        let output = run(&store, &[]).map_err(ctx("bare succeeds"))?;
        assert_eq!(output.text, "Keine sichtbaren Traumberichte.");
        let data = output.data.ok_or(TestError::Missing("list data"))?;
        assert_eq!(data["reports"], serde_json::json!([]));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn list_carries_newest_first_with_proposals() -> TestResult {
        let store = temporary_store("list")?;
        store
            .write_dream_report(&report("dream-old", 0, 1)?)
            .map_err(ctx("write old"))?;
        store
            .write_dream_report(&report("dream-new", 1_758_715_200, 2)?)
            .map_err(ctx("write new"))?;

        let output = run(&store, &["list"]).map_err(ctx("list succeeds"))?;
        let data = output.data.ok_or(TestError::Missing("list data"))?;
        assert_eq!(data["reports"][0]["id"], "dream/2025-09-24/dream-new");
        assert_eq!(data["reports"][0]["date"], "2025-09-24");
        let newest = data["reports"][0]["proposals"]
            .as_array()
            .ok_or(TestError::Missing("proposals array"))?;
        assert_eq!(newest.len(), 3, "{data}");
        assert_eq!(newest[0]["target"], "topic/t0");
        assert_eq!(newest[0]["summary"], "Thema 0 ergänzen");
        assert_eq!(newest[2]["section"], "Palace-Promotionen");
        assert_eq!(data["reports"][1]["id"], "dream/1970-01-01/dream-old");
        assert!(
            output.text.starts_with("2 Traumberichte:"),
            "{}",
            output.text
        );
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn show_resolves_full_dated_and_bare_ids() -> TestResult {
        let store = temporary_store("show")?;
        store
            .write_dream_report(&report("dream-a", 0, 1)?)
            .map_err(ctx("write"))?;
        for reference in ["dream/1970-01-01/dream-a", "1970-01-01/dream-a", "dream-a"] {
            let output = run(&store, &["show", reference]).map_err(ctx("show succeeds"))?;
            let data = output.data.ok_or(TestError::Missing("show data"))?;
            assert_eq!(data["report"]["id"], "dream/1970-01-01/dream-a");
            assert_eq!(data["report"]["date"], "1970-01-01");
            let body = data["report"]["body"]
                .as_str()
                .ok_or(TestError::Missing("body"))?;
            assert!(body.contains("# Traumbericht dream-a"), "{body}");
            assert!(output.text.contains("Zusammenfassung"), "{}", output.text);
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn a_bare_id_shared_by_two_days_is_ambiguous() -> TestResult {
        let store = temporary_store("ambiguous")?;
        store
            .write_dream_report(&report("dream-x", 0, 0)?)
            .map_err(ctx("write day 1"))?;
        store
            .write_dream_report(&report("dream-x", 86_400, 0)?)
            .map_err(ctx("write day 2"))?;
        match run(&store, &["show", "dream-x"]) {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains("mehrdeutig"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "mehrdeutige Id muss InvalidArguments sein, war {other:?}"
                )));
            }
        }
        run(&store, &["show", "1970-01-02/dream-x"]).map_err(ctx("dated id is unique"))?;
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn grammar_errors_are_invalid_arguments() -> TestResult {
        let store = temporary_store("grammar")?;
        for tokens in [
            vec!["run"],
            vec!["show"],
            vec!["show", "unbekannt"],
            vec!["show", "../../etc/passwd"],
        ] {
            match run(&store, &tokens) {
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
    fn proposals_ignore_follow_ups_and_empty_sections() {
        let body = "## Vorgeschlagene Topic-Updates\n\n_Keine._\n\n\
                    ## Vorgeschlagene Palace-Promotionen\n\n- `palace/a`: A\n\n\
                    ## Offene Fäden\n\n- `x`: kein Vorschlag\n";
        let found = proposals(body);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["target"], "palace/a");
        assert_eq!(found[0]["summary"], "A");
    }
}
