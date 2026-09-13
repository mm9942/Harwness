//! Kern-Datentypen für `harw-plan`.
//!
//! Verantwortungsbereich: Definiert `PlanNodeStatus`, `PlanNodeKind`, `Criterion`,
//! `VerificationStep`, `InvalidationCondition`, `EvidenceKind`, `EvidenceRef`,
//! `Assignment`, `PlanNode` und `Plan` gemäß Design-Doc §2.
//!
//! Alle `OffsetDateTime`-Felder werden via `time::serde::rfc3339` serialisiert.
//! Enums tragen `#[serde(rename_all = "snake_case")]`.
//!
//! Rückwärtskompatibilität: Alle in diesem AP neu ergänzten `PlanNode`-Felder
//! (`kind`, `wave`, `assignment`, `parent`) tragen `#[serde(default)]`, damit
//! bestehende JSON-Snapshots (`<root>/plans/<id>/rev-N.json`) ohne diese Felder
//! weiterhin deserialisierbar bleiben. `EvidenceKind` erhält zusätzlich eine
//! manuelle `Deserialize`-Implementierung, die unbekannte oder alternativ
//! geschriebene (kebab-case) Alt-Werte auf `EvidenceKind::Other` abbildet,
//! statt einen Fehler zu erzeugen.
//!
//! Exportierte Typen: [`PlanNodeStatus`], [`PlanNodeKind`], [`Criterion`],
//! [`VerificationStep`], [`InvalidationCondition`], [`EvidenceKind`],
//! [`EvidenceRef`], [`Assignment`], [`PlanNode`], [`Plan`].

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use time::OffsetDateTime;

use crate::ids::{ContractRef, PathOrSymbol, PlanId, RevisionId, TaskId};

/// Status eines Plan-Knotens.
///
/// # Description
/// Legt den Lebenszyklus-Zustand eines `PlanNode` fest. Übergänge werden in
/// `validate.rs` durch eine Status-Matrix geprüft. `Superseded` und `Invalidated`
/// sind terminal — kein weiterer Übergang möglich.
///
/// # Concurrency
/// `Copy` + `Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanNodeStatus {
    /// Entwurfsstadium — noch nicht ausführbar.
    Draft,
    /// Bereit zur Ausführung.
    Ready,
    /// Aktuell in Bearbeitung.
    InProgress,
    /// Blockiert durch eine Abhängigkeit oder ein externes Hindernis.
    Blocked,
    /// Abgeschlossen (erfordert mindestens einen `EvidenceRef`).
    Completed,
    /// Durch einen neuen Plan ersetzt (terminal).
    Superseded,
    /// Durch eine Invalidierungsbedingung ungültig geworden (terminal).
    Invalidated,
}

