//! Gemeinsame Bausteine für JSON-Schemas von Tool-Parametern.
//!
//! Ersetzt die zuvor in mehreren Tool-Crates kopierten Hilfsfunktionen
//! (`object_schema`, `string_property`, `property`, …). Alle Funktionen
//! erzeugen exakt dieselben [`JsonSchema`]-Werte wie die früheren Kopien.

use std::collections::BTreeMap;

use crate::schema::{AdditionalProperties, JsonSchema, JsonSchemaType};

/// Ein Feld-Schema mit Typ und Beschreibung.
///
/// # Arguments
/// * `schema_type` - JSON-Schema-Typ des Feldes.
/// * `description` - Beschreibung für das Modell.
#[must_use]
pub fn property(schema_type: JsonSchemaType, description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(schema_type),
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

/// Ein String-Feld-Schema mit Beschreibung.
#[must_use]
pub fn string_property(description: &str) -> JsonSchema {
    property(JsonSchemaType::String, description)
}

/// Objekt-Schema ohne Zusatzfelder (`additionalProperties: false`).
///
/// # Arguments
/// * `props` - Felder nach Name.
/// * `required` - Namen der Pflichtfelder.
#[must_use]
pub fn object_schema(props: BTreeMap<String, JsonSchema>, required: &[&str]) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(props),
        required: Some(required.iter().map(|name| (*name).to_owned()).collect()),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

/// Wie [`object_schema`], danach [`JsonSchema::into_strict`] (alle Felder
/// Pflicht, optionale als nullable).
#[must_use]
pub fn strict_object_schema(props: BTreeMap<String, JsonSchema>, required: &[&str]) -> JsonSchema {
    object_schema(props, required).into_strict()
}

/// Objekt-Schema, in dem jedes Feld Pflicht ist.
#[must_use]
pub fn object_schema_all_required(props: BTreeMap<String, JsonSchema>) -> JsonSchema {
    let required: Vec<String> = props.keys().cloned().collect();
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(props),
        required: Some(required),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

/// Wie [`object_schema`], mit den Feldern als Slice von `(Name, Schema)`.
#[must_use]
pub fn object_schema_from_pairs(
    properties: &[(&str, JsonSchema)],
    required: &[&str],
) -> JsonSchema {
    let props: BTreeMap<String, JsonSchema> = properties
        .iter()
        .map(|(name, schema)| ((*name).to_owned(), schema.clone()))
        .collect();
    object_schema(props, required)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_match_literal_schemas() {
        let literal = JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("d".to_owned()),
            ..Default::default()
        };
        assert_eq!(string_property("d"), literal);
        assert_eq!(property(JsonSchemaType::String, "d"), literal);

        let mut props = BTreeMap::new();
        props.insert("a".to_owned(), literal.clone());
        props.insert("b".to_owned(), property(JsonSchemaType::Boolean, "x"));
        let expected = JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(props.clone()),
            required: Some(vec!["a".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        };
        assert_eq!(object_schema(props.clone(), &["a"]), expected);
        assert_eq!(
            strict_object_schema(props.clone(), &["a"]),
            expected.clone().into_strict()
        );
        let pairs = [
            ("a", literal),
            ("b", property(JsonSchemaType::Boolean, "x")),
        ];
        assert_eq!(object_schema_from_pairs(&pairs, &["a"]), expected);
        let all = object_schema_all_required(props);
        assert_eq!(all.required, Some(vec!["a".to_owned(), "b".to_owned()]));
    }
}
