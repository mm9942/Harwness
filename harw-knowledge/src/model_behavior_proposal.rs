//! `ModelBehaviorProposal` — der Layer-4-Rückkanal des Modellkatalogs: ein
//! Vorschlag, eine Katalogbehauptung über ein Modell an beobachtetes
//! Verhalten anzupassen, als Typ und als durables Artefakt (Knoten
//! **AW6-06**, Fortsetzung von AW5-09/K8).
//!
//! # Warum es keine `apply`-Funktion gibt
//! Die tragende Regel dieses Teilbaums lautet wörtlich: **Layer-4-Rückkanal —
//! durabler Dream-Job, schlägt vor, committet nie.** Ein System, das seinen
//! eigenen Modellkatalog selbsttätig ändert, kann sich in einen Zustand
//! bringen, den niemand beschlossen hat — und die nächste Beobachtung entstünde
//! dann aus dem geänderten Zustand, nicht aus einer menschlichen Entscheidung.
//! Diese Regel ist hier **strukturell**, nicht nur dokumentarisch wahr gemacht
//! — exakt nach demselben Muster, das [`crate::context_proposal`] (AW5-09) für
//! Kontextprogramme bereits etabliert hat:
//!
//! - [`ModelBehaviorProposal`] trägt keine `apply`-Methode — nur
//!   [`ModelBehaviorProposal::accept`] und [`ModelBehaviorProposal::reject`],
//!   die ausschließlich [`ModelBehaviorProposal::status`] umschreiben (siehe
//!   [`ModelBehaviorProposal::accept`]s Doku für den Beweis, dass dabei kein
//!   anderes Feld berührt wird).
//! - Diese Datei importiert `harw_model_catalog::descriptor` ausschließlich für
//!   **Lese**typen (`ToolCallingSupport`, `TokenCount`) und
//!   `harw_model_catalog::observed::Score` als reinen Wertetyp. Keine Funktion
//!   hier ruft irgendeine Schreib- oder Registrierungsfunktion aus
//!   `harw-model-catalog` auf, keine schreibt einen `ModelDescriptor` fort.
//!   Einen Katalogeintrag zu *ändern* bleibt ausschließlich Sache eines
//!   Menschen, der `harw-model-catalog`s Vendor-Module per Hand pflegt.
//!
//! # Die Heuristik in Worten
//! [`propose_tool_calling_downgrades`] prüft — deterministisch, ohne
//! Systemuhr, ohne Zufallszahl, ohne Modellaufruf — eine Regel:
//!
//! > Für jedes Modell, dessen `tool_schema_reliability` aus mindestens
//! > [`MIN_RELIABLE_SAMPLE_COUNT`] Beobachtungen unterhalb von
//! > [`TOOL_SCHEMA_UNRELIABLE_THRESHOLD`] liegt und dessen Katalogeintrag ein
//! > `tool_calling` oberhalb von `ToolCallingSupport::None` behauptet, schlägt
//! > die Heuristik genau eine Änderung vor: diese Behauptung um exakt eine
//! > Stufe herabzusetzen (`Native → Parallel → Basic → None`) — sortiert nach
//! > (Provider, Modell), und ganz ohne Vorschlag, wenn kein Modell die
//! > Bedingung erfüllt oder die Behauptung bereits `None` ist.
//!
//! Das ist bewusst eine einzige, in einem Satz erklärbare Regel statt eines
//! Punktesystems über mehrere Score-Dimensionen — dasselbe Prinzip, das
//! [`crate::context_proposal::propose_must_include_promotions`] für Kontext-
//! programme bereits durchhält: ein Vorschlag, der aus „dieses Modell hat das
//! Tool-Schema in N gemessenen Läufen wiederholt verletzt" folgt, ist für
//! einen Menschen ohne Rückfrage nachvollziehbar; ein gewichtetes Scoring über
//! alle sechs [`harw_model_catalog::observed::ObservedModelBehavior`]-Felder
//! wäre es nicht. Die Schwelle [`MIN_RELIABLE_SAMPLE_COUNT`] spiegelt bewusst
//! dieselbe Zahl, ab der `harw_model_catalog::provenance::MetricEstimate`
//! einen Wert als `Measured` statt `Insufficient` einstuft — kein zweites,
//! unbegründetes Sample-Kriterium.
//!
//! # Woher die beobachteten Daten kommen — und woher (noch) nicht
//! Die Heuristik oben braucht Beobachtungsdaten: welches Modell hat sein
//! Tool-Schema wie oft verletzt. `harw_model_catalog::observed` definiert
//! genau das passende Vokabular — [`harw_model_catalog::observed::ObservedModelBehavior`]
//! trägt ein `tool_schema_reliability`-Feld vom Typ
//! [`harw_model_catalog::observed::Score`] mitsamt `evidence: Vec<EvaluationRunId>`
//! und `updated_at: Option<OffsetDateTime>`. Aber:
//!
//! 1. Jeder Konstruktor, den `harw-model-catalog` heute exportiert
//!    ([`harw_model_catalog::observed::ObservedModelBehavior::bootstrap`],
//!    [`harw_model_catalog::observed::observations_from_descriptors`],
//!    [`harw_model_catalog::observed::bootstrap_observations`]), liefert
//!    ausschließlich den konservativen Bootstrap-Zustand: alle sechs Scores
//!    fest auf `Score::HALF`, `updated_at = None`, `evidence = vec![]`. Es
//!    gibt **keinen** Konstruktor für einen tatsächlich gemessenen Zustand.
//! 2. Eine vollständige Baumsuche nach `ObservedModelBehavior` und jedem
//!    seiner sechs Score-Felder außerhalb von `harw-model-catalog` selbst
//!    ergab **keinen** einzigen Treffer — kein Aufrufer speist reale
//!    Harness-Lauf-Ergebnisse in diesen Typ ein, und `EvaluationRunId`
//!    (der Belegtyp für `evidence`) wird nirgendwo sonst im Baum referenziert.
//!
//! Es gibt also **keinen Pfad**, über den echtes Modellverhalten (reale
//! Tool-Schema-Verletzungen aus einem Harness-Run) diesen Schreibbereich heute
//! erreicht — dasselbe Muster, das [`crate::context_proposal`] für
//! `Assembly::omissions` bereits gemeldet hat, und vor ihm `validate_patch`
//! ohne Aufrufer und `TraceContext` ohne Erzeuger an anderer Stelle in diesem
//! Ausbauprogramm. Die Heuristik ist deshalb bewusst gegen einen **expliziten,
//! vom Aufrufer gefüllten Eingabetyp** gebaut — [`ModelBehaviorObservationWindow`]
//! — statt auf einen Erzeuger zu warten, den es nicht gibt. Ein künftiger
//! Knoten, der `harw-model-catalog` einen echten `measured`-Konstruktor und
//! einen Harness-Erzeuger für `ObservedModelBehavior` gibt, füllt diesen Typ;
//! diese Lücke wird im Abschlussbericht dieses Knotens genannt, nicht
//! stillschweigend mit erfundenen Daten überbrückt.
//!
//! # Warum kein durabler Dream-Job in dieser Datei verdrahtet wird
//! Der Auftrag nennt `harw-job-runtime`/`harw-session-store`
//! (`StoredJob`/`JobStore`) als Mechanik für einen „durablen Dream-Job, der
//! nicht im Turn läuft". Die Kantenrichtung wurde geprüft: `harw-knowledge`
//! hängt bereits von beiden Crates ab (siehe `Cargo.toml`, ursprünglich für
//! [`crate::dream::DreamReport`]s `WorkId` eingeführt), also entsteht durch
//! bloßes *Verwenden* dieser bestehenden Abhängigkeit kein neuer Zyklus.
//! [`crate::context_proposal`] (AW5-09) — der Knoten, dessen Mechanik dieser
//! hier erbt — hat diese Verdrahtung bei identischer Ausgangslage bewusst
//! ausgelassen: es exportiert keine `StoredJob`-Instanz, sondern ausschließlich
//! die reine, deterministische Vorschlagsfunktion. Dieser Knoten folgt exakt
//! demselben Schnitt: [`propose_tool_calling_downgrades`] ist eine reine
//! Funktion, keine `StoredJob`-Implementierung. Ein Scheduler, der sie
//! periodisch mit echten Beobachtungen aufruft und das Ergebnis über
//! `JobStore` durabel macht, ist ein eigener, noch offener Knoten — dieselbe
//! Lücke wie in Punkt 2 oben, hier nur bezogen auf die *Ausführung* statt auf
//! die *Eingabedaten*.
//!
//! # Gewählte Sichtbarkeit: `OperatorOnly`
//! Wie [`crate::context_proposal::RECOMMENDED_VISIBILITY`]: ein Vorschlag, der
//! aus beobachtetem Modellverhalten entsteht, verrät, was das System über die
//! Zuverlässigkeit der von ihm selbst verwendeten Modelle weiß — und ein
//! Vorschlag zur Änderung der eigenen Steuerungsfläche (hier: welche
//! Modell-Fähigkeit das System als gegeben behandelt) ist per Definition eine
//! Operator-Governance-Frage, kein agentenseitig konsumierbares
//! Wissensfragment. [`RECOMMENDED_VISIBILITY`] hält das fest; durchgesetzt wird
//! es — wie bei jeder anderen Sichtbarkeit in dieser Crate — ausschließlich in
//! [`crate::memory::recall::search_with`], nicht hier (siehe die Tests in
//! diesem Modul, die exakt das prüfen, inklusive des Rückverweis-Sprungs, der
//! K35 war).
//!
//! # Eigene `ArtifactKind` statt einer `ContextProposal`-Variante — Begründung
//! [`ProposedModelChange`] ist inhaltlich etwas anderes als
//! [`crate::context_proposal::ProposedChange`]: eine Katalogbehauptung über
//! ein Modell (`tool_calling`, `context_window`, …) hat keine Beziehung zu
//! einer Kontextprogramm-Sektion (`RawContextSectionSpec`, `exclude`-Muster).
//! Eine gemeinsame `ProposedChange`-Enum über beide Domänen hinweg wäre eine
//! „irgendwas ändern"-Variante mit Freitextfeldern gewesen — genau der Fehler,
//! den K40 bei `StructureDrift` bereits einmal fand: nicht maschinell prüfbar,
//! weil das Ziel-Vokabular pro Variante unterschiedlich ist. Eine **eigene**
//! `ArtifactKind::ModelBehaviorProposal` hält das Vokabular typisiert und lässt
//! den Index weiterhin nach Art filtern (§8.1 in `knowledge-surfaces.md`),
//! ohne dass [`crate::context_proposal::ProposedChange`] mit modell-fremden
//! Varianten aufgebläht wird.
//!
//! Was dagegen **wiederverwendet** wird, ist die gesamte Lebenszyklus- und
//! Artefakt-Mechanik von AW5-09, statt sie ein zweites Mal zu bauen:
//! - [`crate::context_proposal::ProposalStatus`] — derselbe
//!   `Pending`/`Accepted`/`Rejected`-Lebenszyklus, importiert statt dupliziert.
//!   Bewusst **kein** `PalaceStatus` — dieselbe Begründung wie in
//!   [`crate::context_proposal`]s Moduldoku: ein Vorschlag ist keine
//!   etablierte Wahrheit, sondern eine markierte Prüfentscheidung.
//! - Dieselbe generische [`crate::artifact::KnowledgeArtifact`]-Einbettung
//!   (Body = kompaktes JSON) über [`ModelBehaviorProposal::to_artifact`] /
//!   [`ModelBehaviorProposal::from_artifact`].
//! - Dieselbe Belegkette `Vec<harw_plan::EvidenceRef>` (seit AW4-04 mit
//!   `digest: Option<harw_types::ContentDigest>`) — unverändert vom Aufrufer
//!   hereingereicht, nicht hier synthetisiert (dieselbe Begründung wie in
//!   [`crate::context_proposal`]: ein `EvidenceRef` selbst zu bauen würde
//!   `time::OffsetDateTime` verlangen, ohne dass dieser Knoten eine neue
//!   externe Abhängigkeit erfinden darf).
//!
//! # Wo Sichtbarkeit durchgesetzt wird
//! Dieses Modul öffnet **keinen** zweiten, ungeprüften Lesepfad. Ein
//! `ModelBehaviorProposal` läuft nach der Einbettung über
//! [`ModelBehaviorProposal::to_artifact`] durch exakt dieselbe Maschinerie wie
//! jede andere Sichtbarkeit in dieser Crate: [`crate::index::KnowledgeIndex`]
//! kennt nur `ArtifactKind`/`VisibilityScope`/Tags/Body, nie den konkreten Typ
//! dahinter, und [`crate::memory::recall::search_with`] ist die einzige
//! Stelle, die `VisibilityScope` prüft — bei jedem direkten Treffer und bei
//! jedem Rückverweis-Sprung erneut.
//!
//! # Exportierte Typen
//! [`ModelBehaviorProposal`], [`ProposedModelChange`],
//! [`ObservedToolCallingReliability`], [`ModelBehaviorObservationWindow`],
//! [`MIN_RELIABLE_SAMPLE_COUNT`], [`TOOL_SCHEMA_UNRELIABLE_THRESHOLD`],
//! [`RECOMMENDED_VISIBILITY`], [`propose_tool_calling_downgrades`].
//!
//! # Concurrency
//! Reine `Send + Sync`-Werttypen; keine innere Veränderlichkeit, keine
//! Threads, keine I/O in diesem Modul.
//!
//! # Fehler
//! Fallible Pfade liefern [`crate::error::KnowledgeError`] /
//! [`crate::error::KnowledgeResult`], wie jede andere Oberfläche dieser Crate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use harw_model_catalog::descriptor::{TokenCount, ToolCallingSupport};
use harw_model_catalog::observed::Score;
use harw_plan::EvidenceRef;
use harw_types::{ModelId, ProviderId};

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::context_proposal::ProposalStatus;
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::visibility::VisibilityScope;

