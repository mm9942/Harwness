//! `/context-proposal` — Prüffläche für `ContextProposal` (Knoten AW5-09).
//!
//! # Verantwortungsbereich
//! Exponiert `harw_knowledge::context_proposal::ContextProposal` als
//! `/context-proposal`-Command (`channel_parity`): auflisten (`list`),
//! ansehen (`view <id>`), annehmen (`accept <id>`) oder ablehnen
//! (`reject <id>`). **„Annehmen" markiert nur** — es gibt in dieser Datei
//! keinen Aufruf, der ein Kontextprogramm liest, parst oder schreibt.
//! Genau wie `/memory` (siehe dortige Moduldoku) wird diese Operation
//! derzeit **nicht** als Model-Tool exponiert: derselbe Callback deckt auch
//! die mutierenden Subcommands `accept`/`reject` ab, und ein Vorschlag zur
//! Änderung der eigenen Steuerungsfläche ist eine Operator-Governance-Frage,
//! keine agentenseitig automatisierbare Aktion (siehe die Moduldoku von
//! `harw_knowledge::context_proposal`, Abschnitt „Gewählte Sichtbarkeit").
//!
//! # Wo Sichtbarkeit durchgesetzt wird
//! Diese Datei öffnet **keinen** zweiten, ungeprüften Lesepfad. `list`/`view`
//! laufen ausschließlich über `harw_knowledge::memory::recall::search_with`
//! (mit `harw_knowledge::memory::recall::ListAllRanker`, AW5-09 — die
//! `KeywordRanker`-Query mit leerem Text liefert sonst nichts, siehe deren
//! Moduldoku) und fragen **immer** mit
//! `VisibilityScope::OperatorOnly` — dieselbe Disziplin, die
//! `harw_knowledge::context_provider::KnowledgeContextProvider` bereits
//! durchsetzt: `KnowledgeIndex::get`/`iter`/`backlinks` werden nie mit einer
//! vom Aufrufer frei gewählten Id aufgerufen, nur mit einer Id, die
//! `search_with` bereits als sichtbar bestätigt hat (die eine dokumentierte
//! Ausnahme, um den vollen Body nachzuladen).
//!
//! # Subcommands
//! - `list` (Default) — listet alle sichtbaren Vorschläge mit Status.
//! - `view <id>` — zeigt einen Vorschlag vollständig (Programm, Fassung,
//!   Änderungen, Belege, Status).
//! - `accept <id>` — markiert den Vorschlag als `Accepted`. Ändert nur die
//!   Datei des Vorschlags selbst, kein Kontextprogramm.
//! - `reject <id>` — markiert den Vorschlag als `Rejected`.
//!
//! # Nebenläufigkeit
//! Die Operation selbst ist zustandslos; das Backend
//! (`Arc<harw_knowledge::KnowledgeStore>`) wird aus [`OpContext::service`]
//! aufgelöst. `KnowledgeIndex::rebuild` liest die Store-Dateien synchron bei
//! jedem Aufruf neu ein — dieselbe „immer frisch lesen"-Einfachheit wie
//! `PlanStore`-Konsumenten in dieser Crate.
//!
//! # Fehler
//! - [`OpError::NotAvailable`] — kein Knowledge-Store im Kontext registriert.
//! - [`OpError::InvalidArguments`] — fehlende/unbekannte Subcommand-Argumente,
//!   unbekannte oder unsichtbare Id.
//! - [`OpError::Execution`] — Backend-Fehler (I/O, Serde, Sichtbarkeitsgrenzen).

use std::sync::Arc;

use harw_knowledge::artifact::{ArtifactId, ArtifactKind, KnowledgeArtifact, RecallQuery};
use harw_knowledge::context_proposal::ContextProposal;
use harw_knowledge::index::KnowledgeIndex;
use harw_knowledge::memory::recall::{ListAllRanker, search_with};
use harw_knowledge::visibility::VisibilityScope;
use harw_knowledge::KnowledgeStore;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

