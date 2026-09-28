//! Kerntypen des normalisierten Recherche-Vertrags.
//!
//! Verantwortungsbereich: `ResearchQuestion` (gebundene Frage an einen
//! read-only Sub-Agenten), `ResearchFinding` (normalisiertes Ergebnis) sowie
//! die Bausteine `SourceReference`, `VersionReference`, `Confidence` und
//! `FindingBundle` (coding-philosophy.md §4: "Research Can Fan Out
//! Aggressively" — Fragen tragen Quellgrenzen, ein erwartetes Ausgabeschema
//! und eine Stop-Bedingung; Ergebnisse sind normalisiert statt Freitext).
//!
//! Das analytische Handwerk aus `docs/design/wargaming-and-analysis.md` §2.3
//! ist additiv abgebildet: Quellenbewertung nach Admiralty/NATO
//! (`SourceReliability`, `InfoCredibility`), eine von `Confidence` getrennte
//! `Likelihood`, konkurrierende Hypothesen (`HypothesisAssessment`),
//! Schlüsselannahmen (`KeyAssumption`), Indikatoren (`Indicator`) und
//! abweichende Einschätzungen. Alle neuen Felder tragen `#[serde(default)]`,
//! ältere Findings bleiben damit deserialisierbar.
//!
//! Der Vertrag ist ökosystem-neutral: `VersionReference` benennt ein Paket
//! (`package`) samt `ecosystem` (Vorgabe `"cargo"`); das Alt-Feld
//! `crate_name` wird beim Einlesen weiterhin akzeptiert.
//!
//! `QuestionId` ist ein lokales Newtype im Stil von `harw-plan/src/ids.rs`
//! (infallibles `FromStr`, kein Validierungszwang).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Eindeutiger Bezeichner einer Recherche-Frage.
///
/// # Description
/// Newtype über `String`, analog zu `harw_plan::ids::PlanId`. Formatierung
/// wird nicht erzwungen.
///
/// # Examples
/// ```rust,no_run
/// use harw_research::QuestionId;
/// let id = QuestionId::new("q-crate-versions");
/// assert_eq!(id.as_str(), "q-crate-versions");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuestionId(String);

impl QuestionId {
    /// Erstellt eine neue `QuestionId` aus einem String.
    ///
    /// # Arguments
    /// - `s` (`impl Into<String>`): roher Bezeichner.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Gibt die innere String-Repräsentation zurück.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for QuestionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for QuestionId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

/// Erlaubte Quellklassen für Recherche-Belege.
///
/// Grenzt die von `coding-philosophy.md` §4 geforderten "permitted source
/// classes" auf einen geschlossenen, serialisierbaren Satz ein.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceClass {
    /// Datei oder Verzeichnis im lokalen Arbeitsbereich.
    LocalSource,
    /// Quelltext oder Metadaten aus der crates.io-Registry.
    ///
    /// Bleibt für Abwärtskompatibilität erhalten; für neue Findings und
    /// andere Ökosysteme siehe [`SourceClass::PackageRegistrySource`].
    CargoRegistrySource,
    /// Quelltext oder Metadaten aus einer beliebigen Paket-Registry
    /// (crates.io, npm, PyPI, Maven Central, …).
    PackageRegistrySource,
    /// Offizielle Dokumentation eines Anbieters/Projekts.
    OfficialDocs,
    /// Ein Repository (z. B. GitHub) außerhalb der Registry-Metadaten.
    Repository,
    /// Release Notes / Changelogs.
    ReleaseNotes,
    /// Eine formale Spezifikation oder ein Standard.
    Standard,
    /// Allgemeine Web-Quelle ohne speziellere Einordnung.
    Web,
}

/// Aktualitätsanforderung einer Recherche-Frage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// Keine Aktualitätsanforderung.
    AnyTime,
    /// Belege müssen zu einem bestimmten Zeitpunkt gültig sein.
    AsOf(jiff::Timestamp),
}

