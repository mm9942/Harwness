//! Topic memory: one file per coherent subject, `topics/<slug>.md` (§2.1).
//!
//! Spec: `docs/design/knowledge-surfaces.md` §2.1, §2.4. Detailed, not
//! bootstrap-loaded, retrieved on demand via recall. Freely agent-writable
//! within the writer's own scope (no review gate to *write*; only to *promote
//! out of* topic memory into the palace, §2.4). Slug derivation and read/write
//! are implemented fully.

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::lock::KnowledgeLock;
use crate::memory::palace::{PalaceStatus, TITLE_KEY, scan_wikilinks, set_status};
use crate::store::KnowledgeStore;
use crate::visibility::{AgentId, VisibilityScope};

/// Frontmatter-`extra`-Schlüssel der Herkunft eines vorgeschlagenen Themas
/// (siehe [`TopicOrigin`]).
pub const ORIGIN_KEY: &str = "origin";

/// Obergrenze der Titellänge (Unicode-Zeichen) eines vorgeschlagenen Themas.
const MAX_TITLE_CHARS: usize = 120;

/// Derive a filesystem-safe slug from a topic title (lowercase, dash-joined).
#[must_use]
pub fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// Stable id for a topic file (`topic/<slug>`).
#[must_use]
pub fn topic_id(slug: &str) -> ArtifactId {
    ArtifactId::new(format!("topic/{slug}"))
}

/// Read a topic-memory artifact by slug.
///
/// # Errors
/// [`crate::error::KnowledgeError::Io`] if missing/unreadable, parse errors on
/// malformed frontmatter.
pub fn read(store: &KnowledgeStore, slug: &str) -> KnowledgeResult<KnowledgeArtifact> {
    store.read_artifact(
        &store.topic_path(slug),
        topic_id(slug),
        ArtifactKind::TopicMemory,
    )
}

/// Write (create or replace) a topic-memory artifact under `slug`.
///
/// # Errors
/// [`crate::error::KnowledgeError::Frontmatter`]/[`crate::error::KnowledgeError::Io`] on write.
pub fn write(
    store: &KnowledgeStore,
    slug: &str,
    frontmatter: Frontmatter,
    body: impl Into<String>,
) -> KnowledgeResult<()> {
    let artifact =
        KnowledgeArtifact::new(topic_id(slug), ArtifactKind::TopicMemory, frontmatter, body);
    store.write_artifact(&store.topic_path(slug), &artifact)
}

/// Herkunft eines vorgeschlagenen Themas; landet als `extra.origin` im
/// Frontmatter.
///
/// # Beschreibung
/// `kind` benennt den Schreibpfad (`fact` für `/memory promote`, `dream` für
/// angenommene Dream-Vorschläge aus D5), `id` die Quelle in diesem Pfad
/// (Fakt-Name bzw. `<bericht>/<vorschlag>`), `detail` eine optionale
/// Zusatzangabe (etwa der Fakt-Scope `project`/`global`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicOrigin {
    /// Schreibpfad, z. B. `fact` oder `dream`.
    pub kind: String,
    /// Quell-Id innerhalb des Schreibpfads.
    pub id: String,
    /// Optionale Zusatzangabe (z. B. Fakt-Scope).
    pub detail: Option<String>,
}

/// Ein Themenvorschlag, der über [`propose_topic`] als `provisional`
/// geschrieben wird.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicProposal {
    /// Gewünschter Slug; `None` leitet ihn aus [`Self::title`] ab. Wird immer
    /// durch [`slugify`] normalisiert.
    pub slug: Option<String>,
    /// Menschlich lesbarer Titel (`extra.title`).
    pub title: String,
    /// Markdown-Body; `[[wikilinks]]` werden zu Frontmatter-`links`.
    pub body: String,
    /// Tags des Themas.
    pub tags: Vec<String>,
    /// Herkunft (Pflicht: ein Thema ohne Herkunft ist nicht nachvollziehbar).
    pub origin: TopicOrigin,
}