/// Mindestanzahl an Beobachtungen (Evaluierungs-Samples), ab der
/// `tool_schema_reliability` als belastbar genug gilt, um eine Herabstufung
/// vorzuschlagen (siehe Moduldoku, „Die Heuristik in Worten" — dieselbe
/// Schwelle, ab der `harw_model_catalog::provenance::MetricEstimate` zwischen
/// `Insufficient` und `Measured` unterscheidet).
pub const MIN_RELIABLE_SAMPLE_COUNT: u32 = 5;

/// Score-Schwelle (0..=100), unterhalb derer `tool_schema_reliability` als
/// unzuverlässig genug gilt, um eine Herabstufung vorzuschlagen.
pub const TOOL_SCHEMA_UNRELIABLE_THRESHOLD: u8 = 40;

/// Empfohlene Sichtbarkeit für `ModelBehaviorProposal`-Artefakte (siehe
/// Moduldoku, „Gewählte Sichtbarkeit: `OperatorOnly`").
pub const RECOMMENDED_VISIBILITY: VisibilityScope = VisibilityScope::OperatorOnly;

/// Eine einzelne, maschinell prüfbare Änderung an einer Modellkatalog-Behauptung.
///
/// # Description
/// Bildet konkrete, typisierte Stellen ab, an denen ein
/// [`harw_model_catalog::descriptor::ModelDescriptor`] veränderlich ist.
/// `DowngradeToolCalling` bettet [`ToolCallingSupport`] direkt ein (keine
/// Freitext-Beschreibung der Fähigkeit), `LowerContextWindowClaim` bettet
/// [`TokenCount`] direkt ein. Ein Vorschlag, dessen Inhalt Freitext wäre, wäre
/// von einer Notiz nicht zu unterscheiden und maschinell nicht prüfbar
/// (derselbe Fehler, den K40 bei `StructureDrift` fand); jede Variante hier
/// ist stattdessen ein konkretes, typisiertes Datum. Nur
/// [`ProposedModelChange::DowngradeToolCalling`] wird heute von
/// [`propose_tool_calling_downgrades`] erzeugt — `LowerContextWindowClaim`
/// steht als typisiertes Ziel für einen von Hand angelegten Vorschlag bereit,
/// analog zu den ungenutzten Varianten in
/// [`crate::context_proposal::ProposedChange`].
///
/// # Examples
/// ```rust
/// use harw_knowledge::model_behavior_proposal::ProposedModelChange;
/// use harw_model_catalog::descriptor::ToolCallingSupport;
///
/// let change = ProposedModelChange::DowngradeToolCalling {
///     to: ToolCallingSupport::Basic,
/// };
/// let json = serde_json::to_string(&change).expect("change serializes");
/// let restored: ProposedModelChange = serde_json::from_str(&json).expect("change deserializes");
/// assert_eq!(change, restored);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProposedModelChange {
    /// Die behauptete Tool-Calling-Unterstützung um eine Stufe herabsetzen.
    DowngradeToolCalling {
        /// Die vorgeschlagene, herabgesetzte Stufe.
        to: ToolCallingSupport,
    },
    /// Das behauptete Kontextfenster auf einen niedrigeren Wert senken.
    LowerContextWindowClaim {
        /// Die vorgeschlagene, niedrigere Kontextfenstergröße in Tokens.
        to: TokenCount,
    },
}

