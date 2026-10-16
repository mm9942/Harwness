//! Tool-Spezifikationen: das Vokabular, mit dem ein Tool gegenüber dem Modell
//! beschrieben wird.

use crate::schema::JsonSchema;
use serde::Serialize;

/// Newtype für Tool-Namen.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ToolName(pub String);

impl ToolName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Eine Tool-Spezifikation. Aktuell nur Function-Tools.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum ToolSpec {
    #[serde(rename = "function")]
    Function(FunctionToolSpec),
}

/// Modell-/Provider-seitige Spezifikation eines Function-Tools.
///
/// Dieser Typ beschreibt die Oberfläche, die in einen Modell-Request
/// serialisiert wird: Name, Beschreibung, JSON-Parameterschema und
/// Strictness. Er ist **nicht** der allgemeine interne Metadatencontainer für
/// jede Harw-Eigenschaft eines Tools.
///
/// Insbesondere plant
/// `docs/planning/71-semantic-activity-patterns/04-macro-metadata.md`
/// zusätzliche interne Activity-Semantik (Ressourcen-Key, Verb, Effektklasse,
/// Pattern-Eignung). Diese Daten sollen als getrennte Harw-interne
/// Beschreibung modelliert werden, solange kein providerseitiger Grund
/// besteht, sie in `ToolSpec` zu übertragen.
///
/// Ebenso trägt `FunctionToolSpec` keine Laufzeit-Authority: Sichtbarkeit im
/// Modell-Schema ersetzt weder Permission- noch Approval-Prüfungen.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FunctionToolSpec {
    pub name: ToolName,
    pub description: String,
    pub parameters: JsonSchema,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
}

impl ToolSpec {
    pub fn name(&self) -> &str {
        match self {
            ToolSpec::Function(f) => f.name.as_str(),
        }
    }
}