/// Grenzen einer Recherche-Frage: erlaubte Pfade, Crates, URLs und Quellklassen.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct QuestionScope {
    /// Erlaubte Dateipfade/Verzeichnisse im lokalen Arbeitsbereich.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Erlaubte Crate-Namen (z. B. für Registry-/Docs-Recherche).
    #[serde(default)]
    pub crates: Vec<String>,
    /// Erlaubte URLs oder URL-Präfixe.
    #[serde(default)]
    pub urls: Vec<String>,
    /// Erlaubte Quellklassen (leer = keine Einschränkung).
    #[serde(default)]
    pub sources: Vec<SourceClass>,
}

/// Eine an einen read-only Sub-Agenten gebundene Recherche-Frage.
///
/// # Description
/// Trägt alle in `coding-philosophy.md` §4 geforderten Bestandteile: eine
/// gebundene Frage, Quellgrenzen (`scope`), ein erwartetes Ausgabeformat, eine
/// Aktualitätsanforderung und eine klare Stop-Bedingung.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchQuestion {
    /// Eindeutiger Bezeichner der Frage.
    pub id: QuestionId,
    /// Die eigentliche Frage im Klartext.
    pub question: String,
    /// Erlaubte Quellen/Pfade/Crates/URLs für die Beantwortung.
    pub scope: QuestionScope,
    /// Beschreibung des erwarteten Ausgabeformats/-inhalts.
    pub expected_output: String,
    /// Aktualitätsanforderung an die Belege.
    pub freshness: Freshness,
    /// Bedingung, unter der die Recherche als abgeschlossen gilt.
    pub stop_condition: String,
    /// Optionale Zuordnung zu einer Plan-Task (z. B. `harw_plan::ids::TaskId`
    /// als Roh-String, um eine Cross-Crate-Abhängigkeit zu vermeiden).
    #[serde(default)]
    pub owner_task: Option<String>,
}

/// Referenz auf einen einzelnen Beleg.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceReference {
    /// Klasse der Quelle.
    pub kind: SourceClass,
    /// Konkreter Fundort (Pfad, URL, Crate-Name@Version, …).
    pub locator: String,
    /// Zeitpunkt des Abrufs.
    pub retrieved_at: jiff::Timestamp,
    /// Optionaler Integritäts-Digest (z. B. Hash des Inhalts).
    #[serde(default)]
    pub digest: Option<String>,
    /// Kurzes wörtliches Zitat/Auszug als Beleg.
    #[serde(default)]
    pub excerpt: String,
    /// Zuverlässigkeit der Quelle (Admiralty/NATO-Achse 1); `None` = nicht
    /// bewertet.
    #[serde(default)]
    pub reliability: Option<SourceReliability>,
    /// Glaubwürdigkeit der Information (Admiralty/NATO-Achse 2); `None` =
    /// nicht bewertet.
    #[serde(default)]
    pub credibility: Option<InfoCredibility>,
    /// Locator einer anderen Quelle, von der diese abhängt (macht
    /// Zirkelbestätigung sichtbar).
    #[serde(default)]
    pub derived_from: Option<String>,
}

/// Zuverlässigkeit einer Quelle (Admiralty/NATO-Achse 1).
///
/// # Description
/// Serialisiert klein (`"a"` … `"f"`); beim Einlesen werden auch die
/// üblichen Großbuchstaben (`"A"` … `"F"`) akzeptiert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceReliability {
    /// Vollständig zuverlässig.
    #[serde(alias = "A")]
    A,
    /// Überwiegend zuverlässig.
    #[serde(alias = "B")]
    B,
    /// Ziemlich zuverlässig.
    #[serde(alias = "C")]
    C,
    /// Nicht immer zuverlässig.
    #[serde(alias = "D")]
    D,
    /// Unzuverlässig.
    #[serde(alias = "E")]
    E,
    /// Zuverlässigkeit nicht beurteilbar.
    #[serde(alias = "F")]
    F,
}

