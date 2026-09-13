//! `ContextProposal` — ein Vorschlag zur Änderung eines Kontextprogramms, als
//! Typ und als durables Artefakt (Knoten **AW5-09**).
//!
//! # Warum es keine `apply`-Funktion gibt
//! Ein `ContextProposal` sagt: „das Kontextprogramm X sollte anders
//! aussehen." Es ändert nichts. Das ist die tragende Regel dieses
//! Teilbaums: **Layer-4-Rückkanal — schlägt vor, committet nie.** Ein
//! System, das seine eigenen Kontextprogramme selbsttätig ändert, kann sich
//! in einen Zustand bringen, den niemand beschlossen hat — und der nächste
//! Vorschlag entstünde dann aus dem geänderten Zustand, nicht aus einer
//! menschlichen Entscheidung. Diese Regel ist hier **strukturell** wahr
//! gemacht, nicht nur dokumentiert:
//!
//! - [`ContextProposal`] trägt keine `apply`-Methode — nur [`ContextProposal::accept`]
//!   und [`ContextProposal::reject`], die ausschließlich [`ContextProposal::status`]
//!   umschreiben (siehe deren Doku für den genauen Beweis, dass dabei kein
//!   Kontextprogramm berührt wird).
//! - Diese Datei — und alles sonst in `harw-knowledge`/`harw-ops`, das dieser
//!   Knoten schreiben darf — importiert `harw_agent_dsl::context_program`
//!   ausschließlich für **Lese**typen (`RawContextSectionSpec`,
//!   `SectionStrength`) und `harw_agent_dsl::ids::DefinitionId` als reine
//!   Kennung. Keine Funktion hier ruft `resolve_context_program`, keine
//!   schreibt eine `.toml`-Kontextprogrammdatei. Ein Kontextprogramm zu
//!   *definieren* bleibt ausschließlich Sache eines Menschen, der eine
//!   `harwness.context.<name>@<major>`-Definition per Hand pflegt.
//!
//! # Die Heuristik in Worten
//! [`propose_must_include_promotions`] prüft — deterministisch, ohne
//! Systemuhr, ohne Zufallszahl, ohne Modellaufruf — eine Regel:
//!
//! > Für jede Sektion, deren Fragmente im Beobachtungsfenster
//! > ([`ContextObservationWindow`]) insgesamt mindestens
//! > [`OVER_BUDGET_PROMOTION_THRESHOLD`]-mal wegen Budgetüberschreitung
//! > (`harw_context::OmissionReason::OverBudget`) ausgelassen wurden, schlägt
//! > die Heuristik genau eine Änderung vor: diese Sektion auf
//! > `SectionStrength::MustInclude` zu heben — in nach Sektionsname
//! > sortierter Reihenfolge, und ganz ohne Vorschlag, wenn keine Sektion die
//! > Schwelle erreicht.
//!
//! Das ist absichtlich eine einzige, in einem Satz erklärbare Regel statt
//! eines Punktesystems: ein Vorschlag, der aus „dieses Fragment wurde N-mal
//! aus Budgetgründen weggelassen" folgt, ist für einen Menschen ohne
//! Rückfrage nachvollziehbar; ein gewichtetes Scoring über mehrere
//! Auslassungsgründe wäre es nicht. Die Sortierung nach Sektionsname (über
//! eine `BTreeMap`, nie eine `HashMap`) ist der Grund, warum zwei Aufrufe mit
//! derselben Eingabe byte-identische Ausgabe liefern (Golden-Test-fähig).
//!
//! # Woher die beobachteten Daten kommen — und woher (noch) nicht
//! Die Heuristik oben braucht Beobachtungsdaten: welches Fragment wurde
//! warum nicht aufgenommen. `Assembly<Gathered|Admitted|Budgeted|Rendered>`
//! (`harw-core/src/context_budget.rs`, Knoten AW1-03) berechnet **genau**
//! solche `(FragmentLabel, OmissionReason)`-Paare zur Laufzeit einer
//! Montage (`ContextAssemblyV2::omissions`). Aber:
//!
//! 1. `harw-core` schreibt diese Paare **nirgendwo durabel** weg.
//!    `record_context_assembly_metrics` speist sie ausschließlich in
//!    `harw-observe`-Metriken/Tracing ein (Zähler, keine abfragbare Historie).
//! 2. `harw-knowledge` hängt (vor diesem Knoten) nicht von `harw-core` ab,
//!    und umgekehrt gibt es keinen Erzeuger, der Montage-Ergebnisse in
//!    `harw-knowledge` oder `harw-ops` hineinreicht.
//!
//! Es gibt also **keinen Pfad**, über den echte Montage-Beobachtungen diesen
//! Schreibbereich heute erreichen — dasselbe Muster wie `validate_patch` ohne
//! Aufrufer und `TraceContext` ohne Erzeuger an anderer Stelle in diesem
//! Ausbauprogramm. Was diesen Schreibbereich **wohl** erreicht, ist das
//! *Vokabular*: [`harw_context::OmissionReason`] und die übrigen
//! Fragment-Typen sind über die bereits bestehende `harw-context`-Abhängigkeit
//! sichtbar. Die Heuristik ist deshalb bewusst gegen einen **expliziten,
//! vom Aufrufer gefüllten Eingabetyp** gebaut — [`ContextObservationWindow`]
//! — statt auf einen Erzeuger zu warten, den es nicht gibt. Ein künftiger
//! Knoten, der `harw-core`s Montage-Ergebnisse tatsächlich durabel machen
//! will, füllt diesen Typ; diese Lücke wird im Abschlussbericht dieses
//! Knotens genannt, nicht stillschweigend mit erfundenen Daten überbrückt.
//!
//! # Gewählte Sichtbarkeit: `OperatorOnly`
//! Ein Vorschlag entsteht aus beobachtetem Verhalten (welche Fragmente
//! wurden verworfen, wie oft, aus welchem Grund) und kann damit verraten,
//! was das System über den eigenen Betrieb weiß — genau die Art Artefakt,
//! für die [`crate::visibility::VisibilityScope::OperatorOnly`] existiert
//! (dieselbe Begründung, die AW4-05 für [`crate::security::SecurityFinding`]
//! trifft). Für einen `ContextProposal` kommt ein zweites Argument hinzu,
//! das über die Sicherheits-Parallele hinausgeht: die tragende Regel dieses
//! Teilbaums ist „schlägt vor, committet nie" — ein Vorschlag zur Änderung
//! der eigenen Steuerungsfläche ist per Definition eine
//! Operator-Governance-Frage, kein agentenseitig konsumierbares
//! Wissensfragment. [`RECOMMENDED_VISIBILITY`] hält diese Entscheidung als
//! einen benannten Wert fest; sie wird — wie bei jeder anderen Sichtbarkeit
//! in dieser Crate — ausschließlich in
//! [`crate::memory::recall::search_with`] durchgesetzt (siehe unten), nicht
//! hier.
//!
//! # Wo Sichtbarkeit durchgesetzt wird
//! Dieses Modul öffnet **keinen** zweiten, ungeprüften Lesepfad. Ein
//! `ContextProposal` wird über [`ContextProposal::to_artifact`] in ein
//! generisches [`crate::artifact::KnowledgeArtifact`] eingebettet (Body =
//! kompaktes JSON) und läuft danach durch exakt dieselbe Maschinerie wie
//! jede andere Sichtbarkeit in dieser Crate: [`crate::index::KnowledgeIndex`]
//! kennt nur `ArtifactKind`/`VisibilityScope`/Tags/Body, nie den konkreten
//! Typ dahinter, und [`crate::memory::recall::search_with`] ist die einzige
//! Stelle, die `VisibilityScope` prüft — bei jedem direkten Treffer und bei
//! jedem Rückverweis-Sprung erneut. `harw-ops`s Prüffläche (siehe dortige
//! Moduldoku) liest ausschließlich über `search`/`search_with`, nie über
//! `KnowledgeIndex::get`/`iter`/`backlinks` mit einer vom Aufrufer frei
//! gewählten Id — mit derselben einen, dokumentierten Ausnahme wie
//! [`crate::context_provider::KnowledgeContextProvider`]: den vollen Body
//! erst nachladen, *nachdem* `search` die Sichtbarkeit bereits bestätigt hat.
//!
//! # Warum `evidence` nicht hier synthetisiert wird
//! [`ContextProposal::evidence`] ist `Vec<harw_plan::EvidenceRef>` —
//! dieselbe Belegkette, die `harw-plan`-Knoten trägt (`digest:
//! Option<harw_types::ContentDigest>`, AW4-04). Die Kantenrichtung wurde
//! geprüft: `harw-plan` hängt nicht von `harw-knowledge` ab (nur von
//! `harw-macros`/`harw-types`/`serde`/`serde_json`/`time`/`tracing`), also
//! entsteht kein Zyklus. **Nicht** verwendet wird dagegen
//! `harw-plan-bridge::finding_store::offset_from_timestamp` — diese Crate
//! hängt bereits von `harw-knowledge` ab (siehe deren `fragment_registry.rs`),
//! ein Rückgriff von hier aus wäre der Zyklus. Deshalb konstruiert
//! [`propose_must_include_promotions`] **keinen** `EvidenceRef` selbst (das
//! würde `time::OffsetDateTime` und damit einen neuen externen
//! Crate-Eintrag in `Cargo.toml` verlangen, den dieser Knoten laut Auftrag
//! nicht selbst erfinden darf) — Belege werden vom Aufrufer bereits fertig
//! gebaut hereingereicht und unverändert an den Vorschlag angehängt.
//!
//! # Exportierte Typen
//! [`ContextProposal`], [`ProposalStatus`], [`ProposedChange`],
//! [`ObservedOmission`], [`ContextObservationWindow`],
//! [`OVER_BUDGET_PROMOTION_THRESHOLD`], [`RECOMMENDED_VISIBILITY`],
//! [`propose_must_include_promotions`].
//!
//! # Concurrency
//! Reine `Send + Sync`-Werttypen; keine innere Veränderlichkeit, keine
//! Threads, keine I/O in diesem Modul.
//!
//! # Fehler
//! Fallible Pfade liefern [`crate::error::KnowledgeError`] /
//! [`crate::error::KnowledgeResult`], wie jede andere Oberfläche dieser
//! Crate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use harw_agent_dsl::context_program::{RawContextSectionSpec, SectionStrength};
use harw_agent_dsl::ids::DefinitionId;
use harw_context::OmissionReason;
use harw_plan::EvidenceRef;

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::visibility::VisibilityScope;