/// Ein Vorschlag zur Änderung einer Modellkatalog-Behauptung (Knoten AW6-06).
///
/// # Description
/// Trägt alles, was ein Mensch braucht, um den Vorschlag zu beurteilen, ohne
/// Code zu lesen: worauf er sich bezieht ([`Self::target_provider`],
/// [`Self::target_model`], [`Self::target_descriptor_digest`]), was er
/// strukturiert vorschlägt ([`Self::changes`]), warum ([`Self::evidence`]),
/// wann und woher ([`Self::generated_at`], [`Self::produced_by`] — beide
/// hereingereicht, nie aus der Systemuhr gelesen) und in welchem Prüfstatus er
/// steht ([`Self::status`]). Trägt **keine** `apply`-Methode (siehe Moduldoku).
///
/// # Warum `target_descriptor_digest: String`
/// Dieselbe Entscheidung wie
/// [`crate::context_proposal::ContextProposal::target_snapshot_digest`]:
/// `harw_model_catalog::descriptor::ModelDescriptor` hat keinen stabilen,
/// serialisierbaren Fassungs-Bezeichner außer seinem eigenen, vollständigen
/// Inhalt. Statt hier einen Digest zu berechnen (was eine neue Hash-Crate-
/// Abhängigkeit verlangen würde, die dieser Knoten laut Auftrag nicht selbst
/// erfinden darf), übergibt der Aufrufer eine bereits fertig gebildete
/// Zeichenkette, die die beobachtete Fassung identifiziert.
///
/// # Concurrency
/// `Send + Sync`; reiner Werttyp, keine innere Veränderlichkeit.
///
/// # Examples
/// ```rust
/// use harw_knowledge::model_behavior_proposal::{ModelBehaviorProposal, ProposedModelChange};
/// use harw_model_catalog::descriptor::ToolCallingSupport;
/// use harw_knowledge::ArtifactId;
/// use harw_types::{ModelId, ProviderId};
///
/// let proposal = ModelBehaviorProposal::new(
///     ArtifactId::new("model-behavior-proposal/example"),
///     "Tool-Schema wiederholt verletzt",
///     ProviderId::from("openai"),
///     ModelId::from("gpt-5"),
///     "deadbeef".repeat(8),
///     vec![ProposedModelChange::DowngradeToolCalling {
///         to: ToolCallingSupport::Basic,
///     }],
///     Vec::new(),
///     jiff::Timestamp::UNIX_EPOCH,
///     "heuristic:tool-calling-downgrade@1",
/// );
/// assert_eq!(proposal.status, harw_knowledge::context_proposal::ProposalStatus::Pending);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelBehaviorProposal {
    /// Stabile Artefakt-Id (`model-behavior-proposal/<slug>`).
    pub id: ArtifactId,
    /// Menschenlesbare Kurzfassung. [`Self::changes`] ist maschinell prüfbar,
    /// dieser Titel ist nur eine Beschriftung dafür.
    pub title: String,
    /// Welcher Provider betroffen ist.
    pub target_provider: ProviderId,
    /// Welches Modell betroffen ist.
    pub target_model: ModelId,
    /// Welche Fassung des Katalogeintrags beobachtet wurde — siehe „Warum
    /// `target_descriptor_digest: String`" oben.
    pub target_descriptor_digest: String,
    /// Die vorgeschlagenen Änderungen, strukturiert (nicht Prosa).
    pub changes: Vec<ProposedModelChange>,
    /// Die Belege, auf denen dieser Vorschlag beruht.
    pub evidence: Vec<EvidenceRef>,
    /// Wann dieser Vorschlag erzeugt wurde — hereingereicht, nie
    /// `jiff::Timestamp::now()`.
    pub generated_at: jiff::Timestamp,
    /// Woher dieser Vorschlag stammt (z. B.
    /// `"heuristic:tool-calling-downgrade@1"` oder `"human:<operator>"`) —
    /// hereingereicht, nie aus einem Prozesskontext erraten.
    pub produced_by: String,
    /// Prüfstatus — wiederverwendet aus [`crate::context_proposal::ProposalStatus`]
    /// (siehe Moduldoku, „Eigene `ArtifactKind`… — Begründung").
    pub status: ProposalStatus,
    /// Topische Tags, unabhängig vom Link-Graph.
    pub tags: Vec<String>,
}

