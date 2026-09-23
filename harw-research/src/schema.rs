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
/// Bewährt sich in der Praxis als zu roh: manche Modelle (z. B. `glm-5.3`)
/// antworten mit dem eingebetteten Schema selbst (`{"$schema": …, "title":
/// "ResearchFinding", …}`) statt mit einer ausgefüllten Instanz — der reine
/// Schema-Dump lässt Schema und Instanz zu leicht verwechseln. Für neue
/// Aufrufer siehe [`finding_schema_prompt_with_example`], das dieselbe
/// Vertragsform über eine ausgefüllte Beispielinstanz statt eines
/// Schema-Dumps vermittelt. Diese Funktion bleibt für bestehende Aufrufer und
/// als reine Schema-Referenz erhalten.
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

/// Baut einen Prompt-Text aus Feldliste, ausgefüllter Beispielinstanz und
/// Verhaltensregeln — der empfohlene Ersatz für [`finding_schema_prompt`].
///
/// # Description
/// Adressiert drei in der Praxis beobachtete Vertragsbrüche read-only
/// Kind-Agenten (u. a. Modell `glm-5.3`, siehe `harw-ops::explore`):
/// 1. **Schema-Echo** — ein roher `finding_json_schema()`-Dump lädt dazu ein,
///    das Schema selbst zurückzugeben (`{"$schema": …, "title":
///    "ResearchFinding", …}`) statt einer Instanz. Diese Funktion zeigt
///    stattdessen eine **ausgefüllte Beispielinstanz** mit Platzhalterwerten;
///    die `question_id` ist darin bereits die echte, gestellte ID — das Kind
///    muss sie nicht erfinden oder aus dem Schema ableiten.
/// 2. **Freitext/`{"answers":{}}`** — die explizite Regel im Vorspann
///    verlangt ausdrücklich genau ein JSON-Objekt dieser Form, ohne
///    Erklärtext und ohne Tool-Call-Prosa.
/// 3. **Unbelegte Halluzination** — die Regel verlangt, zuerst Werkzeuge
///    aufzurufen und jede Aussage mit tatsächlich gelesenen Stellen zu
///    belegen, sowie keine Zeitstempel zu erfinden (`retrieved_at`/
///    `produced_at` sind der tatsächliche aktuelle Zeitpunkt). Die
///    serverseitige Gegenprobe dazu — ob wirklich ein Werkzeug lief — prüft
///    [`harw_core_bridge`](../harw_core_bridge/index.html)s Fan-out-Auswertung,
///    nicht dieses Crate.
///
/// # Arguments
/// - `question_id` (`&str`): die `id` der gestellten [`crate::types::ResearchQuestion`];
///   wird unverändert in die Beispielinstanz übernommen.
///
/// # Returns
/// Fertiger Text zur direkten Übergabe an ein Modell.
#[must_use]
pub fn finding_schema_prompt_with_example(question_id: &str) -> String {
    let example = json!({
        "question_id": question_id,
        "conclusion": "<Kernaussage in einem vollständigen Satz>",
        "evidence": [{
            "kind": "local_source",
            "locator": "<Pfad:Zeilenbereich oder Symbolname, tatsächlich gelesen>",
            "retrieved_at": "<tatsächlicher aktueller ISO-8601-Zeitstempel>",
            "digest": null,
            "excerpt": "<kurzes wörtliches Zitat der gelesenen Stelle>"
        }],
        "verified_versions": [],
        "constraints": [],
        "compatibility_notes": [],
        "unresolved_questions": [],
        "confidence": "medium",
        "produced_by": "<dein Rollenname>",
        "produced_at": "<tatsächlicher aktueller ISO-8601-Zeitstempel>"
    });
    let pretty = serde_json::to_string_pretty(&example).unwrap_or_else(|_| example.to_string());
    format!(
        "Antworte ausschließlich mit EINEM JSON-Objekt dieser Form, nicht mit dem Schema. \
         Rufe zuerst Werkzeuge auf (fs.read/fs.grep/…), belege jede Aussage mit tatsächlich \
         gelesenen Stellen; erfinde keine Zeitstempel — retrieved_at/produced_at setzt du auf \
         den tatsächlichen aktuellen Zeitpunkt.\n\
         \n\
         Felder:\n\
         - question_id (string, Pflicht): exakt die ID der gestellten Frage, unverändert \
         übernehmen.\n\
         - conclusion (string, Pflicht): Schlussfolgerung in Prosa.\n\
         - evidence (array, Pflicht ab confidence \"medium\"): je Eintrag {{ kind, locator, \
         retrieved_at, digest?, excerpt? }} — kind ist eine der Arten local_source, \
         cargo_registry_source, official_docs, repository, release_notes, standard, web.\n\
         - verified_versions (array, optional): je Eintrag {{ crate_name, version, msrv?, \
         features?, verified_against }}.\n\
         - constraints (array<string>, optional).\n\
         - compatibility_notes (array<string>, optional).\n\
         - unresolved_questions (array<string>, optional).\n\
         - confidence (string, Pflicht): eine der Stufen low, medium, high, verified.\n\
         - produced_by (string, Pflicht): dein Rollenname.\n\
         - produced_at (string, Pflicht): ISO-8601-Zeitstempel.\n\
         \n\
         Beispielinstanz (Platzhalterwerte ersetzen, question_id unverändert übernehmen):\n\
         {pretty}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

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
    fn test_finding_json_schema_required_covers_contract_fields() -> TestResult {
        let schema = finding_json_schema();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .ok_or(TestError::Missing("schema[\"required\"] als Array"))?
            .iter()
            .map(|v| v.as_str())
            .collect::<Option<Vec<&str>>>()
            .ok_or(TestError::Missing("required-Eintrag als String"))?;
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
        Ok(())
    }

    #[test]
    fn test_finding_json_schema_properties_present() -> TestResult {
        let schema = finding_json_schema();
        let properties = schema["properties"]
            .as_object()
            .ok_or(TestError::Missing("schema[\"properties\"] als Objekt"))?;
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
        Ok(())
    }

    #[test]
    fn test_finding_schema_prompt_embeds_schema_json() {
        let prompt = finding_schema_prompt();
        assert!(prompt.contains("Antworte ausschließlich mit JSON"));
        assert!(prompt.contains("\"question_id\""));
        assert!(prompt.contains("draft/2020-12"));
    }

    #[test]
    fn test_finding_schema_prompt_with_example_has_no_raw_schema_dump() {
        let prompt = finding_schema_prompt_with_example("explore-wo-ist-x");
        assert!(
            !prompt.contains("\"$schema\""),
            "die Beispiel-basierte Anweisung darf keinen rohen Schema-Dump enthalten, der zum \
             Schema-Echo einlädt: {prompt}"
        );
        assert!(
            !prompt.contains("\"title\": \"ResearchFinding\""),
            "der Schema-Titel darf im Beispiel-Prompt nicht auftauchen: {prompt}"
        );
    }

    #[test]
    fn test_finding_schema_prompt_with_example_prefills_the_real_question_id() {
        let prompt = finding_schema_prompt_with_example("q-42");
        assert!(
            prompt.contains("\"question_id\": \"q-42\""),
            "die Beispielinstanz muss die echte question_id vorbefüllen, nicht erfinden \
             lassen: {prompt}"
        );
    }

    #[test]
    fn test_finding_schema_prompt_with_example_states_the_instance_only_rule() {
        let prompt = finding_schema_prompt_with_example("q-1");
        assert!(prompt.contains("ausschließlich mit EINEM JSON-Objekt dieser Form"));
        assert!(prompt.contains("nicht mit dem Schema"));
        assert!(prompt.contains("Rufe zuerst Werkzeuge auf"));
        assert!(prompt.contains("erfinde keine Zeitstempel"));
    }

    #[test]
    fn test_finding_schema_prompt_with_example_lists_all_contract_fields() {
        let prompt = finding_schema_prompt_with_example("q-1");
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
                prompt.contains(field),
                "die Feldliste muss '{field}' nennen: {prompt}"
            );
        }
    }
}