/// Art eines Plan-Knotens.
///
/// # Description
/// Klassifiziert die Rolle eines `PlanNode` im Ausführungsgraphen. Steuert
/// Orchestrierung und Worker-Auswahl:
/// - `Explore` markiert eine Vor-Exploration, die vor riskanter Arbeit
///   (z. B. großflächigen Refactorings) stattfindet, um den Ist-Zustand zu
///   erfassen, bevor Änderungen vorgenommen werden.
/// - `Synthesis` markiert eine Barriere, die erst fortgesetzt werden darf,
///   nachdem alle zugehörigen `Research`-Knoten abgeschlossen sind und ihre
///   Ergebnisse zusammengeführt wurden.
/// - `Composite` markiert einen Knoten, der per `Expand`-Aktion in mehrere
///   Kind-Knoten zerlegt wurde; ein `Composite`-Knoten gilt erst dann als
///   `Completed`, wenn alle seine Kinder (siehe [`PlanNode::parent`])
///   `Completed` sind.
///
/// `Coding` ist der Default, da die Mehrzahl der Knoten klassische
/// Implementierungsarbeit beschreibt (Rückwärtskompatibilität mit
/// Alt-Snapshots ohne `kind`-Feld).
///
/// # Concurrency
/// `Copy` + `Send + Sync`.
// `KebabEnum` liefert `ALL`, `as_str`, `Display` und `FromStr`. `ALL` ist die
// einzige Quelle für die Liste der Knotenarten: sie stand vorher dreimal im
// Workspace — in der Config-Validierung, im CLI-Parser und ein drittes Mal als
// Fließtext in dessen Fehlermeldung. Eine neue Variante wäre an allen drei
// Stellen unbemerkt gefehlt.
//
// Alle Varianten sind einwortig, kebab-case und das `snake_case` von serde
// fallen hier also zusammen — die Wire-Form ändert sich nicht.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Default,
    harw_macros::KebabEnum,
)]
#[serde(rename_all = "snake_case")]
#[kebab_enum(error = "crate::error::PlanError", ctor = "unknown_variant")]
pub enum PlanNodeKind {
    /// Recherche-Phase — sammelt Informationen, ändert keinen Code.
    Research,
    /// Vor-Exploration vor riskanter Arbeit (siehe Typ-Dokumentation oben).
    Explore,
    /// Analysiert bestehenden Code oder Daten, ohne sie zu verändern.
    Analysis,
    /// Barriere über Research-Ergebnisse (siehe Typ-Dokumentation oben).
    Synthesis,
    /// Definiert oder verhandelt einen Vertrag (Schnittstelle, Schema,
    /// Protokoll) zwischen mehreren Komponenten oder Workern.
    Contract,
    /// Klassische Implementierungsarbeit (Standardwert).
    #[default]
    Coding,
    /// Führt mehrere abgeschlossene Teilergebnisse zu einem Ganzen zusammen.
    Integration,
    /// Führt Verifikationsschritte aus (Tests, Clippy, manuelle Prüfung, …).
    Verification,
    /// Schreibt oder aktualisiert Dokumentation.
    Docs,
    /// Durch `Expand` in Kind-Knoten zerlegter Knoten (siehe Typ-Dokumentation
    /// oben).
    Composite,
}

/// Akzeptanzkriterium für einen Plan-Knoten.
///
/// # Description
/// Beschreibt, wann ein Knoten als erfüllt gilt, inklusive konkreter
/// Verifikationsschritte.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Criterion {
    /// Menschenlesbare Beschreibung des Kriteriums.
    pub description: String,
    /// Konkrete Verifikationsschritte, die dieses Kriterium belegen.
    pub verification: Vec<VerificationStep>,
}

/// Ein einzelner Verifikationsschritt für ein Akzeptanzkriterium.
///
/// # Description
/// Diskriminiertes Enum (tag `kind`). Vier Varianten: Shell-Befehl, Artefakt,
/// Trace-Event und manuelle Prüfung.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VerificationStep {
    /// Führt einen Shell-Befehl aus und prüft den Exit-Code.
    Command {
        /// Shell-Befehl.
        cmd: String,
        /// Erwarteter Exit-Code.
        expect_exit: i32,
    },
    /// Prüft, ob ein Artefakt-Pfad existiert.
    Artifact {
        /// Pfad des Artefakts.
        path: String,
    },
    /// Erwartet ein bestimmtes Trace-Event.
    TraceEvent {
        /// Name des Trace-Events.
        name: String,
    },
    /// Manuelle Prüfung durch einen Menschen.
    Manual {
        /// Beschreibung der manuellen Prüfung.
        note: String,
    },
}

/// Bedingung, unter der ein Plan-Knoten invalidiert wird.
///
/// # Description
/// Diskriminiertes Enum (tag `kind`). Sechs Varianten gemäß Design-Doc §2.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvalidationCondition {
    /// Ein referenzierter Vertrag hat sich geändert.
    ContractChanged(ContractRef),
    /// Ein Upstream-Knoten wurde erneut geöffnet.
    UpstreamNodeReopened(TaskId),
    /// Die Repository-Revision hat sich verschoben.
    RepoRevisionMoved,
    /// Manuelle Invalidierung.
    ManualInvalidate,
    /// Ein zuvor dokumentierter Research-Fund widerspricht nun der Annahme,
    /// auf der dieser Knoten beruht (die Antwort auf eine offene Frage hat
    /// sich geändert oder wurde widerlegt).
    FindingContradicts {
        /// ID der Frage, deren zuvor beantwortete Annahme widerlegt wurde.
        question_id: String,
    },
    /// Der Explorationsstand für den referenzierten Pfad oder das Symbol gilt
    /// als veraltet (z. B. weil sich der Code seit der letzten
    /// `Explore`-Passage über diesen Bereich geändert hat).
    ExplorationStale {
        /// Pfad oder Symbol, dessen Explorationsstand als veraltet gilt.
        scope: PathOrSymbol,
    },
}