impl ModelBehaviorProposal {
    /// Baut einen neuen, `Pending`-Vorschlag aus seinen Bestandteilen.
    ///
    /// # Description
    /// Reine Feldzuweisung, keine Validierung über „ist `changes` nicht leer"
    /// hinaus — konsistent mit
    /// [`crate::context_proposal::ContextProposal::new`] und den übrigen
    /// Konstruktoren dieser Crate. `tags` startet leer.
    ///
    /// # Arguments
    /// - `id` (`ArtifactId`): stabile Artefakt-Id.
    /// - `title` (`impl Into<String>`): menschenlesbare Kurzfassung.
    /// - `target_provider` (`ProviderId`): betroffener Provider.
    /// - `target_model` (`ModelId`): betroffenes Modell.
    /// - `target_descriptor_digest` (`impl Into<String>`): beobachtete Fassung.
    /// - `changes` (`Vec<ProposedModelChange>`): strukturierte Änderungen.
    /// - `evidence` (`Vec<EvidenceRef>`): Belege.
    /// - `generated_at` (`jiff::Timestamp`): Erzeugungszeitpunkt, hereingereicht.
    /// - `produced_by` (`impl Into<String>`): Herkunft, hereingereicht.
    ///
    /// # Returns
    /// Einen neuen Vorschlag mit `status = ProposalStatus::Pending` und
    /// leeren `tags`.
    ///
    /// # Examples
    /// Siehe die Typdokumentation von [`ModelBehaviorProposal`].
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ArtifactId,
        title: impl Into<String>,
        target_provider: ProviderId,
        target_model: ModelId,
        target_descriptor_digest: impl Into<String>,
        changes: Vec<ProposedModelChange>,
        evidence: Vec<EvidenceRef>,
        generated_at: jiff::Timestamp,
        produced_by: impl Into<String>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            target_provider,
            target_model,
            target_descriptor_digest: target_descriptor_digest.into(),
            changes,
            evidence,
            generated_at,
            produced_by: produced_by.into(),
            status: ProposalStatus::Pending,
            tags: Vec::new(),
        }
    }

    /// Markiert diesen Vorschlag als akzeptiert.
    ///
    /// # Description
    /// Schreibt **ausschließlich** [`Self::status`] auf
    /// [`ProposalStatus::Accepted`]. Es gibt keinen Aufruf in dieser Funktion
    /// — und keine andere Funktion in diesem Modul —, der daraufhin einen
    /// `ModelDescriptor` liest, parst oder schreibt. Das ist der strukturelle
    /// Beweis für „annehmen markiert, wendet nicht an" (siehe auch den Test
    /// `test_accept_never_touches_any_model_descriptor` in diesem Modul).
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// [`KnowledgeError::ModelProposalNotPending`], wenn [`Self::status`]
    /// nicht [`ProposalStatus::Pending`] ist — eine bereits entschiedene
    /// Vorlage wird nicht erneut entschieden.
    ///
    /// # Examples
    /// ```rust
    /// # use harw_knowledge::model_behavior_proposal::ModelBehaviorProposal;
    /// # use harw_knowledge::ArtifactId;
    /// # use harw_types::{ModelId, ProviderId};
    /// let mut proposal = ModelBehaviorProposal::new(
    ///     ArtifactId::new("model-behavior-proposal/example"),
    ///     "Beispiel",
    ///     ProviderId::from("openai"),
    ///     ModelId::from("gpt-5"),
    ///     "deadbeef".repeat(8),
    ///     Vec::new(),
    ///     Vec::new(),
    ///     jiff::Timestamp::UNIX_EPOCH,
    ///     "human:operator",
    /// );
    /// proposal.accept().expect("pending proposal accepts");
    /// assert!(proposal.accept().is_err(), "ein zweites Mal entscheiden ist ein Fehler");
    /// ```
    pub fn accept(&mut self) -> KnowledgeResult<()> {
        self.transition_to(ProposalStatus::Accepted)
    }

    /// Markiert diesen Vorschlag als abgelehnt.
    ///
    /// # Description
    /// Spiegelbild von [`Self::accept`] — derselbe strukturelle Beweis gilt:
    /// es wird ausschließlich [`Self::status`] geschrieben.
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// [`KnowledgeError::ModelProposalNotPending`], wenn [`Self::status`]
    /// nicht [`ProposalStatus::Pending`] ist.
    ///
    /// # Examples
    /// ```rust
    /// # use harw_knowledge::model_behavior_proposal::{ModelBehaviorProposal};
    /// # use harw_knowledge::context_proposal::ProposalStatus;
    /// # use harw_knowledge::ArtifactId;
    /// # use harw_types::{ModelId, ProviderId};
    /// let mut proposal = ModelBehaviorProposal::new(
    ///     ArtifactId::new("model-behavior-proposal/example"),
    ///     "Beispiel",
    ///     ProviderId::from("openai"),
    ///     ModelId::from("gpt-5"),
    ///     "deadbeef".repeat(8),
    ///     Vec::new(),
    ///     Vec::new(),
    ///     jiff::Timestamp::UNIX_EPOCH,
    ///     "human:operator",
    /// );
    /// proposal.reject().expect("pending proposal rejects");
    /// assert_eq!(proposal.status, ProposalStatus::Rejected);
    /// ```
    pub fn reject(&mut self) -> KnowledgeResult<()> {
        self.transition_to(ProposalStatus::Rejected)
    }

    /// Gemeinsame Übergangslogik für [`Self::accept`]/[`Self::reject`].
    fn transition_to(&mut self, to: ProposalStatus) -> KnowledgeResult<()> {
        if self.status != ProposalStatus::Pending {
            return Err(KnowledgeError::ModelProposalNotPending {
                id: self.id.to_string(),
                status: format!("{:?}", self.status),
            });
        }
        self.status = to;
        Ok(())
    }

    /// Bettet diesen Vorschlag als generisches [`KnowledgeArtifact`] ein.
    ///
    /// # Description
    /// Der Body trägt eine kompakte JSON-Serialisierung von `self` — dieselbe
    /// generische Einbettung, über die jede Sichtbarkeitsprüfung dieser Crate
    /// läuft. `frontmatter.visibility` sollte [`RECOMMENDED_VISIBILITY`] sein,
    /// sofern kein spezifischerer Grund dagegenspricht; diese Funktion
    /// erzwingt das nicht (die Sichtbarkeit bleibt Entscheidung des
    /// Aufrufers, wie überall in dieser Crate).
    ///
    /// # Arguments
    /// - `frontmatter` (`Frontmatter`): die anzuhängende Frontmatter
    ///   (Sichtbarkeit, Tags, Links, Autor).
    ///
    /// # Returns
    /// Ein [`KnowledgeArtifact`] mit `kind = ArtifactKind::ModelBehaviorProposal`.
    ///
    /// # Errors
    /// [`KnowledgeError::Json`], wenn die JSON-Serialisierung fehlschlägt (bei
    /// diesem Typ praktisch ausgeschlossen — keine `f64`/`NaN`-Felder).
    ///
    /// # Examples
    /// ```rust
    /// # use harw_knowledge::model_behavior_proposal::{ModelBehaviorProposal, RECOMMENDED_VISIBILITY};
    /// # use harw_knowledge::{AgentId, ArtifactId, ArtifactKind, Frontmatter};
    /// # use harw_types::{ModelId, ProviderId};
    /// let proposal = ModelBehaviorProposal::new(
    ///     ArtifactId::new("model-behavior-proposal/example"),
    ///     "Beispiel",
    ///     ProviderId::from("openai"),
    ///     ModelId::from("gpt-5"),
    ///     "deadbeef".repeat(8),
    ///     Vec::new(),
    ///     Vec::new(),
    ///     jiff::Timestamp::UNIX_EPOCH,
    ///     "human:operator",
    /// );
    /// let frontmatter = Frontmatter::new(
    ///     AgentId::new("system"),
    ///     RECOMMENDED_VISIBILITY,
    ///     jiff::Timestamp::UNIX_EPOCH,
    /// );
    /// let artifact = proposal.to_artifact(frontmatter).expect("proposal serializes");
    /// assert_eq!(artifact.kind, ArtifactKind::ModelBehaviorProposal);
    /// ```
    pub fn to_artifact(&self, frontmatter: Frontmatter) -> KnowledgeResult<KnowledgeArtifact> {
        let body = serde_json::to_string(self)?;
        Ok(KnowledgeArtifact::new(
            self.id.clone(),
            ArtifactKind::ModelBehaviorProposal,
            frontmatter,
            body,
        ))
    }

    /// Liest einen Vorschlag aus einem generischen [`KnowledgeArtifact`] zurück.
    ///
    /// # Description
    /// Inverse von [`Self::to_artifact`]. Prüft zuerst `artifact.kind`, dann
    /// parst den Body als JSON.
    ///
    /// # Arguments
    /// - `artifact` (`&KnowledgeArtifact`): das zu lesende Artefakt.
    ///
    /// # Returns
    /// Den rekonstruierten [`ModelBehaviorProposal`].
    ///
    /// # Errors
    /// - [`KnowledgeError::ArtifactKindMismatch`], wenn `artifact.kind` nicht
    ///   [`ArtifactKind::ModelBehaviorProposal`] ist.
    /// - [`KnowledgeError::Json`], wenn der Body kein gültiges, dem Schema
    ///   entsprechendes JSON ist (unbekannte Felder eingeschlossen —
    ///   `#[serde(deny_unknown_fields)]`).
    ///
    /// # Examples
    /// ```rust
    /// # use harw_knowledge::model_behavior_proposal::{ModelBehaviorProposal, RECOMMENDED_VISIBILITY};
    /// # use harw_knowledge::{AgentId, ArtifactId, Frontmatter};
    /// # use harw_types::{ModelId, ProviderId};
    /// let proposal = ModelBehaviorProposal::new(
    ///     ArtifactId::new("model-behavior-proposal/example"),
    ///     "Beispiel",
    ///     ProviderId::from("openai"),
    ///     ModelId::from("gpt-5"),
    ///     "deadbeef".repeat(8),
    ///     Vec::new(),
    ///     Vec::new(),
    ///     jiff::Timestamp::UNIX_EPOCH,
    ///     "human:operator",
    /// );
    /// let frontmatter = Frontmatter::new(
    ///     AgentId::new("system"),
    ///     RECOMMENDED_VISIBILITY,
    ///     jiff::Timestamp::UNIX_EPOCH,
    /// );
    /// let artifact = proposal.to_artifact(frontmatter).expect("proposal serializes");
    /// let restored = ModelBehaviorProposal::from_artifact(&artifact).expect("proposal parses back");
    /// assert_eq!(restored.id, proposal.id);
    /// ```
    pub fn from_artifact(artifact: &KnowledgeArtifact) -> KnowledgeResult<Self> {
        if artifact.kind != ArtifactKind::ModelBehaviorProposal {
            return Err(KnowledgeError::ArtifactKindMismatch {
                id: artifact.id.to_string(),
                expected: "model_behavior_proposal".to_owned(),
                actual: format!("{:?}", artifact.kind),
            });
        }
        Ok(serde_json::from_str(&artifact.body)?)
    }
}