/// Mindestanzahl an `OverBudget`-Auslassungen einer Sektion im
/// Beobachtungsfenster, ab der [`propose_must_include_promotions`] eine
/// `MustInclude`-Anhebung vorschlägt (siehe Moduldoku, „Die Heuristik in
/// Worten").
pub const OVER_BUDGET_PROMOTION_THRESHOLD: u32 = 3;

/// Empfohlene Sichtbarkeit für `ContextProposal`-Artefakte (siehe Moduldoku,
/// „Gewählte Sichtbarkeit: `OperatorOnly`"). Ein Aufrufer, der ein
/// [`ContextProposal`] über [`ContextProposal::to_artifact`] durabel macht,
/// sollte diesen Wert als `Frontmatter::visibility` verwenden, sofern kein
/// spezifischerer Grund dagegenspricht.
pub const RECOMMENDED_VISIBILITY: VisibilityScope = VisibilityScope::OperatorOnly;

/// Entscheidungsstatus eines Vorschlags (nicht zu verwechseln mit
/// [`crate::memory::palace::PalaceStatus`]).
///
/// # Description
/// `PalaceStatus` beschreibt den Promotions-Lebenszyklus **durablen
/// Wissens** (`Established`/`Provisional`/`Superseded`, niemals gelöscht).
/// `ProposalStatus` beschreibt die **Prüfentscheidung eines Menschen über
/// einen Vorschlag** — ein anderer Begriff für eine andere Sache. Einen
/// akzeptierten Vorschlag als `Established` zu führen wäre derselbe
/// Kategorienfehler, den AW4-05 bei der Umbenennung `Confidence` →
/// `PalaceStatus` bereits einmal korrigiert hat: ein akzeptierter Vorschlag
/// ist keine etablierte Wahrheit, sondern eine markierte Entscheidung — er
/// ändert (siehe Moduldoku) kein einziges Kontextprogramm.
///
/// # Examples
/// ```rust
/// use harw_knowledge::context_proposal::ProposalStatus;
///
/// assert_eq!(ProposalStatus::Pending, ProposalStatus::Pending);
/// assert_ne!(ProposalStatus::Pending, ProposalStatus::Accepted);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    /// Noch nicht durch einen Menschen geprüft.
    Pending,
    /// Ein Mensch hat den Vorschlag markiert. Das Kontextprogramm selbst ist
    /// dadurch **nicht** geändert — siehe [`ContextProposal::accept`].
    Accepted,
    /// Ein Mensch hat den Vorschlag abgelehnt.
    Rejected,
}