/// Argument-Container für `/context-proposal`.
///
/// # Beschreibung
/// Positional wie `/memory`: `sub` ist das Subcommand, `tail` die restlichen
/// Tokens (bei `view`/`accept`/`reject` die Vorschlag-Id).
#[derive(Default, serde::Deserialize)]
pub struct ContextProposalArgs {
    /// Subcommand: `list`, `view`, `accept`, `reject`.
    #[serde(default)]
    pub sub: Option<String>,
    /// Weitere Tokens nach dem Subcommand (üblicherweise genau die Id).
    #[serde(default)]
    pub tail: Vec<String>,
}

impl harw_operations::FromRawArgs for ContextProposalArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, harw_operations::OpError> {
        Ok(Self {
            sub: tokens.first().cloned(),
            tail: tokens.iter().skip(1).cloned().collect(),
        })
    }
}

/// Führt den `/context-proposal`-Subcommand aus.
///
/// # Argumente
/// - `ctx` — Ausführungskontext; muss `Arc<harw_knowledge::KnowledgeStore>`
///   als Service anbieten.
/// - `args` — bereits geparst (Subcommand + Tail-Tokens).
///
/// # Rückgabe
/// `Ok(OpOutput { text })` mit menschenlesbarem Bericht.
///
/// # Fehler
/// Siehe Modul-Doku.
#[operation(
    name = "context-proposal",
    summary = "Kontextprogramm-Vorschläge prüfen: list, view, accept, reject. Wendet nie an.",
    domain = "knowledge",
    permission = "operator",
    command(path = "/context-proposal", visibility = "channel_parity")
)]
async fn context_proposal(
    ctx: &OpContext,
    args: ContextProposalArgs,
) -> Result<OpOutput, OpError> {
    let store = ctx
        .service::<Arc<KnowledgeStore>>()
        .cloned()
        .ok_or_else(|| OpError::NotAvailable("kein Knowledge-Store im Kontext".to_owned()))?;

    let sub = args.sub.as_deref().unwrap_or("list");
    match sub {
        "list" => render_list(&store),
        "view" => render_view(&store, &args.tail),
        "accept" => decide(&store, &args.tail, true),
        "reject" => decide(&store, &args.tail, false),
        other => Err(OpError::InvalidArguments(format!(
            "unbekannter /context-proposal-Subcommand: {other} (list, view, accept, reject)"
        ))),
    }
}

/// Baut die Recall-Anfrage, die jeder Subcommand dieser Datei verwendet: nur
/// `ArtifactKind::ContextProposal`, immer mit `VisibilityScope::OperatorOnly`
/// (siehe Moduldoku, „Wo Sichtbarkeit durchgesetzt wird").
fn proposals_query() -> RecallQuery {
    let mut query = RecallQuery::new(String::new(), VisibilityScope::OperatorOnly);
    query.kinds = vec![ArtifactKind::ContextProposal];
    query
}

/// Rebuilt den Index und liefert alle sichtbaren `ContextProposal`-Artefakte,
/// über `ListAllRanker` (nicht `KeywordRanker` — die Anfrage hat bewusst
/// leeren Text, siehe `ListAllRanker`s Moduldoku in `harw-knowledge`).
fn list_visible_artifacts(store: &KnowledgeStore) -> Result<Vec<KnowledgeArtifact>, OpError> {
    let index = KnowledgeIndex::rebuild(store)
        .map_err(|error| OpError::Execution(format!("Knowledge-Index-Aufbau fehlgeschlagen: {error}")))?;
    let query = proposals_query();
    let result = search_with(&index, &query, &ListAllRanker)
        .map_err(|error| OpError::Execution(format!("Recall fehlgeschlagen: {error}")))?;

    Ok(result
        .hits
        .iter()
        // Die eine dokumentierte Ausnahme (siehe Moduldoku): `index.get` wird
        // nur mit einer Id aufgerufen, die `search_with` bereits als
        // sichtbar bestätigt hat, nie mit einer vom Aufrufer frei gewählten.
        .filter_map(|hit| index.get(&hit.artifact.id).cloned())
        .collect())
}