/// Glaubwürdigkeit einer Information (Admiralty/NATO-Achse 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfoCredibility {
    /// Durch unabhängige Quellen bestätigt.
    Confirmed,
    /// Wahrscheinlich zutreffend.
    ProbablyTrue,
    /// Möglicherweise zutreffend.
    PossiblyTrue,
    /// Zweifelhaft.
    Doubtful,
    /// Unwahrscheinlich.
    Improbable,
    /// Wahrheitsgehalt nicht beurteilbar.
    CannotJudge,
}

/// Vorgabe-Ökosystem für [`VersionReference::ecosystem`] (Abwärtskompatibilität
/// zu Findings, die nur Cargo-Crates kannten).
pub const DEFAULT_ECOSYSTEM: &str = "cargo";

fn default_ecosystem() -> String {
    DEFAULT_ECOSYSTEM.to_owned()
}

/// Verifizierte Versions-/Feature-/Mindestversions-Angabe zu einem Paket.
///
/// # Description
/// Ökosystem-neutral: `package` ist der Paketname im jeweiligen `ecosystem`
/// (z. B. `"cargo"`, `"npm"`, `"pypi"`, `"maven"`). Ältere Findings mit
/// `crate_name` und ohne `ecosystem` werden als Cargo-Crate gelesen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionReference {
    /// Name des Pakets (Alt-Feldname beim Einlesen: `crate_name`).
    #[serde(alias = "crate_name")]
    pub package: String,
    /// Paket-Ökosystem (z. B. `"cargo"`, `"npm"`, `"pypi"`); Vorgabe `"cargo"`.
    #[serde(default = "default_ecosystem")]
    pub ecosystem: String,
    /// Verifizierte Versionsangabe.
    pub version: String,
    /// Optionale Mindestversion der Laufzeit/Toolchain (bei Cargo: MSRV).
    #[serde(default)]
    pub msrv: Option<String>,
    /// Verifizierte/relevante Feature-Flags bzw. Extras.
    #[serde(default)]
    pub features: Vec<String>,
    /// Womit die Version abgeglichen wurde (z. B. "crates.io", "docs.rs", "npmjs.com").
    pub verified_against: String,
}

/// Konfidenzstufe eines Recherche-Ergebnisses, aufsteigend geordnet.
///
/// # Description
/// `Ord` wird für den Vertrag aus `agent-definition-dsl.md` §13 gebraucht:
/// `confidence >= Medium` löst die Beleg-Pflicht in `validate::validate_finding`
/// aus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Schwacher, unbestätigter Anhaltspunkt.
    Low,
    /// Plausibel, aber nicht vollständig verifiziert.
    Medium,
    /// Durch mindestens eine belastbare Quelle gestützt.
    High,
    /// Gegen eine Primärquelle verifiziert.
    Verified,
}

/// Geschätzte Wahrscheinlichkeit einer Aussage, aufsteigend geordnet.
///
/// # Description
/// Bewusst getrennt von [`Confidence`]: `Likelihood` schätzt, *wie
/// wahrscheinlich* die Aussage zutrifft; `Confidence` beschreibt, *wie
/// belastbar* die Analyse dahinter ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Likelihood {
    /// Nahezu ausgeschlossen.
    Remote,
    /// Sehr unwahrscheinlich.
    VeryUnlikely,
    /// Unwahrscheinlich.
    Unlikely,
    /// Etwa gleich wahrscheinlich wie das Gegenteil.
    RoughlyEven,
    /// Wahrscheinlich.
    Likely,
    /// Sehr wahrscheinlich.
    VeryLikely,
    /// Nahezu sicher.
    AlmostCertain,
}