/// Eine einzelne, von außen eingereichte Beobachtung: das gemessene
/// Tool-Schema-Verhalten eines konkreten Modells, mitsamt der Katalog-
/// behauptung, gegen die es geprüft wird, und den Belegen, die diese Messung
/// stützen.
///
/// # Description
/// Bündelt reales Vokabular ([`Score`], [`ToolCallingSupport`], bereits über
/// bestehende `harw-model-catalog`-Typen erreichbar) mit den Feldern, die
/// [`propose_tool_calling_downgrades`] zur Aggregation und Belegbildung
/// braucht. `claimed_tool_calling` wird bewusst hier mitgeführt statt aus
/// einem `ModelDescriptor`-Lookup nachgeschlagen: die Heuristik bleibt damit
/// vollständig deterministisch und unabhängig davon, ob der aktuell installierte
/// Katalog denselben Stand hat, den der Aufrufer beobachtet hat (derselbe
/// Grund, aus dem [`ModelBehaviorProposal::target_descriptor_digest`] die
/// beobachtete Fassung separat festhält).
///
/// # Examples
/// ```rust
/// use harw_knowledge::model_behavior_proposal::ObservedToolCallingReliability;
/// use harw_model_catalog::descriptor::ToolCallingSupport;
/// use harw_model_catalog::observed::Score;
/// use harw_types::{ModelId, ProviderId};
///
/// let observation = ObservedToolCallingReliability {
///     provider: ProviderId::from("openai"),
///     model: ModelId::from("gpt-5"),
///     claimed_tool_calling: ToolCallingSupport::Parallel,
///     tool_schema_reliability: Score::clamp(20),
///     sample_count: 6,
///     target_descriptor_digest: "deadbeef".repeat(8),
///     evidence: Vec::new(),
/// };
/// assert_eq!(observation.sample_count, 6);
/// ```
#[derive(Debug, Clone)]
pub struct ObservedToolCallingReliability {
    /// Betroffener Provider.
    pub provider: ProviderId,
    /// Betroffenes Modell.
    pub model: ModelId,
    /// Die aktuell im Katalog behauptete Tool-Calling-Stufe.
    pub claimed_tool_calling: ToolCallingSupport,
    /// Der gemessene Tool-Schema-Zuverlässigkeitswert.
    pub tool_schema_reliability: Score,
    /// Wie viele Evaluierungs-Läufe hinter diesem Wert stehen.
    pub sample_count: u32,
    /// Welche Fassung des Katalogeintrags beobachtet wurde.
    pub target_descriptor_digest: String,
    /// Die Belege, die diese Beobachtung stützen.
    pub evidence: Vec<EvidenceRef>,
}