/// Art eines Evidenz-Nachweises.
///
/// # Description
/// Ersetzt das vormals freie `String`-Feld `EvidenceRef::kind` durch ein
/// typisiertes Enum. Für Rückwärtskompatibilität mit Alt-Snapshots wird
/// `Deserialize` **manuell** implementiert (siehe `impl Deserialize`
/// unterhalb dieses Typs): Werte werden zunächst als `String` gelesen und
/// über [`FromStr`] tolerant gegenüber kebab-case (`"cargo-test"`) und
/// snake_case (`"cargo_test"`) geparst; jeder nicht zuordenbare Wert fällt
/// auf `EvidenceKind::Other`, statt einen Deserialisierungsfehler
/// auszulösen. `#[serde(other)]` scheidet hierfür aus, da es zwar bei
/// feldlosen Enums greift, aber keine kebab/snake-Normalisierung vornehmen
/// kann (ein exakt anders geschriebener, aber eigentlich bekannter Wert wie
/// `"cargo-test"` würde sonst fälschlich auf `Other` fallen statt auf
/// `CargoTest`).
///
/// # Concurrency
/// `Copy` + `Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// Ein Recherche-/Analyse-Fund (z. B. aus einem Research-Worker).
    Finding,
    /// Ergebnis eines `cargo test`-Laufs.
    CargoTest,
    /// Ergebnis eines `cargo clippy`-Laufs.
    Clippy,
    /// Ein Code-Diff als Nachweis.
    Diff,
    /// Ein Tracing-Span als Nachweis (z. B. zur `TraceEvent`-Verifikation).
    TraceSpan,
    /// Manuell durch einen Menschen erbrachter Nachweis.
    Manual,
    /// Nachweis, der von einem ausgeführten Job stammt (Job-ID im `locator`
    /// von [`EvidenceRef`]). `harw-plan` bleibt dabei frei von einer
    /// Abhängigkeit auf `harw-job-runtime`.
    Job,
    /// Unbekannte oder nicht klassifizierbare Nachweisart — Fallback für
    /// Alt-Daten und zukünftige, hier noch nicht bekannte Nachweisarten.
    Other,
}

impl fmt::Display for EvidenceKind {
    /// Gibt die kanonische snake_case-Repräsentation zurück (identisch zu
    /// der Zeichenkette, auf die auch `Serialize` abbildet).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Finding => "finding",
            Self::CargoTest => "cargo_test",
            Self::Clippy => "clippy",
            Self::Diff => "diff",
            Self::TraceSpan => "trace_span",
            Self::Manual => "manual",
            Self::Job => "job",
            Self::Other => "other",
        };
        f.write_str(s)
    }
}

impl FromStr for EvidenceKind {
    type Err = std::convert::Infallible;

    /// Parst einen String tolerant gegenüber Groß-/Kleinschreibung sowie
    /// kebab-case und snake_case (`"cargo-test"` und `"cargo_test"` liefern
    /// beide `CargoTest`). Diese Konvertierung schlägt nie fehl: jeder nicht
    /// zuordenbare Wert liefert `Ok(EvidenceKind::Other)` — das ist die
    /// Grundlage für die Rückwärtskompatibilität von [`EvidenceKind`] mit
    /// Alt-Snapshots (Design-Doc §2/§7).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.to_ascii_lowercase().replace('-', "_");
        Ok(match normalized.as_str() {
            "finding" => Self::Finding,
            "cargo_test" => Self::CargoTest,
            "clippy" => Self::Clippy,
            "diff" => Self::Diff,
            "trace_span" => Self::TraceSpan,
            "manual" => Self::Manual,
            "job" => Self::Job,
            _ => Self::Other,
        })
    }
}