/// Eine einzelne, maschinell prüfbare Änderung an einem Kontextprogramm
/// (§ Grammatik in `harw_agent_dsl::context_program`).
///
/// # Description
/// Bildet exakt die Stellen ab, an denen `RawContextProgramDefinition`
/// veränderlich ist: die `sections`-Liste (Aufnehmen, Entfernen, Stärke/
/// Detailgrad/Vertrauensanforderung ändern) und die `exclude`-Musterliste.
/// `AddSection` bettet [`RawContextSectionSpec`] direkt ein, statt dessen
/// Felder zu duplizieren — dieselbe Struktur, die eine `[[sections]]`-Zeile
/// im Kontextprogramm-TOML beschreibt. Ein Vorschlag, dessen Inhalt Freitext
/// wäre, wäre von einer Notiz nicht zu unterscheiden und maschinell nicht
/// prüfbar (derselbe Fehler, den K40 bei `StructureDrift` fand); jede
/// Variante hier ist stattdessen ein konkretes, typisiertes Datum.
///
/// # Examples
/// ```rust
/// use harw_agent_dsl::context_program::SectionStrength;
/// use harw_knowledge::context_proposal::ProposedChange;
///
/// let change = ProposedChange::ChangeSectionStrength {
///     section_name: "history.tail".to_owned(),
///     to: SectionStrength::MustInclude,
/// };
/// let json = serde_json::to_string(&change).expect("change serializes");
/// let restored: ProposedChange = serde_json::from_str(&json).expect("change deserializes");
/// assert_eq!(change, restored);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProposedChange {
    /// Eine neue Sektion mit den gegebenen Eigenschaften aufnehmen.
    AddSection {
        /// Die vollständige Sektionsspezifikation (Name, Stärke, Detailgrad,
        /// Mindest-Vertrauensanforderung).
        spec: RawContextSectionSpec,
    },
    /// Eine vorhandene Sektion (nach Name) entfernen.
    RemoveSection {
        /// Name der zu entfernenden Sektion.
        section_name: String,
    },
    /// Die Stärke einer vorhandenen Sektion ändern.
    ChangeSectionStrength {
        /// Name der betroffenen Sektion.
        section_name: String,
        /// Vorgeschlagene neue Stärke.
        to: SectionStrength,
    },
    /// Den Detailgrad einer vorhandenen Sektion ändern.
    ChangeSectionDetail {
        /// Name der betroffenen Sektion.
        section_name: String,
        /// Vorgeschlagener neuer Detailgrad.
        to: harw_context::DetailMode,
    },
    /// Die Mindest-Vertrauensanforderung einer vorhandenen Sektion ändern.
    ChangeSectionTrust {
        /// Name der betroffenen Sektion.
        section_name: String,
        /// Vorgeschlagene neue Mindest-Vertrauensanforderung.
        to: harw_context::TrustClass,
    },
    /// Ein Ausschlussmuster aufnehmen.
    AddExclude {
        /// Das vorgeschlagene Selektor-Muster (§ Grammatik `exclude`).
        pattern: String,
    },
    /// Ein Ausschlussmuster entfernen.
    RemoveExclude {
        /// Das zu entfernende Selektor-Muster.
        pattern: String,
    },
}