/// Das explizite, vom Aufrufer gefüllte Beobachtungsfenster, gegen das
/// [`propose_tool_calling_downgrades`] läuft (siehe Moduldoku, „Woher die
/// beobachteten Daten kommen").
///
/// # Examples
/// ```rust
/// use harw_knowledge::model_behavior_proposal::{
///     ModelBehaviorObservationWindow, ObservedToolCallingReliability,
/// };
/// use harw_model_catalog::descriptor::ToolCallingSupport;
/// use harw_model_catalog::observed::Score;
/// use harw_types::{ModelId, ProviderId};
///
/// let window = ModelBehaviorObservationWindow {
///     observations: vec![ObservedToolCallingReliability {
///         provider: ProviderId::from("openai"),
///         model: ModelId::from("gpt-5"),
///         claimed_tool_calling: ToolCallingSupport::Parallel,
///         tool_schema_reliability: Score::clamp(10),
///         sample_count: 5,
///         target_descriptor_digest: "deadbeef".repeat(8),
///         evidence: Vec::new(),
///     }],
/// };
/// assert_eq!(window.observations.len(), 1);
/// ```
#[derive(Debug, Clone, Default)]
pub struct ModelBehaviorObservationWindow {
    /// Die einzelnen Beobachtungen in diesem Fenster, in beliebiger
    /// Reihenfolge — die Heuristik sortiert selbst (siehe Moduldoku). Enthält
    /// das Fenster mehrere Beobachtungen für dasselbe (Provider, Modell)-Paar,
    /// gewinnt deterministisch die zuletzt im `Vec` stehende (letzte
    /// Einfügung gewinnt, wie bei einer Kartenaktualisierung).
    pub observations: Vec<ObservedToolCallingReliability>,
}

/// Setzt eine Tool-Calling-Stufe um genau eine Stufe herab.
///
/// `None` ist bereits die niedrigste Stufe und wird nicht weiter herabgesetzt
/// (`None` zurück).
fn downgrade_one_step(current: ToolCallingSupport) -> Option<ToolCallingSupport> {
    match current {
        ToolCallingSupport::Native => Some(ToolCallingSupport::Parallel),
        ToolCallingSupport::Parallel => Some(ToolCallingSupport::Basic),
        ToolCallingSupport::Basic => Some(ToolCallingSupport::None),
        ToolCallingSupport::None => None,
    }
}