/// Findet genau ein sichtbares `ContextProposal`-Artefakt nach Id.
///
/// # Errors
/// [`OpError::InvalidArguments`], wenn keine Id übergeben wurde oder kein
/// sichtbares Artefakt mit dieser Id existiert (nicht unterscheidbar von
/// „existiert, aber nicht sichtbar" — fail-closed, kein Informationsleck).
fn find_visible_artifact(store: &KnowledgeStore, tail: &[String]) -> Result<KnowledgeArtifact, OpError> {
    let id = tail.first().ok_or_else(|| {
        OpError::InvalidArguments("/context-proposal <view|accept|reject> <id>".to_owned())
    })?;
    let target = ArtifactId::new(id.as_str());
    list_visible_artifacts(store)?
        .into_iter()
        .find(|artifact| artifact.id == target)
        .ok_or_else(|| {
            OpError::InvalidArguments(format!(
                "kein sichtbarer Kontextprogramm-Vorschlag mit id '{id}'"
            ))
        })
}

/// Leitet die Datei-Slug-Id (ohne `context-proposal/`-Präfix) aus der vollen
/// Artefakt-Id ab, für [`harw_knowledge::KnowledgeStore::context_proposal_path`]
/// (dieselbe Slug-vs-präfigierte-Id-Konvention wie `palace_path`, siehe
/// dortige Moduldoku).
fn proposal_slug(id: &ArtifactId) -> ArtifactId {
    let raw = id.as_str();
    ArtifactId::new(raw.strip_prefix("context-proposal/").unwrap_or(raw))
}

fn render_list(store: &KnowledgeStore) -> Result<OpOutput, OpError> {
    let artifacts = list_visible_artifacts(store)?;
    if artifacts.is_empty() {
        return Ok(OpOutput {
            text: "Keine Kontextprogramm-Vorschläge.".to_owned(),
        });
    }

    let mut buf = format!("{} Kontextprogramm-Vorschlag/Vorschläge:\n", artifacts.len());
    for artifact in &artifacts {
        let status = ContextProposal::from_artifact(artifact)
            .map(|proposal| format!("{:?}", proposal.status))
            .unwrap_or_else(|_| "?".to_owned());
        buf.push_str(&format!("· {} [{status}]\n", artifact.id));
    }
    Ok(OpOutput { text: buf })
}

fn render_view(store: &KnowledgeStore, tail: &[String]) -> Result<OpOutput, OpError> {
    let artifact = find_visible_artifact(store, tail)?;
    let proposal = ContextProposal::from_artifact(&artifact)
        .map_err(|error| OpError::Execution(format!("Vorschlag konnte nicht gelesen werden: {error}")))?;
    Ok(OpOutput {
        text: render_proposal(&proposal),
    })
}

/// Rendert einen Vorschlag vollständig, für `view`.
fn render_proposal(proposal: &ContextProposal) -> String {
    let mut buf = format!(
        "Vorschlag: {}\nTitel: {}\nProgramm: {}\nFassung: {}\nStatus: {:?}\nErzeugt: {} von {}\n",
        proposal.id,
        proposal.title,
        proposal.target_program,
        proposal.target_snapshot_digest,
        proposal.status,
        proposal.generated_at,
        proposal.produced_by,
    );
    buf.push_str(&format!("Änderungen ({}):\n", proposal.changes.len()));
    for change in &proposal.changes {
        buf.push_str(&format!("  - {change:?}\n"));
    }
    buf.push_str(&format!("Belege ({}):\n", proposal.evidence.len()));
    for evidence in &proposal.evidence {
        buf.push_str(&format!("  - [{:?}] {}\n", evidence.kind, evidence.locator));
    }
    buf
}

