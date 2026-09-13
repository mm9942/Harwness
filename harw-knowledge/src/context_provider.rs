//! Knowledge als Fragment-Quelle — `VisibilityScope` wird beim Gather
//! durchgesetzt, nicht danach.
//!
//! # Verantwortungsbereich
//! [`KnowledgeContextProvider`] ist die dritte der drei AW3-03-
//! Quellenbindungen: sie liefert [`harw_context::Fragment`]e aus
//! [`crate::index::KnowledgeIndex`].
//!
//! # Wo die Sichtbarkeit durchgesetzt wird — und warum genau dort
//! [`KnowledgeContextProvider::fragments`] ruft **ausschließlich**
//! [`crate::memory::recall::search`] auf, nie
//! `KnowledgeIndex::iter`/`get`/`backlinks` direkt (mit einer einzigen,
//! dokumentierten Ausnahme — siehe unten). `search` (über `search_with`)
//! ist der Ort, an dem `VisibilityScope` bereits **zweimal** durchgesetzt
//! wird: einmal am direkten Kandidatenfilter, ein zweites Mal bei jedem
//! Sprung in `expand_backlinks` — genau die Disziplin, die AW4-05 in
//! `harw-knowledge` nachträglich einführen musste, nachdem ein `SelfOnly`-
//! Artefakt, das nur über einen Rückverweis erreichbar war, durch die
//! ungeprüfte `expand_backlinks`-Traversierung rutschte (siehe
//! `memory/recall.rs`, Moduldoku und
//! `test_backlink_expansion_does_not_leak_a_self_only_note`). Diese Datei
//! erbt diese Disziplin, indem sie **keinen zweiten, ungeprüften Lesepfad
//! eröffnet**: der einzige Ort, an dem ein Artefakt zu einem Fragment wird,
//! ist [`KnowledgeContextProvider::fragment_from_hit`], und die einzige
//! Eingabe dorthin ist ein `RecallHit`, dessen `ArtifactId` bereits durch
//! `search`s zweifache Prüfung gelaufen ist.
//!
//! Die eine Ausnahme: [`KnowledgeContextProvider::fragment_from_hit`] ruft
//! `KnowledgeIndex::get(&hit.artifact.id)` auf, um den vollen Markdown-Body
//! nachzuladen — `RecallHit` trägt nur die leichte [`crate::index::ArtifactRef`]-
//! Projektion (id, kind, tags, visibility), keinen Body. Das ist **kein**
//! zweiter, ungeprüfter Zugriffspfad: `get` wird nur mit einer `ArtifactId`
//! aufgerufen, die `search` bereits als sichtbar bestätigt hat, nicht mit
//! einer vom Aufrufer frei gewählten Id. Ein Artefakt, das `search` nicht
//! zurückgegeben hätte (weil `OperatorOnly` ohne Berechtigung, oder nur über
//! einen ungeprüften Rückverweis erreichbar), wird nie an `get` übergeben und
//! erscheint deshalb nie in `fragments()`s Ergebnis.
//!
//! # Vertrauen und Namensraum
//! [`KNOWLEDGE_CONTEXT_NAMESPACE`] (`"knowledge"`),
//! [`KNOWLEDGE_CONTEXT_MAX_TRUST`] ([`harw_context::TrustClass::Data`] —
//! Knowledge-Artefakte reichen von Operator-Notizen bis zu
//! agentengeschriebenen Palace-Knoten; keine einheitliche Klasse darüber
//! wäre gerechtfertigt) und [`KNOWLEDGE_CONTEXT_MAY_CARRY_USER_CONTENT`]
//! (`true`).
//!
//! # Exportierte Typen
//! [`KnowledgeContextProvider`], [`KNOWLEDGE_CONTEXT_NAMESPACE`],
//! [`KNOWLEDGE_CONTEXT_MAX_TRUST`], [`KNOWLEDGE_CONTEXT_MAY_CARRY_USER_CONTENT`].
//!
//! # Concurrency
//! `KnowledgeContextProvider<'a>` ist `Send + Sync`, solange
//! [`crate::index::KnowledgeIndex`] es ist (reiner Werttyp, ja). Hält nur
//! eine Referenz auf den Index; `fragments` macht keine I/O — der Index ist
//! bereits im Speicher.
//!
//! # Fehler
//! [`crate::error::KnowledgeError::RecallBoundExceeded`], wenn die
//! übergebene `RecallQuery` `max_artifacts`/`max_hops` über die harten
//! Maxima hinaus anfordert — durchgereicht von `search` (§2.3).