/// Ein Vorschlag zur Änderung eines Kontextprogramms (Knoten AW5-09).
///
/// # Description
/// Trägt alles, was ein Mensch braucht, um den Vorschlag zu beurteilen,
/// ohne Code zu lesen: worauf er sich bezieht ([`Self::target_program`],
/// [`Self::target_snapshot_digest`]), was er strukturiert vorschlägt
/// ([`Self::changes`]), warum ([`Self::evidence`]), wann und woher
/// ([`Self::generated_at`], [`Self::produced_by`] — beide hereingereicht,
/// nie aus der Systemuhr gelesen) und in welchem Prüfstatus er steht
/// ([`Self::status`]). Trägt **keine** `apply`-Methode (siehe Moduldoku).
///
/// # Warum `target_snapshot_digest: String` und nicht `SnapshotId`
/// `harw_agent_dsl::executable::SnapshotId` ist konzeptionell exakt „welche
/// Fassung" — mit einem entscheidenden Fehlen: es hat weder `Serialize`/
/// `Deserialize` noch einen öffentlichen Konstruktor aus einem `String`
/// (sein einziges Feld ist privat; nur `Display`/`Debug`/`Clone`/`PartialEq`/
/// `Eq`/`Hash` sind abgeleitet). Ein Feld dieses Typs auf einem Struct, das
/// `deny_unknown_fields`-Serde-Rundläufe bestehen muss, ist damit **nicht
/// baubar**, ohne `harw-agent-dsl` zu ändern — außerhalb des Schreibbereichs
/// dieses Knotens. `target_snapshot_digest` trägt stattdessen die exakte
/// Zeichenkette, die `SnapshotId::to_string()` (bzw. dessen `Display`-Impl,
/// der BLAKE3-Hex-Digest) liefern würde: ein Aufrufer, der eine `SnapshotId`
/// hält, übergibt `snapshot_id.to_string()`. Diese Lücke — `SnapshotId`
/// fehlt `Serialize`/`Deserialize`/ein öffentlicher `String`-Konstruktor —
/// wird im Abschlussbericht dieses Knotens genannt.
///
/// # Concurrency
/// `Send + Sync`; reiner Werttyp, keine innere Veränderlichkeit.
///
/// # Examples
/// ```rust
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::context_program::SectionStrength;
/// use harw_knowledge::context_proposal::{ContextProposal, ProposalStatus, ProposedChange};
/// use harw_knowledge::ArtifactId;
///
/// let proposal = ContextProposal::new(
///     ArtifactId::new("context-proposal/promote-history-tail"),
///     "history.tail wiederholt über Budget ausgelassen",
///     DefinitionId::parse("harwness.context.security-triage@1").expect("valid id"),
///     "deadbeef".repeat(8),
///     vec![ProposedChange::ChangeSectionStrength {
///         section_name: "history.tail".to_owned(),
///         to: SectionStrength::MustInclude,
///     }],
///     Vec::new(),
///     jiff::Timestamp::UNIX_EPOCH,
///     "heuristic:must-include-promotion@1",
/// );
/// assert_eq!(proposal.status, ProposalStatus::Pending);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextProposal {
    /// Stabile Artefakt-Id (`context-proposal/<slug>`).
    pub id: ArtifactId,
    /// Menschenlesbare Kurzfassung. Nicht die einzige Quelle des „was" —
    /// [`Self::changes`] ist maschinell prüfbar, dieser Titel ist nur eine
    /// Beschriftung dafür.
    pub title: String,
    /// Welches Kontextprogramm dieser Vorschlag betrifft.
    pub target_program: DefinitionId,
    /// Welche Fassung dieses Programms beobachtet wurde — siehe „Warum
    /// `target_snapshot_digest: String`" oben.
    pub target_snapshot_digest: String,
    /// Die vorgeschlagenen Änderungen, strukturiert (nicht Prosa).
    pub changes: Vec<ProposedChange>,
    /// Die Belege, auf denen dieser Vorschlag beruht.
    pub evidence: Vec<EvidenceRef>,
    /// Wann dieser Vorschlag erzeugt wurde — hereingereicht, nie
    /// `jiff::Timestamp::now()`.
    pub generated_at: jiff::Timestamp,
    /// Woher dieser Vorschlag stammt (z. B. `"heuristic:must-include-promotion@1"`
    /// oder `"human:<operator>"` bei einem von Hand angelegten Vorschlag) —
    /// hereingereicht, nie aus einem Prozesskontext erraten.
    pub produced_by: String,
    /// Prüfstatus (siehe [`ProposalStatus`]).
    pub status: ProposalStatus,
    /// Topische Tags, unabhängig vom Link-Graph.
    pub tags: Vec<String>,
}