/// Baut einen [`TopicProposal`] aus den Feldern eines harw-memory-Fakts.
///
/// # Beschreibung
/// `harw-knowledge` kennt `harw_memory::Fact` bewusst nicht (keine
/// Abhängigkeit in diese Richtung); der Aufrufer reicht die Felder durch.
/// Slug = Fakt-Name, Titel = Beschreibung (sonst der Name), Body = Fakt-Body.
///
/// # Argumente
/// - `fact_id` — Name des Fakts (`harw_memory::Fact::name`).
/// - `description` — Kurzbeschreibung (`Fact::description`).
/// - `body` — Fakt-Text.
/// - `tags` — Fakt-Tags.
/// - `scope` — Fakt-Scope als Text (`project`/`global`), landet in
///   `origin.detail`.
#[must_use]
pub fn from_fact(
    fact_id: &str,
    description: &str,
    body: &str,
    tags: &[String],
    scope: &str,
) -> TopicProposal {
    let title = if description.trim().is_empty() {
        fact_id.to_owned()
    } else {
        description.trim().to_owned()
    };
    TopicProposal {
        slug: Some(fact_id.to_owned()),
        title,
        body: body.trim_end().to_owned(),
        tags: tags.to_vec(),
        origin: TopicOrigin {
            kind: "fact".to_owned(),
            id: fact_id.to_owned(),
            detail: (!scope.is_empty()).then(|| scope.to_owned()),
        },
    }
}

/// Schreibt einen Themenvorschlag als neues `topics/<slug>.md` mit Status
/// [`PalaceStatus::Provisional`] — nie über ein bestehendes Thema.
///
/// # Beschreibung
/// Der gemeinsame Schreibpfad für `/memory promote` (Fakt → Thema) und
/// angenommene Dream-Vorschläge (D5). Unter der Dateisperre des Ziels wird
/// geprüft, dass noch kein Thema dieses Slugs existiert; erst dann wird
/// geschrieben. Das Frontmatter trägt `extra.title`, `extra.confidence =
/// provisional` und `extra.origin = {kind, id, detail?, at}`. Zu
/// `established` wird das Thema erst über `/palace promote` (Review-Gate
/// §2.5).
///
/// # Rückgabe
/// Den normalisierten Slug des geschriebenen Themas.
///
/// # Errors
/// - [`KnowledgeError::Io`] mit `InvalidInput`, wenn Titel/Slug leer sind
///   oder der Body leer ist.
/// - [`KnowledgeError::Io`] mit `AlreadyExists`, wenn `topics/<slug>.md`
///   bereits existiert (Konflikt; es wird nie überschrieben).
/// - Sperr-, Render- und Schreibfehler als [`KnowledgeError::Io`] bzw.
///   [`KnowledgeError::Frontmatter`].
pub fn propose_topic(
    store: &KnowledgeStore,
    proposal: &TopicProposal,
    author: &AgentId,
    visibility: VisibilityScope,
    now: jiff::Timestamp,
) -> KnowledgeResult<String> {
    let slug = slugify(proposal.slug.as_deref().unwrap_or(&proposal.title));
    if slug.is_empty() {
        return Err(invalid_input(format!(
            "kein gültiger Themen-Slug aus '{}'",
            proposal.slug.as_deref().unwrap_or(&proposal.title)
        )));
    }
    if proposal.body.trim().is_empty() {
        return Err(invalid_input(format!("Thema '{slug}' hat keinen Inhalt")));
    }
    let title = clamp_title(&proposal.title, &slug);

    let path = store.topic_path(&slug);
    let _lock = KnowledgeLock::for_target(&path)?;
    if path.exists() {
        return Err(KnowledgeError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("Thema 'topic/{slug}' existiert bereits; es wird nie überschrieben"),
        )));
    }

    let mut frontmatter = Frontmatter::new(author.clone(), visibility, now);
    frontmatter.tags = proposal.tags.clone();
    frontmatter.links = scan_wikilinks(&proposal.body)
        .into_iter()
        .map(ArtifactId::new)
        .collect();
    frontmatter
        .extra
        .insert(TITLE_KEY.to_owned(), serde_json::Value::String(title));
    set_status(&mut frontmatter, PalaceStatus::Provisional);
    let mut origin = serde_json::json!({
        "kind": proposal.origin.kind,
        "id": proposal.origin.id,
        "at": now.to_string(),
    });
    if let (Some(detail), Some(fields)) = (&proposal.origin.detail, origin.as_object_mut()) {
        fields.insert(
            "detail".to_owned(),
            serde_json::Value::String(detail.clone()),
        );
    }
    frontmatter.extra.insert(ORIGIN_KEY.to_owned(), origin);
    write(store, &slug, frontmatter, proposal.body.clone())?;
    Ok(slug)
}