impl<'de> Deserialize<'de> for EvidenceKind {
    /// Deserialisiert über die String-Repräsentation und delegiert an
    /// [`FromStr`], damit unbekannte oder alternativ geschriebene
    /// (kebab-case) Alt-Werte nie einen Fehler erzeugen, sondern auf
    /// `EvidenceKind::Other` fallen (Rückwärtskompatibilität, Design-Doc
    /// §2/§7).
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        // `FromStr::Err` ist `Infallible` — der `Err`-Zweig ist unerreichbar
        // und wird ohne `unwrap()`/`expect()` exhaustiv behandelt.
        match raw.parse::<EvidenceKind>() {
            Ok(kind) => Ok(kind),
            Err(never) => match never {},
        }
    }
}

/// Referenz auf einen Evidenz-Nachweis.
///
/// # Description
/// Verknüpft einen konkreten Nachweis (z. B. Testergebnis, Diff) mit dem
/// Plan-Knoten, der dadurch bestätigt wird. Trägt der Nachweis einen
/// [`digest`](Self::digest), ist sein Inhalt zitierfähig — ohne Digest kann
/// ein Sicherheitsbefund auf nichts zeigen, das sich später als unverändert
/// nachweisen lässt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Art des Nachweises (siehe [`EvidenceKind`]). Deserialisierung ist
    /// tolerant gegenüber unbekannten oder alternativ geschriebenen
    /// (kebab-case) Alt-Werten und fällt in diesem Fall auf
    /// `EvidenceKind::Other` zurück.
    pub kind: EvidenceKind,
    /// Lokator des Nachweises (Pfad, URL oder ID).
    pub locator: String,
    /// Zeitpunkt, zu dem der Nachweis angehängt wurde.
    #[serde(with = "time::serde::rfc3339")]
    pub attached_at: OffsetDateTime,
    /// Akteur, der den Nachweis angehängt hat (z. B. `"worker-abc"`, `"human:mia"`).
    pub actor: String,
    /// Digest des Nachweisinhalts, sofern er inhaltsadressiert vorliegt.
    ///
    /// `Option`, weil `EvidenceRef` bereits auf Platte geschrieben wird:
    /// bestehende Dateien kennen dieses Feld nicht und müssen weiter lesbar
    /// bleiben, deshalb `#[serde(default)]`. `skip_serializing_if` sorgt
    /// dafür, dass ein Nachweis ohne Digest nach einem Schreib-Lese-Zyklus
    /// keinen `"digest": null`-Eintrag bekommt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<harw_types::ContentDigest>,
}

/// Zuweisung eines Plan-Knotens an einen ausführenden Worker.
///
/// # Description
/// Verknüpft einen Knoten mit dem verantwortlichen Worker, einem
/// Versuchszähler (für Retries nach Fehlschlägen) und optional einer
/// Job-Referenz. `harw-plan` bleibt dabei bewusst frei von einer Abhängigkeit
/// auf `harw-job-runtime` — die Job-Referenz wird als reiner `String`
/// (`WorkId`-Repräsentation) geführt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    /// Bezeichner des zugewiesenen Workers (z. B. Agent-Name oder Worker-ID).
    pub worker: String,
    /// Nummer des aktuellen Ausführungsversuchs (**1-basiert**: der erste
    /// Versuch trägt `1`). `Reopen` erhöht den Wert, die Task-Identität bleibt
    /// erhalten (coding-philosophy §15: Retry ≠ neue Aufgabe).
    #[serde(default)]
    pub attempt: u32,
    /// Optionale Referenz auf einen extern ausgeführten Job, als `WorkId`
    /// in String-Form. `None`, solange kein Job gestartet wurde.
    #[serde(default)]
    pub job: Option<String>,
}