impl ContextProposal {
    /// Baut einen neuen, `Pending`-Vorschlag aus seinen Bestandteilen.
    ///
    /// # Description
    /// Reine Feldzuweisung, keine Validierung über „ist `changes` nicht
    /// leer" hinaus — konsistent mit den übrigen Konstruktoren dieser Crate
    /// (`SecurityFinding::new`, `Baseline::new`, `PalaceNode::new`), die
    /// ebenfalls keine Geschäftsregeln im Konstruktor prüfen. `tags` startet
    /// leer, wie bei `SecurityFinding`/`Baseline`.
    ///
    /// # Arguments
    /// - `id` (`ArtifactId`): stabile Artefakt-Id.
    /// - `title` (`impl Into<String>`): menschenlesbare Kurzfassung.
    /// - `target_program` (`DefinitionId`): betroffenes Kontextprogramm.
    /// - `target_snapshot_digest` (`impl Into<String>`): beobachtete Fassung.
    /// - `changes` (`Vec<ProposedChange>`): strukturierte Änderungen.
    /// - `evidence` (`Vec<EvidenceRef>`): Belege.
    /// - `generated_at` (`jiff::Timestamp`): Erzeugungszeitpunkt, hereingereicht.
    /// - `produced_by` (`impl Into<String>`): Herkunft, hereingereicht.
    ///
    /// # Returns
    /// Einen neuen Vorschlag mit `status = ProposalStatus::Pending` und
    /// leeren `tags`.
    ///
    /// # Examples
    /// Siehe die Typdokumentation von [`ContextProposal`].
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ArtifactId,
        title: impl Into<String>,
        target_program: DefinitionId,
        target_snapshot_digest: impl Into<String>,
        changes: Vec<ProposedChange>,
        evidence: Vec<EvidenceRef>,
        generated_at: jiff::Timestamp,
        produced_by: impl Into<String>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            target_program,
            target_snapshot_digest: target_snapshot_digest.into(),
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
    /// [`ProposalStatus::Accepted`]. Es gibt keinen Aufruf in dieser
    /// Funktion — und keine andere Funktion in dieser Crate oder in
    /// `harw-ops` (außerhalb von `explore.rs`/`research.rs`, die diesem
    /// Knoten ohnehin nicht gehören) —, der daraufhin ein Kontextprogramm
    /// liest, parst oder schreibt. Das ist der strukturelle Beweis für
    /// „annehmen markiert, wendet nicht an" (siehe auch den Test
    /// `test_accept_never_touches_any_context_program_definition` in
    /// diesem Modul und die entsprechende Prüffläche in
    /// `harw-ops/src/context_proposal.rs`).
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// [`KnowledgeError::ProposalNotPending`], wenn [`Self::status`] nicht
    /// [`ProposalStatus::Pending`] ist — eine bereits entschiedene
    /// Vorlage wird nicht erneut entschieden.
    ///
    /// # Examples
    /// ```rust
    /// # use harw_agent_dsl::ids::DefinitionId;
    /// # use harw_knowledge::context_proposal::ContextProposal;
    /// # use harw_knowledge::ArtifactId;
    /// let mut proposal = ContextProposal::new(
    ///     ArtifactId::new("context-proposal/example"),
    ///     "Beispiel",
    ///     DefinitionId::parse("harwness.context.base@1").expect("valid id"),
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
    /// [`KnowledgeError::ProposalNotPending`], wenn [`Self::status`] nicht
    /// [`ProposalStatus::Pending`] ist.
    ///
    /// # Examples
    /// ```rust
    /// # use harw_agent_dsl::ids::DefinitionId;
    /// # use harw_knowledge::context_proposal::{ContextProposal, ProposalStatus};
    /// # use harw_knowledge::ArtifactId;
    /// let mut proposal = ContextProposal::new(
    ///     ArtifactId::new("context-proposal/example"),
    ///     "Beispiel",
    ///     DefinitionId::parse("harwness.context.base@1").expect("valid id"),
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
    ///
    /// Trips the Context Steward's `steward_redecision_violation_total` null
    /// counter (AW6-08, see [`crate::context_steward`]) on the error branch —
    /// reached in production every time `/context-proposal accept|reject
    /// <id>` (`harw-ops/src/context_proposal.rs::decide`, wired into
    /// `harw-cli` via `harw_ops::register_all`) is called on a proposal that
    /// has already been decided.
    fn transition_to(&mut self, to: ProposalStatus) -> KnowledgeResult<()> {
        if self.status != ProposalStatus::Pending {
            crate::context_steward::STEWARD_REDECISION_VIOLATION.violated(&harw_observe::NullSink, &[]);
            return Err(KnowledgeError::ProposalNotPending {
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
    /// generische Einbettung, über die jede Sichtbarkeitsprüfung dieser
    /// Crate läuft ([`crate::index::KnowledgeIndex`] kennt nur `body: String`,
    /// nie den konkreten Typ dahinter). `frontmatter.visibility` sollte
    /// [`RECOMMENDED_VISIBILITY`] sein, sofern kein spezifischerer Grund
    /// dagegenspricht; diese Funktion erzwingt das nicht (die Sichtbarkeit
    /// bleibt Entscheidung des Aufrufers, wie überall in dieser Crate).
    ///
    /// # Arguments
    /// - `frontmatter` (`Frontmatter`): die anzuhängende Frontmatter
    ///   (Sichtbarkeit, Tags, Links, Autor).
    ///
    /// # Returns
    /// Ein [`KnowledgeArtifact`] mit `kind = ArtifactKind::ContextProposal`.
    ///
    /// # Errors
    /// [`KnowledgeError::Json`], wenn die JSON-Serialisierung fehlschlägt
    /// (bei diesem Typ praktisch ausgeschlossen — keine `f64`/`NaN`-Felder).
    ///
    /// # Examples
    /// ```rust
    /// # use harw_agent_dsl::ids::DefinitionId;
    /// # use harw_knowledge::context_proposal::{ContextProposal, RECOMMENDED_VISIBILITY};
    /// # use harw_knowledge::{AgentId, ArtifactId, ArtifactKind, Frontmatter};
    /// let proposal = ContextProposal::new(
    ///     ArtifactId::new("context-proposal/example"),
    ///     "Beispiel",
    ///     DefinitionId::parse("harwness.context.base@1").expect("valid id"),
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
    /// assert_eq!(artifact.kind, ArtifactKind::ContextProposal);
    /// ```
    pub fn to_artifact(&self, frontmatter: Frontmatter) -> KnowledgeResult<KnowledgeArtifact> {
        let body = serde_json::to_string(self)?;
        Ok(KnowledgeArtifact::new(
            self.id.clone(),
            ArtifactKind::ContextProposal,
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
    /// Den rekonstruierten [`ContextProposal`].
    ///
    /// # Errors
    /// - [`KnowledgeError::ArtifactKindMismatch`], wenn `artifact.kind` nicht
    ///   [`ArtifactKind::ContextProposal`] ist.
    /// - [`KnowledgeError::Json`], wenn der Body kein gültiges, dem Schema
    ///   entsprechendes JSON ist (unbekannte Felder eingeschlossen —
    ///   `#[serde(deny_unknown_fields)]`).
    ///
    /// # Examples
    /// ```rust
    /// # use harw_agent_dsl::ids::DefinitionId;
    /// # use harw_knowledge::context_proposal::{ContextProposal, RECOMMENDED_VISIBILITY};
    /// # use harw_knowledge::{AgentId, ArtifactId, Frontmatter};
    /// let proposal = ContextProposal::new(
    ///     ArtifactId::new("context-proposal/example"),
    ///     "Beispiel",
    ///     DefinitionId::parse("harwness.context.base@1").expect("valid id"),
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
    /// let restored = ContextProposal::from_artifact(&artifact).expect("proposal parses back");
    /// assert_eq!(restored.id, proposal.id);
    /// ```
    pub fn from_artifact(artifact: &KnowledgeArtifact) -> KnowledgeResult<Self> {
        if artifact.kind != ArtifactKind::ContextProposal {
            return Err(KnowledgeError::ArtifactKindMismatch {
                id: artifact.id.to_string(),
                expected: "context_proposal".to_owned(),
                actual: format!("{:?}", artifact.kind),
            });
        }
        Ok(serde_json::from_str(&artifact.body)?)
    }
}

/// Eine einzelne, von außen eingereichte Beobachtung: Fragmente einer
/// Sektion wurden bei der Montage eines konkreten Kontextprogramm-Snapshots
/// wiederholt aus demselben Grund ausgelassen.
///
/// # Description
/// Bündelt reales Vokabular ([`harw_context::OmissionReason`], bereits über
/// die bestehende `harw-context`-Abhängigkeit erreichbar) mit einer
/// Sektionskennung, die `Assembly::omissions` selbst nicht mitführt (dessen
/// `(FragmentLabel, OmissionReason)`-Paare kennen keine Sektion — die
/// Zuordnung Fragment→Sektion liegt beim Aufrufer, der die ursprünglichen
/// `harw_context::Fragment`s gebaut hat). Absichtlich **kein** per-Fragment
/// `FragmentLabel`: die Heuristik entscheidet auf Sektionsebene, nicht auf
/// Fragmentebene, ein Label würde nur unbenutzte Präzision vortäuschen.
///
/// # Examples
/// ```rust
/// use harw_context::OmissionReason;
/// use harw_knowledge::context_proposal::ObservedOmission;
///
/// let observation = ObservedOmission {
///     section_name: "history.tail".to_owned(),
///     reason: OmissionReason::OverBudget,
///     occurrences: 3,
/// };
/// assert_eq!(observation.occurrences, 3);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedOmission {
    /// Name der Sektion, zu der die ausgelassenen Fragmente gehörten.
    pub section_name: String,
    /// Warum ausgelassen wurde.
    pub reason: OmissionReason,
    /// Wie oft diese Kombination im Beobachtungsfenster auftrat.
    pub occurrences: u32,
}

/// Das explizite, vom Aufrufer gefüllte Beobachtungsfenster, gegen das
/// [`propose_must_include_promotions`] läuft (siehe Moduldoku, „Woher die
/// beobachteten Daten kommen").
///
/// # Examples
/// ```rust
/// use harw_context::OmissionReason;
/// use harw_knowledge::context_proposal::{ContextObservationWindow, ObservedOmission};
///
/// let window = ContextObservationWindow {
///     omissions: vec![ObservedOmission {
///         section_name: "history.tail".to_owned(),
///         reason: OmissionReason::OverBudget,
///         occurrences: 1,
///     }],
/// };
/// assert_eq!(window.omissions.len(), 1);
/// ```
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextObservationWindow {
    /// Die einzelnen Beobachtungen in diesem Fenster, in beliebiger
    /// Reihenfolge — die Heuristik sortiert selbst (siehe Moduldoku).
    pub omissions: Vec<ObservedOmission>,
}

/// Leitet — deterministisch — einen Vorschlag aus einem Beobachtungsfenster
/// ab: Sektionen, deren Fragmente wiederholt über Budget ausgelassen wurden,
/// auf `must-include` heben.
///
/// # Description
/// Siehe Moduldoku, „Die Heuristik in Worten", für die Regel in einem Satz.
/// Aggregiert `occurrences` je Sektionsname für
/// `OmissionReason::OverBudget` über eine `BTreeMap` (nie eine `HashMap`),
/// damit die Ausgabereihenfolge — und damit die serialisierte Byte-Folge —
/// bei gleicher Eingabe immer identisch ist. Enthält weder eine Systemuhr
/// noch eine Zufallszahl noch einen Modellaufruf: `generated_at` und
/// `produced_by` kommen vollständig vom Aufrufer, `evidence` wird
/// unverändert durchgereicht (siehe Moduldoku, „Warum `evidence` nicht hier
/// synthetisiert wird").
///
/// # Arguments
/// - `id` (`ArtifactId`): Id des zu erzeugenden Vorschlags.
/// - `target_program` (`DefinitionId`): betroffenes Kontextprogramm.
/// - `target_snapshot_digest` (`impl Into<String>`): beobachtete Fassung.
/// - `window` (`&ContextObservationWindow`): die Beobachtungen.
/// - `generated_at` (`jiff::Timestamp`): Erzeugungszeitpunkt, hereingereicht.
/// - `produced_by` (`impl Into<String>`): Herkunftskennung, z. B.
///   `"heuristic:must-include-promotion@1"`.
/// - `evidence` (`Vec<EvidenceRef>`): vom Aufrufer bereits fertig gebaute
///   Belege; unverändert an den Vorschlag angehängt.
///
/// # Returns
/// `Some(ContextProposal)` mit einer `ChangeSectionStrength`-Änderung je
/// Sektion, die die Schwelle erreicht (sortiert nach Sektionsname), oder
/// `None`, wenn keine Sektion die Schwelle erreicht — ein leeres
/// Beobachtungsfenster erzeugt also **nie** einen Vorschlag.
///
/// # Errors
/// Diese Funktion ist total; sie liefert kein `Result`.
///
/// # Examples
/// ```rust
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_context::OmissionReason;
/// use harw_knowledge::context_proposal::{
///     ContextObservationWindow, ObservedOmission, ProposedChange, propose_must_include_promotions,
/// };
/// use harw_knowledge::ArtifactId;
///
/// let window = ContextObservationWindow {
///     omissions: vec![ObservedOmission {
///         section_name: "history.tail".to_owned(),
///         reason: OmissionReason::OverBudget,
///         occurrences: 3,
///     }],
/// };
/// let proposal = propose_must_include_promotions(
///     ArtifactId::new("context-proposal/example"),
///     DefinitionId::parse("harwness.context.base@1").expect("valid id"),
///     "deadbeef".repeat(8),
///     &window,
///     jiff::Timestamp::UNIX_EPOCH,
///     "heuristic:must-include-promotion@1",
///     Vec::new(),
/// )
/// .expect("threshold reached, a proposal is produced");
/// assert_eq!(proposal.changes.len(), 1);
/// assert!(matches!(proposal.changes[0], ProposedChange::ChangeSectionStrength { .. }));
///
/// // Leere Beobachtung: kein Vorschlag aus dem Nichts.
/// let empty = ContextObservationWindow::default();
/// assert!(
///     propose_must_include_promotions(
///         ArtifactId::new("context-proposal/example"),
///         DefinitionId::parse("harwness.context.base@1").expect("valid id"),
///         "deadbeef".repeat(8),
///         &empty,
///         jiff::Timestamp::UNIX_EPOCH,
///         "heuristic:must-include-promotion@1",
///         Vec::new(),
///     )
///     .is_none()
/// );
/// ```
#[must_use]
pub fn propose_must_include_promotions(
    id: ArtifactId,
    target_program: DefinitionId,
    target_snapshot_digest: impl Into<String>,
    window: &ContextObservationWindow,
    generated_at: jiff::Timestamp,
    produced_by: impl Into<String>,
    evidence: Vec<EvidenceRef>,
) -> Option<ContextProposal> {
    let mut over_budget_totals: BTreeMap<&str, u32> = BTreeMap::new();
    for observation in &window.omissions {
        if observation.reason == OmissionReason::OverBudget {
            *over_budget_totals
                .entry(observation.section_name.as_str())
                .or_insert(0) += observation.occurrences;
        }
    }

    let changes: Vec<ProposedChange> = over_budget_totals
        .into_iter()
        .filter(|(_, total)| *total >= OVER_BUDGET_PROMOTION_THRESHOLD)
        .map(|(section_name, _)| ProposedChange::ChangeSectionStrength {
            section_name: section_name.to_owned(),
            to: SectionStrength::MustInclude,
        })
        .collect();

    if changes.is_empty() {
        return None;
    }

    let title = format!(
        "{} Sektion(en) wiederholt über Budget ausgelassen: auf must-include heben",
        changes.len()
    );

    Some(ContextProposal::new(
        id,
        title,
        target_program,
        target_snapshot_digest,
        changes,
        evidence,
        generated_at,
        produced_by,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        ContextObservationWindow, ContextProposal, ObservedOmission, ProposalStatus,
        ProposedChange, RECOMMENDED_VISIBILITY, propose_must_include_promotions,
    };
    use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter};
    use crate::error::KnowledgeError;
    use crate::visibility::{AgentId, VisibilityScope};

    use harw_agent_dsl::context_program::SectionStrength;
    use harw_agent_dsl::ids::DefinitionId;
    use harw_context::OmissionReason;

    fn program_id() -> DefinitionId {
        DefinitionId::parse("harwness.context.security-triage@1").expect("valid definition id")
    }

    fn sample_proposal() -> ContextProposal {
        ContextProposal::new(
            ArtifactId::new("context-proposal/example"),
            "Beispielvorschlag",
            program_id(),
            "deadbeef".repeat(8),
            vec![ProposedChange::ChangeSectionStrength {
                section_name: "history.tail".to_owned(),
                to: SectionStrength::MustInclude,
            }],
            Vec::new(),
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:must-include-promotion@1",
        )
    }

    /// Rundlauf über serde; unbekanntes Feld wird abgelehnt (K19).
    #[test]
    fn test_serde_roundtrip_and_deny_unknown_fields() {
        let proposal = sample_proposal();
        let json = serde_json::to_string(&proposal).expect("proposal serializes");
        let restored: ContextProposal =
            serde_json::from_str(&json).expect("proposal deserializes");

        assert_eq!(restored.id, proposal.id);
        assert_eq!(restored.title, proposal.title);
        assert_eq!(restored.target_program, proposal.target_program);
        assert_eq!(restored.status, proposal.status);
        assert_eq!(restored.changes, proposal.changes);

        let mut value: serde_json::Value = serde_json::from_str(&json).expect("value parses");
        value
            .as_object_mut()
            .expect("proposal is a JSON object")
            .insert("unexpected_field".to_owned(), serde_json::json!(true));
        let rejected: Result<ContextProposal, _> = serde_json::from_str(&value.to_string());
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
        assert_eq!(artifact.kind, ArtifactKind::ContextProposal);

        let restored =
            ContextProposal::from_artifact(&artifact).expect("artifact parses back into a proposal");
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

        let error = ContextProposal::from_artifact(&wrong_kind)
            .expect_err("wrong artifact kind must be rejected");
        assert!(matches!(error, KnowledgeError::ArtifactKindMismatch { .. }));
    }

    /// Determinismus: gleiche Eingabe → byte-gleicher Vorschlag, zweimal.
    #[test]
    fn test_heuristic_is_deterministic_byte_for_byte() {
        let window = ContextObservationWindow {
            omissions: vec![
                ObservedOmission {
                    section_name: "history.tail".to_owned(),
                    reason: OmissionReason::OverBudget,
                    occurrences: 2,
                },
                ObservedOmission {
                    section_name: "history.tail".to_owned(),
                    reason: OmissionReason::OverBudget,
                    occurrences: 1,
                },
                ObservedOmission {
                    section_name: "goal.invariants".to_owned(),
                    reason: OmissionReason::BelowCeiling,
                    occurrences: 9,
                },
            ],
        };

        let build = || {
            propose_must_include_promotions(
                ArtifactId::new("context-proposal/deterministic"),
                program_id(),
                "deadbeef".repeat(8),
                &window,
                jiff::Timestamp::UNIX_EPOCH,
                "heuristic:must-include-promotion@1",
                Vec::new(),
            )
            .expect("threshold reached (2 + 1 = 3)")
        };

        let first = serde_json::to_vec(&build()).expect("first proposal serializes");
        let second = serde_json::to_vec(&build()).expect("second proposal serializes");
        assert_eq!(first, second, "identical input must produce byte-identical output");
    }

    /// Die Heuristik schlägt bei leerer Beobachtung nichts vor.
    #[test]
    fn test_heuristic_proposes_nothing_from_empty_observation() {
        let empty = ContextObservationWindow::default();
        let proposal = propose_must_include_promotions(
            ArtifactId::new("context-proposal/empty"),
            program_id(),
            "deadbeef".repeat(8),
            &empty,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:must-include-promotion@1",
            Vec::new(),
        );
        assert!(proposal.is_none(), "kein Vorschlag aus dem Nichts");
    }

    /// Unterhalb der Schwelle: ebenfalls kein Vorschlag.
    #[test]
    fn test_heuristic_below_threshold_proposes_nothing() {
        let window = ContextObservationWindow {
            omissions: vec![ObservedOmission {
                section_name: "history.tail".to_owned(),
                reason: OmissionReason::OverBudget,
                occurrences: super::OVER_BUDGET_PROMOTION_THRESHOLD - 1,
            }],
        };
        let proposal = propose_must_include_promotions(
            ArtifactId::new("context-proposal/below-threshold"),
            program_id(),
            "deadbeef".repeat(8),
            &window,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:must-include-promotion@1",
            Vec::new(),
        );
        assert!(proposal.is_none());
    }

    /// Eine Auslassung aus einem anderen Grund als `OverBudget` zählt nicht.
    #[test]
    fn test_heuristic_ignores_non_over_budget_reasons() {
        let window = ContextObservationWindow {
            omissions: vec![ObservedOmission {
                section_name: "plan.current".to_owned(),
                reason: OmissionReason::ExcludedByProgram,
                occurrences: 100,
            }],
        };
        let proposal = propose_must_include_promotions(
            ArtifactId::new("context-proposal/ignored-reason"),
            program_id(),
            "deadbeef".repeat(8),
            &window,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:must-include-promotion@1",
            Vec::new(),
        );
        assert!(proposal.is_none());
    }

    /// Mehrere qualifizierende Sektionen erscheinen sortiert nach Namen.
    #[test]
    fn test_heuristic_orders_multiple_qualifying_sections_by_name() {
        let window = ContextObservationWindow {
            omissions: vec![
                ObservedOmission {
                    section_name: "zzz.last".to_owned(),
                    reason: OmissionReason::OverBudget,
                    occurrences: 5,
                },
                ObservedOmission {
                    section_name: "aaa.first".to_owned(),
                    reason: OmissionReason::OverBudget,
                    occurrences: 5,
                },
            ],
        };
        let proposal = propose_must_include_promotions(
            ArtifactId::new("context-proposal/ordered"),
            program_id(),
            "deadbeef".repeat(8),
            &window,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:must-include-promotion@1",
            Vec::new(),
        )
        .expect("both sections reach the threshold");

        let names: Vec<&str> = proposal
            .changes
            .iter()
            .map(|change| match change {
                ProposedChange::ChangeSectionStrength { section_name, .. } => {
                    section_name.as_str()
                }
                other => panic!("unexpected change variant: {other:?}"),
            })
            .collect();
        assert_eq!(names, vec!["aaa.first", "zzz.last"]);
    }

    /// Ein Vorschlag ist ohne Blick in den Code beurteilbar: die Belege
    /// kommen mit, unverändert durchgereicht.
    #[test]
    fn test_evidence_flows_through_to_the_produced_proposal() {
        use harw_plan::{EvidenceKind, EvidenceRef};

        // `harw_plan::testing::fixture_time()` liefert deterministisch
        // `OffsetDateTime::UNIX_EPOCH` (keine Systemuhr) — dieser Test nennt
        // den Typ `time::OffsetDateTime` dadurch nirgendwo selbst, sodass
        // `harw-knowledge` keine eigene `time`-Abhängigkeit braucht, nur um
        // eine `EvidenceRef`-Fixture zu bauen.
        let evidence = vec![EvidenceRef {
            kind: EvidenceKind::Finding,
            locator: "context-observation:history.tail:over-budget".to_owned(),
            attached_at: harw_plan::testing::fixture_time(),
            actor: "heuristic:must-include-promotion@1".to_owned(),
            digest: None,
        }];
        let window = ContextObservationWindow {
            omissions: vec![ObservedOmission {
                section_name: "history.tail".to_owned(),
                reason: OmissionReason::OverBudget,
                occurrences: super::OVER_BUDGET_PROMOTION_THRESHOLD,
            }],
        };
        let proposal = propose_must_include_promotions(
            ArtifactId::new("context-proposal/with-evidence"),
            program_id(),
            "deadbeef".repeat(8),
            &window,
            jiff::Timestamp::UNIX_EPOCH,
            "heuristic:must-include-promotion@1",
            evidence,
        )
        .expect("threshold reached");

        assert_eq!(proposal.evidence.len(), 1);
        assert_eq!(
            proposal.evidence[0].locator,
            "context-observation:history.tail:over-budget"
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
        assert!(matches!(error, KnowledgeError::ProposalNotPending { .. }));

        let error = proposal
            .reject()
            .expect_err("an already-decided proposal must not flip to rejected either");
        assert!(matches!(error, KnowledgeError::ProposalNotPending { .. }));
    }

    #[test]
    fn test_reject_marks_status() {
        let mut proposal = sample_proposal();
        proposal.reject().expect("pending proposal rejects");
        assert_eq!(proposal.status, ProposalStatus::Rejected);
    }

    /// Es gibt keinen Weg, einen Vorschlag anzuwenden: `ContextProposal`
    /// bietet über `id`/`title`/`target_program`/`target_snapshot_digest`/
    /// `changes`/`evidence`/`generated_at`/`produced_by`/`status`/`tags`
    /// hinaus keine weiteren öffentlichen Methoden als `new`, `accept`,
    /// `reject`, `to_artifact`, `from_artifact` — keine davon liest oder
    /// schreibt eine Kontextprogramm-Definition. Dieser Test beweist die
    /// eine Hälfte davon strukturell: `accept` ändert nichts außer `status`,
    /// nachweisbar daran, dass jedes andere Feld vor und nach dem Aufruf
    /// identisch bleibt.
    #[test]
    fn test_accept_never_touches_any_context_program_definition() {
        let mut proposal = sample_proposal();
        let before_changes = proposal.changes.clone();
        let before_target = proposal.target_program.clone();
        let before_snapshot = proposal.target_snapshot_digest.clone();

        proposal.accept().expect("pending proposal accepts");

        assert_eq!(proposal.changes, before_changes, "accept darf `changes` nicht verändern");
        assert_eq!(
            proposal.target_program, before_target,
            "accept darf `target_program` nicht verändern"
        );
        assert_eq!(
            proposal.target_snapshot_digest, before_snapshot,
            "accept darf `target_snapshot_digest` nicht verändern"
        );
        assert_eq!(proposal.status, ProposalStatus::Accepted);
    }

    /// Deciding an already-decided proposal — the real, production-reached
    /// call site (`ContextProposal::accept`/`reject`, invoked from
    /// `/context-proposal accept|reject <id>`) — trips
    /// `steward_redecision_violation_total` (AW6-08).
    #[test]
    fn test_redeciding_a_proposal_trips_the_steward_redecision_counter() {
        use crate::context_steward::{STEWARD_COUNTER_LOCK, STEWARD_REDECISION_VIOLATION};

        let _guard = STEWARD_COUNTER_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = STEWARD_REDECISION_VIOLATION.count();
        let mut proposal = sample_proposal();
        proposal.accept().expect("first decision succeeds");

        let error = proposal.accept().expect_err("a second decision must fail");
        assert!(matches!(error, KnowledgeError::ProposalNotPending { .. }));
        assert!(
            STEWARD_REDECISION_VIOLATION.count() > before,
            "deciding an already-decided proposal must trip the counter"
        );
    }
}