use harw_context::{Fragment, FragmentLabel, FragmentOrigin, SectionName, Stability, TrustClass};
use harw_lens_types::{BytesOverFour, CostEstimator};

use crate::artifact::{ArtifactKind, RecallQuery};
use crate::error::KnowledgeResult;
use crate::index::KnowledgeIndex;
use crate::memory::recall::{RecallHit, search};

/// Sektionspräfix, unter dem [`KnowledgeContextProvider`] liefert.
pub const KNOWLEDGE_CONTEXT_NAMESPACE: &str = "knowledge";

/// Höchste Vertrauensklasse, die [`KnowledgeContextProvider`] behaupten darf.
pub const KNOWLEDGE_CONTEXT_MAX_TRUST: TrustClass = TrustClass::Data;

/// `true`: Knowledge-Artefakte können Operator- oder Agenten-verfasste
/// Inhalte beliebiger Herkunft tragen — [`KnowledgeContextProvider`] darf
/// deshalb niemals `TrustClass::Instruction` behaupten (siehe Moduldoku).
pub const KNOWLEDGE_CONTEXT_MAY_CARRY_USER_CONTENT: bool = true;

/// Name dieses Providers, für [`FragmentOrigin::provider`].
const PROVIDER_NAME: &str = "KnowledgeContextProvider";

/// Liefert sichtbarkeitsgefilterte Knowledge-Artefakte als
/// [`harw_context::Fragment`]e.
///
/// # Description
/// Siehe Moduldokumentation, Abschnitt „Wo die Sichtbarkeit durchgesetzt
/// wird". Jeder `RecallHit` aus [`crate::memory::recall::search`] wird zu
/// höchstens einem Fragment.
///
/// # Concurrency
/// `Send + Sync`, solange [`KnowledgeIndex`] es ist. Hält nur eine
/// Referenz.
///
/// # Examples
/// ```rust
/// use harw_knowledge::artifact::RecallQuery;
/// use harw_knowledge::context_provider::KnowledgeContextProvider;
/// use harw_knowledge::index::KnowledgeIndex;
/// use harw_knowledge::visibility::VisibilityScope;
///
/// let index = KnowledgeIndex::new();
/// let provider = KnowledgeContextProvider::new(&index);
/// let query = RecallQuery::new("anything", VisibilityScope::SelfOnly);
/// let fragments = provider
///     .fragments(&query, jiff::Timestamp::UNIX_EPOCH)
///     .expect("query within hard bounds always succeeds");
/// assert!(fragments.is_empty(), "leerer Index liefert nichts");
/// ```
pub struct KnowledgeContextProvider<'a> {
    /// Der abzufragende Index.
    index: &'a KnowledgeIndex,
}

impl<'a> KnowledgeContextProvider<'a> {
    /// Konstruiert einen Provider über einem bestehenden Index.
    ///
    /// # Arguments
    /// - `index` (`&'a KnowledgeIndex`): der abzufragende Index.
    ///
    /// # Returns
    /// Den konfigurierten Provider.
    #[must_use]
    pub fn new(index: &'a KnowledgeIndex) -> Self {
        Self { index }
    }

    /// Baut die Fragmente für eine gebundene Recall-Anfrage.
    ///
    /// # Description
    /// Ruft [`crate::memory::recall::search`] mit `query` auf — der
    /// einzige Lesepfad, über den ein Artefakt in dieser Datei erreichbar
    /// ist. `query.caller_scope` bestimmt, was sichtbar ist; die Prüfung
    /// selbst liegt in `search_with` (siehe Moduldoku).
    ///
    /// # Arguments
    /// - `query` (`&RecallQuery`): die gebundene, sichtbarkeitsgebundene
    ///   Anfrage (Text, Kind-/Tag-Filter, `caller_scope`, `max_artifacts`,
    ///   `max_hops`).
    /// - `produced_at` (`jiff::Timestamp`): Zeitstempel für
    ///   [`FragmentOrigin::produced_at`]. Injiziert statt aus der Systemuhr
    ///   gelesen, damit dieselbe Anfrage immer dasselbe Ergebnis liefert.
    ///
    /// # Returns
    /// Ein Fragment je sichtbarem Treffer, in `search`s Reihenfolge
    /// (Score absteigend, direkte Treffer vor Backlink-Treffern).
    ///
    /// # Errors
    /// [`crate::error::KnowledgeError::RecallBoundExceeded`], wenn
    /// `query.max_artifacts` oder `query.max_hops` die harten Maxima
    /// überschreitet (durchgereicht von `search`).
    ///
    /// # Concurrency
    /// Liest den Index einmal synchron; keine I/O.
    pub fn fragments(
        &self,
        query: &RecallQuery,
        produced_at: jiff::Timestamp,
    ) -> KnowledgeResult<Vec<Fragment>> {
        let result = search(self.index, query)?;
        Ok(result
            .hits
            .iter()
            .filter_map(|hit| self.fragment_from_hit(hit, produced_at))
            .collect())
    }

