//! Kerntypen des normalisierten Recherche-Vertrags.
//!
//! Verantwortungsbereich: `ResearchQuestion` (gebundene Frage an einen
//! read-only Sub-Agenten), `ResearchFinding` (normalisiertes Ergebnis) sowie
//! die Bausteine `SourceReference`, `VersionReference`, `Confidence` und
//! `FindingBundle` (coding-philosophy.md §4: "Research Can Fan Out
//! Aggressively" — Fragen tragen Quellgrenzen, ein erwartetes Ausgabeschema
//! und eine Stop-Bedingung; Ergebnisse sind normalisiert statt Freitext).
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
/// let id: QuestionId = "q-crate-versions".parse().unwrap();
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
    CargoRegistrySource,
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
}

/// Verifizierte Versions-/Feature-/MSRV-Angabe zu einer Crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionReference {
    /// Name der Crate.
    pub crate_name: String,
    /// Verifizierte Versionsangabe.
    pub version: String,
    /// Optionale Mindest-Rust-Version.
    #[serde(default)]
    pub msrv: Option<String>,
    /// Verifizierte/relevante Feature-Flags.
    #[serde(default)]
    pub features: Vec<String>,
    /// Womit die Version abgeglichen wurde (z. B. "crates.io", "docs.rs").
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
            }],
            verified_versions: vec![VersionReference {
                crate_name: "jiff".to_owned(),
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

        let bundle = FindingBundle {
            findings: vec![sample_finding()?],
            coverage_gaps: vec!["no docs for feature X".to_owned()],
        };
        let json = serde_json::to_string(&bundle).map_err(ctx("FindingBundle serialisieren"))?;
        let back: FindingBundle =
            serde_json::from_str(&json).map_err(ctx("FindingBundle deserialisieren"))?;
        assert_eq!(bundle, back);
        Ok(())
    }
}