/// Titel ohne Zeilenumbrüche, auf [`MAX_TITLE_CHARS`] gekürzt; leer → Slug.
fn clamp_title(title: &str, slug: &str) -> String {
    let single_line = title.lines().next().unwrap_or("").trim();
    if single_line.is_empty() {
        return slug.to_owned();
    }
    if single_line.chars().count() <= MAX_TITLE_CHARS {
        return single_line.to_owned();
    }
    let mut clamped: String = single_line.chars().take(MAX_TITLE_CHARS).collect();
    clamped.push('…');
    clamped
}

/// `InvalidInput`-Fehler mit Meldung.
fn invalid_input(message: String) -> KnowledgeError {
    KnowledgeError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        message,
    ))
}

#[cfg(test)]
mod tests {
    use super::{ORIGIN_KEY, from_fact, propose_topic, read};
    use crate::error::KnowledgeError;
    use crate::memory::palace::{PalaceStatus, artifact_status};
    use crate::store::KnowledgeStore;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::visibility::{AgentId, VisibilityScope};

    fn temporary_store(label: &str) -> TestResult<KnowledgeStore> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-knowledge-topic-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("create root"))?;
        Ok(KnowledgeStore::new(&root))
    }

    #[test]
    fn propose_topic_writes_a_provisional_topic_with_origin() -> TestResult {
        let store = temporary_store("propose")?;
        let proposal = from_fact(
            "deploy-rollback",
            "Rollback über Canary",
            "Rollback läuft über [[palace/canary]].\n",
            &["ops".to_owned()],
            "project",
        );
        let slug = propose_topic(
            &store,
            &proposal,
            &AgentId::new("operator"),
            VisibilityScope::OperatorOnly,
            jiff::Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("propose"))?;
        assert_eq!(slug, "deploy-rollback");
        let topic = read(&store, &slug).map_err(ctx("read topic"))?;
        assert_eq!(artifact_status(&topic), PalaceStatus::Provisional);
        assert_eq!(topic.frontmatter.extra["title"], "Rollback über Canary");
        let origin = &topic.frontmatter.extra[ORIGIN_KEY];
        assert_eq!(origin["kind"], "fact");
        assert_eq!(origin["id"], "deploy-rollback");
        assert_eq!(origin["detail"], "project");
        assert_eq!(origin["at"], jiff::Timestamp::UNIX_EPOCH.to_string());
        assert_eq!(topic.frontmatter.tags, vec!["ops".to_owned()]);
        assert_eq!(topic.frontmatter.links.len(), 1);
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn propose_topic_never_overwrites_and_rejects_empty_input() -> TestResult {
        let store = temporary_store("conflict")?;
        let proposal = from_fact("a", "", "Text", &[], "global");
        let author = AgentId::new("operator");
        let now = jiff::Timestamp::UNIX_EPOCH;
        propose_topic(
            &store,
            &proposal,
            &author,
            VisibilityScope::OperatorOnly,
            now,
        )
        .map_err(ctx("first"))?;
        let Err(KnowledgeError::Io(error)) = propose_topic(
            &store,
            &proposal,
            &author,
            VisibilityScope::OperatorOnly,
            now,
        ) else {
            return Err(TestError::Unexpected(
                "second write must conflict".to_owned(),
            ));
        };
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);

        for bad in [
            from_fact("", "", "x", &[], ""),
            from_fact("b", "", "  ", &[], ""),
        ] {
            let Err(KnowledgeError::Io(error)) =
                propose_topic(&store, &bad, &author, VisibilityScope::OperatorOnly, now)
            else {
                return Err(TestError::Unexpected(format!("{bad:?} must be refused")));
            };
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }
}