    /// Die eine Stelle, an der ein Artefakt zu einem Fragment wird.
    ///
    /// # Description
    /// `hit.artifact.id` hat bereits `search`s zweifache Sichtbarkeitsprüfung
    /// bestanden (siehe Moduldoku) — der `KnowledgeIndex::get`-Aufruf hier
    /// lädt nur den Body nach, er öffnet keinen zweiten Prüfpfad. Ein
    /// Fragment ohne gültiges Label (sollte bei einer nicht-leeren
    /// `ArtifactId` nie vorkommen) wird übersprungen statt die gesamte
    /// Anfrage scheitern zu lassen.
    fn fragment_from_hit(&self, hit: &RecallHit, produced_at: jiff::Timestamp) -> Option<Fragment> {
        let artifact = self.index.get(&hit.artifact.id)?;

        let section_name = format!(
            "{KNOWLEDGE_CONTEXT_NAMESPACE}.{}",
            kind_slug(artifact.kind)
        );
        let section = SectionName::try_new(section_name).ok()?;
        let label = FragmentLabel::try_new(artifact.id.as_str()).ok()?;

        let body = artifact.body.clone();
        let cost = BytesOverFour.estimate(&body);
        let digest = harw_types::ContentDigest::of(body.as_bytes());

        Some(Fragment {
            label,
            section,
            trust: KNOWLEDGE_CONTEXT_MAX_TRUST,
            stability: Stability::Stable,
            origin: FragmentOrigin {
                provider: PROVIDER_NAME.to_owned(),
                namespace: KNOWLEDGE_CONTEXT_NAMESPACE.to_owned(),
                produced_at,
            },
            cost,
            digest,
            body,
        })
    }
}