/// Stand einer konkurrierenden Hypothese (Analysis of Competing Hypotheses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatus {
    /// Führende Hypothese.
    Leading,
    /// Weiterhin tragfähig.
    Viable,
    /// Durch Belege geschwächt.
    Weakened,
    /// Durch Belege widerlegt.
    Refuted,
}

/// Bewertung einer konkurrierenden Hypothese.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HypothesisAssessment {
    /// Kurzbezeichner, z. B. `"H1"`.
    pub id: String,
    /// Aussage der Hypothese.
    pub statement: String,
    /// Aktueller Stand.
    pub status: HypothesisStatus,
    /// Indizes in `ResearchFinding::evidence`, die zur Hypothese passen.
    #[serde(default)]
    pub consistent_evidence: Vec<usize>,
    /// Indizes in `ResearchFinding::evidence`, die ihr widersprechen.
    #[serde(default)]
    pub inconsistent_evidence: Vec<usize>,
    /// Inkonsistenz-Wert (höher = stärker widerlegt).
    #[serde(default)]
    pub inconsistency_score: f32,
}

/// Stand einer Schlüsselannahme (Key Assumptions Check).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionStatus {
    /// Durch Belege gestützt.
    Supported,
    /// Nur mit Vorbehalt haltbar.
    Caveated,
    /// Nicht gestützt.
    Unsupported,
}

/// Eine Schlüsselannahme, auf der die Schlussfolgerung beruht.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyAssumption {
    /// Kurzbezeichner, z. B. `"A1"`.
    pub id: String,
    /// Aussage der Annahme.
    pub statement: String,
    /// Aktueller Stand.
    pub status: AssumptionStatus,
    /// Liegt die Annahme auf dem kritischen Pfad der Schlussfolgerung?
    #[serde(default)]
    pub on_critical_path: bool,
    /// Begründung der Einstufung.
    #[serde(default)]
    pub rationale: String,
}

/// Beobachtbarer Indikator (Indicators & Warnings), der Hypothesen oder
/// Annahmen stützt bzw. schwächt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Indicator {
    /// Kurzbezeichner, z. B. `"I1"`.
    pub id: String,
    /// Was konkret beobachtet wird.
    pub observable: String,
    /// IDs der Hypothesen/Annahmen, auf die der Indikator einzahlt.
    pub supports: Vec<String>,
    /// Ausgangslage.
    #[serde(default)]
    pub baseline: String,
    /// Schwelle, ab der der Indikator als ausgelöst gilt.
    #[serde(default)]
    pub threshold: String,
    /// Prüfintervall, z. B. `"7d"` oder `"per-release"`.
    #[serde(default)]
    pub check_every: Option<String>,
}

/// Normalisiertes Recherche-Ergebnis eines read-only Sub-Agenten.
///
/// # Description
/// Entspricht der `ResearchFinding`-Skizze aus `coding-philosophy.md` §4;
/// wird von Sub-Agenten als validiertes JSON statt Freitext geliefert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchFinding {
    /// Frage, auf die sich dieses Ergebnis bezieht.
    pub question_id: QuestionId,
    /// Kernaussage/Schlussfolgerung.
    pub conclusion: String,
    /// Belege, die die Schlussfolgerung stützen.
    #[serde(default)]
    pub evidence: Vec<SourceReference>,
    /// Verifizierte Versions-/Feature-/MSRV-Angaben.
    #[serde(default)]
    pub verified_versions: Vec<VersionReference>,
    /// Randbedingungen, die aus der Recherche folgen.
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Kompatibilitätshinweise.
    #[serde(default)]
    pub compatibility_notes: Vec<String>,
    /// Offen gebliebene Fragen.
    #[serde(default)]
    pub unresolved_questions: Vec<String>,
    /// Konfidenzstufe des Ergebnisses.
    pub confidence: Confidence,
    /// Bezeichner des berichtenden Agenten.
    pub produced_by: String,
    /// Zeitpunkt der Erstellung.
    pub produced_at: jiff::Timestamp,
    /// Geschätzte Wahrscheinlichkeit der Kernaussage (getrennt von
    /// `confidence`); `None` = nicht eingeschätzt.
    #[serde(default)]
    pub likelihood: Option<Likelihood>,
    /// Begründung der Konfidenzstufe.
    #[serde(default)]
    pub confidence_rationale: String,
    /// Konkurrierende Hypothesen samt Bewertung.
    #[serde(default)]
    pub hypotheses: Vec<HypothesisAssessment>,
    /// Schlüsselannahmen der Schlussfolgerung.
    #[serde(default)]
    pub key_assumptions: Vec<KeyAssumption>,
    /// Beobachtbare Indikatoren für Hypothesen/Annahmen.
    #[serde(default)]
    pub indicators: Vec<Indicator>,
    /// Abweichende Einschätzungen (Red Team, zweiter Analyst).
    #[serde(default)]
    pub dissent: Vec<String>,
}