/// Führt `accept` (`accept = true`) oder `reject` (`accept = false`) aus.
///
/// # Description
/// Liest das Artefakt, entpackt den [`ContextProposal`], ruft
/// ausschließlich [`ContextProposal::accept`]/[`ContextProposal::reject`]
/// auf (beide ändern nachweislich nur `status`, siehe deren Doku in
/// `harw-knowledge`), schreibt danach exakt dieselbe Datei zurück. Es wird
/// an keiner Stelle ein Kontextprogramm gelesen oder geschrieben.
fn decide(store: &KnowledgeStore, tail: &[String], accept: bool) -> Result<OpOutput, OpError> {
    let mut artifact = find_visible_artifact(store, tail)?;
    let mut proposal = ContextProposal::from_artifact(&artifact)
        .map_err(|error| OpError::Execution(format!("Vorschlag konnte nicht gelesen werden: {error}")))?;

    let transition = if accept {
        proposal.accept()
    } else {
        proposal.reject()
    };
    transition.map_err(|error| OpError::Execution(error.to_string()))?;

    // `touch` datiert nur die Frontmatter dieses einen Vorschlag-Artefakts;
    // die Systemuhr ist an der Operations-Schicht zulässig (anders als in
    // `harw-knowledge` selbst — siehe dessen Moduldoku „Woher die
    // beobachteten Daten kommen").
    artifact.frontmatter.touch(jiff::Timestamp::now());

    let updated_artifact = proposal
        .to_artifact(artifact.frontmatter)
        .map_err(|error| OpError::Execution(format!("Vorschlag konnte nicht serialisiert werden: {error}")))?;
    let slug = proposal_slug(&updated_artifact.id);
    store
        .write_artifact(&store.context_proposal_path(&slug), &updated_artifact)
        .map_err(|error| OpError::Execution(format!("Schreiben fehlgeschlagen: {error}")))?;

    Ok(OpOutput {
        text: format!(
            "Vorschlag {} markiert als {:?}. Kein Kontextprogramm wurde geändert.",
            updated_artifact.id, proposal.status
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ContextProposalArgs, decide, find_visible_artifact, list_visible_artifacts, render_list,
        render_view,
    };
    use crate::testutil::toks;
    use harw_knowledge::context_proposal::{ContextProposal, ProposalStatus, RECOMMENDED_VISIBILITY};
    use harw_knowledge::{AgentId, ArtifactId, Frontmatter, KnowledgeStore};
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, Operation};

    use harw_agent_dsl::ids::DefinitionId;

    fn temporary_store(label: &str) -> KnowledgeStore {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("harw-ops-context-proposal-{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temporary knowledge root");
        KnowledgeStore::new(&root)
    }

    fn write_sample_proposal(store: &KnowledgeStore, slug: &str) -> ArtifactId {
        let proposal = ContextProposal::new(
            ArtifactId::new(format!("context-proposal/{slug}")),
            "Testvorschlag",
            DefinitionId::parse("harwness.context.base@1").expect("valid definition id"),
            "deadbeef".repeat(8),
            Vec::new(),
            Vec::new(),
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:must-include-promotion@1",
        );
        let frontmatter = Frontmatter::new(
            AgentId::new("system"),
            RECOMMENDED_VISIBILITY,
            jiff::Timestamp::UNIX_EPOCH,
        );
        let artifact = proposal
            .to_artifact(frontmatter)
            .expect("proposal embeds into an artifact");
        store
            .write_artifact(&store.context_proposal_path(&ArtifactId::new(slug)), &artifact)
            .expect("write sample proposal");
        ArtifactId::new(format!("context-proposal/{slug}"))
    }

    #[test]
    fn from_raw_args_no_tokens_uses_default_sub() {
        let args = ContextProposalArgs::from_raw_args(&toks(&[])).expect("parses");
        assert!(args.sub.is_none());
        assert!(args.tail.is_empty());
    }

    #[test]
    fn from_raw_args_captures_sub_and_tail() {
        let args =
            ContextProposalArgs::from_raw_args(&toks(&["view", "context-proposal/example"]))
                .expect("parses");
        assert_eq!(args.sub.as_deref(), Some("view"));
        assert_eq!(args.tail, vec!["context-proposal/example".to_owned()]);
    }

    #[test]
    fn context_proposal_operation_is_command_only() {
        let surfaces = &super::ContextProposalOperation.meta().surfaces;

        assert!(
            !surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. })),
            "accept/reject must never be reachable as a model tool"
        );
        assert!(surfaces.iter().any(|surface| {
            matches!(
                surface,
                Surface::Command {
                    path: "/context-proposal",
                    visibility: CommandVisibility::ChannelParity,
                }
            )
        }));
    }

    #[test]
    fn list_is_empty_for_a_fresh_store() {
        let store = temporary_store("empty-list");
        let output = render_list(&store).expect("list succeeds on an empty store");
        assert_eq!(output.text, "Keine Kontextprogramm-Vorschläge.");
        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn list_and_view_find_a_written_proposal() {
        let store = temporary_store("list-view");
        let id = write_sample_proposal(&store, "example");

        let listed = list_visible_artifacts(&store).expect("list succeeds");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);

        let view = render_view(&store, &[id.to_string()]).expect("view succeeds");
        assert!(view.text.contains("Testvorschlag"));
        assert!(view.text.contains("Pending"));

        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn view_with_an_unknown_id_is_invalid_arguments() {
        let store = temporary_store("view-unknown");
        let error = find_visible_artifact(&store, &["context-proposal/nope".to_owned()])
            .expect_err("an unknown id must be rejected");
        assert!(matches!(error, harw_operations::OpError::InvalidArguments(_)));
        std::fs::remove_dir_all(store.root()).ok();
    }

    /// Die Prüffläche: `accept` markiert den Vorschlag, ändert aber
    /// nachweislich kein Kontextprogramm — bewiesen über das Dateisystem,
    /// nicht nur über den Typ: außer der einen Vorschlag-Datei entsteht,
    /// verschwindet oder ändert sich keine einzige Datei im Store.
    #[test]
    fn accept_marks_the_proposal_and_touches_no_other_file() {
        let store = temporary_store("accept-marks-only");
        let id = write_sample_proposal(&store, "example");

        let before: std::collections::BTreeSet<std::path::PathBuf> = walk_files(store.root());
        assert_eq!(before.len(), 1, "genau eine Datei vor dem Entscheid");

        let output = decide(&store, &[id.to_string()], true).expect("accept succeeds");
        assert!(output.text.contains("Accepted"));
        assert!(output.text.contains("Kein Kontextprogramm wurde geändert"));

        let after: std::collections::BTreeSet<std::path::PathBuf> = walk_files(store.root());
        assert_eq!(
            before, after,
            "accept darf keine Datei anlegen oder entfernen — nur die eine Vorschlag-Datei ändert sich"
        );

        let artifact = find_visible_artifact(&store, &[id.to_string()])
            .expect("the accepted proposal is still visible and findable");
        let proposal =
            ContextProposal::from_artifact(&artifact).expect("accepted proposal still parses");
        assert_eq!(proposal.status, ProposalStatus::Accepted);

        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn reject_marks_the_proposal() {
        let store = temporary_store("reject-marks");
        let id = write_sample_proposal(&store, "example");

        decide(&store, &[id.to_string()], false).expect("reject succeeds");

        let artifact =
            find_visible_artifact(&store, &[id.to_string()]).expect("proposal still visible");
        let proposal = ContextProposal::from_artifact(&artifact).expect("proposal still parses");
        assert_eq!(proposal.status, ProposalStatus::Rejected);

        std::fs::remove_dir_all(store.root()).ok();
    }

    #[test]
    fn deciding_twice_is_an_execution_error() {
        let store = temporary_store("decide-twice");
        let id = write_sample_proposal(&store, "example");

        decide(&store, &[id.to_string()], true).expect("first decision succeeds");
        let error = decide(&store, &[id.to_string()], false)
            .expect_err("a second decision on the same proposal must fail");
        assert!(matches!(error, harw_operations::OpError::Execution(_)));

        std::fs::remove_dir_all(store.root()).ok();
    }

    fn walk_files(root: &std::path::Path) -> std::collections::BTreeSet<std::path::PathBuf> {
        let mut files = std::collections::BTreeSet::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.insert(path);
                }
            }
        }
        files
    }
}
