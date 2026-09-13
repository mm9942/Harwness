//! JSON-Schema-Erzeugung für [`crate::types::ResearchFinding`] (Draft 2020-12).
//!
//! Verantwortungsbereich: Erzeugt ein deklaratives Ausgabeschema, das ein
//! Modell als verbindliches Antwortformat erhält (coding-philosophy.md §4:
//! Sub-Agenten liefern validiertes JSON statt Freitext), sowie einen daraus
//! abgeleiteten Prompt-Text. Validiert selbst nichts — siehe `validate.rs`.

use serde_json::{Value, json};

/// Baut das JSON-Schema (Draft 2020-12) für [`crate::types::ResearchFinding`].
///
/// # Description
/// Jede Objekt-Ebene setzt `"additionalProperties": false`. `required` deckt
/// mindestens `question_id`, `conclusion`, `evidence`, `confidence`,
/// `produced_by` und `produced_at` ab.
///
/// # Returns
/// `serde_json::Value` mit dem vollständigen Schema-Objekt.
#[must_use]
pub fn finding_json_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "ResearchFinding",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "question_id": { "type": "string" },
            "conclusion": { "type": "string" },
            "evidence": {
                "type": "array",
                "items": source_reference_schema()
            },
            "verified_versions": {
                "type": "array",
                "items": version_reference_schema()
            },
            "constraints": { "type": "array", "items": { "type": "string" } },
            "compatibility_notes": { "type": "array", "items": { "type": "string" } },
            "unresolved_questions": { "type": "array", "items": { "type": "string" } },
            "confidence": {
                "type": "string",
                "enum": ["low", "medium", "high", "verified"]
            },
            "produced_by": { "type": "string" },
            "produced_at": { "type": "string", "format": "date-time" }
        },
        "required": [
            "question_id",
            "conclusion",
            "evidence",
            "confidence",
            "produced_by",
            "produced_at"
        ]
    })
}

/// Teilschema für [`crate::types::SourceReference`].
fn source_reference_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kind": {
                "type": "string",
                "enum": [
                    "local_source",
                    "cargo_registry_source",
                    "official_docs",
                    "repository",
                    "release_notes",
                    "standard",
                    "web"
                ]
            },
            "locator": { "type": "string" },
            "retrieved_at": { "type": "string", "format": "date-time" },
            "digest": { "type": ["string", "null"] },
            "excerpt": { "type": "string" }
        },
        "required": ["kind", "locator", "retrieved_at"]
    })
}

/// Teilschema für [`crate::types::VersionReference`].
fn version_reference_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "crate_name": { "type": "string" },
            "version": { "type": "string" },
            "msrv": { "type": ["string", "null"] },
            "features": { "type": "array", "items": { "type": "string" } },
            "verified_against": { "type": "string" }
        },
        "required": ["crate_name", "version", "verified_against"]
    })
}

/// Baut einen kompakten, an ein Modell übergebbaren Prompt-Text aus dem
/// JSON-Schema.
///
/// # Description
/// Bettet [`finding_json_schema`] als eingerücktes JSON in eine knappe
/// Anweisung ein. Für read-only Sub-Agenten gedacht, deren Ergebnis als
/// validiertes JSON statt Freitext geliefert werden soll.
///
/// # Returns
/// Fertiger Text zur direkten Übergabe an ein Modell.
#[must_use]
pub fn finding_schema_prompt() -> String {
    let schema = finding_json_schema();
    let pretty = serde_json::to_string_pretty(&schema).unwrap_or_else(|_| schema.to_string());
    format!(
        "Antworte ausschließlich mit JSON dieser Form (keine Erklärtexte, keine Markdown-Fences):\n{pretty}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_finding_json_schema_uses_draft_2020_12() {
        let schema = finding_json_schema();
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn test_finding_json_schema_required_covers_contract_fields() {
        let schema = finding_json_schema();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for field in [
            "question_id",
            "conclusion",
            "evidence",
            "confidence",
            "produced_by",
            "produced_at",
        ] {
            assert!(
                required.contains(&field),
                "required is missing '{field}': {required:?}"
            );
        }
    }

    #[test]
    fn test_finding_json_schema_properties_present() {
        let schema = finding_json_schema();
        let properties = schema["properties"].as_object().unwrap();
        for field in [
            "question_id",
            "conclusion",
            "evidence",
            "verified_versions",
            "constraints",
            "compatibility_notes",
            "unresolved_questions",
            "confidence",
            "produced_by",
            "produced_at",
        ] {
            assert!(
                properties.contains_key(field),
                "properties is missing '{field}'"
            );
        }
    }

    #[test]
    fn test_finding_schema_prompt_embeds_schema_json() {
        let prompt = finding_schema_prompt();
        assert!(prompt.contains("Antworte ausschließlich mit JSON"));
        assert!(prompt.contains("\"question_id\""));
        assert!(prompt.contains("draft/2020-12"));
    }
}