/// Sammlung mehrerer `ResearchFinding`s samt offener Abdeckungslücken.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FindingBundle {
    /// Alle in dieser Runde erzeugten Ergebnisse.
    #[serde(default)]
    pub findings: Vec<ResearchFinding>,
    /// Fragen oder Aspekte, die keine Deckung durch ein `ResearchFinding` haben.
    #[serde(default)]
    pub coverage_gaps: Vec<String>,
    /// Welche Findings (Indizes in `findings`) welche Frage beantworten.
    #[serde(default)]
    pub requirements_trace: Vec<(QuestionId, Vec<usize>)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn ts() -> TestResult<jiff::Timestamp> {
        "2026-08-27T00:00:00Z"
            .parse()
            .map_err(ctx("Zeitstempel parsen"))
    }

    fn sample_finding() -> TestResult<ResearchFinding> {
        Ok(ResearchFinding {
            question_id: QuestionId::new("q-1"),
            conclusion: "jiff 0.2.32 is current".to_owned(),
            evidence: vec![SourceReference {
                kind: SourceClass::CargoRegistrySource,
                locator: "crates.io/crates/jiff".to_owned(),
                retrieved_at: ts()?,
                digest: None,
                excerpt: "version 0.2.32".to_owned(),
                reliability: Some(SourceReliability::A),
                credibility: Some(InfoCredibility::Confirmed),
                derived_from: None,
            }],
            verified_versions: vec![VersionReference {
                package: "jiff".to_owned(),
                ecosystem: DEFAULT_ECOSYSTEM.to_owned(),
                version: "0.2.32".to_owned(),
                msrv: Some("1.85".to_owned()),
                features: vec!["serde".to_owned()],
                verified_against: "crates.io".to_owned(),
            }],
            constraints: vec![],
            compatibility_notes: vec![],
            unresolved_questions: vec![],
            confidence: Confidence::High,
            produced_by: "explorer-1".to_owned(),
            produced_at: ts()?,
            likelihood: Some(Likelihood::VeryLikely),
            confidence_rationale: "registry and docs agree".to_owned(),
            hypotheses: vec![HypothesisAssessment {
                id: "H1".to_owned(),
                statement: "0.2.32 is the latest release".to_owned(),
                status: HypothesisStatus::Leading,
                consistent_evidence: vec![0],
                inconsistent_evidence: vec![],
                inconsistency_score: 0.0,
            }],
            key_assumptions: vec![KeyAssumption {
                id: "A1".to_owned(),
                statement: "registry index is current".to_owned(),
                status: AssumptionStatus::Supported,
                on_critical_path: true,
                rationale: String::new(),
            }],
            indicators: vec![Indicator {
                id: "I1".to_owned(),
                observable: "new jiff release".to_owned(),
                supports: vec!["H1".to_owned()],
                baseline: "0.2.32".to_owned(),
                threshold: "any newer version".to_owned(),
                check_every: Some("7d".to_owned()),
            }],
            dissent: vec![],
        })
    }

    #[test]
    fn test_question_id_roundtrip_and_from_str() -> TestResult {
        let id = QuestionId::new("q-abc");
        let json = serde_json::to_string(&id).map_err(ctx("QuestionId serialisieren"))?;
        let back: QuestionId =
            serde_json::from_str(&json).map_err(ctx("QuestionId deserialisieren"))?;
        assert_eq!(id, back);

        let parsed: QuestionId = "q-abc".parse().map_err(ctx("QuestionId parsen"))?;
        assert_eq!(parsed.as_str(), "q-abc");
        assert_eq!(parsed.to_string(), "q-abc");
        Ok(())
    }

    #[test]
    fn test_source_class_serializes_snake_case() -> TestResult {
        let json = serde_json::to_string(&SourceClass::CargoRegistrySource)
            .map_err(ctx("SourceClass serialisieren"))?;
        assert_eq!(json, "\"cargo_registry_source\"");
        let back: SourceClass =
            serde_json::from_str(&json).map_err(ctx("SourceClass deserialisieren"))?;
        assert_eq!(back, SourceClass::CargoRegistrySource);

        let json = serde_json::to_string(&SourceClass::PackageRegistrySource)
            .map_err(ctx("SourceClass serialisieren"))?;
        assert_eq!(json, "\"package_registry_source\"");
        let back: SourceClass =
            serde_json::from_str(&json).map_err(ctx("SourceClass deserialisieren"))?;
        assert_eq!(back, SourceClass::PackageRegistrySource);
        Ok(())
    }

    #[test]
    fn test_version_reference_accepts_legacy_crate_name_and_defaults_ecosystem() -> TestResult {
        let legacy: VersionReference = serde_json::from_str(
            r#"{"crate_name":"jiff","version":"0.2.32","verified_against":"crates.io"}"#,
        )
        .map_err(ctx("Alt-VersionReference deserialisieren"))?;
        assert_eq!(legacy.package, "jiff");
        assert_eq!(legacy.ecosystem, DEFAULT_ECOSYSTEM);

        let generic: VersionReference = serde_json::from_str(
            r#"{"package":"left-pad","ecosystem":"npm","version":"1.3.0","verified_against":"npmjs.com"}"#,
        )
        .map_err(ctx("generische VersionReference deserialisieren"))?;
        assert_eq!(generic.package, "left-pad");
        assert_eq!(generic.ecosystem, "npm");

        let json = serde_json::to_value(&generic).map_err(ctx("VersionReference serialisieren"))?;
        assert_eq!(json["package"], "left-pad");
        assert!(json.get("crate_name").is_none());
        Ok(())
    }

    #[test]
    fn test_legacy_finding_without_tradecraft_fields_deserializes() -> TestResult {
        let raw = r#"{
            "question_id": "q-1",
            "conclusion": "c",
            "evidence": [{
                "kind": "cargo_registry_source",
                "locator": "crates.io/crates/jiff",
                "retrieved_at": "2026-08-27T00:00:00Z"
            }],
            "confidence": "medium",
            "produced_by": "explorer-1",
            "produced_at": "2026-08-27T00:00:00Z"
        }"#;
        let finding: ResearchFinding =
            serde_json::from_str(raw).map_err(ctx("Alt-Finding deserialisieren"))?;
        assert!(finding.likelihood.is_none());
        assert!(finding.confidence_rationale.is_empty());
        assert!(finding.hypotheses.is_empty());
        assert!(finding.key_assumptions.is_empty());
        assert!(finding.indicators.is_empty());
        assert!(finding.dissent.is_empty());
        let source = finding
            .evidence
            .first()
            .ok_or(crate::test_support::TestError::Missing("evidence[0]"))?;
        assert!(source.reliability.is_none());
        assert!(source.credibility.is_none());
        assert!(source.derived_from.is_none());
        Ok(())
    }

    #[test]
    fn test_source_reliability_accepts_upper_case_alias() -> TestResult {
        let json = serde_json::to_string(&SourceReliability::B)
            .map_err(ctx("Reliability serialisieren"))?;
        assert_eq!(json, "\"b\"");
        let upper: SourceReliability =
            serde_json::from_str("\"B\"").map_err(ctx("Reliability 'B' deserialisieren"))?;
        assert_eq!(upper, SourceReliability::B);
        let cred: InfoCredibility = serde_json::from_str("\"probably_true\"")
            .map_err(ctx("Credibility deserialisieren"))?;
        assert_eq!(cred, InfoCredibility::ProbablyTrue);
        Ok(())
    }

    #[test]
    fn test_likelihood_is_ordered_and_snake_case() -> TestResult {
        assert!(Likelihood::Remote < Likelihood::RoughlyEven);
        assert!(Likelihood::VeryLikely < Likelihood::AlmostCertain);
        let json = serde_json::to_string(&Likelihood::AlmostCertain)
            .map_err(ctx("Likelihood serialisieren"))?;
        assert_eq!(json, "\"almost_certain\"");
        Ok(())
    }

    #[test]
    fn test_freshness_any_time_and_as_of_roundtrip() -> TestResult {
        let any = Freshness::AnyTime;
        let json = serde_json::to_string(&any).map_err(ctx("Freshness serialisieren"))?;
        assert_eq!(
            serde_json::from_str::<Freshness>(&json).map_err(ctx("Freshness deserialisieren"))?,
            any
        );

        let as_of = Freshness::AsOf(ts()?);
        let json = serde_json::to_string(&as_of).map_err(ctx("Freshness serialisieren"))?;
        assert_eq!(
            serde_json::from_str::<Freshness>(&json).map_err(ctx("Freshness deserialisieren"))?,
            as_of
        );
        Ok(())
    }

    #[test]
    fn test_confidence_ordering() {
        assert!(Confidence::Low < Confidence::Medium);
        assert!(Confidence::Medium < Confidence::High);
        assert!(Confidence::High < Confidence::Verified);
    }

    #[test]
    fn test_research_finding_roundtrip() -> TestResult {
        let finding = sample_finding()?;
        let json = serde_json::to_string(&finding).map_err(ctx("ResearchFinding serialisieren"))?;
        let back: ResearchFinding =
            serde_json::from_str(&json).map_err(ctx("ResearchFinding deserialisieren"))?;
        assert_eq!(finding, back);
        Ok(())
    }

    #[test]
    fn test_question_scope_defaults_when_fields_missing() -> TestResult {
        let scope: QuestionScope =
            serde_json::from_str("{}").map_err(ctx("QuestionScope deserialisieren"))?;
        assert!(scope.paths.is_empty());
        assert!(scope.crates.is_empty());
        assert!(scope.urls.is_empty());
        assert!(scope.sources.is_empty());
        Ok(())
    }

    #[test]
    fn test_finding_bundle_defaults_when_fields_missing() -> TestResult {
        let bundle: FindingBundle =
            serde_json::from_str("{}").map_err(ctx("FindingBundle deserialisieren"))?;
        assert!(bundle.findings.is_empty());
        assert!(bundle.coverage_gaps.is_empty());
        assert!(bundle.requirements_trace.is_empty());

        let bundle = FindingBundle {
            findings: vec![sample_finding()?],
            coverage_gaps: vec!["no docs for feature X".to_owned()],
            requirements_trace: vec![(QuestionId::new("q-1"), vec![0])],
        };
        let json = serde_json::to_string(&bundle).map_err(ctx("FindingBundle serialisieren"))?;
        let back: FindingBundle =
            serde_json::from_str(&json).map_err(ctx("FindingBundle deserialisieren"))?;
        assert_eq!(bundle, back);
        Ok(())
    }
}