/// Ein Knoten im Dependency-Graph eines Plans.
///
/// # Description
/// Enthält alle Informationen zu einer einzelnen Arbeitseinheit:
/// Ziel, Abhängigkeiten, Scope, Kriterien, Invalidierungsbedingungen,
/// Status, Evidenz, Art, Wellen-Zuordnung, Worker-Zuweisung und
/// Eltern-Beziehung.
///
/// `created_at` und `updated_at` werden vom Store gesetzt (Design-Doc §7).
///
/// # Rückwärtskompatibilität
/// `kind`, `wave`, `assignment` und `parent` tragen `#[serde(default)]`:
/// Alt-Snapshots ohne diese Felder deserialisieren weiterhin erfolgreich,
/// mit `kind = PlanNodeKind::Coding`, `wave = None`, `assignment = None`
/// und `parent = None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanNode {
    /// Eindeutiger Bezeichner des Knotens.
    pub id: TaskId,
    /// Beschreibung des Ziels dieses Knotens.
    pub objective: String,
    /// Abhängigkeiten (müssen vor diesem Knoten abgeschlossen sein).
    pub dependencies: Vec<TaskId>,
    /// Eingangsverträge, die dieser Knoten konsumiert.
    pub input_contracts: Vec<ContractRef>,
    /// Ausgangsverträge, die dieser Knoten produziert.
    pub output_contracts: Vec<ContractRef>,
    /// Lesebereiche (Pfade/Symbole, die dieser Knoten lesen darf).
    pub read_scope: Vec<PathOrSymbol>,
    /// Schreibbereiche (Pfade/Symbole, die dieser Knoten exklusiv mutieren darf).
    pub write_scope: Vec<PathOrSymbol>,
    /// Verbotene Bereiche (dürfen weder gelesen noch geschrieben werden).
    pub forbidden_scope: Vec<PathOrSymbol>,
    /// Akzeptanzkriterien, die vor Abschluss erfüllt sein müssen.
    pub acceptance_criteria: Vec<Criterion>,
    /// Bedingungen, unter denen der Knoten invalidiert wird.
    pub invalidation_conditions: Vec<InvalidationCondition>,
    /// Aktueller Lebenszyklus-Status.
    pub status: PlanNodeStatus,
    /// Angehängte Evidenz-Nachweise.
    pub evidence: Vec<EvidenceRef>,
    /// Art des Knotens (Standard: [`PlanNodeKind::Coding`]). Steuert, wie der
    /// Knoten von Orchestrierung und Worker-Auswahl behandelt wird.
    #[serde(default)]
    pub kind: PlanNodeKind,
    /// Nummer der parallelen Ausführungswelle, der dieser Knoten zugeordnet
    /// ist. `None`, solange keine Wellenplanung stattgefunden hat.
    #[serde(default)]
    pub wave: Option<u32>,
    /// Aktuelle Zuweisung an einen ausführenden Worker, falls vorhanden.
    #[serde(default)]
    pub assignment: Option<Assignment>,
    /// Eltern-Knoten, falls dieser Knoten durch eine `Expand`-Aktion aus
    /// einem [`PlanNodeKind::Composite`]-Knoten erzeugt wurde. `None` für
    /// Wurzelknoten (nicht aus `Expand` entstanden).
    #[serde(default)]
    pub parent: Option<TaskId>,
    /// Zeitpunkt der Erstellung (wird vom Store gesetzt).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Zeitpunkt der letzten Aktualisierung (wird vom Store gesetzt).
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Ein vollständiger, versionierter Implementierungsplan.
///
/// # Description
/// Enthält alle Knoten, das Ziel-Statement und Revisionsmetadaten.
/// `created_at` und `updated_at` werden vom Store gesetzt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Eindeutiger Bezeichner des Plans.
    pub id: PlanId,
    /// Aktuelle Revisionsnummer (monoton steigend).
    pub revision: RevisionId,
    /// Vorgänger-Revision (falls vorhanden).
    pub parent_revision: Option<RevisionId>,
    /// Ziel-Statement des Plans (Kurzform; die belastbare Zielbeschreibung
    /// steht im gebundenen [`crate::goal::Goal`]).
    pub goal_statement: String,
    /// Bindung an ein persistiertes Goal (`GoalId` in String-Form).
    ///
    /// Der Plan ist nur die *aktuelle Strategie*; das Goal beschreibt den
    /// gewünschten Endzustand und überlebt Plan-Revisionen (philosophy.md §5).
    /// Wird durch [`crate::actions::PlanAction::BindGoal`] gesetzt.
    #[serde(default)]
    pub goal_id: Option<String>,
    /// Alle Knoten des Plans.
    pub nodes: Vec<PlanNode>,
    /// Zeitpunkt der Erstellung (wird vom Store gesetzt).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Zeitpunkt der letzten Aktualisierung (wird vom Store gesetzt).
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    fn make_node(id: &str) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            objective: "Test objective".to_owned(),
            dependencies: vec![],
            input_contracts: vec![],
            output_contracts: vec![],
            read_scope: vec![],
            write_scope: vec![PathOrSymbol::new("src/lib.rs")],
            forbidden_scope: vec![],
            acceptance_criteria: vec![],
            invalidation_conditions: vec![],
            status: PlanNodeStatus::Draft,
            evidence: vec![],
            kind: PlanNodeKind::default(),
            wave: None,
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn test_plan_node_serde_roundtrip() {
        let node = make_node("t-001");
        let json = serde_json::to_string(&node).expect("serialize PlanNode");
        let back: PlanNode = serde_json::from_str(&json).expect("deserialize PlanNode");
        assert_eq!(back.id, node.id);
        assert_eq!(back.status, PlanNodeStatus::Draft);
    }

    #[test]
    fn test_plan_node_status_snake_case() {
        let cases = [
            (PlanNodeStatus::Draft, "\"draft\""),
            (PlanNodeStatus::Ready, "\"ready\""),
            (PlanNodeStatus::InProgress, "\"in_progress\""),
            (PlanNodeStatus::Blocked, "\"blocked\""),
            (PlanNodeStatus::Completed, "\"completed\""),
            (PlanNodeStatus::Superseded, "\"superseded\""),
            (PlanNodeStatus::Invalidated, "\"invalidated\""),
        ];
        for (status, expected) in &cases {
            let json = serde_json::to_string(status).unwrap();
            assert_eq!(
                &json, expected,
                "Status {:?} serialisiert nicht korrekt",
                status
            );
        }
    }

    /// Simuliert einen Alt-Snapshot ohne `kind`/`wave`/`assignment`/`parent`
    /// und prüft, dass die Deserialisierung dank `#[serde(default)]`
    /// weiterhin gelingt und die erwarteten Default-Werte greifen.
    #[test]
    fn test_plan_node_deserializes_legacy_json_without_new_fields() {
        let node = make_node("t-legacy");
        let mut value = serde_json::to_value(&node).expect("serialize PlanNode to Value");
        let obj = value
            .as_object_mut()
            .expect("PlanNode serialisiert als JSON-Objekt");
        obj.remove("kind");
        obj.remove("wave");
        obj.remove("assignment");
        obj.remove("parent");

        let restored: PlanNode =
            serde_json::from_value(value).expect("deserialize legacy PlanNode ohne neue Felder");

        assert_eq!(restored.kind, PlanNodeKind::default());
        assert_eq!(restored.kind, PlanNodeKind::Coding);
        assert_eq!(restored.wave, None);
        assert_eq!(restored.assignment, None);
        assert_eq!(restored.parent, None);
    }

    /// Prüft den Serde-Roundtrip aller `EvidenceKind`-Varianten sowie die
    /// tolerante Deserialisierung von kebab-case- und unbekannten Werten
    /// (Fallback auf `Other`).
    #[test]
    fn test_evidence_kind_roundtrip_and_unknown_fallback() {
        let cases = [
            (EvidenceKind::Finding, "\"finding\""),
            (EvidenceKind::CargoTest, "\"cargo_test\""),
            (EvidenceKind::Clippy, "\"clippy\""),
            (EvidenceKind::Diff, "\"diff\""),
            (EvidenceKind::TraceSpan, "\"trace_span\""),
            (EvidenceKind::Manual, "\"manual\""),
            (EvidenceKind::Job, "\"job\""),
            (EvidenceKind::Other, "\"other\""),
        ];
        for (kind, expected_json) in cases {
            let json = serde_json::to_string(&kind).expect("serialize EvidenceKind");
            assert_eq!(
                json, expected_json,
                "unerwartete Serialisierung für {kind:?}"
            );
            let back: EvidenceKind =
                serde_json::from_str(&json).expect("deserialize EvidenceKind");
            assert_eq!(back, kind, "Roundtrip schlägt fehl für {kind:?}");
        }

        // kebab-case wird toleriert und auf denselben Wert wie snake_case gemappt.
        let kebab: EvidenceKind =
            serde_json::from_str("\"cargo-test\"").expect("deserialize kebab-case Wert");
        assert_eq!(kebab, EvidenceKind::CargoTest);

        // Unbekannte Werte fallen auf `Other`, statt einen Fehler zu erzeugen.
        let unknown: EvidenceKind = serde_json::from_str("\"totally-unknown-value\"")
            .expect("deserialize unbekannten Wert");
        assert_eq!(unknown, EvidenceKind::Other);
    }

    /// Kanonischer Test-Nachweis ohne `digest` — Ausgangspunkt für die
    /// `EvidenceRef`-Tests unten.
    fn make_evidence() -> EvidenceRef {
        EvidenceRef {
            kind: EvidenceKind::CargoTest,
            locator: "cargo test".to_owned(),
            attached_at: OffsetDateTime::UNIX_EPOCH,
            actor: "ci".to_owned(),
            digest: None,
        }
    }

    /// Roundtrip mit gesetztem `digest`: die Hex-Serialisierung von
    /// [`harw_types::ContentDigest`] übersteht Serialisierung und
    /// Deserialisierung unverändert.
    #[test]
    fn test_evidence_ref_serde_roundtrip_with_digest() {
        let evidence = EvidenceRef {
            digest: Some(harw_types::ContentDigest::of(b"evidence content")),
            ..make_evidence()
        };
        let json = serde_json::to_string(&evidence).expect("serialize EvidenceRef mit digest");
        let back: EvidenceRef =
            serde_json::from_str(&json).expect("deserialize EvidenceRef mit digest");
        assert_eq!(back.digest, evidence.digest);
        assert_eq!(back.kind, evidence.kind);
        assert_eq!(back.locator, evidence.locator);
    }

    /// Roundtrip ohne `digest`: `None` bleibt nach Serialisierung und
    /// Deserialisierung `None`.
    #[test]
    fn test_evidence_ref_serde_roundtrip_without_digest() {
        let evidence = make_evidence();
        let json = serde_json::to_string(&evidence).expect("serialize EvidenceRef ohne digest");
        let back: EvidenceRef =
            serde_json::from_str(&json).expect("deserialize EvidenceRef ohne digest");
        assert_eq!(back.digest, None);
    }

    /// Der wichtigste Test: JSON ohne das Feld `digest` (Simulation einer
    /// Bestandsdatei von vor diesem Feld) deserialisiert erfolgreich und
    /// ergibt `None`. Das ist die Zusage "bestehende Dateien bleiben lesbar"
    /// als Test, nicht als Absicht.
    #[test]
    fn test_evidence_ref_deserializes_legacy_json_without_digest_field() {
        let legacy = serde_json::json!({
            "kind": "cargo_test",
            "locator": "cargo test",
            "attached_at": "1970-01-01T00:00:00Z",
            "actor": "ci",
        });
        assert!(
            legacy.get("digest").is_none(),
            "Testaufbau: Legacy-JSON darf kein digest-Feld enthalten"
        );

        let restored: EvidenceRef =
            serde_json::from_value(legacy).expect("deserialize Legacy-EvidenceRef ohne digest");
        assert_eq!(restored.digest, None);
        assert_eq!(restored.kind, EvidenceKind::CargoTest);
        assert_eq!(restored.locator, "cargo test");
    }

    /// Ein `EvidenceRef` ohne `digest` serialisiert kein `"digest": null` —
    /// `skip_serializing_if` muss das Feld vollständig auslassen, nicht nur
    /// auf `null` setzen.
    #[test]
    fn test_evidence_ref_without_digest_omits_null_in_json() {
        let evidence = make_evidence();
        let json = serde_json::to_string(&evidence).expect("serialize EvidenceRef ohne digest");
        assert!(
            !json.contains("digest"),
            "JSON darf kein digest-Feld enthalten, wenn digest None ist: {json}"
        );
    }
}