/// Kurzform eines [`ArtifactKind`] für den Sektionsnamen.
fn kind_slug(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::CoreMemory => "core",
        ArtifactKind::TopicMemory => "topic",
        ArtifactKind::PalaceNode => "palace",
        ArtifactKind::DiaryEntry => "diary",
        ArtifactKind::DreamReport => "dream",
        ArtifactKind::WorkbenchNote | ArtifactKind::WorkbenchHypothesis => "workbench",
        ArtifactKind::KanbanCard => "kanban",
        ArtifactKind::SecurityFinding => "security-finding",
        ArtifactKind::Baseline => "baseline",
        ArtifactKind::ContextProposal => "context-proposal",
        // Ergaenzt mit AW6-06. Bewusst ein eigener Slug statt einer Zusammen-
        // fassung mit `context-proposal`: die beiden Vorschlagsarten tragen
        // verschiedene Vokabulare (Kontextsektionen vs. Katalogfaehigkeiten),
        // und ein gemeinsamer Sektionsname machte sie im Abruf ununterscheidbar.
        ArtifactKind::ModelBehaviorProposal => "model-behavior-proposal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ArtifactId, Frontmatter, KnowledgeArtifact};
    use crate::visibility::{AgentId, VisibilityScope};

    /// Baut ein Test-Artefakt — dieselbe Fixture-Form wie
    /// `memory/recall.rs`s eigene Tests, damit beide Testsuiten exakt
    /// dieselbe AW4-05-Regression gegen dieselben Eingaben prüfen können.
    fn artifact(
        id: &str,
        kind: ArtifactKind,
        visibility: VisibilityScope,
        body: &str,
        links: Vec<ArtifactId>,
    ) -> KnowledgeArtifact {
        let mut frontmatter =
            Frontmatter::new(AgentId::new("agent"), visibility, jiff::Timestamp::UNIX_EPOCH);
        frontmatter.links = links;
        KnowledgeArtifact::new(ArtifactId::new(id), kind, frontmatter, body)
    }

    #[test]
    fn test_operator_only_artifact_absent_without_operator_permission() {
        let mut index = KnowledgeIndex::new();
        index.insert(artifact(
            "security/critical-vuln",
            ArtifactKind::SecurityFinding,
            VisibilityScope::OperatorOnly,
            "critical vulnerability found in the auth module",
            Vec::new(),
        ));

        let provider = KnowledgeContextProvider::new(&index);
        let query = RecallQuery::new("vulnerability", VisibilityScope::SelfOnly);
        let fragments = provider
            .fragments(&query, jiff::Timestamp::UNIX_EPOCH)
            .expect("query within hard bounds succeeds");

        assert!(
            fragments.is_empty(),
            "OperatorOnly-Artefakt wurde ohne Berechtigung zu einem Fragment: {fragments:?}"
        );
    }

    #[test]
    fn test_operator_only_artifact_present_with_operator_permission() {
        let mut index = KnowledgeIndex::new();
        index.insert(artifact(
            "security/critical-vuln",
            ArtifactKind::SecurityFinding,
            VisibilityScope::OperatorOnly,
            "critical vulnerability found in the auth module",
            Vec::new(),
        ));

        let provider = KnowledgeContextProvider::new(&index);
        let query = RecallQuery::new("vulnerability", VisibilityScope::OperatorOnly);
        let fragments = provider
            .fragments(&query, jiff::Timestamp::UNIX_EPOCH)
            .expect("query within hard bounds succeeds");

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].label.as_str(), "security/critical-vuln");
        assert_eq!(fragments[0].section.as_str(), "knowledge.security-finding");
    }

    /// Die AW4-05-Regression an dieser Schicht: ein Artefakt, das nur über
    /// einen Rückverweis erreichbar wäre, rutscht nicht durch, obwohl der
    /// verlinkende Notiz-Text den sichtbaren Treffer trägt.
    #[test]
    fn test_backlink_only_reachable_note_does_not_leak_through() {
        let mut index = KnowledgeIndex::new();
        index.insert(artifact(
            "security/critical-vuln",
            ArtifactKind::SecurityFinding,
            VisibilityScope::OperatorOnly,
            "critical vulnerability found in the auth module",
            Vec::new(),
        ));
        index.insert(artifact(
            "diary/private-note",
            ArtifactKind::DiaryEntry,
            VisibilityScope::SelfOnly,
            "private note referencing the vulnerability",
            vec![ArtifactId::new("security/critical-vuln")],
        ));

        let provider = KnowledgeContextProvider::new(&index);
        let query = RecallQuery::new("vulnerability", VisibilityScope::OperatorOnly);
        let fragments = provider
            .fragments(&query, jiff::Timestamp::UNIX_EPOCH)
            .expect("query within hard bounds succeeds");

        let labels: Vec<&str> = fragments.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["security/critical-vuln"],
            "SelfOnly-Notiz ist über den Rückverweis durchgerutscht: {labels:?}"
        );
    }

    /// `VisibilityScope::visible_to_caller` (`visibility.rs`) fails closed
    /// for every scope except `OperatorOnly` — `SelfOnly`, `DescendantTree`
    /// and `ExplicitlyGranted` are never visible to *any* caller from this
    /// crate alone (full identity proof is `harw-policy`'s job). `OperatorOnly`
    /// is therefore the only scope this test can use to observe a fragment
    /// actually being produced.
    #[test]
    fn test_fragment_fields_are_fully_populated() {
        let mut index = KnowledgeIndex::new();
        index.insert(artifact(
            "topic/rust-tips",
            ArtifactKind::TopicMemory,
            VisibilityScope::OperatorOnly,
            "prefer borrow over clone",
            Vec::new(),
        ));

        let provider = KnowledgeContextProvider::new(&index);
        let query = RecallQuery::new("borrow", VisibilityScope::OperatorOnly);
        let produced_at = jiff::Timestamp::UNIX_EPOCH;
        let fragments = provider
            .fragments(&query, produced_at)
            .expect("query within hard bounds succeeds");

        assert_eq!(fragments.len(), 1);
        let fragment = &fragments[0];
        assert_eq!(fragment.trust, KNOWLEDGE_CONTEXT_MAX_TRUST);
        assert_eq!(fragment.stability, Stability::Stable);
        assert_eq!(fragment.origin.provider, PROVIDER_NAME);
        assert_eq!(fragment.origin.namespace, KNOWLEDGE_CONTEXT_NAMESPACE);
        assert_eq!(fragment.origin.produced_at, produced_at);
        assert!(fragment.cost.0 > 0);
        assert_ne!(fragment.digest, harw_types::ContentDigest::of(b""));
        assert_eq!(fragment.body, "prefer borrow over clone");
        assert_eq!(fragment.section.as_str(), "knowledge.topic");
    }

    #[test]
    fn test_empty_index_contributes_nothing() {
        let index = KnowledgeIndex::new();
        let provider = KnowledgeContextProvider::new(&index);
        let query = RecallQuery::new("anything", VisibilityScope::SelfOnly);
        let fragments = provider
            .fragments(&query, jiff::Timestamp::UNIX_EPOCH)
            .expect("query within hard bounds succeeds");
        assert!(fragments.is_empty());
    }
}
