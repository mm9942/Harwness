use crate::serde_defaults::default_true;
use serde::{Deserialize, Serialize};

/// Manifest eines modellseitigen Harw-Skills.
///
/// Skills sind im aktuellen System **Instruktionsfragmente**: sie liefern
/// Modellkontext und deklarieren, welche Tool-/MCP-Namen zu ihrer Verwendung
/// gehören. Die eigentliche Authority wird an anderen Grenzen entschieden;
/// ein Skill-Manifest ist kein Berechtigungs-Token.
///
/// ## Abgrenzung zu semantischen Patterns
///
/// `docs/planning/71-semantic-activity-patterns/` plant zusätzlich einen
/// verlinkbaren Pattern-Graphen für beobachtete bzw. ableitbare Laufzeit-
/// strukturen. Dieser Typ bildet diesen Plan **noch nicht** ab. Insbesondere
/// existiert hier derzeit kein `patterns`-Feld.
///
/// Die beabsichtigte langfristige Trennung lautet:
///
/// - Skill: modellfreundliche Anleitung / Projektion;
/// - Pattern: typisierte, versionierte Laufzeitsemantik mit Provenienz;
/// - Authority: eigenständige, vertrauenswürdige Laufzeitgrenze.
///
/// Ein späterer Pattern-Verweis muss deshalb additiv und bewusst in dieses
/// wegen `deny_unknown_fields` streng versionierte Manifest eingeführt
/// werden, statt stillschweigend neue Felder zu akzeptieren.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillToml {
    /// Stabiler Katalogname des Skills.
    pub name: String,
    /// Ob der Skill in dieser Konfigurationsschicht aktiviert ist.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Menschenlesbare Kurzbeschreibung für Katalog- und Auswahloberflächen.
    #[serde(default)]
    pub description: String,
    /// Optionale Instruktionsdatei; ohne Angabe gilt der bestehende Default.
    #[serde(default)]
    pub instructions_file: Option<String>,
    /// Vom Skill referenzierte Tool-Namen; keine Authority-Zusage.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Vom Skill referenzierte MCP-Namen; keine Start-/Netzwerkfreigabe.
    #[serde(default)]
    pub mcps: Vec<String>,
}