/// Leitet — deterministisch — Vorschläge aus einem Beobachtungsfenster ab:
/// Modelle, deren Tool-Schema-Zuverlässigkeit belastbar niedrig gemessen
/// wurde, auf eine niedrigere `tool_calling`-Stufe herabsetzen.
///
/// # Description
/// Siehe Moduldoku, „Die Heuristik in Worten", für die Regel in einem Satz.
/// Dedupliziert Beobachtungen je (Provider, Modell)-Paar über eine
/// `BTreeMap` (nie eine `HashMap`), damit die Ausgabereihenfolge — und damit
/// die serialisierte Byte-Folge — bei gleicher Eingabe immer identisch ist.
/// Enthält weder eine Systemuhr noch eine Zufallszahl noch einen
/// Modellaufruf: `generated_at` und `produced_by` kommen vollständig vom
/// Aufrufer, `evidence` wird unverändert je Beobachtung durchgereicht.
///
/// # Arguments
/// - `window` (`&ModelBehaviorObservationWindow`): die Beobachtungen.
/// - `generated_at` (`jiff::Timestamp`): Erzeugungszeitpunkt, hereingereicht.
/// - `produced_by` (`impl Into<String>`): Herkunftskennung, z. B.
///   `"heuristic:tool-calling-downgrade@1"`.
///
/// # Returns
/// Einen `Vec<ModelBehaviorProposal>` mit genau einem Vorschlag je Modell, das
/// die Schwelle erreicht und dessen behauptete Stufe über `None` liegt,
/// sortiert nach (Provider, Modell). Ein leeres Beobachtungsfenster liefert
/// einen leeren `Vec` — **nie** einen Vorschlag aus dem Nichts.
///
/// # Errors
/// Diese Funktion ist total; sie liefert kein `Result`.
///
/// # Examples
/// ```rust
/// use harw_knowledge::model_behavior_proposal::{
///     ModelBehaviorObservationWindow, ObservedToolCallingReliability, ProposedModelChange,
///     propose_tool_calling_downgrades,
/// };
/// use harw_model_catalog::descriptor::ToolCallingSupport;
/// use harw_model_catalog::observed::Score;
/// use harw_types::{ModelId, ProviderId};
///
/// let window = ModelBehaviorObservationWindow {
///     observations: vec![ObservedToolCallingReliability {
///         provider: ProviderId::from("openai"),
///         model: ModelId::from("gpt-5"),
///         claimed_tool_calling: ToolCallingSupport::Parallel,
///         tool_schema_reliability: Score::clamp(10),
///         sample_count: 5,
///         target_descriptor_digest: "deadbeef".repeat(8),
///         evidence: Vec::new(),
///     }],
/// };
/// let proposals = propose_tool_calling_downgrades(
///     &window,
///     jiff::Timestamp::UNIX_EPOCH,
///     "heuristic:tool-calling-downgrade@1",
/// );
/// assert_eq!(proposals.len(), 1);
/// assert!(matches!(
///     proposals[0].changes[0],
///     ProposedModelChange::DowngradeToolCalling { to: ToolCallingSupport::Basic }
/// ));
///
/// // Leere Beobachtung: kein Vorschlag aus dem Nichts.
/// let empty = ModelBehaviorObservationWindow::default();
/// assert!(propose_tool_calling_downgrades(
///     &empty,
///     jiff::Timestamp::UNIX_EPOCH,
///     "heuristic:tool-calling-downgrade@1",
/// )
/// .is_empty());
/// ```
#[must_use]
pub fn propose_tool_calling_downgrades(
    window: &ModelBehaviorObservationWindow,
    generated_at: jiff::Timestamp,
    produced_by: impl Into<String>,
) -> Vec<ModelBehaviorProposal> {
    let produced_by = produced_by.into();

    let mut by_key: BTreeMap<(ProviderId, ModelId), &ObservedToolCallingReliability> =
        BTreeMap::new();
    for observation in &window.observations {
        by_key.insert(
            (observation.provider.clone(), observation.model.clone()),
            observation,
        );
    }

    by_key
        .into_iter()
        .filter_map(|((provider, model), observation)| {
            if observation.sample_count < MIN_RELIABLE_SAMPLE_COUNT {
                return None;
            }
            if observation.tool_schema_reliability.get() >= TOOL_SCHEMA_UNRELIABLE_THRESHOLD {
                return None;
            }
            let downgraded_to = downgrade_one_step(observation.claimed_tool_calling)?;

            let id = ArtifactId::new(format!(
                "model-behavior-proposal/{provider}-{model}-tool-calling-downgrade"
            ));
            let title = format!(
                "{provider}/{model}: tool_schema_reliability {} < {} über {} Beobachtungen — tool_calling auf {downgraded_to:?} herabstufen",
                observation.tool_schema_reliability.get(),
                TOOL_SCHEMA_UNRELIABLE_THRESHOLD,
                observation.sample_count,
            );

            Some(ModelBehaviorProposal::new(
                id,
                title,
                provider,
                model,
                observation.target_descriptor_digest.clone(),
                vec![ProposedModelChange::DowngradeToolCalling { to: downgraded_to }],
                observation.evidence.clone(),
                generated_at,
                produced_by.clone(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ModelBehaviorObservationWindow, ModelBehaviorProposal, ObservedToolCallingReliability,
        ProposedModelChange, RECOMMENDED_VISIBILITY, propose_tool_calling_downgrades,
    };
    use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter};
    use crate::context_proposal::ProposalStatus;
    use crate::error::KnowledgeError;
    use crate::index::KnowledgeIndex;
    use crate::memory::recall::{ListAllRanker, search_with};
    use crate::visibility::{AgentId, VisibilityScope};

    use harw_model_catalog::descriptor::ToolCallingSupport;
    use harw_model_catalog::observed::Score;
    use harw_types::{ModelId, ProviderId};

    fn sample_proposal() -> ModelBehaviorProposal {
        ModelBehaviorProposal::new(
            ArtifactId::new("model-behavior-proposal/example"),
            "Beispielvorschlag",
            ProviderId::from("openai"),
            ModelId::from("gpt-5"),
            "deadbeef".repeat(8),
            vec![ProposedModelChange::DowngradeToolCalling {
                to: ToolCallingSupport::Basic,
            }],
            Vec::new(),
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:tool-calling-downgrade@1",
        )
    }

    fn qualifying_observation() -> ObservedToolCallingReliability {
        ObservedToolCallingReliability {
            provider: ProviderId::from("openai"),
            model: ModelId::from("gpt-5"),
            claimed_tool_calling: ToolCallingSupport::Parallel,
            tool_schema_reliability: Score::clamp(10),
            sample_count: super::MIN_RELIABLE_SAMPLE_COUNT,
            target_descriptor_digest: "deadbeef".repeat(8),
            evidence: Vec::new(),
        }
    }

    /// Rundlauf über serde; unbekanntes Feld wird abgelehnt (K19).
    #[test]
    fn test_serde_roundtrip_and_deny_unknown_fields() {
        let proposal = sample_proposal();
        let json = serde_json::to_string(&proposal).expect("proposal serializes");
        let restored: ModelBehaviorProposal =
            serde_json::from_str(&json).expect("proposal deserializes");

        assert_eq!(restored.id, proposal.id);
        assert_eq!(restored.title, proposal.title);
        assert_eq!(restored.target_provider, proposal.target_provider);
        assert_eq!(restored.status, proposal.status);
        assert_eq!(restored.changes, proposal.changes);

        let mut value: serde_json::Value = serde_json::from_str(&json).expect("value parses");
        value
            .as_object_mut()
            .expect("proposal is a JSON object")
            .insert("unexpected_field".to_owned(), serde_json::json!(true));
        let rejected: Result<ModelBehaviorProposal, _> = serde_json::from_str(&value.to_string());
        assert!(
            rejected.is_err(),
            "ein unbekanntes Feld muss die Deserialisierung ablehnen"
        );
    }

    /// Rundlauf über `KnowledgeArtifact` (die durable Einbettung), zusätzlich
    /// zum reinen Serde-Rundlauf oben.
    #[test]
    fn test_artifact_roundtrip_preserves_kind_and_id() {
        let proposal = sample_proposal();
        let frontmatter = Frontmatter::new(
            AgentId::new("system"),
            RECOMMENDED_VISIBILITY,
            jiff::Timestamp::UNIX_EPOCH,
        );
        let artifact = proposal
            .clone()
            .to_artifact(frontmatter)
            .expect("proposal embeds into an artifact");
        assert_eq!(artifact.kind, ArtifactKind::ModelBehaviorProposal);

        let restored = ModelBehaviorProposal::from_artifact(&artifact)
            .expect("artifact parses back into a proposal");
        assert_eq!(restored.id, proposal.id);
        assert_eq!(restored.changes, proposal.changes);
    }

    /// `from_artifact` weist ein Artefakt mit dem falschen `kind` ab.
    #[test]
    fn test_from_artifact_rejects_kind_mismatch() {
        use crate::artifact::KnowledgeArtifact;

        let frontmatter = Frontmatter::new(
            AgentId::new("system"),
            VisibilityScope::SelfOnly,
            jiff::Timestamp::UNIX_EPOCH,
        );
        let wrong_kind = KnowledgeArtifact::new(
            ArtifactId::new("topic/unrelated"),
            ArtifactKind::TopicMemory,
            frontmatter,
            "{}",
        );

        let error = ModelBehaviorProposal::from_artifact(&wrong_kind)
            .expect_err("wrong artifact kind must be rejected");
        assert!(matches!(error, KnowledgeError::ArtifactKindMismatch { .. }));
    }

    /// Determinismus: gleiche Eingabe → byte-gleicher Vorschlag, zweimal.
    #[test]
    fn test_heuristic_is_deterministic_byte_for_byte() {
        let window = ModelBehaviorObservationWindow {
            observations: vec![qualifying_observation()],
        };

        let build = || {
            propose_tool_calling_downgrades(
                &window,
                jiff::Timestamp::UNIX_EPOCH,
                "heuristic:tool-calling-downgrade@1",
            )
        };

        let first = serde_json::to_vec(&build()).expect("first proposal list serializes");
        let second = serde_json::to_vec(&build()).expect("second proposal list serializes");
        assert_eq!(
            first, second,
            "identical input must produce byte-identical output"
        );
    }

    /// Die Heuristik schlägt bei leerer Beobachtung nichts vor.
    #[test]
    fn test_heuristic_proposes_nothing_from_empty_observation() {
        let empty = ModelBehaviorObservationWindow::default();
        let proposals = propose_tool_calling_downgrades(
            &empty,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:tool-calling-downgrade@1",
        );
        assert!(proposals.is_empty(), "kein Vorschlag aus dem Nichts");
    }

    /// Unterhalb der Sample-Schwelle: kein Vorschlag.
    #[test]
    fn test_heuristic_below_sample_threshold_proposes_nothing() {
        let mut observation = qualifying_observation();
        observation.sample_count = super::MIN_RELIABLE_SAMPLE_COUNT - 1;
        let window = ModelBehaviorObservationWindow {
            observations: vec![observation],
        };
        let proposals = propose_tool_calling_downgrades(
            &window,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:tool-calling-downgrade@1",
        );
        assert!(proposals.is_empty());
    }

    /// Score oberhalb der Unzuverlässigkeitsschwelle: kein Vorschlag.
    #[test]
    fn test_heuristic_above_score_threshold_proposes_nothing() {
        let mut observation = qualifying_observation();
        observation.tool_schema_reliability =
            Score::clamp(super::TOOL_SCHEMA_UNRELIABLE_THRESHOLD);
        let window = ModelBehaviorObservationWindow {
            observations: vec![observation],
        };
        let proposals = propose_tool_calling_downgrades(
            &window,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:tool-calling-downgrade@1",
        );
        assert!(proposals.is_empty());
    }

    /// Eine bereits `None`-Behauptung wird nicht weiter herabgesetzt.
    #[test]
    fn test_heuristic_does_not_downgrade_below_none() {
        let mut observation = qualifying_observation();
        observation.claimed_tool_calling = ToolCallingSupport::None;
        let window = ModelBehaviorObservationWindow {
            observations: vec![observation],
        };
        let proposals = propose_tool_calling_downgrades(
            &window,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:tool-calling-downgrade@1",
        );
        assert!(proposals.is_empty());
    }

    /// Mehrere qualifizierende Modelle erscheinen sortiert nach (Provider, Modell).
    #[test]
    fn test_heuristic_orders_multiple_qualifying_models() {
        let mut zeta = qualifying_observation();
        zeta.provider = ProviderId::from("zeta-vendor");
        zeta.model = ModelId::from("zeta-model");

        let mut alpha = qualifying_observation();
        alpha.provider = ProviderId::from("alpha-vendor");
        alpha.model = ModelId::from("alpha-model");

        let window = ModelBehaviorObservationWindow {
            observations: vec![zeta, alpha],
        };
        let proposals = propose_tool_calling_downgrades(
            &window,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:tool-calling-downgrade@1",
        );

        assert_eq!(proposals.len(), 2);
        assert_eq!(proposals[0].target_provider, ProviderId::from("alpha-vendor"));
        assert_eq!(proposals[1].target_provider, ProviderId::from("zeta-vendor"));
    }

    /// Ein Vorschlag ist ohne Blick in den Code beurteilbar: die Belege
    /// kommen mit, unverändert durchgereicht.
    #[test]
    fn test_evidence_flows_through_to_the_produced_proposal() {
        use harw_plan::{EvidenceKind, EvidenceRef};

        // `harw_plan::testing::fixture_time()` liefert deterministisch
        // `OffsetDateTime::UNIX_EPOCH` (keine Systemuhr).
        let mut observation = qualifying_observation();
        observation.evidence = vec![EvidenceRef {
            kind: EvidenceKind::Finding,
            locator: "model-observation:openai/gpt-5:tool-schema-violation".to_owned(),
            attached_at: harw_plan::testing::fixture_time(),
            actor: "heuristic:tool-calling-downgrade@1".to_owned(),
            digest: None,
        }];
        let window = ModelBehaviorObservationWindow {
            observations: vec![observation],
        };
        let proposals = propose_tool_calling_downgrades(
            &window,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:tool-calling-downgrade@1",
        );

        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].evidence.len(), 1);
        assert_eq!(
            proposals[0].evidence[0].locator,
            "model-observation:openai/gpt-5:tool-schema-violation"
        );
    }

    /// `accept`/`reject` markieren nur den Status — sie sind terminal.
    #[test]
    fn test_accept_marks_status_and_is_terminal() {
        let mut proposal = sample_proposal();
        assert_eq!(proposal.status, ProposalStatus::Pending);

        proposal.accept().expect("pending proposal accepts");
        assert_eq!(proposal.status, ProposalStatus::Accepted);

        let error = proposal
            .accept()
            .expect_err("an already-decided proposal must not accept again");
        assert!(matches!(
            error,
            KnowledgeError::ModelProposalNotPending { .. }
        ));

        let error = proposal
            .reject()
            .expect_err("an already-decided proposal must not flip to rejected either");
        assert!(matches!(
            error,
            KnowledgeError::ModelProposalNotPending { .. }
        ));
    }

    #[test]
    fn test_reject_marks_status() {
        let mut proposal = sample_proposal();
        proposal.reject().expect("pending proposal rejects");
        assert_eq!(proposal.status, ProposalStatus::Rejected);
    }

    /// Es gibt keinen Weg, einen Vorschlag anzuwenden: `ModelBehaviorProposal`
    /// bietet über die Felder hinaus keine weiteren öffentlichen Methoden als
    /// `new`, `accept`, `reject`, `to_artifact`, `from_artifact` — keine davon
    /// liest oder schreibt einen `ModelDescriptor`. Dieser Test beweist die
    /// eine Hälfte davon strukturell: `accept` ändert nichts außer `status`.
    #[test]
    fn test_accept_never_touches_any_model_descriptor() {
        let mut proposal = sample_proposal();
        let before_changes = proposal.changes.clone();
        let before_provider = proposal.target_provider.clone();
        let before_model = proposal.target_model.clone();
        let before_digest = proposal.target_descriptor_digest.clone();

        proposal.accept().expect("pending proposal accepts");

        assert_eq!(
            proposal.changes, before_changes,
            "accept darf `changes` nicht verändern"
        );
        assert_eq!(
            proposal.target_provider, before_provider,
            "accept darf `target_provider` nicht verändern"
        );
        assert_eq!(
            proposal.target_model, before_model,
            "accept darf `target_model` nicht verändern"
        );
        assert_eq!(
            proposal.target_descriptor_digest, before_digest,
            "accept darf `target_descriptor_digest` nicht verändern"
        );
        assert_eq!(proposal.status, ProposalStatus::Accepted);
    }

    /// Die Sichtbarkeitsregel greift: ein `ModelBehaviorProposal`, gespeichert
    /// mit `RECOMMENDED_VISIBILITY` (`OperatorOnly`), ist für einen Aufrufer
    /// ohne Operator-Berechtigung nicht sichtbar — auch nicht über einen
    /// Rückverweis-Sprung (K35) — und für einen Operator-Aufrufer sichtbar.
    #[test]
    fn test_visibility_follows_recommended_visibility_including_backlink_hop() {
        let proposal = sample_proposal();
        let frontmatter = Frontmatter::new(
            AgentId::new("system"),
            RECOMMENDED_VISIBILITY,
            jiff::Timestamp::UNIX_EPOCH,
        );
        let mut proposal_artifact = proposal
            .to_artifact(frontmatter)
            .expect("proposal embeds into an artifact");

        // Ein `SelfOnly`-Notiz-Artefakt, das per Backlink auf den Vorschlag
        // verweist — der K35-Fall: der Rückverweis darf keine der beiden
        // Sichtbarkeiten unterlaufen.
        let mut linking_note_frontmatter = Frontmatter::new(
            AgentId::new("agent"),
            VisibilityScope::SelfOnly,
            jiff::Timestamp::UNIX_EPOCH,
        );
        linking_note_frontmatter.links = vec![proposal_artifact.id.clone()];
        let linking_note = crate::artifact::KnowledgeArtifact::new(
            ArtifactId::new("diary/note-about-the-proposal"),
            ArtifactKind::DiaryEntry,
            linking_note_frontmatter,
            "referencing the model behavior proposal",
        );

        proposal_artifact.frontmatter.links = Vec::new();
        let mut index = KnowledgeIndex::new();
        index.insert(proposal_artifact.clone());
        index.insert(linking_note);

        let unauthorized = crate::artifact::RecallQuery::new("", VisibilityScope::SelfOnly);
        let denied = search_with(&index, &unauthorized, &ListAllRanker)
            .expect("bounded recall query succeeds");
        assert!(
            denied.hits.is_empty(),
            "ein ModelBehaviorProposal darf ohne Operator-Berechtigung nicht sichtbar sein"
        );

        let authorized = crate::artifact::RecallQuery::new("", VisibilityScope::OperatorOnly);
        let granted = search_with(&index, &authorized, &ListAllRanker)
            .expect("bounded recall query succeeds");
        let visible_ids: Vec<ArtifactId> = granted
            .hits
            .iter()
            .map(|hit| hit.artifact.id.clone())
            .collect();
        assert!(
            visible_ids.contains(&proposal_artifact.id),
            "derselbe ModelBehaviorProposal muss für einen Operator-Aufrufer sichtbar sein"
        );
    }
}
